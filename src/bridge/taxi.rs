//! Taxi routes: a clearance such as `A B K 04L` or `B K B21`, routed over the airport's
//! taxiway network from where the aircraft is.
//!
//! The clearance names the taxiways in the order they are followed, and where to: a
//! runway (to its holding point on the last taxiway) or a stand. Short unnamed links
//! between taxiways are taken as needed, at a cost; runways are crossed but never taxied
//! along; before the first named taxiway anything leads onto it (from a stand, an apron).
//!
//! The route found is kept as the current one, for every display that asks: the OANS
//! toolbar window, where the clearance is typed, and the A320 OANS on the ND.

use crate::model::codes::{direc, edgetype, nodetype};
use crate::model::AmdbFeature;
use geo_types::{Coord, Geometry};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{BTreeSet, BinaryHeap, HashMap};
use std::sync::Mutex;

/// Words a clearance may carry that name nothing.
const FILLER: [&str; 13] = ["TAXI", "VIA", "TO", "RWY", "RUNWAY", "HOLD", "SHORT", "OF", "STAND", "GATE", "AND", "THEN", "AT"];

/// How much dearer than its length a stretch is: before the first named taxiway
/// (reaching it from a stand or an apron), an unnamed link between taxiways, and a
/// taxiway the clearance does not name, taken only to reach the first one.
const APPROACH_COST: f64 = 1.2;
const LINK_COST: f64 = 2.0;
const OTHER_TAXIWAY_COST: f64 = 4.0;

#[derive(Debug, Clone)]
struct Node {
    at: Coord<f64>,
    kind: i64,
    runways: Vec<String>,
    stand: Option<String>,
}

#[derive(Debug, Clone)]
struct Edge {
    a: usize,
    b: usize,
    name: Option<String>,
    kind: i64,
    direc: i64,
    pts: Vec<Coord<f64>>,
    len: f64,
}

/// The airport's taxiway network, in the local metre frame the airport is served in.
#[derive(Debug, Clone, Default)]
pub struct Network {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    adj: Vec<Vec<usize>>,
}

fn upper(v: Option<&str>) -> Option<String> {
    v.map(|s| s.trim().to_ascii_uppercase()).filter(|s| !s.is_empty())
}

/// A runway's ends from its name, whichever way the data joins them: `04L/22R`, `04L.22R`.
fn runway_ends(v: &str) -> Vec<String> {
    v.split(['/', '.', '-']).map(|s| s.trim().to_ascii_uppercase()).filter(|s| !s.is_empty()).collect()
}

/// How far from a runway a holding point is taken to be that runway's, in metres.
const HOLD_TO_RUNWAY_M: f64 = 200.0;

fn point_line_distance(p: Coord<f64>, line: &[Coord<f64>]) -> f64 {
    line.windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let len2 = dx * dx + dy * dy;
            let t = if len2 == 0.0 { 0.0 } else { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) };
            dist(p, Coord { x: a.x + t * dx, y: a.y + t * dy })
        })
        .fold(f64::INFINITY, f64::min)
}

impl Network {
    /// From the generator's own routing-network features (before they are converted for
    /// the aircraft), already in the local frame.
    pub fn build(edges: &[AmdbFeature], nodes: &[AmdbFeature]) -> Network {
        let mut net = Network::default();
        let mut index: HashMap<i64, usize> = HashMap::new();
        for f in nodes {
            let (Some(id), Geometry::Point(p)) = (f.props.get("nodeid").and_then(Value::as_i64), &f.geom) else { continue };
            index.insert(id, net.nodes.len());
            net.nodes.push(Node {
                at: p.0,
                kind: f.props.get("nodetype").and_then(Value::as_i64).unwrap_or(0),
                runways: f.get_str("idrwy").map(runway_ends).unwrap_or_default(),
                stand: upper(f.get_str("idstd")),
            });
        }
        for f in edges {
            let Geometry::LineString(line) = &f.geom else { continue };
            if line.0.len() < 2 {
                continue;
            }
            let mut end = |key: &str, at: Coord<f64>| -> usize {
                let id = f.props.get(key).and_then(Value::as_i64);
                match id.and_then(|id| index.get(&id).copied()) {
                    Some(i) => i,
                    None => {
                        net.nodes.push(Node { at, kind: nodetype::UNKNOWN, runways: Vec::new(), stand: None });
                        if let Some(id) = id {
                            index.insert(id, net.nodes.len() - 1);
                        }
                        net.nodes.len() - 1
                    }
                }
            };
            let a = end("stnode", line.0[0]);
            let b = end("ennode", *line.0.last().unwrap());
            let len = line.0.windows(2).map(|w| ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt()).sum();
            let kind = f.props.get("edgetype").and_then(Value::as_i64).unwrap_or(0);
            // A stand's own lead-in edge names the stand; its far node is the stand.
            if kind == edgetype::STAND {
                if let Some(stand) = upper(f.get_str("idstd")) {
                    for n in [a, b] {
                        if net.nodes[n].kind == nodetype::STAND && net.nodes[n].stand.is_none() {
                            net.nodes[n].stand = Some(stand.clone());
                        }
                    }
                }
            }
            if kind == edgetype::RUNWAY {
                if let Some(rwy) = f.get_str("idrwy") {
                    for n in [a, b] {
                        for r in runway_ends(rwy) {
                            if !net.nodes[n].runways.contains(&r) {
                                net.nodes[n].runways.push(r);
                            }
                        }
                    }
                }
            }
            net.edges.push(Edge { a, b, name: upper(f.get_str("idlin")), kind, direc: f.props.get("direc").and_then(Value::as_i64).unwrap_or(direc::BIDIRECTIONAL), pts: line.0.clone(), len });
        }
        // Holding points name their taxiway, not the runway they protect: that is the
        // nearest runway, within reach.
        let runway_lines: Vec<(Vec<String>, Vec<Coord<f64>>)> = edges
            .iter()
            .filter(|f| f.props.get("edgetype").and_then(Value::as_i64) == Some(edgetype::RUNWAY))
            .filter_map(|f| match (&f.geom, f.get_str("idrwy")) {
                (Geometry::LineString(l), Some(r)) => Some((runway_ends(r), l.0.clone())),
                _ => None,
            })
            .collect();
        for node in net.nodes.iter_mut().filter(|n| n.kind == nodetype::HOLDING_POSITION && n.runways.is_empty()) {
            let nearest = runway_lines.iter().map(|(ends, line)| (point_line_distance(node.at, line), ends)).filter(|(d, _)| *d <= HOLD_TO_RUNWAY_M).min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
            if let Some((_, ends)) = nearest {
                node.runways = ends.clone();
            }
        }
        net.adj = vec![Vec::new(); net.nodes.len()];
        for (i, e) in net.edges.iter().enumerate() {
            net.adj[e.a].push(i);
            net.adj[e.b].push(i);
        }
        net
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    fn taxiways(&self) -> BTreeSet<String> {
        self.edges.iter().filter(|e| e.kind != edgetype::RUNWAY).filter_map(|e| e.name.clone()).collect()
    }

    fn runways(&self) -> BTreeSet<String> {
        self.nodes.iter().flat_map(|n| n.runways.iter().cloned()).collect()
    }

    fn stands(&self) -> BTreeSet<String> {
        self.nodes.iter().filter_map(|n| n.stand.clone()).collect()
    }
}

/// Where a clearance ends.
#[derive(Debug, Clone, PartialEq)]
pub enum Destination {
    Runway(String),
    Stand(String),
}

/// A clearance read against an airport: the taxiways in order and where to.
#[derive(Debug, Clone, PartialEq)]
pub struct Clearance {
    pub via: Vec<String>,
    pub to: Destination,
}

/// Read a clearance: each word is a taxiway of this airport, or where to (a runway end
/// such as 04L, or a stand), in any order; filler words (VIA, TO, RWY...) are skipped.
pub fn parse(net: &Network, text: &str) -> Result<Clearance, String> {
    let (taxiways, runways, stands) = (net.taxiways(), net.runways(), net.stands());
    let mut via = Vec::new();
    let mut to = None;
    // A stand's name may be several words ("Korean Air Cargo (2)"): the longest one the
    // clearance ends with is where to.
    let upper_text = text.trim().to_ascii_uppercase();
    let mut text = upper_text.as_str();
    if let Some(stand) = stands.iter().filter(|s| s.contains(' ') && text.ends_with(s.as_str())).max_by_key(|s| s.len()) {
        to = Some(Destination::Stand(stand.clone()));
        text = text[..text.len() - stand.len()].trim_end();
    }
    for word in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';').map(|w| w.trim().to_ascii_uppercase()).filter(|w| !w.is_empty()) {
        if FILLER.contains(&word.as_str()) {
            continue;
        }
        let word = word.trim_start_matches("RWY").to_string();
        if taxiways.contains(&word) && !(runways.contains(&word) || stands.contains(&word)) {
            via.push(word);
        } else if runways.contains(&word) {
            to = Some(Destination::Runway(word));
        } else if stands.contains(&word) {
            to = Some(Destination::Stand(word));
        } else if taxiways.contains(&word) {
            via.push(word);
        } else {
            return Err(format!("No taxiway, runway or stand called {word} here"));
        }
    }
    let Some(to) = to else {
        return Err("Say where to as well: a runway (04L) or a stand".to_string());
    };
    Ok(Clearance { via, to })
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct State {
    cost: f64,
    node: usize,
    phase: usize,
}

impl Eq for State {}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        other.cost.partial_cmp(&self.cost).unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A route found: its line (local metres, from the aircraft), the taxiways it follows in
/// order, and its length.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub points: Vec<Coord<f64>>,
    pub legs: Vec<String>,
    pub length_m: f64,
}

/// Route a clearance from a position.
pub fn route(net: &Network, from: Coord<f64>, clearance: &Clearance) -> Result<Route, String> {
    let n = clearance.via.len();
    let start = (0..net.nodes.len())
        .filter(|&i| !net.adj[i].is_empty())
        .min_by(|&a, &b| dist(net.nodes[a].at, from).partial_cmp(&dist(net.nodes[b].at, from)).unwrap_or(Ordering::Equal))
        .ok_or("This airport has no taxiway network")?;
    if dist(net.nodes[start].at, from) > 1_000.0 {
        return Err("The aircraft is not on this airport's taxiways".to_string());
    }
    let goal = |node: usize, phase: usize| {
        phase == n
            && match &clearance.to {
                Destination::Runway(r) => net.nodes[node].kind == nodetype::HOLDING_POSITION && net.nodes[node].runways.iter().any(|x| x == r),
                Destination::Stand(s) => net.nodes[node].stand.as_deref() == Some(s.as_str()),
            }
    };
    // Where a runway has no holding points in the data, its own nodes do instead.
    let has_holds = match &clearance.to {
        Destination::Runway(r) => net.nodes.iter().any(|x| x.kind == nodetype::HOLDING_POSITION && x.runways.iter().any(|y| y == r)),
        Destination::Stand(_) => true,
    };
    let goal = |node: usize, phase: usize| {
        goal(node, phase)
            || (!has_holds && phase == n && matches!(&clearance.to, Destination::Runway(r) if net.nodes[node].runways.iter().any(|x| x == r)))
    };

    let states = net.nodes.len() * (n + 1);
    let mut best = vec![f64::INFINITY; states];
    let mut prev: Vec<Option<(usize, usize, bool)>> = vec![None; states];
    let key = |node: usize, phase: usize| node * (n + 1) + phase;
    let mut heap = BinaryHeap::new();
    best[key(start, 0)] = 0.0;
    heap.push(State { cost: 0.0, node: start, phase: 0 });
    let mut found = None;
    while let Some(State { cost, node, phase }) = heap.pop() {
        if cost > best[key(node, phase)] {
            continue;
        }
        if goal(node, phase) {
            found = Some((node, phase));
            break;
        }
        for &ei in &net.adj[node] {
            let e = &net.edges[ei];
            let forward = e.a == node;
            let next = if forward { e.b } else { e.a };
            if (forward && e.direc == direc::BACKWARD) || (!forward && e.direc == direc::FORWARD) || e.kind == edgetype::RUNWAY {
                continue;
            }
            let name = e.name.as_deref();
            let step = if phase < n && name == Some(clearance.via[phase].as_str()) {
                Some((phase + 1, e.len))
            } else if phase >= 1 && name == Some(clearance.via[phase - 1].as_str()) {
                Some((phase, e.len))
            } else if name.is_none() || e.kind == edgetype::STAND || e.kind == edgetype::PARKING {
                Some((phase, e.len * if phase == 0 || phase == n { APPROACH_COST } else { LINK_COST }))
            } else if phase == 0 {
                Some((0, e.len * OTHER_TAXIWAY_COST))
            } else {
                None
            };
            let Some((to_phase, w)) = step else { continue };
            let c = cost + w;
            let k = key(next, to_phase);
            if c < best[k] {
                best[k] = c;
                prev[k] = Some((key(node, phase), ei, forward));
                heap.push(State { cost: c, node: next, phase: to_phase });
            }
        }
    }
    let Some((end, end_phase)) = found else {
        let what = match &clearance.to {
            Destination::Runway(r) => format!("runway {r}"),
            Destination::Stand(s) => format!("stand {s}"),
        };
        return Err(if n == 0 { format!("No way found to {what}") } else { format!("No way found along {} to {what}", clearance.via.join(" ")) });
    };

    // Walk back, then lay the edges' own lines end to end.
    let mut steps = Vec::new();
    let mut k = key(end, end_phase);
    while let Some((before, ei, forward)) = prev[k] {
        steps.push((ei, forward));
        k = before;
    }
    steps.reverse();
    let mut points = vec![from, net.nodes[start].at];
    let mut legs: Vec<String> = Vec::new();
    let mut length_m = dist(from, net.nodes[start].at);
    for (ei, forward) in steps {
        let e = &net.edges[ei];
        let pts: Vec<Coord<f64>> = if forward { e.pts.clone() } else { e.pts.iter().rev().copied().collect() };
        points.extend(pts.into_iter().skip(1));
        length_m += e.len;
        if let Some(name) = &e.name {
            if legs.last() != Some(name) {
                legs.push(name.clone());
            }
        }
    }
    Ok(Route { points, legs, length_m })
}

fn dist(a: Coord<f64>, b: Coord<f64>) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// The route every display shows: the last one asked for.
static CURRENT: Mutex<Option<Value>> = Mutex::new(None);

/// The current route as JSON, or null.
pub fn current() -> Value {
    CURRENT.lock().unwrap().clone().unwrap_or(Value::Null)
}

pub fn clear() {
    *CURRENT.lock().unwrap() = None;
}

/// Route a clearance at an airport from a position and keep it as the current route.
/// `points` are in the airport's local metres, as its layers are served in (ARP_AZEQ).
pub fn set(icao: &str, net: &Network, from: Coord<f64>, text: &str) -> Result<Value, String> {
    let clearance = parse(net, text)?;
    let r = route(net, from, &clearance)?;
    let to = match &clearance.to {
        Destination::Runway(r) => format!("RWY {r}"),
        Destination::Stand(s) => format!("STAND {s}"),
    };
    let value = json!({
        "icao": icao,
        "clearance": text.trim().to_ascii_uppercase(),
        "legs": r.legs,
        "to": to,
        "length_m": r.length_m.round(),
        "points": r.points.iter().map(|c| [((c.x * 10.0).round() / 10.0), ((c.y * 10.0).round() / 10.0)]).collect::<Vec<_>>(),
        // Changes whenever a new route is set, so displays can tell.
        "serial": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
    });
    *CURRENT.lock().unwrap() = Some(value.clone());
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::{LineString, Point};

    fn node(id: i64, x: f64, y: f64, kind: i64, rwy: Option<&str>, stand: Option<&str>) -> AmdbFeature {
        AmdbFeature::new(crate::model::Layer::AsrnNode, Point::new(x, y))
            .with("nodeid", id)
            .with("nodetype", kind)
            .with("idrwy", rwy.map_or(Value::Null, |r| Value::from(r)))
            .with("idstd", stand.map_or(Value::Null, |s| Value::from(s)))
    }

    fn edge(a: i64, b: i64, name: Option<&str>, kind: i64, pts: &[(f64, f64)]) -> AmdbFeature {
        AmdbFeature::new(crate::model::Layer::AsrnEdge, LineString::from(pts.to_vec()))
            .with("stnode", a)
            .with("ennode", b)
            .with("edgetype", kind)
            .with("direc", direc::BIDIRECTIONAL)
            .with("idlin", name.map_or(Value::Null, |n| Value::from(n)))
    }

    /// A stand (S1) off taxiway A, which meets B at (200,0); B runs north to a holding
    /// point for 04L (and a shorter way to it along C, not cleared).
    fn airport() -> Network {
        let nodes = vec![
            node(1, 0.0, -30.0, nodetype::STAND, None, Some("S1")),
            node(2, 0.0, 0.0, nodetype::TAXIWAY, None, None),
            node(3, 200.0, 0.0, nodetype::TAXIWAY, None, None),
            node(4, 200.0, 300.0, nodetype::HOLDING_POSITION, None, None),
            node(5, 100.0, 150.0, nodetype::HOLDING_POSITION, None, None),
            node(6, 300.0, 350.0, nodetype::RUNWAY, Some("04L/22R"), None),
            node(7, 0.0, 350.0, nodetype::RUNWAY, Some("04L/22R"), None),
            node(8, 0.0, -60.0, nodetype::STAND, None, Some("Korean Air Cargo (2)")),
        ];
        let edges = vec![
            edge(1, 2, None, edgetype::STAND, &[(0.0, -30.0), (0.0, 0.0)]),
            edge(2, 3, Some("A"), edgetype::TAXIWAY, &[(0.0, 0.0), (200.0, 0.0)]),
            edge(3, 4, Some("B"), edgetype::TAXIWAY, &[(200.0, 0.0), (200.0, 300.0)]),
            edge(2, 5, Some("C"), edgetype::TAXIWAY, &[(0.0, 0.0), (100.0, 150.0)]),
            // The runway, 50 m past B's holding point and 200 m past C's.
            edge(6, 7, None, edgetype::RUNWAY, &[(300.0, 350.0), (0.0, 350.0)]).with("idrwy", "04L/22R"),
            edge(8, 1, None, edgetype::STAND, &[(0.0, -60.0), (0.0, -30.0)]),
        ];
        Network::build(&edges, &nodes)
    }

    #[test]
    fn a_clearance_is_read_as_taxiways_in_order_and_where_to() {
        let net = airport();
        assert_eq!(parse(&net, "taxi to rwy 04L via a b").unwrap(), Clearance { via: vec!["A".into(), "B".into()], to: Destination::Runway("04L".into()) });
        assert_eq!(parse(&net, "A, B, 04L").unwrap().via, vec!["A", "B"]);
        assert_eq!(parse(&net, "C S1").unwrap().to, Destination::Stand("S1".into()));
        assert!(parse(&net, "A B").unwrap_err().contains("where to"));
        assert!(parse(&net, "A Q 04L").unwrap_err().contains("Q"));
        assert_eq!(parse(&net, "a korean air cargo (2)").unwrap(), Clearance { via: vec!["A".into()], to: Destination::Stand("KOREAN AIR CARGO (2)".into()) });
    }

    #[test]
    fn a_route_follows_the_cleared_taxiways_not_the_shortest_way() {
        let net = airport();
        let r = route(&net, Coord { x: 0.0, y: -35.0 }, &parse(&net, "A B 04L").unwrap()).unwrap();
        assert_eq!(r.legs, vec!["A", "B"]);
        assert_eq!(*r.points.last().unwrap(), Coord { x: 200.0, y: 300.0 }, "to B's holding point, not C's closer one");
        assert!((r.length_m - (5.0 + 30.0 + 200.0 + 300.0)).abs() < 1e-6);
        // Along C instead, the nearer holding point.
        let r = route(&net, Coord { x: 0.0, y: -35.0 }, &parse(&net, "C 04L").unwrap()).unwrap();
        assert_eq!(*r.points.last().unwrap(), Coord { x: 100.0, y: 150.0 });
        // Taxiways that do not connect in that order are refused.
        assert!(route(&net, Coord { x: 0.0, y: -35.0 }, &parse(&net, "B A 04L").unwrap()).is_err());
    }

    #[test]
    fn a_route_back_to_a_stand_ends_at_the_stand() {
        let net = airport();
        let r = route(&net, Coord { x: 200.0, y: 290.0 }, &parse(&net, "B A S1").unwrap()).unwrap();
        assert_eq!(r.legs, vec!["B", "A"]);
        assert_eq!(*r.points.last().unwrap(), Coord { x: 0.0, y: -30.0 });
    }
}
