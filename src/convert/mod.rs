//! The intermediate model a navigation database is read into before it is written out in
//! any particular format (PMDG's, Fenix's, MORA-only tooling). Keeping the read side and
//! the write side apart from each other through this means a new source or a new writer
//! is one more implementation against `model::NavSet`, not a change to the others.
//!
//! [`mora`] is the exception that proves the rule: grid minimum off-route altitudes are
//! the one part of a navigation database that is *derived* rather than surveyed, so they
//! are computed here from free terrain and obstacle data rather than read from anywhere.

pub mod model;
pub mod mora;
