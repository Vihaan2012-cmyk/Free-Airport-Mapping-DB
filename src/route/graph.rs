//! The airway network, stored the way a search reads it, not the way it is built.
//!
//! A worldwide network is of the order of 10^5 fixes and 10^6 directed edges, and one
//! flight plan may search it a dozen times over — once to customise each candidate level,
//! more than once again if a rule sends the search back. So the network a search actually
//! walks is not a `Vec` of `Vec`s of owned strings, which is what `Graph::add` is
//! convenient to build with, but two flat arrays: an offset into a run of edges for every
//! fix (forward, for the search going out from the origin, and reverse, for the search
//! coming in from the destination), and edges sixteen bytes wide. Airway names are
//! interned once, in a table the edges index into rather than carry.
//!
//! `Graph` keeps both shapes at once: the growable one `add` writes to, and the flat one
//! [`Graph::compact`] builds from it on first use and keeps, so a graph built once and
//! searched many times pays the cost of flattening only the once.

use super::spatial::Grid;
use crate::dispatch::LatLon;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Instant;

/// The width of a cell in [`Graph::spatial`]'s index: wide enough that a lookup at a
/// realistic radius (a join, or a free-route direct leg's few hundred miles) touches only a
/// handful of cells, narrow enough that a cell over a crowded part of the network still
/// holds few enough fixes to scan.
const SPATIAL_CELL_DEG: f64 = 2.0;

/// A fix the network runs through: its name and where it is. Kept for reporting a route
/// back in names; the search itself reads [`Compact`] and touches this only to translate
/// a node id back into an identifier at the end.
#[derive(Debug, Clone)]
pub struct Fix {
    pub id: String,
    pub pos: LatLon,
}

/// One directed segment of the network, packed to sixteen bytes: a search over a million
/// of these should spend its time on arithmetic, not on chasing pointers.
#[derive(Debug, Clone, Copy)]
pub struct Edge {
    pub to: u32,
    airway: u32,
    min_fl: u16,
    max_fl: u16,
}

const NO_MIN_FL: u16 = 0;
const NO_MAX_FL: u16 = u16::MAX;

impl Edge {
    pub fn min_ft(&self) -> Option<f64> {
        (self.min_fl != NO_MIN_FL).then(|| self.min_fl as f64 * 100.0)
    }

    pub fn max_ft(&self) -> Option<f64> {
        (self.max_fl != NO_MAX_FL).then(|| self.max_fl as f64 * 100.0)
    }

    pub fn airway_id(&self) -> u32 {
        self.airway
    }

    /// Whether the edge may be flown at a level: within its own limits and the aircraft's.
    pub fn allowed_at(&self, level_ft: f64, ceiling_ft: f64) -> bool {
        if level_ft > ceiling_ft {
            return false;
        }
        if self.min_ft().is_some_and(|m| m > level_ft) {
            return false;
        }
        if self.max_ft().is_some_and(|m| m < level_ft) {
            return false;
        }
        true
    }
}

fn to_fl(ft: Option<f64>, sentinel: u16) -> u16 {
    match ft {
        None => sentinel,
        Some(v) => ((v / 100.0).round().clamp(0.0, 65534.0)) as u16,
    }
}

/// One row the builder keeps for a segment out of a fix, before it is flattened.
#[derive(Debug, Clone, Copy)]
struct RawEdge {
    to: u32,
    airway: u32,
    min_fl: u16,
    max_fl: u16,
}

/// The network flattened for a search to run on: forward edges (for the half of a
/// bidirectional search working out from the origin) and reverse edges (for the half
/// working back from the destination, which for a one-way airway are not the same set).
/// A fix's cosine of latitude is kept alongside its position, the one trigonometric call
/// an inner loop would otherwise repeat for every edge leaving it.
pub struct Compact {
    pub offsets: Vec<u32>,
    pub edges: Vec<Edge>,
    pub roffsets: Vec<u32>,
    /// The reverse of `edges`: `redges[i].to` is the fix an original edge came *from*, so
    /// that walking a reverse edge out of a fix reaches a predecessor of it.
    pub redges: Vec<Edge>,
    /// For a reverse edge, the position of the same edge in `edges`: a per-level cost is
    /// stored once, parallel to `edges`, and this is how the backward half of a search
    /// looks the same figure up.
    pub redge_forward: Vec<u32>,
    pub lat: Vec<f32>,
    pub lon: Vec<f32>,
    pub cos_lat: Vec<f32>,
    pub build_time: std::time::Duration,
}

impl Compact {
    pub fn node_count(&self) -> usize {
        self.lat.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn pos(&self, node: u32) -> LatLon {
        (self.lat[node as usize] as f64, self.lon[node as usize] as f64)
    }

    pub fn out(&self, node: u32) -> &[Edge] {
        let (a, b) = (self.offsets[node as usize] as usize, self.offsets[node as usize + 1] as usize);
        &self.edges[a..b]
    }

    /// Where `out(node)` starts in the flat edge array: added to a position within that
    /// slice, the index a per-level cost array (parallel to `edges`) is read at.
    pub fn out_base(&self, node: u32) -> usize {
        self.offsets[node as usize] as usize
    }

    pub fn r#in(&self, node: u32) -> &[Edge] {
        let (a, b) = (self.roffsets[node as usize] as usize, self.roffsets[node as usize + 1] as usize);
        &self.redges[a..b]
    }

    pub fn in_base(&self, node: u32) -> usize {
        self.roffsets[node as usize] as usize
    }

    /// A rough estimate of the memory the flat arrays hold, megabytes.
    pub fn memory_mb(&self) -> f64 {
        let bytes = self.offsets.len() * 4
            + self.roffsets.len() * 4
            + (self.edges.len() + self.redges.len()) * std::mem::size_of::<Edge>()
            + self.redge_forward.len() * 4
            + self.lat.len() * 4
            + self.lon.len() * 4
            + self.cos_lat.len() * 4;
        bytes as f64 / (1024.0 * 1024.0)
    }
}

/// The airway network: every fix, and the segments between them. Built cheaply with
/// [`Graph::add`], one segment at a time; searched by way of [`Graph::compact`], which
/// flattens it once and keeps the result.
pub struct Graph {
    fixes: Vec<Fix>,
    index: HashMap<String, u32>,
    adj: Vec<Vec<RawEdge>>,
    airway_names: Vec<String>,
    airway_index: HashMap<String, u32>,
    compact: RwLock<Option<Arc<Compact>>>,
    spatial: RwLock<Option<Arc<Grid<u32>>>>,
    /// A second index alongside `spatial`, of every fix rather than only ones with a
    /// segment out of them: what [`super::directs::build`] needs, since a mid-ocean
    /// reporting point with no airway at all is exactly what a direct leg is for.
    spatial_all: RwLock<Option<Arc<Grid<u32>>>>,
}

impl Default for Graph {
    fn default() -> Graph {
        Graph { fixes: Vec::new(), index: HashMap::new(), adj: Vec::new(), airway_names: Vec::new(), airway_index: HashMap::new(), compact: RwLock::new(None), spatial: RwLock::new(None), spatial_all: RwLock::new(None) }
    }
}

impl Graph {
    fn key(id: &str, pos: LatLon) -> String {
        // Fixes repeat their names round the world, so a fix is told apart by where it
        // is, to three decimal places: closer than that and two published positions of
        // the same fix are the same fix.
        format!("{id}@{:.3},{:.3}", pos.0, pos.1)
    }

    fn fix(&mut self, id: &str, pos: LatLon) -> u32 {
        let key = Graph::key(id, pos);
        if let Some(i) = self.index.get(&key) {
            return *i;
        }
        self.fixes.push(Fix { id: id.to_string(), pos });
        self.adj.push(Vec::new());
        let i = self.fixes.len() as u32 - 1;
        self.index.insert(key, i);
        i
    }

    fn airway(&mut self, name: &str) -> u32 {
        if let Some(i) = self.airway_index.get(name) {
            return *i;
        }
        self.airway_names.push(name.to_string());
        let i = self.airway_names.len() as u32 - 1;
        self.airway_index.insert(name.to_string(), i);
        i
    }

    /// Add a segment, and its reverse unless it is one-way. The signature other parts of
    /// flight planning build against (the oceanic engine puts tracks into the network this
    /// way): it must keep working exactly as it does today.
    #[allow(clippy::too_many_arguments)]
    pub fn add(&mut self, airway: &str, from: (&str, LatLon), to: (&str, LatLon), one_way: bool, min_ft: Option<f64>, max_ft: Option<f64>) {
        let a = self.fix(from.0, from.1);
        let b = self.fix(to.0, to.1);
        if a == b {
            return;
        }
        let aw = self.airway(airway);
        let (min_fl, max_fl) = (to_fl(min_ft, NO_MIN_FL), to_fl(max_ft, NO_MAX_FL));
        self.adj[a as usize].push(RawEdge { to: b, airway: aw, min_fl, max_fl });
        if !one_way {
            self.adj[b as usize].push(RawEdge { to: a, airway: aw, min_fl, max_fl });
        }
        // A search already holding the flat form would not see this: correctness before
        // speed, since adding to a graph already in use is not something a plan does.
        *self.compact.write().unwrap() = None;
        *self.spatial.write().unwrap() = None;
        *self.spatial_all.write().unwrap() = None;
    }

    /// Every fix in `waypoints` the network does not already have, added with no segment at
    /// all: for a free-route direct leg to land on where nothing is published to fly between
    /// two points, over open ocean above all. Most of what the navigation database's own
    /// table of enroute waypoints offers here is already in the network through some airway
    /// segment, and this is a harmless no-op for each of those — [`Graph::fix`] dedupes by
    /// identifier and position regardless of how a fix arrived. What is left, once every
    /// airway has had its say, is exactly the reporting points a NAT track is built from
    /// message by message and nothing else ever joins.
    pub fn add_fixes(&mut self, waypoints: impl IntoIterator<Item = (String, LatLon)>) {
        for (id, pos) in waypoints {
            self.fix(&id, pos);
        }
        *self.compact.write().unwrap() = None;
        *self.spatial.write().unwrap() = None;
        *self.spatial_all.write().unwrap() = None;
    }

    pub fn fixes(&self) -> &[Fix] {
        &self.fixes
    }

    pub fn airway_name(&self, id: u32) -> &str {
        &self.airway_names[id as usize]
    }

    pub fn fix_id(&self, node: u32) -> &str {
        &self.fixes[node as usize].id
    }

    pub fn fix_pos(&self, node: u32) -> LatLon {
        self.fixes[node as usize].pos
    }

    /// A fix by name and position, if the network has one there: for stitching a SID's
    /// last fix or a STAR's first fix onto the enroute network by identity rather than by
    /// a direct leg, when the procedure and the airway agree on where it is.
    pub fn find(&self, id: &str, pos: LatLon) -> Option<u32> {
        self.index.get(&Graph::key(id, pos)).copied()
    }

    /// The network in the navigation database on this machine.
    pub fn from_navdata() -> Graph {
        let mut g = Graph::default();
        // Collected once and consumed: asking the database for them twice reads ninety
        // thousand rows for nothing.
        let segs = crate::sources::navdata::airway_segments();
        for s in segs {
            g.add(&s.airway, (&s.from.0, (s.from.1, s.from.2)), (&s.to.0, (s.to.1, s.to.2)), s.one_way, s.min_ft, s.max_ft);
        }
        // Every enroute waypoint the database knows of, airway or none: a free-route direct
        // leg needs somewhere to land over open ocean, where nothing is published to fly at
        // all, and this is the only place those points otherwise appear.
        let waypoints = crate::sources::navdata::enroute_waypoints().into_iter().map(|(id, lat, lon)| (id, (lat, lon)));
        g.add_fixes(waypoints);
        g
    }

    /// The worldwide network, built once for the life of the process. Every plan searches
    /// this rather than building its own: the flattening alone takes long enough that
    /// paying for it per-request would dwarf the search itself.
    pub fn shared() -> &'static Graph {
        static G: OnceLock<Graph> = OnceLock::new();
        G.get_or_init(|| {
            let t0 = Instant::now();
            let g = Graph::from_navdata();
            let c = g.compact();
            log::info!("route graph: {} fixes, {} directed edges, built in {:?}, {:.1} MB", g.fixes.len(), c.edges.len(), t0.elapsed(), c.memory_mb());
            g
        })
    }

    /// The flattened network, built on first use and kept: forward and reverse
    /// compressed adjacency, ready for the search's inner loop to read without a
    /// `HashMap` or an allocation.
    pub fn compact(&self) -> Arc<Compact> {
        if let Some(c) = self.compact.read().unwrap().as_ref() {
            return c.clone();
        }
        let mut w = self.compact.write().unwrap();
        if let Some(c) = w.as_ref() {
            return c.clone();
        }
        let t0 = Instant::now();
        let n = self.fixes.len();
        let mut offsets = vec![0u32; n + 1];
        for (i, row) in self.adj.iter().enumerate() {
            offsets[i + 1] = offsets[i] + row.len() as u32;
        }
        let mut edges = Vec::with_capacity(offsets[n] as usize);
        for row in &self.adj {
            for e in row {
                edges.push(Edge { to: e.to, airway: e.airway, min_fl: e.min_fl, max_fl: e.max_fl });
            }
        }
        // The reverse graph: for every edge a -> b, one b -> a in the reverse table, so
        // that walking "in" edges out of b reaches a. Built by counting, then filling: two
        // passes over the edge list rather than a per-edge push into a growable row.
        let mut rcount = vec![0u32; n + 1];
        for (from, row) in self.adj.iter().enumerate() {
            let _ = from;
            for e in row {
                rcount[e.to as usize + 1] += 1;
            }
        }
        for i in 0..n {
            rcount[i + 1] += rcount[i];
        }
        let roffsets = rcount.clone();
        let mut redges = vec![Edge { to: 0, airway: 0, min_fl: 0, max_fl: 0 }; roffsets[n] as usize];
        let mut redge_forward = vec![0u32; roffsets[n] as usize];
        let mut cursor = roffsets.clone();
        let mut flat = 0u32;
        for (from, row) in self.adj.iter().enumerate() {
            for e in row {
                let slot = cursor[e.to as usize];
                redges[slot as usize] = Edge { to: from as u32, airway: e.airway, min_fl: e.min_fl, max_fl: e.max_fl };
                redge_forward[slot as usize] = flat;
                cursor[e.to as usize] += 1;
                flat += 1;
            }
        }
        let mut lat = Vec::with_capacity(n);
        let mut lon = Vec::with_capacity(n);
        let mut cos_lat = Vec::with_capacity(n);
        for f in &self.fixes {
            lat.push(f.pos.0 as f32);
            lon.push(f.pos.1 as f32);
            cos_lat.push(f.pos.0.to_radians().cos().max(0.01) as f32);
        }
        let c = Arc::new(Compact { offsets, edges, roffsets, redges, redge_forward, lat, lon, cos_lat, build_time: t0.elapsed() });
        *w = Some(c.clone());
        c
    }

    /// A coarse spatial index over every fix that has at least one segment out of it,
    /// built on first use and kept, the same way [`Graph::compact`] is: [`Graph::nearest`]
    /// used to be a linear scan of every fix in the network, about forty milliseconds on a
    /// worldwide one and the largest per-plan cost left outside the search proper; this
    /// turns that into a lookup over a handful of grid cells.
    pub fn spatial(&self) -> Arc<Grid<u32>> {
        if let Some(g) = self.spatial.read().unwrap().as_ref() {
            return g.clone();
        }
        let mut w = self.spatial.write().unwrap();
        if let Some(g) = w.as_ref() {
            return g.clone();
        }
        let mut grid = Grid::new(SPATIAL_CELL_DEG);
        for (i, f) in self.fixes.iter().enumerate() {
            if !self.adj[i].is_empty() {
                grid.insert(f.pos, i as u32);
            }
        }
        let g = Arc::new(grid);
        *w = Some(g.clone());
        g
    }

    /// The same index as [`Graph::spatial`], but of every fix the network has, not only
    /// ones with a segment out of them: built once and kept the same way, and what
    /// [`super::directs::build`] reads instead, since a mid-ocean reporting point with no
    /// airway out of it at all is exactly the fix a free-route direct leg needs to land on.
    pub fn spatial_all(&self) -> Arc<Grid<u32>> {
        if let Some(g) = self.spatial_all.read().unwrap().as_ref() {
            return g.clone();
        }
        let mut w = self.spatial_all.write().unwrap();
        if let Some(g) = w.as_ref() {
            return g.clone();
        }
        let mut grid = Grid::new(SPATIAL_CELL_DEG);
        for (i, f) in self.fixes.iter().enumerate() {
            grid.insert(f.pos, i as u32);
        }
        let g = Arc::new(grid);
        *w = Some(g.clone());
        g
    }

    /// The fixes with an edge out of them nearest a point, nearest first, up to `n` of
    /// them within `within_nm`. Read through [`Graph::spatial`], so this is a lookup over a
    /// few grid cells rather than a scan of every fix in the network; called only a handful
    /// of times a plan, to join the airports onto the network, never in the search's own
    /// inner loop.
    pub fn nearest(&self, at: LatLon, n: usize, within_nm: f64) -> Vec<(u32, f64)> {
        let grid = self.spatial();
        grid.near(at, within_nm).into_iter().take(n).map(|(i, d)| (*grid.get(i).0, d)).collect()
    }

    pub fn node_count(&self) -> usize {
        self.fixes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> Graph {
        let mut g = Graph::default();
        g.add("UA1", ("A", (0.0, 0.0)), ("B", (0.0, 1.0)), false, Some(18000.0), Some(45000.0));
        g.add("UA1", ("B", (0.0, 1.0)), ("C", (0.0, 2.0)), true, None, None);
        g
    }

    #[test]
    fn add_builds_forward_and_reverse_and_respects_one_way() {
        let g = tiny();
        let c = g.compact();
        assert_eq!(c.node_count(), 3);
        let a = g.find("A", (0.0, 0.0)).unwrap();
        let b = g.find("B", (0.0, 1.0)).unwrap();
        let cc = g.find("C", (0.0, 2.0)).unwrap();
        assert_eq!(c.out(a).len(), 1);
        assert_eq!(c.out(a)[0].to, b);
        // B -> A exists (UA1 is two-way there) and B -> C exists (one-way there).
        assert_eq!(c.out(b).len(), 2);
        // Nothing flies C -> B: it was added one-way.
        assert_eq!(c.out(cc).len(), 0);
        // The reverse graph carries a predecessor of C from B.
        assert_eq!(c.r#in(cc).len(), 1);
        assert_eq!(c.r#in(cc)[0].to, b);
    }

    #[test]
    fn edge_limits_pack_and_unpack() {
        let g = tiny();
        let c = g.compact();
        let a = g.find("A", (0.0, 0.0)).unwrap();
        let e = &c.out(a)[0];
        assert_eq!(e.min_ft(), Some(18000.0));
        assert_eq!(e.max_ft(), Some(45000.0));
        assert!(e.allowed_at(30000.0, 41000.0));
        assert!(!e.allowed_at(50000.0, 41000.0), "above its own maximum");
        assert!(!e.allowed_at(30000.0, 20000.0), "above the aircraft's ceiling");
        assert!(!e.allowed_at(10000.0, 41000.0), "below its own minimum");
    }

    #[test]
    fn compact_is_cached_until_add_invalidates_it() {
        let mut g = tiny();
        let c1 = g.compact();
        let c2 = g.compact();
        assert!(Arc::ptr_eq(&c1, &c2));
        g.add("UA2", ("A", (0.0, 0.0)), ("D", (1.0, 1.0)), false, None, None);
        let c3 = g.compact();
        assert!(!Arc::ptr_eq(&c1, &c3));
        assert_eq!(c3.node_count(), 4);
    }

    #[test]
    fn airway_names_are_interned_once() {
        let g = tiny();
        let c = g.compact();
        let a = g.find("A", (0.0, 0.0)).unwrap();
        let e = &c.out(a)[0];
        assert_eq!(g.airway_name(e.airway_id()), "UA1");
        assert_eq!(g.fixes().len(), 3);
    }
}
