//! `amdb-bridge` command line.
//!
//! `serve` is the whole integration: it redirects `amdb.api.navigraph.com` to this
//! machine through the hosts file, answers over HTTPS with a locally trusted
//! certificate, and removes the redirect again when it stops.

use super::settings::Settings;
use super::store::{Retention, Store};
use super::{hosts, patcher, server, tls};
use crate::build::BuildOptions;
use crate::cache::Cache;
use crate::output::{Formats, Projection};
use crate::pipeline::{Config, FaaMode, OsmMode};
use crate::sources::http::Http;
use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "amdb-bridge", version, about = "Serve amdbgen airport data to aircraft in place of the Navigraph AMDB API")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct DataArgs {
    /// Directory with generated airports (<ICAO>/...). Default: the cache folder chosen at first run.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Keep nothing on disk for this run (overrides the saved setting).
    #[arg(long = "no-cache")]
    no_cache: bool,
    /// X-Plane install used for airports the Gateway does not have (auto-detected when omitted).
    #[arg(long = "xplane-dir")]
    xplane_dir: Option<PathBuf>,
    /// Optional X-Plane earth_aptmeta.dat index.
    #[arg(long)]
    aptmeta: Option<PathBuf>,
    /// Optional on-disk download cache.
    #[arg(long)]
    cache: Option<PathBuf>,
    /// Seconds to wait on one source before moving to the next (default 120).
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,
}

/// Build whole regions ahead of time.
#[derive(Args, Clone, Default)]
struct BulkArgs {
    /// Areas to build: continents (asia, europe, north-america, south-america, africa, oceania),
    /// ISO countries (DE, IN), or `all`; comma separated. `serve` does this in the background.
    #[arg(long, value_name = "AREAS")]
    bulk: Option<String>,
    /// Bulk filter: airport kinds, comma separated (large, medium, small).
    #[arg(long = "type", default_value = "large,medium", value_name = "KINDS")]
    kinds: String,
    /// Bulk filter: at least this many open runways.
    #[arg(long = "min-runways", default_value_t = 1, value_name = "N")]
    min_runways: usize,
    /// Bulk filter: longest runway at least this long (feet).
    #[arg(long = "min-runway-ft", value_name = "FEET")]
    min_runway_ft: Option<f64>,
    /// Bulk: rebuild airports that already exist instead of skipping them.
    #[arg(long = "rebuild")]
    rebuild: bool,
    /// Bulk: ICAO codes from a text file or a CSV with an `icao` column (e.g. from `amdbgen list --csv`).
    #[arg(long = "from-file", value_name = "FILE")]
    from_file: Option<PathBuf>,
    /// Bulk: delete each airport's source downloads (OSM extract, Gateway scenery) as soon as it is built.
    #[arg(long = "discard-downloads")]
    discard_downloads: bool,
    /// Bulk: airports fetched at the same time. Each one starts on a different OSM source,
    /// so the default keeps the map API and every Overpass endpoint busy at once.
    #[arg(long = "jobs", short = 'j', default_value_t = 6, value_name = "N")]
    jobs: usize,
    /// Bulk: delete every generated airport, cached download and the old status file before starting.
    #[arg(long = "fresh")]
    fresh: bool,
    /// Bulk: OpenStreetMap source: both (alternate map API and Overpass, each the other's fallback), osmapi, or overpass.
    #[arg(long = "osm", default_value = "both", value_name = "MODE")]
    osm: String,
}

fn osm_mode(s: &str) -> Result<OsmMode> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "both" | "mixed" => OsmMode::Both,
        "overpass" => OsmMode::Overpass,
        "osmapi" | "api" | "osm" => OsmMode::OsmApi,
        other => return Err(anyhow!("--osm must be both, osmapi or overpass, not {other}")),
    })
}

/// Wipe the generated airports and downloads before a fresh bulk run.
fn wipe_cache(cfg: &Config) {
    let mut removed = 0usize;
    if let Ok(rd) = std::fs::read_dir(&cfg.out) {
        for e in rd.flatten() {
            let p = e.path();
            let ok = if p.is_dir() { std::fs::remove_dir_all(&p).is_ok() } else { std::fs::remove_file(&p).is_ok() };
            if ok {
                removed += 1;
            }
        }
    }
    if let Some(dl) = cfg.cache.root() {
        let _ = std::fs::remove_dir_all(dl);
        let _ = std::fs::create_dir_all(dl);
    }
    if let Some(parent) = cfg.out.parent() {
        let _ = std::fs::remove_file(parent.join("bulk-status.csv"));
    }
    crate::term::warn(&format!("--fresh: removed {removed} entries from {} and emptied the download cache", cfg.out.display()));
}

/// Remove cached downloads belonging to these airports (files named `<ICAO>.*` or `<ICAO>_*`).
fn discard_downloads(downloads: &Path, icaos: &[String]) -> u64 {
    fn walk(dir: &Path, icaos: &[String], freed: &mut u64) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, icaos, freed);
                continue;
            }
            let name = p.file_name().map(|s| s.to_string_lossy().to_uppercase()).unwrap_or_default();
            if icaos.iter().any(|i| name.starts_with(&format!("{i}.")) || name.starts_with(&format!("{i}_"))) {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                if std::fs::remove_file(&p).is_ok() {
                    *freed += len;
                }
            }
        }
    }
    let mut freed = 0;
    walk(downloads, icaos, &mut freed);
    freed
}

impl BulkArgs {
    fn active(&self) -> bool {
        self.bulk.is_some() || self.from_file.is_some()
    }

    fn label(&self) -> String {
        match (&self.bulk, &self.from_file) {
            (Some(b), Some(f)) => format!("{b} + {}", f.display()),
            (Some(b), None) => b.clone(),
            (None, Some(f)) => f.display().to_string(),
            (None, None) => String::new(),
        }
    }

    fn filter(&self) -> Result<crate::pipeline::Filter> {
        let mut f = crate::pipeline::Filter { min_runways: Some(self.min_runways), min_runway_ft: self.min_runway_ft, ..Default::default() };
        f.kinds = self.kinds.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
        for area in self.bulk.as_deref().unwrap_or("").split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if area.eq_ignore_ascii_case("all") || area.eq_ignore_ascii_case("world") {
                f.all = true;
            } else if let Some(code) = crate::pipeline::continent_code(area) {
                f.continents.push(code.to_string());
            } else if area.len() == 2 && area.chars().all(|c| c.is_ascii_alphabetic()) {
                f.countries.push(area.to_uppercase());
            } else {
                return Err(anyhow!("--bulk: unknown area {area:?} (continent name, 2-letter country code, or all)"));
            }
        }
        // Continents and countries are a union, not an intersection.
        Ok(f)
    }

    /// The ICAO codes the bulk selection resolves to.
    fn resolve(&self, cfg: &Config) -> Result<Vec<String>> {
        let f = self.filter()?;
        let mut out = Vec::new();
        if let Some(p) = &self.from_file {
            out.extend(crate::pipeline::read_icao_file(p)?);
        }
        if f.all {
            out.extend(crate::pipeline::select(cfg, &[], &crate::pipeline::Filter { all: true, continents: Vec::new(), countries: Vec::new(), ..f.clone() })?);
        } else {
            if !f.continents.is_empty() {
                out.extend(crate::pipeline::select(cfg, &[], &crate::pipeline::Filter { countries: Vec::new(), ..f.clone() })?);
            }
            if !f.countries.is_empty() {
                out.extend(crate::pipeline::select(cfg, &[], &crate::pipeline::Filter { continents: Vec::new(), ..f.clone() })?);
            }
        }
        // File order is the build order (priority), so dedupe without sorting.
        let mut seen = std::collections::HashSet::new();
        out.retain(|i| seen.insert(i.clone()));
        Ok(out)
    }
}

/// One line of the bulk status report.
struct BulkRow {
    icao: String,
    status: &'static str,
    seconds: Option<f64>,
    error: String,
}

/// Write `bulk-status.csv` next to the airports folder: every airport of the run with
/// its outcome, the sources its manifest lists, feature count, build time and error.
fn write_bulk_status(cfg: &Config, rows: &[BulkRow], label: &str) -> Result<PathBuf> {
    let path = cfg.out.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| cfg.out.clone()).join("bulk-status.csv");
    let idx = crate::pipeline::load_index(cfg).ok();
    let mut w = csv::Writer::from_path(&path).with_context(|| format!("write {}", path.display()))?;
    w.write_record(["icao", "status", "name", "country", "sources", "features", "build_seconds", "error", "run"])?;
    for r in rows {
        let entry = idx.as_ref().and_then(|i| i.get(&r.icao));
        let manifest = std::fs::read_to_string(cfg.out.join(&r.icao).join("manifest.json")).ok().and_then(|t| serde_json::from_str::<crate::output::manifest::Manifest>(&t).ok());
        w.write_record([
            r.icao.clone(),
            r.status.to_string(),
            manifest.as_ref().and_then(|m| m.name.clone()).or_else(|| entry.and_then(|e| e.name.clone())).unwrap_or_default(),
            manifest.as_ref().and_then(|m| m.country.clone()).or_else(|| entry.and_then(|e| e.country.clone())).unwrap_or_default(),
            manifest.as_ref().map(|m| m.sources.join("+")).unwrap_or_default(),
            manifest.as_ref().map(|m| m.total_features().to_string()).unwrap_or_default(),
            r.seconds.map(|s| format!("{s:.1}")).unwrap_or_default(),
            r.error.clone(),
            label.to_string(),
        ])?;
    }
    w.flush()?;
    Ok(path)
}

/// Build a list of airports in chunks, skipping what already exists unless asked to rebuild.
fn run_bulk(cfg: &Config, icaos: &[String], rebuild: bool, label: &str, discard: Option<&Path>) {
    let todo: Vec<String> = icaos.iter().filter(|i| rebuild || !cfg.out.join(i).join("manifest.json").is_file()).cloned().collect();
    let skipped = icaos.len() - todo.len();
    let mut rows: Vec<BulkRow> = icaos.iter().filter(|i| !todo.contains(i)).map(|i| BulkRow { icao: i.clone(), status: "skipped (already built)", seconds: None, error: String::new() }).collect();
    crate::term::start(&format!("Bulk build {label}: {} airport(s){}", todo.len(), if skipped > 0 { format!(", {skipped} already built") } else { String::new() }));
    if todo.is_empty() {
        return;
    }
    let t0 = std::time::Instant::now();
    let (mut built, mut failed, mut freed) = (0usize, 0usize, 0u64);
    // Chunks of three OSM widths: Gateway fetches and builds of one chunk overlap with
    // the OSM waits of the same chunk.
    let chunk = (cfg.osm_parallel.max(1) * 3).max(6);
    for (n, part) in todo.chunks(chunk).enumerate() {
        match crate::pipeline::run(cfg, part) {
            Ok(s) => {
                built += s.built.len();
                failed += s.failed.len();
                for i in &s.built {
                    let secs = s.timings.iter().find(|(k, _)| k == i).map(|(_, t)| *t);
                    rows.push(BulkRow { icao: i.clone(), status: "built", seconds: secs, error: String::new() });
                }
                for (i, e) in &s.failed {
                    rows.push(BulkRow { icao: i.clone(), status: "failed", seconds: None, error: e.clone() });
                }
            }
            Err(e) => {
                crate::term::warn(&format!("bulk chunk failed: {e:#}"));
                failed += part.len();
                for i in part {
                    rows.push(BulkRow { icao: i.clone(), status: "failed", seconds: None, error: format!("{e:#}") });
                }
            }
        }
        if let Some(d) = discard {
            freed += discard_downloads(d, part);
        }
        let done = ((n + 1) * chunk).min(todo.len());
        let per = t0.elapsed().as_secs_f64() / done as f64;
        let eta = per * (todo.len() - done) as f64;
        crate::term::info(&format!("Bulk {label}: {done}/{} done ({built} built, {failed} failed), about {} left", todo.len(), crate::term::human_secs(eta)));
    }
    crate::term::success(&format!(
        "Bulk build {label} finished: {built} built, {failed} failed, {skipped} skipped, in {}{}",
        crate::term::human_secs(t0.elapsed().as_secs_f64()),
        if discard.is_some() { format!(", {} of downloads discarded", crate::term::human_bytes(freed)) } else { String::new() }
    ));
    rows.sort_by(|a, b| a.icao.cmp(&b.icao));
    match write_bulk_status(cfg, &rows, label) {
        Ok(p) => crate::term::file(None, &p.display().to_string(), &format!("status of all {} airports (built / failed / skipped, sources, features, errors)", rows.len())),
        Err(e) => crate::term::warn(&format!("could not write the status CSV: {e:#}")),
    }
}

#[derive(Args, Clone)]
struct ServeArgs {
    #[command(flatten)]
    data: DataArgs,
    #[command(flatten)]
    bulk: BulkArgs,
    /// HTTPS port for the redirected Navigraph host (the aircraft use 443).
    #[arg(long = "https-port", default_value_t = super::DEFAULT_HTTPS_PORT)]
    https_port: u16,
    /// Plain HTTP port for local tools; 0 disables it.
    #[arg(long = "http-port", default_value_t = super::DEFAULT_PORT)]
    http_port: u16,
    /// Do not touch the hosts file or the certificate store (HTTP/HTTPS only, no redirect).
    #[arg(long = "no-hosts")]
    no_hosts: bool,
    /// Do not patch the iniBuilds A350 EFB (its OANS then needs a Navigraph subscription).
    #[arg(long = "no-patch")]
    no_patch: bool,
    /// Also serve X-Plane 12: install the FlyWithLua moving-map script and build airports for it on demand.
    #[arg(long = "xplane")]
    xplane: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Redirect the Navigraph AMDB host to this machine and serve airports until stopped (Ctrl-C).
    Serve(ServeArgs),
    /// Build airports ahead of time so the first OANS load is instant.
    Prefetch {
        #[command(flatten)]
        data: DataArgs,
        #[command(flatten)]
        bulk: BulkArgs,
        icaos: Vec<String>,
        /// Also the airports of your latest SimBrief OFP (username or pilot id).
        #[arg(long)]
        simbrief: Option<String>,
    },
    /// Show redirect, certificate, storage and aircraft status.
    Status,
    /// Change the storage settings (cache on/off, folder, size limit) asked at first run.
    Setup,
    /// Remove the hosts-file redirect and the local certificate authority (cleanup after a crash).
    Cleanup,
    /// Add the server to the simulator's exe.xml so it starts with the sim.
    Autostart(ServeArgs),
    /// Alternative to the redirect: rewrite aircraft bundles to use http://127.0.0.1:PORT (backups kept).
    Patch {
        #[arg(long = "community")]
        community: Vec<PathBuf>,
        #[arg(long, default_value_t = super::DEFAULT_PORT)]
        port: u16,
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Restore bundles changed by `patch`.
    Unpatch {
        #[arg(long = "community")]
        community: Vec<PathBuf>,
    },
}

/// Saved settings (asking on the first run), with this run's overrides applied.
fn effective_settings(d: &DataArgs) -> Result<Settings> {
    let mut s = Settings::load_or_setup()?;
    if d.no_cache {
        s.cache = false;
    }
    Ok(s)
}

fn make_store(d: &DataArgs, s: &Settings) -> Result<Store> {
    let mut store = Store::new(config(d, s))?;
    store.retention = if d.out.is_some() {
        Retention::KeepAll
    } else if !s.cache {
        Retention::Ephemeral
    } else if s.limit_bytes().is_some() {
        Retention::Limit(s.clone())
    } else {
        Retention::KeepAll
    };
    Ok(store)
}

fn config(d: &DataArgs, s: &Settings) -> Config {
    Config {
        out: d.out.clone().unwrap_or_else(|| s.airports_dir()),
        cache: Cache::new(d.cache.clone().or_else(|| s.downloads_dir()), false, false),
        // A deadline per request, not per airport: public Overpass servers queue work
        // and can sit on a query for many minutes. Giving up at two minutes and moving
        // to the next source in the pool is far quicker than waiting one out.
        http: Http::new(d.timeout.unwrap_or(120), 250),
        formats: Formats { geojson: true, pbf: false },
        projection: Projection::Wgs84,
        xplane_dir: d.xplane_dir.clone().or_else(crate::sources::xplane::local::detect_install),
        aptdat_file: None,
        use_gateway: true,
        osm: OsmMode::OsmApi,
        overpass_mirrors: crate::pipeline::default_mirrors(),
        aptmeta: d.aptmeta.clone(),
        ourairports: true,
        faa: FaaMode::Auto,
        overrides_dir: PathBuf::from("overrides"),
        radius_km: 3.0,
        build: BuildOptions::default(),
        write_ir: false,
        layers: crate::model::ALL_LAYERS.to_vec(),
        index_cache: Cache::for_index(false),
        osm_parallel: 2,
        faa_amdb: true,
    }
}

/// Relaunch this command elevated (UAC prompt) and exit the current process.
fn relaunch_elevated() -> Result<()> {
    let exe = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).map(|a| format!("'{}'", a.replace('\'', "''"))).collect();
    let arg_list = if args.is_empty() { String::from("@()") } else { format!("@({})", args.join(",")) };
    let cmd = format!("Start-Process -FilePath '{}' -ArgumentList {} -Verb RunAs -WorkingDirectory '{}'", exe.display(), arg_list, std::env::current_dir()?.display());
    log::info!("administrator rights are needed for the hosts file; asking for elevation");
    let status = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &cmd]).status()?;
    if !status.success() {
        return Err(anyhow!("elevation was refused"));
    }
    std::process::exit(0);
}

fn serve(a: ServeArgs) -> Result<()> {
    let domain = super::NAVIGRAPH_AMDB_DOMAIN;
    // Ask the storage questions in the user's own window, before any elevation.
    let settings = effective_settings(&a.data)?;
    let mut https = None;
    if !a.no_hosts {
        if !hosts::writable() {
            relaunch_elevated()?;
        }
        let m = tls::ensure(domain)?;
        tls::trust(&m)?;
        hosts::install(domain)?;
        crate::term::success(&format!("Hosts file now sends {domain} here (removed automatically on exit)"));
        https = Some((a.https_port, m.cert_pem, m.key_pem));
        ctrlc::set_handler(move || {
            match hosts::remove() {
                Ok(_) => log::info!("hosts-file redirect removed"),
                Err(e) => log::error!("could not clean the hosts file: {e:#}"),
            }
            std::process::exit(0);
        })?;
    } else if a.https_port != super::DEFAULT_HTTPS_PORT || a.http_port == 0 {
        // Explicit HTTPS without the redirect (testing): still needs the certificate.
        let m = tls::ensure(domain)?;
        https = Some((a.https_port, m.cert_pem, m.key_pem));
    }
    if !a.no_patch {
        for d in communities(&[]) {
            match patcher::patch_a350(&d, false) {
                Ok(files) => {
                    for f in files {
                        crate::term::success(&format!("iniBuilds A350 EFB patched to hand its OANS a token (backup kept, `unpatch` restores): {}", f.path.display()));
                    }
                }
                Err(e) => crate::term::warn(&format!("could not patch the A350 EFB in {}: {e:#}", d.display())),
            }
            match patcher::patch_a220(&d, false) {
                Ok(files) => {
                    for f in files {
                        crate::term::success(&format!("GM5 A220 moving map patched to work with the bridge (backup kept, `unpatch` restores): {}", f.path.display()));
                    }
                }
                Err(e) => crate::term::warn(&format!("could not patch the A220 moving map in {}: {e:#}", d.display())),
            }
            for (pkg, _, _) in patcher::scan_a220(&d) {
                if let Some(w) = patcher::a220_load_order_warning(&d, &pkg) {
                    crate::term::warn(&w);
                }
            }
        }
    }
    crate::term::info(&format!("Storage: {}", if let Some(o) = &a.data.out { format!("airports in {} (kept, no limit)", o.display()) } else { settings.describe() }));
    let store = make_store(&a.data, &settings)?;
    if a.bulk.active() {
        // Resolve now (errors surface before the server starts), build in the background
        // once the server is up so aircraft are served meanwhile.
        let mut cfg = config(&a.data, &settings);
        cfg.osm_parallel = a.bulk.jobs.max(1);
        cfg.osm = osm_mode(&a.bulk.osm)?;
        if a.bulk.fresh {
            wipe_cache(&cfg);
        }
        let icaos = a.bulk.resolve(&cfg)?;
        let label = a.bulk.label();
        let rebuild = a.bulk.rebuild;
        let discard = if a.bulk.discard_downloads { a.data.cache.clone().or_else(|| settings.downloads_dir()) } else { None };
        if let Some(limit) = settings.limit_bytes() {
            let need = icaos.len() as u64 * 4 * 1024 * 1024;
            if a.data.out.is_none() && need > limit {
                crate::term::warn(&format!("Bulk {label} needs roughly {} but the cache limit is {} MB: older airports will be pruned as it goes (raise it with `setup`)", crate::term::human_bytes(need), settings.limit_mb));
            }
        }
        crate::term::info(&format!("Bulk {label}: {} airport(s) selected; building in the background", icaos.len()));
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(3));
            run_bulk(&cfg, &icaos, rebuild, &label, discard.as_deref());
        });
    }
    if a.xplane {
        let port = if a.http_port == 0 { super::DEFAULT_PORT } else { a.http_port };
        match crate::sources::xplane::local::detect_install() {
            Some(root) => match crate::output::xplane::install_script(&root, &format!("http://127.0.0.1:{port}")) {
                Ok(p) => crate::term::success(&format!("X-Plane moving map installed: {} - open it from Plugins > FlyWithLua > Macros > AMDB OANS", p.display())),
                Err(e) => crate::term::warn(&format!("could not install the X-Plane script: {e:#}")),
            },
            None => crate::term::warn("--xplane: no X-Plane 12 install found; the /xp/ route still works if you install tools/xplane/amdb_oans.lua yourself"),
        }
    }
    let result = server::serve(store, server::Listen { http_port: if a.http_port == 0 { None } else { Some(a.http_port) }, https });
    if !a.no_hosts {
        let _ = hosts::remove();
    }
    result
}

fn communities(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut v = patcher::detect_community_dirs();
    for e in extra {
        if !v.contains(e) {
            v.push(e.clone());
        }
    }
    v
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve(a) => serve(a),
        Cmd::Prefetch { data, bulk, icaos, simbrief } => {
            let settings = effective_settings(&data)?;
            let mut cfg = config(&data, &settings);
            cfg.osm_parallel = bulk.jobs.max(1);
            if bulk.active() {
                cfg.osm = osm_mode(&bulk.osm)?;
            }
            if bulk.fresh && bulk.active() {
                wipe_cache(&cfg);
            }
            let mut icaos = icaos;
            if bulk.active() {
                let label = bulk.label();
                let sel = bulk.resolve(&cfg)?;
                let discard = if bulk.discard_downloads { data.cache.clone().or_else(|| settings.downloads_dir()) } else { None };
                run_bulk(&cfg, &sel, bulk.rebuild, &label, discard.as_deref());
                if icaos.is_empty() && simbrief.is_none() {
                    return Ok(());
                }
            }
            if let Some(user) = &simbrief {
                let ofp = crate::sources::simbrief::fetch(&cfg.http, user)?;
                crate::term::info(&format!("SimBrief {}: {}", ofp.flight.clone().unwrap_or_else(|| user.clone()), ofp.icaos().join(" → ")));
                icaos.extend(ofp.icaos());
            }
            if icaos.is_empty() {
                return Err(anyhow!("give ICAO codes, --simbrief or --bulk AREA to prefetch"));
            }
            let store = make_store(&data, &settings)?;
            for i in icaos {
                match store.airport(&i) {
                    Ok(a) => println!("{}: {} features ready", a.icao, a.layers.values().map(Vec::len).sum::<usize>()),
                    Err(e) => println!("{}: FAILED {e:#}", i.to_uppercase()),
                }
            }
            Ok(())
        }
        Cmd::Status => {
            let domain = super::NAVIGRAPH_AMDB_DOMAIN;
            println!("hosts redirect for {domain}: {}", if hosts::is_installed(domain) { "ACTIVE" } else { "not installed" });
            println!("local CA trusted by Windows: {}", if tls::is_trusted() { "yes" } else { "no" });
            println!("certificate folder: {}", tls::data_dir().display());
            match Settings::load() {
                Some(s) => println!("storage: {}  (settings in {})", s.describe(), Settings::path().display()),
                None => println!("storage: not set up yet (the first `serve` asks)"),
            }
            for d in communities(&[]) {
                println!("Community: {}", d.display());
                for c in patcher::scan(&d) {
                    println!("  {}  references the Navigraph AMDB host ({} refs) -> covered by the redirect", c.package, c.literal_hits + c.template_hits);
                }
                for (pkg, f) in patcher::scan_wasm(&d) {
                    println!("  {}  WASM gauge {} uses the Navigraph AMDB host -> covered by the redirect", pkg, f.file_name().unwrap_or_default().to_string_lossy());
                }
                for (pkg, f, patched) in patcher::scan_a350(&d) {
                    println!("  {}  EFB token handler {}: {}", pkg, if patched { "PATCHED (OANS works without a Navigraph subscription)" } else { "not patched (run `serve` or `patch`)" }, f.file_name().unwrap_or_default().to_string_lossy());
                }
                for (pkg, f, patched) in patcher::scan_a220(&d) {
                    println!("  {}  A220 moving map {}: {}", pkg, if patched { "PATCHED (token fallback + bridge airport search)" } else { "not patched (run `serve` or `patch`)" }, f.file_name().unwrap_or_default().to_string_lossy());
                    if let Some(w) = patcher::a220_load_order_warning(&d, &pkg) {
                        println!("  WARNING {w}");
                    }
                }
                for f in patcher::load_record(&d).files {
                    println!("  PATCHED {}", f.path.display());
                }
            }
            Ok(())
        }
        Cmd::Setup => {
            let s = Settings::wizard(&Settings::load().unwrap_or_default())?;
            s.save()?;
            crate::term::success(&format!("Saved: {}  ({})", s.describe(), Settings::path().display()));
            Ok(())
        }
        Cmd::Cleanup => {
            if !hosts::writable() {
                relaunch_elevated()?;
            }
            println!("hosts redirect removed: {}", hosts::remove()?);
            println!("local CA removed from trust store: {}", tls::untrust()?);
            Ok(())
        }
        Cmd::Autostart(a) => {
            let exe = std::env::current_exe()?;
            let args = match &a.data.out {
                Some(o) => format!("serve --out \"{}\"", std::fs::canonicalize(o).unwrap_or(o.clone()).display()),
                None => "serve".to_string(),
            };
            let written = patcher::install_autostart(&exe, &args)?;
            if written.is_empty() {
                println!("no exe.xml found or entry already present");
            }
            for p in written {
                println!("added AMDB Bridge to {} (it will ask for administrator rights when the sim starts)", p.display());
            }
            Ok(())
        }
        Cmd::Patch { community, port, dry_run } => {
            let mut total = 0;
            for d in communities(&community) {
                total += patcher::patch(&d, port, dry_run)?.len();
                total += patcher::patch_a350(&d, dry_run)?.len();
                total += patcher::patch_a220(&d, dry_run)?.len();
            }
            println!("{}{} file(s) patched", if dry_run { "[dry-run] " } else { "" }, total);
            Ok(())
        }
        Cmd::Unpatch { community } => {
            let mut total = 0;
            for d in communities(&community) {
                total += patcher::unpatch(&d)?;
            }
            println!("{total} file(s) restored");
            Ok(())
        }
    }
}
