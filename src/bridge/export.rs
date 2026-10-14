//! The airports as plain files for a web host, so an aircraft can load its airport maps
//! over HTTPS with nothing running beside the simulator: what an aircraft on a console
//! needs, where no program can run. Written for a Hugging Face dataset, which serves files
//! to the cockpit's browser (`coui://html_ui`) with the headers it needs, but any static
//! host will do.
//!
//! * `v1/<AB>/<ABCD>.json`  one airport: `{icao, arp: [lat, lon], layers: {name: FeatureCollection}}`,
//!   every layer the bridge serves, in the airport's own metre grid (Navigraph's
//!   `ARP_AZEQ`, an azimuthal equidistant projection about `arp`) to the centimetre.
//!   Folders by the first two letters keep each under the host's file count.
//! * `v1/search.json`       the list an OANS opens with (large and medium airports).
//! * `v1/search/<C>.json`   every airport whose code, IATA code or name begins with C.
//! * `v1/near/<lat>_<lon>.json` the airports `nearest` picks among, by 5-degree square
//!   (south-west corner).
//! * `v1/cycle.json`, `index.json` (what is here and how it is laid out), `README.md`.

use super::compat::NAVIGRAPH_LAYERS;
use super::store::{AirportData, Store};
use crate::model::codes::source;
use crate::model::Layer;
use anyhow::{Context, Result};
use rayon::prelude::*;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

/// One airport's file, as the bridge would answer `/v1/<ICAO>?projection=NAVIGRAPH:ARP_AZEQ`
/// with every layer, and where its grid is centred.
fn airport_json(store: &Store, icao: &str) -> Result<String> {
    let ap = store.load_built(icao)?;
    let mut layers = Map::new();
    for l in NAVIGRAPH_LAYERS {
        layers.insert(l.name().to_string(), super::server::layer_collection(&ap, *l, false, None));
    }
    Ok(json!({ "icao": icao, "arp": [ap.manifest.arp[0], ap.manifest.arp[1]], "layers": Value::Object(layers) }).to_string())
}

/// The layers an airport's OpenStreetMap half keeps when its Gateway half has the airfield
/// itself: what the Gateway's scenery does not draw.
pub const CONTEXT_LAYERS: [Layer; 6] = [Layer::VerticalPolygonalStructure, Layer::Water, Layer::ServiceRoad, Layer::ConstructionArea, Layer::DeicingArea, Layer::VerticalLineStructure];

/// Index entries from public-domain sources only, so the lists carry nothing of the
/// Gateway's or OpenStreetMap's.
const LIST_SOURCES: [&str; 3] = [source::OURAIRPORTS, source::FAA_NASR, source::FAA_AMDB];

/// One half of an airport: `layers` of a built airport, or nothing when none of them has a
/// feature beyond the reference point.
fn half_json(ap: &AirportData, icao: &str, layers: &[Layer]) -> Option<String> {
    let mut out = Map::new();
    let mut any = false;
    for l in layers {
        let fc = super::server::layer_collection(ap, *l, false, None);
        any |= *l != Layer::AerodromeReferencePoint && fc["features"].as_array().is_some_and(|a| !a.is_empty());
        out.insert(l.name().to_string(), fc);
    }
    any.then(|| json!({ "icao": icao, "arp": [ap.manifest.arp[0], ap.manifest.arp[1]], "layers": Value::Object(out) }).to_string())
}

/// Which layers an airport's OpenStreetMap half carries.
pub fn osm_layers(has_gateway: bool) -> &'static [Layer] {
    if has_gateway {
        &CONTEXT_LAYERS
    } else {
        NAVIGRAPH_LAYERS
    }
}

fn built(store: &Store, icao: &str) -> bool {
    store.out.join(icao).join("manifest.json").is_file()
}

/// The airports as two separately licensed halves, each from a build that never saw the
/// other's source:
///
/// * `gateway/v1/<AB>/<ABCD>.json` from a build of X-Plane Scenery Gateway data (GPL v2 or
///   later) with public-domain data (FAA, OurAirports), for every airport the Gateway has.
/// * `osm/v1/<AB>/<ABCD>.json` from a build of OpenStreetMap (ODbL) with the same
///   public-domain data: only [`CONTEXT_LAYERS`] where the airport has a Gateway half,
///   every layer where it has not.
/// * `v1/search…`, `v1/near…`, `v1/cycle.json`: the lists, from public-domain data only.
/// * `gateway/COPYING`, `gateway/NOTICE`, `osm/LICENSE`, `osm/NOTICE`, `index.json`, `README.md`.
///
/// Each half has its own `arp` and metre grid about it; a reader lines one up on the other.
pub fn export_split(gateway: &Store, osm: &Store, to: &Path, only: &[String]) -> Result<()> {
    let t0 = std::time::Instant::now();
    let icaos: Vec<String> = [&gateway.out, &osm.out]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten())
        .filter(|e| e.path().join("manifest.json").is_file())
        .map(|e| e.file_name().to_string_lossy().to_uppercase())
        .filter(|i| i.len() >= 3 && i.chars().all(|c| c.is_ascii_alphanumeric()))
        .filter(|i| only.is_empty() || only.iter().any(|o| o.eq_ignore_ascii_case(i)))
        .collect::<std::collections::BTreeSet<String>>()
        .into_iter()
        .collect();
    crate::term::start(&format!("Exporting {} airports as Gateway and OpenStreetMap halves into {}", icaos.len(), to.display()));
    let (done, bytes, failed, gw_n, osm_n) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let exported: HashSet<String> = icaos
        .par_iter()
        .filter_map(|icao| {
            let file = |half: &str| to.join(half).join("v1").join(&icao[..2]).join(format!("{icao}.json"));
            let result = (|| -> Result<bool> {
                let mut has_gateway = false;
                if built(gateway, icao) {
                    let ap = gateway.load_built(icao)?;
                    if ap.manifest.sources.iter().any(|s| s == source::XPLANE) {
                        if let Some(text) = half_json(&ap, icao, NAVIGRAPH_LAYERS) {
                            write(&file("gateway"), &text)?;
                            bytes.fetch_add(text.len(), Ordering::Relaxed);
                            gw_n.fetch_add(1, Ordering::Relaxed);
                            has_gateway = true;
                        }
                    }
                }
                let mut has_osm = false;
                if built(osm, icao) {
                    let ap = osm.load_built(icao)?;
                    if let Some(text) = half_json(&ap, icao, osm_layers(has_gateway)) {
                        write(&file("osm"), &text)?;
                        bytes.fetch_add(text.len(), Ordering::Relaxed);
                        osm_n.fetch_add(1, Ordering::Relaxed);
                        has_osm = true;
                    }
                }
                Ok(has_gateway || has_osm)
            })();
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 2000 == 0 {
                crate::term::info(&format!("{n}/{} airports, {}", icaos.len(), crate::term::human_bytes(bytes.load(Ordering::Relaxed) as u64)));
            }
            match result {
                Ok(true) => Some(icao.clone()),
                Ok(false) => None,
                Err(e) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    crate::term::warn(&format!("[{icao}] {e:#}"));
                    None
                }
            }
        })
        .collect();

    let mut entries: Vec<&crate::sources::index::IndexEntry> = gateway.index().by_icao.values().filter(|e| LIST_SOURCES.contains(&e.source)).collect();
    entries.sort_by(|a, b| a.icao.cmp(&b.icao));
    let big = |e: &crate::sources::index::IndexEntry| matches!(e.kind.as_deref(), Some("large_airport") | Some("medium_airport"));
    let row = |e: &crate::sources::index::IndexEntry| super::compat::search_row(&e.icao, e.iata.as_deref(), e.name.as_deref().unwrap_or(""), e.lat, e.lon, e.elevation_ft);
    let opening: Vec<Value> = entries.iter().filter(|e| big(e)).map(|e| row(e)).collect();
    write(&to.join("v1/search.json"), &Value::Array(opening).to_string())?;
    for c in ('0'..='9').chain('A'..='Z') {
        let q = c.to_string();
        let rows: Vec<Value> = entries
            .iter()
            .filter(|e| big(e) || exported.contains(&e.icao))
            .filter(|e| e.icao.starts_with(&q) || e.iata.as_deref().is_some_and(|i| i.to_uppercase().starts_with(&q)) || e.name.as_deref().unwrap_or("").to_uppercase().starts_with(&q))
            .map(|e| row(e))
            .collect();
        if !rows.is_empty() {
            write(&to.join(format!("v1/search/{c}.json")), &Value::Array(rows).to_string())?;
        }
    }
    let mut squares: BTreeMap<(i32, i32), Vec<Value>> = BTreeMap::new();
    for e in entries.iter().filter(|e| matches!(e.kind.as_deref(), Some("large_airport") | Some("medium_airport") | Some("small_airport")) || exported.contains(&e.icao)) {
        let mut r = row(e);
        r["kind"] = Value::from(e.kind.clone().unwrap_or_default());
        squares.entry(((e.lat / 5.0).floor() as i32 * 5, (e.lon / 5.0).floor() as i32 * 5)).or_default().push(r);
    }
    for ((la, lo), rows) in &squares {
        write(&to.join(format!("v1/near/{la}_{lo}.json")), &Value::Array(rows.clone()).to_string())?;
    }
    write(&to.join("v1/cycle.json"), &super::server::cycle_json().to_string())?;
    write(&to.join("gateway/COPYING"), GPL_2)?;
    write(&to.join("gateway/NOTICE"), GATEWAY_NOTICE)?;
    write(&to.join("osm/LICENSE"), ODBL_1)?;
    write(&to.join("osm/NOTICE"), OSM_NOTICE)?;
    let index_json = json!({
        "format": 2,
        "generated": chrono::Utc::now().to_rfc3339(),
        "generator": format!("amdb-bridge {}", env!("CARGO_PKG_VERSION")),
        "airports": exported.len(),
        "gateway_files": gw_n.load(Ordering::Relaxed),
        "osm_files": osm_n.load(Ordering::Relaxed),
        "layers": NAVIGRAPH_LAYERS.iter().map(|l| l.name()).collect::<Vec<_>>(),
        "gateway_file": "gateway/v1/{first two letters}/{ICAO}.json: X-Plane Scenery Gateway data with public-domain data, GPL v2 or later (gateway/COPYING)",
        "osm_file": "osm/v1/{first two letters}/{ICAO}.json: OpenStreetMap data with public-domain data, ODbL 1.0 (osm/LICENSE)",
        "osm_layers_beside_gateway": CONTEXT_LAYERS.iter().map(|l| l.name()).collect::<Vec<_>>(),
        "projection": "NAVIGRAPH:ARP_AZEQ: metres east and north of each file's own arp, azimuthal equidistant on a sphere of radius 6371008.8 m",
        "search": "v1/search.json (large and medium airports); v1/search/{first character}.json (every airport whose ICAO, IATA or name begins with it)",
        "nearest": "v1/near/{5-degree south-west corner lat}_{lon}.json",
    });
    write(&to.join("index.json"), &serde_json::to_string_pretty(&index_json)?)?;
    write(&to.join("README.md"), SPLIT_README)?;
    crate::term::success(&format!(
        "{} airports: {} Gateway halves, {} OpenStreetMap halves ({}), {} squares for nearest, in {}{}",
        exported.len(),
        gw_n.load(Ordering::Relaxed),
        osm_n.load(Ordering::Relaxed),
        crate::term::human_bytes(bytes.load(Ordering::Relaxed) as u64),
        squares.len(),
        crate::term::human_secs(t0.elapsed().as_secs_f64()),
        match failed.load(Ordering::Relaxed) {
            0 => String::new(),
            n => format!(", {n} could not be read"),
        }
    ));
    Ok(())
}

/// Write every built airport (or only those in `only`) and the indexes into `to`.
pub fn export(store: &Store, to: &Path, only: &[String]) -> Result<()> {
    let t0 = std::time::Instant::now();
    let mut icaos: Vec<String> = std::fs::read_dir(&store.out)?
        .flatten()
        .filter(|e| e.path().join("manifest.json").is_file())
        .map(|e| e.file_name().to_string_lossy().to_uppercase())
        .filter(|i| i.len() >= 3 && i.chars().all(|c| c.is_ascii_alphanumeric()))
        .filter(|i| only.is_empty() || only.iter().any(|o| o.eq_ignore_ascii_case(i)))
        .collect();
    icaos.sort();
    crate::term::start(&format!("Exporting {} airports for the web into {}", icaos.len(), to.display()));
    let (done, bytes, failed) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    icaos.par_iter().for_each(|icao| {
        let path = to.join("v1").join(&icao[..2]).join(format!("{icao}.json"));
        match airport_json(store, icao).and_then(|text| write(&path, &text).map(|_| text.len())) {
            Ok(n) => {
                bytes.fetch_add(n, Ordering::Relaxed);
            }
            Err(e) => {
                failed.fetch_add(1, Ordering::Relaxed);
                crate::term::warn(&format!("[{icao}] {e:#}"));
            }
        }
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 2000 == 0 {
            crate::term::info(&format!("{n}/{} airports, {}", icaos.len(), crate::term::human_bytes(bytes.load(Ordering::Relaxed) as u64)));
        }
    });

    // The lists: the one an OANS opens with, and one a first character for a typed search.
    write(&to.join("v1/search.json"), &Value::Array(store.search("")).to_string())?;
    for c in ('0'..='9').chain('A'..='Z') {
        let rows = store.search(&c.to_string());
        if !rows.is_empty() {
            write(&to.join(format!("v1/search/{c}.json")), &Value::Array(rows).to_string())?;
        }
    }
    // Nearest, by five-degree square.
    let mut squares: BTreeMap<(i32, i32), Vec<Value>> = BTreeMap::new();
    for (lat, lon, row) in store.nearest_candidates() {
        squares.entry(((lat / 5.0).floor() as i32 * 5, (lon / 5.0).floor() as i32 * 5)).or_default().push(row);
    }
    for ((la, lo), rows) in &squares {
        write(&to.join(format!("v1/near/{la}_{lo}.json")), &Value::Array(rows.clone()).to_string())?;
    }
    write(&to.join("v1/cycle.json"), &super::server::cycle_json().to_string())?;
    let index = json!({
        "format": 1,
        "generated": chrono::Utc::now().to_rfc3339(),
        "generator": format!("amdb-bridge {}", env!("CARGO_PKG_VERSION")),
        "airports": icaos.len(),
        "layers": NAVIGRAPH_LAYERS.iter().map(|l| l.name()).collect::<Vec<_>>(),
        "airport_file": "v1/{first two letters}/{ICAO}.json",
        "projection": "NAVIGRAPH:ARP_AZEQ: metres east and north of arp, azimuthal equidistant on a sphere of radius 6371008.8 m",
        "search": "v1/search.json (large and medium airports); v1/search/{first character}.json (every airport whose ICAO, IATA or name begins with it)",
        "nearest": "v1/near/{5-degree south-west corner lat}_{lon}.json",
    });
    write(&to.join("index.json"), &serde_json::to_string_pretty(&index)?)?;
    write(&to.join("README.md"), README)?;
    crate::term::success(&format!(
        "{} airports ({}), {} squares for nearest, in {}{}",
        icaos.len() - failed.load(Ordering::Relaxed),
        crate::term::human_bytes(bytes.load(Ordering::Relaxed) as u64),
        squares.len(),
        crate::term::human_secs(t0.elapsed().as_secs_f64()),
        match failed.load(Ordering::Relaxed) {
            0 => String::new(),
            n => format!(", {n} could not be read"),
        }
    ));
    Ok(())
}

/// The same files packed to ship inside an aircraft, for when it cannot reach the web host
/// (offline, or outside requests blocked). The sim lists every file of a package, so tens
/// of thousands of loose airports would slow its loading: they go in packs of about 2 MB,
/// each file in it raw-deflated, and `index.json` says where each is:
/// `{format, packs, files: {"KBOS": [pack, offset, length], "search": ..., "search/K": ...,
/// "near/50_-5": ...}}`, packs named `p000.bin` on. From an `export_split` folder the
/// airports' halves are `"gateway/KBOS"` and `"osm/KBOS"`, and each half's licence files
/// are copied beside the packs (`gateway/COPYING`, `osm/LICENSE`, their `NOTICE`s) so they
/// ship with the data. `select` says which airports go in; the search and nearest lists
/// packed with them name only those, so that an aircraft reading the bundle offers no
/// airport it cannot then load.
pub fn bundle(web: &Path, to: &Path, select: &Select) -> Result<()> {
    use std::io::Write;
    const PACK_BYTES: usize = 2 << 20;
    let t0 = std::time::Instant::now();
    let v1 = web.join("v1");
    let rows_of = |path: &Path| -> Result<Vec<Value>> { Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?) };
    let code = |r: &Value| r.get("idarpt").and_then(Value::as_str).unwrap_or("").to_string();
    let listed: HashSet<String> = rows_of(&v1.join("search.json"))?.iter().map(code).collect();
    let mut large: HashSet<String> = HashSet::new();
    for e in std::fs::read_dir(v1.join("near"))?.flatten() {
        for r in rows_of(&e.path())? {
            if r.get("kind").and_then(Value::as_str) == Some("large_airport") {
                large.insert(code(&r));
            }
        }
    }
    // Each airport's files with the key each is packed under: one file in a single export,
    // the Gateway half then the OpenStreetMap half in a split one.
    let split = web.join("gateway").is_dir() || web.join("osm").is_dir();
    let roots: Vec<(std::path::PathBuf, &str)> = if split { vec![(web.join("gateway").join("v1"), "gateway/"), (web.join("osm").join("v1"), "osm/")] } else { vec![(v1.clone(), "")] };
    let mut by_airport: BTreeMap<String, Vec<(String, std::path::PathBuf)>> = BTreeMap::new();
    for (root, prefix) in &roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        for d in rd.flatten().filter(|d| d.path().is_dir()) {
            let name = d.file_name().to_string_lossy().to_string();
            if name == "search" || name == "near" {
                continue;
            }
            for f in std::fs::read_dir(d.path())?.flatten() {
                let icao = f.file_name().to_string_lossy().trim_end_matches(".json").to_string();
                by_airport.entry(icao.clone()).or_default().push((format!("{prefix}{icao}"), f.path()));
            }
        }
    }
    // Whether an airport goes in is judged on its first file: the Gateway half where there
    // is one, since that has the runways.
    let chosen: Vec<(String, Vec<(String, std::path::PathBuf)>)> = by_airport.into_par_iter().filter(|(icao, files)| select.keeps(icao, &files[0].1, &listed, &large)).collect();
    let kept: HashSet<String> = chosen.iter().map(|(i, _)| i.clone()).collect();
    let mut airports: Vec<(String, std::path::PathBuf)> = chosen.into_iter().flat_map(|(_, files)| files).collect();
    airports.sort();
    crate::term::info(&format!("{} airports go in the bundle", kept.len()));

    // The lists, cut down to what is in the bundle, written beside the packs' sources.
    let lists = to.join("lists");
    let mut items: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut cut = |key: String, src: &Path| -> Result<()> {
        let rows: Vec<Value> = rows_of(src)?.into_iter().filter(|r| kept.contains(&code(r))).collect();
        if !rows.is_empty() {
            let out = lists.join(format!("{}.json", key.replace('/', "_")));
            write(&out, &Value::Array(rows).to_string())?;
            items.push((key, out));
        }
        Ok(())
    };
    cut("search".to_string(), &v1.join("search.json"))?;
    for (dir, prefix) in [("search", "search/"), ("near", "near/")] {
        for e in std::fs::read_dir(v1.join(dir))?.flatten() {
            let name = e.file_name().to_string_lossy().trim_end_matches(".json").to_string();
            cut(format!("{prefix}{name}"), &e.path())?;
        }
    }
    items.push(("cycle".to_string(), v1.join("cycle.json")));
    items.extend(airports);
    std::fs::create_dir_all(to)?;
    let mut files = Map::new();
    let (mut pack, mut current, mut total) = (0usize, Vec::<u8>::new(), 0usize);
    let flush = |pack: usize, data: &[u8]| -> Result<()> { std::fs::write(to.join(format!("p{pack:03}.bin")), data).with_context(|| format!("write pack {pack}")) };
    for (key, path) in &items {
        let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&raw)?;
        let packed = enc.finish()?;
        if !current.is_empty() && current.len() + packed.len() > PACK_BYTES {
            flush(pack, &current)?;
            total += current.len();
            pack += 1;
            current.clear();
        }
        files.insert(key.clone(), json!([pack, current.len(), packed.len()]));
        current.extend_from_slice(&packed);
    }
    if !current.is_empty() {
        flush(pack, &current)?;
        total += current.len();
        pack += 1;
    }
    write(&to.join("index.json"), &json!({ "format": 1, "packs": pack, "files": Value::Object(files) }).to_string())?;
    let _ = std::fs::remove_dir_all(&lists);
    if split {
        for (half, name) in [("gateway", "COPYING"), ("gateway", "NOTICE"), ("osm", "LICENSE"), ("osm", "NOTICE")] {
            let src = web.join(half).join(name);
            if src.is_file() {
                std::fs::create_dir_all(to.join(half))?;
                std::fs::copy(&src, to.join(half).join(name)).with_context(|| format!("copy {}", src.display()))?;
            }
        }
    }
    crate::term::success(&format!("{} files in {pack} packs, {}, into {} in {}", items.len(), crate::term::human_bytes(total as u64), to.display(), crate::term::human_secs(t0.elapsed().as_secs_f64())));
    Ok(())
}

/// Which airports a bundle takes.
pub struct Select {
    /// Only the large and medium airports (the list an OANS opens with).
    pub listed_only: bool,
    /// Only airports with a paved runway at least this long and `min_width_m` wide: the
    /// ones an aircraft can use, not only those it is flown to.
    pub min_runway_m: Option<f64>,
    pub min_width_m: f64,
    /// Every large airport as well, whatever its runways are recorded as.
    pub large_too: bool,
}

/// Surfaces counted as paved: concrete and asphalt, grooved or not, friction seal coat; and
/// an unrecorded one, since a runway long and wide enough is paved in practice.
const PAVED: [i64; 7] = [1, 2, 3, 4, 10, 0, -32767];

impl Select {
    fn keeps(&self, icao: &str, path: &Path, listed: &HashSet<String>, large: &HashSet<String>) -> bool {
        if self.large_too && large.contains(icao) {
            return true;
        }
        if self.listed_only && !listed.contains(icao) {
            return false;
        }
        let Some(min_len) = self.min_runway_m else { return true };
        let Ok(text) = std::fs::read_to_string(path) else { return false };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return false };
        v["layers"]["runwayelement"]["features"].as_array().into_iter().flatten().any(|f| {
            let p = &f["properties"];
            let num = |k: &str| p[k].as_f64().unwrap_or(0.0);
            PAVED.contains(&p["surftype"].as_i64().unwrap_or(0)) && num("length") >= min_len && num("width") >= self.min_width_m
        })
    }
}

const README: &str = "---
license: odbl
---

Redistributable Content for Airport Mapping Database V1
";

const SPLIT_README: &str = "---
license: other
---

Redistributable Content for Airport Mapping Database V1
";

const GPL_2: &str = include_str!("licences/GPL-2.0.txt");
const ODBL_1: &str = include_str!("licences/ODbL-1.0.txt");

const GATEWAY_NOTICE: &str = "Airport layouts in this folder are derived from the X-Plane Scenery Gateway
(https://gateway.x-plane.com/), whose scenery packs are free software: you can
redistribute them and/or modify them under the terms of the GNU General Public License
as published by the Free Software Foundation; either version 2 of the License, or (at
your option) any later version. These files are distributed under the same terms; see
COPYING. The scenery they come from is the Gateway's own download for each airport.

They also hold public-domain data from the FAA and OurAirports, and nothing from
OpenStreetMap: OpenStreetMap data is in ../osm, under its own licence.

Converted by Free Airport Mapping DB:
https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB
";

const OSM_NOTICE: &str = "Map data (c) OpenStreetMap contributors, available under the Open Database
License (ODbL) v1.0: see LICENSE and https://www.openstreetmap.org/copyright.

This folder is a derivative database of OpenStreetMap and is made available under the
ODbL. It also holds public-domain data from the FAA and OurAirports, and nothing from
the X-Plane Scenery Gateway: Gateway data is in ../gateway, under its own licence.

Converted by Free Airport Mapping DB:
https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beside_a_gateway_half_the_osm_half_keeps_only_what_the_gateway_does_not_draw() {
        let beside = osm_layers(true);
        for l in [Layer::AerodromeReferencePoint, Layer::RunwayElement, Layer::TaxiwayElement, Layer::TaxiwayGuidanceLine, Layer::ParkingStandLocation, Layer::ParkingStandArea, Layer::ApronElement, Layer::TaxiwayHoldingPosition] {
            assert!(!beside.contains(&l), "{l:?} would be drawn from both sources");
        }
        for l in [Layer::VerticalPolygonalStructure, Layer::Water, Layer::ServiceRoad] {
            assert!(beside.contains(&l), "{l:?} is missing");
        }
        assert!(CONTEXT_LAYERS.iter().all(|l| NAVIGRAPH_LAYERS.contains(l)));
        assert_eq!(osm_layers(false), NAVIGRAPH_LAYERS);
    }

    #[test]
    fn each_half_ships_its_licence_and_the_card_keeps_its_one_line() {
        assert!(GPL_2.contains("GNU GENERAL PUBLIC LICENSE") && GPL_2.contains("Version 2, June 1991"));
        assert!(ODBL_1.contains("Open Database License"));
        assert!(GATEWAY_NOTICE.contains("GNU General Public License") && GATEWAY_NOTICE.contains("nothing from\nOpenStreetMap"));
        assert!(OSM_NOTICE.contains("OpenStreetMap contributors") && OSM_NOTICE.contains("nothing from\nthe X-Plane Scenery Gateway"));
        assert_eq!(SPLIT_README, "---\nlicense: other\n---\n\nRedistributable Content for Airport Mapping Database V1\n");
    }
}
