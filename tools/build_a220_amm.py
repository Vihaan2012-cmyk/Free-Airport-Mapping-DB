"""Package and install the AMDB Airport Moving Map for the Synaptic A220.

Writes the layout.json MSFS needs, stamps the package size into the manifest, and copies
the result into every Community folder found on this machine (MSFS 2020 and 2024).

The installed folder is named `zzz-amdb-a220-amm` on purpose. MSFS 2020 applies Community
packages alphabetically, so an instrument override only wins if it sorts after the
aircraft it overrides (`synaptic-aircraft-a220`); MSFS 2024 goes by the manifest's
package_order_hint instead, and is unbothered by the prefix. One folder name therefore
works on both.

    python tools/build_a220_amm.py --dry-run     # show what would be installed
    python tools/build_a220_amm.py               # build and install
    python tools/build_a220_amm.py --to DIR      # build into a folder of your own
"""

import argparse
import json
import os
import shutil
import sys

SRC = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "packages", "msfs-a220-amm")
FOLDER = "zzz-amdb-a220-amm"
CONFLICTS = ["zzz-gm5-a220-amm", "gm5-a220-amm"]
AIRCRAFT = "synaptic-aircraft-a220"


def filetime(path):
    """Windows FILETIME: 100-nanosecond ticks since 1601, which is what layout.json wants."""
    return int((os.path.getmtime(path) + 11644473600) * 10_000_000)


def package_files(root):
    """Every file that belongs in layout.json: everything but layout.json itself."""
    out = []
    for dirpath, _, names in os.walk(root):
        for name in sorted(names):
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, root).replace("\\", "/")
            if rel == "layout.json":
                continue
            out.append((rel, full))
    return sorted(out)


def build(root):
    """Write layout.json and update the manifest's total size, in place."""
    entries = []
    total = 0
    for rel, full in package_files(root):
        size = os.path.getsize(full)
        total += size
        # MSFS records these paths lower-cased.
        entries.append({"path": rel.lower(), "size": size, "date": filetime(full)})
    with open(os.path.join(root, "layout.json"), "w", encoding="utf-8") as fh:
        json.dump({"content": entries}, fh, indent=2)
        fh.write("\n")

    mpath = os.path.join(root, "manifest.json")
    with open(mpath, encoding="utf-8") as fh:
        manifest = json.load(fh)
    manifest["total_package_size"] = str(total).zfill(20)
    with open(mpath, "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")
    return entries, total


def community_dirs():
    """Community folders for MSFS 2020 and 2024 (Store and Steam)."""
    out = []
    local = os.environ.get("LOCALAPPDATA", "")
    roaming = os.environ.get("APPDATA", "")
    for cfg in [
        os.path.join(local, "Packages/Microsoft.FlightSimulator_8wekyb3d8bbwe/LocalCache/UserCfg.opt"),
        os.path.join(local, "Packages/Microsoft.Limitless_8wekyb3d8bbwe/LocalCache/UserCfg.opt"),
        os.path.join(roaming, "Microsoft Flight Simulator/UserCfg.opt"),
        os.path.join(roaming, "Microsoft Flight Simulator 2024/UserCfg.opt"),
    ]:
        try:
            with open(cfg, encoding="utf-8", errors="ignore") as fh:
                for line in fh:
                    line = line.strip()
                    if line.startswith("InstalledPackagesPath"):
                        d = os.path.normpath(os.path.join(line.split(None, 1)[1].strip().strip('"'), "Community"))
                        if os.path.isdir(d) and d not in out:
                            out.append(d)
        except (OSError, IndexError):
            pass
    for d in [
        os.path.join(local, "Packages/Microsoft.FlightSimulator_8wekyb3d8bbwe/LocalCache/Packages/Community"),
        os.path.join(local, "Packages/Microsoft.Limitless_8wekyb3d8bbwe/LocalCache/Packages/Community"),
    ]:
        d = os.path.normpath(d)
        if os.path.isdir(d) and d not in out:
            out.append(d)
    return out


def main():
    ap = argparse.ArgumentParser(description="Build and install the A220 airport moving map.")
    ap.add_argument("--to", action="append", metavar="COMMUNITY", help="install into this folder (repeatable); default: every one detected")
    ap.add_argument("--dry-run", action="store_true", help="report what would happen, copy nothing")
    args = ap.parse_args()

    if not os.path.isdir(SRC):
        print("Package source not found:", SRC)
        return 1

    entries, total = build(SRC)
    print("Built %s: %d files, %.1f kB" % (FOLDER, len(entries), total / 1000))
    for e in entries:
        print("   %-72s %7d" % (e["path"], e["size"]))

    targets = args.to or community_dirs()
    if not targets:
        print("\nNo Community folder found. Pass one with --to.")
        return 1

    print()
    for community in targets:
        dest = os.path.join(community, FOLDER)
        sim = "MSFS 2024" if ("Limitless" in community or "2024" in community) else "MSFS 2020"
        print("%s  %s" % (sim, dest))

        if not os.path.isdir(os.path.join(community, AIRCRAFT)):
            print("   note: %s is not in this folder, so the map has no aircraft to attach to here" % AIRCRAFT)
        for other in CONFLICTS:
            if os.path.isdir(os.path.join(community, other)):
                print("   CONFLICT: %s overrides the same display-unit file. Remove it, or only one of the two will load." % other)

        if args.dry_run:
            print("   [dry-run] would copy %d files" % len(entries))
            continue
        if os.path.isdir(dest):
            shutil.rmtree(dest)
        shutil.copytree(SRC, dest)
        print("   installed")

    if not args.dry_run:
        print("\nStart `amdb-bridge serve` (no admin needed for this map), then load the A220.")
        print("The map shows itself on the ground; bind L:AMDB_AMM_VISIBLE / L:AMDB_AMM_RANGE to keys to control it.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
