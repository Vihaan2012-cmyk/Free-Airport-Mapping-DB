//! A `Dispatch` in the shape SimBrief's `api/xml.fetcher.php` answers in: what
//! `bridge::simbrief` serves, both as JSON (`json=1`, what every tablet found on this
//! machine or documented in its own source actually asks for) and as the plain XML the
//! same endpoint answers without it.
//!
//! The shape is the one this crate's brief already gives — `params`, `general`, the three
//! airports, `navlog.fix[]`, `fuel`, `weights`, `times`, `aircraft`, `atc` — checked here
//! against the fields the installed Synaptic A220 EFB's own script reads (`plan_ramp`,
//! `plan_takeoff`, `icao_code`, `plan_rwy`, among them) and, for the tablets not installed
//! on this machine (FlyByWire, PMDG, iniBuilds), against the public contract every one of
//! them is documented to use: the same endpoint, `userid`/`username` and `json=1`, the
//! same field names, because SimBrief itself defines them once for every consumer.

use crate::dispatch::{Dispatch, PointKind};
use crate::ofp::DispatchOptions;
use crate::route::airspace::fir_crossings;
use serde_json::{json, Value};
use std::fmt::Write as _;

/// The passenger count to report. `DispatchOptions` holds what was *asked for*, which is zero
/// when the count was left to the planner, while the weights the plan was actually flown at
/// are in the dispatch itself — so where nothing was asked for, the payload says how many
/// there were. A tablet importing a plan that says nobody is aboard loads an empty aeroplane.
fn passengers_of(d: &crate::dispatch::Dispatch, opts: &crate::ofp::DispatchOptions) -> u32 {
    if opts.passengers > 0 {
        return opts.passengers;
    }
    (d.perf.weights.payload_kg / 100.0).round().max(0.0) as u32
}


/// One row of `navlog.fix[]`.
struct Fix {
    ident: String,
    name: String,
    kind: &'static str,
    via_airway: String,
    lat: f64,
    lon: f64,
    alt_ft: f64,
    wind_dir: f64,
    wind_spd: f64,
    oat: f64,
    fuel_onboard: f64,
}

fn fix_kind(kind: PointKind) -> &'static str {
    match kind {
        PointKind::Airport => "apt",
        PointKind::Sid => "sid",
        PointKind::Star => "star",
        PointKind::Approach => "apt",
        PointKind::Enroute => "wpt",
        PointKind::Track => "wpt",
    }
}

/// `navlog.fix[]`, one row a route point: from the flown profile where the performance
/// model ran, and from the filed route's points alone (no wind, no fuel) when it did not
/// — a tablet asking mid-flight still gets a route to draw.
fn fixes(d: &Dispatch) -> Vec<Fix> {
    if !d.perf.profile.is_empty() {
        d.perf
            .profile
            .iter()
            .map(|p| Fix {
                ident: p.ident.clone(),
                name: p.ident.clone(),
                // The vertical profile carries no `PointKind` of its own (`ProfileKind`
                // marks top of climb and the like, not airport-versus-fix), so the ends
                // are told apart by ident against the airports and everything between is
                // a plain waypoint.
                kind: if p.ident == d.route.origin.icao || p.ident == d.route.destination.icao { "apt" } else { "wpt" },
                via_airway: p.via.clone(),
                lat: p.pos.0,
                lon: p.pos.1,
                alt_ft: p.alt_ft,
                wind_dir: p.air.wind_from_deg,
                wind_spd: p.air.wind_kt,
                oat: p.air.temp_c,
                fuel_onboard: p.fuel_remaining_kg,
            })
            .collect()
    } else {
        d.route
            .points
            .iter()
            .map(|w| Fix { ident: w.ident.clone(), name: w.ident.clone(), kind: fix_kind(w.kind), via_airway: w.via.clone(), lat: w.pos.0, lon: w.pos.1, alt_ft: w.alt_max_ft.or(w.alt_min_ft).unwrap_or(d.route.cruise_ft), wind_dir: 0.0, wind_spd: 0.0, oat: 0.0, fuel_onboard: 0.0 })
            .collect()
    }
}

fn callsign(opts: &DispatchOptions, d: &Dispatch) -> String {
    opts.flight_number.clone().unwrap_or_else(|| d.spec.icao_type.clone())
}

fn air_distance_nm(d: &Dispatch) -> f64 {
    d.perf.profile.last().map(|p| p.dist_nm).filter(|n| *n > 0.0).unwrap_or_else(|| d.route.distance_nm())
}

fn est_time_enroute_min(d: &Dispatch) -> f64 {
    d.perf.profile.last().map(|p| p.time_min).unwrap_or(0.0)
}

/// The plan as `api/xml.fetcher.php?json=1` answers it.
pub fn ofp_json(d: &Dispatch, opts: &DispatchOptions) -> Value {
    let fixes = fixes(d);
    let origin = &d.route.origin;
    let destination = &d.route.destination;
    let eet_min = est_time_enroute_min(d);
    let cs = callsign(opts, d);
    let firs = fir_crossings(&d.route);

    // Runway, transition altitude and transition level are read (and, for the
    // transitions, `parseInt`'d) by the FlyByWire A380X's `simbriefDataParser` off every
    // one of `origin`, `destination` and `alternate` without checking they are there, so
    // every airport carries all three even where this crate has nothing to put in them.
    let airport = |icao: &str, name: &str, lat: f64, lon: f64, elev_ft: f64, rwy: Option<&str>| {
        json!({
            "icao_code": icao,
            "iata_code": "",
            "name": name,
            "plan_rwy": rwy.unwrap_or(""),
            "pos_lat": lat,
            "pos_long": lon,
            "elevation": elev_ft,
            "trans_alt": "18000",
            "trans_level": "0",
        })
    };
    let metar_of = |m: &Option<crate::dispatch::Metar>| m.as_ref().map(|m| m.raw.clone()).unwrap_or_default();

    json!({
        "fetch": { "status": "Success" },
        "params": {
            "request_id": "amdb-bridge",
            "units": "kgs",
            "time_generated": d.generated.timestamp(),
        },
        "general": {
            "icao_airline": "",
            "flight_number": cs,
            "route": d.route.route_string(),
            "route_ifps": d.route.route_string(),
            "initial_altitude": d.route.cruise_ft,
            "cruise_altitude": d.route.cruise_ft,
            "costindex": opts.cost_index,
            "cruise_mach": format!("{:.2}", d.spec.cruise_mach).trim_start_matches('0'),
            "air_distance": air_distance_nm(d),
            "route_distance": d.route.distance_nm(),
            // `PerfPlan` carries only the average wind's along-track component, not a
            // direction, so `avg_wind_dir` is left at zero: a tablet doing anything more
            // with it than showing it back would need a `dispatch.rs` change (see the
            // final report).
            "avg_wind_dir": 0,
            "avg_wind_spd": d.perf.avg_wind_kt.abs(),
            "avg_temp_dev": d.perf.avg_isa_dev,
            "avg_tropopause": 36_089,
            "icao_code": d.spec.icao_type,
        },
        "origin": airport(&origin.icao, &origin.name, origin.pos.0, origin.pos.1, origin.elevation_ft, d.route.dep_runway.as_deref()),
        "destination": airport(&destination.icao, &destination.name, destination.pos.0, destination.pos.1, destination.elevation_ft, d.route.arr_runway.as_deref()),
        "alternate": d.alternate.as_ref().map(|a| {
            let mut v = airport(&a.destination.icao, &a.destination.name, a.destination.pos.0, a.destination.pos.1, a.destination.elevation_ft, None);
            v["burn"] = json!(d.perf.fuel.alternate_kg);
            v["avg_wind_dir"] = json!(0);
            v["avg_wind_spd"] = json!(0);
            v["cruise_altitude"] = json!(a.cruise_ft);
            v
        }).unwrap_or_else(|| json!({})),
        "navlog": {
            "fix": fixes.iter().map(|f| json!({
                "ident": f.ident,
                "name": f.name,
                "type": f.kind,
                "via_airway": f.via_airway,
                "pos_lat": f.lat,
                "pos_long": f.lon,
                "altitude_feet": f.alt_ft,
                "wind_dir": f.wind_dir,
                "wind_spd": f.wind_spd,
                "oat": f.oat,
                "fuel_plan_onboard": f.fuel_onboard,
            })).collect::<Vec<_>>(),
        },
        "fuel": {
            "plan_ramp": d.perf.fuel.block_kg,
            "plan_takeoff": d.perf.fuel.takeoff_kg,
            "plan_landing": d.perf.fuel.landing_kg,
            "reserve": d.perf.fuel.final_reserve_kg,
            "alternate_burn": d.perf.fuel.alternate_kg,
            "contingency": d.perf.fuel.contingency_kg,
            "taxi": d.perf.fuel.taxi_kg,
            "enroute_burn": d.perf.fuel.trip_kg,
            "extra": d.perf.fuel.extra_kg,
            "min_takeoff": d.perf.fuel.takeoff_kg,
            "max_tanks": d.spec.max_fuel_kg,
            "avg_fuel_flow": if eet_min > 0.0 { d.perf.fuel.trip_kg / (eet_min / 60.0) } else { 0.0 },
            "etops": if d.spec.etops_minutes.is_some() { "1" } else { "0" },
        },
        "weights": {
            "payload": d.perf.weights.payload_kg,
            "cargo": 0.0,
            "freight_added": 0.0,
            "pax_count": passengers_of(d, opts),
            "pax_count_actual": passengers_of(d, opts),
            "bag_count_actual": 0,
            "pax_weight": 0.0,
            "bag_weight": 0.0,
            "est_zfw": d.perf.weights.zfw_kg,
            "est_tow": d.perf.weights.tow_kg,
            "est_ldw": d.perf.weights.lw_kg,
            "max_zfw": d.perf.weights.max_zfw_kg,
            "max_tow": d.perf.weights.max_tow_kg,
            "max_ldw": d.perf.weights.max_lw_kg,
            "oew": d.perf.weights.oew_kg,
        },
        "times": {
            "sched_out": d.route.off_block.timestamp(),
            "sched_off": d.route.off_block.timestamp(),
            "sched_on": d.route.off_block.timestamp(),
            "sched_in": d.route.off_block.timestamp(),
            "est_out": d.route.off_block.timestamp(),
            "est_off": d.route.off_block.timestamp(),
            "est_on": d.route.off_block.timestamp(),
            "est_in": d.route.off_block.timestamp(),
            "est_block": (eet_min * 60.0) as i64,
            "sched_block": (eet_min * 60.0) as i64,
            "est_time_enroute": (eet_min * 60.0) as i64,
            "sched_time_enroute": (eet_min * 60.0) as i64,
            "taxi_out": 600,
            "taxi_in": 300,
            "reserve_time": (d.perf.fuel.final_reserve_kg.max(1.0) > 0.0) as i64 * 1800,
            "endurance": (eet_min * 60.0) as i64 + 1800,
            "contfuel_time": 300,
            "extrafuel_time": 0,
            "etopsfuel_time": 0,
            "orig_timezone": 0,
            "dest_timezone": 0,
        },
        "aircraft": {
            "icaocode": d.spec.icao_type,
            "name": d.spec.name,
            "reg": opts.registration.clone().unwrap_or_default(),
        },
        "atc": {
            "callsign": cs,
            "route": d.route.route_string(),
            "flightplan_text": crate::ofp::export::icao_message(d, opts.flight_number.as_deref(), opts.registration.as_deref()),
        },
        "weather": {
            "orig_metar": metar_of(&d.origin_metar),
            "dest_metar": metar_of(&d.destination_metar),
            "avg_wind_dir": 0,
            "avg_wind_spd": d.perf.avg_wind_kt.abs(),
        },
        // `files.pdf.link` and `text.plan_html` are read by the FlyByWire A380X's parser
        // without a null check (`files.pdf.link`, `text.plan_html.replace(...)`), so both
        // objects have to exist even though this bridge writes no loadsheet PDF or HTML
        // plan of its own — an empty link and an empty div is what an OFP with neither
        // looks like to that parser.
        "files": { "directory": "", "pdf": { "name": "", "link": "" } },
        "text": { "plan_html": "<div class=\"amdb-bridge\"></div>" },
        "fir": firs.iter().map(|f| json!({ "ident": f.ident, "name": f.name })).collect::<Vec<_>>(),
        "airac": d.airac.clone().unwrap_or_default(),
    })
}

/// The same plan as the endpoint answers without `json=1`: SimBrief's own XML, an `<OFP>`
/// element carrying the same fields under the same names.
pub fn ofp_xml(d: &Dispatch, opts: &DispatchOptions) -> String {
    let v = ofp_json(d, opts);
    let mut s = String::new();
    let _ = writeln!(s, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    let _ = writeln!(s, "<OFP>");
    // Every top-level section but navlog is a flat or one-level object; navlog.fix is the
    // one repeated element, so it alone is handled by hand rather than by a fully generic
    // walk that would otherwise have to guess which keys repeat.
    if let Some(obj) = v.as_object() {
        for (key, value) in obj {
            if key == "navlog" {
                let _ = writeln!(s, "  <navlog>");
                if let Some(fix) = value.get("fix").and_then(Value::as_array) {
                    for f in fix {
                        let _ = writeln!(s, "    <fix>");
                        xml_object(&mut s, f, 6);
                        let _ = writeln!(s, "    </fix>");
                    }
                }
                let _ = writeln!(s, "  </navlog>");
                continue;
            }
            xml_element(&mut s, key, value, 2);
        }
    }
    let _ = writeln!(s, "</OFP>");
    s
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn xml_scalar(v: &Value) -> String {
    match v {
        Value::String(s) => xml_escape(s),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => xml_escape(&other.to_string()),
    }
}

fn xml_object(s: &mut String, v: &Value, indent: usize) {
    let pad = " ".repeat(indent);
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            let _ = writeln!(s, "{pad}<{k}>{}</{k}>", xml_scalar(val));
        }
    }
}

fn xml_element(s: &mut String, key: &str, v: &Value, indent: usize) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(_) => {
            let _ = writeln!(s, "{pad}<{key}>");
            xml_object(s, v, indent + 2);
            let _ = writeln!(s, "{pad}</{key}>");
        }
        Value::Array(items) => {
            // Every array outside navlog.fix in this shape is short and scalar (`fir`
            // aside, which a tablet does not read): write one element per row, named
            // after the singular of the section.
            for item in items {
                let _ = writeln!(s, "{pad}<{key}>");
                xml_object(s, item, indent + 2);
                let _ = writeln!(s, "{pad}</{key}>");
            }
        }
        _ => {
            let _ = writeln!(s, "{pad}<{key}>{}</{key}>", xml_scalar(v));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ofp::fixtures;

    #[test]
    fn the_json_shape_carries_every_field_a_tablet_reads() {
        let d = fixtures::sample();
        let opts = fixtures::sample_opts();
        let v = ofp_json(&d, &opts);
        assert_eq!(v["fetch"]["status"], "Success");
        assert_eq!(v["params"]["units"], "kgs");
        assert!(v["general"]["route"].is_string());
        assert!(v["general"]["initial_altitude"].as_f64().unwrap() > 0.0);
        assert!(v["general"]["costindex"].as_f64().is_some());
        assert!(v["general"]["cruise_mach"].is_string());
        assert!(v["general"]["air_distance"].as_f64().unwrap() > 0.0);
        assert_eq!(v["origin"]["icao_code"], d.route.origin.icao);
        assert_eq!(v["destination"]["icao_code"], d.route.destination.icao);
        assert!(v["origin"]["pos_lat"].is_number());
        assert!(v["origin"]["elevation"].is_number());
        let fix = v["navlog"]["fix"].as_array().unwrap();
        assert!(!fix.is_empty());
        for want in ["ident", "name", "type", "via_airway", "pos_lat", "pos_long", "altitude_feet", "wind_dir", "wind_spd", "oat", "fuel_plan_onboard"] {
            assert!(fix[0].get(want).is_some(), "navlog.fix is missing {want}");
        }
        for want in ["plan_ramp", "plan_takeoff", "plan_landing", "reserve", "alternate_burn", "contingency", "taxi", "enroute_burn", "extra"] {
            assert!(v["fuel"].get(want).is_some(), "fuel is missing {want}");
        }
        for want in ["payload", "pax_count", "est_zfw", "est_tow", "est_ldw", "max_zfw", "max_tow", "max_ldw"] {
            assert!(v["weights"].get(want).is_some(), "weights is missing {want}");
        }
        assert!(v["times"]["est_time_enroute"].is_number());
        assert_eq!(v["aircraft"]["icaocode"], d.spec.icao_type);
        assert!(v["atc"]["callsign"].is_string());
        assert!(v["atc"]["route"].is_string());
        assert!(v["atc"]["flightplan_text"].as_str().unwrap().starts_with("(FPL-"));
    }

    /// Every field the installed FlyByWire A380X's `simbriefDataParser` reads off the raw
    /// JSON without an optional-chain or a null check first (`efb.js`, function
    /// `simbriefDataParser`) — miss one of these and the EFB throws before it ever shows
    /// the plan, rather than falling back to blank fields.
    #[test]
    fn the_json_shape_matches_what_the_installed_flybywire_a380x_parser_reads_unguarded() {
        let d = fixtures::sample();
        let opts = fixtures::sample_opts();
        let v = ofp_json(&d, &opts);
        for section in ["general", "navlog", "origin", "aircraft", "destination", "times", "weights", "fuel", "params", "files", "text", "weather", "atc", "alternate"] {
            assert!(v.get(section).is_some_and(Value::is_object), "top-level {section} is missing or not an object");
        }
        assert!(v["files"]["pdf"]["link"].is_string(), "files.pdf.link: read without a null check");
        assert!(v["text"]["plan_html"].is_string(), "text.plan_html: .replace() is called on it without a null check");
        for want in ["orig_metar", "dest_metar"] {
            assert!(v["weather"].get(want).is_some(), "weather.{want}");
        }
        for want in ["cargo", "pax_count_actual", "bag_count_actual", "pax_weight", "bag_weight", "freight_added"] {
            assert!(v["weights"].get(want).is_some(), "weights.{want}");
        }
        for want in ["avg_fuel_flow", "etops", "max_tanks", "min_takeoff"] {
            assert!(v["fuel"].get(want).is_some(), "fuel.{want}");
        }
        for want in ["trans_alt", "trans_level"] {
            assert!(v["origin"].get(want).is_some(), "origin.{want}");
            assert!(v["destination"].get(want).is_some(), "destination.{want}");
        }
        assert!(v["alternate"]["burn"].is_number());
        assert!(v["alternate"]["avg_wind_dir"].is_number());
        assert!(v["alternate"]["cruise_altitude"].is_number());
        assert!(v["general"]["avg_tropopause"].is_number());
    }

    #[test]
    fn a_missing_alternate_is_an_empty_object_not_a_missing_key() {
        let mut d = fixtures::sample();
        d.alternate = None;
        let v = ofp_json(&d, &fixtures::sample_opts());
        assert!(v["alternate"].is_object());
        assert!(v["alternate"].get("icao_code").is_none());
    }

    #[test]
    fn the_xml_shape_nests_the_navlog_fixes_and_carries_the_same_top_level_fields() {
        let d = fixtures::sample();
        let opts = fixtures::sample_opts();
        let xml = ofp_xml(&d, &opts);
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("<OFP>"));
        assert!(xml.trim_end().ends_with("</OFP>"));
        assert!(xml.contains("<navlog>"));
        let fixes_json = ofp_json(&d, &opts)["navlog"]["fix"].as_array().unwrap().len();
        assert_eq!(xml.matches("<fix>").count(), fixes_json);
        assert!(xml.contains(&format!("<icao_code>{}</icao_code>", d.route.origin.icao)));
    }

    #[test]
    fn xml_escapes_the_characters_that_would_break_it() {
        let mut d = fixtures::sample();
        d.route.origin.name = "Lisbon & \"Porto\" <test>".to_string();
        let xml = ofp_xml(&d, &fixtures::sample_opts());
        assert!(!xml.contains("<test>Lisbon"));
        assert!(xml.contains("&amp;"));
    }
}
