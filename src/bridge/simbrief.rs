//! A flight plan served the way SimBrief serves one, so the tablets that import from
//! SimBrief import ours.
//!
//! Every tablet reaches one fixed address:
//! `https://www.simbrief.com/api/xml.fetcher.php`, with `userid=` or `username=` naming
//! the pilot and `json=1` asking for JSON rather than SimBrief's native XML. That is what
//! makes this worth doing at all: unlike the chart endpoints, whose names differ from one
//! build to the next and have to be found by anchoring on the surrounding script, this one
//! address is the same literal in every flight bag, so pointing it here is a plain and
//! exactly reversible substitution.
//!
//! It was read out of the scripts of every aircraft installed on the machine this was
//! written on, in both of that machine's community folders: the FlyByWire A380X
//! (`A380X/EFB/efb.js`), the PMDG 737-800 and 777-300ER (`PMDGTablet.js`), the iniBuilds
//! A350 (`ini-efb-a350.js`) and the Synaptic A220 (`a22x/DisplayUnits/instrument.js`),
//! whose string table also carries the field names `plan_ramp`, `plan_takeoff`,
//! `icao_code` and `plan_rwy` this module serves. Two flight bags on the same machine
//! reach no such address at all and so cannot be served this way: the Fenix A320 and the
//! iFly 737 MAX.
//!
//! The fields are what those scripts read, and the FlyByWire A380X's parser is the
//! strictest of them: it takes `general`, `navlog`, `origin`, `aircraft`, `destination`,
//! `times`, `weights`, `fuel`, `params`, `files`, `text`, `weather`, `atc` and `alternate`
//! apart without checking any of them for null, so a reply that left out `files.pdf.link`
//! or `text.plan_html` would not merely look wrong to it — it would throw. Everything it
//! touches is therefore filled, and a test pins each one.
//!
//! `patcher::patch_simbrief_text` points the literal address at this bridge; this module
//! answers whatever it is asked with the one plan `amdbgen dispatch` last saved,
//! whichever `userid` or `username` was asked for. A pilot who wants a *different*
//! aircraft's SimBrief plan served needs their own SimBrief account and the bridge turned
//! off for that flight — the tablets have no way to ask this bridge for anything but the
//! last plan dispatched, and that is the whole of what stands in for their sign-in.

use crate::dispatch::Dispatch;
use crate::ofp::simbrief as shape;
use crate::ofp::DispatchOptions;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// Where `amdbgen dispatch` saves the last plan, and where this module reads it back
/// from: the bridge's own data directory, so it survives the bridge restarting and needs
/// no extra setup.
pub fn data_path() -> PathBuf {
    super::tls::data_dir().join("dispatch").join("last.json")
}

#[derive(Serialize, Deserialize)]
struct SavedPlan {
    dispatch: Dispatch,
    opts: DispatchOptions,
}

/// Save a plan where this module, and the next run of `amdb-bridge`, will find it.
pub fn save(dispatch: &Dispatch, opts: &DispatchOptions) -> Result<PathBuf> {
    let path = data_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(&SavedPlan { dispatch: dispatch.clone(), opts: opts.clone() })?;
    std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// The last plan saved, if one has been.
fn load() -> Result<(Dispatch, DispatchOptions)> {
    let path = data_path();
    let text = std::fs::read_to_string(&path).with_context(|| format!("{}: no flight has been dispatched yet (run `amdbgen dispatch`)", path.display()))?;
    let saved: SavedPlan = serde_json::from_str(&text).with_context(|| format!("read {}", path.display()))?;
    Ok((saved.dispatch, saved.opts))
}

/// Whether a plan is on file to serve.
pub fn has_plan() -> bool {
    data_path().is_file()
}

/// The last plan, as `api/xml.fetcher.php?json=1` answers it.
pub fn json() -> Result<Value> {
    let (d, opts) = load()?;
    Ok(shape::ofp_json(&d, &opts))
}

/// The last plan, as the endpoint answers without `json=1`.
pub fn xml() -> Result<String> {
    let (d, opts) = load()?;
    Ok(shape::ofp_xml(&d, &opts))
}

/// What a client asking `?userid=` or `?username=` with no plan on file sees: the same
/// shape SimBrief answers an unknown pilot with, so a tablet's own error handling (which
/// every one of them already has, for the day SimBrief itself is down) is what runs.
pub fn not_found_json() -> Value {
    serde_json::json!({ "fetch": { "status": "Error: no flight has been dispatched with amdbgen yet" } })
}

pub fn not_found_xml() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<OFP>\n  <fetch>\n    <status>Error: no flight has been dispatched with amdbgen yet</status>\n  </fetch>\n</OFP>\n".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ofp::fixtures;
    use std::sync::Mutex;

    // `data_path` reads a real, shared directory (`tls::data_dir()`), so the tests that
    // touch it run one at a time to avoid one test's save racing another's load.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn a_saved_plan_loads_back_in_the_simbrief_shape() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let d = fixtures::sample();
        let opts = fixtures::sample_opts();
        save(&d, &opts).unwrap();
        assert!(has_plan());
        let v = json().unwrap();
        assert_eq!(v["origin"]["icao_code"], d.route.origin.icao);
        let x = xml().unwrap();
        assert!(x.contains("<OFP>"));
        assert!(x.contains(&d.route.origin.icao));
        let _ = std::fs::remove_file(data_path());
    }

    #[test]
    fn no_saved_plan_is_a_clear_error_not_a_panic() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _ = std::fs::remove_file(data_path());
        assert!(json().is_err());
        assert!(xml().is_err());
        assert!(!has_plan());
    }

    #[test]
    fn the_not_found_shapes_carry_an_error_status() {
        assert_eq!(not_found_json()["fetch"]["status"].as_str().unwrap().starts_with("Error"), true);
        assert!(not_found_xml().contains("Error"));
    }
}
