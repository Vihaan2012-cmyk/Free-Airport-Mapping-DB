//! The weather a flight is planned through: the winds and temperatures aloft, the
//! reports and forecasts at the airports, and the hazards drawn as shapes.
//!
//! Split by what each part is fetched from and how it is read:
//!
//! * [`grib2`] reads the wire format NOAA's model output is shipped in;
//! * [`gfs`] turns that into [`Forecast`], the wind field the route search queries in its
//!   hot loop, fetched and cached by cycle, hour and area;
//! * [`metar`] and [`taf`] parse the fixed-group text of current and forecast reports;
//! * [`common`] is what METAR and TAF share: the day/hour groups, and the wind, visibility,
//!   cloud and pressure tokens both are written in;
//! * [`sigmet`] turns the aviationweather.gov hazard feeds into [`Hazard`]s;
//! * [`alternates`] judges which of a destination's neighbours are fit to divert to.

mod alternates;
mod common;
pub mod gfs;
pub mod grib2;
mod metar;
mod sigmet;
mod taf;

use crate::dispatch::{distance_nm, Air, Airport, Alternate, Bounds, Hazard, LatLon, Metar, Taf, Wind, WindField};
use chrono::{DateTime, Utc};

/// A forecast of the winds and temperatures aloft over an area: NOAA's GFS, at a quarter
/// degree, read through [`grib2`] and interpolated by [`gfs::Forecast`].
pub use gfs::Forecast;

/// Winds given by hand, a few samples interpolated between: inverse-distance weighting
/// within the nearest band of altitude, the same model `route::Conditions::wind_at` uses,
/// since a hand-built set of samples is usually the same handful of points that go into
/// testing the route search itself. The temperature is always the standard atmosphere's,
/// since a plain wind sample carries none of its own.
pub struct Samples(pub Vec<Wind>);

impl WindField for Samples {
    fn air(&self, at: LatLon, alt_ft: f64, _when: DateTime<Utc>) -> Air {
        let band: Vec<&Wind> = {
            let near: Vec<&Wind> = self.0.iter().filter(|w| (w.alt_ft - alt_ft).abs() <= 4000.0).collect();
            if near.is_empty() {
                let Some(best) = self.0.iter().map(|w| (w.alt_ft - alt_ft).abs()).min_by(f64::total_cmp) else {
                    return Air::standard(alt_ft);
                };
                self.0.iter().filter(|w| ((w.alt_ft - alt_ft).abs() - best).abs() < 1.0).collect()
            } else {
                near
            }
        };
        let (mut n, mut e, mut total) = (0.0, 0.0, 0.0);
        for w in band {
            let d = distance_nm(at, (w.lat, w.lon));
            if d > 600.0 {
                continue;
            }
            let weight = 1.0 / (d * d + 25.0);
            let towards = (w.from_deg + 180.0).to_radians();
            n += weight * w.speed_kt * towards.cos();
            e += weight * w.speed_kt * towards.sin();
            total += weight;
        }
        if total == 0.0 {
            return Air::standard(alt_ft);
        }
        let (avg_n, avg_e) = (n / total, e / total);
        let towards_deg = avg_e.atan2(avg_n).to_degrees();
        Air { wind_from_deg: (towards_deg + 180.0).rem_euclid(360.0), wind_kt: (avg_n * avg_n + avg_e * avg_e).sqrt(), temp_c: crate::dispatch::isa_temp_c(alt_ft) }
    }
}

/// Parse a raw METAR into its fields.
pub fn parse_metar(raw: &str) -> anyhow::Result<Metar> {
    metar::parse_metar(raw)
}

/// Parse a raw TAF into its validity period and the changes within it.
pub fn parse_taf(raw: &str) -> anyhow::Result<Taf> {
    taf::parse_taf(raw)
}

/// The latest report for an airport.
pub fn metar(icao: &str) -> anyhow::Result<Metar> {
    metar::metar(icao)
}

/// The latest forecast for an airport.
pub fn taf(icao: &str) -> anyhow::Result<Taf> {
    taf::taf(icao)
}

/// SIGMETs over an area in force at a time, as hazards.
pub fn sigmets(bounds: Bounds, when: DateTime<Utc>) -> anyhow::Result<Vec<Hazard>> {
    sigmet::sigmets(bounds, when)
}

/// The best alternates for a destination at an arrival time, best first.
pub fn choose_alternates(destination: &Airport, eta: DateTime<Utc>, count: usize) -> anyhow::Result<Vec<Alternate>> {
    alternates::choose_alternates(destination, eta, count)
}

/// The pure judgement behind [`choose_alternates`], exposed so it can be tested and used
/// against candidates and TAFs from anywhere, not only the navigation database's own
/// `airports_near`.
pub fn rank_alternates(dest: &Airport, candidates: &[(Airport, f64)], tafs: &[(String, Taf)], eta: DateTime<Utc>) -> Vec<Alternate> {
    alternates::rank_alternates(dest, candidates, tafs, eta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::isa_temp_c;

    #[test]
    fn samples_weight_the_nearest_reports_more_heavily() {
        let winds = Samples(vec![
            Wind { lat: 50.0, lon: 0.0, alt_ft: 35_000.0, from_deg: 270.0, speed_kt: 100.0 },
            Wind { lat: 50.0, lon: 20.0, alt_ft: 35_000.0, from_deg: 90.0, speed_kt: 20.0 },
        ]);
        let a = winds.air((50.0, 1.0), 35_000.0, Utc::now());
        // Much nearer the strong westerly than the far, weak easterly.
        assert!(a.wind_kt > 60.0, "{}", a.wind_kt);
        assert!((a.wind_from_deg - 270.0).abs() < 30.0, "{}", a.wind_from_deg);
    }

    #[test]
    fn samples_fall_back_to_standard_air_with_nothing_nearby() {
        let winds = Samples(vec![Wind { lat: 0.0, lon: 0.0, alt_ft: 35_000.0, from_deg: 90.0, speed_kt: 40.0 }]);
        let a = winds.air((80.0, 170.0), 35_000.0, Utc::now());
        assert_eq!(a.wind_kt, 0.0);
        assert_eq!(a.temp_c, isa_temp_c(35_000.0));
    }
}
