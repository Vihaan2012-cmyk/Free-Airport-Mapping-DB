//! AMDB Build List: drop a list of airports on it and it builds them, with nothing to
//! install. The window it opens shows each airport as it is built and the time left; the
//! airports go into `airports` beside the program, with the downloads and the airport
//! index in folders beside that, and `bulk-status.csv` says how each one went. Airports
//! already built there are skipped, so dropping the same list again carries on.
//!
//! A list is a CSV with an `icao` column (as `amdbgen list --csv` writes) or one airport
//! code a line. An OpenStreetMap extract (`.osm.pbf`, from download.geofabrik.de) dropped
//! with it is read for OSM instead of asking the servers airport by airport: much faster,
//! and the way to build thousands. Opened on its own, it asks for the files to be dragged
//! into its window.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Paths as a console pastes dragged files: separated by spaces, in quotes when they have one.
fn paths_in(line: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        let (path, after) = if let Some(quoted) = rest.strip_prefix('"') {
            match quoted.find('"') {
                Some(end) => (&quoted[..end], &quoted[end + 1..]),
                None => (quoted, ""),
            }
        } else {
            match rest.find(' ') {
                Some(end) => (&rest[..end], &rest[end..]),
                None => (rest, ""),
            }
        };
        if !path.is_empty() {
            out.push(PathBuf::from(path));
        }
        rest = after.trim_start();
    }
    out
}

fn is_extract(p: &Path) -> bool {
    p.to_string_lossy().to_ascii_lowercase().ends_with(".pbf")
}

fn main() {
    amdbgen::term::init(false);
    amdbgen::term::banner("AMDB Build List");
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."));
    let mut files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if files.is_empty() {
        println!("Drag a list of airports onto this window and press Enter.");
        println!("A list is a CSV with an `icao` column, or one airport code a line. Drag an");
        println!("OpenStreetMap extract (.osm.pbf, from download.geofabrik.de) in with it to read");
        println!("OSM from the file instead of downloading it airport by airport.");
        print!("> ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_ok() {
            files = paths_in(&line);
        }
    }
    let extracts: Vec<PathBuf> = files.iter().filter(|p| is_extract(p)).cloned().collect();
    let lists: Vec<&PathBuf> = files.iter().filter(|p| !is_extract(p)).collect();
    for pbf in &extracts {
        amdbgen::term::info(&format!("OpenStreetMap comes from {}", pbf.display()));
    }
    let mut failed = false;
    for file in &lists {
        amdbgen::term::start(&format!("Building the airports in {}", file.display()));
        match amdbgen::bridge::cli::build_list_portable(file, &dir, &extracts) {
            Ok(n) => amdbgen::term::success(&format!("Done with the {n} airports in {}", file.file_name().unwrap_or_default().to_string_lossy())),
            Err(e) => {
                amdbgen::term::error(&format!("{}: {e:#}", file.display()));
                failed = true;
            }
        }
    }
    if lists.is_empty() && !files.is_empty() {
        amdbgen::term::error("No list of airports among the files: drop a CSV or a text file of airport codes");
        failed = true;
    } else if !lists.is_empty() {
        amdbgen::term::info(&format!("The airports are in {}", dir.join("airports").display()));
    }
    // Opened by a drop, the window would close with the program: it waits to be read.
    print!("\nPress Enter to close this window.");
    let _ = std::io::stdout().flush();
    let _ = std::io::stdin().lock().read_line(&mut String::new());
    std::process::exit(i32::from(failed));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragged_paths_are_split_as_a_console_pastes_them() {
        let p = paths_in(r#""C:\My Lists\list 1.csv" C:\osm\europe-latest.osm.pbf"#);
        assert_eq!(p, vec![PathBuf::from(r"C:\My Lists\list 1.csv"), PathBuf::from(r"C:\osm\europe-latest.osm.pbf")]);
        assert!(is_extract(&p[1]) && !is_extract(&p[0]));
        assert!(paths_in("   ").is_empty());
    }
}
