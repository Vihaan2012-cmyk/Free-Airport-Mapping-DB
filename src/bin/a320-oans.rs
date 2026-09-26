//! A320 OANS: the airport moving map for the Fenix A320's captain ND, on its own.
//!
//! The same bridge as AMDB Bridge, serving airports to the A320 OANS only: no desktop
//! window, no other aircraft, no hosts-file redirect and nothing that needs administrator
//! rights. It lives in the notification area, serves on http://127.0.0.1:8770 while it
//! runs, and adds the OANS to the Fenix again after a Fenix update. If AMDB Bridge is
//! already serving, it leaves the job to it and closes.
//!
//! Steps for the installer:
//!
//! * `--install`                 install the A320 OANS package and add it to the Fenix;
//!                               `--community PATH` (more than once if wanted) names the
//!                               Community folder instead of finding it
//! * `--start-with-sim on|off`   start (or stop starting) with Microsoft Flight Simulator
//!
//! Its menu also has "Choose Community folder...", for a simulator whose Community folder
//! is not where its settings file says; it is offered on start when no Fenix is found.
//! The simulator starts it with `--from-sim`, and then it does not ask.
//! * `--quit`                    ask a running copy to exit, and wait for it
//! * `--uninstall`               remove the package, put the Fenix's files back, and the
//!                               simulator start-up entry

#![windows_subsystem = "windows"]

#[cfg(windows)]
fn main() {
    std::process::exit(app::main());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The A320 OANS app is for Windows. Use `amdb-bridge serve` instead.");
    std::process::exit(1);
}

#[cfg(windows)]
mod app {
    extern crate native_windows_gui as nwg;

    use amdbgen::bridge::{desktop, patcher, service, settings::Settings};
    use std::cell::RefCell;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;
    use std::ptr;
    use std::rc::Rc;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use winapi::shared::minwindef::FALSE;
    use winapi::shared::winerror::ERROR_ALREADY_EXISTS;
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::synchapi::{CreateEventW, CreateMutexW, OpenEventW, OpenMutexW, SetEvent, WaitForSingleObject};
    use winapi::um::winbase::INFINITE;
    use winapi::um::winnt::{EVENT_MODIFY_STATE, SYNCHRONIZE};

    /// The name of its simulator start-up entry, beside AMDB Bridge's own.
    const AUTOSTART: &str = "A320 OANS";
    const MUTEX: &str = "A320OANS.Instance";
    const QUIT: &str = "A320OANS.Quit";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn log_path() -> PathBuf {
        amdbgen::bridge::settings::app_dir().join("a320-oans.log")
    }

    /// Status lines go to a320-oans.log beside AMDB Bridge's settings.
    fn log_to_file() {
        let path = log_path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        if std::fs::metadata(&path).map_or(false, |m| m.len() > 2_000_000) {
            let _ = std::fs::remove_file(&path);
        }
        let file = Mutex::new(OpenOptions::new().create(true).append(true).open(&path).ok());
        amdbgen::term::set_sink(Box::new(move |label, msg| {
            if let Some(f) = file.lock().unwrap().as_mut() {
                let _ = writeln!(f, "{} {label:>7}  {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
            }
        }));
        amdbgen::term::init(false);
    }

    pub fn main() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let has = |flag: &str| args.iter().any(|a| a == flag);
        log_to_file();
        if has("--quit") {
            return i32::from(!quit_running());
        }
        if has("--install") {
            let named = args.windows(2).filter(|w| w[0] == "--community").map(|w| PathBuf::from(&w[1])).collect();
            return install(named);
        }
        if has("--uninstall") {
            quit_running();
            return uninstall();
        }
        if let Some(i) = args.iter().position(|a| a == "--start-with-sim") {
            return start_with_sim(args.get(i + 1).map_or(true, |v| v != "off"));
        }
        serve(has("--from-sim"))
    }

    fn install(named: Vec<PathBuf>) -> i32 {
        let mut failed = false;
        let mut found = false;
        let sims: Vec<desktop::Sim> = if named.is_empty() {
            desktop::detect_sims()
        } else {
            named.into_iter().map(|community| desktop::Sim { name: "Community".to_string(), community }).collect()
        };
        if sims.is_empty() {
            amdbgen::term::warn("No simulator settings (UserCfg.opt) found for MSFS 2020 or 2024; name the Community folder with --community");
        }
        for sim in sims {
            amdbgen::term::info(&format!("{}: looking in {}", sim.name, sim.community.display()));
            if !desktop::a320_oans_fits(&sim.community) {
                for line in desktop::fenix_diagnosis(&sim.community) {
                    amdbgen::term::info(&format!("{}:   {line}", sim.name));
                }
                continue;
            }
            found = true;
            match desktop::install_a320_oans(&sim.community) {
                Ok(notes) => notes.iter().for_each(|n| amdbgen::term::success(&format!("{}: {n}", sim.name))),
                Err(e) => {
                    failed = true;
                    amdbgen::term::error(&format!("{}: {e:#}", sim.name));
                }
            }
        }
        if !found {
            amdbgen::term::warn("No Fenix A320 found in any simulator's Community folder");
        }
        i32::from(failed)
    }

    fn uninstall() -> i32 {
        for sim in desktop::detect_sims() {
            if let Err(e) = desktop::remove_a320_oans(&sim.community) {
                amdbgen::term::warn(&format!("{}: {e:#}", sim.name));
            }
        }
        if let Err(e) = patcher::remove_autostart_as(AUTOSTART) {
            amdbgen::term::warn(&format!("simulator start-up entry: {e:#}"));
        }
        0
    }

    fn start_with_sim(on: bool) -> i32 {
        let Ok(exe) = std::env::current_exe() else { return 1 };
        let result = patcher::remove_autostart_as(AUTOSTART).and_then(|_| if on { patcher::install_autostart_as(AUTOSTART, &exe, "--from-sim") } else { Ok(Vec::new()) });
        match result {
            Ok(_) => 0,
            Err(e) => {
                amdbgen::term::error(&format!("simulator start-up entry: {e:#}"));
                1
            }
        }
    }

    /// Ask the running copy to exit and wait (up to 15 s) until it has.
    fn quit_running() -> bool {
        let h = unsafe { OpenEventW(EVENT_MODIFY_STATE, FALSE, wide(QUIT).as_ptr()) };
        if h.is_null() {
            return true;
        }
        unsafe {
            SetEvent(h);
            CloseHandle(h);
        }
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            let m = unsafe { OpenMutexW(SYNCHRONIZE, FALSE, wide(MUTEX).as_ptr()) };
            if m.is_null() {
                return true;
            }
            unsafe { CloseHandle(m) };
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }

    struct Ui {
        window: nwg::MessageWindow,
        icon: nwg::Icon,
        tray: nwg::TrayNotification,
        menu: nwg::Menu,
        status: nwg::MenuItem,
        sep: nwg::MenuSeparator,
        choose: nwg::MenuItem,
        with_sim: nwg::MenuItem,
        open_log: nwg::MenuItem,
        exit: nwg::MenuItem,
        notice: nwg::Notice,
        folder_dialog: nwg::FileDialog,
    }

    fn tell(title: &str, content: &str, icon: nwg::MessageIcons) {
        nwg::message(&nwg::MessageParams { title, content, buttons: nwg::MessageButtons::Ok, icons: icon });
    }

    /// Pick a Community folder and add the A320 OANS to the Fenix in it, remembering the
    /// folder so it is looked in from now on.
    fn choose_community(ui: &Ui) {
        if !ui.folder_dialog.run(None::<&nwg::Window>) {
            return;
        }
        let Ok(chosen) = ui.folder_dialog.get_selected_item() else { return };
        let mut community = PathBuf::from(chosen);
        // The folder above it (where MSFS keeps Community and Official) will do too.
        if !desktop::a320_oans_fits(&community) && community.join("Community").is_dir() {
            community = community.join("Community");
        }
        amdbgen::term::info(&format!("Community folder chosen: {}", community.display()));
        if !desktop::a320_oans_fits(&community) {
            let why = desktop::fenix_diagnosis(&community);
            why.iter().for_each(|l| amdbgen::term::info(&format!("  {l}")));
            tell(
                "A320 OANS",
                &format!("There is no Fenix A320 in\n{}\nthat the A320 OANS can be added to.\n\n{}\n\nChoose the Community folder the Fenix A320 is installed in.", community.display(), why.join("\n")),
                nwg::MessageIcons::Warning,
            );
            return;
        }
        match desktop::install_a320_oans(&community) {
            Ok(notes) => {
                notes.iter().for_each(|n| amdbgen::term::success(n));
                if let Err(e) = desktop::remember_community_folder(&community) {
                    amdbgen::term::warn(&format!("could not remember the folder: {e:#}"));
                }
                tell(
                    "A320 OANS",
                    &format!("The A320 OANS is added to the Fenix A320 in\n{}\n\nRestart Microsoft Flight Simulator, then turn the captain's ND range knob anticlockwise past 10.", community.display()),
                    nwg::MessageIcons::Info,
                );
            }
            Err(e) => {
                amdbgen::term::error(&format!("{e:#}"));
                tell("A320 OANS", &format!("The A320 OANS could not be added:\n\n{e:#}\n\nIs the simulator running? Close it and try again."), nwg::MessageIcons::Error);
            }
        }
    }

    fn serve(from_sim: bool) -> i32 {
        let mutex = unsafe { CreateMutexW(ptr::null_mut(), FALSE, wide(MUTEX).as_ptr()) };
        if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            return 0; // already running
        }
        let quit = unsafe { CreateEventW(ptr::null_mut(), FALSE, FALSE, wide(QUIT).as_ptr()) };

        let settings = Settings::load().unwrap_or_else(|| {
            let s = Settings::default();
            let _ = s.save();
            s
        });
        // Only the A320 OANS: no Navigraph redirect, no X-Plane.
        let opts = service::Options { navigraph_redirect: false, xplane: false };
        let running = match service::start(&settings, &opts) {
            Ok(r) => r,
            Err(e) => {
                // Most often AMDB Bridge itself, which serves the OANS as well.
                amdbgen::term::warn(&format!("Not serving: {e:#}"));
                return 0;
            }
        };

        if nwg::init().is_err() {
            return 1;
        }
        let mut ui = Ui {
            window: Default::default(),
            icon: Default::default(),
            tray: Default::default(),
            menu: Default::default(),
            status: Default::default(),
            sep: Default::default(),
            choose: Default::default(),
            with_sim: Default::default(),
            open_log: Default::default(),
            exit: Default::default(),
            notice: Default::default(),
            folder_dialog: Default::default(),
        };
        let built = (|| -> Result<(), nwg::NwgError> {
            nwg::MessageWindow::builder().build(&mut ui.window)?;
            let res = nwg::EmbedResource::load(None)?;
            ui.icon = res.icon(1, None).unwrap_or_default();
            nwg::TrayNotification::builder().parent(&ui.window).icon(Some(&ui.icon)).tip(Some("A320 OANS: serving airports to the Fenix A320")).build(&mut ui.tray)?;
            nwg::Menu::builder().popup(true).parent(&ui.window).build(&mut ui.menu)?;
            nwg::MenuItem::builder().text("A320 OANS is serving airports").disabled(true).parent(&ui.menu).build(&mut ui.status)?;
            nwg::MenuSeparator::builder().parent(&ui.menu).build(&mut ui.sep)?;
            nwg::MenuItem::builder().text("Choose Community folder...").parent(&ui.menu).build(&mut ui.choose)?;
            nwg::MenuItem::builder().text("Start with Microsoft Flight Simulator").check(patcher::autostart_installed_as(AUTOSTART)).parent(&ui.menu).build(&mut ui.with_sim)?;
            nwg::MenuItem::builder().text("Open log").parent(&ui.menu).build(&mut ui.open_log)?;
            nwg::MenuItem::builder().text("Exit").parent(&ui.menu).build(&mut ui.exit)?;
            nwg::Notice::builder().parent(&ui.window).build(&mut ui.notice)?;
            nwg::FileDialog::builder().title("The Community folder the Fenix A320 is installed in").action(nwg::FileDialogAction::OpenDirectory).build(&mut ui.folder_dialog)?;
            Ok(())
        })();
        if built.is_err() {
            return 1;
        }

        // The installer's --quit (or another copy's) arrives on another thread.
        let sender = ui.notice.sender();
        let quit_addr = quit as usize;
        std::thread::spawn(move || {
            if unsafe { WaitForSingleObject(quit_addr as _, INFINITE) } == 0 {
                sender.notice();
            }
        });

        let ui = Rc::new(RefCell::new(ui));
        let weak = Rc::downgrade(&ui);
        let handle = ui.borrow().window.handle;
        let handler = nwg::full_bind_event_handler(&handle, move |evt, _data, h| {
            let Some(ui) = weak.upgrade() else { return };
            let ui = ui.borrow();
            match evt {
                nwg::Event::OnContextMenu if h == ui.tray.handle => {
                    let mut p = winapi::shared::windef::POINT { x: 0, y: 0 };
                    unsafe { winapi::um::winuser::GetCursorPos(&mut p) };
                    ui.menu.popup(p.x, p.y);
                }
                nwg::Event::OnMenuItemSelected if h == ui.choose.handle => choose_community(&ui),
                nwg::Event::OnMenuItemSelected if h == ui.with_sim.handle => {
                    let on = !ui.with_sim.checked();
                    if start_with_sim(on) == 0 {
                        ui.with_sim.set_checked(on);
                    }
                }
                nwg::Event::OnMenuItemSelected if h == ui.open_log.handle => {
                    desktop::reveal_file(&log_path());
                }
                nwg::Event::OnMenuItemSelected if h == ui.exit.handle => nwg::stop_thread_dispatch(),
                nwg::Event::OnNotice if h == ui.notice.handle => nwg::stop_thread_dispatch(),
                _ => {}
            }
        });
        amdbgen::term::success(&format!("A320 OANS serving on http://127.0.0.1:{}", running.http_port));
        // No Fenix it can use anywhere it looked: offer to choose the folder, unless the
        // simulator started it (a question box in the middle of loading would only annoy).
        let sims = desktop::detect_sims();
        if !from_sim && !sims.iter().any(|s| desktop::a320_oans_fits(&s.community)) {
            for sim in &sims {
                amdbgen::term::info(&format!("{}: looking in {}", sim.name, sim.community.display()));
                desktop::fenix_diagnosis(&sim.community).iter().for_each(|l| amdbgen::term::info(&format!("{}:   {l}", sim.name)));
            }
            let ask = nwg::message(&nwg::MessageParams {
                title: "A320 OANS",
                content: "No Fenix A320 was found in your simulator's Community folder.\n\nChoose the Community folder the Fenix A320 is installed in?",
                buttons: nwg::MessageButtons::YesNo,
                icons: nwg::MessageIcons::Question,
            });
            if ask == nwg::MessageChoice::Yes {
                choose_community(&ui.borrow());
            }
        }
        nwg::dispatch_thread_events();
        nwg::unbind_event_handler(&handler);
        drop(running);
        unsafe {
            CloseHandle(quit);
            CloseHandle(mutex);
        }
        0
    }
}
