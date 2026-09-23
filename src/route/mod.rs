//! A route from one airport to another over the airway network, planned by greedy
//! best-first search on time rather than distance, through the weather.
//!
//! The planner takes three things and nothing else:
//!
//! * the network: every airway segment, with its limits (`Graph`);
//! * the aircraft: how fast it flies, where it cruises, how high it can go (`Aircraft`);
//! * the conditions: the winds aloft and the hazards (`Conditions`), in plain shapes any
//!   source can fill: a winds-aloft forecast, SIGMETs and convective outlooks, restricted
//!   areas that are active. Nothing here fetches them.
//!
//! It works in four stages.
//!
//! 1. **Filter.** A segment the aircraft may not fly is taken out before the search sees
//!    it: one whose minimum altitude is above the cruise level or the aircraft's ceiling,
//!    whose maximum is below the cruise level, which is one-way the other way, or which
//!    passes through a hazard that is to be avoided at the cruise level. A segment through
//!    a hazard that is only unpleasant (turbulence, icing) stays, and costs more.
//! 2. **Cost.** Every segment is costed in minutes, not miles: the ground speed along it
//!    from the true airspeed and the wind at its middle, worked out with the wind
//!    triangle, times any hazard it crosses. A headwind makes a segment longer.
//! 3. **Search.** Greedy best-first: the fix that looks nearest the destination by
//!    estimated time remaining is always the one expanded next. The estimate is the direct
//!    distance at the ground speed the wind gives on the direct bearing, plus an allowance
//!    for any hazard lying across the direct line. Greedy search is fast because it never
//!    looks back, and wrong for the same reason, so it is kept honest two ways: the open
//!    list is cut to a beam of the most promising fixes, with ties going to the one
//!    reached soonest; and a found route is refined afterwards.
//! 4. **Refine.** Where the route zig-zags, a direct leg is put in place of the part it
//!    cuts off, if it is short enough, clear of every hazard to be avoided, and quicker.
//!
//! The route comes out as the legs flown and as the string a flight plan files, the
//! airways collapsed: `LPPT DCT ESP UN873 FUN DCT LPMA`.

use serde::Deserialize;

pub use crate::dispatch::{bearing_deg, distance_nm, sky, Hazard, HazardKind, LatLon, Wind};

pub mod airports;
pub mod airspace;
pub mod etops;
pub mod oceanic;
pub mod rad;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

// ---------------------------------------------------------------------------------
// Inputs.
// ---------------------------------------------------------------------------------

/// What the air is doing and where may not be flown: the winds, the hazards, the flight
/// information regions to keep out of, and what to do about restricted areas. Empty is
/// still air, a clear sky and everywhere open.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Conditions {
    #[serde(default)]
    pub winds: Vec<Wind>,
    /// Weather, conflict zones, anything drawn as a shape: SIGMETs, convective outlooks,
    /// the areas a conflict-zone bulletin names.
    #[serde(default)]
    pub hazards: Vec<Hazard>,
    /// Flight information regions not to enter at all, by code: a country's airspace that
    /// is closed, or one a conflict-zone bulletin says to keep out of.
    #[serde(default)]
    pub avoid_firs: Vec<String>,
    #[serde(default)]
    pub restricted: RestrictedPolicy,
}

/// What to do about the special-use airspace in the navigation database.
#[derive(Debug, Clone, Deserialize)]
pub struct RestrictedPolicy {
    /// Prohibited areas (P): kept out of.
    #[serde(default = "yes")]
    pub avoid_prohibited: bool,
    /// Restricted areas (R): kept out of, since whether one is active is a NOTAM this
    /// planner does not see.
    #[serde(default = "yes")]
    pub avoid_restricted: bool,
    /// Danger, warning, military and training areas: flown through, at this price.
    #[serde(default = "danger_price")]
    pub danger_factor: f64,
}

fn yes() -> bool {
    true
}

fn danger_price() -> f64 {
    1.15
}

impl Default for RestrictedPolicy {
    fn default() -> Self {
        RestrictedPolicy { avoid_prohibited: true, avoid_restricted: true, danger_factor: danger_price() }
    }
}

/// What the route is planned for.
#[derive(Debug, Clone, Copy)]
pub struct Aircraft {
    pub tas_kt: f64,
    pub cruise_ft: f64,
    pub ceiling_ft: f64,
}

/// How the search is run.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// How many fixes the open list keeps: the width of the beam.
    pub beam: usize,
    /// The longest direct leg: onto the airways from the departure, off them to the
    /// destination, and any the refinement puts in.
    pub max_direct_nm: f64,
    /// How many fixes each end of the route may join the airways at.
    pub entry_fixes: usize,
    /// Whether the refinement may replace airway legs with direct ones.
    pub free_route: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { beam: 256, max_direct_nm: 200.0, entry_fixes: 8, free_route: true }
    }
}

// ---------------------------------------------------------------------------------
// The network.
// ---------------------------------------------------------------------------------

/// A named point the network runs through.
#[derive(Debug, Clone)]
pub struct Fix {
    pub id: String,
    pub pos: LatLon,
}

/// A segment out of a fix.
#[derive(Debug, Clone)]
pub struct Edge {
    pub to: usize,
    pub airway: String,
    pub min_ft: Option<f64>,
    pub max_ft: Option<f64>,
}

/// The airway network: fixes, and the segments out of each.
#[derive(Debug, Default)]
pub struct Graph {
    pub fixes: Vec<Fix>,
    pub edges: Vec<Vec<Edge>>,
    /// A fix by its name and where it is, since names repeat round the world.
    index: HashMap<String, usize>,
}

impl Graph {
    fn key(id: &str, pos: LatLon) -> String {
        format!("{id}@{:.3},{:.3}", pos.0, pos.1)
    }

    fn fix(&mut self, id: &str, pos: LatLon) -> usize {
        let key = Graph::key(id, pos);
        if let Some(i) = self.index.get(&key) {
            return *i;
        }
        self.fixes.push(Fix { id: id.to_string(), pos });
        self.edges.push(Vec::new());
        let i = self.fixes.len() - 1;
        self.index.insert(key, i);
        i
    }

    /// Add a segment, and its reverse unless it is one-way.
    #[allow(clippy::too_many_arguments)]
    pub fn add(&mut self, airway: &str, from: (&str, LatLon), to: (&str, LatLon), one_way: bool, min_ft: Option<f64>, max_ft: Option<f64>) {
        let a = self.fix(from.0, from.1);
        let b = self.fix(to.0, to.1);
        if a == b {
            return;
        }
        self.edges[a].push(Edge { to: b, airway: airway.to_string(), min_ft, max_ft });
        if !one_way {
            self.edges[b].push(Edge { to: a, airway: airway.to_string(), min_ft, max_ft });
        }
    }

    /// The network in the navigation database on this machine.
    pub fn from_navdata() -> Graph {
        let mut g = Graph::default();
        for s in crate::sources::navdata::airway_segments() {
            g.add(&s.airway, (&s.from.0, (s.from.1, s.from.2)), (&s.to.0, (s.to.1, s.to.2)), s.one_way, s.min_ft, s.max_ft);
        }
        g
    }

    /// The fixes nearest a point, nearest first, up to `n` of them within `within_nm`,
    /// that have a segment out of them.
    fn nearest(&self, at: LatLon, n: usize, within_nm: f64) -> Vec<(usize, f64)> {
        let mut found: Vec<(usize, f64)> = self
            .fixes
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.edges[*i].is_empty())
            .map(|(i, f)| (i, distance_nm(at, f.pos)))
            .filter(|(_, d)| *d <= within_nm)
            .collect();
        found.sort_by(|a, b| a.1.total_cmp(&b.1));
        found.truncate(n);
        found
    }
}

// ---------------------------------------------------------------------------------
// The sphere, the wind and the shapes.
// ---------------------------------------------------------------------------------

fn midpoint(a: LatLon, b: LatLon) -> LatLon {
    // Close enough at the lengths a segment is; a direct leg across the date line is
    // not one this planner draws.
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

impl Conditions {
    /// The wind at a place and an altitude, from the samples near it: those in the same
    /// band of altitude, weighted by how near they are. Still air where there are none.
    /// Returned as the components it blows *towards*, north and east, knots.
    fn wind_at(&self, at: LatLon, alt_ft: f64) -> (f64, f64) {
        let band: Vec<&Wind> = {
            let near: Vec<&Wind> = self.winds.iter().filter(|w| (w.alt_ft - alt_ft).abs() <= 4000.0).collect();
            if near.is_empty() {
                // The nearest level there is, rather than none.
                let Some(best) = self.winds.iter().map(|w| (w.alt_ft - alt_ft).abs()).min_by(|a, b| a.total_cmp(b)) else { return (0.0, 0.0) };
                self.winds.iter().filter(|w| ((w.alt_ft - alt_ft).abs() - best).abs() < 1.0).collect()
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
            (0.0, 0.0)
        } else {
            (n / total, e / total)
        }
    }

    /// The ground speed along a track, from the airspeed and the wind: the wind triangle.
    fn ground_speed(&self, at: LatLon, track_deg: f64, ac: &Aircraft) -> f64 {
        let (wn, we) = self.wind_at(at, ac.cruise_ft);
        let t = track_deg.to_radians();
        let along = wn * t.cos() + we * t.sin();
        let across = -wn * t.sin() + we * t.cos();
        // The heading has to point into the crosswind; what is left of the airspeed along
        // the track, plus the wind along it, is the ground speed.
        let ratio = (across / ac.tas_kt).clamp(-0.95, 0.95);
        (ac.tas_kt * (1.0 - ratio * ratio).sqrt() + along).max(60.0)
    }

    /// The hazards that fill the cruise level.
    fn at_level(&self, alt_ft: f64) -> impl Iterator<Item = &Hazard> {
        self.hazards.iter().filter(move |h| h.base_ft <= alt_ft && alt_ft <= h.top_ft && h.polygon.len() >= 3)
    }
}

/// Whether a straight line between two places crosses or enters a shape. Worked on a flat
/// projection about the line's middle, which at the size of a weather area is well within
/// the width of the area's own edge.
fn crosses(a: LatLon, b: LatLon, polygon: &[LatLon]) -> bool {
    let mid = midpoint(a, b);
    let cos = mid.0.to_radians().cos().max(0.05);
    let flat = |p: LatLon| ((p.1 - mid.1) * cos, p.0 - mid.0);
    let (p, q) = (flat(a), flat(b));
    let shape: Vec<(f64, f64)> = polygon.iter().map(|&v| flat(v)).collect();
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

/// A hazard's outline ready for testing: the points, the box round them, and how far
/// across it is. The box is what keeps thousands of shapes cheap: a line nowhere near a
/// shape's box cannot cross the shape.
struct Shape {
    polygon: Vec<LatLon>,
    south: f64,
    north: f64,
    west: f64,
    east: f64,
    across_nm: f64,
}

impl Shape {
    fn new(polygon: &[LatLon]) -> Shape {
        let (mut s, mut n, mut w, mut e) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for (lat, lon) in polygon {
            s = s.min(*lat);
            n = n.max(*lat);
            w = w.min(*lon);
            e = e.max(*lon);
        }
        Shape { polygon: polygon.to_vec(), south: s, north: n, west: w, east: e, across_nm: distance_nm((s, w), (n, e)) }
    }

    fn crossed_by(&self, a: LatLon, b: LatLon) -> bool {
        if a.0.max(b.0) < self.south || a.0.min(b.0) > self.north || a.1.max(b.1) < self.west || a.1.min(b.1) > self.east {
            return false;
        }
        crosses(a, b, &self.polygon)
    }
}


// ---------------------------------------------------------------------------------
// The planner.
// ---------------------------------------------------------------------------------

/// A leg of the route as flown.
#[derive(Debug, Clone)]
pub struct Leg {
    pub from: String,
    pub to: String,
    /// The airway, or "DCT" for a direct leg.
    pub via: String,
    pub nm: f64,
    pub ground_speed_kt: f64,
    pub minutes: f64,
    pub from_pos: LatLon,
    pub to_pos: LatLon,
}

/// A planned route.
#[derive(Debug, Clone)]
pub struct Plan {
    pub legs: Vec<Leg>,
    pub nm: f64,
    pub minutes: f64,
    /// How many fixes the search expanded, which says how hard it had to look.
    pub expanded: usize,
    /// How many segments the filter took out before the search began.
    pub filtered_out: usize,
}

impl Plan {
    /// The route as a flight plan files it, consecutive legs on one airway collapsed.
    pub fn route_string(&self, origin: &str, destination: &str) -> String {
        let mut out = vec![origin.to_string()];
        let mut i = 0;
        while i < self.legs.len() {
            let via = &self.legs[i].via;
            let mut j = i;
            while j + 1 < self.legs.len() && &self.legs[j + 1].via == via && via != "DCT" {
                j += 1;
            }
            out.push(via.clone());
            if j + 1 < self.legs.len() {
                out.push(self.legs[j].to.clone());
            }
            i = j + 1;
        }
        out.push(destination.to_string());
        out.join(" ")
    }
}

/// A node of the search: a fix of the network, or one of the two airports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    Origin,
    Fix(usize),
    Destination,
}

/// An entry on the open list, ordered so the heap gives the lowest estimate first, and of
/// two equal estimates the one reached soonest.
#[derive(Debug, Clone, Copy)]
struct Open {
    estimate: f64,
    so_far: f64,
    node: Node,
}

impl PartialEq for Open {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Open {}
impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        other.estimate.total_cmp(&self.estimate).then_with(|| other.so_far.total_cmp(&self.so_far))
    }
}

/// The planner, holding everything a search reads.
struct Planner<'a> {
    graph: &'a Graph,
    ac: Aircraft,
    wx: &'a Conditions,
    opts: Options,
    origin: LatLon,
    destination: LatLon,
    /// The fixes the route may leave the airways at, and the direct leg's time from each.
    exits: HashMap<usize, f64>,
    /// The hazards to be kept out of at the cruise level.
    avoid: Vec<Shape>,
    /// The hazards flown through at a price, and the price.
    penalise: Vec<(Shape, f64)>,
    /// How many segments the filter has turned away, counted as the search meets them.
    turned_away: std::cell::Cell<usize>,
}

impl Planner<'_> {
    fn pos(&self, n: Node) -> LatLon {
        match n {
            Node::Origin => self.origin,
            Node::Destination => self.destination,
            Node::Fix(i) => self.graph.fixes[i].pos,
        }
    }

    /// Whether a straight line may be flown at the cruise level.
    fn clear(&self, a: LatLon, b: LatLon) -> bool {
        !self.avoid.iter().any(|s| s.crossed_by(a, b))
    }

    /// The time to fly a straight line, minutes, and the ground speed along it: from the
    /// wind at its middle, times any hazard it goes through that is only to be paid for.
    fn time(&self, a: LatLon, b: LatLon) -> (f64, f64) {
        let nm = distance_nm(a, b);
        if nm < 0.01 {
            return (0.0, self.ac.tas_kt);
        }
        let gs = self.wx.ground_speed(midpoint(a, b), bearing_deg(a, b), &self.ac);
        let mut minutes = nm / gs * 60.0;
        for (shape, factor) in &self.penalise {
            if shape.crossed_by(a, b) {
                minutes *= factor.max(1.0);
            }
        }
        (minutes, gs)
    }

    /// Whether a segment of the network may be flown: stage one, the filter.
    fn usable(&self, from: usize, e: &Edge) -> bool {
        let level = self.ac.cruise_ft;
        if e.min_ft.is_some_and(|m| m > level || m > self.ac.ceiling_ft) {
            return false;
        }
        if e.max_ft.is_some_and(|m| m < level) {
            return false;
        }
        self.clear(self.graph.fixes[from].pos, self.graph.fixes[e.to].pos)
    }

    /// The estimate of time remaining from a place: the direct distance at the ground
    /// speed the wind gives on the direct bearing, and, where a hazard to be avoided lies
    /// across the direct line, the time to go round the widest of them.
    fn estimate(&self, at: LatLon) -> f64 {
        let nm = distance_nm(at, self.destination);
        if nm < 0.01 {
            return 0.0;
        }
        let gs = self.wx.ground_speed(at, bearing_deg(at, self.destination), &self.ac);
        let detour = self.avoid.iter().filter(|s| s.crossed_by(at, self.destination)).map(|s| s.across_nm * 0.6).fold(0.0, f64::max);
        (nm + detour) / gs * 60.0
    }

    /// The ways on from a node, with the time each takes and what it is flown on.
    fn successors(&self, n: Node) -> Vec<(Node, f64, String)> {
        let mut out = Vec::new();
        match n {
            Node::Origin => {
                for (i, _) in self.graph.nearest(self.origin, self.opts.entry_fixes, self.opts.max_direct_nm) {
                    let to = self.graph.fixes[i].pos;
                    if self.clear(self.origin, to) {
                        out.push((Node::Fix(i), self.time(self.origin, to).0, "DCT".to_string()));
                    }
                }
                // Near enough to go direct the whole way.
                if distance_nm(self.origin, self.destination) <= self.opts.max_direct_nm && self.clear(self.origin, self.destination) {
                    out.push((Node::Destination, self.time(self.origin, self.destination).0, "DCT".to_string()));
                }
            }
            Node::Fix(i) => {
                let here = self.graph.fixes[i].pos;
                for e in &self.graph.edges[i] {
                    if self.usable(i, e) {
                        out.push((Node::Fix(e.to), self.time(here, self.graph.fixes[e.to].pos).0, e.airway.clone()));
                    } else {
                        self.turned_away.set(self.turned_away.get() + 1);
                    }
                }
                if let Some(t) = self.exits.get(&i) {
                    out.push((Node::Destination, *t, "DCT".to_string()));
                }
            }
            Node::Destination => {}
        }
        out
    }

    /// Stage three: greedy best-first, in a beam.
    fn search(&self) -> Option<(Vec<(Node, String)>, usize)> {
        let mut open = BinaryHeap::new();
        let mut came_from: HashMap<Node, (Node, String)> = HashMap::new();
        let mut so_far: HashMap<Node, f64> = HashMap::new();
        let mut closed: HashSet<Node> = HashSet::new();
        so_far.insert(Node::Origin, 0.0);
        open.push(Open { estimate: self.estimate(self.origin), so_far: 0.0, node: Node::Origin });
        let mut expanded = 0;
        while let Some(Open { node, .. }) = open.pop() {
            if !closed.insert(node) {
                continue;
            }
            if node == Node::Destination {
                // Each node carries what the leg out of it is flown on.
                let mut path = vec![(Node::Destination, String::new())];
                let mut at = Node::Destination;
                while let Some((prev, via)) = came_from.get(&at) {
                    path.push((*prev, via.clone()));
                    at = *prev;
                }
                path.reverse();
                return Some((path, expanded));
            }
            expanded += 1;
            let base = so_far[&node];
            for (next, minutes, via) in self.successors(node) {
                if closed.contains(&next) {
                    continue;
                }
                let t = base + minutes;
                // Greedy in what it expands; but a node reached again more quickly takes
                // the quicker way, so the route that comes out is the better of the two.
                if so_far.get(&next).is_some_and(|known| *known <= t) {
                    continue;
                }
                so_far.insert(next, t);
                came_from.insert(next, (node, via));
                let estimate = if next == Node::Destination { 0.0 } else { self.estimate(self.pos(next)) };
                open.push(Open { estimate, so_far: t, node: next });
            }
            // The beam: when the open list grows past twice its width, keep its best.
            if open.len() > self.opts.beam * 2 {
                let mut keep: Vec<Open> = open.into_vec();
                keep.sort_by(|a, b| a.estimate.total_cmp(&b.estimate).then(a.so_far.total_cmp(&b.so_far)));
                keep.truncate(self.opts.beam);
                open = keep.into_iter().collect();
            }
        }
        None
    }

    /// Stage four: straighten the route where a direct leg is short, clear and quicker.
    fn refine(&self, path: Vec<(Node, String)>) -> Vec<(Node, String)> {
        if !self.opts.free_route || path.len() < 3 {
            return path;
        }
        let mut out: Vec<(Node, String)> = vec![path[0].clone()];
        let mut i = 0;
        while i < path.len() - 1 {
            // The furthest point ahead that a direct leg reaches and wins on.
            let from = self.pos(path[i].0);
            let mut best = i + 1;
            for j in (i + 2..path.len()).rev() {
                let to = self.pos(path[j].0);
                if distance_nm(from, to) > self.opts.max_direct_nm || !self.clear(from, to) {
                    continue;
                }
                let along: f64 = (i..j).map(|k| self.time(self.pos(path[k].0), self.pos(path[k + 1].0)).0).sum();
                if self.time(from, to).0 + 0.01 < along {
                    best = j;
                    break;
                }
            }
            if best == i + 1 {
                out.last_mut().unwrap().1 = path[i].1.clone();
                out.push(path[i + 1].clone());
            } else {
                out.last_mut().unwrap().1 = "DCT".to_string();
                out.push(path[best].clone());
            }
            i = best;
        }
        out
    }
}

/// The airspace in the navigation database that bears on a flight between two places,
/// added to the conditions as hazards: every flight information region the conditions
/// say to keep out of, and the special-use areas along the way, avoided or priced as the
/// policy says. `corridor_nm` is how far either side of the direct line to look.
pub fn with_airspace(mut wx: Conditions, a: LatLon, b: LatLon, corridor_nm: f64) -> Conditions {
    for region in crate::sources::navdata::regions(&wx.avoid_firs) {
        for part in region.parts {
            wx.hazards.push(Hazard { name: format!("{} FIR {}", region.ident, region.name), polygon: part, base_ft: 0.0, top_ft: sky(), kind: HazardKind::Avoid, active_from: None, active_to: None, source: "navigation database".to_string() });
        }
    }
    let margin = corridor_nm / 60.0;
    let cos = ((a.0 + b.0) / 2.0).to_radians().cos().max(0.2);
    let (south, north) = (a.0.min(b.0) - margin, a.0.max(b.0) + margin);
    let (west, east) = (a.1.min(b.1) - margin / cos, a.1.max(b.1) + margin / cos);
    let policy = wx.restricted.clone();
    for area in crate::sources::navdata::restricted_areas(south, north, west, east) {
        let kind = match area.kind {
            'P' if policy.avoid_prohibited => HazardKind::Avoid,
            'R' if policy.avoid_restricted => HazardKind::Avoid,
            'P' | 'R' => continue,
            'D' | 'W' | 'M' | 'T' | 'A' => HazardKind::Penalise(policy.danger_factor),
            _ => continue,
        };
        wx.hazards.push(Hazard { name: format!("{} {}", area.designation, area.name), polygon: area.boundary, base_ft: area.lower_ft, top_ft: area.upper_ft, kind, active_from: None, active_to: None, source: "navigation database".to_string() });
    }
    wx
}

/// Plan a route between two airports.
///
/// `None` when the network, as filtered, does not connect them: every way through is
/// closed by a hazard, or above the aircraft, or there is no airway near one end.
pub fn plan(graph: &Graph, origin: (&str, LatLon), destination: (&str, LatLon), ac: Aircraft, wx: &Conditions, opts: Options) -> Option<Plan> {
    let avoid: Vec<Shape> = wx.at_level(ac.cruise_ft).filter(|h| h.kind == HazardKind::Avoid).map(|h| Shape::new(&h.polygon)).collect();
    let penalise: Vec<(Shape, f64)> = wx
        .at_level(ac.cruise_ft)
        .filter_map(|h| match h.kind {
            HazardKind::Penalise(f) => Some((Shape::new(&h.polygon), f)),
            HazardKind::Avoid => None,
        })
        .collect();
    let mut planner = Planner { graph, ac, wx, opts, origin: origin.1, destination: destination.1, exits: HashMap::new(), avoid, penalise, turned_away: std::cell::Cell::new(0) };
    for (i, _) in graph.nearest(destination.1, opts.entry_fixes, opts.max_direct_nm) {
        let from = graph.fixes[i].pos;
        if planner.clear(from, destination.1) {
            let t = planner.time(from, destination.1).0;
            planner.exits.insert(i, t);
        }
    }
    let (path, expanded) = planner.search()?;
    let filtered_out = planner.turned_away.get();
    let path = planner.refine(path);
    let name = |n: Node| match n {
        Node::Origin => origin.0.to_string(),
        Node::Destination => destination.0.to_string(),
        Node::Fix(i) => graph.fixes[i].id.clone(),
    };
    let mut legs = Vec::new();
    for pair in path.windows(2) {
        let (a, b) = (planner.pos(pair[0].0), planner.pos(pair[1].0));
        let (minutes, gs) = planner.time(a, b);
        legs.push(Leg { from: name(pair[0].0), to: name(pair[1].0), via: pair[0].1.clone(), nm: distance_nm(a, b), ground_speed_kt: gs, minutes, from_pos: a, to_pos: b });
    }
    Some(Plan { nm: legs.iter().map(|l| l.nm).sum(), minutes: legs.iter().map(|l| l.minutes).sum(), legs, expanded, filtered_out })
}

/// Plan a filed route: the procedures at each end, and the cheapest way between them over
/// the airway network, obeying every rule and hazard in the request.
pub fn plan_route(graph: &Graph, req: &crate::dispatch::RouteRequest) -> anyhow::Result<crate::dispatch::FiledRoute> {
    plan_routes(graph, req, 1)?.into_iter().next().ok_or_else(|| anyhow::anyhow!("no route"))
}

/// The best few routes, cheapest first: the same search, kept honest by being made to
/// find a route that differs from each one it has already found, so that a route a rule
/// turns down is not the end of the matter.
pub fn plan_routes(graph: &Graph, req: &crate::dispatch::RouteRequest, most: usize) -> anyhow::Result<Vec<crate::dispatch::FiledRoute>> {
    let _ = (graph, req, most);
    anyhow::bail!("plan_route is not built yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    const JET: Aircraft = Aircraft { tas_kt: 450.0, cruise_ft: 35000.0, ceiling_ft: 41000.0 };

    /// A ladder of two parallel airways, north (N) and south (S), from west to east, joined
    /// at each rung, between two airports at either end.
    fn ladder() -> Graph {
        let mut g = Graph::default();
        for i in 0..6 {
            let x = i as f64 * 1.0;
            let (n, s) = (format!("N{i}"), format!("S{i}"));
            if i > 0 {
                let (pn, ps) = (format!("N{}", i - 1), format!("S{}", i - 1));
                g.add("UN1", (&pn, (1.0, x - 1.0)), (&n, (1.0, x)), false, None, None);
                g.add("US2", (&ps, (-1.0, x - 1.0)), (&s, (-1.0, x)), false, None, None);
            }
            g.add("UR9", (&n, (1.0, x)), (&s, (-1.0, x)), false, None, None);
        }
        g
    }

    fn ends() -> ((&'static str, LatLon), (&'static str, LatLon)) {
        (("WEST", (0.0, -0.8)), ("EAST", (0.0, 5.8)))
    }

    fn tight() -> Options {
        Options { free_route: false, max_direct_nm: 90.0, ..Options::default() }
    }

    #[test]
    fn a_route_is_found_and_filed() {
        let g = ladder();
        let (o, d) = ends();
        let p = plan(&g, o, d, JET, &Conditions::default(), tight()).expect("a route");
        let s = p.route_string("WEST", "EAST");
        assert!(s.starts_with("WEST DCT ") && s.ends_with(" DCT EAST"), "{s}");
        assert!(p.minutes > 0.0 && p.nm > 300.0);
    }

    #[test]
    fn a_storm_on_one_airway_sends_the_route_down_the_other() {
        let g = ladder();
        let (o, d) = ends();
        // A cell over the middle of the northern airway, and the whole height of the sky.
        let wx = Conditions {
            winds: Vec::new(),
            hazards: vec![Hazard { name: "CB".into(), polygon: vec![(1.5, 2.2), (1.5, 2.8), (0.4, 2.8), (0.4, 2.2)], base_ft: 0.0, top_ft: 45000.0, kind: HazardKind::Avoid, active_from: None, active_to: None, source: String::new() }],
            ..Conditions::default()
        };
        let p = plan(&g, o, d, JET, &wx, tight()).expect("a route round it");
        assert!(p.legs.iter().all(|l| !(l.from.starts_with('N') && l.to.starts_with('N') && l.from_pos.1 >= 2.0 && l.to_pos.1 <= 3.0)), "{:?}", p.route_string("WEST", "EAST"));
        assert!(p.legs.iter().any(|l| l.via == "US2"));
    }

    #[test]
    fn a_storm_above_the_cruise_level_is_flown_under() {
        let g = ladder();
        let (o, d) = ends();
        let wx = Conditions {
            winds: Vec::new(),
            hazards: vec![Hazard { name: "HIGH".into(), polygon: vec![(3.0, -2.0), (3.0, 8.0), (-3.0, 8.0), (-3.0, -2.0)], base_ft: 39000.0, top_ft: 45000.0, kind: HazardKind::Avoid, active_from: None, active_to: None, source: String::new() }],
            ..Conditions::default()
        };
        assert!(plan(&g, o, d, JET, &wx, tight()).is_some());
    }

    #[test]
    fn a_strong_wind_on_one_side_decides_which_airway() {
        let g = ladder();
        let (o, d) = ends();
        // A westerly jet along the southern airway only: a tailwind worth taking.
        let mut winds = Vec::new();
        for i in 0..8 {
            winds.push(Wind { lat: -1.0, lon: i as f64 - 1.0, alt_ft: 35000.0, from_deg: 270.0, speed_kt: 150.0 });
            winds.push(Wind { lat: 1.0, lon: i as f64 - 1.0, alt_ft: 35000.0, from_deg: 90.0, speed_kt: 60.0 });
        }
        let p = plan(&g, o, d, JET, &Conditions { winds, ..Conditions::default() }, tight()).expect("a route");
        let south = p.legs.iter().filter(|l| l.via == "US2").count();
        let north = p.legs.iter().filter(|l| l.via == "UN1").count();
        assert!(south > north, "{}", p.route_string("WEST", "EAST"));
        assert!(p.legs.iter().filter(|l| l.via == "US2").all(|l| l.ground_speed_kt > 500.0));
    }

    #[test]
    fn an_airway_the_aircraft_cannot_reach_is_left_out() {
        let mut g = Graph::default();
        g.add("UHI", ("A", (0.0, 0.0)), ("B", (0.0, 1.0)), false, Some(45000.0), None);
        g.add("ULO", ("A", (0.0, 0.0)), ("C", (-0.5, 0.5)), false, None, None);
        g.add("ULO", ("C", (-0.5, 0.5)), ("B", (0.0, 1.0)), false, None, None);
        let p = plan(&g, ("X", (0.0, -0.3)), ("Y", (0.0, 1.3)), JET, &Conditions::default(), Options { free_route: false, max_direct_nm: 25.0, ..Options::default() }).expect("the low way");
        assert!(p.legs.iter().all(|l| l.via != "UHI"));
    }

    #[test]
    fn a_one_way_airway_is_flown_only_its_way() {
        let mut g = Graph::default();
        g.add("UOW", ("B", (0.0, 1.0)), ("A", (0.0, 0.0)), true, None, None);
        let found = plan(&g, ("X", (0.0, -0.2)), ("Y", (0.0, 1.2)), JET, &Conditions::default(), Options { free_route: false, max_direct_nm: 15.0, ..Options::default() });
        assert!(found.is_none(), "flown the wrong way along a one-way airway");
    }

    #[test]
    fn refinement_cuts_a_zig_zag_that_is_clear() {
        let mut g = Graph::default();
        g.add("UZZ", ("A", (0.0, 0.0)), ("B", (1.0, 0.5)), false, None, None);
        g.add("UZZ", ("B", (1.0, 0.5)), ("C", (0.0, 1.0)), false, None, None);
        let opts = Options { max_direct_nm: 70.0, ..Options::default() };
        let crooked = plan(&g, ("X", (0.0, -0.2)), ("Y", (0.0, 1.2)), JET, &Conditions::default(), Options { free_route: false, ..opts }).unwrap();
        let straight = plan(&g, ("X", (0.0, -0.2)), ("Y", (0.0, 1.2)), JET, &Conditions::default(), opts).unwrap();
        assert!(straight.minutes < crooked.minutes);
        assert!(straight.legs.iter().all(|l| l.to != "B"));
    }

    #[test]
    fn the_wind_triangle_takes_a_crosswind_off_the_ground_speed() {
        let wx = Conditions { winds: vec![Wind { lat: 0.0, lon: 0.0, alt_ft: 35000.0, from_deg: 0.0, speed_kt: 100.0 }], ..Conditions::default() };
        let east = wx.ground_speed((0.0, 0.0), 90.0, &JET);
        let south = wx.ground_speed((0.0, 0.0), 180.0, &JET);
        let north = wx.ground_speed((0.0, 0.0), 0.0, &JET);
        assert!((south - 550.0).abs() < 1.0 && (north - 350.0).abs() < 1.0);
        assert!(east < 450.0 && east > 430.0, "{east}");
    }
}
