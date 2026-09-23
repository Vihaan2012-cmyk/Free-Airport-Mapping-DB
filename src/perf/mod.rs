//! Aircraft performance: the vertical profile a route is flown on, the fuel it burns and
//! the weights it is flown at.
//!
//! Built from public documents — manufacturers' Aircraft Characteristics, type certificate
//! data sheets, and the textbook equations of drag, thrust lapse and specific fuel
//! consumption — never from a licensed performance model. `aircraft.rs` says where each
//! type's figures came from and, where one had to be estimated, what it was calibrated
//! against.
//!
//! * `atmosphere` — the ISA and the CAS/TAS/Mach conversions everything else is built on.
//! * `aircraft` — the type table.
//! * `aero` — the drag polar, thrust lapse and fuel flow.
//! * `cost` — `CostModel`, precomputed per level for the route search to call millions of
//!   times.
//! * `profile` — the climb/cruise/descent integration `plan` runs.
//! * `fuel` — the fuel breakdown's arithmetic.
//! * `weights` — the weight limits and what happens when one bites.
//! * `etp` — equal-time points and the point of no return.

pub mod aero;
pub mod aircraft;
pub mod atmosphere;
pub mod cost;
pub mod etp;
pub mod fuel;
pub mod profile;
pub mod weights;

use crate::dispatch::{AircraftSpec, PerfPlan, PerfRequest};

/// What is known about a type, by its ICAO designator.
pub fn spec(icao_type: &str) -> anyhow::Result<AircraftSpec> {
    aircraft::lookup(icao_type).map(|t| t.to_spec()).ok_or_else(|| anyhow::anyhow!("no performance data for aircraft type {icao_type}"))
}

/// What a leg of a route costs this aircraft, for the route search to plan on: the real
/// burn at that weight, that level and that temperature, rather than a fixed figure.
///
/// `zfw_kg` is what the aircraft weighs without fuel, and `trip_nm` roughly how far it is
/// going, which together say how heavy it will be at each point of the way.
pub fn cost_model<'a>(spec: &'a AircraftSpec, air: &'a dyn crate::dispatch::WindField, zfw_kg: f64, trip_nm: f64, cost_index: Option<f64>) -> anyhow::Result<Box<dyn crate::dispatch::CostModel + 'a>> {
    let base = aircraft::lookup(&spec.icao_type).ok_or_else(|| anyhow::anyhow!("no performance data for aircraft type {}", spec.icao_type))?;
    let t = base.with_spec(spec);
    Ok(Box::new(cost::PerfCostModel::build(t, air, zfw_kg, trip_nm.max(1.0), cost_index)))
}

/// Fly a route: the profile, the fuel and the weights.
pub fn plan(req: &PerfRequest) -> anyhow::Result<PerfPlan> {
    profile::plan(req)
}
