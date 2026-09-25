//! The planning panel: a flight plan asked for in a browser, and the search drawn as it runs.
//!
//! The bridge already runs an HTTP server for the tablets, so the panel is served from it
//! rather than drawn in the desktop window. That window is built on the plain Windows
//! controls, which have no canvas and no layout of their own; a form of thirty fields beside an
//! animated world map is weeks of drawing code there and an afternoon here, and the result can
//! be opened from the window, from a tablet, or from a phone on the same network.
//!
//! The map is drawn in the page rather than rendered here, because the interesting thing about
//! it moves: [`crate::route::progress`] reports where the search has reached as it reaches
//! there, the page collects those and paints them, and a route that sweeps a thousand miles the
//! wrong way before settling is something you watch happen rather than infer afterwards.

use crate::route::progress::{self, Event};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// One plan being worked on, and everything the page needs to draw it.
pub struct Run {
    pub id: u64,
    origin: Mutex<Option<(f64, f64)>>,
    destination: Mutex<Option<(f64, f64)>>,
    /// Everywhere the search has reached, in the order it reached them.
    reached: Mutex<Vec<(f32, f32)>>,
    /// The best way through found so far, replaced as it improves.
    best: Mutex<Option<(Vec<(f32, f32)>, f64)>>,
    done: AtomicBool,
    summary: Mutex<Option<Value>>,
    failed: Mutex<Option<String>>,
}

impl Run {
    fn new(id: u64) -> Run {
        Run {
            id,
            origin: Mutex::new(None),
            destination: Mutex::new(None),
            reached: Mutex::new(Vec::new()),
            best: Mutex::new(None),
            done: AtomicBool::new(false),
            summary: Mutex::new(None),
            failed: Mutex::new(None),
        }
    }

    /// What has happened since the page last asked, as the page wants it: the points it has not
    /// drawn yet, rather than all of them again.
    fn since(&self, cursor: usize) -> Value {
        let reached = self.reached.lock().map(|r| r.clone()).unwrap_or_default();
        let fresh: Vec<Value> = reached.iter().skip(cursor).map(|&(a, b)| json!([a, b])).collect();
        let best = self.best.lock().ok().and_then(|b| b.clone());
        json!({
            "id": self.id,
            "cursor": reached.len(),
            "reached": fresh,
            "best": best.as_ref().map(|(p, nm)| json!({ "path": p.iter().map(|&(a, b)| json!([a, b])).collect::<Vec<_>>(), "nm": nm })),
            "origin": self.origin.lock().ok().and_then(|o| *o).map(|(a, b)| json!([a, b])),
            "destination": self.destination.lock().ok().and_then(|o| *o).map(|(a, b)| json!([a, b])),
            "done": self.done.load(Ordering::Relaxed),
            "summary": self.summary.lock().ok().and_then(|s| s.clone()),
            "error": self.failed.lock().ok().and_then(|s| s.clone()),
        })
    }
}

impl progress::Sink for Run {
    fn event(&self, event: Event) {
        match event {
            Event::Started { origin, destination } => {
                if let Ok(mut o) = self.origin.lock() {
                    *o = Some(origin);
                }
                if let Ok(mut d) = self.destination.lock() {
                    *d = Some(destination);
                }
            }
            Event::Reached { at, .. } => {
                if let Ok(mut r) = self.reached.lock() {
                    // A page can draw a few tens of thousands of points; past that it is a
                    // smear and not a picture, and the memory is the server's to hold.
                    if r.len() < MOST_POINTS {
                        r.push((at.0 as f32, at.1 as f32));
                    }
                }
            }
            Event::Best { path, nm } => {
                if let Ok(mut b) = self.best.lock() {
                    *b = Some((path.iter().map(|&(a, c)| (a as f32, c as f32)).collect(), nm));
                }
            }
            Event::Finished { .. } => {}
        }
    }
}

const MOST_POINTS: usize = 40_000;

fn current() -> &'static Mutex<Option<Arc<Run>>> {
    static CURRENT: OnceLock<Mutex<Option<Arc<Run>>>> = OnceLock::new();
    CURRENT.get_or_init(|| Mutex::new(None))
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Begin a plan, in a thread of its own so the page can watch it rather than wait for it.
fn start(params: &Map<String, Value>) -> Arc<Run> {
    let run = Arc::new(Run::new(next_id()));
    if let Ok(mut held) = current().lock() {
        *held = Some(run.clone());
    }

    let text = |k: &str| params.get(k).and_then(Value::as_str).unwrap_or("").trim().to_uppercase();
    let number = |k: &str, fallback: f64| params.get(k).and_then(Value::as_str).and_then(|v| v.parse().ok()).unwrap_or(fallback);
    let (origin, destination, aircraft) = (text("from"), text("to"), text("aircraft"));
    let (payload, ci, pax) = (number("payload", 0.0), number("ci", 30.0), number("pax", 0.0));
    let level = params.get("level").and_then(Value::as_str).filter(|v| !v.trim().is_empty()).map(str::to_string);
    let alternate = params.get("alternate").and_then(Value::as_str).filter(|v| !v.trim().is_empty()).map(|v| v.trim().to_uppercase());
    let offline = params.get("offline").and_then(Value::as_str).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let airframe = params.get("airframe").and_then(Value::as_str).filter(|v| !v.trim().is_empty()).map(|v| v.trim().to_uppercase());
    let word = |k: &str| params.get(k).and_then(Value::as_str).map(|v| v.trim().to_uppercase()).filter(|v| !v.is_empty());
    let flag = |k: &str, fallback: bool| params.get(k).and_then(Value::as_str).map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(fallback);
    let letter = |k: &str, fallback: char| word(k).and_then(|v| v.chars().next()).unwrap_or(fallback);
    let (taxi_out, taxi_in) = (number("taxiout", 20.0), number("taxiin", 8.0));
    let (cont_pct, cont_min, reserve) = (number("contpct", 5.0), number("contmin", 5.0), number("reserve", 30.0));
    let (extra, alternates) = (number("extrafuel", 0.0), number("alternates", 3.0));
    let (tankering, steps) = (flag("tankering", false), flag("stepclimbs", true));
    let (rules, ftype) = (letter("rules", 'I'), letter("ftype", 'S'));
    let (deprwy, arrrwy) = (word("deprwy"), word("arrrwy"));
    // A weight given by hand, or nothing at all, which means work it out.
    let given = |k: &str| params.get(k).and_then(Value::as_str).map(|v| v.replace(',', "")).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| *v > 0.0);
    let (oew, mzfw, mtow, mlw, maxfuel, zfw) = (given("oew"), given("mzfw"), given("mtow"), given("mlw"), given("maxfuel"), given("zfw"));
    let freight = number("freight", 0.0);
    let cruise_mach = params.get("mach").and_then(Value::as_str).and_then(|v| v.trim().trim_start_matches('.').parse::<f64>().ok()).map(|m| if m > 1.5 { m / 100.0 } else { m }).filter(|m| (0.3..1.0).contains(m));

    let thread_run = run.clone();
    std::thread::spawn(move || {
        let mut opts = crate::ofp::DispatchOptions::new(origin, destination, aircraft);
        opts.payload_kg = payload;
        opts.passengers = pax as u32;
        opts.cost_index = ci;
        opts.offline = offline;
        opts.alternate = alternate;
        opts.airframe = airframe;
        opts.dep_runway = deprwy;
        opts.arr_runway = arrrwy;
        opts.taxi_out_min = taxi_out;
        opts.taxi_in_min = taxi_in;
        opts.contingency_pct = cont_pct;
        opts.contingency_min_minutes = cont_min;
        opts.reserve_minutes = reserve;
        opts.extra_fuel_kg = extra;
        opts.tankering = tankering;
        opts.alternates = alternates.max(1.0) as usize;
        opts.step_climbs = steps;
        opts.cruise_mach = cruise_mach;
        opts.flight_rules = rules;
        opts.flight_type = ftype;
        opts.oew_kg = oew;
        opts.mzfw_kg = mzfw;
        opts.mtow_kg = mtow;
        opts.mlw_kg = mlw;
        opts.max_fuel_kg = maxfuel;
        opts.zfw_kg = zfw;
        opts.freight_kg = freight;
        opts.level = level.and_then(|l| {
            let t = l.trim().trim_start_matches("FL").to_string();
            t.parse::<f64>().ok().map(|v| if v < 1000.0 { v * 100.0 } else { v })
        });

        progress::watch(Some(thread_run.clone()));
        let planned = crate::ofp::dispatch(&opts);
        progress::watch(None);

        match planned {
            Ok(d) => {
                // The route as it is read back, not merely item 15: the airports and the
                // runways in use are the part a crew copies, and the part that makes the SID
                // and the STAR mean anything.
                let end = |icao: &str, rwy: Option<&String>| match rwy {
                    Some(r) if !r.is_empty() => format!("{icao}/{r}"),
                    _ => icao.to_string(),
                };
                let read_back = format!(
                    "{} {} {}",
                    end(&d.route.origin.icao, d.route.dep_runway.as_ref()),
                    d.route.route_string(),
                    end(&d.route.destination.icao, d.route.arr_runway.as_ref())
                );
                let ground = d.route.distance_nm();
                let direct = crate::dispatch::distance_nm(d.route.origin.pos, d.route.destination.pos);
                // Everything a briefing puts on its first page. It is all worked out already;
                // throwing it away and showing four numbers would be a choice, not a limit.
                let air_min = d.perf.profile.last().map(|p| p.time_min).unwrap_or(0.0);
                let block_min = air_min + opts.taxi_out_min.max(0.0) + opts.taxi_in_min.max(0.0);
                let hhmm = |m: f64| format!("{:02}:{:02}", (m / 60.0) as i64, (m.round() as i64).rem_euclid(60));
                let cruise = d.perf.profile.iter().find(|p| p.kind == crate::dispatch::ProfileKind::TopOfClimb);
                let off = d.route.off_block + chrono::Duration::minutes(opts.taxi_out_min.max(0.0) as i64);
                let on = off + chrono::Duration::minutes(air_min.round() as i64);
                let w = &d.perf.weights;
                let f = &d.perf.fuel;
                if let Ok(mut s) = thread_run.summary.lock() {
                    *s = Some(json!({
                        // Flight info
                        "flight": opts.flight_number.clone().unwrap_or_default(),
                        "registration": opts.registration.clone().unwrap_or_default(),
                        "airframe": opts.airframe.clone().unwrap_or_default(),
                        "origin": d.route.origin.icao,
                        "origin_name": d.route.origin.name,
                        "destination": d.route.destination.icao,
                        "destination_name": d.route.destination.name,
                        "alternate": d.alternate.as_ref().map(|a| a.destination.icao.clone()).unwrap_or_default(),
                        "aircraft": d.spec.icao_type,
                        "aircraft_name": d.spec.name,
                        "dep_date": d.route.off_block.format("%d %b %y").to_string(),
                        "dep_time": d.route.off_block.format("%H:%M").to_string(),
                        "arr_time": on.format("%H:%M").to_string(),
                        "air_time": hhmm(air_min),
                        "block_time": hhmm(block_min),

                        // Flight plan summary
                        "initial_alt": cruise.map(|p| p.alt_ft).unwrap_or(d.route.cruise_ft).round(),
                        "cruise_profile": if let Some(m) = opts.cruise_mach { format!("M{:.2}", m) } else { format!("CI {:.0}", opts.cost_index) },
                        "ground_nm": ground.round(),
                        "direct_nm": direct.round(),
                        "over_pct": ((ground / direct.max(1.0) - 1.0) * 100.0).round(),
                        "avg_wind": format!("{:.0}", d.perf.avg_wind_kt.abs()),
                        "wind_component": format!("{}{:03.0}", if d.perf.avg_wind_kt < 0.0 { "M" } else { "P" }, d.perf.avg_wind_kt.abs()),
                        "isa_dev": format!("{}{:02.0}", if d.perf.avg_isa_dev < 0.0 { "M" } else { "P" }, d.perf.avg_isa_dev.abs()),
                        "airac": d.airac.clone().unwrap_or_default(),
                        "etops": d.spec.etops_minutes,

                        // Load sheet
                        "enroute_burn": f.trip_kg.round(),
                        "block_kg": f.block_kg.round(),
                        "taxi_kg": f.taxi_kg.round(),
                        "contingency_kg": f.contingency_kg.round(),
                        "alternate_kg": f.alternate_kg.round(),
                        "reserve_kg": f.final_reserve_kg.round(),
                        "extra_kg": (f.extra_kg + f.tanker_kg).round(),
                        "pax": opts.passengers,
                        "oew": w.oew_kg.round(),
                        "payload": w.payload_kg.round(),
                        "zfw": w.zfw_kg.round(),
                        "tow": w.tow_kg.round(),
                        "lw": w.lw_kg.round(),
                        "max_zfw": w.max_zfw_kg.round(),
                        "max_tow": w.max_tow_kg.round(),
                        "max_lw": w.max_lw_kg.round(),

                        // Route and the filed plan
                        "route": read_back,
                        "icao_plan": crate::ofp::export::icao_message(&d, opts.flight_number.as_deref(), opts.registration.as_deref(), opts.flight_rules, opts.flight_type),
                        "remarks": d.perf.warnings.clone(),
                        "fixes": d.route.points.iter().map(|w| json!({ "ident": w.ident, "pos": [w.pos.0, w.pos.1] })).collect::<Vec<_>>(),
                    }));
                }
            }
            Err(e) => {
                if let Ok(mut f) = thread_run.failed.lock() {
                    *f = Some(format!("{e:#}"));
                }
            }
        }
        thread_run.done.store(true, Ordering::Relaxed);
    });

    run
}

/// The network's own fixes, thinned and packed small, for the page to draw as its backdrop.
///
/// Two signed sixteen-bit numbers a fix, hundredths of a degree, which is about a third of a
/// nautical mile and far finer than a dot on a world map. Eighty thousand fixes would be a third
/// of a megabyte sent as text and a tenth of that packed; thinning it further costs nothing that
/// can be seen at this size.
fn network_bytes() -> Vec<u8> {
    let graph = crate::route::Graph::shared();
    let fixes = graph.fixes();
    let step = (fixes.len() / 30_000).max(1);
    let mut out = Vec::with_capacity(fixes.len() / step * 4);
    for fix in fixes.iter().step_by(step) {
        out.extend_from_slice(&((fix.pos.0 * 100.0) as i16).to_le_bytes());
        out.extend_from_slice(&((fix.pos.1 * 100.0) as i16).to_le_bytes());
    }
    out
}

/// The panel's own routes. Returns the request back when the path is not one of them.
pub fn handle(req: tiny_http::Request, path: &str, params: &Map<String, Value>, respond_json: impl Fn(tiny_http::Request, u16, String), respond_bytes: impl Fn(tiny_http::Request, u16, &str, Vec<u8>)) -> Option<tiny_http::Request> {
    let rest = match path.find("/plan") {
        Some(i) => path[i + 5..].trim_end_matches('/'),
        None => return Some(req),
    };
    match rest {
        "" => respond_bytes(req, 200, "text/html; charset=utf-8", PAGE.as_bytes().to_vec()),
        "/network.bin" => respond_bytes(req, 200, "application/octet-stream", network_bytes()),
        // The world's coastlines, the same twenty kilobytes the drawn maps use, so the page is
        // a map and not a scatter of dots.
        "/land.bin" => respond_bytes(req, 200, "application/octet-stream", include_bytes!("../../data/world_land.bin").to_vec()),
        // Every type the performance model can actually fly, for the page's own dropdown. Not
        // every type the airframe table mentions: an airframe of a type this planner cannot
        // model is a choice that leads only to an error, and offering it is a discourtesy.
        "/types" => {
            let list: Vec<Value> = crate::perf::aircraft::known_types()
                .into_iter()
                .filter_map(|t| crate::perf::aircraft::lookup(t).map(|d| json!({ "icao": t, "name": d.name, "airframes": crate::perf::airframes::of_type(t).len() })))
                .collect();
            respond_json(req, 200, json!({ "types": list }).to_string());
        }
        // Every airframe of a type, for the page's own dropdown and for the weights it fills
        // in once one is picked.
        "/airframes" => {
            let want = params.get("type").and_then(Value::as_str).unwrap_or("");
            let list: Vec<Value> = crate::perf::airframes::of_type(want)
                .into_iter()
                .map(|a| {
                    json!({
                        "registration": a.registration,
                        "label": a.label(),
                        "name": a.name,
                        "engines": a.engines,
                        "pax": a.max_passengers,
                        "oew": a.oew_kg.round(),
                        "mzfw": a.mzfw_kg.round(),
                        "mtow": a.mtow_kg.round(),
                        "mlw": a.mlw_kg.round(),
                        "max_fuel": a.max_fuel_kg.round(),
                        "climb": a.climb,
                        "cruise": a.cruise,
                        "descent": a.descent,
                    })
                })
                .collect();
            respond_json(req, 200, json!({ "airframes": list }).to_string());
        }
        "/start" => {
            let run = start(params);
            respond_json(req, 200, json!({ "id": run.id }).to_string());
        }
        "/events" => {
            let cursor = params.get("since").and_then(Value::as_str).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
            let held = current().lock().ok().and_then(|h| h.clone());
            match held {
                Some(run) => respond_json(req, 200, run.since(cursor).to_string()),
                None => respond_json(req, 200, json!({ "idle": true }).to_string()),
            }
        }
        _ => return Some(req),
    }
    None
}

const PAGE: &str = include_str!("planner.html");
