# Changelog

## 0.5.0 (2026-09-22)

- **Approach charts.** `amdbgen approach-chart <ICAO>` draws a full approach chart as a
  PDF, styled like an airline chart and branded "AMDB V1": a header, a briefing strip
  (final course, touchdown zone elevation, airport elevation, minimum safe altitude
  within 25 NM, and the decision altitude), a plan view with terrain shading, the
  airport drawn from our own build, the procedure's fixes at their real positions,
  obstacles, the missed approach, a descent profile, and a minima band. `--approach 27L`
  picks a particular approach (`27L-2` for the second to the same runway), `--star
  BIG1A` draws an arrival feeding it, `--kind ils|rnav|loc|circling` sets which system
  minimum applies, and `--list` prints every approach and arrival an airport has, with
  the names to pass back in. It is not for real-world navigation and says so on the
  page.
- **Obstacles**, a new data source: the FAA's Digital Obstacle File (surveyed, United
  States) and OpenStreetMap masts, towers, chimneys and wind turbines worldwide where
  they carry a height tag. Both are free and public and need no account. OpenStreetMap
  gives heights above the ground, so the terrain model converts them to height above sea
  level before an approach chart can weigh them against the approach.
- **An estimated minimum** on every approach chart, worked out from the airport's own
  terrain and obstacle data rather than copied from anywhere: the system minimum for the
  approach type, measured above the touchdown zone elevation (the highest point of the
  first 3,000 ft of the runway, read from the terrain model, not the airport's own
  elevation). Above that floor, an approach with a glidepath is raised only where ground
  or an obstacle breaks through a surface rising 102:1 from the threshold, and only by
  as much as it breaks through; without a glidepath, everything in the segment has to be
  cleared by the required margin. The ground is assessed along the procedure's actual
  path through its fixes rather than a straight box out from the runway, which matters
  in a valley. The chart says which of the three things set the number.
- **The procedure's own minimum**, where it codes one, instead of an estimate. An
  approach without a glidepath ends at a missed approach point, and the altitude on that
  leg is the altitude it descends to; Madeira's VOR/DME to runway 05 codes 940 ft, which
  is what its published chart says. The chart says which it is showing.
- **More of what a chart carries.** DME arcs are drawn as arcs, holding patterns as
  racetracks turning the way the procedure says, fixes are marked as the initial,
  intermediate and final approach fixes and named as a chart names them ("7 DME FUN"),
  and the radio frequencies run across the page under the briefing strip. Fixes given
  only as a radial and a distance from a beacon are placed from the beacon, with the
  magnetic variation measured from the fixes that do carry a position — which is what
  makes an approach outside the United States drawable at all.
- **Circling minima** are worked out over a circle about the aerodrome, by aircraft
  category, rather than down the approach.
- `amdbgen minima-audit --truth <csv>` measures those estimates against a table of
  published minima, split by kind and by whether the chart sits on its system minimum or
  was pushed higher, and separately how close the estimated touchdown zone elevations
  are.
- **A chart's furniture**: terrain tinted by elevation with a key, the minimum safe
  altitude by quadrant rather than one figure for the whole circle, a compass rose
  carrying the measured magnetic variation, degrees and minutes along the edges, and an
  inset of the airport at its own scale where the approach is too long to show both.
- **`amdbgen approach-charts`** draws a list of airports in one run, reading the
  navigation data, the obstacle file, the runway file and the beacons once between all of
  them. About a second a chart the first time an area is drawn and a tenth of that
  afterwards, against six seconds a chart one at a time.
- **Terrain is read at the size needed.** The elevation files carry reduced copies of
  themselves; reading the smallest one still fine enough for the job, rather than the
  largest for everything, is what makes the safe-altitude ring affordable.
- Fixed: the second altitude on a leg was read from the wrong place in the navigation
  data, which put a constant 1,171 ft on charts as though it were a constraint.

## 0.4.2 (2026-09-19)

- **Linux.** `amdb-bridge` and `amdbgen` run on any 64-bit Linux (a static build). They
  find Microsoft Flight Simulator 2020 and 2024 under Steam's Proton, in every Steam
  library including Flatpak and Snap Steam, translating the Wine paths in `UserCfg.opt`,
  and X-Plane 12 through `~/.x-plane` or Steam. `sudo amdb-bridge navigraph on` sets up
  the iniBuilds A350 and FlyByWire A380X once: `/etc/hosts`, the system certificate
  store (which Wine and Proton read), and permission to use port 443 without root. The
  desktop app stays Windows-only.
- **Bulk builds are much faster.** Each airport is built the moment its own download
  finishes instead of waiting for the slowest one in its group, and downloads saved from
  either OpenStreetMap source are reused, so a restart no longer downloads them again.
- **Live progress page** at http://127.0.0.1:8771 while `prefetch` builds a list:
  percentage, airports per hour, time left, the latest airports and any failures.
- `amdb-bridge navigraph on|off` on Windows too, and `serve` uses that setup without
  asking for administrator rights.

## 0.4.1 (2026-09-19)

- Fixed: setting up the A350/A380X could start copies of AMDB Bridge without end, which
  then could not be closed and stopped Windows shutting down. It happened when security
  software or a read-only flag protects the hosts file: the administrator copy took the
  blocked file to mean it had no administrator rights and relaunched itself again. Rights
  are now read from Windows directly, a relaunched copy never relaunches, and a failure
  is reported once in plain words. Uninstalling had the same flaw and is fixed too.
- A read-only flag on the hosts file is cleared before writing.
- **Aircraft report**: one button saves a report on the installed aircraft (where each is
  installed, and short excerpts of its code that mention Navigraph or the map API) and
  a copy of the log to your Downloads folder, for troubleshooting an aircraft that gets
  no maps. Also `amdb-bridge collect`.
- **Save log** copies the log to Downloads. The log records each new client's first
  request in full, and any request it does not recognise.

## 0.4.0 (2026-09-16)

- **AMDB Bridge, a desktop app, and a Windows installer.** No more terminal: one Start
  button, a list of the simulators and aircraft found on the computer with their state,
  install/update/remove for the A220 map, and the options (start with Windows, start
  with the simulator, keep airports on disk, where and how much). It stays in the
  notification area while serving, keeps a log in `%LOCALAPPDATA%mdb-bridgeridge.log`,
  and says in plain words why serving could not start.
- The installer (`AMDB-Bridge-Setup-0.4.0.exe`) installs per user without administrator
  rights, puts the A220 map into every MSFS 2020 and 2024 found, and can set up the
  iniBuilds A350 and FlyByWire A380X in one step. Uninstalling undoes all of it.
- A350/A380X support is now set up once (one administrator prompt) instead of on every
  start: the address redirect and certificate stay in place while the option is on, and
  the app serves those aircraft as a normal user.
- A220 map: runway designators stay level with the screen at every heading instead of
  turning with the runway, and the aircraft's own compass rose, FMS MAP flag, NO FLIGHT
  PLAN message and TCAS panel are hidden while the airport map is drawing.
- When the port is already taken (usually a second copy running), the app says so at
  once, and `amdb-bridge serve` says so in plain words instead of a socket error.

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
