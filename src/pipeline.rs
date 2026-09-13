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
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub enum OsmMode {
    /// OpenStreetMap map API (direct database read, tiled), Overpass as fallback.
    OsmApi,
    /// Overpass only.
    Overpass,
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
}

#[derive(Debug, Default)]
pub struct Summary {
    pub built: Vec<String>,
    pub failed: Vec<(String, String)>,
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

struct Prepared {
    icao: String,
    src: SourceAirport,
    bbox: (f64, f64, f64, f64), // s, w, n, e
    country: Option<String>,
}

fn fetch_xplane(cfg: &Config, icao: &str, known_scenery: Option<Option<i64>>) -> Result<Option<SourceAirport>> {
    if let Some(f) = &cfg.aptdat_file {
        if let Some(block) = xplane::local::extract_airport_block(f, icao)? {
            return Ok(Some(xplane::aptdat::parse(&block, Some(icao))?));
        }
    }
    if let Some(root) = &cfg.xplane_dir {
        if let Some((path, block)) = xplane::local::find_in_root(root, icao)? {
            log::info!("{icao}: apt.dat from {}", path.display());
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
    Ok(None)
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

/// Run the pipeline for a list of ICAOs.
pub fn run(cfg: &Config, icaos: &[String]) -> Result<Summary> {
    let idx = load_index(cfg)?;
    let summary = Mutex::new(Summary::default());
    let index_file = Mutex::new(Index::load_or_new(&cfg.out, cfg.projection.name()));

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

    // Phase B: OSM, one airport at a time (public endpoints throttle parallel use).
    let mut stores: HashMap<String, Store> = HashMap::new();
    if !matches!(cfg.osm, OsmMode::Off) {
        for p in &prepared {
            let t0 = std::time::Instant::now();
            let via_api = matches!(cfg.osm, OsmMode::OsmApi);
            let result = if via_api {
                osm::osmapi::fetch(&cfg.http, &cfg.cache, &p.icao, p.bbox).or_else(|e| {
                    log::warn!("{}: OSM API failed ({e:#}); falling back to Overpass", p.icao);
                    osm::overpass::fetch(&cfg.http, &cfg.cache, &cfg.overpass_mirrors, &p.icao, p.bbox)
                })
            } else {
                osm::overpass::fetch(&cfg.http, &cfg.cache, &cfg.overpass_mirrors, &p.icao, p.bbox)
            };
            match result {
                Ok(s) => {
                    term::info(&format!("[{}] OpenStreetMap: {} nodes, {} ways in {}", p.icao, fmt_n(s.nodes.len()), fmt_n(s.ways.len()), term::human_secs(t0.elapsed().as_secs_f64())));
                    stores.insert(p.icao.clone(), s);
                }
                Err(e) => log::warn!("{}: OSM unavailable: {e:#}", p.icao),
            }
        }
    }

    // FAA tables (only when some airport is in the US).
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

    // Phase C: build + write.
    term::start(&format!("Building {} airport{}", prepared.len(), if prepared.len() == 1 { "" } else { "s" }));
    let stores = Mutex::new(stores);
    prepared.into_par_iter().for_each(|p| {
        let icao = p.icao.clone();
        let t0 = std::time::Instant::now();
        let st = stores.lock().unwrap().remove(&icao);
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
                index_file.lock().unwrap().upsert(IndexAirport {
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
                summary.lock().unwrap().built.push(icao);
            }
            Err(e) => {
                log::error!("{icao}: {e:#}");
                summary.lock().unwrap().failed.push((icao, format!("{e:#}")));
            }
        }
    });
    index_file.lock().unwrap().write(&cfg.out).context("write index")?;
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
    out.retain(|i| !f.excluded(i));
    out.sort();
    out.dedup();
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
