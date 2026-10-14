//! An airport's layers on disk, in either of the two forms a built airport keeps them in:
//!
//! - packed: `<ICAO>/layers.json.br`, one JSON object of every layer's FeatureCollection by
//!   name, compressed with brotli at its best (a tenth of the loose files' space; gzip, as
//!   the first packs were written, `layers.json.gz`, takes a third more). One file an
//!   airport where they are forty-five: a cache of every airport in the world is forty
//!   thousand files to copy, not two million;
//! - loose: `<ICAO>/<layer>.geojson`, a file a layer, as airports built before the packed
//!   form are, and as `--format geojson` still writes them for whoever reads them directly.
//!
//! Whatever reads a layer reads it through `read`, which takes either.

use anyhow::{anyhow, Context, Result};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The packed form's file name.
pub const PACK: &str = "layers.json.br";
/// The first packs', gzipped: read still, and turned into `PACK` by `pack`.
pub const GZIP_PACK: &str = "layers.json.gz";
/// Brotli's best, and its window: about 70 ms an airport, three seconds for the biggest.
const QUALITY: u32 = 11;
const WINDOW: u32 = 22;

type Texts = Rc<HashMap<String, String>>;

thread_local! {
    /// The last pack read on this thread, kept while its file is unchanged: callers read a
    /// layer at a time, and opening the pack for each would unzip it forty-five times.
    static LAST: RefCell<Option<(PathBuf, std::time::SystemTime, u64, Texts)>> = const { RefCell::new(None) };
}

/// Every layer in a pack, by name.
fn unpack(path: &Path) -> Result<HashMap<String, String>> {
    let mut json = String::new();
    let file = std::fs::File::open(path)?;
    if path.extension().is_some_and(|e| e == "br") {
        brotli::Decompressor::new(file, 1 << 16).read_to_string(&mut json)
    } else {
        flate2::read::GzDecoder::new(file).read_to_string(&mut json)
    }
    .with_context(|| format!("unpack {}", path.display()))?;
    let raw: HashMap<String, Box<serde_json::value::RawValue>> = serde_json::from_str(&json).with_context(|| format!("read {}", path.display()))?;
    Ok(raw.into_iter().map(|(k, v)| (k, v.get().to_string())).collect())
}

fn pack_of(dir: &Path) -> Option<Texts> {
    let path = [PACK, GZIP_PACK].iter().map(|n| dir.join(n)).find(|p| p.is_file())?;
    let meta = std::fs::metadata(&path).ok()?;
    let stamp = (meta.modified().ok()?, meta.len());
    let hit = LAST.with(|l| l.borrow().as_ref().filter(|(p, t, n, _)| *p == path && (*t, *n) == stamp).map(|c| c.3.clone()));
    if hit.is_some() {
        return hit;
    }
    let texts: Texts = Rc::new(unpack(&path).ok()?);
    LAST.with(|l| *l.borrow_mut() = Some((path, stamp.0, stamp.1, texts.clone())));
    Some(texts)
}

/// One layer's GeoJSON text (e.g. `runwaythreshold`), from whichever form the airport in
/// `dir` is kept in. None when it has no such layer.
pub fn read(dir: &Path, name: &str) -> Option<String> {
    match pack_of(dir) {
        Some(texts) => texts.get(name).cloned(),
        None => std::fs::read_to_string(dir.join(format!("{name}.geojson"))).ok(),
    }
}

/// Whether an airport's layers are packed.
pub fn is_packed(dir: &Path) -> bool {
    dir.join(PACK).is_file() || dir.join(GZIP_PACK).is_file()
}

/// Write layers, (name, FeatureCollection text), as an airport's pack, in their order.
pub fn write_pack(dir: &Path, layers: &[(String, String)]) -> Result<u64> {
    let mut json = String::with_capacity(layers.iter().map(|(n, t)| n.len() + t.len() + 4).sum::<usize>() + 2);
    json.push('{');
    for (i, (name, text)) in layers.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&serde_json::to_string(name)?);
        json.push(':');
        json.push_str(text.trim_start_matches('\u{feff}'));
    }
    json.push('}');
    let mut w = brotli::CompressorWriter::new(Vec::new(), 1 << 16, QUALITY, WINDOW);
    w.write_all(json.as_bytes())?;
    let bytes = w.into_inner();
    // Written beside it and put in its place, so that a reader never sees half of one.
    let tmp = dir.join(format!("{PACK}.tmp"));
    std::fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(PACK)).with_context(|| format!("write {}", dir.join(PACK).display()))?;
    let _ = std::fs::remove_file(dir.join(GZIP_PACK));
    LAST.with(|l| *l.borrow_mut() = None);
    Ok(bytes.len() as u64)
}

/// The loose layer files in an airport folder.
fn loose(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "geojson")).collect()).unwrap_or_default()
}

/// Remove an airport's loose layer files, once a pack holds them.
pub fn remove_loose(dir: &Path) {
    for p in loose(dir) {
        let _ = std::fs::remove_file(p);
    }
}

/// Pack a built airport's loose layer files into one, or a gzipped pack again with brotli,
/// and remove what it was packed from once the pack has been read back and holds every
/// layer. Returns (bytes before, bytes after), or None when there was nothing to do.
pub fn pack(dir: &Path) -> Result<Option<(u64, u64)>> {
    let files = loose(dir);
    if files.is_empty() {
        let gz = dir.join(GZIP_PACK);
        if !gz.is_file() || dir.join(PACK).is_file() {
            return Ok(None);
        }
        let before = std::fs::metadata(&gz)?.len();
        let mut layers: Vec<(String, String)> = unpack(&gz)?.into_iter().collect();
        layers.sort();
        let after = write_pack(dir, &layers)?;
        let back = unpack(&dir.join(PACK))?;
        if layers.iter().any(|(name, text)| back.get(name) != Some(text)) {
            let _ = std::fs::remove_file(dir.join(PACK));
            return Err(anyhow!("{}: the layers did not read back the same from the new pack", dir.display()));
        }
        return Ok(Some((before, after)));
    }
    let mut layers = Vec::with_capacity(files.len());
    let mut before = 0;
    for p in &files {
        let text = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
        // A pack is one JSON object: a layer that does not parse would spoil the others.
        serde_json::from_str::<serde::de::IgnoredAny>(text.trim_start_matches('\u{feff}')).with_context(|| format!("{} is not JSON", p.display()))?;
        before += text.len() as u64;
        let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        layers.push((name, text));
    }
    layers.sort();
    let after = write_pack(dir, &layers)?;
    let back = unpack(&dir.join(PACK))?;
    for (name, text) in &layers {
        if back.get(name).map(String::as_str) != Some(text.trim_start_matches('\u{feff}')) {
            let _ = std::fs::remove_file(dir.join(PACK));
            return Err(anyhow!("{}: {name} did not read back the same from the pack", dir.display()));
        }
    }
    for p in files {
        std::fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
    }
    Ok(Some((before, after)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gzip against brotli on a big real airport. Run by hand:
    /// `cargo test --release -- --ignored compressors_compared --nocapture`.
    #[test]
    #[ignore]
    fn compressors_compared() {
        let texts = unpack(Path::new("E:/OANS Cache/airports/KJFK/layers.json.gz")).unwrap();
        let json: String = texts.values().cloned().collect::<Vec<_>>().join(",");
        let t = std::time::Instant::now();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(json.as_bytes()).unwrap();
        println!("json {} B; gzip 6: {} B in {:?}", json.len(), gz.finish().unwrap().len(), t.elapsed());
        for q in [5u32, 7, 9, 11] {
            let t = std::time::Instant::now();
            let mut w = brotli::CompressorWriter::new(Vec::new(), 1 << 16, q, 22);
            w.write_all(json.as_bytes()).unwrap();
            let out = w.into_inner();
            let t2 = std::time::Instant::now();
            let mut back = String::new();
            brotli::Decompressor::new(&out[..], 1 << 16).read_to_string(&mut back).unwrap();
            println!("brotli {q}: {} B in {:?}, read back in {:?}", out.len(), t.elapsed() - t2.elapsed(), t2.elapsed());
        }
    }

    #[test]
    fn a_layer_reads_the_same_loose_or_packed() {
        let dir = std::env::temp_dir().join(format!("amdb-layers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rwy = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.5,2.25]},"properties":{"idthr":"09"}}]}"#;
        let empty = r#"{"type":"FeatureCollection","features":[]}"#;
        std::fs::write(dir.join("runwaythreshold.geojson"), rwy).unwrap();
        std::fs::write(dir.join("hotspot.geojson"), empty).unwrap();
        std::fs::write(dir.join("manifest.json"), "{}").unwrap();
        assert_eq!(read(&dir, "runwaythreshold").as_deref(), Some(rwy));

        let (before, after) = pack(&dir).unwrap().unwrap();
        assert_eq!(before, (rwy.len() + empty.len()) as u64);
        assert!(after > 0);
        assert!(is_packed(&dir));
        assert!(!dir.join("runwaythreshold.geojson").exists() && dir.join("manifest.json").exists());
        assert_eq!(read(&dir, "runwaythreshold").as_deref(), Some(rwy));
        assert_eq!(read(&dir, "hotspot").as_deref(), Some(empty));
        assert_eq!(read(&dir, "water"), None);
        // Nothing loose left: a second pack has nothing to do.
        assert!(pack(&dir).unwrap().is_none());
        assert!(dir.join(PACK).is_file() && !dir.join(GZIP_PACK).exists());

        // A pack written again is read again, not the copy kept from before.
        write_pack(&dir, &[("runwaythreshold".into(), empty.into())]).unwrap();
        assert_eq!(read(&dir, "runwaythreshold").as_deref(), Some(empty));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pack as the first ones were written, gzipped, reads, and is packed again with brotli.
    #[test]
    fn a_gzipped_pack_is_read_and_packed_again() {
        let dir = std::env::temp_dir().join(format!("amdb-layers-gz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rwy = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.5,2.25]},"properties":{}}]}"#;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(format!(r#"{{"runwaythreshold":{rwy}}}"#).as_bytes()).unwrap();
        std::fs::write(dir.join(GZIP_PACK), gz.finish().unwrap()).unwrap();
        assert!(is_packed(&dir));
        assert_eq!(read(&dir, "runwaythreshold").as_deref(), Some(rwy));
        assert!(pack(&dir).unwrap().is_some());
        assert!(dir.join(PACK).is_file() && !dir.join(GZIP_PACK).exists());
        assert_eq!(read(&dir, "runwaythreshold").as_deref(), Some(rwy));
        assert!(pack(&dir).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_layer_that_is_not_json_is_not_packed() {
        let dir = std::env::temp_dir().join(format!("amdb-layers-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("runwaythreshold.geojson"), "{ not json").unwrap();
        assert!(pack(&dir).is_err());
        assert!(dir.join("runwaythreshold.geojson").exists() && !is_packed(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
