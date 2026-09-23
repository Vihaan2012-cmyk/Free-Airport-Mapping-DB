//! Annex 3A: aerodrome connectivity — which arrival and departure points an aerodrome
//! may be joined to the airway network by. Only rows keyed to a real enroute point (the
//! `DCT ARR PT` / `DCT DEP PT` column) are built into rules: a row keyed only by a SID
//! or STAR name (`First PT STAR`, `Last PT SID`) names something the airway graph never
//! carries a fix for, and is counted skipped rather than guessed at.

use super::coverage::Coverage;
use super::model::{parse_matcher_list, parse_rad_date, row_active, Groups};
use super::rules::{rules_from_cell, Rule, RuleKey};
use super::xlsx::Sheet;
use chrono::{DateTime, Utc};

/// Whether the sheet is arrivals (`true`) or departures (`false`) — the two share a
/// shape, `ARR`/`DEP` prefixed the same way in every column name.
pub(crate) fn parse(sheet: &Sheet, arrival: bool, when: DateTime<Utc>, groups: &Groups, coverage: &mut Coverage) -> Vec<Rule> {
    let mut out = Vec::new();
    let h = sheet.header();
    let dir = if arrival { "ARR" } else { "DEP" };
    let ad_col = if arrival { "ARR AD" } else { "DEP AD" };
    let dct_col = if arrival { "DCT ARR PT" } else { "DCT DEP PT" };
    let option_col = if arrival { "ARR FPL Option" } else { "DEP FPL Options" };
    let time_col = format!("{dir} Time Applicability");
    let id_col = format!("{dir} ID");
    let cols = ["Change Ind.", "Valid From", "Valid Until", id_col.as_str(), ad_col, dct_col, option_col, time_col.as_str()];
    let Some(idx): Option<Vec<usize>> = cols.iter().map(|c| h.col(c)).collect() else {
        log::warn!("Annex 3A {dir}: expected column not found; skipping the sheet");
        return out;
    };
    let (change, from_date, until_date, id, ad, dct, option, time) = (idx[0], idx[1], idx[2], idx[3], idx[4], idx[5], idx[6], idx[7]);

    for row in 1..sheet.rows.len() {
        let valid_from = parse_rad_date(sheet.cell(row, from_date));
        let valid_until = parse_rad_date(sheet.cell(row, until_date));
        if !row_active(sheet.cell(row, change), valid_from, valid_until, when) {
            continue;
        }
        let row_id = sheet.cell(row, id).trim();
        if row_id.is_empty() {
            continue;
        }
        let Some(gate) = parse_matcher_list(sheet.cell(row, ad), groups) else {
            coverage.skip("Annex 3A: an ARR/DEP AD this does not resolve to aerodromes", sheet.cell(row, ad));
            continue;
        };
        let points = dct_points(sheet.cell(row, dct));
        if points.is_empty() {
            coverage.skip("Annex 3A: keyed to a SID/STAR name rather than an enroute point, so this cannot be indexed by point", sheet.cell(row, option));
            continue;
        }
        for point in points {
            out.extend(rules_from_cell(row_id, "Annex 3A", RuleKey::Point(point), Some((arrival, gate.clone())), sheet.cell(row, option), sheet.cell(row, time), valid_from, valid_until, groups, coverage));
        }
    }
    out
}

/// The `DCT ARR PT` / `DCT DEP PT` cell: one point, or a parenthesised list of them.
fn dct_points(s: &str) -> Vec<String> {
    let s = s.trim();
    let inner = s.strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or(s);
    inner.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::rad::model::Matcher;
    use chrono::TimeZone;

    fn arr_sheet() -> Sheet {
        Sheet {
            name: "Annex 3A ARR".into(),
            rows: vec![
                vec![
                    "Change\nInd.".into(),
                    "Valid\nFrom".into(),
                    "Valid\nUntil".into(),
                    "ARR ID".into(),
                    "ARR AD".into(),
                    "First PT STAR /\nSTAR ID".into(),
                    "DCT ARR PT".into(),
                    "ARR FPL Option".into(),
                    "ARR Time\nApplicability".into(),
                ],
                vec!["AMD".into(), "9 SEP 2026 [2609]".into(), "UFN".into(), "ED5536".into(), "EDDV".into(), "".into(), "(CEL, NIE, ROBEG, SAS)".into(), "NOT AVBL FOR TFC\nARR EDDL".into(), "H24".into()],
                vec!["AMD".into(), "14 AUG 2026 [2608]".into(), "UFN".into(), "LA5509".into(), "LATI".into(), "DIRES".into(), "".into(), "NOT AVBL FOR TFC\nVIA PAPIZARR LATI".into(), "H24".into()],
            ],
        }
    }

    #[test]
    fn a_point_keyed_row_becomes_one_rule_per_point_gated_to_the_aerodrome() {
        let mut cov = Coverage::default();
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let rules = parse(&arr_sheet(), true, when, &Groups::default(), &mut cov);
        assert_eq!(rules.len(), 4);
        assert!(rules.iter().all(|r| r.airport_gate.as_ref().is_some_and(|(arr, m)| *arr && m == &vec![Matcher::Exact("EDDV".into())])));
    }

    #[test]
    fn a_star_keyed_row_with_no_enroute_point_is_skipped() {
        let mut cov = Coverage::default();
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let rules = parse(&arr_sheet(), true, when, &Groups::default(), &mut cov);
        assert!(rules.iter().all(|r| r.id != "LA5509"));
        assert!(cov.skipped_reasons().iter().any(|(r, n, _)| r.contains("SID/STAR") && *n == 1));
    }
}
