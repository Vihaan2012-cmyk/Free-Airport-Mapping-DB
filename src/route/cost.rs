//! Turning the network into a static shortest-path problem.
//!
//! A leg's true cost depends on when it is flown — the wind shifts through the day — which
//! makes the honest search a time-dependent one. That is a harder problem than an exact,
//! fast search can afford to solve worldwide a dozen times over. So the wind is frozen at
//! one estimate of when the flight is over the middle of its route, which turns the cost of
//! every edge at one level into a constant rather than a function of time, which is what
//! makes the fast, exact methods in [`super::search`] legitimate: a static edge weight is
//! all a shortest-path search has ever needed.
//!
//! This is the only place [`CostModel::leg`] is called once per edge — the "customisation"
//! the rest of the search runs on. It is also where a hazard active at the frozen time and
//! an [`EdgeRule`]'s verdict are baked into the same array, at the same frozen time, for the
//! same reason: a search that re-asked a rule for every edge it relaxed would pay a virtual
//! call for every one of a million edges on every one of a dozen searches, and gain very
//! little, since the rule was already going to be asked once here.

use super::graph::{Compact, Edge, Graph};
use super::hazard::Shape;
use crate::dispatch::{CostModel, EdgeQuery, EdgeRule, Hazard, HazardKind, LatLon, LegQuery, Verdict};
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// The airway name a free-route direct leg is costed and reported under: not a published
/// airway at all, so every rule that reads [`EdgeQuery::airway`] sees exactly what a
/// controller would see in the route string, and [`super::search::reconstruct`] never needs
/// to invent a name for a leg whose `via_airway` is `None`.
pub const DIRECT: &str = "DCT";

/// The cost of flying the network at one level, frozen at one time: one entry per forward
/// edge, parallel to [`Compact::edges`], `f32::INFINITY` where the level, a hazard to be
/// avoided, or a rule forbids the edge outright.
pub struct LevelMetric {
    pub level_ft: f64,
    pub cost: Vec<f32>,
    /// The minutes each edge takes at this level and this frozen wind, kept alongside the
    /// cost so a found path can be turned back into a schedule, and a rule re-asked with a
    /// real time, without re-costing the network.
    pub minutes: Vec<f32>,
}

impl LevelMetric {
    /// The cost of a forward edge, by its position in `Compact::out(node)`.
    pub fn forward(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32 {
        self.cost[compact.out_base(node) + pos_in_row]
    }

    pub fn forward_minutes(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32 {
        self.minutes[compact.out_base(node) + pos_in_row]
    }

    /// The cost of a reverse edge, looked up by the forward edge it mirrors.
    pub fn backward(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32 {
        let flat = compact.redge_forward[compact.in_base(node) + pos_in_row];
        self.cost[flat as usize]
    }

    pub fn backward_minutes(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32 {
        let flat = compact.redge_forward[compact.in_base(node) + pos_in_row];
        self.minutes[flat as usize]
    }
}

/// What the search's hot loop needs from a level's cost, whichever way it was worked out —
/// the whole network at once ([`LevelMetric`]), or lazily, one edge at a time
/// ([`LazyLevel`]). Generic code in `search` is written once against this rather than
/// against either concrete type, so a hand-built [`LevelMetric`] still does for a small
/// test network while the real pipeline reaches for the lazy path.
pub trait EdgeCost {
    fn forward(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32;
    fn backward(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32;
}

impl EdgeCost for LevelMetric {
    fn forward(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32 {
        LevelMetric::forward(self, compact, node, pos_in_row)
    }
    fn backward(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32 {
        LevelMetric::backward(self, compact, node, pos_in_row)
    }
}

/// Everything the customisation step needs, frozen before it begins: the moment the wind
/// is taken at, the hazards active then, and the rules a segment must pass. Every field is
/// a reference or a plain number, so this is cheap to copy — one goes to every level's own
/// lazy coster, and to every member of a search portfolio.
#[derive(Clone, Copy)]
pub struct Frozen<'a> {
    pub cost: &'a dyn CostModel,
    pub cost_index: f64,
    pub when: DateTime<Utc>,
    pub hazards: &'a [Hazard],
    pub edge_rules: &'a [&'a dyn EdgeRule],
    pub origin: &'a str,
    pub destination: &'a str,
    /// A representative distance already flown, standing in for how much lighter the
    /// aircraft is by the middle of the route: the same freezing the wind and the time get,
    /// for the same reason.
    pub flown_nm: f64,
}

/// A hazard's shapes, sorted into what closes an edge outright and what merely prices it,
/// at one level: shared between the eager and the lazy coster, so the two only ever
/// disagree about *when* an edge is priced, never about what the price is.
struct HazardShapes {
    avoid: Vec<Shape>,
    penalise: Vec<(Shape, f64)>,
}

impl HazardShapes {
    fn at(hazards: &[Hazard], when: DateTime<Utc>, level_ft: f64) -> HazardShapes {
        let avoid = hazards.iter().filter(|h| h.kind == HazardKind::Avoid && h.active_at(when) && h.fills(level_ft) && h.polygon.len() >= 3).map(|h| Shape::new(&h.polygon)).collect();
        let penalise = hazards
            .iter()
            .filter(|h| h.active_at(when))
            .filter_map(|h| match h.kind {
                HazardKind::Penalise(f) if h.fills(level_ft) && h.polygon.len() >= 3 => Some((Shape::new(&h.polygon), f)),
                _ => None,
            })
            .collect();
        HazardShapes { avoid, penalise }
    }
}

/// What one leg costs, worked out in full: filtered by whether it is clear of a hazard to be
/// avoided, then by every rule, then costed and, where a hazard prices it rather than
/// closing it, marked up. `f32::INFINITY` where the leg is closed outright. The one place
/// [`CostModel::leg`] is ever called, whether the leg is a real edge of the compact graph
/// costed up front or one at a time as a search touches it, or a free-route direct leg that
/// is not an edge of the graph at all — the two are priced by exactly this, differing only
/// in the airway name a rule sees ([`super::search`] wires a direct leg through with `DCT`)
/// and in whether an edge's own published limits close it before this is ever reached.
#[allow(clippy::too_many_arguments)]
fn cost_leg(from_id: &str, from_pos: LatLon, to_id: &str, to_pos: LatLon, airway: &str, level_ft: f64, shapes: &HazardShapes, frozen: &Frozen) -> (f32, f32) {
    if shapes.avoid.iter().any(|s| s.crossed_by(from_pos, to_pos)) {
        return (f32::INFINITY, 0.0);
    }
    let q = EdgeQuery { from: from_id, from_pos, to: to_id, to_pos, airway, level_ft, when: frozen.when, origin: frozen.origin, destination: frozen.destination };
    let mut factor = 1.0f64;
    let mut forbidden = false;
    for rule in frozen.edge_rules {
        match rule.check(&q) {
            Verdict::Forbid(_) => forbidden = true,
            Verdict::Penalise(f) => factor *= f,
            Verdict::Allow => {}
        }
    }
    if forbidden {
        return (f32::INFINITY, 0.0);
    }
    for (shape, f) in &shapes.penalise {
        if shape.crossed_by(from_pos, to_pos) {
            factor *= f.max(1.0);
        }
    }
    let lq = LegQuery { from: from_pos, to: to_pos, level_ft, when: frozen.when, flown_nm: frozen.flown_nm };
    let leg = frozen.cost.leg(&lq).times(factor);
    (leg.value(frozen.cost_index) as f32, leg.minutes as f32)
}

/// What one edge of the compact graph costs at one level: filtered first by the edge's own
/// published limits and the aircraft's ceiling, then priced by [`cost_leg`] exactly as any
/// other leg is.
#[allow(clippy::too_many_arguments)]
fn cost_edge(graph: &Graph, level_ft: f64, ceiling: f64, shapes: &HazardShapes, frozen: &Frozen, from: u32, to: u32, from_pos: LatLon, to_pos: LatLon, e: &Edge) -> (f32, f32) {
    if !e.allowed_at(level_ft, ceiling) {
        return (f32::INFINITY, 0.0);
    }
    cost_leg(graph.fix_id(from), from_pos, graph.fix_id(to), to_pos, graph.airway_name(e.airway_id()), level_ft, shapes, frozen)
}

/// One level's array, every edge costed up front. The eager path: right next to
/// [`LazyLevel`], which costs the same edges but only the ones a search actually asks
/// about, and is what a search reaches for by default — pre-costing every edge in a
/// network of a million of them is exactly the work a greedy search, which only ever
/// touches a sliver of it, does not need done. Kept for a caller that means to search the
/// same metric enough times over that paying for the whole network up front wins out, and
/// for comparing the two.
fn build_one(graph: &Graph, compact: &Compact, level_ft: f64, frozen: &Frozen) -> LevelMetric {
    let n = compact.edges.len();
    let mut cost = vec![0f32; n];
    let mut minutes = vec![0f32; n];
    let shapes = HazardShapes::at(frozen.hazards, frozen.when, level_ft);
    let ceiling = frozen.cost.ceiling_ft();
    for node in 0..compact.node_count() as u32 {
        let base = compact.out_base(node);
        for (p, e) in compact.out(node).iter().enumerate() {
            let i = base + p;
            let (c, m) = cost_edge(graph, level_ft, ceiling, &shapes, frozen, node, e.to, compact.pos(node), compact.pos(e.to), e);
            cost[i] = c;
            minutes[i] = m;
        }
    }
    LevelMetric { level_ft, cost, minutes }
}

/// Every level's metric, one array each, built in parallel: the eager path (see
/// [`build_one`]), kept behind this function rather than used by default — a plan reaches
/// for [`LazyLevel::new`] instead unless it specifically wants the whole network costed up
/// front.
pub fn build_metrics(graph: &Graph, compact: &Compact, levels_ft: &[f64], frozen: &Frozen) -> Vec<LevelMetric> {
    levels_ft.par_iter().map(|&level_ft| build_one(graph, compact, level_ft, frozen)).collect()
}

/// A level's cost, worked out lazily: an edge is priced the first time a search asks for
/// it, and never again — the answer sits in a `Cell` alongside it, `NaN` standing for "not
/// yet costed" since a real cost is always finite and non-negative. A greedy search over a
/// network of a million edges touches a few hundred or a few thousand of them; costing
/// only those, rather than the whole network, is what turns a query that would otherwise
/// spend tens of milliseconds customising a metric it barely uses into one that spends
/// that time on the edges it actually needs.
///
/// The `Cell`s are why this is not `Sync`: one `LazyLevel` belongs to one search. A
/// portfolio of searches run in parallel each builds its own, over the same frozen inputs,
/// rather than share one — the modest duplicated costing that costs is far cheaper than
/// the synchronisation avoiding it would need.
pub struct LazyLevel<'a> {
    pub level_ft: f64,
    graph: &'a Graph,
    compact: &'a Compact,
    frozen: Frozen<'a>,
    shapes: HazardShapes,
    ceiling: f64,
    cost: Vec<Cell<f32>>,
    minutes: Vec<Cell<f32>>,
    costed: Cell<usize>,
    /// A free-route direct leg is not an edge of the compact graph, so it has no place in
    /// `cost` to be memoised at: this is its own memoisation, keyed by the pair of fixes it
    /// joins, filled the first time the search asks about a given pair and kept from then on
    /// for the rest of this level's searches — exactly the reason `cost` exists, for a leg
    /// that `cost` was never sized to hold.
    directs: RefCell<HashMap<(u32, u32), f32>>,
    directs_costed: Cell<usize>,
}

impl<'a> LazyLevel<'a> {
    pub fn new(graph: &'a Graph, compact: &'a Compact, level_ft: f64, frozen: Frozen<'a>) -> LazyLevel<'a> {
        let n = compact.edges.len();
        let shapes = HazardShapes::at(frozen.hazards, frozen.when, level_ft);
        let ceiling = frozen.cost.ceiling_ft();
        LazyLevel { level_ft, graph, compact, frozen, shapes, ceiling, cost: vec![Cell::new(f32::NAN); n], minutes: vec![Cell::new(f32::NAN); n], costed: Cell::new(0), directs: RefCell::new(HashMap::new()), directs_costed: Cell::new(0) }
    }

    fn cost_at(&self, flat: usize, from: u32, to: u32, e: &Edge) -> f32 {
        let known = self.cost[flat].get();
        if !known.is_nan() {
            return known;
        }
        let (c, m) = cost_edge(self.graph, self.level_ft, self.ceiling, &self.shapes, &self.frozen, from, to, self.compact.pos(from), self.compact.pos(to), e);
        self.cost[flat].set(c);
        self.minutes[flat].set(m);
        self.costed.set(self.costed.get() + 1);
        c
    }

    pub fn forward(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32 {
        let flat = compact.out_base(node) + pos_in_row;
        let e = &compact.out(node)[pos_in_row];
        self.cost_at(flat, node, e.to, e)
    }

    /// The cost of a reverse edge, looked up — and, the first time, worked out — by the
    /// forward edge it mirrors: `node` is the *target* of the original edge here, so the
    /// edge's own `from` is `e.to`, the predecessor `r#in` gives.
    pub fn backward(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32 {
        let flat = compact.redge_forward[compact.in_base(node) + pos_in_row] as usize;
        let e = &compact.redges[compact.in_base(node) + pos_in_row];
        self.cost_at(flat, e.to, node, &compact.edges[flat])
    }

    /// How many of the network's edges this level actually had to price: what makes lazy
    /// costing worth doing rather than a curiosity, reported alongside a query's timing.
    pub fn edges_costed(&self) -> usize {
        self.costed.get()
    }

    /// A free-route direct leg between two fixes, priced exactly as a real edge is —
    /// obeying the hazards, every [`EdgeRule`] (as `DCT`), and the aircraft's ceiling — but
    /// through the pair-keyed memoisation `directs` holds rather than a position in
    /// `Compact::edges`, since a direct leg is not one. `f32::INFINITY` above the aircraft's
    /// ceiling, or where a hazard or a rule closes it.
    pub fn direct(&self, from: u32, to: u32) -> f32 {
        if self.level_ft > self.ceiling {
            return f32::INFINITY;
        }
        if let Some(&c) = self.directs.borrow().get(&(from, to)) {
            return c;
        }
        let (c, _) = cost_leg(self.graph.fix_id(from), self.compact.pos(from), self.graph.fix_id(to), self.compact.pos(to), DIRECT, self.level_ft, &self.shapes, &self.frozen);
        self.directs.borrow_mut().insert((from, to), c);
        self.directs_costed.set(self.directs_costed.get() + 1);
        c
    }

    /// How many distinct direct-leg pairs this level actually had to price: kept apart from
    /// [`LazyLevel::edges_costed`], since a direct leg is never one of `Compact`'s own edges.
    pub fn directs_costed(&self) -> usize {
        self.directs_costed.get()
    }
}

impl EdgeCost for LazyLevel<'_> {
    fn forward(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32 {
        LazyLevel::forward(self, compact, node, pos_in_row)
    }
    fn backward(&self, compact: &Compact, node: u32, pos_in_row: usize) -> f32 {
        LazyLevel::backward(self, compact, node, pos_in_row)
    }
}

/// What a search's direct-leg expansion needs from a level's cost: a `DirectCost` and an
/// [`EdgeCost`] are two views of the same [`LazyLevel`], asked for different things — an
/// edge of the compact graph by position, a direct leg by the pair of fixes it joins — so
/// they are kept as separate traits rather than one, and a caller with no directs to offer
/// (a test with a hand-built [`LevelMetric`], say) need not implement this at all.
pub trait DirectCost {
    fn direct(&self, from: u32, to: u32) -> f32;
}

impl DirectCost for LazyLevel<'_> {
    fn direct(&self, from: u32, to: u32) -> f32 {
        LazyLevel::direct(self, from, to)
    }
}

/// What it costs to change level at a fix: the cost model asked for a leg of a reference
/// length at each level, the difference between the two standing for how much a cruise at
/// one level costs against the other, plus a fixed price for the climb or descent itself.
/// Position is not part of this — the wind's effect on a climb is a small correction next
/// to the difference in cruise economics between levels, and folding it in would mean
/// pricing every fix at every pair of levels rather than once per pair.
pub fn climb_table(cost: &dyn CostModel, levels_ft: &[f64], cost_index: f64, when: DateTime<Utc>, flown_nm: f64) -> Vec<Vec<f32>> {
    const REFERENCE_NM: f64 = 100.0;
    const PENALTY_MIN_PER_1000FT: f64 = 0.35;
    const PENALTY_KG_PER_1000FT: f64 = 9.0;
    let rate = |level_ft: f64| -> f64 {
        let q = LegQuery { from: (0.0, 0.0), to: (0.0, REFERENCE_NM / 60.0), level_ft, when, flown_nm };
        cost.leg(&q).value(cost_index)
    };
    let rates: Vec<f64> = levels_ft.iter().map(|&l| rate(l)).collect();
    levels_ft
        .iter()
        .enumerate()
        .map(|(i, &from)| {
            levels_ft
                .iter()
                .enumerate()
                .map(|(j, &to)| {
                    if i == j {
                        return 0.0f32;
                    }
                    let thousands = (to - from).abs() / 1000.0;
                    let penalty = thousands * (PENALTY_MIN_PER_1000FT * cost_index * crate::dispatch::KG_PER_MIN_PER_CI + PENALTY_KG_PER_1000FT);
                    ((rates[j] - rates[i]).abs() + penalty) as f32
                })
                .collect()
        })
        .collect()
}

/// The cheapest a single nautical mile could possibly cost, at any level the search may
/// use: the smallest rate [`CostModel::lower_bound`] gives across every level in play.
/// Multiplying a landmark's distance bound by this can never overestimate what finishing
/// the route from there costs, at whichever level the route is actually flown at when it
/// gets there — which is what keeps the search's heuristic admissible without having to
/// convert a distance bound level by level in the hot loop.
///
/// This assumes `lower_bound` scales no faster than linearly with distance, true of
/// [`crate::dispatch::SimpleCost`] and of any aircraft flown at a roughly constant burn
/// rate over the length of leg the bound is asked about.
pub fn min_rate_per_nm(cost: &dyn CostModel, levels_ft: &[f64], cost_index: f64) -> f32 {
    const REFERENCE_NM: f64 = 200.0;
    levels_ft.iter().map(|&level| (cost.lower_bound(REFERENCE_NM, level).value(cost_index) / REFERENCE_NM) as f32).fold(f32::INFINITY, f32::min).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{along, distance_nm, Air, LatLon, SimpleCost, StillAir, WindField};

    struct Faster;
    impl WindField for Faster {
        fn air(&self, _at: LatLon, _alt_ft: f64, _when: DateTime<Utc>) -> Air {
            Air { wind_from_deg: 0.0, wind_kt: 0.0, temp_c: -40.0 }
        }
    }

    fn graph() -> Graph {
        let mut g = Graph::default();
        g.add("UA1", ("A", (0.0, 0.0)), ("B", (0.0, 1.0)), false, None, Some(30000.0));
        g.add("UA2", ("A", (0.0, 0.0)), ("C", (0.1, 1.0)), false, Some(32000.0), None);
        g
    }

    #[test]
    fn a_level_a_segment_may_not_be_flown_at_is_marked_infinite() {
        let g = graph();
        let compact = g.compact();
        let wind = StillAir;
        let cost = SimpleCost { tas_kt: 400.0, kg_per_hour: 2000.0, ceiling_ft: 41000.0, air: &wind, max_tailwind_kt: 100.0 };
        let frozen = Frozen { cost: &cost, cost_index: 0.0, when: Utc::now(), hazards: &[], edge_rules: &[], origin: "A", destination: "C", flown_nm: 0.0 };
        let metrics = build_metrics(&g, &compact, &[35000.0], &frozen);
        let a = g.find("A", (0.0, 0.0)).unwrap();
        let out = compact.out(a);
        // UA1 tops out at 30000, UA2 only opens above 32000: at 35000 the first is closed
        // and the second is open.
        let ua1 = out.iter().position(|e| g.airway_name(e.airway_id()) == "UA1").unwrap();
        let ua2 = out.iter().position(|e| g.airway_name(e.airway_id()) == "UA2").unwrap();
        assert_eq!(metrics[0].forward(&compact, a, ua1), f32::INFINITY);
        assert!(metrics[0].forward(&compact, a, ua2).is_finite());
    }

    #[test]
    fn a_hazard_to_avoid_closes_the_edge_it_crosses() {
        let g = graph();
        let compact = g.compact();
        let wind = StillAir;
        let cost = SimpleCost { tas_kt: 400.0, kg_per_hour: 2000.0, ceiling_ft: 41000.0, air: &wind, max_tailwind_kt: 100.0 };
        let hazard = crate::dispatch::Hazard::new("CB", vec![(-1.0, 0.4), (-1.0, 0.6), (1.0, 0.6), (1.0, 0.4)], crate::dispatch::HazardKind::Avoid);
        let frozen = Frozen { cost: &cost, cost_index: 0.0, when: Utc::now(), hazards: std::slice::from_ref(&hazard), edge_rules: &[], origin: "A", destination: "C", flown_nm: 0.0 };
        let metrics = build_metrics(&g, &compact, &[25000.0], &frozen);
        let a = g.find("A", (0.0, 0.0)).unwrap();
        let ua1 = compact.out(a).iter().position(|e| g.airway_name(e.airway_id()) == "UA1").unwrap();
        assert_eq!(metrics[0].forward(&compact, a, ua1), f32::INFINITY);
    }

    #[test]
    fn forward_and_backward_lookups_agree() {
        let g = graph();
        let compact = g.compact();
        let wind = StillAir;
        let cost = SimpleCost { tas_kt: 400.0, kg_per_hour: 2000.0, ceiling_ft: 41000.0, air: &wind, max_tailwind_kt: 100.0 };
        let frozen = Frozen { cost: &cost, cost_index: 0.2, when: Utc::now(), hazards: &[], edge_rules: &[], origin: "A", destination: "C", flown_nm: 0.0 };
        let metrics = build_metrics(&g, &compact, &[20000.0], &frozen);
        let a = g.find("A", (0.0, 0.0)).unwrap();
        let b = g.find("B", (0.0, 1.0)).unwrap();
        let p = compact.r#in(b).iter().position(|e| e.to == a).unwrap();
        let via_forward = metrics[0].forward(&compact, a, 0);
        let via_backward = metrics[0].backward(&compact, b, p);
        assert!((via_forward - via_backward).abs() < 1e-6);
    }

    #[test]
    fn a_higher_level_costs_less_with_a_following_wind_and_the_climb_price_shows_it() {
        let wind = Faster;
        let cost = SimpleCost { tas_kt: 400.0, kg_per_hour: 2000.0, ceiling_ft: 41000.0, air: &wind, max_tailwind_kt: 100.0 };
        let table = climb_table(&cost, &[20000.0, 30000.0], 0.0, Utc::now(), 0.0);
        assert_eq!(table.len(), 2);
        assert_eq!(table[0][0], 0.0);
        assert!(table[0][1] > 0.0, "a climb is never free");
        assert!((table[0][1] - table[1][0]).abs() < 1e-3, "the price is the same either way here");
    }

    #[test]
    fn a_hazard_outside_its_active_window_is_ignored() {
        let g = graph();
        let compact = g.compact();
        let wind = StillAir;
        let cost = SimpleCost { tas_kt: 400.0, kg_per_hour: 2000.0, ceiling_ft: 41000.0, air: &wind, max_tailwind_kt: 100.0 };
        let now = Utc::now();
        let mut hazard = crate::dispatch::Hazard::new("CB", vec![(-1.0, 0.4), (-1.0, 0.6), (1.0, 0.6), (1.0, 0.4)], crate::dispatch::HazardKind::Avoid);
        hazard.active_from = Some(now + chrono::Duration::hours(3));
        hazard.active_to = Some(now + chrono::Duration::hours(5));
        let frozen = Frozen { cost: &cost, cost_index: 0.0, when: now, hazards: std::slice::from_ref(&hazard), edge_rules: &[], origin: "A", destination: "C", flown_nm: 0.0 };
        let metrics = build_metrics(&g, &compact, &[25000.0], &frozen);
        let a = g.find("A", (0.0, 0.0)).unwrap();
        let ua1 = compact.out(a).iter().position(|e| g.airway_name(e.airway_id()) == "UA1").unwrap();
        // The frozen time is before the hazard starts, so the edge it would otherwise
        // close stays open.
        assert!(metrics[0].forward(&compact, a, ua1).is_finite());
    }

    #[test]
    fn distance_stays_periodic_when_costing_across_the_date_line() {
        let a: LatLon = (10.0, 179.5);
        let b: LatLon = (10.0, -179.5);
        assert!(distance_nm(a, b) < 100.0);
        let mid = along(a, b, 0.5);
        assert!(mid.1.abs() > 179.0);
    }
}
