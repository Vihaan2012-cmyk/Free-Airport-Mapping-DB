//! Painted lines: TaxiwayGuidanceLine, TaxiwayHoldingPosition,
//! TaxiwayIntersectionMarking, StandGuidanceLine, RunwayExitLine, and roadway lines
//! turned into ServiceRoad surfaces when OSM has no roads.

use super::{conv, Ctx};
use crate::geom::ops::{self, add, scale};
use crate::ir::LineKind;
use crate::model::codes::{catstop, source};
use crate::model::feature::opt;
use crate::model::{AmdbFeature, Layer};
use geo_types::{Coord, LineString};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sem {
    Center,
    CenterIls,
    Lane,
    RunwayHold,
    IlsHold,
    IntersectionHold,
    RoadCenter,
    Ignore,
}

/// Known apt.dat line codes. X-Plane 12 added undocumented codes; 60/62/64 were
/// verified against real scenery geometry (centreline / runway hold / ILS hold).
/// Codes that return `None` are classified geometrically by `classify_geom`.
fn semantic(code: u16) -> Option<Sem> {
    Some(match code {
        0 => Sem::Ignore,
        1 | 51 | 60 => Sem::Center,
        7 | 57 => Sem::CenterIls,
        8 | 9 | 58 | 59 => Sem::Lane,
        4 | 54 | 62 => Sem::RunwayHold,
        6 | 56 | 64 => Sem::IlsHold,
        5 | 55 | 63 => Sem::IntersectionHold,
        3 | 53 | 30 | 31 | 32 => Sem::Ignore, // edge lines
        20 | 21 | 23 | 24 | 25 => Sem::Ignore, // roadway edge / chequer
        22 => Sem::RoadCenter,
        2 | 52 => return None, // broken yellow: ICAO intermediate hold when it crosses a taxi route
        _ => return None,
    })
}

/// Geometric classification for codes without a known meaning.
fn classify_geom(pts: &LineString<f64>, edges: &[EdgeRef], runways: &[super::RwyGeom]) -> Sem {
    let len = ops::length(pts);
    if pts.0.len() < 2 || len < 1.0 {
        return Sem::Ignore;
    }
    let mid = ops::point_at(pts, len / 2.0);
    let a = pts.0[0];
    let b = *pts.0.last().unwrap();
    // Nearest routing edge to the midpoint.
    let mut best: Option<(f64, &EdgeRef)> = None;
    for e in edges {
        let d = ops::point_seg_dist(mid, e.a, e.b);
        if best.map_or(true, |(bd, _)| d < bd) {
            best = Some((d, e));
        }
    }
    let Some((d, e)) = best else { return Sem::Ignore };
    if d > 4.0 {
        return Sem::Ignore;
    }
    let ang = |p: Coord<f64>, q: Coord<f64>| (q.y - p.y).atan2(q.x - p.x).to_degrees();
    let mut diff = (ang(a, b) - ang(e.a, e.b)).abs() % 180.0;
    if diff > 90.0 {
        diff = 180.0 - diff;
    }
    if diff > 60.0 && len < 90.0 {
        let near_rwy = runways.iter().any(|r| ops::point_seg_dist(mid, r.ends[0], r.ends[1]) < 350.0);
        if near_rwy { Sem::RunwayHold } else { Sem::IntersectionHold }
    } else if diff < 30.0 && len >= 5.0 {
        Sem::Center
    } else {
        Sem::Ignore
    }
}

fn lit(light: u16) -> bool {
    matches!(light, 101 | 105 | 107 | 108)
}

struct EdgeRef {
    a: Coord<f64>,
    b: Coord<f64>,
    name: String,
    wingspan: Option<f64>,
}

fn nearest_edge_name(line: &LineString<f64>, edges: &[EdgeRef], max_d: f64) -> (Option<String>, Option<f64>) {
    // Sample a few points along the line and vote.
    let n = line.0.len();
    let samples: Vec<Coord<f64>> = if n <= 3 { line.0.clone() } else { vec![line.0[0], line.0[n / 2], line.0[n - 1]] };
    let mut votes: HashMap<&str, (usize, Option<f64>)> = HashMap::new();
    for s in samples {
        let mut best: Option<(f64, &EdgeRef)> = None;
        for e in edges {
            let d = ops::point_seg_dist(s, e.a, e.b);
            if d <= max_d && best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, e));
            }
        }
        if let Some((_, e)) = best {
            let v = votes.entry(e.name.as_str()).or_insert((0, e.wingspan));
            v.0 += 1;
        }
    }
    votes.into_iter().max_by_key(|(n, (c, _))| (*c, std::cmp::Reverse(n.to_string()))).map(|(n, (_, w))| (Some(n.to_string()), w)).unwrap_or((None, None))
}

pub fn wingspan_for_restriction(r: &str) -> Option<f64> {
    match r.to_ascii_uppercase().as_str() {
        "TAXIWAY_A" => Some(15.0),
        "TAXIWAY_B" => Some(24.0),
        "TAXIWAY_C" => Some(36.0),
        "TAXIWAY_D" => Some(52.0),
        "TAXIWAY_E" => Some(65.0),
        "TAXIWAY_F" => Some(80.0),
        _ => None,
    }
}

pub fn build(ctx: &mut Ctx) {
    let node_pos: HashMap<i64, Coord<f64>> = ctx.src.route_nodes.iter().map(|n| (n.id, ctx.p(n.pos))).collect();
    let edges: Vec<EdgeRef> = ctx
        .src
        .route_edges
        .iter()
        .filter(|e| e.restriction != "runway")
        .filter_map(|e| Some(EdgeRef { a: *node_pos.get(&e.from)?, b: *node_pos.get(&e.to)?, name: e.name.clone()?, wingspan: wingspan_for_restriction(&e.restriction) }))
        .collect();
    let all_edges: Vec<EdgeRef> = ctx
        .src
        .route_edges
        .iter()
        .filter(|e| e.restriction != "runway")
        .filter_map(|e| Some(EdgeRef { a: *node_pos.get(&e.from)?, b: *node_pos.get(&e.to)?, name: e.name.clone().unwrap_or_default(), wingspan: None }))
        .collect();
    let stands: Vec<(String, Coord<f64>)> = ctx.stands_local.iter().map(|s| (s.name.clone(), s.pos)).collect();
    let runway_mp = ctx.runway_mp.clone();
    let runways = ctx.runways.clone();

    // Collect coded runs from painted lines and pavement edges.
    let mut runs: Vec<(conv::CodedRun, Option<String>, &'static str)> = Vec::new();
    for l in &ctx.src.painted_lines {
        for r in conv::coded_runs(&ctx.frame, &l.ring) {
            runs.push((r, l.name.clone(), l.source));
        }
    }
    for pv in &ctx.src.pavements {
        for r in conv::coded_runs(&ctx.frame, &pv.outer) {
            runs.push((r, None, pv.source));
        }
        for h in &pv.holes {
            for r in conv::coded_runs(&ctx.frame, h) {
                runs.push((r, None, pv.source));
            }
        }
    }
    let has_xp_lines = runs.iter().any(|(r, _, _)| matches!(semantic(r.line), Some(Sem::Center) | Some(Sem::CenterIls)));

    let mut road_lines: Vec<LineString<f64>> = Vec::new();
    let mut holds: Vec<LineString<f64>> = Vec::new();
    let mut centerlines: Vec<(conv::CodedRun, Option<String>, &'static str, Sem)> = Vec::new();
    // Pass 1: everything except centrelines, so hold lines are known before exits are cut.
    for (run, desc, src) in runs {
        let sem = semantic(run.line).unwrap_or_else(|| classify_geom(&run.pts, &all_edges, &runways));
        match sem {
            Sem::Ignore => {}
            Sem::RoadCenter => road_lines.push(run.pts),
            Sem::RunwayHold | Sem::IlsHold => {
                let cs = if sem == Sem::RunwayHold { catstop::CAT_I } else { catstop::CAT_II_III };
                let idrwy = nearest_runway(&run.pts, &runways, 500.0);
                let (idlin, _) = nearest_edge_name(&run.pts, &edges, 40.0);
                holds.push(run.pts.clone());
                ctx.push(AmdbFeature::new(Layer::TaxiwayHoldingPosition, run.pts).with("idlin", opt(idlin)).with("idrwy", opt(idrwy)).with("catstop", cs).with("lighting", matches!(run.light, 103 | 104)).with("status", 1).with("source", src));
            }
            Sem::IntersectionHold => {
                let (idlin, _) = nearest_edge_name(&run.pts, &edges, 40.0);
                ctx.push(AmdbFeature::new(Layer::TaxiwayIntersectionMarking, run.pts).with("idlin", opt(idlin)).with("source", src));
            }
            Sem::Center | Sem::CenterIls | Sem::Lane => centerlines.push((run, desc, src, sem)),
        }
    }
    // Pass 2: centrelines, exits extended to the first hold line.
    for (run, desc, src, sem) in centerlines {
        let lighting = lit(run.light);
        emit_centerline(ctx, run.pts, sem == Sem::Lane, sem == Sem::CenterIls, lighting, desc, src, &edges, &stands, &runway_mp, &runways, &holds);
    }

    // OSM fallback centrelines when X-Plane painted none.
    if !has_xp_lines {
        for l in ctx.src.semantic_lines.iter().filter(|l| l.kind == LineKind::TaxiCenterline) {
            let ls = ctx.frame.fwd_line(&l.pts);
            if ls.0.len() < 2 {
                continue;
            }
            let name = l.name.clone();
            emit_centerline(ctx, ls, false, false, false, name, source::OSM, &edges, &stands, &runway_mp, &runways, &[]);
        }
    }
    merge_repeated_exits(ctx);
    // OSM holding-position nodes -> short lines perpendicular to the nearest centreline.
    let centerlines: Vec<LineString<f64>> = ctx.layer(Layer::TaxiwayGuidanceLine).iter().filter_map(|f| if let geo_types::Geometry::LineString(l) = &f.geom { Some(l.clone()) } else { None }).collect();
    let holds: Vec<(Coord<f64>, LineKind, Option<String>)> = ctx.src.semantic_lines.iter().filter(|l| matches!(l.kind, LineKind::RunwayHold | LineKind::IlsHold | LineKind::IntersectionHold) && l.pts.len() == 1).map(|l| (ctx.p(l.pts[0]), l.kind, l.name.clone())).collect();
    let existing_holds: Vec<LineString<f64>> = ctx.layer(Layer::TaxiwayHoldingPosition).iter().filter_map(|f| if let geo_types::Geometry::LineString(l) = &f.geom { Some(l.clone()) } else { None }).collect();
    for (p, kind, name) in holds {
        if existing_holds.iter().any(|h| ops::point_line_dist(p, h) < 15.0) {
            continue; // X-Plane already painted this one
        }
        let dir = nearest_direction(p, &centerlines).unwrap_or(Coord { x: 1.0, y: 0.0 });
        let r = ops::right_of(dir);
        let half = 12.0;
        let ls = LineString(vec![add(p, scale(r, -half)), add(p, scale(r, half))]);
        match kind {
            LineKind::IntersectionHold => ctx.push(AmdbFeature::new(Layer::TaxiwayIntersectionMarking, ls).with("idlin", opt(name)).with("source", source::OSM)),
            _ => {
                let cs = if kind == LineKind::IlsHold { catstop::CAT_II_III } else { catstop::CAT_I };
                let idrwy = nearest_runway(&ls, &runways, 500.0);
                ctx.push(AmdbFeature::new(Layer::TaxiwayHoldingPosition, ls).with("idlin", opt(name)).with("idrwy", opt(idrwy)).with("catstop", cs).with("lighting", false).with("status", 1).with("source", source::OSM));
            }
        }
    }
    // Roadway centrelines (X-Plane) -> service roads if OSM has none.
    let has_osm_roads = ctx.src.semantic_lines.iter().any(|l| l.kind == LineKind::RoadCenter);
    if !has_osm_roads {
        for ls in road_lines {
            for p in ops::tidy_multi(&ops::buffer_line(&ls, 3.0)) {
                ctx.push(AmdbFeature::new(Layer::ServiceRoad, p).with("name", serde_json::Value::Null).with("surftype", 4).with("width", 6.0).with("bridge", false).with("source", source::XPLANE));
            }
        }
    }
}

fn nearest_direction(p: Coord<f64>, lines: &[LineString<f64>]) -> Option<Coord<f64>> {
    let mut best: Option<(f64, Coord<f64>)> = None;
    for l in lines {
        for w in l.0.windows(2) {
            let d = ops::point_seg_dist(p, w[0], w[1]);
            if best.map_or(true, |(bd, _)| d < bd) {
                let len = ops::dist(w[0], w[1]);
                if len > 0.01 {
                    best = Some((d, Coord { x: (w[1].x - w[0].x) / len, y: (w[1].y - w[0].y) / len }));
                }
            }
        }
    }
    best.filter(|(d, _)| *d < 60.0).map(|(_, u)| u)
}

/// How far either side of a runway centreline a painted line counts as on it, in metres.
const CENTRELINE_BAND_M: f64 = 5.0;
/// An end this close to the centreline starts an exit, though it stops short of the band.
const CENTRELINE_REACH_M: f64 = 8.0;
/// FlyByWire's BTV takes no exit that turns more than this from the landing direction
/// (OansBrakeToVacateSelection.selectExitFromOans), nor one starting nearer the threshold
/// than the touchdown zone (BTV_MIN_TOUCHDOWN_ZONE_DISTANCE).
const EXIT_MAX_TURN_DEG: f64 = 120.0;
const TOUCHDOWN_ZONE_M: f64 = 400.0;

/// The parts of a line inside a runway that are off its centreline, each with whether it
/// leaves from the centreline (an exit, returned centreline end first) or not.
fn off_centreline(seg: &LineString<f64>, rw: &super::RwyGeom) -> Vec<(Vec<Coord<f64>>, bool)> {
    let (e0, e1) = (rw.ends[0], rw.ends[1]);
    let len = ops::dist(e0, e1);
    if len < 1.0 || seg.0.len() < 2 {
        return Vec::new();
    }
    let u = Coord { x: (e1.x - e0.x) / len, y: (e1.y - e0.y) / len };
    // Signed distance from the centreline.
    let side = |p: Coord<f64>| (p.x - e0.x) * u.y - (p.y - e0.y) * u.x;
    let lerp = |a: Coord<f64>, b: Coord<f64>, t: f64| Coord { x: a.x + t * (b.x - a.x), y: a.y + t * (b.y - a.y) };
    let mut pieces: Vec<Vec<Coord<f64>>> = Vec::new();
    let mut cur: Vec<Coord<f64>> = Vec::new();
    for w in seg.0.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (da, db) = (side(a), side(b));
        // Where this step crosses the edges of the band.
        let mut ts = vec![0.0, 1.0];
        for level in [CENTRELINE_BAND_M, -CENTRELINE_BAND_M] {
            if (da - level) * (db - level) < 0.0 {
                ts.push((level - da) / (db - da));
            }
        }
        ts.sort_by(|x, y| x.partial_cmp(y).unwrap());
        for t in ts.windows(2) {
            if t[1] - t[0] < 1e-9 {
                continue;
            }
            let dm = da + (t[0] + t[1]) / 2.0 * (db - da);
            if dm.abs() > CENTRELINE_BAND_M {
                if cur.is_empty() {
                    cur.push(lerp(a, b, t[0]));
                }
                cur.push(lerp(a, b, t[1]));
            } else if !cur.is_empty() {
                pieces.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    let reaches = |p: Coord<f64>| side(p).abs() <= CENTRELINE_REACH_M;
    pieces
        .into_iter()
        .filter(|p| p.len() >= 2 && ops::length(&LineString(p.clone())) > 1.0)
        .filter_map(|mut p| match (reaches(p[0]), reaches(*p.last().unwrap())) {
            // Leaves the centreline and comes back to it: along the runway, not off it.
            (true, true) => None,
            (true, false) => Some((p, true)),
            (false, true) => {
                p.reverse();
                Some((p, true))
            }
            (false, false) => Some((p, false)),
        })
        .collect()
}

/// Exits closer than this along the runway, off the same side onto the same taxiway for the
/// same landings, are one exit painted twice (a lead-off and a right-angle line, say).
const SAME_EXIT_M: f64 = 60.0;

/// Keep one exit of each repeat -- the longest, which reaches furthest towards the hold
/// line -- and turn the others into the taxi lines they also are, so the map still draws
/// them but offers the exit once.
fn merge_repeated_exits(ctx: &mut Ctx) {
    let Some(mut exits) = ctx.out.remove(&Layer::RunwayExitLine) else { return };
    let line = |f: &AmdbFeature| if let geo_types::Geometry::LineString(l) = &f.geom { Some(l.clone()) } else { None };
    // (taxiway, runway, landings, side, distance along) of each exit.
    let place = |f: &AmdbFeature| -> Option<(String, String, String, bool, f64)> {
        let l = line(f)?;
        let rw = ctx.runways.iter().find(|r| Some(r.idrwy.as_str()) == f.get_str("idrwy"))?;
        let u = ops::unit_from_heading(rw.heading);
        let (s, e) = (l.0[0], *l.0.last()?);
        let along = (s.x - rw.ends[0].x) * u.x + (s.y - rw.ends[0].y) * u.y;
        let right = (e.x - rw.ends[0].x) * u.y - (e.y - rw.ends[0].y) * u.x > 0.0;
        Some((f.get_str("idlin").unwrap_or("").to_string(), rw.idrwy.clone(), f.get_str("idthr").unwrap_or("").to_string(), right, along))
    };
    exits.sort_by(|a, b| line(b).map_or(0.0, |l| ops::length(&l)).partial_cmp(&line(a).map_or(0.0, |l| ops::length(&l))).unwrap());
    let mut kept: Vec<(AmdbFeature, Option<(String, String, String, bool, f64)>)> = Vec::new();
    let mut demoted = Vec::new();
    for f in exits {
        let at = place(&f);
        let repeat = at.as_ref().is_some_and(|(n, r, t, side, along)| {
            kept.iter().any(|(_, k)| k.as_ref().is_some_and(|(kn, kr, kt, kside, kalong)| kn == n && kr == r && kt == t && kside == side && (kalong - along).abs() < SAME_EXIT_M))
        });
        if repeat {
            demoted.push(f);
        } else {
            kept.push((f, at));
        }
    }
    ctx.out.insert(Layer::RunwayExitLine, kept.into_iter().map(|(f, _)| f).collect());
    for f in demoted {
        let Some(l) = line(&f) else { continue };
        let len = ops::length(&l);
        ctx.push(
            AmdbFeature::new(Layer::TaxiwayGuidanceLine, l)
                .with("idlin", f.props.get("idlin").cloned().unwrap_or(serde_json::Value::Null))
                .with("color", 1)
                .with("style", 1)
                .with("direc", 1)
                .with("lighting", f.props.get("lighting").cloned().unwrap_or(false.into()))
                .with("ilscrit", false)
                .with("wingspan", serde_json::Value::Null)
                .with("length", super::runway::round1(len))
                .with("source", f.get_str("source").unwrap_or(source::DERIVED).to_string()),
        );
    }
}

/// The runway ends whose landings can take an exit (centreline end first), as FlyByWire's
/// BTV would accept it: turning off no more than `EXIT_MAX_TURN_DEG` from the landing
/// direction, and starting beyond the touchdown zone -- measured along the landing, so an
/// exit behind a displaced threshold is not one.
fn exit_thresholds(exit: &LineString<f64>, rw: &super::RwyGeom) -> Vec<String> {
    let start = exit.0[0];
    let len = ops::length(exit);
    let lead = ops::heading_deg(start, ops::point_at(exit, len.min(20.0)));
    (0..2)
        .filter(|&i| {
            let landing = if i == 0 { rw.heading } else { rw.heading + 180.0 };
            let turn = ((lead - landing).rem_euclid(360.0) + 180.0).rem_euclid(360.0) - 180.0;
            let u = ops::unit_from_heading(landing);
            let ahead = (start.x - rw.thresholds[i].x) * u.x + (start.y - rw.thresholds[i].y) * u.y;
            turn.abs() <= EXIT_MAX_TURN_DEG && ahead >= TOUCHDOWN_ZONE_M
        })
        .map(|i| rw.idents[i].clone())
        .collect()
}

fn nearest_runway(line: &LineString<f64>, runways: &[super::RwyGeom], max_d: f64) -> Option<String> {
    let mid = ops::point_at(line, ops::length(line) / 2.0);
    runways
        .iter()
        .map(|r| (ops::point_seg_dist(mid, r.ends[0], r.ends[1]), &r.idrwy))
        .filter(|(d, _)| *d <= max_d)
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
        .map(|(_, id)| id.clone())
}

#[allow(clippy::too_many_arguments)]
fn emit_centerline(
    ctx: &mut Ctx,
    pts: LineString<f64>,
    lane: bool,
    ils_critical: bool,
    lighting: bool,
    desc: Option<String>,
    src: &'static str,
    edges: &[EdgeRef],
    stands: &[(String, Coord<f64>)],
    runway_mp: &geo_types::MultiPolygon<f64>,
    runways: &[super::RwyGeom],
    holds: &[LineString<f64>],
) {
    // A painted line inside a runway is an exit where it leaves the runway centreline. The
    // stretch along the centreline is the runway's own and is dropped, a line across the
    // runway is an exit to each side, and one that never reaches the centreline stays a
    // taxi line. Each exit is extended along the same painted line beyond the runway edge
    // up to the first holding position, or 300 m, as Navigraph captures it.
    let (inside, mut outside) = if runway_mp.0.is_empty() { (vec![], vec![pts]) } else { ops::split_by(runway_mp, &pts) };
    let mut exits: Vec<(Vec<Coord<f64>>, &super::RwyGeom)> = Vec::new();
    for seg in inside {
        let rw = nearest_runway(&seg, runways, 200.0).and_then(|id| runways.iter().find(|r| r.idrwy == id));
        let Some(rw) = rw else {
            outside.push(seg);
            continue;
        };
        for (piece, from_centreline) in off_centreline(&seg, rw) {
            if from_centreline {
                exits.push((piece, rw));
            } else {
                outside.push(LineString(piece));
            }
        }
    }
    let mut extended: Vec<(LineString<f64>, &super::RwyGeom)> = Vec::new();
    for (mut exit, rw) in exits {
        let edge_pt = *exit.last().unwrap();
        // Find the outside piece continuing from the runway edge.
        let idx = outside.iter().position(|o| ops::dist(o.0[0], edge_pt) < 1.0 || ops::dist(*o.0.last().unwrap(), edge_pt) < 1.0);
        if let Some(i) = idx {
            let o = outside.remove(i);
            let mut walk: Vec<Coord<f64>> = if ops::dist(o.0[0], edge_pt) < 1.0 { o.0.clone() } else { o.0.iter().rev().copied().collect() };
            let mut acc = 0.0;
            let mut cut: Option<(usize, Coord<f64>)> = None;
            'seg: for k in 1..walk.len() {
                let (a, b) = (walk[k - 1], walk[k]);
                for h in holds {
                    for w in h.0.windows(2) {
                        if let Some(q) = ops::seg_intersection(geo_types::Line::new(a, b), geo_types::Line::new(w[0], w[1])) {
                            cut = Some((k, q));
                            break 'seg;
                        }
                    }
                }
                let l = ops::dist(a, b);
                if acc + l > 300.0 {
                    let t = (300.0 - acc) / l;
                    cut = Some((k, Coord { x: a.x + t * (b.x - a.x), y: a.y + t * (b.y - a.y) }));
                    break;
                }
                acc += l;
            }
            let remainder: Vec<Coord<f64>> = match cut {
                Some((k, q)) => {
                    let rest: Vec<Coord<f64>> = std::iter::once(q).chain(walk[k..].iter().copied()).collect();
                    walk.truncate(k);
                    walk.push(q);
                    rest
                }
                None => vec![],
            };
            exit.extend(walk.into_iter().skip(1));
            if remainder.len() >= 2 && ops::length(&LineString(remainder.clone())) > 1.0 {
                outside.push(LineString(remainder));
            }
        }
        // FlyByWire reads an exit's direction off its first two points, so no repeats.
        exit.dedup_by(|a, b| ops::dist(*a, *b) < 0.5);
        if exit.len() >= 2 {
            extended.push((LineString(exit), rw));
        }
    }
    for (seg, rw) in extended {
        let idthr = exit_thresholds(&seg, rw);
        if idthr.is_empty() {
            // No landing can take it (it points back up the runway, or starts inside the
            // touchdown zone): a runway entry, which is a taxi line.
            outside.push(seg);
            continue;
        }
        let (idlin, _) = nearest_edge_name(&seg, edges, 60.0);
        let a = ops::heading_deg(seg.0[0], *seg.0.last().unwrap());
        let mut diff = (a - rw.heading).abs() % 180.0;
        if diff > 90.0 {
            diff = 180.0 - diff;
        }
        let exittype = if (20.0..=50.0).contains(&diff) { 2 } else { 1 };
        ctx.push(
            AmdbFeature::new(Layer::RunwayExitLine, seg)
                .with("idlin", opt(idlin))
                .with("idrwy", rw.idrwy.clone())
                .with("idthr", idthr.join("."))
                .with("exittype", exittype)
                .with("lighting", lighting)
                .with("source", src),
        );
    }
    for seg in outside {
        let len = ops::length(&seg);
        let first = seg.0[0];
        let last = *seg.0.last().unwrap();
        // Stand lead-in: short line ending at a stand.
        let stand = stands.iter().filter(|(_, p)| ops::dist(*p, first).min(ops::dist(*p, last)) < 20.0).min_by(|a, b| ops::dist(a.1, last).min(ops::dist(a.1, first)).partial_cmp(&ops::dist(b.1, last).min(ops::dist(b.1, first))).unwrap());
        if let (Some((idstd, _)), true) = (stand, len < 250.0) {
            ctx.push(AmdbFeature::new(Layer::StandGuidanceLine, seg).with("idstd", idstd.clone()).with("idlin", serde_json::Value::Null).with("color", 1).with("style", 1).with("source", src));
            continue;
        }
        let (idlin, wingspan) = nearest_edge_name(&seg, edges, 40.0);
        let idlin = idlin.or_else(|| desc.clone().filter(|d| d.len() <= 5 && !d.to_ascii_lowercase().contains("line")));
        ctx.push(
            AmdbFeature::new(Layer::TaxiwayGuidanceLine, seg)
                .with("idlin", opt(idlin))
                .with("color", 1)
                .with("style", if lane { 2 } else { 1 })
                .with("direc", 1)
                .with("lighting", lighting)
                .with("ilscrit", ils_critical)
                .with("wingspan", opt(wingspan))
                .with("length", super::runway::round1(len))
                .with("source", src),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::RwyGeom;

    /// A 3 km runway 45 m wide along the y axis: 36 at y=0 (landing north), 18 at y=3000.
    fn runway() -> RwyGeom {
        let (e0, e1) = (Coord { x: 0.0, y: 0.0 }, Coord { x: 0.0, y: 3000.0 });
        RwyGeom {
            idrwy: "36.18".into(),
            idents: ["36".into(), "18".into()],
            ends: [e0, e1],
            thresholds: [e0, e1],
            width: 45.0,
            length: 3000.0,
            heading: 0.0,
            poly: ops::rect_between(e0, e1, 45.0),
            surface: 1,
            src_index: 0,
        }
    }

    fn line(pts: &[(f64, f64)]) -> LineString<f64> {
        LineString(pts.iter().map(|&(x, y)| Coord { x, y }).collect())
    }

    #[test]
    fn a_taxi_route_along_the_centreline_is_not_an_exit() {
        assert!(off_centreline(&line(&[(0.5, 1000.0), (-0.5, 1120.0)]), &runway()).is_empty());
    }

    #[test]
    fn a_lead_off_is_one_exit_starting_where_it_leaves_the_centreline() {
        // Along the centreline for 100 m, then off to the east.
        let got = off_centreline(&line(&[(0.0, 900.0), (0.0, 1000.0), (10.0, 1030.0), (22.0, 1050.0)]), &runway());
        assert_eq!(got.len(), 1);
        let (p, exit) = &got[0];
        assert!(*exit);
        assert!((p[0].x - 5.0).abs() < 1e-6 && p[0].y > 1000.0, "starts at the band edge, beyond the along-runway part: {:?}", p[0]);
        assert_eq!(*p.last().unwrap(), Coord { x: 22.0, y: 1050.0 });
    }

    #[test]
    fn a_crossing_is_an_exit_to_each_side() {
        let got = off_centreline(&line(&[(-22.0, 1500.0), (22.0, 1500.0)]), &runway());
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|(p, exit)| *exit && p[0].x.abs() == CENTRELINE_BAND_M && p.last().unwrap().x.abs() == 22.0));
    }

    #[test]
    fn a_line_that_never_reaches_the_centreline_is_a_taxi_line() {
        let got = off_centreline(&line(&[(15.0, 10.0), (22.0, 40.0)]), &runway());
        assert_eq!(got, vec![(vec![Coord { x: 15.0, y: 10.0 }, Coord { x: 22.0, y: 40.0 }], false)]);
    }

    #[test]
    fn exits_belong_to_the_landings_that_can_take_them() {
        let rw = runway();
        // Right-angle exit mid-runway: either direction.
        assert_eq!(exit_thresholds(&line(&[(5.0, 1500.0), (80.0, 1500.0)]), &rw), vec!["36", "18"]);
        // High-speed exit angled north-east: landing north only.
        assert_eq!(exit_thresholds(&line(&[(5.0, 1500.0), (40.0, 1560.0)]), &rw), vec!["36"]);
        // The same, but angled back south-east: landing south only.
        assert_eq!(exit_thresholds(&line(&[(5.0, 1500.0), (40.0, 1440.0)]), &rw), vec!["18"]);
        // Inside 36's touchdown zone: only 18's landings, 2.8 km on.
        assert_eq!(exit_thresholds(&line(&[(5.0, 200.0), (80.0, 200.0)]), &rw), vec!["18"]);
        // Behind a threshold displaced 1 km: not that runway end's, however far from it.
        let mut displaced = runway();
        displaced.thresholds[0] = Coord { x: 0.0, y: 1000.0 };
        assert_eq!(exit_thresholds(&line(&[(5.0, 100.0), (80.0, 100.0)]), &displaced), vec!["18"]);
    }
}
