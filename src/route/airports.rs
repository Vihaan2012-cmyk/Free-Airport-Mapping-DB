//! Airports, from the navigation database on this machine: where one is, and which lie
//! near a place with a runway long enough.
//!
//! Loaded once into a coarse spatial index and kept for the life of the process: the
//! ETOPS rule asks "what lies near here" inside a loop over every fix a route might touch,
//! and a fresh query of the database for each of those would dominate the time a plan
//! takes far more than the search itself does.

use super::spatial::Grid;
use crate::dispatch::{Airport, LatLon};
use std::collections::HashMap;
use std::sync::OnceLock;

struct Entry {
    airport: Airport,
    longest_runway_ft: f64,
}

fn index() -> &'static Grid<Entry> {
    static INDEX: OnceLock<Grid<Entry>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut grid = Grid::new(3.0);
        // Every runway in one pass, then the longest at each airport: asking the database
        // per airport means opening it twenty thousand times over, which costs forty
        // seconds against the eighty-seven milliseconds a route search takes.
        let mut longest_at: HashMap<String, f64> = HashMap::new();
        for r in crate::sources::navdata::all_runways() {
            let entry = longest_at.entry(r.icao).or_insert(0.0);
            *entry = entry.max(r.length_ft);
        }
        for row in crate::sources::navdata::all_airports() {
            let longest = longest_at.get(&row.icao).copied().unwrap_or(0.0);
            let pos = (row.lat, row.lon);
            grid.insert(pos, Entry { airport: Airport { icao: row.icao, name: row.name, pos, elevation_ft: row.elevation_ft }, longest_runway_ft: longest });
        }
        grid
    })
}

/// An airport by its ICAO code.
pub fn airport(icao: &str) -> Option<Airport> {
    let wanted = icao.trim().to_uppercase();
    let idx = index();
    (0..idx.len() as u32).map(|i| idx.get(i).0).find(|e| e.airport.icao == wanted).map(|e| e.airport.clone())
}

/// The airports within a distance of a place whose longest runway is at least a length,
/// nearest first, with that runway's length in feet.
pub fn airports_near(at: LatLon, radius_nm: f64, min_runway_ft: f64) -> Vec<(Airport, f64)> {
    let idx = index();
    idx.near(at, radius_nm)
        .into_iter()
        .filter_map(|(i, d)| {
            let (e, _) = idx.get(i);
            (e.longest_runway_ft >= min_runway_ft).then(|| (e.airport.clone(), d))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // `index()` reads the real navigation database (or finds none), so these are smoke
    // tests of the plumbing rather than of any particular airport: they must pass whether
    // or not this machine has a database installed.

    #[test]
    fn an_unknown_airport_is_none() {
        assert!(airport("ZZZZ9").is_none());
    }

    #[test]
    fn airports_near_nowhere_finds_nothing_that_is_not_there() {
        // The middle of the Pacific, far from anything: whatever the database on this
        // machine holds, nothing real is within a nautical mile of it.
        assert!(airports_near((0.0, -160.0), 1.0, 0.0).is_empty());
    }
}
