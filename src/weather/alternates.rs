//! Choosing an alternate: a pure judgement, `rank_alternates`, over whatever candidates and
//! TAFs are handed to it, wrapped by `choose_alternates` for the ordinary case of asking
//! the navigation database for airports near the destination and fetching their TAFs.
//!
//! `route::airports::airports_near` is another part of this crate's flight planner and is
//! not built in this worktree — it returns nothing here — so the judgement itself is kept
//! separate and pure, testable against candidates and TAFs made up for the purpose.

use crate::dispatch::{distance_nm, Airport, Alternate, Taf, TafChange, TafPeriod};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

/// EASA's planning minima for an airport taken to have a precision approach: a ceiling of
/// at least 700 ft and visibility of at least 3,000 m, over the hour either side of the
/// estimate.
const MIN_CEILING_FT: f64 = 700.0;
const MIN_VISIBILITY_M: f64 = 3_000.0;
const WINDOW: i64 = 3_600; // seconds either side of the estimate

/// The best alternates for a destination at an arrival time, best first: airports within
/// 250 nm whose longest runway is at least 6,000 ft, ranked by `rank_alternates`.
pub fn choose_alternates(destination: &Airport, eta: DateTime<Utc>, count: usize) -> Result<Vec<Alternate>> {
    let candidates = crate::route::airports::airports_near(destination.pos, 250.0, 6000.0);
    let mut tafs = Vec::new();
    for (airport, _) in &candidates {
        if airport.icao.eq_ignore_ascii_case(&destination.icao) {
            continue;
        }
        match super::taf::taf(&airport.icao) {
            Ok(taf) => tafs.push((airport.icao.clone(), taf)),
            Err(e) => log::debug!("{}: no usable TAF for an alternate ({e:#})", airport.icao),
        }
    }
    let ranked = rank_alternates(destination, &candidates, &tafs, eta);
    Ok(ranked.into_iter().take(count).collect())
}

/// The judgement itself, apart from where the candidates or their forecasts came from: an
/// airport is acceptable when every TAF period that counts, over the hour either side of
/// `eta`, meets planning minima; acceptable airports are then ranked by distance.
///
/// A `TEMPO` period always counts. A `PROB` (or `PROB … TEMPO`) period counts too, unless
/// its forecast weather is only showers or thunderstorms — a probability of a shower is
/// not planned round the way a probability of fog is.
pub fn rank_alternates(dest: &Airport, candidates: &[(Airport, f64)], tafs: &[(String, Taf)], eta: DateTime<Utc>) -> Vec<Alternate> {
    let window_from = eta - Duration::seconds(WINDOW);
    let window_to = eta + Duration::seconds(WINDOW);

    let mut out: Vec<Alternate> = candidates
        .iter()
        .filter(|(a, _)| !a.icao.eq_ignore_ascii_case(&dest.icao))
        .filter_map(|(airport, _runway_ft)| {
            let taf = tafs.iter().find(|(icao, _)| icao.eq_ignore_ascii_case(&airport.icao)).map(|(_, t)| t)?;
            let periods: Vec<&TafPeriod> = taf.periods.iter().filter(|p| overlaps(p, window_from, window_to)).filter(|p| counts(p)).collect();
            if periods.is_empty() || periods.iter().any(|p| !meets_minima(p)) {
                return None;
            }
            let distance_nm = distance_nm(dest.pos, airport.pos);
            Some(Alternate { airport: airport.clone(), distance_nm, reason: format!("TAF OK ETA±1h, {distance_nm:.0} nm") })
        })
        .collect();
    out.sort_by(|a, b| a.distance_nm.total_cmp(&b.distance_nm));
    out
}

fn overlaps(p: &TafPeriod, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
    p.from <= to && p.to >= from
}

/// Whether a period is one the minima check applies to at all.
fn counts(p: &TafPeriod) -> bool {
    match p.change {
        TafChange::Base | TafChange::From | TafChange::Becoming | TafChange::Tempo => true,
        TafChange::Prob(_) | TafChange::ProbTempo(_) => !is_only_showers_or_thunderstorms(&p.weather),
    }
}

fn is_only_showers_or_thunderstorms(weather: &[String]) -> bool {
    !weather.is_empty()
        && weather.iter().all(|w| {
            let w = w.trim_start_matches(['-', '+']).trim_start_matches("VC");
            w.starts_with("SH") || w.starts_with("TS")
        })
}

fn meets_minima(p: &TafPeriod) -> bool {
    p.ceiling_ft.is_none_or(|c| c >= MIN_CEILING_FT) && p.visibility_m.is_none_or(|v| v >= MIN_VISIBILITY_M)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::LatLon;

    fn airport(icao: &str, pos: LatLon) -> Airport {
        Airport { icao: icao.to_string(), name: String::new(), pos, elevation_ft: 0.0 }
    }

    fn base_period(from: DateTime<Utc>, to: DateTime<Utc>, ceiling_ft: Option<f64>, visibility_m: Option<f64>) -> TafPeriod {
        TafPeriod { change: TafChange::Base, from, to, wind_from_deg: None, wind_kt: None, gust_kt: None, visibility_m, ceiling_ft, weather: Vec::new() }
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn a_good_taf_within_range_is_accepted_and_reasoned() {
        let dest = airport("EGLL", (51.5, -0.5));
        let alt = airport("EGKK", (51.15, -0.19));
        let eta = now();
        let candidates = vec![(alt.clone(), 8000.0)];
        let taf = Taf { station: "EGKK".into(), issued: Some(eta - Duration::hours(2)), periods: vec![base_period(eta - Duration::hours(3), eta + Duration::hours(6), Some(2000.0), Some(9000.0))], raw: String::new() };
        let tafs = vec![("EGKK".to_string(), taf)];
        let ranked = rank_alternates(&dest, &candidates, &tafs, eta);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].airport.icao, "EGKK");
        assert!(ranked[0].reason.contains("TAF OK ETA±1h"));
        assert!(ranked[0].reason.contains("nm"));
    }

    #[test]
    fn below_minima_in_the_window_is_rejected() {
        let dest = airport("EGLL", (51.5, -0.5));
        let alt = airport("EGKK", (51.15, -0.19));
        let eta = now();
        let candidates = vec![(alt.clone(), 8000.0)];
        // Ceiling below 700 ft during the window.
        let taf = Taf { station: "EGKK".into(), issued: Some(eta - Duration::hours(2)), periods: vec![base_period(eta - Duration::hours(3), eta + Duration::hours(6), Some(400.0), Some(9000.0))], raw: String::new() };
        let tafs = vec![("EGKK".to_string(), taf)];
        assert!(rank_alternates(&dest, &candidates, &tafs, eta).is_empty());
    }

    #[test]
    fn a_candidate_with_no_taf_is_left_out() {
        let dest = airport("EGLL", (51.5, -0.5));
        let alt = airport("EGKK", (51.15, -0.19));
        let eta = now();
        let candidates = vec![(alt, 8000.0)];
        assert!(rank_alternates(&dest, &candidates, &[], eta).is_empty());
    }

    #[test]
    fn the_destination_itself_is_never_offered_as_its_own_alternate() {
        let dest = airport("EGLL", (51.5, -0.5));
        let eta = now();
        let candidates = vec![(dest.clone(), 12000.0)];
        let taf = Taf { station: "EGLL".into(), issued: Some(eta), periods: vec![base_period(eta - Duration::hours(1), eta + Duration::hours(6), Some(3000.0), Some(9000.0))], raw: String::new() };
        let tafs = vec![("EGLL".to_string(), taf)];
        assert!(rank_alternates(&dest, &candidates, &tafs, eta).is_empty());
    }

    #[test]
    fn ranking_is_by_distance_nearest_first() {
        let dest = airport("EGLL", (51.5, -0.5));
        let near = airport("EGKK", (51.15, -0.19)); // roughly 30 nm
        let far = airport("EGCC", (53.35, -2.27)); // roughly 150 nm
        let eta = now();
        let candidates = vec![(far.clone(), 9000.0), (near.clone(), 8000.0)];
        let good = base_period(eta - Duration::hours(1), eta + Duration::hours(6), Some(3000.0), Some(9000.0));
        let tafs = vec![
            ("EGCC".to_string(), Taf { station: "EGCC".into(), issued: Some(eta), periods: vec![good.clone()], raw: String::new() }),
            ("EGKK".to_string(), Taf { station: "EGKK".into(), issued: Some(eta), periods: vec![good], raw: String::new() }),
        ];
        let ranked = rank_alternates(&dest, &candidates, &tafs, eta);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].airport.icao, "EGKK");
        assert!(ranked[0].distance_nm < ranked[1].distance_nm);
    }

    #[test]
    fn tempo_below_minima_rejects_even_though_the_base_is_fine() {
        let dest = airport("EGLL", (51.5, -0.5));
        let alt = airport("EGKK", (51.15, -0.19));
        let eta = now();
        let candidates = vec![(alt, 8000.0)];
        let base = base_period(eta - Duration::hours(3), eta + Duration::hours(6), Some(3000.0), Some(9000.0));
        let tempo = TafPeriod { change: TafChange::Tempo, from: eta - Duration::minutes(30), to: eta + Duration::minutes(30), wind_from_deg: None, wind_kt: None, gust_kt: None, visibility_m: Some(1500.0), ceiling_ft: Some(300.0), weather: vec!["FG".into()] };
        let taf = Taf { station: "EGKK".into(), issued: Some(eta - Duration::hours(2)), periods: vec![base, tempo], raw: String::new() };
        let tafs = vec![("EGKK".to_string(), taf)];
        assert!(rank_alternates(&dest, &candidates, &tafs, eta).is_empty(), "a TEMPO below minima in the window must reject the airport");
    }

    #[test]
    fn a_prob_of_only_showers_is_exempt_but_a_prob_of_fog_is_not() {
        let dest = airport("EGLL", (51.5, -0.5));
        let alt = airport("EGKK", (51.15, -0.19));
        let eta = now();
        let candidates = vec![(alt, 8000.0)];
        let base = base_period(eta - Duration::hours(3), eta + Duration::hours(6), Some(3000.0), Some(9000.0));

        let prob_shower = TafPeriod { change: TafChange::ProbTempo(30), from: eta - Duration::minutes(20), to: eta + Duration::minutes(20), wind_from_deg: None, wind_kt: None, gust_kt: None, visibility_m: Some(1500.0), ceiling_ft: Some(400.0), weather: vec!["TSRA".into()] };
        let taf_ok = Taf { station: "EGKK".into(), issued: Some(eta), periods: vec![base.clone(), prob_shower], raw: String::new() };
        let ranked = rank_alternates(&dest, &candidates, &[("EGKK".to_string(), taf_ok)], eta);
        assert_eq!(ranked.len(), 1, "a PROB of only showers/thunderstorms should not count against minima");

        let prob_fog = TafPeriod { change: TafChange::Prob(30), from: eta - Duration::minutes(20), to: eta + Duration::minutes(20), wind_from_deg: None, wind_kt: None, gust_kt: None, visibility_m: Some(800.0), ceiling_ft: Some(200.0), weather: vec!["FG".into()] };
        let taf_bad = Taf { station: "EGKK".into(), issued: Some(eta), periods: vec![base, prob_fog], raw: String::new() };
        let alt2 = airport("EGKK", (51.15, -0.19));
        assert!(rank_alternates(&dest, &[(alt2, 8000.0)], &[("EGKK".to_string(), taf_bad)], eta).is_empty(), "a PROB of fog is not exempt");
    }

    #[test]
    fn a_period_outside_the_window_does_not_count() {
        let dest = airport("EGLL", (51.5, -0.5));
        let alt = airport("EGKK", (51.15, -0.19));
        let eta = now();
        let candidates = vec![(alt, 8000.0)];
        let base = base_period(eta - Duration::hours(3), eta + Duration::hours(6), Some(3000.0), Some(9000.0));
        // Well outside the +-1h window: should not be checked at all.
        let far_tempo = TafPeriod { change: TafChange::Tempo, from: eta + Duration::hours(4), to: eta + Duration::hours(5), wind_from_deg: None, wind_kt: None, gust_kt: None, visibility_m: Some(100.0), ceiling_ft: Some(50.0), weather: vec!["FG".into()] };
        let taf = Taf { station: "EGKK".into(), issued: Some(eta), periods: vec![base, far_tempo], raw: String::new() };
        let ranked = rank_alternates(&dest, &candidates, &[("EGKK".to_string(), taf)], eta);
        assert_eq!(ranked.len(), 1);
    }
}
