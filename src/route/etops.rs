//! ETOPS, or EDTO: a twin must never be further than a set time, flying on one engine,
//! from an airport it could land at.
//!
//! A diversion airport is chosen for its position alone here; whether it is actually fit
//! to land at when the flight passes it — runway, weather, NOTAMs — is not this module's
//! business. The caller filters the list it hands in to whichever airports it currently
//! trusts, and can hand in a different list for every flight, or even every leg.
//!
//! The list of the world's airports can run to tens of thousands, and `EdgeRule::check`
//! is called for every edge the route search so much as looks at, so a check must never
//! scan it. A `Grid` is built once, when the rule (or the segment list) is made: airports
//! bucketed into cells of a few degrees on a side, so a check only ever looks at the
//! handful of cells within reach of the point being tested.

use crate::dispatch::{distance_nm, Airport, EdgeQuery, EdgeRule, FiledRoute, LatLon, Verdict};
use std::collections::HashMap;

/// How wide a grid cell is, in degrees. Coarse enough that most ETOPS rings (a couple of
/// hundred nautical miles) touch only a handful of cells; fine enough that a cell rarely
/// holds more than a few dozen airports even where they are dense.
const CELL_DEG: f64 = 4.0;

/// How far apart the points are sampled along a segment. An engine failure can happen
/// anywhere along a leg, so the leg is only as good as its worst point; twenty-five miles
/// is close enough that the true worst point is never more than a couple of miles from a
/// sample, which at the speeds and distances ETOPS deals in is not worth resolving further.
const SAMPLE_NM: f64 = 25.0;

/// Airports bucketed by a coarse latitude/longitude grid, so that "is anything within a
/// radius of this point" is a lookup over a few cells rather than a scan of the list.
struct Grid {
    airports: Vec<Airport>,
    cells: HashMap<(i32, i32), Vec<usize>>,
}

fn bucket(p: LatLon) -> (i32, i32) {
    ((p.0 / CELL_DEG).floor() as i32, (p.1 / CELL_DEG).floor() as i32)
}

impl Grid {
    fn build(airports: Vec<Airport>) -> Grid {
        let mut cells: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (i, a) in airports.iter().enumerate() {
            cells.entry(bucket(a.pos)).or_default().push(i);
        }
        Grid { airports, cells }
    }

    /// Whether any airport lies within `radius_nm` of `p`. Looks only at the cells the
    /// radius can reach from `p`'s own cell, not the whole list.
    fn covers(&self, p: LatLon, radius_nm: f64) -> bool {
        self.nearest_within(p, radius_nm).is_some()
    }

    /// The airport nearest `p` within a radius, if one lies inside it. `None` allocates
    /// nothing; a hit walks the handful of candidate airports in range without collecting
    /// them into a list first.
    fn nearest_within(&self, p: LatLon, radius_nm: f64) -> Option<(usize, f64)> {
        let cos = p.0.to_radians().cos().max(0.05);
        let dlat = (radius_nm / 60.0 / CELL_DEG).ceil() as i32 + 1;
        let dlon = (radius_nm / 60.0 / cos / CELL_DEG).ceil() as i32 + 1;
        let (clat, clon) = bucket(p);
        let mut best: Option<(usize, f64)> = None;
        for dy in -dlat..=dlat {
            for dx in -dlon..=dlon {
                let Some(list) = self.cells.get(&(clat + dy, clon + dx)) else { continue };
                for &i in list {
                    let d = distance_nm(p, self.airports[i].pos);
                    if d <= radius_nm && best.is_none_or(|(_, b)| d < b) {
                        best = Some((i, d));
                    }
                }
            }
        }
        best
    }

    /// Every airport within a radius of a point, nearest first. Used only where the
    /// answer is actually printed (`etops_segments`), never on the `EdgeRule::check` path.
    fn within(&self, p: LatLon, radius_nm: f64) -> Vec<(&Airport, f64)> {
        let cos = p.0.to_radians().cos().max(0.05);
        let dlat = (radius_nm / 60.0 / CELL_DEG).ceil() as i32 + 1;
        let dlon = (radius_nm / 60.0 / cos / CELL_DEG).ceil() as i32 + 1;
        let (clat, clon) = bucket(p);
        let mut out = Vec::new();
        for dy in -dlat..=dlat {
            for dx in -dlon..=dlon {
                let Some(list) = self.cells.get(&(clat + dy, clon + dx)) else { continue };
                for &i in list {
                    let d = distance_nm(p, self.airports[i].pos);
                    if d <= radius_nm {
                        out.push((&self.airports[i], d));
                    }
                }
            }
        }
        out.sort_by(|a, b| a.1.total_cmp(&b.1));
        out
    }
}

/// Points along a path (origin, every intermediate point, destination) at a fixed spacing,
/// each with how far it is from the start of the path.
fn sample(path: &[LatLon], step_nm: f64) -> Vec<(f64, LatLon)> {
    let mut out = Vec::new();
    let mut along_so_far = 0.0;
    if let Some(&first) = path.first() {
        out.push((0.0, first));
    }
    for w in path.windows(2) {
        let (a, b) = (w[0], w[1]);
        let leg = distance_nm(a, b);
        if leg < 0.01 {
            continue;
        }
        let steps = (leg / step_nm).ceil().max(1.0) as usize;
        for i in 1..=steps {
            let f = i as f64 / steps as f64;
            out.push((along_so_far + leg * f, crate::dispatch::along(a, b, f)));
        }
        along_so_far += leg;
    }
    out
}

/// A rule the route search checks every edge against: never more than `minutes`, flying at
/// `one_engine_tas_kt`, from every one of a list of airports.
pub struct Etops {
    minutes: u32,
    one_engine_tas_kt: f64,
    grid: Grid,
}

impl Etops {
    pub fn new(minutes: u32, one_engine_tas_kt: f64, airports: Vec<Airport>) -> Etops {
        Etops { minutes, one_engine_tas_kt, grid: Grid::build(airports) }
    }

    fn threshold_nm(&self) -> f64 {
        self.minutes as f64 * self.one_engine_tas_kt / 60.0
    }
}

impl EdgeRule for Etops {
    fn name(&self) -> &str {
        "ETOPS"
    }

    fn check(&self, q: &EdgeQuery) -> Verdict {
        let threshold = self.threshold_nm();
        let nm = distance_nm(q.from_pos, q.to_pos);
        if nm < 0.01 {
            return Verdict::Allow;
        }
        let steps = (nm / SAMPLE_NM).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let f = i as f64 / steps as f64;
            let p = crate::dispatch::along(q.from_pos, q.to_pos, f);
            if !self.grid.covers(p, threshold) {
                return Verdict::Forbid(format!("ETOPS {} min at {:.0} kt: more than {:.0} nm from a diversion airport on {}", self.minutes, self.one_engine_tas_kt, threshold, q.airway));
            }
        }
        Verdict::Allow
    }
}

/// Whether any point of the route — origin, the points between, and the destination — is
/// more than sixty minutes at the one-engine-out speed from every airport on the list. The
/// sixty minutes is the ordinary rule for a twin with no ETOPS approval at all: past it, the
/// flight needs one.
pub fn etops_required(origin: LatLon, destination: LatLon, points: &[LatLon], airports: &[Airport], one_engine_tas_kt: f64) -> bool {
    let grid = Grid::build(airports.to_vec());
    let threshold = one_engine_tas_kt; // Sixty minutes at tas_kt is, in nautical miles, tas_kt itself.
    let mut path = Vec::with_capacity(points.len() + 2);
    path.push(origin);
    path.extend_from_slice(points);
    path.push(destination);
    sample(&path, SAMPLE_NM).iter().any(|(_, p)| !grid.covers(*p, threshold))
}

/// A stretch of the route further than sixty minutes from every airport: where flying it
/// depends on the ETOPS approval, and which airports along it are within the approved time.
#[derive(Debug, Clone, PartialEq)]
pub struct EtopsSegment {
    pub entry_nm: f64,
    pub exit_nm: f64,
    pub entry: LatLon,
    pub exit: LatLon,
    /// The airports within the approved time of some part of this stretch, nearest first
    /// by how much of the stretch each one alone would cover.
    pub airports: Vec<String>,
}

/// Every stretch of a route beyond the sixty-minute ring, and which of the approved
/// airports covers each one, for the flight plan to print alongside the equal-time points.
pub fn etops_segments(route: &FiledRoute, airports: &[Airport], minutes: u32, one_engine_tas_kt: f64) -> Vec<EtopsSegment> {
    let grid = Grid::build(airports.to_vec());
    let base_nm = one_engine_tas_kt;
    let approved_nm = minutes as f64 * one_engine_tas_kt / 60.0;
    let path: Vec<LatLon> = route.points.iter().map(|w| w.pos).collect();
    if path.len() < 2 {
        return Vec::new();
    }
    let samples = sample(&path, SAMPLE_NM);
    let outside = |p: LatLon| !grid.covers(p, base_nm);

    // Refine a transition between a covered and an uncovered sample by bisection, so the
    // entry and exit are placed on the ring itself rather than a quarter of a sample short.
    let refine = |(lo_nm, lo_pos): (f64, LatLon), (hi_nm, hi_pos): (f64, LatLon), want_outside: bool| -> (f64, LatLon) {
        let (mut lo, mut hi) = (lo_nm, hi_nm);
        let (mut lo_p, mut hi_p) = (lo_pos, hi_pos);
        for _ in 0..20 {
            let mid_nm = (lo + hi) / 2.0;
            let mid_p = crate::dispatch::along(lo_p, hi_p, 0.5);
            if outside(mid_p) == want_outside {
                hi = mid_nm;
                hi_p = mid_p;
            } else {
                lo = mid_nm;
                lo_p = mid_p;
            }
        }
        (hi, hi_p)
    };

    let mut out = Vec::new();
    let mut open: Option<(f64, LatLon)> = None;
    for w in samples.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (a_out, b_out) = (outside(a.1), outside(b.1));
        if a_out && open.is_none() {
            open = Some(a);
        }
        if open.is_some() && !b_out {
            let (start_nm, start_pos) = open.take().unwrap();
            let (entry_nm, entry) = if a_out { (start_nm, start_pos) } else { refine(a, b, true) };
            let (exit_nm, exit) = refine(a, b, false);
            out.push(EtopsSegment { entry_nm, exit_nm, entry, exit, airports: covering(&grid, entry, exit, approved_nm) });
        }
    }
    if let Some((start_nm, start_pos)) = open {
        let last = *samples.last().unwrap();
        out.push(EtopsSegment { entry_nm: start_nm, exit_nm: last.0, entry: start_pos, exit: last.1, airports: covering(&grid, start_pos, last.1, approved_nm) });
    }
    out
}

/// The idents of every airport within the approved distance of either end of a stretch, or
/// its middle: enough to say what covers it without sampling the whole of it again.
fn covering(grid: &Grid, entry: LatLon, exit: LatLon, radius_nm: f64) -> Vec<String> {
    let mid = crate::dispatch::along(entry, exit, 0.5);
    let mut idents: Vec<String> = Vec::new();
    for p in [entry, mid, exit] {
        for (a, _) in grid.within(p, radius_nm) {
            if !idents.contains(&a.icao) {
                idents.push(a.icao.clone());
            }
        }
    }
    idents
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn ap(icao: &str, lat: f64, lon: f64) -> Airport {
        Airport { icao: icao.to_string(), name: String::new(), pos: (lat, lon), elevation_ft: 0.0 }
    }

    /// A chain of airports a thousand miles apart along the equator, at whole-degree
    /// spacing, so the distance from any point on the line to the nearest one can be
    /// checked by hand: roughly 60 nautical miles per degree of longitude at the equator.
    fn chain() -> Vec<Airport> {
        (0..=20).map(|i| ap(&format!("A{i:02}"), 0.0, i as f64 * 10.0)).collect()
    }

    fn query<'a>(from: &'a str, from_pos: LatLon, to: &'a str, to_pos: LatLon) -> EdgeQuery<'a> {
        EdgeQuery { from, from_pos, to, to_pos, airway: "DCT", level_ft: 35000.0, when: Utc::now(), origin: from, destination: to }
    }

    #[test]
    fn a_short_hop_between_covered_airports_is_allowed() {
        let e = Etops::new(120, 300.0, chain());
        // 120 minutes at 300 kt is 600 nm, about ten degrees of longitude at the equator:
        // comfortably more than the ten-degree gap between the chain's own airports.
        let q = query("A00", (0.0, 0.0), "A01", (0.0, 10.0));
        assert_eq!(e.check(&q), Verdict::Allow);
    }

    #[test]
    fn a_leg_that_strays_too_far_is_forbidden() {
        // Sixty minutes at 300 kt is 300 nm, five degrees of longitude: a leg that swings
        // ten degrees off the chain, to latitude 9, is about 540 nm from the nearest one.
        let e = Etops::new(60, 300.0, chain());
        let q = query("A00", (0.0, 0.0), "FAR", (9.0, 5.0));
        assert!(matches!(e.check(&q), Verdict::Forbid(_)));
    }

    #[test]
    fn etops_required_answers_by_the_sixty_minute_ring() {
        let airports = chain();
        // Origin and destination both on the line, by way of a point far off it: needs the
        // approval to fly the great circle a point off the chain, not to fly along it.
        let close = etops_required((0.0, 0.0), (0.0, 20.0), &[], &airports, 300.0);
        assert!(!close, "along the chain should need no approval");
        let far = etops_required((0.0, 0.0), (0.0, 20.0), &[(9.0, 10.0)], &airports, 300.0);
        assert!(far, "a detour ten degrees off the chain should need it");
    }

    #[test]
    fn etops_segments_bracket_the_stretch_that_needs_the_approval() {
        let airports = chain();
        let route = FiledRoute {
            origin: ap("A00", 0.0, 0.0),
            destination: ap("A02", 0.0, 20.0),
            dep_runway: None,
            sid: None,
            sid_transition: None,
            star: None,
            star_transition: None,
            arr_runway: None,
            approach: None,
            points: vec![
                crate::dispatch::Waypoint::new("A00", (0.0, 0.0), "DCT", crate::dispatch::PointKind::Airport),
                // A short detour eight degrees north of the middle of the chain, well past
                // the sixty-minute ring, and back down.
                crate::dispatch::Waypoint::new("MID", (8.0, 10.0), "DCT", crate::dispatch::PointKind::Enroute),
                crate::dispatch::Waypoint::new("A02", (0.0, 20.0), "", crate::dispatch::PointKind::Airport),
            ],
            cruise_ft: 35000.0,
            off_block: Utc::now(),
        };
        let segs = etops_segments(&route, &airports, 90, 300.0);
        assert_eq!(segs.len(), 1, "{segs:?}");
        let s = &segs[0];
        assert!(s.entry_nm > 0.0 && s.exit_nm > s.entry_nm && s.exit_nm < route.distance_nm());
        assert!(!s.airports.is_empty());
    }

    /// Not a correctness test: prints how long a million calls to `check` take, run by
    /// hand with `cargo test --release -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn check_is_fast() {
        let mut airports = chain();
        for i in 0..20000 {
            airports.push(ap("W", 40.0 + (i % 50) as f64 * 0.4, (i % 360) as f64 - 180.0));
        }
        let e = Etops::new(120, 300.0, airports);
        let n = 1_000_000;

        // A six-hundred-mile oceanic leg: the worst case, twenty-four samples each
        // scanning every cell within a six-hundred-mile ETOPS ring.
        let long = query("A00", (0.0, 0.0), "A01", (0.0, 10.0));
        let start = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(e.check(&long));
        }
        println!("Etops::check, a 600 nm leg: {:?} per call over {n} calls, 20,020 airports indexed", start.elapsed() / n);

        // A forty-mile domestic airway leg: the ordinary case the search meets millions
        // of times over in one route.
        let short = query("A00", (0.0, 0.0), "A00B", (0.0, 0.67));
        let start = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(e.check(&short));
        }
        println!("Etops::check, a 40 nm leg: {:?} per call over {n} calls, 20,020 airports indexed", start.elapsed() / n);
    }

    #[test]
    fn the_grid_finds_a_near_airport_without_scanning_every_one() {
        let mut many = chain();
        // A great many airports nowhere near the query point, to stand in for "every
        // airport in the world": the grid must not have to look at these to answer.
        for i in 0..5000 {
            many.push(ap("FAR", 60.0 + (i % 10) as f64, (i % 360) as f64));
        }
        let grid = Grid::build(many);
        assert!(grid.covers((0.0, 5.0), 400.0));
        assert!(!grid.covers((30.0, -100.0), 50.0));
    }
}
