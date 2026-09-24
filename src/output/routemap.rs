//! A picture of what the route search saw and what it chose.
//!
//! A flight plan says where the route went; it does not say why, and on a long sector "why" is
//! usually geometric. A route that wanders a thousand miles west before turning north has been
//! led there by something — the shape of the region it was allowed to search in, the airways
//! that happened to be inside it, a rule that forbade the obvious way — and reading that out of
//! a list of fix names is slow work. Drawn on a world map, against the network the search
//! actually sees, it is immediate.
//!
//! So this draws, in the order a reader wants them: the network's own fixes as a faint wash,
//! which is both the shape of the world and the only thing the search can route through; the
//! graticule; the great circle between the two airports; the ellipse the search was confined
//! to; the strips the winds were fetched over; and the route itself.

use crate::dispatch::{along, bearing_deg, distance_nm, travel, Bounds, LatLon};
use crate::output::canvas::{Canvas, Raster};
use pdf_writer::Name;

/// The two faces a chart is set in, named as the PDF resources name them.
const REGULAR: Name = Name(b"F");
const BOLD: Name = Name(b"B");
use anyhow::Result;
use std::path::Path;

/// What to draw on one map.
pub struct Map {
    pub origin: LatLon,
    pub destination: LatLon,
    pub origin_name: String,
    pub destination_name: String,
    /// The route as flown, in order, with each fix's name.
    pub route: Vec<(String, LatLon)>,
    /// Every ellipse the search climbed through, widest last, each with the factor that drew it.
    pub ellipses: Vec<(f64, Vec<LatLon>)>,
    /// The rectangles the winds were asked for.
    pub corridor: Vec<Bounds>,
    /// The network's own fixes, drawn as the backdrop.
    pub network: Vec<LatLon>,
    /// Where the search actually went, sampled as it went there. This is the part a list of
    /// fixes cannot show: not the route, but everywhere the route was nearly.
    pub explored: Vec<LatLon>,
    /// The shortest way through the network by distance alone: not a route, but the floor no
    /// route can beat, so the gap between it and the route is what the search is losing.
    pub floor: Vec<LatLon>,
    pub caption: String,
}

const WIDTH: f32 = 1440.0;
const HEIGHT: f32 = 760.0;
const MARGIN: f32 = 28.0;
/// How much of the page the margin takes at the size the map was drawn for, so a small panel
/// keeps the same proportions rather than losing most of itself to a fixed border.
const MARGIN_SHARE: f32 = MARGIN / WIDTH;

/// Where a place falls on the page.
///
/// Equirectangular, because the point of this picture is to see where things are in relation to
/// one another over a whole hemisphere, and every projection that flatters the shape of a
/// country distorts that. The map is centred on a longitude of the caller's choosing so a route
/// across the date line is drawn in one piece instead of leaving one edge and arriving at the
/// other.
struct Frame {
    centre_lon: f64,
    /// The degrees of longitude and latitude the page covers, and the middle of the band of
    /// latitude it covers. Fitted to what is drawn rather than always the whole world: a
    /// Singapore to Brisbane route on a world map is a dot, and a dot says nothing.
    span_lon: f64,
    span_lat: f64,
    centre_lat: f64,
    width: f32,
    height: f32,
    margin: f32,
}

impl Frame {
    /// A frame holding everything given, with room round it, at the page's own shape.
    ///
    /// Longitudes are unrolled about the first point before they are compared, so a route
    /// across the date line reads as a short run rather than as two clusters a full turn apart,
    /// and the span is then widened to the page's aspect so that a degree across and a degree
    /// down are drawn at the same scale. Without that a short north-south route would be
    /// stretched across the width and read as something it is not.
    fn fit(points: &[LatLon], width: f32, height: f32, margin: f32) -> Frame {
        let plain = Frame { centre_lon: 0.0, span_lon: 360.0, span_lat: 180.0, centre_lat: 0.0, width, height, margin };
        let Some(&first) = points.first() else { return plain };

        let (mut lo_lat, mut hi_lat) = (first.0, first.0);
        let (mut lo_lon, mut hi_lon) = (0.0f64, 0.0f64);
        for p in points {
            lo_lat = lo_lat.min(p.0);
            hi_lat = hi_lat.max(p.0);
            let d = ((p.1 - first.1 + 180.0).rem_euclid(360.0)) - 180.0;
            lo_lon = lo_lon.min(d);
            hi_lon = hi_lon.max(d);
        }

        // A fifth again around what was asked for, and never so tight that a single place
        // becomes a page of one fix.
        const ROOM: f64 = 1.2;
        const LEAST_DEG: f64 = 6.0;
        let centre_lat = ((lo_lat + hi_lat) / 2.0).clamp(-85.0, 85.0);
        let centre_lon = first.1 + (lo_lon + hi_lon) / 2.0;
        let mut span_lat = ((hi_lat - lo_lat) * ROOM).max(LEAST_DEG);
        let mut span_lon = ((hi_lon - lo_lon) * ROOM).max(LEAST_DEG);

        // Widen whichever way round is needed so the page's shape does not distort the scale.
        let page = (width / height) as f64;
        if span_lon / span_lat < page {
            span_lon = span_lat * page;
        } else {
            span_lat = span_lon / page;
        }
        Frame { centre_lon, span_lon: span_lon.min(360.0), span_lat: span_lat.min(180.0), centre_lat, width, height, margin }
    }

    fn at(&self, p: LatLon) -> (f32, f32) {
        let lon = ((p.1 - self.centre_lon + 180.0).rem_euclid(360.0)) - 180.0;
        let x = self.margin + ((lon / self.span_lon + 0.5) as f32) * (self.width - 2.0 * self.margin);
        // The canvas counts y upwards, as a PDF does, so north is the larger figure.
        let y = self.margin + (((p.0 - self.centre_lat) / self.span_lat + 0.5) as f32) * (self.height - 2.0 * self.margin);
        (x, y)
    }

    /// Whether a place falls on the page at all, so the network's hundred thousand fixes are
    /// not all asked to be drawn when a dozen degrees of it are in view.
    fn holds(&self, p: LatLon) -> bool {
        let lon = ((p.1 - self.centre_lon + 180.0).rem_euclid(360.0)) - 180.0;
        lon.abs() <= self.span_lon / 2.0 && (p.0 - self.centre_lat).abs() <= self.span_lat / 2.0
    }

    /// Whether a leg crosses the page's own seam, where drawing a straight line between two
    /// points would run the wrong way across the whole map.
    fn wraps(&self, a: LatLon, b: LatLon) -> bool {
        let f = |p: LatLon| ((p.1 - self.centre_lon + 180.0).rem_euclid(360.0)) - 180.0;
        (f(a) - f(b)).abs() > 180.0
    }

    /// The degrees of latitude and longitude a graticule should be drawn at: close enough
    /// together to read a position off, far enough apart not to become a mesh.
    fn grid_step(&self) -> f64 {
        [1.0, 2.0, 5.0, 10.0, 15.0, 30.0].into_iter().find(|s| self.span_lon / s <= 14.0).unwrap_or(30.0)
    }
}

/// A line through a run of places, broken wherever it would cross the page's seam.
fn polyline(c: &mut dyn Canvas, frame: &Frame, points: &[LatLon]) {
    let mut started = false;
    for pair in points.windows(2) {
        if frame.wraps(pair[0], pair[1]) {
            if started {
                c.stroke();
                started = false;
            }
            continue;
        }
        if !started {
            let (x, y) = frame.at(pair[0]);
            c.move_to(x, y);
            started = true;
        }
        let (x, y) = frame.at(pair[1]);
        c.line_to(x, y);
    }
    if started {
        c.stroke();
    }
}

/// The great circle between two places, as a run of points dense enough to draw as a curve.
pub fn great_circle(a: LatLon, b: LatLon, steps: usize) -> Vec<LatLon> {
    (0..=steps).map(|k| along(a, b, k as f64 / steps as f64)).collect()
}

/// The outline of an ellipse with two places as its foci: out along one side and back along the
/// other, which is the order it has to be drawn in to close.
pub fn ellipse_outline(origin: LatLon, destination: LatLon, half_width: &dyn Fn(f64) -> f64, steps: usize) -> Vec<LatLon> {
    let mut out = Vec::with_capacity(steps * 2 + 3);
    let side = |f: f64, sign: f64| {
        let at = along(origin, destination, f);
        let ahead = along(origin, destination, (f + 1e-4).min(1.0));
        let track = if distance_nm(at, ahead) > 1e-9 { bearing_deg(at, ahead) } else { bearing_deg(origin, destination) };
        travel(at, track + 90.0 * sign, half_width(f))
    };
    for k in 0..=steps {
        out.push(side(k as f64 / steps as f64, 1.0));
    }
    for k in (0..=steps).rev() {
        out.push(side(k as f64 / steps as f64, -1.0));
    }
    if let Some(&first) = out.first() {
        out.push(first);
    }
    out
}

pub fn write(map: &Map, out: &Path, scale: f32) -> Result<()> {
    let mut r = Raster::new(WIDTH, HEIGHT, scale)?;
    draw(&mut r, map, WIDTH, HEIGHT);
    r.write_png(out)
}

/// The same picture as a PNG in memory, at whatever size the caller has room for: what a panel
/// inside a window draws itself from, a few times a second, while a search is running.
pub fn png_bytes(map: &Map, width: f32, height: f32, scale: f32) -> Result<Vec<u8>> {
    let mut r = Raster::new(width, height, scale)?;
    draw(&mut r, map, width, height);
    r.png_bytes()
}

fn draw(c: &mut dyn Canvas, map: &Map, width: f32, height: f32) {
    // Fitted to the route and everywhere the search went, not to the world: Singapore to
    // Brisbane drawn on a whole globe is a dot with an ocean of nothing round it.
    let mut held: Vec<LatLon> = Vec::with_capacity(map.route.len() + map.explored.len() + 2);
    held.push(map.origin);
    held.push(map.destination);
    held.extend(map.route.iter().map(|(_, p)| *p));
    held.extend(map.explored.iter().copied());
    held.extend(map.floor.iter().copied());
    if held.iter().all(|p| p.0 == 0.0 && p.1 == 0.0) {
        held.clear();
    }
    let frame = Frame::fit(&held, width, height, width * MARGIN_SHARE);

    // The network, as a wash of single pixels. It is the backdrop and the subject at once: the
    // shape it makes is the shape of the inhabited world, and it is also the whole of what the
    // search has to route through, so a bare patch on this picture is a bare patch in the plan.
    c.set_fill_rgb(0.78, 0.80, 0.84);
    for &p in map.network.iter().filter(|&&p| frame.holds(p)) {
        let (x, y) = frame.at(p);
        c.rect(x, y, 1.2, 1.2);
    }
    c.fill_nonzero();

    // The graticule, thirty degrees apart, labelled down the left and along the bottom.
    c.set_stroke_rgb(0.87, 0.88, 0.90);
    c.set_line_width(0.6);
    let step = frame.grid_step();
    let (west, east) = (frame.centre_lon - frame.span_lon / 2.0, frame.centre_lon + frame.span_lon / 2.0);
    let (south, north) = ((frame.centre_lat - frame.span_lat / 2.0).max(-89.0), (frame.centre_lat + frame.span_lat / 2.0).min(89.0));
    let mut lat = (south / step).ceil() * step;
    while lat <= north {
        let run: Vec<LatLon> = (0..=24).map(|k| (lat, west + (east - west) * k as f64 / 24.0)).collect();
        polyline(c, &frame, &run);
        lat += step;
    }
    let mut lon = (west / step).ceil() * step;
    while lon <= east {
        let run: Vec<LatLon> = (0..=24).map(|k| (south + (north - south) * k as f64 / 24.0, lon)).collect();
        polyline(c, &frame, &run);
        lon += step;
    }

    // Everywhere the search looked, under everything else, so the route is read against the
    // spread of what it was chosen from.
    c.set_fill_rgb(0.90, 0.72, 0.36);
    for &p in map.explored.iter().filter(|&&p| frame.holds(p)) {
        let (x, y) = frame.at(p);
        c.rect(x - 1.25, y - 1.25, 2.5, 2.5);
    }
    c.fill_nonzero();

    // The strips the winds were asked over.
    c.set_stroke_rgb(0.42, 0.62, 0.86);
    c.set_line_width(1.0);
    c.set_dash(&[3.0, 3.0], 0.0);
    for b in &map.corridor {
        let ring = [(b.south, b.west), (b.north, b.west), (b.north, b.east), (b.south, b.east), (b.south, b.west)];
        polyline(c, &frame, &ring);
    }

    // The ellipses, thinnest first, so the ladder the search climbed is visible as a set of
    // nested outlines rather than one shape.
    for (i, (factor, outline)) in map.ellipses.iter().enumerate() {
        let shade = 0.30 + 0.12 * i as f32;
        c.set_stroke_rgb(shade, 0.55 + 0.08 * i as f32, 0.35);
        c.set_line_width(1.2);
        c.set_dash(&[6.0, 4.0], 0.0);
        polyline(c, &frame, outline);
        if let Some(&p) = outline.first() {
            let (x, y) = frame.at(p);
            c.text(REGULAR, 9.0, x + 3.0, y - 3.0, &format!("x{factor:.2}"), 0.45);
        }
    }

    // The great circle: what the route would be if nothing were in the way.
    c.set_dash(&[2.0, 4.0], 0.0);
    c.set_stroke_rgb(0.45, 0.45, 0.50);
    c.set_line_width(1.2);
    polyline(c, &frame, &great_circle(map.origin, map.destination, 240));
    c.set_dash(&[], 0.0);

    // The floor: the shortest way through this network, whatever the rules and the wind say.
    // Where the route runs well clear of it, the miles between the two are the search's.
    c.set_stroke_rgb(0.20, 0.55, 0.30);
    c.set_line_width(1.6);
    polyline(c, &frame, &map.floor);

    // The route.
    let line: Vec<LatLon> = map.route.iter().map(|(_, p)| *p).collect();
    c.set_stroke_rgb(0.80, 0.12, 0.16);
    c.set_line_width(2.2);
    polyline(c, &frame, &line);

    c.set_fill_rgb(0.80, 0.12, 0.16);
    for (_, p) in &map.route {
        let (x, y) = frame.at(*p);
        c.rect(x - 1.5, y - 1.5, 3.0, 3.0);
    }
    c.fill_nonzero();

    // The two airports, named, and every fix named where the route is sparse enough to read.
    let step = (map.route.len() / 14).max(1);
    for (i, (name, p)) in map.route.iter().enumerate() {
        if i % step != 0 && i != 0 && i + 1 != map.route.len() {
            continue;
        }
        let (x, y) = frame.at(*p);
        c.text(REGULAR, 8.5, x + 4.0, y - 4.0, name, 0.25);
    }
    for (p, name) in [(map.origin, &map.origin_name), (map.destination, &map.destination_name)] {
        let (x, y) = frame.at(p);
        c.set_fill_rgb(0.05, 0.25, 0.65);
        c.rect(x - 3.0, y - 3.0, 6.0, 6.0);
        c.fill_nonzero();
        c.text(BOLD, 11.0, x + 6.0, y + 4.0, name, 0.1);
    }

    c.text(BOLD, 13.0, frame.margin, height - 10.0, &map.caption, 0.1);
    c.text(
        REGULAR,
        9.0,
        frame.margin,
        height - 24.0,
        "grey: network   amber: where the search looked   dotted: great circle   dashed green: ellipse   dashed blue: wind strips   solid green: shortest possible   red: route",
        0.42,
    );
}
