//! Airport data store for the bridge: loads generated airports from `out/`, builds
//! missing ones on demand with the amdbgen pipeline, keeps them in memory in the
//! local metre frame, and knows the airport list for the search endpoint.

use crate::geom::LocalFrame;
use crate::model::{AmdbFeature, Layer, ALL_LAYERS};
use crate::output::manifest::Manifest;
use crate::pipeline::{self, Config};
use crate::sources::index::AirportIndex;
use anyhow::{anyhow, Context, Result};
use geo_types::{Coord, Geometry, LineString, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct AirportData {
    pub icao: String,
    pub frame: LocalFrame,
    pub manifest: Manifest,
    /// Features in the local metre frame, already converted to client conventions.
    pub layers: BTreeMap<Layer, Vec<AmdbFeature>>,
}

/// What happens to an airport's files after it has been loaded into memory.
#[derive(Debug, Clone)]
pub enum Retention {
    /// Keep everything (the user manages the folder).
    KeepAll,
    /// Delete the folder once loaded (caching disabled).
    Ephemeral,
    /// Keep, but prune the cache folder to the configured limit.
    Limit(super::settings::Settings),
}

pub struct Store {
    pub cfg: Config,
    pub out: PathBuf,
    pub retention: Retention,
    index: AirportIndex,
    loaded: Mutex<HashMap<String, Arc<AirportData>>>,
    building: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

fn project_to_local(frame: &LocalFrame, g: &Geometry<f64>) -> Geometry<f64> {
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

impl Store {
    pub fn new(cfg: Config) -> Result<Store> {
        let index = pipeline::load_index(&cfg)?;
        Ok(Store { out: cfg.out.clone(), cfg, index, retention: Retention::KeepAll, loaded: Mutex::new(HashMap::new()), building: Mutex::new(HashMap::new()) })
    }

    /// Airports offered to the client: everything already generated plus every
    /// large/medium airport in the index. Rows follow the SDK `AmdbSearchResponse`;
    /// the query is a prefix match on `idarpt`, `iata` or `name`, as documented.
    pub fn search(&self, q: &str) -> Vec<Value> {
        let q = q.trim().to_uppercase();
        let mut out: HashMap<String, (String, Option<String>, String, f64, f64, Option<f64>)> = HashMap::new();
        for e in self.index.by_icao.values() {
            let big = matches!(e.kind.as_deref(), Some("large_airport") | Some("medium_airport"));
            if !big && !self.out.join(&e.icao).join("manifest.json").is_file() {
                continue;
            }
            out.insert(e.icao.clone(), (e.icao.clone(), e.iata.clone(), e.name.clone().unwrap_or_default(), e.lat, e.lon, e.elevation_ft));
        }
        // Generated airports not in the index (e.g. built from a local apt.dat).
        if let Ok(rd) = std::fs::read_dir(&self.out) {
            for d in rd.flatten() {
                let icao = d.file_name().to_string_lossy().to_uppercase();
                if out.contains_key(&icao) || !d.path().join("manifest.json").is_file() {
                    continue;
                }
                if let Ok(m) = std::fs::read_to_string(d.path().join("manifest.json")).and_then(|t| serde_json::from_str::<Manifest>(&t).map_err(std::io::Error::other)) {
                    out.insert(icao.clone(), (icao, m.iata.clone(), m.name.clone().unwrap_or_default(), m.arp[0], m.arp[1], m.elevation_ft));
                }
            }
        }
        let mut rows: Vec<(String, Option<String>, String, f64, f64, Option<f64>)> = out
            .into_values()
            .filter(|(idarpt, iata, name, _, _, _)| q.is_empty() || idarpt.starts_with(&q) || iata.as_deref().map_or(false, |i| i.to_uppercase().starts_with(&q)) || name.to_uppercase().starts_with(&q))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows.into_iter().map(|(idarpt, iata, name, lat, lon, elev)| super::compat::search_row(&idarpt, iata.as_deref(), &name, lat, lon, elev)).collect()
    }

    /// Get (load or build) an airport.
    pub fn airport(&self, icao: &str) -> Result<Arc<AirportData>> {
        let icao = icao.to_uppercase();
        if let Some(a) = self.loaded.lock().unwrap().get(&icao) {
            return Ok(a.clone());
        }
        // One build per airport at a time.
        let gate = self.building.lock().unwrap().entry(icao.clone()).or_insert_with(|| Arc::new(Mutex::new(()))).clone();
        let _g = gate.lock().unwrap();
        if let Some(a) = self.loaded.lock().unwrap().get(&icao) {
            return Ok(a.clone());
        }
        let dir = self.out.join(&icao);
        // A bulk worker may be on this airport right now: wait for it rather than
        // building the same folder twice.
        if pipeline::is_building(&icao) {
            crate::term::info(&format!("[{icao}] Bulk build is already working on {icao}; waiting for it"));
            let t0 = std::time::Instant::now();
            while pipeline::is_building(&icao) && t0.elapsed().as_secs() < 900 {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        if !dir.join("manifest.json").is_file() {
            crate::term::start(&format!("[{icao}] First request for {icao}: building it now"));
            let summary = pipeline::run(&self.cfg, &[icao.clone()])?;
            if let Some((_, e)) = summary.failed.first() {
                return Err(anyhow!("{icao}: build failed: {e}"));
            }
        }
        let data = Arc::new(self.load_dir(&icao)?);
        match &self.retention {
            Retention::KeepAll => {}
            Retention::Ephemeral => {
                let _ = std::fs::remove_dir_all(&dir);
            }
            Retention::Limit(s) => {
                let removed = super::settings::prune(s, &icao);
                if !removed.is_empty() {
                    crate::term::info(&format!("Cache over its {} MB limit: removed {}", s.limit_mb, removed.join(" ")));
                }
            }
        }
        self.loaded.lock().unwrap().insert(icao, data.clone());
        Ok(data)
    }

    fn load_dir(&self, icao: &str) -> Result<AirportData> {
        let dir = self.out.join(icao);
        let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).context("manifest")?)?;
        let frame = LocalFrame::new(manifest.arp[0], manifest.arp[1]);
        let is_wgs84 = manifest.projection.starts_with("EPSG");
        let mut layers = BTreeMap::new();
        for l in ALL_LAYERS {
            let p = dir.join(format!("{}.geojson", l.name()));
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let (_, feats) = crate::output::geojson::parse_feature_collection(&text, *l)?;
            let mut kept = Vec::with_capacity(feats.len());
            for (i, mut f) in feats.into_iter().enumerate() {
                if is_wgs84 {
                    f.geom = project_to_local(&frame, &f.geom);
                }
                // Layers Navigraph does not serve are dropped here.
                if super::compat::convert(&mut f, i) {
                    kept.push(f);
                }
            }
            layers.insert(*l, kept);
        }
        Ok(AirportData { icao: icao.to_string(), frame, manifest, layers })
    }

    /// Airports around a point, nearest first: index airports (large/medium/small) plus
    /// anything already generated. Not part of the Navigraph API; used by ported
    /// moving maps whose sim-side airport search is unavailable.
    pub fn nearest(&self, lat: f64, lon: f64, radius_km: f64, limit: usize) -> Vec<Value> {
        let mut rows: Vec<(f64, Value)> = self
            .index
            .by_icao
            .values()
            .filter(|e| matches!(e.kind.as_deref(), Some("large_airport") | Some("medium_airport") | Some("small_airport")) || self.out.join(&e.icao).join("manifest.json").is_file())
            .filter_map(|e| {
                let d = pipeline::haversine_km(lat, lon, e.lat, e.lon);
                if d > radius_km {
                    return None;
                }
                let mut row = super::compat::search_row(&e.icao, e.iata.as_deref(), e.name.as_deref().unwrap_or(""), e.lat, e.lon, e.elevation_ft);
                row["distance_nm"] = Value::from((d / 1.852 * 100.0).round() / 100.0);
                row["kind"] = Value::from(e.kind.clone().unwrap_or_default());
                Some((d, row))
            })
            .collect();
        rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        rows.into_iter().take(limit).map(|(_, r)| r).collect()
    }

    pub fn loaded_count(&self) -> usize {
        self.loaded.lock().unwrap().len()
    }
}
