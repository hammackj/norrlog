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

/// How long before a fight opens a cast may still be counted as part of it.
///
/// A pull usually starts with a spell, and nothing has been damaged yet when
/// it goes off, so the cast arrives before the encounter exists. Six seconds
/// covers the slowest cast worth opening with plus the flight time of what it
/// throws — short enough that a buff applied while waiting is not swept into
/// the next fight.
pub const CAST_LEAD_WINDOW: i64 = 6;

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
    /// Times you began casting or singing this, which is not `hits`.
    ///
    /// One cast of a damage-over-time spell lands a dozen ticks, and one that
    /// is resisted lands none, so the number of times a spell went off and the
    /// number of times it did something are different questions. Only the
    /// second could be answered before this.
    ///
    /// Zero for melee, which is swung rather than cast.
    #[serde(default)]
    pub casts: u64,
    /// The mechanism that delivered it — "melee", "spell", "dot", "damage
    /// shield". Recorded per ability because the totals alone cannot say
    /// whether a name is a weapon skill, a song, or thorns, and grouping
    /// output by mechanism is the difference between a readable breakdown and
    /// thirty undifferentiated rows.
    #[serde(default)]
    pub kind: String,
    /// True when this ability was used on yourself — a health-for-mana trade
    /// rather than damage aimed at anything.
    #[serde(default)]
    pub self_inflicted: bool,
}

/// Which of the three groups a mechanism belongs to.
///
/// Spells and songs land the same way and read as one thing to a player;
/// weapon skills are another; thorns and damage shields are neither — they are
/// output you did not choose to make, so lumping them into either would
/// misrepresent both.
pub fn ability_group(kind: &str, self_inflicted: bool) -> &'static str {
    // A spell you cast on yourself for a price — Cannibalization trading
    // health for mana — is a cost, not output. Grouping it with your nukes put
    // it in the same column as damage aimed at something, where it competed
    // for the top of the list with the things actually killing the boss.
    if self_inflicted {
        return "Other";
    }
    match kind {
        "melee" => "Melee",
        "damage shield" => "Other",
        // "spell" and "dot" — a direct nuke and its tick are the same source.
        _ => "Spell",
    }
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
    /// Damage you dealt to yourself. Part of `damage_done`, and subtracted
    /// back out wherever the question is how much you put into something else.
    #[serde(default)]
    pub self_damage: u64,
    /// When this actor last dealt or took damage.
    ///
    /// A fight is one encounter until combat stops, so a mob killed early in a
    /// long pull keeps its row for the rest of it — correct for the totals,
    /// misleading on a live meter, where a name sitting there reads as
    /// something still up. This is what lets a view fade what has gone quiet.
    #[serde(default)]
    pub last_action: i64,
    /// True when at least two creatures in this fight answered to this name.
    ///
    /// The log gives no instance id, so identically-named creatures are one
    /// row and nothing can separate them. What *can* be known is that they
    /// were more than one: a creature attacking or killing something with its
    /// own name is proof, and a charming class produces that constantly —
    /// your charmed fire giant warrior fighting another fire giant warrior.
    ///
    /// The row's totals then pool both sides, and if either was yours the
    /// whole row is marked as yours. This flag does not fix that. It says the
    /// number cannot be read as one creature's, which is the difference
    /// between a wrong figure and an honest one.
    #[serde(default)]
    pub name_shared: bool,
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
    /// Heals cast, and how many of them crit. Separate from `hits`/`crits`,
    /// which are damage: a healer's crit rate and a melee crit rate are
    /// different questions and averaging them answers neither.
    pub heals: u64,
    pub heal_crits: u64,
    /// Overhealing, and the number of heals it could be measured on.
    ///
    /// The client states the pre-cap figure on some heal lines and not others,
    /// so this is a floor over a stated denominator rather than a total: with
    /// `heals_measured_for_overheal` below `heals`, the real waste is higher by
    /// however much the unmeasured heals spilled. Reporting it as a percentage
    /// of all healing would quietly invent the missing lines as perfect.
    pub healing_attempted: u64,
    pub over_healing: u64,
    pub heals_measured_for_overheal: u64,
    /// Which mechanism produced each crit — `Critical`, `Crippling Blow`,
    /// `Slay Undead`. A blended melee crit rate cannot say that a paladin's
    /// rate against undead is a different mechanic from an ordinary critical,
    /// and the slay hits are often the largest of the fight.
    pub crits_by_flag: HashMap<String, u64>,
    /// Every flag the client wrote on a swing, hit or miss.
    ///
    /// Separate from `crits_by_flag`, which counts only the flags that mean a
    /// critical. Flurry, Riposte, Rampage and Finishing Blow are extra attacks
    /// or reactions rather than bigger ones, and asking how often you flurry is
    /// a different question from asking how often you crit.
    ///
    /// Misses count. A flurry that misses still happened — leaving it out
    /// would report the mechanic firing less often than it does, which is the
    /// opposite of what the number is for.
    #[serde(default)]
    pub swings_by_flag: HashMap<String, u64>,
    /// How this actor's own swings were turned aside, by mechanism — dodge,
    /// parry, block, riposte. Its offence.
    ///
    /// A plain miss is not here: it is already counted in `misses`, and it is
    /// a different fact. Missing is you failing to connect; being parried is
    /// the target succeeding, and a defence that stops a third of your swings
    /// is worth telling apart from an accuracy problem.
    #[serde(default)]
    pub swings_stopped_by: HashMap<String, u64>,
    /// How this actor turned swings aside, by the same mechanisms. Its
    /// defence.
    #[serde(default)]
    pub avoided_by: HashMap<String, u64>,
    /// Seconds spent stunned, and how many stuns landed or were shrugged off.
    ///
    /// Time stunned is time not attacking, and it is the one gap in a damage
    /// meter that has no other explanation — a fight where the numbers look
    /// poor reads very differently once you can see twelve of its sixty
    /// seconds were spent unable to act.
    #[serde(default)]
    pub stunned_seconds: i64,
    #[serde(default)]
    pub stuns: u32,
    #[serde(default)]
    pub stuns_avoided: u32,
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
    /// Damage this actor absorbed while charmed by you — that is, while it was
    /// fighting on your side rather than against you.
    ///
    /// The mirror of `damage_while_charmed`, and needed for the same reason. A
    /// charmed mob keeps `damaged_by_you` from the pull that landed the charm,
    /// so without this the boss beating on your pet counted as damage your
    /// side put into the enemy — and the headline could exceed everything you
    /// and the pet dealt.
    #[serde(default)]
    pub damage_taken_while_charmed: u64,
    /// The player this creature fights for, when it is a pet.
    ///
    /// Covers your own pets and other people's. Charmed mobs of yours get an
    /// owner too, so "whose damage is this" has one answer to look up rather
    /// than two flags to combine.
    #[serde(default)]
    pub owner: Option<String>,
    /// True when the pet was summoned rather than charmed.
    ///
    /// The distinction is not cosmetic: a summoned pet is yours for the whole
    /// fight and cannot break, so counting one toward charm uptime would
    /// report a rock-solid charm you never cast. They are indistinguishable in
    /// the line that announces them — both say "Master" — so this is set from
    /// the pet's name or from `/pet leader`, never from that line.
    #[serde(default)]
    pub summoned: bool,
    /// True when a charm of yours was actually seen to land on this creature.
    ///
    /// `charmed_by_you` is the looser claim — "counts as your pet for damage
    /// credit" — and a pet announcing itself with "Master" is enough for that.
    /// It is not enough to call the creature charmed: a summoned pet says the
    /// same words. Two things clear that bar and set this, and only this is
    /// allowed to put the word on screen: a charm spell seen to land, or a
    /// "Master" line from a creature that has damaged you, which no pet of
    /// your own summoning has ever done.
    #[serde(default)]
    pub charm_seen: bool,
    /// True when `owner` is the logging character.
    ///
    /// Stored rather than compared on demand: the owner is a display name and
    /// the places that need the answer — encounter totals, the meter — do not
    /// carry the player's name to compare it against.
    #[serde(default)]
    pub owned_by_you: bool,
}

impl AbilityStats {
    /// Fold another fight's record of the same ability into this one.
    pub fn merge_from(&mut self, o: &AbilityStats) {
        self.total += o.total;
        self.hits += o.hits;
        self.misses += o.misses;
        self.crits += o.crits;
        // `min` starts at zero on a record that has never landed anything, so
        // taking the smaller of the two would lock it there forever.
        self.min = match (self.min, o.min) {
            (0, m) | (m, 0) => m,
            (a, b) => a.min(b),
        };
        self.max = self.max.max(o.max);
        if self.kind.is_empty() {
            self.kind = o.kind.clone();
        }
    }
}

/// Sum one map of totals into another.
fn merge_counts(into: &mut HashMap<String, u64>, from: &HashMap<String, u64>) {
    for (k, v) in from {
        *into.entry(k.clone()).or_default() += v;
    }
}

/// Sum one map of ability records into another.
fn merge_abilities(into: &mut HashMap<String, AbilityStats>, from: &HashMap<String, AbilityStats>) {
    for (k, v) in from {
        into.entry(k.clone()).or_default().merge_from(v);
    }
}

impl ActorStats {
    /// Fold this actor's part in another fight into this one.
    ///
    /// Used to build a whole-session meter out of the fights that make it up.
    /// Every flag is sticky: a creature charmed in one pull and merely fought
    /// in the next was still, at some point, charmed by you.
    pub fn merge_from(&mut self, o: &ActorStats) {
        self.damage_done += o.damage_done;
        self.damage_taken += o.damage_taken;
        self.self_damage += o.self_damage;
        self.name_shared |= o.name_shared;
        self.last_action = self.last_action.max(o.last_action);
        self.stunned_seconds += o.stunned_seconds;
        self.stuns += o.stuns;
        self.stuns_avoided += o.stuns_avoided;
        self.healing_done += o.healing_done;
        self.healing_received += o.healing_received;
        self.hits += o.hits;
        self.misses += o.misses;
        self.crits += o.crits;
        self.heals += o.heals;
        self.heal_crits += o.heal_crits;
        self.healing_attempted += o.healing_attempted;
        self.over_healing += o.over_healing;
        self.heals_measured_for_overheal += o.heals_measured_for_overheal;
        self.avoided += o.avoided;
        self.deaths += o.deaths;
        self.killing_blows += o.killing_blows;
        self.attacks_missed += o.attacks_missed;
        self.charmed_seconds += o.charmed_seconds;
        self.charm_breaks += o.charm_breaks;
        self.damage_while_charmed += o.damage_while_charmed;
        self.damage_taken_while_charmed += o.damage_taken_while_charmed;
        self.damaged_by_you |= o.damaged_by_you;
        self.healed_by_you |= o.healed_by_you;
        self.charmed_by_you |= o.charmed_by_you;
        merge_counts(&mut self.damage_by_kind, &o.damage_by_kind);
        merge_counts(&mut self.damage_taken_by_kind, &o.damage_taken_by_kind);
        merge_counts(&mut self.damage_taken_by_source, &o.damage_taken_by_source);
        merge_counts(&mut self.hits_by_kind, &o.hits_by_kind);
        merge_counts(&mut self.crits_by_kind, &o.crits_by_kind);
        merge_counts(&mut self.crits_by_flag, &o.crits_by_flag);
        merge_counts(&mut self.swings_by_flag, &o.swings_by_flag);
        merge_counts(&mut self.swings_stopped_by, &o.swings_stopped_by);
        merge_counts(&mut self.avoided_by, &o.avoided_by);
        merge_abilities(&mut self.by_ability, &o.by_ability);
        merge_abilities(&mut self.damage_taken_by_ability, &o.damage_taken_by_ability);
        merge_abilities(&mut self.healing_by_spell, &o.healing_by_spell);
    }

    /// Whether this creature fought for you — summoned or charmed.
    ///
    /// `charmed_by_you` is checked as well as the owner flag so fights parsed
    /// before pets carried an owner still count their charms.
    pub fn is_yours(&self) -> bool {
        self.owned_by_you || self.charmed_by_you
    }

    /// Damage this creature contributed to its owner's total.
    ///
    /// A charmed mob was an enemy before the charm landed and is one again the
    /// moment it breaks, so only what it dealt while held belongs to the
    /// charmer. Anything else fighting for you — summoned, or a pet whose kind
    /// was never established — is yours for the whole fight, so all of its
    /// output counts.
    ///
    /// Keyed on the charm rather than on `summoned` because those two are not
    /// a partition: a magician's pet is recognised by neither naming
    /// convention nor `/pet leader`, and crediting it the charmed way would
    /// silently drop every point of damage it dealt.
    pub fn credited_damage(&self) -> u64 {
        if self.charmed_by_you {
            self.damage_while_charmed
        } else {
            self.damage_done
        }
    }

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

    /// Share of heals that critically healed.
    ///
    /// Its own figure rather than folded into [`Self::crit_rate`]: the two come
    /// from different mechanics, and a healer who never swings a weapon would
    /// otherwise have no crit rate at all.
    pub fn heal_crit_rate(&self) -> f64 {
        if self.heals == 0 {
            0.0
        } else {
            self.heal_crits as f64 / self.heals as f64
        }
    }

    /// Crits by the mechanism that caused them, biggest first.
    pub fn crit_causes(&self) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = self
            .crits_by_flag
            .iter()
            .map(|(k, n)| (k.clone(), *n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
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
    /// Spells of yours that were shrugged off: target -> spell -> times.
    ///
    /// Kept per target as well as per spell because the two answer different
    /// questions — whether a spell is worth casting at all, and whether one
    /// creature in particular is resistant to it.
    #[serde(default)]
    pub resists: HashMap<String, HashMap<String, u64>>,
    /// Your own actions in order, for the per-skill timeline.
    ///
    /// Totals answer "what did I do"; only a time axis answers "when". A burn
    /// window, a proc chain, a dot resisted halfway through a fight — all of
    /// them are invisible in a sum and obvious in a lane.
    ///
    /// Yours alone. A charmed pet's damage counts toward your total but it is
    /// not a skill you pressed, and mixing the two makes the lanes unreadable.
    #[serde(default)]
    pub timeline: Vec<TimelineEvent>,
    /// Stance and invocation changes made during the fight.
    #[serde(default)]
    pub marks: Vec<TimelineMark>,
    /// Events that happened but were not recorded, because the fight ran past
    /// [`TIMELINE_MAX`]. Reported rather than hidden: a truncated timeline that
    /// looks complete would misread as a lull.
    #[serde(default)]
    pub timeline_dropped: u64,
    /// Whether `timeline` still holds this fight's events.
    ///
    /// False means "no longer kept", not "nothing happened" — the engine
    /// releases the per-event detail of older fights to bound memory.
    #[serde(default = "yes")]
    pub timeline_retained: bool,
}

fn yes() -> bool {
    true
}

/// How many of your actions one fight retains.
///
/// A busy fight is a few actions a second, so this covers something like
/// twenty minutes of unbroken output — past any real encounter, and short of
/// letting an AFK autoattack session grow without bound.
pub const TIMELINE_MAX: usize = 4000;

/// How one of your actions turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Hit,
    Crit,
    /// A swing that missed, or one the defender avoided outright.
    Miss,
    /// A spell shrugged off.
    Resist,
}

/// One thing you did, and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub at: i64,
    /// The spell, or the melee verb for a swing.
    pub ability: String,
    pub outcome: Outcome,
    /// Zero for anything that did not land.
    pub amount: u64,
    /// The mechanism — "melee", "spell", "dot", "damage shield". Empty for a
    /// cast or a resist, which landed nothing to classify.
    ///
    /// Carried so a damage-over-time effect can be drawn as the span it
    /// occupied rather than as the moments it ticked: uptime is the question a
    /// dot raises, and a row of dots does not answer it.
    #[serde(default)]
    pub kind: String,
}

/// A moment worth marking on the timeline that is not damage.
///
/// Stance and invocation changes: both are deliberate acts mid-fight whose
/// effect is on everything after them, so they belong as a line across the
/// chart rather than as a lane of their own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineMark {
    pub at: i64,
    /// "stance" or "invocation".
    pub kind: String,
    pub label: String,
}

/// One creature's share of a fight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobBreakdown {
    pub name: String,
    /// What you and yours put into it.
    pub damage_done_to_it: u64,
    /// What it put out, at anyone.
    pub damage_it_did: u64,
    /// Times it died — more than one when the same name was pulled repeatedly.
    pub kills: u32,
    pub loot: usize,
    pub resists: u64,
}

/// One spell's resist record within a fight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResistTally {
    pub spell: String,
    pub resisted: u64,
    /// Times the same spell did land here, when it ever did.
    pub landed: u64,
    /// Targets that resisted it, most resistant first.
    pub targets: Vec<(String, u64)>,
}

impl ResistTally {
    /// Share of attempts shrugged off. Only meaningful when the spell also
    /// landed at least once — a spell that never landed has no denominator
    /// beyond its own failures.
    pub fn rate(&self) -> f64 {
        let attempts = self.resisted + self.landed;
        if attempts == 0 {
            0.0
        } else {
            self.resisted as f64 / attempts as f64
        }
    }
}

impl Encounter {
    /// Fold another fight into this one, to build a wider view.
    ///
    /// The result is a real encounter in every respect except time: `start`
    /// and `end` come to span the whole run, gaps included, so its `duration`
    /// is wall-clock and **not** a denominator for damage per second. The
    /// caller knows how much of that span was actually spent fighting and
    /// should divide by that instead.
    ///
    /// The timeline is deliberately not merged: laid end to end across an
    /// evening's pulls it would be unreadable, and the per-fight view already
    /// answers the question it exists for.
    pub fn merge_from(&mut self, o: &Encounter) {
        self.start = if self.start == 0 {
            o.start
        } else {
            self.start.min(o.start)
        };
        self.end = self.end.max(o.end);
        self.coin_copper += o.coin_copper;
        self.deaths.extend(o.deaths.iter().cloned());
        self.loot.extend(o.loot.iter().cloned());
        for (key, stats) in &o.actors {
            match self.actors.get_mut(key) {
                Some(mine) => mine.merge_from(stats),
                None => {
                    self.actors.insert(key.clone(), stats.clone());
                }
            }
        }
        for (target, spells) in &o.resists {
            let into = self.resists.entry(target.clone()).or_default();
            for (spell, n) in spells {
                *into.entry(spell.clone()).or_default() += n;
            }
        }
        self.timeline_retained = false;
    }

    /// Record one of your actions on the timeline, if there is room.
    fn note_timeline(
        &mut self,
        at: i64,
        ability: &str,
        outcome: Outcome,
        amount: u64,
        kind: &str,
    ) {
        if self.timeline.len() >= TIMELINE_MAX {
            self.timeline_dropped += 1;
            return;
        }
        self.timeline.push(TimelineEvent {
            at,
            ability: ability.to_string(),
            outcome,
            amount,
            kind: kind.to_string(),
        });
    }


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
    ///
    /// What a creature absorbed while charmed by you is not part of it. A
    /// charmed mob was an enemy before the charm landed, and keeps
    /// `damaged_by_you` from then on, so counting everything it took meant the
    /// boss hitting your own pet was scored as damage into the enemy — leaving
    /// a headline larger than everything you and the pet dealt put together.
    /// Damage done to it *before* the charm still counts, because then it
    /// really was the enemy.
    pub fn damage_absorbed_by_enemies(&self) -> u64 {
        self.actors
            .values()
            .filter(|s| s.damaged_by_you)
            .map(|s| s.damage_taken - s.damage_taken_while_charmed)
            .sum()
    }

    /// What you and your pets put into the enemy.
    ///
    /// Measured from the output side — your damage plus your pets' — rather
    /// than from what enemies absorbed. The two differ whenever anything else
    /// is hitting the same target: another player, their pet, a mob feuding
    /// with yours. Absorbed damage is a fair account of the fight but a poor
    /// account of *you*, and this figure sits beside your own numbers, so it
    /// answers the question actually being asked of it.
    ///
    /// Self-inflicted damage comes back out. Cannibalization trades health for
    /// mana; it is output that was never aimed at anything, and leaving it in
    /// credited a mana tick as damage to the boss.
    pub fn damage_to_enemies(&self) -> u64 {
        self.your_damage_with_pets()
            .saturating_sub(self.actors.get("you").map(|s| s.self_damage).unwrap_or(0))
    }

    /// Damage absorbed by you and anything fighting alongside you.
    ///
    /// Includes what your charmed pets took while they were holding, which is
    /// the half [`Self::damage_to_enemies`] sets aside.
    pub fn damage_to_allies(&self) -> u64 {
        self.actors
            .values()
            .map(|s| {
                if s.damaged_by_you {
                    s.damage_taken_while_charmed
                } else {
                    s.damage_taken
                }
            })
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
            .filter(|s| s.is_yours())
            .map(|s| s.credited_damage())
            .sum();
        own + pets
    }

    /// Every creature fighting for you this fight that put out damage —
    /// charmed or summoned.
    pub fn your_pets(&self) -> Vec<&ActorStats> {
        let mut v: Vec<&ActorStats> = self
            .actors
            .values()
            .filter(|s| s.is_yours() && s.credited_damage() > 0)
            .collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.credited_damage()));
        v
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

    /// Creatures actually seen to be charmed, for the charm-specific figures.
    ///
    /// Keyed on `charm_seen` rather than `charmed_by_you`: the looser flag is
    /// set by a pet announcing itself, which a summoned pet does too, and
    /// counting one of those here printed "charm held 0% of the fight" under a
    /// fight where no charm was ever cast.
    pub fn charmed_pets(&self) -> Vec<&ActorStats> {
        let mut v: Vec<&ActorStats> = self
            .actors
            .values()
            .filter(|s| s.charm_seen && s.damage_while_charmed > 0)
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
    /// What got shrugged off in this fight, most-resisted first.
    ///
    /// `landed` comes from your own ability breakdown, so a spell that
    /// sometimes worked carries a real rate rather than a bare failure count.
    pub fn resist_tallies(&self) -> Vec<ResistTally> {
        let mut by_spell: HashMap<&str, (u64, Vec<(String, u64)>)> = HashMap::new();
        for (target, spells) in &self.resists {
            for (spell, n) in spells {
                let e = by_spell.entry(spell.as_str()).or_default();
                e.0 += n;
                e.1.push((target.clone(), *n));
            }
        }
        let yours = self.actors.get("you");
        let mut out: Vec<ResistTally> = by_spell
            .into_iter()
            .map(|(spell, (resisted, mut targets))| {
                targets.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                ResistTally {
                    landed: yours
                        .and_then(|s| s.by_ability.get(spell))
                        .map(|a| a.hits)
                        .unwrap_or(0),
                    spell: spell.to_string(),
                    resisted,
                    targets,
                }
            })
            .collect();
        out.sort_by(|a, b| b.resisted.cmp(&a.resisted).then_with(|| a.spell.cmp(&b.spell)));
        out
    }

    /// Per-creature breakdown: what you fought inside this fight.
    ///
    /// A fight bounded by a lull can span a named and the trash that followed
    /// it, which is honest about the combat but lumps their damage and loot
    /// together. This splits it back out by creature without pretending they
    /// were separate fights.
    pub fn per_mob(&self) -> Vec<MobBreakdown> {
        let mut out: Vec<MobBreakdown> = self
            .actors
            .iter()
            .filter(|(key, s)| key.as_str() != "you" && s.damaged_by_you)
            .map(|(_, s)| MobBreakdown {
                name: s.name.clone(),
                damage_done_to_it: s.damage_taken,
                damage_it_did: s.damage_done,
                kills: self
                    .deaths
                    .iter()
                    .filter(|d| d.victim.eq_ignore_ascii_case(&s.name))
                    .count() as u32,
                loot: self
                    .loot
                    .iter()
                    .filter(|l| {
                        l.source
                            .as_deref()
                            .is_some_and(|src| src.eq_ignore_ascii_case(&s.name))
                    })
                    .count(),
                resists: self.resists.get(&s.name).map(|m| m.values().sum()).unwrap_or(0),
            })
            .collect();
        out.sort_by_key(|m| std::cmp::Reverse(m.damage_done_to_it));
        out
    }

    pub fn is_significant(&self) -> bool {
        self.total_damage() > 0
    }

    fn entry(&mut self, actor: &Actor, player: &str) -> &mut ActorStats {
        let key = actor.key();
        self.actors.entry(key).or_insert_with(|| {
            let name = actor.display(player);
            // "Brennagh`s warder" says whose it is without anyone having to
            // type a command, and it is the only attribution available for
            // another player's pet that never talks to you.
            let owner = crate::event::pet_owner_in_name(&name).map(str::to_string);
            ActorStats {
                summoned: owner.is_some(),
                owned_by_you: owner.as_deref() == Some(player),
                owner,
                name,
                ..Default::default()
            }
        })
    }

    /// Pets fighting for `owner`, whatever their kind.
    pub fn pets_of(&self, owner: &str) -> Vec<&ActorStats> {
        let mut v: Vec<&ActorStats> = self
            .actors
            .values()
            .filter(|s| s.owner.as_deref() == Some(owner))
            .collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.credited_damage()));
        v
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
    /// Actor keys currently charmed by the logging character, each mapped to
    /// the display name to put on screen.
    ///
    /// Lives on the tracker rather than the encounter because a charm can be
    /// established in one pull and still hold when the next one starts. That
    /// same span is why entries must be removed as carefully as they are added:
    /// nothing else clears this, so anything left behind is left for the
    /// session.
    charmed: std::collections::HashMap<String, String>,
    /// When the charm currently held on each creature began.
    charm_since: std::collections::HashMap<String, i64>,
    /// When the stun currently on you began, if one is.
    stunned_since: Option<i64>,
    /// Pet actor key -> the display name of the player it belongs to.
    ///
    /// On the tracker rather than the encounter because a pet outlives any one
    /// pull: it is summoned once and fights all evening, and `/pet leader` is
    /// typed once if at all. Losing the attribution between fights would mean
    /// re-deriving it from a line that may never come again.
    pet_owners: std::collections::HashMap<String, String>,
    /// Pets known to be summoned rather than charmed.
    summoned: std::collections::HashSet<String>,
    /// Actor keys that have damaged the logging character at some point.
    ///
    /// The evidence that separates a charmed mob from a summoned pet. Both
    /// answer "Master" in identical words, and the naming conventions catch
    /// only some summons — a magician's pet is called `Lebekab`, which looks
    /// like nothing in particular. What a summoned pet never does is attack
    /// you, and what a charmed mob nearly always did is exactly that, before
    /// the charm and again when it breaks.
    hostile: std::collections::HashSet<String>,
    /// Rolling buffer of damage aimed at you, for death post-mortems.
    recent_incoming: Vec<IncomingHit>,
    /// Casts made with no fight open, awaiting one to belong to.
    pending_casts: Vec<(i64, String)>,
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
            charmed: std::collections::HashMap::new(),
            charm_since: std::collections::HashMap::new(),
            stunned_since: None,
            pet_owners: std::collections::HashMap::new(),
            summoned: std::collections::HashSet::new(),
            hostile: std::collections::HashSet::new(),
            recent_incoming: Vec::new(),
            pending_casts: Vec::new(),
        }
    }

    /// Record that a creature fights for a player, and whether it was summoned.
    ///
    /// Applied to the open encounter as well as remembered, so a pet whose
    /// owner is learned mid-fight — the moment somebody types `/pet leader` —
    /// is credited for the whole of that fight rather than from then on.
    fn set_pet_owner(&mut self, pet: &Actor, owner: &str, summoned: bool) {
        let key = pet.key();
        self.pet_owners.insert(key.clone(), owner.to_string());
        if summoned {
            self.summoned.insert(key);
        }
        let player = self.player.clone();
        let yours = owner == player;
        if let Some(enc) = self.current.as_mut() {
            let st = enc.entry(pet, &player);
            st.owner = Some(owner.to_string());
            st.owned_by_you = yours;
            if summoned {
                st.summoned = true;
            }
        }
    }

    /// Whether this creature is known to be a summoned pet rather than a
    /// charmed mob. Used to keep summoned pets out of the charm bookkeeping.
    fn is_summoned(&self, pet: &Actor) -> bool {
        self.summoned.contains(&pet.key())
            || crate::event::pet_owner_in_name(&pet.display(&self.player)).is_some()
    }

    /// Creatures under your control *right now*, by display name.
    ///
    /// The answer to "what am I holding", which is not the question
    /// [`ActorStats::charmed_by_you`] answers. That flag is deliberately sticky
    /// — it means "was yours at some point this fight", which is what damage
    /// credit needs and the opposite of what a live panel needs. Reading it for
    /// a live display accumulates every creature charmed during the fight and
    /// never drops one.
    ///
    /// Sorted, because the backing map has no order and a panel whose rows
    /// reshuffle on every poll is unreadable.
    pub fn charmed_now(&self) -> Vec<String> {
        let mut v: Vec<String> = self.charmed.values().cloned().collect();
        v.sort();
        v
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
    /// Mark a stance or invocation change on the timeline.
    ///
    /// Only inside a fight: swapping between pulls is ordinary housekeeping,
    /// and marking it would put a line on every chart for something that
    /// happened while nothing was going on.
    pub fn note_mark(&mut self, at: i64, kind: &str, label: &str) {
        if let Some(enc) = self.current.as_mut() {
            if enc.marks.len() < TIMELINE_MAX {
                enc.marks.push(TimelineMark {
                    at,
                    kind: kind.to_string(),
                    label: label.to_string(),
                });
            }
        }
    }

    pub fn set_charmed(&mut self, timestamp: i64, actor: &Actor, charmed: bool) {
        let key = actor.key();
        if charmed {
            self.charmed.insert(key.clone(), actor.display(&self.player));
            // Re-landing a charm that never broke must not restart the clock,
            // or a fight of constant refreshes would report almost no uptime.
            self.charm_since.entry(key).or_insert(timestamp);
            if let Some(enc) = self.current.as_mut() {
                let player = self.player.clone();
                let st = enc.entry(actor, &player);
                st.charmed_by_you = true;
                // This path is only reached when the caller resolved a charm
                // spell against the database, so the charm is a fact here.
                st.charm_seen = true;
                st.owner = Some(player.clone());
                st.owned_by_you = true;
            }
        } else {
            self.end_charm(timestamp, actor, true);
        }
    }

    /// Stop holding a creature, recording a break only if the charm failed.
    ///
    /// A charm coming loose and a charmed pet dying both stop the clock, but
    /// only the first is a break. Counting deaths would inflate the figure a
    /// charmer reads to judge how well the charm is holding — and on a class
    /// that charms to kill, the pet dying is the *good* outcome.
    /// Let go of everything at once, none of it counted as a break.
    fn clear_charms(&mut self, timestamp: i64) {
        // The stored display name rebuilds the actor exactly: the key it was
        // filed under is that name lowercased.
        let held: Vec<String> = self.charmed.values().cloned().collect();
        for name in held {
            self.end_charm(timestamp, &Actor::Named(name), false);
        }
    }

    fn end_charm(&mut self, timestamp: i64, actor: &Actor, was_break: bool) {
        let key = actor.key();
        self.charmed.remove(&key);
        // Absent when the charm was inferred from a "Master" line rather than
        // seen to land, which starts no clock; there is then no span to close
        // and nothing to count.
        if let Some(since) = self.charm_since.remove(&key) {
            if let Some(enc) = self.current.as_mut() {
                let player = self.player.clone();
                let st = enc.entry(actor, &player);
                st.charmed_seconds += (timestamp - since).max(0);
                if was_break {
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
            // A stun is only worth recording inside a fight — being stunned
            // by an ambush while running somewhere is not a gap in anyone's
            // damage.
            Event::Stunned => {
                if self.current.is_some() {
                    self.stunned_since = Some(entry.timestamp);
                    let player = self.player.clone();
                    if let Some(enc) = self.current.as_mut() {
                        enc.entry(&Actor::You, &player).stuns += 1;
                    }
                }
            }
            Event::StunEnded => {
                if let Some(since) = self.stunned_since.take() {
                    let player = self.player.clone();
                    if let Some(enc) = self.current.as_mut() {
                        enc.entry(&Actor::You, &player).stunned_seconds +=
                            (entry.timestamp - since).max(0);
                    }
                }
            }
            Event::StunAvoided => {
                let player = self.player.clone();
                if let Some(enc) = self.current.as_mut() {
                    enc.entry(&Actor::You, &player).stuns_avoided += 1;
                }
            }
            Event::ZoneChange { zone } => {
                // You cannot still be fighting something in a zone you left.
                let z = self.close();
                // Nor holding it. A charmed pet does not cross the zone line
                // with you, and a login arrives here too, so this is also what
                // ends the charms of the session before. Without it the held
                // set is never emptied at all for charms known only from a
                // "Master" line: no spell was named, so no spell can wear off,
                // and a creature charmed in July was still on screen in August.
                self.clear_charms(entry.timestamp);
                self.zone = Some(zone.clone());
                return z.or(closed);
            }
            Event::PetAttack { pet, .. } => {
                // Only something you control calls you Master — but a summoned
                // pet says it too, so this proves ownership, not charm.
                let player = self.player.clone();
                let summoned = self.is_summoned(pet);
                self.set_pet_owner(pet, &player, summoned);
                // Calling it a charm needs evidence of its own, not merely the
                // absence of evidence of summoning. `is_summoned` recognises
                // the naming conventions and whatever `/pet leader` has
                // answered, and that misses a magician's pet entirely — five
                // creatures in these logs that never once attacked the player
                // were being counted as charmed, and one was still on the
                // overlay weeks later. Having fought you is what a summoned pet
                // never does.
                //
                // Deliberately not folded into `is_summoned`: that verdict is
                // recorded permanently in `summoned`, and a mob charmed before
                // it ever swung at you would be branded a pet for good.
                if !summoned && self.hostile.contains(&pet.key()) {
                    // For a charming class this line is the primary evidence a
                    // mob has switched sides: the pet announces itself when
                    // ordered to attack, and the spell that took hold often
                    // cannot be named from the log at all.
                    self.charmed.insert(pet.key(), pet.display(&player));
                    // Start the clock too, or the charm is held but never
                    // timed: this is the only evidence of charm a log gives
                    // for a spell that could not be named, and without a start
                    // there is no span for `end_charm` to close, so uptime and
                    // the break count both stay at zero for the whole session.
                    //
                    // `or_insert`, because the pet repeats this line on every
                    // attack order — dozens of times in one fight — and
                    // restarting the clock at each one would report a charm
                    // that was never held for longer than a single order.
                    self.charm_since.entry(pet.key()).or_insert(entry.timestamp);
                    if let Some(enc) = self.current.as_mut() {
                        let st = enc.entry(pet, &player);
                        st.charmed_by_you = true;
                        // Safe to call it charm on screen now: the objection
                        // was that a summoned pet says the same words, and a
                        // summoned pet has not spent the evening hitting you.
                        st.charm_seen = true;
                    }
                }
            }
            // Deliberately does not `ensure_open`: a spell is not combat until
            // something comes of it, and buffing between pulls must not open a
            // fight. See `note_cast` for where an unattached one goes.
            Event::CastStart { source, spell, .. } if source.is_you() => {
                self.note_cast(entry.timestamp, spell);
            }
            // A pet's casts, on the same footing as your own. Its procs are
            // the bulk of what a summoned pet contributes, and counting the
            // hits without the casts left no way to see how many landed.
            //
            // Only inside an open fight and only for something already known
            // to be yours: a cast by a passing creature is not your pet's, and
            // opening a fight on one would make every caster in earshot a
            // combatant.
            Event::CastStart { source, spell, .. } => {
                let key = source.key();
                let mine = self
                    .pet_owners
                    .get(&key)
                    .is_some_and(|o| *o == self.player)
                    || self.charmed.contains_key(&key);
                if !mine {
                    return None;
                }
                let player = self.player.clone();
                if let Some(enc) = self.current.as_mut() {
                    enc.entry(source, &player)
                        .by_ability
                        .entry(spell.to_string())
                        .or_default()
                        .casts += 1;
                }
            }
            Event::PetLeader { pet, leader } => {
                // A creature that names a leader is a pet by definition. It
                // was also taken as proof of *summoning*, on the reasoning
                // that a charmed mob is not on anybody's pet window — and the
                // game disagrees. `/pet leader` typed at a charmed fire giant
                // answered "My leader is Tharn", which under the old rule filed
                // it as summoned and so excluded it from charm bookkeeping
                // from that moment on: uptime and breaks silently stopped
                // being counted for a creature still plainly charmed.
                //
                // So the command proves whose it is, and summoning is claimed
                // only when nothing says the creature is charmed. That still
                // catches the case this was for — a magician's pet whose
                // generated name matches no convention, which `/pet leader` is
                // the only way to identify at all.
                let owner = leader.display(&self.player);
                let summoned = !self.charmed.contains_key(&pet.key());
                self.set_pet_owner(pet, &owner, summoned);
            }
            // Deliberately does *not* end a charm. Which spell wore off decides
            // that, and naming spells needs the database this crate cannot
            // see, so the caller makes the call via `set_charmed`. Clearing on
            // any effect looks safe but is not: a charmed mob is under a pile
            // of your debuffs, and the first of them to lapse would end the
            // charm early. On one Innoruuk fight that cut a 92-second charm to
            // 14 and lost most of the pet's damage.
            Event::EffectWoreOffTarget { .. } => {}
            // A resist is not combat activity in its own right: it neither
            // opens a fight nor keeps a dying one alive, so it is recorded on
            // the encounter in progress and otherwise ignored. Outside a fight
            // there is nothing to attach it to.
            Event::Resisted { target, spell } => {
                if let Some(enc) = self.current.as_mut() {
                    let who = target.display(&self.player);
                    *enc.resists
                        .entry(who)
                        .or_default()
                        .entry(spell.clone())
                        .or_default() += 1;
                    // The client only reports resists of your own spells, so
                    // this needs no source check.
                    enc.note_timeline(entry.timestamp, spell, Outcome::Resist, 0, "");
                }
            }
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
                let mut src_charmed = self.charmed.contains_key(&source.key());
                // The target's own charm state, which is what decides
                // whether a hit landing on it was landing on an enemy.
                let tgt_charmed = self.charmed.contains_key(&target.key());
                // Blows in either direction count: the game gives you no way to
                // attack a pet of your own summoning, so having swung at a
                // creature says it was an enemy just as plainly as being hit
                // by it does.
                if source.is_you() {
                    self.hostile.insert(target.key());
                }
                if target.is_you() {
                    self.hostile.insert(source.key());
                    // A creature you are holding does not attack you. When one
                    // does, the charm is over, and for a charm known only from
                    // a "Master" line this is the only line in the log that
                    // says so — there is no named spell to wear off.
                    //
                    // Done before the credit below rather than after, so the
                    // blow that ended the charm is not counted as your pet's
                    // work and added to your own damage.
                    if src_charmed {
                        self.end_charm(entry.timestamp, source, true);
                        src_charmed = false;
                    }
                    // Pet attribution is remembered across fights on purpose:
                    // the line naming an owner comes once and the pet fights
                    // all evening. A charmed mob gets that record too, and it
                    // outlives the charm by however long the log runs — a
                    // flighty fiend charmed on 31 July was still filed as your
                    // pet on 19 August, its damage credited to you while it
                    // was beating on you in the same fight.
                    //
                    // Being hit by it settles the question, whatever the
                    // record says and however it was arrived at. Nothing of
                    // your own turns on you.
                    //
                    // That has to include the `summoned` brand. It is inferred
                    // — `/pet leader` answers "My leader is Tharn" for a charmed
                    // mob just as readily as for a conjured one, so the verdict
                    // rests on whether the charm was known to be held at that
                    // instant, and a charm the log never named is not. A fire
                    // giant warrior charmed on 12 August was asked `/pet
                    // leader` a moment after its charm lapsed, was recorded as
                    // summoned for good, and a week later was still the
                    // player's pet in Nagafen's Lair while fighting them.
                    //
                    // Only what was attributed to *you* is dropped. Another
                    // player's pet hitting you says nothing about whose it is.
                    let was_yours = self
                        .pet_owners
                        .get(&source.key())
                        .is_some_and(|o| *o == player);
                    if was_yours {
                        self.pet_owners.remove(&source.key());
                        self.summoned.remove(&source.key());
                        if let Some(enc) = self.current.as_mut() {
                            if let Some(st) = enc.actors.get_mut(&source.key()) {
                                st.owned_by_you = false;
                                st.owner = None;
                                st.summoned = false;
                            }
                        }
                    }
                }
                // A pet is summoned once and fights all evening, while the
                // line that named its owner came in some earlier pull. Carry
                // the attribution forward so every fight credits it, not just
                // the one where `/pet leader` happened to be typed.
                let src_owner = self.pet_owners.get(&source.key()).cloned();
                let src_summoned = self.summoned.contains(&source.key());
                let enc = self.current.as_mut().unwrap();

                let ability_name = ability
                    .clone()
                    .unwrap_or_else(|| kind_label(*kind).to_string());
                let ability_label = ability_name.clone();
                let mut incoming: Option<IncomingHit> = None;
                let src_is_you = source.is_you();
                // A spell cast on yourself for a price. Its damage is real and
                // stays in the totals, but it was never aimed at anything.
                let hit_yourself = src_is_you && target.is_you();
                // Two creatures of the same name, proved by one attacking the
                // other: the log writes both as the same actor, so this line
                // is the only evidence they were ever two.
                let shared_name = !src_is_you && source.key() == target.key();
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
                    if let Some(owner) = &src_owner {
                        s.owned_by_you = owner == &player;
                        s.summoned |= src_summoned;
                        s.owner = Some(owner.clone());
                    }
                    let label = kind_label(*kind).to_string();
                    *s.damage_by_kind.entry(label.clone()).or_default() += *amount;
                    *s.hits_by_kind.entry(label.clone()).or_default() += 1;
                    for f in flags.names() {
                        *s.swings_by_flag.entry(f.to_string()).or_default() += 1;
                    }
                    if crit {
                        *s.crits_by_kind.entry(label).or_default() += 1;
                        // Also by cause. A hit can carry more than one crit
                        // flag in principle, so this can exceed `crits` — it is
                        // a tally of mechanisms, not a second count of hits.
                        for k in flags.crit_kinds() {
                            *s.crits_by_flag.entry(k.to_string()).or_default() += 1;
                        }
                    }
                    let a = s.by_ability.entry(ability_name).or_default();
                    if a.kind.is_empty() {
                        a.kind = kind_label(*kind).to_string();
                    }
                    if hit_yourself {
                        a.self_inflicted = true;
                    }
                    if shared_name {
                        s.name_shared = true;
                    }
                    s.last_action = entry.timestamp;
                    a.record_hit(*amount, crit);
                    if hit_yourself {
                        s.self_damage += *amount;
                    }
                }
                if src_is_you {
                    let outcome = if crit { Outcome::Crit } else { Outcome::Hit };
                    enc.note_timeline(entry.timestamp, &ability_label, outcome, *amount, kind_label(*kind));
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
                    t.last_action = entry.timestamp;
                    if tgt_charmed {
                        t.damage_taken_while_charmed += *amount;
                    }
                    *t.damage_taken_by_kind.entry(label).or_default() += *amount;
                    let ai = t.damage_taken_by_ability.entry(ability_in).or_default();
                    if ai.kind.is_empty() {
                        ai.kind = kind_label(*kind).to_string();
                    }
                    ai.record_hit(*amount, crit);
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
                flags,
            } => {
                self.ensure_open(entry.timestamp);
                let player = self.player.clone();
                let enc = self.current.as_mut().unwrap();
                let name = ability.clone().unwrap_or_else(|| "melee".to_string());
                {
                    let s = enc.entry(source, &player);
                    s.misses += 1;
                    s.by_ability.entry(name.clone()).or_default().misses += 1;
                    // A flurry that misses is still a flurry.
                    for f in flags.names() {
                        *s.swings_by_flag.entry(f.to_string()).or_default() += 1;
                    }
                }
                if source.is_you() {
                    enc.note_timeline(entry.timestamp, &name, Outcome::Miss, 0, "melee");
                }
                enc.entry(target, &player).attacks_missed += 1;
                self.last_combat = entry.timestamp;
                enc.end = entry.timestamp;
            }
            Event::Avoided {
                source,
                target,
                how,
                flags,
            } => {
                self.ensure_open(entry.timestamp);
                let player = self.player.clone();
                let enc = self.current.as_mut().unwrap();
                {
                    let s = enc.entry(source, &player);
                    s.misses += 1;
                    *s.swings_stopped_by.entry(how.clone()).or_default() += 1;
                    // A flurry stopped by a parry still flurried.
                    for f in flags.names() {
                        *s.swings_by_flag.entry(f.to_string()).or_default() += 1;
                    }
                }
                {
                    let t = enc.entry(target, &player);
                    t.avoided += 1;
                    *t.avoided_by.entry(how.clone()).or_default() += 1;
                }
                // Counted as a miss on the timeline for the same reason the
                // meter counts it as one: from your side, the swing produced
                // nothing. The log names no ability on these lines.
                if source.is_you() {
                    enc.note_timeline(entry.timestamp, "melee", Outcome::Miss, 0, "melee");
                }
                self.last_combat = entry.timestamp;
                enc.end = entry.timestamp;
            }
            Event::Heal {
                source,
                target,
                amount,
                attempted,
                spell,
                over_time,
                flags,
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
                let crit = flags.is_crit();
                {
                    let s = enc.entry(source, &player);
                    s.healing_done += *amount;
                    s.heals += 1;
                    if crit {
                        s.heal_crits += 1;
                    }
                    // Only from lines that stated both numbers. Treating a
                    // one-number line as zero overheal would read as "none
                    // wasted" when the truth is that the client did not say.
                    if let Some(full) = attempted {
                        s.heals_measured_for_overheal += 1;
                        s.healing_attempted += *full;
                        s.over_healing += full.saturating_sub(*amount);
                    }
                    s.healing_by_spell
                        .entry(spell_name)
                        .or_default()
                        .record_hit(*amount, crit);
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
                // A creature killed by something of its own name is the same
                // proof a same-name attack is, and it is the last thing the
                // pair does — worth catching, since a short fight may produce
                // no attack line naming both.
                let shared_name = killer
                    .as_ref()
                    .is_some_and(|k| !k.is_you() && k.key() == victim.key());
                {
                    let v = enc.entry(victim, &player);
                    v.deaths += 1;
                    if shared_name {
                        v.name_shared = true;
                    }
                }
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
                // A corpse is not a pet. Nothing else would ever drop it: the
                // live set is deliberately kept across encounters, and a dead
                // creature casts no more spells to wear off, so without this
                // the charm outlives the kill for the rest of the session.
                self.end_charm(entry.timestamp, victim, false);
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
                marks: Vec::new(),
                id: self.next_id,
                zone: self.zone.clone(),
                start: timestamp,
                end: timestamp,
                actors: HashMap::new(),
                deaths: Vec::new(),
                loot: Vec::new(),
                coin_copper: 0,
                resists: HashMap::new(),
                timeline: Vec::new(),
                timeline_dropped: 0,
                timeline_retained: true,
            });
            self.next_id += 1;
            self.last_combat = timestamp;
            // A charm held across the gap between fights keeps running, but
            // its clock must restart here. Leaving it at the previous fight's
            // end counted the lull as charmed time: a 42-second pull reported
            // 73 charmed seconds, and `charm_uptime` capped that to a tidy
            // "100% of the fight, breaking 1 time" — a sentence that cannot be
            // true, sitting on top of a figure that was not either.
            for since in self.charm_since.values_mut() {
                *since = timestamp;
            }
            // The cast that started the pull happened before anything had been
            // damaged, so there was no encounter to count it against. Crediting
            // it here is the difference between "you cast this five times" and
            // four, every fight.
            let lead: Vec<(i64, String)> = self
                .pending_casts
                .drain(..)
                .filter(|(at, _)| timestamp - at <= CAST_LEAD_WINDOW)
                .collect();
            let player = self.player.clone();
            if let Some(enc) = self.current.as_mut() {
                for (_, spell) in lead {
                    enc.entry(&Actor::You, &player)
                        .by_ability
                        .entry(spell)
                        .or_default()
                        .casts += 1;
                }
            }
        }
    }

    /// Count a cast of yours against the fight it belongs to.
    ///
    /// A cast outside combat is held rather than counted, and never opens an
    /// encounter: buffing between pulls is not a fight, and letting a spell
    /// start one would fill the history with encounters in which nothing
    /// happened. It is credited only if a fight starts shortly afterwards.
    fn note_cast(&mut self, timestamp: i64, spell: &str) {
        let player = self.player.clone();
        match self.current.as_mut() {
            Some(enc) => {
                enc.entry(&Actor::You, &player)
                    .by_ability
                    .entry(spell.to_string())
                    .or_default()
                    .casts += 1;
            }
            None => {
                self.pending_casts.push((timestamp, spell.to_string()));
                let cutoff = timestamp - CAST_LEAD_WINDOW;
                self.pending_casts.retain(|(at, _)| *at >= cutoff);
            }
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
        // Charm survives the fight boundary in the game. The interval clock
        // does not: `ensure_open` restarts it at the next fight's first line,
        // so the lull in between is nobody's charmed time. Parking it at this
        // fight's end keeps the value sane for anything that reads it before
        // then.
        for since in self.charm_since.values_mut() {
            *since = ends_at.unwrap_or(*since);
        }

        // A stun still on you when the fight ends counts up to that moment —
        // the same reason a charm does. Dying stunned is the common case and
        // would otherwise report zero.
        if let (Some(since), Some(end)) = (self.stunned_since, ends_at) {
            let player = self.player.clone();
            if let Some(enc) = self.current.as_mut() {
                enc.entry(&Actor::You, &player).stunned_seconds += (end - since).max(0);
            }
            self.stunned_since = Some(end);
        }

        let mut enc = self.current.take()?;
        for st in enc.actors.values_mut() {
            fold_proc_casts(st);
        }
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

/// Fold a proc's casts onto the row its damage landed under.
///
/// A proc is cast by its ranked name and lands by its base one: the buff
/// Spirit of the Puma casts `Puma Maw II`, and the hit reads
/// "for 154 points of prismatic damage by Puma Maw". That produced two rows —
/// eighteen casts and no damage, eighteen hits and no casts — so the output
/// list carried a phantom ability doing nothing, and the real one looked as
/// though it had never been cast.
///
/// Only cast-only rows are folded, and only onto a row that took damage under
/// the base name. Most spells land under the name they were cast with —
/// Envenomed Bolt X does — and folding those would split one ability in two
/// rather than join two into one.
fn fold_proc_casts(st: &mut ActorStats) {
    let folds: Vec<(String, String)> = st
        .by_ability
        .iter()
        .filter(|(_, a)| a.casts > 0 && a.total == 0 && a.hits == 0)
        .filter_map(|(name, _)| {
            let base = crate::spell::base_spell_name(name);
            if base == name.as_str() {
                return None;
            }
            let base = base.to_string();
            st.by_ability
                .get(&base)
                .filter(|t| t.total > 0)
                .map(|_| (name.clone(), base))
        })
        .collect();
    for (ranked, base) in folds {
        let Some(from) = st.by_ability.remove(&ranked) else {
            continue;
        };
        if let Some(to) = st.by_ability.get_mut(&base) {
            to.casts += from.casts;
        }
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

    /// The reported case: with a charmed pet, "damage to enemy" read higher
    /// than everything the player and the pet had dealt.
    ///
    /// A charmed mob keeps `damaged_by_you` from the pull that landed the
    /// charm, so every hit the boss put into the pet was being scored as
    /// damage into the enemy. Damage dealt to it *before* the charm is a
    /// different matter and still counts — then it really was the enemy.
    #[test]
    fn a_boss_beating_on_your_charmed_pet_is_not_damage_to_the_enemy() {
        let mut t = EncounterTracker::new("Tharn");
        let imp = Actor::Named("An imp protector".into());
        let push = |t: &mut EncounterTracker, ts: i64, line: &str| {
            for e in &entries(&[(ts, line)]) {
                t.push(e);
            }
        };
        // You soften the imp up, then charm it.
        push(&mut t, 100, "You slash An imp protector for 70 points of damage.");
        t.set_charmed(101, &imp, true);
        // Now it fights for you, and the boss turns on it.
        push(&mut t, 102, "You slash a fire giant for 100 points of damage.");
        push(
            &mut t,
            103,
            "An imp protector slashes a fire giant for 400 points of damage.",
        );
        push(&mut t, 104, "A fire giant hits An imp protector for 250 points of damage.");
        push(&mut t, 105, "A fire giant hits YOU for 50 points of damage.");
        let enc = t.flush().expect("encounter");

        // The giant absorbed 500; the imp absorbed 70 while it was still an
        // enemy. The 250 it took as your pet is not damage into the enemy.
        assert_eq!(
            enc.damage_to_enemies(),
            570,
            "500 into the giant, 70 into the imp before it was charmed"
        );
        // And it is not more than your side actually dealt.
        assert!(
            enc.damage_to_enemies() <= enc.your_damage_with_pets() + 70,
            "to_enemies {} must not exceed what you and the pet dealt",
            enc.damage_to_enemies()
        );
        // Your side absorbed the rest: your 50 plus the pet's 250.
        assert_eq!(enc.damage_to_allies(), 300);
        // Nothing is lost between the two halves.
        assert_eq!(
            enc.damage_to_enemies() + enc.damage_to_allies(),
            enc.total_damage(),
            "every point lands on one side or the other"
        );
    }

    /// The headline is your side's output, not everything the boss absorbed.
    /// Another player pounding the same target must not inflate your number.
    #[test]
    fn damage_to_enemies_is_your_side_only() {
        let encs = run(&[
            (100, "You slash a giant for 100 points of damage."),
            (101, "Kallous slashes a giant for 900 points of damage."),
            (110, "You have slain a giant!"),
        ]);
        let e = &encs[0];
        assert_eq!(e.damage_to_enemies(), 100, "yours, not Kallous's");
        assert_eq!(
            e.damage_absorbed_by_enemies(),
            1000,
            "the giant did absorb all of it, and that is still available"
        );
    }

    /// Cannibalization trades health for mana. It is damage, but it was never
    /// aimed at anything, so it is neither damage to the enemy nor a spell in
    /// the output breakdown.
    #[test]
    fn self_inflicted_damage_is_not_damage_to_the_enemy() {
        let encs = run(&[
            (100, "You slash a giant for 100 points of damage."),
            (
                101,
                "You hit yourself for 60 points of unresistable damage by Cannibalization I.",
            ),
            (110, "You have slain a giant!"),
        ]);
        let e = &encs[0];
        let you = e.actors.get("you").unwrap();
        assert_eq!(you.damage_done, 160, "it is still damage you dealt");
        assert_eq!(you.self_damage, 60);
        assert_eq!(e.damage_to_enemies(), 100, "but none of it reached a giant");

        let cann = you.by_ability.get("Cannibalization I").expect("recorded");
        assert!(cann.self_inflicted);
        assert_eq!(
            ability_group(&cann.kind, cann.self_inflicted),
            "Other",
            "a cost, not output"
        );
        let slash = you.by_ability.get("slash").or_else(|| you.by_ability.get("melee"));
        if let Some(sl) = slash {
            assert!(!sl.self_inflicted);
            assert_eq!(ability_group(&sl.kind, sl.self_inflicted), "Melee");
        }
    }

    /// A proc is cast under its ranked name and lands under its base one, so
    /// it was being reported as two abilities: one that was cast and never
    /// hit, one that hit and was never cast.
    #[test]
    fn a_procs_casts_land_on_the_row_its_damage_did() {
        let encs = run(&[
            (100, "You begin casting Puma Maw II."),
            (101, "You hit a giant for 154 points of prismatic damage by Puma Maw."),
            (102, "You begin casting Puma Maw II."),
            (103, "You hit a giant for 154 points of prismatic damage by Puma Maw."),
            // A spell that lands under the name it was cast with must not be
            // folded — that would split one ability into two.
            (104, "You begin casting Envenomed Bolt X."),
            (105, "You hit a giant for 400 points of poison damage by Envenomed Bolt X."),
            (110, "You have slain a giant!"),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        assert!(
            !you.by_ability.contains_key("Puma Maw II"),
            "no phantom row: {:?}",
            you.by_ability.keys().collect::<Vec<_>>()
        );
        let puma = you.by_ability.get("Puma Maw").expect("the proc");
        assert_eq!((puma.hits, puma.casts, puma.total), (2, 2, 308));

        let bolt = you.by_ability.get("Envenomed Bolt X").expect("unfolded");
        assert_eq!((bolt.hits, bolt.casts, bolt.total), (1, 1, 400));
        assert!(!you.by_ability.contains_key("Envenomed Bolt"));
    }

    /// A pet's procs are most of what it contributes, so its casts count too.
    #[test]
    fn a_pets_casts_are_recorded_like_your_own() {
        let encs = run(&[
            (100, "Venobtik says, 'My leader is Tharn.'"),
            (101, "Venobtik slashes a giant for 40 points of damage."),
            (102, "Venobtik begins casting Puma Maw II."),
            (103, "Venobtik hit a giant for 154 points of prismatic damage by Puma Maw."),
            (110, "You have slain a giant!"),
        ]);
        let pet = encs[0].actors.get("venobtik").expect("the pet");
        let puma = pet.by_ability.get("Puma Maw").expect("its proc");
        assert_eq!((puma.hits, puma.casts, puma.total), (1, 1, 154));

        // A creature that is not yours contributes nothing here.
        let encs = run(&[
            (100, "You slash a giant for 10 points of damage."),
            (101, "A wandering shaman begins casting Puma Maw II."),
            (110, "You have slain a giant!"),
        ]);
        assert!(
            !encs[0].actors.contains_key("a wandering shaman"),
            "a passing caster is not a combatant"
        );
    }

    /// Two creatures with one name are one row, and nothing in the log can
    /// separate them. What the log does say is that they were two.
    #[test]
    fn a_creature_fighting_its_own_name_marks_the_row_as_shared() {
        let encs = run(&[
            (100, "You slash a fire giant warrior for 100 points of damage."),
            // Your charmed one, fighting another of the same name. The log
            // writes both as the same actor.
            (
                101,
                "A fire giant warrior hits a fire giant warrior for 196 points of damage.",
            ),
            (110, "You have slain a fire giant warrior!"),
        ]);
        let giant = encs[0].actors.get("a fire giant warrior").unwrap();
        assert!(giant.name_shared, "the row pools at least two creatures");

        // A creature hitting something else is not evidence of anything.
        let encs = run(&[
            (100, "You slash a giant for 10 points of damage."),
            (101, "A giant hits a rat for 5 points of damage."),
            (110, "You have slain a giant!"),
        ]);
        assert!(!encs[0].actors.get("a giant").unwrap().name_shared);
        assert!(!encs[0].actors.get("a rat").unwrap().name_shared);
    }

    /// A short fight may produce no attack line naming both, but the kill line
    /// still does.
    #[test]
    fn a_creature_slain_by_its_own_name_marks_the_row_as_shared() {
        let encs = run(&[
            (100, "You slash an imp protector for 10 points of damage."),
            (101, "An imp protector has been slain by an imp protector!"),
        ]);
        let imp = encs[0].actors.get("an imp protector").unwrap();
        assert!(imp.name_shared);

        // Your own killing blow says nothing about how many there were.
        let encs = run(&[
            (100, "You slash a rat for 10 points of damage."),
            (101, "A rat has been slain by Tharn!"),
        ]);
        assert!(!encs[0].actors.get("a rat").unwrap().name_shared);
    }

    /// A charm held across the gap between two fights must not bill the gap to
    /// the second one.
    ///
    /// The clock used to be parked at the first fight's end and picked up
    /// again by the second, so the lull counted as charmed time — 73 charmed
    /// seconds inside a 42-second pull, capped on the way out to a flat 100%
    /// and printed as "held 100% of the fight, breaking 1 time", which cannot
    /// be true.
    #[test]
    fn a_charm_across_a_lull_does_not_bill_the_lull() {
        let mut t = EncounterTracker::new("Tharn");
        let imp = Actor::Named("An imp protector".into());
        let hit = |t: &mut EncounterTracker, ts: i64| {
            for e in &entries(&[(ts, "You slash a rat for 1 point of damage.")]) {
                t.push(e);
            }
        };
        // A short fight with the charm up throughout.
        hit(&mut t, 100);
        t.set_charmed(100, &imp, true);
        hit(&mut t, 110);
        let first = t.flush().expect("first fight");
        assert_eq!(first.duration(), 10);
        assert_eq!(
            first.actors.get("an imp protector").unwrap().charmed_seconds,
            10
        );

        // Five minutes of nothing, then another short fight with the same
        // charm still held and the pet swinging in it — a charmed pet that
        // does nothing gets no row, so it has to act for there to be anything
        // to bill.
        hit(&mut t, 410);
        for e in &entries(&[(
            415,
            "An imp protector slashes a rat for 5 points of damage.",
        )]) {
            t.push(e);
        }
        hit(&mut t, 420);
        let second = t.flush().expect("second fight");
        assert_eq!(second.duration(), 10);
        let held = second.actors.get("an imp protector").unwrap().charmed_seconds;
        assert_eq!(held, 10, "the five-minute lull is not charmed time");
        assert!(
            second.charm_uptime() <= 1.0,
            "and the uptime needs no cap to stay sane"
        );
    }

    /// Time stunned is time not attacking, and the client states the start and
    /// the end as a pair, so the span is measurable rather than guessed.
    #[test]
    fn a_stun_is_counted_as_the_time_it_cost() {
        let encs = run(&[
            (100, "You slash a giant for 10 points of damage."),
            (102, "You are stunned!"),
            (108, "You are no longer stunned."),
            (109, "You avoid the stunning blow."),
            (110, "You slash a giant for 10 points of damage."),
            (112, "You have slain a giant!"),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        assert_eq!(you.stunned_seconds, 6, "102 to 108");
        assert_eq!(you.stuns, 1);
        assert_eq!(you.stuns_avoided, 1);

        // Overcoming it early ends the span just the same.
        let encs = run(&[
            (100, "You slash a giant for 10 points of damage."),
            (101, "You are stunned!"),
            (103, "You overcome the stun!"),
            (110, "You have slain a giant!"),
        ]);
        assert_eq!(encs[0].actors.get("you").unwrap().stunned_seconds, 2);

        // A stun still on you when the fight ends counts up to that moment.
        let encs = run(&[
            (100, "You slash a giant for 10 points of damage."),
            (104, "You are stunned!"),
            (110, "A giant hits YOU for 10 points of damage."),
        ]);
        assert_eq!(
            encs[0].actors.get("you").unwrap().stunned_seconds,
            6,
            "104 to the fight's end at 110"
        );
    }

    /// A mob that died early in a long pull keeps its row, so a live meter
    /// needs to know how long ago each actor last did anything.
    #[test]
    fn an_actor_records_when_it_last_did_something() {
        let encs = run(&[
            (100, "You slash a rat for 10 points of damage."),
            (101, "A rat hits YOU for 5 points of damage."),
            (102, "You have slain a rat!"),
            // The fight carries on against something else. Kept inside the
            // 12-second idle timeout, or this would be two encounters and the
            // point — one fight, one row gone quiet — would be lost.
            (110, "You slash a giant for 40 points of damage."),
            (118, "A giant hits YOU for 20 points of damage."),
            (120, "You have slain a giant!"),
        ]);
        let e = &encs[0];
        let rat = e.actors.get("a rat").unwrap();
        let giant = e.actors.get("a giant").unwrap();
        let you = e.actors.get("you").unwrap();

        assert_eq!(encs.len(), 1, "one fight, or the test proves nothing");
        assert_eq!(rat.last_action, 101, "its last blow, not the fight's end");
        assert_eq!(giant.last_action, 118);
        assert_eq!(you.last_action, 118, "both dealing and taking count");

        // Which is what a view fades against: the rat has been quiet for
        // nineteen of the fight's twenty seconds, the giant for two.
        assert_eq!(e.end - rat.last_action, 19);
        assert_eq!(e.end - giant.last_action, 2);
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

    /// The crit-heal line used to match no rule, so this is as much about the
    /// 306 reaching `healing_done` as about the crit being counted.
    #[test]
    fn crit_heals_are_counted_and_kept_apart_from_melee_crits() {
        let encs = run(&[
            (100, "You slash a mob for 10 points of damage. (Critical)"),
            (
                101,
                "You healed Tharn over time for 306 hit points by Efflorescing Heal. (Critical)",
            ),
            (
                102,
                "You healed Tharn for 100 hit points by Efflorescing Heal.",
            ),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        assert_eq!(you.healing_done, 406, "the flagged heal must still count");
        assert_eq!(you.heals, 2);
        assert_eq!(you.heal_crits, 1);
        assert!((you.heal_crit_rate() - 0.5).abs() < f64::EPSILON);
        // The melee crit is a separate mechanic and must not be mixed in.
        assert_eq!(you.crits, 1);
        let eh = you.healing_by_spell.get("Efflorescing Heal").unwrap();
        assert_eq!(eh.hits, 2);
        assert_eq!(eh.crits, 1);
    }

    /// Overhealing is only claimed for lines that stated both numbers.
    #[test]
    fn overhealing_is_measured_over_a_stated_denominator() {
        let encs = run(&[
            (100, "You slash a mob for 10 points of damage."),
            (
                101,
                "Jabarab healed himself for 47 (241) hit points by Lay on Hands.",
            ),
            (102, "Jabarab healed himself for 0 (6) hit points by Lifetap."),
            // No pre-cap figure: it must not be assumed to have wasted nothing.
            (
                103,
                "Jabarab healed himself for 90 hit points by Healing Water.",
            ),
        ]);
        let j = encs[0].actors.get("jabarab").unwrap();
        assert_eq!(j.healing_done, 137);
        assert_eq!(j.heals, 3);
        assert_eq!(j.healing_attempted, 247);
        assert_eq!(j.over_healing, 194 + 6);
        assert_eq!(
            j.heals_measured_for_overheal, 2,
            "the third heal said nothing, so it is not in the denominator"
        );
    }

    /// A slay is a crit, and it is separable from an ordinary critical.
    #[test]
    fn crits_are_tallied_by_mechanism() {
        let encs = run(&[
            (100, "You slash a mob for 10 points of damage. (Critical)"),
            (101, "You slash a mob for 900 points of damage. (Slay Undead)"),
            (
                102,
                "You slash a mob for 800 points of damage. (Riposte Slay Undead)",
            ),
            (103, "You slash a mob for 5 points of damage."),
        ]);
        let you = encs[0].actors.get("you").unwrap();
        assert_eq!(you.hits, 4);
        assert_eq!(you.crits, 3, "both slay hits are criticals");
        assert_eq!(you.crits_by_kind.get("melee").copied(), Some(3));
        assert_eq!(
            you.crit_causes(),
            vec![("Slay Undead".to_string(), 2), ("Critical".to_string(), 1)]
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
            // It is an enemy first. That is what tells it apart from a pet you
            // summoned, which says the same words when ordered to attack.
            (99, "An imp protector hits YOU for 20 points of damage."),
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
        assert_eq!(e.your_pets().len(), 1);
        // A "Master" line alone would not prove a charm — a summoned pet says
        // the same thing. One from a creature that was hitting you moments
        // earlier does, so this fight belongs on the charm-specific list.
        assert_eq!(e.charmed_pets().len(), 1);
        assert!(e.actors.get("an imp protector").unwrap().charm_seen);
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
            (99, "An imp protector hits YOU for 5 points of damage."),
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

    /// What the overlay reads must empty out when the charm goes, even though
    /// the encounter's own flag is sticky and deliberately does not.
    #[test]
    fn breaking_a_charm_empties_the_live_set() {
        let mut t = EncounterTracker::new("Tharn");
        let imp = Actor::Named("An imp protector".into());
        for e in &entries(&[(100, "You slash a rat for 1 point of damage.")]) {
            t.push(e);
        }
        t.set_charmed(100, &imp, true);
        assert_eq!(t.charmed_now(), vec!["An imp protector".to_string()]);

        t.set_charmed(106, &imp, false);
        assert!(t.charmed_now().is_empty(), "the break clears the panel");
        assert!(
            t.current().unwrap().actors["an imp protector"].charmed_by_you,
            "while the sticky flag survives, so the damage stays credited"
        );
    }

    /// Charming three creatures over a fight and losing each one must not leave
    /// three names on screen. This is the shape of the bug the live set exists
    /// to fix: the sticky flag accumulated every one of them.
    #[test]
    fn successive_charms_do_not_pile_up_on_the_overlay() {
        let mut t = EncounterTracker::new("Tharn");
        for e in &entries(&[(100, "You slash a rat for 1 point of damage.")]) {
            t.push(e);
        }
        for (i, name) in ["An imp protector", "A cave bat", "A dark elf"]
            .iter()
            .enumerate()
        {
            let who = Actor::Named((*name).into());
            let at = 100 + i as i64 * 2;
            t.set_charmed(at, &who, true);
            t.set_charmed(at + 1, &who, false);
        }
        assert!(
            t.charmed_now().is_empty(),
            "three charms held one at a time, none of them still held: {:?}",
            t.charmed_now()
        );
    }

    /// A charmed pet that dies stops being a pet — and its death is not a charm
    /// break. Nothing else clears the live set, which is kept across
    /// encounters, so a corpse left in it stays for the session.
    #[test]
    fn a_charmed_pet_dying_ends_the_charm_without_counting_a_break() {
        let mut t = EncounterTracker::new("Tharn");
        let imp = Actor::Named("An imp protector".into());
        for e in &entries(&[(100, "You slash a rat for 1 point of damage.")]) {
            t.push(e);
        }
        t.set_charmed(100, &imp, true);
        for e in &entries(&[(106, "An imp protector has been slain by a rat!")]) {
            t.push(e);
        }
        assert!(t.charmed_now().is_empty(), "the corpse leaves the panel");

        for e in &entries(&[(108, "You slash a rat for 1 point of damage.")]) {
            t.push(e);
        }
        let enc = t.flush().expect("encounter");
        let st = enc.actors.get("an imp protector").expect("pet recorded");
        assert_eq!(st.charmed_seconds, 6, "the clock still closes at the death");
        assert_eq!(st.charm_breaks, 0, "dying is not the charm failing");
    }

    /// Zoning ends every charm. This is the only thing that ever clears one
    /// known solely from a "Master" line — no spell was named, so no spell can
    /// wear off — and without it such a charm is held for the whole session.
    #[test]
    fn zoning_lets_go_of_every_charm() {
        let mut t = EncounterTracker::new("Tharn");
        for e in &entries(&[
            (99, "An imp protector hits YOU for 10 points of damage."),
            (100, "You slash a rat for 1 point of damage."),
            (101, "An imp protector told you, 'Attacking a rat Master.'"),
        ]) {
            t.push(e);
        }
        t.set_charmed(102, &Actor::Named("A cave bat".into()), true);
        assert_eq!(t.charmed_now().len(), 2, "{:?}", t.charmed_now());

        for e in &entries(&[(400, "You have entered Lavastorm Mountains.")]) {
            t.push(e);
        }
        assert!(
            t.charmed_now().is_empty(),
            "nothing follows you across the line: {:?}",
            t.charmed_now()
        );
    }

    /// A charm known only from a "Master" line must still be timed. No spell
    /// was named, so nothing can wear off — before the clock started here,
    /// uptime and the break count read zero for a whole session of charming.
    #[test]
    fn a_master_line_charm_is_timed_like_any_other() {
        let encs = run(&[
            (100, "An imp protector hits YOU for 20 points of damage."),
            (101, "An imp protector told you, 'Attacking a rat Master.'"),
            (
                104,
                "An imp protector slashes a rat for 30 points of damage.",
            ),
            (112, "You slash a rat for 1 point of damage."),
        ]);
        let st = encs[0].actors.get("an imp protector").expect("pet");
        assert_eq!(st.charmed_seconds, 11, "101 to the end of the fight");
        assert!(encs[0].charm_uptime() > 0.9, "{}", encs[0].charm_uptime());
    }

    /// The pet repeats its attack order constantly. Each one must not restart
    /// the clock, or a charm held all fight reports a couple of seconds.
    #[test]
    fn repeated_attack_orders_do_not_restart_the_clock() {
        let encs = run(&[
            (100, "An imp protector hits YOU for 20 points of damage."),
            (101, "An imp protector told you, 'Attacking a rat Master.'"),
            (105, "An imp protector told you, 'Attacking a rat Master.'"),
            (109, "An imp protector told you, 'Attacking a rat Master.'"),
            (112, "You slash a rat for 1 point of damage."),
        ]);
        let st = encs[0].actors.get("an imp protector").expect("pet");
        assert_eq!(st.charmed_seconds, 11, "held from the first order, not the last");
    }

    /// A creature you hold does not attack you. When one does, that is the
    /// break — the only line saying so when no spell can be named.
    #[test]
    fn a_held_creature_hitting_you_is_the_break() {
        let encs = run(&[
            (99, "An imp protector hits YOU for 10 points of damage."),
            (100, "You slash a rat for 1 point of damage."),
            (101, "An imp protector told you, 'Attacking a rat Master.'"),
            (
                104,
                "An imp protector slashes a rat for 30 points of damage.",
            ),
            // The charm comes loose and it turns on you.
            (106, "An imp protector hits YOU for 50 points of damage."),
            (112, "You slash a rat for 1 point of damage."),
        ]);
        let st = encs[0].actors.get("an imp protector").expect("pet");
        assert_eq!(st.charm_breaks, 1);
        assert_eq!(st.charmed_seconds, 5, "101 to 106, and no longer");
        assert_eq!(
            st.damage_while_charmed, 30,
            "the blow that broke it is not your pet's work"
        );
        assert_eq!(st.damage_done, 90, "though it is still its own damage");
    }

    /// A summoned pet answers "Master" in the same words a charmed mob does,
    /// and its generated name matches no convention. What it never does is
    /// fight you — so it must not reach the charm overlay, which is where two
    /// of these sat for weeks.
    #[test]
    fn a_summoned_pet_that_never_fought_you_is_not_a_charm() {
        let mut t = EncounterTracker::new("Tharn");
        for e in &entries(&[
            (100, "You slash a giant spider for 10 points of damage."),
            (101, "Janer told you, 'Attacking a giant spider Master.'"),
            (102, "Janer slashes a giant spider for 90 points of damage."),
        ]) {
            t.push(e);
        }
        assert!(
            t.charmed_now().is_empty(),
            "nothing here says this was ever an enemy: {:?}",
            t.charmed_now()
        );

        let enc = t.flush().expect("encounter");
        let janer = enc.actors.get("janer").expect("the pet");
        assert!(!janer.charm_seen, "and no charm may be claimed");
        assert!(janer.is_yours(), "though it plainly fights for you");
        assert_eq!(
            enc.your_damage_with_pets(),
            100,
            "so all of its damage counts, not the zero seconds it was charmed"
        );
    }

    /// `/pet leader` answers for a charmed mob too — the pet window lists it.
    /// Reading that as proof of summoning stopped its charm being counted.
    #[test]
    fn pet_leader_on_a_charmed_mob_does_not_make_it_summoned() {
        let mut t = EncounterTracker::new("Tharn");
        for e in &entries(&[
            (99, "A fire giant warrior hits YOU for 10 points of damage."),
            (100, "You slash a rat for 1 point of damage."),
            (101, "A fire giant warrior told you, 'Attacking a rat Master.'"),
            (102, "A fire giant warrior says, 'My leader is Tharn.'"),
        ]) {
            t.push(e);
        }
        assert_eq!(
            t.charmed_now(),
            vec!["A fire giant warrior".to_string()],
            "still held after the command"
        );
        let st = &t.current().unwrap().actors["a fire giant warrior"];
        assert!(!st.summoned, "the pet window does not make it summoned");
        assert!(st.charm_seen, "and the charm still reads as a charm");
        assert_eq!(st.owner.as_deref(), Some("Tharn"), "whose it is, though");
    }

    /// A generated-name pet that never fought you is summoned, and the command
    /// is the only thing that can say so.
    #[test]
    fn pet_leader_still_identifies_a_summoned_pet() {
        let mut t = EncounterTracker::new("Brennagh");
        for e in &entries(&[
            (100, "You slash a rat for 1 point of damage."),
            (101, "Lebekab says, 'My leader is Brennagh.'"),
        ]) {
            t.push(e);
        }
        let st = &t.current().unwrap().actors["lebekab"];
        assert!(st.summoned, "nothing suggests a charm, so it is a summon");
        assert_eq!(st.owner.as_deref(), Some("Brennagh"));
    }

    /// Casts are counted per spell, and a cast is not a hit: one cast of a
    /// dot lands many ticks, and a resisted one lands none.
    #[test]
    fn casts_are_counted_separately_from_what_they_landed() {
        let encs = run(&[
            (100, "You begin casting Envenomed Bolt VI."),
            (101, "You hit a rat for 300 points of magic damage by Envenomed Bolt VI."),
            (104, "You begin casting Envenomed Bolt VI."),
            (105, "You hit a rat for 280 points of magic damage by Envenomed Bolt VI."),
            (106, "A rat has taken 40 damage from your Envenomed Bolt VI."),
            (108, "A rat has taken 40 damage from your Envenomed Bolt VI."),
        ]);
        let you = encs[0].actors.get("you").expect("you");
        let a = you.by_ability.get("Envenomed Bolt VI").expect("the spell");
        assert_eq!(a.casts, 2, "two casts");
        assert_eq!(a.hits, 4, "which landed twice and ticked twice");
    }

    /// The cast that opens a pull happens before anything has been damaged, so
    /// there is no fight for it to belong to yet. It must not be lost.
    #[test]
    fn the_cast_that_opens_a_fight_is_counted_in_it() {
        let encs = run(&[
            (100, "You begin casting Drifting Death V."),
            (103, "A rat has taken 90 damage from your Drifting Death V."),
            (106, "You slash a rat for 10 points of damage."),
        ]);
        let a = encs[0].actors["you"].by_ability.get("Drifting Death V");
        assert_eq!(a.map(|a| a.casts), Some(1), "the opener counts");
    }

    /// Buffing between pulls is not a fight, and must not open one.
    #[test]
    fn a_cast_outside_combat_opens_no_encounter() {
        let mut t = EncounterTracker::new("Tharn");
        for e in &entries(&[
            (100, "You begin singing Chorus of Replenishment."),
            (140, "You begin singing Chorus of Replenishment."),
        ]) {
            t.push(e);
        }
        assert!(t.current().is_none(), "no fight was started by singing");
        assert!(t.flush().is_none(), "and none is produced at the end");
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
            (99, "An imp protector hits YOU for 10 points of damage."),
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

    /// A summoned pet answers "Attacking … Master." exactly as a charmed mob
    /// does. Before pets were told apart, that line marked a warder as charmed
    /// — reporting a charm the character never cast.
    #[test]
    fn a_summoned_pet_is_not_charmed() {
        let encs = run(&[
            (100, "Tharn`s warder told you, 'Attacking a giant Master.'"),
            (101, "Tharn`s warder slashes a giant for 60 points of damage."),
            (102, "You slash a giant for 40 points of damage."),
            (110, "You have slain a giant!"),
        ]);
        let e = &encs[0];
        let pet = e.actors.get("tharn`s warder").expect("the warder");
        assert!(pet.summoned, "a warder is summoned");
        assert!(!pet.charmed_by_you, "and must not read as charmed");
        assert!(pet.owned_by_you);
        assert_eq!(pet.owner.as_deref(), Some("Tharn"));
        // All of a summoned pet's output is its owner's: it never was an enemy.
        assert_eq!(pet.credited_damage(), 60);
        assert_eq!(e.your_damage_with_pets(), 100);
        // ...and it is not counted as a charm anywhere.
        assert!(e.charmed_pets().is_empty());
        assert_eq!(e.charm_uptime(), 0.0);
    }

    /// A charm confirmed by the caller — which is how the daemon reports one,
    /// having resolved the spell against the database — still reads as a
    /// charm, and still counts only what was dealt while held.
    #[test]
    fn a_confirmed_charm_is_still_charmed() {
        let mut t = EncounterTracker::new("Tharn");
        let imp = Actor::parse("An imp protector");
        for e in entries(&[
            (100, "You slash a giant for 10 points of damage."),
            (101, "An imp protector slashes a giant for 5 points of damage."),
        ]) {
            t.push(&e);
        }
        // The resolver decides a charm of yours landed.
        t.set_charmed(102, &imp, true);
        for e in entries(&[
            (103, "An imp protector slashes a giant for 70 points of damage."),
            (110, "You have slain a giant!"),
        ]) {
            t.push(&e);
        }
        let e = t.flush().expect("a fight");
        let stats = e.actors.get("an imp protector").expect("the imp");
        assert!(stats.charm_seen, "a resolved charm is a charm");
        assert!(!stats.summoned);
        // Only the 70 dealt after the charm landed; the earlier 5 was an
        // enemy's damage and stays that way.
        assert_eq!(stats.credited_damage(), 70);
        assert_eq!(e.charmed_pets().len(), 1);
        assert_eq!(e.your_damage_with_pets(), 80);
    }

    /// `/pet leader` is the only line that attributes a pet whose name says
    /// nothing — and it works for other people's pets too.
    #[test]
    fn pet_leader_attributes_someone_elses_pet() {
        let encs = run(&[
            (100, "Gabeker says, 'My leader is Brennagh.'"),
            (101, "Gabeker slashes a giant for 80 points of damage."),
            (102, "You slash a giant for 20 points of damage."),
            (110, "You have slain a giant!"),
        ]);
        let e = &encs[0];
        let pet = e.actors.get("gabeker").expect("the pet");
        assert_eq!(pet.owner.as_deref(), Some("Brennagh"));
        assert!(pet.summoned);
        assert!(!pet.owned_by_you, "it is not the logging character's");
        // Somebody else's pet must not inflate your damage.
        assert_eq!(e.your_damage_with_pets(), 20);
        assert_eq!(e.pets_of("Brennagh").len(), 1);
    }

    /// The attribution has to outlive the pull it was learned in: `/pet
    /// leader` gets typed once an evening, if at all.
    #[test]
    fn pet_ownership_carries_into_later_fights() {
        let encs = run(&[
            (100, "Gabeker says, 'My leader is Tharn.'"),
            (101, "Gabeker slashes a giant for 10 points of damage."),
            (110, "You have slain a giant!"),
            // Well past the idle timeout: a separate fight.
            (400, "Gabeker slashes a rat for 90 points of damage."),
            (405, "You have slain a rat!"),
        ]);
        assert_eq!(encs.len(), 2);
        let later = encs[1].actors.get("gabeker").expect("the pet, later");
        assert_eq!(later.owner.as_deref(), Some("Tharn"));
        assert!(later.owned_by_you);
        assert_eq!(encs[1].your_damage_with_pets(), 90);
    }

    /// The reported case: a mob charmed weeks ago was still being credited as
    /// your pet while it fought you.
    ///
    /// Carrying pet attribution forward is right for a summoned pet, which is
    /// yours until it dies. A charmed mob gets the same record and the charm
    /// ends long before the log does — a flighty fiend charmed on 31 July was
    /// still filed as the player's pet on 19 August, appearing as their pet in
    /// a fight where it was hitting them, while the actual summoned pet went
    /// unlisted.
    #[test]
    fn a_former_charm_that_turns_on_you_stops_being_your_pet() {
        let encs = run(&[
            // It announces itself while charmed, and fights for you.
            (100, "A flighty fiend told you, 'Attacking a giant Master.'"),
            (101, "A flighty fiend slashes a giant for 40 points of damage."),
            (110, "You have slain a giant!"),
            // A separate fight, weeks later in log time. The charm is long
            // gone and it is an enemy again.
            (9000, "You slash a flighty fiend for 30 points of damage."),
            (9001, "A flighty fiend hits YOU for 25 points of damage."),
            (9002, "A flighty fiend slashes Master Yael for 10 points of damage."),
            (9010, "You have slain a flighty fiend!"),
        ]);
        assert_eq!(encs.len(), 2);

        let then = encs[0].actors.get("a flighty fiend").expect("the pet");
        assert!(then.is_yours(), "it really was yours at the time");
        assert_eq!(encs[0].your_damage_with_pets(), 40);

        let now = encs[1].actors.get("a flighty fiend").expect("the enemy");
        assert!(!now.owned_by_you, "it turned on you; it is not your pet");
        assert_eq!(now.owner, None);
        assert!(
            !encs[1].your_pets().iter().any(|p| p.name.to_lowercase() == "a flighty fiend"),
            "and it is not listed among your pets"
        );
        // Its swing at the boss is not your damage either.
        assert_eq!(encs[1].your_damage_with_pets(), 30, "only your own 30");
    }

    /// `/pet leader` answers for a charmed mob too, so the summoned verdict
    /// it produces can be wrong — and being wrong used to be permanent.
    ///
    /// The reported case: a fire giant warrior charmed on 12 August was asked
    /// `/pet leader` just after its charm lapsed, so it was branded summoned
    /// for good. A week later it was still listed as the player's pet, in a
    /// Nagafen's Lair fight where it was hitting them.
    #[test]
    fn a_mistaken_summoned_brand_does_not_survive_being_attacked() {
        let encs = run(&[
            // Charmed, announced, then the charm lapses and `/pet leader` is
            // typed — which files it as summoned.
            (100, "A fire giant warrior told you, 'Attacking a giant Master.'"),
            (101, "A fire giant warrior slashes a giant for 40 points of damage."),
            (150, "A fire giant warrior hits YOU for 10 points of damage."),
            (151, "A fire giant warrior says, 'My leader is Tharn.'"),
            (160, "You have slain a giant!"),
            // A later fight: it is an enemy, not a pet.
            (9000, "You slash a fire giant warrior for 30 points of damage."),
            (9001, "A fire giant warrior hits YOU for 25 points of damage."),
            (9002, "A fire giant warrior slashes Warlord Skarlon for 15 points of damage."),
            (9010, "You have slain a fire giant warrior!"),
        ]);
        // The idle gaps split this into three: the charmed pull, the one
        // where `/pet leader` was typed, and the fight a week later.
        let last = encs.last().expect("the later fight");
        let now = last.actors.get("a fire giant warrior").expect("the enemy");
        assert!(!now.owned_by_you, "it is fighting you, not for you");
        assert!(!now.summoned, "the brand was inferred, and it was wrong");
        assert_eq!(now.owner, None);
        assert!(
            !last.your_pets().iter().any(|p| p.name.to_lowercase().contains("fire giant")),
            "and it is not listed among your pets"
        );
        assert_eq!(last.your_damage_with_pets(), 30, "only your own 30");
    }

    /// Another player's pet hitting you says nothing about whose it is.
    #[test]
    fn being_hit_by_someone_elses_pet_does_not_disown_it() {
        let encs = run(&[
            (100, "Gabeker says, 'My leader is Kallous.'"),
            (101, "Gabeker slashes a giant for 10 points of damage."),
            (102, "Gabeker hits YOU for 5 points of damage."),
            (110, "You have slain a giant!"),
        ]);
        let g = encs[0].actors.get("gabeker").expect("their pet");
        assert_eq!(g.owner.as_deref(), Some("Kallous"), "still theirs");
        assert!(!g.owned_by_you);
    }

    /// A pet with a generated name that was never asked `/pet leader` is
    /// simply "yours" — calling it charmed would invent a spell nobody cast,
    /// and calling it summoned would be a guess in the other direction.
    #[test]
    fn a_pet_of_unknown_kind_claims_neither() {
        let encs = run(&[
            (100, "Xibektik told you, 'Attacking a giant Master.'"),
            (101, "Xibektik slashes a giant for 50 points of damage."),
            (110, "You have slain a giant!"),
        ]);
        let pet = encs[0].actors.get("xibektik").expect("the pet");
        assert!(pet.is_yours(), "it fights for you");
        assert!(!pet.summoned, "nothing said it was summoned");
        assert!(!pet.charm_seen, "and no charm was ever seen to land");
        // The damage still counts as yours; only the label is withheld.
        assert_eq!(encs[0].your_damage_with_pets(), 50);
    }

    /// A mob whose name merely looks possessive is not somebody's pet.
    #[test]
    fn a_possessive_mob_name_is_not_a_pet() {
        assert_eq!(crate::event::pet_owner_in_name("Innoruuk`s Chosen"), None);
        assert_eq!(
            crate::event::pet_owner_in_name("Brennagh`s warder"),
            Some("Brennagh")
        );
        assert_eq!(crate::event::pet_owner_in_name("a lesser mummy"), None);
    }

    /// The timeline is what the totals cannot say: when each thing happened,
    /// and how it turned out.
    #[test]
    fn timeline_records_your_actions_in_order() {
        let encs = run(&[
            (100, "You slash a lesser mummy for 100 points of damage."),
            (101, "You try to slash a lesser mummy, but miss!"),
            (
                102,
                "You crush a lesser mummy for 250 points of damage. (Critical)",
            ),
            (103, "Your target resisted the Shock of Frost spell."),
            (110, "You have slain a lesser mummy!"),
        ]);
        let t = &encs[0].timeline;
        let kinds: Vec<Outcome> = t.iter().map(|e| e.outcome).collect();
        assert_eq!(
            kinds,
            vec![Outcome::Hit, Outcome::Miss, Outcome::Crit, Outcome::Resist]
        );
        assert_eq!(t[0].at, 100);
        assert_eq!(t[0].amount, 100);
        assert_eq!(t[2].amount, 250);
        // Nothing landed, so nothing to report.
        assert_eq!(t[1].amount, 0);
        assert_eq!(t[3].ability, "Shock of Frost");
        assert!(encs[0].timeline_retained);
        assert_eq!(encs[0].timeline_dropped, 0);
    }

    /// A mob hitting you is not one of your skills, and would swamp the lanes.
    #[test]
    fn timeline_holds_only_your_own_actions() {
        let encs = run(&[
            (100, "You slash a lesser mummy for 100 points of damage."),
            (101, "A lesser mummy hits YOU for 40 points of damage."),
            (102, "A lesser mummy tries to hit YOU, but misses!"),
            (110, "You have slain a lesser mummy!"),
        ]);
        assert_eq!(encs[0].timeline.len(), 1);
        assert_eq!(encs[0].timeline[0].amount, 100);
    }

    /// A fight long enough to overflow must say so rather than present a
    /// truncated timeline as the whole story.
    #[test]
    fn timeline_caps_and_counts_what_it_dropped() {
        let mut lines: Vec<(i64, &str)> = Vec::new();
        for i in 0..(TIMELINE_MAX + 50) {
            lines.push((100 + (i as i64 / 10), "You slash a giant for 10 points of damage."));
        }
        let mut t = EncounterTracker::new("Tharn");
        for e in entries(&lines) {
            t.push(&e);
        }
        let enc = t.flush().expect("a fight");
        assert_eq!(enc.timeline.len(), TIMELINE_MAX);
        assert_eq!(enc.timeline_dropped, 50);
    }
}
