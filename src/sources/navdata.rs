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
                "select {ident}, {name}, {frequency}, {lat_col}, {lon_col} from \"{table}\" \
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
            "select llz_identifier, llz_frequency, llz_bearing, gs_angle, llz_latitude, llz_longitude, ils_mls_gls_category \
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
            "select marker_type, marker_latitude, marker_longitude from \"{table}\" \
             where airport_identifier = ?1 and (runway_identifier = ?2 or runway_identifier = ?3)"
        );
        let Ok(mut statement) = connection.prepare(&sql) else { return Vec::new() };
        let rows = statement.query_map([icao.to_uppercase().as_str(), runway, short], |row| {
            let kind = match row.get::<_, String>(0).unwrap_or_default().to_uppercase().as_str() {
                "IM" => Marker::Inner,
                "MM" => Marker::Middle,
                _ => Marker::Outer,
            };
            Ok((kind, row.get::<_, f64>(1)?, row.get::<_, f64>(2)?))
        });
        rows.map(|r| r.flatten().collect()).unwrap_or_default()
    }

    fn msa(&self, icao: &str, at: (f64, f64)) -> Option<Msa> {
        let connection = read_only(&self.path)?;
        let table = self.table("airport_msa")?;
        // An airport carries one of these for each way in — one per runway, one per
        // arrival waypoint, one about a beacon. They are not all worth printing: the one
        // centred on a beacon and split into sectors is what a chart shows, because a
        // single figure for the whole circle carries the highest ground all the way round.
        let sql = format!(
            "select msa_center, msa_center_latitude, msa_center_longitude, radius_limit, \
             sector_bearing_1, sector_altitude_1, sector_bearing_2, sector_altitude_2, \
             sector_bearing_3, sector_altitude_3, sector_bearing_4, sector_altitude_4, \
             sector_bearing_5, sector_altitude_5 from \"{table}\" where airport_identifier = ?1 \
             order by (sector_bearing_5 is not null) + (sector_bearing_4 is not null) \
             + (sector_bearing_3 is not null) + (sector_bearing_2 is not null) desc, \
             (msa_center like 'RW%') asc,              (msa_center_latitude - ?2) * (msa_center_latitude - ?2)              + (msa_center_longitude - ?3) * (msa_center_longitude - ?3) asc limit 1"
        );
        connection
            .query_row(&sql, rusqlite::params![icao.to_uppercase(), at.0, at.1], |row| {
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

    fn hold_at(&self, fix: &str) -> Option<Hold> {
        let connection = read_only(&self.path)?;
        let table = self.table("holdings")?;
        let sql = format!(
            "select waypoint_identifier, waypoint_latitude, waypoint_longitude, inbound_holding_course, \
             turn_direction, leg_time, leg_length, maximum_altitude from \"{table}\" where waypoint_identifier = ?1 limit 1"
        );
        connection
            .query_row(&sql, [fix.to_uppercase()], |row| {
                Ok(Hold {
                    fix: row.get::<_, String>(0).unwrap_or_default(),
                    lat: row.get::<_, f64>(1)?,
                    lon: row.get::<_, f64>(2)?,
                    inbound_deg: row.get::<_, f64>(3).unwrap_or_default(),
                    right_turns: !row.get::<_, String>(4).unwrap_or_default().eq_ignore_ascii_case("L"),
                    leg_time_min: row.get::<_, f64>(5).ok().filter(|v| *v > 0.0),
                    leg_nm: row.get::<_, f64>(6).ok().filter(|v| *v > 0.0),
                    max_altitude_ft: row.get::<_, f64>(7).ok().filter(|v| *v > 0.0),
                })
            })
            .ok()
    }
}

/// The tables worth finding, by what their names end with.
const WANTED: &[&str] = &[
    "vhfnavaids",
    "enroute_ndbnavaids",
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

#[cfg(test)]
mod tests {
    use super::*;

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
