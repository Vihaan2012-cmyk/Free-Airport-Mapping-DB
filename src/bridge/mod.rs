//! Aircraft bridge: a local server that speaks the Navigraph AMDB API surface the
//! aircraft already use (FlyByWire A380X/A32NX OANC, and any add-on built on the
//! same API), fed by amdbgen data; plus a patcher that points installed aircraft
//! packages at it.

pub mod cli;
pub mod compat;
pub mod desktop;
pub mod diagnostics;
pub mod hosts;
pub mod patcher;
pub mod platform;
pub mod progress;
pub mod server;
pub mod service;
pub mod settings;
pub mod store;
pub mod tls;

pub const DEFAULT_PORT: u16 = 8770;

/// A console tool (certutil, reg, ipconfig) run without flashing a console window, which
/// it otherwise does when started from the desktop app.
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
