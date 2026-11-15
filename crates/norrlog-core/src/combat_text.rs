//! Floating combat text: your hits, the damage you take and the heals either
//! way, one number each, for the overlays to float up and fade.
//!
//! The overlays get it the way they get alerts: the snapshot carries the
//! newest few with a sequence number, and a client shows those past the last
//! one it showed. A snapshot can coalesce several lines, so a burst arrives
//! together rather than being lost, and a client that connects late sees
//! nothing old because it starts from the sequence it first sees.

use crate::event::{Actor, Event};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Which way a number went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombatKind {
    /// Damage you dealt.
    Hit,
    /// Damage you took.
    Hurt,
    /// A heal that landed on you, yours or someone else's.
    Heal,
    /// Your heal on someone else.
    HealOut,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombatText {
    pub seq: u64,
    /// The line's timestamp, in the log's clock.
    pub at: i64,
    pub kind: CombatKind,
    pub amount: u64,
    pub crit: bool,
    /// The spell, or the melee verb.
    #[serde(default)]
    pub ability: Option<String>,
    /// Who was on the other end: the target of a hit or an outgoing heal,
    /// the source of damage taken or a heal received.
    pub other: String,
}

/// The newest numbers, oldest first.
#[derive(Debug, Default)]
pub struct CombatTexts {
    seq: u64,
    recent: VecDeque<CombatText>,
}

/// As many as a snapshot carries: more than a fight puts out between two
/// snapshots, few enough not to weigh on every one.
const KEEP: usize = 40;

impl CombatTexts {
    /// Note the number `event` shows, if it shows one.
    pub fn on_event(&mut self, at: i64, event: &Event, player: &str) {
        let Some((kind, amount, crit, ability, other)) = read(event, player) else {
            return;
        };
        self.seq += 1;
        self.recent.push_back(CombatText {
            seq: self.seq,
            at,
            kind,
            amount,
            crit,
            ability,
            other,
        });
        while self.recent.len() > KEEP {
            self.recent.pop_front();
        }
    }

    pub fn recent(&self) -> Vec<CombatText> {
        self.recent.iter().cloned().collect()
    }

    /// The highest sequence issued, so a client can start from it.
    pub fn seq(&self) -> u64 {
        self.seq
    }
}

type Read = (CombatKind, u64, bool, Option<String>, String);

fn read(event: &Event, player: &str) -> Option<Read> {
    let other = |a: &Actor| a.display(player);
    let (kind, amount, crit, ability, who) = match event {
        Event::Damage {
            source,
            target,
            amount,
            ability,
            flags,
            ..
        } => {
            let kind = if target.is_you() {
                CombatKind::Hurt
            } else if source.is_you() {
                CombatKind::Hit
            } else {
                return None;
            };
            let who = if kind == CombatKind::Hurt {
                other(source)
            } else {
                other(target)
            };
            (kind, *amount, flags.is_crit(), ability.clone(), who)
        }
        Event::Heal {
            source,
            target,
            amount,
            spell,
            flags,
            ..
        } => {
            let kind = if target.is_you() {
                CombatKind::Heal
            } else if source.is_you() {
                CombatKind::HealOut
            } else {
                return None;
            };
            let who = if kind == CombatKind::Heal {
                other(source)
            } else {
                other(target)
            };
            (kind, *amount, flags.is_crit(), spell.clone(), who)
        }
        _ => return None,
    };
    (amount > 0).then_some((kind, amount, crit, ability, who))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Parser;

    fn feed(lines: &[&str]) -> CombatTexts {
        let p = Parser::new("Tharn");
        let mut t = CombatTexts::default();
        for l in lines {
            let e = p.parse_line(l).expect(l);
            t.on_event(e.timestamp, &e.event, "Tharn");
        }
        t
    }

    #[test]
    fn each_direction_is_told_apart() {
        let t = feed(&[
            "[Tue Jul 28 14:00:00 2026] You slash a gnoll for 120 points of damage. (Critical)",
            "[Tue Jul 28 14:00:01 2026] A gnoll hits YOU for 12 points of damage.",
            "[Tue Jul 28 14:00:02 2026] Mirela healed you for 300 hit points by Light Healing.",
            "[Tue Jul 28 14:00:03 2026] You healed Mirela for 250 hit points by Light Healing.",
            "[Tue Jul 28 14:00:04 2026] A gnoll hits Mirela for 40 points of damage.",
            "[Tue Jul 28 14:00:05 2026] You have entered West Commonlands.",
        ]);
        let got: Vec<_> = t
            .recent()
            .into_iter()
            .map(|c| (c.seq, c.kind, c.amount, c.crit, c.other))
            .collect();
        assert_eq!(
            got,
            [
                (1, CombatKind::Hit, 120, true, "a gnoll".to_string()),
                (2, CombatKind::Hurt, 12, false, "A gnoll".to_string()),
                (3, CombatKind::Heal, 300, false, "Mirela".to_string()),
                (4, CombatKind::HealOut, 250, false, "Mirela".to_string()),
            ],
            "someone else's fight and a zone line show nothing"
        );
        assert_eq!(t.seq(), 4);
    }

    #[test]
    fn only_the_newest_are_kept() {
        let line = "[Tue Jul 28 14:00:00 2026] A gnoll hits YOU for 12 points of damage.";
        let t = feed(&vec![line; KEEP + 5]);
        let r = t.recent();
        assert_eq!(r.len(), KEEP);
        assert_eq!(r.last().unwrap().seq, (KEEP + 5) as u64);
    }
}
