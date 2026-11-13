//! Writes a [`NavSet`] in Navigraph's DFD v2 layout: `tbl_pa_airports`, `tbl_pd_sids` and
//! the rest, each table prefixed with its ARINC 424 section. Fenix 3 reads this layout and
//! nothing else; its own database is `%PROGRAMDATA%\Fenix\NavdataDFD\navdata-active.db3`, and
//! the `imported.db3` the Fenix 2 writer (`convert::fenix`) produces is no longer opened.
//!
//! Fenix 3 takes a database by hand from `NavdataDFD\Import`, named `<cycle>-r<revision>.db3`
//! (for example `2610-r001.db3`): the MCDU's IMPORT prompt copies it, checks it, adds its
//! own lookup tables and indices, and installs it. Its checks, read out of FenixSystem.exe
//! and so the ones this writer has to pass:
//!
//! * the header's cycle and revision must be the ones in the file name, its
//!   `dataset_version` must be a v2 one, and `effective_fromto` must be real dates
//!   (`ddMMddMMyy`, from and to);
//! * every table and column it reads must exist, so every table is created with exactly the
//!   columns of Fenix's own bundled database, even where this writer leaves it empty;
//! * a leg's `*_ref_table` (`EA`, `D `, `PG` ...) and identifier must resolve to a row of that
//!   table with the same identifier within a millionth of a degree of the leg's own position,
//!   or the import stops with "Unresolved non-empty DFD reference table". Every reference here
//!   is therefore written with the position copied from the row it points at, and its id
//!   filled in as well;
//! * airport identifiers must be unique (it builds a unique index on them);
//! * navaid classes are five characters, decoded position by position;
//! * SID and STAR route types must be ARINC ones.
//!
//! Unlike the Fenix 2 writer, procedures are not flattened: each part of a departure,
//! arrival or approach (runway transition, common route, enroute transition, approach
//! transition, final and missed approach) goes in under its own route type, the way the
//! layout keeps them and Fenix puts them back together.

use crate::convert::model::*;
use crate::convert::TableReport;
use anyhow::{anyhow, Context, Result};
use chrono::{Datelike, NaiveDate};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Every table of the layout, column for column as Fenix's bundled database has them.
const SCHEMA: &[&str] = &[
    r#"CREATE TABLE tbl_as_grid_mora ("mora01" VarChar(3) NOT NULL,"mora02" VarChar(3) NOT NULL,"mora03" VarChar(3) NOT NULL,"mora04" VarChar(3) NOT NULL,"mora05" VarChar(3) NOT NULL,"mora06" VarChar(3) NOT NULL,"mora07" VarChar(3) NOT NULL,"mora08" VarChar(3) NOT NULL,"mora09" VarChar(3) NOT NULL,"mora10" VarChar(3) NOT NULL,"mora11" VarChar(3) NOT NULL,"mora12" VarChar(3) NOT NULL,"mora13" VarChar(3) NOT NULL,"mora14" VarChar(3) NOT NULL,"mora15" VarChar(3) NOT NULL,"mora16" VarChar(3) NOT NULL,"mora17" VarChar(3) NOT NULL,"mora18" VarChar(3) NOT NULL,"mora19" VarChar(3) NOT NULL,"mora20" VarChar(3) NOT NULL,"mora21" VarChar(3) NOT NULL,"mora22" VarChar(3) NOT NULL,"mora23" VarChar(3) NOT NULL,"mora24" VarChar(3) NOT NULL,"mora25" VarChar(3) NOT NULL,"mora26" VarChar(3) NOT NULL,"mora27" VarChar(3) NOT NULL,"mora28" VarChar(3) NOT NULL,"mora29" VarChar(3) NOT NULL,"mora30" VarChar(3) NOT NULL,"quadrant_code" VarChar(1) DEFAULT NULL,"starting_latitude" VarChar(4) NOT NULL,"starting_longitude" VarChar(4) NOT NULL)"#,
    r#"CREATE TABLE tbl_d_vhfnavaids ("airport_identifier" VarChar(4) DEFAULT NULL,"area_code" VarChar(3) NOT NULL,"continent" VarChar(40) DEFAULT NULL,"country" VarChar(40) DEFAULT NULL,"datum_code" VarChar(3) DEFAULT NULL,"dme_elevation" Int DEFAULT NULL,"dme_ident" VarChar(4) DEFAULT NULL,"dme_latitude" Float DEFAULT NULL,"dme_longitude" Float DEFAULT NULL,"icao_code" VarChar(2) DEFAULT NULL,"ilsdme_bias" Real DEFAULT NULL,"magnetic_variation" Real DEFAULT NULL,"navaid_class" VarChar(5) NOT NULL,"navaid_frequency" Real NOT NULL,"navaid_identifier" VarChar(4) NOT NULL,"navaid_latitude" Float DEFAULT NULL,"navaid_longitude" Float DEFAULT NULL,"navaid_name" VarChar(30) NOT NULL,"range" SmallInt DEFAULT NULL,"station_declination" Real DEFAULT NULL, id TEXT)"#,
    r#"CREATE TABLE tbl_db_enroute_ndbnavaids ("area_code" VarChar(3) NOT NULL,"continent" VarChar(40) DEFAULT NULL,"country" VarChar(40) DEFAULT NULL,"datum_code" VarChar(3) DEFAULT NULL,"icao_code" VarChar(2) DEFAULT NULL,"magnetic_variation" Real DEFAULT NULL,"navaid_class" VarChar(5) NOT NULL,"navaid_frequency" Real NOT NULL,"navaid_identifier" VarChar(4) NOT NULL,"navaid_latitude" Float DEFAULT NULL,"navaid_longitude" Float DEFAULT NULL,"navaid_name" VarChar(30) NOT NULL,"range" SmallInt DEFAULT NULL, id TEXT)"#,
    r#"CREATE TABLE tbl_ea_enroute_waypoints ("area_code" VarChar(3) NOT NULL,"continent" VarChar(40) DEFAULT NULL,"country" VarChar(40) DEFAULT NULL,"datum_code" VarChar(3) DEFAULT NULL,"icao_code" VarChar(2) DEFAULT NULL,"magnetic_variation" Real DEFAULT NULL,"waypoint_identifier" VarChar(5) NOT NULL,"waypoint_latitude" Float NOT NULL,"waypoint_longitude" Float NOT NULL,"waypoint_name" VarChar(25) NOT NULL,"waypoint_type" VarChar(3) NOT NULL,"waypoint_usage" VarChar(2) DEFAULT NULL, id TEXT)"#,
    r#"CREATE TABLE tbl_ep_holdings ("area_code" VarChar(3) NOT NULL,"duplicate_identifier" Int DEFAULT NULL,"holding_name" VarChar(25) DEFAULT NULL,"holding_speed" Int DEFAULT NULL,"icao_code" VarChar(2) DEFAULT NULL,"inbound_holding_course" Real DEFAULT NULL,"leg_length" Real DEFAULT NULL,"leg_time" Real DEFAULT NULL,"maximum_altitude" Int DEFAULT NULL,"minimum_altitude" Int DEFAULT NULL,"region_code" VarChar(4) DEFAULT NULL,"turn_direction" VarChar(1) DEFAULT NULL,"waypoint_identifier" VarChar(5) DEFAULT NULL,"waypoint_latitude" Float DEFAULT NULL,"waypoint_longitude" Float DEFAULT NULL,"waypoint_ref_table" VarChar(2) DEFAULT NULL, waypoint_id TEXT)"#,
    r#"CREATE TABLE tbl_er_enroute_airways ("area_code" VarChar(3) NOT NULL,"crusing_table_identifier" VarChar(2) DEFAULT NULL,"direction_restriction" VarChar(1) DEFAULT NULL,"flightlevel" VarChar(1) DEFAULT NULL,"icao_code" VarChar(2) DEFAULT NULL,"inbound_course" Real DEFAULT NULL,"inbound_distance" Real DEFAULT NULL,"maximum_altitude" Int DEFAULT NULL,"minimum_altitude1" Int DEFAULT NULL,"minimum_altitude2" Int DEFAULT NULL,"outbound_course" Real DEFAULT NULL,"route_identifier_postfix" VarChar(1) DEFAULT NULL,"route_identifier" VarChar(6) DEFAULT NULL,"route_type" VarChar(1) DEFAULT NULL,"seqno" Int DEFAULT NULL,"waypoint_description_code" VarChar(4) DEFAULT NULL,"waypoint_identifier" VarChar(5) DEFAULT NULL,"waypoint_latitude" Float DEFAULT NULL,"waypoint_longitude" Float DEFAULT NULL,"waypoint_ref_table" VarChar(2) DEFAULT NULL, waypoint_id TEXT)"#,
    r#"CREATE TABLE tbl_eu_enroute_airway_restriction ("area_code" VarChar(3) NOT NULL,"block_indicator1" VarChar(1) NOT NULL,"block_indicator2" VarChar(1) DEFAULT NULL,"block_indicator3" VarChar(1) DEFAULT NULL,"block_indicator4" VarChar(1) DEFAULT NULL,"block_indicator5" VarChar(1) DEFAULT NULL,"block_indicator6" VarChar(1) DEFAULT NULL,"block_indicator7" VarChar(1) DEFAULT NULL,"end_date" VarChar(7) DEFAULT NULL,"end_waypoint_icao_code" VarChar(2) NOT NULL,"end_waypoint_identifier" VarChar(5) NOT NULL,"end_waypoint_latitude" Float DEFAULT NULL,"end_waypoint_longitude" Float DEFAULT NULL,"end_waypoint_ref_table" VarChar(2) DEFAULT NULL,"restriction_altitude1" Int NOT NULL,"restriction_altitude2" Int DEFAULT NULL,"restriction_altitude3" Int DEFAULT NULL,"restriction_altitude4" Int DEFAULT NULL,"restriction_altitude5" Int DEFAULT NULL,"restriction_altitude6" Int DEFAULT NULL,"restriction_altitude7" Int DEFAULT NULL,"restriction_identifier" Int NOT NULL,"restriction_type" VarChar(2) NOT NULL,"route_identifier" VarChar(6) NOT NULL,"start_date" VarChar(7) DEFAULT NULL,"start_waypoint_icao_code" VarChar(2) NOT NULL,"start_waypoint_identifier" VarChar(5) NOT NULL,"start_waypoint_latitude" Float DEFAULT NULL,"start_waypoint_longitude" Float DEFAULT NULL,"start_waypoint_ref_table" VarChar(2) DEFAULT NULL,"units_of_altitude" VarChar(1) NOT NULL)"#,
    r#"CREATE TABLE tbl_ev_enroute_communication ("area_code" VarChar(3) NOT NULL,"callsign" VarChar(30) DEFAULT NULL,"communication_frequency" Real DEFAULT NULL,"communication_type" VarChar(3) DEFAULT NULL,"fir_rdo_ident" VarChar(4) DEFAULT NULL,"fir_uir_indicator" VarChar(1) DEFAULT NULL,"frequency_units" VarChar(1) DEFAULT NULL,"latitude" Float DEFAULT NULL,"longitude" Float DEFAULT NULL,"remote_facility_icao_code" VarChar(2) DEFAULT NULL,"remote_facility" VarChar(4) DEFAULT NULL,"remote_name" VarChar(25) DEFAULT NULL,"service_indicator" VarChar(3) DEFAULT NULL)"#,
    r#"CREATE TABLE tbl_hdr_header ("creator" VarChar(16) NOT NULL,"cycle" VarChar(4) NOT NULL,"data_provider" VarChar(16) NOT NULL,"dataset_version" VarChar(16) NOT NULL,"dataset" VarChar(20) NOT NULL,"effective_fromto" VarChar(10) NOT NULL,"parsed_at" VarChar(22) NOT NULL,"revision" VarChar(3) NOT NULL)"#,
    r#"CREATE TABLE tbl_pa_airports ("airport_identifier" VarChar(4) NOT NULL,"airport_name" VarChar(30) NOT NULL,"airport_ref_latitude" Float NOT NULL,"airport_ref_longitude" Float NOT NULL,"airport_type" VarChar(1) NOT NULL,"area_code" VarChar(3) NOT NULL,"ata_iata_code" VarChar(3) DEFAULT NULL,"city" VarChar(24) DEFAULT NULL,"continent" VarChar(40) DEFAULT NULL,"country_3letter" VarChar(3) DEFAULT NULL,"country" VarChar(40) DEFAULT NULL,"elevation" Int DEFAULT NULL,"fuel" VarChar(14) DEFAULT NULL,"icao_code" VarChar(2) NOT NULL,"ifr_capability" VarChar(1) DEFAULT NULL,"longest_runway_surface_code" VarChar(1) NOT NULL,"magnetic_variation" Real DEFAULT NULL,"speed_limit_altitude" VarChar(5) DEFAULT NULL,"speed_limit" SmallInt DEFAULT NULL,"state_2letter" VarChar(2) DEFAULT NULL,"state" VarChar(50) DEFAULT NULL,"time_zone" VarChar(3) DEFAULT NULL,"transition_altitude" Int DEFAULT NULL,"transition_level" Int DEFAULT NULL, id TEXT)"#,
    r#"CREATE TABLE tbl_pb_gates ("airport_identifier" VarChar(4) NOT NULL,"area_code" VarChar(3) NOT NULL,"gate_identifier" VarChar(5) NOT NULL,"gate_latitude" Float NOT NULL,"gate_longitude" Float NOT NULL,"icao_code" VarChar(2) NOT NULL,"name" VarChar(25) DEFAULT NULL)"#,
    r#"CREATE TABLE tbl_pc_terminal_waypoints ("area_code" VarChar(3) NOT NULL,"continent" VarChar(40) DEFAULT NULL,"country" VarChar(40) DEFAULT NULL,"datum_code" VarChar(3) DEFAULT NULL,"icao_code" VarChar(2) DEFAULT NULL,"magnetic_variation" Real DEFAULT NULL,"region_code" VarChar(4) NOT NULL,"waypoint_identifier" VarChar(5) NOT NULL,"waypoint_latitude" Float NOT NULL,"waypoint_longitude" Float NOT NULL,"waypoint_name" VarChar(25) NOT NULL,"waypoint_type" VarChar(3) NOT NULL, id TEXT)"#,
    r#"CREATE TABLE tbl_pd_sids ("aircraft_category" VarChar(1) DEFAULT NULL,"airport_identifier" VarChar(4) NOT NULL,"altitude_description" VarChar(1) DEFAULT NULL,"altitude1" Int DEFAULT NULL,"altitude2" Int DEFAULT NULL,"arc_radius" Real DEFAULT NULL,"area_code" VarChar(3) DEFAULT NULL,"authorization_required" VarChar(1) DEFAULT NULL,"center_waypoint_icao_code" VarChar(2) DEFAULT NULL,"center_waypoint_latitude" Float DEFAULT NULL,"center_waypoint_longitude" Float DEFAULT NULL,"center_waypoint_ref_table" VarChar(2) DEFAULT NULL,"center_waypoint" VarChar(5) DEFAULT NULL,"course_flag" VarChar(1) DEFAULT NULL,"course" Real DEFAULT NULL,"distance_time" Real DEFAULT NULL,"path_termination" VarChar(2) NOT NULL,"procedure_identifier" VarChar(6) NOT NULL,"recommended_navaid_icao_code" VarChar(2) DEFAULT NULL,"recommended_navaid_latitude" Float DEFAULT NULL,"recommended_navaid_longitude" Float DEFAULT NULL,"recommended_navaid_ref_table" VarChar(2) DEFAULT NULL,"recommended_navaid" VarChar(4) DEFAULT NULL,"rho" Real DEFAULT NULL,"rnp" Real DEFAULT NULL,"route_distance_holding_distance_time" VarChar(1) DEFAULT NULL,"route_type" VarChar(1) NOT NULL,"seqno" Int NOT NULL,"speed_limit_description" VarChar(1) DEFAULT NULL,"speed_limit" Int DEFAULT NULL,"theta" Real DEFAULT NULL,"transition_altitude" Int DEFAULT NULL,"transition_identifier" VarChar(5) DEFAULT NULL,"turn_direction" VarChar(1) DEFAULT NULL,"vertical_angle" Real DEFAULT NULL,"waypoint_description_code" VarChar(4) DEFAULT NULL,"waypoint_icao_code" VarChar(2) DEFAULT NULL,"waypoint_identifier" VarChar(5) DEFAULT NULL,"waypoint_latitude" Float DEFAULT NULL,"waypoint_longitude" Float DEFAULT NULL,"waypoint_ref_table" VarChar(2) DEFAULT NULL, waypoint_identifier_id TEXT, recommended_navaid_id TEXT, center_waypoint_id TEXT)"#,
    r#"CREATE TABLE tbl_pe_stars ("aircraft_category" VarChar(1) DEFAULT NULL,"airport_identifier" VarChar(4) NOT NULL,"altitude_description" VarChar(1) DEFAULT NULL,"altitude1" Int DEFAULT NULL,"altitude2" Int DEFAULT NULL,"arc_radius" Real DEFAULT NULL,"area_code" VarChar(3) DEFAULT NULL,"authorization_required" VarChar(1) DEFAULT NULL,"center_waypoint_icao_code" VarChar(2) DEFAULT NULL,"center_waypoint_latitude" Float DEFAULT NULL,"center_waypoint_longitude" Float DEFAULT NULL,"center_waypoint_ref_table" VarChar(2) DEFAULT NULL,"center_waypoint" VarChar(5) DEFAULT NULL,"course_flag" VarChar(1) DEFAULT NULL,"course" Real DEFAULT NULL,"distance_time" Real DEFAULT NULL,"path_termination" VarChar(2) NOT NULL,"procedure_identifier" VarChar(6) NOT NULL,"recommended_navaid_icao_code" VarChar(2) DEFAULT NULL,"recommended_navaid_latitude" Float DEFAULT NULL,"recommended_navaid_longitude" Float DEFAULT NULL,"recommended_navaid_ref_table" VarChar(2) DEFAULT NULL,"recommended_navaid" VarChar(4) DEFAULT NULL,"rho" Real DEFAULT NULL,"rnp" Real DEFAULT NULL,"route_distance_holding_distance_time" VarChar(1) DEFAULT NULL,"route_type" VarChar(1) NOT NULL,"seqno" Int NOT NULL,"speed_limit_description" VarChar(1) DEFAULT NULL,"speed_limit" Int DEFAULT NULL,"theta" Real DEFAULT NULL,"transition_altitude" Int DEFAULT NULL,"transition_identifier" VarChar(5) DEFAULT NULL,"turn_direction" VarChar(1) DEFAULT NULL,"vertical_angle" Real DEFAULT NULL,"waypoint_description_code" VarChar(4) DEFAULT NULL,"waypoint_icao_code" VarChar(2) DEFAULT NULL,"waypoint_identifier" VarChar(5) DEFAULT NULL,"waypoint_latitude" Float DEFAULT NULL,"waypoint_longitude" Float DEFAULT NULL,"waypoint_ref_table" VarChar(2) DEFAULT NULL, waypoint_identifier_id TEXT, recommended_navaid_id TEXT, center_waypoint_id TEXT)"#,
    r#"CREATE TABLE tbl_pf_iaps ("aircraft_category" VarChar(1) DEFAULT NULL,"airport_identifier" VarChar(4) NOT NULL,"altitude_description" VarChar(1) DEFAULT NULL,"altitude1" Int DEFAULT NULL,"altitude2" Int DEFAULT NULL,"arc_radius" Real DEFAULT NULL,"area_code" VarChar(3) DEFAULT NULL,"authorization_required" VarChar(1) DEFAULT NULL,"center_waypoint_icao_code" VarChar(2) DEFAULT NULL,"center_waypoint_latitude" Float DEFAULT NULL,"center_waypoint_longitude" Float DEFAULT NULL,"center_waypoint_ref_table" VarChar(2) DEFAULT NULL,"center_waypoint" VarChar(5) DEFAULT NULL,"course_flag" VarChar(1) DEFAULT NULL,"course" Real DEFAULT NULL,"ctl" VarChar(1) DEFAULT NULL,"distance_time" Real DEFAULT NULL,"gnss_fms_indication" VarChar(1) DEFAULT NULL,"lnav_authorized_sbas" VarChar(1) DEFAULT NULL,"lnav_level_service_name" VarChar(1) DEFAULT NULL,"lnav_vnav_authorized_sbas" VarChar(1) DEFAULT NULL,"lnav_vnav_level_service_name" VarChar(1) DEFAULT NULL,"path_termination" VarChar(2) NOT NULL,"procedure_identifier" VarChar(6) NOT NULL,"recommended_navaid_icao_code" VarChar(2) DEFAULT NULL,"recommended_navaid_latitude" Float DEFAULT NULL,"recommended_navaid_longitude" Float DEFAULT NULL,"recommended_navaid_ref_table" VarChar(2) DEFAULT NULL,"recommended_navaid" VarChar(4) DEFAULT NULL,"rho" Real DEFAULT NULL,"rnp" Real DEFAULT NULL,"route_distance_holding_distance_time" VarChar(1) DEFAULT NULL,"route_type" VarChar(1) NOT NULL,"seqno" Int NOT NULL,"speed_limit_description" VarChar(1) DEFAULT NULL,"speed_limit" Int DEFAULT NULL,"theta" Real DEFAULT NULL,"transition_altitude" Int DEFAULT NULL,"transition_identifier" VarChar(5) DEFAULT NULL,"turn_direction" VarChar(1) DEFAULT NULL,"vertical_angle" Real DEFAULT NULL,"waypoint_description_code" VarChar(4) DEFAULT NULL,"waypoint_icao_code" VarChar(2) DEFAULT NULL,"waypoint_identifier" VarChar(5) DEFAULT NULL,"waypoint_latitude" Float DEFAULT NULL,"waypoint_longitude" Float DEFAULT NULL,"waypoint_ref_table" VarChar(2) DEFAULT NULL, waypoint_identifier_id TEXT, recommended_navaid_id TEXT, center_waypoint_id TEXT)"#,
    r#"CREATE TABLE tbl_pg_runways ("airport_identifier" VarChar(4) NOT NULL,"area_code" VarChar(3) DEFAULT NULL,"displaced_threshold_distance" SmallInt DEFAULT NULL,"icao_code" VarChar(2) DEFAULT NULL,"landing_threshold_elevation" Int DEFAULT NULL,"llz_identifier" VarChar(4) DEFAULT NULL,"llz_mls_gls_category" VarChar(1) DEFAULT NULL,"part_time_lights" VarChar(1) DEFAULT NULL,"runway_gradient" Real DEFAULT NULL,"runway_identifier" VarChar(3) NOT NULL,"runway_latitude" Float DEFAULT NULL,"runway_length" Real NOT NULL,"runway_lights" VarChar(1) DEFAULT NULL,"runway_longitude" Float DEFAULT NULL,"runway_magnetic_bearing" Real DEFAULT NULL,"runway_true_bearing" Real DEFAULT NULL,"runway_width" SmallInt NOT NULL,"surface_code" VarChar(3) DEFAULT NULL,"threshold_crossing_height" Int DEFAULT NULL,"traffic_pattern_altitude" Int DEFAULT NULL,"traffic_pattern" VarChar(1) DEFAULT NULL, id TEXT)"#,
    r#"CREATE TABLE tbl_pi_localizers_glideslopes ("airport_identifier" VarChar(4) NOT NULL,"area_code" VarChar(3) DEFAULT NULL,"gs_angle" Real DEFAULT NULL,"gs_beam_width" Real DEFAULT NULL,"gs_elevation" Int DEFAULT NULL,"gs_latitude" Float DEFAULT NULL,"gs_longitude" Float DEFAULT NULL,"gs_position" Int DEFAULT NULL,"icao_code" VarChar(2) DEFAULT NULL,"ils_mls_gls_category" VarChar(1) DEFAULT NULL,"llz_bearing" Real DEFAULT NULL,"llz_frequency" Real DEFAULT NULL,"llz_identifier" VarChar(4) NOT NULL,"llz_latitude" Float DEFAULT NULL,"llz_longitude" Float DEFAULT NULL,"llz_position_reference" VarChar(1) DEFAULT NULL,"llz_position" Int DEFAULT NULL,"llz_truebearing" Real DEFAULT NULL,"llz_width" Real DEFAULT NULL,"runway_identifier" VarChar(3) DEFAULT NULL,"station_declination" Real DEFAULT NULL, id TEXT)"#,
    r#"CREATE TABLE tbl_pm_localizer_marker ("airport_identifier" VarChar(4) NOT NULL,"area_code" VarChar(3) NOT NULL,"icao_code" VarChar(2) NOT NULL,"llz_identifier" VarChar(4) NOT NULL,"marker_identifier" VarChar(5) DEFAULT NULL,"marker_latitude" Float NOT NULL,"marker_longitude" Float NOT NULL,"marker_type" VarChar(3) DEFAULT NULL,"runway_identifier" VarChar(5) NOT NULL, id TEXT)"#,
    r#"CREATE TABLE tbl_pn_terminal_ndbnavaids ("airport_identifier" VarChar(4) NOT NULL,"area_code" VarChar(3) NOT NULL,"continent" VarChar(40) DEFAULT NULL,"country" VarChar(40) DEFAULT NULL,"datum_code" VarChar(3) DEFAULT NULL,"icao_code" VarChar(2) DEFAULT NULL,"magnetic_variation" Real DEFAULT NULL,"navaid_class" VarChar(5) NOT NULL,"navaid_frequency" Real NOT NULL,"navaid_identifier" VarChar(4) NOT NULL,"navaid_latitude" Float DEFAULT NULL,"navaid_longitude" Float DEFAULT NULL,"navaid_name" VarChar(30) NOT NULL,"range" SmallInt DEFAULT NULL, id TEXT)"#,
    r#"CREATE TABLE tbl_pp_pathpoint ("airport_icao_code" VarChar(2) NOT NULL,"airport_identifier" VarChar(4) NOT NULL,"approach_performance_designator" VarChar(1) DEFAULT NULL,"approach_procedure_ident" VarChar(6) NOT NULL,"approach_type_identifier" VarChar(10) NOT NULL,"area_code" VarChar(3) NOT NULL,"course_width_at_threshold" Real NOT NULL,"flight_path_alignment_point_latitude" Float NOT NULL,"flight_path_alignment_point_longitude" Float NOT NULL,"fpap_ellipsoid_height" Real DEFAULT NULL,"fpap_orthometric_height" Real DEFAULT NULL,"glide_path_angle" Real NOT NULL,"gnss_channel_number" Int NOT NULL,"hal" Real NOT NULL,"landing_threshold_point_latitude" Float NOT NULL,"landing_threshold_point_longitude" Float NOT NULL,"length_offset" Int DEFAULT NULL,"ltp_ellipsoid_height" Real NOT NULL,"ltp_orthometric_height" Real DEFAULT NULL,"operation_type" TinyInt DEFAULT NULL,"path_point_tch" Real NOT NULL,"reference_path_data_selector" TinyInt DEFAULT NULL,"reference_path_identifier" VarChar(4) NOT NULL,"route_indicator" VarChar(1) DEFAULT NULL,"runway_identifier" VarChar(4) NOT NULL,"sbas_service_provider_identifier" TinyInt DEFAULT NULL,"tch_units_indicator" VarChar(1) NOT NULL,"val" Real NOT NULL)"#,
    r#"CREATE TABLE tbl_ps_airport_msa ("airport_identifier" VarChar(4) NOT NULL,"area_code" VarChar(3) NOT NULL,"icao_code" VarChar(2) NOT NULL,"magnetic_true_indicator" VarChar(1) DEFAULT NULL,"msa_center_icao_code" VarChar(2) NOT NULL,"msa_center_latitude" Float DEFAULT NULL,"msa_center_longitude" Float DEFAULT NULL,"msa_center_ref_table" VarChar(2) NOT NULL,"msa_center" VarChar(5) NOT NULL,"multiple_code" VarChar(1) DEFAULT NULL,"radius_limit" TinyInt DEFAULT NULL,"sector_altitude_1" TinyInt DEFAULT NULL,"sector_altitude_2" TinyInt DEFAULT NULL,"sector_altitude_3" TinyInt DEFAULT NULL,"sector_altitude_4" TinyInt DEFAULT NULL,"sector_altitude_5" TinyInt DEFAULT NULL,"sector_bearing_1" Real DEFAULT NULL,"sector_bearing_2" Real DEFAULT NULL,"sector_bearing_3" Real DEFAULT NULL,"sector_bearing_4" Real DEFAULT NULL,"sector_bearing_5" Real DEFAULT NULL)"#,
    r#"CREATE TABLE tbl_pt_gls ("airport_identifier" VarChar(4) NOT NULL,"area_code" VarChar(3) NOT NULL,"gls_approach_bearing" Real NOT NULL,"gls_approach_slope" Real NOT NULL,"gls_category" VarChar(1) NOT NULL,"gls_channel" Int NOT NULL,"gls_ref_path_identifier" VarChar(4) NOT NULL,"gls_station_ident" VarChar(4) NOT NULL,"icao_code" VarChar(2) NOT NULL,"magnetic_variation" Real NOT NULL,"runway_identifier" VarChar(5) NOT NULL,"station_elevation" Int NOT NULL,"station_latitude" Float NOT NULL,"station_longitude" Float NOT NULL,"station_type" VarChar(3) DEFAULT NULL, id TEXT)"#,
    r#"CREATE TABLE tbl_pv_airport_communication ("airport_identifier" VarChar(4) NOT NULL,"area_code" VarChar(3) NOT NULL,"callsign" VarChar(25) DEFAULT NULL,"communication_frequency" Real NOT NULL,"communication_type" VarChar(3) NOT NULL,"frequency_units" VarChar(1) NOT NULL,"guard_transmit" VarChar(1) DEFAULT NULL,"icao_code" VarChar(2) NOT NULL,"latitude" Float NOT NULL,"longitude" Float NOT NULL,"narrative" VarChar(60) DEFAULT NULL,"remote_facility_icao_code" VarChar(2) DEFAULT NULL,"remote_facility" VarChar(4) DEFAULT NULL,"sector_facility_icao_code" VarChar(2) DEFAULT NULL,"sector_facility" VarChar(4) DEFAULT NULL,"sectorization" VarChar(6) DEFAULT NULL,"service_indicator" VarChar(3) DEFAULT NULL,"time_of_operation_1" VarChar(10) DEFAULT NULL,"time_of_operation_2" VarChar(10) DEFAULT NULL,"time_of_operation_3" VarChar(10) DEFAULT NULL,"time_of_operation_4" VarChar(10) DEFAULT NULL,"time_of_operation_5" VarChar(10) DEFAULT NULL,"time_of_operation_6" VarChar(10) DEFAULT NULL,"time_of_operation_7" VarChar(10) DEFAULT NULL)"#,
    r#"CREATE TABLE tbl_tc_cruising_tables ("area_code" VarChar(3) DEFAULT NULL,"course_from" Real NOT NULL,"course_to" Real NOT NULL,"cruise_level_from1" Int NOT NULL,"cruise_level_from2" Int DEFAULT NULL,"cruise_level_from3" Int DEFAULT NULL,"cruise_level_from4" Int DEFAULT NULL,"cruise_level_to1" Int NOT NULL,"cruise_level_to2" Int DEFAULT NULL,"cruise_level_to3" Int DEFAULT NULL,"cruise_level_to4" Int DEFAULT NULL,"cruise_table_identifier" VarChar(2) NOT NULL,"mag_true" VarChar(1) NOT NULL,"seqno" Int NOT NULL,"vertical_separation1" Int NOT NULL,"vertical_separation2" Int DEFAULT NULL,"vertical_separation3" Int DEFAULT NULL,"vertical_separation4" Int DEFAULT NULL)"#,
    r#"CREATE TABLE tbl_uc_controlled_airspace ("airspace_center" VarChar(5) NOT NULL,"airspace_classification" VarChar(1) DEFAULT NULL,"airspace_type" VarChar(1) NOT NULL,"arc_bearing" Real DEFAULT NULL,"arc_distance" Real DEFAULT NULL,"arc_origin_latitude" Float DEFAULT NULL,"arc_origin_longitude" Float DEFAULT NULL,"area_code" VarChar(3) NOT NULL,"boundary_via" VarChar(2) NOT NULL,"controlled_airspace_name" VarChar(30) DEFAULT NULL,"flightlevel" VarChar(1) DEFAULT NULL,"icao_code" VarChar(2) NOT NULL,"latitude" Float DEFAULT NULL,"longitude" Float DEFAULT NULL,"lower_limit" VarChar(5) DEFAULT NULL,"multiple_code" VarChar(1) DEFAULT NULL,"seqno" Int NOT NULL,"time_code" VarChar(1) DEFAULT NULL,"unit_indicator_lower_limit" VarChar(1) DEFAULT NULL,"unit_indicator_upper_limit" VarChar(1) DEFAULT NULL,"upper_limit" VarChar(5) DEFAULT NULL)"#,
    r#"CREATE TABLE tbl_uf_fir_uir ("adjacent_fir_identifier" VarChar(4) DEFAULT NULL,"adjacent_uir_identifier" VarChar(4) DEFAULT NULL,"arc_bearing" Real DEFAULT NULL,"arc_distance" Real DEFAULT NULL,"arc_origin_latitude" Float DEFAULT NULL,"arc_origin_longitude" Float DEFAULT NULL,"area_code" VarChar(3) NOT NULL,"boundary_via" VarChar(2) NOT NULL,"cruise_table_identifier" VarChar(2) DEFAULT NULL,"fir_uir_address" VarChar(4) NOT NULL,"fir_uir_identifier" VarChar(4) NOT NULL,"fir_uir_indicator" VarChar(1) NOT NULL,"fir_uir_latitude" Float NOT NULL,"fir_uir_longitude" Float NOT NULL,"fir_uir_name" VarChar(25) DEFAULT NULL,"fir_upper_limit" VarChar(5) DEFAULT NULL,"reporting_units_altitude" Int DEFAULT NULL,"reporting_units_speed" Int DEFAULT NULL,"seqno" Int NOT NULL,"uir_lower_limit" VarChar(5) DEFAULT NULL,"uir_upper_limit" VarChar(5) DEFAULT NULL)"#,
    r#"CREATE TABLE tbl_ur_restrictive_airspace ("arc_bearing" Real DEFAULT NULL,"arc_distance" Real DEFAULT NULL,"arc_origin_latitude" Float DEFAULT NULL,"arc_origin_longitude" Float DEFAULT NULL,"area_code" VarChar(3) DEFAULT NULL,"boundary_via" VarChar(2) DEFAULT NULL,"flightlevel" VarChar(1) DEFAULT NULL,"icao_code" VarChar(2) DEFAULT NULL,"latitude" Float DEFAULT NULL,"longitude" Float DEFAULT NULL,"lower_limit" VarChar(5) DEFAULT NULL,"multiple_code" VarChar(1) DEFAULT NULL,"restrictive_airspace_designation" VarChar(10) DEFAULT NULL,"restrictive_airspace_name" VarChar(30) DEFAULT NULL,"restrictive_type" VarChar(1) DEFAULT NULL,"seqno" Int DEFAULT NULL,"unit_indicator_lower_limit" VarChar(1) DEFAULT NULL,"unit_indicator_upper_limit" VarChar(1) DEFAULT NULL,"upper_limit" VarChar(5) DEFAULT NULL)"#,
];

/// What this writer leaves empty, and why. (The terminal fix and NDB tables are empty too,
/// but not missing anything: the simulator does not say which fixes are terminal ones, so
/// every fix is in `tbl_ea_enroute_waypoints` and every NDB in `tbl_db_enroute_ndbnavaids`.
/// They are left out of this list so `--keep` does not add a second copy of each.) `--keep` fills most of them from Fenix's own
/// database; the two whose rows point at a fix by position are never taken across (see
/// [`crate::convert::carry_over`]).
const OMITTED: &[(&str, &str)] = &[
    ("tbl_as_grid_mora", "grid MORA is not in the simulator's navigation data"),
    ("tbl_ep_holdings", "published holds are not in the simulator's navigation data; holds inside a procedure are written as its HA/HF/HM legs"),
    ("tbl_ps_airport_msa", "minimum safe altitude sectors are not in the simulator's navigation data"),
    ("tbl_eu_enroute_airway_restriction", "airway restrictions are not in the simulator's navigation data"),
    ("tbl_pv_airport_communication", "aerodrome frequencies are not carried by this converter"),
    ("tbl_ev_enroute_communication", "enroute frequencies are not carried by this converter"),
    ("tbl_uc_controlled_airspace", "airspace boundaries are not carried by this converter"),
    ("tbl_ur_restrictive_airspace", "airspace boundaries are not carried by this converter"),
    ("tbl_uf_fir_uir", "flight information regions are not carried by this converter"),
    ("tbl_pm_localizer_marker", "marker beacons are not carried by this converter"),
    ("tbl_pt_gls", "GLS approaches are not carried by this converter"),
    ("tbl_pp_pathpoint", "SBAS path points are not carried by this converter"),
    ("tbl_pb_gates", "stands are not carried by this converter"),
    ("tbl_tc_cruising_tables", "cruising level tables are not carried by this converter"),
];

const FILLED: [&str; 11] = [
    "tbl_hdr_header",
    "tbl_pa_airports",
    "tbl_pg_runways",
    "tbl_d_vhfnavaids",
    "tbl_db_enroute_ndbnavaids",
    "tbl_ea_enroute_waypoints",
    "tbl_pi_localizers_glideslopes",
    "tbl_er_enroute_airways",
    "tbl_pd_sids",
    "tbl_pe_stars",
    "tbl_pf_iaps",
];

/// The revision every database this writes carries: the file name must say the same.
pub const REVISION: &str = "001";

/// `%PROGRAMDATA%\Fenix\NavdataDFD`, where Fenix 3 keeps its navigation data. None when no
/// Fenix 3 has been run on this machine.
pub fn fenix_root() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("PROGRAMDATA")?).join("Fenix").join("NavdataDFD");
    p.is_dir().then_some(p)
}

/// The file a database for this cycle is written to, for Fenix 3 to import.
pub fn import_path(root: &Path, cycle: &str) -> PathBuf {
    root.join("Import").join(format!("{cycle}-r{REVISION}.db3"))
}

/// Fenix 3's database in use, the one `--keep` takes what the simulator lacks from.
pub fn active_database(root: &Path) -> Option<PathBuf> {
    let p = root.join("navdata-active.db3");
    p.is_file().then_some(p)
}

/// The first and last day of an AIRAC cycle given as `YYNN`.
pub fn cycle_dates(cycle: &str) -> Option<(NaiveDate, NaiveDate)> {
    if cycle.len() != 4 || !cycle.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year = 2000 + cycle[..2].parse::<i32>().ok()?;
    let number = cycle[2..].parse::<i64>().ok()?;
    if number < 1 {
        return None;
    }
    let epoch = NaiveDate::from_ymd_opt(2020, 1, 2)?;
    let new_year = NaiveDate::from_ymd_opt(year, 1, 1)?;
    let days = (new_year - epoch).num_days();
    let first = epoch + chrono::Duration::days(days.div_euclid(28) * 28 + if days.rem_euclid(28) == 0 { 0 } else { 28 });
    let start = first + chrono::Duration::days((number - 1) * 28);
    (start.year() == year).then(|| (start, start + chrono::Duration::days(27)))
}

/// The cycle in effect on a day.
pub fn cycle_on(day: NaiveDate) -> String {
    let mut year = day.year();
    loop {
        for n in (1..=14).rev() {
            let c = format!("{:02}{:02}", year % 100, n);
            if let Some((from, _)) = cycle_dates(&c) {
                if from <= day {
                    return c;
                }
            }
        }
        year -= 1;
    }
}

/// `effective_fromto` as Navigraph writes it: day and month of the first day, day and month
/// of the last, then the two-digit year (`2003160425` for 20 March to 16 April 2025).
fn effective_fromto(cycle: &str) -> Option<String> {
    let (from, to) = cycle_dates(cycle)?;
    Some(format!("{}{}{}", from.format("%d%m"), to.format("%d%m"), from.format("%y")))
}

/// A fix, beacon, runway or airport a reference can point at: which table it is in (the
/// code a `*_ref_table` column carries), its id there, and where it is, exactly as written.
#[derive(Clone)]
struct Place {
    table: &'static str,
    id: String,
    region: String,
    airport: Option<String>,
    lat: f64,
    lon: f64,
}

/// Everything a reference can point at, by identifier.
#[derive(Default)]
struct Places {
    by_ident: HashMap<String, Vec<Place>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Want {
    /// A leg's fix or an arc's centre: anything with a position.
    Fix,
    /// A recommended navaid: a beacon or a localiser.
    Navaid,
}

impl Places {
    fn add(&mut self, ident: &str, place: Place) {
        self.by_ident.entry(ident.trim().to_string()).or_default().push(place);
    }

    /// The place of this name the reference most plausibly means: at this airport for a
    /// runway or a localiser, in this region where one is given, and nearest to where the
    /// source puts it (or, failing that, to the airport).
    fn find(&self, ident: &str, want: Want, airport: Option<&str>, region: Option<&str>, near: Option<(f64, f64)>, reach_nm: f64) -> Option<&Place> {
        let all = self.by_ident.get(ident.trim())?;
        let fits = |p: &&Place| {
            let kind = match want {
                Want::Fix => p.table != "PI",
                Want::Navaid => matches!(p.table, "D " | "DB" | "PN" | "PI"),
            };
            let here = p.airport.as_deref().map_or(true, |a| Some(a) == airport);
            kind && here
        };
        let mut found: Vec<&Place> = all.iter().filter(fits).collect();
        if let Some(r) = region.filter(|r| !r.is_empty()) {
            if found.iter().any(|p| p.region == r) {
                found.retain(|p| p.region == r);
            }
        }
        match near {
            Some(at) => found
                .into_iter()
                .map(|p| (crate::dispatch::distance_nm(at, (p.lat, p.lon)), p))
                .filter(|(d, _)| *d <= reach_nm)
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, p)| p),
            None => found.into_iter().next(),
        }
    }
}

/// Write a whole `NavSet` as a DFD v2 database at `path`, replacing anything there.
pub fn write(nav: &NavSet, path: &Path) -> Result<Vec<TableReport>> {
    let fromto = effective_fromto(&nav.cycle).ok_or_else(|| anyhow!("{} is not an AIRAC cycle (YYNN): Fenix 3 needs the cycle's real dates", nav.cycle))?;
    if path.exists() {
        std::fs::remove_file(path).with_context(|| format!("remove existing {}", path.display()))?;
    }
    let mut conn = Connection::open(path).with_context(|| format!("create {}", path.display()))?;
    conn.execute_batch("PRAGMA journal_mode = DELETE;")?;
    for sql in SCHEMA {
        conn.execute_batch(sql).with_context(|| format!("create a table: {}", &sql[..sql.len().min(60)]))?;
    }
    let tx = conn.transaction()?;
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    tx.execute(
        "INSERT INTO tbl_hdr_header (creator, cycle, data_provider, dataset_version, dataset, effective_fromto, parsed_at, revision) VALUES ('AMDB Navdata', ?1, 'MSFS', '2.0.0.0', 'AMDB_SIM', ?2, ?3, ?4)",
        params![nav.cycle, fromto, chrono::Utc::now().format("%Y-%m-%d %H:%M:%SZ").to_string(), REVISION],
    )?;
    counts.insert("tbl_hdr_header", 1);

    let variation = Variation::new(nav);
    let mut places = Places::default();
    let airports = write_airports(&tx, nav, &variation, &mut places)?;
    counts.insert("tbl_pa_airports", airports.len());
    counts.insert("tbl_pg_runways", write_runways(&tx, nav, &variation, &airports, &mut places)?);
    let (vhf, ndb) = write_navaids(&tx, nav, &mut places)?;
    counts.insert("tbl_d_vhfnavaids", vhf);
    counts.insert("tbl_db_enroute_ndbnavaids", ndb);
    counts.insert("tbl_ea_enroute_waypoints", write_waypoints(&tx, nav, &variation, &mut places)?);
    counts.insert("tbl_pi_localizers_glideslopes", write_ils(&tx, nav, &variation, &airports, &mut places)?);
    counts.insert("tbl_er_enroute_airways", write_airways(&tx, nav, &places)?);
    let (sids, stars, iaps) = write_procedures(&tx, nav, &airports, &places)?;
    counts.insert("tbl_pd_sids", sids);
    counts.insert("tbl_pe_stars", stars);
    counts.insert("tbl_pf_iaps", iaps);
    tx.commit()?;
    Ok(report_with(&counts))
}

/// What [`write`] would produce, as near as can be said without resolving every reference.
pub fn plan(nav: &NavSet) -> Vec<TableReport> {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    counts.insert("tbl_hdr_header", 1);
    let mut icaos: Vec<&str> = nav.airports.iter().map(|a| a.icao.as_str()).collect();
    icaos.sort_unstable();
    icaos.dedup();
    counts.insert("tbl_pa_airports", icaos.len());
    counts.insert("tbl_pg_runways", nav.runways.len());
    counts.insert("tbl_d_vhfnavaids", nav.navaids.iter().filter(|n| !is_ndb(n.kind)).count());
    counts.insert("tbl_db_enroute_ndbnavaids", nav.navaids.iter().filter(|n| is_ndb(n.kind)).count());
    counts.insert("tbl_ea_enroute_waypoints", nav.waypoints.len());
    counts.insert("tbl_pi_localizers_glideslopes", nav.ils.len());
    counts.insert("tbl_er_enroute_airways", nav.airways.iter().map(|a| a.legs.len() + a.legs.iter().filter(|l| l.is_start).count().max(1)).sum());
    for (table, kind) in [("tbl_pd_sids", ProcKind::Sid), ("tbl_pe_stars", ProcKind::Star), ("tbl_pf_iaps", ProcKind::Approach)] {
        counts.insert(table, nav.procedures.iter().filter(|p| p.kind == kind).map(|p| p.legs.len()).sum());
    }
    report_with(&counts)
}

fn report_with(counts: &HashMap<&'static str, usize>) -> Vec<TableReport> {
    let mut out: Vec<TableReport> = FILLED.iter().map(|t| TableReport::filled(t, counts.get(t).copied().unwrap_or(0))).collect();
    out.extend(OMITTED.iter().map(|(t, why)| TableReport::empty(t, why)));
    out
}

fn is_ndb(k: NavaidKind) -> bool {
    matches!(k, NavaidKind::Ndb | NavaidKind::NdbDme)
}

/// Magnetic variation near a point, from the nearest beacon the data has: the simulator
/// gives every VOR its variation and nothing else one, and runways and localisers need a
/// magnetic bearing beside their true one.
struct Variation {
    cells: HashMap<(i32, i32), Vec<(f64, f64, f64)>>,
}

impl Variation {
    fn new(nav: &NavSet) -> Self {
        let mut cells: HashMap<(i32, i32), Vec<(f64, f64, f64)>> = HashMap::new();
        // The simulator's beacons all say 0, which means it did not say.
        if nav.navaids.iter().any(|n| n.magnetic_variation_deg != 0.0) {
            for n in nav.navaids.iter().filter(|n| !is_ndb(n.kind)) {
                cells.entry(Self::cell(n.lat, n.lon)).or_default().push((n.lat, n.lon, n.magnetic_variation_deg));
            }
        }
        // A localiser's magnetic course against its runway's true heading is the variation
        // there, wherever the beacons do not say.
        let headings: HashMap<(&str, &str), f64> = nav.runways.iter().map(|r| ((r.airport_icao.as_str(), r.ident.as_str()), r.heading_true_deg)).collect();
        for i in &nav.ils {
            let Some(&heading) = headings.get(&(i.airport_icao.as_str(), i.runway_ident.as_str())) else { continue };
            let var = (heading - i.course_mag_deg + 540.0).rem_euclid(360.0) - 180.0;
            if var.abs() <= 60.0 {
                cells.entry(Self::cell(i.lat, i.lon)).or_default().push((i.lat, i.lon, (var * 10.0).round() / 10.0));
            }
        }
        Variation { cells }
    }

    fn cell(lat: f64, lon: f64) -> (i32, i32) {
        ((lat / 2.0).floor() as i32, (lon / 2.0).floor() as i32)
    }

    /// Degrees, east positive; None where no beacon is within a few hundred miles.
    fn at(&self, lat: f64, lon: f64) -> Option<f64> {
        let (cy, cx) = Self::cell(lat, lon);
        let mut best: Option<(f64, f64)> = None;
        for dy in -2..=2 {
            for dx in -2..=2 {
                for &(blat, blon, var) in self.cells.get(&(cy + dy, cx + dx)).into_iter().flatten() {
                    let d = crate::dispatch::distance_nm((lat, lon), (blat, blon));
                    if best.map_or(true, |(bd, _)| d < bd) {
                        best = Some((d, var));
                    }
                }
            }
        }
        best.map(|(_, v)| v)
    }
}

fn magnetic(true_deg: f64, variation: Option<f64>) -> f64 {
    (true_deg - variation.unwrap_or(0.0)).rem_euclid(360.0)
}

/// A region code the layout can hold: the airport's own region, or the first two letters
/// of its identifier where the source has none.
fn region_of<'a>(code: &'a str, icao: &'a str) -> &'a str {
    if code.trim().is_empty() {
        icao.get(..2).unwrap_or(icao)
    } else {
        code
    }
}

/// Every airport once, by identifier: its id, region, area, position and transition altitude.
struct AirportRow {
    region: String,
    area: String,
    lat: f64,
    lon: f64,
    transition_altitude: Option<i64>,
}

fn write_airports(conn: &Connection, nav: &NavSet, variation: &Variation, places: &mut Places) -> Result<HashMap<String, AirportRow>> {
    let mut stmt = conn.prepare(
        "INSERT INTO tbl_pa_airports (airport_identifier, airport_name, airport_ref_latitude, airport_ref_longitude, airport_type, area_code, elevation, icao_code, \
         ifr_capability, longest_runway_surface_code, magnetic_variation, speed_limit_altitude, speed_limit, transition_altitude, transition_level, id) \
         VALUES (?1, ?2, ?3, ?4, 'C', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;
    let mut longest: HashMap<&str, (f64, Surface)> = HashMap::new();
    for r in &nav.runways {
        let e = longest.entry(r.airport_icao.as_str()).or_insert((0.0, Surface::Unknown));
        if r.length_ft > e.0 {
            *e = (r.length_ft, r.surface);
        }
    }
    let with_procedures: std::collections::HashSet<&str> = nav.procedures.iter().map(|p| p.airport_icao.as_str()).collect();
    let mut out: HashMap<String, AirportRow> = HashMap::new();
    let mut row = 0usize;
    for a in &nav.airports {
        if a.icao.trim().is_empty() || out.contains_key(&a.icao) {
            continue;
        }
        row += 1;
        let id = format!("PA:{row}");
        let region = region_of(&a.icao_code, &a.icao).to_string();
        let surface = longest.get(a.icao.as_str()).map(|(_, s)| surface_letter(*s)).unwrap_or("U");
        let ta = a.transition_altitude_ft.map(|v| v.round() as i64);
        stmt.execute(params![
            a.icao,
            truncate(if a.name.trim().is_empty() { &a.icao } else { &a.name }, 30),
            a.lat,
            a.lon,
            a.area_code,
            a.elevation_ft.round() as i64,
            region,
            if with_procedures.contains(a.icao.as_str()) { "Y" } else { "N" },
            surface,
            variation.at(a.lat, a.lon),
            a.speed_limit_altitude_ft.map(|v| (v.round() as i64).to_string()),
            a.speed_limit_kt.map(|v| v.round() as i64),
            ta,
            a.transition_level_ft.map(|v| v.round() as i64),
            id,
        ])?;
        places.add(&a.icao, Place { table: "PA", id, region: region.clone(), airport: None, lat: a.lat, lon: a.lon });
        out.insert(a.icao.clone(), AirportRow { region, area: a.area_code.clone(), lat: a.lat, lon: a.lon, transition_altitude: ta });
    }
    Ok(out)
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn surface_letter(s: Surface) -> &'static str {
    match s {
        Surface::Asphalt | Surface::Concrete => "H",
        Surface::Grass | Surface::Gravel | Surface::Snow | Surface::Ice => "S",
        Surface::Water => "W",
        Surface::Unknown => "U",
    }
}

/// The four letters the layout's `surface_code` uses.
fn surface_code(s: Surface) -> &'static str {
    match s {
        Surface::Asphalt => "ASPH",
        Surface::Concrete => "CONC",
        Surface::Grass => "GRAS",
        Surface::Gravel => "GRVL",
        Surface::Water => "WATE",
        Surface::Snow => "SNOW",
        Surface::Ice => "ICE",
        Surface::Unknown => "UNKN",
    }
}

fn ils_category(c: IlsCategory) -> &'static str {
    match c {
        IlsCategory::Loc => "0",
        IlsCategory::Cat1 => "1",
        IlsCategory::Cat2 => "2",
        IlsCategory::Cat3 => "3",
    }
}

fn write_runways(conn: &Connection, nav: &NavSet, variation: &Variation, airports: &HashMap<String, AirportRow>, places: &mut Places) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT INTO tbl_pg_runways (airport_identifier, area_code, displaced_threshold_distance, icao_code, landing_threshold_elevation, llz_identifier, \
         llz_mls_gls_category, runway_gradient, runway_identifier, runway_latitude, runway_length, runway_longitude, runway_magnetic_bearing, runway_true_bearing, \
         runway_width, surface_code, threshold_crossing_height, id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
    )?;
    let mut llz: HashMap<(&str, &str), &IlsRec> = HashMap::new();
    for i in &nav.ils {
        llz.entry((i.airport_icao.as_str(), i.runway_ident.as_str())).or_insert(i);
    }
    let mut row = 0usize;
    for r in &nav.runways {
        let Some(apt) = airports.get(&r.airport_icao) else { continue };
        row += 1;
        let id = format!("PG:{row}");
        let ident = format!("RW{}", r.ident);
        let ils = llz.get(&(r.airport_icao.as_str(), r.ident.as_str()));
        stmt.execute(params![
            r.airport_icao,
            apt.area,
            r.displaced_threshold_ft.map(|v| v.round() as i64).unwrap_or(0),
            apt.region,
            r.elevation_ft.round() as i64,
            ils.map(|i| i.ident.as_str()),
            ils.map(|i| ils_category(i.category)),
            ident,
            r.lat,
            r.length_ft.round(),
            r.lon,
            magnetic(r.heading_true_deg, variation.at(r.lat, r.lon)),
            r.heading_true_deg,
            r.width_ft.round() as i64,
            surface_code(r.surface),
            ils.and_then(|i| i.crossing_height_ft).map(|v| v.round() as i64),
            id,
        ])?;
        places.add(&ident, Place { table: "PG", id, region: apt.region.clone(), airport: Some(r.airport_icao.clone()), lat: r.lat, lon: r.lon });
    }
    Ok(row)
}

/// The five-character class the layout gives a beacon, as Fenix decodes it: what it is in the
/// first two places, its coverage in the third.
fn navaid_class(n: &NavaidRec) -> String {
    let coverage = match n.range_nm {
        Some(r) if r >= 100.0 => 'H',
        Some(r) if r >= 40.0 => 'L',
        Some(_) => 'T',
        None => 'U',
    };
    let kind = match n.kind {
        NavaidKind::Vor => "V ",
        NavaidKind::VorDme => "VD",
        NavaidKind::Vortac => "VT",
        NavaidKind::Tacan => " T",
        NavaidKind::Dme => " D",
        NavaidKind::IlsDme => return " IT  ".to_string(),
        NavaidKind::Ndb | NavaidKind::NdbDme => return "H  W ".to_string(),
    };
    format!("{kind}{coverage}  ")
}

fn write_navaids(conn: &Connection, nav: &NavSet, places: &mut Places) -> Result<(usize, usize)> {
    let mut vhf = conn.prepare(
        "INSERT INTO tbl_d_vhfnavaids (area_code, datum_code, dme_elevation, dme_ident, dme_latitude, dme_longitude, icao_code, magnetic_variation, navaid_class, \
         navaid_frequency, navaid_identifier, navaid_latitude, navaid_longitude, navaid_name, range, station_declination, id) \
         VALUES (?1, 'WGE', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?7, ?15)",
    )?;
    let mut ndb = conn.prepare(
        "INSERT INTO tbl_db_enroute_ndbnavaids (area_code, datum_code, icao_code, magnetic_variation, navaid_class, navaid_frequency, navaid_identifier, \
         navaid_latitude, navaid_longitude, navaid_name, range, id) VALUES (?1, 'WGE', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    let (mut n_vhf, mut n_ndb) = (0usize, 0usize);
    // FS2020 keeps the same beacon in more than one package; one row each.
    let mut seen = std::collections::HashSet::new();
    for n in &nav.navaids {
        if n.ident.trim().is_empty() || !seen.insert((n.ident.clone(), is_ndb(n.kind), (n.lat * 1e4).round() as i64, (n.lon * 1e4).round() as i64)) {
            continue;
        }
        let name = truncate(if n.name.trim().is_empty() { &n.ident } else { &n.name }, 30);
        if is_ndb(n.kind) {
            n_ndb += 1;
            let id = format!("DB:{n_ndb}");
            ndb.execute(params![n.area_code, n.region_code, n.magnetic_variation_deg, navaid_class(n), n.frequency, n.ident, n.lat, n.lon, name, n.range_nm.map(|v| v.round() as i64), id])?;
            places.add(&n.ident, Place { table: "DB", id, region: n.region_code.clone(), airport: None, lat: n.lat, lon: n.lon });
        } else {
            n_vhf += 1;
            let id = format!("D:{n_vhf}");
            let has_dme = matches!(n.kind, NavaidKind::VorDme | NavaidKind::Vortac | NavaidKind::Dme | NavaidKind::IlsDme | NavaidKind::Tacan);
            vhf.execute(params![
                n.area_code,
                has_dme.then_some(n.elevation_ft.round() as i64),
                has_dme.then_some(n.ident.as_str()),
                has_dme.then_some(n.lat),
                has_dme.then_some(n.lon),
                n.region_code,
                n.magnetic_variation_deg,
                navaid_class(n),
                n.frequency,
                n.ident,
                n.lat,
                n.lon,
                name,
                n.range_nm.map(|v| v.round() as i64),
                id,
            ])?;
            places.add(&n.ident, Place { table: "D ", id, region: n.region_code.clone(), airport: None, lat: n.lat, lon: n.lon });
        }
    }
    Ok((n_vhf, n_ndb))
}

fn write_waypoints(conn: &Connection, nav: &NavSet, variation: &Variation, places: &mut Places) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT INTO tbl_ea_enroute_waypoints (area_code, datum_code, icao_code, magnetic_variation, waypoint_identifier, waypoint_latitude, waypoint_longitude, \
         waypoint_name, waypoint_type, waypoint_usage, id) VALUES (?1, 'WGE', ?2, ?3, ?4, ?5, ?6, ?7, 'W  ', 'RB', ?8)",
    )?;
    let mut row = 0usize;
    for w in &nav.waypoints {
        if w.ident.trim().is_empty() {
            continue;
        }
        row += 1;
        let id = format!("EA:{row}");
        stmt.execute(params![w.area_code, w.region_code, variation.at(w.lat, w.lon), w.ident, w.lat, w.lon, truncate(w.name.as_deref().unwrap_or(&w.ident), 25), id])?;
        places.add(&w.ident, Place { table: "EA", id, region: w.region_code.clone(), airport: None, lat: w.lat, lon: w.lon });
    }
    Ok(row)
}

fn write_ils(conn: &Connection, nav: &NavSet, variation: &Variation, airports: &HashMap<String, AirportRow>, places: &mut Places) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT INTO tbl_pi_localizers_glideslopes (airport_identifier, area_code, gs_angle, gs_elevation, gs_latitude, gs_longitude, icao_code, ils_mls_gls_category, \
         llz_bearing, llz_frequency, llz_identifier, llz_latitude, llz_longitude, llz_truebearing, runway_identifier, station_declination, id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
    )?;
    let thresholds: HashMap<(&str, &str), (f64, f64)> = nav.runways.iter().map(|r| ((r.airport_icao.as_str(), r.ident.as_str()), (r.lat, r.lon))).collect();
    let mut row = 0usize;
    for i in &nav.ils {
        let Some(apt) = airports.get(&i.airport_icao) else { continue };
        row += 1;
        let id = format!("PI:{row}");
        let var = variation.at(i.lat, i.lon).unwrap_or(0.0);
        let gs = i.glidepath_deg.map(|_| thresholds.get(&(i.airport_icao.as_str(), i.runway_ident.as_str())).copied().unwrap_or((i.lat, i.lon)));
        stmt.execute(params![
            i.airport_icao,
            apt.area,
            i.glidepath_deg,
            i.glidepath_deg.map(|_| i.elevation_ft.round() as i64),
            gs.map(|g| g.0),
            gs.map(|g| g.1),
            apt.region,
            ils_category(i.category),
            i.course_mag_deg,
            i.frequency_mhz,
            i.ident,
            i.lat,
            i.lon,
            (i.course_mag_deg + var).rem_euclid(360.0),
            format!("RW{}", i.runway_ident),
            var,
            id,
        ])?;
        places.add(&i.ident, Place { table: "PI", id, region: apt.region.clone(), airport: Some(i.airport_icao.clone()), lat: i.lat, lon: i.lon });
    }
    Ok(row)
}

fn level_code(l: AirwayLevel) -> &'static str {
    match l {
        AirwayLevel::Both => "B",
        AirwayLevel::High => "H",
        AirwayLevel::Low => "L",
    }
}

/// The airways, one row per fix, each continuous run of them closed by a fix whose
/// description code says it ends the route.
fn write_airways(conn: &Connection, nav: &NavSet, places: &Places) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT INTO tbl_er_enroute_airways (area_code, flightlevel, icao_code, minimum_altitude1, route_identifier, route_type, seqno, waypoint_description_code, \
         waypoint_identifier, waypoint_latitude, waypoint_longitude, waypoint_ref_table, waypoint_id) VALUES (?1, ?2, ?3, ?4, ?5, 'O', ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let mut written = 0usize;
    for a in &nav.airways {
        let mut seq = 10i64;
        let mut last: Option<(f64, f64)> = None;
        for (i, leg) in a.legs.iter().enumerate() {
            let closes = leg.is_end || i + 1 == a.legs.len() || a.legs.get(i + 1).is_some_and(|n| n.is_start || n.from_ident != leg.to_ident);
            let opens = i == 0 || leg.is_start || a.legs.get(i - 1).is_some_and(|p| p.to_ident != leg.from_ident || p.is_end);
            let mut fixes: Vec<(&str, &str, bool)> = Vec::with_capacity(2);
            if opens {
                last = None;
                fixes.push((&leg.from_ident, &leg.from_region, false));
            }
            fixes.push((&leg.to_ident, &leg.to_region, closes));
            for (ident, region, end) in fixes {
                let Some(p) = places.find(ident, Want::Fix, None, Some(region), last, 600.0).or_else(|| places.find(ident, Want::Fix, None, Some(region), None, 0.0)) else { continue };
                let code = match (p.table, end) {
                    ("D " | "DB", true) => "VE  ",
                    ("D " | "DB", false) => "V   ",
                    (_, true) => "EE  ",
                    (_, false) => "E   ",
                };
                stmt.execute(params![
                    "UNK",
                    level_code(leg.level),
                    p.region,
                    leg.min_ft.map(|v| v.round() as i64),
                    a.ident,
                    seq,
                    code,
                    ident,
                    p.lat,
                    p.lon,
                    p.table,
                    p.id,
                ])?;
                last = Some((p.lat, p.lon));
                seq += 10;
                written += 1;
            }
        }
    }
    Ok(written)
}

/// The part of a procedure a `ProcedureRec` holds, named the layout's way.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Part {
    Runway,
    Common,
    Enroute,
    ApproachTransition,
    Final,
    Missed,
}

fn part_of(p: &ProcedureRec) -> Part {
    let trans = p.transition_ident.as_deref().unwrap_or("");
    let runway_like = trans.starts_with("RW") || trans == "ALL";
    match (p.kind, p.part.as_str()) {
        (ProcKind::Approach, "final") => Part::Final,
        (ProcKind::Approach, "missed") => Part::Missed,
        (ProcKind::Approach, _) if !trans.is_empty() => Part::ApproachTransition,
        (ProcKind::Approach, _) => Part::Final,
        (_, "runway") => Part::Runway,
        (_, "common") => Part::Common,
        (_, "enroute") => Part::Enroute,
        _ if trans.is_empty() => Part::Common,
        _ if runway_like => Part::Runway,
        _ => Part::Enroute,
    }
}

/// ARINC route types: SIDs number runway transition 1, common route 2, enroute transition 3;
/// STARs the other way round; an approach's own path takes its type letter and its
/// transitions an `A`.
fn sid_star_route_type(kind: ProcKind, part: Part) -> &'static str {
    match (kind, part) {
        (ProcKind::Sid, Part::Runway) => "1",
        (ProcKind::Sid, Part::Enroute) => "3",
        (ProcKind::Star, Part::Enroute) => "1",
        (ProcKind::Star, Part::Runway) => "3",
        _ => "2",
    }
}

/// An approach's identifier as the layout writes it: type letter, runway, and the letter
/// that tells two of a kind apart (`I27L`, `R04LZ`, `R26-Y`).
fn approach_ident(code: char, runway: &str, suffix: Option<char>) -> String {
    let runway = runway.trim_start_matches("RW");
    match suffix {
        Some(s) if runway.len() == 2 => format!("{code}{runway}-{s}"),
        Some(s) => format!("{code}{runway}{s}"),
        None if runway.len() == 1 => format!("{code}-{runway}"),
        None => format!("{code}{runway}"),
    }
}

/// The description code: what sort of fix it is, whether it ends the segment or is flown
/// over, whether it begins the missed approach, and its role in the approach.
fn description_code(table: Option<&str>, end: bool, leg: &LegRec, first_missed: bool) -> Option<String> {
    let mut c = [b' '; 4];
    c[0] = match table {
        Some("D ") => b'V',
        Some("DB" | "PN") => b'N',
        Some("PG") => b'G',
        Some(_) => b'E',
        None => b' ',
    };
    c[1] = match (end, leg.is_flyover) {
        (true, true) => b'B',
        (true, false) => b'E',
        (false, true) => b'Y',
        (false, false) => b' ',
    };
    if first_missed {
        c[2] = b'M';
    }
    c[3] = if leg.is_map {
        b'M'
    } else if leg.is_faf {
        b'F'
    } else if leg.is_iaf {
        b'A'
    } else if leg.is_if {
        b'B'
    } else {
        b' '
    };
    (c != [b' '; 4]).then(|| String::from_utf8_lossy(&c).into_owned())
}

fn altitude(leg: &LegRec) -> (Option<&'static str>, Option<i64>, Option<i64>) {
    let ft = |v: Option<f64>| v.map(|v| v.round() as i64);
    let (desc, first, second) = match leg.altitude_rule {
        Some(AltitudeRule::At) => ("@", ft(leg.altitude1_ft), None),
        Some(AltitudeRule::AtOrAbove) => ("+", ft(leg.altitude1_ft), None),
        Some(AltitudeRule::AtOrBelow) => ("-", ft(leg.altitude1_ft), None),
        // The layout puts the ceiling first and the floor second.
        Some(AltitudeRule::Between) => ("B", ft(leg.altitude2_ft.or(leg.altitude1_ft)), ft(leg.altitude1_ft)),
        None => return (None, None, None),
    };
    if first.is_none() {
        (None, None, None)
    } else {
        (Some(desc), first, second)
    }
}

fn is_hold(t: PathTerminator) -> bool {
    matches!(t, PathTerminator::Ha | PathTerminator::Hf | PathTerminator::Hm)
}

/// Which records to write. FS2020 keeps an airport's procedures in more than one package,
/// so the same procedure can come more than once, and two copies run together would be one
/// procedure flown twice. A copy is a run of records for one procedure, and the one kept is
/// the one with the most fixes the source could place: some packages' records decode into
/// names that are no fixes at all (`4D7DA`, `F2M0L`), and those place none.
fn best_copies(procedures: &[ProcedureRec]) -> Vec<bool> {
    let key = |p: &ProcedureRec| (p.airport_icao.clone(), p.kind as u8, p.ident.clone());
    let placed = |p: &ProcedureRec| p.legs.iter().filter(|l| l.fix_ident.is_some() && l.fix_lat.is_some()).count();
    let mut starts = Vec::with_capacity(procedures.len());
    let mut score: HashMap<usize, usize> = HashMap::new();
    for (i, p) in procedures.iter().enumerate() {
        let start = if i > 0 && key(&procedures[i - 1]) == key(p) { starts[i - 1] } else { i };
        starts.push(start);
        *score.entry(start).or_default() += placed(p);
    }
    let mut best: HashMap<(String, u8, String), usize> = HashMap::new();
    for (i, p) in procedures.iter().enumerate() {
        if starts[i] != i {
            continue;
        }
        let e = best.entry(key(p)).or_insert(i);
        if score[&i] > score[e] {
            *e = i;
        }
    }
    procedures.iter().zip(&starts).map(|(p, s)| best.get(&key(p)) == Some(s)).collect()
}

/// A transition's identifier as the layout keeps it: up to five letters and digits. Where
/// the source's name is not one (FS2020 leaves some unreadable), the fix the layout names a
/// transition by stands in: the last of a departure's, the first of an arrival's or an
/// approach's.
fn transition_name(p: &ProcedureRec, part: Part) -> Option<String> {
    let named = p.transition_ident.as_deref().map(str::trim).filter(|t| (1..=5).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric())).map(str::to_string);
    let fix = |legs: &mut dyn Iterator<Item = &LegRec>| legs.filter_map(|l| l.fix_ident.as_deref()).map(str::trim).find(|f| !f.is_empty() && f.len() <= 5).map(str::to_string);
    match part {
        Part::Runway => named.or_else(|| p.runway_ident.as_ref().map(|r| format!("RW{}", r.trim_start_matches("RW")))).or_else(|| Some("ALL".into())),
        Part::Enroute if p.kind == ProcKind::Sid => named.or_else(|| fix(&mut p.legs.iter().rev())),
        Part::Enroute | Part::ApproachTransition => named.or_else(|| fix(&mut p.legs.iter())),
        _ => None,
    }
}

/// One procedure's leg rows waiting to be written: its table, identifier, route type,
/// transition, and its legs with the leg that begins the missed approach marked.
struct Segment<'a> {
    kind: ProcKind,
    airport: &'a str,
    ident: String,
    route_type: String,
    transition: Option<String>,
    legs: Vec<(&'a LegRec, bool)>,
}

/// Where one leg's fix, recommended navaid and arc centre are.
struct Placed<'p> {
    fix: Option<&'p Place>,
    navaid: Option<&'p Place>,
    centre: Option<&'p Place>,
}

/// The legs whose path ends at their fix, and so cannot be flown without it.
fn ends_at_fix(t: PathTerminator) -> bool {
    use PathTerminator as P;
    matches!(t, P::If | P::Tf | P::Cf | P::Df | P::Rf | P::Af | P::Ha | P::Hf | P::Hm | P::Fa | P::Fc | P::Fd | P::Fm | P::Pi)
}

/// Every leg of a segment placed, or None when one that ends at its fix cannot be.
fn place_segment<'p>(s: &Segment, apt: &AirportRow, places: &'p Places) -> Option<Vec<Placed<'p>>> {
    let mut last_at = Some((apt.lat, apt.lon));
    let mut out = Vec::with_capacity(s.legs.len());
    for (leg, _) in &s.legs {
        let near = leg.fix_lat.zip(leg.fix_lon);
        let named = leg.fix_ident.as_deref().filter(|f| !f.trim().is_empty());
        let fix = named.and_then(|f| {
            places
                .find(f, Want::Fix, Some(s.airport), leg.fix_region.as_deref(), near.or(last_at), if near.is_some() { 2.0 } else { 300.0 })
                .or_else(|| near.and_then(|_| places.find(f, Want::Fix, Some(s.airport), leg.fix_region.as_deref(), Some((apt.lat, apt.lon)), 300.0)))
        });
        if fix.is_none() && ends_at_fix(leg.path_terminator) {
            return None;
        }
        let from = fix.map(|p| (p.lat, p.lon)).or(Some((apt.lat, apt.lon)));
        let navaid = leg.recommended_navaid.as_deref().filter(|f| !f.trim().is_empty()).and_then(|f| places.find(f, Want::Navaid, Some(s.airport), leg.recommended_navaid_region.as_deref(), from, 300.0));
        let centre = leg.center_fix.as_deref().filter(|f| !f.trim().is_empty()).and_then(|f| places.find(f, Want::Fix, Some(s.airport), leg.center_fix_region.as_deref(), from, 100.0));
        if let Some(f) = fix {
            last_at = Some((f.lat, f.lon));
        }
        out.push(Placed { fix, navaid, centre });
    }
    Some(out)
}

fn write_procedures(conn: &Connection, nav: &NavSet, airports: &HashMap<String, AirportRow>, places: &Places) -> Result<(usize, usize, usize)> {
    // Approaches first get their identifiers, one per approach and unique at its airport.
    let mut idents: HashMap<(&str, &str), String> = HashMap::new();
    let mut taken: HashMap<&str, std::collections::HashSet<String>> = HashMap::new();
    for p in nav.procedures.iter().filter(|p| p.kind == ProcKind::Approach) {
        if idents.contains_key(&(p.airport_icao.as_str(), p.ident.as_str())) {
            continue;
        }
        let runway = p.runway_ident.as_deref().unwrap_or("");
        let code = p.approach_code.unwrap_or('R');
        let used = taken.entry(p.airport_icao.as_str()).or_default();
        let mut ident = approach_ident(code, runway, p.suffix);
        let mut spare = ['Z', 'Y', 'X', 'W', 'V', 'U', 'T', 'S', 'Q', 'P'].into_iter();
        while used.contains(&ident) {
            let Some(s) = spare.next() else { break };
            ident = approach_ident(code, runway, Some(s));
        }
        used.insert(ident.clone());
        idents.insert((p.airport_icao.as_str(), p.ident.as_str()), ident);
    }

    // Then every part in its place. An approach's final and missed legs are one segment, in
    // that order, so they are gathered before anything is written.
    let mut segments: Vec<Segment> = Vec::new();
    let mut finals: HashMap<(&str, &str), usize> = HashMap::new();
    let mut missed: HashMap<(&str, &str), Vec<&LegRec>> = HashMap::new();
    let kept = best_copies(&nav.procedures);
    for (p, _) in nav.procedures.iter().zip(&kept).filter(|(_, k)| **k) {
        if p.legs.is_empty() || !airports.contains_key(&p.airport_icao) {
            continue;
        }
        let part = part_of(p);
        match p.kind {
            ProcKind::Approach => {
                let key = (p.airport_icao.as_str(), p.ident.as_str());
                let ident = idents[&key].clone();
                match part {
                    Part::Missed => missed.entry(key).or_default().extend(p.legs.iter()),
                    Part::ApproachTransition => segments.push(Segment {
                        kind: p.kind,
                        airport: &p.airport_icao,
                        ident,
                        route_type: "A".into(),
                        transition: transition_name(p, part),
                        legs: p.legs.iter().map(|l| (l, false)).collect(),
                    }),
                    _ => match finals.get(&key) {
                        Some(&at) => segments[at].legs.extend(p.legs.iter().map(|l| (l, false))),
                        None => {
                            finals.insert(key, segments.len());
                            segments.push(Segment {
                                kind: p.kind,
                                airport: &p.airport_icao,
                                ident: ident.clone(),
                                route_type: p.approach_code.unwrap_or('R').to_string(),
                                transition: None,
                                legs: p.legs.iter().map(|l| (l, false)).collect(),
                            });
                        }
                    },
                }
            }
            _ => {
                let transition = transition_name(p, part);
                segments.push(Segment {
                    kind: p.kind,
                    airport: &p.airport_icao,
                    ident: truncate(&p.ident, 6),
                    route_type: sid_star_route_type(p.kind, part).into(),
                    transition,
                    legs: p.legs.iter().map(|l| (l, false)).collect(),
                });
            }
        }
    }
    for (key, legs) in missed {
        let Some(&at) = finals.get(&key) else { continue };
        for (i, l) in legs.into_iter().enumerate() {
            segments[at].legs.push((l, i == 0));
        }
    }

    // Every leg placed before anything is written: a part with a leg that ends at a fix
    // nobody can place is left out whole, since a leg to nowhere cannot be flown, and an
    // approach without its final segment takes its transitions with it.
    let resolved: Vec<Option<Vec<Placed>>> = segments.iter().map(|s| place_segment(s, &airports[s.airport], places)).collect();
    let lost_finals: std::collections::HashSet<(&str, &str)> = segments
        .iter()
        .zip(&resolved)
        .filter(|(s, r)| s.kind == ProcKind::Approach && s.route_type != "A" && r.is_none())
        .map(|(s, _)| (s.airport, s.ident.as_str()))
        .collect();
    let left_out = resolved.iter().filter(|r| r.is_none()).count();
    if left_out > 0 {
        log::warn!("{left_out} procedure parts left out: a leg in each ends at a fix the simulator's data does not place");
    }

    let mut counts = (0usize, 0usize, 0usize);
    for table in ["tbl_pd_sids", "tbl_pe_stars", "tbl_pf_iaps"] {
        let kind = match table {
            "tbl_pd_sids" => ProcKind::Sid,
            "tbl_pe_stars" => ProcKind::Star,
            _ => ProcKind::Approach,
        };
        let mut stmt = conn.prepare(&format!(
            "INSERT INTO {table} (airport_identifier, altitude_description, altitude1, altitude2, arc_radius, area_code, center_waypoint_icao_code, \
             center_waypoint_latitude, center_waypoint_longitude, center_waypoint_ref_table, center_waypoint, course_flag, course, distance_time, path_termination, \
             procedure_identifier, recommended_navaid_icao_code, recommended_navaid_latitude, recommended_navaid_longitude, recommended_navaid_ref_table, \
             recommended_navaid, rho, route_distance_holding_distance_time, route_type, seqno, speed_limit_description, speed_limit, theta, transition_altitude, \
             transition_identifier, turn_direction, waypoint_description_code, waypoint_icao_code, waypoint_identifier, waypoint_latitude, waypoint_longitude, \
             waypoint_ref_table, waypoint_identifier_id, recommended_navaid_id, center_waypoint_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, \
             ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40)"
        ))?;
        let mut n = 0usize;
        for (s, placed) in segments.iter().zip(&resolved).filter(|(s, _)| s.kind == kind) {
            let Some(placed) = placed else { continue };
            if s.kind == ProcKind::Approach && lost_finals.contains(&(s.airport, s.ident.as_str())) {
                continue;
            }
            let apt = &airports[s.airport];
            for (i, ((leg, first_missed), at)) in s.legs.iter().zip(placed).enumerate() {
                let end = i + 1 == s.legs.len();
                let (fix, navaid, centre) = (at.fix, at.navaid, at.centre);
                let (alt_desc, alt1, alt2) = altitude(leg);
                // A leg flown fix to fix has no course of its own; the simulator gives some one
                // anyway, often the reciprocal, which a reader might take at its word.
                let course = leg.course_deg.filter(|_| !matches!(leg.path_terminator, PathTerminator::If | PathTerminator::Tf | PathTerminator::Df | PathTerminator::Rf));
                let (distance, kind_of_distance) = if is_hold(leg.path_terminator) {
                    match leg.leg_length {
                        Some(v) if v >= 0.5 => (Some(v), Some("D")),
                        _ => (Some(1.0), Some("T")),
                    }
                } else {
                    (leg.leg_length, leg.leg_length.map(|_| "D"))
                };
                let arc = match (centre, fix) {
                    (Some(c), Some(f)) if matches!(leg.path_terminator, PathTerminator::Rf | PathTerminator::Af) => Some(crate::dispatch::distance_nm((c.lat, c.lon), (f.lat, f.lon))),
                    _ => None,
                };
                stmt.execute(params![
                    s.airport,
                    alt_desc,
                    alt1,
                    alt2,
                    arc,
                    apt.area,
                    centre.map(|p| p.region.as_str()),
                    centre.map(|p| p.lat),
                    centre.map(|p| p.lon),
                    centre.map(|p| p.table),
                    centre.and(leg.center_fix.as_deref()),
                    course.map(|_| "M"),
                    course,
                    distance,
                    leg.path_terminator.code(),
                    s.ident,
                    navaid.map(|p| p.region.as_str()),
                    navaid.map(|p| p.lat),
                    navaid.map(|p| p.lon),
                    navaid.map(|p| p.table),
                    navaid.and(leg.recommended_navaid.as_deref()),
                    leg.rho_nm,
                    kind_of_distance,
                    s.route_type,
                    ((i + 1) * 10) as i64,
                    leg.speed_limit_kt.map(|_| "-"),
                    leg.speed_limit_kt.map(|v| v.round() as i64),
                    leg.theta_deg,
                    apt.transition_altitude,
                    s.transition,
                    match leg.turn {
                        TurnDirection::Left => Some("L"),
                        TurnDirection::Right => Some("R"),
                        TurnDirection::Either => None,
                    },
                    description_code(fix.map(|p| p.table), end, leg, *first_missed),
                    fix.map(|p| p.region.as_str()),
                    fix.and(leg.fix_ident.as_deref()),
                    fix.map(|p| p.lat),
                    fix.map(|p| p.lon),
                    fix.map(|p| p.table),
                    fix.map(|p| p.id.as_str()),
                    navaid.map(|p| p.id.as_str()),
                    centre.map(|p| p.id.as_str()),
                ])?;
                n += 1;
            }
        }
        match kind {
            ProcKind::Sid => counts.0 = n,
            ProcKind::Star => counts.1 = n,
            ProcKind::Approach => counts.2 = n,
        }
    }
    Ok(counts)
}

/// Every non-empty reference in a database that does not resolve the way Fenix resolves it:
/// a row of the named table with the same identifier within a millionth of a degree. Fenix
/// refuses a database with any; a test holds this writer to none.
pub fn unresolved_references(conn: &Connection) -> Result<usize> {
    let target = |code: &str| -> Option<(&'static str, &'static str, &'static str, &'static str, bool)> {
        Some(match code.trim() {
            "PA" => ("tbl_pa_airports", "airport_identifier", "c.airport_ref_latitude", "c.airport_ref_longitude", false),
            "PG" => ("tbl_pg_runways", "runway_identifier", "c.runway_latitude", "c.runway_longitude", true),
            "EA" => ("tbl_ea_enroute_waypoints", "waypoint_identifier", "c.waypoint_latitude", "c.waypoint_longitude", false),
            "PC" => ("tbl_pc_terminal_waypoints", "waypoint_identifier", "c.waypoint_latitude", "c.waypoint_longitude", false),
            "D" => ("tbl_d_vhfnavaids", "navaid_identifier", "COALESCE(c.navaid_latitude, c.dme_latitude)", "COALESCE(c.navaid_longitude, c.dme_longitude)", false),
            "DB" => ("tbl_db_enroute_ndbnavaids", "navaid_identifier", "c.navaid_latitude", "c.navaid_longitude", false),
            "PN" => ("tbl_pn_terminal_ndbnavaids", "navaid_identifier", "c.navaid_latitude", "c.navaid_longitude", true),
            "PI" => ("tbl_pi_localizers_glideslopes", "llz_identifier", "c.llz_latitude", "c.llz_longitude", true),
            _ => return None,
        })
    };
    let sources: [(&str, &[(&str, &str)]); 4] = [
        ("tbl_er_enroute_airways", &[("waypoint", "waypoint_identifier")]),
        ("tbl_pd_sids", &[("waypoint", "waypoint_identifier"), ("recommended_navaid", "recommended_navaid"), ("center_waypoint", "center_waypoint")]),
        ("tbl_pe_stars", &[("waypoint", "waypoint_identifier"), ("recommended_navaid", "recommended_navaid"), ("center_waypoint", "center_waypoint")]),
        ("tbl_pf_iaps", &[("waypoint", "waypoint_identifier"), ("recommended_navaid", "recommended_navaid"), ("center_waypoint", "center_waypoint")]),
    ];
    let mut bad = 0usize;
    for (table, refs) in sources {
        let has_airport = table != "tbl_er_enroute_airways";
        for (prefix, ident_col) in refs {
            let codes: Vec<String> = conn
                .prepare(&format!("SELECT DISTINCT TRIM({prefix}_ref_table) FROM {table} WHERE TRIM(COALESCE({prefix}_ref_table, '')) <> ''"))?
                .query_map([], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            for code in codes {
                let Some((t, ident, lat, lon, scoped)) = target(&code) else {
                    bad += conn.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE TRIM({prefix}_ref_table) = ?1"), [&code], |r| r.get::<_, i64>(0))? as usize;
                    continue;
                };
                let scope = if scoped && has_airport { "AND c.airport_identifier = s.airport_identifier" } else { "" };
                let sql = format!(
                    "SELECT COUNT(*) FROM {table} s WHERE TRIM(s.{prefix}_ref_table) = ?1 AND NOT EXISTS (SELECT 1 FROM {t} c WHERE TRIM(c.{ident}) = TRIM(s.{ident_col}) {scope} \
                     AND (s.{prefix}_latitude IS NULL OR ({lat} - s.{prefix}_latitude) BETWEEN -0.000001 AND 0.000001) \
                     AND (s.{prefix}_longitude IS NULL OR ({lon} - s.{prefix}_longitude) BETWEEN -0.000001 AND 0.000001))"
                );
                bad += conn.query_row(&sql, [&code], |r| r.get::<_, i64>(0))? as usize;
            }
        }
    }
    Ok(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NavSet {
        let leg = |pt: PathTerminator, fix: &str, at: Option<(f64, f64)>| LegRec { path_terminator: pt, fix_ident: Some(fix.into()), fix_lat: at.map(|a| a.0), fix_lon: at.map(|a| a.1), ..Default::default() };
        NavSet {
            cycle: "2610".into(),
            airports: vec![
                AirportRec { icao: "EGLL".into(), name: "LONDON HEATHROW".into(), area_code: "EUR".into(), icao_code: "EG".into(), lat: 51.4706, lon: -0.4619, elevation_ft: 83.0, transition_altitude_ft: Some(6000.0), ..Default::default() },
                AirportRec { icao: "EGLL".into(), name: "DUPLICATE".into(), ..Default::default() },
            ],
            runways: vec![RunwayRec { airport_icao: "EGLL".into(), ident: "27L".into(), heading_true_deg: 269.7, length_ft: 12799.0, width_ft: 164.0, surface: Surface::Asphalt, lat: 51.4648, lon: -0.4341, elevation_ft: 78.0, ..Default::default() }],
            navaids: vec![
                NavaidRec { ident: "BNN".into(), kind: NavaidKind::VorDme, name: "BOVINGDON".into(), region_code: "EG".into(), area_code: "EUR".into(), frequency: 113.75, lat: 51.7262, lon: -0.5498, magnetic_variation_deg: 1.0, range_nm: Some(125.0), ..Default::default() },
                NavaidRec { ident: "BNN".into(), kind: NavaidKind::VorDme, name: "BRONNOY".into(), region_code: "EN".into(), area_code: "EUR".into(), frequency: 115.3, lat: 65.457, lon: 12.214, magnetic_variation_deg: 5.0, range_nm: Some(130.0), ..Default::default() },
                NavaidRec { ident: "EPM".into(), kind: NavaidKind::Ndb, name: "EPSOM".into(), region_code: "EG".into(), area_code: "EUR".into(), frequency: 316.0, lat: 51.319, lon: -0.372, ..Default::default() },
            ],
            ils: vec![IlsRec { ident: "ILL".into(), airport_icao: "EGLL".into(), runway_ident: "27L".into(), frequency_mhz: 109.5, course_mag_deg: 269.0, glidepath_deg: Some(3.0), category: IlsCategory::Cat3, lat: 51.4776, lon: -0.4856, elevation_ft: 80.0, ..Default::default() }],
            waypoints: vec![
                WaypointRec { ident: "CF27L".into(), region_code: "EG".into(), area_code: "EUR".into(), lat: 51.462, lon: -0.231, ..Default::default() },
                WaypointRec { ident: "SLANY".into(), region_code: "EI".into(), area_code: "EUR".into(), lat: 52.158, lon: -5.842, ..Default::default() },
                WaypointRec { ident: "OFSOX".into(), region_code: "EG".into(), area_code: "EUR".into(), lat: 52.029, lon: -5.195, ..Default::default() },
            ],
            airways: vec![AirwayRec {
                ident: "L9".into(),
                legs: vec![AirwayLegRec { sequence: 1, from_ident: "SLANY".into(), from_region: "EI".into(), to_ident: "OFSOX".into(), to_region: "EG".into(), level: AirwayLevel::Both, is_start: true, is_end: true, min_ft: Some(16000.0) }],
            }],
            procedures: vec![
                ProcedureRec {
                    airport_icao: "EGLL".into(),
                    kind: ProcKind::Approach,
                    ident: "ILS RWY 27L".into(),
                    runway_ident: Some("27L".into()),
                    part: "final".into(),
                    approach_code: Some('I'),
                    legs: vec![
                        LegRec { is_if: true, altitude_rule: Some(AltitudeRule::At), altitude1_ft: Some(3000.0), ..leg(PathTerminator::If, "CF27L", Some((51.462, -0.231))) },
                        LegRec { is_map: true, recommended_navaid: Some("ILL".into()), course_deg: Some(269.0), ..leg(PathTerminator::Cf, "RW27L", Some((51.4648, -0.4341))) },
                    ],
                    ..Default::default()
                },
                ProcedureRec {
                    airport_icao: "EGLL".into(),
                    kind: ProcKind::Approach,
                    ident: "ILS RWY 27L".into(),
                    runway_ident: Some("27L".into()),
                    part: "missed".into(),
                    approach_code: Some('I'),
                    legs: vec![LegRec { path_terminator: PathTerminator::Ca, course_deg: Some(269.0), altitude_rule: Some(AltitudeRule::AtOrAbove), altitude1_ft: Some(1080.0), ..Default::default() }, leg(PathTerminator::Df, "BNN", None)],
                    ..Default::default()
                },
                ProcedureRec {
                    airport_icao: "EGLL".into(),
                    kind: ProcKind::Approach,
                    ident: "ILS RWY 27L".into(),
                    runway_ident: Some("27L".into()),
                    transition_ident: Some("EPM".into()),
                    approach_code: Some('I'),
                    legs: vec![leg(PathTerminator::If, "EPM", Some((51.319, -0.372))), leg(PathTerminator::Tf, "CF27L", None)],
                    ..Default::default()
                },
                ProcedureRec {
                    airport_icao: "EGLL".into(),
                    kind: ProcKind::Sid,
                    ident: "BPK7G".into(),
                    transition_ident: Some("RW27L".into()),
                    part: "runway".into(),
                    legs: vec![LegRec { path_terminator: PathTerminator::Ca, course_deg: Some(270.0), ..Default::default() }, leg(PathTerminator::Df, "BNN", None)],
                    ..Default::default()
                },
                ProcedureRec { airport_icao: "EGLL".into(), kind: ProcKind::Star, ident: "BNN1A".into(), part: "common".into(), legs: vec![leg(PathTerminator::If, "BNN", None)], ..Default::default() },
            ],
            mora: Vec::new(),
        }
    }

    fn written(name: &str) -> (PathBuf, Connection) {
        let path = std::env::temp_dir().join(format!("amdb-dfd2-{}-{name}.db3", std::process::id()));
        write(&sample(), &path).unwrap();
        let conn = Connection::open(&path).unwrap();
        (path, conn)
    }

    #[test]
    fn cycles_have_their_real_dates() {
        assert_eq!(cycle_dates("2503"), Some((NaiveDate::from_ymd_opt(2025, 3, 20).unwrap(), NaiveDate::from_ymd_opt(2025, 4, 16).unwrap())));
        assert_eq!(effective_fromto("2503").as_deref(), Some("2003160425"));
        assert_eq!(cycle_dates("2501").unwrap().0, NaiveDate::from_ymd_opt(2025, 1, 23).unwrap());
        assert_eq!(cycle_dates("2001").unwrap().0, NaiveDate::from_ymd_opt(2020, 1, 2).unwrap());
        assert!(cycle_dates("2514").is_none(), "2025 has thirteen cycles");
        assert!(cycle_dates("0000").is_none());
        assert_eq!(cycle_on(NaiveDate::from_ymd_opt(2025, 4, 16).unwrap()), "2503");
        assert_eq!(cycle_on(NaiveDate::from_ymd_opt(2025, 4, 17).unwrap()), "2504");
        assert_eq!(cycle_on(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()), "2513");
    }

    #[test]
    fn every_table_and_column_of_fenix_3s_own_database_is_there() {
        let (path, conn) = written("schema");
        let tables: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name LIKE 'tbl_%'", [], |r| r.get(0)).unwrap();
        assert_eq!(tables, 27);
        let header: (String, String, String, String) = conn.query_row("SELECT cycle, revision, dataset_version, effective_fromto FROM tbl_hdr_header", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!(header, ("2610".into(), "001".into(), "2.0.0.0".into(), effective_fromto("2610").unwrap()));
        assert_eq!(import_path(Path::new("X"), "2610"), Path::new("X").join("Import").join("2610-r001.db3"));
        drop(conn);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn every_reference_resolves_the_way_fenix_resolves_it() {
        let (path, conn) = written("refs");
        assert_eq!(unresolved_references(&conn).unwrap(), 0);
        let airports: i64 = conn.query_row("SELECT COUNT(*) FROM tbl_pa_airports", [], |r| r.get(0)).unwrap();
        assert_eq!(airports, 1, "airport identifiers are unique");
        // The missed approach's BNN is Bovingdon, the one near Heathrow, not Bronnoy.
        let bnn: String = conn.query_row("SELECT waypoint_identifier_id FROM tbl_pf_iaps WHERE waypoint_identifier = 'BNN'", [], |r| r.get(0)).unwrap();
        let name: String = conn.query_row("SELECT navaid_name FROM tbl_d_vhfnavaids WHERE id = ?1", [&bnn], |r| r.get(0)).unwrap();
        assert_eq!(name, "BOVINGDON");
        let rw: (String, String) = conn.query_row("SELECT waypoint_ref_table, waypoint_description_code FROM tbl_pf_iaps WHERE waypoint_identifier = 'RW27L'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(rw, ("PG".into(), "G  M".into()));
        let ils: String = conn.query_row("SELECT recommended_navaid_ref_table FROM tbl_pf_iaps WHERE recommended_navaid = 'ILL'", [], |r| r.get(0)).unwrap();
        assert_eq!(ils, "PI");
        drop(conn);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn procedures_keep_their_parts_apart() {
        let (path, conn) = written("parts");
        let rows: Vec<(String, String, Option<String>, i64, String, Option<String>)> = conn
            .prepare("SELECT procedure_identifier, route_type, transition_identifier, seqno, path_termination, waypoint_description_code FROM tbl_pf_iaps ORDER BY route_type, transition_identifier, seqno")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(rows.iter().all(|r| r.0 == "I27L"));
        assert_eq!(rows.iter().filter(|r| r.1 == "A").map(|r| r.2.as_deref()).collect::<Vec<_>>(), vec![Some("EPM"), Some("EPM")]);
        let finals: Vec<_> = rows.iter().filter(|r| r.1 == "I").collect();
        assert_eq!(finals.iter().map(|r| (r.3, r.4.as_str())).collect::<Vec<_>>(), vec![(10, "IF"), (20, "CF"), (30, "CA"), (40, "DF")], "final, then missed, in one run");
        assert_eq!(finals[2].5.as_deref(), Some("  M "), "the missed approach's first leg says so");
        let sid: (String, String) = conn.query_row("SELECT route_type, transition_identifier FROM tbl_pd_sids LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(sid, ("1".into(), "RW27L".into()));
        let star: (String, Option<String>) = conn.query_row("SELECT route_type, transition_identifier FROM tbl_pe_stars LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(star, ("2".into(), None));
        let airway: Vec<(String, String)> = conn.prepare("SELECT waypoint_identifier, waypoint_description_code FROM tbl_er_enroute_airways ORDER BY seqno").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<std::result::Result<_, _>>().unwrap();
        assert_eq!(airway, vec![("SLANY".into(), "E   ".into()), ("OFSOX".into(), "EE  ".into())]);
        drop(conn);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_procedure_that_comes_twice_is_written_once_and_unreadable_names_are_replaced() {
        let rec = |ident: &str, part: &str, legs: usize| ProcedureRec {
            airport_icao: "EGLL".into(),
            kind: ProcKind::Sid,
            ident: ident.into(),
            part: part.into(),
            legs: (0..legs).map(|i| LegRec { fix_ident: Some(format!("F{i}")), fix_lat: Some(51.0), fix_lon: Some(0.0), ..Default::default() }).collect(),
            ..Default::default()
        };
        let unplaced = |mut p: ProcedureRec| {
            p.legs.iter_mut().for_each(|l| l.fix_lat = None);
            p
        };
        let all = vec![rec("BPK5K", "runway", 1), rec("BPK5K", "enroute", 1), rec("CPT4K", "runway", 1), unplaced(rec("BPK5K", "runway", 2)), unplaced(rec("BPK5K", "enroute", 3))];
        assert_eq!(best_copies(&all), vec![true, true, true, false, false], "the copy whose fixes have places wins");
        let both = vec![rec("BPK5K", "runway", 1), rec("BPK5K", "runway", 2)];
        assert_eq!(best_copies(&both), vec![true, true], "one run is one copy");
        let garbled = ProcedureRec { transition_ident: Some("\u{1}".into()), ..rec("BPK5K", "enroute", 3) };
        assert_eq!(transition_name(&garbled, Part::Enroute).as_deref(), Some("F2"), "a departure's transition is named by its last fix");
        let star = ProcedureRec { kind: ProcKind::Star, ..garbled.clone() };
        assert_eq!(transition_name(&star, Part::Enroute).as_deref(), Some("F0"), "an arrival's by its first");
        assert_eq!(transition_name(&ProcedureRec { transition_ident: Some("LAM".into()), ..garbled }, Part::Enroute).as_deref(), Some("LAM"));
    }

    #[test]
    fn approach_identifiers_follow_the_layout() {
        assert_eq!(approach_ident('I', "27L", None), "I27L");
        assert_eq!(approach_ident('R', "04L", Some('Z')), "R04LZ");
        assert_eq!(approach_ident('R', "26", Some('Y')), "R26-Y");
        assert_eq!(approach_ident('V', "A", None), "V-A");
    }

    #[test]
    fn navaid_classes_are_five_characters() {
        for kind in [NavaidKind::Vor, NavaidKind::VorDme, NavaidKind::Vortac, NavaidKind::Tacan, NavaidKind::Dme, NavaidKind::IlsDme, NavaidKind::Ndb, NavaidKind::NdbDme] {
            assert_eq!(navaid_class(&NavaidRec { kind, range_nm: Some(130.0), ..Default::default() }).len(), 5);
        }
        assert_eq!(navaid_class(&NavaidRec { kind: NavaidKind::VorDme, range_nm: Some(130.0), ..Default::default() }), "VDH  ");
    }
}
