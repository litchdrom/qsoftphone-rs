//! The handle that says "something changed — draw the window again".
//!
//! ## Why it exists
//!
//! egui only draws when something asks it to. Everything that changes
//! in this app arrives from **outside** egui (§3): the registration, the
//! calls, the media, the history, the result of a script. So the threads
//! that produce those events have to be able to say "there is something
//! new" — otherwise the window sits on the last picture until the user
//! happens to move the mouse.
//!
//! The alternative is a timer: draw ten times a second whether anything
//! happened or not. That is what the app used to do, and it is why an
//! idle softphone cost a few percent of a core for nothing. A wake is
//! the same thing without the waste: the window is drawn when there is
//! something to draw.
//!
//! ## How it is used
//!
//! The producer holds a clone of the handle and calls [`Wake::wake`]
//! after it has put its event on the channel. The interface installs
//! the callback on the first frame ([`Wake::set`]) — the callback is
//! `egui`'s own `request_repaint`, and it may be called from any thread.
//!
//! The handle knows nothing about egui: it is a callback, and the crate
//! that draws the window decides what the callback does. That keeps the
//! core crate free of a GUI dependency, and it keeps this type testable
//! on its own.

use std::sync::{Arc, Mutex};

/// What the handle calls when something has changed.
type Callback = Arc<dyn Fn() + Send + Sync>;

/// A handle the event producers use to wake the interface.
///
/// It is cheap to clone and cheap to call: the wake happens on the
/// thread that produced the event, and it does nothing but ask for a
/// repaint.
#[derive(Clone, Default)]
pub struct Wake {
    callback: Arc<Mutex<Option<Callback>>>,
}

impl Wake {
    /// A handle with no callback yet.
    ///
    /// Waking it does nothing: the interface has not drawn a frame yet,
    /// so there is nothing to repaint — and the first frame drains
    /// everything that arrived in the meantime.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the callback that does the waking.
    ///
    /// The callback is called on the thread that wakes the handle, so
    /// it has to be cheap and it must not block: `egui`'s own
    /// `request_repaint` is exactly that.
    pub fn set(&self, callback: impl Fn() + Send + Sync + 'static) {
        let mut slot = self.callback.lock().expect("the wake handle is poisoned");
        *slot = Some(Arc::new(callback));
    }

    /// Ask for a redraw.
    ///
    /// Does nothing when no callback has been installed — see [`Wake::new`].
    pub fn wake(&self) {
        // The guard is dropped before the callback runs: the callback
        // may take its time, and nothing else has to wait for it.
        let callback = {
            let slot = self.callback.lock().expect("the wake handle is poisoned");
            slot.clone()
        };
        if let Some(callback) = callback {
            callback();
        }
    }

    /// Whether a callback has been installed.
    pub fn is_armed(&self) -> bool {
        let slot = self.callback.lock().expect("the wake handle is poisoned");
        slot.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_wake_before_the_callback_is_installed_does_nothing() {
        let wake = Wake::new();
        assert!(!wake.is_armed());
        // Nothing to panic, and nothing to do.
        wake.wake();
    }

    #[test]
    fn the_callback_is_called() {
        let wake = Wake::new();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        wake.set(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        assert!(wake.is_armed());

        wake.wake();
        wake.wake();
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn every_clone_wakes_the_same_callback() {
        let wake = Wake::new();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        wake.set(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });

        let clone = wake.clone();
        clone.wake();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_callback_can_be_replaced() {
        let wake = Wake::new();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));

        let seen = first.clone();
        wake.set(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        let seen = second.clone();
        wake.set(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });

        wake.wake();
        assert_eq!(first.load(Ordering::SeqCst), 0);
        assert_eq!(second.load(Ordering::SeqCst), 1);
    }
}
