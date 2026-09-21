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
    pub runway: String,
    pub published_da_ft: f64,
    pub published_hat_ft: f64,
    pub published_tdze_ft: f64,
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
    pub limited_by: &'static str,
    pub published_hat_ft: f64,
}

fn describe(l: LimitedBy) -> &'static str {
    match l {
        LimitedBy::SystemMinimum => "system",
        LimitedBy::Terrain => "terrain",
        LimitedBy::Obstacle => "obstacle",
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

    let http = crate::sources::http::Http::new(120, 0);
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
                let opts = crate::approach::Options { wide_terrain: false, quiet: true };
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                let setup = match crate::approach::prepare(&http, &cache, &idx, &case.icao, Some(&case.runway), opts) {
                    Ok(s) => s,
                    Err(e) => {
                        crate::term::warn(&format!("[{n}/{total}] {} RW{}: {e:#}", case.icao, case.runway));
                        return None;
                    }
                };
                let est = crate::approach::estimate(&setup, Approach::PrecisionCat1);
                let outcome = Outcome {
                    icao: case.icao.clone(),
                    runway: case.runway.clone(),
                    published_da_ft: case.published_da_ft,
                    published_tdze_ft: case.published_tdze_ft,
                    our_da_ft: est.altitude_ft,
                    our_tdze_ft: setup.tdze_ft,
                    da_error_ft: est.altitude_ft - case.published_da_ft,
                    tdze_error_ft: setup.tdze_ft - case.published_tdze_ft,
                    limited_by: describe(est.limited_by),
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
    let tdze: Vec<f64> = outcomes.iter().map(|o| o.tdze_error_ft).collect();
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
