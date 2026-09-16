//! Windows resources: the app icon for every executable, and for the desktop app also
//! its manifest (modern controls, DPI awareness, run as the user) and version details.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let resources: [(&str, &[&str]); 2] = [("amdb-bridge-gui.rc", &["amdb-bridge-gui"]), ("amdb-bridge-cli.rc", &["amdb-bridge", "amdbgen"])];
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        for (rc, bins) in resources {
            windres(rc, bins, rc == "amdb-bridge-gui.rc");
        }
    } else {
        embed_resource::compile_for("assets/amdb-bridge-gui.rc", ["amdb-bridge-gui"], embed_resource::NONE).manifest_required().unwrap();
        embed_resource::compile_for("assets/amdb-bridge-cli.rc", ["amdb-bridge", "amdbgen"], embed_resource::NONE).manifest_optional().unwrap();
    }
}

/// GNU windres hands the C preprocessor its paths unquoted, so a project folder with a
/// space in its name breaks it. Running it from inside `assets` with bare file names
/// keeps every path it passes on free of spaces.
///
/// MinGW's GCC also links a `default-manifest.o` of its own into every program, and a
/// second manifest beside ours leaves it to chance which one Windows reads. GCC takes
/// that file from its search path, so with `own_manifest` the resources are written
/// under that name into a folder put first on the path with `-B`: ours replaces the
/// default rather than competing with it.
fn windres(rc: &str, bins: &[&str], own_manifest: bool) {
    let assets = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // GCC splits the paths in its specs on spaces as well, so the folder given to `-B`
    // must have none. The build folder usually sits under the project, whose name may
    // well have spaces; a folder under the temp directory keyed to this build does not.
    let manifest_dir = if own_manifest { spaceless_dir(&out_dir, rc) } else { None };
    let dir = manifest_dir.clone().unwrap_or_else(|| out_dir.join(rc.trim_end_matches(".rc")));
    std::fs::create_dir_all(&dir).unwrap();
    let own_manifest = manifest_dir.is_some();
    if !own_manifest && rc == "amdb-bridge-gui.rc" {
        println!("cargo:warning=no folder without spaces for the manifest object; the desktop app may carry two manifests");
    }
    let out = dir.join(if own_manifest { "default-manifest.o" } else { "resources.o" });
    let tool = std::env::var("WINDRES").unwrap_or_else(|_| "windres".to_string());
    let status = Command::new(&tool)
        .current_dir(&assets)
        .args(["--input", rc, "--input-format=rc", "--output-format=coff", "--output"])
        .arg(&out)
        .status()
        .unwrap_or_else(|e| panic!("could not run {tool} (install MinGW binutils, or set WINDRES): {e}"));
    assert!(status.success(), "{tool} failed on assets/{rc}");
    for bin in bins {
        if own_manifest {
            println!("cargo:rustc-link-arg-bin={bin}=-B{}", dir.display());
        } else {
            println!("cargo:rustc-link-arg-bin={bin}={}", out.display());
        }
    }
}

/// A folder for build output whose path has no spaces, or None.
fn spaceless_dir(out_dir: &std::path::Path, rc: &str) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    out_dir.hash(&mut h);
    let dir = std::env::temp_dir().join(format!("amdbgen-res-{:016x}", h.finish())).join(rc.trim_end_matches(".rc"));
    std::fs::create_dir_all(&dir).ok()?;
    if !dir.display().to_string().contains(' ') {
        return Some(dir);
    }
    // The 8.3 short name, where the volume keeps them.
    let out = Command::new("cmd").args(["/c", &format!("for %I in (\"{}\") do @echo %~sI", dir.display())]).output().ok()?;
    let short = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!short.is_empty() && !short.contains(' ')).then(|| PathBuf::from(short))
}
