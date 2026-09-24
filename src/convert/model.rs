//! The in-memory model a navigation-data converter writes from: one shape, independent of
//! whether the target is Fenix's normalised SQLite or Navigraph's ARINC-424-shaped `tbl_*`
//! layout. A source module fills a `NavSet` once; `fenix.rs` and `dfd.rs` each turn it into
//! their own file without needing to know how the other reads.
//!
//! This copy does not exist elsewhere in the crate yet — the read half is being written in
//! parallel — so it is defined here to the spec agreed for it. A handful of fields go
//! beyond that spec because a target schema demands them (Fenix's `TerminalLegsEx` needs a
//! fly-over flag per leg, and an `RF`/arc leg needs to know what it turns about); each is
//! marked below. Expect a small merge once both halves land.

/// A full navigation cycle, ready to write to any target format.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct NavSet {
    /// AIRAC cycle, e.g. "2503".
    pub cycle: String,
    pub airports: Vec<AirportRec>,
    pub runways: Vec<RunwayRec>,
    pub navaids: Vec<NavaidRec>,
    pub ils: Vec<IlsRec>,
    pub waypoints: Vec<WaypointRec>,
    pub airways: Vec<AirwayRec>,
    pub procedures: Vec<ProcedureRec>,
    /// Grid minimum off-route altitudes: read straight off the ground, unlike holds and
    /// MSA sectors below, so this is the one of the three enroute-safety tables that is
    /// never left empty.
    pub mora: Vec<MoraRec>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AirportRec {
    pub icao: String,
    pub name: String,
    /// ARINC area code, e.g. "USA", "EUR".
    pub area_code: String,
    /// ARINC ICAO region prefix, e.g. "K", "ED", "EG".
    pub icao_code: String,
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: f64,
    pub transition_altitude_ft: Option<f64>,
    pub transition_level_ft: Option<f64>,
    /// The speed limit below `speed_limit_altitude_ft`: 250 KT below 10,000 FT, most
    /// places.
    pub speed_limit_kt: Option<f64>,
    pub speed_limit_altitude_ft: Option<f64>,
}

/// Runway surface, reproducing the ARINC 424 / Fenix `SurfaceTypes` codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Surface {
    Asphalt,
    Concrete,
    Gravel,
    Grass,
    Water,
    Snow,
    Ice,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RunwayRec {
    pub airport_icao: String,
    /// e.g. "09L".
    pub ident: String,
    pub heading_true_deg: f64,
    pub length_ft: f64,
    pub width_ft: f64,
    pub surface: Surface,
    /// The landing threshold.
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: f64,
}

/// Reproduces the Fenix `NavaidTypes` codes, which is what this crate writes to; the
/// ARINC/DFD writer maps the same kinds onto its own `type_of_facility` / VOR-type text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum NavaidKind {
    #[default]
    Vor,
    Vortac,
    Tacan,
    VorDme,
    Ndb,
    NdbDme,
    IlsDme,
    Dme,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct NavaidRec {
    pub ident: String,
    pub kind: NavaidKind,
    pub name: String,
    /// ARINC region code, e.g. "K1", "EU".
    pub region_code: String,
    pub area_code: String,
    /// Megahertz, except for an NDB, which is kilohertz — the same convention
    /// `sources::navdata::Beacon` already uses in this crate.
    pub frequency: f64,
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: f64,
    pub magnetic_variation_deg: f64,
    pub range_nm: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum IlsCategory {
    #[default]
    Loc,
    Cat1,
    Cat2,
    Cat3,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct IlsRec {
    pub ident: String,
    pub airport_icao: String,
    pub runway_ident: String,
    pub frequency_mhz: f64,
    pub course_mag_deg: f64,
    pub glidepath_deg: Option<f64>,
    pub category: IlsCategory,
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: f64,
    pub has_dme: bool,
    /// The height the glidepath crosses the threshold at, which a chart prints and which
    /// cannot be worked out from anything else.
    pub crossing_height_ft: Option<f64>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WaypointRec {
    pub ident: String,
    pub region_code: String,
    pub area_code: String,
    pub name: Option<String>,
    pub lat: f64,
    pub lon: f64,
}

/// Which altitudes an airway leg is open at. Carried for completeness even though no
/// source on this machine gives a leg's MEA or maximum altitude (see `AirwayLegRec`) — the
/// enumeration is reproduced now so a source that does have it needs no schema change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AirwayLevel {
    #[default]
    Both,
    High,
    Low,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AirwayLegRec {
    pub sequence: u32,
    pub from_ident: String,
    pub from_region: String,
    pub to_ident: String,
    pub to_region: String,
    pub level: AirwayLevel,
    pub is_start: bool,
    pub is_end: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AirwayRec {
    pub ident: String,
    pub legs: Vec<AirwayLegRec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ProcKind {
    #[default]
    Sid,
    Star,
    Approach,
}

/// ARINC 424 path terminators (field 5.17), the leg types every source and every target
/// format agree on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PathTerminator {
    #[default]
    If,
    Tf,
    Cf,
    Df,
    Fa,
    Fc,
    Fd,
    Fm,
    Ca,
    Cd,
    Ci,
    Cr,
    Rf,
    Af,
    Va,
    Vd,
    Vi,
    Vm,
    Vr,
    Pi,
    Ha,
    Hf,
    Hm,
}

impl PathTerminator {
    /// The two-letter ARINC code, which is also what every target schema stores.
    pub fn code(self) -> &'static str {
        match self {
            PathTerminator::If => "IF",
            PathTerminator::Tf => "TF",
            PathTerminator::Cf => "CF",
            PathTerminator::Df => "DF",
            PathTerminator::Fa => "FA",
            PathTerminator::Fc => "FC",
            PathTerminator::Fd => "FD",
            PathTerminator::Fm => "FM",
            PathTerminator::Ca => "CA",
            PathTerminator::Cd => "CD",
            PathTerminator::Ci => "CI",
            PathTerminator::Cr => "CR",
            PathTerminator::Rf => "RF",
            PathTerminator::Af => "AF",
            PathTerminator::Va => "VA",
            PathTerminator::Vd => "VD",
            PathTerminator::Vi => "VI",
            PathTerminator::Vm => "VM",
            PathTerminator::Vr => "VR",
            PathTerminator::Pi => "PI",
            PathTerminator::Ha => "HA",
            PathTerminator::Hf => "HF",
            PathTerminator::Hm => "HM",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum TurnDirection {
    #[default]
    Either,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AltitudeRule {
    #[default]
    At,
    AtOrAbove,
    AtOrBelow,
    /// Between `altitude1_ft` (the floor) and `altitude2_ft` (the ceiling).
    Between,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LegRec {
    pub sequence: u32,
    pub path_terminator: PathTerminator,
    pub fix_ident: Option<String>,
    pub fix_region: Option<String>,
    pub recommended_navaid: Option<String>,
    pub recommended_navaid_region: Option<String>,
    /// The radial from the recommended navaid, degrees.
    pub theta_deg: Option<f64>,
    /// The distance from the recommended navaid, nautical miles.
    pub rho_nm: Option<f64>,
    pub course_deg: Option<f64>,
    /// The leg length: nautical miles for a distance-terminated leg, minutes for a
    /// time-terminated one (`FC`/holds); the writer does not need to tell which, since it
    /// only ever copies the figure across.
    pub leg_length: Option<f64>,
    pub turn: TurnDirection,
    pub altitude_rule: Option<AltitudeRule>,
    pub altitude1_ft: Option<f64>,
    pub altitude2_ft: Option<f64>,
    pub speed_limit_kt: Option<f64>,
    pub is_iaf: bool,
    pub is_if: bool,
    pub is_faf: bool,
    pub is_map: bool,
    /// Beyond the given spec: Fenix's `TerminalLegsEx.IsFlyOver` needs it and there is
    /// nowhere else to carry it.
    pub is_flyover: bool,
    /// Beyond the given spec: the centre of an `RF` (constant-radius) or `AF` (DME arc)
    /// leg's turn, which Fenix stores as `TerminalLegs.CenterID`/`CenterLat`/`CenterLon`.
    /// `None` on any other leg type.
    pub center_fix: Option<String>,
    pub center_fix_region: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProcedureRec {
    pub airport_icao: String,
    pub kind: ProcKind,
    /// The procedure's published name, e.g. "DEGU1E" or "NIDU2X".
    pub ident: String,
    /// `None` for the procedure's common route; `Some(name)` for one named enroute
    /// transition. A procedure with several transitions or several runway variants
    /// appears as one `ProcedureRec` per already-flattened variant — this crate's own
    /// readers already produce full leg sequences this way (see `Procedures` in
    /// `pipeline.rs`), and Fenix's own `Terminals` table is one row per flattened variant
    /// too, so no re-assembly happens on the way out.
    pub transition_ident: Option<String>,
    /// `None` when the procedure is not runway-specific (an enroute transition, or a STAR
    /// feeding more than one runway).
    pub runway_ident: Option<String>,
    pub legs: Vec<LegRec>,
}

/// One cell of the grid minimum off-route altitude table: never left empty, because it is
/// read straight off the terrain rather than published by an authority the way a hold or an
/// MSA sector is.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct MoraRec {
    pub lat: f64,
    pub lon: f64,
    pub altitude_ft: f64,
}
