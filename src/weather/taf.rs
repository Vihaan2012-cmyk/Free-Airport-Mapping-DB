//! TAFs: the forecast for an airport, fetched as raw text and parsed into its overlapping
//! periods.
//!
//! A TAF is a base forecast for its whole validity period, followed by groups that change
//! or qualify it: `FM` replaces it outright from a given time; `BECMG` is a gradual
//! transition over a stated span; `TEMPO` and `PROB30`/`PROB40` (optionally themselves
//! followed by `TEMPO`) are temporary or probable departures during a stated span, not
//! replacements. `FM` groups are sequential — each runs until the next one, or the end of
//! the TAF — so they, and the base period before the first of them, are threaded together
//! into a chain; `BECMG`, `TEMPO` and `PROB` groups each carry their own explicit span and
//! sit alongside that chain rather than in it. Everything after `RMK` is ignored.

use super::common::{default_http, is_split_statute_miles, is_variable_wind_group, parse_cloud, parse_visibility, parse_wind, resolve_ddhh, short_lived_cache, CloudToken};
use crate::dispatch::{Taf, TafChange, TafPeriod};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};

pub fn parse_taf(raw: &str) -> Result<Taf> {
    let raw = raw.trim();
    let tokens: Vec<&str> = raw.split_whitespace().map(|t| t.trim_end_matches('=')).filter(|t| !t.is_empty()).collect();
    let mut i = 0;
    while tokens.get(i).is_some_and(|t| matches!(*t, "TAF" | "AMD" | "COR" | "CCA")) {
        i += 1;
    }
    let station = tokens.get(i).ok_or_else(|| anyhow!("empty TAF"))?.to_string();
    i += 1;

    let issue_tok = tokens.get(i).ok_or_else(|| anyhow!("{station}: no issue time"))?;
    let issue_digits = issue_tok.strip_suffix('Z').filter(|d| d.len() == 6 && d.chars().all(|c| c.is_ascii_digit())).ok_or_else(|| anyhow!("{station}: no issue time"))?;
    let issued = resolve_ddhh(issue_digits[0..2].parse()?, issue_digits[2..4].parse()?, issue_digits[4..6].parse()?, Utc::now()).ok_or_else(|| anyhow!("{station}: bad issue time"))?;
    i += 1;

    let validity_tok = tokens.get(i).ok_or_else(|| anyhow!("{station}: no validity period"))?;
    let (v_from, v_to) = parse_period_token(validity_tok).ok_or_else(|| anyhow!("{station}: bad validity period {validity_tok}"))?;
    let validity_from = resolve_ddhh(v_from.0, v_from.1, 0, issued).ok_or_else(|| anyhow!("{station}: bad validity start"))?;
    let validity_to = resolve_ddhh(v_to.0, v_to.1, 0, issued).ok_or_else(|| anyhow!("{station}: bad validity end"))?;
    i += 1;

    let chunks = chunk(&tokens[i..]);
    let periods = assemble_periods(&chunks, issued, validity_from, validity_to);

    Ok(Taf { station, issued: Some(issued), periods, raw: raw.to_string() })
}

// ---------------------------------------------------------------------------------
// Splitting the body into chunks at each change-group keyword.
// ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Base,
    From,
    Becmg,
    Tempo,
    Prob(u8),
    ProbTempo(u8),
}

struct Chunk<'a> {
    kind: Kind,
    /// The day/hour(/minute) the group's own header gives, where it gives one: `FM` gives
    /// an exact minute; `BECMG`/`TEMPO`/`PROB` give a day/hour pair for each end.
    from: Option<(u32, u32, u32)>,
    to: Option<(u32, u32, u32)>,
    tokens: Vec<&'a str>,
}

/// The body of the TAF, split at `FM`, `BECMG`, `TEMPO` and `PROB` into one chunk per
/// group, the tokens before the first of them forming the base chunk.
fn chunk<'a>(tokens: &[&'a str]) -> Vec<Chunk<'a>> {
    let mut out = vec![Chunk { kind: Kind::Base, from: None, to: None, tokens: Vec::new() }];
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok == "RMK" {
            break;
        }
        if let Some(rest) = tok.strip_prefix("FM") {
            if rest.len() == 6 && rest.chars().all(|c| c.is_ascii_digit()) {
                let from = (rest[0..2].parse().unwrap(), rest[2..4].parse().unwrap(), rest[4..6].parse().unwrap());
                out.push(Chunk { kind: Kind::From, from: Some(from), to: None, tokens: Vec::new() });
                i += 1;
                continue;
            }
        }
        if tok == "BECMG" || tok == "TEMPO" {
            let kind = if tok == "BECMG" { Kind::Becmg } else { Kind::Tempo };
            i += 1;
            if let Some((from, to)) = tokens.get(i).and_then(|t| parse_period_token(t)) {
                out.push(Chunk { kind, from: Some((from.0, from.1, 0)), to: Some((to.0, to.1, 0)), tokens: Vec::new() });
                i += 1;
                continue;
            }
            // A malformed group: fall back to the base rather than lose the tokens.
            out.push(Chunk { kind, from: None, to: None, tokens: Vec::new() });
            continue;
        }
        if let Some(pct) = tok.strip_prefix("PROB").and_then(|p| p.parse::<u8>().ok()) {
            i += 1;
            let mut kind = Kind::Prob(pct);
            if tokens.get(i) == Some(&"TEMPO") {
                kind = Kind::ProbTempo(pct);
                i += 1;
            }
            if let Some((from, to)) = tokens.get(i).and_then(|t| parse_period_token(t)) {
                out.push(Chunk { kind, from: Some((from.0, from.1, 0)), to: Some((to.0, to.1, 0)), tokens: Vec::new() });
                i += 1;
                continue;
            }
            out.push(Chunk { kind, from: None, to: None, tokens: Vec::new() });
            continue;
        }
        out.last_mut().unwrap().tokens.push(tok);
        i += 1;
    }
    out
}

/// "2312/2418": a day/hour pair either side of a slash, the shape every validity and
/// change-group span is written in.
fn parse_period_token(tok: &str) -> Option<((u32, u32), (u32, u32))> {
    let (a, b) = tok.split_once('/')?;
    if a.len() != 4 || b.len() != 4 || !a.chars().all(|c| c.is_ascii_digit()) || !b.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(((a[0..2].parse().ok()?, a[2..4].parse().ok()?), (b[0..2].parse().ok()?, b[2..4].parse().ok()?)))
}

// ---------------------------------------------------------------------------------
// Turning chunks into periods with real times.
// ---------------------------------------------------------------------------------

fn assemble_periods(chunks: &[Chunk], issued: DateTime<Utc>, validity_from: DateTime<Utc>, validity_to: DateTime<Utc>) -> Vec<TafPeriod> {
    let resolve = |t: (u32, u32, u32)| resolve_ddhh(t.0, t.1, t.2, issued);
    let mut out: Vec<TafPeriod> = Vec::new();
    // The index in `out` of the primary (base or FM) period still waiting to be told
    // where it ends, which is only known once the next primary period is seen.
    let mut open_primary: Option<usize> = None;

    for c in chunks {
        let conditions = read_conditions(&c.tokens);
        match c.kind {
            Kind::Base => {
                out.push(build_period(TafChange::Base, validity_from, validity_to, conditions));
                open_primary = Some(out.len() - 1);
            }
            Kind::From => {
                let start = c.from.and_then(resolve).unwrap_or(validity_from);
                if let Some(idx) = open_primary {
                    out[idx].to = start;
                }
                out.push(build_period(TafChange::From, start, validity_to, conditions));
                open_primary = Some(out.len() - 1);
            }
            Kind::Becmg | Kind::Tempo | Kind::Prob(_) | Kind::ProbTempo(_) => {
                let from = c.from.and_then(resolve).unwrap_or(validity_from);
                let to = c.to.and_then(resolve).unwrap_or(validity_to);
                let change = match c.kind {
                    Kind::Becmg => TafChange::Becoming,
                    Kind::Tempo => TafChange::Tempo,
                    Kind::Prob(p) => TafChange::Prob(p),
                    Kind::ProbTempo(p) => TafChange::ProbTempo(p),
                    _ => unreachable!(),
                };
                out.push(build_period(change, from, to, conditions));
            }
        }
    }
    out
}

#[derive(Default)]
struct Conditions {
    wind_from_deg: Option<f64>,
    wind_kt: Option<f64>,
    gust_kt: Option<f64>,
    visibility_m: Option<f64>,
    ceiling_ft: Option<f64>,
    weather: Vec<String>,
}

fn build_period(change: TafChange, from: DateTime<Utc>, to: DateTime<Utc>, c: Conditions) -> TafPeriod {
    TafPeriod { change, from, to, wind_from_deg: c.wind_from_deg, wind_kt: c.wind_kt, gust_kt: c.gust_kt, visibility_m: c.visibility_m, ceiling_ft: c.ceiling_ft, weather: c.weather }
}

/// A change group's own tokens: wind, visibility, weather and cloud, the same shapes a
/// METAR uses, plus `NSW` ("no significant weather", cancelling what came before) and
/// step-climb/step-descent groups such as `PM`, none of which change anything read here.
fn read_conditions(tokens: &[&str]) -> Conditions {
    let mut c = Conditions::default();
    let mut ceiling: Option<f64> = None;
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if let Some(w) = parse_wind(tok) {
            c.wind_from_deg = w.from_deg;
            c.wind_kt = Some(w.kt);
            c.gust_kt = w.gust_kt;
            i += 1;
            continue;
        }
        if is_variable_wind_group(tok) {
            i += 1;
            continue;
        }
        if tok == "CAVOK" {
            c.visibility_m = Some(10_000.0);
            i += 1;
            continue;
        }
        if let Some(next) = tokens.get(i + 1) {
            if is_split_statute_miles(tok, next) {
                if let Some(v) = parse_visibility(&format!("{tok} {next}")) {
                    c.visibility_m = Some(v);
                    i += 2;
                    continue;
                }
            }
        }
        if let Some(v) = parse_visibility(tok) {
            c.visibility_m = Some(v);
            i += 1;
            continue;
        }
        if let Some(cloud) = parse_cloud(tok) {
            match cloud {
                CloudToken::Clear => {}
                CloudToken::VerticalVisibility(vv) => ceiling = ceiling.or(vv),
                CloudToken::Layer { amount: "BKN" | "OVC", base_ft: Some(base) } => ceiling = Some(ceiling.map_or(base, |c: f64| c.min(base))),
                CloudToken::Layer { .. } => {}
            }
            i += 1;
            continue;
        }
        if tok == "NSW" {
            c.weather.push(tok.to_string());
            i += 1;
            continue;
        }
        if looks_like_weather_group(tok) {
            c.weather.push(tok.to_string());
            i += 1;
            continue;
        }
        // A visibility trend (PM/BECMG-style single-letter markers), a wind shear group
        // or anything else no field here holds.
        i += 1;
    }
    c.ceiling_ft = ceiling;
    c
}

/// Deliberately narrower than METAR's weather-group check: a TAF's tokens include things
/// like `QNH` addenda some national forecasters append, which are not weather and must
/// not be recorded as if they were. Requiring one of the codes this crate actually cares
/// about (for `rank_alternates`'s shower/thunderstorm exemption) keeps it safe either way.
fn looks_like_weather_group(tok: &str) -> bool {
    let t = tok.trim_start_matches(['-', '+']).trim_start_matches("VC");
    if t.is_empty() || t.len() > 8 || t.len() % 2 != 0 || !t.chars().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    const CODES: &[&str] = &["MI", "BC", "PR", "DR", "BL", "SH", "TS", "FZ", "DZ", "RA", "SN", "SG", "IC", "PL", "GR", "GS", "UP", "BR", "FG", "FU", "VA", "DU", "SA", "HZ", "PY", "PO", "SQ", "FC", "SS", "DS", "NSW"];
    t.as_bytes().chunks(2).all(|pair| CODES.contains(&std::str::from_utf8(pair).unwrap_or("")))
}

/// The latest forecast for an airport, fetched as raw text.
pub fn taf(icao: &str) -> Result<Taf> {
    let icao = icao.trim().to_uppercase();
    let http = default_http();
    let cache = short_lived_cache();
    let url = format!("https://aviationweather.gov/api/data/taf?ids={icao}&format=raw");
    let key = format!("taf/{icao}.txt");
    let text = cache.get_or_fetch_text(&key, || http.get_text(&url))?;
    let body = text.trim();
    if body.is_empty() {
        anyhow::bail!("no TAF published for {icao}");
    }
    // The raw endpoint wraps a long TAF onto several lines; joined back into one string
    // of tokens is what the parser expects, matching how the report is written in the
    // first place before line-wrapping for display.
    parse_taf(&body.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn a_taf_with_fm_becmg_tempo_and_prob_resolves_into_the_right_chain() {
        let raw = "TAF EGLL 231100Z 2312/2418 24012KT 9999 SCT035\n\
                    BECMG 2314/2316 25015G25KT\n\
                    TEMPO 2316/2320 4000 SHRA BKN015CB\n\
                    PROB30 TEMPO 2320/2402 0800 FG\n\
                    FM240600 27008KT CAVOK\n\
                    BECMG 2410/2412 9999 NSW";
        let taf = parse_taf(raw).unwrap();
        assert_eq!(taf.station, "EGLL");
        let issued = taf.issued.unwrap();
        assert_eq!((issued.day(), issued.hour(), issued.minute()), (23, 11, 0));

        // Base, FM: the two primary periods, chained end to end.
        let base = &taf.periods[0];
        assert_eq!(base.change, TafChange::Base);
        assert_eq!((base.from.day(), base.from.hour()), (23, 12));
        assert_eq!((base.to.day(), base.to.hour()), (24, 6)); // ends where FM240600 begins
        assert_eq!(base.wind_kt, Some(12.0));

        let fm = taf.periods.iter().find(|p| p.change == TafChange::From).unwrap();
        assert_eq!((fm.from.day(), fm.from.hour()), (24, 6));
        assert_eq!((fm.to.day(), fm.to.hour()), (24, 18)); // runs to the end of validity
        assert_eq!(fm.visibility_m, Some(10_000.0));

        let becmg = taf.periods.iter().find(|p| p.change == TafChange::Becoming).unwrap();
        assert_eq!(becmg.wind_kt, Some(15.0));
        assert_eq!(becmg.gust_kt, Some(25.0));

        let tempo = taf.periods.iter().find(|p| p.change == TafChange::Tempo).unwrap();
        assert_eq!(tempo.visibility_m, Some(4_000.0));
        assert_eq!(tempo.ceiling_ft, Some(1_500.0));
        assert!(tempo.weather.iter().any(|w| w == "SHRA"));

        let prob = taf.periods.iter().find(|p| matches!(p.change, TafChange::ProbTempo(30))).unwrap();
        assert_eq!(prob.visibility_m, Some(800.0));
        assert!(prob.weather.iter().any(|w| w == "FG"));

        // Validity crossing midnight into day 24, and the trailing BECMG on day 24.
        let last_becmg = taf.periods.iter().rev().find(|p| p.change == TafChange::Becoming).unwrap();
        assert_eq!((last_becmg.from.day(), last_becmg.from.hour()), (24, 10));
        assert!(last_becmg.weather.iter().any(|w| w == "NSW"));
    }

    #[test]
    fn validity_crossing_a_month_end_resolves_against_the_issue_time() {
        let raw = "TAF KXXX 312300Z 0100/0206 24010KT 9999 FEW040";
        let taf = parse_taf(raw).unwrap();
        let issued = taf.issued.unwrap();
        assert_eq!(issued.day(), 31);
        let base = &taf.periods[0];
        assert_eq!(base.from.day(), 1);
        assert_eq!(base.to.day(), 2);
        // The end is a real calendar day after the issue day, not the 31st again.
        assert!(base.to > base.from);
    }

    #[test]
    fn prob_without_tempo_is_its_own_change_kind() {
        let raw = "TAF KDEN 231100Z 2312/2412 25012KT 9999 SCT040\n\
                    PROB40 2318/2321 1600 -SHSN";
        let taf = parse_taf(raw).unwrap();
        let prob = taf.periods.iter().find(|p| matches!(p.change, TafChange::Prob(40))).unwrap();
        assert_eq!(prob.visibility_m, Some(1_600.0));
        assert!(prob.weather.iter().any(|w| w == "-SHSN"));
    }

    /// A round-the-world set, the same spirit as the METAR fixtures: enough real TAFs,
    /// including the awkward shapes, to trust the parser generally rather than only on
    /// the one worked example above.
    const REPORTS: &[&str] = &[
        "TAF KJFK 231120Z 2312/2418 28015G25KT P6SM FEW250\nFM240100 26010KT P6SM SCT250\nFM241200 24012G20KT P6SM BKN040",
        "TAF EDDF 231100Z 2312/2418 27008KT 9999 SCT035\nBECMG 2315/2317 26012G22KT 9999 SCT030\nTEMPO 2318/2322 4000 TSRA BKN020CB",
        "TAF RJTT 231100Z 2312/2418 09008KT 9999 FEW020\nBECMG 2318/2320 12010KT 9999 SCT025",
        "TAF OMDB 231100Z 2312/2418 32008KT CAVOK\nTEMPO 2313/2318 33018G28KT 4000 DU",
        "TAF VABB 231100Z 2312/2418 22010KT 4000 HZ SCT018\nPROB30 TEMPO 2316/2320 2000 TSRA BKN012CB",
        "TAF EGLL 231100Z 2312/2418 25012KT 9999 BKN035\nBECMG 2313/2315 9999 NSW",
        "TAF LFPG 231100Z 2312/2418 26010KT CAVOK\nFM241000 24008KT 9999 SCT030",
        "TAF KORD 231120Z 2312/2418 29018G28KT P6SM SCT050\nFM231800 30015KT P6SM BKN060\nTEMPO 2313/2317 5SM -SHRA BKN025",
        "TAF ZBAA 231100Z 2312/2418 18004MPS 9999 FEW030\nBECMG 2316/2318 20006MPS 9999 SCT035",
        "TAF NZAA 231100Z 2312/2418 22014G24KT 9999 BKN025\nTEMPO 2312/2318 4500 SHRA",
        "TAF SBGR 231100Z 2312/2418 09006KT 9999 FEW030\nBECMG 2318/2320 12008KT 8000 -RA BKN020",
        "TAF FACT 231100Z 2312/2418 18020G30KT 9999 FEW020\nFM240600 20012KT 9999 SCT025",
        "TAF WSSS 231100Z 2312/2418 09008KT 9999 FEW018CB\nTEMPO 2314/2320 4000 TSRA BKN014CB",
        "TAF LEMD 231100Z 2312/2418 VRB03KT CAVOK\nBECMG 2314/2316 22012KT 9999 FEW040",
        "TAF EHAM 231100Z 2312/2418 25012KT 9999 SCT030\nBECMG 2316/2318 27018G28KT 9999 -RA BKN020",
        "TAF KLAX 231120Z 2312/2418 25008KT P6SM SCT200\nFM231900 26012KT P6SM FEW250",
        "TAF LOWW 231100Z 2312/2418 27010KT 9999 FEW040\nTEMPO 2313/2317 25020G32KT",
        "TAF RCTP 231100Z 2312/2418 05008KT 9999 FEW015\nBECMG 2318/2320 08012KT 9999 SCT300",
        "TAF EGCC 231100Z 2312/2418 23015G25KT 9999 -RA BKN025\nTEMPO 2312/2318 20025G35KT",
        "TAF LSZH 231100Z 2312/2418 26008KT 9999 SCT035\nBECMG 2316/2318 24012KT 9999 FEW040",
        "TAF EDDB 231100Z 2312/2418 VRB02KT 9999 FEW040\nFM241300 27010KT 9999 SCT035",
        "TAF EKCH 231100Z 2312/2418 23012KT 9999 SCT030\nPROB40 2313/2317 1500 SHSN",
        "TAF ENGM 231100Z 2312/2418 20008KT 9999 SCT025\nBECMG 2315/2317 24014G24KT 9999 -SN BKN015",
        "TAF LIRF 231100Z 2312/2418 06006KT CAVOK\nTEMPO 2314/2318 22016G26KT",
        "TAF LTBA 231100Z 2312/2418 03008KT CAVOK\nFM241100 20010KT CAVOK",
        "TAF LGAV 231100Z 2312/2418 01006KT CAVOK\nBECMG 2314/2316 22012KT CAVOK",
        "TAF SKBO 231100Z 2312/2418 09006KT 9999 FEW035\nTEMPO 2318/2322 4000 -TSRA BKN018CB",
        "TAF MMMX 231100Z 2312/2418 05005KT 9999 FEW200\nBECMG 2318/2320 27010KT 9999 SCT120",
        "TAF RPLL 231100Z 2312/2418 VRB02KT 9999 FEW020\nTEMPO 2313/2318 4000 TSRA",
        "TAF PANC 231120Z 2312/2418 15006KT P6SM FEW070\nFM240200 18012G22KT P6SM BKN040",
    ];

    #[test]
    fn thirty_or_more_real_tafs_parse_without_error() {
        assert!(REPORTS.len() >= 30, "only {} reports", REPORTS.len());
        for raw in REPORTS {
            let taf = parse_taf(raw).unwrap_or_else(|e| panic!("{e:#}\n{raw}"));
            assert!(!taf.periods.is_empty(), "{raw}");
            assert!(taf.periods[0].to > taf.periods[0].from, "{raw}");
        }
    }

    /// The live TAF for three airports round the world. Not run by default: it needs the
    /// network and aviationweather.gov's own availability.
    #[test]
    #[ignore]
    fn real_tafs_for_egll_kjfk_vabb() {
        for icao in ["EGLL", "KJFK", "VABB"] {
            match taf(icao) {
                Ok(t) => {
                    println!("{icao}: issued {:?}, {} period(s)", t.issued, t.periods.len());
                    for p in &t.periods {
                        println!("  {:?} {} -> {} wind {:?}/{:?}G{:?} vis {:?} ceil {:?} wx {:?}", p.change, p.from, p.to, p.wind_from_deg, p.wind_kt, p.gust_kt, p.visibility_m, p.ceiling_ft, p.weather);
                    }
                }
                Err(e) => println!("{icao}: {e:#}"),
            }
        }
    }
}
