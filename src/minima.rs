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

    /// Whether the approach is flown down a glidepath, which decides which rule its
    /// obstacle clearance follows.
    pub fn has_glidepath(self) -> bool {
        matches!(self, Approach::PrecisionCat1 | Approach::VerticallyGuided)
    }

    /// Clearance required above the controlling obstacle, in feet. Only used where there
    /// is no glidepath; with one, the clearance surface does this job.
    pub fn obstacle_margin_ft(self) -> f64 {
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
    /// Terrain along the approach. An estimate.
    Terrain,
    /// An obstacle under the approach: a mast, a tower, a chimney. Surveyed where it
    /// comes from the FAA's obstacle file, mapped by hand where it comes from
    /// OpenStreetMap.
    Obstacle,
    /// The procedure's own minimum, which is not an estimate at all: the navigation data
    /// codes the altitude the approach descends to, and where it does, that is the
    /// published figure.
    Coded,
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
    /// The obstacle that set the minimum, when one did: what it is, and how high its
    /// top is above sea level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obstacle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obstacle_top_ft: Option<f64>,
    /// True when the published figure should match this one.
    pub reliable: bool,
}

/// Rounded up, never down: a minimum may be higher than the calculation, never lower.
fn round_up(ft: f64, step: f64) -> f64 {
    (ft / step).ceil() * step
}

/// Something under the approach, and the altitude it forces.
#[derive(Debug, Clone)]
pub struct Limit {
    /// Top of the thing, feet above sea level.
    pub top_ft: f64,
    /// The lowest altitude an approach may use because of it.
    pub required_ft: f64,
    /// What it is, for the chart to name.
    pub what: String,
}

/// The obstacle clearance surface under an approach with a glidepath, in feet above the
/// touchdown zone, at a point so far along the approach and so far to the side of it.
///
/// The surface is not a flat ramp. Along the approach it rises at 102 to 1 for a three
/// degree glidepath, which is gentle: a mile out it is only sixty feet up. Sideways it
/// rises far faster, and that is what keeps ordinary masts and towers either side of an
/// airport from setting the minimum. Within four hundred feet of the centreline the
/// surface is flat across; beyond that it climbs one foot for every four out, and beyond
/// three thousand feet one for every seven. A two hundred foot tower a third of a mile
/// to the side sits under four hundred feet of surface and does not touch the approach,
/// which is exactly what the published charts say.
fn clearance_surface_ft(touchdown_elev_ft: f64, along_nm: f64, across_nm: f64) -> f64 {
    const FT_PER_NM: f64 = 6076.115;
    let along_ft = along_nm.max(0.0) * FT_PER_NM;
    let across_ft = across_nm.abs() * FT_PER_NM;
    let sideways_ft = if across_ft <= 400.0 {
        0.0
    } else if across_ft <= 3000.0 {
        (across_ft - 400.0) / 4.0
    } else {
        (3000.0 - 400.0) / 4.0 + (across_ft - 3000.0) / 7.0
    };
    touchdown_elev_ft + along_ft / 102.0 + sideways_ft
}

/// How far to the side the obstacle clearance stays at its full value, and how far out
/// it has faded to nothing. Without a glidepath the approach is protected by a margin
/// rather than a surface, and that margin tapers away towards the edge of the area, so
/// a mountainside at the very edge does not carry the same weight as a hill on the
/// centreline.
const PRIMARY_NM: f64 = 0.8;

/// The altitude one thing on the ground forces, or `None` if it forces nothing.
pub fn required_altitude(approach: Approach, touchdown_elev_ft: f64, top_ft: f64, along_nm: f64, across_nm: f64) -> Option<f64> {
    if !approach.has_glidepath() {
        let (_, _, edge_nm) = corridor(approach);
        let taper = if across_nm.abs() <= PRIMARY_NM {
            1.0
        } else {
            (1.0 - (across_nm.abs() - PRIMARY_NM) / (edge_nm - PRIMARY_NM).max(0.01)).clamp(0.0, 1.0)
        };
        return Some(top_ft + approach.obstacle_margin_ft() * taper);
    }
    let penetration = top_ft - clearance_surface_ft(touchdown_elev_ft, along_nm, across_nm);
    (penetration > 0.0).then(|| touchdown_elev_ft + approach.system_minimum_ft() + penetration)
}

/// The minimum the procedure itself codes, where it codes one.
///
/// An approach without a glidepath ends its final segment at a missed approach point
/// rather than at the runway, and the altitude on that last leg is the altitude the
/// aircraft may descend to: the published minimum. An approach with a glidepath ends at
/// the runway itself, and that altitude is the height it crosses the threshold at, which
/// is not a minimum at all — so only the first kind gives us one.
pub fn coded_minimum(final_legs: &[&crate::sources::msfs::procedures::Leg], touchdown_elev_ft: f64) -> Option<f64> {
    let last = final_legs.last()?;
    if last.fix.starts_with("RW") || last.fix.is_empty() {
        return None;
    }
    let altitude = last.altitude_ft?;
    // A published minimum is never at the ground and never in the flight levels.
    (altitude > touchdown_elev_ft + 150.0 && altitude < touchdown_elev_ft + 5000.0).then_some(altitude)
}

/// An estimate that simply reports the procedure's own minimum.
pub fn from_coded(approach: Approach, touchdown_elev_ft: f64, coded_ft: f64, highest_terrain_ft: f64) -> Estimate {
    Estimate {
        approach,
        altitude_ft: coded_ft,
        height_ft: coded_ft - touchdown_elev_ft,
        limited_by: LimitedBy::Coded,
        highest_terrain_ft,
        obstacle: None,
        obstacle_top_ft: None,
        reliable: true,
    }
}

/// Estimate the minimum for one approach.
///
/// A system-limited answer is left exactly as the rule gives it, because that is how it
/// is published: Heathrow's 200 ft over an 83 ft touchdown zone is charted as 283 ft.
/// An answer that something on the ground pushed up is an estimate, so it is rounded up
/// to the next 10 ft rather than pretending to a precision it does not have.
pub fn estimate_with(approach: Approach, touchdown_elev_ft: f64, terrain: Option<Limit>, obstacle: Option<Limit>) -> Estimate {
    let by_system = touchdown_elev_ft + approach.system_minimum_ft();
    let by_terrain = terrain.as_ref().map(|l| l.required_ft).unwrap_or(f64::NEG_INFINITY);
    let by_obstacle = obstacle.as_ref().map(|l| l.required_ft).unwrap_or(f64::NEG_INFINITY);
    let (altitude, limited_by) = if by_system >= by_terrain && by_system >= by_obstacle {
        (by_system, LimitedBy::SystemMinimum)
    } else if by_obstacle >= by_terrain {
        (by_obstacle, LimitedBy::Obstacle)
    } else {
        (by_terrain, LimitedBy::Terrain)
    };
    let altitude = if limited_by == LimitedBy::SystemMinimum { altitude } else { round_up(altitude, 10.0) };
    Estimate {
        approach,
        altitude_ft: altitude,
        height_ft: altitude - touchdown_elev_ft,
        limited_by,
        highest_terrain_ft: terrain.as_ref().map(|l| l.top_ft).unwrap_or(touchdown_elev_ft),
        obstacle: obstacle.as_ref().map(|l| l.what.clone()),
        obstacle_top_ft: obstacle.as_ref().map(|l| l.top_ft),
        reliable: limited_by == LimitedBy::SystemMinimum,
    }
}

/// The path an approach is flown down, as a line on the ground ending at the threshold.
///
/// Assessing the ground in a straight box out from the runway is only right where the
/// approach is straight. In a valley it is badly wrong: the box takes in the walls on
/// either side of a procedure that is actually threading between them, and the minimum
/// comes out thousands of feet too high. The fixes give the real path, so the ground is
/// read along that instead.
#[derive(Debug, Clone)]
pub struct Path {
    /// Ordered from the start of the approach to the threshold.
    points: Vec<(f64, f64)>,
    /// For each point, how far it is from the threshold along the path, in miles.
    from_threshold_nm: Vec<f64>,
}

fn nm_between(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dn = (b.0 - a.0) * 60.0;
    let de = (b.1 - a.1) * 60.0 * ((a.0 + b.0) / 2.0).to_radians().cos().max(0.05);
    (dn * dn + de * de).sqrt()
}

impl Path {
    /// From the fixes an approach is flown through, threshold last.
    pub fn new(points: Vec<(f64, f64)>) -> Option<Path> {
        if points.len() < 2 {
            return None;
        }
        let mut from_threshold_nm = vec![0.0; points.len()];
        for i in (0..points.len() - 1).rev() {
            from_threshold_nm[i] = from_threshold_nm[i + 1] + nm_between(points[i], points[i + 1]);
        }
        Some(Path { points, from_threshold_nm })
    }

    /// A straight approach, for when the data gives no fixes to follow.
    pub fn straight(threshold: (f64, f64), track_deg: f64, length_nm: f64) -> Path {
        let back = (track_deg + 180.0).to_radians();
        let start = (
            threshold.0 + length_nm * back.cos() / 60.0,
            threshold.1 + length_nm * back.sin() / 60.0 / threshold.0.to_radians().cos().max(0.05),
        );
        Path { points: vec![start, threshold], from_threshold_nm: vec![length_nm, 0.0] }
    }

    pub fn threshold(&self) -> (f64, f64) {
        *self.points.last().expect("a path always has points")
    }

    /// How far a point is from the threshold along the path, and how far it lies to the
    /// side of it, both in miles. The nearest part of the path is the one that counts.
    pub fn position(&self, lat: f64, lon: f64) -> Option<(f64, f64)> {
        let cos = self.threshold().0.to_radians().cos().max(0.05);
        let to_xy = |p: (f64, f64)| ((p.1 - self.threshold().1) * 60.0 * cos, (p.0 - self.threshold().0) * 60.0);
        let (px, py) = to_xy((lat, lon));
        let mut best: Option<(f64, f64)> = None;
        for i in 0..self.points.len() - 1 {
            let (ax, ay) = to_xy(self.points[i]);
            let (bx, by) = to_xy(self.points[i + 1]);
            let (dx, dy) = (bx - ax, by - ay);
            let len2 = dx * dx + dy * dy;
            if len2 < 1e-9 {
                continue;
            }
            let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
            let (cx, cy) = (ax + t * dx, ay + t * dy);
            let across = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let along = self.from_threshold_nm[i + 1] + (1.0 - t) * len2.sqrt();
            if best.as_ref().map(|(_, b)| across < *b).unwrap_or(true) {
                best = Some((along, across));
            }
        }
        best
    }
}

/// Circling is not flown along the approach at all: the aircraft manoeuvres visually
/// about the aerodrome, so what matters is everything within a radius of it, and the
/// radius depends on how fast the aeroplane is.
///
/// The figures are the ones procedure designers use: the area grows with category, and
/// no circling minimum may sit lower than a set height above the aerodrome whatever the
/// ground does. Everything in the area has to be cleared by the same margin.
pub const CIRCLING_AREA: [(char, f64, f64); 4] = [('A', 1.68, 394.0), ('B', 2.66, 492.0), ('C', 4.20, 591.0), ('D', 5.28, 700.0)];

/// The clearance kept above whatever stands in the circling area.
const CIRCLING_MARGIN_FT: f64 = 295.0;

/// The circling minimum for one aircraft category: what the highest thing within its
/// radius forces, or the floor for that category, whichever is higher.
pub fn circling_minimum(
    patch: &crate::sources::copernicus::Patch,
    obstacles: &[crate::sources::obstacles::Obstacle],
    airport: (f64, f64),
    aerodrome_elev_ft: f64,
    category: usize,
) -> (f64, Option<String>) {
    let (_, radius_nm, floor_ft) = CIRCLING_AREA[category.min(3)];
    let mut highest = f64::NEG_INFINITY;
    let mut what: Option<String> = None;
    let within = |lat: f64, lon: f64| {
        let dn = (lat - airport.0) * 60.0;
        let de = (lon - airport.1) * 60.0 * airport.0.to_radians().cos().max(0.05);
        dn.hypot(de) <= radius_nm
    };
    for row in 0..patch.height {
        for col in 0..patch.width {
            let h = patch.at(row, col);
            if !h.is_finite() {
                continue;
            }
            let (lat, lon) = patch.position(row, col);
            if within(lat, lon) {
                highest = highest.max(h as f64 / 0.3048);
            }
        }
    }
    for o in obstacles {
        let Some(top) = o.top_ft else { continue };
        if within(o.lat, o.lon) && top > highest {
            highest = top;
            what = Some(o.label());
        }
    }
    let by_ground = if highest.is_finite() { round_up(highest + CIRCLING_MARGIN_FT, 10.0) } else { f64::NEG_INFINITY };
    let by_floor = aerodrome_elev_ft + floor_ft;
    if by_ground > by_floor {
        (by_ground, what)
    } else {
        (by_floor, None)
    }
}

/// Terrain that matters to an approach: the sample that forces the highest altitude, by
/// the rule for this kind of approach.
pub fn limiting_terrain_limit(patch: &crate::sources::copernicus::Patch, path: &Path, approach: Approach, touchdown_elev_ft: f64) -> Option<Limit> {
    let mut best: Option<Limit> = None;
    for (along_nm, across_nm, top_ft) in terrain_along(patch, path, approach) {
        let Some(required_ft) = required_altitude(approach, touchdown_elev_ft, top_ft, along_nm, across_nm) else { continue };
        if best.as_ref().map(|b| required_ft > b.required_ft).unwrap_or(true) {
            best = Some(Limit { top_ft, required_ft, what: "terrain".to_string() });
        }
    }
    best
}

/// The highest ground along the approach, whether or not it changes the minimum. The
/// chart prints this, so it is worth having even when nothing was limited by it.
pub fn highest_terrain(patch: &crate::sources::copernicus::Patch, path: &Path, approach: Approach) -> Option<f64> {
    terrain_along(patch, path, approach)
        .into_iter()
        .map(|(_, _, ft)| ft)
        .fold(None, |acc: Option<f64>, ft| Some(acc.map_or(ft, |a| a.max(ft))))
}

/// The obstacle that forces the highest altitude, by the same rule.
pub fn limiting_obstacle(obstacles: &[crate::sources::obstacles::Obstacle], path: &Path, approach: Approach, touchdown_elev_ft: f64) -> Option<Limit> {
    let (start_nm, length_nm, half_width_nm) = corridor(approach);
    let mut best: Option<Limit> = None;
    for o in obstacles {
        let Some(top_ft) = o.top_ft else { continue };
        let Some((along_nm, across_nm)) = path.position(o.lat, o.lon) else { continue };
        if along_nm < start_nm || along_nm > length_nm || across_nm > half_width_nm {
            continue;
        }
        let Some(required_ft) = required_altitude(approach, touchdown_elev_ft, top_ft, along_nm, across_nm) else { continue };
        if best.as_ref().map(|b| required_ft > b.required_ft).unwrap_or(true) {
            best = Some(Limit { top_ft, required_ft, what: o.label() });
        }
    }
    best
}

/// Where the assessment starts, how far out it reaches and how far to the side, in
/// nautical miles.
///
/// An approach with a glidepath only reaches about a mile out, which is not obvious
/// until you work out where the aircraft is. It leaves the decision altitude a little
/// over half a mile from the threshold; a mast two miles out is passed before that, with
/// the aircraft six hundred feet above it, and cannot bear on the decision altitude at
/// all. It bears on the altitude the approach crosses its fixes at, which the procedure
/// already states. Assessing the whole ten miles was what put a hundred feet on the
/// minimum at every airport with a tower down the approach.
///
/// Without a glidepath the aircraft levels off and flies the last miles at one altitude,
/// so everything in the segment does have to be cleared.
fn corridor(approach: Approach) -> (f64, f64, f64) {
    if approach.has_glidepath() {
        (0.0, 1.2, 1.0)
    } else {
        (0.0, 6.0, 1.5)
    }
}

/// The same corridor, for terrain rather than for obstacles.
///
/// The elevation model is a model of the *surface*: within the airport fence it holds
/// hangars, terminals and trees, standing tens of feet over a runway whose own elevation
/// is what the minimum is measured from. Read literally, that turns every large airport
/// into one with an obstacle on short final. Real procedure design uses a survey of a
/// graded runway strip there, which we do not have, so the ground is read from a mile
/// out: far enough to be outside the airport, and still under the part of the glidepath
/// where a hill would matter. Obstacles, which are surveyed points with their own
/// heights, are assessed the whole way in.
fn terrain_corridor(approach: Approach) -> (f64, f64, f64) {
    let (_, length, width) = corridor(approach);
    (if approach.has_glidepath() { 0.25 } else { 0.5 }, length, width)
}

/// Heights along the approach: how far each is from the threshold, how far to the side,
/// and how high it is.
fn terrain_along(patch: &crate::sources::copernicus::Patch, path: &Path, approach: Approach) -> Vec<(f64, f64, f64)> {
    let (start_nm, length_nm, half_width_nm) = terrain_corridor(approach);
    let mut out = Vec::new();
    for row in 0..patch.height {
        for col in 0..patch.width {
            let h = patch.at(row, col);
            if !h.is_finite() {
                continue;
            }
            let (lat, lon) = patch.position(row, col);
            let Some((along_nm, across_nm)) = path.position(lat, lon) else { continue };
            if along_nm < start_nm || along_nm > length_nm || across_nm > half_width_nm {
                continue;
            }
            out.push((along_nm, across_nm, h as f64 / 0.3048));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A limit from something whose distance from the threshold is known.
    fn limit(approach: Approach, tdze: f64, top_ft: f64, distance_nm: f64) -> Option<Limit> {
        required_altitude(approach, tdze, top_ft, distance_nm, 0.0).map(|required_ft| Limit { top_ft, required_ft, what: "terrain".into() })
    }

    #[test]
    fn flat_airports_sit_on_the_system_minimum() {
        // Heathrow: 83 ft touchdown zone, nothing high nearby. Charted as 283 ft.
        let terrain = limit(Approach::PrecisionCat1, 83.0, 120.0, 3.0);
        let e = estimate_with(Approach::PrecisionCat1, 83.0, terrain, None);
        assert_eq!(e.altitude_ft, 283.0);
        assert_eq!(e.limited_by, LimitedBy::SystemMinimum);
        assert!(e.reliable);
    }

    #[test]
    fn terrain_raises_the_minimum_and_marks_it_an_estimate() {
        // A non-precision approach with a 2000 ft ridge under it: no glidepath to hide
        // beneath, so the whole ridge has to be cleared.
        let terrain = limit(Approach::NonPrecision, 600.0, 2000.0, 3.0);
        let e = estimate_with(Approach::NonPrecision, 600.0, terrain, None);
        assert_eq!(e.altitude_ft, 2250.0); // 2000 + 250
        assert_eq!(e.limited_by, LimitedBy::Terrain);
        assert!(!e.reliable);
    }

    #[test]
    fn a_mast_below_the_clearance_surface_changes_nothing() {
        // Four miles out the surface is 238 ft above the touchdown zone, so a 300 ft
        // top clears underneath it and the approach keeps its floor.
        let tdze = 100.0;
        assert_eq!(required_altitude(Approach::PrecisionCat1, tdze, 300.0, 4.0, 0.0), None);
        // The same top a mile from the threshold does stick through, and lifts the
        // decision altitude by as much as it sticks through.
        let required = required_altitude(Approach::PrecisionCat1, tdze, 300.0, 1.0, 0.0).unwrap();
        let penetration = 300.0 - (tdze + 6076.115 / 102.0);
        assert!((required - (tdze + 200.0 + penetration)).abs() < 0.01);
    }

    #[test]
    fn a_tower_off_to_the_side_does_not_set_the_minimum() {
        // The same tower that matters on the centreline is under the surface a third of
        // a mile to the side, where the surface has climbed four hundred feet.
        let tdze = 100.0;
        assert!(required_altitude(Approach::PrecisionCat1, tdze, 300.0, 1.0, 0.0).is_some());
        assert_eq!(required_altitude(Approach::PrecisionCat1, tdze, 300.0, 1.0, 0.33), None);
    }

    #[test]
    fn clearance_fades_towards_the_edge_of_a_non_precision_area() {
        // On the centreline the whole margin applies; at the outer edge, none of it.
        let on = required_altitude(Approach::NonPrecision, 100.0, 400.0, 3.0, 0.0).unwrap();
        let edge = required_altitude(Approach::NonPrecision, 100.0, 400.0, 3.0, 1.5).unwrap();
        assert_eq!(on, 650.0);
        assert_eq!(edge, 400.0);
    }

    #[test]
    fn without_a_glidepath_everything_has_to_be_cleared() {
        // No surface to hide under: the margin applies wherever the obstacle is.
        assert_eq!(required_altitude(Approach::NonPrecision, 100.0, 400.0, 4.0, 0.0), Some(650.0));
    }

    #[test]
    fn a_path_measures_along_itself_and_out_to_the_side() {
        // Two miles of approach running due north to a threshold at the origin.
        let path = Path::new(vec![(-2.0 / 60.0, 0.0), (0.0, 0.0)]).unwrap();
        let (along, across) = path.position(-1.0 / 60.0, 0.0).unwrap();
        assert!((along - 1.0).abs() < 0.01, "{along}");
        assert!(across < 0.01, "{across}");
        // A point a mile to the east of the halfway mark.
        let (along, across) = path.position(-1.0 / 60.0, 1.0 / 60.0).unwrap();
        assert!((along - 1.0).abs() < 0.01, "{along}");
        assert!((across - 1.0).abs() < 0.02, "{across}");
    }

    #[test]
    fn a_straight_path_starts_where_the_approach_does() {
        let path = Path::straight((50.0, 0.0), 360.0, 10.0);
        // The approach comes from the south when landing north.
        assert!(path.points[0].0 < 50.0);
        assert_eq!(path.threshold(), (50.0, 0.0));
    }

    #[test]
    fn minima_are_rounded_up_never_down() {
        assert_eq!(round_up(201.0, 10.0), 210.0);
        assert_eq!(round_up(200.0, 10.0), 200.0);
    }
}
