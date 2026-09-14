//! X-Plane 12 OANS data: one compact Lua file per airport for the FlyWithLua moving
//! map (`tools/xplane/amdb_oans.lua`), and an index for nearest-airport lookup.
//!
//! The Lua side is kept dumb and everything costly happens here:
//! * coordinates are integer metres from the reference point, x east and y north;
//! * polygons arrive already triangulated, because FlyWithLua's ImGui draw list can
//!   fill triangles but not arbitrary polygons;
//! * all geometry is bucketed into 300 m tiles carrying their real bounding boxes, so
//!   the script only touches what is on screen;
//! * each tile is its own Lua function, so no single function approaches LuaJIT's
//!   limit on constants however large the airport.

use crate::geom::LocalFrame;
use anyhow::{anyhow, Context, Result};
use geo::{Area, Centroid, Contains, Simplify, TriangulateEarcut};
use geo_types::{Coord, Geometry, LineString, MultiPolygon, Polygon};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const TILE: f64 = 300.0;
/// Anything farther than this from the reference point is a data error (a stray OSM
/// node), not a real airport feature; dropping it prevents triangles and lines from
/// fanning across the whole map. The largest airports are under 8 km from their ARP.
const MAX_M: f64 = 20000.0;
/// Long lines are cut into pieces this long so each lands in the tile it crosses.
const MAX_PIECE: f64 = 150.0;
/// Douglas-Peucker tolerance, metres. Still sub-pixel at the closest OANS range (0.25 NM
/// is about 0.9 px/m), so curves stay smooth, while the dense points left by tessellating
/// the source beziers are dropped: fewer triangles and segments per frame.
const SIMPLIFY_M: f64 = 1.0;
/// Sheds and service huts are not part of the OANS depiction, and at a big airport there
/// are hundreds of them. Keep terminals, named landmarks, and anything this size or over.
const MIN_BUILDING_M2: f64 = 300.0;
/// The shoulder and runway-edge outlines are decorative bands a couple of pixels wide, and
/// they come from the boundary of a union, which is very dense. A coarser tolerance is
/// invisible and saves thousands of segments a frame.
const OUTLINE_SIMPLIFY_M: f64 = 2.0;
/// Guidance lines for the wide view, where a metre is a fraction of a pixel.
const FAR_SIMPLIFY_M: f64 = 20.0;
/// Label kinds, matching the constants in the script.
const TWY: u8 = 1;
const RWY: u8 = 2;
const STAND: u8 = 3;
const TERM: u8 = 4;

/// The FlyWithLua script, embedded so `amdbgen xplane --install` needs no checkout.
pub const SCRIPT: &str = include_str!("../../tools/xplane/amdb_oans.lua");
const BRIDGE_MARK: &str = "--@BRIDGE@";

struct Feat {
    geom: Geometry<f64>,
    props: Map<String, Value>,
}

fn load_layer(dir: &Path, name: &str) -> Vec<Feat> {
    let Ok(text) = std::fs::read_to_string(dir.join(format!("{name}.geojson"))) else { return vec![] };
    let Ok(fc) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else { return vec![] };
    fc.get("features")
        .and_then(Value::as_array)
        .map(|fs| {
            fs.iter()
                .filter_map(|f| {
                    let geom = crate::output::geojson::geometry_from_json(f.get("geometry")?).ok()?;
                    let props = f.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
                    Some(Feat { geom, props })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn prop_str(p: &Map<String, Value>, k: &str) -> Option<String> {
    match p.get(k)? {
        Value::String(s) if !s.trim().is_empty() && s != "None" => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn prop_f64(p: &Map<String, Value>, k: &str) -> Option<f64> {
    match p.get(k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn polygons(g: &Geometry<f64>) -> Vec<&Polygon<f64>> {
    match g {
        Geometry::Polygon(p) => vec![p],
        Geometry::MultiPolygon(m) => m.0.iter().collect(),
        _ => vec![],
    }
}

fn lines(g: &Geometry<f64>) -> Vec<&LineString<f64>> {
    match g {
        Geometry::LineString(l) => vec![l],
        Geometry::MultiLineString(m) => m.0.iter().collect(),
        _ => vec![],
    }
}

fn dist(a: Coord<f64>, b: Coord<f64>) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// Total length of a polyline and the point halfway along it.
fn midpoint(pts: &[Coord<f64>]) -> (f64, Coord<f64>) {
    let total: f64 = pts.windows(2).map(|w| dist(w[0], w[1])).sum();
    let mut acc = 0.0;
    for w in pts.windows(2) {
        let d = dist(w[0], w[1]);
        if d > 0.0 && acc + d >= total / 2.0 {
            let t = (total / 2.0 - acc) / d;
            return (total, Coord { x: w[0].x + t * (w[1].x - w[0].x), y: w[0].y + t * (w[1].y - w[0].y) });
        }
        acc += d;
    }
    (total, pts[0])
}

/// Names fetched through the OSM map API were stored XML-escaped by builds before the
/// parser fix ("E/F &amp; Link"); show them as the text they are. `&amp;` goes last so
/// an escaped entity like `&amp;lt;` decodes once, to `&lt;`, not twice.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// OpenStreetMap names a structure as precisely as the mapper liked: "Terminal 3 Gate
/// L2C", "Terminal 3 (Building #8)". Printed verbatim they bury the close-in view, and
/// several of them name the same terminal. Cut back to the landmark so the callers'
/// de-duplication collapses them to one label per terminal.
fn tidy_name(s: &str) -> String {
    let mut t = s.trim();
    for cut in [" Gate ", " (", ", "] {
        if let Some(i) = t.find(cut) {
            t = t[..i].trim_end();
        }
    }
    if t.chars().count() > 20 {
        return t.chars().take(20).collect::<String>().trim_end().to_string();
    }
    t.to_string()
}

/// A stand is labelled with its designator, the way it is called on the radio and printed
/// on the OANS: "E15B", not OpenStreetMap's "Terminal 2 Gate E15B". Stands that are not
/// gates keep their name, shortened.
fn tidy_stand(s: &str) -> String {
    let mut t = s.trim();
    if let Some(i) = t.rfind(" (") {
        t = t[..i].trim_end();
    }
    if let Some(i) = t.rfind(" Gate ") {
        let tail = t[i + 6..].trim();
        if !tail.is_empty() {
            t = tail;
        }
    }
    if t.chars().count() > 12 {
        return t.chars().take(12).collect::<String>().trim_end().to_string();
    }
    t.to_string()
}

/// A Lua string literal.
fn lua_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => {}
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn key(x: f64, y: f64) -> (i32, i32) {
    ((x / TILE).floor() as i32, (y / TILE).floor() as i32)
}

#[derive(Default)]
struct Tile {
    bbox: Option<[i64; 4]>,
    fill: BTreeMap<&'static str, Vec<i64>>,
    line: BTreeMap<&'static str, Vec<Vec<i64>>>,
    text: Vec<(i64, i64, String, u8)>,
}

impl Tile {
    fn grow(&mut self, x: i64, y: i64) {
        let b = self.bbox.get_or_insert([x, y, x, y]);
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x);
        b[3] = b[3].max(y);
    }
}

struct Grid {
    frame: LocalFrame,
    tiles: BTreeMap<(i32, i32), Tile>,
}

impl Grid {
    fn project(&self, c: Coord<f64>) -> Coord<f64> {
        self.frame.forward(c.x, c.y)
    }

    /// Project a source polygon into clean metre-frame polygons. A ring that is not
    /// inside the exterior is not a hole (a runway written as two rectangles in one
    /// Polygon, say): it becomes its own polygon. Fed straight to ear-cutting, such a
    /// "hole" spans triangles right across the airport.
    fn clean(&self, p: &Polygon<f64>) -> Vec<Polygon<f64>> {
        let f = &self.frame;
        let proj = |ls: &LineString<f64>| {
            let mut pts: Vec<Coord<f64>> = Vec::with_capacity(ls.0.len());
            for c in &ls.0 {
                let m = f.forward(c.x, c.y);
                if m.x.abs() > MAX_M || m.y.abs() > MAX_M {
                    continue;
                }
                if pts.last().map_or(true, |l| dist(*l, m) > 0.05) {
                    pts.push(m);
                }
            }
            if pts.len() >= 3 && pts.first() != pts.last() {
                pts.push(pts[0]);
            }
            LineString(pts).simplify(SIMPLIFY_M)
        };
        let ext = proj(p.exterior());
        if ext.0.len() < 4 {
            return vec![];
        }
        let shell = Polygon::new(ext.clone(), vec![]);
        let mut holes = Vec::new();
        let mut extra = Vec::new();
        for r in p.interiors() {
            let ring = proj(r);
            if ring.0.len() < 4 {
                continue;
            }
            if shell.contains(&ring.0[0]) {
                holes.push(ring);
            } else {
                extra.push(Polygon::new(ring, vec![]));
            }
        }
        let mut out = vec![Polygon::new(ext, holes)];
        out.extend(extra);
        out
    }

    /// Triangulate a filled layer. Each triangle goes to the tile of its centroid, which
    /// grows to the triangle's full extent so large ones are never culled too early.
    fn fill(&mut self, layer: &'static str, g: &Geometry<f64>) {
        for src in polygons(g) {
            for pm in self.clean(src) {
                for t in pm.earcut_triangles() {
                    let corners = [t.v1(), t.v2(), t.v3()];
                    let (cx, cy) = ((corners[0].x + corners[1].x + corners[2].x) / 3.0, (corners[0].y + corners[1].y + corners[2].y) / 3.0);
                    // Ear-cutting a self-intersecting ring yields triangles outside it;
                    // keep only those whose centre is really inside the polygon.
                    if !pm.contains(&Coord { x: cx, y: cy }) {
                        continue;
                    }
                    let v = corners.map(|c| (c.x.round() as i64, c.y.round() as i64));
                    // Rounding to metres can flatten a sliver to nothing; skip those.
                    let area2 = (v[1].0 - v[0].0) * (v[2].1 - v[0].1) - (v[2].0 - v[0].0) * (v[1].1 - v[0].1);
                    if area2 == 0 {
                        continue;
                    }
                    let tile = self.tiles.entry(key(cx, cy)).or_default();
                    for &(x, y) in &v {
                        tile.grow(x, y);
                    }
                    let buf = tile.fill.entry(layer).or_default();
                    for &(x, y) in &v {
                        buf.push(x);
                        buf.push(y);
                    }
                }
            }
        }
    }

    /// Outline the outer boundary of a set of polygons after unioning them, so that
    /// seams between adjacent elements (taxiway pieces, runway sections) do not show:
    /// the OANS shoulder look around taxiways, and the white edge around runways.
    fn outline_union(&mut self, layer: &'static str, polys: &[Polygon<f64>]) {
        let merged: MultiPolygon<f64> = crate::geom::ops::union_all(polys);
        for p in &merged.0 {
            for ring in std::iter::once(p.exterior()).chain(p.interiors()) {
                self.piece_line(layer, &ring.simplify(OUTLINE_SIMPLIFY_M).0);
            }
        }
    }

    /// A polyline already in the metre frame, cut into tile-sized pieces.
    fn piece_line(&mut self, layer: &'static str, pts: &[Coord<f64>]) {
        let mut piece: Vec<Coord<f64>> = Vec::new();
        let mut len = 0.0;
        for &c in pts {
            match piece.last() {
                None => piece.push(c),
                Some(&last) => {
                    let d = dist(last, c);
                    if d < 1.0 {
                        continue;
                    }
                    piece.push(c);
                    len += d;
                    if len >= MAX_PIECE {
                        self.piece(layer, &piece);
                        piece = vec![c];
                        len = 0.0;
                    }
                }
            }
        }
        if piece.len() >= 2 {
            self.piece(layer, &piece);
        }
    }

    /// A line layer, cut into short pieces with near-duplicate points dropped.
    fn line(&mut self, layer: &'static str, g: &Geometry<f64>) {
        self.line_tol(layer, g, SIMPLIFY_M);
    }

    /// The same, simplified at `tol` metres.
    fn line_tol(&mut self, layer: &'static str, g: &Geometry<f64>, tol: f64) {
        for ls in lines(g) {
            let pts: Vec<Coord<f64>> = LineString(ls.0.iter().map(|c| self.project(*c)).collect::<Vec<_>>()).simplify(tol).0;
            self.piece_line(layer, &pts);
        }
    }

    fn piece(&mut self, layer: &'static str, pts: &[Coord<f64>]) {
        if pts.iter().any(|c| c.x.abs() > MAX_M || c.y.abs() > MAX_M) {
            return;
        }
        let tile = self.tiles.entry(key(pts[0].x, pts[0].y)).or_default();
        let mut flat = Vec::with_capacity(pts.len() * 2);
        for c in pts {
            let (x, y) = (c.x.round() as i64, c.y.round() as i64);
            tile.grow(x, y);
            flat.push(x);
            flat.push(y);
        }
        tile.line.entry(layer).or_default().push(flat);
    }

    fn label(&mut self, at: Coord<f64>, text: &str, kind: u8) {
        let text = decode_entities(text.trim());
        if text.is_empty() || at.x.abs() > MAX_M || at.y.abs() > MAX_M {
            return;
        }
        let (x, y) = (at.x.round() as i64, at.y.round() as i64);
        let tile = self.tiles.entry(key(at.x, at.y)).or_default();
        tile.grow(x, y);
        tile.text.push((x, y, text.to_string(), kind));
    }
}

fn join_into(s: &mut String, v: &[i64]) {
    for (i, n) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(s, "{n}");
    }
}

/// Write `<dir>/oans.lua` for one built airport folder. Returns the file size in bytes.
pub fn write(dir: &Path) -> Result<u64> {
    let s = render(dir)?;
    let out = dir.join("oans.lua");
    std::fs::write(&out, &s).with_context(|| format!("write {}", out.display()))?;
    Ok(s.len() as u64)
}

/// The Lua moving-map data for one built airport folder, as a `return {...}` chunk.
/// Used both by [`write`] and, live, by the bridge's `/xp/` route.
pub fn render(dir: &Path) -> Result<String> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).with_context(|| format!("read {}/manifest.json (is it a built airport?)", dir.display()))?;
    let manifest: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).context("parse manifest.json")?;
    let folder = dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let icao = manifest.get("icao").and_then(Value::as_str).map(str::to_string).unwrap_or(folder);
    let name = manifest.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let arp = manifest.get("arp").and_then(Value::as_array).ok_or_else(|| anyhow!("{icao}: manifest has no reference point"))?;
    let (lat0, lon0) = match (arp.first().and_then(Value::as_f64), arp.get(1).and_then(Value::as_f64)) {
        (Some(a), Some(b)) => (a, b),
        _ => return Err(anyhow!("{icao}: bad reference point in manifest")),
    };
    let frame = LocalFrame::new(lat0, lon0);
    // Metres per degree at the reference point, from our own projection: the script
    // uses these to place the aircraft, so both sides agree.
    let d = 0.01;
    let mx = frame.forward(lon0 + d, lat0).x / d;
    let my = frame.forward(lon0, lat0 + d).y / d;
    let mut g = Grid { frame, tiles: BTreeMap::new() };

    // Pavement fills, plus the cleaned polygons kept for the outlines below.
    let mut pavement: Vec<Polygon<f64>> = Vec::new();
    let mut runways: Vec<Polygon<f64>> = Vec::new();
    for (file, layer) in [
        ("apronelement", "apron"),
        ("deicingarea", "apron"),
        ("taxiwayelement", "taxiway"),
        ("runwaydisplacedarea", "rwyext"),
        ("blastpad", "rwyext"),
        ("stopway", "rwyext"),
        ("runwayelement", "runway"),
    ] {
        for f in load_layer(dir, file) {
            g.fill(layer, &f.geom);
            for p in polygons(&f.geom) {
                let cleaned = g.clean(p);
                if layer == "runway" {
                    runways.extend(cleaned.iter().cloned());
                }
                pavement.extend(cleaned);
            }
        }
    }
    // The OANS draws a shoulder strip around the outside of all pavement, and a white
    // edge around each runway. Both come from the union, so seams between adjacent
    // elements never show.
    g.outline_union("shoulder", &pavement);
    g.outline_union("rwyedge", &runways);
    // Only landmarks carry a name in the data (terminals, tower, hangars). A terminal
    // split into several polygons is labelled once, on its largest part.
    let mut named: BTreeMap<String, (f64, Coord<f64>)> = BTreeMap::new();
    for f in load_layer(dir, "verticalpolygonalstructure") {
        let terminal = prop_f64(&f.props, "plysttyp") == Some(1.0);
        let name = prop_str(&f.props, "name");
        // Projected area, so the threshold is real square metres.
        let area: f64 = polygons(&f.geom).into_iter().flat_map(|p| g.clean(p)).map(|p| p.unsigned_area()).sum();
        if !terminal && name.is_none() && area < MIN_BUILDING_M2 {
            continue;
        }
        g.fill(if terminal { "terminal" } else { "building" }, &f.geom);
        if let (Some(raw), Some(c)) = (name, f.geom.centroid()) {
            let key = tidy_name(&raw);
            if !key.is_empty() {
                let at = g.project(c.0);
                let e = named.entry(key).or_insert((-1.0, at));
                if area > e.0 {
                    *e = (area, at);
                }
            }
        }
    }
    for (name, (_, at)) in named {
        g.label(at, &name, TERM);
    }

    let guides = load_layer(dir, "taxiwayguidanceline");
    for f in &guides {
        g.line("guide", &f.geom);
        // The wide view draws the taxiway network alone, at a scale where fine detail is
        // sub-pixel: a coarse copy keeps a whole big airport inside the vertex budget.
        g.line_tol("guidefar", &f.geom, FAR_SIMPLIFY_M);
    }
    for (file, layer) in [("standguidanceline", "stand"), ("runwayexitline", "exit"), ("taxiwayholdingposition", "hold"), ("paintedcenterline", "rwycl")] {
        for f in load_layer(dir, file) {
            g.line(layer, &f.geom);
        }
    }

    // One label per taxiway designator, halfway along its longest guidance line, as the
    // OANS places them. (Per-tile labels would repeat the same letter dozens of times.)
    let mut best: BTreeMap<String, (f64, Coord<f64>)> = BTreeMap::new();
    for f in &guides {
        let Some(id) = prop_str(&f.props, "idlin") else { continue };
        for ls in lines(&f.geom) {
            let pts: Vec<Coord<f64>> = ls.0.iter().map(|c| g.project(*c)).collect();
            if pts.len() < 2 {
                continue;
            }
            let (len, mid) = midpoint(&pts);
            let e = best.entry(id.clone()).or_insert((0.0, mid));
            if len > e.0 {
                *e = (len, mid);
            }
        }
    }
    for (id, (len, mid)) in best {
        if len >= 40.0 {
            g.label(mid, &id, TWY);
        }
    }

    // Runway designators just beyond each threshold, as the OANS draws them.
    for f in load_layer(dir, "runwaythreshold") {
        let (Geometry::Point(p), Some(id)) = (&f.geom, prop_str(&f.props, "idthr")) else { continue };
        let m = g.project(p.0);
        let b = prop_f64(&f.props, "brngtrue").unwrap_or(0.0).to_radians();
        g.label(Coord { x: m.x - b.sin() * 55.0, y: m.y - b.cos() * 55.0 }, &id, RWY);
    }
    for f in load_layer(dir, "parkingstandlocation") {
        let (Geometry::Point(p), Some(id)) = (&f.geom, prop_str(&f.props, "idstd")) else { continue };
        let at = g.project(p.0);
        g.label(at, &tidy_stand(&id), STAND);
    }

    let mut s = String::with_capacity(1 << 20);
    writeln!(s, "-- amdbgen X-Plane OANS data: {icao} {name}")?;
    writeln!(s, "-- integer metres from the reference point, x east, y north; fills are triangles")?;
    writeln!(s, "local A = {{v=1, icao={}, name={}, lat={lat0:.7}, lon={lon0:.7}, mx={mx:.4}, my={my:.4}, tiles={{}}}}", lua_str(&icao), lua_str(&name))?;
    s.push_str("local T = A.tiles\n");
    for t in g.tiles.values() {
        let Some(b) = t.bbox else { continue };
        write!(s, "T[#T+1]=(function() return {{b={{{},{},{},{}}}", b[0], b[1], b[2], b[3])?;
        if !t.fill.is_empty() {
            s.push_str(",f={");
            for (i, (k, v)) in t.fill.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                write!(s, "{k}={{")?;
                join_into(&mut s, v);
                s.push('}');
            }
            s.push('}');
        }
        if !t.line.is_empty() {
            s.push_str(",l={");
            for (i, (k, ls)) in t.line.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                write!(s, "{k}={{")?;
                for (j, l) in ls.iter().enumerate() {
                    if j > 0 {
                        s.push(',');
                    }
                    s.push('{');
                    join_into(&mut s, l);
                    s.push('}');
                }
                s.push('}');
            }
            s.push('}');
        }
        if !t.text.is_empty() {
            s.push_str(",t={");
            for (i, (x, y, txt, k)) in t.text.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                write!(s, "{x},{y},{},{k}", lua_str(txt))?;
            }
            s.push('}');
        }
        s.push_str("} end)()\n");
    }
    s.push_str("return A\n");
    Ok(s)
}

/// Rebuild `<root>/index.lua` from every airport folder that has an `oans.lua`. The
/// folder name is the key, since that is the path the script loads.
pub fn write_index(root: &Path) -> Result<usize> {
    let mut rows: Vec<(String, f64, f64)> = Vec::new();
    for e in std::fs::read_dir(root).with_context(|| format!("read {}", root.display()))?.flatten() {
        let d = e.path();
        if !d.join("oans.lua").is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(d.join("manifest.json")) else { continue };
        let Ok(m) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else { continue };
        let Some(arp) = m.get("arp").and_then(Value::as_array) else { continue };
        if let (Some(lat), Some(lon)) = (arp.first().and_then(Value::as_f64), arp.get(1).and_then(Value::as_f64)) {
            rows.push((e.file_name().to_string_lossy().to_string(), lat, lon));
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut s = String::from("-- amdbgen X-Plane OANS index: icao, lat, lon\nreturn {\n");
    for (i, (c, la, lo)) in rows.iter().enumerate() {
        write!(s, "{},{la:.6},{lo:.6},", lua_str(c))?;
        if i % 8 == 7 {
            s.push('\n');
        }
    }
    s.push_str("\n}\n");
    std::fs::write(root.join("index.lua"), s).with_context(|| format!("write {}/index.lua", root.display()))?;
    Ok(rows.len())
}

/// Copy the FlyWithLua script into an X-Plane install, pointing it at the bridge's
/// base URL (e.g. `http://127.0.0.1:8770/`).
pub fn install_script(xplane_root: &Path, bridge_url: &str) -> Result<PathBuf> {
    let scripts = xplane_root.join("Resources").join("plugins").join("FlyWithLua").join("Scripts");
    if !scripts.is_dir() {
        return Err(anyhow!("FlyWithLua is not installed in {} (no {})", xplane_root.display(), scripts.display()));
    }
    let url = if bridge_url.ends_with('/') { bridge_url.to_string() } else { format!("{bridge_url}/") };
    let script: String = SCRIPT
        .lines()
        .map(|l| if l.contains(BRIDGE_MARK) { format!("local BRIDGE = {} {BRIDGE_MARK}", lua_str(&url)) } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let out = scripts.join("amdb_oans.lua");
    std::fs::write(&out, script).with_context(|| format!("write {}", out.display()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path) -> PathBuf {
        let ap = root.join("TEST");
        std::fs::create_dir_all(&ap).unwrap();
        std::fs::write(ap.join("manifest.json"), r#"{"icao":"TEST","name":"Test \"Field\"","arp":[50.0,8.0]}"#).unwrap();
        std::fs::write(
            ap.join("runwayelement.geojson"),
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"idrwy":"09/27"},
               "geometry":{"type":"Polygon","coordinates":[[[7.99,49.9998],[8.01,49.9998],[8.01,50.0002],[7.99,50.0002],[7.99,49.9998]]]}}]}"#,
        )
        .unwrap();
        std::fs::write(
            ap.join("runwaythreshold.geojson"),
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"idthr":"09","brngtrue":90},
               "geometry":{"type":"Point","coordinates":[7.99,50.0]}}]}"#,
        )
        .unwrap();
        std::fs::write(
            ap.join("taxiwayguidanceline.geojson"),
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"idlin":"A"},
               "geometry":{"type":"LineString","coordinates":[[7.995,50.0006],[8.0,50.0006],[8.005,50.0006]]}}]}"#,
        )
        .unwrap();
        ap
    }

    #[test]
    fn writes_tiles_triangles_lines_and_labels() {
        let root = std::env::temp_dir().join(format!("amdbgen-xp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ap = fixture(&root);
        let n = write(&ap).unwrap();
        let lua = std::fs::read_to_string(ap.join("oans.lua")).unwrap();
        assert_eq!(n as usize, lua.len());
        assert!(lua.starts_with("-- amdbgen X-Plane OANS data: TEST"));
        assert!(lua.contains(r#"name="Test \"Field\"""#), "quotes are escaped");
        assert!(lua.contains("runway={"), "runway triangles written");
        assert!(lua.contains("guide={{"), "guidance line written");
        assert!(lua.contains(r#","09",2"#), "runway designator label");
        assert!(lua.contains(r#","A",1"#), "taxiway label");
        assert!(lua.trim_end().ends_with("return A"));
        // A 1.4 km x 45 m runway is two triangles after earcut, in integer metres.
        let tri_line = lua.lines().find(|l| l.contains("runway={")).unwrap();
        assert!(!tri_line.contains('.'), "coordinates are integers");

        assert_eq!(write_index(&root).unwrap(), 1);
        let idx = std::fs::read_to_string(root.join("index.lua")).unwrap();
        assert!(idx.contains(r#""TEST",50.000000,8.000000"#));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn installs_script_pointing_at_the_bridge() {
        let root = std::env::temp_dir().join(format!("amdbgen-xpi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(install_script(&root, "http://127.0.0.1:8770").is_err(), "no FlyWithLua, no install");
        std::fs::create_dir_all(root.join("Resources/plugins/FlyWithLua/Scripts")).unwrap();
        let p = install_script(&root, "http://127.0.0.1:8770").unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains(r#"local BRIDGE = "http://127.0.0.1:8770/" --@BRIDGE@"#), "trailing slash added");
        assert_eq!(s.matches("local BRIDGE").count(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_disjoint_ring_is_not_a_hole() {
        // KJFK runway 13L/31R: one Polygon holding two separate rectangles. Ear-cutting
        // that as exterior+hole spans triangles across the airport (3.7 km edges).
        let root = std::env::temp_dir().join(format!("amdbgen-xph-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ap = root.join("TEST");
        std::fs::create_dir_all(&ap).unwrap();
        std::fs::write(ap.join("manifest.json"), r#"{"icao":"TEST","name":"t","arp":[50.0,8.0]}"#).unwrap();
        std::fs::write(
            ap.join("runwayelement.geojson"),
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"idrwy":"09/27"},
               "geometry":{"type":"Polygon","coordinates":[
                 [[7.990,50.0000],[7.995,50.0000],[7.995,50.0004],[7.990,50.0004],[7.990,50.0000]],
                 [[8.020,50.0100],[8.025,50.0100],[8.025,50.0104],[8.020,50.0104],[8.020,50.0100]]]}}]}"#,
        )
        .unwrap();
        let lua = render(&ap).unwrap();
        let line = lua.lines().find(|l| l.contains("runway={")).unwrap_or("");
        // Two rectangles -> four triangles; every edge under 400 m.
        let nums: Vec<i64> = line.split("runway={").nth(1).unwrap().split('}').next().unwrap().split(',').filter_map(|s| s.trim().parse().ok()).collect();
        let tris = nums.len() / 6;
        assert!(tris >= 2, "expected triangles, got {tris} in {line}");
        for t in nums.chunks(6) {
            let e = |a: usize, b: usize| (((t[a] - t[b]).pow(2) + (t[a + 1] - t[b + 1]).pow(2)) as f64).sqrt();
            let longest = e(0, 2).max(e(2, 4)).max(e(4, 0));
            assert!(longest < 400.0, "triangle spans {longest:.0} m: {t:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stands_are_labelled_with_their_designator() {
        assert_eq!(tidy_stand("Terminal 2 Gate E15B"), "E15B");
        assert_eq!(tidy_stand("Terminal 1 Gate C4"), "C4");
        assert_eq!(tidy_stand("Terminal 2 Gate E13 (2)"), "E13");
        assert_eq!(tidy_stand("United Maint. (4)"), "United Maint", "13 chars, cut to the 12-char cap");
        assert_eq!(tidy_stand("Fed Ex 9"), "Fed Ex 9");
        assert_eq!(tidy_stand("A12"), "A12");
    }

    #[test]
    fn structure_names_are_cut_back_to_the_landmark() {
        assert_eq!(tidy_name("Terminal 3 Gate L2C"), "Terminal 3");
        assert_eq!(tidy_name("Terminal 3 (Building #8)"), "Terminal 3");
        assert_eq!(tidy_name("Terminal 2 Gate E1A"), "Terminal 2");
        assert_eq!(tidy_name("Roman C. Pucinski Tower"), "Roman C. Pucinski To");
        assert_eq!(tidy_name("  Terminal 5  "), "Terminal 5");
        assert_eq!(tidy_name("Gatehouse"), "Gatehouse", "only a separate word Gate cuts");
    }

    #[test]
    fn xml_entities_are_decoded_once() {
        assert_eq!(decode_entities("Concourse E/F &amp; Link"), "Concourse E/F & Link");
        assert_eq!(decode_entities("&quot;A&quot; &lt;B&gt;"), "\"A\" <B>");
        assert_eq!(decode_entities("&amp;lt;"), "&lt;", "an escaped entity decodes once");
        assert_eq!(decode_entities("plain"), "plain");
    }

    #[test]
    fn lua_strings_are_escaped() {
        assert_eq!(lua_str(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(lua_str("x\ny"), r#""x\ny""#);
    }
}
