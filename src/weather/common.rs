//! What METAR and TAF share: the day/hour groups both are written in, and the tokens both
//! use for wind, visibility, cloud and pressure. A SIGMET's own text is free-form prose
//! rather than fixed groups, so it does not draw on this.

use crate::cache::Cache;
use crate::sources::http::Http;
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use std::path::PathBuf;

pub(super) fn default_http() -> Http {
    Http::new(20, 100)
}

/// A cache for reports that go stale in minutes, not days: short enough that a plan run
/// an hour later gets a fresh METAR, long enough that replanning the same flight twice in
/// a minute does not ask aviationweather.gov twice.
pub(super) fn short_lived_cache() -> Cache {
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    let mut cache = Cache::new(Some(base.join("amdbgen").join("weather").join("reports")), false, false);
    cache.max_age = Some(std::time::Duration::from_secs(600));
    cache
}

/// A day-of-month and hour(:minute), resolved to the real date nearest a reference time.
/// METARs give only a day and time, and TAF validity periods only a day and hour (24
/// standing for midnight at the start of the next day); neither carries a month or year,
/// so the one meant is whichever real calendar date near the reference this could be.
///
/// Trying the reference's own month and the one either side of it, and keeping whichever
/// candidate lands closest, is what makes a validity period that crosses the end of a
/// month resolve correctly: the far side of the boundary is simply a date in the
/// neighbouring month, found the same way as any other.
pub(super) fn resolve_ddhh(day: u32, hour: u32, minute: u32, near: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let (hour, extra_day) = if hour >= 24 { (hour - 24, 1) } else { (hour, 0) };
    let mut best: Option<(i64, DateTime<Utc>)> = None;
    for delta_month in [-1i32, 0, 1] {
        let total = near.month() as i32 - 1 + delta_month;
        let year = near.year() + total.div_euclid(12);
        let month = (total.rem_euclid(12) + 1) as u32;
        let Some(date) = NaiveDate::from_ymd_opt(year, month, day) else { continue };
        let Some(naive) = date.and_hms_opt(hour, minute, 0) else { continue };
        let dt = Utc.from_utc_datetime(&naive) + Duration::days(extra_day);
        let diff = (dt - near).num_seconds().abs();
        if best.as_ref().is_none_or(|(d, _)| diff < *d) {
            best = Some((diff, dt));
        }
    }
    best.map(|(_, dt)| dt)
}

// ---------------------------------------------------------------------------------
// Wind.
// ---------------------------------------------------------------------------------

pub(super) struct WindGroup {
    pub from_deg: Option<f64>,
    pub kt: f64,
    pub gust_kt: Option<f64>,
}

/// "24012KT", "VRB03KT", "24015G28KT", "24012MPS", "090V150" (a wind that only names the
/// variable range, no speed, is left to the caller — this reads the speed group).
pub(super) fn parse_wind(tok: &str) -> Option<WindGroup> {
    let (digits, to_kt): (&str, fn(f64) -> f64) = if let Some(n) = tok.strip_suffix("KT") {
        (n, |v| v)
    } else if let Some(n) = tok.strip_suffix("MPS") {
        (n, |v| v * 1.943_844_5)
    } else if let Some(n) = tok.strip_suffix("KMH") {
        (n, |v| v * 0.539_957)
    } else {
        return None;
    };
    let (from_deg, rest) = if let Some(r) = digits.strip_prefix("VRB") {
        (None, r)
    } else if digits.len() >= 5 && digits.as_bytes()[0..3].iter().all(u8::is_ascii_digit) {
        (digits[0..3].parse::<f64>().ok(), &digits[3..])
    } else {
        return None;
    };
    let (speed_str, gust_str) = match rest.split_once('G') {
        Some((s, g)) => (s, Some(g)),
        None => (rest, None),
    };
    if speed_str.is_empty() || !speed_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let kt = to_kt(speed_str.parse().ok()?);
    let gust_kt = gust_str.filter(|g| !g.is_empty() && g.chars().all(|c| c.is_ascii_digit())).and_then(|g| g.parse().ok()).map(to_kt);
    Some(WindGroup { from_deg, kt, gust_kt })
}

/// "210V270": a variable-wind-direction group, which carries no speed of its own and is
/// recognised only so it is not mistaken for something else.
pub(super) fn is_variable_wind_group(tok: &str) -> bool {
    let bytes = tok.as_bytes();
    bytes.len() == 7 && bytes[0..3].iter().all(u8::is_ascii_digit) && bytes[3] == b'V' && bytes[4..7].iter().all(u8::is_ascii_digit)
}

// ---------------------------------------------------------------------------------
// Visibility.
// ---------------------------------------------------------------------------------

/// Visibility in metres, from whichever way it was written: four bare digits (metres, and
/// `9999` meaning ten kilometres or more), or a statute-mile group — `10SM`, `P6SM` (six
/// or more), `M1/4SM` (under a quarter), `1/2SM`, or `1 1/2SM` split as two tokens, joined
/// by the caller before this is tried.
pub(super) fn parse_visibility(tok: &str) -> Option<f64> {
    if let Some(sm) = tok.strip_suffix("SM") {
        let sm = sm.trim_start_matches(['P', 'M']);
        let miles: f64 = if let Some((whole, frac)) = sm.split_once(' ') {
            whole.parse::<f64>().ok()? + parse_fraction(frac)?
        } else if sm.contains('/') {
            parse_fraction(sm)?
        } else if !sm.is_empty() {
            sm.parse().ok()?
        } else {
            return None;
        };
        return Some(miles * 1609.344);
    }
    if tok.len() == 4 && tok.chars().all(|c| c.is_ascii_digit()) {
        let m: f64 = tok.parse().ok()?;
        return Some(if m >= 9999.0 { 10_000.0 } else { m });
    }
    None
}

fn parse_fraction(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/')?;
    let d: f64 = d.parse().ok()?;
    if d == 0.0 {
        return None;
    }
    Some(n.parse::<f64>().ok()? / d)
}

/// Whether two consecutive tokens are a whole number and a fraction of statute miles that
/// belong together, such as `1` and `1/2SM`.
pub(super) fn is_split_statute_miles(whole: &str, next: &str) -> bool {
    !whole.is_empty() && whole.chars().all(|c| c.is_ascii_digit()) && next.ends_with("SM") && next.contains('/')
}

// ---------------------------------------------------------------------------------
// Cloud.
// ---------------------------------------------------------------------------------

pub(super) enum CloudToken {
    /// A layer: how much of the sky, its base in hundreds of feet (missing as `///`).
    Layer { amount: &'static str, base_ft: Option<f64> },
    /// `VV002`: the sky is obscured, and this is how far up can be seen.
    VerticalVisibility(Option<f64>),
    /// `SKC`, `CLR`, `NSC`, `NCD`: nothing to report.
    Clear,
}

pub(super) fn parse_cloud(tok: &str) -> Option<CloudToken> {
    match tok {
        "SKC" | "CLR" | "NSC" | "NCD" => return Some(CloudToken::Clear),
        _ => {}
    }
    if let Some(rest) = tok.strip_prefix("VV") {
        let ft = if rest.starts_with('/') { None } else { rest.get(0..3).and_then(|h| h.parse::<f64>().ok()).map(|h| h * 100.0) };
        return Some(CloudToken::VerticalVisibility(ft));
    }
    for amount in ["FEW", "SCT", "BKN", "OVC"] {
        if let Some(rest) = tok.strip_prefix(amount) {
            let height = rest.get(0..3);
            let base_ft = match height {
                Some(h) if h.chars().all(|c| c.is_ascii_digit()) => h.parse::<f64>().ok().map(|v| v * 100.0),
                _ => None, // "///" or missing: the layer is there, its height is not known
            };
            return Some(CloudToken::Layer { amount, base_ft });
        }
    }
    None
}

// ---------------------------------------------------------------------------------
// Pressure and temperature.
// ---------------------------------------------------------------------------------

/// "Q1015" (hectopascals) or "A3002" (inches of mercury, hundredths).
pub(super) fn parse_pressure_hpa(tok: &str) -> Option<f64> {
    if let Some(n) = tok.strip_prefix('Q') {
        if n.len() == 4 && n.chars().all(|c| c.is_ascii_digit()) {
            return n.parse().ok();
        }
    }
    if let Some(n) = tok.strip_prefix('A') {
        if n.len() == 4 && n.chars().all(|c| c.is_ascii_digit()) {
            let inhg: f64 = n.parse::<f64>().ok()? / 100.0;
            return Some(inhg * 33.8639);
        }
    }
    None
}

/// "18/12", "M05/M08", "24/M02": temperature and dewpoint, Celsius, `M` marking negative.
pub(super) fn parse_temp_dewpoint(tok: &str) -> Option<(Option<f64>, Option<f64>)> {
    let (t, d) = tok.split_once('/')?;
    if t.is_empty() {
        return None;
    }
    let one = |s: &str| -> Option<f64> {
        if s.is_empty() || s.contains('/') {
            return None;
        }
        let neg = s.starts_with('M');
        let digits = s.trim_start_matches('M');
        if digits.is_empty() || digits.len() > 3 || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let v: f64 = digits.parse().ok()?;
        Some(if neg { -v } else { v })
    };
    // A bare temperature token (no slash at all) never reaches here; requiring the whole
    // side of the slash to parse as a temperature or be empty (missing dewpoint) is what
    // keeps this from firing on unrelated groups that happen to contain a slash.
    let temp = one(t)?;
    let dew = if d.is_empty() { None } else { Some(one(d)?) };
    Some((Some(temp), dew))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn wind_groups_cover_the_usual_shapes() {
        let w = parse_wind("24012KT").unwrap();
        assert_eq!(w.from_deg, Some(240.0));
        assert_eq!(w.kt, 12.0);
        assert_eq!(w.gust_kt, None);

        let g = parse_wind("28015G22KT").unwrap();
        assert_eq!(g.kt, 15.0);
        assert_eq!(g.gust_kt, Some(22.0));

        let vrb = parse_wind("VRB03KT").unwrap();
        assert_eq!(vrb.from_deg, None);

        let mps = parse_wind("24010MPS").unwrap();
        assert!((mps.kt - 19.43845).abs() < 0.001);

        assert!(is_variable_wind_group("210V270"));
        assert!(!is_variable_wind_group("24012KT"));
    }

    #[test]
    fn visibility_covers_metres_and_statute_miles() {
        assert_eq!(parse_visibility("9999"), Some(10_000.0));
        assert_eq!(parse_visibility("0350"), Some(350.0));
        assert!((parse_visibility("10SM").unwrap() - 16_093.44).abs() < 0.01);
        assert!((parse_visibility("P6SM").unwrap() - 9_656.064).abs() < 0.01);
        assert!((parse_visibility("M1/4SM").unwrap() - 402.336).abs() < 0.01);
        assert!((parse_visibility("1 1/2SM").unwrap() - 2_414.016).abs() < 0.01);
        assert!(is_split_statute_miles("1", "1/2SM"));
        assert!(!is_split_statute_miles("SCT035", "24012KT"));
    }

    #[test]
    fn day_hour_resolves_across_a_month_end() {
        let issued = Utc.with_ymd_and_hms(2026, 3, 31, 23, 0, 0).unwrap();
        let resolved = resolve_ddhh(1, 6, 0, issued).unwrap();
        assert_eq!((resolved.month(), resolved.day()), (4, 1));
        // Hour 24 rolls to the start of the next real day.
        let rolled = resolve_ddhh(31, 24, 0, issued).unwrap();
        assert_eq!((rolled.month(), rolled.day(), rolled.hour()), (4, 1, 0));
    }

    #[test]
    fn temp_dewpoint_reads_negative_and_missing() {
        assert_eq!(parse_temp_dewpoint("18/12"), Some((Some(18.0), Some(12.0))));
        assert_eq!(parse_temp_dewpoint("M05/M08"), Some((Some(-5.0), Some(-8.0))));
        assert_eq!(parse_temp_dewpoint("24/M02"), Some((Some(24.0), Some(-2.0))));
        assert_eq!(parse_temp_dewpoint("SCT035"), None);
    }

    #[test]
    fn pressure_reads_hpa_and_inhg() {
        assert_eq!(parse_pressure_hpa("Q1015"), Some(1015.0));
        assert!((parse_pressure_hpa("A3002").unwrap() - 1016.594).abs() < 0.01);
    }
}
