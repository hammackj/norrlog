//! Profiles of the creatures you fight, built only from what a log proves.
//!
//! # What is a fact about the creature, and what is not
//!
//! This is the whole design problem. "A fire goblin hits 58% of the time" is
//! not a fact about a fire goblin — it is a fact about a fire goblin swinging
//! at *you*, at your level, wearing your armour. Recorded as a property of the
//! creature it is misinformation, and worse, misinformation that looks
//! authoritative once it reaches a database page.
//!
//! So the two kinds are kept apart:
//!
//! * **About the creature.** Times it died, times it killed you, the largest
//!   single hit ever seen from it, how much damage it absorbed before dying.
//! * **About the pairing.** Swings, hits and misses — stored per character and
//!   per level, never pooled. A site can aggregate those later across many
//!   characters, which is the only way they become a statement about the
//!   creature rather than about one player.
//!
//! # Health is estimated from a distribution, not asserted
//!
//! Nothing announces a creature's health. What can be measured is the damage
//! that went into creatures of its name between one death and the next, and
//! that is pulled in both directions at once:
//!
//! * **Upwards**, and this is the serious one: two creatures of the same name
//!   are written identically by the client, so a pull with three of them pools
//!   all three into one sample and whichever dies first claims the lot. The
//!   log cannot tell them apart, and neither can we.
//! * **Downwards**, by damage from people whose lines this log never saw, and
//!   by a creature that was already hurt when the fight started.
//! * **Upwards again**, by overkill on the killing blow.
//!
//! An earlier version of this called the maximum an `hp_floor`, which was
//! wrong twice over: it is not a floor, and taking the maximum deliberately
//! selects the most inflated sample. The giveaway was real data — the same
//! creature read *lower* at a harder tier than an easier one.
//!
//! So the samples are kept and reported as a spread: [`NpcProfile::health_min`],
//! [`NpcProfile::health_median`], [`NpcProfile::health_max`] over
//! [`NpcProfile::health_samples`] deaths. The median is the figure to show,
//! because single pulls are the common case and multi-mob pools are the tail;
//! the count is what says how much to believe it.
//!
//! # Identity
//!
//! A creature is `(zone, mode, difficulty, name)` — see [`crate::zone`]. The
//! same name in Nagafen's Lair and in Befallen is two creatures, and the same
//! name at Awakened and at Refined is two more, with different health and
//! different loot. Rolling those together would produce an average of things
//! that are not alike.
//!
//! # Across logs
//!
//! One log is one character's few days, and the client throws it away and
//! starts another. A creature's record only becomes worth reading once every
//! log that ever met it has contributed, which is what [`merge`] is for.
//!
//! Merging is not summing. Counts add and records take the extreme, but the
//! two interesting fields resist it: melee stays split by the pairing that
//! produced it, because pooling two characters' hit rates destroys the very
//! property the split exists to protect; and health is recomputed from
//! [`HealthSample`]s rather than folded from summaries, because medians do
//! not average. That is why the samples persist individually and the spread
//! does not.

use crate::drops::canonical_npc;
use crate::game::Game;
use crate::zone::{Mode, Zone};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A creature's body type, where the log has proved one.
///
/// Proof comes from spells the game refuses to cast at anything else: a great
/// many spells are restricted to undead, to summoned creatures, to animals or
/// to plants, and the spell file says which. One of those landing is the game
/// itself answering the question, so no guessing from names is involved.
///
/// Deliberately not exhaustive of the game's body types — only the four the
/// spell file can vouch for appear here. A creature nobody happened to cast
/// such a spell at simply has no kind, which is why this is an `Option`
/// everywhere it is stored rather than a variant meaning "unknown".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CreatureKind {
    Undead,
    Summoned,
    Animal,
    Plant,
}

impl CreatureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CreatureKind::Undead => "undead",
            CreatureKind::Summoned => "summoned",
            CreatureKind::Animal => "animal",
            CreatureKind::Plant => "plant",
        }
    }

    /// Read back what [`CreatureKind::as_str`] wrote.
    ///
    /// `None` for anything unrecognised, and for the empty string a stored row
    /// uses to mean nothing was ever proved.
    pub fn from_str(s: &str) -> Option<CreatureKind> {
        match s {
            "undead" => Some(CreatureKind::Undead),
            "summoned" => Some(CreatureKind::Summoned),
            "animal" => Some(CreatureKind::Animal),
            "plant" => Some(CreatureKind::Plant),
            _ => None,
        }
    }
}

/// What identifies one creature in one place.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NpcKey {
    pub zone: String,
    pub mode: Mode,
    pub difficulty: u8,
    pub name: String,
}

/// One death that yielded a health measurement.
///
/// Kept as its own row, carrying the creature it belongs to, so that a median
/// can be taken over every log at once. A stored min/median/max could not be
/// merged with another log's: three medians do not make a median.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSample {
    pub zone: String,
    pub mode: Mode,
    pub difficulty: u8,
    pub name: String,
    /// When the creature died.
    pub at: i64,
    /// Damage seen to go into it since the last one of its name died here.
    pub hp: u64,
}

/// What became of one swing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swing {
    /// It landed, for this much.
    Hit(u64),
    /// It missed outright.
    Missed,
    /// You dodged, parried, riposted or blocked.
    Avoided,
}

/// Melee as observed against one character at one level.
///
/// Kept apart from the creature's own record because every number here is
/// about the pairing: a miss is your armour as much as its accuracy, and the
/// same creature will read differently against a different character.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeleeAgainst {
    pub character: String,
    /// Your level when this was observed, or 0 if the log had not said.
    pub level: u32,
    /// The stance you were fighting in: balanced, defensive, offensive,
    /// evasive — or empty before the log says. A defensive stance turns aside
    /// swings an offensive one takes, so pooling across stances measures your
    /// habits rather than the creature.
    pub stance: String,
    pub swings: u64,
    pub hits: u64,
    /// The creature swung and missed.
    pub misses: u64,
    /// You dodged, parried, riposted or blocked it. Separate from a miss
    /// because it is your defence rather than its accuracy — pooling the two
    /// would credit the creature for your riposte skill.
    pub avoided: u64,
    pub damage: u64,
    pub max_hit: u64,
}

impl MeleeAgainst {
    /// Landed swings as a fraction. A statement about the pairing, never about
    /// the creature alone.
    pub fn hit_rate(&self) -> f64 {
        if self.swings == 0 {
            0.0
        } else {
            self.hits as f64 / self.swings as f64
        }
    }
}

/// Everything one log can say about one creature in one place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpcProfile {
    pub name: String,
    pub zone: String,
    pub mode: Mode,
    pub difficulty: u8,
    pub game: Game,
    /// Its body type, on the days the log proved one; see [`CreatureKind`].
    ///
    /// Absent for most creatures and expected to stay that way: it is set only
    /// when someone cast a body-restricted spell at this one, so the field
    /// reports what has been proved, not what is known.
    #[serde(default)]
    pub kind: Option<CreatureKind>,
    /// Times it was seen to die.
    pub kills: u64,
    /// Times it killed you.
    pub deaths_to_it: u64,
    /// Deaths that produced a health sample.
    pub health_samples: u64,
    /// The spread of damage absorbed between deaths. Show the median; the
    /// others say how noisy the evidence is.
    pub health_min: u64,
    pub health_median: u64,
    pub health_max: u64,
    /// The largest single hit ever seen from it, across everyone.
    pub max_hit: u64,
    /// Melee per character and level, never pooled.
    pub melee: Vec<MeleeAgainst>,
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BestiaryReport {
    pub game: Game,
    /// Most-killed first.
    pub npcs: Vec<NpcProfile>,
}

/// Accumulates creature profiles.
#[derive(Debug, Default)]
pub struct Bestiary {
    game: Game,
    player: String,
    level: u32,
    stance: String,
    zone: Option<Zone>,
    npcs: HashMap<NpcKey, NpcProfile>,
    /// Damage dealt into a creature since the last time one of that name died
    /// here. Becomes a health sample when it dies.
    in_flight: HashMap<NpcKey, u64>,
    /// Every health sample per creature, so the report can show a spread
    /// rather than a single number that hides how noisy it is. Timestamped
    /// because these outlive the log they came from — a row in a database
    /// nobody can date back to a kill is a row nobody can check.
    health: HashMap<NpcKey, Vec<(i64, u64)>>,
}

/// The spread to report for a set of health samples: min, median, max.
///
/// The median rather than the mean, and never the maximum on its own: a
/// three-mob pull pools three creatures into one sample, and a mean drifts
/// towards that tail while the median rides it out. Shared with [`merge`] so
/// one log and fifty are summarised by the same rule.
pub fn health_spread(samples: &[u64]) -> (u64, u64, u64) {
    if samples.is_empty() {
        return (0, 0, 0);
    }
    let mut v = samples.to_vec();
    v.sort_unstable();
    let median = if v.len() % 2 == 1 {
        v[v.len() / 2]
    } else {
        (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2
    };
    (v[0], median, v[v.len() - 1])
}

impl Bestiary {
    pub fn new(game: Game, player: &str) -> Self {
        Bestiary {
            game,
            player: player.to_string(),
            ..Default::default()
        }
    }

    pub fn on_zone(&mut self, label: &str) {
        // A zone change ends every fight in the old zone: anything still
        // accumulating there was left alive, and its partial damage is not a
        // health floor for anything.
        self.in_flight.clear();
        self.zone = Some(Zone::parse(label));
    }

    pub fn on_level(&mut self, level: u32) {
        self.level = level;
    }

    pub fn on_stance(&mut self, stance: &str) {
        self.stance = stance.to_lowercase();
    }

    fn key(&self, npc: &str) -> Option<NpcKey> {
        let zone = self.zone.as_ref()?;
        let name = canonical_npc(npc).trim();
        if name.is_empty() {
            return None;
        }
        Some(NpcKey {
            zone: zone.name.clone(),
            mode: zone.mode,
            difficulty: zone.difficulty,
            name: name.to_string(),
        })
    }

    fn profile(&mut self, key: NpcKey, at: i64) -> &mut NpcProfile {
        let (zone, mode, difficulty, name) = (
            key.zone.clone(),
            key.mode,
            key.difficulty,
            key.name.clone(),
        );
        let game = self.game;
        self.npcs.entry(key).or_insert_with(|| NpcProfile {
            name,
            zone,
            mode,
            difficulty,
            game,
            kind: None,
            kills: 0,
            deaths_to_it: 0,
            health_samples: 0,
            health_min: 0,
            health_median: 0,
            health_max: 0,
            max_hit: 0,
            melee: Vec::new(),
            first_seen: at,
            last_seen: at,
        })
    }

    /// Damage dealt *into* a creature, from anyone. Feeds the health floor.
    pub fn on_damage_to(&mut self, at: i64, npc: &str, amount: u64) {
        let Some(key) = self.key(npc) else { return };
        *self.in_flight.entry(key.clone()).or_insert(0) += amount;
        self.profile(key, at).last_seen = at;
    }

    /// A spell that only works on one body type landed on this creature.
    ///
    /// The game enforces the restriction, so a single one of these settles the
    /// question outright — there is no counting or corroboration to do, and a
    /// creature already filed under a kind is left alone rather than churned.
    ///
    /// A contradiction is reported and then ignored, keeping the kind that
    /// arrived first. It should not happen; if it does, the cause is upstream —
    /// two creatures sharing a name, or a spell file whose columns have moved —
    /// and quietly flip-flopping the field would hide it.
    pub fn on_kind_evidence(&mut self, at: i64, npc: &str, kind: CreatureKind) {
        let Some(key) = self.key(npc) else { return };
        let p = self.profile(key, at);
        match p.kind {
            None => p.kind = Some(kind),
            Some(k) if k != kind => {
                eprintln!(
                    "[bestiary] {} is filed as {} but a {}-only spell landed on it; keeping {}",
                    p.name,
                    k.as_str(),
                    kind.as_str(),
                    k.as_str()
                );
            }
            Some(_) => {}
        }
    }

    /// A swing by a creature at you, however it turned out.
    ///
    /// Every outcome is recorded, not just the landed ones: a hit rate built
    /// from hits alone is a hundred percent for every creature in the game.
    pub fn on_swing(&mut self, at: i64, npc: &str, outcome: Swing) {
        let Some(key) = self.key(npc) else { return };
        let (player, level, stance) = (self.player.clone(), self.level, self.stance.clone());
        let p = self.profile(key, at);
        p.last_seen = at;
        if let Swing::Hit(amount) = outcome {
            p.max_hit = p.max_hit.max(amount);
        }
        let m = match p
            .melee
            .iter_mut()
            .position(|m| m.character == player && m.level == level && m.stance == stance)
        {
            Some(i) => &mut p.melee[i],
            None => {
                p.melee.push(MeleeAgainst {
                    character: player,
                    level,
                    stance,
                    ..Default::default()
                });
                p.melee.last_mut().unwrap()
            }
        };
        m.swings += 1;
        match outcome {
            Swing::Hit(amount) => {
                m.hits += 1;
                m.damage += amount;
                m.max_hit = m.max_hit.max(amount);
            }
            Swing::Missed => m.misses += 1,
            Swing::Avoided => m.avoided += 1,
        }
    }

    /// A creature died.
    pub fn on_kill(&mut self, at: i64, npc: &str) {
        let Some(key) = self.key(npc) else { return };
        let absorbed = self.in_flight.remove(&key).unwrap_or(0);
        // A death we saw no damage for says nothing about health — someone
        // else killed it, or it died out of sight — so it counts as a kill but
        // not as a sample.
        if absorbed > 0 {
            self.health.entry(key.clone()).or_default().push((at, absorbed));
        }
        let p = self.profile(key, at);
        p.kills += 1;
        p.last_seen = at;
    }

    /// A creature killed you.
    pub fn on_your_death(&mut self, at: i64, npc: &str) {
        let Some(key) = self.key(npc) else { return };
        let p = self.profile(key, at);
        p.deaths_to_it += 1;
        p.last_seen = at;
    }

    /// Every health measurement this log produced, one row per death.
    ///
    /// What persists, in place of the summary: see [`merge`].
    pub fn health_samples(&self) -> Vec<HealthSample> {
        let mut out: Vec<HealthSample> = self
            .health
            .iter()
            .flat_map(|(key, samples)| {
                samples.iter().map(move |(at, hp)| HealthSample {
                    zone: key.zone.clone(),
                    mode: key.mode,
                    difficulty: key.difficulty,
                    name: key.name.clone(),
                    at: *at,
                    hp: *hp,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            a.zone
                .cmp(&b.zone)
                .then(a.mode.cmp(&b.mode))
                .then(a.difficulty.cmp(&b.difficulty))
                .then(a.name.cmp(&b.name))
                .then(a.at.cmp(&b.at))
                .then(a.hp.cmp(&b.hp))
        });
        out
    }

    pub fn report(&self) -> BestiaryReport {
        let mut npcs: Vec<NpcProfile> = self
            .npcs
            .iter()
            .map(|(key, p)| {
                let mut p = p.clone();
                if let Some(samples) = self.health.get(key) {
                    let v: Vec<u64> = samples.iter().map(|(_, hp)| *hp).collect();
                    let (min, median, max) = health_spread(&v);
                    p.health_samples = v.len() as u64;
                    p.health_min = min;
                    p.health_median = median;
                    p.health_max = max;
                }
                p
            })
            .collect();
        for p in &mut npcs {
            p.melee.sort_by(|a, b| {
                a.character
                    .cmp(&b.character)
                    .then(a.level.cmp(&b.level))
                    .then(a.stance.cmp(&b.stance))
            });
        }
        npcs.sort_by(|a, b| {
            b.kills
                .cmp(&a.kills)
                .then(a.zone.cmp(&b.zone))
                .then(a.name.cmp(&b.name))
        });
        BestiaryReport {
            game: self.game,
            npcs,
        }
    }
}

/// Fold profiles from any number of logs into one profile per creature.
///
/// Each rule is chosen for what the field means, not for what is convenient:
///
/// * **Kills and deaths add.** They are counts of disjoint events; two logs
///   never saw the same death, because each log is its own stretch of time.
/// * **`max_hit` takes the larger.** It is a record, and a record over more
///   evidence is still a record.
/// * **`first_seen`/`last_seen` take the outer bounds**, so the span reads as
///   "known about since" rather than "this file starts here".
/// * **Melee merges only within `(character, level, stance)`.** Different
///   pairings stay different rows for ever; that split is the whole reason
///   these numbers are trustworthy at all.
/// * **Health is recomputed from `health`,** the raw samples, and the caller
///   must pass every sample it wants counted. Summaries are not mergeable.
///
/// Caller-supplied profiles may repeat: passing the live log's report
/// alongside stored rows from *other* logs is the intended use. Passing the
/// same log twice would double its counts, which is why the server excludes
/// the log it is following from the stored side rather than de-duplicating
/// after the fact.
pub fn merge(
    profiles: impl IntoIterator<Item = NpcProfile>,
    health: &[HealthSample],
) -> Vec<NpcProfile> {
    type Key = (Game, String, Mode, u8, String);
    let mut merged: HashMap<Key, NpcProfile> = HashMap::new();

    for mut p in profiles {
        let key = (
            p.game,
            p.zone.clone(),
            p.mode,
            p.difficulty,
            p.name.clone(),
        );
        // Melee always goes through the folding path below, even for the first
        // profile of a creature. Inserting one wholesale would trust it to
        // already hold one row per pairing, and a caller reading rows back out
        // of a database has no such guarantee — the duplicates would then
        // survive every later merge untouched.
        let incoming = std::mem::take(&mut p.melee);
        match merged.get_mut(&key) {
            None => {
                merged.insert(key.clone(), p);
            }
            Some(into) => {
                into.kills += p.kills;
                into.deaths_to_it += p.deaths_to_it;
                into.max_hit = into.max_hit.max(p.max_hit);
                into.first_seen = into.first_seen.min(p.first_seen);
                into.last_seen = into.last_seen.max(p.last_seen);
                // A kind proved anywhere holds everywhere: one log casting an
                // undead-only spell settles it for a creature fifty other logs
                // only ever hit with a sword.
                into.kind = into.kind.or(p.kind);
            }
        }
        let into = merged.get_mut(&key).expect("just inserted");
        for m in incoming {
            match into.melee.iter_mut().find(|e| {
                e.character == m.character && e.level == m.level && e.stance == m.stance
            }) {
                Some(e) => {
                    e.swings += m.swings;
                    e.hits += m.hits;
                    e.misses += m.misses;
                    e.avoided += m.avoided;
                    e.damage += m.damage;
                    e.max_hit = e.max_hit.max(m.max_hit);
                }
                None => into.melee.push(m),
            }
        }
    }

    // Health last, from the samples, replacing whatever spread the individual
    // profiles arrived with.
    let mut by_creature: HashMap<(String, Mode, u8, String), Vec<u64>> = HashMap::new();
    for s in health {
        by_creature
            .entry((s.zone.clone(), s.mode, s.difficulty, s.name.clone()))
            .or_default()
            .push(s.hp);
    }
    let mut out: Vec<NpcProfile> = merged.into_values().collect();
    for p in &mut out {
        let samples = by_creature
            .get(&(p.zone.clone(), p.mode, p.difficulty, p.name.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let (min, median, max) = health_spread(samples);
        p.health_samples = samples.len() as u64;
        p.health_min = min;
        p.health_median = median;
        p.health_max = max;
        p.melee.sort_by(|a, b| {
            a.character
                .cmp(&b.character)
                .then(a.level.cmp(&b.level))
                .then(a.stance.cmp(&b.stance))
        });
    }
    out.sort_by(|a, b| {
        b.kills
            .cmp(&a.kills)
            .then(a.zone.cmp(&b.zone))
            .then(a.name.cmp(&b.name))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bestiary() -> Bestiary {
        let mut b = Bestiary::new(Game::EverQuestLegends, "Tharn");
        b.on_level(50);
        b
    }

    /// The same creature at two tiers is two creatures: different health,
    /// different loot, and averaging them describes neither.
    #[test]
    fn tier_and_mode_separate_one_name_into_several_creatures() {
        let mut b = bestiary();

        b.on_zone("Nagafen's Lair 1 (Awakened)");
        b.on_damage_to(100, "a fire goblin", 400);
        b.on_kill(100, "a fire goblin");

        b.on_zone("Nagafen's Lair - Solo 4 (Refined)");
        b.on_damage_to(200, "a fire goblin", 9000);
        b.on_kill(200, "a fire goblin");

        let r = b.report();
        assert_eq!(r.npcs.len(), 2, "one name, two creatures: {:?}", r.npcs);
        let refined = r
            .npcs
            .iter()
            .find(|n| n.difficulty == 4 && n.mode == Mode::Solo)
            .unwrap();
        assert_eq!(refined.health_median, 9000);
        let awakened = r
            .npcs
            .iter()
            // Unmarked in the log, so the mode is not stated -- see
            // `zone::Mode::Instance`. It read as Group until the client
            // turned out to write that marker only sometimes.
            .find(|n| n.difficulty == 1 && n.mode == Mode::Instance)
            .unwrap();
        assert_eq!(awakened.health_median, 400);
        // Same place, though — which is what makes a zone page possible.
        assert_eq!(refined.zone, awakened.zone);
    }

    /// Health is a spread, not a single number: one sample can be a clean
    /// solo kill and the next a scratch on something someone else finished.
    #[test]
    fn health_is_reported_as_a_spread_over_its_samples() {
        let mut b = bestiary();
        b.on_zone("Befallen 2 (Adaptive)");

        b.on_damage_to(100, "a skeleton", 1200);
        b.on_kill(100, "a skeleton");
        // A second one finished off by someone else: we only saw a scratch.
        b.on_damage_to(200, "a skeleton", 30);
        b.on_kill(200, "a skeleton");

        let p = &b.report().npcs[0];
        assert_eq!(p.kills, 2);
        assert_eq!(
            (p.health_samples, p.health_min, p.health_max),
            (2, 30, 1200),
            "both samples kept, spread reported"
        );
        assert_eq!(p.health_median, 615, "the midpoint of the two");
    }

    /// Damage that never killed anything is not evidence of health, and must
    /// not leak into the next creature of the same name.
    #[test]
    fn damage_to_something_that_lived_is_not_a_health_floor() {
        let mut b = bestiary();
        b.on_zone("Befallen 2 (Adaptive)");
        b.on_damage_to(100, "a skeleton", 5000); // it got away
        b.on_zone("Befallen 1 (Awakened)"); // zoned out mid-fight
        b.on_zone("Befallen 2 (Adaptive)");
        b.on_damage_to(300, "a skeleton", 40);
        b.on_kill(300, "a skeleton");

        let p = b
            .report()
            .npcs
            .into_iter()
            .find(|n| n.difficulty == 2)
            .unwrap();
        assert_eq!(p.health_median, 40, "the 5000 belonged to one that did not die");
        assert_eq!(p.health_samples, 1);
    }

    /// Melee is about the pairing, so it is filed under the character and
    /// level it was seen at — and a level-up starts a new record rather than
    /// polluting the old one.
    #[test]
    fn melee_is_recorded_per_character_and_level() {
        let mut b = bestiary();
        b.on_zone("The Plane of Hate - Solo");

        b.on_swing(100, "an ashenhand disciple", Swing::Hit(120));
        b.on_swing(101, "an ashenhand disciple", Swing::Missed);
        b.on_level(51);
        b.on_swing(200, "an ashenhand disciple", Swing::Hit(90));

        let p = &b.report().npcs[0];
        assert_eq!(p.melee.len(), 2, "two levels, two records");
        let at50 = p.melee.iter().find(|m| m.level == 50).unwrap();
        assert_eq!((at50.swings, at50.hits, at50.misses), (2, 1, 1));
        assert!((at50.hit_rate() - 0.5).abs() < 1e-9);
        assert_eq!(at50.max_hit, 120);
        let at51 = p.melee.iter().find(|m| m.level == 51).unwrap();
        assert_eq!((at51.swings, at51.hits), (1, 1));
        // The biggest hit is the creature's own record, across every pairing.
        assert_eq!(p.max_hit, 120);
    }

    /// A miss is evidence too. Counting only landed swings would give every
    /// creature a perfect hit rate.
    #[test]
    fn a_miss_counts_as_a_swing() {
        let mut b = bestiary();
        b.on_zone("Innothule Swamp");
        for _ in 0..3 {
            b.on_swing(100, "a swamp alligator", Swing::Missed);
        }
        b.on_swing(101, "a swamp alligator", Swing::Avoided);
        b.on_swing(102, "a swamp alligator", Swing::Hit(45));
        let m = &b.report().npcs[0].melee[0];
        assert_eq!((m.swings, m.hits, m.misses, m.avoided), (5, 1, 3, 1));
        assert!((m.hit_rate() - 0.2).abs() < 1e-9);
    }

    /// Nothing can be filed before the log says where you are.
    #[test]
    fn observations_before_a_zone_is_known_are_dropped() {
        let mut b = bestiary();
        b.on_kill(100, "a fire goblin");
        b.on_swing(100, "a fire goblin", Swing::Hit(50));
        assert!(b.report().npcs.is_empty(), "no zone, no creature");
    }

    /// The article is the client's, not the creature's — the same rule the
    /// drop tracker already follows, so the two agree about who is who.
    #[test]
    fn the_article_does_not_make_a_second_creature() {
        let mut b = bestiary();
        b.on_zone("Innothule Swamp");
        b.on_kill(100, "a fire goblin");
        b.on_kill(200, "fire goblin");
        let r = b.report();
        assert_eq!(r.npcs.len(), 1);
        assert_eq!(r.npcs[0].kills, 2);
        assert_eq!(r.npcs[0].name, "fire goblin");
    }

    /// The samples are what persists, so each one has to carry the creature it
    /// belongs to and the moment it was taken.
    #[test]
    fn health_samples_are_emitted_one_per_death() {
        let mut b = bestiary();
        b.on_zone("Befallen 2 (Adaptive)");
        b.on_damage_to(100, "a skeleton", 1200);
        b.on_kill(100, "a skeleton");
        b.on_damage_to(200, "a skeleton", 30);
        b.on_kill(200, "a skeleton");
        // A kill nobody saw damage for is not a measurement of anything.
        b.on_kill(300, "a skeleton");

        let s = b.health_samples();
        assert_eq!(s.len(), 2, "three kills, two measurements: {s:?}");
        assert_eq!((s[0].at, s[0].hp), (100, 1200));
        assert_eq!((s[1].at, s[1].hp), (200, 30));
        assert_eq!(s[0].name, "skeleton");
        assert_eq!((s[0].difficulty, s[0].mode), (2, Mode::Instance));
    }

    /// Two logs of the same creature are one creature. Counts add, the record
    /// hit survives, and the span covers both.
    #[test]
    fn merging_two_logs_adds_counts_and_keeps_records() {
        let mut first = Bestiary::new(Game::EverQuestLegends, "Tharn");
        first.on_level(50);
        first.on_zone("Nagafen's Lair - Solo 4 (Refined)");
        first.on_damage_to(100, "a fire giant", 20000);
        first.on_kill(100, "a fire giant");
        first.on_swing(100, "a fire giant", Swing::Hit(300));

        let mut second = Bestiary::new(Game::EverQuestLegends, "Tharn");
        second.on_level(50);
        second.on_zone("Nagafen's Lair - Solo 4 (Refined)");
        second.on_damage_to(900, "a fire giant", 24000);
        second.on_kill(900, "a fire giant");
        second.on_swing(900, "a fire giant", Swing::Hit(410));
        second.on_your_death(901, "a fire giant");

        let health = [first.health_samples(), second.health_samples()].concat();
        let merged = merge(
            first
                .report()
                .npcs
                .into_iter()
                .chain(second.report().npcs),
            &health,
        );

        assert_eq!(merged.len(), 1, "one creature, not two: {merged:?}");
        let p = &merged[0];
        assert_eq!((p.kills, p.deaths_to_it), (2, 1));
        assert_eq!(p.max_hit, 410, "the record is the larger of the two");
        assert_eq!((p.first_seen, p.last_seen), (100, 901));
        assert_eq!(p.melee.len(), 1, "same character, level and stance");
        assert_eq!(p.melee[0].swings, 2);
        // Both samples, and a median over both rather than of two medians.
        assert_eq!(
            (p.health_samples, p.health_min, p.health_median, p.health_max),
            (2, 20000, 22000, 24000)
        );
    }

    /// The reason health persists as samples: merging summaries cannot
    /// reproduce the median, and the difference is not small.
    #[test]
    fn a_median_over_every_sample_is_not_a_median_of_medians() {
        let mut skewed = Bestiary::new(Game::EverQuestLegends, "Tharn");
        skewed.on_zone("Befallen 2 (Adaptive)");
        // One clean kill, then two pulls that pooled several skeletons each.
        for (at, hp) in [(10, 1000), (20, 5000), (30, 6000)] {
            skewed.on_damage_to(at, "a skeleton", hp);
            skewed.on_kill(at, "a skeleton");
        }
        let mut clean = Bestiary::new(Game::EverQuestLegends, "Tharn");
        clean.on_zone("Befallen 2 (Adaptive)");
        for (at, hp) in [(40, 1100), (50, 1050)] {
            clean.on_damage_to(at, "a skeleton", hp);
            clean.on_kill(at, "a skeleton");
        }

        let per_log_medians = [skewed.report().npcs[0].health_median, clean.report().npcs[0].health_median];
        let health = [skewed.health_samples(), clean.health_samples()].concat();
        let merged = merge(
            skewed.report().npcs.into_iter().chain(clean.report().npcs),
            &health,
        );

        assert_eq!(per_log_medians, [5000, 1075]);
        assert_eq!(merged[0].health_samples, 5);
        assert_eq!(
            merged[0].health_median, 1100,
            "the true midpoint of all five, not the 3037 that averaging the \
             two medians would have invented"
        );
    }

    /// Merging must never pool one character's melee into another's, whatever
    /// else it adds together — that split is why the numbers mean anything.
    #[test]
    fn merging_keeps_each_pairing_apart() {
        let mut main = Bestiary::new(Game::EverQuestLegends, "Tharn");
        main.on_level(50);
        main.on_zone("Innothule Swamp");
        main.on_swing(100, "a swamp alligator", Swing::Hit(45));

        let mut alt = Bestiary::new(Game::EverQuestLegends, "Grimhide");
        alt.on_level(12);
        alt.on_zone("Innothule Swamp");
        alt.on_swing(200, "a swamp alligator", Swing::Missed);
        alt.on_swing(201, "a swamp alligator", Swing::Missed);

        let merged = merge(main.report().npcs.into_iter().chain(alt.report().npcs), &[]);
        let p = &merged[0];
        assert_eq!(p.melee.len(), 2, "two characters, two records");
        let grim = p.melee.iter().find(|m| m.character == "Grimhide").unwrap();
        assert_eq!((grim.swings, grim.hits, grim.level), (2, 0, 12));
        let tharn = p.melee.iter().find(|m| m.character == "Tharn").unwrap();
        assert_eq!((tharn.swings, tharn.hits, tharn.level), (1, 1, 50));
        // The creature's own record still spans everyone.
        assert_eq!(p.max_hit, 45);
    }

    /// Merging respects the identity rule: different tiers stay different
    /// creatures no matter how many logs are folded in.
    #[test]
    fn merging_does_not_collapse_tiers_or_zones() {
        let mut b = bestiary();
        b.on_zone("Nagafen's Lair - Solo 1 (Awakened)");
        b.on_kill(100, "a fire giant");
        b.on_zone("Nagafen's Lair - Solo 4 (Refined)");
        b.on_kill(200, "a fire giant");
        b.on_zone("Befallen 1 (Awakened)");
        b.on_kill(300, "a fire giant");

        let merged = merge(b.report().npcs, &[]);
        assert_eq!(merged.len(), 3, "three creatures: {merged:?}");
        assert!(merged.iter().all(|p| p.kills == 1));
    }

    #[test]
    fn deaths_to_a_creature_are_counted() {
        let mut b = bestiary();
        b.on_zone("The Plane of Fear - Solo 2 (Adaptive)");
        b.on_your_death(100, "a dracoliche");
        b.on_your_death(200, "a dracoliche");
        let p = &b.report().npcs[0];
        assert_eq!(p.deaths_to_it, 2);
        assert_eq!(p.kills, 0, "it killing you is not you killing it");
    }
}
