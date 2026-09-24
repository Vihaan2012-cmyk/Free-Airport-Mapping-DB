//! Reads the simulator's own navigation data into a [`NavSet`], so that an aircraft's
//! database can be written from it.
//!
//! This is the middle of the converter: [`crate::sources::msfs`] reaches the files (loose
//! for FS2020, packed in one `minimal.fsarchive` for FS2024), [`crate::convert::fenix`] and
//! [`crate::convert::dfd`] write the databases, and this turns one into the other.
//!
//! # What the record layouts are, and how they were established
//!
//! None of this is documented. It was worked out by dumping records and checking the numbers
//! that came out against figures that can be verified independently — the same way the
//! procedure records in [`crate::sources::msfs::procedures`] were decoded, and the X-Plane
//! line codes elsewhere in this crate.
//!
//! ```text
//! section 0x03  airport 0x56   ICAO at +0x28, longitude +0x0C, latitude +0x10,
//!                              elevation +0x14 as millimetres, children from +0x44
//!               child 0x19     the airport's name
//!               child 0x12     the region's name ("CHRISTCHURCH"), not a runway
//!               child 0x42/0x48/0xFA  departures, arrivals, approaches
//! section 0x13  VOR 0x13       longitude +0x08, latitude +0x0C, frequency +0x14 (Hz),
//!                              ident +0x20
//! section 0x17  NDB 0x17       frequency +0x08 (Hz/1000), longitude +0x0C, latitude +0x10,
//!                              ident +0x20
//! section 0x22  waypoint 0x22  type +0x06, route count +0x07, longitude +0x08,
//!                              latitude +0x0C, magnetic variation +0x10 as a float,
//!                              ident +0x14, region +0x18 — twenty-eight bytes, then one
//!                              thirty-three byte entry per airway through it
//! ```
//!
//! The elevation being millimetres is the kind of thing worth showing rather than asserting:
//! Manapouri reads 208,178, which is 683 feet against a published 687, and Queenstown reads
//! 354,939, which is 1,164 against a published 1,171.
//!
//! # The airway entry
//!
//! An airway is not a record of its own. Each waypoint carries one thirty-three byte entry
//! for every airway that passes through it, naming the fix before and the fix after, so an
//! airway is reassembled by following those links from end to end:
//!
//! ```text
//! +0x00  route type
//! +0x01  name, eight bytes, null padded ("W347", "Q67", "UN873")
//! +0x09  the previous fix: ident +0x09, region +0x0D
//! +0x11  the minimum altitude of the leg from that fix, a float in metres
//! +0x15  the next fix: ident +0x15, region +0x19
//! +0x1D  the minimum altitude of the leg to that fix, a float in metres
//! ```
//!
//! The altitudes being metres is again checkable rather than assumed: the figures that come
//! out are 1371.60, 1341.12 and 1188.72 metres, which are 4,500, 4,400 and 3,900 feet to the
//! foot. A minimum enroute altitude is always published on a round hundred of feet, so a
//! field read at the wrong offset or the wrong scale would not land on them.
//!
//! # What is not here, and why
//!
//! **Runway geometry.** The navigation package carries none: an airport record's children are
//! its name, its region and its procedures, and nothing else. Position, length, width,
//! bearing and surface live in the simulator's *scenery* package, which is a different and
//! far larger dataset. A database written from this therefore has procedures that name their
//! runways but no runway records to match, which is stated plainly rather than papered over —
//! see [`NavSet::runways`] coming out empty and what the `convert` command prints.
//!
//! **Published holds and minimum safe altitude sectors.** Not in the simulator's data at all.
//! The holds that form part of a procedure are, as the `HA`/`HF`/`HM` legs they are, and those
//! do come through.

use crate::convert::model::*;
use crate::sources::msfs::{bgl, procedures};
use anyhow::Result;
use std::collections::HashMap;

/// Metres to feet, for the two fields the simulator holds in metric units.
const FT_PER_M: f64 = 3.280_839_895;

/// One airway entry as a waypoint carries it.
#[derive(Debug, Clone)]
struct RouteEntry {
    name: String,
    prev: Option<String>,
    next: Option<String>,
    /// The minimum altitude of the leg on to `next`, feet.
    min_ft: Option<f64>,
    level: AirwayLevel,
}

const ENTRY_LEN: usize = 33;
const WAYPOINT_HEADER: usize = 28;

fn route_entries(d: &[u8]) -> Vec<RouteEntry> {
    let mut out = Vec::new();
    let mut at = WAYPOINT_HEADER;
    while at + ENTRY_LEN <= d.len() {
        let e = &d[at..at + ENTRY_LEN];
        let name: String = e[1..9].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
        if !name.is_empty() {
            let fix = |off: usize| -> Option<String> {
                let id = bgl::ident(bgl::u32le(e, off));
                (!id.is_empty()).then_some(id)
            };
            let metres = |off: usize| -> Option<f64> {
                let v = bgl::f32le(e, off) as f64;
                (v.is_finite() && v > 1.0).then(|| (v * FT_PER_M / 100.0).round() * 100.0)
            };
            out.push(RouteEntry {
                name: name.trim().to_string(),
                prev: fix(0x09),
                next: fix(0x15),
                min_ft: metres(0x1d),
                // The type byte distinguishes the low-level network from the upper one; a
                // value this has not been seen to carry is taken as serving both rather than
                // as excluding a route from a search that would otherwise find it.
                level: match e[0] {
                    1 => AirwayLevel::Low,
                    2 => AirwayLevel::High,
                    _ => AirwayLevel::Both,
                },
            });
        }
        at += ENTRY_LEN;
    }
    out
}

/// One fix as the simulator holds it, before it becomes a [`WaypointRec`].
struct Fix {
    ident: String,
    region: String,
    lat: f64,
    lon: f64,
    routes: Vec<RouteEntry>,
}

/// Read the whole of the simulator's navigation data.
///
/// `cycle` is what to stamp the result with: the simulator does not label its data with an
/// AIRAC cycle anywhere this crate has found, so the caller says what it is rather than this
/// inventing one.
pub fn read(cycle: &str) -> Result<NavSet> {
    read_from(cycle, Simulator::Newest)
}

/// Which simulator's navigation data to read.
///
/// This matters for more than speed. A machine with both simulators installed has two whole
/// navigation datasets, of two different AIRAC cycles, and reading both would put the older
/// one's airports and fixes in the same database as the newer one's — duplicated, and half of
/// them stale. So one is chosen, and by default it is whichever is newer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Simulator {
    /// FS2024's packed archive where it exists, else FS2020's loose files.
    Newest,
    /// FS2020's loose `scenery/*.bgl`.
    Fs2020,
    /// FS2024's `minimal.fsarchive`.
    Fs2024,
}

pub fn read_from(cycle: &str, which: Simulator) -> Result<NavSet> {
    // Every file's bytes, then all of them read at once. Three and a half thousand files
    // each parsed independently is exactly what a thread pool is for, and reading them one
    // after another was the difference between a quarter of a minute and a quarter of an hour.
    let archives = crate::sources::msfs::nav_archives();
    let loose = crate::sources::msfs::nav_dirs();
    let use_archive = match which {
        Simulator::Fs2024 => true,
        Simulator::Fs2020 => false,
        Simulator::Newest => !archives.is_empty(),
    };
    if use_archive && archives.is_empty() {
        anyhow::bail!("no FS2024 navigation archive found on this machine");
    }
    if !use_archive && loose.is_empty() {
        anyhow::bail!("no FS2020 navigation data found on this machine");
    }
    let t0 = std::time::Instant::now();
    let say = |i: usize, name: &str, detail: String, since: std::time::Instant| crate::term::stage(i, 7, name, &detail, since.elapsed().as_millis());
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    if use_archive {
        for path in &archives {
            let archive = crate::sources::msfs::fsarchive::FsArchive::open(path)?;
            archive.for_each_with_prefix(&[""], |_p, data| blobs.push(data))?;
        }
        log::info!("reading FS2024's packed navigation data");
    } else {
        crate::sources::msfs::for_each_bgl(&[""], |_name, data| blobs.push(data));
        log::info!("reading FS2020's navigation data");
    }
    let files = blobs.len();
    let bytes: usize = blobs.iter().map(|b| b.len()).sum();
    say(1, "files read", format!("{files} files, {:.0} MB", bytes as f64 / 1e6), t0);

    let t = std::time::Instant::now();
    use rayon::prelude::*;
    let (airports, navaids, fixes) = blobs
        .par_iter()
        .map(|data| {
            let (mut a, mut n, mut f) = (Vec::new(), Vec::new(), Vec::new());
            read_airports(data, &mut a);
            read_navaids(data, &mut n);
            read_fixes(data, &mut f);
            (a, n, f)
        })
        .reduce(
            || (Vec::new(), Vec::new(), Vec::new()),
            |mut acc, part| {
                acc.0.extend(part.0);
                acc.1.extend(part.1);
                acc.2.extend(part.2);
                acc
            },
        );
    say(2, "airports", format!("{}", airports.len()), t);
    say(3, "beacons", format!("{} VOR and NDB", navaids.len()), t);
    say(4, "fixes", format!("{} before merging", fixes.len()), t);

    let t = std::time::Instant::now();
    // The fixes are already in hand from the simulator's own waypoint records, so the
    // per-airport database lookups a chart needs are not paid for here.
    procedures::set_database_enrichment(false);
    let procedures = read_procedures(&blobs);
    procedures::set_database_enrichment(true);
    drop(blobs);
    let legs: usize = procedures.iter().map(|p| p.legs.len()).sum();
    say(5, "procedures", format!("{} with {legs} legs", procedures.len()), t);

    // A fix can be listed in more than one file, once for each tile its neighbourhood spans,
    // so the airways on every copy are gathered onto one.
    let mut by_key: HashMap<(String, String), Fix> = HashMap::new();
    for f in fixes {
        let key = (f.ident.clone(), f.region.clone());
        match by_key.get_mut(&key) {
            Some(kept) => kept.routes.extend(f.routes),
            None => {
                by_key.insert(key, f);
            }
        }
    }
    let fixes: Vec<Fix> = by_key.into_values().collect();
    say(6, "fixes merged", format!("{} distinct", fixes.len()), t0);

    let waypoints: Vec<WaypointRec> = fixes
        .iter()
        .map(|f| WaypointRec { ident: f.ident.clone(), region_code: f.region.clone(), area_code: area_of(&f.region), name: None, lat: f.lat, lon: f.lon, ..Default::default() })
        .collect();
    let t = std::time::Instant::now();
    let airways = assemble_airways(&fixes);
    let airway_legs: usize = airways.iter().map(|a| a.legs.len()).sum();
    let with_alt = airways.iter().flat_map(|a| &a.legs).filter(|l| l.min_ft.is_some()).count();
    say(7, "airways", format!("{} with {airway_legs} legs, {with_alt} with a minimum altitude", airways.len()), t);

    Ok(NavSet { cycle: cycle.to_string(), airports, runways: Vec::new(), navaids, ils: Vec::new(), waypoints, airways, procedures, mora: Vec::new() })
}

/// ARINC's three-letter area code, from the two-letter region an ident sits in. Only the
/// division the format actually uses is reproduced: everything outside the Americas and the
/// Pacific is `EUR` in this scheme's own loose sense, which is what the installed databases
/// themselves do with it.
fn area_of(region: &str) -> String {
    match region.chars().next().unwrap_or(' ') {
        'K' | 'C' | 'M' | 'P' | 'T' => "USA".to_string(),
        'S' => "SAM".to_string(),
        'N' | 'A' | 'Y' => "PAC".to_string(),
        'R' | 'V' | 'W' | 'Z' => "EEU".to_string(),
        'D' | 'F' | 'G' | 'H' => "AFR".to_string(),
        'O' | 'U' => "MES".to_string(),
        _ => "EUR".to_string(),
    }
}

/// The airport record's id. FS2024 renumbered every record type in the navigation data —
/// the airport is `0x113` where FS2020's is `0x56`, the VOR `0x105` against `0x13`, the NDB
/// `0x106` against `0x17`, the waypoint `0x108` against `0x22` — while keeping the sections
/// they live in. Where only the id changed, both are accepted. Where the record's own layout
/// changed as well, as the airport's has, that is said rather than guessed at: see this
/// module's doc comment.
const REC_AIRPORT_2024: u16 = 0x113;
/// FS2024 keeps an airport's identifier as four plain characters here, where FS2020 packs it
/// at `+0x28`.
const IDENT_TEXT_2024: usize = 0x6F;

fn read_airports(d: &[u8], out: &mut Vec<AirportRec>) {
    for rec in bgl::section_records(d, bgl::SECTION_AIRPORT) {
        if (rec.id != bgl::REC_AIRPORT && rec.id != REC_AIRPORT_2024) || rec.end - rec.start < 0x80 {
            continue;
        }
        let icao = if rec.id == REC_AIRPORT_2024 {
            let at = rec.start + IDENT_TEXT_2024;
            d[at..rec.end.min(at + 5)].iter().take_while(|&&b| b.is_ascii_alphanumeric()).map(|&b| b.to_ascii_uppercase() as char).collect()
        } else {
            bgl::ident(bgl::u32le(d, rec.start + 0x28))
        };
        if icao.len() < 3 {
            continue;
        }
        let lat = bgl::lat(bgl::u32le(d, rec.start + 0x10));
        let lon = bgl::lon(bgl::u32le(d, rec.start + 0x0c));
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            continue;
        }
        // The elevation is millimetres; see this module's own doc comment for how that was
        // established rather than guessed.
        let elevation_ft = bgl::u32le(d, rec.start + 0x14) as f64 / 1000.0 * FT_PER_M;
        let children_at = if rec.id == REC_AIRPORT_2024 { rec.start + 0x5C } else { rec.start + 0x44 };
        let name = child_text(d, children_at, rec.end, 0x19).unwrap_or_default();
        let region = if rec.id == REC_AIRPORT_2024 { icao.chars().take(2).collect() } else { bgl::ident(bgl::u32le(d, rec.start + 0x24)) };
        out.push(AirportRec {
            icao,
            name,
            icao_code: region.chars().take(2).collect(),
            area_code: area_of(&region),
            lat,
            lon,
            elevation_ft: elevation_ft.round(),
            // The simulator's navigation data carries no transition altitude or level and no
            // aerodrome speed limit; an aircraft reading a null here falls back to its own
            // default, which is better than a figure this invented.
            transition_altitude_ft: None,
            transition_level_ft: None,
            speed_limit_kt: None,
            speed_limit_altitude_ft: None,
            ..Default::default()
        });
    }
}

/// A child record's text payload, for the name records that carry one.
fn child_text(d: &[u8], from: usize, to: usize, id: u16) -> Option<String> {
    for c in bgl::records(d, from, to) {
        if c.id != id || c.end - c.start <= 8 {
            continue;
        }
        let text: String = d[c.start + 8..c.end].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
        let text = text.trim().to_string();
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

fn read_navaids(d: &[u8], out: &mut Vec<NavaidRec>) {
    for (section, vor) in [(0x13u32, true), (0x17, false)] {
        for rec in bgl::section_records(d, section) {
            if rec.end - rec.start < 0x24 {
                continue;
            }
            let ident = bgl::ident(bgl::u32le(d, rec.start + 0x20));
            if ident.is_empty() {
                continue;
            }
            let (lon_at, lat_at, freq_at) = if vor { (0x08, 0x0c, 0x14) } else { (0x0c, 0x10, 0x08) };
            let lat = bgl::lat(bgl::u32le(d, rec.start + lat_at));
            let lon = bgl::lon(bgl::u32le(d, rec.start + lon_at));
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                continue;
            }
            let raw = bgl::u32le(d, rec.start + freq_at) as f64;
            let frequency = if vor { raw / 1.0e6 } else { raw / 1000.0 };
            let region = bgl::ident(bgl::u32le(d, rec.start + 0x1c));
            out.push(NavaidRec {
                ident,
                // The record says whether a beacon is a VOR or an NDB but not whether a DME
                // is collocated with it, so the plainer of the two is written: an aircraft
                // that finds a VOR where a VOR/DME stands loses a distance readout, where the
                // other way round would have it display a distance that does not exist.
                kind: if vor { NavaidKind::Vor } else { NavaidKind::Ndb },
                name: String::new(),
                region_code: region.chars().take(2).collect(),
                area_code: area_of(&region),
                frequency,
                lat,
                lon,
                elevation_ft: 0.0,
                magnetic_variation_deg: 0.0,
                range_nm: None,
                ..Default::default()
            });
        }
    }
}

fn read_fixes(d: &[u8], out: &mut Vec<Fix>) {
    for rec in bgl::section_records(d, 0x22) {
        if rec.end - rec.start < WAYPOINT_HEADER {
            continue;
        }
        let r = &d[rec.start..rec.end];
        let ident = bgl::ident(bgl::u32le(r, 0x14));
        if ident.is_empty() {
            continue;
        }
        let lat = bgl::lat(bgl::u32le(r, 0x0c));
        let lon = bgl::lon(bgl::u32le(r, 0x08));
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            continue;
        }
        let region = bgl::ident(bgl::u32le(r, 0x18));
        out.push(Fix { ident, region: region.chars().take(2).collect(), lat, lon, routes: route_entries(r) });
    }
}

/// Turn the per-fix airway entries into whole airways.
///
/// Each entry says which fix comes before and which after on its own airway, so the airway is
/// the chain those links make. A chain is walked from a fix that has no predecessor; where an
/// airway's data is incomplete enough that no such fix exists — a link naming a fix that is
/// not itself in the data, which happens at the edge of what a tile covers — the remaining
/// links are still emitted as their own short chains rather than dropped, since a route search
/// can use a stretch of an airway it cannot see the whole of.
fn assemble_airways(fixes: &[Fix]) -> Vec<AirwayRec> {
    // Every (airway, fix) link, by airway. The region of a fix is looked up in a map rather
    // than searched for in the link list: a linear search there is what turned a long airway
    // into quadratic work, and there are airways with thousands of fixes on them.
    let mut region_of: HashMap<&str, &str> = HashMap::new();
    let mut by_airway: HashMap<&str, Vec<(&Fix, &RouteEntry)>> = HashMap::new();
    for f in fixes {
        region_of.insert(f.ident.as_str(), f.region.as_str());
        for e in &f.routes {
            by_airway.entry(e.name.as_str()).or_default().push((f, e));
        }
    }
    let mut out = Vec::new();
    for (name, links) in by_airway {
        // Which fix follows which, and the altitude and level of the leg between.
        let mut forward: HashMap<&str, (&str, Option<f64>, AirwayLevel, &str)> = HashMap::new();
        let mut has_predecessor: HashMap<&str, bool> = HashMap::new();
        for (f, e) in &links {
            has_predecessor.entry(f.ident.as_str()).or_insert(false);
            if let Some(next) = &e.next {
                forward.insert(f.ident.as_str(), (next.as_str(), e.min_ft, e.level, f.region.as_str()));
                has_predecessor.insert(next.as_str(), true);
            }
        }
        // A set, not a list: walking a chain and asking "have I been here" of a growing
        // vector is the other half of the same quadratic cost.
        let mut used: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut legs: Vec<AirwayLegRec> = Vec::new();
        // A fix nothing leads to starts a chain. Where the data is circular or its ends lie
        // outside what was read, no such fix exists, so every fix is tried in turn and the
        // `used` set keeps a chain from being emitted twice.
        let mut starts: Vec<&str> = has_predecessor.iter().filter(|(_, &p)| !p).map(|(i, _)| *i).collect();
        starts.sort_unstable();
        let mut rest: Vec<&str> = has_predecessor.keys().copied().collect();
        rest.sort_unstable();
        for start in starts.into_iter().chain(rest) {
            if used.contains(start) {
                continue;
            }
            let mut at = start;
            while let Some(&(next, min_ft, level, region)) = forward.get(at) {
                if !used.insert(at) {
                    break;
                }
                legs.push(AirwayLegRec {
                    sequence: legs.len() as u32 + 1,
                    from_ident: at.to_string(),
                    from_region: region.to_string(),
                    to_ident: next.to_string(),
                    to_region: region_of.get(next).copied().unwrap_or_default().to_string(),
                    level,
                    is_start: at == start,
                    is_end: !forward.contains_key(next),
                    min_ft,
                });
                at = next;
            }
        }
        if !legs.is_empty() {
            out.push(AirwayRec { ident: name.to_string(), legs });
        }
    }
    out.sort_by(|a, b| a.ident.cmp(&b.ident));
    out
}

/// Every airport's procedures, out of the bytes already read, in parallel.
fn read_procedures(blobs: &[Vec<u8>]) -> Vec<ProcedureRec> {
    use rayon::prelude::*;
    blobs
        .par_iter()
        .flat_map_iter(|data| {
            procedures::from_bytes(data, "").into_iter().flat_map(|airport| {
                let icao = airport.icao.clone();
                airport
                    .procedures
                    .into_iter()
                    .flat_map(move |p| {
                        let kind = match p.kind {
                            procedures::Kind::Sid => ProcKind::Sid,
                            procedures::Kind::Star => ProcKind::Star,
                            procedures::Kind::Approach => ProcKind::Approach,
                        };
                        let (icao, name, runway) = (icao.clone(), p.name.clone(), p.runway.clone());
                        p.transitions.into_iter().filter_map(move |t| {
                            let legs: Vec<LegRec> = t.legs.iter().enumerate().map(|(i, l)| leg_of(i, l)).collect();
                            (!legs.is_empty()).then(|| ProcedureRec {
                                airport_icao: icao.clone(),
                                kind,
                                ident: name.clone(),
                                transition_ident: (!t.name.is_empty()).then(|| t.name.clone()),
                                runway_ident: (!runway.is_empty()).then(|| runway.clone()),
                                legs,
                                ..Default::default()
                            })
                        })
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

fn leg_of(i: usize, l: &procedures::Leg) -> LegRec {
    LegRec {
        sequence: i as u32 + 1,
        path_terminator: path_of(&l.path),
        fix_ident: (!l.fix.is_empty()).then(|| l.fix.clone()),
        fix_region: None,
        recommended_navaid: (!l.navaid.is_empty()).then(|| l.navaid.clone()),
        recommended_navaid_region: None,
        theta_deg: l.theta_deg,
        rho_nm: l.rho_nm,
        course_deg: l.course_deg,
        // The simulator holds a leg's own length in metres; a database wants nautical miles.
        leg_length: l.distance_m.map(|m| m / 1852.0),
        turn: match l.turn {
            Some(procedures::Turn::Left) => TurnDirection::Left,
            Some(procedures::Turn::Right) => TurnDirection::Right,
            None => TurnDirection::Either,
        },
        altitude_rule: rule_of(l.altitude_rule),
        altitude1_ft: l.altitude_ft,
        altitude2_ft: l.altitude2_ft,
        speed_limit_kt: l.speed_kt,
        is_iaf: l.role == Some(procedures::FixRole::Initial),
        is_if: l.role == Some(procedures::FixRole::Intermediate),
        is_faf: l.role == Some(procedures::FixRole::Final),
        is_map: l.role == Some(procedures::FixRole::MissedApproachPoint),
        ..Default::default()
    }
}

fn path_of(code: &str) -> PathTerminator {
    match code.to_uppercase().as_str() {
        "IF" => PathTerminator::If,
        "TF" => PathTerminator::Tf,
        "CF" => PathTerminator::Cf,
        "DF" => PathTerminator::Df,
        "FA" => PathTerminator::Fa,
        "FC" => PathTerminator::Fc,
        "FD" => PathTerminator::Fd,
        "FM" => PathTerminator::Fm,
        "CA" => PathTerminator::Ca,
        "CD" => PathTerminator::Cd,
        "CI" => PathTerminator::Ci,
        "CR" => PathTerminator::Cr,
        "RF" => PathTerminator::Rf,
        "AF" => PathTerminator::Af,
        "VA" => PathTerminator::Va,
        "VD" => PathTerminator::Vd,
        "VI" => PathTerminator::Vi,
        "VM" => PathTerminator::Vm,
        "VR" => PathTerminator::Vr,
        "PI" => PathTerminator::Pi,
        "HA" => PathTerminator::Ha,
        "HF" => PathTerminator::Hf,
        "HM" => PathTerminator::Hm,
        _ => PathTerminator::Tf,
    }
}

fn rule_of(r: procedures::AltitudeRule) -> Option<AltitudeRule> {
    use procedures::AltitudeRule as R;
    match r {
        R::At => Some(AltitudeRule::At),
        R::AtOrAbove => Some(AltitudeRule::AtOrAbove),
        R::AtOrBelow => Some(AltitudeRule::AtOrBelow),
        R::Between => Some(AltitudeRule::Between),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A waypoint record built by hand in the shape the files use, with two airways through
    /// it, so the entry layout is pinned by a test rather than only by the dump it came from.
    fn waypoint_with_two_airways() -> Vec<u8> {
        let mut d = vec![0u8; WAYPOINT_HEADER + ENTRY_LEN * 2];
        d[0] = 0x22;
        d[6] = 3;
        d[7] = 2;
        // 1371.6 metres is four thousand five hundred feet.
        let put_f32 = |d: &mut Vec<u8>, at: usize, v: f32| d[at..at + 4].copy_from_slice(&v.to_le_bytes());
        for (k, (name, min_m)) in [("W347", 1371.6f32), ("Q67", 0.0)].into_iter().enumerate() {
            let base = WAYPOINT_HEADER + k * ENTRY_LEN;
            d[base] = 3;
            d[base + 1..base + 1 + name.len()].copy_from_slice(name.as_bytes());
            put_f32(&mut d, base + 0x1d, min_m);
        }
        d
    }

    #[test]
    fn an_airway_entry_is_thirty_three_bytes_and_its_altitude_is_metres() {
        let d = waypoint_with_two_airways();
        let entries = route_entries(&d);
        assert_eq!(entries.len(), 2, "one entry per airway");
        assert_eq!(entries[0].name, "W347");
        assert_eq!(entries[0].min_ft, Some(4500.0), "1371.6 m is four thousand five hundred feet");
        assert_eq!(entries[1].name, "Q67");
        assert_eq!(entries[1].min_ft, None, "an unset altitude is not a zero-foot minimum");
    }

    #[test]
    fn a_waypoint_with_no_airways_yields_none() {
        let mut d = vec![0u8; WAYPOINT_HEADER];
        d[0] = 0x22;
        assert!(route_entries(&d).is_empty());
    }

    /// Three fixes linked A to B to C on one airway come out as one airway of two legs, in
    /// order, with the first marked as the start and the last as the end.
    #[test]
    fn linked_fixes_become_one_airway_in_order() {
        let fix = |ident: &str, prev: Option<&str>, next: Option<&str>, min: Option<f64>| Fix {
            ident: ident.to_string(),
            region: "EG".to_string(),
            lat: 51.0,
            lon: 0.0,
            routes: vec![RouteEntry { name: "L620".to_string(), prev: prev.map(str::to_string), next: next.map(str::to_string), min_ft: min, level: AirwayLevel::Both }],
        };
        let fixes = vec![fix("A", None, Some("B"), Some(4500.0)), fix("B", Some("A"), Some("C"), Some(5500.0)), fix("C", Some("B"), None, None)];
        let airways = assemble_airways(&fixes);
        assert_eq!(airways.len(), 1);
        let a = &airways[0];
        assert_eq!(a.ident, "L620");
        assert_eq!(a.legs.len(), 2, "three fixes are two legs");
        assert_eq!((a.legs[0].from_ident.as_str(), a.legs[0].to_ident.as_str()), ("A", "B"));
        assert_eq!((a.legs[1].from_ident.as_str(), a.legs[1].to_ident.as_str()), ("B", "C"));
        assert!(a.legs[0].is_start && !a.legs[0].is_end);
        assert!(a.legs[1].is_end);
        assert_eq!(a.legs[0].min_ft, Some(4500.0));
    }

    #[test]
    fn an_area_code_follows_the_region() {
        assert_eq!(area_of("KJ"), "USA");
        assert_eq!(area_of("EG"), "EUR");
        assert_eq!(area_of("YM"), "PAC");
    }

    /// The whole of the simulator's navigation data, read for real. Ignored by default: it
    /// walks several thousand files and needs a simulator installed.
    #[test]
    #[ignore]
    fn reads_the_real_navigation_data() {
        let nav = read("2603").expect("read");
        println!("airports={} navaids={} waypoints={} airways={} procedures={}", nav.airports.len(), nav.navaids.len(), nav.waypoints.len(), nav.airways.len(), nav.procedures.len());
        let legs: usize = nav.airways.iter().map(|a| a.legs.len()).sum();
        let with_alt = nav.airways.iter().flat_map(|a| &a.legs).filter(|l| l.min_ft.is_some()).count();
        println!("airway legs={legs}, of which {with_alt} carry a minimum altitude");
        for a in nav.airways.iter().take(4) {
            println!("  {} : {} legs, first {} -> {} min {:?}", a.ident, a.legs.len(), a.legs[0].from_ident, a.legs[0].to_ident, a.legs[0].min_ft);
        }
        assert!(nav.airports.len() > 1000, "a worldwide dataset");
        assert!(!nav.airways.is_empty(), "airways were found");
    }
}
