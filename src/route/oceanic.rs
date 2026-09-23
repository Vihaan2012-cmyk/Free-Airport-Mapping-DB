//! Organised track systems: the North Atlantic tracks, published twice a day by the
//! Gander and Shanwick oceanic centres and relayed by the FAA as plain text; the Pacific
//! ones (PACOTS); and, where a free source can be found for them, the Australian ones.
//!
//! A track message names its points four different ways, sometimes all within the one
//! track:
//!
//! * `"57/20"` — degrees latitude, a slash, degrees longitude. The hemisphere of each is
//!   assumed from the track system (north, and west of Greenwich for the Atlantic and
//!   Pacific messages this module fetches) since the message does not repeat it for every
//!   point.
//! * `"5720N"` — the same two numbers run together, with the hemisphere letter for the
//!   latitude written out. The longitude's hemisphere is still assumed, as above.
//! * `"H5720"` — a point half a degree either side of the whole degree the four digits
//!   give. This letter is not documented anywhere this crate's author could find; it is
//!   inferred from context (such points sit between two whole-degree points a track passes
//!   near, exactly where a half-degree offset would put them) in the same way the X-Plane
//!   line codes this crate reads elsewhere were worked out empirically.
//! * A named fix, two to seven letters, resolved against the navigation database.
//!
//! What is fetched is cached and re-parsed rather than kept as a standing schedule: a
//! track message is reissued twice a day and a stale one is worse than none, since a route
//! planned on it would be flown against a track nobody is clearing traffic onto any more.

use super::Graph;
use crate::cache::Cache;
use crate::dispatch::{EdgeQuery, EdgeRule, LatLon, Verdict};
use crate::sources::http::Http;
use chrono::{DateTime, Datelike, Duration, Utc};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSystem {
    NorthAtlantic,
    Pacific,
    Australian,
}

impl TrackSystem {
    /// The hemisphere a bare degrees figure is assumed to be in when the message gives no
    /// letter for it — true for west, false for east.
    fn default_lon_west(self) -> bool {
        !matches!(self, TrackSystem::Australian)
    }

    /// Roughly where the system's tracks lie, for resolving a named fix that the
    /// navigation database carries more than one of round the world.
    fn near(self) -> LatLon {
        match self {
            TrackSystem::NorthAtlantic => (55.0, -30.0),
            TrackSystem::Pacific => (35.0, -170.0),
            TrackSystem::Australian => (-30.0, 155.0),
        }
    }
}

/// One track: its points, the levels it may be flown at, which way, and when. `points` is
/// stored in the order the track is actually flown: west to east for an eastbound track
/// even though the message this was parsed from may list it the other way round, so that
/// `add_tracks` and `TrackRule` never need to know which way is which.
#[derive(Debug, Clone)]
pub struct Track {
    pub system: TrackSystem,
    /// As filed: "NATA", "PACOTS 3", or the single letter the message gives it ("A").
    pub ident: String,
    pub points: Vec<(String, LatLon)>,
    pub levels_ft: Vec<f64>,
    pub eastbound: bool,
    pub valid_from: DateTime<Utc>,
    pub valid_to: DateTime<Utc>,
    /// The Track Message Identifier, where the message carries one: a serial number that
    /// increments with every reissue, for a flight plan to print alongside the track.
    pub tmi: Option<u32>,
}

// ---------------------------------------------------------------------------------
// Coordinates.
// ---------------------------------------------------------------------------------

/// Exactly two ASCII digits, so that "5" is never read as fifty.
fn deg2(s: &str) -> Option<u32> {
    if s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

/// A point given in one of the three numeric shorthands a track message writes a
/// coordinate in. `None` for anything else, including a named fix — see `resolve_point`.
fn parse_coord(token: &str, default_lon_west: bool) -> Option<LatLon> {
    let lon_sign = if default_lon_west { -1.0 } else { 1.0 };
    if let Some((a, b)) = token.split_once('/') {
        let lat = deg2(a)? as f64;
        let lon = deg2(b)? as f64;
        return Some((lat, lon * lon_sign));
    }
    if let Some(rest) = token.strip_prefix('H') {
        let (a, b) = rest.split_at_checked(2)?;
        let lat = deg2(a)? as f64 + 0.5;
        let lon = deg2(b)? as f64 + 0.5;
        return Some((lat, lon * lon_sign));
    }
    let bytes = token.as_bytes();
    if bytes.len() == 5 && bytes[..4].iter().all(u8::is_ascii_digit) {
        let hemisphere = bytes[4];
        if hemisphere == b'N' || hemisphere == b'S' {
            let lat = deg2(&token[0..2])? as f64 * if hemisphere == b'S' { -1.0 } else { 1.0 };
            let lon = deg2(&token[2..4])? as f64 * lon_sign;
            return Some((lat, lon));
        }
    }
    None
}

/// A stable, unique-enough identifier for a coordinate point, for the network to key it by:
/// tenths of a degree, so a half-degree point does not collide with its whole-degree
/// neighbour.
fn coord_ident(pos: LatLon) -> String {
    let lat10 = (pos.0 * 10.0).round() as i64;
    let lon10 = (pos.1 * 10.0).round() as i64;
    format!("{}{:03}{}{:04}", if lat10 >= 0 { "N" } else { "S" }, lat10.abs(), if lon10 >= 0 { "E" } else { "W" }, lon10.abs())
}

/// A point of a track, coordinate or named fix, with the resolver a caller supplies for the
/// named ones — the navigation database when this is fetched live, a handful of fixtures
/// in a test.
fn resolve_point(token: &str, default_lon_west: bool, resolve: &dyn Fn(&str) -> Option<LatLon>) -> Option<(String, LatLon)> {
    if let Some(pos) = parse_coord(token, default_lon_west) {
        return Some((coord_ident(pos), pos));
    }
    let upper = token.to_uppercase();
    if (2..=7).contains(&upper.len()) && upper.chars().all(|c| c.is_ascii_alphabetic()) {
        if let Some(pos) = resolve(&upper) {
            return Some((upper, pos));
        }
    }
    None
}

// ---------------------------------------------------------------------------------
// The message itself.
// ---------------------------------------------------------------------------------

const IGNORED_PREFIXES: &[&str] =
    &["NAT TRACK", "PACOTS", "AUSOTS", "PART ", "END OF PART", "EUR RTS", "NAT TRACKS FOR", "TMI", "STANDARD ROUTE", "REMARKS", "(NAT"];

fn levels_of(rest: &str) -> Vec<f64> {
    let rest = rest.trim().trim_start_matches(':').trim();
    if rest.is_empty() || rest == "NIL" {
        return Vec::new();
    }
    rest.split_whitespace().filter_map(|t| t.parse::<f64>().ok()).map(|fl| fl * 100.0).collect()
}

/// "231130Z" as the day, hour and minute it gives.
fn parse_dhm(tok: &str) -> Option<(u32, u32, u32)> {
    let tok = tok.strip_suffix('Z')?;
    if tok.len() != 6 || !tok.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((tok[0..2].parse().ok()?, tok[2..4].parse().ok()?, tok[4..6].parse().ok()?))
}

fn month_num(tok: &str) -> Option<u32> {
    const NAMES: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    NAMES.iter().position(|n| *n == tok).map(|i| i as u32 + 1)
}

fn ymd_hm(year: i32, month: u32, day: u32, hour: u32, min: u32) -> Option<DateTime<Utc>> {
    chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, min, 0).map(|d| d.and_utc())
}

fn looks_like_timestamp_line(upper: &str) -> bool {
    upper.split_whitespace().next().is_some_and(|first| first.len() == 7 && first.ends_with('Z') && first[..6].bytes().all(|b| b.is_ascii_digit()))
}

fn is_ignored_line(upper: &str) -> bool {
    IGNORED_PREFIXES.iter().any(|p| upper.starts_with(p)) || looks_like_timestamp_line(upper)
}

/// The window a message says its tracks are valid for: a line carrying two "DDHHMMZ" times
/// either side of the word "TO", and, following the second, the month. The year is taken
/// from `published` since the message never repeats it; a window whose end reads earlier
/// than its start (crossing midnight into the next day) is pushed a day later.
fn validity_window(text: &str, published: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    for line in text.lines() {
        let upper = line.trim().to_uppercase();
        let tokens: Vec<&str> = upper.split_whitespace().collect();
        let Some(to_idx) = tokens.iter().position(|&t| t == "TO") else { continue };
        if to_idx == 0 {
            continue;
        }
        let (Some(from), Some(to)) = (parse_dhm(tokens[to_idx - 1]), tokens.get(to_idx + 1).and_then(|t| parse_dhm(t))) else { continue };
        let month = tokens.get(to_idx + 2).and_then(|t| month_num(t)).unwrap_or_else(|| published.month());
        let (Some(start), Some(mut end)) = (ymd_hm(published.year(), month, from.0, from.1, from.2), ymd_hm(published.year(), month, to.0, to.1, to.2)) else { continue };
        if end < start {
            end += Duration::days(1);
        }
        return Some((start, end));
    }
    None
}

fn tmi_of(text: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let upper = line.trim().to_uppercase();
        let rest = upper.strip_prefix("TMI")?;
        rest.trim().trim_start_matches(':').trim().split_whitespace().next()?.parse().ok()
    })
}

/// The first token of a line, if the line looks like the start of a track: a short
/// alphabetic identifier followed by at least two points.
fn is_track_start(line: &str) -> Option<(&str, Vec<&str>)> {
    let mut it = line.split_whitespace();
    let first = it.next()?;
    if first.is_empty() || first.len() > 7 || !first.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    if first.chars().next()?.is_ascii_digit() {
        return None; // An identifier never starts with a digit; a bare coordinate does.
    }
    let rest: Vec<&str> = it.collect();
    (rest.len() >= 2).then_some((first, rest))
}

#[allow(clippy::too_many_arguments)]
fn push_track(
    tracks: &mut Vec<Track>,
    ident: Option<String>,
    points: Vec<(String, LatLon)>,
    east: &[f64],
    west: &[f64],
    system: TrackSystem,
    valid_from: DateTime<Utc>,
    valid_to: DateTime<Utc>,
    tmi: Option<u32>,
) {
    let Some(ident) = ident else { return };
    if points.len() < 2 {
        return;
    }
    let (eastbound, levels_ft) = if !east.is_empty() {
        (true, east.to_vec())
    } else if !west.is_empty() {
        (false, west.to_vec())
    } else {
        return; // Neither direction published: not in force.
    };
    let points = if eastbound { points.into_iter().rev().collect() } else { points };
    tracks.push(Track { system, ident, points, levels_ft, eastbound, valid_from, valid_to, tmi });
}

/// Every track a message describes, with named fixes resolved through `resolve`.
pub fn parse_track_message(text: &str, system: TrackSystem, published: DateTime<Utc>, resolve: &dyn Fn(&str) -> Option<LatLon>) -> anyhow::Result<Vec<Track>> {
    let default_lon_west = system.default_lon_west();
    let (valid_from, valid_to) = validity_window(text, published).unwrap_or((published, published + Duration::hours(23)));
    let tmi = tmi_of(text);

    let mut tracks = Vec::new();
    let mut current_ident: Option<String> = None;
    let mut current_points: Vec<(String, LatLon)> = Vec::new();
    let mut east_lvls: Vec<f64> = Vec::new();
    let mut west_lvls: Vec<f64> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let upper = line.to_uppercase();
        if let Some(rest) = upper.strip_prefix("EAST LVLS") {
            east_lvls = levels_of(rest);
            continue;
        }
        if let Some(rest) = upper.strip_prefix("WEST LVLS") {
            west_lvls = levels_of(rest);
            continue;
        }
        if is_ignored_line(&upper) {
            continue;
        }
        if let Some((ident, tokens)) = is_track_start(line) {
            push_track(&mut tracks, current_ident.take(), std::mem::take(&mut current_points), &east_lvls, &west_lvls, system, valid_from, valid_to, tmi);
            east_lvls.clear();
            west_lvls.clear();
            current_ident = Some(ident.to_uppercase());
            for t in tokens {
                match resolve_point(t, default_lon_west, resolve) {
                    Some(p) => current_points.push(p),
                    None => log::warn!("oceanic track {ident}: could not place point {t:?}"),
                }
            }
        }
    }
    push_track(&mut tracks, current_ident.take(), current_points, &east_lvls, &west_lvls, system, valid_from, valid_to, tmi);

    if tracks.is_empty() {
        anyhow::bail!("no tracks found in the message");
    }
    Ok(tracks)
}

// ---------------------------------------------------------------------------------
// Fetching.
// ---------------------------------------------------------------------------------

/// The FAA's plain-text relay of the current Gander/Shanwick NAT track message.
const NAT_URL: &str = "https://www.notams.faa.gov/common/nat.html";

/// The same relay, for the Pacific Organized Track System.
const PACOTS_URL: &str = "https://www.notams.faa.gov/common/pacots.html";

/// The page is served as HTML with the message inside a `<pre>` block; anything else is
/// tried as plain text in case the page changes shape.
fn extract_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    if let (Some(start), Some(end)) = (lower.find("<pre"), lower.find("</pre")) {
        if let Some(after) = html[start..].find('>') {
            let from = start + after + 1;
            if from < end {
                return unescape(&html[from..end]);
            }
        }
    }
    unescape(html)
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").replace("&#39;", "'").replace("&quot;", "\"")
}

fn fetch_system(http: &Http, cache: &Cache, url: &str, key: &str, system: TrackSystem, when: DateTime<Utc>) -> anyhow::Result<Vec<Track>> {
    let raw = cache.get_or_fetch_text(key, || http.get_text(url))?;
    let text = extract_text(&raw);
    let near = system.near();
    let resolve = |ident: &str| crate::sources::navdata::fixes_near(std::slice::from_ref(&ident.to_string()), near).get(ident).copied();
    parse_track_message(&text, system, when, &resolve)
}

/// The tracks in force at a time, fetched and cached: the North Atlantic and Pacific
/// systems from the FAA's public relays, and the Australian one where this crate's author
/// could confirm a free source for it (at the time of writing, none was found, so this
/// returns whatever the other two systems produced — see the report this module's author
/// filed alongside it).
pub fn fetch_tracks(when: DateTime<Utc>) -> anyhow::Result<Vec<Track>> {
    let http = Http::new(20, 500);
    let mut cache = Cache::for_index(false);
    cache.max_age = Some(std::time::Duration::from_secs(3 * 3600));

    let mut out = Vec::new();
    match fetch_system(&http, &cache, NAT_URL, "oceanic/nat.html", TrackSystem::NorthAtlantic, when) {
        Ok(v) => out.extend(v),
        Err(e) => log::warn!("NAT tracks: {e:#}"),
    }
    match fetch_system(&http, &cache, PACOTS_URL, "oceanic/pacots.html", TrackSystem::Pacific, when) {
        Ok(v) => out.extend(v),
        Err(e) => log::warn!("PACOTS: {e:#}"),
    }
    if out.is_empty() {
        anyhow::bail!("no oceanic track message could be fetched");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------
// Into the network, and the rule.
// ---------------------------------------------------------------------------------

/// Put each track into the network as a one-way airway named by its ident, its floor and
/// ceiling the lowest and highest of its published levels — the coarse filter `Graph`
/// itself can apply. Which of those levels is actually published, the window it is valid
/// in, and the direction, are `TrackRule`'s job; `Graph` only knows a range.
pub fn add_tracks(graph: &mut Graph, tracks: &[Track]) {
    for t in tracks {
        if t.points.len() < 2 || t.levels_ft.is_empty() {
            continue;
        }
        let min_ft = t.levels_ft.iter().cloned().fold(f64::MAX, f64::min);
        let max_ft = t.levels_ft.iter().cloned().fold(f64::MIN, f64::max);
        for w in t.points.windows(2) {
            graph.add(&t.ident, (w[0].0.as_str(), w[0].1), (w[1].0.as_str(), w[1].1), true, Some(min_ft), Some(max_ft));
        }
    }
}

struct Indexed {
    track: Track,
    /// Where each point's ident falls in the flown order, for an O(1) check that a
    /// segment goes the one way the track may be flown.
    position: HashMap<String, usize>,
}

/// A track may only be flown at its levels, inside its time, the one way it runs. Built
/// once from the tracks in force; every check after that is a couple of hash lookups.
pub struct TrackRule {
    by_ident: HashMap<String, Indexed>,
}

impl TrackRule {
    pub fn new(tracks: Vec<Track>) -> TrackRule {
        let by_ident = tracks
            .into_iter()
            .map(|t| {
                let position = t.points.iter().enumerate().map(|(i, (id, _))| (id.clone(), i)).collect();
                (t.ident.clone(), Indexed { track: t, position })
            })
            .collect();
        TrackRule { by_ident }
    }
}

impl EdgeRule for TrackRule {
    fn name(&self) -> &str {
        "oceanic tracks"
    }

    fn check(&self, q: &EdgeQuery) -> Verdict {
        let Some(it) = self.by_ident.get(q.airway) else { return Verdict::Allow };
        if q.when < it.track.valid_from || q.when > it.track.valid_to {
            return Verdict::Forbid(format!("{}: outside the window it is published for", q.airway));
        }
        if !it.track.levels_ft.iter().any(|l| (l - q.level_ft).abs() < 1.0) {
            return Verdict::Forbid(format!("{}: FL{:.0} is not one of its published levels", q.airway, q.level_ft / 100.0));
        }
        match (it.position.get(q.from), it.position.get(q.to)) {
            (Some(&i), Some(&j)) if j == i + 1 => Verdict::Allow,
            _ => Verdict::Forbid(format!("{}: not flown the way it runs", q.airway)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;

    const NAT_FIXTURE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/airspace/nat_track.txt"));

    fn fixture_resolver() -> Map<String, LatLon> {
        Map::from([("ENTRY".to_string(), (50.0, -15.0)), ("EXIT".to_string(), (58.0, -65.0)), ("START".to_string(), (61.0, -25.0)), ("FINISH".to_string(), (63.0, -75.0))])
    }

    fn published() -> DateTime<Utc> {
        Utc::now().with_year(2026).unwrap().with_month(9).unwrap().with_day(23).unwrap()
    }

    #[test]
    fn every_coordinate_form_is_read() {
        assert_eq!(parse_coord("57/20", true), Some((57.0, -20.0)));
        assert_eq!(parse_coord("5720N", true), Some((57.0, -20.0)));
        assert_eq!(parse_coord("H5720", true), Some((57.5, -20.5)));
        assert_eq!(parse_coord("5720S", false), Some((-57.0, 20.0)));
        assert_eq!(parse_coord("NOTAC", true), None, "a named fix is not a coordinate");
    }

    #[test]
    fn the_message_yields_two_tracks_with_all_four_forms_resolved() {
        let fixtures = fixture_resolver();
        let resolve = |ident: &str| fixtures.get(ident).copied();
        let tracks = parse_track_message(NAT_FIXTURE, TrackSystem::NorthAtlantic, published(), &resolve).expect("a parsed message");
        assert_eq!(tracks.len(), 2, "{tracks:#?}");

        let a = tracks.iter().find(|t| t.ident == "A").expect("track A");
        assert!(!a.eastbound);
        assert_eq!(a.levels_ft, vec![33000.0, 35000.0, 37000.0]);
        assert_eq!(a.points.len(), 5);
        assert_eq!(a.points[0].0, "ENTRY");
        assert_eq!(a.points.last().unwrap().0, "EXIT");
        // The half-degree point sits between the whole-degree ones either side of it.
        let half = a.points.iter().map(|(_, p)| *p).find(|p| p.0.fract().abs() > 0.01);
        assert!(half.is_some_and(|p| p.0 == 55.5 && p.1 == -30.5), "{half:?}");
        assert_eq!(a.tmi, Some(231));
        assert_eq!(a.valid_from, ymd_hm(2026, 9, 23, 11, 30).unwrap());
        assert_eq!(a.valid_to, ymd_hm(2026, 9, 23, 23, 0).unwrap());

        let b = tracks.iter().find(|t| t.ident == "B").expect("track B");
        assert!(b.eastbound);
        // Eastbound tracks are stored as flown: the message's own last point first.
        assert_eq!(b.points[0].0, "FINISH");
        assert_eq!(b.points.last().unwrap().0, "START");
    }

    #[test]
    fn add_tracks_makes_one_way_edges_in_the_flown_direction() {
        let fixtures = fixture_resolver();
        let resolve = |ident: &str| fixtures.get(ident).copied();
        let tracks = parse_track_message(NAT_FIXTURE, TrackSystem::NorthAtlantic, published(), &resolve).unwrap();
        let mut g = Graph::default();
        add_tracks(&mut g, &tracks);
        let a = tracks.iter().find(|t| t.ident == "A").unwrap();
        let flat = g.compact();
        let node = |id: &str| g.fixes().iter().position(|f| f.id == id).map(|i| i as u32);
        let entry = node("ENTRY").expect("ENTRY in the graph");
        let airways: Vec<&str> = flat.out(entry).iter().map(|e| g.airway_name(e.airway_id())).collect();
        assert!(airways.contains(&a.ident.as_str()));
        // ENTRY is the start of the westbound track, so nothing flies back into it on A.
        let exit = node("EXIT").unwrap();
        assert!(flat.out(exit).iter().all(|e| g.airway_name(e.airway_id()) != a.ident), "EXIT has no way out on a one-way track");
    }

    #[test]
    fn the_rule_allows_only_the_right_level_window_and_direction() {
        let fixtures = fixture_resolver();
        let resolve = |ident: &str| fixtures.get(ident).copied();
        let tracks = parse_track_message(NAT_FIXTURE, TrackSystem::NorthAtlantic, published(), &resolve).unwrap();
        let a = tracks.iter().find(|t| t.ident == "A").unwrap().clone();
        let (p0, p1) = (a.points[0].clone(), a.points[1].clone());
        let rule = TrackRule::new(tracks);

        let ok = EdgeQuery { from: &p0.0, from_pos: p0.1, to: &p1.0, to_pos: p1.1, airway: "A", level_ft: 35000.0, when: a.valid_from + Duration::hours(1), origin: "X", destination: "Y" };
        assert_eq!(rule.check(&ok), Verdict::Allow);

        let wrong_level = EdgeQuery { level_ft: 34000.0, ..ok };
        assert!(matches!(rule.check(&wrong_level), Verdict::Forbid(_)));

        let outside_window = EdgeQuery { when: a.valid_to + Duration::hours(1), ..ok };
        assert!(matches!(rule.check(&outside_window), Verdict::Forbid(_)));

        let backwards = EdgeQuery { from: &p1.0, from_pos: p1.1, to: &p0.0, to_pos: p0.1, ..ok };
        assert!(matches!(rule.check(&backwards), Verdict::Forbid(_)));

        // A different airway is none of this rule's business.
        let other_airway = EdgeQuery { airway: "UN873", level_ft: 12000.0, ..ok };
        assert_eq!(rule.check(&other_airway), Verdict::Allow);
    }

    #[test]
    fn a_message_with_no_recognisable_track_is_an_error() {
        let resolve = |_: &str| None;
        assert!(parse_track_message("NAT TRACK MESSAGE\n231200Z SEP\n", TrackSystem::NorthAtlantic, published(), &resolve).is_err());
    }

    /// Not a correctness test: prints how long a million calls to `check` take, run by
    /// hand with `cargo test --release -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn check_is_fast() {
        let fixtures = fixture_resolver();
        let resolve = |ident: &str| fixtures.get(ident).copied();
        let tracks = parse_track_message(NAT_FIXTURE, TrackSystem::NorthAtlantic, published(), &resolve).unwrap();
        let a = tracks.iter().find(|t| t.ident == "A").unwrap().clone();
        let (p0, p1) = (a.points[0].clone(), a.points[1].clone());
        let rule = TrackRule::new(tracks);
        let q = EdgeQuery { from: &p0.0, from_pos: p0.1, to: &p1.0, to_pos: p1.1, airway: "A", level_ft: 35000.0, when: a.valid_from + Duration::hours(1), origin: "X", destination: "Y" };
        let start = std::time::Instant::now();
        let n = 1_000_000;
        for _ in 0..n {
            std::hint::black_box(rule.check(&q));
        }
        let per = start.elapsed() / n;
        println!("TrackRule::check: {per:?} per call over {n} calls");
    }

    /// Run by hand: the live FAA relays, which need a network this environment may not have.
    #[test]
    #[ignore]
    fn live_tracks_can_be_fetched() {
        let tracks = fetch_tracks(Utc::now()).expect("a live track message");
        println!("{} tracks fetched", tracks.len());
        for t in tracks.iter().take(3) {
            println!("{}: {} points, {:?} eastbound={}", t.ident, t.points.len(), t.levels_ft, t.eastbound);
        }
    }
}
