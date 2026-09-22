//! Beacons, localisers and glideslopes, from X-Plane's own navigation file.
//!
//! A chart is half beacons. Jeppesen's plate for Kennedy's runway 04R carries CANARSIE
//! 112.3 CRI, DEER PARK 117.7 DPK, KENNEDY 115.9 JFK, and the localiser itself as "044°
//! 109.5 IJFK" — and every fix on the approach is given as a distance from that
//! localiser's DME. None of that is in the simulator's own airport data: the approach
//! records name the localiser but carry neither its frequency nor its position.
//!
//! X-Plane's `earth_nav.dat` has all of it, worldwide, in a plain text file that ships
//! with the simulator. One line to a transmitter:
//!
//! ```text
//!  4  40.647663889  -73.752963889  13  10950  18  15870.680 IJFK KJFK K6 04R ILS-cat-III
//!  6  40.628361111  -73.769730556  13  10950  18 300030.680 IJFK KJFK K6 04R GS
//! 12  40.612472222  -73.894444444   4  11230  25      0.000  CRI ENRT K6 CANARSIE VOR/DME
//! ```
//!
//! Two of those columns are packed and worth explaining. A localiser's course column is
//! the magnetic front course multiplied by 360 with the true bearing added, so Kennedy's
//! 15870.680 is a front course of 044° flown on a true bearing of 030.68° — the
//! difference being the magnetic variation. A glideslope's is the angle multiplied by
//! hundredths of a degree multiplied by a thousand with the same true bearing added, so
//! 300030.680 is a three degree path.
//!
//! This is the same provenance as the simulator's own navigation data, which every chart
//! here already reads: it is the copy on this machine, read where it lies, and nothing
//! from it is ever redistributed.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Vor,
    Ndb,
    Dme,
    Localiser,
    Glideslope,
}

#[derive(Debug, Clone)]
pub struct Beacon {
    pub ident: String,
    /// What a chart prints above the frequency: "CANARSIE", "DEER PARK".
    pub name: String,
    pub kind: Kind,
    /// Megahertz, except for an NDB, which is kilohertz.
    pub frequency: f64,
    pub lat: f64,
    pub lon: f64,
    /// The airport a localiser, glideslope or ILS DME belongs to, and its runway.
    pub airport: String,
    pub runway: String,
    /// A localiser's front course, in magnetic degrees, or a glideslope's angle.
    pub course_mag_deg: Option<f64>,
    pub glidepath_deg: Option<f64>,
}

/// Everything in the file, read once.
pub fn index() -> &'static Vec<Beacon> {
    static INDEX: OnceLock<Vec<Beacon>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let beacons = load();
        log::debug!("X-Plane navigation data: {} transmitters", beacons.len());
        beacons
    })
}

fn load() -> Vec<Beacon> {
    let Some(root) = super::local::detect_install() else { return Vec::new() };
    // A Navigraph subscriber's update is installed beside the default copy and is the
    // newer of the two, so it is preferred where it is there.
    let candidates = [root.join("Custom Data").join("earth_nav.dat"), root.join("Resources").join("default data").join("earth_nav.dat")];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let beacons: Vec<Beacon> = text.lines().filter_map(parse_line).collect();
        if !beacons.is_empty() {
            return beacons;
        }
    }
    Vec::new()
}

/// One transmitter from one line, or nothing where the line is a header or a sort we do
/// not draw.
fn parse_line(line: &str) -> Option<Beacon> {
    let mut parts = line.split_whitespace();
    let code: u32 = parts.next()?.parse().ok()?;
    let kind = match code {
        2 => Kind::Ndb,
        3 => Kind::Vor,
        4 | 5 => Kind::Localiser,
        6 => Kind::Glideslope,
        12 | 13 => Kind::Dme,
        _ => return None,
    };
    let lat: f64 = parts.next()?.parse().ok()?;
    let lon: f64 = parts.next()?.parse().ok()?;
    let _elevation = parts.next()?;
    let raw_frequency: f64 = parts.next()?.parse().ok()?;
    let _range = parts.next()?;
    let packed: f64 = parts.next()?.parse().ok()?;
    let ident = parts.next()?.to_string();
    let airport = parts.next().unwrap_or("").to_string();
    let _region = parts.next();
    let rest: Vec<&str> = parts.collect();
    // A localiser and a glideslope name their runway before the description; everything
    // else goes straight into the name.
    let (runway, name_from) = match kind {
        Kind::Localiser | Kind::Glideslope => (rest.first().copied().unwrap_or("").to_string(), 1),
        _ => (String::new(), 0),
    };
    let name = tidy_name(&rest[name_from.min(rest.len())..]);
    // An NDB is given in kilohertz and everything else in hundredths of a megahertz.
    let frequency = if kind == Kind::Ndb { raw_frequency } else { raw_frequency / 100.0 };
    let course_mag_deg = (kind == Kind::Localiser && packed > 0.0).then(|| (packed / 360.0).floor());
    // The angle is stored as hundredths of a degree, multiplied by a thousand, with the
    // true bearing added on: 300030.680 is a three degree path flown on 030.68 true.
    let glidepath_deg = (kind == Kind::Glideslope && packed > 0.0).then(|| (packed / 1000.0).floor() / 100.0);
    Some(Beacon {
        ident,
        name,
        kind,
        frequency,
        lat,
        lon,
        airport,
        runway,
        course_mag_deg,
        glidepath_deg,
    })
}

/// The name a chart would print: the words before the sort of transmitter, which the file
/// puts at the end of the line.
fn tidy_name(words: &[&str]) -> String {
    let mut words: Vec<&str> = words.to_vec();
    while let Some(last) = words.last() {
        let tail = last.to_uppercase();
        let is_type = tail.starts_with("VOR")
            || tail.starts_with("NDB")
            || tail.starts_with("DME")
            || tail.starts_with("ILS")
            || tail.starts_with("LOC")
            || tail.starts_with("LDA")
            || tail.starts_with("SDF")
            || tail.starts_with("IGS")
            || tail == "GS"
            || tail == "TACAN"
            || tail == "VORTAC";
        if is_type && words.len() > 1 {
            words.pop();
        } else {
            break;
        }
    }
    words.join(" ")
}

/// The beacons within a distance of a point, nearest first. Localisers and glideslopes
/// are left out: they belong to one runway and are drawn with it, not as beacons in their
/// own right.
pub fn within(lat: f64, lon: f64, radius_nm: f64) -> Vec<Beacon> {
    let cos = lat.to_radians().cos().max(0.05);
    let mut out: Vec<(f64, Beacon)> = index()
        .iter()
        .filter(|b| matches!(b.kind, Kind::Vor | Kind::Ndb))
        .filter_map(|b| {
            let nm = ((b.lat - lat) * 60.0).hypot((b.lon - lon) * 60.0 * cos);
            (nm <= radius_nm).then(|| (nm, b.clone()))
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.into_iter().map(|(_, b)| b).collect()
}

/// What guides an approach to one runway: the localiser, its glidepath angle, and the DME
/// the fixes are measured from.
#[derive(Debug, Clone)]
pub struct Ils {
    pub ident: String,
    pub frequency: f64,
    pub course_mag_deg: Option<f64>,
    pub glidepath_deg: Option<f64>,
    /// Where the distances printed against the fixes are measured from, where the
    /// localiser has a DME with it.
    pub dme: Option<(f64, f64)>,
}

/// The localiser serving a runway, where there is one.
pub fn ils(icao: &str, runway: &str) -> Option<Ils> {
    let icao = icao.to_uppercase();
    let runway = runway.trim().trim_start_matches("RW").to_uppercase();
    let short = runway.trim_start_matches('0');
    let serves = |b: &Beacon| {
        b.airport.eq_ignore_ascii_case(&icao) && {
            let r = b.runway.to_uppercase();
            r == runway || r.trim_start_matches('0') == short
        }
    };
    let loc = index().iter().find(|b| b.kind == Kind::Localiser && serves(b))?;
    let glidepath = index().iter().find(|b| b.kind == Kind::Glideslope && serves(b)).and_then(|b| b.glidepath_deg);
    // The DME is listed under the localiser's own ident, at the airport rather than at a
    // runway.
    let dme = index()
        .iter()
        .find(|b| b.kind == Kind::Dme && b.ident.eq_ignore_ascii_case(&loc.ident))
        .map(|b| (b.lat, b.lon));
    Some(Ils {
        ident: loc.ident.clone(),
        frequency: loc.frequency,
        course_mag_deg: loc.course_mag_deg,
        glidepath_deg: glidepath,
        dme,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_localiser_line_carries_its_course_and_its_name() {
        let line = " 4  40.647663889  -73.752963889       13    10950    18  15870.680 IJFK KJFK K6 04R ILS-cat-III";
        let b = parse_line(line).expect("a localiser");
        assert_eq!(b.ident, "IJFK");
        assert_eq!(b.kind, Kind::Localiser);
        assert_eq!(b.frequency, 109.5);
        assert_eq!(b.runway, "04R");
        // The course column is the magnetic front course times 360, plus the true bearing.
        assert_eq!(b.course_mag_deg, Some(44.0));
    }

    #[test]
    fn a_glideslope_line_carries_its_angle() {
        let line = " 6  40.628361111  -73.769730556       13    10950    18 300030.680 IJFK KJFK K6 04R GS";
        let b = parse_line(line).expect("a glideslope");
        assert_eq!(b.glidepath_deg, Some(3.0));
    }

    #[test]
    fn a_beacon_is_named_the_way_a_chart_names_it() {
        let line = "12  40.612472222  -73.894444444        4    11230    25      0.000  CRI ENRT K6 CANARSIE VOR/DME";
        let b = parse_line(line).expect("a beacon");
        assert_eq!(b.ident, "CRI");
        assert_eq!(b.name, "CANARSIE");
        assert_eq!(b.frequency, 112.3);
        let line = "3   9.037805556    7.285111111     1191    11630   130     -0.000  ABC ENRT DN ABUJA VOR/DME";
        assert_eq!(parse_line(line).expect("a beacon").name, "ABUJA");
    }
}
