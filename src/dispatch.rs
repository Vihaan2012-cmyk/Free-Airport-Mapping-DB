//! The shapes the parts of flight planning hand one another.
//!
//! A flight plan is made by five parts that know nothing of each other's insides:
//!
//! * `route` finds the way over the airway network, obeying the network's rules
//!   (`EdgeRule`, `RouteRule`) and keeping out of what is drawn as a `Hazard`;
//! * `route::rad`, `route::oceanic`, `route::airspace` and `route::etops` supply those
//!   rules, hazards and extra airways;
//! * `weather` supplies the air (`WindField`), the reports (`Metar`, `Taf`) and more
//!   hazards;
//! * `perf` flies the route: the vertical profile, the fuel and the weights (`PerfPlan`);
//! * `ofp` puts it all together (`Dispatch`) and prints it.
//!
//! Everything that crosses from one part to another is defined here, and only here, so
//! each part can be written and tested against this file alone. It changes only by
//! agreement: a part that needs something more asks for it rather than adding it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------------
// The sphere.
// ---------------------------------------------------------------------------------

/// A place on the earth, latitude and longitude in degrees.
pub type LatLon = (f64, f64);

pub const EARTH_NM: f64 = 3440.065;

/// Great-circle distance, nautical miles.
pub fn distance_nm(a: LatLon, b: LatLon) -> f64 {
    let (p1, p2) = (a.0.to_radians(), b.0.to_radians());
    let dp = p2 - p1;
    let dl = (b.1 - a.1).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_NM * h.sqrt().min(1.0).asin()
}

/// Initial great-circle bearing, degrees true.
pub fn bearing_deg(a: LatLon, b: LatLon) -> f64 {
    let (p1, p2) = (a.0.to_radians(), b.0.to_radians());
    let dl = (b.1 - a.1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// The place a distance along a great circle from a start, on an initial bearing.
pub fn travel(from: LatLon, bearing_true_deg: f64, nm: f64) -> LatLon {
    let d = nm / EARTH_NM;
    let (p1, l1, b) = (from.0.to_radians(), from.1.to_radians(), bearing_true_deg.to_radians());
    let p2 = (p1.sin() * d.cos() + p1.cos() * d.sin() * b.cos()).asin();
    let l2 = l1 + (b.sin() * d.sin() * p1.cos()).atan2(d.cos() - p1.sin() * p2.sin());
    (p2.to_degrees(), (l2.to_degrees() + 540.0) % 360.0 - 180.0)
}

/// The place a fraction of the way along the great circle from one place to another.
pub fn along(a: LatLon, b: LatLon, fraction: f64) -> LatLon {
    travel(a, bearing_deg(a, b), distance_nm(a, b) * fraction)
}

/// A box of latitude and longitude, for asking a source for what lies in an area.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub south: f64,
    pub north: f64,
    pub west: f64,
    pub east: f64,
}

impl Bounds {
    /// The box round two places and a margin either side, nautical miles. A pair either
    /// side of the date line gets a box that crosses it the short way: `west` greater
    /// than `east`.
    pub fn around(a: LatLon, b: LatLon, margin_nm: f64) -> Bounds {
        let m = margin_nm / 60.0;
        let cos = ((a.0 + b.0) / 2.0).to_radians().cos().max(0.2);
        let (south, north) = ((a.0.min(b.0) - m).max(-90.0), (a.0.max(b.0) + m).min(90.0));
        let (lo, hi) = (a.1.min(b.1), a.1.max(b.1));
        if hi - lo <= 180.0 {
            Bounds { south, north, west: lo - m / cos, east: hi + m / cos }
        } else {
            Bounds { south, north, west: hi - m / cos, east: lo + m / cos }
        }
    }

    pub fn contains(&self, p: LatLon) -> bool {
        let lon_ok = if self.west <= self.east { self.west <= p.1 && p.1 <= self.east } else { p.1 >= self.west || p.1 <= self.east };
        self.south <= p.0 && p.0 <= self.north && lon_ok
    }
}

// ---------------------------------------------------------------------------------
// Airports.
// ---------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Airport {
    pub icao: String,
    #[serde(default)]
    pub name: String,
    pub pos: LatLon,
    #[serde(default)]
    pub elevation_ft: f64,
}

// ---------------------------------------------------------------------------------
// The air.
// ---------------------------------------------------------------------------------

/// The temperature of the standard atmosphere at a pressure altitude, Celsius.
pub fn isa_temp_c(alt_ft: f64) -> f64 {
    if alt_ft <= 36_089.0 {
        15.0 - 1.98 * alt_ft / 1000.0
    } else {
        -56.5
    }
}

/// The air at one place and height: the wind, and the temperature.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Air {
    /// Where the wind blows from, degrees true.
    pub wind_from_deg: f64,
    pub wind_kt: f64,
    pub temp_c: f64,
}

impl Air {
    /// Still air at the standard temperature.
    pub fn standard(alt_ft: f64) -> Air {
        Air { wind_from_deg: 0.0, wind_kt: 0.0, temp_c: isa_temp_c(alt_ft) }
    }

    /// How far the temperature is from standard at that height.
    pub fn isa_dev(&self, alt_ft: f64) -> f64 {
        self.temp_c - isa_temp_c(alt_ft)
    }

    /// The wind along a track (positive behind) and across it (positive from the left,
    /// blowing to the right), knots.
    pub fn components(&self, track_true_deg: f64) -> (f64, f64) {
        let towards = (self.wind_from_deg + 180.0).to_radians();
        let (n, e) = (self.wind_kt * towards.cos(), self.wind_kt * towards.sin());
        let t = track_true_deg.to_radians();
        (n * t.cos() + e * t.sin(), -n * t.sin() + e * t.cos())
    }

    /// The ground speed along a track at a true airspeed: the wind triangle.
    pub fn ground_speed(&self, track_true_deg: f64, tas_kt: f64) -> f64 {
        let (along, across) = self.components(track_true_deg);
        let ratio = (across / tas_kt.max(1.0)).clamp(-0.95, 0.95);
        (tas_kt * (1.0 - ratio * ratio).sqrt() + along).max(30.0)
    }
}

/// Anything that can say what the air is doing: a forecast model, a handful of reported
/// winds, still air.
pub trait WindField: Send + Sync {
    /// The air at a place, a pressure altitude and a time. Still standard air where the
    /// source knows nothing.
    fn air(&self, at: LatLon, alt_ft: f64, when: DateTime<Utc>) -> Air;
}

/// Still air at the standard temperature everywhere.
pub struct StillAir;

impl WindField for StillAir {
    fn air(&self, _at: LatLon, alt_ft: f64, _when: DateTime<Utc>) -> Air {
        Air::standard(alt_ft)
    }
}

/// A wind aloft: where it was measured or forecast, at what altitude, the direction it
/// blows from (true) and how strong.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wind {
    pub lat: f64,
    pub lon: f64,
    pub alt_ft: f64,
    pub from_deg: f64,
    pub speed_kt: f64,
}

// ---------------------------------------------------------------------------------
// Hazards.
// ---------------------------------------------------------------------------------

/// What to do about a hazard: keep out of it, or go through it at a price.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HazardKind {
    /// Convection, volcanic ash, an active restricted area, a conflict zone: no segment
    /// may cross it.
    Avoid,
    /// Turbulence, icing, a danger area: a segment may cross it, and its time is
    /// multiplied by this.
    Penalise(f64),
}

/// A hazard: a shape on the ground, the band of altitudes it fills, and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hazard {
    #[serde(default)]
    pub name: String,
    /// The outline, latitude and longitude pairs, in order round it.
    pub polygon: Vec<LatLon>,
    #[serde(default)]
    pub base_ft: f64,
    #[serde(default = "sky")]
    pub top_ft: f64,
    pub kind: HazardKind,
    /// When it starts and stops mattering; always, where these are empty.
    #[serde(default)]
    pub active_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub active_to: Option<DateTime<Utc>>,
    /// Where it came from, for the flight plan to say: "SIGMET", "NOTAM A1234/26",
    /// "EASA CZIB-2022-01".
    #[serde(default)]
    pub source: String,
}

pub fn sky() -> f64 {
    99_999.0
}

impl Hazard {
    /// A hazard with no time limits and no source, the whole height of the sky.
    pub fn new(name: impl Into<String>, polygon: Vec<LatLon>, kind: HazardKind) -> Hazard {
        Hazard { name: name.into(), polygon, base_ft: 0.0, top_ft: sky(), kind, active_from: None, active_to: None, source: String::new() }
    }

    pub fn active_at(&self, when: DateTime<Utc>) -> bool {
        self.active_from.is_none_or(|t| t <= when) && self.active_to.is_none_or(|t| when <= t)
    }

    pub fn fills(&self, alt_ft: f64) -> bool {
        self.base_ft <= alt_ft && alt_ft <= self.top_ft
    }
}

// ---------------------------------------------------------------------------------
// Rules of the network.
// ---------------------------------------------------------------------------------

/// A segment the route search is thinking of flying, and when.
#[derive(Debug, Clone, Copy)]
pub struct EdgeQuery<'a> {
    pub from: &'a str,
    pub from_pos: LatLon,
    pub to: &'a str,
    pub to_pos: LatLon,
    /// The airway, "DCT", or a SID, STAR or oceanic track name.
    pub airway: &'a str,
    pub level_ft: f64,
    /// The estimated time at `from`.
    pub when: DateTime<Utc>,
    pub origin: &'a str,
    pub destination: &'a str,
}

/// What a rule says about a segment.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Allow,
    /// Not to be flown; the rule's own reference and why.
    Forbid(String),
    /// Flown at a price: its time is multiplied by this.
    Penalise(f64),
}

/// A rule the search asks about every segment before flying it: route availability,
/// one-way conditional routes, the reach of an ETOPS rule.
pub trait EdgeRule: Send + Sync {
    fn name(&self) -> &str;
    fn check(&self, q: &EdgeQuery) -> Verdict;
}

/// Where a filed route breaks a rule that can only be judged on the whole of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Violation {
    /// The rule's own reference, such as a RAD identifier.
    pub rule: String,
    /// The fix or airway it concerns, where it concerns one.
    pub at: Option<String>,
    pub message: String,
}

/// A rule judged on a whole route: a RAD restriction that depends on where the flight
/// came from or goes next.
pub trait RouteRule: Send + Sync {
    fn name(&self) -> &str;
    fn check_route(&self, route: &FiledRoute) -> Vec<Violation>;
}

// ---------------------------------------------------------------------------------
// Cruise levels.
// ---------------------------------------------------------------------------------

/// How a country sets levels against direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LevelScheme {
    /// ICAO: tracks 000–179 magnetic fly odd thousands, 180–359 even.
    EastOdd,
    /// Italy, France, Portugal and a few others: 090–269 odd, 270–089 even.
    SouthOdd,
}

/// Whether a flight level suits a magnetic track under a scheme. With RVSM the levels
/// run every thousand feet to FL410; above it, and without RVSM above FL290, they run
/// every four thousand.
pub fn level_suits(track_mag_deg: f64, level_ft: f64, rvsm: bool, scheme: LevelScheme) -> bool {
    let fl = (level_ft / 100.0).round() as i64;
    let odd_way = match scheme {
        LevelScheme::EastOdd => (track_mag_deg.rem_euclid(360.0)) < 180.0,
        LevelScheme::SouthOdd => {
            let t = track_mag_deg.rem_euclid(360.0);
            (90.0..270.0).contains(&t)
        }
    };
    if fl % 10 != 0 {
        return false;
    }
    let thousands = fl / 10;
    let wide = if rvsm { fl > 410 } else { fl > 290 };
    if !wide {
        return (thousands % 2 == 1) == odd_way;
    }
    // Above the band, every four thousand: with RVSM 450, 490 … one way and 430, 470 …
    // the other; without it 330, 370 … and 310, 350 ….
    let (odd_from, even_from) = if rvsm { (450, 430) } else { (290, 310) };
    if odd_way { (fl - odd_from).rem_euclid(40) == 0 } else { (fl - even_from).rem_euclid(40) == 0 }
}

/// The levels a track may be flown at, from the lowest to the ceiling, highest first.
pub fn levels_for(track_mag_deg: f64, floor_ft: f64, ceiling_ft: f64, rvsm: bool, scheme: LevelScheme) -> Vec<f64> {
    let mut out = Vec::new();
    let mut fl = (ceiling_ft / 1000.0).floor() as i64 * 10;
    while fl as f64 * 100.0 >= floor_ft {
        if level_suits(track_mag_deg, fl as f64 * 100.0, rvsm, scheme) {
            out.push(fl as f64 * 100.0);
        }
        fl -= 10;
    }
    out
}

// ---------------------------------------------------------------------------------
// The filed route.
// ---------------------------------------------------------------------------------

/// What part of the flight a point belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointKind {
    Airport,
    Sid,
    Enroute,
    /// A point of an oceanic track.
    Track,
    Star,
    Approach,
}

/// A point of the route, and how the route goes on from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Waypoint {
    pub ident: String,
    pub pos: LatLon,
    /// What the leg *out of* this point is flown on: an airway, "DCT", the SID or STAR
    /// name, or an oceanic track. Empty on the last point.
    pub via: String,
    pub kind: PointKind,
    /// A published altitude constraint at the point, where there is one.
    #[serde(default)]
    pub alt_min_ft: Option<f64>,
    #[serde(default)]
    pub alt_max_ft: Option<f64>,
    #[serde(default)]
    pub speed_max_kt: Option<f64>,
    /// The magnetic variation there, degrees east positive, for printing magnetic tracks.
    #[serde(default)]
    pub mag_var_deg: f64,
}

impl Waypoint {
    pub fn new(ident: impl Into<String>, pos: LatLon, via: impl Into<String>, kind: PointKind) -> Waypoint {
        Waypoint { ident: ident.into(), pos, via: via.into(), kind, alt_min_ft: None, alt_max_ft: None, speed_max_kt: None, mag_var_deg: 0.0 }
    }
}

/// A route as it is filed: the airports, the procedures at each end, every point in
/// between, and the level planned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FiledRoute {
    pub origin: Airport,
    pub destination: Airport,
    #[serde(default)]
    pub dep_runway: Option<String>,
    #[serde(default)]
    pub sid: Option<String>,
    #[serde(default)]
    pub sid_transition: Option<String>,
    #[serde(default)]
    pub star: Option<String>,
    #[serde(default)]
    pub star_transition: Option<String>,
    #[serde(default)]
    pub arr_runway: Option<String>,
    #[serde(default)]
    pub approach: Option<String>,
    /// Every point, the origin first and the destination last.
    pub points: Vec<Waypoint>,
    /// The initial cruise level; step climbs are for `perf` to find.
    pub cruise_ft: f64,
    pub off_block: DateTime<Utc>,
}

impl FiledRoute {
    /// The distance along every point, nautical miles.
    pub fn distance_nm(&self) -> f64 {
        self.points.windows(2).map(|w| distance_nm(w[0].pos, w[1].pos)).sum()
    }

    /// The route as item 15 of a flight plan files it, without speed and level: the SID,
    /// then airways and the fixes they are joined and left at, then the STAR. Consecutive
    /// legs on one airway collapse into one.
    pub fn route_string(&self) -> String {
        let pts: Vec<&Waypoint> = self.points.iter().filter(|p| matches!(p.kind, PointKind::Enroute | PointKind::Track)).collect();
        let mut out: Vec<String> = Vec::new();
        if let Some(sid) = &self.sid {
            out.push(sid.clone());
        }
        let mut i = 0;
        while i < pts.len() {
            if out.is_empty() || out.last().map(|s| s.as_str()) != Some(pts[i].ident.as_str()) {
                out.push(pts[i].ident.clone());
            }
            let via = &pts[i].via;
            if i + 1 >= pts.len() || via.is_empty() {
                break;
            }
            let mut j = i;
            while j + 1 < pts.len() && &pts[j + 1].via == via && via != "DCT" && j + 2 < pts.len() {
                j += 1;
            }
            out.push(via.clone());
            i = j + 1;
        }
        if let Some(star) = &self.star {
            out.push(star.clone());
        }
        // An airway never ends the string: the last airway's exit is the last point.
        while out.last().is_some_and(|s| s == "DCT") {
            out.pop();
        }
        out.join(" ")
    }
}

/// What the route search is asked for.
pub struct RouteRequest<'a> {
    pub origin: &'a Airport,
    pub destination: &'a Airport,
    pub cruise_ft: f64,
    pub tas_kt: f64,
    pub ceiling_ft: f64,
    pub off_block: DateTime<Utc>,
    pub air: &'a dyn WindField,
    pub hazards: &'a [Hazard],
    pub edge_rules: &'a [&'a dyn EdgeRule],
    pub route_rules: &'a [&'a dyn RouteRule],
    /// The runway in use at each end, if it is known; otherwise chosen from the surface
    /// wind, and failing that the longest.
    pub dep_runway: Option<String>,
    pub arr_runway: Option<String>,
    /// The surface wind at each end, from and knots, from the latest report.
    pub origin_wind: Option<(f64, f64)>,
    pub destination_wind: Option<(f64, f64)>,
    pub rvsm: bool,
    /// Flight information regions not to enter, by code.
    pub avoid_firs: &'a [String],
    /// Whether directs may replace airways where the airspace allows it.
    pub free_route: bool,
}

// ---------------------------------------------------------------------------------
// The aircraft and how it is to be flown.
// ---------------------------------------------------------------------------------

/// What `perf` knows about a type: its limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AircraftSpec {
    /// ICAO type designator: A20N, B77W, B738.
    pub icao_type: String,
    pub name: String,
    pub engine: String,
    pub engines: u8,
    pub oew_kg: f64,
    pub mzfw_kg: f64,
    pub mtow_kg: f64,
    pub mlw_kg: f64,
    pub max_fuel_kg: f64,
    pub ceiling_ft: f64,
    pub mmo: f64,
    pub vmo_kt: f64,
    /// The usual cruise Mach, for when no cost index is given.
    pub cruise_mach: f64,
    /// The ETOPS/EDTO approval, minutes, for a twin that has one.
    pub etops_minutes: Option<u32>,
    /// The true airspeed flown with one engine out, for the ETOPS circles.
    pub one_engine_tas_kt: Option<f64>,
}

/// How the fuel is to be planned. The defaults are the EASA rules for a jet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FuelPolicy {
    pub taxi_kg: f64,
    /// Contingency as a share of trip fuel, percent.
    pub contingency_pct: f64,
    /// And never less than this many minutes of holding.
    pub contingency_min_minutes: f64,
    /// Final reserve, minutes of holding at 1,500 ft over the alternate.
    pub final_reserve_min: f64,
    pub extra_kg: f64,
    /// Carry fuel for the return where it is dearer at the destination.
    pub tanker: bool,
    /// Price per kilogram at each end, for tankering and the cost index.
    pub price_origin: Option<f64>,
    pub price_destination: Option<f64>,
}

impl Default for FuelPolicy {
    fn default() -> Self {
        FuelPolicy { taxi_kg: 200.0, contingency_pct: 5.0, contingency_min_minutes: 5.0, final_reserve_min: 30.0, extra_kg: 0.0, tanker: false, price_origin: None, price_destination: None }
    }
}

/// How the cruise is to be flown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CruisePolicy {
    /// Cost index, where the speed is to be chosen by it.
    pub cost_index: Option<f64>,
    /// A fixed Mach instead.
    pub mach: Option<f64>,
    /// Step climbs, and how big a step is.
    pub step_climbs: bool,
    pub step_ft: f64,
}

impl Default for CruisePolicy {
    fn default() -> Self {
        CruisePolicy { cost_index: None, mach: None, step_climbs: true, step_ft: 2000.0 }
    }
}

/// What `perf` is asked to fly.
pub struct PerfRequest<'a> {
    pub spec: &'a AircraftSpec,
    pub route: &'a FiledRoute,
    /// The route on from the destination to the alternate, if one is planned.
    pub alternate: Option<&'a FiledRoute>,
    pub air: &'a dyn WindField,
    pub payload_kg: f64,
    pub cruise: CruisePolicy,
    pub fuel: FuelPolicy,
    /// Airports for the equal-time points, where the flight is oceanic or under ETOPS.
    pub etp_airports: &'a [Airport],
    pub scheme: LevelScheme,
    pub rvsm: bool,
}

// ---------------------------------------------------------------------------------
// What `perf` gives back.
// ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileKind {
    Waypoint,
    TopOfClimb,
    TopOfDescent,
    StepClimb,
    EqualTime,
    NoReturn,
}

/// One line of the navigation log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfilePoint {
    pub ident: String,
    pub kind: ProfileKind,
    pub pos: LatLon,
    /// What the leg into this point was flown on.
    pub via: String,
    pub alt_ft: f64,
    /// From the origin, cumulative.
    pub dist_nm: f64,
    pub time_min: f64,
    pub fuel_used_kg: f64,
    pub fuel_remaining_kg: f64,
    pub gross_kg: f64,
    /// The leg into this point.
    pub track_true_deg: f64,
    pub tas_kt: f64,
    pub gs_kt: f64,
    pub mach: f64,
    pub air: Air,
    /// The grid minimum off-route altitude there, if known.
    pub mora_ft: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FuelBreakdown {
    pub taxi_kg: f64,
    pub trip_kg: f64,
    pub contingency_kg: f64,
    pub alternate_kg: f64,
    pub final_reserve_kg: f64,
    pub extra_kg: f64,
    pub tanker_kg: f64,
    /// Everything but taxi: what must be on board at take-off.
    pub takeoff_kg: f64,
    pub block_kg: f64,
    pub landing_kg: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Weights {
    pub oew_kg: f64,
    pub payload_kg: f64,
    pub zfw_kg: f64,
    pub tow_kg: f64,
    pub lw_kg: f64,
    pub max_zfw_kg: f64,
    pub max_tow_kg: f64,
    pub max_lw_kg: f64,
    /// The limit that bit, if one did: payload was cut to keep under it.
    pub limited_by: Option<String>,
}

/// An equal-time point between two airports, for an engine failure or a depressurisation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqualTimePoint {
    pub between: (String, String),
    pub pos: LatLon,
    pub dist_nm: f64,
    pub time_min: f64,
    /// Fuel needed from the point to either airport in the worst of the cases planned.
    pub fuel_needed_kg: f64,
}

/// The flight to the alternate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlternatePlan {
    pub icao: String,
    pub dist_nm: f64,
    pub time_min: f64,
    pub fuel_kg: f64,
    pub cruise_ft: f64,
}

/// The route flown: the profile, the fuel and the weights.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PerfPlan {
    pub profile: Vec<ProfilePoint>,
    pub fuel: FuelBreakdown,
    pub weights: Weights,
    /// Where each step climb starts, and to what level.
    pub step_climbs: Vec<(String, f64)>,
    pub avg_wind_kt: f64,
    pub avg_isa_dev: f64,
    pub equal_time_points: Vec<EqualTimePoint>,
    pub no_return: Option<ProfilePoint>,
    pub alternate: Option<AlternatePlan>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------------
// Weather reports.
// ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Metar {
    pub station: String,
    pub observed: Option<DateTime<Utc>>,
    /// Where the wind is from, true; `None` when it is variable.
    pub wind_from_deg: Option<f64>,
    pub wind_kt: f64,
    pub gust_kt: Option<f64>,
    pub visibility_m: Option<f64>,
    /// The lowest broken or overcast layer, feet above the field.
    pub ceiling_ft: Option<f64>,
    pub temp_c: Option<f64>,
    pub dewpoint_c: Option<f64>,
    pub qnh_hpa: Option<f64>,
    pub raw: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TafChange {
    Base,
    From,
    Becoming,
    Tempo,
    Prob(u8),
    ProbTempo(u8),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TafPeriod {
    pub change: TafChange,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub wind_from_deg: Option<f64>,
    pub wind_kt: Option<f64>,
    pub gust_kt: Option<f64>,
    pub visibility_m: Option<f64>,
    pub ceiling_ft: Option<f64>,
    /// Weather groups as written: "TSRA", "-SN", "FG".
    pub weather: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Taf {
    pub station: String,
    pub issued: Option<DateTime<Utc>>,
    pub periods: Vec<TafPeriod>,
    pub raw: String,
}

/// An airport the destination could be diverted to, and why it was chosen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alternate {
    pub airport: Airport,
    pub distance_nm: f64,
    pub reason: String,
}

// ---------------------------------------------------------------------------------
// The whole of it.
// ---------------------------------------------------------------------------------

/// Everything the flight plan prints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dispatch {
    pub route: FiledRoute,
    pub perf: PerfPlan,
    pub spec: AircraftSpec,
    pub alternate: Option<FiledRoute>,
    pub origin_metar: Option<Metar>,
    pub destination_metar: Option<Metar>,
    pub origin_taf: Option<Taf>,
    pub destination_taf: Option<Taf>,
    pub alternate_taf: Option<Taf>,
    /// Every hazard the route was planned round, by name and source.
    pub hazards: Vec<String>,
    pub violations: Vec<Violation>,
    pub generated: DateTime<Utc>,
    /// The AIRAC cycle of the navigation data, where it is known.
    pub airac: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_the_semicircle() {
        assert!(level_suits(90.0, 35_000.0, true, LevelScheme::EastOdd));
        assert!(!level_suits(90.0, 36_000.0, true, LevelScheme::EastOdd));
        assert!(level_suits(270.0, 36_000.0, true, LevelScheme::EastOdd));
        assert!(level_suits(90.0, 41_000.0, true, LevelScheme::EastOdd));
        assert!(level_suits(270.0, 43_000.0, true, LevelScheme::EastOdd));
        assert!(level_suits(90.0, 45_000.0, true, LevelScheme::EastOdd));
        assert!(!level_suits(90.0, 35_500.0, true, LevelScheme::EastOdd));
        assert!(level_suits(180.0, 35_000.0, true, LevelScheme::SouthOdd));
        assert!(level_suits(0.0, 34_000.0, true, LevelScheme::SouthOdd));
    }

    #[test]
    fn travel_and_back() {
        let a = (38.77, -9.13);
        let b = travel(a, 225.0, 520.0);
        assert!((distance_nm(a, b) - 520.0).abs() < 0.5);
        assert!((bearing_deg(a, b) - 225.0).abs() < 0.5);
    }

    #[test]
    fn the_wind_triangle() {
        let head = Air { wind_from_deg: 90.0, wind_kt: 50.0, temp_c: -50.0 };
        assert!((head.ground_speed(90.0, 450.0) - 400.0).abs() < 0.5);
        assert!((head.ground_speed(270.0, 450.0) - 500.0).abs() < 0.5);
    }
}
