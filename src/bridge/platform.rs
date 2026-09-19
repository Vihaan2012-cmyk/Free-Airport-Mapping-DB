//! Where things live on each operating system, and the Linux side of running Microsoft
//! Flight Simulator through Steam's Proton.

use std::path::{Path, PathBuf};

/// The home folder of the person using the program. Under `sudo` that is the user who
/// ran sudo, not root, so a one-time setup run with sudo keeps its files where the
/// program will look for them afterwards.
pub fn user_home() -> PathBuf {
    #[cfg(unix)]
    if let Ok(user) = std::env::var("SUDO_USER") {
        if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
            if let Some(home) = passwd.lines().find_map(|l| {
                let f: Vec<&str> = l.split(':').collect();
                (f.len() > 5 && f[0] == user).then(|| PathBuf::from(f[5]))
            }) {
                return home;
            }
        }
    }
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// The program's own folder: settings, certificates and, by default, built airports.
pub fn data_dir() -> PathBuf {
    if cfg!(windows) {
        return std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(".")).join("amdb-bridge");
    }
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) if std::env::var_os("SUDO_USER").is_none() => PathBuf::from(d).join("amdb-bridge"),
        _ => user_home().join(".local").join("share").join("amdb-bridge"),
    }
}

/// After a setup run with sudo, give the files it created in the user's folder back to
/// the user, or the program could not write its own settings afterwards.
pub fn return_to_user(path: &Path) {
    #[cfg(unix)]
    if let (Ok(uid), Ok(gid)) = (std::env::var("SUDO_UID"), std::env::var("SUDO_GID")) {
        let _ = std::process::Command::new("chown").arg("-R").arg(format!("{uid}:{gid}")).arg(path).output();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Every Steam library folder on this machine (Linux): the standard, Flatpak and Snap
/// installs, and the extra libraries listed in each one's libraryfolders.vdf.
pub fn steam_libraries() -> Vec<PathBuf> {
    let home = user_home();
    let roots = [
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".steam/root"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        home.join("snap/steam/common/.local/share/Steam"),
    ];
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        let p = std::fs::canonicalize(&p).unwrap_or(p);
        if p.join("steamapps").is_dir() && !out.contains(&p) {
            out.push(p);
        }
    };
    for r in roots {
        add(r.clone());
        if let Ok(vdf) = std::fs::read_to_string(r.join("steamapps").join("libraryfolders.vdf")) {
            for line in vdf.lines() {
                let parts: Vec<&str> = line.split('"').collect();
                if parts.len() >= 4 && parts[1] == "path" {
                    add(PathBuf::from(parts[3].replace("\\\\", "\\")));
                }
            }
        }
    }
    out
}

/// A Windows path as written inside a Proton prefix (`C:\users\...`, `Z:\home\...`),
/// turned into the Linux path it points at.
pub fn wine_path(pfx: &Path, win: &str) -> PathBuf {
    let win = win.trim().trim_matches('"');
    let (drive, rest) = match win.split_once(':') {
        Some((d, r)) if d.len() == 1 => (d.to_ascii_lowercase(), r),
        _ => return PathBuf::from(win.replace('\\', "/")),
    };
    let rest = rest.replace('\\', "/");
    let rest = rest.trim_start_matches('/');
    // Wine maps each drive letter through a link in dosdevices; C: and Z: also have
    // fixed defaults for prefixes where the link cannot be read.
    let base = std::fs::canonicalize(pfx.join("dosdevices").join(format!("{drive}:"))).unwrap_or_else(|_| match drive.as_str() {
        "c" => pfx.join("drive_c"),
        "z" => PathBuf::from("/"),
        _ => pfx.join("dosdevices").join(format!("{drive}:")),
    });
    base.join(rest)
}

/// Steam app ids of Microsoft Flight Simulator 2020 and 2024.
pub const MSFS_APPS: [(&str, &str, &str); 2] = [("MSFS 2020 (Proton)", "1250410", "Microsoft Flight Simulator"), ("MSFS 2024 (Proton)", "2537590", "Microsoft Flight Simulator 2024")];

/// Microsoft Flight Simulator installs under Proton: (name, Community folder).
pub fn proton_sims() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for lib in steam_libraries() {
        for (name, app, folder) in MSFS_APPS {
            let pfx = lib.join("steamapps").join("compatdata").join(app).join("pfx");
            let roaming = pfx.join("drive_c/users/steamuser/AppData/Roaming").join(folder);
            let from_cfg = std::fs::read_to_string(roaming.join("UserCfg.opt")).ok().and_then(|text| {
                text.lines().find_map(|l| l.trim().strip_prefix("InstalledPackagesPath").map(|p| wine_path(&pfx, p).join("Community")))
            });
            let community = from_cfg.filter(|c| c.is_dir()).or_else(|| Some(roaming.join("Packages").join("Community")).filter(|c| c.is_dir()));
            if let Some(c) = community {
                if !out.iter().any(|(_, p)| p == &c) {
                    out.push((name.to_string(), c));
                }
            }
        }
    }
    out
}

/// X-Plane installs on Linux: the folders its installer records, then Steam.
pub fn xplane_candidates() -> Vec<PathBuf> {
    let home = user_home();
    let mut out = Vec::new();
    for name in ["x-plane_install_12.txt", "x-plane_install_11.txt"] {
        if let Ok(text) = std::fs::read_to_string(home.join(".x-plane").join(name)) {
            out.extend(text.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| PathBuf::from(l.trim_end_matches('/'))));
        }
    }
    for lib in steam_libraries() {
        out.push(lib.join("steamapps/common/X-Plane 12"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn wine_paths_map_to_linux() {
        let pfx = Path::new("/nonexistent/pfx");
        assert_eq!(wine_path(pfx, r#""C:\users\steamuser\AppData\Roaming\Microsoft Flight Simulator\Packages""#), pfx.join("drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator/Packages"));
        assert_eq!(wine_path(pfx, r"Z:\mnt\games\MSFS"), PathBuf::from("/mnt/games/MSFS"));
    }
}
