//! What a chart is made of besides the procedure: beacons, localisers, marker beacons,
//! published safe altitudes and published holds.
//!
//! None of it is in the simulator's own procedure data, which names a localiser without
//! saying where it is or what it is tuned to, and names a hold without saying which way
//! it turns. It is all, however, already on the machine — usually several times over —
//! because every serious add-on carries a navigation database of its own. So rather than
//! ask for any one of them, this reads whichever it finds, in order of how much each can
//! say:
//!
//! 1. **A navigation database shipped with an aircraft.** PMDG's `e_dfd_PMDG.s3db`, the
//!    A350's and the A220's `BundledData`, and anything else in the same shape: ARINC 424
//!    as SQLite, which carries the localisers with their courses and categories, the
//!    marker beacons, the published minimum safe altitude sectors and the published
//!    holding patterns. Nothing else has the last two at all.
//! 2. **X-Plane's `earth_nav.dat`**, if X-Plane is installed: beacons, localisers,
//!    glideslopes and DMEs worldwide, in a text file.
//! 3. **The simulator's own beacons**, which give a frequency and a position but no name.
//!
//! Each is read where it lies and nothing is copied out of it, exactly as this crate
//! already reads the simulator's own navigation data to draw a procedure at all.

use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Vor,
    Ndb,
}

/// A beacon, as a chart prints it: a name over a frequency and an ident.
#[derive(Debug, Clone)]
pub struct Beacon {
    pub ident: String,
    pub name: String,
    pub kind: Kind,
    /// Megahertz, except for an NDB, which is kilohertz.
    pub frequency: f64,
    pub lat: f64,
    pub lon: f64,
}

/// Where a marker beacon stands on the approach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Marker {
    Inner,
    Middle,
    Outer,
}

impl Marker {
    pub fn label(self) -> &'static str {
        match self {
            Marker::Inner => "IM",
            Marker::Middle => "MM",
            Marker::Outer => "OM",
        }
    }
}

/// What guides an approach down to one runway.
#[derive(Debug, Clone, Default)]
pub struct Ils {
    pub ident: String,
    pub frequency: f64,
    pub course_mag_deg: Option<f64>,
    pub glidepath_deg: Option<f64>,
    /// Where the distances printed against the fixes are measured from.
    pub dme: Option<(f64, f64)>,
    /// The category a chart prints beside it: I, II or III.
    pub category: Option<String>,
    pub markers: Vec<(Marker, f64, f64)>,
}

/// One sector of the published minimum safe altitude, as bearings *from* the centre.
#[derive(Debug, Clone)]
pub struct MsaSector {
    pub from_deg: f64,
    pub to_deg: f64,
    pub altitude_ft: f64,
}

/// The published safe altitude around an airport: what it is centred on, how far it
/// reaches, and each sector of it.
#[derive(Debug, Clone)]
pub struct Msa {
    pub centre: (f64, f64),
    pub centre_name: String,
    pub radius_nm: f64,
    pub sectors: Vec<MsaSector>,
}

/// A published holding pattern.
#[derive(Debug, Clone)]
pub struct Hold {
    pub fix: String,
    pub lat: f64,
    pub lon: f64,
    /// The course flown towards the fix, in magnetic degrees.
    pub inbound_deg: f64,
    pub right_turns: bool,
    pub leg_time_min: Option<f64>,
    pub leg_nm: Option<f64>,
    pub max_altitude_ft: Option<f64>,
    /// The lowest the hold may be flown: the MHA a chart prints.
    pub min_altitude_ft: Option<f64>,
    /// The fastest it may be flown, knots.
    pub speed_kt: Option<f64>,
}

/// What a runway record says about the runway itself.
#[derive(Debug, Clone, Copy)]
pub struct Runway {
    /// The height the glidepath crosses the threshold at, which a chart prints and which
    /// cannot be worked out from anything else.
    pub threshold_crossing_ft: Option<f64>,
    /// The published elevation of the landing threshold.
    pub threshold_elevation_ft: Option<f64>,
    pub magnetic_bearing_deg: Option<f64>,
}

/// The published record for one runway.
pub fn runway(icao: &str, runway: &str) -> Option<Runway> {
    database()?.runway(icao, runway)
}

/// The hold at a fix, preferring the one flown back towards a given place.
///
/// A fix can carry several published holds — one for an arrival, one for a missed
/// approach, one for an airway — and they are told apart by which way they are flown. A
/// missed approach holds on a course back towards the airport it has just left, because
/// that is the way the aircraft arrives at the fix.
pub fn hold_towards(fix: &str, from: (f64, f64)) -> Option<Hold> {
    let db = database()?;
    let holds = db.holds_at(fix);
    let best = holds.into_iter().min_by(|a, b| {
        let angle = |h: &Hold| {
            let cos = h.lat.to_radians().cos().max(0.05);
            let back = ((from.1 - h.lon) * 60.0 * cos).atan2((from.0 - h.lat) * 60.0).to_degrees();
            ((h.inbound_deg - back + 540.0) % 360.0 - 180.0).abs()
        };
        angle(a).total_cmp(&angle(b))
    })?;
    Some(best)
}

/// The beacons within a distance of a point, nearest first.
pub fn beacons_near(lat: f64, lon: f64, radius_nm: f64) -> Vec<Beacon> {
    if let Some(db) = database() {
        let found = db.beacons_near(lat, lon, radius_nm);
        if !found.is_empty() {
            return found;
        }
    }
    let from_xplane = crate::sources::xplane::navdata::within(lat, lon, radius_nm);
    if !from_xplane.is_empty() {
        return from_xplane
            .into_iter()
            .map(|b| Beacon {
                ident: b.ident,
                name: b.name,
                kind: if b.kind == crate::sources::xplane::navdata::Kind::Ndb { Kind::Ndb } else { Kind::Vor },
                frequency: b.frequency,
                lat: b.lat,
                lon: b.lon,
            })
            .collect();
    }
    // The simulator's own, which know no names.
    let cos = lat.to_radians().cos().max(0.05);
    let mut out: Vec<(f64, Beacon)> = crate::sources::msfs::navaids::index()
        .iter()
        .flat_map(|(ident, all)| all.iter().map(move |n| (ident, n)))
        .filter_map(|(ident, n)| {
            let nm = ((n.lat - lat) * 60.0).hypot((n.lon - lon) * 60.0 * cos);
            (nm <= radius_nm).then(|| {
                (
                    nm,
                    Beacon {
                        ident: ident.clone(),
                        name: String::new(),
                        kind: if n.kind == crate::sources::msfs::navaids::Kind::Ndb { Kind::Ndb } else { Kind::Vor },
                        frequency: n.frequency,
                        lat: n.lat,
                        lon: n.lon,
                    },
                )
            })
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.into_iter().map(|(_, b)| b).collect()
}

/// The localiser serving a runway, where one serves it.
pub fn ils(icao: &str, runway: &str) -> Option<Ils> {
    if let Some(found) = database().and_then(|db| db.ils(icao, runway)) {
        return Some(found);
    }
    let x = crate::sources::xplane::navdata::ils(icao, runway)?;
    Some(Ils {
        ident: x.ident,
        frequency: x.frequency,
        course_mag_deg: x.course_mag_deg,
        glidepath_deg: x.glidepath_deg,
        dme: x.dme,
        category: None,
        markers: Vec::new(),
    })
}

/// The published minimum safe altitude around an airport, where one is published.
/// The published safe altitude about a named beacon or fix at an airport, where there is
/// one about it.
pub fn msa_about(icao: &str, centre: &str) -> Option<Msa> {
    database()?.msa_about(icao, centre)
}

pub fn msa(icao: &str, at: (f64, f64)) -> Option<Msa> {
    database()?.msa(icao, at)
}

/// The published hold at a fix, where one is published.
pub fn hold_at(fix: &str) -> Option<Hold> {
    database()?.hold_at(fix)
}

/// Which navigation database is being read, for the chart to say so.
pub fn source() -> Option<String> {
    let db = database()?;
    Some(match &db.airac {
        Some(cycle) => format!("{} (AIRAC {cycle})", db.name),
        None => db.name.clone(),
    })
}

/// The database in use, found once.
fn database() -> Option<&'static Database> {
    static DB: OnceLock<Option<Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let found = Database::find();
        match &found {
            Some(db) => log::info!("navigation database: {} ({})", db.name, db.path.display()),
            None => log::debug!("no aircraft navigation database found"),
        }
        found
    })
    .as_ref()
}

/// A read-only connection to the navigation database in use, and the real name of the
/// table whose name ends with `wanted` ("airports", "runways", "fir_uir"), for a part of
/// the crate that has a question of its own to ask it.
pub(crate) fn open_table(wanted: &str) -> Option<(rusqlite::Connection, String)> {
    let db = database()?;
    let connection = read_only(&db.path)?;
    let name = {
        let mut statement = connection.prepare("select name from sqlite_master where type = 'table'").ok()?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0)).ok()?;
        let found = rows.flatten().find(|n| n.to_lowercase().ends_with(&wanted.to_lowercase()));
        found?
    };
    Some((connection, name))
}

/// The AIRAC cycle of the navigation database in use.
pub(crate) fn airac() -> Option<String> {
    database()?.airac.clone()
}

/// An ARINC 424 database in the shape the add-ons ship it.
///
/// Two spellings of the same thing are in the wild: the plain one PMDG uses
/// (`tbl_vhfnavaids`) and the one that prefixes every table with its ARINC section
/// (`tbl_d_vhfnavaids`). The columns are the same in both, so the tables are found by
/// what their names end with.
pub struct Database {
    path: PathBuf,
    name: String,
    airac: Option<String>,
    tables: std::collections::HashMap<String, String>,
}

impl Database {
    /// The best database on this machine: the newest cycle, or failing that the biggest
    /// file, which is the one with the most in it.
    fn find() -> Option<Database> {
        let mut best: Option<(String, u64, Database)> = None;
        for path in candidates() {
            let Some(db) = Database::open(&path) else { continue };
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let cycle = db.airac.clone().unwrap_or_default();
            let better = match &best {
                None => true,
                Some((cycle_so_far, size_so_far, _)) => (cycle.as_str(), size) > (cycle_so_far.as_str(), *size_so_far),
            };
            if better {
                best = Some((cycle, size, db));
            }
        }
        best.map(|(_, _, db)| db)
    }

    fn open(path: &std::path::Path) -> Option<Database> {
        let connection = read_only(path)?;
        let mut tables = std::collections::HashMap::new();
        {
            let mut statement = connection.prepare("select name from sqlite_master where type = 'table'").ok()?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0)).ok()?;
            for name in rows.flatten() {
                // "tbl_pi_localizers_glideslopes" and "tbl_localizers_glideslopes" are the
                // same table, so each is filed under what it ends with.
                let lower = name.to_lowercase();
                for wanted in WANTED {
                    if lower.ends_with(wanted) {
                        tables.insert((*wanted).to_string(), name.clone());
                    }
                }
            }
        }
        if !tables.contains_key("vhfnavaids") {
            return None;
        }
        // The cycle, where the database says: four digits, year then cycle of the year.
        let airac = tables.get("header").and_then(|table| {
            connection
                .query_row(&format!("select current_airac from \"{table}\" limit 1"), [], |row| row.get::<_, String>(0))
                .ok()
        });
        let name = path
            .components()
            .rev()
            .find_map(|c| {
                let part = c.as_os_str().to_string_lossy().to_string();
                (part.contains('-') && !part.ends_with(".s3db")).then_some(part)
            })
            .unwrap_or_else(|| path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        Some(Database { path: path.to_path_buf(), name, airac, tables })
    }

    fn table(&self, wanted: &str) -> Option<&str> {
        self.tables.get(wanted).map(String::as_str)
    }

    fn beacons_near(&self, lat: f64, lon: f64, radius_nm: f64) -> Vec<Beacon> {
        let connection = match read_only(&self.path) {
            Some(c) => c,
            None => return Vec::new(),
        };
        let cos = lat.to_radians().cos().max(0.05);
        let (dlat, dlon) = (radius_nm / 60.0, radius_nm / 60.0 / cos);
        let mut out: Vec<(f64, Beacon)> = Vec::new();
        let mut gather = |table: &str, ident: &str, name: &str, frequency: &str, lat_col: &str, lon_col: &str, kind: Kind, only_beacons: bool| {
            // The table of VHF transmitters holds every localiser's DME as well, which is
            // not a beacon and must not be drawn as one: a chart would show a hexagon
            // called IJFK sitting on the runway. They are told apart by their class — a
            // beacon's begins with a V, a localiser's is an I.
            let filter = if only_beacons { " and (navaid_class is null or trim(navaid_class) like 'V%')" } else { "" };
            let sql = format!(
                "select {ident}, {name}, {frequency}, {lat_col}, {lon_col} from \"{table}\" 
                 where {lat_col} between ?1 and ?2 and {lon_col} between ?3 and ?4{filter}"
            );
            let Ok(mut statement) = connection.prepare(&sql) else { return };
            let rows = statement.query_map([lat - dlat, lat + dlat, lon - dlon, lon + dlon], |row| {
                Ok(Beacon {
                    ident: row.get::<_, String>(0).unwrap_or_default(),
                    name: tidy_name(&row.get::<_, String>(1).unwrap_or_default()),
                    kind,
                    frequency: row.get::<_, f64>(2).unwrap_or_default(),
                    lat: row.get::<_, f64>(3)?,
                    lon: row.get::<_, f64>(4)?,
                })
            });
            let Ok(rows) = rows else { return };
            for beacon in rows.flatten() {
                if beacon.ident.is_empty() {
                    continue;
                }
                let nm = ((beacon.lat - lat) * 60.0).hypot((beacon.lon - lon) * 60.0 * cos);
                if nm <= radius_nm {
                    out.push((nm, beacon));
                }
            }
        };
        if let Some(table) = self.table("vhfnavaids") {
            gather(table, "vor_identifier", "vor_name", "vor_frequency", "vor_latitude", "vor_longitude", Kind::Vor, true);
        }
        if let Some(table) = self.table("enroute_ndbnavaids") {
            gather(table, "ndb_identifier", "ndb_name", "ndb_frequency", "ndb_latitude", "ndb_longitude", Kind::Ndb, false);
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.dedup_by(|a, b| a.1.ident == b.1.ident);
        out.into_iter().map(|(_, b)| b).collect()
    }

    fn ils(&self, icao: &str, runway: &str) -> Option<Ils> {
        let connection = read_only(&self.path)?;
        let table = self.table("localizers_glideslopes")?;
        let runway = format!("RW{}", runway.trim().trim_start_matches("RW").to_uppercase());
        let short = format!("RW{}", runway.trim_start_matches("RW").trim_start_matches('0'));
        let sql = format!(
            "select llz_identifier, llz_frequency, llz_bearing, gs_angle, llz_latitude, llz_longitude, ils_mls_gls_category 
             from \"{table}\" where airport_identifier = ?1 and (runway_identifier = ?2 or runway_identifier = ?3) limit 1"
        );
        let found = connection
            .query_row(&sql, [icao.to_uppercase().as_str(), runway.as_str(), short.as_str()], |row| {
                Ok(Ils {
                    ident: row.get::<_, String>(0).unwrap_or_default(),
                    frequency: row.get::<_, f64>(1).unwrap_or_default(),
                    course_mag_deg: row.get::<_, f64>(2).ok(),
                    glidepath_deg: row.get::<_, f64>(3).ok().filter(|a| *a > 0.5),
                    // The distances on an ILS approach are measured from the localiser
                    // itself where the database says nothing more precise.
                    dme: Some((row.get::<_, f64>(4)?, row.get::<_, f64>(5)?)),
                    category: row.get::<_, String>(6).ok().map(|c| category(&c)),
                    markers: Vec::new(),
                })
            })
            .ok()?;
        let markers = self.markers(&connection, icao, &runway, &short);
        Some(Ils { markers, ..found })
    }

    fn markers(&self, connection: &rusqlite::Connection, icao: &str, runway: &str, short: &str) -> Vec<(Marker, f64, f64)> {
        let Some(table) = self.table("localizer_marker") else { return Vec::new() };
        let sql = format!(
            "select marker_type, marker_latitude, marker_longitude from \"{table}\" 
             where airport_identifier = ?1 and (runway_identifier = ?2 or runway_identifier = ?3)"
        );
        let Ok(mut statement) = connection.prepare(&sql) else { return Vec::new() };
        let rows = statement.query_map([icao.to_uppercase().as_str(), runway, short], |row| {
            let kind = match row.get::<_, String>(0).unwrap_or_default().trim().to_uppercase().as_str() {
                "IM" => Marker::Inner,
                "MM" => Marker::Middle,
                _ => Marker::Outer,
            };
            Ok((kind, row.get::<_, f64>(1)?, row.get::<_, f64>(2)?))
        });
        rows.map(|r| r.flatten().collect()).unwrap_or_default()
    }

    fn msa_about(&self, icao: &str, centre: &str) -> Option<Msa> {
        self.msa_where(icao, (0.0, 0.0), Some(centre))
    }

    fn msa(&self, icao: &str, at: (f64, f64)) -> Option<Msa> {
        self.msa_where(icao, at, None)
    }

    fn msa_where(&self, icao: &str, at: (f64, f64), centre: Option<&str>) -> Option<Msa> {
        let connection = read_only(&self.path)?;
        let table = self.table("airport_msa")?;
        // An airport carries one of these for each way in — one per runway, one per
        // arrival waypoint, one about a beacon. They are not all worth printing: the one
        // centred on a beacon and split into sectors is what a chart shows, because a
        // single figure for the whole circle carries the highest ground all the way round.
        let sql = format!(
            "select msa_center, msa_center_latitude, msa_center_longitude, radius_limit, 
             sector_bearing_1, sector_altitude_1, sector_bearing_2, sector_altitude_2, 
             sector_bearing_3, sector_altitude_3, sector_bearing_4, sector_altitude_4, 
             sector_bearing_5, sector_altitude_5 from \"{table}\" where airport_identifier = ?1 {}
             order by (sector_bearing_5 is not null) + (sector_bearing_4 is not null) 
             + (sector_bearing_3 is not null) + (sector_bearing_2 is not null) desc, 
             (msa_center like 'RW%') asc,              (msa_center_latitude - ?2) * (msa_center_latitude - ?2)              + (msa_center_longitude - ?3) * (msa_center_longitude - ?3) asc limit 1",
            if centre.is_some() { "and trim(msa_center) = ?4" } else { "and ?4 is not null" }
        );
        connection
            .query_row(&sql, rusqlite::params![icao.to_uppercase(), at.0, at.1, centre.unwrap_or("").to_uppercase()], |row| {
                let mut sectors = Vec::new();
                for i in 0..5 {
                    let bearing: Option<f64> = row.get(4 + i * 2).ok();
                    let altitude: Option<f64> = row.get(5 + i * 2).ok();
                    if let (Some(bearing), Some(altitude)) = (bearing, altitude) {
                        // The altitude is in hundreds of feet.
                        sectors.push(MsaSector { from_deg: bearing, to_deg: bearing, altitude_ft: altitude * 100.0 });
                    }
                }
                // Each sector runs to where the next begins, the last wrapping round. One
                // sector on its own is the whole circle.
                let count = sectors.len();
                for i in 0..count {
                    sectors[i].to_deg = if count == 1 { sectors[i].from_deg + 360.0 } else { sectors[(i + 1) % count].from_deg };
                }
                Ok(Msa {
                    centre_name: row.get::<_, String>(0).unwrap_or_default(),
                    centre: (row.get::<_, f64>(1)?, row.get::<_, f64>(2)?),
                    radius_nm: row.get::<_, f64>(3).unwrap_or(25.0),
                    sectors,
                })
            })
            .ok()
            .filter(|m| !m.sectors.is_empty())
    }

    fn runway(&self, icao: &str, runway: &str) -> Option<Runway> {
        let connection = read_only(&self.path)?;
        let table = self.table("runways")?;
        let wanted = format!("RW{}", runway.trim().trim_start_matches("RW").to_uppercase());
        let short = format!("RW{}", wanted.trim_start_matches("RW").trim_start_matches('0'));
        let sql = format!(
            "select threshold_crossing_height, landing_threshold_elevation, runway_magnetic_bearing \
             from \"{table}\" where airport_identifier = ?1 and (runway_identifier = ?2 or runway_identifier = ?3) limit 1"
        );
        connection
            .query_row(&sql, [icao.to_uppercase().as_str(), wanted.as_str(), short.as_str()], |row| {
                Ok(Runway {
                    threshold_crossing_ft: row.get::<_, f64>(0).ok().filter(|v| *v > 5.0 && *v < 200.0),
                    threshold_elevation_ft: row.get::<_, f64>(1).ok(),
                    magnetic_bearing_deg: row.get::<_, f64>(2).ok(),
                })
            })
            .ok()
    }

    /// Every hold published at a fix.
    fn holds_at(&self, fix: &str) -> Vec<Hold> {
        let Some(connection) = read_only(&self.path) else { return Vec::new() };
        let Some(table) = self.table("holdings") else { return Vec::new() };
        // The minimum altitude and speed are in the databases that have them; the query
        // falls back to without them for one that does not.
        for extra in [true, false] {
            let Ok(mut statement) = connection.prepare(&hold_sql(table, extra)) else { continue };
            let rows = statement.query_map([fix.to_uppercase()], |row| hold_row(row, extra));
            return rows.map(|r| r.flatten().collect()).unwrap_or_default();
        }
        Vec::new()
    }

    fn hold_at(&self, fix: &str) -> Option<Hold> {
        self.holds_at(fix).into_iter().next()
    }
}

impl Database {
    /// Where each of a list of fixes is, taking the one nearest a point where a name is
    /// used more than once in the world, as five-letter names are.
    fn fixes_near(&self, idents: &[String], near: (f64, f64)) -> std::collections::HashMap<String, (f64, f64)> {
        let mut out = std::collections::HashMap::new();
        let Some(connection) = read_only(&self.path) else { return out };
        let cos = near.0.to_radians().cos().max(0.05);
        let far = |lat: f64, lon: f64| ((lat - near.0) * 60.0).hypot((lon - near.1) * 60.0 * cos);
        let sources = [
            ("terminal_waypoints", "waypoint_identifier", "waypoint_latitude", "waypoint_longitude"),
            ("enroute_waypoints", "waypoint_identifier", "waypoint_latitude", "waypoint_longitude"),
            ("vhfnavaids", "vor_identifier", "vor_latitude", "vor_longitude"),
            ("enroute_ndbnavaids", "ndb_identifier", "ndb_latitude", "ndb_longitude"),
            ("terminal_ndbnavaids", "ndb_identifier", "ndb_latitude", "ndb_longitude"),
        ];
        for ident in idents {
            let mut best: Option<(f64, (f64, f64))> = None;
            for (wanted, id_col, lat_col, lon_col) in sources {
                let Some(table) = self.table(wanted) else { continue };
                let sql = format!("select {lat_col}, {lon_col} from \"{table}\" where {id_col} = ?1");
                let Ok(mut statement) = connection.prepare_cached(&sql) else { continue };
                let Ok(rows) = statement.query_map([ident.as_str()], |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?))) else { continue };
                for (lat, lon) in rows.flatten() {
                    let d = far(lat, lon);
                    if best.map_or(true, |(b, _)| d < b) {
                        best = Some((d, (lat, lon)));
                    }
                }
            }
            // Further than a procedure reaches is a different fix of the same name.
            if let Some((_, at)) = best.filter(|(d, _)| *d < 400.0) {
                out.insert(ident.clone(), at);
            }
        }
        out
    }

    fn runway_threshold(&self, icao: &str, runway: &str) -> Option<(f64, f64)> {
        let connection = read_only(&self.path)?;
        let table = self.table("runways")?;
        let wanted = format!("RW{}", runway.trim().trim_start_matches("RW").to_uppercase());
        let sql = format!("select runway_latitude, runway_longitude from \"{table}\" where airport_identifier = ?1 and runway_identifier = ?2 limit 1");
        connection.query_row(&sql, [icao.to_uppercase().as_str(), wanted.as_str()], |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?))).ok()
    }
}

/// One segment of an airway: from a fix to the next along it, and what it may be flown
/// at. `one_way` is true when it may only be flown from `from` to `to`.
#[derive(Debug, Clone)]
pub struct AirwaySegment {
    pub airway: String,
    pub from: (String, f64, f64),
    pub to: (String, f64, f64),
    pub one_way: bool,
    /// Minimum enroute altitude, feet.
    pub min_ft: Option<f64>,
    /// The highest the airway may be flown, feet.
    pub max_ft: Option<f64>,
}

/// Every airway segment in the navigation database: the network a route is planned on.
/// Every airway segment in the navigation database, kept on disk between runs.
///
/// Reading the ninety thousand rows the table holds takes a tenth of a second, which is
/// nothing against the forty seconds this module used to spend elsewhere and a great deal
/// against the two milliseconds a route search itself needs. The rows cannot change while a
/// navigation database stays where it is, so they are written once in a flat binary form —
/// strings as a length and their bytes, numbers little-endian — and read back after. The key
/// carries the AIRAC cycle and the file's own size, so a database swapped for a newer cycle,
/// or replaced in place, is read afresh rather than answered from a stale file.
pub fn airway_segments() -> Vec<AirwaySegment> {
    let Some(db) = database() else { return Vec::new() };
    let Some(key) = cache_key(db, "airways") else { return db.airway_segments() };
    let store = crate::cache::Cache::for_index(false);
    match store.get_or_fetch_bytes(&key, || Ok(encode_segments(&db.airway_segments()))) {
        Ok(bytes) => match decode_segments(&bytes) {
            Some(found) if !found.is_empty() => found,
            // A file written by an older layout, or a truncated one: read the database.
            _ => db.airway_segments(),
        },
        Err(_) => db.airway_segments(),
    }
}

/// Where the flattened route network is kept between runs, keyed like everything else read
/// whole out of one navigation database.
pub fn graph_cache_key() -> Option<String> {
    cache_key(database()?, "route-graph")
}

/// A cache key for something read whole out of one navigation database: the cycle it
/// publishes, and the file's own length, so that a database replaced in place without the
/// cycle changing is still noticed.
fn cache_key(db: &Database, what: &str) -> Option<String> {
    let airac = db.airac.clone().unwrap_or_else(|| "none".to_string());
    let size = std::fs::metadata(&db.path).map(|m| m.len()).unwrap_or(0);
    (size > 0).then(|| format!("navdata/{what}-{airac}-{size}.bin"))
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u16).to_le_bytes());
    out.extend(s.as_bytes());
}

fn put_opt_f64(out: &mut Vec<u8>, v: Option<f64>) {
    // Not-a-number stands for absent, which no real altitude ever is.
    out.extend(v.unwrap_or(f64::NAN).to_le_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.bytes.get(self.at..self.at + n)?;
        self.at += n;
        Some(out)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn opt_f64(&mut self) -> Option<Option<f64>> {
        let v = self.f64()?;
        Some((!v.is_nan()).then_some(v))
    }

    fn string(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

const SEGMENTS_MAGIC: u32 = 0x414d_4457;

fn encode_segments(segments: &[AirwaySegment]) -> Vec<u8> {
    let mut out = Vec::with_capacity(segments.len() * 48 + 8);
    out.extend(SEGMENTS_MAGIC.to_le_bytes());
    out.extend((segments.len() as u32).to_le_bytes());
    for s in segments {
        put_str(&mut out, &s.airway);
        for (ident, lat, lon) in [&s.from, &s.to] {
            put_str(&mut out, ident);
            out.extend(lat.to_le_bytes());
            out.extend(lon.to_le_bytes());
        }
        out.push(u8::from(s.one_way));
        put_opt_f64(&mut out, s.min_ft);
        put_opt_f64(&mut out, s.max_ft);
    }
    out
}

fn decode_segments(bytes: &[u8]) -> Option<Vec<AirwaySegment>> {
    let mut r = Reader { bytes, at: 0 };
    if r.u32()? != SEGMENTS_MAGIC {
        return None;
    }
    let n = r.u32()? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let airway = r.string()?;
        let from = (r.string()?, r.f64()?, r.f64()?);
        let to = (r.string()?, r.f64()?, r.f64()?);
        let one_way = *r.take(1)?.first()? != 0;
        out.push(AirwaySegment { airway, from, to, one_way, min_ft: r.opt_f64()?, max_ft: r.opt_f64()? });
    }
    Some(out)
}

/// Every enroute waypoint the navigation database knows of, by identifier and position —
/// not only the ones an airway happens to pass through. Most of these are already reached
/// by some segment `airway_segments` returns; a good many are not, and are exactly what a
/// free-route direct leg over open ocean needs somewhere to land on: the reporting points
/// (`5945N`, `6050N`, and the like) a North Atlantic track is built from message by message,
/// which persist in the database, five-letter names and lat/lon points alike, long after the
/// message that once joined a given day's tracks between them has expired.
pub fn enroute_waypoints() -> Vec<(String, f64, f64)> {
    database().map(|db| db.enroute_waypoints()).unwrap_or_default()
}

impl Database {
    fn airway_segments(&self) -> Vec<AirwaySegment> {
        let mut out = Vec::new();
        let Some(connection) = read_only(&self.path) else { return out };
        let Some(table) = self.table("enroute_airways") else { return out };
        let sql = format!(
            "select route_identifier, area_code, seqno, waypoint_identifier, waypoint_latitude, waypoint_longitude, \
             waypoint_description_code, direction_restriction, minimum_altitude1, maximum_altitude \
             from \"{table}\" order by route_identifier, area_code, seqno"
        );
        let Ok(mut statement) = connection.prepare(&sql) else { return out };
        #[allow(clippy::type_complexity)]
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, String>(3)?,
                row.get::<_, f64>(4)?,
                row.get::<_, f64>(5)?,
                row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                row.get::<_, Option<f64>>(8)?,
                row.get::<_, Option<f64>>(9)?,
            ))
        });
        let Ok(rows) = rows else { return out };
        let rows: Vec<_> = rows.flatten().collect();
        for pair in rows.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            // The same airway in the same part of the world, and not past its end: the
            // second character of the description marks the last fix of a stretch.
            if a.0 != b.0 || a.1 != b.1 || a.5.chars().nth(1) == Some('E') {
                continue;
            }
            // A pair of fixes an ocean apart is two stretches of one name, not a segment.
            let cos = a.3.to_radians().cos().max(0.05);
            if ((b.3 - a.3) * 60.0).hypot((b.4 - a.4) * 60.0 * cos) > 1200.0 {
                continue;
            }
            // Forward means in the order the airway is listed; backward the other way.
            let (from, to, one_way) = match b.6.trim() {
                "F" => ((a.2.clone(), a.3, a.4), (b.2.clone(), b.3, b.4), true),
                "B" => ((b.2.clone(), b.3, b.4), (a.2.clone(), a.3, a.4), true),
                _ => ((a.2.clone(), a.3, a.4), (b.2.clone(), b.3, b.4), false),
            };
            out.push(AirwaySegment {
                airway: a.0.clone(),
                from,
                to,
                one_way,
                min_ft: b.7.filter(|v| *v > 0.0 && *v < 60000.0),
                max_ft: b.8.filter(|v| *v > 0.0 && *v < 99000.0),
            });
        }
        out
    }

    fn enroute_waypoints(&self) -> Vec<(String, f64, f64)> {
        let mut out = Vec::new();
        let Some(connection) = read_only(&self.path) else { return out };
        let Some(table) = self.table("enroute_waypoints") else { return out };
        let sql = format!("select waypoint_identifier, waypoint_latitude, waypoint_longitude from \"{table}\"");
        let Ok(mut statement) = connection.prepare(&sql) else { return out };
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?, row.get::<_, f64>(2)?)));
        let Ok(rows) = rows else { return out };
        out.extend(rows.flatten());
        out
    }
}

/// What the database says of an airport as a whole.
#[derive(Debug, Clone, Default)]
pub struct AirportInfo {
    pub transition_altitude_ft: Option<f64>,
    pub transition_level_ft: Option<f64>,
    /// The speed limit below an altitude: 250 KT below 10,000 FT, most places.
    pub speed_limit: Option<(f64, f64)>,
}

/// One piece of controlled airspace: its class, name and limits, and its boundary as a
/// closed line of points with the arcs worked out.
#[derive(Debug, Clone)]
pub struct Airspace {
    pub class: String,
    pub name: String,
    pub lower: String,
    pub upper: String,
    pub boundary: Vec<(f64, f64)>,
}

/// A frequency the way a chart prints it: what it is, the name it is called by, and the
/// frequency.
#[derive(Debug, Clone)]
pub struct Communication {
    pub kind: String,
    pub callsign: String,
    pub mhz: f64,
}

pub fn airport_info(icao: &str) -> Option<AirportInfo> {
    database()?.airport_info(icao)
}

/// The grid minimum off-route altitudes over an area: (south edge, west edge, feet) for
/// each one-degree square.
pub fn grid_mora(south: f64, north: f64, west: f64, east: f64) -> Vec<(f64, f64, f64)> {
    database().map(|db| db.grid_mora(south, north, west, east)).unwrap_or_default()
}

/// The controlled airspace an airport sits in.
pub fn airspace(icao: &str) -> Vec<Airspace> {
    database().map(|db| db.airspace(icao)).unwrap_or_default()
}

/// Every frequency an airport publishes.
pub fn communications(icao: &str) -> Vec<Communication> {
    database().map(|db| db.communications(icao)).unwrap_or_default()
}

impl Database {
    fn airport_info(&self, icao: &str) -> Option<AirportInfo> {
        let connection = read_only(&self.path)?;
        let table = self.table("airports")?;
        let sql = format!("select transition_altitude, transition_level, speed_limit, speed_limit_altitude from \"{table}\" where airport_identifier = ?1 limit 1");
        connection
            .query_row(&sql, [icao.to_uppercase()], |row| {
                let limit: Option<f64> = row.get(2).ok();
                let below: Option<f64> = row.get(3).ok().or_else(|| row.get::<_, String>(3).ok().and_then(|s| s.trim().trim_start_matches("FL").parse::<f64>().ok().map(|v| if v < 1000.0 { v * 100.0 } else { v })));
                Ok(AirportInfo {
                    transition_altitude_ft: row.get::<_, f64>(0).ok().filter(|v| *v > 0.0),
                    transition_level_ft: row.get::<_, f64>(1).ok().filter(|v| *v > 0.0),
                    speed_limit: limit.zip(below).filter(|(s, a)| *s > 0.0 && *a > 0.0),
                })
            })
            .ok()
    }

    fn grid_mora(&self, south: f64, north: f64, west: f64, east: f64) -> Vec<(f64, f64, f64)> {
        let mut out = Vec::new();
        let Some(connection) = read_only(&self.path) else { return out };
        let Some(table) = self.table("grid_mora") else { return out };
        let columns: String = (1..=30).map(|i| format!("mora{i:02}")).collect::<Vec<_>>().join(", ");
        let sql = format!("select starting_latitude, starting_longitude, {columns} from \"{table}\" where starting_latitude between ?1 and ?2");
        let Ok(mut statement) = connection.prepare(&sql) else { return out };
        let rows = statement.query_map([south.floor() - 1.0, north.ceil()], |row| {
            let lat: f64 = row.get(0)?;
            let lon: f64 = row.get(1)?;
            let values: Vec<Option<String>> = (0..30).map(|i| row.get::<_, Option<String>>(2 + i).ok().flatten()).collect();
            Ok((lat, lon, values))
        });
        let Ok(rows) = rows else { return out };
        for (lat, lon0, values) in rows.flatten() {
            if lat + 1.0 < south || lat > north {
                continue;
            }
            for (i, v) in values.into_iter().enumerate() {
                let lon = lon0 + i as f64;
                if lon + 1.0 < west || lon > east {
                    continue;
                }
                // Hundreds of feet; "UNK" and blanks are squares nobody has surveyed.
                if let Some(ft) = v.and_then(|s| s.trim().parse::<f64>().ok()).filter(|h| *h > 0.0) {
                    out.push((lat, lon, ft * 100.0));
                }
            }
        }
        // West of 90° W the tables in the wild carry each band of squares several times
        // over with different figures, and nothing says which is meant. The highest is
        // taken: an altitude that is meant to clear everything is never made unsafe by
        // being too high.
        out.sort_by(|a, b| (a.0, a.1).partial_cmp(&(b.0, b.1)).unwrap_or(std::cmp::Ordering::Equal).then(b.2.total_cmp(&a.2)));
        out.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        out
    }

    fn airspace(&self, icao: &str) -> Vec<Airspace> {
        let mut out = Vec::new();
        let Some(connection) = read_only(&self.path) else { return out };
        let Some(table) = self.table("controlled_airspace") else { return out };
        let sql = format!(
            "select multiple_code, airspace_classification, controlled_airspace_name, boundary_via, latitude, longitude, \
             arc_origin_latitude, arc_origin_longitude, arc_distance, lower_limit, upper_limit \
             from \"{table}\" where airspace_center = ?1 order by multiple_code, seqno"
        );
        let Ok(mut statement) = connection.prepare(&sql) else { return out };
        #[allow(clippy::type_complexity)]
        let rows = statement.query_map([icao.to_uppercase()], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, Option<f64>>(6)?,
                row.get::<_, Option<f64>>(7)?,
                row.get::<_, Option<f64>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        });
        let Ok(rows) = rows else { return out };
        let rows: Vec<_> = rows.flatten().collect();
        let mut i = 0;
        while i < rows.len() {
            let group = rows[i].0.clone();
            let mut part = Vec::new();
            while i < rows.len() && rows[i].0 == group {
                part.push(rows[i].clone());
                i += 1;
            }
            let first = &part[0];
            let mut boundary: Vec<(f64, f64)> = Vec::new();
            for (k, r) in part.iter().enumerate() {
                let via = r.3.trim().to_uppercase();
                let next = part.get(k + 1).or(part.first()).and_then(|n| n.4.zip(n.5));
                let here = r.4.zip(r.5);
                match via.chars().next() {
                    // A whole circle about a point.
                    Some('C') => {
                        if let (Some(o), Some(radius)) = (r.6.zip(r.7), r.8) {
                            boundary.extend(arc(o, radius, 0.0, 360.0, true));
                        }
                    }
                    // An arc from here to the next point, one way or the other about its origin.
                    Some(turn @ ('R' | 'L')) => {
                        if let (Some(h), Some(n), Some(o), Some(radius)) = (here, next, r.6.zip(r.7), r.8) {
                            let (from, to) = (bearing(o, h), bearing(o, n));
                            boundary.extend(arc(o, radius, from, to, turn == 'R'));
                        }
                    }
                    _ => {
                        if let Some(h) = here {
                            boundary.push(h);
                        }
                    }
                }
            }
            if boundary.len() >= 3 {
                out.push(Airspace {
                    class: first.1.trim().to_string(),
                    name: first.2.clone().unwrap_or_default().trim().to_string(),
                    lower: first.9.clone().unwrap_or_default(),
                    upper: first.10.clone().unwrap_or_default(),
                    boundary,
                });
            }
        }
        out
    }

    fn communications(&self, icao: &str) -> Vec<Communication> {
        let Some(connection) = read_only(&self.path) else { return Vec::new() };
        let Some(table) = self.table("airport_communication") else { return Vec::new() };
        let sql = format!("select communication_type, callsign, communication_frequency from \"{table}\" where airport_identifier = ?1");
        let Ok(mut statement) = connection.prepare(&sql) else { return Vec::new() };
        let rows = statement.query_map([icao.to_uppercase()], |row| {
            Ok(Communication {
                kind: row.get::<_, Option<String>>(0)?.unwrap_or_default().trim().to_string(),
                callsign: row.get::<_, Option<String>>(1)?.unwrap_or_default().trim().to_string(),
                mhz: row.get::<_, f64>(2)?,
            })
        });
        rows.map(|r| r.flatten().collect()).unwrap_or_default()
    }
}

fn hold_sql(table: &str, extra: bool) -> String {
    format!(
        "select waypoint_identifier, waypoint_latitude, waypoint_longitude, inbound_holding_course, \
         turn_direction, leg_time, leg_length, maximum_altitude{} from \"{table}\" where waypoint_identifier = ?1",
        if extra { ", minimum_altitude, holding_speed" } else { "" }
    )
}

fn hold_row(row: &rusqlite::Row, extra: bool) -> rusqlite::Result<Hold> {
    Ok(Hold {
        fix: row.get::<_, String>(0).unwrap_or_default(),
        lat: row.get::<_, f64>(1)?,
        lon: row.get::<_, f64>(2)?,
        inbound_deg: row.get::<_, f64>(3).unwrap_or_default(),
        right_turns: !row.get::<_, String>(4).unwrap_or_default().eq_ignore_ascii_case("L"),
        leg_time_min: row.get::<_, f64>(5).ok().filter(|v| *v > 0.0),
        leg_nm: row.get::<_, f64>(6).ok().filter(|v| *v > 0.0),
        max_altitude_ft: row.get::<_, f64>(7).ok().filter(|v| *v > 0.0),
        min_altitude_ft: if extra { row.get::<_, f64>(8).ok().filter(|v| *v > 0.0) } else { None },
        speed_kt: if extra { row.get::<_, f64>(9).ok().filter(|v| *v > 0.0) } else { None },
    })
}

/// One row of a boundary as the ARINC tables give it: how it runs on from this point
/// (great circle, rhumb line, clockwise or anticlockwise arc, whole circle), the point,
/// and the arc's origin and radius where it is an arc.
struct BoundaryRow {
    via: String,
    at: Option<(f64, f64)>,
    arc_origin: Option<(f64, f64)>,
    arc_nm: Option<f64>,
}

/// A boundary's rows as a closed line of points, the arcs laid out.
fn boundary(rows: &[BoundaryRow]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (k, r) in rows.iter().enumerate() {
        let next = rows.get(k + 1).or(rows.first()).and_then(|n| n.at);
        match r.via.trim().to_uppercase().chars().next() {
            Some('C') => {
                if let (Some(o), Some(radius)) = (r.arc_origin, r.arc_nm) {
                    out.extend(arc(o, radius, 0.0, 360.0, true));
                }
            }
            Some(turn @ ('R' | 'L')) => {
                if let (Some(h), Some(n), Some(o), Some(radius)) = (r.at, next, r.arc_origin, r.arc_nm) {
                    out.extend(arc(o, radius, bearing(o, h), bearing(o, n), turn == 'R'));
                }
            }
            _ => {
                if let Some(h) = r.at {
                    out.push(h);
                }
            }
        }
    }
    out
}

/// An airspace limit as the tables write it, in feet: "GND", "UNLTD", "FL275", "06000".
fn limit_ft(s: &str) -> Option<f64> {
    let s = s.trim().to_uppercase();
    match s.as_str() {
        "" => None,
        "GND" | "SFC" => Some(0.0),
        "UNLTD" | "UNL" | "NOTSP" => Some(99_999.0),
        _ => match s.strip_prefix("FL") {
            Some(fl) => fl.parse::<f64>().ok().map(|v| v * 100.0),
            None => s.parse::<f64>().ok(),
        },
    }
}

/// A flight information region's outline: every part of it, as closed lines of points.
///
/// ARINC's `fir_uir` table carries three kinds of row under one identifier: the FIR itself
/// (indicator `F`), its UIR (`U`), a single boundary that stands for both at once (`B`),
/// and — read here but never turned into a region of its own — a portion delegated to or
/// from a neighbour (`C`, and `A` for the delegations an oceanic control area makes). A
/// delegated portion's boundary overlaps the region it is carved out of and its "name" is
/// really a note ("DELEGATED BY EGTT TO LFFF"); folding it in as though it were a region
/// is what used to make a route flicker between two identifiers crossing a border and
/// print that note as the region's own name.
#[derive(Debug, Clone)]
pub struct Region {
    pub ident: String,
    /// The FIR's name where the region has one, else the UIR's: a single label regardless
    /// of level, for a caller such as `route::mod`'s "avoid this FIR" that wants the
    /// region as a whole rather than to read it by altitude.
    pub name: String,
    /// Every part of the region, its FIR and UIR layers together, floor to ceiling: for
    /// the same whole-region callers `name` serves.
    pub parts: Vec<Vec<(f64, f64)>>,
    /// The FIR and the UIR taken apart, each under its own name and vertical band — these
    /// do differ (Paris's FIR is named "PARIS", its UIR "FRANCE") — for a route report
    /// that must say which one a level actually crosses rather than print both.
    pub layers: Vec<RegionLayer>,
}

/// One vertical layer of a region: the FIR (surface up to its ceiling), the UIR (from
/// there up), or, where ARINC gives one boundary for both, the same boundary twice, once
/// for each band.
#[derive(Debug, Clone)]
pub struct RegionLayer {
    pub name: String,
    pub floor_ft: f64,
    pub ceiling_ft: f64,
    pub parts: Vec<Vec<(f64, f64)>>,
}

/// An area it is prohibited, restricted or dangerous to fly through, and the band of
/// altitudes it fills. `kind` is the ARINC letter: P prohibited, R restricted, D danger,
/// W warning, A alert, M military operations, T training, and so on.
#[derive(Debug, Clone)]
pub struct Restricted {
    pub designation: String,
    pub name: String,
    pub kind: char,
    pub lower_ft: f64,
    pub upper_ft: f64,
    pub boundary: Vec<(f64, f64)>,
}

/// The outlines of the flight information regions named, however many parts each has.
pub fn regions(idents: &[String]) -> Vec<Region> {
    database().map(|db| db.regions(idents)).unwrap_or_default()
}

/// Every restricted area with a corner inside a box (south, north, west, east).
pub fn restricted_areas(south: f64, north: f64, west: f64, east: f64) -> Vec<Restricted> {
    database().map(|db| db.restricted_areas(south, north, west, east)).unwrap_or_default()
}

/// What is gathered for one real indicator (`F`, `U` or `B`) while a region's rows are
/// read: its name and vertical limits, taken from whichever row first carries them, and
/// every boundary chain closed off under it. A delegated portion's rows (`C`, `A`, or any
/// other indicator) are read past but never fed into one of these.
#[derive(Default)]
struct IndicatorGroup {
    name: Option<String>,
    fir_upper: Option<String>,
    uir_lower: Option<String>,
    uir_upper: Option<String>,
    parts: Vec<Vec<(f64, f64)>>,
}

impl Database {
    fn regions(&self, idents: &[String]) -> Vec<Region> {
        let mut out = Vec::new();
        let Some(connection) = read_only(&self.path) else { return out };
        let Some(table) = self.table("fir_uir") else { return out };
        let sql = format!(
            "select fir_uir_indicator, fir_uir_name, boundary_via, fir_uir_latitude, fir_uir_longitude, \
             arc_origin_latitude, arc_origin_longitude, arc_distance, fir_upper_limit, uir_lower_limit, uir_upper_limit \
             from \"{table}\" where fir_uir_identifier = ?1 order by fir_uir_indicator, seqno"
        );
        for ident in idents {
            let Ok(mut statement) = connection.prepare(&sql) else { continue };
            #[allow(clippy::type_complexity)]
            let rows: Vec<(String, Option<String>, BoundaryRow, Option<String>, Option<String>, Option<String>)> = match statement.query_map([ident.to_uppercase()], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(1)?,
                    BoundaryRow {
                        via: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        at: row.get::<_, Option<f64>>(3)?.zip(row.get::<_, Option<f64>>(4)?),
                        arc_origin: row.get::<_, Option<f64>>(5)?.zip(row.get::<_, Option<f64>>(6)?),
                        arc_nm: row.get::<_, Option<f64>>(7)?,
                    },
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            }) {
                Ok(r) => r.flatten().collect(),
                Err(_) => continue,
            };

            // A region is written in parts, one boundary chain ending where its own via
            // says it does ("...E"), grouped first by which real indicator it belongs to
            // (a change of indicator ends a chain too, whether or not it was marked E).
            let mut groups: std::collections::HashMap<String, IndicatorGroup> = std::collections::HashMap::new();
            let mut current: Vec<BoundaryRow> = Vec::new();
            let mut current_indicator = String::new();
            for (indicator, name, row, fir_upper, uir_lower, uir_upper) in rows {
                if indicator != current_indicator {
                    if !current.is_empty() {
                        groups.entry(current_indicator.clone()).or_default().parts.push(boundary(&current));
                        current.clear();
                    }
                    current_indicator = indicator.clone();
                }
                if !matches!(indicator.as_str(), "F" | "U" | "B") {
                    // A delegated portion: its boundary rows are read (to keep the loop's
                    // chain-tracking in step) but never accumulated into a group.
                    continue;
                }
                let group = groups.entry(indicator).or_default();
                group.name = group.name.take().or(name);
                group.fir_upper = group.fir_upper.take().or(fir_upper);
                group.uir_lower = group.uir_lower.take().or(uir_lower);
                group.uir_upper = group.uir_upper.take().or(uir_upper);
                let ends = row.via.trim().len() > 1 && row.via.trim().ends_with('E');
                current.push(row);
                if ends {
                    group.parts.push(boundary(&current));
                    current.clear();
                }
            }
            if !current.is_empty() && matches!(current_indicator.as_str(), "F" | "U" | "B") {
                groups.entry(current_indicator.clone()).or_default().parts.push(boundary(&current));
            }

            let mut parts = Vec::new();
            let mut layers = Vec::new();
            for key in ["B", "F", "U"] {
                let Some(g) = groups.get(key) else { continue };
                let mut group_parts = g.parts.clone();
                group_parts.retain(|p| p.len() >= 3);
                if group_parts.is_empty() {
                    continue;
                }
                parts.extend(group_parts.clone());
                let name = g.name.clone().unwrap_or_default();
                let sky = 99_999.0;
                match key {
                    // One boundary, read as the FIR up to its stated ceiling and again as
                    // the UIR from there: ARINC gives it once because the two share a line.
                    "B" => {
                        layers.push(RegionLayer { name: name.clone(), floor_ft: 0.0, ceiling_ft: g.fir_upper.as_deref().and_then(limit_ft).unwrap_or(sky), parts: group_parts.clone() });
                        layers.push(RegionLayer { name, floor_ft: g.uir_lower.as_deref().and_then(limit_ft).unwrap_or(0.0), ceiling_ft: g.uir_upper.as_deref().and_then(limit_ft).unwrap_or(sky), parts: group_parts });
                    }
                    "F" => layers.push(RegionLayer { name, floor_ft: 0.0, ceiling_ft: g.fir_upper.as_deref().and_then(limit_ft).unwrap_or(sky), parts: group_parts }),
                    "U" => layers.push(RegionLayer { name, floor_ft: g.uir_lower.as_deref().and_then(limit_ft).unwrap_or(0.0), ceiling_ft: g.uir_upper.as_deref().and_then(limit_ft).unwrap_or(sky), parts: group_parts }),
                    _ => unreachable!(),
                }
            }
            if layers.is_empty() {
                // Nothing but delegated rows under this identifier, or nothing at all.
                continue;
            }
            // The FIR's own name leads; a region with no FIR of its own (an oceanic
            // control area, typically) is named after its UIR instead.
            let name = groups.get("F").and_then(|g| g.name.clone()).or_else(|| groups.get("B").and_then(|g| g.name.clone())).or_else(|| groups.get("U").and_then(|g| g.name.clone())).unwrap_or_default();
            out.push(Region { ident: ident.to_uppercase(), name: name.trim().to_string(), parts, layers });
        }
        out
    }

    fn restricted_areas(&self, south: f64, north: f64, west: f64, east: f64) -> Vec<Restricted> {
        let mut out = Vec::new();
        let Some(connection) = read_only(&self.path) else { return out };
        let Some(table) = self.table("restrictive_airspace") else { return out };
        // The areas with any point in the box, then every row of each of them.
        let find = format!(
            "select distinct restrictive_airspace_designation, icao_code, multiple_code from \"{table}\" \
             where latitude between ?1 and ?2 and longitude between ?3 and ?4"
        );
        let Ok(mut statement) = connection.prepare(&find) else { return out };
        let Ok(found) = statement.query_map(rusqlite::params![south, north, west, east], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?.unwrap_or_default(), row.get::<_, Option<String>>(2)?.unwrap_or_default()))
        }) else {
            return out;
        };
        let found: Vec<(String, String, String)> = found.flatten().collect();
        let rows_sql = format!(
            "select restrictive_airspace_name, restrictive_type, boundary_via, latitude, longitude, arc_origin_latitude, \
             arc_origin_longitude, arc_distance, lower_limit, upper_limit from \"{table}\" \
             where restrictive_airspace_designation = ?1 and ifnull(icao_code, '') = ?2 and ifnull(multiple_code, '') = ?3 order by seqno"
        );
        let Ok(mut statement) = connection.prepare(&rows_sql) else { return out };
        for (designation, region, multiple) in found {
            #[allow(clippy::type_complexity)]
            let rows: Vec<(Option<String>, Option<String>, BoundaryRow, Option<String>, Option<String>)> = match statement.query_map([&designation, &region, &multiple], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    BoundaryRow {
                        via: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        at: row.get::<_, Option<f64>>(3)?.zip(row.get::<_, Option<f64>>(4)?),
                        arc_origin: row.get::<_, Option<f64>>(5)?.zip(row.get::<_, Option<f64>>(6)?),
                        arc_nm: row.get::<_, Option<f64>>(7)?,
                    },
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            }) {
                Ok(r) => r.flatten().collect(),
                Err(_) => continue,
            };
            let Some(first) = rows.first() else { continue };
            let kind = first.1.clone().and_then(|k| k.trim().chars().next()).unwrap_or('R');
            let name = rows.iter().find_map(|r| r.0.clone()).unwrap_or_default();
            let lower_ft = rows.iter().find_map(|r| r.3.as_deref().and_then(limit_ft)).unwrap_or(0.0);
            let upper_ft = rows.iter().find_map(|r| r.4.as_deref().and_then(limit_ft)).unwrap_or(99_999.0);
            let rows: Vec<BoundaryRow> = rows.into_iter().map(|r| r.2).collect();
            let outline = boundary(&rows);
            if outline.len() >= 3 {
                out.push(Restricted { designation, name: name.trim().to_string(), kind, lower_ft, upper_ft, boundary: outline });
            }
        }
        out
    }
}

/// True bearing from one point to another, degrees.
fn bearing(from: (f64, f64), to: (f64, f64)) -> f64 {
    let cos = from.0.to_radians().cos().max(0.05);
    let (dn, de) = (to.0 - from.0, (to.1 - from.1) * cos);
    (de.atan2(dn).to_degrees() + 360.0) % 360.0
}

/// Points round an arc about an origin at a radius in miles, from one bearing to another,
/// clockwise or not.
fn arc(origin: (f64, f64), radius_nm: f64, from_deg: f64, to_deg: f64, clockwise: bool) -> Vec<(f64, f64)> {
    let cos = origin.0.to_radians().cos().max(0.05);
    let mut sweep = if clockwise { (to_deg - from_deg + 360.0) % 360.0 } else { -((from_deg - to_deg + 360.0) % 360.0) };
    if sweep.abs() < 1e-6 {
        sweep = if clockwise { 360.0 } else { -360.0 };
    }
    let steps = ((sweep.abs() / 4.0).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            let b = (from_deg + sweep * i as f64 / steps as f64).to_radians();
            (origin.0 + radius_nm * b.cos() / 60.0, origin.1 + radius_nm * b.sin() / 60.0 / cos)
        })
        .collect()
}

/// Where each named fix is, the nearest of that name to a point. Fixes the navigation
/// database does not know are left out.
pub fn fixes_near(idents: &[String], near: (f64, f64)) -> std::collections::HashMap<String, (f64, f64)> {
    database().map(|db| db.fixes_near(idents, near)).unwrap_or_default()
}

/// Where a runway's landing threshold is.
pub fn runway_threshold(icao: &str, runway: &str) -> Option<(f64, f64)> {
    database()?.runway_threshold(icao, runway)
}

/// The tables worth finding, by what their names end with.
const WANTED: &[&str] = &[
    "runways",
    "vhfnavaids",
    "enroute_ndbnavaids",
    "terminal_ndbnavaids",
    "enroute_waypoints",
    "terminal_waypoints",
    "airports",
    "enroute_airways",
    "grid_mora",
    "controlled_airspace",
    "restrictive_airspace",
    "fir_uir",
    "airport_communication",
    "localizers_glideslopes",
    "localizer_marker",
    "airport_msa",
    "holdings",
    "header",
];

/// Opened read-only and immutable, so that a database an aircraft is using is never
/// locked or written to.
fn read_only(path: &std::path::Path) -> Option<rusqlite::Connection> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let uri = format!("file:{}?mode=ro&immutable=1", path.to_string_lossy().replace('\\', "/").replace(' ', "%20").replace('?', "%3f").replace('#', "%23"));
    rusqlite::Connection::open_with_flags(uri, flags).ok()
}

/// Where the add-ons keep their navigation databases.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for sim in crate::bridge::desktop::detect_sims() {
        let Ok(entries) = std::fs::read_dir(&sim.community) else { continue };
        for entry in entries.flatten() {
            let package = entry.path();
            if !package.is_dir() {
                continue;
            }
            // The two places they are kept, and nothing else is searched: a package can
            // hold tens of thousands of files and none of the rest are databases.
            for rest in [["Config", "NavData"], ["Navigraph", "BundledData"]] {
                let dir = package.join(rest[0]).join(rest[1]);
                let Ok(files) = std::fs::read_dir(&dir) else { continue };
                for file in files.flatten() {
                    let path = file.path();
                    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                    if name.ends_with(".s3db") || name.ends_with(".db3") || name.ends_with(".sqlite") {
                        out.push(path);
                    }
                }
            }
        }
    }
    out
}

/// A name the way a chart prints it, without the sort of transmitter after it.
fn tidy_name(name: &str) -> String {
    let mut words: Vec<&str> = name.split_whitespace().collect();
    while let Some(last) = words.last() {
        let tail = last.to_uppercase();
        let is_type = matches!(tail.as_str(), "VOR" | "VOR/DME" | "VORTAC" | "TACAN" | "DME" | "NDB" | "LOM" | "LOCATOR");
        if is_type && words.len() > 1 {
            words.pop();
        } else {
            break;
        }
    }
    words.join(" ")
}

/// The category of an ILS, as a chart writes it.
fn category(code: &str) -> String {
    match code.trim() {
        "1" => "CAT I".to_string(),
        "2" => "CAT II".to_string(),
        "3" => "CAT III".to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------------
// Airports and runways, for `route::airports`.
// ---------------------------------------------------------------------------------

/// One row of the airport table: identifier, name, position and field elevation.
#[derive(Debug, Clone)]
pub struct AirportRow {
    pub icao: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: f64,
}

/// Every airport in the navigation database, for `route::airports` to load once into its
/// own spatial index rather than ask this crate's connection for one at a time.
pub fn all_airports() -> Vec<AirportRow> {
    let Some((connection, table)) = open_table("airports") else { return Vec::new() };
    let sql = format!("select airport_identifier, airport_name, airport_ref_latitude, airport_ref_longitude, elevation from \"{table}\"");
    let Ok(mut statement) = connection.prepare(&sql) else { return Vec::new() };
    let rows = statement.query_map([], |row| {
        Ok(AirportRow {
            icao: row.get::<_, String>(0)?.trim().to_uppercase(),
            name: row.get::<_, Option<String>>(1)?.unwrap_or_default().trim().to_string(),
            lat: row.get::<_, f64>(2)?,
            lon: row.get::<_, f64>(3)?,
            elevation_ft: row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
        })
    });
    rows.map(|r| r.flatten().filter(|a| !a.icao.is_empty()).collect()).unwrap_or_default()
}

/// One row of the runway table: which airport, its identifier, its length and where its
/// threshold is.
#[derive(Debug, Clone)]
pub struct RunwayRow {
    pub icao: String,
    /// Without the "RW" the table itself prefixes it with: "09L", not "RW09L".
    pub ident: String,
    pub length_ft: f64,
    pub bearing_true_deg: Option<f64>,
    pub lat: f64,
    pub lon: f64,
}

/// Every runway in the database, in one pass.
///
/// `runways` opens the database afresh and asks it for one airport, which is the right
/// shape for a plan that wants the runways at two of them. Anything that wants all of them
/// — the airport index a diversion search is built on — must not call it in a loop: at
/// twenty thousand airports, opening a hundred-and-sixty-megabyte file that many times took
/// forty seconds, against the eighty-seven milliseconds the route search itself needs.
pub fn all_runways() -> Vec<RunwayRow> {
    let Some((connection, table)) = open_table("runways") else { return Vec::new() };
    let sql = format!(
        "select airport_identifier, runway_identifier, runway_length, \
         coalesce(runway_true_bearing, runway_magnetic_bearing), runway_latitude, runway_longitude \
         from \"{table}\""
    );
    let Ok(mut statement) = connection.prepare(&sql) else { return Vec::new() };
    let rows = statement.query_map([], runway_row);
    rows.map(|r| r.flatten().filter(|w| !w.ident.is_empty()).collect()).unwrap_or_default()
}

fn runway_row(row: &rusqlite::Row) -> rusqlite::Result<RunwayRow> {
    Ok(RunwayRow {
        icao: row.get::<_, String>(0)?.trim().to_uppercase(),
        // Only the "RW" comes off. A trailing `trim_start_matches('0')` used to run after
        // it too, turning "RW09L" into "9L" — losing the digit a SID or STAR transition
        // ("RW09L") is matched against, which is always written two digits wide.
        ident: row.get::<_, String>(1)?.trim().trim_start_matches("RW").to_string(),
        length_ft: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
        bearing_true_deg: row.get::<_, Option<f64>>(3).ok().flatten(),
        lat: row.get::<_, f64>(4)?,
        lon: row.get::<_, f64>(5)?,
    })
}

/// Every runway published for an airport: what a route search picks the departure and
/// arrival ends from.
pub fn runways(icao: &str) -> Vec<RunwayRow> {
    let Some((connection, table)) = open_table("runways") else { return Vec::new() };
    // Not every database carries a true bearing alongside the magnetic one this file
    // already reads elsewhere; where it does not, the magnetic figure is close enough
    // that a runway is never picked against the wind by more than a token amount.
    let sql = format!(
        "select airport_identifier, runway_identifier, runway_length, \
         coalesce(runway_true_bearing, runway_magnetic_bearing), runway_latitude, runway_longitude \
         from \"{table}\" where airport_identifier = ?1"
    );
    let Ok(mut statement) = connection.prepare(&sql) else { return Vec::new() };
    let rows = statement.query_map([icao.to_uppercase()], runway_row);
    rows.map(|r| r.flatten().filter(|w| !w.ident.is_empty()).collect()).unwrap_or_default()
}

// ---------------------------------------------------------------------------------
// SIDs and STARs, for the route search to join a flight plan onto the airways with.
// ---------------------------------------------------------------------------------

/// Which family of procedure a leg belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcedureKind {
    Sid,
    Star,
}

/// One leg of a published SID or STAR, as the ARINC 424 tables write it: which procedure,
/// which transition it belongs to (blank for the common portion, "RWxx" for a leg specific
/// to one runway), its place in sequence, the fix, and what it constrains a flight to.
#[derive(Debug, Clone)]
pub struct ProcedureLeg {
    pub procedure: String,
    pub transition: String,
    pub seqno: i64,
    pub fix: String,
    pub lat: f64,
    pub lon: f64,
    /// '+' at or above `altitude1_ft`, '-' at or below it, 'B' between it and
    /// `altitude2_ft`; blank means exactly at `altitude1_ft`.
    pub altitude_description: String,
    pub altitude1_ft: Option<f64>,
    pub altitude2_ft: Option<f64>,
    pub speed_max_kt: Option<f64>,
}

/// Every leg of every SID or STAR published at an airport, in the order each procedure is
/// flown. `route::procedures` groups these into named procedures — a runway-specific start,
/// a common middle, an enroute transition at the far end for a SID and the other way about
/// for a STAR — and picks the one that fits the runway and the rest of the flight.
///
/// The sequence column is `seqno`, not the `sequence_number` an earlier reading of this
/// table assumed: every add-on's copy of `tbl_sids`/`tbl_stars` uses the short name, and the
/// long one was never in the wild, so the query used to fail to prepare at all and every
/// airport answered with no procedures whatever the runway.
pub fn procedure_legs(icao: &str, kind: ProcedureKind) -> Vec<ProcedureLeg> {
    let suffix = match kind {
        ProcedureKind::Sid => "sids",
        ProcedureKind::Star => "stars",
    };
    let Some((connection, table)) = open_table(suffix) else { return Vec::new() };
    let sql = format!(
        "select procedure_identifier, transition_identifier, seqno, waypoint_identifier, waypoint_latitude, waypoint_longitude, \
         altitude_description, altitude1, altitude2, \
         case when trim(speed_limit_description) = '+' then null else speed_limit end \
         from \"{table}\" where airport_identifier = ?1 order by procedure_identifier, transition_identifier, seqno"
    );
    let Ok(mut statement) = connection.prepare(&sql) else { return Vec::new() };
    let rows = statement.query_map([icao.to_uppercase()], |row| {
        Ok(ProcedureLeg {
            procedure: row.get::<_, String>(0)?.trim().to_string(),
            transition: row.get::<_, Option<String>>(1)?.unwrap_or_default().trim().to_uppercase(),
            seqno: row.get::<_, i64>(2).unwrap_or_default(),
            fix: row.get::<_, Option<String>>(3)?.unwrap_or_default().trim().to_string(),
            lat: row.get::<_, Option<f64>>(4)?.unwrap_or_default(),
            lon: row.get::<_, Option<f64>>(5)?.unwrap_or_default(),
            altitude_description: row.get::<_, Option<String>>(6)?.unwrap_or_default().trim().to_string(),
            altitude1_ft: row.get::<_, Option<f64>>(7).ok().flatten(),
            altitude2_ft: row.get::<_, Option<f64>>(8).ok().flatten(),
            // A '+' speed limit is a minimum ("at or above"), which nothing here models;
            // carrying it as `speed_max_kt` would print a cap that is really a floor, so
            // the query itself leaves it out rather than every caller having to know why.
            speed_max_kt: row.get::<_, Option<f64>>(9).ok().flatten(),
        })
    });
    rows.map(|r| r.flatten().filter(|l| !l.fix.is_empty() && !l.procedure.is_empty()).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the database on this machine says of its airports, runways and SIDs. Run by
    /// hand: there is no aircraft navigation database on a build machine to check it
    /// against automatically.
    #[test]
    #[ignore]
    fn airports_runways_and_sids_from_the_installed_database() {
        let airports = all_airports();
        println!("airports: {}", airports.len());
        if let Some(a) = airports.iter().find(|a| a.icao == "KJFK") {
            println!("KJFK: {a:?}");
            for rw in runways(&a.icao) {
                println!("  {rw:?}");
            }
            for leg in procedure_legs(&a.icao, ProcedureKind::Sid).iter().take(20) {
                println!("  SID {leg:?}");
            }
        }
    }

    /// What the database on this machine says round Kennedy. Run by hand.
    #[test]
    #[ignore]
    fn kennedy_from_the_installed_database() {
        println!("mora {:?}", grid_mora(40.0, 41.0, -74.5, -73.0));
        println!("info {:?}", airport_info("KJFK"));
        println!("airspace {}", airspace("KJFK").len());
    }

    #[test]
    fn a_beacon_keeps_its_name_and_loses_its_type() {
        assert_eq!(tidy_name("CANARSIE VOR/DME"), "CANARSIE");
        assert_eq!(tidy_name("DEER PARK"), "DEER PARK");
        assert_eq!(tidy_name("VOR"), "VOR");
    }

    #[test]
    fn a_category_is_written_the_way_a_chart_writes_it() {
        assert_eq!(category("3"), "CAT III");
        assert_eq!(category("1"), "CAT I");
    }
}
