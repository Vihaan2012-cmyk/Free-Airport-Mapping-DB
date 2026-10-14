//! Charts drawn ahead into each airport's own folder, served to the flight bags as they are:
//!
//! * `<airports>/<ICAO>/charts/`       the FAA's CIFP charts, for American airports
//! * `<airports>/<ICAO>/charts-NNNN/`  drawn from a navigation data set of AIRAC cycle NNNN
//!   (`amdbgen navdata-charts --subfolder charts-NNNN`)
//! * `<airports>/<ICAO>/chart.pdf`     the airport diagram
//!
//! A set may be packed into one file, `<set>.pack` (see `output::chart_pack`); its charts are
//! read through it by the paths they had in the folder.
//!
//! A flight bag is shown the set of its aircraft's own cycle: the cycle of the aircraft's
//! navigation database, or the simulator's for an aircraft that has none. An aircraft on
//! any other cycle, or one the bridge cannot tell (a flight bag reached by redirecting its
//! host rather than patched), is shown the newest set drawn from an aircraft's database.
//!
//! The charts are PDFs and a flight bag wants pictures, so each page is rendered when it is
//! asked for; for the night theme its lightness is turned over and its colours kept.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// A chart filed in an airport's folder, as a flight bag's list names it.
pub struct Filed {
    pub id: String,
    pub path: PathBuf,
    pub category: &'static str,
    pub type_code: &'static str,
    pub precision: bool,
    pub index_number: String,
    pub name: String,
    pub procedures: Vec<String>,
    pub runways: Vec<String>,
}

fn is_pdf(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// The chart sets in an airport's folder: each subfolder called `charts` or `charts-…` with
/// a PDF in it, or packed into `<set>.pack`.
pub fn sets(airport: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(airport) else { return Vec::new() };
    let is_set = |n: &str| n == "charts" || n.starts_with("charts-");
    let mut out: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            match name.strip_suffix(".pack") {
                Some(set) if is_set(set) && e.path().is_file() => Some(set.to_string()),
                _ if is_set(&name) && e.path().is_dir() => Some(name),
                _ => None,
            }
        })
        .filter(|set| crate::output::chart_pack::names(airport, set).iter().any(|n| is_pdf(Path::new(n))))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The set an aircraft on `cycle` is shown, `current` being the cycle of the simulator's own
/// navigation data.
///
/// Its own cycle's set first. An aircraft on the current cycle at an airport with no set of
/// that cycle drawn (an American one, whose charts are the FAA's) is shown the FAA's, which
/// are current too. Any other aircraft is shown the newest set drawn from an aircraft
/// database, which is nearer its own data than the simulator's.
pub fn pick(sets: &[String], cycle: Option<&str>, current: Option<&str>) -> Option<String> {
    let has = |s: &str| sets.iter().any(|x| x == s);
    if let Some(c) = cycle {
        let own = format!("charts-{c}");
        if has(&own) {
            return Some(own);
        }
    }
    let mut order: Vec<String> = Vec::new();
    if cycle.is_some() && cycle == current {
        order.push("charts".to_string());
    }
    let mut databases: Vec<&String> = sets.iter().filter(|s| s.strip_prefix("charts-").is_some_and(|c| Some(c) != current)).collect();
    databases.sort();
    order.extend(databases.into_iter().rev().cloned());
    order.push("charts".to_string());
    order.extend(current.map(|c| format!("charts-{c}")));
    order.into_iter().find(|s| has(s)).or_else(|| sets.first().cloned())
}

/// The AIRAC cycle an aircraft flies on, by the name of its Community package: that of the
/// navigation database it ships (PMDG's `Config/NavData`, the `Navigraph/BundledData` of
/// the A350 and the A220), or the simulator's when it ships none, as FlyByWire's do. None
/// for a package the bridge cannot find.
pub fn aircraft_cycle(aircraft: &str) -> Option<String> {
    static SEEN: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if let Some(c) = seen.lock().unwrap().get(aircraft) {
        return c.clone();
    }
    // A package name, never a path.
    if aircraft.is_empty() || aircraft.contains("..") || !aircraft.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
        return None;
    }
    let found = (|| {
        for sim in crate::bridge::desktop::detect_sims() {
            let pkg = sim.community.join(aircraft);
            if !pkg.is_dir() {
                continue;
            }
            for rest in [["Config", "NavData"], ["Navigraph", "BundledData"]] {
                let Ok(files) = std::fs::read_dir(pkg.join(rest[0]).join(rest[1])) else { continue };
                let mut cycles: Vec<String> = files
                    .flatten()
                    .map(|f| f.path())
                    .filter(|p| p.extension().is_some_and(|e| ["s3db", "db3", "sqlite"].iter().any(|x| e.eq_ignore_ascii_case(x))))
                    .filter_map(|p| crate::sources::navdata::database_cycle(&p))
                    .collect();
                cycles.sort();
                if let Some(c) = cycles.pop() {
                    return Some(c);
                }
            }
            // No database of its own: it flies on the simulator's.
            return crate::sources::navdata::sim_cycle();
        }
        None
    })();
    seen.lock().unwrap().insert(aircraft.to_string(), found.clone());
    found
}

/// A file name as part of an id, which goes in an address: letters and digits, the rest `_`.
fn slug(stem: &str) -> String {
    stem.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// An approach's runway, out of its name: `ILS Z RWY 04R` is 04R, a circling `VOR-A` is A.
fn runway_of(name: &str) -> Option<String> {
    if let Some(at) = name.find("RWY ") {
        let rw: String = name[at + 4..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        return (!rw.is_empty()).then_some(rw);
    }
    name.rsplit_once('-').map(|(_, l)| l.trim()).filter(|l| l.len() == 1 && l.chars().all(|c| c.is_ascii_alphanumeric())).map(str::to_string)
}

/// Navigraph's chart type for an approach, from what its name says it is.
fn approach_type(name: &str) -> (&'static str, bool) {
    let n = name.to_ascii_uppercase();
    match () {
        _ if n.starts_with("ILS") => ("01", true),
        _ if n.starts_with("LOC BC") => ("1E", false),
        _ if n.starts_with("LOC") => ("1D", false),
        _ if n.starts_with("LDA") => ("1F", false),
        _ if n.starts_with("VOR") => ("03", false),
        _ if n.starts_with("NDB") => ("06", false),
        _ => ("1L", false),
    }
}

/// Every chart of one set, filed as a chart binder files them: arrivals, departures, the
/// airport diagram, then the approaches.
pub fn list(icao: &str, airport: &Path, set: &str) -> Vec<Filed> {
    let mut stems: Vec<(String, PathBuf)> = crate::output::chart_pack::names(airport, set)
        .into_iter()
        .map(|n| airport.join(set).join(n))
        .filter(|p| is_pdf(p))
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().to_string(), p)))
        .collect();
    stems.sort();
    let letter = |n: usize| if n == 0 { String::new() } else { ((b'A' + (n as u8 - 1).min(25)) as char).to_string() };
    let mut out = Vec::new();
    for (prefix, category, type_code, series) in [("STAR ", "ARR", "J", "10-2"), ("SID ", "DEP", "G", "10-3")] {
        for (n, (stem, path)) in stems.iter().filter(|(s, _)| s.starts_with(prefix)).enumerate() {
            let procedures: Vec<String> = stem[prefix.len()..].split(" - ").map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
            out.push(Filed {
                id: format!("{icao}~{set}~{}", slug(stem)),
                path: path.clone(),
                category,
                type_code,
                precision: false,
                index_number: format!("{series}{}", letter(n)),
                name: procedures.join(" / "),
                procedures,
                runways: Vec::new(),
            });
        }
    }
    let approaches: Vec<&(String, PathBuf)> = stems.iter().filter(|(s, _)| !s.starts_with("SID ") && !s.starts_with("STAR ")).collect();
    let mut runways: Vec<String> = approaches.iter().filter_map(|(s, _)| runway_of(s)).filter(|r| r.len() > 1).collect();
    runways.sort();
    runways.dedup();
    let diagram = airport.join("chart.pdf");
    if diagram.is_file() {
        out.push(Filed {
            id: format!("{icao}~APT"),
            path: diagram,
            category: "APT",
            type_code: "AM",
            precision: false,
            index_number: "10-9".to_string(),
            name: "AIRPORT DIAGRAM".to_string(),
            procedures: Vec::new(),
            runways,
        });
    }
    for (n, (stem, path)) in approaches.into_iter().enumerate() {
        let (type_code, precision) = approach_type(stem);
        out.push(Filed {
            id: format!("{icao}~{set}~{}", slug(stem)),
            path: path.clone(),
            category: "APP",
            type_code,
            precision,
            index_number: format!("11-{}", n + 1),
            name: stem.clone(),
            procedures: vec![stem.clone()],
            runways: runway_of(stem).into_iter().collect(),
        });
    }
    out
}

/// The file a chart's id names.
pub fn path_of(icao: &str, airport: &Path, id: &str) -> Option<PathBuf> {
    if id == format!("{icao}~APT") {
        return Some(airport.join("chart.pdf")).filter(|p| p.is_file());
    }
    let mut parts = id.splitn(3, '~');
    let (_, set, wanted) = (parts.next()?, parts.next()?, parts.next()?);
    if set != "charts" && !set.starts_with("charts-") || set.contains("..") {
        return None;
    }
    crate::output::chart_pack::names(airport, set).into_iter().map(|n| airport.join(set).join(n)).find(|p| is_pdf(p) && p.file_stem().is_some_and(|s| slug(&s.to_string_lossy()) == wanted))
}

/// When a set came into force, as its `cycle.txt` says: `01 OCT 2026`.
pub fn in_force(airport: &Path, set: &str) -> String {
    let text = crate::output::chart_pack::read(&airport.join(set).join("cycle.txt")).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();
    let Some(at) = text.find("in force ") else { return String::new() };
    let rest = text[at + "in force ".len()..].trim_start_matches("from ");
    rest.split([')', '\r', '\n']).next().unwrap_or("").split(" to ").next().unwrap_or("").trim().to_uppercase()
}

/// Where a chart's plan view lies in its picture at `scale`, and the ground under it, from
/// the `<chart>.geo.json` written beside the PDF: the `planview` a flight bag puts the
/// aircraft on the chart by. The corners are bottom-left then top-right, in pixels from the
/// picture's top left and in longitude and latitude.
pub fn georef(pdf: &Path, scale: f32) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(&crate::output::chart_pack::read(&pdf.with_extension("geo.json"))?).ok()?;
    let num = |key: &str, i: usize| v.get(key)?.get(i)?.as_f64();
    let (page_h, x, y, w, h) = (num("page", 1)?, num("plan", 0)?, num("plan", 1)?, num("plan", 2)?, num("plan", 3)?);
    let (west, south, east, north) = (num("latlng", 0)?, num("latlng", 1)?, num("latlng", 2)?, num("latlng", 3)?);
    if w <= 0.0 || h <= 0.0 || east <= west || north <= south {
        return None;
    }
    // The page is measured up from its foot and the picture down from its head.
    let s = scale as f64;
    Some(serde_json::json!({
        "pixels": { "x1": x * s, "y1": (page_h - y) * s, "x2": (x + w) * s, "y2": (page_h - y - h) * s },
        "latlng": { "lng1": west, "lat1": south, "lng2": east, "lat2": north },
    }))
}

fn open(path: &Path) -> Result<hayro::hayro_syntax::Pdf> {
    let bytes = crate::output::chart_pack::read(path).ok_or_else(|| anyhow!("{}: not there", path.display()))?;
    hayro::hayro_syntax::Pdf::new(bytes).map_err(|e| anyhow!("{}: cannot be read as a PDF ({e:?})", path.display()))
}

/// A chart's size in pixels at `scale` pixels to the point.
pub fn size(path: &Path, scale: f32) -> Option<(u32, u32)> {
    let pdf = open(path).ok()?;
    let pages = pdf.pages();
    let (w, h) = pages.iter().next()?.render_dimensions();
    Some(((w * scale) as u32, (h * scale) as u32))
}

/// A chart as a PNG at `scale` pixels to the point. By night every colour's lightness is
/// turned over and its hue kept: white paper goes black, black ink white, and water and
/// terrain keep their colours, darkened.
pub fn picture(path: &Path, scale: f32, night: bool) -> Result<Vec<u8>> {
    let pdf = open(path)?;
    let pages = pdf.pages();
    let page = pages.iter().next().ok_or_else(|| anyhow!("{}: no pages", path.display()))?;
    let settings = hayro::PixmapSettings { x_scale: scale, y_scale: scale, bg_color: hayro::vello_cpu::color::palette::css::WHITE };
    let mut pixmap = hayro::render(page, &hayro::RenderCache::new(), &hayro::hayro_interpret::InterpreterSettings::default(), &hayro::RenderSettings::default(), &settings);
    if night {
        for px in pixmap.data_as_u8_slice_mut().chunks_exact_mut(4) {
            let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
            // In HSL terms lightness is half of max + min; adding 255 - max - min to every
            // channel takes it to 1 - lightness and leaves hue and saturation as they were.
            let shift = 255 - r.max(g).max(b) - r.min(g).min(b);
            for c in px.iter_mut().take(3) {
                *c = (*c as i32 + shift).clamp(0, 255) as u8;
            }
        }
    }
    pixmap.into_png().map_err(|e| anyhow!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn an_aircraft_is_shown_its_own_cycle_or_the_database_set() {
        let all = s(&["charts-2503", "charts-2610"]);
        assert_eq!(pick(&all, Some("2610"), Some("2610")).as_deref(), Some("charts-2610"));
        assert_eq!(pick(&all, Some("2404"), Some("2610")).as_deref(), Some("charts-2503"));
        assert_eq!(pick(&all, None, Some("2610")).as_deref(), Some("charts-2503"), "a flight bag the bridge cannot tell");
        // American: the FAA's for an aircraft on the current cycle, the database set otherwise.
        let us = s(&["charts", "charts-2503"]);
        assert_eq!(pick(&us, Some("2610"), Some("2610")).as_deref(), Some("charts"));
        assert_eq!(pick(&us, Some("2303"), Some("2610")).as_deref(), Some("charts-2503"));
        // Only what there is.
        assert_eq!(pick(&s(&["charts-2610"]), Some("2404"), Some("2610")).as_deref(), Some("charts-2610"));
        assert_eq!(pick(&[], Some("2404"), Some("2610")), None);
    }

    #[test]
    fn names_say_what_a_chart_is() {
        assert_eq!(runway_of("ILS Z RWY 04R").as_deref(), Some("04R"));
        assert_eq!(runway_of("RNAV (GPS) RWY 23 (2)").as_deref(), Some("23"));
        assert_eq!(runway_of("VOR-A").as_deref(), Some("A"));
        assert_eq!(approach_type("ILS RWY 27L"), ("01", true));
        assert_eq!(approach_type("LOC BC RWY 05").0, "1E");
        assert_eq!(approach_type("RNAV (GPS) Y RWY 05").0, "1L");
        assert_eq!(slug("RNAV (GPS) Y RWY 05"), "RNAV__GPS__Y_RWY_05");
    }

    #[test]
    fn a_set_is_listed_as_a_binder_files_it_and_found_again_by_id() {
        let dir = std::env::temp_dir().join(format!("amdb-filed-{}", std::process::id()));
        let set = dir.join("charts-2503");
        std::fs::create_dir_all(&set).unwrap();
        for name in ["ILS RWY 05.pdf", "VOR-A.pdf", "SID DEGU1E - GALO1E.pdf", "STAR NIDU1P.pdf"] {
            std::fs::write(set.join(name), b"%PDF-1.7").unwrap();
        }
        std::fs::write(dir.join("chart.pdf"), b"%PDF-1.7").unwrap();
        std::fs::write(set.join("cycle.txt"), "Drawn by amdbgen from x database AIRAC 2503 (in force 20 MAR 2025 to 16 APR 2025).\r\n").unwrap();
        assert_eq!(sets(&dir), s(&["charts-2503"]));
        let charts = list("LPMA", &dir, "charts-2503");
        let cats: Vec<&str> = charts.iter().map(|c| c.category).collect();
        assert_eq!(cats, ["ARR", "DEP", "APT", "APP", "APP"]);
        let dep = &charts[1];
        assert_eq!((dep.name.as_str(), dep.procedures.len(), dep.index_number.as_str()), ("DEGU1E / GALO1E", 2, "10-3"));
        assert_eq!(charts[2].runways, s(&["05"]));
        assert_eq!(charts[3].runways, s(&["05"]));
        assert!(charts[3].precision);
        for c in &charts {
            assert_eq!(path_of("LPMA", &dir, &c.id).as_deref(), Some(c.path.as_path()), "{}", c.id);
        }
        assert_eq!(path_of("LPMA", &dir, "LPMA~charts-2503~NOPE"), None);
        assert_eq!(path_of("LPMA", &dir, "LPMA~..~x"), None);
        assert_eq!(in_force(&dir, "charts-2503"), "20 MAR 2025");
        // Georeferenced from the file beside it, in pixels down from the picture's head.
        assert!(georef(&charts[3].path, 2.5).is_none(), "nothing written yet");
        std::fs::write(charts[3].path.with_extension("geo.json"), r#"{"page":[595.0,842.0],"plan":[30.0,100.0,500.0,400.0],"latlng":[-17.0,32.5,-16.5,32.9]}"#).unwrap();
        let pv = georef(&charts[3].path, 2.5).unwrap();
        assert_eq!(pv["pixels"]["x1"], 75.0);
        assert_eq!(pv["pixels"]["y1"], (842.0 - 100.0) * 2.5);
        assert_eq!(pv["pixels"]["y2"], (842.0 - 500.0) * 2.5);
        assert_eq!(pv["latlng"]["lat2"], 32.9);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
