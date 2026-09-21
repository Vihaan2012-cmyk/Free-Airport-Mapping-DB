//! FAA NASR 28-day subscription (public domain, US only): arresting systems, LAHSO,
//! declared distances and stopways from the CSV distribution.
//!
//! Column names follow the NASR CSV layout; every lookup is by header name and
//! tolerant of missing columns so a layout change degrades to "no data" rather than
//! a failure.

use crate::cache::Cache;
use crate::ir::*;
use crate::model::codes::source;
use crate::sources::http::Http;
use anyhow::{anyhow, Context, Result};
use chrono::{Datelike, NaiveDate, Utc};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// A known NASR effective date; cycles repeat every 28 days.
const EPOCH: (i32, u32, u32) = (2024, 1, 25);

pub fn current_cycle_date(today: NaiveDate) -> NaiveDate {
    let epoch = NaiveDate::from_ymd_opt(EPOCH.0, EPOCH.1, EPOCH.2).unwrap();
    let days = (today - epoch).num_days();
    let cycles = days.div_euclid(28);
    epoch + chrono::Duration::days(cycles * 28)
}

pub fn cycle_url(d: NaiveDate) -> String {
    let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(d.month() - 1) as usize];
    format!("https://nfdc.faa.gov/webContent/28DaySub/extra/{:02}_{}_{}_CSV.zip", d.day(), mon, d.year())
}

/// The three CSV tables we use, as raw text.
#[derive(Debug, Default, Clone)]
pub struct NasrTables {
    pub apt_base: String,
    pub apt_rwy_end: String,
    pub apt_ars: String,
}

fn read_member(z: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>, name: &str) -> Option<String> {
    let idx = (0..z.len()).find(|i| z.by_index(*i).ok().map_or(false, |f| f.name().to_ascii_uppercase().ends_with(&name.to_ascii_uppercase())))?;
    let mut f = z.by_index(idx).ok()?;
    let mut b = Vec::new();
    f.read_to_end(&mut b).ok()?;
    Some(String::from_utf8_lossy(&b).into_owned())
}

pub fn tables_from_zip(bytes: Vec<u8>) -> Result<NasrTables> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("open NASR zip")?;
    let apt_base = read_member(&mut z, "APT_BASE.csv").ok_or_else(|| anyhow!("APT_BASE.csv missing"))?;
    let apt_rwy_end = read_member(&mut z, "APT_RWY_END.csv").unwrap_or_default();
    let apt_ars = read_member(&mut z, "APT_ARS.csv").unwrap_or_default();
    Ok(NasrTables { apt_base, apt_rwy_end, apt_ars })
}

/// Load NASR tables from a local zip/directory or download the current cycle.
pub fn load_tables(http: &Http, cache: &Cache, local: Option<&Path>) -> Result<NasrTables> {
    if let Some(p) = local {
        if p.is_dir() {
            let rd = |n: &str| std::fs::read_to_string(p.join(n)).unwrap_or_default();
            return Ok(NasrTables { apt_base: rd("APT_BASE.csv"), apt_rwy_end: rd("APT_RWY_END.csv"), apt_ars: rd("APT_ARS.csv") });
        }
        return tables_from_zip(std::fs::read(p).with_context(|| format!("read {}", p.display()))?);
    }
    let cycle = current_cycle_date(Utc::now().date_naive());
    let key = format!("faa/nasr_{}", cycle);
    let sibling: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);
    let base = cache.get_or_fetch_text(&format!("{key}/APT_BASE.csv"), || {
        let bytes = http.get_bytes(&cycle_url(cycle))?;
        let t = tables_from_zip(bytes)?;
        // Store the siblings alongside when a cache dir exists; the closure only returns APT_BASE.
        if let Some(dir) = cache.path(&key) {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("APT_RWY_END.csv"), &t.apt_rwy_end)?;
            std::fs::write(dir.join("APT_ARS.csv"), &t.apt_ars)?;
        } else {
            *sibling.lock().unwrap() = Some((t.apt_rwy_end.clone(), t.apt_ars.clone()));
        }
        Ok(t.apt_base)
    })?;
    if let Some((rwy_end, ars)) = sibling.lock().unwrap().take() {
        return Ok(NasrTables { apt_base: base, apt_rwy_end: rwy_end, apt_ars: ars });
    }
    let rd = |n: &str| cache.path(&format!("{key}/{n}")).and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    Ok(NasrTables { apt_base: base, apt_rwy_end: rd("APT_RWY_END.csv"), apt_ars: rd("APT_ARS.csv") })
}

struct Table {
    headers: Vec<String>,
    rows: Vec<csv::StringRecord>,
}

impl Table {
    fn parse(text: &str) -> Table {
        let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(text.as_bytes());
        let headers = rdr.headers().map(|h| h.iter().map(|s| s.trim().to_uppercase()).collect()).unwrap_or_default();
        let rows = rdr.records().filter_map(|r| r.ok()).collect();
        Table { headers, rows }
    }
    fn col(&self, name: &str) -> Option<usize> {
        self.headers.iter().position(|h| h == name)
    }
    fn get<'a>(&self, row: &'a csv::StringRecord, name: &str) -> Option<&'a str> {
        self.col(name).and_then(|i| row.get(i)).map(str::trim).filter(|s| !s.is_empty())
    }
}

/// Per-runway-end enrichment for one airport.
#[derive(Debug, Default, Clone)]
pub struct EndInfo {
    pub tora_m: Option<f64>,
    pub toda_m: Option<f64>,
    pub asda_m: Option<f64>,
    pub lda_m: Option<f64>,
    pub stopway_m: Option<f64>,
    pub displaced_m: Option<f64>,
    pub tdz_elev_ft: Option<f64>,
}

#[derive(Debug, Default, Clone)]
pub struct NasrAirport {
    pub faa_id: String,
    pub ends: HashMap<String, EndInfo>,
    pub lahso: Vec<Lahso>,
    pub arresting: Vec<ArrestingSystem>,
}

fn norm_end(s: &str) -> String {
    let s = s.trim().to_uppercase();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    let suffix: String = s.chars().skip_while(|c| c.is_ascii_digit()).collect();
    match digits.parse::<u32>() {
        Ok(n) => format!("{n:02}{suffix}"),
        Err(_) => s,
    }
}

const FT: f64 = 0.3048;

/// Every surveyed touchdown zone elevation the United States publishes, by ICAO and
/// then by runway end.
///
/// A minimum is measured from the touchdown zone, and a foot of error there is a foot of
/// error in the minimum. The terrain model is what we have everywhere else; here there
/// is a survey, and it is in a file we already read for declared distances.
pub fn touchdown_zone_elevations(tables: &NasrTables) -> HashMap<String, HashMap<String, f64>> {
    let base = Table::parse(&tables.apt_base);
    let mut icao_of: HashMap<String, String> = HashMap::new();
    for r in &base.rows {
        if let (Some(site), Some(icao)) = (base.get(r, "SITE_NO"), base.get(r, "ICAO_ID")) {
            icao_of.insert(site.to_string(), icao.to_uppercase());
        }
    }
    let ends = Table::parse(&tables.apt_rwy_end);
    let mut out: HashMap<String, HashMap<String, f64>> = HashMap::new();
    for r in &ends.rows {
        let (Some(site), Some(end_id)) = (ends.get(r, "SITE_NO"), ends.get(r, "RWY_END_ID")) else { continue };
        let Some(icao) = icao_of.get(site) else { continue };
        // The touchdown zone elevation where it is surveyed, else the runway end's own.
        let Some(ft) = ends
            .get(r, "TDZ_ELEV")
            .and_then(|v| v.parse::<f64>().ok())
            .or_else(|| ends.get(r, "RWY_END_ELEV").and_then(|v| v.parse::<f64>().ok()))
        else {
            continue;
        };
        out.entry(icao.clone()).or_default().insert(norm_end(end_id), ft);
    }
    out
}

/// Extract everything NASR knows about `icao`.
pub fn lookup(tables: &NasrTables, icao: &str) -> Option<NasrAirport> {
    let base = Table::parse(&tables.apt_base);
    let row = base.rows.iter().find(|r| base.get(r, "ICAO_ID").map_or(false, |v| v.eq_ignore_ascii_case(icao)))?;
    let site = base.get(row, "SITE_NO")?.to_string();
    let faa_id = base.get(row, "ARPT_ID").unwrap_or("").to_string();
    let mut out = NasrAirport { faa_id, ..Default::default() };

    let ends = Table::parse(&tables.apt_rwy_end);
    for r in ends.rows.iter().filter(|r| ends.get(r, "SITE_NO") == Some(site.as_str())) {
        let Some(end_id) = ends.get(r, "RWY_END_ID") else { continue };
        let pf = |n: &str| ends.get(r, n).and_then(|v| v.parse::<f64>().ok());
        let tora = pf("TKOF_RUN_AVBL").map(|v| v * FT);
        let asda = pf("ACLT_STOP_DIST_AVBL").map(|v| v * FT);
        let info = EndInfo {
            tora_m: tora,
            toda_m: pf("TKOF_DIST_AVBL").map(|v| v * FT),
            asda_m: asda,
            lda_m: pf("LNDG_DIST_AVBL").map(|v| v * FT),
            stopway_m: match (asda, tora) {
                (Some(a), Some(t)) if a > t + 1.0 => Some(a - t),
                _ => None,
            },
            displaced_m: pf("DISPLACED_THR_LEN").map(|v| v * FT),
            tdz_elev_ft: pf("TDZ_ELEV"),
        };
        out.ends.insert(norm_end(end_id), info);
        if let Some(ald) = pf("LAHSO_ALD") {
            if ald > 0.0 {
                out.lahso.push(Lahso {
                    runway_end: norm_end(end_id),
                    available_m: ald * FT,
                    intersecting: ends.get(r, "RWY_END_INTERSECT_LAHSO").map(norm_end).or_else(|| ends.get(r, "LAHSO_DESC").map(str::to_string)),
                    source: source::FAA_NASR,
                });
            }
        }
    }

    let ars = Table::parse(&tables.apt_ars);
    for r in ars.rows.iter().filter(|r| ars.get(r, "SITE_NO") == Some(site.as_str())) {
        let (Some(end_id), Some(dev)) = (ars.get(r, "RWY_END_ID"), ars.get(r, "ARREST_DEVICE_CODE")) else { continue };
        out.arresting.push(ArrestingSystem { runway_end: norm_end(end_id), kind: dev.to_string(), distance_m: None, source: source::FAA_NASR });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_dates_step_by_28_days() {
        let d = current_cycle_date(NaiveDate::from_ymd_opt(2024, 2, 1).unwrap());
        assert_eq!(d, NaiveDate::from_ymd_opt(2024, 1, 25).unwrap());
        let d2 = current_cycle_date(NaiveDate::from_ymd_opt(2024, 2, 22).unwrap());
        assert_eq!(d2, NaiveDate::from_ymd_opt(2024, 2, 22).unwrap());
        assert_eq!(cycle_url(d2), "https://nfdc.faa.gov/webContent/28DaySub/extra/22_Feb_2024_CSV.zip");
    }

    #[test]
    fn looks_up_airport_by_icao() {
        let t = NasrTables {
            apt_base: "EFF_DATE,SITE_NO,ARPT_ID,ICAO_ID,ARPT_NAME\n2024/01/25,12345.*A,SEA,KSEA,SEATTLE\n".into(),
            apt_rwy_end: "SITE_NO,RWY_ID,RWY_END_ID,TKOF_RUN_AVBL,TKOF_DIST_AVBL,ACLT_STOP_DIST_AVBL,LNDG_DIST_AVBL,LAHSO_ALD,RWY_END_INTERSECT_LAHSO,DISPLACED_THR_LEN\n12345.*A,16L/34R,16L,11901,11901,12401,11901,8000,34C,\n12345.*A,16L/34R,34R,11901,11901,11901,11901,,,\n".into(),
            apt_ars: "SITE_NO,RWY_ID,RWY_END_ID,ARREST_DEVICE_CODE\n12345.*A,16L/34R,34R,BAK-12\n".into(),
        };
        let a = lookup(&t, "KSEA").unwrap();
        assert_eq!(a.faa_id, "SEA");
        let e = &a.ends["16L"];
        assert!((e.stopway_m.unwrap() - 500.0 * FT).abs() < 1e-6);
        assert!(a.ends["34R"].stopway_m.is_none());
        assert_eq!(a.lahso.len(), 1);
        assert_eq!(a.lahso[0].intersecting.as_deref(), Some("34C"));
        assert_eq!(a.arresting[0].kind, "BAK-12");
        assert!(lookup(&t, "KZZZ").is_none());
    }
}
