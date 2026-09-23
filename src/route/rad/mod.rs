//! Europe's Route Availability Document: the rules the Network Manager checks every
//! flight plan against before it accepts it.
//!
//! EUROCONTROL publishes it free, every AIRAC cycle and every amendment inside one, as
//! a single Excel workbook — one tab per annex — from `nm.eurocontrol.int/RAD/`, no
//! login required (`fetch`). Annex 1 defines the areas and named groups of aerodromes
//! the other annexes refer to (`annex1`); 2A caps flight levels (`annex2a`); 2B holds
//! the bulk of it, the local and cross-border restrictions (`annex2b`); 2C ties
//! restrictions to conditional routes and flexible use of airspace (`annex2c`); 3A is
//! aerodrome connectivity (`annex3a`) and 3B the limits on direct routing (`annex3b`).
//!
//! Every one of those writes its restrictions as semi-structured English — "Not
//! available for traffic ARR EGLL, EGKK", "Only available for traffic DEP LF** above
//! FL245" — read by a small condition language of its own (`condition`) built to fail
//! to parse an unfamiliar phrasing rather than silently misread it. What a rule cannot
//! be parsed into is counted, not applied (`coverage`), and `Rad::coverage` says how
//! much of the document was actually checked.
//!
//! What only needs a segment, its level, its time, and the flight's origin and
//! destination is checked edge by edge, in the search's hot loop (`EdgeRule`, indexed
//! by airway and by point so a lookup is never a scan). What needs the rest of the
//! route — a `COMPULSORY` requirement, a restriction that names a SID or STAR the
//! airway graph never carries a fix for — is checked once, against the whole filed
//! route (`RouteRule`).

mod annex1;
mod annex2a;
mod annex2b;
mod annex2c;
mod annex3a;
mod annex3b;
mod condition;
mod coverage;
mod fetch;
mod model;
mod rules;
mod xlsx;

use crate::dispatch::{EdgeQuery, EdgeRule, FiledRoute, RouteRule, Verdict, Violation};
use chrono::{DateTime, Utc};
use coverage::Coverage;
use rules::{Rule, RuleKey, Scope};
use std::collections::HashMap;
use std::path::Path;

/// The RAD in force at a time.
pub struct Rad {
    rules: Vec<Rule>,
    by_point: HashMap<String, Vec<usize>>,
    by_airway: HashMap<String, Vec<usize>>,
    route_rules: Vec<usize>,
    coverage: Coverage,
    edition: Option<String>,
}

impl Rad {
    /// The edition in force at `when`, from Eurocontrol's publication, cached. Every
    /// row of the RAD carries its own Valid From/Valid Until dates — it is a "Rolling
    /// RAD Document" — so the one workbook Eurocontrol currently publishes already
    /// carries whatever is due to change in cycles still to come, and answers for any
    /// `when` this is likely to be asked about, past or near future alike.
    ///
    /// Downloaded, and cached, unless the `AMDB_RAD_FILE` environment variable names a
    /// file or a folder to read it from instead.
    pub fn load(when: DateTime<Utc>) -> anyhow::Result<Rad> {
        let http = crate::sources::http::Http::new(30, 400);
        let cache = fetch::default_cache();
        let bytes = fetch::workbook_bytes(&http, &cache)?;
        Rad::from_bytes(&bytes, when)
    }

    /// The RAD read from a workbook already on disk: a file, or a folder to find one
    /// `.xlsx` in.
    pub fn from_path(path: impl AsRef<Path>, when: DateTime<Utc>) -> anyhow::Result<Rad> {
        let bytes = fetch::read_path(path.as_ref())?;
        Rad::from_bytes(&bytes, when)
    }

    fn from_bytes(bytes: &[u8], when: DateTime<Utc>) -> anyhow::Result<Rad> {
        let workbook = xlsx::Workbook::read(bytes)?;
        let mut coverage = Coverage::default();
        let groups = workbook.sheet("Annex 1").map(|s| annex1::parse(s, when)).unwrap_or_default();
        let mut rules = Vec::new();
        if let Some(s) = workbook.sheet("Annex 2A") {
            rules.extend(annex2a::parse(s, when, &groups, &mut coverage));
        }
        if let Some(s) = workbook.sheet("Annex 2B") {
            rules.extend(annex2b::parse(s, when, &groups, &mut coverage));
        }
        if let Some(s) = workbook.sheet("Annex 2C") {
            rules.extend(annex2c::parse(s, when, &groups, &mut coverage));
        }
        if let Some(s) = workbook.sheet("Annex 3A ARR") {
            rules.extend(annex3a::parse(s, true, when, &groups, &mut coverage));
        }
        if let Some(s) = workbook.sheet("Annex 3A DEP") {
            rules.extend(annex3a::parse(s, false, when, &groups, &mut coverage));
        }
        if let Some(s) = workbook.sheet("Annex 3B DCT") {
            rules.extend(annex3b::parse_dct(s, when, &groups, &mut coverage));
        }
        if let Some(s) = workbook.sheet("Annex 3B FRA LIM") {
            annex3b::parse_fra_lim(s, when, &mut coverage);
        }
        let edition = workbook.sheet("Cover").and_then(|s| s.rows.first()).and_then(|r| r.first()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Ok(Rad::index(rules, coverage, edition))
    }

    fn index(rules: Vec<Rule>, coverage: Coverage, edition: Option<String>) -> Rad {
        let mut by_point: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_airway: HashMap<String, Vec<usize>> = HashMap::new();
        let mut route_rules = Vec::new();
        for (i, rule) in rules.iter().enumerate() {
            match (rule.scope, &rule.key) {
                (Scope::Edge, RuleKey::Point(p)) => by_point.entry(p.to_uppercase()).or_default().push(i),
                (Scope::Edge, RuleKey::Airway { airway, .. }) => by_airway.entry(airway.to_uppercase()).or_default().push(i),
                (Scope::Route, _) => route_rules.push(i),
            }
        }
        Rad { rules, by_point, by_airway, route_rules, coverage, edition }
    }

    /// The edition read off the workbook's cover sheet, such as `"RAD 2609 V1.23
    /// (Rolling RAD Document)"`, where one was found.
    pub fn edition(&self) -> Option<&str> {
        self.edition.as_deref()
    }

    /// How many rows of the RAD parsed into a rule this crate checks, and how many
    /// were skipped, honestly, rather than risk being misread. The fraction is the
    /// measure of how much of the document a route was actually checked against.
    pub fn coverage(&self) -> (usize, usize) {
        self.coverage.counts()
    }

    /// Why rows were skipped, the most common reason first, with a few example texts
    /// for each — for the flight plan to print alongside the coverage fraction.
    pub fn skipped_reasons(&self) -> Vec<(String, usize, Vec<String>)> {
        self.coverage.skipped_reasons()
    }
}

impl EdgeRule for Rad {
    fn name(&self) -> &str {
        "RAD"
    }

    /// Every rule keyed to either end of the segment or to its airway — never a scan
    /// of the whole rule set, which is what keeps this fast enough for the search's hot
    /// loop. The first rule that forbids the segment ends the search; the RAD is a
    /// matter of open or shut, never of degree, so nothing here ever returns
    /// `Verdict::Penalise`.
    fn check(&self, q: &EdgeQuery) -> Verdict {
        let candidates = self.by_point.get(q.from).into_iter().chain(self.by_point.get(q.to)).chain(self.by_airway.get(q.airway)).flatten();
        for &i in candidates {
            if let Some(reason) = self.rules[i].edge_forbid(q) {
                return Verdict::Forbid(reason);
            }
        }
        Verdict::Allow
    }
}

impl RouteRule for Rad {
    fn name(&self) -> &str {
        "RAD"
    }

    fn check_route(&self, route: &FiledRoute) -> Vec<Violation> {
        self.route_rules.iter().filter_map(|&i| self.rules[i].route_violation(route)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{Airport, PointKind, Waypoint};
    use super::xlsx::tests::build as build_xlsx;
    use chrono::TimeZone;

    fn header_2b() -> &'static [&'static str] {
        &["Change\nInd.", "Valid\nFrom", "Valid\nUntil", "ID", "Airway", "From", "To", "Point or\nAirspace", "Utilization", "Time\nApplicability"]
    }

    fn header_1() -> &'static [&'static str] {
        &["Change\nInd.", "Valid\nFrom", "Valid\nUntil", "ID", "Definition"]
    }

    /// A tiny end-to-end workbook: one Annex 1 group, and two Annex 2B rows — one that
    /// parses and forbids an edge, one written in a phrasing this crate refuses.
    fn tiny_workbook() -> Vec<u8> {
        build_xlsx(&[
            ("Annex 1", &[header_1(), &["", "1 JAN 2020", "UFN", "TEST_GROUP", "(EGLL, EGKK)"]]),
            (
                "Annex 2B",
                &[
                    header_2b(),
                    &["AMD", "1 JAN 2026", "UFN", "T1", "", "", "", "ABC", "NOT AVBL FOR TFC\nARR TEST_GROUP", "H24"],
                    &["AMD", "1 JAN 2026", "UFN", "T2", "", "", "", "DEF", "Reduced separation applies here", "H24"],
                ],
            ),
        ])
    }

    #[test]
    fn a_tiny_workbook_reads_end_to_end_and_forbids_what_it_parsed() {
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 12, 0, 0).unwrap();
        let rad = Rad::from_bytes(&tiny_workbook(), when).unwrap();
        assert_eq!(rad.coverage(), (1, 1));
        assert!(rad.skipped_reasons().iter().any(|(_, n, samples)| *n == 1 && !samples.is_empty()));

        let forbidden = EdgeQuery { from: "XYZ", from_pos: (0.0, 0.0), to: "ABC", to_pos: (0.0, 0.0), airway: "UN1", level_ft: 35_000.0, when, origin: "LFPG", destination: "EGLL" };
        assert!(matches!(rad.check(&forbidden), Verdict::Forbid(_)));

        let allowed = EdgeQuery { destination: "EDDF", ..forbidden };
        assert!(matches!(rad.check(&allowed), Verdict::Allow));
    }

    #[test]
    fn a_compulsory_rule_is_checked_once_against_the_whole_route_not_edge_by_edge() {
        let header = &["Change\nInd.", "Valid\nFrom", "Valid\nUntil", "ARR ID", "ARR AD", "First PT STAR /\nSTAR ID", "DCT ARR PT", "ARR FPL Option", "ARR Time\nApplicability"];
        let bytes = build_xlsx(&[("Annex 3A ARR", &[header, &["AMD", "1 JAN 2026", "UFN", "R1", "LEMG", "", "BLN", "COMPULSORY FOR TFC\nVIA BLN", "H24"]])]);
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 12, 0, 0).unwrap();
        let rad = Rad::from_bytes(&bytes, when).unwrap();
        assert_eq!(rad.coverage(), (1, 0));

        let origin = Airport { icao: "LFPG".into(), name: String::new(), pos: (0.0, 0.0), elevation_ft: 0.0 };
        let destination = Airport { icao: "LEMG".into(), name: String::new(), pos: (0.0, 0.0), elevation_ft: 0.0 };
        let base = FiledRoute {
            origin,
            destination,
            dep_runway: None,
            sid: None,
            sid_transition: None,
            star: None,
            star_transition: None,
            arr_runway: None,
            approach: None,
            points: vec![],
            cruise_ft: 35_000.0,
            off_block: when,
        };
        assert!(!rad.check_route(&base).is_empty(), "LEMG arrival never flew the compulsory point");
        let via_bln = FiledRoute { points: vec![Waypoint::new("BLN", (0.0, 0.0), "DCT", PointKind::Enroute)], ..base };
        assert!(rad.check_route(&via_bln).is_empty());
    }

    #[test]
    fn a_workbook_missing_every_annex_sheet_still_loads_empty() {
        let bytes = build_xlsx(&[("Cover", &[&["nothing here"]])]);
        let when = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let rad = Rad::from_bytes(&bytes, when).unwrap();
        assert_eq!(rad.coverage(), (0, 0));
    }

    /// The real thing: a workbook downloaded by hand from
    /// https://www.nm.eurocontrol.int/RAD/ and pointed at with `AMDB_RAD_FILE`. Run by
    /// hand — `cargo test --release --lib route::rad -- --ignored --nocapture` — to see
    /// how much of a real edition this parses, and the commonest phrasings it does not.
    #[test]
    #[ignore]
    fn coverage_against_a_real_edition() {
        let Ok(path) = std::env::var("AMDB_RAD_FILE") else {
            eprintln!("set AMDB_RAD_FILE to a downloaded RAD .xlsx to run this");
            return;
        };
        let when = Utc::now();
        let rad = Rad::from_path(path, when).expect("read the real workbook");
        let (parsed, skipped) = rad.coverage();
        println!("edition: {:?}", rad.edition());
        println!("parsed {parsed}, skipped {skipped} ({:.1}% parsed)", 100.0 * parsed as f64 / (parsed + skipped).max(1) as f64);
        for (reason, n, samples) in rad.skipped_reasons() {
            println!("{n:>5}  {reason}");
            for s in samples {
                println!("         e.g. {s}");
            }
        }
    }
}
