//! Reading Microsoft Flight Simulator's own navigation data from the user's install.
//!
//! The simulator ships the current AIRAC cycle as BGL files, and those files carry the
//! departures, arrivals and approaches for the whole world. Everything here reads that
//! copy on the machine it runs on; none of it may be redistributed, so the data is used
//! to draw for the user and is never written into anything we publish.

pub mod bgl;
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
