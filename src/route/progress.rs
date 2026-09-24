//! What the search is doing, while it is doing it.
//!
//! A route appears after some seconds with no account of how it was arrived at. That is fine
//! for a flight plan and useless for understanding one: the interesting part of a long search
//! is which way it went first, where it spread out, and how many times it changed its mind
//! before settling. Watching it is how the wandering this module was written alongside became
//! obvious — a route reaching a thousand miles west is one line in a list of fixes and an
//! unmistakable sweep on a map.
//!
//! So a search may be watched. Nothing watches by default and the cost of not watching is one
//! atomic read per search, not per fix: the sink is taken once when a search starts, and where
//! there is none the sampling below never runs at all.
//!
//! The events are a sampling, not a record. A long search expands millions of states and no
//! watcher wants millions of points; one in every [`SAMPLE`] is enough to draw the shape of the
//! thing, and a watcher that cannot keep up is expected to drop what it cannot use rather than
//! slow the search down to its own speed.

use crate::dispatch::LatLon;
use std::sync::{Arc, OnceLock, RwLock};

/// One in how many expansions is reported. A search that expands two million states sends four
/// thousand points, which is a picture; sending all two million would be a slideshow of one
/// frame and a great deal of memory.
pub const SAMPLE: usize = 512;

/// Something that happened while a route was being searched for.
#[derive(Debug, Clone)]
pub enum Event {
    /// A search began, between these two places.
    Started { origin: LatLon, destination: LatLon },
    /// The search reached here. One of these arrives per [`SAMPLE`] expansions.
    Reached { at: LatLon, expanded: usize },
    /// The best way through found so far, which may still be bettered.
    Best { path: Vec<LatLon>, nm: f64 },
    /// A route was settled on, or the search gave up.
    Finished { found: bool },
}

/// Somewhere for those events to go. Implementations must not block: a sink that waits is a
/// search that waits.
pub trait Sink: Send + Sync {
    fn event(&self, event: Event);
}

fn slot() -> &'static RwLock<Option<Arc<dyn Sink>>> {
    static SLOT: OnceLock<RwLock<Option<Arc<dyn Sink>>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Watch every search from now until this is called again with `None`.
///
/// One place for the whole process rather than a parameter threaded through the search, which
/// would have meant an argument on a dozen functions between the caller and the loop that would
/// be `None` in every one of them but this. A watcher is a thing the program is doing, not a
/// property of one route request.
pub fn watch(sink: Option<Arc<dyn Sink>>) {
    if let Ok(mut held) = slot().write() {
        *held = sink;
    }
}

/// The watcher, if there is one. Taken once at the start of a search, never per fix.
pub fn watcher() -> Option<Arc<dyn Sink>> {
    slot().read().ok().and_then(|held| held.clone())
}

/// Report an event, where anything is listening.
pub fn report(event: Event) {
    if let Some(sink) = watcher() {
        sink.event(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Collect(Mutex<Vec<Event>>);

    impl Sink for Collect {
        fn event(&self, event: Event) {
            self.0.lock().unwrap().push(event);
        }
    }

    /// Nothing watches until something does, and nothing is kept when nothing watches.
    #[test]
    fn events_go_nowhere_until_a_watcher_is_set() {
        watch(None);
        assert!(watcher().is_none());
        report(Event::Finished { found: true });

        let sink = Arc::new(Collect::default());
        watch(Some(sink.clone()));
        report(Event::Finished { found: true });
        report(Event::Reached { at: (1.0, 2.0), expanded: SAMPLE });
        watch(None);
        // Sent after the watcher was taken away: not kept.
        report(Event::Finished { found: false });

        let held = sink.0.lock().unwrap();
        assert_eq!(held.len(), 2, "only what was sent while watching");
        assert!(matches!(held[0], Event::Finished { found: true }));
        assert!(matches!(held[1], Event::Reached { at, .. } if at == (1.0, 2.0)));
    }
}
