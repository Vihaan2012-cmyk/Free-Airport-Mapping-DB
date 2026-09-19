//! The aircraft report a tester sends when an aircraft does not get its maps: which
//! third-party aircraft are installed and where, and short excerpts of their panel code
//! and gauges wherever they mention Navigraph, the AMDB API or a sign-in token.
//!
//! Only excerpts are copied, never whole files: the aircraft are paid products, and a
//! few hundred characters around each mention is all that is needed to see how an
//! aircraft asks for its maps. The user's own folder names are replaced by
//! `%USERPROFILE%` so the report can be shared as it is.

use super::desktop;
use super::settings::Settings;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// What to look for in panel scripts, case-insensitively. The most specific come first,
/// so a mention is excerpted under its most telling name.
const TERMS: [&str; 7] = ["requestnavigraphaccesstoken", "navigraph", "amdb", "access_token", "oans", "simbrief", "127.0.0.1"];
/// Characters kept either side of a mention.
const CONTEXT: usize = 180;
/// Mentions kept per term in one file, and per package in all.
const PER_TERM_PER_FILE: usize = 4;
const PER_PACKAGE: usize = 60;
/// Strings kept from the gauges of one package.
const WASM_STRINGS_PER_PACKAGE: usize = 80;
/// Panel scripts larger than this are skipped rather than read whole. Gauges are read
/// in pieces and have no limit.
const MAX_SCRIPT: u64 = 128 * 1024 * 1024;

/// Where a package came from, as the simulator's folders tell it.
fn origin(folder: &str) -> &'static str {
    match folder {
        "Community" | "Community2024" => "Community folder",
        "StreamedPackages" => "streamed by MSFS 2024 (not stored as readable files)",
        _ => "Official / Marketplace",
    }
}

/// Every package folder under a simulator's package library, with where it came from.
fn packages(sim: &desktop::Sim) -> Vec<(PathBuf, &'static str)> {
    let mut out = Vec::new();
    let Some(library) = sim.community.parent() else { return out };
    let mut roots: Vec<(PathBuf, &str)> = vec![(sim.community.clone(), "Community")];
    for name in ["Community2024", "StreamedPackages"] {
        roots.push((library.join(name), name));
    }
    for official in ["Official", "Official2020", "Official2024"] {
        for store in ["OneStore", "Steam"] {
            roots.push((library.join(official).join(store), official));
        }
    }
    for (root, kind) in roots {
        let Ok(rd) = fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            if e.path().is_dir() && !out.iter().any(|(p, _): &(PathBuf, _)| p == &e.path()) {
                out.push((e.path(), origin(kind)));
            }
        }
    }
    out
}

#[derive(Default)]
struct Manifest {
    title: String,
    creator: String,
    version: String,
    content_type: String,
}

fn manifest(pkg: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(pkg.join("manifest.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(Manifest { title: s("title"), creator: s("creator"), version: s("package_version"), content_type: s("content_type") })
}

/// A package worth reporting: an aircraft from a third party. The simulator's own
/// aircraft and liveries are left out, which keeps the report to what matters.
fn third_party_aircraft(name: &str, m: &Manifest) -> bool {
    let n = name.to_ascii_lowercase();
    let own = ["asobo-", "microsoft-", "fs20-asobo", "fs24-asobo", "fs24-microsoft", "fs-base", "workingtitle-"].iter().any(|p| n.starts_with(p)) || m.creator.to_ascii_lowercase().contains("asobo");
    m.content_type.eq_ignore_ascii_case("AIRCRAFT") && !own
}

/// A streamed package: no manifest to read, so go by its name.
fn looks_like_aircraft(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("aircraft") && !n.contains("livery") && !n.starts_with("fs24-asobo") && !n.starts_with("fs20-asobo") && !n.starts_with("fs24-microsoft")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Byte index at or before `i` that starts a character.
fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Excerpts around each mention of the search terms, on one line each.
fn excerpts(text: &str) -> Vec<(&'static str, String)> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    // Stretches of text already excerpted, under any term: a mention inside one adds nothing.
    let mut covered: Vec<(usize, usize)> = Vec::new();
    for term in TERMS {
        let mut from = 0;
        let mut kept = 0;
        while let Some(off) = lower[from..].find(term) {
            let at = from + off;
            from = at + term.len();
            if covered.iter().any(|&(s, e)| at >= s && at < e) {
                continue;
            }
            let start = floor_char(text, at.saturating_sub(CONTEXT));
            let end = floor_char(text, (at + term.len() + CONTEXT).min(text.len()));
            covered.push((start, end));
            let snippet: String = text[start..end].chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
            out.push((term, snippet));
            kept += 1;
            if kept >= PER_TERM_PER_FILE {
                break;
            }
        }
    }
    out
}

/// Collects the printable strings in a compiled gauge that name a server or the map API,
/// fed in pieces so a large gauge is never held whole.
#[derive(Default)]
struct GaugeStrings {
    run: Vec<u8>,
    found: Vec<String>,
}

impl GaugeStrings {
    /// Longest printable run kept; anything longer is data, not a string.
    const MAX_RUN: usize = 4096;

    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if (0x20..0x7f).contains(&b) {
                if self.run.len() < Self::MAX_RUN {
                    self.run.push(b);
                }
            } else {
                self.flush();
            }
        }
    }

    fn flush(&mut self) {
        if self.run.len() >= 6 {
            let s = String::from_utf8_lossy(&self.run);
            let l = s.to_ascii_lowercase();
            if l.contains("navigraph") || l.contains("amdb") || l.contains("http") || l.contains("oans") || l.contains("token") {
                let s: String = s.chars().take(300).collect();
                if !self.found.contains(&s) {
                    self.found.push(s);
                }
            }
        }
        self.run.clear();
    }

    fn finish(mut self) -> Vec<String> {
        self.flush();
        self.found
    }
}

#[cfg(test)]
fn gauge_strings(bytes: &[u8]) -> Vec<String> {
    let mut g = GaugeStrings::default();
    g.feed(bytes);
    g.finish()
}

fn gauge_strings_in(path: &Path) -> std::io::Result<Vec<String>> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut g = GaugeStrings::default();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        g.feed(&buf[..n]);
    }
    Ok(g.finish())
}

fn human(b: u64) -> String {
    crate::term::human_bytes(b)
}

/// Replace the user's own folder with a placeholder.
fn scrub(text: &str) -> String {
    let mut t = text.to_string();
    for var in ["USERPROFILE", "LOCALAPPDATA", "APPDATA"] {
        if let Ok(v) = std::env::var(var) {
            if v.len() > 3 {
                t = t.replace(&v, &format!("%{var}%"));
                t = t.replace(&v.replace('\\', "/"), &format!("%{var}%"));
            }
        }
    }
    if let Ok(user) = std::env::var("USERNAME") {
        if user.len() > 2 {
            t = t.replace(&format!("\\Users\\{user}"), "\\Users\\%USERNAME%").replace(&format!("/Users/{user}"), "/Users/%USERNAME%");
        }
    }
    t
}

/// Append a package's section to `r`. Returns how many relevant findings it had.
fn report_package(r: &mut String, pkg: &Path, origin: &str, m: Option<&Manifest>) -> usize {
    let name = pkg.file_name().unwrap_or_default().to_string_lossy();
    let _ = writeln!(r, "\n----------------------------------------------------------------------");
    let _ = writeln!(r, "{name}");
    if let Some(m) = m {
        let _ = writeln!(r, "  title:    {}", m.title);
        let _ = writeln!(r, "  creator:  {}", m.creator);
        let _ = writeln!(r, "  version:  {}", m.version);
    }
    let _ = writeln!(r, "  where:    {origin}");
    let _ = writeln!(r, "  path:     {}", pkg.display());

    let mut files = Vec::new();
    walk(pkg, &mut files);
    let mut kinds: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for f in &files {
        let ext = f.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_else(|| "(none)".into());
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        let k = kinds.entry(ext).or_default();
        k.0 += 1;
        k.1 += size;
    }
    let mut kinds: Vec<_> = kinds.into_iter().collect();
    kinds.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    let summary: Vec<String> = kinds.iter().take(10).map(|(e, (n, s))| format!("{e} {n} ({})", human(*s))).collect();
    let _ = writeln!(r, "  files:    {}  [{}]", files.len(), summary.join(", "));
    let protected = kinds.iter().any(|(e, _)| e == "fsarchive" || e == "enc");
    if protected {
        let _ = writeln!(r, "  note:     packed/protected files present; panel code in them cannot be read");
    }

    // Panel scripts.
    let mut kept = 0;
    let mut scripts = 0;
    for f in files.iter().filter(|f| f.extension().map_or(false, |e| e.eq_ignore_ascii_case("js") || e.eq_ignore_ascii_case("mjs") || e.eq_ignore_ascii_case("html"))) {
        if f.metadata().map_or(true, |m| m.len() > MAX_SCRIPT) {
            continue;
        }
        scripts += 1;
        let Ok(bytes) = fs::read(f) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let found = excerpts(&text);
        if found.is_empty() {
            continue;
        }
        let rel = f.strip_prefix(pkg).unwrap_or(f).display().to_string();
        let _ = writeln!(r, "\n  [script] {rel}  ({})", human(bytes.len() as u64));
        for (term, snippet) in found {
            let _ = writeln!(r, "    <{term}> {snippet}");
            kept += 1;
        }
        if kept >= PER_PACKAGE {
            let _ = writeln!(r, "\n  (more mentions left out)");
            break;
        }
    }
    let _ = writeln!(r, "\n  scripts read: {scripts}, mentions kept: {kept}");

    // Compiled gauges, read in pieces: the A350's main gauge alone is 69 MB. Aircraft
    // variants often carry byte-identical copies of the same gauge; each is read once.
    let mut strings_kept = 0;
    let mut seen: Vec<(String, u64)> = Vec::new();
    for f in files.iter().filter(|f| f.extension().map_or(false, |e| e.eq_ignore_ascii_case("wasm"))) {
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        let key = (f.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase(), size);
        let rel = f.strip_prefix(pkg).unwrap_or(f).display().to_string();
        if seen.contains(&key) {
            let _ = writeln!(r, "\n  [gauge] {rel}  (same as above)");
            continue;
        }
        seen.push(key);
        let Ok(mut found) = gauge_strings_in(f) else { continue };
        // A big gauge has far more matches than are kept, most of them C++ symbol names.
        // Addresses tell the most, then anything naming Navigraph or the map API.
        found.sort_by_key(|s| {
            let l = s.to_ascii_lowercase();
            if l.contains("://") {
                0
            } else if l.contains("amdb") || l.contains("navigraph") {
                1
            } else {
                2
            }
        });
        let _ = writeln!(r, "\n  [gauge] {rel}  ({}, {} relevant strings)", human(size), found.len());
        for s in found.into_iter().take(WASM_STRINGS_PER_PACKAGE.saturating_sub(strings_kept)) {
            let _ = writeln!(r, "    {s}");
            strings_kept += 1;
        }
    }
    kept + strings_kept
}

/// Write the report into `dir` and return its path. `filter`, when given, keeps only
/// packages whose folder name or title contains it (case-insensitive), and then
/// includes them even if they are not recognised as third-party aircraft.
pub fn collect(dir: &Path, filter: Option<&str>) -> Result<PathBuf> {
    let now = chrono::Local::now();
    let mut r = String::new();
    let _ = writeln!(r, "AMDB Bridge aircraft report");
    let _ = writeln!(r, "version {}  ·  {}", env!("CARGO_PKG_VERSION"), now.format("%Y-%m-%d %H:%M"));
    if let Some(f) = filter {
        let _ = writeln!(r, "packages matching: {f}");
    }

    let domain = super::NAVIGRAPH_AMDB_DOMAIN;
    let _ = writeln!(r, "\nThis computer");
    let _ = writeln!(r, "  Navigraph address points here:   {}", super::hosts::is_installed(domain));
    let _ = writeln!(r, "  local certificate trusted:       {}", super::tls::is_trusted());
    let settings = Settings::load();
    let _ = writeln!(r, "  A350/A380X option on:            {}", settings.as_ref().map_or(false, |s| s.navigraph_redirect));
    let _ = writeln!(r, "  X-Plane 12:                      {}", desktop::xplane_state().map_or("not found".to_string(), |(p, s)| format!("{} ({s:?})", p.display())));

    let sims = desktop::detect_sims();
    if sims.is_empty() {
        let _ = writeln!(r, "\nNo Microsoft Flight Simulator found.");
    }
    let wanted = filter.map(|f| f.to_ascii_lowercase());
    for sim in &sims {
        let _ = writeln!(r, "\n======================================================================");
        let _ = writeln!(r, "{}  ({})", sim.name, sim.community.display());
        let mut streamed = Vec::new();
        let mut quiet = Vec::new();
        let mut reported = 0;
        for (pkg, origin) in packages(sim) {
            let name = pkg.file_name().unwrap_or_default().to_string_lossy().to_string();
            let m = manifest(&pkg);
            let matches_filter = |title: &str| wanted.as_ref().map_or(true, |w| name.to_ascii_lowercase().contains(w) || title.to_ascii_lowercase().contains(w));
            match &m {
                Some(m) if matches_filter(&m.title) && (wanted.is_some() || third_party_aircraft(&name, m)) => {
                    // A package with nothing relevant gets one line, not a section.
                    let mut section = String::new();
                    if report_package(&mut section, &pkg, origin, Some(m)) > 0 || wanted.is_some() {
                        r.push_str(&section);
                        reported += 1;
                    } else {
                        quiet.push(format!("{name}  ({}, {} {}, {origin})", m.title, m.creator, m.version));
                    }
                }
                None if origin.starts_with("streamed") && matches_filter("") && (wanted.is_some() || looks_like_aircraft(&name)) => streamed.push(name),
                _ => {}
            }
        }
        if !quiet.is_empty() {
            let _ = writeln!(r, "\nOther aircraft packages (nothing in them mentions Navigraph or the map API):");
            for q in quiet {
                let _ = writeln!(r, "  {q}");
            }
        }
        if !streamed.is_empty() {
            let _ = writeln!(r, "\nStreamed aircraft (MSFS 2024 keeps these packed; nothing inside can be read):");
            for s in streamed {
                let _ = writeln!(r, "  {s}");
            }
        }
        if reported == 0 {
            let _ = writeln!(r, "\n(no matching aircraft with readable files)");
        }
    }

    let _ = writeln!(r, "\n======================================================================");
    let _ = writeln!(r, "Send this file together with the AMDB-Bridge-log file saved next to it.");

    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("AMDB-Bridge-aircraft-report-{}.txt", now.format("%Y%m%d-%H%M%S")));
    fs::write(&path, scrub(&r)).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Copy the app's log into `dir` under a dated name, with the user's folder names
/// replaced as in the report. None when there is no log yet.
pub fn save_log_copy(dir: &Path) -> Result<Option<PathBuf>> {
    let log = super::settings::app_dir().join("bridge.log");
    let Ok(bytes) = fs::read(&log) else { return Ok(None) };
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("AMDB-Bridge-log-{}.txt", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    fs::write(&path, scrub(&String::from_utf8_lossy(&bytes))).with_context(|| format!("write {}", path.display()))?;
    Ok(Some(path))
}

/// The report and a copy of the log, both saved to the Downloads folder where a tester
/// will find them. Returns the report's path, then the log copy's.
pub fn collect_to_downloads(filter: Option<&str>) -> Result<(PathBuf, Option<PathBuf>)> {
    let dir = desktop::downloads_dir();
    let report = collect(&dir, filter)?;
    let log = save_log_copy(&dir)?;
    Ok((report, log))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpts_are_short_and_on_one_line() {
        let body = format!("{}x.on('RequestNavigraphAccessToken',()=>{{\nconst t=getToken();}}){}", "a".repeat(500), "b".repeat(500));
        let found = excerpts(&body);
        assert!(found.iter().any(|(t, _)| *t == "requestnavigraphaccesstoken"));
        // One excerpt per mention, not one for the containing word as well.
        assert_eq!(found.iter().filter(|(t, _)| *t == "navigraph").count(), 0);
        for (_, s) in &found {
            assert!(s.len() <= 2 * CONTEXT + 40);
            assert!(!s.contains('\n'));
        }
    }

    #[test]
    fn gauge_strings_keep_only_relevant_text() {
        let mut bytes = b"\x00\x01https://amdb.api.navigraph.com/v1/\x00junk text here\x00\x02Bearer token\x00".to_vec();
        bytes.extend_from_slice(&[0xff; 10]);
        let s = gauge_strings(&bytes);
        assert_eq!(s, vec!["https://amdb.api.navigraph.com/v1/".to_string(), "Bearer token".to_string()]);
    }

    #[test]
    fn multibyte_text_near_a_mention_is_cut_safely() {
        let body = format!("{}navigraph{}", "é".repeat(200), "ü".repeat(200));
        assert_eq!(excerpts(&body).len(), 1);
    }
}
