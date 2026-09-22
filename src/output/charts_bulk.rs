//! Approach charts for a lot of airports at once.
//!
//! One at a time, most of the work is not the chart: it is reading the navigation data to
//! find the airport, the obstacle file, the runway file and the beacons, all of which are
//! the same for every airport and all of which a fresh run does again. Doing a list in one
//! run reads each of them once, works on several airports at a time, and leaves the
//! terrain cache warm for the next airport along, which is usually a near neighbour.

use crate::minima::Approach;
use crate::sources::msfs::procedures::{self, Kind};
use anyhow::Result;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Options {
    /// Where the PDFs go.
    pub out_dir: PathBuf,
    /// How many airports to work on at once.
    pub jobs: usize,
    /// Which system minimum applies, where the procedure codes none of its own.
    pub kind: Approach,
    /// A chart for every runway rather than only the airport's fullest approach.
    pub every_runway: bool,
    /// Leave off the minimum safe altitude ring, which costs a second and much wider
    /// read of the terrain for every airport.
    pub no_msa: bool,
}

/// Draw charts for every airport named, and say how it went.
pub fn run(icaos: &[String], opts: &Options) -> Result<()> {
    std::fs::create_dir_all(&opts.out_dir)?;
    let started = std::time::Instant::now();
    crate::term::start(&format!("Approach charts for {} airports", icaos.len()));

    // Everything shared, read once.
    let http = crate::sources::http::Http::new(300, 0);
    let cache = crate::cache::Cache::for_index(false);
    let mut idx = crate::sources::index::AirportIndex::default();
    idx.load_ourairports_online(&http, &cache)?;
    let found = procedures::find_many(icaos)?;
    crate::term::info(&format!("{} of {} airports have procedures in the navigation data", found.len(), icaos.len()));

    let done = AtomicUsize::new(0);
    let drawn = AtomicUsize::new(0);
    let total = icaos.len();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(opts.jobs.max(1)).build()?;
    let failures: Vec<String> = pool.install(|| {
        icaos
            .par_iter()
            .flat_map(|icao| {
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                let icao = icao.to_uppercase();
                let Some(procedures) = found.get(&icao) else {
                    return vec![format!("{icao}: no procedures")];
                };
                let runways: Vec<Option<String>> = if opts.every_runway {
                    let mut seen: Vec<String> = procedures.procedures.iter().filter(|p| p.kind == Kind::Approach).map(|p| p.runway.clone()).collect();
                    seen.sort();
                    seen.dedup();
                    seen.into_iter().map(Some).collect()
                } else {
                    vec![None]
                };
                let mut problems = Vec::new();
                for runway in runways {
                    match one(procedures.clone(), runway.as_deref(), &http, &cache, &idx, opts) {
                        Ok(path) => {
                            let count = drawn.fetch_add(1, Ordering::Relaxed) + 1;
                            crate::term::info(&format!("[{n}/{total}] {}", path.file_name().unwrap_or_default().to_string_lossy()));
                            let _ = count;
                        }
                        Err(e) => problems.push(format!("{icao} {}: {e:#}", runway.unwrap_or_default())),
                    }
                }
                problems
            })
            .collect()
    });

    let seconds = started.elapsed().as_secs_f64();
    let charts = drawn.load(Ordering::Relaxed);
    crate::term::success(&format!(
        "{charts} charts in {} ({:.0} an hour), {} could not be drawn",
        crate::term::human_secs(seconds),
        charts as f64 / seconds.max(0.001) * 3600.0,
        failures.len()
    ));
    for problem in failures.iter().take(20) {
        crate::term::warn(problem);
    }
    crate::term::file(None, &opts.out_dir.display().to_string(), "charts");
    Ok(())
}

/// One chart.
fn one(
    procedures: procedures::AirportProcedures,
    runway: Option<&str>,
    http: &crate::sources::http::Http,
    cache: &crate::cache::Cache,
    idx: &crate::sources::index::AirportIndex,
    opts: &Options,
) -> Result<PathBuf> {
    let setup = crate::approach::prepare_from(procedures, http, cache, idx, runway, crate::approach::Options { quiet: true, wide_terrain: !opts.no_msa, ..Default::default() })?;
    let procedure = setup.procedure();
    let est = crate::approach::estimate(&setup, opts.kind);
    let out = opts.out_dir.join(format!("{}-RW{}.pdf", setup.procedures.icao, procedure.runway));
    let chart = crate::output::approach::Chart {
        airport: &setup.procedures,
        airport_name: setup.airport_name.as_deref(),
        procedure,
        star: None,
        patch: &setup.patch,
        wide: setup.wide.as_ref(),
        obstacles: &setup.obstacles,
        threshold: setup.threshold.map(|(lat, lon, _)| (lat, lon)),
        tdze_ft: setup.tdze_ft,
        tdze_surveyed: setup.tdze_surveyed,
        field_elev_ft: setup.field_elev_ft,
        msa_ft: setup.msa_ft,
        msa_sectors: &setup.msa_sectors,
        track_deg: setup.track_deg(),
        course_mag_deg: setup.course_mag_deg(),
        variation_deg: setup.variation_deg(),
        kind: opts.kind,
        airport_dir: setup.airport_dir.as_deref(),
        runway_ends: setup.runway_ends,
    };
    crate::output::approach::write(&chart, &est, &out)?;
    Ok(out)
}

/// The airports named on the command line, or one per line of a file.
pub fn airports(args: &[String], list: Option<&Path>) -> Result<Vec<String>> {
    let mut out: Vec<String> = args.iter().map(|a| a.to_uppercase()).collect();
    if let Some(path) = list {
        let text = std::fs::read_to_string(path)?;
        for line in text.lines() {
            // One per line, or the first column of a CSV.
            let icao = line.split(',').next().unwrap_or("").trim().to_uppercase();
            if icao.len() == 4 && icao.chars().all(|c| c.is_ascii_alphanumeric()) {
                out.push(icao);
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}
