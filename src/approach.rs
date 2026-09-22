//! Everything an approach needs before anything can be said about it: the procedure, the
//! ground under it, what stands up out of that ground, and where the runway actually is.
//!
//! The chart and the accuracy audit both start here, so both are measuring the same
//! thing.

use crate::cache::Cache;
use crate::sources::copernicus::{self, Patch};
use crate::sources::http::Http;
use crate::sources::index::AirportIndex;
use crate::sources::msfs::procedures::{self, AirportProcedures};
use crate::sources::obstacles::{self, Obstacle};
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// How wide a net to cast. The chart wants the safe-altitude ring, which costs another
/// terrain read; the audit does not.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub wide_terrain: bool,
    pub quiet: bool,
    /// How far out to ask for obstacles. The chart wants them for its safe-altitude
    /// ring; a measurement only needs the ones the approach passes over.
    pub obstacle_radius_km: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options { wide_terrain: true, quiet: false, obstacle_radius_km: 46.0 }
    }
}

pub struct Setup {
    pub procedures: AirportProcedures,
    /// Which of the airport's procedures was chosen.
    pub procedure: usize,
    pub patch: Patch,
    pub wide: Option<Patch>,
    pub obstacles: Vec<Obstacle>,
    /// The landing threshold and the direction the runway points.
    pub threshold: Option<(f64, f64, Option<f64>)>,
    pub tdze_ft: f64,
    pub field_elev_ft: f64,
    pub airport_name: Option<String>,
    /// The other code the airport is known by, and the town and region it serves, which
    /// a chart heads its page with.
    pub airport_iata: Option<String>,
    pub airport_place: Option<String>,
    /// The other airports near this one, for the map to name.
    pub nearby_airports: Vec<(String, f64, f64)>,
    pub airport_dir: Option<PathBuf>,
    pub msa_ft: Option<f64>,
    /// The same, by quadrant, which is how a chart prints it.
    pub msa_sectors: Vec<crate::minima::Sector>,
    /// True when the touchdown zone elevation is a published survey rather than a
    /// reading off the terrain model.
    pub tdze_surveyed: bool,
    /// Both ends of the landing runway, threshold first.
    pub runway_ends: Option<((f64, f64), (f64, f64))>,
    /// What the landing runway offers: its size and its lights.
    pub runway_detail: Option<Threshold>,
}

impl Setup {
    pub fn procedure(&self) -> &procedures::Procedure {
        &self.procedures.procedures[self.procedure]
    }

    /// The final approach track, degrees true.
    ///
    /// Taken from the ground rather than from the file: the bearing from the last fix
    /// before the runway to the threshold. The file does carry a course with each leg,
    /// but it is stored as the reciprocal of the track flown, and a chart drawn on a
    /// guess about that would point the approach the wrong way. The runway's own bearing
    /// and then the runway number are the fallbacks.
    pub fn track_deg(&self) -> f64 {
        let p = self.procedure();
        let thr = self.threshold.map(|(lat, lon, _)| (lat, lon));
        let last_fix = p
            .transitions
            .iter()
            .filter(|t| t.part == "final")
            .flat_map(|t| t.legs.iter())
            .filter_map(|l| l.lat.zip(l.lon))
            .next_back();
        if let (Some((flat, flon)), Some((tlat, tlon))) = (last_fix, thr) {
            return bearing(flat, flon, tlat, tlon);
        }
        self.threshold
            .and_then(|(_, _, b)| b)
            .unwrap_or_else(|| p.runway.trim_end_matches(|c: char| c.is_alphabetic()).parse::<f64>().unwrap_or(0.0) * 10.0)
    }

    /// The course a chart would print, which is magnetic. The file's course is the
    /// reciprocal of the track, so it is turned round to whichever way agrees with the
    /// ground; without one, the true track is all we can offer.
    pub fn course_mag_deg(&self) -> Option<f64> {
        let track = self.track_deg();
        let stored = self
            .procedure()
            .transitions
            .iter()
            .filter(|t| t.part == "final")
            .flat_map(|t| t.legs.iter())
            .find_map(|l| l.course_deg)?;
        let flipped = (stored + 180.0) % 360.0;
        let diff = |a: f64| ((a - track + 540.0) % 360.0 - 180.0).abs();
        Some(if diff(stored) <= diff(flipped) { stored } else { flipped })
    }

    /// How far magnetic north is from true north here, in degrees, positive east.
    ///
    /// Not stated anywhere we can read, but the procedure gives its courses in magnetic
    /// and the ground gives the same courses in true, so the difference is the answer.
    pub fn variation_deg(&self) -> Option<f64> {
        // The runway is the sanity check: our own build gives its bearing on the ground,
        // and its number is that bearing in magnetic, to the nearest ten degrees.
        let from_runway = || -> Option<f64> {
            let (_, _, bearing) = self.threshold?;
            let number: f64 = self.procedure().runway.trim_end_matches(|c: char| c.is_alphabetic()).parse().ok()?;
            Some(((bearing? - number * 10.0 + 540.0) % 360.0) - 180.0)
        };
        let rough = from_runway();
        // Magnetic north is nowhere near thirty degrees off true outside the far north,
        // and a measurement that disagrees with the runway by more than the rounding of
        // a runway number has measured something else.
        let measured = self.procedures.magnetic_variation_deg.filter(|m| {
            m.abs() <= 30.0 && rough.map(|r| ((m - r + 540.0) % 360.0 - 180.0).abs() <= 20.0).unwrap_or(true)
        });
        measured.or(rough)
    }

    /// True where the approach cannot be landed off straight ahead.
    ///
    /// An approach whose final course is more than thirty degrees off the runway is not
    /// flown to a landing: the aircraft breaks off and manoeuvres visually, so only
    /// circling minima apply to it. Madeira's approach to runway 05 comes in on 211 to a
    /// runway pointing 050, and its published chart is titled a circling one.
    pub fn is_circling_only(&self) -> bool {
        // An approach named for a letter rather than a runway — a VOR-A, an NDB-B — serves
        // the aerodrome and not a runway. There is nothing to land straight off.
        let runway = &self.procedure().runway;
        if runway.len() == 1 && runway.chars().all(|c| c.is_ascii_alphabetic()) {
            return true;
        }
        let Some((lat, lon, Some(runway_bearing))) = self.threshold else { return false };
        // The track is taken from the last fix to the threshold, so a fix almost on top
        // of the threshold gives a bearing that means nothing. Only a long enough
        // baseline is worth judging by.
        let baseline = self
            .final_legs()
            .iter()
            .filter_map(|l| l.lat.zip(l.lon))
            .next_back()
            .map(|(flat, flon)| nm_apart((flat, flon), (lat, lon)))
            .unwrap_or(0.0);
        if baseline < 1.5 {
            return false;
        }
        // Well clear of the thirty degrees the rules allow, so that an offset localiser
        // or a loose reading is not mistaken for one that cannot be landed off.
        let offset = ((self.track_deg() - runway_bearing + 540.0) % 360.0 - 180.0).abs();
        offset > 45.0
    }

    /// Where the missed approach goes, as positions on the ground.
    pub fn missed_track(&self) -> Vec<(f64, f64)> {
        self.procedure()
            .transitions
            .iter()
            .filter(|t| t.part == "missed")
            .flat_map(|t| t.legs.iter())
            .filter_map(|l| l.lat.zip(l.lon))
            .collect()
    }

    /// The legs of the final approach segment.
    pub fn final_legs(&self) -> Vec<&procedures::Leg> {
        self.procedure().transitions.iter().filter(|t| t.part == "final").flat_map(|t| t.legs.iter()).collect()
    }

    /// How far a fix is from the threshold, in nautical miles, from where it actually
    /// is. The file's own distance is to the approach's navaid, not along the leg.
    pub fn distance_nm(&self, lat: f64, lon: f64) -> f64 {
        let (tlat, tlon) = self.threshold_point();
        let dn = (lat - tlat) * 60.0;
        let de = (lon - tlon) * 60.0 * tlat.to_radians().cos().max(0.05);
        (dn * dn + de * de).sqrt()
    }

    /// The ground track of the final approach, ending at the threshold. Built from the
    /// fixes where the data gives their positions, and a straight line in from the final
    /// track where it does not.
    pub fn path(&self) -> crate::minima::Path {
        let threshold = self.threshold_point();
        let mut points: Vec<(f64, f64)> = self
            .procedure()
            .transitions
            .iter()
            .filter(|t| t.part == "final")
            .flat_map(|t| t.legs.iter())
            .filter_map(|l| l.lat.zip(l.lon))
            .collect();
        // The last fix is the runway itself, which has no position of its own.
        points.retain(|p| nm_apart(*p, threshold) > 0.2);
        points.push(threshold);
        crate::minima::Path::new(points).unwrap_or_else(|| crate::minima::Path::straight(threshold, self.track_deg(), 12.0))
    }

    /// Every runway end of the airport, which is what a circling area is drawn from.
    /// Failing a build to read them from, the airport itself.
    pub fn runway_ends(&self) -> Vec<(f64, f64)> {
        let ends: Vec<(f64, f64)> = self.airport_dir.as_deref().map(|d| thresholds(d).into_iter().map(|t| (t.lat, t.lon)).collect()).unwrap_or_default();
        if ends.is_empty() {
            vec![(self.procedures.lat, self.procedures.lon)]
        } else {
            ends
        }
    }

    pub fn threshold_point(&self) -> (f64, f64) {
        self.threshold.map(|(lat, lon, _)| (lat, lon)).unwrap_or((self.procedures.lat, self.procedures.lon))
    }
}

/// The surveyed touchdown zone elevation of a runway end, where the country publishes
/// one. Only the United States does, in a file we already read; everywhere else the
/// terrain model answers.
fn surveyed_tdze_ft(http: &Http, cache: &Cache, icao: &str, runway: &str) -> Option<f64> {
    static TABLE: std::sync::OnceLock<std::collections::HashMap<String, std::collections::HashMap<String, f64>>> = std::sync::OnceLock::new();
    if !icao.starts_with('K') && !icao.starts_with('P') {
        return None;
    }
    let table = TABLE.get_or_init(|| match crate::sources::faa::load_tables(http, cache, None) {
        Ok(t) => crate::sources::faa::touchdown_zone_elevations(&t),
        Err(e) => {
            log::warn!("FAA runway elevations: {e:#}");
            Default::default()
        }
    });
    let want = runway.trim().to_uppercase();
    table.get(&icao.to_uppercase())?.get(&want).copied()
}

/// Miles between two points.
fn nm_apart(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dn = (b.0 - a.0) * 60.0;
    let de = (b.1 - a.1) * 60.0 * a.0.to_radians().cos().max(0.05);
    (dn * dn + de * de).sqrt()
}

/// Bearing from one point to another, degrees true. Over the few miles an approach
/// covers, flat trigonometry is as good as the great circle.
pub fn bearing(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    let dn = to_lat - from_lat;
    let de = (to_lon - from_lon) * from_lat.to_radians().cos().max(0.05);
    (de.atan2(dn).to_degrees() + 360.0) % 360.0
}

/// A landing threshold as our own build of the airport records it.
#[derive(Debug, Clone)]
pub struct Threshold {
    /// The end's own name, "27L".
    pub name: String,
    /// The runway it belongs to, "09R/27L", which is how the other end is found.
    pub runway: String,
    pub lat: f64,
    pub lon: f64,
    pub bearing_deg: Option<f64>,
    /// Landing distance available and runway width, metres.
    pub landing_m: Option<f64>,
    pub width_m: Option<f64>,
    /// What this end has to guide an approach by, in the words a chart uses.
    pub lighting: Vec<&'static str>,
}

/// Every landing threshold our own build of an airport knows.
pub fn thresholds(dir: &Path) -> Vec<Threshold> {
    let Ok(text) = std::fs::read_to_string(dir.join("runwaythreshold.geojson")) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let mut out = Vec::new();
    for f in v["features"].as_array().into_iter().flatten() {
        let props = &f["properties"];
        let (Some(name), Some(co)) = (props["idthr"].as_str(), f["geometry"]["coordinates"].as_array()) else { continue };
        let (Some(lon), Some(lat)) = (co.first().and_then(|v| v.as_f64()), co.get(1).and_then(|v| v.as_f64())) else { continue };
        let mut lighting = Vec::new();
        match props["vasis"].as_i64() {
            Some(1) => lighting.push("PAPI"),
            Some(2) => lighting.push("VASI"),
            Some(3) => lighting.push("APAPI"),
            _ => {}
        }
        if props["tohlight"].as_i64().unwrap_or(0) != 0 {
            lighting.push("ALS");
        }
        if props["tdzlight"].as_bool().unwrap_or(false) {
            lighting.push("TDZ");
        }
        if props["reil"].as_i64().unwrap_or(0) != 0 {
            lighting.push("REIL");
        }
        out.push(Threshold {
            name: name.trim().to_uppercase(),
            runway: props["idrwy"].as_str().unwrap_or("").trim().to_uppercase(),
            lat,
            lon,
            bearing_deg: props["brngtrue"].as_f64(),
            landing_m: props["lda"].as_f64(),
            width_m: props["width"].as_f64(),
            lighting,
        });
    }
    out
}

/// The landing threshold of one runway. A chart's final track and its touchdown zone
/// elevation both hang off this, so it is worth reading when we have it.
pub fn threshold_of(dir: &Path, runway: &str) -> Option<(f64, f64, Option<f64>)> {
    let want = runway.trim().to_uppercase();
    thresholds(dir).into_iter().find(|t| t.name == want).map(|t| (t.lat, t.lon, t.bearing_deg))
}

/// The runway an approach actually lands on, from the ones our own build knows.
///
/// An approach that names no runway in its legs falls back to the one coded in its
/// header, and that name is not always one the airport has: a designator may be there
/// that the airport does not use, or it may match a short strip alongside the real
/// runway. Both ends and their length are known here, so the longest runway with the
/// right number wins, which is the one an instrument approach serves.
fn choose_runway(dir: &Path, wanted: &str) -> Option<(String, ((f64, f64), (f64, f64)))> {
    let want = wanted.trim().to_uppercase();
    let number: String = want.chars().take_while(|c| c.is_ascii_digit()).collect();
    let list = thresholds(dir);
    let pair = |t: &Threshold| {
        let far = list.iter().find(|o| o.runway == t.runway && o.name != t.name)?;
        Some((t.name.clone(), ((t.lat, t.lon), (far.lat, far.lon))))
    };
    let length_nm = |ends: &((f64, f64), (f64, f64))| nm_apart(ends.0, ends.1);
    // The name we were given wins where the airport has it: at a parallel-runway airport
    // 27L and 27R are both real, and preferring one over the other would put the wrong
    // runway on the chart.
    let exact = list.iter().find(|t| t.name == want).and_then(pair);
    // Unless it is too short to be the one an instrument approach serves. Some builds
    // carry a grass strip or a taxiway numbered like the runway beside it.
    const TOO_SHORT_NM: f64 = 0.43; // about 800 m
    if let Some(found) = &exact {
        if length_nm(&found.1) >= TOO_SHORT_NM {
            return exact;
        }
    }
    let mut candidates: Vec<(String, ((f64, f64), (f64, f64)))> = list
        .iter()
        .filter(|t| !number.is_empty() && t.name.chars().take_while(|c| c.is_ascii_digit()).collect::<String>() == number)
        .filter_map(pair)
        .collect();
    candidates.sort_by(|a, b| length_nm(&b.1).total_cmp(&length_nm(&a.1)));
    candidates.into_iter().next().or(exact)
}

/// Where our own build of an airport is, when it has been built.
/// Where our own build of an airport is, when it has been built.
pub fn built_dir(icao: &str) -> Option<PathBuf> {
    let dir = crate::bridge::settings::Settings::load().map(|s| s.airports_dir()).unwrap_or_else(|| PathBuf::from("out")).join(icao);
    dir.join("manifest.json").is_file().then_some(dir)
}

/// Read everything an approach needs. `approach` names one, as `--approach` takes it.
pub fn prepare(http: &Http, cache: &Cache, idx: &AirportIndex, icao: &str, approach: Option<&str>, opts: Options) -> Result<Setup> {
    let icao = icao.to_uppercase();
    let Some(procedures) = procedures::find(&icao)? else {
        return Err(anyhow!("{icao} has no procedures in the simulator's navigation data (is a simulator installed?)"));
    };
    prepare_from(procedures, http, cache, idx, approach, opts)
}

/// The same, for an airport whose procedures have already been read.
pub fn prepare_from(procedures: AirportProcedures, http: &Http, cache: &Cache, idx: &AirportIndex, approach: Option<&str>, opts: Options) -> Result<Setup> {
    let icao = procedures.icao.to_uppercase();
    let chosen = crate::output::approach::pick(&procedures, approach).ok_or_else(|| anyhow!("{icao} has no approach matching {}; try --list", approach.unwrap_or("any")))?;
    let index = procedures.procedures.iter().position(|p| std::ptr::eq(p, chosen)).unwrap_or(0);
    let mut procedures = procedures;
    let mut runway = procedures.procedures[index].runway.clone();

    let entry = idx.get(&icao);
    let field_elev_ft = entry.as_ref().and_then(|e| e.elevation_ft).unwrap_or(0.0);
    let airport_name = entry.as_ref().and_then(|e| e.name.clone());
    let airport_iata = entry.as_ref().and_then(|e| e.iata.clone()).filter(|s| s.len() == 3);
    // The airports round about, which a chart names so that a reader looking down at a
    // runway knows whether it is the one he is going to. Only those within the piece of
    // country a plan view covers, nearest first.
    let cos = procedures.lat.to_radians().cos().max(0.05);
    let mut nearby: Vec<(f64, String, f64, f64)> = idx
        .by_icao
        .iter()
        .filter(|(other, _)| other.as_str() != icao)
        .filter_map(|(other, e)| {
            let nm = ((e.lat - procedures.lat) * 60.0).hypot((e.lon - procedures.lon) * 60.0 * cos);
            (nm <= 14.0).then(|| (nm, crate::output::approach::shorten_place(e.name.as_deref().unwrap_or(other)), e.lat, e.lon))
        })
        .collect();
    nearby.sort_by(|a, b| a.0.total_cmp(&b.0));
    let nearby_airports: Vec<(String, f64, f64)> = nearby.into_iter().take(6).map(|(_, name, lat, lon)| (name, lat, lon)).collect();
    let airport_place = entry.as_ref().and_then(|e| match (e.city.as_deref(), e.region.as_deref()) {
        (Some(city), Some(region)) => {
            // The region is given as a country-qualified code; a chart prints the part
            // that names the state.
            let state = region.rsplit('-').next().unwrap_or(region);
            Some(format!("{city}, {state}"))
        }
        (Some(city), None) => Some(city.to_string()),
        _ => None,
    });
    if !opts.quiet {
        crate::term::info(&format!("{icao}: RW{runway} approach, field elevation {field_elev_ft:.0} ft"));
    }

    let airport_dir = built_dir(&icao);
    // The navigation data's runway is checked against the runways the airport has.
    let chosen_runway = airport_dir.as_deref().and_then(|d| choose_runway(d, &runway));
    if let Some((real, _)) = &chosen_runway {
        if *real != runway {
            if !opts.quiet {
                crate::term::info(&format!("{icao}: the navigation data calls this runway {runway}; the airport's is {real}"));
            }
            runway = real.clone();
            procedures.procedures[index].runway = real.clone();
        }
    }
    if !opts.quiet {
        match &airport_dir {
            Some(d) => crate::term::info(&format!("{icao}: drawing the airport from {}", d.display())),
            None => crate::term::warn(&format!("{icao} is not built yet, so only the runway is drawn; `amdbgen build {icao}` first for the full layout")),
        }
    }
    let threshold = airport_dir.as_deref().and_then(|d| threshold_of(d, &runway));

    let runway_ends = chosen_runway.map(|(_, ends)| ends);
    let runway_detail = airport_dir.as_deref().and_then(|d| thresholds(d).into_iter().find(|t| t.name == runway.to_uppercase()));

    let patch = copernicus::patch(http, cache, procedures.lat, procedures.lon, 14.0, 120.0)?;
    // A minimum is measured from the touchdown zone, not from the airport's own
    // elevation: at a large airport the two are tens of feet apart.
    let surveyed = surveyed_tdze_ft(http, cache, &icao, &runway);
    if let (false, Some(ft)) = (opts.quiet, surveyed) {
        crate::term::info(&format!("{icao}: touchdown zone {ft:.0} ft for RW{runway}, surveyed (FAA NASR)"));
    }
    let tdze_ft = match (surveyed, threshold) {
        (Some(ft), _) => ft,
        (None, Some((lat, lon, bearing))) => {
            // At the model's own resolution: a touchdown zone is only 900 m long.
            let fine = copernicus::patch(http, cache, lat, lon, 2.0, 30.0).ok();
            let tdz = fine
                .as_ref()
                .and_then(|f| bearing.and_then(|b| f.touchdown_zone_ft(lat, lon, b)).or_else(|| f.height_at(lat, lon).map(|m| (m / 0.3048).round())));
            match tdz {
                Some(ft) => {
                    if !opts.quiet {
                        crate::term::info(&format!("{icao}: touchdown zone {ft:.0} ft over the first 3,000 ft of RW{runway} ({field_elev_ft:.0} ft at the airport reference point)"));
                    }
                    ft
                }
                None => field_elev_ft,
            }
        }
        (None, None) => field_elev_ft,
    };

    let mirrors = crate::pipeline::default_mirrors();
    let mut obstacles = obstacles::around(http, cache, &icao, procedures.lat, procedures.lon, opts.obstacle_radius_km, &mirrors).unwrap_or_default();
    // The safe altitude ring is the one place a smoothed reading will not do: it is
    // summits that set it, and a coarse copy of the ground rounds summits off.
    let wide = opts.wide_terrain.then(|| copernicus::patch(http, cache, procedures.lat, procedures.lon, 46.3, 120.0).ok()).flatten();
    if let Some(w) = &wide {
        obstacles::resolve_tops(&mut obstacles, w);
    }
    obstacles::resolve_tops(&mut obstacles, &patch);
    obstacles.retain(|o| o.top_ft.is_some());
    if let (false, Some(o)) = (opts.quiet, obstacles.first()) {
        crate::term::info(&format!("{icao}: {} obstacles from {}, highest {:.0} ft", obstacles.len(), o.source, o.top_ft.unwrap_or(0.0)));
    }
    // The minimum safe altitude a chart prints in its corner: the highest thing within
    // 25 NM, plus a thousand feet, rounded up to the next hundred.
    // The variation is measured from the fixes; without it the sectors are drawn on true
    // bearings, which is close enough to put a label in the right quadrant.
    let variation = procedures.magnetic_variation_deg.unwrap_or(0.0);
    let msa_sectors = wide
        .as_ref()
        .map(|w| crate::minima::safe_altitude_sectors(w, &obstacles, (procedures.lat, procedures.lon), 25.0, variation, field_elev_ft))
        .unwrap_or_default();
    let msa_ft = msa_sectors.iter().map(|s| s.altitude_ft).fold(f64::NEG_INFINITY, f64::max);
    let msa_ft = msa_ft.is_finite().then_some(msa_ft);

    Ok(Setup {
        procedures,
        procedure: index,
        patch,
        wide,
        obstacles,
        threshold,
        tdze_ft,
        field_elev_ft,
        airport_name,
        airport_iata,
        airport_place,
        nearby_airports,
        msa_ft,
        msa_sectors,
        tdze_surveyed: surveyed.is_some(),
        runway_ends,
        runway_detail,
        airport_dir,
    })
}

impl Setup {
    /// How far the final approach fix is from the threshold, which is how long the
    /// segment the minimum is flown over is.
    ///
    /// The fix is the one the data marks as final, or failing that the last fix on the
    /// final that carries an altitude. A figure outside what a final segment can be is
    /// not believed: a segment is a few miles long, never half a mile and never twenty.
    pub fn final_segment_nm(&self) -> Option<f64> {
        let legs = self.final_legs();
        let marked = legs
            .iter()
            .rev()
            .find(|l| l.role == Some(crate::sources::msfs::procedures::FixRole::Final))
            .and_then(|l| l.lat.zip(l.lon));
        let fallback = || {
            legs.iter()
                .filter(|l| l.altitude_ft.is_some() && !l.fix.starts_with("RW"))
                .filter_map(|l| l.lat.zip(l.lon))
                .next_back()
        };
        let (lat, lon) = marked.or_else(fallback)?;
        let nm = self.distance_nm(lat, lon);
        (2.0..=15.0).contains(&nm).then_some(nm)
    }
}

/// The minimum for a prepared approach.
/// What the ground looks like around an approach, for working out where an estimate
/// goes wrong. Everything here is a height above sea level in feet.
#[derive(Debug, Clone, Default)]
pub struct Survey {
    /// The highest ground in the corridor the minimum is worked out from.
    pub corridor_terrain_ft: f64,
    /// The highest obstacle in that corridor.
    pub corridor_obstacle_ft: f64,
    /// The highest obstacle and the highest ground within a mile and a third of the
    /// airport, within two and a third, and within five: the circling areas for the
    /// aircraft categories, and a wider look for comparison.
    pub near_obstacle_ft: f64,
    pub near_terrain_ft: f64,
    pub ring13_ft: f64,
    pub ring23_ft: f64,
    pub ring50_ft: f64,
    /// How far out the final approach fix is, in miles.
    pub faf_nm: f64,
}

/// Look at everything around the approach, whether or not it sets the minimum.
pub fn survey(setup: &Setup, kind: crate::minima::Approach) -> Survey {
    let path = setup.path();
    let (airport_lat, airport_lon) = (setup.procedures.lat, setup.procedures.lon);
    let segment = setup.final_segment_nm();
    let mut s = Survey {
        corridor_terrain_ft: crate::minima::highest_terrain(&setup.patch, &path, kind, segment).unwrap_or(f64::NAN),
        ..Default::default()
    };
    s.corridor_obstacle_ft = crate::minima::limiting_obstacle(&setup.obstacles, &path, kind, setup.tdze_ft, segment)
        .map(|l| l.top_ft)
        .unwrap_or(f64::NAN);
    let near = |lat: f64, lon: f64| {
        let dn = (lat - airport_lat) * 60.0;
        let de = (lon - airport_lon) * 60.0 * airport_lat.to_radians().cos().max(0.05);
        dn.hypot(de) <= 5.0
    };
    s.near_obstacle_ft = setup
        .obstacles
        .iter()
        .filter(|o| near(o.lat, o.lon))
        .filter_map(|o| o.top_ft)
        .fold(f64::NAN, f64::max);
    let mut highest = f64::NAN;
    for row in 0..setup.patch.height {
        for col in 0..setup.patch.width {
            let h = setup.patch.at(row, col);
            if !h.is_finite() {
                continue;
            }
            let (lat, lon) = setup.patch.position(row, col);
            if near(lat, lon) {
                highest = highest.max(h as f64 / 0.3048);
            }
        }
    }
    s.near_terrain_ft = highest;
    // The highest of anything at all within each ring.
    for (radius, slot) in [(1.3, 0), (2.3, 1), (5.0, 2)] {
        let mut top = f64::NAN;
        let inside = |lat: f64, lon: f64| {
            let dn = (lat - airport_lat) * 60.0;
            let de = (lon - airport_lon) * 60.0 * airport_lat.to_radians().cos().max(0.05);
            dn.hypot(de) <= radius
        };
        for o in setup.obstacles.iter().filter(|o| inside(o.lat, o.lon)) {
            if let Some(t) = o.top_ft {
                top = if top.is_nan() { t } else { top.max(t) };
            }
        }
        for row in 0..setup.patch.height {
            for col in 0..setup.patch.width {
                let h = setup.patch.at(row, col);
                if !h.is_finite() {
                    continue;
                }
                let (lat, lon) = setup.patch.position(row, col);
                if inside(lat, lon) {
                    let ft = h as f64 / 0.3048;
                    top = if top.is_nan() { ft } else { top.max(ft) };
                }
            }
        }
        match slot {
            0 => s.ring13_ft = top,
            1 => s.ring23_ft = top,
            _ => s.ring50_ft = top,
        }
    }
    s.faf_nm = segment.unwrap_or(f64::NAN);
    s
}

/// The circling minima for every aircraft category, where the procedure does not code
/// one of its own.
pub fn circling_table(setup: &Setup) -> Vec<(char, f64, Option<String>)> {
    crate::minima::CIRCLING_AREA
        .iter()
        .enumerate()
        .map(|(i, (letter, _, _))| {
            let (ft, what) = crate::minima::circling_minimum(&setup.patch, &setup.obstacles, &setup.runway_ends(), setup.field_elev_ft, i);
            (*letter, ft, what)
        })
        .collect()
}

pub fn estimate(setup: &Setup, kind: crate::minima::Approach) -> crate::minima::Estimate {
    // An approach that cannot be landed off straight ahead has circling minima whatever
    // it is flown on.
    let kind = if setup.is_circling_only() && kind != crate::minima::Approach::Circling { crate::minima::Approach::Circling } else { kind };
    let path = setup.path();
    // How long the segment the minimum is flown over is, which is how far out it is
    // worth looking.
    let segment = setup.final_segment_nm();
    // What the procedure codes at its missed approach point, which is a floor rather
    // than the answer: see `coded_minimum` for why.
    let coded = crate::minima::coded_minimum(&setup.final_legs(), setup.tdze_ft);
    let mut terrain = crate::minima::limiting_terrain_limit(&setup.patch, &path, kind, setup.tdze_ft, segment);
    // The chart prints the highest ground near the approach whether or not it set the
    // number, so it is looked up even when nothing was limited by it.
    if let Some(top) = crate::minima::highest_terrain(&setup.patch, &path, kind, segment) {
        match &mut terrain {
            Some(t) => t.top_ft = t.top_ft.max(top),
            None => terrain = Some(crate::minima::Limit { top_ft: top, required_ft: f64::NEG_INFINITY, what: "terrain".to_string() }),
        }
    }
    // Circling is flown about the aerodrome rather than down the approach, so it is
    // worked out over the whole area instead. The smallest category is what the single
    // figure reports; the chart prints all four.
    if kind == crate::minima::Approach::Circling {
        let (ft, what) = crate::minima::circling_minimum(&setup.patch, &setup.obstacles, &setup.runway_ends(), setup.field_elev_ft, 0);
        let highest = terrain.as_ref().map(|t| t.top_ft).unwrap_or(setup.tdze_ft);
        let limit = crate::minima::Limit { top_ft: highest, required_ft: ft, what: what.clone().unwrap_or_else(|| "terrain".into()) };
        return crate::minima::estimate_with(kind, setup.field_elev_ft, Some(limit), None);
    }
    let obstacle = crate::minima::limiting_obstacle(&setup.obstacles, &path, kind, setup.tdze_ft, segment);
    let worked_out = crate::minima::estimate_with(kind, setup.tdze_ft, terrain.clone(), obstacle);
    let mut answer = match coded {
        Some(ft) if ft > worked_out.altitude_ft => {
            let highest = terrain.map(|t| t.top_ft).unwrap_or(setup.tdze_ft);
            crate::minima::from_coded(kind, setup.tdze_ft, ft, highest)
        }
        _ => worked_out,
    };
    // Going around has to clear what lies beyond the runway. Usually that asks for a
    // steeper climb rather than a higher minimum; where it asks for more than a chart
    // would, the minimum goes up.
    if let Some(missed) = missed_approach(setup, &answer) {
        if let Some(raised) = missed.raises_to_ft.filter(|ft| *ft > answer.altitude_ft) {
            answer.altitude_ft = raised;
            answer.height_ft = raised - setup.tdze_ft;
            answer.limited_by = crate::minima::LimitedBy::Obstacle;
            answer.obstacle = Some(format!("{} on the missed approach", missed.what));
            answer.obstacle_top_ft = Some(raised);
            answer.reliable = false;
        }
    }
    answer
}

/// What the state's own chart publishes for this approach, where it publishes one.
///
/// Only the United States, for now: the FAA gives every approach chart away as a PDF
/// whose minima band is text. Everywhere else the minimum is still worked out.
pub fn published(
    http: &crate::sources::http::Http,
    cache: &crate::cache::Cache,
    setup: &Setup,
    kind: crate::minima::Approach,
) -> Option<crate::sources::dtpp::Published> {
    let procedure = setup.procedure();
    let circling = kind == crate::minima::Approach::Circling || setup.is_circling_only();
    let line = if circling {
        crate::sources::dtpp::Line::Circling
    } else {
        crate::sources::dtpp::Line::StraightIn(procedure.approach_type?)
    };
    // A circling minimum is quoted above the aerodrome; a straight-in one above the
    // touchdown zone. Which it is decides what the reading is checked against.
    let against = if circling { setup.field_elev_ft } else { setup.tdze_ft };
    crate::sources::dtpp::published(http, cache, &setup.procedures.icao, line, &procedure.runway, procedure.suffix, against)
}

/// The minimum a chart should print: what the state publishes where it publishes one, and
/// what we work out where it does not.
pub fn with_published(est: crate::minima::Estimate, published: Option<&crate::sources::dtpp::Published>) -> crate::minima::Estimate {
    match published {
        Some(p) => crate::minima::from_published(est.approach, p.altitude_ft, p.height_ft, &p.chart, est.highest_terrain_ft),
        None => est,
    }
}

/// The circling minima by aircraft category, published where they are published.
///
/// A row that prints one figure means every category shares it; a row that prints four
/// means they differ, which on a circling line is the usual case.
pub fn circling_from(published: Option<&crate::sources::dtpp::Published>, worked_out: Vec<(char, f64)>) -> Vec<(char, f64)> {
    match published.filter(|p| !p.circling.is_empty()) {
        Some(p) => ['A', 'B', 'C', 'D']
            .iter()
            .enumerate()
            .filter_map(|(i, letter)| p.circling.get(i).or_else(|| p.circling.last()).map(|c| (*letter, c.altitude_ft)))
            .collect(),
        None => worked_out,
    }
}

/// The minimum flown down the localiser alone, where the same chart publishes one.
///
/// An ILS chart carries two straight-in lines: the one flown down the glidepath, and a
/// higher one for when the glidepath is out. An aircraft that loses it on the way in
/// needs the second, so the chart prints both.
pub fn published_localiser(
    http: &crate::sources::http::Http,
    cache: &crate::cache::Cache,
    setup: &Setup,
    kind: crate::minima::Approach,
) -> Option<crate::sources::dtpp::Published> {
    if kind != crate::minima::Approach::PrecisionCat1 || setup.is_circling_only() {
        return None;
    }
    let procedure = setup.procedure();
    let line = crate::sources::dtpp::Line::StraightIn(crate::sources::msfs::procedures::ApproachType::Localiser);
    crate::sources::dtpp::published(http, cache, &setup.procedures.icao, line, &procedure.runway, procedure.suffix, setup.tdze_ft)
}

/// The airway a published missed approach joins, where it names one.
///
/// A chart writes it in a black flag on the track: V-44 out of Kennedy. It is in the
/// sentence the state publishes and nowhere else — "heading 099 and V44 to DPK VOR/DME
/// and hold" — so it is picked out of that by its shape: a letter for the sort of airway,
/// then a number.
pub fn missed_airway(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == ',').find_map(|word| {
        let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        let bare = word.replace('-', "");
        let mut chars = bare.chars();
        let first = chars.next()?;
        let rest: String = chars.collect();
        let sort = matches!(first, 'V' | 'J' | 'Q' | 'T' | 'B' | 'A' | 'G' | 'L' | 'M' | 'N' | 'R' | 'W' | 'Y' | 'Z');
        let numbered = !rest.is_empty() && rest.len() <= 3 && rest.chars().all(|c| c.is_ascii_digit());
        (sort && numbered).then(|| {
            // Written with a hyphen, the way a chart writes it.
            format!("{first}-{rest}")
        })
    })
}

/// What the missed approach from a given minimum asks for.
pub fn missed_approach(setup: &Setup, est: &crate::minima::Estimate) -> Option<crate::minima::MissedApproach> {
    crate::minima::missed_approach(&setup.patch, &setup.obstacles, &setup.path(), &setup.missed_track(), est.altitude_ft)
}
