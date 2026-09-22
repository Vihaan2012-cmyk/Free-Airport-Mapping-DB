//! AMDB Bridge desktop app: start and stop the bridge, install the A220 moving map, and
//! set the options, without a terminal. Also the steps the installer runs:
//!
//! * `--install-a220`        install the A220 map into every simulator found
//! * `--run-at-login on|off` add or remove the Windows start-up entry
//! * `--quit`                ask a running copy to exit, and wait for it
//! * `--uninstall`           undo everything outside the program folder
//! * `--tray`                open in the notification area without showing the window

#![windows_subsystem = "windows"]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod instance;

#[cfg(windows)]
fn main() {
    std::process::exit(app::main());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The AMDB Bridge desktop app is for Windows. Use `amdb-bridge serve` instead.");
    std::process::exit(1);
}
