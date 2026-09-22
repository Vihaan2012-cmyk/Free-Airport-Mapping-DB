//! An approach chart, laid out the way an airline chart is: a header naming the
//! procedure, a briefing strip of the numbers a crew reads first, a plan view, a descent
//! profile and a minima band along the bottom.
//!
//! Everything on the page comes from free data or from the simulator's own navigation
//! database on this machine: the procedure and its fixes from the simulator, the terrain
//! from the Copernicus DEM, the obstacles from the FAA's obstacle file or OpenStreetMap,
//! and the airport itself from our own build. The minimum is worked out here rather than
//! copied from anywhere, and the page says so.

use crate::minima::{Approach, Estimate, LimitedBy};
use crate::sources::copernicus::Patch;
use crate::sources::msfs::procedures::{AirportProcedures, FixRole, Kind, Leg, Procedure, Transition, Turn};
use crate::sources::navdata::{Beacon, Ils, Kind as NavaidKind};
use crate::sources::obstacles::Obstacle;
use anyhow::{Context, Result};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};
use std::path::Path as FsPath;

const W: f32 = 595.0; // A4 portrait, points
const H: f32 = 842.0;
const MARGIN: f32 = 28.0;
const INK: f32 = 0.10;
const RULE: f32 = 0.30;
/// The smallest and largest the plan view will scale itself to.
const PLAN_MIN_NM: f64 = 6.0;
const PLAN_MAX_NM: f64 = 30.0;

/// What the chart is drawn from.
pub struct Chart<'a> {
    pub airport: &'a AirportProcedures,
    pub airport_name: Option<&'a str>,
    pub procedure: &'a Procedure,
    /// An arrival drawn feeding the approach, when one was asked for.
    pub star: Option<&'a Procedure>,
    pub patch: &'a Patch,
    /// A wider, coarser terrain read, which covers the corners of the map that the close
    /// one does not reach.
    pub wide: Option<&'a Patch>,
    pub obstacles: &'a [Obstacle],
    /// The landing threshold, where our own build of the airport gives one.
    pub threshold: Option<(f64, f64)>,
    /// Touchdown zone elevation: what the minimum is measured from.
    pub tdze_ft: f64,
    /// True when that elevation is a published survey.
    pub tdze_surveyed: bool,
    pub field_elev_ft: f64,
    /// Minimum safe altitude within 25 NM, where a wide enough terrain patch was read.
    pub msa_ft: Option<f64>,
    /// The same by quadrant, which is how a chart prints it.
    pub msa_sectors: &'a [crate::minima::Sector],
    /// The final approach track, in degrees true, which is what the drawing uses.
    pub track_deg: f64,
    /// The course to print, which is magnetic where the data carries one.
    pub course_mag_deg: Option<f64>,
    /// How far magnetic north is from true north here, positive east.
    pub variation_deg: Option<f64>,
    pub kind: Approach,
    /// The circling minimum for each aircraft category, where one was worked out.
    pub circling: &'a [(char, f64)],
    pub airport_dir: Option<&'a FsPath>,
    /// Both ends of the landing runway, threshold first.
    pub runway_ends: Option<((f64, f64), (f64, f64))>,
    /// How long and wide the landing runway is, in metres, and its lights.
    pub runway_size: Option<(f64, f64)>,
    pub runway_lighting: &'a [&'static str],
    /// The dates the navigation data is in force between.
    pub airac: Option<(String, String)>,
    /// What the missed approach asks for, where it asks for more than the standard climb.
    pub missed_climb: Option<(f64, String)>,
    /// The altitude the procedure codes at its missed approach point, which is the
    /// published minimum in some of the world's data and a lower crossing altitude in
    /// the rest, so the chart reports it rather than relying on it.
    pub coded_ft: Option<f64>,
    /// What the safe altitude ring is measured from, which is the published centre where
    /// there is a published one and the airport itself where there is not.
    pub msa_caption: String,
    /// The glidepath angle, where the approach is flown down one.
    pub glidepath_deg: Option<f64>,
    /// The minimum with the glidepath out of use — the localiser line of the same chart,
    /// which is published beside the ILS one and is what is flown when the glidepath
    /// fails.
    pub published_loc: Option<(f64, f64)>,
    /// The beacons near the airport, nearest first, for the plan to draw.
    pub navaids: &'a [Beacon],
    /// The localiser serving this runway: what it is called, what it is tuned to, and
    /// what the distances on the fixes are measured from.
    pub ils: Option<&'a Ils>,
    /// The minimum as the state's own chart publishes it, where it could be read.
    pub published: Option<&'a crate::sources::dtpp::Published>,
    /// True where the approach is not flown to a straight-in landing.
    pub circling_only: bool,
}

/// The bytes a PDF string carries, in the one-byte encoding the built-in fonts use.
///
/// A chart is not all letters and digits: a course is written 044 with a degree sign and
/// a visibility as a half or a quarter. WinAnsi has those where ASCII does not, so the
/// few a chart uses are spelled out here rather than turned into question marks.
fn ascii(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| match c {
            c if c.is_ascii() && !c.is_control() => c as u8,
            '\u{b0}' => 0xB0,
            '\u{bd}' => 0xBD,
            '\u{bc}' => 0xBC,
            '\u{be}' => 0xBE,
            '\u{2013}' | '\u{2014}' => b'-',
            '\u{2018}' | '\u{2019}' => b'\'',
            '\u{201c}' | '\u{201d}' => b'"',
            _ => b'?',
        })
        .collect()
}

/// Helvetica's own character widths, thousandths of the point size, for space to tilde.
/// Guessing at these is what makes a right-aligned title run off the page.
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222,
    833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];
const HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278,
    889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

fn text_width(font: Name, size: f32, s: &str) -> f32 {
    let table = if font.0 == b"B" { &HELVETICA_BOLD } else { &HELVETICA };
    let mils: u32 = s.chars().map(|c| if (' '..='~').contains(&c) { table[c as usize - 32] as u32 } else { 500 }).sum();
    mils as f32 * size / 1000.0
}

fn text(c: &mut Content, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32) {
    c.begin_text();
    c.set_fill_gray(grey);
    c.set_font(font, size);
    c.next_line(x, y);
    c.show(Str(&ascii(s)));
    c.end_text();
}

fn text_right(c: &mut Content, font: Name, size: f32, right: f32, y: f32, s: &str, grey: f32) {
    text(c, font, size, right - text_width(font, size, s), y, s, grey);
}

fn text_centred(c: &mut Content, font: Name, size: f32, centre: f32, y: f32, s: &str, grey: f32) {
    text(c, font, size, centre - text_width(font, size, s) / 2.0, y, s, grey);
}

/// A label over the map, on a white patch so it reads over shaded ground.
fn label(c: &mut Content, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32) {
    c.set_fill_gray(1.0);
    c.rect(x - 1.0, y - 2.0, text_width(font, size, s) + 2.0, size + 1.0);
    c.fill_nonzero();
    text(c, font, size, x, y, s, grey);
}

/// Where labels have already been put, so the next one does not land on top of them.
#[derive(Default)]
struct Taken {
    boxes: Vec<(f32, f32, f32, f32)>,
    /// Fixes already drawn, so the ways in that share one do not draw it again.
    fixes: std::collections::HashSet<String>,
}

impl Taken {
    /// Keep a piece of the page clear. The runway and the approach are drawn before any
    /// label is placed, so what they occupy has to be spoken for or a beacon's box will
    /// sit on top of them.
    fn reserve(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.boxes.push((x, y, w, h));
    }

    /// True the first time a fix is seen.
    fn first_time(&mut self, fix: &str) -> bool {
        self.fixes.insert(fix.to_string())
    }

    fn free(&self, x: f32, y: f32, w: f32, h: f32) -> bool {
        !self.boxes.iter().any(|(ox, oy, ow, oh)| x < ox + ow && *ox < x + w && y < oy + oh && *oy < y + h)
    }

    /// Put a label down if there is room, either where asked or a little below.
    fn label(&mut self, c: &mut Content, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32) -> bool {
        let (w, h) = (text_width(font, size, s) + 2.0, size + 2.0);
        for drop in [0.0, -h, -2.0 * h, h] {
            if self.free(x, y + drop, w, h) {
                label(c, font, size, x, y + drop, s, grey);
                self.boxes.push((x, y + drop, w, h));
                return true;
            }
        }
        false
    }
}

fn line(c: &mut Content, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, grey: f32) {
    c.set_stroke_gray(grey);
    c.set_line_width(w);
    c.move_to(x1, y1);
    c.line_to(x2, y2);
    c.stroke();
}

fn box_outline(c: &mut Content, x: f32, y: f32, w: f32, h: f32, weight: f32, grey: f32) {
    c.set_stroke_gray(grey);
    c.set_line_width(weight);
    c.rect(x, y, w, h);
    c.stroke();
}

fn fill_box(c: &mut Content, x: f32, y: f32, w: f32, h: f32, grey: f32) {
    c.set_fill_gray(grey);
    c.rect(x, y, w, h);
    c.fill_nonzero();
}

/// A circle, as four Bezier arcs.
fn circle(c: &mut Content, cx: f32, cy: f32, r: f32) {
    let k = r * 0.5523;
    c.move_to(cx + r, cy);
    c.cubic_to(cx + r, cy + k, cx + k, cy + r, cx, cy + r);
    c.cubic_to(cx - k, cy + r, cx - r, cy + k, cx - r, cy);
    c.cubic_to(cx - r, cy - k, cx - k, cy - r, cx, cy - r);
    c.cubic_to(cx + k, cy - r, cx + r, cy - k, cx + r, cy);
    c.close_path();
}

/// The legs that make up the final approach, and the ones that make up the missed.
fn part_legs<'a>(p: &'a Procedure, part: &str) -> Vec<&'a Leg> {
    p.transitions.iter().filter(|t| t.part == part).flat_map(|t| t.legs.iter()).collect()
}

fn final_legs(p: &Procedure) -> Vec<&Leg> {
    part_legs(p, "final")
}

/// The transitions that feed the approach: the ways in from an arrival.
fn feeder_transitions(p: &Procedure) -> Vec<&Transition> {
    p.transitions.iter().filter(|t| t.part.is_empty() && !t.name.is_empty()).collect()
}

/// A point on the page for a latitude and longitude, and the window it belongs to.
struct View {
    west: f64,
    north: f64,
    deg_w: f64,
    deg_h: f64,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl View {
    /// The window the plan view shows.
    ///
    /// The airport is always on it, with room around it, and the approach gets the
    /// larger share of the paper, because that is the half with something on it. A fix
    /// further out than the window reaches runs off the edge, the way it does on a
    /// printed chart.
    fn around(airport: (f64, f64), track_deg: f64, points: &[(f64, f64)], x: f32, y: f32, w: f32, h: f32) -> View {
        let cos = airport.0.to_radians().cos().max(0.05);
        let far = points
            .iter()
            .map(|(lat, lon)| {
                let dn = (lat - airport.0) * 60.0;
                let de = (lon - airport.1) * 60.0 * cos;
                (dn * dn + de * de).sqrt()
            })
            .fold(0.0f64, f64::max);
        // The approach runs out from the airport in one direction, so the window is
        // centred between the two: the airport at one end, the farthest fix at the other,
        // with a margin around both.
        let back = (track_deg + 180.0).to_radians();
        let shift_nm = far / 2.0;
        let centre = (airport.0 + shift_nm * back.cos() / 60.0, airport.1 + shift_nm * back.sin() / 60.0 / cos);
        let half_nm = (far / 2.0 * 1.2 + 1.5).clamp(PLAN_MIN_NM / 2.0, PLAN_MAX_NM / 2.0);

        // The shorter side of the box has to hold that, whichever way the approach runs.
        let aspect = (w / h) as f64;
        let (half_w_nm, half_h_nm) = if aspect >= 1.0 { (half_nm * aspect, half_nm) } else { (half_nm, half_nm / aspect) };
        let deg_h = half_h_nm * 2.0 / 60.0;
        let deg_w = half_w_nm * 2.0 / 60.0 / cos;
        View { west: centre.1 - deg_w / 2.0, north: centre.0 + deg_h / 2.0, deg_w, deg_h, x, y, w, h }
    }

    fn at(&self, lat: f64, lon: f64) -> (f32, f32) {
        let fx = (lon - self.west) / self.deg_w;
        let fy = (self.north - lat) / self.deg_h;
        (self.x + fx as f32 * self.w, self.y + self.h - fy as f32 * self.h)
    }

    fn inside(&self, p: (f32, f32), slack: f32) -> bool {
        p.0 >= self.x - slack && p.0 <= self.x + self.w + slack && p.1 >= self.y - slack && p.1 <= self.y + self.h + slack
    }

    /// Nautical miles across the window, measured at the middle of it: a degree of
    /// longitude shrinks as you go north, so taking it at the top edge would put the
    /// scale bar out by half a percent.
    fn span_nm(&self) -> f64 {
        let middle = self.north - self.deg_h / 2.0;
        self.deg_w * 60.0 * middle.to_radians().cos()
    }

    fn px_per_nm(&self) -> f32 {
        self.w / self.span_nm().max(0.001) as f32
    }
}

/// Working in miles about a point, which is how arcs and holding patterns are laid out
/// before they are put on the page.
#[derive(Debug, Clone, Copy)]
struct Local {
    lat: f64,
    lon: f64,
    cos: f64,
}

impl Local {
    fn at(origin: (f64, f64)) -> Local {
        Local { lat: origin.0, lon: origin.1, cos: origin.0.to_radians().cos().max(0.05) }
    }

    /// East and north of the origin, in miles.
    fn to_nm(&self, lat: f64, lon: f64) -> (f64, f64) {
        ((lon - self.lon) * 60.0 * self.cos, (lat - self.lat) * 60.0)
    }

    fn to_ll(&self, east_nm: f64, north_nm: f64) -> (f64, f64) {
        (self.lat + north_nm / 60.0, self.lon + east_nm / 60.0 / self.cos)
    }
}

/// The points of an arc of radius `radius_nm` from `from` to `to`.
///
/// A DME arc leg says only how far out it is flown; the centre is the navaid, and both
/// ends of the arc are that far from it. Two circles of the right radius pass through
/// both ends, and the turn direction picks which one, so the navaid's own position is
/// never needed.
fn arc_points(from: (f64, f64), to: (f64, f64), radius_nm: f64, turn: Option<Turn>) -> Vec<(f64, f64)> {
    let local = Local::at(from);
    let (ax, ay) = (0.0, 0.0);
    let (bx, by) = local.to_nm(to.0, to.1);
    let (dx, dy) = (bx - ax, by - ay);
    let half = (dx * dx + dy * dy).sqrt() / 2.0;
    if half < 0.05 || half > radius_nm {
        return vec![from, to];
    }
    // The centre sits on the perpendicular bisector, this far off the midpoint.
    let off = (radius_nm * radius_nm - half * half).sqrt();
    let (mx, my) = ((ax + bx) / 2.0, (ay + by) / 2.0);
    let (nx, ny) = (-dy / (half * 2.0), dx / (half * 2.0));
    let sign = if matches!(turn, Some(Turn::Left)) { -1.0 } else { 1.0 };
    let (cx, cy) = (mx + nx * off * sign, my + ny * off * sign);
    let start = (ay - cy).atan2(ax - cx);
    let end = (by - cy).atan2(bx - cx);
    let mut sweep = end - start;
    // Take the way round that matches the turn: right turns run clockwise.
    while sweep > std::f64::consts::PI {
        sweep -= std::f64::consts::TAU;
    }
    while sweep < -std::f64::consts::PI {
        sweep += std::f64::consts::TAU;
    }
    let steps = ((sweep.abs().to_degrees() / 4.0) as usize).clamp(4, 90);
    (0..=steps)
        .map(|i| {
            let a = start + sweep * i as f64 / steps as f64;
            local.to_ll(cx + radius_nm * a.cos(), cy + radius_nm * a.sin())
        })
        .collect()
}

/// A holding pattern as a line on the ground: the inbound leg to the fix, the turn onto
/// the outbound, the outbound leg alongside it, and the turn back.
///
/// Everything is worked out in miles east and north of the fix. The aircraft flies the
/// inbound leg on `inbound_deg` and turns the way `turn` says, which is to the right
/// unless the procedure says otherwise.
fn hold_points(fix: (f64, f64), inbound_deg: f64, turn: Option<Turn>, leg_nm: f64) -> Vec<(f64, f64)> {
    let local = Local::at(fix);
    let c = inbound_deg.to_radians();
    // The way the aircraft is going, and the side it turns towards.
    let u = (c.sin(), c.cos());
    let hand = if matches!(turn, Some(Turn::Left)) { -1.0 } else { 1.0 };
    let s = (u.1 * hand, -u.0 * hand);
    let radius = (leg_nm / 4.0).clamp(0.4, 1.2);

    let mut pts: Vec<(f64, f64)> = Vec::new();
    let arc = |pts: &mut Vec<(f64, f64)>, centre: (f64, f64), from: (f64, f64)| {
        let start = (from.1 - centre.1).atan2(from.0 - centre.0);
        for i in 1..=16 {
            // A right-hand turn goes clockwise, which is the way angles decrease.
            let a = start - hand * std::f64::consts::PI * i as f64 / 16.0;
            pts.push((centre.0 + radius * a.cos(), centre.1 + radius * a.sin()));
        }
    };
    // Inbound leg, ending at the fix.
    let entry = (-u.0 * leg_nm, -u.1 * leg_nm);
    pts.push(entry);
    pts.push((0.0, 0.0));
    // The turn at the fix, onto the outbound.
    arc(&mut pts, (s.0 * radius, s.1 * radius), (0.0, 0.0));
    // The outbound leg, alongside the inbound.
    let outbound_end = (s.0 * radius * 2.0 - u.0 * leg_nm, s.1 * radius * 2.0 - u.1 * leg_nm);
    pts.push(outbound_end);
    // The turn back onto the inbound.
    arc(&mut pts, (outbound_end.0 - s.0 * radius, outbound_end.1 - s.1 * radius), outbound_end);
    pts.into_iter().map(|(east, north)| local.to_ll(east, north)).collect()
}

/// The bands terrain is tinted in, and the colour of each: the height it starts at, then
/// red, green and blue.
///
/// A chart tints high ground rather than shading it grey, because the point is to see at
/// a glance where the ground is high, not to read a height off it. These are the bands an
/// aeronautical chart uses, in the tints it uses, kept pale enough that the procedure
/// drawn over them stays the thing the eye goes to.
/// The tint for water, and what counts as water: ground the elevation model puts at sea
/// level. An airport at or below sea level would have its own ground painted blue, so the
/// sea is only drawn where the airport stands clear of it — but only just clear is
/// enough, and has to be, because a coastal airport is the one whose chart most needs its
/// water. Kennedy stands at 13 ft with Jamaica Bay on three sides.
const WATER_TINT: (f32, f32, f32) = (0.80, 0.90, 0.95);
const WATER_FT: f64 = 1.0;
const WATER_NEEDS_FIELD_FT: f64 = 5.0;

const TERRAIN_BANDS: [(f64, (f32, f32, f32)); 5] = [
    (656.0, (0.93, 0.93, 0.88)),
    (1312.0, (0.95, 0.89, 0.75)),
    (1969.0, (0.91, 0.80, 0.60)),
    (2625.0, (0.85, 0.68, 0.46)),
    (f64::MAX, (0.76, 0.55, 0.36)),
];

/// The tint for a height, or nothing where the ground is below the lowest band.
fn terrain_tint(height_ft: f64) -> Option<(f32, f32, f32)> {
    if height_ft < TERRAIN_BANDS[0].0 {
        return None;
    }
    TERRAIN_BANDS.iter().find(|(top, _)| height_ft < *top).map(|(_, tint)| *tint)
}

/// Heights tinted in bands, as a terrain picture rather than contours: quick to read and
/// honest about what the model can say.
fn draw_terrain(c: &mut Content, patch: &Patch, v: &View, field_ft: f64) {
    let cw = (patch.step_lon / v.deg_w) as f32 * v.w + 0.4;
    let ch = (patch.step_lat / v.deg_h) as f32 * v.h + 0.4;
    for row in 0..patch.height {
        for col in 0..patch.width {
            let ht = patch.at(row, col);
            if !ht.is_finite() {
                continue;
            }
            let height = ht as f64 / 0.3048;
            // Sea first, then the ground above it. Ground no higher than the airport is
            // not worth tinting whatever its height above the sea: an airport on a
            // plateau would otherwise sit in a wash of colour.
            let tint = if height <= WATER_FT && field_ft >= WATER_NEEDS_FIELD_FT {
                Some(WATER_TINT)
            } else {
                terrain_tint(height).filter(|_| height > field_ft + 250.0)
            };
            let Some((r, g, b)) = tint else { continue };
            let (lat, lon) = patch.position(row, col);
            let (px, py) = v.at(lat, lon);
            if !v.inside((px, py), 0.0) {
                continue;
            }
            c.set_fill_rgb(r, g, b);
            c.rect(px, py - ch, cw, ch);
            c.fill_nonzero();
        }
    }
}

/// The airport as we built it: pavement first, then water and buildings, then the
/// runways on top in a darker grey so they read at chart scale.
fn draw_airport(c: &mut Content, dir: &FsPath, v: &View) -> bool {
    use geo_types::Geometry;
    let mut drawn = false;
    for (layer, grey) in [("apronelement", 0.84), ("taxiwayelement", 0.78), ("water", 0.90), ("verticalpolygonalstructure", 0.66), ("runwayelement", 0.22)] {
        let feats = crate::output::chart::load_layer(dir, layer);
        if feats.is_empty() {
            continue;
        }
        c.set_fill_gray(grey);
        let mut any = false;
        for feat in &feats {
            let polys: Vec<&geo_types::Polygon<f64>> = match &feat.geom {
                Geometry::Polygon(p) => vec![p],
                Geometry::MultiPolygon(m) => m.0.iter().collect(),
                _ => vec![],
            };
            for p in polys {
                let pts: Vec<(f32, f32)> = p.exterior().0.iter().map(|co| v.at(co.y, co.x)).collect();
                if pts.len() < 3 || !pts.iter().any(|p| v.inside(*p, 20.0)) {
                    continue;
                }
                for (i, (px, py)) in pts.iter().enumerate() {
                    if i == 0 {
                        c.move_to(*px, *py);
                    } else {
                        c.line_to(*px, *py);
                    }
                }
                c.close_path();
                any = true;
            }
        }
        if any {
            c.fill_even_odd();
            drawn = true;
        } else {
            c.end_path();
        }
    }
    drawn
}

/// A fix, drawn where it actually is. Charts mark the final approach fix differently
/// from the rest, so it can be picked out at a glance.
fn draw_fix(c: &mut Content, font: Name, bold: Name, v: &View, leg: &Leg, role: Option<&str>, floor_ft: f64, taken: &mut Taken, dme: Option<(&str, (f64, f64))>) {
    let is_faf = role == Some("FAF");
    // The missed approach point is marked where it falls, which is often the runway.
    let (Some(lat), Some(lon)) = (leg.lat, leg.lon) else { return };
    if leg.fix.is_empty() || leg.fix.starts_with("RW") || !taken.first_time(&leg.fix) {
        return;
    }
    let (px, py) = v.at(lat, lon);
    if !v.inside((px, py), -6.0) {
        return;
    }
    c.set_stroke_gray(INK);
    c.set_fill_gray(INK);
    if is_faf {
        // The cross a chart puts at the final approach fix.
        for (dx, dy) in [(0.0f32, 1.0f32), (1.0, 0.0)] {
            c.move_to(px - dx * 5.0 - dy * 1.8, py - dy * 5.0 - dx * 1.8);
            c.line_to(px + dx * 5.0 - dy * 1.8, py + dy * 5.0 - dx * 1.8);
            c.line_to(px + dx * 5.0 + dy * 1.8, py + dy * 5.0 + dx * 1.8);
            c.line_to(px - dx * 5.0 + dy * 1.8, py - dy * 5.0 + dx * 1.8);
            c.close_path();
        }
        c.fill_nonzero();
    } else {
        // A solid triangle, the way a named fix is drawn.
        c.move_to(px, py + 4.0);
        c.line_to(px + 3.6, py - 2.6);
        c.line_to(px - 3.6, py - 2.6);
        c.close_path();
        c.fill_nonzero();
    }
    if !taken.label(c, bold, 7.0, px + 6.0, py + 1.0, &leg.fix, INK) {
        return;
    }
    let mut row = py - 7.0;
    if let Some(a) = leg.altitude_ft.filter(|a| *a > floor_ft) {
        let rule = match leg.altitude_rule {
            crate::sources::msfs::procedures::AltitudeRule::AtOrAbove => "+",
            crate::sources::msfs::procedures::AltitudeRule::AtOrBelow => "-",
            _ => "",
        };
        taken.label(c, font, 6.5, px + 6.0, row, &format!("{a:.0}{rule}"), 0.28);
        row -= 7.0;
    }
    // How the fix is defined, which is how a chart names it: "7 DME FUN".
    //
    // Only where the fix's own name confirms it. The distance in the data is measured
    // from the beacon, and at some airports the published chart measures from somewhere
    // else along the same installation, which puts the two a mile or two apart. A fix
    // called FUN7 settles it; one called JOBBS does not, and is left unlabelled rather
    // than labelled with a figure that disagrees with the chart it is meant to match.
    // A fix on an ILS is named by how far it is from the localiser's DME: EBBEE is
    // D6.1 IJFK, and that is how a crew checks it.
    if let (Some((ident, (dlat, dlon))), Some((flat, flon))) = (dme, leg.lat.zip(leg.lon)) {
        let nm = ((flat - dlat) * 60.0).hypot((flon - dlon) * 60.0 * dlat.to_radians().cos().max(0.05));
        if nm > 0.4 {
            taken.label(c, font, 6.0, px + 6.0, row, &format!("D{nm:.1} {ident}"), 0.4);
            row -= 7.0;
        }
    } else if let (Some(rho), false) = (leg.rho_nm, leg.navaid.is_empty()) {
        if let Some(digits) = leg.fix.strip_prefix(leg.navaid.as_str()) {
            let named: Option<f64> = digits.parse::<f64>().ok().map(|v| if digits.len() > 2 { v / 10.0 } else { v });
            if named.map(|n| (n - rho).abs() < 0.3).unwrap_or(false) {
                taken.label(c, font, 6.0, px + 6.0, row, &format!("{rho:.1} DME {}", leg.navaid), 0.4);
                row -= 7.0;
            }
        }
    }
    if let Some(part) = role {
        taken.label(c, bold, 6.0, px + 6.0, row, part, 0.15);
    }
}

/// What part each fix plays, as the data marks it.
fn fix_roles(finals: &[&Leg]) -> Vec<Option<&'static str>> {
    finals.iter().map(|leg| leg.role.map(FixRole::label)).collect()
}

/// A run of legs as a line on the map, through the fixes that have a position.
fn draw_track(c: &mut Content, v: &View, legs: &[&Leg], start: Option<(f64, f64)>, weight: f32, dashed: bool, grey: f32) -> Vec<(f32, f32)> {
    let mut pts: Vec<(f32, f32)> = Vec::new();
    let mut last_ll: Option<(f64, f64)> = start;
    if let Some((lat, lon)) = start {
        pts.push(v.at(lat, lon));
    }
    for leg in legs {
        if let (Some(lat), Some(lon)) = (leg.lat, leg.lon) {
            // An arc leg is flown round its navaid at a fixed distance, not straight.
            match (leg.path.as_str(), leg.rho_nm, last_ll) {
                ("AF", Some(radius), Some(from)) => {
                    for p in arc_points(from, (lat, lon), radius, leg.turn).into_iter().skip(1) {
                        pts.push(v.at(p.0, p.1));
                    }
                }
                _ => pts.push(v.at(lat, lon)),
            }
            last_ll = Some((lat, lon));
        }
    }
    if pts.len() < 2 {
        return pts;
    }
    c.save_state();
    if dashed {
        c.set_dash_pattern([5.0, 3.0], 0.0);
    }
    c.set_stroke_gray(grey);
    c.set_line_width(weight);
    for (i, (px, py)) in pts.iter().enumerate() {
        if i == 0 {
            c.move_to(*px, *py);
        } else {
            c.line_to(*px, *py);
        }
    }
    c.stroke();
    c.restore_state();
    pts
}

/// An arrowhead at the end of a track, pointing the way it is flown.
fn arrow_head(c: &mut Content, from: (f32, f32), to: (f32, f32), grey: f32) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let (nx, ny) = (-uy, ux);
    c.set_fill_gray(grey);
    c.move_to(to.0, to.1);
    c.line_to(to.0 - ux * 7.0 + nx * 3.0, to.1 - uy * 7.0 + ny * 3.0);
    c.line_to(to.0 - ux * 7.0 - nx * 3.0, to.1 - uy * 7.0 - ny * 3.0);
    c.close_path();
    c.fill_nonzero();
}

/// The holding patterns a procedure ends in, drawn as the racetrack a chart draws.
fn draw_holds(c: &mut Content, font: Name, v: &View, legs: &[&Leg]) {
    for leg in legs {
        if !matches!(leg.path.as_str(), "HM" | "HA" | "HF") {
            continue;
        }
        let (Some(lat), Some(lon)) = (leg.lat, leg.lon) else { continue };
        let Some(inbound) = leg.course_deg else { continue };
        let leg_nm = leg.distance_m.map(|m| m / 1852.0).filter(|nm| *nm > 0.3 && *nm < 12.0).unwrap_or(4.0);
        let pts: Vec<(f32, f32)> = hold_points((lat, lon), inbound, leg.turn, leg_nm).into_iter().map(|p| v.at(p.0, p.1)).collect();
        if pts.iter().all(|p| !v.inside(*p, 40.0)) {
            continue;
        }
        c.set_stroke_gray(INK);
        c.set_line_width(1.1);
        for (i, (px, py)) in pts.iter().enumerate() {
            if i == 0 {
                c.move_to(*px, *py);
            } else {
                c.line_to(*px, *py);
            }
        }
        c.stroke();
        if let Some(alt) = leg.altitude_ft {
            let p = v.at(lat, lon);
            label(c, font, 6.5, p.0 + 6.0, p.1 - 14.0, &format!("{alt:.0}"), 0.3);
        }
    }
}

/// The localiser's beam, drawn the way a chart draws it: a long narrow wedge running
/// back from the threshold along the course, widening as it goes.
///
/// It is what tells a reader at a glance which way the approach is flown and how far the
/// guidance reaches. The width is the real one — a localiser is held to about two and a
/// half degrees either side of the centreline — so the wedge is honest about how much
/// room there is out at ten miles.
fn draw_feather(c: &mut Content, v: &View, threshold: (f64, f64), track_deg: f64, length_nm: f64) {
    const HALF_ANGLE_DEG: f64 = 2.5;
    let back = (track_deg + 180.0).to_radians();
    let cos = threshold.0.to_radians().cos().max(0.05);
    let along = |nm: f64, across_nm: f64| {
        let (sin, cosb) = (back.sin(), back.cos());
        // Out along the reciprocal of the track, then to the side of it.
        let north = nm * cosb - across_nm * sin;
        let east = nm * sin + across_nm * cosb;
        (threshold.0 + north / 60.0, threshold.1 + east / 60.0 / cos)
    };
    let half = length_nm * HALF_ANGLE_DEG.to_radians().tan();
    let tip = v.at(threshold.0, threshold.1);
    let (left, right) = (along(length_nm, -half), along(length_nm, half));
    let (lx, ly) = v.at(left.0, left.1);
    let (rx, ry) = v.at(right.0, right.1);
    c.save_state();
    c.set_fill_gray(0.88);
    c.move_to(tip.0, tip.1);
    c.line_to(lx, ly);
    c.line_to(rx, ry);
    c.close_path();
    c.fill_nonzero();
    c.restore_state();
    line(c, tip.0, tip.1, lx, ly, 0.5, 0.55);
    line(c, tip.0, tip.1, rx, ry, 0.5, 0.55);
}

/// The marker beacons on the approach: the oval a chart draws across the course, with the
/// two letters that say which it is.
fn draw_markers(c: &mut Content, font: Name, v: &View, markers: &[(crate::sources::navdata::Marker, f64, f64)], track_deg: f64, taken: &mut Taken) {
    for (kind, lat, lon) in markers {
        let (px, py) = v.at(*lat, *lon);
        if !v.inside((px, py), 6.0) {
            continue;
        }
        // An ellipse lying across the course, which is how it is drawn and which says
        // where the beam crosses.
        let a = (track_deg as f32 + 90.0).to_radians();
        let (dx, dy) = (a.sin() * 5.0, a.cos() * 5.0);
        c.set_fill_gray(0.15);
        c.move_to(px + dx, py + dy);
        c.line_to(px + dy * 0.45, py - dx * 0.45);
        c.line_to(px - dx, py - dy);
        c.line_to(px - dy * 0.45, py + dx * 0.45);
        c.close_path();
        c.fill_nonzero();
        taken.label(c, font, 6.0, px + 7.0, py - 2.0, kind.label(), 0.15);
    }
}

/// The beacons in the piece of country the chart covers.
///
/// No approach chart is without them: they are how a reader knows where they are, and
/// half the procedures in the world are written against one. A VOR takes the compass
/// rose's hexagon, an NDB a ring of dots, and each carries its name and its frequency the
/// way a chart prints them.
fn draw_navaids(c: &mut Content, font: Name, bold: Name, v: &View, navaids: &[Beacon], wanted: &[String], taken: &mut Taken) {
    let mut drawn = 0;
    for n in navaids {
        let ident = &n.ident;
        let named = wanted.iter().any(|w| w == ident);
        let (px, py) = v.at(n.lat, n.lon);
        if !v.inside((px, py), 14.0) {
            continue;
        }
        // The ones the procedure names are always drawn; the rest fill the country in,
        // and only so far as they do not crowd it.
        if !named {
            if drawn >= 5 {
                continue;
            }
            drawn += 1;
        }
        let grey = if named { INK } else { 0.35 };
        match n.kind {
            NavaidKind::Vor => {
                // A hexagon, point up, as every chart in the world draws a VOR.
                let r = 5.0f32;
                for i in 0..6 {
                    let a = (i as f32 * 60.0 - 90.0).to_radians();
                    let (x, y) = (px + r * a.cos(), py + r * a.sin());
                    if i == 0 {
                        c.move_to(x, y);
                    } else {
                        c.line_to(x, y);
                    }
                }
                c.close_path();
                c.set_fill_gray(1.0);
                c.fill_nonzero();
                for i in 0..6 {
                    let a = (i as f32 * 60.0 - 90.0).to_radians();
                    let b = ((i + 1) as f32 * 60.0 - 90.0).to_radians();
                    line(c, px + r * a.cos(), py + r * a.sin(), px + r * b.cos(), py + r * b.sin(), 0.9, grey);
                }
                c.set_fill_gray(grey);
                circle(c, px, py, 1.1);
                c.fill_nonzero();
            }
            NavaidKind::Ndb => {
                // A ring of dots, which is how a chart draws a beacon you can only home to.
                for i in 0..10 {
                    let a = (i as f32 * 36.0).to_radians();
                    c.set_fill_gray(grey);
                    circle(c, px + 5.0 * a.cos(), py + 5.0 * a.sin(), 0.7);
                    c.fill_nonzero();
                }
                c.set_fill_gray(grey);
                circle(c, px, py, 1.3);
                c.fill_nonzero();
            }
        }
        // The name over the frequency and the ident, in the little box a chart puts them
        // in. A beacon without its frequency is only half of one, and without its name a
        // reader has to know the country by heart.
        let printed = match n.kind {
            NavaidKind::Ndb => format!("{:.0}", n.frequency),
            NavaidKind::Vor => format!("{:.2}", n.frequency),
        };
        let size = if named { 7.0 } else { 6.0 };
        let line = format!("{printed}  {ident}");
        let bw = text_width(bold, size, &line).max(text_width(font, size - 1.0, &n.name)) + 10.0;
        let bh = if n.name.is_empty() { 11.0 } else { 18.0 };
        if !taken.first_time(ident) {
            continue;
        }
        // Beside the beacon if there is room, and if not then the other side, or above,
        // or below. A box dropped on the runway is worse than one a little out of the way.
        let places = [(px + 8.0, py - bh / 2.0), (px - 8.0 - bw, py - bh / 2.0), (px - bw / 2.0, py + 9.0), (px - bw / 2.0, py - 9.0 - bh)];
        let Some(&(bx, by)) = places
            .iter()
            .find(|(bx, by)| taken.free(*bx, *by, bw, bh) && v.inside((*bx, *by), 0.0) && v.inside((bx + bw, by + bh), 0.0))
        else {
            continue;
        };
        taken.reserve(bx, by, bw, bh);
        fill_box(c, bx, by, bw, bh, 1.0);
        box_outline(c, bx, by, bw, bh, 0.6, grey);
        if !n.name.is_empty() {
            text(c, font, size - 1.0, bx + 4.0, by + bh - 7.0, &n.name, 0.35);
        }
        text(c, bold, size, bx + 4.0, by + 3.0, &line, grey);
    }
}

/// Obstacles, as the spike a chart uses, with the top above sea level beside it. The one
/// that set the minimum is drawn heavier.
fn draw_obstacles(c: &mut Content, font: Name, bold: Name, v: &View, obstacles: &[Obstacle], floor_ft: f64, controlling_top: Option<f64>) {
    // Tallest first, one to a square of paper, so a forest of masts does not turn into a
    // wall of numbers. Anything that cannot reach the approach is left off.
    let mut taken: Vec<(f32, f32)> = Vec::new();
    let mut drawn = 0;
    for o in obstacles {
        let Some(top) = o.top_ft else { continue };
        let controls_it = controlling_top.map(|t| (t - top).abs() < 0.5).unwrap_or(false);
        if top < floor_ft && !controls_it {
            continue;
        }
        let (px, py) = v.at(o.lat, o.lon);
        if !v.inside((px, py), -4.0) {
            continue;
        }
        if px > v.x + v.w - 40.0 {
            continue;
        }
        if !controls_it && taken.iter().any(|(tx, ty)| (tx - px).abs() < 34.0 && (ty - py).abs() < 13.0) {
            continue;
        }
        taken.push((px, py));
        drawn += 1;
        // A chart marks the few that matter, not every mast in the county. The file holds
        // thousands around a big city and they would bury the procedure.
        if drawn > 10 {
            break;
        }
        let controls = controls_it;
        let h = if controls { 11.0 } else { 8.0 };
        c.set_fill_gray(if controls { 0.0 } else { 0.25 });
        c.move_to(px, py + h);
        c.line_to(px + 3.0, py);
        c.line_to(px - 3.0, py);
        c.close_path();
        c.fill_nonzero();
        let f = if controls { bold } else { font };
        label(c, f, if controls { 7.5 } else { 6.5 }, px + 5.0, py + h - 6.0, &format!("{top:.0}"), if controls { 0.0 } else { 0.3 });
    }
}

/// A latitude or longitude the way a chart rules it: whole degrees and minutes.
fn degrees_minutes(value: f64, is_latitude: bool) -> String {
    let hand = match (is_latitude, value >= 0.0) {
        (true, true) => 'N',
        (true, false) => 'S',
        (false, true) => 'E',
        (false, false) => 'W',
    };
    let value = value.abs();
    let degrees = value.floor();
    let minutes = ((value - degrees) * 60.0).round();
    let (degrees, minutes) = if minutes >= 60.0 { (degrees + 1.0, 0.0) } else { (degrees, minutes) };
    format!("{hand}{degrees:.0} {minutes:02.0}")
}

/// North arrow, scale bar and the minimum safe altitude ring: the furniture that tells
/// you how to read the map.
#[allow(clippy::too_many_arguments)]
fn draw_furniture(
    c: &mut Content,
    font: Name,
    bold: Name,
    v: &View,
    msa_ft: Option<f64>,
    sectors: &[crate::minima::Sector],
    variation_deg: Option<f64>,
    runway_deg: Option<f64>,
    has_water: bool,
    highest_ft: f64,
    caption: &str,
) {
    // Degrees and minutes along the edges, as a chart rules them.
    let step = (if v.span_nm() > 24.0 { 20.0 } else if v.span_nm() > 10.0 { 10.0 } else { 5.0 }) / 60.0;
    let mut lat = (v.north / step).floor() * step;
    while lat > v.north - v.deg_h {
        let (_, py) = v.at(lat, v.west);
        line(c, v.x, py, v.x + 6.0, py, 0.7, 0.45);
        line(c, v.x + v.w - 6.0, py, v.x + v.w, py, 0.7, 0.45);
        text(c, font, 5.0, v.x + 8.0, py + 1.5, &degrees_minutes(lat, true), 0.45);
        lat -= step;
    }
    let mut lon = (v.west / step).ceil() * step;
    while lon < v.west + v.deg_w {
        let (px, _) = v.at(v.north, lon);
        line(c, px, v.y, px, v.y + 6.0, 0.7, 0.45);
        line(c, px, v.y + v.h - 6.0, px, v.y + v.h, 0.7, 0.45);
        text_centred(c, font, 5.0, px, v.y + v.h - 12.0, &degrees_minutes(lon, false), 0.45);
        lon += step;
    }

    // The compass rose, top left inside the box: true north, the ticks of the card, the
    // runway lying across it, and how far magnetic north is from true.
    let r = 21.0;
    let (nx, ny) = (v.x + r + 8.0, v.y + v.h - r - 10.0);
    fill_box(c, nx - r - 5.0, ny - r - 13.0, 2.0 * r + 10.0, 2.0 * r + 20.0, 1.0);
    c.set_stroke_gray(0.35);
    c.set_line_width(0.7);
    circle(c, nx, ny, r);
    c.stroke();
    for step in 0..12 {
        let a = (step as f32 * 30.0).to_radians();
        let (sx, sy) = (a.sin(), a.cos());
        let inner = if step % 3 == 0 { r - 6.0 } else { r - 3.0 };
        line(c, nx + sx * inner, ny + sy * inner, nx + sx * r, ny + sy * r, 0.6, 0.35);
    }
    // The runway, drawn across the rose the way it lies on the ground.
    if let Some(deg) = runway_deg {
        let a = (deg as f32).to_radians();
        let (sx, sy) = (a.sin(), a.cos());
        line(c, nx - sx * (r - 5.0), ny - sy * (r - 5.0), nx + sx * (r - 5.0), ny + sy * (r - 5.0), 2.0, INK);
    }
    // True north.
    c.set_fill_gray(INK);
    c.move_to(nx, ny + r + 6.0);
    c.line_to(nx + 3.5, ny + r - 4.0);
    c.line_to(nx - 3.5, ny + r - 4.0);
    c.close_path();
    c.fill_nonzero();
    text_centred(c, bold, 7.0, nx, ny - r - 10.0, "N", INK);
    if let Some(var) = variation_deg {
        let hand = if var >= 0.0 { "E" } else { "W" };
        text_centred(c, font, 6.0, nx, ny - r - 19.0, &format!("VAR {:.1}{hand}", var.abs()), 0.35);
    }

    // What the tints mean, above the scale bar. Only the bands the ground here reaches:
    // a key running to 2,625 ft over an airport whose highest ground is sixty tells the
    // reader nothing and makes the page look like it failed to draw.
    let swatch = 8.0;
    let bands = TERRAIN_BANDS.iter().take_while(|(top, _)| *top < highest_ft).count() + 1;
    let bands = bands.min(TERRAIN_BANDS.len());
    let rows = bands + usize::from(has_water);
    if rows == 0 {
        return;
    }
    let key_h = rows as f32 * swatch + 12.0;
    let (kx, ky) = (v.x + 10.0, v.y + 30.0);
    fill_box(c, kx - 4.0, ky - 4.0, 54.0, key_h, 1.0);
    text(c, font, 5.5, kx, ky + key_h - 12.0, "ELEVATION", 0.4);
    if has_water {
        let (r, g, b) = WATER_TINT;
        c.set_fill_rgb(r, g, b);
        c.rect(kx, ky, 13.0, swatch - 1.5);
        c.fill_nonzero();
        box_outline(c, kx, ky, 13.0, swatch - 1.5, 0.3, 0.6);
        text(c, font, 5.5, kx + 16.0, ky + 1.0, "WATER", 0.35);
    }
    let ky = ky + if has_water { swatch } else { 0.0 };
    for (i, (top, (r, g, b))) in TERRAIN_BANDS.iter().take(bands).enumerate() {
        let row = ky + (bands - 1 - i) as f32 * swatch;
        c.set_fill_rgb(*r, *g, *b);
        c.rect(kx, row, 13.0, swatch - 1.5);
        c.fill_nonzero();
        box_outline(c, kx, row, 13.0, swatch - 1.5, 0.3, 0.6);
        let label = if *top == f64::MAX || i + 1 == bands { format!("{:.0}+", if i == 0 { 0.0 } else { TERRAIN_BANDS[i - 1].0 }) } else { format!("{top:.0}") };
        text(c, font, 5.5, kx + 16.0, row + 1.0, &label, 0.35);
    }

    // Scale bar, bottom left.
    let px_nm = v.px_per_nm();
    let step_nm = if v.span_nm() > 16.0 { 5.0 } else { 2.0 };
    let bar = step_nm as f32 * px_nm;
    let (bx, by) = (v.x + 12.0, v.y + 16.0);
    fill_box(c, bx - 4.0, by - 4.0, bar + 30.0, 16.0, 1.0);
    line(c, bx, by, bx + bar, by, 1.2, INK);
    for i in 0..=1 {
        let x = bx + i as f32 * bar;
        line(c, x, by - 3.0, x, by + 3.0, 1.2, INK);
    }
    text(c, font, 6.5, bx + bar + 3.0, by - 2.0, &format!("{step_nm:.0} NM"), INK);

    // The safe altitude ring, divided into the quadrants a chart divides it into.
    let (cx, cy) = (v.x + v.w - 42.0, v.y + v.h - 44.0);
    let r = 30.0;
    if !sectors.is_empty() {
        fill_box(c, cx - r - 12.0, cy - r - 20.0, 2.0 * r + 24.0, 2.0 * r + 30.0, 1.0);
        c.set_stroke_gray(INK);
        c.set_line_width(0.9);
        circle(c, cx, cy, r);
        c.stroke();
        for s in sectors {
            // The line that starts this sector, and its figure in the middle of it.
            let start = (s.from_deg as f32).to_radians();
            line(c, cx, cy, cx + start.sin() * r, cy + start.cos() * r, 0.6, 0.4);
            let mut sweep = s.to_deg - s.from_deg;
            if sweep <= 0.0 {
                sweep += 360.0;
            }
            let middle = ((s.from_deg + sweep / 2.0) as f32).to_radians();
            let (lx, ly) = (cx + middle.sin() * r * 0.55, cy + middle.cos() * r * 0.55);
            text_centred(c, bold, 7.5, lx, ly - 2.5, &format!("{:.0}", s.altitude_ft), INK);
            // The bearing the sector starts at, just outside the ring.
            let (bx, by) = (cx + start.sin() * (r + 6.0), cy + start.cos() * (r + 6.0));
            text_centred(c, font, 5.0, bx, by - 2.0, &format!("{:03.0}", s.from_deg), 0.45);
        }
        text_centred(c, font, 5.5, cx, cy - r - 16.0, caption, 0.3);
    } else if let Some(msa) = msa_ft {
        fill_box(c, cx - r - 12.0, cy - r - 20.0, 2.0 * r + 24.0, 2.0 * r + 30.0, 1.0);
        c.set_stroke_gray(INK);
        c.set_line_width(0.9);
        circle(c, cx, cy, r * 0.7);
        c.stroke();
        text_centred(c, bold, 10.0, cx, cy - 3.0, &format!("{msa:.0}"), INK);
        text_centred(c, font, 5.5, cx, cy - r - 16.0, caption, 0.3);
    }
}

/// How wide the inset is, in miles.
const INSET_NM: f64 = 2.2;

/// The airport again, close up.
///
/// The plan view has to hold an approach that runs twenty miles out, at which scale the
/// airport is a few millimetres of grey. A chart answers that with an inset: the same
/// airport at a scale where the runways are runways, in the corner where there is
/// nothing else to show.
fn draw_inset(c: &mut Content, font: Name, bold: Name, ch: &Chart, x: f32, y: f32, w: f32, h: f32) {
    let centre = ch.threshold.unwrap_or((ch.airport.lat, ch.airport.lon));
    let cos = centre.0.to_radians().cos().max(0.05);
    let aspect = (w / h) as f64;
    let half = INSET_NM / 2.0;
    let (half_w, half_h) = if aspect >= 1.0 { (half * aspect, half) } else { (half, half / aspect) };
    let v = View {
        west: centre.1 - half_w / 60.0 / cos,
        north: centre.0 + half_h / 60.0,
        deg_w: half_w * 2.0 / 60.0 / cos,
        deg_h: half_h * 2.0 / 60.0,
        x,
        y,
        w,
        h,
    };
    fill_box(c, x, y, w, h, 1.0);
    c.save_state();
    c.rect(x, y, w, h);
    c.clip_nonzero();
    c.end_path();
    draw_terrain(c, ch.patch, &v, ch.field_elev_ft);
    if let Some(dir) = ch.airport_dir {
        draw_airport(c, dir, &v);
    }
    // The landing runway, and the threshold the approach ends at.
    if let Some((near, far)) = ch.runway_ends {
        let (nx, ny) = v.at(near.0, near.1);
        let (fx, fy) = v.at(far.0, far.1);
        line(c, nx, ny, fx, fy, 6.0, 1.0);
        line(c, nx, ny, fx, fy, 3.4, INK);
        // Which end is being landed on, and what it is called.
        c.set_fill_gray(INK);
        circle(c, nx, ny, 2.6);
        c.fill_nonzero();
        label(c, bold, 6.5, nx + 5.0, ny - 8.0, &ch.procedure.runway, INK);
        // What the runway is, and what it has to guide an approach by.
        let mut about = Vec::new();
        if let Some((length, width)) = ch.runway_size {
            about.push(format!("{length:.0}m x {width:.0}m"));
        }
        if !ch.runway_lighting.is_empty() {
            about.push(ch.runway_lighting.join(" "));
        }
        if !about.is_empty() {
            label(c, font, 5.5, x + 4.0, y + 4.0, &about.join("   "), 0.3);
        }
    }
    // The last of the final, so the inset joins on to the picture above it.
    let finals = final_legs(ch.procedure);
    let mut track: Vec<&Leg> = finals.iter().rev().take(2).rev().copied().collect();
    track.retain(|l| l.lat.is_some());
    if !track.is_empty() {
        draw_track(c, &v, &track, None, 1.6, false, INK);
    }
    c.restore_state();
    box_outline(c, x, y, w, h, 1.0, INK);
    fill_box(c, x + 1.0, y + h - 11.0, w - 2.0, 10.0, 1.0);
    // What it is across, not what it is tall: the box is wider than it is high.
    text(c, bold, 6.0, x + 4.0, y + h - 8.5, &format!("{}  {:.1} NM ACROSS", ch.airport.icao, v.span_nm()), INK);
}

/// The plan view: terrain, the airport, the procedure and what stands up under it.
#[allow(clippy::too_many_arguments)]
fn draw_plan(c: &mut Content, font: Name, bold: Name, ch: &Chart, x: f32, y: f32, w: f32, h: f32, est: &Estimate, track_deg: f64) -> View {
    // Everything that will be drawn decides how far out the window reaches.
    let finals = final_legs(ch.procedure);
    let missed = part_legs(ch.procedure, "missed");
    let mut points: Vec<(f64, f64)> = Vec::new();
    let _ = &missed;
    for l in &finals {
        if let (Some(a), Some(o)) = (l.lat, l.lon) {
            points.push((a, o));
        }
    }
    let centre = ch.threshold.unwrap_or((ch.airport.lat, ch.airport.lon));
    let v = View::around((ch.airport.lat, ch.airport.lon), track_deg, &points, x, y, w, h);

    // Everything the map draws stays inside the map.
    c.save_state();
    c.rect(x, y, w, h);
    c.clip_nonzero();
    c.end_path();
    if let Some(wide) = ch.wide {
        draw_terrain(c, wide, &v, ch.field_elev_ft);
    }
    draw_terrain(c, ch.patch, &v, ch.field_elev_ft);
    let have_airport = ch.airport_dir.map(|d| draw_airport(c, d, &v)).unwrap_or(false);
    // The runway itself. At eight or sixteen miles across, the pavement we build is a
    // couple of millimetres of grey; a chart still has to show which strip is being
    // landed on, so it is drawn as a mark over the top with a white casing.
    match ch.runway_ends {
        Some((near, far)) => {
            let (nx, ny) = v.at(near.0, near.1);
            let (fx, fy) = v.at(far.0, far.1);
            line(c, nx, ny, fx, fy, 5.0, 1.0);
            line(c, nx, ny, fx, fy, 2.6, INK);
        }
        None if !have_airport => {
            // Nothing built for this airport: a plain mark, to the runway's direction.
            let (cx, cy) = v.at(centre.0, centre.1);
            let back = (track_deg + 180.0).to_radians();
            let half = 0.7 * v.px_per_nm();
            line(c, cx - back.sin() as f32 * half, cy - back.cos() as f32 * half, cx + back.sin() as f32 * half, cy + back.cos() as f32 * half, 3.0, INK);
        }
        None => {}
    }

    // Every hold the procedure ends in, drawn where it is flown.
    let mut holds: Vec<&Leg> = Vec::new();
    // The final is labelled first, so where labels crowd it is the arrival that gives way.
    let mut taken = Taken::default();
    // The runway, and the corner the safe-altitude ring sits in, are already on the page.
    if let Some((near, far)) = ch.runway_ends {
        let (nx, ny) = v.at(near.0, near.1);
        let (fx, fy) = v.at(far.0, far.1);
        taken.reserve(nx.min(fx) - 6.0, ny.min(fy) - 6.0, (fx - nx).abs() + 12.0, (fy - ny).abs() + 12.0);
    }
    taken.reserve(v.x + v.w - 120.0, v.y + v.h - 100.0, 120.0, 100.0);
    // What the distances on the fixes are measured from.
    let dme = ch.ils.and_then(|i| i.dme.map(|at| (i.ident.as_str(), at)));
    // The beacons the procedure is written against, which are drawn whether or not they
    // are near: the missed approach goes to one, and the reader has to see it.
    let wanted: Vec<String> = ch
        .procedure
        .transitions
        .iter()
        .flat_map(|t| t.legs.iter())
        .flat_map(|l| [l.navaid.clone(), l.fix.clone()])
        .filter(|s| !s.is_empty())
        .collect();
    draw_navaids(c, font, bold, &v, ch.navaids, &wanted, &mut taken);
    // The arrival that feeds the approach, lightest of all.
    if let Some(star) = ch.star {
        for t in &star.transitions {
            let legs: Vec<&Leg> = t.legs.iter().collect();
            let pts = draw_track(c, &v, &legs, None, 0.8, true, 0.45);
            if let Some(last) = pts.last() {
                if pts.len() > 1 {
                    arrow_head(c, pts[pts.len() - 2], *last, 0.45);
                }
            }
            for leg in &legs {
                draw_fix(c, font, bold, &v, leg, None, ch.tdze_ft, &mut taken, dme);
            }
            holds.extend(legs.iter().copied());
        }
    }

    // The ways in to the approach, thin, each starting at an initial approach fix.
    for t in feeder_transitions(ch.procedure) {
        let legs: Vec<&Leg> = t.legs.iter().collect();
        draw_track(c, &v, &legs, None, 0.9, false, 0.35);
        for (i, leg) in legs.iter().enumerate() {
            draw_fix(c, font, bold, &v, leg, (i == 0).then_some("IAF"), ch.tdze_ft, &mut taken, dme);
        }
        holds.extend(legs.iter().copied());
    }

    // The localiser's beam, under the track it guides.
    if let (Some(thr), true) = (ch.threshold, ch.ils.is_some()) {
        let reach = finals
            .iter()
            .filter_map(|l| l.lat.zip(l.lon))
            .map(|(lat, lon)| ((lat - thr.0) * 60.0).hypot((lon - thr.1) * 60.0 * thr.0.to_radians().cos().max(0.05)))
            .fold(0.0f64, f64::max);
        if reach > 1.0 {
            draw_feather(c, &v, thr, track_deg, reach);
        }
    }
    // The final, heavy, ending at the threshold.
    let end = if ch.threshold.is_some() { Some(centre) } else { None };
    let mut track_pts = draw_track(c, &v, &finals, None, 2.0, false, INK);
    if let Some((lat, lon)) = end {
        let p = v.at(lat, lon);
        if let Some(prev) = track_pts.last().copied() {
            line(c, prev.0, prev.1, p.0, p.1, 2.0, INK);
        }
        track_pts.push(p);
    }
    let roles = fix_roles(&finals);
    for (leg, role) in finals.iter().zip(roles) {
        draw_fix(c, font, bold, &v, leg, role, ch.tdze_ft, &mut taken, dme);
    }
    holds.extend(finals.iter().copied());
    // The inbound course, written along the final the way a chart does, and the
    // localiser it is flown on in the box a chart puts beside it.
    if track_pts.len() >= 2 {
        let a = track_pts[0];
        let b = track_pts[track_pts.len() - 1];
        let course = ch.ils.and_then(|i| i.course_mag_deg).or(ch.course_mag_deg).unwrap_or(track_deg);
        let (mx, my) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        taken.label(c, bold, 8.5, mx + 7.0, my - 14.0, &format!("{course:03.0}"), INK);
        if let Some(ils) = ch.ils {
            let line = format!("{course:03.0}\u{b0}   {:.2}   {}", ils.frequency, ils.ident);
            let bw = text_width(bold, 7.5, &line) + 12.0;
            let (bx, by) = (mx - bw - 12.0, my - 20.0);
            if v.inside((bx, by), 0.0) && v.inside((bx + bw, by + 13.0), 0.0) {
                fill_box(c, bx, by, bw, 13.0, 1.0);
                box_outline(c, bx, by, bw, 13.0, 0.8, INK);
                text(c, bold, 7.5, bx + 6.0, by + 4.0, &line, INK);
            }
        }
    }

    // The missed approach, dashed with an arrow, as charts draw it.
    if !missed.is_empty() {
        let pts = draw_track(c, &v, &missed, ch.threshold, 1.4, true, 0.15);
        if pts.len() >= 2 {
            arrow_head(c, pts[pts.len() - 2], pts[pts.len() - 1], 0.15);
        }
        for leg in &missed {
            draw_fix(c, font, bold, &v, leg, None, ch.tdze_ft, &mut taken, dme);
        }
        holds.extend(missed.iter().copied());
    }
    if let Some(ils) = ch.ils {
        draw_markers(c, font, &v, &ils.markers, track_deg, &mut taken);
    }
    draw_holds(c, font, &v, &holds);

    draw_obstacles(c, font, bold, &v, ch.obstacles, ch.tdze_ft + 150.0, est.obstacle_top_ft.filter(|_| est.limited_by == LimitedBy::Obstacle));
    c.restore_state();
    // The airport close up, in the bottom corner, but only when the plan is too wide to
    // show it properly on its own.
    if v.span_nm() > 9.0 {
        let (iw, ih) = (140.0, 122.0);
        draw_inset(c, font, bold, ch, x + w - iw - 6.0, y + 16.0, iw, ih);
    }
    // Whether there is any sea in sight, so the key only mentions it when there is.
    let has_water = ch.field_elev_ft >= WATER_NEEDS_FIELD_FT
        && (0..ch.patch.height).step_by(4).any(|row| {
            (0..ch.patch.width).step_by(4).any(|col| {
                let h = ch.patch.at(row, col);
                h.is_finite() && (h as f64 / 0.3048) <= WATER_FT
            })
        });
    draw_furniture(c, font, bold, &v, ch.msa_ft, ch.msa_sectors, ch.variation_deg, Some(track_deg), has_water, est.highest_terrain_ft, &ch.msa_caption);
    box_outline(c, x, y, w, h, 1.2, INK);
    v
}

/// What the missed approach asks for, in a sentence, read off its legs.
fn missed_text(legs: &[&Leg]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for leg in legs {
        let piece = match leg.path.as_str() {
            "CA" | "VA" | "FA" => leg.altitude_ft.map(|a| format!("climb to {a:.0} ft")),
            "HM" | "HA" | "HF" => Some(if leg.fix.is_empty() { "hold".to_string() } else { format!("hold at {}", leg.fix) }),
            "DF" | "TF" | "CF" | "IF" => (!leg.fix.is_empty()).then(|| format!("direct {}", leg.fix)),
            "CI" | "VI" | "VM" | "FM" => leg.course_deg.map(|c| format!("track {c:03.0}")),
            _ => None,
        };
        if let Some(p) = piece {
            if !parts.contains(&p) {
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

/// How far a fix is from the threshold, in miles, from where it actually is.
fn fix_distance_nm(leg: &Leg, thr: (f64, f64)) -> Option<f64> {
    let (lat, lon) = (leg.lat?, leg.lon?);
    let dn = (lat - thr.0) * 60.0;
    let de = (lon - thr.1) * 60.0 * thr.0.to_radians().cos().max(0.05);
    Some((dn * dn + de * de).sqrt())
}

/// The descent profile: the ladder of altitudes down to the minimum, over the ground.
#[allow(clippy::too_many_arguments)]
fn draw_profile(c: &mut Content, font: Name, bold: Name, ch: &Chart, x: f32, y: f32, w: f32, h: f32, est: &Estimate, track_deg: f64) {
    box_outline(c, x, y, w, h, 1.2, INK);
    let legs = final_legs(ch.procedure);
    let alts: Vec<f64> = legs.iter().filter_map(|l| l.altitude_ft).collect();
    let top_ft = alts.iter().cloned().fold(ch.tdze_ft + 1200.0, f64::max) + 200.0;
    let base_ft = ch.tdze_ft - 200.0;
    let ground_y = y + 26.0;
    let scale_y = (h - 46.0) / (top_ft - base_ft).max(100.0) as f32;
    let at_ft = |ft: f64| ground_y + ((ft - ch.tdze_ft) as f32).max(-20.0) * scale_y;

    let thr = ch.threshold.unwrap_or((ch.airport.lat, ch.airport.lon));
    // How far out the profile runs: to the farthest fix on the final, or ten miles.
    let total_nm = legs.iter().filter_map(|l| fix_distance_nm(l, thr)).fold(9.0, f64::max);
    let (left, right) = (x + 46.0, x + w - 16.0);
    let at_nm = |nm: f64| right - (nm / total_nm) as f32 * (right - left);

    // The ground under the approach, from the terrain model.
    let back = (track_deg + 180.0).to_radians();
    let mut ground: Vec<(f32, f32)> = Vec::new();
    let steps = 60;
    for i in 0..=steps {
        let nm = total_nm * i as f64 / steps as f64;
        let (dn, de) = (nm / 60.0 * back.cos(), nm / 60.0 * back.sin() / thr.0.to_radians().cos().max(0.05));
        let Some(m) = ch.patch.height_at(thr.0 + dn, thr.1 + de) else { continue };
        ground.push((at_nm(nm), at_ft(m / 0.3048)));
    }
    if ground.len() > 2 {
        c.set_fill_gray(0.88);
        c.move_to(ground[0].0, ground_y - 12.0);
        for (px, py) in &ground {
            c.line_to(*px, py.max(ground_y - 12.0));
        }
        c.line_to(ground[ground.len() - 1].0, ground_y - 12.0);
        c.close_path();
        c.fill_nonzero();
    }
    line(c, left - 30.0, ground_y, right, ground_y, 1.0, INK);

    // The descent itself, from each fix's altitude down to the threshold crossing.
    let mut points: Vec<(f32, f32, &Leg)> = Vec::new();
    for leg in legs.iter().rev() {
        if let Some(a) = leg.altitude_ft.filter(|a| *a > ch.tdze_ft) {
            // The fix at the runway has no position of its own: it is the threshold.
            let nm = fix_distance_nm(leg, thr).unwrap_or(0.0).max(0.0);
            points.push((at_nm(nm), at_ft(a), leg));
        }
    }
    let tch = at_ft(ch.tdze_ft + 50.0);
    c.set_stroke_gray(INK);
    c.set_line_width(1.6);
    c.move_to(at_nm(0.0), tch);
    for (px, py, _) in points.iter() {
        c.line_to(*px, *py);
    }
    c.stroke();

    for (px, py, leg) in &points {
        // The altitude in a box, and the fix under the ground line, as a chart sets them.
        let s = format!("{:.0}", leg.altitude_ft.unwrap_or(0.0));
        label(c, bold, 7.5, px - text_width(bold, 7.5, &s) / 2.0, py + 4.0, &s, INK);
        if !leg.fix.is_empty() {
            text_centred(c, font, 6.5, *px, ground_y - 9.0, &leg.fix, 0.25);
            line(c, *px, ground_y - 3.0, *px, *py, 0.4, 0.5);
        }
    }
    // How far it is from each fix to the next, written between them the way a chart
    // writes it.
    let mut ordered: Vec<&(f32, f32, &Leg)> = points.iter().collect();
    ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
    for pair in ordered.windows(2) {
        let (left, right) = (pair[0], pair[1]);
        let (Some(a), Some(b)) = (fix_distance_nm(left.2, thr), fix_distance_nm(right.2, thr)) else { continue };
        let gap = (a - b).abs();
        if gap < 0.3 || right.0 - left.0 < 26.0 {
            continue;
        }
        let middle = (left.0 + right.0) / 2.0;
        label(c, font, 6.0, middle - 9.0, ground_y - 19.0, &format!("{gap:.1} NM"), 0.35);
    }

    // Glidepath angle and threshold crossing height, in the corner a chart puts them.
    label(c, font, 6.5, at_nm(0.0) - 44.0, tch + 9.0, "TCH 50", 0.3);
    if let Some(deg) = ch.glidepath_deg {
        text(c, bold, 8.0, x + 6.0, y + h - 12.0, &format!("GP {deg:.2}"), INK);
    }
    text(c, font, 6.5, x + 6.0, y + h - 21.0, &format!("{total_nm:.1} NM"), 0.35);

    // From the final approach fix to the missed approach point, and how long that takes
    // at the speeds an aeroplane flies it: a crew times the last segment, so a chart
    // always carries this.
    // From the fix marked as the final approach fix to the one marked as the missed
    // approach point, both of which the data states, measured the way it is flown: an
    // approach that passes over its beacon and carries on covers more ground than the
    // difference between the two fixes' distances from the runway.
    let faf_at = legs.iter().position(|l| l.role == Some(FixRole::Final));
    let map_at = legs.iter().position(|l| l.role == Some(FixRole::MissedApproachPoint)).unwrap_or(legs.len().saturating_sub(1));
    let segment = match faf_at {
        Some(start) if map_at >= start => {
            // The missed approach point is often the runway itself, which carries no
            // position of its own: it is the threshold.
            let where_it_is = |leg: &Leg| leg.lat.zip(leg.lon).unwrap_or(thr);
            legs[start..=map_at]
                .windows(2)
                .map(|pair| {
                    let (a, b) = (where_it_is(pair[0]), where_it_is(pair[1]));
                    let dn = (b.0 - a.0) * 60.0;
                    let de = (b.1 - a.1) * 60.0 * a.0.to_radians().cos().max(0.05);
                    dn.hypot(de)
                })
                .sum::<f64>()
        }
        _ => 0.0,
    };
    if segment > 0.5 {
        // Ground speed against what it costs: how long the last segment takes, and on a
        // glidepath how fast to come down. Both are what a chart is read for on the way
        // in, and the descent rate is the number set in the aircraft.
        const SPEEDS: [f64; 6] = [80.0, 100.0, 120.0, 140.0, 160.0, 180.0];
        let slope = ch.glidepath_deg.filter(|d| *d > 0.5);
        let rows = if slope.is_some() { 3 } else { 2 };
        let (tw, th) = (14.0 + SPEEDS.len() as f32 * 21.0, 8.0 + rows as f32 * 8.0);
        let (tx, ty) = (x + w - tw - 6.0, y + h - th - 6.0);
        fill_box(c, tx, ty, tw, th, 1.0);
        box_outline(c, tx, ty, tw, th, 0.7, RULE);
        let row_y = |n: usize| ty + th - 8.0 - n as f32 * 8.0;
        text(c, font, 5.5, tx + 3.0, row_y(0), "KT", 0.4);
        text(c, font, 5.5, tx + 3.0, row_y(1), "MIN", 0.4);
        if slope.is_some() {
            text(c, font, 5.5, tx + 3.0, row_y(2), "FPM", 0.4);
        }
        for (i, speed) in SPEEDS.iter().enumerate() {
            let cx = tx + 14.0 + i as f32 * 21.0;
            let minutes = segment / speed * 60.0;
            text(c, font, 6.0, cx, row_y(0), &format!("{speed:.0}"), 0.25);
            text(c, bold, 6.0, cx, row_y(1), &format!("{}:{:02.0}", minutes as u32, (minutes.fract() * 60.0).round()), INK);
            if let Some(deg) = slope {
                // A mile of ground at this speed, down the glidepath, in feet a minute.
                let fpm = speed * deg.to_radians().tan() * 6076.115 / 60.0;
                text(c, font, 6.0, cx, row_y(2), &format!("{:.0}", (fpm / 5.0).round() * 5.0), 0.25);
            }
        }
        text(c, font, 5.5, tx - 2.0 - text_width(font, 5.5, "FAF TO MAP 0.0 NM"), ty + th / 2.0 - 2.0, &format!("FAF TO MAP {segment:.1} NM"), 0.35);
    }

    // The minimum, across the profile.
    let my = at_ft(est.altitude_ft);
    c.save_state();
    c.set_dash_pattern([4.0, 2.0], 0.0);
    line(c, left - 30.0, my, right, my, 1.0, 0.0);
    c.restore_state();
    label(c, bold, 7.5, x + 4.0, my + 2.0, &format!("{:.0}", est.altitude_ft), 0.0);
}

/// The band along the bottom: the number, how it was reached, and what it is not.
fn draw_minima(c: &mut Content, font: Name, bold: Name, ch: &Chart, x: f32, y: f32, w: f32, h: f32, est: &Estimate) {
    box_outline(c, x, y, w, h, 1.2, INK);
    fill_box(c, x, y + h - 14.0, w, 14.0, 0.14);
    // An approach named for a letter has no runway to name in the heading.
    let lettered = ch.procedure.runway.len() == 1 && ch.procedure.runway.chars().all(|c| c.is_ascii_alphabetic());
    let heading = if ch.circling_only && lettered {
        "CIRCLING TO LAND".to_string()
    } else if ch.circling_only {
        format!("CIRCLING TO LAND RWY {}", ch.procedure.runway)
    } else {
        format!("STRAIGHT-IN LANDING RWY {}", ch.procedure.runway)
    };
    text(c, bold, 8.0, x + 6.0, y + h - 10.5, &heading, 1.0);
    let stamp = match est.limited_by {
        LimitedBy::Published => "AS PUBLISHED BY THE FAA",
        LimitedBy::Coded => "FROM THE PROCEDURE'S OWN CODED MINIMUM",
        _ => "AMDB V1 ESTIMATE - NOT A PUBLISHED MINIMUM",
    };
    text_right(c, font, 7.0, x + w - 6.0, y + h - 10.5, stamp, 1.0);

    // The number itself, in the box on the left.
    let col = 150.0;
    line(c, x + col, y, x + col, y + h - 14.0, 0.8, RULE);
    text(c, font, 7.0, x + 8.0, y + h - 26.0, est.approach.label(), 0.3);
    text(c, bold, 22.0, x + 8.0, y + h - 52.0, &format!("{:.0}'", est.altitude_ft), INK);
    text(c, font, 9.0, x + 8.0, y + h - 63.0, &format!("({:.0}' above touchdown)", est.height_ft), 0.25);

    // How it was reached, in the words a chart would not use but a reader wants.
    let why = match est.limited_by {
        LimitedBy::Published => format!(
            "Read from the FAA's own chart, \"{}\", where this line is printed as {}. Nothing here is estimated. Terrain under the approach reaches {:.0} ft.",
            ch.published.map(|p| p.chart.as_str()).unwrap_or("the published approach chart"),
            ch.published.map(|p| p.label.as_str()).unwrap_or("the straight-in minimum"),
            est.highest_terrain_ft
        ),
        LimitedBy::Coded => format!(
            "This is the procedure's own minimum, read from the navigation data rather than worked out here: the approach descends to {:.0} ft at its missed approach point. Terrain under the approach reaches {:.0} ft.",
            est.altitude_ft, est.highest_terrain_ft
        ),
        LimitedBy::SystemMinimum => format!(
            "Set by the system minimum for a {} approach: {:.0} ft above the touchdown zone, which is the floor no chart goes below. Nothing under the approach reaches it, so a published chart should carry this same number.",
            est.approach.label(),
            est.approach.system_minimum_ft()
        ),
        LimitedBy::Terrain => format!(
            "Set by terrain under the approach, which reaches {:.0} ft; the minimum clears it by {:.0} ft.",
            est.highest_terrain_ft,
            est.approach.obstacle_margin_ft()
        ),
        LimitedBy::Obstacle => format!(
            "Set by an obstacle under the approach: {} topping out at {:.0} ft, cleared by {:.0} ft. Highest terrain nearby is {:.0} ft.",
            est.obstacle.as_deref().unwrap_or("an obstacle"),
            est.obstacle_top_ft.unwrap_or(0.0),
            est.approach.obstacle_margin_ft(),
            est.highest_terrain_ft
        ),
    };
    let mut row = y + h - 26.0;
    for l in wrap(&why, 62) {
        text(c, font, 7.0, x + col + 8.0, row, &l, 0.2);
        row -= 9.0;
    }
    // With the glidepath out, the same chart publishes a higher minimum flown down the
    // localiser alone. A chart prints it beside the other, and an aircraft that loses its
    // glidepath on the way in needs it.
    if let Some((alt, hat)) = ch.published_loc {
        let row = y + 28.0;
        text(c, font, 6.0, x + 8.0, row, "LOC (GS OUT)", 0.4);
        text(c, bold, 10.0, x + 62.0, row, &format!("{alt:.0}'"), INK);
        text(c, font, 7.0, x + 92.0, row, &format!("({hat:.0}')"), 0.3);
    }
    // Circling, by aircraft category: the faster the aeroplane, the wider it goes and
    // the more it has to clear.
    if !ch.circling.is_empty() {
        let cy = y + 6.0;
        text(c, font, 6.0, x + 8.0, cy + 9.0, "CIRCLING", 0.4);
        for (i, (letter, ft)) in ch.circling.iter().enumerate() {
            let cx = x + 8.0 + i as f32 * 34.0;
            text(c, font, 5.5, cx, cy, &letter.to_string(), 0.45);
            text(c, bold, 7.5, cx + 7.0, cy, &format!("{ft:.0}"), INK);
        }
    }
    let where_from = if ch.tdze_surveyed {
        ", as surveyed and published for this runway end"
    } else if ch.threshold.is_some() {
        ", the highest the terrain model finds in the first 3,000 ft of the runway"
    } else {
        " (the airport's own elevation: no built runway to measure at)"
    };
    if let Some(coded) = ch.coded_ft.filter(|ft| (ft - est.altitude_ft).abs() > 5.0) {
        row -= 3.0;
        let line = format!("The procedure itself codes {coded:.0} ft at its missed approach point.");
        text(c, font, 7.0, x + col + 8.0, row, &line, 0.2);
        row -= 9.0;
    }
    let tail = format!(
        "Touchdown zone {:.0} ft{}. Obstacles: {}.",
        ch.tdze_ft,
        where_from,
        if ch.obstacles.is_empty() { "none near this approach, or no obstacle source answered".to_string() } else { format!("{} near this airport, from {}", ch.obstacles.len(), ch.obstacles[0].source) }
    );
    for l in wrap(&tail, 62) {
        text(c, font, 7.0, x + col + 8.0, row, &l, 0.4);
        row -= 9.0;
    }
}

/// The radios, across the page under the briefing strip, in the order they are used.
fn draw_frequencies(c: &mut Content, font: Name, bold: Name, ch: &Chart, x: f32, y: f32, w: f32, h: f32) {
    let list = ch.airport.briefing_frequencies();
    if list.is_empty() {
        return;
    }
    box_outline(c, x, y, w, h, 0.8, RULE);
    let cw = w / list.len().max(3) as f32;
    for (i, f) in list.iter().enumerate() {
        let cx = x + i as f32 * cw;
        if i > 0 {
            line(c, cx, y + 2.0, cx, y + h - 2.0, 0.6, 0.6);
        }
        text(c, font, 6.0, cx + 6.0, y + h - 8.0, &f.kind, 0.45);
        text(c, bold, 8.5, cx + 6.0, y + 4.0, &format!("{:.3}", f.mhz), INK);
    }
}

/// The notes a chart carries, as far as they can be worked out from the data rather than
/// copied from a state's own chart: what the approach is measured from, and what stands
/// near it.
fn chart_notes(ch: &Chart, est: &Estimate) -> Vec<String> {
    let mut out = Vec::new();
    let finals = final_legs(ch.procedure);
    let mut navaids: Vec<&str> = finals.iter().filter(|l| !l.navaid.is_empty() && l.rho_nm.is_some()).map(|l| l.navaid.as_str()).collect();
    navaids.sort_unstable();
    navaids.dedup();
    if !navaids.is_empty() {
        out.push(format!("DME {} required.", navaids.join(" and ")));
    }
    if est.highest_terrain_ft > ch.tdze_ft + 500.0 {
        out.push(format!("Terrain under the approach reaches {:.0} ft.", est.highest_terrain_ft));
    }
    if let Some(top) = ch.obstacles.iter().filter_map(|o| o.top_ft).fold(None, |a: Option<f64>, t| Some(a.map_or(t, |a| a.max(t)))) {
        if top > ch.tdze_ft + 300.0 {
            out.push(format!("Highest obstacle near the airport {top:.0} ft."));
        }
    }
    if ch.tdze_surveyed {
        out.push("Touchdown zone elevation is the published survey.".to_string());
    } else {
        out.push("Touchdown zone elevation is read from the terrain model.".to_string());
    }
    out
}

/// The strip of numbers a crew reads first, in boxes across the page.
fn draw_briefing(c: &mut Content, font: Name, bold: Name, ch: &Chart, x: f32, y: f32, w: f32, h: f32, est: &Estimate, track_deg: f64) {
    let cells: Vec<(String, String)> = vec![
        (
            if ch.course_mag_deg.is_some() { "FINAL CRS".into() } else { "FINAL CRS (T)".to_string() },
            format!("{:03.0}", ch.course_mag_deg.unwrap_or(track_deg)),
        ),
        ("TDZE".into(), format!("{:.0}'", ch.tdze_ft)),
        ("APT ELEV".into(), format!("{:.0}'", ch.field_elev_ft)),
        ("MSA 25 NM".into(), ch.msa_ft.map(|m| format!("{m:.0}'")).unwrap_or_else(|| "-".into())),
        ("DA(H)".into(), format!("{:.0}'({:.0}')", est.altitude_ft, est.height_ft)),
    ];
    let cw = w / cells.len() as f32;
    box_outline(c, x, y, w, h, 1.2, INK);
    for (i, (k, v)) in cells.iter().enumerate() {
        let cx = x + i as f32 * cw;
        if i > 0 {
            line(c, cx, y, cx, y + h, 0.8, RULE);
        }
        text(c, font, 6.5, cx + 6.0, y + h - 10.0, k, 0.4);
        text(c, bold, 12.0, cx + 6.0, y + 7.0, v, INK);
    }
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    for word in s.split_whitespace() {
        let last = out.last_mut().unwrap();
        if last.len() + word.len() + 1 > width {
            out.push(word.to_string());
        } else {
            if !last.is_empty() {
                last.push(' ');
            }
            last.push_str(word);
        }
    }
    out
}

/// The abbreviations a chart uses for the long words in an airport's name.
fn shorten(name: &str) -> String {
    let mut out = name.to_uppercase();
    for (long, short) in [
        ("INTERNATIONAL", "INTL"),
        ("REGIONAL", "RGNL"),
        ("MUNICIPAL", "MUNI"),
        ("MEMORIAL", "MEM"),
        ("COUNTY", "CO"),
        ("FIELD", "FLD"),
        ("AIRPORT", ""),
        ("  ", " "),
    ] {
        out = out.replace(long, short);
    }
    out.trim().to_string()
}

/// What the procedure is called on the page: the name a chart would print, where the
/// data says what sort of approach it is, and a plain description where it does not. An
/// approach that serves no one runway is lettered rather than numbered.
pub fn title_of(p: &Procedure) -> String {
    let lettered = p.runway.len() == 1 && p.runway.chars().all(|c| c.is_ascii_alphabetic());
    if let Some(what) = p.approach_type {
        let suffix = p.suffix.map(|c| format!(" {c}")).unwrap_or_default();
        return if lettered {
            format!("{}{suffix} {}", what.label(), p.runway)
        } else {
            format!("{}{suffix} RWY {}", what.label(), p.runway)
        };
    }
    let what = if lettered { format!("APPROACH {}", p.runway) } else { format!("APPROACH RWY {}", p.runway) };
    match p.variant {
        Some(n) if n > 1 => format!("{what} ({n})"),
        _ => what,
    }
}

/// Write the chart for a minimum that has already been worked out.
pub fn write(ch: &Chart, est: &Estimate, out: &FsPath) -> Result<()> {
    let track = ch.track_deg;
    let mut pdf = Pdf::new();
    let (cat, tree, page_id, content_id, font_id, bold_id) = (Ref::new(1), Ref::new(2), Ref::new(3), Ref::new(4), Ref::new(5), Ref::new(6));
    pdf.catalog(cat).pages(tree);
    pdf.pages(tree).kids([page_id]).count(1);
    let mut page = pdf.page(page_id);
    page.media_box(Rect::new(0.0, 0.0, W, H));
    page.parent(tree);
    page.contents(content_id);
    page.resources().fonts().pair(Name(b"F"), font_id).pair(Name(b"B"), bold_id);
    page.finish();
    // The encoding has to be stated, or a reader is free to assume the font's own, in
    // which a degree sign is not where WinAnsi puts it.
    pdf.type1_font(font_id).base_font(Name(b"Helvetica")).encoding_predefined(Name(b"WinAnsiEncoding"));
    pdf.type1_font(bold_id).base_font(Name(b"Helvetica-Bold")).encoding_predefined(Name(b"WinAnsiEncoding"));

    let (f, b) = (Name(b"F"), Name(b"B"));
    let mut c = Content::new();

    // Header: the name block on the left, the procedure on the right, the way a chart
    // puts its identity where the thumb falls.
    let head_h = 42.0;
    let head_y = H - MARGIN - head_h;
    fill_box(&mut c, MARGIN, head_y, W - 2.0 * MARGIN, head_h, 0.10);
    text(&mut c, b, 15.0, MARGIN + 8.0, head_y + head_h - 18.0, "AMDB V1", 1.0);
    text(&mut c, f, 6.5, MARGIN + 8.0, head_y + head_h - 28.0, "FREE AIRPORT", 0.75);
    text(&mut c, f, 6.5, MARGIN + 8.0, head_y + head_h - 36.0, "MAPPING DATABASE", 0.75);
    line(&mut c, MARGIN + 96.0, head_y + 4.0, MARGIN + 96.0, head_y + head_h - 4.0, 0.8, 0.6);
    let title = title_of(ch.procedure);
    text_right(&mut c, b, 13.0, W - MARGIN - 8.0, head_y + head_h - 17.0, &title, 1.0);
    let name_x = MARGIN + 106.0;
    let short_name = shorten(ch.airport_name.unwrap_or(""));
    let room = W - MARGIN - 16.0 - text_width(b, 13.0, &title) - name_x;
    let mut heading = format!("{}  {}", ch.airport.icao, short_name);
    while text_width(b, 13.0, &heading) > room && heading.len() > ch.airport.icao.len() + 4 {
        heading.pop();
    }
    text(&mut c, b, 13.0, name_x, head_y + head_h - 17.0, heading.trim_end(), 1.0);
    text(&mut c, f, 7.5, name_x, head_y + head_h - 29.0, &format!("{:.4}, {:.4}", ch.airport.lat, ch.airport.lon), 0.8);
    let star_note = ch.star.map(|s| format!(" - VIA {}", s.name)).unwrap_or_default();
    let source = match est.limited_by {
        LimitedBy::Published => "published minimum",
        LimitedBy::Coded => "coded minimum",
        _ => "estimated minimum",
    };
    let circling_note = if ch.circling_only { " - CIRCLING ONLY" } else { "" };
    text_right(&mut c, f, 7.5, W - MARGIN - 8.0, head_y + head_h - 29.0, &format!("{}{circling_note} - {source}{star_note}", ch.kind.label().to_uppercase()), 0.8);

    let strip_h = 30.0;
    let strip_y = head_y - 4.0 - strip_h;
    draw_briefing(&mut c, f, b, ch, MARGIN, strip_y, W - 2.0 * MARGIN, strip_h, est, track);

    let freq_h = 18.0;
    let freq_y = strip_y - 3.0 - freq_h;
    draw_frequencies(&mut c, f, b, ch, MARGIN, freq_y, W - 2.0 * MARGIN, freq_h);
    let below_strip = if ch.airport.briefing_frequencies().is_empty() { strip_y } else { freq_y };

    let min_h = 104.0;
    let min_y = MARGIN + 26.0;
    let prof_h = 140.0;
    let prof_y = min_y + min_h + 30.0;
    let plan_y = prof_y + prof_h + 6.0;
    let plan_h = below_strip - 6.0 - plan_y;
    let v = draw_plan(&mut c, f, b, ch, MARGIN, plan_y, W - 2.0 * MARGIN, plan_h, est, track);

    // A caption under the plan, saying what the picture is made of.
    let built = if ch.airport_dir.is_some() { "airport from our own build" } else { "airport not built yet" };
    let caption = format!("{:.0} NM across - terrain tinted by elevation - {built}", v.span_nm());
    let cap_w = text_width(f, 6.5, &caption) + 10.0;
    fill_box(&mut c, MARGIN + 92.0, plan_y + 1.0, cap_w, 11.0, 1.0);
    text(&mut c, f, 6.5, MARGIN + 96.0, plan_y + 4.0, &caption, 0.4);

    draw_profile(&mut c, f, b, ch, MARGIN, prof_y, W - 2.0 * MARGIN, prof_h, est, track);

    // The missed approach, in its own line above the minima band, as charts do.
    let missed = part_legs(ch.procedure, "missed");
    let missed_y = min_y + min_h + 19.0;
    text(&mut c, b, 7.0, MARGIN + 2.0, missed_y, "MISSED APPROACH:", INK);
    let room = W - 2.0 * MARGIN - 86.0;
    let mut sentence = missed_text(&missed);
    while text_width(f, 7.0, &sentence) > room && sentence.len() > 4 {
        sentence.pop();
    }
    text(&mut c, f, 7.0, MARGIN + 86.0, missed_y, &sentence, 0.2);
    if let Some((climb, what)) = &ch.missed_climb {
        let note = format!("CLIMB {climb:.0} FT/NM TO CLEAR {}", what.to_uppercase());
        text_right(&mut c, b, 6.5, W - MARGIN - 2.0, missed_y, &note, 0.15);
    }

    // The notes, in one line under the missed approach.
    let notes = chart_notes(ch, est).join("  ");
    for (i, row) in wrap(&notes, 150).into_iter().take(2).enumerate() {
        text(&mut c, f, 6.5, MARGIN + 2.0, missed_y - 9.0 - i as f32 * 8.0, &row, 0.35);
    }

    draw_minima(&mut c, f, b, ch, MARGIN, min_y, W - 2.0 * MARGIN, min_h, est);

    // Footer.
    line(&mut c, MARGIN, MARGIN + 18.0, W - MARGIN, MARGIN + 18.0, 0.8, RULE);
    let printed = chrono::Utc::now().format("%d %b %Y").to_string().to_uppercase();
    let caveat = match est.limited_by {
        LimitedBy::Published => "NOT FOR REAL-WORLD NAVIGATION. The minimum is read from the state's own published chart; everything else here is drawn from free data: fly the published chart.",
        LimitedBy::Coded => "NOT FOR REAL-WORLD NAVIGATION. The minimum is the one coded in the simulator's navigation data: fly the published chart.",
        _ => "NOT FOR REAL-WORLD NAVIGATION. The minimum on this chart is calculated, not published: fly the published chart.",
    };
    text(&mut c, f, 6.5, MARGIN, MARGIN + 9.0, caveat, 0.25);
    text(
        &mut c,
        f,
        6.0,
        MARGIN,
        MARGIN + 1.0,
        &format!(
            "AMDB V1 - drawn {printed} - navigation data {} - terrain Copernicus DEM (ESA) - obstacles {} - airport OpenStreetMap and the X-Plane Scenery Gateway",
            ch.airac.as_ref().map(|(from, to)| format!("in force {from} to {to}")).unwrap_or_else(|| ch.airport.source.clone()),
            ch.obstacles.first().map(|o| o.source).unwrap_or("none found")
        ),
        0.45,
    );

    pdf.stream(content_id, &c.finish());
    std::fs::write(out, pdf.finish()).with_context(|| format!("write {}", out.display()))?;
    Ok(())
}

/// Every approach an airport has, in the order they are named on the chart.
pub fn approaches(a: &AirportProcedures) -> Vec<&Procedure> {
    a.procedures.iter().filter(|p| p.kind == Kind::Approach).collect()
}

/// The approach a name asks for. A runway on its own ("27L") takes the first approach to
/// it; a runway and a number ("27L-2") takes that one of several.
pub fn pick<'a>(a: &'a AirportProcedures, want: Option<&str>) -> Option<&'a Procedure> {
    let all = approaches(a);
    let Some(want) = want else {
        // Nothing asked for: the approach with the most in it.
        return all.into_iter().max_by_key(|p| final_legs(p).len());
    };
    let want = want.trim().trim_start_matches("RW").to_uppercase();
    // A name, the way a chart writes it: "ILS 35C", "RNAV Z 16".
    // A name, the way a chart writes it: every word asked for has to appear in it.
    // "RNAV 16" finds "RNAV (GPS) Z RWY 16"; failing that the runway alone is used.
    let words: Vec<&str> = want.split_whitespace().collect();
    if words.len() > 1 {
        if let Some(found) = all.iter().find(|p| {
            let name = p.name.to_uppercase().replace(['(', ')'], " ");
            words.iter().all(|word| name.split_whitespace().any(|part| part == *word))
        }) {
            return Some(found);
        }
    }
    let want = words.last().copied().unwrap_or(&want).to_string();
    let (runway, variant) = match want.split_once('-') {
        Some((r, n)) => (r.to_string(), n.trim().parse::<usize>().ok()),
        None => (want.clone(), None),
    };
    all.iter()
        .find(|p| p.runway == runway && variant.map(|n| p.variant == Some(n)).unwrap_or(true))
        .or_else(|| all.iter().find(|p| p.runway.starts_with(&runway)))
        .copied()
}

/// The arrival a name asks for, matched loosely so "BIG1A" finds "BIG1A" and "BIG" finds
/// the first arrival that starts with it.
pub fn pick_star<'a>(a: &'a AirportProcedures, want: &str) -> Option<&'a Procedure> {
    let want = want.trim().to_uppercase();
    let stars = a.procedures.iter().filter(|p| p.kind == Kind::Star);
    stars.clone().find(|p| p.name.to_uppercase() == want).or_else(|| stars.clone().find(|p| p.name.to_uppercase().starts_with(&want)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::msfs::procedures::AltitudeRule;

    fn leg(path: &str, fix: &str, alt: Option<f64>) -> Leg {
        Leg { path: path.into(), fix: fix.into(), altitude_ft: alt, ..Leg::default() }
    }

    #[test]
    fn a_missed_approach_reads_as_a_sentence() {
        let legs = vec![leg("CA", "", Some(3000.0)), leg("DF", "BOSSI", None), leg("HM", "BOSSI", None)];
        let refs: Vec<&Leg> = legs.iter().collect();
        assert_eq!(missed_text(&refs), "Climb to 3000 ft, then direct BOSSI, then hold at BOSSI.");
    }

    #[test]
    fn the_window_never_shrinks_below_the_minimum_span() {
        let v = View::around((51.5, -0.5), 270.0, &[(51.5001, -0.5001)], 0.0, 0.0, 400.0, 400.0);
        assert!(v.span_nm() >= PLAN_MIN_NM - 0.01);
    }
}
