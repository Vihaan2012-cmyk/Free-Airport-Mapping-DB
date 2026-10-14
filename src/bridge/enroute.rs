//! The flight bags' enroute map: `/enroute/styles/{style}/{z}/{x}/{y}@2x.png`, the address a
//! patched flight bag's map asks in place of Navigraph's enroute service. Answered from the
//! tiles drawn ahead (`amdbgen enroute-tiles`) into `<airports>/enroute/<cycle>/<style>/<z>/`, a
//! pack a column of them (`<x>.pack`) and any drawn since as files of their own (`<x>/<y>.png`).
//!
//! The set is chosen as the charts are (`filed::pick`): the asking aircraft's own cycle's
//! where there is one, else the newest drawn from an aircraft's database rather than the
//! simulator's. A tile the run found plain is not kept: one missing to the depth `meta.json`
//! says was drawn, or under one missing there, is the plain sea or land. Any other tile not
//! there is drawn the first time it is asked for, and kept.
//!
//! A style is asked for by day or by night (`ifr.lo.night`); the tiles are kept by day and
//! turned to night when they are served. `{y}.png` and `{y}@2x.png` are the same 512-pixel
//! tile.

use crate::output::enroute::{self, Data, Ground, Meta, Plain, Source};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The drawing data of a source, read the first time a tile has to be drawn from it here.
fn data(source: Source) -> Option<&'static Data> {
    static DATABASE: OnceLock<Option<Data>> = OnceLock::new();
    static SIMULATOR: OnceLock<Option<Data>> = OnceLock::new();
    let cell = if source == Source::Simulator { &SIMULATOR } else { &DATABASE };
    cell.get_or_init(|| match Data::load(&crate::sources::http::Http::new(120, 0), &crate::cache::Cache::for_index(false), source) {
        Ok(d) => Some(d),
        Err(e) => {
            crate::term::warn(&format!("Enroute map: {e:#}"));
            None
        }
    })
    .as_ref()
}

/// The coastlines alone, for what colour a plain tile is.
fn ground() -> &'static Ground {
    static GROUND: OnceLock<Ground> = OnceLock::new();
    GROUND.get_or_init(|| Ground::load(&crate::sources::http::Http::new(120, 0), &crate::cache::Cache::for_index(false)))
}

/// A plain tile's picture, by day or night, made once.
fn plain_picture(style: &str, plain: Plain, night: bool) -> Result<Vec<u8>> {
    static MADE: OnceLock<Mutex<HashMap<(String, Plain, bool), Vec<u8>>>> = OnceLock::new();
    let made = MADE.get_or_init(Default::default);
    let key = (style.to_string(), plain, night);
    if let Some(png) = made.lock().unwrap().get(&key) {
        return Ok(png.clone());
    }
    let day = enroute::plain_tile(style, plain)?;
    let png = if night { crate::output::canvas::png_to_night(&day)? } else { day };
    made.lock().unwrap().insert(key, png.clone());
    Ok(png)
}

/// The cycles tiles are kept for, newest first.
fn cycles(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.len() == 4 && n.bytes().all(|b| b.is_ascii_digit())).collect()).unwrap_or_default();
    out.sort();
    out.reverse();
    out
}

/// The set an aircraft on `own` is shown, `kept` newest first and `current` the cycle of
/// the simulator's own data: its own cycle's, else the newest not the simulator's (drawn
/// from an aircraft's database, nearer its own data), else the newest.
fn pick(kept: &[String], own: Option<&str>, current: Option<&str>) -> Option<String> {
    if let Some(c) = own.filter(|c| kept.iter().any(|k| k == c)) {
        return Some(c.to_string());
    }
    kept.iter().find(|k| Some(k.as_str()) != current).or(kept.first()).cloned()
}

/// One tile as a PNG. `rest` is what follows `/enroute/` in the address.
pub fn tile(root: &Path, rest: &str, aircraft: Option<&str>) -> Result<Vec<u8>> {
    // PMDG's tablet is pointed at `/enroute/app-domain` (see `patcher`): the same tiles.
    let rest = rest.trim_matches('/');
    let parts: Vec<&str> = rest.strip_prefix("app-domain/").unwrap_or(rest).split('/').collect();
    let ["styles", style, z, x, y] = parts[..] else { return Err(anyhow!("not an enroute tile: {rest}")) };
    let (base, night) = match style.strip_suffix(".night") {
        Some(b) => (b, true),
        None => (style.strip_suffix(".day").unwrap_or(style), false),
    };
    if !enroute::STYLES.contains(&base) {
        return Err(anyhow!("{style} is not an enroute style"));
    }
    let y = y.split(['@', '.']).next().unwrap_or("");
    let (z, x, y): (u8, u32, u32) = (z.parse()?, x.parse()?, y.parse()?);
    if z > enroute::DEEPEST || x >= 1 << z || y >= 1 << z {
        return Err(anyhow!("no tile {z}/{x}/{y}"));
    }
    let current = crate::sources::navdata::sim_cycle();
    let own = aircraft.and_then(crate::bridge::filed::aircraft_cycle);
    let chosen = pick(&cycles(root), own.as_deref(), current.as_deref());
    let meta = chosen.as_ref().and_then(|c| Meta::read(&root.join(c)));
    if let Some(c) = &chosen {
        if let Some(day) = enroute::read_tile(root, c, base, z, x, y) {
            // Kept as WebP or PNG; a flight bag is handed PNG.
            return if night { crate::output::canvas::png_to_night(&day) } else { crate::output::canvas::as_png(&day) };
        }
        // Not kept: plain, where it is within what was drawn or under a tile that was plain.
        if let Some(deepest) = meta.as_ref().and_then(|m| m.max_zoom.get(base).copied()) {
            let dz = z.saturating_sub(deepest);
            if dz == 0 || !enroute::has_tile(root, c, base, deepest, x >> dz, y >> dz) {
                return plain_picture(base, ground().plain_tile_at(z, x, y), night);
            }
        }
    }
    // Drawn now, out of what that set was drawn from, and kept.
    let source = meta.as_ref().map(Meta::source).unwrap_or(Source::Database);
    let d = data(source).ok_or_else(|| anyhow!("no navigation data to draw the enroute map from"))?;
    let png = enroute::tile(d, base, z, x, y)?;
    let path: PathBuf = enroute::tile_path(root, &d.cycle, base, z, x, y);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, &png);
    if night {
        crate::output::canvas::png_to_night(&png)
    } else {
        Ok(png)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_aircraft_is_shown_its_own_cycle_else_a_database_s() {
        let kept = vec!["2610".to_string(), "2503".to_string()];
        assert_eq!(pick(&kept, Some("2610"), Some("2610")).as_deref(), Some("2610"));
        assert_eq!(pick(&kept, Some("2503"), Some("2610")).as_deref(), Some("2503"));
        assert_eq!(pick(&kept, Some("2604"), Some("2610")).as_deref(), Some("2503"));
        assert_eq!(pick(&kept, None, Some("2610")).as_deref(), Some("2503"));
        assert_eq!(pick(&["2610".to_string()], Some("2503"), Some("2610")).as_deref(), Some("2610"));
        assert_eq!(pick(&[], Some("2503"), Some("2610")), None);
    }
}
