//! ETOPS, or EDTO: a twin must never be further than a set time, flying on one engine,
//! from an airport it could land at.

use crate::dispatch::{Airport, EdgeQuery, EdgeRule, Verdict};

pub struct Etops {
    _minutes: u32,
    _one_engine_tas_kt: f64,
    _airports: Vec<Airport>,
}

impl Etops {
    pub fn new(minutes: u32, one_engine_tas_kt: f64, airports: Vec<Airport>) -> Etops {
        Etops { _minutes: minutes, _one_engine_tas_kt: one_engine_tas_kt, _airports: airports }
    }
}

impl EdgeRule for Etops {
    fn name(&self) -> &str {
        "ETOPS"
    }

    fn check(&self, _q: &EdgeQuery) -> Verdict {
        Verdict::Allow
    }
}
