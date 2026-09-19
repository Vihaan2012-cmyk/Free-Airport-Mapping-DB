//! A live progress page for a bulk build, served on 127.0.0.1 while `prefetch` runs.
//!
//! Progress is counted from the output folder: an airport is done when its
//! `manifest.json` exists. That moves airport by airport, where the build itself reports
//! only after each batch, and it stays right across restarts of the same list.

use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime};
use tiny_http::{Header, Response, Server};

pub const DEFAULT_PORT: u16 = 8771;

pub struct Progress {
    label: String,
    icaos: Vec<String>,
    out: PathBuf,
    started: Instant,
    /// Airports already built when this run started, so the rate counts only this run.
    done_at_start: usize,
    failed: Mutex<Vec<(String, String)>>,
    finished: AtomicBool,
}

static CURRENT: OnceLock<Arc<Progress>> = OnceLock::new();

fn is_done(out: &std::path::Path, icao: &str) -> bool {
    out.join(icao).join("manifest.json").is_file()
}

/// Note a failed airport on the page, if one is being served.
pub fn record_failed(icao: &str, error: &str) {
    if let Some(p) = CURRENT.get() {
        p.failed.lock().unwrap().push((icao.to_string(), error.chars().take(200).collect()));
    }
}

/// Mark the run finished on the page.
pub fn finish() {
    if let Some(p) = CURRENT.get() {
        p.finished.store(true, Ordering::Relaxed);
    }
}

impl Progress {
    fn status(&self) -> serde_json::Value {
        let done: Vec<&String> = self.icaos.iter().filter(|i| is_done(&self.out, i)).collect();
        let total = self.icaos.len();
        let failed = self.failed.lock().unwrap().clone();
        let this_run = done.len().saturating_sub(self.done_at_start);
        let elapsed = self.started.elapsed().as_secs_f64();
        let remaining = total.saturating_sub(done.len() + failed.len());
        let per_hour = if elapsed > 60.0 { this_run as f64 / elapsed * 3600.0 } else { 0.0 };
        let eta = if this_run > 0 { Some(elapsed / this_run as f64 * remaining as f64) } else { None };
        // The most recently finished airports, newest first.
        let mut recent: Vec<(SystemTime, &String)> = done
            .iter()
            .filter_map(|i| std::fs::metadata(self.out.join(i).join("manifest.json")).and_then(|m| m.modified()).ok().map(|t| (t, *i)))
            .collect();
        recent.sort_by(|a, b| b.0.cmp(&a.0));
        let recent: Vec<serde_json::Value> = recent
            .into_iter()
            .take(12)
            .map(|(t, i)| {
                let name = std::fs::read_to_string(self.out.join(i).join("manifest.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
                    .unwrap_or_default();
                let ago = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
                json!({"icao": i, "name": name, "ago": ago})
            })
            .collect();
        json!({
            "label": self.label,
            "total": total,
            "done": done.len(),
            "done_this_run": this_run,
            "failed": failed.iter().map(|(i, e)| json!({"icao": i, "error": e})).collect::<Vec<_>>(),
            "remaining": remaining,
            "elapsed": elapsed,
            "per_hour": per_hour,
            "eta": eta,
            "recent": recent,
            "finished": self.finished.load(Ordering::Relaxed),
            "out": self.out.display().to_string(),
        })
    }
}

/// Start serving the page for this run. Returns the address, or None when the port is
/// taken (the build goes ahead either way).
pub fn serve(label: &str, icaos: &[String], out: PathBuf, port: u16) -> Option<String> {
    let done_at_start = icaos.iter().filter(|i| is_done(&out, i)).count();
    let p = Arc::new(Progress { label: label.to_string(), icaos: icaos.to_vec(), out, started: Instant::now(), done_at_start, failed: Mutex::new(Vec::new()), finished: AtomicBool::new(false) });
    let _ = CURRENT.set(p.clone());
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr).ok()?;
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let path = req.url().split('?').next().unwrap_or("/").to_string();
            let (body, kind) = if path == "/status.json" { (p.status().to_string(), "application/json") } else { (PAGE.to_string(), "text/html; charset=utf-8") };
            let resp = Response::from_string(body)
                .with_header(Header::from_bytes(&b"Content-Type"[..], kind.as_bytes()).unwrap())
                .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap());
            let _ = req.respond(resp);
        }
    });
    Some(format!("http://{addr}/"))
}

const PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>AMDB build progress</title>
<style>
:root{--bg:#f4f6f8;--card:#fff;--ink:#16202b;--dim:#5b6776;--line:#dde3ea;--bar:#e6ebf0;--fill:#1f6feb;--ok:#1a7f37;--bad:#c42b1c}
@media (prefers-color-scheme:dark){:root{--bg:#0e141b;--card:#161e27;--ink:#e6edf3;--dim:#8b98a6;--line:#26313d;--bar:#223040;--fill:#4c9aff;--ok:#3fb950;--bad:#f06a5f}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.5 "Segoe UI",system-ui,sans-serif;padding:24px 16px}
main{max-width:760px;margin:0 auto;display:grid;gap:16px}
.card{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:20px}
h1{margin:0;font-size:20px;font-weight:600}.sub{color:var(--dim);font-size:13px;word-break:break-all}
.big{display:flex;align-items:baseline;gap:12px;flex-wrap:wrap}.pct{font-size:44px;font-weight:600;font-variant-numeric:tabular-nums}
.count{color:var(--dim);font-variant-numeric:tabular-nums}
.bar{height:12px;background:var(--bar);border-radius:6px;overflow:hidden;margin-top:12px}.bar>i{display:block;height:100%;background:var(--fill);width:0;transition:width .6s}
.stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(130px,1fr));gap:12px}
.stat b{display:block;font-size:22px;font-weight:600;font-variant-numeric:tabular-nums}.stat span{color:var(--dim);font-size:13px}
h2{margin:0 0 10px;font-size:15px;font-weight:600}
ul{list-style:none;margin:0;padding:0}li{display:flex;gap:12px;padding:6px 0;border-top:1px solid var(--line);font-size:14px}li:first-child{border-top:0}
.icao{font-family:Consolas,monospace;font-weight:600;min-width:48px}.name{flex:1;color:var(--dim);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.ago{color:var(--dim);font-variant-numeric:tabular-nums}
.bad .icao{color:var(--bad)}.done{color:var(--ok);font-weight:600}.empty{color:var(--dim);font-size:14px}
</style></head><body><main>
<section class="card"><h1>Building airports</h1><div class="sub" id="label"></div>
<div class="big"><span class="pct" id="pct">-</span><span class="count" id="count"></span><span id="state"></span></div>
<div class="bar"><i id="fill"></i></div></section>
<section class="card stats">
<div class="stat"><b id="rate">-</b><span>airports per hour</span></div>
<div class="stat"><b id="eta">-</b><span>time left</span></div>
<div class="stat"><b id="failed">0</b><span>failed</span></div>
<div class="stat"><b id="elapsed">-</b><span>running for</span></div></section>
<section class="card"><h2>Just finished</h2><ul id="recent"><li class="empty">Nothing yet - the first airports take a minute or two.</li></ul></section>
<section class="card" id="failbox" hidden><h2>Failed (will be retried if you run the same command again)</h2><ul id="fails"></ul></section>
</main><script>
const $=id=>document.getElementById(id);
const dur=s=>{if(s==null)return"-";s=Math.round(s);const h=Math.floor(s/3600),m=Math.floor(s%3600/60);return h?h+" h "+m+" min":m?m+" min":s+" s"};
const esc=t=>String(t).replace(/[&<>"]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c]));
async function tick(){try{const r=await fetch("status.json",{cache:"no-store"});const s=await r.json();
const pct=s.total?Math.floor(s.done/s.total*1000)/10:0;$("pct").textContent=pct+"%";$("fill").style.width=pct+"%";
$("count").textContent=s.done+" of "+s.total+" built";$("label").textContent=s.label+"  ->  "+s.out;
$("state").innerHTML=s.finished?'<span class="done">Finished</span>':"";
$("rate").textContent=s.per_hour?Math.round(s.per_hour):"-";$("eta").textContent=s.finished?"done":dur(s.eta);
$("failed").textContent=s.failed.length;$("elapsed").textContent=dur(s.elapsed);
if(s.recent.length)$("recent").innerHTML=s.recent.map(a=>`<li><span class="icao">${esc(a.icao)}</span><span class="name">${esc(a.name)}</span><span class="ago">${dur(a.ago)} ago</span></li>`).join("");
$("failbox").hidden=!s.failed.length;$("fails").innerHTML=s.failed.slice(-30).reverse().map(f=>`<li class="bad"><span class="icao">${esc(f.icao)}</span><span class="name">${esc(f.error)}</span></li>`).join("");
document.title=pct+"% - AMDB build";}catch(e){$("state").textContent="(build not running)"}}
tick();setInterval(tick,3000);
</script></body></html>"#;
