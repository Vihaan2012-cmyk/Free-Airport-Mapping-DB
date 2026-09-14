//! HTTP server implementing the Navigraph AMDB API surface exactly as the official
//! `@navigraph/amdb` SDK calls it:
//!
//! * `GET /v1/cycle`
//! * `GET /v1/search?q=`                                  (prefix match on idarpt, iata, name)
//! * `GET /v1/{ICAO}?include=a,b&exclude=c&projection=&precision=`
//! * `GET /v1/{ICAO}/{layer}?projection=&precision=`
//!
//! Any path prefix before `/v1/` is ignored so SDK-style hosts also land here.
//! Authorization headers are accepted and ignored.

use super::compat::{feattype, layer_from_client_name, NAVIGRAPH_LAYERS};
use super::store::{AirportData, Store, XpState};
use crate::geom::ops;
use crate::model::{AmdbFeature, Layer};
use crate::output::geojson::geometry_to_json;
use anyhow::Result;
use geo::Centroid;
use geo_types::{Coord, Geometry, LineString, Point};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, Server};

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("header")
}

fn respond_json(req: Request, status: u16, body: String) {
    // CORS: panel browsers (Coherent GT) preflight any request carrying an Authorization
    // header, and neither the spec nor that engine treat "*" as covering it, so echo the
    // headers the client asked for, or a fixed list.
    let asked = req.headers().iter().find(|h| h.field.equiv("Access-Control-Request-Headers")).map(|h| h.value.as_str().to_string()).filter(|s| !s.trim().is_empty());
    let allow_headers = asked.unwrap_or_else(|| "Authorization, Accept, Content-Type, X-Requested-With, Origin".to_string());
    let mut resp = Response::from_string(body).with_status_code(status);
    resp.add_header(header("Content-Type", "application/json; charset=utf-8"));
    resp.add_header(header("Access-Control-Allow-Origin", "*"));
    resp.add_header(header("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS"));
    resp.add_header(header("Access-Control-Allow-Headers", &allow_headers));
    resp.add_header(header("Access-Control-Expose-Headers", "Content-Length, Content-Type"));
    resp.add_header(header("Access-Control-Max-Age", "86400"));
    resp.add_header(header("Cache-Control", "no-store"));
    let _ = req.respond(resp);
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() + 1 && i + 2 <= bytes.len() - 1 => {
                let h = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                if let Some(v) = h {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for kv in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        m.insert(url_decode(k), Value::from(url_decode(v)));
    }
    m
}

/// Current AIRAC-style cycle (28-day cycles from a known start).
pub fn cycle_json() -> Value {
    let epoch = chrono::NaiveDate::from_ymd_opt(2026, 9, 3).unwrap(); // AIRAC 2609 start
    let today = chrono::Utc::now().date_naive();
    let n = (today - epoch).num_days().div_euclid(28);
    let start = epoch + chrono::Duration::days(n * 28);
    let end = start + chrono::Duration::days(28);
    let year: i32 = start.format("%Y").to_string().parse().unwrap();
    let jan = chrono::NaiveDate::from_ymd_opt(year, 1, 1).unwrap();
    let first_in_year = {
        let k = (jan - epoch).num_days().div_euclid(28);
        let mut s = epoch + chrono::Duration::days(k * 28);
        if s < jan {
            s += chrono::Duration::days(28);
        }
        s
    };
    let idx = (start - first_in_year).num_days() / 28 + 1;
    json!({
        "cycle_start_date": start.to_string(),
        "cycle_end_date": end.to_string(),
        "import_time": start.to_string(),
        "airac_cycle": format!("{}{idx:02}", start.format("%y")).parse::<i64>().unwrap_or(0),
    })
}

fn snap(c: Coord<f64>, precision: Option<f64>) -> Coord<f64> {
    match precision {
        Some(p) if p > 0.0 => Coord { x: (c.x / p).round() * p, y: (c.y / p).round() * p },
        _ => c,
    }
}

fn snap_geometry(g: &Geometry<f64>, precision: Option<f64>) -> Geometry<f64> {
    let Some(_) = precision else { return g.clone() };
    let ls = |l: &LineString<f64>| LineString(l.0.iter().map(|c| snap(*c, precision)).collect());
    match g {
        Geometry::Point(p) => Geometry::Point(Point(snap(p.0, precision))),
        Geometry::LineString(l) => Geometry::LineString(ls(l)),
        Geometry::Polygon(p) => Geometry::Polygon(geo_types::Polygon::new(ls(p.exterior()), p.interiors().iter().map(ls).collect())),
        Geometry::MultiPolygon(m) => Geometry::MultiPolygon(geo_types::MultiPolygon(m.0.iter().map(|p| geo_types::Polygon::new(ls(p.exterior()), p.interiors().iter().map(ls).collect())).collect())),
        Geometry::MultiLineString(m) => Geometry::MultiLineString(geo_types::MultiLineString(m.0.iter().map(ls).collect())),
        other => other.clone(),
    }
}

/// Geometry-derived members Navigraph adds: `centroid` for polygons, `midpoint` and
/// `longest_segment` for lines, both in the response projection.
fn derived_props(geom: &Geometry<f64>, decimals: i32) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    match geom {
        Geometry::Polygon(_) | Geometry::MultiPolygon(_) => {
            if let Some(c) = geom.centroid() {
                out.push(("centroid", geometry_to_json(&Geometry::Point(c), decimals)));
            }
        }
        Geometry::LineString(l) => {
            let len = ops::length(l);
            let mid = ops::point_at(l, len / 2.0);
            out.push(("midpoint", geometry_to_json(&Geometry::Point(Point(mid)), decimals)));
            let longest = l.0.windows(2).map(|w| ops::dist(w[0], w[1])).fold(0.0, f64::max);
            out.push(("longest_segment", Value::from((longest * 100.0).round() / 100.0)));
        }
        Geometry::MultiLineString(m) => {
            if let Some(l) = m.0.iter().max_by(|a, b| ops::length(a).partial_cmp(&ops::length(b)).unwrap()) {
                let len = ops::length(l);
                out.push(("midpoint", geometry_to_json(&Geometry::Point(Point(ops::point_at(l, len / 2.0))), decimals)));
                let longest = l.0.windows(2).map(|w| ops::dist(w[0], w[1])).fold(0.0, f64::max);
                out.push(("longest_segment", Value::from((longest * 100.0).round() / 100.0)));
            }
        }
        _ => {}
    }
    out
}

/// One layer as a FeatureCollection in the requested projection.
fn layer_collection(ap: &AirportData, layer: Layer, wgs84: bool, precision: Option<f64>) -> Value {
    let decimals = if wgs84 { 7 } else { 2 };
    let feats = ap.layers.get(&layer).map(Vec::as_slice).unwrap_or(&[]);
    let arr: Vec<Value> = feats
        .iter()
        .map(|f: &AmdbFeature| {
            let mut geom = if wgs84 { ap.frame.to_wgs84(&f.geom) } else { f.geom.clone() };
            geom = snap_geometry(&geom, precision);
            let mut props = f.props.clone();
            for (k, v) in derived_props(&geom, decimals) {
                props.insert(k.to_string(), v);
            }
            json!({"type":"Feature","geometry": geometry_to_json(&geom, decimals),"properties": Value::Object(props)})
        })
        .collect();
    json!({"type":"FeatureCollection","features": arr})
}

/// Friendly name for the calling add-on, from its User-Agent.
fn client_name(agent: &str) -> String {
    let a = agent.to_ascii_lowercase();
    if a.contains("flybywire") || a.contains("fbw") {
        "FlyByWire".to_string()
    } else if a.contains("inibuilds") || a.contains("a350") {
        "iniBuilds".to_string()
    } else if a.contains("wasm") || a.contains("msfs") || a.contains("flightsimulator") {
        format!("MSFS ({agent})")
    } else if a.contains("coherent") || a.contains("chrome") {
        "aircraft panel (Coherent GT)".to_string()
    } else if agent.is_empty() {
        "unknown client".to_string()
    } else {
        agent.to_string()
    }
}

/// Plain-text (Lua) response for the X-Plane route.
fn respond_text(req: Request, status: u16, body: &str) {
    let mut resp = Response::from_string(body).with_status_code(status);
    resp.add_header(header("Content-Type", "text/plain; charset=utf-8"));
    resp.add_header(header("Access-Control-Allow-Origin", "*"));
    resp.add_header(header("Cache-Control", "no-store"));
    let _ = req.respond(resp);
}

/// A Lua double-quoted string.
fn lua_q(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => {}
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// The X-Plane moving map: `/xp/nearest?lat&lon` or `/xp/{ICAO}`. Returns a Lua chunk
/// (`return {...}`) the FlyWithLua script loads: the airport when built, or
/// `{building="ICAO"}` while a background build runs.
fn handle_xp(store: Arc<Store>, req: Request, rest: &str, params: &Map<String, Value>) {
    let icao = if rest.eq_ignore_ascii_case("nearest") {
        let num = |k: &str| params.get(k).and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok());
        match (num("lat"), num("lon")) {
            (Some(lat), Some(lon)) if lat.abs() <= 90.0 && lon.abs() <= 180.0 => match store.nearest(lat, lon, 80.0, 1).first().and_then(|r| r.get("idarpt")).and_then(Value::as_str) {
                Some(i) => i.to_string(),
                None => return respond_text(req, 200, "return {}\n"),
            },
            _ => return respond_text(req, 400, "return {error=\"need lat and lon\"}\n"),
        }
    } else if rest.len() == 4 && rest.chars().all(|c| c.is_ascii_alphanumeric()) {
        rest.to_uppercase()
    } else {
        return respond_text(req, 404, "return {}\n");
    };
    match store.xplane_state(&icao) {
        XpState::Ready(dir) => match crate::output::xplane::render(&dir) {
            Ok(s) => {
                crate::term::success(&format!("[{icao}] Served X-Plane OANS ({})", crate::term::human_bytes(s.len() as u64)));
                respond_text(req, 200, &s);
            }
            Err(e) => respond_text(req, 200, &format!("return {{error={}}}\n", lua_q(&format!("{e:#}")))),
        },
        XpState::Building => respond_text(req, 200, &format!("return {{building={}}}\n", lua_q(&icao))),
    }
}

fn handle(store: Arc<Store>, req: Request) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let params = parse_query(query);
    if *req.method() == Method::Options {
        let origin = req.headers().iter().find(|h| h.field.equiv("Origin")).map(|h| h.value.as_str().to_string()).unwrap_or_default();
        crate::term::step(None, &format!("OPTIONS {}  CORS preflight from {}", path, if origin.is_empty() { "unknown origin".to_string() } else { origin }));
        respond_json(req, 204, String::new());
        return;
    }
    if let Some(p) = path.find("/xp/") {
        let rest = path[p + 4..].trim_matches('/').to_string();
        handle_xp(store, req, &rest, &params);
        return;
    }
    let Some(pos) = path.find("/v1/") else {
        respond_json(req, 404, json!({"error":"not found","hint":"expected /v1/..."}).to_string());
        return;
    };
    let rest = path[pos + 4..].trim_matches('/');
    let agent = req.headers().iter().find(|h| h.field.equiv("User-Agent")).map(|h| h.value.as_str().to_string()).unwrap_or_default();
    let auth = req.headers().iter().any(|h| h.field.equiv("Authorization"));
    crate::term::step(None, &format!("{} /v1/{}{}  from {}{}", req.method(), rest, if query.is_empty() { String::new() } else { format!("?{}", if query.len() > 90 { format!("{}…", &query[..90]) } else { query.to_string() }) }, client_name(&agent), if auth { " (with token)" } else { "" }));
    let t0 = std::time::Instant::now();
    // Navigraph's API answers in EPSG:4326 (lat/lon) unless a projection is asked for;
    // FlyByWire asks for NAVIGRAPH:ARP_AZEQ explicitly, the GM5 A220 map asks for nothing.
    let projection = params.get("projection").and_then(Value::as_str).unwrap_or("EPSG:4326").to_string();
    let wgs84 = projection.eq_ignore_ascii_case("EPSG:4326");
    let precision = params.get("precision").and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok());
    let mut parts = rest.splitn(2, '/');
    let head = parts.next().unwrap_or("");
    let tail = parts.next();
    match (head, tail) {
        ("cycle", None) => respond_json(req, 200, cycle_json().to_string()),
        ("search", None) => {
            let q = params.get("q").and_then(Value::as_str).unwrap_or("");
            let results = store.search(q);
            crate::term::info(&format!("Search {:?}: {} airports", q, results.len()));
            respond_json(req, 200, Value::Array(results).to_string());
        }
        ("nearest", None) => {
            let num = |k: &str| params.get(k).and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok());
            match (num("lat"), num("lon")) {
                (Some(lat), Some(lon)) if lat.abs() <= 90.0 && lon.abs() <= 180.0 => {
                    let radius = num("radius_km").unwrap_or(60.0).clamp(1.0, 500.0);
                    let limit = num("limit").map(|v| v as usize).unwrap_or(16).clamp(1, 64);
                    let rows = store.nearest(lat, lon, radius, limit);
                    crate::term::info(&format!("Nearest to {lat:.3},{lon:.3} within {radius:.0} km: {}", rows.iter().filter_map(|r| r.get("idarpt").and_then(Value::as_str)).take(6).collect::<Vec<_>>().join(" ")));
                    respond_json(req, 200, Value::Array(rows).to_string());
                }
                _ => respond_json(req, 400, json!({"error":"nearest needs lat= and lon= in degrees"}).to_string()),
            }
        }
        ("", None) => respond_json(req, 200, json!({"service":"amdb-bridge","version":env!("CARGO_PKG_VERSION"),"loaded":store.loaded_count()}).to_string()),
        (icao, layer_name) if icao.len() == 4 && icao.chars().all(|c| c.is_ascii_alphanumeric()) => {
            let ap = match store.airport(icao) {
                Ok(a) => a,
                Err(e) => {
                    log::error!("{icao}: {e:#}");
                    respond_json(req, 404, json!({"error": format!("{e:#}")}).to_string());
                    return;
                }
            };
            match layer_name {
                Some(name) => {
                    // Single layer: the bare FeatureCollection.
                    match layer_from_client_name(name).filter(|l| feattype(*l).is_some()) {
                        Some(l) => {
                            crate::term::success(&format!("[{icao}] Served {name} in {}", crate::term::human_secs(t0.elapsed().as_secs_f64())));
                            respond_json(req, 200, layer_collection(&ap, l, wgs84, precision).to_string());
                        }
                        None => respond_json(req, 404, json!({"error": format!("unknown layer {name}")}).to_string()),
                    }
                }
                None => {
                    let include: Vec<(String, Layer)> = params.get("include").and_then(Value::as_str).unwrap_or("").split(',').map(str::trim).filter(|s| !s.is_empty()).filter_map(|n| layer_from_client_name(n).map(|l| (n.to_ascii_lowercase(), l))).collect();
                    let exclude: Vec<Layer> = params.get("exclude").and_then(Value::as_str).unwrap_or("").split(',').filter_map(layer_from_client_name).collect();
                    let wanted: Vec<(String, Layer)> = if include.is_empty() {
                        NAVIGRAPH_LAYERS.iter().map(|l| (l.name().to_string(), *l)).collect()
                    } else {
                        include
                    }
                    .into_iter()
                    .filter(|(_, l)| !exclude.contains(l))
                    .collect();
                    let mut body = Map::new();
                    for (name, l) in wanted {
                        body.insert(name, layer_collection(&ap, l, wgs84, precision));
                    }
                    crate::term::success(&format!("[{icao}] Served {} layers ({}) in {}", body.len(), if wgs84 { "lat/lon" } else { "ARP metres" }, crate::term::human_secs(t0.elapsed().as_secs_f64())));
                    respond_json(req, 200, Value::Object(body).to_string());
                }
            }
        }
        _ => {
            crate::term::warn(&format!("Unknown request /v1/{rest}"));
            respond_json(req, 404, json!({"error":"not found","path":rest}).to_string())
        }
    }
}

/// Listeners to run. HTTPS answers the redirected Navigraph host; HTTP is for local
/// tools and patched bundles. Both bind to loopback only.
pub struct Listen {
    pub http_port: Option<u16>,
    /// (port, certificate PEM, private key PEM)
    pub https: Option<(u16, Vec<u8>, Vec<u8>)>,
}

/// Run the server until the process exits.
pub fn serve(store: Store, listen: Listen) -> Result<()> {
    let store = Arc::new(store);
    let mut handles = Vec::new();
    if let Some((port, cert, key)) = listen.https {
        let addr = format!("127.0.0.1:{port}");
        let server = Server::https(&addr, tiny_http::SslConfig { certificate: cert, private_key: key }).map_err(|e| anyhow::anyhow!("bind https {addr}: {e}"))?;
        crate::term::success(&format!("Listening on https://{addr}/v1/  (airports in {})", store.out.display()));
        let st = store.clone();
        handles.push(std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let st = st.clone();
                std::thread::spawn(move || handle(st, req));
            }
        }));
    }
    if let Some(port) = listen.http_port {
        let addr = format!("127.0.0.1:{port}");
        let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("bind http {addr}: {e}"))?;
        crate::term::info(&format!("Also on http://{addr}/v1/ for local tools"));
        let st = store.clone();
        handles.push(std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let st = st.clone();
                std::thread::spawn(move || handle(st, req));
            }
        }));
    }
    if handles.is_empty() {
        return Err(anyhow::anyhow!("nothing to listen on"));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_query_and_snaps() {
        let m = parse_query("projection=NAVIGRAPH%3AARP_AZEQ&include=a%2Cb&q=");
        assert_eq!(m["projection"], "NAVIGRAPH:ARP_AZEQ");
        assert_eq!(m["include"], "a,b");
        assert_eq!(m["q"], "");
        let c = cycle_json();
        assert_eq!(c["cycle_start_date"].as_str().unwrap().len(), 10);
        let s = snap(Coord { x: 12.34, y: -7.77 }, Some(0.5));
        assert_eq!((s.x, s.y), (12.5, -8.0));
        let d = derived_props(&Geometry::LineString(LineString(vec![Coord { x: 0.0, y: 0.0 }, Coord { x: 10.0, y: 0.0 }, Coord { x: 10.0, y: 4.0 }])), 2);
        assert_eq!(d[1].1, 10.0);
        assert_eq!(d[0].1["coordinates"], json!([7.0, 0.0]));
    }
}
