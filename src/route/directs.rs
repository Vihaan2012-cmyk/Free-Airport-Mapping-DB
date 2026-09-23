//! Free-route direct legs: a short list of nearby fixes, further along the route than a fix
//! itself, that a search may fly to directly rather than by a published airway.
//!
//! Every pair of fixes in a worldwide network is a hundred-thousand squared — impossible to
//! even enumerate, let alone search. What makes a direct leg affordable is asking a much
//! smaller question for each fix rather than the impossible one for every pair: not "which
//! of every other fix could this reach direct", but "of the handful of fixes actually near
//! this one, which lie further towards the destination than it already is" — answered from
//! [`super::spatial`]'s coarse grid, a lookup over a few cells, never a scan of the network.
//!
//! The "further along" rule is what keeps this from being another O(n²) in disguise once the
//! grid has narrowed a fix down to its geographic neighbours: without it, two fixes a few
//! miles apart either side of the route would each offer the other as a direct leg, and nine
//! of the resulting steps in ten would just be a detour the search then has to notice and
//! refuse. Measuring "further along" against the destination alone, rather than against the
//! whole route so far, is what makes it cheap — one more distance per candidate, no state
//! about the route carried into a build that happens once, long before there is a route to
//! carry state about.

use super::ellipse::Ellipse;
use super::graph::Compact;
use super::spatial::Grid;
use crate::dispatch::{distance_nm, LatLon};

/// How many direct-leg neighbours a fix keeps, nearest first: enough to give the search a
/// real choice of where to go without every fix fanning out into a search of its own.
const MAX_NEIGHBOURS: usize = 6;
/// The width of a cell in the grid this builds for itself: wide enough that even the longer,
/// oceanic leg length touches only a handful of cells. Building the adjacency over busy,
/// fix-dense airspace — thousands of candidates within even a thin ellipse's own reach of a
/// European hub — is the real cost this pays; tuning the cell width narrower was tried and
/// made no measurable difference, since [`Grid::near`]'s own scan touches more, smaller
/// cells rather than fewer points.
const CELL_DEG: f64 = 2.0;

/// The direct-leg adjacency for one attempt: built once, read by every level and every beam
/// width the portfolio in [`super::Context::attempt`] tries, since which fixes are near
/// which other fixes does not depend on level — only what a leg between them costs does, and
/// that is priced separately, per level, by [`super::cost::LazyLevel::direct`].
pub struct Directs {
    /// Flattened, `Compact`-shaped adjacency: `offsets[i]..offsets[i+1]` into `neighbours`
    /// gives the fix `i`'s own direct-leg neighbours, so a search reads this exactly the way
    /// it already reads `Compact::out`.
    offsets: Vec<u32>,
    neighbours: Vec<u32>,
}

impl Directs {
    pub fn out(&self, node: u32) -> &[u32] {
        let (a, b) = (self.offsets[node as usize] as usize, self.offsets[node as usize + 1] as usize);
        &self.neighbours[a..b]
    }
}

/// Build the direct-leg adjacency: for every fix inside `ellipse` (every fix at all, where
/// `ellipse` is `None` — the last rung of [`super::STAGES`], where the search sees the whole
/// network), the nearest few fixes that are also inside it, within `max_nm`, and strictly
/// nearer `destination` than the fix itself is — a direct leg never goes backwards, which is
/// the rule that keeps this list short and every leg on it a genuine shortcut rather than a
/// detour dressed as one.
///
/// Every fix is read off `compact` directly — not only ones with an airway out of them,
/// because a mid-ocean reporting point with no published airway at all — exactly what a
/// North Atlantic crossing has instead of one — is precisely what a direct leg needs to land
/// on — and a plain pass over the whole network rather than a lookup keyed off a grid round
/// `origin` alone. A circle round one focus was tried first, on the reasoning that any point
/// inside the ellipse is, in particular, no further than the ellipse's own limit from that
/// focus; it is a correct superset, but a loose one on a long-haul route, where a thin
/// ellipse is still tens of thousands of miles-squared and the circle needed to be sure of
/// catching all of it is barely smaller than the network itself — worse than the plain pass
/// it was meant to save, once the grid lookups and the sort it also pays for are counted.
/// Reading every fix's position directly costs a fixed sum over the whole network, the same
/// whether the two ends are two hundred miles apart or ten thousand.
pub fn build(compact: &Compact, ellipse: Option<&Ellipse>, destination: LatLon, max_nm: f64) -> Directs {
    let n = compact.node_count();
    let inside = |p: LatLon| ellipse.is_none_or(|e| e.contains(p));

    let mut grid: Grid<u32> = Grid::new(CELL_DEG);
    for node in 0..n as u32 {
        let pos = compact.pos(node);
        if inside(pos) {
            grid.insert(pos, node);
        }
    }

    // Every fix inside the ellipse is offered direct legs, well-connected ones included.
    // Only offering them to sparsely connected fixes — on the reasoning that a fix with
    // three airways out of it is not the sparse spot a direct leg exists for — was tried and
    // measured: it is about a quarter faster on a long-haul route and worse on every route
    // it changes at all. London to Rome went from eight per cent over the great circle to
    // fourteen, Mumbai to Delhi from ten to twenty-three, and London to Dubai — the route
    // the filter was introduced to improve — from fifteen per cent to twenty-one. A
    // well-connected fix is often exactly where a route should leave the airways, because
    // the airways leaving it go somewhere else.
    let mut rows: Vec<Vec<u32>> = vec![Vec::new(); n];
    for node in 0..n as u32 {
        let pos = compact.pos(node);
        if !inside(pos) {
            continue;
        }
        let own_dist = distance_nm(pos, destination);
        let mut nearby: Vec<(u32, f64)> = grid
            .near(pos, max_nm)
            .into_iter()
            .filter_map(|(i, d)| {
                let (&candidate, cpos) = grid.get(i);
                if candidate == node || distance_nm(cpos, destination) >= own_dist {
                    return None;
                }
                Some((candidate, d))
            })
            .collect();
        nearby.sort_by(|a, b| a.1.total_cmp(&b.1));
        nearby.truncate(MAX_NEIGHBOURS);
        rows[node as usize] = nearby.into_iter().map(|(i, _)| i).collect();
    }

    let mut offsets = vec![0u32; n + 1];
    for i in 0..n {
        offsets[i + 1] = offsets[i] + rows[i].len() as u32;
    }
    let mut neighbours = Vec::with_capacity(offsets[n] as usize);
    for row in &rows {
        neighbours.extend_from_slice(row);
    }
    Directs { offsets, neighbours }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::graph::Graph;

    /// A line of fixes running from the origin towards the destination, plus one well off
    /// to the side (the same distance along, but a long way across): every fix should offer
    /// only its neighbours further towards the destination, and the off-line fix should
    /// never appear as anyone's neighbour once the ellipse is thin.
    fn line_with_a_stray() -> (Graph, LatLon, LatLon) {
        let mut g = Graph::default();
        for i in 0..8 {
            let x = i as f64;
            g.add_fixes([(format!("F{i}"), (0.0, x))]);
        }
        g.add_fixes([("STRAY".to_string(), (30.0, 3.5))]);
        (g, (0.0, -0.2), (0.0, 7.2))
    }

    #[test]
    fn a_direct_leg_never_goes_backwards() {
        let (g, o, d) = line_with_a_stray();
        let compact = g.compact();
        let ellipse = Ellipse::new(o, d, 5.0, 500.0); // fat enough to hold everything.
        let directs = build(&compact, Some(&ellipse), d, 1000.0);
        for node in 0..compact.node_count() as u32 {
            let own = distance_nm(compact.pos(node), d);
            for &nb in directs.out(node) {
                let nb_dist = distance_nm(compact.pos(nb), d);
                assert!(nb_dist < own, "a direct leg from {node} to {nb} did not move closer to the destination ({nb_dist} vs {own})");
            }
        }
    }

    #[test]
    fn a_fix_outside_the_ellipse_is_never_offered_or_offered_to() {
        let (g, o, d) = line_with_a_stray();
        let compact = g.compact();
        let stray = g.find("STRAY", (30.0, 3.5)).unwrap();
        // Thin enough that the stray fix, thirty degrees off the direct line, sits well
        // outside it.
        let ellipse = Ellipse::new(o, d, 1.05, 50.0);
        assert!(!ellipse.contains(compact.pos(stray)));
        let directs = build(&compact, Some(&ellipse), d, 1000.0);
        assert!(directs.out(stray).is_empty(), "a fix outside the ellipse should have no direct-leg neighbours of its own");
        for node in 0..compact.node_count() as u32 {
            assert!(!directs.out(node).contains(&stray), "the stray fix outside the ellipse was offered as a direct-leg neighbour");
        }
    }

    #[test]
    fn nothing_further_than_max_nm_is_offered() {
        let (g, o, d) = line_with_a_stray();
        let compact = g.compact();
        let ellipse = Ellipse::new(o, d, 5.0, 500.0);
        // Each fix here is exactly one degree (about 60 nm) from the next: a maximum leg
        // shorter than that should leave every fix with no neighbours at all.
        let directs = build(&compact, Some(&ellipse), d, 30.0);
        for node in 0..compact.node_count() as u32 {
            for &nb in directs.out(node) {
                assert!(distance_nm(compact.pos(node), compact.pos(nb)) <= 30.0 + 1e-6);
            }
        }
    }
}
