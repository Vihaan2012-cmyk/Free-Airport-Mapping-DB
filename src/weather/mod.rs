//! The weather a flight is planned through: the winds and temperatures aloft, the
//! reports and forecasts at the airports, and the hazards drawn as shapes.

use crate::dispatch::{Air, Airport, Alternate, Bounds, Hazard, LatLon, Metar, Taf, Wind, WindField};
use chrono::{DateTime, Utc};

/// A forecast of the winds and temperatures aloft over an area.
pub struct Forecast {
    _private: (),
}

impl Forecast {
    /// The global model's forecast over an area for a time, fetched and cached.
    pub fn fetch(bounds: Bounds, when: DateTime<Utc>) -> anyhow::Result<Forecast> {
        let _ = (bounds, when);
        anyhow::bail!("the winds aloft are not built yet")
    }
}

impl WindField for Forecast {
    fn air(&self, _at: LatLon, alt_ft: f64, _when: DateTime<Utc>) -> Air {
        Air::standard(alt_ft)
    }
}

/// Winds given by hand, a few samples interpolated between.
pub struct Samples(pub Vec<Wind>);

impl WindField for Samples {
    fn air(&self, _at: LatLon, alt_ft: f64, _when: DateTime<Utc>) -> Air {
        Air::standard(alt_ft)
    }
}

pub fn parse_metar(raw: &str) -> anyhow::Result<Metar> {
    let _ = raw;
    anyhow::bail!("METARs are not built yet")
}

pub fn parse_taf(raw: &str) -> anyhow::Result<Taf> {
    let _ = raw;
    anyhow::bail!("TAFs are not built yet")
}

/// The latest report for an airport.
pub fn metar(icao: &str) -> anyhow::Result<Metar> {
    let _ = icao;
    anyhow::bail!("METARs are not built yet")
}

/// The latest forecast for an airport.
pub fn taf(icao: &str) -> anyhow::Result<Taf> {
    let _ = icao;
    anyhow::bail!("TAFs are not built yet")
}

/// SIGMETs over an area in force at a time, as hazards.
pub fn sigmets(bounds: Bounds, when: DateTime<Utc>) -> anyhow::Result<Vec<Hazard>> {
    let _ = (bounds, when);
    anyhow::bail!("SIGMETs are not built yet")
}

/// The best alternates for a destination at an arrival time, best first.
pub fn choose_alternates(destination: &Airport, eta: DateTime<Utc>, count: usize) -> anyhow::Result<Vec<Alternate>> {
    let _ = (destination, eta, count);
    anyhow::bail!("alternates are not built yet")
}
