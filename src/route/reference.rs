//! The shortest way through the network by distance alone, found exactly.
//!
//! This is not a route and is not meant to be flown: it knows nothing of wind, cost index,
//! levels, ETOPS or any other rule, and a real route is rightly longer than it. What it is, is
//! the floor — no route through this network can be shorter — and that makes it the one honest
//! answer to "should the search have done better than this?".
//!
//! Without it, a route nine thousand miles long against a seven-thousand-mile great circle is
//! just a number: the detour might be the airways, or the rules, or the search giving up early,
//! and there is no way to tell which from the route alone. With it, the three are separable. If
//! the floor is also nine thousand, the network has no better way and the search is doing its
//! job; if the floor is seven, the miles are being lost somewhere this measures and the search
//! is where to look.
//!
//! Dijkstra rather than anything cleverer, because it is asked once, off the critical path, and
//! an exact answer is the entire point of asking.

use super::directs;
use super::graph::Graph;
use crate::dispatch::{distance_nm, LatLon};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// How near an airport a fix has to be to serve as a way on or off the network.
const JOIN_NM: f64 = 220.0;
const JOIN_WIDTH: usize = 64;

/// The shortest way from one airport to another through the network, and how long it is.
///
/// `direct_max_nm` says how long a free-route direct leg may be; nought restricts the walk to
/// published airways alone. The difference between the two answers is worth knowing on its own:
/// on an oceanic sector it is the difference between a route and a detour round half the world.
pub fn shortest_path(graph: &Graph, origin: LatLon, destination: LatLon, direct_max_nm: f64) -> Option<(Vec<(String, LatLon)>, f64)> {
    let compact = graph.compact();
    let starts: Vec<u32> = graph.nearest(origin, JOIN_WIDTH, JOIN_NM).into_iter().map(|(n, _)| n).collect();
    let goals: Vec<u32> = graph.nearest(destination, JOIN_WIDTH, JOIN_NM).into_iter().map(|(n, _)| n).collect();
    if starts.is_empty() || goals.is_empty() {
        return None;
    }
    let legs = (direct_max_nm > 0.0).then(|| directs::build(&compact, None, destination, direct_max_nm));

    let n = compact.node_count();
    let mut dist = vec![f64::INFINITY; n];
    let mut prev = vec![u32::MAX; n];
    // Ordered by tenths of a mile as an integer, which a binary heap can order and a float
    // cannot, and which is finer than any distance here means anything to.
    let key = |nm: f64| Reverse((nm * 10.0) as u64);
    let mut heap: BinaryHeap<(Reverse<u64>, u32)> = BinaryHeap::new();
    for &s in &starts {
        dist[s as usize] = 0.0;
        heap.push((key(0.0), s));
    }
    while let Some((Reverse(raw), node)) = heap.pop() {
        if raw as f64 / 10.0 > dist[node as usize] + 0.11 {
            continue;
        }
        let cost = dist[node as usize];
        let airways = compact.out(node).iter().map(|e| e.to);
        let free = legs.as_ref().map(|l| l.out(node)).unwrap_or(&[]).iter().copied();
        for next in airways.chain(free) {
            let step = distance_nm(compact.pos(node), compact.pos(next));
            if cost + step < dist[next as usize] {
                dist[next as usize] = cost + step;
                prev[next as usize] = node;
                heap.push((key(cost + step), next));
            }
        }
    }

    let goal = goals.iter().copied().filter(|&g| dist[g as usize].is_finite()).min_by(|&a, &b| dist[a as usize].total_cmp(&dist[b as usize]))?;
    let mut path = Vec::new();
    let mut at = goal;
    while at != u32::MAX {
        path.push((graph.fix_id(at).to_string(), compact.pos(at)));
        at = prev[at as usize];
    }
    path.reverse();
    Some((path, dist[goal as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line of fixes with an airway along it and a short cut off to one side: the walk takes
    /// the short cut when a direct leg reaches it and the airway when none does.
    fn zigzag() -> Graph {
        let mut g = Graph::default();
        // The airway goes out and back, so following it is much longer than cutting across.
        g.add("LONG", ("A", (0.0, 0.0)), ("B", (8.0, 2.0)), false, None, None);
        g.add("LONG", ("B", (8.0, 2.0)), ("C", (0.0, 4.0)), false, None, None);
        g.add("LONG", ("C", (0.0, 4.0)), ("D", (8.0, 6.0)), false, None, None);
        g.add("LONG", ("D", (8.0, 6.0)), ("E", (0.0, 8.0)), false, None, None);
        g
    }

    #[test]
    fn the_floor_on_airways_alone_follows_the_airways() {
        let g = zigzag();
        let (path, nm) = shortest_path(&g, (0.0, 0.0), (0.0, 8.0), 0.0).expect("a way through");
        assert_eq!(path.first().map(|p| p.0.as_str()), Some("A"));
        assert_eq!(path.last().map(|p| p.0.as_str()), Some("E"));
        // Out and back four times is far longer than the 480 nm straight across.
        assert!(nm > 1400.0, "{nm:.0} nm on the airway");
    }

    /// With a direct leg long enough to cut across, the floor drops to very near the straight
    /// line. This is the measurement that told us the network was not the problem: Delhi to San
    /// Francisco is 85 per cent over the great circle on airways alone and 1 per cent with
    /// direct legs, so the miles our search was losing were not the network's to lose.
    #[test]
    fn a_direct_leg_cuts_the_corner_and_the_floor_falls() {
        let g = zigzag();
        let (_, airways_only) = shortest_path(&g, (0.0, 0.0), (0.0, 8.0), 0.0).expect("a way through");
        let (path, with_directs) = shortest_path(&g, (0.0, 0.0), (0.0, 8.0), 600.0).expect("a way through");
        assert!(with_directs < airways_only / 2.0, "{with_directs:.0} against {airways_only:.0}");
        let straight = distance_nm((0.0, 0.0), (0.0, 8.0));
        assert!(with_directs < straight * 1.05, "{with_directs:.0} against {straight:.0} straight");
        assert_eq!(path.last().map(|p| p.0.as_str()), Some("E"));
    }

    /// Two airports the network does not join have no floor at all, which is a different answer
    /// from a floor that happens to be large.
    #[test]
    fn nothing_connects_is_answered_as_nothing_rather_than_as_a_huge_distance() {
        let mut g = Graph::default();
        g.add("WEST", ("A", (0.0, 0.0)), ("B", (0.0, 1.0)), false, None, None);
        assert!(shortest_path(&g, (0.0, 0.0), (0.0, 120.0), 0.0).is_none());
    }
}
