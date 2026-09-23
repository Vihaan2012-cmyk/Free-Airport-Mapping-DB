//! One parsed rule, whichever annex it came from: what it is keyed to, when it applies,
//! and how it is checked — edge by edge in the hot loop, or once against a whole route.

use super::condition::{self, Ctx, Restriction};
use super::coverage::Coverage;
use super::model::{any_match, parse_time_applicability, Groups, Matcher, Validity};
use crate::dispatch::{EdgeQuery, FiledRoute, Violation};
use chrono::NaiveDate;

/// What a rule is keyed to: a specific airway segment (or, with `from`/`to` both
/// empty, that airway or conditional route's designator wherever it is flown), or a
/// single point — a fix, a beacon, an aerodrome's DCT entry point — the segment or
/// route touches.
#[derive(Debug, Clone)]
pub(crate) enum RuleKey {
    Airway { airway: String, from: String, to: String },
    Point(String),
}

impl RuleKey {
    fn edge_matches(&self, q: &EdgeQuery) -> bool {
        match self {
            RuleKey::Airway { airway, from, to } => {
                if !q.airway.eq_ignore_ascii_case(airway) {
                    return false;
                }
                if from.is_empty() && to.is_empty() {
                    return true;
                }
                (q.from.eq_ignore_ascii_case(from) && q.to.eq_ignore_ascii_case(to)) || (q.from.eq_ignore_ascii_case(to) && q.to.eq_ignore_ascii_case(from))
            }
            RuleKey::Point(p) => q.from.eq_ignore_ascii_case(p) || q.to.eq_ignore_ascii_case(p),
        }
    }

    fn route_matches(&self, route: &FiledRoute) -> bool {
        match self {
            RuleKey::Airway { airway, .. } => route.points.iter().any(|w| w.via.eq_ignore_ascii_case(airway)),
            RuleKey::Point(p) => route.points.iter().any(|w| w.ident.eq_ignore_ascii_case(p)),
        }
    }

    fn label(&self) -> Option<String> {
        match self {
            RuleKey::Airway { airway, .. } if !airway.is_empty() => Some(airway.clone()),
            RuleKey::Point(p) if !p.is_empty() => Some(p.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Checked edge by edge, level by level, in the search's hot loop.
    Edge,
    /// Checked once, against the whole filed route.
    Route,
}

/// One restriction, resolved to a rule: where it is checked, and against what.
pub(crate) struct Rule {
    pub id: String,
    pub annex: &'static str,
    pub source_text: String,
    pub validity: Validity,
    pub scope: Scope,
    pub key: RuleKey,
    /// Annex 3A only: a row's own ARR AD/DEP AD column, gating the restriction to one
    /// direction at one aerodrome before its condition text is even read.
    pub airport_gate: Option<(bool, Vec<Matcher>)>,
    pub restriction: Restriction,
}

struct EdgeCtx<'a>(&'a EdgeQuery<'a>);

impl Ctx for EdgeCtx<'_> {
    fn origin(&self) -> &str {
        self.0.origin
    }
    fn destination(&self) -> &str {
        self.0.destination
    }
    fn level_ft(&self) -> f64 {
        self.0.level_ft
    }
    fn has_point(&self, ident: &str) -> bool {
        self.0.from.eq_ignore_ascii_case(ident) || self.0.to.eq_ignore_ascii_case(ident)
    }
}

struct RouteCtx<'a>(&'a FiledRoute);

impl Ctx for RouteCtx<'_> {
    fn origin(&self) -> &str {
        &self.0.origin.icao
    }
    fn destination(&self) -> &str {
        &self.0.destination.icao
    }
    fn level_ft(&self) -> f64 {
        self.0.cruise_ft
    }
    fn has_point(&self, ident: &str) -> bool {
        self.0.points.iter().any(|w| w.ident.eq_ignore_ascii_case(ident))
    }
    fn sid(&self) -> Option<&str> {
        self.0.sid.as_deref()
    }
    fn star(&self) -> Option<&str> {
        self.0.star.as_deref()
    }
}

fn gate_ok(gate: &Option<(bool, Vec<Matcher>)>, origin: &str, destination: &str) -> bool {
    match gate {
        None => true,
        Some((arrival, matchers)) => any_match(matchers, if *arrival { destination } else { origin }),
    }
}

fn violation(rule: &Rule) -> Violation {
    Violation { rule: format!("{} {}", rule.annex, rule.id), at: rule.key.label(), message: rule.source_text.clone() }
}

impl Rule {
    /// Stage one, `EdgeRule::check`: whether this edge, at this level and time, is shut
    /// by the rule. `None` when the rule does not even bear on this edge.
    pub(crate) fn edge_forbid(&self, q: &EdgeQuery) -> Option<String> {
        if self.scope != Scope::Edge || !self.validity.active_at(q.when) || !self.key.edge_matches(q) || !gate_ok(&self.airport_gate, q.origin, q.destination) {
            return None;
        }
        let ctx = EdgeCtx(q);
        self.restriction.forbids(&ctx).then(|| format!("{} {}: {}", self.annex, self.id, self.source_text))
    }

    /// Stage two, `RouteRule::check_route`: a rule judged on the whole route —
    /// something a `Compulsory` keyword requires be present somewhere in it, or a
    /// `NOT`/`ONLY AVBL` rule this crate could only promote to Route scope, most often
    /// because its clause names a SID or STAR the airway graph never sees.
    pub(crate) fn route_violation(&self, route: &FiledRoute) -> Option<Violation> {
        if self.scope != Scope::Route || !self.validity.active_at(route.off_block) || !gate_ok(&self.airport_gate, &route.origin.icao, &route.destination.icao) {
            return None;
        }
        let ctx = RouteCtx(route);
        if self.restriction.keyword.is_compulsory() {
            if !self.restriction.compulsory_applies(&ctx) || self.restriction.requirement_met(&ctx) {
                None
            } else {
                Some(violation(self))
            }
        } else {
            if !self.key.route_matches(route) {
                return None;
            }
            self.restriction.forbids(&ctx).then(|| violation(self))
        }
    }
}

/// Whether a Scope::Edge rule is possible for a key, or the rule must wait for the
/// whole route: a `Compulsory` restriction is always judged on the whole route (it asks
/// whether something happened anywhere in it, not whether one edge is open), and so is
/// any restriction that names a SID or STAR, which the airway graph never carries.
fn scope_for(key: &RuleKey, restriction: &Restriction) -> Scope {
    let keyed = key.label().is_some();
    if keyed && !restriction.keyword.is_compulsory() && !restriction.uses_sid_or_star() {
        Scope::Edge
    } else {
        Scope::Route
    }
}

/// One row's condition cell, which may hold several restrictions separated by a line
/// of dashes, turned into rules — one per block that parses, a count and a sample kept
/// for every block that does not.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rules_from_cell(id: &str, annex: &'static str, key: RuleKey, airport_gate: Option<(bool, Vec<Matcher>)>, utilization: &str, time_applicability: &str, from: Option<NaiveDate>, until: Option<NaiveDate>, groups: &Groups, coverage: &mut Coverage) -> Vec<Rule> {
    let mut out = Vec::new();
    for (i, (text, time_text)) in condition::split_blocks(utilization, time_applicability).into_iter().enumerate() {
        match condition::parse_restriction(&text, groups) {
            Ok(restriction) => {
                let Some(time) = parse_time_applicability(&time_text) else {
                    coverage.skip("a time applicability this does not parse (most often a season named by AIRAC rather than a date)", &time_text);
                    continue;
                };
                coverage.parsed();
                let scope = scope_for(&key, &restriction);
                let rule_id = if i == 0 { id.to_string() } else { format!("{id}.{}", i + 1) };
                out.push(Rule { id: rule_id, annex, source_text: text, validity: Validity { from, until, time }, scope, key: key.clone(), airport_gate: airport_gate.clone(), restriction });
            }
            Err(e) => coverage.skip(e.0, &text),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{Airport, PointKind, Waypoint};
    use chrono::{TimeZone, Utc};

    fn q<'a>(from: &'a str, to: &'a str, airway: &'a str, origin: &'a str, destination: &'a str, level_ft: f64) -> EdgeQuery<'a> {
        EdgeQuery { from, from_pos: (0.0, 0.0), to, to_pos: (0.0, 0.0), airway, level_ft, when: Utc.with_ymd_and_hms(2026, 9, 23, 12, 0, 0).unwrap(), origin, destination }
    }

    fn groups() -> Groups {
        Groups::default()
    }

    #[test]
    fn a_point_keyed_rule_forbids_only_the_traffic_and_the_edge_it_names() {
        let mut cov = Coverage::default();
        let rules = rules_from_cell("X1", "2B", RuleKey::Point("ABC".into()), None, "NOT AVBL FOR TFC ARR EGLL", "H24", None, None, &groups(), &mut cov);
        assert_eq!(rules.len(), 1);
        let r = &rules[0];
        assert_eq!(r.scope, Scope::Edge);
        assert!(r.edge_forbid(&q("XYZ", "ABC", "UN1", "LFPG", "EGLL", 35_000.0)).is_some());
        assert!(r.edge_forbid(&q("XYZ", "DEF", "UN1", "LFPG", "EGLL", 35_000.0)).is_none(), "a different point");
        assert!(r.edge_forbid(&q("XYZ", "ABC", "UN1", "LFPG", "EDDF", 35_000.0)).is_none(), "a different destination");
    }

    #[test]
    fn a_compulsory_rule_only_applies_to_the_traffic_it_names_and_checks_the_whole_route() {
        let mut cov = Coverage::default();
        let gate = Some((true, vec![Matcher::Exact("LEMG".into())]));
        let rules = rules_from_cell("Y1", "3A", RuleKey::Point(String::new()), gate, "COMPULSORY FOR TFC VIA BLN", "H24", None, None, &groups(), &mut cov);
        assert_eq!(rules.len(), 1);
        let r = &rules[0];
        assert_eq!(r.scope, Scope::Route);
        let origin = Airport { icao: "LFPG".into(), name: String::new(), pos: (0.0, 0.0), elevation_ft: 0.0 };
        let via_bln = FiledRoute {
            origin: origin.clone(),
            destination: Airport { icao: "LEMG".into(), name: String::new(), pos: (0.0, 0.0), elevation_ft: 0.0 },
            dep_runway: None,
            sid: None,
            sid_transition: None,
            star: None,
            star_transition: None,
            arr_runway: None,
            approach: None,
            points: vec![Waypoint::new("BLN", (0.0, 0.0), "DCT", PointKind::Enroute)],
            cruise_ft: 35_000.0,
            off_block: Utc.with_ymd_and_hms(2026, 9, 23, 12, 0, 0).unwrap(),
        };
        assert!(r.route_violation(&via_bln).is_none(), "the compulsory point was flown");
        let not_via = FiledRoute { points: vec![], ..via_bln.clone() };
        assert!(r.route_violation(&not_via).is_some(), "the compulsory point was not flown");
        let elsewhere = FiledRoute { destination: origin.clone(), points: vec![], ..not_via };
        assert!(r.route_violation(&elsewhere).is_none(), "not even an arrival at LEMG, so the rule never applied");
    }

    #[test]
    fn an_unparseable_block_is_counted_not_guessed_at() {
        let mut cov = Coverage::default();
        let rules = rules_from_cell("Z1", "2B", RuleKey::Point("X".into()), None, "Reduced separation applies here", "H24", None, None, &groups(), &mut cov);
        assert!(rules.is_empty());
        assert_eq!(cov.counts(), (0, 1));
    }
}
