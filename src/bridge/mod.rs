//! Aircraft bridge: a local server that speaks the Navigraph AMDB API surface the
//! aircraft already use (FlyByWire A380X/A32NX OANC, and any add-on built on the
//! same API), fed by amdbgen data; plus a patcher that points installed aircraft
//! packages at it.

pub mod charts;
pub mod cli;
pub mod compat;
pub mod desktop;
pub mod diagnostics;
pub mod hosts;
pub mod patcher;
pub mod platform;
pub mod progress;
pub mod planner;
pub mod server;
pub mod service;
pub mod settings;
pub mod simbrief;
pub mod store;
pub mod taxi;
pub mod tls;

pub const DEFAULT_PORT: u16 = 8770;

/// A console tool run without flashing a console window, which it otherwise does when
/// started from the desktop app. Windows now calls the equivalent APIs directly, so this
/// only backs the non-Windows fallbacks (reg, tasklist) that never run there anyway.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn quiet_command(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}
pub const NAVIGRAPH_AMDB_HOST: &str = "https://amdb.api.navigraph.com";
pub const NAVIGRAPH_AMDB_DOMAIN: &str = "amdb.api.navigraph.com";
pub const DEFAULT_HTTPS_PORT: u16 = 443;

/// The hosts the Fenix A320's flight bag calls, on top of the map's.
///
/// It is the one flight bag that cannot be patched: its web app is served out of an
/// encrypted bundle by its own local gateway and exists nowhere on disk, so the addresses
/// it calls cannot be rewritten the way every other bag's are. They are redirected
/// instead. Read out of the app itself rather than guessed: the sign-in is the OAuth
/// device flow, the charts are the same v2 API this bridge already answers, and the cycle
/// is a single call of its own.
pub const NAVIGRAPH_EFB_DOMAINS: [&str; 3] = ["identity.api.navigraph.com", "api.navigraph.com", "charts.api.navigraph.com"];

/// Every host the redirect covers: the map's alone, or the flight bag's as well.
pub fn navigraph_domains(with_efb: bool) -> Vec<&'static str> {
    let mut v = vec![NAVIGRAPH_AMDB_DOMAIN];
    if with_efb {
        v.extend(NAVIGRAPH_EFB_DOMAINS);
    }
    v
}
