//! amdbgen: builds a Navigraph-style Airport Mapping Database from free sources.

/// mimalloc rather than Windows' heap: a bulk run allocates and frees buffers of megabytes
/// on every thread (decoded map blocks, terrain, pages), and the system heap hands each
/// back to the OS and faults it in again, tens of thousands of faults a second, with the
/// threads queued on the memory manager rather than working.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let verbose = std::env::args().any(|a| a == "-v" || a == "--verbose") || std::env::var("AMDB_VERBOSE").is_ok();
    amdbgen::term::init(verbose);
    amdbgen::term::banner("amdbgen");
    if let Err(e) = amdbgen::cli::run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
