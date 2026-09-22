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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LOG_LINES: usize = 500;

// Status colours, readable on the system dialog background.
const GREEN: [u8; 3] = [16, 124, 16];
const AMBER: [u8; 3] = [176, 104, 0];
const RED: [u8; 3] = [196, 43, 28];
const GREY: [u8; 3] = [96, 96, 96];

/// State shared with the threads that do slow work and with the log sink.
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
}

/// Build the list. `redirect` is the A350/A380X option, `serving_redirect` whether the
/// redirect is in place right now.
fn take_inventory(redirect: bool, serving_redirect: bool, xplane_on: bool) -> Inventory {
    let mut rows: Vec<[String; 3]> = Vec::new();
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
    }
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
            .size((640, 646))
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

        // Options
        nwg::Label::builder().parent(w).text("Options").font(Some(&ui.font_header)).position((20, 336)).size((600, 22)).build(&mut ui.options_header)?;
        let opts: [(&mut nwg::CheckBox, &str); 6] = [
            (&mut ui.opt_start, "Start serving as soon as AMDB Bridge opens"),
            (&mut ui.opt_login, "Open AMDB Bridge in the notification area when Windows starts"),
            (&mut ui.opt_sim, "Open AMDB Bridge when Microsoft Flight Simulator starts"),
            (&mut ui.opt_xplane, "Install the X-Plane 12 moving map when serving starts"),
            (&mut ui.opt_redirect, "Also serve the iniBuilds A350 and FlyByWire A380X (asks for administrator permission once)"),
            (&mut ui.opt_cache, "Keep built airports on disk, so they load instantly next time"),
        ];
        for (i, (cb, text)) in opts.into_iter().enumerate() {
            nwg::CheckBox::builder().parent(w).text(text).position((20, 360 + i as i32 * 23)).size((600, 22)).build(cb)?;
        }
        nwg::TextInput::builder().parent(w).readonly(true).position((40, 500)).size((340, 25)).build(&mut ui.folder)?;
        nwg::Button::builder().parent(w).text("Change…").position((386, 498)).size((90, 29)).build(&mut ui.folder_change)?;
        nwg::Label::builder().parent(w).text("Limit").h_align(nwg::HTextAlign::Right).position((484, 503)).size((40, 22)).build(&mut ui.limit_label)?;
        nwg::TextInput::builder().parent(w).align(nwg::HTextAlign::Right).limit(7).placeholder_text(Some("none")).position((530, 500)).size((58, 25)).build(&mut ui.limit)?;
        nwg::Label::builder().parent(w).text("MB").position((594, 503)).size((26, 22)).build(&mut ui.limit_unit)?;

        // Activity
        nwg::Label::builder().parent(w).text("Activity").font(Some(&ui.font_header)).position((20, 540)).size((200, 22)).build(&mut ui.activity_header)?;
        nwg::Button::builder().parent(w).text("Aircraft report").font(Some(&ui.font_small)).position((258, 537)).size((124, 26)).build(&mut ui.collect)?;
        nwg::Button::builder().parent(w).text("Airports folder").font(Some(&ui.font_small)).position((388, 537)).size((124, 26)).build(&mut ui.open_folder)?;
        nwg::Button::builder().parent(w).text("Save log").font(Some(&ui.font_small)).position((518, 537)).size((102, 26)).build(&mut ui.open_log)?;
        nwg::TextBox::builder()
            .parent(w)
            .readonly(true)
            .flags(nwg::TextBoxFlags::VISIBLE | nwg::TextBoxFlags::VSCROLL | nwg::TextBoxFlags::AUTOVSCROLL | nwg::TextBoxFlags::TAB_STOP)
            .font(Some(&ui.font_small))
            .position((20, 568))
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
                } else if handle == ui.refresh.handle {
                    self.refresh_inventory();
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
            E::OnNotice if handle == ui.notice.handle => self.drain(),
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
