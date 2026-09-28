//! FAA airport mapping layers (US only), from the agency's public ArcGIS feature
//! services. These are DO-272 airport-mapping features published as open data by a US
//! government agency: no key, no registration, public domain.
//!
//! What we take and why:
//! * **hotspots** always, with their published caution text. No other free worldwide
//!   source publishes hotspots at all, so this is pure gain for the airports it covers.
//! * **wind indicators** when no other source gave us one.
//! * **taxiway and apron polygons, and buildings** only as a fallback, when neither the
//!   Scenery Gateway nor a local X-Plane install had the airport. Where scenery exists
//!   it is richer (bezier edges, markings, stands), so duplicating it would be worse.
//!
//! Coverage is roughly 187 US airports for the pavement layers and 66 for hotspots.

use crate::cache::Cache;
use crate::ir::{Area, AreaKind, Building, LonLat, Pavement, PavementHint, PointStructure, SourceAirport};
use crate::model::codes::{plysttyp, pntsttyp, source, surftype};
use crate::sources::http::Http;
use anyhow::{Context, Result};
use serde_json::Value;

const BASE: &str = "https://services6.arcgis.com/ssFJjBXIUyZDrSYZ/arcgis/rest/services";

/// True for airports the FAA publishes mapping data for.
pub fn covers(icao: &str, country: Option<&str>) -> bool {
    // Sources spell the country differently ("US" from the index, "USA United States"
    // from apt.dat), so accept either and fall back to the ICAO prefixes the FAA
    // assigns when nothing said.
    if let Some(c) = country {
        let c = c.trim().to_uppercase();
        if c == "US" || c == "USA" || c.starts_with("US ") || c.starts_with("USA ") || c.contains("UNITED STATES") {
            return true;
        }
        // A recognised other country: not FAA territory.
        if c.len() == 2 || c.len() == 3 {
            return false;
        }
    }
    let u = icao.to_uppercase();
    u.starts_with('K') || u.starts_with("PA") || u.starts_with("PH")
}

fn query_url(layer: &str, icao: &str) -> String {
    format!("{BASE}/{layer}/FeatureServer/0/query?where=ICAO_ID%3D%27{}%27&outFields=*&outSR=4326&f=geojson", icao.to_uppercase())
}

/// The airports a layer has any feature for, asked once per layer (distinct ICAO_IDs) and
/// kept for a week. The FAA maps a couple of hundred airports; asking every US airport
/// and heliport of a big batch about every layer cost tens of thousands of requests that
/// all came back empty. None when the service will not say, and each airport is asked.
fn covered(http: &Http, cache: &Cache, name: &str) -> Option<std::sync::Arc<std::collections::HashSet<String>>> {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex, OnceLock};
    type Once = Arc<OnceLock<Option<Arc<HashSet<String>>>>>;
    // One slot a layer, so every worker that asks while the list is coming waits for the
    // one request rather than making its own.
    static KNOWN: OnceLock<Mutex<HashMap<String, Once>>> = OnceLock::new();
    let slot = KNOWN.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap().entry(name.to_string()).or_default().clone();
    slot.get_or_init(|| {
        let path = cache.path(&format!("faa-amdb/_covered/{name}.json"));
        let fresh = path.as_ref().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() < 7 * 24 * 3600);
        let from_disk = if fresh { path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok()) } else { None };
        let ids = from_disk.or_else(|| {
            let mut ids = Vec::new();
            let mut offset = 0;
            loop {
                let url = format!("{BASE}/{name}/FeatureServer/0/query?where=1%3D1&outFields=ICAO_ID&returnDistinctValues=true&returnGeometry=false&resultOffset={offset}&f=json");
                let v: Value = http.get_text(&url).ok().and_then(|t| serde_json::from_str(&t).ok())?;
                if v.get("error").is_some() {
                    return None;
                }
                let page: Vec<String> = v.get("features")?.as_array()?.iter().filter_map(|f| f.get("attributes")?.get("ICAO_ID")?.as_str().map(|s| s.trim().to_uppercase())).collect();
                offset += page.len();
                let more = v.get("exceededTransferLimit").and_then(Value::as_bool).unwrap_or(false) && !page.is_empty();
                ids.extend(page);
                if !more {
                    break;
                }
            }
            if let Some(p) = &path {
                if let Some(dir) = p.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(p, serde_json::to_string(&ids).unwrap_or_default());
            }
            Some(ids)
        });
        let set = ids.map(|v| Arc::new(v.into_iter().filter(|s| !s.is_empty()).collect::<HashSet<String>>()));
        if let Some(s) = &set {
            crate::term::info(&format!("FAA {name}: {} airports mapped", s.len()));
        }
        set
    })
    .clone()
}

/// Every layer the FAA publishes for airports, by the name its service goes by.
const LAYERS: [&str; 5] = ["AM_Hotspot", "AM_Wind_Indicator", "AM_Taxiway", "AM_Apron", "AM_Building"];

/// The airports the FAA maps anything at all for, out of the coverage lists. Empty when
/// the service will not say, in which case nothing is known to be worth putting last.
pub fn mapped_airports(cache: &Cache) -> std::collections::HashSet<String> {
    let http = Http::new(60, 100);
    LAYERS.iter().filter_map(|name| covered(&http, cache, name)).flat_map(|s| s.iter().cloned().collect::<Vec<_>>()).collect()
}

/// One layer as GeoJSON features. Missing layers and transport errors are not fatal:
/// the caller logs and carries on with whatever the other sources gave. An airport the
/// layer is known to have nothing for is not asked about.
fn layer(http: &Http, cache: &Cache, icao: &str, name: &str) -> Result<Vec<Value>> {
    if covered(http, cache, name).is_some_and(|set| !set.contains(&icao.to_uppercase())) {
        return Ok(Vec::new());
    }
    let key = format!("faa-amdb/{}/{}.json", icao.to_uppercase(), name);
    let text = cache.get_or_fetch_text(&key, || {
        crate::term::step(Some(icao), &format!("GET FAA {name}"));
        http.get_text(&query_url(name, icao))
    })?;
    let v: Value = serde_json::from_str(&text).with_context(|| format!("parse FAA {name}"))?;
    if let Some(err) = v.get("error") {
        return Err(anyhow::anyhow!("FAA {name}: {err}"));
    }
    Ok(v.get("features").and_then(Value::as_array).cloned().unwrap_or_default())
}

/// A property as text. The service returns some fields as numbers (HOT_ID) and some as
/// strings (SURFACE), so accept either.
fn prop_str(f: &Value, k: &str) -> Option<String> {
    match f.get("properties")?.get(k)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The FAA's `SURFACE` is DO-272 coded, the same list as our `surftype`.
fn surface(f: &Value) -> i64 {
    prop_str(f, "SURFACE").and_then(|s| s.parse::<i64>().ok()).unwrap_or(surftype::UNKNOWN)
}

/// Rings of every polygon in a GeoJSON feature, as (outer, holes).
fn rings(f: &Value) -> Vec<(Vec<LonLat>, Vec<Vec<LonLat>>)> {
    let Some(g) = f.get("geometry") else { return vec![] };
    let Ok(geom) = crate::output::geojson::geometry_from_json(g) else { return vec![] };
    let to_ring = |ls: &geo_types::LineString<f64>| ls.0.clone();
    let one = |p: &geo_types::Polygon<f64>| (to_ring(p.exterior()), p.interiors().iter().map(to_ring).collect());
    match geom {
        geo_types::Geometry::Polygon(p) => vec![one(&p)],
        geo_types::Geometry::MultiPolygon(m) => m.0.iter().map(one).collect(),
        _ => vec![],
    }
}

/// A plain (straight-edged) pavement ring from a list of positions.
fn ring(pts: &[LonLat]) -> crate::ir::Ring {
    crate::ir::Ring { verts: pts.iter().map(|p| crate::ir::Vertex { pos: *p, ctrl: None, line: 0, light: 0 }).collect(), closed: true }
}

fn point(f: &Value) -> Option<LonLat> {
    let g = f.get("geometry")?;
    match crate::output::geojson::geometry_from_json(g).ok()? {
        geo_types::Geometry::Point(p) => Some(p.0),
        _ => None,
    }
}

/// Every layer the FAA has for the airports of a batch that it maps at all, fetched on a
/// thread of its own with its own connection, so a big batch never waits on the FAA:
/// by the time a worker reaches one of these airports its answers are in the cache.
/// The rest of the batch the coverage lists already answer without a request.
pub fn prefetch(cache: Cache, icaos: Vec<String>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let http = Http::new(60, 100);
        let wanted: Vec<String> = icaos.into_iter().map(|i| i.to_uppercase()).filter(|i| covers(i, None)).collect();
        let mut n = 0usize;
        for name in LAYERS {
            let Some(set) = covered(&http, &cache, name) else { continue };
            for icao in wanted.iter().filter(|i| set.contains(*i)) {
                if layer(&http, &cache, icao, name).is_ok() {
                    n += 1;
                }
            }
        }
        crate::term::info(&format!("FAA airport mapping fetched ahead: {n} layer(s)"));
    })
}

/// Fetch what the FAA publishes for this airport.
///
/// `want_pavement` asks for the taxiway, apron and building polygons as well; pass it
/// only when no scenery was found, so real scenery is never duplicated.
/// `want_windsock` likewise for wind indicators.
pub fn fetch(http: &Http, cache: &Cache, icao: &str, want_pavement: bool, want_windsock: bool) -> Result<SourceAirport> {
    let mut out = SourceAirport::new(icao);
    let mut got = 0usize;

    // Hotspots: the reason this source exists. `reference` carries the caution text.
    match layer(http, cache, icao, "AM_Hotspot") {
        Ok(fs) => {
            for f in &fs {
                let id = prop_str(f, "HOT_ID").map(|s| format!("HS{s}"));
                let text = prop_str(f, "CS_TEXT");
                for (outer, holes) in rings(f) {
                    out.areas.push(Area { kind: AreaKind::Hotspot, outer, holes, name: id.clone(), surface: None, reference: text.clone(), source: source::FAA_AMDB });
                    got += 1;
                }
            }
        }
        Err(e) => log::info!("{icao}: no FAA hotspots ({e:#})"),
    }

    if want_windsock {
        match layer(http, cache, icao, "AM_Wind_Indicator") {
            Ok(fs) => {
                for f in &fs {
                    if let Some(pos) = point(f) {
                        out.point_structures.push(PointStructure { pos, kind: pntsttyp::WINDSOCK, height_m: None, name: None, source: source::FAA_AMDB });
                        got += 1;
                    }
                }
            }
            Err(e) => log::info!("{icao}: no FAA wind indicators ({e:#})"),
        }
    }

    if want_pavement {
        for (name, hint) in [("AM_Taxiway", PavementHint::Taxiway), ("AM_Apron", PavementHint::Apron)] {
            match layer(http, cache, icao, name) {
                Ok(fs) => {
                    for f in &fs {
                        let designator = prop_str(f, "DESIGNATOR");
                        let surf = surface(f);
                        for (outer, holes) in rings(f) {
                            out.pavements.push(Pavement { surface: surf, name: designator.clone(), hint, outer: ring(&outer), holes: holes.iter().map(|h| ring(h)).collect(), source: source::FAA_AMDB });
                            got += 1;
                        }
                    }
                }
                Err(e) => log::info!("{icao}: no FAA {name} ({e:#})"),
            }
        }
        match layer(http, cache, icao, "AM_Building") {
            Ok(fs) => {
                for f in &fs {
                    for (outer, holes) in rings(f) {
                        out.buildings.push(Building { outer, holes, name: None, kind: plysttyp::BUILDING, height_m: None, levels: None, source: source::FAA_AMDB });
                        got += 1;
                    }
                }
            }
            Err(e) => log::info!("{icao}: no FAA buildings ({e:#})"),
        }
    }

    if got > 0 {
        out.sources.push(source::FAA_AMDB.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_us_airports_only() {
        assert!(covers("KJFK", Some("US")));
        assert!(covers("KJFK", Some("USA United States")), "apt.dat spells the country out");
        assert!(!covers("LFPG", Some("FR")));
        assert!(!covers("LFPG", Some("FRA France")));
        assert!(!covers("KJFK", Some("FR")), "an explicit country wins over the prefix");
        assert!(covers("KJFK", None), "no country: fall back to the ICAO prefix");
        assert!(covers("PANC", None));
        assert!(!covers("VOBL", None));
        assert!(!covers("VOBL", Some("IND India")));
    }

    #[test]
    fn parses_a_hotspot_feature() {
        let f: Value = serde_json::from_str(
            r#"{"type":"Feature","properties":{"HOT_ID":3,"CS_TEXT":"Watch for Twy K.","SURFACE":"5"},
                "geometry":{"type":"Polygon","coordinates":[[[-73.8,40.6],[-73.7,40.6],[-73.7,40.7],[-73.8,40.6]]]}}"#,
        )
        .unwrap();
        assert_eq!(prop_str(&f, "HOT_ID").as_deref(), Some("3"));
        assert_eq!(prop_str(&f, "CS_TEXT").as_deref(), Some("Watch for Twy K."));
        assert_eq!(surface(&f), 5);
        let r = rings(&f);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0.len(), 4);
        assert!(r[0].1.is_empty());
        assert!(prop_str(&f, "MISSING").is_none());
    }

    #[test]
    fn query_url_is_icao_scoped() {
        let u = query_url("AM_Hotspot", "kjfk");
        assert!(u.contains("AM_Hotspot/FeatureServer/0/query"));
        assert!(u.contains("ICAO_ID%3D%27KJFK%27"));
        assert!(u.contains("outSR=4326") && u.contains("f=geojson"));
    }
}
