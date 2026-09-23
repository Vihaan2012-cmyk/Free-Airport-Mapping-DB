//! Equal-time points and the point of no return: where, flown at a given speed and level
//! with the wind as forecast, the time to one place equals the time to another.
//!
//! Both are the same problem — a zero-crossing search along the route for `time to A minus
//! time to B` — so `find` does the geometry and `perf::profile` supplies the two ends, the
//! speed and the fuel it is worth.

use crate::dispatch::{along, bearing_deg, distance_nm, Airport, EqualTimePoint, LatLon, WindField};
use chrono::{DateTime, Utc};

/// The speed, level and burn rate an equal-time or no-return search is flown at.
#[derive(Debug, Clone, Copy)]
pub struct EtpCase {
    pub tas_kt: f64,
    pub level_ft: f64,
    pub burn_kg_min: f64,
}

fn time_to_min(pos: LatLon, when: DateTime<Utc>, target: LatLon, air: &dyn WindField, case: &EtpCase) -> f64 {
    let d = distance_nm(pos, target);
    if d < 0.01 {
        return 0.0;
    }
    let a = air.air(along(pos, target, 0.5), case.level_ft, when);
    let gs = a.ground_speed(bearing_deg(pos, target), case.tas_kt);
    d / gs * 60.0
}

/// A point along the route by the distance flown to reach it, interpolated between the
/// nearest two the route was given at.
pub(crate) fn pos_at(route: &[(f64, LatLon)], dist_nm: f64) -> LatLon {
    for w in route.windows(2) {
        if dist_nm >= w[0].0 && dist_nm <= w[1].0 {
            let span = w[1].0 - w[0].0;
            let frac = if span > 0.0 { (dist_nm - w[0].0) / span } else { 0.0 };
            return along(w[0].1, w[1].1, frac);
        }
    }
    route.last().map(|w| w.1).unwrap_or((0.0, 0.0))
}

/// The distance along the route, the place, and the time from it to `a`, where flying on to
/// `a` takes exactly as long as flying on to `b`. `eta_at` turns a distance flown into the
/// time the aircraft would be there, for the wind to be sampled at the right moment.
///
/// `None` where the route never crosses over — `a` is always nearer, or always farther,
/// along the whole of it.
pub fn find(route: &[(f64, LatLon)], a: LatLon, b: LatLon, air: &dyn WindField, eta_at: &dyn Fn(f64) -> DateTime<Utc>, case: &EtpCase) -> Option<(f64, LatLon, f64)> {
    if route.len() < 2 {
        return None;
    }
    let total = route.last()?.0;
    if total < 0.01 {
        return None;
    }
    let g = |d: f64| -> f64 {
        let pos = pos_at(route, d);
        let when = eta_at(d);
        time_to_min(pos, when, a, air, case) - time_to_min(pos, when, b, air, case)
    };

    const SAMPLES: usize = 96;
    let mut lo = 0.0;
    let mut lo_g = g(0.0);
    let mut bracket = None;
    for i in 1..=SAMPLES {
        let d = total * i as f64 / SAMPLES as f64;
        let gd = g(d);
        if lo_g == 0.0 || gd == 0.0 || lo_g.signum() != gd.signum() {
            bracket = Some((lo, d));
            break;
        }
        lo = d;
        lo_g = gd;
    }
    let (mut a0, mut b0) = bracket?;
    let mut ga = g(a0);
    for _ in 0..48 {
        let mid = (a0 + b0) / 2.0;
        let gm = g(mid);
        if gm == 0.0 {
            a0 = mid;
            b0 = mid;
            break;
        }
        if gm.signum() == ga.signum() {
            a0 = mid;
            ga = gm;
        } else {
            b0 = mid;
        }
    }
    let d = (a0 + b0) / 2.0;
    let pos = pos_at(route, d);
    let when = eta_at(d);
    let time_min = time_to_min(pos, when, a, air, case);
    Some((d, pos, time_min))
}

/// The equal-time point between two airports, as `PerfPlan` reports it: the fuel figure is
/// the larger of the time to each, so a residual asymmetry from the search never understates
/// what is needed.
pub fn equal_time_point(route: &[(f64, LatLon)], a: &Airport, b: &Airport, air: &dyn WindField, eta_at: &dyn Fn(f64) -> DateTime<Utc>, case: &EtpCase) -> Option<EqualTimePoint> {
    let (dist_nm, pos, _) = find(route, a.pos, b.pos, air, eta_at, case)?;
    let when = eta_at(dist_nm);
    let to_a = time_to_min(pos, when, a.pos, air, case);
    let to_b = time_to_min(pos, when, b.pos, air, case);
    let time_min = to_a.max(to_b);
    Some(EqualTimePoint { between: (a.icao.clone(), b.icao.clone()), pos, dist_nm, time_min, fuel_needed_kg: time_min * case.burn_kg_min })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{Air, StillAir};

    fn straight_route(a: LatLon, b: LatLon, n: usize) -> Vec<(f64, LatLon)> {
        let total = distance_nm(a, b);
        (0..=n).map(|i| (total * i as f64 / n as f64, along(a, b, i as f64 / n as f64))).collect()
    }

    #[test]
    fn still_air_lands_the_equal_time_point_on_the_midpoint() {
        let a = Airport { icao: "AAAA".into(), name: String::new(), pos: (0.0, 0.0), elevation_ft: 0.0 };
        let b = Airport { icao: "BBBB".into(), name: String::new(), pos: (0.0, 10.0), elevation_ft: 0.0 };
        let route = straight_route(a.pos, b.pos, 40);
        let case = EtpCase { tas_kt: 450.0, level_ft: 35_000.0, burn_kg_min: 60.0 };
        let etp = equal_time_point(&route, &a, &b, &StillAir, &|_| Utc::now(), &case).expect("a crossing");
        let mid_nm = distance_nm(a.pos, b.pos) / 2.0;
        assert!((etp.dist_nm - mid_nm).abs() < mid_nm * 0.02, "{} vs {mid_nm}", etp.dist_nm);
    }

    /// A wind straight down the route, from `a` towards `b`: flying on to `b` gets a
    /// tailwind boost that flying back to `a` does not, so the crossing should move towards
    /// `a` — the point moves upwind of the midpoint, matching the textbook rule.
    struct Tailwind;
    impl WindField for Tailwind {
        fn air(&self, _at: LatLon, alt_ft: f64, _when: DateTime<Utc>) -> Air {
            Air { wind_from_deg: 270.0, wind_kt: 80.0, temp_c: crate::dispatch::isa_temp_c(alt_ft) }
        }
    }

    #[test]
    fn a_wind_moves_the_equal_time_point_upwind() {
        let a = Airport { icao: "AAAA".into(), name: String::new(), pos: (0.0, 0.0), elevation_ft: 0.0 };
        let b = Airport { icao: "BBBB".into(), name: String::new(), pos: (0.0, 10.0), elevation_ft: 0.0 };
        let route = straight_route(a.pos, b.pos, 40);
        let case = EtpCase { tas_kt: 300.0, level_ft: 35_000.0, burn_kg_min: 40.0 };
        let etp = equal_time_point(&route, &a, &b, &Tailwind, &|_| Utc::now(), &case).expect("a crossing");
        let mid_nm = distance_nm(a.pos, b.pos) / 2.0;
        // A tailwind flying east helps the run on to B more than the turn-back to A, so the
        // point where times are equal sits closer to A (west) than the midpoint.
        assert!(etp.dist_nm < mid_nm, "{} should be west of the midpoint {mid_nm}", etp.dist_nm);
    }
}
