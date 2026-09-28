//! AMDB Build List: drop a list of airports on it and it builds them, with nothing to
//! install. The window it opens shows each airport as it is built and the time left; the
//! airports go into `airports` beside the program, with the downloads and the airport
//! index in folders beside that, and `bulk-status.csv` says how each one went. Airports
//! already built there are skipped, so dropping the same list again carries on.
//!
//! A list is a CSV with an `icao` column (as `amdbgen list --csv` writes) or one airport
//! code a line. Opened on its own, it asks for the file to be dragged into its window.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

fn main() {
    amdbgen::term::init(false);
    amdbgen::term::banner("AMDB Build List");
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."));
    let mut files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if files.is_empty() {
        println!("Drag a list of airports onto this window and press Enter.");
        println!("A list is a CSV with an `icao` column, or one airport code a line.");
        print!("> ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_ok() {
            // A file dragged into a console arrives as its path, in quotes when it has spaces.
            let path = line.trim().trim_matches('"');
            if !path.is_empty() {
                files.push(PathBuf::from(path));
            }
        }
    }
    let mut failed = false;
    for file in &files {
        amdbgen::term::start(&format!("Building the airports in {}", file.display()));
        match amdbgen::bridge::cli::build_list_portable(file, &dir) {
            Ok(n) => amdbgen::term::success(&format!("Done with the {n} airports in {}", file.file_name().unwrap_or_default().to_string_lossy())),
            Err(e) => {
                amdbgen::term::error(&format!("{}: {e:#}", file.display()));
                failed = true;
            }
        }
    }
    if !files.is_empty() {
        amdbgen::term::info(&format!("The airports are in {}", dir.join("airports").display()));
    }
    // Opened by a drop, the window would close with the program: it waits to be read.
    print!("\nPress Enter to close this window.");
    let _ = std::io::stdout().flush();
    let _ = std::io::stdin().lock().read_line(&mut String::new());
    std::process::exit(i32::from(failed));
}
