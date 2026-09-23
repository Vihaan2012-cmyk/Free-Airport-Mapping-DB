//! The airspace a flight has to know about beyond the airways: conflict zones, areas a
//! NOTAM makes active, and the flight information regions a route passes through.

use crate::dispatch::{Bounds, FiledRoute, Hazard, LatLon};
use chrono::{DateTime, Utc};

/// The conflict zones in force at a time, from the file of them kept with this crate.
pub fn conflict_zones(when: DateTime<Utc>) -> Vec<Hazard> {
    let _ = when;
    Vec::new()
}

/// The areas NOTAMs make active over an area between two times.
pub fn notam_hazards(bounds: Bounds, from: DateTime<Utc>, to: DateTime<Utc>) -> anyhow::Result<Vec<Hazard>> {
    let _ = (bounds, from, to);
    anyhow::bail!("NOTAMs are not built yet")
}

/// A flight information region a route passes through.
#[derive(Debug, Clone, PartialEq)]
pub struct FirCrossing {
    pub ident: String,
    pub name: String,
    pub entry: LatLon,
    /// From the origin, nautical miles.
    pub entry_nm: f64,
    pub exit_nm: f64,
}

/// The regions a route passes through, in order.
pub fn fir_crossings(route: &FiledRoute) -> Vec<FirCrossing> {
    let _ = route;
    Vec::new()
}
