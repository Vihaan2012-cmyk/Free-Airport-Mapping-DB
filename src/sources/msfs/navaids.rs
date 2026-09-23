//! Beacons: where they are, and what they are called.
//!
//! An approach outside the United States is usually written against a beacon rather than
//! against named waypoints: Madeira's is a string of positions on radials from the
//! Funchal DVOR/DME, and a fix called "FUN12" is simply twelve miles out on one of them.
//! Those fixes have no position of their own in the data, so without the beacon there is
//! nothing to draw.
//!
//! Layout, worked out from the files and checked against published frequencies and
//! positions:
//!
//! ```text
//! VOR 0x13   longitude +0x08  latitude +0x0C  elevation +0x10  frequency Hz  +0x14  ident +0x20
//! NDB 0x17   frequency +0x08  longitude +0x0C  latitude +0x10  elevation +0x14  ident +0x20
//! ```
//!
//! Both carry their name as a child record, which we do not need. Checked against
//! Funchal (112.20, N32 44 50 W016 42 20) and Lambourne (115.60), which match the
//! published plates exactly.

use super::bgl;
use std::collections::HashMap;
use std::sync::OnceLock;

const SECTION_VOR: u32 = 0x13;
const SECTION_NDB: u32 = 0x17;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Vor,
    Ndb,
}

#[derive(Debug, Clone, Copy)]
pub struct Navaid {
    pub lat: f64,
    pub lon: f64,
    /// Megahertz for a VOR, kilohertz for an NDB.
    pub frequency: f64,
    /// Which sort of beacon it is, which is what decides the symbol drawn for it.
    pub kind: Kind,
}

/// Every beacon the simulator knows, by ident. Read once: it is a few megabytes of file
/// and a moment's work, and every approach after the first wants it.
///
/// An ident names more than one beacon in the world: there is a KTM in Nepal and another
/// on the far side of the Arabian Sea. So each ident keeps all of its beacons, and the one
/// a procedure means is the one nearest the airport it belongs to.
pub fn index() -> &'static HashMap<String, Vec<Navaid>> {
    static INDEX: OnceLock<HashMap<String, Vec<Navaid>>> = OnceLock::new();
    INDEX.get_or_init(load)
}

/// A beacon by ident alone: the VOR of that name where there is one. Only for where
/// nothing says where to look; a procedure's beacon is found with `find_near`.
pub fn find(ident: &str) -> Option<Navaid> {
    let all = index().get(&ident.to_uppercase())?;
    all.iter().find(|n| n.kind == Kind::Vor).or_else(|| all.first()).copied()
}

/// The beacon of an ident nearest a point, and only if it is near enough to be the one a
/// procedure there means: two beacons of one name are never within a few hundred miles.
pub fn find_near(ident: &str, near: (f64, f64)) -> Option<Navaid> {
    let cos = near.0.to_radians().cos().max(0.05);
    let nm = |n: &Navaid| ((n.lat - near.0) * 60.0).hypot((n.lon - near.1) * 60.0 * cos);
    // Nearest first; a VOR before an NDB of the same name at the same place.
    index()
        .get(&ident.to_uppercase())?
        .iter()
        .filter(|n| nm(n) < 300.0)
        .min_by(|a, b| (nm(a) + if a.kind == Kind::Vor { 0.0 } else { 0.5 }).total_cmp(&(nm(b) + if b.kind == Kind::Vor { 0.0 } else { 0.5 })))
        .copied()
}

fn load() -> HashMap<String, Vec<Navaid>> {
    let mut out = HashMap::new();
    for dir in super::nav_dirs() {
        // The files sit in numbered folders under the scenery directory, so this walks a
        // level down. The beacons are in their own files; the airport files are far
        // larger and hold none of them.
        let mut folders = vec![dir];
        while let Some(folder) = folders.pop() {
            let Ok(entries) = std::fs::read_dir(&folder) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                    continue;
                }
                if !path.file_name().map(|n| n.to_string_lossy().to_uppercase().starts_with("NVX")).unwrap_or(false) {
                    continue;
                }
                if let Ok(data) = std::fs::read(&path) {
                    read_file(&data, &mut out);
                }
            }
        }
    }
    log::debug!("navaids: {} beacons", out.len());
    out
}

fn read_file(d: &[u8], out: &mut HashMap<String, Vec<Navaid>>) {
    for (section, vor) in [(SECTION_VOR, true), (SECTION_NDB, false)] {
        for rec in bgl::section_records(d, section) {
            if rec.end - rec.start < 0x24 {
                continue;
            }
            let ident = bgl::ident(bgl::u32le(d, rec.start + 0x20));
            if ident.is_empty() {
                continue;
            }
            let (lon_at, lat_at, freq_at) = if vor { (0x08, 0x0C, 0x14) } else { (0x0C, 0x10, 0x08) };
            let lat = bgl::lat(bgl::u32le(d, rec.start + lat_at));
            let lon = bgl::lon(bgl::u32le(d, rec.start + lon_at));
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                continue;
            }
            let raw = bgl::u32le(d, rec.start + freq_at) as f64;
            let frequency = if vor { raw / 1.0e6 } else { raw / 1000.0 };
            // The same beacon can be listed more than once, in neighbouring files; a copy
            // within a mile of one already kept is the same beacon.
            let kind = if vor { Kind::Vor } else { Kind::Ndb };
            let all = out.entry(ident).or_default();
            if !all.iter().any(|n| n.kind == kind && (n.lat - lat).abs() < 0.02 && (n.lon - lon).abs() < 0.02) {
                all.push(Navaid { lat, lon, frequency, kind });
            }
        }
    }
}

/// Every beacon within a given distance of a point, nearest first.
///
/// A chart draws the beacons in the piece of country it covers whether or not the
/// procedure is written against them: they are how a reader knows where they are.
pub fn within(lat: f64, lon: f64, radius_nm: f64) -> Vec<(String, Navaid)> {
    let cos = lat.to_radians().cos().max(0.05);
    let mut out: Vec<(String, Navaid, f64)> = index()
        .iter()
        .flat_map(|(ident, all)| all.iter().map(move |n| (ident, n)))
        .filter_map(|(ident, n)| {
            let dn = (n.lat - lat) * 60.0;
            let de = (n.lon - lon) * 60.0 * cos;
            let nm = dn.hypot(de);
            (nm <= radius_nm).then(|| (ident.clone(), *n, nm))
        })
        .collect();
    out.sort_by(|a, b| a.2.total_cmp(&b.2));
    out.into_iter().map(|(ident, n, _)| (ident, n)).collect()
}

/// A position a given distance out on a radial from a beacon.
///
/// Radials are magnetic, so the variation has to be put back to get a bearing on the
/// ground; at a few miles' range, flat trigonometry is well inside the width of the line
/// this ends up being drawn with.
pub fn along_radial(from: Navaid, radial_deg: f64, distance_nm: f64, variation_deg: f64) -> (f64, f64) {
    let true_deg = (radial_deg + variation_deg).to_radians();
    let north = distance_nm * true_deg.cos();
    let east = distance_nm * true_deg.sin();
    (from.lat + north / 60.0, from.lon + east / 60.0 / from.lat.to_radians().cos().max(0.05))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_radial_runs_the_way_it_points() {
        let beacon = Navaid { lat: 50.0, lon: 0.0, frequency: 112.2, kind: Kind::Vor };
        let north = along_radial(beacon, 0.0, 6.0, 0.0);
        assert!((north.0 - 50.1).abs() < 1e-6, "{north:?}");
        assert!(north.1.abs() < 1e-6);
        let east = along_radial(beacon, 90.0, 6.0, 0.0);
        assert!((east.0 - 50.0).abs() < 1e-6);
        assert!(east.1 > 0.1);
    }
}
