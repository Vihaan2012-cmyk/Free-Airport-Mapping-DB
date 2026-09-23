//! The operational flight plan: everything planned, put together and printed.

use crate::dispatch::Dispatch;

/// What a flight is to be planned for.
#[derive(Debug, Clone, Default)]
pub struct DispatchOptions {
    pub origin: String,
    pub destination: String,
    pub aircraft: String,
}

/// Plan a flight from end to end.
pub fn dispatch(opts: &DispatchOptions) -> anyhow::Result<Dispatch> {
    let _ = opts;
    anyhow::bail!("flight planning is not built yet")
}
