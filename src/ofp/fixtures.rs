//! A `Dispatch` built once from the fake providers, for `text`, `pdf`, `export` and
//! `bridge::simbrief` to render and export in their own tests without each repeating the
//! setup. Test-only (`ofp::mod` gates this module behind `#[cfg(test)]`), so it carries no
//! weight in the shipped binary.

use crate::dispatch::Dispatch;
use crate::ofp::providers::fake::{FakeProviders, AIRCRAFT, DESTINATION, ORIGIN};
use crate::ofp::{dispatch_with, DispatchOptions};

/// A complete, deterministic `Dispatch`: two Iberian airports, an A320neo, a named
/// alternate, a straight-line performance estimate (the fake fails `perf_plan`, which is
/// also what most of the network on a real machine will do before `perf` is built).
pub(crate) fn sample() -> Dispatch {
    let mut opts = DispatchOptions::new(ORIGIN, DESTINATION, AIRCRAFT);
    opts.payload_kg = 12_000.0;
    opts.passengers = 150;
    opts.cost_index = 25.0;
    opts.flight_number = Some("TAP123".to_string());
    opts.registration = Some("CS-TVA".to_string());
    dispatch_with(&opts, &FakeProviders::new()).expect("the fake providers always produce a plan")
}

/// The `DispatchOptions` `sample()` was built from, for renderers that also want the
/// options (the flight number, the registration).
pub(crate) fn sample_opts() -> DispatchOptions {
    let mut opts = DispatchOptions::new(ORIGIN, DESTINATION, AIRCRAFT);
    opts.payload_kg = 12_000.0;
    opts.passengers = 150;
    opts.cost_index = 25.0;
    opts.flight_number = Some("TAP123".to_string());
    opts.registration = Some("CS-TVA".to_string());
    opts
}
