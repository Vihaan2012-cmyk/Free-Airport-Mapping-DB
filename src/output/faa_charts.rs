//! Every chart the FAA publishes for the United States -- approaches, departures, arrivals,
//! airport diagrams, hot spots, takeoff and alternate minimums -- downloaded from its
//! digital Terminal Procedures Publication (d-TPP) for one cycle and filed by airport.
//!
//! The d-TPP's index is one XML file for the country naming each chart's PDF. Takeoff and
//! alternate minimums are one PDF for a whole region, listed under every airport in it, so
//! each PDF is downloaded once into `<cycle>/_pdf/` and each airport's folder links to it
//! (a hard link: no second copy on disk). The index also marks each chart unchanged, added
//! or changed since the cycle before; with that cycle already downloaded beside it, the
//! unchanged ones are linked from there and only the rest are fetched.
//!
//! Written to be left running while flying: a few connections, a cap on the download rate,
//! and on Windows the process in background mode (lowest CPU, disk and memory priority).

use crate::sources::dtpp;
use crate::sources::http::Http;
use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub struct Options {
    /// Where cycles go: `<out>/<cycle>/`.
    pub out: PathBuf,
    /// The cycle, as the FAA numbers it ("2609"); the one in force today when left out.
    pub cycle: Option<String>,
    /// Downloads at once.
    pub jobs: usize,
    /// Most bytes a second, over the whole run; 0 for no limit.
    pub rate: u64,
    /// Only these airports (ICAO or FAA identifier); all when empty.
    pub only: Vec<String>,
    /// Run in the background: lowest CPU, disk and memory priority.
    pub background: bool,
}

/// One chart as the index lists it under an airport.
#[derive(Debug, Clone)]
struct Chart {
    state: String,
    city: String,
    airport: String,
    faa: String,
    icao: String,
    seq: u32,
    code: String,
    name: String,
    action: String,
    pdf: String,
}

impl Chart {
    /// The airport's folder: its ICAO code, or the FAA identifier of one that has none.
    fn folder(&self) -> &str {
        if self.icao.is_empty() {
            &self.faa
        } else {
            &self.icao
        }
    }
}

/// Every chart in the index that is in this cycle (deleted ones left out).
fn parse(xml: &str) -> Vec<Chart> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = Vec::new();
    let (mut state, mut city) = (String::new(), String::new());
    let mut apt = Chart { state: String::new(), city: String::new(), airport: String::new(), faa: String::new(), icao: String::new(), seq: 0, code: String::new(), name: String::new(), action: String::new(), pdf: String::new() };
    let mut rec = apt.clone();
    let mut field = String::new();
    let attr = |e: &quick_xml::events::BytesStart, k: &str| e.attributes().flatten().find(|a| a.key.as_ref() == k).map(|a| a.value.trim().to_string()).unwrap_or_default();
    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) => {
                let tag = e.name().as_ref().to_string();
                match tag.as_str() {
                    "state_code" => state = attr(&e, "ID"),
                    "city_name" => city = attr(&e, "ID"),
                    "airport_name" => {
                        apt = Chart { state: state.clone(), city: city.clone(), airport: attr(&e, "ID"), faa: attr(&e, "apt_ident").to_uppercase(), icao: attr(&e, "icao_ident").to_uppercase(), ..apt.clone() };
                    }
                    "record" => rec = Chart { seq: 0, code: String::new(), name: String::new(), action: String::new(), pdf: String::new(), ..apt.clone() },
                    _ => {}
                }
                field = tag;
            }
            Ok(Event::Text(t)) => {
                let text = t.xml_content(quick_xml::XmlVersion::Implicit1_0).trim().to_string();
                match field.as_str() {
                    "chartseq" => rec.seq = text.parse().unwrap_or(0),
                    "chart_code" => rec.code = text,
                    "chart_name" => rec.name = text,
                    "useraction" => rec.action = text,
                    "pdf_name" => rec.pdf = text,
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == "record" && !rec.pdf.is_empty() && !rec.folder().is_empty() && rec.action != "D" && !rec.pdf.to_uppercase().starts_with("DELETED") {
                    out.push(rec.clone());
                }
                field.clear();
            }
            _ => {}
        }
    }
    out
}

/// The cycle before this one: 2609 for 2610, 2513 for 2601.
fn previous_cycle(cycle: &str) -> Option<String> {
    let (y, n): (u32, u32) = (cycle.get(..2)?.parse().ok()?, cycle.get(2..)?.parse().ok()?);
    Some(if n > 1 { format!("{y:02}{:02}", n - 1) } else { format!("{:02}13", y.checked_sub(1)?) })
}

/// A chart's name as a file name: what the index calls it, safe on Windows.
fn file_name(seq_no: usize, c: &Chart) -> String {
    let clean: String = c.name.chars().map(|ch| if "\\/:*?\"<>|".contains(ch) || ch.is_control() { '-' } else { ch }).collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{seq_no:03} {} {}", c.code, clean.trim_end_matches('.'))
}

/// Holds the whole run to a byte rate: each download that puts it ahead waits it out.
struct Limiter {
    rate: u64,
    start: Instant,
    bytes: AtomicU64,
}

impl Limiter {
    fn took(&self, n: u64) {
        let total = self.bytes.fetch_add(n, Ordering::Relaxed) + n;
        if self.rate == 0 {
            return;
        }
        let due = Duration::from_secs_f64(total as f64 / self.rate as f64);
        let elapsed = self.start.elapsed();
        if due > elapsed {
            std::thread::sleep(due - elapsed);
        }
    }
}

#[cfg(windows)]
fn enter_background() {
    use winapi::um::processthreadsapi::{GetCurrentProcess, SetPriorityClass};
    // IDLE_PRIORITY_CLASS, which is what Task Manager shows, then
    // PROCESS_MODE_BACKGROUND_BEGIN, which also lowers disk and memory priority.
    let ok = unsafe { SetPriorityClass(GetCurrentProcess(), 0x0000_0040) != 0 && SetPriorityClass(GetCurrentProcess(), 0x0010_0000) != 0 };
    crate::term::info(if ok { "Running in the background: lowest CPU, disk and memory priority" } else { "Could not lower this process's priority; carrying on" });
}

#[cfg(not(windows))]
fn enter_background() {}

/// Download a cycle's charts. Returns the cycle's folder.
pub fn run(opts: &Options) -> Result<PathBuf> {
    if opts.background {
        enter_background();
    }
    let cycle = opts.cycle.clone().unwrap_or_else(|| dtpp::cycle(chrono::Utc::now().date_naive()));
    let dir = opts.out.join(&cycle);
    let pdf_dir = dir.join("_pdf");
    std::fs::create_dir_all(&pdf_dir).with_context(|| format!("create {}", pdf_dir.display()))?;
    let http = Http::new(120, 0);

    // The index, kept beside the charts it lists.
    let index_path = dir.join("d-TPP_Metafile.xml");
    let xml = match std::fs::read_to_string(&index_path) {
        Ok(t) if t.contains("</digital_tpp>") => t,
        _ => {
            crate::term::step(None, &format!("GET the d-TPP index for cycle {cycle}"));
            let bytes = http.get_bytes(&dtpp::metafile_url(&cycle)).with_context(|| format!("the FAA's d-TPP index for cycle {cycle}"))?;
            std::fs::write(&index_path, &bytes)?;
            String::from_utf8_lossy(&bytes).to_string()
        }
    };
    let mut charts = parse(&xml);
    if !opts.only.is_empty() {
        let only: Vec<String> = opts.only.iter().map(|s| s.trim().to_uppercase()).collect();
        charts.retain(|c| only.contains(&c.icao) || only.contains(&c.faa));
    }
    if charts.is_empty() {
        return Err(anyhow!("the d-TPP index for cycle {cycle} lists no charts{}", if opts.only.is_empty() { "" } else { " for those airports" }));
    }
    let mut unique: BTreeMap<String, bool> = BTreeMap::new(); // pdf -> unchanged since the cycle before
    for c in &charts {
        let unchanged = c.action.is_empty();
        unique.entry(c.pdf.clone()).and_modify(|u| *u &= unchanged).or_insert(unchanged);
    }
    let airports = charts.iter().map(Chart::folder).collect::<std::collections::HashSet<_>>().len();
    crate::term::start(&format!("FAA charts, cycle {cycle}: {} charts at {airports} airports, {} PDFs, into {}", charts.len(), unique.len(), dir.display()));

    let prev_pdf = previous_cycle(&cycle).map(|p| opts.out.join(p).join("_pdf")).filter(|p| p.is_dir());
    let limiter = Limiter { rate: opts.rate, start: Instant::now(), bytes: AtomicU64::new(0) };
    let (done, linked, fetched, failed) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let total = unique.len();
    let t0 = Instant::now();
    let work: Vec<(&String, &bool)> = unique.iter().collect();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(opts.jobs.max(1)).build()?;
    pool.install(|| {
        work.par_iter().for_each(|(pdf, unchanged)| {
            let dest = pdf_dir.join(pdf);
            let have = std::fs::metadata(&dest).is_ok_and(|m| m.len() > 0);
            if !have {
                // Unchanged since the cycle before, and that cycle is here: the same file.
                let reused = **unchanged && prev_pdf.as_ref().is_some_and(|p| std::fs::hard_link(p.join(pdf), &dest).is_ok() || std::fs::copy(p.join(pdf), &dest).is_ok());
                if reused {
                    linked.fetch_add(1, Ordering::Relaxed);
                } else {
                    match fetch(&http, &cycle, pdf, &dest) {
                        Ok(n) => {
                            fetched.fetch_add(1, Ordering::Relaxed);
                            limiter.took(n);
                        }
                        Err(e) => {
                            failed.fetch_add(1, Ordering::Relaxed);
                            crate::term::warn(&format!("{pdf}: {e:#}"));
                        }
                    }
                }
            }
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 500 == 0 || n == total {
                let mb = limiter.bytes.load(Ordering::Relaxed) as f64 / 1e6;
                let secs = t0.elapsed().as_secs_f64().max(0.1);
                crate::term::info(&format!("{n}/{total} PDFs ({} downloaded, {} from the cycle before), {mb:.0} MB at {:.1} MB/s", fetched.load(Ordering::Relaxed), linked.load(Ordering::Relaxed), mb / secs));
            }
        });
    });

    // Each airport's folder: its charts in the index's order, named as the index names them.
    let mut by_airport: BTreeMap<String, Vec<&Chart>> = BTreeMap::new();
    for c in &charts {
        by_airport.entry(c.folder().to_string()).or_default().push(c);
    }
    let mut rows = Vec::new();
    for (folder, mut list) in by_airport {
        list.sort_by_key(|c| c.seq);
        let adir = dir.join(&folder);
        std::fs::create_dir_all(&adir)?;
        let mut used: HashMap<String, usize> = HashMap::new();
        for (i, c) in list.iter().enumerate() {
            let src = pdf_dir.join(&c.pdf);
            if !src.is_file() {
                continue;
            }
            let base = file_name(i + 1, c);
            let k = used.entry(base.clone()).or_insert(0);
            *k += 1;
            let name = if *k == 1 { format!("{base}.pdf") } else { format!("{base} ({k}).pdf") };
            let dest = adir.join(&name);
            if !dest.is_file() {
                if std::fs::hard_link(&src, &dest).is_err() {
                    std::fs::copy(&src, &dest).with_context(|| format!("put {} in {}", c.pdf, adir.display()))?;
                }
            }
            rows.push([c.state.clone(), c.city.clone(), c.airport.clone(), c.faa.clone(), c.icao.clone(), c.code.clone(), c.name.clone(), format!("{folder}/{name}"), c.pdf.clone()]);
        }
    }
    let mut w = csv::Writer::from_path(dir.join("index.csv"))?;
    w.write_record(["state", "city", "airport", "faa_id", "icao", "code", "chart", "file", "faa_pdf"])?;
    for r in &rows {
        w.write_record(r)?;
    }
    w.flush()?;

    let f = failed.load(Ordering::Relaxed);
    let mb = limiter.bytes.load(Ordering::Relaxed) as f64 / 1e6;
    let msg = format!(
        "FAA charts, cycle {cycle}: {} charts filed at {airports} airports ({} PDFs downloaded, {mb:.0} MB; {} from the cycle before) in {}",
        rows.len(),
        fetched.load(Ordering::Relaxed),
        linked.load(Ordering::Relaxed),
        crate::term::human_secs(t0.elapsed().as_secs_f64())
    );
    if f == 0 {
        crate::term::success(&msg);
    } else {
        crate::term::warn(&format!("{msg}; {f} PDFs failed: run it again to fetch just those"));
    }
    Ok(dir)
}

/// One PDF, checked to be one, written whole or not at all. Returns its size.
fn fetch(http: &Http, cycle: &str, pdf: &str, dest: &Path) -> Result<u64> {
    let bytes = http.get_bytes(&dtpp::chart_url(cycle, pdf))?;
    if !bytes.starts_with(b"%PDF") {
        return Err(anyhow!("not a PDF ({} bytes)", bytes.len()));
    }
    let part = dest.with_extension("part");
    std::fs::write(&part, &bytes)?;
    std::fs::rename(&part, dest)?;
    Ok(bytes.len() as u64)
}

/// A byte count as `4M`, `500K` or plain bytes.
pub fn parse_rate(s: &str) -> Result<u64> {
    let s = s.trim().to_uppercase();
    let (num, mult) = match s.chars().last() {
        Some('M') => (&s[..s.len() - 1], 1_000_000.0),
        Some('K') => (&s[..s.len() - 1], 1_000.0),
        _ => (s.as_str(), 1.0),
    };
    let v: f64 = num.trim().parse().map_err(|_| anyhow!("a rate is a number of bytes a second, like 4M or 500K, not {s}"))?;
    Ok((v * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"<?xml version="1.0"?><digital_tpp cycle="2609"><state_code ID="MA"><city_name ID="BOSTON"><airport_name ID="GENERAL EDWARD LAWRENCE LOGAN INTL" apt_ident="BOS" icao_ident="KBOS"><record><chartseq>10100</chartseq><chart_code>MIN</chart_code><chart_name>TAKEOFF MINIMUMS</chart_name><useraction></useraction><pdf_name>NE1TO.PDF</pdf_name></record><record><chartseq>70000</chartseq><chart_code>APD</chart_code><chart_name>AIRPORT DIAGRAM</chart_name><useraction>C</useraction><pdf_name>00058AD.PDF</pdf_name></record><record><chartseq>53400</chartseq><chart_code>IAP</chart_code><chart_name>VOR/DME RWY 27</chart_name><useraction>A</useraction><pdf_name>00058VDM27.PDF</pdf_name></record><record><chartseq>53500</chartseq><chart_code>IAP</chart_code><chart_name>OLD RWY 9</chart_name><useraction>D</useraction><pdf_name>DELETED_JOB.PDF</pdf_name></record></airport_name></city_name><city_name ID="BEVERLY"><airport_name ID="BEVERLY RGNL" apt_ident="BVY" icao_ident=""><record><chartseq>10100</chartseq><chart_code>MIN</chart_code><chart_name>TAKEOFF MINIMUMS</chart_name><useraction></useraction><pdf_name>NE1TO.PDF</pdf_name></record></airport_name></city_name></state_code></digital_tpp>"#;

    #[test]
    fn reads_every_chart_and_leaves_out_the_deleted() {
        let c = parse(INDEX);
        assert_eq!(c.len(), 4);
        assert_eq!((c[0].state.as_str(), c[0].city.as_str(), c[0].icao.as_str(), c[0].faa.as_str()), ("MA", "BOSTON", "KBOS", "BOS"));
        assert!(c.iter().all(|x| x.pdf != "DELETED_JOB.PDF"));
        // An airport with no ICAO code is filed under its FAA identifier.
        assert_eq!(c[3].folder(), "BVY");
        assert_eq!(c[3].pdf, "NE1TO.PDF");
        assert_eq!(c[1].action, "C");
    }

    #[test]
    fn file_names_are_safe_and_in_order() {
        let c = &parse(INDEX)[2];
        assert_eq!(file_name(3, c), "003 IAP VOR-DME RWY 27");
    }

    #[test]
    fn cycles_count_back_over_the_year() {
        assert_eq!(previous_cycle("2610").as_deref(), Some("2609"));
        assert_eq!(previous_cycle("2701").as_deref(), Some("2613"));
    }

    #[test]
    fn rates_read_as_people_write_them() {
        assert_eq!(parse_rate("4M").unwrap(), 4_000_000);
        assert_eq!(parse_rate("500k").unwrap(), 500_000);
        assert_eq!(parse_rate("0").unwrap(), 0);
        assert!(parse_rate("fast").is_err());
    }
}
