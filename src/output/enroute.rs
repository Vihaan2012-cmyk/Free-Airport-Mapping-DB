//! Enroute charts as map tiles, for the flight bags whose map asks Navigraph's enroute
//! service for its pictures (the iniBuilds A350's Leaflet map asks
//! `enroute-bitmap.charts.api-v2.navigraph.com/styles/{style}/{z}/{x}/{y}@2x.png`). The
//! same squares, 512 pixels a side on the web mercator grid, drawn from an aircraft's
//! navigation database or the simulator's own data instead: airways, fixes, navaids,
//! airports, controlled and restricted airspace, flight information regions and the grid
//! minimum off-route altitudes, over coastlines from Natural Earth (public domain).
//!
//! Four styles, as Navigraph names them: `ifr.lo` (low airways, MORA, terminal airspace),
//! `ifr.hi` (high airways and the regions), `vfr` (airspace and airfields) and `world`
//! (the ground, the regions and the large airports). Tiles are drawn by day; the night
//! side is the same picture turned over (`canvas::png_to_night`) when it is served.
//!
//! `run` draws them ahead, to a zoom, into `<out>/<cycle>/<style>/z/x/y.png`. Most of the
//! world is open sea or bare land with nothing on it, at any zoom: such a tile is plain
//! (`plain`), and so is every tile under it, so none of them is drawn or kept. `meta.json`
//! beside the styles says how deep each style was drawn, and the bridge answers a tile not
//! kept there with the plain colour; any other it draws the first time it is asked for.

use crate::output::approach::text_width;
use crate::output::canvas::{Canvas, Raster};
use anyhow::{anyhow, Result};
use pdf_writer::Name;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const F: Name = Name(b"F");
const B: Name = Name(b"B");
/// A tile is 256 points drawn at two pixels to the point: the `@2x` picture.
const TILE_PT: f32 = 256.0;
const SCALE: f32 = 2.0;
/// The styles drawn, as Navigraph names them.
pub const STYLES: [&str; 4] = ["ifr.lo", "ifr.hi", "vfr", "world"];
/// The deepest zoom a tile is drawn at.
pub const DEEPEST: u8 = 18;
/// Leeway round a tile, in points, for a line or symbol drawn across its edge.
const EDGE: f32 = 2.0;

/// An airway between two fixes. `level` is the database's: `H` high, `L` low, `B` both.
struct Segment {
    ident: String,
    level: char,
    a: (f64, f64),
    b: (f64, f64),
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Fix,
    Vor,
    Dme,
    Ndb,
    /// An airport, and whether it is a large one.
    Airport(bool),
}

struct Point {
    ident: String,
    kind: Kind,
    /// Frequency for a beacon: megahertz, kilohertz for an NDB.
    freq: Option<f64>,
    /// Which airway levels a fix is on: `H`, `L` or both.
    levels: (bool, bool),
    lat: f64,
    lon: f64,
}

#[derive(Clone, Copy, PartialEq)]
enum AreaKind {
    /// Controlled airspace of a class (A to G).
    Class(char),
    /// Restrictive airspace of a type: P prohibited, R restricted, D danger, and so on.
    Restricted(char),
    Region,
}

struct Area {
    kind: AreaKind,
    label: String,
    /// Its outline on the mercator square (see `mercator`), unwrapped so that no step
    /// crosses 180 degrees of longitude.
    ring: Vec<(f64, f64)>,
    /// south, west, north, east.
    bbox: (f64, f64, f64, f64),
    /// Where its name goes: the mean of its points, (lat, lon).
    centre: (f64, f64),
}

/// The ground: polygons of land and lakes on the mercator square, with their extents.
struct Shape {
    lake: bool,
    rings: Vec<Vec<(f64, f64)>>,
    bbox: (f64, f64, f64, f64),
}

impl Shape {
    /// A polygon from its rings of (lon, lat), the first the outside.
    fn new(lake: bool, rings: Vec<Vec<(f64, f64)>>) -> Shape {
        let (w, s, e, n) = rings[0].iter().fold((180.0f64, 90.0f64, -180.0f64, -90.0f64), |(w, s, e, n), &(lon, lat)| (w.min(lon), s.min(lat), e.max(lon), n.max(lat)));
        let rings = rings.into_iter().map(|r| r.into_iter().map(|(lon, lat)| mercator(lat, lon)).collect()).collect();
        Shape { lake, rings, bbox: (s, w, n, e) }
    }
}

/// A place on the square the web map is drawn on, x east from 180W and y south from
/// 85.05N, both 0 to 1 (x past them for a longitude taken past 180). Worked out once for
/// every point of an outline, so that a tile only scales and shifts them: the logarithm and
/// tangent per point were most of drawing an airspace-dense tile.
fn mercator(lat: f64, lon: f64) -> (f64, f64) {
    let lat = lat.clamp(-85.05, 85.05).to_radians();
    ((lon + 180.0) / 360.0, (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0)
}

/// The coastlines and lake shores, from Natural Earth.
pub struct Ground {
    shapes: Vec<Shape>,
}

/// What a tile with nothing on it is: all sea, or all land.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Plain {
    Sea,
    Land,
}

/// Everything a tile is drawn from, read once.
pub struct Data {
    pub cycle: String,
    segments: Vec<Segment>,
    segment_cells: HashMap<(i32, i32), Vec<u32>>,
    points: Vec<Point>,
    point_cells: HashMap<(i32, i32), Vec<u32>>,
    areas: Vec<Area>,
    mora: HashMap<(i32, i32), f64>,
    ground: Ground,
}

/// Where the airways, fixes, navaids and airports are read from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// The navigation database in use (see `navdata::use_database`).
    Database,
    /// The simulator's own navigation data, of its current cycle. It has no airspace,
    /// regions or MORA: those still come from the database in use.
    Simulator,
}

impl Source {
    /// `--from`: a navigation database, `sim` for the simulator's own data, or nothing for
    /// the newest database on the machine. A database named is the one read from then on.
    pub fn from_arg(from: Option<&Path>) -> Result<Source> {
        match from {
            Some(p) if p.as_os_str().eq_ignore_ascii_case("sim") => Ok(Source::Simulator),
            Some(db) => {
                crate::sources::navdata::use_database(db).ok_or_else(|| anyhow!("{} is not a navigation database", db.display()))?;
                Ok(Source::Database)
            }
            None => Ok(Source::Database),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Source::Database => "database",
            Source::Simulator => "simulator",
        }
    }
}

fn cell(lat: f64, lon: f64) -> (i32, i32) {
    (lat.floor() as i32, lon.floor() as i32)
}

/// A ring of airspace from its rows: (via, lat, lon, arc origin, arc distance), in order.
fn ring(rows: &[(String, Option<(f64, f64)>, Option<(f64, f64)>, Option<f64>)]) -> Vec<(f64, f64)> {
    use crate::sources::navdata::{arc, bearing};
    let mut out = Vec::new();
    for (k, (via, here, origin, radius)) in rows.iter().enumerate() {
        let next = rows.get(k + 1).or(rows.first()).and_then(|n| n.1);
        match via.trim().chars().next() {
            Some('C') => {
                if let (Some(o), Some(r)) = (origin, radius) {
                    out.extend(arc(*o, *r, 0.0, 360.0, true));
                }
            }
            Some(turn @ ('R' | 'L')) => {
                if let (Some(h), Some(n), Some(o), Some(r)) = (here, next, origin, radius) {
                    out.extend(arc(*o, *r, bearing(*o, *h), bearing(*o, n), turn == 'R'));
                }
            }
            _ => out.extend(*here),
        }
    }
    // No step across the antimeridian: each point is taken on the side of the one before.
    for i in 1..out.len() {
        let prev = out[i - 1].1;
        while out[i].1 - prev > 180.0 {
            out[i].1 -= 360.0;
        }
        while prev - out[i].1 > 180.0 {
            out[i].1 += 360.0;
        }
    }
    out
}

fn extent(points: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    points.iter().fold((90.0, 1000.0, -90.0, -1000.0), |(s, w, n, e), &(lat, lon)| (s.min(lat), w.min(lon), n.max(lat), e.max(lon)))
}

/// Rows of a table, grouped by `key` in the order given: the parts of each airspace.
type Grouped = Vec<(String, String, Vec<(String, Option<(f64, f64)>, Option<(f64, f64)>, Option<f64>)>)>;

fn airspace_rows(table_ending: &str, key: &str, label: &str, kind_col: &str, lat: &str, lon: &str) -> Grouped {
    let Some((c, t)) = crate::sources::navdata::open_table(table_ending) else { return Vec::new() };
    let sql = format!("select {key}, {kind_col}, {label}, boundary_via, {lat}, {lon}, arc_origin_latitude, arc_origin_longitude, arc_distance from \"{t}\" order by {key}, seqno");
    let Ok(mut st) = c.prepare(&sql) else { return Vec::new() };
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            r.get::<_, Option<f64>>(4)?.zip(r.get::<_, Option<f64>>(5)?),
            r.get::<_, Option<f64>>(6)?.zip(r.get::<_, Option<f64>>(7)?),
            r.get::<_, Option<f64>>(8)?,
        ))
    });
    let Ok(rows) = rows else { return Vec::new() };
    // One airspace a key, its rows in boundary order.
    let mut out: Grouped = Vec::new();
    for (k, kind, name, via, here, origin, radius) in rows.flatten() {
        match out.last_mut() {
            Some(g) if g.0 == k => g.2.push((via, here, origin, radius)),
            _ => out.push((k, format!("{kind}|{name}"), vec![(via, here, origin, radius)])),
        }
    }
    out
}

/// The Natural Earth polygons, fetched once and kept.
fn natural_earth(http: &crate::sources::http::Http, cache: &crate::cache::Cache, name: &str, lake: bool) -> Vec<Shape> {
    let url = format!("https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/{name}.geojson");
    let mut forever = cache.clone();
    forever.max_age = None;
    let Ok(text) = forever.get_or_fetch_text(&format!("naturalearth/{name}.geojson"), || http.get_text(&url)) else {
        crate::term::warn(&format!("Natural Earth {name} could not be fetched: tiles are drawn without it"));
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let ring = |r: &serde_json::Value| -> Vec<(f64, f64)> { r.as_array().into_iter().flatten().filter_map(|p| Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?))).collect() };
    let mut out = Vec::new();
    for f in v["features"].as_array().into_iter().flatten() {
        let g = &f["geometry"];
        let polys: Vec<&serde_json::Value> = match g["type"].as_str() {
            Some("Polygon") => vec![&g["coordinates"]],
            Some("MultiPolygon") => g["coordinates"].as_array().map(|a| a.iter().collect()).unwrap_or_default(),
            _ => Vec::new(),
        };
        for p in polys {
            let rings: Vec<Vec<(f64, f64)>> = p.as_array().into_iter().flatten().map(ring).filter(|r| r.len() >= 3).collect();
            if rings.is_empty() {
                continue;
            }
            out.push(Shape::new(lake, rings));
        }
    }
    out
}

impl Ground {
    pub fn load(http: &crate::sources::http::Http, cache: &crate::cache::Cache) -> Ground {
        let mut shapes = natural_earth(http, cache, "ne_50m_land", false);
        shapes.extend(natural_earth(http, cache, "ne_50m_lakes", true));
        Ground { shapes }
    }

    /// Whether no shore crosses a tile; and if none does, what it is all of.
    fn plain_at(&self, v: &View, margin: f64) -> Option<Plain> {
        for s in self.shapes.iter().filter(|s| v.touches(s.bbox, margin)) {
            let k = v.wrap((s.bbox.1 + s.bbox.3) / 2.0);
            if s.rings.iter().any(|r| path_crosses(r.iter().map(|&m| v.place(m, k)))) {
                return None;
            }
        }
        Some(self.under(v))
    }

    /// What the middle of a tile is, as the tile is painted: land, unless a lake is over it.
    fn under(&self, v: &View) -> Plain {
        let holds = |lake: bool| {
            self.shapes.iter().filter(|s| s.lake == lake && v.touches(s.bbox, 0.0)).any(|s| {
                let k = v.wrap((s.bbox.1 + s.bbox.3) / 2.0);
                encloses_middle(v, &s.rings, k)
            })
        };
        if !holds(true) && holds(false) {
            Plain::Land
        } else {
            Plain::Sea
        }
    }

    /// What a tile none of whose shores cross it is all of.
    pub fn plain_tile_at(&self, z: u8, x: u32, y: u32) -> Plain {
        self.under(&View::new(z, x, y))
    }
}

/// Whether the middle of a tile is inside a shape, its rings taken even-odd as they are
/// filled.
fn encloses_middle(v: &View, rings: &[Vec<(f64, f64)>], k: f64) -> bool {
    let (px, py) = (TILE_PT as f64 / 2.0, TILE_PT as f64 / 2.0);
    let mut inside = false;
    for r in rings {
        let Some(&last) = r.last() else { continue };
        let (mut xj, mut yj) = v.place64(last, k);
        for &m in r {
            let (xi, yi) = v.place64(m, k);
            if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            (xj, yj) = (xi, yi);
        }
    }
    inside
}

/// Fixes on airways, with the levels of the airways they are on.
type OnAirways = HashMap<(String, i64, i64), (f64, f64, bool, bool)>;

fn on_airway(fixes: &mut OnAirways, ident: &str, (lat, lon): (f64, f64), level: char) {
    let e = fixes.entry((ident.to_string(), (lat * 1000.0) as i64, (lon * 1000.0) as i64)).or_insert((lat, lon, false, false));
    e.2 |= level != 'L';
    e.3 |= level != 'H';
}

fn fix_points(fixes: OnAirways) -> Vec<Point> {
    fixes.into_iter().map(|((ident, _, _), (lat, lon, hi, lo))| Point { ident, kind: Kind::Fix, freq: None, levels: (hi, lo), lat, lon }).collect()
}

/// The airways, fixes, navaids and airports of the navigation database in use.
fn from_database(large: &HashSet<String>, medium: &HashSet<String>) -> Result<(Vec<Segment>, Vec<Point>)> {
    let mut segments = Vec::new();
    let mut fixes = OnAirways::new();
    if let Some((c, t)) = crate::sources::navdata::open_table("enroute_airways") {
        let sql = format!("select route_identifier, area_code, waypoint_identifier, waypoint_latitude, waypoint_longitude, waypoint_description_code, flightlevel from \"{t}\" order by route_identifier, area_code, seqno");
        let mut st = c.prepare(&sql)?;
        #[allow(clippy::type_complexity)]
        let rows: Vec<(String, String, String, f64, f64, String, String)> = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                ))
            })?
            .flatten()
            .collect();
        for r in &rows {
            on_airway(&mut fixes, &r.2, (r.3, r.4), r.6.chars().next().unwrap_or('B'));
        }
        for pair in rows.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if a.0 != b.0 || a.1 != b.1 || a.5.chars().nth(1) == Some('E') {
                continue;
            }
            segments.push(Segment { ident: a.0.clone(), level: b.6.chars().next().unwrap_or('B'), a: (a.3, a.4), b: (b.3, b.4) });
        }
    }
    let mut points = fix_points(fixes);
    for (ending, ndb) in [("vhfnavaids", false), ("enroute_ndbnavaids", true)] {
        let Some((c, t)) = crate::sources::navdata::open_table(ending) else { continue };
        let sql = format!("select navaid_identifier, navaid_frequency, navaid_latitude, navaid_longitude, navaid_class from \"{t}\"");
        let Ok(mut st) = c.prepare(&sql) else { continue };
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?, r.get::<_, Option<f64>>(2)?, r.get::<_, Option<f64>>(3)?, r.get::<_, Option<String>>(4)?.unwrap_or_default())));
        for (ident, freq, lat, lon, class) in rows.into_iter().flatten().flatten() {
            let (Some(lat), Some(lon)) = (lat, lon) else { continue };
            let kind = if ndb { Kind::Ndb } else if class.starts_with('V') { Kind::Vor } else { Kind::Dme };
            points.push(Point { ident, kind, freq, levels: (true, true), lat, lon });
        }
    }
    for a in crate::sources::navdata::all_airports() {
        let big = large.contains(&a.icao);
        if big || medium.contains(&a.icao) {
            points.push(Point { ident: a.icao, kind: Kind::Airport(big), freq: None, levels: (true, true), lat: a.lat, lon: a.lon });
        }
    }
    Ok((segments, points))
}

/// The airways, fixes, navaids and airports of the simulator's own navigation data, and
/// its cycle.
fn from_simulator(large: &HashSet<String>, medium: &HashSet<String>) -> Result<(String, Vec<Segment>, Vec<Point>)> {
    use crate::convert::from_sim::{read_enroute, Simulator};
    use crate::convert::model::{AirwayLevel, NavaidKind};
    let cycle = crate::sources::navdata::sim_cycle().ok_or_else(|| anyhow!("the simulator's navigation data says no cycle"))?;
    // FS2020's loose files, which the cycle is read from and whose airways name each fix's
    // neighbours; FS2024's packed data where there are none.
    let nav = read_enroute(&cycle, Simulator::Fs2020).or_else(|_| read_enroute(&cycle, Simulator::Newest))?;
    let mut at: HashMap<(&str, &str), (f64, f64)> = HashMap::new();
    for n in &nav.navaids {
        at.insert((n.ident.as_str(), n.region_code.as_str()), (n.lat, n.lon));
    }
    for w in &nav.waypoints {
        at.insert((w.ident.as_str(), w.region_code.as_str()), (w.lat, w.lon));
    }
    let mut segments = Vec::new();
    let mut fixes = OnAirways::new();
    for a in &nav.airways {
        for l in &a.legs {
            let (Some(&p), Some(&q)) = (at.get(&(l.from_ident.as_str(), l.from_region.as_str())), at.get(&(l.to_ident.as_str(), l.to_region.as_str()))) else { continue };
            let level = match l.level {
                AirwayLevel::High => 'H',
                AirwayLevel::Low => 'L',
                AirwayLevel::Both => 'B',
            };
            on_airway(&mut fixes, &l.from_ident, p, level);
            on_airway(&mut fixes, &l.to_ident, q, level);
            segments.push(Segment { ident: a.ident.clone(), level, a: p, b: q });
        }
    }
    let mut points = fix_points(fixes);
    for n in &nav.navaids {
        let kind = match n.kind {
            NavaidKind::Vor | NavaidKind::Vortac | NavaidKind::VorDme => Kind::Vor,
            NavaidKind::Tacan | NavaidKind::Dme => Kind::Dme,
            NavaidKind::Ndb | NavaidKind::NdbDme => Kind::Ndb,
            NavaidKind::IlsDme => continue,
        };
        points.push(Point { ident: n.ident.clone(), kind, freq: Some(n.frequency).filter(|f| *f > 0.0), levels: (true, true), lat: n.lat, lon: n.lon });
    }
    for a in &nav.airports {
        let big = large.contains(&a.icao);
        if big || medium.contains(&a.icao) {
            points.push(Point { ident: a.icao.clone(), kind: Kind::Airport(big), freq: None, levels: (true, true), lat: a.lat, lon: a.lon });
        }
    }
    Ok((cycle, segments, points))
}

impl Data {
    /// Everything out of the source, the airspace and MORA out of the navigation database
    /// in use (see `navdata::use_database`), and the coastlines.
    pub fn load(http: &crate::sources::http::Http, cache: &crate::cache::Cache, source: Source) -> Result<Data> {
        let mut idx = crate::sources::index::AirportIndex::default();
        let _ = idx.load_ourairports_online(http, cache);
        let large: HashSet<String> = idx.by_icao.values().filter(|e| e.kind.as_deref() == Some("large_airport")).map(|e| e.icao.clone()).collect();
        let medium: HashSet<String> = idx.by_icao.values().filter(|e| e.kind.as_deref() == Some("medium_airport")).map(|e| e.icao.clone()).collect();
        let (cycle, segments, points) = match source {
            Source::Database => {
                let cycle = crate::sources::navdata::airac().ok_or_else(|| anyhow!("no navigation database with airways found"))?;
                let (segments, points) = from_database(&large, &medium)?;
                (cycle, segments, points)
            }
            Source::Simulator => from_simulator(&large, &medium)?,
        };

        // Airspace and the regions.
        let mut areas = Vec::new();
        let groups = [
            ("controlled_airspace", "icao_code || '/' || airspace_center || '/' || multiple_code", "controlled_airspace_name", "airspace_classification", "latitude", "longitude"),
            // Designators repeat from country to country (D123 is in many), so the country is
            // part of what keeps one area's points apart from another's.
            ("restrictive_airspace", "icao_code || '/' || restrictive_airspace_designation || '/' || multiple_code", "restrictive_airspace_name", "restrictive_type", "latitude", "longitude"),
            ("fir_uir", "fir_uir_identifier || '/' || fir_uir_indicator", "fir_uir_name", "fir_uir_indicator", "fir_uir_latitude", "fir_uir_longitude"),
        ];
        for (n, (table, key, label, kind_col, lat, lon)) in groups.iter().enumerate() {
            for (k, kind_label, rows) in airspace_rows(table, key, label, kind_col, lat, lon) {
                let points = ring(&rows);
                if points.len() < 3 {
                    continue;
                }
                let (kind, name) = kind_label.split_once('|').unwrap_or(("", ""));
                let c = kind.trim().chars().next().unwrap_or(' ');
                let kind = match n {
                    0 => AreaKind::Class(c),
                    1 => AreaKind::Restricted(c),
                    _ => AreaKind::Region,
                };
                // A UIR shares the FIR's boundary more often than not: one line is enough.
                if kind == AreaKind::Region && c == 'U' {
                    continue;
                }
                let label = match kind {
                    AreaKind::Restricted(_) => k.split('/').nth(1).unwrap_or("").to_string(),
                    AreaKind::Region => name.trim().to_string(),
                    AreaKind::Class(_) => name.trim().to_string(),
                };
                let n = points.len() as f64;
                let centre = (points.iter().map(|p| p.0).sum::<f64>() / n, points.iter().map(|p| p.1).sum::<f64>() / n);
                areas.push(Area { kind, label, bbox: extent(&points), centre, ring: points.iter().map(|&(lat, lon)| mercator(lat, lon)).collect() });
            }
        }
        let mora = crate::sources::navdata::grid_mora(-90.0, 90.0, -180.0, 180.0).into_iter().map(|(lat, lon, ft)| (cell(lat, lon), ft)).collect();
        let ground = Ground::load(http, cache);

        let mut segment_cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, s) in segments.iter().enumerate() {
            let (la0, la1) = (s.a.0.min(s.b.0).floor() as i32, s.a.0.max(s.b.0).floor() as i32);
            let (lo0, lo1) = (s.a.1.min(s.b.1).floor() as i32, s.a.1.max(s.b.1).floor() as i32);
            // A segment across the antimeridian is filed under both of its ends' cells only.
            if lo1 - lo0 > 180 {
                segment_cells.entry(cell(s.a.0, s.a.1)).or_default().push(i as u32);
                segment_cells.entry(cell(s.b.0, s.b.1)).or_default().push(i as u32);
                continue;
            }
            for la in la0..=la1 {
                for lo in lo0..=lo1 {
                    segment_cells.entry((la, lo)).or_default().push(i as u32);
                }
            }
        }
        let mut point_cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, p) in points.iter().enumerate() {
            point_cells.entry(cell(p.lat, p.lon)).or_default().push(i as u32);
        }
        crate::term::info(&format!(
            "Enroute data, AIRAC {cycle} from the {}: {} airway segments, {} fixes, navaids and airports; from {}: {} airspace outlines, {} MORA squares; {} coastline and lake shapes",
            source.name(),
            segments.len(),
            points.len(),
            crate::sources::navdata::source().unwrap_or_else(|| "no database".to_string()),
            areas.len(),
            HashMap::len(&mora),
            ground.shapes.len()
        ));
        Ok(Data { cycle, segments, segment_cells, points, point_cells, areas, mora, ground })
    }
}

/// One tile's place on the ground.
struct View {
    z: u8,
    x: u32,
    y: u32,
    /// south, west, north, east of the tile itself.
    bounds: (f64, f64, f64, f64),
}

fn lat_of(y: f64, n: f64) -> f64 {
    (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan().to_degrees()
}

impl View {
    fn new(z: u8, x: u32, y: u32) -> View {
        let n = (1u64 << z) as f64;
        let bounds = (lat_of(y as f64 + 1.0, n), x as f64 / n * 360.0 - 180.0, lat_of(y as f64, n), (x as f64 + 1.0) / n * 360.0 - 180.0);
        View { z, x, y, bounds }
    }

    fn centre_lon(&self) -> f64 {
        (self.bounds.1 + self.bounds.3) / 2.0
    }

    /// A position as a point on the tile (y up from its foot), its longitude first taken on
    /// the side of the world the tile is on.
    fn at(&self, lat: f64, lon: f64) -> (f32, f32) {
        let (x, y) = self.at64(lat, lon);
        (x as f32, y as f32)
    }

    fn at64(&self, lat: f64, lon: f64) -> (f64, f64) {
        self.place64(mercator(lat, lon), 0.0)
    }

    /// A place on the mercator square as a point on the tile, shifted by `k` degrees of
    /// longitude.
    fn place(&self, m: (f64, f64), k: f64) -> (f32, f32) {
        let (x, y) = self.place64(m, k);
        (x as f32, y as f32)
    }

    fn place64(&self, (mx, my): (f64, f64), k: f64) -> (f64, f64) {
        let n = (1u64 << self.z) as f64;
        let x = (mx + k / 360.0) * n - self.x as f64;
        let y = my * n - self.y as f64;
        (x * TILE_PT as f64, TILE_PT as f64 - y * TILE_PT as f64)
    }

    /// The shift by whole turns that brings a longitude nearest the tile.
    fn wrap(&self, lon: f64) -> f64 {
        let c = self.centre_lon();
        let mut k = 0.0;
        while lon + k - c > 180.0 {
            k -= 360.0;
        }
        while c - (lon + k) > 180.0 {
            k += 360.0;
        }
        k
    }

    /// Cells of a degree the tile covers, with a margin for what is drawn across its edge.
    fn cells(&self, margin_deg: f64) -> Vec<(i32, i32)> {
        let (s, w, n, e) = self.bounds;
        let mut out = Vec::new();
        for la in (s - margin_deg).floor() as i32..=(n + margin_deg).floor() as i32 {
            for lo in (w - margin_deg).floor() as i32..=(e + margin_deg).floor() as i32 {
                let lo = ((lo + 180).rem_euclid(360)) - 180;
                out.push((la, lo));
            }
        }
        out
    }

    fn touches(&self, (s, w, n, e): (f64, f64, f64, f64), margin: f64) -> bool {
        let (ts, tw, tn, te) = self.bounds;
        if n < ts - margin || s > tn + margin {
            return false;
        }
        // Longitude, on whichever side of the world the box is nearer.
        let k = self.wrap((w + e) / 2.0);
        !(e + k < tw - margin || w + k > te + margin)
    }
}

/// Whether a straight line between two points of a tile passes over it, `EDGE` round it
/// included.
fn crosses((x1, y1): (f32, f32), (x2, y2): (f32, f32)) -> bool {
    let (lo, hi) = (-EDGE, TILE_PT + EDGE);
    if x1.max(x2) < lo || x1.min(x2) > hi || y1.max(y2) < lo || y1.min(y2) > hi {
        return false;
    }
    // The part of the line inside the square (Liang and Barsky).
    let (dx, dy) = (x2 - x1, y2 - y1);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [(-dx, x1 - lo), (dx, hi - x1), (-dy, y1 - lo), (dy, hi - y1)] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

/// Whether a closed outline, as points on the tile, passes over it.
fn path_crosses(points: impl Iterator<Item = (f32, f32)> + Clone) -> bool {
    let Some(mut prev) = points.clone().last() else { return false };
    for here in points {
        if crosses(prev, here) {
            return true;
        }
        prev = here;
    }
    false
}

/// Labels placed so far, so that none is written over another.
#[derive(Default)]
struct Labels(Vec<(f32, f32, f32, f32)>);

impl Labels {
    fn take(&mut self, x: f32, y: f32, w: f32, h: f32) -> bool {
        if self.0.iter().any(|&(a, b, c, d)| x < a + c && a < x + w && y < b + d && b < y + h) {
            return false;
        }
        self.0.push((x, y, w, h));
        true
    }
}

/// A style's look, by day.
struct Look {
    sea: (f32, f32, f32),
    land: (f32, f32, f32),
}

fn look(style: &str) -> Look {
    match style {
        "world" => Look { sea: (0.78, 0.86, 0.93), land: (0.95, 0.94, 0.89) },
        "vfr" => Look { sea: (0.80, 0.89, 0.96), land: (0.93, 0.95, 0.88) },
        _ => Look { sea: (0.88, 0.92, 0.95), land: (0.99, 0.98, 0.95) },
    }
}

/// Which style is asked for: low, high, VFR, world.
fn which(style: &str) -> (bool, bool, bool, bool) {
    (style == "ifr.lo", style == "ifr.hi", style == "vfr", style == "world")
}

/// How an area is outlined in a style at a zoom: colour, width and dash. None where it is
/// not drawn.
fn area_line(kind: AreaKind, style: &str, z: u8) -> Option<((f32, f32, f32), f32, &'static [f32])> {
    let (lo, hi, vfr, world) = which(style);
    let line: ((f32, f32, f32), f32, &'static [f32]) = match kind {
        AreaKind::Region if z >= 2 => ((0.45, 0.55, 0.45), if hi || world { 0.9 } else { 0.6 }, &[4.0, 1.5, 1.0, 1.5]),
        AreaKind::Class('B') if (lo || vfr) && z >= 6 => ((0.15, 0.35, 0.75), 1.3, &[]),
        AreaKind::Class('C') if (lo || vfr) && z >= 6 => ((0.65, 0.20, 0.55), 1.0, &[]),
        AreaKind::Class('D') if (lo || vfr) && z >= 8 => ((0.20, 0.40, 0.80), 0.7, &[3.0, 1.5]),
        AreaKind::Class('A') if vfr && z >= 7 => ((0.45, 0.45, 0.60), 0.6, &[]),
        AreaKind::Class('E') if vfr && z >= 8 => ((0.65, 0.40, 0.65), 0.5, &[1.5, 1.5]),
        AreaKind::Restricted('P') if (lo || vfr) && z >= 6 => ((0.80, 0.15, 0.15), 1.0, &[]),
        AreaKind::Restricted('R' | 'D') if (lo || vfr) && z >= 7 => ((0.80, 0.25, 0.20), 0.7, &[2.5, 1.5]),
        AreaKind::Restricted('W' | 'A' | 'M' | 'T') if vfr && z >= 8 => ((0.70, 0.45, 0.20), 0.5, &[2.0, 1.5]),
        _ => return None,
    };
    Some(line)
}

/// Whether an airway of a level is drawn in a style at a zoom.
fn airway_shown(level: char, style: &str, z: u8) -> bool {
    let (lo, hi, _, _) = which(style);
    z >= 4 && ((lo && level != 'H') || (hi && level != 'L'))
}

/// Whether a fix, navaid or airport is drawn in a style at a zoom.
fn point_shown(p: &Point, style: &str, z: u8) -> bool {
    let (lo, hi, vfr, world) = which(style);
    match p.kind {
        Kind::Airport(true) => z >= if world { 4 } else { 5 },
        Kind::Airport(false) => z >= if world { 7 } else { 6 } && !hi,
        Kind::Vor => (lo || hi || vfr) && z >= 6,
        Kind::Dme => (lo || vfr) && z >= 8,
        Kind::Ndb => (lo || vfr) && z >= 7,
        Kind::Fix => z >= 7 && ((lo && p.levels.1) || (hi && p.levels.0)),
    }
}

/// Whether the grid MORA figures are written in a style at a zoom.
fn mora_shown(style: &str, z: u8) -> bool {
    style == "ifr.lo" && z >= 5
}

/// Whether something is drawn at one zoom or another.
fn ever(shown: impl Fn(u8) -> bool) -> bool {
    (0..=DEEPEST).any(shown)
}

fn shape_path(c: &mut dyn Canvas, v: &View, rings: &[Vec<(f64, f64)>], k: f64) {
    for r in rings {
        for (i, &m) in r.iter().enumerate() {
            let (x, y) = v.place(m, k);
            if i == 0 {
                c.move_to(x, y);
            } else {
                c.line_to(x, y);
            }
        }
        c.close_path();
    }
}

/// Stroke an outline, only the stretches of it over the tile. An airspace's ring can run
/// thousands of points round off the tile, and stroking all of them, dashed above all, was
/// most of a tile's time. Each stretch's dash starts where it would have had the whole ring
/// been stroked, so that it meets the next tile's.
fn stroke_ring(c: &mut dyn Canvas, v: &View, ring: &[(f64, f64)], k: f64, dash: &[f32]) {
    let Some(&first) = ring.first() else { return };
    let period: f32 = dash.iter().sum();
    let (mut prev, mut along, mut open) = (v.place(first, k), 0.0f32, false);
    for &m in ring[1..].iter().chain(std::iter::once(&ring[0])) {
        let here = v.place(m, k);
        if crosses(prev, here) {
            if !open {
                c.set_dash(dash, if period > 0.0 { along % period } else { 0.0 });
                c.move_to(prev.0, prev.1);
                open = true;
            }
            c.line_to(here.0, here.1);
        } else if open {
            c.stroke();
            open = false;
        }
        along += (here.0 - prev.0).hypot(here.1 - prev.1);
        prev = here;
    }
    if open {
        c.stroke();
    }
}

fn inside(x: f32, y: f32, pad: f32) -> bool {
    x >= -pad && x <= TILE_PT + pad && y >= -pad && y <= TILE_PT + pad
}

/// An airway's ends on a tile, the far one taken on the near one's side of the world.
fn airway_ends(v: &View, s: &Segment) -> ((f32, f32), (f32, f32)) {
    let k = v.wrap(s.a.1);
    let a = s.a.1 + k;
    let mut b = s.b.1 + k;
    while b - a > 180.0 {
        b -= 360.0;
    }
    while a - b > 180.0 {
        b += 360.0;
    }
    (v.at(s.a.0, a), v.at(s.b.0, b))
}

/// Whether a tile has nothing on it in a style, at this zoom or any deeper: no shore
/// across it, no airway, fix, navaid, airport, airspace outline or name, nor MORA figure.
/// Then it is one colour, which this says, and so is every tile under it.
pub fn plain(data: &Data, style: &str, z: u8, x: u32, y: u32) -> Option<Plain> {
    let v = View::new(z, x, y);
    let margin = (v.bounds.3 - v.bounds.1) * 0.15;
    if ever(|z| mora_shown(style, z)) {
        for (la, lon) in v.cells(0.0) {
            if data.mora.contains_key(&(la, lon)) {
                let k = v.wrap(lon as f64 + 0.5);
                let (px, py) = v.at(la as f64 + 0.5, lon as f64 + 0.5 + k);
                if inside(px, py, 0.0) {
                    return None;
                }
            }
        }
    }
    for cl in v.cells(margin) {
        for &i in data.segment_cells.get(&cl).into_iter().flatten() {
            let s = &data.segments[i as usize];
            if ever(|z| airway_shown(s.level, style, z)) {
                let (p, q) = airway_ends(&v, s);
                if crosses(p, q) {
                    return None;
                }
            }
        }
        for &i in data.point_cells.get(&cl).into_iter().flatten() {
            let p = &data.points[i as usize];
            if ever(|z| point_shown(p, style, z)) {
                let (px, py) = v.at(p.lat, p.lon + v.wrap(p.lon));
                if inside(px, py, EDGE) {
                    return None;
                }
            }
        }
    }
    for a in data.areas.iter().filter(|a| v.touches(a.bbox, margin)) {
        if !ever(|z| area_line(a.kind, style, z).is_some()) {
            continue;
        }
        let k = v.wrap((a.bbox.1 + a.bbox.3) / 2.0);
        let (cx, cy) = v.at(a.centre.0, a.centre.1 + k);
        if inside(cx, cy, EDGE) || path_crosses(a.ring.iter().map(|&m| v.place(m, k))) {
            return None;
        }
    }
    data.ground.plain_at(&v, margin)
}

/// The picture of a plain tile: the style's sea or land, and nothing else.
pub fn plain_tile(style: &str, plain: Plain) -> Result<Vec<u8>> {
    let lk = look(style);
    let colour = if plain == Plain::Land { lk.land } else { lk.sea };
    let mut r = Raster::new(TILE_PT, TILE_PT, SCALE)?;
    let c: &mut dyn Canvas = &mut r;
    c.set_fill_rgb(colour.0, colour.1, colour.2);
    c.rect(0.0, 0.0, TILE_PT, TILE_PT);
    c.fill_nonzero();
    r.png_compact_bytes()
}

/// One tile, by day, as a PNG: `style` one of `STYLES`.
pub fn tile(data: &Data, style: &str, z: u8, x: u32, y: u32) -> Result<Vec<u8>> {
    if !STYLES.contains(&style) {
        return Err(anyhow!("{style} is not an enroute style"));
    }
    let mut r = Raster::new(TILE_PT, TILE_PT, SCALE)?;
    let c: &mut dyn Canvas = &mut r;
    let v = View::new(z, x, y);
    let span = v.bounds.3 - v.bounds.1;
    let margin = span * 0.15;
    let lk = look(style);
    let (lo, hi, _, world) = which(style);

    // The ground.
    c.set_fill_rgb(lk.sea.0, lk.sea.1, lk.sea.2);
    c.rect(0.0, 0.0, TILE_PT, TILE_PT);
    c.fill_nonzero();
    for lake in [false, true] {
        let colour = if lake { lk.sea } else { lk.land };
        c.set_fill_rgb(colour.0, colour.1, colour.2);
        for s in data.ground.shapes.iter().filter(|s| s.lake == lake && v.touches(s.bbox, margin)) {
            let k = v.wrap((s.bbox.1 + s.bbox.3) / 2.0);
            shape_path(c, &v, &s.rings, k);
            c.fill_even_odd();
        }
    }

    // Grid MORA, in the low style: the figure in thousands, the hundreds smaller after it.
    if mora_shown(style, z) {
        let size = if z >= 7 { 16.0 } else { 11.0 };
        for (la, lon) in v.cells(0.0) {
            let Some(ft) = data.mora.get(&(la, lon)) else { continue };
            let k = v.wrap(lon as f64 + 0.5);
            let (x, y) = v.at(la as f64 + 0.5, lon as f64 + 0.5 + k);
            if !inside(x, y, 0.0) {
                continue;
            }
            let (th, hu) = ((ft / 1000.0).floor() as i64, ((ft % 1000.0) / 100.0).round() as i64);
            let big = th.to_string();
            let w = text_width(B, size, &big);
            c.text_styled(B, size, x - w / 2.0, y - size / 3.0, 0.0, &big, (0.72, 0.74, 0.78));
            c.text_styled(B, size * 0.6, x + w / 2.0 + 0.5, y + size * 0.15, 0.0, &hu.to_string(), (0.72, 0.74, 0.78));
        }
    }

    // Regions, then airspace.
    let mut labels = Labels::default();
    for a in data.areas.iter().filter(|a| v.touches(a.bbox, margin)) {
        let Some((rgb, width, dash)) = area_line(a.kind, style, z) else { continue };
        let k = v.wrap((a.bbox.1 + a.bbox.3) / 2.0);
        c.set_stroke_rgb(rgb.0, rgb.1, rgb.2);
        c.set_line_width(width);
        stroke_ring(c, &v, &a.ring, k, dash);
        // Its name, in the middle of it, where that is on the tile and there is room.
        let named = match a.kind {
            AreaKind::Restricted(_) => z >= 8,
            AreaKind::Region => (hi || world) && (4..=6).contains(&z),
            AreaKind::Class(_) => z >= 9,
        };
        if named && !a.label.is_empty() {
            let (x, y) = v.at(a.centre.0, a.centre.1 + k);
            let size = if a.kind == AreaKind::Region { 7.0 } else { 6.0 };
            let w = text_width(B, size, &a.label);
            if inside(x, y, -12.0) && labels.take(x - w / 2.0, y - 1.0, w, size + 1.0) {
                c.text_styled(B, size, x - w / 2.0, y, 0.0, &a.label, rgb);
            }
        }
    }
    c.set_dash(&[], 0.0);

    // Airways.
    let mut seen: HashSet<u32> = HashSet::new();
    if lo || hi {
        let mut along: Vec<(f32, f32, f32, String)> = Vec::new();
        for cell in v.cells(margin) {
            for &i in data.segment_cells.get(&cell).into_iter().flatten() {
                let s = &data.segments[i as usize];
                if !airway_shown(s.level, style, z) || !seen.insert(i) {
                    continue;
                }
                let ((x1, y1), (x2, y2)) = airway_ends(&v, s);
                let (rgb, width) = if hi { ((0.20, 0.32, 0.55), if s.level == 'B' { 0.7 } else { 0.8 }) } else { ((0.40, 0.50, 0.66), 0.6) };
                c.set_stroke_rgb(rgb.0, rgb.1, rgb.2);
                c.set_line_width(if z >= 7 { width * 1.2 } else { width });
                c.move_to(x1, y1);
                c.line_to(x2, y2);
                c.stroke();
                let len = (x2 - x1).hypot(y2 - y1);
                if z >= 6 && len > 40.0 {
                    let (mx, my) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
                    if inside(mx, my, -4.0) {
                        let mut angle = (y2 - y1).atan2(x2 - x1).to_degrees();
                        if !(-90.0..=90.0).contains(&angle) {
                            angle += 180.0;
                        }
                        along.push((mx, my, angle, s.ident.clone()));
                    }
                }
            }
        }
        // Airway names, along their lines, where there is room.
        let size = if z >= 8 { 6.5 } else { 5.5 };
        for (mx, my, angle, ident) in along {
            let w = text_width(B, size, &ident);
            if !labels.take(mx - w / 2.0 - 1.0, my - size / 2.0 - 1.0, w + 2.0, size + 2.0) {
                continue;
            }
            let (ca, sa) = (angle.to_radians().cos(), angle.to_radians().sin());
            let (sx, sy) = (mx - ca * w / 2.0 + sa * size * 0.35, my - sa * w / 2.0 - ca * size * 0.35);
            c.text_styled(B, size, sx, sy, angle, &ident, if hi { (0.15, 0.25, 0.50) } else { (0.25, 0.35, 0.55) });
        }
    }

    // Fixes, navaids and airports, the more important first so that their labels win.
    let mut here: Vec<&Point> = v.cells(margin).iter().flat_map(|cl| data.point_cells.get(cl).into_iter().flatten()).map(|&i| &data.points[i as usize]).collect();
    let rank = |p: &Point| match p.kind {
        Kind::Airport(true) => 0,
        Kind::Vor => 1,
        Kind::Airport(false) => 2,
        Kind::Ndb | Kind::Dme => 3,
        Kind::Fix => 4,
    };
    here.sort_by_key(|p| rank(p));
    for p in here {
        if !point_shown(p, style, z) {
            continue;
        }
        let k = v.wrap(p.lon);
        let (x, y) = v.at(p.lat, p.lon + k);
        if !inside(x, y, EDGE) {
            continue;
        }
        match p.kind {
            Kind::Airport(big) => {
                let r = if big { 3.2 } else { 2.4 };
                c.set_fill_rgb(0.12, 0.45, 0.28);
                c.set_stroke_rgb(1.0, 1.0, 1.0);
                c.set_line_width(0.6);
                circle(c, x, y, r);
                c.fill_even_odd_and_stroke();
            }
            Kind::Vor | Kind::Dme => {
                c.set_stroke_rgb(0.10, 0.30, 0.62);
                c.set_line_width(0.8);
                if p.kind == Kind::Vor {
                    for i in 0..=6 {
                        let a = (i as f32 * 60.0).to_radians();
                        let (px, py) = (x + 3.5 * a.cos(), y + 3.5 * a.sin());
                        if i == 0 {
                            c.move_to(px, py);
                        } else {
                            c.line_to(px, py);
                        }
                    }
                } else {
                    c.rect(x - 2.5, y - 2.5, 5.0, 5.0);
                }
                c.stroke();
                c.set_fill_rgb(0.10, 0.30, 0.62);
                circle(c, x, y, 0.7);
                c.fill_nonzero();
            }
            Kind::Ndb => {
                c.set_stroke_rgb(0.55, 0.30, 0.55);
                c.set_line_width(0.7);
                c.set_dash(&[0.6, 0.9], 0.0);
                circle(c, x, y, 3.2);
                c.stroke();
                c.set_dash(&[], 0.0);
                c.set_fill_rgb(0.55, 0.30, 0.55);
                circle(c, x, y, 0.8);
                c.fill_nonzero();
            }
            Kind::Fix => {
                c.set_stroke_rgb(0.30, 0.38, 0.50);
                c.set_line_width(0.6);
                c.move_to(x, y + 2.2);
                c.line_to(x - 1.9, y - 1.1);
                c.line_to(x + 1.9, y - 1.1);
                c.close_path();
                c.stroke();
            }
        }
        // The name beside it, where there is room.
        let labelled = match p.kind {
            Kind::Fix => z >= 8,
            Kind::Airport(_) => z >= 6,
            _ => z >= 7,
        };
        if !labelled {
            continue;
        }
        let size = match p.kind {
            Kind::Airport(true) => 7.0,
            Kind::Fix => 5.0,
            _ => 6.0,
        };
        let text = match (p.kind, p.freq) {
            (Kind::Vor | Kind::Dme, Some(f)) if z >= 8 => format!("{} {:.2}", p.ident, f),
            (Kind::Ndb, Some(f)) if z >= 8 => format!("{} {:.0}", p.ident, f),
            _ => p.ident.clone(),
        };
        let font = if matches!(p.kind, Kind::Fix) { F } else { B };
        let w = text_width(font, size, &text);
        let (lx, ly) = (x + 4.0, y + 1.5);
        if !labels.take(lx - 0.5, ly - 1.0, w + 1.0, size + 1.0) {
            continue;
        }
        let rgb = match p.kind {
            Kind::Airport(_) => (0.08, 0.35, 0.20),
            Kind::Vor | Kind::Dme => (0.08, 0.25, 0.55),
            Kind::Ndb => (0.45, 0.22, 0.45),
            Kind::Fix => (0.25, 0.30, 0.40),
        };
        c.text_styled(font, size, lx, ly, 0.0, &text, rgb);
    }
    r.png_compact_bytes()
}

fn circle(c: &mut dyn Canvas, x: f32, y: f32, r: f32) {
    let k = 0.552_284_8 * r;
    c.move_to(x + r, y);
    c.cubic_to(x + r, y + k, x + k, y + r, x, y + r);
    c.cubic_to(x - k, y + r, x - r, y + k, x - r, y);
    c.cubic_to(x - r, y - k, x - k, y - r, x, y - r);
    c.cubic_to(x + k, y - r, x + r, y - k, x + r, y);
    c.close_path();
}

/// Where a tile of its own is kept, `<root>/<cycle>/<style>/<z>/<x>/<y>.png`, until its
/// column is packed (see `pack_path`).
pub fn tile_path(root: &Path, cycle: &str, style: &str, z: u8, x: u32, y: u32) -> PathBuf {
    root.join(cycle).join(style).join(z.to_string()).join(x.to_string()).join(format!("{y}.png"))
}

/// What a cycle's folder of tiles holds: its `meta.json`.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Meta {
    pub cycle: String,
    /// `simulator` or `database`: what the airways, fixes and navaids were read from.
    pub source: String,
    /// How deep each style has been drawn, all of it: a tile to that zoom that is not kept,
    /// and any under it, is plain.
    pub max_zoom: BTreeMap<String, u8>,
}

impl Meta {
    pub fn read(dir: &Path) -> Option<Meta> {
        serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).ok()?).ok()
    }

    fn write(&self, dir: &Path) -> Result<()> {
        std::fs::write(dir.join("meta.json"), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn source(&self) -> Source {
        if self.source == Source::Simulator.name() {
            Source::Simulator
        } else {
            Source::Database
        }
    }
}

/// Remove a tile kept from an earlier run that is now plain, and those under it to
/// `deepest`. How many files went.
fn remove_plain(root: &Path, cycle: &str, style: &str, z: u8, x: u32, y: u32, deepest: u8) -> usize {
    let mut n = 0;
    for dz in 0..=deepest.saturating_sub(z) {
        let (x0, y0, span) = (x << dz, y << dz, 1u32 << dz);
        for cx in x0..x0 + span {
            for cy in y0..y0 + span {
                if std::fs::remove_file(tile_path(root, cycle, style, z + dz, cx, cy)).is_ok() {
                    n += 1;
                }
            }
            // Its column's folder, once nothing is left in it.
            let _ = std::fs::remove_dir(root.join(cycle).join(style).join((z + dz).to_string()).join(cx.to_string()));
        }
    }
    n
}

/// The first bytes of a column's pack.
const PACK_MAGIC: &[u8; 8] = b"AMDBTPK1";

/// Where a column of tiles is packed: `<root>/<cycle>/<style>/<z>/<x>.pack`. A pack is
/// `PACK_MAGIC`, the number of tiles (u32), then for each, by y, its y, offset from the
/// start of the file and length (u32 each, little-endian), then the PNGs. One file a column
/// where there were up to a thousand, and none of the space a small file wastes: two and a
/// half million tiles took half as much again on disk as they held.
pub fn pack_path(root: &Path, cycle: &str, style: &str, z: u8, x: u32) -> PathBuf {
    root.join(cycle).join(style).join(z.to_string()).join(format!("{x}.pack"))
}

/// A pack's index, (y, offset, length) by y, and the open file.
fn pack_index(path: &Path) -> Option<(std::fs::File, Vec<(u32, u32, u32)>)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if &head[..8] != PACK_MAGIC {
        return None;
    }
    let n = u32::from_le_bytes(head[8..12].try_into().ok()?) as usize;
    let mut raw = vec![0u8; n * 12];
    f.read_exact(&mut raw).ok()?;
    let word = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    Some((f, raw.chunks_exact(12).map(|e| (word(&e[0..4]), word(&e[4..8]), word(&e[8..12]))).collect()))
}

/// A kept tile: a file of its own (drawn since its column was packed), or out of its pack.
pub fn read_tile(root: &Path, cycle: &str, style: &str, z: u8, x: u32, y: u32) -> Option<Vec<u8>> {
    use std::io::{Read, Seek};
    if let Ok(png) = std::fs::read(tile_path(root, cycle, style, z, x, y)) {
        return Some(png);
    }
    let (mut f, index) = pack_index(&pack_path(root, cycle, style, z, x))?;
    let (_, at, len) = index[index.binary_search_by_key(&y, |e| e.0).ok()?];
    f.seek(std::io::SeekFrom::Start(at as u64)).ok()?;
    let mut png = vec![0u8; len as usize];
    f.read_exact(&mut png).ok()?;
    Some(png)
}

/// Whether a tile is kept, on its own or in its column's pack.
pub fn has_tile(root: &Path, cycle: &str, style: &str, z: u8, x: u32, y: u32) -> bool {
    tile_path(root, cycle, style, z, x, y).is_file() || pack_index(&pack_path(root, cycle, style, z, x)).is_some_and(|(_, i)| i.binary_search_by_key(&y, |e| e.0).is_ok())
}

/// Which tiles each column's pack holds, read once a column: a run asks of every tile.
#[derive(Default)]
struct Packed(std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<Vec<u32>>>>);

impl Packed {
    fn has(&self, root: &Path, cycle: &str, style: &str, z: u8, x: u32, y: u32) -> bool {
        if tile_path(root, cycle, style, z, x, y).is_file() {
            return true;
        }
        let path = pack_path(root, cycle, style, z, x);
        let known = self.0.lock().unwrap().get(&path).cloned();
        let ys = known.unwrap_or_else(|| {
            let ys = std::sync::Arc::new(pack_index(&path).map(|(_, i)| i.into_iter().map(|e| e.0).collect::<Vec<u32>>()).unwrap_or_default());
            self.0.lock().unwrap().insert(path, ys.clone());
            ys
        });
        ys.binary_search(&y).is_ok()
    }
}

/// One tile as tightly as it is kept: four-channel PNGs drawn before as `compact_png`, and
/// when squeezing, a PNG as lossless WebP (`canvas::lossless_webp`). Whatever it becomes is
/// read back and must be the same to the pixel, or it stays as it was.
fn squeezed(png: Vec<u8>, squeeze: bool) -> Result<Vec<u8>> {
    let is_png = png.starts_with(b"\x89PNG");
    // Byte 25 of a PNG is its colour type: 6 is the four channels drawn before.
    let four = is_png && png.get(25) == Some(&6);
    if !(four || (squeeze && is_png)) {
        return Ok(png);
    }
    let px = tiny_skia::Pixmap::decode_png(&png)?;
    let small = if squeeze { crate::output::canvas::lossless_webp(px.data(), px.width(), px.height())? } else { crate::output::canvas::compact_png(px.data(), px.width(), px.height())? };
    let same = crate::output::canvas::decode_picture(&small).is_ok_and(|back| back.data() == px.data());
    Ok(if same && (four || small.len() < png.len()) { small } else { png })
}

/// Pack a column's tiles of their own (`<zdir>/<x>/<y>.png`) into its pack, with any it
/// already holds (see `squeezed`); the files go once the pack is written. (tiles, bytes
/// before, bytes after), or None when there was nothing to do: no files of its own, and
/// when squeezing, no PNG left in its pack.
fn pack_column(zdir: &Path, x: &str, squeeze: bool) -> Result<Option<(usize, u64, u64)>> {
    use std::io::{Read, Seek};
    let loose_dir = zdir.join(x);
    let files: Vec<PathBuf> = std::fs::read_dir(&loose_dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "png")).collect()).unwrap_or_default();
    let pack = zdir.join(format!("{x}.pack"));
    if files.is_empty() && !(squeeze && pack.is_file()) {
        let _ = std::fs::remove_dir(&loose_dir);
        return Ok(None);
    }
    let mut tiles: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    let mut before = 0u64;
    let mut changed = !files.is_empty();
    if let Some((mut f, index)) = pack_index(&pack) {
        before += f.metadata().map(|m| m.len()).unwrap_or(0);
        for (y, at, len) in index {
            let mut png = vec![0u8; len as usize];
            f.seek(std::io::SeekFrom::Start(at as u64))?;
            f.read_exact(&mut png)?;
            let out = squeezed(png.clone(), squeeze)?;
            changed |= out != png;
            tiles.insert(y, out);
        }
    }
    if !changed {
        return Ok(None);
    }
    for p in &files {
        let Some(y) = p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u32>().ok()) else { continue };
        let png = std::fs::read(p)?;
        before += png.len() as u64;
        tiles.insert(y, squeezed(png, squeeze)?);
    }
    let head = 12 + 12 * tiles.len();
    let mut out = Vec::with_capacity(head + tiles.values().map(Vec::len).sum::<usize>());
    out.extend_from_slice(PACK_MAGIC);
    out.extend_from_slice(&(tiles.len() as u32).to_le_bytes());
    let mut at = head as u32;
    for (y, png) in &tiles {
        out.extend_from_slice(&y.to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        at += png.len() as u32;
    }
    for png in tiles.values() {
        out.extend_from_slice(png);
    }
    // Written beside it and put in its place, so that a reader never sees half of one.
    let tmp = zdir.join(format!("{x}.pack.tmp"));
    std::fs::write(&tmp, &out)?;
    std::fs::rename(&tmp, &pack)?;
    for p in &files {
        let _ = std::fs::remove_file(p);
    }
    let _ = std::fs::remove_dir(&loose_dir);
    Ok(Some((tiles.len(), before, out.len() as u64)))
}

/// Pack every column under `root` that has tiles of its own (`pack_column`): a whole folder
/// of cycles, or one cycle's folder. `squeeze` packs each tile again as tightly as it goes.
fn pack_columns(root: &Path, pool: &rayon::ThreadPool, squeeze: bool) -> Result<()> {
    let subdirs = |d: &Path| -> Vec<PathBuf> { std::fs::read_dir(d).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default() };
    let is_zoom = |p: &Path| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.parse::<u8>().is_ok());
    // <style>/<z> folders, under a cycle or under each cycle.
    let mut zdirs = Vec::new();
    for a in subdirs(root) {
        for b in subdirs(&a) {
            if is_zoom(&b) {
                zdirs.push(b);
            } else {
                zdirs.extend(subdirs(&b).into_iter().filter(|c| is_zoom(c)));
            }
        }
    }
    let mut columns: Vec<(PathBuf, String)> = Vec::new();
    for z in &zdirs {
        let mut names: HashSet<String> = subdirs(z).into_iter().filter_map(|c| Some(c.file_name()?.to_str()?.to_string())).collect();
        if squeeze {
            let packs = std::fs::read_dir(z).map(|rd| rd.flatten().filter_map(|e| e.file_name().to_str()?.strip_suffix(".pack").map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
            names.extend(packs);
        }
        columns.extend(names.into_iter().map(|n| (z.clone(), n)));
    }
    if columns.is_empty() {
        return Ok(());
    }
    crate::term::start(&format!("Packing {} columns of tiles in {}, a file a column", columns.len(), root.display()));
    let (tiles, before, after, failed, done) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let t0 = std::time::Instant::now();
    pool.install(|| {
        columns.par_iter().for_each(|(zdir, x)| {
            match pack_column(zdir, x, squeeze) {
                Ok(Some((n, b, a))) => {
                    tiles.fetch_add(n, Ordering::Relaxed);
                    before.fetch_add(b as usize, Ordering::Relaxed);
                    after.fetch_add(a as usize, Ordering::Relaxed);
                }
                Ok(None) => {}
                Err(e) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    crate::term::warn(&format!("{}: {e:#}", zdir.join(x).display()));
                }
            }
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 1000 == 0 {
                let secs = t0.elapsed().as_secs_f64();
                crate::term::info(&format!("{n}/{} columns packed, about {} left", columns.len(), crate::term::human_secs(secs / n as f64 * (columns.len() - n) as f64)));
            }
        });
    });
    let (b, a) = (before.load(Ordering::Relaxed), after.load(Ordering::Relaxed));
    crate::term::success(&format!(
        "{} tiles in {} packs in {}: {} to {}{}",
        tiles.load(Ordering::Relaxed),
        columns.len() - failed.load(Ordering::Relaxed),
        crate::term::human_secs(t0.elapsed().as_secs_f64()),
        crate::term::human_bytes(b as u64),
        crate::term::human_bytes(a as u64),
        match failed.load(Ordering::Relaxed) {
            0 => String::new(),
            f => format!("; {f} columns left as they were"),
        }
    ));
    Ok(())
}

/// Pack every tile kept under `root`, each cycle, style and zoom: a pack a column, each tile
/// squeezed into lossless WebP, about half its PNG's size, not a pixel changed. A column
/// already squeezed is left alone, so this can be stopped and run again.
pub fn repack(root: &Path, jobs: usize, background: bool) -> Result<()> {
    if background {
        super::faa_charts::enter_background();
    }
    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs.max(1)).build()?;
    pack_columns(root, &pool, true)
}

pub struct Options {
    /// Where the tiles go, a folder a cycle.
    pub out: PathBuf,
    /// The navigation database to draw from, `sim` for the simulator's data; the newest
    /// database on the machine when None.
    pub from: Option<PathBuf>,
    pub styles: Vec<String>,
    /// Every tile from zoom 0 to this.
    pub max_zoom: u8,
    pub jobs: usize,
    pub background: bool,
    /// Leave tiles already drawn as they are.
    pub resume: bool,
}

/// Draw each style down to a zoom, ahead of any flight bag asking: a zoom at a time, the
/// tiles under a plain one never looked at.
pub fn run(opts: &Options) -> Result<()> {
    if opts.background {
        super::faa_charts::enter_background();
    }
    let source = Source::from_arg(opts.from.as_deref())?;
    let http = crate::sources::http::Http::new(120, 0);
    let cache = crate::cache::Cache::for_index(false);
    let data = Data::load(&http, &cache, source)?;
    let styles: Vec<String> = if opts.styles.is_empty() { STYLES.iter().map(|s| s.to_string()).collect() } else { opts.styles.clone() };
    if let Some(s) = styles.iter().find(|s| !STYLES.contains(&s.as_str())) {
        return Err(anyhow!("{s} is not an enroute style"));
    }
    if opts.max_zoom > DEEPEST {
        return Err(anyhow!("tiles go to zoom {DEEPEST} at most"));
    }
    let dir = opts.out.join(&data.cycle);
    std::fs::create_dir_all(&dir)?;
    // What they are drawn from goes in first, for the bridge to draw what it is asked for
    // and is not here yet out of the same.
    let mut meta = Meta::read(&dir).unwrap_or_default();
    meta.cycle = data.cycle.clone();
    meta.source = source.name().to_string();
    meta.write(&dir)?;
    crate::term::start(&format!("Enroute tiles, AIRAC {}: {} styles, zoom 0 to {}, into {}", data.cycle, styles.len(), opts.max_zoom, dir.display()));

    let (drawn, kept, plain_n, removed, bytes) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let packed = Packed::default();
    let t0 = std::time::Instant::now();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(opts.jobs.max(1)).build()?;
    let mut level: Vec<(usize, u32, u32)> = (0..styles.len()).map(|s| (s, 0, 0)).collect();
    for z in 0..=opts.max_zoom {
        let (t, todo, done) = (std::time::Instant::now(), level.len(), AtomicUsize::new(0));
        let before = (drawn.load(Ordering::Relaxed), plain_n.load(Ordering::Relaxed));
        let next: Vec<(usize, u32, u32)> = pool.install(|| {
            level
                .par_iter()
                .flat_map_iter(|&(s, x, y)| {
                    let style = styles[s].as_str();
                    let path = tile_path(&opts.out, &data.cycle, style, z, x, y);
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % 5000 == 0 {
                        let secs = t.elapsed().as_secs_f64();
                        crate::term::info(&format!("zoom {z}: {n}/{todo}, about {} left at this zoom", crate::term::human_secs(secs / n as f64 * (todo - n) as f64)));
                    }
                    if plain(&data, style, z, x, y).is_some() {
                        plain_n.fetch_add(1, Ordering::Relaxed);
                        if path.is_file() {
                            removed.fetch_add(remove_plain(&opts.out, &data.cycle, style, z, x, y, opts.max_zoom), Ordering::Relaxed);
                        }
                        return Vec::new();
                    }
                    if opts.resume && packed.has(&opts.out, &data.cycle, style, z, x, y) {
                        kept.fetch_add(1, Ordering::Relaxed);
                    } else {
                        match tile(&data, style, z, x, y) {
                            Ok(png) => {
                                if let Some(dir) = path.parent() {
                                    let _ = std::fs::create_dir_all(dir);
                                }
                                bytes.fetch_add(png.len(), Ordering::Relaxed);
                                drawn.fetch_add(1, Ordering::Relaxed);
                                let _ = std::fs::write(&path, png);
                            }
                            Err(e) => crate::term::warn(&format!("{style} {z}/{x}/{y}: {e:#}")),
                        }
                    }
                    if z < opts.max_zoom {
                        vec![(s, 2 * x, 2 * y), (s, 2 * x + 1, 2 * y), (s, 2 * x, 2 * y + 1), (s, 2 * x + 1, 2 * y + 1)]
                    } else {
                        Vec::new()
                    }
                })
                .collect()
        });
        crate::term::info(&format!(
            "zoom {z}: {todo} looked at, {} drawn, {} plain (nothing under them looked at) in {}",
            drawn.load(Ordering::Relaxed) - before.0,
            plain_n.load(Ordering::Relaxed) - before.1,
            crate::term::human_secs(t.elapsed().as_secs_f64())
        ));
        level = next;
    }
    // What was drawn, a file a tile, into its columns' packs.
    pack_columns(&dir, &pool, false)?;
    for style in &styles {
        let deepest = meta.max_zoom.get(style).copied().unwrap_or(0).max(opts.max_zoom);
        meta.max_zoom.insert(style.clone(), deepest);
    }
    meta.write(&dir)?;
    let gone = removed.load(Ordering::Relaxed);
    crate::term::success(&format!(
        "Enroute tiles in {}: {} drawn ({}), {} already there, {} plain and not kept{}",
        crate::term::human_secs(t0.elapsed().as_secs_f64()),
        drawn.load(Ordering::Relaxed),
        crate::term::human_bytes(bytes.load(Ordering::Relaxed) as u64),
        kept.load(Ordering::Relaxed),
        plain_n.load(Ordering::Relaxed),
        if gone > 0 { format!("; {gone} plain tiles from an earlier run removed") } else { String::new() }
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_sit_where_the_web_map_puts_them() {
        let v = View::new(1, 0, 0);
        assert!((v.bounds.1 + 180.0).abs() < 1e-9 && v.bounds.3.abs() < 1e-9);
        assert!(v.bounds.0.abs() < 1e-9 && (v.bounds.2 - 85.0511).abs() < 1e-3);
        // The tile's own corners come out at its corners.
        let (x, y) = v.at(0.0, 0.0);
        assert!((x - TILE_PT).abs() < 1e-3 && y.abs() < 1e-3);
        let (x, y) = v.at(85.0511, -180.0);
        assert!(x.abs() < 1e-3 && (y - TILE_PT).abs() < 0.05);
    }

    #[test]
    fn longitudes_are_taken_on_the_tile_s_side_of_the_world() {
        let v = View::new(3, 7, 3); // 135E to 180E
        assert_eq!(v.wrap(-179.0), 360.0);
        assert_eq!(v.wrap(170.0), 0.0);
        let w = View::new(3, 0, 3); // 180W to 135W
        assert_eq!(w.wrap(179.0), -360.0);
    }

    #[test]
    fn a_ring_does_not_jump_the_antimeridian() {
        let rows: Vec<(String, Option<(f64, f64)>, Option<(f64, f64)>, Option<f64>)> =
            [(10.0, 179.0), (10.0, -179.0), (11.0, -179.0), (11.0, 179.0)].iter().map(|&p| ("G".to_string(), Some(p), None, None)).collect();
        let r = ring(&rows);
        assert_eq!(r[1].1, 181.0);
        assert!(r.windows(2).all(|w| (w[1].1 - w[0].1).abs() <= 180.0));
    }

    #[test]
    fn labels_do_not_overlap() {
        let mut l = Labels::default();
        assert!(l.take(0.0, 0.0, 10.0, 5.0));
        assert!(!l.take(5.0, 2.0, 10.0, 5.0));
        assert!(l.take(11.0, 0.0, 10.0, 5.0));
    }

    #[test]
    fn a_line_crosses_a_tile_only_where_it_passes_over_it() {
        assert!(crosses((10.0, 10.0), (20.0, 20.0)));
        assert!(crosses((-100.0, 128.0), (400.0, 128.0)));
        // Past a corner, inside the square both lines' boxes make, but not over the tile.
        assert!(!crosses((-100.0, 200.0), (200.0, 500.0)));
        assert!(!crosses((-50.0, -50.0), (-50.0, 500.0)));
    }

    /// An island four degrees square at 10N 10E, an airway along 20N, nothing else.
    fn island() -> Data {
        let square = vec![(10.0, 10.0), (14.0, 10.0), (14.0, 14.0), (10.0, 14.0), (10.0, 10.0)];
        let ground = Ground { shapes: vec![Shape::new(false, vec![square])] };
        let segments = vec![Segment { ident: "UL1".into(), level: 'H', a: (20.0, 0.0), b: (20.0, 30.0) }];
        let mut segment_cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for lo in 0..=30 {
            segment_cells.entry((20, lo)).or_default().push(0);
        }
        Data { cycle: "0000".into(), segments, segment_cells, points: Vec::new(), point_cells: HashMap::new(), areas: Vec::new(), mora: HashMap::new(), ground }
    }

    /// The tile at a zoom with a place in it.
    fn tile_of(z: u8, lat: f64, lon: f64) -> (u32, u32) {
        let n = (1u64 << z) as f64;
        let x = ((lon + 180.0) / 360.0 * n) as u32;
        let r = lat.to_radians();
        let y = ((1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * n) as u32;
        (x, y)
    }

    #[test]
    fn a_tile_with_nothing_on_it_is_plain_and_so_is_everything_under_it() {
        let d = island();
        // Open sea, the middle of the island, its shore, and the airway in the high style
        // but not in the world style, which has no airways.
        let (x, y) = tile_of(8, -30.0, -30.0);
        assert_eq!(plain(&d, "ifr.hi", 8, x, y), Some(Plain::Sea));
        let (x, y) = tile_of(8, 12.0, 12.0);
        assert_eq!(plain(&d, "ifr.hi", 8, x, y), Some(Plain::Land));
        let (x, y) = tile_of(8, 10.0, 12.0);
        assert_eq!(plain(&d, "ifr.hi", 8, x, y), None);
        let (x, y) = tile_of(6, 20.0, 15.0);
        assert_eq!(plain(&d, "ifr.hi", 6, x, y), None);
        assert_eq!(plain(&d, "world", 6, x, y), Some(Plain::Sea));
        // Each plain tile's quarters, and theirs, are plain alike.
        for z in 3..6u8 {
            let n = 1u32 << z;
            for x in 0..n {
                for y in 0..n {
                    let Some(p) = plain(&d, "ifr.hi", z, x, y) else { continue };
                    for (cx, cy) in [(2 * x, 2 * y), (2 * x + 1, 2 * y), (2 * x, 2 * y + 1), (2 * x + 1, 2 * y + 1)] {
                        assert_eq!(plain(&d, "ifr.hi", z + 1, cx, cy), Some(p), "{z}/{x}/{y} -> {}/{cx}/{cy}", z + 1);
                    }
                }
            }
        }
    }

    /// Real tiles packed again with `compact_png`: how much smaller and how long, and every
    /// pixel of every tile kept.
    /// The tile of the most colours is written out as it was and as packed, for a look.
    /// Run by hand: `cargo test --release -- --ignored compact_tiles`.
    #[test]
    #[ignore]
    fn compact_tiles_keep_every_pixel() {
        let root = Path::new("E:/OANS Cache/airports/enroute/2610");
        let (mut before, mut after, mut n, mut secs) = (0usize, 0usize, 0usize, 0f64);
        let (mut reduced, mut moved, mut worst, mut most) = (0usize, 0usize, 0i32, (0usize, Vec::new(), Vec::new()));
        for style in STYLES {
            for z in [6u8, 8, 10] {
                let Ok(cols) = std::fs::read_dir(root.join(style).join(z.to_string())) else { continue };
                for col in cols.flatten().filter(|c| c.path().is_dir()).step_by(7).take(6) {
                    for f in std::fs::read_dir(col.path()).unwrap().flatten().step_by(5).take(6) {
                        let png = std::fs::read(f.path()).unwrap();
                        let px = tiny_skia::Pixmap::decode_png(&png).unwrap();
                        let colours: HashSet<[u8; 3]> = px.data().chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
                        let t = std::time::Instant::now();
                        let small = crate::output::canvas::compact_png(px.data(), px.width(), px.height()).unwrap();
                        secs += t.elapsed().as_secs_f64();
                        let back = tiny_skia::Pixmap::decode_png(&small).unwrap();
                        assert_eq!(back.data(), px.data(), "{}", f.path().display());
                        if colours.len() > 256 {
                            reduced += 1;
                            for (a, b) in px.data().chunks_exact(4).zip(back.data().chunks_exact(4)) {
                                let d = (0..3).map(|k| (a[k] as i32 - b[k] as i32).abs()).max().unwrap_or(0);
                                moved += (d > 0) as usize;
                                worst = worst.max(d);
                            }
                            if colours.len() > most.0 {
                                most = (colours.len(), png.clone(), small.clone());
                            }
                        }
                        (before, after, n) = (before + png.len(), after + small.len(), n + 1);
                    }
                }
            }
        }
        println!("{n} tiles: {:.1} KB to {:.1} KB each ({:.0}%), {:.1} ms each", before as f64 / n as f64 / 1024.0, after as f64 / n as f64 / 1024.0, after as f64 / before as f64 * 100.0, secs / n as f64 * 1000.0);
        println!("{reduced} had more than 256 colours: {:.2}% of their pixels moved, at most by {worst} of 255", moved as f64 / (reduced.max(1) * 512 * 512) as f64 * 100.0);
        let out = std::env::temp_dir();
        std::fs::write(out.join("amdb-tile-before.png"), &most.1).unwrap();
        std::fs::write(out.join("amdb-tile-after.png"), &most.2).unwrap();
        println!("densest ({} colours) written to {}", most.0, out.join("amdb-tile-*.png").display());
    }

    /// A column of tiles packed reads back tile for tile, its files gone; a tile drawn
    /// since joins the pack the next time, and one not drawn is not there.
    #[test]
    fn tiles_read_the_same_packed() {
        let root = std::env::temp_dir().join(format!("amdb-tile-packs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sea = plain_tile("ifr.lo", Plain::Sea).unwrap();
        let land = plain_tile("ifr.lo", Plain::Land).unwrap();
        for (y, png) in [(3u32, &sea), (9, &land)] {
            let p = tile_path(&root, "2610", "ifr.lo", 5, 7, y);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, png).unwrap();
        }
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        pack_columns(&root, &pool, true).unwrap();
        assert!(pack_path(&root, "2610", "ifr.lo", 5, 7).is_file());
        assert!(!tile_path(&root, "2610", "ifr.lo", 5, 7, 3).exists());
        // Kept as lossless WebP now: the same pixels, and a PNG of them for a flight bag.
        let pixels = |png: &[u8]| crate::output::canvas::decode_picture(png).unwrap().data().to_vec();
        let kept = |y: u32| read_tile(&root, "2610", "ifr.lo", 5, 7, y).unwrap();
        assert!(kept(3).starts_with(b"RIFF"));
        assert_eq!(pixels(&kept(3)), pixels(&sea));
        assert_eq!(pixels(&kept(9)), pixels(&land));
        assert!(crate::output::canvas::as_png(&kept(3)).unwrap().starts_with(b"\x89PNG"));
        assert_eq!(pixels(&crate::output::canvas::as_png(&kept(3)).unwrap()), pixels(&sea));
        assert!(has_tile(&root, "2610", "ifr.lo", 5, 7, 9) && !has_tile(&root, "2610", "ifr.lo", 5, 7, 4));
        assert_eq!(read_tile(&root, "2610", "ifr.lo", 5, 7, 4), None);

        let p = tile_path(&root, "2610", "ifr.lo", 5, 7, 4);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, &land).unwrap();
        assert!(Packed::default().has(&root, "2610", "ifr.lo", 5, 7, 4));
        pack_columns(&root, &pool, true).unwrap();
        assert!(!p.exists());
        for (y, png) in [(3u32, &sea), (4, &land), (9, &land)] {
            assert_eq!(pixels(&kept(y)), pixels(png), "tile {y}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_plain_tile_is_drawn_the_same_as_its_picture() {
        let d = island();
        for (lat, lon, style) in [(11.0, 11.0, "ifr.lo"), (-30.0, -30.0, "vfr")] {
            let (x, y) = tile_of(9, lat, lon);
            let p = plain(&d, style, 9, x, y).expect("plain");
            assert_eq!(tile(&d, style, 9, x, y).unwrap(), plain_tile(style, p).unwrap());
            assert_eq!(d.ground.plain_tile_at(9, x, y), p);
        }
    }
}
