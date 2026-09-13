# Changelog

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
  `amdbgen --continent`, `--min-runways`.
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
