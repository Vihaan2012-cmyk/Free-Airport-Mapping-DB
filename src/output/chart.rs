//! Jeppesen-style airport diagram as a vector PDF (A4 portrait), drawn straight from a
//! built airport folder. Black, white and greys like a printed "10-9" ground chart:
//! runways black with designators and dimensions, taxiways and aprons grey with
//! taxiway letters, terminals dark, holding positions, hotspots, stands, ARP, tower,
//! a frequency box, a runway table, a scale bar and a north arrow.

use crate::geom::LocalFrame;
use crate::output::manifest::Manifest;
use anyhow::{Context, Result};
use geo::{Area, Centroid};
use geo_types::{Coord, Geometry, LineString, Polygon};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

const A4_SHORT: f32 = 595.276;
const A4_LONG: f32 = 841.89;
const MARGIN: f32 = 26.0;
const HEADER_H: f32 = 132.0;
const FOOTER_H: f32 = 30.0;
const FONT: Name = Name(b"F1");
const BOLD: Name = Name(b"F2");

pub(crate) struct Feat {
    pub geom: Geometry<f64>,
    pub props: Map<String, Value>,
}

pub(crate) fn load_layer(dir: &Path, name: &str) -> Vec<Feat> {
    let Ok(text) = std::fs::read_to_string(dir.join(format!("{name}.geojson"))) else { return vec![] };
    let Ok(fc) = serde_json::from_str::<Value>(&text) else { return vec![] };
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
        Value::String(s) if !s.is_empty() && s != "None" => Some(s.clone()),
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

/// WinAnsi-safe bytes for the built-in fonts.
fn ascii(s: &str) -> Vec<u8> {
    crate::output::winansi(s)
}

/// Approximate advance width of Helvetica text.
fn text_w(size: f32, s: &str) -> f32 {
    0.53 * size * s.chars().count() as f32
}

/// Page mapping of the local metre frame.
struct MapArea {
    frame: LocalFrame,
    scale: f32,
    ox: f32,
    oy: f32,
}

impl MapArea {
    fn pt(&self, c: Coord<f64>) -> (f32, f32) {
        let l = self.frame.forward(c.x, c.y);
        (self.ox + l.x as f32 * self.scale, self.oy + l.y as f32 * self.scale)
    }

    fn ring(&self, content: &mut Content, r: &LineString<f64>) {
        for (i, c) in r.0.iter().enumerate() {
            let (x, y) = self.pt(*c);
            if i == 0 {
                content.move_to(x, y);
            } else {
                content.line_to(x, y);
            }
        }
        content.close_path();
    }

    fn polygon_path(&self, content: &mut Content, p: &Polygon<f64>) {
        self.ring(content, p.exterior());
        for h in p.interiors() {
            self.ring(content, h);
        }
    }

    /// Fill (even-odd, so holes stay open) and/or stroke every polygon of a geometry.
    fn fill_geom(&self, content: &mut Content, g: &Geometry<f64>, fill: Option<[f32; 3]>, stroke: Option<([f32; 3], f32)>) {
        let polys: Vec<&Polygon<f64>> = match g {
            Geometry::Polygon(p) => vec![p],
            Geometry::MultiPolygon(m) => m.0.iter().collect(),
            _ => vec![],
        };
        if polys.is_empty() {
            return;
        }
        if let Some(c) = fill {
            content.set_fill_rgb(c[0], c[1], c[2]);
        }
        if let Some((c, w)) = stroke {
            content.set_stroke_rgb(c[0], c[1], c[2]);
            content.set_line_width(w);
        }
        for p in polys {
            self.polygon_path(content, p);
        }
        match (fill.is_some(), stroke.is_some()) {
            (true, true) => content.fill_even_odd_and_stroke(),
            (true, false) => content.fill_even_odd(),
            (false, true) => content.stroke(),
            _ => content.end_path(),
        };
    }

    fn stroke_lines(&self, content: &mut Content, g: &Geometry<f64>, colour: [f32; 3], width: f32) {
        let lines: Vec<&LineString<f64>> = match g {
            Geometry::LineString(l) => vec![l],
            Geometry::MultiLineString(m) => m.0.iter().collect(),
            _ => vec![],
        };
        if lines.is_empty() {
            return;
        }
        content.set_stroke_rgb(colour[0], colour[1], colour[2]);
        content.set_line_width(width);
        for l in lines {
            for (i, c) in l.0.iter().enumerate() {
                let (x, y) = self.pt(*c);
                if i == 0 {
                    content.move_to(x, y);
                } else {
                    content.line_to(x, y);
                }
            }
        }
        content.stroke();
    }
}

fn text(content: &mut Content, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32) {
    content.set_fill_gray(grey);
    content.begin_text();
    content.set_font(font, size);
    content.next_line(x, y);
    content.show(Str(&ascii(s)));
    content.end_text();
}

fn text_right(content: &mut Content, font: Name, size: f32, x_right: f32, y: f32, s: &str, grey: f32) {
    text(content, font, size, x_right - text_w(size, s), y, s, grey);
}

/// Centred text rotated by `angle` degrees (counter-clockwise), kept upright.
fn text_rot(content: &mut Content, font: Name, size: f32, x: f32, y: f32, angle: f32, s: &str, grey: f32) {
    let mut a = angle % 360.0;
    if a > 180.0 {
        a -= 360.0;
    }
    if a > 90.0 {
        a -= 180.0;
    } else if a < -90.0 {
        a += 180.0;
    }
    let (sn, cs) = a.to_radians().sin_cos();
    content.set_fill_gray(grey);
    content.begin_text();
    content.set_font(font, size);
    content.set_text_matrix([cs, sn, -sn, cs, x, y]);
    content.next_line(-text_w(size, s) / 2.0, -size * 0.35);
    content.show(Str(&ascii(s)));
    content.end_text();
}

/// Label with a white box behind it, centred on (x, y).
fn label_boxed(content: &mut Content, font: Name, size: f32, x: f32, y: f32, s: &str) {
    let w = text_w(size, s) + 2.0;
    let h = size + 1.2;
    content.set_fill_gray(1.0);
    content.rect(x - w / 2.0, y - h / 2.0, w, h);
    content.fill_nonzero();
    text(content, font, size, x - w / 2.0 + 1.0, y - size * 0.35, s, 0.0);
}

fn circle(content: &mut Content, x: f32, y: f32, r: f32) {
    const K: f32 = 0.5523;
    content.move_to(x + r, y);
    content.cubic_to(x + r, y + K * r, x + K * r, y + r, x, y + r);
    content.cubic_to(x - K * r, y + r, x - r, y + K * r, x - r, y);
    content.cubic_to(x - r, y - K * r, x - K * r, y - r, x, y - r);
    content.cubic_to(x + K * r, y - r, x + r, y - K * r, x + r, y);
    content.close_path();
}

fn fmt_coord(lat: f64, lon: f64) -> String {
    let f = |v: f64, pos: char, neg: char, w: usize| {
        let h = if v >= 0.0 { pos } else { neg };
        let a = v.abs();
        let d = a.floor();
        let m = (a - d) * 60.0;
        format!("{h}{:0w$}° {m:04.1}'", d as i64, w = w)
    };
    format!("{}  {}", f(lat, 'N', 'S', 2), f(lon, 'E', 'W', 3))
}

/// Write `<dir>`'s airport diagram to `out`. Returns the PDF size in bytes.
pub fn write(dir: &Path, out: &Path) -> Result<u64> {
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).with_context(|| format!("read {}/manifest.json (is it a built airport?)", dir.display()))?)?;
    let frame = LocalFrame::new(manifest.arp[0], manifest.arp[1]);
    let l = |n: &str| load_layer(dir, n);
    let (runways, taxiways, aprons) = (l("runwayelement"), l("taxiwayelement"), l("apronelement"));
    let thresholds = l("runwaythreshold");
    let buildings = l("verticalpolygonalstructure");
    let water = l("water");
    let displaced = l("runwaydisplacedarea");
    let blastpads = l("blastpad");
    let stopways = l("stopway");
    let holds = l("taxiwayholdingposition");
    let stands = l("parkingstandlocation");
    let hotspots = l("hotspot");
    let arp = l("aerodromereferencepoint");
    let freqs = l("frequencyarea");
    let towers = l("verticalpointstructure");
    let construction = l("constructionarea");

    // Extent: the movement area, padded.
    let mut ext = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    fn grow(ext: &mut [f64; 4], frame: &LocalFrame, g: &Geometry<f64>) {
        if let Some(r) = geo::BoundingRect::bounding_rect(g) {
            let a = frame.forward(r.min().x, r.min().y);
            let b = frame.forward(r.max().x, r.max().y);
            ext[0] = ext[0].min(a.x).min(b.x);
            ext[1] = ext[1].min(a.y).min(b.y);
            ext[2] = ext[2].max(a.x).max(b.x);
            ext[3] = ext[3].max(a.y).max(b.y);
        }
    }
    for f in &runways {
        grow(&mut ext, &frame, &f.geom);
    }
    if ext[0] != f64::MAX {
        // Pavement may only extend the runway box by so much; a stray polygon far away
        // (a long access road tagged as apron) must not shrink the whole chart.
        let (rw, rh) = ((ext[2] - ext[0]).max(1500.0), (ext[3] - ext[1]).max(1500.0));
        let allowed = [ext[0] - 0.6 * rw, ext[1] - 0.6 * rh, ext[2] + 0.6 * rw, ext[3] + 0.6 * rh];
        for f in taxiways.iter().chain(&aprons) {
            if let Some(r) = geo::BoundingRect::bounding_rect(&f.geom) {
                let a = frame.forward(r.min().x, r.min().y);
                let b = frame.forward(r.max().x, r.max().y);
                if a.x >= allowed[0] && a.y >= allowed[1] && b.x <= allowed[2] && b.y <= allowed[3] {
                    grow(&mut ext, &frame, &f.geom);
                }
            }
        }
    }
    if ext[0] == f64::MAX {
        let b = manifest.bbox.unwrap_or([manifest.arp[1] - 0.02, manifest.arp[0] - 0.02, manifest.arp[1] + 0.02, manifest.arp[0] + 0.02]);
        let a = frame.forward(b[0], b[1]);
        let c = frame.forward(b[2], b[3]);
        ext = [a.x, a.y, c.x, c.y];
    }
    let pad = 0.05 * (ext[2] - ext[0]).max(ext[3] - ext[1]).max(200.0);
    ext = [ext[0] - pad, ext[1] - pad, ext[2] + pad, ext[3] + pad];

    // Which way round the page goes is decided by measuring, not by a rule of thumb about the
    // aerodrome's own shape. The box the map gets is not the page: the header takes 132 points
    // off the top of it, so landscape leaves a box near twice as wide as it is tall while
    // portrait leaves one slightly taller than wide. An aerodrome a little wider than tall --
    // Kennedy, at about four to three -- passed a "wider than tall, so landscape" test and then
    // sat in the middle of that very wide box with a third of the sheet blank on either side.
    // Trying both and keeping whichever draws the aerodrome larger cannot pick the worse one.
    let fit = |page_w: f32, page_h: f32| {
        let (x0, y0, x1, y1) = (MARGIN, MARGIN + FOOTER_H, page_w - MARGIN, page_h - MARGIN - HEADER_H);
        let (w, h) = (x1 - x0, y1 - y0);
        let scale = (w / (ext[2] - ext[0]) as f32).min(h / (ext[3] - ext[1]) as f32);
        (scale, x0, y0, x1, y1, w, h)
    };
    let wide = fit(A4_LONG, A4_SHORT);
    let tall = fit(A4_SHORT, A4_LONG);
    let landscape = wide.0 >= tall.0;
    let (page_w, page_h) = if landscape { (A4_LONG, A4_SHORT) } else { (A4_SHORT, A4_LONG) };
    let (scale, x0, y0, x1, y1, w, h) = if landscape { wide } else { tall };
    let ox = x0 + (w - (ext[2] - ext[0]) as f32 * scale) / 2.0 - ext[0] as f32 * scale;
    let oy = y0 + (h - (ext[3] - ext[1]) as f32 * scale) / 2.0 - ext[1] as f32 * scale;
    let map = MapArea { frame, scale, ox, oy };

    let mut c = Content::new();
    // ---- map (clipped) ----
    c.save_state();
    c.rect(x0, y0, w, h);
    c.clip_nonzero();
    c.end_path();
    c.set_line_join(pdf_writer::types::LineJoinStyle::RoundJoin);
    for f in &water {
        map.fill_geom(&mut c, &f.geom, Some([0.86, 0.91, 0.96]), None);
    }
    for f in &aprons {
        map.fill_geom(&mut c, &f.geom, Some([0.88, 0.88, 0.88]), None);
    }
    for f in &taxiways {
        map.fill_geom(&mut c, &f.geom, Some([0.74, 0.74, 0.74]), None);
    }
    for f in &construction {
        map.fill_geom(&mut c, &f.geom, Some([0.93, 0.93, 0.93]), Some(([0.4, 0.4, 0.4], 0.5)));
    }
    for f in displaced.iter().chain(&blastpads).chain(&stopways) {
        map.fill_geom(&mut c, &f.geom, Some([0.55, 0.55, 0.55]), None);
    }
    for f in &runways {
        map.fill_geom(&mut c, &f.geom, Some([0.08, 0.08, 0.08]), None);
    }
    for f in &buildings {
        let terminal = prop_f64(&f.props, "plysttyp") == Some(1.0);
        map.fill_geom(&mut c, &f.geom, Some(if terminal { [0.22, 0.22, 0.22] } else { [0.42, 0.42, 0.42] }), None);
    }
    for f in &holds {
        map.stroke_lines(&mut c, &f.geom, [0.0, 0.0, 0.0], 1.1);
    }
    for f in &hotspots {
        map.fill_geom(&mut c, &f.geom, None, Some(([0.65, 0.0, 0.0], 1.2)));
        if let (Some(id), Some(ct)) = (prop_str(&f.props, "idhot"), f.geom.centroid()) {
            let (x, y) = map.pt(ct.0);
            text(&mut c, BOLD, 6.0, x + 3.0, y + 3.0, &id, 0.65);
        }
    }
    // Stand numbers (skipped on very large aprons to keep the sheet readable).
    // Stand numbers only where they stay legible (big hubs get a gate chart, not this).
    if stands.len() <= 220 {
        for f in &stands {
            if let (Geometry::Point(p), Some(id)) = (&f.geom, prop_str(&f.props, "idstd")) {
                let (x, y) = map.pt(p.0);
                text(&mut c, FONT, 5.0, x - text_w(5.0, &id) / 2.0, y - 1.6, &id, 0.25);
            }
        }
    }
    // Runway dimensions on the runway, designators at the ends.
    for f in &runways {
        let (Some(ct), Some(len)) = (f.geom.centroid(), prop_f64(&f.props, "length")) else { continue };
        let width = prop_f64(&f.props, "width").unwrap_or(0.0);
        let brg = prop_f64(&f.props, "brngtrue").unwrap_or(0.0) as f32;
        let (x, y) = map.pt(ct.0);
        let label = format!("{:.0} x {:.0} m", len, width);
        if len as f32 * scale > text_w(7.0, &label) + 40.0 {
            text_rot(&mut c, FONT, 7.0, x, y, 90.0 - brg, &label, 1.0);
        }
    }
    for f in &thresholds {
        let (Geometry::Point(p), Some(id)) = (&f.geom, prop_str(&f.props, "idthr")) else { continue };
        let brg = prop_f64(&f.props, "brngtrue").unwrap_or(0.0);
        let (sn, cs) = brg.to_radians().sin_cos();
        let back = 20.0 / scale as f64; // metres behind the threshold
        let l = map.frame.forward(p.0.x, p.0.y);
        let q = Coord { x: l.x - sn * back, y: l.y - cs * back };
        let (x, y) = (map.ox + q.x as f32 * map.scale, map.oy + q.y as f32 * map.scale);
        label_boxed(&mut c, BOLD, 11.0, x, y, &id);
    }
    // Taxiway letters: one per designator, on its largest piece.
    let mut best: BTreeMap<String, (f64, Coord<f64>)> = BTreeMap::new();
    for f in &taxiways {
        let Some(id) = prop_str(&f.props, "idlin") else { continue };
        let area = match &f.geom {
            Geometry::Polygon(p) => p.unsigned_area(),
            Geometry::MultiPolygon(m) => m.unsigned_area(),
            _ => 0.0,
        };
        let Some(ct) = f.geom.centroid() else { continue };
        let e = best.entry(id).or_insert((0.0, ct.0));
        if area > e.0 {
            *e = (area, ct.0);
        }
    }
    for (id, (_, ct)) in &best {
        let (x, y) = map.pt(*ct);
        label_boxed(&mut c, BOLD, 8.5, x, y, id);
    }
    // Terminal names.
    let mut named: BTreeMap<String, (f64, Coord<f64>)> = BTreeMap::new();
    for f in &buildings {
        let (Some(name), Some(ct)) = (prop_str(&f.props, "name"), f.geom.centroid()) else { continue };
        if !matches!(prop_f64(&f.props, "plysttyp"), Some(1.0) | Some(3.0)) {
            continue; // terminals and the tower; hangars and cargo sheds stay unlabelled
        }
        let area = match &f.geom {
            Geometry::Polygon(p) => p.unsigned_area(),
            Geometry::MultiPolygon(m) => m.unsigned_area(),
            _ => 0.0,
        };
        // "Terminal 4 (2)" style duplicates collapse onto the largest piece.
        let base = name.split(" (").next().unwrap_or(&name).trim().to_string();
        let e = named.entry(base).or_insert((0.0, ct.0));
        if area > e.0 {
            *e = (area, ct.0);
        }
    }
    for (name, (area, ct)) in &named {
        if *area < 6000.0 {
            continue;
        }
        let (x, y) = map.pt(*ct);
        let short: String = name.chars().take(26).collect::<String>().to_uppercase();
        label_boxed(&mut c, FONT, 6.5, x, y, &short);
    }
    // Tower and ARP symbols.
    for f in &towers {
        if !matches!(prop_str(&f.props, "pntsttyp").as_deref(), Some("3") | Some("tower")) {
            continue;
        }
        if let Geometry::Point(p) = &f.geom {
            let (x, y) = map.pt(p.0);
            c.set_fill_gray(0.0);
            c.rect(x - 2.5, y - 2.5, 5.0, 5.0);
            c.fill_nonzero();
            text(&mut c, BOLD, 7.0, x + 4.5, y - 2.5, "TWR", 0.0);
        }
    }
    if let Some(Geometry::Point(p)) = arp.first().map(|f| &f.geom) {
        let (x, y) = map.pt(p.0);
        c.set_stroke_gray(0.0);
        c.set_line_width(0.8);
        circle(&mut c, x, y, 4.0);
        c.stroke();
        c.set_fill_gray(0.0);
        circle(&mut c, x, y, 1.2);
        c.fill_nonzero();
        text(&mut c, BOLD, 7.0, x + 6.0, y - 2.5, "ARP", 0.0);
    }
    c.restore_state();

    // Map frame, north arrow, scale bar.
    c.set_stroke_gray(0.0);
    c.set_line_width(0.8);
    c.rect(x0, y0, w, h);
    c.stroke();
    let (nx, ny) = (x1 - 22.0, y1 - 30.0);
    c.set_fill_gray(0.0);
    c.move_to(nx, ny + 16.0);
    c.line_to(nx - 5.0, ny);
    c.line_to(nx, ny + 4.0);
    c.line_to(nx + 5.0, ny);
    c.close_path();
    c.fill_nonzero();
    text(&mut c, BOLD, 8.0, nx - 3.0, ny + 19.0, "N", 0.0);
    let bar_m = [100.0, 200.0, 250.0, 500.0, 1000.0, 2000.0, 5000.0].into_iter().filter(|m| m * scale <= 110.0).last().unwrap_or(100.0);
    let bar_w = bar_m * scale;
    let (bx, by) = (x0 + 12.0, y0 + 12.0);
    c.set_fill_gray(1.0);
    c.rect(bx - 4.0, by - 4.0, bar_w + 8.0, 18.0);
    c.fill_nonzero();
    c.set_stroke_gray(0.0);
    c.set_line_width(0.6);
    c.rect(bx, by, bar_w, 4.0);
    c.stroke();
    c.set_fill_gray(0.0);
    c.rect(bx, by, bar_w / 2.0, 4.0);
    c.fill_nonzero();
    text(&mut c, FONT, 6.0, bx - 1.5, by + 6.0, "0", 0.0);
    let lbl = format!("{} m / {} ft", bar_m as i64, (bar_m as f64 * 3.28084).round() as i64);
    text_right(&mut c, FONT, 6.0, bx + bar_w + 1.0, by + 6.0, &lbl, 0.0);

    // ---- header ----
    let top = page_h - MARGIN;
    c.set_stroke_gray(0.0);
    c.set_line_width(1.2);
    c.move_to(x0, y1 + 6.0);
    c.line_to(x1, y1 + 6.0);
    c.stroke();
    let name = manifest.name.clone().unwrap_or_else(|| manifest.icao.clone());
    let arp_props = arp.first().map(|f| f.props.clone()).unwrap_or_default();
    let city = prop_str(&arp_props, "city");
    let country = manifest.country.clone().or_else(|| prop_str(&arp_props, "country"));
    text(&mut c, BOLD, 18.0, x0, top - 15.0, &name.to_uppercase(), 0.0);
    let mut place: Vec<String> = Vec::new();
    if let Some(c) = city {
        place.push(c);
    }
    if let Some(c) = country {
        // Skip the country when the city string already carries one ("New York, USA").
        if !place.iter().any(|p| p.contains(',') || p.to_lowercase().contains(&c.to_lowercase())) {
            place.push(c);
        }
    }
    text(&mut c, FONT, 9.0, x0, top - 26.0, &place.join(", "), 0.2);
    let codes = match &manifest.iata {
        Some(i) => format!("{} / {}", manifest.icao, i),
        None => manifest.icao.clone(),
    };
    text_right(&mut c, BOLD, 18.0, x1, top - 15.0, &codes, 0.0);
    text_right(&mut c, FONT, 8.0, x1, top - 26.0, "AIRPORT DIAGRAM", 0.2);
    let elev = manifest.elevation_ft.map(|e| format!("ELEV {e:.0}'")).unwrap_or_default();
    let ta = prop_f64(&arp_props, "transalt").map(|t| format!("   TRANS ALT {t:.0}'")).unwrap_or_default();
    let tl = prop_str(&arp_props, "translvl").map(|t| format!("   TRANS LEVEL {t}")).unwrap_or_default();
    text(&mut c, BOLD, 8.5, x0, top - 40.0, &format!("{elev}   ARP {}{ta}{tl}", fmt_coord(manifest.arp[0], manifest.arp[1])), 0.0);

    // Runway table (left).
    let mut ry = top - 55.0;
    text(&mut c, BOLD, 7.5, x0, ry, "RWY", 0.0);
    text(&mut c, BOLD, 7.5, x0 + 56.0, ry, "DIMENSIONS", 0.0);
    text(&mut c, BOLD, 7.5, x0 + 176.0, ry, "SURFACE", 0.0);
    let surf = crate::model::codes::legend();
    let mut rows: BTreeMap<String, &Feat> = BTreeMap::new();
    for f in &runways {
        if let Some(id) = prop_str(&f.props, "idrwy") {
            let e = rows.entry(id).or_insert(f);
            if prop_f64(&f.props, "length").unwrap_or(0.0) > prop_f64(&e.props, "length").unwrap_or(0.0) {
                *e = f;
            }
        }
    }
    for (id, f) in rows.iter().take(7) {
        ry -= 9.5;
        let len = prop_f64(&f.props, "length").unwrap_or(0.0);
        let wid = prop_f64(&f.props, "width").unwrap_or(0.0);
        let st = prop_str(&f.props, "surftype").and_then(|s| surf.get("surftype").and_then(|m| m.get(&s)).and_then(Value::as_str).map(str::to_uppercase)).unwrap_or_default();
        text(&mut c, BOLD, 8.0, x0, ry, id, 0.0);
        text(&mut c, FONT, 8.0, x0 + 56.0, ry, &format!("{:.0} x {:.0} m  ({:.0}' x {:.0}')", len, wid, len * 3.28084, wid * 3.28084), 0.0);
        text(&mut c, FONT, 8.0, x0 + 176.0, ry, &st, 0.0);
    }

    // Frequencies (right).
    let fx = x1 - 210.0;
    let mut fy = top - 55.0;
    c.set_stroke_gray(0.0);
    c.set_line_width(0.5);
    c.move_to(fx - 8.0, top - 46.0);
    c.line_to(fx - 8.0, y1 + 10.0);
    c.stroke();
    text(&mut c, BOLD, 7.5, fx, fy, "COMMUNICATIONS", 0.0);
    let mut fr: Vec<(String, String)> = freqs.iter().filter_map(|f| Some((prop_str(&f.props, "name").unwrap_or_else(|| "FREQ".into()), prop_str(&f.props, "frq")?))).collect();
    fr.sort();
    fr.dedup();
    let rows_fit = (((top - 55.0) - (y1 + 12.0)) / 9.0).floor().max(1.0) as usize;
    for (n, q) in fr.iter().take(rows_fit) {
        fy -= 9.0;
        let short: String = n.chars().take(26).collect();
        text(&mut c, FONT, 7.5, fx, fy, &short.to_uppercase(), 0.0);
        text_right(&mut c, BOLD, 7.5, x1, fy, q, 0.0);
    }
    if fr.is_empty() {
        text(&mut c, FONT, 6.5, fx, fy - 7.5, "no frequencies published", 0.4);
    }

    // ---- footer ----
    let fy0 = y0 - 9.0;
    text(&mut c, FONT, 5.5, x0, fy0, &format!("amdbgen {} - generated {} - sources: {} - (c) OpenStreetMap contributors (ODbL), X-Plane Scenery Gateway", env!("CARGO_PKG_VERSION"), manifest.generated.get(..10).unwrap_or(&manifest.generated), manifest.sources.join(", ")), 0.35);
    text(&mut c, BOLD, 6.0, x0, fy0 - 9.0, "FOR FLIGHT SIMULATION ONLY - NOT FOR REAL-WORLD NAVIGATION", 0.0);
    text_right(&mut c, FONT, 5.5, x1, fy0, &format!("{} scale 1:{}", manifest.icao, ((1.0 / scale) * 2834.65).round() as i64), 0.35);

    // ---- assemble ----
    let mut pdf = Pdf::new();
    let (catalog, pages, page, font, bold, stream) = (Ref::new(1), Ref::new(2), Ref::new(3), Ref::new(4), Ref::new(5), Ref::new(6));
    pdf.catalog(catalog).pages(pages);
    pdf.pages(pages).kids([page]).count(1);
    {
        let mut pg = pdf.page(page);
        pg.media_box(Rect::new(0.0, 0.0, page_w, page_h));
        pg.parent(pages);
        pg.contents(stream);
        let mut res = pg.resources();
        let mut fonts = res.fonts();
        fonts.pair(FONT, font);
        fonts.pair(BOLD, bold);
        fonts.finish();
        res.finish();
        pg.finish();
    }
    pdf.type1_font(font).base_font(Name(b"Helvetica")).encoding_predefined(Name(b"WinAnsiEncoding"));
    pdf.type1_font(bold).base_font(Name(b"Helvetica-Bold")).encoding_predefined(Name(b"WinAnsiEncoding"));
    let data = c.finish();
    pdf.stream(stream, &data);
    let bytes = pdf.finish();
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;
    Ok(bytes.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_pdf_for_a_minimal_airport() {
        let dir = std::env::temp_dir().join(format!("amdbgen-chart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), r#"{"icao":"TEST","iata":"TST","name":"Test Field","country":"XX","arp":[50.0,8.0],"elevation_ft":300,"projection":"EPSG:4326","formats":[],"generated":"2026-09-13T00:00:00Z","generator":"t","sources":["xplane"],"bbox":null,"layers":{},"warnings":[]}"#).unwrap();
        std::fs::write(dir.join("runwayelement.geojson"), r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[7.99,49.999],[8.01,49.999],[8.01,50.0],[7.99,50.0],[7.99,49.999]]]},"properties":{"idrwy":"09/27","length":1400,"width":45,"brngtrue":90,"surftype":4}}]}"#).unwrap();
        std::fs::write(dir.join("runwaythreshold.geojson"), r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[7.99,49.9995]},"properties":{"idthr":"09","brngtrue":90}}]}"#).unwrap();
        let out = dir.join("chart.pdf");
        let n = write(&dir, &out).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert!(n > 500 && bytes.starts_with(b"%PDF"));
        assert!(n > 1500, "runway and threshold must be drawn (got {n} bytes)");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
