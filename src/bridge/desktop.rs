//! What the desktop app shows and changes on this machine: which simulators are
//! installed, the state of the aircraft the bridge serves, and the Windows start-up
//! entry. Kept free of any window code, so the
//! installer's command-line steps and the app share one implementation.

use super::patcher;
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;


/// One simulator's Community folder.
#[derive(Debug, Clone)]
pub struct Sim {
    /// "MSFS 2020", "MSFS 2024 (Steam)" and so on.
    pub name: String,
    pub community: PathBuf,
}

/// Every Microsoft Flight Simulator on this machine, from each one's `UserCfg.opt`.
/// A default Community folder is only used when that simulator's config names none, so
/// a moved package library does not also list the stale folder it left behind.
pub fn detect_sims() -> Vec<Sim> {
    if !cfg!(windows) {
        return super::platform::proton_sims().into_iter().map(|(name, community)| Sim { name, community }).collect();
    }
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let roaming = std::env::var("APPDATA").unwrap_or_default();
    let installs = [
        ("MSFS 2020", format!("{local}/Packages/Microsoft.FlightSimulator_8wekyb3d8bbwe/LocalCache")),
        ("MSFS 2024", format!("{local}/Packages/Microsoft.Limitless_8wekyb3d8bbwe/LocalCache")),
        ("MSFS 2020 (Steam)", format!("{roaming}/Microsoft Flight Simulator")),
        ("MSFS 2024 (Steam)", format!("{roaming}/Microsoft Flight Simulator 2024")),
    ];
    let mut out: Vec<Sim> = Vec::new();
    for (name, base) in installs {
        let base = PathBuf::from(base);
        let mut community = fs::read_to_string(base.join("UserCfg.opt")).ok().and_then(|text| {
            text.lines().find_map(|l| l.trim().strip_prefix("InstalledPackagesPath").map(|p| PathBuf::from(p.trim().trim_matches('"')).join("Community")))
        });
        if community.as_ref().map_or(true, |c| !c.is_dir()) {
            community = Some(base.join("Packages").join("Community")).filter(|c| c.is_dir());
        }
        if let Some(c) = community {
            if !out.iter().any(|s| s.community == c) {
                out.push(Sim { name: name.to_string(), community: c });
            }
        }
    }
    out
}






#[derive(Debug, Clone, PartialEq)]
pub enum A350State {
    /// The EFB is patched to hand its OANS a token, so the redirect is all it needs.
    Patched,
    NotPatched,
}

/// The iniBuilds A350 EFBs found in a Community folder.
pub fn a350_state(community: &Path) -> Option<A350State> {
    let found = patcher::scan_a350(community);
    if found.is_empty() {
        return None;
    }
    Some(if found.iter().all(|(_, _, patched)| *patched) { A350State::Patched } else { A350State::NotPatched })
}

#[derive(Debug, Clone, PartialEq)]
pub enum XPlaneState {
    NoFlyWithLua,
    ScriptMissing,
    Installed,
}

/// X-Plane 12's install folder and the moving map's state in it.
pub fn xplane_state() -> Option<(PathBuf, XPlaneState)> {
    let root = crate::sources::xplane::local::detect_install()?;
    let scripts = root.join("Resources").join("plugins").join("FlyWithLua").join("Scripts");
    let state = if !scripts.is_dir() {
        XPlaneState::NoFlyWithLua
    } else if scripts.join("amdb_oans.lua").is_file() {
        XPlaneState::Installed
    } else {
        XPlaneState::ScriptMissing
    };
    Some((root, state))
}

fn quiet(program: &str) -> Command {
    super::quiet_command(program)
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "AMDB Bridge";

/// AMDB Bridge opens when Windows starts.
pub fn run_at_login() -> bool {
    quiet("reg").args(["query", RUN_KEY, "/v", RUN_VALUE]).output().map_or(false, |o| o.status.success())
}

/// Open (or stop opening) `exe --tray` when Windows starts, for this user only.
pub fn set_run_at_login(on: bool, exe: &Path) -> Result<()> {
    let out = if on {
        let command = format!("\"{}\" --tray", exe.display());
        quiet("reg").args(["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &command, "/f"]).output()
    } else {
        if !run_at_login() {
            return Ok(());
        }
        quiet("reg").args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]).output()
    }
    .context("run reg.exe")?;
    if !out.status.success() {
        return Err(anyhow!("could not change the Windows start-up entry: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// AMDB Bridge opens when a simulator starts.
pub fn start_with_sim() -> bool {
    patcher::autostart_installed()
}

/// Add (or remove) the app to every simulator's exe.xml. Returns the files changed.
pub fn set_start_with_sim(on: bool, exe: &Path) -> Result<Vec<PathBuf>> {
    // Remove first either way, so an entry left by the command-line tool is replaced by
    // one for this program rather than blocking it.
    let mut changed = patcher::remove_autostart()?;
    if on {
        if patcher::exe_xml_files().is_empty() {
            return Err(anyhow!("no simulator has an exe.xml yet - start Microsoft Flight Simulator once, then try again"));
        }
        for p in patcher::install_autostart(exe, "--tray")? {
            if !changed.contains(&p) {
                changed.push(p);
            }
        }
    }
    Ok(changed)
}

/// A350/A380X support is set up on this computer: the Navigraph map server's address
/// points here and the certificate that answers for it is trusted.
pub fn navigraph_ready() -> bool {
    let domain = super::NAVIGRAPH_AMDB_DOMAIN;
    super::hosts::is_installed(domain) && super::tls::data_dir().join("ca.pem").is_file() && super::tls::is_trusted()
}

/// Set up (or take down) A350/A380X support. Needs administrator rights, and is done
/// once rather than every time serving starts: the address stays pointed here while
/// the option is on, and the app then serves those aircraft as a normal user.
pub fn setup_navigraph(on: bool) -> Result<()> {
    let domain = super::NAVIGRAPH_AMDB_DOMAIN;
    if on {
        let m = super::tls::ensure(domain)?;
        super::tls::trust(&m)?;
        super::hosts::install(domain)?;
        allow_port_443();
    } else {
        super::hosts::remove()?;
        super::tls::untrust()?;
    }
    // Run with sudo, the certificate files were created as root in the user's folder.
    super::platform::return_to_user(&super::platform::data_dir());
    Ok(())
}

/// Linux only lets root listen on ports below 1024. Give this program that one right,
/// so it can answer the aircraft on 443 without running as root.
fn allow_port_443() {
    #[cfg(unix)]
    if let Ok(exe) = std::env::current_exe() {
        match Command::new("setcap").arg("cap_net_bind_service=+ep").arg(&exe).output() {
            Ok(o) if o.status.success() => log::info!("{} may now listen on port 443", exe.display()),
            _ => log::warn!("could not let {} use port 443 (install setcap, or run `sudo setcap cap_net_bind_service=+ep {}`)", exe.display(), exe.display()),
        }
    }
}

/// Patch every iniBuilds A350 EFB found, so its airport map does not wait for a
/// Navigraph sign-in. Returns one line per aircraft changed.
pub fn patch_a350_everywhere() -> Vec<String> {
    let mut notes = Vec::new();
    for sim in detect_sims() {
        match patcher::patch_a350(&sim.community, false) {
            Ok(files) if !files.is_empty() => notes.push(format!("{}: iniBuilds A350 EFB patched so its airport map works without a Navigraph subscription", sim.name)),
            Ok(_) => {}
            Err(e) => notes.push(format!("{}: could not patch the iniBuilds A350 EFB: {e:#}", sim.name)),
        }
    }
    notes
}

/// The FlyByWire A380X is in this Community folder.
pub fn a380x_in_community(community: &Path) -> bool {
    community.join("flybywire-aircraft-a380-842").is_dir()
}

/// A Microsoft Flight Simulator is running. It reads Community packages only at start,
/// so a map installed now appears after the next restart.
pub fn sim_running() -> bool {
    quiet("tasklist").args(["/NH", "/FO", "CSV"]).output().map_or(false, |o| {
        let list = String::from_utf8_lossy(&o.stdout).to_ascii_lowercase();
        list.contains("\"flightsimulator.exe\"") || list.contains("\"flightsimulator2024.exe\"")
    })
}

/// The user's Downloads folder, wherever they have moved it; `%USERPROFILE%\Downloads`
/// if Windows cannot say.
pub fn downloads_dir() -> PathBuf {
    #[cfg(windows)]
    unsafe {
        use winapi::um::combaseapi::CoTaskMemFree;
        use winapi::um::knownfolders::FOLDERID_Downloads;
        use winapi::um::shlobj::SHGetKnownFolderPath;
        let mut raw = std::ptr::null_mut();
        if SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut raw) == 0 && !raw.is_null() {
            let len = (0..).take_while(|&i| *raw.offset(i) != 0).count();
            let path = String::from_utf16_lossy(std::slice::from_raw_parts(raw, len));
            CoTaskMemFree(raw as _);
            return PathBuf::from(path);
        }
    }
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into())).join("Downloads")
}

/// Show a folder or file in Explorer, with a file selected.
pub fn reveal_file(path: &Path) {
    let _ = Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
}

/// Show a folder or file in Explorer.
pub fn reveal(path: &Path) {
    let _ = Command::new("explorer").arg(path).spawn();
}

/// Undo everything the app may have changed outside its own folder, for the uninstaller:
/// the start-up entries, the A350 patch, the hosts-file redirect and the certificate.
/// Each step runs
/// even if an earlier one fails; the failures come back as text.
pub fn uninstall_cleanup() -> Vec<String> {
    let mut problems = Vec::new();
    let mut note = |r: Result<()>, what: &str| {
        if let Err(e) = r {
            problems.push(format!("{what}: {e:#}"));
        }
    };
    for sim in detect_sims() {
        note(patcher::unpatch(&sim.community).map(|_| ()), &format!("restore patched aircraft in {}", sim.name));
    }
    note(patcher::remove_autostart().map(|_| ()), "remove the simulator start-up entry");
    if let Ok(exe) = std::env::current_exe() {
        note(set_run_at_login(false, &exe), "remove the Windows start-up entry");
    }
    let domain = super::NAVIGRAPH_AMDB_DOMAIN;
    if super::hosts::is_installed(domain) || super::tls::is_trusted() {
        if super::hosts::writable() {
            note(super::hosts::remove().map(|_| ()), "remove the hosts-file redirect");
            note(super::tls::untrust().map(|_| ()), "remove the local certificate");
        } else {
            problems.push("the hosts-file redirect and certificate need administrator rights to remove".to_string());
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;


}
