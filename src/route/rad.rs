//! Europe's Route Availability Document: the rules the network manager checks every
//! flight plan against.

use crate::dispatch::{EdgeQuery, EdgeRule, FiledRoute, RouteRule, Verdict, Violation};
use chrono::{DateTime, Utc};

/// The RAD in force at a time.
pub struct Rad {
    _rules: (),
}

impl Rad {
    /// The edition in force at `when`, from Eurocontrol's publication, cached.
    pub fn load(when: DateTime<Utc>) -> anyhow::Result<Rad> {
        let _ = when;
        anyhow::bail!("the RAD is not built yet")
    }
}

impl EdgeRule for Rad {
    fn name(&self) -> &str {
        "RAD"
    }

    fn check(&self, _q: &EdgeQuery) -> Verdict {
        Verdict::Allow
    }
}

impl RouteRule for Rad {
    fn name(&self) -> &str {
        "RAD"
    }

    fn check_route(&self, _route: &FiledRoute) -> Vec<Violation> {
        Vec::new()
    }
}
