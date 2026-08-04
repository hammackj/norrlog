//! Alternate advancement catalogue, read from `/alt list`.
//!
//! # What `/alt list` gives us
//!
//! The client dumps the abilities available to your class combination, one
//! block per rank:
//!
//! ```text
//! Ability #3815: Destructive Cascade
//! Description: ...increases the damage dealt by your critical damage over time spells by 125%.
//! Cost per Level: 2
//! Ability #3815: Destructive Cascade
//! Description: ...by 150%.
//! Cost per Level: 4
//! ```
//!
//! Repeated IDs are successive ranks, not duplicates. Nothing in the dump
//! states which rank you currently hold — that comes from the purchase lines
//! the client writes when you buy one, which this module is matched against.
//!
//! # The dump is capped
//!
//! Every observed dump stops at exactly [`DUMP_CAP`] entries, and two dumps
//! taken eleven minutes apart were identical. So the catalogue in the log is a
//! *prefix* of the real one, and a ladder can be cut short: on the character
//! this was built against, Destructive Cascade's three ranks match the
//! purchase history exactly, while Steadfast Will shows two ranks despite rank
//! six having been bought.
//!
//! Consequently this module reports **ranks seen**, never "max rank", and
//! flags any ability whose owned rank exceeds what the dump showed. Repeated
//! dumps are merged rather than replaced, so coverage can only improve.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Entries after which every observed `/alt list` dump stops.
///
/// Not a rule of the game we can verify — an observation. It is recorded so
/// that a dump of exactly this length can be reported as probably truncated
/// rather than silently trusted as complete.
pub const DUMP_CAP: usize = 100;

/// One purchasable rank of an ability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AaRank {
    /// 1-based, in the order the dump listed them.
    pub rank: u32,
    pub cost: u32,
    pub description: String,
}

/// An ability as described by `/alt list`, with what we know you own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AaAbility {
    pub id: u32,
    pub name: String,
    pub ranks: Vec<AaRank>,
    /// Highest rank seen bought in the log. Ranks bought before logging began
    /// are invisible, so this is a floor.
    pub owned_rank: u32,
    /// Points spent on it within the log.
    pub spent: u32,
    /// Cost of the ranks listed above `owned_rank`. `None` when the dump did
    /// not show enough of the ladder for the answer to mean anything.
    pub cost_to_finish: Option<u32>,
    /// Owned more ranks than the dump listed — proof the ladder is cut short.
    pub truncated: bool,
}

/// Everything the AA catalogue view shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AaReport {
    pub abilities: Vec<AaAbility>,
    /// Entries in the most recent dump, before merging.
    pub last_dump_entries: usize,
    /// The dump stopped exactly on the cap, so the catalogue is incomplete.
    pub dump_probably_truncated: bool,
    /// Abilities bought that no dump ever described.
    pub undescribed: Vec<String>,
    /// Total cost of every unowned rank we can actually see.
    pub visible_cost_to_finish: u32,
}

/// Accumulates `/alt list` dumps and matches them against purchases.
#[derive(Debug, Default)]
pub struct AaCatalog {
    /// Ability id to (name, ranks).
    known: HashMap<u32, (String, Vec<AaRank>)>,
    /// Ranks accumulated for the dump currently being read.
    pending: HashMap<u32, Vec<AaRank>>,
    /// The entry awaiting its `Cost per Level:` line.
    open: Option<(u32, String)>,
    last_dump_entries: usize,
    in_dump: usize,
}

impl AaCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Note an `Ability #N: Name` line.
    ///
    /// A dump is not delimited in the log — it simply starts, and combat lines
    /// resume after it. So an entry arriving while another is still open ends
    /// the previous one, and the run is closed lazily by [`Self::report`].
    pub fn on_entry(&mut self, id: u32, name: &str) {
        self.open = Some((id, name.to_string()));
        if self.in_dump == 0 {
            // A fresh run: start collecting again rather than doubling the
            // rank counts of the run before it.
            self.pending.clear();
        }
        self.in_dump += 1;
        self.known
            .entry(id)
            .or_insert_with(|| (name.to_string(), Vec::new()));
    }

    /// Note the `Cost per Level: N` line that closes an entry.
    pub fn on_cost(&mut self, cost: u32, description: &str) {
        let Some((id, _)) = self.open.take() else {
            return;
        };
        let ranks = self.pending.entry(id).or_default();
        ranks.push(AaRank {
            rank: ranks.len() as u32 + 1,
            cost,
            description: description.to_string(),
        });
    }

    /// Note any line that is not part of a dump, ending the current run.
    pub fn on_other(&mut self) {
        if self.in_dump == 0 {
            return;
        }
        self.last_dump_entries = self.in_dump;
        self.in_dump = 0;
        self.open = None;
        // Merge: a later dump may show more of a ladder than an earlier one,
        // never fewer, so keep whichever saw further.
        for (id, ranks) in self.pending.drain() {
            if let Some((_, existing)) = self.known.get_mut(&id) {
                if ranks.len() > existing.len() {
                    *existing = ranks;
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }

    /// Cross-reference the catalogue with what has been bought.
    ///
    /// `owned` maps lowercased ability name to (highest rank, points spent).
    pub fn report(&self, owned: &HashMap<String, (u32, u32)>) -> AaReport {
        let mut abilities: Vec<AaAbility> = self
            .known
            .iter()
            .map(|(id, (name, ranks))| {
                let (owned_rank, spent) =
                    owned.get(&name.to_lowercase()).copied().unwrap_or((0, 0));
                let truncated = owned_rank as usize > ranks.len();
                AaAbility {
                    id: *id,
                    name: name.clone(),
                    // Only meaningful when the dump showed at least as far as
                    // we have already climbed.
                    cost_to_finish: (!truncated).then(|| {
                        ranks
                            .iter()
                            .filter(|r| r.rank > owned_rank)
                            .map(|r| r.cost)
                            .sum()
                    }),
                    owned_rank,
                    spent,
                    truncated,
                    ranks: ranks.clone(),
                }
            })
            .collect();

        // Unfinished and cheapest-to-finish first: that is the buy list.
        abilities.sort_by(|a, b| {
            let a_done = a.cost_to_finish == Some(0);
            let b_done = b.cost_to_finish == Some(0);
            a_done
                .cmp(&b_done)
                .then(
                    a.cost_to_finish
                        .unwrap_or(u32::MAX)
                        .cmp(&b.cost_to_finish.unwrap_or(u32::MAX)),
                )
                .then(a.name.cmp(&b.name))
        });

        let described: std::collections::BTreeSet<String> =
            self.known.values().map(|(n, _)| n.to_lowercase()).collect();
        let mut undescribed: Vec<String> = owned
            .iter()
            .filter(|(k, _)| !described.contains(*k))
            .map(|(k, _)| k.clone())
            .collect();
        undescribed.sort();

        AaReport {
            visible_cost_to_finish: abilities.iter().filter_map(|a| a.cost_to_finish).sum(),
            last_dump_entries: self.last_dump_entries,
            dump_probably_truncated: self.last_dump_entries >= DUMP_CAP,
            undescribed,
            abilities,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(c: &mut AaCatalog, entries: &[(u32, &str, u32)]) {
        for (id, name, cost) in entries {
            c.on_entry(*id, name);
            c.on_cost(*cost, "");
        }
        c.on_other();
    }

    fn owned(pairs: &[(&str, u32, u32)]) -> HashMap<String, (u32, u32)> {
        pairs
            .iter()
            .map(|(n, r, s)| (n.to_lowercase(), (*r, *s)))
            .collect()
    }

    /// Repeated ids are successive ranks. This is the whole format.
    #[test]
    fn a_repeated_ability_id_builds_a_rank_ladder() {
        let mut c = AaCatalog::new();
        feed(
            &mut c,
            &[
                (3815, "Destructive Cascade", 2),
                (3815, "Destructive Cascade", 4),
                (3815, "Destructive Cascade", 6),
            ],
        );
        let r = c.report(&owned(&[("Destructive Cascade", 2, 6)]));
        assert_eq!(r.abilities.len(), 1);
        let d = &r.abilities[0];
        assert_eq!(d.ranks.len(), 3);
        assert_eq!(
            d.ranks.iter().map(|x| x.cost).collect::<Vec<_>>(),
            vec![2, 4, 6]
        );
        assert_eq!(d.owned_rank, 2);
        // Only rank 3 is left, at 6 points.
        assert_eq!(d.cost_to_finish, Some(6));
        assert!(!d.truncated);
    }

    /// The dump is a prefix of the real catalogue, so owning more ranks than
    /// it lists is expected and must not be reported as a finished ability.
    #[test]
    fn owning_past_the_listed_ladder_is_flagged_not_reported_as_complete() {
        let mut c = AaCatalog::new();
        feed(
            &mut c,
            &[(434, "Steadfast Will", 6), (434, "Steadfast Will", 3)],
        );
        let r = c.report(&owned(&[("Steadfast Will", 6, 16)]));
        let s = &r.abilities[0];
        assert!(s.truncated, "rank 6 owned but only 2 ranks listed");
        assert_eq!(
            s.cost_to_finish, None,
            "no honest answer from a cut-short ladder"
        );
    }

    /// Two dumps must not double the ladder.
    #[test]
    fn repeating_a_dump_does_not_double_the_ranks() {
        let mut c = AaCatalog::new();
        let d = [(40, "Combat Fury", 2), (40, "Combat Fury", 3)];
        feed(&mut c, &d);
        feed(&mut c, &d);
        assert_eq!(c.report(&owned(&[])).abilities[0].ranks.len(), 2);
    }

    /// A later, fuller dump wins; a shorter one never erases what we saw.
    #[test]
    fn a_longer_ladder_replaces_a_shorter_one_and_not_the_reverse() {
        let mut c = AaCatalog::new();
        feed(&mut c, &[(1, "Innate Eminence", 3)]);
        feed(
            &mut c,
            &[(1, "Innate Eminence", 3), (1, "Innate Eminence", 5)],
        );
        assert_eq!(c.report(&owned(&[])).abilities[0].ranks.len(), 2);
        feed(&mut c, &[(1, "Innate Eminence", 3)]);
        assert_eq!(
            c.report(&owned(&[])).abilities[0].ranks.len(),
            2,
            "a shorter later dump must not lose ranks"
        );
    }

    #[test]
    fn a_full_length_dump_is_reported_as_probably_truncated() {
        let mut c = AaCatalog::new();
        let entries: Vec<(u32, &str, u32)> =
            (0..DUMP_CAP as u32).map(|i| (i, "Filler", 1)).collect();
        feed(&mut c, &entries);
        let r = c.report(&owned(&[]));
        assert_eq!(r.last_dump_entries, DUMP_CAP);
        assert!(r.dump_probably_truncated);
    }

    #[test]
    fn abilities_bought_but_never_described_are_listed() {
        let mut c = AaCatalog::new();
        feed(&mut c, &[(40, "Combat Fury", 2)]);
        let r = c.report(&owned(&[
            ("Combat Fury", 1, 2),
            ("Symphonic Aura: Disabled", 3, 0),
        ]));
        assert_eq!(r.undescribed, vec!["symphonic aura: disabled"]);
    }

    /// The buy list leads with what is cheapest to finish, not with what is
    /// already done.
    #[test]
    fn finished_abilities_sort_below_unfinished_ones() {
        let mut c = AaCatalog::new();
        feed(&mut c, &[(1, "Done", 5), (2, "Cheap", 1), (3, "Dear", 9)]);
        let r = c.report(&owned(&[("Done", 1, 5)]));
        let names: Vec<&str> = r.abilities.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Cheap", "Dear", "Done"]);
    }
}
