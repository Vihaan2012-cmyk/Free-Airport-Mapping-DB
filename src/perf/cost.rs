//! What a leg costs the route search: `dispatch::CostModel`, built once per route and then
//! called millions of times as the search explores it, so everything it needs is worked out
//! in advance.
//!
//! A level's table is a handful of fuel-flow samples across the weight the aircraft could
//! plausibly be at (from empty to maximum take-off weight), at the Mach the cost index or the
//! fixed speed gives. `leg` looks the weight up, interpolates, asks the wind field for the
//! air at the leg's middle, and is done — no allocation, no iteration, no search of its own.

use crate::dispatch::{along, bearing_deg, distance_nm, CostModel, LegCost, LegQuery, WindField};
use crate::perf::aircraft::TypeData;
use crate::perf::aero;
use crate::perf::atmosphere;

/// The ECON Mach a cost index gives: it rises from a fuel-economy speed at cost index zero
/// towards the type's overspeed limit as the cost index rises, the shape every FMS's ECON
/// mode follows even though the exact curve is proprietary to each one. Cost index is taken
/// on the usual 0-100 scale (kg/min of fuel value per 100 lb/hr, roughly).
pub fn econ_mach(t: &TypeData, cost_index: Option<f64>) -> f64 {
    match cost_index {
        None => t.cruise_mach,
        Some(ci) => {
            let ci = ci.clamp(0.0, 100.0);
            let low = (t.cruise_mach - 0.06).max(0.45);
            let high = (t.mmo - 0.02).min(t.cruise_mach + 0.04).max(low + 0.01);
            low + (high - low) * (ci / 100.0)
        }
    }
}

/// How many samples of weight a level's table carries between empty and maximum take-off
/// weight. Coarse is fine: the search only ever needs an interpolated figure, not an exact
/// one, and a wider table costs more to build without buying more accuracy than the rest of
/// the model has anyway.
const WEIGHT_BINS: usize = 17;

struct LevelTable {
    /// True airspeed at this level and Mach on a standard day, knots. Independent of
    /// weight, so unlike burn it needs no table.
    tas_kt: f64,
    /// Fuel burn, kilograms per hour, at `WEIGHT_BINS` evenly spaced weights from the
    /// type's empty weight to its maximum take-off weight, standard-day temperature.
    burn_kg_h: [f64; WEIGHT_BINS],
    /// The least this level could possibly burn, at any weight the aircraft could be at,
    /// any Mach it could be flown at: the true minimum, not achieved at the flown Mach.
    burn_bound_kg_h: f64,
    /// The fastest ground speed physically available at this level: the true airspeed at
    /// the type's overspeed limit, on a day warm enough to be generous, plus a tailwind no
    /// forecast is ever likely to beat.
    gs_bound_kt: f64,
}

impl LevelTable {
    fn burn_at(&self, weight_kg: f64, oew_kg: f64, mtow_kg: f64) -> f64 {
        let span = (mtow_kg - oew_kg).max(1.0);
        let frac = ((weight_kg - oew_kg) / span).clamp(0.0, 1.0) * (WEIGHT_BINS - 1) as f64;
        let lo = frac.floor() as usize;
        let hi = (lo + 1).min(WEIGHT_BINS - 1);
        let t = frac - lo as f64;
        self.burn_kg_h[lo] * (1.0 - t) + self.burn_kg_h[hi] * t
    }
}

/// A generous ceiling on the tailwind a `lower_bound` may rely on. Recorded jet-stream cores
/// rarely exceed 250 kt; this leaves real headroom above that so a strong but plausible
/// forecast in `WindField` can never be mistaken for one the model could not have foreseen.
const MAX_PLAUSIBLE_TAILWIND_KT: f64 = 260.0;

/// How much warmer than standard the bound assumes the day might be, which raises the speed
/// of sound and so the fastest true airspeed the type could show for a given Mach limit.
const BOUND_ISA_DEV_C: f64 = 25.0;

pub struct PerfCostModel<'a> {
    t: TypeData,
    air: &'a dyn WindField,
    zfw_kg: f64,
    trip_nm: f64,
    trip_fuel_kg: f64,
    mach: f64,
    levels: Vec<LevelTable>,
}

impl<'a> PerfCostModel<'a> {
    pub fn build(t: TypeData, air: &'a dyn WindField, zfw_kg: f64, trip_nm: f64, cost_index: Option<f64>) -> PerfCostModel<'a> {
        let mach = econ_mach(&t, cost_index);
        let max_level_1000 = (t.ceiling_ft / 1000.0).ceil() as i64;
        let mut levels = Vec::with_capacity((max_level_1000 + 1) as usize);
        for i in 0..=max_level_1000 {
            let level_ft = (i * 1000) as f64;
            let temp_c = crate::dispatch::isa_temp_c(level_ft);
            let tas_kt = atmosphere::tas_from_mach(mach, temp_c);
            let mut burn_kg_h = [0.0; WEIGHT_BINS];
            for (k, slot) in burn_kg_h.iter_mut().enumerate() {
                let w = t.oew_kg + (t.mtow_kg - t.oew_kg) * (k as f64 / (WEIGHT_BINS - 1) as f64);
                *slot = aero::cruise_fuel_flow_kg_h(&t, w, level_ft, mach, temp_c);
            }
            // The true minimum burn at this level: scan every Mach the type could fly, at
            // its emptiest weight, since both push burn down together.
            let mut burn_bound_kg_h = f64::MAX;
            let mut mach_scan = 0.35;
            while mach_scan <= t.mmo {
                let ff = aero::cruise_fuel_flow_kg_h(&t, t.oew_kg, level_ft, mach_scan, temp_c);
                if ff < burn_bound_kg_h {
                    burn_bound_kg_h = ff;
                }
                mach_scan += 0.01;
            }
            let warm_temp_c = temp_c + BOUND_ISA_DEV_C;
            let fastest_tas = atmosphere::tas_from_mach(t.mmo, warm_temp_c).max(atmosphere::tas_from_cas(t.vmo_kt, level_ft, warm_temp_c));
            let gs_bound_kt = fastest_tas + MAX_PLAUSIBLE_TAILWIND_KT;
            levels.push(LevelTable { tas_kt, burn_kg_h, burn_bound_kg_h, gs_bound_kt });
        }
        // A first estimate of trip fuel, for guessing how heavy the aircraft is at a given
        // distance flown: burn at the optimum altitude for roughly the mid-trip weight.
        let rep_weight = (zfw_kg + t.mtow_kg).clamp(t.oew_kg, t.mtow_kg * 2.0) / 2.0;
        let rep_level = aero::optimum_altitude_ft(&t, rep_weight, mach).min(t.ceiling_ft);
        let rep_temp = crate::dispatch::isa_temp_c(rep_level);
        let rep_tas = atmosphere::tas_from_mach(mach, rep_temp).max(100.0);
        let rep_burn = aero::cruise_fuel_flow_kg_h(&t, rep_weight, rep_level, mach, rep_temp);
        let trip_fuel_kg = rep_burn * (trip_nm / rep_tas);
        PerfCostModel { t, air, zfw_kg, trip_nm: trip_nm.max(1.0), trip_fuel_kg, mach, levels }
    }

    /// The weight estimated at a distance already flown: heaviest at the start, lightest at
    /// the end, the fuel burnt assumed roughly even with distance.
    fn weight_at(&self, flown_nm: f64) -> f64 {
        let remaining = (1.0 - (flown_nm / self.trip_nm).clamp(0.0, 1.0)) * self.trip_fuel_kg;
        (self.zfw_kg + remaining).clamp(self.t.oew_kg, self.t.mtow_kg)
    }

    fn level(&self, level_ft: f64) -> &LevelTable {
        let i = (level_ft / 1000.0).round().clamp(0.0, (self.levels.len() - 1) as f64) as usize;
        &self.levels[i]
    }

    /// The Mach every level's table was built at: the cost index's ECON Mach, or the fixed
    /// one, resolved once at construction.
    pub fn cruise_mach(&self) -> f64 {
        self.mach
    }
}

impl CostModel for PerfCostModel<'_> {
    fn leg(&self, q: &LegQuery) -> LegCost {
        let nm = distance_nm(q.from, q.to);
        if nm < 0.01 {
            return LegCost::default();
        }
        let level = self.level(q.level_ft);
        let weight = self.weight_at(q.flown_nm);
        let air = self.air.air(along(q.from, q.to, 0.5), q.level_ft, q.when);
        // The table is built on a standard day; a real one is corrected for by the same
        // square-root-of-temperature-ratio rule the speed of sound, and so the specific
        // fuel consumption, follows.
        let isa_temp_k = crate::dispatch::isa_temp_c(q.level_ft) + 273.15;
        let temp_ratio = ((air.temp_c + 273.15).max(150.0) / isa_temp_k.max(150.0)).max(0.5);
        let tas_kt = level.tas_kt * temp_ratio.sqrt();
        let burn_kg_h = level.burn_at(weight, self.t.oew_kg, self.t.mtow_kg) * temp_ratio.sqrt();
        let gs = air.ground_speed(bearing_deg(q.from, q.to), tas_kt);
        let minutes = nm / gs * 60.0;
        LegCost { minutes, fuel_kg: minutes / 60.0 * burn_kg_h }
    }

    fn lower_bound(&self, nm: f64, level_ft: f64) -> LegCost {
        if nm < 0.01 {
            return LegCost::default();
        }
        let level = self.level(level_ft);
        let minutes = nm / level.gs_bound_kt * 60.0;
        LegCost { minutes, fuel_kg: minutes / 60.0 * level.burn_bound_kg_h }
    }

    fn ceiling_ft(&self) -> f64 {
        self.t.ceiling_ft
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{Air, StillAir};
    use crate::perf::aircraft::lookup;
    use chrono::Utc;

    /// A wind field bounded well inside `MAX_PLAUSIBLE_TAILWIND_KT`, standing in for the
    /// worst a real forecast could throw at the model.
    struct BoundedWind {
        speed_kt: f64,
        from_deg: f64,
        temp_bias_c: f64,
    }

    impl WindField for BoundedWind {
        fn air(&self, _at: (f64, f64), alt_ft: f64, _when: chrono::DateTime<Utc>) -> Air {
            Air { wind_from_deg: self.from_deg, wind_kt: self.speed_kt, temp_c: crate::dispatch::isa_temp_c(alt_ft) + self.temp_bias_c }
        }
    }

    fn rng(seed: &mut u64) -> f64 {
        // A tiny xorshift, so the test needs no dependency and is reproducible.
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 11) as f64 / (1u64 << 53) as f64
    }

    #[test]
    fn lower_bound_never_beats_the_leg_over_ten_thousand_random_legs() {
        let types = ["A20N", "B738", "A359", "B77W", "A388", "AT76"];
        let mut seed = 0x9E3779B97F4A7C15u64;
        for icao in types {
            let t = lookup(icao).unwrap();
            for _ in 0..1700 {
                let speed = rng(&mut seed) * 220.0;
                let from_deg = rng(&mut seed) * 360.0;
                let temp_bias = (rng(&mut seed) - 0.5) * 30.0;
                let wind = BoundedWind { speed_kt: speed, from_deg, temp_bias_c: temp_bias };
                let zfw = t.oew_kg + rng(&mut seed) * (t.mzfw_kg - t.oew_kg);
                let trip_nm = 50.0 + rng(&mut seed) * 6000.0;
                let ci = if rng(&mut seed) < 0.5 { None } else { Some(rng(&mut seed) * 100.0) };
                let model = PerfCostModel::build(t, &wind, zfw, trip_nm, ci);
                let level_ft = (rng(&mut seed) * (t.ceiling_ft / 1000.0)).floor() * 1000.0;
                let nm = 5.0 + rng(&mut seed) * 500.0;
                let flown_nm = rng(&mut seed) * trip_nm;
                let bearing = rng(&mut seed) * 360.0;
                let from = (rng(&mut seed) * 60.0 - 30.0, rng(&mut seed) * 60.0 - 30.0);
                let to = crate::dispatch::travel(from, bearing, nm);
                let q = LegQuery { from, to, level_ft, when: Utc::now(), flown_nm };
                let bound = model.lower_bound(nm, level_ft);
                let actual = model.leg(&q);
                assert!(bound.minutes <= actual.minutes + 1e-6, "{icao}: bound {} > actual {} minutes (nm {nm} level {level_ft})", bound.minutes, actual.minutes);
                assert!(bound.fuel_kg <= actual.fuel_kg + 1e-6, "{icao}: bound {} > actual {} fuel (nm {nm} level {level_ft})", bound.fuel_kg, actual.fuel_kg);
            }
        }
    }

    #[test]
    fn a_heavier_aircraft_burns_more_at_the_same_level() {
        let t = lookup("A320").unwrap();
        let still = StillAir;
        let light = PerfCostModel::build(t, &still, t.oew_kg + 2000.0, 500.0, None);
        let heavy = PerfCostModel::build(t, &still, t.mzfw_kg, 500.0, None);
        let q = |flown: f64| LegQuery { from: (0.0, 0.0), to: (0.0, 1.0), level_ft: 35_000.0, when: Utc::now(), flown_nm: flown };
        assert!(heavy.leg(&q(0.0)).fuel_kg >= light.leg(&q(0.0)).fuel_kg);
    }

    #[test]
    fn a_higher_cost_index_flies_faster() {
        let t = lookup("A320").unwrap();
        let still = StillAir;
        let slow = PerfCostModel::build(t, &still, t.mzfw_kg * 0.8, 800.0, Some(0.0));
        let fast = PerfCostModel::build(t, &still, t.mzfw_kg * 0.8, 800.0, Some(100.0));
        let q = LegQuery { from: (0.0, 0.0), to: (0.0, 1.0), level_ft: 35_000.0, when: Utc::now(), flown_nm: 0.0 };
        assert!(fast.leg(&q).minutes < slow.leg(&q).minutes);
    }

    #[test]
    fn calling_leg_and_lower_bound_allocates_nothing_new_per_call() {
        // Not a real allocation counter (none is wired into the test harness); this is a
        // speed smoke test instead — the table lookup should be microseconds, not
        // milliseconds, across a large number of calls.
        let t = lookup("B77W").unwrap();
        let still = StillAir;
        let model = PerfCostModel::build(t, &still, t.mzfw_kg * 0.85, 5500.0, Some(30.0));
        let q = LegQuery { from: (10.0, 10.0), to: (11.0, 11.0), level_ft: 37_000.0, when: Utc::now(), flown_nm: 1200.0 };
        let start = std::time::Instant::now();
        let mut acc = 0.0;
        for _ in 0..200_000 {
            acc += model.leg(&q).fuel_kg;
        }
        let elapsed = start.elapsed();
        println!("200,000 calls to leg() took {elapsed:?}, {:.1} ns/call", elapsed.as_nanos() as f64 / 200_000.0);
        assert!(acc > 0.0);
        assert!(elapsed.as_secs_f64() < 2.0, "200,000 calls took {elapsed:?}");
    }
}
