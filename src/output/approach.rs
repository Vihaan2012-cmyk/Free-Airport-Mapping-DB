//! An approach chart: the airport, the terrain around it, the procedure's final track
//! and descent profile, and an estimate of the minimum.
//!
//! Everything on the page comes from free data or from the simulator's own navigation
//! database on this machine. The minima box carries an estimate, not a published figure,
//! and the page says so: the terrain model cannot see masts and aerials, which are
//! usually what sets a real minimum.

use crate::minima::{self, Approach, Estimate, LimitedBy};
use crate::sources::copernicus::Patch;
use crate::sources::msfs::procedures::{AirportProcedures, Kind, Leg, Procedure};
use anyhow::{Context, Result};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};

const W: f32 = 595.0; // A4 portrait, points
const H: f32 = 842.0;
const MARGIN: f32 = 36.0;
const INK: f32 = 0.12;
/// How wide the plan view is, in nautical miles. A real approach chart is about this.
const PLAN_SPAN_NM: f64 = 8.0;

fn ascii(s: &str) -> Vec<u8> {
    s.chars().map(|c| if c.is_ascii() && !c.is_control() { c as u8 } else { b'?' }).collect()
}

fn text(c: &mut Content, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32) {
    c.begin_text();
    c.set_fill_gray(grey);
    c.set_font(font, size);
    c.next_line(x, y);
    c.show(Str(&ascii(s)));
    c.end_text();
}

fn line(c: &mut Content, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, grey: f32) {
    c.set_stroke_gray(grey);
    c.set_line_width(w);
    c.move_to(x1, y1);
    c.line_to(x2, y2);
    c.stroke();
}

fn box_outline(c: &mut Content, x: f32, y: f32, w: f32, h: f32, grey: f32) {
    c.set_stroke_gray(grey);
    c.set_line_width(0.8);
    c.rect(x, y, w, h);
    c.stroke();
}

/// The legs that make up the final approach, and the fix the approach starts from.
fn final_legs(p: &Procedure) -> Vec<&Leg> {
    p.transitions.iter().filter(|t| t.part == "final").flat_map(|t| t.legs.iter()).collect()
}

/// Heights shaded in bands, as a terrain picture rather than contours: quick to read
/// and honest about a 30 m model's resolution.
fn draw_terrain(c: &mut Content, patch: &Patch, win: &Window, x: f32, y: f32, w: f32, h: f32, field_ft: f64) {
    let cw = w / win.cols() as f32;
    let ch = h / win.rows() as f32;
    for row in win.row0..win.row1 {
        for col in win.col0..win.col1 {
            let v = patch.at(row, col);
            if !v.is_finite() {
                continue;
            }
            let ft = v as f64 / 0.3048;
            // Ground near the airport stays white; higher ground darkens in steps, the
            // way a chart shades terrain above the aerodrome.
            let above = ft - field_ft;
            let g = match above {
                a if a < 500.0 => continue,
                a if a < 1000.0 => 0.92,
                a if a < 2000.0 => 0.84,
                a if a < 4000.0 => 0.74,
                _ => 0.62,
            };
            c.set_fill_gray(g);
            c.rect(x + (col - win.col0) as f32 * cw, y + h - (row + 1 - win.row0) as f32 * ch, cw + 0.4, ch + 0.4);
            c.fill_nonzero();
        }
    }
}

/// The part of a patch the plan view shows: a square of `span_nm` about the airport,
/// while the minimum is worked out from the whole patch.
struct Window {
    row0: usize,
    row1: usize,
    col0: usize,
    col1: usize,
}

impl Window {
    /// `aspect` is the width over the height of the box it will be drawn in, so the
    /// ground is not stretched to fit.
    fn centred(patch: &Patch, span_nm: f64, aspect: f64) -> Window {
        let half_lat = span_nm / 2.0 / 60.0;
        let half_lon = half_lat * aspect / (patch.north - patch.height as f64 * patch.step_lat / 2.0).to_radians().cos().max(0.05);
        let rows = ((half_lat / patch.step_lat) as usize).clamp(4, patch.height / 2);
        let cols = ((half_lon / patch.step_lon) as usize).clamp(4, patch.width / 2);
        Window {
            row0: patch.height / 2 - rows,
            row1: patch.height / 2 + rows,
            col0: patch.width / 2 - cols,
            col1: patch.width / 2 + cols,
        }
    }

    fn rows(&self) -> usize {
        self.row1 - self.row0
    }

    fn cols(&self) -> usize {
        self.col1 - self.col0
    }
}

/// Where a latitude and longitude falls inside the plan view.
struct Frame {
    patch_w_deg: f64,
    patch_h_deg: f64,
    west: f64,
    north: f64,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Frame {
    fn at(&self, lat: f64, lon: f64) -> (f32, f32) {
        let fx = (lon - self.west) / self.patch_w_deg;
        let fy = (self.north - lat) / self.patch_h_deg;
        (self.x + fx as f32 * self.w, self.y + self.h - fy as f32 * self.h)
    }

    fn inside(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x - 20.0 && p.0 <= self.x + self.w + 20.0 && p.1 >= self.y - 20.0 && p.1 <= self.y + self.h + 20.0
    }
}

/// The airport as we built it: pavement, then water and buildings. Drawn in flat greys
/// so the procedure and the terrain shading stay the things the eye goes to.
fn draw_airport(c: &mut Content, dir: &std::path::Path, f: &Frame) -> bool {
    use geo_types::Geometry;
    let mut drawn = false;
    for (layer, grey) in [("apronelement", 0.82), ("taxiwayelement", 0.76), ("water", 0.88), ("verticalpolygonalstructure", 0.62), ("runwayelement", 0.25)] {
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
                let pts: Vec<(f32, f32)> = p.exterior().0.iter().map(|co| f.at(co.y, co.x)).collect();
                if pts.len() < 3 || !pts.iter().any(|p| f.inside(*p)) {
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

/// Plan view: terrain, the airport, and the final track with its fixes.
#[allow(clippy::too_many_arguments)]
fn draw_plan(c: &mut Content, font: Name, bold: Name, patch: &Patch, x: f32, y: f32, w: f32, h: f32, field_ft: f64, track_deg: f64, legs: &[&Leg], runway: &str, airport_dir: Option<&std::path::Path>) {
    let win = Window::centred(patch, PLAN_SPAN_NM, (w / h) as f64);
    draw_terrain(c, patch, &win, x, y, w, h, field_ft);
    let frame = Frame {
        patch_w_deg: win.cols() as f64 * patch.step_lon,
        patch_h_deg: win.rows() as f64 * patch.step_lat,
        west: patch.west + win.col0 as f64 * patch.step_lon,
        north: patch.north - win.row0 as f64 * patch.step_lat,
        x,
        y,
        w,
        h,
    };
    let have_airport = airport_dir.map(|d| draw_airport(c, d, &frame)).unwrap_or(false);
    box_outline(c, x, y, w, h, INK);

    // The airport sits at the middle of the patch; the approach comes in on the
    // reciprocal of the landing direction.
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let back = (track_deg + 180.0).to_radians();
    let (dx, dy) = (back.sin() as f32, back.cos() as f32);
    // Scale: the window is a known number of miles across.
    let miles_across = win.cols() as f64 * patch.step_lon * 60.0 * patch.north.to_radians().cos();
    let px_per_nm = w / miles_across.max(0.001) as f32;

    if !have_airport {
        // No built airport to draw: a plain runway mark, to its real direction.
        let half = 0.7 * px_per_nm;
        line(c, cx - dx * half, cy - dy * half, cx + dx * half, cy + dy * half, 3.0, INK);
    }

    // Final track, out to the farthest fix that has a distance.
    let reach = legs.iter().filter_map(|l| l.distance_m).sum::<f64>().max(10.0 * 1852.0) / 1852.0;
    // Stop at the edge of the box, leaving room for a fix label.
    let room = |d: f32, o: f32, lo: f32, hi: f32| if d.abs() < 1e-3 { f32::MAX } else if d > 0.0 { (hi - o) / d } else { (lo - o) / d };
    let to_edge = room(dx, cx, x + 10.0, x + w - 34.0).min(room(dy, cy, y + 10.0, y + h - 14.0));
    let end = (reach as f32 * px_per_nm).min(to_edge.max(10.0));
    c.save_state();
    c.set_dash_pattern([4.0, 3.0], 0.0);
    line(c, cx, cy, cx + dx * end, cy + dy * end, 1.2, INK);
    c.restore_state();

    // Fixes along it, spaced by their leg distances where the data gives them.
    let mut at_nm = 0.0;
    for leg in legs.iter().rev() {
        if leg.fix.starts_with("RW") || leg.fix.is_empty() {
            continue;
        }
        at_nm += leg.distance_m.map(|m| m / 1852.0).unwrap_or(4.0);
        let d = at_nm as f32 * px_per_nm;
        if d > end {
            break;
        }
        let (fx, fy) = (cx + dx * d, cy + dy * d);
        c.set_stroke_gray(INK);
        c.set_line_width(1.0);
        c.move_to(fx - 3.0, fy - 3.0);
        c.line_to(fx + 3.0, fy + 3.0);
        c.move_to(fx - 3.0, fy + 3.0);
        c.line_to(fx + 3.0, fy - 3.0);
        c.stroke();
        text(c, font, 7.0, fx + 5.0, fy + 3.0, &leg.fix, INK);
        if let Some(a) = leg.altitude_ft {
            text(c, font, 6.5, fx + 5.0, fy - 5.0, &format!("{a:.0}"), 0.35);
        }
    }
    text(c, bold, 8.0, cx + 6.0, cy - 10.0, &format!("RW{runway}"), INK);
    let source = if have_airport { "airport from our own build" } else { "airport not built yet" };
    // A strip behind the caption, so it reads over shaded ground.
    c.set_fill_gray(1.0);
    c.rect(x + 1.0, y + h - 16.0, w - 2.0, 15.0);
    c.fill_nonzero();
    text(c, font, 7.0, x + 6.0, y + h - 12.0, &format!("{:.0} NM across, terrain shaded above {:.0} ft, {source}", miles_across, field_ft + 500.0), 0.35);
}

/// Profile: the descent, with each fix's altitude.
fn draw_profile(c: &mut Content, font: Name, x: f32, y: f32, w: f32, h: f32, legs: &[&Leg], field_ft: f64, est: &Estimate) {
    box_outline(c, x, y, w, h, INK);
    let alts: Vec<f64> = legs.iter().filter_map(|l| l.altitude_ft).collect();
    let top = alts.iter().cloned().fold(field_ft + 1000.0, f64::max);
    let scale_y = (h - 26.0) / (top - field_ft).max(100.0) as f32;
    let ground = y + 14.0;
    line(c, x + 4.0, ground, x + w - 4.0, ground, 1.0, INK);

    let total: f64 = legs.iter().filter_map(|l| l.distance_m).sum::<f64>().max(10.0 * 1852.0);
    let mut from_thr = 0.0;
    let mut points: Vec<(f32, f32, &Leg)> = Vec::new();
    for leg in legs.iter().rev() {
        if let Some(a) = leg.altitude_ft {
            let px = x + w - 10.0 - (from_thr / total) as f32 * (w - 20.0);
            let py = ground + ((a - field_ft) as f32).max(0.0) * scale_y;
            points.push((px, py, leg));
        }
        from_thr += leg.distance_m.unwrap_or(4.0 * 1852.0);
    }
    c.set_stroke_gray(INK);
    c.set_line_width(1.4);
    for (i, (px, py, _)) in points.iter().enumerate() {
        if i == 0 {
            c.move_to(*px, *py);
        } else {
            c.line_to(*px, *py);
        }
    }
    c.stroke();
    for (px, py, leg) in &points {
        text(c, font, 6.5, *px - 10.0, *py + 4.0, &format!("{}", leg.altitude_ft.unwrap_or(0.0)), INK);
        if !leg.fix.is_empty() {
            text(c, font, 6.5, *px - 10.0, ground - 9.0, &leg.fix, 0.35);
        }
    }
    // The estimated minimum, as a line across the profile.
    let my = ground + ((est.altitude_ft - field_ft) as f32).max(0.0) * scale_y;
    c.save_state();
    c.set_dash_pattern([3.0, 2.0], 0.0);
    line(c, x + 4.0, my, x + w - 4.0, my, 0.8, 0.45);
    c.restore_state();
    text(c, font, 6.5, x + 6.0, my + 3.0, &format!("{:.0} ft estimated", est.altitude_ft), 0.45);
}

fn draw_minima(c: &mut Content, font: Name, bold: Name, x: f32, y: f32, w: f32, h: f32, est: &Estimate) {
    box_outline(c, x, y, w, h, INK);
    text(c, bold, 9.0, x + 8.0, y + h - 14.0, "MINIMUM (estimated)", INK);
    text(c, bold, 20.0, x + 8.0, y + h - 38.0, &format!("{:.0} ft", est.altitude_ft), INK);
    text(c, font, 8.0, x + 8.0, y + h - 50.0, &format!("{:.0} ft above touchdown - {}", est.height_ft, est.approach.label()), 0.3);
    let why = match est.limited_by {
        LimitedBy::SystemMinimum => "Set by the system minimum for this approach type, which is what the published chart uses too.".to_string(),
        LimitedBy::Terrain => format!("Set by terrain ({:.0} ft) under the approach. An estimate only: masts and aerials are not in the data.", est.highest_terrain_ft),
    };
    for (i, l) in wrap(&why, 58).into_iter().enumerate() {
        text(c, font, 7.0, x + 8.0, y + h - 64.0 - i as f32 * 9.0, &l, 0.3);
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

/// Write the chart. `procedure` is the approach to draw.
pub fn write(airport: &AirportProcedures, procedure: &Procedure, patch: &Patch, field_elev_ft: f64, kind: Approach, airport_dir: Option<&std::path::Path>, out: &std::path::Path) -> Result<Estimate> {
    let legs = final_legs(procedure);
    let track = legs.iter().rev().find_map(|l| l.course_deg).unwrap_or_else(|| {
        // No course in the data: take it from the runway number.
        procedure.runway.trim_end_matches(|c: char| c.is_alphabetic()).parse::<f64>().unwrap_or(0.0) * 10.0
    });
    // The last leg's altitude is the height crossing the threshold, roughly 50 ft above
    // the touchdown zone; the minimum is measured from the touchdown zone itself.
    let crossing_ft = legs.last().and_then(|l| l.altitude_ft).unwrap_or(field_elev_ft + 50.0);
    let highest = minima::limiting_terrain(patch, airport.lat, airport.lon, track, kind, field_elev_ft).unwrap_or(field_elev_ft);
    let est = minima::estimate(kind, field_elev_ft, highest);

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

    // Title block
    let top = H - MARGIN;
    text(&mut c, b, 16.0, MARGIN, top - 14.0, &format!("{}  RW{}", airport.icao, procedure.runway), INK);
    text(&mut c, f, 9.0, MARGIN, top - 28.0, &format!("{} approach - final track {:.0}deg - threshold crossing {:.0} ft", kind.label(), track, crossing_ft), 0.3);
    text(&mut c, f, 8.0, MARGIN, top - 40.0, &format!("{:.4}, {:.4}   procedures from the simulator's navigation data ({})", airport.lat, airport.lon, airport.source), 0.45);
    line(&mut c, MARGIN, top - 46.0, W - MARGIN, top - 46.0, 0.8, INK);

    let plan_h = 330.0;
    let plan_y = top - 46.0 - 10.0 - plan_h;
    draw_plan(&mut c, f, b, patch, MARGIN, plan_y, W - 2.0 * MARGIN, plan_h, field_elev_ft, track, &legs, &procedure.runway, airport_dir);

    let prof_h = 150.0;
    let prof_y = plan_y - 12.0 - prof_h;
    draw_profile(&mut c, f, MARGIN, prof_y, W - 2.0 * MARGIN, prof_h, &legs, field_elev_ft, &est);

    let min_h = 110.0;
    let min_y = prof_y - 12.0 - min_h;
    draw_minima(&mut c, f, b, MARGIN, min_y, W - 2.0 * MARGIN, min_h, &est);

    let foot = "Generated from open data and the simulator's own navigation database. Not for real-world navigation: use the published chart.";
    text(&mut c, f, 7.0, MARGIN, MARGIN, foot, 0.45);
    text(&mut c, f, 7.0, MARGIN, MARGIN - 9.0, "Terrain: Copernicus DEM (ESA). Airport data: OpenStreetMap and the X-Plane Scenery Gateway.", 0.55);

    pdf.stream(content_id, &c.finish());
    std::fs::write(out, pdf.finish()).with_context(|| format!("write {}", out.display()))?;
    Ok(est)
}

/// The approach for a runway, or the first one the airport has.
pub fn pick<'a>(a: &'a AirportProcedures, runway: Option<&str>) -> Option<&'a Procedure> {
    let approaches = a.procedures.iter().filter(|p| p.kind == Kind::Approach);
    match runway {
        Some(r) => {
            let want = r.trim_start_matches("RW").to_uppercase();
            approaches.clone().find(|p| p.runway == want).or_else(|| approaches.clone().find(|p| p.runway.starts_with(&want)))
        }
        None => approaches.clone().max_by_key(|p| final_legs(p).len()),
    }
}
