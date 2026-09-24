//! The intermediate model a navigation database is written out from.
//!
//! Every reader (MSFS's own BGLs today, whatever else joins it later) fills one of these;
//! every writer (PMDG's format, Fenix's, MORA-only tooling) reads one back. Neither side
//! needs to know the other exists. The fields are ARINC 424's rather than any one writer's,
//! because a field a writer does not use is cheap to ignore and a field it needs but was
//! never carried here is not recoverable after the fact.

use serde::{Deserialize, Serialize};

/// A whole navigation database, read from one source and not yet written to any format.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NavSet {
    /// The AIRAC cycle this data was built for, where the source can say. Left as a plain
    /// string because sources disagree on how to spell it: "2503", "AIRAC 2503", a pair of
    /// effective/expiry dates. A source that carries no cycle label at all leaves this
    /// empty rather than guessing.
    pub cycle: String,
    pub airports: Vec<AirportRec>,
    /// Both ends of a runway as separate records ("09L" and "27R" of the one strip), the
    /// way ARINC 424 and every FMS database keeps them: an approach or a departure is
    /// always to or from one end, never to the strip as a whole.
    pub runways: Vec<RunwayRec>,
    pub navaids: Vec<NavaidRec>,
    /// Localisers, glideslopes and marker beacons: the parts of an ILS with their own
    /// position and frequency, kept apart from `navaids` because an FMS treats them apart.
    pub ils: Vec<IlsRec>,
    pub waypoints: Vec<WaypointRec>,
    /// One record per airway segment, in the sequence the airway is flown. A whole airway
    /// is the run of records that share an `airway_ident`, in `sequence` order.
    pub airways: Vec<AirwayRec>,
    pub procedures: Vec<ProcedureRec>,
    /// Minimum off-route altitudes. Another engineer fills this from a different source;
    /// nothing here writes to it, so it is always empty out of this reader.
    pub mora: Vec<MoraRec>,
}

/// What a beacon transmits. TACAN carries both a bearing and a DME element, which is why
/// it is its own kind rather than "VOR with a DME".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavaidKind {
    Vor,
    Ndb,
    Dme,
    Tacan,
}

impl Default for NavaidKind {
    fn default() -> Self {
        NavaidKind::Vor
    }
}

/// A paved surface an aircraft's performance tables care about the friction of, or the
/// lack of pavement at all. Kept coarse: a writer that wants to know "hard or soft" or
/// "usable or not" can answer that from this; the finer surface codes ARINC 424 has are
/// not something a BGL carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Hard,
    Soft,
    Water,
    Unknown,
}

impl Default for Surface {
    fn default() -> Self {
        Surface::Unknown
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AirportRec {
    pub icao: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: f64,
    /// Degrees, positive east. Absent where nothing in the source states or implies it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magnetic_variation_deg: Option<f64>,
}

/// One end of a runway. `ident` is the end's own designator ("09L"), not the strip's
/// ("09L/27R"): a leg or an approach is always flown to one end.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunwayRec {
    pub airport_icao: String,
    pub ident: String,
    /// The threshold of this end - where a landing roll may begin, which is not always
    /// where the pavement itself begins.
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: f64,
    /// True heading of this end's centreline, degrees.
    pub heading_true_deg: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magnetic_variation_deg: Option<f64>,
    pub length_ft: f64,
    pub width_ft: f64,
    pub surface: Surface,
    /// Distance the displaced threshold sits down the runway from the physical start,
    /// zero where there is none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_displaced_threshold: bool,
    #[serde(default)]
    pub displaced_threshold_ft: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NavaidRec {
    pub ident: String,
    /// ARINC 424's region code (two letters: "K1", "EU", ...), where the source carries
    /// one; empty where it does not and idents alone must do to disambiguate.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub region: String,
    pub kind: NavaidKind,
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: f64,
    /// Megahertz for a VOR, TACAN's paired VOR channel or a DME co-located with one;
    /// kilohertz for an NDB.
    pub frequency: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magnetic_variation_deg: Option<f64>,
    /// Nautical miles, where the source states a service range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_nm: Option<f64>,
}

/// What sort of ILS component one record is. A full ILS is a localiser plus a glidepath
/// plus, sometimes, marker beacons - four records here, not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IlsKind {
    Localiser,
    Glideslope,
    InnerMarker,
    MiddleMarker,
    OuterMarker,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IlsRec {
    pub airport_icao: String,
    pub runway_ident: String,
    pub ident: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<IlsKind>,
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: f64,
    /// Megahertz; zero for a marker beacon, which has none of its own.
    #[serde(default)]
    pub frequency: f64,
    /// True course of the localiser front course, degrees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub course_true_deg: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magnetic_variation_deg: Option<f64>,
    /// Glidepath angle, degrees; 3.0 for the great majority, but not all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glidepath_deg: Option<f64>,
    /// ICAO Annex 10 category, where the source states one ("I", "II", "IIIB", ...).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub category: String,
}

/// Whether a fix is one an aircraft would look up in the enroute structure or only ever
/// meets as part of one airport's own procedures. ARINC 424 keeps the same distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaypointKind {
    Enroute,
    Terminal,
}

impl Default for WaypointKind {
    fn default() -> Self {
        WaypointKind::Enroute
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WaypointRec {
    pub ident: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub region: String,
    pub kind: WaypointKind,
    /// The airport a terminal waypoint belongs to; empty for an enroute one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub airport_icao: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// One segment of an airway: the fix it runs through and the limits that apply between
/// it and the next fix in sequence. A route engine that only wants "is this the low or
/// the high structure" reads `minimum_altitude_ft`/`maximum_altitude_ft` off this, which
/// is the whole reason this record exists rather than a bare list of fix names.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AirwayRec {
    pub airway_ident: String,
    /// Position along the airway, starting at 1; the airway itself is every record
    /// sharing `airway_ident`, ordered by this.
    pub sequence: u32,
    pub fix_ident: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix_region: String,
    pub lat: f64,
    pub lon: f64,
    /// Feet. `None` where the source states no floor for this segment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_altitude_ft: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_altitude_ft: Option<f64>,
    /// True where the segment may only be flown one way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub one_way: bool,
    /// True for the upper airway structure (jet routes, "J"/"Q"/"UL" and the like), false
    /// for the low-level structure ("V"/"A"/"L"). This is the split a PMDG-style database
    /// keys its high/low route lookup on.
    #[serde(default)]
    pub is_high_level: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureKind {
    Sid,
    Star,
    Approach,
}

impl Default for ProcedureKind {
    fn default() -> Self {
        ProcedureKind::Sid
    }
}

/// How a leg's altitude field or fields are to be read: unconstrained, at exactly one
/// value, at-or-above, at-or-below, or a window between two values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AltitudeRule {
    None,
    At,
    AtOrAbove,
    AtOrBelow,
    Between,
}

impl Default for AltitudeRule {
    fn default() -> Self {
        AltitudeRule::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnDirection {
    Left,
    Right,
}

/// What part a fix plays in a procedure, where the source marks one. A leg can be more
/// than one of these at once (a fix is often both the final approach fix and, on a
/// missed-approach-only procedure, the point the missed approach starts from), so this is
/// carried as flags on the leg rather than a single value here.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct FixRoleFlags {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub initial_approach_fix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub intermediate_fix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub final_approach_fix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub missed_approach_point: bool,
}

/// One leg of a procedure transition, carrying everything ARINC 424 gives a leg: the path
/// terminator, the fix it resolves to, the navaid it may be flown relative to, and the
/// constraints that apply while flying it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcedureLeg {
    /// ARINC 424 path terminator: "IF", "TF", "CF", "DF", "HM" and so on.
    pub path_terminator: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix_ident: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix_region: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_lon: Option<f64>,
    /// The navaid this leg is flown relative to (a `CF`'s reference, an arc's centre),
    /// where the path terminator has one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub recommended_navaid: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub recommended_navaid_region: String,
    /// Radial from the recommended navaid, degrees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theta_deg: Option<f64>,
    /// Distance from the recommended navaid, nautical miles: what "12 DME FUN" is built
    /// from, and the radius an arc leg holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rho_nm: Option<f64>,
    /// The leg's own course, degrees, for the path terminators that fly one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub course_deg: Option<f64>,
    /// The leg's own length, nautical miles, for the path terminators that have one
    /// (`FC`, `FD`, a hold's inbound leg).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_nm: Option<f64>,
    pub altitude_rule: AltitudeRule,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub altitude1_ft: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub altitude2_ft: Option<f64>,
    /// The most the aircraft may fly at this leg, knots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_limit_kt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_direction: Option<TurnDirection>,
    #[serde(default)]
    pub role: FixRoleFlags,
}

/// One transition of one procedure: a departure's runway, common or enroute portion; an
/// arrival's the same; an approach's transition, its final segment or its missed approach.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Transition {
    /// Empty for a procedure's common portion; a runway ("RW09L", or "ALL") or a fix name
    /// otherwise.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ident: String,
    pub legs: Vec<ProcedureLeg>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcedureRec {
    pub airport_icao: String,
    pub kind: ProcedureKind,
    /// The procedure's own name: "SKORR6", "ILS Y RWY 13R", and so on.
    pub ident: String,
    /// The runway an approach serves, where it names one; empty for a circling approach
    /// or where a SID/STAR's transitions carry their own runway idents instead.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub runway_ident: String,
    pub transitions: Vec<Transition>,
}

/// A minimum off-route altitude cell. Left undefined beyond the shape ARINC 424's MORA
/// grid needs, because filling this in is another engineer's part of the pipeline.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MoraRec {
    /// South-west corner of a one-degree cell.
    pub lat: i32,
    pub lon: i32,
    pub altitude_ft: f64,
}
