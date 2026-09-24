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

    let thread_run = run.clone();
    std::thread::spawn(move || {
        let mut opts = crate::ofp::DispatchOptions::new(origin, destination, aircraft);
        opts.payload_kg = payload;
        opts.passengers = pax as u32;
        opts.cost_index = ci;
        opts.offline = offline;
        opts.alternate = alternate;
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
                if let Ok(mut s) = thread_run.summary.lock() {
                    *s = Some(json!({
                        "origin": d.route.origin.icao,
                        "destination": d.route.destination.icao,
                        "aircraft": d.spec.icao_type,
                        "ground_nm": ground.round(),
                        "direct_nm": direct.round(),
                        "over_pct": ((ground / direct.max(1.0) - 1.0) * 100.0).round(),
                        "block_kg": d.perf.fuel.block_kg.round(),
                        "route": read_back,
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
