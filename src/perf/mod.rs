//! Aircraft performance: the vertical profile a route is flown on, the fuel it burns and
//! the weights it is flown at.

use crate::dispatch::{AircraftSpec, PerfPlan, PerfRequest};

/// What is known about a type, by its ICAO designator.
pub fn spec(icao_type: &str) -> anyhow::Result<AircraftSpec> {
    let _ = icao_type;
    anyhow::bail!("aircraft performance is not built yet")
}

/// What a leg of a route costs this aircraft, for the route search to plan on: the real
/// burn at that weight, that level and that temperature, rather than a fixed figure.
///
/// `zfw_kg` is what the aircraft weighs without fuel, and `trip_nm` roughly how far it is
/// going, which together say how heavy it will be at each point of the way.
pub fn cost_model<'a>(spec: &'a AircraftSpec, air: &'a dyn crate::dispatch::WindField, zfw_kg: f64, trip_nm: f64, cost_index: Option<f64>) -> anyhow::Result<Box<dyn crate::dispatch::CostModel + 'a>> {
    let _ = (spec, air, zfw_kg, trip_nm, cost_index);
    anyhow::bail!("aircraft performance is not built yet")
}

/// Fly a route: the profile, the fuel and the weights.
pub fn plan(req: &PerfRequest) -> anyhow::Result<PerfPlan> {
    let _ = req;
    anyhow::bail!("aircraft performance is not built yet")
}
