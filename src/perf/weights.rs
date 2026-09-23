//! The weights the aircraft is flown at, against the limits it may not cross: where a limit
//! bites, the payload is cut rather than the fuel, since the fuel is what the flight was
//! planned to need.

use crate::dispatch::{AircraftSpec, FuelBreakdown, Weights};

/// Work out zero-fuel, take-off and landing weight from a requested payload and the fuel
/// already planned, cutting the payload against whichever limit binds hardest and returning
/// a warning for each cut and for fuel that will not fit in the tanks.
pub fn compute(spec: &AircraftSpec, payload_kg: f64, fuel: &FuelBreakdown) -> (Weights, Vec<String>) {
    let mut warnings = Vec::new();
    let mut payload = payload_kg.max(0.0);
    let mut bound: Vec<&'static str> = Vec::new();

    if spec.oew_kg + payload > spec.mzfw_kg {
        payload = (spec.mzfw_kg - spec.oew_kg).max(0.0);
        bound.push("maximum zero-fuel weight");
    }
    let mut zfw_kg = spec.oew_kg + payload;

    let mut tow_kg = zfw_kg + fuel.takeoff_kg;
    if tow_kg > spec.mtow_kg {
        let cut = tow_kg - spec.mtow_kg;
        payload = (payload - cut).max(0.0);
        zfw_kg = spec.oew_kg + payload;
        tow_kg = zfw_kg + fuel.takeoff_kg;
        bound.push("maximum take-off weight");
    }

    let mut lw_kg = zfw_kg + fuel.landing_kg;
    if lw_kg > spec.mlw_kg {
        let cut = lw_kg - spec.mlw_kg;
        payload = (payload - cut).max(0.0);
        zfw_kg = spec.oew_kg + payload;
        tow_kg = zfw_kg + fuel.takeoff_kg;
        lw_kg = zfw_kg + fuel.landing_kg;
        bound.push("maximum landing weight");
    }

    if fuel.takeoff_kg > spec.max_fuel_kg + 1.0 {
        warnings.push(format!("{:.0} kg of fuel is planned at take-off, more than the {:.0} kg the tanks hold", fuel.takeoff_kg, spec.max_fuel_kg));
    }

    let limited_by = if bound.is_empty() {
        None
    } else {
        let joined = bound.join(", then the ");
        warnings.push(format!("payload cut to {:.0} kg to stay under the {joined}", payload));
        Some(bound.last().copied().unwrap_or_default().to_string())
    };

    (
        Weights {
            oew_kg: spec.oew_kg,
            payload_kg: payload,
            zfw_kg,
            tow_kg,
            lw_kg,
            max_zfw_kg: spec.mzfw_kg,
            max_tow_kg: spec.mtow_kg,
            max_lw_kg: spec.mlw_kg,
            limited_by,
        },
        warnings,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::aircraft::lookup;

    fn fuel(takeoff_kg: f64) -> FuelBreakdown {
        FuelBreakdown { takeoff_kg, landing_kg: takeoff_kg * 0.3, block_kg: takeoff_kg + 200.0, ..Default::default() }
    }

    #[test]
    fn a_light_payload_is_carried_whole() {
        let spec = lookup("A320").unwrap().to_spec();
        let (w, warnings) = compute(&spec, 15_000.0, &fuel(8_000.0));
        assert_eq!(w.payload_kg, 15_000.0);
        assert!(w.limited_by.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn too_much_payload_is_cut_to_mzfw() {
        let spec = lookup("A320").unwrap().to_spec();
        let huge_payload = spec.mzfw_kg - spec.oew_kg + 10_000.0;
        // Light enough fuel that the take-off and landing weight limits stay clear of this,
        // isolating the zero-fuel weight limit the test means to check.
        let (w, warnings) = compute(&spec, huge_payload, &fuel(3_000.0));
        assert!((w.zfw_kg - spec.mzfw_kg).abs() < 0.5);
        assert_eq!(w.limited_by.as_deref(), Some("maximum zero-fuel weight"));
        assert!(!warnings.is_empty());
    }

    #[test]
    fn heavy_fuel_cuts_payload_to_mtow() {
        let spec = lookup("A20N").unwrap().to_spec();
        let payload = spec.mzfw_kg - spec.oew_kg; // as much payload as MZFW allows
        let (w, _) = compute(&spec, payload, &fuel(spec.max_fuel_kg));
        assert!(w.tow_kg <= spec.mtow_kg + 0.5);
        assert_eq!(w.limited_by.as_deref(), Some("maximum take-off weight"));
        assert!(w.payload_kg < payload);
    }

    #[test]
    fn fuel_beyond_tank_capacity_warns() {
        let spec = lookup("A320").unwrap().to_spec();
        let (_, warnings) = compute(&spec, 5_000.0, &fuel(spec.max_fuel_kg + 3_000.0));
        assert!(warnings.iter().any(|w| w.contains("tanks")));
    }
}
