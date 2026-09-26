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
    // And any Community folder chosen by hand.
    for c in super::settings::Settings::load().map(|s| s.community_folders).unwrap_or_default() {
        if c.is_dir() && !out.iter().any(|s| same_folder(&s.community, &c)) {
            out.push(Sim { name: "Chosen Community folder".to_string(), community: c });
        }
    }
    out
}

/// The same folder, however it is spelled.
fn same_folder(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Remember a Community folder chosen by hand, so it is looked in from now on.
pub fn remember_community_folder(community: &Path) -> Result<()> {
    let mut s = super::settings::Settings::load().unwrap_or_default();
    if !s.community_folders.iter().any(|c| same_folder(c, community)) {
        s.community_folders.push(community.to_path_buf());
        s.save()?;
    }
    Ok(())
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

/// Folder name of the A320 OANS in a Community folder.
pub const A320_OANS_FOLDER: &str = "amdb-a320-oans";

/// Earlier names of the A320 OANS package, removed when it is installed or removed so that
/// two copies never load at once.
const A320_OANS_OLD_FOLDERS: [&str; 1] = ["zzz-amdb-fenix-oans"];

/// The Fenix A320 is in this Community folder.
pub fn fenix_in_community(community: &Path) -> bool {
    fs::read_dir(community).map_or(false, |rd| rd.flatten().any(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with("fnx-aircraft-320")))
}

/// The Fenix in this Community folder is one the A320 OANS can be added to: one whose
/// panel.cfg and cockpit behaviours are where the MSFS 2020 or MSFS 2024 Fenix keeps them.
pub fn a320_oans_fits(community: &Path) -> bool {
    !patcher::scan_fenix_oans(community).is_empty()
}

/// Why the A320 OANS does not fit a Community folder, for the log: each Fenix-looking
/// folder in it and which of the files the OANS changes it has.
pub fn fenix_diagnosis(community: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(community) else {
        return vec![format!("cannot read {}", community.display())];
    };
    let mut fenix: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.file_name().map_or(false, |n| n.to_string_lossy().to_ascii_lowercase().starts_with("fnx"))).collect();
    fenix.sort();
    if fenix.is_empty() {
        out.push("no fnx* folder in it".to_string());
    }
    for pkg in fenix {
        let fnx = pkg.join("SimObjects/Airplanes/FNX_32X");
        let files = [
            "Panel/panel.cfg",
            "model/FNX32X_Interior.xml",
            "attachments/fnx/Part_Interior_Cockpit/panel/panel.cfg",
            "attachments/fnx/Part_Interior_Cockpit/model/Cockpit_Behavior.xml",
        ];
        let have: Vec<&str> = files.iter().copied().filter(|f| fnx.join(f).is_file()).collect();
        let name = pkg.file_name().unwrap_or_default().to_string_lossy().to_string();
        if !fnx.is_dir() {
            continue; // a livery or another variant
        }
        let parts: Vec<String> = fs::read_dir(fnx.join("attachments/fnx")).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
        out.push(format!(
            "{name}: FNX_32X has {}; attachments/fnx: {}",
            if have.is_empty() { "none of the files the OANS changes".to_string() } else { have.join(", ") },
            if parts.is_empty() { "-".to_string() } else { parts.join(", ") }
        ));
    }
    out
}

/// The A320 OANS shipped with this program: next to the executable once installed, or
/// the package in the source tree when run from a build folder.
pub fn bundled_a320_oans() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let installed = dir.join("msfs").join(A320_OANS_FOLDER);
    if installed.join("manifest.json").is_file() {
        return Some(installed);
    }
    dir.ancestors().map(|a| a.join("packages").join("msfs-a320-oans")).find(|p| p.join("manifest.json").is_file())
}

/// Whether the A320 OANS is in one Community folder, and at which version.
pub fn a320_oans_state(community: &Path) -> MapState {
    let dest = community.join(A320_OANS_FOLDER);
    let Some(installed) = package_version(&dest) else { return MapState::NotInstalled };
    match bundled_a320_oans().and_then(|b| package_version(&b)) {
        Some(available) if newer(&available, &installed) => MapState::Outdated { installed, available },
        _ => MapState::Installed(installed),
    }
}

/// The Fenix in this Community folder still has the lines that load the A320 OANS. A
/// Fenix update replaces the files they were added to, so they can go missing.
pub fn fenix_loads_a320_oans(community: &Path) -> bool {
    let files = patcher::scan_fenix_oans(community);
    !files.is_empty() && files.iter().all(|(_, _, patched)| *patched)
}

/// Install (or update) the A320 OANS into one Community folder and add it to the Fenix
/// there. Returns what was done, one line per step, for the activity log.
pub fn install_a320_oans(community: &Path) -> Result<Vec<String>> {
    let src = bundled_a320_oans().ok_or_else(|| anyhow!("the A320 OANS files are missing from this installation - reinstall AMDB Bridge"))?;
    if !a320_oans_fits(community) {
        return Err(anyhow!("no Fenix A320 in {} that the A320 OANS can be added to (its cockpit files are not where the MSFS 2020 or 2024 Fenix keeps them)", community.display()));
    }
    let mut notes = Vec::new();
    for old in A320_OANS_OLD_FOLDERS {
        let old = community.join(old);
        if old.exists() {
            fs::remove_dir_all(&old).with_context(|| format!("remove the earlier OANS package {} (is the simulator running?)", old.display()))?;
        }
    }
    let dest = community.join(A320_OANS_FOLDER);
    if dest.exists() {
        fs::remove_dir_all(&dest).with_context(|| format!("remove the old A320 OANS in {} (is the simulator running?)", dest.display()))?;
    }
    copy_dir(&src, &dest)?;
    notes.push(format!("A320 OANS {} installed in {}", package_version(&dest).unwrap_or_default(), community.display()));
    // The package adds its own files and replaces nothing. What makes the Fenix load them
    // is two gauge lines in its panel.cfg and the range knob's OANS positions, in the
    // aircraft's own files: a backup of each is kept beside it and removing the OANS puts
    // them back.
    for f in patcher::patch_fenix_oans(community, false)? {
        notes.push(format!("OANS added to the Fenix (backup kept): {}", f.path.display()));
    }
    Ok(notes)
}

/// Add the A320 OANS to the Fenix again where it is installed but a Fenix update has
/// replaced the files it was added to. Returns the files patched.
pub fn repatch_a320_oans(community: &Path) -> Result<Vec<PathBuf>> {
    if !community.join(A320_OANS_FOLDER).join("manifest.json").is_file() {
        return Ok(Vec::new());
    }
    Ok(patcher::patch_fenix_oans(community, false)?.into_iter().map(|f| f.path).collect())
}

/// Remove the A320 OANS from one Community folder and put the Fenix's files back.
/// Returns false when it was not installed there.
pub fn remove_a320_oans(community: &Path) -> Result<bool> {
    patcher::unpatch_fenix_oans(community)?;
    let mut was = false;
    for folder in std::iter::once(A320_OANS_FOLDER).chain(A320_OANS_OLD_FOLDERS) {
        let dest = community.join(folder);
        if dest.exists() {
            fs::remove_dir_all(&dest).with_context(|| format!("remove {} (is the simulator running?)", dest.display()))?;
            was = true;
        }
    }
    Ok(was)
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
    // The package adds the map's own two files and replaces nothing, so what makes the
    // aircraft load them is two lines added to the instrument page it already has. The
    // page is the aircraft's: a backup is kept beside it and removing the map puts it back.
    for f in patcher::patch_a220_page(community, false)? {
        notes.push(format!("moving map added to the A220's instrument page (backup kept): {}", f.path.display()));
    }
    Ok(notes)
}

/// Remove the A220 map from one Community folder and put back what it displaced.
/// Returns false when it was not installed there.
pub fn remove_a220_map(community: &Path) -> Result<bool> {
    // First the two lines, so the aircraft is never left asking for a script that has
    // gone; then the package.
    patcher::unpatch_a220_page(community)?;
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

#[cfg(not(windows))]
fn quiet(program: &str) -> Command {
    super::quiet_command(program)
}

/// A NUL-terminated UTF-16 string for the Win32 wide-character APIs.
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(not(windows))]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "AMDB Bridge";

/// The standalone A320 OANS download is installed, for this user or for everyone: its
/// installer's uninstall entry (the AppId in installer/a320-oans.iss) is there.
#[cfg(windows)]
pub fn standalone_a320_oans_installed() -> bool {
    use winapi::shared::winerror::ERROR_SUCCESS;
    use winapi::um::winnt::KEY_READ;
    use winapi::um::winreg::{RegCloseKey, RegOpenKeyExW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let sub = wide(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{8C3E51A7-4F2B-4D9A-B6E0-71A5D2C9F413}_is1");
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE].into_iter().any(|root| unsafe {
        let mut hkey = std::ptr::null_mut();
        let found = RegOpenKeyExW(root, sub.as_ptr(), 0, KEY_READ, &mut hkey) == ERROR_SUCCESS as i32;
        if found {
            RegCloseKey(hkey);
        }
        found
    })
}

#[cfg(not(windows))]
pub fn standalone_a320_oans_installed() -> bool {
    false
}

/// AMDB Bridge opens when Windows starts.
#[cfg(windows)]
pub fn run_at_login() -> bool {
    use winapi::shared::winerror::ERROR_SUCCESS;
    use winapi::um::winnt::KEY_READ;
    use winapi::um::winreg::{RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_CURRENT_USER};
    let sub = wide(RUN_SUBKEY);
    let value = wide(RUN_VALUE);
    unsafe {
        let mut hkey = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_READ, &mut hkey) != ERROR_SUCCESS as i32 {
            return false;
        }
        let status = RegQueryValueExW(hkey, value.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        RegCloseKey(hkey);
        status == ERROR_SUCCESS as i32
    }
}

/// AMDB Bridge opens when Windows starts.
#[cfg(not(windows))]
pub fn run_at_login() -> bool {
    quiet("reg").args(["query", RUN_KEY, "/v", RUN_VALUE]).output().map_or(false, |o| o.status.success())
}

/// Open (or stop opening) `exe --tray` when Windows starts, for this user only.
#[cfg(windows)]
pub fn set_run_at_login(on: bool, exe: &Path) -> Result<()> {
    use winapi::shared::winerror::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use winapi::um::winnt::{KEY_SET_VALUE, REG_SZ};
    use winapi::um::winreg::{RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY_CURRENT_USER};
    let sub = wide(RUN_SUBKEY);
    let value = wide(RUN_VALUE);
    unsafe {
        let mut hkey = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) != ERROR_SUCCESS as i32 {
            // No key means nothing to remove; only being asked to add is then a failure.
            return if on { Err(anyhow!("could not open the Windows start-up registry key")) } else { Ok(()) };
        }
        let status = if on {
            // REG_SZ data carries its own terminating NUL, so the whole wide string counts.
            let data = wide(&format!("\"{}\" --tray", exe.display()));
            RegSetValueExW(hkey, value.as_ptr(), 0, REG_SZ, data.as_ptr() as *const u8, (data.len() * 2) as u32)
        } else {
            match RegDeleteValueW(hkey, value.as_ptr()) {
                // Absent already is the state asked for, not a failure.
                s if s == ERROR_FILE_NOT_FOUND as i32 => ERROR_SUCCESS as i32,
                s => s,
            }
        };
        RegCloseKey(hkey);
        if status != ERROR_SUCCESS as i32 {
            return Err(anyhow!("could not change the Windows start-up entry (registry error {status})"));
        }
    }
    Ok(())
}

/// Open (or stop opening) `exe --tray` when Windows starts, for this user only.
#[cfg(not(windows))]
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
    setup_navigraph_with(on, false)
}

/// The same, optionally covering the hosts the Fenix A320's flight bag calls as well.
///
/// It is the one flight bag that cannot be patched -- its web app is served out of an
/// encrypted bundle by its own local gateway and exists nowhere on disk -- so the only
/// way to answer it is to be the address it calls.
///
/// This is deliberately not what `navigraph on` does by default. The map's own host
/// serves one aircraft feature; these three include the sign-in, and while they are
/// redirected every program on the machine that resolves them reaches this bridge, not
/// only the aeroplane. That is a bigger thing to switch on than a moving map, so it is
/// asked for separately and says so.
///
/// One certificate carries every name, because a server presents one certificate per
/// connection whichever host was asked for, and it is signed by the authority already in
/// the store rather than by a new one each time.
pub fn setup_navigraph_with(on: bool, with_efb: bool) -> Result<()> {
    let domains = super::navigraph_domains(with_efb);
    if on {
        let m = super::tls::ensure_for(super::NAVIGRAPH_AMDB_DOMAIN, &domains)?;
        // A newly made authority has the same name as the one it replaces, so the store
        // would report it installed and refuse everything it signs. The old one goes
        // first. This is the ordinary case on a machine set up before the authority's key
        // was kept, where there is no key to reuse and a new one has to be made.
        if m.fresh_ca {
            let _ = super::tls::untrust();
        }
        super::tls::trust(&m)?;
        super::hosts::install_all(&domains)?;
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
#[cfg(windows)]
pub fn sim_running() -> bool {
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::tlhelp32::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase();
                if name == "flightsimulator.exe" || name == "flightsimulator2024.exe" {
                    found = true;
                    break;
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        found
    }
}

/// A Microsoft Flight Simulator is running. It reads Community packages only at start,
/// so a map installed now appears after the next restart.
#[cfg(not(windows))]
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
/// the A220 map and the A320 OANS in every simulator (putting back what they changed), the start-up
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
        // The standalone A320 OANS download uses the same package; it stays while that is installed.
        if !standalone_a320_oans_installed() {
            note(remove_a320_oans(&sim.community).map(|_| ()), &format!("remove the A320 OANS from {}", sim.name));
        }
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

    #[test]
    fn a320_oans_goes_into_the_fenix_and_comes_out_cleanly() {
        if bundled_a320_oans().is_none() {
            // Only meaningful where the package sits in the source tree above the test binary.
            return;
        }
        let root = std::env::temp_dir().join(format!("amdb-a320-oans-{}", std::process::id()));
        let community = root.join("Community");
        let fenix = community.join("fnx-aircraft-320").join("SimObjects/Airplanes/FNX_32X");
        let (panel, model) = (fenix.join("Panel/panel.cfg"), fenix.join("model/FNX32X_Interior.xml"));
        let panel_text = "[VCockpit02]\nsize_mm=768,768\npixel_size=768,768\ntexture=$A320_ND_Captain\nhtmlgauge00=B, 0,0,768,768\n\n[VPainting01]\nsize_mm=605,128\n";
        let model_text = "\t<UseTemplate Name=\"FNX32X_Interact_Knob_Increment_Template\">\n\t\t<ANIM_NAME>EFIS_1_Range_Selector_Knob</ANIM_NAME>\n\t</UseTemplate>\n";
        for (path, text) in [(&panel, panel_text), (&model, model_text)] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        // The package's list of its files and their sizes, as Fenix ships it.
        let layout = community.join("fnx-aircraft-320").join("layout.json");
        let write_layout = |panel_len: usize| {
            let entry = |p: &str, n: usize| format!("    {{\n      \"path\": \"{p}\",\n      \"size\": {n},\n      \"date\": 133000000000000000\n    }}");
            let entries = [entry("SimObjects/Airplanes/FNX_32X/Panel/panel.cfg", panel_len), entry("SimObjects/Airplanes/FNX_32X/model/FNX32X_Interior.xml", model_text.len())];
            fs::write(&layout, format!("{{\n  \"content\": [\n{}\n  ]\n}}\n", entries.join(",\n"))).unwrap();
        };
        write_layout(panel_text.len());
        let listed = |path: &Path| -> u64 {
            let rel = path.strip_prefix(community.join("fnx-aircraft-320")).unwrap().to_string_lossy().replace('\\', "/");
            let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&layout).unwrap()).unwrap();
            v["content"].as_array().unwrap().iter().find(|e| e["path"].as_str().unwrap().eq_ignore_ascii_case(&rel)).unwrap()["size"].as_u64().unwrap()
        };
        let size = |path: &Path| fs::metadata(path).unwrap().len();
        let old = community.join("zzz-amdb-fenix-oans");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("manifest.json"), "{}").unwrap();

        install_a320_oans(&community).unwrap();
        assert!(matches!(a320_oans_state(&community), MapState::Installed(_)));
        assert!(fenix_loads_a320_oans(&community));
        assert!(!old.exists(), "the earlier package would load alongside");
        assert!(fs::read_to_string(&panel).unwrap().contains("amdb-oans/oans-nd.html"));
        assert_eq!((listed(&panel), listed(&model)), (size(&panel), size(&model)), "layout.json follows the changed files");

        // A Fenix update puts its own, newer files back; serving adds the OANS again, and the
        // backup follows the update rather than keeping the version before it.
        let updated = panel_text.replace("size_mm=605,128", "size_mm=605,130");
        fs::write(&panel, &updated).unwrap();
        write_layout(updated.len());
        assert!(!fenix_loads_a320_oans(&community));
        assert_eq!(repatch_a320_oans(&community).unwrap(), vec![panel.clone()]);
        assert!(fenix_loads_a320_oans(&community));
        assert_eq!(listed(&panel), size(&panel));

        assert!(remove_a320_oans(&community).unwrap());
        assert_eq!(a320_oans_state(&community), MapState::NotInstalled);
        assert_eq!(fs::read_to_string(&panel).unwrap(), updated, "the updated Fenix file, not the one before the update");
        assert_eq!(fs::read_to_string(&model).unwrap(), model_text);
        assert_eq!((listed(&panel), listed(&model)), (size(&panel), size(&model)), "and layout.json back in step");
        let _ = fs::remove_dir_all(&root);
    }
}
