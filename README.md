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

Three programs:

- **AMDB Bridge** is the desktop app: install it, press Start, fly. It serves maps to
  the Synaptic A220, iniBuilds A350, FlyByWire A380X and X-Plane 12.
- **`amdbgen`** builds the data: 45 layers per airport as GeoJSON and Geobuf PBF.
- **`amdb-bridge`** is the command-line server behind the app, for scripting and CI.

![amdbgen building Frankfurt](docs/cli-a3357c7.svg)

## Install (Windows)

1. Download **`AMDB-Bridge-Setup-<version>.exe`** from the
   [latest release](https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB/releases/latest)
   and run it. It installs for your user only, so it needs no administrator rights,
   except for the optional A350/A380X step described below.
2. Leave the setup options ticked:
   - **A220 moving map** copies the map into every Microsoft Flight Simulator 2020 and
     2024 it finds. If you had the GM5 A220 map, it is set aside and put back on uninstall.
   - **A350 and A380X** points Navigraph's map server address at your computer and
     trusts a local certificate, so those aircraft load maps from AMDB Bridge. Windows
     asks for administrator permission once.
3. AMDB Bridge opens and starts serving. Load your aircraft. The first visit to an
   airport takes 20-40 seconds while it is built; after that it is instant.

Closing the window keeps AMDB Bridge running in the notification area. Right-click its
icon to exit. The window also installs, updates or removes the A220 map, starts with
Windows or with the simulator, and shows what it is doing.

Uninstalling removes everything it changed outside its own folder: the A220 map (and
brings back any map it set aside), the start-up entries, the A350 patch, the address
redirect and the certificate. It asks before deleting the airports it built.

To build the installer yourself: `python tools/make_installer.py` (needs Rust and
Inno Setup 6).

## Linux

Download **`amdb-bridge-<version>-linux-x64.tar.gz`**; it runs on any 64-bit distro.

```
tar xzf amdb-bridge-*-linux-x64.tar.gz && cd amdb-bridge-*-linux-x64
sudo ./amdb-bridge navigraph on    # once, only for the iniBuilds A350 / FlyByWire A380X
./amdb-bridge serve --xplane       # every time you fly
```

It finds Microsoft Flight Simulator under Steam's Proton and X-Plane 12 by itself. For
the A220 map, copy `msfs/zzz-amdb-a220-amm` into your MSFS Community folder
(`amdb-bridge status` prints where that is). Run `navigraph on` again after updating,
since a new binary loses its permission to use port 443.

## Quick start (command line)

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
| FAA Digital Obstacle File (US only, automatic) | Surveyed obstacles for approach charts: masts, towers, chimneys, wind turbines | public domain |
| OpenStreetMap masts, towers, chimneys and wind turbines (worldwide, tagged height only) | Obstacles for approach charts outside the US | ODbL |
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

## Airport moving map for the A220 (MSFS)

An airport moving map for the Synaptic A220, as its own Community package rather than a
patch of anyone else's add-on. It reads straight from a running `amdb-bridge` over HTTP,
so it needs no Navigraph account, no hosts-file redirect, no certificate and no
administrator rights. MSFS 2020 and 2024 both work from the one folder.

```
python tools/build_a220_amm.py            # install into every sim found
python tools/build_a220_amm.py --uninstall  # remove it, restoring anything it displaced
amdb-bridge serve --no-hosts --no-patch   # then leave this running while you fly
```

Releases ship the same package with an `install.bat` for people who would rather not
clone the repository. It draws 22 layers in the aircraft's own palette — runway markings,
shoulders, service roads, stand areas, guidance lines, holding positions, structures and
hotspots — with runway designators boxed and turned along the runway, and stand numbers
that thin out as the range widens. The map appears by itself once you are on the ground;
`L:AMDB_AMM_VISIBLE` and `L:AMDB_AMM_RANGE` are bindable if you want manual control.

Only one package can override the A220's display units, so remove any other A220 moving
map first. `install.bat` does that for you and keeps the displaced copy in `_disabled`.

## X-Plane 12

An A380-style airport moving map in a floating window, drawn by a FlyWithLua script
that fetches from the running bridge (which builds the airport you are at on demand,
exactly like it does for MSFS). Needs [FlyWithLua NG+](https://forums.x-plane.org/index.php?/files/file/82888-flywithlua-ng-next-generation-plus-edition-for-x-plane-12-win-lin-mac/).

```
amdb-bridge serve --xplane          # installs the script and serves X-Plane on demand
```

Then in the sim: Plugins > FlyWithLua > FlyWithLua Macros > **AMDB OANS**, or bind a key
to `amdb/oans/toggle`. It shows the nearest airport by itself, building it if needed;
`+` and `-` change the range (0.25 to 4 NM), and ARC / PLAN switches between heading-up
and north-up. Keep `amdb-bridge serve --xplane` running while you fly.

The script asks the bridge's `/xp/nearest?lat&lon` route, which returns the airport as a
Lua chunk (integer metres from the reference point, polygons already triangulated,
bucketed into 300 m tiles) or `{building="ICAO"}` while a background build runs, so the
sim never stalls. `amdbgen xplane --all --dir out` also writes the same data as files
for offline use.

## Charts and previews

```
amdbgen chart EDDF --open        # Jeppesen-style airport diagram, vector PDF (out/EDDF/chart.pdf)
amdbgen view EDDF --open         # OANS-style moving-map page (out/EDDF/viewer.html)
```

The PDF is drawn from the layers: runways with designators and dimensions, taxiway
letters, aprons, terminals, holding positions, hotspots, stands, ARP, tower, runway
table, frequencies, scale bar; portrait or landscape to fit the field.

### Approach charts

```
amdbgen approach-chart EDDF --approach 27L --open          # ILS to 27L, PDF
amdbgen approach-chart EDDF --approach 27L --star BIG1A    # ...with an arrival feeding it
amdbgen approach-chart EDDF --list                         # every approach and arrival EDDF has
amdbgen approach-charts --list airports.txt --out-dir out  # a whole list of airports at once
```

A full approach chart, styled like an airline chart and branded "AMDB V1". It needs
Microsoft Flight Simulator installed, since the procedure and its fixes are read from
its own navigation data (never redistributed). The page has a header, a briefing strip
(final course, touchdown zone elevation, airport elevation, minimum safe altitude within
25 NM, and the decision altitude), the radio frequencies, a plan view with terrain
shading, the airport drawn from our own build, a descent profile, and a minima band.

On the plan view: the procedure's fixes at their real positions, marked as the initial,
intermediate and final approach fixes and named the way a chart names them ("7 DME FUN");
DME arcs flown as arcs rather than as straight lines; holding patterns as racetracks,
turning the way the procedure says; obstacles; and the missed approach. A fix that the
data gives only as a radial and a distance from a beacon — which is how most approaches
outside the United States are written — is placed from the beacon, with the magnetic
variation measured from the fixes that do carry a position.

Around it: terrain tinted by elevation with a key, a compass rose carrying that measured
variation, degrees and minutes ruled along the edges, the minimum safe altitude by
quadrant, and, where the approach is long enough that the airport would otherwise be a
few millimetres of grey, an inset of the airport at its own scale.

`approach-charts` does a list of airports in one run, which reads the navigation data,
the obstacle file, the runway file and the beacons once between all of them instead of
once each: about a second a chart the first time an area is drawn, and a tenth of that
once its terrain is cached. `--every-runway` draws each runway rather than the airport's
fullest approach, `--jobs` sets how many airports run at once, and `--no-msa` leaves off
the safe-altitude ring, which is the one part that reads terrain far from the airport. Obstacles come from the FAA's Digital
Obstacle File in the United States and from OpenStreetMap everywhere else (see
Sources); OpenStreetMap gives heights above the ground, so the terrain model converts
them to height above sea level before they can be weighed against the approach.

`--approach 27L` picks a particular approach when a runway has more than one
(`27L-2` for the second); `--list` prints every approach and arrival the airport has,
with the exact names to pass back in. `--star BIG1A` draws an arrival feeding the
approach. `--kind ils|rnav|loc|circling` sets which system minimum applies (see below);
left out, an ILS is assumed.

Nothing on the chart is for real-world navigation, and the page says so.

#### How the minimum is worked out

Where the procedure codes a minimum of its own, that is what the chart carries, and it
is not an estimate at all: an approach without a glidepath ends its final segment at a
missed approach point, and the altitude on that last leg is the altitude the approach
descends to. Madeira's VOR/DME approach to runway 05 codes 940 ft, which is exactly what
the published chart says. An approach with a glidepath ends at the runway instead, and
that altitude is the height it crosses the threshold at, so it gives no minimum and one
has to be worked out.

The rest of this section is how that is done. It is not copied from anywhere: it is
worked out from the airport's own terrain and obstacles, the same way a real approach is
designed, in outline.

Every approach type has a system minimum it may never go below, measured above the
touchdown zone elevation: 200 ft for an ILS CAT I, 250 ft for RNAV with vertical
guidance, 300 ft for a non-precision approach, 400 ft circling. The touchdown zone
elevation is not the airport's own elevation; it is the highest point of the first
3,000 ft of the runway, read from the terrain model.

Above that floor, ground and obstacles can push the minimum higher, and how depends on
whether the approach flies a glidepath. With one (ILS, RNAV with vertical guidance),
only what breaks through a surface rising from the threshold at 102:1 matters — anything
below that surface is already cleared by the descent and changes nothing, and anything
that breaks through raises the decision altitude by exactly as much as it breaks
through. Without a glidepath the aircraft levels off for the segment, so everything in
it has to clear by the required margin instead. Either way, the ground is assessed along
the procedure's actual path through its fixes rather than a straight box out from the
runway, because a box is badly wrong in a valley: it takes in the walls either side of a
procedure that is actually threading between them.

How much clearance is kept, and how wide an area it is kept over, depend on how finely
the course can be held: a localiser or an RNAV track keeps 250 ft over an area a mile
either side, a course held on a beacon keeps 300 ft over an area two and a half miles
wide, since it wanders further. Beyond the middle of that area the clearance tapers away
to nothing at the edge, as it does in the rules.

Circling is not flown down the approach at all — the aircraft manoeuvres visually about
the aerodrome — so it is worked out over the arcs swung from every runway end, at a
radius that depends on the aircraft category, with a floor for each category that no
circling minimum goes below.

The safe altitude printed in the corner follows the same idea: the circle within 25 NM is
split into quadrants, each clearing whatever stands in it (and within five miles outside
it) by a thousand feet, or two thousand where the ground is mountainous. Madeira's
western sector comes out at 8,000 ft against a published 8,200, and its eastern at 3,400
against 3,500.

The result is an estimate, and the chart says which of the four things — the procedure's
own coded minimum, the system minimum, terrain, or an obstacle — set it.

#### How close this gets

Measured against 382 published ILS approaches across the United States, read out of the
FAA's own charts: the median estimate is within a foot of the published decision
altitude, and 85 per cent are within fifty feet.

The set splits in two. Where the published chart sits on its system minimum, which is
285 of the 382, the estimate is the published figure: a median error of half a foot, and
95 per cent within fifty feet. Where the published chart was pushed higher, which is the
other 97 and mostly airports ringed by mountains, the estimate comes out too low, by
fifty feet at the median and several hundred at the worst. Those minima are raised for
things this does not model: the climb gradient a missed approach has to make, and what
stands in the missed approach area rather than under the approach itself.

Touchdown zone elevations are exact in the United States because they are surveyed and
published. Everywhere else they come from the terrain model and carry a few feet with
them, which carries straight through to the minimum.

Approaches **without** a glidepath are a different matter. Measured against 346 published
minima read off the same charts — localiser, VOR, NDB, RNAV without vertical guidance,
and circling — the median error is about 60 ft, and it runs low as often as high. Those
minima are designed segment by segment against rules this does not reproduce, so where
the procedure codes no minimum of its own, treat the figure as the right order of
magnitude rather than a number to fly. Where it does code one, the chart simply reports
it and the question does not arise.

The clearances and areas above are the published ones, not values fitted to that set. A
fitted version, trained on half of it and measured on the half it had not seen, came out
five feet better and behaved badly on the kinds with few examples.

#### Checking the estimator

```
amdbgen minima-audit --truth published.csv --out measurements.csv
```

If you have a set of published minima to check the estimator against, `minima-audit`
measures the estimates against them: a CSV of `icao,runway,published_da_ft,
published_hat_ft,published_tdze_ft`, one row per chart. It prints how close the
estimates come, split by whether the published chart sits right on its system minimum
or was pushed higher by terrain or an obstacle, and separately how close the estimated
touchdown zone elevations are to the published ones. `--jobs N` sets how many airports
it works on at once (default 4). No truth set ships with the project; build one from
charts you already have.

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
