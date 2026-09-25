//! amdb-navdata: the navigation-data converter on its own.
//!
//! The same converter `amdbgen navdata` runs, behind its own front door and shipped as
//! its own download. It reads the simulator's own navigation data -- FS2020's loose files
//! or FS2024's packed archive -- and writes it into the database an add-on aircraft reads,
//! so the aeroplane flies on current data rather than whatever cycle it shipped with.
//!
//! It is one implementation with two ways in, not a copy: `amdbgen::cli::convert_cmd` is
//! what both call, so the standalone download and the subcommand cannot drift apart.
//! Nothing is taken out of `amdbgen` to make this exist.

use amdbgen::cli::ConvertArgs;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "amdb-navdata",
    version,
    about = "Write an aircraft's navigation database from the simulator's own data",
    long_about = "Reads the navigation data Microsoft Flight Simulator already has on this \
                  computer and writes it into the database an add-on aircraft reads, so the \
                  aeroplane flies on current data instead of the cycle it shipped with.\n\n\
                  Nothing is downloaded and nothing licensed is redistributed: the data is \
                  the simulator's, and it stays on this machine. The aircraft's own database \
                  is never overwritten without a backup being kept beside it first.\n\n\
                  This is the same converter as `amdbgen navdata`, on its own."
)]
struct Cli {
    #[command(flatten)]
    convert: ConvertArgs,
    /// Show debug output.
    #[arg(short, long, global = true)]
    verbose: bool,
}

fn main() {
    let cli = Cli::parse();
    amdbgen::term::init(cli.verbose || std::env::var("AMDB_VERBOSE").is_ok());
    amdbgen::term::banner("amdb-navdata");
    if let Err(e) = amdbgen::cli::convert_cmd(cli.convert) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
