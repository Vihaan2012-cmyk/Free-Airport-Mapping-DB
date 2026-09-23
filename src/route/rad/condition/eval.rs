//! Turning a parsed restriction into a yes or no: against one segment, asked edge by
//! edge, or against a whole filed route, asked once.

use super::ast::{Clause, Keyword, Restriction, Term};
use crate::route::rad::model::any_match;

/// What a restriction's terms are checked against. A segment on its own only knows its
/// two ends; a whole route also knows every point along it and, where it flies one, its
/// SID and STAR.
pub(crate) trait Ctx {
    fn origin(&self) -> &str;
    fn destination(&self) -> &str;
    fn level_ft(&self) -> f64;
    fn has_point(&self, ident: &str) -> bool;
    fn sid(&self) -> Option<&str> {
        None
    }
    fn star(&self) -> Option<&str> {
        None
    }
}

fn term_matches(term: &Term, ctx: &dyn Ctx) -> bool {
    match term {
        Term::Arr(m) => any_match(m, ctx.destination()),
        Term::Dep(m) => any_match(m, ctx.origin()),
        Term::ArrOrDep(m) => any_match(m, ctx.origin()) || any_match(m, ctx.destination()),
        Term::Via(points) => points.iter().any(|p| ctx.has_point(p)),
        Term::Level(l) => l.matches_level(ctx.level_ft()) && l.at.as_ref().is_none_or(|pts| pts.iter().any(|p| ctx.has_point(p))),
        Term::Sid(names) => ctx.sid().is_some_and(|s| names.iter().any(|n| n.eq_ignore_ascii_case(s))),
        Term::Star(names) => ctx.star().is_some_and(|s| names.iter().any(|n| n.eq_ignore_ascii_case(s))),
    }
}

fn clause_matches(clause: &Clause, ctx: &dyn Ctx) -> bool {
    clause.terms.iter().all(|t| term_matches(t, ctx))
}

fn any_clause(clauses: &[Clause], ctx: &dyn Ctx) -> bool {
    clauses.iter().any(|c| clause_matches(c, ctx))
}

/// Whether a term describes the traffic a rule is for (`ARR`, `DEP`, `SID`, `STAR`) —
/// a fixed fact about a route, either true or false for the whole of it — rather than
/// something the route must go on to do (`VIA` a point, cross a level at one).
fn is_gate_term(t: &Term) -> bool {
    matches!(t, Term::Arr(_) | Term::Dep(_) | Term::ArrOrDep(_) | Term::Sid(_) | Term::Star(_))
}

impl Restriction {
    /// Whether this traffic is shut out by the restriction on its own — never true for
    /// a `Compulsory` keyword, which never forbids anything; it asks for a presence,
    /// answered by `requirement_met`.
    pub(crate) fn forbids(&self, ctx: &dyn Ctx) -> bool {
        let included = any_clause(&self.include, ctx);
        let excepted = self.except.as_ref().is_some_and(|e| any_clause(e, ctx));
        match self.keyword {
            Keyword::NotAvailable => included && !excepted,
            Keyword::OnlyAvailable => !(included && !excepted),
            Keyword::Compulsory | Keyword::OnlyAvailableAndCompulsory => false,
        }
    }

    /// For a `Compulsory` restriction: whether the traffic it names did what it
    /// demands. The caller only asks this once it has already decided the restriction
    /// governs this route at all (an Annex 3A row's own ADEP/ADES, or an Annex 2B row's
    /// point or airway).
    pub(crate) fn requirement_met(&self, ctx: &dyn Ctx) -> bool {
        any_clause(&self.include, ctx) && !self.except.as_ref().is_some_and(|e| any_clause(e, ctx))
    }

    /// For a `Compulsory` restriction: whether it governs this route at all — its
    /// traffic-describing terms match — judged without its `VIA` or level terms, which
    /// describe what the route must still go on to do rather than who it is for. A
    /// clause with no traffic-describing term at all (everything is external, an Annex
    /// 3A row's own ADEP/ADES column) governs every route the caller has already gated.
    pub(crate) fn compulsory_applies(&self, ctx: &dyn Ctx) -> bool {
        self.include.iter().any(|c| c.terms.iter().filter(|t| is_gate_term(t)).all(|t| term_matches(t, ctx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::rad::condition::parser::parse_restriction;
    use crate::route::rad::model::Groups;

    struct Fake {
        origin: &'static str,
        destination: &'static str,
        level_ft: f64,
        points: &'static [&'static str],
    }

    impl Ctx for Fake {
        fn origin(&self) -> &str {
            self.origin
        }
        fn destination(&self) -> &str {
            self.destination
        }
        fn level_ft(&self) -> f64 {
            self.level_ft
        }
        fn has_point(&self, ident: &str) -> bool {
            self.points.contains(&ident)
        }
    }

    #[test]
    fn not_available_forbids_only_the_traffic_it_names() {
        let r = parse_restriction("Not available for traffic ARR EGLL, EGKK", &Groups::default()).unwrap();
        let matches = Fake { origin: "LFPG", destination: "EGLL", level_ft: 35_000.0, points: &[] };
        let other = Fake { origin: "LFPG", destination: "EDDF", level_ft: 35_000.0, points: &[] };
        assert!(r.forbids(&matches));
        assert!(!r.forbids(&other));
    }

    #[test]
    fn only_available_forbids_everyone_else() {
        let r = parse_restriction("Only available for traffic DEP EDDF", &Groups::default()).unwrap();
        let matches = Fake { origin: "EDDF", destination: "LFPG", level_ft: 35_000.0, points: &[] };
        let other = Fake { origin: "EDDL", destination: "LFPG", level_ft: 35_000.0, points: &[] };
        assert!(!r.forbids(&matches));
        assert!(r.forbids(&other));
    }

    #[test]
    fn an_exception_is_carved_back_out() {
        let r = parse_restriction("Not available for traffic DEP EDDF EXC ARR EGLL", &Groups::default()).unwrap();
        let excepted = Fake { origin: "EDDF", destination: "EGLL", level_ft: 35_000.0, points: &[] };
        let not_excepted = Fake { origin: "EDDF", destination: "LFPG", level_ft: 35_000.0, points: &[] };
        assert!(!r.forbids(&excepted));
        assert!(r.forbids(&not_excepted));
    }

    #[test]
    fn a_level_qualifier_only_bites_above_its_floor() {
        let r = parse_restriction("Only available for traffic DEP LF** above FL245", &Groups::default()).unwrap();
        let high = Fake { origin: "LFPG", destination: "EGLL", level_ft: 35_000.0, points: &[] };
        let low = Fake { origin: "LFPG", destination: "EGLL", level_ft: 20_000.0, points: &[] };
        assert!(!r.forbids(&high));
        assert!(r.forbids(&low));
    }

    #[test]
    fn compulsory_never_forbids_but_states_a_requirement() {
        let r = parse_restriction("Compulsory for traffic DEP EDDF VIA ABC", &Groups::default()).unwrap();
        let through = Fake { origin: "EDDF", destination: "LFPG", level_ft: 35_000.0, points: &["ABC"] };
        let not_through = Fake { origin: "EDDF", destination: "LFPG", level_ft: 35_000.0, points: &[] };
        assert!(!r.forbids(&through) && !r.forbids(&not_through));
        assert!(r.requirement_met(&through));
        assert!(!r.requirement_met(&not_through));
    }
}
