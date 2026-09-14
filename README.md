# Free Airport Mapping DB

[![CI](https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/actions/workflows/ci.yml/badge.svg)](https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?logo=rust)](https://www.rust-lang.org)
[![DO-272 layers](https://img.shields.io/badge/DO--272%20layers-45-success)](#output)
[![Navigraph AMDB API](https://img.shields.io/badge/API-Navigraph%20AMDB%20compatible-8A2BE2)](#aircraft-bridge-amdb-bridge)
[![MSFS](https://img.shields.io/badge/MSFS-2020%20%7C%202024-informational)](#aircraft-bridge-amdb-bridge)
[![Data](https://img.shields.io/badge/sources-X--Plane%20Gateway%20%2B%20OpenStreetMap-lightgrey)](#sources)

A free, worldwide **Airport Mapping Database (AMDB)** for flight simulation: every
DO-272 / AMXM layer for any airport, built from open sources, served to aircraft in
exactly the shape their OANS / ANF / BTV code already expects from Navigraph.

Two binaries:

- **`amdbgen`** builds the data: 45 layers per airport as GeoJSON and Geobuf PBF.
- **`amdb-bridge`** serves it through the Navigraph AMDB API surface so aircraft like
  the FlyByWire A380X show it without any code changes.

![amdbgen building Frankfurt](docs/cli-a3357c7.svg)

## Quick start

```
cargo build --release
target\release\amdbgen build EDDF KJFK                    # any ICAO, worldwide
target\release\amdbgen build --simbrief YOUR_NAME         # your latest SimBrief OFP airports
target\release\amdbgen build --country DE --type large --chart   # a batch, with PDF charts
target\release\amdbgen chart EDDF --open                  # Jeppesen-style airport diagram (PDF)
target\release\amdb-bridge serve                          # feed the aircraft (see below)
```

## Sources

All free, no keys, nothing to install.

| Source | Used for | Licence |
|---|---|---|
| X-Plane Scenery Gateway | Runways, pavement, painted lines, stands, taxi routing network, signs, lights, frequencies | free |
| OpenStreetMap (map API, tiled) | Terminals, buildings, towers, fences, roads, water, construction, deicing | ODbL |
| OurAirports | Worldwide airport index | public domain |
| FAA NASR (US only, automatic) | Declared distances, stopways, arresting systems, LAHSO | public domain |
| Local X-Plane install (auto-detected) | Fallback for airports the Gateway does not have: Custom Scenery first, then Global Airports; `--xplane-dir` to point elsewhere, `--aptdat` for one file | yours |
| `overrides/<ICAO>/<layer>.geojson` | Anything to add or replace per airport (hotspots, blind spots) | yours |

The two index files are cached under `%LOCALAPPDATA%\amdbgen\index` and refreshed
daily. Nothing else is cached unless you pass `--cache DIR`.

## Output

```
out/
  index.json               every airport built
  codes.json               legend for the numeric attribute codes
  EDDF/
    manifest.json          ARP, elevation, per-layer counts, sources, empty reasons
    runwayelement.geojson  one FeatureCollection per layer ...
    runwayelement.pbf      ... and the same as Geobuf
    ...                    45 layers
```

Properties use the DO-272 names (`idarpt`, `idrwy`, `idthr`, `idlin`, `idstd`,
`surftype`, `catstop`, `tora/toda/asda/lda`, ...). Layers with no free worldwide
source (hotspot, ATC blind spot, survey points) are written empty with the reason in
the manifest and filled from `overrides/`.

### Selecting airports

Positional ICAO codes, `--from-file list.txt`, `--simbrief NAME`, or any mix of:

| Flag | Picks |
|---|---|
| `--country DE,AT,CH` | ISO countries |
| `--region VI` / `--icao-prefix ED` | aptmeta region / ICAO prefix |
| `--near 48.86,2.35 --within 150` | within a radius (km) of a point |
| `--bbox 5.9,47.3,15.0,55.1` | inside a box (west,south,east,north) |
| `--type large,medium` | OurAirports kinds: large, medium, small, heliport, seaplane, closed |
| `--min-runway-ft 8000` | at least one open runway that long |
| `--iata CDG,ORY` / `--search "de gaulle"` | by IATA, or name/city text |
| `--exclude LFPG,ET` | drop codes or prefixes |
| `--limit 50 --offset 100` | page through big selections |
| `--all` | everything in the index |

`amdbgen list ...` shows what a selection resolves to, `amdbgen info LFPG` what the
index knows (runways, Gateway scenery), `amdbgen search heathrow` finds codes.
`amdbgen list ... --csv file.csv` exports the selection with names, kinds and runway
counts; edit it in a spreadsheet and feed it back with `--from-file file.csv` (both
tools take a CSV with an `icao` column or a plain list, and build in file order, so
the file is a priority list). Ready-made lists are in `lists/`:
`airports-to-build.csv` (every large and medium airport, 5,075, in priority order:
India first, then the USA, then the rest by continent) and `three-plus-runways.csv`
(365). `tools/make_build_list.py` without `--all` makes the shorter 2,572-airport list
(large, International-named or 2+ runway airports plus one per country and state).
`serve --from-file lists\airports-to-build.csv` serves and builds the list at once.
`tools/make_build_list.py` regenerates it from `all-large-medium.csv`.

### Batch control

`--skip-existing` (don't rebuild what's there), `--retry-failed` (from index.json),
`--dry-run`, `--clean`, `--chunk 20` (bounded memory / API load), `--fail-fast`,
`--chart` / `--viewer` / `--zip` (per airport), `--report batch.json`, `-j 4`.
Also: `amdbgen stats [ICAO...]`, `amdbgen zip --all`, `amdbgen clean EDDF`,
`amdbgen layers`, `amdbgen codes`, `amdbgen validate out/EDDF`.

Output flags: `--profile full|map`, `--layers a,b,c`, `--format geojson,pbf`,
`--projection wgs84|metres`, `--osm osmapi|overpass|off`, `--faa off`, `-v`.

## Aircraft bridge (`amdb-bridge`)

Any aircraft that talks to `amdb.api.navigraph.com` gets this data instead, with the
same features it has with Navigraph (map, labels, BTV, FMS runway highlight), because
the aircraft code is untouched: only the address changes.

```
amdb-bridge serve                           # redirect + serve; keep it running while the sim is up
amdb-bridge serve --bulk asia               # ...and build a whole region in the background
amdb-bridge prefetch EDDF KJFK              # or: --simbrief YOUR_NAME
amdb-bridge prefetch --bulk DE,AT,CH --min-runways 2 --type large,medium
amdb-bridge status                          # redirect / certificate / storage / detected aircraft
amdb-bridge setup                           # change the storage answers given at first run
amdb-bridge cleanup                         # remove redirect + certificate
amdb-bridge autostart                       # start with the sim (exe.xml)
```

`--bulk` takes continents (africa, antarctica, asia, europe, north-america, oceania,
south-america), ISO countries, or `all`, comma separated; `--type` (default
large,medium), `--min-runways` (default 1) and `--min-runway-ft` narrow it down, and
`-j N` sets how many airports are fetched from OpenStreetMap at once (default 6; the
main speed lever), airports already built are skipped unless `--rebuild`, `--discard-downloads` deletes
each airport's source downloads once it is built, and every run ends with
`bulk-status.csv` next to the airports folder (built / failed / skipped per airport,
sources used, feature count, error). `amdbgen build --continent europe --min-runways 2`
does the same outside the bridge. Source order per airport: Scenery Gateway, then the
local X-Plane install, then OpenStreetMap alone; with no runway anywhere the airport
is written with only its reference point and the status CSV says so.

The first `serve` asks three questions: keep generated airports and downloads on
disk, where, and up to how much space (oldest airports are dropped first). Answers
live in `%LOCALAPPDATA%\amdb-bridge\config.json`; `--no-cache` keeps nothing for one
run and `--out DIR` uses your own folder with no limit.

`serve` asks for administrator rights, points `amdb.api.navigraph.com` at your PC
through the hosts file, answers over HTTPS with a locally generated certificate it
registers as trusted, and removes the redirect when it stops. The API and response
schema follow Navigraph's own SDK (`@navigraph/amdb`) 1:1: `/v1/cycle`,
`/v1/search?q=`, `/v1/{ICAO}?include=&exclude=&projection=&precision=`,
`/v1/{ICAO}/{layer}`, all 36 Navigraph layers with their exact property sets and
enum values. No Navigraph account is needed. `patch` / `unpatch` are a no-admin
alternative that rewrites the aircraft bundles instead.

Aircraft: FlyByWire A380X (OANS + BTV, tested), FlyByWire A32NX development builds,
iniBuilds A350 (its EFB only hands the OANS gauge a token with a Navigraph
subscription, so `serve` rewrites that one handler; backup kept, `unpatch` restores,
`--no-patch` skips), and the GM5 A220 Airport Moving Map for the Synaptic A220
(`serve` adds a token fallback and lets it find the airport through the bridge's
`/v1/nearest`, since the sim-side search returns nothing under MSFS 2020; on 2020 the
package folder must sort after `synaptic-aircraft-a220`, e.g. `zzz-gm5-a220-amm`,
which `tools/port_a220_amm.py` does for you).

## X-Plane 12

An A380-style airport moving map in a floating window, drawn by a FlyWithLua script
from the same data. Needs [FlyWithLua NG+](https://forums.x-plane.org/index.php?/files/file/82888-flywithlua-ng-next-generation-plus-edition-for-x-plane-12-win-lin-mac/).

```
amdbgen xplane --all --dir out --install     # data for every built airport + install the script
```

In the sim: Plugins > FlyWithLua > FlyWithLua Macros > **AMDB OANS**, or bind a key to
`amdb/oans/toggle`. It loads the nearest airport by itself; `+` and `-` change the
range (0.25 to 4 NM), and ARC / PLAN switches between heading-up and north-up.

`xplane` writes `<ICAO>/oans.lua` (integer metres from the reference point, polygons
already triangulated, bucketed into 300 m tiles) and an `index.lua`, so the script only
draws what is on screen. Run it again after building more airports.

## Charts and previews

```
amdbgen chart EDDF --open        # Jeppesen-style airport diagram, vector PDF (out/EDDF/chart.pdf)
amdbgen view EDDF --open         # OANS-style moving-map page (out/EDDF/viewer.html)
```

The PDF is drawn from the layers: runways with designators and dimensions, taxiway
letters, aprons, terminals, holding positions, hotspots, stands, ARP, tower, runway
table, frequencies, scale bar; portrait or landscape to fit the field.

## Layout

- `src/model` layer registry, code lists, feature container
- `src/geom` local metre projection, Bézier tessellation, buffers, boolean ops
- `src/sources` xplane (gateway, local, apt.dat), osm (map API, Overpass, tags), index, faa, simbrief, overrides
- `src/build` conflation and derivation: runways, markings, pavement, lines, stands, ASRN, structures, signs
- `src/output` GeoJSON, Geobuf, manifest, PDF chart, HTML preview, X-Plane Lua data
- `tools/xplane` the FlyWithLua moving-map script
- `src/bridge` server, Navigraph-schema compat, hosts redirect, TLS, patcher, settings
- `src/pipeline.rs` fetch, merge, build, validate, write

## Licence

MIT. Generated data derives from OpenStreetMap (ODbL) and the X-Plane Scenery
Gateway; it is for simulation only and not for real-world navigation.
