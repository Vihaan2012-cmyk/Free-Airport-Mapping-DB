//! Read apt.dat from a local X-Plane installation or an explicit file.
//!
//! Search order inside an X-Plane root: `Custom Scenery/*/Earth nav data/apt.dat`
//! (scenery_packs.ini order is not honoured; the first file containing the ICAO wins),
//! then `Global Scenery/Global Airports/Earth nav data/apt.dat`, then
//! `Resources/default scenery/default apt dat/Earth nav data/apt.dat`.

use anyhow::{Context, Result};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Candidate apt.dat files under an X-Plane root, most specific first.
pub fn candidate_files(xplane_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(xplane_root.join("Custom Scenery")) {
        let mut packs: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
        packs.sort();
        for p in packs {
            let f = p.join("Earth nav data").join("apt.dat");
            if f.is_file() {
                out.push(f);
            }
        }
    }
    for rel in ["Global Scenery/Global Airports/Earth nav data/apt.dat", "Resources/default scenery/default apt dat/Earth nav data/apt.dat"] {
        let f = xplane_root.join(rel);
        if f.is_file() {
            out.push(f);
        }
    }
    out
}

/// Extract the text block for one airport from a (possibly huge) apt.dat file.
/// Returns `Ok(None)` if the ICAO is not in the file.
pub fn extract_airport_block(file: &Path, icao: &str) -> Result<Option<String>> {
    let f = fs::File::open(file).with_context(|| format!("open {}", file.display()))?;
    let mut r = BufReader::with_capacity(1 << 20, f);
    let icao_up = icao.to_uppercase();
    let mut line = Vec::new();
    let mut block: Option<String> = None;
    loop {
        line.clear();
        let n = r.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        let s = String::from_utf8_lossy(&line);
        let t = s.trim_start();
        let is_header = t.starts_with("1 ") || t.starts_with("16 ") || t.starts_with("17 ");
        if is_header {
            if block.is_some() {
                break; // next airport reached
            }
            let code = t.split_whitespace().nth(4).unwrap_or("");
            if code.eq_ignore_ascii_case(&icao_up) {
                block = Some(String::from("I\n1200\n"));
            }
        }
        if let Some(b) = block.as_mut() {
            if t.starts_with("99") && t.trim().len() <= 3 {
                break;
            }
            b.push_str(&s);
        }
    }
    Ok(block.map(|mut b| {
        b.push_str("99\n");
        b
    }))
}

/// Find and extract `icao` from any apt.dat under an X-Plane root.
pub fn find_in_root(xplane_root: &Path, icao: &str) -> Result<Option<(PathBuf, String)>> {
    for f in candidate_files(xplane_root) {
        if let Some(block) = extract_airport_block(&f, icao)? {
            return Ok(Some((f, block)));
        }
    }
    Ok(None)
}

/// The X-Plane 12 (then 11) install recorded by the installer in
/// `%LOCALAPPDATA%\x-plane_install_12.txt`, if it still exists.
pub fn detect_install() -> Option<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    for name in ["x-plane_install_12.txt", "x-plane_install_11.txt"] {
        let Ok(text) = fs::read_to_string(Path::new(&local).join(name)) else { continue };
        for line in text.lines() {
            let p = PathBuf::from(line.trim().trim_end_matches(['/', '\\']));
            if !line.trim().is_empty() && (p.join("Resources").is_dir() || p.join("Custom Scenery").is_dir()) {
                return Some(p);
            }
        }
    }
    None
}

/// One apt.dat file in the offset index, with the size/mtime it was scanned at.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct IndexedFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
}

/// Byte offsets of every airport in every apt.dat under an X-Plane root, so the
/// multi-hundred-megabyte Global Airports file is scanned once, not once per airport.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct AptIndex {
    pub files: Vec<IndexedFile>,
    /// ICAO -> (file index, byte offset, byte length). The first file (Custom Scenery
    /// before Global Airports) wins.
    pub airports: std::collections::HashMap<String, (usize, u64, u64)>,
}

fn file_stamp(p: &Path) -> IndexedFile {
    let m = fs::metadata(p).ok();
    IndexedFile {
        path: p.to_path_buf(),
        size: m.as_ref().map(|m| m.len()).unwrap_or(0),
        mtime: m.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0),
    }
}

fn index_cache_path(root: &Path) -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    let mut h: u64 = 1469598103934665603;
    for b in root.to_string_lossy().to_lowercase().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    base.join("amdbgen").join("index").join(format!("xplane-apt-{h:016x}.json"))
}

impl AptIndex {
    /// Scan (or reload the cached scan of) every apt.dat under `root`.
    pub fn for_root(root: &Path) -> Result<AptIndex> {
        let files: Vec<IndexedFile> = candidate_files(root).iter().map(|p| file_stamp(p)).collect();
        let cache = index_cache_path(root);
        if let Ok(text) = fs::read_to_string(&cache) {
            if let Ok(idx) = serde_json::from_str::<AptIndex>(&text) {
                if idx.files == files {
                    return Ok(idx);
                }
            }
        }
        let t0 = std::time::Instant::now();
        let mut idx = AptIndex { files: files.clone(), airports: Default::default() };
        for (fi, f) in files.iter().enumerate() {
            crate::term::step(None, &format!("Indexing {} ({})", f.path.display(), crate::term::human_bytes(f.size)));
            scan_file(&f.path, fi, &mut idx.airports)?;
        }
        crate::term::info(&format!("X-Plane install indexed: {} airports in {} apt.dat file(s) in {}", idx.airports.len(), files.len(), crate::term::human_secs(t0.elapsed().as_secs_f64())));
        if let Some(parent) = cache.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&cache, serde_json::to_string(&idx)?);
        Ok(idx)
    }

    pub fn contains(&self, icao: &str) -> bool {
        self.airports.contains_key(&icao.to_uppercase())
    }

    /// The airport's apt.dat text (with a header and trailing 99), and the file it came from.
    pub fn extract(&self, icao: &str) -> Result<Option<(PathBuf, String)>> {
        let Some(&(fi, off, len)) = self.airports.get(&icao.to_uppercase()) else { return Ok(None) };
        let path = &self.files[fi].path;
        let mut f = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        f.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; len as usize];
        f.read_exact(&mut buf).with_context(|| format!("read {} at {off}", path.display()))?;
        let mut text = String::from("I\n1200\n");
        text.push_str(&String::from_utf8_lossy(&buf));
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("99\n");
        Ok(Some((path.clone(), text)))
    }
}

/// Record the byte range of every airport block in one apt.dat.
fn scan_file(path: &Path, fi: usize, out: &mut std::collections::HashMap<String, (usize, u64, u64)>) -> Result<()> {
    let f = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut r = BufReader::with_capacity(4 << 20, f);
    let mut line = Vec::new();
    let mut pos: u64 = 0;
    let mut cur: Option<(String, u64)> = None;
    let close = |cur: &mut Option<(String, u64)>, end: u64, out: &mut std::collections::HashMap<String, (usize, u64, u64)>| {
        if let Some((icao, start)) = cur.take() {
            out.entry(icao).or_insert((fi, start, end - start));
        }
    };
    loop {
        line.clear();
        let n = r.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        let t = String::from_utf8_lossy(&line);
        let t = t.trim_start();
        if t.starts_with("1 ") || t.starts_with("16 ") || t.starts_with("17 ") {
            close(&mut cur, pos, out);
            if let Some(code) = t.split_whitespace().nth(4) {
                cur = Some((code.to_uppercase(), pos));
            }
        } else if t.starts_with("99") && t.trim().len() <= 3 {
            close(&mut cur, pos, out);
        }
        pos += n as u64;
    }
    close(&mut cur, pos, out);
    Ok(())
}

/// Process-wide cache of root indexes, so a batch build scans each install once.
static INDEXES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, std::sync::Arc<AptIndex>>>> = std::sync::OnceLock::new();

/// Extract `icao` from an X-Plane install through its offset index.
pub fn lookup(xplane_root: &Path, icao: &str) -> Result<Option<(PathBuf, String)>> {
    let map = INDEXES.get_or_init(|| std::sync::Mutex::new(Default::default()));
    let idx = {
        let mut m = map.lock().unwrap();
        match m.get(xplane_root) {
            Some(i) => i.clone(),
            None => {
                let i = std::sync::Arc::new(AptIndex::for_root(xplane_root)?);
                m.insert(xplane_root.to_path_buf(), i.clone());
                i
            }
        }
    };
    idx.extract(icao)
}

/// Cheap check whether a file looks like an apt.dat (header line "A" or "I" + version).
pub fn looks_like_aptdat(file: &Path) -> bool {
    let Ok(mut f) = fs::File::open(file) else { return false };
    let mut buf = [0u8; 64];
    let n = f.read(&mut buf).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(0));
    let head = String::from_utf8_lossy(&buf[..n]);
    let mut lines = head.lines();
    matches!(lines.next().map(str::trim), Some("I") | Some("A")) && lines.next().map_or(false, |l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extracts_block_for_requested_icao() {
        let dir = std::env::temp_dir().join(format!("amdbgen-local-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("apt.dat");
        let mut f = fs::File::create(&p).unwrap();
        writeln!(f, "I\n1200 Version\n\n1 10 0 0 KAAA First\n100 30 1 0 0.25 0 0 0 09 1 2 0 0 0 0 0 0 27 1 2.1 0 0 0 0 0 0\n\n1 20 0 0 KBBB Second\n1302 city X\n99").unwrap();
        let b = extract_airport_block(&p, "kbbb").unwrap().unwrap();
        assert!(b.contains("KBBB Second"));
        assert!(b.contains("1302 city X"));
        assert!(!b.contains("KAAA"));
        assert!(b.trim_end().ends_with("99"));
        assert!(extract_airport_block(&p, "ZZZZ").unwrap().is_none());
        assert!(looks_like_aptdat(&p));
        // Offset index gives the same block, and Custom Scenery wins over the global file.
        let root = dir.join("root");
        fs::create_dir_all(root.join("Custom Scenery/MyPack/Earth nav data")).unwrap();
        fs::create_dir_all(root.join("Global Scenery/Global Airports/Earth nav data")).unwrap();
        fs::copy(&p, root.join("Global Scenery/Global Airports/Earth nav data/apt.dat")).unwrap();
        fs::write(root.join("Custom Scenery/MyPack/Earth nav data/apt.dat"), "I\n1200 Version\n\n1 30 0 0 KBBB Custom\n99\n").unwrap();
        let idx = AptIndex::for_root(&root).unwrap();
        assert!(idx.contains("kaaa") && idx.contains("KBBB") && !idx.contains("ZZZZ"));
        let (from, text) = idx.extract("KBBB").unwrap().unwrap();
        assert!(from.to_string_lossy().contains("MyPack"));
        assert!(text.contains("KBBB Custom") && text.trim_end().ends_with("99"));
        let (_, a) = idx.extract("KAAA").unwrap().unwrap();
        assert!(a.contains("KAAA First") && a.contains("100 30") && !a.contains("KBBB"));
        let _ = fs::remove_dir_all(&dir);
    }
}
