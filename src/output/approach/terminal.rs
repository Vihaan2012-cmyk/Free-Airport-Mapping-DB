//! Departure and arrival charts: a SID or a STAR on one page, laid out the way an
//! airline's departure and arrival pages are.
//!
//! Top to bottom: the airport and the kind of page; a strip of the frequency the
//! procedure is flown on, the airport's elevation and the transition altitude; the
//! procedure's name in a box, with its coded name and runways; the map; and along the
//! foot the initial climb runway by runway and the routing of each transition.
//!
//! The map is drawn from everything on this machine that says anything about the ground
//! and the air over it: the terrain and the sea from the Copernicus elevation model, the
//! grid minimum off-route altitudes and the controlled airspace from an aircraft's
//! navigation database, the beacons from the same, and the procedure itself from the
//! simulator. The track is the one flown rather than a line joined through the fixes: a
//! climb on a heading goes out on that heading, and a turn goes the way the procedure
//! says, round the long way if that is the way it says.

use super::*;
use crate::sources::msfs::procedures::{AltitudeRule, Kind, Leg, Procedure};
use crate::sources::navdata::{AirportInfo, Airspace, Beacon, Communication};

/// The blue altitudes and the magenta speeds are printed in, and the red of airspace.
const BLUE: (f32, f32, f32) = (0.10, 0.22, 0.80);
const MAGENTA: (f32, f32, f32) = (0.66, 0.05, 0.38);
const AIRSPACE: (f32, f32, f32) = (0.66, 0.16, 0.14);
const INK_RGB: (f32, f32, f32) = (INK, INK, INK);

/// Everything a departure or arrival chart is drawn from.
pub struct Terminal<'a> {
    pub airport: &'a AirportProcedures,
    pub airport_name: Option<String>,
    pub airport_iata: Option<String>,
    pub airport_place: Option<String>,
    pub field_elev_ft: f64,
    pub procedure: &'a Procedure,
    pub navaids: Vec<Beacon>,
    /// Each runway the procedure serves: its name and both ends, the named end first.
    pub runways: Vec<(String, (f64, f64), (f64, f64))>,
    /// Every runway at the airport, for drawing the airport itself.
    pub all_runways: Vec<((f64, f64), (f64, f64))>,
    pub info: Option<AirportInfo>,
    pub comms: Vec<Communication>,
    pub airspace: Vec<Airspace>,
    pub mora: Vec<(f64, f64, f64)>,
    pub patch: Option<Patch>,
    /// The other airports on the map: the place, the airport's name, its code, where.
    pub nearby: Vec<(String, String, String, f64, f64)>,
}

/// A piece of the procedure as the page describes it: the runways or the transition it
/// belongs to, and its legs. Runway transitions that are flown the same way are one
/// route, headed with all of their runways, as a chart heads them.
struct Route<'a> {
    heading: String,
    /// The runways a runway route is for; the transition's name otherwise.
    names: Vec<String>,
    part: &'a str,
    legs: Vec<&'a Leg>,
}

fn is_sid(p: &Procedure) -> bool {
    p.kind == Kind::Sid
}

/// An area navigation procedure: every leg after the climb off the runway flies to a
/// named waypoint, not along a radial.
pub fn is_rnav(p: &Procedure) -> bool {
    let legs: Vec<&Leg> = p.transitions.iter().flat_map(|t| t.legs.iter()).collect();
    !legs.is_empty()
        && legs.iter().all(|l| matches!(l.path.as_str(), "IF" | "TF" | "DF" | "RF" | "VA" | "CA" | "FA" | "VM" | "FM" | "HM" | "HA" | "HF" | "VI" | "CI"))
        && legs.iter().any(|l| matches!(l.path.as_str(), "TF" | "RF" | "DF"))
        && !legs.iter().any(|l| l.theta_deg.is_some() && matches!(l.path.as_str(), "CF" | "CR" | "VR" | "AF"))
}

/// The pieces in the order they are flown: out from the runway on a departure, in to
/// it on an arrival.
fn routes(p: &Procedure) -> Vec<Route<'_>> {
    let mut runway: Vec<Route> = Vec::new();
    let mut common: Vec<Route> = Vec::new();
    let mut enroute: Vec<Route> = Vec::new();
    for t in &p.transitions {
        let legs: Vec<&Leg> = t.legs.iter().collect();
        match t.part.as_str() {
            "runway" => {
                let signature: Vec<(&str, &str)> = legs.iter().map(|l| (l.path.as_str(), l.fix.as_str())).collect();
                // The runway fix differs by runway and nothing else does.
                let same = runway.iter_mut().find(|r| {
                    let theirs: Vec<(&str, &str)> = r.legs.iter().map(|l| (l.path.as_str(), l.fix.as_str())).collect();
                    theirs.len() == signature.len() && theirs.iter().zip(&signature).all(|(a, b)| a.0 == b.0 && (a.1 == b.1 || (a.1.starts_with("RW") && b.1.starts_with("RW"))))
                });
                let name = t.name.trim_start_matches("RW").to_string();
                match same {
                    Some(r) => r.names.push(name),
                    None => runway.push(Route { heading: String::new(), names: vec![name], part: "runway", legs }),
                }
            }
            "common" => common.push(Route { heading: String::new(), names: Vec::new(), part: "common", legs }),
            _ => enroute.push(Route { heading: format!("{} TRANSITION", t.name), names: vec![t.name.clone()], part: "enroute", legs }),
        }
    }
    for r in &mut runway {
        r.heading = runway_list(&r.names);
    }
    if is_sid(p) {
        runway.into_iter().chain(common).chain(enroute).collect()
    } else {
        enroute.into_iter().chain(common).chain(runway).collect()
    }
}

/// Runways the way a chart lists them: "31L/R" for a pair, "4L/R, 13L" for more.
fn runway_list(names: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    let mut sorted: Vec<String> = names.to_vec();
    sorted.sort();
    sorted.dedup();
    while i < sorted.len() {
        let here = &sorted[i];
        let number: String = here.chars().take_while(|c| c.is_ascii_digit()).collect();
        let number = number.trim_start_matches('0').to_string();
        let mut sides: Vec<String> = vec![here.chars().skip_while(|c| c.is_ascii_digit()).collect()];
        let mut j = i + 1;
        while j < sorted.len() && sorted[j].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().trim_start_matches('0') == number {
            sides.push(sorted[j].chars().skip_while(|c| c.is_ascii_digit()).collect());
            j += 1;
        }
        out.push(format!("{number}{}", sides.join("/")));
        i = j;
    }
    out.join(", ")
}

/// What a chart calls the procedure: the fix it is named for written out in full, then
/// its number and letter. "DEGU1E" goes to DEGUN, so it is "DEGUN 1E".
pub fn title(p: &Procedure) -> String {
    let (fix, number) = named_fix(p);
    if number.is_empty() {
        fix
    } else {
        format!("{fix} {number}")
    }
}

/// The fix a procedure is named for, and its number and letter.
fn named_fix(p: &Procedure) -> (String, String) {
    let (stem, number) = match p.name.find(|c: char| c.is_ascii_digit()) {
        Some(at) => (&p.name[..at], &p.name[at..]),
        None => (p.name.as_str(), ""),
    };
    let fix = p
        .transitions
        .iter()
        .flat_map(|t| t.legs.iter())
        .map(|l| l.fix.as_str())
        .filter(|f| f.len() >= stem.len() && f.starts_with(stem) && f.chars().all(|c| c.is_ascii_alphabetic()))
        .min_by_key(|f| f.len())
        .unwrap_or(stem);
    (fix.to_string(), number.to_string())
}

/// The runways a procedure serves, by name.
pub fn runways_of(p: &Procedure) -> Vec<String> {
    let mut out: Vec<String> = p.transitions.iter().filter(|t| t.part == "runway").map(|t| t.name.trim_start_matches("RW").to_string()).collect();
    out.sort();
    out.dedup();
    out
}

/// An altitude the way a route description writes it.
fn altitude_phrase(leg: &Leg) -> Option<String> {
    let a = leg.altitude_ft?;
    Some(match leg.altitude_rule {
        AltitudeRule::AtOrAbove => format!("at or above {a:.0}"),
        AltitudeRule::AtOrBelow => format!("at or below {a:.0}"),
        AltitudeRule::Between => match leg.altitude2_ft {
            Some(b) => format!("between {:.0} and {:.0}", a.min(b), a.max(b)),
            None => format!("at {a:.0}"),
        },
        _ => format!("at {a:.0}"),
    })
}

fn turn_word(leg: &Leg) -> &'static str {
    match leg.turn {
        Some(Turn::Left) => "LEFT turn ",
        Some(Turn::Right) => "RIGHT turn ",
        None => "",
    }
}

/// A route in words, read off its legs, the way a chart's initial climb box writes it:
/// "Climb on heading 314° to 520, then LEFT turn direct to cross SKORR at or above 2500
/// and at or below 210 KT, then on track 225° to RNGRR."
fn route_text(legs: &[&Leg], departure: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (i, leg) in legs.iter().enumerate() {
        let alt = altitude_phrase(leg);
        let speed = leg.speed_kt.map(|s| format!("at or below {s:.0} KT"));
        let constraint = match (&alt, &speed) {
            (Some(a), Some(s)) => format!(" {a} and {s}"),
            (Some(a), None) => format!(" {a}"),
            (None, Some(s)) => format!(" {s}"),
            _ => String::new(),
        };
        let cross = if constraint.is_empty() { "" } else { "cross " };
        let course = leg.course_deg.map(|c| format!("{c:03.0}\u{b0}"));
        let piece = match leg.path.as_str() {
            // Where a transition starts: its limits belong to the route before it.
            "IF" if i == 0 => (!leg.fix.is_empty() && !leg.fix.starts_with("RW")).then(|| format!("from {}", leg.fix)),
            "CA" | "VA" | "FA" => {
                let how = if leg.path == "VA" { "heading" } else { "track" };
                let verb = if departure { "climb" } else { "continue" };
                match (course, leg.altitude_ft) {
                    (Some(c), Some(a)) => Some(format!("{}{verb} on {how} {c} to {a:.0}", turn_word(leg))),
                    (Some(c), None) => Some(format!("{}{how} {c}", turn_word(leg))),
                    (None, Some(a)) => Some(format!("{verb} to {a:.0}")),
                    _ => None,
                }
            }
            "VI" | "CI" | "VM" | "FM" | "VD" | "CD" | "VR" | "CR" => course.map(|c| {
                let how = if leg.path.starts_with('V') { "heading" } else { "track" };
                let mut s = format!("{}{how} {c}", turn_word(leg));
                if let Some(next) = legs.get(i + 1) {
                    if matches!(next.path.as_str(), "CF" | "TF") && !next.fix.is_empty() {
                        s.push_str(&format!(" to intercept course to {}", next.fix));
                    }
                }
                if leg.path.ends_with('M') {
                    s.push_str(", expect RADAR vectors");
                }
                s
            }),
            "DF" if !leg.fix.is_empty() => Some(format!("{}direct to {cross}{}{constraint}", turn_word(leg), leg.fix)),
            "TF" | "CF" | "IF" | "AF" | "RF" if !leg.fix.is_empty() && !leg.fix.starts_with("RW") => {
                let on = match (leg.path.as_str(), course) {
                    ("CF", Some(c)) => format!("on track {c} "),
                    _ => String::new(),
                };
                Some(format!("{}{on}to {cross}{}{constraint}", turn_word(leg), leg.fix))
            }
            "TF" | "CF" if leg.fix.starts_with("RW") => Some(format!("to RWY {}", leg.fix.trim_start_matches("RW"))),
            "HM" | "HA" | "HF" if !leg.fix.is_empty() => Some(format!("hold at {}", leg.fix)),
            _ => None,
        };
        if let Some(p) = piece {
            if parts.last() != Some(&p) {
                parts.push(p);
            }
        }
    }
    if parts.is_empty() {
        return "As published.".to_string();
    }
    let mut s = parts.join(", then ");
    s.get_mut(0..1).map(|c| c.make_ascii_uppercase());
    format!("{s}.")
}

/// Course (magnetic) and distance between two points.
fn course_and_nm(a: (f64, f64), b: (f64, f64), variation_deg: f64) -> (f64, f64) {
    let cos = ((a.0 + b.0) / 2.0).to_radians().cos().max(0.05);
    let (dn, de) = ((b.0 - a.0) * 60.0, (b.1 - a.1) * 60.0 * cos);
    let true_deg = (de.atan2(dn).to_degrees() + 360.0) % 360.0;
    let mag = ((true_deg - variation_deg) % 360.0 + 360.0) % 360.0;
    (if mag.round() == 0.0 { 360.0 } else { mag }, dn.hypot(de))
}

// ---------------------------------------------------------------------------------
// The path flown.
// ---------------------------------------------------------------------------------

/// A piece of the drawn track, and what it is: a leg to a fix, a heading, or vectors.
struct Piece {
    points: Vec<(f64, f64)>,
    /// A heading flown to an altitude: the heading and the altitude, for its label.
    heading: Option<(f64, Option<f64>)>,
    vectors: bool,
    /// The leg flown to reach the end of it, for the course and distance label.
    to_fix: bool,
}

fn unit(b: f64) -> (f64, f64) {
    (b.to_radians().sin(), b.to_radians().cos())
}

/// The turn from a point and a track round to a fix, the way the procedure says to turn,
/// at the radius of a turn at climb speed; then the line to the fix.
fn turn_to_fix(from: (f64, f64), track_true: f64, turn: Option<Turn>, fix: (f64, f64)) -> Vec<(f64, f64)> {
    const RADIUS_NM: f64 = 1.3;
    let local = Local::at(from);
    let (fx, fy) = local.to_nm(fix.0, fix.1);
    let bearing_to = |x: f64, y: f64| ((fx - x).atan2(fy - y).to_degrees() + 360.0) % 360.0;
    let diff = |a: f64, b: f64| ((b - a + 540.0) % 360.0) - 180.0;
    // Which way round: as the procedure says, or the shorter way where it says nothing.
    let left = match turn {
        Some(Turn::Left) => true,
        Some(Turn::Right) => false,
        None => diff(track_true, bearing_to(0.0, 0.0)) < 0.0,
    };
    if diff(track_true, bearing_to(0.0, 0.0)).abs() < 10.0 || fx.hypot(fy) < 0.3 {
        return vec![from, fix];
    }
    let side = if left { -90.0 } else { 90.0 };
    let (cx, cy) = {
        let (ux, uy) = unit(track_true + side);
        (ux * RADIUS_NM, uy * RADIUS_NM)
    };
    let mut out = vec![from];
    let mut h = track_true;
    for _ in 0..72 {
        h = (h + if left { -5.0 } else { 5.0 } + 360.0) % 360.0;
        let (ux, uy) = unit(h - side);
        let (px, py) = (cx + ux * RADIUS_NM, cy + uy * RADIUS_NM);
        out.push(local.to_ll(px, py));
        if diff(h, bearing_to(px, py)).abs() < 6.0 {
            break;
        }
    }
    out.push(fix);
    out
}

/// The path a route is flown along, in pieces, from a starting point and track where it
/// has one (the end of the runway, for a departure).
fn flown(legs: &[&Leg], start: Option<((f64, f64), f64)>, variation_deg: f64, field_ft: f64) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    let (mut at, mut track) = match start {
        Some((p, t)) => (Some(p), Some(t)),
        None => (None, None),
    };
    let to_true = |mag: f64| (mag + variation_deg + 360.0) % 360.0;
    for (i, leg) in legs.iter().enumerate() {
        let fix = leg.lat.zip(leg.lon).filter(|_| !leg.fix.is_empty());
        match leg.path.as_str() {
            "IF" => {
                if let Some(f) = fix {
                    at = Some(f);
                }
            }
            "VA" | "CA" | "FA" | "VI" | "CI" | "VM" | "FM" | "VD" | "CD" | "VR" | "CR" => {
                let (Some(from), Some(course)) = (at, leg.course_deg) else { continue };
                let heading = to_true(course);
                // How far a climb to the altitude takes, at a gradient an airliner flies;
                // an intercept or vectors are shown by a short line on the heading.
                let nm = match (leg.path.as_str(), leg.altitude_ft) {
                    ("VA" | "CA" | "FA", Some(a)) => ((a - field_ft) / 450.0).clamp(1.0, 6.0),
                    ("VM" | "FM", _) => 4.0,
                    _ => 2.0,
                };
                let mut points = match (track, leg.turn) {
                    (Some(t), Some(_)) if ((heading - t + 540.0) % 360.0 - 180.0).abs() > 15.0 => {
                        // Turn on to the heading the way the leg says, then fly it.
                        let local = Local::at(from);
                        let left = leg.turn == Some(Turn::Left);
                        let side = if left { -90.0 } else { 90.0 };
                        let (ux, uy) = unit(t + side);
                        let (cx, cy) = (ux * 1.3, uy * 1.3);
                        let mut pts = vec![from];
                        let mut h = t;
                        for _ in 0..72 {
                            if ((heading - h + 540.0) % 360.0 - 180.0).abs() < 5.0 {
                                break;
                            }
                            h = (h + if left { -5.0 } else { 5.0 } + 360.0) % 360.0;
                            let (vx, vy) = unit(h - side);
                            pts.push(local.to_ll(cx + vx * 1.3, cy + vy * 1.3));
                        }
                        pts
                    }
                    _ => vec![from],
                };
                let last = *points.last().unwrap_or(&from);
                let local = Local::at(last);
                let (ux, uy) = unit(heading);
                points.push(local.to_ll(ux * nm, uy * nm));
                at = points.last().copied();
                track = Some(heading);
                pieces.push(Piece {
                    points,
                    heading: matches!(leg.path.as_str(), "VA" | "CA" | "FA" | "VI" | "CI" | "VM" | "FM").then(|| (course, leg.altitude_ft)),
                    vectors: leg.path.ends_with('M'),
                    to_fix: false,
                });
            }
            "DF" => {
                let Some(f) = fix else { continue };
                let points = match (at, track) {
                    (Some(from), Some(t)) => turn_to_fix(from, t, leg.turn, f),
                    (Some(from), None) => vec![from, f],
                    _ => vec![f],
                };
                track = Some(bearing_between(points[points.len().saturating_sub(2)], f));
                at = Some(f);
                // A leg that is mostly a turn has no one course to label it with.
                let straight = points.len() <= 3;
                pieces.push(Piece { points, heading: None, vectors: false, to_fix: straight });
            }
            "TF" | "CF" | "RF" | "AF" => {
                let Some(f) = fix else { continue };
                let Some(from) = at else {
                    at = Some(f);
                    continue;
                };
                // Joining a track flown on a heading: the turn on to it the way the leg
                // says, like a direct.
                let points = match (track, leg.turn, i > 0 && pieces.last().is_some_and(|p| p.heading.is_some())) {
                    (Some(t), turn, true) => turn_to_fix(from, t, turn, f),
                    _ => vec![from, f],
                };
                track = Some(bearing_between(from, f));
                at = Some(f);
                let straight = points.len() <= 3;
                pieces.push(Piece { points, heading: None, vectors: false, to_fix: straight });
            }
            _ => {}
        }
    }
    pieces
}

fn bearing_between(a: (f64, f64), b: (f64, f64)) -> f64 {
    let cos = a.0.to_radians().cos().max(0.05);
    let (dn, de) = (b.0 - a.0, (b.1 - a.1) * cos);
    (de.atan2(dn).to_degrees() + 360.0) % 360.0
}

// ---------------------------------------------------------------------------------
// Drawing.
// ---------------------------------------------------------------------------------

/// Text written along a line on the page, the right way up whichever way the line runs,
/// centred on a point and lifted off the line by `lift`.
#[allow(clippy::too_many_arguments)]
fn text_along(c: &mut dyn Canvas, font: Name, size: f32, mid: (f32, f32), dir: (f32, f32), lift: f32, s: &str, rgb: (f32, f32, f32)) {
    let (mut dx, mut dy) = dir;
    let len = dx.hypot(dy).max(1e-3);
    dx /= len;
    dy /= len;
    if dx < 0.0 {
        dx = -dx;
        dy = -dy;
    }
    let (nx, ny) = (-dy, dx);
    let w = text_width(font, size, s);
    let (x, y) = (mid.0 - dx * w / 2.0 + nx * lift, mid.1 - dy * w / 2.0 + ny * lift);
    c.text_styled(font, size, x, y, dy.atan2(dx).to_degrees(), s, rgb);
}

/// An altitude the way the map prints it: blue, underlined for at or above, overlined
/// for at or below, both for at.
fn draw_altitude(c: &mut dyn Canvas, bold: Name, size: f32, x: f32, y: f32, leg: &Leg) -> f32 {
    let Some(a) = leg.altitude_ft else { return 0.0 };
    let s = format!("{a:.0}");
    let w = text_width(bold, size, &s);
    c.text_styled(bold, size, x, y, 0.0, &s, BLUE);
    c.set_stroke_rgb(BLUE.0, BLUE.1, BLUE.2);
    c.set_line_width(0.9);
    let under = matches!(leg.altitude_rule, AltitudeRule::AtOrAbove | AltitudeRule::At | AltitudeRule::Between);
    let over = matches!(leg.altitude_rule, AltitudeRule::AtOrBelow | AltitudeRule::At | AltitudeRule::Between);
    if under {
        c.move_to(x - 1.0, y - 2.2);
        c.line_to(x + w + 1.0, y - 2.2);
        c.stroke();
    }
    if over {
        c.move_to(x - 1.0, y + size * 0.78 + 1.6);
        c.line_to(x + w + 1.0, y + size * 0.78 + 1.6);
        c.stroke();
    }
    w
}

/// A waypoint: the four-pointed star of an area navigation fix, or the triangle of one
/// defined by beacons.
fn draw_waypoint(c: &mut dyn Canvas, px: f32, py: f32, rnav: bool) {
    c.set_stroke_gray(INK);
    c.set_line_width(0.9);
    if rnav {
        let (r, k) = (6.5f32, 1.6f32);
        let pts = [(0.0, r), (k, k), (r, 0.0), (k, -k), (0.0, -r), (-k, -k), (-r, 0.0), (-k, k)];
        for (i, (dx, dy)) in pts.iter().enumerate() {
            if i == 0 {
                c.move_to(px + dx, py + dy);
            } else {
                c.line_to(px + dx, py + dy);
            }
        }
        c.close_path();
        c.set_fill_gray(1.0);
        c.fill_even_odd_and_stroke();
    } else {
        c.move_to(px, py + 4.5);
        c.line_to(px + 4.0, py - 2.8);
        c.line_to(px - 4.0, py - 2.8);
        c.close_path();
        c.set_fill_gray(1.0);
        c.fill_even_odd_and_stroke();
    }
}

/// A named fix with what is asked there: its name, the speed limit in magenta and the
/// altitude in blue, stacked beside it where there is room.
#[allow(clippy::too_many_arguments)]
fn draw_fix_block(c: &mut dyn Canvas, f: Name, b: Name, v: &View, leg: &Leg, rnav: bool, taken: &mut Taken, seen: &mut Vec<String>) {
    let _ = f;
    let (Some(lat), Some(lon)) = (leg.lat, leg.lon) else { return };
    if leg.fix.is_empty() || leg.fix.starts_with("RW") {
        return;
    }
    let (px, py) = v.at(lat, lon);
    if !v.inside((px, py), -4.0) {
        return;
    }
    // A fix is written once: the transitions that start at the end of the common route
    // name it again with the same limits, and a chart prints them the once.
    if seen.contains(&leg.fix) {
        return;
    }
    let first = true;
    seen.push(leg.fix.clone());
    draw_waypoint(c, px, py, rnav);
    // What is written: the name once; the limits wherever a leg sets them.
    let mut rows: Vec<(String, f32, (f32, f32, f32), bool)> = Vec::new();
    if first {
        rows.push((leg.fix.clone(), 10.0, INK_RGB, false));
    }
    if let Some(s) = leg.speed_kt {
        rows.push((format!("MAX {s:.0} KT"), 8.5, MAGENTA, false));
    }
    if leg.altitude_ft.is_some() {
        rows.push((String::new(), 9.5, BLUE, true));
    }
    if rows.is_empty() {
        return;
    }
    let widths: Vec<f32> = rows.iter().map(|(s, size, _, alt)| if *alt { text_width(b, *size, &format!("{:.0}", leg.altitude_ft.unwrap_or(0.0))) + 2.0 } else { text_width(b, *size, s) + 2.0 }).collect();
    let bw = widths.iter().copied().fold(0.0f32, f32::max);
    let bh: f32 = rows.iter().map(|(_, size, _, _)| size + 2.5).sum();
    // Beside the fix, in any of eight directions and at a growing distance, the nearest
    // clear one; a fix's name is never left off, so where none is clear it goes to the
    // right of the fix regardless.
    let mut places: Vec<(f32, f32)> = Vec::new();
    for gap in [9.0f32, 16.0, 26.0] {
        places.extend([
            (px + gap, py - bh / 2.0 + 2.0),
            (px - gap - bw, py - bh / 2.0 + 2.0),
            (px - bw / 2.0, py + gap),
            (px - bw / 2.0, py - gap - bh),
            (px + gap * 0.7, py + gap * 0.7),
            (px - gap * 0.7 - bw, py + gap * 0.7),
            (px + gap * 0.7, py - gap * 0.7 - bh),
            (px - gap * 0.7 - bw, py - gap * 0.7 - bh),
        ]);
    }
    let on_paper = |x: f32, y: f32| v.inside((x, y), 0.0) && v.inside((x + bw, y + bh), 0.0);
    let (bx, by) = places.iter().copied().find(|(x, y)| taken.free(*x, *y, bw, bh) && on_paper(*x, *y)).or_else(|| places.iter().copied().find(|(x, y)| on_paper(*x, *y))).unwrap_or(places[0]);
    taken.reserve(bx, by, bw, bh);
    let mut row_y = by + bh;
    for (s, size, rgb, alt) in &rows {
        row_y -= size + 2.5;
        if *alt {
            draw_altitude(c, b, *size, bx, row_y + 2.0, leg);
        } else {
            c.text_styled(b, *size, bx, row_y + 2.0, 0.0, s, *rgb);
        }
    }
}

/// The header: the airport on the left, the place and the kind of page on the right,
/// whose it is in the middle.
fn draw_header(c: &mut dyn Canvas, f: Name, b: Name, t: &Terminal) -> f32 {
    let top = H - MARGIN;
    let idents = match &t.airport_iata {
        Some(iata) if !iata.is_empty() => format!("{}/{}", t.airport.icao, iata),
        _ => t.airport.icao.clone(),
    };
    text(c, b, 15.0, MARGIN, top - 14.0, &idents, INK);
    text(c, f, 9.5, MARGIN, top - 26.0, &shorten(t.airport_name.as_deref().unwrap_or("")).to_uppercase(), INK);
    let place = t.airport_place.clone().unwrap_or_default().to_uppercase();
    text_right(c, b, 15.0, W - MARGIN, top - 14.0, &place, INK);
    let badge = format!("{}{}", if is_rnav(t.procedure) { "RNAV " } else { "" }, if is_sid(t.procedure) { "SID" } else { "STAR" });
    let bw = text_width(b, 9.0, &badge) + 10.0;
    fill_box(c, W - MARGIN - bw, top - 29.0, bw, 12.0, INK);
    text(c, b, 9.0, W - MARGIN - bw + 5.0, top - 26.0, &badge, 1.0);
    let middle = W / 2.0;
    text_centred(c, b, 11.0, middle - 20.0, top - 12.0, "AMDB V1", INK);
    if let Some((from, to)) = crate::sources::msfs::airac_dates() {
        text_centred(c, f, 6.5, middle - 20.0, top - 23.0, &format!("EFF {from} - {to}"), 0.3);
    }
    top - 34.0
}

/// The strip of what is briefed before flying it. Returns where it ends.
fn draw_strip(c: &mut dyn Canvas, f: Name, b: Name, t: &Terminal, top: f32) -> f32 {
    let h = 40.0;
    let y = top - h;
    let (x, w) = (MARGIN, W - 2.0 * MARGIN);
    box_outline(c, x, y, w, h, 1.0, INK);
    // The frequency the procedure is flown on, called by its name.
    let wanted: &[(&str, &str)] = if is_sid(t.procedure) { &[("DEP", "Departure")] } else { &[("ATI", "ATIS"), ("APP", "Approach")] };
    let mut cells: Vec<(String, String)> = Vec::new();
    for (kind, label) in wanted {
        let mut found: Vec<&Communication> = t.comms.iter().filter(|m| m.kind == *kind).collect();
        found.sort_by(|a, b| a.mhz.total_cmp(&b.mhz));
        found.dedup_by(|a, b| (a.mhz - b.mhz).abs() < 0.001);
        if let Some(first) = found.first() {
            let name = if first.callsign.is_empty() { label.to_string() } else { format!("{} {label}", first.callsign) };
            let freqs: Vec<String> = found.iter().take(2).map(|m| format!("{:.3}", m.mhz).trim_end_matches('0').trim_end_matches('.').to_string()).collect();
            cells.push((name, freqs.join("  ")));
        } else {
            // The simulator's own list, where the database has none.
            let sim = if *kind == "DEP" { "DEPARTURE" } else if *kind == "ATI" { "ATIS" } else { "APPROACH" };
            if let Some(fr) = t.airport.frequencies.iter().find(|fr| fr.kind == sim) {
                cells.push((label.to_string(), format!("{:.3}", fr.mhz)));
            }
        }
    }
    cells.push(("Apt Elev".to_string(), format!("{:.0}", t.field_elev_ft)));
    let cell_w = 88.0;
    for (i, (label, value)) in cells.iter().enumerate() {
        let cx = x + cell_w * i as f32 + cell_w / 2.0;
        line(c, x + cell_w * (i + 1) as f32, y, x + cell_w * (i + 1) as f32, y + h, 0.8, INK);
        text_centred(c, f, 6.5, cx, y + h - 12.0, label, INK);
        let mut size = 10.5;
        while size > 6.0 && text_width(b, size, value) > cell_w - 6.0 {
            size -= 0.5;
        }
        text_centred(c, b, size, cx, y + 9.0, value, INK);
    }
    // The notes: transition altitude and level, what the procedure needs of the aircraft,
    // and the speed limit.
    let nx = x + cell_w * cells.len() as f32;
    let mut notes: Vec<String> = Vec::new();
    let info = t.info.clone().unwrap_or_default();
    let mut trans = Vec::new();
    if let Some(a) = info.transition_altitude_ft {
        trans.push(format!("Trans alt: {a:.0}"));
    }
    if let Some(l) = info.transition_level_ft {
        trans.push(format!("Trans level: FL{:.0}", l / 100.0));
    }
    if trans.is_empty() {
        trans.push("Trans alt: by ATC".into());
    }
    notes.push(trans.join("   "));
    if is_rnav(t.procedure) {
        notes.push("RNAV 1 - DME/DME/IRU or GPS required".into());
    }
    if let Some((kt, below)) = info.speed_limit {
        notes.push(format!("MAX {kt:.0} KT below {below:.0}'"));
    }
    let row_h = h / notes.len().max(1) as f32;
    for (i, n) in notes.iter().enumerate() {
        let ry = y + h - row_h * (i + 1) as f32;
        if i > 0 {
            line(c, nx, ry + row_h, x + w, ry + row_h, 0.6, INK);
        }
        text(c, f, 7.5, nx + 5.0, ry + row_h / 2.0 - 2.5, n, INK);
    }
    y
}

/// The procedure's name, its coded name and its runways, in their box.
fn draw_title_box(c: &mut dyn Canvas, f: Name, b: Name, t: &Terminal, top: f32) -> f32 {
    let h = 46.0;
    let y = top - h;
    box_outline(c, MARGIN, y, W - 2.0 * MARGIN, h, 1.0, INK);
    let kind = if is_sid(t.procedure) { "DEPARTURE" } else { "ARRIVAL" };
    let rnav = if is_rnav(t.procedure) { "RNAV " } else { "" };
    text_centred(c, b, 13.0, W / 2.0, y + h - 15.0, &format!("{} {rnav}{kind}", title(t.procedure)), INK);
    let (fix, _) = named_fix(t.procedure);
    text_centred(c, f, 10.0, W / 2.0, y + h - 28.0, &format!("({}.{fix})", t.procedure.name), INK);
    let runways = runways_of(t.procedure);
    let label = if runways.len() == 1 { "RWY" } else { "RWYS" };
    text_centred(c, f, 10.0, W / 2.0, y + h - 41.0, &format!("({label} {})", runway_list(&runways)), INK);
    y
}

/// The table and routing at the foot. Returns how tall it came out, drawing it only when
/// `draw` is set, so the map can be given what is left.
fn draw_foot(c: &mut dyn Canvas, f: Name, b: Name, t: &Terminal, draw: bool) -> f32 {
    let size = 7.8;
    let lead = size + 2.4;
    let (x, w) = (MARGIN, W - 2.0 * MARGIN);
    let sid = is_sid(t.procedure);
    let all = routes(t.procedure);
    let first_col = 58.0;
    let text_w = w - first_col - 10.0;
    // The table: one row per runway (a departure) or per transition (an arrival).
    let (table, rest): (Vec<&Route>, Vec<&Route>) = all.iter().partition(|r| if sid { r.part == "runway" } else { r.part == "enroute" });
    let heading = if sid { "INITIAL CLIMB" } else { "TRANSITION ROUTING" };
    let col_head = if sid { "RWY" } else { "FROM" };
    let mut rows: Vec<(String, Vec<String>)> = Vec::new();
    for r in &table {
        let label = if sid { r.heading.clone() } else { r.names.first().cloned().unwrap_or_default() };
        rows.push((label, wrap_to_width(&route_text(&r.legs, sid), f, size, text_w)));
    }
    let mut routing: Vec<String> = Vec::new();
    for r in &rest {
        let head = match (r.part, sid) {
            ("enroute", _) => format!("{}: ", r.heading),
            ("runway", false) => format!("RWY {}: ", r.heading),
            _ => String::new(),
        };
        routing.push(format!("{head}{}", route_text(&r.legs, sid)));
    }
    let routing_lines: Vec<String> = routing.iter().flat_map(|s| wrap_to_width(s, f, size, w - 12.0)).collect();
    let table_h = if rows.is_empty() { 0.0 } else { 14.0 + rows.iter().map(|(_, l)| l.len().max(1) as f32 * lead + 5.0).sum::<f32>() };
    let routing_h = if routing_lines.is_empty() { 0.0 } else { 14.0 + routing_lines.len() as f32 * lead + 5.0 };
    let total = table_h + routing_h;
    if !draw {
        return total;
    }
    let bottom = MARGIN + 20.0;
    // Routing, at the very foot.
    let mut y = bottom;
    if routing_h > 0.0 {
        box_outline(c, x, y, w, routing_h, 1.0, INK);
        line(c, x, y + routing_h - 12.0, x + w, y + routing_h - 12.0, 0.6, INK);
        text_centred(c, b, 7.5, x + w / 2.0, y + routing_h - 9.0, "ROUTING", INK);
        let mut ry = y + routing_h - 12.0 - lead;
        for l in &routing_lines {
            // The transition's name in bold, where the line starts with it.
            if let Some((head, tail)) = l.split_once(": ").filter(|(h, _)| h.ends_with("TRANSITION") || h.starts_with("RWY")) {
                text(c, b, size, x + 6.0, ry, &format!("{head}:"), INK);
                text(c, f, size, x + 6.0 + text_width(b, size, &format!("{head}: ")), ry, tail, INK);
            } else {
                text(c, f, size, x + 6.0, ry, l, INK);
            }
            ry -= lead;
        }
        y += routing_h;
    }
    if rows.is_empty() {
        return total;
    }
    // The table above it.
    box_outline(c, x, y, w, table_h, 1.0, INK);
    line(c, x, y + table_h - 12.0, x + w, y + table_h - 12.0, 0.6, INK);
    line(c, x + first_col, y, x + first_col, y + table_h, 0.6, INK);
    text_centred(c, b, 7.5, x + first_col / 2.0, y + table_h - 9.0, col_head, INK);
    text_centred(c, b, 7.5, x + first_col + (w - first_col) / 2.0, y + table_h - 9.0, heading, INK);
    let mut ry = y + table_h - 12.0;
    for (i, (label, lines)) in rows.iter().enumerate() {
        let rh = lines.len().max(1) as f32 * lead + 5.0;
        if i > 0 {
            line(c, x, ry, x + w, ry, 0.5, INK);
        }
        let mut ly = ry - lead;
        let lsize = if text_width(b, 8.0, label) > first_col - 6.0 { 6.5 } else { 8.0 };
        text_centred(c, b, lsize, x + first_col / 2.0, ry - rh / 2.0 - 2.5, label, INK);
        for l in lines {
            text(c, f, size, x + first_col + 5.0, ly, l, INK);
            ly -= lead;
        }
        ry -= rh;
    }
    total
}

/// The page. Returns the window its map shows.
fn draw(c: &mut dyn Canvas, t: &Terminal, f: Name, b: Name) -> View {
    let top = draw_header(c, f, b, t);
    let top = draw_strip(c, f, b, t, top);
    let top = draw_title_box(c, f, b, t, top);
    let foot = draw_foot(c, f, b, t, false);
    draw_foot(c, f, b, t, true);
    let map_y = MARGIN + 20.0 + foot;
    let v = draw_map(c, f, b, t, MARGIN, map_y, W - 2.0 * MARGIN, top - map_y);
    line(c, MARGIN, MARGIN + 17.0, W - MARGIN, MARGIN + 17.0, 0.6, RULE);
    text(c, f, 6.2, MARGIN, MARGIN + 9.0, "NOT FOR REAL-WORLD NAVIGATION. Drawn from the simulator's navigation data: fly the published chart.", 0.25);
    let printed = chrono::Utc::now().format("%d %b %Y").to_string().to_uppercase();
    text(
        c,
        f,
        5.8,
        MARGIN,
        MARGIN + 1.0,
        &format!(
            "AMDB V1 - drawn {printed} - procedure {} - fixes, MORA and airspace {} - terrain Copernicus DEM (ESA)",
            t.airport.source,
            crate::sources::navdata::source().unwrap_or_else(|| "simulator".into())
        ),
        0.45,
    );
    v
}

/// The map.
#[allow(clippy::too_many_arguments)]
fn draw_map(c: &mut dyn Canvas, f: Name, b: Name, t: &Terminal, x: f32, y: f32, w: f32, h: f32) -> View {
    let variation = t.airport.magnetic_variation_deg.unwrap_or(0.0);
    let routes = routes(t.procedure);
    let sid = is_sid(t.procedure);
    let rnav = is_rnav(t.procedure);
    // The path of every route, flown.
    let mut drawn: Vec<(usize, Vec<Piece>)> = Vec::new();
    for (i, r) in routes.iter().enumerate() {
        if r.part == "runway" && sid {
            // A climb-out off each runway the route serves, from its far end on its own
            // heading: two parallel runways are two lines that join at the first fix.
            let mut any = false;
            for name in &r.names {
                if let Some((_, near, far)) = t.runways.iter().find(|(n, _, _)| n == name) {
                    drawn.push((i, flown(&r.legs, Some((*far, bearing_between(*near, *far))), variation, t.field_elev_ft)));
                    any = true;
                }
            }
            if any {
                continue;
            }
        }
        drawn.push((i, flown(&r.legs, None, variation, t.field_elev_ft)));
    }
    let mut points: Vec<(f64, f64)> = drawn.iter().flat_map(|(_, p)| p.iter().flat_map(|p| p.points.iter().copied())).collect();
    points.extend(routes.iter().flat_map(|r| r.legs.iter()).filter_map(|l| l.lat.zip(l.lon)));
    points.push((t.airport.lat, t.airport.lon));
    let cos = t.airport.lat.to_radians().cos().max(0.05);
    let reach = points.iter().map(|(lat, lon)| ((lat - t.airport.lat) * 60.0).hypot((lon - t.airport.lon) * 60.0 * cos)).fold(0.0f64, f64::max);
    let v = View::around_within((t.airport.lat, t.airport.lon), &points, x, y, w, h, 10.0, 320.0, (reach * 0.07).max(2.0));

    c.save_state();
    c.rect(x, y, w, h);
    c.clip_nonzero();
    c.end_path();

    // The ground, and the sea.
    if let Some(patch) = &t.patch {
        draw_terrain(c, patch, &v, t.field_elev_ft.max(WATER_NEEDS_FIELD_FT));
    }
    let mut taken = Taken::default();
    // The controlled airspace the airport sits in: its outer edge, in red.
    if let Some(outer) = t
        .airspace
        .iter()
        .max_by(|a, b| extent(&a.boundary).total_cmp(&extent(&b.boundary)))
    {
        c.save_state();
        c.set_stroke_rgb(AIRSPACE.0, AIRSPACE.1, AIRSPACE.2);
        c.set_line_width(1.4);
        for (i, (lat, lon)) in outer.boundary.iter().enumerate() {
            let (px, py) = v.at(*lat, *lon);
            if i == 0 {
                c.move_to(px, py);
            } else {
                c.line_to(px, py);
            }
        }
        c.close_path();
        c.stroke();
        c.restore_state();
        // Its class, written along its edge where the edge is on the paper.
        let on_page: Vec<(f32, f32)> = outer.boundary.iter().map(|(la, lo)| v.at(*la, *lo)).collect();
        if let Some(pair) = on_page.windows(2).filter(|p| v.inside(p[0], -30.0) && v.inside(p[1], -30.0)).max_by(|a, b| {
            let la = (a[1].0 - a[0].0).hypot(a[1].1 - a[0].1);
            let lb = (b[1].0 - b[0].0).hypot(b[1].1 - b[0].1);
            la.total_cmp(&lb)
        }) {
            let mid = ((pair[0].0 + pair[1].0) / 2.0, (pair[0].1 + pair[1].1) / 2.0);
            text_along(c, b, 9.0, mid, (pair[1].0 - pair[0].0, pair[1].1 - pair[0].1), 4.0, &format!("CLASS {}", outer.class), AIRSPACE);
        }
    }

    // The airport: a grey disc with its runways on it.
    let rw_pts: Vec<(f32, f32)> = t.all_runways.iter().flat_map(|(a, bb)| [v.at(a.0, a.1), v.at(bb.0, bb.1)]).collect();
    let (ax, ay) = v.at(t.airport.lat, t.airport.lon);
    let radius = rw_pts.iter().map(|p| (p.0 - ax).hypot(p.1 - ay)).fold(0.0f32, f32::max).max(9.0) + 7.0;
    c.set_fill_rgb(0.72, 0.72, 0.72);
    circle(c, ax, ay, radius);
    c.fill_nonzero();
    for (a, bb) in &t.all_runways {
        let (p, q) = (v.at(a.0, a.1), v.at(bb.0, bb.1));
        line(c, p.0, p.1, q.0, q.1, 4.2, INK);
        line(c, p.0, p.1, q.0, q.1, 2.4, 1.0);
    }
    if t.all_runways.is_empty() {
        c.set_fill_gray(INK);
        circle(c, ax, ay, 3.0);
        c.fill_nonzero();
    }
    taken.reserve(ax - radius, ay - radius, radius * 2.0, radius * 2.0);
    taken.reserve(v.x, v.y + v.h - 96.0, 72.0, 96.0);
    taken.reserve(v.x, v.y, v.w * 0.4, 28.0);

    // The other airports, in grey: a small ringed symbol, and the place, the name and
    // the code under it, the way a chart names the airports round about.
    let grey = (0.42, 0.42, 0.42);
    for (place, name, code, lat, lon) in &t.nearby {
        let (px, py) = v.at(*lat, *lon);
        if !v.inside((px, py), -8.0) {
            continue;
        }
        let lines: Vec<&str> = [place.as_str(), name.as_str(), code.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
        let bw = lines.iter().map(|l| text_width(f, 6.8, l)).fold(0.0f32, f32::max) + 2.0;
        let bh = lines.len() as f32 * 8.0;
        // Above the symbol if there is room, else below, else to either side, and always
        // on the paper.
        let place_at = [(px - bw / 2.0, py + 8.0), (px - bw / 2.0, py - 8.0 - bh), (px + 9.0, py - bh / 2.0), (px - 9.0 - bw, py - bh / 2.0)]
            .into_iter()
            .find(|(x0, y0)| taken.free(*x0, *y0, bw, bh) && taken.free(px - 6.0, py - 6.0, 12.0, 12.0) && v.inside((*x0, *y0), 0.0) && v.inside((x0 + bw, y0 + bh), 0.0));
        let Some((x0, y0)) = place_at else { continue };
        c.set_stroke_rgb(grey.0, grey.1, grey.2);
        c.set_line_width(0.8);
        circle(c, px, py, 3.2);
        c.stroke();
        for k in 0..6 {
            let a = (k as f32 * 60.0).to_radians();
            c.move_to(px + 3.2 * a.cos(), py + 3.2 * a.sin());
            c.line_to(px + 5.4 * a.cos(), py + 5.4 * a.sin());
            c.stroke();
        }
        for (k, l) in lines.iter().enumerate() {
            let lw = text_width(f, 6.8, l);
            c.text_styled(f, 6.8, px - lw / 2.0, y0 + bh - 7.0 - k as f32 * 8.0, 0.0, l, grey);
        }
        taken.reserve(x0, y0, bw, bh);
        taken.reserve(px - 6.0, py - 6.0, 12.0, 12.0);
    }

    // The beacons.
    let wanted: Vec<String> = t.procedure.transitions.iter().flat_map(|tr| tr.legs.iter()).flat_map(|l| [l.navaid.clone(), l.fix.clone()]).filter(|s| !s.is_empty()).collect();
    draw_navaids(c, f, b, &v, &t.navaids, &wanted, &mut taken);

    // The tracks: the part every flight flies solid, the transitions dashed.
    let mut labelled: Vec<(String, String)> = Vec::new();
    for (i, pieces) in &drawn {
        let r = &routes[*i];
        let dashed = r.part == "enroute";
        let mut last_page: Option<((f32, f32), (f32, f32))> = None;
        for p in pieces {
            let pts: Vec<(f32, f32)> = round_turns(p.points.clone()).iter().map(|(la, lo)| v.at(*la, *lo)).collect();
            if pts.len() < 2 {
                continue;
            }
            c.save_state();
            if dashed || p.vectors {
                c.set_dash(&[6.0, 3.5], 0.0);
            }
            c.set_stroke_gray(INK);
            c.set_line_width(if dashed { 1.6 } else { 1.9 });
            for (k, q) in pts.iter().enumerate() {
                if k == 0 {
                    c.move_to(q.0, q.1);
                } else {
                    c.line_to(q.0, q.1);
                }
            }
            c.stroke();
            c.restore_state();
            // The paper the track covers, so the names of the fixes are put beside it and
            // not on it.
            for pair in pts.windows(2) {
                let (a, z) = (pair[0], pair[1]);
                let steps = ((z.0 - a.0).hypot(z.1 - a.1) / 6.0).ceil().max(1.0) as usize;
                for s in 0..=steps {
                    let t = s as f32 / steps as f32;
                    taken.reserve(a.0 + (z.0 - a.0) * t - 2.5, a.1 + (z.1 - a.1) * t - 2.5, 5.0, 5.0);
                }
            }
            last_page = Some((pts[pts.len() - 2], pts[pts.len() - 1]));
            // A heading flown to an altitude: the heading along it, the altitude at its
            // end, as the chart writes them.
            if let Some((hdg, alt)) = p.heading {
                let (a0, a1) = (pts[pts.len() - 2], pts[pts.len() - 1]);
                let mid = ((a0.0 + a1.0) / 2.0, (a0.1 + a1.1) / 2.0);
                text_along(c, b, 8.5, mid, (a1.0 - a0.0, a1.1 - a0.1), 4.0, &format!("{hdg:03.0}\u{b0} hdg"), INK_RGB);
                if let Some(a) = alt {
                    let leg = Leg { altitude_ft: Some(a), altitude_rule: AltitudeRule::AtOrAbove, ..Leg::default() };
                    let s = format!("{a:.0}");
                    let sw = text_width(b, 9.5, &s);
                    let (lx, ly) = (a1.0 + 5.0, a1.1 + 3.0);
                    if taken.free(lx, ly - 3.0, sw + 2.0, 13.0) {
                        draw_altitude(c, b, 9.5, lx, ly, &leg);
                        taken.reserve(lx, ly - 3.0, sw + 2.0, 13.0);
                    }
                }
                if p.vectors {
                    text_along(c, f, 7.0, mid, (a1.0 - a0.0, a1.1 - a0.1), -9.0, "VECTORS", INK_RGB);
                }
            }
            // Course and distance along a leg to a fix, once for a leg routes share.
            if p.to_fix && p.points.len() >= 2 {
                let (a, z) = (p.points[0], *p.points.last().unwrap());
                let key = (format!("{:.4}{:.4}", a.0, a.1), format!("{:.4}{:.4}", z.0, z.1));
                if labelled.contains(&key) {
                    continue;
                }
                labelled.push(key);
                let (a0, a1) = (pts[pts.len() - 2], pts[pts.len() - 1]);
                let page_len = (a1.0 - a0.0).hypot(a1.1 - a0.1);
                if page_len < 32.0 {
                    continue;
                }
                let (course, nm) = course_and_nm(p.points[p.points.len() - 2], z, variation);
                let mid = ((a0.0 + a1.0) / 2.0, (a0.1 + a1.1) / 2.0);
                let size = if page_len < 60.0 { 7.5 } else { 8.5 };
                text_along(c, b, size, mid, (a1.0 - a0.0, a1.1 - a0.1), 4.0, &format!("{course:03.0}\u{b0}"), INK_RGB);
                text_along(c, f, size - 1.0, mid, (a1.0 - a0.0, a1.1 - a0.1), -9.5, &format!("{nm:.1}"), INK_RGB);
                // The transition's name along its longest leg, as the chart writes it.
                if r.part == "enroute" && page_len > 110.0 {
                    let (fix, _) = named_fix(t.procedure);
                    let name = r.names.first().cloned().unwrap_or_default();
                    let q = (a0.0 + (a1.0 - a0.0) * 0.28, a0.1 + (a1.1 - a0.1) * 0.28);
                    let _ = fix;
                    text_along(c, b, 8.0, q, (a1.0 - a0.0, a1.1 - a0.1), 4.0, &format!("{name} ({}.{name})", t.procedure.name), INK_RGB);
                }
            }
        }
        if let Some((from, to)) = last_page {
            arrow_head(c, from, to, INK);
        }
    }
    // The fixes over the tracks, with what is asked at each.
    let mut seen: Vec<String> = Vec::new();
    for r in &routes {
        for leg in &r.legs {
            draw_fix_block(c, f, b, &v, leg, rnav, &mut taken, &mut seen);
        }
    }
    let holds: Vec<&Leg> = routes.iter().flat_map(|r| r.legs.iter().copied()).collect();
    draw_holds(c, f, &v, &holds);
    draw_mora(c, b, &v, &t.mora, &mut taken);
    c.restore_state();

    draw_furniture(c, f, b, &v, t.airport.magnetic_variation_deg, None, false, 0.0, 0.0);
    box_outline(c, x, y, w, h, 1.0, INK);
    v
}

/// The grid minimum off-route altitudes, big and grey in each square, as a chart prints
/// them: thousands large and hundreds small. Drawn last, in open paper within the part
/// of each square that is on the map, so a figure never sits on a track or a fix.
fn draw_mora(c: &mut dyn Canvas, b: Name, v: &View, mora: &[(f64, f64, f64)], taken: &mut Taken) {
    let (south, east) = (v.north - v.deg_h, v.west + v.deg_w);
    for (lat, lon, ft) in mora {
        let (s, n) = (lat.max(south), (lat + 1.0).min(v.north));
        let (w0, e) = (lon.max(v.west), (lon + 1.0).min(east));
        if n <= s || e <= w0 {
            continue;
        }
        let big = format!("{}", (ft / 1000.0).floor() as i64);
        let small = format!("{}", ((ft % 1000.0) / 100.0).round() as i64 % 10);
        let bw = text_width(b, 20.0, &big);
        let (box_w, box_h) = (bw + 14.0, 22.0);
        // The middle of the visible part first, then outwards across it on a grid.
        let mut spots: Vec<(f64, f64, f64)> = Vec::new();
        for i in 0..=6 {
            for j in 0..=6 {
                let (la, lo) = (s + (n - s) * i as f64 / 6.0, w0 + (e - w0) * j as f64 / 6.0);
                let d = (i as f64 - 3.0).hypot(j as f64 - 3.0);
                spots.push((d, la, lo));
            }
        }
        spots.sort_by(|a, b| a.0.total_cmp(&b.0));
        let found = spots.into_iter().map(|(_, la, lo)| v.at(la, lo)).find(|(px, py)| {
            let (x0, y0) = (px - box_w / 2.0, py - 8.0);
            v.inside((x0, y0), -6.0) && v.inside((x0 + box_w, y0 + box_h), -6.0) && taken.free(x0, y0, box_w, box_h)
        });
        let Some((px, py)) = found else { continue };
        c.text_styled(b, 20.0, px - bw / 2.0 - 4.0, py - 7.0, 0.0, &big, (0.62, 0.62, 0.62));
        c.text_styled(b, 12.0, px + bw / 2.0 - 3.0, py - 7.0, 0.0, &small, (0.62, 0.62, 0.62));
        taken.reserve(px - box_w / 2.0, py - 8.0, box_w, box_h);
    }
}

/// How far a boundary reaches, as the area of the box round it.
fn extent(points: &[(f64, f64)]) -> f64 {
    let (mut n, mut s, mut e, mut wst) = (f64::MIN, f64::MAX, f64::MIN, f64::MAX);
    for (lat, lon) in points {
        n = n.max(*lat);
        s = s.min(*lat);
        e = e.max(*lon);
        wst = wst.min(*lon);
    }
    (n - s) * (e - wst)
}

/// Gather what a departure or arrival chart needs and draw it with `f`.
pub fn with_terminal<R>(icao: &str, name: &str, f: impl FnOnce(&Terminal) -> Result<R>) -> Result<R> {
    let icao = icao.to_uppercase();
    let found = crate::sources::msfs::procedures::find(&icao)?.ok_or_else(|| anyhow::anyhow!("{icao} has no procedures in the simulator's navigation data"))?;
    let procedure = found
        .procedures
        .iter()
        .find(|p| p.kind != Kind::Approach && p.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow::anyhow!("{icao} has no departure or arrival called {name}"))?;
    // The airport's own name and place, where the index has them. Offline is no reason
    // not to draw the chart.
    let http = crate::sources::http::Http::new(120, 0);
    let cache = crate::cache::Cache::for_index(false);
    let (mut airport_name, mut airport_iata, mut airport_place, mut field_elev_ft) = (None, None, None, 0.0);
    let mut nearby: Vec<(String, String, String, f64, f64)> = Vec::new();
    {
        let mut idx = crate::sources::index::AirportIndex::default();
        if idx.load_ourairports_online(&http, &cache).is_ok() {
            // The airports a crew would see out of the window on the way, named the way
            // a chart names them: the place, then the airport.
            let cos = found.lat.to_radians().cos().max(0.05);
            for (code, e) in &idx.by_icao {
                if code == &icao || !matches!(e.kind.as_deref(), Some("large_airport") | Some("medium_airport")) {
                    continue;
                }
                let nm = ((e.lat - found.lat) * 60.0).hypot((e.lon - found.lon) * 60.0 * cos);
                if nm > 160.0 {
                    continue;
                }
                let place = match (e.city.as_deref(), e.region.as_deref()) {
                    (Some(city), Some(region)) => format!("{} {}", city.to_uppercase(), region.rsplit('-').next().unwrap_or(region)),
                    (Some(city), None) => city.to_uppercase(),
                    _ => String::new(),
                };
                nearby.push((place, shorten(e.name.as_deref().unwrap_or("")), code.clone(), e.lat, e.lon));
            }
        }
        if idx.by_icao.contains_key(&icao) {
            if let Some(e) = idx.get(&icao) {
                airport_name = e.name.clone();
                airport_iata = e.iata.clone().filter(|s| s.len() == 3);
                field_elev_ft = e.elevation_ft.unwrap_or(0.0);
                airport_place = match (e.city.as_deref(), e.region.as_deref()) {
                    (Some(city), Some(region)) => Some(format!("{city}, {}", region.rsplit('-').next().unwrap_or(region))),
                    (Some(city), None) => Some(city.to_string()),
                    _ => None,
                };
            }
        }
    }
    // Every runway the procedure serves, with its other end, which is its reciprocal.
    let mut runways = Vec::new();
    for rw in runways_of(procedure) {
        let Some(near) = crate::sources::navdata::runway_threshold(&icao, &rw) else { continue };
        let Some(far) = reciprocal(&rw).and_then(|r| crate::sources::navdata::runway_threshold(&icao, &r)) else { continue };
        runways.push((rw, near, far));
    }
    // Every runway at the airport, for the picture of it.
    let mut all_runways = Vec::new();
    for n in 1..=36u32 {
        for side in ["", "L", "C", "R"] {
            let rw = format!("{n:02}{side}");
            if n > 18 {
                continue;
            }
            let (Some(a), Some(bb)) = (crate::sources::navdata::runway_threshold(&icao, &rw), reciprocal(&rw).and_then(|r| crate::sources::navdata::runway_threshold(&icao, &r))) else { continue };
            all_runways.push((a, bb));
        }
    }
    let reach = procedure
        .transitions
        .iter()
        .flat_map(|t| t.legs.iter())
        .filter_map(|l| l.lat.zip(l.lon))
        .map(|(lat, lon)| ((lat - found.lat) * 60.0).hypot((lon - found.lon) * 60.0 * found.lat.to_radians().cos().max(0.05)))
        .fold(20.0f64, f64::max);
    let navaids = crate::sources::navdata::beacons_near(found.lat, found.lon, reach.min(150.0));
    let deg = reach / 60.0 * 1.3;
    let cos = found.lat.to_radians().cos().max(0.05);
    let mora = crate::sources::navdata::grid_mora(found.lat - deg, found.lat + deg, found.lon - deg / cos, found.lon + deg / cos);
    // The terrain over the whole of it, at a spacing that keeps the patch a size the
    // network and the drawing can manage.
    let radius_km = (reach * 1.852 * 1.35).clamp(20.0, 330.0);
    let step_m = (radius_km * 1000.0 / 260.0).max(90.0);
    // The model has no tile over the open sea at all, so a square with nothing in it is
    // sea, which is what a departure over the water has to show.
    let patch = crate::sources::copernicus::patch(&http, &cache, found.lat, found.lon, radius_km, step_m).ok().map(|mut p| {
        for h in p.heights.iter_mut() {
            if !h.is_finite() {
                *h = 0.0;
            }
        }
        p
    });
    let t = Terminal {
        airport: &found,
        airport_name,
        airport_iata,
        airport_place,
        field_elev_ft,
        procedure,
        navaids,
        runways,
        all_runways,
        info: crate::sources::navdata::airport_info(&icao),
        comms: crate::sources::navdata::communications(&icao),
        airspace: crate::sources::navdata::airspace(&icao),
        mora,
        patch,
        nearby,
    };
    f(&t)
}

/// The other end of a runway: 05 is 23, 09L is 27R.
fn reciprocal(runway: &str) -> Option<String> {
    let digits: String = runway.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: u32 = digits.parse().ok()?;
    let side = match &runway[digits.len()..] {
        "L" => "R",
        "R" => "L",
        other => other,
    };
    let other = if n > 18 { n - 18 } else { n + 18 };
    Some(format!("{other:02}{side}"))
}

/// Write a departure or arrival chart as a PDF.
pub fn write(t: &Terminal, out: &FsPath) -> Result<()> {
    write_page(out, |c, f, b| {
        let _ = draw(c, t, f, b);
    })
}

/// The same as a picture, by day and by night.
pub fn picture(t: &Terminal, scale: f32) -> Result<Picture> {
    picture_of(scale, |c, f, b| draw(c, t, f, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg(path: &str, fix: &str) -> Leg {
        Leg { path: path.into(), fix: fix.into(), ..Leg::default() }
    }

    #[test]
    fn a_departure_reads_the_way_the_chart_writes_it() {
        let mut climb = leg("VA", "");
        climb.course_deg = Some(314.0);
        climb.altitude_ft = Some(520.0);
        let mut direct = leg("DF", "SKORR");
        direct.turn = Some(Turn::Left);
        direct.altitude_ft = Some(2500.0);
        direct.altitude_rule = AltitudeRule::AtOrAbove;
        direct.speed_kt = Some(210.0);
        let legs = [climb, direct];
        let refs: Vec<&Leg> = legs.iter().collect();
        assert_eq!(route_text(&refs, true), "Climb on heading 314\u{b0} to 520, then LEFT turn direct to cross SKORR at or above 2500 and at or below 210 KT.");
    }

    #[test]
    fn names_and_runways_are_written_as_a_chart_writes_them() {
        let p = Procedure {
            kind: Kind::Sid,
            name: "DEGU1E".into(),
            approach_type: None,
            suffix: None,
            variant: None,
            runway: String::new(),
            transitions: vec![crate::sources::msfs::procedures::Transition { name: "RW05".into(), part: "runway".into(), legs: vec![leg("IF", "RW05"), leg("DF", "MA647"), leg("TF", "DEGUN")] }],
        };
        assert_eq!(title(&p), "DEGUN 1E");
        assert_eq!(runways_of(&p), vec!["05".to_string()]);
        assert_eq!(reciprocal("05").as_deref(), Some("23"));
        assert_eq!(reciprocal("27R").as_deref(), Some("09L"));
        assert_eq!(runway_list(&["31L".into(), "31R".into()]), "31L/R");
        assert_eq!(runway_list(&["04L".into(), "04R".into(), "13L".into()]), "4L/R, 13L");
    }

    #[test]
    fn a_left_turn_goes_the_long_way_round_when_it_says_so() {
        // Out on 314 and back to a fix to the south-west: left is the long way.
        let from = (40.65, -73.80);
        let fix = (40.60, -73.85);
        let left = turn_to_fix(from, 314.0, Some(Turn::Left), fix);
        let right = turn_to_fix(from, 314.0, Some(Turn::Right), fix);
        assert!(left.len() > 3 && right.len() > 3);
        // The left turn swings out west before coming back; the right one does not.
        let west = |pts: &[(f64, f64)]| pts.iter().map(|p| p.1).fold(f64::MAX, f64::min);
        assert!(west(&left) < west(&right) || left.len() != right.len());
    }
}
