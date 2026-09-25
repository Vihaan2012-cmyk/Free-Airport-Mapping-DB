//! AMDB Navdata: the navigation-data converter with a window round it.
//!
//! The converting is `amdbgen::cli::convert_cmd`, the same call `amdb-navdata` and
//! `amdbgen convert` make. This file is only the window: it fills in the same
//! `ConvertArgs` the command line parses, hands them over, and shows what comes back.
//! Nothing about converting is decided here, so the three ways in cannot disagree about
//! what they do or be fixed one at a time.

#![windows_subsystem = "windows"]

#[cfg(not(windows))]
fn main() {
    eprintln!("The AMDB Navdata window is for Windows. Use `amdb-navdata` instead.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::main());
}

#[cfg(windows)]
mod win {
    use amdbgen::cli::ConvertArgs;
    use native_windows_gui as nwg;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Mutex, OnceLock};

    const VERSION: &str = env!("CARGO_PKG_VERSION");

    /// The aircraft this knows, by the package folder each installs as: what to call it,
    /// and which database layout it reads.
    ///
    /// What actually differs is the layout, not the aeroplane -- most of these read the
    /// same one -- but nobody looking for this thinks in layouts. They think "I fly the
    /// 777-300ER". So the aeroplanes are what is shown, down to the variant, because that
    /// is what is installed and what the folder names say.
    const KNOWN: [(&str, &str, &str); 14] = [
        ("pmdg-aircraft-736", "PMDG 737-600", "dfd"),
        ("pmdg-aircraft-737", "PMDG 737-700", "dfd"),
        ("pmdg-aircraft-738", "PMDG 737-800", "dfd"),
        ("pmdg-aircraft-739", "PMDG 737-900", "dfd"),
        ("pmdg-aircraft-77er", "PMDG 777-200ER", "dfd"),
        ("pmdg-aircraft-77l", "PMDG 777-200LR", "dfd"),
        ("pmdg-aircraft-77f", "PMDG 777F", "dfd"),
        ("pmdg-aircraft-77w", "PMDG 777-300ER", "dfd"),
        ("inibuilds-aircraft-a350", "iniBuilds A350", "dfd"),
        ("inibuilds-aircraft-a320", "iniBuilds A320", "dfd"),
        ("synaptic-aircraft-a220", "Synaptic A220", "dfd"),
        ("fnx-aircraft-319", "Fenix A319", "fenix"),
        ("fnx-aircraft-320", "Fenix A320", "fenix"),
        ("fnx-aircraft-321", "Fenix A321", "fenix"),
    ];

    /// The aircraft actually installed on this machine, in the order above.
    ///
    /// A list of every aeroplane this could convert for would mostly be aeroplanes the
    /// reader does not own. Liveries install as packages too and are skipped: they carry
    /// no navigation database of their own.
    fn installed() -> Vec<(&'static str, &'static str)> {
        let mut found: Vec<(&str, &str)> = Vec::new();
        for dir in amdbgen::bridge::patcher::detect_community_dirs() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                if name.contains("-liveries") || !e.path().is_dir() {
                    continue;
                }
                if let Some((_, label, layout)) = KNOWN.iter().find(|(pkg, _, _)| name == *pkg) {
                    if !found.iter().any(|(l, _)| l == label) {
                        found.push((label, layout));
                    }
                }
            }
        }
        found.sort_by_key(|(label, _)| KNOWN.iter().position(|(_, l, _)| l == label).unwrap_or(usize::MAX));
        // Nothing recognised -- an install this does not know, or none at all. Offer the
        // two layouts by name rather than an empty box with no way forward.
        if found.is_empty() {
            found = vec![("Navigraph layout (A350, A220, PMDG)", "dfd"), ("Fenix layout", "fenix")];
        }
        found
    }

    /// The simulator read. FS2024's navigation data is newer, but its airport and
    /// procedure records are renumbered and re-laid-out in ways this crate has not
    /// finished decoding, so a conversion from it comes out with about a quarter of the
    /// procedures FS2020 gives. `amdb-navdata --sim fs2020` is still there for anyone who
    /// wants the fuller answer.
    const SIM: &str = "fs2024";

    /// What the title bar and the borders add to the height the page needs, in logical
    /// pixels. Measured, not assumed: `.size()` takes the whole window, not its inside.
    const FRAME: i32 = 18;

    /// A line on its way from the converter to the window.
    enum Note {
        Line(String),
        Done(Result<(), String>),
    }

    /// Where the converter's status lines are sent while one is running. The sink can only
    /// be set once for the life of the program, so it is set once and reads this.
    static LINES: Mutex<Option<(mpsc::Sender<Note>, nwg::NoticeSender)>> = Mutex::new(None);
    static BUSY: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static RX: RefCell<Option<mpsc::Receiver<Note>>> = const { RefCell::new(None) };
    }

    #[derive(Default)]
    struct Ui {
        window: nwg::Window,
        font_title: nwg::Font,
        font_small: nwg::Font,
        title: nwg::Label,
        version: nwg::Label,
        subtitle: nwg::Label,
        aircraft_label: nwg::Label,
        aircraft: Vec<nwg::CheckBox>,
        sim_label: nwg::Label,
        cycle_label: nwg::Label,
        cycle: nwg::TextInput,
        cycle_hint: nwg::Label,
        to_file: nwg::RadioButton,
        in_place: nwg::RadioButton,
        out: nwg::TextInput,
        browse: nwg::Button,
        dialog: nwg::FileDialog,
        dry_run: nwg::Button,
        convert: nwg::Button,
        log: nwg::TextBox,
        notice: nwg::Notice,
    }

    pub fn main() -> i32 {
        // A window program has nowhere to print to, so a panic would otherwise take the
        // program away with no word of why. This says what happened before it goes.
        std::panic::set_hook(Box::new(|info| {
            let what = info.payload().downcast_ref::<&str>().map(|s| (*s).to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_else(|| "something went wrong".into());
            let at = info.location().map(|l| format!("

{}:{}", l.file(), l.line())).unwrap_or_default();
            nwg::simple_message("AMDB Navdata", &format!("{what}{at}"));
        }));
        amdbgen::term::init(std::env::var("AMDB_VERBOSE").is_ok());
        if let Err(e) = nwg::init() {
            eprintln!("could not start the window system: {e}");
            return 1;
        }
        let mut font = nwg::Font::default();
        let _ = nwg::Font::builder().family("Segoe UI").size(17).build(&mut font);
        nwg::Font::set_global_default(Some(font));

        let ui = Rc::new(RefCell::new(Ui::default()));
        if let Err(e) = build(&ui) {
            nwg::simple_message("AMDB Navdata", &format!("The window could not be created: {e}"));
            return 1;
        }
        wire(&ui);
        // Built hidden so none of it is drawn half-made, then shown once it is whole.
        {
            let u = ui.borrow();
            u.out.set_enabled(true);
            u.browse.set_enabled(true);
            u.window.set_visible(true);
        }
        nwg::dispatch_thread_events();
        0
    }

    fn build(ui: &Rc<RefCell<Ui>>) -> Result<(), nwg::NwgError> {
        let mut u = ui.borrow_mut();
        nwg::Font::builder().family("Segoe UI").size(28).weight(600).build(&mut u.font_title)?;
        nwg::Font::builder().family("Segoe UI").size(15).build(&mut u.font_small)?;
        // Every control's position and size is given in logical pixels and scaled for the
        // display by the library. The window's own size is not, so on a screen at 150% the
        // controls grow and the window does not, and the right-hand column falls off the
        // edge. It is scaled here to match them.
        let scale = nwg::scale_factor();
        let px = |v: i32| (v as f64 * scale).round() as i32;
        // How many rows of tick boxes there are decides where everything under them sits
        // and how tall the window has to be, so the page is measured rather than guessed
        // at: no strip of nothing under the last thing on it.
        let fleet = installed();
        let rows = fleet.len().div_ceil(3) as i32;
        let under = 92 + rows * 26 + 10;
        let log_top = under + 202;
        let bottom = log_top + 168 + 16;
        nwg::Window::builder()
            .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::MINIMIZE_BOX)
            .size((px(620), px(bottom + FRAME)))
            .center(true)
            .title("AMDB Navdata")
            .build(&mut u.window)?;

        // Every control is parented by the window, which outlives them all.
        let w: &nwg::Window = unsafe { &*(&u.window as *const nwg::Window) };
        let (title, small) = unsafe { (&*(&u.font_title as *const nwg::Font), &*(&u.font_small as *const nwg::Font)) };

        nwg::Label::builder().parent(w).text("AMDB Navdata").font(Some(title)).position((20, 14)).size((400, 34)).build(&mut u.title)?;
        nwg::Label::builder()
            .parent(w)
            .text(&format!("Version {VERSION}"))
            .font(Some(small))
            .h_align(nwg::HTextAlign::Right)
            .position((400, 24))
            .size((196, 20))
            .build(&mut u.version)?;
        nwg::Label::builder()
            .parent(w)
            .text("Writes the navigation data this computer's simulator already has into the\ndatabase an add-on aircraft reads. Nothing is downloaded.")
            .font(Some(small))
            .position((20, 54))
            .size((576, 40))
            .build(&mut u.subtitle)?;

        nwg::Label::builder().parent(w).text("Aircraft").position((20, 94)).size((90, 22)).build(&mut u.aircraft_label)?;
        u.aircraft = Vec::new();
        for (i, (name, _)) in fleet.iter().enumerate() {
            let mut b = nwg::CheckBox::default();
            let (col, row) = (i as i32 % 3, i as i32 / 3);
            nwg::CheckBox::builder()
                .parent(w)
                .text(name)
                .check_state(if i == 0 { nwg::CheckBoxState::Checked } else { nwg::CheckBoxState::Unchecked })
                .position((116 + col * 162, 92 + row * 26))
                .size((158, 22))
                .build(&mut b)?;
            u.aircraft.push(b);
        }

        nwg::Label::builder().parent(w).text("Simulator").position((20, under)).size((90, 22)).build(&mut u.sim_label)?;
        nwg::Label::builder()
            .parent(w)
            .text("Microsoft Flight Simulator 2024")
            .position((116, under))
            .size((300, 22))
            .build(&mut u.cycle_hint)?;

        nwg::Label::builder().parent(w).text("AIRAC").position((20, under + 30)).size((90, 22)).build(&mut u.cycle_label)?;
        nwg::TextInput::builder().parent(w).text(&amdbgen::bridge::server::cycle_code()).position((116, under + 26)).size((80, 28)).build(&mut u.cycle)?;

        nwg::RadioButton::builder()
            .parent(w)
            .text("Write to a file")
            .check_state(nwg::RadioButtonState::Checked)
            .position((116, under + 66))
            .size((150, 24))
            .build(&mut u.to_file)?;
        nwg::RadioButton::builder()
            .parent(w)
            .text("Replace the aircraft's own (a backup is kept beside it)")
            .position((116, under + 92))
            .size((460, 24))
            .build(&mut u.in_place)?;
        nwg::TextInput::builder().parent(w).text("").position((116, under + 120)).size((390, 28)).build(&mut u.out)?;
        nwg::Button::builder().parent(w).text("Browse...").position((512, under + 120)).size((84, 28)).build(&mut u.browse)?;
        nwg::FileDialog::builder()
            .action(nwg::FileDialogAction::Save)
            .title("Where to write the navigation database")
            .filters("Navigation database(*.db3)|Any(*.*)")
            .build(&mut u.dialog)?;

        nwg::Button::builder().parent(w).text("Dry run").position((330, under + 160)).size((124, 32)).build(&mut u.dry_run)?;
        nwg::Button::builder().parent(w).text("Convert").position((464, under + 160)).size((132, 32)).build(&mut u.convert)?;
        nwg::TextBox::builder()
            .parent(w)
            .text("Ready.\r\n")
            .font(Some(small))
            .flags(nwg::TextBoxFlags::VISIBLE | nwg::TextBoxFlags::VSCROLL | nwg::TextBoxFlags::AUTOVSCROLL)
            .readonly(true)
            .position((20, log_top))
            .size((576, 168))
            .build(&mut u.log)?;
        nwg::Notice::builder().parent(w).build(&mut u.notice)?;
        Ok(())
    }

    fn wire(ui: &Rc<RefCell<Ui>>) {
        let handler_ui = ui.clone();
        let window = ui.borrow().window.handle;
        let handler = nwg::full_bind_event_handler(&window, move |evt, _data, handle| {
            let u = handler_ui.borrow();
            match evt {
                nwg::Event::OnWindowClose => nwg::stop_thread_dispatch(),
                nwg::Event::OnButtonClick => {
                    if handle == u.browse.handle {
                        drop(u);
                        browse(&handler_ui);
                    } else if handle == u.dry_run.handle || handle == u.convert.handle {
                        let dry = handle == u.dry_run.handle;
                        drop(u);
                        start(&handler_ui, dry);
                    } else if handle == u.to_file.handle || handle == u.in_place.handle {
                        let to_file = u.to_file.check_state() == nwg::RadioButtonState::Checked;
                        u.out.set_enabled(to_file);
                        u.browse.set_enabled(to_file);
                    }
                }
                nwg::Event::OnNotice => {
                    if handle == u.notice.handle {
                        drop(u);
                        drain(&handler_ui);
                    }
                }
                _ => {}
            }
        });
        // The handler has to outlive this function, and lives as long as the window.
        std::mem::forget(handler);
    }

    fn browse(ui: &Rc<RefCell<Ui>>) {
        let u = ui.borrow();
        if u.dialog.run(Some(&u.window)) {
            if let Ok(p) = u.dialog.get_selected_item() {
                u.out.set_text(&PathBuf::from(p).display().to_string());
            }
        }
    }

    fn say(ui: &Rc<RefCell<Ui>>, line: &str) {
        let u = ui.borrow();
        let mut text = u.log.text();
        text.push_str(line);
        text.push_str("\r\n");
        u.log.set_text(&text);
        u.log.scroll_lastline();
    }

    /// Start a conversion on a worker thread, so the window stays answerable while it
    /// runs and the converter's own status lines come back as it goes.
    fn start(ui: &Rc<RefCell<Ui>>, dry_run: bool) {
        if BUSY.swap(true, Ordering::SeqCst) {
            return;
        }
        let args = match collect(ui, dry_run) {
            Ok(a) => a,
            Err(e) => {
                BUSY.store(false, Ordering::SeqCst);
                say(ui, &format!("! {e}"));
                return;
            }
        };
        {
            let u = ui.borrow();
            u.log.set_text("");
            u.convert.set_enabled(false);
            u.dry_run.set_enabled(false);
        }
        say(ui, if dry_run { "Dry run: nothing will be written." } else { "Converting..." });

        let (tx, rx) = mpsc::channel();
        RX.with(|c| *c.borrow_mut() = Some(rx));
        *LINES.lock().unwrap() = Some((tx.clone(), ui.borrow().notice.sender()));

        // The sink takes effect once only, so it is installed once and reads whichever
        // conversion is running rather than being replaced each time.
        static WIRED: OnceLock<()> = OnceLock::new();
        WIRED.get_or_init(|| {
            amdbgen::term::set_sink(Box::new(|label: &str, msg: &str| {
                let held = LINES.lock().unwrap();
                if let Some((tx, notice)) = held.as_ref() {
                    let mark = match label {
                        "success" => "+",
                        "warn" => "!",
                        "error" => "x",
                        // A program's own output, such as the table of counts, is what
                        // the reader came for; it is not marked up as a status line.
                        "plain" => "",
                        _ => " ",
                    };
                    let _ = tx.send(Note::Line(if mark.is_empty() { msg.to_string() } else { format!("{mark} {msg}") }));
                    notice.notice();
                }
            }));
        });

        std::thread::spawn(move || {
            let outcome = amdbgen::cli::convert_cmd(args).map_err(|e| format!("{e:#}"));
            let held = LINES.lock().unwrap();
            if let Some((tx, notice)) = held.as_ref() {
                let _ = tx.send(Note::Done(outcome));
                notice.notice();
            }
        });
    }

    /// The window's side of the notice: everything said since it was last woken.
    fn drain(ui: &Rc<RefCell<Ui>>) {
        let notes: Vec<Note> = RX.with(|c| c.borrow().as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default());
        for n in notes {
            match n {
                Note::Line(l) => say(ui, &l),
                Note::Done(r) => {
                    BUSY.store(false, Ordering::SeqCst);
                    {
                        let u = ui.borrow();
                        u.convert.set_enabled(true);
                        u.dry_run.set_enabled(true);
                    }
                    match r {
                        Ok(()) => say(ui, "Done."),
                        Err(e) => say(ui, &format!("x {e}")),
                    }
                }
            }
        }
    }

    /// The window's fields as the command line's arguments, so there is one set of rules
    /// about what a valid request is and the window cannot ask for something the command
    /// line would refuse.
    fn collect(ui: &Rc<RefCell<Ui>>, dry_run: bool) -> Result<ConvertArgs, String> {
        let u = ui.borrow();
        let fleet = installed();
        let ticked: Vec<&str> = u
            .aircraft
            .iter()
            .zip(fleet.iter())
            .filter(|(b, _)| b.check_state() == nwg::CheckBoxState::Checked)
            .map(|(_, (_, layout))| *layout)
            .collect();
        if ticked.is_empty() {
            return Err("tick at least one aircraft".into());
        }
        // Three of them read one layout and the Fenix reads another, so a tick list that
        // crosses both is two different databases and cannot be one run.
        let to = ticked[0];
        if ticked.iter().any(|l| *l != to) {
            return Err("the Fenix reads a different layout from the others; convert it on its own".into());
        }
        let sim = SIM;
        let cycle = u.cycle.text().trim().to_string();
        if cycle.is_empty() {
            return Err("give an AIRAC cycle, four digits, such as 2603".into());
        }
        let in_place = u.in_place.check_state() == nwg::RadioButtonState::Checked;
        let out = u.out.text().trim().to_string();
        if !dry_run && !in_place && out.is_empty() {
            return Err("choose a file to write to, or ask for the aircraft's own to be replaced".into());
        }
        if in_place && to == "dfd" && !dry_run {
            return Err("three aircraft read the Navigraph layout, in three places, so there is nowhere fixed to replace: write to a file".into());
        }
        Ok(ConvertArgs {
            to: to.to_string(),
            from_sim: true,
            sim: sim.to_string(),
            cycle,
            from_json: None,
            out: (!in_place && !out.is_empty()).then(|| PathBuf::from(out)),
            in_place: in_place && !dry_run,
            dry_run,
        })
    }
}
