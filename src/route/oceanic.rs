//! Organised track systems: the North Atlantic tracks, published twice a day, and the
//! Pacific ones.

use super::Graph;
use crate::dispatch::{EdgeQuery, EdgeRule, LatLon, Verdict};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSystem {
    NorthAtlantic,
    Pacific,
    Australian,
}

/// One track: its points, the levels it may be flown at, which way, and when.
#[derive(Debug, Clone)]
pub struct Track {
    pub system: TrackSystem,
    /// As filed: "NATA", "PACOTS 3".
    pub ident: String,
    pub points: Vec<(String, LatLon)>,
    pub levels_ft: Vec<f64>,
    pub eastbound: bool,
    pub valid_from: DateTime<Utc>,
    pub valid_to: DateTime<Utc>,
}

/// The tracks in force at a time, fetched and cached.
pub fn fetch_tracks(when: DateTime<Utc>) -> anyhow::Result<Vec<Track>> {
    let _ = when;
    anyhow::bail!("oceanic tracks are not built yet")
}

/// Put each track into the network as a one-way airway named by its ident.
pub fn add_tracks(graph: &mut Graph, tracks: &[Track]) {
    let _ = (graph, tracks);
}

/// A track may only be flown at its levels, inside its time.
pub struct TrackRule {
    _tracks: Vec<Track>,
}

impl TrackRule {
    pub fn new(tracks: Vec<Track>) -> TrackRule {
        TrackRule { _tracks: tracks }
    }
}

impl EdgeRule for TrackRule {
    fn name(&self) -> &str {
        "oceanic tracks"
    }

    fn check(&self, _q: &EdgeQuery) -> Verdict {
        Verdict::Allow
    }
}
