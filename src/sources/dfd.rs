//! Procedures out of an aircraft's navigation database: the ARINC 424 data the add-ons
//! ship as SQLite, which `navdata` already reads for beacons, localisers, holds and safe
//! altitudes. Every departure, arrival and approach in it is read into the same
//! `AirportProcedures` the simulator's data and the FAA's CIFP give, so the same charts
//! are drawn from it.
//!
//! The rows are ARINC 424 records with a column a field, and are read as `cifp` reads the
//! FAA's file, with two differences: each leg carries its fix's position, so there is no
//! waypoint table to look it up in; and Navigraph's layout files a leg's length under
//! `distance_time` and whether that is miles or minutes under
//! `route_distance_holding_distance_time`, the other way round from their names, so each
//! of the two is told by what it holds.

use crate::sources::cifp::{assemble, Routes};
use crate::sources::msfs::procedures::{AirportProcedures, AltitudeRule, FixRole, Frequency, Leg, Turn};
use anyhow::{anyhow, Result};
use rusqlite::types::ValueRef;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// A table, by what its name ends with: Navigraph's layout puts the ARINC section in
/// every name (`tbl_pf_iaps`), PMDG's does not (`tbl_iaps`).
fn table(connection: &rusqlite::Connection, ending: &str) -> Option<String> {
    let mut statement = connection.prepare("select name from sqlite_master where type = 'table'").ok()?;
    let names: Vec<String> = statement.query_map([], |row| row.get::<_, String>(0)).ok()?.flatten().collect();
    names.into_iter().find(|n| n.to_lowercase().ends_with(&format!("_{ending}")))
}

/// A text column as it is stored, spaces kept: a description code's letters count by
/// position. Empty when the column is missing, empty or not text.
fn raw(row: &rusqlite::Row, name: &str) -> String {
    match row.get_ref(name) {
        Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).into_owned(),
        _ => String::new(),
    }
}

fn text(row: &rusqlite::Row, name: &str) -> String {
    raw(row, name).trim().to_string()
}

/// A number column, None when it is missing, empty or not a number.
fn num(row: &rusqlite::Row, name: &str) -> Option<f64> {
    match row.get_ref(name) {
        Ok(ValueRef::Real(v)) => Some(v),
        Ok(ValueRef::Integer(v)) => Some(v as f64),
        Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).trim().parse().ok(),
        _ => None,
    }
}

/// One leg, and its description code.
fn leg(row: &rusqlite::Row) -> (Leg, String) {
    let path = text(row, "path_termination");
    let alt1 = num(row, "altitude1").filter(|v| *v > 0.0);
    let alt2 = num(row, "altitude2").filter(|v| *v > 0.0);
    let rule = match text(row, "altitude_description").as_str() {
        "+" | "H" | "J" | "V" | "C" => AltitudeRule::AtOrAbove,
        "-" | "Y" => AltitudeRule::AtOrBelow,
        "B" => AltitudeRule::Between,
        _ if alt1.is_some() => AltitudeRule::At,
        _ => AltitudeRule::None,
    };
    // Of the two columns, one holds the length and the other says whether it is a
    // distance or a holding time (T).
    let (a, b) = ("distance_time", "route_distance_holding_distance_time");
    let length = num(row, a).or_else(|| num(row, b));
    let flag = [text(row, a), text(row, b)].into_iter().find(|t| t.parse::<f64>().is_err() && !t.is_empty()).unwrap_or_default();
    let rf = path == "RF";
    let desc = format!("{:<4}", raw(row, "waypoint_description_code"));
    let role = match desc.chars().nth(3) {
        Some('A' | 'C' | 'D') => Some(FixRole::Initial),
        Some('B' | 'I') => Some(FixRole::Intermediate),
        Some('F') => Some(FixRole::Final),
        Some('M') => Some(FixRole::MissedApproachPoint),
        _ => None,
    };
    let leg = Leg {
        fix: text(row, "waypoint_identifier"),
        altitude_rule: rule,
        altitude_ft: alt1,
        altitude2_ft: if rule == AltitudeRule::Between { alt2 } else { None },
        course_deg: num(row, "course"),
        distance_m: (flag != "T").then_some(length).flatten().map(|nm| nm * 1852.0),
        // A radius-to-fix leg turns round its centre fix; any other is measured from its
        // recommended navaid.
        navaid: text(row, if rf { "center_waypoint" } else { "recommended_navaid" }),
        theta_deg: num(row, "theta"),
        rho_nm: if rf { num(row, "arc_radius") } else { num(row, "rho") },
        turn: match text(row, "turn_direction").as_str() {
            "L" => Some(Turn::Left),
            "R" => Some(Turn::Right),
            _ => None,
        },
        role,
        placed_on_radial: false,
        speed_kt: num(row, "speed_limit").filter(|v| *v > 0.0),
        lat: num(row, "waypoint_latitude"),
        lon: num(row, "waypoint_longitude"),
        path,
    };
    (leg, desc)
}

/// What a communication type is called where the simulator's data names it.
fn frequency_kind(code: &str) -> Option<&'static str> {
    Some(match code {
        "ATI" => "ATIS",
        "APP" | "ARR" => "APPROACH",
        "TWR" => "TOWER",
        "GND" => "GROUND",
        "CLD" | "CLR" | "DEL" => "DELIVERY",
        "DEP" => "DEPARTURE",
        "UNI" => "UNICOM",
        "MUL" => "MULTICOM",
        _ => return None,
    })
}

/// Every airport in the database with procedures, by its identifier. `source` is what the
/// procedures are said to come from on the charts.
pub fn read(path: &Path, source: &str) -> Result<HashMap<String, AirportProcedures>> {
    let connection = crate::sources::navdata::read_only(path).ok_or_else(|| anyhow!("cannot open {}", path.display()))?;
    let airports_table = table(&connection, "airports").ok_or_else(|| anyhow!("{} has no airports table", path.display()))?;
    let mut airports: HashMap<String, (f64, f64, Option<f64>)> = HashMap::new();
    {
        let mut statement = connection.prepare(&format!("select * from \"{airports_table}\""))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            if let (Some(lat), Some(lon)) = (num(row, "airport_ref_latitude"), num(row, "airport_ref_longitude")) {
                airports.insert(text(row, "airport_identifier"), (lat, lon, num(row, "magnetic_variation")));
            }
        }
    }

    let mut procs: Routes = BTreeMap::new();
    for (ending, section) in [("sids", 'D'), ("stars", 'E'), ("iaps", 'F')] {
        let Some(name) = table(&connection, ending) else {
            continue;
        };
        // The order the records are filed in: a route's legs together, in sequence.
        let mut statement = connection.prepare(&format!("select * from \"{name}\" order by airport_identifier, procedure_identifier, route_type, transition_identifier, seqno"))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let key = (text(row, "route_type").chars().next().unwrap_or(' '), text(row, "transition_identifier"));
            let routes = procs.entry((text(row, "airport_identifier"), section, text(row, "procedure_identifier"))).or_default();
            if routes.last().map(|(k, _, _)| k != &key).unwrap_or(true) {
                routes.push((key, Vec::new(), Vec::new()));
            }
            let last = routes.last_mut().expect("just pushed");
            let (leg, desc) = leg(row);
            last.1.push(leg);
            last.2.push(desc);
        }
    }
    let mut out = assemble(procs, &airports, source);

    // The frequencies a chart briefs, which the simulator's data gives with its procedures.
    if let Some(name) = table(&connection, "airport_communication") {
        let mut statement = connection.prepare(&format!("select * from \"{name}\""))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let Some(a) = out.get_mut(&text(row, "airport_identifier")) else {
                continue;
            };
            let (Some(kind), Some(mhz)) = (frequency_kind(&text(row, "communication_type")), num(row, "communication_frequency")) else {
                continue;
            };
            if (108.0..=137.0).contains(&mhz) {
                a.frequencies.push(Frequency { kind: kind.to_string(), mhz, name: text(row, "callsign") });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::msfs::procedures::Kind;

    /// Two approaches and a departure in Navigraph's layout, the length and its flag the
    /// way round Navigraph files them.
    fn database() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "create table tbl_pa_airports (airport_identifier text, airport_ref_latitude real, airport_ref_longitude real, magnetic_variation real);
             insert into tbl_pa_airports values ('LPMA', 32.6942, -16.7781, -5.0);
             create table tbl_pf_iaps (airport_identifier text, procedure_identifier text, route_type text, transition_identifier text, seqno integer,
                 path_termination text, waypoint_identifier text, waypoint_latitude real, waypoint_longitude real, waypoint_description_code text,
                 altitude_description text, altitude1 integer, altitude2 integer, course real, distance_time real, route_distance_holding_distance_time text,
                 recommended_navaid text, center_waypoint text, arc_radius real, rho real, theta real, turn_direction text, speed_limit integer);
             insert into tbl_pf_iaps values
                 ('LPMA','R05-Y','A','MONEC',10,'IF','MONEC',32.456,-16.830,'E  C','+',3000,null,null,null,null,null,null,null,null,null,null,230),
                 ('LPMA','R05-Y','A','MONEC',30,'RF','MA510',32.534,-16.803,'E  B','+',2000,null,null,1.7,'D',null,'MAC03',5.0,null,null,'L',null),
                 ('LPMA','R05-Y','R',null,10,'TF','MA502',32.683,-16.792,'E  F','@',2000,null,null,null,null,null,null,null,null,null,null,null),
                 ('LPMA','R05-Y','R',null,20,'TF','RW05',32.690,-16.780,'G  M','@',390,null,null,null,null,null,null,null,null,null,null,null),
                 ('LPMA','R05-Y','R',null,30,'CA',null,null,null,'  M ','+',1000,null,52.0,null,null,null,null,null,null,null,null,null),
                 ('LPMA','R05-Y','R',null,40,'HM','MONEC',32.456,-16.830,'E  H','+',4000,null,230.0,1.0,'T',null,null,null,null,null,'R',null);
             create table tbl_pd_sids (airport_identifier text, procedure_identifier text, route_type text, transition_identifier text, seqno integer,
                 path_termination text, waypoint_identifier text, waypoint_latitude real, waypoint_longitude real, waypoint_description_code text,
                 altitude_description text, altitude1 integer, course real);
             insert into tbl_pd_sids values
                 ('LPMA','DEGU1E','5','RW05',10,'CA',null,null,null,null,'+',500,52.0),
                 ('LPMA','DEGU1E','5','RW05',40,'TF','DEGUN',32.9,-16.6,'EE  ',null,null,null);
             create table tbl_pv_airport_communication (airport_identifier text, communication_type text, communication_frequency real, callsign text);
             insert into tbl_pv_airport_communication values ('LPMA','TWR',124.66,'MADEIRA'), ('LPMA','ACC',132.255,'LISBON');",
        )
        .unwrap();
        c
    }

    #[test]
    fn reads_approaches_departures_and_frequencies() {
        let dir = std::env::temp_dir().join(format!("amdbgen-dfd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.s3db");
        let _ = std::fs::remove_file(&path);
        database().execute("vacuum into ?1", [path.to_string_lossy()]).unwrap();
        let all = read(&path, "test AIRAC 2503").unwrap();
        let a = &all["LPMA"];
        assert_eq!(a.source, "test AIRAC 2503");
        assert_eq!(a.magnetic_variation_deg, Some(-5.0));

        let app = a.procedures.iter().find(|p| p.kind == Kind::Approach).unwrap();
        assert_eq!(app.name, "RNAV (GPS) Y RWY 05");
        assert_eq!(app.runway, "05");
        let parts: Vec<&str> = app.transitions.iter().map(|t| if t.part.is_empty() { t.name.as_str() } else { t.part.as_str() }).collect();
        assert_eq!(parts, ["MONEC", "final", "missed"]);
        // The arc: its radius and centre, its length in metres.
        let rf = &app.transitions[0].legs[1];
        assert_eq!((rf.path.as_str(), rf.navaid.as_str(), rf.rho_nm, rf.turn), ("RF", "MAC03", Some(5.0), Some(Turn::Left)));
        assert!((rf.distance_m.unwrap() - 1.7 * 1852.0).abs() < 0.01);
        assert_eq!(app.transitions[0].legs[0].speed_kt, Some(230.0));
        assert_eq!(app.transitions[0].legs[0].role, Some(FixRole::Initial));
        // The missed approach starts at the climb, after the runway.
        assert_eq!(app.transitions[1].legs.last().unwrap().role, Some(FixRole::MissedApproachPoint));
        let hold = app.transitions[2].legs.last().unwrap();
        assert_eq!((hold.path.as_str(), hold.distance_m, hold.turn), ("HM", None, Some(Turn::Right)));

        let sid = a.procedures.iter().find(|p| p.kind == Kind::Sid).unwrap();
        assert_eq!((sid.name.as_str(), sid.transitions[0].name.as_str(), sid.transitions[0].legs.len()), ("DEGU1E", "RW05", 2));
        assert_eq!(a.frequencies.len(), 1);
        assert_eq!((a.frequencies[0].kind.as_str(), a.frequencies[0].mhz), ("TOWER", 124.66));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
