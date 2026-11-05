//! Airports already built, from the Hugging Face dataset, so that an airport's first load
//! is a download of a second or two rather than a build of 3 to 25 seconds that asks
//! OpenStreetMap's servers for it.
//!
//! The dataset keeps every airport built twice, in two archives: `gateway/builds.tar`, built
//! from the X-Plane Scenery Gateway's data alone, and `osm/builds.tar`, from OpenStreetMap's
//! alone (see `export`). Each holds `<ICAO>/manifest.json` and `<ICAO>/layers.json.br`, the
//! folder a build writes, sorted by code. Beside each is `builds.idx.json`, written by
//! `amdb-bridge index-builds`, saying where in the archive each airport lies, so that an
//! airport is one byte range of each archive and never the whole of it.
//!
//! The two are put together the way the 787's reader puts them together: the Gateway's
//! airfield, with OpenStreetMap's buildings, water and roads where it has them, or
//! OpenStreetMap's airport whole where the Gateway has none.

use super::export::CONTEXT_LAYERS;
use crate::model::codes::source;
use crate::sources::http::Http;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const DATASET: &str = "https://huggingface.co/datasets/Royale2072/free-airport-mapping-db/resolve/main";

/// Beside each archive: where each airport lies in it.
pub const INDEX: &str = "builds.idx.json";

const ARCHIVE: &str = "builds.tar";

/// The two halves, in the order they are put together.
const HALVES: [&str; 2] = ["gateway", "osm"];

/// How long after the indexes could not be read they are asked for again.
const RETRY_AFTER: Duration = Duration::from_secs(600);

const BLOCK: u64 = 512;

/// The brotli quality an airport put together from its halves is packed at: it is made
/// while an aircraft waits on it.
const QUICK: u32 = 5;

static OFF: AtomicBool = AtomicBool::new(false);

/// Build every airport here instead, for this run.
pub fn turn_off() {
    OFF.store(true, Ordering::Relaxed);
}

pub fn is_on() -> bool {
    !OFF.load(Ordering::Relaxed)
}

/// The dataset's address: `AMDB_DATASET_URL` when set, which is how a copy of it on this
/// computer is tried before it is published.
fn base() -> String {
    std::env::var("AMDB_DATASET_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| DATASET.to_string()).trim_end_matches('/').to_string()
}

/// Its own client: the shared one waits a quarter of a second between requests, out of
/// courtesy to OpenStreetMap, and a file host needs none of that.
fn http() -> &'static Http {
    static HTTP: OnceLock<Http> = OnceLock::new();
    HTTP.get_or_init(|| Http::new(60, 0))
}

/// Where each airport lies in one archive: the archive's size, and by code the offset of
/// its first entry and the length of its entries.
#[derive(Debug, Default, PartialEq)]
pub struct Index {
    pub size: u64,
    pub airports: HashMap<String, (u64, u64)>,
}

impl Index {
    fn from_json(text: &str) -> Result<Index> {
        let v: Value = serde_json::from_str(text).context("read the index")?;
        let size = v["size"].as_u64().ok_or_else(|| anyhow!("the index gives no archive size"))?;
        let mut airports = HashMap::new();
        for (icao, at) in v["airports"].as_object().ok_or_else(|| anyhow!("the index lists no airports"))? {
            if let (Some(offset), Some(len)) = (at[0].as_u64(), at[1].as_u64()) {
                airports.insert(icao.clone(), (offset, len));
            }
        }
        Ok(Index { size, airports })
    }

    fn to_json(&self) -> String {
        let airports: std::collections::BTreeMap<&String, [u64; 2]> = self.airports.iter().map(|(k, (o, l))| (k, [*o, *l])).collect();
        json!({ "format": 1, "archive": ARCHIVE, "size": self.size, "airports": airports }).to_string()
    }
}

/// What an entry is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    File,
    /// Extended attributes of the entry after it, as Python's `tarfile` writes before each.
    Extended,
    Other,
}

/// One entry's header: its name, size and kind. None for an empty block (the end) or a
/// header that does not read.
fn header(block: &[u8]) -> Option<(String, u64, Kind)> {
    if block.len() < BLOCK as usize || block.iter().all(|b| *b == 0) {
        return None;
    }
    let text = |r: std::ops::Range<usize>| {
        let f = &block[r];
        String::from_utf8_lossy(&f[..f.iter().position(|b| *b == 0).unwrap_or(f.len())]).into_owned()
    };
    let size = u64::from_str_radix(text(124..136).trim(), 8).ok()?;
    let name = text(0..100);
    let prefix = if &block[257..262] == b"ustar" { text(345..500) } else { String::new() };
    let kind = match block[156] {
        b'0' | 0 => Kind::File,
        b'x' | b'g' => Kind::Extended,
        _ => Kind::Other,
    };
    Some((if prefix.is_empty() { name } else { format!("{prefix}/{name}") }, size, kind))
}

fn padded(size: u64) -> u64 {
    size.div_ceil(BLOCK) * BLOCK
}

/// The airport an entry belongs to: the folder it is in.
fn airport_of(name: &str) -> Option<&str> {
    let (icao, file) = name.trim_start_matches("./").split_once('/')?;
    (!icao.is_empty() && !file.is_empty() && !file.contains('/')).then_some(icao)
}

/// Where each airport lies in an archive, read from its headers alone.
pub fn index_archive(archive: &Path) -> Result<Index> {
    let mut f = std::io::BufReader::new(std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?);
    let size = std::fs::metadata(archive)?.len();
    let mut airports: HashMap<String, (u64, u64)> = HashMap::new();
    let mut at = 0u64;
    let mut block = [0u8; BLOCK as usize];
    let mut extended: Option<u64> = None;
    while at + BLOCK <= size {
        f.seek(SeekFrom::Start(at))?;
        f.read_exact(&mut block)?;
        let Some((name, len, kind)) = header(&block) else { break };
        let end = at + BLOCK + padded(len);
        if kind == Kind::Extended {
            extended.get_or_insert(at);
            at = end;
            continue;
        }
        let start = extended.take().unwrap_or(at);
        if let (Kind::File, Some(icao)) = (kind, airport_of(&name)) {
            let e = airports.entry(icao.to_uppercase()).or_insert((start, 0));
            if e.0 + e.1 != start {
                bail!("{}: {icao}'s entries are not together", archive.display());
            }
            e.1 = end - e.0;
        }
        at = end;
    }
    Ok(Index { size, airports })
}

/// Write `builds.idx.json` beside each archive.
pub fn write_indexes(archives: &[std::path::PathBuf]) -> Result<()> {
    for a in archives {
        let index = index_archive(a)?;
        let to = a.with_file_name(INDEX);
        std::fs::write(&to, index.to_json()).with_context(|| format!("write {}", to.display()))?;
        crate::term::success(&format!("{} airports in {} indexed in {}", index.airports.len(), a.display(), to.display()));
    }
    Ok(())
}

/// The files of `icao` in a piece of an archive, by name within its folder.
fn files_in(piece: &[u8], icao: &str) -> Result<HashMap<String, Vec<u8>>> {
    let mut files = HashMap::new();
    let mut at = 0usize;
    while at + BLOCK as usize <= piece.len() {
        let Some((name, len, kind)) = header(&piece[at..at + BLOCK as usize]) else { break };
        let start = at + BLOCK as usize;
        let end = start + len as usize;
        if end > piece.len() {
            bail!("{icao}: the archive ends inside {name}");
        }
        match airport_of(&name) {
            _ if kind != Kind::File => {}
            Some(a) if a.eq_ignore_ascii_case(icao) => {
                files.insert(name.rsplit('/').next().unwrap_or_default().to_string(), piece[start..end].to_vec());
            }
            _ => bail!("{icao}: the index points at {name}, so it is not this archive's index"),
        }
        at = start + padded(len) as usize;
    }
    Ok(files)
}

/// One half of an airport, as its build wrote it.
#[derive(Debug, Clone)]
pub struct Half {
    pub manifest: Value,
    pub pack: Vec<u8>,
}

impl Half {
    fn from_files(mut files: HashMap<String, Vec<u8>>, icao: &str) -> Result<Half> {
        let manifest = files.remove("manifest.json").ok_or_else(|| anyhow!("{icao}: no manifest.json"))?;
        let pack = files.remove(crate::output::layers::PACK).ok_or_else(|| anyhow!("{icao}: no {}", crate::output::layers::PACK))?;
        Ok(Half { manifest: serde_json::from_slice(&manifest).with_context(|| format!("{icao}: manifest.json"))?, pack })
    }

    fn sources(&self) -> impl Iterator<Item = &str> {
        self.manifest["sources"].as_array().into_iter().flatten().filter_map(Value::as_str)
    }
}

fn has_features(fc: &str) -> bool {
    serde_json::from_str::<Value>(fc).ok().and_then(|v| v["features"].as_array().map(|a| !a.is_empty())).unwrap_or(false)
}

/// An airport from its halves: the manifest and the pack its folder is to hold.
///
/// The Gateway's half, when the Gateway has the airport, with each of OpenStreetMap's
/// [`CONTEXT_LAYERS`] that has something in it in place of its own; otherwise
/// OpenStreetMap's half whole. The Gateway's build lists every airport, with no more than
/// a reference point for those its data does not have, so only one with X-Plane among its
/// sources counts as the Gateway having it.
pub fn merge(gateway: Option<Half>, osm: Option<Half>) -> Result<Half> {
    let (g, o) = match (gateway, osm) {
        (Some(g), o) if g.sources().any(|s| s == source::XPLANE) => (g, o),
        (_, Some(o)) => return Ok(o),
        (Some(g), None) => return Ok(g),
        (None, None) => bail!("neither half"),
    };
    let Some(o) = o else { return Ok(g) };
    let theirs = crate::output::layers::unpack_bytes(&o.pack)?;
    let taken: Vec<&str> = CONTEXT_LAYERS.iter().map(|l| l.name()).filter(|n| theirs.get(*n).is_some_and(|t| has_features(t))).collect();
    if taken.is_empty() {
        return Ok(g);
    }
    let mut layers = crate::output::layers::unpack_bytes(&g.pack)?;
    let mut manifest = g.manifest.clone();
    let mut sources: BTreeSet<String> = g.sources().map(str::to_string).collect();
    for name in &taken {
        layers.insert(name.to_string(), theirs[*name].clone());
        let info = o.manifest["layers"][*name].clone();
        sources.extend(info["sources"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string));
        manifest["layers"][*name] = info;
    }
    manifest["sources"] = json!(sources);
    if let (Some(a), Some(b)) = (g.manifest["bbox"].as_array(), o.manifest["bbox"].as_array()) {
        let n = |v: &Vec<Value>, i: usize| v.get(i).and_then(Value::as_f64);
        if let (Some(a), Some(b)) = ((0..4).map(|i| n(a, i)).collect::<Option<Vec<f64>>>(), (0..4).map(|i| n(b, i)).collect::<Option<Vec<f64>>>()) {
            manifest["bbox"] = json!([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]);
        }
    }
    let mut layers: Vec<(String, String)> = layers.into_iter().collect();
    layers.sort();
    Ok(Half { manifest, pack: crate::output::layers::pack_bytes_at(&layers, QUICK)? })
}

enum State {
    Untried,
    Ready(Arc<Vec<Index>>),
    Failed(Instant),
}

static STATE: Mutex<State> = Mutex::new(State::Untried);

/// The two indexes, read once a run, and again ten minutes after they could not be.
fn indexes() -> Option<Arc<Vec<Index>>> {
    let mut state = STATE.lock().unwrap();
    match &*state {
        State::Ready(i) => return Some(i.clone()),
        State::Failed(t) if t.elapsed() < RETRY_AFTER => return None,
        _ => {}
    }
    let read = || -> Result<Vec<Index>> { HALVES.iter().map(|h| Index::from_json(&http().get_text(&format!("{}/{h}/{INDEX}", base()))?)).collect() };
    match read() {
        Ok(i) => {
            crate::term::info(&format!("Hugging Face dataset: {} airports already built", i[0].airports.len().max(i[1].airports.len())));
            let i = Arc::new(i);
            *state = State::Ready(i.clone());
            Some(i)
        }
        Err(e) => {
            crate::term::warn(&format!("The Hugging Face dataset could not be read, so airports are built here: {e:#}"));
            *state = State::Failed(Instant::now());
            None
        }
    }
}

/// Read the indexes now, in the background, so that the first airport does not wait on them.
pub fn warm() {
    if is_on() {
        std::thread::spawn(|| {
            let _ = indexes();
        });
    }
}

/// Whether the dataset has `icao`, from the indexes if they have been read; never waits.
pub fn has(icao: &str) -> bool {
    if !is_on() {
        return false;
    }
    let Ok(state) = STATE.try_lock() else { return false };
    matches!(&*state, State::Ready(i) if i.iter().any(|x| x.airports.contains_key(&icao.to_uppercase())))
}

/// Put `icao` into `out` from the dataset. False when the dataset is off, cannot be
/// reached, or does not have it: then it is built here, as before.
pub fn install(out: &Path, icao: &str) -> Result<bool> {
    if !is_on() {
        return Ok(false);
    }
    let icao = icao.to_uppercase();
    let Some(idx) = indexes() else { return Ok(false) };
    if !idx.iter().any(|i| i.airports.contains_key(&icao)) {
        return Ok(false);
    }
    let t0 = Instant::now();
    let half = |h: &str, i: &Index| -> Result<Option<Half>> {
        let Some(&(offset, len)) = i.airports.get(&icao) else { return Ok(None) };
        let piece = http().get_range(&format!("{}/{h}/{ARCHIVE}", base()), offset, len)?;
        if piece.len() as u64 != len {
            *STATE.lock().unwrap() = State::Failed(Instant::now());
            bail!("{icao}: {h}/{ARCHIVE} sent {} bytes of {len}; the archive has changed since its index", piece.len());
        }
        let files = files_in(&piece, &icao).inspect_err(|_| *STATE.lock().unwrap() = State::Failed(Instant::now()))?;
        Ok(Some(Half::from_files(files, &icao)?))
    };
    let (gateway, osm) = std::thread::scope(|s| {
        let osm = s.spawn(|| half(HALVES[1], &idx[1]));
        let gateway = half(HALVES[0], &idx[0]);
        (gateway, osm.join().unwrap_or_else(|_| Err(anyhow!("{icao}: the OpenStreetMap half's download stopped"))))
    });
    let (gateway, osm) = (gateway?, osm?);
    let which = match (gateway.as_ref().is_some_and(|g| g.sources().any(|s| s == source::XPLANE)), osm.is_some()) {
        (true, true) => "Gateway and OpenStreetMap",
        (true, false) => "Gateway",
        _ => "OpenStreetMap",
    };
    let ap = merge(gateway, osm)?;
    let dir = out.join(&icao);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    crate::output::layers::write_pack_bytes(&dir, &ap.pack)?;
    std::fs::write(dir.join("manifest.json"), serde_json::to_string_pretty(&ap.manifest)?).with_context(|| format!("write {}", dir.join("manifest.json").display()))?;
    crate::term::success(&format!("[{icao}] Downloaded already built from the Hugging Face dataset ({which}) in {}", crate::term::human_secs(t0.elapsed().as_secs_f64())));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, data: &[u8]) -> Vec<u8> {
        let mut h = vec![0u8; BLOCK as usize];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|b| *b as u32).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        h.extend_from_slice(data);
        h.resize(BLOCK as usize + padded(data.len() as u64) as usize, 0);
        h
    }

    fn pack(layers: &[(&str, &str)]) -> Vec<u8> {
        crate::output::layers::pack_bytes(&layers.iter().map(|(n, t)| (n.to_string(), t.to_string())).collect::<Vec<_>>()).unwrap()
    }

    const NONE: &str = r#"{"type":"FeatureCollection","features":[]}"#;
    const ONE: &str = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}}]}"#;
    const TWO: &str = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[3,4]},"properties":{}}]}"#;

    #[test]
    fn an_archive_is_indexed_by_airport_and_each_piece_holds_only_that_airport() {
        let mut tar = Vec::new();
        for (icao, size) in [("AAAA", 10usize), ("BBBB", 700)] {
            let mut pax = entry("././@PaxHeader", b"28 mtime=1760123456.789
");
            pax[156] = b'x';
            tar.extend(pax);
            tar.extend(entry(&format!("{icao}/layers.json.br"), &vec![7u8; size]));
            tar.extend(entry(&format!("{icao}/manifest.json"), format!(r#"{{"icao":"{icao}"}}"#).as_bytes()));
        }
        tar.extend(entry("index.json", b"{}"));
        tar.extend(vec![0u8; 1024]);
        let path = std::env::temp_dir().join(format!("amdb-dataset-{}.tar", std::process::id()));
        std::fs::write(&path, &tar).unwrap();
        let index = index_archive(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(index.size, tar.len() as u64);
        assert_eq!(index.airports.len(), 2);
        assert_eq!(Index::from_json(&index.to_json()).unwrap(), index);
        let (o, l) = index.airports["BBBB"];
        let files = files_in(&tar[o as usize..(o + l) as usize], "bbbb").unwrap();
        assert_eq!(files["layers.json.br"], vec![7u8; 700]);
        assert_eq!(files["manifest.json"], br#"{"icao":"BBBB"}"#.to_vec());
        let (o, l) = index.airports["AAAA"];
        assert!(files_in(&tar[o as usize..(o + l) as usize], "BBBB").is_err(), "a piece of another airport is an index that does not fit");
    }

    #[test]
    fn the_gateway_airfield_takes_openstreetmaps_buildings_and_keeps_its_own_runways() {
        let gateway = Half { manifest: json!({"sources": ["ourairports", "xplane"], "bbox": [0.0, 0.0, 1.0, 1.0], "layers": {"water": {"count": 0, "sources": []}}}), pack: pack(&[("runwayelement", ONE), ("water", NONE), ("serviceroad", ONE)]) };
        let osm = Half { manifest: json!({"sources": ["osm"], "bbox": [-1.0, 0.5, 0.5, 2.0], "layers": {"water": {"count": 1, "sources": ["osm"]}}}), pack: pack(&[("runwayelement", TWO), ("water", TWO), ("serviceroad", NONE)]) };
        let ap = merge(Some(gateway), Some(osm)).unwrap();
        let layers = crate::output::layers::unpack_bytes(&ap.pack).unwrap();
        assert_eq!(layers["runwayelement"], ONE);
        assert_eq!(layers["water"], TWO);
        assert_eq!(layers["serviceroad"], ONE, "an empty OpenStreetMap layer leaves the Gateway's");
        assert_eq!(ap.manifest["sources"], json!(["osm", "ourairports", "xplane"]));
        assert_eq!(ap.manifest["layers"]["water"]["count"], 1);
        assert_eq!(ap.manifest["bbox"], json!([-1.0, 0.0, 1.0, 2.0]));
    }

    #[test]
    fn without_the_gateway_openstreetmap_has_the_airport_and_with_nothing_to_add_the_gateway_is_untouched() {
        let reference_only = Half { manifest: json!({"sources": ["ourairports"]}), pack: pack(&[("runwayelement", NONE)]) };
        let osm = Half { manifest: json!({"sources": ["osm"]}), pack: pack(&[("runwayelement", TWO)]) };
        assert_eq!(merge(Some(reference_only), Some(osm.clone())).unwrap().pack, osm.pack);
        let gateway = Half { manifest: json!({"sources": ["xplane"]}), pack: pack(&[("runwayelement", ONE)]) };
        let quiet = Half { manifest: json!({"sources": ["osm"]}), pack: pack(&[("water", NONE)]) };
        assert_eq!(merge(Some(gateway.clone()), Some(quiet)).unwrap().pack, gateway.pack);
        assert_eq!(merge(Some(gateway.clone()), None).unwrap().pack, gateway.pack);
        assert!(merge(None, None).is_err());
    }
}
