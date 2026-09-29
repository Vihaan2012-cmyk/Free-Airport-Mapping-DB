//! The FAA's Coded Instrument Flight Procedures (CIFP): every published departure,
//! arrival and approach in the United States as ARINC 424-18 records, free and public
//! domain, one zip of about 9 MB a cycle. Read into the same `AirportProcedures` the
//! simulator's navigation data gives, so every chart that draws from one draws from the
//! other.
//!
//! The file is fixed-width, 132 columns a record. What is read:
//!
//! * `PA` airports: reference point and magnetic variation
//! * `PC` terminal waypoints, `PG` runways, `PN` terminal NDBs, `EA` enroute waypoints,
//!   `D ` VHF navaids and `DB` NDBs: where each fix a leg names is
//! * `PD` departures, `PE` arrivals and `PF` approaches: the legs, in order, with their
//!   path terminators, altitudes, courses, distances, turns, speeds and fix roles
//!
//! An approach's legs run from the final approach through the missed approach in one
//! route; the missed approach is split off where a leg is marked the first of it.
//!
//! Everything else a chart prints beside a procedure comes out of the same file too
//! (`Nav`): VOR and NDB beacons, localisers with their DMEs, runway thresholds and
//! crossing heights, the minimum safe altitudes, the grid minimum off-route altitudes,
//! the holds the procedures fly and the transition altitudes. Charts drawn from these
//! alone can be passed on: the CIFP is the FAA's, and in the public domain.

use crate::cache::Cache;
use crate::sources::http::Http;
use crate::sources::msfs::procedures::{AirportProcedures, AltitudeRule, ApproachType, FixRole, Kind, Leg, Procedure, Transition, Turn};
use crate::sources::navdata as nd;
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;

/// The day a cycle comes into force: 3 September 2026 for 2609, every 28 days on.
pub fn cycle_start(cycle: &str) -> Option<chrono::NaiveDate> {
    let (y, n): (i64, i64) = (cycle.get(..2)?.parse().ok()?, cycle.get(2..)?.parse().ok()?);
    if !(1..=13).contains(&n) {
        return None;
    }
    let ordinal = (y - 26) * 13 + (n - 1) - 8;
    chrono::NaiveDate::from_ymd_opt(2026, 9, 3)?.checked_add_signed(chrono::Duration::days(ordinal * 28))
}

fn zip_url(cycle: &str) -> Option<String> {
    Some(format!("https://aeronav.faa.gov/Upload_313-d/cifp/CIFP_{}.zip", cycle_start(cycle)?.format("%y%m%d")))
}

/// The cycle's ARINC 424 file, fetched once and kept.
pub fn file(http: &Http, cache: &Cache, cycle: &str) -> Result<String> {
    let url = zip_url(cycle).ok_or_else(|| anyhow!("{cycle} is not a cycle"))?;
    let zip = cache.get_or_fetch_bytes(&format!("cifp/CIFP_{cycle}.zip"), || http.get_bytes(&url)).with_context(|| format!("the FAA's CIFP for cycle {cycle}"))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip))?;
    let name = (0..archive.len()).filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string())).find(|n| n.to_uppercase().ends_with("FAACIFP18")).ok_or_else(|| anyhow!("no FAACIFP18 in the CIFP zip"))?;
    let mut text = Vec::new();
    archive.by_name(&name)?.read_to_end(&mut text)?;
    Ok(String::from_utf8_lossy(&text).into_owned())
}

/// Columns `a` to `b` of a record, as ARINC 424 numbers them (from 1, inclusive).
fn col(line: &str, a: usize, b: usize) -> &str {
    line.get(a - 1..b.min(line.len())).unwrap_or("")
}

/// `N42214660` / `W071002300`: degrees, minutes, seconds and hundredths.
fn coord(s: &str) -> Option<f64> {
    let s = s.trim();
    if !s.is_ascii() || s.len() < 2 {
        return None;
    }
    let (hemi, digits) = s.split_at(1);
    let deg_len = if hemi == "N" || hemi == "S" { 2 } else { 3 };
    if digits.len() < deg_len + 6 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let d: f64 = digits[..deg_len].parse().ok()?;
    let m: f64 = digits[deg_len..deg_len + 2].parse().ok()?;
    let sec: f64 = digits[deg_len + 2..].parse::<f64>().ok()? / 100.0;
    let v = d + m / 60.0 + sec / 3600.0;
    Some(if hemi == "S" || hemi == "W" { -v } else { v })
}

fn tenths(s: &str) -> Option<f64> {
    let s = s.trim();
    (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).then(|| s.parse::<f64>().ok().map(|v| v / 10.0)).flatten()
}

/// An altitude field: `06000` feet, or `FL180`.
fn altitude(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(fl) = s.strip_prefix("FL") {
        return fl.parse::<f64>().ok().map(|v| v * 100.0);
    }
    s.parse::<f64>().ok().filter(|v| *v > 0.0)
}

/// Where each fix is: the airport's own (terminal waypoints, runways, terminal NDBs) by
/// airport and name, the rest (enroute waypoints, navaids) by name and ICAO region.
#[derive(Default)]
struct Fixes {
    terminal: HashMap<(String, String), (f64, f64)>,
    enroute: HashMap<(String, String), (f64, f64)>,
}

impl Fixes {
    fn find(&self, airport: &str, fix: &str, region: &str, section: &str) -> Option<(f64, f64)> {
        let terminal = || self.terminal.get(&(airport.to_string(), fix.to_string())).copied();
        let enroute = || self.enroute.get(&(fix.to_string(), region.to_string())).copied();
        if section.starts_with('P') {
            terminal().or_else(enroute)
        } else {
            enroute().or_else(terminal)
        }
    }
}

/// The approach type a final approach's route type says.
fn approach_type(route: char) -> Option<ApproachType> {
    match route {
        'I' => Some(ApproachType::Ils),
        'L' | 'U' => Some(ApproachType::Localiser),
        'B' => Some(ApproachType::LocaliserBackCourse),
        'X' => Some(ApproachType::Lda),
        'R' | 'H' | 'J' => Some(ApproachType::Rnav),
        'P' => Some(ApproachType::Gps),
        'V' | 'D' | 'S' | 'T' => Some(ApproachType::Vor),
        'N' | 'Q' => Some(ApproachType::Ndb),
        _ => None,
    }
}

/// A departure's or arrival's route type: which part of the procedure it is.
fn terminal_part(kind: Kind, route: char) -> &'static str {
    match (kind, route) {
        (Kind::Sid, '1' | '4' | 'F' | 'T') => "runway",
        (Kind::Sid, '3' | '6' | 'S' | 'V') => "enroute",
        (Kind::Star, '3' | '6' | '9' | 'S') => "runway",
        (Kind::Star, '1' | '4' | '7' | 'F') => "enroute",
        _ => "common",
    }
}

/// One leg out of a procedure record.
fn leg(line: &str, airport: &str, fixes: &Fixes) -> Leg {
    let fix = col(line, 30, 34).trim().to_string();
    let region = col(line, 35, 36).trim();
    let section = col(line, 37, 38);
    let pos = if fix.is_empty() { None } else { fixes.find(airport, &fix, region, section) };
    let alt1 = altitude(col(line, 85, 89));
    let alt2 = altitude(col(line, 90, 94));
    let rule = match col(line, 83, 83) {
        "+" | "H" | "J" | "V" | "C" => AltitudeRule::AtOrAbove,
        "-" | "Y" => AltitudeRule::AtOrBelow,
        "B" => AltitudeRule::Between,
        _ if alt1.is_some() => AltitudeRule::At,
        _ => AltitudeRule::None,
    };
    let path = col(line, 48, 49).trim().to_string();
    let dist = col(line, 75, 78).trim();
    // An arc leg flies round a centre at a radius: a DME arc's radius is the distance from
    // its navaid; a radius-to-fix leg carries its own.
    let rho = if path == "RF" { col(line, 57, 62).trim().parse::<f64>().ok().map(|v| v / 1000.0) } else { tenths(col(line, 67, 70)) };
    let desc = col(line, 40, 43);
    let role = match desc.chars().nth(3) {
        Some('A' | 'C' | 'D') => Some(FixRole::Initial),
        Some('B' | 'I') => Some(FixRole::Intermediate),
        Some('F') => Some(FixRole::Final),
        Some('M') => Some(FixRole::MissedApproachPoint),
        _ => None,
    };
    Leg {
        path,
        fix,
        altitude_rule: rule,
        altitude_ft: alt1,
        altitude2_ft: if rule == AltitudeRule::Between { alt2 } else { None },
        // A course is magnetic unless it ends in T.
        course_deg: tenths(col(line, 71, 74).trim_end_matches('T')),
        // A distance, not a holding time (which starts with T).
        distance_m: (!dist.starts_with('T')).then(|| tenths(dist)).flatten().map(|nm| nm * 1852.0),
        navaid: if col(line, 48, 49) == "RF" { col(line, 107, 111).trim().to_string() } else { col(line, 51, 54).trim().to_string() },
        theta_deg: tenths(col(line, 63, 66)),
        rho_nm: rho,
        turn: match col(line, 44, 44) {
            "L" => Some(Turn::Left),
            "R" => Some(Turn::Right),
            _ => None,
        },
        role,
        placed_on_radial: false,
        speed_kt: col(line, 100, 102).trim().parse::<f64>().ok().filter(|v| *v > 0.0),
        lat: pos.map(|p| p.0),
        lon: pos.map(|p| p.1),
    }
}

/// The runway and suffix an approach identifier names: `I04R` is runway 04R, `H33LZ`
/// runway 33L with the Z that tells two approaches to it apart, `VDM-A` no runway.
fn approach_runway(ident: &str) -> (String, Option<char>) {
    let rest: String = ident.chars().skip(1).collect();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() != 2 {
        let letter = ident.rsplit('-').next().filter(|s| s.len() == 1).and_then(|s| s.chars().next());
        return (letter.map(|c| c.to_string()).unwrap_or_default(), None);
    }
    let after: Vec<char> = rest.chars().skip(2).collect();
    let (side, more) = match after.first() {
        Some(c @ ('L' | 'R' | 'C')) => (Some(*c), &after[1..]),
        _ => (None, &after[..]),
    };
    let runway = format!("{digits}{}", side.map(String::from).unwrap_or_default());
    let suffix = more.iter().find(|c| c.is_ascii_uppercase()).copied().filter(|c| ('S'..='Z').contains(c));
    (runway, suffix)
}

/// Every airport in the file with procedures, by ICAO code.
pub fn parse(text: &str) -> HashMap<String, AirportProcedures> {
    read(text, "").0
}

/// Every airport in the file with procedures, by ICAO code, and everything else a chart
/// prints beside them. `cycle` is what the file is, for the chart's dates.
pub fn read(text: &str, cycle: &str) -> (HashMap<String, AirportProcedures>, Nav) {
    let mut fixes = Fixes::default();
    let mut nav = Nav { cycle: cycle.to_string(), ..Nav::default() };
    let mut airports: HashMap<String, (f64, f64, Option<f64>)> = HashMap::new();
    let mut msa_lines = Vec::new();
    for line in text.lines().filter(|l| l.len() >= 51 && l.starts_with('S')) {
        let (sec, sub) = (col(line, 5, 5), if col(line, 5, 5) == "P" { col(line, 13, 13) } else { col(line, 6, 6) });
        let pos = || coord(col(line, 33, 41)).zip(coord(col(line, 42, 51)));
        let airport = || col(line, 7, 10).trim().to_string();
        let name = || col(line, 94, 123).trim().to_string();
        match (sec, sub) {
            ("P", "A") => {
                if let Some((lat, lon)) = pos() {
                    // W0150 is 15.0 degrees west.
                    let v = col(line, 52, 56);
                    let var = tenths(&v[1.min(v.len())..]).map(|d| if v.starts_with('W') { -d } else { d });
                    airports.insert(airport(), (lat, lon, var));
                    fixes.terminal.insert((airport(), airport()), (lat, lon));
                    let ta = altitude(col(line, 71, 75));
                    let tl = altitude(col(line, 76, 80));
                    nav.info.insert(airport(), nd::AirportInfo { transition_altitude_ft: ta, transition_level_ft: tl, speed_limit: None });
                }
            }
            ("P", "C" | "N") => {
                if let Some(p) = pos() {
                    fixes.terminal.insert((airport(), col(line, 14, 18).trim().to_string()), p);
                    if sub == "N" {
                        if let Some(khz) = tenths(col(line, 23, 27)) {
                            nav.beacons.push(nd::Beacon { ident: col(line, 14, 17).trim().to_string(), name: name(), kind: nd::Kind::Ndb, frequency: khz, lat: p.0, lon: p.1 });
                        }
                    }
                }
            }
            ("P", "G") => {
                if let Some(p) = pos() {
                    let ident = col(line, 14, 18).trim().to_string();
                    fixes.terminal.insert((airport(), ident.clone()), p);
                    let runway = nd::Runway {
                        threshold_crossing_ft: col(line, 76, 77).trim().parse::<f64>().ok().filter(|v| *v > 0.0),
                        threshold_elevation_ft: col(line, 67, 71).trim().parse::<f64>().ok(),
                        magnetic_bearing_deg: tenths(col(line, 28, 31)),
                    };
                    nav.runways.insert((airport(), ident.trim_start_matches("RW").to_string()), (p, runway));
                }
            }
            ("P", "I") => {
                // A localiser: its ident, frequency, the runway it serves and its course.
                let ils = nd::Ils {
                    ident: col(line, 14, 17).trim().to_string(),
                    frequency: col(line, 23, 27).trim().parse::<f64>().map(|v| v / 100.0).unwrap_or(0.0),
                    course_mag_deg: tenths(col(line, 52, 55)),
                    glidepath_deg: col(line, 88, 90).trim().parse::<f64>().ok().filter(|v| *v > 0.0).map(|v| v / 100.0),
                    dme: None,
                    category: match col(line, 18, 18) {
                        "1" => Some("I".into()),
                        "2" => Some("II".into()),
                        "3" => Some("III".into()),
                        _ => None,
                    },
                    markers: Vec::new(),
                };
                nav.ils.insert((airport(), col(line, 28, 32).trim().trim_start_matches("RW").to_string()), ils);
            }
            ("P", "S") => msa_lines.push(line),
            ("E", "A") => {
                if let Some(p) = pos() {
                    fixes.enroute.insert((col(line, 14, 18).trim().to_string(), col(line, 20, 21).trim().to_string()), p);
                }
            }
            ("D", " " | "B") => {
                let ident = col(line, 14, 17).trim().to_string();
                if let Some(p) = pos() {
                    fixes.enroute.insert((ident.clone(), col(line, 20, 21).trim().to_string()), p);
                    let (kind, frequency) = if sub == "B" { (nd::Kind::Ndb, tenths(col(line, 23, 27))) } else { (nd::Kind::Vor, col(line, 23, 27).trim().parse::<f64>().ok().map(|v| v / 100.0)) };
                    if let Some(frequency) = frequency {
                        nav.beacons.push(nd::Beacon { ident: ident.clone(), name: name(), kind, frequency, lat: p.0, lon: p.1 });
                    }
                }
                // Where the DME is: an ILS/DME's is what the distances on an approach are
                // measured from.
                if sub == " " {
                    if let Some(d) = coord(col(line, 56, 64)).zip(coord(col(line, 65, 74))) {
                        nav.dmes.entry(ident).or_default().push((airport(), d));
                    }
                }
            }
            ("A", "S") => {
                // A band of grid minimum off-route altitudes: its south-west corner, then
                // thirty one-degree squares eastward, in hundreds of feet.
                let lat = col(line, 14, 16);
                let lon = col(line, 17, 20);
                let (Some(la), Some(lo)) = (lat.get(1..).and_then(|v| v.parse::<f64>().ok()), lon.get(1..).and_then(|v| v.parse::<f64>().ok())) else { continue };
                let la = if lat.starts_with('S') { -la } else { la };
                let lo = if lon.starts_with('W') { -lo } else { lo };
                for i in 0..30 {
                    if let Some(h) = col(line, 31 + 3 * i, 33 + 3 * i).trim().parse::<f64>().ok().filter(|h| *h > 0.0) {
                        nav.mora.push((la, lo + i as f64, h * 100.0));
                    }
                }
            }
            _ => {}
        }
    }
    // The minimum safe altitudes, once every centre they are about can be found.
    for line in msa_lines {
        let airport = col(line, 7, 10).trim().to_string();
        let centre = col(line, 14, 18).trim().to_string();
        let region = col(line, 19, 20).trim();
        let section = col(line, 21, 22);
        let Some(at) = fixes.find(&airport, &centre, region, section) else { continue };
        let mut sectors = Vec::new();
        let mut radius: f64 = 0.0;
        for i in 0..7 {
            let at_col = 43 + 11 * i;
            let (from, to, alt, r) = (col(line, at_col, at_col + 2), col(line, at_col + 3, at_col + 5), col(line, at_col + 6, at_col + 8), col(line, at_col + 9, at_col + 10));
            let (Ok(from), Ok(to), Ok(alt)) = (from.trim().parse::<f64>(), to.trim().parse::<f64>(), alt.trim().parse::<f64>()) else { break };
            radius = radius.max(r.trim().parse::<f64>().unwrap_or(25.0));
            sectors.push(nd::MsaSector { from_deg: from, to_deg: to, altitude_ft: alt * 100.0 });
        }
        if !sectors.is_empty() {
            let name = if centre == airport { "ARP".to_string() } else { centre };
            nav.msa.entry(airport).or_default().push(nd::Msa { centre: at, centre_name: name, radius_nm: if radius > 0.0 { radius } else { 25.0 }, sectors });
        }
    }

    // Procedure records, grouped by airport, then procedure, then route and transition,
    // keeping the file's order within each.
    type RouteKey = (char, String);
    let mut procs: BTreeMap<(String, char, String), Vec<(RouteKey, Vec<Leg>, Vec<String>)>> = BTreeMap::new();
    for line in text.lines().filter(|l| l.len() >= 100 && l.starts_with('S') && col(l, 5, 5) == "P") {
        let sub = col(line, 13, 13).chars().next().unwrap_or(' ');
        if !matches!(sub, 'D' | 'E' | 'F') || col(line, 39, 39) != "0" && col(line, 39, 39) != "1" {
            continue;
        }
        let airport = col(line, 7, 10).trim().to_string();
        let ident = col(line, 14, 19).trim().to_string();
        let route = col(line, 20, 20).chars().next().unwrap_or(' ');
        let trans = col(line, 21, 25).trim().to_string();
        let entry = procs.entry((airport.clone(), sub, ident)).or_default();
        let key = (route, trans);
        if entry.last().map(|(k, _, _)| k != &key).unwrap_or(true) {
            entry.push((key, Vec::new(), Vec::new()));
        }
        let last = entry.last_mut().expect("just pushed");
        let this = leg(line, &airport, &fixes);
        // A hold a procedure flies, as the chart prints holds.
        if matches!(this.path.as_str(), "HM" | "HF" | "HA") {
            if let (Some(lat), Some(lon), Some(inbound)) = (this.lat, this.lon, this.course_deg) {
                let length = col(line, 75, 78).trim();
                let hold = nd::Hold {
                    fix: this.fix.clone(),
                    lat,
                    lon,
                    inbound_deg: inbound,
                    right_turns: this.turn != Some(Turn::Left),
                    leg_time_min: length.strip_prefix('T').and_then(tenths),
                    leg_nm: (!length.starts_with('T')).then(|| tenths(length)).flatten(),
                    max_altitude_ft: None,
                    min_altitude_ft: this.altitude_ft,
                    speed_kt: this.speed_kt,
                };
                let at = nav.holds.entry(this.fix.clone()).or_default();
                if !at.iter().any(|h| (h.inbound_deg - hold.inbound_deg).abs() < 1.0 && h.right_turns == hold.right_turns) {
                    at.push(hold);
                }
            }
        }
        last.1.push(this);
        last.2.push(col(line, 40, 43).to_string());
    }

    let mut out: HashMap<String, AirportProcedures> = HashMap::new();
    for ((airport, sub, ident), routes) in procs {
        let Some(&(lat, lon, var)) = airports.get(&airport) else { continue };
        let kind = match sub {
            'D' => Kind::Sid,
            'E' => Kind::Star,
            _ => Kind::Approach,
        };
        let mut transitions = Vec::new();
        let mut approach_kind = None;
        for ((route, trans), legs, descs) in routes {
            match kind {
                Kind::Approach if route == 'A' => transitions.push(Transition { name: trans, part: String::new(), legs }),
                Kind::Approach => {
                    approach_kind = approach_kind.or(approach_type(route));
                    // The missed approach begins at the leg marked the first of it.
                    let split = descs.iter().position(|d| d.chars().nth(2) == Some('M')).unwrap_or(legs.len());
                    let (fin, missed) = legs.split_at(split);
                    transitions.push(Transition { name: String::new(), part: "final".into(), legs: fin.to_vec() });
                    if !missed.is_empty() {
                        transitions.push(Transition { name: String::new(), part: "missed".into(), legs: missed.to_vec() });
                    }
                }
                _ => transitions.push(Transition { name: trans, part: terminal_part(kind, route).into(), legs }),
            }
        }
        let (runway, suffix, name) = if kind == Kind::Approach {
            let (runway, suffix) = approach_runway(&ident);
            let label = approach_kind.map(ApproachType::label).unwrap_or("APPROACH");
            let s = suffix.map(|c| format!(" {c}")).unwrap_or_default();
            let name = if runway.chars().next().is_some_and(|c| c.is_ascii_digit()) { format!("{label}{s} RWY {runway}") } else { format!("{label}-{runway}") };
            (runway, suffix, name)
        } else {
            (String::new(), None, ident.clone())
        };
        let a = out.entry(airport.clone()).or_insert_with(|| AirportProcedures {
            icao: airport.clone(),
            lat,
            lon,
            procedures: Vec::new(),
            magnetic_variation_deg: var,
            frequencies: Vec::new(),
            source: "FAA CIFP".to_string(),
        });
        a.procedures.push(Procedure { kind, name, approach_type: approach_kind.filter(|_| kind == Kind::Approach), suffix, variant: None, runway, transitions });
    }
    // Number the approaches that share a runway, as the simulator's reader does.
    for a in out.values_mut() {
        let mut seen: HashMap<String, usize> = HashMap::new();
        for p in a.procedures.iter_mut().filter(|p| p.kind == Kind::Approach) {
            let n = seen.entry(p.runway.clone()).or_insert(0);
            *n += 1;
            p.variant = Some(*n);
        }
    }
    // A localiser's DME is the one of its name at its airport.
    for ((airport, _), ils) in nav.ils.iter_mut() {
        ils.dme = nav.dmes.get(&ils.ident).and_then(|all| all.iter().find(|(a, _)| a == airport).or_else(|| all.first())).map(|(_, p)| *p);
    }
    (out, nav)
}

/// What the CIFP says besides its procedures: everything a chart prints beside one.
#[derive(Default)]
pub struct Nav {
    pub cycle: String,
    beacons: Vec<nd::Beacon>,
    dmes: HashMap<String, Vec<(String, (f64, f64))>>,
    ils: HashMap<(String, String), nd::Ils>,
    runways: HashMap<(String, String), ((f64, f64), nd::Runway)>,
    msa: HashMap<String, Vec<nd::Msa>>,
    mora: Vec<(f64, f64, f64)>,
    holds: HashMap<String, Vec<nd::Hold>>,
    info: HashMap<String, nd::AirportInfo>,
}

/// The ways an airport can be named: CIFP names one with no ICAO code by its FAA
/// identifier (00R), where OurAirports and the built airports put a K in front (K00R).
pub fn spellings(icao: &str) -> Vec<String> {
    let icao = icao.trim().to_uppercase();
    let mut out = vec![icao.clone()];
    if icao.len() == 3 {
        out.push(format!("K{icao}"));
    } else if icao.len() == 4 && icao.starts_with('K') && icao[1..].chars().any(|c| c.is_ascii_digit()) {
        out.push(icao[1..].to_string());
    }
    out
}

fn nm_between(a: (f64, f64), b: (f64, f64)) -> f64 {
    let cos = a.0.to_radians().cos().max(0.05);
    ((a.0 - b.0) * 60.0).hypot((a.1 - b.1) * 60.0 * cos)
}

impl Nav {
    /// The dates the cycle is in force, as a chart prints them.
    pub fn dates(&self) -> Option<(String, String)> {
        let from = cycle_start(&self.cycle)?;
        let to = from.checked_add_signed(chrono::Duration::days(28))?;
        let f = |d: chrono::NaiveDate| d.format("%d %b %Y").to_string().to_uppercase();
        Some((f(from), f(to)))
    }

    pub fn source(&self) -> String {
        format!("FAA CIFP (cycle {})", self.cycle)
    }

    pub fn beacons_near(&self, lat: f64, lon: f64, radius_nm: f64) -> Vec<nd::Beacon> {
        let mut found: Vec<(f64, &nd::Beacon)> = self.beacons.iter().map(|b| (nm_between((lat, lon), (b.lat, b.lon)), b)).filter(|(d, _)| *d <= radius_nm).collect();
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        found.into_iter().map(|(_, b)| b.clone()).collect()
    }

    /// Where a navaid is, the nearest of its name to a place: a beacon, or a DME.
    pub fn navaid_near(&self, ident: &str, near: (f64, f64)) -> Option<(f64, f64)> {
        let beacons = self.beacons.iter().filter(|b| b.ident.eq_ignore_ascii_case(ident)).map(|b| (b.lat, b.lon));
        let dmes = self.dmes.get(&ident.to_uppercase()).into_iter().flatten().map(|(_, p)| *p);
        beacons.chain(dmes).filter(|p| nm_between(near, *p) < 300.0).min_by(|a, b| nm_between(near, *a).total_cmp(&nm_between(near, *b)))
    }

    pub fn ils(&self, icao: &str, runway: &str) -> Option<nd::Ils> {
        let rw = runway.trim_start_matches("RW").to_uppercase();
        spellings(icao).into_iter().find_map(|a| self.ils.get(&(a, rw.clone())).cloned())
    }

    fn runway_record(&self, icao: &str, runway: &str) -> Option<&((f64, f64), nd::Runway)> {
        let rw = runway.trim_start_matches("RW").to_uppercase();
        spellings(icao).into_iter().find_map(|a| self.runways.get(&(a, rw.clone())))
    }

    pub fn runway(&self, icao: &str, runway: &str) -> Option<nd::Runway> {
        self.runway_record(icao, runway).map(|(_, r)| *r)
    }

    pub fn runway_threshold(&self, icao: &str, runway: &str) -> Option<(f64, f64)> {
        self.runway_record(icao, runway).map(|(p, _)| *p)
    }

    fn msas(&self, icao: &str) -> Option<&Vec<nd::Msa>> {
        spellings(icao).into_iter().find_map(|a| self.msa.get(&a))
    }

    /// The airport's minimum safe altitude nearest a place.
    pub fn msa(&self, icao: &str, at: (f64, f64)) -> Option<nd::Msa> {
        self.msas(icao)?.iter().min_by(|a, b| nm_between(at, a.centre).total_cmp(&nm_between(at, b.centre))).cloned()
    }

    pub fn msa_about(&self, icao: &str, centre: &str) -> Option<nd::Msa> {
        self.msas(icao)?.iter().find(|m| m.centre_name.eq_ignore_ascii_case(centre)).cloned()
    }

    pub fn holds_at(&self, fix: &str) -> Vec<nd::Hold> {
        self.holds.get(&fix.to_uppercase()).cloned().unwrap_or_default()
    }

    /// The grid minimum off-route altitudes over an area: (south edge, west edge, feet)
    /// for each one-degree square, the highest where a square is given twice.
    pub fn grid_mora(&self, south: f64, north: f64, west: f64, east: f64) -> Vec<(f64, f64, f64)> {
        let mut best: BTreeMap<(i64, i64), f64> = BTreeMap::new();
        for &(la, lo, ft) in &self.mora {
            if la + 1.0 < south || la > north || lo + 1.0 < west || lo > east {
                continue;
            }
            let e = best.entry((la.round() as i64, lo.round() as i64)).or_insert(0.0);
            *e = e.max(ft);
        }
        best.into_iter().map(|((la, lo), ft)| (la as f64, lo as f64, ft)).collect()
    }

    pub fn airport_info(&self, icao: &str) -> Option<nd::AirportInfo> {
        spellings(icao).into_iter().find_map(|a| self.info.get(&a).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Records from cycle 2609, exactly as the file has them: Logan, runway 04R, the fixes
    // used, the ILS 04R (the GOSHI feeder, the final and the missed approach) and the
    // start of the BLZZR SIX departure from 04R.
    const SAMPLE: &str = "SUSAP KBOSK6ABOS     0     100YHN42214660W071002300W015000019         1800018000C    MNAR    GENERAL EDWARD LAWRENCE LOGAN 388731711
SUSAP KBOSK6GRW04R   0100060350 N42211455W071003727         -0022300018115551150IIBOS3                                     398102004
SUSAP KBOSK6CGOSHI K60    W     N42021109W071094714                       W0139     NAR           GOSHI                    389382504
SUSAP KBOSK6CWINNI K60    C     N42070171W071072822                       W0139     NAR           WINNI                    390442605
SUSAP KBOSK6CNABBO K60    C     N42114268W071051320                       W0139     NAR           NABBO                    389792605
SUSAP KBOSK6CMILTT K60    C     N42162512W071025708                       W0139     NAR           MILTT                    389742605
SUSAP KBOSK6CWAXEN K60    C     N42350442W070544669                       W0141     NAR           WAXEN                    390412504
SUSAP KBOSK6CNHANT K60    W     N42262324W070575997                       W0140     NAR           NHANT                    389812605
SUSAP KBOSK6FI04R  AGOSHI 010GOSHIK6PC0E  A    IF                                   06000     18000210              0 PS   396232208
SUSAP KBOSK6FI04R  AGOSHI 020WINNIK6PC0EE B 010TF IBOSK6      21470169        PI  + 04000                           0 PS   396242208
SUSAP KBOSK6FI04R  I      010WINNIK6PC0E  I    IF IBOSK6      21470169        PI  J 040000170018000                 0 DS   396252405
SUSAP KBOSK6FI04R  I      011NABBOK6PC0E       CF IBOSK6      2147011903500050PI  + 03000                           0 DS   396262405
SUSAP KBOSK6FI04R  I      020MILTTK6PC0E  F    CF IBOSK6      2147006903500050PI  H 0170001700            BOS   K6D 0 DS   396272405
SUSAP KBOSK6FI04R  I      030RW04RK6PG0GY M    CF IBOSK6      2147001803500051PI    00069             -300          0 DS   396282405
SUSAP KBOSK6FI04R  I      040WAXENK6PC0EYM     CF BOS K6      0300014003000140D   + 03000                           0 DS   396292405
SUSAP KBOSK6FI04R  I      050WAXENK6PC0EE  L   HM                     2100T010    + 03000                           0 DS   396302405
SUSAP KBOSK6DBLZZR64RW04R 010         0        VA                     0347        + 00520     18000       KBOS  K6PA       390562312
SUSAP KBOSK6DBLZZR64RW04R 020NHANTK6PC0E       DF                                                                          390572312
";

    #[test]
    fn reads_an_approach_with_its_feeder_final_and_missed() {
        let all = parse(SAMPLE);
        let bos = &all["KBOS"];
        assert!((bos.lat - 42.3629).abs() < 0.001 && (bos.lon + 71.0064).abs() < 0.001, "{} {}", bos.lat, bos.lon);
        assert_eq!(bos.magnetic_variation_deg, Some(-15.0));
        let ils = bos.procedures.iter().find(|p| p.kind == Kind::Approach).unwrap();
        assert_eq!(ils.name, "ILS RWY 04R");
        assert_eq!(ils.runway, "04R");
        assert_eq!(ils.approach_type, Some(ApproachType::Ils));
        let parts: Vec<(&str, &str, usize)> = ils.transitions.iter().map(|t| (t.name.as_str(), t.part.as_str(), t.legs.len())).collect();
        assert_eq!(parts, vec![("GOSHI", "", 2), ("", "final", 4), ("", "missed", 2)]);
        let feeder = &ils.transitions[0].legs;
        assert_eq!(feeder[0].role, Some(FixRole::Initial));
        assert_eq!((feeder[0].speed_kt, feeder[0].altitude_ft), (Some(210.0), Some(6000.0)));
        assert_eq!((feeder[1].altitude_rule, feeder[1].altitude_ft), (AltitudeRule::AtOrAbove, Some(4000.0)));
        assert!(feeder[1].lat.is_some(), "the fix is placed from its waypoint record");
        let fin = &ils.transitions[1].legs;
        let roles: Vec<Option<FixRole>> = fin.iter().map(|l| l.role).collect();
        assert_eq!(roles, vec![Some(FixRole::Intermediate), None, Some(FixRole::Final), Some(FixRole::MissedApproachPoint)]);
        assert_eq!(fin[3].fix, "RW04R");
        assert!(fin[3].lat.is_some(), "the runway is placed from its runway record");
        // A localiser course: the navaid, its radial and distance, the course and length.
        assert_eq!((fin[1].navaid.as_str(), fin[1].theta_deg, fin[1].rho_nm, fin[1].course_deg), ("IBOS", Some(214.7), Some(11.9), Some(35.0)));
        assert_eq!(fin[1].distance_m, Some(5.0 * 1852.0));
        let missed = &ils.transitions[2].legs;
        assert_eq!((missed[0].path.as_str(), missed[0].fix.as_str()), ("CF", "WAXEN"));
        // The hold at the end: its course, and a time rather than a distance.
        assert_eq!((missed[1].path.as_str(), missed[1].turn, missed[1].course_deg, missed[1].distance_m), ("HM", Some(Turn::Left), Some(210.0), None));
    }

    #[test]
    fn reads_a_departure_by_its_parts() {
        let all = parse(SAMPLE);
        let sid = all["KBOS"].procedures.iter().find(|p| p.kind == Kind::Sid).unwrap();
        assert_eq!(sid.name, "BLZZR6");
        assert_eq!(sid.transitions[0].name, "RW04R");
        assert_eq!(sid.transitions[0].part, "runway");
        assert_eq!(sid.transitions[0].legs.len(), 2);
        assert_eq!((sid.transitions[0].legs[0].path.as_str(), sid.transitions[0].legs[0].course_deg), ("VA", Some(34.7)));
        assert_eq!(sid.transitions[0].legs[1].fix, "NHANT");
    }

    #[test]
    fn reads_what_a_chart_prints_beside_the_procedure() {
        let (_, nav) = read(SAMPLE, "2609");
        assert_eq!(nav.dates(), Some(("03 SEP 2026".to_string(), "01 OCT 2026".to_string())));
        let rw = nav.runway("KBOS", "04R").unwrap();
        assert_eq!((rw.threshold_crossing_ft, rw.threshold_elevation_ft, rw.magnetic_bearing_deg), (Some(51.0), Some(18.0), Some(35.0)));
        assert!(nav.runway_threshold("KBOS", "RW04R").is_some());
        assert_eq!(nav.airport_info("KBOS").and_then(|i| i.transition_altitude_ft), Some(18000.0));
        // The missed approach's hold at WAXEN: 210 inbound, left turns, a one-minute leg.
        let hold = &nav.holds_at("WAXEN")[0];
        assert_eq!((hold.inbound_deg, hold.right_turns, hold.leg_time_min, hold.min_altitude_ft), (210.0, false, Some(1.0), Some(3000.0)));
    }

    #[test]
    fn airports_are_found_with_or_without_the_k() {
        assert_eq!(spellings("00R"), vec!["00R".to_string(), "K00R".to_string()]);
        assert_eq!(spellings("K00R"), vec!["K00R".to_string(), "00R".to_string()]);
        assert_eq!(spellings("KBOS"), vec!["KBOS".to_string()]);
    }

    #[test]
    fn approach_identifiers() {
        assert_eq!(approach_runway("I04R"), ("04R".to_string(), None));
        assert_eq!(approach_runway("R22LZ"), ("22L".to_string(), Some('Z')));
        assert_eq!(approach_runway("R09"), ("09".to_string(), None));
        assert_eq!(approach_runway("VDM-A"), ("A".to_string(), None));
    }

    #[test]
    fn cycles_start_on_their_day() {
        assert_eq!(cycle_start("2609"), chrono::NaiveDate::from_ymd_opt(2026, 9, 3));
        assert_eq!(cycle_start("2610"), chrono::NaiveDate::from_ymd_opt(2026, 10, 1));
        assert_eq!(zip_url("2609").as_deref(), Some("https://aeronav.faa.gov/Upload_313-d/cifp/CIFP_260903.zip"));
    }
}
