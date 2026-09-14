# Changelog

## 0.3.0 (2026-09-14)

- An airport moving map for the Synaptic A220 in MSFS, as our own package rather than a
  patch of someone else's: `packages/msfs-a220-amm`, installed by
  `python tools/build_a220_amm.py`. It draws 22 layers in the aircraft's own palette,
  including the runway markings, shoulders, service roads and stand areas, with runway
  designators boxed and turned along the runway, stand numbers that thin out with range,
  and hotspots outlined. It reads straight from a local `amdb-bridge` over HTTP, so it
  needs no Navigraph account, no hosts-file redirect, no certificate and no administrator
  rights. MSFS 2020 and 2024 both work from the one folder.
- Fixed: `patch --community DIR` and `unpatch --community DIR` also acted on every other
  Community folder found on the machine, because the named folder was added to the
  detected ones instead of replacing them. Naming a folder now means only that folder.

## 0.2.0 (2026-09-14)

- X-Plane 12: an A380-style airport moving map as a FlyWithLua script that fetches from
  the bridge, which builds the nearest airport on demand (`amdb-bridge serve --xplane`
  installs the script and serves the `/xp/` route). `amdbgen xplane` also writes the same
  compact Lua data (triangulated, simplified, tiled) as files for offline use.
- X-Plane moving map, closer to the Airbus depiction: the shoulder is laid down before the
  pavement so it reads as a band outside it rather than eating the edge; labels are
  measured with real glyph metrics and collision-culled in priority order instead of
  overprinting one another; ARC gains a compass scale and a broken half-range arc; the
  ownship is a swept-wing symbol; and a readout strip carries mode, range, heading and
  airport, so nothing is printed straight onto the map.
- FAA open airport-mapping layers for US airports: hotspots with their published caution
  text, and pavement when no scenery exists.
- OSM: every source at once. The map API and each Overpass endpoint form a pool worked by
  a shared queue, with regional instances for their own countries and a per-source
  deadline (about 4x faster bulk builds).
- Fixed: the X-Plane moving map scattered white triangles across the window at big
  airports. ImGui indexes one draw list with 16-bit integers, so a frame over 65,535
  vertices wrapped around; a full Kennedy frame needed 110,352. Frames now cull to what is
  really on screen, shed detail in steps as the range widens (the wide view is the Airbus
  depiction: white runways and a grey taxiway network), and stay inside a vertex budget.
- Fixed: runways and taxiways written as several rings in one polygon were ear-cut as if
  the extra rings were holes, spanning triangles across the airport.
- Fixed: OSM names fetched through the map API kept XML entities ("E/F &amp; Link").

## 0.1.0 (2026-09-13)

- `amdbgen`: builds all 45 DO-272 / AMXM airport-mapping layers for any airport from
  the X-Plane Scenery Gateway, OpenStreetMap, OurAirports and (US only) FAA NASR, as
  GeoJSON and Geobuf PBF, in WGS84 or ARP-centred metres.
- `amdb-bridge`: serves the data through the Navigraph AMDB API surface (1:1 with the
  official `@navigraph/amdb` SDK schema) so aircraft OANS / ANF / BTV work unchanged;
  hosts-file redirect with local TLS, cleaned up on exit; bundle patcher as an
  alternative.
- SimBrief import (`--simbrief`) to build or prefetch a flight's airports.
- OANS-style HTML preview (`amdbgen view`, `tools/`).
- Jeppesen-style airport diagram as a vector PDF (`amdbgen chart`, `--chart`), portrait
  or landscape to fit the field.
- Batch CLI: selection by country, region, prefix, radius, box, type, runway length,
  IATA, name search, exclusions and paging; `--skip-existing`, `--retry-failed`,
  `--dry-run`, `--clean`, `--chunk`, `--fail-fast`, `--zip`, `--report`; `list`, `info`,
  `search`, `view`, `stats`, `zip`, `clean`, `layers`, `codes`.
- Bulk builds: `amdb-bridge serve --bulk asia` (background) / `prefetch --bulk`, by
  continent, country or `all`, filtered by `--type`, `--min-runways`, `--min-runway-ft`;
  `amdbgen --continent`, `--min-runways`; `--from-file` takes a CSV with an `icao`
  column and keeps its order; `amdbgen list --csv` exports selections; ready-made
  priority lists in `lists/`.
- Source order is now Gateway first, then the local X-Plane install (auto-detected,
  Custom Scenery before Global Airports, indexed once) for airports the Gateway lacks.
- `amdb-bridge`: patches the GM5 A220 moving map automatically; first-run storage setup (cache on/off, folder, size limit with oldest-
  first pruning; `setup`, `--no-cache`); request logging; CORS preflight for clients
  that send an Authorization header; EPSG:4326 default projection like Navigraph;
  `/v1/nearest` for moving maps without a sim-side airport search; iniBuilds A350 EFB
  token handler patched automatically so its OANS works without a subscription;
  WASM-gauge aircraft detection in `status`.
- Data: runway designators zero-padded, construction areas never cover live pavement,
  building names kept only for terminals, towers and hangars, exit lines extended to
  the first holding position, DO-272 building capture rule, no synthetic shoulders by
  default.
