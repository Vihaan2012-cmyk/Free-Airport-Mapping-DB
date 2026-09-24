//! A route from one airport to another over the airway network — fast, on a network with
//! some 10^5 fixes and 10^6 directed edges, and honest about what the speed costs.
//!
//! One flight plan may search the network a dozen times over: once a candidate level, a
//! few more when a rule turns a route down, a few more again for the next-best
//! alternative — and now, deliberately, several times over again for one level, since
//! [`Context::attempt`] runs a small portfolio of searches (a few widths of beam) and
//! keeps the cheapest. The pipeline is built in stages that each do their work once and
//! hand the next stage something cheap to use:
//!
//! 1. **[`graph`]** stores the network as compressed adjacency: flat arrays a search reads
//!    by index, built once behind [`Graph::shared`] and kept for the life of the process.
//! 2. **[`cost`]** turns a candidate level into a *static* shortest-path problem — the
//!    wind is frozen at an estimate of when the flight is over the middle of its route, so
//!    a leg's cost is a number rather than a function of when it is flown — and prices an
//!    edge only the first time a search actually asks about it ([`cost::LazyLevel`]),
//!    memoised from then on: pricing all million edges of a level before a search that
//!    touches a few hundred of them begins is exactly the work this avoids.
//! 3. **[`landmarks`]** builds the lower bound the search steers by: a handful of points
//!    spread round the network, each with its distance to and from every fix, cached by
//!    the navigation data's AIRAC cycle so the expensive part — Dijkstra from each of
//!    them — is paid for once a cycle rather than once a flight plan.
//! 4. **[`search`]** is greedy best-first over *(fix, level)* states, so that a route may
//!    climb or descend at any fix along it and the level and the way round are chosen
//!    together. It expands whichever state looks nearest the goal by the landmark
//!    estimate and never reconsiders one already settled, which is what makes it fast and
//!    what makes it approximate: nothing here guarantees the cheapest route there is, only
//!    a reasonable one found by touching a sliver of the network. [`search::refine`]
//!    straightens what greediness leaves crooked afterwards.
//! 5. **[`procedures`]** picks the SID and STAR that fit the runway in use, and
//!    **[`airports`]** answers where an airport is and what lies near one.
//! 6. This module ties the five together: [`plan_routes`] chooses runways, joins the SID's
//!    last fix to the STAR's first over the airways, applies every rule and hazard, and
//!    retries a route a rule turns down or that repeats one already found.

use crate::dispatch::{bearing_deg, distance_nm, sky, EdgeQuery, EdgeRule, FiledRoute, Hazard, HazardKind, LatLon, LegQuery, LevelScheme, PointKind, RouteRequest, Verdict, Waypoint};
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::Arc;

pub mod airports;
pub mod airspace;
pub mod cost;
pub mod directs;
pub mod bridge;
pub mod ellipse;
pub mod reference;
pub mod etops;
pub mod graph;
pub mod hazard;
pub mod landmarks;
pub mod oceanic;
pub mod ord32;
pub mod procedures;
pub mod rad;
pub mod search;
pub mod spatial;

pub use ellipse::Ellipse;
pub use graph::{Edge, Fix, Graph};
pub use hazard::{crosses, midpoint};

// ---------------------------------------------------------------------------------
// Tunables that `RouteRequest` has no room for: sensible constants rather than more
// configuration, documented here so a change is a one-line edit rather than a search.
// ---------------------------------------------------------------------------------

/// The longest a direct leg onto or off the airways may be: from the SID's last fix to the
/// network, and from the network to the STAR's first fix. Also the longest a free-route
/// direct leg between two enroute fixes may be over land, where free-route airspace is
/// generally good for a few hundred miles at a time.
const MAX_DIRECT_NM: f64 = 220.0;
/// How far the refinement pass may straighten a route in one leg. Longer than a join, because
/// a join is a leg onto the network from an aerodrome while this is a leg between two fixes the
/// route already flies over — and because the rules are now asked whether it is allowed, which
/// is what used to hold it down to the same figure.
const REFINE_MAX_NM: f64 = 600.0;
/// The longest a free-route direct leg may be over open ocean, tried only once nothing
/// shorter connects at all: there is nothing to fly between out there but the oceanic
/// reporting points a track message once joined, spaced widely enough that
/// [`MAX_DIRECT_NM`] alone could not chain them into a crossing.
const OCEAN_MAX_DIRECT_NM: f64 = 600.0;
/// The floor beneath a percentage-of-direct ellipse: see [`ellipse::Ellipse`]'s own doc
/// comment for why a percentage alone is too thin on a short hop.
const ELLIPSE_SLACK_NM: f64 = 120.0;
/// How many landmarks the ALT bound is built from: inside the 16–32 the method is usually
/// described with.
const LANDMARK_COUNT: usize = 24;
/// The widths of beam the search portfolio tries, per level: narrow and fast, wide and
/// better, run together since there is no telling in advance which wins on a given
/// network. The widest is also what the one joint, every-level-together search uses.
const BEAM_WIDTHS: [usize; 3] = [64, 256, 1024];
/// How many candidate levels are kept when the request leaves the choice to the search:
/// enough to give a real choice, few enough that the state space stays small.
const MAX_CANDIDATE_LEVELS: usize = 12;
/// A published SID/STAR altitude window is worth very little without knowing the aircraft's
/// own vertical profile, so cruise levels start clear of terminal airspace rather than at
/// the ground.
const LEVEL_FLOOR_FT: f64 = 10_000.0;
const TAXI_MINUTES: i64 = 15;
const RULE_RETRIES: usize = 3;
/// How many of the nearest fixes are considered when looking for ones a route may join the
/// network at: wide, because the nearest are usually terminal fixes on the low-level network
/// and the first usable one can be a long way down the list.
const JOIN_POOL: usize = 512;

// ---------------------------------------------------------------------------------
// Cruise levels and runways.
// ---------------------------------------------------------------------------------

/// Which countries fly the semicircular rule reversed. Nothing in the navigation database
/// says so — it is a fact of each country's AIP, not of ARINC 424 — so this is the list
/// `dispatch::LevelScheme`'s own doc comment gives (France, Italy, Portugal) and no other:
/// no field of the navigation data encodes it, and guessing at the "few others" from the
/// data on this machine would be no more than a guess.
const SOUTH_ODD_PREFIXES: [&str; 3] = ["LF", "LI", "LP"];

fn scheme_for(origin_icao: &str, destination_icao: &str) -> LevelScheme {
    let reversed = |icao: &str| SOUTH_ODD_PREFIXES.iter().any(|p| icao.starts_with(p));
    if reversed(origin_icao) || reversed(destination_icao) {
        LevelScheme::SouthOdd
    } else {
        LevelScheme::EastOdd
    }
}

/// The levels a route search may consider, when the request leaves the choice to it: every
/// level the semicircular rule allows on the initial track, narrowed to a manageable
/// number nearest the level asked for (or a typical long-haul cruise, absent even that).
fn candidate_levels(req: &RouteRequest) -> Vec<f64> {
    if !req.levels_ft.is_empty() {
        return req.levels_ft.to_vec();
    }
    let scheme = scheme_for(&req.origin.icao, &req.destination.icao);
    let track = bearing_deg(req.origin.pos, req.destination.pos);
    let mut levels = crate::dispatch::levels_for(track, LEVEL_FLOOR_FT, req.cost.ceiling_ft(), req.rvsm, scheme);
    if levels.len() > MAX_CANDIDATE_LEVELS {
        let about = if req.cruise_ft > 0.0 { req.cruise_ft } else { 35_000.0 };
        levels.sort_by(|a, b| (a - about).abs().total_cmp(&(b - about).abs()));
        levels.truncate(MAX_CANDIDATE_LEVELS);
    }
    levels.sort_by(|a, b| a.total_cmp(b));
    levels
}

/// The runway in use: given outright, else the one with the best headwind component, else
/// the longest. The wind here is taken to be true, for consistency with [`dispatch::Air`];
/// a runway's own bearing is compared to it directly, without a magnetic variation figure
/// this database does not cheaply carry per runway — over the handful of degrees variation
/// usually runs, that changes which end has the better headwind only very rarely.
fn choose_runway(given: Option<&str>, wind: Option<(f64, f64)>, runways: &[crate::sources::navdata::RunwayRow]) -> Option<String> {
    if let Some(g) = given {
        return Some(g.trim().to_uppercase());
    }
    if runways.is_empty() {
        return None;
    }
    if let Some((from_deg, kt)) = wind {
        if kt > 0.0 {
            let best = runways.iter().filter_map(|r| r.bearing_true_deg.map(|b| (r, (from_deg - b).to_radians().cos()))).max_by(|(_, a), (_, b)| a.total_cmp(b));
            if let Some((r, _)) = best {
                return Some(r.ident.clone());
            }
        }
    }
    runways.iter().max_by(|a, b| a.length_ft.total_cmp(&b.length_ft)).map(|r| r.ident.clone())
}

// ---------------------------------------------------------------------------------
// Airspace: the request's own hazards, plus the flight information regions it says to
// avoid and the restricted areas the navigation database publishes along the way.
// ---------------------------------------------------------------------------------

/// The hazards a route is planned round: what the request already carries, plus every
/// flight information region it names to avoid and the restricted, prohibited, danger,
/// warning, military and training areas the navigation database publishes near the direct
/// line between two places. Prohibited and restricted areas are kept out of outright;
/// danger, warning, military and training areas are flown through at a fixed price, since
/// whether one is truly active is a NOTAM this planner does not see.
pub fn with_airspace(mut hazards: Vec<Hazard>, avoid_firs: &[String], a: LatLon, b: LatLon, corridor_nm: f64) -> Vec<Hazard> {
    for region in crate::sources::navdata::regions(avoid_firs) {
        for part in region.parts {
            hazards.push(Hazard { name: format!("{} FIR {}", region.ident, region.name), polygon: part, base_ft: 0.0, top_ft: sky(), kind: HazardKind::Avoid, active_from: None, active_to: None, source: "navigation database".to_string() });
        }
    }
    let margin = corridor_nm / 60.0;
    let cos = ((a.0 + b.0) / 2.0).to_radians().cos().max(0.2);
    let (south, north) = ((a.0.min(b.0) - margin).max(-90.0), (a.0.max(b.0) + margin).min(90.0));
    let (west, east) = (a.1.min(b.1) - margin / cos, a.1.max(b.1) + margin / cos);
    for area in crate::sources::navdata::restricted_areas(south, north, west, east) {
        let kind = match area.kind {
            'P' | 'R' => HazardKind::Avoid,
            'D' | 'W' | 'M' | 'T' | 'A' => HazardKind::Penalise(1.15),
            _ => continue,
        };
        hazards.push(Hazard { name: format!("{} {}", area.designation, area.name), polygon: area.boundary, base_ft: area.lower_ft, top_ft: area.upper_ft, kind, active_from: None, active_to: None, source: "navigation database".to_string() });
    }
    hazards
}

// ---------------------------------------------------------------------------------
// A rule the search invents for itself: penalise an edge a route rule named, or an edge
// belonging to a route already found, and search again.
// ---------------------------------------------------------------------------------

#[derive(Default)]
struct Nudge {
    fixes: HashSet<String>,
    airways: HashSet<String>,
}

impl Nudge {
    fn is_empty(&self) -> bool {
        self.fixes.is_empty() && self.airways.is_empty()
    }

    fn absorb_route(&mut self, route: &FiledRoute) {
        for w in &route.points {
            if matches!(w.kind, PointKind::Enroute | PointKind::Track) {
                self.fixes.insert(w.ident.clone());
                if !w.via.is_empty() && w.via != "DCT" {
                    self.airways.insert(w.via.clone());
                }
            }
        }
    }
}

impl EdgeRule for Nudge {
    fn name(&self) -> &str {
        "search retry"
    }

    /// A hard `Forbid`, not a price: greedy best-first orders its open list on the
    /// landmark estimate alone, so a `Penalise`'d edge costs more once found but is not
    /// looked at any differently while the search is choosing what to expand next — a
    /// soft nudge has no teeth against a search that never weighs what it has spent.
    /// Taking the edge out of the network entirely is the only way to actually send a
    /// retry, or the next of the best few routes, a different way round.
    fn check(&self, q: &EdgeQuery) -> Verdict {
        if self.fixes.contains(q.from) || self.fixes.contains(q.to) || self.airways.contains(q.airway) {
            Verdict::Forbid("search retry".into())
        } else {
            Verdict::Allow
        }
    }
}

// ---------------------------------------------------------------------------------
// Joining the airports onto the network.
// ---------------------------------------------------------------------------------

/// How many fixes each end of the route may join the airways at: a SID or an airport
/// rarely has only one within reach, and giving the search a single, forced choice is what
/// leaves a rule or a retry with nowhere else to send it.
const ENTRY_CANDIDATES: usize = 8;

/// One rung of [`STAGES`]: how wide a join, how fat an ellipse (`None` for no ellipse at
/// all), and how long a free-route direct leg may be.
struct Stage {
    join_width: usize,
    ellipse_factor: Option<f64>,
    direct_max_nm: f64,
}

/// The single escalation [`plan_routes`] climbs when a narrower attempt finds nothing: wider
/// join pools and a fatter ellipse together, not two ladders climbed apart. They serve the
/// same end — trying harder once the easy case has failed — and a join widened on its own
/// would seed the search from fixes a still-thin ellipse then prunes straight back out,
/// while an ellipse widened on its own would offer a search still joined too narrowly
/// somewhere new it can never reach. Only at the last two rungs does the ellipse go
/// altogether and the direct leg lengthen to what an ocean crossing needs — tried last and
/// only because trying it always would let a route stretch a direct leg across a gap real
/// free-route airspace would never allow, where a narrower rung already had an answer.
/// The ellipse each rung of [`STAGES`] confines the search to, widest last, and the slack every
/// one of them is drawn with: what a picture of the search has to draw to show where it was
/// allowed to look.
pub fn stage_ellipses() -> (Vec<f64>, f64) {
    (STAGES.iter().filter_map(|s| s.ellipse_factor).collect(), ELLIPSE_SLACK_NM)
}

const STAGES: [Stage; 5] = [
    Stage { join_width: ENTRY_CANDIDATES, ellipse_factor: Some(1.08), direct_max_nm: MAX_DIRECT_NM },
    Stage { join_width: 24, ellipse_factor: Some(1.25), direct_max_nm: MAX_DIRECT_NM },
    Stage { join_width: 64, ellipse_factor: Some(1.6), direct_max_nm: MAX_DIRECT_NM },
    Stage { join_width: 64, ellipse_factor: None, direct_max_nm: MAX_DIRECT_NM },
    Stage { join_width: 64, ellipse_factor: None, direct_max_nm: OCEAN_MAX_DIRECT_NM },
];

/// The fixes a SID's last point (or an airport with none) may join the network at: the
/// fix already in the network at exactly this identifier and place, if there is one —
/// preferred so a SID that ends on a named airway fix joins it by identity — followed by
/// the nearest fixes with a segment out of them, within reach of a direct leg.
/// The fixes a route may join the airway network at, or leave it at: the procedure's own
/// end if the network has it, then the nearest fixes to it.
///
/// `usable` is what keeps this honest. Ten thousand of the airway segments in a navigation
/// database top out below eighteen thousand feet: they are the low-level network, and at a
/// cruise level they are rightly refused. A fix whose every airway is one of those is no use
/// as a place to join the network at a cruise level — a search starting there is stranded on
/// the low network with no way up to the one it means to fly, and a destination the network
/// plainly connects is never reached. So a fix is taken only if at least one airway out of it
/// (or into it, at the arrival end) may be flown at one of the levels in play, and the search
/// widens its net until it finds enough of them. Only if nothing at all qualifies does it
/// fall back to the nearest fixes regardless, since a poor join beats no route.
fn join_candidates(graph: &Graph, ident: Option<&str>, pos: LatLon, want: usize, usable: &dyn Fn(u32) -> bool) -> Vec<u32> {
    let mut out = Vec::with_capacity(want);
    if let Some(id) = ident {
        if let Some(node) = graph.find(id, pos) {
            if usable(node) {
                out.push(node);
            }
        }
    }
    // A wide pool, because near a busy airport the nearest fixes are all terminal ones on
    // the low-level network: at Rome the nearest fix usable at a cruise level is
    // eighty-eight miles out, well inside the direct leg allowed but a long way down a list
    // ordered by distance. Sorting a few hundred candidates once a plan costs nothing
    // against failing to find a route at all.
    for (node, _) in graph.nearest(pos, JOIN_POOL, MAX_DIRECT_NM) {
        if out.len() >= want {
            break;
        }
        if usable(node) && !out.contains(&node) {
            out.push(node);
        }
    }
    if out.is_empty() {
        for (node, _) in graph.nearest(pos, want, MAX_DIRECT_NM) {
            if !out.contains(&node) {
                out.push(node);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------
// The search itself, one attempt: one set of levels, one set of extra hazards and rules on
// top of the request's own. Returns the cheapest `FiledRoute` a portfolio of greedy
// searches, run in parallel, finds.
// ---------------------------------------------------------------------------------

/// One member of the portfolio [`Context::attempt`] searches: a single level on its own at
/// a beam width, or every level together, able to climb, at the widest one.
#[derive(Debug, Clone, Copy)]
enum Job {
    Single(usize, usize),
    Joint(usize),
}

/// What a query actually cost, for the timings this crate's speed is measured by:
/// how many of the network's edges the winning portfolio member (and, since every member
/// runs, the portfolio as a whole) had to price, and how many states the winner expanded.
#[derive(Debug, Clone, Copy, Default)]
pub struct AttemptStats {
    pub edges_costed: usize,
    pub nodes_expanded: usize,
    /// The winning portfolio member's own cost, before refinement: what a ratio against
    /// the true optimum is measured against.
    pub cost: f32,
}

struct Context<'a> {
    graph: &'a Graph,
    compact: Arc<graph::Compact>,
    req: &'a RouteRequest<'a>,
    levels_ft: Vec<f64>,
    dep_runway: Option<String>,
    arr_runway: Option<String>,
    sid: Option<procedures::Procedure>,
    star: Option<procedures::Procedure>,
    entry_pos: LatLon,
    exit_pos: LatLon,
    entry_candidates: Vec<u32>,
    exit_candidates: Vec<u32>,
    frozen_when: DateTime<Utc>,
    flown_nm: f64,
    rate_per_nm: f32,
    landmarks: Arc<landmarks::Landmarks>,
    /// The region worth searching at all, where this attempt uses one: `None` at the last
    /// rungs of [`STAGES`], where the search sees the whole network.
    ellipse: Option<Ellipse>,
    /// This attempt's free-route direct legs, built once from `ellipse`: `None` where
    /// `direct_max_nm` was never given, which is every test that has no business paying for
    /// a feature it is not exercising.
    directs: Option<directs::Directs>,
}

impl<'a> Context<'a> {
    /// The real pipeline: runways and procedures asked of the navigation database on this
    /// machine.
    #[allow(clippy::too_many_arguments)]
    fn build(graph: &'a Graph, req: &'a RouteRequest<'a>, join_width: usize, ellipse_factor: Option<f64>, direct_max_nm: Option<f64>) -> anyhow::Result<Context<'a>> {
        let dep_runway = {
            let runways = crate::sources::navdata::runways(&req.origin.icao);
            choose_runway(req.dep_runway.as_deref(), req.origin_wind, &runways)
        };
        let arr_runway = {
            let runways = crate::sources::navdata::runways(&req.destination.icao);
            choose_runway(req.arr_runway.as_deref(), req.destination_wind, &runways)
        };
        let sid = dep_runway.as_deref().and_then(|rw| procedures::sid_for_runway(&req.origin.icao, rw, req.destination.pos, graph));
        let star = arr_runway.as_deref().and_then(|rw| procedures::star_for_runway(&req.destination.icao, rw, req.origin.pos, graph));
        Context::build_with(graph, req, dep_runway, arr_runway, sid, star, join_width, ellipse_factor, direct_max_nm)
    }

    /// The pipeline from a runway and a procedure already in hand: what the real
    /// [`Context::build`] delegates to once it has asked the navigation database, and what
    /// a test that has no database to ask, and no business waiting on one, calls directly
    /// with fixtures of its own.
    ///
    /// `ellipse_factor` is `None` for no ellipse at all — the whole network is fair game,
    /// which is both the last rung of [`STAGES`] and every existing test's own fixture
    /// network, small enough that pruning it was never the point. `direct_max_nm` is `None`
    /// to build no free-route direct legs at all, `Some` for the longest one this attempt
    /// allows.
    #[allow(clippy::too_many_arguments)]
    fn build_with(graph: &'a Graph, req: &'a RouteRequest<'a>, dep_runway: Option<String>, arr_runway: Option<String>, sid: Option<procedures::Procedure>, star: Option<procedures::Procedure>, join_width: usize, ellipse_factor: Option<f64>, direct_max_nm: Option<f64>) -> anyhow::Result<Context<'a>> {
        let compact = graph.compact();
        let levels_ft = candidate_levels(req);
        if levels_ft.is_empty() {
            anyhow::bail!("no usable cruise level on this track");
        }

        let entry_pos = sid.as_ref().and_then(|s| s.points.last()).map(|w| w.pos).unwrap_or(req.origin.pos);
        let entry_ident = sid.as_ref().and_then(|s| s.points.last()).map(|w| w.ident.clone());
        let exit_pos = star.as_ref().and_then(|s| s.points.first()).map(|w| w.pos).unwrap_or(req.destination.pos);
        let exit_ident = star.as_ref().and_then(|s| s.points.first()).map(|w| w.ident.clone());

        // A fix is worth joining the network at only if something can be flown out of it
        // (or into it) at one of the levels this route is being planned for.
        let ceiling = req.cost.ceiling_ft();
        let out_usable = |node: u32| compact.out(node).iter().any(|e| levels_ft.iter().any(|&l| e.allowed_at(l, ceiling)));
        let in_usable = |node: u32| compact.r#in(node).iter().any(|e| levels_ft.iter().any(|&l| e.allowed_at(l, ceiling)));
        let entry_candidates = join_candidates(graph, entry_ident.as_deref(), entry_pos, join_width, &out_usable);
        let exit_candidates = join_candidates(graph, exit_ident.as_deref(), exit_pos, join_width, &in_usable);
        if entry_candidates.is_empty() {
            anyhow::bail!("no airway network within reach of the departure");
        }
        if exit_candidates.is_empty() {
            anyhow::bail!("no airway network within reach of the destination");
        }

        // Freeze the wind at an estimate of when the flight is over the middle of the
        // enroute portion, from a first pass of the cost model itself over the direct
        // line — good enough to pick a moment, not meant to be flown.
        let taxi = chrono::Duration::minutes(TAXI_MINUTES);
        let mid_level = levels_ft[levels_ft.len() / 2];
        let probe = req.cost.leg(&LegQuery { from: entry_pos, to: exit_pos, level_ft: mid_level, when: req.off_block + taxi, flown_nm: 0.0 });
        let frozen_when = req.off_block + taxi + chrono::Duration::seconds((probe.minutes * 30.0).round() as i64);
        let flown_nm = distance_nm(entry_pos, exit_pos) / 2.0;

        let rate_per_nm = cost::min_rate_per_nm(req.cost, &levels_ft, req.cost_index);
        let landmarks = landmarks::tables(&compact, LANDMARK_COUNT.min(compact.node_count().max(1)));

        let ellipse = ellipse_factor.map(|k| Ellipse::new(req.origin.pos, req.destination.pos, k, ELLIPSE_SLACK_NM));
        let directs = direct_max_nm.map(|max_nm| directs::build(&compact, ellipse.as_ref(), req.destination.pos, max_nm));

        Ok(Context { graph, compact, req, levels_ft, dep_runway, arr_runway, sid, star, entry_pos, exit_pos, entry_candidates, exit_candidates, frozen_when, flown_nm, rate_per_nm, landmarks, ellipse, directs })
    }

    fn bias(&self, from: LatLon, to: LatLon) -> Vec<f32> {
        self.levels_ft.iter().map(|&level_ft| self.req.cost.leg(&LegQuery { from, to, level_ft, when: self.frozen_when, flown_nm: 0.0 }).value(self.req.cost_index) as f32).collect()
    }

    /// The candidate entry fixes, each with its own per-level bias: the direct leg's cost
    /// from wherever the SID leaves off differs fix by fix, so the bias cannot be shared
    /// between them the way it could when there was only ever one candidate.
    fn entry_bias(&self) -> Vec<(u32, Vec<f32>)> {
        self.entry_candidates.iter().map(|&node| (node, self.bias(self.entry_pos, self.graph.fix_pos(node)))).collect()
    }

    fn exit_bias(&self) -> Vec<(u32, Vec<f32>)> {
        self.exit_candidates.iter().map(|&node| (node, self.bias(self.graph.fix_pos(node), self.exit_pos))).collect()
    }

    /// One attempt: customise the metric with the request's rules and hazards plus
    /// whatever this attempt adds on top, search a portfolio in parallel, refine and
    /// assemble the cheapest into a `FiledRoute`. `None` when nothing connects the SID to
    /// the STAR under these hazards and rules.
    fn attempt(&self, extra_hazards: &[Hazard], nudge: &Nudge) -> Option<(FiledRoute, AttemptStats)> {
        let mut hazards: Vec<Hazard> = self.req.hazards.to_vec();
        hazards.extend_from_slice(extra_hazards);
        let mut rules: Vec<&dyn EdgeRule> = self.req.edge_rules.to_vec();
        if !nudge.is_empty() {
            rules.push(nudge);
        }
        let frozen = cost::Frozen { cost: self.req.cost, cost_index: self.req.cost_index, when: self.frozen_when, hazards: &hazards, edge_rules: &rules, origin: &self.req.origin.icao, destination: &self.req.destination.icao, flown_nm: self.flown_nm };
        let climb = cost::climb_table(self.req.cost, &self.levels_ft, self.req.cost_index, self.frozen_when, self.flown_nm);
        let entry_bias = self.entry_bias();
        let exit_bias = self.exit_bias();

        // The portfolio: every candidate level searched on its own at a few widths of
        // beam — a narrow one fast, a wide one better, and there is no telling in advance
        // which wins on a given network — plus one search over every level together,
        // able to climb or descend, at the widest beam, so the joint state the module's
        // own doc comment describes is still one of the routes actually tried. Every
        // member costs only the edges it touches, and every member runs on its own
        // `LazyLevel`, since the memoisation inside one belongs to one search.
        let widest = *BEAM_WIDTHS.iter().max().unwrap();
        let jobs: Vec<Job> = (0..self.levels_ft.len()).flat_map(|li| BEAM_WIDTHS.iter().map(move |&beam| Job::Single(li, beam))).chain(std::iter::once(Job::Joint(widest))).collect();

        use rayon::prelude::*;
        let results: Vec<Option<(search::Found, Job, usize)>> = jobs
            .into_par_iter()
            .map(|job| {
                let mut scratch = search::Scratch::new();
                match job {
                    Job::Single(li, beam) => {
                        let level_ft = self.levels_ft[li];
                        let lazy = cost::LazyLevel::new(self.graph, &self.compact, level_ft, frozen);
                        let single_climb = vec![vec![0.0f32]];
                        let starts: Vec<(u32, &[f32])> = entry_bias.iter().map(|(n, b)| (*n, &b[li..=li])).collect();
                        let goals: Vec<(u32, &[f32])> = exit_bias.iter().map(|(n, b)| (*n, &b[li..=li])).collect();
                        let one = std::slice::from_ref(&lazy);
                        let lazy_dc: &dyn cost::DirectCost = &lazy;
                        let direct_ctx = self.directs.as_ref().map(|adj| search::DirectContext { adj, cost: std::slice::from_ref(&lazy_dc) });
                        search::greedy_best_first(&self.compact, one, direct_ctx.as_ref(), &single_climb, &starts, &goals, self.ellipse.as_ref(), Some(&self.landmarks), self.rate_per_nm, beam, &mut scratch)
                            .map(|found| (found, job, lazy.edges_costed() + lazy.directs_costed()))
                    }
                    Job::Joint(beam) => {
                        let lazies: Vec<cost::LazyLevel> = self.levels_ft.iter().map(|&level_ft| cost::LazyLevel::new(self.graph, &self.compact, level_ft, frozen)).collect();
                        let starts: Vec<(u32, &[f32])> = entry_bias.iter().map(|(n, b)| (*n, b.as_slice())).collect();
                        let goals: Vec<(u32, &[f32])> = exit_bias.iter().map(|(n, b)| (*n, b.as_slice())).collect();
                        let lazy_dcs: Vec<&dyn cost::DirectCost> = lazies.iter().map(|l| l as &dyn cost::DirectCost).collect();
                        let direct_ctx = self.directs.as_ref().map(|adj| search::DirectContext { adj, cost: &lazy_dcs });
                        search::greedy_best_first(&self.compact, &lazies, direct_ctx.as_ref(), &climb, &starts, &goals, self.ellipse.as_ref(), Some(&self.landmarks), self.rate_per_nm, beam, &mut scratch)
                            .map(|found| (found, job, lazies.iter().map(|l| l.edges_costed() + l.directs_costed()).sum()))
                    }
                }
            })
            .collect();

        let costed_total: usize = results.iter().flatten().map(|(_, _, c)| c).sum();
        let (found, job, _) = results.into_iter().flatten().min_by(|a, b| a.0.cost.total_cmp(&b.0.cost))?;
        let (expanded, winning_cost) = (found.expanded, found.cost);

        let level_of: Box<dyn Fn(u8) -> f64> = match job {
            Job::Single(li, _) => {
                let level_ft = self.levels_ft[li];
                Box::new(move |_| level_ft)
            }
            Job::Joint(_) => {
                let levels_ft = self.levels_ft.clone();
                Box::new(move |idx: u8| levels_ft[idx as usize])
            }
        };
        // With the rules asked, a shortcut can be allowed to run further than a join does:
        // the reason it was held to the same short limit was that a long one might redraw a
        // forbidden airway as a direct leg, and that is now checked rather than guarded against.
        let ident_of = |node: u32| self.graph.fix_id(node).to_string();
        let refined = search::refine(
            &self.compact,
            &found.steps,
            level_of.as_ref(),
            self.req.cost,
            self.req.cost_index,
            self.frozen_when,
            &hazards,
            REFINE_MAX_NM,
            &rules,
            (&self.req.origin.icao, &self.req.destination.icao),
            &ident_of,
        );
        let route = self.assemble(refined, level_of.as_ref());
        Some((route, AttemptStats { edges_costed: costed_total, nodes_expanded: expanded, cost: winning_cost }))
    }

    fn assemble(&self, steps: Vec<search::Step>, level_of: &dyn Fn(u8) -> f64) -> FiledRoute {
        let mut enroute: Vec<Waypoint> = Vec::new();
        let mut prev_node: Option<u32> = None;
        let mut cruise_ft = None;
        for step in &steps {
            if cruise_ft.is_none() {
                cruise_ft = Some(level_of(step.level_idx));
            }
            if prev_node == Some(step.fix) {
                continue;
            }
            if let Some(last) = enroute.last_mut() {
                last.via = step.via_airway.map(|id| self.graph.airway_name(id).to_string()).unwrap_or_else(|| "DCT".to_string());
            }
            let ident = self.graph.fix_id(step.fix).to_string();
            let pos = self.graph.fix_pos(step.fix);
            enroute.push(Waypoint::new(ident, pos, "", PointKind::Enroute));
            prev_node = Some(step.fix);
        }

        let mut points = Vec::new();
        points.push(Waypoint::new(self.req.origin.icao.clone(), self.req.origin.pos, self.dep_runway.clone().unwrap_or_default(), PointKind::Airport));
        let sid_name = self.sid.as_ref().map(|s| s.ident.clone());
        if let Some(sid) = &self.sid {
            let mut sid_points = sid.points.clone();
            // Where the SID's own last fix is also the fix the search joined the airway
            // network at — by identity, per `join_candidates` — it is one fix flown once,
            // not two printed back to back with nothing between. The join keeps the
            // network's own copy, since that is the one whose `via` already says what is
            // flown out of it, but carries across whatever the SID published there.
            if let (Some(last), Some(first)) = (sid_points.last(), enroute.first_mut()) {
                if same_fix(last, first) {
                    first.alt_min_ft = first.alt_min_ft.or(last.alt_min_ft);
                    first.alt_max_ft = first.alt_max_ft.or(last.alt_max_ft);
                    first.speed_max_kt = first.speed_max_kt.or(last.speed_max_kt);
                    sid_points.pop();
                }
            }
            points.extend(sid_points);
        }
        points.extend(enroute);
        let star_name = self.star.as_ref().map(|s| s.ident.clone());
        if let Some(star) = &self.star {
            let mut star_points = star.points.clone();
            if let (Some(last), Some(first)) = (points.last_mut(), star_points.first()) {
                if same_fix(last, first) {
                    last.alt_min_ft = last.alt_min_ft.or(first.alt_min_ft);
                    last.alt_max_ft = last.alt_max_ft.or(first.alt_max_ft);
                    last.speed_max_kt = last.speed_max_kt.or(first.speed_max_kt);
                    star_points.remove(0);
                }
            }
            points.extend(star_points);
        }
        points.push(Waypoint::new(self.req.destination.icao.clone(), self.req.destination.pos, String::new(), PointKind::Airport));

        FiledRoute {
            origin: self.req.origin.clone(),
            destination: self.req.destination.clone(),
            dep_runway: self.dep_runway.clone(),
            sid: sid_name,
            sid_transition: self.sid.as_ref().and_then(|s| s.transition.clone()),
            star: star_name,
            star_transition: self.star.as_ref().and_then(|s| s.transition.clone()),
            arr_runway: self.arr_runway.clone(),
            approach: None,
            points,
            cruise_ft: cruise_ft.unwrap_or(self.levels_ft[0]),
            off_block: self.req.off_block,
        }
    }
}

/// Plan a filed route: the procedures at each end, and the cheapest way between them over
/// the airway network, obeying every rule and hazard in the request.
pub fn plan_route(graph: &Graph, req: &RouteRequest) -> anyhow::Result<FiledRoute> {
    plan_routes(graph, req, 1)?.into_iter().next().ok_or_else(|| anyhow::anyhow!("no route"))
}

/// The best few routes, cheapest first, made to differ from one another: after a route is
/// found, [`RouteRule`]s judge it as a whole, and a violation sends the search back with
/// what it named marked up, up to three times; a route already accepted has its own fixes
/// and airways marked up the same way before the next one is searched for, so the second
/// and third routes are genuine alternatives rather than the same one again.
pub fn plan_routes(graph: &Graph, req: &RouteRequest, most: usize) -> anyhow::Result<Vec<FiledRoute>> {
    if most == 0 {
        return Ok(Vec::new());
    }
    // A narrow join and a thin ellipse first, widening only if nothing connects. Near a
    // busy airport the nearest fixes are terminal ones on the low-level network, and the
    // first fix usable at a cruise level — and in the same part of the upper network as the
    // other end — can be eighty miles out and a long way down a list ordered by distance.
    // Widening always would find those routes, but at a cost: every extra candidate seeds
    // the search with another start state, diluting the beam and making the ordinary short
    // flight's route worse, and a fatter ellipse offers the search fixes a shorter one would
    // have kept it from ever considering. So both escalate together, only when a narrower
    // rung fails outright: see [`STAGES`]'s own doc comment for why they are one ladder.
    let mut last: anyhow::Error = anyhow::anyhow!("no route");
    for stage in STAGES {
        match Context::build(graph, req, stage.join_width, stage.ellipse_factor, Some(stage.direct_max_nm)).and_then(|ctx| plan_from_context(&ctx, req, most)) {
            Ok(found) if !found.is_empty() => return Ok(found),
            Ok(_) => last = anyhow::anyhow!("no route"),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// [`plan_routes`] from a runway and a procedure already resolved: the seam a test with a
/// fixture network and no navigation database to ask uses, so its timing and its outcome
/// depend only on the network it built, never on what happens to be installed on the
/// machine running it.
#[cfg(test)]
fn plan_routes_with(graph: &Graph, req: &RouteRequest, most: usize, dep_runway: Option<String>, arr_runway: Option<String>, sid: Option<procedures::Procedure>, star: Option<procedures::Procedure>) -> anyhow::Result<Vec<FiledRoute>> {
    if most == 0 {
        return Ok(Vec::new());
    }
    // No ellipse, no direct legs: every existing test built its network to exercise the
    // search itself, on fixtures far too small for pruning to be the point.
    let ctx = Context::build_with(graph, req, dep_runway, arr_runway, sid, star, ENTRY_CANDIDATES, None, None)?;
    plan_from_context(&ctx, req, most)
}

#[cfg(test)]
fn plan_route_with(graph: &Graph, req: &RouteRequest, dep_runway: Option<String>, arr_runway: Option<String>, sid: Option<procedures::Procedure>, star: Option<procedures::Procedure>) -> anyhow::Result<FiledRoute> {
    plan_routes_with(graph, req, 1, dep_runway, arr_runway, sid, star)?.into_iter().next().ok_or_else(|| anyhow::anyhow!("no route"))
}

/// Whether two waypoints are the same published fix: the same name at, near enough for
/// floating point, the same place. What [`Context::assemble`] uses to tell a SID or STAR
/// boundary joined to the airway network by identity from one merely close to it.
fn same_fix(a: &Waypoint, b: &Waypoint) -> bool {
    a.ident == b.ident && (a.pos.0 - b.pos.0).abs() < 1e-6 && (a.pos.1 - b.pos.1).abs() < 1e-6
}

fn plan_from_context(ctx: &Context, req: &RouteRequest, most: usize) -> anyhow::Result<Vec<FiledRoute>> {
    let mut accepted: Vec<FiledRoute> = Vec::new();
    let mut diversity = Nudge::default();

    for _ in 0..most {
        let mut route = None;
        let mut retry = Nudge::default();
        for _ in 0..=RULE_RETRIES {
            let combined = Nudge { fixes: diversity.fixes.union(&retry.fixes).cloned().collect(), airways: diversity.airways.union(&retry.airways).cloned().collect() };
            let Some((candidate, _stats)) = ctx.attempt(&[], &combined) else { break };
            let violations: Vec<_> = req.route_rules.iter().flat_map(|r| r.check_route(&candidate)).collect();
            if violations.is_empty() {
                route = Some(candidate);
                break;
            }
            for v in &violations {
                if let Some(at) = &v.at {
                    retry.fixes.insert(at.clone());
                    retry.airways.insert(at.clone());
                }
            }
            route = Some(candidate); // kept in case every retry still violates something.
        }
        let Some(route) = route else { break };
        diversity.absorb_route(&route);
        accepted.push(route);
    }

    if accepted.is_empty() {
        anyhow::bail!("no route connects {} to {} under the rules and hazards given", req.origin.icao, req.destination.icao);
    }
    accepted.sort_by(|a, b| a.distance_nm().total_cmp(&b.distance_nm()));
    Ok(accepted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{Air, Airport, CostModel, LegCost, RouteRule, SimpleCost, StillAir, WindField};
    use chrono::Utc;

    fn airport(icao: &str, pos: LatLon) -> Airport {
        Airport { icao: icao.to_string(), name: String::new(), pos, elevation_ft: 0.0 }
    }

    /// A ladder network like `search`'s tests, but as a full `Graph` a `RouteRequest` can
    /// be planned over: north (N) and south (S) airways, joined at each rung, between two
    /// entry fixes near the "airports".
    fn ladder() -> Graph {
        let mut g = Graph::default();
        for i in 0..6 {
            let x = i as f64;
            if i > 0 {
                g.add("UN1", (&format!("N{}", i - 1), (1.0, x - 1.0)), (&format!("N{i}"), (1.0, x)), false, None, None);
                g.add("US2", (&format!("S{}", i - 1), (-1.0, x - 1.0)), (&format!("S{i}"), (-1.0, x)), false, None, None);
            }
            g.add("UR9", (&format!("N{i}"), (1.0, x)), (&format!("S{i}"), (-1.0, x)), false, None, None);
        }
        g
    }

    fn simple_cost(air: &dyn WindField) -> SimpleCost<'_> {
        SimpleCost { tas_kt: 450.0, kg_per_hour: 2200.0, ceiling_ft: 41000.0, air, max_tailwind_kt: 120.0 }
    }

    fn base_request<'a>(origin: &'a Airport, destination: &'a Airport, cost: &'a dyn CostModel, air: &'a dyn WindField) -> RouteRequest<'a> {
        RouteRequest {
            origin,
            destination,
            cruise_ft: 0.0,
            levels_ft: &[],
            cost,
            cost_index: 20.0,
            off_block: Utc::now(),
            air,
            hazards: &[],
            edge_rules: &[],
            route_rules: &[],
            dep_runway: None,
            arr_runway: None,
            origin_wind: None,
            destination_wind: None,
            rvsm: true,
            avoid_firs: &[],
            free_route: true,
        }
    }

    #[test]
    fn a_route_is_found_between_two_places_off_the_network() {
        let g = ladder();
        let o = airport("WEST", (0.0, -0.8));
        let d = airport("EAST", (0.0, 5.8));
        let air = StillAir;
        let cost = simple_cost(&air);
        let mut req = base_request(&o, &d, &cost, &air);
        req.levels_ft = &[35000.0];
        let route = plan_route_with(&g, &req, None, None, None, None).expect("a route");
        assert_eq!(route.origin.icao, "WEST");
        assert_eq!(route.destination.icao, "EAST");
        assert!(route.points.first().unwrap().kind == PointKind::Airport);
        assert!(route.points.last().unwrap().kind == PointKind::Airport);
        assert!(route.distance_nm() > 300.0);
        assert!((route.cruise_ft - 35000.0).abs() < 1.0);
    }

    #[test]
    fn a_hazard_on_one_airway_sends_the_route_down_the_other() {
        let g = ladder();
        let o = airport("WEST", (0.0, -0.8));
        let d = airport("EAST", (0.0, 5.8));
        let air = StillAir;
        let cost = simple_cost(&air);
        let mut req = base_request(&o, &d, &cost, &air);
        req.levels_ft = &[35000.0];
        let hazard = Hazard { name: "CB".into(), polygon: vec![(1.5, 2.2), (1.5, 2.8), (0.4, 2.8), (0.4, 2.2)], base_ft: 0.0, top_ft: 45000.0, kind: HazardKind::Avoid, active_from: None, active_to: None, source: String::new() };
        req.hazards = std::slice::from_ref(&hazard);
        let route = plan_route_with(&g, &req, None, None, None, None).expect("a route round it");
        assert!(route.points.iter().all(|w| w.ident != "N2" && w.ident != "N3"), "{}", route.route_string());
        assert!(route.points.iter().any(|w| w.ident.starts_with('S')));
    }

    /// A rule that forbids the southern airway only above FL300: at FL200 the route
    /// should still be free to use it, at FL360 it should not.
    struct HighOnly;
    impl EdgeRule for HighOnly {
        fn name(&self) -> &str {
            "high only"
        }
        fn check(&self, q: &EdgeQuery) -> Verdict {
            if q.airway == "US2" && q.level_ft > 30000.0 {
                Verdict::Forbid("test".into())
            } else {
                Verdict::Allow
            }
        }
    }

    /// A strong tailwind along the southern airway only, so that at any level the rule
    /// leaves it open, the cheapest route actually prefers it rather than merely being
    /// allowed to use it.
    struct SouthTailwind;
    impl WindField for SouthTailwind {
        fn air(&self, at: LatLon, _alt_ft: f64, _when: DateTime<Utc>) -> Air {
            if at.0 < 0.0 {
                Air { wind_from_deg: 270.0, wind_kt: 150.0, temp_c: -30.0 }
            } else {
                Air { wind_from_deg: 90.0, wind_kt: 40.0, temp_c: -30.0 }
            }
        }
    }

    #[test]
    fn a_rule_that_forbids_one_level_only_is_honoured_per_level() {
        let g = ladder();
        let o = airport("WEST", (0.0, -0.8));
        let d = airport("EAST", (0.0, 5.8));
        let air = SouthTailwind;
        let cost = simple_cost(&air);
        let rule = HighOnly;
        let rule_ref: &dyn EdgeRule = &rule;
        let mut low = base_request(&o, &d, &cost, &air);
        low.levels_ft = &[20000.0];
        low.edge_rules = std::slice::from_ref(&rule_ref);
        let low_route = plan_route_with(&g, &low, None, None, None, None).unwrap();
        assert!(low_route.points.iter().any(|w| w.ident.starts_with('S') && w.kind == PointKind::Enroute), "{}", low_route.route_string());

        let mut high = base_request(&o, &d, &cost, &air);
        high.levels_ft = &[36000.0];
        high.edge_rules = std::slice::from_ref(&rule_ref);
        let high_route = plan_route_with(&g, &high, None, None, None, None).unwrap();
        assert!(high_route.points.iter().all(|w| !w.ident.starts_with('S') || w.kind != PointKind::Enroute), "{}", high_route.route_string());
    }

    /// A route rule that turns down whichever route uses the northern airway: the search
    /// should retry and come back by the southern one.
    struct NoNorth;
    impl RouteRule for NoNorth {
        fn name(&self) -> &str {
            "no north"
        }
        fn check_route(&self, route: &FiledRoute) -> Vec<crate::dispatch::Violation> {
            route
                .points
                .iter()
                .filter(|w| w.ident.starts_with('N') && w.kind == PointKind::Enroute)
                .map(|w| crate::dispatch::Violation { rule: "no north".into(), at: Some(w.ident.clone()), message: "not today".into() })
                .collect()
        }
    }

    #[test]
    fn a_route_rule_violation_sends_the_search_back() {
        let g = ladder();
        let o = airport("WEST", (0.0, -0.8));
        let d = airport("EAST", (0.0, 5.8));
        let air = StillAir;
        let cost = simple_cost(&air);
        let rule = NoNorth;
        let rule_ref: &dyn RouteRule = &rule;
        let mut req = base_request(&o, &d, &cost, &air);
        req.levels_ft = &[35000.0];
        req.route_rules = std::slice::from_ref(&rule_ref);
        let route = plan_route_with(&g, &req, None, None, None, None).expect("a route that avoids the north");
        assert!(route.points.iter().all(|w| !(w.ident.starts_with('N') && w.kind == PointKind::Enroute)), "{}", route.route_string());
    }

    /// A SID whose own last fix is written under the same name and place as a network fix
    /// ("N0") joins it by identity. `Context::assemble` is exercised directly, with the
    /// path it is given fixed by hand, rather than through the search: the search is free
    /// to pick any entry candidate whose *own* direct leg turns out cheapest overall — that
    /// is what it is for — so a test of the merge itself must not depend on which one it
    /// happens to prefer on a given network. The assembled route should carry the SID's
    /// name, print "N0" exactly once rather than the SID's own copy immediately followed by
    /// the network's, and keep the constraint the SID published there.
    #[test]
    fn a_sid_joins_the_network_by_identity_without_printing_the_join_fix_twice() {
        let g = ladder();
        let o = airport("WEST", (1.0, -0.6));
        let d = airport("EAST", (0.0, 5.8));
        let air = StillAir;
        let cost = simple_cost(&air);
        let mut req = base_request(&o, &d, &cost, &air);
        req.levels_ft = &[35000.0];
        let mut joined = Waypoint::new("N0", (1.0, 0.0), "DEP1A", PointKind::Sid);
        joined.alt_min_ft = Some(3000.0);
        let sid = procedures::Procedure {
            ident: "DEP1A".to_string(),
            transition: None,
            points: vec![Waypoint::new("RW09", (1.0, -0.05), "DEP1A", PointKind::Sid), joined],
        };
        let ctx = Context::build_with(&g, &req, Some("09".to_string()), None, Some(sid), None, ENTRY_CANDIDATES, None, None).expect("a context");
        let n0 = g.find("N0", (1.0, 0.0)).expect("N0 is a network fix");
        let n1 = g.find("N1", (1.0, 1.0)).expect("N1 is a network fix");
        let steps = vec![search::Step { fix: n0, level_idx: 0, via_airway: None }, search::Step { fix: n1, level_idx: 0, via_airway: None }];
        let route = ctx.assemble(steps, &|_| 35000.0);
        assert_eq!(route.sid.as_deref(), Some("DEP1A"));
        let n0_points: Vec<&Waypoint> = route.points.iter().filter(|w| w.ident == "N0").collect();
        assert_eq!(n0_points.len(), 1, "{}", route.route_string());
        assert_eq!(n0_points[0].kind, PointKind::Enroute);
        assert_eq!(n0_points[0].alt_min_ft, Some(3000.0));
    }

    /// A STAR whose own first fix is not known to the network at all cannot be merged with
    /// anything — `same_fix` never matches a fix by a different name in a different place —
    /// so both it and whatever enroute fix the search actually lands on beforehand should
    /// still be printed, in order, each once. Which fix that is is the search's business,
    /// not this test's, so nothing here names one.
    #[test]
    fn a_star_with_no_network_fix_at_its_entry_is_not_merged_away() {
        let g = ladder();
        let o = airport("WEST", (0.0, -0.8));
        let d = airport("EAST", (1.0, 5.5));
        let air = StillAir;
        let cost = simple_cost(&air);
        let mut req = base_request(&o, &d, &cost, &air);
        req.levels_ft = &[35000.0];
        let star = procedures::Procedure {
            ident: "ARR1A".to_string(),
            transition: None,
            points: vec![Waypoint::new("STARX", (1.3, 5.6), "ARR1A", PointKind::Star), Waypoint::new("RW27", (1.0, 5.75), "ARR1A", PointKind::Star)],
        };
        let route = plan_route_with(&g, &req, None, Some("27".to_string()), None, Some(star)).expect("a route");
        assert_eq!(route.star.as_deref(), Some("ARR1A"));
        let star_at = route.points.iter().position(|w| w.ident == "STARX" && w.kind == PointKind::Star).expect("STARX present once, as a STAR fix");
        assert!(star_at > 0, "{}", route.route_string());
        let before = &route.points[star_at - 1];
        assert_eq!(before.kind, PointKind::Enroute, "{}", route.route_string());
        assert_ne!(before.ident, "STARX");
    }

    /// A rule that forbids one named airway outright, at any level: used below to force a
    /// route onto a fix that would otherwise never be worth the search's while, so a test
    /// can tell whether the ellipse — not the cost — is what kept a fix off the route.
    /// A rule that forbids an airway — and the line it occupies, however it is filed.
    ///
    /// The second half matters. A rule that only matched the airway's *name* would let the
    /// refinement pass redraw exactly the same two points as an unnamed direct leg and call it
    /// an improvement, which is the fault `search::refine` now guards against: a real
    /// restriction keeps an aeroplane off a line, not off a name.
    struct ForbidAirway(&'static str);
    impl ForbidAirway {
        /// Whether a leg runs between the same two places the forbidden airway joins, within a
        /// mile either end.
        fn same_line(q: &EdgeQuery) -> bool {
            // The two ends of `UDIR` in `direct_and_a_far_detour`.
            const FORBIDDEN: [(f64, f64); 2] = [(0.0, 0.0), (0.0, 8.0)];
            let near = |a: LatLon, b: LatLon| distance_nm(a, b) < 60.0;
            (near(q.from_pos, FORBIDDEN[0]) && near(q.to_pos, FORBIDDEN[1])) || (near(q.from_pos, FORBIDDEN[1]) && near(q.to_pos, FORBIDDEN[0]))
        }
    }
    impl EdgeRule for ForbidAirway {
        fn name(&self) -> &str {
            "forbid airway"
        }
        fn check(&self, q: &EdgeQuery) -> Verdict {
            if q.airway == self.0 || (q.airway == "DCT" && ForbidAirway::same_line(q)) {
                Verdict::Forbid("test".into())
            } else {
                Verdict::Allow
            }
        }
    }

    /// Two ways from `WEST` to `EAST`: a single direct hop, and a single fix a long way off
    /// it — `FAR`, ten degrees of latitude clear of the direct line — that is the only other
    /// way across once `UDIR` is forbidden. Each real edge here is already longer than
    /// `MAX_DIRECT_NM`, so a free-route direct leg can never stand in for either one: what
    /// the tests below see is the ellipse alone, not a direct leg quietly bridging the gap.
    fn direct_and_a_far_detour() -> (Graph, Airport, Airport) {
        let mut g = Graph::default();
        g.add("UDIR", ("E0", (0.0, 0.0)), ("E4", (0.0, 8.0)), false, None, None);
        g.add("UFAR", ("E0", (0.0, 0.0)), ("FAR", (10.0, 4.0)), false, None, None);
        g.add("UFAR", ("FAR", (10.0, 4.0)), ("E4", (0.0, 8.0)), false, None, None);
        let o = airport("WEST", (0.0, -0.3));
        let d = airport("EAST", (0.0, 8.3));
        (g, o, d)
    }

    #[test]
    fn a_fix_outside_the_ellipse_is_never_on_the_route() {
        let (g, o, d) = direct_and_a_far_detour();
        let air = StillAir;
        let cost = simple_cost(&air);
        let forbid = ForbidAirway("UDIR");
        let forbid_ref: &dyn EdgeRule = &forbid;
        let mut req = base_request(&o, &d, &cost, &air);
        req.levels_ft = &[35000.0];
        req.edge_rules = std::slice::from_ref(&forbid_ref);

        // Thin enough that FAR sits well outside it: with the direct line forbidden,
        // nothing at all should connect, which is the strongest proof a pruned fix never
        // appears on the route — there is no other way across for it to appear on.
        let thin = Context::build_with(&g, &req, None, None, None, None, ENTRY_CANDIDATES, Some(1.1), Some(MAX_DIRECT_NM)).expect("a context");
        assert!(thin.attempt(&[], &Nudge::default()).is_none(), "a thin ellipse should have pruned the only way across");

        // The same network and the same forbidden airway, now with an ellipse fat enough to
        // hold FAR: the detour is the only way across, so it must be found, and found using
        // exactly the fix the thin ellipse pruned.
        let fat = Context::build_with(&g, &req, None, None, None, None, ENTRY_CANDIDATES, Some(5.0), Some(MAX_DIRECT_NM)).expect("a context");
        let (route, _) = fat.attempt(&[], &Nudge::default()).expect("a route once the ellipse is fat enough to hold the detour");
        assert!(route.points.iter().any(|w| w.ident == "FAR"), "{}", route.route_string());
    }

    #[test]
    fn the_ellipse_ladder_widens_when_a_thin_one_finds_nothing() {
        let (g, o, d) = direct_and_a_far_detour();
        let air = StillAir;
        let cost = simple_cost(&air);
        let forbid = ForbidAirway("UDIR");
        let forbid_ref: &dyn EdgeRule = &forbid;
        let mut req = base_request(&o, &d, &cost, &air);
        req.levels_ft = &[35000.0];
        req.edge_rules = std::slice::from_ref(&forbid_ref);

        // Every ellipsed rung of `STAGES` is too thin to hold FAR — only the un-ellipsed
        // rungs at the end can — so the full pipeline must climb the whole ladder to find
        // anything at all, exactly the escalation `plan_routes` promises when a narrow
        // attempt finds nothing.
        let route = plan_route(&g, &req).expect("plan_routes should widen all the way to no ellipse at all");
        assert!(route.points.iter().any(|w| w.ident == "FAR"), "{}", route.route_string());
    }

    #[test]
    fn the_best_few_routes_differ_from_one_another() {
        let g = ladder();
        let o = airport("WEST", (0.0, -0.8));
        let d = airport("EAST", (0.0, 5.8));
        let air = StillAir;
        let cost = simple_cost(&air);
        let mut req = base_request(&o, &d, &cost, &air);
        req.levels_ft = &[35000.0];
        let routes = plan_routes_with(&g, &req, 2, None, None, None, None).unwrap();
        assert_eq!(routes.len(), 2);
        let strings: Vec<String> = routes.iter().map(|r| r.route_string()).collect();
        assert_ne!(strings[0], strings[1], "{strings:?}");
    }

    /// Two levels of the same network, one that flies straight but low with a headwind,
    /// one that must detour round a hazard but flies high with a tailwind so strong it
    /// still wins: the cheapest combination is not the cheapest level searched alone.
    struct WindsAloft;
    impl WindField for WindsAloft {
        fn air(&self, at: LatLon, alt_ft: f64, _when: DateTime<Utc>) -> Air {
            if alt_ft > 30000.0 {
                Air { wind_from_deg: 270.0, wind_kt: 220.0, temp_c: -50.0 }
            } else {
                let _ = at;
                Air { wind_from_deg: 90.0, wind_kt: 40.0, temp_c: -20.0 }
            }
        }
    }

    #[test]
    fn the_level_and_the_route_are_chosen_together() {
        let mut g = Graph::default();
        // A short, direct low airway, and a longer high one that must dogleg round a
        // hazard placed only in the low band, over it.
        g.add("LOW", ("A", (0.0, 0.0)), ("B", (0.0, 4.0)), false, None, Some(28000.0));
        g.add("HIGH", ("A", (0.0, 0.0)), ("C", (1.0, 2.0)), false, Some(30000.0), None);
        g.add("HIGH", ("C", (1.0, 2.0)), ("B", (0.0, 4.0)), false, Some(30000.0), None);
        let o = airport("ORIG", (0.0, -0.1));
        let d = airport("DEST", (0.0, 4.1));
        let air = WindsAloft;
        let cost = simple_cost(&air);
        let mut req = base_request(&o, &d, &cost, &air);
        req.levels_ft = &[20000.0, 34000.0];
        req.cost_index = 0.0;
        let route = plan_route_with(&g, &req, None, None, None, None).expect("a route");
        assert!((route.cruise_ft - 34000.0).abs() < 1.0, "expected the high, longer, faster way; got {}", route.cruise_ft);
    }

    #[test]
    fn a_direct_leg_across_the_date_line_is_short() {
        let mut g = Graph::default();
        // A degree either side of 180°: about 59 nm the short way, and about 21,300 nm the
        // wrong way round the earth — the scale a longitude-averaging bug would produce.
        g.add("PAC", ("PACFX", (10.0, 179.5)), ("PACFY", (10.0, -179.5)), false, None, None);
        let o = airport("WPAC", (10.0, 179.3));
        let d = airport("EPAC", (10.0, -179.3));
        let air = StillAir;
        let cost = simple_cost(&air);
        let mut req = base_request(&o, &d, &cost, &air);
        req.levels_ft = &[35000.0];
        let route = plan_route_with(&g, &req, None, None, None, None).expect("a route across the date line");
        assert!(route.distance_nm() < 150.0, "{}", route.distance_nm());
    }

    #[test]
    fn levels_default_when_none_are_given() {
        let o = airport("A", (51.0, 0.0));
        let d = airport("B", (48.0, 2.0));
        let air = StillAir;
        let cost = simple_cost(&air);
        let req = base_request(&o, &d, &cost, &air);
        let levels = candidate_levels(&req);
        assert!(!levels.is_empty());
        assert!(levels.len() <= MAX_CANDIDATE_LEVELS);
    }

    #[test]
    fn south_odd_applies_only_where_the_data_gives_no_better_answer() {
        assert_eq!(scheme_for("LFPG", "EGLL"), LevelScheme::SouthOdd);
        assert_eq!(scheme_for("LIRF", "LGAV"), LevelScheme::SouthOdd);
        assert_eq!(scheme_for("LPPT", "GCLP"), LevelScheme::SouthOdd);
        assert_eq!(scheme_for("EGLL", "KJFK"), LevelScheme::EastOdd);
    }

    #[test]
    fn runway_choice_prefers_the_given_then_the_headwind_then_the_longest() {
        use crate::sources::navdata::RunwayRow;
        let runways = vec![
            RunwayRow { icao: "TEST".into(), ident: "09".into(), length_ft: 8000.0, bearing_true_deg: Some(90.0), lat: 0.0, lon: 0.0 },
            RunwayRow { icao: "TEST".into(), ident: "27".into(), length_ft: 12000.0, bearing_true_deg: Some(270.0), lat: 0.0, lon: 0.0 },
        ];
        assert_eq!(choose_runway(Some("09"), Some((270.0, 15.0)), &runways), Some("09".into()));
        assert_eq!(choose_runway(None, Some((270.0, 15.0)), &runways), Some("27".into()));
        assert_eq!(choose_runway(None, Some((90.0, 15.0)), &runways), Some("09".into()));
        assert_eq!(choose_runway(None, None, &runways), Some("27".into()), "longest, absent any wind");
    }

    #[test]
    fn leg_cost_value_folds_fuel_and_time() {
        let a = LegCost { minutes: 60.0, fuel_kg: 1000.0 };
        assert!((a.value(0.0) - 1000.0).abs() < 1e-6);
        assert!(a.value(50.0) > a.value(0.0));
    }

    // -----------------------------------------------------------------------------
    // Real-data smoke tests. Ignored: they need an aircraft navigation database on
    // the machine running them, which this sandbox does not have. Run by hand with
    // `cargo test --release -- --ignored route::tests::smoke` once one is installed.
    // -----------------------------------------------------------------------------

    fn smoke(origin: &str, o: LatLon, destination: &str, d: LatLon) {
        let graph = Graph::shared();
        let o = airport(origin, o);
        let d = airport(destination, d);
        let air = StillAir;
        let cost = SimpleCost { tas_kt: 470.0, kg_per_hour: 2600.0, ceiling_ft: 41000.0, air: &air, max_tailwind_kt: 150.0 };
        let req = base_request(&o, &d, &cost, &air);
        let t0 = std::time::Instant::now();
        match plan_route(graph, &req) {
            Ok(route) => println!("{origin}-{destination}: {} ({:?})", route.route_string(), t0.elapsed()),
            Err(e) => println!("{origin}-{destination}: {e:#} ({:?})", t0.elapsed()),
        }
    }

    #[test]
    #[ignore]
    fn smoke_egll_lfpg() {
        smoke("EGLL", (51.4706, -0.4619), "LFPG", (49.0097, 2.5479));
    }

    #[test]
    #[ignore]
    fn smoke_kjfk_kbos() {
        smoke("KJFK", (40.6413, -73.7781), "KBOS", (42.3656, -71.0096));
    }

    #[test]
    #[ignore]
    fn smoke_vabb_vidp() {
        smoke("VABB", (19.0887, 72.8679), "VIDP", (28.5562, 77.1000));
    }

    #[test]
    #[ignore]
    fn smoke_egll_kjfk() {
        smoke("EGLL", (51.4706, -0.4619), "KJFK", (40.6413, -73.7781));
    }

    #[test]
    #[ignore]
    fn smoke_egll_omdb() {
        smoke("EGLL", (51.4706, -0.4619), "OMDB", (25.2528, 55.3644));
    }

    // TODO(incomplete): the sweep harness the task asks for (all seven flights, per-flight
    // time/%-over/edges-costed/nodes-expanded, run against the real navigation database) has
    // not been written yet. While diagnosing the EGLL-OMDB outlier by hand, ad hoc `#[ignore]`
    // tests here (since removed) found: real airways alone (no ellipse, no direct legs, real
    // conflict-zone hazards) already reach OMDB in about 70ms at 15.3% over great-circle; the
    // STAGES ladder was NOT the cause (trying wider rungs only made this route worse and far
    // slower, so "stop at the first successful rung" was left as it is); the actual cause was
    // `directs::build` — a candidate pool of ~12,000 fixes on a thin ellipse, each paying its
    // own ~220 nm grid query, cost roughly a second on its own, and the resulting free-route
    // legs gave the greedy search enough rope to zig-zag past what `search::refine`'s 220 nm
    // cap could straighten back out (22% over, 1.5-3.5s). The `WELL_CONNECTED_OUT_DEGREE`
    // filter in `directs.rs` (skip a fix already carrying a few real edges: it is not the
    // sparse spot a direct leg exists for) is a first, partial fix, landed here; it alone
    // brought OMDB to 9.2% over in about 740ms of `Context::build` for one rung. A second half
    // of the fix — widening `search::refine`'s own straightening cap, since a 220 nm cap
    // cannot undo a zig-zag bigger than that — was tried (REFINE_MAX_NM = 1500.0) and reverted:
    // it broke `a_fix_outside_the_ellipse_is_never_on_the_route` and
    // `the_ellipse_ladder_widens_when_a_thin_one_finds_nothing`, because `search::refine` costs
    // its candidate shortcut through the raw `CostModel` directly rather than through
    // `cost::cost_leg`, so it never asks an `EdgeRule` whether the straight line it proposes is
    // allowed — a forbidden airway can be silently redrawn as a `DCT` covering the same two
    // points once the cap is wide enough to reach across it. `search::refine` needs the edge
    // rules threaded through (or its shortcut costed via `cost_leg`) before that cap can safely
    // widen; until then this is reverted to the original `MAX_DIRECT_NM`. `directs::build` is
    // still the dominant cost for a long-haul route and has not been brought under a sensible
    // budget, and none of this has been re-checked against the six shorter flights to confirm
    // they have not regressed. That confirmation, the refine/EdgeRule fix above, the
    // graph-caching work in `sources::navdata`/`graph.rs`, `Graph::key`'s numeric key, the
    // `LANDMARK_COUNT` check, and the final constant sweep are all still to do.

    // -----------------------------------------------------------------------------
    // A synthetic worldwide-scale network, for the timings a real one could not be
    // measured on in this sandbox (no aircraft navigation database is installed
    // here). A grid of fixes at roughly one-degree spacing, four-connected plus a
    // scattering of longer "airway" shortcuts, lands in the 10^5 fixes / 10^6
    // directed edges the real network is expected to be. Ignored by default: this
    // builds and searches a genuinely large graph, which does not belong in the
    // ordinary fast test run.
    // -----------------------------------------------------------------------------

    fn synthetic_worldwide() -> Graph {
        let mut g = Graph::default();
        let (lat_step, lon_step) = (0.7f64, 0.9f64);
        let lats: Vec<f64> = (0..229).map(|x| -80.0 + x as f64 * lat_step).collect();
        let lons: Vec<f64> = (0..400).map(|x| -180.0 + x as f64 * lon_step).collect();
        let name = |i: usize, j: usize| format!("G{i:04}{j:04}");
        let pos = |i: usize, j: usize| (lats[i], lons[j]);
        for i in 0..lats.len() {
            for j in 0..lons.len() {
                let here = (name(i, j), pos(i, j));
                // East, north, and the two diagonals: six or eight airways out of most
                // fixes, in the range a real route network runs.
                for (di, dj) in [(0i64, 1i64), (1, 0), (1, 1), (1, -1)] {
                    let (ni, nj) = (i as i64 + di, j as i64 + dj);
                    if ni < 0 || nj < 0 || ni as usize >= lats.len() || nj as usize >= lons.len() {
                        continue;
                    }
                    let (ni, nj) = (ni as usize, nj as usize);
                    let there = (name(ni, nj), pos(ni, nj));
                    g.add("UA1", (&here.0, here.1), (&there.0, there.1), false, None, None);
                }
            }
        }
        // A scattering of long-range shortcuts, the way a handful of oceanic tracks or
        // long domestic airways would sit among mostly-local segments.
        let mut rng: u64 = 0xD1B54A32D192ED03;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for _ in 0..100_000 {
            let (i1, j1) = ((next() % lats.len() as u64) as usize, (next() % lons.len() as u64) as usize);
            let (i2, j2) = ((next() % lats.len() as u64) as usize, (next() % lons.len() as u64) as usize);
            let a = (name(i1, j1), pos(i1, j1));
            let b = (name(i2, j2), pos(i2, j2));
            g.add("UL9", (a.0.as_str(), a.1), (b.0.as_str(), b.1), true, None, None);
        }
        g
    }

    /// The true optimum for one candidate level, from every entry candidate to every exit
    /// candidate, plus the direct legs at each end: plain Dijkstra, eagerly costed, the
    /// same reference [`search::tests::the_greedy_search_is_close_to_optimal_on_random_graphs`]
    /// measures the greedy search against. Not what `Context::attempt` actually searches —
    /// it never changes level — but an honest lower bound the portfolio's winner can be
    /// compared to.
    fn true_optimum(ctx: &Context) -> f32 {
        let mut best = f32::INFINITY;
        for &level_ft in &ctx.levels_ft {
            let frozen = cost::Frozen { cost: ctx.req.cost, cost_index: ctx.req.cost_index, when: ctx.frozen_when, hazards: ctx.req.hazards, edge_rules: ctx.req.edge_rules, origin: &ctx.req.origin.icao, destination: &ctx.req.destination.icao, flown_nm: ctx.flown_nm };
            let metric = cost::build_metrics(ctx.graph, &ctx.compact, std::slice::from_ref(&level_ft), &frozen).pop().unwrap();
            for &entry in &ctx.entry_candidates {
                let entry_bias = ctx.req.cost.leg(&LegQuery { from: ctx.entry_pos, to: ctx.graph.fix_pos(entry), level_ft, when: ctx.frozen_when, flown_nm: 0.0 }).value(ctx.req.cost_index) as f32;
                let dijkstra = search::dijkstra_from(&ctx.compact, &metric, entry);
                for &exit in &ctx.exit_candidates {
                    let exit_bias = ctx.req.cost.leg(&LegQuery { from: ctx.graph.fix_pos(exit), to: ctx.exit_pos, level_ft, when: ctx.frozen_when, flown_nm: 0.0 }).value(ctx.req.cost_index) as f32;
                    let total = entry_bias + dijkstra[exit as usize] + exit_bias;
                    best = best.min(total);
                }
            }
        }
        best
    }

    /// A short flight, a European-scale one, and a transatlantic one, over the synthetic
    /// worldwide network: the wall time to find a route warm, how many of the network's
    /// edges were actually costed, how many nodes were expanded, and the route found as a
    /// ratio of the true optimum — the four numbers together that say what the speed
    /// costs.
    #[test]
    #[ignore]
    fn timings_on_a_synthetic_worldwide_network() {
        let t0 = std::time::Instant::now();
        let g = synthetic_worldwide();
        let compact = g.compact();
        println!("build: {} fixes, {} directed edges, {:?}, {:.1} MB", compact.node_count(), compact.edge_count(), t0.elapsed(), compact.memory_mb());

        let t1 = std::time::Instant::now();
        let _ = landmarks::tables(&compact, LANDMARK_COUNT);
        println!("landmarks ({LANDMARK_COUNT}): {:?} (cached from here on)", t1.elapsed());

        let air = StillAir;
        let cost = SimpleCost { tas_kt: 470.0, kg_per_hour: 2600.0, ceiling_ft: 41000.0, air: &air, max_tailwind_kt: 150.0 };
        let levels_ft = [31000.0, 33000.0, 35000.0, 37000.0, 39000.0];

        let cases = [("short domestic (~150nm)", (0.0, 0.0), (1.0, 1.4)), ("European-scale (~1500nm)", (10.0, -30.0), (30.0, -10.0)), ("transatlantic (~5500nm)", (51.0, -1.4), (40.0, -74.0))];
        for (label, a, b) in cases {
            let o = airport("ORIG", a);
            let d = airport("DEST", b);
            let mut req = base_request(&o, &d, &cost, &air);
            req.levels_ft = &levels_ft;
            let Ok(ctx) = Context::build_with(&g, &req, None, None, None, None, ENTRY_CANDIDATES, None, None) else {
                println!("{label}: no network within reach");
                continue;
            };

            // Cold: the first query against this origin/destination pays for landmark
            // tables (already warmed above) and its own lazy costing.
            let tq = std::time::Instant::now();
            let cold = ctx.attempt(&[], &Nudge::default());
            let cold_elapsed = tq.elapsed();

            // Warm: a second, identical query, so the number reported is what a plan
            // actually pays once the process has settled — landmark tables built,
            // nothing else there is to warm, since every level's `LazyLevel` is rebuilt
            // fresh per query by design (see `LazyLevel`'s own doc comment for why).
            let tq = std::time::Instant::now();
            let warm = ctx.attempt(&[], &Nudge::default());
            let warm_elapsed = tq.elapsed();

            match (cold, warm) {
                (Some(_), Some((_, ws))) => {
                    let optimum = true_optimum(&ctx);
                    let ratio = if optimum.is_finite() && optimum > 0.0 { ws.cost / optimum } else { f32::NAN };
                    println!(
                        "{label}: cold {cold_elapsed:?}, warm {warm_elapsed:?}, {} of {} edges costed, {} nodes expanded, cost {:.0} vs optimum {:.0} (ratio {ratio:.3})",
                        ws.edges_costed,
                        compact.edge_count(),
                        ws.nodes_expanded,
                        ws.cost,
                        optimum
                    );
                }
                _ => println!("{label}: no route found"),
            }
        }
    }
}

#[cfg(test)]
mod reachability {
    use super::*;
    use crate::dispatch::{Airport, SimpleCost, StillAir};

    /// Whether the network itself connects two airports at a level, worked out with a plain
    /// breadth-first walk over the edges the level allows: the question of whether a route
    /// exists at all, kept apart from whether the greedy search finds it.
    #[test]
    #[ignore]
    fn what_the_network_connects() {
        let graph = Graph::shared();
        let compact = graph.compact();
        let air = StillAir;
        let cost = SimpleCost { tas_kt: 450.0, kg_per_hour: 2500.0, ceiling_ft: 41000.0, air: &air, max_tailwind_kt: 150.0 };
        for (o, op, d, dp) in [
            ("EGLL", (51.4706, -0.4619), "LFPG", (49.0097, 2.5479)),
            ("EGLL", (51.4706, -0.4619), "LIRF", (41.8003, 12.2389)),
            ("EGLL", (51.4706, -0.4619), "KJFK", (40.6413, -73.7781)),
        ] {
            let origin = Airport { icao: o.into(), name: String::new(), pos: op, elevation_ft: 0.0 };
            let destination = Airport { icao: d.into(), name: String::new(), pos: dp, elevation_ft: 0.0 };
            let req = RouteRequest { origin: &origin, destination: &destination, cruise_ft: 35000.0, levels_ft: &[35000.0], cost: &cost, cost_index: 20.0, off_block: chrono::Utc::now(), air: &air, hazards: &[], edge_rules: &[], route_rules: &[], dep_runway: None, arr_runway: None, origin_wind: None, destination_wind: None, rvsm: true, avoid_firs: &[], free_route: true };
            let ctx = Context::build(graph, &req, ENTRY_CANDIDATES, None, None).expect("a context");
            // Every node reachable from any entry candidate, ignoring cost entirely.
            let mut seen = vec![false; compact.node_count()];
            let mut queue: std::collections::VecDeque<u32> = ctx.entry_candidates.iter().copied().collect();
            for &n in &ctx.entry_candidates {
                seen[n as usize] = true;
            }
            let mut count = 0usize;
            while let Some(u) = queue.pop_front() {
                count += 1;
                for e in compact.out(u) {
                    if !seen[e.to as usize] {
                        seen[e.to as usize] = true;
                        queue.push_back(e.to);
                    }
                }
            }
            let exits_reached = ctx.exit_candidates.iter().filter(|&&n| seen[n as usize]).count();
            println!("{o}->{d}: {count} of {} fixes reachable ignoring cost; {exits_reached} of {} arrival candidates", compact.node_count(), ctx.exit_candidates.len());

            // The same walk, but only over edges the level and its rules actually allow.
            let frozen = cost::Frozen { cost: &cost, cost_index: 20.0, when: ctx.frozen_when, hazards: &[], edge_rules: &[], origin: o, destination: d, flown_nm: ctx.flown_nm };
            let lazy = cost::LazyLevel::new(graph, &compact, 35000.0, frozen);
            use crate::route::cost::EdgeCost;
            let mut seen2 = vec![false; compact.node_count()];
            let mut q2: std::collections::VecDeque<u32> = ctx.entry_candidates.iter().copied().collect();
            for &n in &ctx.entry_candidates {
                seen2[n as usize] = true;
            }
            let (mut c2, mut cut) = (0usize, 0usize);
            while let Some(u) = q2.pop_front() {
                c2 += 1;
                for (p, e) in compact.out(u).iter().enumerate() {
                    if !lazy.forward(&compact, u, p).is_finite() {
                        cut += 1;
                        continue;
                    }
                    if !seen2[e.to as usize] {
                        seen2[e.to as usize] = true;
                        q2.push_back(e.to);
                    }
                }
            }
            let e2 = ctx.exit_candidates.iter().filter(|&&n| seen2[n as usize]).count();
            println!("   at FL350: {c2} fixes reachable, {cut} edges refused; {e2} of {} arrival candidates", ctx.exit_candidates.len());
            // How near the destination the reachable part of the upper network actually gets.
            let mut best = (f64::MAX, String::new());
            for n in 0..compact.node_count() as u32 {
                if seen2[n as usize] {
                    let nm = distance_nm(compact.pos(n), dp);
                    if nm < best.0 {
                        best = (nm, graph.fix_id(n).to_string());
                    }
                }
            }
            println!("   nearest reachable fix to {d}: {} at {:.0} nm", best.1, best.0);
        }
    }
}
