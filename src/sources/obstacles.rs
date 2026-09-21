//! Obstacles near an airport: masts, towers, chimneys and the like. On a real approach
//! these are usually what sets the minimum, so a minimum worked out from terrain alone
//! is only half the answer.
//!
//! Two free sources, and both are public.
//!
//! In the United States the FAA's Digital Obstacle File is the authority: every
//! obstruction it knows of, with a surveyed height above ground and above sea level, and
//! whether it is lit. It is served as a queryable layer, so only the obstacles near the
//! airport are fetched.
//!
//! Everywhere else OpenStreetMap carries the tall structures people have mapped. A
//! height tag is common on masts and chimneys and rare on everything else, so only the
//! ones that carry a height are kept: a guessed height would push a minimum up for no
//! reason. OpenStreetMap heights are above the ground, so the terrain model turns them
//! into heights above sea level.

use crate::cache::Cache;
use crate::sources::copernicus::Patch;
use crate::sources::http::Http;
use anyhow::Result;
use serde::{Deserialize, Serialize};

const FAA_DOF: &str = "https://services6.arcgis.com/ssFJjBXIUyZDrSYZ/ArcGIS/rest/services/Digital_Obstacle_File/FeatureServer/0/query";

/// Below this an obstacle cannot reach an approach path and only clutters the chart.
const MIN_HEIGHT_FT: f64 = 50.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Obstacle {
    pub lat: f64,
    pub lon: f64,
    /// What it is: "TOWER", "mast", "chimney".
    pub kind: String,
    /// Height above the ground in feet, where the source gives one.
    pub agl_ft: Option<f64>,
    /// Top above sea level in feet. Surveyed in the FAA file; terrain plus the tagged
    /// height in OpenStreetMap.
    pub top_ft: Option<f64>,
    pub lit: bool,
    /// Which source it came from, for the chart's credits.
    pub source: &'static str,
}

impl Obstacle {
    pub fn label(&self) -> String {
        let kind = self.kind.trim().to_uppercase();
        match self.agl_ft {
            Some(a) => format!("{kind} {a:.0} AGL"),
            None => kind,
        }
    }
}

/// Obstacles within `radius_km` of a point. The FAA file is tried first and covers the
/// United States; OpenStreetMap answers for the rest of the world.
pub fn around(http: &Http, cache: &Cache, icao: &str, lat: f64, lon: f64, radius_km: f64, mirrors: &[String]) -> Result<Vec<Obstacle>> {
    let bbox = bbox(lat, lon, radius_km);
    let mut out = match faa(http, cache, icao, bbox) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("{icao}: FAA obstacle file: {e:#}");
            Vec::new()
        }
    };
    if out.is_empty() {
        out = match osm(http, cache, icao, bbox, mirrors) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("{icao}: OpenStreetMap obstacles: {e:#}");
                Vec::new()
            }
        };
    }
    out.retain(|o| o.agl_ft.unwrap_or(0.0) >= MIN_HEIGHT_FT || o.top_ft.is_some() && o.agl_ft.is_none());
    out.sort_by(|a, b| b.top_ft.unwrap_or(0.0).total_cmp(&a.top_ft.unwrap_or(0.0)));
    Ok(out)
}

/// South, west, north, east.
fn bbox(lat: f64, lon: f64, radius_km: f64) -> (f64, f64, f64, f64) {
    let dlat = radius_km / 111.32;
    let dlon = dlat / lat.to_radians().cos().abs().max(0.05);
    (lat - dlat, lon - dlon, lat + dlat, lon + dlon)
}

fn faa(http: &Http, cache: &Cache, icao: &str, (s, w, n, e): (f64, f64, f64, f64)) -> Result<Vec<Obstacle>> {
    let key = format!("obstacles/faa/{}.json", icao.to_uppercase());
    let geometry = format!("{w:.5},{s:.5},{e:.5},{n:.5}");
    let text = cache.get_or_fetch_text(&key, || {
        let url = format!(
            "{FAA_DOF}?where=1%3D1&outFields=Type_Code,AGL,AMSL,Lighting,Lat_DD,Long_DD&geometry={geometry}&geometryType=esriGeometryEnvelope&inSR=4326&outSR=4326&returnGeometry=false&resultRecordCount=2000&f=json"
        );
        // The service is shared and answers "too many requests" with a 200 and an error
        // body, so the body decides whether this is an answer worth keeping.
        let mut last = String::new();
        for attempt in 0..3 {
            match http.get_text(&url) {
                Ok(t) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                    if v.get("features").is_some() {
                        return Ok(t);
                    }
                    last = v["error"]["message"].as_str().unwrap_or("no features in the answer").to_string();
                }
                Err(e) => last = format!("{e:#}"),
            }
            log::warn!("FAA obstacle file: {last}; retrying");
            std::thread::sleep(std::time::Duration::from_secs(10 * (attempt + 1)));
        }
        Err(anyhow::anyhow!("{last}"))
    })?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let mut out = Vec::new();
    for f in v.get("features").and_then(|f| f.as_array()).into_iter().flatten() {
        let a = &f["attributes"];
        let num = |k: &str| a.get(k).and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.trim().parse().ok())));
        let (Some(lat), Some(lon)) = (num("Lat_DD"), num("Long_DD")) else { continue };
        out.push(Obstacle {
            lat,
            lon,
            kind: a["Type_Code"].as_str().unwrap_or("OBSTACLE").trim().to_string(),
            agl_ft: num("AGL"),
            top_ft: num("AMSL"),
            lit: !matches!(a["Lighting"].as_str().unwrap_or("N").trim(), "N" | "U" | ""),
            source: "FAA Digital Obstacle File",
        });
    }
    Ok(out)
}

/// The tall things OpenStreetMap maps, worldwide.
fn osm(http: &Http, cache: &Cache, icao: &str, (s, w, n, e): (f64, f64, f64, f64), mirrors: &[String]) -> Result<Vec<Obstacle>> {
    let key = format!("obstacles/osm/{}.json", icao.to_uppercase());
    let q = format!(
        "[out:json][timeout:90];(\
nwr[\"man_made\"~\"^(mast|tower|communications_tower|chimney|water_tower|cooling_tower|storage_tank|silo|windmill|crane|antenna)$\"]({s:.5},{w:.5},{n:.5},{e:.5});\
nwr[\"building\"~\"^(tower|skyscraper)$\"]({s:.5},{w:.5},{n:.5},{e:.5});\
nwr[\"generator:source\"=\"wind\"]({s:.5},{w:.5},{n:.5},{e:.5});\
);out center tags;"
    );
    let text = cache.get_or_fetch_text(&key, || {
        let mut last = None;
        for m in mirrors {
            match http.post_form_text_once(m, &[("data", q.as_str())]) {
                Ok(t) => return Ok(t),
                Err(err) => {
                    log::warn!("overpass {m}: {err:#}");
                    last = Some(err);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("no overpass mirror answered")))
    })?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let mut out = Vec::new();
    for el in v.get("elements").and_then(|e| e.as_array()).into_iter().flatten() {
        let (lat, lon) = match (el.get("lat").and_then(|v| v.as_f64()), el.get("lon").and_then(|v| v.as_f64())) {
            (Some(a), Some(o)) => (a, o),
            _ => match el.get("center") {
                Some(c) => (c["lat"].as_f64().unwrap_or(f64::NAN), c["lon"].as_f64().unwrap_or(f64::NAN)),
                None => continue,
            },
        };
        if !lat.is_finite() || !lon.is_finite() {
            continue;
        }
        let tags = &el["tags"];
        let Some(metres) = tagged_height(tags) else { continue };
        let kind = ["man_made", "building", "generator:source"].iter().find_map(|k| tags[*k].as_str()).unwrap_or("structure");
        out.push(Obstacle {
            lat,
            lon,
            kind: kind.to_string(),
            agl_ft: Some((metres / 0.3048).round()),
            // Filled in from the terrain model, which is what the ground under it is.
            top_ft: None,
            lit: tags["lit"].as_str() == Some("yes"),
            source: "OpenStreetMap",
        });
    }
    Ok(out)
}

/// A height in metres from the tags that carry one. Feet are written a few ways, so
/// both are read and anything unparseable is dropped rather than guessed at.
fn tagged_height(tags: &serde_json::Value) -> Option<f64> {
    for key in ["height", "building:height", "tower:height", "man_made:height"] {
        let Some(raw) = tags[key].as_str() else { continue };
        let raw = raw.trim();
        let feet = raw.ends_with('\'') || raw.to_ascii_lowercase().ends_with("ft");
        let num: String = raw.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let Ok(v) = num.parse::<f64>() else { continue };
        if v <= 0.0 || v > 1000.0 {
            continue;
        }
        return Some(if feet { v * 0.3048 } else { v });
    }
    None
}

/// Give every obstacle a top above sea level, using the terrain under it where the
/// source only gave a height above ground.
pub fn resolve_tops(obstacles: &mut [Obstacle], patch: &Patch) {
    for o in obstacles.iter_mut() {
        if o.top_ft.is_some() {
            continue;
        }
        let (Some(ground_m), Some(agl)) = (patch.height_at(o.lat, o.lon), o.agl_ft) else { continue };
        o.top_ft = Some((ground_m / 0.3048 + agl).round());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_are_read_in_both_units() {
        let m = serde_json::json!({ "height": "120 m" });
        assert_eq!(tagged_height(&m), Some(120.0));
        let ft = serde_json::json!({ "height": "300 ft" });
        assert_eq!(tagged_height(&ft).map(|v| v.round()), Some(91.0));
        let none = serde_json::json!({ "man_made": "mast" });
        assert_eq!(tagged_height(&none), None);
    }

    #[test]
    fn a_bbox_widens_with_latitude() {
        let (_, w, _, e) = bbox(60.0, 10.0, 10.0);
        let (_, w2, _, e2) = bbox(0.0, 10.0, 10.0);
        assert!(e - w > e2 - w2);
    }
}
