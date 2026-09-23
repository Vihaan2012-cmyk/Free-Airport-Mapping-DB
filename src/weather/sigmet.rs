//! SIGMETs: the international and United States feeds on aviationweather.gov, each
//! already a polygon, a level band and a validity window in JSON rather than the free
//! text a SIGMET is transmitted as — so unlike METAR and TAF there is no grammar to write
//! here, only the mapping from what the feed calls a hazard to what a route search does
//! about it.

use super::common::{default_http, short_lived_cache};
use crate::dispatch::{bearing_deg, travel, Bounds, Hazard, HazardKind, LatLon};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Deserialize)]
struct RawCoord {
    lat: f64,
    lon: f64,
}

/// The international feed (`isigmet`): thunderstorms, volcanic ash, tropical cyclones,
/// severe turbulence and icing, and mountain waves (which this crate does nothing with —
/// it is not one of the hazards asked for).
#[derive(Deserialize)]
struct RawIntl {
    #[serde(rename = "icaoId")]
    icao_id: Option<String>,
    #[serde(rename = "seriesId")]
    series_id: Option<String>,
    hazard: Option<String>,
    base: Option<f64>,
    top: Option<f64>,
    #[serde(rename = "validTimeFrom")]
    valid_from: Option<i64>,
    #[serde(rename = "validTimeTo")]
    valid_to: Option<i64>,
    coords: Option<Vec<RawCoord>>,
}

/// The United States feed (`airsigmet`), which carries AIRMETs alongside SIGMETs under
/// one schema; only entries marked `SIGMET` are used.
#[derive(Deserialize)]
struct RawUs {
    #[serde(rename = "icaoId")]
    icao_id: Option<String>,
    #[serde(rename = "seriesId")]
    series_id: Option<String>,
    hazard: Option<String>,
    #[serde(rename = "airSigmetType")]
    kind: Option<String>,
    #[serde(rename = "altitudeLow1")]
    alt_low: Option<f64>,
    #[serde(rename = "altitudeHi1")]
    alt_high: Option<f64>,
    #[serde(rename = "validTimeFrom")]
    valid_from: Option<i64>,
    #[serde(rename = "validTimeTo")]
    valid_to: Option<i64>,
    coords: Option<Vec<RawCoord>>,
}

/// SIGMETs over an area in force at a time, as hazards.
pub fn sigmets(bounds: Bounds, when: DateTime<Utc>) -> Result<Vec<Hazard>> {
    let http = default_http();
    let cache = short_lived_cache();

    let mut out = Vec::new();
    match cache.get_or_fetch_text("sigmet/isigmet.json", || http.get_text("https://aviationweather.gov/api/data/isigmet?format=json")) {
        Ok(text) => out.extend(parse_intl(&text)),
        Err(e) => log::warn!("international SIGMETs: {e:#}"),
    }
    match cache.get_or_fetch_text("sigmet/airsigmet.json", || http.get_text("https://aviationweather.gov/api/data/airsigmet?format=json")) {
        Ok(text) => out.extend(parse_us(&text)),
        Err(e) => log::warn!("United States SIGMETs: {e:#}"),
    }

    out.retain(|h: &Hazard| h.active_at(when) && overlaps(&bounds, &h.polygon));
    Ok(out)
}

fn kind_for(hazard: &str) -> Option<HazardKind> {
    match hazard {
        "TS" | "CONVECTIVE" | "VA" | "TC" => Some(HazardKind::Avoid),
        "TURB" | "ICE" => Some(HazardKind::Penalise(1.3)),
        _ => None, // mountain wave and anything else this crate takes no action on
    }
}

fn parse_intl(text: &str) -> Vec<Hazard> {
    let raw: Vec<RawIntl> = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("international SIGMET feed: {e:#}");
            return Vec::new();
        }
    };
    raw.into_iter()
        .filter_map(|r| {
            let kind = kind_for(r.hazard.as_deref().unwrap_or(""))?;
            let polygon = shape_from(r.coords?)?;
            let source = format!("{} SIGMET {}", r.icao_id.as_deref().unwrap_or("?"), r.series_id.as_deref().unwrap_or("?"));
            Some(Hazard {
                name: r.hazard.unwrap_or_default(),
                polygon,
                base_ft: r.base.unwrap_or(0.0).max(0.0),
                top_ft: r.top.unwrap_or_else(crate::dispatch::sky),
                kind,
                active_from: r.valid_from.and_then(|t| DateTime::from_timestamp(t, 0)),
                active_to: r.valid_to.and_then(|t| DateTime::from_timestamp(t, 0)),
                source,
            })
        })
        .collect()
}

fn parse_us(text: &str) -> Vec<Hazard> {
    let raw: Vec<RawUs> = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("United States SIGMET feed: {e:#}");
            return Vec::new();
        }
    };
    raw.into_iter()
        .filter(|r| r.kind.as_deref() == Some("SIGMET"))
        .filter_map(|r| {
            let kind = kind_for(r.hazard.as_deref().unwrap_or(""))?;
            let polygon = shape_from(r.coords?)?;
            let source = format!("{} SIGMET {}", r.icao_id.as_deref().unwrap_or("?"), r.series_id.as_deref().unwrap_or("?"));
            Some(Hazard {
                name: r.hazard.unwrap_or_default(),
                polygon,
                base_ft: r.alt_low.unwrap_or(0.0).max(0.0),
                top_ft: r.alt_high.unwrap_or_else(crate::dispatch::sky),
                kind,
                active_from: r.valid_from.and_then(|t| DateTime::from_timestamp(t, 0)),
                active_to: r.valid_to.and_then(|t| DateTime::from_timestamp(t, 0)),
                source,
            })
        })
        .collect()
}

/// The feed's coordinates as a polygon. Almost every SIGMET is already an area of three
/// points or more; the rare line or point one (a tropical cyclone's centre, a line of
/// storms) is given a width instead of being dropped, since a hazard the search does not
/// know about is worse than one drawn a little wider than the forecaster meant.
fn shape_from(coords: Vec<RawCoord>) -> Option<Vec<LatLon>> {
    let points: Vec<LatLon> = coords.iter().map(|c| (c.lat, c.lon)).collect();
    match points.len() {
        0 => None,
        1 => Some(buffer_point(points[0], 30.0)),
        2 => Some(buffer_line(points[0], points[1], 20.0)),
        _ => Some(points),
    }
}

fn buffer_point(p: LatLon, radius_nm: f64) -> Vec<LatLon> {
    (0..8).map(|i| travel(p, i as f64 * 45.0, radius_nm)).collect()
}

/// A corridor either side of a two-point line, as a closed rectangle.
fn buffer_line(a: LatLon, b: LatLon, half_width_nm: f64) -> Vec<LatLon> {
    let brg = bearing_deg(a, b);
    let (left, right) = (brg - 90.0, brg + 90.0);
    vec![travel(a, left, half_width_nm), travel(b, left, half_width_nm), travel(b, right, half_width_nm), travel(a, right, half_width_nm)]
}

/// Whether a hazard's outline has anything to do with an area: a vertex of the polygon
/// inside the box, or a corner of the box inside the polygon's own bounding box. Cheap and
/// biased towards including a hazard rather than missing one, which is the direction it is
/// safe to be wrong in here.
fn overlaps(bounds: &Bounds, polygon: &[LatLon]) -> bool {
    if polygon.iter().any(|p| bounds.contains(*p)) {
        return true;
    }
    let (mut south, mut north, mut west, mut east) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for (lat, lon) in polygon {
        south = south.min(*lat);
        north = north.max(*lat);
        west = west.min(*lon);
        east = east.max(*lon);
    }
    let corners = [(bounds.south, bounds.west), (bounds.south, bounds.east), (bounds.north, bounds.west), (bounds.north, bounds.east)];
    corners.iter().any(|&(lat, lon)| south <= lat && lat <= north && west <= lon && lon <= east)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_isigmet() -> String {
        r#"[
            {"icaoId":"MHTG","seriesId":"2","hazard":"VA","qualifier":"","base":0,"top":14000,
             "validTimeFrom":1790152200,"validTimeTo":1790173800,
             "coords":[{"lon":-91.5,"lat":14.7},{"lon":-91.6,"lat":14.6},{"lon":-91.7,"lat":14.8}]},
            {"icaoId":"ZYSH","seriesId":"5","hazard":"TS","base":null,"top":34000,
             "validTimeFrom":1790155200,"validTimeTo":1790169600,
             "coords":[{"lon":131.1,"lat":43.4},{"lon":131.3,"lat":44.1},{"lon":131.0,"lat":44.8}]},
            {"icaoId":"EGRR","seriesId":"1","hazard":"TURB","qualifier":"SEV","base":14000,"top":38000,
             "validTimeFrom":1790152200,"validTimeTo":1790173800,
             "coords":[{"lon":-10.0,"lat":50.0},{"lon":-9.0,"lat":51.0},{"lon":-8.0,"lat":50.0}]},
            {"icaoId":"EGRR","seriesId":"3","hazard":"MTW","qualifier":"SEV","base":14000,"top":36000,
             "validTimeFrom":1790152200,"validTimeTo":1790173800,
             "coords":[{"lon":-3.0,"lat":57.0},{"lon":-2.0,"lat":58.0},{"lon":-1.0,"lat":57.0}]}
        ]"#
        .to_string()
    }

    #[test]
    fn thunderstorms_and_ash_are_avoided_turbulence_is_penalised_mountain_wave_is_left_out() {
        let hazards = parse_intl(&sample_isigmet());
        assert_eq!(hazards.len(), 3, "the mountain wave entry should be left out");
        let va = hazards.iter().find(|h| h.name == "VA").unwrap();
        assert_eq!(va.kind, HazardKind::Avoid);
        assert_eq!(va.source, "MHTG SIGMET 2");
        let ts = hazards.iter().find(|h| h.name == "TS").unwrap();
        assert_eq!(ts.kind, HazardKind::Avoid);
        assert_eq!(ts.base_ft, 0.0); // a null base reads as the surface
        let turb = hazards.iter().find(|h| h.name == "TURB").unwrap();
        assert_eq!(turb.kind, HazardKind::Penalise(1.3));
        assert_eq!((turb.base_ft, turb.top_ft), (14000.0, 38000.0));
    }

    fn sample_airsigmet() -> String {
        r#"[
            {"icaoId":"KKCI","seriesId":"41W","hazard":"CONVECTIVE","airSigmetType":"SIGMET",
             "altitudeHi1":36000,"altitudeLow1":null,
             "validTimeFrom":1790168100,"validTimeTo":1790175300,
             "coords":[{"lat":48.3,"lon":-103.8},{"lat":46.4,"lon":-102.8},{"lat":45.3,"lon":-109.3}]},
            {"icaoId":"KKCI","seriesId":"1Z","hazard":"IFR","airSigmetType":"AIRMET",
             "altitudeHi1":8000,"altitudeLow1":0,
             "validTimeFrom":1790168100,"validTimeTo":1790175300,
             "coords":[{"lat":40.0,"lon":-90.0},{"lat":41.0,"lon":-91.0},{"lat":39.0,"lon":-92.0}]}
        ]"#
        .to_string()
    }

    #[test]
    fn only_the_sigmet_type_survives_the_us_feed() {
        let hazards = parse_us(&sample_airsigmet());
        assert_eq!(hazards.len(), 1);
        assert_eq!(hazards[0].kind, HazardKind::Avoid);
        assert_eq!(hazards[0].source, "KKCI SIGMET 41W");
    }

    #[test]
    fn a_two_point_line_is_buffered_into_a_corridor() {
        let shape = shape_from(vec![RawCoord { lat: 40.0, lon: -80.0 }, RawCoord { lat: 42.0, lon: -78.0 }]).unwrap();
        assert_eq!(shape.len(), 4);
        // Every corner should be some tens of miles from the line it buffers.
        for p in &shape {
            let d1 = crate::dispatch::distance_nm(*p, (40.0, -80.0));
            let d2 = crate::dispatch::distance_nm(*p, (42.0, -78.0));
            assert!(d1.min(d2) < 40.0, "{p:?} too far from the line");
        }
    }

    #[test]
    fn overlap_is_true_when_either_shape_pokes_into_the_other() {
        let bounds = Bounds { south: 40.0, north: 50.0, west: -10.0, east: 10.0 };
        // Entirely inside.
        assert!(overlaps(&bounds, &[(45.0, 0.0), (46.0, 1.0), (44.0, 1.0)]));
        // The hazard's box swallows the bounds whole, with no vertex inside it.
        assert!(overlaps(&bounds, &[(-80.0, -170.0), (-80.0, 170.0), (80.0, 170.0), (80.0, -170.0)]));
        // Nowhere near.
        assert!(!overlaps(&bounds, &[(60.0, 100.0), (61.0, 101.0), (59.0, 101.0)]));
    }
}
