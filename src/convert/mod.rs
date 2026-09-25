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
pub mod from_sim;
pub mod model;
pub mod mora;

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

/// Fill the tables this converter leaves empty from the database being replaced.
///
/// The simulator's navigation data is broader than an aircraft's own -- nearly twice the
/// aerodromes, three times the procedures -- but it does not carry everything: no radio
/// frequencies, no published holds, no grid MORA. Replacing the aircraft's database
/// outright therefore trades one set of gaps for another, and a Fenix A320 would lose
/// fifty-three thousand frequencies to gain sixteen thousand aerodromes.
///
/// It need not be a trade. The tables this converter fills are replaced; the ones it
/// reports as left empty are taken from the old database, which already has them. Only
/// empty tables are touched, and only where the two schemas agree column for column, so
/// nothing generated is ever overwritten and a table that has changed shape is skipped
/// rather than mangled.
///
/// Returns what was carried over, table by table.
pub fn carry_over(into: &Path, from: &Path, report: &[TableReport]) -> Result<Vec<(String, usize)>> {
    use rusqlite::Connection;
    let conn = Connection::open(into).with_context(|| format!("open {}", into.display()))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    conn.execute("ATTACH DATABASE ?1 AS old", rusqlite::params![from.to_string_lossy()])
        .with_context(|| format!("read {}", from.display()))?;

    let columns = |db: &str, table: &str| -> Result<Vec<String>> {
        let mut st = conn.prepare(&format!("PRAGMA {db}.table_info(\"{table}\")"))?;
        let rows = st.query_map([], |r| r.get::<_, String>(1))?.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    };

    let mut done = Vec::new();
    // Anything that came out empty, however it was reported: a table the writer says it
    // leaves empty, and one it wrote nothing into.
    let empty: Vec<&str> = report.iter().filter(|t| t.omitted_because.is_some() || t.rows == 0).map(|t| t.table.as_str()).collect();
    for table in empty {
        let (new_cols, old_cols) = (columns("main", table)?, columns("old", table)?);
        if new_cols.is_empty() || old_cols.is_empty() || new_cols != old_cols {
            log::warn!("{table}: not carried over, the two databases spell it differently");
            continue;
        }
        // A table that points at another by its surrogate number cannot come across.
        //
        // This converter numbers the aerodromes, runways and waypoints afresh, so row 1433
        // is not the aerodrome it was: carrying `Markers` over by its `AirportID` moved
        // every marker beacon in the file to a different aerodrome -- four hundred of four
        // hundred sampled, a Canadian airport's markers landing in Mongolia. It looks
        // right, because the numbers still resolve to something.
        //
        // The ones that can come across are keyed by what a thing is actually called --
        // `airport_identifier`, `waypoint_identifier`, a latitude and longitude -- and
        // those mean the same in any database. A column named `<something>ID` is a
        // reference to a renumbered table; the table's own `ID` is not.
        if let Some(col) = new_cols.iter().find(|c| c.len() > 2 && c.to_ascii_lowercase().ends_with("id") && !c.eq_ignore_ascii_case("id")) {
            log::warn!("{table}: not carried over, its {col} is a row number this conversion gives out afresh");
            continue;
        }
        let already: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM main.\"{table}\""), [], |r| r.get(0))?;
        if already > 0 {
            continue; // written after all; what is generated wins
        }
        let n = conn.execute(&format!("INSERT INTO main.\"{table}\" SELECT * FROM old.\"{table}\""), [])?;
        if n > 0 {
            done.push((table.to_string(), n));
        }
    }
    conn.execute_batch("DETACH DATABASE old;")?;
    Ok(done)
}

/// Where Fenix keeps the database it reads on startup, on this machine's default install —
/// used by `--in-place`, which needs somewhere to write without being told.
pub fn fenix_default_path() -> Option<PathBuf> {
    let base = std::env::var_os("PROGRAMDATA")?;
    let p = PathBuf::from(base).join("Fenix").join("Navdata").join("imported.db3");
    p.exists().then_some(p)
}
