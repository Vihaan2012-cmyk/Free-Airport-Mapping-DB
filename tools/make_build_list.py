#!/usr/bin/env python3
"""Build the priority airport list for bulk generation.

Input : a full export from `amdbgen list --all --type large,medium --csv all.csv`
Output: airports-to-build.csv, ordered by priority, with a `why` column.

Included:
  * every large airport;
  * every large/medium airport whose name says International / Intl;
  * every large/medium airport with two or more open runways;
  * at least one airport per country (the one with the longest runway if none of the
    above matched), so small states are not skipped;
  * at least one per Indian state / union territory and per US state.
Order: India first, then the United States, then everyone else by continent and country.

Usage: python tools/make_build_list.py all.csv airports-to-build.csv
"""
import csv, re, sys
from collections import defaultdict

src, dst = sys.argv[1], sys.argv[2]
rows = list(csv.DictReader(open(src, encoding="utf-8")))
for r in rows:
    r["runways"] = int(r.get("runways") or 0)
    r["longest_ft"] = float(r.get("longest_ft") or 0)

INTL = re.compile(r"\b(international|intl|internacional|internationale|internazionale)\b", re.I)
why = defaultdict(set)
for r in rows:
    if r["kind"] == "large_airport":
        why[r["icao"]].add("large")
    if INTL.search(r["name"] or ""):
        why[r["icao"]].add("international")
    if r["runways"] >= 2:
        why[r["icao"]].add("2+ runways")

by_icao = {r["icao"]: r for r in rows}

# Every country gets at least one airport.
by_country = defaultdict(list)
for r in rows:
    by_country[r["country"]].append(r)
for country, rs in by_country.items():
    if not any(r["icao"] in why for r in rs):
        best = max(rs, key=lambda r: (r["longest_ft"], r["runways"]))
        why[best["icao"]].add(f"main airport of {country}")

# Every state of the priority countries (India, USA) gets at least one airport.
STATE_COUNTRIES = ("IN", "US")
by_state = defaultdict(list)
for r in rows:
    if r["country"] in STATE_COUNTRIES and r.get("region"):
        by_state[r["region"]].append(r)
for state, rs in by_state.items():
    if not any(r["icao"] in why for r in rs):
        best = max(rs, key=lambda r: (bool(INTL.search(r["name"] or "")), r["longest_ft"], r["runways"]))
        why[best["icao"]].add(f"main airport of {state}")

def priority(r):
    if r["country"] == "IN":
        return 0
    if r["country"] == "US":
        return 1
    return 2

out = [by_icao[i] for i in why]
out.sort(key=lambda r: (priority(r), r["continent"], r["country"], -r["runways"], r["icao"]))
with open(dst, "w", newline="", encoding="utf-8") as fh:
    w = csv.writer(fh)
    w.writerow(["priority", "icao", "iata", "name", "city", "country", "region", "continent", "kind", "runways", "longest_ft", "why"])
    for r in out:
        w.writerow([priority(r) + 1, r["icao"], r["iata"], r["name"], r["city"], r["country"], r.get("region", ""), r["continent"], r["kind"], r["runways"], int(r["longest_ft"]), " + ".join(sorted(why[r["icao"]]))])

n_in = sum(1 for r in out if r["country"] == "IN")
n_us = sum(1 for r in out if r["country"] == "US")
states = {c: sum(1 for s in by_state if s.startswith(c + "-")) for c in STATE_COUNTRIES}
eu = {r["country"] for r in out if r["continent"] == "EU"}
print(f"{len(out)} airports: {n_in} India ({states['IN']} states/UTs), {n_us} USA ({states['US']} states), {len(out) - n_in - n_us} elsewhere; {len(by_country)} countries, {len(eu)} European countries covered")
