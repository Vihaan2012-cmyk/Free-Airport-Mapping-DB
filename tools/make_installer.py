"""Build the Windows installer: dist/AMDB-Bridge-Setup-<version>.exe.

Compiles the release binaries, refreshes the A220 map package's layout.json, and runs
Inno Setup's compiler on installer/amdb-bridge.iss.

    python tools/make_installer.py

Needs Rust and Inno Setup 6 (winget install JRSoftware.InnoSetup).
"""

import os
import re
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def version():
    text = open(os.path.join(ROOT, "Cargo.toml"), encoding="utf-8").read()
    return re.search(r'^version\s*=\s*"([^"]+)"', text, re.M).group(1)


def iscc():
    found = shutil.which("ISCC") or shutil.which("iscc")
    if found:
        return found
    for base in (os.environ.get("LOCALAPPDATA", ""), os.environ.get("ProgramFiles(x86)", ""), os.environ.get("ProgramFiles", "")):
        for sub in (os.path.join("Programs", "Inno Setup 6"), "Inno Setup 6"):
            p = os.path.join(base, sub, "ISCC.exe")
            if os.path.isfile(p):
                return p
    return None


def run(cmd, **kw):
    print(">", " ".join(cmd))
    subprocess.run(cmd, cwd=ROOT, check=True, **kw)


def main():
    compiler = iscc()
    if not compiler:
        print("Inno Setup 6 not found. Install it with:  winget install JRSoftware.InnoSetup")
        return 1
    v = version()

    manifest = os.path.join(ROOT, "packages", "msfs-a220-amm", "manifest.json")
    pkg = open(manifest, encoding="utf-8").read()
    if f'"package_version": "{v}"' not in pkg:
        print(f"note: the A220 map package is not version {v}; the app will not offer it as an update")

    # layout.json lists every file with its size, so it has to be rebuilt after any change.
    run([sys.executable, os.path.join("tools", "build_a220_amm.py"), "--dry-run"], stdout=subprocess.DEVNULL)
    run(["cargo", "build", "--release", "--locked", "--bin", "amdb-bridge-gui", "--bin", "amdb-bridge", "--bin", "amdbgen"])
    run([compiler, f"/DAppVersion={v}", "/Q", os.path.join("installer", "amdb-bridge.iss")])

    out = os.path.join(ROOT, "dist", f"AMDB-Bridge-Setup-{v}.exe")
    print(f"\nBuilt {out} ({os.path.getsize(out) / 1e6:.1f} MB)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
