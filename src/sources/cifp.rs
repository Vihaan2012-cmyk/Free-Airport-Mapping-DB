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

use crate::cache::Cache;
use crate::sources::http::Http;
use crate::sources::msfs::procedures::{AirportProcedures, AltitudeRule, ApproachType, FixRole, Kind, Leg, Procedure, Transition, Turn};
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
    let mut fixes = Fixes::default();
    let mut airports: HashMap<String, (f64, f64, Option<f64>)> = HashMap::new();
    for line in text.lines().filter(|l| l.len() >= 51 && l.starts_with('S')) {
        let (sec, sub) = (col(line, 5, 5), if col(line, 5, 5) == "P" { col(line, 13, 13) } else { col(line, 6, 6) });
        let pos = || coord(col(line, 33, 41)).zip(coord(col(line, 42, 51)));
        match (sec, sub) {
            ("P", "A") => {
                if let Some((lat, lon)) = pos() {
                    // W0150 is 15.0 degrees west.
                    let v = col(line, 52, 56);
                    let var = tenths(&v[1.min(v.len())..]).map(|d| if v.starts_with('W') { -d } else { d });
                    airports.insert(col(line, 7, 10).trim().to_string(), (lat, lon, var));
                }
            }
            ("P", "C" | "G" | "N") => {
                if let Some(p) = pos() {
                    fixes.terminal.insert((col(line, 7, 10).trim().to_string(), col(line, 14, 18).trim().to_string()), p);
                }
            }
            ("E", "A") | ("D", " " | "B") => {
                if let Some(p) = pos() {
                    fixes.enroute.insert((col(line, 14, 18).trim().to_string(), col(line, 20, 21).trim().to_string()), p);
                }
            }
            _ => {}
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
        last.1.push(leg(line, &airport, &fixes));
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
    out
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
