//! Whether a straight line crosses a hazard's outline: one piece of geometry every part of
//! planning that keeps out of shapes needs — the search filtering a segment out, the
//! customisation step pricing one, the refinement checking a direct leg is clear — done
//! once, and done correctly across the ±180° meridian.
//!
//! A flat projection about a leg's own middle is accurate enough at the size of a weather
//! area or a restricted zone, but only if "the middle" and "how far east" are worked out
//! the short way round the earth. Simply averaging two longitudes, or subtracting one from
//! another, breaks the moment a leg or a shape straddles the date line: halfway from 179°E
//! to 179°W is the date line itself, not 0°, and 179°E is one degree from 179°W, not 358.

use crate::dispatch::{along, LatLon};

/// The great-circle midpoint of a leg. Correct across the antimeridian because it is built
/// from bearing and distance, both already periodic in longitude, rather than by averaging
/// the two longitudes directly.
pub fn midpoint(a: LatLon, b: LatLon) -> LatLon {
    along(a, b, 0.5)
}

/// A longitude difference brought into (-180°, 180°]: how far east a point lies of a
/// reference meridian, the short way round.
fn wrap_lon(dlon: f64) -> f64 {
    let d = dlon.rem_euclid(360.0);
    if d > 180.0 {
        d - 360.0
    } else {
        d
    }
}

/// Whether a straight line between two places crosses or enters a shape, worked on a flat
/// projection about the *shape's* own middle, not the line's.
///
/// A projection about the line's own midpoint was the first attempt here, and it is wrong
/// for a shape nowhere near the line: wrapping the shape's far side into (-180°, 180°] of a
/// distant reference point can turn a compact hazard the size of a thunderstorm into one
/// that appears to reach most of the way round the earth. Unwrapping the shape's own
/// points against one another — each the short way from the last, the way longitudes
/// unwrap into a continuous angle — keeps it the size it actually is; the line's endpoints
/// are then placed in that same local frame the short way from the shape's own reference
/// meridian, which correctly puts a line nowhere near the shape nowhere near it in the
/// projection either.
pub fn crosses(a: LatLon, b: LatLon, polygon: &[LatLon]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let ref_lon = polygon[0].1;
    let mut unwrapped = Vec::with_capacity(polygon.len());
    let mut prev_lon = ref_lon;
    for p in polygon {
        let lon = prev_lon + wrap_lon(p.1 - prev_lon);
        unwrapped.push((p.0, lon));
        prev_lon = lon;
    }
    let mean_lat = polygon.iter().map(|p| p.0).sum::<f64>() / polygon.len() as f64;
    let cos = mean_lat.to_radians().cos().max(0.05);
    let flat = |lat: f64, lon: f64| ((lon - ref_lon) * cos, lat - mean_lat);
    let shape: Vec<(f64, f64)> = unwrapped.iter().map(|&(lat, lon)| flat(lat, lon)).collect();
    let near_lon = |lon: f64| ref_lon + wrap_lon(lon - ref_lon);
    let p = flat(a.0, near_lon(a.1));
    let q = flat(b.0, near_lon(b.1));
    if inside(p, &shape) || inside(q, &shape) {
        return true;
    }
    (0..shape.len()).any(|i| segments_cross(p, q, shape[i], shape[(i + 1) % shape.len()]))
}

fn inside(p: (f64, f64), shape: &[(f64, f64)]) -> bool {
    let mut odd = false;
    let mut j = shape.len() - 1;
    for i in 0..shape.len() {
        let (a, b) = (shape[i], shape[j]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            odd = !odd;
        }
        j = i;
    }
    odd
}

fn segments_cross(p: (f64, f64), q: (f64, f64), r: (f64, f64), s: (f64, f64)) -> bool {
    let side = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
    let (d1, d2) = (side(r, s, p), side(r, s, q));
    let (d3, d4) = (side(p, q, r), side(p, q, s));
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0)
}

/// A hazard's outline ready for repeated testing: the points, a box round them for a cheap
/// rejection, and how far across the box is. The box is conservative rather than exact
/// where the outline itself straddles the date line — it may be wider than it needs to be
/// there, which costs a little speed and never correctness, since a wide box only means
/// fewer legs are rejected before the real test runs.
pub struct Shape {
    polygon: Vec<LatLon>,
    south: f64,
    north: f64,
    west: f64,
    east: f64,
    pub across_nm: f64,
}

impl Shape {
    pub fn new(polygon: &[LatLon]) -> Shape {
        let (mut s, mut n, mut w, mut e) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for (lat, lon) in polygon {
            s = s.min(*lat);
            n = n.max(*lat);
            w = w.min(*lon);
            e = e.max(*lon);
        }
        Shape { polygon: polygon.to_vec(), south: s, north: n, west: w, east: e, across_nm: crate::dispatch::distance_nm((s, w), (n, e)) }
    }

    pub fn crossed_by(&self, a: LatLon, b: LatLon) -> bool {
        if a.0.max(b.0) < self.south || a.0.min(b.0) > self.north || a.1.max(b.1) < self.west || a.1.min(b.1) > self.east {
            return false;
        }
        crosses(a, b, &self.polygon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_midpoint_of_a_leg_over_the_date_line_is_near_it_not_at_zero() {
        let a = (10.0, 170.0);
        let b = (10.0, -170.0);
        let mid = midpoint(a, b);
        // Averaging the raw longitudes gives 0.0, on the wrong side of the world; the
        // great-circle midpoint sits near ±180 instead.
        assert!(mid.1.abs() > 170.0, "{:?}", mid);
        assert!((crate::dispatch::distance_nm(a, b) - 1200.0).abs() < 50.0);
    }

    #[test]
    fn a_shape_straddling_the_date_line_is_crossed_correctly() {
        let polygon = vec![(9.0, 175.0), (9.0, -175.0), (11.0, -175.0), (11.0, 175.0)];
        // A leg that runs straight through the sliver either side of the line.
        assert!(crosses((10.0, 178.0), (10.0, -178.0), &polygon));
        // A leg well clear of it, on the other side of the earth.
        assert!(!crosses((10.0, 0.0), (10.0, 5.0), &polygon));
    }

    #[test]
    fn a_shape_is_entered_and_left_correctly_without_the_date_line() {
        let polygon = vec![(0.0, 0.0), (0.0, 2.0), (2.0, 2.0), (2.0, 0.0)];
        assert!(crosses((1.0, -1.0), (1.0, 3.0), &polygon));
        assert!(!crosses((5.0, -1.0), (5.0, 3.0), &polygon));
        let shape = Shape::new(&polygon);
        assert!(shape.crossed_by((1.0, -1.0), (1.0, 3.0)));
        assert!(!shape.crossed_by((5.0, -1.0), (5.0, 3.0)));
    }
}
