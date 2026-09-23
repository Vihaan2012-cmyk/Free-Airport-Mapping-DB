//! Which published SID or STAR fits a flight, out of however many an airport publishes.
//!
//! A departure runway usually has more than one SID off it — one climbing out over the
//! sea, one turning inland — and an ARINC 424 procedure names the runway it starts from
//! (its "runway transition"), a common portion every aircraft on it flies regardless of
//! where it is bound, and, at the far end, a choice of enroute transitions that each set
//! the flight on a different departure corridor. A STAR is the same shape read the other
//! way: an enroute transition for wherever the flight is arriving from, a common portion,
//! and a runway transition at the near end.
//!
//! None of this is a single foreign key an SQL query can follow — a procedure's shape is
//! recovered from how ARINC 424 spells the transition column: blank (or "ALL") for the
//! common portion, "RWxx" for a leg belonging to one runway (or a pair of parallel ones,
//! suffixed "B"), anything else a named enroute transition. Grouping by that convention,
//! rather than by a `route_type` code whose exact meaning has drifted between vendors and
//! cycles, is what every practical ARINC 424 reader does, this one included.

use crate::dispatch::{bearing_deg, distance_nm, LatLon, PointKind, Waypoint};
use crate::route::Graph;
use crate::sources::navdata::{procedure_legs, ProcedureKind, ProcedureLeg};
use std::collections::BTreeMap;

/// A procedure ready to fly: its published name, the enroute transition chosen (if it has
/// more than one and one had to be), and every point of it in order.
#[derive(Debug, Clone)]
pub struct Procedure {
    pub ident: String,
    pub transition: Option<String>,
    pub points: Vec<Waypoint>,
}

/// The SID published at an airport that starts from a runway and, where it offers more
/// than one way on to the enroute network, whose last fix best points at the destination:
/// the transition (or the lack of one) that leaves the flight already headed roughly the
/// right way, needing the least turn once it reaches the airways, with a procedure whose
/// last fix the airway network also knows by that name winning close calls, since the
/// route can then join it by identity rather than by a direct leg.
pub fn sid_for_runway(icao: &str, runway: &str, destination: LatLon, graph: &Graph) -> Option<Procedure> {
    best_sid(&procedure_legs(icao, ProcedureKind::Sid), runway, destination, &|id, pos| graph.find(id, pos).is_some())
}

/// The STAR published at an airport that ends on a runway and, where more than one
/// enroute transition joins it, whose entry fix is best placed coming from the origin, the
/// same way [`sid_for_runway`] prefers one the network already knows by name.
pub fn star_for_runway(icao: &str, runway: &str, origin: LatLon, graph: &Graph) -> Option<Procedure> {
    best_star(&procedure_legs(icao, ProcedureKind::Star), runway, origin, &|id, pos| graph.find(id, pos).is_some())
}

/// Worth this many degrees of a worse-aimed turn to a selection score: joining the network
/// by identity, rather than by a direct leg [`crate::route::join_candidates`] must then go
/// looking for, is a real saving, but not one so large that a procedure aimed wildly the
/// wrong way should ever win against one aimed roughly right.
const NETWORK_JOIN_BONUS_DEG: f64 = 90.0;

/// Whether a transition column names a leg specific to a runway, and if so whether it is
/// this one: "RW27" serves every parallel runway 27, "RW27B" serves both of a pair,
/// "RW27L" only the left. The runway is compared by its number rather than its digits as
/// written, so a caller's "9L" still matches a table's two-digit "RW09L".
fn runway_matches(transition: &str, runway: &str) -> bool {
    let Some(rest) = transition.strip_prefix("RW") else { return false };
    let digits = |s: &str| -> Option<u32> {
        let d: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
        (!d.is_empty()).then(|| d.parse().ok()).flatten()
    };
    match (digits(runway), digits(rest)) {
        (Some(a), Some(b)) if a == b => {}
        _ => return false,
    }
    let rw_side = runway.chars().find(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_uppercase());
    match rest.chars().find(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_uppercase()) {
        None => true,
        Some('B') => true,
        side => side == rw_side,
    }
}

fn is_runway_leg(transition: &str) -> bool {
    transition.starts_with("RW")
}

fn is_common(transition: &str) -> bool {
    let t = transition.trim();
    t.is_empty() || t.eq_ignore_ascii_case("ALL")
}

/// The angle from one bearing to another, signed, in (-180°, 180°]: how much of a turn it
/// is from the first to the second.
fn turn(from_deg: f64, to_deg: f64) -> f64 {
    let d = (to_deg - from_deg + 540.0) % 360.0 - 180.0;
    d
}

fn bearing_of(a: &ProcedureLeg, b: &ProcedureLeg) -> f64 {
    bearing_deg((a.lat, a.lon), (b.lat, b.lon))
}

/// An altitude description code turned into the `alt_min_ft`/`alt_max_ft` a `Waypoint`
/// carries: '+' at or above, '-' at or below, 'B' between the two figures, and blank (or
/// anything not recognised) exactly at the one figure given.
fn constraint(desc: &str, alt1: Option<f64>, alt2: Option<f64>) -> (Option<f64>, Option<f64>) {
    match desc.trim().to_uppercase().as_str() {
        "+" => (alt1, None),
        "-" => (None, alt1),
        "B" => match (alt1, alt2) {
            (Some(a), Some(b)) => (Some(a.min(b)), Some(a.max(b))),
            _ => (alt1, alt2),
        },
        _ => (alt1, alt1),
    }
}

fn to_waypoint(leg: &ProcedureLeg, ident: &str, kind: PointKind) -> Waypoint {
    let (min, max) = constraint(&leg.altitude_description, leg.altitude1_ft, leg.altitude2_ft);
    let mut w = Waypoint::new(leg.fix.clone(), (leg.lat, leg.lon), ident, kind);
    w.alt_min_ft = min;
    w.alt_max_ft = max;
    w.speed_max_kt = leg.speed_max_kt;
    w
}

fn group_by_procedure(legs: &[ProcedureLeg]) -> BTreeMap<&str, Vec<&ProcedureLeg>> {
    let mut out: BTreeMap<&str, Vec<&ProcedureLeg>> = BTreeMap::new();
    for l in legs {
        out.entry(l.procedure.as_str()).or_default().push(l);
    }
    out
}

fn sequence<'a>(parts: impl IntoIterator<Item = &'a ProcedureLeg>) -> Vec<&'a ProcedureLeg> {
    let mut seq: Vec<&ProcedureLeg> = parts.into_iter().collect();
    seq.sort_by_key(|l| l.seqno);
    seq.dedup_by(|a, b| a.fix == b.fix && a.lat == b.lat && a.lon == b.lon);
    seq
}

/// The pure selection logic, worked entirely from legs already fetched: kept apart from
/// [`sid_for_runway`] so it can be tested against fixtures rather than a real database.
/// `in_network` answers whether a fix, by its published name and position, is one the
/// airway graph already has a node for — a real database in the caller, a fixture's own
/// stand-in in a test.
pub fn best_sid(legs: &[ProcedureLeg], runway: &str, destination: LatLon, in_network: &dyn Fn(&str, LatLon) -> bool) -> Option<Procedure> {
    let mut best: Option<(f64, Procedure)> = None;
    for (name, all) in group_by_procedure(legs) {
        let runway_legs: Vec<&ProcedureLeg> = all.iter().filter(|l| runway_matches(&l.transition, runway)).copied().collect();
        let has_any_runway_leg = all.iter().any(|l| is_runway_leg(&l.transition));
        if has_any_runway_leg && runway_legs.is_empty() {
            continue; // This SID starts from a different runway.
        }
        let common: Vec<&ProcedureLeg> = all.iter().filter(|l| is_common(&l.transition)).copied().collect();
        let mut transitions: BTreeMap<&str, Vec<&ProcedureLeg>> = BTreeMap::new();
        for l in &all {
            if !is_runway_leg(&l.transition) && !is_common(&l.transition) {
                transitions.entry(l.transition.as_str()).or_default().push(l);
            }
        }
        let mut variants: Vec<Option<&str>> = vec![None];
        variants.extend(transitions.keys().copied().map(Some));
        for variant in variants {
            let mut seq = sequence(runway_legs.iter().copied().chain(common.iter().copied()));
            if let Some(t) = variant {
                seq.extend(sequence(transitions[t].iter().copied()));
            }
            let Some(&last) = seq.last() else { continue };
            let heading_after = if seq.len() >= 2 { bearing_of(seq[seq.len() - 2], last) } else { bearing_deg((last.lat, last.lon), destination) };
            let heading_needed = bearing_deg((last.lat, last.lon), destination);
            let mut score = turn(heading_after, heading_needed).abs() + distance_nm((last.lat, last.lon), destination) * 0.002;
            if in_network(&last.fix, (last.lat, last.lon)) {
                score -= NETWORK_JOIN_BONUS_DEG;
            }
            if best.as_ref().is_none_or(|(b, _)| score < *b) {
                let points = seq.iter().map(|l| to_waypoint(l, name, PointKind::Sid)).collect();
                best = Some((score, Procedure { ident: name.to_string(), transition: variant.map(str::to_string), points }));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The pure selection logic for a STAR, mirroring [`best_sid`]: the enroute transition
/// comes first here, the runway-specific legs last. `in_network` is the same test of
/// whether the *entry* fix — the first point flown, where a STAR joins the airways — is one
/// the graph already has a node for.
pub fn best_star(legs: &[ProcedureLeg], runway: &str, origin: LatLon, in_network: &dyn Fn(&str, LatLon) -> bool) -> Option<Procedure> {
    let mut best: Option<(f64, Procedure)> = None;
    for (name, all) in group_by_procedure(legs) {
        let runway_legs: Vec<&ProcedureLeg> = all.iter().filter(|l| runway_matches(&l.transition, runway)).copied().collect();
        let has_any_runway_leg = all.iter().any(|l| is_runway_leg(&l.transition));
        if has_any_runway_leg && runway_legs.is_empty() {
            continue;
        }
        let common: Vec<&ProcedureLeg> = all.iter().filter(|l| is_common(&l.transition)).copied().collect();
        let mut transitions: BTreeMap<&str, Vec<&ProcedureLeg>> = BTreeMap::new();
        for l in &all {
            if !is_runway_leg(&l.transition) && !is_common(&l.transition) {
                transitions.entry(l.transition.as_str()).or_default().push(l);
            }
        }
        let mut variants: Vec<Option<&str>> = vec![None];
        variants.extend(transitions.keys().copied().map(Some));
        for variant in variants {
            let mut seq = Vec::new();
            if let Some(t) = variant {
                seq.extend(sequence(transitions[t].iter().copied()));
            }
            seq.extend(sequence(common.iter().copied().chain(runway_legs.iter().copied())));
            let Some(&first) = seq.first() else { continue };
            let heading_in = bearing_deg(origin, (first.lat, first.lon));
            let heading_of_star = if seq.len() >= 2 { bearing_of(first, seq[1]) } else { bearing_deg(origin, (first.lat, first.lon)) };
            let mut score = turn(heading_in, heading_of_star).abs() + distance_nm(origin, (first.lat, first.lon)) * 0.0005;
            if in_network(&first.fix, (first.lat, first.lon)) {
                score -= NETWORK_JOIN_BONUS_DEG;
            }
            if best.as_ref().is_none_or(|(b, _)| score < *b) {
                let points = seq.iter().map(|l| to_waypoint(l, name, PointKind::Star)).collect();
                best = Some((score, Procedure { ident: name.to_string(), transition: variant.map(str::to_string), points }));
            }
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg(proc: &str, transition: &str, seq: i64, fix: &str, lat: f64, lon: f64) -> ProcedureLeg {
        ProcedureLeg { procedure: proc.into(), transition: transition.into(), seqno: seq, fix: fix.into(), lat, lon, altitude_description: String::new(), altitude1_ft: None, altitude2_ft: None, speed_max_kt: None }
    }

    /// What every test that has no graph of its own to ask uses: nothing is ever in the
    /// network, so the network-join bonus never enters into it and the heading-only tests
    /// below keep testing only what they say they test.
    fn no_network(_id: &str, _pos: LatLon) -> bool {
        false
    }

    #[test]
    fn runway_matching_understands_shared_and_specific_transitions() {
        assert!(runway_matches("RW27", "27L"));
        assert!(runway_matches("RW27B", "27R"));
        assert!(runway_matches("RW27L", "27L"));
        assert!(!runway_matches("RW27L", "27R"));
        assert!(!runway_matches("RW09", "27L"));
        assert!(!runway_matches("KONAN", "27L"));
    }

    /// The runway ident a caller passes in may or may not be two digits wide — the CLI, a
    /// wind-based choice and a table read each write it slightly differently — and none of
    /// that should matter to whether it is the runway a transition was published for.
    #[test]
    fn runway_matching_does_not_care_whether_the_leading_zero_is_there() {
        assert!(runway_matches("RW09L", "9L"));
        assert!(runway_matches("RW09L", "09L"));
        assert!(runway_matches("RW04", "4"));
    }

    /// Two SIDs off the same runway, one climbing east and one climbing west: the one
    /// picked should be the one whose last fix already points roughly at the destination.
    #[test]
    fn the_sid_whose_last_fix_points_at_the_destination_is_chosen() {
        let legs = vec![
            leg("EAST1A", "RW09", 10, "RW09", 0.0, 0.0),
            leg("EAST1A", "", 20, "EAXIT", 0.0, 1.0),
            leg("WEST1A", "RW09", 10, "RW09", 0.0, 0.0),
            leg("WEST1A", "", 20, "WEXIT", 0.0, -1.0),
        ];
        let destination = (0.0, 5.0);
        let sid = best_sid(&legs, "09", destination, &no_network).expect("a SID");
        assert_eq!(sid.ident, "EAST1A");
        assert_eq!(sid.points.last().unwrap().ident, "EAXIT");
    }

    #[test]
    fn a_sid_for_a_different_runway_is_not_offered() {
        let legs = vec![leg("NORTH1A", "RW27", 10, "RW27", 0.0, 0.0), leg("NORTH1A", "", 20, "NEXIT", 0.0, 1.0)];
        assert!(best_sid(&legs, "09", (0.0, 5.0), &no_network).is_none());
    }

    #[test]
    fn a_sid_with_no_runway_legs_at_all_serves_every_runway() {
        let legs = vec![leg("ANY1A", "", 10, "START", 0.0, 0.0), leg("ANY1A", "", 20, "EXIT", 0.0, 1.0)];
        assert!(best_sid(&legs, "09", (0.0, 5.0), &no_network).is_some());
        assert!(best_sid(&legs, "27", (0.0, 5.0), &no_network).is_some());
    }

    #[test]
    fn the_sid_carries_altitude_constraints() {
        let mut l = leg("CON1A", "RW09", 10, "FIX1", 0.0, 0.5);
        l.altitude_description = "+".into();
        l.altitude1_ft = Some(4000.0);
        let sid = best_sid(std::slice::from_ref(&l), "09", (0.0, 5.0), &no_network).unwrap();
        assert_eq!(sid.points[0].alt_min_ft, Some(4000.0));
        assert_eq!(sid.points[0].alt_max_ft, None);
    }

    /// Two SIDs aimed close enough to the destination that heading alone barely tells them
    /// apart: the one whose last fix the airway network already has under that name should
    /// win, since the route can then join it by identity rather than a direct leg.
    #[test]
    fn a_sid_whose_last_fix_is_in_the_network_is_preferred_on_a_close_call() {
        let legs = vec![
            leg("KNOWN1A", "RW09", 10, "RW09", 0.0, 0.0),
            leg("KNOWN1A", "", 20, "KEXIT", 0.0, 1.0),
            leg("ORPHN1A", "RW09", 10, "RW09", 0.0, 0.0),
            leg("ORPHN1A", "", 20, "OEXIT", 0.02, 1.0),
        ];
        let destination = (0.0, 5.0);
        let in_network = |id: &str, _pos: LatLon| id == "KEXIT";
        let sid = best_sid(&legs, "09", destination, &in_network).expect("a SID");
        assert_eq!(sid.ident, "KNOWN1A");
    }

    /// The network-join bonus is a tie-breaker, not a trump card: a SID whose last fix is
    /// nowhere near the destination still loses to one that is, however well the loser's
    /// fix happens to be known to the network.
    #[test]
    fn the_network_join_bonus_never_beats_a_much_better_heading() {
        let legs = vec![
            leg("EAST1A", "RW09", 10, "RW09", 0.0, 0.0),
            leg("EAST1A", "", 20, "EAXIT", 0.0, 1.0),
            leg("WEST1A", "RW09", 10, "RW09", 0.0, 0.0),
            leg("WEST1A", "", 20, "WEXIT", 0.0, -1.0),
        ];
        let destination = (0.0, 5.0);
        let in_network = |id: &str, _pos: LatLon| id == "WEXIT";
        let sid = best_sid(&legs, "09", destination, &in_network).expect("a SID");
        assert_eq!(sid.ident, "EAST1A");
    }

    /// Two STARs onto the same runway, entered from opposite sides: the one entered from
    /// the direction the flight is actually coming from should be picked.
    #[test]
    fn the_star_whose_entry_fits_the_origin_is_chosen() {
        let legs = vec![
            leg("NORD1A", "NORTH", 10, "NENTRY", 1.0, 0.0),
            leg("NORD1A", "", 20, "COMMON", 0.5, 0.0),
            leg("NORD1A", "RW27", 30, "RW27", 0.0, 0.0),
            leg("SUD1A", "SOUTH", 10, "SENTRY", -1.0, 0.0),
            leg("SUD1A", "", 20, "COMMON", -0.5, 0.0),
            leg("SUD1A", "RW27", 30, "RW27", 0.0, 0.0),
        ];
        let origin = (5.0, 0.0);
        let star = best_star(&legs, "27", origin, &no_network).expect("a STAR");
        assert_eq!(star.ident, "NORD1A");
        assert_eq!(star.points.first().unwrap().ident, "NENTRY");
    }

    /// Real navigation data, read from whatever is installed on this machine — run by hand,
    /// since a build machine has no PMDG (or equivalent) database to check against. Each of
    /// these airports is known to publish both, so a `None` back means the reader is broken
    /// again, not that the airport happens to have nothing today.
    #[test]
    #[ignore]
    fn sids_and_stars_from_the_installed_database() {
        let graph = Graph::from_navdata();
        for (icao, runway) in [("EGLL", "27R"), ("LIRF", "16L"), ("KJFK", "04L"), ("VIDP", "11R")] {
            let legs = procedure_legs(icao, ProcedureKind::Sid);
            println!("{icao}: {} SID legs", legs.len());
            assert!(!legs.is_empty(), "{icao} should publish at least one SID");
            let sid = best_sid(&legs, runway, (0.0, 0.0), &|id, pos| graph.find(id, pos).is_some());
            println!("{icao}/{runway} SID: {sid:?}");
            assert!(sid.is_some(), "{icao}/{runway} should choose a SID");

            let legs = procedure_legs(icao, ProcedureKind::Star);
            println!("{icao}: {} STAR legs", legs.len());
            assert!(!legs.is_empty(), "{icao} should publish at least one STAR");
            let star = best_star(&legs, runway, (0.0, 0.0), &|id, pos| graph.find(id, pos).is_some());
            println!("{icao}/{runway} STAR: {star:?}");
            assert!(star.is_some(), "{icao}/{runway} should choose a STAR");
        }
    }
}
