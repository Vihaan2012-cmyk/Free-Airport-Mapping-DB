//! OpenStreetMap from a downloaded extract (`.osm.pbf`, a region from Geofabrik or the
//! whole planet) instead of from the servers, for building many airports at once.
//!
//! The servers are not meant for it: the map API's policy forbids bulk downloading, and
//! the public Overpass instances ask for no more than about ten thousand queries a day.
//! One read of an extract serves every airport inside it, with no requests at all.
//!
//! The file is read three times, each read decoding its blocks in parallel and handling
//! them in file order (an extract is sorted: nodes, then ways, then relations):
//!
//! 1. Nodes inside any airport's box are kept (a coarse grid of the boxes turns nearly
//!    every other node away with one bit test), and the tagged nodes among them; then the
//!    ways with the tags the airport data uses that touch those nodes, the ids of every
//!    other way that touches them, and the relations with those tags that have one of
//!    those ways as a member.
//! 2. The member ways those relations need (outlines of multipolygons, usually untagged).
//! 3. Where a way runs out of an airport's box, the coordinates of its nodes outside it,
//!    so it comes whole, as the map API gives it.
//!
//! What is kept is what the Overpass query asks for (see `overpass::query`), and each
//! airport's share is saved where a download of it would be (`osm/extract/<ICAO>.json`,
//! in the map API's store format), so building finds it and asks nothing of the servers.

use super::elements::{Member, Relation, Store, Tags, Way};
use crate::cache::Cache;
use anyhow::{anyhow, Context, Result};
use geo_types::Coord;
use osmpbf::{BlobReader, BlobType, PrimitiveBlock, RelMemberType};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// (south, west, north, east) in degrees.
pub type BBox = (f64, f64, f64, f64);

/// An airport to extract, and the box its OpenStreetMap data comes from.
pub struct Target {
    pub icao: String,
    pub bbox: BBox,
}

#[derive(Debug, Default)]
pub struct Stats {
    pub airports: usize,
    /// Airports the file has nothing in the box of: outside its area, most likely. Nothing
    /// is saved for them, and building downloads their OSM as usual.
    pub outside_file: usize,
    pub nodes: usize,
    pub ways: usize,
    pub relations: usize,
}

/// Where an airport's share of an extract is saved.
pub fn cache_key(icao: &str) -> String {
    format!("osm/extract/{}.json", icao.to_uppercase())
}

// ---- which elements are wanted: the same as the Overpass query ------------------------

fn tag<'a>(tags: &'a [(&str, &str)], k: &str) -> Option<&'a str> {
    tags.iter().find(|(key, _)| *key == k).map(|(_, v)| *v)
}

const MAN_MADE: [&str; 9] = ["tower", "mast", "chimney", "antenna", "storage_tank", "silo", "communications_tower", "water_tower", "lighthouse"];

/// A node the airport data uses on its own (stands, holding points, lights, masts).
fn wanted_node(tags: &[(&str, &str)]) -> bool {
    match tag(tags, "aeroway") {
        Some("navigationaid") => tag(tags, "navigationaid").is_some_and(|n| ["papi", "vasi", "als", "reil"].iter().any(|k| n.contains(k))),
        Some(_) => true,
        None => tag(tags, "man_made").is_some_and(|m| MAN_MADE.contains(&m)),
    }
}

/// A way or relation the airport data uses.
fn wanted_area_or_line(tags: &[(&str, &str)], is_way: bool) -> bool {
    if tag(tags, "aeroway").is_some_and(|a| a != "navigationaid") || tags.iter().any(|(k, _)| *k == "building" || *k == "water" || *k == "construction" || *k == "deicing") {
        return true;
    }
    if tag(tags, "man_made").is_some_and(|m| MAN_MADE.contains(&m)) || tag(tags, "natural") == Some("water") {
        return true;
    }
    if tag(tags, "landuse").is_some_and(|l| ["construction", "reservoir", "basin"].contains(&l)) {
        return true;
    }
    if !is_way {
        return false;
    }
    if tag(tags, "barrier").is_some_and(|b| b == "fence" || b == "wall") || tag(tags, "power").is_some_and(|p| p == "line" || p == "minor_line") {
        return true;
    }
    // Airside service roads only: no parking aisles, no public road network.
    tag(tags, "highway").is_some_and(|h| ["service", "unclassified", "living_street", "track"].contains(&h))
        && !tag(tags, "service").is_some_and(|s| ["parking_aisle", "driveway", "drive-through", "emergency_access"].iter().any(|x| s.contains(x)))
}

fn owned(tags: &[(&str, &str)]) -> Tags {
    tags.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ---- where the boxes are ----------------------------------------------------------------

/// Grid cells of 0.05 degrees over the world: a bit for every cell a box touches, and for
/// those cells the boxes that touch them.
struct Grid {
    bits: Vec<u64>,
    cells: HashMap<u32, Vec<u32>>,
    boxes: Vec<BBox>,
}

const CELL: f64 = 0.05;
const LAT_CELLS: i64 = (180.0 / CELL) as i64;
const LON_CELLS: i64 = (360.0 / CELL) as i64;

impl Grid {
    fn new(boxes: Vec<BBox>) -> Grid {
        let mut g = Grid { bits: vec![0; (LAT_CELLS * LON_CELLS) as usize / 64 + 1], cells: HashMap::new(), boxes };
        for (i, &(s, w, n, e)) in g.boxes.clone().iter().enumerate() {
            // A box across the antimeridian is two.
            let spans: Vec<(f64, f64)> = if w <= e { vec![(w, e)] } else { vec![(w, 180.0), (-180.0, e)] };
            for (w, e) in spans {
                for la in Self::lat_cell(s)..=Self::lat_cell(n) {
                    for lo in Self::lon_cell(w)..=Self::lon_cell(e) {
                        let c = (la * LON_CELLS + lo) as u32;
                        g.bits[c as usize / 64] |= 1 << (c % 64);
                        g.cells.entry(c).or_default().push(i as u32);
                    }
                }
            }
        }
        g
    }

    fn lat_cell(lat: f64) -> i64 {
        (((lat + 90.0) / CELL) as i64).clamp(0, LAT_CELLS - 1)
    }

    fn lon_cell(lon: f64) -> i64 {
        (((lon + 180.0) / CELL) as i64).clamp(0, LON_CELLS - 1)
    }

    fn cell(lat: f64, lon: f64) -> u32 {
        (Self::lat_cell(lat) * LON_CELLS + Self::lon_cell(lon)) as u32
    }

    /// Whether a point is in any box, and if so which.
    fn boxes_at(&self, lat: f64, lon: f64) -> impl Iterator<Item = u32> + '_ {
        let c = Self::cell(lat, lon);
        let hit = self.bits[c as usize / 64] & (1 << (c % 64)) != 0;
        hit.then(|| self.cells.get(&c)).flatten().into_iter().flatten().copied().filter(move |&i| {
            let (s, w, n, e) = self.boxes[i as usize];
            lat >= s && lat <= n && if w <= e { lon >= w && lon <= e } else { lon >= w || lon <= e }
        })
    }

    fn in_any(&self, lat: f64, lon: f64) -> bool {
        self.boxes_at(lat, lon).next().is_some()
    }
}

/// Nodes inside the boxes, sorted by id, with a bit per 64 ids in front of them: most
/// ways touch none of these nodes, and the bit says so without a search.
#[derive(Default)]
struct Inside {
    nodes: Vec<(i64, i32, i32)>,
    bits: Vec<u64>,
}

impl Inside {
    fn seal(&mut self) {
        if !self.nodes.windows(2).all(|w| w[0].0 < w[1].0) {
            self.nodes.sort_unstable_by_key(|n| n.0);
            self.nodes.dedup_by_key(|n| n.0);
        }
        let max = self.nodes.last().map_or(0, |n| n.0.max(0) as u64 >> 6);
        self.bits = vec![0; max as usize / 64 + 1];
        for n in &self.nodes {
            let b = (n.0.max(0) as u64) >> 6;
            self.bits[b as usize / 64] |= 1 << (b % 64);
        }
    }

    fn get(&self, id: i64) -> Option<(i32, i32)> {
        let b = (id.max(0) as u64) >> 6;
        if self.bits.get(b as usize / 64).is_none_or(|w| w & (1 << (b % 64)) == 0) {
            return None;
        }
        self.nodes.binary_search_by_key(&id, |n| n.0).ok().map(|i| (self.nodes[i].1, self.nodes[i].2))
    }
}

fn deg(e7: i32) -> f64 {
    e7 as f64 * 1e-7
}

// ---- reading the file ----------------------------------------------------------------------

/// Blocks decoded at a time: enough to keep every core busy, few enough to hold in memory.
const BATCH: usize = 128;

/// A decoded block and where in the file it starts.
struct Block {
    offset: u64,
    data: PrimitiveBlock,
}

fn decode(pbf: &Path, blobs: Vec<osmpbf::Blob>) -> Result<Vec<Block>> {
    blobs
        .into_par_iter()
        .map(|b| Ok(Block { offset: b.offset().map_or(0, |o| o.0), data: b.to_primitiveblock().map_err(|e| anyhow!("{}: {e}", pbf.display()))? }))
        .collect()
}

/// Every data block of the file in order, decoded in parallel a batch at a time. `f` says
/// whether to go on.
fn blocks(pbf: &Path, mut f: impl FnMut(&[Block]) -> Result<bool>) -> Result<()> {
    let reader = BlobReader::seekable_from_path(pbf).with_context(|| format!("open {}", pbf.display()))?;
    let mut batch = Vec::with_capacity(BATCH);
    for blob in reader {
        let blob = blob.map_err(|e| anyhow!("{}: {e}", pbf.display()))?;
        if blob.get_type() != BlobType::OsmData {
            continue;
        }
        batch.push(blob);
        if batch.len() == BATCH && !f(&decode(pbf, std::mem::take(&mut batch))?)? {
            return Ok(());
        }
    }
    if !batch.is_empty() {
        f(&decode(pbf, batch)?)?;
    }
    Ok(())
}

/// Only the blocks starting at `offsets` (in file order), decoded in parallel a batch at
/// a time: the later passes need a few blocks of the file, not all of it.
fn blocks_at(pbf: &Path, offsets: &[u64], mut f: impl FnMut(&[Block])) -> Result<()> {
    let mut reader = BlobReader::seekable_from_path(pbf).with_context(|| format!("open {}", pbf.display()))?;
    for chunk in offsets.chunks(BATCH) {
        let blobs = chunk.iter().map(|&o| reader.blob_from_offset(osmpbf::ByteOffset(o)).map_err(|e| anyhow!("{}: {e}", pbf.display()))).collect::<Result<Vec<_>>>()?;
        f(&decode(pbf, blobs)?);
    }
    Ok(())
}

/// Where each block of one kind is, and the ids it holds (a sorted file's blocks hold
/// consecutive ranges).
#[derive(Default)]
struct BlockIndex(Vec<(u64, i64, i64)>);

impl BlockIndex {
    /// The blocks holding any of `ids` (sorted).
    fn holding(&self, ids: &[i64]) -> Vec<u64> {
        self.0
            .iter()
            .filter(|&&(_, lo, hi)| {
                let i = ids.partition_point(|&x| x < lo);
                i < ids.len() && ids[i] <= hi
            })
            .map(|&(o, _, _)| o)
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Nodes,
    Ways,
    Relations,
}

/// What a block of a sorted file holds (a block holds one kind).
fn kind(b: &PrimitiveBlock) -> Option<Kind> {
    let g = b.groups().next()?;
    if g.dense_nodes().next().is_some() || g.nodes().len() > 0 {
        Some(Kind::Nodes)
    } else if g.ways().len() > 0 {
        Some(Kind::Ways)
    } else if g.relations().len() > 0 {
        Some(Kind::Relations)
    } else {
        None
    }
}

struct KeptWay {
    id: i64,
    refs: Vec<i64>,
    tags: Tags,
}

struct KeptRelation {
    id: i64,
    members: Vec<Member>,
    tags: Tags,
}

/// Fill the OSM cache for `targets` from the extract at `pbf`. An airport the file has no
/// node in the box of is left alone: it is most likely outside the file's area (a region
/// extract), and an empty store saved for it would build it without OSM for good. `log`
/// hears how it goes.
pub fn fill_cache(pbf: &Path, targets: &[Target], cache: &Cache, log: &dyn Fn(&str)) -> Result<Stats> {
    let grid = Grid::new(targets.iter().map(|t| t.bbox).collect());
    let touched: Vec<std::sync::atomic::AtomicBool> = (0..targets.len()).map(|_| std::sync::atomic::AtomicBool::new(false)).collect();
    let mut inside = Inside::default();
    let mut tagged_nodes: Vec<(i64, i32, i32, Tags)> = Vec::new();
    let mut ways: Vec<KeptWay> = Vec::new();
    let mut touching: HashSet<i64> = HashSet::new();
    let mut relations: Vec<KeptRelation> = Vec::new();
    let mut last = Kind::Nodes;
    let mut sealed = false;
    let t0 = std::time::Instant::now();
    let mut seen_blocks = 0usize;

    // Pass 1: nodes in the boxes, the ways that touch them, the relations that use those;
    // and where every node and way block is, with the ids it holds, for the later passes.
    let mut node_blocks = BlockIndex::default();
    let mut way_blocks = BlockIndex::default();
    blocks(pbf, |batch| {
        for b in batch {
            if let Some(k) = kind(&b.data) {
                if k < last {
                    return Err(anyhow!("{} is not sorted (nodes, then ways, then relations): sort it with `osmium sort` first", pbf.display()));
                }
                last = k;
            }
        }
        type NodePart = (Vec<(i64, i32, i32)>, Vec<(i64, i32, i32, Tags)>, (u64, i64, i64));
        let node_parts: Vec<NodePart> = batch
            .par_iter()
            .filter(|b| kind(&b.data) == Some(Kind::Nodes))
            .map(|blk| {
                let (mut ins, mut tagged) = (Vec::new(), Vec::new());
                let (mut lo, mut hi) = (i64::MAX, i64::MIN);
                // Inside a box, and which: an airport with no node inside is not in the file.
                let mark = |lat: i32, lon: i32| -> bool {
                    let mut any = false;
                    for a in grid.boxes_at(deg(lat), deg(lon)) {
                        any = true;
                        touched[a as usize].store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    any
                };
                for g in blk.data.groups() {
                    for n in g.dense_nodes() {
                        let (id, lat, lon) = (n.id(), n.decimicro_lat(), n.decimicro_lon());
                        lo = lo.min(id);
                        hi = hi.max(id);
                        if mark(lat, lon) {
                            ins.push((id, lat, lon));
                            let t: Vec<(&str, &str)> = n.tags().collect();
                            if !t.is_empty() && wanted_node(&t) {
                                tagged.push((id, lat, lon, owned(&t)));
                            }
                        }
                    }
                    for n in g.nodes() {
                        let (id, lat, lon) = (n.id(), n.decimicro_lat(), n.decimicro_lon());
                        lo = lo.min(id);
                        hi = hi.max(id);
                        if mark(lat, lon) {
                            ins.push((id, lat, lon));
                            let t: Vec<(&str, &str)> = n.tags().collect();
                            if !t.is_empty() && wanted_node(&t) {
                                tagged.push((id, lat, lon, owned(&t)));
                            }
                        }
                    }
                }
                (ins, tagged, (blk.offset, lo, hi))
            })
            .collect();
        for (ins, tagged, at) in node_parts {
            inside.nodes.extend(ins);
            tagged_nodes.extend(tagged);
            node_blocks.0.push(at);
        }
        if !sealed && batch.iter().any(|b| kind(&b.data).is_some_and(|k| k > Kind::Nodes)) {
            inside.seal();
            sealed = true;
            log(&format!("{} nodes inside the airports' boxes, in {}", inside.nodes.len(), crate::term::human_secs(t0.elapsed().as_secs_f64())));
        }
        type WayPart = (Vec<KeptWay>, Vec<i64>, (u64, i64, i64));
        let way_parts: Vec<WayPart> = batch
            .par_iter()
            .filter(|b| kind(&b.data) == Some(Kind::Ways))
            .map(|blk| {
                let (mut kept, mut touch) = (Vec::new(), Vec::new());
                let (mut lo, mut hi) = (i64::MAX, i64::MIN);
                for w in blk.data.groups().flat_map(|g| g.ways()) {
                    lo = lo.min(w.id());
                    hi = hi.max(w.id());
                    // Nearly every way in a file touches no airport: find that out from the
                    // refs as they are decoded, without collecting them.
                    if !w.refs().any(|r| inside.get(r).is_some()) {
                        continue;
                    }
                    let refs: Vec<i64> = w.refs().collect();
                    let t: Vec<(&str, &str)> = w.tags().collect();
                    if wanted_area_or_line(&t, true) {
                        kept.push(KeptWay { id: w.id(), refs, tags: owned(&t) });
                    } else {
                        touch.push(w.id());
                    }
                }
                (kept, touch, (blk.offset, lo, hi))
            })
            .collect();
        for (kept, touch, at) in way_parts {
            touching.extend(kept.iter().map(|w| w.id));
            touching.extend(touch);
            ways.extend(kept);
            way_blocks.0.push(at);
        }
        let rel_parts: Vec<Vec<KeptRelation>> = batch
            .par_iter()
            .filter(|b| kind(&b.data) == Some(Kind::Relations))
            .map(|blk| {
                let mut kept = Vec::new();
                for r in blk.data.groups().flat_map(|g| g.relations()) {
                    let t: Vec<(&str, &str)> = r.tags().collect();
                    if !wanted_area_or_line(&t, false) {
                        continue;
                    }
                    let members: Vec<Member> = r
                        .members()
                        .map(|m| Member {
                            kind: match m.member_type {
                                RelMemberType::Node => 'n',
                                RelMemberType::Way => 'w',
                                RelMemberType::Relation => 'r',
                            },
                            id: m.member_id,
                            role: m.role().unwrap_or("").to_string(),
                        })
                        .collect();
                    if members.iter().any(|m| m.kind == 'w' && touching.contains(&m.id)) {
                        kept.push(KeptRelation { id: r.id(), members, tags: owned(&t) });
                    }
                }
                kept
            })
            .collect();
        relations.extend(rel_parts.into_iter().flatten());
        seen_blocks += batch.len();
        if seen_blocks % (BATCH * 40) == 0 {
            log(&format!("read {seen_blocks} blocks: {} ways and {} relations kept so far", ways.len(), relations.len()));
        }
        Ok(true)
    })?;
    if !sealed {
        inside.seal();
    }
    drop(touching);
    log(&format!("first read done in {}: {} ways, {} relations", crate::term::human_secs(t0.elapsed().as_secs_f64()), ways.len(), relations.len()));

    // Pass 2: the member ways the kept relations need that were not kept for their own
    // tags, from the way blocks that hold their ids only.
    let t2 = std::time::Instant::now();
    let have: HashSet<i64> = ways.iter().map(|w| w.id).collect();
    let mut members: Vec<i64> = relations.iter().flat_map(|r| r.members.iter().filter(|m| m.kind == 'w').map(|m| m.id)).filter(|id| !have.contains(id)).collect();
    drop(have);
    members.sort_unstable();
    members.dedup();
    if !members.is_empty() {
        let at = way_blocks.holding(&members);
        let found_before = ways.len();
        blocks_at(pbf, &at, |batch| {
            let found: Vec<KeptWay> = batch
                .par_iter()
                .flat_map_iter(|blk| {
                    blk.data
                        .groups()
                        .flat_map(|g| g.ways())
                        .filter(|w| members.binary_search(&w.id()).is_ok())
                        .map(|w| KeptWay { id: w.id(), refs: w.refs().collect(), tags: owned(&w.tags().collect::<Vec<_>>()) })
                        .collect::<Vec<_>>()
                })
                .collect();
            ways.extend(found);
        })?;
        log(&format!(
            "member ways: {} of {} found, from {} of {} way blocks, in {}",
            ways.len() - found_before,
            members.len(),
            at.len(),
            way_blocks.0.len(),
            crate::term::human_secs(t2.elapsed().as_secs_f64())
        ));
    }

    // Pass 3: nodes of kept ways that lie outside every box, from the node blocks that
    // hold their ids only.
    let t3 = std::time::Instant::now();
    let mut outside: HashMap<i64, (i32, i32)> = HashMap::new();
    let mut missing: Vec<i64> = ways.iter().flat_map(|w| w.refs.iter().copied()).filter(|&r| inside.get(r).is_none()).collect();
    missing.sort_unstable();
    missing.dedup();
    if !missing.is_empty() {
        let at = node_blocks.holding(&missing);
        blocks_at(pbf, &at, |batch| {
            let found: Vec<(i64, i32, i32)> = batch
                .par_iter()
                .flat_map_iter(|blk| {
                    let mut v = Vec::new();
                    for g in blk.data.groups() {
                        v.extend(g.dense_nodes().filter(|n| missing.binary_search(&n.id()).is_ok()).map(|n| (n.id(), n.decimicro_lat(), n.decimicro_lon())));
                        v.extend(g.nodes().filter(|n| missing.binary_search(&n.id()).is_ok()).map(|n| (n.id(), n.decimicro_lat(), n.decimicro_lon())));
                    }
                    v
                })
                .collect();
            outside.extend(found.into_iter().map(|(id, lat, lon)| (id, (lat, lon))));
        })?;
        log(&format!(
            "nodes outside the boxes: {} of {} found, from {} of {} node blocks, in {}",
            outside.len(),
            missing.len(),
            at.len(),
            node_blocks.0.len(),
            crate::term::human_secs(t3.elapsed().as_secs_f64())
        ));
    }

    // Each airport's share: the ways with a node in its box, the relations using those,
    // and its tagged nodes; then every node those ways need.
    let coord = |id: i64| inside.get(id).or_else(|| outside.get(&id).copied());
    let mut per_way: Vec<Vec<u32>> = Vec::with_capacity(ways.len());
    let mut way_index: HashMap<i64, usize> = HashMap::with_capacity(ways.len());
    for (i, w) in ways.iter().enumerate() {
        let mut airports: Vec<u32> = w.refs.iter().filter_map(|&r| inside.get(r)).flat_map(|(la, lo)| grid.boxes_at(deg(la), deg(lo)).collect::<Vec<_>>()).collect();
        airports.sort_unstable();
        airports.dedup();
        per_way.push(airports);
        way_index.insert(w.id, i);
    }
    let mut airport_ways: Vec<Vec<usize>> = vec![Vec::new(); targets.len()];
    for (i, airports) in per_way.iter().enumerate() {
        for &a in airports {
            airport_ways[a as usize].push(i);
        }
    }
    let mut airport_rels: Vec<Vec<usize>> = vec![Vec::new(); targets.len()];
    for (ri, r) in relations.iter().enumerate() {
        let mut airports: Vec<u32> = r.members.iter().filter(|m| m.kind == 'w').filter_map(|m| way_index.get(&m.id)).flat_map(|&i| per_way[i].iter().copied()).collect();
        airports.sort_unstable();
        airports.dedup();
        for a in airports {
            airport_rels[a as usize].push(ri);
            // A member way that touches only other parts of the relation still belongs to it.
            for m in r.members.iter().filter(|m| m.kind == 'w') {
                if let Some(&wi) = way_index.get(&m.id) {
                    if !airport_ways[a as usize].contains(&wi) {
                        airport_ways[a as usize].push(wi);
                    }
                }
            }
        }
    }
    let mut airport_nodes: Vec<Vec<usize>> = vec![Vec::new(); targets.len()];
    for (ni, n) in tagged_nodes.iter().enumerate() {
        for a in grid.boxes_at(deg(n.1), deg(n.2)) {
            airport_nodes[a as usize].push(ni);
        }
    }

    let t4 = std::time::Instant::now();
    let outside_file = touched.iter().filter(|t| !t.load(std::sync::atomic::Ordering::Relaxed)).count();
    let stats = Stats { airports: targets.len() - outside_file, outside_file, nodes: inside.nodes.len() + outside.len(), ways: ways.len(), relations: relations.len() };
    targets.par_iter().enumerate().try_for_each(|(a, t)| -> Result<()> {
        if !touched[a].load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        let mut st = Store::default();
        for &wi in &airport_ways[a] {
            let w = &ways[wi];
            for &r in &w.refs {
                if let Some((la, lo)) = coord(r) {
                    st.nodes.insert(r, Coord { x: deg(lo), y: deg(la) });
                }
            }
            st.ways.insert(w.id, Way { id: w.id, nodes: w.refs.clone(), tags: w.tags.clone() });
        }
        for &ri in &airport_rels[a] {
            let r = &relations[ri];
            st.relations.insert(r.id, Relation { id: r.id, members: r.members.clone(), tags: r.tags.clone() });
        }
        for &ni in &airport_nodes[a] {
            let (id, la, lo, tags) = &tagged_nodes[ni];
            st.nodes.insert(*id, Coord { x: deg(*lo), y: deg(*la) });
            st.node_tags.insert(*id, tags.clone());
        }
        let Some(path) = cache.path(&cache_key(&t.icao)) else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        std::fs::write(&path, serde_json::to_string(&st)?).with_context(|| format!("write {}", path.display()))
    })?;
    log(&format!("{} airports saved in {}", stats.airports, crate::term::human_secs(t4.elapsed().as_secs_f64())));
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_matches_the_overpass_query() {
        assert!(wanted_area_or_line(&[("aeroway", "taxiway")], true));
        assert!(!wanted_area_or_line(&[("aeroway", "navigationaid")], true));
        assert!(wanted_area_or_line(&[("building", "yes")], true));
        assert!(wanted_area_or_line(&[("highway", "service")], true));
        assert!(!wanted_area_or_line(&[("highway", "service"), ("service", "parking_aisle")], true));
        assert!(!wanted_area_or_line(&[("highway", "residential")], true));
        assert!(wanted_area_or_line(&[("barrier", "fence")], true));
        assert!(!wanted_area_or_line(&[("barrier", "fence")], false), "fences are ways only, as the query has them");
        assert!(wanted_area_or_line(&[("type", "multipolygon"), ("natural", "water")], false));
        assert!(wanted_node(&[("aeroway", "parking_position")]));
        assert!(wanted_node(&[("aeroway", "navigationaid"), ("navigationaid", "papi")]));
        assert!(!wanted_node(&[("aeroway", "navigationaid"), ("navigationaid", "vor")]));
        assert!(!wanted_node(&[("amenity", "cafe")]));
    }

    #[test]
    fn the_grid_finds_points_in_boxes_and_across_the_antimeridian() {
        let g = Grid::new(vec![(42.3, -71.1, 42.4, -70.9), (-17.9, 179.9, -17.7, -179.9)]);
        assert_eq!(g.boxes_at(42.36, -71.0).collect::<Vec<_>>(), vec![0]);
        assert!(!g.in_any(42.36, -71.2));
        assert_eq!(g.boxes_at(-17.8, 179.95).collect::<Vec<_>>(), vec![1]);
        assert_eq!(g.boxes_at(-17.8, -179.95).collect::<Vec<_>>(), vec![1]);
        assert!(!g.in_any(0.0, 0.0));
    }

    #[test]
    fn inside_nodes_are_found_by_id() {
        let mut ins = Inside { nodes: vec![(900, 1, 2), (5, 3, 4), (70, 5, 6)], ..Default::default() };
        ins.seal();
        assert_eq!(ins.get(70), Some((5, 6)));
        assert_eq!(ins.get(5), Some((3, 4)));
        assert_eq!(ins.get(6), None);
        assert_eq!(ins.get(1_000_000), None);
    }
}
