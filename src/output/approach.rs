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
use crate::sources::msfs::procedures::{AirportProcedures, Kind, Leg, Procedure, Transition};
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
const PLAN_MAX_NM: f64 = 16.0;

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
    /// The final approach track, in degrees true, which is what the drawing uses.
    pub track_deg: f64,
    /// The course to print, which is magnetic where the data carries one.
    pub course_mag_deg: Option<f64>,
    pub kind: Approach,
    pub airport_dir: Option<&'a FsPath>,
    /// Both ends of the landing runway, threshold first.
    pub runway_ends: Option<((f64, f64), (f64, f64))>,
}

fn ascii(s: &str) -> Vec<u8> {
    s.chars().map(|c| if c.is_ascii() && !c.is_control() { c as u8 } else { b'?' }).collect()
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
        // Room for the airport itself, then as much of the approach as will fit.
        let span_nm = (far * 1.35).clamp(PLAN_MIN_NM, PLAN_MAX_NM);
        // Slide the middle of the window back down the approach, so the airport sits
        // off-centre with the final laid out in front of it.
        let back = (track_deg + 180.0).to_radians();
        let shift_nm = span_nm * 0.18;
        let centre = (airport.0 + shift_nm * back.cos() / 60.0, airport.1 + shift_nm * back.sin() / 60.0 / cos);

        let aspect = (w / h) as f64;
        let (half_w_nm, half_h_nm) = if aspect >= 1.0 { (span_nm / 2.0, span_nm / 2.0 / aspect) } else { (span_nm / 2.0 * aspect, span_nm / 2.0) };
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

    /// Nautical miles across the window.
    fn span_nm(&self) -> f64 {
        self.deg_w * 60.0 * self.north.to_radians().cos()
    }

    fn px_per_nm(&self) -> f32 {
        self.w / self.span_nm().max(0.001) as f32
    }
}

/// Heights shaded in bands, as a terrain picture rather than contours: quick to read and
/// honest about what a 30 m model can say.
fn draw_terrain(c: &mut Content, patch: &Patch, v: &View, field_ft: f64) {
    let cw = (patch.step_lon / v.deg_w) as f32 * v.w + 0.4;
    let ch = (patch.step_lat / v.deg_h) as f32 * v.h + 0.4;
    for row in 0..patch.height {
        for col in 0..patch.width {
            let ht = patch.at(row, col);
            if !ht.is_finite() {
                continue;
            }
            let above = ht as f64 / 0.3048 - field_ft;
            let g = match above {
                a if a < 500.0 => continue,
                a if a < 1000.0 => 0.93,
                a if a < 2000.0 => 0.86,
                a if a < 4000.0 => 0.77,
                _ => 0.66,
            };
            let (lat, lon) = patch.position(row, col);
            let (px, py) = v.at(lat, lon);
            if !v.inside((px, py), 0.0) {
                continue;
            }
            c.set_fill_gray(g);
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
fn draw_fix(c: &mut Content, font: Name, bold: Name, v: &View, leg: &Leg, is_faf: bool, floor_ft: f64) {
    let (Some(lat), Some(lon)) = (leg.lat, leg.lon) else { return };
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
    label(c, bold, 7.0, px + 6.0, py + 1.0, &leg.fix, INK);
    if let Some(a) = leg.altitude_ft.filter(|a| *a > floor_ft) {
        let rule = match leg.altitude_rule {
            crate::sources::msfs::procedures::AltitudeRule::AtOrAbove => "+",
            crate::sources::msfs::procedures::AltitudeRule::AtOrBelow => "-",
            _ => "",
        };
        label(c, font, 6.5, px + 6.0, py - 7.0, &format!("{a:.0}{rule}"), 0.28);
    }
}

/// A run of legs as a line on the map, through the fixes that have a position.
fn draw_track(c: &mut Content, v: &View, legs: &[&Leg], start: Option<(f64, f64)>, weight: f32, dashed: bool, grey: f32) -> Vec<(f32, f32)> {
    let mut pts: Vec<(f32, f32)> = Vec::new();
    if let Some((lat, lon)) = start {
        pts.push(v.at(lat, lon));
    }
    for leg in legs {
        if let (Some(lat), Some(lon)) = (leg.lat, leg.lon) {
            pts.push(v.at(lat, lon));
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
        if drawn > 24 {
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

/// North arrow, scale bar and the minimum safe altitude ring: the furniture that tells
/// you how to read the map.
fn draw_furniture(c: &mut Content, font: Name, bold: Name, v: &View, msa_ft: Option<f64>) {
    // North arrow, top left inside the box.
    let (nx, ny) = (v.x + 16.0, v.y + v.h - 30.0);
    fill_box(c, nx - 9.0, ny - 8.0, 18.0, 34.0, 1.0);
    c.set_fill_gray(INK);
    c.move_to(nx, ny + 18.0);
    c.line_to(nx + 4.0, ny + 6.0);
    c.line_to(nx, ny + 9.0);
    c.line_to(nx - 4.0, ny + 6.0);
    c.close_path();
    c.fill_nonzero();
    line(c, nx, ny + 9.0, nx, ny - 4.0, 0.8, INK);
    text_centred(c, bold, 7.5, nx, ny - 6.5, "N", INK);

    // Scale bar, bottom left.
    let px_nm = v.px_per_nm();
    let step_nm = if v.span_nm() > 16.0 { 5.0 } else { 2.0 };
    let bar = step_nm as f32 * px_nm;
    let (bx, by) = (v.x + 12.0, v.y + 14.0);
    fill_box(c, bx - 4.0, by - 4.0, bar + 30.0, 16.0, 1.0);
    line(c, bx, by, bx + bar, by, 1.2, INK);
    for i in 0..=1 {
        let x = bx + i as f32 * bar;
        line(c, x, by - 3.0, x, by + 3.0, 1.2, INK);
    }
    text(c, font, 6.5, bx + bar + 3.0, by - 2.0, &format!("{step_nm:.0} NM"), INK);

    // Minimum safe altitude ring, the way a chart shows it in the corner.
    if let Some(msa) = msa_ft {
        let (cx, cy) = (v.x + v.w - 32.0, v.y + v.h - 32.0);
        fill_box(c, cx - 26.0, cy - 26.0, 52.0, 52.0, 1.0);
        c.set_stroke_gray(INK);
        c.set_line_width(0.9);
        circle(c, cx, cy, 20.0);
        c.stroke();
        text_centred(c, bold, 10.0, cx, cy - 2.0, &format!("{msa:.0}"), INK);
        text_centred(c, font, 5.5, cx, cy - 11.0, "MSA 25 NM", 0.3);
        text_centred(c, font, 5.5, cx, cy + 8.0, "ARP", 0.3);
    }
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
                draw_fix(c, font, bold, &v, leg, false, ch.tdze_ft);
            }
        }
    }

    // The ways in to the approach, thin.
    for t in feeder_transitions(ch.procedure) {
        let legs: Vec<&Leg> = t.legs.iter().collect();
        draw_track(c, &v, &legs, None, 0.9, false, 0.35);
        for leg in &legs {
            draw_fix(c, font, bold, &v, leg, false, ch.tdze_ft);
        }
    }

    // The final, heavy, ending at the threshold.
    let faf_fix = finals.iter().rev().find(|l| l.altitude_ft.is_some() && !l.fix.is_empty()).map(|l| l.fix.clone());
    let end = if ch.threshold.is_some() { Some(centre) } else { None };
    let mut track_pts = draw_track(c, &v, &finals, None, 2.0, false, INK);
    if let Some((lat, lon)) = end {
        let p = v.at(lat, lon);
        if let Some(prev) = track_pts.last().copied() {
            line(c, prev.0, prev.1, p.0, p.1, 2.0, INK);
        }
        track_pts.push(p);
    }
    for leg in &finals {
        let is_faf = faf_fix.as_deref() == Some(leg.fix.as_str());
        draw_fix(c, font, bold, &v, leg, is_faf, ch.tdze_ft);
    }
    // The inbound course, written along the final the way a chart does.
    if track_pts.len() >= 2 {
        let a = track_pts[0];
        let b = track_pts[track_pts.len() - 1];
        let course = ch.course_mag_deg.unwrap_or(track_deg);
        label(c, bold, 8.5, (a.0 + b.0) / 2.0 + 7.0, (a.1 + b.1) / 2.0 - 14.0, &format!("{course:03.0}"), INK);
    }

    // The missed approach, dashed with an arrow, as charts draw it.
    if !missed.is_empty() {
        let pts = draw_track(c, &v, &missed, ch.threshold, 1.4, true, 0.15);
        if pts.len() >= 2 {
            arrow_head(c, pts[pts.len() - 2], pts[pts.len() - 1], 0.15);
        }
        for leg in &missed {
            draw_fix(c, font, bold, &v, leg, false, ch.tdze_ft);
        }
    }

    draw_obstacles(c, font, bold, &v, ch.obstacles, ch.tdze_ft + 150.0, est.obstacle_top_ft.filter(|_| est.limited_by == LimitedBy::Obstacle));
    c.restore_state();
    draw_furniture(c, font, bold, &v, ch.msa_ft);
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

    // Glidepath angle and threshold crossing height, in the corner a chart puts them.
    label(c, font, 6.5, at_nm(0.0) - 44.0, tch + 9.0, "TCH 50", 0.3);
    text(c, bold, 8.0, x + 6.0, y + h - 12.0, "GP 3.00", INK);
    text(c, font, 6.5, x + 6.0, y + h - 21.0, &format!("{total_nm:.1} NM"), 0.35);

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
    text(c, bold, 8.0, x + 6.0, y + h - 10.5, &format!("STRAIGHT-IN LANDING RWY {}", ch.procedure.runway), 1.0);
    text_right(c, font, 7.0, x + w - 6.0, y + h - 10.5, "AMDB V1 ESTIMATE - NOT A PUBLISHED MINIMUM", 1.0);

    // The number itself, in the box on the left.
    let col = 150.0;
    line(c, x + col, y, x + col, y + h - 14.0, 0.8, RULE);
    text(c, font, 7.0, x + 8.0, y + h - 26.0, est.approach.label(), 0.3);
    text(c, bold, 22.0, x + 8.0, y + h - 50.0, &format!("{:.0}'", est.altitude_ft), INK);
    text(c, font, 9.0, x + 8.0, y + h - 63.0, &format!("({:.0}' above touchdown)", est.height_ft), 0.25);

    // How it was reached, in the words a chart would not use but a reader wants.
    let why = match est.limited_by {
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
    let where_from = if ch.tdze_surveyed {
        ", as surveyed and published for this runway end"
    } else if ch.threshold.is_some() {
        ", the highest the terrain model finds in the first 3,000 ft of the runway"
    } else {
        " (the airport's own elevation: no built runway to measure at)"
    };
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

/// What the procedure is called on the page. An approach that serves no one runway is
/// lettered rather than numbered, and is not called a runway approach.
pub fn title_of(p: &Procedure) -> String {
    let lettered = p.runway.len() == 1 && p.runway.chars().all(|c| c.is_ascii_alphabetic());
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
    pdf.type1_font(font_id).base_font(Name(b"Helvetica"));
    pdf.type1_font(bold_id).base_font(Name(b"Helvetica-Bold"));

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
    text_right(&mut c, f, 7.5, W - MARGIN - 8.0, head_y + head_h - 29.0, &format!("{} - estimated minimum{star_note}", ch.kind.label().to_uppercase()), 0.8);

    let strip_h = 30.0;
    let strip_y = head_y - 4.0 - strip_h;
    draw_briefing(&mut c, f, b, ch, MARGIN, strip_y, W - 2.0 * MARGIN, strip_h, est, track);

    let min_h = 86.0;
    let min_y = MARGIN + 26.0;
    let prof_h = 146.0;
    let prof_y = min_y + min_h + 18.0;
    let plan_y = prof_y + prof_h + 6.0;
    let plan_h = strip_y - 6.0 - plan_y;
    let v = draw_plan(&mut c, f, b, ch, MARGIN, plan_y, W - 2.0 * MARGIN, plan_h, est, track);

    // A caption under the plan, saying what the picture is made of.
    let built = if ch.airport_dir.is_some() { "airport from our own build" } else { "airport not built yet" };
    let caption = format!("{:.0} NM across - terrain shaded above {:.0} ft - {built}", v.span_nm(), ch.field_elev_ft + 500.0);
    let cap_w = text_width(f, 6.5, &caption) + 10.0;
    fill_box(&mut c, W - MARGIN - 1.0 - cap_w, plan_y + 1.0, cap_w, 11.0, 1.0);
    text(&mut c, f, 6.5, W - MARGIN - cap_w + 4.0, plan_y + 4.0, &caption, 0.4);

    draw_profile(&mut c, f, b, ch, MARGIN, prof_y, W - 2.0 * MARGIN, prof_h, est, track);

    // The missed approach, in its own line above the minima band, as charts do.
    let missed = part_legs(ch.procedure, "missed");
    let missed_y = min_y + min_h + 6.0;
    text(&mut c, b, 7.0, MARGIN + 2.0, missed_y, "MISSED APPROACH:", INK);
    let room = W - 2.0 * MARGIN - 86.0;
    let mut sentence = missed_text(&missed);
    while text_width(f, 7.0, &sentence) > room && sentence.len() > 4 {
        sentence.pop();
    }
    text(&mut c, f, 7.0, MARGIN + 86.0, missed_y, &sentence, 0.2);

    draw_minima(&mut c, f, b, ch, MARGIN, min_y, W - 2.0 * MARGIN, min_h, est);

    // Footer.
    line(&mut c, MARGIN, MARGIN + 18.0, W - MARGIN, MARGIN + 18.0, 0.8, RULE);
    let stamp = chrono::Utc::now().format("%d %b %Y").to_string().to_uppercase();
    text(&mut c, f, 6.5, MARGIN, MARGIN + 9.0, "NOT FOR REAL-WORLD NAVIGATION. The minimum on this chart is calculated, not published: fly the published chart.", 0.25);
    text(
        &mut c,
        f,
        6.0,
        MARGIN,
        MARGIN + 1.0,
        &format!(
            "AMDB V1 - {stamp} - procedure from {} - terrain Copernicus DEM (ESA) - obstacles {} - airport OpenStreetMap and the X-Plane Scenery Gateway",
            ch.airport.source,
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
        Leg {
            path: path.into(),
            fix: fix.into(),
            altitude_rule: AltitudeRule::None,
            altitude_ft: alt,
            altitude2_ft: None,
            course_deg: None,
            distance_m: None,
            lat: None,
            lon: None,
        }
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
