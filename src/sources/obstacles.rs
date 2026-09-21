//! Obstacles near an airport: masts, towers, chimneys and the like. On a real approach
//! these are usually what sets the minimum, so a minimum worked out from terrain alone
//! is only half the answer.
//!
//! Two free sources, and both are public.
//!
//! In the United States the FAA's Digital Obstacle File is the authority: every
//! obstruction it knows of, with a surveyed height above ground and above sea level, and
//! whether it is lit. It is published as one file for the country, reissued every 56
//! days, and read once per run.
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
    let icao = &format!("{}-{radius_km:.0}km", icao.to_uppercase());

    let mut out = match faa(http, cache, bbox) {
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

/// The Digital Obstacle File as the FAA publishes it: one zip for the country, a
/// fixed-width file per state, reissued every 56 days.
///
/// The same data is served as a queryable layer, but that service shares one quota with
/// everyone else using it and starts refusing requests after a few dozen airports. The
/// file is thirty megabytes, is cached, and then answers every airport in the country
/// without asking anyone's permission.
fn faa(http: &Http, cache: &Cache, (s, w, n, e): (f64, f64, f64, f64)) -> Result<Vec<Obstacle>> {
    static ALL: std::sync::OnceLock<Vec<Obstacle>> = std::sync::OnceLock::new();
    let all = ALL.get_or_init(|| match load_dof(http, cache) {
        Ok(v) => v,
        Err(err) => {
            log::warn!("FAA obstacle file: {err:#}");
            Vec::new()
        }
    });
    Ok(all.iter().filter(|o| o.lat >= s && o.lat <= n && o.lon >= w && o.lon <= e).cloned().collect())
}

/// The cycle running on a date. The file is reissued every 56 days; 2 August 2026 was
/// one such issue, and every other is a whole number of cycles from it.
pub fn dof_cycle(today: chrono::NaiveDate) -> chrono::NaiveDate {
    let epoch = chrono::NaiveDate::from_ymd_opt(2026, 8, 2).expect("a real date");
    let days = (today - epoch).num_days();
    epoch + chrono::Duration::days(days.div_euclid(56) * 56)
}

fn dof_url(cycle: chrono::NaiveDate) -> String {
    format!("https://aeronav.faa.gov/Obst_Data/DOF_{}.zip", cycle.format("%y%m%d"))
}

fn load_dof(http: &Http, cache: &Cache) -> Result<Vec<Obstacle>> {
    let cycle = dof_cycle(chrono::Utc::now().date_naive());
    let url = dof_url(cycle);
    let bytes = cache.get_or_fetch_bytes(&format!("obstacles/faa/DOF_{}.zip", cycle.format("%y%m%d")), || http.get_bytes(&url))?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let mut member = zip.by_index(i)?;
        if !member.name().to_ascii_uppercase().ends_with(".DAT") {
            continue;
        }
        let mut text = String::new();
        {
            use std::io::Read;
            let mut raw = Vec::new();
            member.read_to_end(&mut raw)?;
            text = String::from_utf8_lossy(&raw).into_owned();
        }
        for line in text.lines() {
            if let Some(o) = parse_dof_line(line) {
                out.push(o);
            }
        }
    }
    Ok(out)
}

/// One obstacle from a line of the file. The columns are fixed: position in degrees,
/// minutes and seconds, then the type, the height above ground and the height above sea
/// level, both in feet.
fn parse_dof_line(line: &str) -> Option<Obstacle> {
    if line.len() < 96 || !line.as_bytes()[9].is_ascii_whitespace() {
        return None;
    }
    let field = |a: usize, b: usize| line.get(a..b).map(str::trim).unwrap_or("");
    let lat = dms(field(34, 47))?;
    let lon = dms(field(48, 61))?;
    let agl: f64 = field(83, 88).parse().ok()?;
    let amsl: f64 = field(89, 94).parse().ok()?;
    if agl < MIN_HEIGHT_FT {
        return None;
    }
    Some(Obstacle {
        lat,
        lon,
        kind: field(62, 80).to_string(),
        agl_ft: Some(agl),
        top_ft: Some(amsl),
        lit: !matches!(field(95, 96), "N" | "U" | ""),
        source: "FAA Digital Obstacle File",
    })
}

/// "30 10 45.00N" and "088 04 39.00W" as they are written in the file.
fn dms(s: &str) -> Option<f64> {
    let s = s.trim();
    let hemisphere = s.chars().last()?;
    let mut parts = s[..s.len() - 1].split_whitespace();
    let deg: f64 = parts.next()?.parse().ok()?;
    let min: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    let value = deg + min / 60.0 + sec / 3600.0;
    match hemisphere {
        'N' | 'E' => Some(value),
        'S' | 'W' => Some(-value),
        _ => None,
    }
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
    fn a_line_of_the_obstacle_file_reads_as_an_obstacle() {
        let line = "01-001307 O US AL DAUPHIN ISLAND   30 10 45.00N 088 04 39.00W RIG                1 00236 00236 R 5 D M 1990ASO01578OE C 2014138 ";
        let o = parse_dof_line(line).expect("a well formed line");
        assert!((o.lat - 30.179_166).abs() < 1e-5, "{}", o.lat);
        assert!((o.lon + 88.077_5).abs() < 1e-5, "{}", o.lon);
        assert_eq!(o.kind, "RIG");
        assert_eq!(o.agl_ft, Some(236.0));
        assert_eq!(o.top_ft, Some(236.0));
    }

    #[test]
    fn the_obstacle_file_is_reissued_every_eight_weeks() {
        use chrono::NaiveDate;
        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(dof_cycle(day(2026, 8, 2)), day(2026, 8, 2));
        assert_eq!(dof_cycle(day(2026, 9, 21)), day(2026, 8, 2));
        assert_eq!(dof_cycle(day(2026, 9, 28)), day(2026, 9, 27));
        assert_eq!(dof_cycle(day(2026, 6, 7)), day(2026, 6, 7));
    }

    #[test]
    fn a_bbox_widens_with_latitude() {
        let (_, w, _, e) = bbox(60.0, 10.0, 10.0);
        let (_, w2, _, e2) = bbox(0.0, 10.0, 10.0);
        assert!(e - w > e2 - w2);
    }
}
