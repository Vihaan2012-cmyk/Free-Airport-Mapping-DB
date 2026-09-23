//! Annex 2B: the local and cross-border restrictions that are the bulk of the RAD — an
//! airway segment, or a single point or airspace, closed, opened or made compulsory to
//! traffic described in free condition text.

use super::coverage::Coverage;
use super::model::{parse_rad_date, row_active, Groups};
use super::rules::{rules_from_cell, Rule, RuleKey};
use super::xlsx::Sheet;
use chrono::{DateTime, Utc};

pub(crate) fn parse(sheet: &Sheet, when: DateTime<Utc>, groups: &Groups, coverage: &mut Coverage) -> Vec<Rule> {
    let mut out = Vec::new();
    let h = sheet.header();
    let cols = ["Change Ind.", "Valid From", "Valid Until", "ID", "Airway", "From", "To", "Point or Airspace", "Utilization", "Time Applicability"];
    let Some(idx): Option<Vec<usize>> = cols.iter().map(|c| h.col(c)).collect() else {
        log::warn!("Annex 2B: expected column not found; skipping the sheet");
        return out;
    };
    let (change, from_date, until_date, id, airway, from, to, point, utilization, time) = (idx[0], idx[1], idx[2], idx[3], idx[4], idx[5], idx[6], idx[7], idx[8], idx[9]);

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
        let Some(key) = row_key(sheet, row, airway, from, to, point) else {
            coverage.skip("Annex 2B: no Airway/From/To and no Point or Airspace to key the row on", sheet.cell(row, utilization));
            continue;
        };
        out.extend(rules_from_cell(row_id, "Annex 2B", key, None, sheet.cell(row, utilization), sheet.cell(row, time), valid_from, valid_until, groups, coverage));
    }
    out
}

fn row_key(sheet: &Sheet, row: usize, airway: usize, from: usize, to: usize, point: usize) -> Option<RuleKey> {
    let airway_id = sheet.cell(row, airway).trim();
    if !airway_id.is_empty() {
        return Some(RuleKey::Airway { airway: airway_id.to_string(), from: sheet.cell(row, from).trim().to_string(), to: sheet.cell(row, to).trim().to_string() });
    }
    let point = sheet.cell(row, point).trim();
    (!point.is_empty()).then(|| RuleKey::Point(point.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn header() -> Vec<String> {
        vec![
            "Change\nInd.".into(),
            "Valid\nFrom".into(),
            "Valid\nUntil".into(),
            "ID".into(),
            "Airway".into(),
            "From".into(),
            "To".into(),
            "Point or\nAirspace".into(),
            "Utilization".into(),
            "Time\nApplicability".into(),
        ]
    }

    #[test]
    fn a_point_keyed_row_and_an_airway_keyed_row_both_parse() {
        let sheet = Sheet {
            name: "Annex 2B".into(),
            rows: vec![
                header(),
                vec!["AMD".into(), "6 AUG 2026 [2608]".into(), "UFN".into(), "ED2056".into(), "".into(), "".into(), "".into(), "EDUUUTA".into(), "NOT AVBL FOR TFC\nDEP EDDF".into(), "H24".into()],
                vec!["AMD".into(), "6 AUG 2026 [2608]".into(), "UFN".into(), "ED2516".into(), "N850".into(), "XAROL".into(), "GISEM".into(), "".into(), "NOT AVBL FOR TFC\nABV FL245".into(), "H24".into()],
            ],
        };
        let mut cov = Coverage::default();
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let rules = parse(&sheet, when, &Groups::default(), &mut cov);
        assert_eq!(rules.len(), 2);
        assert!(matches!(rules[0].key, RuleKey::Point(ref p) if p == "EDUUUTA"));
        assert!(matches!(rules[1].key, RuleKey::Airway { ref airway, ref from, ref to } if airway == "N850" && from == "XAROL" && to == "GISEM"));
    }

    #[test]
    fn a_row_with_nothing_to_key_it_on_is_skipped() {
        let sheet = Sheet { name: "Annex 2B".into(), rows: vec![header(), vec!["".into(), "1 JAN 2026".into(), "UFN".into(), "X1".into(), "".into(), "".into(), "".into(), "".into(), "NOT AVBL FOR TFC\nARR EGLL".into(), "H24".into()]] };
        let mut cov = Coverage::default();
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        assert!(parse(&sheet, when, &Groups::default(), &mut cov).is_empty());
    }
}
