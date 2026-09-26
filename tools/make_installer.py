"""Build the Windows downloads.

  dist/AMDB-Bridge-Setup-<version>.exe   everything, as an installer
  dist/AMDB-Navdata-Setup-<version>.exe  the converter on its own, as an installer
  dist/AMDB-Navdata-<version>.zip        the same, as a zip to unpack anywhere
  dist/A320-OANS-Setup-<oans version>.exe  the Fenix A320 OANS on its own, with its own
                                         bridge; versioned as packages/msfs-a320-oans
  dist/AMDB-Airport-Map-Setup-<version>.exe  the Airport Map toolbar window, without a
                                         bridge (it installs through the user's own);
                                         versioned as packages/msfs-amdb-oans-toolbar

Compiles the release binaries, refreshes the A220 map package's layout.json, runs Inno
Setup's compiler on installer/amdb-bridge.iss, and packs the converter separately.

    python tools/make_installer.py

Needs Rust and Inno Setup 6 (winget install JRSoftware.InnoSetup).
"""

import glob
import json
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


def stage_webview2_loader():
    """Copy WebView2Loader.dll beside the built exes.

    `webview2-com-sys` (which `amdb-bridge-gui` links, for the planning panel's web
    view) fetches this DLL during its build and leaves it in a hashed subfolder of
    `target/release/build/`, one per architecture. Cargo never copies it to
    `target/release/` itself -- that is left to whoever packages the final exe -- and
    nothing here ever did, so `AMDB Bridge.exe` could not start on a machine that had
    never had one land beside it by accident. Every release since the web view was
    added shipped that.

    The hashed folder name is not stable across a `cargo update` or a different
    toolchain, so the installer cannot name it directly; this copies the x64 one (the
    only architecture the installer targets) to the one place in `target/release`
    that is stable, and the `.iss` file sources it from there.
    """
    hits = glob.glob(os.path.join(ROOT, "target", "release", "build", "webview2-com-sys-*", "out", "x64", "WebView2Loader.dll"))
    if not hits:
        print("warning: WebView2Loader.dll not found in any webview2-com-sys build output; AMDB Bridge.exe will not start")
        return
    # More than one hashed folder can exist after a dependency bump; the newest build
    # is the one this compile actually produced.
    src = max(hits, key=os.path.getmtime)
    dst = os.path.join(ROOT, "target", "release", "WebView2Loader.dll")
    shutil.copyfile(src, dst)
    print(f"staged {dst}")


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
    # The A320 OANS is built from FlyByWire's source by its own script, which also writes
    # its layout.json; the installer ships whatever that last built.
    if not os.path.isfile(os.path.join(ROOT, "packages", "msfs-a320-oans", "html_ui", "Pages", "VCockpit", "Instruments", "amdb-oans", "oans-nd.js")):
        print("The A320 OANS is not built. Build it with:  cd tools/fenix-oans && npm install && node build.mjs")
        return 1
    run(["cargo", "build", "--release", "--locked", "--bin", "amdb-bridge-gui", "--bin", "amdb-bridge", "--bin", "amdbgen", "--bin", "amdb-navdata", "--bin", "amdb-navdata-gui", "--bin", "a320-oans"])
    stage_webview2_loader()
    run([compiler, f"/DAppVersion={v}", "/Q", os.path.join("installer", "amdb-bridge.iss")])
    run([compiler, f"/DAppVersion={v}", "/Q", os.path.join("installer", "amdb-navdata.iss")])
    # The A320 OANS is versioned as its package, apart from AMDB Bridge.
    oans = json.load(open(os.path.join(ROOT, "packages", "msfs-a320-oans", "manifest.json"), encoding="utf-8"))["package_version"]
    run([compiler, f"/DAppVersion={oans}", "/Q", os.path.join("installer", "a320-oans.iss")])
    # So is the Airport Map, which is built by the same script.
    toolbar = json.load(open(os.path.join(ROOT, "packages", "msfs-amdb-oans-toolbar", "manifest.json"), encoding="utf-8"))["package_version"]
    run([compiler, f"/DAppVersion={toolbar}", "/Q", os.path.join("installer", "amdb-oans-toolbar.iss")])

    out = os.path.join(ROOT, "dist", f"AMDB-Bridge-Setup-{v}.exe")
    print(f"\nBuilt {out} ({os.path.getsize(out) / 1e6:.1f} MB)")
    z = navdata_zip(v)
    print(f"Built {z} ({os.path.getsize(z) / 1e6:.1f} MB)")
    n = os.path.join(ROOT, "dist", f"AMDB-Navdata-Setup-{v}.exe")
    print(f"Built {n} ({os.path.getsize(n) / 1e6:.1f} MB)")
    a = os.path.join(ROOT, "dist", f"A320-OANS-Setup-{oans}.exe")
    print(f"Built {a} ({os.path.getsize(a) / 1e6:.1f} MB)")
    t = os.path.join(ROOT, "dist", f"AMDB-Airport-Map-Setup-{toolbar}.exe")
    print(f"Built {t} ({os.path.getsize(t) / 1e6:.1f} MB)")
    return 0

NAVDATA_README = "AMDB Navdata {v}\n\nThe navigation-data converter on its own.\n\n  AMDB Navdata.exe   a window: tick the aeroplane, press Convert\n  amdb-navdata.exe   the same converter on the command line\n\nIt reads the navigation data Microsoft Flight Simulator already has on this computer\nand writes it into the database an add-on aircraft reads, so the aeroplane flies on\ncurrent data instead of whatever AIRAC cycle it shipped with.\n\nNothing is downloaded and nothing licensed is redistributed: the data is the\nsimulator's own, and it stays on this machine. The aircraft's database is never\noverwritten without a backup being kept beside it first.\n\n  See what would be written, touching nothing:\n    amdb-navdata --to dfd --from-sim --cycle 2609 --dry-run\n\n  Write a Navigraph-layout database (iniBuilds A350, Synaptic A220, PMDG 737 and 777):\n    amdb-navdata --to dfd --from-sim --cycle 2609 --out navdata.db3\n\n  Replace the Fenix A320's own, keeping a backup beside it:\n    amdb-navdata --to fenix --from-sim --cycle 2609 --in-place\n\n  amdb-navdata --help  for the rest.\n\nThe window reads FS2024. FS2020 gives more procedures, and the command line will\nread it: add --sim fs2020.\n\nThis is the same converter as `amdbgen navdata`, which the full AMDB Bridge installer\nstill carries. Take this one if the converter is all you want.\n\nNOT FOR REAL-WORLD NAVIGATION.\n"


def navdata_zip(v):
    """The converter on its own: the one binary, a page on using it, and the licence."""
    exe = os.path.join(ROOT, "target", "release", "amdb-navdata.exe")
    gui = os.path.join(ROOT, "target", "release", "amdb-navdata-gui.exe")
    out = os.path.join(ROOT, "dist", f"AMDB-Navdata-{v}.zip")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    if os.path.exists(out):
        os.remove(out)
    text = NAVDATA_README.format(v=v).replace(chr(10), chr(13) + chr(10))
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        z.write(gui, "AMDB Navdata.exe")
        z.write(exe, "amdb-navdata.exe")
        z.writestr("README.txt", text)
        z.write(os.path.join(ROOT, "LICENSE"), "LICENSE.txt")
    return out


if __name__ == "__main__":
    sys.exit(main())
