//! Winds and temperatures aloft from NOAA's Global Forecast System, at a quarter degree,
//! fetched through NOMADS's own GRIB filter so that only the parameters, levels and area a
//! route actually needs cross the network — a whole 0.25-degree GFS file is gigabytes, and
//! a route needs three parameters over a strip a few hundred miles wide.
//!
//! Everything is fetched and decoded once, up front, into flat arrays: `air` is called
//! from inside the route search's hot loop, perhaps a million times over one plan, and by
//! the time it runs there must be nothing left to do but look up four corners of a grid,
//! two levels and two times, and interpolate between them.

use super::grib2;
use crate::cache::Cache;
use crate::dispatch::{Air, Bounds, LatLon, WindField};
use crate::sources::http::Http;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Timelike, Utc};
use std::path::PathBuf;

/// The pressure levels asked for, 850 down to 150 hPa: below FL050 and above FL450 a
/// route is either climbing or descending rather than cruising, so the levels a flight
/// actually spends time at are all inside this band. Kept sorted by falling pressure,
/// which is rising altitude, so a lookup is a scan rather than a search.
const LEVELS_HPA: [f64; 9] = [850.0, 700.0, 600.0, 500.0, 400.0, 300.0, 250.0, 200.0, 150.0];

const CYCLE_HOURS: [u32; 4] = [0, 6, 12, 18];

/// A forecast of the winds and temperatures aloft over an area: two time steps, three
/// hours apart, bracketing the time asked for, each at every level in [`LEVELS_HPA`], laid
/// out as flat grids ready to be indexed rather than searched.
pub struct Forecast {
    grid: grib2::GridDef,
    t0: DateTime<Utc>,
    t1: DateTime<Utc>,
    /// `level * ni * nj + j * ni + i`, metres per second, Kelvin.
    u0: Vec<f32>,
    v0: Vec<f32>,
    temp0: Vec<f32>,
    u1: Vec<f32>,
    v1: Vec<f32>,
    temp1: Vec<f32>,
    /// The strongest wind anywhere in either snapshot, at each level, knots: what
    /// [`Forecast::max_tailwind_kt`] answers from.
    max_wind_kt_by_level: [f64; LEVELS_HPA.len()],
}

impl Forecast {
    /// The global model's forecast over an area for a time, fetched and cached.
    ///
    /// GFS cycles are run at 00, 06, 12 and 18Z and published four to five hours later;
    /// the newest one that has actually appeared is used, falling back a cycle at a time
    /// on a 404 until one answers. Within a cycle, the two forecast hours either side of
    /// `when`, three hours apart, are fetched so [`WindField::air`] can interpolate
    /// between them.
    pub fn fetch(bounds: Bounds, when: DateTime<Utc>) -> Result<Forecast> {
        let http = default_http();
        let cache = default_cache();
        Forecast::fetch_with(&http, &cache, bounds, when, Utc::now())
    }

    fn fetch_with(http: &Http, cache: &Cache, bounds: Bounds, when: DateTime<Utc>, now: DateTime<Utc>) -> Result<Forecast> {
        let mut cycle = latest_published_cycle(now);
        let mut last_err = None;
        // Eight cycles back is two days: generous enough for NOMADS to be well behind,
        // without an unbounded search when the service is simply down.
        for _ in 0..8 {
            match Forecast::fetch_cycle(http, cache, bounds, when, cycle) {
                Ok(f) => return Ok(f),
                Err(e) => {
                    last_err = Some(e);
                    cycle -= Duration::hours(6);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no GFS cycle answered")))
    }

    fn fetch_cycle(http: &Http, cache: &Cache, bounds: Bounds, when: DateTime<Utc>, cycle: DateTime<Utc>) -> Result<Forecast> {
        let hours_since = (when - cycle).num_seconds() as f64 / 3600.0;
        let lo_hour = (hours_since / 3.0).floor().max(0.0) as u32 * 3;
        let hi_hour = lo_hour + 3;
        let fields_lo = fetch_hour(http, cache, cycle, lo_hour, bounds).with_context(|| format!("GFS {} f{lo_hour:03}", cycle.format("%Y%m%d%HZ")))?;
        let fields_hi = fetch_hour(http, cache, cycle, hi_hour, bounds).with_context(|| format!("GFS {} f{hi_hour:03}", cycle.format("%Y%m%d%HZ")))?;
        Forecast::assemble(fields_lo, fields_hi)
    }

    fn assemble(fields_lo: Vec<grib2::Field>, fields_hi: Vec<grib2::Field>) -> Result<Forecast> {
        let grid = fields_lo.first().map(|f| f.grid).ok_or_else(|| anyhow!("GFS answered with no fields"))?;
        let snap_lo = build_snapshot(&fields_lo, &grid)?;
        let snap_hi = build_snapshot(&fields_hi, &grid)?;
        let n = grid.ni * grid.nj;
        let mut max_wind_kt_by_level = [0.0f64; LEVELS_HPA.len()];
        for (level, slot) in max_wind_kt_by_level.iter_mut().enumerate() {
            let mut m = 0.0f64;
            for (u, v) in [(&snap_lo.u, &snap_lo.v), (&snap_hi.u, &snap_hi.v)] {
                for k in 0..n {
                    let idx = level * n + k;
                    let speed = ((u[idx] as f64).powi(2) + (v[idx] as f64).powi(2)).sqrt() * MPS_TO_KT;
                    if speed.is_finite() && speed > m {
                        m = speed;
                    }
                }
            }
            *slot = m;
        }
        Ok(Forecast { grid, t0: snap_lo.time, t1: snap_hi.time, u0: snap_lo.u, v0: snap_lo.v, temp0: snap_lo.t, u1: snap_hi.u, v1: snap_hi.v, temp1: snap_hi.t, max_wind_kt_by_level })
    }

    /// The strongest wind anywhere in the grid at a level, knots: the bound the route
    /// search cannot beat, so its estimate of what is left to fly is never wrong to be
    /// optimistic about. Built once when the forecast is fetched, not per call.
    pub fn max_tailwind_kt(&self, level_ft: f64) -> f64 {
        let (lo, hi, _) = level_bracket(isa_pressure_hpa(level_ft));
        self.max_wind_kt_by_level[lo].max(self.max_wind_kt_by_level[hi])
    }

    /// The four grid corners around a place, and the fractions across them: index
    /// arithmetic on the canonical grid, wrapping in longitude where the grid runs the
    /// whole way round and clamping to the edge otherwise (a route that strays just
    /// outside the area asked for gets the edge's air rather than a crash).
    #[inline]
    fn cell(&self, at: LatLon) -> (usize, usize, usize, usize, f64, f64) {
        let (i0, i1, fx) = if self.grid.ni <= 1 || self.grid.di <= 0.0 {
            (0, 0, 0.0)
        } else {
            let lon_in_frame = self.grid.lo1 + (at.1 - self.grid.lo1).rem_euclid(360.0);
            let raw_i = (lon_in_frame - self.grid.lo1) / self.grid.di;
            if self.grid.wraps() {
                let i0f = raw_i.floor();
                let i0 = i0f.rem_euclid(self.grid.ni as f64) as usize;
                ((i0 as usize).min(self.grid.ni - 1), (i0 + 1) % self.grid.ni, raw_i - i0f)
            } else {
                let clamped = raw_i.clamp(0.0, (self.grid.ni - 1) as f64);
                let i0 = (clamped.floor() as usize).min(self.grid.ni - 2);
                (i0, i0 + 1, (clamped - i0 as f64).clamp(0.0, 1.0))
            }
        };
        let (j0, j1, fy) = if self.grid.nj <= 1 || self.grid.dj <= 0.0 {
            (0, 0, 0.0)
        } else {
            let raw_j = (self.grid.la1 - at.0) / self.grid.dj;
            let clamped = raw_j.clamp(0.0, (self.grid.nj - 1) as f64);
            let j0 = (clamped.floor() as usize).min(self.grid.nj - 2);
            (j0, j0 + 1, (clamped - j0 as f64).clamp(0.0, 1.0))
        };
        (i0, i1, j0, j1, fx, fy)
    }

    #[inline]
    fn time_frac(&self, when: DateTime<Utc>) -> f64 {
        let span = (self.t1 - self.t0).num_seconds() as f64;
        if span <= 0.0 {
            return 0.0;
        }
        ((when - self.t0).num_seconds() as f64 / span).clamp(0.0, 1.0)
    }
}

impl WindField for Forecast {
    fn air(&self, at: LatLon, alt_ft: f64, when: DateTime<Utc>) -> Air {
        let (lo, hi, lfrac) = level_bracket(isa_pressure_hpa(alt_ft));
        let (i0, i1, j0, j1, fx, fy) = self.cell(at);
        let tfrac = self.time_frac(when);
        let n = self.grid.ni * self.grid.nj;
        let ni = self.grid.ni;

        // Bilinear in position at one level, then linear in log-pressure between the two
        // bracketing levels, then linear in time between the two forecast snapshots: no
        // allocation, no I/O, and every lookup is a multiply and an array read.
        let at_level = |arr: &[f32], level: usize| -> f64 {
            let base = level * n;
            let v00 = arr[base + j0 * ni + i0] as f64;
            let v10 = arr[base + j0 * ni + i1] as f64;
            let v01 = arr[base + j1 * ni + i0] as f64;
            let v11 = arr[base + j1 * ni + i1] as f64;
            let top = v00 + (v10 - v00) * fx;
            let bot = v01 + (v11 - v01) * fx;
            top + (bot - top) * fy
        };
        let snapshot = |u: &[f32], v: &[f32], t: &[f32]| -> (f64, f64, f64) {
            let ulo = at_level(u, lo);
            let uhi = at_level(u, hi);
            let vlo = at_level(v, lo);
            let vhi = at_level(v, hi);
            let tlo = at_level(t, lo);
            let thi = at_level(t, hi);
            (ulo + (uhi - ulo) * lfrac, vlo + (vhi - vlo) * lfrac, tlo + (thi - tlo) * lfrac)
        };
        let (u0, v0, t0) = snapshot(&self.u0, &self.v0, &self.temp0);
        let (u1, v1, t1) = snapshot(&self.u1, &self.v1, &self.temp1);
        let u = u0 + (u1 - u0) * tfrac;
        let v = v0 + (v1 - v0) * tfrac;
        let t_k = t0 + (t1 - t0) * tfrac;

        let speed_ms = (u * u + v * v).sqrt();
        // The vector (u east, v north) points where the wind blows *towards*; the report
        // is always of where it blows *from*.
        let towards_deg = u.atan2(v).to_degrees();
        let from_deg = (towards_deg + 180.0).rem_euclid(360.0);
        Air { wind_from_deg: from_deg, wind_kt: speed_ms * MPS_TO_KT, temp_c: t_k - 273.15 }
    }
}

const MPS_TO_KT: f64 = 1.943_844_5;

/// The pressure the ICAO standard atmosphere gives at a pressure altitude, hectopascals:
/// the troposphere's power law below the tropopause at 36,089 ft, the isothermal
/// stratosphere's exponential above it. Textbook physics, not any one program's code.
pub fn isa_pressure_hpa(alt_ft: f64) -> f64 {
    let alt_m = alt_ft * 0.3048;
    const TROPOPAUSE_M: f64 = 11_000.0;
    if alt_m <= TROPOPAUSE_M {
        1013.25 * (1.0 - 2.255_77e-5 * alt_m).powf(5.255_88)
    } else {
        226.321 * (-(alt_m - TROPOPAUSE_M) / 6341.62).exp()
    }
}

/// The two levels in [`LEVELS_HPA`] a pressure falls between, and how far across in
/// log-pressure: `0` at the lower (`lo`), `1` at the higher (`hi`). A pressure outside the
/// table's range clamps to its nearest end rather than extrapolating.
fn level_bracket(p_hpa: f64) -> (usize, usize, f64) {
    let n = LEVELS_HPA.len();
    if p_hpa >= LEVELS_HPA[0] {
        return (0, 0, 0.0);
    }
    if p_hpa <= LEVELS_HPA[n - 1] {
        return (n - 1, n - 1, 0.0);
    }
    for i in 0..n - 1 {
        let (hi_p, lo_p) = (LEVELS_HPA[i], LEVELS_HPA[i + 1]);
        if p_hpa <= hi_p && p_hpa >= lo_p {
            let frac = (hi_p.ln() - p_hpa.ln()) / (hi_p.ln() - lo_p.ln());
            return (i, i + 1, frac);
        }
    }
    (n - 1, n - 1, 0.0)
}

// ---------------------------------------------------------------------------------
// Fetching.
// ---------------------------------------------------------------------------------

fn default_http() -> Http {
    Http::new(60, 150)
}

/// A forecast for a given cycle, hour and area never changes once NOAA has published it,
/// so unlike the METAR and TAF caches this one never expires.
fn default_cache() -> Cache {
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    Cache::new(Some(base.join("amdbgen").join("weather").join("gfs")), false, false)
}

/// The most recent cycle that should have appeared on NOMADS by `now`: it runs at one of
/// the four synoptic hours and takes four to five hours to be processed and published, so
/// five hours are allowed before a cycle is tried at all.
fn latest_published_cycle(now: DateTime<Utc>) -> DateTime<Utc> {
    let candidate = now - Duration::hours(5);
    let hour = CYCLE_HOURS.iter().rev().find(|h| **h <= candidate.hour()).copied().unwrap_or(18);
    let date = if hour <= candidate.hour() { candidate.date_naive() } else { (candidate - Duration::days(1)).date_naive() };
    date.and_hms_opt(hour, 0, 0).expect("a real hour").and_utc()
}

fn filter_url(cycle: DateTime<Utc>, hour: u32, bounds: Bounds) -> String {
    let (west, east) = query_lon_range(bounds);
    let mut levels = String::new();
    for lvl in LEVELS_HPA {
        levels.push_str(&format!("&lev_{}_mb=on", lvl as i64));
    }
    format!(
        "https://nomads.ncep.noaa.gov/cgi-bin/filter_gfs_0p25.pl?dir=%2Fgfs.{date}%2F{cyc:02}%2Fatmos&file=gfs.t{cyc:02}z.pgrb2.0p25.f{hour:03}\
         &var_UGRD=on&var_VGRD=on&var_TMP=on{levels}&subregion=&toplat={north:.3}&leftlon={west:.3}&rightlon={east:.3}&bottomlat={south:.3}",
        date = cycle.format("%Y%m%d"),
        cyc = cycle.hour(),
        north = bounds.north,
        south = bounds.south,
    )
}

/// The longitudes to ask NOMADS for: plain west/east normally, and `east` pushed past 360
/// where the area crosses the date line, since the filter takes a plain numeric range
/// rather than any notion of wrapping.
fn query_lon_range(bounds: Bounds) -> (f64, f64) {
    if bounds.west <= bounds.east {
        (bounds.west, bounds.east)
    } else {
        (bounds.west, bounds.east + 360.0)
    }
}

fn fetch_hour(http: &Http, cache: &Cache, cycle: DateTime<Utc>, hour: u32, bounds: Bounds) -> Result<Vec<grib2::Field>> {
    let key = format!("{}/{hour:03}/{:.2}_{:.2}_{:.2}_{:.2}.grib2", cycle.format("%Y%m%d%H"), bounds.south, bounds.north, bounds.west, bounds.east);
    let url = filter_url(cycle, hour, bounds);
    let bytes = cache.get_or_fetch_bytes(&key, || http.get_bytes(&url))?;
    grib2::decode_all(&bytes)
}

struct Snapshot {
    time: DateTime<Utc>,
    u: Vec<f32>,
    v: Vec<f32>,
    t: Vec<f32>,
}

fn valid_time(f: &grib2::Field) -> DateTime<Utc> {
    f.reference_time + Duration::seconds((f.forecast_hours * 3600.0).round() as i64)
}

fn level_index(value_pa: f64) -> Option<usize> {
    let hpa = (value_pa / 100.0).round() as i64;
    LEVELS_HPA.iter().position(|l| *l as i64 == hpa)
}

/// The 27 fields one forecast hour answers with (three parameters at nine levels, in
/// whatever order the filter sent them) sorted into the flat, level-major arrays `air`
/// reads.
fn build_snapshot(fields: &[grib2::Field], grid: &grib2::GridDef) -> Result<Snapshot> {
    let n = grid.ni * grid.nj;
    let mut u = vec![f32::NAN; LEVELS_HPA.len() * n];
    let mut v = vec![f32::NAN; LEVELS_HPA.len() * n];
    let mut t = vec![f32::NAN; LEVELS_HPA.len() * n];
    let mut time = None;
    for f in fields {
        if f.grid != *grid {
            anyhow::bail!("GFS answered with fields on two different grids");
        }
        let Some(level) = level_index(f.level_value) else { continue };
        let slot = match (f.category, f.number) {
            (2, 2) => &mut u, // UGRD
            (2, 3) => &mut v, // VGRD
            (0, 0) => &mut t, // TMP
            _ => continue,
        };
        slot[level * n..(level + 1) * n].copy_from_slice(&f.values);
        time.get_or_insert_with(|| valid_time(f));
    }
    Ok(Snapshot { time: time.ok_or_else(|| anyhow!("no usable fields in this forecast hour"))?, u, v, t })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn isa_pressure_matches_the_textbook_figures() {
        assert!((isa_pressure_hpa(0.0) - 1013.25).abs() < 0.1);
        // Half of sea-level pressure is reached close to 18,000 ft, the aviation rule of
        // thumb ("half the pressure every 18,000 feet") that transition altitudes and
        // cabin-pressurisation schedules are built on.
        assert!((isa_pressure_hpa(18_000.0) - 506.6).abs() < 5.0, "{}", isa_pressure_hpa(18_000.0));
        // The tropopause, 36,089 ft: the standard atmosphere's own figure is 226.32 hPa.
        assert!((isa_pressure_hpa(36_089.0) - 226.32).abs() < 0.5, "{}", isa_pressure_hpa(36_089.0));
    }

    #[test]
    fn level_bracket_picks_the_levels_either_side_in_log_pressure() {
        let (lo, hi, frac) = level_bracket(850.0);
        assert_eq!((lo, hi), (0, 0));
        assert_eq!(frac, 0.0);
        let (lo, hi, frac) = level_bracket(150.0);
        assert_eq!((lo, hi), (8, 8));
        assert_eq!(frac, 0.0);
        // Between 500 and 400 hPa (indices 3 and 4), nearer 500 in log-pressure.
        let (lo, hi, frac) = level_bracket(470.0);
        assert_eq!((lo, hi), (3, 4));
        assert!(frac > 0.0 && frac < 0.5, "{frac}");
        // Above the table's top and below its bottom clamp rather than extrapolate.
        assert_eq!(level_bracket(1000.0), (0, 0, 0.0));
        assert_eq!(level_bracket(50.0), (8, 8, 0.0));
    }

    #[test]
    fn a_cycle_falls_back_five_hours_before_it_is_trusted() {
        let ymdh = |y, m, d, h| Utc.with_ymd_and_hms(y, m, d, h, 0, 0).single().unwrap();
        // Just past 12Z: the 06Z cycle is trusted, 12Z is not (it needs another hour).
        assert_eq!(latest_published_cycle(ymdh(2026, 6, 1, 16)), ymdh(2026, 6, 1, 6));
        assert_eq!(latest_published_cycle(ymdh(2026, 6, 1, 17)), ymdh(2026, 6, 1, 12));
        // Just after midnight: the day before's 18Z cycle.
        assert_eq!(latest_published_cycle(ymdh(2026, 6, 2, 2)), ymdh(2026, 6, 1, 18));
    }

    #[test]
    fn wraparound_bounds_push_the_east_edge_past_360() {
        let b = Bounds { south: 40.0, north: 50.0, west: 170.0, east: -170.0 };
        assert_eq!(query_lon_range(b), (170.0, 190.0));
        let b = Bounds { south: 40.0, north: 50.0, west: -10.0, east: 10.0 };
        assert_eq!(query_lon_range(b), (-10.0, 10.0));
    }

    // ------------------------------------------------------------------
    // A tiny synthetic Forecast, built directly (not through decoding), to exercise
    // interpolation and the wraparound grid without any network access.
    // ------------------------------------------------------------------

    fn synthetic_forecast() -> Forecast {
        // A 4x3 grid, 1 degree apart, north-west corner at (10N, 0E): global in longitude
        // is not attempted here, just a plain regional box.
        let grid = grib2::GridDef { ni: 4, nj: 3, la1: 10.0, lo1: 0.0, di: 1.0, dj: 1.0 };
        let n = grid.ni * grid.nj;
        let levels = LEVELS_HPA.len();
        // At every level and both times, U increases 10 m/s per degree east, V is a flat
        // 5 m/s, and temperature is a flat 250 K — enough to check bilinear interpolation
        // and time interpolation without the level dimension muddying it.
        let make = |u_at_t1: bool| -> (Vec<f32>, Vec<f32>, Vec<f32>) {
            let mut u = vec![0.0f32; levels * n];
            let mut v = vec![0.0f32; levels * n];
            let mut t = vec![0.0f32; levels * n];
            for level in 0..levels {
                for j in 0..grid.nj {
                    for i in 0..grid.ni {
                        let idx = level * n + j * grid.ni + i;
                        u[idx] = if u_at_t1 { 20.0 + i as f32 * 10.0 } else { i as f32 * 10.0 };
                        v[idx] = 5.0;
                        t[idx] = 250.0;
                    }
                }
            }
            (u, v, t)
        };
        let (u0, v0, temp0) = make(false);
        let (u1, v1, temp1) = make(true);
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t1 = t0 + Duration::hours(3);
        Forecast { grid, t0, t1, u0, v0, temp0, u1, v1, temp1, max_wind_kt_by_level: [50.0; LEVELS_HPA.len()] }
    }

    #[test]
    fn air_interpolates_bilinearly_and_in_time() {
        let f = synthetic_forecast();
        // Exactly on a grid point, at the forecast's first time: U should be exact.
        let a = f.air((10.0, 1.0), 5_000.0, f.t0);
        let speed_a = (10f64 * 10f64 + 5f64 * 5f64).sqrt() * MPS_TO_KT;
        assert!((a.wind_kt - speed_a).abs() < 0.5, "{} vs {}", a.wind_kt, speed_a);
        // Halfway between two columns, still nowhere near the second time: U halfway
        // between 0 and 10 m/s, i.e. 5 m/s.
        let b = f.air((10.0, 0.5), 5_000.0, f.t0);
        let speed_b = (5f64 * 5f64 + 5f64 * 5f64).sqrt() * MPS_TO_KT;
        assert!((b.wind_kt - speed_b).abs() < 0.5, "{} vs {}", b.wind_kt, speed_b);
        // Halfway in time between t0 and t1: U at column 0 goes from 0 to 20, so 10 m/s.
        let mid = t0_plus_half(&f);
        let c = f.air((10.0, 0.0), 5_000.0, mid);
        let speed_c = (10f64 * 10f64 + 5f64 * 5f64).sqrt() * MPS_TO_KT;
        assert!((c.wind_kt - speed_c).abs() < 0.5, "{} vs {}", c.wind_kt, speed_c);
        assert!((c.temp_c - (250.0 - 273.15)).abs() < 0.1);
    }

    fn t0_plus_half(f: &Forecast) -> DateTime<Utc> {
        f.t0 + (f.t1 - f.t0) / 2
    }

    #[test]
    fn air_clamps_at_the_edge_of_a_non_wrapping_grid() {
        let f = synthetic_forecast();
        let inside = f.air((10.0, 3.0), 5_000.0, f.t0);
        let outside = f.air((10.0, 30.0), 5_000.0, f.t0);
        assert_eq!(inside.wind_kt, outside.wind_kt, "past the east edge should read the edge's own air");
    }

    #[test]
    fn max_tailwind_kt_reads_the_precomputed_bound() {
        let mut f = synthetic_forecast();
        f.max_wind_kt_by_level[3] = 210.0;
        f.max_wind_kt_by_level[4] = 90.0;
        // 470 hPa falls between levels 3 and 4: the bound must not be beatable by either.
        let level_ft = pressure_to_alt_ft_for_test(470.0);
        assert_eq!(f.max_tailwind_kt(level_ft), 210.0);
    }

    /// The inverse of `isa_pressure_hpa`, by bisection: only used to build a test input.
    fn pressure_to_alt_ft_for_test(target_hpa: f64) -> f64 {
        let (mut lo, mut hi) = (0.0f64, 60_000.0f64);
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            if isa_pressure_hpa(mid) > target_hpa {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    }

    #[test]
    fn a_million_calls_to_air_is_fast() {
        let f = synthetic_forecast();
        let start = std::time::Instant::now();
        let calls = 1_000_000;
        let mut acc = 0.0;
        for i in 0..calls {
            let lat = 10.0 - (i % 100) as f64 * 0.02;
            let lon = (i % 400) as f64 * 0.01;
            let alt = 5_000.0 + (i % 40_000) as f64;
            let a = f.air((lat, lon), alt, f.t0 + Duration::seconds(i % 10_800));
            acc += a.wind_kt;
        }
        let elapsed = start.elapsed();
        assert!(acc.is_finite());
        // Generous: this is a correctness backstop, not a micro-benchmark, but a call
        // costing more than a few microseconds would mean an allocation crept in.
        assert!(elapsed.as_secs_f64() < 10.0, "{calls} calls took {elapsed:?}");
        eprintln!("{calls} calls to Forecast::air took {elapsed:?} ({:.1} ns/call)", elapsed.as_nanos() as f64 / calls as f64);
    }

    #[test]
    fn a_filter_url_names_the_cycle_hour_levels_and_area() {
        let cycle = Utc.with_ymd_and_hms(2026, 9, 23, 6, 0, 0).unwrap();
        let url = filter_url(cycle, 3, Bounds { south: 45.0, north: 55.0, west: -40.0, east: -10.0 });
        assert!(url.contains("dir=%2Fgfs.20260923%2F06%2Fatmos"));
        assert!(url.contains("file=gfs.t06z.pgrb2.0p25.f003"));
        assert!(url.contains("var_UGRD=on") && url.contains("var_VGRD=on") && url.contains("var_TMP=on"));
        assert!(url.contains("lev_850_mb=on") && url.contains("lev_150_mb=on"));
        assert!(url.contains("toplat=55") && url.contains("bottomlat=45"));
    }

    // ------------------------------------------------------------------
    // Real-data smoke tests: not run by default, since they need the network and NOAA's
    // own availability. See the module-level report this crate's author writes for what
    // these printed on a real run.
    // ------------------------------------------------------------------

    #[test]
    #[ignore]
    fn the_north_atlantic_at_fl350() {
        let bounds = Bounds::around((50.0, -30.0), (50.0, -30.0), 600.0);
        let start_fetch = std::time::Instant::now();
        let forecast = Forecast::fetch(bounds, Utc::now() + Duration::hours(6)).expect("GFS should answer");
        println!("fetch took {:?}", start_fetch.elapsed());
        let air = forecast.air((50.0, -30.0), 35_000.0, Utc::now() + Duration::hours(6));
        println!("50N 30W FL350: {:.0} deg / {:.0} kt, ISA dev {:.1} C", air.wind_from_deg, air.wind_kt, air.isa_dev(35_000.0));
        println!("strongest wind anywhere in the grid at FL350: {:.0} kt", forecast.max_tailwind_kt(35_000.0));

        let calls = 200_000;
        let start = std::time::Instant::now();
        let mut acc = 0.0;
        for i in 0..calls {
            let a = forecast.air((45.0 + (i % 100) as f64 * 0.1, -40.0 + (i % 200) as f64 * 0.1), 35_000.0, Utc::now() + Duration::hours(6));
            acc += a.wind_kt;
        }
        let elapsed = start.elapsed();
        println!("{calls} real-grid calls to air() took {elapsed:?} ({:.1} ns/call, sum {acc:.0})", elapsed.as_nanos() as f64 / calls as f64);
    }
}
