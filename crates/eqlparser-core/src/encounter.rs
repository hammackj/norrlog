//! Encounter segmentation and combat aggregation.
//!
//! EverQuest has no notion of a "fight" — the log is a flat stream. Encounters
//! are inferred: combat activity opens one, and a gap with no combat closes it.
//! Zone changes force a close, since you cannot still be fighting something in
//! a zone you have left.
//!
//! Aggregation is deliberately per-[`Actor`] rather than per-player: the log
//! shows mobs hitting each other and pets acting independently, and a DPS meter
//! that silently discarded those would misreport who did what. Classification
//! of friend vs. foe is left to the presentation layer, which has the context
//! to decide; this module records what is observable.

use crate::event::{Actor, DamageKind, Event, LogEntry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Combat gap, in seconds, after which an encounter is considered over.
///
/// EverQuest fights contain natural lulls — medding, re-buffing, running
/// between pulls — so this is long enough to hold a fight together through a
/// pause but short enough to separate consecutive pulls.
pub const DEFAULT_IDLE_TIMEOUT: i64 = 12;

/// How far back a death looks for the blows that caused it.
///
/// Long enough to cover a burst that began before the final hit, short enough
/// that ordinary chip damage from earlier in the fight does not crowd it out.
pub const DEATH_WINDOW: i64 = 15;

/// How many individual blows a death lists.
pub const DEATH_BLOWS_SHOWN: usize = 10;

/// Per-ability damage breakdown.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AbilityStats {
    pub total: u64,
    pub hits: u64,
    pub misses: u64,
    pub crits: u64,
    pub min: u64,
    pub max: u64,
}

impl AbilityStats {
    fn record_hit(&mut self, amount: u64, crit: bool) {
        self.total += amount;
        self.hits += 1;
        if crit {
            self.crits += 1;
        }
        if self.min == 0 || amount < self.min {
            self.min = amount;
        }
        if amount > self.max {
            self.max = amount;
        }
    }

    pub fn average(&self) -> f64 {
        if self.hits == 0 {
            0.0
        } else {
            self.total as f64 / self.hits as f64
        }
    }

    /// Share of swings that landed, counting only attempts we observed.
    pub fn accuracy(&self) -> f64 {
        let attempts = self.hits + self.misses;
        if attempts == 0 {
            0.0
        } else {
            self.hits as f64 / attempts as f64
        }
    }
}

/// Everything one actor did, and had done to it, within an encounter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActorStats {
    pub name: String,
    pub damage_done: u64,
    pub damage_taken: u64,
    pub healing_done: u64,
    pub healing_received: u64,
    pub hits: u64,
    pub misses: u64,
    pub crits: u64,
    /// Attacks against this actor that it actively avoided.
    pub avoided: u64,
    pub deaths: u64,
    pub killing_blows: u64,
    /// Damage split by delivery mechanism.
    pub damage_by_kind: HashMap<String, u64>,
    /// Damage *received*, split the same three ways as damage dealt.
    ///
    /// Outgoing figures on the attacker cannot answer "what is hitting me":
    /// in a multi-target fight a mob's output is spread across several
    /// victims, so incoming has to be accumulated on the receiving actor.
    pub damage_taken_by_kind: HashMap<String, u64>,
    pub damage_taken_by_ability: HashMap<String, AbilityStats>,
    pub damage_taken_by_source: HashMap<String, u64>,
    /// Attacks that missed this actor outright.
    pub attacks_missed: u64,
    /// Hits and crits per delivery mechanism.
    ///
    /// A single combined crit rate is misleading: damage shields never crit at
    /// all, and dividing melee crits by every hit including damage-shield procs
    /// and dot ticks understates the real rate substantially.
    pub hits_by_kind: HashMap<String, u64>,
    pub crits_by_kind: HashMap<String, u64>,
    /// Damage split by named ability or spell.
    pub by_ability: HashMap<String, AbilityStats>,
    /// Healing split by spell, so a healer's output can be broken down the same
    /// way damage is. Heals deliberately do not share `by_ability`: mixing them
    /// would corrupt damage totals and per-ability shares.
    pub healing_by_spell: HashMap<String, AbilityStats>,
    /// True if the logging character damaged this actor — strong evidence it
    /// is an enemy.
    pub damaged_by_you: bool,
    /// True if the logging character healed this actor — strong evidence it is
    /// an ally.
    pub healed_by_you: bool,
    /// True if this actor was under your control at any point in the fight.
    ///
    /// Charmed creatures are enemies before and after the charm, so this flag
    /// coexists with `damaged_by_you`; the UI should treat charm as the more
    /// specific classification while it holds.
    pub charmed_by_you: bool,
    /// Seconds this creature spent charmed by you during the fight.
    ///
    /// Charm on a bard is a rhythm rather than a state — it breaks and is
    /// resung constantly — so the useful figure is how much of the fight it
    /// actually held, not whether it was ever charmed at all.
    #[serde(default)]
    pub charmed_seconds: i64,
    /// How many times the charm broke.
    #[serde(default)]
    pub charm_breaks: u32,
    /// Damage this actor dealt while charmed by you. Kept apart from
    /// `damage_done` so a charmed mob's contribution can be credited to you
    /// without double counting it in the encounter total.
    pub damage_while_charmed: u64,
}

impl ActorStats {
    /// Share of landed attacks that critically hit, across all damage kinds.
    ///
    /// Prefer [`Self::crit_rate_for`] where the mechanism matters: this figure
    /// mixes together things with very different crit behaviour.
    pub fn crit_rate(&self) -> f64 {
        if self.hits == 0 {
            0.0
        } else {
            self.crits as f64 / self.hits as f64
        }
    }

    /// Crit rate for one delivery mechanism: "melee", "spell", "dot",
    /// "damage shield".
    pub fn crit_rate_for(&self, kind: &str) -> f64 {
        let hits = self.hits_by_kind.get(kind).copied().unwrap_or(0);
        if hits == 0 {
            return 0.0;
        }
        self.crits_by_kind.get(kind).copied().unwrap_or(0) as f64 / hits as f64
    }

    /// Hits, crits and rate per mechanism, ordered for display.
    pub fn crit_breakdown(&self) -> Vec<(&'static str, u64, u64, f64)> {
        ["melee", "spell", "dot", "damage shield"]
            .into_iter()
            .filter_map(|k| {
                let hits = self.hits_by_kind.get(k).copied().unwrap_or(0);
                if hits == 0 {
                    return None;
                }
                let crits = self.crits_by_kind.get(k).copied().unwrap_or(0);
                Some((k, hits, crits, crits as f64 / hits as f64))
            })
            .collect()
    }

    pub fn accuracy(&self) -> f64 {
        let attempts = self.hits + self.misses;
        if attempts == 0 {
            0.0
        } else {
            self.hits as f64 / attempts as f64
        }
    }

    /// Abilities ordered by damage contributed, highest first.
    pub fn abilities_by_damage(&self) -> Vec<(&str, &AbilityStats)> {
        let mut v: Vec<(&str, &AbilityStats)> = self
            .by_ability
            .iter()
            .map(|(k, s)| (k.as_str(), s))
            .collect();
        v.sort_by_key(|(_, s)| std::cmp::Reverse(s.total));
        v
    }

    /// What hit this actor hardest, by ability.
    pub fn incoming_by_ability(&self) -> Vec<(&str, &AbilityStats)> {
        let mut v: Vec<(&str, &AbilityStats)> = self
            .damage_taken_by_ability
            .iter()
            .map(|(k, s)| (k.as_str(), s))
            .collect();
        v.sort_by_key(|(_, s)| std::cmp::Reverse(s.total));
        v
    }

    /// Who hit this actor hardest.
    pub fn incoming_by_source(&self) -> Vec<(&str, u64)> {
        let mut v: Vec<(&str, u64)> = self
            .damage_taken_by_source
            .iter()
            .map(|(k, n)| (k.as_str(), *n))
            .collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        v
    }

    /// Share of attacks against this actor that failed to land.
    pub fn avoidance_rate(&self) -> f64 {
        let swings = self.damage_taken_hits() + self.attacks_missed + self.avoided;
        if swings == 0 {
            return 0.0;
        }
        (self.attacks_missed + self.avoided) as f64 / swings as f64
    }

    /// Number of damaging hits received.
    pub fn damage_taken_hits(&self) -> u64 {
        self.damage_taken_by_ability.values().map(|a| a.hits).sum()
    }

    /// Heal spells ordered by amount healed, highest first.
    pub fn spells_by_healing(&self) -> Vec<(&str, &AbilityStats)> {
        let mut v: Vec<(&str, &AbilityStats)> = self
            .healing_by_spell
            .iter()
            .map(|(k, s)| (k.as_str(), s))
            .collect();
        v.sort_by_key(|(_, s)| std::cmp::Reverse(s.total));
        v
    }

    /// Whether this actor did anything worth showing in a meter.
    ///
    /// Damage alone is too narrow a test: a dedicated healer, or a tank being
    /// beaten on, would be filtered out of every view despite being central to
    /// the fight.
    pub fn is_active(&self) -> bool {
        self.damage_done > 0 || self.healing_done > 0 || self.damage_taken > 0
    }
}

/// A death that occurred during an encounter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeathRecord {
    pub timestamp: i64,
    pub victim: String,
    pub killer: Option<String>,
    /// What landed on you in the seconds before you died, newest first.
    ///
    /// The client names a killer but never says what actually did it, and the
    /// killing blow is rarely the interesting part — a death is usually the
    /// last few seconds, not the last hit. Only recorded for your own deaths.
    #[serde(default)]
    pub final_blows: Vec<IncomingHit>,
    /// Total damage you took in [`DEATH_WINDOW`] seconds before dying.
    #[serde(default)]
    pub damage_in_window: u64,
}

/// One hit that landed on you.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingHit {
    pub at: i64,
    pub source: String,
    pub ability: String,
    pub amount: u64,
}

/// An item looted during an encounter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LootRecord {
    pub timestamp: i64,
    pub item: String,
    pub quantity: u64,
    pub source: Option<String>,
    /// Sale proceeds in copper, when the item was auto-sold.
    pub sold_for_copper: Option<u64>,
    /// What became of it. Kept separately from `sold_for_copper` because a
    /// sale for nothing ("for free") and a plain keep are both zero-coin
    /// events but very different outcomes.
    #[serde(default)]
    pub fate: LootFate,
    /// Container it was routed into, or the item it was combined into.
    #[serde(default)]
    pub destination: Option<String>,
}

/// What happened to an item the moment it was looted.
///
/// A flattened form of [`crate::LootDisposition`] without the payload, so it
/// can be counted and displayed. The payload lives in
/// [`LootRecord::destination`] and [`LootRecord::sold_for_copper`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LootFate {
    /// Went into your inventory.
    #[default]
    Kept,
    /// Auto-sold on pickup.
    Sold,
    /// Auto-routed into a container.
    Stored,
    /// Combined into another item on pickup.
    Transmuted,
}

impl LootFate {
    /// Flatten a disposition into (fate, destination, sale proceeds).
    ///
    /// Shared by the encounter tracker and the daemon so the two can never
    /// disagree about what happened to an item.
    pub fn split(d: &crate::event::LootDisposition) -> (LootFate, Option<String>, Option<u64>) {
        use crate::event::LootDisposition as D;
        match d {
            D::Kept => (LootFate::Kept, None, None),
            D::Sold(c) => (LootFate::Sold, None, Some(c.as_copper())),
            D::Stored { destination } => (LootFate::Stored, Some(destination.clone()), None),
            D::Transmuted { into } => (LootFate::Transmuted, Some(into.clone()), None),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LootFate::Kept => "kept",
            LootFate::Sold => "sold",
            LootFate::Stored => "stored",
            LootFate::Transmuted => "transmuted",
        }
    }
}

/// A single inferred fight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Encounter {
    pub id: u64,
    pub zone: Option<String>,
    pub start: i64,
    /// Timestamp of the last combat activity, not of the closing gap.
    pub end: i64,
    /// Stats keyed by [`Actor::key`].
    pub actors: HashMap<String, ActorStats>,
    pub deaths: Vec<DeathRecord>,
    pub loot: Vec<LootRecord>,
    pub coin_copper: u64,
}

impl Encounter {
    /// Wall-clock length. Floored at one second so a burst fight that starts
    /// and ends within the same log second cannot divide by zero.
    pub fn duration(&self) -> i64 {
        (self.end - self.start).max(1)
    }

    pub fn stats(&self, actor: &Actor) -> Option<&ActorStats> {
        self.actors.get(&actor.key())
    }

    /// Damage per second for one actor.
    pub fn dps(&self, actor_key: &str) -> f64 {
        self.actors
            .get(actor_key)
            .map(|s| s.damage_done as f64 / self.duration() as f64)
            .unwrap_or(0.0)
    }

    /// Healing per second for one actor.
    pub fn hps(&self, actor_key: &str) -> f64 {
        self.actors
            .get(actor_key)
            .map(|s| s.healing_done as f64 / self.duration() as f64)
            .unwrap_or(0.0)
    }

    /// Every point of damage dealt in the fight, by both sides.
    ///
    /// This includes enemies hitting you, so it is **not** "damage done to the
    /// boss" — see [`Self::damage_to_enemies`] for that. Kept because it is the
    /// sum of the meter, but it is a poor headline figure on its own.
    pub fn total_damage(&self) -> u64 {
        self.actors.values().map(|s| s.damage_done).sum()
    }

    /// Damage absorbed by things you fought.
    ///
    /// This is the figure a damage meter is usually asked for: how much your
    /// side put into the enemy. Measured from what enemies *took* rather than
    /// what allies *dealt*, so damage from an unidentified ally still counts.
    pub fn damage_to_enemies(&self) -> u64 {
        self.actors
            .values()
            .filter(|s| s.damaged_by_you)
            .map(|s| s.damage_taken)
            .sum()
    }

    /// Damage absorbed by you and anything fighting alongside you.
    pub fn damage_to_allies(&self) -> u64 {
        self.actors
            .values()
            .filter(|s| !s.damaged_by_you)
            .map(|s| s.damage_taken)
            .sum()
    }

    /// Your damage plus everything your charmed pets dealt while under your
    /// control.
    ///
    /// Charm is a damage cooldown for the classes that have it, so judging a
    /// bard on personal damage alone understates the pull considerably. Both
    /// figures are exposed rather than one replacing the other.
    pub fn your_damage_with_pets(&self) -> u64 {
        let own = self.actors.get("you").map(|s| s.damage_done).unwrap_or(0);
        let pets: u64 = self
            .actors
            .values()
            .filter(|s| s.charmed_by_you)
            .map(|s| s.damage_while_charmed)
            .sum();
        own + pets
    }

    /// Actors that were under your control during this fight.
    /// Share of the fight your charms held, 0.0–1.0.
    ///
    /// Summed across pets and capped at 1.0: charming two creatures at once is
    /// possible but does not make a fight more than fully charmed.
    pub fn charm_uptime(&self) -> f64 {
        let held: i64 = self
            .actors
            .values()
            .filter(|s| s.charmed_by_you)
            .map(|s| s.charmed_seconds)
            .sum();
        (held as f64 / self.duration() as f64).min(1.0)
    }

    /// Total times a charm broke during the fight.
    pub fn charm_breaks(&self) -> u32 {
        self.actors.values().map(|s| s.charm_breaks).sum()
    }

    pub fn charmed_pets(&self) -> Vec<&ActorStats> {
        let mut v: Vec<&ActorStats> = self
            .actors
            .values()
            .filter(|s| s.charmed_by_you && s.damage_while_charmed > 0)
            .collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.damage_while_charmed));
        v
    }

    /// Actors ranked by damage done, highest first. This is the DPS meter.
    pub fn damage_ranking(&self) -> Vec<(&str, &ActorStats)> {
        let mut v: Vec<(&str, &ActorStats)> = self
            .actors
            .iter()
            .filter(|(_, s)| s.damage_done > 0)
            .map(|(k, s)| (k.as_str(), s))
            .collect();
        v.sort_by_key(|(_, s)| std::cmp::Reverse(s.damage_done));
        v
    }

    /// Actors ranked by healing done, highest first.
    pub fn healing_ranking(&self) -> Vec<(&str, &ActorStats)> {
        let mut v: Vec<(&str, &ActorStats)> = self
            .actors
            .iter()
            .filter(|(_, s)| s.healing_done > 0)
            .map(|(k, s)| (k.as_str(), s))
            .collect();
        v.sort_by_key(|(_, s)| std::cmp::Reverse(s.healing_done));
        v
    }

    /// Actors ranked by damage absorbed, highest first — who was taking the
    /// beating.
    pub fn damage_taken_ranking(&self) -> Vec<(&str, &ActorStats)> {
        let mut v: Vec<(&str, &ActorStats)> = self
            .actors
            .iter()
            .filter(|(_, s)| s.damage_taken > 0)
            .map(|(k, s)| (k.as_str(), s))
            .collect();
        v.sort_by_key(|(_, s)| std::cmp::Reverse(s.damage_taken));
        v
    }

    /// Every actor that did anything, ranked by damage. Used to build meters
    /// that must not drop pure healers or pure damage sponges.
    pub fn active_actors(&self) -> Vec<(&str, &ActorStats)> {
        let mut v: Vec<(&str, &ActorStats)> = self
            .actors
            .iter()
            .filter(|(_, s)| s.is_active())
            .map(|(k, s)| (k.as_str(), s))
            .collect();
        v.sort_by_key(|(_, s)| std::cmp::Reverse(s.damage_done));
        v
    }

    /// Total damage the logging character absorbed.
    pub fn your_damage_taken(&self) -> u64 {
        self.actors.get("you").map(|s| s.damage_taken).unwrap_or(0)
    }

    /// A human-readable name for the fight.
    ///
    /// Names the encounter after the toughest thing the logging character
    /// killed, measured by damage absorbed, rather than the first thing to die.
    /// A pull that clears six trash mobs and a named boss should read as the
    /// boss fight, and kill order rarely puts the boss first.
    pub fn title(&self) -> String {
        let mut killed: Vec<(&str, u64)> = self
            .deaths
            .iter()
            .filter_map(|d| {
                let s = self.actors.get(&d.victim.to_lowercase())?;
                s.damaged_by_you
                    .then_some((d.victim.as_str(), s.damage_taken))
            })
            .collect();
        // Deduplicate repeated names (several identical trash mobs) so the
        // "+N more" count reflects distinct opponents.
        killed.sort_by_key(|(_, dmg)| std::cmp::Reverse(*dmg));
        killed.dedup_by(|a, b| a.0 == b.0);

        if let Some((name, _)) = killed.first() {
            return match killed.len() {
                1 => name.to_string(),
                n => format!("{name} (+{} more)", n - 1),
            };
        }
        // Nothing died: name it after whatever we hit hardest.
        self.actors
            .values()
            .filter(|s| s.damaged_by_you)
            .max_by_key(|s| s.damage_taken)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Unknown encounter".to_string())
    }

    /// Whether anything worth reporting happened. Used to discard encounters
    /// opened by a stray miss.
    pub fn is_significant(&self) -> bool {
        self.total_damage() > 0
    }

    fn entry(&mut self, actor: &Actor, player: &str) -> &mut ActorStats {
        let key = actor.key();
        self.actors.entry(key).or_insert_with(|| ActorStats {
            name: actor.display(player),
            ..Default::default()
        })
    }
}

/// Streaming encounter segmenter.
///
/// Feed it parsed entries in timestamp order; it yields each encounter as soon
/// as the following activity proves the fight is over.
pub struct EncounterTracker {
    player: String,
    idle_timeout: i64,
    next_id: u64,
    zone: Option<String>,
    current: Option<Encounter>,
    /// Timestamp of the most recent combat activity in the open encounter.
    last_combat: i64,
    /// Actor keys currently charmed by the logging character.
    ///
    /// Lives on the tracker rather than the encounter because a charm can be
    /// established in one pull and still hold when the next one starts.
    charmed: std::collections::HashSet<String>,
    /// When the charm currently held on each creature began.
    charm_since: std::collections::HashMap<String, i64>,
    /// Rolling buffer of damage aimed at you, for death post-mortems.
    recent_incoming: Vec<IncomingHit>,
}

/// How long after a fight closes its corpses may still be looted.
///
/// Looting happens *after* combat, and on a boss the gap can exceed the idle
/// timeout comfortably — a real Innoruuk kill yielded an item 22 seconds after
/// the encounter had closed.
pub const LOOT_GRACE_SECONDS: i64 = 120;

impl EncounterTracker {
    pub fn new(player: impl Into<String>) -> Self {
        EncounterTracker {
            player: player.into(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            next_id: 1,
            zone: None,
            current: None,
            last_combat: 0,
            charmed: std::collections::HashSet::new(),
            charm_since: std::collections::HashMap::new(),
            recent_incoming: Vec::new(),
        }
    }

    /// Actor keys currently under your control.
    pub fn charmed(&self) -> &std::collections::HashSet<String> {
        &self.charmed
    }

    /// Mark or clear a creature as charmed by you.
    ///
    /// Exists because the only in-log proof of a charm is a spell landing, and
    /// deciding *which* spell landed needs the spell database, which lives
    /// outside this crate's reach. The caller resolves it and reports the
    /// verdict here.
    ///
    /// Marking is deliberately sticky on the encounter's actor record: charm
    /// breaks and is recast constantly, and a mob that fought for you at any
    /// point in a fight should read as a pet for the whole of it rather than
    /// flickering between pet and enemy.
    pub fn set_charmed(&mut self, timestamp: i64, actor: &Actor, charmed: bool) {
        let key = actor.key();
        if charmed {
            self.charmed.insert(key.clone());
            // Re-landing a charm that never broke must not restart the clock,
            // or a fight of constant refreshes would report almost no uptime.
            self.charm_since.entry(key).or_insert(timestamp);
            if let Some(enc) = self.current.as_mut() {
                let player = self.player.clone();
                enc.entry(actor, &player).charmed_by_you = true;
            }
        } else {
            self.charmed.remove(&key);
            if let Some(since) = self.charm_since.remove(&key) {
                if let Some(enc) = self.current.as_mut() {
                    let player = self.player.clone();
                    let st = enc.entry(actor, &player);
                    st.charmed_seconds += (timestamp - since).max(0);
                    st.charm_breaks += 1;
                }
            }
        }
    }

    pub fn with_idle_timeout(mut self, seconds: i64) -> Self {
        self.idle_timeout = seconds;
        self
    }

    pub fn current(&self) -> Option<&Encounter> {
        self.current.as_ref()
    }

    pub fn current_zone(&self) -> Option<&str> {
        self.zone.as_deref()
    }

    /// Consume one entry. Returns an encounter if this entry closed one.
    pub fn push(&mut self, entry: &LogEntry) -> Option<Encounter> {
        // Close a stale encounter before doing anything else, so the closing
        // entry is attributed to the next fight rather than the previous one.
        let mut closed = None;
        if self.current.is_some() && entry.timestamp - self.last_combat > self.idle_timeout {
            closed = self.close();
        }

        match &entry.event {
            Event::ZoneChange { zone } => {
                // You cannot still be fighting something in a zone you left.
                let z = self.close();
                self.zone = Some(zone.clone());
                return z.or(closed);
            }
            Event::PetAttack { pet, .. } => {
                // Only something you control calls you Master.
                self.charmed.insert(pet.key());
                if let Some(enc) = self.current.as_mut() {
                    let player = self.player.clone();
                    enc.entry(pet, &player).charmed_by_you = true;
                }
            }
            // Deliberately does *not* end a charm. Which spell wore off decides
            // that, and naming spells needs the database this crate cannot
            // see, so the caller makes the call via `set_charmed`. Clearing on
            // any effect looks safe but is not: a charmed mob is under a pile
            // of your debuffs, and the first of them to lapse would end the
            // charm early. On one Innoruuk fight that cut a 92-second charm to
            // 14 and lost most of the pet's damage.
            Event::EffectWoreOffTarget { .. } => {}
            Event::Damage {
                source,
                target,
                amount,
                kind,
                ability,
                flags,
                ..
            } => {
                self.ensure_open(entry.timestamp);
                let crit = flags.is_crit();
                let player = self.player.clone();
                let src_charmed = self.charmed.contains(&source.key());
                let enc = self.current.as_mut().unwrap();

                let ability_name = ability
                    .clone()
                    .unwrap_or_else(|| kind_label(*kind).to_string());
                let ability_label = ability_name.clone();
                let mut incoming: Option<IncomingHit> = None;
                let src_is_you = source.is_you();
                {
                    let s = enc.entry(source, &player);
                    s.damage_done += *amount;
                    s.hits += 1;
                    if crit {
                        s.crits += 1;
                    }
                    if src_charmed {
                        s.charmed_by_you = true;
                        s.damage_while_charmed += *amount;
                    }
                    let label = kind_label(*kind).to_string();
                    *s.damage_by_kind.entry(label.clone()).or_default() += *amount;
                    *s.hits_by_kind.entry(label.clone()).or_default() += 1;
                    if crit {
                        *s.crits_by_kind.entry(label).or_default() += 1;
                    }
                    s.by_ability
                        .entry(ability_name)
                        .or_default()
                        .record_hit(*amount, crit);
                }
                {
                    let label = kind_label(*kind).to_string();
                    let ability_in = ability
                        .clone()
                        .unwrap_or_else(|| kind_label(*kind).to_string());
                    // Use the canonical actor name, not the raw text: the log
                    // writes "A giant" and "a giant" for the same creature, and
                    // keying on the literal string splits one attacker in two.
                    let source_name = enc
                        .actors
                        .get(&source.key())
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| source.display(&player));
                    let t = enc.entry(target, &player);
                    t.damage_taken += *amount;
                    *t.damage_taken_by_kind.entry(label).or_default() += *amount;
                    t.damage_taken_by_ability
                        .entry(ability_in)
                        .or_default()
                        .record_hit(*amount, crit);
                    *t.damage_taken_by_source
                        .entry(source_name.clone())
                        .or_default() += *amount;
                    if src_is_you {
                        t.damaged_by_you = true;
                    }
                    if target.is_you() {
                        incoming = Some(IncomingHit {
                            at: entry.timestamp,
                            source: source_name,
                            ability: ability_label,
                            amount: *amount,
                        });
                    }
                }
                // Buffered outside the encounter so a death that opens its own
                // encounter still has the run-up available.
                if let Some(hit) = incoming {
                    self.recent_incoming.push(hit);
                    let cutoff = entry.timestamp - DEATH_WINDOW;
                    self.recent_incoming.retain(|h| h.at >= cutoff);
                }
                self.last_combat = entry.timestamp;
                enc.end = entry.timestamp;
            }
            Event::Miss {
                source,
                target,
                ability,
                ..
            } => {
                self.ensure_open(entry.timestamp);
                let player = self.player.clone();
                let enc = self.current.as_mut().unwrap();
                let name = ability.clone().unwrap_or_else(|| "melee".to_string());
                {
                    let s = enc.entry(source, &player);
                    s.misses += 1;
                    s.by_ability.entry(name).or_default().misses += 1;
                }
                enc.entry(target, &player).attacks_missed += 1;
                self.last_combat = entry.timestamp;
                enc.end = entry.timestamp;
            }
            Event::Avoided { source, target, .. } => {
                self.ensure_open(entry.timestamp);
                let player = self.player.clone();
                let enc = self.current.as_mut().unwrap();
                enc.entry(source, &player).misses += 1;
                enc.entry(target, &player).avoided += 1;
                self.last_combat = entry.timestamp;
                enc.end = entry.timestamp;
            }
            Event::Heal {
                source,
                target,
                amount,
                spell,
                over_time,
            } => {
                // Healing alone does not open a fight; out-of-combat healing is
                // constant in EverQuest and would glue everything together.
                if self.current.is_none() {
                    return closed;
                }
                let player = self.player.clone();
                let enc = self.current.as_mut().unwrap();
                let src_is_you = source.is_you();
                // Unnamed heals still need a bucket, or a healer's breakdown
                // silently loses everything the client did not name.
                let spell_name = spell.clone().unwrap_or_else(|| {
                    if *over_time { "heal over time" } else { "heal" }.to_string()
                });
                {
                    let s = enc.entry(source, &player);
                    s.healing_done += *amount;
                    s.healing_by_spell
                        .entry(spell_name)
                        .or_default()
                        .record_hit(*amount, false);
                }
                {
                    let t = enc.entry(target, &player);
                    t.healing_received += *amount;
                    if src_is_you {
                        t.healed_by_you = true;
                    }
                }
            }
            Event::Death { victim, killer } => {
                self.ensure_open(entry.timestamp);
                let player = self.player.clone();
                let enc = self.current.as_mut().unwrap();
                enc.entry(victim, &player).deaths += 1;
                if let Some(k) = killer {
                    enc.entry(k, &player).killing_blows += 1;
                }
                // Only your own deaths get a post-mortem: the buffer only
                // holds damage aimed at you, so attaching it to a mob's death
                // would be nonsense.
                let (final_blows, damage_in_window) = if victim.is_you() {
                    let cutoff = entry.timestamp - DEATH_WINDOW;
                    let mut blows: Vec<IncomingHit> = self
                        .recent_incoming
                        .iter()
                        .filter(|h| h.at >= cutoff)
                        .cloned()
                        .collect();
                    let total = blows.iter().map(|h| h.amount).sum();
                    blows.reverse();
                    blows.truncate(DEATH_BLOWS_SHOWN);
                    (blows, total)
                } else {
                    (Vec::new(), 0)
                };
                enc.deaths.push(DeathRecord {
                    timestamp: entry.timestamp,
                    victim: victim.display(&player),
                    killer: killer.as_ref().map(|k| k.display(&player)),
                    final_blows,
                    damage_in_window,
                });
                self.last_combat = entry.timestamp;
                enc.end = entry.timestamp;
            }
            Event::Loot {
                item,
                quantity,
                source,
                disposition,
            } => {
                // Loot arrives just after a kill, so attribute it to the fight
                // that is still open rather than opening a new one.
                if let Some(enc) = self.current.as_mut() {
                    let (fate, destination, sold) = LootFate::split(disposition);
                    enc.loot.push(LootRecord {
                        timestamp: entry.timestamp,
                        item: item.clone(),
                        quantity: *quantity,
                        source: source.as_ref().map(|s| s.display(&self.player)),
                        sold_for_copper: sold,
                        fate,
                        destination,
                    });
                }
            }
            Event::CoinLoot { coin } => {
                if let Some(enc) = self.current.as_mut() {
                    enc.coin_copper += coin.as_copper();
                }
            }
            _ => {}
        }
        closed
    }

    fn ensure_open(&mut self, timestamp: i64) {
        if self.current.is_none() {
            self.current = Some(Encounter {
                id: self.next_id,
                zone: self.zone.clone(),
                start: timestamp,
                end: timestamp,
                actors: HashMap::new(),
                deaths: Vec::new(),
                loot: Vec::new(),
                coin_copper: 0,
            });
            self.next_id += 1;
            self.last_combat = timestamp;
        }
    }

    /// Close the open encounter, returning it if it was worth keeping.
    pub fn close(&mut self) -> Option<Encounter> {
        // A charm still held when the fight ends counts up to that moment;
        // otherwise the most common case — charming right through to the kill
        // — would report zero uptime.
        let ends_at = self.current.as_ref().map(|e| e.end);
        if let (Some(end), Some(enc)) = (ends_at, self.current.as_mut()) {
            for (key, since) in self.charm_since.iter() {
                if let Some(st) = enc.actors.get_mut(key) {
                    st.charmed_seconds += (end - *since).max(0);
                }
            }
        }
        // Charm survives the fight boundary in the game, but the interval
        // clock restarts with the next encounter.
        for since in self.charm_since.values_mut() {
            *since = ends_at.unwrap_or(*since);
        }

        let enc = self.current.take()?;
        if enc.is_significant() {
            Some(enc)
        } else {
            // An encounter with no damage was opened by a stray miss or death
            // message; reuse its id rather than leaving a gap.
            self.next_id -= 1;
            None
        }
    }

    /// Close any open encounter at end of input.
    pub fn flush(&mut self) -> Option<Encounter> {
        self.close()
    }
}

fn kind_label(k: DamageKind) -> &'static str {
    match k {
        DamageKind::Melee => "melee",
        DamageKind::DirectSpell => "spell",
        DamageKind::DamageOverTime => "dot",
        DamageKind::DamageShield => "damage shield",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    /// Build entries from raw log bodies at given offsets from a base time.
    fn entries(lines: &[(i64, &str)]) -> Vec<LogEntry> {
        let p = Parser::new("Tharn");
        lines
            .iter()
            .map(|(ts, body)| LogEntry {
                timestamp: *ts,
                event: p.parse_body(body),
                raw: body.to_string(),
            })
            .collect()
    }

    fn run(lines: &[(i64, &str)]) -> Vec<Encounter> {
        let mut t = EncounterTracker::new("Tharn");
        let mut out = Vec::new();
        for e in entries(lines) {
            if let Some(enc) = t.push(&e) {
                out.push(enc);
            }
        }
        if let Some(enc) = t.flush() {
            out.push(enc);
        }
        out
    }

    #[test]
    fn single_fight_aggregates_damage_and_dps() {
        let encs = run(&[
            (100, "You slash a lesser mummy for 100 points of damage."),
            (105, "You slash a lesser mummy for 100 points of damage."),
            (110, "You have slain a lesser mummy!"),
        ]);
        assert_eq!(encs.len(), 1);
        let e = &encs[0];
        assert_eq!(e.total_damage(), 200);
        assert_eq!(e.duration(), 10);
        assert_eq!(e.dps("you"), 20.0);
        assert_eq!(e.title(), "a lesser mummy");
    }

    #[test]
    fn idle_gap_splits_encounters() {
        let encs = run(&[
            (100, "You slash a lesser mummy for 50 points of damage."),
            (105, "You have slain a lesser mummy!"),
            // Well past the idle timeout.
            (400, "You slash a giant rat for 70 points of damage."),
            (405, "You have slain a giant rat!"),
        ]);
        assert_eq!(encs.len(), 2, "gap should split the fights");
        assert_eq!(encs[0].total_damage(), 50);
        assert_eq!(encs[1].total_damage(), 70);
    }

    #[test]
    fn short_lull_does_not_split() {
        let encs = run(&[
            (100, "You slash a lesser mummy for 50 points of damage."),
            // Inside the 12s default timeout.
            (108, "You slash a lesser mummy for 50 points of damage."),
            (115, "You have slain a lesser mummy!"),
        ]);
        assert_eq!(encs.len(), 1, "a brief lull should not split a fight");
        assert_eq!(encs[0].total_damage(), 100);
    }

    #[test]
    fn zone_change_closes_encounter() {
        let encs = run(&[
            (100, "You slash a lesser mummy for 50 points of damage."),
            (102, "You have entered West Commonlands."),
            (103, "You slash a giant rat for 10 points of damage."),
        ]);
        assert_eq!(encs.len(), 2);
        assert_eq!(encs[1].zone.as_deref(), Some("West Commonlands"));
    }

    #[test]
    fn damage_split_by_kind_and_ability() {
        let encs = run(&[
            (100, "You slash a lesser mummy for 100 points of damage."),
            (
                101,
                "You hit a lesser mummy for 40 points of magic damage by Shallow Breath.",
            ),
            (
                102,
                "A lesser mummy has taken 30 damage from your Drifting Death IV.",
            ),
            (
                103,
                "A lesser mummy is pierced by YOUR thorns for 10 points of non-melee damage.",
            ),
            (104, "You have slain a lesser mummy!"),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        assert_eq!(you.damage_done, 180);
        assert_eq!(you.damage_by_kind.get("melee"), Some(&100));
        assert_eq!(you.damage_by_kind.get("spell"), Some(&40));
        assert_eq!(you.damage_by_kind.get("dot"), Some(&30));
        assert_eq!(you.damage_by_kind.get("damage shield"), Some(&10));
        assert_eq!(you.by_ability.get("Shallow Breath").unwrap().total, 40);
    }

    /// Crit rate must be reported per mechanism. Damage shields never crit, so
    /// folding them into one denominator drags the melee rate down.
    #[test]
    fn crit_rate_is_split_by_damage_kind() {
        let encs = run(&[
            // Melee: 2 hits, 1 crit => 50%.
            (100, "You slash a mob for 100 points of damage. (Critical)"),
            (101, "You slash a mob for 50 points of damage."),
            // Dot: 2 ticks, 1 crit => 50%.
            (
                102,
                "A mob has taken 30 damage from your Drifting Death IV. (Critical)",
            ),
            (
                103,
                "A mob has taken 20 damage from your Drifting Death IV.",
            ),
            // Damage shield: never crits.
            (
                104,
                "A mob is pierced by YOUR thorns for 10 points of non-melee damage.",
            ),
            (
                105,
                "A mob is pierced by YOUR thorns for 10 points of non-melee damage.",
            ),
            (106, "You have slain a mob!"),
        ]);
        let you = encs[0].actors.get("you").unwrap();

        assert!((you.crit_rate_for("melee") - 0.5).abs() < 1e-9);
        assert!((you.crit_rate_for("dot") - 0.5).abs() < 1e-9);
        assert_eq!(you.crit_rate_for("damage shield"), 0.0);

        // The combined figure is diluted by the uncrittable damage shield.
        assert!(
            you.crit_rate() < 0.4,
            "combined rate ({}) should be lower than either real rate",
            you.crit_rate()
        );

        let bd = you.crit_breakdown();
        assert_eq!(bd.len(), 3, "only mechanisms actually used appear");
        let melee = bd.iter().find(|(k, ..)| *k == "melee").unwrap();
        assert_eq!((melee.1, melee.2), (2, 1));
    }

    #[test]
    fn crits_and_misses_feed_rates() {
        let encs = run(&[
            (
                100,
                "You slash a lesser mummy for 100 points of damage. (Critical)",
            ),
            (101, "You slash a lesser mummy for 50 points of damage."),
            (102, "You try to slash a lesser mummy, but miss!"),
            (103, "You have slain a lesser mummy!"),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        assert_eq!(you.hits, 2);
        assert_eq!(you.misses, 1);
        assert_eq!(you.crits, 1);
        assert!((you.crit_rate() - 0.5).abs() < 1e-9);
        assert!((you.accuracy() - 2.0 / 3.0).abs() < 1e-9);
    }

    /// "Total damage" counts both sides, which is not what a damage meter is
    /// usually asked for. The two must be reported distinctly.
    #[test]
    fn damage_to_enemies_excludes_what_the_enemy_dealt_to_you() {
        let encs = run(&[
            (100, "You slash Cazic-Thule for 300 points of damage."),
            (101, "Cazic-Thule hits YOU for 200 points of damage."),
            (102, "You have slain Cazic-Thule!"),
        ]);
        let e = &encs[0];
        assert_eq!(e.total_damage(), 500, "both sides combined");
        assert_eq!(e.damage_to_enemies(), 300, "only what the enemy absorbed");
        assert_eq!(e.damage_to_allies(), 200);
        assert_eq!(e.your_damage_taken(), 200);
    }

    /// A charmed pet fights for you, so what it deals counts toward the enemy,
    /// and what it absorbs does not count as damage to your side.
    #[test]
    fn charmed_pet_damage_counts_toward_the_enemy() {
        let encs = run(&[
            (100, "You slash a fire giant for 100 points of damage."),
            (
                101,
                "An imp protector told you, 'Attacking a fire giant Master.'",
            ),
            (
                102,
                "An imp protector slashes a fire giant for 400 points of damage.",
            ),
            (103, "A fire giant hits YOU for 50 points of damage."),
        ]);
        let e = &encs[0];
        assert_eq!(
            e.damage_to_enemies(),
            500,
            "your 100 plus the charmed pet's 400"
        );
        assert_eq!(e.damage_to_allies(), 50);
    }

    /// "What is hitting me" has to be accumulated on the receiving actor: an
    /// attacker's outgoing totals are spread across everything it fought.
    #[test]
    fn incoming_damage_is_broken_down_on_the_receiver() {
        let encs = run(&[
            (100, "You slash a giant for 10 points of damage."),
            (101, "A giant hits YOU for 200 points of damage."),
            (102, "A giant hits YOU for 100 points of damage."),
            (
                103,
                "a giant hit you for 60 points of fire damage by Draught of Fire.",
            ),
            (
                104,
                "You have taken 20 damage from Choking by a thunder spirit.",
            ),
            (105, "A thunder spirit hits YOU for 30 points of damage."),
            (106, "A giant tries to hit YOU, but misses!"),
        ]);
        let you = encs[0].actors.get("you").unwrap();

        assert_eq!(you.damage_taken, 410);
        assert_eq!(you.damage_taken_by_kind.get("melee"), Some(&330));
        assert_eq!(you.damage_taken_by_kind.get("spell"), Some(&60));
        assert_eq!(you.damage_taken_by_kind.get("dot"), Some(&20));

        // Which attack hurt most.
        let worst = you.incoming_by_ability();
        assert_eq!(worst[0].0, "hits");
        assert_eq!(worst[0].1.total, 330);
        assert_eq!(worst[0].1.max, 200, "biggest single hit");

        // Who hurt most.
        // The canonical name is whichever casing was seen first.
        let by_src = you.incoming_by_source();
        assert_eq!(by_src[0].0, "a giant");
        assert_eq!(by_src[0].1, 360);
        assert_eq!(by_src[1].1, 50);

        assert_eq!(you.attacks_missed, 1);
        assert!(you.avoidance_rate() > 0.0);
    }

    /// The log writes "A giant" and "a giant" for one creature; the incoming
    /// source tally must not split it into two attackers.
    #[test]
    fn incoming_source_folds_name_casing() {
        let encs = run(&[
            (100, "You slash a giant for 10 points of damage."),
            (101, "A giant hits YOU for 100 points of damage."),
            (
                102,
                "a giant hit you for 50 points of fire damage by Draught of Fire.",
            ),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        let by_src = you.incoming_by_source();
        assert_eq!(by_src.len(), 1, "one attacker, not two: {by_src:?}");
        assert_eq!(by_src[0].1, 150);
    }

    /// A mob splitting its attention must not have all its output counted as
    /// incoming to one victim.
    #[test]
    fn incoming_excludes_damage_dealt_to_others() {
        let encs = run(&[
            (100, "You slash a giant for 10 points of damage."),
            (101, "A giant hits YOU for 100 points of damage."),
            (102, "A giant hits Reklar for 900 points of damage."),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        assert_eq!(you.damage_taken, 100, "only what hit you");
        assert_eq!(you.damage_taken_by_source.get("a giant"), Some(&100));
        // The attacker's own outgoing total is the full 1000.
        assert_eq!(encs[0].actors.get("a giant").unwrap().damage_done, 1000);
    }

    #[test]
    fn damage_taken_is_attributed_to_the_target() {
        let encs = run(&[
            (100, "A lesser mummy hits YOU for 30 points of damage."),
            (101, "You slash a lesser mummy for 10 points of damage."),
            (102, "You have slain a lesser mummy!"),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        assert_eq!(you.damage_taken, 30);
        assert_eq!(you.damage_done, 10);
        let mummy = encs[0].actors.get("a lesser mummy").unwrap();
        assert_eq!(mummy.damage_done, 30);
        assert!(mummy.damaged_by_you);
    }

    #[test]
    fn healing_does_not_open_an_encounter() {
        let encs = run(&[
            (100, "You healed Tharn for 500 hit points by Healing Water."),
            (200, "You healed Tharn for 500 hit points by Healing Water."),
        ]);
        assert!(
            encs.is_empty(),
            "out-of-combat healing must not create fights"
        );
    }

    #[test]
    fn healing_is_broken_down_by_spell() {
        let encs = run(&[
            (100, "You slash a mob for 10 points of damage."),
            (101, "You healed Tharn for 300 hit points by Healing Water."),
            (102, "You healed Tharn for 200 hit points by Healing Water."),
            (
                103,
                "You healed Tharn over time for 50 hit points by Blooming Heal.",
            ),
            (104, "You healed Tharn for 25 hit points."),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        assert_eq!(you.healing_done, 575);
        let hw = you.healing_by_spell.get("Healing Water").unwrap();
        assert_eq!(hw.total, 500);
        assert_eq!(hw.hits, 2);
        assert_eq!(hw.max, 300);
        assert_eq!(you.healing_by_spell.get("Blooming Heal").unwrap().total, 50);
        // Unnamed heals must still land somewhere rather than vanishing.
        assert_eq!(you.healing_by_spell.get("heal").unwrap().total, 25);

        let ranked = you.spells_by_healing();
        assert_eq!(ranked[0].0, "Healing Water");
        // Heals must not pollute the damage breakdown.
        assert!(!you.by_ability.contains_key("Healing Water"));
    }

    #[test]
    fn damage_taken_ranking_finds_who_took_the_beating() {
        let encs = run(&[
            (100, "A giant hits YOU for 500 points of damage."),
            (101, "A giant hits Reklar for 100 points of damage."),
            (102, "You slash a giant for 10 points of damage."),
        ]);
        let ranked = encs[0].damage_taken_ranking();
        assert_eq!(ranked[0].1.name, "Tharn");
        assert_eq!(ranked[0].1.damage_taken, 500);
        assert_eq!(ranked[1].1.damage_taken, 100);
        assert_eq!(encs[0].your_damage_taken(), 500);
    }

    /// A pure healer deals no damage; filtering meters on damage alone would
    /// erase them from every view.
    #[test]
    fn pure_healer_still_counts_as_active() {
        let encs = run(&[
            (100, "You slash a mob for 10 points of damage."),
            (
                101,
                "Cleric healed Tharn for 400 hit points by Superior Healing.",
            ),
        ]);
        let cleric = encs[0].actors.get("cleric").unwrap();
        assert_eq!(cleric.damage_done, 0);
        assert!(
            cleric.is_active(),
            "a healer with no damage is still active"
        );
        assert!(
            encs[0]
                .active_actors()
                .iter()
                .any(|(_, s)| s.name == "Cleric"),
            "healer must appear among active actors"
        );
        assert!(
            !encs[0]
                .damage_ranking()
                .iter()
                .any(|(_, s)| s.name == "Cleric"),
            "but not in the damage meter"
        );
        assert_eq!(encs[0].healing_ranking()[0].1.name, "Cleric");
    }

    #[test]
    fn healing_inside_a_fight_is_recorded() {
        let encs = run(&[
            (100, "You slash a lesser mummy for 10 points of damage."),
            (101, "You healed Tharn for 500 hit points by Healing Water."),
            (102, "You have slain a lesser mummy!"),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        assert_eq!(you.healing_done, 500);
        assert_eq!(you.healing_received, 500);
        assert!(you.healed_by_you);
        assert!((encs[0].hps("you") - 250.0).abs() < 1e-9);
    }

    #[test]
    fn loot_and_coin_attach_to_the_fight() {
        let encs = run(&[
            (100, "You slash Lord Nagafen for 10 points of damage."),
            (101, "You have slain Lord Nagafen!"),
            (
                102,
                "You receive 12 platinum, 3 silver and 4 copper from the corpse.",
            ),
            (
                103,
                "--You have looted a Cloak of Flames +3 from Lord Nagafen's corpse.--",
            ),
        ]);
        assert_eq!(encs.len(), 1);
        assert_eq!(encs[0].coin_copper, 12_034);
        assert_eq!(encs[0].loot.len(), 1);
        assert_eq!(encs[0].loot[0].item, "Cloak of Flames +3");
        assert_eq!(encs[0].loot[0].source.as_deref(), Some("Lord Nagafen"));
    }

    #[test]
    fn encounter_with_no_damage_is_discarded() {
        let encs = run(&[(100, "You try to slash a lesser mummy, but miss!")]);
        assert!(encs.is_empty(), "a whiff alone is not an encounter");
    }

    #[test]
    fn title_prefers_what_you_killed() {
        let encs = run(&[
            (100, "You slash Lord Nagafen for 10 points of damage."),
            (101, "A rat hits a bystander for 5 points of damage."),
            (102, "A bystander has been slain by a rat!"),
            (103, "You have slain Lord Nagafen!"),
        ]);
        assert_eq!(encs[0].title(), "Lord Nagafen");
    }

    /// The boss rarely dies first. Titling by kill order would name this fight
    /// after the trash mob.
    #[test]
    fn title_names_the_toughest_kill_not_the_first() {
        let encs = run(&[
            (
                100,
                "You slash a fire giant warrior for 500 points of damage.",
            ),
            (101, "You have slain a fire giant warrior!"),
            (102, "You slash Lord Nagafen for 9000 points of damage."),
            (103, "You have slain Lord Nagafen!"),
        ]);
        assert_eq!(encs[0].title(), "Lord Nagafen (+1 more)");
    }

    #[test]
    fn title_counts_distinct_opponents() {
        let encs = run(&[
            (100, "You slash a rat for 10 points of damage."),
            (101, "You have slain a rat!"),
            (102, "You slash a rat for 10 points of damage."),
            (103, "You have slain a rat!"),
        ]);
        assert_eq!(
            encs[0].title(),
            "a rat",
            "identical mobs should not inflate the count"
        );
    }

    #[test]
    fn damage_ranking_orders_by_output() {
        let encs = run(&[
            (100, "You slash a mob for 10 points of damage."),
            (101, "Reklar slashes a mob for 90 points of damage."),
            (102, "You have slain a mob!"),
        ]);
        let ranking = encs[0].damage_ranking();
        assert_eq!(ranking[0].1.name, "Reklar");
        assert_eq!(ranking[0].1.damage_done, 90);
        assert_eq!(ranking[1].1.damage_done, 10);
    }

    #[test]
    fn deaths_are_recorded_with_killer() {
        let encs = run(&[
            (100, "A lesser mummy hits YOU for 30 points of damage."),
            (101, "You have been slain by a lesser mummy!"),
        ]);
        let e = &encs[0];
        assert_eq!(e.deaths.len(), 1);
        assert_eq!(e.deaths[0].victim, "Tharn");
        assert_eq!(e.deaths[0].killer.as_deref(), Some("a lesser mummy"));
        assert_eq!(e.actors.get("you").unwrap().deaths, 1);
        assert_eq!(e.actors.get("a lesser mummy").unwrap().killing_blows, 1);
    }

    #[test]
    fn charmed_pet_damage_is_credited_to_you() {
        let encs = run(&[
            (
                100,
                "You slash a fire giant warrior for 100 points of damage.",
            ),
            (
                101,
                "An imp protector told you, 'Attacking a fire giant warrior Master.'",
            ),
            (
                102,
                "An imp protector slashes a fire giant warrior for 400 points of damage.",
            ),
            (103, "You have slain a fire giant warrior!"),
        ]);
        let e = &encs[0];
        let imp = e.actors.get("an imp protector").unwrap();
        assert!(imp.charmed_by_you);
        assert_eq!(imp.damage_while_charmed, 400);
        // Personal damage stays honest...
        assert_eq!(e.actors.get("you").unwrap().damage_done, 100);
        // ...but the pet-inclusive figure reflects the whole pull.
        assert_eq!(e.your_damage_with_pets(), 500);
        assert_eq!(e.charmed_pets().len(), 1);
    }

    #[test]
    fn damage_before_the_charm_is_not_credited() {
        let encs = run(&[
            // The imp is a hostile mob first.
            (100, "An imp protector hits YOU for 50 points of damage."),
            (101, "An imp protector told you, 'Attacking a rat Master.'"),
            (
                102,
                "An imp protector slashes a rat for 30 points of damage.",
            ),
            (103, "You slash a rat for 1 point of damage."),
        ]);
        let imp = encs[0].actors.get("an imp protector").unwrap();
        assert_eq!(imp.damage_done, 80, "all damage is still recorded");
        assert_eq!(
            imp.damage_while_charmed, 30,
            "only post-charm damage counts toward your total"
        );
        assert_eq!(encs[0].your_damage_with_pets(), 31);
    }

    #[test]
    fn charm_break_stops_crediting_pet_damage() {
        // The tracker no longer reads charm from effects wearing off — which
        // spell it was decides that, and naming spells needs the database.
        // The caller reports the break through `set_charmed`.
        let mut t = EncounterTracker::new("Tharn");
        let before = entries(&[
            (100, "You slash a rat for 1 point of damage."),
            (101, "An imp protector told you, 'Attacking a rat Master.'"),
            (
                102,
                "An imp protector slashes a rat for 30 points of damage.",
            ),
        ]);
        for e in &before {
            t.push(e);
        }
        t.set_charmed(103, &Actor::Named("An imp protector".into()), false);
        for e in &entries(&[(104, "An imp protector hits YOU for 70 points of damage.")]) {
            t.push(e);
        }
        let enc = t.flush().expect("encounter");

        let imp = enc.actors.get("an imp protector").unwrap();
        assert_eq!(
            imp.damage_while_charmed, 30,
            "damage after the charm broke must not be credited"
        );
        assert_eq!(enc.your_damage_with_pets(), 31);
    }

    /// A death is the last few seconds, not the last hit — so the record has
    /// to carry the run-up, and only for your own deaths.
    #[test]
    fn your_death_records_the_blows_that_caused_it() {
        let encs = run(&[
            (100, "A fire giant hits YOU for 300 points of damage."),
            (102, "A fire giant crushes YOU for 450 points of damage."),
            (104, "You have been slain by a fire giant!"),
        ]);
        let d = &encs[0].deaths[0];
        assert_eq!(d.victim.to_lowercase(), "tharn", "the player, by name");
        assert_eq!(d.damage_in_window, 750);
        assert_eq!(d.final_blows.len(), 2);
        // Newest first: the last thing to land leads.
        assert_eq!(d.final_blows[0].amount, 450);
        assert_eq!(d.final_blows[1].amount, 300);
        assert_eq!(d.final_blows[0].source.to_lowercase(), "a fire giant");
    }

    /// The buffer only holds damage aimed at you, so a mob dying must not be
    /// handed your incoming hits.
    #[test]
    fn a_mob_death_carries_no_post_mortem() {
        let encs = run(&[
            (100, "A fire giant hits YOU for 300 points of damage."),
            (102, "You slash a fire giant for 900 points of damage."),
            (104, "You have slain a fire giant!"),
        ]);
        let d = encs[0].deaths.iter().find(|d| d.victim != "Tharn").unwrap();
        assert!(d.final_blows.is_empty());
        assert_eq!(d.damage_in_window, 0);
    }

    /// Damage from earlier in a long fight is not what killed you.
    #[test]
    fn blows_older_than_the_death_window_are_not_blamed() {
        let encs = run(&[
            (100, "A fire giant hits YOU for 999 points of damage."),
            (108, "A fire giant hits YOU for 10 points of damage."),
            (118, "A fire giant hits YOU for 20 points of damage."),
            (120, "You have been slain by a fire giant!"),
        ]);
        let d = &encs[0].deaths[0];
        // The 999 at t=100 is outside the 15s window ending at t=120.
        assert_eq!(d.damage_in_window, 30, "{:?}", d.final_blows);
        assert_eq!(d.final_blows.len(), 2);
    }

    /// Charm breaks and is resung constantly, so the number worth reporting is
    /// how much of the fight it actually held.
    ///
    /// Timestamps stay inside the idle timeout; spacing them further apart
    /// would close the fight and start another.
    #[test]
    fn charm_uptime_measures_held_time_not_whether_it_ever_landed() {
        let mut t = EncounterTracker::new("Tharn");
        let imp = Actor::Named("An imp protector".into());
        let hit = |t: &mut EncounterTracker, ts: i64| {
            for e in &entries(&[(ts, "You slash a rat for 1 point of damage.")]) {
                t.push(e);
            }
        };
        hit(&mut t, 100);
        t.set_charmed(100, &imp, true);
        t.set_charmed(106, &imp, false); // 6s held, one break
        hit(&mut t, 106);
        t.set_charmed(108, &imp, true);
        hit(&mut t, 112); // still charmed when the fight ends
        let enc = t.flush().expect("encounter");

        let st = enc.actors.get("an imp protector").expect("pet recorded");
        assert_eq!(st.charmed_seconds, 10, "6s + 4s still held at the end");
        assert_eq!(st.charm_breaks, 1);
        // 10 of a 12-second fight.
        assert!(
            (enc.charm_uptime() - 10.0 / 12.0).abs() < 0.01,
            "{}",
            enc.charm_uptime()
        );
    }

    /// Refreshing a charm that never broke must not restart its clock.
    #[test]
    fn re_landing_an_unbroken_charm_does_not_reset_uptime() {
        let mut t = EncounterTracker::new("Tharn");
        let imp = Actor::Named("An imp protector".into());
        let hit = |t: &mut EncounterTracker, ts: i64| {
            for e in &entries(&[(ts, "You slash a rat for 1 point of damage.")]) {
                t.push(e);
            }
        };
        hit(&mut t, 100);
        t.set_charmed(100, &imp, true);
        t.set_charmed(104, &imp, true); // refresh, no break
        hit(&mut t, 106);
        t.set_charmed(108, &imp, true); // refresh, no break
        hit(&mut t, 112);
        let enc = t.flush().expect("encounter");

        let st = enc.actors.get("an imp protector").expect("pet recorded");
        assert_eq!(st.charmed_seconds, 12, "held the whole fight");
        assert_eq!(st.charm_breaks, 0);
        assert!((enc.charm_uptime() - 1.0).abs() < 0.01);
    }

    /// A charmed mob is under a pile of your debuffs. The first of them to
    /// lapse must not be read as the charm ending.
    #[test]
    fn an_unrelated_debuff_fading_does_not_end_a_charm() {
        let encs = run(&[
            (100, "You slash a rat for 1 point of damage."),
            (101, "An imp protector told you, 'Attacking a rat Master.'"),
            (
                102,
                "An imp protector slashes a rat for 30 points of damage.",
            ),
            (
                103,
                "Your Chords of Dissonance spell has worn off of an imp protector.",
            ),
            (
                104,
                "An imp protector slashes a rat for 70 points of damage.",
            ),
        ]);
        let imp = encs[0].actors.get("an imp protector").unwrap();
        assert_eq!(
            imp.damage_while_charmed, 100,
            "a debuff lapsing is not a charm break"
        );
    }

    /// A charm established on one pull can still hold when the next begins.
    #[test]
    fn charm_survives_across_encounters() {
        let encs = run(&[
            (100, "You slash a rat for 1 point of damage."),
            (101, "An imp protector told you, 'Attacking a rat Master.'"),
            // Long gap splits the encounter; the charm is still up.
            (500, "You slash a bat for 1 point of damage."),
            (
                501,
                "An imp protector slashes a bat for 200 points of damage.",
            ),
        ]);
        assert_eq!(encs.len(), 2);
        assert_eq!(
            encs[1].your_damage_with_pets(),
            201,
            "charm should persist into the next pull"
        );
    }

    /// Mob names appear both sentence-cased and lowercase; they must not split
    /// into two actors or damage totals will be halved.
    #[test]
    fn actor_case_folding_keeps_one_entry() {
        let encs = run(&[
            (100, "You slash a lesser mummy for 10 points of damage."),
            (101, "A lesser mummy hits YOU for 5 points of damage."),
            (102, "You have slain a lesser mummy!"),
        ]);
        let mummy_entries: Vec<_> = encs[0]
            .actors
            .keys()
            .filter(|k| k.contains("mummy"))
            .collect();
        assert_eq!(
            mummy_entries.len(),
            1,
            "case variants must fold: {mummy_entries:?}"
        );
    }
}
