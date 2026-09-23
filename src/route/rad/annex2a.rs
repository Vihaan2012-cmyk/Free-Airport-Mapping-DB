//! Annex 2A: flight level capping — a flow between an aerodrome (or a group of them)
//! and another, over one point, held to a level. Written as a `RFL FL355` or `RFL BLW
//! FL315` against a `From (ADEP)`, `To (ADES)` and `Condition` (`VIA <point>`) rather
//! than the free condition text the other annexes use, so this builds the same
//! `Restriction` the evaluator already knows how to check rather than a text cell.

use super::condition::{parse_level_phrase, parse_via_phrase, Clause, Keyword, Restriction, Term};
use super::coverage::Coverage;
use super::model::{parse_matcher_list, parse_rad_date, parse_time_applicability, row_active, Groups, Validity};
use super::rules::{Rule, RuleKey, Scope};
use super::xlsx::Sheet;
use chrono::{DateTime, Utc};

pub(crate) fn parse(sheet: &Sheet, when: DateTime<Utc>, groups: &Groups, coverage: &mut Coverage) -> Vec<Rule> {
    let mut out = Vec::new();
    let h = sheet.header();
    let cols = ["Change Ind.", "Valid From", "Valid Until", "ID", "From (ADEP)", "To (ADES)", "Condition", "Flight Level Capping", "Time Applicability"];
    let Some(idx): Option<Vec<usize>> = cols.iter().map(|c| h.col(c)).collect() else {
        log::warn!("Annex 2A: expected column not found; skipping the sheet");
        return out;
    };
    let (change, from, until, id, adep, ades, condition, capping, time) = (idx[0], idx[1], idx[2], idx[3], idx[4], idx[5], idx[6], idx[7], idx[8]);

    for row in 1..sheet.rows.len() {
        let from_date = parse_rad_date(sheet.cell(row, from));
        let until_date = parse_rad_date(sheet.cell(row, until));
        if !row_active(sheet.cell(row, change), from_date, until_date, when) {
            continue;
        }
        let row_id = sheet.cell(row, id).trim();
        if row_id.is_empty() {
            continue;
        }
        let text = format!("RFL FL capping: {} {} -> {} {}", sheet.cell(row, adep), sheet.cell(row, condition), sheet.cell(row, ades), sheet.cell(row, capping));
        match build(sheet, row, adep, ades, condition, capping, groups) {
            Some((clause, key)) => {
                let Some(applicability) = parse_time_applicability(sheet.cell(row, time)) else {
                    coverage.skip("a time applicability this does not parse (most often a season named by AIRAC rather than a date)", sheet.cell(row, time));
                    continue;
                };
                coverage.parsed();
                out.push(Rule {
                    id: row_id.to_string(),
                    annex: "Annex 2A",
                    source_text: text,
                    validity: Validity { from: from_date, until: until_date, time: applicability },
                    scope: Scope::Edge,
                    key,
                    airport_gate: None,
                    restriction: Restriction { keyword: Keyword::OnlyAvailable, include: vec![clause], except: None },
                });
            }
            None => coverage.skip("Annex 2A: an ADEP/ADES this does not resolve to aerodromes (most often crossing airspace written where an aerodrome was expected), or a Condition this cannot key a point on", &text),
        }
    }
    out
}

/// The clause a capping row comes down to (`DEP <adep> & ARR <ades> & <level>`, with a
/// `VIA` if the row gives one) and the point the rule is keyed to. A row without a
/// single point in its Condition column has nothing this crate can index it by without
/// scanning every edge, so it is refused rather than applied everywhere.
fn build(sheet: &Sheet, row: usize, adep: usize, ades: usize, condition: usize, capping: usize, groups: &Groups) -> Option<(Clause, RuleKey)> {
    let adep = parse_matcher_list(sheet.cell(row, adep), groups)?;
    let ades = parse_matcher_list(sheet.cell(row, ades), groups)?;
    let level = parse_level_phrase(sheet.cell(row, capping).trim(), groups).ok()?;
    let via = sheet.cell(row, condition).trim();
    let point = if via.is_empty() { None } else { parse_via_phrase(via).ok().filter(|p| p.len() == 1).map(|p| p[0].clone()) }?;
    let mut terms = vec![Term::Dep(adep), Term::Arr(ades)];
    terms.push(Term::Level(level));
    Some((Clause { terms }, RuleKey::Point(point)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sheet() -> Sheet {
        Sheet {
            name: "Annex 2A".into(),
            rows: vec![
                vec![
                    "Change\nInd.".into(),
                    "Valid\nFrom".into(),
                    "Valid\nUntil".into(),
                    "ID".into(),
                    "From\n(ADEP)".into(),
                    "Crossing\nAirspace".into(),
                    "To\n(ADES)".into(),
                    "Condition".into(),
                    "Flight Level\nCapping".into(),
                    "Time\nApplicability".into(),
                ],
                vec!["NEW".into(), "22 SEP 2026".into(), "UFN".into(), "LF4473".into(), "LFBBCTA".into(), "(LE,LF)".into(), "(LEPA,LEIB,LEMH)".into(), "VIA LFBBUSUD".into(), "RFL FL355".into(), "H24".into()],
                vec!["NEW".into(), "22 SEP 2026".into(), "UFN".into(), "LF4999".into(), "EDDF".into(), "".into(), "LFPG".into(), "VIA ABC".into(), "RFL BLW FL315".into(), "H24".into()],
            ],
        }
    }

    #[test]
    fn a_capping_row_with_an_airspace_adep_is_skipped_not_misread() {
        let mut cov = Coverage::default();
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let rules = parse(&sheet(), when, &Groups::default(), &mut cov);
        // Only LF4999 has real four-letter aerodromes at both ends; LF4473's ADEP is a
        // CTA identifier, not an aerodrome, and is correctly refused.
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "LF4999");
        assert_eq!(cov.counts(), (1, 1));
    }

    #[test]
    fn the_cap_forbids_the_flow_above_it() {
        let mut cov = Coverage::default();
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let rules = parse(&sheet(), when, &Groups::default(), &mut cov);
        let r = &rules[0];
        use crate::dispatch::EdgeQuery;
        let high = EdgeQuery { from: "XXX", from_pos: (0.0, 0.0), to: "ABC", to_pos: (0.0, 0.0), airway: "UN1", level_ft: 35_000.0, when, origin: "EDDF", destination: "LFPG" };
        let low = EdgeQuery { level_ft: 30_000.0, ..high };
        assert!(r.edge_forbid(&high).is_some());
        assert!(r.edge_forbid(&low).is_none());
    }
}
