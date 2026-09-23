"""Put the GM5 A220 moving map patcher back, with its author's permission.

It was taken out in v0.5.1 because it held six lines of that add-on's own source verbatim
as a search anchor, so those lines travelled inside our binary wherever it went. That was
the whole objection, and permission settles it.

The patcher is restored from the commit before its removal. That commit's copy of
patcher.rs predates the Synaptic instrument-page patcher added in v0.5.1, so this script
puts those functions back afterwards, and re-adds the GM5 call sites to the bridge's
command line. Run it from the repository root:

    python tools/readd_gm5_patcher.py
    cargo test
"""

import os
import subprocess
import sys

# The commit before the removal, which still has the GM5 block.
BEFORE_REMOVAL = "39beabe"

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
os.chdir(ROOT)

PATCHER = "src/bridge/patcher.rs"
CLI = "src/bridge/cli.rs"


def sub(path, old, new, count=1):
    text = open(path, encoding="utf-8").read()
    found = text.count(old)
    if found != count:
        sys.exit(f"{path}: expected {count} of {old[:60]!r}, found {found}")
    open(path, "w", encoding="utf-8", newline="").write(text.replace(old, new))


def already_done():
    return "A220_TOKEN_FN" in open(PATCHER, encoding="utf-8").read()


if already_done():
    sys.exit("The GM5 patcher is already there; nothing to do.")

# ---------------------------------------------------------------------------------
# 1. The file as it was before the removal.
# ---------------------------------------------------------------------------------
subprocess.run(["git", "checkout", BEFORE_REMOVAL, "--", PATCHER], check=True)
print(f"restored {PATCHER} from {BEFORE_REMOVAL}")

# ---------------------------------------------------------------------------------
# 2. The Synaptic instrument-page patcher, which that copy predates.
# ---------------------------------------------------------------------------------
PAGE_BLOCK = r'''// ---------------------------------------------------------------------------------
// Synaptic A220 display units. Our moving map ships as its own two files and replaces
// nothing; what makes the aircraft load them is two lines added to the instrument page
// it already has. The page is the aircraft's, so it is edited where it sits, a backup is
// kept beside it, and `unpatch` puts it back.
// ---------------------------------------------------------------------------------

const A220_PAGE_MARK: &str = "<!--amdb-bridge-a220-->";
/// The aircraft's own bundle, which the two lines are added after so the map loads last.
const A220_PAGE_ANCHOR: &str = "/Pages/VCockpit/Instruments/a22x/DisplayUnits/instrument.index.js\"></script>";

/// Add the map's script and stylesheet to the A220's instrument page. None when the page
/// is not that page, or already carries them.
pub fn patch_a220_page_text(text: &str) -> Option<String> {
    if text.contains(A220_PAGE_MARK) || text.contains("amdb-a220-amm.js") {
        return None;
    }
    let at = text.find(A220_PAGE_ANCHOR)? + A220_PAGE_ANCHOR.len();
    let added = format!(
        "\n{A220_PAGE_MARK}\n<script type=\"text/html\" import-async=\"false\" import-script=\"/Pages/VCockpit/Instruments/a22x/DisplayUnits/amdb-a220-amm.js\"></script>\n<link rel=\"stylesheet\" href=\"/Pages/VCockpit/Instruments/a22x/DisplayUnits/amdb-a220-amm.css\" />"
    );
    Some(format!("{}{}{}", &text[..at], added, &text[at..]))
}

/// A220 instrument pages in a Community folder: (package, file, already patched).
pub fn scan_a220_page(community: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(community) else { return out };
    for pkg in rd.flatten() {
        let pdir = pkg.path();
        if !pdir.is_dir() {
            continue;
        }
        let name = pkg.file_name().to_string_lossy().to_string();
        // Our own package carries no instrument page, so only the aircraft's is found.
        let page = pdir.join("html_ui/Pages/VCockpit/Instruments/a22x/DisplayUnits/instrument.html");
        let Ok(text) = fs::read_to_string(&page) else { continue };
        if text.contains(A220_PAGE_ANCHOR) || text.contains(A220_PAGE_MARK) {
            let patched = text.contains(A220_PAGE_MARK) || text.contains("amdb-a220-amm.js");
            out.push((name, page, patched));
        }
    }
    out
}

/// Add the map to the A220's instrument page in a Community folder, where the map package
/// is installed alongside it (backups kept, recorded for `unpatch`).
pub fn patch_a220_page(community: &Path, dry_run: bool) -> Result<Vec<PatchedFile>> {
    // Nothing to load if the map itself is not installed, and a page pointed at a script
    // that is not there would only log an error every frame.
    if !a220_map_installed(community) {
        return Ok(Vec::new());
    }
    let mut record = load_record(community);
    let mut done = Vec::new();
    for (pkg, path, patched) in scan_a220_page(community) {
        if patched {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let Some(new_text) = patch_a220_page_text(&text) else {
            log::warn!("{pkg}: {} is a version of the A220 instrument page this bridge does not know", path.display());
            continue;
        };
        log::info!("{}{}: moving map added to {}", if dry_run { "[dry-run] " } else { "" }, pkg, path.display());
        if dry_run {
            done.push(PatchedFile { path: path.clone(), backup: PathBuf::new(), replacements: 1 });
            continue;
        }
        let backup = PathBuf::from(format!("{}{}", path.display(), BACKUP_SUFFIX));
        if !backup.exists() {
            fs::copy(&path, &backup).with_context(|| format!("backup {}", path.display()))?;
        }
        fs::write(&path, new_text)?;
        let pf = PatchedFile { path: path.clone(), backup, replacements: 1 };
        record.files.retain(|f| f.path != pf.path);
        record.files.push(pf.clone());
        done.push(pf);
    }
    if !dry_run && !done.is_empty() {
        save_record(community, &record)?;
    }
    Ok(done)
}

/// Whether our map package is present in this Community folder.
pub fn a220_map_installed(community: &Path) -> bool {
    let Ok(rd) = fs::read_dir(community) else { return false };
    rd.flatten().any(|e| {
        e.path()
            .join("html_ui/Pages/VCockpit/Instruments/a22x/DisplayUnits/amdb-a220-amm.js")
            .is_file()
    })
}

/// Candidate Community folders for MSFS 2020 and 2024 (Store and Steam) on this machine.'''

sub(PATCHER, "/// Candidate Community folders for MSFS 2020 and 2024 (Store and Steam) on this machine.", PAGE_BLOCK)

RESTORE_BLOCK = r'''/// Put one patched file back and forget it. Returns false when it was not patched.
pub fn restore_one(community: &Path, path: &Path) -> Result<bool> {
    let mut record = load_record(community);
    let Some(f) = record.files.iter().find(|f| f.path == path).cloned() else { return Ok(false) };
    if f.backup.is_file() {
        fs::copy(&f.backup, &f.path).with_context(|| format!("restore {}", f.path.display()))?;
        let _ = fs::remove_file(&f.backup);
    }
    record.files.retain(|o| o.path != f.path);
    if record.files.is_empty() {
        let _ = fs::remove_file(record_path(community));
    } else {
        save_record(community, &record)?;
    }
    Ok(true)
}

/// Take the moving map out of the A220's instrument page, wherever it was added.
pub fn unpatch_a220_page(community: &Path) -> Result<usize> {
    let mut n = 0;
    for (_, path, patched) in scan_a220_page(community) {
        if patched && restore_one(community, &path)? {
            n += 1;
        }
    }
    Ok(n)
}

/// Every simulator exe.xml that exists on this machine.'''

sub(PATCHER, "/// Every simulator exe.xml that exists on this machine.", RESTORE_BLOCK)

PAGE_TEST = r'''    #[test]
    fn a220_page_loads_the_map_and_keeps_the_aircraft_intact() {
        let page = "<script type=\"text/html\" id=\"DisplayUnits\">\n</script>\n<script type=\"text/html\" import-async=\"false\" import-script=\"/Pages/VCockpit/Instruments/a22x/DisplayUnits/instrument.index.js\"></script>\n<link rel=\"stylesheet\" href=\"/Pages/VCockpit/Instruments/a22x/DisplayUnits/instrument.css\" />\n";
        let out = patch_a220_page_text(page).unwrap();
        assert!(out.contains("amdb-a220-amm.js"));
        assert!(out.contains("amdb-a220-amm.css"));
        // The aircraft's own two lines survive, in order, ahead of ours.
        assert!(out.find("instrument.index.js").unwrap() < out.find("amdb-a220-amm.js").unwrap());
        assert!(out.contains("instrument.css"));
        assert!(patch_a220_page_text(&out).is_none(), "idempotent");
        assert!(patch_a220_page_text("some other instrument page").is_none());
    }

    #[test]
    fn patches_and_restores() {'''

sub(PATCHER, "    #[test]\n    fn patches_and_restores() {", PAGE_TEST)
print("re-added the instrument-page patcher")

# ---------------------------------------------------------------------------------
# 3. The GM5 call sites, which the removal took out of the command line.
# ---------------------------------------------------------------------------------
sub(CLI, """            match patcher::patch_a220_page(&d, false) {""",
    """            match patcher::patch_a220(&d, false) {
                Ok(files) => {
                    for f in files {
                        crate::term::success(&format!("GM5 A220 moving map patched to work with the bridge (backup kept, `unpatch` restores): {}", f.path.display()));
                    }
                }
                Err(e) => crate::term::warn(&format!("could not patch the A220 moving map in {}: {e:#}", d.display())),
            }
            for (pkg, _, _) in patcher::scan_a220(&d) {
                if let Some(w) = patcher::a220_load_order_warning(&d, &pkg) {
                    crate::term::warn(&w);
                }
            }
            match patcher::patch_a220_page(&d, false) {""")

sub(CLI, """                for (pkg, f, patched) in patcher::scan_a220_page(&d) {""",
    """                for (pkg, f, patched) in patcher::scan_a220(&d) {
                    println!("  {}  A220 moving map {}: {}", pkg, if patched { "PATCHED (token fallback + bridge airport search)" } else { "not patched (run `serve` or `patch`)" }, f.file_name().unwrap_or_default().to_string_lossy());
                    if let Some(w) = patcher::a220_load_order_warning(&d, &pkg) {
                        println!("  WARNING {w}");
                    }
                }
                for (pkg, f, patched) in patcher::scan_a220_page(&d) {""")

sub(CLI, """                total += patcher::patch_a220_page(&d, dry_run)?.len();""",
    """                total += patcher::patch_a220(&d, dry_run)?.len();
                total += patcher::patch_a220_page(&d, dry_run)?.len();""")
print("re-added the GM5 call sites")

print("\nDone. Now run:  cargo test")
