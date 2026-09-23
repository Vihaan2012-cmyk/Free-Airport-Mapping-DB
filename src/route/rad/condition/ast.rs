//! What a restriction's condition text parses into: a keyword, and the clause of
//! traffic, points and levels it is written against.

use crate::route::rad::model::Matcher;

/// How a restriction constrains whatever it is keyed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keyword {
    /// `NOT AVBL FOR TFC` / `NOT ALW`: closed to traffic matching the clause.
    NotAvailable,
    /// `ONLY AVBL FOR TFC` / `ALW`: open only to traffic matching the clause.
    OnlyAvailable,
    /// `COMPULSORY FOR TFC`: traffic matching the clause must use what this is keyed
    /// to somewhere in its route — a requirement, not a forbid.
    Compulsory,
    /// `ONLY AVBL AND COMPULSORY FOR TFC`: both at once.
    OnlyAvailableAndCompulsory,
}

impl Keyword {
    /// Whether this keyword's sense is "must be used", checked once against a whole
    /// route, rather than "may be used", checked edge by edge.
    pub(crate) fn is_compulsory(self) -> bool {
        matches!(self, Keyword::Compulsory | Keyword::OnlyAvailableAndCompulsory)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LevelCmp {
    Exact,
    AtOrAbove,
    AtOrBelow,
    /// `RFL BTN FL195 AND FL245`: `ft` is the lower bound, `ft2` the upper.
    Between,
}

/// A flight level qualifier: `RFL FL355`, `ABV FL245 AT NOMBO`, `RFL BLW FL315`, `RFL
/// BTN FL195 AND FL245`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LevelTerm {
    pub cmp: LevelCmp,
    pub ft: f64,
    /// The upper bound of a `Between`; unused otherwise.
    pub ft2: Option<f64>,
    /// Where written `AT <point(s)>`: the level only bites where the segment or route
    /// touches one of these.
    pub at: Option<Vec<String>>,
}

impl LevelTerm {
    pub(crate) fn matches_level(&self, level_ft: f64) -> bool {
        // A flight level is written to the nearest hundred feet; a little slack keeps a
        // segment costed at, say, 34,980 ft from missing an "FL350" written exactly.
        const SLACK: f64 = 60.0;
        match self.cmp {
            LevelCmp::Exact => (level_ft - self.ft).abs() < SLACK,
            LevelCmp::AtOrAbove => level_ft >= self.ft - SLACK,
            LevelCmp::AtOrBelow => level_ft <= self.ft + SLACK,
            LevelCmp::Between => level_ft >= self.ft - SLACK && level_ft <= self.ft2.unwrap_or(self.ft) + SLACK,
        }
    }
}

/// One term of a clause: a single condition on the traffic.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Term {
    Arr(Vec<Matcher>),
    Dep(Vec<Matcher>),
    /// `ARR/DEP` or `DEP/ARR`: either end of the flight matches.
    ArrOrDep(Vec<Matcher>),
    /// A point, or an `OR` of points: the segment or route passes through one of them.
    Via(Vec<String>),
    Level(LevelTerm),
    Sid(Vec<String>),
    Star(Vec<String>),
}

/// Terms joined by an implied `AND`: every one of them must hold.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Clause {
    pub terms: Vec<Term>,
}

/// A condition cell, parsed: the keyword, what it is written against, and the `EXC`
/// carved out of that, if there was one. `include` and `except` are each an `OR` of
/// clauses — more than one only where the cell wrote a flat, single-level list of
/// numbered or lettered alternatives.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Restriction {
    pub keyword: Keyword,
    pub include: Vec<Clause>,
    pub except: Option<Vec<Clause>>,
}

impl Restriction {
    /// Whether any clause names a SID or STAR — something the airway graph a segment
    /// query is asked against never carries, so a restriction that turns on one can
    /// only be judged against a whole filed route.
    pub(crate) fn uses_sid_or_star(&self) -> bool {
        fn any(clauses: &[Clause]) -> bool {
            clauses.iter().any(|c| c.terms.iter().any(|t| matches!(t, Term::Sid(_) | Term::Star(_))))
        }
        any(&self.include) || self.except.as_ref().is_some_and(|e| any(e))
    }
}
