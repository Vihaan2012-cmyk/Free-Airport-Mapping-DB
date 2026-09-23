//! Annex 3B: the limits on direct routing. `DCT` holds one row per direct leg outside
//! Free Route Airspace — a pair of points, the band of levels it may be flown at, and
//! whether it is open at all — built as rules keyed the same way an airway segment is,
//! since a direct leg is filed the same way (`airway == "DCT"`). `FRA LIM` holds the
//! cross-border limits on directs *inside* Free Route Airspace, which turn on which
//! polygon a point falls in; without that geometry this crate has no way to check them,
//! so every row there is counted skipped rather than guessed at.

use super::condition::{Clause, Keyword, LevelCmp, LevelTerm, Restriction, Term};
use super::coverage::Coverage;
use super::model::{parse_rad_date, parse_time_applicability, row_active, Groups, Validity};
use super::rules::{rules_from_cell, Rule, RuleKey};
use super::xlsx::Sheet;
use chrono::{DateTime, Utc};

/// A little inside each edge of a written band: a segment costed at exactly a band's
/// own boundary (a flight level is always a round hundred feet) must fall inside it,
/// not be caught by the same rounding slack the level comparison allows either side.
const BAND_MARGIN_FT: f64 = 100.0;

pub(crate) fn parse_dct(sheet: &Sheet, when: DateTime<Utc>, groups: &Groups, coverage: &mut Coverage) -> Vec<Rule> {
    let mut out = Vec::new();
    let h = sheet.header();
    let cols = ["Change Ind.", "Valid From", "Valid Until", "ID", "From", "To", "Lower Vert. Limit (FL)", "Upper Vert. Limit (FL)", "Available or Not (Y/N)", "Utilization", "Time Availability"];
    let Some(idx): Option<Vec<usize>> = cols.iter().map(|c| h.col(c)).collect() else {
        log::warn!("Annex 3B DCT: expected column not found; skipping the sheet");
        return out;
    };
    let (change, from_date, until_date, id, from, to, lower, upper, available, utilization, time) = (idx[0], idx[1], idx[2], idx[3], idx[4], idx[5], idx[6], idx[7], idx[8], idx[9], idx[10]);

    for row in 1..sheet.rows.len() {
        let valid_from = parse_rad_date(sheet.cell(row, from_date));
        let valid_until = parse_rad_date(sheet.cell(row, until_date));
        if !row_active(sheet.cell(row, change), valid_from, valid_until, when) {
            continue;
        }
        let (row_id, from_pt, to_pt) = (sheet.cell(row, id).trim(), sheet.cell(row, from).trim(), sheet.cell(row, to).trim());
        if row_id.is_empty() || from_pt.is_empty() || to_pt.is_empty() {
            continue;
        }
        let key = RuleKey::Airway { airway: "DCT".to_string(), from: from_pt.to_string(), to: to_pt.to_string() };
        let Some(applicability) = parse_time_applicability(sheet.cell(row, time)) else {
            coverage.skip("Annex 3B DCT: a Time Availability this does not parse", sheet.cell(row, time));
            continue;
        };
        let validity = Validity { from: valid_from, until: valid_until, time: applicability };

        if !sheet.cell(row, available).trim().to_uppercase().starts_with('Y') {
            coverage.parsed();
            out.push(closed_rule(row_id, key, validity, format!("{from_pt} {to_pt}: not available")));
        } else {
            if let Some(band) = band_restriction(sheet.cell(row, lower), sheet.cell(row, upper)) {
                coverage.parsed();
                out.push(Rule { id: row_id.to_string(), annex: "Annex 3B", source_text: format!("{from_pt} {to_pt}: {}-{}", sheet.cell(row, lower), sheet.cell(row, upper)), validity: validity.clone(), scope: super::rules::Scope::Edge, key: key.clone(), airport_gate: None, restriction: band });
            }
            let utilization_text = sheet.cell(row, utilization);
            if !utilization_text.trim().is_empty() {
                out.extend(rules_from_cell(row_id, "Annex 3B", key, None, utilization_text, sheet.cell(row, time), valid_from, valid_until, groups, coverage));
            }
        }
    }
    out
}

fn closed_rule(id: &str, key: RuleKey, validity: Validity, text: String) -> Rule {
    Rule { id: id.to_string(), annex: "Annex 3B", source_text: text, validity, scope: super::rules::Scope::Edge, key, airport_gate: None, restriction: Restriction { keyword: Keyword::NotAvailable, include: vec![Clause { terms: Vec::new() }], except: None } }
}

/// A `NOT AVBL` restriction that fires outside `[lower, upper]`, built from the two
/// vertical limit cells. `None` when neither cell carries a flight level — an
/// unbounded direct with no other restriction, sound to leave unconstrained.
fn band_restriction(lower_cell: &str, upper_cell: &str) -> Option<Restriction> {
    let lower = last_fl(lower_cell);
    let upper = last_fl(upper_cell);
    if lower.is_none() && upper.is_none() {
        return None;
    }
    let mut include = Vec::new();
    if let Some(lower) = lower {
        include.push(Clause { terms: vec![Term::Level(LevelTerm { cmp: LevelCmp::AtOrBelow, ft: lower - BAND_MARGIN_FT, ft2: None, at: None })] });
    }
    if let Some(upper) = upper {
        include.push(Clause { terms: vec![Term::Level(LevelTerm { cmp: LevelCmp::AtOrAbove, ft: upper + BAND_MARGIN_FT, ft2: None, at: None })] });
    }
    Some(Restriction { keyword: Keyword::NotAvailable, include, except: None })
}

/// The last `FLnnn` a cell carries — some rows write two limits run together with no
/// separator (`FL145FL125`), and the second is the one meant; a cell that prefixes it
/// with other text (`MEAFL025`, the minimum enroute altitude) still ends in one.
fn last_fl(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    let mut found = None;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if &bytes[i..i + 2] == b"FL" {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 2 {
                if let Ok(v) = s[i + 2..j].parse::<f64>() {
                    found = Some(v * 100.0);
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    found
}

/// Annex 3B FRA LIM: cross-border direct-routing limits inside Free Route Airspace,
/// which turn on the shape of a Free Route Airspace polygon this crate does not hold.
/// Every active row is counted, honestly, as not evaluated.
pub(crate) fn parse_fra_lim(sheet: &Sheet, when: DateTime<Utc>, coverage: &mut Coverage) {
    let h = sheet.header();
    let cols = ["Change Ind.", "Valid From", "Valid Until", "RAD Application ID", "Cross-border DCT Limits"];
    let Some(idx): Option<Vec<usize>> = cols.iter().map(|c| h.col(c)).collect() else {
        return;
    };
    let (change, from_date, until_date, id, limits) = (idx[0], idx[1], idx[2], idx[3], idx[4]);
    for row in 1..sheet.rows.len() {
        let valid_from = parse_rad_date(sheet.cell(row, from_date));
        let valid_until = parse_rad_date(sheet.cell(row, until_date));
        if !row_active(sheet.cell(row, change), valid_from, valid_until, when) || sheet.cell(row, id).trim().is_empty() {
            continue;
        }
        coverage.skip("Annex 3B FRA LIM: a cross-border Free Route Airspace limit, which turns on airspace geometry this crate does not hold", sheet.cell(row, limits));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use crate::dispatch::EdgeQuery;

    fn dct_sheet() -> Sheet {
        Sheet {
            name: "Annex 3B DCT".into(),
            rows: vec![
                vec![
                    "Change\nInd.".into(),
                    "Valid\nFrom".into(),
                    "Valid\nUntil".into(),
                    "ID".into(),
                    "From".into(),
                    "To".into(),
                    "Lower Vert.\nLimit (FL)".into(),
                    "Upper Vert.\nLimit (FL)".into(),
                    "Available or Not\n(Y/N)".into(),
                    "Utilization".into(),
                    "Time Availability".into(),
                ],
                vec!["AMD".into(), "22 SEP 2026 [2609]".into(), "UFN".into(), "LF5016".into(), "VALKU".into(), "LARON".into(), "FL125".into(), "FL195".into(), "Yes".into(), "".into(), "H24".into()],
                vec!["AMD".into(), "1 JAN 2026".into(), "UFN".into(), "XX0001".into(), "AAA".into(), "BBB".into(), "".into(), "".into(), "No".into(), "".into(), "H24".into()],
            ],
        }
    }

    #[test]
    fn a_band_forbids_outside_it_and_allows_inside() {
        let mut cov = Coverage::default();
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let rules = parse_dct(&dct_sheet(), when, &Groups::default(), &mut cov);
        let band = rules.iter().find(|r| r.id == "LF5016").unwrap();
        let query = |level_ft: f64| EdgeQuery { from: "VALKU", from_pos: (0.0, 0.0), to: "LARON", to_pos: (0.0, 0.0), airway: "DCT", level_ft, when, origin: "LFPG", destination: "LFBO" };
        assert!(band.edge_forbid(&query(10_000.0)).is_some(), "below the band");
        assert!(band.edge_forbid(&query(15_000.0)).is_none(), "inside the band");
        assert!(band.edge_forbid(&query(25_000.0)).is_some(), "above the band");
    }

    #[test]
    fn not_available_forbids_at_any_level() {
        let mut cov = Coverage::default();
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let rules = parse_dct(&dct_sheet(), when, &Groups::default(), &mut cov);
        let closed = rules.iter().find(|r| r.id == "XX0001").unwrap();
        let query = EdgeQuery { from: "AAA", from_pos: (0.0, 0.0), to: "BBB", to_pos: (0.0, 0.0), airway: "DCT", level_ft: 35_000.0, when, origin: "LFPG", destination: "LFBO" };
        assert!(closed.edge_forbid(&query).is_some());
    }

    #[test]
    fn the_last_flight_level_in_a_run_together_cell_wins() {
        assert_eq!(last_fl("FL145FL125"), Some(12_500.0));
        assert_eq!(last_fl("MEAFL025"), Some(2_500.0));
        assert_eq!(last_fl(""), None);
    }

    #[test]
    fn fra_lim_rows_are_all_counted_skipped() {
        let sheet = Sheet {
            name: "Annex 3B FRA LIM".into(),
            rows: vec![
                vec!["Change\nInd.".into(), "Valid\nFrom".into(), "Valid\nUntil".into(), "RAD Application ID".into(), "Cross-border\nDCT Limits".into()],
                vec!["".into(), "27 NOV 2025 [2512]".into(), "UFN".into(), "LFFRAC_FRA".into(), "NOT ALW EXC 1. X".into()],
            ],
        };
        let mut cov = Coverage::default();
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        parse_fra_lim(&sheet, when, &mut cov);
        assert_eq!(cov.counts(), (0, 1));
    }
}
