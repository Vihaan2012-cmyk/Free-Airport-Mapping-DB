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
    /// Install the X-Plane moving map when X-Plane 12 is found.
    pub xplane: bool,
}

impl Options {
    pub fn from_settings(s: &Settings) -> Options {
        Options { navigraph_redirect: s.navigraph_redirect, xplane: s.xplane }
    }
}

pub struct Running {
    handle: ServerHandle,
    /// Serving the A350 and A380X through the Navigraph address.
    pub redirected: bool,
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

/// A redirect left in the hosts file by `amdb-bridge serve` that did not stop cleanly (a
/// crash, a killed process) while the app's A350/A380X option is off. While it is there,
/// aircraft that use the Navigraph API reach nothing, so it is removed when possible.
pub fn clear_stale_redirect(settings: &Settings) -> Option<String> {
    let domain = super::NAVIGRAPH_AMDB_DOMAIN;
    if settings.navigraph_redirect || !hosts::is_installed(domain) {
        return None;
    }
    if hosts::writable() {
        return Some(match hosts::remove() {
            Ok(_) => "Removed a Navigraph redirect left behind by an earlier run".to_string(),
            Err(e) => format!("A Navigraph redirect was left behind by an earlier run and could not be removed: {e:#}"),
        });
    }
    Some("A Navigraph redirect was left behind by an earlier run, so aircraft using Navigraph's own maps cannot reach them. Tick and untick the A350/A380X option to remove it.".to_string())
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
    let mut redirected = false;
    if opts.navigraph_redirect {
        if !desktop::navigraph_ready() && hosts::writable() {
            desktop::setup_navigraph(true)?;
        }
        if desktop::navigraph_ready() {
            port_free(super::DEFAULT_HTTPS_PORT)?;
            let m = tls::ensure(domain)?;
            https = Some((super::DEFAULT_HTTPS_PORT, m.cert_pem, m.key_pem));
            redirected = true;
            for note in desktop::patch_a350_everywhere() {
                crate::term::success(&note);
            }
        } else {
            crate::term::warn("A350/A380X support is not set up on this computer; untick and tick its option to set it up");
        }
    }
    crate::term::info(&format!("Storage: {}", describe_storage(settings)));
    let store = Arc::new(make_store(&DataArgs::default(), settings)?);
    let handle = server::start(store, Listen { http_port: Some(http_port), https })?;
    if redirected {
        crate::term::success("Serving the iniBuilds A350 and FlyByWire A380X too");
    }
    if opts.xplane {
        if let Some(root) = crate::sources::xplane::local::detect_install() {
            match crate::output::xplane::install_script(&root, &format!("http://127.0.0.1:{http_port}")) {
                Ok(_) => crate::term::success("X-Plane 12 moving map ready: Plugins > FlyWithLua > Macros > AMDB OANS"),
                Err(e) => crate::term::warn(&format!("X-Plane 12: {e:#}")),
            }
        }
    }
    crate::term::success("Ready: load your aircraft");
    Ok(Running { handle, redirected, http_port, started: Instant::now() })
}
