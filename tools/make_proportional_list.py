#!/usr/bin/env python3
"""Pick N airports with proportional representation: each continent gets a share of
the slots proportional to its number of large+medium airports, and inside a continent
each country gets a share proportional to its count (at least one per country).
Within a country the best airports come first: large before medium, then
International in the name, then more runways, then the longer runway.

Input : `amdbgen list --all --type large,medium --csv all.csv` (needs the continent column)
Output: CSV in the same layout as airports-to-build.csv (priority column = continent rank)

Usage: python tools/make_proportional_list.py all.csv out.csv --total 3000 [--exclude AF,AN]
"""
import csv, re, sys
from collections import defaultdict

args = sys.argv[1:]
src, dst = args[0], args[1]
total = int(args[args.index("--total") + 1]) if "--total" in args else 3000
excluded = set(args[args.index("--exclude") + 1].upper().split(",")) if "--exclude" in args else set()

INTL = re.compile(r"\b(international|intl|internacional|internationale|internazionale)\b", re.I)
rows = [r for r in csv.DictReader(open(src, encoding="utf-8")) if r.get("continent") and r["continent"].upper() not in excluded]
for r in rows:
    r["runways"] = int(r.get("runways") or 0)
    r["longest_ft"] = float(r.get("longest_ft") or 0)

def rank(r):
    return (r["kind"] != "large_airport", not INTL.search(r["name"] or ""), -r["runways"], -r["longest_ft"], r["icao"])

def share(counts, slots, minimum=0):
    """Largest-remainder apportionment of `slots` over {key: count}, with a floor."""
    total_count = sum(counts.values())
    if total_count == 0:
        return {}
    exact = {k: slots * c / total_count for k, c in counts.items()}
    got = {k: max(minimum, int(exact[k])) for k in counts}
    got = {k: min(got[k], counts[k]) for k in counts}
    # hand out the remainder by largest fractional part, never beyond what a key has
    while sum(got.values()) < min(slots, total_count):
        k = max((k for k in counts if got[k] < counts[k]), key=lambda k: exact[k] - got[k])
        got[k] += 1
    # if the minimum pushed us over, trim from the keys with the biggest overshoot
    while sum(got.values()) > slots:
        k = max((k for k in counts if got[k] > minimum), key=lambda k: got[k] - exact[k])
        got[k] -= 1
    return got

by_cont = defaultdict(list)
for r in rows:
    by_cont[r["continent"].upper()].append(r)
cont_slots = share({c: len(rs) for c, rs in by_cont.items()}, total)

out = []
summary = []
for cont in sorted(by_cont, key=lambda c: -cont_slots.get(c, 0)):
    by_country = defaultdict(list)
    for r in by_cont[cont]:
        by_country[r["country"]].append(r)
    c_slots = share({k: len(v) for k, v in by_country.items()}, cont_slots.get(cont, 0), minimum=1)
    picked = []
    for country, rs in by_country.items():
        rs.sort(key=rank)
        picked.extend(rs[: c_slots.get(country, 0)])
    picked.sort(key=lambda r: (r["country"], rank(r)))
    out.append((cont, picked))
    summary.append(f"{cont}: {len(picked)} of {len(by_cont[cont])} across {len(by_country)} countries")

with open(dst, "w", newline="", encoding="utf-8") as fh:
    w = csv.writer(fh)
    w.writerow(["priority", "icao", "iata", "name", "city", "country", "region", "continent", "kind", "runways", "longest_ft", "why"])
    for pri, (cont, picked) in enumerate(out, start=1):
        for r in picked:
            w.writerow([pri, r["icao"], r["iata"], r["name"], r["city"], r["country"], r.get("region", ""), cont, r["kind"], r["runways"], int(r["longest_ft"]), f"share of {cont}/{r['country']}"])
n = sum(len(p) for _, p in out)
print(f"{n} airports; " + "; ".join(summary) + (f"; excluded {','.join(sorted(excluded))}" if excluded else ""))
