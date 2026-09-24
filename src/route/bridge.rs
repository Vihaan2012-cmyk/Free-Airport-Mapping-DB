//! Joining the two halves of a route the published network does not join.
//!
//! Almost everywhere, a route from one airport to another can be flown on airways the whole
//! way, and where the airways run out a free-route direct leg of a couple of hundred miles —
//! six hundred over an ocean — chains across the gap. Over the pole neither is true. There are
//! no airways across the Arctic and no reporting points to chain through: a flight from Delhi
//! to San Francisco leaves the Russian network somewhere north of Siberia and rejoins the
//! North American one over the Canadian Arctic, and between those two fixes there is nothing
//! at all. The route is flown as one long direct leg, which is what the polar tracks are.
//!
//! So where every widening of the search has already failed, the two halves are found
//! separately and joined by a single direct leg. "Found separately" is the whole idea: the
//! furthest a search can actually get from the origin, and the furthest back it can get from
//! the destination, are each answerable without knowing how to cross the middle — they are
//! plain reachability questions over the network, which a breadth-first walk answers in a few
//! milliseconds over a hundred and fifty thousand edges. The leg between those two fixes is
//! then the one thing the network could not supply.
//!
//! This is deliberately the last thing tried. A direct leg of a thousand miles or more is not
//! something to offer a search that could have found a published route instead, because it
//! would take it: it is always shorter than going round.

use super::graph::Compact;
use crate::dispatch::{distance_nm, LatLon};

/// How much further than the great circle a bridged route may be before the bridge is judged
/// not to have helped.
///
/// A bridge is only ever built after every published way round has already failed, so the
/// question is not whether it is optimal but whether it is a route at all. Half again as far as
/// the direct line is a generous bound that still refuses the case this guards against: two
/// fixes that are each "furthest along" on their own side yet lie so far apart, or so far off
/// the line, that flying between them is not what the flight would do.
const WORTH_IT: f64 = 1.5;

/// The pair of fixes a bridging direct leg should join: the furthest a search can reach from
/// the origin, and the furthest back it can reach from the destination.
///
/// `None` where there is nothing useful to join — the two halves already meet, or the bridge
/// would be so long a detour that it is not a route the flight would fly.
pub fn span(compact: &Compact, starts: &[u32], goals: &[u32], origin: LatLon, destination: LatLon, usable: &dyn Fn(&super::graph::Edge) -> bool) -> Option<(u32, u32)> {
    let forward = reachable(compact, starts, usable, Direction::Forward);
    let backward = reachable(compact, goals, usable, Direction::Backward);
    if forward.is_empty() || backward.is_empty() {
        return None;
    }

    // The last fix on each side: the one that gets nearest the far end. That is what makes the
    // leg between them the shortest one that could possibly bridge the gap, without having to
    // weigh every pair of the two sides against each other.
    let last = |set: &[u32], towards: LatLon| -> Option<u32> { set.iter().copied().min_by(|&a, &b| distance_nm(compact.pos(a), towards).total_cmp(&distance_nm(compact.pos(b), towards))) };
    let a = last(&forward, destination)?;
    let b = last(&backward, origin)?;

    // The two halves already meet, so there is no gap and nothing to bridge; whatever stopped
    // the search was not a missing leg.
    if a == b {
        return None;
    }

    let direct = distance_nm(origin, destination).max(1.0);
    let bridged = distance_nm(origin, compact.pos(a)) + distance_nm(compact.pos(a), compact.pos(b)) + distance_nm(compact.pos(b), destination);
    if bridged > direct * WORTH_IT {
        return None;
    }
    Some((a, b))
}

enum Direction {
    Forward,
    Backward,
}

/// Every fix reachable from a set of fixes, following only edges a flight could use.
///
/// A plain breadth-first walk, because reachability is all that is asked: which fixes can be
/// got to at all, not how dearly. Pricing them would mean the whole cost model over a network
/// the search has already failed on, to answer a question that does not depend on the answer.
fn reachable(compact: &Compact, from: &[u32], usable: &dyn Fn(&super::graph::Edge) -> bool, direction: Direction) -> Vec<u32> {
    let mut seen = vec![false; compact.node_count()];
    let mut queue: Vec<u32> = Vec::new();
    for &node in from {
        if (node as usize) < seen.len() && !seen[node as usize] {
            seen[node as usize] = true;
            queue.push(node);
        }
    }
    let mut out = queue.clone();
    let mut head = 0;
    while head < queue.len() {
        let node = queue[head];
        head += 1;
        let edges = match direction {
            Direction::Forward => compact.out(node),
            Direction::Backward => compact.r#in(node),
        };
        for edge in edges {
            if !usable(edge) {
                continue;
            }
            let next = edge.to;
            if !seen[next as usize] {
                seen[next as usize] = true;
                queue.push(next);
                out.push(next);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::graph::Graph;

    /// Two clusters of fixes with no edge between them: a route from one to the other has to
    /// be bridged, and the bridge joins the innermost fix of each — the one nearest the gap.
    fn two_clusters() -> Graph {
        let mut g = Graph::default();
        // A west cluster along the equator, joined along itself, and an east cluster a long way
        // off joined along itself, with nothing at all between the two.
        for i in 0..3 {
            g.add(
                "WEST",
                (&format!("W{i}"), (0.0, i as f64)),
                (&format!("W{}", i + 1), (0.0, (i + 1) as f64)),
                false,
                None,
                None,
            );
            g.add(
                "EAST",
                (&format!("E{i}"), (0.0, 40.0 + i as f64)),
                (&format!("E{}", i + 1), (0.0, 40.0 + (i + 1) as f64)),
                false,
                None,
                None,
            );
        }
        g
    }

    #[test]
    fn the_bridge_joins_the_last_fix_of_each_side() {
        let g = two_clusters();
        let compact = g.compact();
        let origin = (0.0, 0.0);
        let destination = (0.0, 43.0);
        let starts = vec![g.find("W0", origin).expect("W0")];
        let goals = vec![g.find("E3", destination).expect("E3")];
        let (a, b) = span(&compact, &starts, &goals, origin, destination, &|_| true).expect("a bridge");
        // W3 is the furthest east the west cluster reaches; E0 the furthest west the east
        // cluster reaches back. The gap between them is what the leg crosses.
        assert_eq!(g.fix_id(a), g.fix_id(g.find("W3", (0.0, 3.0)).expect("W3")));
        assert_eq!(g.fix_id(b), g.fix_id(g.find("E0", (0.0, 40.0)).expect("E0")));
    }

    /// Where the two sides already meet there is no gap, so no bridge is offered: whatever
    /// stopped the search was not a missing leg, and a direct leg would not help it.
    #[test]
    fn no_bridge_where_the_two_sides_already_meet() {
        let mut g = two_clusters();
        g.add("JOIN", ("W3", (0.0, 3.0)), ("E0", (0.0, 40.0)), false, None, None);
        let compact = g.compact();
        let origin = (0.0, 0.0);
        let destination = (0.0, 43.0);
        let starts = vec![g.find("W0", origin).expect("W0")];
        let goals = vec![g.find("E3", destination).expect("E3")];
        assert!(span(&compact, &starts, &goals, origin, destination, &|_| true).is_none());
    }

    /// A bridge that would make the route half again as long as flying direct is not a route
    /// the flight would fly, so it is refused rather than returned as the best available.
    #[test]
    fn no_bridge_where_it_would_be_a_detour_rather_than_a_crossing() {
        let mut g = Graph::default();
        // Both clusters sit a long way off the line between the two airports, in opposite
        // directions, so joining their innermost fixes is a detour and not a crossing.
        g.add("NORTH", ("N0", (60.0, 0.0)), ("N1", (61.0, 1.0)), false, None, None);
        g.add("SOUTH", ("S0", (-60.0, 9.0)), ("S1", (-61.0, 10.0)), false, None, None);
        let compact = g.compact();
        let origin = (0.0, 0.0);
        let destination = (0.0, 10.0);
        let starts = vec![g.find("N0", (60.0, 0.0)).expect("N0")];
        let goals = vec![g.find("S1", (-61.0, 10.0)).expect("S1")];
        assert!(span(&compact, &starts, &goals, origin, destination, &|_| true).is_none());
    }

    /// An edge a flight could not use is not a way through: reachability honours the same
    /// level limits the search does, or the bridge would be built from the wrong fix.
    #[test]
    fn an_unusable_edge_is_not_a_way_through() {
        let g = two_clusters();
        let compact = g.compact();
        let origin = (0.0, 0.0);
        let destination = (0.0, 43.0);
        let starts = vec![g.find("W0", origin).expect("W0")];
        let goals = vec![g.find("E3", destination).expect("E3")];
        // With nothing usable, each side reaches only the fix it started on.
        let (a, b) = span(&compact, &starts, &goals, origin, destination, &|_| false).expect("a bridge from the seeds alone");
        assert_eq!(g.fix_id(a), g.fix_id(starts[0]));
        assert_eq!(g.fix_id(b), g.fix_id(goals[0]));
    }
}

#[cfg(test)]
mod diagnosis {
    use super::*;
    use crate::route::graph::Graph;

    /// Printed, not asserted: whether the network actually joins two airports at all.
    ///
    /// This is the fact that tells a failure to find a route apart from a failure to have one.
    /// If the destination's own join fixes are reachable from the origin's, every rung of the
    /// ladder failing is the search going the wrong way, and no bridging leg will help it.
    #[test]
    #[ignore]
    fn report_whether_the_network_joins_these_airports() {
        let g = Graph::shared();
        let compact = g.compact();
        for (from, to, o, d) in [("VIDP", "KSFO", (28.5665, 77.1031), (37.6188, -122.3754)), ("VOBL", "KJFK", (13.1979, 77.7063), (40.6398, -73.7789))] {
            let starts: Vec<u32> = g.nearest(o, 64, 220.0).into_iter().map(|(n, _)| n).collect();
            let goals: Vec<u32> = g.nearest(d, 64, 220.0).into_iter().map(|(n, _)| n).collect();
            let forward = reachable(&compact, &starts, &|_| true, Direction::Forward);
            let joins = goals.iter().any(|gl| forward.contains(gl));
            let nearest = forward.iter().copied().min_by(|&a, &b| distance_nm(compact.pos(a), d).total_cmp(&distance_nm(compact.pos(b), d)));
            println!("{from} -> {to}: {} start(s), {} goal(s), {} fixes reachable", starts.len(), goals.len(), forward.len());
            println!("   destination reachable on airways: {joins}");
            if let Some(n) = nearest {
                println!("   nearest reachable fix to {to}: {} at {:.0} nm", g.fix_id(n), distance_nm(compact.pos(n), d));
            }
        }
    }
}

#[cfg(test)]
mod optimum {
    use super::*;
    use crate::route::graph::Graph;
    use std::collections::BinaryHeap;

    /// Printed, not asserted: the shortest way through the network by distance alone, found
    /// exactly by Dijkstra, and where it goes.
    ///
    /// This is the benchmark the search is trying to approach. Distance is not what a route is
    /// chosen on — wind and cost index are — but a route far longer than this one is not long
    /// because of the wind, and a search that finds nothing at all when this finds a way has
    /// gone the wrong way rather than run out of network.
    #[test]
    #[ignore]
    fn report_the_shortest_way_through_the_network() {
        let g = Graph::shared();
        let compact = g.compact();
        for (from, to, o, d, direct_nm) in [("VIDP", "KSFO", (28.5665, 77.1031), (37.6188, -122.3754), 0.0), ("VIDP", "KSFO", (28.5665, 77.1031), (37.6188, -122.3754), 600.0), ("VOBL", "KJFK", (13.1979, 77.7063), (40.6398, -73.7789), 0.0), ("VOBL", "KJFK", (13.1979, 77.7063), (40.6398, -73.7789), 600.0)] {
            let starts: Vec<u32> = g.nearest(o, 64, 220.0).into_iter().map(|(n, _)| n).collect();
            let goals: Vec<u32> = g.nearest(d, 64, 220.0).into_iter().map(|(n, _)| n).collect();
            // With `direct_nm` of nought the walk uses published airways alone; with a figure
            // it may also take the free-route direct legs an oceanic crossing is actually flown
            // on, which is the network the search really sees.
            let legs = (direct_nm > 0.0).then(|| crate::route::directs::build(&compact, None, d, direct_nm));
            let n = compact.node_count();
            let mut dist = vec![f64::INFINITY; n];
            let mut prev = vec![u32::MAX; n];
            // Ordered by tenths of a mile as an integer, which a binary heap can order and a
            // float cannot, and which is finer than any distance here is meaningful to.
            let key = |nm: f64| std::cmp::Reverse((nm * 10.0) as u64);
            let mut heap: BinaryHeap<(std::cmp::Reverse<u64>, u32)> = BinaryHeap::new();
            for &s in &starts {
                dist[s as usize] = 0.0;
                heap.push((key(0.0), s));
            }
            while let Some((std::cmp::Reverse(raw), node)) = heap.pop() {
                let cost = raw as f64 / 10.0;
                if cost > dist[node as usize] + 0.11 {
                    continue;
                }
                let cost = dist[node as usize];
                let airway = compact.out(node).iter().map(|e| e.to);
                let free = legs.as_ref().map(|l| l.out(node)).unwrap_or(&[]).iter().copied();
                for next in airway.chain(free) {
                    let step = distance_nm(compact.pos(node), compact.pos(next));
                    if cost + step < dist[next as usize] {
                        dist[next as usize] = cost + step;
                        prev[next as usize] = node;
                        heap.push((key(cost + step), next));
                    }
                }
            }
            let best = goals.iter().copied().filter(|&gl| dist[gl as usize].is_finite()).min_by(|&a, &b| dist[a as usize].total_cmp(&dist[b as usize]));
            let direct = distance_nm(o, d);
            match best {
                None => println!("{from} -> {to}: nothing connects"),
                Some(goal) => {
                    let _ = direct_nm;
                    let mut path = Vec::new();
                    let mut at = goal;
                    while at != u32::MAX {
                        path.push(at);
                        at = prev[at as usize];
                    }
                    path.reverse();
                    println!("{from} -> {to} (directs up to {direct_nm:.0} nm): {:.0} nm, {direct:.0} nm direct ({:.0}% over), {} fixes", dist[goal as usize], (dist[goal as usize] / direct - 1.0) * 100.0, path.len());
                    // Where it goes, sampled, so polar and Pacific are told apart at a glance.
                    let step = (path.len() / 12).max(1);
                    let trace: Vec<String> = path.iter().step_by(step).map(|&nd| { let p = compact.pos(nd); format!("{}({:.0},{:.0})", g.fix_id(nd), p.0, p.1) }).collect();
                    println!("   {}", trace.join(" "));
                }
            }
        }
    }
}
