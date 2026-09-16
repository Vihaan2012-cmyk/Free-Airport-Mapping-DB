//! Terminal output in the style of build tools: a symbol, a coloured label, then the
//! message. Also installs itself as the `log` backend so every module gets the same look.
//!
//! ```text
//! i  info     Welcome to amdbgen, v0.1.0
//! ▶  start    Building 2 airports
//! [EDDF] » ✓ success  Built EDDF in 14.2 s
//! [EDDF] »   file     out/EDDF/ — 45 layers, 5.4 MB geojson
//! ✓  success  Built 2 airports in 31.0 s
//! ```

use console::{style, Emoji, Term};
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::sync::OnceLock;

static TERM: OnceLock<Term> = OnceLock::new();
static VERBOSE: OnceLock<bool> = OnceLock::new();
static SINK: OnceLock<Sink> = OnceLock::new();

/// Receives every status line as plain text: the label (`info`, `success`, `warn`,
/// `error`, `start`, `step`) and the message with its `[SCOPE] ` prefix kept.
pub type Sink = Box<dyn Fn(&str, &str) + Send + Sync>;

/// Also hand every status line to `sink`, for a program with no terminal to print to.
/// Only the first call takes effect.
pub fn set_sink(sink: Sink) {
    let _ = SINK.set(sink);
}

const CHECK: Emoji = Emoji("✓", "+");
const PLAY: Emoji = Emoji("▶", ">");
const CROSS: Emoji = Emoji("✗", "x");
const BANG: Emoji = Emoji("!", "!");
const INFO: Emoji = Emoji("i", "i");
const SEP: Emoji = Emoji("»", ">");

fn term() -> &'static Term {
    TERM.get_or_init(Term::stdout)
}

fn line(symbol: &str, label: &str, colour: fn(&str) -> String, scope: Option<&str>, msg: &str) {
    let scope_txt = scope.map(|s| format!("{} {} ", style(format!("[{s}]")).dim(), SEP)).unwrap_or_default();
    let text = format!("{scope_txt}{} {:<8} {}", colour(symbol), colour(label), msg);
    let _ = term().write_line(&text);
    if let Some(sink) = SINK.get() {
        let plain = console::strip_ansi_codes(msg);
        match scope {
            Some(s) => sink(label, &format!("[{s}] {plain}")),
            None => sink(label, &plain),
        }
    }
}

fn green(s: &str) -> String {
    style(s).green().bold().underlined().to_string()
}
fn cyan(s: &str) -> String {
    style(s).blue().bold().underlined().to_string()
}
fn blue(s: &str) -> String {
    style(s).blue().underlined().to_string()
}
fn yellow(s: &str) -> String {
    style(s).yellow().bold().underlined().to_string()
}
fn red(s: &str) -> String {
    style(s).red().bold().underlined().to_string()
}
fn dim(s: &str) -> String {
    style(s).dim().to_string()
}

/// Split a leading `[SCOPE] ` off a message.
fn split_scope(msg: &str) -> (Option<&str>, &str) {
    if let Some(rest) = msg.strip_prefix('[') {
        if let Some((scope, tail)) = rest.split_once("] ") {
            return (Some(scope), tail);
        }
    }
    (None, msg)
}

pub fn banner(app: &str) {
    let _ = term().write_line("");
    info(&format!("Welcome to {}, v{}", style(app).bold(), env!("CARGO_PKG_VERSION")));
}

pub fn info(msg: &str) {
    let (s, m) = split_scope(msg);
    line(&INFO.to_string(), "info", cyan, s, m);
}

pub fn start(msg: &str) {
    let (s, m) = split_scope(msg);
    let _ = term().write_line("");
    line(&PLAY.to_string(), "start", green, s, m);
}

pub fn success(msg: &str) {
    let (s, m) = split_scope(msg);
    line(&CHECK.to_string(), "success", green, s, m);
}

pub fn warn(msg: &str) {
    let (s, m) = split_scope(msg);
    line(&BANG.to_string(), "warn", yellow, s, m);
}

pub fn error(msg: &str) {
    let (s, m) = split_scope(msg);
    line(&CROSS.to_string(), "error", red, s, m);
}

/// One pipeline step (an API call, a phase of the build).
pub fn step(scope: Option<&str>, msg: &str) {
    line("·", "step", blue, scope, msg);
}

/// Per-layer progress line: `layer 12/45  taxiwayelement  65 features`.
pub fn layer(scope: Option<&str>, i: usize, n: usize, name: &str, count: usize) {
    let scope_txt = scope.map(|s| format!("{} {} ", style(format!("[{s}]")).dim(), SEP)).unwrap_or_default();
    let counter = style(format!("{i:>2}/{n}")).dim().to_string();
    let count_txt = if count == 0 { style("empty").dim().to_string() } else { style(format!("{count} features")).green().to_string() };
    let text = format!("{scope_txt}{} {:<8} {} {:<34} {}", style("·").magenta(), style("layer").magenta().bold().underlined(), counter, name, count_txt);
    let _ = term().write_line(&text);
}

/// A produced file or folder with its size.
pub fn file(scope: Option<&str>, path: &str, detail: &str) {
    let scope_txt = scope.map(|s| format!("{} {} ", style(format!("[{s}]")).dim(), SEP)).unwrap_or_default();
    let text = format!("{scope_txt}  {:<8} {} {} {}", blue("file"), dim(path), dim("—"), style(detail).blue());
    let _ = term().write_line(&text);
}

pub fn human_bytes(b: u64) -> String {
    if b >= 1_000_000 {
        format!("{:.2} MB", b as f64 / 1e6)
    } else if b >= 1_000 {
        format!("{:.2} kB", b as f64 / 1e3)
    } else {
        format!("{b} B")
    }
}

pub fn human_secs(s: f64) -> String {
    if s < 1.0 {
        format!("{:.0} ms", s * 1000.0)
    } else {
        format!("{s:.1} s")
    }
}

struct UiLogger;

impl Log for UiLogger {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= if *VERBOSE.get().unwrap_or(&false) { Level::Debug } else { Level::Info }
    }
    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        // Third-party crates stay quiet unless verbose.
        if !r.target().starts_with("amdbgen") && !*VERBOSE.get().unwrap_or(&false) {
            return;
        }
        let msg = r.args().to_string();
        match r.level() {
            Level::Error => error(&msg),
            Level::Warn => warn(&msg),
            Level::Info => info(&msg),
            _ => {
                let (s, m) = split_scope(&msg);
                line("·", "debug", dim, s, &dim(m));
            }
        }
    }
    fn flush(&self) {}
}

/// Install the styled logger. `verbose` also shows debug lines and other crates.
pub fn init(verbose: bool) {
    let _ = VERBOSE.set(verbose);
    let _ = log::set_boxed_logger(Box::new(UiLogger));
    log::set_max_level(if verbose { LevelFilter::Debug } else { LevelFilter::Info });
}
