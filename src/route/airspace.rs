//! The airspace a flight has to know about beyond the airways: conflict zones, areas a
//! NOTAM makes active, and the flight information regions a route passes through.

use crate::dispatch::{along, sky, travel, Bounds, FiledRoute, Hazard, HazardKind, LatLon};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------------
// Conflict zones: kept by hand in data/conflict_zones.json, compiled into the binary.
// ---------------------------------------------------------------------------------

const RAW_ZONES: &str = include_str!("../../data/conflict_zones.json");

#[derive(Debug, Deserialize)]
struct ZonesFile {
    #[allow(dead_code)]
    reviewed: String,
    zones: Vec<ZoneEntry>,
}

#[derive(Debug, Deserialize)]
struct ZoneEntry {
    reference: String,
    #[allow(dead_code)]
    authority: String,
    name: String,
    #[serde(default)]
    firs: Vec<String>,
    #[serde(default)]
    polygon: Option<Vec<[f64; 2]>>,
    #[serde(default)]
    floor_fl: f64,
    #[serde(default = "unlimited_fl")]
    ceiling_fl: f64,
    /// "prohibition" or "advisory".
    severity: String,
    /// An advisory may be flown through at a price instead of avoided outright; a
    /// prohibition is always `Avoid` regardless of what this says.
    #[serde(default)]
    penalise: Option<f64>,
    #[serde(default)]
    valid_from: Option<String>,
    #[serde(default)]
    valid_to: Option<String>,
}

fn unlimited_fl() -> f64 {
    999.0
}

impl ZoneEntry {
    fn kind(&self) -> HazardKind {
        if self.severity == "prohibition" {
            HazardKind::Avoid
        } else {
            self.penalise.map(HazardKind::Penalise).unwrap_or(HazardKind::Avoid)
        }
    }

    fn window(&self) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
        (self.valid_from.as_deref().and_then(parse_rfc3339), self.valid_to.as_deref().and_then(parse_rfc3339))
    }

    fn active_at(&self, when: DateTime<Utc>) -> bool {
        let (from, to) = self.window();
        from.is_none_or(|f| f <= when) && to.is_none_or(|t| when <= t)
    }
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

fn zones_data() -> &'static ZonesFile {
    static DATA: OnceLock<ZonesFile> = OnceLock::new();
    DATA.get_or_init(|| serde_json::from_str(RAW_ZONES).expect("data/conflict_zones.json is well formed"))
}

/// Every conflict zone in force at a time, as hazards: a prohibition avoided outright, an
/// advisory avoided unless the file says otherwise, `source` carrying the bulletin's own
/// reference so a flight plan can print it. Where the navigation database has no FIR of the
/// name the entry gives, the entry's own polygon is used instead where it has one; where
/// there is neither, the entry is skipped and a warning is logged, rather than silently
/// planning through airspace a bulletin says to keep out of.
pub fn conflict_zones(when: DateTime<Utc>) -> Vec<Hazard> {
    hazards_from(&zones_data().zones, when)
}

fn hazards_from(zones: &[ZoneEntry], when: DateTime<Utc>) -> Vec<Hazard> {
    let mut out = Vec::new();
    for z in zones {
        if !z.active_at(when) {
            continue;
        }
        let (from, to) = z.window();
        let kind = z.kind();
        let base_ft = z.floor_fl * 100.0;
        let top_ft = if z.ceiling_fl >= 900.0 { sky() } else { z.ceiling_fl * 100.0 };

        let regions = if z.firs.is_empty() { Vec::new() } else { crate::sources::navdata::regions(&z.firs) };
        let found: HashSet<String> = regions.iter().map(|r| r.ident.to_uppercase()).collect();
        let all_found = !z.firs.is_empty() && z.firs.iter().all(|f| found.contains(&f.to_uppercase()));

        if all_found {
            for region in &regions {
                for part in &region.parts {
                    out.push(Hazard {
                        name: format!("{} ({})", z.name, region.ident),
                        polygon: part.clone(),
                        base_ft,
                        top_ft,
                        kind: kind.clone(),
                        active_from: from,
                        active_to: to,
                        source: z.reference.clone(),
                    });
                }
            }
        } else if let Some(poly) = &z.polygon {
            out.push(Hazard {
                name: z.name.clone(),
                polygon: poly.iter().map(|p| (p[0], p[1])).collect(),
                base_ft,
                top_ft,
                kind,
                active_from: from,
                active_to: to,
                source: z.reference.clone(),
            });
        } else {
            log::warn!("conflict zone {}: no FIR of that name in the navigation database and no polygon given, skipped", z.reference);
        }
    }
    out
}

// ---------------------------------------------------------------------------------
// NOTAMs.
// ---------------------------------------------------------------------------------

/// A NOTAM read down to what a route search needs: where it is, the band it fills, when it
/// runs, and what it says to do about it.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedNotam {
    pub id: String,
    pub fir: String,
    /// The five-letter Q-code, "QRTCA", "QWMLW".
    pub q_code: String,
    pub lower_ft: f64,
    pub upper_ft: f64,
    pub centre: LatLon,
    pub radius_nm: f64,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_to: Option<DateTime<Utc>>,
    /// Item D, the schedule, kept as printed ("DAILY 0600-1200") rather than parsed into a
    /// recurrence: `Hazard` carries one active window, not a calendar, so a NOTAM that only
    /// bites for part of each day is treated as active for the whole of B to C, the
    /// conservative reading. A flight plan that wants the finer detail can still print this.
    pub schedule: Option<String>,
    pub text: String,
}

/// Every item (`Q)`, `A)`, `B)`, ...) as the space-joined tokens that follow its marker, up
/// to the next one.
fn items(text: &str) -> HashMap<char, String> {
    let mut out: HashMap<char, String> = HashMap::new();
    let mut current: Option<char> = None;
    for tok in text.split_whitespace() {
        let mut chars = tok.chars();
        if tok.len() == 2 && chars.next().is_some_and(|c| c.is_ascii_uppercase()) && chars.next() == Some(')') {
            current = Some(tok.chars().next().unwrap());
            out.entry(current.unwrap()).or_default();
            continue;
        }
        if let Some(c) = current {
            let entry = out.entry(c).or_default();
            if !entry.is_empty() {
                entry.push(' ');
            }
            entry.push_str(tok);
        }
    }
    out
}

/// "DDMM[N/S]DDDMM[E/W]RRR": the ICAO Q-line's own coordinate and radius, fourteen
/// characters and no separators.
fn parse_coord_radius(s: &str) -> Option<(LatLon, f64)> {
    if s.len() != 14 || !s.is_char_boundary(14) {
        return None;
    }
    let bytes = s.as_bytes();
    let lat_deg: f64 = s.get(0..2)?.parse().ok()?;
    let lat_min: f64 = s.get(2..4)?.parse().ok()?;
    let lat_h = *bytes.get(4)?;
    let lon_deg: f64 = s.get(5..8)?.parse().ok()?;
    let lon_min: f64 = s.get(8..10)?.parse().ok()?;
    let lon_h = *bytes.get(10)?;
    let radius: f64 = s.get(11..14)?.parse().ok()?;
    if !matches!(lat_h, b'N' | b'S') || !matches!(lon_h, b'E' | b'W') {
        return None;
    }
    let lat = (lat_deg + lat_min / 60.0) * if lat_h == b'S' { -1.0 } else { 1.0 };
    let lon = (lon_deg + lon_min / 60.0) * if lon_h == b'W' { -1.0 } else { 1.0 };
    Some(((lat, lon), radius))
}

/// "YYMMDDHHmm", the form B) and C) are written in; `None` for anything else, including
/// "PERM", which is exactly the right reading for an end that never comes.
fn parse_notam_time(s: &str) -> Option<DateTime<Utc>> {
    if s.len() != 10 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year = 2000 + s[0..2].parse::<i32>().ok()?;
    let month: u32 = s[2..4].parse().ok()?;
    let day: u32 = s[4..6].parse().ok()?;
    let hour: u32 = s[6..8].parse().ok()?;
    let min: u32 = s[8..10].parse().ok()?;
    chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, min, 0).map(|d| d.and_utc())
}

/// A raw NOTAM, the ICAO Q-line and the B/C/D/E items, read into its parts.
pub fn parse_notam_text(text: &str) -> Option<ParsedNotam> {
    let id = text.split_whitespace().next()?.to_string();
    let it = items(text);
    let q = it.get(&'Q')?;
    let parts: Vec<&str> = q.split('/').collect();
    if parts.len() < 8 {
        return None;
    }
    let (centre, radius_nm) = parse_coord_radius(parts[7])?;
    Some(ParsedNotam {
        id,
        fir: parts[0].to_string(),
        q_code: parts[1].to_string(),
        lower_ft: parts[5].parse::<f64>().unwrap_or(0.0) * 100.0,
        upper_ft: parts[6].parse::<f64>().unwrap_or(999.0) * 100.0,
        centre,
        radius_nm,
        effective_from: it.get(&'B').and_then(|s| parse_notam_time(s)),
        effective_to: it.get(&'C').and_then(|s| parse_notam_time(s)),
        schedule: it.get(&'D').cloned(),
        text: it.get(&'E').cloned().unwrap_or_default(),
    })
}

/// What the Q-code says to do: `QRD*` a danger area, priced rather than avoided; the rest
/// of `QR**` a restriction, active or temporary, avoided; `QW*` a warning or a military
/// exercise, priced. Every other subject (lighting, communications, obstacles and the
/// rest) is not an airspace hazard at all, and is not turned into one.
fn notam_kind(q_code: &str) -> Option<HazardKind> {
    let body = q_code.strip_prefix('Q')?;
    if body.starts_with("RD") {
        Some(HazardKind::Penalise(1.15))
    } else if body.starts_with('R') {
        Some(HazardKind::Avoid)
    } else if body.starts_with('W') {
        Some(HazardKind::Penalise(1.15))
    } else {
        None
    }
}

/// A circle, as a polygon a fixed number of points round it: `Hazard` only draws polygons.
fn circle_polygon(centre: LatLon, radius_nm: f64, sides: usize) -> Vec<LatLon> {
    (0..sides).map(|i| travel(centre, i as f64 * 360.0 / sides as f64, radius_nm)).collect()
}

fn to_hazard(n: &ParsedNotam) -> Option<Hazard> {
    let kind = notam_kind(&n.q_code)?;
    let mut name = format!("NOTAM {} ({})", n.id, n.fir);
    if let Some(sched) = &n.schedule {
        name.push_str(&format!(" [{sched}]"));
    }
    Some(Hazard {
        name,
        polygon: circle_polygon(n.centre, n.radius_nm.max(0.5), 24),
        base_ft: n.lower_ft,
        top_ft: if n.upper_ft <= 0.0 { sky() } else { n.upper_ft },
        kind,
        active_from: n.effective_from,
        active_to: n.effective_to,
        source: n.id.clone(),
    })
}

/// Credentials for the FAA's NOTAM Search API (`https://api.faa.gov`, free to register
/// for): a `client_id` and `client_secret` from `FAA_NOTAM_CLIENT_ID` /
/// `FAA_NOTAM_CLIENT_SECRET`, or both at once from `FAA_NOTAM_KEY` as `"id:secret"`.
fn notam_credentials() -> Option<(String, String)> {
    if let (Ok(id), Ok(secret)) = (std::env::var("FAA_NOTAM_CLIENT_ID"), std::env::var("FAA_NOTAM_CLIENT_SECRET")) {
        return Some((id, secret));
    }
    let combined = std::env::var("FAA_NOTAM_KEY").ok()?;
    let (id, secret) = combined.split_once(':')?;
    Some((id.to_string(), secret.to_string()))
}

/// `crate::sources::http::Http` sends one fixed set of headers and has nowhere to put the
/// `client_id`/`client_secret` this API is gated behind, so this talks to `ureq` directly —
/// the same crate `Http` itself uses — rather than widen that shared type for one caller.
/// See this module's author's report for the change to `Http` that would let this go
/// through it instead.
fn get_with_auth(url: &str, client_id: &str, client_secret: &str) -> anyhow::Result<String> {
    let cfg = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(20))).http_status_as_error(false).build();
    let agent = cfg.new_agent();
    let mut resp = agent
        .get(url)
        .header("client_id", client_id)
        .header("client_secret", client_secret)
        .header("User-Agent", concat!("amdbgen/", env!("CARGO_PKG_VERSION")))
        .call()?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string()?;
    if status >= 400 {
        anyhow::bail!("HTTP {status}: {}", text.chars().take(200).collect::<String>());
    }
    Ok(text)
}

/// Circles no more than a hundred nautical miles across — the API's own limit — laid out
/// closely enough that neighbouring ones overlap, covering a box with none of it missed.
fn tiles(b: Bounds) -> Vec<(LatLon, f64)> {
    const RADIUS_NM: f64 = 100.0;
    const STEP_NM: f64 = 150.0; // Less than 2 x radius: circles overlap, nothing falls between them.
    let mut out = Vec::new();
    let mut lat = b.south;
    loop {
        let cos = lat.to_radians().cos().max(0.05);
        let step_lon = STEP_NM / 60.0 / cos;
        let east = if b.west <= b.east { b.east } else { b.east + 360.0 };
        // Walked so that a tile always lands exactly on the east edge (and, below, the
        // north edge) rather than only wherever a fixed step happens to fall: a box
        // narrower than one step must still get a tile that reaches its far corner.
        let mut lon = b.west;
        loop {
            out.push(((lat, ((lon + 540.0) % 360.0) - 180.0), RADIUS_NM));
            if lon >= east {
                break;
            }
            lon = (lon + step_lon).min(east);
        }
        if lat >= b.north {
            break;
        }
        lat = (lat + STEP_NM / 60.0).min(b.north);
    }
    out
}

fn notam_url(centre: LatLon, radius_nm: f64, from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    format!(
        "https://external-api.faa.gov/notamapi/v1/notams?locationLongitude={:.4}&locationLatitude={:.4}&locationRadius={:.0}&effectiveStartDate={}&effectiveEndDate={}&pageSize=1000&responseFormat=geoJson",
        centre.1, centre.0, radius_nm.min(100.0), from.to_rfc3339(), to.to_rfc3339()
    )
}

/// The raw ICAO text of every NOTAM in an API response. The field this reads
/// (`items[].properties.coreNOTAMData.notam.text`) is the FAA's documented shape at the
/// time this was written; if it has since moved, the fallback walks the whole response for
/// anything that reads like a NOTAM (it carries both a `Q)` and an `E)` item) rather than
/// return nothing.
fn raw_texts(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let mut out = Vec::new();
    for item in v.get("items").and_then(|i| i.as_array()).into_iter().flatten() {
        if let Some(t) = item.pointer("/properties/coreNOTAMData/notam/text").and_then(|t| t.as_str()) {
            out.push(t.to_string());
        } else if let Some(t) = find_notam_text(item) {
            out.push(t);
        }
    }
    out
}

fn find_notam_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) if s.contains("Q)") && s.contains("E)") => Some(s.clone()),
        serde_json::Value::Object(map) => map.values().find_map(find_notam_text),
        serde_json::Value::Array(items) => items.iter().find_map(find_notam_text),
        _ => None,
    }
}

/// The areas NOTAMs make active over an area between two times, from the FAA's NOTAM
/// Search API. Needs a free `api.faa.gov` registration; see `notam_credentials`.
pub fn notam_hazards(bounds: Bounds, from: DateTime<Utc>, to: DateTime<Utc>) -> anyhow::Result<Vec<Hazard>> {
    let Some((client_id, client_secret)) = notam_credentials() else {
        anyhow::bail!("no FAA NOTAM API credentials: set FAA_NOTAM_CLIENT_ID and FAA_NOTAM_CLIENT_SECRET (or FAA_NOTAM_KEY as \"id:secret\"); register free at https://api.faa.gov");
    };
    let cache = crate::cache::Cache::for_index(false);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (centre, radius) in tiles(bounds) {
        let key = format!("notam/{:.2}_{:.2}_{}_{}.json", centre.0, centre.1, from.timestamp(), to.timestamp());
        let url = notam_url(centre, radius, from, to);
        let text = match cache.get_or_fetch_text(&key, || get_with_auth(&url, &client_id, &client_secret)) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("NOTAM tile at {centre:?}: {e:#}");
                continue;
            }
        };
        for raw in raw_texts(&text) {
            let Some(parsed) = parse_notam_text(&raw) else { continue };
            if parsed.effective_to.is_some_and(|t| t < from) || parsed.effective_from.is_some_and(|f| f > to) {
                continue;
            }
            if !seen.insert(parsed.id.clone()) {
                continue;
            }
            if let Some(h) = to_hazard(&parsed) {
                out.push(h);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------
// Flight information regions along a route.
// ---------------------------------------------------------------------------------

/// A flight information region a route passes through.
#[derive(Debug, Clone, PartialEq)]
pub struct FirCrossing {
    pub ident: String,
    pub name: String,
    pub entry: LatLon,
    /// From the origin, nautical miles.
    pub entry_nm: f64,
    pub exit_nm: f64,
}

/// The longitude span of a set of points, found the way it has to be for points scattered
/// round a circle rather than a line: sort them, take the widest gap between neighbours —
/// wrapping from the last back to the first — and the box is everything that is not that
/// gap. `(west, east)`, in `Bounds`' own convention: `west > east` where the box crosses
/// the date line. A set that really does reach most of the way round (a polar or equatorial
/// region) still gets a wide box, because the widest gap is still the true one; what this
/// avoids is a region such as an oceanic control area whose points sit near +179 and -179
/// both, which read as a plain minimum and maximum would span the entire globe rather than
/// the sliver either side of the date line they actually are.
fn lon_span(mut lons: Vec<f64>) -> (f64, f64) {
    lons.sort_by(|a, b| a.partial_cmp(b).unwrap());
    lons.dedup();
    let (Some(&first), Some(&last)) = (lons.first(), lons.last()) else { return (0.0, 0.0) };
    let mut west = first;
    let mut east = last;
    let mut widest = first + 360.0 - last;
    for w in lons.windows(2) {
        let gap = w[1] - w[0];
        if gap > widest {
            widest = gap;
            west = w[1];
            east = w[0];
        }
    }
    (west, east)
}

/// A longitude range as one interval, or two either side of the date line where it wraps.
fn lon_intervals(west: f64, east: f64) -> [(f64, f64); 2] {
    if west <= east {
        [(west, east), (f64::NAN, f64::NAN)]
    } else {
        [(west, 180.0), (-180.0, east)]
    }
}

/// Whether two boxes share any ground, each allowed to wrap the date line.
fn bounds_overlap(a: Bounds, b: Bounds) -> bool {
    if a.north < b.south || b.north < a.south {
        return false;
    }
    let (ai, bi) = (lon_intervals(a.west, a.east), lon_intervals(b.west, b.east));
    ai.iter().any(|&(aw, ae)| !aw.is_nan() && bi.iter().any(|&(bw, be)| !bw.is_nan() && aw <= be && bw <= ae))
}

/// Every region's own box: the latitude range plain, the longitude range by `lon_span`
/// since a region can wrap the date line even where a route asking about it does not.
/// Computed once and kept — the table has tens of thousands of rows across a few hundred
/// regions, and every route this crate plans asks the same question of it.
fn region_bounds() -> &'static Vec<(String, Bounds)> {
    static BOUNDS: OnceLock<Vec<(String, Bounds)>> = OnceLock::new();
    BOUNDS.get_or_init(|| {
        let Some((conn, table)) = crate::sources::navdata::open_table("fir_uir") else { return Vec::new() };
        let sql = format!("select fir_uir_identifier, fir_uir_latitude, fir_uir_longitude from \"{table}\"");
        let Ok(mut statement) = conn.prepare(&sql) else { return Vec::new() };
        let Ok(rows) = statement.query_map([], |row| Ok((row.get::<_, Option<String>>(0)?.unwrap_or_default(), row.get::<_, Option<f64>>(1)?, row.get::<_, Option<f64>>(2)?))) else {
            return Vec::new();
        };
        let mut by_ident: HashMap<String, (Vec<f64>, Vec<f64>)> = HashMap::new();
        for (ident, lat, lon) in rows.flatten() {
            let (Some(lat), Some(lon)) = (lat, lon) else { continue };
            if ident.is_empty() {
                continue;
            }
            let entry = by_ident.entry(ident).or_default();
            entry.0.push(lat);
            entry.1.push(lon);
        }
        by_ident
            .into_iter()
            .map(|(ident, (lats, lons))| {
                let south = lats.iter().cloned().fold(f64::INFINITY, f64::min);
                let north = lats.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let (west, east) = lon_span(lons);
                (ident, Bounds { south, north, west, east })
            })
            .collect()
    })
}

/// The idents of every FIR/UIR whose own box overlaps a box: not "has a boundary point
/// inside it", which a large region round a short route can fail even while the route is
/// deep inside it (Rome's FIR reaches to Sardinia and the Adriatic; a box hugging a direct
/// London-Rome track can miss every one of its boundary points while still ending inside
/// it), but whether the two boxes overlap at all, which a region that wholly contains the
/// route's box still does.
fn fir_idents_near(b: Bounds) -> Vec<String> {
    region_bounds().iter().filter(|(_, rb)| bounds_overlap(b, *rb)).map(|(ident, _)| ident.clone()).collect()
}

fn route_bounds(pts: &[LatLon], margin_nm: f64) -> Bounds {
    let (mut s, mut n, mut w, mut e): (f64, f64, f64, f64) = (90.0, -90.0, 180.0, -180.0);
    for &(lat, lon) in pts {
        s = s.min(lat);
        n = n.max(lat);
        w = w.min(lon);
        e = e.max(lon);
    }
    let m = margin_nm / 60.0;
    let cos = ((s + n) / 2.0).to_radians().cos().max(0.2);
    Bounds { south: (s - m).max(-90.0), north: (n + m).min(90.0), west: w - m / cos, east: e + m / cos }
}

/// Points along a polyline at a fixed spacing, each with how far it is from the start.
fn sample_route(pts: &[LatLon], step_nm: f64) -> Vec<(f64, LatLon)> {
    let mut out = Vec::new();
    let mut so_far = 0.0;
    if let Some(&first) = pts.first() {
        out.push((0.0, first));
    }
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let leg = crate::dispatch::distance_nm(a, b);
        if leg < 0.01 {
            continue;
        }
        let steps = (leg / step_nm).ceil().max(1.0) as usize;
        for i in 1..=steps {
            let f = i as f64 / steps as f64;
            out.push((so_far + leg * f, along(a, b, f)));
        }
        so_far += leg;
    }
    out
}

/// Whether a point lies inside a polygon, flattened locally about the point itself: good
/// enough at the size a flight information region is, and never confused by the polygon's
/// own size the way flattening about its centroid would be for one that spans a hemisphere.
fn point_in_polygon(p: LatLon, polygon: &[LatLon]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let cos = p.0.to_radians().cos().max(0.05);
    let flat = |q: LatLon| ((q.1 - p.1) * cos, q.0 - p.0);
    let shape: Vec<(f64, f64)> = polygon.iter().map(|&q| flat(q)).collect();
    let mut inside = false;
    let mut j = shape.len() - 1;
    for i in 0..shape.len() {
        let (a, b) = (shape[i], shape[j]);
        if (a.1 > 0.0) != (b.1 > 0.0) && 0.0 < (b.0 - a.0) * (0.0 - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// One region's boundary at a single layer: whichever of its FIR or its UIR is meant,
/// under that layer's own name.
struct Zone<'a> {
    ident: &'a str,
    name: &'a str,
    parts: &'a [Vec<(f64, f64)>],
}

/// Every region reduced to the one layer that actually applies to a level: whichever of
/// its FIR or its UIR holds it. Built once per call so a route sampled against the result
/// can never straddle both bands of one region at once, the way sampling against
/// `Region::parts` whole would, and so it is reported as "PARIS" below the split and
/// "FRANCE" above it rather than as both, or as neither's proper name.
fn zones_at(level_ft: f64, regions: &[crate::sources::navdata::Region]) -> Vec<Zone<'_>> {
    regions
        .iter()
        .flat_map(|r| r.layers.iter().filter(move |l| l.floor_ft <= level_ft && level_ft <= l.ceiling_ft).map(move |l| Zone { ident: &r.ident, name: &l.name, parts: &l.parts }))
        .collect()
}

fn region_at(p: LatLon, zones: &[Zone]) -> Option<usize> {
    zones.iter().position(|z| z.parts.iter().any(|part| point_in_polygon(p, part)))
}

/// Where between two samples — one inside the zone at `idx`, the other not — the route
/// actually leaves it, found by halving the gap until it is closer than a wingspan.
fn bisect(a: (f64, LatLon), b: (f64, LatLon), zones: &[Zone], idx: usize) -> (f64, LatLon) {
    let (mut lo, mut hi) = (a, b);
    for _ in 0..24 {
        let mid_nm = (lo.0 + hi.0) / 2.0;
        let mid_pos = along(lo.1, hi.1, 0.5);
        if region_at(mid_pos, zones) == Some(idx) {
            lo = (mid_nm, mid_pos);
        } else {
            hi = (mid_nm, mid_pos);
        }
    }
    hi
}

/// Consecutive crossings of the same region collapse into one. Sampling a border that
/// weaves can flicker in and out of a neighbour for a sample or two, and a region that is
/// genuinely left and rejoined through a stretch this module could put in no region at all
/// (a box drawn too tight, a gap in the data) still touches the output list twice running,
/// with nothing between; either way it is one crossing, not several. A region truly left
/// and re-entered later, with a different one flown in between, still prints twice — the
/// two touches are not adjacent in the list, so they are not merged.
fn merge_consecutive(crossings: Vec<FirCrossing>) -> Vec<FirCrossing> {
    let mut out: Vec<FirCrossing> = Vec::new();
    for c in crossings {
        if let Some(last) = out.last_mut() {
            if last.ident == c.ident {
                last.exit_nm = c.exit_nm;
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// The regions a route's points pass through at a level, in order, sampled every few miles
/// with a point-in-polygon test, the crossing itself found by bisection, and any run of
/// the same region merged into one.
fn crossings_of(pts: &[LatLon], level_ft: f64, regions: &[crate::sources::navdata::Region]) -> Vec<FirCrossing> {
    if pts.len() < 2 || regions.is_empty() {
        return Vec::new();
    }
    let zones = zones_at(level_ft, regions);
    if zones.is_empty() {
        return Vec::new();
    }
    let samples = sample_route(pts, 4.0);
    let mut out = Vec::new();
    let mut current = region_at(samples[0].1, &zones);
    let mut entry = samples[0];

    for w in samples.windows(2) {
        let (a, b) = (w[0], w[1]);
        let next = region_at(b.1, &zones);
        if next == current {
            continue;
        }
        if let Some(idx) = current {
            let cross = bisect(a, b, &zones, idx);
            out.push(FirCrossing { ident: zones[idx].ident.to_string(), name: zones[idx].name.to_string(), entry: entry.1, entry_nm: entry.0, exit_nm: cross.0 });
            entry = cross;
        } else {
            entry = b;
        }
        current = next;
    }
    if let Some(idx) = current {
        let last = *samples.last().unwrap();
        out.push(FirCrossing { ident: zones[idx].ident.to_string(), name: zones[idx].name.to_string(), entry: entry.1, entry_nm: entry.0, exit_nm: last.0 });
    }
    merge_consecutive(out)
}

/// The regions a route passes through, in order, once each: the region responsible for
/// the airspace at the level the route is filed at, not the delegated portions ARINC also
/// carries and not both the FIR and the UIR of a region a cruise level has already climbed
/// past. `route.cruise_ft` stands for the level over the whole route; near either airport,
/// where the flight is really still climbing or already down, that slightly overstates how
/// high it is, but it is what `FiledRoute` carries and no worse a reading of "which layer"
/// than treating the whole route as though it were at one level, which a report that lists
/// regions rather than altitudes has to do somewhere.
pub fn fir_crossings(route: &FiledRoute) -> Vec<FirCrossing> {
    let pts: Vec<LatLon> = route.points.iter().map(|w| w.pos).collect();
    if pts.len() < 2 {
        return Vec::new();
    }
    let idents = fir_idents_near(route_bounds(&pts, 25.0));
    if idents.is_empty() {
        return Vec::new();
    }
    crossings_of(&pts, route.cruise_ft, &crate::sources::navdata::regions(&idents))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTAM_FIXTURE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/airspace/notam.txt"));
    const NOTAM_DANGER_FIXTURE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/airspace/notam_danger.txt"));

    // -- Conflict zones ------------------------------------------------------------

    const TEST_ZONES: &str = r#"{
        "reviewed": "2026-01-01",
        "zones": [
            { "reference": "TEST-1", "authority": "TEST", "name": "No FIR, has a polygon",
              "polygon": [[10.0, 10.0], [10.0, 12.0], [12.0, 12.0], [12.0, 10.0]],
              "floor_fl": 0, "ceiling_fl": 260, "severity": "prohibition" },
            { "reference": "TEST-2", "authority": "TEST", "name": "No FIR and no polygon: skipped",
              "severity": "prohibition" },
            { "reference": "TEST-3", "authority": "TEST", "name": "An advisory priced rather than avoided",
              "polygon": [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
              "severity": "advisory", "penalise": 1.2 },
            { "reference": "TEST-4", "authority": "TEST", "name": "Not yet in force",
              "polygon": [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
              "severity": "prohibition", "valid_from": "2099-01-01T00:00:00Z" }
        ]
    }"#;

    #[test]
    fn the_real_data_file_parses() {
        let data = zones_data();
        assert!(!data.zones.is_empty());
        for z in &data.zones {
            assert!(!z.reference.is_empty());
            assert!(z.severity == "prohibition" || z.severity == "advisory", "{}: {}", z.reference, z.severity);
        }
    }

    #[test]
    fn a_zone_with_no_fir_falls_back_to_its_own_polygon() {
        let file: ZonesFile = serde_json::from_str(TEST_ZONES).unwrap();
        let hazards = hazards_from(&file.zones, Utc::now());
        // TEST-1 and TEST-3 have polygons and are in force now; TEST-2 has neither and is
        // skipped; TEST-4 is not in force yet.
        assert_eq!(hazards.len(), 2, "{hazards:#?}");
        let avoid = hazards.iter().find(|h| h.source == "TEST-1").expect("TEST-1");
        assert_eq!(avoid.kind, HazardKind::Avoid);
        assert_eq!(avoid.top_ft, 26000.0);
        let priced = hazards.iter().find(|h| h.source == "TEST-3").expect("TEST-3");
        assert_eq!(priced.kind, HazardKind::Penalise(1.2));
    }

    #[test]
    fn a_zone_not_yet_in_force_is_left_out() {
        let file: ZonesFile = serde_json::from_str(TEST_ZONES).unwrap();
        assert!(hazards_from(&file.zones, Utc::now()).iter().all(|h| h.source != "TEST-4"));
    }

    // -- NOTAMs ----------------------------------------------------------------------

    #[test]
    fn a_restriction_notam_reads_its_q_line_and_schedule() {
        let n = parse_notam_text(NOTAM_FIXTURE).expect("a parsed NOTAM");
        assert_eq!(n.id, "A1234/26");
        assert_eq!(n.fir, "KZAU");
        assert_eq!(n.q_code, "QRTCA");
        assert_eq!(n.lower_ft, 0.0);
        assert_eq!(n.upper_ft, 9000.0);
        assert!((n.centre.0 - 41.8333).abs() < 0.001, "{}", n.centre.0);
        assert!((n.centre.1 + 87.6667).abs() < 0.001, "{}", n.centre.1);
        assert_eq!(n.radius_nm, 5.0);
        assert_eq!(n.effective_from, parse_notam_time("2609230600"));
        assert_eq!(n.effective_to, parse_notam_time("2609240600"));
        assert_eq!(n.schedule.as_deref(), Some("DAILY 0600-1200"));

        let hazard = to_hazard(&n).expect("a restriction is Avoid");
        assert_eq!(hazard.kind, HazardKind::Avoid);
        assert_eq!(hazard.polygon.len(), 24);
        assert!(hazard.name.contains("DAILY 0600-1200"));
    }

    #[test]
    fn a_warning_notam_is_priced_and_a_perm_end_is_open() {
        let n = parse_notam_text(NOTAM_DANGER_FIXTURE).expect("a parsed NOTAM");
        assert_eq!(n.q_code, "QWMLW");
        assert_eq!(n.effective_to, None, "PERM should read as no end at all");
        let hazard = to_hazard(&n).unwrap();
        assert_eq!(hazard.kind, HazardKind::Penalise(1.15));
        assert_eq!(hazard.active_to, None);
    }

    #[test]
    fn a_notam_about_something_other_than_airspace_is_not_a_hazard() {
        // QOLAS: obstacle lighting, a subject this module does not turn into a hazard.
        let n = parse_notam_text("X0001/26 NOTAMN\nQ) EGTT/QOLAS/IV/NBO/A/000/999/5130N00010W025\nA) EGLL\nE) NOTHING TO SEE HERE").unwrap();
        assert_eq!(notam_kind(&n.q_code), None);
        assert!(to_hazard(&n).is_none());
    }

    #[test]
    fn a_circle_hazard_is_a_polygon_at_the_right_radius() {
        let poly = circle_polygon((10.0, 20.0), 60.0, 24);
        assert_eq!(poly.len(), 24);
        for p in &poly {
            let d = crate::dispatch::distance_nm((10.0, 20.0), *p);
            assert!((d - 60.0).abs() < 1.0, "{d}");
        }
    }

    #[test]
    fn credentials_are_read_either_as_a_pair_or_as_one_combined_key() {
        // Cleared first: another test running in parallel on the same process must not
        // see a stale value one of these leaves behind.
        for var in ["FAA_NOTAM_CLIENT_ID", "FAA_NOTAM_CLIENT_SECRET", "FAA_NOTAM_KEY"] {
            std::env::remove_var(var);
        }
        assert_eq!(notam_credentials(), None);

        std::env::set_var("FAA_NOTAM_KEY", "abc:def");
        assert_eq!(notam_credentials(), Some(("abc".to_string(), "def".to_string())));
        std::env::remove_var("FAA_NOTAM_KEY");

        std::env::set_var("FAA_NOTAM_CLIENT_ID", "id1");
        std::env::set_var("FAA_NOTAM_CLIENT_SECRET", "secret1");
        assert_eq!(notam_credentials(), Some(("id1".to_string(), "secret1".to_string())));
        for var in ["FAA_NOTAM_CLIENT_ID", "FAA_NOTAM_CLIENT_SECRET"] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn tiles_cover_a_box_with_overlap_and_none_wider_than_the_api_allows() {
        let b = Bounds { south: 40.0, north: 42.0, west: -90.0, east: -87.0 };
        let t = tiles(b);
        assert!(!t.is_empty());
        assert!(t.iter().all(|(_, r)| *r <= 100.0));
        // Every corner of the box is within some tile's radius of that tile's centre.
        for corner in [(b.south, b.west), (b.south, b.east), (b.north, b.west), (b.north, b.east)] {
            assert!(t.iter().any(|(c, r)| crate::dispatch::distance_nm(*c, corner) <= *r + 1.0), "{corner:?} uncovered: {t:?}");
        }
    }

    // -- FIR crossings -----------------------------------------------------------------

    /// A region with a single part and a single layer covering the whole sky: good enough
    /// for any test that is not about the FIR/UIR split itself.
    fn region(ident: &str, box_: [(f64, f64); 4]) -> crate::sources::navdata::Region {
        region_parts(ident, &format!("{ident} FIR"), vec![box_.to_vec()])
    }

    fn region_parts(ident: &str, name: &str, parts: Vec<Vec<(f64, f64)>>) -> crate::sources::navdata::Region {
        crate::sources::navdata::Region {
            ident: ident.to_string(),
            name: name.to_string(),
            parts: parts.clone(),
            layers: vec![crate::sources::navdata::RegionLayer { name: name.to_string(), floor_ft: 0.0, ceiling_ft: 99_999.0, parts }],
        }
    }

    /// A region with the FIR and UIR named and bounded apart, the way LFFF's really are:
    /// "PARIS" below `split_ft`, "FRANCE" above it.
    fn layered_region(ident: &str, box_: [(f64, f64); 4], fir_name: &str, split_ft: f64, uir_name: &str) -> crate::sources::navdata::Region {
        let parts = vec![box_.to_vec()];
        crate::sources::navdata::Region {
            ident: ident.to_string(),
            name: fir_name.to_string(),
            parts: parts.clone(),
            layers: vec![
                crate::sources::navdata::RegionLayer { name: fir_name.to_string(), floor_ft: 0.0, ceiling_ft: split_ft, parts: parts.clone() },
                crate::sources::navdata::RegionLayer { name: uir_name.to_string(), floor_ft: split_ft, ceiling_ft: 99_999.0, parts },
            ],
        }
    }

    const CRUISE_FT: f64 = 35_000.0;

    #[test]
    fn a_straight_leg_through_two_regions_is_split_at_the_boundary() {
        // Two FIRs side by side along the equator, meeting at longitude 0.
        let west = region("WEST", [(-5.0, -10.0), (5.0, -10.0), (5.0, 0.0), (-5.0, 0.0)]);
        let east = region("EAST", [(-5.0, 0.0), (5.0, 0.0), (5.0, 10.0), (-5.0, 10.0)]);
        let pts = vec![(0.0, -8.0), (0.0, 8.0)];
        let crossings = crossings_of(&pts, CRUISE_FT, &[west, east]);
        assert_eq!(crossings.len(), 2, "{crossings:#?}");
        assert_eq!(crossings[0].ident, "WEST");
        assert_eq!(crossings[1].ident, "EAST");
        assert!((crossings[0].entry_nm - 0.0).abs() < 1.0);
        // The boundary sits at longitude 0, eight degrees from the start: at the equator,
        // a degree of longitude is a degree of great-circle arc, sixty nautical miles.
        let expected_boundary_nm = crate::dispatch::distance_nm((0.0, -8.0), (0.0, 0.0));
        assert!((crossings[0].exit_nm - expected_boundary_nm).abs() < 2.0, "{} vs {}", crossings[0].exit_nm, expected_boundary_nm);
        assert!((crossings[1].entry_nm - crossings[0].exit_nm).abs() < 1e-6);
        let total = crate::dispatch::distance_nm((0.0, -8.0), (0.0, 8.0));
        assert!((crossings[1].exit_nm - total).abs() < 1.0);
    }

    #[test]
    fn a_leg_that_never_enters_a_region_crosses_nothing() {
        let far = region("FAR", [(40.0, 40.0), (41.0, 40.0), (41.0, 41.0), (40.0, 41.0)]);
        let pts = vec![(0.0, 0.0), (0.0, 10.0)];
        assert!(crossings_of(&pts, CRUISE_FT, &[far]).is_empty());
    }

    #[test]
    fn consecutive_touches_of_one_region_across_a_gap_are_reported_once() {
        // Two disjoint pieces of the same region, the way a region genuinely can be drawn
        // (an archipelago, say), with a stretch between them that is in no known region at
        // all — the sort of hole a tight box or missing data leaves. The route touches
        // "A" twice with nothing else in between, so it is one crossing, not two.
        let a = region_parts("A", "A FIR", vec![vec![(-5.0, -10.0), (5.0, -10.0), (5.0, -6.0), (-5.0, -6.0)], vec![(-5.0, 6.0), (5.0, 6.0), (5.0, 10.0), (-5.0, 10.0)]]);
        let pts = vec![(0.0, -8.0), (0.0, 8.0)];
        let crossings = crossings_of(&pts, CRUISE_FT, &[a]);
        assert_eq!(crossings.len(), 1, "{crossings:#?}");
        assert_eq!(crossings[0].ident, "A");
        assert!((crossings[0].entry_nm - 0.0).abs() < 1.0);
        let total = crate::dispatch::distance_nm((0.0, -8.0), (0.0, 8.0));
        assert!((crossings[0].exit_nm - total).abs() < 1.0, "{} vs {}", crossings[0].exit_nm, total);
    }

    #[test]
    fn a_region_left_and_later_rejoined_with_another_flown_between_prints_twice() {
        // Unlike the gap above, here a different region is actually flown between the two
        // touches of "A", so they are not adjacent in the crossing list and must not merge.
        let a = region("A", [(-5.0, -10.0), (5.0, -10.0), (5.0, -6.0), (-5.0, -6.0)]);
        let b = region("B", [(-5.0, -6.0), (5.0, -6.0), (5.0, 6.0), (-5.0, 6.0)]);
        let pts = vec![(0.0, -8.0), (0.0, 0.0), (0.0, 8.0)];
        // The straight leg from -8 to 8 would just cross A once then B; flying it via 0.0
        // and back doesn't touch A twice by itself, so cross A, B, and A again explicitly.
        let a2 = region("A", [(-5.0, 6.0), (5.0, 6.0), (5.0, 10.0), (-5.0, 10.0)]);
        let crossings = crossings_of(&pts, CRUISE_FT, &[a, b, a2]);
        let idents: Vec<&str> = crossings.iter().map(|c| c.ident.as_str()).collect();
        assert_eq!(idents, vec!["A", "B", "A"], "{crossings:#?}");
    }

    #[test]
    fn the_layer_that_holds_the_level_is_the_one_reported() {
        // Paris below the split, France above it: the FIR and UIR of one region, named
        // and bounded apart the way LFFF's really are in the ARINC table.
        let paris = layered_region("LFFF", [(-2.0, -2.0), (-2.0, 2.0), (2.0, 2.0), (2.0, -2.0)], "PARIS", 19_500.0, "FRANCE");
        let pts = vec![(0.0, -1.0), (0.0, 1.0)];

        let low = crossings_of(&pts, 10_000.0, &[paris.clone()]);
        assert_eq!(low.len(), 1, "{low:#?}");
        assert_eq!(low[0].ident, "LFFF");
        assert_eq!(low[0].name, "PARIS");

        let high = crossings_of(&pts, 39_000.0, &[paris]);
        assert_eq!(high.len(), 1, "{high:#?}");
        assert_eq!(high[0].ident, "LFFF");
        assert_eq!(high[0].name, "FRANCE");
    }

    /// Run by hand: the live FAA NOTAM API, which needs credentials and a network.
    #[test]
    #[ignore]
    fn live_notams_can_be_fetched() {
        let bounds = Bounds { south: 41.0, north: 42.0, west: -88.5, east: -87.0 };
        let hazards = notam_hazards(bounds, Utc::now(), Utc::now() + chrono::Duration::hours(6)).expect("live NOTAMs");
        println!("{} NOTAM hazards near KORD", hazards.len());
    }

    // -- Real navigation database, run by hand -----------------------------------------
    //
    // These need a navigation database installed (see `sources::navdata`) and check the
    // fix against real ARINC 424 data rather than hand-built regions: no run of the same
    // identifier back to back, no name that is really a delegation note, and a flight into
    // the destination ends in that country's own region.

    /// A direct great-circle route between two airports, at one level throughout: enough
    /// for `fir_crossings`, which only reads the points and the filed cruise level.
    fn direct_route(origin_icao: &str, origin_pos: LatLon, destination_icao: &str, destination_pos: LatLon, cruise_ft: f64) -> FiledRoute {
        use crate::dispatch::{Airport, PointKind, Waypoint};
        FiledRoute {
            origin: Airport { icao: origin_icao.to_string(), name: String::new(), pos: origin_pos, elevation_ft: 0.0 },
            destination: Airport { icao: destination_icao.to_string(), name: String::new(), pos: destination_pos, elevation_ft: 0.0 },
            dep_runway: None,
            sid: None,
            sid_transition: None,
            star: None,
            star_transition: None,
            arr_runway: None,
            approach: None,
            points: vec![Waypoint::new(origin_icao, origin_pos, "DCT", PointKind::Airport), Waypoint::new(destination_icao, destination_pos, "", PointKind::Airport)],
            cruise_ft,
            off_block: Utc::now(),
        }
    }

    fn assert_sane(crossings: &[FirCrossing], destination_ident: &str) {
        assert!(!crossings.is_empty(), "no FIR crossings at all");
        for c in crossings {
            assert!(!c.name.to_uppercase().contains("DELEGATED"), "a delegation note printed as a name: {c:?}");
        }
        for w in crossings.windows(2) {
            assert_ne!(w[0].ident, w[1].ident, "consecutive crossings of {} were not merged: {crossings:#?}", w[0].ident);
        }
        assert_eq!(crossings.last().unwrap().ident, destination_ident, "the flight should end in the destination's own region: {crossings:#?}");
    }

    #[test]
    #[ignore]
    fn london_to_rome() {
        let route = direct_route("EGLL", (51.4775, -0.4614), "LIRF", (41.8003, 12.2389), 39_000.0);
        let crossings = fir_crossings(&route);
        println!("{crossings:#?}");
        assert_sane(&crossings, "LIRR");
    }

    #[test]
    #[ignore]
    fn london_to_new_york_crosses_shanwick_and_gander() {
        let route = direct_route("EGLL", (51.4775, -0.4614), "KJFK", (40.6398, -73.7789), 35_000.0);
        let crossings = fir_crossings(&route);
        println!("{crossings:#?}");
        assert_sane(&crossings, "KZNY");
        assert!(crossings.iter().any(|c| c.ident == "EGGX"), "no Shanwick: {crossings:#?}");
        assert!(crossings.iter().any(|c| c.ident.starts_with("CZQ")), "no Gander: {crossings:#?}");
    }

    #[test]
    #[ignore]
    fn mumbai_to_delhi() {
        let route = direct_route("VABB", (19.0887, 72.8679), "VIDP", (28.5665, 77.1031), 37_000.0);
        let crossings = fir_crossings(&route);
        println!("{crossings:#?}");
        assert_sane(&crossings, "VIDF");
    }

    #[test]
    #[ignore]
    fn new_york_to_boston() {
        let route = direct_route("KJFK", (40.6398, -73.7789), "KBOS", (42.3643, -71.0052), 35_000.0);
        let crossings = fir_crossings(&route);
        println!("{crossings:#?}");
        assert_sane(&crossings, "KZBW");
    }
}
