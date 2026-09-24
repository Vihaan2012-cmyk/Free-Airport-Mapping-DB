//! Where a flight may be planned point to point, and where it may not.
//!
//! A free-route direct leg between two fixes is always at least as short as the airway between
//! them, because an airway is a line through more points than two. Left to itself a search
//! therefore takes a direct every time, and comes out with a route across Europe and Asia made
//! of twenty consecutive `DCT` legs — the shortest way through the network, and not a route any
//! controller would accept over China or India.
//!
//! The answer is not to price directs higher. That was tried four ways and measured: a
//! thirty per cent surcharge per mile, a twenty-five mile surcharge per leg, and thinning the
//! direct-leg mesh from six neighbours to one all moved London to Sydney from eighteen direct
//! legs to seventeen while making the route one to two per cent longer. They could not do
//! better, because the fixes at the ends of those legs share no airway at all: there was never
//! an airway alternative for the price to steer towards.
//!
//! What actually decides it is airspace. Europe above FL195 has been free route since the end
//! of 2022 and a direct there is ordinary; China and India are airways and a direct there is
//! not a flight plan. So this carries where free routing is implemented, from
//! `data/free_route.json`, and a direct leg outside it costs infinity — which is to say it is
//! not offered at all.
//!
//! The lookup has to be cheap: it is asked once per direct leg per level, in the hot loop of
//! the search. So the areas are rasterised once into a coarse grid of the lowest level free
//! routing begins at in each cell, and a lookup is then an index and a comparison.

use crate::dispatch::LatLon;
use serde::Deserialize;

/// How wide a grid cell is, in degrees. Half a degree is about thirty miles, which is finer
/// than the certainty with which any of this is known and coarse enough that the whole world is
/// a quarter of a megabyte.
const CELL_DEG: f64 = 0.5;
const COLS: usize = (360.0 / CELL_DEG) as usize;
const ROWS: usize = (180.0 / CELL_DEG) as usize;

/// The flight level at or above which a cell is free route. `NONE` is "never".
const NONE: u16 = u16::MAX;

#[derive(Deserialize)]
struct Areas {
    areas: Vec<Area>,
}

#[derive(Deserialize)]
struct Area {
    #[serde(default)]
    name: String,
    #[serde(default)]
    reference: String,
    /// Regions named exactly, for oceanic control areas whose prefix also covers domestic
    /// airspace that is not free route.
    #[serde(default)]
    firs: Vec<String>,
    /// Regions named by the first letters of their identifier, for a whole continent's worth.
    #[serde(default)]
    prefixes: Vec<String>,
    floor_fl: f64,
    #[serde(default)]
    ceiling_fl: f64,
}

fn data() -> &'static Areas {
    static DATA: std::sync::OnceLock<Areas> = std::sync::OnceLock::new();
    DATA.get_or_init(|| serde_json::from_str(include_str!("../../data/free_route.json")).unwrap_or(Areas { areas: Vec::new() }))
}

/// The lowest level free routing begins at in each cell of the world, or [`NONE`].
///
/// Built once, from the navigation database's own region outlines, and kept. Where the data has
/// no region of a name an area asks for, that area simply covers nothing: an area is a claim
/// about airspace we can find, not a licence to assume.
fn grid() -> &'static Vec<u16> {
    static GRID: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
    GRID.get_or_init(build)
}

fn build() -> Vec<u16> {
    let mut grid = vec![NONE; COLS * ROWS];
    let known = crate::sources::navdata::region_idents();
    if known.is_empty() {
        log::warn!("free route: the navigation database has no region outlines, so every direct leg is allowed");
        return vec![0; COLS * ROWS];
    }

    let mut painted = 0usize;
    for area in &data().areas {
        // The regions this area covers: named exactly, or by the start of their name.
        let mut want: Vec<String> = area.firs.iter().map(|f| f.to_uppercase()).filter(|f| known.contains(f)).collect();
        for prefix in &area.prefixes {
            let p = prefix.to_uppercase();
            want.extend(known.iter().filter(|k| k.starts_with(&p)).cloned());
        }
        want.sort();
        want.dedup();
        if want.is_empty() {
            log::info!("free route: {} ({}) names no region this navigation data has", area.name, area.reference);
            continue;
        }

        let floor = (area.floor_fl.max(0.0) as u16).min(NONE - 1);
        for region in crate::sources::navdata::regions(&want) {
            for part in &region.parts {
                paint(&mut grid, part, floor);
                painted += 1;
            }
        }
    }
    log::info!("free route: {} area outline(s) from {} area(s)", painted, data().areas.len());
    grid
}

/// Mark every cell whose middle falls inside a region with the lowest floor claimed for it.
///
/// Longitudes are unrolled about the outline's own first point before anything is compared: an
/// oceanic region such as Oakland's straddles the date line, and in raw longitude its points sit
/// at both ends of the scale, so its box is the width of the world and a ray cast through it
/// paints most of Asia. Unrolled, it is a region a few tens of degrees across like any other.
fn paint(grid: &mut [u16], polygon: &[LatLon], floor: u16) {
    if polygon.len() < 3 {
        return;
    }
    let origin = polygon[0].1;
    let unroll = |lon: f64| origin + ((lon - origin + 180.0).rem_euclid(360.0)) - 180.0;
    let ring: Vec<LatLon> = polygon.iter().map(|p| (p.0, unroll(p.1))).collect();

    let (mut south, mut north) = (90.0f64, -90.0f64);
    let (mut west, mut east) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in &ring {
        south = south.min(p.0);
        north = north.max(p.0);
        west = west.min(p.1);
        east = east.max(p.1);
    }
    // An outline that really does wrap the world once unrolled is not one to trust, and
    // painting from it would mark a band right round the earth.
    if east - west >= 355.0 || north - south >= 179.0 {
        return;
    }

    let (r0, r1) = (row_of(north), row_of(south));
    for r in r0..=r1.min(ROWS - 1) {
        let lat = 90.0 - (r as f64 + 0.5) * CELL_DEG;
        if lat < south || lat > north {
            continue;
        }
        let mut lon = (west / CELL_DEG).floor() * CELL_DEG;
        while lon <= east {
            if inside((lat, lon), &ring) {
                let c = col_of(((lon + 180.0).rem_euclid(360.0)) - 180.0);
                let cell = &mut grid[r * COLS + c];
                *cell = (*cell).min(floor);
            }
            lon += CELL_DEG;
        }
    }
}

fn row_of(lat: f64) -> usize {
    (((90.0 - lat) / CELL_DEG).floor().max(0.0) as usize).min(ROWS - 1)
}

fn col_of(lon: f64) -> usize {
    (((lon + 180.0) / CELL_DEG).floor().max(0.0) as usize).min(COLS - 1)
}

/// The usual ray cast, in degrees. Good enough for an outline whose own points are a tenth of a
/// degree apart and which is being asked about at half-degree steps.
fn inside(p: LatLon, polygon: &[LatLon]) -> bool {
    let mut hit = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[j]);
        if (a.0 > p.0) != (b.0 > p.0) {
            let span = b.0 - a.0;
            if span.abs() > f64::EPSILON && p.1 < a.1 + (b.1 - a.1) * (p.0 - a.0) / span {
                hit = !hit;
            }
        }
        j = i;
    }
    hit
}

/// Whether a flight at this level may be planned point to point here.
pub fn permits(at: LatLon, level_ft: f64) -> bool {
    let floor = grid()[row_of(at.0) * COLS + col_of(at.1)];
    floor != NONE && level_ft >= floor as f64 * 100.0
}

/// Whether a direct leg between two places may be flown at all: both ends have to be somewhere
/// it is allowed, and so does the middle, since a leg that leaves free route airspace on the way
/// across is no more filable than one that starts outside it.
pub fn permits_leg(from: LatLon, to: LatLon, level_ft: f64) -> bool {
    permits(from, level_ft) && permits(to, level_ft) && permits(crate::dispatch::along(from, to, 0.5), level_ft)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The data file has to parse, and to say something: an empty list would silently allow
    /// every direct leg everywhere, which is what this exists to stop.
    #[test]
    fn the_areas_are_read_and_are_not_empty() {
        let areas = data();
        assert!(areas.areas.len() >= 4, "{} areas", areas.areas.len());
        for a in &areas.areas {
            assert!(!a.reference.trim().is_empty(), "{} has no reference", a.name);
            assert!(!a.firs.is_empty() || !a.prefixes.is_empty(), "{} names no regions", a.name);
            assert!(a.floor_fl >= 0.0 && a.floor_fl < 700.0, "{} floor {}", a.name, a.floor_fl);
            assert!(a.ceiling_fl > a.floor_fl, "{} ceiling {}", a.name, a.ceiling_fl);
        }
    }

    /// A point inside a square is inside it, and one outside is not.
    #[test]
    fn the_ray_cast_knows_inside_from_outside() {
        let square = [(10.0, 10.0), (10.0, 20.0), (20.0, 20.0), (20.0, 10.0)];
        assert!(inside((15.0, 15.0), &square));
        assert!(!inside((15.0, 25.0), &square));
        assert!(!inside((5.0, 15.0), &square));
    }

    /// Painting a region marks the cells inside it and leaves the rest alone, and a second area
    /// claiming a lower floor over the same ground wins, because the lower one is the one a
    /// flight can actually use.
    #[test]
    fn painting_marks_the_inside_and_keeps_the_lowest_floor() {
        let mut grid = vec![NONE; COLS * ROWS];
        let square = [(10.0, 10.0), (10.0, 20.0), (20.0, 20.0), (20.0, 10.0)];
        paint(&mut grid, &square, 195);
        assert_eq!(grid[row_of(15.0) * COLS + col_of(15.0)], 195);
        assert_eq!(grid[row_of(15.0) * COLS + col_of(25.0)], NONE);
        paint(&mut grid, &square, 55);
        assert_eq!(grid[row_of(15.0) * COLS + col_of(15.0)], 55);
        paint(&mut grid, &square, 390);
        assert_eq!(grid[row_of(15.0) * COLS + col_of(15.0)], 55, "a higher floor does not undo a lower one");
    }
}

#[cfg(test)]
mod wrap_tests {
    use super::*;

    /// A region across the date line paints itself and not the far side of the world. Before
    /// longitudes were unrolled, Oakland's oceanic area marked Iran, India and China as free
    /// route, because its box in raw longitude was the width of the earth.
    #[test]
    fn a_region_across_the_date_line_paints_only_itself() {
        let mut grid = vec![NONE; COLS * ROWS];
        // Twenty degrees either side of the date line.
        let across = [(10.0, 170.0), (10.0, -170.0), (30.0, -170.0), (30.0, 170.0)];
        paint(&mut grid, &across, 55);
        assert_eq!(grid[row_of(20.0) * COLS + col_of(175.0)], 55, "inside, east of the line");
        assert_eq!(grid[row_of(20.0) * COLS + col_of(-175.0)], 55, "inside, west of the line");
        assert_eq!(grid[row_of(20.0) * COLS + col_of(60.0)], NONE, "Iran is not in it");
        assert_eq!(grid[row_of(20.0) * COLS + col_of(100.0)], NONE, "nor is south-east Asia");
        assert_eq!(grid[row_of(20.0) * COLS + col_of(-60.0)], NONE, "nor is the Atlantic");
    }
}

#[cfg(test)]
mod report {
    use super::*;

    /// Printed, not asserted: what the grid says at places whose answer is known, so a rule
    /// that quietly permits everything is visible rather than inferred from routes not changing.
    #[test]
    #[ignore]
    fn report_where_free_routing_is_permitted() {
        let cells = grid().iter().filter(|&&f| f != NONE).count();
        println!("cells with free routing: {} of {} ({:.1}%)", cells, COLS * ROWS, 100.0 * cells as f64 / (COLS * ROWS) as f64);
        for (name, at) in [
            ("London", (51.5, -0.5)),
            ("central France", (47.0, 3.0)),
            ("Poland", (52.0, 20.0)),
            ("Turkey", (39.0, 33.0)),
            ("Iran", (32.0, 54.0)),
            ("India", (22.0, 78.0)),
            ("China", (35.0, 105.0)),
            ("Indonesia", (-2.0, 115.0)),
            ("Australia", (-25.0, 134.0)),
            ("mid-Atlantic", (50.0, -30.0)),
            ("mid-Pacific", (20.0, -160.0)),
            ("Kansas", (38.0, -98.0)),
            ("Arabian Sea", (16.3, 72.5)),
            ("off Oman", (19.6, 45.6)),
            ("Red Sea", (24.6, 36.8)),
            ("Egypt", (30.7, 28.7)),
            ("Balkans", (36.9, 20.2)),
            ("N Atlantic 45W", (44.5, -41.5)),
        ] {
            let f = grid()[row_of(at.0) * COLS + col_of(at.1)];
            let floor = if f == NONE { "never".to_string() } else { format!("FL{f}") };
            println!("  {name:16} {floor:>8}   at FL350: {}", permits(at, 35_000.0));
        }
    }
}
