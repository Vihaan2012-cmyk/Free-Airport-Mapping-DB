//! What the desktop app shows and changes on this machine: which simulators are
//! installed, whether the A220 moving map is in each one, the state of the aircraft the
//! bridge serves, and the Windows start-up entry. Kept free of any window code, so the
//! installer's command-line steps and the app share one implementation.

use super::patcher;
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Folder name of the A220 map in a Community folder. The `zzz-` prefix makes MSFS 2020
/// load it after the aircraft it overrides; MSFS 2024 goes by the manifest instead.
pub const A220_MAP_FOLDER: &str = "zzz-amdb-a220-amm";
/// Other packages that replace the same display-unit file. Only one of them can load,
/// so they are set aside while ours is installed and put back when it is removed.
const A220_MAP_CONFLICTS: [&str; 2] = ["zzz-gm5-a220-amm", "gm5-a220-amm"];
/// Where set-aside packages wait, next to the Community folder.
const PARKED: &str = "_disabled";

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

/// The A220 map shipped with this program: next to the executable once installed, or
/// the package in the source tree when run from a build folder.
pub fn bundled_a220_map() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let installed = dir.join("msfs").join(A220_MAP_FOLDER);
    if installed.join("manifest.json").is_file() {
        return Some(installed);
    }
    dir.ancestors().map(|a| a.join("packages").join("msfs-a220-amm")).find(|p| p.join("manifest.json").is_file())
}

/// `package_version` from an MSFS package manifest.
pub fn package_version(package: &Path) -> Option<String> {
    let text = fs::read_to_string(package.join("manifest.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    v.get("package_version")?.as_str().map(str::to_string)
}

#[derive(Debug, Clone, PartialEq)]
pub enum MapState {
    NotInstalled,
    Installed(String),
    /// Installed, but an older version than the one this program carries.
    Outdated { installed: String, available: String },
}

pub fn a220_map_state(community: &Path) -> MapState {
    let dest = community.join(A220_MAP_FOLDER);
    let Some(installed) = package_version(&dest) else { return MapState::NotInstalled };
    match bundled_a220_map().and_then(|b| package_version(&b)) {
        Some(available) if newer(&available, &installed) => MapState::Outdated { installed, available },
        _ => MapState::Installed(installed),
    }
}

/// `a` is a later dotted version than `b`.
fn newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| s.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(a) > parse(b)
}

/// The Synaptic A220 is in this Community folder. Marketplace copies live elsewhere, so
/// its absence here does not mean the aircraft is missing.
pub fn a220_in_community(community: &Path) -> bool {
    fs::read_dir(community).map_or(false, |rd| rd.flatten().any(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with("synaptic-aircraft-a220")))
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), &to).with_context(|| format!("copy to {}", to.display()))?;
        }
    }
    Ok(())
}

/// Install (or update) the A220 map into one Community folder. Returns what was done,
/// one line per step, for the activity log.
pub fn install_a220_map(community: &Path) -> Result<Vec<String>> {
    let src = bundled_a220_map().ok_or_else(|| anyhow!("the A220 map files are missing from this installation - reinstall AMDB Bridge"))?;
    let mut notes = Vec::new();
    let parked = community.parent().map(|p| p.join(PARKED)).unwrap_or_else(|| community.join(PARKED));
    for other in A220_MAP_CONFLICTS {
        let from = community.join(other);
        if from.is_dir() {
            fs::create_dir_all(&parked)?;
            let to = parked.join(other);
            if to.exists() {
                fs::remove_dir_all(&to)?;
            }
            fs::rename(&from, &to).with_context(|| format!("move {} aside (is the simulator running?)", from.display()))?;
            notes.push(format!("set {other} aside in {} (both replace the same display file; it comes back if the map is removed)", parked.display()));
        }
    }
    let dest = community.join(A220_MAP_FOLDER);
    // Replaced whole, never merged: layout.json lists every file, and a stale entry stops
    // the simulator loading the package.
    if dest.exists() {
        fs::remove_dir_all(&dest).with_context(|| format!("remove the old map in {} (is the simulator running?)", dest.display()))?;
    }
    copy_dir(&src, &dest)?;
    notes.push(format!("A220 moving map {} installed in {}", package_version(&dest).unwrap_or_default(), community.display()));
    Ok(notes)
}

/// Remove the A220 map from one Community folder and put back what it displaced.
/// Returns false when it was not installed there.
pub fn remove_a220_map(community: &Path) -> Result<bool> {
    let dest = community.join(A220_MAP_FOLDER);
    let was = dest.exists();
    if was {
        fs::remove_dir_all(&dest).with_context(|| format!("remove {} (is the simulator running?)", dest.display()))?;
    }
    if let Some(parked) = community.parent().map(|p| p.join(PARKED)) {
        for other in A220_MAP_CONFLICTS {
            let from = parked.join(other);
            let to = community.join(other);
            if from.is_dir() && !to.exists() {
                fs::rename(&from, &to)?;
            }
        }
    }
    Ok(was)
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
    } else {
        super::hosts::remove()?;
        super::tls::untrust()?;
    }
    Ok(())
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

/// Show a folder or file in Explorer.
pub fn reveal(path: &Path) {
    let _ = Command::new("explorer").arg(path).spawn();
}

/// Undo everything the app may have changed outside its own folder, for the uninstaller:
/// the A220 map in every simulator (putting back what it displaced), the start-up
/// entries, the A350 patch, the hosts-file redirect and the certificate. Each step runs
/// even if an earlier one fails; the failures come back as text.
pub fn uninstall_cleanup() -> Vec<String> {
    let mut problems = Vec::new();
    let mut note = |r: Result<()>, what: &str| {
        if let Err(e) = r {
            problems.push(format!("{what}: {e:#}"));
        }
    };
    for sim in detect_sims() {
        note(remove_a220_map(&sim.community).map(|_| ()), &format!("remove the A220 map from {}", sim.name));
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

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("0.4.0", "0.1.0"));
        assert!(!newer("0.4.0", "0.4.0"));
        assert!(!newer("0.3.9", "0.4"));
    }

    #[test]
    fn map_install_sets_conflicts_aside_and_restores_them() {
        let root = std::env::temp_dir().join(format!("amdb-desktop-{}", std::process::id()));
        let community = root.join("Community");
        let gm5 = community.join("zzz-gm5-a220-amm");
        fs::create_dir_all(&gm5).unwrap();
        fs::write(gm5.join("manifest.json"), "{}").unwrap();
        if bundled_a220_map().is_none() {
            // Only meaningful where the package sits in the source tree above the test binary.
            let _ = fs::remove_dir_all(&root);
            return;
        }
        install_a220_map(&community).unwrap();
        assert!(community.join(A220_MAP_FOLDER).join("manifest.json").is_file());
        assert!(!gm5.exists());
        assert!(root.join(PARKED).join("zzz-gm5-a220-amm").is_dir());
        assert!(matches!(a220_map_state(&community), MapState::Installed(_)));
        assert!(remove_a220_map(&community).unwrap());
        assert!(gm5.join("manifest.json").is_file());
        assert_eq!(a220_map_state(&community), MapState::NotInstalled);
        let _ = fs::remove_dir_all(&root);
    }
}
