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

use crate::dispatch::{distance_nm, LatLon};

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
