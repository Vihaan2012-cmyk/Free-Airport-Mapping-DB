//! Flying a route: climb, cruise leg by leg with step climbs, descent, and the navigation
//! log every waypoint, top of climb, top of descent and step climb comes out as.
//!
//! The three phases are each a small numerical integration — a few hundred steps of a
//! fraction of a minute apiece — rather than a closed-form result, because thrust, drag and
//! specific fuel consumption all depend on the altitude and weight that the integration
//! itself is discovering. This runs once per plan, not once per leg the way `cost_model`
//! does, so the cost of a few thousand steps of arithmetic is not a concern.
//!
//! Wind and temperature are sampled at the aircraft's actual position and time during
//! cruise, where the distance flown makes it matter; climb and descent are short enough
//! against the leg that the air is sampled once, at the airport underneath, without much
//! loss — a simplification the docstring on `dispatch::PerfRequest` allows ("loosely").
//! Magnetic variation for the semicircular rule is likewise approximated by the true track,
//! which only misplaces a level choice within a few degrees of a boundary.

use crate::dispatch::{along, bearing_deg, distance_nm, levels_for, Air, AlternatePlan, EqualTimePoint, FiledRoute, LevelScheme, PerfPlan, PerfRequest, ProfileKind, ProfilePoint, WindField};
use crate::perf::aero;
use crate::perf::aircraft::{self, TypeData};
use crate::perf::atmosphere;
use crate::perf::cost::econ_mach;
use crate::perf::etp;
use crate::perf::fuel::{self, HoldRates, TankerInputs};
use crate::perf::weights;
use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------------
// The climb/descent speed schedule.
// ---------------------------------------------------------------------------------

/// The Mach flown at an altitude on a CAS/Mach climb or descent schedule: 250 kt (or the
/// type's overspeed limit, if lower) below 10,000 ft, then a fixed CAS, capped at a Mach —
/// exactly the shape a "climb via SID, 250 below 10,000, then 280/.78" clearance describes,
/// worked the same way whichever direction it is flown.
fn schedule_mach(t: &TypeData, alt_ft: f64, cas_above_10k: f64, mach_cap: f64, temp_c: f64) -> f64 {
    let cas = if alt_ft < 10_000.0 { 250.0_f64.min(t.vmo_kt - 20.0) } else { cas_above_10k };
    atmosphere::mach_from_cas(cas.max(150.0), alt_ft, temp_c).min(mach_cap)
}

// ---------------------------------------------------------------------------------
// The vertical integrator: shared by the initial climb, every step climb, and the descent
// flown backwards from the destination.
// ---------------------------------------------------------------------------------

/// One sample: the distance covered so far (from the segment's own start, not the route's),
/// the time taken, the altitude and the aircraft's weight there.
type Trace = Vec<(f64, f64, f64, f64)>;

struct VertResult {
    dist_nm: f64,
    time_min: f64,
    fuel_kg: f64,
    end_weight_kg: f64,
    trace: Trace,
}

/// Integrate a climb or a descent between two altitudes at a fixed track and a single
/// sampled point of air, in fractions of a minute.
///
/// `climbing` selects the physics: a climb flies at the greatest thrust the engines can
/// give and gains height; a descent flies backwards from touchdown at flight idle, so the
/// weight *grows* as the integration proceeds (the aircraft is lighter after burning fuel
/// down to the ground than it was at the top of descent) and what is returned as `dist_nm`
/// and `time_min` is measured from touchdown outward, not from the top of descent down.
#[allow(clippy::too_many_arguments)]
fn integrate_vertical(t: &TypeData, start_weight_kg: f64, start_alt_ft: f64, target_alt_ft: f64, climbing: bool, cas_kt: f64, mach_cap: f64, wind_pos: crate::dispatch::LatLon, track_deg: f64, air: &dyn WindField, start_time: DateTime<Utc>) -> VertResult {
    let mut alt = start_alt_ft;
    let mut weight = start_weight_kg;
    let mut dist = 0.0;
    let mut time = 0.0;
    let mut trace: Trace = vec![(0.0, 0.0, alt, weight)];
    if target_alt_ft <= start_alt_ft + 1.0 {
        return VertResult { dist_nm: 0.0, time_min: 0.0, fuel_kg: 0.0, end_weight_kg: weight, trace };
    }
    // `aero::rate_of_climb_fpm` spends all of the excess thrust on gaining height; a real
    // climb also spends a good share of it accelerating (true airspeed keeps rising through
    // a constant-CAS climb) and is usually flown at a derated climb thrust rather than the
    // maximum the engines could give, for noise and engine life. Both effects are folded
    // into one fraction here rather than modelled separately, and calibrated against
    // typical reported initial climb rates (2,000-3,500 fpm for a narrowbody) instead of
    // the 5,000+ fpm an uncorrected excess-thrust figure implies.
    const CLIMB_THRUST_FRACTION: f64 = 0.60;
    const MAX_STEPS: usize = 4000;
    for _ in 0..MAX_STEPS {
        if alt >= target_alt_ft - 1.0 {
            break;
        }
        let when = start_time + chrono::Duration::milliseconds((time * 60_000.0) as i64);
        let local = air.air(wind_pos, alt, when);
        let mach = schedule_mach(t, alt, cas_kt, mach_cap, local.temp_c);
        let tas_kt = atmosphere::tas_from_mach(mach, local.temp_c);
        let rate_fpm = if climbing {
            let thrust = aero::max_thrust_n(t, alt, mach, local.temp_c) * CLIMB_THRUST_FRACTION;
            let drag = aero::drag_n(t, weight, alt, mach, local.temp_c);
            let tas_ms = tas_kt * aero::KT_TO_MS;
            ((thrust - drag).max(0.0) * tas_ms / (weight * aero::G0) * aero::MS_TO_FPM).max(80.0)
        } else {
            let idle_thrust = aero::max_thrust_n(t, alt, mach, local.temp_c) * 0.05;
            let drag = aero::drag_n(t, weight, alt, mach, local.temp_c);
            let tas_ms = tas_kt * aero::KT_TO_MS;
            ((drag - idle_thrust).max(0.0) * tas_ms / (weight * aero::G0) * aero::MS_TO_FPM).max(300.0)
        };
        let remaining = (target_alt_ft - alt).max(0.0);
        let step_dt = (0.25_f64).min((remaining / rate_fpm).max(1e-4));
        let gs = local.ground_speed(track_deg, tas_kt);
        let d_dist = gs * step_dt / 60.0;
        let thrust_n = if climbing { aero::max_thrust_n(t, alt, mach, local.temp_c) * CLIMB_THRUST_FRACTION } else { aero::max_thrust_n(t, alt, mach, local.temp_c) * 0.05 };
        let d_fuel = aero::fuel_flow_kg_h(t, thrust_n, mach, local.temp_c) / 60.0 * step_dt;
        weight += if climbing { -d_fuel } else { d_fuel };
        weight = weight.max(t.oew_kg * 0.5);
        dist += d_dist;
        time += step_dt;
        alt = (alt + rate_fpm * step_dt).min(target_alt_ft);
        trace.push((dist, time, alt, weight));
    }
    let fuel_kg = if climbing { start_weight_kg - weight } else { weight - start_weight_kg };
    VertResult { dist_nm: dist, time_min: time, fuel_kg: fuel_kg.max(0.0), end_weight_kg: weight, trace }
}

/// Linear interpolation of `(time, alt, weight)` at a distance along a trace built by
/// `integrate_vertical`.
fn interp_trace(trace: &Trace, x: f64) -> (f64, f64, f64) {
    if trace.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    if x <= trace[0].0 {
        let s = trace[0];
        return (s.1, s.2, s.3);
    }
    let last = trace[trace.len() - 1];
    if x >= last.0 {
        return (last.1, last.2, last.3);
    }
    for w in trace.windows(2) {
        if x >= w[0].0 && x <= w[1].0 {
            let span = (w[1].0 - w[0].0).max(1e-9);
            let f = (x - w[0].0) / span;
            return (w[0].1 + (w[1].1 - w[0].1) * f, w[0].2 + (w[1].2 - w[0].2) * f, w[0].3 + (w[1].3 - w[0].3) * f);
        }
    }
    (last.1, last.2, last.3)
}

// ---------------------------------------------------------------------------------
// Flying one route: climb, cruise (with step climbs), descent, and the log.
// ---------------------------------------------------------------------------------

struct FlightResult {
    profile: Vec<ProfilePoint>,
    trip_fuel_kg: f64,
    trip_minutes: f64,
    step_climbs: Vec<(String, f64)>,
    avg_wind_kt: f64,
    avg_isa_dev: f64,
    cruise_level_used: f64,
    total_dist_nm: f64,
}

fn mora_at(pos: crate::dispatch::LatLon) -> Option<f64> {
    let south = pos.0.floor() - 1.0;
    let north = pos.0.ceil() + 1.0;
    let west = pos.1.floor() - 1.0;
    let east = pos.1.ceil() + 1.0;
    crate::sources::navdata::grid_mora(south, north, west, east)
        .into_iter()
        .find(|(lat, lon, _)| *lat <= pos.0 && pos.0 < lat + 1.0 && *lon <= pos.1 && pos.1 < lon + 1.0)
        .map(|(_, _, m)| m)
        .filter(|m| *m > 0.0)
}

/// A synthetic profile point not tied to a filed waypoint: the top of climb, a step climb,
/// the top of descent.
///
/// `weight_kg` is the aircraft's whole gross weight there; `zfw_kg` is what it weighs with
/// no fuel at all, fixed for the flight, so `weight_kg - zfw_kg` is what is actually left in
/// the tanks — not the same number as the gross weight itself, which is what this used to
/// report as `fuel_remaining_kg`.
#[allow(clippy::too_many_arguments)]
fn synthetic_point(ident: &str, kind: ProfileKind, pos: crate::dispatch::LatLon, via: &str, alt_ft: f64, dist_nm: f64, time_min: f64, weight_kg: f64, tow_kg: f64, zfw_kg: f64, track_true_deg: f64, mach: f64, air: Air) -> ProfilePoint {
    let tas_kt = atmosphere::tas_from_mach(mach, air.temp_c);
    let gs_kt = air.ground_speed(track_true_deg, tas_kt);
    ProfilePoint {
        ident: ident.to_string(),
        kind,
        pos,
        via: via.to_string(),
        alt_ft,
        dist_nm,
        time_min,
        fuel_used_kg: (tow_kg - weight_kg).max(0.0),
        fuel_remaining_kg: (weight_kg - zfw_kg).max(0.0),
        gross_kg: weight_kg,
        track_true_deg,
        tas_kt,
        gs_kt,
        mach,
        air,
        mora_ft: mora_at(pos),
    }
}

#[allow(clippy::too_many_arguments)]
/// Make the climb monotonic and honour a cap at a fix for every fix before it.
fn enforce_climb(profile: &mut [crate::dispatch::ProfilePoint], points: &[crate::dispatch::Waypoint], toc_dist: f64) {
    let caps: std::collections::HashMap<&str, f64> = points.iter().filter_map(|w| w.alt_max_ft.map(|m| (w.ident.as_str(), m))).collect();
    let climbing: Vec<usize> = (0..profile.len()).filter(|&i| profile[i].dist_nm <= toc_dist).collect();
    // Backwards: a cap at a fix binds everything before it, because the aeroplane is climbing.
    let mut cap = f64::MAX;
    for &i in climbing.iter().rev() {
        if let Some(&c) = caps.get(profile[i].ident.as_str()) {
            cap = cap.min(c);
        }
        profile[i].alt_ft = profile[i].alt_ft.min(cap);
    }
    // Forwards: never lose height on a departure.
    let mut floor = f64::MIN;
    for &i in &climbing {
        profile[i].alt_ft = profile[i].alt_ft.max(floor);
        floor = profile[i].alt_ft;
    }
}

fn fly_once(t: &TypeData, route: &FiledRoute, air: &dyn WindField, tow_kg: f64, zfw_kg: f64, mach: f64, mut cruise_level: f64, climb_cas: f64, climb_mach: f64, descent_cas: f64, descent_mach: f64, step_ft: f64, step_climbs_on: bool, rvsm: bool, scheme: LevelScheme, start_time: DateTime<Utc>, warnings: &mut Vec<String>) -> FlightResult {
    let points = &route.points;
    let mut route_dist = vec![0.0];
    let mut acc = 0.0;
    for w in points.windows(2) {
        acc += distance_nm(w[0].pos, w[1].pos);
        route_dist.push(acc);
    }
    let total_nm = acc;
    cruise_level = cruise_level.min(t.ceiling_ft).max(1000.0);

    // The climb, from the field to the initial cruise level, sampling the air under the
    // departure since the whole of it is flown close by.
    let initial_track = if points.len() > 1 { bearing_deg(points[0].pos, points[1].pos) } else { 0.0 };
    let climb = integrate_vertical(t, tow_kg, route.origin.elevation_ft, cruise_level, true, climb_cas, climb_mach, points[0].pos, initial_track, air, start_time);
    let toc_dist = climb.dist_nm.min(total_nm);
    let toc_time = climb.time_min;
    let toc_weight = climb.end_weight_kg;

    // The descent, flown backwards from the destination for the same reason: the whole of
    // it is close to the field it ends at.
    let landing_weight_est = (zfw_kg + 1_800.0).min(t.mlw_kg);
    let final_track = if points.len() > 1 { bearing_deg(points[points.len() - 2].pos, points[points.len() - 1].pos) } else { 0.0 };
    let descent = integrate_vertical(t, landing_weight_est, route.destination.elevation_ft, cruise_level, false, descent_cas, descent_mach, points[points.len() - 1].pos, final_track, air, start_time);
    let mut tod_dist = (total_nm - descent.dist_nm).max(0.0);

    if tod_dist < toc_dist {
        // The route is too short to reach the filed level and come back down again: flown
        // as a climb straight into the descent, meeting wherever the two curves cross.
        warnings.push(format!("{:.0} nm is too short to reach FL{:.0} and descend again; flown as a climb straight into the descent", total_nm, cruise_level / 100.0));
        tod_dist = toc_dist;
    }

    // Cruise, from the top of climb to the top of descent, stepping in fine distance
    // increments so wind and temperature are sampled along the actual track, and climbing
    // where the optimum altitude has risen enough to be worth it.
    let mut cruise_trace: Trace = vec![(toc_dist, toc_time, cruise_level, toc_weight)];
    let mut step_climbs: Vec<(String, f64)> = Vec::new();
    let mut step_points: Vec<ProfilePoint> = Vec::new();
    let mut dist = toc_dist;
    let mut time = toc_time;
    let mut weight = toc_weight;
    let mut level = cruise_level;
    const CRUISE_STEP_NM: f64 = 15.0;
    while dist < tod_dist - 0.01 {
        let this_step = CRUISE_STEP_NM.min(tod_dist - dist);
        let pos = pos_along(points, &route_dist, dist);
        let next_pos = pos_along(points, &route_dist, (dist + this_step).min(tod_dist));
        let track = bearing_deg(pos, next_pos);
        let when = start_time + chrono::Duration::milliseconds((time * 60_000.0) as i64);
        let local = air.air(pos, level, when);

        // A step climb only pays for itself if there is enough cruise left afterwards to
        // benefit from it; without this a light aircraft on a short sector can end up
        // staircasing all the way to the top of descent instead of actually cruising.
        let cruise_left_nm = tod_dist - dist;
        if step_climbs_on && cruise_left_nm > 80.0 {
            let opt = aero::optimum_altitude_ft(t, weight, mach);
            if opt >= level + step_ft {
                let buffet = aero::buffet_ceiling_ft(t, weight, mach, 1.3);
                let limit = t.ceiling_ft.min(buffet);
                let candidates = levels_for(track, level + 1.0, limit, rvsm, scheme);
                if let Some(&next_level) = candidates.iter().filter(|&&l| l > level).min_by(|a, b| a.total_cmp(b)) {
                    let climb_here = integrate_vertical(t, weight, level, next_level, true, climb_cas, climb_mach, pos, track, air, when);
                    dist += climb_here.dist_nm;
                    time += climb_here.time_min;
                    weight = climb_here.end_weight_kg;
                    cruise_trace.push((dist, time, next_level, weight));
                    step_points.push(synthetic_point(&format!("STEP{}", step_climbs.len() + 1), ProfileKind::StepClimb, pos_along(points, &route_dist, dist.min(tod_dist)), "", next_level, dist, time, weight, tow_kg, zfw_kg, track, mach, local));
                    step_climbs.push((fix_at(points, &route_dist, dist), next_level));
                    level = next_level;
                    continue;
                }
            }
        }

        let burn_kg_h = aero::cruise_fuel_flow_kg_h(t, weight, level, mach, local.temp_c);
        let tas_kt = atmosphere::tas_from_mach(mach, local.temp_c);
        let gs = local.ground_speed(track, tas_kt).max(30.0);
        let step_time = this_step / gs * 60.0;
        let step_fuel = burn_kg_h / 60.0 * step_time;
        dist += this_step;
        time += step_time;
        weight -= step_fuel;
        cruise_trace.push((dist, time, level, weight));
    }
    let cruise_end_weight = weight;
    let cruise_end_time = time;

    // Assemble the log: every filed waypoint, plus the top of climb, the step climbs and
    // the top of descent.
    let mut profile: Vec<ProfilePoint> = Vec::with_capacity(points.len() + step_points.len() + 2);
    let mut wind_sum = 0.0;
    let mut isa_sum = 0.0;
    let mut n = 0.0;
    for (i, wp) in points.iter().enumerate() {
        let d = route_dist[i];
        let (t_min, mut alt_ft, w_kg) = if d <= toc_dist {
            interp_trace(&climb.trace, d)
        } else if d <= tod_dist {
            interp_trace(&cruise_trace, d)
        } else {
            let from_dest = total_nm - d;
            let (ttg, a, wt) = interp_trace(&descent.trace, from_dest);
            // The descent was integrated backwards from an assumed landing weight
            // (`landing_weight_est`) that has nothing to do with the weight the cruise
            // actually arrived at the top of descent with, so `wt` on its own is not on
            // the same footing as `cruise_end_weight` — using it as the aircraft's weight
            // here is what used to make FUELUSED and FUELREM jump backwards right after
            // TOD. What carries over correctly is the *fuel burned since the top of
            // descent* (`descent.end_weight_kg - wt`, both on the descent's own scale),
            // subtracted from the real weight at TOD.
            let burned_since_tod = (descent.end_weight_kg - wt).max(0.0);
            (cruise_end_time + (descent.time_min - ttg), a, cruise_end_weight - burned_since_tod)
        };
        // A SID or STAR altitude constraint at this fix: loosely respected by pulling the
        // logged altitude to sit inside it, rather than re-flying the climb or descent to
        // meet it exactly.
        if let Some(min_ft) = wp.alt_min_ft {
            alt_ft = alt_ft.max(min_ft);
        }
        if let Some(max_ft) = wp.alt_max_ft {
            alt_ft = alt_ft.min(max_ft);
        }
        let track = if i + 1 < points.len() { bearing_deg(wp.pos, points[i + 1].pos) } else if i > 0 { bearing_deg(points[i - 1].pos, wp.pos) } else { 0.0 };
        let when = start_time + chrono::Duration::milliseconds((t_min * 60_000.0) as i64);
        let local_air = air.air(wp.pos, alt_ft, when);
        let phase_mach = if d <= toc_dist { schedule_mach(t, alt_ft, climb_cas, climb_mach, local_air.temp_c) } else if d <= tod_dist { mach } else { schedule_mach(t, alt_ft, descent_cas, descent_mach, local_air.temp_c) };
        wind_sum += local_air.wind_kt;
        isa_sum += local_air.isa_dev(alt_ft);
        n += 1.0;
        profile.push(synthetic_point(&wp.ident, ProfileKind::Waypoint, wp.pos, &wp.via, alt_ft, d, t_min, w_kg, tow_kg, zfw_kg, track, phase_mach, local_air));
    }
    // The constraints applied point by point above say only where the aeroplane must be at
    // each fix; they do not make the profile between them flyable. A climb that is pulled up
    // to a minimum at one fix and down to a maximum at the next reads as a descent in the
    // middle of a departure, and a cap at a fix says nothing about the fixes before it even
    // though an aeroplane climbing towards that cap must already be below it. Both are fixed
    // by two passes over the climb: backwards, carrying each cap to every fix before it, and
    // then forwards, refusing to let the altitude fall.
    enforce_climb(&mut profile, points, toc_dist);

    // The top of climb and the top of descent, inserted in their place by distance.
    let toc_pos = pos_along(points, &route_dist, toc_dist);
    let toc_air = air.air(toc_pos, cruise_level, start_time + chrono::Duration::milliseconds((toc_time * 60_000.0) as i64));
    profile.push(synthetic_point("TOC", ProfileKind::TopOfClimb, toc_pos, "", cruise_level, toc_dist, toc_time, toc_weight, tow_kg, zfw_kg, initial_track, mach, toc_air));
    let tod_pos = pos_along(points, &route_dist, tod_dist);
    let tod_air = air.air(tod_pos, level, start_time + chrono::Duration::milliseconds((cruise_end_time * 60_000.0) as i64));
    profile.push(synthetic_point("TOD", ProfileKind::TopOfDescent, tod_pos, "", level, tod_dist, cruise_end_time, cruise_end_weight, tow_kg, zfw_kg, final_track, mach, tod_air));
    profile.extend(step_points);
    profile.sort_by(|a, b| a.dist_nm.total_cmp(&b.dist_nm));

    let trip_minutes = cruise_end_time + descent.time_min;
    let landing_weight_kg = cruise_end_weight - descent.fuel_kg;
    let trip_fuel_kg = tow_kg - landing_weight_kg;

    FlightResult {
        profile,
        trip_fuel_kg: trip_fuel_kg.max(0.0),
        trip_minutes,
        step_climbs,
        avg_wind_kt: if n > 0.0 { wind_sum / n } else { 0.0 },
        avg_isa_dev: if n > 0.0 { isa_sum / n } else { 0.0 },
        cruise_level_used: cruise_level,
        total_dist_nm: total_nm,
    }
}

/// The fix a distance along the route is at, for naming a step climb. A step is read off the
/// plan against the route, so it is named for the nearest fix rather than described by how far
/// along it happens: a crew steps at MESAN, not at "1,204 nm from the origin".
fn fix_at(points: &[crate::dispatch::Waypoint], route_dist: &[f64], dist_nm: f64) -> String {
    points
        .iter()
        .zip(route_dist)
        .filter(|(w, _)| !w.ident.is_empty())
        .min_by(|a, b| (a.1 - dist_nm).abs().total_cmp(&(b.1 - dist_nm).abs()))
        .map(|(w, _)| w.ident.clone())
        .unwrap_or_else(|| format!("{dist_nm:.0}NM"))
}

/// The position at a distance along the filed route's own points (not a trace).
fn pos_along(points: &[crate::dispatch::Waypoint], route_dist: &[f64], dist_nm: f64) -> crate::dispatch::LatLon {
    for w in route_dist.windows(2).enumerate() {
        let (i, pair) = w;
        if dist_nm >= pair[0] && dist_nm <= pair[1] {
            let span = (pair[1] - pair[0]).max(1e-9);
            return along(points[i].pos, points[i + 1].pos, (dist_nm - pair[0]) / span);
        }
    }
    points.last().map(|p| p.pos).unwrap_or((0.0, 0.0))
}

// ---------------------------------------------------------------------------------
// The whole plan.
// ---------------------------------------------------------------------------------

/// A rough first guess at trip fuel, to seed the take-off weight the profile is first flown
/// at: the great-circle distance at the chosen Mach, burnt at a mid-weight cruise rate.
fn rough_trip_fuel_kg(t: &TypeData, zfw_kg: f64, trip_nm: f64, mach: f64) -> f64 {
    let rep_weight = ((zfw_kg + t.mtow_kg) / 2.0).clamp(t.oew_kg, t.mtow_kg);
    let level = aero::optimum_altitude_ft(t, rep_weight, mach).min(t.ceiling_ft);
    let temp_c = crate::dispatch::isa_temp_c(level);
    let tas = atmosphere::tas_from_mach(mach, temp_c).max(100.0);
    let burn = aero::cruise_fuel_flow_kg_h(t, rep_weight, level, mach, temp_c);
    burn * trip_nm / tas
}

fn holding_burn_kg_min(t: &TypeData, weight_kg: f64, alt_ft: f64) -> f64 {
    let cas = (t.vmo_kt * 0.55).clamp(150.0, 230.0);
    let temp_c = crate::dispatch::isa_temp_c(alt_ft);
    let mach = atmosphere::mach_from_cas(cas, alt_ft, temp_c);
    aero::cruise_fuel_flow_kg_h(t, weight_kg, alt_ft, mach, temp_c) / 60.0
}

/// Fly a route: the profile, the fuel and the weights.
pub fn plan(req: &PerfRequest) -> anyhow::Result<PerfPlan> {
    if req.route.points.len() < 2 {
        anyhow::bail!("a route needs at least two points to fly");
    }
    let base = aircraft::lookup(&req.spec.icao_type).ok_or_else(|| anyhow::anyhow!("unknown aircraft type {}", req.spec.icao_type))?;
    let t = base.with_spec(req.spec);
    let mut warnings: Vec<String> = Vec::new();

    let zfw_target = (t.oew_kg + req.payload_kg.max(0.0)).min(t.mzfw_kg);
    let mach = req.cruise.mach.unwrap_or_else(|| econ_mach(&t, req.cruise.cost_index));
    let climb_cas = (t.vmo_kt - 30.0).clamp(180.0, 320.0);
    let climb_mach = (t.cruise_mach - 0.02).max(0.45).min(mach);
    let descent_cas = climb_cas;
    let descent_mach = climb_mach.max(mach - 0.02);

    let trip_nm = req.route.distance_nm();
    // A short hop cannot climb all the way to its theoretical optimum and still have room
    // to come down again; this is the same rule of thumb a dispatcher uses picking an
    // initial level for a short sector, not a substitute for the climb/descent physics
    // (which still decide exactly how far up the flight gets).
    let distance_cap_ft = match trip_nm {
        d if d < 150.0 => 21_000.0,
        d if d < 300.0 => 27_000.0,
        d if d < 500.0 => 33_000.0,
        d if d < 800.0 => 36_000.0,
        // A sector of eight hundred to twelve hundred miles is flown in the middle thirties,
        // not at the aeroplane's ceiling: the climb and descent take too large a share of it
        // for the last few thousand feet to pay for themselves. London to Rome is eight
        // hundred and forty-five miles and is flown at FL360 to FL380, not FL390.
        d if d < 1200.0 => 38_000.0,
        _ => f64::MAX,
    };
    // The cap applies whichever way the level got here: a level the route search already
    // chose is just as capable of being too high for the distance as one worked out fresh
    // here, since the search picks from candidates built around the aircraft's usual
    // cruise (`nominal_cruise_ft`) with no notion of how short this particular trip is.
    let requested_level = req.route.cruise_ft;
    let initial_level = if requested_level > 1000.0 {
        requested_level
    } else {
        aero::optimum_altitude_ft(&t, zfw_target * 1.15, mach)
    }
    .min(distance_cap_ft)
    .min(t.ceiling_ft);

    let mut tow_kg = (zfw_target + rough_trip_fuel_kg(&t, zfw_target, trip_nm, mach) * 1.12).min(t.mtow_kg);

    let mut flight = fly_once(&t, req.route, req.air, tow_kg, zfw_target, mach, initial_level, climb_cas, climb_mach, descent_cas, descent_mach, req.cruise.step_ft, req.cruise.step_climbs, req.rvsm, req.scheme, req.route.off_block, &mut warnings);
    for _ in 0..5 {
        let candidate_tow = (zfw_target + flight.trip_fuel_kg * 1.12).min(t.mtow_kg);
        if (candidate_tow - tow_kg).abs() < 20.0 {
            break;
        }
        tow_kg = candidate_tow;
        warnings.clear();
        flight = fly_once(&t, req.route, req.air, tow_kg, zfw_target, mach, initial_level, climb_cas, climb_mach, descent_cas, descent_mach, req.cruise.step_ft, req.cruise.step_climbs, req.rvsm, req.scheme, req.route.off_block, &mut warnings);
    }

    // The alternate: a small profile of its own, at a lower level, starting when the
    // primary flight is expected to land.
    let landed_at = req.route.off_block + chrono::Duration::milliseconds((flight.trip_minutes * 60_000.0) as i64);
    let alternate_plan = req.alternate.map(|alt_route| {
        let alt_zfw = zfw_target;
        let alt_level = aero::optimum_altitude_ft(&t, alt_zfw * 1.05, mach * 0.9).min(25_000.0).min(t.ceiling_ft).max(8_000.0);
        let alt_tow = (alt_zfw + 3_000.0).min(t.mtow_kg);
        let mut throwaway = Vec::new();
        let alt_flight = fly_once(&t, alt_route, req.air, alt_tow, alt_zfw, (mach * 0.9).max(0.5), alt_level, climb_cas, climb_mach, descent_cas, descent_mach, req.cruise.step_ft, false, req.rvsm, req.scheme, landed_at, &mut throwaway);
        AlternatePlan { icao: alt_route.destination.icao.clone(), dist_nm: alt_flight.total_dist_nm, time_min: alt_flight.trip_minutes, fuel_kg: alt_flight.trip_fuel_kg, cruise_ft: alt_flight.cruise_level_used }
    });

    // Fuel: the trip is what was just flown; contingency's floor and the final reserve are
    // costed at the burn rates a hold would actually fly at.
    let contingency_rate = if flight.trip_minutes > 0.1 { flight.trip_fuel_kg / flight.trip_minutes } else { holding_burn_kg_min(&t, zfw_target + 2000.0, 20_000.0) };
    let reserve_weight = zfw_target + req.fuel.final_reserve_min * 20.0; // a light guess at what is left when the hold starts
    let final_reserve_rate = holding_burn_kg_min(&t, reserve_weight.min(t.mlw_kg), 1_500.0);
    let rates = HoldRates { contingency_kg_min: contingency_rate, final_reserve_kg_min: final_reserve_rate };

    let tanker = match (req.fuel.price_origin, req.fuel.price_destination) {
        (Some(po), Some(pd)) => {
            // How much extra a kilogram carried the length of the trip costs: the model's
            // own weight sensitivity of cruise burn, sampled at the flown weight and level.
            let base = aero::cruise_fuel_flow_kg_h(&t, tow_kg, flight.cruise_level_used, mach, crate::dispatch::isa_temp_c(flight.cruise_level_used));
            let bumped = aero::cruise_fuel_flow_kg_h(&t, tow_kg + 1000.0, flight.cruise_level_used, mach, crate::dispatch::isa_temp_c(flight.cruise_level_used));
            let extra_burn_per_1000kg = (bumped - base) * (trip_nm / atmosphere::tas_from_mach(mach, crate::dispatch::isa_temp_c(flight.cruise_level_used)).max(100.0));
            let penalty_fraction = (extra_burn_per_1000kg / 1000.0).clamp(0.0, 0.5);
            let capacity_left = (t.max_fuel_kg - (flight.trip_fuel_kg + req.fuel.taxi_kg)).max(0.0);
            Some(TankerInputs { price_origin: po, price_destination: pd, penalty_fraction, capacity_left_kg: capacity_left })
        }
        _ => None,
    };

    let (fuel_breakdown, fuel_warnings) = fuel::compute(&req.fuel, flight.trip_fuel_kg, &rates, alternate_plan.as_ref().map(|a| a.fuel_kg), tanker.as_ref());
    warnings.extend(fuel_warnings);

    let (final_weights, weight_warnings) = weights::compute(req.spec, req.payload_kg, &fuel_breakdown);
    warnings.extend(weight_warnings);

    // Equal-time points, for each consecutive pair of the airports given, both for an
    // engine failure (at the one-engine speed, where the type has one) and for a
    // depressurisation (a descent to FL100).
    let mut route_dist = vec![0.0];
    let mut acc = 0.0;
    for w in req.route.points.windows(2) {
        acc += distance_nm(w[0].pos, w[1].pos);
        route_dist.push(acc);
    }
    let polyline: Vec<(f64, crate::dispatch::LatLon)> = req.route.points.iter().zip(route_dist.iter()).map(|(p, d)| (*d, p.pos)).collect();
    let eta_at = |d: f64| -> DateTime<Utc> {
        let frac = (d / trip_nm.max(0.01)).clamp(0.0, 1.0);
        req.route.off_block + chrono::Duration::milliseconds((frac * flight.trip_minutes * 60_000.0) as i64)
    };
    let mut equal_time_points: Vec<EqualTimePoint> = Vec::new();
    if req.etp_airports.len() >= 2 {
        let engine_out_tas = req.spec.one_engine_tas_kt.unwrap_or(mach_tas(&t, mach));
        let engine_out_burn = holding_burn_kg_min(&t, zfw_target + 5000.0, 10_000.0) * 1.4;
        let depress_tas = mach_tas(&t, (mach * 0.75).max(0.5));
        let depress_burn = holding_burn_kg_min(&t, zfw_target + 5000.0, 10_000.0);
        for pair in req.etp_airports.windows(2) {
            let engine_case = etp::EtpCase { tas_kt: engine_out_tas, level_ft: 10_000.0, burn_kg_min: engine_out_burn };
            if let Some(p) = etp::equal_time_point(&polyline, &pair[0], &pair[1], req.air, &eta_at, &engine_case) {
                equal_time_points.push(p);
            }
            let depress_case = etp::EtpCase { tas_kt: depress_tas, level_ft: 10_000.0, burn_kg_min: depress_burn };
            if let Some(p) = etp::equal_time_point(&polyline, &pair[0], &pair[1], req.air, &eta_at, &depress_case) {
                equal_time_points.push(p);
            }
        }
    }

    // The point of no return for the route as a whole: where turning back to the origin
    // takes as long, at normal cruise speed, as pressing on to the destination. Like the
    // equal-time points above, this only means something where a diversion is genuinely
    // far away — `req.etp_airports` is how `ofp` tells this module the flight is oceanic or
    // under an ETOPS rule, and an empty list is how it says it is not.
    let pnr_case = etp::EtpCase { tas_kt: mach_tas(&t, mach), level_ft: flight.cruise_level_used, burn_kg_min: contingency_rate };
    let no_return = if req.etp_airports.len() >= 2 {
        etp::find(&polyline, req.route.origin.pos, req.route.destination.pos, req.air, &eta_at, &pnr_case).map(|(dist_nm, pos, time_min)| {
            let when = eta_at(dist_nm);
            let local = req.air.air(pos, flight.cruise_level_used, when);
            let frac = (dist_nm / trip_nm.max(0.01)).clamp(0.0, 1.0);
            let weight_kg = tow_kg - flight.trip_fuel_kg * frac;
            synthetic_point("PNR", ProfileKind::NoReturn, pos, "", flight.cruise_level_used, dist_nm, time_min, weight_kg, tow_kg, zfw_target, bearing_deg(req.route.origin.pos, req.route.destination.pos), mach, local)
        })
    } else {
        None
    };

    Ok(PerfPlan {
        profile: flight.profile,
        fuel: fuel_breakdown,
        weights: final_weights,
        step_climbs: flight.step_climbs,
        avg_wind_kt: flight.avg_wind_kt,
        avg_isa_dev: flight.avg_isa_dev,
        equal_time_points,
        no_return,
        alternate: alternate_plan,
        warnings,
    })
}

fn mach_tas(t: &TypeData, mach: f64) -> f64 {
    atmosphere::tas_from_mach(mach, crate::dispatch::isa_temp_c((t.ceiling_ft * 0.7).min(30_000.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{AircraftSpec, Airport, CruisePolicy, FuelPolicy, PointKind, StillAir, Waypoint};

    fn simple_route(icao_from: &str, from: crate::dispatch::LatLon, icao_to: &str, to: crate::dispatch::LatLon, cruise_ft: f64) -> FiledRoute {
        let mid = along(from, to, 0.5);
        FiledRoute {
            origin: Airport { icao: icao_from.to_string(), name: String::new(), pos: from, elevation_ft: 500.0 },
            destination: Airport { icao: icao_to.to_string(), name: String::new(), pos: to, elevation_ft: 500.0 },
            dep_runway: None,
            sid: None,
            sid_transition: None,
            star: None,
            star_transition: None,
            arr_runway: None,
            approach: None,
            points: vec![
                Waypoint::new(icao_from, from, "DCT", PointKind::Airport),
                Waypoint::new("MID", mid, "DCT", PointKind::Enroute),
                Waypoint::new(icao_to, to, "", PointKind::Airport),
            ],
            cruise_ft,
            off_block: chrono::Utc::now(),
        }
    }

    fn request<'a>(route: &'a FiledRoute, spec: &'a AircraftSpec, air: &'a dyn WindField) -> PerfRequest<'a> {
        PerfRequest { spec, route, alternate: None, air, payload_kg: spec.mzfw_kg - spec.oew_kg, cruise: CruisePolicy::default(), fuel: FuelPolicy::default(), etp_airports: &[], scheme: LevelScheme::EastOdd, rvsm: true }
    }

    #[test]
    fn fuel_time_and_distance_all_rise_with_distance() {
        let spec = aircraft::lookup("A320").unwrap().to_spec();
        let still = StillAir;
        let mut prev_fuel = 0.0;
        let mut prev_time = 0.0;
        for nm in [200.0, 600.0, 1400.0] {
            let to = crate::dispatch::travel((51.5, 0.0), 90.0, nm);
            let route = simple_route("EGLL", (51.5, 0.0), "DEST", to, 35_000.0);
            let req = request(&route, &spec, &still);
            let plan = plan(&req).expect("a plan");
            assert!(plan.fuel.trip_kg > prev_fuel, "{nm}: {} vs {prev_fuel}", plan.fuel.trip_kg);
            let arrival_min = plan.profile.iter().map(|p| p.time_min).fold(0.0, f64::max);
            assert!(arrival_min > prev_time, "{nm}: {arrival_min} vs {prev_time}");
            prev_fuel = plan.fuel.trip_kg;
            prev_time = arrival_min;
        }
    }

    #[test]
    fn a_long_flight_steps_and_a_short_one_does_not() {
        let spec = aircraft::lookup("A359").unwrap().to_spec();
        let still = StillAir;

        let short_to = crate::dispatch::travel((51.5, 0.0), 90.0, 600.0);
        let short_route = simple_route("EGLL", (51.5, 0.0), "DEST", short_to, 35_000.0);
        let short_req = request(&short_route, &spec, &still);
        let short_plan = plan(&short_req).expect("a short plan");

        let long_to = crate::dispatch::travel((51.5, 0.0), 90.0, 5_500.0);
        let long_route = simple_route("EGLL", (51.5, 0.0), "DEST", long_to, 33_000.0);
        let long_req = request(&long_route, &spec, &still);
        let long_plan = plan(&long_req).expect("a long plan");

        assert!(long_plan.step_climbs.len() >= short_plan.step_climbs.len());
        assert!(!long_plan.step_climbs.is_empty(), "a 5,500 nm A350 flight should step climb at least once");
    }

    #[test]
    fn profile_points_carry_a_full_navigation_log() {
        let spec = aircraft::lookup("B738").unwrap().to_spec();
        let still = StillAir;
        let to = crate::dispatch::travel((40.0, -73.8), 45.0, 900.0);
        let route = simple_route("KJFK", (40.0, -73.8), "DEST", to, 37_000.0);
        let req = request(&route, &spec, &still);
        let plan = plan(&req).expect("a plan");
        assert!(plan.profile.iter().any(|p| p.kind == ProfileKind::TopOfClimb));
        assert!(plan.profile.iter().any(|p| p.kind == ProfileKind::TopOfDescent));
        assert!(plan.profile.windows(2).all(|w| w[0].dist_nm <= w[1].dist_nm + 1e-6));
        assert!(plan.profile.windows(2).all(|w| w[0].time_min <= w[1].time_min + 1e-6));
    }

    /// Calibration: trip fuel and flight time against a publicly reported figure for the
    /// type over roughly the given distance. Fuel is checked tightly (12%): the type table's
    /// cruise TSFC is a single scalar per type calibrated directly against this figure, so
    /// there is no excuse for it drifting far. Time is checked loosely (25%): this model
    /// flies a great-circle direct routing at a fixed econ Mach with no vectoring, holding,
    /// step-off or ATC-driven speed restriction, none of which the TSFC knob can correct
    /// for, and the reference block times bundle in all of that real-world slack. The
    /// reference figures themselves are the widely reported average cruise fuel flow and
    /// block-time figures for each type at roughly this stage length (airline and
    /// manufacturer fuel-burn literature, general aviation-press consensus, cross-checked
    /// where possible against a published flight-plan analysis) rather than a single cited
    /// flight, since no licensed performance chart was used to produce them.
    fn calibration_case(icao: &str, nm: f64, payload_frac: f64, cruise_ft: f64, ref_fuel_kg: f64, ref_minutes: f64) {
        let spec = aircraft::lookup(icao).unwrap().to_spec();
        let still = StillAir;
        let to = crate::dispatch::travel((51.5, 0.0), 90.0, nm);
        let route = simple_route("EGLL", (51.5, 0.0), "DEST", to, cruise_ft);
        let mut req = request(&route, &spec, &still);
        req.payload_kg = (spec.mzfw_kg - spec.oew_kg) * payload_frac;
        let plan = plan(&req).expect("a plan");
        let flight_min = plan.profile.iter().map(|p| p.time_min).fold(0.0, f64::max);
        let fuel_err = (plan.fuel.trip_kg - ref_fuel_kg).abs() / ref_fuel_kg;
        let time_err = (flight_min - ref_minutes).abs() / ref_minutes;
        const FUEL_TOLERANCE: f64 = 0.12;
        const TIME_TOLERANCE: f64 = 0.25;
        assert!(fuel_err <= FUEL_TOLERANCE, "{icao}/{nm}nm: trip fuel {:.0} kg vs reference {ref_fuel_kg:.0} kg ({:.0}% off, tolerance {:.0}%)", plan.fuel.trip_kg, fuel_err * 100.0, FUEL_TOLERANCE * 100.0);
        assert!(time_err <= TIME_TOLERANCE, "{icao}/{nm}nm: flight time {flight_min:.0} min vs reference {ref_minutes:.0} min ({:.0}% off, tolerance {:.0}%)", time_err * 100.0, TIME_TOLERANCE * 100.0);
    }

    // A20N, ~200 nm: a short domestic sector. Reference: roughly 1,450 kg trip fuel from
    // the widely reported ~2,300-2,400 kg/h total A320neo cruise burn (theflyingengineer.com,
    // pilotrise.com), and 38 minutes airborne.
    #[test]
    fn calibration_a20n_200nm() {
        calibration_case("A20N", 200.0, 0.60, 0.0, 1_450.0, 38.0);
    }

    // B738, ~1,000 nm: a typical medium-haul sector at a published ~2,500-2,600 kg/h
    // (850 USG/h) cruise burn (flyawaysimulation.com; 737 Airplane Characteristics
    // literature), roughly 5,800 kg trip fuel and 2 h 30 in the air. This is the figure the
    // task that tightened this calibration named directly: the model used to land at 4,778
    // kg here, 18% light.
    #[test]
    fn calibration_b738_1000nm() {
        calibration_case("B738", 1_000.0, 0.70, 0.0, 5_800.0, 150.0);
    }

    // A359, ~4,500 nm: a long-haul sector at a widely reported ~5,700-6,200 kg/h A350-900
    // cruise burn, roughly 56 tonnes trip fuel and just under 10 hours in the air.
    #[test]
    fn calibration_a359_4500nm() {
        calibration_case("A359", 4_500.0, 0.75, 0.0, 56_000.0, 590.0);
    }

    // B77W, ~5,500 nm: an ultra-long-haul sector at a widely reported ~7,500-7,900 kg/h
    // 777-300ER cruise burn, cross-checked against Aircraft Commerce's published GE90-115B
    // LHR-NRT/NRT-LHR block fuel from Jeppesen flight plans (Issue 60, Oct/Nov 2008):
    // 26,901-29,468 USG (roughly 81,500-89,300 kg) over 5,200-5,471 nm ESAD, which includes
    // taxi and reserves this model's trip fuel does not — consistent with this model's
    // smaller trip-only figure sitting a little under that range. Roughly 88 tonnes trip
    // fuel and about 11 h 40 in the air.
    #[test]
    fn calibration_b77w_5500nm() {
        calibration_case("B77W", 5_500.0, 0.75, 0.0, 88_000.0, 700.0);
    }

    // A388, ~7,000 nm: the longest missions the A380 flew, at a widely reported
    // ~11,000-12,000 kg/h cruise burn, roughly 155 tonnes trip fuel and about 14 hours in
    // the air.
    #[test]
    fn calibration_a388_7000nm() {
        calibration_case("A388", 7_000.0, 0.75, 0.0, 155_000.0, 840.0);
    }
}
