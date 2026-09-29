//! Charts for every American airport with published procedures, drawn from the FAA's
//! CIFP (see `sources::cifp`): each approach, departure and arrival, into the airport's own
//! `charts` folder beside its layers and its airport diagram.
//!
//! The procedures are the FAA's; the drawing is ours, the same as the flight bag charts
//! drawn from the simulator's data. Nothing of the FAA's printed charts is fetched: the
//! minima are worked out from the terrain and the obstacles, not read off the FAA's page.
//! The navaids, holds and safe altitudes a chart prints beside the procedure come from the
//! simulator's navigation data on this computer, where it has them.

use crate::sources::msfs::procedures::{AirportProcedures, Kind};
use anyhow::Result;
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Options {
    /// Built airports: each one's charts go into `<airports>/<ICAO>/charts/`.
    pub airports: PathBuf,
    /// The cycle ("2609"); the one in force today when left out.
    pub cycle: Option<String>,
    /// Airports drawn at once.
    pub jobs: usize,
    /// Only these airports; every American airport with procedures when empty.
    pub only: Vec<String>,
    /// Lowest CPU, disk and memory priority.
    pub background: bool,
    /// Leave off the minimum safe altitude ring, which reads terrain 25 miles round.
    pub no_msa: bool,
}

/// A procedure's name as a file name.
fn file_name(s: &str) -> String {
    let clean: String = s.chars().map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '-' } else { c }).collect();
    clean.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn run(opts: &Options) -> Result<()> {
    if opts.background {
        super::faa_charts::enter_background();
    }
    let http = crate::sources::http::Http::new(120, 0);
    let cache = crate::cache::Cache::for_index(false);
    let cycle = opts.cycle.clone().unwrap_or_else(|| crate::sources::dtpp::cycle(chrono::Utc::now().date_naive()));
    crate::term::step(None, &format!("Reading the FAA's CIFP for cycle {cycle}"));
    let all = crate::sources::cifp::parse(&crate::sources::cifp::file(&http, &cache, &cycle)?);
    let mut idx = crate::sources::index::AirportIndex::default();
    idx.load_ourairports_online(&http, &cache)?;

    let only: Vec<String> = opts.only.iter().map(|s| s.trim().to_uppercase()).collect();
    let mut list: Vec<&AirportProcedures> = all.values().filter(|a| only.is_empty() || only.contains(&a.icao)).filter(|a| opts.airports.join(&a.icao).join("manifest.json").is_file()).collect();
    list.sort_by(|a, b| a.icao.cmp(&b.icao));
    let unbuilt = all.values().filter(|a| only.is_empty() || only.contains(&a.icao)).count() - list.len();
    let in_force = crate::sources::cifp::cycle_start(&cycle).map(|d| format!(" (in force from {})", d.format("%-d %b %Y"))).unwrap_or_default();
    crate::term::start(&format!(
        "Charts from the FAA's CIFP, cycle {cycle}{in_force}: {} airports{}",
        list.len(),
        if unbuilt > 0 { format!(" ({unbuilt} with procedures are not among the built airports)") } else { String::new() }
    ));

    let bulk = super::charts_bulk::Options { out_dir: opts.airports.clone(), jobs: 1, kind: None, every_runway: false, no_msa: opts.no_msa };
    let (done, drawn, failed) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let total = list.len();
    let t0 = std::time::Instant::now();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(opts.jobs.max(1)).build()?;
    pool.install(|| {
        list.par_iter().for_each(|a| {
            let dir = opts.airports.join(&a.icao).join("charts");
            if let Err(e) = std::fs::create_dir_all(&dir) {
                crate::term::warn(&format!("[{}] {e:#}", a.icao));
                return;
            }
            let mut wanted: HashSet<String> = HashSet::new();
            let mut problems = Vec::new();
            // Each approach on its own page: handed over alone, it is the one drawn.
            for p in a.procedures.iter().filter(|p| p.kind == Kind::Approach) {
                let name = format!("{}.pdf", file_name(&p.name));
                let one = AirportProcedures { procedures: vec![p.clone()], ..(*a).clone() };
                match super::charts_bulk::draw(one, None, &http, &cache, &idx, &bulk, false, Some(dir.join(&name))) {
                    Ok(_) => {
                        wanted.insert(name);
                    }
                    Err(e) => problems.push(format!("{}: {e:#}", p.name)),
                }
            }
            // Departures and arrivals, a file for each page: a chart puts the procedures that
            // share their routes on one page, named for all of them.
            for group in super::approach::terminal::groups(a) {
                let names: Vec<&str> = group.iter().map(|p| p.name.as_str()).collect();
                let name = format!("{} {}.pdf", if group[0].kind == Kind::Sid { "SID" } else { "STAR" }, file_name(&names.join(" - ")));
                let out = dir.join(&name);
                match super::approach::terminal::with_terminal_from((*a).clone(), names[0], |t| super::approach::terminal::write(t, &out)) {
                    Ok(()) => {
                        wanted.insert(name);
                    }
                    Err(e) => problems.push(format!("{}: {e:#}", names.join(" - "))),
                }
            }
            // The folder is ours: what an earlier cycle had and this one does not, goes.
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().to_string();
                    if n.to_ascii_lowercase().ends_with(".pdf") && !wanted.contains(&n) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            let note = format!(
                "Drawn by amdbgen from the FAA's CIFP, cycle {cycle}{in_force}.\r\nThe procedures are the FAA's; the minima are worked out from terrain and obstacles, not the published ones.\r\nNOT FOR REAL-WORLD NAVIGATION.\r\n"
            );
            let _ = std::fs::write(dir.join("cycle.txt"), note);
            drawn.fetch_add(wanted.len(), Ordering::Relaxed);
            failed.fetch_add(problems.len(), Ordering::Relaxed);
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            crate::term::info(&format!("[{n}/{total}] {}: {} charts{}", a.icao, wanted.len(), if problems.is_empty() { String::new() } else { format!(", {} could not be drawn ({})", problems.len(), problems.first().cloned().unwrap_or_default()) }));
        });
    });
    crate::term::success(&format!(
        "{} charts at {total} airports in {}, {} could not be drawn",
        drawn.load(Ordering::Relaxed),
        crate::term::human_secs(t0.elapsed().as_secs_f64()),
        failed.load(Ordering::Relaxed)
    ));
    Ok(())
}
