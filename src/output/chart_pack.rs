//! An airport's set of charts (`<ICAO>/charts`, `<ICAO>/charts-NNNN`) packed into one file,
//! `<ICAO>/<set>.pack`, about two fifths of the space the PDFs take one by one, and every
//! file in it given back to the byte.
//!
//! A chart's page is deflated in its PDF, and pages deflated one at a time leave nothing
//! for the next to share: the same terrain, frames and words over an airport's every chart.
//! So the pack keeps each page inflated, and the whole set in one brotli stream, which finds
//! what the charts have in common. A page is deflated again when its PDF is read, in the
//! same way it was drawn (`canvas::deflate`), which gives back the bytes it had: that is
//! checked of every page as it is packed, and a page it is not true of is kept as it was.
//!
//! What reads a set reads a file of it by its path as though the set were still a folder
//! (`read`, `names`), and a folder of the set drawn since is read before the pack.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

const MAGIC: &[u8; 8] = b"AMDBCPK1";
/// Brotli's best, with a window wide enough for an airport's whole set as it is read.
const QUALITY: u32 = 11;
const WINDOW: u32 = 24;

/// Part of a file: as it is, or a page kept inflated and deflated again when read.
#[derive(Clone, PartialEq, Debug)]
enum Part {
    Kept(Vec<u8>),
    Inflated(Vec<u8>),
}

type Files = Arc<HashMap<String, Vec<Part>>>;

/// Where a set is packed: `<airport>/<set>.pack`.
pub fn pack_path(airport: &Path, set: &str) -> PathBuf {
    airport.join(format!("{set}.pack"))
}

/// A file's parts: a PDF's deflated streams inflated, each only where deflating it again
/// gives back exactly what it was; anything else kept as it is.
fn split(bytes: &[u8], name: &str) -> Vec<Part> {
    if !name.to_ascii_lowercase().ends_with(".pdf") {
        return vec![Part::Kept(bytes.to_vec())];
    }
    let mut parts = Vec::new();
    let (mut kept_from, mut at) = (0usize, 0usize);
    while let Some(found) = find(&bytes[at..], b"stream\n") {
        let start = at + found + b"stream\n".len();
        at = start;
        // Not the end of an `endstream`: the keyword begins a line or follows `>>`.
        if start >= 10 && &bytes[start - 10..start - 7] == b"end" {
            continue;
        }
        let dict = &bytes[start.saturating_sub(2048)..start];
        let Some(len) = last_length(dict) else { continue };
        let end = start + len;
        if end > bytes.len() || !bytes[end..].starts_with(b"\nendstream") && !bytes[end..].starts_with(b"endstream") {
            continue;
        }
        if find(dict, b"/FlateDecode").is_none() {
            at = end;
            continue;
        }
        let stream = &bytes[start..end];
        let mut inflated = Vec::new();
        if flate2::read::ZlibDecoder::new(stream).read_to_end(&mut inflated).is_ok() && super::canvas::deflate(&inflated) == stream {
            parts.push(Part::Kept(bytes[kept_from..start].to_vec()));
            parts.push(Part::Inflated(inflated));
            kept_from = end;
        }
        at = end;
    }
    parts.push(Part::Kept(bytes[kept_from..].to_vec()));
    parts
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The `/Length` of the last dictionary in `dict`, the one before a stream.
fn last_length(dict: &[u8]) -> Option<usize> {
    let at = dict.windows(8).rposition(|w| w == b"/Length ")? + 8;
    let digits: String = dict[at..].iter().take_while(|b| b.is_ascii_digit()).map(|&b| b as char).collect();
    digits.parse().ok()
}

fn join(parts: &[Part]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        match p {
            Part::Kept(b) => out.extend_from_slice(b),
            Part::Inflated(b) => out.extend_from_slice(&super::canvas::deflate(b)),
        }
    }
    out
}

fn encode(files: &[(String, Vec<Part>)]) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    raw.extend_from_slice(MAGIC);
    raw.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for (name, parts) in files {
        raw.extend_from_slice(&(name.len() as u16).to_le_bytes());
        raw.extend_from_slice(name.as_bytes());
        raw.extend_from_slice(&(parts.len() as u32).to_le_bytes());
        for p in parts {
            let (kind, b) = match p {
                Part::Kept(b) => (0u8, b),
                Part::Inflated(b) => (1u8, b),
            };
            raw.push(kind);
            raw.extend_from_slice(&(b.len() as u32).to_le_bytes());
            raw.extend_from_slice(b);
        }
    }
    let mut w = brotli::CompressorWriter::new(Vec::new(), 1 << 16, QUALITY, WINDOW);
    w.write_all(&raw)?;
    Ok(w.into_inner())
}

fn decode(packed: &[u8]) -> Result<HashMap<String, Vec<Part>>> {
    let mut raw = Vec::new();
    brotli::Decompressor::new(packed, 1 << 16).read_to_end(&mut raw)?;
    let mut at = 0usize;
    let mut take = |n: usize| -> Result<&[u8]> {
        let s = raw.get(at..at + n).ok_or_else(|| anyhow!("a chart pack cut short"))?;
        at += n;
        Ok(s)
    };
    if take(8)? != MAGIC {
        return Err(anyhow!("not a chart pack"));
    }
    let u32_of = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
    let count = u32_of(take(4)?);
    let mut files = HashMap::with_capacity(count);
    for _ in 0..count {
        let n = take(2)?;
        let name_len = u16::from_le_bytes([n[0], n[1]]) as usize;
        let name = String::from_utf8(take(name_len)?.to_vec())?;
        let parts_n = u32_of(take(4)?);
        let mut parts = Vec::with_capacity(parts_n);
        for _ in 0..parts_n {
            let kind = take(1)?[0];
            let len = u32_of(take(4)?);
            let b = take(len)?.to_vec();
            parts.push(if kind == 1 { Part::Inflated(b) } else { Part::Kept(b) });
        }
        files.insert(name, parts);
    }
    Ok(files)
}

/// A pack's files, the last few read kept while unchanged: a flight bag asks for one chart
/// after another of the same airport.
fn files_of(pack: &Path) -> Option<Files> {
    static KEPT: OnceLock<Mutex<Vec<(PathBuf, std::time::SystemTime, Files)>>> = OnceLock::new();
    let kept = KEPT.get_or_init(Default::default);
    let stamp = std::fs::metadata(pack).ok()?.modified().ok()?;
    if let Some(f) = kept.lock().unwrap().iter().find(|(p, t, _)| p == pack && *t == stamp).map(|k| k.2.clone()) {
        return Some(f);
    }
    let files: Files = Arc::new(decode(&std::fs::read(pack).ok()?).ok()?);
    let mut k = kept.lock().unwrap();
    k.retain(|(p, _, _)| p != pack);
    if k.len() >= 4 {
        k.remove(0);
    }
    k.push((pack.to_path_buf(), stamp, files.clone()));
    Some(files)
}

/// `<airport>/<set>/<name>` as the set's folder and pack: (airport, set, name).
fn place(path: &Path) -> Option<(&Path, String, String)> {
    let name = path.file_name()?.to_str()?.to_string();
    let set_dir = path.parent()?;
    let set = set_dir.file_name()?.to_str()?.to_string();
    Some((set_dir.parent()?, set, name))
}

/// A file of a set by its path as a folder's: from the folder where it is there (drawn
/// since the set was packed), from the pack otherwise.
pub fn read(path: &Path) -> Option<Vec<u8>> {
    if let Ok(b) = std::fs::read(path) {
        return Some(b);
    }
    let (airport, set, name) = place(path)?;
    files_of(&pack_path(airport, &set))?.get(&name).map(|parts| join(parts))
}

/// The names of a set's files: its folder's where it has one, its pack's otherwise.
pub fn names(airport: &Path, set: &str) -> Vec<String> {
    if let Ok(rd) = std::fs::read_dir(airport.join(set)) {
        let out: Vec<String> = rd.flatten().filter_map(|e| e.file_name().to_str().map(str::to_string)).collect();
        if !out.is_empty() {
            return out;
        }
    }
    files_of(&pack_path(airport, set)).map(|f| f.keys().cloned().collect()).unwrap_or_default()
}

/// Pack an airport's set folder into its pack, then remove the folder. A folder of the
/// whole set (one with its `cycle.txt`, as a run leaves it) takes the pack's place; one of
/// some charts only joins what the pack holds. Every file is read back from the new pack
/// and must be the same to the byte before the folder goes. (bytes before, bytes after), or
/// None when there was no folder.
pub fn pack_set(airport: &Path, set: &str) -> Result<Option<(u64, u64)>> {
    let dir = airport.join(set);
    let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(None) };
    let paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    if paths.is_empty() {
        let _ = std::fs::remove_dir(&dir);
        return Ok(None);
    }
    let pack = pack_path(airport, set);
    let mut files: HashMap<String, Vec<Part>> = HashMap::new();
    let mut before = 0u64;
    let whole = dir.join("cycle.txt").is_file();
    if !whole && pack.is_file() {
        before += std::fs::metadata(&pack)?.len();
        files = decode(&std::fs::read(&pack)?).with_context(|| format!("read {}", pack.display()))?;
    }
    let mut originals: Vec<(String, Vec<u8>)> = Vec::new();
    for p in &paths {
        let name = p.file_name().and_then(|n| n.to_str()).ok_or_else(|| anyhow!("{}: a name that is not text", p.display()))?.to_string();
        let bytes = std::fs::read(p)?;
        before += bytes.len() as u64;
        let parts = split(&bytes, &name);
        if join(&parts) != bytes {
            return Err(anyhow!("{}: did not come back the same", p.display()));
        }
        files.insert(name.clone(), parts);
        originals.push((name, bytes));
    }
    let mut sorted: Vec<(String, Vec<Part>)> = files.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let packed = encode(&sorted)?;
    let tmp = airport.join(format!("{set}.pack.tmp"));
    std::fs::write(&tmp, &packed)?;
    let back = decode(&std::fs::read(&tmp)?)?;
    for (name, bytes) in &originals {
        if back.get(name).map(|p| join(p)).as_ref() != Some(bytes) {
            let _ = std::fs::remove_file(&tmp);
            return Err(anyhow!("{}/{name}: did not read back the same from the pack", dir.display()));
        }
    }
    std::fs::rename(&tmp, &pack)?;
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    let _ = std::fs::remove_dir(&dir);
    Ok(Some((before, packed.len() as u64)))
}

/// The chart sets an airport keeps in folders (`charts`, `charts-NNNN`).
pub fn set_folders(airport: &Path) -> Vec<String> {
    std::fs::read_dir(airport)
        .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().to_str().map(str::to_string)).filter(|n| n == "charts" || n.starts_with("charts-")).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chart's PDF as the charts write them: a deflated page, a dictionary with its length.
    fn pdf(text: &str) -> Vec<u8> {
        let page = super::super::canvas::deflate(text.as_bytes());
        let mut out = b"%PDF-1.7\n1 0 obj\n<<\n  /Length ".to_vec();
        out.extend_from_slice(format!("{}\n  /Filter /FlateDecode\n>>\nstream\n", page.len()).as_bytes());
        out.extend_from_slice(&page);
        out.extend_from_slice(b"\nendstream\nendobj\n2 0 obj\n<< /Length 5 >>\nstream\nplain\nendstream\nendobj\n%%EOF\n");
        out
    }

    #[test]
    fn a_set_packed_reads_back_to_the_byte() {
        let airport = std::env::temp_dir().join(format!("amdb-chart-pack-{}", std::process::id())).join("LFPG");
        let _ = std::fs::remove_dir_all(airport.parent().unwrap());
        let dir = airport.join("charts-2610");
        std::fs::create_dir_all(&dir).unwrap();
        let a = pdf(&"0 0 m 10 10 l S\n".repeat(400));
        let b = pdf(&"0 0 m 10 10 l S\n".repeat(399));
        std::fs::write(dir.join("ILS RWY 09L.pdf"), &a).unwrap();
        std::fs::write(dir.join("ILS RWY 09L.geo.json"), "{}").unwrap();
        std::fs::write(dir.join("ILS RWY 27R.pdf"), &b).unwrap();
        std::fs::write(dir.join("cycle.txt"), "AIRAC 2610, in force from 01 OCT 2026").unwrap();
        // The page is kept inflated, and comes back deflated as it was.
        assert!(split(&a, "ILS RWY 09L.pdf").iter().any(|p| matches!(p, Part::Inflated(_))));
        assert_eq!(join(&split(&a, "x.pdf")), a);

        let (before, after) = pack_set(&airport, "charts-2610").unwrap().unwrap();
        assert!(after < before);
        assert!(!dir.exists() && pack_path(&airport, "charts-2610").is_file());
        assert_eq!(read(&dir.join("ILS RWY 09L.pdf")).unwrap(), a);
        assert_eq!(read(&dir.join("ILS RWY 27R.pdf")).unwrap(), b);
        assert_eq!(read(&dir.join("ILS RWY 09L.geo.json")).unwrap(), b"{}");
        let mut n = names(&airport, "charts-2610");
        n.sort();
        assert_eq!(n, ["ILS RWY 09L.geo.json", "ILS RWY 09L.pdf", "ILS RWY 27R.pdf", "cycle.txt"]);
        assert_eq!(read(&dir.join("VOR RWY 09L.pdf")), None);

        // Charts drawn since, without the set's cycle.txt: they join it.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("VOR RWY 09L.pdf"), &b).unwrap();
        pack_set(&airport, "charts-2610").unwrap();
        assert_eq!(names(&airport, "charts-2610").len(), 5);
        assert_eq!(read(&dir.join("VOR RWY 09L.pdf")).unwrap(), b);
        let _ = std::fs::remove_dir_all(airport.parent().unwrap());
    }
}
