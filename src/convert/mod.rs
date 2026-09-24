//! The intermediate model a navigation database is read into before it is written out in
//! any particular format (PMDG's, Fenix's, MORA-only tooling). Keeping the read side and
//! the write side apart from each other through this means a new source or a new writer
//! is one more implementation against `model::NavSet`, not a change to the others.

pub mod model;
