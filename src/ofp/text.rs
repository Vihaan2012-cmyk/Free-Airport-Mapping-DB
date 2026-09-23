//! The operational flight plan, printed the way an airline's dispatch system prints one:
//! a header, the fuel, the weights against their limits, the route as ATC would read it
//! and as item 15 of a flight plan, the navigation log, the step climbs, the equal-time
//! points, the point of no return, the FIR crossings, the reports and forecasts as
//! issued, the hazards planned round, the violations, and what was left to a warning.
//!
//! `pdf::write` typesets exactly this text, monospaced, across as many A4 pages as it
//! takes: there is one source of the plan's content, and only one place it is laid out.

use crate::dispatch::{Dispatch, PointKind, TafChange, Waypoint};
use crate::ofp::DispatchOptions;
use crate::route::airspace::fir_crossings;
use chrono::{DateTime, Utc};
use std::fmt::Write as _;

fn hdg(deg: f64) -> String {
    format!("{:03.0}", deg.rem_euclid(360.0))
}

fn hm(dt: DateTime<Utc>) -> String {
    dt.format("%d%H%M").to_string()
}

fn fmt_hm(minutes: f64) -> String {
    let total = minutes.round().max(0.0) as i64;
    format!("{:02}{:02}", total / 60, total % 60)
}

/// The whole plan, as one string, ready to print or lay out on a PDF page a line at a
/// time.
pub fn render(d: &Dispatch, opts: &DispatchOptions) -> String {
    let mut s = String::new();
    header(&mut s, d, opts);
    fuel_and_weights(&mut s, d);
    route_lines(&mut s, d);
    nav_log(&mut s, d);
    step_climbs(&mut s, d);
    equal_time_points(&mut s, d);
    point_of_no_return(&mut s, d);
    fir_section(&mut s, d, &fir_crossings(&d.route));
    reports(&mut s, d);
    hazards_and_rules(&mut s, d);
    violations(&mut s, d);
    notes(&mut s, d);
    s
}

fn header(s: &mut String, d: &Dispatch, opts: &DispatchOptions) {
    let flight = opts.flight_number.clone().unwrap_or_else(|| "----".to_string());
    let reg = opts.registration.clone().unwrap_or_default();
    let _ = writeln!(s, "OPERATIONAL FLIGHT PLAN");
    let _ = writeln!(s, "========================================================================");
    let _ = writeln!(s, "FLIGHT {flight}   {} -> {}   {}", d.route.origin.icao, d.route.destination.icao, d.generated.format("%Y-%m-%d %H:%MZ"));
    let _ = writeln!(s, "AIRCRAFT {} ({})  REG {reg}   ENGINES {} x {}", d.spec.icao_type, d.spec.name, d.spec.engines, d.spec.engine);
    let _ = writeln!(s, "AIRAC {}", d.airac.clone().unwrap_or_else(|| "unknown".to_string()));
    let _ = writeln!(s, "OFF-BLOCK {}Z   COST INDEX {:.0}   CRUISE FL{:03.0}", hm(d.route.off_block), opts.cost_index, d.route.cruise_ft / 100.0);
    let _ = writeln!(s);
}

fn fuel_and_weights(s: &mut String, d: &Dispatch) {
    let f = &d.perf.fuel;
    let _ = writeln!(s, "FUEL (KG)");
    let _ = writeln!(s, "  TAXI {:.0}  TRIP {:.0}  CONTINGENCY {:.0}  ALTN {:.0}", f.taxi_kg, f.trip_kg, f.contingency_kg, f.alternate_kg);
    let _ = writeln!(s, "  FINAL RESERVE {:.0}  EXTRA {:.0}  TANKER {:.0}", f.final_reserve_kg, f.extra_kg, f.tanker_kg);
    let _ = writeln!(s, "  TAKEOFF FUEL {:.0}   BLOCK FUEL {:.0}   LANDING FUEL {:.0}", f.takeoff_kg, f.block_kg, f.landing_kg);
    let _ = writeln!(s);
    let w = &d.perf.weights;
    let _ = writeln!(s, "WEIGHTS (KG)");
    let _ = writeln!(s, "  OEW {:.0}  PAYLOAD {:.0}", w.oew_kg, w.payload_kg);
    let _ = writeln!(s, "  ZFW {:.0} / {:.0} MAX{}", w.zfw_kg, w.max_zfw_kg, if w.zfw_kg > w.max_zfw_kg { "  *** OVER LIMIT ***" } else { "" });
    let _ = writeln!(s, "  TOW {:.0} / {:.0} MAX{}", w.tow_kg, w.max_tow_kg, if w.tow_kg > w.max_tow_kg { "  *** OVER LIMIT ***" } else { "" });
    let _ = writeln!(s, "  LW  {:.0} / {:.0} MAX{}", w.lw_kg, w.max_lw_kg, if w.lw_kg > w.max_lw_kg { "  *** OVER LIMIT ***" } else { "" });
    if let Some(by) = &w.limited_by {
        let _ = writeln!(s, "  PAYLOAD LIMITED BY {by}");
    }
    let _ = writeln!(s);
}

fn route_lines(s: &mut String, d: &Dispatch) {
    let _ = writeln!(s, "ROUTE");
    let _ = writeln!(s, "  ATC   {}", atc_route(d));
    let _ = writeln!(s, "  ITEM15 {}", d.route.route_string());
    if let Some(alt) = &d.alternate {
        let _ = writeln!(s, "  ALTERNATE {}", alt.destination.icao);
    }
    let _ = writeln!(s);
}

/// The way ATC reads the route back: the departure airport and the runway in use, the SID
/// where one was flown, every enroute fix at its level, the STAR, then the arrival airport
/// and its runway — the shape a filed route is read back in, not merely item 15's shorter
/// notation.
fn atc_route(d: &Dispatch) -> String {
    let mut parts = vec![airport_and_runway(&d.route.origin.icao, d.route.dep_runway.as_deref())];
    if let Some(sid) = &d.route.sid {
        parts.push(sid.clone());
    }
    for w in d.route.points.iter().filter(|w| matches!(w.kind, PointKind::Enroute | PointKind::Track)) {
        parts.push(w.ident.clone());
    }
    if let Some(star) = &d.route.star {
        parts.push(star.clone());
    }
    parts.push(airport_and_runway(&d.route.destination.icao, d.route.arr_runway.as_deref()));
    parts.join(" ")
}

/// An airport with the runway in use, the way a chart or a filed route names it: "EGLL/27R",
/// or just the airport where no runway was settled on.
fn airport_and_runway(icao: &str, runway: Option<&str>) -> String {
    match runway {
        Some(rw) if !rw.is_empty() => format!("{icao}/{rw}"),
        _ => icao.to_string(),
    }
}

fn nav_log(s: &mut String, d: &Dispatch) {
    let _ = writeln!(s, "NAVIGATION LOG");
    let _ = writeln!(s, "  FIX      VIA      TRK  LVL   WIND      OAT ISADEV  TAS  GS   LEGNM CUMNM  TIME CUMTIME FUELUSED FUELREM MORA");
    if d.perf.profile.is_empty() {
        let _ = writeln!(s, "  (no navigation log: the performance model did not run)");
    }
    // The performance profile carries none of a SID or STAR's own published constraints —
    // it is built to fly the route, not to say what was filed — so they are looked up here,
    // by the identifier a procedure fix and its profile point share, from the route itself.
    let constraints = procedure_constraints(d);
    let mut prev_dist = 0.0;
    let mut prev_time = 0.0;
    for p in &d.perf.profile {
        let leg_nm = p.dist_nm - prev_dist;
        let leg_time = p.time_min - prev_time;
        prev_dist = p.dist_nm;
        prev_time = p.time_min;
        let isa_dev = p.air.isa_dev(p.alt_ft);
        let cons = constraints.get(p.ident.as_str()).map(|w| constraint_note(w)).unwrap_or_default();
        let _ = writeln!(
            s,
            "  {:<8} {:<8} {} FL{:03.0} {:03.0}/{:02.0}KT {:+4.0} {:+3.0}  {:3.0} {:3.0} {:5.0} {:5.0} {} {}   {:6.0}  {:6.0}  {}{cons}",
            p.ident,
            p.via,
            hdg(p.track_true_deg),
            p.alt_ft / 100.0,
            p.air.wind_from_deg,
            p.air.wind_kt,
            p.air.temp_c,
            isa_dev,
            p.tas_kt,
            p.gs_kt,
            leg_nm,
            p.dist_nm,
            fmt_hm(leg_time),
            fmt_hm(p.time_min),
            p.fuel_used_kg,
            p.fuel_remaining_kg,
            p.mora_ft.map(|m| format!("{m:.0}")).unwrap_or_else(|| "-".to_string()),
        );
    }
    let _ = writeln!(s);
}

/// Every SID or STAR fix that carries a published constraint, by identifier: what
/// [`nav_log`] annotates each matching profile line with.
fn procedure_constraints(d: &Dispatch) -> std::collections::HashMap<&str, &Waypoint> {
    d.route
        .points
        .iter()
        .filter(|w| matches!(w.kind, PointKind::Sid | PointKind::Star))
        .filter(|w| w.alt_min_ft.is_some() || w.alt_max_ft.is_some() || w.speed_max_kt.is_some())
        .map(|w| (w.ident.as_str(), w))
        .collect()
}

/// A published constraint the way a chart prints it against a procedure fix: "AT OR ABOVE
/// 4000FT", "AT OR BELOW FL100", "4000-6000FT", "250KT MAX" — whichever of altitude and
/// speed the database actually gave.
fn constraint_note(w: &Waypoint) -> String {
    let mut parts = Vec::new();
    match (w.alt_min_ft, w.alt_max_ft) {
        (Some(min), Some(max)) if (min - max).abs() < 1.0 => parts.push(format!("{min:.0}FT")),
        (Some(min), Some(max)) => parts.push(format!("{min:.0}-{max:.0}FT")),
        (Some(min), None) => parts.push(format!("{min:.0}FT+")),
        (None, Some(max)) => parts.push(format!("{max:.0}FT-")),
        (None, None) => {}
    }
    if let Some(kt) = w.speed_max_kt {
        parts.push(format!("{kt:.0}KT MAX"));
    }
    if parts.is_empty() { String::new() } else { format!("  [{}]", parts.join(" ")) }
}

fn step_climbs(s: &mut String, d: &Dispatch) {
    if d.perf.step_climbs.is_empty() {
        return;
    }
    let _ = writeln!(s, "STEP CLIMBS");
    for (at, level) in &d.perf.step_climbs {
        let _ = writeln!(s, "  AT {at} CLIMB TO FL{:03.0}", level / 100.0);
    }
    let _ = writeln!(s);
}

fn equal_time_points(s: &mut String, d: &Dispatch) {
    if d.perf.equal_time_points.is_empty() {
        return;
    }
    let label = if d.spec.etops_minutes.is_some() { "ETOPS EQUAL-TIME POINTS" } else { "EQUAL-TIME POINTS" };
    let _ = writeln!(s, "{label}");
    for etp in &d.perf.equal_time_points {
        let _ = writeln!(s, "  BETWEEN {} AND {}  AT {:.0} NM / {}  FUEL NEEDED {:.0} KG", etp.between.0, etp.between.1, etp.dist_nm, fmt_hm(etp.time_min), etp.fuel_needed_kg);
    }
    let _ = writeln!(s);
}

fn point_of_no_return(s: &mut String, d: &Dispatch) {
    let Some(nr) = &d.perf.no_return else { return };
    let _ = writeln!(s, "POINT OF NO RETURN");
    let _ = writeln!(s, "  {}  {:.0} NM / {}  FUEL REMAINING {:.0} KG", nr.ident, nr.dist_nm, fmt_hm(nr.time_min), nr.fuel_remaining_kg);
    let _ = writeln!(s);
}

fn fir_section(s: &mut String, d: &Dispatch, crossings: &[crate::route::airspace::FirCrossing]) {
    if crossings.is_empty() {
        return;
    }
    let _ = writeln!(s, "FIR CROSSINGS");
    let total_min = d.perf.profile.last().map(|p| p.time_min).unwrap_or(0.0);
    let total_nm = d.route.distance_nm().max(1.0);
    for c in crossings {
        // Time at the entry point, interpolated from the total time on the share of the
        // route flown by then: exact once the performance model has run, a fair estimate
        // when the fallback's straight-line one has.
        let entry_min = if total_min > 0.0 { total_min * (c.entry_nm / total_nm) } else { 0.0 };
        let at = d.route.off_block + chrono::Duration::seconds((entry_min * 60.0).round() as i64);
        let _ = writeln!(s, "  {} {}  ENTRY {:.0} NM ({}Z)  EXIT {:.0} NM", c.ident, c.name, c.entry_nm, hm(at), c.exit_nm);
    }
    let _ = writeln!(s);
}

fn report_line(s: &mut String, label: &str, raw: &str) {
    if raw.is_empty() {
        let _ = writeln!(s, "  {label} not available");
    } else {
        let _ = writeln!(s, "  {label} {raw}");
    }
}

fn taf_period_label(c: TafChange) -> &'static str {
    match c {
        TafChange::Base => "BASE",
        TafChange::From => "FM",
        TafChange::Becoming => "BECMG",
        TafChange::Tempo => "TEMPO",
        TafChange::Prob(_) => "PROB",
        TafChange::ProbTempo(_) => "PROB TEMPO",
    }
}

fn reports(s: &mut String, d: &Dispatch) {
    let _ = writeln!(s, "WEATHER");
    report_line(s, &format!("{} METAR", d.route.origin.icao), d.origin_metar.as_ref().map(|m| m.raw.as_str()).unwrap_or_default());
    report_line(s, &format!("{} METAR", d.route.destination.icao), d.destination_metar.as_ref().map(|m| m.raw.as_str()).unwrap_or_default());
    if let Some(alt) = &d.alternate {
        report_line(s, &format!("{} TAF", alt.destination.icao), d.alternate_taf.as_ref().map(|t| t.raw.as_str()).unwrap_or_default());
    }
    for (label, taf) in [(d.route.origin.icao.as_str(), &d.origin_taf), (d.route.destination.icao.as_str(), &d.destination_taf)] {
        if let Some(t) = taf {
            let _ = writeln!(s, "  {label} TAF {}", t.raw);
            for p in &t.periods {
                let _ = writeln!(s, "    {} {}-{}", taf_period_label(p.change), p.from.format("%d%H%MZ"), p.to.format("%d%H%MZ"));
            }
        }
    }
    let _ = writeln!(s, "  AVERAGE WIND {:+.0} KT   AVERAGE ISA DEVIATION {:+.0}", d.perf.avg_wind_kt, d.perf.avg_isa_dev);
    let _ = writeln!(s);
}

fn hazards_and_rules(s: &mut String, d: &Dispatch) {
    let _ = writeln!(s, "HAZARDS PLANNED ROUND");
    if d.hazards.is_empty() {
        let _ = writeln!(s, "  none reported");
    }
    for h in &d.hazards {
        let _ = writeln!(s, "  {h}");
    }
    let _ = writeln!(s);
}

fn violations(s: &mut String, d: &Dispatch) {
    if d.violations.is_empty() {
        return;
    }
    let _ = writeln!(s, "VIOLATIONS");
    for v in &d.violations {
        let _ = writeln!(s, "  {} {}: {}", v.rule, v.at.clone().unwrap_or_default(), v.message);
    }
    let _ = writeln!(s);
}

fn notes(s: &mut String, d: &Dispatch) {
    if d.perf.warnings.is_empty() {
        return;
    }
    let _ = writeln!(s, "NOTES");
    for w in &d.perf.warnings {
        let _ = writeln!(s, "  - {w}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ofp::fixtures;

    #[test]
    fn the_plan_carries_every_section_the_fixture_has_data_for() {
        let d = fixtures::sample();
        let opts = fixtures::sample_opts();
        let text = render(&d, &opts);
        assert!(text.contains("OPERATIONAL FLIGHT PLAN"));
        assert!(text.contains("FUEL (KG)"));
        assert!(text.contains("WEIGHTS (KG)"));
        assert!(text.contains("ROUTE"));
        assert!(text.contains(&d.route.origin.icao));
        assert!(text.contains(&d.route.destination.icao));
        assert!(text.contains("NAVIGATION LOG"));
        // `route::airspace::fir_crossings` is still a stub that answers nothing, so the
        // section is correctly left out here; `fir_crossings_are_printed_with_their_time`
        // below checks its formatting directly, without depending on the stub.
        assert!(text.contains("WEATHER"));
        assert!(text.contains("HAZARDS PLANNED ROUND"));
        assert!(text.contains("NOTES"));
        // The fake's RAD failure and performance fallback both end up as notes.
        assert!(text.contains("RAD"));
    }

    #[test]
    fn an_over_limit_weight_is_flagged() {
        let mut d = fixtures::sample();
        d.perf.weights.tow_kg = d.perf.weights.max_tow_kg + 500.0;
        let text = render(&d, &fixtures::sample_opts());
        assert!(text.contains("OVER LIMIT"));
    }

    #[test]
    fn fir_crossings_are_printed_with_their_time() {
        use crate::route::airspace::FirCrossing;
        let d = fixtures::sample();
        let crossings = vec![FirCrossing { ident: "LPPC".to_string(), name: "Lisbon FIR".to_string(), entry: d.route.origin.pos, entry_nm: 0.0, exit_nm: d.route.distance_nm() }];
        let mut s = String::new();
        fir_section(&mut s, &d, &crossings);
        assert!(s.contains("FIR CROSSINGS"));
        assert!(s.contains("LPPC"));
        assert!(s.contains("Lisbon FIR"));
        let mut empty = String::new();
        fir_section(&mut empty, &d, &[]);
        assert!(empty.is_empty());
    }

    #[test]
    fn headings_wrap_into_three_digits() {
        assert_eq!(hdg(5.0), "005");
        assert_eq!(hdg(-10.0), "350");
        assert_eq!(hdg(361.0), "001");
    }

    #[test]
    fn minutes_print_as_hours_and_minutes() {
        assert_eq!(fmt_hm(90.0), "0130");
        assert_eq!(fmt_hm(5.0), "0005");
    }

    #[test]
    fn the_atc_route_names_the_runway_and_the_procedure_at_each_end() {
        let mut d = fixtures::sample();
        d.route.dep_runway = Some("27R".to_string());
        d.route.arr_runway = Some("16L".to_string());
        d.route.sid = Some("DVR3J".to_string());
        d.route.star = Some("RITE3B".to_string());
        let line = atc_route(&d);
        assert!(line.starts_with(&format!("{}/27R DVR3J ", d.route.origin.icao)), "{line}");
        assert!(line.ends_with(&format!(" RITE3B {}/16L", d.route.destination.icao)), "{line}");
    }

    /// Item 15's route string is `FiledRoute::route_string`'s own job (it is defined in
    /// `dispatch.rs`, frozen); what belongs here is only that the ATC line never names a
    /// procedure that was not flown.
    #[test]
    fn the_atc_route_names_no_procedure_where_there_is_none() {
        let mut d = fixtures::sample();
        d.route.dep_runway = None;
        d.route.sid = None;
        d.route.star = None;
        let line = atc_route(&d);
        assert!(!line.contains('/'));
        assert!(line.starts_with(&format!("{} ", d.route.origin.icao)));
    }

    #[test]
    fn constraint_note_formats_each_shape_of_constraint() {
        use crate::dispatch::{PointKind, Waypoint};
        let mut w = Waypoint::new("X", (0.0, 0.0), "", PointKind::Sid);
        w.alt_min_ft = Some(3000.0);
        assert_eq!(constraint_note(&w), "  [3000FT+]");
        w.alt_min_ft = None;
        w.alt_max_ft = Some(6000.0);
        assert_eq!(constraint_note(&w), "  [6000FT-]");
        w.alt_min_ft = Some(4000.0);
        w.alt_max_ft = Some(4000.0);
        assert_eq!(constraint_note(&w), "  [4000FT]");
        w.alt_min_ft = Some(3000.0);
        w.alt_max_ft = Some(6000.0);
        w.speed_max_kt = Some(250.0);
        assert_eq!(constraint_note(&w), "  [3000-6000FT 250KT MAX]");
    }

    #[test]
    fn nav_log_prints_a_procedures_published_constraint_at_its_fix() {
        use crate::dispatch::{Air, PointKind, ProfileKind, ProfilePoint, Waypoint};
        let mut d = fixtures::sample();
        let mut w = Waypoint::new("BPK", (51.75, -0.11), "BPK5K", PointKind::Sid);
        w.alt_min_ft = Some(4000.0);
        d.route.points.push(w);
        d.perf.profile.push(ProfilePoint {
            ident: "BPK".to_string(),
            kind: ProfileKind::Waypoint,
            pos: (51.75, -0.11),
            via: "BPK5K".to_string(),
            alt_ft: 4000.0,
            dist_nm: 10.0,
            time_min: 3.0,
            fuel_used_kg: 100.0,
            fuel_remaining_kg: 9000.0,
            gross_kg: 60000.0,
            track_true_deg: 90.0,
            tas_kt: 250.0,
            gs_kt: 240.0,
            mach: 0.4,
            air: Air::standard(4000.0),
            mora_ft: None,
        });
        let mut s = String::new();
        nav_log(&mut s, &d);
        assert!(s.contains("BPK"));
        assert!(s.contains("4000FT+"));
    }
}
