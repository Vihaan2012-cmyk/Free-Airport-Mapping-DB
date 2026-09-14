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
use geo::{Centroid, Simplify, TriangulateEarcut};
use geo_types::{Coord, Geometry, LineString, Polygon};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const TILE: f64 = 300.0;
/// Long lines are cut into pieces this long so each lands in the tile it crosses.
const MAX_PIECE: f64 = 150.0;
/// Douglas-Peucker tolerance, metres. Sub-pixel at the closest OANS range, so bends stay
/// smooth, while most redundant bezier points are dropped for fewer draw calls.
const SIMPLIFY_M: f64 = 0.3;
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

    /// Triangulate a filled layer. Each triangle goes to the tile of its centroid, which
    /// grows to the triangle's full extent so large ones are never culled too early.
    fn fill(&mut self, layer: &'static str, g: &Geometry<f64>) {
        for p in polygons(g) {
            let f = &self.frame;
            let proj = |ls: &LineString<f64>| LineString(ls.0.iter().map(|c| f.forward(c.x, c.y)).collect::<Vec<_>>());
            let pm = Polygon::new(proj(p.exterior()), p.interiors().iter().map(proj).collect()).simplify(SIMPLIFY_M);
            for t in pm.earcut_triangles() {
                let corners = [t.v1(), t.v2(), t.v3()];
                let v = corners.map(|c| (c.x.round() as i64, c.y.round() as i64));
                // Rounding to metres can flatten a sliver to nothing; skip those.
                let area2 = (v[1].0 - v[0].0) * (v[2].1 - v[0].1) - (v[2].0 - v[0].0) * (v[1].1 - v[0].1);
                if area2 == 0 {
                    continue;
                }
                let (cx, cy) = ((corners[0].x + corners[1].x + corners[2].x) / 3.0, (corners[0].y + corners[1].y + corners[2].y) / 3.0);
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

    /// A line layer, cut into short pieces with near-duplicate points dropped.
    fn line(&mut self, layer: &'static str, g: &Geometry<f64>) {
        for ls in lines(g) {
            let pts: Vec<Coord<f64>> = LineString(ls.0.iter().map(|c| self.project(*c)).collect::<Vec<_>>()).simplify(SIMPLIFY_M).0;
            let mut piece: Vec<Coord<f64>> = Vec::new();
            let mut len = 0.0;
            for c in pts {
                match piece.last() {
                    None => piece.push(c),
                    Some(&last) => {
                        let d = dist(last, c);
                        if d < 1.5 {
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
    }

    fn piece(&mut self, layer: &'static str, pts: &[Coord<f64>]) {
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

    /// Emit a polygon's rings as edge lines, so filled pavement gets a defined boundary
    /// (the taxiway/runway shoulder look).
    fn outline(&mut self, layer: &'static str, g: &Geometry<f64>) {
        for p in polygons(g) {
            for ring in std::iter::once(p.exterior()).chain(p.interiors()) {
                self.line(layer, &Geometry::LineString(ring.clone()));
            }
        }
    }

    fn label(&mut self, at: Coord<f64>, text: &str, kind: u8) {
        let text = decode_entities(text.trim());
        if text.is_empty() {
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
        }
    }
    // Edge outlines give the pavement a defined boundary (the shoulder look).
    for file in ["taxiwayelement", "runwayelement", "runwaydisplacedarea", "apronelement"] {
        for f in load_layer(dir, file) {
            g.outline("edge", &f.geom);
        }
    }
    for f in load_layer(dir, "verticalpolygonalstructure") {
        let terminal = prop_f64(&f.props, "plysttyp") == Some(1.0);
        g.fill(if terminal { "terminal" } else { "building" }, &f.geom);
        // Only landmarks carry a name in the data (terminals, tower, hangars).
        if let (Some(name), Some(c)) = (prop_str(&f.props, "name"), f.geom.centroid()) {
            let at = g.project(c.0);
            g.label(at, &name, TERM);
        }
    }

    let guides = load_layer(dir, "taxiwayguidanceline");
    for f in &guides {
        g.line("guide", &f.geom);
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
        g.label(at, &id, STAND);
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
