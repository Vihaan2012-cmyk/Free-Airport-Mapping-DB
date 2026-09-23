//! A recursive-descent parser over a condition cell's tokens, bounded deliberately: it
//! knows `ARR`, `DEP`, `VIA`, flight levels, `SID`/`STAR`, one level of `EXC` and one
//! level of numbered or lettered alternatives under it. Text that needs more than that —
//! a sequenced route (`AND-THEN`), a path rather than a point (`VIA (A DCT B)`), a
//! second level of lettered or numbered sub-options — is refused rather than guessed
//! at, and every refusal says why, so the rule is counted as skipped instead of being
//! silently misread.

use super::ast::{Clause, Keyword, LevelCmp, LevelTerm, Restriction, Term};
use super::lexer::{is_reserved, tokenize, Tok};
use crate::route::rad::model::{Groups, Matcher};

#[derive(Debug, Clone)]
pub(crate) struct Unparseable(pub String);

impl std::fmt::Display for Unparseable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerKind {
    Numeric,
    Letter,
}

/// Whether a word is a list marker — `"1."`, `"A."` — and which kind.
fn marker_kind(word: &str) -> Option<MarkerKind> {
    let core = word.strip_suffix('.')?;
    if core.is_empty() {
        return None;
    }
    if core.chars().all(|c| c.is_ascii_digit()) {
        Some(MarkerKind::Numeric)
    } else if core.len() == 1 && core.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        Some(MarkerKind::Letter)
    } else {
        None
    }
}

/// A list where every item is a single ident — no `EGHH/HI/HO/HR` or `KELLY/SOSIM/WAL`,
/// the RAD's compact shorthand for several codes or points sharing everything but a
/// suffix. Reconstructing that shorthand takes knowing where one code ends and the next
/// starts, which is not written down; silently keeping the slash would read three
/// points as one that can never be reached, which forbids or requires nothing while
/// looking as if it does. Refused instead, so it is counted, not misread.
fn no_slash(items: Vec<String>) -> Result<Vec<String>, Unparseable> {
    match items.iter().find(|s| s.contains('/')) {
        Some(bad) => Err(Unparseable(format!("{bad} packs more than one code or point into one word with '/', which this does not expand"))),
        None => Ok(items),
    }
}

fn parse_fl(word: &str) -> Option<f64> {
    let digits = word.strip_prefix("FL")?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse::<f64>().ok().map(|n| n * 100.0)
}

/// `FL245-FL660`, the RAD's own way of writing a band in one word (`BTN FLxxx AND
/// FLyyy`, spelt out, is the other).
fn parse_fl_range(word: &str) -> Option<(f64, f64)> {
    let (lo, hi) = word.split_once('-')?;
    Some((parse_fl(lo)?, parse_fl(hi)?))
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    groups: &'a Groups,
}

impl<'a> Parser<'a> {
    fn new(text: &str, groups: &'a Groups) -> Parser<'a> {
        Parser { toks: tokenize(text), pos: 0, groups }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn word(&self) -> Option<&str> {
        match self.peek() {
            Some(Tok::Word(w)) => Some(w.as_str()),
            _ => None,
        }
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, w: &str) -> bool {
        if self.word() == Some(w) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_seq(&mut self, seq: &[&str]) -> bool {
        let start = self.pos;
        for w in seq {
            if !self.eat_word(w) {
                self.pos = start;
                return false;
            }
        }
        true
    }

    /// One of a set of synonyms for the one word at this position — the RAD itself
    /// abbreviates (`AVBL`, `TFC`); the worked examples that shaped this grammar spell
    /// the same words out (`available`, `traffic`), so both read the same way.
    fn eat_word_alt(&mut self, alts: &[&str]) -> bool {
        if self.word().is_some_and(|w| alts.contains(&w)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_seq_alt(&mut self, seq: &[&[&str]]) -> bool {
        let start = self.pos;
        for alts in seq {
            if !self.eat_word_alt(alts) {
                self.pos = start;
                return false;
            }
        }
        true
    }

    fn peek_marker(&self) -> Option<MarkerKind> {
        self.word().and_then(marker_kind)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    // -- the keyword ---------------------------------------------------------------

    fn parse_keyword(&mut self) -> Result<Keyword, Unparseable> {
        const AVBL: &[&str] = &["AVBL", "AVAILABLE"];
        const TFC: &[&str] = &["TFC", "TRAFFIC"];
        if self.eat_seq_alt(&[&["ONLY"], AVBL, &["AND"], &["COMPULSORY"], &["FOR"], TFC]) {
            return Ok(Keyword::OnlyAvailableAndCompulsory);
        }
        if self.eat_seq_alt(&[&["ONLY"], AVBL, &["FOR"], TFC]) {
            return Ok(Keyword::OnlyAvailable);
        }
        if self.eat_seq_alt(&[&["COMPULSORY"], &["FOR"], TFC]) {
            return Ok(Keyword::Compulsory);
        }
        if self.eat_seq_alt(&[&["NOT"], AVBL, &["FOR"], TFC]) {
            return Ok(Keyword::NotAvailable);
        }
        if self.eat_seq(&["NOT", "ALW"]) {
            return Ok(Keyword::NotAvailable);
        }
        if self.eat_seq(&["ALW"]) {
            return Ok(Keyword::OnlyAvailable);
        }
        Err(Unparseable(format!("no recognised keyword (not available for traffic, only available for traffic, compulsory for traffic, ALW, NOT ALW) at the start of {:?}", self.toks)))
    }

    // -- lists: a single ident, or a parenthesised, comma-separated list of them ----

    /// Raw tokens of a list — the caller turns them into aerodromes, points or names.
    /// A word that fails to end the list at a comma or `)` is a route or a path
    /// written where a plain list was expected, and is refused rather than partly
    /// read.
    fn parse_token_list(&mut self) -> Result<Vec<String>, Unparseable> {
        if self.eat(&Tok::LParen) {
            let mut out = Vec::new();
            loop {
                match self.peek() {
                    Some(Tok::Word(w)) => {
                        out.push(w.clone());
                        self.pos += 1;
                    }
                    other => return Err(Unparseable(format!("expected an item inside ( ), found {other:?}"))),
                }
                if self.eat(&Tok::Comma) {
                    continue;
                }
                if self.eat(&Tok::RParen) {
                    break;
                }
                return Err(Unparseable(format!("expected ',' or ')' in a list, found {:?} — this reads like a route, not a plain list", self.peek())));
            }
            if out.is_empty() {
                return Err(Unparseable("an empty ( )".to_string()));
            }
            return no_slash(out);
        }
        let mut out = Vec::new();
        loop {
            match self.peek() {
                Some(Tok::Word(w)) if !is_reserved(w) && marker_kind(w).is_none() => {
                    out.push(w.clone());
                    self.pos += 1;
                }
                _ => break,
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        if out.is_empty() {
            return Err(Unparseable("expected an aerodrome, point or name".to_string()));
        }
        no_slash(out)
    }

    fn parse_airport_list(&mut self) -> Result<Vec<Matcher>, Unparseable> {
        let toks = self.parse_token_list()?;
        let mut out = Vec::new();
        for t in toks {
            match self.groups.resolve(&t) {
                Some(mut m) => out.append(&mut m),
                None => return Err(Unparseable(format!("{t} is not a four-letter code, a wildcard, or a known Annex 1 group"))),
            }
        }
        Ok(out)
    }

    // -- a flight level qualifier ----------------------------------------------------

    fn try_parse_level(&mut self) -> Result<Option<LevelTerm>, Unparseable> {
        let start = self.pos;
        let had_rfl = self.eat_word("RFL");
        if self.eat_word("BTN") || self.eat_word("BETWEEN") {
            let word = self.word().map(str::to_string);
            let (lo, hi) = if let Some((lo, hi)) = word.as_deref().and_then(parse_fl_range) {
                self.pos += 1;
                (lo, hi)
            } else {
                let lo = word.as_deref().and_then(parse_fl).ok_or_else(|| Unparseable("BTN/BETWEEN not followed by an FLxxx or an FLxxx-FLyyy".to_string()))?;
                self.pos += 1;
                if !self.eat_word("AND") {
                    return Err(Unparseable("BTN/BETWEEN FLxxx not followed by AND FLyyy".to_string()));
                }
                let hi = self.word().and_then(parse_fl).ok_or_else(|| Unparseable("BTN/BETWEEN ... AND not followed by an FLxxx".to_string()))?;
                self.pos += 1;
                (lo, hi)
            };
            let at = if self.eat_word("AT") { Some(self.parse_token_list()?) } else { None };
            return Ok(Some(LevelTerm { cmp: LevelCmp::Between, ft: lo.min(hi), ft2: Some(lo.max(hi)), at }));
        }
        let cmp = if self.eat_word("ABV") || self.eat_word("ABOVE") {
            Some(LevelCmp::AtOrAbove)
        } else if self.eat_word("BLW") || self.eat_word("BELOW") {
            Some(LevelCmp::AtOrBelow)
        } else {
            None
        };
        let Some(ft) = self.word().and_then(parse_fl) else {
            if had_rfl || cmp.is_some() {
                return Err(Unparseable("RFL, ABV or BLW not followed by an FLxxx".to_string()));
            }
            self.pos = start;
            return Ok(None);
        };
        self.pos += 1;
        let at = if self.eat_word("AT") { Some(self.parse_token_list()?) } else { None };
        Ok(Some(LevelTerm { cmp: cmp.unwrap_or(LevelCmp::Exact), ft, ft2: None, at }))
    }

    // -- a term, a clause, and a clause list ------------------------------------------

    fn parse_term(&mut self) -> Result<Term, Unparseable> {
        if self.eat_word("ARR/DEP") || self.eat_word("DEP/ARR") {
            return Ok(Term::ArrOrDep(self.parse_airport_list()?));
        }
        if self.eat_word("ARR") {
            return Ok(Term::Arr(self.parse_airport_list()?));
        }
        if self.eat_word("DEP") {
            return Ok(Term::Dep(self.parse_airport_list()?));
        }
        if self.eat_word("VIA") {
            return Ok(Term::Via(self.parse_token_list()?));
        }
        if self.eat_word("SID") {
            return Ok(Term::Sid(self.parse_token_list()?));
        }
        if self.eat_word("STAR") {
            return Ok(Term::Star(self.parse_token_list()?));
        }
        if let Some(level) = self.try_parse_level()? {
            return Ok(Term::Level(level));
        }
        Err(Unparseable(format!("expected ARR, DEP, VIA, RFL/ABV/BLW, SID or STAR, found {:?}", self.peek())))
    }

    /// Zero or more terms, `AND`ed. Empty is legal — `NOT AVBL FOR TFC EXC DEP EKBI`
    /// closes everything except the exception, with nothing of its own written before
    /// `EXC` — and is read as vacuously true, which is exactly that sense: every clause
    /// in an empty `OR` still has to hold, and an empty `AND` holds trivially.
    fn parse_clause(&mut self) -> Result<Clause, Unparseable> {
        let mut terms = Vec::new();
        loop {
            match self.peek() {
                None => break,
                Some(Tok::Word(w)) if w == "EXC" || w == "EXCEPT" => break,
                _ if self.peek_marker().is_some() => break,
                _ => {
                    terms.push(self.parse_term()?);
                    self.eat(&Tok::Amp);
                    self.eat_word("AND");
                    self.eat_word("WITH");
                }
            }
        }
        Ok(Clause { terms })
    }

    /// One clause, or a flat `OR` of them where the cell wrote a single level of
    /// numbered (`1.`, `2.` …) or lettered (`a.`, `b.` …) alternatives. A second level
    /// nested inside one of them is a marker of the other kind, or the same kind seen
    /// again where none was expected, and is refused by the mismatch this produces one
    /// level up.
    fn parse_clause_list(&mut self) -> Result<Vec<Clause>, Unparseable> {
        let Some(kind0) = self.peek_marker() else {
            return Ok(vec![self.parse_clause()?]);
        };
        let mut clauses = Vec::new();
        while let Some(kind) = self.peek_marker() {
            if kind != kind0 {
                return Err(Unparseable("an enumeration mixes numbered and lettered markers, or nests a second level".to_string()));
            }
            self.pos += 1;
            clauses.push(self.parse_clause()?);
        }
        Ok(clauses)
    }
}

/// A condition cell, parsed whole: the keyword, what it includes, and what an `EXC`
/// carves back out. Every token must be accounted for — anything left over is exactly
/// the unfamiliar phrasing this is built to refuse rather than misread.
pub(crate) fn parse_restriction(text: &str, groups: &Groups) -> Result<Restriction, Unparseable> {
    let mut p = Parser::new(text, groups);
    let keyword = p.parse_keyword()?;
    let include = p.parse_clause_list()?;
    let except = if p.eat_word("EXC") || p.eat_word("EXCEPT") { Some(p.parse_clause_list()?) } else { None };
    if !p.at_end() {
        return Err(Unparseable(format!("unread text after the condition: {:?}", &p.toks[p.pos..])));
    }
    // A bare keyword, with nothing to say what it is for and nothing carved back out,
    // is too little to tell "for everyone" from "the cell is just incomplete" apart —
    // Annex 3A writes rows exactly this way to register a connectivity option rather
    // than state a restriction, and this must not read one as a blanket closure.
    if except.is_none() && include.iter().all(|c| c.terms.is_empty()) {
        return Err(Unparseable("a bare keyword with nothing after it and no EXC".to_string()));
    }
    Ok(Restriction { keyword, include, except })
}

/// A flight level qualifier on its own, as Annex 2A's Flight Level Capping column
/// writes it: `RFL FL355`, `RFL BLW FL315`.
pub(crate) fn parse_level_phrase(text: &str, groups: &Groups) -> Result<LevelTerm, Unparseable> {
    let mut p = Parser::new(text, groups);
    let level = p.try_parse_level()?.ok_or_else(|| Unparseable(format!("not a flight level: {text:?}")))?;
    if !p.at_end() {
        return Err(Unparseable(format!("unread text after the flight level: {:?}", &p.toks[p.pos..])));
    }
    Ok(level)
}

/// A bare point or list of points, as Annex 2A's Condition column writes `VIA
/// <point>` — the same list grammar as a clause's `VIA`, without the keyword.
pub(crate) fn parse_via_phrase(text: &str) -> Result<Vec<String>, Unparseable> {
    let groups = Groups::default();
    let mut p = Parser::new(text, &groups);
    if !p.eat_word("VIA") {
        return Err(Unparseable(format!("expected VIA, found {:?}", p.peek())));
    }
    let points = p.parse_token_list()?;
    if !p.at_end() {
        return Err(Unparseable(format!("unread text after VIA: {:?}", &p.toks[p.pos..])));
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> Groups {
        let mut g = Groups::default();
        g.insert("AJACCIO_GROUP", vec![Matcher::Exact("LFKJ".into()), Matcher::Exact("LFKO".into())]);
        g
    }

    #[test]
    fn a_simple_not_available_parses() {
        let r = parse_restriction("Not available for traffic ARR EGLL, EGKK", &groups()).unwrap();
        assert_eq!(r.keyword, Keyword::NotAvailable);
        assert_eq!(r.include.len(), 1);
        assert_eq!(r.include[0].terms, vec![Term::Arr(vec![Matcher::Exact("EGLL".into()), Matcher::Exact("EGKK".into())])]);
        assert!(r.except.is_none());
    }

    #[test]
    fn a_wildcard_and_a_level_without_a_connector() {
        let r = parse_restriction("Only available for traffic DEP LF** above FL245", &groups()).unwrap();
        assert_eq!(r.keyword, Keyword::OnlyAvailable);
        assert_eq!(r.include[0].terms, vec![Term::Dep(vec![Matcher::Prefix("LF".into())]), Term::Level(LevelTerm { cmp: LevelCmp::AtOrAbove, ft: 24_500.0, ft2: None, at: None })]);
    }

    #[test]
    fn a_bare_via_point() {
        let r = parse_restriction("Not available for traffic via ABC", &groups()).unwrap();
        assert_eq!(r.include[0].terms, vec![Term::Via(vec!["ABC".into()])]);
    }

    #[test]
    fn compulsory_for_traffic() {
        let r = parse_restriction("Compulsory for traffic DEP EDDF", &groups()).unwrap();
        assert_eq!(r.keyword, Keyword::Compulsory);
    }

    #[test]
    fn with_joins_terms_like_an_ampersand() {
        let r = parse_restriction("Only available for traffic DEP EDDF with ARR EGLL", &groups()).unwrap();
        assert_eq!(r.include[0].terms, vec![Term::Dep(vec![Matcher::Exact("EDDF".into())]), Term::Arr(vec![Matcher::Exact("EGLL".into())])]);
    }

    #[test]
    fn except_is_a_synonym_for_exc() {
        let r = parse_restriction("Not available for traffic DEP EDDF except ARR EGLL", &groups()).unwrap();
        assert!(r.except.is_some());
    }

    #[test]
    fn a_level_band_is_read_as_between() {
        let l = parse_level_phrase("RFL BTN FL195 AND FL245", &groups()).unwrap();
        assert_eq!(l, LevelTerm { cmp: LevelCmp::Between, ft: 19_500.0, ft2: Some(24_500.0), at: None });
    }

    #[test]
    fn a_level_band_written_as_a_dash_range_is_read_too() {
        let l = parse_level_phrase("BTN FL245-FL660", &groups()).unwrap();
        assert_eq!(l, LevelTerm { cmp: LevelCmp::Between, ft: 24_500.0, ft2: Some(66_000.0), at: None });
    }

    #[test]
    fn arr_dep_combined_matches_either_end() {
        let r = parse_restriction("Not available for traffic ARR/DEP EGLL", &groups()).unwrap();
        assert_eq!(r.include[0].terms, vec![Term::ArrOrDep(vec![Matcher::Exact("EGLL".into())])]);
    }

    #[test]
    fn a_packed_slash_shorthand_is_rejected_rather_than_read_as_one_code() {
        assert!(parse_restriction("Only available for traffic ARR EGHH/HI/HO/HR", &groups()).is_err());
        assert!(parse_restriction("Not available for traffic via KELLY/SOSIM/WAL", &groups()).is_err());
    }

    #[test]
    fn a_group_name_resolves_through_annex_1() {
        let r = parse_restriction("Not available for traffic ARR AJACCIO_GROUP", &groups()).unwrap();
        assert_eq!(r.include[0].terms, vec![Term::Arr(vec![Matcher::Exact("LFKJ".into()), Matcher::Exact("LFKO".into())])]);
    }

    #[test]
    fn an_exception_is_read_back_out() {
        let r = parse_restriction("Not available for traffic DEP EDDF EXC ARR EGLL", &groups()).unwrap();
        assert_eq!(r.except.unwrap()[0].terms, vec![Term::Arr(vec![Matcher::Exact("EGLL".into())])]);
    }

    #[test]
    fn a_flat_numbered_exception_list_is_an_or() {
        let r = parse_restriction("Not available for traffic DEP EDDF EXC 1. ARR EGLL 2. ARR LFPG", &groups()).unwrap();
        let except = r.except.unwrap();
        assert_eq!(except.len(), 2);
        assert_eq!(except[0].terms, vec![Term::Arr(vec![Matcher::Exact("EGLL".into())])]);
        assert_eq!(except[1].terms, vec![Term::Arr(vec![Matcher::Exact("LFPG".into())])]);
    }

    #[test]
    fn a_route_sequence_is_rejected_not_misread() {
        assert!(parse_restriction("Not available for traffic via (LAMPU DCT NOMBO Y161 RIDAR)", &groups()).is_err());
    }

    #[test]
    fn a_sequenced_and_then_is_rejected() {
        assert!(parse_restriction("Not available for traffic via KUNOD AND-THEN via TEDGO", &groups()).is_err());
    }

    #[test]
    fn a_second_level_of_enumeration_is_rejected() {
        let text = "Not available for traffic DEP EDDF EXC 1. ARR EGLL a. VIA X b. VIA Y 2. ARR LFPG";
        assert!(parse_restriction(text, &groups()).is_err());
    }

    #[test]
    fn an_unknown_word_at_the_start_is_rejected() {
        assert!(parse_restriction("Reduced separation applies", &groups()).is_err());
    }

    #[test]
    fn an_unknown_group_is_rejected_rather_than_silently_never_matching() {
        assert!(parse_restriction("Not available for traffic ARR SOME_UNKNOWN_GROUP", &groups()).is_err());
    }

    #[test]
    fn trailing_unread_text_is_rejected() {
        assert!(parse_restriction("Not available for traffic DEP EDDF and then some trailing nonsense (((", &groups()).is_err());
    }

    #[test]
    fn a_level_capping_phrase_parses_on_its_own() {
        let l = parse_level_phrase("RFL BLW FL315", &groups()).unwrap();
        assert_eq!(l, LevelTerm { cmp: LevelCmp::AtOrBelow, ft: 31_500.0, ft2: None, at: None });
    }

    #[test]
    fn a_level_at_a_point() {
        let l = parse_level_phrase("ABV FL245 AT NOMBO", &groups()).unwrap();
        assert_eq!(l.at, Some(vec!["NOMBO".to_string()]));
    }

    #[test]
    fn a_via_phrase_on_its_own() {
        assert_eq!(parse_via_phrase("VIA LFBBUSUD").unwrap(), vec!["LFBBUSUD".to_string()]);
    }
}
