//! The operational flight plan: everything planned, put together and printed.
//!
//! `dispatch` is the one function that calls all four other parts of flight planning
//! (`route`, `route::rad`, `route::oceanic`, `route::airspace`, `route::etops`, `weather`,
//! `perf`) and hands back one `Dispatch`. Every one of those calls goes through the
//! `Providers` trait (`providers.rs`), so this file can be written, read and tested
//! without any of them working yet: only a missing airport, a missing aircraft type or no
//! route at all stops a plan being produced. Everything else that fails — a forecast, a
//! report, the RAD, an oceanic track, an alternate, even the performance model itself —
//! degrades to a line in `Dispatch.perf.warnings`, which is the one place in the frozen
//! `Dispatch` shape built to carry exactly that.

pub mod export;
pub mod pdf;
pub mod providers;
pub mod simbrief;
pub mod text;

#[cfg(test)]
pub(crate) mod fixtures;

use crate::dispatch::{
    self, Airport, Alternate, Bounds, CostModel, CruisePolicy, Dispatch, EdgeRule, FiledRoute, FuelPolicy, Hazard, LatLon, LevelScheme, PerfRequest, RouteRequest, RouteRule, SimpleCost, Violation, Wind, WindField,
};
use crate::route;
use crate::weather::Samples;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use providers::{Providers, RealProviders};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------------------------
// What a flight is to be planned for.
// ---------------------------------------------------------------------------------

/// What a flight is to be planned for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchOptions {
    pub origin: String,
    pub destination: String,
    /// ICAO type designator, e.g. A20N.
    pub aircraft: String,
    pub payload_kg: f64,
    pub passengers: u32,
    /// A level asked for; left out, `levels_for` chooses a few around the aircraft's
    /// usual cruise and the search picks the cheapest.
    pub level: Option<f64>,
    pub cost_index: f64,
    /// Block off time. An hour from now by default.
    pub off_block: DateTime<Utc>,
    /// An alternate asked for; left out, `choose_alternates` picks one.
    pub alternate: Option<String>,
    /// Flight information regions the route must not enter.
    pub avoid_firs: Vec<String>,
    pub rvsm: bool,
    /// No network: still air, no reports, no hazards but what `conditions_file` gives.
    pub offline: bool,
    /// Extra hazards and winds by hand, over whatever the weather sources give.
    pub conditions_file: Option<PathBuf>,
    /// For item 15 and the printed plan; not asked of any provider.
    pub flight_number: Option<String>,
    pub registration: Option<String>,
    /// A particular aeroplane rather than a type: its registration, as `perf::airframes` holds
    /// them. Given one, the plan is worked out at that airframe's own weights.
    pub airframe: Option<String>,
}

impl Default for DispatchOptions {
    fn default() -> Self {
        DispatchOptions {
            origin: String::new(),
            destination: String::new(),
            aircraft: String::new(),
            payload_kg: 0.0,
            passengers: 0,
            level: None,
            cost_index: 30.0,
            off_block: Utc::now() + Duration::hours(1),
            alternate: None,
            avoid_firs: Vec::new(),
            rvsm: true,
            offline: false,
            conditions_file: None,
            flight_number: None,
            registration: None,
            airframe: None,
        }
    }
}

impl DispatchOptions {
    pub fn new(origin: impl Into<String>, destination: impl Into<String>, aircraft: impl Into<String>) -> DispatchOptions {
        DispatchOptions { origin: origin.into(), destination: destination.into(), aircraft: aircraft.into(), ..Default::default() }
    }
}

// ---------------------------------------------------------------------------------
// Extra hazards and winds by hand.
// ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
struct ConditionsFile {
    #[serde(default)]
    hazards: Vec<Hazard>,
    #[serde(default)]
    winds: Vec<Wind>,
}

impl ConditionsFile {
    /// Read the file; a warning and nothing extra on any problem, never an error — the
    /// plan is still worth having without it.
    fn load(path: Option<&Path>, warnings: &mut Vec<String>) -> ConditionsFile {
        let Some(path) = path else { return ConditionsFile::default() };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                warnings.push(format!("{}: {e}", path.display()));
                return ConditionsFile::default();
            }
        };
        match serde_json::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                warnings.push(format!("{}: {e}", path.display()));
                ConditionsFile::default()
            }
        }
    }
}

// ---------------------------------------------------------------------------------
// Small geography: who the RAD, the oceanic tracks and the semicircular rule apply to.
// ---------------------------------------------------------------------------------

/// Whether an ICAO code is inside the area Eurocontrol's Route Availability Document
/// covers: the `E` and `L` prefixes in full, and the handful of `U`, `B` and `G` states
/// that are also members of the network manager's EUR region — Iceland (`BI`), the Canary
/// Islands (`GC`, Spanish airspace though the country prefix is African), and the former
/// Soviet states west of the Urals that file into the same system (Ukraine `UK`, Belarus
/// `UM`, Moldova is already `LU`, and the Caucasus `UG`/`UD`/`UB`).
fn is_european(icao: &str) -> bool {
    let icao = icao.to_ascii_uppercase();
    let Some(first) = icao.chars().next() else { return false };
    if matches!(first, 'E' | 'L') {
        return true;
    }
    let two: String = icao.chars().take(2).collect();
    matches!(two.as_str(), "BI" | "GC" | "UK" | "UM" | "UG" | "UD" | "UB")
}

/// Whether a track is one an organised track system is likely to cover: a long enough
/// leg between opposite shores of an ocean, told apart by the ICAO region a prefix falls
/// in rather than by any shoreline this crate carries. A rough test, and a cautious one —
/// it costs nothing to ask `oceanic_tracks` for a system that turns out not to apply, and
/// it costs a missed one-way airway to skip a system that did.
fn crosses_ocean(origin: &str, destination: &str, nm: f64) -> bool {
    if nm < 700.0 {
        return false;
    }
    let region = |icao: &str| -> &'static str {
        match icao.chars().next().unwrap_or(' ').to_ascii_uppercase() {
            'K' | 'C' | 'M' | 'T' | 'S' | 'P' => "americas",
            'E' | 'L' | 'G' | 'D' | 'H' | 'O' | 'U' => "europe_africa_asia_west",
            'R' | 'Z' | 'V' | 'W' | 'Y' | 'N' | 'A' => "asia_pacific",
            _ => "other",
        }
    };
    let (a, b) = (region(origin), region(destination));
    a != b && a != "other" && b != "other"
}

/// Italy, France and Portugal fly the semicircular rule the other way round; everywhere
/// else in this crate's reach follows the ICAO scheme.
fn level_scheme(origin: &str, destination: &str) -> LevelScheme {
    let south_odd = |icao: &str| icao.len() >= 2 && matches!(&icao[..2], "LF" | "LI" | "LP");
    if south_odd(origin) || south_odd(destination) {
        LevelScheme::SouthOdd
    } else {
        LevelScheme::EastOdd
    }
}

/// A passenger and their bags, kilograms: the IATA standard adult passenger weight most
/// dispatch systems assume where nobody has weighed the load, used to turn a passenger count
/// this planner has picked for itself into a payload when neither was given.
const PAX_AND_BAGS_KG: f64 = 100.0;

/// A cruise level to build the candidate levels around, absent one asked for. `perf::spec`
/// does not carry a usual cruise altitude (a change worth asking for — see the final
/// report), so this reads it off the ceiling: four thousand feet under it, which for every
/// jet in the fleet this crate expects to plan for lands within a couple of steps of the
/// altitude it is actually flown at.
fn nominal_cruise_ft(spec: &dispatch::AircraftSpec) -> f64 {
    (spec.ceiling_ft - 4000.0).clamp(28_000.0, spec.ceiling_ft)
}

/// A rough true airspeed at cruise from the Mach number alone, for the fallback cost
/// model: the speed of sound at the standard temperature four thousand feet under the
/// aircraft's ceiling.
fn tas_from_mach(spec: &dispatch::AircraftSpec) -> f64 {
    let alt = nominal_cruise_ft(spec);
    let t_kelvin = dispatch::isa_temp_c(alt) + 273.15;
    let sound_kt = 38.967_854 * t_kelvin.sqrt();
    (spec.cruise_mach.max(0.5) * sound_kt).min(spec.vmo_kt.max(200.0))
}

/// A rough total fuel flow, kilograms an hour, from the aircraft's weight alone: about
/// what a jet burns at cruise for every tonne of its maximum weight. Documented as crude
/// because it is: the fallback exists so a plan can still be flown before `perf::spec` and
/// `perf::cost_model` are built, not to be accurate once they are.
fn burn_from_weight(spec: &dispatch::AircraftSpec) -> f64 {
    (spec.mtow_kg * 0.028).max(400.0)
}

// ---------------------------------------------------------------------------------
// ETOPS diversion airports.
// ---------------------------------------------------------------------------------

/// Airports along the direct track that could serve an ETOPS diversion: sampled points
/// along the great circle, each asked of `airports_near` out to what the one-engine speed
/// covers in the approval, with a runway long enough for the type. Capped, since a long
/// oceanic leg can otherwise pull in the same handful of airports from every sample point.
/// The shortest runway a twin will be planned to divert to.
const ETOPS_MIN_RUNWAY_FT: f64 = 6000.0;

/// Every airport a twin could divert to along the way: what the ETOPS rule measures the route
/// against.
///
/// The list has to cover the whole route. Gathering it used to stop as soon as two dozen
/// airports had been found, which on any real flight happened at the first sample point — there
/// are far more than two dozen airports with a six-thousand-foot runway within an hour or three
/// of Bangalore — so the rule was left holding only airports near the departure. Every point
/// further along was then further from a diversion airport than the rule allows, every edge the
/// search looked at was forbidden, and a Boeing 777 could not be planned anywhere at all while
/// an Airbus A380 on the same route planned in under three seconds, because four engines need
/// no such rule. There is no cap now: `etops::Grid` buckets what it is given and is built for
/// tens of thousands, so a few hundred costs nothing, and a cap on this list is a cap on where
/// the aeroplane is allowed to fly.
fn etops_airports(near: &dyn Fn(LatLon, f64) -> Vec<Airport>, origin: LatLon, destination: LatLon, one_engine_tas_kt: f64, minutes: u32) -> Vec<Airport> {
    let reach_nm = one_engine_tas_kt * minutes as f64 / 60.0;
    // Closely enough spaced that consecutive circles of `reach_nm` overlap, so no stretch of the
    // route falls between two samples and is searched for diversions by neither.
    let span_nm = dispatch::distance_nm(origin, destination);
    let samples = ((span_nm / reach_nm.max(1.0)).ceil() as usize * 2).clamp(6, 64);
    let mut out: Vec<Airport> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for i in 0..=samples {
        let at = dispatch::along(origin, destination, i as f64 / samples as f64);
        for a in near(at, reach_nm) {
            if seen.insert(a.icao.clone()) {
                out.push(a);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------
// A performance plan good enough to fly on before `perf::plan` exists, or when it fails.
// ---------------------------------------------------------------------------------

/// A straight-line estimate of the profile, the fuel and the weights, from nothing but
/// the route, the cost model already chosen and the EASA-style fuel policy. No step
/// climbs, no equal-time points, one level the whole way: what it stands in for is a
/// performance model, not a flight, and it says so in its own warning.
fn fallback_perf_plan(spec: &dispatch::AircraftSpec, route: &FiledRoute, alternate: Option<&FiledRoute>, air: &dyn WindField, payload_kg: f64, cost: &dyn CostModel, fuel: &FuelPolicy) -> crate::dispatch::PerfPlan {
    use crate::dispatch::{FuelBreakdown, LegQuery, ProfileKind, ProfilePoint, Weights};

    let mut profile = Vec::new();
    let (mut dist_nm, mut minutes, mut trip_kg) = (0.0, 0.0, 0.0);
    let zfw_kg = (spec.oew_kg + payload_kg).min(spec.mzfw_kg);
    for (i, w) in route.points.iter().enumerate() {
        if i > 0 {
            let prev = &route.points[i - 1];
            let leg = cost.leg(&LegQuery { from: prev.pos, to: w.pos, level_ft: route.cruise_ft, when: route.off_block + Duration::seconds((minutes * 60.0) as i64), flown_nm: dist_nm });
            dist_nm += dispatch::distance_nm(prev.pos, w.pos);
            minutes += leg.minutes;
            trip_kg += leg.fuel_kg;
        }
        let air_here = air.air(w.pos, route.cruise_ft, route.off_block + Duration::seconds((minutes * 60.0) as i64));
        profile.push(ProfilePoint {
            ident: w.ident.clone(),
            kind: ProfileKind::Waypoint,
            pos: w.pos,
            via: w.via.clone(),
            alt_ft: route.cruise_ft,
            dist_nm,
            time_min: minutes,
            fuel_used_kg: trip_kg,
            fuel_remaining_kg: 0.0, // filled in below, once the total is known
            gross_kg: 0.0,
            track_true_deg: 0.0,
            tas_kt: 0.0,
            gs_kt: 0.0,
            mach: 0.0,
            air: air_here,
            mora_ft: None,
        });
    }

    let alt_kg = match alternate {
        Some(alt) => {
            let leg = cost.leg(&LegQuery { from: alt.origin.pos, to: alt.destination.pos, level_ft: alt.cruise_ft, when: alt.off_block, flown_nm: 0.0 });
            leg.fuel_kg
        }
        None => 0.0,
    };
    // The burn rate the route was just flown at, kilograms a minute, is what both the
    // percentage rule and the minimum-minutes rule for contingency are measured against.
    let burn_per_min = trip_kg.max(1.0) / minutes.max(1.0);
    let contingency_kg = (trip_kg * fuel.contingency_pct / 100.0).max(burn_per_min * fuel.contingency_min_minutes);
    let final_reserve_kg = burn_per_min * fuel.final_reserve_min;
    let block_kg = trip_kg + contingency_kg + alt_kg + final_reserve_kg + fuel.extra_kg;
    let takeoff_kg = block_kg;
    let landing_kg = takeoff_kg - trip_kg;

    for pt in &mut profile {
        pt.fuel_remaining_kg = takeoff_kg - pt.fuel_used_kg;
        pt.gross_kg = zfw_kg + pt.fuel_remaining_kg;
    }

    let tow_kg = (zfw_kg + takeoff_kg).min(spec.mtow_kg);
    let lw_kg = (tow_kg - trip_kg).min(spec.mlw_kg);
    let limited = if spec.oew_kg + payload_kg > spec.mzfw_kg { Some("MZFW".to_string()) } else if zfw_kg + takeoff_kg > spec.mtow_kg { Some("MTOW".to_string()) } else { None };

    crate::dispatch::PerfPlan {
        profile,
        fuel: FuelBreakdown { taxi_kg: fuel.taxi_kg, trip_kg, contingency_kg, alternate_kg: alt_kg, final_reserve_kg, extra_kg: fuel.extra_kg, tanker_kg: 0.0, takeoff_kg, block_kg: block_kg + fuel.taxi_kg, landing_kg },
        weights: Weights { oew_kg: spec.oew_kg, payload_kg, zfw_kg, tow_kg, lw_kg, max_zfw_kg: spec.mzfw_kg, max_tow_kg: spec.mtow_kg, max_lw_kg: spec.mlw_kg, limited_by: limited },
        step_climbs: Vec::new(),
        avg_wind_kt: 0.0,
        avg_isa_dev: 0.0,
        equal_time_points: Vec::new(),
        no_return: None,
        alternate: alternate.map(|a| crate::dispatch::AlternatePlan { icao: a.destination.icao.clone(), dist_nm: a.distance_nm(), time_min: 0.0, fuel_kg: alt_kg, cruise_ft: a.cruise_ft }),
        warnings: vec!["performance model not available: fuel and time are a straight-line estimate on a fixed cost model, not a flown vertical profile".to_string()],
    }
}

// ---------------------------------------------------------------------------------
// The orchestration itself.
// ---------------------------------------------------------------------------------

/// Plan a flight from end to end, calling the real four other parts.
pub fn dispatch(opts: &DispatchOptions) -> Result<Dispatch> {
    dispatch_with(opts, &RealProviders)
}

/// Plan a flight from end to end against any `Providers`: what every other command in
/// this module is built on top of, and what the tests exercise directly.
/// How far either side of the great circle the winds are asked for. A route is allowed to
/// wander this far off the direct line and still be flown on forecast air rather than on the
/// air at the corridor's edge; it is the margin the one box round the two airports used, kept
/// so that widening the corridor is a deliberate change and not a side effect of this one.
const WIND_MARGIN_NM: f64 = 300.0;

pub fn dispatch_with(opts: &DispatchOptions, p: &dyn Providers) -> Result<Dispatch> {
    let mut warnings: Vec<String> = Vec::new();
    // A plan is built the way a map is: one named thing after another. Each is announced
    // with what it found and what it cost, so that a plan that comes out wrong, or slowly,
    // says which part of it to look at.
    const STAGES: usize = 12;
    let mut stage_n = 0usize;
    let mut clock = std::time::Instant::now();
    let mut say = |name: &str, detail: String| {
        stage_n += 1;
        crate::term::stage(stage_n, STAGES, name, &detail, clock.elapsed().as_millis());
        clock = std::time::Instant::now();
    };

    // The airports and the aircraft: the only things whose absence is fatal.
    let origin = p.airport(&opts.origin).ok_or_else(|| anyhow!("{} is not an airport this planner knows", opts.origin.to_uppercase()))?;
    let destination = p.airport(&opts.destination).ok_or_else(|| anyhow!("{} is not an airport this planner knows", opts.destination.to_uppercase()))?;
    let spec = p.aircraft_spec(&opts.aircraft).with_context(|| format!("{} is not an aircraft this planner knows", opts.aircraft.to_uppercase()))?;
    // A particular aeroplane, where one was asked for: its own empty weight and its own four
    // limits over the type's, which is what decides how much this flight can actually carry.
    // The modelled performance -- the ceiling, the speeds, the burn -- stays the type's.
    let spec = match opts.airframe.as_deref().and_then(crate::perf::airframes::by_registration) {
        Some(frame) => {
            warnings.push(format!("weights are {}'s own ({})", frame.registration, frame.label()));
            frame.onto(&spec)
        }
        None => spec,
    };

    // Nobody flies an empty aeroplane: a payload of zero, left as `DispatchOptions`'s
    // default, is not a real flight to plan, so a passenger count and payload are filled in
    // here rather than carried through as zero. An explicit `--pax` is honoured over the
    // type's typical seating, and an explicit `--payload` over the standard passenger-and-
    // bag weight; either can still be given as exactly nothing by asking for the other one
    // alone with a very light aircraft, which is a real choice this does not second-guess.
    let passengers = if opts.passengers > 0 { opts.passengers } else { crate::perf::aircraft::typical_pax(&spec.icao_type) };
    let payload_kg = if opts.payload_kg > 0.0 { opts.payload_kg } else { passengers as f64 * PAX_AND_BAGS_KG };

    let direct_nm = dispatch::distance_nm(origin.pos, destination.pos);
    say("airports", format!("{} to {}, {direct_nm:.0} nm direct", origin.icao, destination.icao));
    say("aircraft", format!("{} {}, ceiling FL{:.0}", spec.icao_type, spec.name, spec.ceiling_ft / 100.0));

    // The area everything about the air is asked over. One box round the two airports is the
    // box round two *points*, and on a long route the great circle between them leaves it: from
    // Bangalore to New York the box reaches 45 degrees north while the route runs past 70. So
    // the air is asked for over a corridor that follows the circle, cut into the run of
    // rectangles a rectangular source can answer -- fewer grid points on most routes, and on
    // the ones where it is not fewer it is at least the right ones.
    let corridor = crate::route::ellipse::Ellipse::new(origin.pos, destination.pos, 1.0, 1.0).corridor(WIND_MARGIN_NM);
    // A single box still round the whole of it, for the sources that take one area and whose
    // answers are a handful of items rather than a grid: hazards and notices are cheap to ask
    // widely for, and narrowing them would only risk missing one.
    let bounds = Bounds::around(origin.pos, destination.pos, 300.0);
    let extra = ConditionsFile::load(opts.conditions_file.as_deref(), &mut warnings);

    // The air.
    let wind: Arc<dyn WindField> = if !extra.winds.is_empty() {
        warnings.push(format!("{} wind sample(s) from the conditions file override the forecast", extra.winds.len()));
        Arc::new(Samples(extra.winds.clone()))
    } else if opts.offline {
        Arc::new(dispatch::StillAir)
    } else {
        match p.wind_field(&corridor, opts.off_block) {
            Ok(w) => w,
            Err(e) => {
                warnings.push(format!("winds aloft not available ({e:#}); planned on still air"));
                Arc::new(dispatch::StillAir)
            }
        }
    };

    say("winds aloft", if opts.offline { "still air (offline)".to_string() } else { format!("forecast over {} strip(s)", corridor.len()) });

    // The reports at both ends.
    let origin_metar = if opts.offline { None } else { p.metar(&origin.icao).map_err(|e| warnings.push(format!("{} METAR not available: {e:#}", origin.icao))).ok() };
    let destination_metar = if opts.offline { None } else { p.metar(&destination.icao).map_err(|e| warnings.push(format!("{} METAR not available: {e:#}", destination.icao))).ok() };
    let origin_taf = if opts.offline { None } else { p.taf(&origin.icao).map_err(|e| warnings.push(format!("{} TAF not available: {e:#}", origin.icao))).ok() };
    let destination_taf = if opts.offline { None } else { p.taf(&destination.icao).map_err(|e| warnings.push(format!("{} TAF not available: {e:#}", destination.icao))).ok() };

    say("reports", format!("{} METAR, {} TAF", [&origin_metar, &destination_metar].iter().filter(|m| m.is_some()).count(), [&origin_taf, &destination_taf].iter().filter(|t| t.is_some()).count()));

    // The hazards: SIGMETs, conflict zones, NOTAM areas, and the file.
    let mut hazards: Vec<Hazard> = Vec::new();
    if !opts.offline {
        match p.sigmets(bounds, opts.off_block) {
            Ok(h) => hazards.extend(h),
            Err(e) => warnings.push(format!("SIGMETs not available: {e:#}")),
        }
    }
    hazards.extend(p.conflict_zones(opts.off_block));
    if !opts.offline {
        match p.notam_hazards(bounds, opts.off_block, opts.off_block + Duration::hours(18)) {
            Ok(h) => hazards.extend(h),
            Err(e) => warnings.push(format!("NOTAM areas not available: {e:#}")),
        }
    }
    hazards.extend(extra.hazards.clone());

    say("hazards", format!("{} area(s) to keep out of or pay for", hazards.len()));

    // The rules: RAD in Europe, oceanic tracks over an ocean, ETOPS for an approved twin.
    let europe = is_european(&origin.icao) || is_european(&destination.icao);
    let rad = if europe && !opts.offline {
        match p.rad(opts.off_block) {
            Ok(r) => {
                warnings.push("RAD: Eurocontrol Route Availability Document checked".to_string());
                Some(r)
            }
            Err(e) => {
                warnings.push(format!("RAD: not checked ({e:#})"));
                None
            }
        }
    } else {
        None
    };

    let trip_nm = dispatch::distance_nm(origin.pos, destination.pos);
    let ocean = crosses_ocean(&origin.icao, &destination.icao, trip_nm);
    let tracks = if ocean && !opts.offline {
        match p.oceanic_tracks(opts.off_block) {
            Ok(t) => t,
            Err(e) => {
                warnings.push(format!("oceanic tracks not applied: {e:#}"));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    say("oceanic tracks", if !ocean { "not an ocean crossing".to_string() } else { format!("{} track(s)", tracks.len()) });
    let graph = p.network(&tracks);
    say("airway network", format!("{} fixes, {} edges", graph.fixes().len(), graph.compact().edge_count()));
    let track_rule = (!tracks.is_empty()).then(|| route::oceanic::TrackRule::new(tracks));

    let needs_etops = spec.engines == 2 && spec.etops_minutes.is_some() && trip_nm > 400.0;
    let etops = needs_etops.then(|| {
        let tas = spec.one_engine_tas_kt.unwrap_or_else(|| tas_from_mach(&spec) * 0.6);
        let near = |at: LatLon, radius_nm: f64| p.airports_near(at, radius_nm, ETOPS_MIN_RUNWAY_FT).into_iter().map(|(a, _)| a).collect();
        let airports = etops_airports(&near, origin.pos, destination.pos, tas, spec.etops_minutes.unwrap_or(180));
        warnings.push(format!("ETOPS {} planned round {} candidate diversion airport(s)", spec.etops_minutes.unwrap_or(180), airports.len()));
        route::etops::Etops::new(spec.etops_minutes.unwrap_or(180), tas, airports)
    });

    say("rules", format!("RAD {}, ETOPS {}", if rad.is_some() { "on" } else { "off" }, if etops.is_some() { "on" } else { "off" }));

    let mut edge_rules: Vec<&dyn EdgeRule> = Vec::new();
    if let Some(r) = &rad {
        edge_rules.push(r);
    }
    if let Some(t) = &track_rule {
        edge_rules.push(t);
    }
    if let Some(e) = &etops {
        edge_rules.push(e);
    }
    let mut route_rules: Vec<&dyn RouteRule> = Vec::new();
    if let Some(r) = &rad {
        route_rules.push(r);
    }

    // The cost model.
    let zfw_kg = (spec.oew_kg + payload_kg).min(spec.mzfw_kg);
    let cost_model = match p.cost_model(&spec, wind.as_ref(), zfw_kg, trip_nm, Some(opts.cost_index)) {
        Ok(c) => c,
        Err(e) => {
            warnings.push(format!("performance-based cost model not available ({e:#}); planned on a fixed speed and burn"));
            Box::new(SimpleCost { tas_kt: tas_from_mach(&spec), kg_per_hour: burn_from_weight(&spec), ceiling_ft: spec.ceiling_ft, air: wind.as_ref(), max_tailwind_kt: 220.0 })
        }
    };

    say("cost model", format!("cost index {:.0}, {:.0} kg without fuel", opts.cost_index, zfw_kg));

    // The candidate levels: the one asked for, or a few round the aircraft's usual cruise.
    let track_deg = dispatch::bearing_deg(origin.pos, destination.pos);
    let scheme = level_scheme(&origin.icao, &destination.icao);
    let levels: Vec<f64> = match opts.level {
        Some(l) => vec![l],
        None => {
            let usual = nominal_cruise_ft(&spec);
            let mut v = dispatch::levels_for(track_deg, 10_000.0, spec.ceiling_ft.min(usual + 6000.0), opts.rvsm, scheme);
            v.truncate(4);
            v
        }
    };
    if levels.is_empty() {
        anyhow::bail!("no cruise level between 10,000 ft and the aircraft's ceiling suits this track");
    }

    // The route, and the way round, chosen together.
    let req = RouteRequest {
        origin: &origin,
        destination: &destination,
        cruise_ft: opts.level.unwrap_or(0.0),
        levels_ft: &levels,
        cost: cost_model.as_ref(),
        cost_index: opts.cost_index,
        off_block: opts.off_block,
        air: wind.as_ref(),
        hazards: &hazards,
        edge_rules: &edge_rules,
        route_rules: &route_rules,
        dep_runway: None,
        arr_runway: None,
        origin_wind: origin_metar.as_ref().and_then(|m| m.wind_from_deg.map(|d| (d, m.wind_kt))),
        destination_wind: destination_metar.as_ref().and_then(|m| m.wind_from_deg.map(|d| (d, m.wind_kt))),
        rvsm: opts.rvsm,
        avoid_firs: &opts.avoid_firs,
        free_route: true,
    };
    say("levels", levels.iter().map(|l| format!("FL{:.0}", l / 100.0)).collect::<Vec<_>>().join(" "));
    let mut found = p.plan_routes(&graph, &req, 1).map_err(|e| anyhow!("no usable route from {} to {}: {e:#}", origin.icao, destination.icao))?;
    if found.is_empty() {
        anyhow::bail!("no route found from {} to {}", origin.icao, destination.icao);
    }
    let route = found.remove(0);
    say("route", format!("{} points, {:.0} nm, {:.0}% over direct", route.points.len(), route.distance_nm(), (route.distance_nm() / direct_nm.max(1.0) - 1.0) * 100.0));

    let violations: Vec<Violation> = route_rules.iter().flat_map(|r| r.check_route(&route)).collect();
    let firs = p.fir_crossings(&route);
    if !firs.is_empty() {
        warnings.push(format!("crossed {} flight information region(s): {}", firs.len(), firs.iter().map(|f| f.ident.as_str()).collect::<Vec<_>>().join(", ")));
    }

    // The alternate: the one given, or the best of `choose_alternates`, each considered
    // with its own route at a low level until one plans. `choose_alternates` wants an
    // arrival time, which needs a rough total time before the performance model has run;
    // summed leg by leg on the cost model already chosen, which is exactly what it is for.
    let route_minutes: f64 = route
        .points
        .windows(2)
        .map(|w| cost_model.leg(&dispatch::LegQuery { from: w[0].pos, to: w[1].pos, level_ft: route.cruise_ft, when: route.off_block, flown_nm: 0.0 }).minutes)
        .sum();
    let eta = route.off_block + Duration::seconds((route_minutes * 60.0).round() as i64);
    let candidates: Vec<Alternate> = if let Some(icao) = &opts.alternate {
        match p.airport(icao) {
            Some(a) => vec![Alternate { distance_nm: dispatch::distance_nm(destination.pos, a.pos), airport: a, reason: "requested".to_string() }],
            None => {
                warnings.push(format!("{icao} (requested as the alternate) is not an airport this planner knows"));
                Vec::new()
            }
        }
    } else if !opts.offline {
        match p.choose_alternates(&destination, eta, 3) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!("no alternate chosen automatically: {e:#}"));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let mut alt_route: Option<FiledRoute> = None;
    for cand in &candidates {
        let low = (cand.airport.elevation_ft + 10_000.0).clamp(10_000.0, spec.ceiling_ft);
        let low_levels = [low];
        let alt_req = RouteRequest {
            origin: &destination,
            destination: &cand.airport,
            cruise_ft: 0.0,
            levels_ft: &low_levels,
            cost: cost_model.as_ref(),
            cost_index: opts.cost_index,
            off_block: eta,
            air: wind.as_ref(),
            hazards: &hazards,
            edge_rules: &[],
            route_rules: &[],
            dep_runway: None,
            arr_runway: None,
            origin_wind: None,
            destination_wind: None,
            rvsm: opts.rvsm,
            avoid_firs: &opts.avoid_firs,
            free_route: true,
        };
        match p.plan_routes(&graph, &alt_req, 1) {
            Ok(mut v) if !v.is_empty() => {
                alt_route = Some(v.remove(0));
                break;
            }
            Ok(_) => {}
            Err(e) => warnings.push(format!("alternate {}: {e:#}", cand.airport.icao)),
        }
    }
    let alternate_taf = match (&alt_route, opts.offline) {
        (Some(a), false) => p.taf(&a.destination.icao).map_err(|e| warnings.push(format!("{} TAF not available: {e:#}", a.destination.icao))).ok(),
        _ => None,
    };

    // The vertical profile, the fuel and the weights. Equal-time points and a point of no
    // return only mean something where turning back is a real question — an ocean crossing,
    // or a twin flying under an ETOPS/EDTO approval — so `etp_airports` is left empty for
    // anything else, which is `perf::plan`'s own signal (see its doc comment on
    // `PerfRequest::etp_airports`) to leave both off the plan rather than print them for
    // every short domestic sector at whatever the great-circle midpoint happens to be.
    let mut etp_airports: Vec<Airport> = Vec::new();
    if ocean || needs_etops {
        etp_airports.push(origin.clone());
        etp_airports.push(destination.clone());
        if let Some(a) = &alt_route {
            etp_airports.push(a.destination.clone());
        }
    }
    let fuel_policy = FuelPolicy::default();
    let perf_req = PerfRequest {
        spec: &spec,
        route: &route,
        alternate: alt_route.as_ref(),
        air: wind.as_ref(),
        payload_kg,
        cruise: CruisePolicy { cost_index: Some(opts.cost_index), ..Default::default() },
        fuel: fuel_policy.clone(),
        etp_airports: &etp_airports,
        scheme,
        rvsm: opts.rvsm,
    };
    let mut perf = match p.perf_plan(&perf_req) {
        Ok(pp) => pp,
        Err(e) => {
            warnings.push(format!("performance model not available ({e:#}); flown on a straight-line estimate"));
            fallback_perf_plan(&spec, &route, alt_route.as_ref(), wind.as_ref(), payload_kg, cost_model.as_ref(), &fuel_policy)
        }
    };
    perf.warnings.extend(warnings);
    // `cost_model` borrows `spec` and `wind`; both are moved into (or outlived by) the
    // `Dispatch` below, so the borrow is dropped explicitly here rather than left to run
    // to the end of the function, past the move.
    drop(cost_model);

    Ok(Dispatch {
        route,
        perf,
        spec,
        alternate: alt_route,
        origin_metar,
        destination_metar,
        origin_taf,
        destination_taf,
        alternate_taf,
        hazards: hazards.iter().map(describe_hazard).collect(),
        violations,
        generated: Utc::now(),
        airac: crate::sources::navdata::airac(),
    })
}

fn describe_hazard(h: &Hazard) -> String {
    if h.source.is_empty() {
        h.name.clone()
    } else {
        format!("{} ({})", h.name, h.source)
    }
}

#[cfg(test)]
mod etops_airport_tests {
    use super::*;

    /// A world with an airport every five degrees along the equator, so "near" means something
    /// at every point of a long route rather than only at its ends.
    fn dense_world(at: LatLon, radius_nm: f64) -> Vec<Airport> {
        (-36..=36)
            .map(|k| {
                let lon = k as f64 * 5.0;
                Airport { icao: format!("X{:03}", k + 36), name: String::new(), pos: (0.0, lon), elevation_ft: 0.0 }
            })
            .filter(|a| dispatch::distance_nm(at, a.pos) <= radius_nm)
            .collect()
    }

    /// The diversion list has to cover the whole route, not merely its beginning.
    ///
    /// It used to stop as soon as two dozen airports had been found, which on any real flight
    /// happened at the first sample point: the rule was then left holding only airports near the
    /// departure, every point further along was further from a diversion than it allows, and no
    /// twin could be planned at all.
    #[test]
    fn the_diversion_list_reaches_the_far_end_of_the_route() {
        // A hundred and fifty degrees apart along the equator: nine thousand miles, which is a
        // long-haul sector and not a hop across the date line.
        let origin = (0.0, -75.0);
        let destination = (0.0, 75.0);
        let found = etops_airports(&dense_world, origin, destination, 450.0, 180u32);
        assert!(found.len() > 24, "far more than the old cap: {}", found.len());
        let reach_nm = 450.0 * 180.0 / 60.0;
        // An airport near each end, and one near the middle, all have to be in the list.
        for probe in [origin, destination, (0.0, 0.0)] {
            assert!(found.iter().any(|a| dispatch::distance_nm(a.pos, probe) <= reach_nm), "nothing within reach of {probe:?}");
        }
    }

    /// The samples are close enough together that no stretch of the route falls between two of
    /// them and is searched for diversions by neither.
    #[test]
    fn no_stretch_of_the_route_goes_unsampled() {
        let origin = (0.0, -75.0);
        let destination = (0.0, 75.0);
        let (tas, minutes) = (450.0, 180u32);
        let reach_nm = tas * minutes as f64 / 60.0;
        let found = etops_airports(&dense_world, origin, destination, tas, minutes);
        // Walk the route and check every point has something within reach in the list.
        for k in 0..=200 {
            let at = dispatch::along(origin, destination, k as f64 / 200.0);
            assert!(found.iter().any(|a| dispatch::distance_nm(a.pos, at) <= reach_nm), "nothing within reach at {at:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use providers::fake::{FakeProviders, AIRCRAFT, DESTINATION, ORIGIN};

    fn opts() -> DispatchOptions {
        DispatchOptions::new(ORIGIN, DESTINATION, AIRCRAFT)
    }

    #[test]
    fn a_plan_comes_out_of_nothing_but_the_fake() {
        let d = dispatch_with(&opts(), &FakeProviders::new()).unwrap();
        assert_eq!(d.route.origin.icao, ORIGIN);
        assert_eq!(d.route.destination.icao, DESTINATION);
        assert!(d.perf.fuel.block_kg > 0.0);
        assert!(d.alternate.is_some(), "choose_alternates should have found one");
        // RAD is a stub that always errors: both airports are European, so it was asked
        // about and the failure shows up as a warning, not as a missing plan.
        assert!(d.perf.warnings.iter().any(|w| w.contains("RAD")));
    }

    #[test]
    fn a_missing_airport_is_fatal() {
        let mut o = opts();
        o.origin = "ZZZZ".to_string();
        assert!(dispatch_with(&o, &FakeProviders::new()).is_err());
    }

    #[test]
    fn a_missing_aircraft_is_fatal() {
        let mut o = opts();
        o.aircraft = "ZZZZ".to_string();
        assert!(dispatch_with(&o, &FakeProviders::new()).is_err());
    }

    #[test]
    fn no_route_is_fatal() {
        let p = FakeProviders { no_route: true, ..FakeProviders::new() };
        assert!(dispatch_with(&opts(), &p).is_err());
    }

    #[test]
    fn a_missing_forecast_degrades_to_a_warning_not_a_failure() {
        let p = FakeProviders::failing(&["wind_field", "metar", "taf", "sigmets", "notam_hazards", "choose_alternates"]);
        let d = dispatch_with(&opts(), &p).unwrap();
        assert!(d.origin_metar.is_none());
        assert!(d.alternate.is_none());
        assert!(d.perf.warnings.iter().any(|w| w.contains("winds aloft")));
        assert!(d.perf.warnings.iter().any(|w| w.contains("METAR")));
    }

    #[test]
    fn a_missing_performance_model_falls_back_to_a_straight_line_estimate() {
        let p = FakeProviders::failing(&["perf_plan"]);
        let d = dispatch_with(&opts(), &p).unwrap();
        assert!(d.perf.warnings.iter().any(|w| w.contains("straight-line")));
        assert!(!d.perf.profile.is_empty());
        assert!(d.perf.fuel.block_kg > d.perf.fuel.trip_kg);
        assert!(d.perf.weights.tow_kg <= d.spec.mtow_kg);
    }

    #[test]
    fn a_missing_cost_model_falls_back_to_simple_cost_and_still_routes() {
        let p = FakeProviders::failing(&["cost_model"]);
        let d = dispatch_with(&opts(), &p).unwrap();
        assert!(d.perf.warnings.iter().any(|w| w.contains("fixed speed")));
    }

    #[test]
    fn a_level_asked_for_is_the_one_flown() {
        let mut o = opts();
        o.level = Some(37_000.0);
        let d = dispatch_with(&o, &FakeProviders::new()).unwrap();
        assert_eq!(d.route.cruise_ft, 37_000.0);
    }

    #[test]
    fn offline_skips_every_network_call() {
        let mut o = opts();
        o.offline = true;
        let d = dispatch_with(&o, &FakeProviders::new()).unwrap();
        assert!(d.origin_metar.is_none());
        assert!(d.origin_taf.is_none());
        assert!(d.alternate.is_none(), "no alternate is chosen offline unless one is named");
    }

    #[test]
    fn a_named_alternate_is_used_over_the_chosen_one() {
        let mut o = opts();
        o.alternate = Some("LEBL".to_string());
        let d = dispatch_with(&o, &FakeProviders::new()).unwrap();
        assert_eq!(d.alternate.unwrap().destination.icao, "LEBL");
    }

    #[test]
    fn european_airports_are_recognised_by_prefix() {
        assert!(is_european("LPPT"));
        assert!(is_european("EGLL"));
        assert!(is_european("BIKF"));
        assert!(is_european("GCLP"));
        assert!(is_european("UKBB"));
        assert!(!is_european("KJFK"));
        assert!(!is_european("VABB"));
    }

    #[test]
    fn an_ocean_apart_is_recognised_and_a_short_hop_is_not() {
        assert!(crosses_ocean("EGLL", "KJFK", 3000.0));
        assert!(!crosses_ocean("EGLL", "LFPG", 200.0));
        assert!(!crosses_ocean("EGLL", "KJFK", 400.0));
    }

    #[test]
    fn conditions_file_hazards_and_winds_reach_the_plan() {
        let dir = std::env::temp_dir().join(format!("ofp-conditions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("extra.json");
        std::fs::write(&path, r#"{"hazards":[{"name":"pilot-reported CB","polygon":[[38.9,-9.0],[38.9,-8.5],[38.5,-8.5]],"kind":{"avoid":null}}],"winds":[]}"#).unwrap();
        let mut o = opts();
        o.conditions_file = Some(path);
        let d = dispatch_with(&o, &FakeProviders::new()).unwrap();
        assert!(d.hazards.iter().any(|h| h.contains("pilot-reported CB")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_conditions_file_is_a_warning_not_a_failure() {
        let dir = std::env::temp_dir().join(format!("ofp-bad-conditions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.json");
        std::fs::write(&path, "not json").unwrap();
        let mut o = opts();
        o.conditions_file = Some(path);
        let d = dispatch_with(&o, &FakeProviders::new());
        assert!(d.is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
