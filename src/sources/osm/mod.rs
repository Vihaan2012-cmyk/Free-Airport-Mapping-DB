//! OpenStreetMap: the OSM map API (default) or Overpass (fallback), mapped through `tags`,
//! or a downloaded extract read ahead of a large build (`extract`).

pub mod elements;
pub mod extract;
pub mod osmapi;
pub mod overpass;
pub mod tags;

/// Some OpenStreetMap data is saved for this airport (from the map API, Overpass or an
/// extract), without reading it.
pub fn has_cached(cache: &crate::cache::Cache, icao: &str) -> bool {
    let icao = icao.to_uppercase();
    [extract::cache_key(&icao), format!("osm/osmapi/{icao}.json"), format!("osm/overpass/{icao}.json")].iter().any(|k| cache.path(k).is_some_and(|p| p.is_file()))
}

/// A saved download of this airport from either source, if one is still fresh. Each
/// source files its data under its own name and in its own format, and which source an
/// airport is sent to depends on its place in the list, so after a restart the same
/// airport can be sent to the other one. Looking in both avoids downloading it again.
pub fn cached(cache: &crate::cache::Cache, icao: &str) -> Option<(elements::Store, &'static str)> {
    let icao = icao.to_uppercase();
    let not_cached = || Err(anyhow::anyhow!("not cached"));
    if let Ok(text) = cache.get_or_fetch_text(&extract::cache_key(&icao), not_cached) {
        if let Ok(store) = serde_json::from_str::<elements::Store>(&text) {
            return Some((store, "OSM extract"));
        }
    }
    if let Ok(text) = cache.get_or_fetch_text(&format!("osm/osmapi/{icao}.json"), not_cached) {
        if let Ok(store) = serde_json::from_str::<elements::Store>(&text) {
            return Some((store, "saved map API download"));
        }
    }
    if let Ok(text) = cache.get_or_fetch_text(&format!("osm/overpass/{icao}.json"), not_cached) {
        if let Ok(store) = overpass::parse_json(&text) {
            return Some((store, "saved Overpass download"));
        }
    }
    None
}
