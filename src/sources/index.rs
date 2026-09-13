//! Airport index: ICAO -> ARP / elevation / names, from X-Plane `earth_aptmeta.dat`
//! (local), OurAirports CSV (public domain download), and the Gateway airport list.

use crate::cache::Cache;
use crate::ir::AirportHeader;
use crate::sources::http::Http;
use anyhow::{Context, Result};
use geo_types::Coord;
use std::collections::HashMap;
use std::path::Path;

pub const OURAIRPORTS_AIRPORTS: &str = "https://davidmegginson.github.io/ourairports-data/airports.csv";
pub const OURAIRPORTS_RUNWAYS: &str = "https://davidmegginson.github.io/ourairports-data/runways.csv";

#[derive(Debug, Clone, Default)]
pub struct IndexEntry {
    pub icao: String,
    pub iata: Option<String>,
    pub name: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: Option<f64>,
    pub transition_alt_ft: Option<f64>,
    pub transition_level: Option<String>,
    pub kind: Option<String>,
    /// OurAirports continent code: AF, AN, AS, EU, NA, OC, SA.
    pub continent: Option<String>,
    /// Recommended Gateway scenery id, when the Gateway list has been loaded.
    pub gateway_scenery: Option<i64>,
    pub source: &'static str,
}

impl IndexEntry {
    pub fn header(&self) -> AirportHeader {
        AirportHeader {
            icao: self.icao.clone(),
            iata: self.iata.clone(),
            faa: None,
            name: self.name.clone(),
            city: self.city.clone(),
            country: self.country.clone(),
            region: self.region.clone(),
            arp: Some(Coord { x: self.lon, y: self.lat }),
            elevation_ft: self.elevation_ft,
            transition_alt_ft: self.transition_alt_ft,
            transition_level: self.transition_level.clone(),
            mag_var: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct AirportIndex {
    pub by_icao: HashMap<String, IndexEntry>,
    /// OurAirports runway rows keyed by ICAO (ident) for cross-checks / fallback.
    pub runways: HashMap<String, Vec<OaRunway>>,
}

#[derive(Debug, Clone)]
pub struct OaRunway {
    pub length_ft: Option<f64>,
    pub width_ft: Option<f64>,
    pub surface: Option<String>,
    pub closed: bool,
    pub le_ident: String,
    pub le_lat: Option<f64>,
    pub le_lon: Option<f64>,
    pub le_displaced_ft: Option<f64>,
    pub he_ident: String,
    pub he_lat: Option<f64>,
    pub he_lon: Option<f64>,
    pub he_displaced_ft: Option<f64>,
}

impl AirportIndex {
    pub fn get(&self, icao: &str) -> Option<&IndexEntry> {
        self.by_icao.get(&icao.to_uppercase())
    }

    /// Fill only missing entries (first-loaded source wins).
    fn insert_if_absent(&mut self, e: IndexEntry) {
        self.by_icao.entry(e.icao.clone()).or_insert(e);
    }

    /// Load X-Plane `earth_aptmeta.dat` (columns: ICAO region lat lon elev_ft class
    /// longest_rwy_ft flags transition_alt transition_level).
    pub fn load_aptmeta(&mut self, path: &Path) -> Result<usize> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let mut n = 0;
        for line in text.lines().skip(2) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 5 || !f[0].chars().all(|c| c.is_ascii_alphanumeric()) {
                continue;
            }
            let (Ok(lat), Ok(lon)) = (f[2].parse::<f64>(), f[3].parse::<f64>()) else { continue };
            let ta = f.get(8).and_then(|s| s.parse::<f64>().ok()).filter(|v| *v > 0.0);
            let tl = f.get(9).map(|s| s.to_string()).filter(|s| s != "-1");
            self.insert_if_absent(IndexEntry {
                icao: f[0].to_uppercase(),
                region: Some(f[1].to_string()),
                lat,
                lon,
                elevation_ft: f[4].parse().ok(),
                transition_alt_ft: ta,
                transition_level: tl,
                kind: f.get(5).map(|s| s.to_string()),
                source: crate::model::codes::source::APTMETA,
                ..Default::default()
            });
            n += 1;
        }
        Ok(n)
    }

    /// Load OurAirports `airports.csv` (and `runways.csv` if given).
    pub fn load_ourairports(&mut self, airports_csv: &str, runways_csv: Option<&str>) -> Result<usize> {
        let mut n = 0;
        let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(airports_csv.as_bytes());
        let headers = rdr.headers()?.clone();
        let col = |name: &str| headers.iter().position(|h| h == name);
        let (c_ident, c_type, c_name, c_lat, c_lon, c_elev, c_iso, c_region, c_city, c_gps, c_iata, c_icao) = (
            col("ident"), col("type"), col("name"), col("latitude_deg"), col("longitude_deg"), col("elevation_ft"),
            col("iso_country"), col("iso_region"), col("municipality"), col("gps_code"), col("iata_code"), col("icao_code"),
        );
        let c_continent = col("continent");
        for rec in rdr.records() {
            let rec = rec?;
            let g = |c: Option<usize>| c.and_then(|i| rec.get(i)).map(|s| s.trim()).filter(|s| !s.is_empty());
            let icao = g(c_icao).or_else(|| g(c_gps)).or_else(|| g(c_ident));
            let Some(icao) = icao else { continue };
            if icao.len() != 4 || !icao.chars().all(|c| c.is_ascii_alphanumeric()) {
                continue;
            }
            let (Some(lat), Some(lon)) = (g(c_lat).and_then(|s| s.parse().ok()), g(c_lon).and_then(|s| s.parse().ok())) else { continue };
            let e = IndexEntry {
                icao: icao.to_uppercase(),
                iata: g(c_iata).map(str::to_string),
                name: g(c_name).map(str::to_string),
                region: g(c_region).map(str::to_string),
                country: g(c_iso).map(str::to_string),
                city: g(c_city).map(str::to_string),
                lat,
                lon,
                elevation_ft: g(c_elev).and_then(|s| s.parse().ok()),
                kind: g(c_type).map(str::to_string),
                continent: g(c_continent).map(str::to_uppercase),
                source: crate::model::codes::source::OURAIRPORTS,
                ..Default::default()
            };
            // OurAirports has names/IATA that aptmeta lacks: merge into an existing entry.
            match self.by_icao.get_mut(&e.icao) {
                Some(existing) => {
                    if existing.continent.is_none() { existing.continent = e.continent; }
                    if existing.name.is_none() { existing.name = e.name; }
                    if existing.iata.is_none() { existing.iata = e.iata; }
                    if existing.country.is_none() { existing.country = e.country; }
                    if existing.city.is_none() { existing.city = e.city; }
                    if existing.elevation_ft.is_none() { existing.elevation_ft = e.elevation_ft; }
                }
                None => { self.by_icao.insert(e.icao.clone(), e); }
            }
            n += 1;
        }
        if let Some(rcsv) = runways_csv {
            let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(rcsv.as_bytes());
            let headers = rdr.headers()?.clone();
            let col = |name: &str| headers.iter().position(|h| h == name);
            let cols: Vec<Option<usize>> = ["airport_ident", "length_ft", "width_ft", "surface", "closed", "le_ident", "le_latitude_deg", "le_longitude_deg", "le_displaced_threshold_ft", "he_ident", "he_latitude_deg", "he_longitude_deg", "he_displaced_threshold_ft"].iter().map(|c| col(c)).collect();
            for rec in rdr.records() {
                let rec = rec?;
                let g = |k: usize| cols[k].and_then(|i| rec.get(i)).map(|s| s.trim()).filter(|s| !s.is_empty());
                let Some(ident) = g(0) else { continue };
                let pf = |k: usize| g(k).and_then(|s| s.parse::<f64>().ok());
                self.runways.entry(ident.to_uppercase()).or_default().push(OaRunway {
                    length_ft: pf(1),
                    width_ft: pf(2),
                    surface: g(3).map(str::to_string),
                    closed: g(4) == Some("1"),
                    le_ident: g(5).unwrap_or("").to_string(),
                    le_lat: pf(6),
                    le_lon: pf(7),
                    le_displaced_ft: pf(8),
                    he_ident: g(9).unwrap_or("").to_string(),
                    he_lat: pf(10),
                    he_lon: pf(11),
                    he_displaced_ft: pf(12),
                });
            }
        }
        Ok(n)
    }

    /// Load the Gateway airport list (all ~41k airports with their recommended scenery id).
    pub fn load_gateway_list(&mut self, http: &Http, cache: &Cache) -> Result<usize> {
        let text = cache.get_or_fetch_text("gateway/airports.json", || http.get_text("https://gateway.x-plane.com/apiv1/airports"))?;
        let v: serde_json::Value = serde_json::from_str(&text).context("gateway airports json")?;
        let mut n = 0;
        for a in v.get("airports").and_then(|x| x.as_array()).map(|x| x.as_slice()).unwrap_or(&[]) {
            let Some(code) = a.get("AirportCode").and_then(|c| c.as_str()) else { continue };
            let icao = code.to_uppercase();
            let scenery = a.get("RecommendedSceneryId").and_then(|s| s.as_i64());
            let (lat, lon) = (a.get("Latitude").and_then(|x| x.as_f64()), a.get("Longitude").and_then(|x| x.as_f64()));
            match self.by_icao.get_mut(&icao) {
                Some(e) => e.gateway_scenery = scenery,
                None => {
                    let (Some(lat), Some(lon)) = (lat, lon) else { continue };
                    self.by_icao.insert(icao.clone(), IndexEntry { icao, name: a.get("AirportName").and_then(|x| x.as_str()).map(str::to_string), lat, lon, elevation_ft: a.get("Elevation").and_then(|x| x.as_f64()), gateway_scenery: scenery, source: crate::model::codes::source::XPLANE, ..Default::default() });
                }
            }
            n += 1;
        }
        Ok(n)
    }

    /// Download OurAirports CSVs through the cache and load them.
    pub fn load_ourairports_online(&mut self, http: &Http, cache: &Cache) -> Result<usize> {
        let a = cache.get_or_fetch_text("ourairports/airports.csv", || http.get_text(OURAIRPORTS_AIRPORTS))?;
        let r = cache.get_or_fetch_text("ourairports/runways.csv", || http.get_text(OURAIRPORTS_RUNWAYS)).ok();
        self.load_ourairports(&a, r.as_deref())
    }

    /// Fallback: ask the Gateway for one airport's ARP.
    pub fn load_gateway_single(&mut self, http: &Http, cache: &Cache, icao: &str) -> Result<bool> {
        let info = crate::sources::xplane::gateway::airport_info(http, cache, icao)?;
        if let (Some(lat), Some(lon)) = (info.latitude, info.longitude) {
            self.insert_if_absent(IndexEntry {
                icao: info.icao.to_uppercase(),
                name: info.airport_name,
                lat,
                lon,
                elevation_ft: info.elevation,
                source: crate::model::codes::source::XPLANE,
                ..Default::default()
            });
            return Ok(true);
        }
        Ok(false)
    }

    /// ICAOs whose index entry matches a country (ISO2) or an aptmeta region prefix.
    pub fn icaos_matching(&self, country: Option<&str>, region_prefix: Option<&str>, icao_prefix: Option<&str>) -> Vec<String> {
        let mut v: Vec<String> = self
            .by_icao
            .values()
            .filter(|e| country.map_or(true, |c| e.country.as_deref().map_or(false, |x| x.eq_ignore_ascii_case(c))))
            .filter(|e| region_prefix.map_or(true, |r| e.region.as_deref().map_or(false, |x| x.to_uppercase().starts_with(&r.to_uppercase()))))
            .filter(|e| icao_prefix.map_or(true, |p| e.icao.starts_with(&p.to_uppercase())))
            .map(|e| e.icao.clone())
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aptmeta_and_ourairports() {
        let dir = std::env::temp_dir().join(format!("amdbgen-index-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("earth_aptmeta.dat");
        std::fs::write(&p, "I\n1210 Version\n\nVIDP VI  28.566500000   77.103100000   777 C 14534 0 18000 FL180\n 00C K2  37.203177778 -107.869194444  6684 C  5000 0    -1    -1\n").unwrap();
        let mut idx = AirportIndex::default();
        assert_eq!(idx.load_aptmeta(&p).unwrap(), 2);
        let e = idx.get("vidp").unwrap();
        assert!((e.lat - 28.5665).abs() < 1e-9);
        assert_eq!(e.transition_level.as_deref(), Some("FL180"));
        assert!(idx.get("00C").unwrap().transition_alt_ft.is_none());

        let csv = "id,ident,type,name,latitude_deg,longitude_deg,elevation_ft,continent,iso_country,iso_region,municipality,scheduled_service,icao_code,iata_code,gps_code,local_code\n1,VIDP,large_airport,\"Indira Gandhi International Airport\",28.5665,77.1031,777,AS,IN,IN-DL,New Delhi,yes,VIDP,DEL,VIDP,\n2,VABB,large_airport,Mumbai,19.0887,72.8679,39,AS,IN,IN-MH,Mumbai,yes,VABB,BOM,VABB,\n";
        let rws = "id,airport_ref,airport_ident,length_ft,width_ft,surface,lighted,closed,le_ident,le_latitude_deg,le_longitude_deg,le_elevation_ft,le_heading_degT,le_displaced_threshold_ft,he_ident,he_latitude_deg,he_longitude_deg,he_elevation_ft,he_heading_degT,he_displaced_threshold_ft\n1,1,VIDP,14534,150,ASP,1,0,10,28.55,77.08,,,0,28,28.57,77.12,,,1000\n";
        assert_eq!(idx.load_ourairports(csv, Some(rws)).unwrap(), 2);
        let e = idx.get("VIDP").unwrap();
        assert_eq!(e.iata.as_deref(), Some("DEL"));
        assert_eq!(e.region.as_deref(), Some("VI")); // aptmeta loaded first keeps its region
        assert_eq!(idx.get("VABB").unwrap().country.as_deref(), Some("IN"));
        assert_eq!(idx.runways["VIDP"][0].he_displaced_ft, Some(1000.0));
        assert_eq!(idx.icaos_matching(Some("IN"), None, None), vec!["VABB", "VIDP"]);
        assert_eq!(idx.icaos_matching(None, None, Some("VI")), vec!["VIDP"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
