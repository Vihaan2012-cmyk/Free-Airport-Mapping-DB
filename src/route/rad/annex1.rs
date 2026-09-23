//! Annex 1: the named groups of aerodromes the other annexes refer to instead of
//! spelling four codes out every time — `AJACCIO_GROUP` for `(LFKF, LFKG, LFKJ, LFKO)`.

use super::model::{parse_rad_date, row_active, Groups, Matcher};
use super::xlsx::Sheet;
use chrono::{DateTime, Utc};

/// Every group defined and in force at `when`. A group's own membership rarely has a
/// time-of-day or day-of-week qualifier, so only the AIRAC dates and the Change
/// Indicator are read.
pub(crate) fn parse(sheet: &Sheet, when: DateTime<Utc>) -> Groups {
    let mut groups = Groups::default();
    let h = sheet.header();
    let (Some(change), Some(from), Some(until), Some(id), Some(def)) = (h.col("Change Ind."), h.col("Valid From"), h.col("Valid Until"), h.col("ID"), h.col("Definition")) else {
        return groups;
    };
    for row in 1..sheet.rows.len() {
        let from_date = parse_rad_date(sheet.cell(row, from));
        let until_date = parse_rad_date(sheet.cell(row, until));
        if !row_active(sheet.cell(row, change), from_date, until_date, when) {
            continue;
        }
        let name = sheet.cell(row, id).trim();
        if name.is_empty() {
            continue;
        }
        if let Some(members) = parse_definition(sheet.cell(row, def)) {
            groups.insert(name, members);
        }
    }
    groups
}

/// A definition cell: `(LFKF, LFKG, LFKJ, LFKO)`, a comma-separated list inside
/// parentheses, each a code or a wildcard.
fn parse_definition(s: &str) -> Option<Vec<Matcher>> {
    let inner = s.trim().strip_prefix('(')?.strip_suffix(')')?;
    let members: Vec<Matcher> = inner
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            let stripped = t.trim_end_matches('*');
            if stripped.len() < t.len() && !stripped.is_empty() {
                Matcher::Prefix(stripped.to_uppercase())
            } else {
                Matcher::Exact(t.to_uppercase())
            }
        })
        .collect();
    (!members.is_empty()).then_some(members)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::rad::xlsx::Sheet;
    use chrono::TimeZone;

    fn sheet() -> Sheet {
        Sheet {
            name: "Annex 1".into(),
            rows: vec![
                vec!["Change\nInd.".into(), "Valid\nFrom".into(), "Valid\nUntil".into(), "ID".into(), "Definition".into()],
                vec!["".into(), "23 MAR 2023 [2303]".into(), "UFN".into(), "AJACCIO_GROUP".into(), "(LFKF, LFKG, LFKJ, LFKO)".into()],
                vec!["DEL".into(), "1 JAN 2020".into(), "UFN".into(), "OLD_GROUP".into(), "(EGLL)".into()],
                vec!["".into(), "1 JAN 2030".into(), "UFN".into(), "FUTURE_GROUP".into(), "(EDDF)".into()],
            ],
        }
    }

    #[test]
    fn a_group_is_read_and_a_deleted_or_not_yet_valid_one_is_not() {
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let groups = parse(&sheet(), when);
        assert_eq!(groups.resolve("AJACCIO_GROUP").map(|v| v.len()), Some(4));
        assert_eq!(groups.resolve("OLD_GROUP"), None);
        assert_eq!(groups.resolve("FUTURE_GROUP"), None);
    }

    #[test]
    fn a_wildcard_inside_a_definition_is_read_as_a_prefix() {
        let members = parse_definition("(EGLL, LF**)").unwrap();
        assert_eq!(members, vec![Matcher::Exact("EGLL".into()), Matcher::Prefix("LF".into())]);
    }
}
