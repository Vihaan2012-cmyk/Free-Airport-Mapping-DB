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
    pub airport_dir: Option<PathBuf>,
    pub msa_ft: Option<f64>,
    /// True when the touchdown zone elevation is a published survey rather than a
    /// reading off the terrain model.
    pub tdze_surveyed: bool,
    /// Both ends of the landing runway, threshold first.
    pub runway_ends: Option<((f64, f64), (f64, f64))>,
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
        out.push(Threshold {
            name: name.trim().to_uppercase(),
            runway: props["idrwy"].as_str().unwrap_or("").trim().to_uppercase(),
            lat,
            lon,
            bearing_deg: props["brngtrue"].as_f64(),
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
    let chosen = crate::output::approach::pick(&procedures, approach).ok_or_else(|| anyhow!("{icao} has no approach matching {}; try --list", approach.unwrap_or("any")))?;
    let index = procedures.procedures.iter().position(|p| std::ptr::eq(p, chosen)).unwrap_or(0);
    let mut procedures = procedures;
    let mut runway = procedures.procedures[index].runway.clone();

    let entry = idx.get(&icao);
    let field_elev_ft = entry.as_ref().and_then(|e| e.elevation_ft).unwrap_or(0.0);
    let airport_name = entry.as_ref().and_then(|e| e.name.clone());
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
    let wide = opts.wide_terrain.then(|| copernicus::patch(http, cache, procedures.lat, procedures.lon, 46.3, 600.0).ok()).flatten();
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
    let msa_ft = wide.as_ref().map(|w| {
        let ground = w.range().1 as f64 / 0.3048;
        let top = obstacles.iter().filter_map(|o| o.top_ft).fold(ground, f64::max);
        ((top + 1000.0) / 100.0).ceil() * 100.0
    });

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
        airport_dir,
        msa_ft,
        tdze_surveyed: surveyed.is_some(),
        runway_ends,
    })
}

/// The minimum for a prepared approach.
/// The circling minima for every aircraft category, where the procedure does not code
/// one of its own.
pub fn circling_table(setup: &Setup) -> Vec<(char, f64, Option<String>)> {
    crate::minima::CIRCLING_AREA
        .iter()
        .enumerate()
        .map(|(i, (letter, _, _))| {
            let (ft, what) = crate::minima::circling_minimum(&setup.patch, &setup.obstacles, (setup.procedures.lat, setup.procedures.lon), setup.field_elev_ft, i);
            (*letter, ft, what)
        })
        .collect()
}

pub fn estimate(setup: &Setup, kind: crate::minima::Approach) -> crate::minima::Estimate {
    let path = setup.path();
    // Where the procedure codes its own minimum, that is the answer, and the terrain is
    // only looked at so the chart can say what is under the approach.
    if let Some(coded) = crate::minima::coded_minimum(&setup.final_legs(), setup.tdze_ft) {
        let highest = crate::minima::highest_terrain(&setup.patch, &path, kind).unwrap_or(setup.tdze_ft);
        return crate::minima::from_coded(kind, setup.tdze_ft, coded, highest);
    }
    let mut terrain = crate::minima::limiting_terrain_limit(&setup.patch, &path, kind, setup.tdze_ft);
    // The chart prints the highest ground near the approach whether or not it set the
    // number, so it is looked up even when nothing was limited by it.
    if let Some(top) = crate::minima::highest_terrain(&setup.patch, &path, kind) {
        match &mut terrain {
            Some(t) => t.top_ft = t.top_ft.max(top),
            None => terrain = Some(crate::minima::Limit { top_ft: top, required_ft: f64::NEG_INFINITY, what: "terrain".to_string() }),
        }
    }
    // Circling is flown about the aerodrome rather than down the approach, so it is
    // worked out over the whole area instead. The smallest category is what the single
    // figure reports; the chart prints all four.
    if kind == crate::minima::Approach::Circling {
        let (ft, what) = crate::minima::circling_minimum(&setup.patch, &setup.obstacles, (setup.procedures.lat, setup.procedures.lon), setup.field_elev_ft, 0);
        let highest = terrain.as_ref().map(|t| t.top_ft).unwrap_or(setup.tdze_ft);
        let limit = crate::minima::Limit { top_ft: highest, required_ft: ft, what: what.clone().unwrap_or_else(|| "terrain".into()) };
        return crate::minima::estimate_with(kind, setup.field_elev_ft, Some(limit), None);
    }
    let obstacle = crate::minima::limiting_obstacle(&setup.obstacles, &path, kind, setup.tdze_ft);
    crate::minima::estimate_with(kind, setup.tdze_ft, terrain, obstacle)
}
