//! Writing an aircraft's own navigation database from this crate's data, so an airliner
//! flies on current navigation data instead of whatever cycle it happened to ship with —
//! iniBuilds' A350 on three-and-a-half-year-old AIRAC 2303, PMDG's 737 and 777 on 2404,
//! Fenix's A320 and the Synaptic A220 on 2503, all measured on the machine this was written
//! on. The FlyByWire A380X is the one aircraft that never goes stale, because it reads the
//! simulator's own navigation data directly rather than shipping a database of its own; this
//! module is the same trick applied to the aircraft that cannot be changed to read live data,
//! by regenerating the file they do read.
//!
//! `model` defines the shape every target writer reads from (`NavSet`); `fenix` writes
//! Fenix's `imported.db3`. `dfd` is meant to write the Navigraph-DFD-shaped `tbl_*` layout
//! that iniBuilds, Synaptic and PMDG all bundle, but is not carried past a stub in this
//! pass — see its own doc comment for why landing Fenix completely took the whole budget.

pub mod dfd;
pub mod fenix;
pub mod model;

pub use model::NavSet;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Which of the two shapes a `convert` run writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Fenix,
    Dfd,
}

impl std::str::FromStr for Target {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "fenix" => Ok(Target::Fenix),
            "dfd" | "navigraph" => Ok(Target::Dfd),
            other => Err(anyhow::anyhow!("--to takes fenix or dfd, not {other}")),
        }
    }
}

/// One line of what a `convert` run reports: a table, how many rows it holds, and — for a
/// table this crate deliberately never fills — why, so a pilot reading the command's output
/// learns a hold or an MSA ring is missing rather than trusting one that was never there.
#[derive(Debug, Clone)]
pub struct TableReport {
    pub table: String,
    pub rows: usize,
    pub omitted_because: Option<String>,
}

impl TableReport {
    pub fn filled(table: &str, rows: usize) -> Self {
        TableReport { table: table.to_string(), rows, omitted_because: None }
    }

    pub fn empty(table: &str, why: &str) -> Self {
        TableReport { table: table.to_string(), rows: 0, omitted_because: Some(why.to_string()) }
    }
}

/// Copy a database aside before writing over it — the same house rule `bridge::patcher`
/// follows before it patches a script (see its own doc comment): a user's working file is
/// never the only copy of itself while this crate is still finding out whether the write it
/// is about to make succeeds. A backup already sitting next to the file from an earlier run
/// is left alone, the same way the patcher leaves its own `.bak` files alone, so repeated
/// runs never overwrite the one copy of the database the aircraft shipped with.
pub fn backup(path: &Path) -> Result<PathBuf> {
    let backup = PathBuf::from(format!("{}.amdbgen.bak", path.display()));
    if !backup.exists() {
        std::fs::copy(path, &backup).with_context(|| format!("backup {}", path.display()))?;
    }
    Ok(backup)
}

/// Where Fenix keeps the database it reads on startup, on this machine's default install —
/// used by `--in-place`, which needs somewhere to write without being told.
pub fn fenix_default_path() -> Option<PathBuf> {
    let base = std::env::var_os("PROGRAMDATA")?;
    let p = PathBuf::from(base).join("Fenix").join("Navdata").join("imported.db3");
    p.exists().then_some(p)
}
