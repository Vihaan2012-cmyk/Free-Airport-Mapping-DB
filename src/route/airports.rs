//! Airports, from the navigation database on this machine: where one is, and which lie
//! near a place with a runway long enough.

use crate::dispatch::{Airport, LatLon};

/// An airport by its ICAO code.
pub fn airport(icao: &str) -> Option<Airport> {
    let _ = icao;
    None
}

/// The airports within a distance of a place whose longest runway is at least a length,
/// nearest first, with that runway's length in feet.
pub fn airports_near(at: LatLon, radius_nm: f64, min_runway_ft: f64) -> Vec<(Airport, f64)> {
    let _ = (at, radius_nm, min_runway_ft);
    Vec::new()
}
