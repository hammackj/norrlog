//! Countdowns that hold steady between snapshots.
//!
//! The daemon says how long is left in whole seconds, worked out at the
//! moment it builds each snapshot, and the window counts down on its own in
//! between. Each push therefore lands up to a second either side of where
//! the bar already was, and the bars jumped back and forth with every one.
//! Here each countdown keeps the moment it ends, and a push moves that only
//! when it disagrees by more than rounding can explain: a recast, or a
//! duration learned since.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// More than whole-second rounding at both ends can account for.
const NEWS: Duration = Duration::from_millis(1500);

/// How long an ended countdown is remembered, in case it shows again.
const FORGET: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Ends {
    ends: RefCell<HashMap<String, Instant>>,
}

impl Ends {
    /// Seconds left on `key`, given `remaining` as a snapshot received at
    /// `received` stated it.
    pub fn left(&self, key: &str, remaining: f64, received: Instant) -> f64 {
        self.left_at(key, remaining, received, Instant::now())
    }

    fn left_at(&self, key: &str, remaining: f64, received: Instant, now: Instant) -> f64 {
        let stated = received + Duration::from_secs_f64(remaining.max(0.));
        let mut ends = self.ends.borrow_mut();
        let end = ends.entry(key.to_string()).or_insert(stated);
        let apart = if *end > stated {
            *end - stated
        } else {
            stated - *end
        };
        if apart > NEWS {
            *end = stated;
        }
        let left = end.saturating_duration_since(now).as_secs_f64();
        ends.retain(|_, e| *e + FORGET > now);
        left
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_does_not_move_a_bar_and_a_recast_does() {
        let e = Ends::default();
        let t0 = Instant::now();
        let s = Duration::from_secs_f64;
        assert_eq!(e.left_at("x", 40., t0, t0), 40.);
        // 2.6 s later a push says 38 (it was 37.4, rounded up): held at 37.4.
        let at = t0 + s(2.6);
        assert!((e.left_at("x", 38., at, at) - 37.4).abs() < 1e-6);
        // And one that says 37 a moment later: still held.
        let at = t0 + s(2.9);
        assert!((e.left_at("x", 37., at, at) - 37.1).abs() < 1e-6);
        // A recast: 47 s again, taken at once.
        let at = t0 + s(5.);
        assert!((e.left_at("x", 47., at, at) - 47.).abs() < 1e-6);
        // Counting on with no push in between.
        assert!((e.left_at("x", 47., at, at + s(10.)) - 37.).abs() < 1e-6);
        // Each key is its own.
        assert_eq!(e.left_at("y", 5., at, at), 5.);
    }
}
