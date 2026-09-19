//! Per-airport pipeline and batch orchestration.

use crate::build::{self, BuildOptions};
use crate::cache::Cache;
use crate::geom::LocalFrame;
use crate::ir::SourceAirport;
use crate::model::codes::source;
use crate::model::{AmdbFeature, Layer};
use crate::output::manifest::{Index, IndexAirport, LayerInfo, Manifest};
use crate::output::{Formats, Projection};
use crate::sources::http::Http;
use crate::sources::index::AirportIndex;
use crate::sources::osm::{self, elements::Store};
use crate::sources::overrides::Overrides;
use crate::sources::xplane;
use crate::sources::{faa, index};
use crate::term;
use anyhow::{anyhow, Context, Result};
use geo_types::{Coord, Geometry, LineString, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub enum OsmMode {
    /// OpenStreetMap map API (direct database read, tiled), Overpass as fallback.
    OsmApi,
    /// Overpass only.
    Overpass,
    /// Alternate: even airports of a batch go to the map API first, odd ones to
    /// Overpass first, each with the other as fallback. Twice the throughput and
    /// neither service carries the whole load.
    Both,
    Off,
}

#[derive(Debug, Clone)]
pub enum FaaMode {
    Auto,
    Off,
    File(PathBuf),
}

pub struct Config {
    pub out: PathBuf,
    pub cache: Cache,
    pub http: Http,
    pub formats: Formats,
    pub projection: Projection,
    pub xplane_dir: Option<PathBuf>,
    pub aptdat_file: Option<PathBuf>,
    pub use_gateway: bool,
    pub osm: OsmMode,
    pub overpass_mirrors: Vec<String>,
    pub aptmeta: Option<PathBuf>,
    pub ourairports: bool,
    pub faa: FaaMode,
    pub overrides_dir: PathBuf,
    pub radius_km: f64,
    pub build: BuildOptions,
    pub write_ir: bool,
    /// Layers to write (default: all 45).
    pub layers: Vec<Layer>,
    /// Always-on daily cache for the airport index files.
    pub index_cache: Cache,
    /// Airports fetched from OpenStreetMap concurrently (2 for one-off builds, more
    /// for bulk runs; OSM's policy is fine with a few parallel map calls).
    pub osm_parallel: usize,
    /// Use the FAA's open airport-mapping layers for US airports (hotspots, and
    /// pavement when no scenery exists).
    pub faa_amdb: bool,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub built: Vec<String>,
    pub failed: Vec<(String, String)>,
    /// Build time per built airport in seconds.
    pub timings: Vec<(String, f64)>,
}

/// Load the airport index from every configured source.
pub fn load_index(cfg: &Config) -> Result<AirportIndex> {
    let mut idx = AirportIndex::default();
    if let Some(p) = &cfg.aptmeta {
        let n = idx.load_aptmeta(p)?;
        log::info!("aptmeta: {n} airports");
    }
    if cfg.ourairports {
        match idx.load_ourairports_online(&cfg.http, &cfg.index_cache) {
            Ok(n) => term::info(&format!("Index: {} airports from OurAirports", fmt_n(n))),
            Err(e) => log::warn!("ourairports unavailable: {e:#}"),
        }
    }
    if cfg.use_gateway {
        match idx.load_gateway_list(&cfg.http, &cfg.index_cache) {
            Ok(n) => term::info(&format!("Index: {} airports on the X-Plane Gateway", fmt_n(n))),
            Err(e) => log::warn!("gateway list unavailable: {e:#}"),
        }
    }
    Ok(idx)
}

pub fn fmt_n(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Serialises index.json updates across threads and processes' threads.
static INDEX_WRITE: Mutex<()> = Mutex::new(());

/// Airports currently being built somewhere in this process, so the bridge can wait
/// for a bulk worker instead of building the same airport a second time.
static IN_PROGRESS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct InProgress(Vec<String>);

impl InProgress {
    fn register(icaos: &[String]) -> InProgress {
        let up: Vec<String> = icaos.iter().map(|s| s.to_uppercase()).collect();
        IN_PROGRESS.lock().unwrap().extend(up.iter().cloned());
        InProgress(up)
    }
}

impl Drop for InProgress {
    fn drop(&mut self) {
        let mut g = IN_PROGRESS.lock().unwrap();
        for i in &self.0 {
            if let Some(pos) = g.iter().position(|x| x == i) {
                g.swap_remove(pos);
            }
        }
    }
}

/// True while another thread is building this airport.
pub fn is_building(icao: &str) -> bool {
    let up = icao.to_uppercase();
    IN_PROGRESS.lock().unwrap().iter().any(|x| *x == up)
}

struct Prepared {
    icao: String,
    src: SourceAirport,
    bbox: (f64, f64, f64, f64), // s, w, n, e
    country: Option<String>,
}

/// Source order: an explicit apt.dat file, then the Scenery Gateway (newest community
/// scenery), then the local X-Plane install for airports the Gateway does not have or
/// cannot deliver.
fn fetch_xplane(cfg: &Config, icao: &str, known_scenery: Option<Option<i64>>) -> Result<Option<SourceAirport>> {
    if let Some(f) = &cfg.aptdat_file {
        if let Some(block) = xplane::local::extract_airport_block(f, icao)? {
            return Ok(Some(xplane::aptdat::parse(&block, Some(icao))?));
        }
    }
    if cfg.use_gateway {
        match xplane::gateway::fetch_aptdat(&cfg.http, &cfg.cache, icao, known_scenery) {
            Ok(Some(text)) => return Ok(Some(xplane::aptdat::parse(&text, Some(icao))?)),
            Ok(None) => log::info!("{icao}: no Gateway scenery"),
            Err(e) => log::warn!("{icao}: gateway failed: {e:#}"),
        }
    }
    if let Some(root) = &cfg.xplane_dir {
        match xplane::local::lookup(root, icao) {
            Ok(Some((path, block))) => {
                term::step(Some(icao), &format!("Not on the Gateway; using the local X-Plane copy from {}", path.display()));
                return Ok(Some(xplane::aptdat::parse(&block, Some(icao))?));
            }
            Ok(None) => log::info!("{icao}: not in the local X-Plane install either"),
            Err(e) => log::warn!("{icao}: local X-Plane lookup failed: {e:#}"),
        }
    }
    Ok(None)
}

/// ICAO codes from a text or CSV file. Plain text: codes separated by whitespace,
/// commas or semicolons, `#` starts a comment. CSV (first line has a header with an
/// `icao`, `ident` or `idarpt` column): that column only, so a list exported with
/// `amdbgen list --csv` can be edited in a spreadsheet and fed straight back.
pub fn read_icao_file(path: &std::path::Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let text = text.trim_start_matches('\u{feff}');
    let first = text.lines().next().unwrap_or("").to_ascii_lowercase();
    let is_icao = |s: &str| (3..=4).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric()) && s.chars().any(|c| c.is_ascii_digit() || c.is_ascii_uppercase() || c.is_ascii_lowercase());
    let header_cols: Vec<&str> = first.split(',').map(str::trim).collect();
    if let Some(col) = header_cols.iter().position(|h| matches!(h.trim_matches('"'), "icao" | "ident" | "idarpt" | "icao_code")) {
        let mut rdr = csv::ReaderBuilder::new().flexible(true).has_headers(true).from_reader(text.as_bytes());
        let mut out = Vec::new();
        for rec in rdr.records() {
            let rec = rec?;
            if let Some(v) = rec.get(col).map(str::trim) {
                if is_icao(v) {
                    out.push(v.to_uppercase());
                }
            }
        }
        return Ok(out);
    }
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        out.extend(line.split(|c: char| c.is_whitespace() || c == ',' || c == ';').map(str::trim).filter(|s| is_icao(s)).map(str::to_uppercase));
    }
    Ok(out)
}

/// The airport's country from whatever source already knows it.
fn country_hint(src: &SourceAirport, entry: &Option<crate::sources::index::IndexEntry>) -> Option<String> {
    src.header.country.clone().or_else(|| entry.as_ref().and_then(|e| e.country.clone()))
}

fn prepare(cfg: &Config, idx: &AirportIndex, icao: &str) -> Result<Prepared> {
    let icao = icao.to_uppercase();
    let mut src = SourceAirport::new(&icao);
    let entry = idx.get(&icao).cloned();
    if let Some(e) = &entry {
        src.absorb(SourceAirport { header: e.header(), sources: vec![e.source.to_string()], ..Default::default() });
    }
    // Trust a "no scenery" answer only when the Gateway list itself was loaded.
    let known = entry.as_ref().filter(|e| e.source == source::XPLANE || e.gateway_scenery.is_some()).map(|e| e.gateway_scenery);
    let t_xp = std::time::Instant::now();
    if let Some(xp) = fetch_xplane(cfg, &icao, known)? {
        term::info(&format!("[{icao}] X-Plane apt.dat: {} runways, {} pavements, {} stands, {} taxi routes in {}", xp.runways.len(), xp.pavements.len(), xp.stands.len(), xp.route_edges.len(), term::human_secs(t_xp.elapsed().as_secs_f64())));
        // X-Plane datum/elevation are authoritative when present.
        let mut merged = xp;
        merged.absorb(std::mem::take(&mut src));
        src = merged;
    } else {
        term::warn(&format!("[{icao}] No X-Plane scenery on the Gateway; runways and taxiways will come from OpenStreetMap"));
    }
    // FAA airport mapping (US only): hotspots always, pavement and buildings only when
    // no scenery was found, so real scenery is never duplicated. Never fatal.
    if cfg.faa_amdb && crate::sources::faa_amdb::covers(&icao, country_hint(&src, &entry).as_deref()) {
        let no_scenery = src.pavements.is_empty() && src.runways.is_empty();
        let no_windsock = !src.point_structures.iter().any(|p| p.kind == crate::model::codes::pntsttyp::WINDSOCK);
        match crate::sources::faa_amdb::fetch(&cfg.http, &cfg.cache, &icao, no_scenery, no_windsock) {
            Ok(extra) => {
                let (h, p) = (extra.areas.len(), extra.pavements.len());
                if h + p + extra.buildings.len() + extra.point_structures.len() > 0 {
                    term::info(&format!("[{icao}] FAA airport mapping: {h} hotspot(s){}", if p > 0 { format!(", {p} pavement polygons (no scenery available)") } else { String::new() }));
                    src.absorb(extra);
                }
            }
            Err(e) => log::info!("{icao}: FAA airport mapping unavailable: {e:#}"),
        }
    }
    if src.header.arp.is_none() && src.runways.is_empty() {
        // Last resort: ask the Gateway for the position.
        let mut tmp = AirportIndex::default();
        if cfg.use_gateway && tmp.load_gateway_single(&cfg.http, &cfg.cache, &icao).unwrap_or(false) {
            if let Some(e) = tmp.get(&icao) {
                src.header.arp = Some(Coord { x: e.lon, y: e.lat });
                src.header.name = src.header.name.take().or_else(|| e.name.clone());
            }
        }
    }
    let (lat, lon) = build::choose_arp(&src).ok_or_else(|| anyhow!("{icao}: unknown airport (not in any index or source)"))?;
    let bbox = match src.bbox() {
        Some((w, s, e, n)) if !src.runways.is_empty() || !src.pavements.is_empty() => {
            // 500 m beyond the outermost pavement/stand is enough for terminals and towers.
            let dlat = 0.0045;
            let dlon = 0.0045 / lat.to_radians().cos().max(0.2);
            (s - dlat, w - dlon, n + dlat, e + dlon)
        }
        _ => {
            let dlat = cfg.radius_km / 111.195;
            let dlon = dlat / lat.to_radians().cos().max(0.2);
            (lat - dlat, lon - dlon, lat + dlat, lon + dlon)
        }
    };
    let country = src.header.country.clone().or_else(|| entry.as_ref().and_then(|e| e.country.clone()));
    Ok(Prepared { icao, src, bbox, country })
}

fn faa_enrich(tables: &faa::NasrTables, src: &mut SourceAirport) {
    let Some(n) = faa::lookup(tables, &src.header.icao) else { return };
    if src.header.faa.is_none() {
        src.header.faa = Some(n.faa_id.clone());
    }
    for r in src.runways.iter_mut() {
        for k in 0..2 {
            let ident = r.ends[k].ident.to_uppercase();
            let key = {
                let digits: String = ident.chars().take_while(|c| c.is_ascii_digit()).collect();
                let suffix: String = ident.chars().skip_while(|c| c.is_ascii_digit()).collect();
                digits.parse::<u32>().map(|d| format!("{d:02}{suffix}")).unwrap_or(ident.clone())
            };
            if let Some(info) = n.ends.get(&key) {
                if let Some(sw) = info.stopway_m {
                    r.stopway_m[k] = sw;
                }
                r.ends[k].tora_m = info.tora_m;
                r.ends[k].toda_m = info.toda_m;
                r.ends[k].asda_m = info.asda_m;
                r.ends[k].lda_m = info.lda_m;
                r.ends[k].tdze_ft = info.tdz_elev_ft;
            }
        }
    }
    term::info(&format!("[{}] FAA NASR: declared distances for {} runway ends, {} arresting systems, {} LAHSO", src.header.icao, n.ends.len(), n.arresting.len(), n.lahso.len()));
    src.arresting.extend(n.arresting);
    src.lahso.extend(n.lahso);
    src.sources.push(source::FAA_NASR.to_string());
}

fn project_geometry(frame: &LocalFrame, g: &Geometry<f64>) -> Geometry<f64> {
    let f = |c: &Coord<f64>| frame.forward(c.x, c.y);
    let ls = |l: &LineString<f64>| LineString(l.0.iter().map(f).collect());
    let poly = |p: &Polygon<f64>| Polygon::new(ls(p.exterior()), p.interiors().iter().map(ls).collect());
    match g {
        Geometry::Point(p) => Geometry::Point(Point(f(&p.0))),
        Geometry::LineString(l) => Geometry::LineString(ls(l)),
        Geometry::Polygon(p) => Geometry::Polygon(poly(p)),
        Geometry::MultiPoint(m) => Geometry::MultiPoint(MultiPoint(m.0.iter().map(|p| Point(f(&p.0))).collect())),
        Geometry::MultiLineString(m) => Geometry::MultiLineString(MultiLineString(m.0.iter().map(ls).collect())),
        Geometry::MultiPolygon(m) => Geometry::MultiPolygon(MultiPolygon(m.0.iter().map(poly).collect())),
        other => other.clone(),
    }
}

fn apply_overrides(frame: &LocalFrame, features: &mut BTreeMap<Layer, Vec<AmdbFeature>>, ov: Overrides, icao: &str) -> Vec<String> {
    let mut notes = Vec::new();
    for (layer, (replace, feats)) in ov.layers {
        let projected: Vec<AmdbFeature> = feats
            .into_iter()
            .map(|mut f| {
                f.geom = project_geometry(frame, &f.geom);
                f.props.insert("source".into(), source::OVERRIDE.into());
                f.props.entry("idarpt".to_string()).or_insert_with(|| icao.into());
                f
            })
            .collect();
        let target = features.entry(layer).or_default();
        if replace {
            target.clear();
        }
        let n = projected.len();
        // Give override features ids after the generated ones.
        let base = target.len();
        for (i, mut f) in projected.into_iter().enumerate() {
            if !f.props.contains_key("id") {
                f.set("id", format!("{icao}:{}:ov{}", layer.name(), base + i + 1));
            }
            target.push(f);
        }
        notes.push(format!("override {}: {} feature(s){}", layer.name(), n, if replace { " (replaced)" } else { "" }));
    }
    notes
}

fn build_and_write(cfg: &Config, mut p: Prepared, osm_store: Option<Store>, faa_tables: Option<&faa::NasrTables>) -> Result<Manifest> {
    let icao = p.icao.clone();
    if let Some(st) = osm_store {
        let osm_src = osm::tags::store_to_ir(&st, &icao);
        p.src.absorb(osm_src);
    }
    if let Some(t) = faa_tables {
        faa_enrich(t, &mut p.src);
    }
    if p.src.runways.is_empty() {
        term::warn(&format!("[{icao}] No runway from any source (Gateway, local X-Plane, OpenStreetMap): the airport is written with its reference point and empty layers"));
    }
    p.src.sources.sort();
    p.src.sources.dedup();
    let dir = cfg.out.join(&icao);
    std::fs::create_dir_all(&dir)?;
    if cfg.write_ir {
        std::fs::write(dir.join("_source.json"), serde_json::to_string(&p.src)?)?;
    }
    let t_build = std::time::Instant::now();
    let res = build::build(&p.src, cfg.build.clone())?;
    let mut features = res.features;
    let mut warnings = res.warnings;
    let n_feat: usize = features.values().map(Vec::len).sum();
    term::info(&format!("[{icao}] Derived {} layers, {} features in {}", cfg.layers.len(), fmt_n(n_feat), term::human_secs(t_build.elapsed().as_secs_f64())));
    let ov = Overrides::load(&cfg.overrides_dir, &icao)?;
    let notes = apply_overrides(&res.frame, &mut features, ov, &icao);
    for n in &notes {
        term::info(&format!("[{icao}] {n}"));
    }
    warnings.extend(notes);
    let rep = crate::validate::validate_and_fix(&mut features);
    if rep.errors.is_empty() && rep.warnings.is_empty() {
        term::info(&format!("[{icao}] Validated: no issues"));
    } else {
        term::warn(&format!("[{icao}] Validated: {} dropped, {} warnings", rep.errors.len(), rep.warnings.len()));
    }
    warnings.extend(rep.warnings.iter().cloned());
    for e in &rep.errors {
        warnings.push(format!("validation: {e}"));
    }
    let t_write = std::time::Instant::now();
    crate::output::write_airport(&dir, &icao, &res.frame, &features, cfg.projection, cfg.formats, &cfg.layers)?;
    let _ = t_write;

    let mut layers = BTreeMap::new();
    for (layer, feats) in features.iter().filter(|(l, _)| cfg.layers.contains(l)) {
        let mut sources = res.layer_sources.get(layer).cloned().unwrap_or_default();
        if feats.iter().any(|f| f.get_str("source") == Some(source::OVERRIDE)) {
            sources.push(source::OVERRIDE.into());
        }
        let empty_reason = if feats.is_empty() { Some(empty_reason(*layer, &p.src)) } else { None };
        layers.insert(layer.name().to_string(), LayerInfo { count: feats.len(), sources, empty_reason });
    }
    let bbox = p.src.bbox().map(|(a, b, c, d)| [a, b, c, d]);
    let mut formats = Vec::new();
    if cfg.formats.geojson {
        formats.push("geojson".into());
    }
    if cfg.formats.pbf {
        formats.push("pbf".into());
    }
    let m = Manifest {
        icao: icao.clone(),
        iata: p.src.header.iata.clone(),
        name: p.src.header.name.clone(),
        country: p.country.clone(),
        arp: [res.frame.lat0, res.frame.lon0],
        elevation_ft: p.src.header.elevation_ft,
        projection: cfg.projection.name().into(),
        formats,
        generated: chrono::Utc::now().to_rfc3339(),
        generator: format!("amdbgen {}", env!("CARGO_PKG_VERSION")),
        sources: p.src.sources.clone(),
        bbox,
        layers,
        warnings,
    };
    m.write(&dir)?;
    Ok(m)
}

fn empty_reason(layer: Layer, src: &SourceAirport) -> String {
    let has_xp = src.sources.iter().any(|s| s == source::XPLANE);
    let has_osm = src.sources.iter().any(|s| s == source::OSM);
    match layer {
        Layer::Hotspot | Layer::AtcBlindSpot => "no free worldwide source; supply via overrides/<ICAO>/<layer>.geojson".into(),
        Layer::SurveyControlPoint | Layer::PositionMarking => "surveyor data only; supply via overrides".into(),
        Layer::LandAndHoldShortOperationLocation | Layer::ArrestingGearLocation | Layer::ArrestingSystemLocation => "not published for this airport (FAA NASR covers US airports only)".into(),
        Layer::Stopway => "no stopway in sources (FAA declared distances / OSM aeroway=stopway)".into(),
        Layer::DeicingArea | Layer::DeicingGroup => "no deicing pad tagged in OSM".into(),
        Layer::ConstructionArea => "no construction area in OSM".into(),
        Layer::Water => "no water within the airport extent".into(),
        Layer::BridgeSide => "no bridged taxiway in OSM".into(),
        Layer::VerticalPolygonalStructure | Layer::VerticalPointStructure | Layer::VerticalLineStructure | Layer::ServiceRoad => {
            if has_osm { "nothing matching within the airport extent in OSM".into() } else { "OSM source not used".into() }
        }
        Layer::AsrnNode | Layer::AsrnEdge => "no taxi routing network (X-Plane) and no OSM taxiways".into(),
        Layer::AerodromeSign | Layer::AerodromeSurfaceLighting | Layer::FrequencyArea => if has_xp { "not present in the X-Plane scenery".into() } else { "X-Plane source not available".into() },
        Layer::FinalApproachAndTakeOffArea | Layer::TouchDownLiftOffArea | Layer::HelipadThreshold => "no helipad in sources".into(),
        Layer::RunwayShoulder => "runways have no shoulder in the X-Plane data".into(),
        Layer::RunwayDisplacedArea => "no displaced thresholds".into(),
        Layer::Blastpad => "no blastpads".into(),
        Layer::RunwayIntersection => "runways do not cross".into(),
        _ => "no data in sources".into(),
    }
}

/// One OpenStreetMap source in the pool.
enum OsmSource {
    /// The OSM map API: returns everything inside the box, and its byte limit is per IP,
    /// so it is one source no matter how many threads use it.
    MapApi,
    /// One Overpass endpoint. Each is a separate server with its own limits, so several
    /// of them genuinely multiply throughput.
    Overpass(String),
}

impl OsmSource {
    fn label(&self) -> String {
        match self {
            OsmSource::MapApi => "the OSM map API".to_string(),
            OsmSource::Overpass(u) => format!("overpass {}", u.split('/').nth(2).unwrap_or(u)),
        }
    }
}

/// Every source usable for one airport: the map API, then the airport's regional
/// Overpass instance when it has one, then the worldwide Overpass pool.
fn osm_pool(cfg: &Config, country: Option<&str>) -> Vec<OsmSource> {
    let mut pool = Vec::new();
    if matches!(cfg.osm, OsmMode::OsmApi | OsmMode::Both) {
        pool.push(OsmSource::MapApi);
    }
    if matches!(cfg.osm, OsmMode::Overpass | OsmMode::Both) {
        pool.extend(osm::overpass::endpoints_for(country, 0, &cfg.overpass_mirrors).into_iter().map(OsmSource::Overpass));
    }
    if pool.is_empty() {
        pool.push(OsmSource::MapApi);
    }
    pool
}

/// Run the pipeline for a list of ICAOs.
pub fn run(cfg: &Config, icaos: &[String]) -> Result<Summary> {
    let idx = load_index(cfg)?;
    let summary = Mutex::new(Summary::default());
    // Entries for index.json are collected here and merged under a lock at the end, so
    // several `run`s in different threads (bulk workers + the bridge) cannot clobber
    // each other's entries.
    let index_file: Mutex<Vec<IndexAirport>> = Mutex::new(Vec::new());
    let _guard = InProgress::register(icaos);

    term::start(&format!("Fetching sources for {} airport{}", icaos.len(), if icaos.len() == 1 { "" } else { "s" }));
    let t_all = std::time::Instant::now();
    // Phase A: X-Plane + index (network-paced).
    let prepared: Vec<Prepared> = icaos
        .par_iter()
        .filter_map(|icao| match prepare(cfg, &idx, icao) {
            Ok(p) => Some(p),
            Err(e) => {
                log::error!("{icao}: {e:#}");
                summary.lock().unwrap().failed.push((icao.to_uppercase(), format!("{e:#}")));
                None
            }
        })
        .collect();

    // FAA tables (only when some airport is in the US), loaded before any building
    // starts so builds can begin the moment an airport's own data is in.
    let faa_tables: Option<faa::NasrTables> = match &cfg.faa {
        FaaMode::Off => None,
        FaaMode::File(p) => Some(faa::load_tables(&cfg.http, &cfg.cache, Some(p))?),
        FaaMode::Auto => {
            let any_us = prepared.iter().any(|p| p.country.as_deref() == Some("US") || (p.icao.starts_with('K') && p.country.is_none()) || p.icao.starts_with("PA") || p.icao.starts_with("PH"));
            if any_us {
                match faa::load_tables(&cfg.http, &cfg.cache, None) {
                    Ok(t) => Some(t),
                    Err(e) => {
                        log::warn!("FAA NASR unavailable: {e:#}");
                        None
                    }
                }
            } else {
                None
            }
        }
    };

    // Build one airport and record the result.
    let build_one = |p: Prepared, st: Option<Store>| {
        let icao = p.icao.clone();
        let t0 = std::time::Instant::now();
        let is_us = p.country.as_deref() == Some("US") || (p.country.is_none() && icao.starts_with('K'));
        let t = if is_us { faa_tables.as_ref() } else { None };
        match build_and_write(cfg, p, st, t) {
            Ok(m) => {
                term::success(&format!("[{icao}] Built {icao} in {} ({} features{})", term::human_secs(t0.elapsed().as_secs_f64()), fmt_n(m.total_features()), if m.warnings.is_empty() { String::new() } else { format!(", {} warnings", m.warnings.len()) }));
                let dir = cfg.out.join(&icao);
                let (mut gj, mut pb) = (0u64, 0u64);
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        let len = e.metadata().map(|x| x.len()).unwrap_or(0);
                        match e.path().extension().and_then(|x| x.to_str()) {
                            Some("geojson") => gj += len,
                            Some("pbf") => pb += len,
                            _ => {}
                        }
                    }
                }
                let shown = std::env::current_dir().ok().and_then(|cwd| dir.strip_prefix(&cwd).ok().map(|r| r.to_path_buf())).unwrap_or(dir.clone());
                let shown = format!("{}{}", shown.display(), std::path::MAIN_SEPARATOR);
                if gj > 0 { term::file(Some(&icao), &format!("{shown}*.geojson"), &format!("{} layers, {}", cfg.layers.len(), term::human_bytes(gj))); }
                if pb > 0 { term::file(Some(&icao), &format!("{shown}*.pbf"), &format!("{} layers, {}", cfg.layers.len(), term::human_bytes(pb))); }
                term::file(Some(&icao), &format!("{shown}manifest.json"), "sources, counts, warnings");
                index_file.lock().unwrap().push(IndexAirport {
                    icao: icao.clone(),
                    iata: m.iata.clone(),
                    name: m.name.clone(),
                    country: m.country.clone(),
                    arp: m.arp,
                    bbox: m.bbox,
                    features: m.total_features(),
                    sources: m.sources.clone(),
                    dir: icao.clone(),
                    error: None,
                });
                let mut s = summary.lock().unwrap();
                s.timings.push((icao.clone(), t0.elapsed().as_secs_f64()));
                s.built.push(icao);
            }
            Err(e) => {
                log::error!("{icao}: {e:#}");
                summary.lock().unwrap().failed.push((icao, format!("{e:#}")));
            }
        }
    };

    // Phase B: OpenStreetMap, every source working at once, and each airport built the
    // moment its own data is in. The map API and each Overpass endpoint form a pool;
    // airport k of a batch starts on entry k, so a batch of n airports keeps n different
    // servers busy rather than queueing on one. If a source fails or is throttled, that
    // airport walks on to the next in the pool. Nothing waits for the slowest download.
    term::start(&format!("Fetching and building {} airport{}", prepared.len(), if prepared.len() == 1 { "" } else { "s" }));
    if matches!(cfg.osm, OsmMode::Off) {
        prepared.into_par_iter().for_each(|p| build_one(p, None));
    } else {
        let slots: Vec<Mutex<Option<Prepared>>> = prepared.into_iter().map(|p| Mutex::new(Some(p))).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let build_one = &build_one;
        rayon::scope(|rs| {
            std::thread::scope(|sc| {
                for _ in 0..cfg.osm_parallel.max(1) {
                    sc.spawn(|| loop {
                        // A shared queue rather than fixed groups: a worker that finishes
                        // early takes the next airport immediately.
                        let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(slot) = slots.get(k) else { break };
                        let Some(p) = slot.lock().unwrap().take() else { continue };
                        let t0 = std::time::Instant::now();
                        let mut last: Result<Store> = Err(anyhow!("no OpenStreetMap source available"));
                        let mut used = String::from("none");
                        if let Some((s, from)) = osm::cached(&cfg.cache, &p.icao) {
                            last = Ok(s);
                            used = from.to_string();
                        } else {
                            let pool = osm_pool(cfg, p.country.as_deref());
                            // Alternate between the two fastest sources (the map API and the
                            // first Overpass endpoint), then fall back through the rest.
                            // Congested mirrors are a backstop, never anyone's first choice.
                            let fast = pool.len().min(2);
                            let primary = if fast == 0 { 0 } else { k % fast };
                            let order: Vec<usize> = std::iter::once(primary).chain((0..pool.len()).filter(|j| *j != primary)).collect();
                            for &i in &order {
                                let Some(src) = pool.get(i) else { continue };
                                let attempt = match src {
                                    OsmSource::MapApi => osm::osmapi::fetch(&cfg.http, &cfg.cache, &p.icao, p.bbox),
                                    OsmSource::Overpass(url) => osm::overpass::fetch(&cfg.http, &cfg.cache, std::slice::from_ref(url), &p.icao, p.bbox),
                                };
                                match attempt {
                                    Ok(s) => {
                                        used = src.label();
                                        last = Ok(s);
                                        break;
                                    }
                                    Err(e) => {
                                        log::warn!("{}: {} failed ({e:#}); trying the next source", p.icao, src.label());
                                        last = Err(e);
                                    }
                                }
                            }
                        }
                        let store = match last {
                            Ok(s) => {
                                term::info(&format!("[{}] OpenStreetMap via {used}: {} nodes, {} ways in {}", p.icao, fmt_n(s.nodes.len()), fmt_n(s.ways.len()), term::human_secs(t0.elapsed().as_secs_f64())));
                                Some(s)
                            }
                            Err(e) => {
                                log::warn!("{}: no OSM source answered: {e:#}", p.icao);
                                None
                            }
                        };
                        // Build on the shared pool, so this worker moves straight on to
                        // the next download.
                        rs.spawn(move |_| build_one(p, store));
                    });
                }
            });
        });
    }
    {
        let _lock = INDEX_WRITE.lock().unwrap();
        let mut idx = Index::load_or_new(&cfg.out, cfg.projection.name());
        for a in index_file.into_inner().unwrap() {
            idx.upsert(a);
        }
        idx.write(&cfg.out).context("write index")?;
    }
    let summary = summary.into_inner().unwrap();
    let _ = std::io::Write::flush(&mut std::io::stdout());
    println!();
    if summary.failed.is_empty() {
        term::success(&format!("Built {} airport{} in {}", summary.built.len(), if summary.built.len() == 1 { "" } else { "s" }, term::human_secs(t_all.elapsed().as_secs_f64())));
    } else {
        term::warn(&format!("Built {} airport(s), {} failed, in {}", summary.built.len(), summary.failed.len(), term::human_secs(t_all.elapsed().as_secs_f64())));
    }
    Ok(summary)
}

/// Resolve an airport selection into ICAOs using the index.
/// How to pick airports from the index (all criteria are ANDed; explicit ICAO codes
/// are always included).
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub country: Option<String>,
    /// Any of these ISO countries.
    pub countries: Vec<String>,
    /// Any of these continent codes (AF, AN, AS, EU, NA, OC, SA).
    pub continents: Vec<String>,
    pub region: Option<String>,
    pub prefix: Option<String>,
    pub all: bool,
    /// Centre (lat, lon) and radius in km.
    pub near: Option<(f64, f64, f64)>,
    /// West, south, east, north in degrees.
    pub bbox: Option<[f64; 4]>,
    /// OurAirports kinds, matched as substrings ("large" matches "large_airport").
    pub kinds: Vec<String>,
    pub min_runway_ft: Option<f64>,
    /// At least this many open runways (OurAirports data).
    pub min_runways: Option<usize>,
    pub iata: Vec<String>,
    /// Case-insensitive substring of the name or city.
    pub search: Option<String>,
    /// ICAO codes (4 chars) or prefixes to drop from the result.
    pub exclude: Vec<String>,
    pub offset: usize,
    pub limit: Option<usize>,
}

impl Filter {
    pub fn needs_index(&self) -> bool {
        self.country.is_some() || !self.countries.is_empty() || !self.continents.is_empty() || self.region.is_some() || self.prefix.is_some() || self.all || self.near.is_some() || self.bbox.is_some() || !self.kinds.is_empty() || self.min_runway_ft.is_some() || self.min_runways.is_some() || !self.iata.is_empty() || self.search.is_some()
    }

    fn excluded(&self, icao: &str) -> bool {
        self.exclude.iter().any(|x| if x.len() >= 4 { icao.eq_ignore_ascii_case(x) } else { icao.to_uppercase().starts_with(&x.to_uppercase()) })
    }
}

/// Continent code for a name or code the user typed ("asia", "AS", "north-america", "na").
pub fn continent_code(s: &str) -> Option<&'static str> {
    let k: String = s.trim().to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    Some(match k.as_str() {
        "af" | "africa" => "AF",
        "an" | "antarctica" => "AN",
        "as" | "asia" => "AS",
        "eu" | "europe" => "EU",
        "na" | "northamerica" | "north" => "NA",
        "oc" | "oceania" | "australia" | "pacific" => "OC",
        "sa" | "southamerica" | "south" => "SA",
        _ => return None,
    })
}

/// Great-circle distance in km.
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6371.0088 * a.sqrt().asin()
}

/// Airports selected by explicit codes plus the filter, sorted and deduplicated.
pub fn select(cfg: &Config, explicit: &[String], f: &Filter) -> Result<Vec<String>> {
    let mut out: Vec<String> = explicit.iter().map(|s| s.trim().to_uppercase()).filter(|s| !s.is_empty()).collect();
    if f.needs_index() {
        let idx = load_index(cfg)?;
        let mut v: Vec<String> = idx
            .by_icao
            .values()
            .filter(|e| f.country.as_deref().map_or(true, |c| e.country.as_deref().map_or(false, |x| x.eq_ignore_ascii_case(c))))
            .filter(|e| f.countries.is_empty() || e.country.as_deref().map_or(false, |x| f.countries.iter().any(|c| c.eq_ignore_ascii_case(x))))
            .filter(|e| f.continents.is_empty() || e.continent.as_deref().map_or(false, |x| f.continents.iter().any(|c| c.eq_ignore_ascii_case(x))))
            .filter(|e| f.min_runways.map_or(true, |min| idx.runways.get(&e.icao).map_or(false, |rs| rs.iter().filter(|r| !r.closed).count() >= min)))
            .filter(|e| f.region.as_deref().map_or(true, |r| e.region.as_deref().map_or(false, |x| x.to_uppercase().starts_with(&r.to_uppercase()))))
            .filter(|e| f.prefix.as_deref().map_or(true, |p| e.icao.starts_with(&p.to_uppercase())))
            .filter(|e| f.near.map_or(true, |(lat, lon, km)| haversine_km(lat, lon, e.lat, e.lon) <= km))
            .filter(|e| f.bbox.map_or(true, |[w, s, ea, n]| e.lon >= w && e.lon <= ea && e.lat >= s && e.lat <= n))
            .filter(|e| f.kinds.is_empty() || e.kind.as_deref().map_or(false, |k| f.kinds.iter().any(|want| k.to_ascii_lowercase().contains(&want.to_ascii_lowercase()))))
            .filter(|e| f.min_runway_ft.map_or(true, |min| idx.runways.get(&e.icao).map_or(false, |rs| rs.iter().any(|r| !r.closed && r.length_ft.unwrap_or(0.0) >= min))))
            .filter(|e| f.iata.is_empty() || e.iata.as_deref().map_or(false, |i| f.iata.iter().any(|want| want.eq_ignore_ascii_case(i))))
            .filter(|e| f.search.as_deref().map_or(true, |q| {
                let q = q.to_lowercase();
                e.name.as_deref().map_or(false, |n| n.to_lowercase().contains(&q)) || e.city.as_deref().map_or(false, |c| c.to_lowercase().contains(&q))
            }))
            .map(|e| e.icao.clone())
            .collect();
        v.sort();
        let v: Vec<String> = v.into_iter().skip(f.offset).take(f.limit.unwrap_or(usize::MAX)).collect();
        out.extend(v);
    }
    // Explicit codes keep their order (a list file is a priority order); index results
    // come sorted after them; duplicates keep their first position.
    let mut seen = std::collections::HashSet::new();
    out.retain(|i| !f.excluded(i) && seen.insert(i.clone()));
    Ok(out)
}

/// Backwards-compatible wrapper around [`select`].
pub fn select_icaos(cfg: &Config, explicit: &[String], country: Option<&str>, region: Option<&str>, prefix: Option<&str>, all: bool) -> Result<Vec<String>> {
    let f = Filter { country: country.map(str::to_string), region: region.map(str::to_string), prefix: prefix.map(str::to_string), all, ..Default::default() };
    select(cfg, explicit, &f)
}

pub fn default_mirrors() -> Vec<String> {
    osm::overpass::DEFAULT_MIRRORS.iter().map(|s| s.to_string()).collect()
}

#[allow(dead_code)]
fn _unused(_: index::IndexEntry) {}
