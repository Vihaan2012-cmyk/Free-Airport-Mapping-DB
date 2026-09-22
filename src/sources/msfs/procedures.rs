//! Departures, arrivals and approaches for one airport, read from the simulator's own
//! navigation data.
//!
//! Layout, worked out from the files themselves and checked across 7,631 airports:
//!
//! ```text
//! airport 0x56                 ICAO at +0x28, position at +0x0C
//!   name    0x19               "TT:AIRPORTEK.KDFW.name"
//!   SID     0x42  name at +0x0C, children at +0x14
//!   STAR    0x48  "
//!   approach 0xFA              altitudes and course in the header, children at +0x24
//!     runway transition 0x46   children at +0x14
//!     enroute transition 0x4A  name at +0x08, children at +0x10
//!     approach transition 0x49 name at +0x14, children at +0x1C
//!       legs 0xF9 / 0xF4 (final) / 0xF5 (missed) / 0xF6
//!         count at +0x06, then fixed 72-byte legs
//! ```
//!
//! A leg is 72 bytes:
//!
//! ```text
//! +0x00 path type (the ARINC path terminator)   +0x18 distance from that navaid, metres
//! +0x01 how to read the altitudes               +0x1C the leg's own course, degrees
//! +0x02 turn direction, 1 left and 2 right      +0x20 the leg's own length, metres
//! +0x04 the fix it ends at                      +0x24 altitude, metres
//! +0x08 that fix's region                       +0x28 second altitude, metres
//! +0x0C the navaid it is measured from          +0x2C -1 where there is no constraint
//! +0x10 that navaid's region                    +0x30 always 357.9; not a height
//! +0x14 radial from that navaid, degrees
//! ```
//!
//! The navaid, radial and distance are how a chart writes a fix: "12 DME FUN" is a fix
//! twelve miles out on a radial from the Funchal beacon, and an arc leg is flown at that
//! distance around it. The turn direction was read off a published chart to settle which
//! value means which way: Madeira's hold at FUSUL is a left-hand pattern and carries a 1.
//!
//! The path numbering is the one the simulators have used since FSX; every transition
//! read started with an initial fix, which is what that numbering predicts, so it is
//! taken as confirmed.

use super::bgl::{self, Record};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const REC_SID: u16 = 0x42;
const REC_STAR: u16 = 0x48;
const REC_APPROACH: u16 = 0xFA;
/// Transition records, with the offset their children start at.
const TRANSITIONS: [(u16, usize); 3] = [(0x46, 0x14), (0x4A, 0x10), (0x49, 0x1C)];
/// Leg lists. The two under an approach are its final legs and its missed approach.
const LEGS_PLAIN: u16 = 0xF9;
const LEGS_FINAL: u16 = 0xF4;
const LEGS_MISSED: u16 = 0xF5;
const LEGS_TRANSITION: u16 = 0xF6;
const LEG_SIZE: usize = 72;

/// ARINC 424 path terminators, in the simulators' numbering.
const PATHS: [&str; 24] = ["", "AF", "CA", "CD", "CF", "CI", "CR", "DF", "FA", "FC", "FD", "FM", "HA", "HF", "HM", "IF", "PI", "RF", "TF", "VA", "VD", "VI", "VM", "VR"];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Sid,
    Star,
    Approach,
}

/// How a leg's altitudes are to be read.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AltitudeRule {
    /// No constraint.
    None,
    At,
    AtOrAbove,
    AtOrBelow,
    Between,
}

impl AltitudeRule {
    fn from(b: u8) -> AltitudeRule {
        match b {
            1 => AltitudeRule::At,
            2 => AltitudeRule::AtOrAbove,
            3 => AltitudeRule::AtOrBelow,
            4 => AltitudeRule::Between,
            _ => AltitudeRule::None,
        }
    }
}

fn feet(metres: f32) -> Option<f64> {
    (metres.is_finite() && metres > -1000.0 && metres != -1.0 && metres != 0.0).then(|| (metres as f64 / 0.3048).round())
}

fn degrees(v: f32) -> Option<f64> {
    (v.is_finite() && v > 0.0 && v <= 360.0).then(|| (v as f64 * 10.0).round() / 10.0)
}

fn metres(v: f32) -> Option<f64> {
    (v.is_finite() && v > 0.0 && v < 1.0e7).then(|| (v as f64).round())
}

/// A leg with nothing filled in, for tests and for callers that build one by hand.
impl Default for Leg {
    fn default() -> Self {
        Leg {
            path: String::new(),
            fix: String::new(),
            altitude_rule: AltitudeRule::None,
            altitude_ft: None,
            altitude2_ft: None,
            course_deg: None,
            distance_m: None,
            navaid: String::new(),
            theta_deg: None,
            rho_nm: None,
            turn: None,
            placed_on_radial: false,
            lat: None,
            lon: None,
        }
    }
}

/// Which way a turn is flown, where the leg says.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Turn {
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Leg {
    /// ARINC path terminator: IF, TF, CF, DF, HM and so on.
    pub path: String,
    /// The fix this leg ends at, where the path type has one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix: String,
    pub altitude_rule: AltitudeRule,
    /// Altitude constraints in feet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub altitude_ft: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub altitude2_ft: Option<f64>,
    /// Course in degrees, for the path types that fly one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub course_deg: Option<f64>,
    /// Leg length in metres, for the path types that have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
    /// The navaid this leg is measured from, where it names one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub navaid: String,
    /// Radial from that navaid, degrees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theta_deg: Option<f64>,
    /// Distance from that navaid in miles: what a chart prints as "12 DME FUN", and the
    /// radius an arc leg is flown at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rho_nm: Option<f64>,
    /// Which way the turn goes, on the legs that turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<Turn>,
    /// True when the position was worked out from a radial and a distance rather than
    /// read from the waypoint table.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub placed_on_radial: bool,
    /// Where the fix is, when the waypoint records name it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    /// Runway or fix the transition is named for; empty for a procedure's common part.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// "final" and "missed" mark the two halves of an approach.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub part: String,
    pub legs: Vec<Leg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Procedure {
    pub kind: Kind,
    pub name: String,
    /// Which of several approaches to the same runway this is, counting from 1. The
    /// file does record an approach type, but its coding is not understood well enough
    /// to print "ILS" or "RNAV" without the risk of printing the wrong one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<usize>,
    /// Runway the procedure serves, where it names one (approaches always do).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub runway: String,
    pub transitions: Vec<Transition>,
}

/// A frequency an airport is worked on: what it is for, the frequency itself, and the
/// name it is called by.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frequency {
    pub kind: String,
    pub mhz: f64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// What each of the coded kinds is. The numbering runs 1 to 15 across every airport in
/// the data; 4 never appears, so it is left unnamed rather than guessed at.
fn com_kind(code: u16) -> &'static str {
    match code {
        1 => "ATIS",
        2 => "MULTICOM",
        3 => "UNICOM",
        5 => "GROUND",
        6 => "TOWER",
        7 => "DELIVERY",
        8 => "APPROACH",
        9 => "DEPARTURE",
        10 => "CENTRE",
        11 => "FSS",
        12 => "AWOS",
        13 => "ASOS",
        14 => "PRE-TAXI",
        15 => "REMOTE DELIVERY",
        _ => "RADIO",
    }
}

/// The frequencies an airport is worked on, in the order the file lists them.
fn frequencies(d: &[u8], rec: &Record) -> Vec<Frequency> {
    let mut out = Vec::new();
    for child in bgl::records(d, rec.start + 0x44, rec.end) {
        if child.id != bgl::REC_COM || child.end - child.start < 14 {
            continue;
        }
        let code = u16::from_le_bytes([d[child.start + 0x06], d[child.start + 0x07]]);
        let hz = bgl::u32le(d, child.start + 0x08);
        // The VHF air band, with a little room either side for the military fields that
        // carry frequencies above it.
        if !(100_000_000..=400_000_000).contains(&hz) {
            continue;
        }
        let raw = &d[child.start + 0x0C..child.end];
        let name: String = raw.iter().take_while(|b| **b != 0).map(|b| *b as char).filter(|c| c.is_ascii_graphic() || *c == ' ').collect();
        out.push(Frequency { kind: com_kind(code).to_string(), mhz: (hz as f64 / 1000.0).round() / 1000.0, name: name.trim().to_string() });
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AirportProcedures {
    pub icao: String,
    pub lat: f64,
    pub lon: f64,
    pub procedures: Vec<Procedure>,
    /// The frequencies the airport is worked on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frequencies: Vec<Frequency>,
    /// The file this came from, for reporting a decoding problem.
    pub source: String,
}

impl AirportProcedures {
    /// The frequencies a chart prints, one of each kind, in the order a crew uses them.
    pub fn briefing_frequencies(&self) -> Vec<&Frequency> {
        let mut out = Vec::new();
        for want in ["ATIS", "APPROACH", "TOWER", "GROUND", "DELIVERY"] {
            if let Some(f) = self.frequencies.iter().find(|f| f.kind == want) {
                out.push(f);
            }
        }
        out
    }
}

fn legs(d: &[u8], rec: &Record, fixes: &Fixes) -> Vec<Leg> {
    let count = u32::from(u16::from_le_bytes([d[rec.start + 6], d[rec.start + 7]])) as usize;
    let mut out = Vec::new();
    for i in 0..count {
        let b = rec.start + 8 + i * LEG_SIZE;
        if b + LEG_SIZE > rec.end {
            break;
        }
        let path = *PATHS.get(d[b] as usize).unwrap_or(&"");
        let fix = bgl::ident(bgl::u32le(d, b + 4));
        let (lat, lon) = fixes.get(&fix).copied().map(|(a, o)| (Some(a), Some(o))).unwrap_or((None, None));
        let theta_deg = degrees(bgl::f32le(d, b + 0x14));
        let rho_nm = metres(bgl::f32le(d, b + 0x18)).map(|m| (m / 1852.0 * 10.0).round() / 10.0);
        out.push(Leg {
            path: path.to_string(),
            fix,
            altitude_rule: AltitudeRule::from(d[b + 1]),
            altitude_ft: feet(bgl::f32le(d, b + 0x24)),
            altitude2_ft: feet(bgl::f32le(d, b + 0x28)),
            // The leg's own course where it has one, else the radial it is flown along.
            course_deg: degrees(bgl::f32le(d, b + 0x1C)).or(theta_deg),
            distance_m: metres(bgl::f32le(d, b + 0x20)),
            navaid: bgl::ident(bgl::u32le(d, b + 0x0C)),
            theta_deg,
            rho_nm,
            turn: match d[b + 2] {
                1 => Some(Turn::Left),
                2 => Some(Turn::Right),
                _ => None,
            },
            placed_on_radial: false,
            lat,
            lon,
        });
    }
    out
}

fn transitions(d: &[u8], proc_rec: &Record, children_at: usize, fixes: &Fixes) -> Vec<Transition> {
    let mut out = Vec::new();
    for rec in bgl::records(d, proc_rec.start + children_at, proc_rec.end) {
        // The legs of a procedure's own path hang directly off it.
        if matches!(rec.id, LEGS_PLAIN | LEGS_FINAL | LEGS_MISSED | LEGS_TRANSITION) {
            let part = match rec.id {
                LEGS_FINAL => "final",
                LEGS_MISSED => "missed",
                _ => "",
            };
            out.push(Transition { name: String::new(), part: part.to_string(), legs: legs(d, &rec, fixes) });
            continue;
        }
        let Some((_, kids_at)) = TRANSITIONS.iter().find(|(id, _)| *id == rec.id) else { continue };
        let name_at = match rec.id {
            0x4A => rec.start + 0x08,
            0x49 => rec.start + 0x14,
            _ => rec.start,
        };
        let name = if name_at > rec.start {
            let end = (name_at + 8).min(rec.end);
            String::from_utf8_lossy(&d[name_at..end]).trim_end_matches('\0').trim().to_string()
        } else {
            String::new()
        };
        for legrec in bgl::records(d, rec.start + kids_at, rec.end) {
            if matches!(legrec.id, LEGS_PLAIN | LEGS_FINAL | LEGS_MISSED | LEGS_TRANSITION) {
                let part = match legrec.id {
                    LEGS_FINAL => "final",
                    LEGS_MISSED => "missed",
                    _ => "",
                };
                out.push(Transition { name: name.clone(), part: part.to_string(), legs: legs(d, &legrec, fixes) });
            }
        }
    }
    out
}

fn name_at(d: &[u8], rec: &Record, at: usize) -> String {
    let start = rec.start + at;
    let end = (start + 8).min(rec.end);
    if start >= end {
        return String::new();
    }
    String::from_utf8_lossy(&d[start..end]).trim_end_matches('\0').trim().to_string()
}

/// The runway an approach serves, taken from the runway fix its final legs end at
/// (`RW13R`), which is more dependable than the coded runway in the header.
fn runway_of(transitions: &[Transition]) -> String {
    transitions
        .iter()
        .filter(|t| t.part == "final")
        .flat_map(|t| t.legs.iter())
        .filter_map(|l| l.fix.strip_prefix("RW").map(str::to_string))
        .next_back()
        .unwrap_or_default()
}

/// The runway coded in an approach's header: a number at +0x07, and a left/right/centre
/// designator in the top half of the byte after it.
fn header_runway(d: &[u8], rec: &Record) -> String {
    if rec.end - rec.start < 0x0A {
        return String::new();
    }
    let number = d[rec.start + 0x07];
    if number == 0 || number > 36 {
        return String::new();
    }
    let designator = match d[rec.start + 0x08] >> 4 {
        1 => "L",
        2 => "R",
        3 => "C",
        _ => "",
    };
    format!("{number:02}{designator}")
}

/// Put the fixes that have no position of their own where they belong.
///
/// A fix like "FUN12" is not in the waypoint table: it is a position on a radial from a
/// beacon, and the leg says which beacon, which radial and how far. Outside the United
/// States most approaches are written that way, so without this the plan view of an
/// approach is nearly empty.
///
/// Radials are magnetic. The variation is not stated anywhere we can read, so it is
/// measured: any fix that does have a position, and is also given as a radial from a
/// beacon, says what the difference between the two is here. Where no fix says, the
/// fixes that need one cannot be placed, and are left where they were - unplaced.
fn place_fixes_on_radials(procedures: &mut [Procedure]) {
    let mut known: Vec<f64> = Vec::new();
    for leg in procedures.iter().flat_map(|p| p.transitions.iter()).flat_map(|t| t.legs.iter()) {
        let (Some(lat), Some(lon), Some(theta)) = (leg.lat, leg.lon, leg.theta_deg) else { continue };
        let Some(beacon) = super::navaids::find(&leg.navaid) else { continue };
        let north = lat - beacon.lat;
        let east = (lon - beacon.lon) * beacon.lat.to_radians().cos().max(0.05);
        if north.hypot(east) * 60.0 < 0.5 {
            continue;
        }
        let true_deg = (east.atan2(north).to_degrees() + 360.0) % 360.0;
        known.push((true_deg - theta + 540.0) % 360.0 - 180.0);
    }
    // The middle of what the fixes say, which shrugs off one badly coded leg.
    let variation = if known.is_empty() {
        None
    } else {
        known.sort_by(f64::total_cmp);
        Some(known[known.len() / 2])
    };
    for leg in procedures.iter_mut().flat_map(|p| p.transitions.iter_mut()).flat_map(|t| t.legs.iter_mut()) {
        if leg.lat.is_some() {
            continue;
        }
        // A leg that ends at the beacon itself ends where the beacon is.
        if let Some(beacon) = super::navaids::find(&leg.fix) {
            leg.lat = Some(beacon.lat);
            leg.lon = Some(beacon.lon);
            leg.placed_on_radial = true;
            continue;
        }
        let (Some(variation), Some(theta), Some(rho)) = (variation, leg.theta_deg, leg.rho_nm) else { continue };
        let Some(beacon) = super::navaids::find(&leg.navaid) else { continue };
        let (lat, lon) = super::navaids::along_radial(beacon, theta, rho, variation);
        leg.lat = Some(lat);
        leg.lon = Some(lon);
        leg.placed_on_radial = true;
    }
    // A fix that one leg has placed is the same fix wherever else it is named: a hold
    // gives only its fix and its inbound course, and has to borrow the position from the
    // leg that flew there.
    let mut known_fixes: std::collections::HashMap<String, (f64, f64)> = std::collections::HashMap::new();
    for leg in procedures.iter().flat_map(|p| p.transitions.iter()).flat_map(|t| t.legs.iter()) {
        if let (Some(lat), Some(lon)) = (leg.lat, leg.lon) {
            known_fixes.entry(leg.fix.clone()).or_insert((lat, lon));
        }
    }
    for leg in procedures.iter_mut().flat_map(|p| p.transitions.iter_mut()).flat_map(|t| t.legs.iter_mut()) {
        if leg.lat.is_none() {
            if let Some((lat, lon)) = known_fixes.get(&leg.fix) {
                leg.lat = Some(*lat);
                leg.lon = Some(*lon);
            }
        }
    }
}

/// Fix positions, by ident.
pub type Fixes = std::collections::HashMap<String, (f64, f64)>;

/// Every waypoint in one file. Procedures name their fixes by ident alone, and the
/// terminal fixes of an airport sit in the same file as the airport, so this is what
/// turns a list of names into something that can be drawn.
fn waypoints(d: &[u8]) -> Fixes {
    let mut out = Fixes::new();
    for rec in bgl::section_records(d, bgl::SECTION_WAYPOINT) {
        if rec.id != bgl::REC_WAYPOINT || rec.end - rec.start < 28 {
            continue;
        }
        let ident = bgl::ident(bgl::u32le(d, rec.start + 0x14));
        if ident.is_empty() {
            continue;
        }
        let lon = bgl::lon(bgl::u32le(d, rec.start + 0x08));
        let lat = bgl::lat(bgl::u32le(d, rec.start + 0x0C));
        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
            out.entry(ident).or_insert((lat, lon));
        }
    }
    out
}

fn airport(d: &[u8], rec: &Record, file: &Path, fixes: &Fixes) -> Option<AirportProcedures> {
    if rec.end - rec.start < 0x44 {
        return None;
    }
    let icao = bgl::ident(bgl::u32le(d, rec.start + 0x28));
    if icao.is_empty() {
        return None;
    }
    let mut procedures = Vec::new();
    for child in bgl::records(d, rec.start + 0x44, rec.end) {
        let kind = match child.id {
            REC_SID => Kind::Sid,
            REC_STAR => Kind::Star,
            REC_APPROACH => Kind::Approach,
            _ => continue,
        };
        let children_at = if kind == Kind::Approach { 0x24 } else { 0x14 };
        let trans = transitions(d, &child, children_at, fixes);
        let runway = if kind == Kind::Approach {
            let from_legs = runway_of(&trans);
            if from_legs.is_empty() { header_runway(d, &child) } else { from_legs }
        } else {
            String::new()
        };
        let name = match kind {
            Kind::Approach => format!("RW{runway}"),
            _ => name_at(d, &child, 0x0C),
        };
        procedures.push(Procedure { kind, name, runway, variant: None, transitions: trans });
    }
    if procedures.is_empty() {
        return None;
    }
    place_fixes_on_radials(&mut procedures);
    // An approach to no particular runway takes a letter, the way a circling approach
    // is named on a chart.
    let mut letter = b'A';
    for p in procedures.iter_mut().filter(|p| p.kind == Kind::Approach && p.runway.is_empty()) {
        p.runway = (letter as char).to_string();
        p.name = format!("APPROACH {}", p.runway);
        letter = if letter >= b'Z' { b'Z' } else { letter + 1 };
    }
    // Number the approaches that share a runway, so one can be asked for by name.
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for p in procedures.iter_mut().filter(|p| p.kind == Kind::Approach) {
        let n = seen.entry(p.runway.clone()).or_insert(0);
        *n += 1;
        p.variant = Some(*n);
        if *n > 1 {
            p.name = format!("RW{} ({})", p.runway, n);
        }
    }
    Some(AirportProcedures {
        icao,
        lat: bgl::lat(bgl::u32le(d, rec.start + 0x10)),
        lon: bgl::lon(bgl::u32le(d, rec.start + 0x0C)),
        procedures,
        frequencies: frequencies(d, rec),
        source: file.file_name().unwrap_or_default().to_string_lossy().to_string(),
    })
}

/// Read the procedures of one airport from the simulator's navigation data. None when
/// no simulator is installed, or the airport has no published procedures.
pub fn find(icao: &str) -> Result<Option<AirportProcedures>> {
    let want = icao.to_uppercase();
    for dir in super::nav_dirs() {
        for file in walk_nax(&dir) {
            let data = std::fs::read(&file).with_context(|| format!("read {}", file.display()))?;
            for rec in bgl::section_records(&data, bgl::SECTION_AIRPORT) {
                if rec.id != bgl::REC_AIRPORT || rec.end - rec.start < 0x44 {
                    continue;
                }
                if bgl::ident(bgl::u32le(&data, rec.start + 0x28)) != want {
                    continue;
                }
                if let Some(a) = airport(&data, &rec, &file, &waypoints(&data)) {
                    return Ok(Some(a));
                }
            }
        }
    }
    Ok(None)
}

fn walk_nax(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_nax(&p));
        } else if p.file_name().map_or(false, |n| n.to_string_lossy().to_uppercase().starts_with("NAX")) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn altitudes_convert_and_ignore_the_empty_marker() {
        assert_eq!(feet(914.4), Some(3000.0));
        assert_eq!(feet(-1.0), None);
        assert_eq!(feet(0.0), None);
    }

    #[test]
    fn a_runway_comes_from_the_final_leg() {
        let t = vec![Transition {
            name: String::new(),
            part: "final".into(),
            legs: vec![
                Leg { path: "IF".into(), fix: "MORRY".into(), altitude_rule: AltitudeRule::At, ..Leg::default() },
                Leg { path: "TF".into(), fix: "RW13R".into(), ..Leg::default() },
            ],
        }];
        assert_eq!(runway_of(&t), "13R");
    }
}
