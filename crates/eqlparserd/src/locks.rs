//! Locks that survive a panic.
//!
//! # The problem with `.lock().unwrap()`
//!
//! A `std` mutex is *poisoned* if a thread panics while holding it, and every
//! later `lock()` returns `Err`. Unwrapping that turns one panic into a
//! permanent outage: the follower thread holds the engine lock across
//! `push_entry`, so a single bad line that panics there would poison the lock,
//! and from then on every HTTP handler would panic on its own unwrap. The
//! daemon keeps running and keeps accepting connections — it just answers
//! everything with a 500, forever, until somebody notices and restarts it.
//!
//! That is the worst of the available failures. The parser is an accumulator
//! of observations, not a ledger: a panic part-way through one log line may
//! leave a counter short, which is a wrong number in one panel. Refusing to
//! serve anything ever again is not a safer answer to that, it is a louder
//! one aimed at the wrong person.
//!
//! # So poisoning is recovered from, and said out loud
//!
//! These helpers take the guard anyway. Ignoring it silently would be its own
//! kind of lie, so the first recovery prints a line naming what happened: the
//! panic itself has already been printed above it, and this says the process
//! carried on rather than that nothing was wrong.
//!
//! Warned once, not once per call — a poisoned lock stays poisoned, and a
//! handler that ran every second would otherwise fill the log with a
//! restatement of the same fact.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

static WARNED: AtomicBool = AtomicBool::new(false);

fn warn(what: &str) {
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "[eqlparserd] a thread panicked while holding the {what} lock; \
             carrying on with the state it left behind. Figures derived from \
             the moment of the panic may be short. The panic itself is logged \
             above."
        );
    }
}

/// A mutex that hands over its contents even after a holder has panicked.
///
/// An extension trait rather than free functions so a call site keeps the
/// shape it had — `s.engine.locked("engine").sky_report()` — and so the `Arc`
/// wrapping every one of these derefs on its own.
pub trait Guarded<T> {
    /// `what` names the lock, for the one line printed if it was poisoned.
    fn locked(&self, what: &str) -> MutexGuard<'_, T>;
}

impl<T> Guarded<T> for Mutex<T> {
    fn locked(&self, what: &str) -> MutexGuard<'_, T> {
        match self.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                warn(what);
                poisoned.into_inner()
            }
        }
    }
}

/// The same, for the snapshot's reader-writer lock.
pub trait Shared<T> {
    fn reading(&self, what: &str) -> RwLockReadGuard<'_, T>;
    fn writing(&self, what: &str) -> RwLockWriteGuard<'_, T>;
}

impl<T> Shared<T> for RwLock<T> {
    fn reading(&self, what: &str) -> RwLockReadGuard<'_, T> {
        match self.read() {
            Ok(g) => g,
            Err(poisoned) => {
                warn(what);
                poisoned.into_inner()
            }
        }
    }

    fn writing(&self, what: &str) -> RwLockWriteGuard<'_, T> {
        match self.write() {
            Ok(g) => g,
            Err(poisoned) => {
                warn(what);
                poisoned.into_inner()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// The whole point: a panic under the lock must not take the next caller
    /// with it.
    #[test]
    fn a_panic_under_the_lock_does_not_break_the_next_caller() {
        let m = Arc::new(Mutex::new(vec![1u32, 2, 3]));
        let m2 = m.clone();
        let died = std::thread::spawn(move || {
            let mut g = m2.locked("test");
            g.push(4);
            panic!("something went wrong mid-update");
        })
        .join();
        assert!(died.is_err(), "the thread really did panic");
        assert!(m.lock().is_err(), "and really did poison the mutex");

        // The state it left behind is visible, and usable.
        let g = m.locked("test");
        assert_eq!(*g, vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_poisoned_rwlock_is_recovered_in_both_directions() {
        let l = Arc::new(RwLock::new(0u32));
        let l2 = l.clone();
        let _ = std::thread::spawn(move || {
            let mut g = l2.writing("test");
            *g = 7;
            panic!("mid-write");
        })
        .join();
        assert!(l.read().is_err(), "poisoned");
        assert_eq!(*l.reading("test"), 7);
        *l.writing("test") = 8;
        assert_eq!(*l.reading("test"), 8);
    }
}
