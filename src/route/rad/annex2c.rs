//! Annex 2C: restrictions tied to a conditional route or a piece of flexible-use
//! airspace, filed like an airway designator (`EKD745R`) even though it is really a
//! restricted area's identifier. No column of its own carries a time of day — activation
//! is a matter of the day's AUP/UUP, which this crate has no way to know — so every row
//! reads as in force whenever its AIRAC dates say it is.

use super::coverage::Coverage;
use super::model::{parse_rad_date, row_active, Groups};
use super::rules::{rules_from_cell, Rule, RuleKey};
use super::xlsx::Sheet;
use chrono::{DateTime, Utc};

pub(crate) fn parse(sheet: &Sheet, when: DateTime<Utc>, groups: &Groups, coverage: &mut Coverage) -> Vec<Rule> {
    let mut out = Vec::new();
    let h = sheet.header();
    let cols = ["Change Ind.", "Valid From", "Valid Until", "ID", "AIP RSA ID", "Traffic Flow Rule applied during times and within vertical limits allocated at EAUP/EUUP"];
    let Some(idx): Option<Vec<usize>> = cols.iter().map(|c| h.col(c)).collect() else {
        log::warn!("Annex 2C: expected column not found; skipping the sheet");
        return out;
    };
    let (change, from_date, until_date, id, rsa_id, utilization) = (idx[0], idx[1], idx[2], idx[3], idx[4], idx[5]);

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
        let key_id = {
            let rsa = sheet.cell(row, rsa_id).trim();
            if rsa.is_empty() { row_id } else { rsa }
        };
        let key = RuleKey::Airway { airway: key_id.to_string(), from: String::new(), to: String::new() };
        out.extend(rules_from_cell(row_id, "Annex 2C", key, None, sheet.cell(row, utilization), "H24", valid_from, valid_until, groups, coverage));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_restricted_area_row_is_keyed_by_its_aip_id_and_parses() {
        let sheet = Sheet {
            name: "Annex 2C".into(),
            rows: vec![
                vec!["Change\nInd.".into(), "Valid\nFrom".into(), "Valid\nUntil".into(), "ID".into(), "AIP RSA ID".into(), "Traffic Flow Rule applied\nduring times and within vertical limits allocated at EAUP/EUUP".into()],
                vec!["".into(), "18 AUG 2026 [2608]".into(), "UFN".into(), "EKD745R".into(), "EKD745".into(), "NOT AVBL FOR TFC\nEXC DEP EKBI".into()],
            ],
        };
        let mut cov = Coverage::default();
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let rules = parse(&sheet, when, &Groups::default(), &mut cov);
        assert_eq!(rules.len(), 1);
        assert!(matches!(&rules[0].key, RuleKey::Airway { airway, .. } if airway == "EKD745"));
    }
}
