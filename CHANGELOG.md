# Changelog

## 1.0.0 (2026-09-23)

The first release that does everything it set out to: a moving map on every airport in
the world, and charts on the tablets, from nothing but free data and what is already on
the machine.

- **Charts on the tablets.** The iniBuilds A350, the PMDG 777 and 737 and the Synaptic
  A220 EFB now list and show charts served by the bridge, with no Navigraph account.
  `amdb-bridge charts on|off|status`, `amdbgen tablet-charts on|off|status` (no
  administrator rights needed) and a *Tablet charts* button in the desktop app turn it on
  and off; the original script is kept inside the patched one and put back exactly.
  Each airport lists its departures, arrivals and approaches, and they are drawn ahead
  of being asked for, so a tablet no longer waits on the first page.
- **Departure and arrival charts.** The simulator's SID and STAR records are now read in
  full — runway transitions, the common route and the enroute transitions, with speed
  limits — and drawn one procedure to a page with MSA, holds, transition altitude,
  frequencies, grid MORA and airspace, as `amdbgen procedure-chart`.
- **Approach charts, closer to what a pilot expects.** Terrain in bands relative to the
  field, the highest peaks marked, DME arcs flown as arcs, step-down bars on the profile,
  straight-in visibility from the EU-OPS table and circling never below straight-in.
  Minimum safe altitude rings were drawn rotated half a turn on every chart; they are
  now the right way round. A beacon sharing its ident with another is taken nearest the
  airport (Kathmandu's KTM was once found in the Arabian Sea).
- **Moving maps answered again.** A change in 0.6 to how tablet requests were told apart
  dropped every moving-map request on the floor; it no longer does.
- **`amdbgen patch-status`** lists every patchable aircraft in every Community folder,
  MSFS 2020 and 2024, and whether it is patched.
- Obstacles fetched from OpenStreetMap are kept for thirty days rather than one, which
  took several minutes off the first chart of a session.

## 0.6.1 (2026-09-23)

- **The GM5 A220 moving map is patched again**, with its author's permission. It was
  taken out in 0.5.1 because the patcher carries a few lines of that add-on's script as a
  search anchor. `serve` and `patch` once more give Gunman5's map a token fallback and
  point its nearest-airport search at the bridge; `status` reports it; `unpatch` puts it
  back. The Synaptic instrument-page patcher for our own map is unchanged beside it.

## 0.6.0 (2026-09-23)

- **Charts can be drawn as pictures.** `amdbgen approach-chart LPMA --approach 05 --png`
  writes a PNG instead of a PDF, at `--scale` pixels to the point (three is about 216 to
  the inch on A4). This is what an electronic flight bag asks for, and it is the thing
  that stood between us and answering one.

  Nothing about the drawing differs. A chart is built out of about two dozen operations —
  move, line, fill, set a grey, write some text — and those are now named rather than
  written straight into a PDF stream, with two places to send them: the stream as before,
  and a bitmap. The PDF a chart comes out as is byte-for-byte what it was, which is the
  proof that the two cannot drift apart: there is only one set of drawing code.

  The letters come from the sans-serif face already installed on the machine — Arial on
  Windows, which carries Helvetica's metrics exactly, so the width tables the layout is
  measured with still hold. Nothing of anyone's is shipped.

- **Approach charts from the desktop app.** Type an airport, press Find procedures, and
  the app lists what the navigation data on this computer holds: how many departures and
  arrivals it has, and every instrument approach, each with the arrivals that feed it.
  Pick one, press Draw chart, and it is written to Downloads and opened.

- **The way in to the approach is drawn on the profile.** A plate's profile carries the
  arrival as well as the final: Madeira's shows the aeroplane leaving the VOR at 4,000 ft,
  running out to the turn and coming back at 3,000 to join the approach. That upper line
  is what makes its profile a wedge rather than one slope.

## 0.5.9 (2026-09-23)

- **Approach charts from the desktop app.** Type an airport, press Find procedures, and
  the app lists what the navigation data on this computer holds: how many departures and
  arrivals it has, and every instrument approach, each with the arrivals that feed it —
  so a crew handed a particular arrival can see which approach it leads to. Pick one,
  press Draw chart, and it is written to Downloads and opened.

- **The way in to the approach is drawn on the profile.** A plate's profile carries the
  arrival as well as the final: Madeira's shows the aeroplane leaving the VOR at 4,000 ft,
  running out to the turn and coming back at 3,000 to join the approach. That upper line
  is what makes its profile a wedge rather than one slope, and it was the largest thing
  ours was missing — not detail, a whole second path. Of the transitions that feed the
  approach, the one drawn is the most direct whose every leg the band can hold; the
  longer ones are entries from an airway and belong on the arrival chart.

## 0.5.8 (2026-09-23)

- **The profile is drawn in the direction the approach is flown.** Heathrow's 09L runs
  east, so its plate puts the runway on the right and the aeroplane arrives from the
  left. Madeira's runs southwest, so its plate puts the runway on the *left* and the
  aeroplane arrives from the right. Ours always drew the runway on the right, whatever
  the approach did, which is why Madeira's profile never looked like its plate however
  much detail went into it. An approach with any westerly component now reads right to
  left, and everything on the band — the ground line, the runway bar, the threshold
  elevations, the distance ruler, the arrow on the course — turns with it.

- The beacon column is only drawn where the approach is actually flown over the station.
  On an ILS it was putting a grey spike through the runway for a VOR that merely
  happened to sit near the field.

## 0.5.7 (2026-09-23)

The profile band.

- **The descent the approach is designed around is drawn against the steps that fly it.**
  A plate shows both: the solid line is what an aeroplane does, the dotted one is the
  path it is meant to stay on the whole way down, and a step-down that dips below the
  dotted line is the thing a crew is looking for. We drew only the steps.

- **Sea is drawn as sea.** The elevation model reads sea level over water, and filled in
  the same grey as a hill that is a flat strip under the aeroplane which looks like a
  fault in the drawing. It is tinted as water, the way the plan view tints it, so a
  reader watches the coast go by.

- **The runway is at the end of the band**, as the heavy bar a plate draws, so the eye
  knows which end is the ground being landed on.

- **The band is sized for the approach.** It reserved twelve hundred feet of headroom
  whatever the approach did, which on one that never climbs above three thousand is a
  third of the band left empty. It now takes what the drawing uses.

- The descent angle is written on the descent rather than under the minimum ruled across
  it, where half of it was being struck through.

## 0.5.6 (2026-09-23)

- **The visual glidepath is found again.** A PAPI is matched to the runway it serves by
  where it stands and which way it points, rather than by the label on it. Madeira's are
  called PAPI-5L, PAPI-5R and PAPI-23L — that suffix is the side of the runway the unit
  sits on, not a runway ident — so comparing it to "05" matched nothing and every chart
  for the field came out with an empty lighting box. This is a fix to the airport build,
  so it reaches the moving map and the airport diagram as well as the approach chart.
  Rebuild an airport to pick it up.

- **The missed approach box says what its fix is found on.** Where there is no second
  altitude to print, the middle cell now carries the beacon, its frequency and the
  radial — FUN 112.20 R-170 — which is how a crew identifies the fix without a map.

## 0.5.5 (2026-09-23)

- **Published holds are drawn.** A hold at an initial approach fix is somewhere to wait
  for the approach, not a leg of it, so nothing in the coded route mentions it — it lives
  in the navigation database's own holding table, against the fix's name. We read that
  table for the missed approach and never asked it about anything else, which is why
  Madeira drew one racetrack where the plate has two. Every fix on the chart is now asked
  about, and each hold is drawn where it is flown, with its inbound course and its
  maximum holding altitude.

- **The circling minima carry a visibility.** A minimum with no visibility beside it is
  half a minimum, and outside the United States we read no published one, so the table
  from PANS-OPS is printed and headed as the standard it is. Where a state's own chart
  was read, that visibility is used and this column does not appear.

- **The missed approach point is given as a distance**: "MAP at D3.6 FUN", which is how a
  crew flying a non-precision approach knows where it ends.

## 0.5.4 (2026-09-23)

- **Tracks are drawn round.** A procedure is coded as fixes joined by straight legs, and
  drawn literally that is a polyline with a sharp angle at every fix — a shape no
  published chart has, because no aircraft flies it. Each turn is now a tangent arc of
  about a mile's radius. That is what turns Madeira's squared-off run out to FUN08, across
  to ABUSU and back down to FUN7 into the racetrack the plate draws over it: the course
  reversal was in our data all along, drawn as three corners.

- **The missed approach is written the way a plate writes it.** It used to read "Track
  137, then direct FUSUL, then hold at FUSUL" from legs that say a good deal more than
  that. It now reads "Track heading 137° to intercept FUN R-170, proceed to FUSUL
  climbing to 3000' and hold" — the radial the heading is flown to meet, the altitude
  climbed to, and the way round the turn goes where the procedure codes one.

- **Circling speeds are circling speeds.** The Max Kts column printed the speed each
  category crosses the threshold at — 90/120/140/165 — where a circling table is headed
  by the speed it may circle at: 100/135/180/205. Every figure in the column was thirty
  knots light.

- **A circling-only approach gives its whole width to the circling minima.** Two thirds
  of the band was ruled for a straight-in that does not exist, so four cells sat empty
  where a plate has the minimum.

## 0.5.3 (2026-09-22)

The plan view, which was the weakest part of the page.

- **The window is framed on what it has to hold.** It used to be sized by how far the
  approach reached and then shifted a fixed share of the way back towards it, which
  works until the approach leaves in one direction and the missed approach in another.
  At Madeira that put the airport hard against the bottom edge with its runway and
  missed approach running off the paper. The window is now the box around everything
  that must be on it, centred on itself, with real room around the airport because that
  is where the runway, the missed approach and the labels for both are drawn.

- **The ways in are drawn as tracks.** A hairline a third of a shade off the sea reads
  as a construction line; the arrival transitions are now at a weight that can be
  followed, and carry an arrowhead saying which way round they are flown.

- **The runway can be seen at any scale.** Twenty-six miles across the paper, a two-mile
  runway is eight millimetres of it and a short one is three, so the mark is stretched
  about its own middle to a length that reads.

- **The highest ground on the page is marked and named**, with the triangle a chart
  marks a summit with.

- **Empty columns are not ruled.** The minima table drew FULL / TDZ-CL out / ALS out
  whether or not a published visibility had been read to put under them, and three
  empty ruled columns say the chart failed to draw rather than that there is nothing to
  say. Outside the United States there is usually nothing to say.

- A fix measured from a beacon is written D12.0 FUN, the way it is read on the
  instrument, rather than 12.0 DME FUN.

## 0.5.2 (2026-09-22)

The profile band and the plan view, read against the published plate they copy.

- **The recommended altitudes table**, which a plate prints beside the plan so the
  descent can be checked against the DME at a glance. The numbers are not separately
  published: they are the profile, read off at whole miles, so they are taken from the
  descent angle and the final approach fix and cannot disagree with the picture above
  them. Against Jeppesen's own table for Madeira the two agree exactly at the fix and
  within forty feet seven miles later. On an approach with a glidepath the same table is
  the one flown when the glidepath fails, and it is headed LOC (GS out), as a plate
  heads it.

- **The height of each terrain band, written on the band.** The tint said where the
  ground was high and never how high; the figure goes in the widest piece of each band
  on the page.

- **The DME beside every fix**, not only where there is a localiser with one. On a VOR
  approach that number is how the fix is identified in the aeroplane: Madeira's fixes
  now read D12.0 FUN, D7.0 FUN, D3.6 FUN, which is what the plate calls them.

- **The angle the descent is actually flown at.** Without a glidepath to state one, the
  profile printed three degrees and the rate-of-descent table was computed from it —
  a third steeper than Madeira's approach asks for. The angle is now taken from the
  altitudes the procedure publishes over the distance they are flown, written on the
  slope where a plate writes it, and called a descent angle rather than a glidepath.

- **The missed approach point is marked.** Only approaches carrying a published
  localiser minimum were getting the M, because only those drew the V beside it.

- The beacon the approach passes over is drawn as a chart draws a station passage, and
  fix names, altitudes and the distances along the bottom are set at the weight a plate
  sets them in.

## 0.5.1 (2026-09-22)

- **The A220 airport moving map is back, carrying only our own files.** The map itself
  was always ours; what was not was the display-unit bootstrap the old package shipped
  to make the aircraft load it. The package now contains its script and stylesheet and
  nothing else, the map drives itself off the `update` event the aircraft already fires,
  and the two lines that load it are added to the aircraft's own instrument page where
  it sits. A backup is kept beside that file, removing the map takes the lines out
  again, and `amdb-bridge unpatch` restores it. No file of anyone else's is replaced or
  redistributed.

- **The GM5 A220 map patcher is gone.** It matched that add-on by holding six lines of
  its source verbatim, so those lines travelled inside our binary. Nothing replaces it.

- **Accented letters print.** Charts used the built-in fonts as though they were ASCII,
  so Funchal's DR. NÉLIO MENDONÇA came out as question marks. WinAnsi already carries
  the accented letters, and the Central European ones it has no glyph for are written as
  the plain letter underneath rather than as a question mark.

- **The final approach track is measured along the final segment**, not from its last
  fix to the threshold. On a circling approach those are different directions: at
  Madeira the old rule gave 265° for an approach flown on 206°, which pushed the plan
  view the wrong way and opened it to 43 NM of mostly empty sea. The same chart is now
  24 NM across.

- The minima table's approach label is set in capitals, as a plate sets it.

## 0.5.0 (2026-09-22)

- **The A220 airport moving map package is withdrawn.** It replaced the Synaptic A220's
  own display-unit bootstrap, and the file that did so was all but identical to the
  aircraft's: same class, same four members, same registration call. That is the
  aircraft maker's code however small, and shipping it is not ours to do. The package,
  the tools that built and installed it, the installer option and the buttons in the
  desktop app are all gone; nothing that remains redistributes anyone else's files.

- **Approach charts.** `amdbgen approach-chart <ICAO>` draws a full approach chart as a
  PDF, styled like an airline chart and branded "AMDB V1": a header, a briefing strip
  (final course, touchdown zone elevation, airport elevation, minimum safe altitude
  within 25 NM, and the decision altitude), a plan view with terrain shading, the
  airport drawn from our own build, the procedure's fixes at their real positions,
  obstacles, the missed approach, a descent profile, and a minima band. `--approach 27L`
  picks a particular approach, by the name a chart gives it (`ILS 27L`, `RNAV Z 16`) or
  by runway (`27L-2` for the second to the same runway), `--star BIG1A` draws an arrival
  feeding it, `--kind ils|rnav|loc|circling` overrides what sort of approach it is where
  the data does not say, and `--list` prints every approach and arrival an airport has,
  with the names to pass back in. It is not for real-world navigation and says so on the
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
- **The published minimum itself**, in the United States. The FAA gives every approach
  chart away as a PDF, and the minima band on it is text rather than a picture, so for an
  American approach there is nothing to estimate: the chart prints the number the real
  chart prints, reads the circling minima for all four aircraft categories off the same
  band, and says which chart it read and which line. Reading that band means finding it
  by its row labels wherever it sits on the page, telling the altitude from the height
  above touchdown from the ceiling-and-visibility figure printed beside them, telling
  four category columns from one, and preferring the plain minimum over the lower one a
  chart offers with a condition attached. `amdbgen published-check` reads 41 charts whose
  minima were taken off the printed page by hand and reports any it gets wrong; all 41
  are right. The reading is thrown away rather than trusted where the altitude and its
  height above touchdown disagree with the surveyed elevation of the runway.
- **What sort of approach it is**, from the navigation data rather than from a flag. The
  approach record says whether it is an ILS, a localiser, an LDA, RNAV, a VOR or an NDB,
  which is what sets the floor and the width of the area assessed, and is what the chart
  is titled: "ILS RWY 18L", "RNAV (GPS) Z RWY 16". An approach named for a letter rather
  than a runway, and one whose final course is well off the runway, is flown to a
  circling minimum and the chart says so.
- **The missed approach** is weighed too: what an aircraft going around from the minimum
  would have to climb over. Usually that asks for a steeper climb than the standard 200 ft
  a mile, which the chart notes; only where no reasonable climb would do is the minimum
  itself raised.
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
- **A segment distance table and a timing table** under the profile: how far each leg
  runs, and the time from the final approach fix to the missed approach point at the
  speeds an aeroplane flies it.
- Fixed: the second altitude on a leg was read from the wrong place in the navigation
  data, which put a constant 1,171 ft on charts as though it were a constraint.
- Fixed: an approach without a glidepath was assessed for the whole ten miles it may be
  flown over, rather than from the final approach fix inward. What stands before that fix
  is cleared by the altitude the procedure crosses it at, and counting it put a tower at
  Burlington seven hundred feet into a minimum it has nothing to do with.

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
