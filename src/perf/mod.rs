//! Aircraft performance: the vertical profile a route is flown on, the fuel it burns and
//! the weights it is flown at.

use crate::dispatch::{AircraftSpec, PerfPlan, PerfRequest};

/// What is known about a type, by its ICAO designator.
pub fn spec(icao_type: &str) -> anyhow::Result<AircraftSpec> {
    let _ = icao_type;
    anyhow::bail!("aircraft performance is not built yet")
}

/// Fly a route: the profile, the fuel and the weights.
pub fn plan(req: &PerfRequest) -> anyhow::Result<PerfPlan> {
    let _ = req;
    anyhow::bail!("aircraft performance is not built yet")
}
