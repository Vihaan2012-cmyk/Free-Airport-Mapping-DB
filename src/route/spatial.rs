//! A coarse spatial index: places bucketed into one-degree cells of latitude and
//! longitude, so that "what lies near here" costs a handful of lookups rather than a scan
//! of everything there is. Good enough for airports and fixes, which never crowd a single
//! degree cell by more than a few hundred; not meant for anything denser.

use crate::dispatch::{distance_nm, LatLon};
use std::collections::HashMap;

/// An index of places by position, each keeping the payload it was inserted with.
pub struct Grid<T> {
    cell_deg: f64,
    cells: HashMap<(i32, i32), Vec<u32>>,
    pos: Vec<LatLon>,
    items: Vec<T>,
}

fn cell_of(p: LatLon, cell_deg: f64) -> (i32, i32) {
    ((p.0 / cell_deg).floor() as i32, (p.1 / cell_deg).floor() as i32)
}

impl<T> Grid<T> {
    /// A new, empty index; `cell_deg` is the width of a cell, degrees. Two or three
    /// degrees suits airports and fixes worldwide: enough that a search at a realistic
    /// radius touches only a handful of neighbouring cells.
    pub fn new(cell_deg: f64) -> Grid<T> {
        Grid { cell_deg, cells: HashMap::new(), pos: Vec::new(), items: Vec::new() }
    }

    pub fn insert(&mut self, at: LatLon, item: T) {
        let i = self.pos.len() as u32;
        self.pos.push(at);
        self.items.push(item);
        self.cells.entry(cell_of(at, self.cell_deg)).or_default().push(i);
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, i: u32) -> (&T, LatLon) {
        (&self.items[i as usize], self.pos[i as usize])
    }

    /// Every item within `radius_nm` of a point, nearest first.
    pub fn near(&self, at: LatLon, radius_nm: f64) -> Vec<(u32, f64)> {
        let cos = at.0.to_radians().cos().max(0.05);
        // A cell's width in nautical miles shrinks with latitude in longitude but not in
        // latitude, so the search spreads over enough cells to be sure of catching
        // everything within the radius: the wider of the two spans either way.
        let lat_cells = (radius_nm / 60.0 / self.cell_deg).ceil() as i32 + 1;
        let lon_cells = (radius_nm / 60.0 / cos / self.cell_deg).ceil() as i32 + 1;
        let (cy, cx) = cell_of(at, self.cell_deg);
        let mut out = Vec::new();
        for dy in -lat_cells..=lat_cells {
            for dx in -lon_cells..=lon_cells {
                let Some(bucket) = self.cells.get(&(cy + dy, cx + dx)) else { continue };
                for &i in bucket {
                    let d = distance_nm(at, self.pos[i as usize]);
                    if d <= radius_nm {
                        out.push((i, d));
                    }
                }
            }
        }
        out.sort_by(|a, b| a.1.total_cmp(&b.1));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_what_is_near_and_not_what_is_far() {
        let mut g: Grid<&str> = Grid::new(2.0);
        g.insert((51.5, -0.1), "LONDON");
        g.insert((48.9, 2.3), "PARIS");
        g.insert((40.7, -74.0), "NEW YORK");
        let near_london = g.near((51.47, 0.45), 60.0);
        assert_eq!(near_london.len(), 1);
        assert_eq!(g.get(near_london[0].0).0, &"LONDON");
        let near_europe = g.near((50.0, 1.0), 400.0);
        assert_eq!(near_europe.len(), 2);
    }

    #[test]
    fn crosses_a_cell_boundary() {
        let mut g: Grid<u32> = Grid::new(1.0);
        for i in 0..5 {
            g.insert((0.05, i as f64 * 0.9 - 1.8), i);
        }
        let found = g.near((0.0, 0.0), 80.0);
        assert!(found.len() >= 3, "{}", found.len());
    }
}
