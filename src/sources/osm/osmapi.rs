//! OpenStreetMap map API (`/api/0.6/map?bbox=`): a direct read of the live OSM
//! database, no key, no job queue. Each call is limited to 50 000 nodes, so the
//! airport box is split into tiles when the server says it is too big, tiles are
//! fetched concurrently and merged by element id.

use super::elements::{Member, Relation, Store, Tags, Way};
use crate::cache::Cache;
use crate::sources::http::Http;
use anyhow::{anyhow, Context, Result};
use geo_types::Coord;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::sync::{Arc, Mutex};

pub const ENDPOINT: &str = "https://api.openstreetmap.org/api/0.6/map";
const MAX_CONCURRENT: usize = 4;
const MAX_DEPTH: u32 = 5;

/// (south, west, north, east)
type BBox = (f64, f64, f64, f64);

fn url(b: BBox) -> String {
    format!("{ENDPOINT}?bbox={:.6},{:.6},{:.6},{:.6}", b.1, b.0, b.3, b.2)
}

/// Parse an OSM XML document into a store (ids are merged, later wins).
pub fn parse_xml(text: &str, st: &mut Store) -> Result<()> {
    let mut r = Reader::from_str(text);
    r.config_mut().trim_text(true);
    let mut cur_way: Option<Way> = None;
    let mut cur_rel: Option<Relation> = None;
    let mut cur_node: Option<(i64, Tags)> = None;
    let attr = |e: &quick_xml::events::BytesStart, name: &str| -> Option<String> {
        e.attributes().flatten().find(|a| a.key.as_ref() == name).map(|a| a.value.to_string())
    };
    loop {
        let ev = r.read_event().context("osm xml")?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.name().as_ref() {
                    "node" => {
                        let id: i64 = attr(e, "id").and_then(|v| v.parse().ok()).unwrap_or(0);
                        let lat: f64 = attr(e, "lat").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                        let lon: f64 = attr(e, "lon").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                        st.nodes.insert(id, Coord { x: lon, y: lat });
                        if !empty {
                            cur_node = Some((id, Tags::new()));
                        }
                    }
                    "way" => {
                        let id: i64 = attr(e, "id").and_then(|v| v.parse().ok()).unwrap_or(0);
                        cur_way = Some(Way { id, nodes: vec![], tags: Tags::new() });
                        if empty {
                            cur_way = None;
                        }
                    }
                    "relation" => {
                        let id: i64 = attr(e, "id").and_then(|v| v.parse().ok()).unwrap_or(0);
                        cur_rel = Some(Relation { id, members: vec![], tags: Tags::new() });
                        if empty {
                            cur_rel = None;
                        }
                    }
                    "nd" => {
                        if let (Some(w), Some(r)) = (cur_way.as_mut(), attr(e, "ref").and_then(|v| v.parse::<i64>().ok())) {
                            w.nodes.push(r);
                        }
                    }
                    "member" => {
                        if let Some(rel) = cur_rel.as_mut() {
                            let kind = match attr(e, "type").as_deref() {
                                Some("node") => 'n',
                                Some("way") => 'w',
                                _ => 'r',
                            };
                            if let Some(id) = attr(e, "ref").and_then(|v| v.parse::<i64>().ok()) {
                                rel.members.push(Member { kind, id, role: attr(e, "role").unwrap_or_default() });
                            }
                        }
                    }
                    "tag" => {
                        if let (Some(k), Some(v)) = (attr(e, "k"), attr(e, "v")) {
                            if let Some(w) = cur_way.as_mut() {
                                w.tags.insert(k, v);
                            } else if let Some(rel) = cur_rel.as_mut() {
                                rel.tags.insert(k, v);
                            } else if let Some((_, t)) = cur_node.as_mut() {
                                t.insert(k, v);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::End(ref e) => match e.name().as_ref() {
                "node" => {
                    if let Some((id, t)) = cur_node.take() {
                        if !t.is_empty() {
                            st.node_tags.insert(id, t);
                        }
                    }
                }
                "way" => {
                    if let Some(w) = cur_way.take() {
                        st.ways.insert(w.id, w);
                    }
                }
                "relation" => {
                    if let Some(rel) = cur_rel.take() {
                        st.relations.insert(rel.id, rel);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

fn split(b: BBox) -> [BBox; 4] {
    let (s, w, n, e) = b;
    let (ms, me) = ((s + n) / 2.0, (w + e) / 2.0);
    [(s, w, ms, me), (s, me, ms, e), (ms, w, n, me), (ms, me, n, e)]
}

/// At most this many map calls in flight process-wide, however many airports are being
/// fetched at once: the API throttles on bandwidth and tells us when we overdo it.
const MAX_GLOBAL_CALLS: usize = 6;
static GATE: (Mutex<usize>, std::sync::Condvar) = (Mutex::new(0), std::sync::Condvar::new());

struct Slot;

impl Slot {
    fn acquire() -> Slot {
        let (m, cv) = &GATE;
        let mut n = m.lock().unwrap();
        while *n >= MAX_GLOBAL_CALLS {
            n = cv.wait(n).unwrap();
        }
        *n += 1;
        Slot
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let (m, cv) = &GATE;
        *m.lock().unwrap() -= 1;
        cv.notify_one();
    }
}

/// Seconds the server asked us to wait ("Please try again in 11 seconds").
fn retry_after_secs(msg: &str) -> Option<u64> {
    let i = msg.find("try again in ")?;
    msg[i + 13..].split_whitespace().next()?.parse::<u64>().ok()
}

/// Fetch one tile; `Ok(None)` means "too many nodes, split further". A bandwidth
/// throttle (HTTP 509 / 429) is obeyed: wait the time the server names, then retry.
fn fetch_tile(http: &Http, b: BBox) -> Result<Option<String>> {
    let mut waited = 0u64;
    for attempt in 0..10 {
        let result = {
            let _slot = Slot::acquire();
            http.get_text_once(&url(b))
        };
        match result {
            Ok(t) => return Ok(Some(t)),
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("HTTP 400") || msg.contains("too many nodes") {
                    return Ok(None);
                }
                let throttled = msg.contains("HTTP 509") || msg.contains("HTTP 429") || msg.contains("too much data");
                if throttled && attempt < 9 && waited < 600 {
                    let secs = retry_after_secs(&msg).unwrap_or(15).clamp(2, 120) + 1;
                    log::info!("OSM API throttled us; waiting {secs}s as asked");
                    std::thread::sleep(std::time::Duration::from_secs(secs));
                    waited += secs;
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err(anyhow!("OSM API kept throttling"))
}

/// Fetch everything in `bbox`, tiling as needed, into one merged store.
pub fn fetch_bbox(http: &Http, bbox: BBox) -> Result<Store> {
    fetch_bbox_scoped(http, bbox, None)
}

pub fn fetch_bbox_scoped(http: &Http, bbox: BBox, scope: Option<&str>) -> Result<Store> {
    crate::term::step(scope, &format!("GET {ENDPOINT} bbox {:.4},{:.4} to {:.4},{:.4}", bbox.1, bbox.0, bbox.3, bbox.2));
    let store = Arc::new(Mutex::new(Store::default()));
    let mut queue: Vec<(BBox, u32)> = vec![(bbox, 0)];
    let mut tiles = 0usize;
    while !queue.is_empty() {
        let batch: Vec<(BBox, u32)> = queue.drain(..queue.len().min(MAX_CONCURRENT)).collect();
        let results: Vec<(BBox, u32, Result<Option<String>>)> = std::thread::scope(|sc| {
            let hs: Vec<_> = batch.iter().map(|(b, d)| { let (b, d) = (*b, *d); sc.spawn(move || (b, d, fetch_tile(http, b))) }).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (b, d, res) in results {
            match res? {
                Some(text) => {
                    tiles += 1;
                    let before = store.lock().unwrap().nodes.len();
                    parse_xml(&text, &mut store.lock().unwrap())?;
                    let after = store.lock().unwrap().nodes.len();
                    crate::term::step(scope, &format!("OSM tile {tiles}: {} ({} new nodes)", crate::term::human_bytes(text.len() as u64), after - before));
                }
                None => {
                    if d >= MAX_DEPTH {
                        return Err(anyhow!("tile still too dense after {MAX_DEPTH} splits"));
                    }
                    crate::term::step(scope, &format!("OSM tile over 50k nodes, splitting into 4 (depth {})", d + 1));
                    queue.extend(split(b).into_iter().map(|t| (t, d + 1)));
                }
            }
        }
    }
    log::debug!("osm api: {tiles} tile(s)");
    Ok(Arc::try_unwrap(store).map(|m| m.into_inner().unwrap()).unwrap_or_default())
}

/// Fetch (cached when a cache dir is configured) the store for an airport box.
pub fn fetch(http: &Http, cache: &Cache, icao: &str, bbox: BBox) -> Result<Store> {
    let key = format!("osm/osmapi/{}.json", icao.to_uppercase());
    let text = cache.get_or_fetch_text(&key, || {
        let st = fetch_bbox_scoped(http, bbox, Some(icao))?;
        if st.nodes.is_empty() {
            return Err(anyhow!("OSM API returned no nodes"));
        }
        serde_json::to_string(&st).context("serialise store")
    })?;
    serde_json::from_str(&text).context("cached store")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_osm_xml() {
        let xml = r#"<?xml version="1.0"?><osm version="0.6">
          <node id="1" lat="28.5" lon="77.1"/>
          <node id="2" lat="28.6" lon="77.2"><tag k="aeroway" v="parking_position"/><tag k="ref" v="5"/></node>
          <way id="10"><nd ref="1"/><nd ref="2"/><tag k="aeroway" v="taxiway"/><tag k="ref" v="A"/></way>
          <relation id="20"><member type="way" ref="10" role="outer"/><tag k="type" v="multipolygon"/></relation>
        </osm>"#;
        let mut st = Store::default();
        parse_xml(xml, &mut st).unwrap();
        assert_eq!(st.nodes.len(), 2);
        assert_eq!(st.node_tags[&2]["ref"], "5");
        assert_eq!(st.ways[&10].nodes, vec![1, 2]);
        assert_eq!(st.ways[&10].tags["ref"], "A");
        assert_eq!(st.relations[&20].members[0].kind, 'w');
        let q = split((0.0, 0.0, 1.0, 1.0));
        assert_eq!(q[3], (0.5, 0.5, 1.0, 1.0));
        assert!(url((50.0, 8.0, 50.1, 8.1)).ends_with("bbox=8.000000,50.000000,8.100000,50.100000"));
    }
}
