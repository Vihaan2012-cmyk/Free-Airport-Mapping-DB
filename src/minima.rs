//! An estimate of an approach's minimum descent height.
//!
//! Real minima are designed, not derived: a state's procedure designers lay an obstacle
//! assessment surface around the approach, find the controlling obstacle, add the margin
//! their rules require, apply the system minimum for the approach type, then round and
//! adjust by local policy. Two rule sets are in use (PANS-OPS and the American TERPS)
//! and they do not agree.
//!
//! What can be reproduced from free data is the *shape* of that calculation:
//!
//! * the **system minimum**, a floor below which an approach of a given type may never
//!   go, which is what decides a large share of real minima; and
//! * a **terrain clearance**, from the elevation model, which stands in for the obstacle
//!   survey we do not have.
//!
//! Where the system minimum is the higher of the two, the answer is the published one.
//! Where terrain decides, the answer is an estimate and says so: the model sees
//! buildings and trees but not masts, aerials or cranes, and those are usually what sets
//! a real minimum. Nothing here is a substitute for the published chart.

use serde::{Deserialize, Serialize};

/// Approach types, with the lowest height above touchdown each may use.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approach {
    /// ILS or GLS with a decision height.
    PrecisionCat1,
    /// Localiser, LP, VOR or similar: descent to a minimum altitude, no glidepath.
    NonPrecision,
    /// RNAV with vertical guidance (LPV, LNAV/VNAV).
    VerticallyGuided,
    /// Circling to land.
    Circling,
}

impl Approach {
    /// Height above touchdown, in feet, below which this kind of approach may not go.
    pub fn system_minimum_ft(self) -> f64 {
        match self {
            Approach::PrecisionCat1 => 200.0,
            Approach::VerticallyGuided => 250.0,
            Approach::NonPrecision => 300.0,
            Approach::Circling => 400.0,
        }
    }

    /// Clearance required above the controlling obstacle, in feet.
    fn obstacle_margin_ft(self) -> f64 {
        match self {
            // A precision approach keeps its clearance through the glidepath geometry
            // rather than a flat margin; this stands in for it.
            Approach::PrecisionCat1 | Approach::VerticallyGuided => 100.0,
            Approach::NonPrecision => 250.0,
            Approach::Circling => 300.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Approach::PrecisionCat1 => "ILS CAT I",
            Approach::VerticallyGuided => "RNAV (vertical guidance)",
            Approach::NonPrecision => "non-precision",
            Approach::Circling => "circling",
        }
    }
}

/// What set the number.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitedBy {
    /// The floor for this approach type. The published minimum will be this too.
    SystemMinimum,
    /// Terrain along the approach. An estimate: obstacles are not in the data.
    Terrain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Estimate {
    pub approach: Approach,
    /// Decision altitude or minimum descent altitude, feet above sea level.
    pub altitude_ft: f64,
    /// Height above the touchdown zone.
    pub height_ft: f64,
    pub limited_by: LimitedBy,
    /// Highest terrain found in the approach area, feet above sea level.
    pub highest_terrain_ft: f64,
    /// True when the published figure should match this one.
    pub reliable: bool,
}

/// Rounded up, never down: a minimum may be higher than the calculation, never lower.
fn round_up(ft: f64, step: f64) -> f64 {
    (ft / step).ceil() * step
}

/// Estimate the minimum for one approach.
///
/// `touchdown_elev_ft` is the elevation of the touchdown zone and `highest_terrain_ft`
/// the highest ground found in the approach area, both above sea level.
///
/// A system-limited answer is left exactly as the rule gives it, because that is how it
/// is published: Heathrow's 200 ft over an 83 ft touchdown zone is charted as 283 ft. A
/// terrain-limited answer is an estimate, so it is rounded up to the next 10 ft rather
/// than pretending to a precision it does not have.
pub fn estimate(approach: Approach, touchdown_elev_ft: f64, highest_terrain_ft: f64) -> Estimate {
    let by_system = touchdown_elev_ft + approach.system_minimum_ft();
    let by_terrain = highest_terrain_ft + approach.obstacle_margin_ft();
    let (altitude, limited_by) = if by_system >= by_terrain { (by_system, LimitedBy::SystemMinimum) } else { (by_terrain, LimitedBy::Terrain) };
    let altitude = if limited_by == LimitedBy::SystemMinimum { altitude } else { round_up(altitude, 10.0) };
    let threshold_elev_ft = touchdown_elev_ft;
    Estimate {
        approach,
        altitude_ft: altitude,
        height_ft: altitude - threshold_elev_ft,
        limited_by,
        highest_terrain_ft,
        reliable: limited_by == LimitedBy::SystemMinimum,
    }
}

/// Terrain that matters to an approach.
///
/// For an approach with a glidepath, ground only counts where it comes close to the
/// descent path: a hill five miles out sits a couple of thousand feet below a 3 degree
/// slope and changes nothing, which is why a flat "highest ground plus a margin" rule
/// puts such approaches too high. For one without a glidepath the aircraft levels off,
/// so the highest ground in the segment is what counts.
pub fn limiting_terrain(patch: &crate::sources::copernicus::Patch, thr_lat: f64, thr_lon: f64, final_track_deg: f64, approach: Approach, touchdown_elev_ft: f64) -> Option<f64> {
    let glidepath = matches!(approach, Approach::PrecisionCat1 | Approach::VerticallyGuided);
    let (length_nm, half_width_nm) = if glidepath { (10.0, 1.0) } else { (5.0, 1.5) };
    let samples = terrain_along(patch, thr_lat, thr_lon, final_track_deg, length_nm, half_width_nm);
    if samples.is_empty() {
        return None;
    }
    if !glidepath {
        return samples.iter().map(|(_, h)| *h).fold(f64::NEG_INFINITY, f64::max).into();
    }
    // Only ground that reaches within the margin of the 3 degree path counts, and then
    // it counts as the height the aircraft must not go below.
    let margin = approach.obstacle_margin_ft();
    let mut limiting = f64::NEG_INFINITY;
    for (along_nm, h) in samples {
        let path_ft = touchdown_elev_ft + 50.0 + (3.0f64).to_radians().tan() * along_nm * 6076.12;
        if h + margin > path_ft {
            limiting = limiting.max(h);
        }
    }
    limiting.is_finite().then_some(limiting)
}

/// Heights in the approach corridor, with how far each is from the threshold.
fn terrain_along(patch: &crate::sources::copernicus::Patch, thr_lat: f64, thr_lon: f64, final_track_deg: f64, length_nm: f64, half_width_nm: f64) -> Vec<(f64, f64)> {
    // The approach comes *from* the reciprocal of the landing direction.
    let back = (final_track_deg + 180.0).to_radians();
    let (m_per_deg_lat, m_per_deg_lon) = (111_320.0, 111_320.0 * thr_lat.to_radians().cos().max(0.05));
    let mut out = Vec::new();
    for row in 0..patch.height {
        for col in 0..patch.width {
            let h = patch.at(row, col);
            if !h.is_finite() {
                continue;
            }
            let (lat, lon) = patch.position(row, col);
            // Distance along the approach path, and to the side of it, in metres.
            let (dn, de) = ((lat - thr_lat) * m_per_deg_lat, (lon - thr_lon) * m_per_deg_lon);
            let along = dn * back.cos() + de * back.sin();
            let across = -dn * back.sin() + de * back.cos();
            if along < -500.0 || along > length_nm * 1852.0 || across.abs() > half_width_nm * 1852.0 {
                continue;
            }
            out.push((along / 1852.0, h as f64 / 0.3048));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_airports_sit_on_the_system_minimum() {
        // Heathrow: 83 ft touchdown zone, nothing high nearby. Charted as 283 ft.
        let e = estimate(Approach::PrecisionCat1, 83.0, 120.0);
        assert_eq!(e.altitude_ft, 283.0);
        assert_eq!(e.limited_by, LimitedBy::SystemMinimum);
        assert!(e.reliable);
    }

    #[test]
    fn terrain_raises_the_minimum_and_marks_it_an_estimate() {
        // A non-precision approach with a 2000 ft ridge under it.
        let e = estimate(Approach::NonPrecision, 600.0, 2000.0);
        assert_eq!(e.altitude_ft, 2250.0); // 2000 + 250
        assert_eq!(e.limited_by, LimitedBy::Terrain);
        assert!(!e.reliable);
    }

    #[test]
    fn minima_are_rounded_up_never_down() {
        assert_eq!(round_up(201.0, 10.0), 210.0);
        assert_eq!(round_up(200.0, 10.0), 200.0);
    }
}
