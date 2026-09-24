//! The region a route is worth searching in at all.
//!
//! An airway network is worldwide, but a route between two airports is not: nothing that
//! could not plausibly lie on the way between them is worth the search touching, whether
//! that means a neighbour the search's own expansion could wander off to, or a fix offered a
//! free-route direct leg in [`super::directs`]. The region is the classic one for "plausibly
//! on the way between two points" — an ellipse with the origin and destination as its foci,
//! since a point on it has one fixed property: the distance from one focus to the point plus
//! the point to the other focus is constant, and inside it that sum is less. A factor just
//! over one draws a thin sliver hugging the great circle; a larger one a fatter ellipse that
//! tolerates a real detour round weather, an airway's own zig-zag, or a coastline.
//!
//! A percentage alone is the wrong shape for this: ten per cent of a hundred-and-fifty-mile
//! hop is fifteen miles, nowhere near enough width to hold a real route once an airway
//! doglegs even slightly, so the limit is `slack_nm` clear of the direct distance whenever
//! the percentage alone would be thinner than that.

use crate::dispatch::{along, bearing_deg, distance_nm, travel, Bounds, LatLon};

/// An ellipse with two positions as its foci, sized as the greater of a multiple of the
/// direct distance between them and a fixed slack beyond it.
pub struct Ellipse {
    origin: LatLon,
    destination: LatLon,
    /// The direct distance and the limit are both worked out once, in [`Ellipse::new`], so
    /// that [`Ellipse::contains`] — called for every fix a search or a direct-leg build
    /// looks at — is nothing but two more distances and a comparison.
    limit_nm: f64,
}

impl Ellipse {
    /// `factor` widens the ellipse as a multiple of the direct distance; `slack_nm` is the
    /// floor beneath which a percentage alone would be too thin to hold a real route.
    pub fn new(origin: LatLon, destination: LatLon, factor: f64, slack_nm: f64) -> Ellipse {
        let direct = distance_nm(origin, destination);
        let limit_nm = (direct * factor).max(direct + slack_nm);
        Ellipse { origin, destination, limit_nm }
    }

    /// Whether a point could plausibly lie on the way from the origin to the destination:
    /// the defining property of an ellipse, the sum of the distances to its two foci within
    /// the limit fixed when it was built.
    pub fn contains(&self, p: LatLon) -> bool {
        distance_nm(self.origin, p) + distance_nm(p, self.destination) <= self.limit_nm
    }

    pub fn limit_nm(&self) -> f64 {
        self.limit_nm
    }

    /// How far the ellipse reaches either side of the great circle at a fraction of the way
    /// along it, nautical miles. It is widest halfway between the foci and narrows towards
    /// them, though not to nothing: an ellipse closes at its vertices, which lie beyond its
    /// foci, so at a focus itself it is still about as wide as the slack is long.
    ///
    /// The textbook figure — `sqrt(a^2 - c^2)` scaled towards the foci — is a plane's, and this
    /// ellipse is drawn on a sphere with great-circle distances, so on a long route the two
    /// disagree by enough to matter. The answer is found instead by walking out perpendicular
    /// to the track until [`Ellipse::contains`] stops holding, which makes the width exactly
    /// the width of *this* ellipse rather than of its flat approximation. It costs a few dozen
    /// distances, and it is asked once per strip rather than once per fix.
    pub fn half_width_nm(&self, fraction: f64) -> f64 {
        let at = along(self.origin, self.destination, fraction.clamp(0.0, 1.0));
        let across = bearing_deg(self.origin, self.destination) + 90.0;
        // Nothing can reach further off the line than the slack itself allows.
        let mut hi = self.limit_nm;
        if self.contains(travel(at, across, hi)) {
            return hi;
        }
        let mut lo = 0.0;
        for _ in 0..40 {
            let mid = (lo + hi) / 2.0;
            if self.contains(travel(at, across, mid)) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The ellipse as a run of latitude and longitude rectangles along the great circle
    /// between its foci.
    ///
    /// A source that answers for an area answers for a rectangle — the global forecast model's
    /// own subsetting service does, and so does every other one worth asking — and the
    /// rectangle round the whole of a long ellipse is very nearly the rectangle round half the
    /// world. The way to ask a rectangular source for a curved region is the way the area
    /// under a curve is worked out: in strips. Each strip is the rectangle round one short arc
    /// of the great circle, widened by the ellipse's own half-width there, and the union of
    /// them hugs the ellipse the more closely the more strips there are. The total area falls
    /// with it, because the corridor is thin and its bounding box is not.
    ///
    /// It also fixes something a single box gets plainly wrong. The box round two airports is
    /// the box round two *points*: Bangalore to New York gives one reaching 45 degrees north,
    /// while the great circle between them runs past 70. A route planned in that box leaves it
    /// almost immediately and is planned on whatever lies at its edge. Strips follow the
    /// circle, so they contain the route by construction.
    ///
    /// `margin_nm` widens every strip beyond the ellipse, for a route that wanders a little
    /// further than the search's own region allows for.
    /// How many strips to cut the corridor into, for a corridor of this length.
    ///
    /// More is not better, which is the one surprising thing about this. The cost of asking is
    /// not the area of the corridor but the sum of the areas of the strips' own rectangles, and
    /// a short strip lying diagonally has a rectangle far larger than itself — cut a diagonal
    /// band into enough pieces and the pieces' boxes cost more than one box round the whole
    /// thing. Measured over routes from Heathrow-Paris to Heathrow-Sydney, the least is always
    /// between two and eight strips, and one strip per twelve hundred miles lands within about
    /// a tenth of the best on every one of them.
    pub fn strips_for(direct_nm: f64) -> usize {
        ((direct_nm / 1_200.0).ceil() as usize).clamp(1, 8)
    }

    /// The corridor cut into as many strips as its length warrants.
    pub fn corridor(&self, margin_nm: f64) -> Vec<Bounds> {
        self.tiles(Ellipse::strips_for(distance_nm(self.origin, self.destination)), margin_nm)
    }

    pub fn tiles(&self, strips: usize, margin_nm: f64) -> Vec<Bounds> {
        let strips = strips.max(1);
        let n = strips as f64;
        (0..strips)
            .map(|k| {
                let (lo, hi) = (k as f64 / n, (k + 1) as f64 / n);
                // The widest the ellipse gets anywhere along this strip, so the strip covers
                // the whole of its own arc: at whichever end is nearer the centre, or at the
                // centre itself where the strip straddles it.
                let widest = if lo <= 0.5 && hi >= 0.5 { 0.5 } else if (lo - 0.5).abs() < (hi - 0.5).abs() { lo } else { hi };
                let half = self.half_width_nm(widest) + margin_nm;
                // Widened across the track only, never along it. Padding a strip in the
                // direction it already runs buys nothing and is paid for twice, because the
                // next strip covers the same ground: the strips would overlap by more than
                // they advance, and asking for a corridor would cost more than asking for the
                // box round the whole of it.
                let mut corners = Vec::with_capacity((SAMPLES_PER_STRIP + 1) * 2);
                for step in 0..=SAMPLES_PER_STRIP {
                    let f = lo + (hi - lo) * step as f64 / SAMPLES_PER_STRIP as f64;
                    let on_line = along(self.origin, self.destination, f);
                    // The track turns along a great circle, so the perpendicular is taken
                    // where the strip actually is rather than where it started.
                    let ahead = along(self.origin, self.destination, (f + 1e-4).min(1.0));
                    let across = if f >= 1.0 { bearing_deg(along(self.origin, self.destination, f - 1e-4), on_line) } else { bearing_deg(on_line, ahead) } + 90.0;
                    corners.push(travel(on_line, across, half));
                    corners.push(travel(on_line, across + 180.0, half));
                }
                box_round(&corners)
            })
            .collect()
    }
}

/// How many places along a strip the corridor's edges are measured at. A strip is a straight
/// rectangle standing in for a curved band, so its edges are sampled rather than taken from its
/// two ends alone, which would cut the corner off every turn the great circle makes.
const SAMPLES_PER_STRIP: usize = 8;

/// The smallest latitude and longitude rectangle holding every one of a set of places, taking
/// the shorter way round in longitude where that is the shorter way — a corridor from Asia to
/// America crosses the date line or the prime meridian, and either way the box must be the one
/// that wraps rather than the one that spans the whole world the other way.
fn box_round(points: &[LatLon]) -> Bounds {
    let (mut south, mut north) = (90.0f64, -90.0f64);
    for p in points {
        south = south.min(p.0);
        north = north.max(p.0);
    }
    // The longitudes are unrolled onto a continuous line, each one taken to the turn nearest
    // the one before, so a set straddling the date line reads as a short run rather than as two
    // clusters 360 degrees apart.
    let mut unrolled = Vec::with_capacity(points.len());
    let mut previous = points.first().map(|p| p.1).unwrap_or(0.0);
    for p in points {
        let shifted = previous + (p.1 - previous + 180.0).rem_euclid(360.0) - 180.0;
        unrolled.push(shifted);
        previous = shifted;
    }
    let lo = unrolled.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = unrolled.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let (west, east) = if hi - lo >= 360.0 {
        (-180.0, 180.0)
    } else {
        (((lo + 180.0).rem_euclid(360.0)) - 180.0, ((hi + 180.0).rem_euclid(360.0)) - 180.0)
    };
    Bounds { south, north, west, east }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_on_the_direct_line_is_always_inside() {
        let o = (51.0, 0.0);
        let d = (40.0, -74.0);
        let ell = Ellipse::new(o, d, 1.01, 10.0);
        let mid = crate::dispatch::along(o, d, 0.5);
        assert!(ell.contains(mid));
        assert!(ell.contains(o));
        assert!(ell.contains(d));
    }

    #[test]
    fn a_point_far_off_the_line_is_excluded_by_a_thin_ellipse() {
        let o = (51.0, 0.0);
        let d = (40.0, -74.0);
        // Lisbon is nowhere near the great-circle track from London to New York.
        let lisbon = (38.77, -9.13);
        let thin = Ellipse::new(o, d, 1.02, 20.0);
        assert!(!thin.contains(lisbon));
        // The same point is inside a fat enough ellipse: this is what "widen and try again"
        // relies on rather than a fix simply being unreachable outright.
        let fat = Ellipse::new(o, d, 3.0, 20.0);
        assert!(fat.contains(lisbon));
    }

    #[test]
    fn the_slack_floor_widens_a_short_hop_a_percentage_alone_would_starve() {
        let o = (51.4706, -0.4619);
        let d = (49.0097, 2.5479); // EGLL-LFPG, a couple of hundred miles.
        let direct = distance_nm(o, d);
        let percent_only = Ellipse::new(o, d, 1.1, 0.0);
        let with_slack = Ellipse::new(o, d, 1.1, 120.0);
        assert!(with_slack.limit_nm() > percent_only.limit_nm());
        assert!(with_slack.limit_nm() >= direct + 120.0 - 1e-6);
    }
}

#[cfg(test)]
mod tiling_tests {
    use super::*;

    /// Every point on the great circle is inside some strip. This is the property the whole
    /// thing exists for: a route planned on a wind field fetched over these strips is never
    /// planned on air from outside them.
    #[test]
    fn the_strips_contain_the_whole_great_circle() {
        for (o, d) in [((13.2, 77.7), (40.6, -73.8)), ((51.5, -0.5), (-33.9, 151.2)), ((35.6, 139.8), (37.6, -122.4)), ((-34.8, -58.5), (1.4, 103.9))] {
            let ell = Ellipse::new(o, d, 1.0, 1.0);
            let tiles = ell.tiles(24, 300.0);
            for k in 0..=400 {
                let p = along(o, d, k as f64 / 400.0);
                assert!(tiles.iter().any(|t| t.contains(p)), "{o:?} to {d:?}: {p:?} is in no strip");
            }
        }
    }

    /// And the corridor costs less to ask for than one box round the whole of it. The
    /// comparison has to be against a box that contains the route, not the box round the two
    /// airports, because on two of these five routes that box does not contain it at all.
    #[test]
    fn the_corridor_costs_less_than_one_box_round_all_of_it() {
        for (o, d) in [((13.2, 77.7), (40.6, -73.8)), ((51.5, -0.5), (40.6, -73.8)), ((51.5, -0.5), (-33.9, 151.2)), ((1.4, 103.9), (-33.9, 151.2))] {
            let ell = Ellipse::new(o, d, 1.0, 1.0);
            let corridor = ell.corridor(300.0);
            let strips: f64 = corridor.iter().map(|b| (b.north - b.south) * span_deg(*b)).sum();
            let one = box_round(&corridor.iter().flat_map(|b| [(b.south, b.west), (b.north, b.east)]).collect::<Vec<_>>());
            let whole = (one.north - one.south) * span_deg(one);
            assert!(strips < whole, "strips {strips:.0} deg^2 against one box {whole:.0} deg^2");
        }
    }

    /// Printed, not asserted: what the corridor actually costs against what one box costs,
    /// for routes of different shapes, so the choice of strip count is made on measurements.
    #[test]
    #[ignore]
    fn measure_corridor_against_one_box() {
        for (name, o, d) in [
            ("VOBL-KJFK polar", (13.2, 77.7), (40.6, -73.8)),
            ("EGLL-KJFK atlantic", (51.5, -0.5), (40.6, -73.8)),
            ("EGLL-YSSY antipodal", (51.5, -0.5), (-33.9, 151.2)),
            ("WSSS-YSSY equator", (1.4, 103.9), (-33.9, 151.2)),
            ("EGLL-LFPG short", (51.5, -0.5), (49.0, 2.5)),
        ] {
            let endpoints = Bounds::around(o, d, 300.0);
            let mut on_line: Vec<LatLon> = (0..=200).map(|k| along(o, d, k as f64 / 200.0)).collect();
            on_line.extend((0..=200).map(|k| {
                let f = k as f64 / 200.0;
                let at = along(o, d, f);
                travel(at, bearing_deg(o, d) + 90.0, 300.0)
            }));
            on_line.extend((0..=200).map(|k| {
                let f = k as f64 / 200.0;
                let at = along(o, d, f);
                travel(at, bearing_deg(o, d) - 90.0, 300.0)
            }));
            let containing = box_round(&on_line);
            let ell = Ellipse::new(o, d, 1.0, 1.0);
            println!("{name}");
            println!("  endpoints box {:8.0} deg^2  (contains the line: {})", (endpoints.north - endpoints.south) * span_deg(endpoints), (0..=100).all(|k| endpoints.contains(along(o, d, k as f64 / 100.0))));
            println!("  containing box{:8.0} deg^2", (containing.north - containing.south) * span_deg(containing));
            for strips in [1, 2, 4, 8, 16, 32, 64] {
                let t = ell.tiles(strips, 300.0);
                let area: f64 = t.iter().map(|b| (b.north - b.south) * span_deg(*b)).sum();
                println!("  {strips:3} strips   {area:8.0} deg^2");
            }
        }
    }

    fn span_deg(b: Bounds) -> f64 {
        if b.west <= b.east { b.east - b.west } else { b.east + 360.0 - b.west }
    }

    /// The ellipse is widest halfway between its foci and narrows towards them. It does not
    /// narrow to nothing there: it closes at its vertices, which lie beyond the foci.
    #[test]
    fn the_half_width_is_widest_between_the_foci_and_narrows_towards_them() {
        let (o, d) = ((51.0, 0.0), (40.0, -74.0));
        let ell = Ellipse::new(o, d, 1.1, 10.0);
        assert!(ell.half_width_nm(0.5) > ell.half_width_nm(0.25));
        assert!(ell.half_width_nm(0.25) > ell.half_width_nm(0.0));
        assert!(ell.half_width_nm(0.0) > 0.0, "a focus is inside the ellipse, not on it");
        // The width is found from `contains` itself, so a point just inside it is inside the
        // ellipse and a point just outside it is not — at the middle and at a focus alike.
        let track = bearing_deg(o, d);
        for f in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let at = along(o, d, f);
            let w = ell.half_width_nm(f);
            assert!(ell.contains(travel(at, track + 90.0, w * 0.99)), "inside at {f}");
            assert!(!ell.contains(travel(at, track + 90.0, w * 1.01 + 1.0)), "outside at {f}");
        }
    }
}
