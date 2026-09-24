//! Writes a [`NavSet`] in the Navigraph DFD `tbl_*` layout, which three of the aircraft on
//! a typical install read: iniBuilds' A350 and Synaptic's A220 under
//! `Navigraph\BundledData\`, and PMDG's 737 and 777 under `Config\NavData\e_dfd_PMDG.s3db`.
//! One writer therefore replaces three stale databases — on the machine this was written
//! for, AIRAC 2303, 2503 and 2404 respectively, the oldest of them three and a half years
//! out of date.
//!
//! Where Fenix's schema normalises everything behind integer row IDs, this one does the
//! opposite: every table repeats the identifiers in full, exactly as ARINC 424's own fixed
//! records do, and nothing needs resolving against anything else. That makes it the simpler
//! of the two to write and the more forgiving to get slightly wrong, since a mistaken field
//! is one wrong column rather than a broken join. What it costs instead is breadth: the
//! three procedure tables carry thirty-seven columns each and are written from one shared
//! function, because a SID, a STAR and an approach differ in this layout only by which table
//! they land in.
//!
//! Two details this layout will catch out anyone writing it from the column names alone, both
//! learned the hard way elsewhere in this crate:
//!
//! * the procedure tables' sequence column is `seqno`, not `sequence_number` — a query
//!   asking for the latter simply fails to prepare, and [`crate::sources::navdata`] records
//!   what that cost to find out;
//! * `tbl_pi_localizers_glideslopes` and `tbl_localizers_glideslopes` are the same table
//!   under two spellings, one prefixed with its ARINC section. The databases this writes are
//!   read by add-ons that look the table up by suffix, so the plain spelling is written and
//!   the prefixed one is not created.
//!
//! The installed databases carry no indices at all — twenty-seven tables and not one
//! `CREATE INDEX` between them — so none are created here either. That is worth stating
//! rather than silently imitating: a four-hundred-thousand-row procedure table is scanned
//! rather than sought through, and an aircraft's own reader evidently copes.

use crate::convert::model::{AirwayLevel, IlsCategory, NavaidKind, NavSet, ProcKind, ProcedureRec, Surface};
use crate::convert::TableReport;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;

/// Everything the three databases hold that a simulator's own navigation data cannot
/// supply, with the reason each is left empty. Printed by the `convert` command, because a
/// pilot who does not know the holds are gone is worse off than one flying a stale database.
const OMITTED: &[(&str, &str)] = &[
    ("tbl_holdings", "a published holding pattern is not in the simulator's navigation data; only the holds that form part of a procedure are, and those are written as the HA/HF/HM legs they are"),
    ("tbl_airport_msa", "published minimum safe altitude sectors are not in the simulator's navigation data at all"),
    ("tbl_enroute_airway_restriction", "airway altitude restrictions are not in the simulator's navigation data"),
    ("tbl_controlled_airspace", "airspace boundaries are not carried by this converter"),
    ("tbl_restrictive_airspace", "airspace boundaries are not carried by this converter"),
    ("tbl_fir_uir", "flight information region boundaries are not carried by this converter"),
    ("tbl_airport_communication", "aerodrome frequencies are not carried by this converter"),
    ("tbl_enroute_communication", "enroute frequencies are not carried by this converter"),
    ("tbl_localizer_marker", "marker beacons are not carried by this converter"),
    ("tbl_gls", "GLS approaches are not carried by this converter"),
    ("tbl_pathpoints", "SBAS final approach path points are not carried by this converter"),
    ("tbl_gate", "stands are not carried by this converter"),
    ("tbl_cruising_tables", "cruising level tables are not carried by this converter"),
];

/// Every table the layout has, so that an aircraft's own reader finds each one it looks for
/// even where this writer leaves it empty. A missing table is a failed query; an empty one is
/// an honest answer.
const SCHEMA: &[&str] = &[
    "CREATE TABLE [tbl_header] ([version] TEXT, [arincversion] TEXT, [record_set] TEXT, [current_airac] TEXT, [revision] TEXT, [effective_fromto] TEXT, [previous_airac] TEXT, [previous_fromto] TEXT, [parsed_at] TEXT)",
    "CREATE TABLE [tbl_airports] ([area_code] TEXT, [icao_code] TEXT, [airport_identifier] TEXT, [airport_identifier_3letter] TEXT, [airport_name] TEXT, [airport_ref_latitude] DOUBLE, [airport_ref_longitude] DOUBLE, [ifr_capability] TEXT, [longest_runway_surface_code] TEXT, [elevation] INTEGER, [transition_altitude] INTEGER, [transition_level] INTEGER, [speed_limit] INTEGER, [speed_limit_altitude] INTEGER, [iata_ata_designator] TEXT, [id] TEXT)",
    "CREATE TABLE [tbl_runways] ([area_code] TEXT, [icao_code] TEXT, [airport_identifier] TEXT, [runway_identifier] TEXT, [runway_latitude] DOUBLE, [runway_longitude] DOUBLE, [runway_gradient] DOUBLE, [runway_magnetic_bearing] DOUBLE, [runway_true_bearing] DOUBLE, [landing_threshold_elevation] INTEGER, [displaced_threshold_distance] INTEGER, [threshold_crossing_height] INTEGER, [runway_length] INTEGER, [runway_width] INTEGER, [llz_identifier] TEXT, [llz_mls_gls_category] TEXT, [surface_code] TEXT, [id] TEXT)",
    "CREATE TABLE [tbl_vhfnavaids] ([area_code] TEXT, [airport_identifier] TEXT, [icao_code] TEXT, [vor_identifier] TEXT, [vor_name] TEXT, [vor_frequency] DOUBLE, [navaid_class] TEXT, [vor_latitude] DOUBLE, [vor_longitude] DOUBLE, [dme_ident] TEXT, [dme_latitude] DOUBLE, [dme_longitude] DOUBLE, [dme_elevation] INTEGER, [ilsdme_bias] DOUBLE, [range] INTEGER, [station_declination] DOUBLE, [magnetic_variation] DOUBLE, [id] TEXT)",
    "CREATE TABLE [tbl_enroute_ndbnavaids] ([area_code] TEXT, [icao_code] TEXT, [ndb_identifier] TEXT, [ndb_name] TEXT, [ndb_frequency] DOUBLE, [navaid_class] TEXT, [ndb_latitude] DOUBLE, [ndb_longitude] DOUBLE, [range] INTEGER, [id] TEXT)",
    "CREATE TABLE [tbl_terminal_ndbnavaids] ([area_code] TEXT, [airport_identifier] TEXT, [icao_code] TEXT, [ndb_identifier] TEXT, [ndb_name] TEXT, [ndb_frequency] DOUBLE, [navaid_class] TEXT, [ndb_latitude] DOUBLE, [ndb_longitude] DOUBLE, [range] INTEGER, [id] TEXT)",
    "CREATE TABLE [tbl_enroute_waypoints] ([area_code] TEXT, [icao_code] TEXT, [waypoint_identifier] TEXT, [waypoint_name] TEXT, [waypoint_type] TEXT, [waypoint_usage] TEXT, [waypoint_latitude] DOUBLE, [waypoint_longitude] DOUBLE, [id] TEXT)",
    "CREATE TABLE [tbl_terminal_waypoints] ([area_code] TEXT, [region_code] TEXT, [icao_code] TEXT, [waypoint_identifier] TEXT, [waypoint_name] TEXT, [waypoint_type] TEXT, [waypoint_latitude] DOUBLE, [waypoint_longitude] DOUBLE, [id] TEXT)",
    "CREATE TABLE [tbl_localizers_glideslopes] ([area_code] TEXT, [icao_code] TEXT, [airport_identifier] TEXT, [runway_identifier] TEXT, [llz_identifier] TEXT, [llz_latitude] DOUBLE, [llz_longitude] DOUBLE, [llz_frequency] DOUBLE, [llz_bearing] DOUBLE, [llz_width] DOUBLE, [ils_mls_gls_category] TEXT, [gs_latitude] DOUBLE, [gs_longitude] DOUBLE, [gs_angle] DOUBLE, [gs_elevation] INTEGER, [station_declination] DOUBLE, [id] TEXT)",
    "CREATE TABLE [tbl_enroute_airways] ([area_code] TEXT, [route_identifier] TEXT, [seqno] INTEGER, [icao_code] TEXT, [waypoint_identifier] TEXT, [waypoint_latitude] DOUBLE, [waypoint_longitude] DOUBLE, [waypoint_description_code] TEXT, [route_type] TEXT, [flightlevel] TEXT, [direction_restriction] TEXT, [crusing_table_identifier] TEXT, [minimum_altitude1] INTEGER, [minimum_altitude2] INTEGER, [maximum_altitude] INTEGER, [outbound_course] DOUBLE, [inbound_course] DOUBLE, [inbound_distance] DOUBLE, [id] TEXT)",
    "CREATE TABLE [tbl_grid_mora] ([starting_latitude] INTEGER, [starting_longitude] INTEGER, [mora01] INTEGER, [mora02] INTEGER, [mora03] INTEGER, [mora04] INTEGER, [mora05] INTEGER, [mora06] INTEGER, [mora07] INTEGER, [mora08] INTEGER, [mora09] INTEGER, [mora10] INTEGER, [mora11] INTEGER, [mora12] INTEGER, [mora13] INTEGER, [mora14] INTEGER, [mora15] INTEGER, [mora16] INTEGER, [mora17] INTEGER, [mora18] INTEGER, [mora19] INTEGER, [mora20] INTEGER, [mora21] INTEGER, [mora22] INTEGER, [mora23] INTEGER, [mora24] INTEGER, [mora25] INTEGER, [mora26] INTEGER, [mora27] INTEGER, [mora28] INTEGER, [mora29] INTEGER, [mora30] INTEGER)",
    // The three procedure tables are one shape; see `PROC_COLUMNS`.
    "CREATE TABLE [tbl_holdings] ([area_code] TEXT, [region_code] TEXT, [icao_code] TEXT, [waypoint_identifier] TEXT, [holding_name] TEXT, [waypoint_latitude] DOUBLE, [waypoint_longitude] DOUBLE, [duplicate_identifier] INTEGER, [inbound_holding_course] DOUBLE, [turn_direction] TEXT, [leg_length] DOUBLE, [leg_time] DOUBLE, [minimum_altitude] TEXT, [maximum_altitude] TEXT, [holding_speed] INTEGER)",
    "CREATE TABLE [tbl_airport_msa] ([area_code] TEXT, [icao_code] TEXT, [airport_identifier] TEXT, [msa_center] TEXT, [msa_center_latitude] DOUBLE, [msa_center_longitude] DOUBLE, [magnetic_true_indicator] TEXT, [multiple_code] TEXT, [radius_limit] INTEGER, [sector_bearing_1] INTEGER, [sector_altitude_1] INTEGER, [sector_bearing_2] INTEGER, [sector_altitude_2] INTEGER, [sector_bearing_3] INTEGER, [sector_altitude_3] INTEGER, [sector_bearing_4] INTEGER, [sector_altitude_4] INTEGER, [sector_bearing_5] INTEGER, [sector_altitude_5] INTEGER)",
    "CREATE TABLE [tbl_enroute_airway_restriction] ([area_code] TEXT, [route_identifier] TEXT, [restriction_identifier] INTEGER, [restriction_type] TEXT, [start_waypoint_identifier] TEXT, [start_waypoint_latitude] DOUBLE, [start_waypoint_longitude] DOUBLE, [end_waypoint_identifier] TEXT, [end_waypoint_latitude] DOUBLE, [end_waypoint_longitude] DOUBLE, [start_date] TEXT, [end_date] TEXT, [units_of_altitude] TEXT, [restriction_altitude1] INTEGER, [block_indicator1] TEXT, [restriction_altitude2] INTEGER, [block_indicator2] TEXT, [restriction_altitude3] INTEGER, [block_indicator3] TEXT, [restriction_altitude4] INTEGER, [block_indicator4] TEXT, [restriction_altitude5] INTEGER, [block_indicator5] TEXT, [restriction_altitude6] INTEGER, [block_indicator6] TEXT, [restriction_altitude7] INTEGER, [block_indicator7] TEXT, [restriction_notes] TEXT)",
    "CREATE TABLE [tbl_controlled_airspace] ([area_code] TEXT, [icao_code] TEXT, [airspace_center] TEXT, [controlled_airspace_name] TEXT, [airspace_type] TEXT, [airspace_classification] TEXT, [multiple_code] TEXT, [time_code] TEXT, [seqno] INTEGER, [flightlevel] TEXT, [boundary_via] TEXT, [latitude] DOUBLE, [longitude] DOUBLE, [arc_origin_latitude] DOUBLE, [arc_origin_longitude] DOUBLE, [arc_distance] DOUBLE, [arc_bearing] DOUBLE, [unit_indicator_lower_limit] TEXT, [lower_limit] TEXT, [unit_indicator_upper_limit] TEXT, [upper_limit] TEXT)",
    "CREATE TABLE [tbl_restrictive_airspace] ([area_code] TEXT, [icao_code] TEXT, [restrictive_airspace_designation] TEXT, [restrictive_airspace_name] TEXT, [restrictive_type] TEXT, [multiple_code] TEXT, [seqno] INTEGER, [boundary_via] TEXT, [flightlevel] TEXT, [latitude] DOUBLE, [longitude] DOUBLE, [arc_origin_latitude] DOUBLE, [arc_origin_longitude] DOUBLE, [arc_distance] DOUBLE, [arc_bearing] DOUBLE, [unit_indicator_lower_limit] TEXT, [lower_limit] TEXT, [unit_indicator_upper_limit] TEXT, [upper_limit] TEXT)",
    "CREATE TABLE [tbl_fir_uir] ([area_code] TEXT, [fir_uir_identifier] TEXT, [fir_uir_address] TEXT, [fir_uir_name] TEXT, [fir_uir_indicator] TEXT, [seqno] INTEGER, [boundary_via] TEXT, [adjacent_fir_identifier] TEXT, [adjacent_uir_identifier] TEXT, [reporting_units_speed] INTEGER, [reporting_units_altitude] INTEGER, [fir_uir_latitude] DOUBLE, [fir_uir_longitude] DOUBLE, [arc_origin_latitude] DOUBLE, [arc_origin_longitude] DOUBLE, [arc_distance] DOUBLE, [arc_bearing] DOUBLE, [fir_upper_limit] TEXT, [uir_lower_limit] TEXT, [uir_upper_limit] TEXT, [cruise_table_identifier] TEXT)",
    "CREATE TABLE [tbl_airport_communication] ([area_code] TEXT, [icao_code] TEXT, [airport_identifier] TEXT, [communication_type] TEXT, [communication_frequency] DOUBLE, [frequency_units] TEXT, [service_indicator] TEXT, [callsign] TEXT, [latitude] DOUBLE, [longitude] DOUBLE)",
    "CREATE TABLE [tbl_enroute_communication] ([area_code] TEXT, [fir_rdo_ident] TEXT, [fir_uir_indicator] TEXT, [communication_type] TEXT, [communication_frequency] DOUBLE, [frequency_units] TEXT, [service_indicator] TEXT, [remote_name] TEXT, [callsign] TEXT, [latitude] DOUBLE, [longitude] DOUBLE)",
    "CREATE TABLE [tbl_localizer_marker] ([area_code] TEXT, [icao_code] TEXT, [airport_identifier] TEXT, [runway_identifier] TEXT, [llz_identifier] TEXT, [marker_identifier] TEXT, [marker_type] TEXT, [marker_latitude] DOUBLE, [marker_longitude] DOUBLE, [id] TEXT)",
    "CREATE TABLE [tbl_gls] ([area_code] TEXT, [airport_identifier] TEXT, [icao_code] TEXT, [gls_ref_path_identifier] TEXT, [gls_category] TEXT, [gls_channel] INTEGER, [runway_identifier] TEXT, [gls_approach_bearing] DOUBLE, [station_latitude] DOUBLE, [station_longitude] DOUBLE, [gls_station_ident] TEXT, [gls_approach_slope] DOUBLE, [magentic_variation] DOUBLE, [station_elevation] INTEGER, [station_type] TEXT, [id] TEXT)",
    "CREATE TABLE [tbl_pathpoints] ([area_code] TEXT, [airport_identifier] TEXT, [icao_code] TEXT, [approach_procedure_ident] TEXT, [runway_identifier] TEXT, [sbas_service_provider_identifier] TEXT, [reference_path_identifier] TEXT, [landing_threshold_latitude] DOUBLE, [landing_threshold_longitude] DOUBLE, [ltp_ellipsoid_height] DOUBLE, [glidepath_angle] DOUBLE, [flightpath_alignment_latitude] DOUBLE, [flightpath_alignment_longitude] DOUBLE, [course_width_at_threshold] DOUBLE, [length_offset] DOUBLE, [path_point_tch] DOUBLE, [tch_units_indicator] TEXT, [hal] DOUBLE, [val] DOUBLE, [fpap_ellipsoid_height] DOUBLE, [fpap_orthometric_height] DOUBLE, [ltp_orthometric_height] DOUBLE, [approach_type_identifier] TEXT, [gnss_channel_number] INTEGER)",
    "CREATE TABLE [tbl_gate] ([area_code] TEXT, [airport_identifier] TEXT, [icao_code] TEXT, [gate_identifier] TEXT, [gate_latitude] DOUBLE, [gate_longitude] DOUBLE, [name] TEXT)",
    "CREATE TABLE [tbl_cruising_tables] ([cruise_table_identifier] TEXT, [seqno] INTEGER, [course_from] DOUBLE, [course_to] DOUBLE, [mag_true] TEXT, [cruise_level_from1] TEXT, [vertical_separation1] TEXT, [cruise_level_to1] TEXT, [cruise_level_from2] TEXT, [vertical_separation2] TEXT, [cruise_level_to2] TEXT, [cruise_level_from3] TEXT, [vertical_separation3] TEXT, [cruise_level_to3] TEXT, [cruise_level_from4] TEXT, [vertical_separation4] TEXT, [cruise_level_to4] TEXT)",
];

/// The columns `tbl_sids`, `tbl_stars` and `tbl_iaps` share. Written once and reused for all
/// three, since in this layout the three differ only in which table a procedure lands in.
const PROC_COLUMNS: &str = "[area_code] TEXT, [airport_identifier] TEXT, [procedure_identifier] TEXT, [route_type] TEXT, [transition_identifier] TEXT, [seqno] INTEGER, \
     [waypoint_icao_code] TEXT, [waypoint_identifier] TEXT, [waypoint_latitude] DOUBLE, [waypoint_longitude] DOUBLE, [waypoint_description_code] TEXT, [turn_direction] TEXT, \
     [rnp] DOUBLE, [path_termination] TEXT, [recommanded_navaid] TEXT, [recommanded_navaid_latitude] DOUBLE, [recommanded_navaid_longitude] DOUBLE, [arc_radius] DOUBLE, \
     [theta] DOUBLE, [rho] DOUBLE, [magnetic_course] DOUBLE, [route_distance_holding_distance_time] DOUBLE, [distance_time] TEXT, [altitude_description] TEXT, \
     [altitude1] INTEGER, [altitude2] INTEGER, [transition_altitude] INTEGER, [speed_limit_description] TEXT, [speed_limit] INTEGER, [vertical_angle] DOUBLE, \
     [center_waypoint] TEXT, [center_waypoint_latitude] DOUBLE, [center_waypoint_longitude] DOUBLE, [aircraft_category] TEXT, [id] TEXT, [recommanded_id] TEXT, [center_id] TEXT";

const PROC_TABLES: [(&str, ProcKind); 3] = [("tbl_sids", ProcKind::Sid), ("tbl_stars", ProcKind::Star), ("tbl_iaps", ProcKind::Approach)];

/// Which table a procedure belongs in.
/// Every fix and beacon by identifier, so a writer can place a leg without searching.
fn position_index(nav: &NavSet) -> HashMap<&str, (f64, f64)> {
    let mut out: HashMap<&str, (f64, f64)> = HashMap::with_capacity(nav.waypoints.len() + nav.navaids.len());
    for n in &nav.navaids {
        out.insert(n.ident.as_str(), (n.lat, n.lon));
    }
    // A waypoint of the same name as a beacon wins, since a procedure leg naming a fix means
    // the fix.
    for w in &nav.waypoints {
        out.insert(w.ident.as_str(), (w.lat, w.lon));
    }
    out
}

/// Every airport's area and region code by identifier, for the same reason.
fn airport_index(nav: &NavSet) -> HashMap<&str, (&str, &str, Option<f64>)> {
    nav.airports.iter().map(|a| (a.icao.as_str(), (a.area_code.as_str(), a.icao_code.as_str(), a.transition_altitude_ft))).collect()
}

fn proc_table(k: ProcKind) -> &'static str {
    match k {
        ProcKind::Sid => "tbl_sids",
        ProcKind::Star => "tbl_stars",
        ProcKind::Approach => "tbl_iaps",
    }
}

/// ARINC 424's own one-letter surface code.
fn surface_code(s: Surface) -> &'static str {
    match s {
        Surface::Asphalt => "ASPH",
        Surface::Concrete => "CONC",
        Surface::Gravel => "GRVL",
        Surface::Grass => "GRAS",
        Surface::Water => "WATE",
        Surface::Ice => "ICE",
        Surface::Snow => "SNOW",
        Surface::Unknown => "UNKN",
    }
}

/// The category a chart prints beside a localiser, as this layout spells it.
fn ils_category(c: IlsCategory) -> &'static str {
    match c {
        IlsCategory::Loc => "0",
        IlsCategory::Cat1 => "1",
        IlsCategory::Cat2 => "2",
        IlsCategory::Cat3 => "3",
    }
}

/// The `navaid_class` field: five characters describing what a transmitter actually is. Only
/// the first two carry meaning for a reader picking a tuned aid apart from a DME, so the rest
/// are left as the spaces the format pads them with.
fn navaid_class(k: NavaidKind) -> &'static str {
    match k {
        NavaidKind::Vor => "V    ",
        NavaidKind::VorDme => "VD   ",
        NavaidKind::Vortac => "VT   ",
        NavaidKind::Tacan => "T    ",
        NavaidKind::Dme => "D    ",
        NavaidKind::IlsDme => "ID   ",
        NavaidKind::Ndb => "H    ",
        NavaidKind::NdbDme => "HD   ",
    }
}

fn level_code(l: AirwayLevel) -> &'static str {
    match l {
        AirwayLevel::Both => "B",
        AirwayLevel::High => "H",
        AirwayLevel::Low => "L",
    }
}

/// The route type character, by the same reasoning `fenix::route_type` sets out: a free-text
/// field with no lookup behind it, worth getting close to ARINC's own convention without
/// pretending to certainty.
fn route_type(p: &ProcedureRec) -> &'static str {
    match p.kind {
        ProcKind::Sid if p.transition_ident.is_some() => "4",
        ProcKind::Sid if p.runway_ident.is_some() => "2",
        ProcKind::Sid => "3",
        ProcKind::Star if p.transition_ident.is_some() => "1",
        ProcKind::Star => "3",
        // An approach's route type is the letter its own identifier begins with — I for an
        // ILS, R for an RNAV, D for a VOR/DME and so on — which the procedure's ident
        // already carries, so it is derived there rather than guessed here.
        ProcKind::Approach => "",
    }
}

/// An approach's route type taken from its published identifier's first letter, which is the
/// convention this crate's approach handling already leans on elsewhere.
fn approach_route_type(ident: &str) -> String {
    match ident.chars().next().unwrap_or(' ') {
        'I' => "I".to_string(),
        'L' => "L".to_string(),
        'R' => "R".to_string(),
        'D' => "D".to_string(),
        'V' => "V".to_string(),
        'N' => "N".to_string(),
        'S' => "S".to_string(),
        'X' => "X".to_string(),
        'G' => "G".to_string(),
        'P' => "P".to_string(),
        other => other.to_string(),
    }
}

/// The description code every fix in a procedure carries: four characters saying what part
/// the fix plays. Only the roles this crate actually decodes from the simulator's data are
/// set; the rest stay blank rather than being invented.
fn description_code(is_end: bool, is_faf: bool, is_map: bool, is_iaf: bool, is_if: bool, is_flyover: bool) -> String {
    let mut c = [b' '; 4];
    if is_end {
        c[0] = b'E';
    }
    if is_flyover {
        c[1] = b'Y';
    }
    c[3] = if is_faf {
        b'F'
    } else if is_map {
        b'M'
    } else if is_iaf {
        b'A'
    } else if is_if {
        b'B'
    } else {
        b' '
    };
    String::from_utf8_lossy(&c).into_owned()
}

/// Write a whole `NavSet` as a DFD database at `path`, replacing anything already there.
///
/// The caller is expected to have taken a backup ([`crate::convert::backup`]) if the path is
/// a database an aircraft is actually using: this function's job is to produce a correct
/// file, not to decide whether overwriting one is wise.
pub fn write(nav: &NavSet, path: &Path) -> Result<Vec<TableReport>> {
    if path.exists() {
        std::fs::remove_file(path).with_context(|| format!("remove existing {}", path.display()))?;
    }
    let mut conn = Connection::open(path).with_context(|| format!("create {}", path.display()))?;
    create_schema(&conn)?;
    let tx = conn.transaction()?;
    let mut counts: HashMap<&'static str, usize> = HashMap::new();

    write_header(&tx, nav)?;
    counts.insert("tbl_header", 1);

    counts.insert("tbl_airports", write_airports(&tx, nav)?);
    counts.insert("tbl_runways", write_runways(&tx, nav)?);
    let (vhf, enroute_ndb, terminal_ndb) = write_navaids(&tx, nav)?;
    counts.insert("tbl_vhfnavaids", vhf);
    counts.insert("tbl_enroute_ndbnavaids", enroute_ndb);
    counts.insert("tbl_terminal_ndbnavaids", terminal_ndb);
    let (enroute_wp, terminal_wp) = write_waypoints(&tx, nav)?;
    counts.insert("tbl_enroute_waypoints", enroute_wp);
    counts.insert("tbl_terminal_waypoints", terminal_wp);
    counts.insert("tbl_localizers_glideslopes", write_ils(&tx, nav)?);
    counts.insert("tbl_enroute_airways", write_airways(&tx, nav)?);
    counts.insert("tbl_grid_mora", write_mora(&tx, nav)?);
    for (table, kind) in PROC_TABLES {
        counts.insert(table, write_procedures(&tx, nav, kind)?);
    }

    tx.commit()?;
    Ok(report_with(&counts))
}

/// What [`write`] would produce, without touching disk: every count here is a plain function
/// of the model, so a dry run is exact rather than an estimate.
pub fn plan(nav: &NavSet) -> Vec<TableReport> {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    counts.insert("tbl_header", 1);
    counts.insert("tbl_airports", nav.airports.len());
    counts.insert("tbl_runways", nav.runways.len());
    counts.insert("tbl_vhfnavaids", nav.navaids.iter().filter(|n| !is_ndb(n.kind)).count());
    let ndbs: Vec<_> = nav.navaids.iter().filter(|n| is_ndb(n.kind)).collect();
    counts.insert("tbl_enroute_ndbnavaids", ndbs.len());
    counts.insert("tbl_terminal_ndbnavaids", 0);
    counts.insert("tbl_enroute_waypoints", nav.waypoints.len());
    counts.insert("tbl_terminal_waypoints", 0);
    counts.insert("tbl_localizers_glideslopes", nav.ils.len());
    counts.insert("tbl_enroute_airways", nav.airways.iter().map(|a| a.legs.len() + 1).sum());
    counts.insert("tbl_grid_mora", mora_rows(nav));
    for (table, kind) in PROC_TABLES {
        counts.insert(table, nav.procedures.iter().filter(|p| p.kind == kind).map(|p| p.legs.len()).sum());
    }
    report_with(&counts)
}

fn is_ndb(k: NavaidKind) -> bool {
    matches!(k, NavaidKind::Ndb | NavaidKind::NdbDme)
}

fn report_with(counts: &HashMap<&'static str, usize>) -> Vec<TableReport> {
    let ordered = [
        "tbl_header",
        "tbl_airports",
        "tbl_runways",
        "tbl_vhfnavaids",
        "tbl_enroute_ndbnavaids",
        "tbl_terminal_ndbnavaids",
        "tbl_enroute_waypoints",
        "tbl_terminal_waypoints",
        "tbl_localizers_glideslopes",
        "tbl_enroute_airways",
        "tbl_sids",
        "tbl_stars",
        "tbl_iaps",
        "tbl_grid_mora",
    ];
    let mut out: Vec<TableReport> = ordered.iter().map(|t| TableReport::filled(t, counts.get(t).copied().unwrap_or(0))).collect();
    out.extend(OMITTED.iter().map(|(t, why)| TableReport::empty(t, why)));
    out
}

fn create_schema(conn: &Connection) -> Result<()> {
    for sql in SCHEMA {
        conn.execute_batch(sql).with_context(|| format!("create a table: {}", &sql[..sql.len().min(60)]))?;
    }
    for (table, _) in PROC_TABLES {
        conn.execute_batch(&format!("CREATE TABLE [{table}] ({PROC_COLUMNS})")).with_context(|| format!("create {table}"))?;
    }
    Ok(())
}

/// The cycle a database says it holds. The three aircraft that read this layout each carry a
/// slightly different header — Navigraph's own writes `creator` and `dataset`, PMDG's writes
/// `arincversion` and `current_airac` — so every column both shapes have is filled, and a
/// reader looking for either finds what it expects.
fn write_header(conn: &Connection, nav: &NavSet) -> Result<()> {
    conn.execute(
        "INSERT INTO tbl_header (version, arincversion, record_set, current_airac, revision, effective_fromto, previous_airac, previous_fromto, parsed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params!["1.16", "424-18", "extended", nav.cycle, "1", cycle_window(&nav.cycle), "", "", chrono::Utc::now().format("%Y-%m-%d %H:%M:%SZ").to_string()],
    )?;
    Ok(())
}

/// The `effective_fromto` field: the two two-digit day-of-cycle figures the format packs
/// together. Without knowing a cycle's real dates this cannot be computed honestly, so the
/// cycle's own digits are repeated, which is what an unknown window looks like rather than a
/// plausible-looking invention.
fn cycle_window(cycle: &str) -> String {
    format!("{cycle}{cycle}")
}

fn write_airports(conn: &Connection, nav: &NavSet) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT INTO tbl_airports (area_code, icao_code, airport_identifier, airport_identifier_3letter, airport_name, airport_ref_latitude, airport_ref_longitude, \
         ifr_capability, longest_runway_surface_code, elevation, transition_altitude, transition_level, speed_limit, speed_limit_altitude, iata_ata_designator, id) \
         VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, 'Y', ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?13)",
    )?;
    let mut longest: HashMap<&str, (f64, Surface)> = HashMap::new();
    for r in &nav.runways {
        let e = longest.entry(r.airport_icao.as_str()).or_insert((0.0, Surface::Unknown));
        if r.length_ft > e.0 {
            *e = (r.length_ft, r.surface);
        }
    }
    for a in &nav.airports {
        let surface = longest.get(a.icao.as_str()).map(|(_, s)| surface_code(*s)).unwrap_or("UNKN");
        stmt.execute(params![
            a.area_code,
            a.icao_code,
            a.icao,
            a.name,
            a.lat,
            a.lon,
            surface,
            a.elevation_ft.round() as i64,
            a.transition_altitude_ft.map(|v| v.round() as i64),
            a.transition_level_ft.map(|v| v.round() as i64),
            a.speed_limit_kt.map(|v| v.round() as i64),
            a.speed_limit_altitude_ft.map(|v| v.round() as i64),
            a.icao,
        ])?;
    }
    Ok(nav.airports.len())
}

fn write_runways(conn: &Connection, nav: &NavSet) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT INTO tbl_runways (area_code, icao_code, airport_identifier, runway_identifier, runway_latitude, runway_longitude, runway_gradient, \
         runway_magnetic_bearing, runway_true_bearing, landing_threshold_elevation, displaced_threshold_distance, threshold_crossing_height, \
         runway_length, runway_width, llz_identifier, llz_mls_gls_category, surface_code, id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, 0, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
    )?;
    // The localiser serving each end, so a runway row names it the way the format does.
    let mut llz: HashMap<(&str, &str), (&str, IlsCategory, Option<f64>)> = HashMap::new();
    for i in &nav.ils {
        llz.insert((i.airport_icao.as_str(), i.runway_ident.as_str()), (i.ident.as_str(), i.category, i.crossing_height_ft));
    }
    let airports = airport_index(nav);
    for r in &nav.runways {
        let found = llz.get(&(r.airport_icao.as_str(), r.ident.as_str()));
        let area = airports.get(r.airport_icao.as_str());
        stmt.execute(params![
            area.map(|a| a.0).unwrap_or("USA"),
            area.map(|a| a.1).unwrap_or(""),
            r.airport_icao,
            format!("RW{}", r.ident),
            r.lat,
            r.lon,
            r.heading_true_deg,
            r.heading_true_deg,
            r.elevation_ft.round() as i64,
            found.and_then(|(_, _, tch)| tch.map(|v| v.round() as i64)),
            r.length_ft.round() as i64,
            r.width_ft.round() as i64,
            found.map(|(id, _, _)| *id),
            found.map(|(_, c, _)| ils_category(*c)),
            surface_code(r.surface),
            format!("{}|RW{}", r.airport_icao, r.ident),
        ])?;
    }
    Ok(nav.runways.len())
}

/// The tuned aids, split as the layout splits them: everything that is not an NDB into
/// `tbl_vhfnavaids`, and the NDBs into the enroute table. A terminal NDB is one an approach
/// is written against, which this model does not distinguish, so that table is left empty
/// rather than guessed at — an aircraft reading it finds the same beacon in the enroute
/// table, which is where a reader looks first.
fn write_navaids(conn: &Connection, nav: &NavSet) -> Result<(usize, usize, usize)> {
    let mut vhf = conn.prepare(
        "INSERT INTO tbl_vhfnavaids (area_code, airport_identifier, icao_code, vor_identifier, vor_name, vor_frequency, navaid_class, vor_latitude, vor_longitude, \
         dme_ident, dme_latitude, dme_longitude, dme_elevation, ilsdme_bias, range, station_declination, magnetic_variation, id) \
         VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, ?14, ?15, ?16)",
    )?;
    let mut ndb = conn.prepare(
        "INSERT INTO tbl_enroute_ndbnavaids (area_code, icao_code, ndb_identifier, ndb_name, ndb_frequency, navaid_class, ndb_latitude, ndb_longitude, range, id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    let (mut n_vhf, mut n_ndb) = (0, 0);
    for n in &nav.navaids {
        let id = format!("{}|{}", n.region_code, n.ident);
        if is_ndb(n.kind) {
            ndb.execute(params![n.area_code, n.region_code, n.ident, n.name, n.frequency, navaid_class(n.kind), n.lat, n.lon, n.range_nm.map(|v| v.round() as i64), id])?;
            n_ndb += 1;
        } else {
            let has_dme = matches!(n.kind, NavaidKind::VorDme | NavaidKind::Vortac | NavaidKind::Dme | NavaidKind::IlsDme | NavaidKind::Tacan);
            vhf.execute(params![
                n.area_code,
                n.region_code,
                n.ident,
                n.name,
                n.frequency,
                navaid_class(n.kind),
                n.lat,
                n.lon,
                has_dme.then_some(n.ident.as_str()),
                has_dme.then_some(n.lat),
                has_dme.then_some(n.lon),
                has_dme.then_some(n.elevation_ft.round() as i64),
                n.range_nm.map(|v| v.round() as i64),
                n.magnetic_variation_deg,
                n.magnetic_variation_deg,
                id,
            ])?;
            n_vhf += 1;
        }
    }
    Ok((n_vhf, n_ndb, 0))
}

/// The fixes. This layout keeps enroute and terminal waypoints in separate tables, and the
/// model does not say which a fix is, so all of them are written as enroute: a reader looking
/// for a terminal fix by identifier finds it there, where a missing row would leave a
/// procedure leg pointing at nothing.
fn write_waypoints(conn: &Connection, nav: &NavSet) -> Result<(usize, usize)> {
    let mut stmt = conn.prepare(
        "INSERT INTO tbl_enroute_waypoints (area_code, icao_code, waypoint_identifier, waypoint_name, waypoint_type, waypoint_usage, waypoint_latitude, waypoint_longitude, id) \
         VALUES (?1, ?2, ?3, ?4, 'R  ', 'RB', ?5, ?6, ?7)",
    )?;
    for w in &nav.waypoints {
        stmt.execute(params![w.area_code, w.region_code, w.ident, w.name.as_deref().unwrap_or(w.ident.as_str()), w.lat, w.lon, format!("{}|{}", w.region_code, w.ident)])?;
    }
    Ok((nav.waypoints.len(), 0))
}

fn write_ils(conn: &Connection, nav: &NavSet) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT INTO tbl_localizers_glideslopes (area_code, icao_code, airport_identifier, runway_identifier, llz_identifier, llz_latitude, llz_longitude, \
         llz_frequency, llz_bearing, llz_width, ils_mls_gls_category, gs_latitude, gs_longitude, gs_angle, gs_elevation, station_declination, id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 3.5, ?10, ?11, ?12, ?13, ?14, 0, ?15)",
    )?;
    let airports = airport_index(nav);
    for i in &nav.ils {
        let area = airports.get(i.airport_icao.as_str());
        stmt.execute(params![
            area.map(|a| a.0).unwrap_or("USA"),
            area.map(|a| a.1).unwrap_or(""),
            i.airport_icao,
            format!("RW{}", i.runway_ident),
            i.ident,
            i.lat,
            i.lon,
            i.frequency_mhz,
            i.course_mag_deg,
            ils_category(i.category),
            i.glidepath_deg.map(|_| i.lat),
            i.glidepath_deg.map(|_| i.lon),
            i.glidepath_deg,
            i.elevation_ft.round() as i64,
            format!("{}|RW{}", i.airport_icao, i.runway_ident),
        ])?;
    }
    Ok(nav.ils.len())
}

/// The airways, one row per fix along each rather than one per segment: this layout walks an
/// airway as an ordered list of the fixes on it, so a run of n segments is n+1 rows, the
/// first marked as the start and the last as the end in its description code.
fn write_airways(conn: &Connection, nav: &NavSet) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT INTO tbl_enroute_airways (area_code, route_identifier, seqno, icao_code, waypoint_identifier, waypoint_latitude, waypoint_longitude, \
         waypoint_description_code, route_type, flightlevel, direction_restriction, crusing_table_identifier, minimum_altitude1, minimum_altitude2, \
         maximum_altitude, outbound_course, inbound_course, inbound_distance, id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'O', ?9, NULL, NULL, ?10, NULL, NULL, 0, 0, 0, ?11)",
    )?;
    // Every fix's position, indexed once. Searching the waypoint list for each leg is a
    // linear scan of a quarter of a million records, and there are hundreds of thousands of
    // legs: it turned a ten-second write into a six-minute one.
    let index = position_index(nav);
    let position = |ident: &str, _region: &str| -> Option<(f64, f64)> { index.get(ident).copied() };
    let mut written = 0usize;
    for a in &nav.airways {
        // Each leg names where it starts; the last leg's end closes the list.
        let mut seq = 10u32;
        for (i, leg) in a.legs.iter().enumerate() {
            let last = i + 1 == a.legs.len();
            for (ident, region, is_first, is_last) in [(&leg.from_ident, &leg.from_region, i == 0, false)].into_iter().chain(last.then_some((&leg.to_ident, &leg.to_region, false, true))) {
                let Some((lat, lon)) = position(ident, region) else { continue };
                let code = if is_first {
                    "EB  "
                } else if is_last {
                    "EE  "
                } else {
                    "E   "
                };
                stmt.execute(params![
                    "USA",
                    a.ident,
                    seq,
                    region,
                    ident,
                    lat,
                    lon,
                    code,
                    level_code(leg.level),
                    leg.min_ft.map(|v| v.round() as i64),
                    format!("{}|{}", a.ident, seq)
                ])?;
                seq += 10;
                written += 1;
            }
        }
    }
    Ok(written)
}

/// How many rows a MORA grid becomes: one per starting latitude and each block of thirty
/// degrees of longitude, since that is how the format packs it.
fn mora_rows(nav: &NavSet) -> usize {
    let mut keys: Vec<(i64, i64)> = nav.mora.iter().map(|m| (m.lat.floor() as i64, (m.lon.floor() as i64).div_euclid(30) * 30)).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.len()
}

fn write_mora(conn: &Connection, nav: &NavSet) -> Result<usize> {
    let columns: String = (1..=30).map(|i| format!("mora{i:02}")).collect::<Vec<_>>().join(", ");
    let holes: String = (3..=32).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
    let mut stmt = conn.prepare(&format!("INSERT INTO tbl_grid_mora (starting_latitude, starting_longitude, {columns}) VALUES (?1, ?2, {holes})"))?;
    // A row is a latitude and a block of thirty degrees of longitude; a cell the model has
    // nothing for stays null rather than being filled with a floor that was never computed.
    let mut rows: HashMap<(i64, i64), [Option<i64>; 30]> = HashMap::new();
    for m in &nav.mora {
        let lat = m.lat.floor() as i64;
        let lon = m.lon.floor() as i64;
        let block = lon.div_euclid(30) * 30;
        let slot = (lon - block) as usize;
        if slot < 30 {
            rows.entry((lat, block)).or_insert([None; 30])[slot] = Some((m.altitude_ft / 100.0).round() as i64);
        }
    }
    let mut keys: Vec<_> = rows.keys().copied().collect();
    keys.sort_unstable();
    for key in &keys {
        let values = &rows[key];
        let mut bound: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(key.0), Box::new(key.1)];
        for v in values.iter() {
            bound.push(Box::new(*v));
        }
        stmt.execute(rusqlite::params_from_iter(bound.iter().map(|b| b.as_ref())))?;
    }
    Ok(keys.len())
}

/// The legs of every procedure of one kind. All three tables share a shape, so this writes
/// whichever of them the kind belongs in.
fn write_procedures(conn: &Connection, nav: &NavSet, kind: ProcKind) -> Result<usize> {
    let table = proc_table(kind);
    let mut stmt = conn.prepare(&format!(
        "INSERT INTO [{table}] (area_code, airport_identifier, procedure_identifier, route_type, transition_identifier, seqno, waypoint_icao_code, \
         waypoint_identifier, waypoint_latitude, waypoint_longitude, waypoint_description_code, turn_direction, rnp, path_termination, recommanded_navaid, \
         recommanded_navaid_latitude, recommanded_navaid_longitude, arc_radius, theta, rho, magnetic_course, route_distance_holding_distance_time, \
         distance_time, altitude_description, altitude1, altitude2, transition_altitude, speed_limit_description, speed_limit, vertical_angle, \
         center_waypoint, center_waypoint_latitude, center_waypoint_longitude, aircraft_category, id, recommanded_id, center_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, NULL, ?22, ?23, ?24, ?25, ?26, ?27, NULL, ?28, ?29, ?30, NULL, ?31, ?32, ?33)"
    ))?;
    let index = position_index(nav);
    let position = |ident: &str| -> Option<(f64, f64)> { index.get(ident).copied() };
    let mut written = 0usize;
    let airports = airport_index(nav);
    for p in nav.procedures.iter().filter(|p| p.kind == kind) {
        let area = airports.get(p.airport_icao.as_str());
        let rt = if p.kind == ProcKind::Approach { approach_route_type(&p.ident) } else { route_type(p).to_string() };
        let transition_alt = area.and_then(|a| a.2).map(|v| v.round() as i64);
        for (i, leg) in p.legs.iter().enumerate() {
            let last = i + 1 == p.legs.len();
            let fix = leg.fix_ident.as_deref();
            let fix_pos = fix.and_then(position);
            let nav_pos = leg.recommended_navaid.as_deref().and_then(position);
            let centre_pos = leg.center_fix.as_deref().and_then(position);
            stmt.execute(params![
                area.map(|a| a.0).unwrap_or("USA"),
                p.airport_icao,
                p.ident,
                rt,
                p.transition_ident.as_deref().unwrap_or(""),
                ((i + 1) * 10) as i64,
                leg.fix_region.as_deref(),
                fix,
                fix_pos.map(|(lat, _)| lat),
                fix_pos.map(|(_, lon)| lon),
                description_code(last, leg.is_faf, leg.is_map, leg.is_iaf, leg.is_if, leg.is_flyover),
                turn_code(leg.turn),
                leg.path_terminator.code(),
                leg.recommended_navaid.as_deref(),
                nav_pos.map(|(lat, _)| lat),
                nav_pos.map(|(_, lon)| lon),
                arc_radius_nm(centre_pos, fix_pos),
                leg.theta_deg,
                leg.rho_nm,
                leg.course_deg,
                leg.leg_length,
                leg.altitude_rule.map(altitude_code),
                leg.altitude1_ft.map(|v| v.round() as i64),
                leg.altitude2_ft.map(|v| v.round() as i64),
                transition_alt,
                leg.speed_limit_kt.map(|_| "-"),
                leg.speed_limit_kt.map(|v| v.round() as i64),
                leg.center_fix.as_deref(),
                centre_pos.map(|(lat, _)| lat),
                centre_pos.map(|(_, lon)| lon),
                format!("{}|{}|{}", p.airport_icao, p.ident, (i + 1) * 10),
                leg.recommended_navaid.as_deref().map(|n| format!("{}|{n}", leg.recommended_navaid_region.as_deref().unwrap_or(""))),
                leg.center_fix.as_deref().map(|c| format!("|{c}")),
            ])?;
            written += 1;
        }
    }
    Ok(written)
}

/// The radius of a constant-radius or DME-arc leg: the model carries the centre of the turn
/// but not its radius, because the radius is not a separate fact — it is how far the fix is
/// from that centre, which is exactly what a reader flying the arc needs.
fn arc_radius_nm(centre: Option<(f64, f64)>, fix: Option<(f64, f64)>) -> Option<f64> {
    let (c, f) = (centre?, fix?);
    Some(crate::dispatch::distance_nm(c, f))
}

fn turn_code(t: crate::convert::model::TurnDirection) -> Option<&'static str> {
    use crate::convert::model::TurnDirection;
    match t {
        TurnDirection::Left => Some("L"),
        TurnDirection::Right => Some("R"),
        TurnDirection::Either => None,
    }
}

fn altitude_code(r: crate::convert::model::AltitudeRule) -> &'static str {
    use crate::convert::model::AltitudeRule;
    match r {
        AltitudeRule::At => "@",
        AltitudeRule::AtOrAbove => "+",
        AltitudeRule::AtOrBelow => "-",
        AltitudeRule::Between => "B",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::model::*;

    fn sample() -> NavSet {
        NavSet {
            cycle: "2603".to_string(),
            airports: vec![AirportRec {
                icao: "EGLL".into(),
                name: "LONDON HEATHROW".into(),
                area_code: "EUR".into(),
                icao_code: "EG".into(),
                lat: 51.4706,
                lon: -0.4619,
                elevation_ft: 83.0,
                transition_altitude_ft: Some(6000.0),
                transition_level_ft: None,
                speed_limit_kt: Some(250.0),
                speed_limit_altitude_ft: Some(10000.0),
                ..Default::default()
            }],
            runways: vec![RunwayRec {
                airport_icao: "EGLL".into(),
                ident: "27R".into(),
                heading_true_deg: 269.7,
                length_ft: 12008.0,
                width_ft: 164.0,
                surface: Surface::Asphalt,
                lat: 51.4775,
                lon: -0.4335,
                elevation_ft: 78.0,
                ..Default::default()
            }],
            navaids: vec![
                NavaidRec { ident: "LON".into(), kind: NavaidKind::VorDme, name: "LONDON".into(), region_code: "EG".into(), area_code: "EUR".into(), frequency: 113.6, lat: 51.5, lon: -0.46, elevation_ft: 80.0, magnetic_variation_deg: -1.0, range_nm: Some(60.0), ..Default::default() },
                NavaidRec { ident: "BIG".into(), kind: NavaidKind::Ndb, name: "BIGGIN".into(), region_code: "EG".into(), area_code: "EUR".into(), frequency: 375.0, lat: 51.33, lon: 0.03, elevation_ft: 0.0, magnetic_variation_deg: -1.0, range_nm: None, ..Default::default() },
            ],
            ils: vec![IlsRec {
                ident: "ILL".into(),
                airport_icao: "EGLL".into(),
                runway_ident: "27R".into(),
                frequency_mhz: 109.5,
                course_mag_deg: 270.0,
                glidepath_deg: Some(3.0),
                category: IlsCategory::Cat3,
                lat: 51.4775,
                lon: -0.45,
                elevation_ft: 78.0,
                has_dme: true,
                crossing_height_ft: Some(56.0),
                ..Default::default()
            }],
            waypoints: vec![
                WaypointRec { ident: "DET".into(), region_code: "EG".into(), area_code: "EUR".into(), name: Some("DETLING".into()), lat: 51.30, lon: 0.60, ..Default::default() },
                WaypointRec { ident: "MAY".into(), region_code: "EG".into(), area_code: "EUR".into(), name: Some("MAYFIELD".into()), lat: 51.01, lon: 0.12, ..Default::default() },
            ],
            airways: vec![AirwayRec {
                ident: "L620".into(),
                legs: vec![AirwayLegRec { sequence: 1, from_ident: "DET".into(), from_region: "EG".into(), to_ident: "MAY".into(), to_region: "EG".into(), level: AirwayLevel::Both, is_start: true, is_end: true, min_ft: Some(4500.0) }],
            }],
            procedures: vec![ProcedureRec {
                airport_icao: "EGLL".into(),
                kind: ProcKind::Sid,
                ident: "DET2F".into(),
                transition_ident: None,
                runway_ident: Some("27R".into()),
                legs: vec![LegRec {
                    sequence: 1,
                    path_terminator: PathTerminator::Tf,
                    fix_ident: Some("DET".into()),
                    fix_region: Some("EG".into()),
                    altitude_rule: Some(AltitudeRule::AtOrAbove),
                    altitude1_ft: Some(6000.0),
                    speed_limit_kt: Some(250.0),
                    turn: TurnDirection::Right,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            mora: vec![MoraRec { lat: 51.0, lon: 0.0, altitude_ft: 2300.0 }, MoraRec { lat: 51.0, lon: 1.0, altitude_ft: 2400.0 }],
        }
    }

    #[test]
    fn a_written_database_reads_back_with_every_table_present() {
        let dir = std::env::temp_dir().join(format!("amdbgen-dfd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dfd.s3db");
        let report = write(&sample(), &path).expect("write");
        let conn = Connection::open(&path).unwrap();
        // Every table of the layout exists, whether or not this writer fills it.
        let count: i64 = conn.query_row("select count(*) from sqlite_master where type='table'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 27, "all twenty-seven tables of the layout");
        let airports: i64 = conn.query_row("select count(*) from tbl_airports", [], |r| r.get(0)).unwrap();
        assert_eq!(airports, 1);
        let ident: String = conn.query_row("select runway_identifier from tbl_runways", [], |r| r.get(0)).unwrap();
        assert_eq!(ident, "RW27R", "a runway is named the way the format names it");
        let vhf: i64 = conn.query_row("select count(*) from tbl_vhfnavaids", [], |r| r.get(0)).unwrap();
        let ndb: i64 = conn.query_row("select count(*) from tbl_enroute_ndbnavaids", [], |r| r.get(0)).unwrap();
        assert_eq!((vhf, ndb), (1, 1), "an NDB goes in its own table");
        let sids: i64 = conn.query_row("select count(*) from tbl_sids", [], |r| r.get(0)).unwrap();
        assert_eq!(sids, 1);
        let cycle: String = conn.query_row("select current_airac from tbl_header", [], |r| r.get(0)).unwrap();
        assert_eq!(cycle, "2603");
        // What it says it left out is what it actually left out.
        for omitted in report.iter().filter(|r| r.omitted_because.is_some()) {
            let rows: i64 = conn.query_row(&format!("select count(*) from [{}]", omitted.table), [], |r| r.get(0)).unwrap();
            assert_eq!(rows, 0, "{} was reported empty", omitted.table);
        }
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_airway_becomes_one_row_per_fix_not_one_per_segment() {
        let nav = sample();
        let planned = plan(&nav);
        let airways = planned.iter().find(|r| r.table == "tbl_enroute_airways").unwrap();
        assert_eq!(airways.rows, 2, "one segment is two fixes");
    }

    #[test]
    fn a_mora_row_packs_thirty_degrees_of_longitude() {
        let nav = sample();
        assert_eq!(mora_rows(&nav), 1, "both cells sit in the same block of thirty");
    }

    #[test]
    fn a_dry_run_and_a_real_write_agree() {
        let dir = std::env::temp_dir().join(format!("amdbgen-dfd-agree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dfd.s3db");
        let nav = sample();
        let planned = plan(&nav);
        let written = write(&nav, &path).expect("write");
        for a in &planned {
            let b = written.iter().find(|r| r.table == a.table).expect("the same tables");
            assert_eq!(a.rows, b.rows, "{} differs between the plan and the write", a.table);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
