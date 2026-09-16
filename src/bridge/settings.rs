//! Persistent bridge settings and the first-run setup.
//!
//! On the first `serve` the bridge asks whether to keep generated airports and
//! downloaded source data on disk, where, and up to how much space. The answers are
//! saved to `%LOCALAPPDATA%\amdb-bridge\config.json`; `amdb-bridge setup` asks again.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub version: u32,
    /// Keep generated airports and downloads on disk between runs.
    pub cache: bool,
    /// Folder holding `airports/` and `downloads/`.
    pub cache_dir: PathBuf,
    /// Disk budget for the cache folder in MB; 0 = unlimited.
    pub limit_mb: u64,
    /// Desktop app: start serving as soon as the window opens.
    #[serde(default = "yes")]
    pub start_on_open: bool,
    /// Redirect the Navigraph AMDB host here, for aircraft that call it directly
    /// (iniBuilds A350, FlyByWire A380X). Needs administrator rights.
    #[serde(default)]
    pub navigraph_redirect: bool,
    /// Install the X-Plane 12 moving map and serve its route when X-Plane is found.
    #[serde(default = "yes")]
    pub xplane: bool,
}

fn yes() -> bool {
    true
}

pub fn app_dir() -> PathBuf {
    std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(".")).join("amdb-bridge")
}

impl Default for Settings {
    fn default() -> Self {
        Settings { version: 1, cache: true, cache_dir: app_dir().join("cache"), limit_mb: 2048, start_on_open: true, navigraph_redirect: false, xplane: true }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        app_dir().join("config.json")
    }

    pub fn load() -> Option<Settings> {
        let text = std::fs::read_to_string(Self::path()).ok()?;
        // Tolerate a UTF-8 BOM (PowerShell's Set-Content writes one).
        serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
    }

    pub fn save(&self) -> Result<()> {
        let p = Self::path();
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(&p, serde_json::to_string_pretty(self)?).with_context(|| format!("write {}", p.display()))?;
        Ok(())
    }

    pub fn airports_dir(&self) -> PathBuf {
        if self.cache {
            self.cache_dir.join("airports")
        } else {
            std::env::temp_dir().join("amdb-bridge").join("airports")
        }
    }

    pub fn downloads_dir(&self) -> Option<PathBuf> {
        if self.cache {
            Some(self.cache_dir.join("downloads"))
        } else {
            None
        }
    }

    pub fn limit_bytes(&self) -> Option<u64> {
        if self.cache && self.limit_mb > 0 {
            Some(self.limit_mb * 1024 * 1024)
        } else {
            None
        }
    }

    /// Saved settings, or the first-run setup (interactive when possible, defaults otherwise).
    pub fn load_or_setup() -> Result<Settings> {
        if let Some(s) = Self::load() {
            return Ok(s);
        }
        let s = if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() { Self::wizard(&Settings::default())? } else { Settings::default() };
        s.save()?;
        crate::term::success(&format!("Settings saved to {}", Self::path().display()));
        Ok(s)
    }

    /// Ask the three questions, starting from `current`.
    pub fn wizard(current: &Settings) -> Result<Settings> {
        crate::term::start("First-run setup (press Enter to keep the value in brackets)");
        println!("  Generated airports take 2-6 MB each and 15-40 s to build; source downloads 5-20 MB each.");
        println!("  Keeping them on disk makes the next load of the same airport instant and works offline.");
        println!();
        let cache = ask_bool("Keep generated airports and downloads on disk?", current.cache)?;
        let (cache_dir, limit_mb) = if cache {
            let dir = ask_path("Cache folder", &current.cache_dir)?;
            let limit = ask_u64("Storage limit in MB (0 = unlimited; oldest airports are removed first)", current.limit_mb)?;
            (dir, limit)
        } else {
            println!("  Airports will be rebuilt on every sim session and kept only in memory.");
            (current.cache_dir.clone(), current.limit_mb)
        };
        println!();
        Ok(Settings { cache, cache_dir, limit_mb, ..current.clone() })
    }

    pub fn describe(&self) -> String {
        if !self.cache {
            return "caching off (airports rebuilt every session, nothing kept on disk)".to_string();
        }
        let used = dir_size(&self.cache_dir);
        format!(
            "cache on at {} using {}{}",
            self.cache_dir.display(),
            crate::term::human_bytes(used),
            if self.limit_mb > 0 { format!(" of {} MB", self.limit_mb) } else { ", no limit".to_string() }
        )
    }
}

fn prompt(q: &str, default: &str) -> Result<String> {
    print!("  {q} [{default}]: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let t = line.trim().to_string();
    Ok(if t.is_empty() { default.to_string() } else { t })
}

fn ask_bool(q: &str, default: bool) -> Result<bool> {
    loop {
        let a = prompt(q, if default { "Y/n" } else { "y/N" })?.to_ascii_lowercase();
        match a.as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            "y/n" => return Ok(true),
            "y/n " | "" => return Ok(default),
            _ if a == "y/n".to_string() => return Ok(default),
            _ => {
                if a.starts_with('y') {
                    return Ok(true);
                }
                if a.starts_with('n') {
                    return Ok(false);
                }
                println!("  please answer y or n");
            }
        }
    }
}

fn ask_path(q: &str, default: &Path) -> Result<PathBuf> {
    loop {
        let a = prompt(q, &default.display().to_string())?;
        let a = a.trim().trim_matches('"').to_string();
        // A stray yes/no here means "keep the default", not a folder called "y".
        if matches!(a.to_ascii_lowercase().as_str(), "y" | "yes" | "n" | "no" | "ok") {
            return Ok(default.to_path_buf());
        }
        if !Path::new(&a).is_absolute() {
            println!("  please give a full path (e.g. D:/amdb-cache)");
            continue;
        }
        let p = PathBuf::from(a);
        match std::fs::create_dir_all(&p) {
            Ok(()) => return Ok(p),
            Err(e) => println!("  cannot create {}: {e}", p.display()),
        }
    }
}

fn ask_u64(q: &str, default: u64) -> Result<u64> {
    loop {
        let a = prompt(q, &default.to_string())?;
        let digits: String = a.chars().filter(|c| c.is_ascii_digit()).collect();
        match digits.parse::<u64>() {
            Ok(v) => return Ok(v),
            Err(_) => println!("  please enter a number of megabytes"),
        }
    }
}

/// Total size of every file under `dir`.
pub fn dir_size(dir: &Path) -> u64 {
    fn walk(p: &Path, acc: &mut u64) {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, acc);
                } else if let Ok(m) = e.metadata() {
                    *acc += m.len();
                }
            }
        }
    }
    let mut n = 0;
    walk(dir, &mut n);
    n
}

fn modified(p: &Path) -> std::time::SystemTime {
    std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH)
}

/// Bring the cache folder under `limit` bytes by deleting the least recently built
/// airports (never `keep`) and then the oldest downloads. Returns what was removed.
pub fn prune(s: &Settings, keep: &str) -> Vec<String> {
    let Some(limit) = s.limit_bytes() else { return vec![] };
    let mut removed = Vec::new();
    let mut used = dir_size(&s.cache_dir);
    if used <= limit {
        return removed;
    }
    let airports = s.airports_dir();
    let mut folders: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&airports)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir() && p.file_name().map_or(false, |n| n != keep)).map(|p| (modified(&p.join("manifest.json")), p)).collect())
        .unwrap_or_default();
    folders.sort();
    for (_, p) in folders {
        if used <= limit {
            break;
        }
        let size = dir_size(&p);
        if std::fs::remove_dir_all(&p).is_ok() {
            used = used.saturating_sub(size);
            removed.push(p.file_name().unwrap().to_string_lossy().to_string());
        }
    }
    if used > limit {
        if let Some(dl) = s.downloads_dir() {
            let mut files: Vec<(std::time::SystemTime, PathBuf, u64)> = Vec::new();
            fn walk(p: &Path, out: &mut Vec<(std::time::SystemTime, PathBuf, u64)>) {
                if let Ok(rd) = std::fs::read_dir(p) {
                    for e in rd.flatten() {
                        let path = e.path();
                        if path.is_dir() {
                            walk(&path, out);
                        } else if let Ok(m) = e.metadata() {
                            out.push((m.modified().unwrap_or(std::time::UNIX_EPOCH), path, m.len()));
                        }
                    }
                }
            }
            walk(&dl, &mut files);
            files.sort();
            for (_, p, size) in files {
                if used <= limit {
                    break;
                }
                if std::fs::remove_file(&p).is_ok() {
                    used = used.saturating_sub(size);
                    removed.push(p.file_name().unwrap().to_string_lossy().to_string());
                }
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_removes_oldest_airports_first() {
        let root = std::env::temp_dir().join(format!("amdb-bridge-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Settings { cache: true, cache_dir: root.clone(), limit_mb: 0, ..Settings::default() };
        for (name, age) in [("AAAA", 300u64), ("BBBB", 200), ("CCCC", 100)] {
            let d = s.airports_dir().join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("manifest.json"), vec![b'x'; 1024 * 600]).unwrap();
            let t = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
            let f = std::fs::File::options().write(true).open(d.join("manifest.json")).unwrap();
            f.set_modified(t).unwrap();
        }
        let s = Settings { limit_mb: 1, ..s }; // 1 MB: room for one 600 KB airport
        let removed = prune(&s, "CCCC");
        assert_eq!(removed, vec!["AAAA".to_string(), "BBBB".to_string()]);
        assert!(s.airports_dir().join("CCCC").is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }
}
