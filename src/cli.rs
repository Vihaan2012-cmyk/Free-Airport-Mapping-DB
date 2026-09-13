//! Command line interface.

use crate::build::BuildOptions;
use crate::cache::Cache;
use crate::output::manifest::{Index, Manifest};
use crate::output::preview::{self, Preview};
use crate::output::{Formats, Projection};
use crate::pipeline::{self, Config, FaaMode, Filter, OsmMode, Summary};
use crate::sources::http::Http;
use crate::term;
use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "amdbgen", version, about = "Build a Navigraph-style Airport Mapping Database (DO-272 layers) from free sources")]
struct Cli {
    /// Show debug output.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build AMDB layers for airports (GeoJSON + PBF per layer).
    Build(BuildArgs),
    /// Pre-download source data into the cache without building.
    Fetch(BuildArgs),
    /// Validate a generated airport directory (out/<ICAO>).
    Validate { dir: PathBuf },
    /// List the ICAO codes a selection resolves to (country, region, prefix, radius, box, type ...).
    List(SelectArgs),
    /// Show what the index knows about airports (name, IATA, position, elevation, runways, Gateway scenery).
    Info(SelectArgs),
    /// Search the airport index by name, city, IATA or ICAO.
    Search {
        text: String,
        /// Maximum results.
        #[arg(long, default_value_t = 25)]
        limit: usize,
        #[command(flatten)]
        index: IndexArgs,
    },
    /// Write a Jeppesen-style airport diagram as a PDF (<ICAO>/chart.pdf) for a built airport.
    Chart(PreviewArgs),
    /// Write an OANS-style moving-map preview (viewer.html) for a built airport.
    View(PreviewArgs),
    /// Per-layer feature counts, sources and file sizes of built airports.
    Stats {
        /// ICAO codes (looked up under --dir) or airport folders. Default: every airport in --dir.
        targets: Vec<String>,
        #[arg(long, default_value = "out")]
        dir: PathBuf,
    },
    /// Pack built airports into <dir>/<ICAO>.zip.
    Zip {
        icaos: Vec<String>,
        #[arg(long, default_value = "out")]
        dir: PathBuf,
        /// Every airport in --dir.
        #[arg(long)]
        all: bool,
    },
    /// Delete built airports from the output directory (and their entries in index.json).
    Clean {
        icaos: Vec<String>,
        #[arg(long, default_value = "out")]
        dir: PathBuf,
        /// Every airport in --dir.
        #[arg(long)]
        all: bool,
    },
    /// List the 45 DO-272 layers with geometry kind and map-profile membership.
    Layers,
    /// Print the legend for the numeric attribute codes (the contents of codes.json).
    Codes,
}

/// Where the airport index comes from.
#[derive(Args, Clone, Default)]
pub struct IndexArgs {
    /// X-Plane earth_aptmeta.dat (airport index with ARP/elevation/transition levels).
    #[arg(long)]
    aptmeta: Option<PathBuf>,
    /// Do not download the OurAirports index.
    #[arg(long = "no-ourairports")]
    no_ourairports: bool,
    /// Optional on-disk cache for downloads (off by default: every run refetches).
    #[arg(long)]
    cache: Option<PathBuf>,
    /// Never use the network (only meaningful with --cache).
    #[arg(long, requires = "cache")]
    offline: bool,
    /// Ignore cached downloads and refetch (only meaningful with --cache).
    #[arg(long, requires = "cache")]
    refresh: bool,
}

#[derive(Args, Clone, Default)]
pub struct SelectArgs {
    /// ICAO codes (e.g. EDDF VIDP KJFK).
    icaos: Vec<String>,
    /// ICAO codes from a text file: one per line or separated by spaces/commas, # starts a comment.
    #[arg(long = "from-file", value_name = "FILE")]
    from_file: Option<PathBuf>,
    /// Add the airports of your latest SimBrief OFP (username or pilot id).
    #[arg(long)]
    simbrief: Option<String>,
    /// All airports in an ISO country, e.g. IN, DE (repeatable as a comma list: DE,AT,CH).
    #[arg(long)]
    country: Option<String>,
    /// All airports whose aptmeta region starts with this (e.g. VI, K2).
    #[arg(long)]
    region: Option<String>,
    /// All airports whose ICAO starts with this prefix (e.g. ED, VA).
    #[arg(long = "icao-prefix")]
    prefix: Option<String>,
    /// Every airport in the index (worldwide).
    #[arg(long)]
    all: bool,
    /// Airports within --within km of a point, given as LAT,LON (e.g. 48.86,2.35).
    #[arg(long, value_name = "LAT,LON")]
    near: Option<String>,
    /// Radius for --near in km.
    #[arg(long, default_value_t = 100.0, value_name = "KM")]
    within: f64,
    /// Airports inside a box WEST,SOUTH,EAST,NORTH in degrees (e.g. 5.9,47.3,15.0,55.1).
    #[arg(long, value_name = "W,S,E,N")]
    bbox: Option<String>,
    /// Airport kinds, comma separated: large, medium, small, heliport, seaplane, closed.
    #[arg(long = "type", value_name = "KINDS")]
    kinds: Option<String>,
    /// Only airports with an open runway at least this long (feet, OurAirports data).
    #[arg(long = "min-runway-ft", value_name = "FEET")]
    min_runway_ft: Option<f64>,
    /// Only airports with at least this many open runways.
    #[arg(long = "min-runways", value_name = "N")]
    min_runways: Option<usize>,
    /// All airports on these continents: africa, antarctica, asia, europe, north-america, oceania, south-america (or AF/AN/AS/EU/NA/OC/SA), comma separated.
    #[arg(long, value_name = "NAMES")]
    continent: Option<String>,
    /// Select by IATA code(s), comma separated (e.g. CDG,ORY).
    #[arg(long)]
    iata: Option<String>,
    /// Airports whose name or city contains this text.
    #[arg(long)]
    search: Option<String>,
    /// Drop ICAO codes or prefixes from the selection, comma separated (e.g. EDDF,ET).
    #[arg(long)]
    exclude: Option<String>,
    /// Keep at most N airports of the index selection (explicit codes are always kept).
    #[arg(long)]
    limit: Option<usize>,
    /// Skip the first N airports of the index selection (paging for big batches).
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[command(flatten)]
    index: IndexArgs,
}

impl SelectArgs {
    fn filter(&self) -> Result<Filter> {
        let list = |s: &Option<String>| -> Vec<String> { s.as_deref().unwrap_or("").split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect() };
        let near = match &self.near {
            Some(s) => {
                let v: Vec<f64> = s.split(',').map(|x| x.trim().parse::<f64>()).collect::<std::result::Result<_, _>>().map_err(|_| anyhow!("--near expects LAT,LON, got {s}"))?;
                if v.len() != 2 || v[0].abs() > 90.0 || v[1].abs() > 180.0 {
                    return Err(anyhow!("--near expects LAT,LON in degrees, got {s}"));
                }
                Some((v[0], v[1], self.within))
            }
            None => None,
        };
        let bbox = match &self.bbox {
            Some(s) => {
                let v: Vec<f64> = s.split(',').map(|x| x.trim().parse::<f64>()).collect::<std::result::Result<_, _>>().map_err(|_| anyhow!("--bbox expects W,S,E,N, got {s}"))?;
                if v.len() != 4 || v[0] > v[2] || v[1] > v[3] {
                    return Err(anyhow!("--bbox expects WEST,SOUTH,EAST,NORTH with west<east and south<north, got {s}"));
                }
                Some([v[0], v[1], v[2], v[3]])
            }
            None => None,
        };
        let mut continents = Vec::new();
        for c in list(&self.continent) {
            continents.push(pipeline::continent_code(&c).ok_or_else(|| anyhow!("unknown continent {c} (africa, antarctica, asia, europe, north-america, oceania, south-america)"))?.to_string());
        }
        Ok(Filter {
            country: self.country.clone(),
            countries: Vec::new(),
            continents,
            region: self.region.clone(),
            prefix: self.prefix.clone(),
            all: self.all,
            near,
            bbox,
            kinds: list(&self.kinds),
            min_runway_ft: self.min_runway_ft,
            min_runways: self.min_runways,
            iata: list(&self.iata),
            search: self.search.clone(),
            exclude: list(&self.exclude),
            offset: self.offset,
            limit: self.limit,
        })
    }

    /// Explicit codes: the positional ones plus --from-file.
    fn explicit(&self) -> Result<Vec<String>> {
        let mut v: Vec<String> = self.icaos.clone();
        if let Some(p) = &self.from_file {
            let text = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
            for line in text.lines() {
                let line = line.split('#').next().unwrap_or("");
                v.extend(line.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|s| !s.is_empty()).map(str::to_string));
            }
        }
        Ok(v)
    }

    /// Resolve the selection against the index (and SimBrief when asked).
    fn resolve(&self, cfg: &Config) -> Result<Vec<String>> {
        let f = self.filter()?;
        let explicit = self.explicit()?;
        let mut icaos = if let Some(countries) = self.country.as_deref().filter(|c| c.contains(',')) {
            // Several countries: union of each.
            let mut all = Vec::new();
            for c in countries.split(',').map(str::trim).filter(|c| !c.is_empty()) {
                let f = Filter { country: Some(c.to_string()), ..f.clone() };
                all.extend(pipeline::select(cfg, &[], &f)?);
            }
            all.extend(pipeline::select(cfg, &explicit, &Filter { exclude: f.exclude.clone(), ..Default::default() })?);
            all.sort();
            all.dedup();
            all
        } else {
            pipeline::select(cfg, &explicit, &f)?
        };
        if let Some(user) = &self.simbrief {
            let ofp = crate::sources::simbrief::fetch(&cfg.http, user)?;
            term::info(&format!("SimBrief {}: {}", ofp.flight.clone().unwrap_or_else(|| user.clone()), ofp.icaos().join(" → ")));
            for i in ofp.icaos() {
                if !icaos.contains(&i) {
                    icaos.push(i);
                }
            }
        }
        Ok(icaos)
    }
}

#[derive(Args, Clone)]
pub struct PreviewArgs {
    /// ICAO code (looked up under --dir) or a built airport folder.
    target: String,
    /// Output directory with built airports.
    #[arg(long, default_value = "out")]
    dir: PathBuf,
    /// Output file (default: <airport folder>/chart.pdf or viewer.html).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Open the page in the default browser afterwards.
    #[arg(long)]
    open: bool,
}

#[derive(Args, Clone)]
pub struct BuildArgs {
    #[command(flatten)]
    select: SelectArgs,
    /// Output directory.
    #[arg(long, default_value = "out")]
    out: PathBuf,
    /// Output formats: geojson, pbf, or geojson,pbf.
    #[arg(long, default_value = "geojson,pbf")]
    format: String,
    /// Coordinate output: wgs84 (EPSG:4326) or metres (azimuthal equidistant from ARP).
    #[arg(long, default_value = "wgs84")]
    projection: String,
    /// X-Plane installation root (Custom Scenery / Global Airports apt.dat are used).
    #[arg(long = "xplane-dir")]
    xplane_dir: Option<PathBuf>,
    /// A specific apt.dat file (any size) to read airports from.
    #[arg(long = "aptdat")]
    aptdat_file: Option<PathBuf>,
    /// Do not query the X-Plane Scenery Gateway.
    #[arg(long = "no-gateway")]
    no_gateway: bool,
    /// OSM source: osmapi (default: direct OpenStreetMap API, Overpass fallback), overpass, or off.
    #[arg(long, default_value = "osmapi")]
    osm: String,
    /// Overpass mirror(s), comma separated.
    #[arg(long)]
    overpass: Option<String>,
    /// FAA NASR enrichment: auto (US airports only), off, or a path to the CSV zip/dir.
    #[arg(long, default_value = "auto")]
    faa: String,
    /// Directory with overrides/<ICAO>/<layer>.geojson.
    #[arg(long, default_value = "overrides")]
    overrides: PathBuf,
    /// OSM query radius around the ARP (km) when the airport extent is unknown.
    #[arg(long, default_value_t = 3.0)]
    radius_km: f64,
    /// Parallel jobs.
    #[arg(long, short = 'j')]
    jobs: Option<usize>,
    /// Do not generate Annex 14 runway markings.
    #[arg(long = "no-markings")]
    no_markings: bool,
    /// Derive synthetic 3.5 m taxiway shoulders around all pavement (off by default; Navigraph only has real ones).
    #[arg(long = "shoulders")]
    shoulders: bool,
    /// Also write the merged source model (_source.json) for debugging.
    #[arg(long = "write-source")]
    write_source: bool,
    /// HTTP timeout in seconds.
    #[arg(long, default_value_t = 300)]
    timeout: u64,
    /// Which layers to write: full (all 45) or map (the 34 a moving map draws).
    #[arg(long, default_value = "full")]
    profile: String,
    /// Explicit comma-separated layer list (overrides --profile), e.g. runwayelement,taxiwayelement.
    #[arg(long)]
    layers: Option<String>,

    // ---- batch control ----
    /// Skip airports that already have <out>/<ICAO>/manifest.json.
    #[arg(long = "skip-existing")]
    skip_existing: bool,
    /// Also rebuild airports whose last build failed (recorded in <out>/index.json).
    #[arg(long = "retry-failed")]
    retry_failed: bool,
    /// Print the resolved selection and exit without downloading or building.
    #[arg(long = "dry-run")]
    dry_run: bool,
    /// Delete <out>/<ICAO> before building it.
    #[arg(long)]
    clean: bool,
    /// Build in chunks of N airports (bounds memory and API load on big batches; 0 = all at once).
    #[arg(long, default_value_t = 0, value_name = "N")]
    chunk: usize,
    /// Stop the batch at the first airport that fails.
    #[arg(long = "fail-fast")]
    fail_fast: bool,
    /// Also write chart.pdf (Jeppesen-style airport diagram) for every built airport.
    #[arg(long)]
    chart: bool,
    /// Also write viewer.html (OANS-style moving-map preview) for every built airport.
    #[arg(long)]
    viewer: bool,
    /// Also pack every built airport into <out>/<ICAO>.zip.
    #[arg(long)]
    zip: bool,
    /// Write a JSON batch report (selected, built, failed, skipped, timings) to this file.
    #[arg(long, value_name = "FILE")]
    report: Option<PathBuf>,
}

impl BuildArgs {
    /// Minimal arguments for commands that only need the index.
    fn index_only(select: SelectArgs) -> BuildArgs {
        BuildArgs {
            select,
            out: PathBuf::from("out"),
            format: "geojson".into(),
            projection: "wgs84".into(),
            xplane_dir: None,
            aptdat_file: None,
            no_gateway: true,
            osm: "off".into(),
            overpass: None,
            faa: "off".into(),
            overrides: PathBuf::from("overrides"),
            radius_km: 5.0,
            jobs: None,
            no_markings: false,
            shoulders: false,
            write_source: false,
            timeout: 120,
            profile: "full".into(),
            layers: None,
            skip_existing: false,
            retry_failed: false,
            dry_run: false,
            clean: false,
            chunk: 0,
            fail_fast: false,
            chart: false,
            viewer: false,
            zip: false,
            report: None,
        }
    }
}

pub fn config(a: &BuildArgs) -> Result<Config> {
    let formats = Formats { geojson: a.format.contains("geojson") || a.format.contains("json"), pbf: a.format.contains("pbf") };
    if !formats.geojson && !formats.pbf {
        return Err(anyhow!("--format must include geojson and/or pbf"));
    }
    let projection = match a.projection.to_ascii_lowercase().as_str() {
        "wgs84" | "4326" | "epsg:4326" | "latlon" => Projection::Wgs84,
        "metres" | "meters" | "local" | "aeqd" => Projection::LocalMetres,
        other => return Err(anyhow!("unknown projection {other}")),
    };
    let osm = match a.osm.to_ascii_lowercase().as_str() {
        "osmapi" | "api" | "osm" => OsmMode::OsmApi,
        "overpass" => OsmMode::Overpass,
        "off" | "none" => OsmMode::Off,
        other => return Err(anyhow!("unknown --osm mode {other}")),
    };
    let faa = match a.faa.to_ascii_lowercase().as_str() {
        "auto" => FaaMode::Auto,
        "off" | "none" => FaaMode::Off,
        _ => FaaMode::File(PathBuf::from(&a.faa)),
    };
    let mirrors = a.overpass.as_ref().map(|s| s.split(',').map(|m| m.trim().to_string()).collect()).unwrap_or_else(pipeline::default_mirrors);
    let build = BuildOptions { runway_markings: !a.no_markings, derive_shoulders: a.shoulders, ..Default::default() };
    let layers: Vec<crate::model::Layer> = if let Some(list) = &a.layers {
        let mut v = Vec::new();
        for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            v.push(crate::model::Layer::from_name(name).ok_or_else(|| anyhow!("unknown layer {name}"))?);
        }
        v
    } else {
        match a.profile.to_ascii_lowercase().as_str() {
            "full" | "all" => crate::model::ALL_LAYERS.to_vec(),
            "map" | "oans" => crate::model::layer::MAP_PROFILE.to_vec(),
            other => return Err(anyhow!("unknown profile {other} (full|map)")),
        }
    };
    let ix = &a.select.index;
    Ok(Config {
        out: a.out.clone(),
        cache: Cache::new(ix.cache.clone(), ix.offline, ix.refresh),
        http: Http::new(a.timeout, 250),
        formats,
        projection,
        xplane_dir: a.xplane_dir.clone(),
        aptdat_file: a.aptdat_file.clone(),
        use_gateway: !a.no_gateway,
        osm,
        overpass_mirrors: mirrors,
        aptmeta: ix.aptmeta.clone(),
        ourairports: !ix.no_ourairports,
        faa,
        overrides_dir: a.overrides.clone(),
        radius_km: a.radius_km,
        build,
        write_ir: a.write_source,
        layers,
        index_cache: Cache::for_index(ix.offline),
    })
}

/// `ICAO` -> `<dir>/ICAO`, or an existing folder as given.
fn resolve_target(dir: &Path, target: &str) -> PathBuf {
    let p = Path::new(target);
    if p.is_dir() {
        p.to_path_buf()
    } else {
        dir.join(target.trim().to_uppercase())
    }
}

/// Built airports found in an output directory (folders with a manifest.json).
fn built_airports(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter(|e| e.path().join("manifest.json").is_file()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn read_manifest(dir: &Path) -> Result<Manifest> {
    let p = dir.join("manifest.json");
    let text = std::fs::read_to_string(&p).with_context(|| format!("read {} (not a built airport?)", p.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))
}

/// Total bytes per extension in an airport folder.
fn folder_sizes(dir: &Path) -> (u64, u64, u64) {
    let (mut gj, mut pb, mut other) = (0u64, 0u64, 0u64);
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let len = e.metadata().map(|m| m.len()).unwrap_or(0);
            match e.path().extension().and_then(|x| x.to_str()) {
                Some("geojson") => gj += len,
                Some("pbf") => pb += len,
                _ => other += len,
            }
        }
    }
    (gj, pb, other)
}

fn shown_path(p: &Path) -> String {
    std::env::current_dir().ok().and_then(|cwd| p.strip_prefix(&cwd).ok().map(|r| r.display().to_string())).unwrap_or_else(|| p.display().to_string())
}

/// Pack an airport folder into `<dir>/<ICAO>.zip` (files under `<ICAO>/`).
fn zip_airport(folder: &Path) -> Result<(PathBuf, u64)> {
    let name = folder.file_name().map(|s| s.to_string_lossy().to_string()).ok_or_else(|| anyhow!("bad folder {}", folder.display()))?;
    let out = folder.with_extension("zip");
    let file = std::fs::File::create(&out).with_context(|| format!("create {}", out.display()))?;
    let mut z = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut entries: Vec<PathBuf> = std::fs::read_dir(folder)?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    entries.sort();
    for p in entries {
        let fname = p.file_name().unwrap().to_string_lossy().to_string();
        z.start_file(format!("{name}/{fname}"), opts)?;
        let mut f = std::fs::File::open(&p)?;
        std::io::copy(&mut f, &mut z)?;
    }
    z.finish()?;
    let len = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    Ok((out, len))
}

fn write_preview(folder: &Path, kind: Preview, out: Option<&Path>) -> Result<PathBuf> {
    let out = out.map(Path::to_path_buf).unwrap_or_else(|| folder.join(kind.default_file()));
    let n = preview::write(folder, kind, &out)?;
    let icao = folder.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    term::file(Some(&icao), &shown_path(&out), &format!("{}, {}", kind.label(), term::human_bytes(n)));
    Ok(out)
}

/// Jeppesen-style airport diagram PDF for a built airport folder.
fn chart_pdf(folder: &Path, out: Option<&Path>) -> Result<PathBuf> {
    let out = out.map(Path::to_path_buf).unwrap_or_else(|| folder.join("chart.pdf"));
    let n = crate::output::chart::write(folder, &out)?;
    let icao = folder.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    term::file(Some(&icao), &shown_path(&out), &format!("airport diagram (PDF), {}", term::human_bytes(n)));
    Ok(out)
}

fn open_in_browser(p: &Path) {
    let p = std::fs::canonicalize(p).unwrap_or(p.to_path_buf());
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("cmd").args(["/C", "start", "", &p.display().to_string()]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(&p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&p).spawn();
    if let Err(e) = r {
        term::warn(&format!("could not open {}: {e}", p.display()));
    }
}

/// After a build: previews and zips for the airports that were built.
fn post_build(a: &BuildArgs, built: &[String]) {
    for icao in built {
        let folder = a.out.join(icao);
        if a.chart {
            if let Err(e) = chart_pdf(&folder, None) {
                term::warn(&format!("[{icao}] chart: {e:#}"));
            }
        }
        if a.viewer {
            if let Err(e) = write_preview(&folder, Preview::Viewer, None) {
                term::warn(&format!("[{icao}] viewer: {e:#}"));
            }
        }
        if a.zip {
            match zip_airport(&folder) {
                Ok((p, n)) => term::file(Some(icao), &shown_path(&p), &format!("zip, {}", term::human_bytes(n))),
                Err(e) => term::warn(&format!("[{icao}] zip: {e:#}")),
            }
        }
    }
}

fn build_cmd(a: BuildArgs) -> Result<()> {
    if let Some(j) = a.jobs {
        rayon::ThreadPoolBuilder::new().num_threads(j).build_global().ok();
    }
    let cfg = config(&a)?;
    let t0 = std::time::Instant::now();
    let mut icaos = a.select.resolve(&cfg)?;
    if a.retry_failed {
        let idx = Index::load_or_new(&a.out, cfg.projection.name());
        let failed: Vec<String> = idx.airports.iter().filter(|x| x.error.is_some()).map(|x| x.icao.clone()).collect();
        if !failed.is_empty() {
            term::info(&format!("Retrying {} airport(s) that failed last time: {}", failed.len(), failed.join(" ")));
        }
        for f in failed {
            if !icaos.contains(&f) {
                icaos.push(f);
            }
        }
        icaos.sort();
    }
    let selected = icaos.len();
    let mut skipped: Vec<String> = Vec::new();
    if a.skip_existing {
        icaos.retain(|i| {
            let exists = a.out.join(i).join("manifest.json").is_file();
            if exists {
                skipped.push(i.clone());
            }
            !exists
        });
        if !skipped.is_empty() {
            term::info(&format!("Skipping {} already built airport(s)", skipped.len()));
        }
    }
    if icaos.is_empty() {
        if selected > 0 {
            term::success("Nothing to do: every selected airport is already built");
            return Ok(());
        }
        return Err(anyhow!("no airports selected (give ICAO codes, --from-file, --simbrief, or --country/--region/--icao-prefix/--near/--bbox/--type/--all)"));
    }
    if a.dry_run {
        for i in &icaos {
            println!("{i}");
        }
        term::info(&format!("{} airport(s) would be built into {}{}", icaos.len(), shown_path(&a.out), if skipped.is_empty() { String::new() } else { format!(" ({} skipped as already built)", skipped.len()) }));
        return Ok(());
    }
    if a.clean {
        for i in &icaos {
            let d = a.out.join(i);
            if d.is_dir() {
                std::fs::remove_dir_all(&d).with_context(|| format!("remove {}", d.display()))?;
                term::step(Some(i), &format!("Removed {}", shown_path(&d)));
            }
        }
    }
    let chunk = if a.fail_fast && a.chunk == 0 { 1 } else { a.chunk };
    let mut summary = Summary::default();
    if chunk == 0 || chunk >= icaos.len() {
        summary = pipeline::run(&cfg, &icaos)?;
    } else {
        let total = icaos.len().div_ceil(chunk);
        for (n, part) in icaos.chunks(chunk).enumerate() {
            if total > 1 {
                term::info(&format!("Batch {}/{}: {}", n + 1, total, part.join(" ")));
            }
            let s = pipeline::run(&cfg, part)?;
            summary.built.extend(s.built);
            summary.failed.extend(s.failed);
            if a.fail_fast && !summary.failed.is_empty() {
                term::warn("Stopping at the first failure (--fail-fast)");
                break;
            }
        }
    }
    post_build(&a, &summary.built);
    if let Some(rp) = &a.report {
        let report = serde_json::json!({
            "generated": chrono::Utc::now().to_rfc3339(),
            "out": a.out,
            "selected": selected,
            "built": summary.built,
            "failed": summary.failed.iter().map(|(i, e)| serde_json::json!({"icao": i, "error": e})).collect::<Vec<_>>(),
            "skipped": skipped,
            "seconds": t0.elapsed().as_secs_f64(),
        });
        std::fs::write(rp, serde_json::to_string_pretty(&report)?).with_context(|| format!("write {}", rp.display()))?;
        term::file(None, &shown_path(rp), "batch report");
    }
    if !summary.failed.is_empty() {
        for (icao, e) in &summary.failed {
            term::error(&format!("[{icao}] {e}"));
        }
        return Err(anyhow!("{} airport(s) failed", summary.failed.len()));
    }
    Ok(())
}

fn info_cmd(s: SelectArgs) -> Result<()> {
    let cfg = config(&BuildArgs::index_only(s.clone()))?;
    let icaos = s.resolve(&cfg)?;
    if icaos.is_empty() {
        return Err(anyhow!("no airports selected"));
    }
    let idx = pipeline::load_index(&cfg)?;
    let shown = if s.limit.is_none() && icaos.len() > 200 { 200 } else { icaos.len() };
    for i in icaos.iter().take(shown) {
        match idx.get(i) {
            Some(e) => {
                let rwys = idx.runways.get(&e.icao).map(|rs| {
                    rs.iter().filter(|r| !r.closed).map(|r| format!("{}/{}{}", r.le_ident, r.he_ident, r.length_ft.map(|l| format!(" {l:.0} ft")).unwrap_or_default())).collect::<Vec<_>>().join(", ")
                }).unwrap_or_default();
                println!(
                    "{}  {}  {}\n      {}{}  {:.4},{:.4}  elev {}  {}{}\n      index: {}{}{}",
                    e.icao,
                    e.iata.clone().unwrap_or_else(|| "---".into()),
                    e.name.clone().unwrap_or_default(),
                    e.city.clone().map(|c| format!("{c}, ")).unwrap_or_default(),
                    e.country.clone().unwrap_or_default(),
                    e.lat,
                    e.lon,
                    e.elevation_ft.map(|v| format!("{v:.0} ft")).unwrap_or_else(|| "?".into()),
                    e.kind.clone().unwrap_or_default(),
                    e.transition_alt_ft.map(|t| format!("  TA {t:.0} ft")).unwrap_or_default(),
                    e.source,
                    e.gateway_scenery.map(|g| format!("  Gateway scenery {g}")).unwrap_or_default(),
                    if rwys.is_empty() { String::new() } else { format!("\n      runways: {rwys}") },
                );
            }
            None => println!("{i}  (not in the index; a build will still try the Gateway)"),
        }
    }
    if shown < icaos.len() {
        eprintln!("... {} more (use --limit to page)", icaos.len() - shown);
    }
    eprintln!("{} airport(s)", icaos.len());
    Ok(())
}

fn search_cmd(text: &str, limit: usize, index: IndexArgs) -> Result<()> {
    let cfg = config(&BuildArgs::index_only(SelectArgs { index, ..Default::default() }))?;
    let idx = pipeline::load_index(&cfg)?;
    let q = text.trim().to_lowercase();
    let mut hits: Vec<(u8, &crate::sources::index::IndexEntry)> = idx
        .by_icao
        .values()
        .filter_map(|e| {
            let rank = if e.icao.to_lowercase() == q || e.iata.as_deref().map_or(false, |i| i.to_lowercase() == q) {
                0
            } else if e.icao.to_lowercase().starts_with(&q) {
                1
            } else if e.name.as_deref().map_or(false, |n| n.to_lowercase().contains(&q)) {
                2
            } else if e.city.as_deref().map_or(false, |c| c.to_lowercase().contains(&q)) {
                3
            } else {
                return None;
            };
            Some((rank, e))
        })
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.icao.cmp(&b.1.icao)));
    for (_, e) in hits.iter().take(limit) {
        println!("{}  {:<4} {:<45} {}{}", e.icao, e.iata.clone().unwrap_or_default(), e.name.clone().unwrap_or_default(), e.city.clone().map(|c| format!("{c}, ")).unwrap_or_default(), e.country.clone().unwrap_or_default());
    }
    eprintln!("{} match(es){}", hits.len(), if hits.len() > limit { format!(", showing {limit}") } else { String::new() });
    Ok(())
}

fn stats_cmd(targets: Vec<String>, dir: PathBuf) -> Result<()> {
    let targets = if targets.is_empty() { built_airports(&dir) } else { targets };
    if targets.is_empty() {
        return Err(anyhow!("no built airports in {}", dir.display()));
    }
    let mut grand = 0usize;
    for t in &targets {
        let folder = resolve_target(&dir, t);
        let m = read_manifest(&folder)?;
        let (gj, pb, _) = folder_sizes(&folder);
        term::start(&format!("{} {}  {}  ARP {:.4},{:.4}{}", m.icao, m.iata.clone().unwrap_or_default(), m.name.clone().unwrap_or_default(), m.arp[0], m.arp[1], m.elevation_ft.map(|e| format!("  elev {e:.0} ft")).unwrap_or_default()));
        let n = m.layers.len();
        for (i, (name, info)) in m.layers.iter().enumerate() {
            term::layer(None, i + 1, n, name, info.count);
            if info.count == 0 {
                if let Some(r) = &info.empty_reason {
                    term::step(None, &format!("{name}: {r}"));
                }
            }
        }
        for w in &m.warnings {
            term::warn(w);
        }
        term::info(&format!("{} features in {} layers; sources: {}; generated {} by {}", pipeline::fmt_n(m.total_features()), n, m.sources.join(", "), m.generated, m.generator));
        if gj > 0 { term::file(Some(&m.icao), &format!("{}{}*.geojson", shown_path(&folder), std::path::MAIN_SEPARATOR), &term::human_bytes(gj)); }
        if pb > 0 { term::file(Some(&m.icao), &format!("{}{}*.pbf", shown_path(&folder), std::path::MAIN_SEPARATOR), &term::human_bytes(pb)); }
        grand += m.total_features();
        println!();
    }
    if targets.len() > 1 {
        term::success(&format!("{} airports, {} features", targets.len(), pipeline::fmt_n(grand)));
    }
    Ok(())
}

fn clean_cmd(icaos: Vec<String>, dir: PathBuf, all: bool) -> Result<()> {
    let icaos = if all { built_airports(&dir) } else { icaos.iter().map(|s| s.trim().to_uppercase()).collect() };
    if icaos.is_empty() {
        return Err(anyhow!("give ICAO codes or --all"));
    }
    let mut idx = Index::load_or_new(&dir, "");
    let projection = idx.projection.clone();
    let mut n = 0;
    for i in &icaos {
        let d = dir.join(i);
        if d.is_dir() {
            std::fs::remove_dir_all(&d).with_context(|| format!("remove {}", d.display()))?;
            term::step(Some(i), &format!("Removed {}", shown_path(&d)));
            n += 1;
        }
        let z = dir.join(format!("{i}.zip"));
        if z.is_file() {
            let _ = std::fs::remove_file(&z);
        }
        idx.airports.retain(|a| &a.icao != i);
    }
    if dir.join("index.json").is_file() {
        idx.projection = projection;
        idx.write(&dir)?;
    }
    term::success(&format!("Removed {n} airport(s)"));
    Ok(())
}

fn zip_cmd(icaos: Vec<String>, dir: PathBuf, all: bool) -> Result<()> {
    let icaos = if all { built_airports(&dir) } else { icaos.iter().map(|s| s.trim().to_uppercase()).collect() };
    if icaos.is_empty() {
        return Err(anyhow!("give ICAO codes or --all"));
    }
    for i in &icaos {
        let folder = resolve_target(&dir, i);
        if !folder.join("manifest.json").is_file() {
            term::warn(&format!("[{i}] not built ({})", shown_path(&folder)));
            continue;
        }
        let (p, n) = zip_airport(&folder)?;
        term::file(Some(i), &shown_path(&p), &format!("zip, {}", term::human_bytes(n)));
    }
    Ok(())
}

fn layers_cmd() {
    use crate::model::layer::{GeomKind, MAP_PROFILE};
    use crate::model::ALL_LAYERS;
    println!("{:>2}  {:<34} {:<8} {}", "#", "layer", "geometry", "map profile");
    for (i, l) in ALL_LAYERS.iter().enumerate() {
        let kind = match l.kind() {
            GeomKind::Point => "point",
            GeomKind::Curve => "line",
            GeomKind::Surface => "polygon",
        };
        println!("{:>2}  {:<34} {:<8} {}", i + 1, l.name(), kind, if MAP_PROFILE.contains(l) { "yes" } else { "" });
    }
    eprintln!("{} layers, {} in the map profile", ALL_LAYERS.len(), MAP_PROFILE.len());
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Build(a) | Cmd::Fetch(a) => build_cmd(a),
        Cmd::Validate { dir } => {
            let rep = crate::validate::validate_dir(&dir)?;
            for e in &rep.errors {
                println!("ERROR {e}");
            }
            for w in &rep.warnings {
                println!("WARN  {w}");
            }
            println!("{}: {} error(s), {} warning(s)", dir.display(), rep.errors.len(), rep.warnings.len());
            if rep.errors.is_empty() { Ok(()) } else { Err(anyhow!("validation failed")) }
        }
        Cmd::List(s) => {
            let cfg = config(&BuildArgs::index_only(s.clone()))?;
            let icaos = s.resolve(&cfg)?;
            for i in &icaos {
                println!("{i}");
            }
            eprintln!("{} airport(s)", icaos.len());
            Ok(())
        }
        Cmd::Info(s) => info_cmd(s),
        Cmd::Search { text, limit, index } => search_cmd(&text, limit, index),
        Cmd::Chart(p) => {
            let folder = resolve_target(&p.dir, &p.target);
            let out = chart_pdf(&folder, p.out.as_deref())?;
            if p.open {
                open_in_browser(&out);
            }
            Ok(())
        }
        Cmd::View(p) => preview_cmd(p, Preview::Viewer),
        Cmd::Stats { targets, dir } => stats_cmd(targets, dir),
        Cmd::Zip { icaos, dir, all } => zip_cmd(icaos, dir, all),
        Cmd::Clean { icaos, dir, all } => clean_cmd(icaos, dir, all),
        Cmd::Layers => {
            layers_cmd();
            Ok(())
        }
        Cmd::Codes => {
            println!("{}", serde_json::to_string_pretty(&crate::model::codes::legend())?);
            Ok(())
        }
    }
}

fn preview_cmd(p: PreviewArgs, kind: Preview) -> Result<()> {
    let folder = resolve_target(&p.dir, &p.target);
    let out = write_preview(&folder, kind, p.out.as_deref())?;
    if p.open {
        open_in_browser(&out);
    }
    Ok(())
}
