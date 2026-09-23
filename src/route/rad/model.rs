//! What every annex shares: how a group of airports is written, how a row's validity is
//! written, and the small date and time parsing that both come down to.

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc, Weekday};
use std::collections::HashMap;

// ---------------------------------------------------------------------------------
// Airports: an exact code, or a two-letter prefix written with a trailing wildcard.
// ---------------------------------------------------------------------------------

/// One way a restriction names an aerodrome: its own code, or every code sharing a
/// prefix — the RAD writes the second as `LF**` or `LF*`, a whole country or region at
/// once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Matcher {
    Exact(String),
    Prefix(String),
}

impl Matcher {
    pub(crate) fn matches(&self, icao: &str) -> bool {
        match self {
            Matcher::Exact(code) => code == icao,
            Matcher::Prefix(prefix) => icao.starts_with(prefix.as_str()),
        }
    }

    /// A token as the RAD writes it: `EGLL`, `LF**`, `LF*`. Never a group name — the
    /// caller resolves those against `Groups` first.
    fn from_token(tok: &str) -> Matcher {
        let stripped = tok.trim_end_matches('*');
        if stripped.len() < tok.len() && !stripped.is_empty() {
            Matcher::Prefix(stripped.to_string())
        } else {
            Matcher::Exact(tok.to_string())
        }
    }
}

pub(crate) fn any_match(matchers: &[Matcher], icao: &str) -> bool {
    matchers.iter().any(|m| m.matches(icao))
}

/// Annex 1: named groups of aerodromes, so a restriction can say `AJACCIO_GROUP`
/// instead of listing four codes every time.
#[derive(Debug, Clone, Default)]
pub(crate) struct Groups {
    by_name: HashMap<String, Vec<Matcher>>,
}

impl Groups {
    pub(crate) fn insert(&mut self, name: &str, members: Vec<Matcher>) {
        self.by_name.insert(normalise_name(name), members);
    }

    fn get(&self, name: &str) -> Option<&[Matcher]> {
        self.by_name.get(&normalise_name(name)).map(Vec::as_slice)
    }

    /// A token from a restriction's traffic clause turned into the matchers it stands
    /// for: itself, wildcarded, or an Annex 1 group looked up by name. `None` when it
    /// looks like a group reference (not a plain four-letter code or wildcard) and no
    /// such group is known — a rule built on it must not be silently misread as never
    /// matching anything, so it is rejected instead.
    pub(crate) fn resolve(&self, tok: &str) -> Option<Vec<Matcher>> {
        let plain = tok.trim_end_matches('*');
        let looks_like_code = plain.len() == 4 && plain.chars().all(|c| c.is_ascii_alphanumeric());
        if looks_like_code {
            return Some(vec![Matcher::from_token(tok)]);
        }
        // A shorter prefix wildcard, "LF*" with one letter, or a two-letter area code
        // used bare as a prefix (Annex 2A's "Crossing Airspace" writes these): still a
        // wildcard, not a group.
        if tok.ends_with('*') && plain.chars().all(|c| c.is_ascii_alphabetic()) && !plain.is_empty() {
            return Some(vec![Matcher::from_token(tok)]);
        }
        self.get(tok).map(|m| m.to_vec())
    }
}

/// A cell naming one or more aerodromes — `EDDF`, `LF**`, `(LEPA,LEIB,LEMH)` — turned
/// into matchers, each token resolved through `groups`. `None` when any token fails to
/// resolve: an airspace identifier such as `LFBBCTA` where an aerodrome or group was
/// expected is exactly the kind of thing this must not silently treat as never matching.
pub(crate) fn parse_matcher_list(s: &str, groups: &Groups) -> Option<Vec<Matcher>> {
    let s = s.trim();
    let inner = s.strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or(s);
    let mut out = Vec::new();
    for tok in inner.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        out.extend(groups.resolve(tok)?);
    }
    (!out.is_empty()).then_some(out)
}

fn normalise_name(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase()
}

// ---------------------------------------------------------------------------------
// Validity: the dates and days and times of day a row applies over.
// ---------------------------------------------------------------------------------

/// One window of the week a restriction bites in: which days, and which part of each.
/// Times are read as written, UTC, which is how the RAD publishes them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TimeWindow {
    pub from_day: Weekday,
    pub to_day: Weekday,
    pub from: NaiveTime,
    pub to: NaiveTime,
}

impl TimeWindow {
    fn day_in_range(&self, day: Weekday) -> bool {
        let (from, to, day) = (self.from_day.num_days_from_monday(), self.to_day.num_days_from_monday(), day.num_days_from_monday());
        if from <= to {
            (from..=to).contains(&day)
        } else {
            day >= from || day <= to
        }
    }

    fn time_in_range(&self, t: NaiveTime) -> bool {
        if self.from <= self.to {
            self.from <= t && t <= self.to
        } else {
            // A window that runs past midnight: "22:30-06:45".
            t >= self.from || t <= self.to
        }
    }

    fn contains(&self, at: DateTime<Utc>) -> bool {
        // A window spanning midnight also needs the day either side of the boundary
        // checked at the day it started on, not just the day the clock reads.
        if self.from <= self.to {
            self.day_in_range(at.weekday()) && self.time_in_range(at.time())
        } else if self.time_in_range(at.time()) {
            let day = if at.time() <= self.to { at.weekday().pred() } else { at.weekday() };
            self.day_in_range(day)
        } else {
            false
        }
    }
}

/// When in the week a row's restriction applies. `Always` is the RAD's `H24`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TimeApplicability {
    Always,
    Windows(Vec<TimeWindow>),
}

impl TimeApplicability {
    fn active_at(&self, when: DateTime<Utc>) -> bool {
        match self {
            TimeApplicability::Always => true,
            TimeApplicability::Windows(windows) => windows.iter().any(|w| w.contains(when)),
        }
    }
}

/// When a row applies: the AIRAC dates it is published between, and the days and
/// times of day within that its restriction bites. `None` for `until` is the RAD's
/// `UFN`, until further notice.
#[derive(Debug, Clone)]
pub(crate) struct Validity {
    pub from: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
    pub time: TimeApplicability,
}

impl Validity {
    pub(crate) fn active_at(&self, when: DateTime<Utc>) -> bool {
        let date = when.date_naive();
        self.from.is_none_or(|d| d <= date) && self.until.is_none_or(|d| date <= d) && self.time.active_at(when)
    }
}

/// Whether a row, by its Change Indicator and its dates, is the one in force at `when`.
/// A `DEL` row is the rolling document's record that the restriction stops existing
/// over its own validity window — never an active row in its own right.
pub(crate) fn row_active(change_ind: &str, from: Option<NaiveDate>, until: Option<NaiveDate>, when: DateTime<Utc>) -> bool {
    if change_ind.trim().eq_ignore_ascii_case("DEL") {
        return false;
    }
    let date = when.date_naive();
    from.is_none_or(|d| d <= date) && until.is_none_or(|d| date <= d)
}

// ---------------------------------------------------------------------------------
// Dates and times as the RAD writes them.
// ---------------------------------------------------------------------------------

const MONTHS: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

/// A calendar date out of a cell that writes it either way round — `22 SEP 2026 [2609]`
/// or `2609 [22 SEP 2026]` — by finding the `D MON YYYY` inside it rather than trusting
/// the order. `UFN`, "until further notice", and an empty cell both read as `None`, open
/// ended.
pub(crate) fn parse_rad_date(s: &str) -> Option<NaiveDate> {
    let upper = s.to_uppercase();
    if upper.trim().is_empty() || upper.contains("UFN") {
        return None;
    }
    let words: Vec<&str> = upper.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    for w in words.windows(3) {
        let (Ok(day), Some(month), Ok(year)) = (w[0].parse::<u32>(), MONTHS.iter().position(|m| *m == w[1]), w[2].parse::<i32>()) else { continue };
        if let Some(d) = NaiveDate::from_ymd_opt(year, month as u32 + 1, day) {
            return Some(d);
        }
    }
    None
}

/// The three-letter weekday abbreviations the RAD writes: `MON`, `TUE`, and so on.
fn parse_weekday(w: &str) -> Option<Weekday> {
    match w.to_uppercase().as_str() {
        "MON" => Some(Weekday::Mon),
        "TUE" => Some(Weekday::Tue),
        "WED" => Some(Weekday::Wed),
        "THU" => Some(Weekday::Thu),
        "FRI" => Some(Weekday::Fri),
        "SAT" => Some(Weekday::Sat),
        "SUN" => Some(Weekday::Sun),
        _ => None,
    }
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    let (h, m) = s.trim().split_once(':')?;
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

/// One piece of a time applicability string: `HH:MM-HH:MM`, `DAY HH:MM-HH:MM`, or
/// `DAY-DAY HH:MM-HH:MM`. The parenthesised alternative time some rows carry alongside
/// (a second timezone) is dropped; the first is taken as UTC, which is how the RAD
/// publishes its primary figure.
fn parse_time_piece(piece: &str) -> Option<TimeWindow> {
    let piece = piece.split('(').next().unwrap_or(piece).trim();
    let tokens: Vec<&str> = piece.split_whitespace().collect();
    let (days, times) = match tokens.as_slice() {
        [time] => (None, *time),
        [day, time] => (Some(*day), *time),
        _ => return None,
    };
    let (from_t, to_t) = times.split_once('-')?;
    let from = parse_time(from_t)?;
    let to = parse_time(to_t)?;
    let (from_day, to_day) = match days {
        None => (Weekday::Mon, Weekday::Sun),
        Some(d) => match d.split_once('-') {
            Some((a, b)) => (parse_weekday(a)?, parse_weekday(b)?),
            None => {
                let day = parse_weekday(d)?;
                (day, day)
            }
        },
    };
    Some(TimeWindow { from_day, to_day, from, to })
}

/// A Time Applicability cell: `H24` for always, or one or more day/time windows joined
/// by `&`. Anything with a season written against an AIRAC name (`AIRAC MAR - FIRST
/// AIRAC OCT`), which this does not resolve to real dates, is rejected rather than
/// applied the whole year round.
pub(crate) fn parse_time_applicability(s: &str) -> Option<TimeApplicability> {
    let upper = s.to_uppercase();
    if upper.contains("AIRAC") || upper.contains("SEASON") {
        return None;
    }
    if upper.trim() == "H24" || upper.trim().is_empty() {
        return Some(TimeApplicability::Always);
    }
    let mut windows = Vec::new();
    for piece in upper.split('&') {
        windows.push(parse_time_piece(piece.trim())?);
    }
    if windows.is_empty() {
        None
    } else {
        Some(TimeApplicability::Windows(windows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_date_is_found_whichever_way_round_it_is_written() {
        assert_eq!(parse_rad_date("22 SEP 2026 [2609]"), NaiveDate::from_ymd_opt(2026, 9, 22));
        assert_eq!(parse_rad_date("2608 [06 AUG 2026]"), NaiveDate::from_ymd_opt(2026, 8, 6));
        assert_eq!(parse_rad_date("UFN"), None);
        assert_eq!(parse_rad_date(""), None);
    }

    #[test]
    fn a_wildcard_matches_the_prefix_and_nothing_else() {
        let m = Matcher::from_token("LF**");
        assert!(m.matches("LFPG") && !m.matches("LEPA"));
        let exact = Matcher::from_token("EGLL");
        assert!(exact.matches("EGLL") && !exact.matches("EGLC"));
    }

    #[test]
    fn a_group_resolves_by_name_and_a_code_resolves_itself() {
        let mut g = Groups::default();
        g.insert("AJACCIO_GROUP", vec![Matcher::Exact("LFKJ".into())]);
        assert_eq!(g.resolve("AJACCIO_GROUP"), Some(vec![Matcher::Exact("LFKJ".into())]));
        assert_eq!(g.resolve("EGLL"), Some(vec![Matcher::Exact("EGLL".into())]));
        assert_eq!(g.resolve("LF**"), Some(vec![Matcher::Prefix("LF".into())]));
        assert_eq!(g.resolve("UNKNOWN_GROUP"), None);
    }

    #[test]
    fn h24_is_always_and_a_seasonal_cell_is_rejected() {
        assert_eq!(parse_time_applicability("H24"), Some(TimeApplicability::Always));
        assert_eq!(parse_time_applicability("AIRAC MAR - FIRST AIRAC OCT H24"), None);
    }

    #[test]
    fn a_day_range_and_time_window_are_read() {
        let ta = parse_time_applicability("MON-THU 22:30-06:45").unwrap();
        let TimeApplicability::Windows(w) = ta else { panic!() };
        assert_eq!(w.len(), 1);
        let midweek_night = Utc.with_ymd_and_hms(2026, 9, 23, 23, 0, 0).unwrap(); // a Wednesday
        assert!(w[0].contains(midweek_night));
        let midweek_noon = Utc.with_ymd_and_hms(2026, 9, 23, 12, 0, 0).unwrap();
        assert!(!w[0].contains(midweek_noon));
        let friday_night = Utc.with_ymd_and_hms(2026, 9, 25, 23, 0, 0).unwrap();
        assert!(!w[0].contains(friday_night));
    }

    #[test]
    fn row_active_respects_change_indicator_and_dates() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day);
        let when = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        assert!(row_active("AMD", d(2026, 8, 1), None, when));
        assert!(!row_active("DEL", d(2026, 8, 1), None, when));
        assert!(!row_active("AMD", d(2026, 10, 1), None, when));
        assert!(!row_active("AMD", d(2026, 1, 1), d(2026, 8, 31), when));
    }
}
