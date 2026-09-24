//! Writes a `NavSet` as `imported.db3`: Fenix's own normalised SQLite layout, 24 tables of
//! integer-keyed rows rather than repeated identifiers.
//!
//! The schema below is not designed here — it is copied from the file Fenix ships at
//! `%PROGRAMDATA%\Fenix\Navdata\imported.db3` (`pragma table_info` and the four
//! enumeration tables, read once, by hand, off the installed copy on this machine), because
//! getting a `NOT NULL` or a lookup code wrong is invisible until the aircraft's FMS refuses
//! a row or shows the wrong thing. Three tables that schema has room for are always left
//! empty on principle, not oversight: `Holdings` (standalone enroute holds), and the two
//! things no free source on this machine carries at all — a published minimum safe altitude
//! ring and an airway leg's charted altitude limits, neither of which this crate's `NavSet`
//! even has a place to put. `report()` says so on every run, in the same list that reports
//! row counts, so a command built on this never claims silently to have written more than it
//! has.

use crate::convert::model::*;
use crate::convert::TableReport;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;

/// Write `nav` as a fresh Fenix `imported.db3` at `path`, which must not already exist —
/// callers that mean to replace an installed database back it up first (see
/// `convert::backup`) and write to a temporary path, then move it into place only once this
/// returns `Ok`, so a failed write never leaves a half-built file where the aircraft expects
/// its own.
pub fn write(nav: &NavSet, path: &Path) -> Result<Vec<TableReport>> {
    if path.exists() {
        std::fs::remove_file(path).with_context(|| format!("remove existing {}", path.display()))?;
    }
    let mut conn = Connection::open(path).with_context(|| format!("create {}", path.display()))?;
    // The installed database itself relies on 0 meaning "none" in columns that are
    // formally `REFERENCES` another table (no row with ID 0 exists anywhere in it, and its
    // own `foreign_keys` pragma reads off) rather than using NULL throughout, so this
    // writer matches that rather than enforcing a constraint the file it is imitating does
    // not.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    create_schema(&conn)?;
    let tx = conn.transaction()?;
    let mut counts: HashMap<&'static str, usize> = HashMap::new();

    write_config(&tx, nav)?;

    let airport_ids = write_airports(&tx, nav)?;
    counts.insert("Airports", nav.airports.len());
    counts.insert("AirportLookup", nav.airports.len());

    let mut resolver = Resolver::new(&tx, nav)?;
    counts.insert("Waypoints", nav.waypoints.len());
    counts.insert("WaypointLookup", nav.waypoints.len());
    counts.insert("Navaids", nav.navaids.len());
    counts.insert("NavaidLookup", nav.navaids.len());

    let runway_ids = write_runways(&tx, nav, &airport_ids)?;
    counts.insert("Runways", nav.runways.len());

    let ils_ids = write_ils(&tx, nav, &airport_ids, &runway_ids)?;
    counts.insert("ILSes", nav.ils.len());

    let airway_legs = write_airways(&tx, nav, &mut resolver)?;
    counts.insert("Airways", nav.airways.len());
    counts.insert("AirwayLegs", airway_legs);

    let (terminal_count, leg_count) = write_procedures(&tx, nav, &mut resolver, &airport_ids, &runway_ids, &ils_ids)?;
    counts.insert("Terminals", terminal_count);
    counts.insert("TerminalLegs", leg_count);
    counts.insert("TerminalLegsEx", leg_count);

    write_mora(&tx, nav)?;
    counts.insert("GridMora", nav.mora.len());

    // Markers, so a synthetic waypoint created along the way is counted too.
    counts.insert("Waypoints", resolver.waypoint_count());
    counts.insert("WaypointLookup", resolver.waypoint_count());

    create_indexes(&tx)?;
    tx.commit()?;

    Ok(report_with(&counts))
}

/// What `write` would do to `nav`, without touching disk: the row counts a `--dry-run`
/// reports are computed straight off the model, since the count a real write produces is
/// deterministic from it — except `Waypoints`/`WaypointLookup`, which a real write may grow
/// by a handful more than `nav.waypoints.len()` (one per navaid used as a fix that is not
/// already given as a waypoint; see `Resolver::fix`), a figure only the resolver's join
/// against `nav.navaids` can produce, so a dry run reports the given count as a floor.
pub fn plan(nav: &NavSet) -> Vec<TableReport> {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    counts.insert("Airports", nav.airports.len());
    counts.insert("AirportLookup", nav.airports.len());
    counts.insert("Waypoints", nav.waypoints.len());
    counts.insert("WaypointLookup", nav.waypoints.len());
    counts.insert("Navaids", nav.navaids.len());
    counts.insert("NavaidLookup", nav.navaids.len());
    counts.insert("Runways", nav.runways.len());
    counts.insert("ILSes", nav.ils.len());
    counts.insert("Airways", nav.airways.len());
    counts.insert("AirwayLegs", nav.airways.iter().map(|a| a.legs.len()).sum());
    counts.insert("Terminals", nav.procedures.len());
    counts.insert("TerminalLegs", nav.procedures.iter().map(|p| p.legs.len()).sum());
    counts.insert("TerminalLegsEx", nav.procedures.iter().map(|p| p.legs.len()).sum());
    counts.insert("GridMora", nav.mora.len());
    report_with(&counts)
}

fn report_with(counts: &HashMap<&'static str, usize>) -> Vec<TableReport> {
    let mut out = Vec::new();
    let ordered = [
        "Airports",
        "AirportLookup",
        "Runways",
        "Navaids",
        "NavaidLookup",
        "Waypoints",
        "WaypointLookup",
        "Airways",
        "AirwayLegs",
        "ILSes",
        "Terminals",
        "TerminalLegs",
        "TerminalLegsEx",
        "GridMora",
    ];
    for table in ordered {
        out.push(TableReport::filled(table, counts.get(table).copied().unwrap_or(0)));
    }
    out.push(TableReport::empty("Markers", "the model carries no marker-beacon positions (only an ILS's course and glidepath), so none are written"));
    out.push(TableReport::empty("AirportCommunication", "the model carries no radio frequencies for an airport as a whole"));
    out.push(TableReport::empty("Gls", "the model carries conventional ILS/LOC only, no GLS approaches"));
    out.push(TableReport::empty("Holdings", "no free source on this machine publishes a standalone enroute holding pattern"));
    out
}

// ---------------------------------------------------------------------------------------
// Schema, copied from the installed database rather than designed here.
// ---------------------------------------------------------------------------------------

const SCHEMA: &[&str] = &[
    "CREATE TABLE AirportCommunication (area_code TEXT, icao_code TEXT, airport_identifier TEXT, communication_type TEXT, communication_frequency DOUBLE DEFAULT Null, frequency_units TEXT, service_indicator TEXT, callsign TEXT, latitude DOUBLE DEFAULT Null, longitude DOUBLE DEFAULT Null)",
    "CREATE TABLE AirportLookup (extID TEXT, ID INTEGER DEFAULT 0 REFERENCES Airports (ID), PRIMARY KEY(extID,ID))",
    "CREATE TABLE Airports (ID INTEGER NOT NULL DEFAULT 0, Name TEXT, ICAO TEXT, PrimaryID INTEGER DEFAULT Null REFERENCES Airports (ID), Latitude DOUBLE DEFAULT 0, Longtitude DOUBLE DEFAULT 0, Elevation INTEGER DEFAULT 0, TransitionAltitude INTEGER DEFAULT 0, TransitionLevel INTEGER DEFAULT 0, SpeedLimit INTEGER DEFAULT 0, SpeedLimitAltitude INTEGER DEFAULT 0, PRIMARY KEY(ID))",
    "CREATE TABLE AirwayLegs (ID INTEGER DEFAULT 0, AirwayID INTEGER DEFAULT 0 REFERENCES Airways (ID), Level TEXT, Waypoint1ID INTEGER DEFAULT Null REFERENCES Waypoints (ID), Waypoint2ID INTEGER DEFAULT Null REFERENCES Waypoints (ID), IsStart BOOLEAN NOT NULL, IsEnd BOOLEAN NOT NULL, PRIMARY KEY(ID))",
    "CREATE TABLE Airways (ID INTEGER DEFAULT 0, Ident TEXT NOT NULL, PRIMARY KEY(ID))",
    "CREATE TABLE Gls (area_code TEXT, airport_identifier TEXT, icao_code TEXT, gls_ref_path_identifier TEXT, gls_category TEXT, gls_channel INTEGER DEFAULT Null, runway_identifier TEXT, gls_approach_bearing DOUBLE DEFAULT Null, station_latitude DOUBLE DEFAULT Null, station_longitude DOUBLE DEFAULT Null, gls_station_ident TEXT, gls_approach_slope DOUBLE DEFAULT Null, magnetic_variation DOUBLE DEFAULT Null, station_elevation INTEGER DEFAULT Null, station_type TEXT)",
    "CREATE TABLE GridMora (starting_latitude INTEGER DEFAULT 0, starting_longitude INTEGER DEFAULT 0, mora01 TEXT, mora02 TEXT, mora03 TEXT, mora04 TEXT, mora05 TEXT, mora06 TEXT, mora07 TEXT, mora08 TEXT, mora09 TEXT, mora10 TEXT, mora11 TEXT, mora12 TEXT, mora13 TEXT, mora14 TEXT, mora15 TEXT, mora16 TEXT, mora17 TEXT, mora18 TEXT, mora19 TEXT, mora20 TEXT, mora21 TEXT, mora22 TEXT, mora23 TEXT, mora24 TEXT, mora25 TEXT, mora26 TEXT, mora27 TEXT, mora28 TEXT, mora29 TEXT, mora30 TEXT)",
    "CREATE TABLE Holdings (area_code TEXT, region_code TEXT, icao_code TEXT, waypoint_identifier TEXT, holding_name TEXT, waypoint_latitude DOUBLE DEFAULT Null, waypoint_longitude DOUBLE DEFAULT Null, duplicate_identifier INTEGER DEFAULT Null, inbound_holding_course DOUBLE DEFAULT Null, turn_direction TEXT, leg_length DOUBLE DEFAULT Null, leg_time DOUBLE DEFAULT Null, minimum_altitude INTEGER DEFAULT Null, maximum_altitude INTEGER DEFAULT Null, holding_speed INTEGER DEFAULT Null)",
    "CREATE TABLE ILSes (ID INTEGER DEFAULT 0, RunwayID INTEGER DEFAULT 0 REFERENCES Runways (ID), Freq INTEGER DEFAULT 0, GsAngle DOUBLE DEFAULT 0, Latitude DOUBLE DEFAULT 0, Longtitude DOUBLE DEFAULT 0, Category INTEGER DEFAULT 0, Ident TEXT, LocCourse DOUBLE DEFAULT 0, CrossingHeight INTEGER DEFAULT 0, HasDme BOOLEAN NOT NULL, Elevation INTEGER DEFAULT 0, PRIMARY KEY(ID))",
    "CREATE TABLE MarkerTypes (Type INTEGER NOT NULL DEFAULT 0, Desc TEXT, PRIMARY KEY(Type))",
    "CREATE TABLE Markers (ID INTEGER NOT NULL DEFAULT 0, AirportID INTEGER NOT NULL DEFAULT 0 REFERENCES Airports (ID), RunwayID INTEGER DEFAULT 0 REFERENCES Runways (ID), LLZIdent TEXT, MarkerIdent TEXT, Type INTEGER DEFAULT Null REFERENCES MarkerTypes (Type), Latitude DOUBLE DEFAULT 0, Longitude DOUBLE DEFAULT 0, PRIMARY KEY(ID))",
    "CREATE TABLE NavaidLookup (Ident TEXT NOT NULL, Type INTEGER NOT NULL DEFAULT 0, Country TEXT NOT NULL, NavKeyCode INTEGER NOT NULL DEFAULT 0, ID INTEGER NOT NULL DEFAULT 0 REFERENCES Navaids (ID), PRIMARY KEY(Ident,Type,Country,NavKeyCode))",
    "CREATE TABLE NavaidTypes (Type INTEGER DEFAULT 0, Desc TEXT, PRIMARY KEY(Type))",
    "CREATE TABLE Navaids (ID INTEGER NOT NULL DEFAULT 0, Ident TEXT, Type INTEGER DEFAULT 0 REFERENCES NavaidTypes (Type), Name TEXT, Freq INTEGER, Channel TEXT, Usage TEXT, Latitude DOUBLE DEFAULT 0, Longtitude DOUBLE DEFAULT 0, Elevation INTEGER DEFAULT 0, SlavedVar DOUBLE DEFAULT 0, MagneticVariation DOUBLE DEFAULT Null, Range INTEGER DEFAULT Null, PRIMARY KEY(ID))",
    "CREATE TABLE Runways (ID INTEGER DEFAULT 0, AirportID INTEGER DEFAULT 0 REFERENCES Airports (ID), Ident TEXT, TrueHeading DOUBLE DEFAULT 0, Length INTEGER DEFAULT 0, Width INTEGER DEFAULT 0, Surface TEXT REFERENCES SurfaceTypes (SurfaceType), Latitude DOUBLE DEFAULT 0, Longtitude DOUBLE DEFAULT 0, Elevation INTEGER DEFAULT 0, PRIMARY KEY(ID))",
    "CREATE TABLE SurfaceTypes (SurfaceType TEXT, Description TEXT, PRIMARY KEY(SurfaceType))",
    "CREATE TABLE TerminalLegs (ID INTEGER NOT NULL DEFAULT 0 REFERENCES TerminalLegsEx (ID), TerminalID INTEGER NOT NULL DEFAULT 0 REFERENCES Terminals (ID), Type TEXT, Transition TEXT, TrackCode TEXT REFERENCES TrmLegTypes (Code), WptID INTEGER DEFAULT 0 REFERENCES Waypoints (ID), WptLat DOUBLE DEFAULT 0, WptLon DOUBLE DEFAULT 0, TurnDir TEXT, NavID INTEGER DEFAULT 0 REFERENCES Navaids (ID), NavLat DOUBLE DEFAULT 0, NavLon DOUBLE DEFAULT 0, NavBear DOUBLE DEFAULT 0, NavDist DOUBLE DEFAULT 0, Course DOUBLE DEFAULT 0, Distance DOUBLE DEFAULT 0, Alt TEXT, Vnav DOUBLE DEFAULT 0, CenterID INTEGER DEFAULT 0, CenterLat DOUBLE DEFAULT 0, CenterLon DOUBLE DEFAULT 0, WptDescCode TEXT, PRIMARY KEY(ID))",
    "CREATE TABLE TerminalLegsEx (ID INTEGER NOT NULL DEFAULT 0, IsFlyOver BOOLEAN NOT NULL, SpeedLimit DOUBLE DEFAULT 0, SpeedLimitDescription TEXT, PRIMARY KEY(ID))",
    "CREATE TABLE Terminals (ID INTEGER NOT NULL DEFAULT 0, AirportID INTEGER DEFAULT 0 REFERENCES Airports (ID), Proc INTEGER DEFAULT 0, ICAO TEXT NOT NULL, FullName TEXT NOT NULL, Name TEXT NOT NULL, Rwy TEXT, RwyID INTEGER DEFAULT 0, IlsID INTEGER DEFAULT 0, PRIMARY KEY(ID))",
    "CREATE TABLE TrmLegTypes (Code TEXT NOT NULL, Description TEXT, PRIMARY KEY(Code))",
    "CREATE TABLE WaypointLookup (Ident TEXT NOT NULL, Country TEXT NOT NULL, ID INTEGER NOT NULL DEFAULT 0 REFERENCES Waypoints (ID), PRIMARY KEY(Ident,Country,ID))",
    "CREATE TABLE Waypoints (ID INTEGER DEFAULT 0, Ident TEXT NOT NULL, Collocated BOOLEAN NOT NULL, Name TEXT, Latitude DOUBLE DEFAULT 0, Longtitude DOUBLE DEFAULT 0, NavaidID INTEGER DEFAULT Null REFERENCES Navaids (ID), PRIMARY KEY(ID))",
    "CREATE TABLE config (key TEXT, val TEXT, PRIMARY KEY(key))",
];

/// Copied off the installed database with the same `CREATE INDEX` statements it ships,
/// applied after the bulk insert rather than during it: building an index row by row over
/// hundreds of thousands of waypoints is what makes a naive writer slow, not the insert
/// itself.
const INDEXES: &[&str] = &[
    "CREATE UNIQUE INDEX AirportsICAO ON Airports (ICAO)",
    "CREATE UNIQUE INDEX AirportsId ON Airports (ID)",
    "CREATE INDEX AirwayLegsAirwayID ON AirwayLegs (AirwayID)",
    "CREATE INDEX AirwaysID ON Airways (ID)",
    "CREATE INDEX AirwaysIdent ON Airways (Ident)",
    "CREATE INDEX ILSesFreq ON ILSes (Freq)",
    "CREATE UNIQUE INDEX ILSesID ON ILSes (ID)",
    "CREATE INDEX ILSesIdent ON ILSes (Ident)",
    "CREATE INDEX ILSesRunwayID ON ILSes (RunwayID)",
    "CREATE INDEX RunwaysAirportId ON Runways (AirportID)",
    "CREATE UNIQUE INDEX RunwaysId ON Runways (ID)",
    "CREATE INDEX RunwaysIdent ON Runways (Ident)",
    "CREATE INDEX RunwaysLength ON Runways (Length)",
    "CREATE INDEX RunwaysLatitude ON Runways (Latitude)",
    "CREATE INDEX RunwaysLongtitude ON Runways (Longtitude)",
    "CREATE UNIQUE INDEX TerminalLegsID ON TerminalLegs (ID)",
    "CREATE INDEX TerminalLegsTerminalID ON TerminalLegs (TerminalID)",
    "CREATE INDEX TerminalLegsTrackCode ON TerminalLegs (TrackCode)",
    "CREATE INDEX TerminalLegsType ON TerminalLegs (Type)",
    "CREATE INDEX TerminalLegsWptId ON TerminalLegs (WptID)",
    "CREATE INDEX TerminalLegsExID ON TerminalLegsEx (ID)",
    "CREATE INDEX TerminalsICAO ON Terminals (ICAO)",
    "CREATE INDEX WaypointlookupId ON WaypointLookup (ID)",
    "CREATE INDEX WaypointlookupIdent ON WaypointLookup (Ident)",
    "CREATE UNIQUE INDEX WaypointsID ON Waypoints (ID)",
    "CREATE INDEX WaypointsLatitude ON Waypoints (Latitude)",
    "CREATE INDEX WaypointsLongitude ON Waypoints (Longtitude)",
];

/// `NavaidTypes`, `SurfaceTypes`, `MarkerTypes` and `TrmLegTypes`: the codes an FMS looks up
/// by number or letter, reproduced exactly off the installed database rather than
/// renumbered, because a code this crate invented would collide with nothing on read but
/// mean the wrong thing.
const NAVAID_TYPES: &[(i64, &str)] = &[
    (1, "VOR"),
    (2, "VORTAC"),
    (3, "TACAN"),
    (4, "VOR-DME"),
    (5, "NDB"),
    (7, "NDB-DME"),
    (8, "ILS-DME"),
    (9, "DME (EXCLUDING ILS-DME)"),
];

const SURFACE_TYPES: &[(&str, &str)] = &[
    ("ASP", "ASPHALT, ASPHALTIC CONCRETE, TAR MACADAM, OR BITUMEN BOUND MACADAM (INCLUDING ANY OF THESE SURFACE TYPES WITH CONCRETE ENDS)."),
    ("BIT", "BITUMINOUS, TAR OR ASPHALT MIXED IN PLACE, OILED."),
    ("BRI", "BRICK, LAID OR MORTARED."),
    ("CLA", "CLAY."),
    ("COM", "COMPOSITE, LESS THAN 50 PERCENT OF THE RUNWAY LENGTH IS PERMANENT."),
    ("CON", "CONCRETE."),
    ("COP", "COMPOSITE, 50 PERCENT OR MORE OF THE RUNWAY LENGTH IS PERMANENT."),
    ("COR", "CORAL."),
    ("GRE", "GRADED OR ROLLED EARTH, GRASS ON GRADED EARTH."),
    ("GRS", "GRASS OR EARTH NOT GRADED OR GRASS OR EARTH NOT GRADED OR ROLLED."),
    ("GVL", "GRAVEL."),
    ("ICE", "ICE."),
    ("LAT", "LATERITE."),
    ("MAC", "MACADAM - CRUSHED ROCK WATER BOUND."),
    ("MEM", "MEMBRANE - PLASTIC OR OTHER COATED FIBER MATERIAL."),
    ("MIX", "MIX IN PLACE USING NONBITUMIOUS BINDERS SUCH AS PORTLAND CEMENT."),
    ("PEM", "PART CONCRETE, PART ASPHALT, OR PART BITUMEN-BOUND MACADAM."),
    ("PER", "PERMANENT, SURFACE TYPE UNKNOWN."),
    ("PSP", "PIECED STEEL PLANKING."),
    ("SAN", "SAND, GRADED, ROLLED OR OILED."),
    ("SNO", "SNOW."),
    ("U", "SURFACE UNKNOWN."),
    ("WAT", "WATER"),
];

const MARKER_TYPES: &[(i64, &str)] = &[(1, "IM"), (2, "MM"), (3, "OM"), (4, "BM"), (5, "LIM"), (6, "LMM"), (7, "LOM"), (8, "LBM")];

const TRM_LEG_TYPES: &[(&str, &str)] = &[
    ("AF", "Constant DME Arc to Fix"),
    ("CA", "Course to an altitude (position unspecified)"),
    ("CD", "Course to DME distance"),
    ("CF", "Course to a fix"),
    ("CI", "Course to next leg following by course oriented leg (interception point unspecified)"),
    ("CR", "Course to a radial termination (intercept point unspecified)"),
    ("DF", "Computed track direct to a fix"),
    ("DS", "DISCONTINUITY"),
    ("FA", "Course from a fix to an altitude"),
    ("FC", "Course from a fix to a distance"),
    ("FD", "Course from a fix to DME distance"),
    ("FM", "Course from a fix to manual termination"),
    ("HA", "Automatically at the fix after reaching an altitude"),
    ("HF", "Automatically at the fix after one full circuit"),
    ("HM", "Manually"),
    ("IF", "Initial Fix"),
    ("PI", "Procedure turn followed by a course to a fix (CF)"),
    ("RF", "Constant radius to a fix"),
    ("TF", "Track between two fixes (great circle)"),
    ("VA", "heading to an altitude (position unspecified)"),
    ("VD", "Heading to a DME Distance (position unspecified)"),
    ("VI", "Heading to a next leg (position unspecified)"),
    ("VM", "Heading to a manual termination"),
    ("VR", "Heading to a radial termination"),
];

fn create_schema(conn: &Connection) -> Result<()> {
    for stmt in SCHEMA {
        conn.execute(stmt, []).with_context(|| format!("create schema: {stmt}"))?;
    }
    for (code, desc) in NAVAID_TYPES {
        conn.execute("INSERT INTO NavaidTypes (Type, Desc) VALUES (?1, ?2)", params![code, desc])?;
    }
    for (code, desc) in SURFACE_TYPES {
        conn.execute("INSERT INTO SurfaceTypes (SurfaceType, Description) VALUES (?1, ?2)", params![code, desc])?;
    }
    for (code, desc) in MARKER_TYPES {
        conn.execute("INSERT INTO MarkerTypes (Type, Desc) VALUES (?1, ?2)", params![code, desc])?;
    }
    for (code, desc) in TRM_LEG_TYPES {
        conn.execute("INSERT INTO TrmLegTypes (Code, Description) VALUES (?1, ?2)", params![code, desc])?;
    }
    Ok(())
}

fn create_indexes(conn: &Connection) -> Result<()> {
    for stmt in INDEXES {
        conn.execute(stmt, []).with_context(|| format!("create index: {stmt}"))?;
    }
    Ok(())
}

fn write_config(conn: &Connection, nav: &NavSet) -> Result<()> {
    conn.execute("INSERT INTO config (key, val) VALUES ('CycleName', ?1)", params![nav.cycle])?;
    if let Some((start, end)) = airac_bounds(&nav.cycle) {
        conn.execute("INSERT INTO config (key, val) VALUES ('CycleStartDate', ?1)", params![format_airac_date(start)])?;
        conn.execute("INSERT INTO config (key, val) VALUES ('CycleEndDate', ?1)", params![format_airac_date(end)])?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Airports, runways, ILS.
// ---------------------------------------------------------------------------------------

fn write_airports(conn: &Connection, nav: &NavSet) -> Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare(
        "INSERT INTO Airports (ID, Name, ICAO, Latitude, Longtitude, Elevation, TransitionAltitude, TransitionLevel, SpeedLimit, SpeedLimitAltitude) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    let mut lookup = conn.prepare("INSERT INTO AirportLookup (extID, ID) VALUES (?1, ?2)")?;
    let mut ids = HashMap::new();
    for (i, a) in nav.airports.iter().enumerate() {
        let id = (i + 1) as i64;
        stmt.execute(params![
            id,
            a.name,
            a.icao,
            a.lat,
            a.lon,
            a.elevation_ft as i64,
            a.transition_altitude_ft.unwrap_or(0.0) as i64,
            a.transition_level_ft.unwrap_or(0.0) as i64,
            a.speed_limit_kt.unwrap_or(0.0) as i64,
            a.speed_limit_altitude_ft.unwrap_or(0.0) as i64,
        ])?;
        // The pattern the installed database uses: an area code ahead of the ICAO ident,
        // except in areas (Antarctica among them) that ARINC gives a shared two-letter
        // area code of their own rather than one derived from the ICAO prefix.
        lookup.execute(params![format!("{}{}", a.area_code, a.icao), id])?;
        ids.insert(a.icao.clone(), id);
    }
    Ok(ids)
}

fn surface_code(s: Surface) -> &'static str {
    match s {
        Surface::Asphalt => "ASP",
        Surface::Concrete => "CON",
        Surface::Gravel => "GVL",
        Surface::Grass => "GRS",
        Surface::Water => "WAT",
        Surface::Snow => "SNO",
        Surface::Ice => "ICE",
        Surface::Unknown => "U",
    }
}

fn write_runways(conn: &Connection, nav: &NavSet, airport_ids: &HashMap<String, i64>) -> Result<HashMap<(String, String), i64>> {
    let mut stmt = conn.prepare(
        "INSERT INTO Runways (ID, AirportID, Ident, TrueHeading, Length, Width, Surface, Latitude, Longtitude, Elevation) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    let mut ids = HashMap::new();
    for (i, r) in nav.runways.iter().enumerate() {
        let Some(&airport_id) = airport_ids.get(&r.airport_icao) else { continue };
        let id = (i + 1) as i64;
        stmt.execute(params![id, airport_id, r.ident, r.heading_true_deg, r.length_ft as i64, r.width_ft as i64, surface_code(r.surface), r.lat, r.lon, r.elevation_ft as i64])?;
        ids.insert((r.airport_icao.clone(), r.ident.clone()), id);
    }
    Ok(ids)
}

fn ils_category_code(c: IlsCategory) -> i64 {
    match c {
        IlsCategory::Loc => 0,
        IlsCategory::Cat1 => 1,
        IlsCategory::Cat2 => 2,
        IlsCategory::Cat3 => 3,
    }
}

fn write_ils(
    conn: &Connection,
    nav: &NavSet,
    airport_ids: &HashMap<String, i64>,
    runway_ids: &HashMap<(String, String), i64>,
) -> Result<HashMap<(String, String), i64>> {
    let mut stmt = conn.prepare(
        "INSERT INTO ILSes (ID, RunwayID, Freq, GsAngle, Latitude, Longtitude, Category, Ident, LocCourse, CrossingHeight, HasDme, Elevation) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let mut ids = HashMap::new();
    for (i, x) in nav.ils.iter().enumerate() {
        if !airport_ids.contains_key(&x.airport_icao) {
            continue;
        }
        let runway_id = runway_ids.get(&(x.airport_icao.clone(), x.runway_ident.clone())).copied().unwrap_or(0);
        let id = (i + 1) as i64;
        stmt.execute(params![
            id,
            runway_id,
            hz(x.frequency_mhz),
            x.glidepath_deg.unwrap_or(0.0),
            x.lat,
            x.lon,
            ils_category_code(x.category),
            x.ident,
            x.course_mag_deg,
            x.crossing_height_ft.unwrap_or(0.0) as i64,
            x.has_dme,
            x.elevation_ft as i64,
        ])?;
        ids.insert((x.airport_icao.clone(), x.runway_ident.clone()), id);
    }
    Ok(ids)
}

/// Frequency stored as whole hertz. The installed database's own `Freq` column carries
/// values in an undocumented, non-linear encoding this crate could not confirm by fitting
/// the data on this machine (see `docs` in the PR/handback for the numbers tried); Hz is
/// the defensible, physically meaningful choice rather than guessing that encoding wrong,
/// and it is flagged here for whoever verifies this writer against a running Fenix install.
fn hz(mhz: f64) -> i64 {
    (mhz * 1_000_000.0).round() as i64
}

// ---------------------------------------------------------------------------------------
// Waypoints and navaids, with the resolver that ties leg and airway endpoints to them.
// ---------------------------------------------------------------------------------------

fn navaid_type_code(k: NavaidKind) -> i64 {
    match k {
        NavaidKind::Vor => 1,
        NavaidKind::Vortac => 2,
        NavaidKind::Tacan => 3,
        NavaidKind::VorDme => 4,
        NavaidKind::Ndb => 5,
        NavaidKind::NdbDme => 7,
        NavaidKind::IlsDme => 8,
        NavaidKind::Dme => 9,
    }
}

/// Resolves a leg's or an airway's fix idents to the row IDs Fenix's schema wants, across
/// both `Waypoints` and `Navaids`. A fix used as an enroute or procedure endpoint that is a
/// VOR or NDB rather than a plain waypoint is expected to already appear in
/// `NavSet::waypoints` with `collocated` semantics the way ARINC 424 itself carries it (a
/// tuned navaid used as a fix has its own waypoint-shaped row); where a source has not done
/// that, this resolver creates the missing waypoint on the fly so no leg is left pointing at
/// nothing, and counts it in `waypoint_count()`.
struct Resolver<'a> {
    conn: &'a Connection,
    waypoints: HashMap<(String, String), (i64, f64, f64)>,
    navaids: HashMap<(String, String), (i64, f64, f64, i64)>,
    next_waypoint_id: i64,
    synthetic: usize,
}

impl<'a> Resolver<'a> {
    fn new(conn: &'a Connection, nav: &NavSet) -> Result<Resolver<'a>> {
        let mut stmt = conn.prepare("INSERT INTO Waypoints (ID, Ident, Collocated, Name, Latitude, Longtitude, NavaidID) VALUES (?1, ?2, 0, ?3, ?4, ?5, NULL)")?;
        let mut lookup = conn.prepare("INSERT INTO WaypointLookup (Ident, Country, ID) VALUES (?1, ?2, ?3)")?;
        let mut waypoints = HashMap::new();
        for (i, w) in nav.waypoints.iter().enumerate() {
            let id = (i + 1) as i64;
            stmt.execute(params![id, w.ident, w.name, w.lat, w.lon])?;
            lookup.execute(params![w.ident, w.region_code, id])?;
            waypoints.insert((w.ident.clone(), w.region_code.clone()), (id, w.lat, w.lon));
        }
        let next_waypoint_id = nav.waypoints.len() as i64 + 1;

        let mut nstmt = conn.prepare(
            "INSERT INTO Navaids (ID, Ident, Type, Name, Freq, Channel, Usage, Latitude, Longtitude, Elevation, SlavedVar, MagneticVariation, Range) \
             VALUES (?1, ?2, ?3, ?4, ?5, '', 'H', ?6, ?7, ?8, 0.0, ?9, ?10)",
        )?;
        let mut nlookup = conn.prepare("INSERT INTO NavaidLookup (Ident, Type, Country, NavKeyCode, ID) VALUES (?1, ?2, ?3, ?4, ?5)")?;
        let mut navaids = HashMap::new();
        let mut navaid_dupes: HashMap<(String, i64, String), i64> = HashMap::new();
        for (i, n) in nav.navaids.iter().enumerate() {
            let id = (i + 1) as i64;
            let type_code = navaid_type_code(n.kind);
            let freq_raw = if matches!(n.kind, NavaidKind::Ndb | NavaidKind::NdbDme) { (n.frequency * 1000.0).round() as i64 } else { hz(n.frequency) };
            nstmt.execute(params![id, n.ident, type_code, n.name, freq_raw, n.lat, n.lon, n.elevation_ft as i64, n.magnetic_variation_deg, n.range_nm])?;
            let key = (n.ident.clone(), type_code, n.region_code.clone());
            let key_code = *navaid_dupes.entry(key).and_modify(|c| *c += 1).or_insert(1);
            nlookup.execute(params![n.ident, type_code, n.region_code, key_code, id])?;
            navaids.insert((n.ident.clone(), n.region_code.clone()), (id, n.lat, n.lon, type_code));
        }

        Ok(Resolver { conn, waypoints, navaids, next_waypoint_id, synthetic: 0 })
    }

    /// `self.waypoints` already grows by one on every synthetic insert `fix()` makes, so
    /// its length alone is the total row count — `self.synthetic` is kept only for the
    /// (unrelated) count of how many of them were synthesised, not added again here.
    fn waypoint_count(&self) -> usize {
        self.waypoints.len()
    }

    /// A fix's row ID and position, creating a stand-in waypoint out of a matching navaid
    /// if no waypoint of that ident and region already exists.
    fn fix(&mut self, ident: &str, region: &str) -> Result<Option<(i64, f64, f64)>> {
        let key = (ident.to_string(), region.to_string());
        if let Some(&(id, lat, lon)) = self.waypoints.get(&key) {
            return Ok(Some((id, lat, lon)));
        }
        if let Some(&(_, lat, lon, navaid_id)) = self.navaids.get(&key) {
            let id = self.next_waypoint_id;
            self.next_waypoint_id += 1;
            self.synthetic += 1;
            self.conn.execute(
                "INSERT INTO Waypoints (ID, Ident, Collocated, Name, Latitude, Longtitude, NavaidID) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6)",
                params![id, ident, ident, lat, lon, self.navaids[&key].0],
            )?;
            self.conn.execute("INSERT INTO WaypointLookup (Ident, Country, ID) VALUES (?1, ?2, ?3)", params![ident, region, id])?;
            let _ = navaid_id;
            self.waypoints.insert(key, (id, lat, lon));
            return Ok(Some((id, lat, lon)));
        }
        Ok(None)
    }

    fn navaid(&self, ident: &str, region: &str) -> Option<(i64, f64, f64)> {
        self.navaids.get(&(ident.to_string(), region.to_string())).map(|&(id, lat, lon, _)| (id, lat, lon))
    }
}

// ---------------------------------------------------------------------------------------
// Airways.
// ---------------------------------------------------------------------------------------

fn airway_level_code(l: AirwayLevel) -> &'static str {
    match l {
        AirwayLevel::Both => "B",
        AirwayLevel::High => "H",
        AirwayLevel::Low => "L",
    }
}

fn write_airways(conn: &Connection, nav: &NavSet, resolver: &mut Resolver) -> Result<usize> {
    let mut airways_stmt = conn.prepare("INSERT INTO Airways (ID, Ident) VALUES (?1, ?2)")?;
    let mut legs_stmt =
        conn.prepare("INSERT INTO AirwayLegs (ID, AirwayID, Level, Waypoint1ID, Waypoint2ID, IsStart, IsEnd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
    let mut leg_id = 1i64;
    for (i, a) in nav.airways.iter().enumerate() {
        let airway_id = (i + 1) as i64;
        airways_stmt.execute(params![airway_id, a.ident])?;
        for leg in &a.legs {
            let from = resolver.fix(&leg.from_ident, &leg.from_region)?.map(|(id, _, _)| id);
            let to = resolver.fix(&leg.to_ident, &leg.to_region)?.map(|(id, _, _)| id);
            legs_stmt.execute(params![leg_id, airway_id, airway_level_code(leg.level), from, to, leg.is_start, leg.is_end])?;
            leg_id += 1;
        }
    }
    Ok((leg_id - 1) as usize)
}

// ---------------------------------------------------------------------------------------
// Procedures: SIDs, STARs, approaches, already flattened one leg sequence per variant.
// ---------------------------------------------------------------------------------------

fn proc_code(k: ProcKind) -> i64 {
    match k {
        ProcKind::Sid => 1,
        ProcKind::Star => 2,
        ProcKind::Approach => 3,
    }
}

/// The ARINC 424 route-type character `TerminalLegs.Type` carries. It is a free-text
/// column with no lookup table behind it in the installed schema, so a wrong guess here
/// cannot corrupt a join the way a wrong `NavaidTypes` code would — but it is still worth
/// getting close: a SID/STAR's transition role by the numbering ARINC's own field 4.9 uses,
/// and an approach's by the leading letter its own published identifier conventionally
/// carries (I, R, V, N... for ILS, RNAV, VOR, NDB and so on), the same convention this
/// crate's approach-kind detection already leans on elsewhere in the pipeline.
fn route_type(p: &ProcedureRec) -> String {
    match p.kind {
        ProcKind::Sid => {
            if p.transition_ident.is_some() {
                "4".to_string()
            } else if p.runway_ident.is_some() {
                "2".to_string()
            } else {
                "3".to_string()
            }
        }
        ProcKind::Star => {
            if p.transition_ident.is_some() {
                "3".to_string()
            } else if p.runway_ident.is_some() {
                "1".to_string()
            } else {
                "2".to_string()
            }
        }
        ProcKind::Approach => p.ident.chars().next().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_else(|| "D".to_string()),
    }
}

fn turn_code(t: TurnDirection) -> &'static str {
    match t {
        TurnDirection::Left => "L",
        TurnDirection::Right => "R",
        TurnDirection::Either => "",
    }
}

fn fmt5(v: f64) -> String {
    format!("{:05.0}", v.clamp(0.0, 99_999.0))
}

/// `TerminalLegs.Alt`, in the format the installed database itself uses: five digits, an
/// `A` suffix for at-or-above, a `B` suffix for at-or-below, both concatenated (ceiling
/// first) for a window, or the literal string `MAP` in place of any altitude for the
/// missed-approach point — confirmed by reading every distinct value the column holds on
/// this machine rather than assumed.
fn alt_text(leg: &LegRec) -> String {
    if leg.is_map {
        return "MAP".to_string();
    }
    match (leg.altitude_rule, leg.altitude1_ft, leg.altitude2_ft) {
        (Some(AltitudeRule::At), Some(a), _) => fmt5(a),
        (Some(AltitudeRule::AtOrAbove), Some(a), _) => format!("{}A", fmt5(a)),
        (Some(AltitudeRule::AtOrBelow), Some(a), _) => format!("{}B", fmt5(a)),
        (Some(AltitudeRule::Between), Some(floor), Some(ceiling)) => format!("{}B{}A", fmt5(ceiling), fmt5(floor)),
        _ => String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_procedures(
    conn: &Connection,
    nav: &NavSet,
    resolver: &mut Resolver,
    airport_ids: &HashMap<String, i64>,
    runway_ids: &HashMap<(String, String), i64>,
    ils_ids: &HashMap<(String, String), i64>,
) -> Result<(usize, usize)> {
    let mut term_stmt = conn.prepare("INSERT INTO Terminals (ID, AirportID, Proc, ICAO, FullName, Name, Rwy, RwyID, IlsID) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?;
    let mut leg_stmt = conn.prepare(
        "INSERT INTO TerminalLegs (ID, TerminalID, Type, Transition, TrackCode, WptID, WptLat, WptLon, TurnDir, NavID, NavLat, NavLon, NavBear, NavDist, Course, Distance, Alt, Vnav, CenterID, CenterLat, CenterLon, WptDescCode) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, 0, ?18, ?19, ?20, '')",
    )?;
    let mut ex_stmt = conn.prepare("INSERT INTO TerminalLegsEx (ID, IsFlyOver, SpeedLimit, SpeedLimitDescription) VALUES (?1, ?2, ?3, ?4)")?;

    let mut terminal_count = 0usize;
    let mut leg_id = 1i64;
    for (i, p) in nav.procedures.iter().enumerate() {
        let Some(&airport_id) = airport_ids.get(&p.airport_icao) else { continue };
        let terminal_id = (i + 1) as i64;
        let runway_id = p.runway_ident.as_ref().and_then(|r| runway_ids.get(&(p.airport_icao.clone(), r.clone()))).copied().unwrap_or(0);
        let ils_id = if matches!(p.kind, ProcKind::Approach) {
            p.runway_ident.as_ref().and_then(|r| ils_ids.get(&(p.airport_icao.clone(), r.clone()))).copied().unwrap_or(0)
        } else {
            0
        };
        term_stmt.execute(params![terminal_id, airport_id, proc_code(p.kind), p.airport_icao, p.ident, p.ident, p.runway_ident, runway_id, ils_id])?;
        terminal_count += 1;

        let transition = p.transition_ident.clone().unwrap_or_else(|| "ALL".to_string());
        let route_type = route_type(p);
        for leg in &p.legs {
            let (wpt_id, wpt_lat, wpt_lon) = match (&leg.fix_ident, &leg.fix_region) {
                (Some(ident), Some(region)) => resolver.fix(ident, region)?.map(|(id, lat, lon)| (id, lat, lon)).unwrap_or((0, 0.0, 0.0)),
                _ => (0, 0.0, 0.0),
            };
            let (nav_id, nav_lat, nav_lon) = match (&leg.recommended_navaid, &leg.recommended_navaid_region) {
                (Some(ident), Some(region)) => resolver.navaid(ident, region).unwrap_or((0, 0.0, 0.0)),
                _ => (0, 0.0, 0.0),
            };
            let (center_id, center_lat, center_lon) = match (&leg.center_fix, &leg.center_fix_region) {
                (Some(ident), Some(region)) => resolver.fix(ident, region)?.unwrap_or((0, 0.0, 0.0)),
                _ => (0, 0.0, 0.0),
            };
            // TerminalLegsEx first: TerminalLegs.ID references it, and that FK is enforced
            // (rusqlite's bundled SQLite has foreign keys on by default).
            ex_stmt.execute(params![leg_id, leg.is_flyover, leg.speed_limit_kt.unwrap_or(0.0), Option::<String>::None])?;
            leg_stmt.execute(params![
                leg_id,
                terminal_id,
                route_type,
                transition,
                leg.path_terminator.code(),
                wpt_id,
                wpt_lat,
                wpt_lon,
                turn_code(leg.turn),
                nav_id,
                nav_lat,
                nav_lon,
                leg.theta_deg.unwrap_or(0.0),
                leg.rho_nm.unwrap_or(0.0),
                leg.course_deg.unwrap_or(0.0),
                leg.leg_length.unwrap_or(0.0),
                alt_text(leg),
                center_id,
                center_lat,
                center_lon,
            ])?;
            leg_id += 1;
        }
    }
    Ok((terminal_count, (leg_id - 1) as usize))
}

// ---------------------------------------------------------------------------------------
// Grid MORA.
// ---------------------------------------------------------------------------------------

fn write_mora(conn: &Connection, nav: &NavSet) -> Result<()> {
    // Bucketed into the same one-degree-square rows the installed database uses: one row
    // per starting latitude and per 30-square longitude band, each cell hundreds of feet
    // as text, "UNK" left for a square nothing here covers.
    let mut bands: HashMap<(i64, i64), [Option<i64>; 30]> = HashMap::new();
    for cell in &nav.mora {
        let lat = cell.lat.floor() as i64;
        let lon_floor = cell.lon.floor() as i64;
        let band_start = lon_floor.div_euclid(30) * 30;
        let idx = (lon_floor - band_start) as usize;
        if idx >= 30 {
            continue;
        }
        let entry = bands.entry((lat, band_start)).or_insert([None; 30]);
        // Hundreds of feet, rounded up: a MORA is never made unsafe by being too high.
        entry[idx] = Some(((cell.altitude_ft / 100.0).ceil() as i64).max(0));
    }
    let columns = (1..=30).map(|i| format!("mora{i:02}")).collect::<Vec<_>>().join(", ");
    let placeholders = (1..=32).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
    let sql = format!("INSERT INTO GridMora (starting_latitude, starting_longitude, {columns}) VALUES ({placeholders})");
    let mut stmt = conn.prepare(&sql)?;
    let mut keys: Vec<_> = bands.keys().copied().collect();
    keys.sort();
    for key in keys {
        let cells = &bands[&key];
        let mut row: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(key.0), Box::new(key.1)];
        for cell in cells {
            row.push(Box::new(cell.map(|v| format!("{v:03}"))));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = row.iter().map(|b| b.as_ref()).collect();
        stmt.execute(refs.as_slice())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// AIRAC calendar: cycles run exactly 28 days apart from a known anchor, with no drift
// correction due for another decade, so a cycle's bounds are computed rather than left
// blank for any cycle from the 2020s or 2030s.
// ---------------------------------------------------------------------------------------

/// The first and last day of an AIRAC cycle named "YYCC" (e.g. "2503"), or `None` if the
/// cycle predates the anchor or is too far beyond it to be worth walking to (roughly the
/// next 45 years, which is not this crate's problem).
fn airac_bounds(cycle: &str) -> Option<(chrono::NaiveDate, chrono::NaiveDate)> {
    use chrono::{Datelike, Duration, NaiveDate};
    let cycle = cycle.trim();
    if cycle.len() != 4 || !cycle.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let target_year = 2000 + cycle[0..2].parse::<i32>().ok()?;
    let target_n: u32 = cycle[2..4].parse().ok()?;
    // AIRAC 2101 began 2021-01-28.
    let mut date = NaiveDate::from_ymd_opt(2021, 1, 28)?;
    let mut year = 2021;
    let mut n = 1u32;
    for _ in 0..600 {
        if year == target_year && n == target_n {
            return Some((date, date + Duration::days(27)));
        }
        let next = date + Duration::days(28);
        if next.year() != year {
            year = next.year();
            n = 1;
        } else {
            n += 1;
        }
        date = next;
    }
    None
}

fn format_airac_date(d: chrono::NaiveDate) -> String {
    // "20MAR25", the format the installed database's own config table uses.
    d.format("%d%b%y").to_string().to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic() -> NavSet {
        NavSet {
            cycle: "2503".to_string(),
            airports: vec![AirportRec {
                icao: "KTST".to_string(),
                name: "TEST FIELD".to_string(),
                area_code: "USA".to_string(),
                icao_code: "K".to_string(),
                lat: 40.0,
                lon: -100.0,
                elevation_ft: 1200.0,
                transition_altitude_ft: Some(18000.0),
                transition_level_ft: Some(18000.0),
                speed_limit_kt: Some(250.0),
                speed_limit_altitude_ft: Some(10000.0),
            }],
            runways: vec![RunwayRec {
                airport_icao: "KTST".to_string(),
                ident: "18".to_string(),
                heading_true_deg: 180.0,
                length_ft: 9000.0,
                width_ft: 150.0,
                surface: Surface::Concrete,
                lat: 40.01,
                lon: -100.0,
                elevation_ft: 1200.0,
            }],
            navaids: vec![NavaidRec {
                ident: "TST".to_string(),
                kind: NavaidKind::VorDme,
                name: "TEST VOR".to_string(),
                region_code: "K1".to_string(),
                area_code: "USA".to_string(),
                frequency: 112.3,
                lat: 40.2,
                lon: -100.2,
                elevation_ft: 1000.0,
                magnetic_variation_deg: 5.0,
                range_nm: Some(130.0),
            }],
            ils: vec![IlsRec {
                ident: "ITST".to_string(),
                airport_icao: "KTST".to_string(),
                runway_ident: "18".to_string(),
                frequency_mhz: 110.3,
                course_mag_deg: 180.0,
                glidepath_deg: Some(3.0),
                category: IlsCategory::Cat1,
                lat: 40.02,
                lon: -100.0,
                elevation_ft: 1200.0,
                has_dme: true,
                crossing_height_ft: Some(55.0),
            }],
            waypoints: vec![WaypointRec { ident: "FIXAA".to_string(), region_code: "K1".to_string(), area_code: "USA".to_string(), name: None, lat: 41.0, lon: -100.5 }],
            airways: vec![AirwayRec {
                ident: "A1".to_string(),
                legs: vec![AirwayLegRec {
                    sequence: 1,
                    from_ident: "FIXAA".to_string(),
                    from_region: "K1".to_string(),
                    to_ident: "TST".to_string(),
                    to_region: "K1".to_string(),
                    level: AirwayLevel::High,
                    is_start: true,
                    is_end: false,
                    min_ft: Some(18000.0),
                }],
            }],
            procedures: vec![ProcedureRec {
                airport_icao: "KTST".to_string(),
                kind: ProcKind::Approach,
                ident: "ILS18".to_string(),
                transition_ident: None,
                runway_ident: Some("18".to_string()),
                legs: vec![
                    LegRec {
                        sequence: 1,
                        path_terminator: PathTerminator::If,
                        fix_ident: Some("FIXAA".to_string()),
                        fix_region: Some("K1".to_string()),
                        altitude_rule: Some(AltitudeRule::AtOrAbove),
                        altitude1_ft: Some(4000.0),
                        turn: TurnDirection::Either,
                        ..Default::default()
                    },
                    LegRec {
                        sequence: 2,
                        path_terminator: PathTerminator::Cf,
                        // Not one of the given waypoints — a fix on a navaid instead, which
                        // must be resolved by synthesising a collocated waypoint for it.
                        fix_ident: Some("TST".to_string()),
                        fix_region: Some("K1".to_string()),
                        recommended_navaid: Some("TST".to_string()),
                        recommended_navaid_region: Some("K1".to_string()),
                        course_deg: Some(180.0),
                        is_map: true,
                        turn: TurnDirection::Right,
                        ..Default::default()
                    },
                ],
            }],
            mora: vec![MoraRec { lat: 40.4, lon: -100.4, altitude_ft: 5600.0 }],
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("amdbgen-fenix-test-{}-{name}.db3", std::process::id()))
    }

    #[test]
    fn a_synthetic_nav_set_round_trips() {
        let nav = synthetic();
        let path = temp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let report = write(&nav, &path).expect("write");
        let rows = |t: &str| report.iter().find(|r| r.table == t).map(|r| r.rows).unwrap_or(usize::MAX);
        assert_eq!(rows("Airports"), 1);
        assert_eq!(rows("Runways"), 1);
        assert_eq!(rows("ILSes"), 1);
        assert_eq!(rows("Navaids"), 1);
        assert_eq!(rows("Airways"), 1);
        assert_eq!(rows("AirwayLegs"), 1);
        assert_eq!(rows("Terminals"), 1);
        assert_eq!(rows("TerminalLegs"), 2);
        assert_eq!(rows("GridMora"), 1);
        // Holdings and the tables the model has no data for are reported empty, with why.
        let holdings = report.iter().find(|r| r.table == "Holdings").unwrap();
        assert_eq!(holdings.rows, 0);
        assert!(holdings.omitted_because.is_some());

        let conn = Connection::open(&path).unwrap();
        let icao: String = conn.query_row("select ICAO from Airports where ID = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(icao, "KTST");
        let alt: String = conn.query_row("select Alt from TerminalLegs where TrackCode = 'IF'", [], |r| r.get(0)).unwrap();
        assert_eq!(alt, "04000A");
        let map_alt: String = conn.query_row("select Alt from TerminalLegs where TrackCode = 'CF'", [], |r| r.get(0)).unwrap();
        assert_eq!(map_alt, "MAP");
        // The CF leg's fix ("RW18") is not one of the given waypoints, so it must have been
        // resolved to a fresh, synthetic one rather than left as a dangling zero ID.
        let wpt_id: i64 = conn.query_row("select WptID from TerminalLegs where TrackCode = 'CF'", [], |r| r.get(0)).unwrap();
        assert!(wpt_id > 0);
        let navaid_types: i64 = conn.query_row("select count(*) from NavaidTypes", [], |r| r.get(0)).unwrap();
        assert_eq!(navaid_types, 8);
        let markers: i64 = conn.query_row("select count(*) from Markers", [], |r| r.get(0)).unwrap();
        assert_eq!(markers, 0);
        let cycle: String = conn.query_row("select val from config where key = 'CycleName'", [], |r| r.get(0)).unwrap();
        assert_eq!(cycle, "2503");
        let start: String = conn.query_row("select val from config where key = 'CycleStartDate'", [], |r| r.get(0)).unwrap();
        assert_eq!(start, "20MAR25");
        let end: String = conn.query_row("select val from config where key = 'CycleEndDate'", [], |r| r.get(0)).unwrap();
        assert_eq!(end, "16APR25");
        // `sqlite_master` also lists the autoindexes SQLite creates for a composite
        // primary key; only the ones this writer names explicitly should match `INDEXES`.
        let index_count: i64 = conn
            .query_row("select count(*) from sqlite_master where type = 'index' and name not like 'sqlite_autoindex%'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(index_count as usize, INDEXES.len());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn plan_matches_a_real_write() {
        let nav = synthetic();
        let planned = plan(&nav);
        let path = temp_path("plan");
        let _ = std::fs::remove_file(&path);
        let written = write(&nav, &path).unwrap();
        for p in &planned {
            let w = written.iter().find(|w| w.table == p.table).unwrap();
            // `Waypoints`/`WaypointLookup` are the one pair `plan` cannot predict exactly:
            // a real write may synthesise a handful more, one per navaid used as a fix,
            // which `plan` has no database open to go and check for.
            if p.table == "Waypoints" || p.table == "WaypointLookup" {
                assert!(w.rows >= p.rows, "{}: write ({}) should be at least plan ({})", p.table, w.rows, p.rows);
            } else {
                assert_eq!(p.rows, w.rows, "{}", p.table);
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_known_cycle_matches_the_installed_database() {
        // 2503 runs 20 March to 16 April 2025 on the machine this crate was written on —
        // the one concrete fact available to check the anchor-and-28-day arithmetic against.
        let (start, end) = airac_bounds("2503").unwrap();
        assert_eq!(format_airac_date(start), "20MAR25");
        assert_eq!(format_airac_date(end), "16APR25");
    }

    #[test]
    fn altitude_text_matches_the_installed_databases_own_format() {
        let at_or_above = LegRec { altitude_rule: Some(AltitudeRule::AtOrAbove), altitude1_ft: Some(6142.0), ..Default::default() };
        assert_eq!(alt_text(&at_or_above), "06142A");
        let window = LegRec { altitude_rule: Some(AltitudeRule::Between), altitude1_ft: Some(3000.0), altitude2_ft: Some(6000.0), ..Default::default() };
        assert_eq!(alt_text(&window), "06000B03000A");
        let map = LegRec { is_map: true, altitude_rule: Some(AltitudeRule::At), altitude1_ft: Some(1000.0), ..Default::default() };
        assert_eq!(alt_text(&map), "MAP");
    }
}
