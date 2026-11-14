//! Your spells running on somebody else: a heal over time on the tank, a
//! dot or a slow on a mob, one row per target and spell.
//!
//! Three things in the log speak to one of these, and none is enough alone:
//!
//! - **The landing**, "Valkyrie Warden is covered in the breath of Trushar."
//!   It names the target but not the spell; the caller pairs it with your
//!   recent cast (see the engine's `resolve_span`) and passes the spell and
//!   the spell file's duration. That is where a countdown comes from.
//! - **The ticks**, "You healed Valkyrie Warden over time for 644 hit points
//!   by Breath of Trushar." Every six seconds, naming both. They say it is
//!   still running, which the spell file's duration cannot: focus effects and
//!   AAs stretch durations by amounts the log never states. A span that
//!   outlives its expected end while still ticking is shown as ticking, not
//!   as expired. A tick for a span whose landing was missed (the log was
//!   joined mid-way) opens one with no countdown at all, rather than a
//!   made-up one.
//! - **The end**, "Your Breath of Trushar spell has worn off of Valkyrie
//!   Warden." Decisive. So is the target dying.
//!
//! The spell file's duration is a starting point only. Live EverQuest
//! stretches durations by focus effects and AAs the log never names, so
//! Breath of Trushar, 24 s in the file, runs about 46 s for a shaman who has
//! them. Every landing that is seen to wear off says how long that spell
//! really lasts for you, and the next landing counts down from that.
//!
//! Without an end line, a span is dropped once its expected end and its last
//! tick are both [`GRACE`] behind: a target that walked out of range, or a
//! wear-off line the client never wrote.

use serde::{Deserialize, Serialize};

/// How long past both its expected end and its last tick a span is kept
/// without an end line. Three missed ticks.
pub const GRACE: i64 = 18;

/// Spans kept at most, oldest dropped first. A raid healer's rotation is a
/// dozen; this is a bound, not a target.
const MAX: usize = 40;

#[derive(Debug, Clone)]
struct Span {
    target: String,
    spell: String,
    category: String,
    /// When it landed, and when the spell file says it ends. `None` for a
    /// span known only from its ticks.
    landed: Option<(i64, i64)>,
    last_tick: Option<i64>,
}

impl Span {
    fn is(&self, target: &str, spell: &str) -> bool {
        self.target.eq_ignore_ascii_case(target) && crate::spell::same_spell(&self.spell, spell)
    }

    /// The latest moment anything said it was running, or would be.
    fn alive_until(&self) -> i64 {
        let end = self.landed.map_or(i64::MIN, |(_, until)| until);
        end.max(self.last_tick.unwrap_or(i64::MIN))
    }
}

/// One row, as the overlay and the dashboard show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnOther {
    pub target: String,
    pub spell: String,
    /// "heal", "damage", "slow", ... from the spell file, for colouring.
    pub category: String,
    /// Seconds left by the spell file's duration, never below zero. `None`
    /// when its landing was not seen.
    pub remaining: Option<i64>,
    /// The spell file's whole duration, for a bar. `None` with `remaining`.
    pub total: Option<i64>,
    /// Still ticking past its expected end: the duration was stretched by
    /// something the log does not state.
    pub overrun: bool,
}

#[derive(Debug, Default, Clone)]
pub struct OnOthers {
    spans: Vec<Span>,
    /// How long each spell (by base name, lowercased) last ran from landing
    /// to wearing off.
    learned: std::collections::HashMap<String, i64>,
}

fn key(spell: &str) -> String {
    crate::spell::base_spell_name(spell).to_lowercase()
}

impl OnOthers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Your `spell` landed on `target` at `at`; the spell file expects it to
    /// run to `until`, and what it was seen to last wins over that. A recast
    /// refreshes the row rather than adding a second.
    pub fn landed(&mut self, at: i64, until: i64, spell: &str, target: &str, category: &str) {
        let until = self.learned.get(&key(spell)).map_or(until, |d| at + d);
        if until <= at {
            return;
        }
        self.prune(at);
        match self.spans.iter_mut().find(|s| s.is(target, spell)) {
            Some(s) => {
                s.landed = Some((at, until));
                s.last_tick = None;
                s.category = category.to_string();
            }
            None => {
                if self.spans.len() >= MAX {
                    self.spans.remove(0);
                }
                self.spans.push(Span {
                    target: target.to_string(),
                    spell: spell.to_string(),
                    category: category.to_string(),
                    landed: Some((at, until)),
                    last_tick: None,
                });
            }
        }
    }

    /// A tick of your `spell` on `target`: a heal over time or a dot.
    pub fn ticked(&mut self, at: i64, spell: &str, target: &str, category: &str) {
        self.prune(at);
        match self.spans.iter_mut().find(|s| s.is(target, spell)) {
            Some(s) => s.last_tick = Some(at),
            None => {
                if self.spans.len() >= MAX {
                    self.spans.remove(0);
                }
                self.spans.push(Span {
                    target: target.to_string(),
                    spell: spell.to_string(),
                    category: category.to_string(),
                    landed: None,
                    last_tick: Some(at),
                });
            }
        }
    }

    /// "Your `spell` spell has worn off of `target`." Learns how long it
    /// lasted, when its landing was seen.
    pub fn wore_off(&mut self, at: i64, target: &str, spell: &str) {
        if let Some((landed, _)) = self
            .spans
            .iter()
            .find(|s| s.is(target, spell))
            .and_then(|s| s.landed)
        {
            // Seconds-resolution stamps: a landing and its wear-off a few
            // seconds apart is a dispel or a line out of order, not a
            // duration worth learning.
            if at - landed >= 6 {
                self.learned.insert(key(spell), at - landed);
            }
        }
        self.spans.retain(|s| !s.is(target, spell));
    }

    /// Nothing runs on a corpse.
    pub fn died(&mut self, target: &str) {
        self.spans
            .retain(|s| !s.target.eq_ignore_ascii_case(target));
    }

    /// Everything, as at a death of your own or a camp: the log has stopped
    /// saying anything about any of them.
    pub fn clear(&mut self) {
        self.spans.clear();
    }

    /// What is running at `now`, the soonest to end first and those with no
    /// countdown last. What has gone quiet is left out, and dropped on the
    /// next landing or tick.
    pub fn report(&self, now: i64) -> Vec<OnOther> {
        let mut rows: Vec<OnOther> = self
            .spans
            .iter()
            .filter(|s| now <= s.alive_until() + GRACE)
            .map(|s| {
                let (remaining, total, overrun) = match s.landed {
                    Some((at, until)) => (
                        Some((until - now).max(0)),
                        Some(until - at),
                        now > until && s.last_tick.is_some_and(|t| t >= until),
                    ),
                    None => (None, None, false),
                };
                OnOther {
                    target: s.target.clone(),
                    spell: s.spell.clone(),
                    category: s.category.clone(),
                    remaining,
                    total,
                    overrun,
                }
            })
            .collect();
        rows.sort_by_key(|r| (r.remaining.is_none(), r.remaining.unwrap_or(0)));
        rows
    }

    /// Forget what has gone quiet, so the list stays bounded by what is
    /// running rather than by [`MAX`].
    fn prune(&mut self, now: i64) {
        self.spans.retain(|s| now <= s.alive_until() + GRACE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOT: &str = "Breath of Trushar";

    #[test]
    fn each_target_has_its_own_countdown_and_a_recast_refreshes_it() {
        let mut o = OnOthers::new();
        o.landed(100, 148, BOT, "Valkyrie Warden", "heal");
        o.landed(110, 158, BOT, "Garrowx", "heal");
        let r = o.report(120);
        assert_eq!(r.len(), 2);
        assert_eq!(
            (r[0].target.as_str(), r[0].remaining),
            ("Valkyrie Warden", Some(28))
        );
        assert_eq!(
            (r[1].target.as_str(), r[1].remaining),
            ("Garrowx", Some(38))
        );
        assert_eq!(r[0].total, Some(48));
        // Recast on the warden: one row, a fresh 48 s.
        o.landed(130, 178, BOT, "valkyrie warden", "heal");
        let r = o.report(130);
        assert_eq!(r.len(), 2);
        assert_eq!(
            (r[0].target.as_str(), r[0].remaining),
            ("Garrowx", Some(28))
        );
        assert_eq!(r[1].remaining, Some(48));
    }

    #[test]
    fn worn_off_and_death_end_it_and_silence_lets_it_go() {
        let mut o = OnOthers::new();
        o.landed(100, 148, BOT, "Valkyrie Warden", "heal");
        o.landed(100, 136, "Envenomed Breath", "a gnoll", "damage");
        o.landed(100, 148, BOT, "Garrowx", "heal");
        o.wore_off(101, "Valkyrie Warden", BOT);
        o.died("a gnoll");
        assert_eq!(o.report(101).len(), 1);
        // No end line: kept through the grace, then dropped.
        assert_eq!(o.report(148 + GRACE).len(), 1);
        assert!(o.report(149 + GRACE).is_empty());
    }

    #[test]
    fn a_wear_off_teaches_the_real_duration() {
        let mut o = OnOthers::new();
        // The file says 24 s; it wears off after 46.
        o.landed(100, 124, BOT, "Valkyrie Warden", "heal");
        o.wore_off(146, "Valkyrie Warden", BOT);
        o.landed(200, 224, BOT, "Garrowx", "heal");
        let r = o.report(200);
        assert_eq!((r[0].remaining, r[0].total), (Some(46), Some(46)));
        // A dispel moments after landing teaches nothing.
        o.wore_off(202, "Garrowx", BOT);
        o.landed(300, 324, BOT, "Garrowx", "heal");
        assert_eq!(o.report(300)[0].total, Some(46));
    }

    #[test]
    fn ticks_past_the_expected_end_keep_it_and_say_so() {
        let mut o = OnOthers::new();
        o.landed(100, 136, BOT, "Valkyrie Warden", "heal");
        for t in (106..=160).step_by(6) {
            o.ticked(t, BOT, "Valkyrie Warden", "heal");
        }
        let r = o.report(160);
        assert_eq!(r.len(), 1, "still ticking, so still running");
        assert_eq!(r[0].remaining, Some(0));
        assert!(r[0].overrun);
        assert!(!o.report(130).is_empty() && !o.report(130)[0].overrun);
    }

    #[test]
    fn a_tick_without_a_landing_shows_without_a_countdown() {
        let mut o = OnOthers::new();
        o.ticked(100, BOT, "Valkyrie Warden", "heal");
        o.landed(100, 130, "Malo", "a gnoll", "debuff");
        let r = o.report(101);
        assert_eq!(r.len(), 2);
        assert_eq!(
            r[1].target, "Valkyrie Warden",
            "the uncounted one goes last"
        );
        assert_eq!((r[1].remaining, r[1].total), (None, None));
        assert!(o.report(100 + GRACE).len() == 2);
        assert_eq!(o.report(131 + GRACE).len(), 0);
    }
}
