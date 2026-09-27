//! The bridge as a service the desktop app starts and stops: the same work as
//! `amdb-bridge serve`, without the terminal prompts, and with a handle to stop it.

use super::cli::{make_store, DataArgs};
use super::server::{self, Listen, ServerHandle};
use super::settings::Settings;
use super::{desktop, hosts, tls};
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;

/// What to serve, from the app's settings.
#[derive(Debug, Clone)]
pub struct Options {
    /// Redirect the Navigraph AMDB host here (needs administrator rights).
    pub navigraph_redirect: bool,
    /// Redirect the Fenix A320 tablet's Navigraph hosts here (needs administrator rights).
    pub fenix_charts: bool,
    /// Install the X-Plane moving map when X-Plane 12 is found.
    pub xplane: bool,
}

impl Options {
    pub fn from_settings(s: &Settings) -> Options {
        Options { navigraph_redirect: s.navigraph_redirect, fenix_charts: s.fenix_charts, xplane: s.xplane }
    }
}

pub struct Running {
    handle: ServerHandle,
    /// Serving the A350 and A380X through the Navigraph address.
    pub redirected: bool,
    /// Serving the Fenix A320's tablet through Navigraph's sign-in and charts addresses.
    pub fenix_charts: bool,
    pub http_port: u16,
    pub started: Instant,
}

impl Running {
    pub fn airports_loaded(&self) -> usize {
        self.handle.store.loaded_count()
    }

    /// Stop serving. The A350/A380X setup stays in place for next time.
    pub fn stop(self) {
        self.handle.stop();
        crate::term::info("Stopped serving");
    }
}

/// True when this process may edit the hosts file.
pub fn elevated() -> bool {
    hosts::writable()
}

/// A redirect left in the hosts file (by `amdb-bridge serve` that did not stop cleanly, a
/// crash, a killed process) for an option that is off: the A350/A380X map host, or the
/// Fenix tablet's. While it is there, programs that use those Navigraph addresses reach
/// nothing, so it is removed when possible, keeping the other option's hosts.
pub fn clear_stale_redirect(settings: &Settings) -> Option<String> {
    let a350 = hosts::is_installed(super::NAVIGRAPH_AMDB_DOMAIN);
    let fenix = super::NAVIGRAPH_EFB_DOMAINS.iter().any(|d| hosts::is_installed(d));
    let stale_a350 = a350 && !settings.navigraph_redirect;
    let stale_fenix = fenix && !settings.fenix_charts;
    if !stale_a350 && !stale_fenix {
        return None;
    }
    if hosts::writable() {
        return Some(match desktop::setup_redirects(a350 && !stale_a350, fenix && !stale_fenix) {
            Ok(_) => "Removed a Navigraph redirect left behind by an earlier run".to_string(),
            Err(e) => format!("A Navigraph redirect was left behind by an earlier run and could not be removed: {e:#}"),
        });
    }
    let which = if stale_a350 { "A350/A380X" } else { "Fenix A320 tablet charts" };
    Some(format!(
        "A Navigraph redirect was left behind by an earlier run, so programs using those Navigraph addresses cannot reach them. Tick and untick the {which} option to remove it."
    ))
}

/// Fail early, in plain words, when another program is already listening on `port`.
fn port_free(port: u16) -> Result<()> {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            Err(anyhow::anyhow!("port {port} is already in use - is amdb-bridge already running in another window?"))
        }
        Err(e) if port == super::DEFAULT_HTTPS_PORT => Err(anyhow::anyhow!("port 443 is taken by another program (often Skype, VMware or a local web server): {e}")),
        Err(e) => Err(anyhow::anyhow!("cannot listen on port {port}: {e}")),
    }
}

/// Where airports are kept, without measuring the folder: in a large cache that takes
/// seconds, and the window shows the size anyway.
fn describe_storage(s: &Settings) -> String {
    if !s.cache {
        return "airports are rebuilt each session and not kept on disk".to_string();
    }
    match s.limit_mb {
        0 => format!("airports kept in {}", s.cache_dir.display()),
        mb => format!("airports kept in {} (up to {mb} MB)", s.cache_dir.display()),
    }
}

/// Start serving. The A350 and A380X are served too when that option is on and set up;
/// no administrator rights are needed for either.
pub fn start(settings: &Settings, opts: &Options) -> Result<Running> {
    let domain = super::NAVIGRAPH_AMDB_DOMAIN;
    crate::term::start(&format!("Starting AMDB Bridge {}", env!("CARGO_PKG_VERSION")));
    let http_port = super::DEFAULT_PORT;
    // Before any of the slow work, so a second copy fails in a moment, not ten seconds.
    port_free(http_port)?;
    let mut https = None;
    if opts.navigraph_redirect && !desktop::navigraph_ready() && hosts::writable() {
        desktop::setup_navigraph(true)?;
    }
    if opts.fenix_charts && !desktop::fenix_charts_ready() && hosts::writable() {
        desktop::setup_fenix_charts(true)?;
    }
    let redirected = opts.navigraph_redirect && desktop::navigraph_ready();
    let fenix_charts = opts.fenix_charts && desktop::fenix_charts_ready();
    if opts.navigraph_redirect && !redirected {
        crate::term::warn("A350/A380X support is not set up on this computer; untick and tick its option to set it up");
    }
    if opts.fenix_charts && !fenix_charts {
        crate::term::warn("The Fenix A320's tablet charts are not set up on this computer; untick and tick their option to set them up");
    }
    if redirected || fenix_charts {
        port_free(super::DEFAULT_HTTPS_PORT)?;
        // One certificate for every Navigraph name, whichever of them point here.
        let m = tls::ensure_for(domain, &super::navigraph_domains(true))?;
        https = Some((super::DEFAULT_HTTPS_PORT, m.cert_pem, m.key_pem));
    }
    if redirected {
        for note in desktop::patch_a350_everywhere() {
            crate::term::success(&note);
        }
    }
    crate::term::info(&format!("Storage: {}", describe_storage(settings)));
    let store = Arc::new(make_store(&DataArgs::default(), settings)?);
    let handle = server::start(store, Listen { http_port: Some(http_port), https })?;
    if redirected {
        crate::term::success("Serving the iniBuilds A350 and FlyByWire A380X too");
    }
    if fenix_charts {
        crate::term::success("Serving the Fenix A320's tablet charts too");
    }
    if opts.xplane {
        if let Some(root) = crate::sources::xplane::local::detect_install() {
            match crate::output::xplane::install_script(&root, &format!("http://127.0.0.1:{http_port}")) {
                Ok(_) => crate::term::success("X-Plane 12 moving map ready: Plugins > FlyWithLua > Macros > AMDB OANS"),
                Err(e) => crate::term::warn(&format!("X-Plane 12: {e:#}")),
            }
        }
    }
    // Only where the A320 OANS is installed: a Fenix update drops the lines that load it.
    for sim in desktop::detect_sims() {
        match desktop::repatch_a320_oans(&sim.community) {
            Ok(files) if !files.is_empty() => crate::term::success(&format!("{}: A320 OANS added to the Fenix again after a Fenix update; restart the simulator to load it", sim.name)),
            Ok(_) => {}
            Err(e) => crate::term::warn(&format!("{}: could not add the A320 OANS to the Fenix: {e:#}", sim.name)),
        }
    }
    crate::term::success("Ready: load your aircraft");
    Ok(Running { handle, redirected, fenix_charts, http_port, started: Instant::now() })
}
