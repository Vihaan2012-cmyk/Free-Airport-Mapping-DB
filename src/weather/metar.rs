//! METARs: the current observation at an airport, fetched as raw text from
//! aviationweather.gov's data API and parsed group by group.
//!
//! A METAR is a fixed order of groups, but which groups appear varies with the airport and
//! the weather, so this reads it the way it has to be read: walk the tokens left to right,
//! and classify each one by its own shape rather than by its position. A token nothing
//! here recognises — a weather-phenomena group, a runway visual range, `///` for a group
//! whose value was not available — is simply skipped; `dispatch::Metar` has nowhere to put
//! most of them, and remarks (after `RMK`) are never looked at.

use super::common::{default_http, is_split_statute_miles, is_variable_wind_group, parse_cloud, parse_pressure_hpa, parse_temp_dewpoint, parse_visibility, parse_wind, resolve_ddhh, short_lived_cache, CloudToken};
use crate::dispatch::Metar;
use anyhow::{anyhow, Result};
use chrono::Utc;

/// Parse a raw METAR into its fields.
pub fn parse_metar(raw: &str) -> Result<Metar> {
    let raw = raw.trim();
    let tokens: Vec<&str> = raw.split_whitespace().map(|t| t.trim_end_matches('=')).filter(|t| !t.is_empty()).collect();
    let mut i = 0;
    while tokens.get(i).is_some_and(|t| matches!(*t, "METAR" | "SPECI" | "COR" | "AMD")) {
        i += 1;
    }
    let station = tokens.get(i).ok_or_else(|| anyhow!("empty METAR"))?.to_string();
    i += 1;

    let mut observed = None;
    if let Some(rest) = tokens.get(i).and_then(|t| t.strip_suffix('Z')) {
        if rest.len() == 6 && rest.chars().all(|c| c.is_ascii_digit()) {
            let day: u32 = rest[0..2].parse()?;
            let hour: u32 = rest[2..4].parse()?;
            let minute: u32 = rest[4..6].parse()?;
            observed = resolve_ddhh(day, hour, minute, Utc::now());
            i += 1;
        }
    }
    if tokens.get(i) == Some(&"AUTO") {
        i += 1;
    }

    let mut m = Metar { station, observed, raw: raw.to_string(), ..Metar::default() };
    let mut ceiling: Option<f64> = None;

    while i < tokens.len() {
        let tok = tokens[i];
        if tok == "RMK" {
            break;
        }
        if let Some(w) = parse_wind(tok) {
            m.wind_from_deg = w.from_deg;
            m.wind_kt = w.kt;
            m.gust_kt = w.gust_kt;
            i += 1;
            continue;
        }
        if is_variable_wind_group(tok) {
            i += 1;
            continue;
        }
        if tok == "CAVOK" {
            // Visibility at least 10 km, no cloud below 5,000 ft (or the highest minimum
            // sector altitude) and no cumulonimbus, no significant weather: `ceiling_ft`
            // simply stays unset, the same as a report with no cloud group at all.
            m.visibility_m = Some(10_000.0);
            i += 1;
            continue;
        }
        if let Some(next) = tokens.get(i + 1) {
            if is_split_statute_miles(tok, next) {
                if let Some(v) = parse_visibility(&format!("{tok} {next}")) {
                    m.visibility_m = Some(v);
                    i += 2;
                    continue;
                }
            }
        }
        if let Some(v) = parse_visibility(tok) {
            m.visibility_m = Some(v);
            i += 1;
            continue;
        }
        if is_runway_visual_range(tok) {
            i += 1;
            continue;
        }
        if let Some(c) = parse_cloud(tok) {
            match c {
                CloudToken::Clear => {}
                CloudToken::VerticalVisibility(vv) => ceiling = ceiling.or(vv),
                CloudToken::Layer { amount: "BKN" | "OVC", base_ft: Some(base) } => ceiling = Some(ceiling.map_or(base, |c: f64| c.min(base))),
                CloudToken::Layer { .. } => {}
            }
            i += 1;
            continue;
        }
        if let Some((t, d)) = parse_temp_dewpoint(tok) {
            m.temp_c = t;
            m.dewpoint_c = d;
            i += 1;
            continue;
        }
        if let Some(p) = parse_pressure_hpa(tok) {
            m.qnh_hpa = Some(p);
            i += 1;
            continue;
        }
        // A weather-phenomena group, a "///" missing group, or anything else this crate
        // has no field for.
        i += 1;
    }
    m.ceiling_ft = ceiling;
    Ok(m)
}

/// "R27L/1200", "R09/M0600V1200FT", "R27L/P2000FT": a runway visual range, which
/// `dispatch::Metar` has no field for.
fn is_runway_visual_range(tok: &str) -> bool {
    tok.len() > 3 && tok.starts_with('R') && tok.as_bytes()[1].is_ascii_digit() && tok.contains('/')
}

/// The latest report for an airport, fetched as raw text.
pub fn metar(icao: &str) -> Result<Metar> {
    let icao = icao.trim().to_uppercase();
    let http = default_http();
    let cache = short_lived_cache();
    let url = format!("https://aviationweather.gov/api/data/metar?ids={icao}&format=raw");
    let key = format!("metar/{icao}.txt");
    let text = cache.get_or_fetch_text(&key, || http.get_text(&url))?;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).ok_or_else(|| anyhow!("no METAR published for {icao}"))?;
    parse_metar(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    /// Thirty-plus real reports from round the world, including the awkward ones: fog and
    /// vertical visibility, gusting and variable wind, statute miles as a mixed fraction,
    /// CAVOK, an all-clear US report with remarks, missing groups written as slashes, and
    /// metres-per-second wind.
    const REPORTS: &[(&str, &str)] = &[
        ("EGLL", "EGLL 231250Z 24012KT 210V270 9999 FEW035 SCT250 18/12 Q1015 NOSIG"),
        ("KJFK", "KJFK 231251Z 28015G22KT 10SM FEW250 24/16 A3002 RMK AO2 SLP162 T02390161"),
        ("VABB", "VABB 231230Z 24008KT 5000 HZ SCT018 FEW300 33/26 Q1006"),
        ("LFPG", "LFPG 231230Z VRB02KT CAVOK 15/09 Q1020 NOSIG"),
        ("EDDF", "EDDF 231220Z 27006KT 240V310 9999 FEW040 12/07 Q1018 NOSIG"),
        ("RJTT", "RJTT 231200Z 09010KT 060V120 9999 FEW020 22/18 Q1012"),
        ("YSSY", "YSSY 231200Z 15012KT 9999 SCT025 BKN100 19/11 Q1019"),
        ("OMDB", "OMDB 231200Z 32006KT CAVOK 34/16 Q1001 NOSIG"),
        ("ZBAA", "ZBAA 231200Z 18003MPS 9999 FEW030 26/20 Q1005 NOSIG"),
        ("SBGR", "SBGR 231200Z 08006KT 9999 FEW030 22/17 Q1017"),
        ("CYYZ", "CYYZ 231151Z 27008KT 15SM FEW250 08/M03 A3005 RMK CI2"),
        ("FACT", "FACT 231200Z 18022G32KT 9999 FEW018 SCT300 19/13 Q1017"),
        ("NZAA", "NZAA 231200Z 23015G25KT 9999 BKN025 14/10 Q1009"),
        ("VHHH", "VHHH 231200Z 09006KT 060V120 CAVOK 29/24 Q1006"),
        ("LEMD", "LEMD 231200Z VRB02KT 9999 FEW040 21/09 Q1022 NOSIG"),
        ("EHAM", "EHAM 231225Z 25011KT 200V280 9999 SCT030 BKN045 14/09 Q1017 NOSIG"),
        ("KLAX", "KLAX 231253Z 25008KT 10SM FEW018 SCT250 18/13 A2993 RMK AO2 SLP131"),
        ("KORD", "KORD 231251Z 30016G24KT 10SM SCT045 BKN055 09/M01 A2999 RMK AO2 PK WND 30028/1218"),
        ("LOWW", "LOWW 231220Z 27009KT 240V300 9999 FEW040 SCT100 11/06 Q1019 NOSIG"),
        ("EDDM", "EDDM 231220Z 28007KT 9999 FEW045 10/05 Q1018 NOSIG"),
        ("RCTP", "RCTP 231200Z 05008KT 020V090 9999 FEW015 SCT300 26/22 Q1012"),
        ("WSSS", "WSSS 231200Z 08010KT 050V110 9999 FEW018CB SCT300 30/25 Q1009"),
        ("EGCC", "EGCC 231250Z 23015G25KT 9999 -RA FEW015 BKN025 13/11 Q1005"),
        ("LSZH", "LSZH 231220Z 26008KT 9999 SCT035 12/06 Q1017 NOSIG"),
        ("EDDB", "EDDB 231220Z VRB02KT 9999 FEW040 13/07 Q1018 NOSIG"),
        ("EKCH", "EKCH 231220Z 23012KT 9999 SCT030 BKN045 12/08 Q1013 NOSIG"),
        ("ENGM", "ENGM 231220Z 20008KT 9999 SCT025 09/05 Q1011 NOSIG"),
        ("LIRF", "LIRF 231220Z 06006KT 020V100 CAVOK 20/12 Q1019 NOSIG"),
        ("LTBA", "LTBA 231220Z 03008KT CAVOK 22/13 Q1015"),
        ("LGAV", "LGAV 231220Z 01006KT CAVOK 24/16 Q1013"),
        ("SKBO", "SKBO 231200Z 09006KT 9999 FEW035 18/09 Q1024"),
        ("MMMX", "MMMX 231200Z 05005KT 9999 FEW200 22/07 Q1023"),
        ("RPLL", "RPLL 231200Z VRB02KT 9999 FEW020 SCT300 31/25 Q1008"),
        ("PANC", "PANC 231153Z 15006KT 10SM FEW070 SCT250 M02/M09 A3021 RMK AO2 SLP262"),
        // Low visibility, vertical visibility and missing groups.
        ("KDEN", "KDEN 231253Z 32005KT 1/4SM FG VV002 03/03 A3018 RMK AO2 VIS 1/8V1/2"),
        ("KBOS", "KBOS 231254Z 00000KT 1 1/2SM BR FEW003 SCT010 09/08 A3005 RMK AO2 SLP180"),
        ("EGLL_MISSING", "EGLL 231250Z ///// SCT035 18/12 Q1015"),
        ("SKC_REPORT", "PHNL 231253Z 05012KT 10SM SKC 27/21 A3001 RMK AO2 SLP159"),
        ("NCD_REPORT", "OERK 231200Z 32010KT 9999 NCD 38/09 Q1001"),
        ("MPS_GUST", "UUEE 231200Z 25010G18MPS 4000 -SHRA BKN020CB 16/12 Q0998 NOSIG"),
        ("PLUS_SM", "KIAH 231253Z 15005KT P6SM FEW250 27/22 A3005"),
    ];

    #[test]
    fn thirty_or_more_real_metars_parse_without_error() {
        assert!(REPORTS.len() >= 30, "only {} reports", REPORTS.len());
        for (label, raw) in REPORTS {
            let m = parse_metar(raw).unwrap_or_else(|e| panic!("{label}: {e:#}\n{raw}"));
            assert!(!m.station.is_empty(), "{label}: no station");
            assert_eq!(m.raw, *raw);
        }
    }

    #[test]
    fn wind_gust_and_variable_direction() {
        let m = parse_metar("EGLL 231250Z 24012KT 210V270 9999 FEW035 SCT250 18/12 Q1015").unwrap();
        assert_eq!(m.wind_from_deg, Some(240.0));
        assert_eq!(m.wind_kt, 12.0);
        assert_eq!(m.gust_kt, None);

        let g = parse_metar("KORD 231251Z 30016G24KT 10SM SCT045 BKN055 09/M01 A2999").unwrap();
        assert_eq!(g.wind_kt, 16.0);
        assert_eq!(g.gust_kt, Some(24.0));
    }

    #[test]
    fn calm_wind_is_zero_from_no_direction() {
        let m = parse_metar("KBOS 231254Z 00000KT 1 1/2SM BR FEW003 SCT010 09/08 A3005").unwrap();
        assert_eq!(m.wind_from_deg, Some(0.0));
        assert_eq!(m.wind_kt, 0.0);
    }

    #[test]
    fn a_mixed_fraction_of_statute_miles_is_read_as_one_number() {
        let m = parse_metar("KBOS 231254Z 00000KT 1 1/2SM BR FEW003 SCT010 09/08 A3005").unwrap();
        assert!((m.visibility_m.unwrap() - 2_414.016).abs() < 0.01);
    }

    #[test]
    fn cavok_gives_ten_kilometres_and_no_ceiling() {
        let m = parse_metar("LFPG 231230Z VRB02KT CAVOK 15/09 Q1020 NOSIG").unwrap();
        assert_eq!(m.visibility_m, Some(10_000.0));
        assert_eq!(m.ceiling_ft, None);
    }

    #[test]
    fn the_ceiling_is_the_lowest_broken_or_overcast_layer() {
        let m = parse_metar("EGCC 231250Z 23015G25KT 9999 -RA FEW015 BKN025 13/11 Q1005").unwrap();
        assert_eq!(m.ceiling_ft, Some(2_500.0));
        // A scattered or few layer alone is not a ceiling.
        let clear_ish = parse_metar("VABB 231230Z 24008KT 5000 HZ SCT018 FEW300 33/26 Q1006").unwrap();
        assert_eq!(clear_ish.ceiling_ft, None);
    }

    #[test]
    fn vertical_visibility_stands_in_for_a_ceiling_when_the_sky_is_obscured() {
        let m = parse_metar("KDEN 231253Z 32005KT 1/4SM FG VV002 03/03 A3018").unwrap();
        assert_eq!(m.ceiling_ft, Some(200.0));
        assert!((m.visibility_m.unwrap() - 402.336).abs() < 0.01);
    }

    #[test]
    fn negative_temperatures_and_dewpoints_read_the_m_prefix() {
        let m = parse_metar("CYYZ 231151Z 27008KT 15SM FEW250 08/M03 A3005").unwrap();
        assert_eq!(m.temp_c, Some(8.0));
        assert_eq!(m.dewpoint_c, Some(-3.0));
    }

    #[test]
    fn pressure_reads_both_q_and_a_groups() {
        let q = parse_metar("EGLL 231250Z 24012KT 9999 FEW035 18/12 Q1015").unwrap();
        assert_eq!(q.qnh_hpa, Some(1015.0));
        let a = parse_metar("KJFK 231251Z 28015G22KT 10SM FEW250 24/16 A3002").unwrap();
        assert!((a.qnh_hpa.unwrap() - 1016.594).abs() < 0.01);
    }

    #[test]
    fn a_missing_wind_group_written_as_slashes_does_not_stop_the_rest_parsing() {
        let m = parse_metar("EGLL 231250Z ///// SCT035 18/12 Q1015").unwrap();
        assert_eq!(m.wind_from_deg, None);
        assert_eq!(m.wind_kt, 0.0);
        assert_eq!(m.temp_c, Some(18.0));
        assert_eq!(m.qnh_hpa, Some(1015.0));
    }

    #[test]
    fn sky_clear_and_no_cloud_detected_leave_no_ceiling() {
        assert_eq!(parse_metar("PHNL 231253Z 05012KT 10SM SKC 27/21 A3001").unwrap().ceiling_ft, None);
        assert_eq!(parse_metar("OERK 231200Z 32010KT 9999 NCD 38/09 Q1001").unwrap().ceiling_ft, None);
    }

    #[test]
    fn metres_per_second_wind_converts_to_knots() {
        let m = parse_metar("UUEE 231200Z 25010G18MPS 4000 -SHRA BKN020CB 16/12 Q0998").unwrap();
        assert!((m.wind_kt - 19.43845).abs() < 0.001);
        assert!((m.gust_kt.unwrap() - 34.98921).abs() < 0.001);
        assert_eq!(m.ceiling_ft, Some(2_000.0));
    }

    #[test]
    fn runway_visual_range_is_skipped_without_upsetting_what_follows() {
        let m = parse_metar("KDEN 231253Z 32005KT 1/4SM R35L/2600FT FG VV002 03/03 A3018").unwrap();
        assert_eq!(m.temp_c, Some(3.0));
        assert_eq!(m.qnh_hpa.is_some(), true);
    }

    #[test]
    fn plus_six_statute_miles_is_the_at_least_reading() {
        let m = parse_metar("KIAH 231253Z 15005KT P6SM FEW250 27/22 A3005").unwrap();
        assert!((m.visibility_m.unwrap() - 9_656.064).abs() < 0.01);
    }

    #[test]
    fn observed_time_resolves_against_now() {
        let m = parse_metar("EGLL 231250Z 24012KT 9999 FEW035 18/12 Q1015").unwrap();
        let observed = m.observed.expect("a time");
        assert_eq!(observed.day(), 23);
        assert_eq!(observed.hour(), 12);
        assert_eq!(observed.minute(), 50);
    }

    /// The live METAR for three airports round the world. Not run by default: it needs
    /// the network and aviationweather.gov's own availability.
    #[test]
    #[ignore]
    fn real_metars_for_egll_kjfk_vabb() {
        for icao in ["EGLL", "KJFK", "VABB"] {
            match metar(icao) {
                Ok(m) => println!("{icao}: {m:?}"),
                Err(e) => println!("{icao}: {e:#}"),
            }
        }
    }
}
