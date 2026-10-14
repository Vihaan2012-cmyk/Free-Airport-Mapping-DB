//! Charts for every airport a navigation data set has procedures for, drawn as the FAA's
//! are (`cifp_charts`): each approach on a page of its own, the departures and arrivals a
//! page a route, into `<out>/<ICAO>/`. The procedures come from the simulator's own
//! navigation data or from an aircraft's navigation database.
//!
//! Both are licensed data, unlike the CIFP, so these charts are for the machine they are
//! drawn on. They are kept out of the built airports' folders, which hold only what can
//! be passed on.

use crate::sources::msfs::procedures::{self, AirportProcedures, Kind};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub enum From {
    /// The simulator's own navigation data.
    Simulator,
    /// An aircraft's navigation database (`.s3db`).
    Database(PathBuf),
}

pub struct Options {
    pub from: From,
    /// The built airports: only those are drawn, since a chart draws the airport from them.
    pub airports: PathBuf,
    /// Where the charts go, a folder an airport: `<out>/<ICAO>/`.
    pub out: Option<PathBuf>,
    /// Or in each built airport's own folder, under this name: `<airports>/<ICAO>/<subfolder>/`.
    pub subfolder: Option<String>,
    /// Leave out the airports whose own folder already has charts (the FAA's).
    pub remaining: bool,
    /// Leave out the airports this run has finished before, from the same data: each
    /// airport's `cycle.txt` is written once its charts are.
    pub resume: bool,
    /// Only these airports; every one with procedures when empty.
    pub only: Vec<String>,
    /// Airports drawn at once.
    pub jobs: usize,
    /// Lowest CPU, disk and memory priority.
    pub background: bool,
    /// Leave off the minimum safe altitude ring, which reads terrain 25 miles round.
    pub no_msa: bool,
}

impl Options {
    /// The folder one airport's charts go in.
    fn dir(&self, icao: &str) -> PathBuf {
        match (&self.subfolder, &self.out) {
            (Some(name), _) => self.airports.join(icao).join(name),
            (None, Some(out)) => out.join(icao),
            (None, None) => self.airports.join(icao).join("charts-navdata"),
        }
    }
}

/// How many charts an airport gets: one an approach, one a departure or arrival page.
fn chart_count(a: &AirportProcedures) -> usize {
    a.procedures.iter().filter(|p| p.kind == Kind::Approach).count() + super::approach::terminal::groups(a).len()
}

pub fn run(opts: &Options) -> Result<()> {
    if opts.background {
        super::faa_charts::enter_background();
    }
    let only: HashSet<String> = opts.only.iter().map(|s| s.trim().to_uppercase()).collect();
    // The folder an airport is built under, which for an American one without an ICAO code
    // is its FAA identifier with a K in front.
    let folder = |icao: &str| crate::sources::cifp::spellings(icao).into_iter().find(|s| (only.is_empty() || only.contains(s)) && opts.airports.join(s).join("manifest.json").is_file());
    let charted = |folder: &str| {
        crate::output::chart_pack::names(&opts.airports.join(folder), "charts").iter().any(|n| n.to_ascii_lowercase().ends_with(".pdf"))
    };
    let wanted = |icao: &str| folder(icao).filter(|f| !(opts.remaining && charted(f)));

    let (all, from): (Vec<AirportProcedures>, String) = match &opts.from {
        From::Database(path) => {
            let (name, cycle) = crate::sources::navdata::use_database(path).ok_or_else(|| anyhow!("{} is not a navigation database", path.display()))?;
            let cycle = cycle.unwrap_or_default();
            let label = format!("{name} database AIRAC {cycle}");
            crate::term::step(None, &format!("Reading the procedures in {label} ({})", path.display()));
            let read = crate::sources::dfd::read(path, &label)?;
            (read.into_values().collect(), label)
        }
        From::Simulator => {
            let start = crate::sources::msfs::airac_dates().and_then(|(from, _)| chrono::NaiveDate::parse_from_str(&from, "%d %b %Y").ok());
            let cycle = start.map(crate::sources::dtpp::cycle).unwrap_or_default();
            let label = format!("MSFS AIRAC {cycle}");
            crate::term::step(None, &format!("Finding the airports in the simulator's navigation data, AIRAC {cycle}"));
            let icaos: Vec<String> = procedures::all_icaos().into_iter().filter(|i| wanted(i).is_some()).collect();
            crate::term::step(None, &format!("Reading the procedures of {} airports", icaos.len()));
            let read = procedures::find_many(&icaos)?;
            (read.into_values().map(|a| AirportProcedures { source: label.clone(), ..a }).collect(), label)
        }
    };
    let found = all.len();
    let mut list: Vec<AirportProcedures> = all.into_iter().filter_map(|a| wanted(&a.icao).map(|f| AirportProcedures { icao: f, ..a })).filter(|a| chart_count(a) > 0).collect();
    list.sort_by(|a, b| a.icao.cmp(&b.icao));
    if opts.resume {
        let before = list.len();
        // Finished from the same data, and every chart with the georeference beside it that
        // puts the aircraft on it in a flight bag (charts drawn before that was written are
        // drawn again).
        // A set may be packed (see `chart_pack`): its files are read as the folder's were.
        let names = |dir: &std::path::Path| match (dir.parent(), dir.file_name().and_then(|n| n.to_str())) {
            (Some(airport), Some(set)) => crate::output::chart_pack::names(airport, set),
            _ => Vec::new(),
        };
        let georeferenced = |dir: &std::path::Path| {
            let all = names(dir);
            all.iter().filter_map(|n| n.strip_suffix(".pdf")).all(|stem| all.iter().any(|n| *n == format!("{stem}.geo.json")))
        };
        let done_before = |icao: &str| {
            crate::output::chart_pack::read(&opts.dir(icao).join("cycle.txt")).is_some_and(|t| String::from_utf8_lossy(&t).contains(&format!("from {from}"))) && georeferenced(&opts.dir(icao))
        };
        list.retain(|a| !done_before(&a.icao));
        crate::term::info(&format!("{} airports already drawn from {from} are left as they are", before - list.len()));
    }
    let charts_total: usize = list.iter().map(chart_count).sum();
    crate::term::start(&format!(
        "Charts from {from}: {charts_total} charts at {} airports, into {} ({} airports with procedures left out: not built{})",
        list.len(),
        opts.dir("<ICAO>").display(),
        found.saturating_sub(list.len()),
        if opts.remaining { " or already charted" } else { "" }
    ));
    let dates = crate::sources::navdata::cycle_dates().map(|(a, b)| format!(" (in force {a} to {b})")).unwrap_or_default();

    let http = crate::sources::http::Http::new(120, 0);
    let cache = crate::cache::Cache::for_index(false);
    let mut idx = crate::sources::index::AirportIndex::default();
    idx.load_ourairports_online(&http, &cache)?;
    let bulk = super::charts_bulk::Options { out_dir: opts.airports.clone(), jobs: 1, kind: None, every_runway: false, no_msa: opts.no_msa };
    let (done, drawn, attempted, failed) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let total = list.len();
    let t0 = std::time::Instant::now();
    // The airports whose ground is already on disk first, then the rest: on a slow line a
    // run otherwise sits waiting on terrain while charts it could draw now wait behind.
    let (ready, later): (Vec<AirportProcedures>, Vec<AirportProcedures>) = list.into_iter().partition(|a| crate::sources::copernicus::has_square(&cache, a.lat, a.lon));
    crate::term::info(&format!("{} airports have their terrain on disk and go first; {} need it downloaded", ready.len(), later.len()));
    let pool = rayon::ThreadPoolBuilder::new().num_threads(opts.jobs.max(1)).build()?;
    // Airports drawn with ground missing, because the terrain would not download: not marked
    // done, so they are drawn again, once at the end of this run and by any run resumed.
    let short_of_ground: std::sync::Mutex<Vec<AirportProcedures>> = std::sync::Mutex::new(Vec::new());
    let draw = |a: &AirportProcedures| {
        let dir = opts.dir(&a.icao);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            crate::term::warn(&format!("[{}] {e:#}", a.icao));
            return;
        }
        crate::sources::copernicus::take_missed();
        let (drawn_here, problems) = super::cifp_charts::draw_airport(a, &dir, &http, &cache, &idx, &bulk);
        let missed = crate::sources::copernicus::take_missed();
        if missed == 0 {
            let note = format!(
                "Drawn by amdbgen from {from}{dates}.
The procedures are licensed navigation data: these charts are for this computer, not to be passed on.
The minima are worked out from terrain and obstacles, not the published ones.
NOT FOR REAL-WORLD NAVIGATION.
"
            );
            let _ = std::fs::write(dir.join("cycle.txt"), note);
        } else {
            let _ = std::fs::remove_file(dir.join("cycle.txt"));
            short_of_ground.lock().unwrap().push(a.clone());
        }
        drawn.fetch_add(drawn_here, Ordering::Relaxed);
        failed.fetch_add(problems.len(), Ordering::Relaxed);
        let so_far = attempted.fetch_add(drawn_here + problems.len(), Ordering::Relaxed) + drawn_here + problems.len();
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        // Time left at the rate so far, by charts rather than airports: an airport can
        // have one chart or eighty.
        let secs = t0.elapsed().as_secs_f64();
        let left = if so_far > 0 { secs / so_far as f64 * charts_total.saturating_sub(so_far) as f64 } else { 0.0 };
        crate::term::info(&format!(
            "[{n}/{total}] {}: {} charts{}{} | {so_far}/{charts_total} charts, {:.0} an hour, about {} left",
            a.icao,
            drawn_here,
            if problems.is_empty() { String::new() } else { format!(", {} could not be drawn ({})", problems.len(), problems.first().cloned().unwrap_or_default()) },
            if missed == 0 { String::new() } else { format!(", {missed} pieces of terrain would not download: to be drawn again") },
            so_far as f64 / secs.max(0.001) * 3600.0,
            crate::term::human_secs(left)
        ));
    };
    for part in [ready, later] {
        pool.install(|| part.par_iter().for_each(&draw));
    }
    let again = std::mem::take(&mut *short_of_ground.lock().unwrap());
    if !again.is_empty() {
        crate::term::start(&format!("Drawing again the {} airports whose terrain did not all download", again.len()));
        pool.install(|| again.par_iter().for_each(&draw));
        let still = short_of_ground.lock().unwrap().len();
        if still > 0 {
            crate::term::warn(&format!("{still} airports are still short of terrain; run again with --resume to draw them once it downloads"));
        }
    }
    crate::term::success(&format!(
        "{} charts at {total} airports in {}, {} could not be drawn",
        drawn.load(Ordering::Relaxed),
        crate::term::human_secs(t0.elapsed().as_secs_f64()),
        failed.load(Ordering::Relaxed)
    ));
    Ok(())
}
