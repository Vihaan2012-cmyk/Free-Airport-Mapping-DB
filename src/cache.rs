//! Simple on-disk cache for downloaded source data so rebuilds work offline.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Cache {
    /// `None` means no on-disk cache: every run refetches.
    root: Option<PathBuf>,
    /// When true, never hit the network; missing entries are errors.
    pub offline: bool,
    /// When true, ignore existing entries and refetch.
    pub refresh: bool,
    /// Entries older than this are refetched (stale copy kept if the fetch fails).
    pub max_age: Option<std::time::Duration>,
}

impl Cache {
    pub fn new(root: Option<PathBuf>, offline: bool, refresh: bool) -> Self {
        Self { root, offline, refresh, max_age: None }
    }

    pub fn memory_only() -> Self {
        Self { root: None, offline: false, refresh: false, max_age: None }
    }

    /// Always-on cache for the small worldwide index files (airport lists), refreshed daily.
    pub fn for_index(offline: bool) -> Self {
        let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
        Self { offline, ..Self::for_index_at(base.join("amdbgen").join("index")) }
    }

    /// The same index cache in a folder of the caller's choosing (a portable program keeps
    /// it beside itself).
    pub fn for_index_at(root: PathBuf) -> Self {
        Self { root: Some(root), offline: false, refresh: false, max_age: Some(std::time::Duration::from_secs(24 * 3600)) }
    }

    fn fresh(&self, p: &Path) -> bool {
        match self.max_age {
            None => true,
            Some(max) => fs::metadata(p).and_then(|m| m.modified()).map(|t| t.elapsed().map(|e| e < max).unwrap_or(true)).unwrap_or(false),
        }
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn path(&self, key: &str) -> Option<PathBuf> {
        self.root.as_ref().map(|r| r.join(key))
    }

    pub fn get_or_fetch_text(&self, key: &str, fetch: impl FnOnce() -> Result<String>) -> Result<String> {
        let Some(p) = self.path(key) else { return fetch() };
        let have = p.is_file();
        if !self.refresh && have && (self.fresh(&p) || self.offline) {
            return fs::read_to_string(&p).with_context(|| format!("read cache {}", p.display()));
        }
        if self.offline {
            anyhow::bail!("offline and not cached: {key}");
        }
        let text = match fetch() {
            Ok(t) => t,
            Err(e) if have => {
                log::warn!("{key}: refresh failed ({e:#}); using the cached copy");
                return fs::read_to_string(&p).with_context(|| format!("read cache {}", p.display()));
            }
            Err(e) => return Err(e),
        };
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = p.with_extension("tmp");
        fs::write(&tmp, &text)?;
        fs::rename(&tmp, &p)?;
        Ok(text)
    }

    pub fn get_or_fetch_bytes(&self, key: &str, fetch: impl FnOnce() -> Result<Vec<u8>>) -> Result<Vec<u8>> {
        let Some(p) = self.path(key) else { return fetch() };
        if !self.refresh && p.is_file() && (self.fresh(&p) || self.offline) {
            return fs::read(&p).with_context(|| format!("read cache {}", p.display()));
        }
        if self.offline {
            anyhow::bail!("offline and not cached: {key}");
        }
        let bytes = fetch()?;
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = p.with_extension("tmp");
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, &p)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_and_respects_offline() {
        let dir = std::env::temp_dir().join(format!("amdbgen-cache-test-{}", std::process::id()));
        let c = Cache::new(Some(dir.clone()), false, false);
        let mut calls = 0;
        let v = c.get_or_fetch_text("a/b.txt", || { calls += 1; Ok("hello".into()) }).unwrap();
        assert_eq!(v, "hello");
        let v2 = c.get_or_fetch_text("a/b.txt", || { calls += 1; Ok("other".into()) }).unwrap();
        assert_eq!(v2, "hello");
        assert_eq!(calls, 1);
        let off = Cache::new(Some(dir.clone()), true, false);
        let mem = Cache::memory_only();
        let mut n = 0;
        mem.get_or_fetch_text("a/b.txt", || { n += 1; Ok("fresh".into()) }).unwrap();
        mem.get_or_fetch_text("a/b.txt", || { n += 1; Ok("fresh".into()) }).unwrap();
        assert_eq!(n, 2);
        assert!(off.get_or_fetch_text("missing.txt", || Ok("x".into())).is_err());
        assert_eq!(off.get_or_fetch_text("a/b.txt", || Ok("x".into())).unwrap(), "hello");
        let _ = fs::remove_dir_all(&dir);
    }
}
