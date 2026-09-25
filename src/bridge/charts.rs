//! Approach charts for an electronic flight bag, answered the way the Navigraph SDK
//! (`@navigraph/charts` and `@navigraph/auth`) asks for them:
//!
//! * `POST /identity/connect/deviceauthorization`, `/connect/token`, `/connect/revocation`
//! * `GET  /v2/charts/{ICAO}?version=&rules=`       the airport's charts
//! * `GET  /v2/charts/{ICAO}/{id}.png?theme=`       one of them, by day or by night
//! * `GET  /v2/charts/{ICAO}/{id}.thumb.png?theme=` a card naming it, for the list
//! * `GET  /v2/airport/{ICAO}`                       what the airport is
//!
//! Every chart is one we draw ourselves from free data: nothing here comes from, or goes
//! to, Navigraph. The flight bags are pointed here by `patcher::patch_charts`, which
//! rewrites the three places the SDK builds its addresses; nothing else on the machine
//! is redirected.
//!
//! The sign-in is a stand-in. The SDK reads the user out of the access token without
//! checking who signed it, and an EFB opens its charts only for a user with a charts
//! subscription, so the token says that. It is only ever shown to the flight bag, which
//! uses it for nothing but asking this server for our own charts.

use super::store::Store;
use crate::output::approach;
use crate::sources::msfs::procedures::{self, ApproachType, AirportProcedures, Procedure};
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;

/// Pixels to the point the charts are drawn at: about 180 to the inch on A4, 1488 by
/// 2105 pixels, which is what a flight bag expects to zoom into.
pub const SCALE: f32 = 2.5;

/// What the stand-in sign-in says the user is called.
const USER_NAME: &str = "AMDB Bridge";

fn b64(bytes: &[u8]) -> String {
    // Standard alphabet with padding: the SDK decodes the token with `atob`, which
    // refuses the URL-safe one.
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// An access token the SDK will read a charts subscriber out of.
fn access_token() -> String {
    let now = chrono::Utc::now().timestamp();
    let header = json!({ "alg": "none", "typ": "JWT" });
    let payload = json!({
        "sub": "amdb-bridge-local",
        "preferred_username": USER_NAME,
        "scope": ["openid", "userinfo", "offline_access", "charts", "tiles", "amdb"],
        "subscriptions": ["charts"],
        "iat": now,
        "nbf": now,
        // Ten years: the token names nothing that expires.
        "exp": now + 10 * 365 * 24 * 3600,
        "iss": "amdb-bridge",
    });
    format!("{}.{}.{}", b64(header.to_string().as_bytes()), b64(payload.to_string().as_bytes()), b64(b"amdb-bridge"))
}

/// The answer to the token endpoint, for any grant: a device code being polled, or a
/// refresh. The device flow is answered on its first poll, so signing in takes a second
/// and there is no code to type anywhere.
pub fn token_json() -> Value {
    json!({
        "access_token": access_token(),
        "refresh_token": "amdb-bridge-local",
        "token_type": "Bearer",
        "expires_in": 10 * 365 * 24 * 3600,
        "scope": "openid userinfo offline_access charts tiles amdb",
    })
}

/// The answer to the device-authorization endpoint.
pub fn device_json(port: u16) -> Value {
    let uri = format!("http://127.0.0.1:{port}/identity/device");
    json!({
        "device_code": "amdb-bridge-local",
        "user_code": "AMDB",
        "verification_uri": uri,
        "verification_uri_complete": uri,
        "expires_in": 600,
        "interval": 1,
    })
}

/// What an approach is called in the list, and its id in addresses: the runway, with
/// the number that tells two approaches to it apart.
fn chart_id(icao: &str, p: &Procedure) -> String {
    match p.variant {
        Some(n) if n > 1 => format!("{icao}-{}-{n}", p.runway),
        _ => format!("{icao}-{}", p.runway),
    }
}

/// The approach an id names.
fn approach_name(icao: &str, id: &str) -> Option<String> {
    let rest = id.strip_prefix(icao)?.strip_prefix('-')?;
    Some(rest.to_string())
}

/// Navigraph's chart type for an approach: the code a flight bag sorts and badges by.
fn type_code(t: Option<ApproachType>) -> &'static str {
    match t {
        Some(ApproachType::Ils) => "01",
        Some(ApproachType::Vor) => "03",
        Some(ApproachType::Ndb) => "06",
        Some(ApproachType::Localiser) => "1D",
        Some(ApproachType::LocaliserBackCourse) => "1E",
        Some(ApproachType::Lda) => "1F",
        Some(ApproachType::Rnav) | Some(ApproachType::Gps) => "1L",
        None => "1L",
    }
}

/// Where built aerodromes live, and how to build one that is not there yet.
///
/// The ground diagram is drawn from the layers an airport is built into, not from the
/// simulator's navigation data, so unlike every other chart here it needs that build. The
/// bridge already builds airports on demand for the moving map; this is the same folder
/// and the same settings, kept here so that the background drawing thread -- which owns
/// no `Store` -- can reach them.
static GROUND: std::sync::OnceLock<crate::pipeline::Config> = std::sync::OnceLock::new();

/// Told to the module once, as the bridge starts.
pub fn set_ground(cfg: crate::pipeline::Config) {
    let _ = GROUND.set(cfg);
}

/// The folder holding `icao`'s layers, building it if this is the first time it has been
/// asked for. Returns `None` when the bridge has not been started, which is the case in
/// the tests and when the chart code is used from the command line.
fn ground_dir(icao: &str) -> Result<PathBuf> {
    let cfg = GROUND.get().ok_or_else(|| anyhow!("the bridge has not said where built airports are kept"))?;
    let dir = cfg.out.join(icao);
    if dir.join("manifest.json").is_file() {
        return Ok(dir);
    }
    // A bulk worker or the moving map may be on this airport right now; waiting for it
    // is cheaper, and safer, than building the same folder twice.
    let t0 = std::time::Instant::now();
    while crate::pipeline::is_building(icao) && t0.elapsed().as_secs() < 900 {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    if dir.join("manifest.json").is_file() {
        return Ok(dir);
    }
    crate::term::start(&format!("[{icao}] Building {icao} for its airport diagram"));
    let summary = crate::pipeline::run(cfg, std::slice::from_ref(&icao.to_string()))?;
    if let Some((_, e)) = summary.failed.first() {
        return Err(anyhow!("{icao}: build failed: {e}"));
    }
    Ok(dir)
}

/// Where drawn charts are kept, by cycle, so a new cycle draws them again.
fn cache_dir(icao: &str) -> PathBuf {
    let cycle = crate::sources::msfs::airac_dates().map(|(from, _)| from.replace(' ', "")).unwrap_or_else(|| "current".to_string());
    super::tls::data_dir().join("charts").join(cycle).join(icao)
}

fn procedures_for(icao: &str) -> Result<AirportProcedures> {
    procedures::find(icao)?.ok_or_else(|| anyhow!("{icao} has no procedures in the simulator's navigation data"))
}

/// One entry on an airport's chart list, before it is put in the shape a flight bag wants.
struct Entry {
    id: String,
    category: &'static str,
    type_code: &'static str,
    precision: bool,
    index_number: String,
    name: String,
    procedures: Vec<String>,
    runways: Vec<String>,
}

/// Every chart an airport has: its departures, its arrivals and its approaches, in the
/// order a chart binder files them. A departure or arrival page carries every procedure
/// a chart puts together on one page, and its id names the first of them.
fn entries(icao: &str, found: &AirportProcedures) -> Vec<Entry> {
    use crate::output::approach::terminal;
    use crate::sources::msfs::procedures::Kind;
    let mut out = Vec::new();
    let letter = |n: usize| if n == 0 { String::new() } else { ((b'A' + (n as u8 - 1).min(25)) as char).to_string() };
    let groups = terminal::groups(found);
    for (kind, category, code, rnav_code, series) in [(Kind::Star, "ARR", "J", "JG", "10-2"), (Kind::Sid, "DEP", "G", "GG", "10-3")] {
        for (n, g) in groups.iter().filter(|g| g[0].kind == kind).enumerate() {
            let mut runways: Vec<String> = g.iter().flat_map(|p| terminal::runways_of(p)).collect();
            runways.sort();
            runways.dedup();
            let rnav = g.iter().all(|p| terminal::is_rnav(p));
            out.push(Entry {
                id: format!("{icao}-{category}-{}", g[0].name),
                category,
                type_code: if rnav { rnav_code } else { code },
                precision: false,
                index_number: format!("{series}{}", letter(n)),
                name: g.iter().map(|p| terminal::title(p)).collect::<Vec<_>>().join(" / "),
                procedures: g.iter().map(|p| p.name.clone()).collect(),
                runways,
            });
        }
    }
    // The aerodrome itself, filed where a binder files it: ahead of the approaches, and
    // the page a crew opens on the ground.
    //
    // It is named against every runway the aerodrome has. The flight bags group a chart
    // list by runway and put anything that names none under a heading reading "Runway
    // n/a", which is no place for the one page that belongs to the whole aerodrome; a
    // 10-9 is filed under each runway for the same reason.
    let mut all_runways: Vec<String> = groups.iter().flatten().flat_map(|p| terminal::runways_of(p)).collect();
    all_runways.sort();
    all_runways.dedup();
    out.push(Entry {
        id: format!("{icao}-APT"),
        category: "APT",
        type_code: "AM",
        precision: false,
        index_number: "10-9".to_string(),
        name: "AIRPORT DIAGRAM".to_string(),
        procedures: Vec::new(),
        runways: all_runways,
    });
    for (n, p) in approach::approaches(found).into_iter().enumerate() {
        out.push(Entry {
            id: chart_id(icao, p),
            category: "APP",
            type_code: type_code(p.approach_type),
            precision: matches!(p.approach_type, Some(ApproachType::Ils)),
            index_number: format!("11-{}", n + 1),
            name: approach::title_of(p),
            procedures: vec![p.name.clone()],
            runways: vec![p.runway.clone()],
        });
    }
    out
}

/// What a chart already drawn says about itself: where its plan is on the ground, and its
/// size, which a landscape page does not share with the rest.
fn drawn_meta(dir: &std::path::Path, id: &str) -> Option<Value> {
    std::fs::read_to_string(dir.join(format!("{id}.json"))).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok())
}

/// The airport's charts, in the shape `getChartsIndex` returns. `base` is the address
/// the charts are fetched back from, as the flight bag reached this server.
///
/// Asking for the list starts every chart on it drawing in the background, approaches
/// first, so that by the time one is chosen it is usually waiting.
pub fn index_json(icao: &str, base: &str) -> Result<Value> {
    let icao = icao.to_uppercase();
    let found = procedures_for(&icao)?;
    let (width, height) = approach::picture_size(SCALE);
    let date = crate::sources::msfs::airac_dates().map(|(from, _)| from).unwrap_or_default();
    let dir = cache_dir(&icao);
    let list = entries(&icao, &found);
    let mut charts = Vec::new();
    for e in &list {
        let id = &e.id;
        let url = |kind: &str, theme: &str| format!("{base}/v2/charts/{icao}/{id}{kind}?theme={theme}");
        let mut chart = json!({
            "id": id,
            "icao_airport_identifier": icao,
            "category": e.category,
            "type_code": e.type_code,
            "precision_approach": e.precision,
            "index_number": e.index_number,
            "name": e.name,
            "revision_date": date,
            "width": width,
            "height": height,
            "procedures": e.procedures,
            "runways": e.runways,
            "image_day_url": url(".png", "day"),
            "image_night_url": url(".png", "night"),
            "thumb_day_url": url(".thumb.png", "day"),
            "thumb_night_url": url(".thumb.png", "night"),
            "image_day": format!("{id}.png"),
            "image_night": format!("{id}.night.png"),
            "thumb_day": format!("{id}.thumb.png"),
            "thumb_night": format!("{id}.thumb.night.png"),
            "is_georeferenced": false,
            "bounding_boxes": Value::Null,
        });
        // A chart already drawn knows where its plan is on the ground and how big it
        // is; one not yet drawn gains them the next time the list is asked for.
        if let Some(meta) = drawn_meta(&dir, id) {
            let planview = meta.get("planview").cloned().unwrap_or_else(|| meta.clone());
            chart["is_georeferenced"] = json!(true);
            chart["bounding_boxes"] = json!({ "planview": planview, "insets": [] });
            if let (Some(w), Some(h)) = (meta.get("width"), meta.get("height")) {
                chart["width"] = w.clone();
                chart["height"] = h.clone();
            }
        }
        charts.push(chart);
    }
    let count = |c: &str| list.iter().filter(|e| e.category == c).count();
    crate::term::info(&format!("[{icao}] Chart list: {} departure, {} arrival and {} approach pages", count("DEP"), count("ARR"), count("APP")));
    // Approaches first -- they are what a list is usually opened for -- then the terminal
    // pages, and the ground diagram last, because it is the one that may have to fetch and
    // build the aerodrome before it can draw anything.
    let order = |c: &str| match c {
        "APP" => 0,
        "APT" => 2,
        _ => 1,
    };
    let mut ahead: Vec<&Entry> = list.iter().collect();
    ahead.sort_by_key(|e| order(e.category));
    draw_ahead(&icao, ahead.into_iter().map(|e| e.id.clone()).collect());
    Ok(json!({ "charts": charts }))
}

/// Draw an airport's charts in the background, once per airport while the bridge runs.
fn draw_ahead(icao: &str, ids: Vec<String>) {
    static STARTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    {
        let mut started = STARTED.lock().unwrap_or_else(|e| e.into_inner());
        if started.iter().any(|s| s == icao) {
            return;
        }
        started.push(icao.to_string());
    }
    let icao = icao.to_string();
    std::thread::spawn(move || {
        for id in ids {
            if let Err(e) = image(&icao, &id, false) {
                log::warn!("[{icao}] could not draw {id} ahead: {e:#}");
            }
        }
    });
}

/// What `getAirportInfo` returns, as much of it as the simulator's data says.
pub fn airport_json(icao: &str, store: &Store) -> Result<Value> {
    let icao = icao.to_uppercase();
    let found = procedures_for(&icao)?;
    let name = store
        .search(&icao)
        .into_iter()
        .find(|r| r.get("idarpt").and_then(Value::as_str) == Some(icao.as_str()))
        .and_then(|r| r.get("name").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| icao.clone());
    Ok(json!({
        "icao_airport_identifier": icao,
        "iata_airport_designator": "",
        "name": name,
        "city": "",
        "latitude": found.lat,
        "longitude": found.lon,
        "magnetic_variation": found.magnetic_variation_deg.unwrap_or(0.0),
        "elevation": 0,
        "longest_runway": 0,
        "country_code": "",
        "country_name": "",
        "state_province_code": "",
        "state_province_name": "",
        "fuel_types": [],
        "oxygen": [],
        "repairs": [],
        "landing_fee": false,
        "jet_starting_unit": false,
        "precision_airport": approach::approaches(&found).iter().any(|p| p.approach_type == Some(ApproachType::Ils)),
        "beacon": false,
        "customs": false,
        "airport_type": "",
        "time_zone": "",
        "icao_code": "",
        "daylight_savings": false,
        "datum_code": "WGE",
        "revision_date": "",
        "parsed_cycle": "",
        "std_charts": true,
        "cao_charts": false,
        "vfr_charts": false,
    }))
}

/// One chart as a picture, drawn the first time it is asked for and kept after.
///
/// Drawing gathers terrain and obstacles over the network and can take a while, so only
/// one chart is drawn at a time: a flight bag asking for the day and night sides at once
/// then waits for the first rather than drawing the same chart twice.
pub fn image(icao: &str, id: &str, night: bool) -> Result<Vec<u8>> {
    static DRAWING: Mutex<()> = Mutex::new(());
    let icao = icao.to_uppercase();
    let dir = cache_dir(&icao);
    let file = dir.join(format!("{id}.{}.png", if night { "night" } else { "day" }));
    if let Ok(bytes) = std::fs::read(&file) {
        return Ok(bytes);
    }
    let _one_at_a_time = DRAWING.lock().unwrap_or_else(|e| e.into_inner());
    // Drawn while this one waited.
    if let Ok(bytes) = std::fs::read(&file) {
        return Ok(bytes);
    }
    let t0 = std::time::Instant::now();
    crate::term::info(&format!("[{icao}] Drawing {id} for the flight bag"));
    // The ground diagram, a departure or arrival page, or an approach.
    let terminal = ["DEP", "ARR"].iter().find_map(|k| id.strip_prefix(&format!("{icao}-{k}-")));
    let picture = match terminal {
        _ if id == format!("{icao}-APT") => crate::output::chart::picture(&ground_dir(&icao)?, SCALE)?,
        Some(name) => crate::output::approach::terminal::with_terminal(&icao, name, |t| crate::output::approach::terminal::picture(t, SCALE))?,
        None => {
            let want = approach_name(&icao, id).ok_or_else(|| anyhow!("{id} is not one of {icao}'s charts"))?;
            approach::with_chart(&icao, Some(&want), None, None, |chart, est| approach::picture(chart, est, SCALE))?
        }
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::write(dir.join(format!("{id}.day.png")), &picture.day)?;
    std::fs::write(dir.join(format!("{id}.night.png")), &picture.night)?;
    let (x1, y1, x2, y2) = picture.plan.pixels;
    let (lng1, lat1, lng2, lat2) = picture.plan.latlng;
    let meta = json!({
        "planview": {
            "pixels": { "x1": x1, "y1": y1, "x2": x2, "y2": y2 },
            "latlng": { "lng1": lng1, "lat1": lat1, "lng2": lng2, "lat2": lat2 },
        },
        "width": picture.width,
        "height": picture.height,
    });
    std::fs::write(dir.join(format!("{id}.json")), meta.to_string())?;
    crate::term::success(&format!("[{icao}] Drew {id} in {}", crate::term::human_secs(t0.elapsed().as_secs_f64())));
    Ok(if night { picture.night } else { picture.day })
}

/// A card naming a chart, cheap enough to answer for a whole list at once.
pub fn thumbnail(icao: &str, id: &str, night: bool) -> Result<Vec<u8>> {
    let icao = icao.to_uppercase();
    let found = procedures_for(&icao)?;
    let title = entries(&icao, &found).into_iter().find(|e| e.id == id).map(|e| e.name).ok_or_else(|| anyhow!("{id} is not one of {icao}'s charts"))?;
    approach::thumbnail(&title, &icao, night)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SDK reads the user out of the token's middle part with `atob` and wants `sub`
    /// in it; the flight bags want a charts subscription.
    #[test]
    fn token_reads_as_a_charts_subscriber() {
        let token = token_json()["access_token"].as_str().unwrap().to_string();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let payload = base64::engine::general_purpose::STANDARD.decode(parts[1]).unwrap();
        let v: Value = serde_json::from_slice(&payload).unwrap();
        assert!(v.get("sub").is_some());
        assert_eq!(v["subscriptions"][0], "charts");
        assert!(v["exp"].as_i64().unwrap() > chrono::Utc::now().timestamp());
    }

    /// The ground diagram through the bridge: on the list where a binder files it, and
    /// drawn as a picture with a georeference an EFB can put the aeroplane on. Needs an
    /// airport already built under `out/`, so it is run by hand:
    /// `cargo test --release -- --ignored serves_an_airport_diagram`
    #[test]
    #[ignore]
    fn serves_an_airport_diagram() {
        let mut cfg = crate::bridge::cli::config(&Default::default(), &Default::default());
        cfg.out = "out".into();
        super::set_ground(cfg);
        let list = index_json("WSSS", "http://127.0.0.1:8770").unwrap();
        let charts = list["charts"].as_array().unwrap();
        let apt = charts.iter().find(|c| c["id"] == "WSSS-APT").expect("the diagram is on the list");
        assert_eq!(apt["category"], "APT");
        assert_eq!(apt["index_number"], "10-9");
        // Named against every runway, so the flight bags file it under each of them
        // rather than under a heading reading "Runway n/a".
        let rwys = apt["runways"].as_array().unwrap();
        assert!(rwys.iter().any(|r| r == "02L") && rwys.iter().any(|r| r == "20R"), "{rwys:?}");
        let png = image("WSSS", "WSSS-APT", false).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert_ne!(png, image("WSSS", "WSSS-APT", true).unwrap());
        // Asked again, the list now knows where the diagram's map lies on the ground.
        let again = index_json("WSSS", "http://127.0.0.1:8770").unwrap();
        let apt = again["charts"].as_array().unwrap().iter().find(|c| c["id"] == "WSSS-APT").unwrap().clone();
        assert_eq!(apt["is_georeferenced"], true);
        let b = &apt["bounding_boxes"]["planview"]["latlng"];
        assert!(b["lng1"].as_f64().unwrap() < b["lng2"].as_f64().unwrap(), "{b}");
        assert!(b["lat1"].as_f64().unwrap() < b["lat2"].as_f64().unwrap(), "{b}");
        println!("WSSS-APT {}x{} over {b}", apt["width"], apt["height"]);
        let thumb = thumbnail("WSSS", "WSSS-APT", false).unwrap();
        assert_eq!(&thumb[1..4], b"PNG");
        std::fs::write(std::env::temp_dir().join("efb-WSSS-APT.png"), &png).unwrap();

        // And a departure page, which shares the list with it. A SID with no legs draws a
        // blank sheet, which is what the leg-list reader's missing 0xF7 used to produce,
        // so the picture is checked for having something on it rather than only for being
        // a PNG: a page of white compresses to almost nothing.
        let dep = charts.iter().find(|c| c["category"] == "DEP").expect("a departure page");
        let id = dep["id"].as_str().unwrap().to_string();
        let png = image("WSSS", &id, false).unwrap();
        assert!(png.len() > 60_000, "{id} came out at {} bytes: an empty sheet?", png.len());
        println!("{id} \"{}\" {} bytes", dep["name"], png.len());
        std::fs::write(std::env::temp_dir().join(format!("efb-{id}.png")), &png).unwrap();
    }

    /// The chart list, read the way FlyByWire's flight bag reads it.
    ///
    /// Its A380X and A32NX sort the list into their five tabs by one field and nothing
    /// else -- `fbw-common`'s NavigraphChartUI does
    ///
    /// ```text
    /// STAR: category === "ARR"   APP: "APP"   TAXI: "APT"   SID: "DEP"   REF: "REF"
    /// ```
    ///
    /// so a category it does not know puts a chart in no tab at all and it simply is not
    /// there. It then reads `id`, `name`, `index_number`, the two image addresses and
    /// `bounding_boxes` off whatever it shows, and groups the approach tab by walking
    /// `chart.runways`, which it indexes without checking -- a chart without that array
    /// would throw rather than come out unsorted. This holds the answer to that shape.
    ///
    /// `cargo test --release -- --ignored answers_what_flybywire_reads`
    #[test]
    #[ignore]
    fn answers_what_flybywire_reads() {
        let list = index_json("WSSS", "http://127.0.0.1:8770").unwrap();
        let charts = list["charts"].as_array().unwrap();
        assert!(!charts.is_empty());
        for c in charts {
            let cat = c["category"].as_str().unwrap_or("");
            assert!(matches!(cat, "ARR" | "APP" | "APT" | "DEP" | "REF"), "{cat} is in none of the five tabs: {c}");
            for f in ["id", "name", "index_number", "image_day_url", "image_night_url"] {
                assert!(c[f].as_str().is_some_and(|v| !v.is_empty()), "{f} missing from {c}");
            }
            assert!(c["runways"].is_array(), "runways is indexed without checking: {c}");
            assert!(c["bounding_boxes"].is_object() || c["bounding_boxes"].is_null());
            for f in ["image_day_url", "image_night_url"] {
                assert!(c[f].as_str().unwrap().starts_with("http://127.0.0.1:8770/v2/charts/WSSS/"), "{f} is not fetchable: {c}");
            }
        }
        let tab = |cat: &str| charts.iter().filter(|c| c["category"] == cat).count();
        // Every tab a crew would look in has something in it, the ground diagram included:
        // it is the one that fills FlyByWire's TAXI tab.
        assert!(tab("APT") >= 1, "nothing in TAXI");
        assert!(tab("DEP") >= 1, "nothing in SID");
        assert!(tab("ARR") >= 1, "nothing in STAR");
        assert!(tab("APP") >= 1, "nothing in APP");
        println!("WSSS: STAR {}, APP {}, TAXI {}, SID {}", tab("ARR"), tab("APP"), tab("APT"), tab("DEP"));
    }

    /// A real airport through the whole of it: the list, one chart drawn by day and by
    /// night, and the list again carrying where that chart's plan lies. Needs the
    /// simulator's navigation data and the network, so it is run by hand:
    /// `cargo test -- --ignored draws_a_real_airport`.
    #[test]
    #[ignore]
    fn draws_a_real_airport() {
        let list = index_json("LPMA", "http://127.0.0.1:8770").unwrap();
        let charts = list["charts"].as_array().unwrap();
        assert!(!charts.is_empty());
        let first = &charts[0];
        let id = first["id"].as_str().unwrap().to_string();
        println!("{} charts; first {} \"{}\"", charts.len(), id, first["name"]);
        let day = image("LPMA", &id, false).unwrap();
        let night = image("LPMA", &id, true).unwrap();
        assert_eq!(&day[1..4], b"PNG");
        assert_ne!(day, night);
        let out = std::env::temp_dir();
        std::fs::write(out.join(format!("{id}.day.png")), &day).unwrap();
        std::fs::write(out.join(format!("{id}.night.png")), &night).unwrap();
        std::fs::write(out.join(format!("{id}.thumb.png")), thumbnail("LPMA", &id, false).unwrap()).unwrap();
        let again = index_json("LPMA", "http://127.0.0.1:8770").unwrap();
        let drawn = again["charts"].as_array().unwrap().iter().find(|c| c["id"] == id.as_str()).unwrap();
        assert_eq!(drawn["is_georeferenced"], true);
        let pv = &drawn["bounding_boxes"]["planview"];
        println!("planview {pv}");
        // Bottom-left first: further down the picture, and further south and west.
        assert!(pv["pixels"]["y1"].as_f64().unwrap() > pv["pixels"]["y2"].as_f64().unwrap());
        assert!(pv["latlng"]["lat1"].as_f64().unwrap() < pv["latlng"]["lat2"].as_f64().unwrap());
        println!("written to {}", out.display());
    }

    #[test]
    fn ids_name_their_approach() {
        assert_eq!(approach_name("LPMA", "LPMA-05"), Some("05".to_string()));
        assert_eq!(approach_name("KSFO", "KSFO-28L-2"), Some("28L-2".to_string()));
        assert_eq!(approach_name("LPMA", "EGLL-27L"), None);
    }
}
