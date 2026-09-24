//! The AMDB Bridge window: one Start/Stop control, what was found on this machine, the
//! options, and the activity log. It lives in the notification area while serving, so
//! closing the window does not stop the maps a flight is using.

use crate::instance::{self, Instance};
use amdbgen::bridge::desktop::{self, A350State, MapState, XPlaneState};
use amdbgen::bridge::service::{self, Running};
use amdbgen::bridge::settings::{dir_size, Settings};
use native_windows_gui as nwg;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LOG_LINES: usize = 500;

/// Where the planning panel starts, and how wide the window is with and without it. The panel
/// is built at this offset always and simply falls outside a narrow window, so showing it is a
/// resize and not a rebuild.
/// A plan being worked out, watched while it runs.
///
/// The search reports where it has reached as it reaches there; this collects those, the window
/// redraws the map from them a few times a second, and the route appears as it is found rather
/// than all at once at the end.
#[derive(Default)]
struct Planning {
    reached: Mutex<Vec<amdbgen::dispatch::LatLon>>,
    best: Mutex<Option<Vec<amdbgen::dispatch::LatLon>>>,
    ends: Mutex<Option<(amdbgen::dispatch::LatLon, amdbgen::dispatch::LatLon)>>,
    done: AtomicBool,
    outcome: Mutex<Option<Result<(String, String), String>>>,
}

impl amdbgen::route::progress::Sink for Planning {
    fn event(&self, event: amdbgen::route::progress::Event) {
        use amdbgen::route::progress::Event as E;
        match event {
            E::Started { origin, destination } => {
                if let Ok(mut e) = self.ends.lock() {
                    *e = Some((origin, destination));
                }
            }
            E::Reached { at, .. } => {
                if let Ok(mut r) = self.reached.lock() {
                    if r.len() < 40_000 {
                        r.push(at);
                    }
                }
            }
            E::Best { path, .. } => {
                if let Ok(mut b) = self.best.lock() {
                    *b = Some(path);
                }
            }
            E::Finished { .. } => {}
        }
    }
}

const PANEL_X: i32 = 660;
const NARROW: u32 = 640;
const WIDE: u32 = 1940;
/// The map inside the panel, in pixels. Drawn by the same renderer that writes `route-map`, at
/// the two-to-one the whole world takes on an equirectangular page, and big enough that a route
/// across a hemisphere is a route and not a smudge.
const MAP_W: i32 = 700;
const MAP_H: i32 = 350;

// Status colours, readable on the system dialog background.
const GREEN: [u8; 3] = [16, 124, 16];
const AMBER: [u8; 3] = [176, 104, 0];
const RED: [u8; 3] = [196, 43, 28];
const GREY: [u8; 3] = [96, 96, 96];

/// State shared with the threads that do slow work and with the log sink.
/// The command-line tool that ships beside this program, which draws the charts.
fn amdbgen_exe() -> Option<PathBuf> {
    let here = std::env::current_exe().ok()?;
    let dir = here.parent()?;
    let name = if cfg!(windows) { "amdbgen.exe" } else { "amdbgen" };
    // Installed, it sits next to the app. Run from a build folder it sits next to the
    // build of this program, which is the same place.
    let beside = dir.join(name);
    beside.is_file().then_some(beside)
}

#[derive(Default)]
struct Shared {
    lines: Mutex<Vec<String>>,
    started: Mutex<Option<anyhow::Result<Running>>>,
    inventory: Mutex<Option<Inventory>>,
    report: Mutex<Option<anyhow::Result<(PathBuf, Option<PathBuf>)>>>,
    cache_bytes: Mutex<Option<u64>>,
    notice: Mutex<Option<nwg::NoticeSender>>,
    log_file: Mutex<Option<File>>,
}

impl Shared {
    fn wake(&self) {
        if let Some(n) = self.notice.lock().unwrap().as_ref() {
            n.notice();
        }
    }
}

fn log_path() -> PathBuf {
    amdbgen::bridge::settings::app_dir().join("bridge.log")
}

/// Send the bridge's status lines to the window and to `bridge.log`.
fn install_sink(shared: &Arc<Shared>, fresh: bool) {
    let path = log_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let too_big = std::fs::metadata(&path).map_or(false, |m| m.len() > 2_000_000);
    let file = if fresh && too_big { File::create(&path).ok() } else { OpenOptions::new().create(true).append(true).open(&path).ok() };
    *shared.log_file.lock().unwrap() = file;
    let sh = shared.clone();
    amdbgen::term::set_sink(Box::new(move |label, msg| {
        let mark = match label {
            "success" => "✓",
            "warn" => "!",
            "error" => "✗",
            "start" => "▶",
            _ => "·",
        };
        let now = chrono::Local::now();
        let line = format!("{}  {mark} {msg}", now.format("%H:%M:%S"));
        if let Some(f) = sh.log_file.lock().unwrap().as_mut() {
            let _ = writeln!(f, "{} {}", now.format("%Y-%m-%d"), line);
        }
        sh.lines.lock().unwrap().push(line);
        sh.wake();
    }));
    amdbgen::term::init(false);
}

// --------------------------------------------------------------------------------------
// Command-line steps for the installer. No window; results go to bridge.log.

fn install_map_everywhere() -> i32 {
    let sims = desktop::detect_sims();
    if sims.is_empty() {
        amdbgen::term::warn("No Microsoft Flight Simulator found; the A220 map can be installed later from AMDB Bridge");
        return 0;
    }
    let mut failed = false;
    for sim in sims {
        match desktop::install_a220_map(&sim.community) {
            Ok(notes) => notes.iter().for_each(|n| amdbgen::term::success(&format!("{}: {n}", sim.name))),
            Err(e) => {
                failed = true;
                amdbgen::term::error(&format!("{}: {e:#}", sim.name));
            }
        }
    }
    i32::from(failed)
}

fn uninstall(relaunched: bool) -> i32 {
    instance::ask_to_quit(Duration::from_secs(10));
    let problems = desktop::uninstall_cleanup();
    let needs_admin = problems.iter().any(|p| p.contains("administrator"));
    for p in &problems {
        amdbgen::term::warn(p);
    }
    // The redirect and the certificate are the only machine-wide changes; ask once, and
    // never from a copy that is already elevated or was itself relaunched.
    if needs_admin && !relaunched && !instance::is_elevated() {
        instance::run_elevated("--uninstall --relaunched", true);
    }
    0
}

/// `--setup-navigraph on|off`: A350/A380X support, for the installer and the option.
/// Asks Windows for administrator rights when this copy does not have them.
fn setup_navigraph(on: bool, relaunched: bool) -> i32 {
    if instance::is_elevated() || relaunched {
        // Do the work here, whatever happens: a copy that has been elevated once must
        // never start another, or a failure repeats itself in an endless chain.
        if let Err(e) = desktop::setup_navigraph(on) {
            amdbgen::term::error(&format!("A350/A380X setup failed: {e:#}. If security software protects the hosts file, allow AMDB Bridge to change it and try again."));
            return 1;
        }
    } else if !instance::run_elevated(if on { "--setup-navigraph on --relaunched" } else { "--setup-navigraph off --relaunched" }, true) {
        amdbgen::term::warn("Administrator permission was not given, so A350/A380X support was not changed");
        return 1;
    }
    let done = desktop::navigraph_ready() == on;
    if done {
        if on {
            desktop::patch_a350_everywhere().iter().for_each(|n| amdbgen::term::success(n));
        }
        let mut s = Settings::load().unwrap_or_default();
        s.navigraph_redirect = on;
        let _ = s.save();
        amdbgen::term::success(if on { "A350/A380X support set up" } else { "A350/A380X support removed" });
    }
    i32::from(!done)
}

fn quit_running() -> i32 {
    i32::from(!instance::ask_to_quit(Duration::from_secs(15)))
}

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let shared = Arc::new(Shared::default());
    let headless = has("--install-a220") || has("--uninstall") || has("--quit") || has("--run-at-login") || has("--setup-navigraph");
    install_sink(&shared, !headless);

    if has("--quit") {
        return quit_running();
    }
    if has("--install-a220") {
        return install_map_everywhere();
    }
    let relaunched = has("--relaunched");
    if has("--uninstall") {
        return uninstall(relaunched);
    }
    if let Some(i) = args.iter().position(|a| a == "--setup-navigraph") {
        return setup_navigraph(args.get(i + 1).map_or(true, |v| v != "off"), relaunched);
    }
    if let Some(i) = args.iter().position(|a| a == "--run-at-login") {
        let on = args.get(i + 1).map_or(true, |v| v != "off");
        return match std::env::current_exe().map_err(anyhow::Error::from).and_then(|exe| desktop::set_run_at_login(on, &exe)) {
            Ok(()) => 0,
            Err(e) => {
                amdbgen::term::error(&format!("{e:#}"));
                1
            }
        };
    }

    let tray_only = has("--tray");
    let Some(inst) = Instance::acquire(Duration::ZERO) else {
        // Already running: bring that window forward instead of opening a second one.
        // Started by Windows or the simulator, stay quiet.
        if !tray_only {
            instance::ask_to_show();
        }
        return 0;
    };

    if let Err(e) = nwg::init() {
        amdbgen::term::error(&format!("could not start the window system: {e}"));
        return 1;
    }
    let mut font = nwg::Font::default();
    let _ = nwg::Font::builder().family("Segoe UI").size(17).build(&mut font);
    nwg::Font::set_global_default(Some(font));

    let app = match App::build(shared, inst, !tray_only) {
        Ok(app) => app,
        Err(e) => {
            nwg::simple_message("AMDB Bridge", &format!("The window could not be created: {e}"));
            return 1;
        }
    };
    app.after_open();
    nwg::dispatch_thread_events();
    app.shutdown();
    0
}

// --------------------------------------------------------------------------------------

/// What the simulators-and-aircraft list shows, worked out on a background thread: the
/// A350 check reads through every package's panel scripts, which takes a while in a
/// well-stocked Community folder.
struct Inventory {
    rows: Vec<[String; 3]>,
    can_install: bool,
    can_remove: bool,
    /// Whether the flight bags' charts come from the bridge: None where there are none.
    tablets_on: Option<bool>,
}

/// Build the list. `redirect` is the A350/A380X option, `serving_redirect` whether the
/// redirect is in place right now.
fn take_inventory(redirect: bool, serving_redirect: bool, xplane_on: bool) -> Inventory {
    let mut rows: Vec<[String; 3]> = Vec::new();
    let mut tablets: Vec<bool> = Vec::new();
    let sims = desktop::detect_sims();
    let ready = redirect && desktop::navigraph_ready();
    for sim in &sims {
        let map = match desktop::a220_map_state(&sim.community) {
            MapState::NotInstalled => "Not installed".to_string(),
            MapState::Installed(v) => format!("Installed (v{v})"),
            MapState::Outdated { installed, available } => format!("Update available (v{installed} → v{available})"),
        };
        rows.push([sim.name.clone(), "Synaptic A220 moving map".into(), map]);
        let navigraph = |when_serving: &str| -> String {
            if serving_redirect {
                when_serving.to_string()
            } else if redirect && ready {
                "Ready when serving".to_string()
            } else if redirect {
                "Not set up: tick the option again".to_string()
            } else {
                "Needs the A350/A380X option below".to_string()
            }
        };
        if let Some(a350) = desktop::a350_state(&sim.community) {
            let status = if a350 == A350State::Patched { "Ready" } else { "Patched when serving starts" };
            rows.push([sim.name.clone(), "iniBuilds A350 OANS".into(), navigraph(status)]);
        }
        if desktop::a380x_in_community(&sim.community) {
            rows.push([sim.name.clone(), "FlyByWire A380X OANS".into(), navigraph("Ready")]);
        }
        for (pkg, _, on) in amdbgen::bridge::patcher::scan_charts(&sim.community) {
            tablets.push(on);
            rows.push([sim.name.clone(), format!("Tablet charts: {}", tablet_name(&pkg)), if on { "From the bridge".into() } else { "Its own (Navigraph)".into() }]);
        }
    }
    if let Some((_, state)) = desktop::xplane_state() {
        let s = match state {
            XPlaneState::NoFlyWithLua => "Needs FlyWithLua",
            XPlaneState::ScriptMissing if xplane_on => "Installed when serving starts",
            XPlaneState::ScriptMissing => "Off",
            XPlaneState::Installed => "Installed",
        };
        rows.push(["X-Plane 12".into(), "FlyWithLua moving map".into(), s.into()]);
    }
    if rows.is_empty() {
        rows.push(["-".into(), "No simulator found on this computer".into(), String::new()]);
    }
    Inventory {
        can_install: !sims.is_empty() && desktop::bundled_a220_map().is_some(),
        can_remove: sims.iter().any(|s| desktop::a220_map_state(&s.community) != MapState::NotInstalled),
        rows,
        // On when any is: the button then turns them all off, which is the safe way round.
        tablets_on: (!tablets.is_empty()).then(|| tablets.iter().any(|on| *on)),
    }
}

/// An aircraft package's folder, as the list names it.
fn tablet_name(package: &str) -> String {
    let p = package.to_ascii_lowercase();
    let name = if p.contains("a350") {
        "iniBuilds A350"
    } else if p.contains("738") || p.contains("737") {
        "PMDG 737"
    } else if p.contains("77er") {
        "PMDG 777-200ER"
    } else if p.contains("77f") {
        "PMDG 777F"
    } else if p.contains("77w") {
        "PMDG 777-300ER"
    } else {
        return package.to_string();
    };
    name.to_string()
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Stopped,
    Starting,
    Serving,
    Failed,
}

struct State {
    settings: Settings,
    running: Option<Running>,
    planner_open: bool,
    planning: Option<Arc<Planning>>,
    /// The image frame keeps only a handle, so the bitmap it shows has to be held here.
    plan_bitmap: Option<nwg::Bitmap>,
    phase: Phase,
    error: String,
    instance: Instance,
    told_about_tray: bool,
    next_detail: Instant,
    next_cache_scan: Instant,
    exiting: bool,
}

#[derive(Default)]
struct Ui {
    window: nwg::Window,
    icon: nwg::Icon,
    small_icon: nwg::Icon,
    font_title: nwg::Font,
    font_status: nwg::Font,
    font_header: nwg::Font,
    font_small: nwg::Font,

    title: nwg::Label,
    version: nwg::Label,
    subtitle: nwg::Label,

    status: nwg::Label,
    detail: nwg::Label,
    start: nwg::Button,

    sims_header: nwg::Label,
    sims: nwg::ListView,
    install: nwg::Button,
    remove: nwg::Button,
    refresh: nwg::Button,
    planner: nwg::Button,

    // The planning panel, which the window widens to show.
    plan_header: nwg::Label,
    plan_airline_label: nwg::Label,
    plan_airline: nwg::TextInput,
    plan_fltnum_label: nwg::Label,
    plan_fltnum: nwg::TextInput,
    plan_flight_label: nwg::Label,
    plan_flight: nwg::TextInput,
    plan_fltrule_label: nwg::Label,
    plan_fltrule: nwg::TextInput,
    plan_from_label: nwg::Label,
    plan_from: nwg::TextInput,
    plan_to_label: nwg::Label,
    plan_to: nwg::TextInput,
    plan_altn_label: nwg::Label,
    plan_altn: nwg::TextInput,
    plan_altcount_label: nwg::Label,
    plan_altcount: nwg::TextInput,
    plan_type_label: nwg::Label,
    plan_type: nwg::TextInput,
    plan_airframe_label: nwg::Label,
    plan_airframe: nwg::TextInput,
    plan_eobt_label: nwg::Label,
    plan_eobt: nwg::TextInput,
    plan_blocktime_label: nwg::Label,
    plan_blocktime: nwg::TextInput,
    plan_climb_label: nwg::Label,
    plan_climb: nwg::TextInput,
    plan_cruise_label: nwg::Label,
    plan_cruise: nwg::TextInput,
    plan_ci_label: nwg::Label,
    plan_ci: nwg::TextInput,
    plan_descent_label: nwg::Label,
    plan_descent: nwg::TextInput,
    plan_layout_label: nwg::Label,
    plan_layout: nwg::TextInput,
    plan_airac_label: nwg::Label,
    plan_airac: nwg::TextInput,
    plan_units_label: nwg::Label,
    plan_units: nwg::TextInput,
    plan_maps_label: nwg::Label,
    plan_maps: nwg::TextInput,
    plan_rules_label: nwg::Label,
    plan_rules: nwg::TextInput,
    plan_taxiout_label: nwg::Label,
    plan_taxiout: nwg::TextInput,
    plan_taxiin_label: nwg::Label,
    plan_taxiin: nwg::TextInput,
    plan_level_label: nwg::Label,
    plan_level: nwg::TextInput,
    plan_deprwy_label: nwg::Label,
    plan_deprwy: nwg::TextInput,
    plan_arrrwy_label: nwg::Label,
    plan_arrrwy: nwg::TextInput,
    plan_pax_label: nwg::Label,
    plan_pax: nwg::TextInput,
    plan_payload_label: nwg::Label,
    plan_payload: nwg::TextInput,
    plan_freight_label: nwg::Label,
    plan_freight: nwg::TextInput,
    plan_zfw_label: nwg::Label,
    plan_zfw: nwg::TextInput,
    plan_reg_label: nwg::Label,
    plan_reg: nwg::TextInput,
    plan_avoid_label: nwg::Label,
    plan_avoid: nwg::TextInput,
    plan_contfuel_label: nwg::Label,
    plan_contfuel: nwg::TextInput,
    plan_resfuel_label: nwg::Label,
    plan_resfuel: nwg::TextInput,
    plan_taxifuel_label: nwg::Label,
    plan_taxifuel: nwg::TextInput,
    plan_blockfuel_label: nwg::Label,
    plan_blockfuel: nwg::TextInput,
    plan_arrfuel_label: nwg::Label,
    plan_arrfuel: nwg::TextInput,
    plan_melfuel_label: nwg::Label,
    plan_melfuel: nwg::TextInput,
    plan_atcfuel_label: nwg::Label,
    plan_atcfuel: nwg::TextInput,
    plan_extrafuel_label: nwg::Label,
    plan_extrafuel: nwg::TextInput,
    plan_navlog: nwg::CheckBox,
    plan_etops: nwg::CheckBox,
    plan_steps: nwg::CheckBox,
    plan_rwyanalysis: nwg::CheckBox,
    plan_notams: nwg::CheckBox,
    plan_firnotams: nwg::CheckBox,
    plan_hazards: nwg::CheckBox,
    plan_rvsm: nwg::CheckBox,
    plan_offline: nwg::CheckBox,
    plan_go: nwg::Button,
    plan_status: nwg::Label,
    plan_result: nwg::Label,
    plan_route: nwg::TextBox,
    plan_map: nwg::ImageFrame,
    plan_timer: nwg::AnimationTimer,

    charts_header: nwg::Label,
    chart_icao_label: nwg::Label,
    chart_icao: nwg::TextInput,
    chart_find: nwg::Button,
    chart_draw: nwg::Button,
    chart_tablets: nwg::Button,
    charts: nwg::ListView,

    options_header: nwg::Label,
    opt_start: nwg::CheckBox,
    opt_login: nwg::CheckBox,
    opt_sim: nwg::CheckBox,
    opt_xplane: nwg::CheckBox,
    opt_redirect: nwg::CheckBox,
    opt_cache: nwg::CheckBox,
    folder: nwg::TextInput,
    folder_change: nwg::Button,
    limit_label: nwg::Label,
    limit: nwg::TextInput,
    limit_unit: nwg::Label,

    activity_header: nwg::Label,
    collect: nwg::Button,
    open_folder: nwg::Button,
    open_log: nwg::Button,
    log: nwg::TextBox,

    tray: nwg::TrayNotification,
    tray_menu: nwg::Menu,
    tray_open: nwg::MenuItem,
    tray_start: nwg::MenuItem,
    tray_stop: nwg::MenuItem,
    tray_sep: nwg::MenuSeparator,
    tray_exit: nwg::MenuItem,

    timer: nwg::AnimationTimer,
    notice: nwg::Notice,
    folder_dialog: nwg::FileDialog,
}

pub struct App {
    ui: Ui,
    state: RefCell<State>,
    shared: Arc<Shared>,
    status_colour: Rc<Cell<[u8; 3]>>,
    log_lines: RefCell<VecDeque<String>>,
    handlers: RefCell<Vec<nwg::EventHandler>>,
    raw_handlers: RefCell<Vec<nwg::RawEventHandler>>,
    /// Set while the options are being filled in from settings, so doing that does not
    /// run the handlers that react to the user changing them.
    loading: Cell<bool>,
}

fn checked(b: bool) -> nwg::CheckBoxState {
    if b {
        nwg::CheckBoxState::Checked
    } else {
        nwg::CheckBoxState::Unchecked
    }
}

fn is_checked(c: &nwg::CheckBox) -> bool {
    c.check_state() == nwg::CheckBoxState::Checked
}

impl App {
    fn build(shared: Arc<Shared>, instance: Instance, show: bool) -> Result<Rc<App>, nwg::NwgError> {
        let settings = Settings::load().unwrap_or_else(|| {
            let s = Settings::default();
            let _ = s.save();
            s
        });
        let mut ui = Ui::default();

        let res = nwg::EmbedResource::load(None)?;
        ui.icon = res.icon(1, None).unwrap_or_default();
        let small = unsafe { winapi::um::winuser::GetSystemMetrics(winapi::um::winuser::SM_CXSMICON) } as u32;
        ui.small_icon = res.icon(1, Some((small, small))).unwrap_or_default();

        nwg::Font::builder().family("Segoe UI Semibold").size(32).build(&mut ui.font_title)?;
        nwg::Font::builder().family("Segoe UI Semibold").size(24).build(&mut ui.font_status)?;
        nwg::Font::builder().family("Segoe UI Semibold").size(18).build(&mut ui.font_header)?;
        nwg::Font::builder().family("Segoe UI").size(15).build(&mut ui.font_small)?;

        nwg::Window::builder()
            .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::MINIMIZE_BOX)
            .size((640, 822))
            .center(true)
            .title("AMDB Bridge")
            .icon(Some(&ui.icon))
            .build(&mut ui.window)?;
        let w = &ui.window;

        // Header
        nwg::Label::builder().parent(w).text("AMDB Bridge").font(Some(&ui.font_title)).position((20, 10)).size((420, 36)).build(&mut ui.title)?;
        nwg::Label::builder()
            .parent(w)
            .text(&format!("Version {VERSION}"))
            .font(Some(&ui.font_small))
            .h_align(nwg::HTextAlign::Right)
            .position((440, 22))
            .size((180, 20))
            .build(&mut ui.version)?;
        nwg::Label::builder().parent(w).text("Airport moving maps for your simulator, built from free open data.").position((20, 46)).size((600, 20)).build(&mut ui.subtitle)?;

        // Server
        nwg::Label::builder().parent(w).text("●  Stopped").font(Some(&ui.font_status)).position((20, 80)).size((440, 30)).build(&mut ui.status)?;
        nwg::Button::builder().parent(w).text("Start").font(Some(&ui.font_header)).position((480, 76)).size((140, 38)).build(&mut ui.start)?;
        nwg::Label::builder().parent(w).text("").position((20, 116)).size((600, 20)).build(&mut ui.detail)?;

        // Simulators and aircraft
        nwg::Label::builder().parent(w).text("Simulators and aircraft").font(Some(&ui.font_header)).position((20, 148)).size((600, 22)).build(&mut ui.sims_header)?;
        nwg::ListView::builder()
            .parent(w)
            .list_style(nwg::ListViewStyle::Detailed)
            .flags(nwg::ListViewFlags::VISIBLE | nwg::ListViewFlags::SINGLE_SELECTION | nwg::ListViewFlags::TAB_STOP)
            .ex_flags(nwg::ListViewExFlags::FULL_ROW_SELECT)
            .position((20, 172))
            .size((600, 112))
            .build(&mut ui.sims)?;
        // Widths in logical pixels; the library scales them for the display.
        for (i, (name, width)) in [("Where", 112), ("What", 204), ("Status", 258)].into_iter().enumerate() {
            ui.sims.insert_column(nwg::InsertListViewColumn { index: Some(i as i32), fmt: None, width: Some(width), text: Some(name.to_string()) });
        }
        ui.sims.set_headers_enabled(true);
        nwg::Button::builder().parent(w).text("Install or update A220 map").position((20, 292)).size((220, 30)).build(&mut ui.install)?;
        nwg::Button::builder().parent(w).text("Remove A220 map").position((248, 292)).size((160, 30)).build(&mut ui.remove)?;
        nwg::Button::builder().parent(w).text("Refresh").position((520, 292)).size((100, 30)).build(&mut ui.refresh)?;
        nwg::Button::builder().parent(w).text("Flight planning").position((416, 292)).size((96, 30)).build(&mut ui.planner)?;

        // ---- The planning panel ----------------------------------------------------
        // Everything here sits to the right of the window's own width, so it is simply not
        // on screen until the window is widened for it. Drawn in the window rather than in
        // a page because that is where the rest of this program is.
        const PX: i32 = PANEL_X;
        // Four columns of form on the left, the map beside it. Everything a dispatcher expects
        // is on the page; the ones this planner settles for you are shown greyed rather than
        // left out, so what the plan assumed is visible instead of only in the code.
        const C: [i32; 4] = [PANEL_X, PANEL_X + 134, PANEL_X + 268, PANEL_X + 402];
        const FW: i32 = 124;
        const MAP_X: i32 = PANEL_X + 552;
        nwg::Label::builder().parent(w).text("Flight planning").font(Some(&ui.font_header)).position((PX, 12)).size((400, 22)).build(&mut ui.plan_header)?;

        nwg::Label::builder().parent(w).text("Airline").font(Some(&ui.font_small)).position((C[0], 40)).size((FW, 16)).build(&mut ui.plan_airline_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("ZZZ")).position((C[0], 58)).size((FW, 23)).build(&mut ui.plan_airline)?;
        nwg::Label::builder().parent(w).text("Flight number").font(Some(&ui.font_small)).position((C[1], 40)).size((FW, 16)).build(&mut ui.plan_fltnum_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("0000")).position((C[1], 58)).size((FW, 23)).build(&mut ui.plan_fltnum)?;
        nwg::Label::builder().parent(w).text("ATC callsign").font(Some(&ui.font_small)).position((C[2], 40)).size((FW, 16)).build(&mut ui.plan_flight_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("AUTO")).position((C[2], 58)).size((FW, 23)).build(&mut ui.plan_flight)?;
        nwg::Label::builder().parent(w).text("Type of flight").font(Some(&ui.font_small)).position((C[3], 40)).size((FW, 16)).build(&mut ui.plan_fltrule_label)?;
        nwg::TextInput::builder().parent(w).text("Scheduled").readonly(true).position((C[3], 58)).size((FW, 23)).build(&mut ui.plan_fltrule)?;
        nwg::Label::builder().parent(w).text("Depart").font(Some(&ui.font_small)).position((C[0], 88)).size((FW, 16)).build(&mut ui.plan_from_label)?;
        nwg::TextInput::builder().parent(w).text("WSSS").position((C[0], 106)).size((FW, 23)).build(&mut ui.plan_from)?;
        nwg::Label::builder().parent(w).text("Arrive").font(Some(&ui.font_small)).position((C[1], 88)).size((FW, 16)).build(&mut ui.plan_to_label)?;
        nwg::TextInput::builder().parent(w).text("YBBN").position((C[1], 106)).size((FW, 23)).build(&mut ui.plan_to)?;
        nwg::Label::builder().parent(w).text("Alternate").font(Some(&ui.font_small)).position((C[2], 88)).size((FW, 16)).build(&mut ui.plan_altn_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("AUTO")).position((C[2], 106)).size((FW, 23)).build(&mut ui.plan_altn)?;
        nwg::Label::builder().parent(w).text("Alternates").font(Some(&ui.font_small)).position((C[3], 88)).size((FW, 16)).build(&mut ui.plan_altcount_label)?;
        nwg::TextInput::builder().parent(w).text("1").readonly(true).position((C[3], 106)).size((FW, 23)).build(&mut ui.plan_altcount)?;
        nwg::Label::builder().parent(w).text("Aircraft").font(Some(&ui.font_small)).position((C[0], 136)).size((FW, 16)).build(&mut ui.plan_type_label)?;
        nwg::TextInput::builder().parent(w).text("B77W").position((C[0], 154)).size((FW, 23)).build(&mut ui.plan_type)?;
        nwg::Label::builder().parent(w).text("Airframe").font(Some(&ui.font_small)).position((C[1], 136)).size((FW, 16)).build(&mut ui.plan_airframe_label)?;
        nwg::TextInput::builder().parent(w).text("Default").readonly(true).position((C[1], 154)).size((FW, 23)).build(&mut ui.plan_airframe)?;
        nwg::Label::builder().parent(w).text("Off blocks UTC").font(Some(&ui.font_small)).position((C[2], 136)).size((FW, 16)).build(&mut ui.plan_eobt_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("HHMM")).position((C[2], 154)).size((FW, 23)).build(&mut ui.plan_eobt)?;
        nwg::Label::builder().parent(w).text("Sched block time").font(Some(&ui.font_small)).position((C[3], 136)).size((FW, 16)).build(&mut ui.plan_blocktime_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("H:MM")).readonly(true).position((C[3], 154)).size((FW, 23)).build(&mut ui.plan_blocktime)?;
        nwg::Label::builder().parent(w).text("Climb profile").font(Some(&ui.font_small)).position((C[0], 184)).size((FW, 16)).build(&mut ui.plan_climb_label)?;
        nwg::TextInput::builder().parent(w).text("AUTO").readonly(true).position((C[0], 202)).size((FW, 23)).build(&mut ui.plan_climb)?;
        nwg::Label::builder().parent(w).text("Cruise").font(Some(&ui.font_small)).position((C[1], 184)).size((FW, 16)).build(&mut ui.plan_cruise_label)?;
        nwg::TextInput::builder().parent(w).text("CI").readonly(true).position((C[1], 202)).size((FW, 23)).build(&mut ui.plan_cruise)?;
        nwg::Label::builder().parent(w).text("Cost index").font(Some(&ui.font_small)).position((C[2], 184)).size((FW, 16)).build(&mut ui.plan_ci_label)?;
        nwg::TextInput::builder().parent(w).text("60").position((C[2], 202)).size((FW, 23)).build(&mut ui.plan_ci)?;
        nwg::Label::builder().parent(w).text("Descent profile").font(Some(&ui.font_small)).position((C[3], 184)).size((FW, 16)).build(&mut ui.plan_descent_label)?;
        nwg::TextInput::builder().parent(w).text("AUTO").readonly(true).position((C[3], 202)).size((FW, 23)).build(&mut ui.plan_descent)?;
        nwg::Label::builder().parent(w).text("OFP layout").font(Some(&ui.font_small)).position((C[0], 232)).size((FW, 16)).build(&mut ui.plan_layout_label)?;
        nwg::TextInput::builder().parent(w).text("LIDO").readonly(true).position((C[0], 250)).size((FW, 23)).build(&mut ui.plan_layout)?;
        nwg::Label::builder().parent(w).text("AIRAC cycle").font(Some(&ui.font_small)).position((C[1], 232)).size((FW, 16)).build(&mut ui.plan_airac_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("installed")).readonly(true).position((C[1], 250)).size((FW, 23)).build(&mut ui.plan_airac)?;
        nwg::Label::builder().parent(w).text("Units").font(Some(&ui.font_small)).position((C[2], 232)).size((FW, 16)).build(&mut ui.plan_units_label)?;
        nwg::TextInput::builder().parent(w).text("KG").readonly(true).position((C[2], 250)).size((FW, 23)).build(&mut ui.plan_units)?;
        nwg::Label::builder().parent(w).text("Flight maps").font(Some(&ui.font_small)).position((C[3], 232)).size((FW, 16)).build(&mut ui.plan_maps_label)?;
        nwg::TextInput::builder().parent(w).text("Detailed").readonly(true).position((C[3], 250)).size((FW, 23)).build(&mut ui.plan_maps)?;
        nwg::Label::builder().parent(w).text("Flight rules").font(Some(&ui.font_small)).position((C[0], 280)).size((FW, 16)).build(&mut ui.plan_rules_label)?;
        nwg::TextInput::builder().parent(w).text("IFR").readonly(true).position((C[0], 298)).size((FW, 23)).build(&mut ui.plan_rules)?;
        nwg::Label::builder().parent(w).text("Taxi out (min)").font(Some(&ui.font_small)).position((C[1], 280)).size((FW, 16)).build(&mut ui.plan_taxiout_label)?;
        nwg::TextInput::builder().parent(w).text("20").readonly(true).position((C[1], 298)).size((FW, 23)).build(&mut ui.plan_taxiout)?;
        nwg::Label::builder().parent(w).text("Taxi in (min)").font(Some(&ui.font_small)).position((C[2], 280)).size((FW, 16)).build(&mut ui.plan_taxiin_label)?;
        nwg::TextInput::builder().parent(w).text("8").readonly(true).position((C[2], 298)).size((FW, 23)).build(&mut ui.plan_taxiin)?;
        nwg::Label::builder().parent(w).text("Altitude").font(Some(&ui.font_small)).position((C[3], 280)).size((FW, 16)).build(&mut ui.plan_level_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("AUTO")).position((C[3], 298)).size((FW, 23)).build(&mut ui.plan_level)?;
        nwg::Label::builder().parent(w).text("Departure runway").font(Some(&ui.font_small)).position((C[0], 328)).size((FW, 16)).build(&mut ui.plan_deprwy_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("AUTO")).readonly(true).position((C[0], 346)).size((FW, 23)).build(&mut ui.plan_deprwy)?;
        nwg::Label::builder().parent(w).text("Arrival runway").font(Some(&ui.font_small)).position((C[1], 328)).size((FW, 16)).build(&mut ui.plan_arrrwy_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("AUTO")).readonly(true).position((C[1], 346)).size((FW, 23)).build(&mut ui.plan_arrrwy)?;
        nwg::Label::builder().parent(w).text("Passengers").font(Some(&ui.font_small)).position((C[2], 328)).size((FW, 16)).build(&mut ui.plan_pax_label)?;
        nwg::TextInput::builder().parent(w).text("300").position((C[2], 346)).size((FW, 23)).build(&mut ui.plan_pax)?;
        nwg::Label::builder().parent(w).text("Payload kg").font(Some(&ui.font_small)).position((C[3], 328)).size((FW, 16)).build(&mut ui.plan_payload_label)?;
        nwg::TextInput::builder().parent(w).text("40000").position((C[3], 346)).size((FW, 23)).build(&mut ui.plan_payload)?;
        nwg::Label::builder().parent(w).text("Freight kg").font(Some(&ui.font_small)).position((C[0], 376)).size((FW, 16)).build(&mut ui.plan_freight_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("NONE")).readonly(true).position((C[0], 394)).size((FW, 23)).build(&mut ui.plan_freight)?;
        nwg::Label::builder().parent(w).text("Zero fuel weight").font(Some(&ui.font_small)).position((C[1], 376)).size((FW, 16)).build(&mut ui.plan_zfw_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("AUTO")).readonly(true).position((C[1], 394)).size((FW, 23)).build(&mut ui.plan_zfw)?;
        nwg::Label::builder().parent(w).text("Registration").font(Some(&ui.font_small)).position((C[2], 376)).size((FW, 16)).build(&mut ui.plan_reg_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("AUTO")).position((C[2], 394)).size((FW, 23)).build(&mut ui.plan_reg)?;
        nwg::Label::builder().parent(w).text("Avoid FIRs").font(Some(&ui.font_small)).position((C[3], 376)).size((FW, 16)).build(&mut ui.plan_avoid_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("none")).position((C[3], 394)).size((FW, 23)).build(&mut ui.plan_avoid)?;
        nwg::Label::builder().parent(w).text("Contingency fuel").font(Some(&ui.font_small)).position((C[0], 424)).size((FW, 16)).build(&mut ui.plan_contfuel_label)?;
        nwg::TextInput::builder().parent(w).text("Auto").readonly(true).position((C[0], 442)).size((FW, 23)).build(&mut ui.plan_contfuel)?;
        nwg::Label::builder().parent(w).text("Reserve fuel").font(Some(&ui.font_small)).position((C[1], 424)).size((FW, 16)).build(&mut ui.plan_resfuel_label)?;
        nwg::TextInput::builder().parent(w).text("Auto").readonly(true).position((C[1], 442)).size((FW, 23)).build(&mut ui.plan_resfuel)?;
        nwg::Label::builder().parent(w).text("Taxi fuel").font(Some(&ui.font_small)).position((C[2], 424)).size((FW, 16)).build(&mut ui.plan_taxifuel_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("AUTO")).readonly(true).position((C[2], 442)).size((FW, 23)).build(&mut ui.plan_taxifuel)?;
        nwg::Label::builder().parent(w).text("Block fuel").font(Some(&ui.font_small)).position((C[3], 424)).size((FW, 16)).build(&mut ui.plan_blockfuel_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("AUTO")).readonly(true).position((C[3], 442)).size((FW, 23)).build(&mut ui.plan_blockfuel)?;
        nwg::Label::builder().parent(w).text("Arrival fuel").font(Some(&ui.font_small)).position((C[0], 472)).size((FW, 16)).build(&mut ui.plan_arrfuel_label)?;
        nwg::TextInput::builder().parent(w).placeholder_text(Some("AUTO")).readonly(true).position((C[0], 490)).size((FW, 23)).build(&mut ui.plan_arrfuel)?;
        nwg::Label::builder().parent(w).text("MEL fuel").font(Some(&ui.font_small)).position((C[1], 472)).size((FW, 16)).build(&mut ui.plan_melfuel_label)?;
        nwg::TextInput::builder().parent(w).text("0").readonly(true).position((C[1], 490)).size((FW, 23)).build(&mut ui.plan_melfuel)?;
        nwg::Label::builder().parent(w).text("ATC fuel").font(Some(&ui.font_small)).position((C[2], 472)).size((FW, 16)).build(&mut ui.plan_atcfuel_label)?;
        nwg::TextInput::builder().parent(w).text("0").readonly(true).position((C[2], 490)).size((FW, 23)).build(&mut ui.plan_atcfuel)?;
        nwg::Label::builder().parent(w).text("Extra / tankering").font(Some(&ui.font_small)).position((C[3], 472)).size((FW, 16)).build(&mut ui.plan_extrafuel_label)?;
        nwg::TextInput::builder().parent(w).text("0").readonly(true).position((C[3], 490)).size((FW, 23)).build(&mut ui.plan_extrafuel)?;

        nwg::CheckBox::builder().parent(w).text("Detailed navlog").check_state(nwg::CheckBoxState::Checked).position((C[0], 522)).size((FW + 8, 21)).build(&mut ui.plan_navlog)?;
        nwg::CheckBox::builder().parent(w).text("ETOPS planning").check_state(nwg::CheckBoxState::Checked).position((C[1], 522)).size((FW + 8, 21)).build(&mut ui.plan_etops)?;
        nwg::CheckBox::builder().parent(w).text("Plan stepclimbs").check_state(nwg::CheckBoxState::Checked).position((C[2], 522)).size((FW + 8, 21)).build(&mut ui.plan_steps)?;
        nwg::CheckBox::builder().parent(w).text("Runway analysis").position((C[3], 522)).size((FW + 8, 21)).build(&mut ui.plan_rwyanalysis)?;
        nwg::CheckBox::builder().parent(w).text("Include NOTAMs").check_state(nwg::CheckBoxState::Checked).position((C[0], 546)).size((FW + 8, 21)).build(&mut ui.plan_notams)?;
        nwg::CheckBox::builder().parent(w).text("FIR NOTAMs").check_state(nwg::CheckBoxState::Checked).position((C[1], 546)).size((FW + 8, 21)).build(&mut ui.plan_firnotams)?;
        nwg::CheckBox::builder().parent(w).text("Hazards").check_state(nwg::CheckBoxState::Checked).position((C[2], 546)).size((FW + 8, 21)).build(&mut ui.plan_hazards)?;
        nwg::CheckBox::builder().parent(w).text("RVSM").check_state(nwg::CheckBoxState::Checked).position((C[3], 546)).size((FW + 8, 21)).build(&mut ui.plan_rvsm)?;
        nwg::CheckBox::builder().parent(w).text("Offline (still air)").check_state(nwg::CheckBoxState::Checked).position((C[0], 570)).size((FW + 8, 21)).build(&mut ui.plan_offline)?;
        for c in [&ui.plan_navlog, &ui.plan_etops, &ui.plan_steps, &ui.plan_rwyanalysis, &ui.plan_notams, &ui.plan_firnotams, &ui.plan_hazards] {
            c.set_enabled(false);
        }

        let by = 600;
        nwg::Button::builder().parent(w).text("Generate").position((C[0], by)).size((FW, 30)).build(&mut ui.plan_go)?;
        nwg::Label::builder().parent(w).text("idle").font(Some(&ui.font_small)).position((C[1], by + 8)).size((390, 18)).build(&mut ui.plan_status)?;

        // The map, beside the form, with the route it found under it.
        nwg::ImageFrame::builder().parent(w).position((MAP_X, 40)).size((MAP_W, MAP_H)).build(&mut ui.plan_map)?;
        nwg::Label::builder().parent(w).text("").font(Some(&ui.font_small)).position((MAP_X, 40 + MAP_H + 8)).size((MAP_W, 18)).build(&mut ui.plan_result)?;
        nwg::TextBox::builder()
            .parent(w)
            .readonly(true)
            .flags(nwg::TextBoxFlags::VISIBLE | nwg::TextBoxFlags::VSCROLL)
            .font(Some(&ui.font_small))
            .position((MAP_X, 40 + MAP_H + 30))
            .size((MAP_W, 160))
            .build(&mut ui.plan_route)?;

        nwg::AnimationTimer::builder().parent(w).interval(Duration::from_millis(400)).active(false).build(&mut ui.plan_timer)?;

        // Options
        // ---- Approach charts -------------------------------------------------------
        nwg::Label::builder().parent(w).text("Approach charts").font(Some(&ui.font_header)).position((20, 336)).size((600, 22)).build(&mut ui.charts_header)?;
        nwg::Label::builder().parent(w).text("Airport").position((20, 365)).size((52, 22)).build(&mut ui.chart_icao_label)?;
        nwg::TextInput::builder().parent(w).limit(4).placeholder_text(Some("ICAO")).position((74, 362)).size((70, 25)).build(&mut ui.chart_icao)?;
        nwg::Button::builder().parent(w).text("Find procedures").position((156, 361)).size((132, 27)).build(&mut ui.chart_find)?;
        nwg::Button::builder().parent(w).text("Draw chart").position((296, 361)).size((132, 27)).build(&mut ui.chart_draw)?;
        // The A350 and PMDG tablets, which can take their charts from here. Labelled once
        // the aircraft have been looked through.
        nwg::Button::builder().parent(w).text("Tablet charts…").position((436, 361)).size((184, 27)).build(&mut ui.chart_tablets)?;
        ui.chart_tablets.set_enabled(false);
        nwg::ListView::builder()
            .parent(w)
            .list_style(nwg::ListViewStyle::Detailed)
            .flags(nwg::ListViewFlags::VISIBLE | nwg::ListViewFlags::SINGLE_SELECTION | nwg::ListViewFlags::TAB_STOP)
            .ex_flags(nwg::ListViewExFlags::FULL_ROW_SELECT)
            .position((20, 394))
            .size((600, 104))
            .build(&mut ui.charts)?;
        for (i, (name, width)) in [("Approach", 206), ("Runway", 90), ("Arrivals that feed it", 278)].into_iter().enumerate() {
            ui.charts.insert_column(nwg::InsertListViewColumn { index: Some(i as i32), fmt: None, width: Some(width), text: Some(name.to_string()) });
        }
        ui.charts.set_headers_enabled(true);
        ui.chart_draw.set_enabled(false);

        nwg::Label::builder().parent(w).text("Options").font(Some(&ui.font_header)).position((20, 512)).size((600, 22)).build(&mut ui.options_header)?;
        let opts: [(&mut nwg::CheckBox, &str); 6] = [
            (&mut ui.opt_start, "Start serving as soon as AMDB Bridge opens"),
            (&mut ui.opt_login, "Open AMDB Bridge in the notification area when Windows starts"),
            (&mut ui.opt_sim, "Open AMDB Bridge when Microsoft Flight Simulator starts"),
            (&mut ui.opt_xplane, "Install the X-Plane 12 moving map when serving starts"),
            (&mut ui.opt_redirect, "Also serve the iniBuilds A350 and FlyByWire A380X (asks for administrator permission once)"),
            (&mut ui.opt_cache, "Keep built airports on disk, so they load instantly next time"),
        ];
        for (i, (cb, text)) in opts.into_iter().enumerate() {
            nwg::CheckBox::builder().parent(w).text(text).position((20, 536 + i as i32 * 23)).size((600, 22)).build(cb)?;
        }
        nwg::TextInput::builder().parent(w).readonly(true).position((40, 676)).size((340, 25)).build(&mut ui.folder)?;
        nwg::Button::builder().parent(w).text("Change…").position((386, 674)).size((90, 29)).build(&mut ui.folder_change)?;
        nwg::Label::builder().parent(w).text("Limit").h_align(nwg::HTextAlign::Right).position((484, 679)).size((40, 22)).build(&mut ui.limit_label)?;
        nwg::TextInput::builder().parent(w).align(nwg::HTextAlign::Right).limit(7).placeholder_text(Some("none")).position((530, 676)).size((58, 25)).build(&mut ui.limit)?;
        nwg::Label::builder().parent(w).text("MB").position((594, 679)).size((26, 22)).build(&mut ui.limit_unit)?;

        // Activity
        nwg::Label::builder().parent(w).text("Activity").font(Some(&ui.font_header)).position((20, 716)).size((200, 22)).build(&mut ui.activity_header)?;
        nwg::Button::builder().parent(w).text("Aircraft report").font(Some(&ui.font_small)).position((258, 713)).size((124, 26)).build(&mut ui.collect)?;
        nwg::Button::builder().parent(w).text("Airports folder").font(Some(&ui.font_small)).position((388, 713)).size((124, 26)).build(&mut ui.open_folder)?;
        nwg::Button::builder().parent(w).text("Save log").font(Some(&ui.font_small)).position((518, 713)).size((102, 26)).build(&mut ui.open_log)?;
        nwg::TextBox::builder()
            .parent(w)
            .readonly(true)
            .flags(nwg::TextBoxFlags::VISIBLE | nwg::TextBoxFlags::VSCROLL | nwg::TextBoxFlags::AUTOVSCROLL | nwg::TextBoxFlags::TAB_STOP)
            .font(Some(&ui.font_small))
            .position((20, 744))
            .size((600, 66))
            .build(&mut ui.log)?;

        // Notification area
        nwg::TrayNotification::builder().parent(w).icon(Some(&ui.small_icon)).tip(Some("AMDB Bridge")).build(&mut ui.tray)?;
        nwg::Menu::builder().popup(true).parent(w).build(&mut ui.tray_menu)?;
        nwg::MenuItem::builder().text("Open AMDB Bridge").parent(&ui.tray_menu).build(&mut ui.tray_open)?;
        nwg::MenuItem::builder().text("Start serving").parent(&ui.tray_menu).build(&mut ui.tray_start)?;
        nwg::MenuItem::builder().text("Stop serving").parent(&ui.tray_menu).build(&mut ui.tray_stop)?;
        nwg::MenuSeparator::builder().parent(&ui.tray_menu).build(&mut ui.tray_sep)?;
        nwg::MenuItem::builder().text("Exit").parent(&ui.tray_menu).build(&mut ui.tray_exit)?;

        nwg::AnimationTimer::builder().parent(w).interval(Duration::from_millis(400)).active(true).build(&mut ui.timer)?;
        nwg::Notice::builder().parent(w).build(&mut ui.notice)?;
        nwg::FileDialog::builder().title("Where should built airports be kept?").action(nwg::FileDialogAction::OpenDirectory).build(&mut ui.folder_dialog)?;

        *shared.notice.lock().unwrap() = Some(ui.notice.sender());

        let app = Rc::new(App {
            ui,
            state: RefCell::new(State {
                settings,
                running: None,
                planner_open: false,
                planning: None,
                plan_bitmap: None,
                phase: Phase::Stopped,
                error: String::new(),
                instance,
                told_about_tray: false,
                next_detail: Instant::now(),
                next_cache_scan: Instant::now(),
                exiting: false,
            }),
            shared,
            status_colour: Rc::new(Cell::new(GREY)),
            log_lines: RefCell::new(VecDeque::new()),
            handlers: RefCell::new(Vec::new()),
            raw_handlers: RefCell::new(Vec::new()),
            loading: Cell::new(false),
        });
        app.bind();
        app.load_options();
        app.refresh_inventory();
        app.show_phase();
        if show {
            app.ui.window.set_visible(true);
        }
        Ok(app)
    }

    fn bind(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let handler = nwg::full_bind_event_handler(&self.ui.window.handle, move |evt, data, handle| {
            let Some(app) = weak.upgrade() else { return };
            app.on_event(evt, &data, handle);
        });
        self.handlers.borrow_mut().push(handler);

        // Colour the status line. Static controls ask their parent for colours, and the
        // library offers no text colour for labels, so answer that question directly.
        let status = self.ui.status.handle.hwnd().unwrap() as usize;
        let colour = self.status_colour.clone();
        let raw = nwg::bind_raw_event_handler(&self.ui.window.handle, 0x1_0001, move |_hwnd, msg, wparam, lparam| {
            use winapi::um::wingdi::{SetBkMode, SetTextColor, RGB, TRANSPARENT};
            use winapi::um::winuser::{GetSysColorBrush, COLOR_3DFACE, WM_CTLCOLORSTATIC};
            if msg == WM_CTLCOLORSTATIC && lparam as usize == status {
                let [r, g, b] = colour.get();
                let hdc = wparam as winapi::shared::windef::HDC;
                unsafe {
                    SetTextColor(hdc, RGB(r, g, b));
                    SetBkMode(hdc, TRANSPARENT as i32);
                    return Some(GetSysColorBrush(COLOR_3DFACE) as isize);
                }
            }
            None
        });
        if let Ok(raw) = raw {
            self.raw_handlers.borrow_mut().push(raw);
        }
    }

    fn on_event(&self, evt: nwg::Event, data: &nwg::EventData, handle: nwg::ControlHandle) {
        use nwg::Event as E;
        let ui = &self.ui;
        match evt {
            E::OnWindowClose if handle == ui.window.handle => {
                if !self.state.borrow().exiting {
                    if let nwg::EventData::OnWindowClose(close) = data {
                        close.close(false);
                    }
                    self.hide_to_tray();
                }
            }
            E::OnButtonClick => {
                if handle == ui.start.handle {
                    self.toggle_serving();
                } else if handle == ui.install.handle {
                    self.install_map();
                } else if handle == ui.remove.handle {
                    self.remove_map();
                } else if handle == ui.planner.handle {
                    self.toggle_planner();
                } else if handle == ui.plan_go.handle {
                    self.start_planning();
                } else if handle == ui.refresh.handle {
                    self.refresh_inventory();
                } else if handle == ui.chart_find.handle {
                    self.find_procedures();
                } else if handle == ui.chart_draw.handle {
                    self.draw_chart();
                } else if handle == ui.chart_tablets.handle {
                    self.toggle_tablet_charts();
                } else if handle == ui.folder_change.handle {
                    self.change_folder();
                } else if handle == ui.open_folder.handle {
                    let dir = self.state.borrow().settings.airports_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    desktop::reveal(&dir);
                } else if handle == ui.collect.handle {
                    self.collect_report();
                } else if handle == ui.open_log.handle {
                    self.save_log();
                } else if handle == ui.opt_start.handle {
                    self.option_start();
                } else if handle == ui.opt_login.handle {
                    self.option_login();
                } else if handle == ui.opt_sim.handle {
                    self.option_sim();
                } else if handle == ui.opt_xplane.handle {
                    self.option_xplane();
                } else if handle == ui.opt_redirect.handle {
                    self.option_redirect();
                } else if handle == ui.opt_cache.handle {
                    self.option_cache();
                }
            }
            E::OnTextInput if handle == ui.limit.handle => self.option_limit(),
            E::OnMousePress(nwg::MousePressEvent::MousePressLeftUp) if handle == ui.tray.handle => self.show_window(),
            E::OnContextMenu if handle == ui.tray.handle => {
                let running = self.state.borrow().phase == Phase::Serving;
                ui.tray_start.set_enabled(!running && self.state.borrow().phase != Phase::Starting);
                ui.tray_stop.set_enabled(running);
                let mut p = winapi::shared::windef::POINT { x: 0, y: 0 };
                unsafe { winapi::um::winuser::GetCursorPos(&mut p) };
                ui.tray_menu.popup(p.x, p.y);
            }
            E::OnMenuItemSelected => {
                if handle == ui.tray_open.handle {
                    self.show_window();
                } else if handle == ui.tray_start.handle || handle == ui.tray_stop.handle {
                    self.toggle_serving();
                } else if handle == ui.tray_exit.handle {
                    self.exit();
                }
            }
            E::OnTimerTick if handle == ui.timer.handle => self.tick(),
            E::OnTimerTick if handle == ui.plan_timer.handle => self.tick_planning(),
            E::OnNotice if handle == ui.notice.handle => {
                self.drain();
                // A plan finishing wakes the window through the same notice, so the panel is
                // brought up to date whenever anything else is.
                if self.state.borrow().planning.is_some() {
                    self.tick_planning();
                }
            }
            _ => {}
        }
    }

    // ----------------------------------------------------------------------------------
    // Lifecycle

    fn after_open(self: &Rc<Self>) {
        self.log_line(&format!("AMDB Bridge {VERSION}"));
        if let Some(msg) = service::clear_stale_redirect(&self.state.borrow().settings) {
            amdbgen::term::warn(&msg);
        }
        let start = self.state.borrow().settings.start_on_open;
        if start {
            self.start_serving(false);
        } else {
            self.drain();
        }
        self.scan_cache();
    }

    fn shutdown(&self) {
        if let Some(r) = self.state.borrow_mut().running.take() {
            r.stop();
        }
        self.ui.tray.set_visibility(false);
        for h in self.handlers.borrow_mut().drain(..) {
            nwg::unbind_event_handler(&h);
        }
        for h in self.raw_handlers.borrow_mut().drain(..) {
            let _ = nwg::unbind_raw_event_handler(&h);
        }
    }

    fn exit(&self) {
        self.state.borrow_mut().exiting = true;
        self.ui.window.set_visible(false);
        nwg::stop_thread_dispatch();
    }

    fn show_window(&self) {
        let w = &self.ui.window;
        w.set_visible(true);
        w.restore();
        w.set_focus();
        unsafe { winapi::um::winuser::SetForegroundWindow(w.handle.hwnd().unwrap()) };
    }

    fn hide_to_tray(&self) {
        self.ui.window.set_visible(false);
        let mut st = self.state.borrow_mut();
        if !st.told_about_tray {
            st.told_about_tray = true;
            let text = if st.phase == Phase::Serving {
                "Still serving maps. Open AMDB Bridge or exit it from this icon."
            } else {
                "AMDB Bridge is in the notification area. Right-click the icon to exit."
            };
            self.ui.tray.show(text, Some("AMDB Bridge"), Some(nwg::TrayNotificationFlags::USER_ICON), Some(&self.ui.icon));
        }
    }

    fn tick(&self) {
        let (show, quit) = {
            let st = self.state.borrow();
            (st.instance.show_requested(), st.instance.quit_requested())
        };
        if quit {
            self.exit();
            return;
        }
        if show {
            self.show_window();
        }
        let now = Instant::now();
        let (detail_due, scan_due) = {
            let st = self.state.borrow();
            (now >= st.next_detail, now >= st.next_cache_scan)
        };
        if detail_due {
            self.state.borrow_mut().next_detail = now + Duration::from_secs(2);
            self.show_phase();
        }
        if scan_due {
            self.scan_cache();
        }
    }

    /// Work that other threads finished: log lines, a start result, a cache size.
    fn drain(&self) {
        let lines: Vec<String> = std::mem::take(&mut *self.shared.lines.lock().unwrap());
        for l in lines {
            self.log_line(&l);
        }
        let started = self.shared.started.lock().unwrap().take();
        if let Some(result) = started {
            let failure = {
                let mut st = self.state.borrow_mut();
                match result {
                    Ok(running) => {
                        st.running = Some(running);
                        st.phase = Phase::Serving;
                        None
                    }
                    Err(e) => {
                        st.phase = Phase::Failed;
                        st.error = format!("{e:#}");
                        Some(st.error.clone())
                    }
                }
            };
            if let Some(e) = failure {
                amdbgen::term::error(&format!("Could not start: {e}"));
                self.drain_lines_only();
            }
            self.show_phase();
            self.refresh_inventory();
        }
        let report = self.shared.report.lock().unwrap().take();
        if let Some(result) = report {
            self.ui.collect.set_enabled(true);
            match result {
                Ok((report, log)) => {
                    amdbgen::term::success(&format!("Saved to Downloads: {}", report.file_name().unwrap_or_default().to_string_lossy()));
                    if let Some(log) = &log {
                        amdbgen::term::success(&format!("Saved to Downloads: {}", log.file_name().unwrap_or_default().to_string_lossy()));
                    }
                    amdbgen::term::info("Send both files with your report");
                    desktop::reveal_file(&report);
                }
                Err(e) => amdbgen::term::error(&format!("Could not write the aircraft report: {e:#}")),
            }
            self.drain_lines_only();
        }
        let inventory = self.shared.inventory.lock().unwrap().take();
        if let Some(inv) = inventory {
            self.show_inventory(inv);
        }
        if self.shared.cache_bytes.lock().unwrap().is_some() {
            self.show_phase();
        }
    }

    fn drain_lines_only(&self) {
        let lines: Vec<String> = std::mem::take(&mut *self.shared.lines.lock().unwrap());
        for l in lines {
            self.log_line(&l);
        }
    }

    fn log_line(&self, line: &str) {
        use winapi::um::winuser::{SendMessageW, EM_REPLACESEL, EM_SETSEL};
        let mut lines = self.log_lines.borrow_mut();
        lines.push_back(line.to_string());
        let hwnd = self.ui.log.handle.hwnd().unwrap();
        if lines.len() > LOG_LINES {
            while lines.len() > LOG_LINES / 2 {
                lines.pop_front();
            }
            let text: Vec<&str> = lines.iter().map(String::as_str).collect();
            self.ui.log.set_text(&text.join("\r\n"));
        } else {
            let prefix = if lines.len() > 1 { "\r\n" } else { "" };
            let wide: Vec<u16> = format!("{prefix}{line}").encode_utf16().chain(Some(0)).collect();
            let end = unsafe { winapi::um::winuser::GetWindowTextLengthW(hwnd) }.max(0) as usize;
            unsafe {
                SendMessageW(hwnd, EM_SETSEL as u32, end, end as isize);
                SendMessageW(hwnd, EM_REPLACESEL as u32, 0, wide.as_ptr() as isize);
            }
        }
        self.ui.log.scroll_lastline();
    }

    // ----------------------------------------------------------------------------------
    // Serving

    fn toggle_serving(&self) {
        let phase = self.state.borrow().phase;
        match phase {
            Phase::Starting => {}
            Phase::Serving => {
                if let Some(r) = self.state.borrow_mut().running.take() {
                    r.stop();
                }
                self.state.borrow_mut().phase = Phase::Stopped;
                self.drain_lines_only();
                self.show_phase();
                self.refresh_inventory();
            }
            Phase::Stopped | Phase::Failed => self.start_serving(true),
        }
    }

    /// Start in the background.
    fn start_serving(&self, _asked: bool) {
        let settings = self.state.borrow().settings.clone();
        {
            let mut st = self.state.borrow_mut();
            st.phase = Phase::Starting;
            st.error.clear();
        }
        self.show_phase();
        let shared = self.shared.clone();
        let opts = service::Options::from_settings(&settings);
        std::thread::spawn(move || {
            let result = service::start(&settings, &opts);
            *shared.started.lock().unwrap() = Some(result);
            shared.wake();
        });
    }

    fn show_phase(&self) {
        let st = self.state.borrow();
        let cache = *self.shared.cache_bytes.lock().unwrap();
        let storage = if !st.settings.cache {
            "airports are not kept on disk".to_string()
        } else {
            let used = cache.map(amdbgen::term::human_bytes).unwrap_or_else(|| "…".into());
            if st.settings.limit_mb > 0 {
                format!("{used} of {} MB used on disk", st.settings.limit_mb)
            } else {
                format!("{used} used on disk")
            }
        };
        let (text, colour, detail, button) = match st.phase {
            Phase::Stopped => ("●  Not serving", GREY, format!("Press Start before loading your aircraft.  ·  {storage}"), "Start"),
            Phase::Starting => ("●  Starting…", AMBER, "Getting ready. This takes a moment the first time.".to_string(), "Starting…"),
            Phase::Serving => {
                let r = st.running.as_ref().unwrap();
                let n = r.airports_loaded();
                let airports = match n {
                    0 => "no airports loaded yet".to_string(),
                    1 => "1 airport loaded".to_string(),
                    n => format!("{n} airports loaded"),
                };
                let extra = if r.redirected { "  ·  A350/A380X on" } else { "" };
                ("●  Serving maps", GREEN, format!("{airports}  ·  {storage}{extra}"), "Stop")
            }
            Phase::Failed => ("●  Could not start", RED, st.error.lines().next().unwrap_or_default().to_string(), "Try again"),
        };
        if self.ui.status.text() != text {
            self.status_colour.set(colour);
            self.ui.status.set_text(text);
        }
        if self.ui.detail.text() != detail {
            self.ui.detail.set_text(&detail);
        }
        if self.ui.start.text() != button {
            self.ui.start.set_text(button);
        }
        self.ui.start.set_enabled(st.phase != Phase::Starting);
        let tip = format!("AMDB Bridge - {}", text.trim_start_matches(['●', ' ']));
        self.ui.tray.set_tip(&tip);
    }

    fn scan_cache(&self) {
        let settings = {
            let mut st = self.state.borrow_mut();
            st.next_cache_scan = Instant::now() + Duration::from_secs(60);
            st.settings.clone()
        };
        if !settings.cache {
            return;
        }
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let bytes = dir_size(&settings.cache_dir);
            *shared.cache_bytes.lock().unwrap() = Some(bytes);
            shared.wake();
        });
    }

    // ----------------------------------------------------------------------------------
    // Simulators and the A220 map

    /// What this airport has, read from the navigation data the simulator already has.
    ///
    /// The approaches are what the list shows, because an approach is what a chart is
    /// drawn of. The arrivals that feed each one are named beside it, so a reader handed
    /// a particular arrival can see which approach it leads to and ask for that chart.
    fn find_procedures(&self) {
        use amdbgen::sources::msfs::procedures::Kind;
        let icao = self.ui.chart_icao.text().trim().to_uppercase();
        if icao.len() < 3 {
            amdbgen::term::warn("Type an airport's ICAO code first, like LPMA or KJFK");
            self.drain_lines_only();
            return;
        }
        let found = match amdbgen::sources::msfs::procedures::find(&icao) {
            Ok(Some(a)) => a,
            Ok(None) => {
                amdbgen::term::warn(&format!("{icao} is not in the navigation data on this computer"));
                self.ui.charts.clear();
                self.ui.chart_draw.set_enabled(false);
                self.drain_lines_only();
                return;
            }
            Err(e) => {
                amdbgen::term::error(&format!("Could not read the navigation data: {e:#}"));
                self.drain_lines_only();
                return;
            }
        };
        let count = |k: Kind| found.procedures.iter().filter(|p| p.kind == k).count();
        amdbgen::term::info(&format!(
            "{icao}: {} departures, {} arrivals, {} approaches",
            count(Kind::Sid),
            count(Kind::Star),
            count(Kind::Approach)
        ));
        let lv = &self.ui.charts;
        lv.set_redraw(false);
        lv.clear();
        let mut rows = 0;
        for p in found.procedures.iter().filter(|p| p.kind == Kind::Approach) {
            // The ways in to this approach, which is how an arrival hands over to it.
            let mut feeds: Vec<&str> = p
                .transitions
                .iter()
                .filter(|tr| tr.part.is_empty() && !tr.name.is_empty())
                .map(|tr| tr.name.as_str())
                .collect();
            feeds.dedup();
            let joined = feeds.join(", ");
            for (col, text) in [p.name.as_str(), p.runway.as_str(), joined.as_str()].into_iter().enumerate() {
                lv.insert_item(nwg::InsertListViewItem {
                    index: Some(rows),
                    column_index: col as i32,
                    text: Some(text.to_string()),
                    image: None,
                });
            }
            rows += 1;
        }
        lv.set_redraw(true);
        self.ui.chart_draw.set_enabled(rows > 0);
        if rows == 0 {
            amdbgen::term::warn(&format!("{icao} has no instrument approaches in the navigation data"));
        }
        self.drain_lines_only();
    }

    /// Point the A350 and PMDG tablets' charts at the bridge, or give them back their own.
    /// On when any of them is on, so a click from a mixed state turns them all off.
    fn toggle_tablet_charts(&self) {
        let sims = desktop::detect_sims();
        let on = sims.iter().any(|s| amdbgen::bridge::patcher::scan_charts(&s.community).iter().any(|(_, _, on)| *on));
        let port = amdbgen::bridge::DEFAULT_PORT;
        let mut changed = 0;
        for sim in &sims {
            let done = if on { amdbgen::bridge::patcher::unpatch_charts(&sim.community) } else { amdbgen::bridge::patcher::patch_charts(&sim.community, port) };
            match done {
                Ok(files) => changed += files.len(),
                Err(e) => amdbgen::term::error(&format!("Could not change the tablets in {}: {e:#}", sim.community.display())),
            }
        }
        if on {
            amdbgen::term::success(&format!("Tablet charts off: {changed} tablet(s) back on their own Navigraph charts"));
        } else {
            amdbgen::term::success(&format!("Tablet charts on: {changed} tablet(s) now take approach charts from the bridge. Keep it serving while you fly; restart the flight if the aircraft is already loaded."));
        }
        self.drain_lines_only();
        self.refresh_inventory();
    }

    /// Draw the approach picked in the list, by the command-line tool beside this
    /// program. It goes to the network for terrain and obstacles, so it runs on its own
    /// and the window stays answerable while it does.
    fn draw_chart(&self) {
        let icao = self.ui.chart_icao.text().trim().to_uppercase();
        let Some(row) = self.ui.charts.selected_item() else {
            amdbgen::term::warn("Pick an approach from the list first");
            self.drain_lines_only();
            return;
        };
        let Some(item) = self.ui.charts.item(row, 0, 260) else { return };
        let approach = item.text;
        let Some(exe) = amdbgen_exe() else {
            amdbgen::term::error("amdbgen.exe is missing from this installation - reinstall AMDB Bridge");
            self.drain_lines_only();
            return;
        };
        // A chart's name goes in a filename, and an approach is called things like
        // "RNAV (GPS) Z RWY 05" or "ILS 27L/R".
        let tidy: String = approach.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let name = format!("{icao}-{}.pdf", tidy.trim_matches('-'));
        let out = desktop::downloads_dir().join(name);
        amdbgen::term::info(&format!("Drawing {icao} {approach}, which takes a moment the first time..."));
        self.drain_lines_only();
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let result = std::process::Command::new(exe)
                .args(["approach-chart", &icao, "--approach", &approach, "--out"])
                .arg(&out)
                .arg("--open")
                .output();
            match result {
                Ok(o) if o.status.success() => amdbgen::term::success(&format!("Chart written to {}", out.display())),
                Ok(o) => {
                    let why = String::from_utf8_lossy(&o.stderr);
                    let last = why.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no reason given");
                    amdbgen::term::error(&format!("Could not draw {icao} {approach}: {}", last.trim()));
                }
                Err(e) => amdbgen::term::error(&format!("Could not run amdbgen: {e}")),
            }
            shared.wake();
        });
    }

    /// Show or hide the planning panel, by widening the window for it.
    ///
    /// Every control of the panel is built beyond the narrow window's own width, so it is
    /// already there and simply not on screen; showing it is a resize.
    fn toggle_planner(&self) {
        let open = !self.state.borrow().planner_open;
        self.state.borrow_mut().planner_open = open;
        let (w, h) = self.ui.window.size();
        let _ = w;
        self.ui.window.set_size(if open { WIDE } else { NARROW }, h);
        self.ui.planner.set_text(if open { "Hide planning" } else { "Flight planning" });
        if open {
            self.redraw_plan();
        }
    }

    /// Work out a plan, in a thread of its own, watching the search as it runs.
    fn start_planning(&self) {
        if self.state.borrow().planning.as_ref().is_some_and(|p| !p.done.load(Ordering::Relaxed)) {
            return;
        }
        let text = |c: &nwg::TextInput| c.text().trim().to_uppercase();
        let number = |c: &nwg::TextInput, fallback: f64| c.text().trim().parse::<f64>().unwrap_or(fallback);

        let mut opts = amdbgen::ofp::DispatchOptions::new(text(&self.ui.plan_from), text(&self.ui.plan_to), text(&self.ui.plan_type));
        opts.payload_kg = number(&self.ui.plan_payload, 0.0);
        opts.passengers = number(&self.ui.plan_pax, 0.0) as u32;
        opts.cost_index = number(&self.ui.plan_ci, 30.0);
        opts.offline = self.ui.plan_offline.check_state() == nwg::CheckBoxState::Checked;
        opts.rvsm = self.ui.plan_rvsm.check_state() == nwg::CheckBoxState::Checked;
        let altn = text(&self.ui.plan_altn);
        opts.alternate = (!altn.is_empty()).then_some(altn);
        // The callsign, or the airline and number put together, which is what a callsign is
        // when nobody has typed one.
        let typed = text(&self.ui.plan_flight);
        let joined = format!("{}{}", text(&self.ui.plan_airline), text(&self.ui.plan_fltnum));
        let flight = if typed.is_empty() { joined } else { typed };
        opts.flight_number = (!flight.is_empty()).then_some(flight);
        let avoid = text(&self.ui.plan_avoid);
        opts.avoid_firs = avoid.split(',').map(|p| p.trim().to_uppercase()).filter(|p| !p.is_empty()).collect();
        // Off blocks as `HHMM`, taken as the next such time; left empty it stays an hour from
        // now, which is what `DispatchOptions::new` already chose.
        let when = self.ui.plan_eobt.text().trim().to_string();
        if when.len() == 4 {
            if let (Ok(hh), Ok(mm)) = (when[..2].parse::<u32>(), when[2..].parse::<u32>()) {
                use chrono::{Datelike, TimeZone, Utc};
                let now = Utc::now();
                if let Some(t) = Utc.with_ymd_and_hms(now.year(), now.month(), now.day(), hh.min(23), mm.min(59), 0).single() {
                    opts.off_block = if t < now { t + chrono::Duration::days(1) } else { t };
                }
            }
        }
        let reg = text(&self.ui.plan_reg);
        opts.registration = (!reg.is_empty()).then_some(reg);
        let level = text(&self.ui.plan_level);
        opts.level = level.trim_start_matches("FL").parse::<f64>().ok().map(|v| if v < 1000.0 { v * 100.0 } else { v });

        let planning = Arc::new(Planning::default());
        self.state.borrow_mut().planning = Some(planning.clone());
        self.ui.plan_status.set_text("searching...");
        self.ui.plan_result.set_text("");
        self.ui.plan_route.set_text("");
        self.ui.plan_go.set_enabled(false);
        self.ui.plan_timer.start();

        let notice = self.shared.notice.lock().ok().and_then(|n| *n);
        std::thread::spawn(move || {
            amdbgen::route::progress::watch(Some(planning.clone()));
            let planned = amdbgen::ofp::dispatch(&opts);
            amdbgen::route::progress::watch(None);
            if let Ok(mut out) = planning.outcome.lock() {
                *out = Some(match planned {
                    Ok(d) => {
                        let ground = d.route.distance_nm();
                        let direct = amdbgen::dispatch::distance_nm(d.route.origin.pos, d.route.destination.pos);
                        Ok((
                            format!(
                                "{} nm flown, {} nm direct ({:+.0}%), {} kg block",
                                ground.round(),
                                direct.round(),
                                (ground / direct.max(1.0) - 1.0) * 100.0,
                                d.perf.fuel.block_kg.round()
                            ),
                            d.route.route_string(),
                        ))
                    }
                    Err(e) => Err(format!("{e:#}")),
                });
            }
            planning.done.store(true, Ordering::Relaxed);
            if let Some(n) = notice {
                n.notice();
            }
        });
    }

    /// Redraw the panel's map from whatever the search has reported so far.
    fn redraw_plan(&self) {
        use amdbgen::output::routemap::{self, Map};
        let held = self.state.borrow().planning.clone();
        let (reached, best, ends) = match &held {
            Some(p) => (
                p.reached.lock().map(|r| r.clone()).unwrap_or_default(),
                p.best.lock().map(|b| b.clone()).unwrap_or_default(),
                p.ends.lock().map(|e| *e).unwrap_or_default(),
            ),
            None => (Vec::new(), None, None),
        };
        let (origin, destination) = ends.unwrap_or(((0.0, 0.0), (0.0, 0.0)));
        let map = Map {
            origin,
            destination,
            origin_name: self.ui.plan_from.text().trim().to_uppercase(),
            destination_name: self.ui.plan_to.text().trim().to_uppercase(),
            route: best.unwrap_or_default().into_iter().map(|p| (String::new(), p)).collect(),
            ellipses: Vec::new(),
            corridor: Vec::new(),
            network: amdbgen::route::Graph::shared().fixes().iter().map(|f| f.pos).collect(),
            explored: reached,
            floor: Vec::new(),
            caption: String::new(),
        };
        let Ok(png) = routemap::png_bytes(&map, MAP_W as f32, MAP_H as f32, 1.0) else { return };
        let mut bitmap = nwg::Bitmap::default();
        if nwg::Bitmap::builder().source_bin(Some(&png)).build(&mut bitmap).is_ok() {
            self.ui.plan_map.set_bitmap(Some(&bitmap));
            // The frame keeps only a handle, so the bitmap has to outlive this call.
            self.state.borrow_mut().plan_bitmap = Some(bitmap);
        }
    }

    /// Called while a plan is being worked out: redraw, and finish when it is done.
    fn tick_planning(&self) {
        self.redraw_plan();
        let held = self.state.borrow().planning.clone();
        let Some(p) = held else { return };
        if !p.done.load(Ordering::Relaxed) {
            let n = p.reached.lock().map(|r| r.len()).unwrap_or(0);
            self.ui.plan_status.set_text(&format!("searching... {} states", n * 512));
            return;
        }
        self.ui.plan_timer.stop();
        self.ui.plan_go.set_enabled(true);
        match p.outcome.lock().ok().and_then(|o| o.clone()) {
            Some(Ok((summary, route))) => {
                self.ui.plan_status.set_text("planned");
                self.ui.plan_result.set_text(&summary);
                self.ui.plan_route.set_text(&route);
            }
            Some(Err(e)) => {
                self.ui.plan_status.set_text("no route");
                self.ui.plan_route.set_text(&e);
            }
            None => self.ui.plan_status.set_text("stopped"),
        }
        self.state.borrow_mut().planning = None;
    }

    fn refresh_inventory(&self) {
        let (redirect, serving_redirect, xplane_on) = {
            let st = self.state.borrow();
            (st.settings.navigraph_redirect, st.running.as_ref().map_or(false, |r| r.redirected), st.settings.xplane)
        };
        self.ui.refresh.set_enabled(false);
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let inv = take_inventory(redirect, serving_redirect, xplane_on);
            *shared.inventory.lock().unwrap() = Some(inv);
            shared.wake();
        });
    }

    fn show_inventory(&self, inv: Inventory) {
        let lv = &self.ui.sims;
        lv.set_redraw(false);
        lv.clear();
        for (r, row) in inv.rows.iter().enumerate() {
            for (c, text) in row.iter().enumerate() {
                let item = nwg::InsertListViewItem { index: Some(r as i32), column_index: c as i32, text: Some(text.clone()), image: None };
                if c == 0 {
                    lv.insert_item(item);
                } else {
                    lv.update_item(r, item);
                }
            }
        }
        lv.set_redraw(true);
        self.ui.install.set_enabled(inv.can_install);
        match inv.tablets_on {
            Some(on) => {
                self.ui.chart_tablets.set_text(if on { "Tablet charts: ON" } else { "Tablet charts: OFF" });
                self.ui.chart_tablets.set_enabled(true);
            }
            None => {
                self.ui.chart_tablets.set_text("No chart tablets found");
                self.ui.chart_tablets.set_enabled(false);
            }
        }
        self.ui.remove.set_enabled(inv.can_remove);
        self.ui.refresh.set_enabled(true);
    }

    /// Write the aircraft report in the background; reading every aircraft's panel code
    /// takes a little while in a full Community folder.
    fn collect_report(&self) {
        self.ui.collect.set_enabled(false);
        amdbgen::term::info("Collecting aircraft information…");
        self.drain_lines_only();
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let result = amdbgen::bridge::diagnostics::collect_to_downloads(None);
            *shared.report.lock().unwrap() = Some(result);
            shared.wake();
        });
    }

    /// Put a copy of the log in Downloads, where it is easy to attach to a message.
    fn save_log(&self) {
        match amdbgen::bridge::diagnostics::save_log_copy(&desktop::downloads_dir()) {
            Ok(Some(path)) => {
                amdbgen::term::success(&format!("Log saved to Downloads: {}", path.file_name().unwrap_or_default().to_string_lossy()));
                desktop::reveal_file(&path);
            }
            Ok(None) => amdbgen::term::warn("There is no log yet"),
            Err(e) => amdbgen::term::error(&format!("Could not save the log: {e:#}")),
        }
        self.drain_lines_only();
    }

    fn install_map(&self) {
        let sims = desktop::detect_sims();
        let mut ok = 0;
        for sim in &sims {
            match desktop::install_a220_map(&sim.community) {
                Ok(notes) => {
                    ok += 1;
                    notes.iter().for_each(|n| amdbgen::term::success(&format!("{}: {n}", sim.name)));
                }
                Err(e) => amdbgen::term::error(&format!("{}: {e:#}", sim.name)),
            }
        }
        self.drain_lines_only();
        self.refresh_inventory();
        if ok > 0 && desktop::sim_running() {
            nwg::modal_info_message(&self.ui.window, "AMDB Bridge", "The A220 map is installed. Restart Microsoft Flight Simulator to load it.");
        }
    }

    fn remove_map(&self) {
        let choice = nwg::modal_message(
            &self.ui.window,
            &nwg::MessageParams {
                title: "AMDB Bridge",
                content: "Remove the A220 moving map from every simulator on this computer?\n\nAny other A220 map it set aside is put back.",
                buttons: nwg::MessageButtons::YesNo,
                icons: nwg::MessageIcons::Question,
            },
        );
        if choice != nwg::MessageChoice::Yes {
            return;
        }
        for sim in desktop::detect_sims() {
            match desktop::remove_a220_map(&sim.community) {
                Ok(true) => amdbgen::term::success(&format!("{}: A220 moving map removed", sim.name)),
                Ok(false) => {}
                Err(e) => amdbgen::term::error(&format!("{}: {e:#}", sim.name)),
            }
        }
        self.drain_lines_only();
        self.refresh_inventory();
    }

    // ----------------------------------------------------------------------------------
    // Options

    fn load_options(&self) {
        self.loading.set(true);
        let st = self.state.borrow();
        let s = &st.settings;
        let ui = &self.ui;
        ui.opt_start.set_check_state(checked(s.start_on_open));
        ui.opt_login.set_check_state(checked(desktop::run_at_login()));
        ui.opt_sim.set_check_state(checked(desktop::start_with_sim()));
        ui.opt_xplane.set_check_state(checked(s.xplane));
        ui.opt_redirect.set_check_state(checked(s.navigraph_redirect));
        ui.opt_cache.set_check_state(checked(s.cache));
        ui.folder.set_text(&s.cache_dir.display().to_string());
        ui.limit.set_text(&if s.limit_mb == 0 { String::new() } else { s.limit_mb.to_string() });
        for c in [&ui.folder_change as &dyn Enable, &ui.limit, &ui.folder] {
            c.enable(s.cache);
        }
        drop(st);
        self.loading.set(false);
    }

    fn save(&self) {
        let st = self.state.borrow();
        if let Err(e) = st.settings.save() {
            drop(st);
            amdbgen::term::error(&format!("Could not save settings: {e:#}"));
            self.drain_lines_only();
        }
    }

    fn option_start(&self) {
        self.state.borrow_mut().settings.start_on_open = is_checked(&self.ui.opt_start);
        self.save();
    }

    fn exe() -> PathBuf {
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("AMDB Bridge.exe"))
    }

    fn option_login(&self) {
        let on = is_checked(&self.ui.opt_login);
        if let Err(e) = desktop::set_run_at_login(on, &Self::exe()) {
            self.ui.opt_login.set_check_state(checked(!on));
            nwg::modal_error_message(&self.ui.window, "AMDB Bridge", &format!("{e:#}"));
        }
    }

    fn option_sim(&self) {
        let on = is_checked(&self.ui.opt_sim);
        match desktop::set_start_with_sim(on, &Self::exe()) {
            Ok(files) => {
                for f in files {
                    amdbgen::term::info(&format!("Updated {}", f.display()));
                }
                self.drain_lines_only();
            }
            Err(e) => {
                self.ui.opt_sim.set_check_state(checked(desktop::start_with_sim()));
                nwg::modal_error_message(&self.ui.window, "AMDB Bridge", &format!("{e:#}"));
            }
        }
    }

    fn option_xplane(&self) {
        self.state.borrow_mut().settings.xplane = is_checked(&self.ui.opt_xplane);
        self.save();
        self.refresh_inventory();
    }

    fn option_redirect(&self) {
        let on = is_checked(&self.ui.opt_redirect);
        if on {
            let choice = nwg::modal_message(
                &self.ui.window,
                &nwg::MessageParams {
                    title: "Serve the A350 and A380X",
                    content: "The iniBuilds A350 and FlyByWire A380X ask Navigraph's map server for airports directly. To answer them, AMDB Bridge points that address at this computer, installs a local certificate, and patches the A350's EFB so it does not ask you to sign in.\n\nWindows asks for administrator permission once. While this option is on, those aircraft get their airport maps from AMDB Bridge, so keep it running when you fly them. Untick it, or uninstall AMDB Bridge, to undo all of it.\n\nThe A220 map and X-Plane do not need this.",
                    buttons: nwg::MessageButtons::OkCancel,
                    icons: nwg::MessageIcons::Info,
                },
            );
            if choice != nwg::MessageChoice::Ok {
                self.ui.opt_redirect.set_check_state(checked(false));
                return;
            }
        }
        // The setup runs as a separate elevated copy; this one waits for it.
        let code = setup_navigraph(on, false);
        self.drain_lines_only();
        if code != 0 {
            self.ui.opt_redirect.set_check_state(checked(!on));
            return;
        }
        self.state.borrow_mut().settings.navigraph_redirect = on;
        self.save();
        let serving = self.state.borrow().phase == Phase::Serving;
        if serving {
            // Restart so the change applies now rather than on the next start.
            self.toggle_serving();
            self.start_serving(true);
        }
        self.refresh_inventory();
    }

    fn option_cache(&self) {
        let on = is_checked(&self.ui.opt_cache);
        self.state.borrow_mut().settings.cache = on;
        self.save();
        for c in [&self.ui.folder_change as &dyn Enable, &self.ui.limit, &self.ui.folder] {
            c.enable(on);
        }
        self.applies_next_start();
        self.scan_cache();
        self.show_phase();
    }

    fn change_folder(&self) {
        let current = self.state.borrow().settings.cache_dir.clone();
        let _ = std::fs::create_dir_all(&current);
        let _ = self.ui.folder_dialog.set_default_folder(&current.display().to_string());
        if !self.ui.folder_dialog.run(Some(&self.ui.window)) {
            return;
        }
        let Ok(chosen) = self.ui.folder_dialog.get_selected_item() else { return };
        let dir = PathBuf::from(chosen);
        self.state.borrow_mut().settings.cache_dir = dir.clone();
        self.save();
        self.ui.folder.set_text(&dir.display().to_string());
        self.applies_next_start();
        self.scan_cache();
    }

    fn option_limit(&self) {
        if self.loading.get() {
            return;
        }
        let digits: String = self.ui.limit.text().chars().filter(char::is_ascii_digit).collect();
        let mb = digits.parse::<u64>().unwrap_or(0);
        if self.state.borrow().settings.limit_mb == mb {
            return;
        }
        self.state.borrow_mut().settings.limit_mb = mb;
        self.save();
        self.show_phase();
    }

    fn applies_next_start(&self) {
        if self.state.borrow().phase == Phase::Serving {
            amdbgen::term::info("Storage changes apply the next time serving starts");
            self.drain_lines_only();
        }
    }
}

/// Controls that can be greyed out together.
trait Enable {
    fn enable(&self, on: bool);
}

impl Enable for nwg::Button {
    fn enable(&self, on: bool) {
        self.set_enabled(on);
    }
}

impl Enable for nwg::TextInput {
    fn enable(&self, on: bool) {
        self.set_enabled(on);
    }
}
