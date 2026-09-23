//! The aerodynamic and propulsion model: a drag polar, a thrust lapse and a fuel flow, built
//! from the type's geometry and engine figures in `aircraft.rs`.
//!
//! Every equation here is textbook aerodynamics and propulsion — the parabolic drag polar
//! with a compressibility rise near the critical Mach, and a power-law thrust lapse with
//! density ratio for a high-bypass turbofan (see Torenbeek, *Synthesis of Subsonic Airplane
//! Design*, and Mattingly, *Elements of Gas Turbine Propulsion*, for both) — none of it comes
//! from a licensed performance database. What is aircraft-specific lives in `TypeData`.

use crate::perf::aircraft::TypeData;
use crate::perf::atmosphere;

pub(crate) const KT_TO_MS: f64 = 0.514_444_4;
pub(crate) const MS_TO_FPM: f64 = 196.850_39;
pub const G0: f64 = 9.806_65;

/// Dynamic pressure at a Mach number, altitude and temperature, pascals.
fn dynamic_pressure_pa(alt_ft: f64, mach: f64, temp_c: f64) -> f64 {
    let tas_ms = atmosphere::tas_from_mach(mach, temp_c) * KT_TO_MS;
    0.5 * atmosphere::density_kgm3(alt_ft, temp_c) * tas_ms * tas_ms
}

/// The lift coefficient needed for level flight at a weight, altitude, Mach and temperature.
pub fn cl(t: &TypeData, weight_kg: f64, alt_ft: f64, mach: f64, temp_c: f64) -> f64 {
    let q = dynamic_pressure_pa(alt_ft, mach, temp_c).max(1.0);
    weight_kg * G0 / (q * t.wing_area_m2)
}

/// The drag coefficient: the parabolic polar, plus a compressibility rise past the type's
/// critical Mach. The rise is a quartic, which is the shape most drag-divergence charts show
/// (a gentle knee, then a steep climb) rather than a physical derivation.
pub fn cd(t: &TypeData, cl: f64, mach: f64) -> f64 {
    let compressibility = if mach > t.mach_crit { 20.0 * (mach - t.mach_crit).powi(4) } else { 0.0 };
    t.cd0 + t.induced_k() * cl * cl + compressibility
}

/// Drag in level flight, newtons.
pub fn drag_n(t: &TypeData, weight_kg: f64, alt_ft: f64, mach: f64, temp_c: f64) -> f64 {
    let cl_ = cl(t, weight_kg, alt_ft, mach, temp_c);
    let cd_ = cd(t, cl_, mach);
    cd_ * dynamic_pressure_pa(alt_ft, mach, temp_c) * t.wing_area_m2
}

/// Lift-to-drag ratio in level flight.
pub fn lift_to_drag(t: &TypeData, weight_kg: f64, alt_ft: f64, mach: f64, temp_c: f64) -> f64 {
    let d = drag_n(t, weight_kg, alt_ft, mach, temp_c);
    if d <= 1.0 {
        0.0
    } else {
        weight_kg * G0 / d
    }
}

/// The maximum thrust every engine together can give at an altitude, Mach and temperature: a
/// density-ratio power law (exponent 0.7, typical of a high-bypass turbofan) with a mild
/// Mach correction for ram drag, clamped so it never goes to zero.
pub fn max_thrust_n(t: &TypeData, alt_ft: f64, mach: f64, temp_c: f64) -> f64 {
    let sigma = atmosphere::density_kgm3(alt_ft, temp_c) / atmosphere::RHO0_KGM3;
    let lapse = sigma.powf(0.7) * (1.0 - 0.16 * mach).max(0.1);
    t.thrust_sls_n * t.engines as f64 * lapse
}

/// Thrust-specific fuel consumption at a Mach and temperature, kilograms of fuel per
/// newton-hour: the type's reference cruise TSFC (published in kgf, converted here),
/// adjusted for the well-known rise in specific fuel consumption with both Mach and ambient
/// temperature.
pub fn tsfc_n_h(t: &TypeData, mach: f64, temp_c: f64) -> f64 {
    let ref_tsfc_n_h = t.tsfc_cruise_kgf_h / G0;
    let theta = (temp_c + 273.15) / atmosphere::T0_K;
    let mach_factor = 0.7 + 0.3 * (mach / t.cruise_mach.max(0.3));
    ref_tsfc_n_h * mach_factor.max(0.3) * theta.max(0.5).sqrt()
}

/// Fuel flow for a given thrust, kilograms per hour.
pub fn fuel_flow_kg_h(t: &TypeData, thrust_n: f64, mach: f64, temp_c: f64) -> f64 {
    thrust_n.max(0.0) * tsfc_n_h(t, mach, temp_c)
}

/// Fuel flow in level cruising flight, where thrust equals drag.
pub fn cruise_fuel_flow_kg_h(t: &TypeData, weight_kg: f64, alt_ft: f64, mach: f64, temp_c: f64) -> f64 {
    fuel_flow_kg_h(t, drag_n(t, weight_kg, alt_ft, mach, temp_c), mach, temp_c)
}

/// Fuel flow at flight idle: a small fraction of the thrust available, which is how idle
/// scales in practice (idle N1 sits at roughly 4-7% of full thrust across the flight
/// envelope) since no public idle-fuel-flow chart exists per type.
pub fn idle_fuel_flow_kg_h(t: &TypeData, alt_ft: f64, mach: f64, temp_c: f64) -> f64 {
    let idle_thrust = max_thrust_n(t, alt_ft, mach, temp_c) * 0.05;
    fuel_flow_kg_h(t, idle_thrust, mach, temp_c)
}

/// The rate of climb available at full climb thrust, feet per minute: excess power divided
/// by weight, the standard energy-height result.
pub fn rate_of_climb_fpm(t: &TypeData, weight_kg: f64, alt_ft: f64, mach: f64, temp_c: f64) -> f64 {
    let thrust = max_thrust_n(t, alt_ft, mach, temp_c);
    let drag = drag_n(t, weight_kg, alt_ft, mach, temp_c);
    let tas_ms = atmosphere::tas_from_mach(mach, temp_c) * KT_TO_MS;
    let weight_n = weight_kg * G0;
    (thrust - drag).max(0.0) * tas_ms / weight_n * MS_TO_FPM
}

/// The altitude, up to the ceiling, at which a weight and Mach fly with the least drag
/// (equivalently the best lift-to-drag ratio): found by a coarse scan, which is cheap enough
/// since it is only ever called when deciding a step climb, not per leg.
pub fn optimum_altitude_ft(t: &TypeData, weight_kg: f64, mach: f64) -> f64 {
    let mut best_alt = 0.0;
    let mut best_ld = 0.0;
    let mut alt = 0.0;
    while alt <= t.ceiling_ft {
        let temp_c = crate::dispatch::isa_temp_c(alt);
        let ld = lift_to_drag(t, weight_kg, alt, mach, temp_c);
        if ld > best_ld {
            best_ld = ld;
            best_alt = alt;
        }
        alt += 500.0;
    }
    best_alt.min(t.ceiling_ft)
}

/// The highest altitude at which the lift coefficient needed for level flight, times a
/// manoeuvre-margin factor, stays below an assumed buffet-onset lift coefficient. Real
/// buffet boundaries come from wind-tunnel testing that is never published per type, so this
/// uses a lift coefficient that falls with Mach in the shape every public buffet chart
/// shows (shock-induced buffet closing in as speed rises) rather than a measured curve.
/// `margin_g` is the manoeuvre margin required at the initial cruise altitude — 1.3g is the
/// figure commonly cited for transport-category certification.
pub fn buffet_ceiling_ft(t: &TypeData, weight_kg: f64, mach: f64, margin_g: f64) -> f64 {
    let cl_buffet = (1.25 - 0.6 * (mach - 0.6).max(0.0)).max(0.30);
    let mut alt = t.ceiling_ft;
    while alt > 0.0 {
        let temp_c = crate::dispatch::isa_temp_c(alt);
        if cl(t, weight_kg, alt, mach, temp_c) * margin_g <= cl_buffet {
            return alt;
        }
        alt -= 500.0;
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::aircraft::lookup;

    #[test]
    fn optimum_altitude_beats_a_low_one_on_drag() {
        // At a light-ish weight the theoretical minimum-drag altitude sits comfortably
        // under the ceiling, so the optimum found should beat both a low altitude and the
        // ceiling itself. At a heavy weight the minimum can sit at or past the ceiling — a
        // real aircraft simply cannot climb high enough to reach it — so this is checked at
        // a representative mid-cruise weight rather than near MTOW.
        let t = lookup("A320").unwrap();
        let w = t.mtow_kg * 0.75;
        let opt = optimum_altitude_ft(&t, w, t.cruise_mach);
        let low = drag_n(&t, w, 15_000.0, t.cruise_mach, crate::dispatch::isa_temp_c(15_000.0));
        let at_opt = drag_n(&t, w, opt, t.cruise_mach, crate::dispatch::isa_temp_c(opt));
        let high = drag_n(&t, w, t.ceiling_ft, t.cruise_mach, crate::dispatch::isa_temp_c(t.ceiling_ft));
        assert!(at_opt <= low, "the optimum altitude should beat a low one on drag: {at_opt} vs {low}");
        assert!(opt < t.ceiling_ft, "at this weight the optimum should sit under the ceiling: {opt}");
        assert!(at_opt <= high * 1.01, "the optimum altitude should be no worse than the ceiling: {at_opt} vs {high}");
    }

    #[test]
    fn optimum_altitude_rises_as_weight_falls() {
        let t = lookup("A359").unwrap();
        let heavy = optimum_altitude_ft(&t, t.mtow_kg * 0.95, t.cruise_mach);
        let light = optimum_altitude_ft(&t, t.mtow_kg * 0.65, t.cruise_mach);
        assert!(light > heavy, "a lighter aircraft should want a higher altitude: {light} vs {heavy}");
    }

    #[test]
    fn thrust_lapses_with_altitude() {
        let t = lookup("B738").unwrap();
        let sea = max_thrust_n(&t, 0.0, 0.3, 15.0);
        let cruise = max_thrust_n(&t, 35_000.0, t.cruise_mach, crate::dispatch::isa_temp_c(35_000.0));
        assert!(cruise < sea, "thrust should fall with altitude: {cruise} vs {sea}");
    }

    #[test]
    fn buffet_ceiling_falls_with_weight() {
        let t = lookup("A388").unwrap();
        let heavy = buffet_ceiling_ft(&t, t.mtow_kg * 0.95, t.cruise_mach, 1.3);
        let light = buffet_ceiling_ft(&t, t.mtow_kg * 0.55, t.cruise_mach, 1.3);
        assert!(heavy <= light, "a heavier aircraft should buffet lower: {heavy} vs {light}");
    }

    #[test]
    fn climb_rate_is_positive_at_typical_climb_weight_and_falls_towards_the_ceiling() {
        let t = lookup("A20N").unwrap();
        let w = t.mtow_kg * 0.92;
        let low = rate_of_climb_fpm(&t, w, 10_000.0, 0.5, crate::dispatch::isa_temp_c(10_000.0));
        let high = rate_of_climb_fpm(&t, w, 38_000.0, t.cruise_mach, crate::dispatch::isa_temp_c(38_000.0));
        assert!(low > 500.0, "{low}");
        assert!(high < low, "climb rate should fall approaching the ceiling: {high} vs {low}");
    }
}
