//! The fuel breakdown: taxi, trip, contingency, the alternate, the final reserve, extra and
//! tankering, added up into what must be aboard at take-off, at the blocks, and at landing.
//!
//! Everything here is arithmetic on figures `profile.rs` has already flown (the trip burn,
//! the burn rate for a hold at low level) — nothing in this file touches the aerodynamic
//! model itself.

use crate::dispatch::{FuelBreakdown, FuelPolicy};

/// What tankering is weighed against: the price of fuel at each end, and how much extra the
/// aircraft burns carrying a kilogram of it the length of the trip.
pub struct TankerInputs {
    pub price_origin: f64,
    pub price_destination: f64,
    /// Extra fuel burnt over the trip per kilogram of fuel carried, as a fraction — the
    /// familiar rule of thumb that carrying fuel costs fuel, from the model's own
    /// weight-sensitivity of burn rather than a published tankering table.
    pub penalty_fraction: f64,
    /// How much more the operator would take if it pays: usually the tank space left once
    /// everything else the flight needs is aboard.
    pub capacity_left_kg: f64,
}

/// The burn rates a hold is costed at.
pub struct HoldRates {
    /// Kilograms a minute of holding for contingency's minimum, roughly the trip's cruise
    /// burn rate.
    pub contingency_kg_min: f64,
    /// Kilograms a minute of the final reserve hold, at 1,500 ft over the alternate at the
    /// weight the aircraft will then be — lower than cruise, since it is low and light.
    pub final_reserve_kg_min: f64,
}

/// Add up the fuel: `trip_kg` and the burn rates in `rates` come from a profile already
/// flown; `alternate_kg` from a small profile flown to it, or `None` where the request gave
/// no alternate, which is reported as a warning rather than treated as no reserve at all.
pub fn compute(policy: &FuelPolicy, trip_kg: f64, rates: &HoldRates, alternate_kg: Option<f64>, tanker: Option<&TankerInputs>) -> (FuelBreakdown, Vec<String>) {
    let mut warnings = Vec::new();

    let contingency_kg = (trip_kg * policy.contingency_pct / 100.0).max(policy.contingency_min_minutes * rates.contingency_kg_min);

    let alternate_kg_val = alternate_kg.unwrap_or(0.0);
    if alternate_kg.is_none() {
        warnings.push("no alternate was planned; only the final reserve covers a diversion".to_string());
    }

    let final_reserve_kg = policy.final_reserve_min * rates.final_reserve_kg_min;

    let tanker_kg = match tanker {
        Some(t) if policy.tanker && t.capacity_left_kg > 0.0 => {
            let saving_per_kg = t.price_destination - t.price_origin;
            let cost_per_kg = t.penalty_fraction * t.price_origin;
            if saving_per_kg > cost_per_kg {
                t.capacity_left_kg.max(0.0)
            } else {
                0.0
            }
        }
        _ => 0.0,
    };

    let takeoff_kg = trip_kg + contingency_kg + alternate_kg_val + final_reserve_kg + policy.extra_kg + tanker_kg;
    let block_kg = takeoff_kg + policy.taxi_kg;
    let landing_kg = takeoff_kg - trip_kg;

    (
        FuelBreakdown {
            taxi_kg: policy.taxi_kg,
            trip_kg,
            contingency_kg,
            alternate_kg: alternate_kg_val,
            final_reserve_kg,
            extra_kg: policy.extra_kg,
            tanker_kg,
            takeoff_kg,
            block_kg,
            landing_kg,
        },
        warnings,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> FuelPolicy {
        FuelPolicy::default()
    }

    fn rates() -> HoldRates {
        HoldRates { contingency_kg_min: 40.0, final_reserve_kg_min: 25.0 }
    }

    #[test]
    fn the_pieces_add_up() {
        let (f, _) = compute(&policy(), 5000.0, &rates(), Some(800.0), None);
        assert!((f.takeoff_kg - (f.trip_kg + f.contingency_kg + f.alternate_kg + f.final_reserve_kg + f.extra_kg + f.tanker_kg)).abs() < 1e-6);
        assert!((f.block_kg - (f.takeoff_kg + f.taxi_kg)).abs() < 1e-6);
        assert!((f.landing_kg - (f.takeoff_kg - f.trip_kg)).abs() < 1e-6);
    }

    #[test]
    fn contingency_never_falls_below_the_stated_minutes() {
        // A short trip where 5% of trip fuel is less than five minutes of holding.
        let (f, _) = compute(&policy(), 200.0, &rates(), Some(100.0), None);
        assert!(f.contingency_kg >= policy().contingency_min_minutes * rates().contingency_kg_min - 1e-6);
    }

    #[test]
    fn no_alternate_warns() {
        let (_, warnings) = compute(&policy(), 5000.0, &rates(), None, None);
        assert!(warnings.iter().any(|w| w.contains("alternate")));
    }

    #[test]
    fn tankering_is_taken_only_when_it_pays() {
        let cheap_at_origin = TankerInputs { price_origin: 0.8, price_destination: 1.2, penalty_fraction: 0.03, capacity_left_kg: 3000.0 };
        let (f, _) = compute(&FuelPolicy { tanker: true, ..policy() }, 5000.0, &rates(), Some(800.0), Some(&cheap_at_origin));
        assert_eq!(f.tanker_kg, 3000.0);

        let not_worth_it = TankerInputs { price_origin: 1.0, price_destination: 1.02, penalty_fraction: 0.05, capacity_left_kg: 3000.0 };
        let (f2, _) = compute(&FuelPolicy { tanker: true, ..policy() }, 5000.0, &rates(), Some(800.0), Some(&not_worth_it));
        assert_eq!(f2.tanker_kg, 0.0);
    }

    #[test]
    fn tankering_is_off_unless_the_policy_asks_for_it() {
        let cheap_at_origin = TankerInputs { price_origin: 0.8, price_destination: 1.2, penalty_fraction: 0.03, capacity_left_kg: 3000.0 };
        let (f, _) = compute(&policy(), 5000.0, &rates(), Some(800.0), Some(&cheap_at_origin));
        assert_eq!(f.tanker_kg, 0.0);
    }
}
