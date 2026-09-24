//! Reading Microsoft Flight Simulator's own navigation data from the user's install.
//!
//! The simulator ships the current AIRAC cycle as BGL files, and those files carry the
//! departures, arrivals and approaches for the whole world. Everything here reads that
//! copy on the machine it runs on; none of it may be redistributed, so the data is used
//! to draw for the user and is never written into anything we publish.

pub mod bgl;
pub mod fsarchive;
pub mod navaids;

/// The dates the navigation data is in force between, as the simulator records them.
///
/// The cycle file carries two dates and almost nothing else readable: the day the data
/// came into force and the day it goes out. A chart is only as current as its data, so
/// it is worth saying which.
pub fn airac_dates() -> Option<(String, String)> {
    for dir in nav_dirs() {
        let mut folders = vec![dir];
        while let Some(folder) = folders.pop() {
            let Ok(entries) = std::fs::read_dir(&folder) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                    continue;
                }
                if !path.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case("AIRACCycle.bgl")).unwrap_or(false) {
                    continue;
                }
                let Ok(data) = std::fs::read(&path) else { continue };
                let dates = day_month_year(&data);
                if dates.len() >= 2 {
                    return Some((dates[0].clone(), dates[1].clone()));
                }
            }
        }
    }
    None
}

/// Every "dd-mm-yy" in a file, in the order they appear.
fn day_month_year(d: &[u8]) -> Vec<String> {
    const MONTHS: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    let mut out = Vec::new();
    for w in d.windows(8) {
        let digits = |i: usize| w[i].is_ascii_digit();
        if !(digits(0) && digits(1) && w[2] == b'-' && digits(3) && digits(4) && w[5] == b'-' && digits(6) && digits(7)) {
            continue;
        }
        let text = String::from_utf8_lossy(w);
        let (day, month, year) = (&text[0..2], &text[3..5], &text[6..8]);
        let Ok(month_number) = month.parse::<usize>() else { continue };
        if !(1..=12).contains(&month_number) {
            continue;
        }
        let pretty = format!("{day} {} 20{year}", MONTHS[month_number - 1]);
        if !out.contains(&pretty) {
            out.push(pretty);
        }
    }
    out
}
pub mod procedures;

use std::path::PathBuf;

/// `fs-base-nav/scenery` folders of every Microsoft Flight Simulator on this machine.
/// The airports, with their procedures, live in the `NAX*.bgl` files there.
pub fn nav_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for sim in crate::bridge::desktop::detect_sims() {
        let Some(library) = sim.community.parent() else { continue };
        for official in ["Official", "Official2020", "Official2024"] {
            for store in ["OneStore", "Steam"] {
                let d = library.join(official).join(store).join("fs-base-nav").join("scenery");
                if d.is_dir() && !out.contains(&d) {
                    out.push(d);
                }
            }
        }
    }
    out
}

/// `.fsarchive` files holding a whole simulator's navigation data in one blob - how
/// FS2024 ships it, in place of the loose `scenery/*.bgl` tree `nav_dirs` finds for older
/// installs. Found by name rather than a fixed path, since the one seen so far is
/// `fs24-fs-base-nav`, and a future cycle could ship under a different numbered name the
/// same way `fs-base-nav` itself is versioned by simulator.
pub fn nav_archives() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for sim in crate::bridge::desktop::detect_sims() {
        let Some(library) = sim.community.parent() else { continue };
        let Ok(packages) = std::fs::read_dir(library.join("StreamedPackages")) else { continue };
        for pkg in packages.flatten() {
            let path = pkg.path();
            let is_nav_package = path.is_dir() && path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase().contains("fs-base-nav")).unwrap_or(false);
            if !is_nav_package {
                continue;
            }
            let Ok(content) = std::fs::read_dir(path.join("content")) else { continue };
            for c in content.flatten() {
                let cp = c.path();
                let is_archive = cp.extension().map(|e| e.eq_ignore_ascii_case("fsarchive")).unwrap_or(false);
                if is_archive && !out.contains(&cp) {
                    out.push(cp);
                }
            }
        }
    }
    out
}

/// Every BGL whose name starts with one of `prefixes` (`"nax"`, `"nvx"`, `"atx"`), from
/// every simulator install found - loose files under `nav_dirs` and packed ones inside
/// `nav_archives` alike. A caller that wants, say, every `nvx` file in the world calls
/// this once instead of knowing which of the two layouts a given install uses.
///
/// This is what makes FS2024's navigation data reachable at all: before this, amdbgen
/// could only walk loose `scenery/*.bgl` files, and FS2024 has none - its whole dataset
/// is the one packed `minimal.fsarchive`.
pub fn for_each_bgl(prefixes: &[&str], mut visit: impl FnMut(String, Vec<u8>)) {
    let lower: Vec<String> = prefixes.iter().map(|p| p.to_ascii_lowercase()).collect();
    for dir in nav_dirs() {
        let mut folders = vec![dir];
        while let Some(folder) = folders.pop() {
            let Ok(entries) = std::fs::read_dir(&folder) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                    continue;
                }
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()) else { continue };
                if !lower.iter().any(|p| name.starts_with(p.as_str())) {
                    continue;
                }
                if let Ok(data) = std::fs::read(&path) {
                    visit(path.display().to_string(), data);
                }
            }
        }
    }
    for archive_path in nav_archives() {
        match fsarchive::FsArchive::open(&archive_path) {
            Ok(archive) => {
                let prefix_refs: Vec<&str> = prefixes.to_vec();
                if let Err(e) = archive.for_each_with_prefix(&prefix_refs, |p, data| visit(format!("{}::{p}", archive_path.display()), data)) {
                    log::warn!("reading {}: {e}", archive_path.display());
                }
            }
            Err(e) => log::warn!("opening {}: {e}", archive_path.display()),
        }
    }
}
