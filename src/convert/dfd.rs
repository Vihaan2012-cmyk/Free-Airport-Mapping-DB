//! Writes a `NavSet` in the Navigraph-DFD `tbl_*` layout that iniBuilds' A350, Synaptic's
//! A220 and PMDG's 737/777 all bundle under three different paths (see `sources::navdata`
//! for where each one lives and the two spellings — `tbl_pi_localizers_glideslopes` and
//! `tbl_localizers_glideslopes` — one writer would need to produce or at least tolerate).
//!
//! This pass does not carry that writer past this stub. The DFD layout is the same 27
//! tables in three places, which sounds like less work than Fenix's 24, but every one of
//! those tables is ARINC 424 fixed-format text carried straight into columns — thirty-odd
//! columns each for `tbl_sids`/`tbl_stars`/`tbl_iaps` alone, `seqno` rather than
//! `sequence_number` for the procedure tables (a fact `sources::navdata.rs` already
//! documents the hard way), and three header shapes to match depending on which aircraft's
//! copy is being replaced. Getting that right needed the whole of this pass's budget spent
//! on Fenix instead, on the principle that a converter which writes one target completely is
//! worth more than one that writes two badly. `plan` and `write` both say so rather than
//! producing a file that looks complete and silently is not.

use crate::convert::model::NavSet;
use crate::convert::TableReport;
use anyhow::{bail, Result};
use std::path::Path;

const NOT_YET_WRITTEN: &str = "the DFD writer was not carried past a stub in this pass; every table it would fill is reported empty rather than half-written. Use --to fenix.";

/// What a DFD write would report: nothing filled, everything explained, matching the shape
/// `fenix::plan` uses so a caller can treat either target identically until this lands.
pub fn plan(_nav: &NavSet) -> Vec<TableReport> {
    ["tbl_airports", "tbl_runways", "tbl_vhfnavaids", "tbl_enroute_waypoints", "tbl_terminal_waypoints", "tbl_localizers_glideslopes", "tbl_sids", "tbl_stars", "tbl_iaps", "tbl_enroute_airways"]
        .iter()
        .map(|t| TableReport::empty(t, NOT_YET_WRITTEN))
        .collect()
}

/// Refuses to write: see the module doc comment for why this pass stops here.
pub fn write(_nav: &NavSet, _path: &Path) -> Result<Vec<TableReport>> {
    bail!("{NOT_YET_WRITTEN}")
}
