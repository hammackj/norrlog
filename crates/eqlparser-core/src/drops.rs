//! Where items come from: what dropped them, and how often it was seen.
//!
//! # What the log can and cannot say
//!
//! A loot line names the corpse it came from:
//!
//! ```text
//! --You have looted a Small Piece of High Quality Ore from a fire goblin's corpse.--
//! ```
//!
//! That is solid evidence the creature dropped it. What it is *not* is a drop
//! rate. Two things stand in the way, and both are recorded rather than
//! papered over:
//!
//! * You do not loot every corpse. A count of sightings over a count of kills
//!   is therefore a floor on the drop rate, never the rate itself.
//! * Only kills you were part of are counted at all.
//!
//! So this keeps the two numbers side by side — times looted, and kills seen —
//! and computes no rate from them. A consumer that wants one can divide, and
//! must say what it is dividing.
//!
//! # Names
//!
//! Corpse names arrive with the article the client writes: "a fire goblin",
//! "an imp protector". The article is dropped for the canonical name, since
//! "a fire goblin" and "fire goblin" are the same creature and a database that
//! files them separately is worse than useless. Unique NPCs carry no article
//! and are left alone.

use crate::game::Game;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One item seen dropping from one creature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropSighting {
    pub npc: String,
    /// Zones it was seen in, alphabetically. A creature can appear in more
    /// than one, and which zone a drop came from is worth keeping.
    pub zones: Vec<String>,
    /// Times the item was looted from this creature.
    pub looted: u64,
    /// Total quantity looted, which exceeds `looted` for stacked drops.
    pub quantity: u64,
    /// Kills of this creature seen in the same logs.
    ///
    /// The denominator for a drop rate, and the reason none is published
    /// here: corpses you never looted are counted as kills, so any rate
    /// derived from this is a floor.
    pub kills: u64,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// Everything known about where one item comes from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDrops {
    pub item: String,
    /// Which game this was observed in.
    pub game: Game,
    /// Creatures seen dropping it, most often first.
    pub sources: Vec<DropSighting>,
    /// Times looted, across every source.
    pub looted: u64,
    pub quantity: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DropReport {
    pub game: Game,
    pub items: Vec<ItemDrops>,
    /// Creatures killed, and how often — the denominators, so a consumer can
    /// see the base a sighting count sits on even for items never seen.
    pub kills: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Default)]
struct Sighting {
    npc: String,
    zones: std::collections::BTreeSet<String>,
    looted: u64,
    quantity: u64,
    first_seen: i64,
    last_seen: i64,
}

/// Accumulates drop observations.
#[derive(Debug, Default)]
pub struct DropTracker {
    game: Game,
    /// item key -> npc key -> sighting.
    items: HashMap<String, HashMap<String, Sighting>>,
    /// Display name per item key, as the client last wrote it.
    names: HashMap<String, String>,
    /// npc key -> (display name, kills seen).
    kills: HashMap<String, (String, u64)>,
}

/// Strip the article the client puts in front of a non-unique creature.
pub fn canonical_npc(name: &str) -> &str {
    let n = name.trim();
    for prefix in ["a ", "an ", "A ", "An "] {
        if let Some(rest) = n.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    n
}

impl DropTracker {
    pub fn new(game: Game) -> Self {
        DropTracker {
            game,
            ..Default::default()
        }
    }

    pub fn game(&self) -> Game {
        self.game
    }

    /// Record an item looted from a creature's corpse.
    ///
    /// `npc` is the corpse's owner with `'s corpse` already stripped. Loot
    /// with no named source, or from a player's corpse, is not a drop and is
    /// the caller's job to filter out.
    pub fn on_drop(
        &mut self,
        timestamp: i64,
        item: &str,
        quantity: u64,
        npc: &str,
        zone: Option<&str>,
    ) {
        let npc = canonical_npc(npc);
        if item.is_empty() || npc.is_empty() {
            return;
        }
        let item_key = item.to_lowercase();
        self.names.insert(item_key.clone(), item.to_string());

        let e = self
            .items
            .entry(item_key)
            .or_default()
            .entry(npc.to_lowercase())
            .or_insert_with(|| Sighting {
                npc: npc.to_string(),
                first_seen: timestamp,
                ..Default::default()
            });
        e.looted += 1;
        // A stacked drop is one sighting of several items, and both numbers
        // matter: how often it drops, and how much of it.
        e.quantity += quantity.max(1);
        e.last_seen = timestamp;
        if let Some(z) = zone {
            if !z.is_empty() {
                e.zones.insert(z.to_string());
            }
        }
    }

    /// Record a creature killed, whether or not anything was looted from it.
    pub fn on_kill(&mut self, npc: &str) {
        let npc = canonical_npc(npc);
        if npc.is_empty() {
            return;
        }
        let e = self
            .kills
            .entry(npc.to_lowercase())
            .or_insert_with(|| (npc.to_string(), 0));
        e.1 += 1;
    }

    pub fn report(&self) -> DropReport {
        let mut items: Vec<ItemDrops> = self
            .items
            .iter()
            .map(|(key, sources)| {
                let mut sources: Vec<DropSighting> = sources
                    .iter()
                    .map(|(npc_key, s)| DropSighting {
                        npc: s.npc.clone(),
                        zones: s.zones.iter().cloned().collect(),
                        looted: s.looted,
                        quantity: s.quantity,
                        kills: self.kills.get(npc_key).map(|(_, n)| *n).unwrap_or(0),
                        first_seen: s.first_seen,
                        last_seen: s.last_seen,
                    })
                    .collect();
                sources.sort_by(|a, b| b.looted.cmp(&a.looted).then(a.npc.cmp(&b.npc)));
                ItemDrops {
                    item: self.names.get(key).cloned().unwrap_or_else(|| key.clone()),
                    game: self.game,
                    looted: sources.iter().map(|s| s.looted).sum(),
                    quantity: sources.iter().map(|s| s.quantity).sum(),
                    sources,
                }
            })
            .collect();
        items.sort_by(|a, b| b.looted.cmp(&a.looted).then(a.item.cmp(&b.item)));

        let mut kills: Vec<(String, u64)> = self.kills.values().cloned().collect();
        kills.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        DropReport {
            game: self.game,
            items,
            kills,
        }
    }

    /// What one item is known to drop from.
    pub fn sources_for(&self, item: &str) -> Vec<DropSighting> {
        self.report()
            .items
            .into_iter()
            .find(|i| i.item.eq_ignore_ascii_case(item))
            .map(|i| i.sources)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The article is the client's, not the creature's: filing "a fire
    /// goblin" apart from "fire goblin" would split one creature in two.
    #[test]
    fn the_article_is_not_part_of_the_name() {
        assert_eq!(canonical_npc("a fire goblin"), "fire goblin");
        assert_eq!(canonical_npc("an imp protector"), "imp protector");
        assert_eq!(
            canonical_npc("Innoruuk, the Prince of Hate"),
            "Innoruuk, the Prince of Hate",
            "a unique NPC has no article to strip"
        );
        assert_eq!(canonical_npc("a fire goblin"), canonical_npc("fire goblin"));
    }

    #[test]
    fn drops_accumulate_per_creature_with_zones() {
        let mut t = DropTracker::new(Game::EverQuestLegends);
        t.on_drop(100, "Small Piece of High Quality Ore", 1, "a fire goblin", Some("Nagafen's Lair"));
        t.on_drop(200, "Small Piece of High Quality Ore", 2, "fire goblin", Some("Solusek's Eye"));
        t.on_drop(300, "Small Piece of High Quality Ore", 1, "an imp protector", None);

        let r = t.report();
        assert_eq!(r.game, Game::EverQuestLegends);
        let ore = &r.items[0];
        assert_eq!(ore.looted, 3);
        assert_eq!(ore.quantity, 4, "one drop was a stack of two");
        assert_eq!(ore.sources.len(), 2, "the article does not make a second creature");
        assert_eq!(ore.sources[0].npc, "fire goblin");
        assert_eq!(ore.sources[0].looted, 2);
        assert_eq!(
            ore.sources[0].zones,
            vec!["Nagafen's Lair", "Solusek's Eye"],
            "both zones kept"
        );
        assert_eq!((ore.sources[0].first_seen, ore.sources[0].last_seen), (100, 200));
    }

    /// Kills are the denominator, and are counted whether or not the corpse
    /// was looted — which is exactly why the pair is a floor on the drop rate
    /// rather than the rate.
    #[test]
    fn kills_are_counted_alongside_sightings() {
        let mut t = DropTracker::new(Game::EverQuest);
        for _ in 0..10 {
            t.on_kill("a fire goblin");
        }
        t.on_drop(100, "Fire Emerald", 1, "a fire goblin", None);

        let r = t.report();
        let s = &r.items[0].sources[0];
        assert_eq!((s.looted, s.kills), (1, 10));
        assert_eq!(r.kills, vec![("fire goblin".to_string(), 10)]);
    }

    /// A creature killed but never seen dropping anything still belongs in the
    /// record: it is the denominator for everything it did not drop.
    #[test]
    fn a_creature_that_dropped_nothing_is_still_recorded() {
        let mut t = DropTracker::new(Game::Unknown);
        t.on_kill("a rat");
        let r = t.report();
        assert!(r.items.is_empty());
        assert_eq!(r.kills, vec![("rat".to_string(), 1)]);
    }
}
