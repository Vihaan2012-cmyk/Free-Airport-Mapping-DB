//! The operational flight plan, printed the way an airline's dispatch system prints one.
//!
//! The layout is LIDO's, set to 68 columns, because that is the one SimBrief prints and the
//! one the people who read these already know how to read: the `[ OFP ]` banner and the
//! header block, the planned fuel table, the release a dispatcher signs, the times and the
//! weights against their limits, then the flight log — three rows to a fix, every field in a
//! column of its own — and the weather at each end. A figure put somewhere else on the page
//! costs a reader time for nothing, so the columns here are the measured columns of a real
//! plan rather than an approximation of one.
//!
//! `pdf::write` typesets exactly this text, monospaced, across as many A4 pages as it takes:
//! there is one source of the plan's content, and only one place it is laid out.

use crate::dispatch::{Dispatch, PointKind, ProfileKind, TafChange, Waypoint};
use crate::ofp::DispatchOptions;
use crate::route::airspace::fir_crossings;
use chrono::{DateTime, Utc};
use std::fmt::Write as _;

/// The page is 68 columns wide. Every rule, every centred heading and every right-aligned
/// figure on it is measured from this one number.
const WIDTH: usize = 68;
const RULE: &str = "--------------------------------------------------------------------";

/// The column the header block's right-hand figures begin at.
const RIGHT_COLUMN: usize = 50;

// ---------------------------------------------------------------------------------------
// Placing fields in columns
// ---------------------------------------------------------------------------------------

/// A line built by placing fields at fixed columns, which is how a flight log's three rows
/// line up with one another and with their headings: each field owns a column and a width,
/// and nothing that overruns its width is allowed to push the next field along — that is what
/// turns one long name into a whole row of figures read against the wrong headings.
struct Row(Vec<char>);

impl Row {
    fn new() -> Self {
        Row(Vec::new())
    }

    fn put(&mut self, col: usize, text: &str) -> &mut Self {
        if self.0.len() < col {
            self.0.resize(col, ' ');
        }
        for (i, c) in text.chars().enumerate() {
            match self.0.get_mut(col + i) {
                Some(slot) => *slot = c,
                None => self.0.push(c),
            }
        }
        self
    }

    /// Left-aligned at `col`, cut to `width`.
    fn left(&mut self, col: usize, width: usize, text: &str) -> &mut Self {
        let cut: String = text.chars().take(width).collect();
        self.put(col, &cut)
    }

    /// Right-aligned within `width` from `col`, the shape every figure on this page takes.
    ///
    /// A value too wide for its column fills it with asterisks rather than losing digits off
    /// one end: a truncated number reads as a real one, and on this page that is the
    /// difference between a fuel figure and a wrong fuel figure.
    fn right(&mut self, col: usize, width: usize, text: &str) -> &mut Self {
        let n = text.chars().count();
        if n > width {
            return self.put(col, &"*".repeat(width));
        }
        self.put(col + width - n, text)
    }

    fn line(&self) -> String {
        let s: String = self.0.iter().collect();
        s.trim_end().to_string()
    }
}

/// A heading centred on the page over its own underline, as every block heading on a LIDO
/// plan is set.
fn centred(s: &mut String, title: &str) {
    let n = title.chars().count();
    let pad = (WIDTH + 1).saturating_sub(n) / 2;
    let _ = writeln!(s, "{:pad$}{title}", "");
    let _ = writeln!(s, "{:pad$}{}", "", "-".repeat(n));
}

// ---------------------------------------------------------------------------------------
// Figures, in the forms an operational plan writes them
// ---------------------------------------------------------------------------------------

fn hdg(deg: f64) -> String {
    format!("{:03.0}", deg.rem_euclid(360.0))
}

fn hm(dt: DateTime<Utc>) -> String {
    dt.format("%H%M").to_string()
}

fn ddmon(dt: DateTime<Utc>) -> String {
    dt.format("%d%b%y").to_string().to_uppercase()
}

fn fmt_hm(minutes: f64) -> String {
    let total = minutes.round().max(0.0) as i64;
    format!("{:02}{:02}", total / 60, total % 60)
}

/// A signed figure the way an operational plan writes one: a letter for the sign, so a minus
/// can never be read as one of the rules that cross the page, and the magnitude zero-padded.
/// A seventeen-degree ISA deviation is `P017`; two knots of headwind are `M002`.
fn signed(v: f64, digits: usize) -> String {
    let letter = if v < 0.0 { 'M' } else { 'P' };
    format!("{letter}{:0w$.0}", v.abs(), w = digits)
}

/// An outside air temperature as the flight log's own column writes it: two digits when it is
/// above freezing, `M` and two digits when it is below.
fn oat(c: f64) -> String {
    if c < 0.0 {
        format!("M{:02.0}", c.abs())
    } else {
        format!("{c:02.0}")
    }
}

/// Degrees and minutes, rolling 59.95 minutes up into the next degree rather than printing a
/// sixtieth minute.
fn degrees_minutes(v: f64) -> (f64, f64) {
    let a = v.abs();
    let mut d = a.floor();
    let mut m = (a - d) * 60.0;
    if m >= 59.95 {
        m = 0.0;
        d += 1.0;
    }
    (d, m)
}

/// A latitude the way a flight log prints one: `N2741.8`.
fn lat_text(lat: f64) -> String {
    let (d, m) = degrees_minutes(lat);
    format!("{}{:02.0}{:04.1}", if lat < 0.0 { 'S' } else { 'N' }, d, m)
}

/// A longitude, three degrees wide: `E08521.6`.
fn lon_text(lon: f64) -> String {
    let (d, m) = degrees_minutes(lon);
    format!("{}{:03.0}{:04.1}", if lon < 0.0 { 'W' } else { 'E' }, d, m)
}

/// A Mach number without its leading nought, which is the only way three columns hold one:
/// `.62`.
fn mach_text(m: f64) -> String {
    if m <= 0.0 {
        return String::new();
    }
    let text = format!("{m:.2}");
    text.strip_prefix('0').unwrap_or(&text).to_string()
}

/// Fuel in tonnes to a tenth, which is how the flight log carries it.
fn tonnes(kg: f64) -> String {
    format!("{:.1}", kg / 1000.0)
}

/// An altitude in hundreds of feet, which is what the log's `FL` column holds at every height
/// — a fix at 5,900 ft reads `059` there, the same way a cruise level reads `210`.
fn hundreds(alt_ft: f64) -> String {
    format!("{:03.0}", (alt_ft.max(0.0) / 100.0).round())
}

/// The altitude below which a level is read back as feet rather than as a flight level: the
/// departure's own transition altitude — what a climb is cleared against — and the arrival's
/// own transition level — what a descent is, which sits at or a little above the transition
/// altitude — each from `navdata::airport_info` where the navigation database has it
/// published. 18,000 ft (the FAA's own, and the commonest figure a dispatch system falls back
/// on where a state publishes none) stands in for whichever of the two it does not have.
const DEFAULT_TRANSITION_FT: f64 = 18_000.0;

fn climb_transition_ft(icao: &str) -> f64 {
    crate::sources::navdata::airport_info(icao).and_then(|i| i.transition_altitude_ft).unwrap_or(DEFAULT_TRANSITION_FT)
}

fn descent_transition_ft(icao: &str) -> f64 {
    let info = crate::sources::navdata::airport_info(icao);
    info.as_ref()
        .and_then(|i| i.transition_level_ft)
        .or_else(|| info.and_then(|i| i.transition_altitude_ft))
        .unwrap_or(DEFAULT_TRANSITION_FT)
}

/// The level actually climbed to, from the flight log's own top of climb, rather than the
/// route's filed `cruise_ft`: the route search picks a level around the aircraft's usual
/// cruise with no notion of how short the trip is, and it is `perf::plan`'s distance cap, not
/// the route search, that has the last word on a short sector — the two can disagree, and
/// what was actually flown is the one worth printing.
fn flown_cruise_ft(d: &Dispatch) -> f64 {
    d.perf.profile.iter().find(|p| p.kind == ProfileKind::TopOfClimb).map(|p| p.alt_ft).unwrap_or(d.route.cruise_ft)
}

fn total_minutes(d: &Dispatch) -> f64 {
    d.perf.profile.last().map(|p| p.time_min).unwrap_or(0.0)
}

/// The mean fuel flow over the trip, in kilograms a minute: the rate every quantity of fuel on
/// this page was worked out at, and so the rate each of them has to be turned back into a time
/// at if the two columns of the fuel table are to agree with one another.
fn burn_per_minute(d: &Dispatch) -> f64 {
    let minutes = total_minutes(d);
    if minutes > 1.0 { d.perf.fuel.trip_kg / minutes } else { 0.0 }
}

/// How long a quantity of fuel lasts at the trip's own mean flow. Nothing is printed where
/// there is no flow to divide by: an endurance worked out from a zero burn is not a figure.
fn endurance(d: &Dispatch, kg: f64) -> Option<f64> {
    let rate = burn_per_minute(d);
    if rate > 0.0 { Some(kg / rate) } else { None }
}

/// The air distance: how far the aeroplane flies through the air, which is the ground distance
/// less what the wind carried it. It is what a fuel figure is really a function of, and it is
/// the sum over every leg of the true airspeed by the time flown.
fn air_distance_nm(d: &Dispatch) -> f64 {
    let mut prev = 0.0;
    let mut nm = 0.0;
    for p in &d.perf.profile {
        nm += p.tas_kt * (p.time_min - prev) / 60.0;
        prev = p.time_min;
    }
    nm
}

/// The mean wind over the route as a direction and a speed, by averaging the wind vectors
/// rather than the directions: the mean of 350 and 010 degrees is north, not south.
fn average_wind(d: &Dispatch) -> (f64, f64) {
    let mut north = 0.0;
    let mut east = 0.0;
    let mut n = 0.0;
    for p in &d.perf.profile {
        let r = p.air.wind_from_deg.to_radians();
        north += p.air.wind_kt * r.cos();
        east += p.air.wind_kt * r.sin();
        n += 1.0;
    }
    if n == 0.0 {
        return (0.0, 0.0);
    }
    let (north, east) = (north / n, east / n);
    (east.atan2(north).to_degrees().rem_euclid(360.0), (north * north + east * east).sqrt())
}

/// The highest grid minimum off-route altitude on the route, and the fix it is at: the one
/// figure on the flight log a crew wants before they want any of the others.
fn critical_mora(d: &Dispatch) -> Option<(f64, String)> {
    d.perf.profile.iter().filter_map(|p| p.mora_ft.map(|m| (m, p.ident.clone()))).max_by(|a, b| a.0.total_cmp(&b.0))
}

/// The four times a flight is measured by: off blocks, airborne, on the ground, on blocks.
///
/// The taxi figures are the ones the fuel was planned on, because the page cannot say one thing
/// about taxi in its fuel table and another in its times.
fn milestones(d: &Dispatch, opts: &DispatchOptions) -> (DateTime<Utc>, DateTime<Utc>, DateTime<Utc>, DateTime<Utc>) {
    let out = d.route.off_block;
    let off = out + chrono::Duration::minutes(opts.taxi_out_min.max(0.0).round() as i64);
    let on = off + chrono::Duration::minutes(total_minutes(d).round() as i64);
    let inn = on + chrono::Duration::minutes(opts.taxi_in_min.max(0.0).round() as i64);
    (out, off, on, inn)
}

// ---------------------------------------------------------------------------------------
// The plan
// ---------------------------------------------------------------------------------------

/// The whole plan, as one string, ready to print or to lay out on a PDF page a line at a time.
pub fn render(d: &Dispatch, opts: &DispatchOptions) -> String {
    let mut s = String::new();
    header(&mut s, d, opts);
    dispatch_remarks(&mut s, d);
    planned_fuel(&mut s, d, opts.taxi_out_min.max(0.0));
    fmc_info(&mut s, d);
    tankering(&mut s, d);
    self_briefing(&mut s);
    alternate_route(&mut s, d);
    routing(&mut s, d, opts);
    departure_clearance(&mut s);
    times(&mut s, d, opts);
    weights(&mut s, d, opts);
    flight_log(&mut s, d);
    step_climbs(&mut s, d);
    equal_time_points(&mut s, d);
    point_of_no_return(&mut s, d);
    fir_section(&mut s, d, &fir_crossings(&d.route));
    atc_flight_plan(&mut s, d, opts);
    airport_weather(&mut s, d);
    hazards_and_rules(&mut s, d);
    violations(&mut s, d);
    notes(&mut s, d);
    footer(&mut s, d);
    s
}

/// The banner and the header block: the flight, the aeroplane, the two airports and their
/// times down the left, and the figures that describe the whole trip — the distances, the mean
/// wind and temperature, the mean fuel flow — down the right, each right-aligned to the page's
/// own edge.
fn header(s: &mut String, d: &Dispatch, opts: &DispatchOptions) {
    let flight = opts.flight_number.clone().unwrap_or_else(|| "--------".to_string());
    let reg = opts.registration.clone().unwrap_or_else(|| "------".to_string());
    let w = &d.perf.weights;
    let (wind_deg, wind_kt) = average_wind(d);
    let air_min = total_minutes(d);
    let (out, off, on, inn) = milestones(d, opts);

    let _ = writeln!(s);
    let _ = writeln!(s, "[ OFP ]");
    let _ = writeln!(s, "{RULE}");

    let mut r = Row::new();
    r.left(0, 9, &flight);
    r.put(10, &d.generated.format("%d%b%Y").to_string().to_uppercase());
    r.put(23, &format!("{}-{}", d.route.origin.icao, d.route.destination.icao));
    r.left(35, 4, &d.spec.icao_type);
    r.left(40, 7, &reg);
    r.put(48, "RELEASE");
    r.right(56, 4, &hm(d.generated));
    r.put(61, &ddmon(d.generated));
    let _ = writeln!(s, "{}", r.line());

    let mut r = Row::new();
    r.put(0, "OFP 1");
    r.left(13, WIDTH - 13, &format!("{}-{}", d.route.origin.name, d.route.destination.name));
    let _ = writeln!(s, "{}", r.line());
    let _ = writeln!(s);

    // The left column: who is flying what, between which two airports, and when.
    let mut left = Vec::new();

    let mut r = Row::new();
    r.put(2, "ATC C/S");
    r.left(12, 12, &flight);
    r.left(25, 10, &d.route.origin.icao);
    r.left(36, 12, &d.route.destination.icao);
    left.push(r);

    let mut r = Row::new();
    r.put(0, &d.generated.format("%d%b%Y").to_string().to_uppercase());
    r.left(12, 12, &reg);
    r.put(25, &format!("{}/{}", hm(out), hm(off)));
    r.put(36, &format!("{}/{}", hm(on), hm(inn)));
    left.push(r);

    let mut r = Row::new();
    r.left(0, 34, &format!("{} / {}", d.spec.name, d.spec.engine));
    r.put(36, "STA");
    r.right(40, 5, &hm(inn));
    left.push(r);

    let mut r = Row::new();
    r.put(25, "CTOT:....");
    left.push(r);

    left.push(Row::new());

    let mut r = Row::new();
    r.put(0, "MAXIMUM");
    r.put(11, "TOW");
    r.right(15, 6, &format!("{:.0}", w.max_tow_kg));
    r.put(23, "LAW");
    r.right(27, 6, &format!("{:.0}", w.max_lw_kg));
    r.put(35, "ZFW");
    r.right(39, 6, &format!("{:.0}", w.max_zfw_kg));
    left.push(r);

    let mut r = Row::new();
    r.put(0, "ESTIMATED");
    r.put(11, "TOW");
    r.right(15, 6, &format!("{:.0}", w.tow_kg));
    r.put(23, "LAW");
    r.right(27, 6, &format!("{:.0}", w.lw_kg));
    r.put(35, "ZFW");
    r.right(39, 6, &format!("{:.0}", w.zfw_kg));
    left.push(r);

    left.push(Row::new());
    left.push(Row::new());

    let mut r = Row::new();
    r.put(0, "ALTN");
    r.left(5, 12, d.alternate.as_ref().map(|a| a.destination.icao.as_str()).unwrap_or("...."));
    left.push(r);

    // The right column: the trip, in figures.
    let right: Vec<(&str, String)> = vec![
        ("CRZ SYS", format!("CI {:.0}", opts.cost_index)),
        ("GND DIST", format!("{:.0}", d.route.distance_nm())),
        ("AIR DIST", format!("{:.0}", air_distance_nm(d))),
        ("G/C DIST", format!("{:.0}", crate::dispatch::distance_nm(d.route.origin.pos, d.route.destination.pos))),
        ("AVG WIND", format!("{}/{:03.0}", hdg(wind_deg), wind_kt)),
        ("AVG W/C", signed(d.perf.avg_wind_kt, 3)),
        ("AVG ISA", signed(d.perf.avg_isa_dev, 3)),
        ("AVG FF KG/HR", format!("{:.0}", burn_per_minute(d) * 60.0)),
        ("CRZ LEVEL", format!("FL{}", hundreds(flown_cruise_ft(d)))),
        ("TIME ENROUTE", fmt_hm(air_min)),
    ];

    for i in 0..left.len().max(right.len()) {
        let mut r = if i < left.len() { std::mem::replace(&mut left[i], Row::new()) } else { Row::new() };
        if let Some((label, value)) = right.get(i) {
            r.put(RIGHT_COLUMN, label);
            r.right(RIGHT_COLUMN, WIDTH - RIGHT_COLUMN, value);
        }
        let _ = writeln!(s, "{}", r.line());
    }

    // The levels the cruise is flown at, in the order they are flown: the initial level at the
    // departure, then every step and the fix it starts at.
    let mut steps = vec![format!("{}/{:04.0}", d.route.origin.icao, flown_cruise_ft(d) / 100.0)];
    for (at, level) in &d.perf.step_climbs {
        steps.push(format!("{at}/{:04.0}", level / 100.0));
    }
    wrapped(s, "FL STEPS ", steps.iter().map(String::as_str), '/');
    let _ = writeln!(s, "{RULE}");
}

/// What the dispatcher wanted the crew to know, which on a plan built by a machine is whatever
/// the planner could not make good on. The first such thing is said here, at the top, where a
/// remark is looked for; the rest are under NOTES, and the count says how many to go and read.
fn dispatch_remarks(s: &mut String, d: &Dispatch) {
    let total = d.perf.warnings.len() + d.violations.len();
    if total == 0 {
        let _ = writeln!(s, "DISP RMKS   NIL");
    } else {
        let first = d.violations.first().map(|v| v.message.clone()).or_else(|| d.perf.warnings.first().cloned()).unwrap_or_default();
        let remark = first.to_uppercase();
        wrapped(s, "DISP RMKS   ", remark.split(' '), ' ');
        if total > 1 {
            let _ = writeln!(s, "            AND {} MORE, UNDER NOTES", total - 1);
        }
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "{RULE}");
}

/// The fuel table: every quantity that goes into the block figure, what it is carried for, and
/// how long it lasts. The rules across it are the table's own 33 columns, not the page's,
/// because it is a table and not a section.
fn planned_fuel(s: &mut String, d: &Dispatch, taxi_out: f64) {
    const SUB: &str = "---------------------------------";
    let f = &d.perf.fuel;
    let alternate = d.perf.alternate.as_ref();

    // The table's own columns: a label, the airport the quantity belongs to, the quantity, and
    // what it buys. `MINIMUM T/OFF FUEL` is wider than the label column and runs into the
    // airport's, which is why the airport is only placed when there is one.
    let row = |s: &mut String, label: &str, apt: &str, quantity: Option<String>, minutes: Option<f64>| {
        let mut r = Row::new();
        r.left(0, 20, label);
        // The airport column holds four characters, not the three a IATA code would take: the
        // plan names airports by their ICAO code throughout, and it is not going to name them
        // one way here and another everywhere else.
        if !apt.is_empty() {
            r.right(15, 4, apt);
        }
        if let Some(q) = quantity {
            r.right(20, 6, &q);
        }
        if let Some(m) = minutes {
            r.right(29, 4, &fmt_hm(m));
        }
        let _ = writeln!(s, "{}", r.line());
    };
    let kgs = |kg: f64| Some(format!("{kg:.0}"));

    let _ = writeln!(s, "         PLANNED FUEL");
    let _ = writeln!(s, "{SUB}");
    let _ = writeln!(s, "FUEL           ARPT   FUEL   TIME");
    let _ = writeln!(s, "{SUB}");

    // Each quantity's time is its own fuel at the trip's mean flow, so the two columns can
    // never disagree: the contingency really does buy the minutes printed against it.
    let contingency_min = endurance(d, f.contingency_kg);
    let reserve_min = crate::dispatch::FuelPolicy::default().final_reserve_min;
    row(s, "TRIP", &d.route.destination.icao, kgs(f.trip_kg), Some(total_minutes(d)));
    row(s, &contingency_label(d, contingency_min), "", kgs(f.contingency_kg), contingency_min);
    row(s, "ALTN", alternate.map(|a| a.icao.as_str()).unwrap_or(""), kgs(f.alternate_kg), Some(alternate.map(|a| a.time_min).unwrap_or(0.0)));
    row(s, "FINRES", "", kgs(f.final_reserve_kg), Some(reserve_min));
    let _ = writeln!(s, "{SUB}");

    let minimum_kg = f.trip_kg + f.contingency_kg + f.alternate_kg + f.final_reserve_kg;
    let minimum_min = total_minutes(d) + contingency_min.unwrap_or(0.0) + alternate.map(|a| a.time_min).unwrap_or(0.0) + reserve_min;
    row(s, "MINIMUM T/OFF FUEL", "", kgs(minimum_kg), Some(minimum_min));
    let _ = writeln!(s, "{SUB}");

    // Tankered fuel is extra fuel: it is carried by choice and burnt like any other, so it
    // belongs on the extra line rather than in a category of its own the table has no room
    // for. What it was carried for is said below, under tankering.
    let extra_kg = f.extra_kg + f.tanker_kg;
    row(s, "EXTRA", "", kgs(extra_kg), endurance(d, extra_kg).or(Some(0.0)));
    let _ = writeln!(s, "{SUB}");
    row(s, "T/OFF FUEL", "", kgs(f.takeoff_kg), Some(minimum_min));
    row(s, "TAXI", &d.route.origin.icao, kgs(f.taxi_kg), Some(taxi_out));
    let _ = writeln!(s, "{SUB}");
    row(s, "BLOCK FUEL", &d.route.origin.icao, kgs(f.block_kg), None);
    row(s, "PIC EXTRA", "", Some(".....".to_string()), None);
    row(s, "TOTAL FUEL", "", Some(".....".to_string()), None);
    let _ = writeln!(s, "REASON FOR PIC EXTRA ............");
    let _ = writeln!(s, "{RULE}");
}

/// The contingency line's label, which says which of the two rules that govern contingency
/// fuel actually bound: a share of the trip fuel, or a floor in minutes of holding. The policy
/// itself is not on the plan, so it is read back out of the fuel — a quantity that is a round
/// percentage of the trip fuel got there by the percentage rule, and anything else by the
/// floor. Saying which rule bound matters, because it is what a reader checks the figure
/// against.
fn contingency_label(d: &Dispatch, minutes: Option<f64>) -> String {
    let trip = d.perf.fuel.trip_kg;
    if trip > 0.0 {
        let pct = d.perf.fuel.contingency_kg / trip * 100.0;
        if (pct - pct.round()).abs() < 0.01 && pct >= 1.0 {
            return format!("CONT {:.0}%", pct.round());
        }
    }
    minutes.map(|m| format!("CONT {m:.0} MIN")).unwrap_or_else(|| "CONT".to_string())
}

/// The two figures a crew type into the aeroplane. They are sums of the table above, and they
/// are printed on their own because arriving at them by adding up under time pressure is how
/// they come to be wrong.
fn fmc_info(s: &mut String, d: &Dispatch) {
    let f = &d.perf.fuel;
    let _ = writeln!(s, "FMC INFO:");
    for (label, kg) in [("FINRES+ALTN", f.final_reserve_kg + f.alternate_kg), ("TRIP+TAXI", f.trip_kg + f.taxi_kg)] {
        let mut r = Row::new();
        r.left(0, 20, label);
        r.right(20, 6, &format!("{kg:.0}"));
        let _ = writeln!(s, "{}", r.line());
    }
    let _ = writeln!(s, "{RULE}");
}

fn tankering(s: &mut String, d: &Dispatch) {
    if d.perf.fuel.tanker_kg > 0.0 {
        let _ = writeln!(s, "TANKERING {:.0} KGS RECOMMENDED (P), CARRIED IN EXTRA", d.perf.fuel.tanker_kg);
    } else {
        let _ = writeln!(s, "NO TANKERING RECOMMENDED (P)");
    }
    let _ = writeln!(s, "{RULE}");
}

/// The declaration and the signatures. A plan is a document somebody accepts, and this is
/// where they accept it.
fn self_briefing(s: &mut String) {
    let _ = writeln!(s, "I HEREWITH CONFIRM THAT I HAVE PERFORMED A THOROUGH SELF BRIEFING");
    let _ = writeln!(s, "ABOUT THE DESTINATION AND ALTERNATE AIRPORTS OF THIS FLIGHT");
    let _ = writeln!(s, "INCLUDING THE APPLICABLE INSTRUMENT APPROACH PROCEDURES, AIRPORT");
    let _ = writeln!(s, "FACILITIES, NOTAMS AND ALL OTHER RELEVANT PARTICULAR INFORMATION.");
    let _ = writeln!(s);
    let mut r = Row::new();
    r.put(0, "DISPATCHER: AMDBGEN");
    r.put(40, "PIC NAME: ......., ......");
    let _ = writeln!(s, "{}", r.line());
    let _ = writeln!(s);
    let mut r = Row::new();
    r.put(0, "TEL: ...............");
    r.put(35, "PIC SIGNATURE: ...............");
    let _ = writeln!(s, "{}", r.line());
    let _ = writeln!(s, "{RULE}");
}

/// The diversion, and what reaching it from the destination costs.
fn alternate_route(s: &mut String, d: &Dispatch) {
    let mut r = Row::new();
    r.put(0, "ALTERNATE ROUTE TO:");
    r.put(55, "FINRES");
    r.right(55, WIDTH - 55, &format!("{:.0}", d.perf.fuel.final_reserve_kg));
    let _ = writeln!(s, "{}", r.line());
    let _ = writeln!(s, "APT      TRK DST              VIA                 FL  WC  TIME  FUEL");
    let _ = writeln!(s, "{RULE}");
    match &d.perf.alternate {
        Some(a) => {
            let filed = d.alternate.as_ref();
            let via = filed.map(|f| f.route_string()).filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "DCT".to_string());
            let mut r = Row::new();
            r.left(0, 8, &a.icao);
            if let Some(f) = filed {
                r.right(9, 3, &hdg(crate::dispatch::bearing_deg(f.origin.pos, f.destination.pos)));
            }
            r.right(13, 4, &format!("{:.0}", a.dist_nm));
            r.left(30, 19, &via);
            r.right(50, 3, &hundreds(a.cruise_ft));
            r.right(58, 4, &fmt_hm(a.time_min));
            r.right(63, 5, &format!("{:.0}", a.fuel_kg));
            let _ = writeln!(s, "{}", r.line());
        }
        None => {
            let _ = writeln!(s, "NO ALTERNATE");
        }
    }
    let _ = writeln!(s, "{RULE}");
}

/// The route, once compactly as it is filed and once with the runways and procedures at each
/// end, which is the form it is read back in — plus the two transition levels, because a
/// cleared level means one thing below them and another above.
fn routing(s: &mut String, d: &Dispatch, opts: &DispatchOptions) {
    let _ = writeln!(s, "ROUTING:");
    let _ = writeln!(s);
    let _ = writeln!(s, "ROUTE ID: {}", opts.flight_number.clone().unwrap_or_else(|| "DEFRTE".to_string()));
    let _ = writeln!(
        s,
        "{} {} {}",
        airport_and_runway(&d.route.origin.icao, d.route.dep_runway.as_deref()),
        d.route.route_string(),
        airport_and_runway(&d.route.destination.icao, d.route.arr_runway.as_deref())
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "AS READ BACK:");
    let _ = writeln!(s, "{}", atc_route(d));
    let _ = writeln!(s);
    let mut r = Row::new();
    r.put(0, &format!("TRANS ALT {} {:.0}", d.route.origin.icao, climb_transition_ft(&d.route.origin.icao)));
    r.put(35, &format!("TRANS LVL {} FL{}", d.route.destination.icao, hundreds(descent_transition_ft(&d.route.destination.icao))));
    let _ = writeln!(s, "{}", r.line());
    let _ = writeln!(s, "{RULE}");
}

fn departure_clearance(s: &mut String) {
    let _ = writeln!(s, "DEPARTURE ATC CLEARANCE:");
    for _ in 0..3 {
        let _ = writeln!(s, ".");
    }
    let _ = writeln!(s, "{RULE}");
}

/// Out, off, on and in, against what was scheduled and what actually happened. The actual
/// column is dotted because it is the crew's to fill in.
fn times(s: &mut String, d: &Dispatch, opts: &DispatchOptions) {
    let (out, off, on, inn) = milestones(d, opts);
    let block_min = total_minutes(d) + opts.taxi_out_min.max(0.0) + opts.taxi_in_min.max(0.0);
    centred(s, "TIMES");
    let _ = writeln!(s);
    let mut r = Row::new();
    r.put(15, "ESTIMATED");
    r.put(32, "SKED");
    r.put(50, "ACTUAL");
    let _ = writeln!(s, "{}", r.line());
    let _ = writeln!(s);
    for (label, at) in [("OUT", out), ("OFF", off), ("ON", on), ("IN", inn)] {
        let mut r = Row::new();
        r.put(0, label);
        r.put(15, &format!("{}Z", hm(at)));
        r.put(32, &format!("{}Z", hm(at)));
        r.put(50, "......Z");
        let _ = writeln!(s, "{}", r.line());
        let _ = writeln!(s);
    }
    let mut r = Row::new();
    r.put(0, "BLOCK TIME");
    r.put(15, &fmt_hm(block_min));
    r.put(32, &fmt_hm(block_min));
    r.put(50, "......");
    let _ = writeln!(s, "{}", r.line());
    let _ = writeln!(s, "{RULE}");
}

/// The weights, in tonnes, estimated against their limits. A weight over its limit is said in
/// words: an aeroplane above its maximum take-off weight is the one thing on this page that
/// stops the flight, and it is not something to mark with a character a reader might take for
/// a footnote.
fn weights(s: &mut String, d: &Dispatch, opts: &DispatchOptions) {
    let w = &d.perf.weights;
    centred(s, "WEIGHTS");
    let _ = writeln!(s);
    let mut r = Row::new();
    r.right(12, 7, "EST");
    r.right(21, 7, "MAX");
    r.put(33, "ACTUAL");
    let _ = writeln!(s, "{}", r.line());
    let _ = writeln!(s);

    let line = |s: &mut String, label: &str, est: String, max: Option<String>, note: &str| {
        let mut r = Row::new();
        r.left(0, 12, label);
        r.right(12, 7, &est);
        if let Some(max) = max {
            r.right(21, 7, &max);
        }
        r.put(33, "......");
        r.put(41, note);
        let _ = writeln!(s, "{}", r.line());
        let _ = writeln!(s);
    };

    let over = |v: f64, max: f64| if v > max { "OVER LIMIT" } else { "" };
    let limited = w.limited_by.as_deref().map(|b| format!("LIMITED BY {b}")).unwrap_or_default();
    line(s, "PAX", format!("{}", opts.passengers), None, "");
    line(s, "PAYLOAD", tonnes(w.payload_kg), None, &limited);
    line(s, "ZFW", tonnes(w.zfw_kg), Some(tonnes(w.max_zfw_kg)), over(w.zfw_kg, w.max_zfw_kg));
    line(s, "FUEL", tonnes(d.perf.fuel.block_kg), Some(tonnes(d.spec.max_fuel_kg)), over(d.perf.fuel.block_kg, d.spec.max_fuel_kg));
    line(s, "TOW", tonnes(w.tow_kg), Some(tonnes(w.max_tow_kg)), over(w.tow_kg, w.max_tow_kg));
    line(s, "LAW", tonnes(w.lw_kg), Some(tonnes(w.max_lw_kg)), over(w.lw_kg, w.max_lw_kg));
    let _ = writeln!(s, "{RULE}");
}

// ---------------------------------------------------------------------------------------
// The flight log
// ---------------------------------------------------------------------------------------

// The log's column grid. Three rows to a fix, and every field on all three rows is placed
// against these, which is what makes a block read down as well as across.
const C_AWY: (usize, usize) = (0, 28);
const C_FL: (usize, usize) = (29, 3);
const C_IMT: (usize, usize) = (35, 3);
const C_MN: (usize, usize) = (40, 3);
const C_WIND: (usize, usize) = (44, 7);
const C_OAT: (usize, usize) = (53, 3);
// The two fuel columns end where their headings do but begin a character earlier than them:
// an aeroplane carrying 242 tonnes needs five characters for a figure in tonnes, and a
// four-engined heavy is exactly the case a plan must not lose a digit off.
const C_EFOB: (usize, usize) = (57, 5);
const C_PBRN: (usize, usize) = (63, 5);

const C_POSITION: (usize, usize) = (0, 11);
const C_LAT: (usize, usize) = (11, 8);
const C_EET: (usize, usize) = (20, 4);
const C_ETO: (usize, usize) = (25, 3);
const C_MORA: (usize, usize) = (29, 3);
const C_ITT: (usize, usize) = (35, 3);
const C_TAS: (usize, usize) = (40, 3);
const C_COMP: (usize, usize) = (47, 4);
const C_TDV: (usize, usize) = (53, 3);

const C_IDENT: (usize, usize) = (0, 10);
const C_LONG: (usize, usize) = (11, 8);
const C_TTLT: (usize, usize) = (20, 4);
const C_ATO: (usize, usize) = (25, 3);
const C_DIS: (usize, usize) = (29, 3);
const C_RDIS: (usize, usize) = (34, 4);
const C_GS: (usize, usize) = (40, 3);
const C_AFOB: (usize, usize) = (58, 4);
const C_ABRN: (usize, usize) = (64, 4);

/// The three heading rows the log's fields are read against.
fn log_headings(s: &mut String) {
    let _ = writeln!(s, "AWY                           FL   IMT   MN    WIND  OAT  EFOB  PBRN");
    let _ = writeln!(s, "POSITION    LAT      EET ETO MORA  ITT  TAS    COMP  TDV");
    let _ = writeln!(s, "IDENT       LONG    TTLT ATO DIS  RDIS   GS               AFOB  ABRN");
}

/// A labelled value too long for the page, broken between whole atoms and never inside one.
/// The label is written once and every continuation is indented to its width, so the value
/// reads as one value down the page rather than as a column of unlabelled fragments.
///
/// What counts as an atom is the caller's to say, and it matters: prose breaks between words,
/// but the list of cruise levels breaks between whole `FIX/LEVEL` pairs, because a line ending
/// in a fix with its level on the next line is a level a reader has to reassemble.
fn wrapped<'a>(s: &mut String, label: &str, atoms: impl IntoIterator<Item = &'a str>, separator: char) {
    let indent = label.chars().count();
    let room = WIDTH.saturating_sub(indent).max(1);
    let mut line = String::new();
    let mut first = true;
    for atom in atoms {
        let piece = if line.is_empty() { atom.to_string() } else { format!("{separator}{atom}") };
        if !line.is_empty() && line.chars().count() + piece.chars().count() > room {
            let _ = writeln!(s, "{}{line}", if first { label.to_string() } else { " ".repeat(indent) });
            first = false;
            line = atom.to_string();
        } else {
            line.push_str(&piece);
        }
    }
    let _ = writeln!(s, "{}{line}", if first { label.to_string() } else { " ".repeat(indent) });
}

/// The flight log: every fix in turn, three rows to a fix. The first row carries what was
/// flown on the leg into it — the airway or procedure, the level, the track, the Mach, the
/// wind and the temperature; the second the fix itself, its latitude, the time to it and the
/// terrain under it; the third its longitude, the running totals, and the columns a crew fills
/// in as they go.
fn flight_log(s: &mut String, d: &Dispatch) {
    centred(s, "FLIGHT LOG");
    let _ = writeln!(s);
    if let Some((ft, at)) = critical_mora(d) {
        let _ = writeln!(s, "MOST CRITICAL MORA {ft:.0} FEET AT {at}");
    }
    let _ = writeln!(s, "{RULE}");
    log_headings(s);
    let _ = writeln!(s, "{RULE}");

    if d.perf.profile.is_empty() {
        let _ = writeln!(s, "NO FLIGHT LOG: THE PERFORMANCE MODEL DID NOT RUN");
        let _ = writeln!(s, "{RULE}");
        return;
    }

    // The performance profile carries none of a SID or STAR's own published constraints — it
    // is built to fly the route, not to say what was filed — so they are looked up here, by
    // the identifier a procedure fix and its profile point share, from the route itself.
    let constraints = procedure_constraints(d);
    let total_nm = d.route.distance_nm().max(1.0);
    let last = d.perf.profile.len() - 1;
    let mut prev_dist = 0.0;
    let mut prev_time = 0.0;

    for (i, p) in d.perf.profile.iter().enumerate() {
        let leg_nm = p.dist_nm - prev_dist;
        let leg_time = p.time_min - prev_time;
        prev_dist = p.dist_nm;
        prev_time = p.time_min;

        // The wind component as it was actually flown, rather than as it was forecast: the
        // difference between the ground speed and the airspeed is the one figure that cannot
        // disagree with the rest of the row it sits in.
        let component = p.gs_kt - p.tas_kt;

        // The airports are named, not identified: a crew reads the log against a chart, and
        // the chart says TRIBHUVAN INTL. The two computed points are named for what they are.
        let (name, ident) = match (i, p.kind) {
            (0, _) => (d.route.origin.name.as_str(), d.route.origin.icao.as_str()),
            (i, _) if i == last => (d.route.destination.name.as_str(), d.route.destination.icao.as_str()),
            (_, ProfileKind::TopOfClimb) => ("T O C", ""),
            (_, ProfileKind::TopOfDescent) => ("T O D", ""),
            _ => (p.ident.as_str(), p.ident.as_str()),
        };

        let mut upper = Row::new();
        upper.left(C_AWY.0, C_AWY.1, &p.via);
        // Standing on the ground at the departure gate is not a level to read back, so the
        // column is left empty there rather than filled with the field's own elevation.
        if i > 0 {
            upper.right(C_FL.0, C_FL.1, &hundreds(p.alt_ft));
        }
        upper.right(C_IMT.0, C_IMT.1, &hdg(p.track_true_deg - waypoint_variation(d, &p.ident)));
        upper.right(C_MN.0, C_MN.1, &mach_text(p.mach));
        upper.right(C_WIND.0, C_WIND.1, &format!("{}/{:03.0}", hdg(p.air.wind_from_deg), p.air.wind_kt));
        upper.right(C_OAT.0, C_OAT.1, &oat(p.air.temp_c));
        upper.right(C_EFOB.0, C_EFOB.1, &tonnes(p.fuel_remaining_kg));
        upper.right(C_PBRN.0, C_PBRN.1, &tonnes(p.fuel_used_kg));
        let _ = writeln!(s, "{}", upper.line());

        let mut middle = Row::new();
        middle.left(C_POSITION.0, C_POSITION.1, name);
        middle.right(C_LAT.0, C_LAT.1, &lat_text(p.pos.0));
        if i > 0 {
            middle.right(C_EET.0, C_EET.1, &fmt_hm(leg_time));
        }
        middle.right(C_ETO.0, C_ETO.1, "...");
        // MORA is carried in hundreds of feet, as the column's three characters require: an
        // 11,300 ft grid minimum reads 113 here, and in full on the line above the headings.
        if let Some(mora) = p.mora_ft {
            middle.right(C_MORA.0, C_MORA.1, &format!("{:.0}", mora / 100.0));
        }
        middle.right(C_ITT.0, C_ITT.1, &hdg(p.track_true_deg));
        middle.right(C_TAS.0, C_TAS.1, &format!("{:.0}", p.tas_kt));
        middle.right(C_COMP.0, C_COMP.1, &signed(component, 3));
        middle.right(C_TDV.0, C_TDV.1, &signed(p.air.isa_dev(p.alt_ft), 2));
        let _ = writeln!(s, "{}", middle.line());

        let mut lower = Row::new();
        lower.left(C_IDENT.0, C_IDENT.1, ident);
        lower.right(C_LONG.0, C_LONG.1, &lon_text(p.pos.1));
        lower.right(C_TTLT.0, C_TTLT.1, &fmt_hm(p.time_min));
        lower.right(C_ATO.0, C_ATO.1, "...");
        if i > 0 {
            lower.right(C_DIS.0, C_DIS.1, &format!("{leg_nm:.0}"));
        }
        lower.right(C_RDIS.0, C_RDIS.1, &format!("{:.0}", (total_nm - p.dist_nm).max(0.0)));
        lower.right(C_GS.0, C_GS.1, &format!("{:.0}", p.gs_kt));
        lower.right(C_AFOB.0, C_AFOB.1, "....");
        lower.right(C_ABRN.0, C_ABRN.1, "....");
        let _ = writeln!(s, "{}", lower.line());

        // A published constraint is not one of the log's own columns, so it goes on a line of
        // its own under the fix it belongs to rather than into a column that cannot hold it.
        if let Some(w) = constraints.get(p.ident.as_str()) {
            let note = constraint_note(w);
            if !note.is_empty() {
                let _ = writeln!(s, "{note}");
            }
        }
        let _ = writeln!(s);
    }
    let _ = writeln!(s, "{RULE}");
}

/// The magnetic variation at a fix, where the route carries one. It is what turns the true
/// track the performance model flew into the magnetic track the `IMT` column asks for; where
/// the navigation data gave none it is nought, the two columns read alike, and [`notes`] says
/// so rather than leaving a reader to assume a correction was applied.
fn waypoint_variation(d: &Dispatch, ident: &str) -> f64 {
    d.route.points.iter().find(|w| w.ident == ident).map(|w| w.mag_var_deg).unwrap_or(0.0)
}

/// Every SID or STAR fix that carries a published constraint, by identifier: what
/// [`flight_log`] annotates each matching fix with.
fn procedure_constraints(d: &Dispatch) -> std::collections::HashMap<&str, &Waypoint> {
    d.route
        .points
        .iter()
        .filter(|w| matches!(w.kind, PointKind::Sid | PointKind::Star))
        .filter(|w| w.alt_min_ft.is_some() || w.alt_max_ft.is_some() || w.speed_max_kt.is_some())
        .map(|w| (w.ident.as_str(), w))
        .collect()
}

/// A published constraint the way a chart prints it against a procedure fix: "4000FT+" for at
/// or above, "6000FT-" for at or below, a range where both are given, and the speed where
/// there is one — whichever of the two the database actually gave.
fn constraint_note(w: &Waypoint) -> String {
    let mut parts = Vec::new();
    match (w.alt_min_ft, w.alt_max_ft) {
        (Some(min), Some(max)) if (min - max).abs() < 1.0 => parts.push(format!("{min:.0}FT")),
        (Some(min), Some(max)) => parts.push(format!("{min:.0}-{max:.0}FT")),
        (Some(min), None) => parts.push(format!("{min:.0}FT+")),
        (None, Some(max)) => parts.push(format!("{max:.0}FT-")),
        (None, None) => {}
    }
    if let Some(kt) = w.speed_max_kt {
        parts.push(format!("{kt:.0}KT MAX"));
    }
    if parts.is_empty() { String::new() } else { format!("            CONSTRAINT {}", parts.join(" ")) }
}

// ---------------------------------------------------------------------------------------
// The sections after the log
// ---------------------------------------------------------------------------------------

fn step_climbs(s: &mut String, d: &Dispatch) {
    if d.perf.step_climbs.is_empty() {
        return;
    }
    centred(s, "STEP CLIMBS");
    let _ = writeln!(s);
    for (at, level) in &d.perf.step_climbs {
        let _ = writeln!(s, "AT {at} CLIMB TO FL{}", hundreds(*level));
    }
    let _ = writeln!(s, "{RULE}");
}

fn equal_time_points(s: &mut String, d: &Dispatch) {
    if d.perf.equal_time_points.is_empty() {
        return;
    }
    centred(s, if d.spec.etops_minutes.is_some() { "ETOPS EQUAL-TIME POINTS" } else { "EQUAL-TIME POINTS" });
    let _ = writeln!(s);
    for (i, etp) in d.perf.equal_time_points.iter().enumerate() {
        // `perf::profile` works out one equal-time point between each pair of airports for an
        // engine failure, then one for a depressurisation, always in that order and always
        // both together — `EqualTimePoint` itself carries no label to say which is which, so
        // the position in the list is the only thing that does.
        let case = if i % 2 == 0 { "ENGINE FAILURE" } else { "DEPRESSURISATION" };
        let _ = writeln!(s, "{case}  BETWEEN {} AND {}  AT {:.0} NM / {}  FUEL {:.0} KGS", etp.between.0, etp.between.1, etp.dist_nm, fmt_hm(etp.time_min), etp.fuel_needed_kg);
    }
    let _ = writeln!(s, "{RULE}");
}

fn point_of_no_return(s: &mut String, d: &Dispatch) {
    let Some(nr) = &d.perf.no_return else { return };
    centred(s, "POINT OF NO RETURN");
    let _ = writeln!(s);
    let _ = writeln!(s, "{}  {:.0} NM / {}  FUEL REMAINING {:.0} KGS", nr.ident, nr.dist_nm, fmt_hm(nr.time_min), nr.fuel_remaining_kg);
    let _ = writeln!(s, "{RULE}");
}

fn fir_section(s: &mut String, d: &Dispatch, crossings: &[crate::route::airspace::FirCrossing]) {
    if crossings.is_empty() {
        return;
    }
    centred(s, "FIR CROSSINGS");
    let _ = writeln!(s);
    let total_min = total_minutes(d);
    let total_nm = d.route.distance_nm().max(1.0);
    for c in crossings {
        // Time at the entry point, interpolated from the total time on the share of the route
        // flown by then: exact once the performance model has run, a fair estimate when the
        // fallback's straight-line one has.
        let entry_min = if total_min > 0.0 { total_min * (c.entry_nm / total_nm) } else { 0.0 };
        let at = d.route.off_block + chrono::Duration::seconds((entry_min * 60.0).round() as i64);
        let mut r = Row::new();
        r.left(0, 6, &c.ident);
        r.left(7, 28, &c.name);
        r.right(36, 9, &format!("{:.0} NM", c.entry_nm));
        r.right(46, 6, &format!("{}Z", hm(at)));
        r.right(53, 15, &format!("EXIT {:.0} NM", c.exit_nm));
        let _ = writeln!(s, "{}", r.line());
    }
    let _ = writeln!(s, "{RULE}");
}

/// The route as it is actually filed, in the ICAO flight plan's own form. A plan that cannot
/// be filed is not a plan, and this is the part a pilot copies out.
fn atc_flight_plan(s: &mut String, d: &Dispatch, opts: &DispatchOptions) {
    centred(s, "ICAO FLIGHT PLAN");
    let _ = writeln!(s);
    for line in crate::ofp::export::icao_message(d, opts.flight_number.as_deref(), opts.registration.as_deref(), opts.flight_rules, opts.flight_type).lines() {
        let _ = writeln!(s, "{line}");
    }
    let _ = writeln!(s, "{RULE}");
}

/// The way ATC reads the route back: the departure airport and the runway in use, the SID where
/// one was flown, every enroute fix, the STAR, then the arrival airport and its runway — the
/// shape a filed route is read back in, not merely item 15's shorter notation.
fn atc_route(d: &Dispatch) -> String {
    let mut parts = vec![airport_and_runway(&d.route.origin.icao, d.route.dep_runway.as_deref())];
    if let Some(sid) = &d.route.sid {
        parts.push(sid.clone());
    }
    for w in d.route.points.iter().filter(|w| matches!(w.kind, PointKind::Enroute | PointKind::Track)) {
        parts.push(w.ident.clone());
    }
    if let Some(star) = &d.route.star {
        parts.push(star.clone());
    }
    parts.push(airport_and_runway(&d.route.destination.icao, d.route.arr_runway.as_deref()));
    parts.join(" ")
}

/// An airport with the runway in use, the way a chart or a filed route names it: "EGLL/27R", or
/// just the airport where no runway was settled on.
fn airport_and_runway(icao: &str, runway: Option<&str>) -> String {
    match runway {
        Some(rw) if !rw.is_empty() => format!("{icao}/{rw}"),
        _ => icao.to_string(),
    }
}

fn taf_period_label(c: TafChange) -> &'static str {
    match c {
        TafChange::Base => "BASE",
        TafChange::From => "FM",
        TafChange::Becoming => "BECMG",
        TafChange::Tempo => "TEMPO",
        TafChange::Prob(_) => "PROB",
        TafChange::ProbTempo(_) => "PROB TEMPO",
    }
}

/// The weather at each end, as issued. A report is quoted exactly or it is not a report, so
/// nothing here is reworded — only the reports the plan was built on are named, in the order
/// they matter: where it leaves from, where it is going, and where it would go instead.
fn airport_weather(s: &mut String, d: &Dispatch) {
    centred(s, "AIRPORT WX LIST");
    let _ = writeln!(s);
    let ends = [
        ("DEPARTURE", &d.route.origin, d.origin_metar.as_ref(), d.origin_taf.as_ref()),
        ("DESTINATION", &d.route.destination, d.destination_metar.as_ref(), d.destination_taf.as_ref()),
    ];
    for (role, airport, metar, taf) in ends {
        let _ = writeln!(s, "{role}:");
        let _ = writeln!(s, "{}  {}", airport.icao, airport.name);
        match metar {
            Some(m) => {
                let _ = writeln!(s, "  SA  {}", m.raw);
            }
            None => {
                let _ = writeln!(s, "  SA  NOT AVAILABLE");
            }
        }
        match taf {
            Some(t) => {
                let _ = writeln!(s, "  FT  {}", t.raw);
                for p in &t.periods {
                    let _ = writeln!(s, "      {} {}-{}", taf_period_label(p.change), p.from.format("%d%H%MZ"), p.to.format("%d%H%MZ"));
                }
            }
            None => {
                let _ = writeln!(s, "  FT  NOT AVAILABLE");
            }
        }
        let _ = writeln!(s);
    }
    if let Some(alt) = &d.alternate {
        let _ = writeln!(s, "ALTERNATE:");
        let _ = writeln!(s, "{}  {}", alt.destination.icao, alt.destination.name);
        match &d.alternate_taf {
            Some(t) => {
                let _ = writeln!(s, "  FT  {}", t.raw);
            }
            None => {
                let _ = writeln!(s, "  FT  NOT AVAILABLE");
            }
        }
        let _ = writeln!(s);
    }
    let _ = writeln!(s, "AIRPORTLIST ENDED");
    let _ = writeln!(s, "{RULE}");
}

fn hazards_and_rules(s: &mut String, d: &Dispatch) {
    centred(s, "HAZARDS PLANNED ROUND");
    let _ = writeln!(s);
    if d.hazards.is_empty() {
        let _ = writeln!(s, "NONE REPORTED");
    }
    for h in &d.hazards {
        wrapped(s, "", h.split(' '), ' ');
    }
    let _ = writeln!(s, "{RULE}");
}

fn violations(s: &mut String, d: &Dispatch) {
    if d.violations.is_empty() {
        return;
    }
    centred(s, "VIOLATIONS");
    let _ = writeln!(s);
    for v in &d.violations {
        let text = format!("{} {}: {}", v.rule, v.at.clone().unwrap_or_default(), v.message);
        wrapped(s, "", text.split(' '), ' ');
    }
    let _ = writeln!(s, "{RULE}");
}

fn notes(s: &mut String, d: &Dispatch) {
    // Where the navigation data gave no variation at any fix, the log's magnetic and true
    // track columns hold the same number, and a reader is owed that plainly.
    let no_variation = d.route.points.iter().all(|w| w.mag_var_deg == 0.0);
    if d.perf.warnings.is_empty() && !no_variation {
        return;
    }
    centred(s, "NOTES");
    let _ = writeln!(s);
    for w in &d.perf.warnings {
        wrapped(s, "- ", w.split(' '), ' ');
    }
    if no_variation {
        let _ = writeln!(s, "- NO MAGNETIC VARIATION IN THE NAVIGATION DATA: IMT EQUALS ITT");
    }
    let _ = writeln!(s, "{RULE}");
}

/// Where the plan came from and what it may be used for.
fn footer(s: &mut String, d: &Dispatch) {
    let _ = writeln!(s, "GENERATED {} {}Z  AIRAC {}  BY AMDBGEN", ddmon(d.generated), hm(d.generated), d.airac.clone().unwrap_or_else(|| "----".to_string()));
    let _ = writeln!(s, "FOR FLIGHT SIMULATION USE ONLY. NOT FOR REAL WORLD NAVIGATION.");
    let _ = writeln!(s, "{RULE}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ofp::fixtures;

    #[test]
    fn the_plan_carries_every_section_the_fixture_has_data_for() {
        let d = fixtures::sample();
        let opts = fixtures::sample_opts();
        let text = render(&d, &opts);
        assert!(text.contains("[ OFP ]"), "the banner");
        assert!(text.contains("PLANNED FUEL"), "the fuel table");
        assert!(text.contains("MINIMUM T/OFF FUEL"), "the minimum take-off fuel line");
        assert!(text.contains("FMC INFO"), "the two figures typed into the aeroplane");
        assert!(text.contains("FLIGHT LOG"), "the flight log");
        assert!(text.contains("ICAO FLIGHT PLAN"), "the filed flight plan");
        assert!(text.contains("WEIGHTS"), "the weights block");
        assert!(text.contains("TIMES"), "the times block");
        assert!(text.contains("ROUTING"), "the route");
        assert!(text.contains("AIRPORT WX LIST"), "the weather");
        assert!(text.contains("HAZARDS PLANNED ROUND"));
        assert!(text.contains(&d.route.origin.icao));
        assert!(text.contains(&d.route.destination.icao));
        // `route::airspace::fir_crossings` is still a stub that answers nothing, so that
        // section is correctly left out here; `fir_crossings_are_printed_with_their_time`
        // below checks its formatting directly, without depending on the stub.
        // The fake's RAD failure and performance fallback both end up as notes.
        assert!(text.contains("RAD"));
    }

    /// The page is set to 68 columns and nothing may run past them: the PDF cuts what does,
    /// and a cut line loses figures off its right-hand end without saying so.
    #[test]
    fn no_line_runs_past_the_page_width() {
        let text = render(&fixtures::sample(), &fixtures::sample_opts());
        for line in text.lines() {
            // The weather reports are quoted exactly as issued, and a METAR is as long as it
            // is; every line the plan itself lays out has to fit.
            let quoted = line.starts_with("  SA  ") || line.starts_with("  FT  ") || line.starts_with("      ");
            assert!(quoted || line.chars().count() <= WIDTH, "{} columns: {line}", line.chars().count());
        }
    }

    #[test]
    fn an_over_limit_weight_is_flagged() {
        let mut d = fixtures::sample();
        d.perf.weights.tow_kg = d.perf.weights.max_tow_kg + 500.0;
        let text = render(&d, &fixtures::sample_opts());
        assert!(text.contains("OVER LIMIT"));
    }

    /// The three rows of a flight log block only mean anything if they line up with the three
    /// heading rows, so the headings and the fields are checked against the same columns.
    #[test]
    fn the_flight_log_puts_each_field_under_its_heading() {
        let d = fixtures::sample();
        let mut s = String::new();
        flight_log(&mut s, &d);
        let lines: Vec<&str> = s.lines().collect();
        let head = lines.iter().position(|l| l.starts_with("AWY ")).expect("the headings");
        assert_eq!(lines[head].find("EFOB").map(|c| c + 4), Some(C_EFOB.0 + C_EFOB.1));
        assert_eq!(lines[head].find("PBRN").map(|c| c + 4), Some(C_PBRN.0 + C_PBRN.1));
        assert_eq!(lines[head + 1].find("MORA"), Some(C_MORA.0));
        assert_eq!(lines[head + 1].find("COMP"), Some(C_COMP.0));
        assert_eq!(lines[head + 2].find("RDIS"), Some(C_RDIS.0));
        assert_eq!(lines[head + 2].find("AFOB"), Some(C_AFOB.0));
        // Three headings, a rule, then the first block: its upper row, then the middle row,
        // which names the departure airport rather than identifying it.
        let middle = lines[head + 5];
        assert!(middle.starts_with(&d.route.origin.name.chars().take(11).collect::<String>()), "{middle}");
    }

    #[test]
    fn fir_crossings_are_printed_with_their_time() {
        use crate::route::airspace::FirCrossing;
        let d = fixtures::sample();
        let crossings = vec![FirCrossing { ident: "LPPC".to_string(), name: "Lisbon FIR".to_string(), entry: d.route.origin.pos, entry_nm: 0.0, exit_nm: d.route.distance_nm() }];
        let mut s = String::new();
        fir_section(&mut s, &d, &crossings);
        assert!(s.contains("FIR CROSSINGS"));
        assert!(s.contains("LPPC"));
        assert!(s.contains("Lisbon FIR"));
        let mut empty = String::new();
        fir_section(&mut empty, &d, &[]);
        assert!(empty.is_empty());
    }

    #[test]
    fn headings_wrap_into_three_digits() {
        assert_eq!(hdg(5.0), "005");
        assert_eq!(hdg(-10.0), "350");
        assert_eq!(hdg(361.0), "001");
    }

    #[test]
    fn minutes_print_as_hours_and_minutes() {
        assert_eq!(fmt_hm(90.0), "0130");
        assert_eq!(fmt_hm(5.0), "0005");
    }

    /// The forms every figure on the page is written in, checked against the real plan these
    /// were measured from: a Kathmandu departure at N2741.8 E08521.6, Mach .62, seventeen
    /// degrees above ISA, fourteen knots on the nose.
    #[test]
    fn figures_take_the_forms_an_operational_plan_writes_them_in() {
        assert_eq!(lat_text(27.696_666), "N2741.8");
        assert_eq!(lon_text(85.36), "E08521.6");
        assert_eq!(lat_text(-27.696_666), "S2741.8");
        assert_eq!(lon_text(-85.36), "W08521.6");
        assert_eq!(mach_text(0.62), ".62");
        assert_eq!(mach_text(0.0), "");
        assert_eq!(signed(17.0, 3), "P017");
        assert_eq!(signed(-14.0, 3), "M014");
        assert_eq!(signed(17.0, 2), "P17");
        assert_eq!(oat(19.0), "19");
        assert_eq!(oat(-5.0), "M05");
        assert_eq!(hundreds(21_000.0), "210");
        assert_eq!(hundreds(5_900.0), "059");
        assert_eq!(tonnes(2_410.0), "2.4");
    }

    /// A minute that rounds up to sixty rolls into the next degree rather than printing as a
    /// sixtieth minute, which is not a position.
    #[test]
    fn a_position_never_prints_a_sixtieth_minute() {
        assert_eq!(lat_text(27.999_9), "N2800.0");
        assert_eq!(lon_text(85.999_9), "E08600.0");
    }

    /// A figure too wide for its column fills it rather than losing digits off one end: a
    /// truncated fuel figure reads as a real one.
    #[test]
    fn a_figure_too_wide_for_its_column_is_marked_rather_than_cut() {
        let mut r = Row::new();
        r.right(0, 3, "12345");
        assert_eq!(r.line(), "***");
        let mut r = Row::new();
        r.right(0, 5, "123");
        assert_eq!(r.line(), "  123");
    }

    #[test]
    fn the_route_read_back_names_the_runway_and_the_procedure_at_each_end() {
        let mut d = fixtures::sample();
        d.route.dep_runway = Some("27R".to_string());
        d.route.arr_runway = Some("16L".to_string());
        d.route.sid = Some("DVR3J".to_string());
        d.route.star = Some("RITE3B".to_string());
        let line = atc_route(&d);
        assert!(line.starts_with(&format!("{}/27R DVR3J ", d.route.origin.icao)), "{line}");
        assert!(line.ends_with(&format!(" RITE3B {}/16L", d.route.destination.icao)), "{line}");
    }

    /// Item 15's route string is `FiledRoute::route_string`'s own job (it is defined in
    /// `dispatch.rs`, frozen); what belongs here is only that the read-back line never names a
    /// procedure that was not flown.
    #[test]
    fn the_route_read_back_names_no_procedure_where_there_is_none() {
        let mut d = fixtures::sample();
        d.route.dep_runway = None;
        d.route.sid = None;
        d.route.star = None;
        let line = atc_route(&d);
        assert!(!line.contains('/'));
        assert!(line.starts_with(&format!("{} ", d.route.origin.icao)));
    }

    #[test]
    fn constraint_note_formats_each_shape_of_constraint() {
        use crate::dispatch::{PointKind, Waypoint};
        let mut w = Waypoint::new("X", (0.0, 0.0), "", PointKind::Sid);
        w.alt_min_ft = Some(3000.0);
        assert!(constraint_note(&w).ends_with("3000FT+"));
        w.alt_min_ft = None;
        w.alt_max_ft = Some(6000.0);
        assert!(constraint_note(&w).ends_with("6000FT-"));
        w.alt_min_ft = Some(4000.0);
        w.alt_max_ft = Some(4000.0);
        assert!(constraint_note(&w).ends_with("4000FT"));
        w.alt_min_ft = Some(3000.0);
        w.alt_max_ft = Some(6000.0);
        w.speed_max_kt = Some(250.0);
        assert!(constraint_note(&w).ends_with("3000-6000FT 250KT MAX"));
    }

    #[test]
    fn the_flight_log_prints_a_procedures_published_constraint_at_its_fix() {
        use crate::dispatch::{Air, PointKind, ProfileKind, ProfilePoint, Waypoint};
        let mut d = fixtures::sample();
        let mut w = Waypoint::new("BPK", (51.75, -0.11), "BPK5K", PointKind::Sid);
        w.alt_min_ft = Some(4000.0);
        d.route.points.push(w);
        d.perf.profile.push(ProfilePoint {
            ident: "BPK".to_string(),
            kind: ProfileKind::Waypoint,
            pos: (51.75, -0.11),
            via: "BPK5K".to_string(),
            alt_ft: 4000.0,
            dist_nm: 10.0,
            time_min: 3.0,
            fuel_used_kg: 100.0,
            fuel_remaining_kg: 9000.0,
            gross_kg: 60000.0,
            track_true_deg: 90.0,
            tas_kt: 250.0,
            gs_kt: 240.0,
            mach: 0.4,
            air: Air::standard(4000.0),
            mora_ft: None,
        });
        let mut s = String::new();
        flight_log(&mut s, &d);
        assert!(s.contains("BPK"));
        assert!(s.contains("4000FT+"));
    }

    /// The fuel table's two columns are worked out at one rate, so a quantity's time really is
    /// the time that quantity buys — a reader who divides one by the other gets the rate back.
    #[test]
    fn the_fuel_tables_times_agree_with_its_quantities() {
        let mut d = fixtures::sample();
        // Two hours of flying on 6,000 kg is fifty kilograms a minute, so 1,500 kg of
        // contingency has to print as thirty minutes and not as anything else.
        d.perf.fuel.trip_kg = 6_000.0;
        d.perf.fuel.contingency_kg = 1_500.0;
        // Both figures are read off the profile's last point, and the fixture's fallback plan
        // need not have one, so the test puts the two hours there itself.
        d.perf.profile = vec![crate::dispatch::ProfilePoint {
            ident: "END".to_string(),
            kind: crate::dispatch::ProfileKind::Waypoint,
            pos: (0.0, 0.0),
            via: String::new(),
            alt_ft: 30_000.0,
            dist_nm: 900.0,
            time_min: 120.0,
            fuel_used_kg: 6_000.0,
            fuel_remaining_kg: 2_000.0,
            gross_kg: 60_000.0,
            track_true_deg: 90.0,
            tas_kt: 450.0,
            gs_kt: 450.0,
            mach: 0.78,
            air: crate::dispatch::Air::standard(30_000.0),
            mora_ft: None,
        }];
        assert_eq!(burn_per_minute(&d), 50.0);
        assert_eq!(endurance(&d, 1_500.0).map(|m| m.round()), Some(30.0));
        // And with nothing burning, no endurance is claimed at all.
        d.perf.fuel.trip_kg = 0.0;
        assert_eq!(endurance(&d, 1_500.0), None);
    }
}
