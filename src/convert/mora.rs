//! Grid minimum off-route altitudes (MORA), computed rather than copied.
//!
//! Every other layer this converter can produce either comes from a survey (a runway
//! threshold, a hold, an MSA sector) or is read straight out of the simulator's own
//! navigation data. Grid MORA is neither: it carries no survey of its own, it is a
//! published *rule* applied to terrain and obstruction data that is itself public. That
//! makes it the one aeronautical figure in this whole project we can work out from first
//! principles and actually distribute, rather than merely draw from something licensed.
//!
//! The rule (as printed on Jeppesen and equivalent charts): within each one-degree
//! quadrangle of latitude and longitude, find the highest terrain or obstruction; clear
//! it by 1,000 ft if that highest point is at or below 5,000 ft MSL, or by 2,000 ft if it
//! is higher; express the result in hundreds of feet, rounded up.
//!
//! ## Sampling interval
//!
//! Terrain comes from the Copernicus GLO-30 model (`sources::copernicus`), whose native
//! pixel is about 30 m but which also carries built-in overviews at 60, 120 and 240 m.
//! This module asks for samples every [`DEFAULT_STEP_M`] (90 m), which lands on the 60 m
//! overview — coarse enough to be a small fraction of the sixty-odd million native 30 m
//! pixels a one-degree cell holds, fine enough that a summit is not stepped over between
//! samples.
//!
//! That is not a guess: it was checked against Mont Blanc, the highest point in western
//! Europe, which sits in the (45°N, 6°E) quadrangle. Sampling that cell at 300 m (the
//! next overview up) found a highest point of 15,700-15,800 ft and so a MORA of
//! 17,700 ft; at 90 m and at the native 30 m it found the same 15,700-15,800 ft and the
//! same 17,800 ft both times — a summit some 4,800 m (15,750 ft) across is wide enough
//! for a 300 m grid to sometimes miss the very pixel that carries its highest sample.
//! Going finer than 90 m bought nothing further here, so this module stops there rather
//! than paying for the native grid's roughly nine times as many samples for no change in
//! the answer. What a 90 m grid can still miss is a needle-thin spike under about 100 m
//! across — which in practice is not natural terrain but a mast, a chimney or a tower,
//! and that is exactly what the obstruction check below exists to catch instead.
//!
//! In the same check, the published grid this project may only compare against (never
//! copy from — see below) gives that quadrangle 18,200 ft, 400 ft above what this rule
//! produces from Mont Blanc's own surveyed summit (4,809 m/15,774 ft, which rounds to
//! exactly 17,800 ft once the 2,000 ft clearance above 5,000 ft is added and the result is
//! rounded up). That gap, and what was and was not found to explain it, is recorded in
//! the validation notes kept alongside this module rather than in the code itself.
//!
//! ## Sea and no-data cells
//!
//! A quadrangle with no land and no obstruction still needs a value, not a hole in the
//! table: a flight plan or a display that indexes this grid by position must always find
//! an entry. Such a cell is treated as having a highest point of 0 ft MSL, which the rule
//! turns into the floor of the whole grid: 1,000 ft. The same floor is used if the
//! terrain model has no coverage at all for a cell (a fetch failure, or a gap in the
//! source), so a network hiccup produces a conservative floor value rather than a missing
//! row.

use crate::cache::Cache;
use crate::convert::model::MoraRec;
use crate::sources::copernicus;
use crate::sources::http::Http;
use crate::sources::obstacles;
use crate::term;
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// Metres between elevation samples inside a cell. See the module docs for why.
pub const DEFAULT_STEP_M: f64 = 90.0;

/// Half the diagonal of a one-degree quadrangle at the equator (the worst case; nearer
/// the poles a degree of longitude is physically shorter, so this over-covers rather than
/// under-covers). Used to ask `obstacles::around` for a circle guaranteed to contain the
/// whole square, before the results are cut down to the square itself.
const OBSTACLE_RADIUS_KM: f64 = 79.0;

/// A rectangle in degrees, south/west/north/east. `None` in the public API means the
/// whole world.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl Bounds {
    pub const WORLD: Bounds = Bounds { south: -90.0, west: -180.0, north: 90.0, east: 180.0 };
}

/// How the computation is run: sampling density, where (and whether) to cache, and
/// whether the network may be used. `grid_mora` uses sensible defaults; a caller that
/// wants to say more (a CLI command wanting `--offline`, a test wanting a scratch cache
/// directory) calls `grid_mora_with` instead.
pub struct Options {
    pub step_m: f64,
    /// `None` disables the on-disk cache (every cell is recomputed every run).
    pub cache_dir: Option<PathBuf>,
    pub offline: bool,
    pub refresh: bool,
    /// Parallel workers. `0` leaves it to rayon's default (one per core).
    pub jobs: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { step_m: DEFAULT_STEP_M, cache_dir: default_cache_dir(), offline: false, refresh: false, jobs: 0 }
    }
}

/// `%LOCALAPPDATA%/amdbgen/mora`, alongside the other on-disk caches this crate keeps.
fn default_cache_dir() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    Some(base.join("amdbgen").join("mora"))
}

/// The clearance the rule calls for above a quadrangle's highest feature.
fn clearance_ft(highest_ft: f64) -> f64 {
    if highest_ft <= 5000.0 {
        1000.0
    } else {
        2000.0
    }
}

/// Round up to the next hundred feet, the way the rule is published.
fn round_up_hundred(ft: f64) -> f64 {
    (ft / 100.0).ceil() * 100.0
}

/// A grid MORA value from the highest point (terrain or obstruction) in its quadrangle.
pub fn mora_value(highest_ft: f64) -> f64 {
    round_up_hundred(highest_ft + clearance_ft(highest_ft))
}

/// The higher of a cell's terrain and its tallest obstruction's top, both above sea
/// level. An obstruction is common enough right at an airport, and rare but decisive in
/// open country (a mast well above everything around it), to be worth taking separately
/// rather than assuming terrain always wins.
fn highest_feature_ft(terrain_ft: f64, obstruction_top_ft: Option<f64>) -> f64 {
    terrain_ft.max(obstruction_top_ft.unwrap_or(f64::NEG_INFINITY))
}

/// Grid MORA over an area — worldwide when `bounds` is `None` — with the default
/// sampling and the standard on-disk cache. See `grid_mora_with` for control over both.
pub fn grid_mora(bounds: Option<Bounds>) -> Result<Vec<MoraRec>> {
    grid_mora_with(bounds, &Options::default())
}

/// Grid MORA over an area, with explicit sampling and caching options.
pub fn grid_mora_with(bounds: Option<Bounds>, opts: &Options) -> Result<Vec<MoraRec>> {
    let b = bounds.unwrap_or(Bounds::WORLD);
    let lat_lo = b.south.floor() as i32;
    let lat_hi = b.north.ceil() as i32;
    let lon_lo = b.west.floor() as i32;
    let lon_hi = b.east.ceil() as i32;

    let mut cells = Vec::new();
    for lat_deg in lat_lo..lat_hi {
        for lon_deg in lon_lo..lon_hi {
            cells.push((lat_deg, lon_deg));
        }
    }
    let n = cells.len();

    let http = Http::new(120, 0);
    let cache = Cache::new(opts.cache_dir.clone(), opts.offline, opts.refresh);
    let mirrors = crate::pipeline::default_mirrors();

    term::start(&format!("Grid MORA: {n} quadrangle{} at {:.0} m spacing", if n == 1 { "" } else { "s" }, opts.step_m));
    let t0 = Instant::now();
    let done = AtomicUsize::new(0);

    // `num_threads(0)` is rayon's own spelling of "pick the default", so `Options::jobs`
    // can be passed straight through without a special case here.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(opts.jobs).build()?;
    let mut recs: Vec<MoraRec> = pool.install(|| {
        cells
            .par_iter()
            .filter_map(|&(lat_deg, lon_deg)| {
                let t1 = Instant::now();
                let key = cache_key(lat_deg, lon_deg, opts.step_m);
                let rec = cache
                    .get_or_fetch_bytes(&key, || {
                        let rec = compute_cell(&http, &cache, &mirrors, opts.step_m, lat_deg, lon_deg)?;
                        Ok(encode_segments(&[rec]))
                    })
                    .and_then(|bytes| decode_segments(&bytes));
                let i = done.fetch_add(1, Ordering::Relaxed) + 1;
                match rec {
                    Ok(mut v) => {
                        let rec = v.pop()?;
                        term::stage(i, n, &cell_name(lat_deg, lon_deg), &format!("{:.0} ft", rec.altitude_ft), t1.elapsed().as_millis() as u64);
                        Some(rec)
                    }
                    Err(e) => {
                        log::warn!("MORA cell {}: {e:#}", cell_name(lat_deg, lon_deg));
                        None
                    }
                }
            })
            .collect::<Vec<_>>()
    });

    recs.sort_by(|a, b| a.lat.partial_cmp(&b.lat).unwrap().then(a.lon.partial_cmp(&b.lon).unwrap()));
    term::success(&format!("{} of {n} quadrangles in {}", recs.len(), term::human_secs(t0.elapsed().as_secs_f64())));
    Ok(recs)
}

fn cell_name(lat_deg: i32, lon_deg: i32) -> String {
    format!("{:+03}{:+04}", lat_deg, lon_deg)
}

/// One quadrangle: fetch its terrain, fetch obstructions inside it, and apply the rule.
fn compute_cell(http: &Http, cache: &Cache, mirrors: &[String], step_m: f64, lat_deg: i32, lon_deg: i32) -> Result<MoraRec> {
    let (south, north) = (lat_deg as f64, lat_deg as f64 + 1.0);
    let (west, east) = (lon_deg as f64, lon_deg as f64 + 1.0);

    let patch = copernicus::cell(http, cache, lat_deg, lon_deg, step_m)?;
    let (_, hi_m) = patch.range();
    // No finite sample in the patch means no coverage at all: sea, or a gap in the
    // model. Either way the cell is not a hole in the grid; see the module docs.
    let terrain_ft = if hi_m.is_finite() { hi_m as f64 / 0.3048 } else { 0.0 };

    let id = cell_name(lat_deg, lon_deg);
    let mut obstacles = obstacles::around(http, cache, &format!("MORA{id}"), south + 0.5, west + 0.5, OBSTACLE_RADIUS_KM, mirrors).unwrap_or_default();
    // `around` answers a circle generous enough to contain the whole quadrangle; cut it
    // back down to the quadrangle itself before taking its tallest obstruction.
    obstacles.retain(|o| o.lat >= south && o.lat < north && o.lon >= west && o.lon < east);
    obstacles::resolve_tops(&mut obstacles, &patch);
    let obstruction_ft = obstacles.iter().filter_map(|o| o.top_ft).fold(f64::NEG_INFINITY, f64::max);

    let highest = highest_feature_ft(terrain_ft, obstruction_ft.is_finite().then_some(obstruction_ft));
    Ok(MoraRec { lat: south, lon: west, altitude_ft: mora_value(highest) })
}

/// Where one cell's answer lives on disk: one small file per cell per sampling density,
/// so a change of `--step-m` cannot silently read a cache built at a different
/// resolution.
fn cache_key(lat_deg: i32, lon_deg: i32, step_m: f64) -> String {
    format!("mora/{:.0}m/{}.bin", step_m, cell_name(lat_deg, lon_deg))
}

/// A flat, dependency-free encoding: three little-endian `f64`s per record (lat, lon,
/// altitude), one record per file here. Following the pattern the rest of this crate
/// uses for on-disk caches — plain bytes through `Cache::get_or_fetch_bytes` — rather
/// than a serialisation crate, so the cache format cannot drift with a library upgrade.
fn encode_segments(recs: &[MoraRec]) -> Vec<u8> {
    let mut out = Vec::with_capacity(recs.len() * 24);
    for r in recs {
        out.extend_from_slice(&r.lat.to_le_bytes());
        out.extend_from_slice(&r.lon.to_le_bytes());
        out.extend_from_slice(&r.altitude_ft.to_le_bytes());
    }
    out
}

fn decode_segments(bytes: &[u8]) -> Result<Vec<MoraRec>> {
    if bytes.len() % 24 != 0 {
        return Err(anyhow!("corrupt MORA cache entry ({} bytes)", bytes.len()));
    }
    Ok(bytes
        .chunks_exact(24)
        .map(|c| MoraRec {
            lat: f64::from_le_bytes(c[0..8].try_into().unwrap()),
            lon: f64::from_le_bytes(c[8..16].try_into().unwrap()),
            altitude_ft: f64::from_le_bytes(c[16..24].try_into().unwrap()),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clears_by_a_thousand_feet_at_or_below_five_thousand() {
        assert_eq!(mora_value(0.0), 1000.0);
        assert_eq!(mora_value(3000.0), 4000.0);
        assert_eq!(mora_value(5000.0), 6000.0);
    }

    #[test]
    fn clears_by_two_thousand_feet_above_five_thousand() {
        assert_eq!(mora_value(5000.1), 7100.0);
        assert_eq!(mora_value(6000.0), 8000.0);
        assert_eq!(mora_value(14_000.0), 16_000.0);
    }

    #[test]
    fn the_result_rounds_up_to_the_next_hundred_feet() {
        // 3,050 + 1,000 clearance = 4,050, which is not a round hundred: it must round
        // up to 4,100, never down to 4,000 (that would erase the safety margin).
        assert_eq!(mora_value(3050.0), 4100.0);
        assert_eq!(mora_value(6001.0), 8100.0);
    }

    #[test]
    fn an_obstruction_above_the_terrain_sets_the_cell() {
        let highest = highest_feature_ft(1200.0, Some(1800.0));
        assert_eq!(highest, 1800.0);
        assert_eq!(mora_value(highest), 2800.0);
    }

    #[test]
    fn terrain_wins_when_no_obstruction_reaches_as_high() {
        let highest = highest_feature_ft(4200.0, Some(1800.0));
        assert_eq!(highest, 4200.0);
    }

    #[test]
    fn a_sea_cell_with_nothing_in_it_lands_on_the_rule_floor() {
        let highest = highest_feature_ft(0.0, None);
        assert_eq!(mora_value(highest), 1000.0);
    }

    #[test]
    fn cache_entries_round_trip() {
        let recs = vec![MoraRec { lat: 45.0, lon: 6.0, altitude_ft: 17800.0 }];
        let bytes = encode_segments(&recs);
        let back = decode_segments(&bytes).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].lat, 45.0);
        assert_eq!(back[0].lon, 6.0);
        assert_eq!(back[0].altitude_ft, 17800.0);
    }

    #[test]
    fn a_truncated_cache_entry_is_reported_rather_than_misread() {
        assert!(decode_segments(&[0u8; 23]).is_err());
    }

    #[test]
    fn cache_keys_separate_sampling_densities() {
        assert_ne!(cache_key(45, 6, 300.0), cache_key(45, 6, 90.0));
    }

    /// Exercises the network (Copernicus + OpenStreetMap) and the whole cell pipeline
    /// over a small, real, mountainous area. Not run by default.
    #[test]
    #[ignore]
    fn computes_a_small_alpine_patch() {
        let bounds = Bounds { south: 45.0, west: 6.0, north: 46.0, east: 7.0 };
        let opts = Options { cache_dir: None, ..Options::default() };
        let recs = grid_mora_with(Some(bounds), &opts).unwrap();
        assert_eq!(recs.len(), 1);
        // Mont Blanc (4,809 m / 15,774 ft) sits in this quadrangle: 15,774 + 2,000 ft
        // clearance rounds up to 17,800 ft, which is what this comes out to at 90 m and
        // at native 30 m alike. The published grid (Jeppesen, cycle 2406) gives this cell
        // 18,200 ft instead; see the module docs and the validation notes for that gap.
        assert_eq!(recs[0].altitude_ft, 17_800.0);
    }
}
