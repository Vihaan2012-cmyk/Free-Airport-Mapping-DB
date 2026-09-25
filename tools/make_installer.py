"""Build the Windows downloads.

  dist/AMDB-Bridge-Setup-<version>.exe   everything, as an installer
  dist/AMDB-Navdata-<version>.zip        the navigation-data converter on its own

Compiles the release binaries, refreshes the A220 map package's layout.json, runs Inno
Setup's compiler on installer/amdb-bridge.iss, and packs the converter separately.

    python tools/make_installer.py

Needs Rust and Inno Setup 6 (winget install JRSoftware.InnoSetup).
"""

import os
import re
import shutil
import subprocess
import sys
import zipfile

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
    run(["cargo", "build", "--release", "--locked", "--bin", "amdb-bridge-gui", "--bin", "amdb-bridge", "--bin", "amdbgen", "--bin", "amdb-navdata"])
    run([compiler, f"/DAppVersion={v}", "/Q", os.path.join("installer", "amdb-bridge.iss")])

    out = os.path.join(ROOT, "dist", f"AMDB-Bridge-Setup-{v}.exe")
    print(f"\nBuilt {out} ({os.path.getsize(out) / 1e6:.1f} MB)")
    z = navdata_zip(v)
    print(f"Built {z} ({os.path.getsize(z) / 1e6:.1f} MB)")
    return 0

NAVDATA_README = "AMDB Navdata {v}\n\nThe navigation-data converter on its own.\n\nIt reads the navigation data Microsoft Flight Simulator already has on this computer\nand writes it into the database an add-on aircraft reads, so the aeroplane flies on\ncurrent data instead of whatever AIRAC cycle it shipped with.\n\nNothing is downloaded and nothing licensed is redistributed: the data is the\nsimulator's own, and it stays on this machine. The aircraft's database is never\noverwritten without a backup being kept beside it first.\n\n  See what would be written, touching nothing:\n    amdb-navdata --to dfd --from-sim --cycle 2503 --dry-run\n\n  Write a Navigraph-layout database (iniBuilds A350, Synaptic A220, PMDG 737 and 777):\n    amdb-navdata --to dfd --from-sim --cycle 2503 --out navdata.db3\n\n  Replace the Fenix A320's own, keeping a backup beside it:\n    amdb-navdata --to fenix --from-sim --cycle 2503 --in-place\n\n  amdb-navdata --help  for the rest.\n\nThis is the same converter as `amdbgen navdata`, which the full AMDB Bridge installer\nstill carries. Take this one if the converter is all you want.\n\nNOT FOR REAL-WORLD NAVIGATION.\n"


def navdata_zip(v):
    """The converter on its own: the one binary, a page on using it, and the licence."""
    exe = os.path.join(ROOT, "target", "release", "amdb-navdata.exe")
    out = os.path.join(ROOT, "dist", f"AMDB-Navdata-{v}.zip")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    if os.path.exists(out):
        os.remove(out)
    text = NAVDATA_README.format(v=v).replace(chr(10), chr(13) + chr(10))
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        z.write(exe, "amdb-navdata.exe")
        z.writestr("README.txt", text)
        z.write(os.path.join(ROOT, "LICENSE"), "LICENSE.txt")
    return out


if __name__ == "__main__":
    sys.exit(main())
