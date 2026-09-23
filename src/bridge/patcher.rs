//! Points installed aircraft at the local bridge by rewriting the Navigraph AMDB
//! host inside their JavaScript bundles. Backups are kept next to each file and a
//! record is written so `unpatch` restores them exactly.
//!
//! Handled forms:
//! * literal `https://amdb.api.navigraph.com` (FlyByWire builds and anything on the fbw-sdk);
//! * Navigraph SDK templates `https://amdb.api.${fn()}` (the host is computed), which
//!   become `http://127.0.0.1:PORT/${fn()}` — the server ignores the extra prefix;
//! * the iniBuilds A350 EFB, whose OANS gauge (WASM) only fetches AMDB data once the
//!   EFB has handed it a Navigraph token over the comm bus. The EFB answers the
//!   gauge's `RequestNavigraphAccessToken` with an empty string unless a Navigraph
//!   account with a subscription is signed in, so that handler is rewritten to always
//!   answer with a placeholder token. The bridge ignores the bearer token, and the
//!   gauge's requests still reach it through the hosts-file redirect;
//! * the flight bags built on the Navigraph SDK (the A350 EFB, the PMDG 737 and 777
//!   tablets), whose charts are pointed at the bridge by `patch_charts`.

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PatchRecord {
    pub files: Vec<PatchedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchedFile {
    pub path: PathBuf,
    pub backup: PathBuf,
    pub replacements: usize,
}

pub const BACKUP_SUFFIX: &str = ".amdb-bridge.bak";

/// Opaque placeholder the patched A350 EFB hands to its OANS gauge; the bridge does
/// not check bearer tokens, it only needs the gauge to believe it has one.
pub const A350_TOKEN: &str = "amdb-bridge-local";
const A350_MARK: &str = "/*amdb-bridge*/";
const A350_HANDLER: &str = "'RequestNavigraphAccessToken',()=>{";

/// Index just past the `}` that closes the block opened at `open` (which must be a `{`),
/// skipping string literals. None if the text is unbalanced.
fn block_end(text: &str, open: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' | b'`' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Rewrite the A350 EFB token handler. None when the text is not an A350 EFB bundle or
/// is already patched.
pub fn patch_a350_text(text: &str) -> Option<String> {
    if text.contains(A350_MARK) {
        return None;
    }
    let start = text.find(A350_HANDLER)?;
    let open = start + A350_HANDLER.len() - 1;
    let end = block_end(text, open)?;
    let push = format!("Coherent.call('COMM_BUS_WASM_CALLBACK','SetNavigraphAccessToken','{A350_TOKEN}')");
    let mut out = String::with_capacity(text.len() + 400);
    out.push_str(&text[..open]);
    out.push_str(&format!("{{{A350_MARK}{push};}}"));
    out.push_str(&text[end..]);
    // The gauge may register its comm-bus handler after the EFB starts, so also push
    // the token periodically; a repeated set is harmless.
    out.push_str(&format!("\n{A350_MARK}setInterval(()=>{{try{{{push};}}catch(e){{}}}},30000);\n"));
    Some(out)
}

/// A350 EFB bundles in a Community folder: (package, file, already patched).
pub fn scan_a350(community: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(community) else { return out };
    for pkg in rd.flatten() {
        let pdir = pkg.path();
        if !pdir.is_dir() {
            continue;
        }
        let mut js = Vec::new();
        walk_js(&pdir.join("html_ui"), &mut js);
        for f in js {
            let name = f.file_name().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if !name.starts_with("ini-efb") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&f) else { continue };
            if text.contains(A350_HANDLER) || text.contains(A350_MARK) {
                out.push((pkg.file_name().to_string_lossy().to_string(), f, text.contains(A350_MARK)));
            }
        }
    }
    out
}

/// Apply the A350 EFB token patch in a Community folder (backups kept, recorded for
/// `unpatch`). Returns the files changed.
pub fn patch_a350(community: &Path, dry_run: bool) -> Result<Vec<PatchedFile>> {
    let mut record = load_record(community);
    let mut done = Vec::new();
    for (pkg, path, patched) in scan_a350(community) {
        if patched {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let Some(new_text) = patch_a350_text(&text) else { continue };
        log::info!("{}{}: Navigraph token handler rewritten in {}", if dry_run { "[dry-run] " } else { "" }, pkg, path.display());
        if dry_run {
            done.push(PatchedFile { path: path.clone(), backup: PathBuf::new(), replacements: 1 });
            continue;
        }
        let backup = PathBuf::from(format!("{}{}", path.display(), BACKUP_SUFFIX));
        if !backup.exists() {
            fs::copy(&path, &backup).with_context(|| format!("backup {}", path.display()))?;
        }
        fs::write(&path, new_text)?;
        let pf = PatchedFile { path: path.clone(), backup, replacements: 1 };
        record.files.retain(|f| f.path != pf.path);
        record.files.push(pf.clone());
        done.push(pf);
    }
    if !dry_run && !done.is_empty() {
        save_record(community, &record)?;
    }
    Ok(done)
}

// ---------------------------------------------------------------------------------
// GM5 A220 Airport Moving Map (Gunman5). Its script fetches the Navigraph AMDB URL
// directly, so the hosts redirect reaches it, but it refuses to fetch without the
// A220's stored Navigraph token and, under MSFS 2020, its sim-side nearest-airport
// search never returns anything. Two small rewrites fix both; the bridge's
// /v1/nearest endpoint stands in for the sim search.
// ---------------------------------------------------------------------------------

const A220_MARK: &str = "/*amdb-bridge-a220*/";
const A220_TOKEN_FN: &str = "    function currentStoredAccessToken() {\n        const token = normalizeToken(readStoredValue(DU_ACCESS_KEY));\n        if (!token) return null;\n        const info = inspectToken(token);\n        return info.expired ? null : token;\n    }";
const A220_SEARCH_FN: &str = "    async function runNearestSearchWithRetry(state, radiusMeters, maxItems, stage) {";

/// Rewrite the A220 map script. None when it is not that script or already patched.
pub fn patch_a220_text(text: &str) -> Option<String> {
    if text.contains(A220_MARK) || text.contains("bridgeNearestSearch(") || !text.contains("GM5_A220_AMM") {
        return None;
    }
    let text = text.replace("\r\n", "\n");
    let token_fn = format!(
        "    function currentStoredAccessToken() {{\n        {A220_MARK} /* no Navigraph login needed: the local bridge ignores bearer tokens */\n        const token = normalizeToken(readStoredValue(DU_ACCESS_KEY));\n        if (!token) return '{A350_TOKEN}';\n        const info = inspectToken(token);\n        return info.expired ? '{A350_TOKEN}' : token;\n    }}"
    );
    let search_fn = format!(
        "{A220_MARK}\n    function bridgeFacilityKey(ident) {{\n        return 'A      ' + String(ident).toUpperCase() + ' ';\n    }}\n\n    async function bridgeNearestSearch(state, radiusMeters, maxItems, stage) {{\n        const url = `${{AMDB_BASE}}/nearest?lat=${{encodeURIComponent(state.lat)}}&lon=${{encodeURIComponent(state.lon)}}&radius_km=${{Math.max(5, Math.round(radiusMeters / 1000))}}&limit=${{maxItems || 16}}`;\n        const response = await withTimeout(fetch(url, {{ method: 'GET', headers: {{ 'Accept': 'application/json' }} }}), SEARCH_TIMEOUT_MS, `bridge nearest ${{stage || ''}} timeout`);\n        if (!response.ok) throw new Error(`bridge nearest HTTP ${{response.status}}`);\n        const rows = await response.json();\n        const added = [];\n        if (nearest && Array.isArray(rows)) {{\n            for (let i = 0; i < rows.length; i++) {{\n                const row = rows[i] || {{}};\n                const ident = String(row.idarpt || '').trim().toUpperCase();\n                if (!/^[A-Z]{{4}}$/.test(ident)) continue;\n                const coords = row.coordinates || {{}};\n                const lat = Number(coords.lat), lon = Number(coords.lon);\n                if (!Number.isFinite(lat) || !Number.isFinite(lon)) continue;\n                const key = bridgeFacilityKey(ident);\n                nearest.facilities.set(key, {{ icao: key, ident: ident, name: row.name || ident, lat: lat, lon: lon, runways: [], source: 'amdb-bridge' }});\n                added.push(key);\n            }}\n        }}\n        log('Bridge nearest airports', added.length, stage || '');\n        return {{ sessionId: nearest ? nearest.sessionId : null, searchId: 'bridge', added: added, removed: [] }};\n    }}\n\n    async function runNearestSearchWithRetry(state, radiusMeters, maxItems, stage) {{\n        let native = null;\n        let nativeError = null;\n        try {{\n            native = await runNearestSearchWithRetryNative(state, radiusMeters, maxItems, stage);\n        }} catch (e) {{\n            nativeError = e;\n        }}\n        const nativeCount = native && Array.isArray(native.added) ? native.added.length : 0;\n        const known = nearest && nearest.currentRaw ? nearest.currentRaw.size : 0;\n        if (nativeCount > 0 || (known > 0 && !nativeError)) return native;\n        try {{\n            return await bridgeNearestSearch(state, radiusMeters, maxItems, stage);\n        }} catch (e) {{\n            if (nativeError) throw nativeError;\n            throw e;\n        }}\n    }}\n\n    async function runNearestSearchWithRetryNative(state, radiusMeters, maxItems, stage) {{"
    );
    if !text.contains(A220_TOKEN_FN) || !text.contains(A220_SEARCH_FN) {
        return None;
    }
    Some(text.replacen(A220_TOKEN_FN, &token_fn, 1).replacen(A220_SEARCH_FN, &search_fn, 1))
}

/// A220 moving-map scripts in a Community folder: (package, file, already patched).
pub fn scan_a220(community: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(community) else { return out };
    for pkg in rd.flatten() {
        let pdir = pkg.path();
        if !pdir.is_dir() {
            continue;
        }
        let mut js = Vec::new();
        walk_js(&pdir.join("html_ui"), &mut js);
        for f in js {
            let name = f.file_name().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if name != "gm5-a220-amm.js" {
                continue;
            }
            let Ok(text) = fs::read_to_string(&f) else { continue };
            if text.contains("GM5_A220_AMM") {
                // Also counts copies ported by tools/port_a220_amm.py as patched.
                let patched = text.contains(A220_MARK) || text.contains("bridgeNearestSearch(");
                out.push((pkg.file_name().to_string_lossy().to_string(), f, patched));
            }
        }
    }
    out
}

/// On MSFS 2020 the Community packages apply alphabetically, so the map package must
/// sort after the aircraft it overrides. Returns a warning when it does not.
pub fn a220_load_order_warning(community: &Path, package: &str) -> Option<String> {
    let aircraft = "synaptic-aircraft-a220";
    if !community.join(aircraft).is_dir() || package.to_ascii_lowercase() > aircraft.to_string() {
        return None;
    }
    let is_2024 = community.to_string_lossy().contains("Limitless") || community.to_string_lossy().contains("2024");
    if is_2024 {
        return None; // MSFS 2024 honours package_order_hint instead
    }
    Some(format!("{package} sorts before {aircraft}, so MSFS 2020 will ignore its instrument override: rename the folder to zzz-{package}"))
}

/// Apply the A220 map patch in a Community folder (backups kept, recorded for
/// `unpatch`). Returns the files changed.
pub fn patch_a220(community: &Path, dry_run: bool) -> Result<Vec<PatchedFile>> {
    let mut record = load_record(community);
    let mut done = Vec::new();
    for (pkg, path, patched) in scan_a220(community) {
        if patched {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let Some(new_text) = patch_a220_text(&text) else {
            log::warn!("{pkg}: {} is a version of the A220 map this bridge does not know how to patch", path.display());
            continue;
        };
        log::info!("{}{}: token fallback and bridge airport search added to {}", if dry_run { "[dry-run] " } else { "" }, pkg, path.display());
        if dry_run {
            done.push(PatchedFile { path: path.clone(), backup: PathBuf::new(), replacements: 2 });
            continue;
        }
        let backup = PathBuf::from(format!("{}{}", path.display(), BACKUP_SUFFIX));
        if !backup.exists() {
            fs::copy(&path, &backup).with_context(|| format!("backup {}", path.display()))?;
        }
        fs::write(&path, new_text)?;
        let pf = PatchedFile { path: path.clone(), backup, replacements: 2 };
        record.files.retain(|f| f.path != pf.path);
        record.files.push(pf.clone());
        done.push(pf);
    }
    if !dry_run && !done.is_empty() {
        save_record(community, &record)?;
    }
    Ok(done)
}

// ---------------------------------------------------------------------------------
// Synaptic A220 display units. Our moving map ships as its own two files and replaces
// nothing; what makes the aircraft load them is two lines added to the instrument page
// it already has. The page is the aircraft's, so it is edited where it sits, a backup is
// kept beside it, and `unpatch` puts it back.
// ---------------------------------------------------------------------------------

const A220_PAGE_MARK: &str = "<!--amdb-bridge-a220-->";
/// The aircraft's own bundle, which the two lines are added after so the map loads last.
const A220_PAGE_ANCHOR: &str = "/Pages/VCockpit/Instruments/a22x/DisplayUnits/instrument.index.js\"></script>";

/// Add the map's script and stylesheet to the A220's instrument page. None when the page
/// is not that page, or already carries them.
pub fn patch_a220_page_text(text: &str) -> Option<String> {
    if text.contains(A220_PAGE_MARK) || text.contains("amdb-a220-amm.js") {
        return None;
    }
    let at = text.find(A220_PAGE_ANCHOR)? + A220_PAGE_ANCHOR.len();
    let added = format!(
        "\n{A220_PAGE_MARK}\n<script type=\"text/html\" import-async=\"false\" import-script=\"/Pages/VCockpit/Instruments/a22x/DisplayUnits/amdb-a220-amm.js\"></script>\n<link rel=\"stylesheet\" href=\"/Pages/VCockpit/Instruments/a22x/DisplayUnits/amdb-a220-amm.css\" />"
    );
    Some(format!("{}{}{}", &text[..at], added, &text[at..]))
}

/// A220 instrument pages in a Community folder: (package, file, already patched).
pub fn scan_a220_page(community: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(community) else { return out };
    for pkg in rd.flatten() {
        let pdir = pkg.path();
        if !pdir.is_dir() {
            continue;
        }
        let name = pkg.file_name().to_string_lossy().to_string();
        // Our own package carries no instrument page, so only the aircraft's is found.
        let page = pdir.join("html_ui/Pages/VCockpit/Instruments/a22x/DisplayUnits/instrument.html");
        let Ok(text) = fs::read_to_string(&page) else { continue };
        if text.contains(A220_PAGE_ANCHOR) || text.contains(A220_PAGE_MARK) {
            let patched = text.contains(A220_PAGE_MARK) || text.contains("amdb-a220-amm.js");
            out.push((name, page, patched));
        }
    }
    out
}

/// Add the map to the A220's instrument page in a Community folder, where the map package
/// is installed alongside it (backups kept, recorded for `unpatch`).
pub fn patch_a220_page(community: &Path, dry_run: bool) -> Result<Vec<PatchedFile>> {
    // Nothing to load if the map itself is not installed, and a page pointed at a script
    // that is not there would only log an error every frame.
    if !a220_map_installed(community) {
        return Ok(Vec::new());
    }
    let mut record = load_record(community);
    let mut done = Vec::new();
    for (pkg, path, patched) in scan_a220_page(community) {
        if patched {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let Some(new_text) = patch_a220_page_text(&text) else {
            log::warn!("{pkg}: {} is a version of the A220 instrument page this bridge does not know", path.display());
            continue;
        };
        log::info!("{}{}: moving map added to {}", if dry_run { "[dry-run] " } else { "" }, pkg, path.display());
        if dry_run {
            done.push(PatchedFile { path: path.clone(), backup: PathBuf::new(), replacements: 1 });
            continue;
        }
        let backup = PathBuf::from(format!("{}{}", path.display(), BACKUP_SUFFIX));
        if !backup.exists() {
            fs::copy(&path, &backup).with_context(|| format!("backup {}", path.display()))?;
        }
        fs::write(&path, new_text)?;
        let pf = PatchedFile { path: path.clone(), backup, replacements: 1 };
        record.files.retain(|f| f.path != pf.path);
        record.files.push(pf.clone());
        done.push(pf);
    }
    if !dry_run && !done.is_empty() {
        save_record(community, &record)?;
    }
    Ok(done)
}

/// Whether our map package is present in this Community folder.
pub fn a220_map_installed(community: &Path) -> bool {
    let Ok(rd) = fs::read_dir(community) else { return false };
    rd.flatten().any(|e| {
        e.path()
            .join("html_ui/Pages/VCockpit/Instruments/a22x/DisplayUnits/amdb-a220-amm.js")
            .is_file()
    })
}

// ---------------------------------------------------------------------------------
// Charts on the flight bags. The iniBuilds A350 and the PMDG 737 and 777 tablets are
// built on the Navigraph SDK, which makes every address it calls in three small
// functions. Rewriting those to the bridge sends the tablet's sign-in and its charts
// here, and nothing else anywhere: the hosts file is not touched and Navigraph Hub and
// Simlink go on reaching Navigraph. Each rewrite keeps the text it replaced, so `off`
// puts the file back to the byte.
// ---------------------------------------------------------------------------------

const CHARTS_MARK: &str = "/*amdb-charts:";

/// The functions the SDK builds its addresses in, and the path on the bridge each is
/// sent to.
const CHARTS_ROOTS: [(&str, &str); 3] = [("getIdentityApiRoot", "/identity"), ("getChartsApiRoot", "/v2/charts"), ("getAirportApiRoot", "/v2/airport")];

/// Flight bags with their own Navigraph client rather than the SDK, which name the two
/// hosts outright: the Synaptic A220's. Each quoted address is swapped for the bridge's.
const CHARTS_LITERALS: [(&str, &str); 2] = [("'https://identity.api.navigraph.com'", "/identity"), ("'https://api.navigraph.com'", "")];

/// The flight bag scripts, by file name.
fn is_efb_script(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("ini-efb") || name == "pmdgtablet.js" || name.starts_with("efb-a220")
}

/// Whether a script is one the charts patch knows: built on the SDK, or naming the hosts.
fn knows_charts(text: &str) -> bool {
    CHARTS_ROOTS.iter().all(|(n, _)| text.contains(n)) || CHARTS_LITERALS.iter().all(|(l, _)| text.contains(l))
}

/// Where the body of `name = () => BODY` is in the text, the body ending at the first
/// comma, semicolon or line end that is not inside brackets or a string. Found exactly
/// once, or not at all.
fn arrow_body(text: &str, name: &str) -> Option<(usize, usize)> {
    let mut found = None;
    let mut from = 0;
    while let Some(at) = text[from..].find(name) {
        let at = from + at;
        from = at + name.len();
        // A whole name, not the end of a longer one.
        if text[..at].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
            continue;
        }
        let after = text[from..].trim_start();
        let Some(after) = after.strip_prefix('=') else { continue };
        let after = after.trim_start();
        let Some(after) = after.strip_prefix("()") else { continue };
        let after = after.trim_start();
        let Some(after) = after.strip_prefix("=>") else { continue };
        let body_start = text.len() - after.trim_start().len();
        let b = text.as_bytes();
        let (mut i, mut depth) = (body_start, 0i32);
        while i < b.len() {
            match b[i] {
                b'\'' | b'"' | b'`' => {
                    let q = b[i];
                    i += 1;
                    while i < b.len() && b[i] != q {
                        if b[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' if depth > 0 => depth -= 1,
                b')' | b']' | b'}' | b',' | b';' | b'\n' | b'\r' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        if found.is_some() {
            return None;
        }
        found = Some((body_start, i));
    }
    found
}

/// Point a flight bag script's charts at the bridge. None when it is not one the SDK
/// built, or is already pointed there.
pub fn patch_charts_text(text: &str, port: u16) -> Option<String> {
    if text.contains(CHARTS_MARK) {
        return None;
    }
    let mut spans = Vec::new();
    if CHARTS_ROOTS.iter().all(|(n, _)| text.contains(n)) {
        for (name, path) in CHARTS_ROOTS {
            let (s, e) = arrow_body(text, name)?;
            spans.push((s, e, path));
        }
    } else {
        // Each quoted host, wherever it stands; the identity host first, since it is the
        // longer and the plain API host must not be found inside it.
        for (literal, path) in CHARTS_LITERALS {
            let mut from = 0;
            let mut found = false;
            while let Some(at) = text[from..].find(literal) {
                let s = from + at;
                let e = s + literal.len();
                if !spans.iter().any(|(a, b, _)| s < *b && *a < e) {
                    spans.push((s, e, path));
                    found = true;
                }
                from = e;
            }
            if !found {
                return None;
            }
        }
    }
    spans.sort_by_key(|s| s.0);
    let mut out = String::with_capacity(text.len() + 600);
    let mut at = 0;
    for (s, e, path) in spans {
        let was = base64::engine::general_purpose::STANDARD.encode(&text[s..e]);
        out.push_str(&text[at..s]);
        out.push_str(&format!("{CHARTS_MARK}{was}*/'http://127.0.0.1:{port}{path}'"));
        at = e;
    }
    out.push_str(&text[at..]);
    Some(out)
}

/// Put a flight bag script's own addresses back. None when it was not pointed here.
pub fn unpatch_charts_text(text: &str) -> Option<String> {
    if !text.contains(CHARTS_MARK) {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(CHARTS_MARK) {
        out.push_str(&rest[..at]);
        let after = &rest[at + CHARTS_MARK.len()..];
        let end = after.find("*/")?;
        let was = base64::engine::general_purpose::STANDARD.decode(&after[..end]).ok()?;
        out.push_str(std::str::from_utf8(&was).ok()?);
        // Past the address that stood in for it: one quoted string.
        let tail = &after[end + 2..];
        let quoted = tail.strip_prefix('\'')?;
        let close = quoted.find('\'')?;
        rest = &quoted[close + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Flight bag scripts in a Community folder: (package, file, pointed at the bridge).
pub fn scan_charts(community: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(community) else { return out };
    for pkg in rd.flatten() {
        let pdir = pkg.path();
        if !pdir.is_dir() {
            continue;
        }
        let mut js = Vec::new();
        walk_js(&pdir.join("html_ui"), &mut js);
        for f in js {
            let name = f.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            if !is_efb_script(&name) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&f) else { continue };
            if text.contains(CHARTS_MARK) || knows_charts(&text) {
                out.push((pkg.file_name().to_string_lossy().to_string(), f, text.contains(CHARTS_MARK)));
            }
        }
    }
    out
}

/// Point every flight bag in a Community folder at the bridge's charts. Returns the files
/// changed.
pub fn patch_charts(community: &Path, port: u16) -> Result<Vec<PathBuf>> {
    let mut done = Vec::new();
    for (pkg, path, patched) in scan_charts(community) {
        if patched {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let Some(new_text) = patch_charts_text(&text, port) else {
            log::warn!("{pkg}: {} is a version of the flight bag this bridge does not know", path.display());
            continue;
        };
        fs::write(&path, new_text).with_context(|| format!("write {}", path.display()))?;
        log::info!("{pkg}: charts now come from the bridge ({})", path.display());
        done.push(path);
    }
    Ok(done)
}

/// Give every flight bag in a Community folder its own addresses back.
pub fn unpatch_charts(community: &Path) -> Result<Vec<PathBuf>> {
    let mut done = Vec::new();
    for (pkg, path, patched) in scan_charts(community) {
        if !patched {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let Some(old) = unpatch_charts_text(&text) else {
            log::warn!("{pkg}: could not read back what {} was", path.display());
            continue;
        };
        fs::write(&path, old).with_context(|| format!("write {}", path.display()))?;
        done.push(path);
    }
    Ok(done)
}

// ---------------------------------------------------------------------------------
// SimBrief, on the flight bags that read it directly (`bridge::simbrief`'s own doc
// comment has what confirmed the address and its field names). Unlike the charts patch,
// what is patched here is one constant every one of them names outright — SimBrief's own
// public endpoint — so it needs none of the charts patch's per-build marker and base64:
// what stood at that address before is always this one address, so restoring it is the
// same plain substring swap the other way round.
// ---------------------------------------------------------------------------------

pub const SIMBRIEF_HOST: &str = "https://www.simbrief.com/api/xml.fetcher.php";

fn simbrief_target(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/xml.fetcher.php")
}

/// `Some(true)` pointed at the bridge, `Some(false)` still SimBrief's own, `None` neither.
fn simbrief_state(text: &str, port: u16) -> Option<bool> {
    if text.contains(&simbrief_target(port)) {
        Some(true)
    } else if text.contains(SIMBRIEF_HOST) {
        Some(false)
    } else {
        None
    }
}

/// Point a script's SimBrief address at the bridge. `None` when it does not name it.
pub fn patch_simbrief_text(text: &str, port: u16) -> Option<String> {
    if !text.contains(SIMBRIEF_HOST) {
        return None;
    }
    Some(text.replace(SIMBRIEF_HOST, &simbrief_target(port)))
}

/// Give a script its own SimBrief address back. `None` when it was not pointed here.
pub fn unpatch_simbrief_text(text: &str, port: u16) -> Option<String> {
    let target = simbrief_target(port);
    if !text.contains(&target) {
        return None;
    }
    Some(text.replace(&target, SIMBRIEF_HOST))
}

/// Scripts naming the SimBrief endpoint in a Community folder: (package, file, already
/// pointed at the bridge).
pub fn scan_simbrief(community: &Path, port: u16) -> Vec<(String, PathBuf, bool)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(community) else { return out };
    for pkg in rd.flatten() {
        let pdir = pkg.path();
        if !pdir.is_dir() {
            continue;
        }
        let mut js = Vec::new();
        walk_js(&pdir.join("html_ui"), &mut js);
        for f in js {
            let Ok(text) = fs::read_to_string(&f) else { continue };
            if let Some(patched) = simbrief_state(&text, port) {
                out.push((pkg.file_name().to_string_lossy().to_string(), f, patched));
            }
        }
    }
    out
}

/// Point every script naming SimBrief in a Community folder at the bridge.
pub fn patch_simbrief(community: &Path, port: u16) -> Result<Vec<PathBuf>> {
    let mut done = Vec::new();
    for (pkg, path, patched) in scan_simbrief(community, port) {
        if patched {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let Some(new_text) = patch_simbrief_text(&text, port) else { continue };
        fs::write(&path, new_text).with_context(|| format!("write {}", path.display()))?;
        log::info!("{pkg}: SimBrief plans now come from the bridge ({})", path.display());
        done.push(path);
    }
    Ok(done)
}

/// Give every script in a Community folder its own SimBrief address back.
pub fn unpatch_simbrief(community: &Path, port: u16) -> Result<Vec<PathBuf>> {
    let mut done = Vec::new();
    for (_pkg, path, patched) in scan_simbrief(community, port) {
        if !patched {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let Some(old) = unpatch_simbrief_text(&text, port) else { continue };
        fs::write(&path, old).with_context(|| format!("write {}", path.display()))?;
        done.push(path);
    }
    Ok(done)
}

/// Candidate Community folders for MSFS 2020 and 2024 (Store and Steam) on this machine.
pub fn detect_community_dirs() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return super::desktop::detect_sims().into_iter().map(|s| s.community).collect();
    }
    let mut out = Vec::new();
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let roaming = std::env::var("APPDATA").unwrap_or_default();
    let cfgs = [
        format!("{local}/Packages/Microsoft.FlightSimulator_8wekyb3d8bbwe/LocalCache/UserCfg.opt"),
        format!("{local}/Packages/Microsoft.Limitless_8wekyb3d8bbwe/LocalCache/UserCfg.opt"),
        format!("{roaming}/Microsoft Flight Simulator/UserCfg.opt"),
        format!("{roaming}/Microsoft Flight Simulator 2024/UserCfg.opt"),
    ];
    for c in cfgs {
        if let Ok(t) = fs::read_to_string(&c) {
            for line in t.lines() {
                let l = line.trim();
                if let Some(rest) = l.strip_prefix("InstalledPackagesPath") {
                    let p = rest.trim().trim_matches('"');
                    let d = Path::new(p).join("Community");
                    if d.is_dir() && !out.contains(&d) {
                        out.push(d);
                    }
                }
            }
        }
    }
    for d in [
        format!("{local}/Packages/Microsoft.FlightSimulator_8wekyb3d8bbwe/LocalCache/Packages/Community"),
        format!("{local}/Packages/Microsoft.Limitless_8wekyb3d8bbwe/LocalCache/Packages/Community"),
    ] {
        let d = PathBuf::from(d);
        if d.is_dir() && !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

fn walk_js(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_js(&p, out);
        } else if p.extension().and_then(|s| s.to_str()).map_or(false, |x| x.eq_ignore_ascii_case("js") || x.eq_ignore_ascii_case("mjs")) {
            out.push(p);
        }
    }
}

/// A file that references the Navigraph AMDB host, with the package it belongs to.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub package: String,
    pub path: PathBuf,
    pub literal_hits: usize,
    pub template_hits: usize,
}

pub fn scan(community: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(community) else { return out };
    for pkg in rd.flatten() {
        let pdir = pkg.path();
        if !pdir.is_dir() {
            continue;
        }
        let mut js = Vec::new();
        walk_js(&pdir.join("html_ui"), &mut js);
        for f in js {
            let Ok(text) = fs::read_to_string(&f) else { continue };
            let literal = text.matches(super::NAVIGRAPH_AMDB_HOST).count();
            let template = text.matches("https://amdb.api.${").count();
            if literal + template > 0 {
                out.push(Candidate { package: pkg.file_name().to_string_lossy().to_string(), path: f, literal_hits: literal, template_hits: template });
            }
        }
    }
    out
}

/// Packages whose compiled WASM gauges talk to the Navigraph AMDB host (iniBuilds
/// A350 and similar). These cannot be patched; only the hosts-file redirect reaches them.
pub fn scan_wasm(community: &Path) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()).map_or(false, |x| x.eq_ignore_ascii_case("wasm")) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(community) else { return out };
    let needle = super::NAVIGRAPH_AMDB_HOST.as_bytes();
    for pkg in rd.flatten() {
        let pdir = pkg.path();
        if !pdir.is_dir() {
            continue;
        }
        let mut wasm = Vec::new();
        walk(&pdir.join("SimObjects"), &mut wasm);
        for f in wasm {
            let Ok(bytes) = fs::read(&f) else { continue };
            if bytes.windows(needle.len()).any(|w| w == needle) {
                out.push((pkg.file_name().to_string_lossy().to_string(), f));
                break;
            }
        }
    }
    out
}

fn record_path(community: &Path) -> PathBuf {
    community.join(".amdb-bridge-patches.json")
}

pub fn load_record(community: &Path) -> PatchRecord {
    fs::read_to_string(record_path(community)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_record(community: &Path, r: &PatchRecord) -> Result<()> {
    fs::write(record_path(community), serde_json::to_string_pretty(r)?)?;
    Ok(())
}

/// Patch every candidate in a Community folder. Returns the files changed.
pub fn patch(community: &Path, port: u16, dry_run: bool) -> Result<Vec<PatchedFile>> {
    let local = format!("http://127.0.0.1:{port}");
    let mut record = load_record(community);
    let mut done = Vec::new();
    for c in scan(community) {
        let text = fs::read_to_string(&c.path)?;
        let patched = text.replace(super::NAVIGRAPH_AMDB_HOST, &local).replace("https://amdb.api.${", &format!("{local}/${{"));
        if patched == text {
            continue;
        }
        let n = c.literal_hits + c.template_hits;
        log::info!("{}{}: {} replacement(s) in {}", if dry_run { "[dry-run] " } else { "" }, c.package, n, c.path.display());
        if dry_run {
            done.push(PatchedFile { path: c.path.clone(), backup: PathBuf::new(), replacements: n });
            continue;
        }
        let backup = PathBuf::from(format!("{}{}", c.path.display(), BACKUP_SUFFIX));
        if !backup.exists() {
            fs::copy(&c.path, &backup).with_context(|| format!("backup {}", c.path.display()))?;
        }
        fs::write(&c.path, patched)?;
        let pf = PatchedFile { path: c.path.clone(), backup, replacements: n };
        record.files.retain(|f| f.path != pf.path);
        record.files.push(pf.clone());
        done.push(pf);
    }
    if !dry_run && !done.is_empty() {
        save_record(community, &record)?;
    }
    Ok(done)
}

/// Restore every backed-up file in a Community folder.
pub fn unpatch(community: &Path) -> Result<usize> {
    let record = load_record(community);
    let mut n = 0;
    for f in &record.files {
        if f.backup.is_file() {
            fs::copy(&f.backup, &f.path).with_context(|| format!("restore {}", f.path.display()))?;
            let _ = fs::remove_file(&f.backup);
            n += 1;
        }
    }
    let _ = fs::remove_file(record_path(community));
    // The flight bags keep what their charts patch replaced inside the file, not in a
    // backup, so they are put back by reading it out.
    n += unpatch_charts(community)?.len();
    Ok(n)
}

/// Put one patched file back and forget it. Returns false when it was not patched.
pub fn restore_one(community: &Path, path: &Path) -> Result<bool> {
    let mut record = load_record(community);
    let Some(f) = record.files.iter().find(|f| f.path == path).cloned() else { return Ok(false) };
    if f.backup.is_file() {
        fs::copy(&f.backup, &f.path).with_context(|| format!("restore {}", f.path.display()))?;
        let _ = fs::remove_file(&f.backup);
    }
    record.files.retain(|o| o.path != f.path);
    if record.files.is_empty() {
        let _ = fs::remove_file(record_path(community));
    } else {
        save_record(community, &record)?;
    }
    Ok(true)
}

/// Take the moving map out of the A220's instrument page, wherever it was added.
pub fn unpatch_a220_page(community: &Path) -> Result<usize> {
    let mut n = 0;
    for (_, path, patched) in scan_a220_page(community) {
        if patched && restore_one(community, &path)? {
            n += 1;
        }
    }
    Ok(n)
}

/// Every simulator exe.xml that exists on this machine.
pub fn exe_xml_files() -> Vec<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let roaming = std::env::var("APPDATA").unwrap_or_default();
    [
        format!("{local}/Packages/Microsoft.FlightSimulator_8wekyb3d8bbwe/LocalCache/exe.xml"),
        format!("{local}/Packages/Microsoft.Limitless_8wekyb3d8bbwe/LocalCache/exe.xml"),
        format!("{roaming}/Microsoft Flight Simulator/exe.xml"),
        format!("{roaming}/Microsoft Flight Simulator 2024/exe.xml"),
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|p| p.is_file())
    .collect()
}

/// Is the bridge registered to start with any simulator?
pub fn autostart_installed() -> bool {
    exe_xml_files().iter().any(|p| fs::read_to_string(p).map_or(false, |t| t.contains(AUTOSTART_NAME)))
}

const AUTOSTART_NAME: &str = "<Name>AMDB Bridge</Name>";

/// Remove the bridge's `Launch.Addon` block from every exe.xml. Returns the files changed.
pub fn remove_autostart() -> Result<Vec<PathBuf>> {
    let mut changed = Vec::new();
    for p in exe_xml_files() {
        let text = fs::read_to_string(&p)?;
        let Some(name) = text.find(AUTOSTART_NAME) else { continue };
        let Some(open) = text[..name].rfind("<Launch.Addon>") else { continue };
        let Some(close_rel) = text[name..].find("</Launch.Addon>") else { continue };
        let mut end = name + close_rel + "</Launch.Addon>".len();
        // Take the line break and the indentation before the block with it.
        let start = text[..open].rfind('\n').map_or(open, |i| i + 1);
        if text[end..].starts_with("\r\n") {
            end += 2;
        } else if text[end..].starts_with('\n') {
            end += 1;
        }
        let new = format!("{}{}", &text[..start], &text[end..]);
        fs::write(&p, new)?;
        changed.push(p);
    }
    Ok(changed)
}

/// Register the bridge in the simulator's exe.xml so it starts with the sim.
pub fn install_autostart(exe: &Path, args: &str) -> Result<Vec<PathBuf>> {
    let candidates: Vec<String> = exe_xml_files().iter().map(|p| p.display().to_string()).collect();
    let entry = format!(
        "  <Launch.Addon>\n    <Name>AMDB Bridge</Name>\n    <Disabled>False</Disabled>\n    <ManualLoad>False</ManualLoad>\n    <Path>{}</Path>\n    <CommandLine>{}</CommandLine>\n  </Launch.Addon>\n",
        exe.display(),
        args
    );
    let mut written = Vec::new();
    for c in candidates {
        let p = PathBuf::from(&c);
        if !p.is_file() {
            continue;
        }
        let text = fs::read_to_string(&p)?;
        if text.contains(AUTOSTART_NAME) {
            continue;
        }
        let Some(pos) = text.rfind("</SimBase.Document>") else { continue };
        let new = format!("{}{}{}", &text[..pos], entry, &text[pos..]);
        fs::copy(&p, format!("{c}{BACKUP_SUFFIX}"))?;
        fs::write(&p, new)?;
        written.push(p);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a350_handler_is_rewritten() {
        let src = "x['on']('RequestNavigraphAccessToken',()=>{const a='{';if(DataStore['get']('a350_ng_isauthed')=='0'){Coherent['call']('COMM_BUS_WASM_CALLBACK','SetNavigraphAccessToken','');return;}}),this['t']=setInterval(()=>{},1);";
        let out = patch_a350_text(src).unwrap();
        assert!(out.contains("'RequestNavigraphAccessToken',()=>{/*amdb-bridge*/Coherent.call('COMM_BUS_WASM_CALLBACK','SetNavigraphAccessToken','amdb-bridge-local');}),this['t']=setInterval"));
        assert!(!out.contains("a350_ng_isauthed"));
        assert!(out.trim_end().ends_with("},30000);"));
        assert!(patch_a350_text(&out).is_none(), "idempotent");
        assert!(patch_a350_text("nothing here").is_none());
    }

    #[test]
    fn a220_map_gets_token_fallback_and_bridge_search() {
        let src = format!("const SHARED = window.GM5_A220_AMM_SHARED;\n{}\n    const x = 1;\n{}\n        let lastError = null;\n    }}\n", A220_TOKEN_FN, A220_SEARCH_FN);
        let out = patch_a220_text(&src).unwrap();
        assert!(out.contains("if (!token) return 'amdb-bridge-local';"));
        assert!(out.contains("async function bridgeNearestSearch("));
        assert!(out.contains("async function runNearestSearchWithRetryNative("));
        assert_eq!(out.matches("async function runNearestSearchWithRetry(").count(), 1);
        assert!(patch_a220_text(&out).is_none(), "idempotent");
        assert!(patch_a220_text("not the map").is_none());
        assert!(a220_load_order_warning(Path::new("C:/nowhere"), "gm5-a220-amm").is_none());
    }

    #[test]
    fn a220_page_loads_the_map_and_keeps_the_aircraft_intact() {
        let page = "<script type=\"text/html\" id=\"DisplayUnits\">\n</script>\n<script type=\"text/html\" import-async=\"false\" import-script=\"/Pages/VCockpit/Instruments/a22x/DisplayUnits/instrument.index.js\"></script>\n<link rel=\"stylesheet\" href=\"/Pages/VCockpit/Instruments/a22x/DisplayUnits/instrument.css\" />\n";
        let out = patch_a220_page_text(page).unwrap();
        assert!(out.contains("amdb-a220-amm.js"));
        assert!(out.contains("amdb-a220-amm.css"));
        // The aircraft's own two lines survive, in order, ahead of ours.
        assert!(out.find("instrument.index.js").unwrap() < out.find("amdb-a220-amm.js").unwrap());
        assert!(out.contains("instrument.css"));
        assert!(patch_a220_page_text(&out).is_none(), "idempotent");
        assert!(patch_a220_page_text("some other instrument page").is_none());
    }

    /// The three address functions as the A350's obfuscated bundle writes them, and as
    /// the PMDG tablet does, each pointed at the bridge and put back to the byte.
    #[test]
    fn flight_bag_charts_point_here_and_come_back() {
        let a350 = "IDENTITY_REVOCATION_ENDPOINT=_0x33dd97(0x1a89),getIdentityApiRoot=()=>_0x33dd97(0x36c)+getDefaultAppDomain(),getIdentityDeviceAuthEndpoint=()=>getIdentityApiRoot()+IDENTITY_DEVICE_AUTH_ENDPOINT;var getChartsApiRoot=()=>_0x33dd97(0x1687)+getDefaultAppDomain()+_0x33dd97(0x1354),getAirportApiRoot=()=>_0x33dd97(0x1687)+getDefaultAppDomain()+_0x33dd97(0x48a);function getAirportInfo(){}";
        let pmdg = "var getIdentityApiRoot = () => `https://identity.api.${getDefaultAppDomain$1()}`;\nvar x = 1;\nvar getChartsApiRoot = () => `https://api.${getDefaultAppDomain$1()}/v2/charts`;\nvar getAirportApiRoot = () => `https://api.${getDefaultAppDomain$1()}/v2/airport`;\n";
        for original in [a350, pmdg] {
            let patched = patch_charts_text(original, 8770).unwrap();
            assert!(patched.contains("'http://127.0.0.1:8770/identity'"));
            assert!(patched.contains("'http://127.0.0.1:8770/v2/charts'"));
            assert!(patched.contains("'http://127.0.0.1:8770/v2/airport'"));
            // The calls that use the roots are left alone.
            assert!(patched.contains("getIdentityApiRoot()+IDENTITY_DEVICE_AUTH_ENDPOINT") || original == pmdg);
            assert!(patch_charts_text(&patched, 8770).is_none(), "idempotent");
            assert_eq!(unpatch_charts_text(&patched).unwrap(), original);
        }
        // The A220's own client, which names both hosts in its string table.
        let a220 = "var _0x=['https://',\'https://api.navigraph.com\',\'/v2/charts\',\'https://identity.api.navigraph.com\',\'/connect/token\'];";
        let patched = patch_charts_text(a220, 8770).unwrap();
        assert!(patched.contains("'http://127.0.0.1:8770'"));
        assert!(patched.contains("'http://127.0.0.1:8770/identity'"));
        assert!(!patched.contains("navigraph.com'"));
        assert_eq!(unpatch_charts_text(&patched).unwrap(), a220);
        // Not a flight bag, or one missing a root: left alone.
        assert!(patch_charts_text("var getChartsApiRoot = () => 'x';", 8770).is_none());
        assert!(unpatch_charts_text(a350).is_none());
    }

    /// The installed flight bags on this machine, patched and put back in memory without
    /// writing anything. Run by hand: `cargo test -- --ignored installed_flight_bags`.
    #[test]
    #[ignore]
    fn installed_flight_bags() {
        let mut seen = 0;
        for d in detect_community_dirs() {
            for (pkg, path, patched) in scan_charts(&d) {
                let text = fs::read_to_string(&path).unwrap();
                let original = if patched { unpatch_charts_text(&text).unwrap() } else { text };
                let out = patch_charts_text(&original, 8770).unwrap_or_else(|| panic!("{pkg}: {} not recognised", path.display()));
                assert_eq!(unpatch_charts_text(&out).unwrap(), original, "{pkg} round trip");
                println!("{pkg}: {} ok ({} bytes)", path.display(), out.len());
                seen += 1;
            }
        }
        assert!(seen > 0, "no flight bags found");
    }

    #[test]
    fn simbrief_address_is_pointed_here_and_put_back() {
        let original = "async function fetchOfp(id){return fetch('https://www.simbrief.com/api/xml.fetcher.php?userid='+id+'&json=1');}";
        let patched = patch_simbrief_text(original, 8770).unwrap();
        assert!(patched.contains("http://127.0.0.1:8770/api/xml.fetcher.php?userid="));
        assert!(!patched.contains("simbrief.com"));
        assert!(patch_simbrief_text(&patched, 8770).is_none(), "idempotent");
        assert_eq!(unpatch_simbrief_text(&patched, 8770).unwrap(), original);
        assert!(unpatch_simbrief_text(original, 8770).is_none());
        assert!(patch_simbrief_text("no simbrief here", 8770).is_none());
    }

    #[test]
    fn simbrief_scan_reports_which_state_a_script_is_in() {
        let dir = std::env::temp_dir().join(format!("amdb-bridge-simbrief-{}", std::process::id()));
        let pkg = dir.join("some-efb").join("html_ui");
        fs::create_dir_all(&pkg).unwrap();
        let f = pkg.join("efb.js");
        fs::write(&f, "fetch(`https://www.simbrief.com/api/xml.fetcher.php?json=1`)").unwrap();
        let found = scan_simbrief(&dir, 8770);
        assert_eq!(found.len(), 1);
        assert!(!found[0].2, "not patched yet");
        patch_simbrief(&dir, 8770).unwrap();
        let found = scan_simbrief(&dir, 8770);
        assert!(found[0].2, "patched now");
        unpatch_simbrief(&dir, 8770).unwrap();
        let found = scan_simbrief(&dir, 8770);
        assert!(!found[0].2, "restored");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn patches_and_restores() {
        let dir = std::env::temp_dir().join(format!("amdb-bridge-patch-{}", std::process::id()));
        let pkg = dir.join("some-aircraft").join("html_ui").join("Pages");
        fs::create_dir_all(&pkg).unwrap();
        let f = pkg.join("bundle.js");
        let original = "fetch(`https://amdb.api.navigraph.com/v1/${icao}`); x=`https://amdb.api.${dom()}/v1/search`;";
        fs::write(&f, original).unwrap();
        let done = patch(&dir, 8770, false).unwrap();
        assert_eq!(done.len(), 1);
        let t = fs::read_to_string(&f).unwrap();
        assert!(t.contains("http://127.0.0.1:8770/v1/${icao}"));
        assert!(t.contains("http://127.0.0.1:8770/${dom()}/v1/search"));
        assert!(!t.contains("navigraph.com"));
        assert!(scan(&dir).is_empty());
        assert_eq!(unpatch(&dir).unwrap(), 1);
        assert_eq!(fs::read_to_string(&f).unwrap(), original);
        let _ = fs::remove_dir_all(&dir);
    }
}
