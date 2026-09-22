//! Published minima, read off the FAA's own approach charts.
//!
//! Everything else in this crate works a minimum *out*: from the terrain, the obstacles
//! and the rules a procedure designer follows. That is the only thing to be done in most
//! of the world, and measured against 382 published American ILS minima it lands within
//! five feet about half the time. Half the time is not what a chart should say.
//!
//! In the United States there is no need to work it out at all. The FAA publishes every
//! instrument approach chart as a PDF, free, and the minima band on the chart is a table
//! of text — not a picture of one. So for an American approach the chart can print the
//! number the real chart prints, because it is reading the real chart.
//!
//! Three things had to be understood to read that table.
//!
//! **Where it is.** Not at a fixed place on the page: the band is the bottom-most table,
//! and how far down it starts depends on how many notes are stacked above it. It is found
//! by its row labels instead — `S-ILS 18L`, `S-LOC 6`, a bare `S-19R` on a chart with
//! only one sort of approach on it, `LNAV MDA`, or `CIRCLING`.
//!
//! **Which number is which.** A row reads `692/18  200 (200-½)`. The altitude is the
//! first figure; the height above the touchdown zone is the `200` *outside* the brackets.
//! The number inside them is a ceiling-and-visibility figure for an alternate, rounded to
//! the nearest hundred, and reading it as the height is wrong by up to fifty feet without
//! ever looking wrong.
//!
//! **Which row.** Where all four aircraft categories share a minimum the row prints one
//! pair of figures; where they differ it splits into four columns, and on a circling line
//! that is the norm rather than the exception. The leftmost pair is category A, which is
//! the single figure to report. And some charts print the same label twice — once for the
//! plain minimum and once for a lower one that needs a DME fix or a steeper climb — in
//! either order in the file, so the row printed highest on the page is the one taken.

use crate::cache::Cache;
use crate::sources::http::Http;
use crate::sources::msfs::procedures::ApproachType;
use anyhow::Result;
use std::collections::HashMap;

/// The 28-day cycle running on a date, as the FAA numbers it: the year, then which cycle
/// of that year. Cycle 2609 ran from 3 September 2026, and every other is a whole number
/// of cycles from it, thirteen to the year.
pub fn cycle(today: chrono::NaiveDate) -> String {
    let epoch = chrono::NaiveDate::from_ymd_opt(2026, 9, 3).expect("a real date");
    // Where this date falls relative to that cycle, then counted from the first cycle of
    // 2026 so the year rolls over on its own.
    let from_epoch = (today - epoch).num_days().div_euclid(28);
    let ordinal = from_epoch + 8;
    let year = 2026 + ordinal.div_euclid(13);
    let n = ordinal.rem_euclid(13) + 1;
    format!("{:02}{:02}", year % 100, n)
}

fn metafile_url(cycle: &str) -> String {
    format!("https://aeronav.faa.gov/d-tpp/{cycle}/xml_data/d-TPP_Metafile.xml")
}

fn chart_url(cycle: &str, pdf: &str) -> String {
    format!("https://aeronav.faa.gov/d-tpp/{cycle}/{pdf}")
}

/// One approach chart in the cycle: what it is called and which file it is.
#[derive(Debug, Clone)]
pub struct ChartRef {
    pub name: String,
    pub pdf: String,
}

/// Every instrument approach chart in the cycle, by ICAO code.
///
/// The index is one 16 MB file for the country, so it is fetched and read once and kept.
/// An airport outside the United States is simply not in it, which is the answer for most
/// of the world and not a failure.
fn index(http: &Http, cache: &Cache) -> &'static HashMap<String, Vec<ChartRef>> {
    static INDEX: std::sync::OnceLock<HashMap<String, Vec<ChartRef>>> = std::sync::OnceLock::new();
    INDEX.get_or_init(|| match load_index(http, cache) {
        Ok(map) => map,
        Err(err) => {
            log::warn!("FAA chart index: {err:#}");
            HashMap::new()
        }
    })
}

fn load_index(http: &Http, cache: &Cache) -> Result<HashMap<String, Vec<ChartRef>>> {
    let cycle = cycle(chrono::Utc::now().date_naive());
    let url = metafile_url(&cycle);
    let bytes = cache.get_or_fetch_bytes(&format!("charts/faa/d-TPP_{cycle}.xml"), || http.get_bytes(&url))?;
    Ok(parse_index(&String::from_utf8_lossy(&bytes)))
}

/// The airports and their approach charts, out of the index file.
pub fn parse_index(xml: &str) -> HashMap<String, Vec<ChartRef>> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out: HashMap<String, Vec<ChartRef>> = HashMap::new();
    let mut icao = String::new();
    // Which field of a record is being read, and what the record has said so far.
    let mut field = String::new();
    let (mut code, mut name, mut pdf) = (String::new(), String::new(), String::new());
    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) => {
                let tag = e.name().as_ref().to_string();
                if tag == "airport_name" {
                    icao.clear();
                    if let Some(a) = e.attributes().flatten().find(|a| a.key.as_ref() == "icao_ident") {
                        icao = a.value.trim().to_uppercase();
                    }
                } else if tag == "record" {
                    code.clear();
                    name.clear();
                    pdf.clear();
                }
                field = tag;
            }
            Ok(Event::Text(t)) => {
                let text = t.xml_content(quick_xml::XmlVersion::Implicit1_0).trim().to_string();
                match field.as_str() {
                    "chart_code" => code = text,
                    "chart_name" => name = text,
                    "pdf_name" => pdf = text,
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == "record" && code == "IAP" && !pdf.is_empty() && !icao.is_empty() {
                    out.entry(icao.clone()).or_default().push(ChartRef { name: name.clone(), pdf: pdf.clone() });
                }
                field.clear();
            }
            _ => {}
        }
    }
    out
}

/// A minimum as the chart publishes it.
#[derive(Debug, Clone)]
pub struct Published {
    /// The decision altitude or minimum descent altitude, above sea level.
    pub altitude_ft: f64,
    /// Its height above the touchdown zone, or above the aerodrome on a circling line.
    pub height_ft: f64,
    /// The row label the way the chart prints it, footnote marks and all, so that a
    /// minimum which comes with a condition attached can be seen to.
    pub label: String,
    /// Which chart it was read from.
    pub chart: String,
    /// Every aircraft category, left to right, where the row prints them separately.
    pub categories: Vec<(f64, f64)>,
    /// The circling line from the same chart, where it carries one, by category.
    pub circling: Vec<(f64, f64)>,
}

/// Which line of the band is wanted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Line {
    /// The straight-in line for a sort of approach.
    StraightIn(ApproachType),
    /// The circling line, which belongs to the chart rather than to one runway.
    Circling,
}

/// The published minimum for an approach, where the FAA publishes one.
///
/// `touchdown_ft` is what we already know the touchdown zone elevation to be, and is used
/// to check the reading rather than to make it: the altitude less its height above
/// touchdown is that elevation, so a reading that disagrees with a surveyed figure by
/// more than a few hundred feet has read the wrong part of the page and is thrown away. A
/// wrong minimum on a chart is worse than an estimated one.
pub fn published(
    http: &Http,
    cache: &Cache,
    icao: &str,
    line: Line,
    runway: &str,
    suffix: Option<char>,
    touchdown_ft: f64,
) -> Option<Published> {
    let charts = index(http, cache).get(&icao.to_uppercase())?;
    let want = match line {
        Line::StraightIn(what) => what,
        // A circling line is printed on whichever chart serves the runway, and the
        // approach types are tried in the order a chart is most likely to carry one.
        Line::Circling => ApproachType::Ils,
    };
    let chart = pick_chart(charts, want, runway, suffix).or_else(|| match line {
        Line::Circling => [ApproachType::Vor, ApproachType::Rnav, ApproachType::Ndb, ApproachType::Localiser]
            .into_iter()
            .find_map(|what| pick_chart(charts, what, runway, suffix)),
        _ => None,
    })?;
    let bytes = match fetch_chart(http, cache, &chart.pdf) {
        Ok(b) => b,
        Err(err) => {
            log::info!("{icao}: chart {}: {err:#}", chart.pdf);
            return None;
        }
    };
    let mut published = read_chart(&bytes, &chart.name, line, runway).or_else(|| {
        log::info!("{icao}: no {line:?} line on chart {}", chart.pdf);
        None
    })?;
    published.chart = chart.name.clone();
    if !sane(&published, touchdown_ft) {
        log::info!(
            "{icao}: {} on {} reads {:.0}/{:.0}, which does not agree with a touchdown zone of {touchdown_ft:.0} ft",
            published.label,
            chart.pdf,
            published.altitude_ft,
            published.height_ft
        );
        return None;
    }
    Some(published)
}

/// The wanted line of one chart's minima band, from the file itself.
///
/// Split out from `published` so that it can be run against charts whose minima have been
/// read off the printed page by hand: forty-one of them, picked to cover every sort of
/// approach and every awkward layout the band has.
pub fn read_chart(pdf: &[u8], chart_name: &str, line: Line, runway: &str) -> Option<Published> {
    let items = text_items(pdf);
    if items.is_empty() {
        // Some charts, almost all of them at military fields, are drawn as line art with
        // no text in them at all. There is nothing to read.
        return None;
    }
    // An ILS chart carries its localiser line about eight points under its ILS line, so
    // the rows there have to be told apart more finely.
    let name = chart_name.to_uppercase();
    let tight = name.contains("ILS") && name.contains("LOC");
    let read = read_line(&items, line, runway, tight)?;
    let circling = match line {
        Line::Circling => Vec::new(),
        _ => read_line(&items, Line::Circling, runway, false).map(|c| c.categories).unwrap_or_default(),
    };
    Some(Published {
        altitude_ft: read.altitude_ft,
        height_ft: read.height_ft,
        categories: read.categories,
        label: read.label,
        chart: chart_name.to_string(),
        circling,
    })
}

/// One chart's file, fetched and kept.
pub fn fetch_chart(http: &Http, cache: &Cache, pdf: &str) -> Result<Vec<u8>> {
    let cycle = cycle(chrono::Utc::now().date_naive());
    let url = chart_url(&cycle, pdf);
    cache.get_or_fetch_bytes(&format!("charts/faa/{cycle}/{pdf}"), || http.get_bytes(&url))
}

/// Every approach chart published for an airport.
pub fn charts_at(http: &Http, cache: &Cache, icao: &str) -> Vec<ChartRef> {
    index(http, cache).get(&icao.to_uppercase()).cloned().unwrap_or_default()
}

/// Whether a reading can be believed.
fn sane(p: &Published, touchdown_ft: f64) -> bool {
    if !(100.0..=4000.0).contains(&p.height_ft) || p.altitude_ft <= p.height_ft {
        return false;
    }
    // The altitude less its height above touchdown is the touchdown zone elevation, which
    // is known from elsewhere and surveyed. They have to agree.
    let implied = p.altitude_ft - p.height_ft;
    !touchdown_ft.is_finite() || (implied - touchdown_ft).abs() <= 400.0
}

/// Which chart carries this approach.
///
/// A name is what a chart is called on the page: "ILS OR LOC RWY 23", "RNAV (GPS) Y RWY
/// 14R", "VOR-A". The runway has to match and the sort of approach has to be on it — a
/// localiser minimum is printed on the combined ILS chart, which is why a name carrying
/// both counts for either. Charts for category II and III minima are a different table
/// with a different grammar, and are left alone.
fn pick_chart<'a>(charts: &'a [ChartRef], what: ApproachType, runway: &str, suffix: Option<char>) -> Option<&'a ChartRef> {
    let mut best: Option<(i32, &ChartRef)> = None;
    for chart in charts {
        let name = chart.name.to_uppercase();
        if name.contains("CAT II") || name.contains("CAT III") || name.contains("SA CAT") {
            continue;
        }
        if !name.split(|c: char| !c.is_ascii_alphanumeric() && c != '/').any(|w| w == type_token(what)) {
            continue;
        }
        if !serves_runway(&name, runway) {
            continue;
        }
        // A chart with the same letter as the approach is the right one of several; a
        // chart with no letter at all is right where the approach has none.
        let letter = chart_letter(&name);
        let score = match (suffix, letter) {
            (Some(a), Some(b)) if a == b => 3,
            (None, None) => 2,
            (Some(_), None) | (None, Some(_)) => 1,
            _ => 0,
        };
        if best.map(|(s, _)| score > s).unwrap_or(true) {
            best = Some((score, chart));
        }
    }
    best.map(|(_, c)| c)
}

/// What the sort of approach is called in a chart's name.
fn type_token(what: ApproachType) -> &'static str {
    match what {
        ApproachType::Ils => "ILS",
        ApproachType::Localiser | ApproachType::LocaliserBackCourse => "LOC",
        ApproachType::Lda => "LDA",
        ApproachType::Rnav => "RNAV",
        ApproachType::Gps => "GPS",
        ApproachType::Vor => "VOR",
        ApproachType::Ndb => "NDB",
    }
}

/// Whether a chart's name serves this runway. A runway approach ends in its number, with
/// the leading zero written or not; a lettered approach serves the aerodrome rather than
/// a runway and is named for a letter instead.
fn serves_runway(name: &str, runway: &str) -> bool {
    let want = runway.trim().trim_start_matches("RW").to_uppercase();
    let short = want.trim_start_matches('0');
    match name.rsplit_once("RWY ") {
        Some((_, tail)) => {
            let tail = tail.trim();
            tail == want || tail == short
        }
        // "VOR-A", "NDB OR GPS-B": the letter after the last hyphen.
        None => {
            let letter = name.rsplit('-').next().map(str::trim).unwrap_or("");
            letter.len() == 1 && letter == want
        }
    }
}

/// The letter that tells two approaches of the same sort apart: the Y in "ILS Y OR LOC Y
/// RWY 23". Only Y, Z and W are used this way, and only as a word of their own.
fn chart_letter(name: &str) -> Option<char> {
    name.split_whitespace()
        .find(|w| w.len() == 1 && matches!(*w, "Y" | "Z" | "W"))
        .and_then(|w| w.chars().next())
}

/// One string drawn on the page, and where it sits.
#[derive(Debug, Clone)]
pub struct Item {
    pub x: f64,
    pub y: f64,
    pub text: String,
}

/// What one row of the band says.
struct Reading {
    altitude_ft: f64,
    height_ft: f64,
    /// Every aircraft category printed on the row, left to right. One entry where the
    /// categories share a minimum, four where they do not.
    categories: Vec<(f64, f64)>,
    label: String,
}

/// The straight-in or circling line, out of the text on the page.
///
/// `tight` is for a chart that carries an ILS and a localiser line together: they sit
/// about eight points apart, so the band searched around a row has to be narrower than
/// that or the localiser's higher minimum is read as the ILS's. Everywhere else the band
/// is wider, because a row whose four categories differ is printed as two stacked lines
/// with the label between them.
fn read_line(items: &[Item], line: Line, runway: &str, tight: bool) -> Option<Reading> {
    let tolerance = if tight { 3.5 } else { 8.0 };
    let mut rows: Vec<(f64, f64, String)> = Vec::new();
    for item in items {
        let label = item.text.trim();
        if label.is_empty() {
            continue;
        }
        match matches_label(label, line, runway) {
            Label::Whole => rows.push((item.x, item.y, label.to_string())),
            Label::NeedsRunway => {
                // The label was drawn without its runway number, which is then the next
                // string along the same row.
                let next = items
                    .iter()
                    .filter(|o| (o.y - item.y).abs() <= 3.5 && o.x > item.x)
                    .min_by(|a, b| a.x.total_cmp(&b.x));
                if let Some(next) = next {
                    if runway_token(next.text.trim(), runway) {
                        rows.push((item.x, item.y, format!("{label} {}", next.text.trim())));
                    }
                }
            }
            Label::No => {}
        }
    }
    // The primary box is the one printed highest on the page; a second box with the same
    // label is a conditional minimum, whichever order the file draws them in.
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (x, y, label) in rows {
        let categories = row_values(items, x, y, tolerance);
        if let Some(&(altitude, height)) = categories.first() {
            return Some(Reading { altitude_ft: altitude, height_ft: height, categories, label });
        }
    }
    None
}

/// What a row says, column by column, left to right. The leftmost is category A.
///
/// A row that reads `640-1  618 (700-1)` is one column, shared by every category; a row
/// that reads four pairs across the page is four. They are told apart by reading left to
/// right and starting a new column at each altitude: whatever height follows belongs to
/// the altitude before it. Where the four differ the pairs are often printed as two
/// stacked lines rather than one, which is why a column's two halves are found by where
/// they sit across the page rather than by which line they are on.
fn row_values(items: &[Item], row_x: f64, row_y: f64, tolerance: f64) -> Vec<(f64, f64)> {
    let mut near: Vec<&Item> = items.iter().filter(|o| (o.y - row_y).abs() <= tolerance && o.x >= row_x - 2.0).collect();
    near.sort_by(|a, b| a.x.total_cmp(&b.x));
    let mut altitudes: Vec<(f64, f64)> = Vec::new();
    let mut heights: Vec<(f64, f64)> = Vec::new();
    for item in near {
        let text = item.text.trim();
        if let Some(altitude) = altitude_token(text) {
            altitudes.push((item.x, altitude as f64));
        } else if let Some(height) = height_token(text) {
            heights.push((item.x, height as f64));
        }
    }
    // A height belongs to the altitude it is printed under or beside, which is the
    // nearest one across the page: on a row split into two lines the height is drawn a
    // few points to the left of its own altitude, so reading order alone would hand each
    // height to the column before it.
    let mut used = vec![false; heights.len()];
    altitudes
        .into_iter()
        .filter_map(|(ax, altitude)| {
            let nearest = heights
                .iter()
                .enumerate()
                .filter(|(i, _)| !used[*i])
                .min_by(|(_, a), (_, b)| (a.0 - ax).abs().total_cmp(&(b.0 - ax).abs()))
                .map(|(i, h)| (i, h.1))?;
            used[nearest.0] = true;
            Some((altitude, nearest.1))
        })
        .collect()
}

/// Whether a string is the label of the row wanted, and whether its runway number was
/// drawn separately.
enum Label {
    Whole,
    NeedsRunway,
    No,
}

fn matches_label(text: &str, line: Line, runway: &str) -> Label {
    // Hyphens join the parts of a label as often as spaces do ("S-ILS-13", "S- ILS 16"),
    // and a trailing footnote mark is part of the label rather than of the runway.
    let flat = text.to_uppercase().replace('-', " ");
    let words: Vec<&str> = flat.split_whitespace().collect();
    if words.is_empty() {
        return Label::No;
    }
    let what = match line {
        Line::Circling => {
            let circling = words[0].trim_end_matches(|c: char| !c.is_ascii_alphanumeric()) == "CIRCLING";
            return if circling && (words.len() == 1 || words.get(1) == Some(&"MDA")) { Label::Whole } else { Label::No };
        }
        Line::StraightIn(what) => what,
    };
    if what == ApproachType::Rnav || what == ApproachType::Gps {
        // An RNAV chart serves one runway, so its rows are named for what is flown rather
        // than for the runway: "LNAV MDA". The vertically guided lines on the same chart
        // are a different minimum and not this one, and "LNAV/VNAV" is often drawn as
        // "LNAV/" with "VNAV" below it.
        let first = words[0].trim_start_matches("S ").trim();
        let lnav = first == "LNAV" || first == "LNAV MDA";
        if !lnav || text.trim().ends_with('/') || flat.contains("VNAV") || flat.contains("LPV") {
            return Label::No;
        }
        return match words.get(1) {
            None | Some(&"MDA") => Label::Whole,
            Some(w) if runway_token(w, runway) => Label::Whole,
            _ => Label::No,
        };
    }
    // Everything else is a straight-in line: an S, then the sort of approach where the
    // chart bothers to say it, then the runway.
    let mut rest = &words[..];
    if rest[0] != "S" {
        return Label::No;
    }
    rest = &rest[1..];
    if let Some(first) = rest.first() {
        let token = type_token(what);
        if *first == token || first.split('/').next() == Some(token) {
            rest = &rest[1..];
        }
    }
    match rest {
        [] => Label::NeedsRunway,
        [one] if runway_token(one, runway) => Label::Whole,
        _ => Label::No,
    }
}

/// Whether a string is this runway's number as the band prints it: the leading zero
/// dropped, and any footnote mark glued straight onto the digits.
fn runway_token(text: &str, runway: &str) -> bool {
    let want = runway.trim().trim_start_matches("RW").to_uppercase();
    let short = want.trim_start_matches('0');
    let got: String = text
        .to_uppercase()
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, 'L' | 'R' | 'C'))
        .collect();
    !got.is_empty() && (got == want || got == short)
}

/// Digits at the start of a string: their value, how many there were, and what follows.
fn leading_number(text: &str) -> Option<(u32, usize, &str)> {
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    Some((digits.parse().ok()?, digits.len(), &text[digits.len()..]))
}

/// An altitude, as a minima band writes one: "640-1", "1040/40", or on its own.
fn altitude_token(text: &str) -> Option<u32> {
    let (value, len, rest) = leading_number(text.trim())?;
    if !(3..=5).contains(&len) {
        return None;
    }
    let rest = rest.trim_start();
    if rest.starts_with('/') || rest.starts_with('-') {
        return Some(value);
    }
    // A bare number, which has to be big enough not to be a page or amendment number.
    (rest.is_empty() && value > 50).then_some(value)
}

/// A height above touchdown: the number outside the brackets in "618 (700-1)". The one
/// inside is a ceiling and visibility for an alternate, and is only taken where there is
/// no other, which a few layouts do.
fn height_token(text: &str) -> Option<u32> {
    let text = text.trim();
    if let Some((value, len, rest)) = leading_number(text) {
        if (2..=4).contains(&len) {
            let rest = rest.trim_start();
            if let Some(inside) = rest.strip_prefix('(') {
                if let Some((_, _, after)) = leading_number(inside.trim_start()) {
                    let after = after.trim_start();
                    if after.starts_with('-') || after.starts_with('/') {
                        return Some(value);
                    }
                }
            }
        }
    }
    let inside = text.strip_prefix('(')?.strip_suffix(')')?;
    let (value, len, rest) = leading_number(inside.trim())?;
    let rest = rest.trim_start();
    ((2..=4).contains(&len) && (rest.starts_with('-') || rest.starts_with('/'))).then_some(value)
}

/// Every string drawn on a chart, and where it sits on the page.
///
/// Enough of the PDF text model to know which strings share a row, which is all a table
/// needs: the text matrix from `Tm`, the line offsets from `Td`, `TD` and `T*`, and the
/// strings from `Tj` and `TJ`. The charts are one page each and every stream in them is
/// deflated, so the streams are simply all decompressed and read in turn.
pub fn text_items(pdf: &[u8]) -> Vec<Item> {
    let mut out = Vec::new();
    for stream in streams(pdf) {
        read_text(&stream, &mut out);
    }
    out
}

/// Every deflated stream in the file, decompressed. One that will not decompress is not
/// a content stream — an image, or a font — and is no loss.
fn streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(found) = find(pdf, b"stream", at) {
        // "endstream" carries the word too, and stepping into one of those would read
        // the rest of the file as a stream that never decompresses.
        if found >= 3 && &pdf[found - 3..found] == b"end" {
            at = found + b"stream".len();
            continue;
        }
        let mut start = found + b"stream".len();
        if pdf.get(start) == Some(&b'\r') {
            start += 1;
        }
        if pdf.get(start) == Some(&b'\n') {
            start += 1;
        }
        let Some(end) = find(pdf, b"endstream", start) else { break };
        at = end + b"endstream".len();
        let mut decoded = Vec::new();
        if flate2::read::ZlibDecoder::new(&pdf[start..end]).read_to_end(&mut decoded).is_ok() && !decoded.is_empty() {
            out.push(decoded);
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= haystack.len() {
        return None;
    }
    haystack[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// A token of a content stream, as far as text placement cares.
enum Token {
    Number(f64),
    Text(String),
    Open,
    Other,
}

/// The text-showing operators of one content stream.
fn read_text(stream: &[u8], out: &mut Vec<Item>) {
    // The text matrix and the line matrix it is reset to, as the six numbers of `Tm`.
    let mut tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut tlm = tm;
    let mut stack: Vec<Token> = Vec::new();
    let mut at = 0;
    while at < stream.len() {
        let c = stream[at];
        match c {
            b'(' => {
                let (text, next) = pdf_string(stream, at);
                stack.push(Token::Text(text));
                at = next;
            }
            b'<' => {
                // A hex string, or a dictionary. Neither places text, but a hex string
                // has to be stepped over as one thing.
                at = find(stream, b">", at).map(|i| i + 1).unwrap_or(stream.len());
                stack.push(Token::Other);
            }
            b'[' => {
                stack.push(Token::Open);
                at += 1;
            }
            b']' => at += 1,
            b'/' => {
                at += 1;
                while at < stream.len() && !is_break(stream[at]) {
                    at += 1;
                }
                stack.push(Token::Other);
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let start = at;
                at += 1;
                while at < stream.len() && matches!(stream[at], b'+' | b'-' | b'.' | b'0'..=b'9') {
                    at += 1;
                }
                let text = String::from_utf8_lossy(&stream[start..at]);
                stack.push(text.parse().map(Token::Number).unwrap_or(Token::Other));
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'\'' | b'"' | b'*' => {
                let start = at;
                while at < stream.len() && matches!(stream[at], b'A'..=b'Z' | b'a'..=b'z' | b'\'' | b'"' | b'*') {
                    at += 1;
                }
                let op = String::from_utf8_lossy(&stream[start..at]).to_string();
                operator(&op, &mut stack, &mut tm, &mut tlm, out);
                stack.clear();
            }
            _ => at += 1,
        }
    }
}

fn is_break(c: u8) -> bool {
    c.is_ascii_whitespace() || matches!(c, b'/' | b'[' | b']' | b'<' | b'>' | b'(' | b')')
}

fn operator(op: &str, stack: &mut Vec<Token>, tm: &mut [f64; 6], tlm: &mut [f64; 6], out: &mut Vec<Item>) {
    let numbers: Vec<f64> = stack
        .iter()
        .filter_map(|t| match t {
            Token::Number(n) => Some(*n),
            _ => None,
        })
        .collect();
    match op {
        "BT" => {
            *tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            *tlm = *tm;
        }
        "Tm" if numbers.len() >= 6 => {
            let n = &numbers[numbers.len() - 6..];
            *tm = [n[0], n[1], n[2], n[3], n[4], n[5]];
            *tlm = *tm;
        }
        "Td" | "TD" if numbers.len() >= 2 => {
            let (tx, ty) = (numbers[numbers.len() - 2], numbers[numbers.len() - 1]);
            tlm[4] += tx * tlm[0] + ty * tlm[2];
            tlm[5] += tx * tlm[1] + ty * tlm[3];
            *tm = *tlm;
        }
        "T*" => {
            // The leading is not tracked; a line is a line, and only the order of rows
            // matters once they are grouped by where they sit.
            tlm[5] -= 10.0;
            *tm = *tlm;
        }
        "Tj" | "'" | "\"" => {
            if let Some(Token::Text(text)) = stack.last() {
                push(out, tm, text);
            }
        }
        "TJ" => {
            // Everything since the bracket that opened the array.
            let from = stack.iter().rposition(|t| matches!(t, Token::Open)).map(|i| i + 1).unwrap_or(0);
            let text: String = stack[from..]
                .iter()
                .filter_map(|t| match t {
                    Token::Text(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect();
            if !text.is_empty() {
                push(out, tm, &text);
            }
        }
        _ => {}
    }
}

fn push(out: &mut Vec<Item>, tm: &[f64; 6], text: &str) {
    // A font or a character map decompresses like a content stream and is read like one
    // too, but what comes out of it is not text on the page. Control characters are the
    // tell, and nothing a chart prints has any.
    if !text.trim().is_empty() && !text.chars().any(|c| c.is_control()) {
        out.push(Item { x: tm[4], y: tm[5], text: text.to_string() });
    }
}

/// A PDF string, from its opening bracket. Returns the text and where the string ended.
fn pdf_string(stream: &[u8], at: usize) -> (String, usize) {
    let mut text = String::new();
    let mut depth = 0usize;
    let mut i = at;
    while i < stream.len() {
        match stream[i] {
            b'(' => {
                depth += 1;
                if depth > 1 {
                    text.push('(');
                }
                i += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return (text, i + 1);
                }
                text.push(')');
                i += 1;
            }
            b'\\' => {
                let next = stream.get(i + 1).copied().unwrap_or(b'\\');
                match next {
                    b'n' => text.push('\n'),
                    b'r' => text.push('\r'),
                    b't' => text.push('\t'),
                    b'b' | b'f' => text.push(' '),
                    b'0'..=b'7' => {
                        // An octal escape, of up to three digits.
                        let mut value = 0u32;
                        let mut used = 0;
                        while used < 3 {
                            match stream.get(i + 1 + used) {
                                Some(d @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(d - b'0');
                                    used += 1;
                                }
                                _ => break,
                            }
                        }
                        text.push(char::from_u32(value & 0xFF).unwrap_or(' '));
                        i += 1 + used;
                        continue;
                    }
                    other => text.push(char::from(other)),
                }
                i += 2;
            }
            // The strings on these charts are in a single-byte encoding, so a byte is a
            // character.
            other => {
                text.push(char::from(other));
                i += 1;
            }
        }
    }
    (text, stream.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_run_twenty_eight_days_and_roll_over_the_year() {
        let day = |y, m, d| chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(cycle(day(2026, 9, 3)), "2609");
        assert_eq!(cycle(day(2026, 9, 30)), "2609");
        assert_eq!(cycle(day(2026, 10, 1)), "2610");
        assert_eq!(cycle(day(2026, 1, 22)), "2601");
        assert_eq!(cycle(day(2026, 1, 21)), "2513");
        assert_eq!(cycle(day(2027, 1, 21)), "2701");
    }

    #[test]
    fn a_height_is_the_number_outside_the_brackets() {
        assert_eq!(height_token("618 (700-1)"), Some(618));
        assert_eq!(height_token("200 (200-\u{bd})"), Some(200));
        assert_eq!(height_token("(500-1)"), Some(500));
        assert_eq!(height_token("1040/40"), None);
    }

    #[test]
    fn an_altitude_is_the_figure_before_the_visibility() {
        assert_eq!(altitude_token("640-1"), Some(640));
        assert_eq!(altitude_token("1040/40"), Some(1040));
        assert_eq!(altitude_token("4640"), Some(4640));
        assert_eq!(altitude_token("12"), None);
        assert_eq!(altitude_token("618 (700-1)"), None);
    }

    #[test]
    fn labels_are_matched_however_they_are_written() {
        let ils = Line::StraightIn(ApproachType::Ils);
        assert!(matches!(matches_label("S-ILS 18L", ils, "18L"), Label::Whole));
        assert!(matches!(matches_label("S-ILS 6", ils, "06"), Label::Whole));
        assert!(matches!(matches_label("S-ILS 18#", ils, "18"), Label::Whole));
        assert!(matches!(matches_label("S- ILS", ils, "16"), Label::NeedsRunway));
        assert!(matches!(matches_label("S-ILS 35", ils, "17"), Label::No));
        // A single-type chart drops the sort of approach from the label.
        let vor = Line::StraightIn(ApproachType::Vor);
        assert!(matches!(matches_label("S-19R", vor, "19R"), Label::Whole));
        assert!(matches!(matches_label("S-VOR/DME 17", vor, "17"), Label::Whole));
        // The vertically guided lines of an RNAV chart are a different minimum.
        let rnav = Line::StraightIn(ApproachType::Rnav);
        assert!(matches!(matches_label("LNAV MDA", rnav, "14R"), Label::Whole));
        assert!(matches!(matches_label("LNAV/", rnav, "14R"), Label::No));
        assert!(matches!(matches_label("LPV DA", rnav, "14R"), Label::No));
        assert!(matches!(matches_label("CIRCLING", Line::Circling, "14R"), Label::Whole));
    }

    #[test]
    fn a_chart_is_chosen_by_its_name() {
        let charts = vec![
            ChartRef { name: "ILS Y OR LOC Y RWY 23".into(), pdf: "y.pdf".into() },
            ChartRef { name: "ILS Z OR LOC Z RWY 23".into(), pdf: "z.pdf".into() },
            ChartRef { name: "ILS RWY 23 (CAT II)".into(), pdf: "cat2.pdf".into() },
            ChartRef { name: "VOR-A".into(), pdf: "a.pdf".into() },
        ];
        assert_eq!(pick_chart(&charts, ApproachType::Ils, "23", Some('Z')).unwrap().pdf, "z.pdf");
        assert_eq!(pick_chart(&charts, ApproachType::Localiser, "23", Some('Y')).unwrap().pdf, "y.pdf");
        assert_eq!(pick_chart(&charts, ApproachType::Vor, "A", None).unwrap().pdf, "a.pdf");
        assert!(pick_chart(&charts, ApproachType::Ndb, "23", None).is_none());
    }
}
