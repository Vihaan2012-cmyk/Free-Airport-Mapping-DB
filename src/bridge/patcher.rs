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

/// The comm-bus event the OANS gauge asks the flight bag for a token with.
const TOKEN_EVENT: &str = "RequestNavigraphAccessToken";

/// Opening `{` of the block that answers the gauge's token request.
///
/// The A350 ships `'RequestNavigraphAccessToken',()=>{`, which was matched outright, and
/// iniBuilds' newer flight bag spells the same handoff with different quoting and
/// spacing. Matching the event name alone would be too loose: a bundle that both raises
/// the event and answers it names it twice, and the first is as likely to be the raise --
/// rewriting that would push a token from the wrong side and leave the handler as it was.
///
/// So the anchor is the registration rather than the name: the name has to be the
/// argument of an `on(` call. What follows is the callback, `=>` or `function`, and then
/// its block. Found exactly once, or not at all -- if a bundle registers the handler
/// twice this does not guess which.
fn token_handler_open(text: &str) -> Option<usize> {
    let quote = |c: char| c == '\u{27}' || c == '\u{22}' || c == '\u{60}';
    let mut found = None;
    let mut from = 0;
    while let Some(at) = text[from..].find(TOKEN_EVENT) {
        let at = from + at;
        from = at + TOKEN_EVENT.len();
        // The name must be the first argument of an `on` call. Minifiers spell that
        // either `.on(` or `['on'](`, so the quotes and brackets come off before the
        // method's name is read, and `on` has to be the whole of it -- `addon(` is not.
        let before = text[..at].trim_end_matches(quote).trim_end();
        let Some(before) = before.strip_suffix('(') else { continue };
        let before = before.trim_end().trim_end_matches(']').trim_end_matches(quote);
        let Some(head) = before.strip_suffix("on") else { continue };
        if head.chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
            continue;
        }
        // The callback follows the name, inside the same argument list.
        let after = &text[from..];
        let window = &after[..after.len().min(80)];
        let Some(rel) = window.find("=>").or_else(|| window.find("function")) else { continue };
        let Some(brace) = after[rel..].find('{') else { continue };
        if found.is_some() {
            return None;
        }
        found = Some(from + rel + brace);
    }
    found
}

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
    let open = token_handler_open(text)?;
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
            // iniBuilds' newer flight bag does not use the A350's `ini-efb*` bundle
            // names, so the name is only a way to avoid reading every script in the
            // folder; what settles it is whether the handler is actually in there.
            if !name.contains("efb") && !name.contains("ois") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&f) else { continue };
            if token_handler_open(&text).is_some() || text.contains(A350_MARK) {
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
///
/// This is only a cheap way to avoid reading every script in a Community folder; whether
/// a file is really a flight bag is settled by [`knows_charts`], which asks whether the
/// SDK's three address functions are in it. `efb.js` is FlyByWire's -- the A32NX and the
/// A380X both build their flight bag to that name out of `fbw-common` -- and is general
/// enough that it would match other things, which is exactly why the real test is the
/// contents and not the name.
fn is_efb_script(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("ini-efb") || name == "pmdgtablet.js" || name.starts_with("efb-a220") || name == "efb.js"
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

// ---------------------------------------------------------------------------------
// Fenix A320 OANS shell. Proof-of-life only: one invisible manager gauge (NO_TEXTURE,
// 1x1 -- the pattern Fenix's own LOD.html already uses) added as a new [VCockpitNN]
// block in Fenix's own panel.cfg, pointed at a single HTML file our own package adds
// under the aircraft's html_ui folder. Nothing of Fenix's own is replaced; panel.cfg
// is the aircraft's own file, edited where it sits, with a backup kept beside it.
// ---------------------------------------------------------------------------------

const FENIX_OANS_MARK: &str = "//amdb-bridge-fenix-oans";
/// Paths our own package adds under `html_ui/Pages/VCockpit/Instruments/`, where panel.cfg
/// gauge paths resolve -- Fenix never had anything here, so nothing is overridden.
const FENIX_OANS_GAUGE_PATH: &str = "amdb-oans/oans-shell.html";
const FENIX_OANS_ND_PATH: &str = "amdb-oans/oans-nd.html";
/// The Captain ND's block is found by its texture, not its number.
const FENIX_ND_TEXTURE: &str = "texture=$A320_ND_Captain";

/// Add both OANS gauges to Fenix's panel.cfg text: the invisible manager as a new
/// `[VCockpitNN]` block, and the ND overlay stacked after the Captain ND's own gauge on
/// the same texture (the way Fenix stacks its PFD over its weather radar). Only what is
/// missing is added. None when both are already there, or when either anchor is not
/// found (a panel.cfg shape this bridge does not know).
pub fn patch_fenix_oans_text(text: &str) -> Option<String> {
    let mut out = text.to_string();
    if !out.contains(FENIX_OANS_GAUGE_PATH) {
        out = add_fenix_manager_block(&out)?;
    }
    if !out.contains(FENIX_OANS_ND_PATH) {
        out = add_fenix_nd_overlay(&out)?;
    }
    if !fenix_nd_is_sharp(&out) {
        out = sharpen_fenix_nd(&out)?;
    }
    (out != text).then_some(out)
}

/// The captain ND's `[VCockpit..]` block: its start and end in the text.
fn fenix_nd_block(text: &str) -> Option<(usize, usize)> {
    let tex = text.find(FENIX_ND_TEXTURE)?;
    let start = text[..tex].rfind("\n[").map_or(0, |i| i + 1);
    let end = text[tex..].find("\n[").map_or(text.len(), |i| tex + i);
    Some((start, end))
}

/// A `key=W,H` line's two numbers, spaces allowed.
fn fenix_pair(block: &str, key: &str) -> Option<(u32, u32)> {
    let line = block.lines().find(|l| l.trim_start().starts_with(key) && l.trim_start()[key.len()..].trim_start().starts_with('='))?;
    let (w, h) = line.split_once('=')?.1.split_once(',')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The captain ND is drawn at twice the pixels of its layout, for sharp OANS text. Fenix's
/// own ND scales itself to it, and so does the OANS.
fn fenix_nd_is_sharp(text: &str) -> bool {
    let Some((start, end)) = fenix_nd_block(text) else { return false };
    let block = &text[start..end];
    matches!((fenix_pair(block, "size_mm"), fenix_pair(block, "pixel_size")), (Some((w, h)), Some((pw, ph))) if pw >= 2 * w && ph >= 2 * h)
}

/// Render the captain ND's texture at twice its layout size (768 -> 1536).
fn sharpen_fenix_nd(text: &str) -> Option<String> {
    let (start, end) = fenix_nd_block(text)?;
    let block = &text[start..end];
    let (w, h) = fenix_pair(block, "size_mm")?;
    let line_at = block.lines().scan(0, |at, l| {
        let here = *at;
        *at += l.len() + 1;
        Some((here, l))
    });
    let (at, line) = line_at.into_iter().find(|(_, l)| l.trim_start().starts_with("pixel_size"))?;
    let sharp = format!("{FENIX_OANS_MARK} drawn at twice the pixels, for sharp OANS text\npixel_size={},{}", 2 * w, 2 * h);
    let from = start + at;
    Some(format!("{}{}{}", &text[..from], sharp, &text[from + line.len()..]))
}

/// Stack the ND overlay after the last gauge in the Captain ND's block, at that block's size.
fn add_fenix_nd_overlay(text: &str) -> Option<String> {
    let tex = text.find(FENIX_ND_TEXTURE)?;
    let start = text[..tex].rfind("\n[").map_or(0, |i| i + 1);
    let end = text[tex..].find("\n[").map_or(text.len(), |i| tex + i);
    let block = &text[start..end];
    let mut next = 0u32;
    let mut from = 0;
    while let Some(at) = block[from..].find("htmlgauge") {
        let at = from + at + "htmlgauge".len();
        from = at;
        let digits: String = block[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u32>() {
            next = next.max(n + 1);
        }
    }
    let size = block.lines().find_map(|l| l.trim().strip_prefix("size_mm=")).map(|s| s.replace(' ', "")).unwrap_or_else(|| "768,768".to_string());
    let at = start + block.trim_end().len();
    let line = format!("\n{FENIX_OANS_MARK}\nhtmlgauge{next:02}={FENIX_OANS_ND_PATH}, 0,0,{size}");
    Some(format!("{}{}{}", &text[..at], line, &text[at..]))
}

fn add_fenix_manager_block(text: &str) -> Option<String> {
    let mut max = 0u32;
    let mut from = 0;
    while let Some(at) = text[from..].find("[VCockpit") {
        let at = from + at + "[VCockpit".len();
        from = at;
        let digits: String = text[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u32>() {
            max = max.max(n);
        }
    }
    if max == 0 {
        return None;
    }
    let next = max + 1;
    let block = format!("[VCockpit{next:02}]\n{FENIX_OANS_MARK}\nsize_mm=1,1\npixel_size=1,1\ntexture=NO_TEXTURE\nhtmlgauge00={FENIX_OANS_GAUGE_PATH}, 0,0,1,1\n");
    // After the last cockpit, before the paintings (MSFS 2020); the MSFS 2024 cockpit part
    // has none, and it goes at the end.
    Some(match text.find("\n[VPainting") {
        Some(at) => format!("{}{}\n{}", &text[..at + 1], block, &text[at + 1..]),
        None => format!("{}\n\n{}", text.trim_end(), block),
    })
}

// The Captain EFIS range knob. Its template runs INC_CODE / DEC_CODE for every click,
// wheel step and drag, and lets each knob override them, so the knob itself is given one
// more position below 10 NM without Fenix's own range variable ever leaving 0..5:
// turning left past 10 NM turns OANS on and keeps zooming in (5, 2, 1, 0.5, 0.2 NM);
// turning right zooms back out and, past 5 NM, returns to the normal ND at 10 NM.
// OANS lives in L:AMDB_OANS_ZOOM: 0 off, 1..5 closer in.

const FENIX_KNOB_ANCHOR: &str = "<ANIM_NAME>EFIS_1_Range_Selector_Knob</ANIM_NAME>";
const FENIX_KNOB_MARK: &str = "<!--amdb-bridge-fenix-oans-->";
const FENIX_KNOB_DEC: &str = "(L:AMDB_OANS_ZOOM) 0 &gt; if{ (L:AMDB_OANS_ZOOM) 1 + 5 min (&gt;L:AMDB_OANS_ZOOM) } els{ (L:S_FCU_EFIS1_ND_ZOOM) 0 &gt; if{ (L:S_FCU_EFIS1_ND_ZOOM) 1 - (&gt;L:S_FCU_EFIS1_ND_ZOOM) } els{ 1 (&gt;L:AMDB_OANS_ZOOM) } }";
const FENIX_KNOB_INC: &str = "(L:AMDB_OANS_ZOOM) 0 &gt; if{ (L:AMDB_OANS_ZOOM) 1 - (&gt;L:AMDB_OANS_ZOOM) } els{ (L:S_FCU_EFIS1_ND_ZOOM) 1 + 5 min (&gt;L:S_FCU_EFIS1_ND_ZOOM) }";

/// Give the Captain range knob its OANS positions. None when already done, or when the
/// knob is not where this bridge expects it.
pub fn patch_fenix_knob_text(text: &str) -> Option<String> {
    if text.contains(FENIX_KNOB_MARK) {
        return None;
    }
    let at = text.find(FENIX_KNOB_ANCHOR)?;
    let close = at + text[at..].find("</UseTemplate>")?;
    let line = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent = &text[line..at];
    let close_line = text[..close].rfind('\n').map_or(0, |i| i + 1);
    let added = format!("{indent}{FENIX_KNOB_MARK}\n{indent}<INC_CODE>{FENIX_KNOB_INC}</INC_CODE>\n{indent}<DEC_CODE>{FENIX_KNOB_DEC}</DEC_CODE>\n");
    Some(format!("{}{}{}", &text[..close_line], added, &text[close_line..]))
}

/// The two Fenix files the OANS needs changed: panel.cfg for the gauges, the cockpit
/// model for the range knob.
/// The Fenix files the OANS changes, in the MSFS 2020 layout and in the MSFS 2024 one,
/// where the cockpit is an attachment with its own panel.cfg and behaviours.
fn fenix_oans_files(pdir: &Path) -> [(PathBuf, &'static str); 4] {
    let fnx = pdir.join("SimObjects/Airplanes/FNX_32X");
    let cockpit = fnx.join("attachments/fnx/Part_Interior_Cockpit");
    [
        (fnx.join("Panel/panel.cfg"), "panel.cfg"),
        (fnx.join("model/FNX32X_Interior.xml"), "cockpit model"),
        (cockpit.join("panel/panel.cfg"), "panel.cfg"),
        (cockpit.join("model/Cockpit_Behavior.xml"), "cockpit model"),
    ]
}

fn fenix_file_patched(kind: &str, text: &str) -> bool {
    match kind {
        "panel.cfg" => text.contains(FENIX_OANS_GAUGE_PATH) && text.contains(FENIX_OANS_ND_PATH) && fenix_nd_is_sharp(text),
        _ => text.contains(FENIX_KNOB_MARK),
    }
}

/// Fenix files in a Community folder the OANS changes: (package, file, already patched).
pub fn scan_fenix_oans(community: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(community) else { return out };
    for pkg in rd.flatten() {
        let pdir = pkg.path();
        if !pdir.is_dir() {
            continue;
        }
        let name = pkg.file_name().to_string_lossy().to_string();
        for (path, kind) in fenix_oans_files(&pdir) {
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let is_fenix = if kind == "panel.cfg" { text.contains("[VCockpit") } else { text.contains(FENIX_KNOB_ANCHOR) || text.contains(FENIX_KNOB_MARK) };
            if is_fenix {
                out.push((name.clone(), path, fenix_file_patched(kind, &text)));
            }
        }
    }
    out
}

/// Add the OANS gauges to Fenix's panel.cfg and the OANS positions to its range knob in a
/// Community folder (backups kept, recorded for `unpatch`).
pub fn patch_fenix_oans(community: &Path, dry_run: bool) -> Result<Vec<PatchedFile>> {
    let mut record = load_record(community);
    let mut done = Vec::new();
    for (pkg, path, patched) in scan_fenix_oans(community) {
        if patched {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let is_panel = path.file_name().is_some_and(|n| n == "panel.cfg");
        let Some(new_text) = (if is_panel { patch_fenix_oans_text(&text) } else { patch_fenix_knob_text(&text) }) else {
            log::warn!("{pkg}: {} is a version of this Fenix file the bridge does not know how to patch", path.display());
            continue;
        };
        log::info!("{}{}: OANS added to {}", if dry_run { "[dry-run] " } else { "" }, pkg, path.display());
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

/// Put Fenix's panel.cfg and range knob back, wherever the OANS changed them.
pub fn unpatch_fenix_oans(community: &Path) -> Result<usize> {
    let mut n = 0;
    for (_, path, patched) in scan_fenix_oans(community) {
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
    autostart_installed_as(AUTOSTART_NAME)
}

/// The name AMDB Bridge's entry goes by in exe.xml. The standalone A320 OANS has its own,
/// so neither replaces the other's.
pub const AUTOSTART_NAME: &str = "AMDB Bridge";

fn autostart_tag(name: &str) -> String {
    format!("<Name>{name}</Name>")
}

/// Is a program registered, under this name, to start with any simulator?
pub fn autostart_installed_as(name: &str) -> bool {
    let tag = autostart_tag(name);
    exe_xml_files().iter().any(|p| fs::read_to_string(p).map_or(false, |t| t.contains(&tag)))
}

/// Remove the bridge's `Launch.Addon` block from every exe.xml. Returns the files changed.
pub fn remove_autostart() -> Result<Vec<PathBuf>> {
    remove_autostart_as(AUTOSTART_NAME)
}

/// Remove the `Launch.Addon` block of this name from every exe.xml. Returns the files changed.
pub fn remove_autostart_as(name: &str) -> Result<Vec<PathBuf>> {
    let tag = autostart_tag(name);
    let mut changed = Vec::new();
    for p in exe_xml_files() {
        let text = fs::read_to_string(&p)?;
        let Some(at) = text.find(&tag) else { continue };
        let Some(open) = text[..at].rfind("<Launch.Addon>") else { continue };
        let Some(close_rel) = text[at..].find("</Launch.Addon>") else { continue };
        let mut end = at + close_rel + "</Launch.Addon>".len();
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
    install_autostart_as(AUTOSTART_NAME, exe, args)
}

/// Register a program, under this name, in the simulator's exe.xml so it starts with the sim.
pub fn install_autostart_as(name: &str, exe: &Path, args: &str) -> Result<Vec<PathBuf>> {
    let tag = autostart_tag(name);
    let candidates: Vec<String> = exe_xml_files().iter().map(|p| p.display().to_string()).collect();
    let entry = format!(
        "  <Launch.Addon>\n    {tag}\n    <Disabled>False</Disabled>\n    <ManualLoad>False</ManualLoad>\n    <Path>{}</Path>\n    <CommandLine>{}</CommandLine>\n  </Launch.Addon>\n",
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
        if text.contains(&tag) {
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

    /// The token handler is found by its registration, not by the event's name.
    ///
    /// A bundle that both raises the event and answers it names it twice, and the raise
    /// may well come first; rewriting that would push a token from the wrong side and
    /// leave the handler untouched. Anchoring on `on(` keeps them apart.
    #[test]
    fn token_handler_is_found_by_its_registration() {
        let a350 = "x['wasmListener']['on']('RequestNavigraphAccessToken',()=>{ return ''; });";
        assert!(token_handler_open(a350).is_some(), "the A350's own spelling");

        // iniBuilds' newer flight bag: different quoting, spacing and callback form.
        let newer = "bus.on( \"RequestNavigraphAccessToken\", function () { return null; });";
        assert!(token_handler_open(newer).is_some(), "a newer spelling of the same handoff");

        // The gauge side raises it; there is no handler here to rewrite.
        let raise = "this.emit('RequestNavigraphAccessToken');";
        assert!(token_handler_open(raise).is_none(), "a raise is not a handler");

        // Raised first, answered second: the registration is what must be found.
        let both = "emit('RequestNavigraphAccessToken');
bus.on('RequestNavigraphAccessToken',()=>{ return 'tok'; });";
        let open = token_handler_open(both).expect("the handler, not the raise");
        assert_eq!(both.as_bytes()[open], b'{', "the index is the opening brace itself");
        assert!(both[..open].ends_with("()=>"), "landed on {:?}", &both[open.saturating_sub(12)..=open]);
        assert!(both[..open].contains("bus.on("), "landed on the raise, not the registration");

        // Registered twice: this does not guess which.
        let twice = "bus.on('RequestNavigraphAccessToken',()=>{1});bus.on('RequestNavigraphAccessToken',()=>{2});";
        assert!(token_handler_open(twice).is_none(), "two registrations");
    }

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
    /// Every flight bag on this machine, rewritten and read back.
    ///
    /// They are all built on the same Navigraph SDK, so nothing about any one rewrite is
    /// new -- but FlyByWire's bundle is seven megabytes of minified JavaScript naming the
    /// three address functions several times each, once at the definition and again at
    /// every call, and the rewrite has to find the definitions and only those. A file
    /// named `efb.js` is also general enough to be somebody else's, which is why the scan
    /// settles it on the contents. Neither is worth taking on trust.
    ///
    /// Nothing is written: each file is rewritten in memory and thrown away.
    ///
    /// `cargo test --release -- --ignored rewrites_every_flight_bag`
    #[test]
    #[ignore]
    fn rewrites_every_flight_bag() {
        let mut seen = 0;
        for community in detect_community_dirs() {
            for (pkg, path, already) in scan_charts(&community) {
                let text = std::fs::read_to_string(&path).unwrap();
                // A file already pointed at the bridge is not rewritten again; put it back
                // first so this checks the rewrite rather than the guard against it.
                let text = if already { unpatch_charts_text(&text).expect("a patched file can be put back") } else { text };
                let out = patch_charts_text(&text, 8770).unwrap_or_else(|| panic!("{pkg}: {} is a flight bag the patch does not know", path.display()));
                assert!(out.contains("'http://127.0.0.1:8770/identity'"), "{pkg}: identity");
                if CHARTS_ROOTS.iter().all(|(n, _)| text.contains(n)) {
                    // Built on the SDK: it asks the three functions for whole addresses.
                    assert!(out.contains("'http://127.0.0.1:8770/v2/charts'"), "{pkg}: charts");
                    assert!(out.contains("'http://127.0.0.1:8770/v2/airport'"), "{pkg}: airport");
                } else {
                    // Its own client, naming the hosts outright and adding the path itself,
                    // so what it is given is the bridge and nothing after it.
                    assert!(out.contains("'http://127.0.0.1:8770'"), "{pkg}: host");
                }
                // What was replaced is kept inside the replacement, so `off` puts the file
                // back to the byte rather than to something that merely looks the same.
                assert!(out.contains(CHARTS_MARK), "{pkg}: the mark that lets it be put back");
                assert_eq!(unpatch_charts_text(&out).as_deref(), Some(text.as_str()), "{pkg}: does not come back the same");
                assert!(patch_charts_text(&out, 8770).is_none(), "{pkg}: a patched file is patched twice");
                println!("{pkg}: {} ok ({} bytes)", path.file_name().unwrap().to_string_lossy(), text.len());
                seen += 1;
            }
        }
        assert!(seen > 0, "no flight bag found in any Community folder");
    }

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

    const FENIX_PANEL: &str = "[VCockpit01]\nsize_mm=768,768\ntexture=$A320_PFD_Captain\nhtmlgauge00=A, 0,0,768,768\n\n[VCockpit02]\nsize_mm=768,768\npixel_size=768,768\ntexture=$A320_ND_Captain\nhtmlgauge00=B, 0,0,768,768\n\n[VCockpit15]\nsize_mm=1,1\ntexture=NO_TEXTURE\nhtmlgauge00=C, 0,0,1,1\n\n[VPainting01]\nsize_mm\t= 605, 128\n";

    #[test]
    fn fenix_oans_adds_manager_block_and_nd_overlay_once() {
        let out = patch_fenix_oans_text(FENIX_PANEL).unwrap();
        assert!(out.contains("htmlgauge00=C, 0,0,1,1\n\n[VCockpit16]\n//amdb-bridge-fenix-oans\nsize_mm=1,1\n"));
        assert!(out.contains("texture=NO_TEXTURE\nhtmlgauge00=amdb-oans/oans-shell.html, 0,0,1,1\n\n[VPainting01]"));
        assert!(out.contains("htmlgauge00=B, 0,0,768,768\n//amdb-bridge-fenix-oans\nhtmlgauge01=amdb-oans/oans-nd.html, 0,0,768,768\n\n[VCockpit15]"));
        assert!(out.contains("size_mm=768,768\n//amdb-bridge-fenix-oans drawn at twice the pixels, for sharp OANS text\npixel_size=1536,1536\ntexture=$A320_ND_Captain"));
        assert_eq!(out.matches("pixel_size=1536,1536").count(), 1, "only the ND is sharpened");
        assert!(!out[..out.find("[VCockpit02]").unwrap()].contains("oans-nd"), "overlay went into the PFD block");
        assert!(patch_fenix_oans_text(&out).is_none());
    }

    /// The MSFS 2024 cockpit part: the ND gauge after two others, and no paintings.
    const FENIX_2024_PANEL: &str = "[VCockpit02]\nsize_mm=768,768\npixel_size=768,768\ntexture=$A320_ND_Captain\nhtmlgauge00=R, 0,0,768,768\nhtmlgauge01=C, 0,0,384,264\nhtmlgauge02=ND, 0,0,768,768\n\n[VCockpit16]\nsize_mm = 1024, 512\ntexture = DisplaysReflection\nhtmlgauge00=F, 0, 0, 1024, 512";

    #[test]
    fn fenix_2024_cockpit_gets_the_overlay_on_top_and_the_manager_at_the_end() {
        let out = patch_fenix_oans_text(FENIX_2024_PANEL).unwrap();
        assert!(out.contains("htmlgauge02=ND, 0,0,768,768\n//amdb-bridge-fenix-oans\nhtmlgauge03=amdb-oans/oans-nd.html, 0,0,768,768\n\n[VCockpit16]"));
        assert!(out.ends_with("htmlgauge00=F, 0, 0, 1024, 512\n\n[VCockpit17]\n//amdb-bridge-fenix-oans\nsize_mm=1,1\npixel_size=1,1\ntexture=NO_TEXTURE\nhtmlgauge00=amdb-oans/oans-shell.html, 0,0,1,1\n"));
        assert!(patch_fenix_oans_text(&out).is_none());
    }

    #[test]
    fn fenix_2024_files_are_found_in_the_cockpit_attachment() {
        let root = std::env::temp_dir().join(format!("amdb-fenix-2024-{}", std::process::id()));
        let cockpit = root.join("fnx-aircraft-320/SimObjects/Airplanes/FNX_32X/attachments/fnx/Part_Interior_Cockpit");
        fs::create_dir_all(cockpit.join("panel")).unwrap();
        fs::create_dir_all(cockpit.join("model")).unwrap();
        fs::write(cockpit.join("panel/panel.cfg"), FENIX_2024_PANEL).unwrap();
        fs::write(cockpit.join("model/Cockpit_Behavior.xml"), "<UseTemplate><ANIM_NAME>EFIS_1_Range_Selector_Knob</ANIM_NAME>\n</UseTemplate>\n").unwrap();
        let found = scan_fenix_oans(&root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|(_, _, patched)| !patched));
    }

    #[test]
    fn fenix_knob_gains_oans_positions_once() {
        let xml = "\t\t\t<UseTemplate Name=\"FNX32X_Interact_Knob_Increment_Template\">\n\t\t\t\t<ANIM_NAME>EFIS_1_Mode_Selector_Knob</ANIM_NAME>\n\t\t\t\t<VAR_MAX>4</VAR_MAX>\n\t\t\t</UseTemplate>\n\t\t\t<UseTemplate Name=\"FNX32X_Interact_Knob_Increment_Template\">\n\t\t\t\t<ANIM_NAME>EFIS_1_Range_Selector_Knob</ANIM_NAME>\n\t\t\t\t<VAR_NAME>S_FCU_EFIS1_ND_ZOOM</VAR_NAME>\n\t\t\t\t<VAR_MAX>5</VAR_MAX>\n\t\t\t</UseTemplate>\n";
        let out = patch_fenix_knob_text(xml).unwrap();
        // Inside the range knob's own block, before its closing tag, at its indentation.
        let range = &out[out.find("EFIS_1_Range_Selector_Knob").unwrap()..];
        let inc = range.find("\t\t\t\t<INC_CODE>").unwrap();
        assert!(inc < range.find("</UseTemplate>").unwrap());
        assert!(range.contains("<DEC_CODE>(L:AMDB_OANS_ZOOM) 0 &gt; if{"));
        assert!(!out[..out.find("EFIS_1_Range_Selector_Knob").unwrap()].contains("INC_CODE"), "the mode knob was changed");
        assert!(patch_fenix_knob_text(&out).is_none());
    }

    #[test]
    fn fenix_oans_adds_only_the_missing_overlay() {
        let manager_only = FENIX_PANEL.replace("[VPainting01]", "[VCockpit16]\nhtmlgauge00=amdb-oans/oans-shell.html, 0,0,1,1\n\n[VPainting01]");
        let out = patch_fenix_oans_text(&manager_only).unwrap();
        assert_eq!(out.matches("oans-shell.html").count(), 1);
        assert_eq!(out.matches("oans-nd.html").count(), 1);
    }
}
