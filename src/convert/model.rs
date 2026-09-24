//! Data model shared by the AMDB converters. Kept deliberately small here: the fuller
//! model (aircraft-database record types for Fenix, DFD and the rest) lands alongside
//! this from another branch of the same work; this file adds only what grid MORA needs.

/// One quadrangle's minimum off-route altitude: the south-west corner of the one-degree
/// cell, and the altitude in feet.
pub struct MoraRec {
    pub lat: f64,
    pub lon: f64,
    pub altitude_ft: f64,
}
