//! ALT: A*, Landmarks and Triangle inequality (Goldberg & Harrelson), the lower bound the
//! exact search runs on.
//!
//! A straight-line distance is an honest lower bound on how far is left to fly, but a poor
//! one once a flight has to go round something the size of the Himalayas or the North
//! Atlantic track system: the search learns nothing from it until it is nearly there. A
//! handful of landmarks spread round the network, each with its distance to and from every
//! fix worked out once by Dijkstra, gives a much tighter bound from the triangle
//! inequality — `d(v,t) >= d(v,L) - d(t,L)` for any landmark `L` — without ever being able
//! to overestimate, which is what keeps the search that uses it exact.
//!
//! The tables are the expensive part: 2 × landmarks Dijkstra runs over the whole network.
//! They depend only on the network's shape, which changes with the AIRAC cycle and nothing
//! else a flight plan varies, so they are built once per cycle and kept in the cache.

use super::graph::Compact;

/// How long a free-route leg the bound assumes, and how many each fix keeps. The length is the
/// longest any rung of the search offers, so the bound never assumes less of the network than
/// the search may use; the count is a little above what a flight's own list keeps, since that
/// list is filtered by where the flight is going and its legs need not be the very nearest.
const FREE_LEG_NM: f64 = 600.0;
const FREE_LEG_NEIGHBOURS: usize = 8;
use super::ord32::OrderedF32;
use std::collections::BinaryHeap;

/// The landmark tables: for each of a handful of fixes spread round the network, its
/// distance to every fix and from every fix, nautical miles.
pub struct Landmarks {
    pub ids: Vec<u32>,
    from: Vec<f32>,
    to: Vec<f32>,
    n_nodes: usize,
}

impl Landmarks {
    /// A landmark's distance *to* a fix: exposed alongside [`Landmarks::to_at`] so that
    /// [`super::search::heuristic_to`] can gather a search's small, fixed set of goals into
    /// its own tables once, rather than read a landmark's row — one of up to sixty-odd
    /// thousand entries — anew for every push.
    pub fn from_at(&self, l: usize, v: u32) -> f32 {
        self.from[l * self.n_nodes + v as usize]
    }

    /// A landmark's distance *from* a fix: see [`Landmarks::from_at`].
    pub fn to_at(&self, l: usize, v: u32) -> f32 {
        self.to[l * self.n_nodes + v as usize]
    }

    /// A lower bound on the great-circle distance remaining from `v` to `t`, nautical
    /// miles: the greatest, over every landmark, of the two triangle-inequality bounds a
    /// directed graph allows — one built from what is known of the way there, the other
    /// from what is known of the way back, since on a one-way airway they can differ.
    ///
    /// A landmark neither reachable from `v` nor able to reach `t` (or the other way
    /// about) carries an infinite distance on one side of its own term, which is not a
    /// bound at all — subtracting a finite figure from it would otherwise poison the
    /// `max` with an infinity and turn an admissible heuristic into one that overestimates
    /// wildly, so a landmark that cannot speak to a pair is simply left out of the vote
    /// rather than allowed to dominate it.
    pub fn lower_bound_nm(&self, v: u32, t: u32) -> f32 {
        if v == t {
            return 0.0;
        }
        let mut best = 0.0f32;
        for l in 0..self.ids.len() {
            let via_v = self.to_at(l, v) - self.to_at(l, t);
            let via_t = self.from_at(l, t) - self.from_at(l, v);
            if via_v.is_finite() {
                best = best.max(via_v);
            }
            if via_t.is_finite() {
                best = best.max(via_t);
            }
        }
        best.max(0.0)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// A search's goals, gathered out of these tables once: every landmark's distance to
    /// and from each of them, in a small array the search rereads on every push instead of
    /// this one. A search with up to a few dozen goals and two dozen landmarks otherwise
    /// touches over a thousand entries scattered through tables sized by the whole network —
    /// a cache miss apiece — on every single state it looks at; gathered once, the same
    /// figures come from a few hundred bytes that stay hot for the rest of the search.
    pub fn gather(&self, goals: &[u32]) -> GoalRows {
        let mut to = Vec::with_capacity(self.ids.len() * goals.len());
        let mut from = Vec::with_capacity(self.ids.len() * goals.len());
        for l in 0..self.ids.len() {
            for &t in goals {
                to.push(self.to_at(l, t));
                from.push(self.from_at(l, t));
            }
        }
        GoalRows { to, from, n_goals: goals.len() }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.ids.len() * 4 + (self.from.len() + self.to.len()) * 4);
        out.extend((self.ids.len() as u32).to_le_bytes());
        out.extend((self.n_nodes as u32).to_le_bytes());
        for id in &self.ids {
            out.extend(id.to_le_bytes());
        }
        for v in &self.from {
            out.extend(v.to_le_bytes());
        }
        for v in &self.to {
            out.extend(v.to_le_bytes());
        }
        out
    }

    fn from_bytes(bytes: &[u8]) -> Option<Landmarks> {
        let mut at = 0usize;
        let take_u32 = |at: &mut usize| -> Option<u32> {
            let v = bytes.get(*at..*at + 4)?;
            *at += 4;
            Some(u32::from_le_bytes(v.try_into().ok()?))
        };
        let n_landmarks = take_u32(&mut at)? as usize;
        let n_nodes = take_u32(&mut at)? as usize;
        let mut ids = Vec::with_capacity(n_landmarks);
        for _ in 0..n_landmarks {
            ids.push(take_u32(&mut at)?);
        }
        let take_f32s = |at: &mut usize, count: usize| -> Option<Vec<f32>> {
            let mut out = Vec::with_capacity(count);
            for _ in 0..count {
                let v = bytes.get(*at..*at + 4)?;
                *at += 4;
                out.push(f32::from_le_bytes(v.try_into().ok()?));
            }
            Some(out)
        };
        let from = take_f32s(&mut at, n_landmarks * n_nodes)?;
        let to = take_f32s(&mut at, n_landmarks * n_nodes)?;
        Some(Landmarks { ids, from, to, n_nodes })
    }
}

/// A search's goals' rows out of the landmark tables, gathered once by [`Landmarks::gather`]
/// rather than read from the full tables on every push: `landmarks × goals` entries, small
/// enough to stay in cache for the life of one search.
pub struct GoalRows {
    to: Vec<f32>,
    from: Vec<f32>,
    n_goals: usize,
}

impl GoalRows {
    pub fn to_at(&self, l: usize, goal_idx: usize) -> f32 {
        self.to[l * self.n_goals + goal_idx]
    }

    pub fn from_at(&self, l: usize, goal_idx: usize) -> f32 {
        self.from[l * self.n_goals + goal_idx]
    }
}

/// Plain, single-source Dijkstra on great-circle distance, forward if `reverse` is false
/// and along the reverse graph (so the result is "distance to the source") if it is true.
fn dijkstra_distance(compact: &Compact, free: &super::directs::Directs, source: u32, reverse: bool) -> Vec<f32> {
    let n = compact.node_count();
    let mut dist = vec![f32::INFINITY; n];
    let mut heap: BinaryHeap<std::cmp::Reverse<(OrderedF32, u32)>> = BinaryHeap::new();
    dist[source as usize] = 0.0;
    heap.push(std::cmp::Reverse((OrderedF32(0.0), source)));
    while let Some(std::cmp::Reverse((d, u))) = heap.pop() {
        if d.0 > dist[u as usize] {
            continue;
        }
        let (upos, ucos) = (compact.pos(u), compact.cos_lat[u as usize] as f64);
        let airways: &[super::graph::Edge] = if reverse { compact.r#in(u) } else { compact.out(u) };
        // The free-route legs are the same either way round, so one neighbourhood serves the
        // forward run and the reverse one alike.
        for v in airways.iter().map(|e| e.to).chain(free.out(u).iter().copied()) {
            let vpos = compact.pos(v);
            let w = fast_distance_nm(upos, ucos, vpos) as f32;
            let nd = d.0 + w;
            if nd < dist[v as usize] {
                dist[v as usize] = nd;
                heap.push(std::cmp::Reverse((OrderedF32(nd), v)));
            }
        }
    }
    dist
}

/// Haversine with the source's cosine of latitude already in hand: what the landmark
/// tables are built with, and what the search's own distance bound is measured in, so the
/// two never disagree over a fraction of a mile.
pub fn fast_distance_nm(a: (f64, f64), cos_a: f64, b: (f64, f64)) -> f64 {
    let (p1, p2) = (a.0.to_radians(), b.0.to_radians());
    let dp = p2 - p1;
    let dl = (b.1 - a.1).to_radians();
    let h = (dp / 2.0).sin().powi(2) + cos_a * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * crate::dispatch::EARTH_NM * h.sqrt().min(1.0).asin()
}

/// Landmarks chosen by farthest-point sampling: the first arbitrary, each next the fix
/// known to be furthest from every landmark chosen so far, so that a handful of them
/// spread to the corners of the network rather than clustering in the middle of it.
fn choose_landmarks(compact: &Compact, free: &super::directs::Directs, count: usize) -> Vec<u32> {
    let n = compact.node_count();
    if n == 0 {
        return Vec::new();
    }
    let count = count.min(n);
    let mut chosen = Vec::with_capacity(count);
    let mut min_dist = vec![f32::INFINITY; n];
    // Start from the fix furthest north: as good an arbitrary seed as any, and
    // deterministic, which a test can rely on.
    let mut next = (0..n as u32).max_by(|&a, &b| compact.lat[a as usize].total_cmp(&compact.lat[b as usize])).unwrap();
    for _ in 0..count {
        chosen.push(next);
        let d = dijkstra_distance(compact, free, next, false);
        for i in 0..n {
            if d[i] < min_dist[i] {
                min_dist[i] = d[i];
            }
        }
        next = match (0..n as u32).filter(|&i| !chosen.contains(&i)).max_by(|&a, &b| min_dist[a as usize].total_cmp(&min_dist[b as usize])) {
            Some(i) => i,
            None => break,
        };
    }
    chosen
}

/// Build the tables from scratch: the expensive step, `2 × landmarks` runs of Dijkstra
/// over the whole network.
fn build(compact: &Compact, count: usize) -> Landmarks {
    // The network the bound is measured over: the published airways, and the free-route legs a
    // search may fly instead of them. Leaving the legs out made the bound an over-estimate
    // rather than a bound, and a search cannot be steered by a figure that is too large.
    let free = super::directs::neighbourhood(compact, FREE_LEG_NM, FREE_LEG_NEIGHBOURS);
    let ids = choose_landmarks(compact, &free, count);
    let n = compact.node_count();
    let mut from = vec![0f32; ids.len() * n];
    let mut to = vec![0f32; ids.len() * n];
    // One graph, many independent single-source runs: exactly the shape rayon is for.
    use rayon::prelude::*;
    let rows: Vec<(Vec<f32>, Vec<f32>)> = ids.par_iter().map(|&l| (dijkstra_distance(compact, &free, l, false), dijkstra_distance(compact, &free, l, true))).collect();
    for (i, (f, t)) in rows.into_iter().enumerate() {
        from[i * n..(i + 1) * n].copy_from_slice(&f);
        to[i * n..(i + 1) * n].copy_from_slice(&t);
    }
    Landmarks { ids, from, to, n_nodes: n }
}

/// The landmark tables for a network, built once and kept: on disk keyed by the navigation
/// data's AIRAC cycle when one is known, so a second process on the same cycle finds them
/// already there, and in memory for the rest of one process's life either way.
pub fn tables(compact: &Compact, count: usize) -> std::sync::Arc<Landmarks> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<(u64, usize), std::sync::Arc<Landmarks>>>> = std::sync::OnceLock::new();
    // Keyed by the network's own content, not merely its size: two different networks
    // that happen to share a fix count and an edge count are not the same network, and
    // serving one's landmark tables to a search over the other silently turns an
    // admissible bound into one that is not.
    let key = (content_hash(compact), count);
    let cache = CACHE.get_or_init(Default::default);
    if let Some(found) = cache.lock().unwrap().get(&key) {
        return found.clone();
    }
    // The free legs are part of what the tables measure, so their shape is part of the key:
    // tables built without them answer a different question, and serving those would quietly
    // restore the over-estimate this replaced.
    let disk_key = crate::sources::navdata::airac().map(|airac| format!("route/landmarks-{airac}-{}-{count}-f{}x{FREE_LEG_NEIGHBOURS}.bin", key.0, FREE_LEG_NM as u32));
    let store = crate::cache::Cache::for_index(false);
    let built = if let Some(key) = &disk_key {
        let bytes = store.get_or_fetch_bytes(key, || Ok(build(compact, count).to_bytes())).ok();
        bytes.and_then(|b| Landmarks::from_bytes(&b)).unwrap_or_else(|| build(compact, count))
    } else {
        build(compact, count)
    };
    let built = std::sync::Arc::new(built);
    cache.lock().unwrap().insert(key, built.clone());
    built
}

/// A fast, content-based fingerprint of the network: every fix's position and every
/// edge's endpoint and airway, folded together. Cheap enough next to the Dijkstra runs
/// this guards the cache of that hashing a worldwide network once a process costs nothing
/// worth measuring.
fn content_hash(compact: &Compact) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    compact.node_count().hash(&mut h);
    compact.offsets.hash(&mut h);
    for e in &compact.edges {
        e.to.hash(&mut h);
        e.airway_id().hash(&mut h);
    }
    for &v in &compact.lat {
        v.to_bits().hash(&mut h);
    }
    for &v in &compact.lon {
        v.to_bits().hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::graph::Graph;

    /// A ladder network wide enough that the straight line between the ends is a poor
    /// estimate of the way round: landmarks should still bound the true distance without
    /// ever exceeding it.
    fn ladder(rungs: usize) -> Graph {
        let mut g = Graph::default();
        for i in 0..rungs {
            let x = i as f64;
            if i > 0 {
                g.add("N", (&format!("N{}", i - 1), (1.0, x - 1.0)), (&format!("N{i}"), (1.0, x)), false, None, None);
                g.add("S", (&format!("S{}", i - 1), (-1.0, x - 1.0)), (&format!("S{i}"), (-1.0, x)), false, None, None);
            }
            g.add("R", (&format!("N{i}"), (1.0, x)), (&format!("S{i}"), (-1.0, x)), false, None, None);
        }
        g
    }

    #[test]
    fn the_bound_never_exceeds_the_true_distance() {
        let g = ladder(12);
        let compact = g.compact();
        let lm = tables_for_test(&compact, 6);
        for a in 0..compact.node_count() as u32 {
            let free = crate::route::directs::neighbourhood(&compact, FREE_LEG_NM, FREE_LEG_NEIGHBOURS);
            let dijkstra = dijkstra_distance(&compact, &free, a, false);
            for b in 0..compact.node_count() as u32 {
                let d = dijkstra[b as usize];
                if d.is_finite() {
                    let bound = lm.lower_bound_nm(a, b);
                    assert!(bound <= d + 1e-3, "landmark bound {bound} exceeded the true distance {d} from {a} to {b}");
                }
            }
        }
    }

    #[test]
    fn a_landmark_is_never_further_from_itself_than_zero() {
        let g = ladder(6);
        let compact = g.compact();
        let lm = tables_for_test(&compact, 4);
        for &l in &lm.ids {
            assert_eq!(lm.lower_bound_nm(l, l), 0.0);
        }
    }

    #[test]
    fn round_trips_through_bytes() {
        let g = ladder(5);
        let compact = g.compact();
        let lm = build(&compact, 3);
        let bytes = lm.to_bytes();
        let back = Landmarks::from_bytes(&bytes).unwrap();
        assert_eq!(back.ids, lm.ids);
        assert_eq!(back.from, lm.from);
        assert_eq!(back.to, lm.to);
    }

    /// The in-process build, without touching the disk cache: what the unit tests use, so
    /// they never depend on `LOCALAPPDATA` or an AIRAC cycle being available.
    fn tables_for_test(compact: &Compact, count: usize) -> Landmarks {
        build(compact, count)
    }
}

#[cfg(test)]
mod bound_report {
    use super::*;
    use crate::route::graph::Graph;

    /// Printed, not asserted: what the bound actually says, against the straight line it can
    /// never be less than and the floor it must never exceed.
    ///
    /// A bound larger than the floor is not a bound. It is the one number that says whether the
    /// tables are steering the search or misleading it, and it is worth reading directly rather
    /// than inferring from the routes that come out.
    #[test]
    #[ignore]
    fn report_the_bound_against_the_floor() {
        let g = Graph::shared();
        let compact = g.compact();
        let tables = tables(&compact, 24);
        for (from, to, o, d) in [("VIDP", "KSFO", (28.5665, 77.1031), (37.6188, -122.3754)), ("VOBL", "KJFK", (13.1979, 77.7063), (40.6398, -73.7789))] {
            let (Some((s, _)), Some((t, _))) = (g.nearest(o, 1, 220.0).first().copied(), g.nearest(d, 1, 220.0).first().copied()) else { continue };
            let straight = crate::dispatch::distance_nm(compact.pos(s), compact.pos(t));
            let bound = tables.lower_bound_nm(s, t);
            let floor = crate::route::reference::shortest_path(g, o, d, 600.0).map(|(_, nm)| nm).unwrap_or(f64::NAN);
            println!("{from} -> {to}: straight {straight:.0} nm, bound {bound:.0} nm, floor {floor:.0} nm");
            println!("   the bound is {}", if bound as f64 <= floor + 1.0 { "a bound" } else { "LARGER THAN THE FLOOR, so not a bound" });
        }
    }
}
