//! How close our estimated minima are to the published ones.
//!
//! The input is a table of published figures read off real charts: the decision altitude
//! for a straight-in ILS, and the height above the touchdown zone it is quoted with. The
//! difference between those two is the touchdown zone elevation, which lets the two
//! halves of an estimate be measured separately: how well the terrain model finds the
//! runway, and how well the rules find the minimum above it.
//!
//! Nothing here is fitted to the answers. The numbers this prints are what the estimator
//! already does.

use crate::minima::{Approach, LimitedBy};
use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub icao: String,
    #[serde(default)]
    pub runway: String,
    /// What kind of approach the published figure is for. Left out, it is an ILS.
    #[serde(default)]
    pub kind: String,
    #[serde(alias = "published_mda_ft")]
    pub published_da_ft: f64,
    pub published_hat_ft: f64,
    /// Missing for a circling minimum, which is quoted above the aerodrome rather than
    /// above a touchdown zone.
    #[serde(default)]
    pub published_tdze_ft: Option<f64>,
}

impl Case {
    fn approach(&self) -> Approach {
        match self.kind.trim().to_ascii_lowercase().as_str() {
            "circling" => Approach::Circling,
            "loc" | "lda" | "lnav" => Approach::Localiser,
            "vor" | "np" | "nonprecision" => Approach::NonPrecision,
            "ndb" => Approach::Ndb,
            "rnav" | "lpv" | "lnav/vnav" => Approach::VerticallyGuided,
            _ => Approach::PrecisionCat1,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub icao: String,
    pub runway: String,
    pub published_da_ft: f64,
    pub published_tdze_ft: f64,
    pub our_da_ft: f64,
    pub our_tdze_ft: f64,
    pub da_error_ft: f64,
    pub tdze_error_ft: f64,
    /// What kind of approach the published figure was for.
    pub kind: String,
    /// The lowest altitude the procedure codes on its final, ignoring the runway fix:
    /// a candidate for the minimum that costs nothing to read.
    pub coded_floor_ft: f64,
    /// What the terrain model makes of the touchdown zone, for comparing against the
    /// survey: the highest sample, the middle one, the lowest, and a low percentile.
    pub dem_max_ft: f64,
    pub dem_median_ft: f64,
    pub dem_min_ft: f64,
    pub dem_p25_ft: f64,
    /// What the ground around the approach looks like, for working out where an
    /// estimate went wrong.
    pub corridor_terrain_ft: f64,
    pub corridor_obstacle_ft: f64,
    pub near_terrain_ft: f64,
    pub near_obstacle_ft: f64,
    pub ring13_ft: f64,
    pub ring23_ft: f64,
    pub ring50_ft: f64,
    pub faf_nm: f64,
    pub tdze_used_ft: f64,
    pub field_elev_ft: f64,
    pub limited_by: &'static str,
    /// What set our number, where something on the ground did.
    pub controlling: String,
    pub published_hat_ft: f64,
}

fn describe(l: LimitedBy) -> &'static str {
    match l {
        LimitedBy::SystemMinimum => "system",
        LimitedBy::Terrain => "terrain",
        LimitedBy::Obstacle => "obstacle",
        LimitedBy::Coded => "coded",
    }
}

/// Middle value of a list, which says more about a spread of errors than the mean does.
fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn share(v: &[f64], within: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.iter().filter(|e| e.abs() <= within).count() as f64 * 100.0 / v.len() as f64
}

/// Run every case and print how the estimator did.
pub fn run(truth: &Path, out: Option<&Path>, jobs: usize) -> Result<()> {
    let mut rdr = csv::Reader::from_path(truth).with_context(|| format!("read {}", truth.display()))?;
    let cases: Vec<Case> = rdr.deserialize().collect::<Result<_, _>>()?;
    crate::term::start(&format!("Measuring {} published approaches", cases.len()));

    let http = crate::sources::http::Http::new(300, 0);
    let cache = crate::cache::Cache::for_index(false);
    let mut idx = crate::sources::index::AirportIndex::default();
    idx.load_ourairports_online(&http, &cache)?;

    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs.max(1)).build()?;
    let done = std::sync::atomic::AtomicUsize::new(0);
    let total = cases.len();
    let outcomes: Vec<Outcome> = pool.install(|| {
        cases
            .par_iter()
            .filter_map(|case| {
                let opts = crate::approach::Options { wide_terrain: false, quiet: true, obstacle_radius_km: 8.0 };
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                // A circling minimum names no runway, so any approach at the airport
                // gives the geometry it is worked out from.
                let wanted = (!case.runway.trim().is_empty()).then(|| case.runway.clone());
                let setup = match crate::approach::prepare(&http, &cache, &idx, &case.icao, wanted.as_deref(), opts) {
                    Ok(s) => s,
                    Err(e) => {
                        crate::term::warn(&format!("[{n}/{total}] {} RW{}: {e:#}", case.icao, case.runway));
                        return None;
                    }
                };
                // What the terrain model would have said, even where a survey answered.
                let samples = setup
                    .threshold
                    .and_then(|(lat, lon, bearing)| {
                        let fine = crate::sources::copernicus::patch(&http, &cache, lat, lon, 2.0, 30.0).ok()?;
                        Some(fine.touchdown_zone_samples(lat, lon, bearing?))
                    })
                    .unwrap_or_default();
                let pick = |f: f64| if samples.is_empty() { f64::NAN } else { samples[((samples.len() - 1) as f64 * f) as usize] };
                let est = crate::approach::estimate(&setup, case.approach());
                let look = crate::approach::survey(&setup, case.approach());
                let outcome = Outcome {
                    icao: case.icao.clone(),
                    runway: case.runway.clone(),
                    published_da_ft: case.published_da_ft,
                    published_tdze_ft: case.published_tdze_ft.unwrap_or(f64::NAN),
                    our_da_ft: est.altitude_ft,
                    our_tdze_ft: setup.tdze_ft,
                    da_error_ft: est.altitude_ft - case.published_da_ft,
                    tdze_error_ft: case.published_tdze_ft.map(|t| setup.tdze_ft - t).unwrap_or(f64::NAN),
                    kind: if case.kind.trim().is_empty() { "ils".to_string() } else { case.kind.trim().to_string() },
                    corridor_terrain_ft: look.corridor_terrain_ft,
                    corridor_obstacle_ft: look.corridor_obstacle_ft,
                    near_terrain_ft: look.near_terrain_ft,
                    near_obstacle_ft: look.near_obstacle_ft,
                    ring13_ft: look.ring13_ft,
                    ring23_ft: look.ring23_ft,
                    ring50_ft: look.ring50_ft,
                    faf_nm: look.faf_nm,
                    tdze_used_ft: setup.tdze_ft,
                    field_elev_ft: setup.field_elev_ft,
                    dem_max_ft: pick(1.0),
                    dem_median_ft: pick(0.5),
                    dem_min_ft: pick(0.0),
                    dem_p25_ft: pick(0.25),
                    coded_floor_ft: setup
                        .final_legs()
                        .iter()
                        .filter(|l| !l.fix.starts_with("RW"))
                        .filter_map(|l| l.altitude_ft)
                        .fold(f64::NAN, f64::min),
                    limited_by: describe(est.limited_by),
                    controlling: est.obstacle.clone().unwrap_or_default(),
                    published_hat_ft: case.published_hat_ft,
                };
                crate::term::info(&format!(
                    "[{n}/{total}] {} RW{}: ours {:.0} ft, published {:.0} ft ({:+.0} ft, {})",
                    outcome.icao, outcome.runway, outcome.our_da_ft, outcome.published_da_ft, outcome.da_error_ft, outcome.limited_by
                ));
                Some(outcome)
            })
            .collect()
    });

    if outcomes.is_empty() {
        return Err(anyhow::anyhow!("nothing could be measured"));
    }
    if let Some(path) = out {
        let mut w = csv::Writer::from_path(path)?;
        for o in &outcomes {
            w.serialize(o)?;
        }
        w.flush()?;
        crate::term::file(None, &path.display().to_string(), "measurements");
    }

    report(&outcomes);
    Ok(())
}

/// The summary: the whole set, then the part of it the rules are supposed to get exactly
/// right, then the part where something on the ground pushed the published figure up.
pub fn report(outcomes: &[Outcome]) {
    let all: Vec<f64> = outcomes.iter().map(|o| o.da_error_ft).collect();
    let tdze: Vec<f64> = outcomes.iter().map(|o| o.tdze_error_ft).filter(|e| e.is_finite()).collect();
    let flat: Vec<f64> = outcomes.iter().filter(|o| o.published_hat_ft <= 200.0).map(|o| o.da_error_ft).collect();
    let raised: Vec<f64> = outcomes.iter().filter(|o| o.published_hat_ft > 200.0).map(|o| o.da_error_ft).collect();

    let line = |name: &str, v: &[f64]| {
        if v.is_empty() {
            println!("  {name:<34} -");
            return;
        }
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        println!(
            "  {name:<34} n={:<4} mean {:+6.1} ft   median |error| {:5.1} ft   within 5 ft {:4.0}%   within 10 ft {:4.0}%   within 50 ft {:4.0}%",
            v.len(),
            mean,
            median(v.iter().map(|e| e.abs()).collect()),
            share(v, 5.0),
            share(v, 10.0),
            share(v, 50.0)
        );
    };

    println!("\nAgainst published minima:");
    line("every approach", &all);
    line("where the chart is at 200 ft", &flat);
    line("where the chart is higher", &raised);
    line("touchdown zone elevation", &tdze);

    let mut by_kind: std::collections::BTreeMap<&str, Vec<f64>> = Default::default();
    for o in outcomes {
        by_kind.entry(o.kind.as_str()).or_default().push(o.da_error_ft);
    }
    if by_kind.len() > 1 {
        println!("  by kind:");
        for (k, v) in &by_kind {
            line(&format!("    {k}"), v);
        }
    }

    let mut by_limit: std::collections::BTreeMap<&str, usize> = Default::default();
    for o in outcomes {
        *by_limit.entry(o.limited_by).or_default() += 1;
    }
    let parts: Vec<String> = by_limit.iter().map(|(k, v)| format!("{v} {k}")).collect();
    println!("  what set our number:               {}", parts.join(", "));

    // The worst few, which is where the next improvement is.
    let mut worst: Vec<&Outcome> = outcomes.iter().collect();
    worst.sort_by(|a, b| b.da_error_ft.abs().total_cmp(&a.da_error_ft.abs()));
    println!("  furthest off:");
    for o in worst.iter().take(5) {
        println!(
            "    {} RW{:<4} ours {:.0} ft, published {:.0} ft ({:+.0} ft; touchdown zone {:+.0} ft, set by {})",
            o.icao, o.runway, o.our_da_ft, o.published_da_ft, o.da_error_ft, o.tdze_error_ft, o.limited_by
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_an_even_list_is_the_middle_pair() {
        assert_eq!(median(vec![1.0, 3.0, 5.0, 7.0]), 4.0);
        assert_eq!(median(vec![5.0, 1.0, 3.0]), 3.0);
    }

    #[test]
    fn share_counts_both_directions() {
        assert_eq!(share(&[-4.0, 4.0, 40.0, -40.0], 5.0), 50.0);
    }
}
