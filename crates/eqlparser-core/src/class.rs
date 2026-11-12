//! Character class inference.
//!
//! EverQuest Legends lets a character hold three classes at once, and the
//! combination changes over a character's life. The log never states the
//! classes outright, so we infer them from evidence.
//!
//! # Evidence sources
//!
//! * **Spells** — authoritative. `spells_us.txt` records, for all 16 classes,
//!   the level at which that class may cast each spell (255 = never). A spell
//!   in the character's book is therefore direct evidence of every class that
//!   could have granted it, and a spell only one class can cast is conclusive.
//! * **Skills** — supplementary, and necessarily curated. Warriors and rogues
//!   have no spells at all, so without skills they would be invisible. The
//!   table below is deliberately limited to skills that are strongly
//!   diagnostic; ambiguous skills are simply omitted rather than guessed at.
//!
//! # Why evidence is timestamped
//!
//! Classes change. A character that was war/enc, then war/dru/rog, then
//! war/dru/brd leaves all of that behind in one log: old enchanter spells stay
//! in the spellbook and old rogue skills stay on the sheet. Reporting a single
//! all-time answer would blend three eras together, so every piece of evidence
//! carries a timestamp and queries are scoped to a time window.

use serde::{Deserialize, Serialize};

/// The 16 EverQuest classes, in the order their level requirements appear in
/// `spells_us.txt` (fields 36..=51).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Class {
    Warrior,
    Cleric,
    Paladin,
    Ranger,
    ShadowKnight,
    Druid,
    Monk,
    Bard,
    Rogue,
    Shaman,
    Necromancer,
    Wizard,
    Magician,
    Enchanter,
    Beastlord,
    Berserker,
}

/// Field index of the first class level in a `spells_us.txt` record.
/// Verified empirically: this is the only 16-wide block where ~95% of spells
/// hold the sentinel 255, and single-class spells light up exactly one slot
/// (Chords of Dissonance -> Bard, Chaotic Feedback -> Enchanter,
/// Camouflage -> Druid/Ranger).
pub const CLASS_FIELD_BASE: usize = 36;

/// Number of classes, and therefore the width of the class block.
pub const CLASS_COUNT: usize = 16;

/// Sentinel meaning "this class can never cast this spell".
pub const CLASS_CANNOT_CAST: u8 = 255;

impl Class {
    pub const ALL: [Class; CLASS_COUNT] = [
        Class::Warrior,
        Class::Cleric,
        Class::Paladin,
        Class::Ranger,
        Class::ShadowKnight,
        Class::Druid,
        Class::Monk,
        Class::Bard,
        Class::Rogue,
        Class::Shaman,
        Class::Necromancer,
        Class::Wizard,
        Class::Magician,
        Class::Enchanter,
        Class::Beastlord,
        Class::Berserker,
    ];

    pub fn from_index(i: usize) -> Option<Class> {
        Class::ALL.get(i).copied()
    }

    pub fn index(self) -> usize {
        Class::ALL.iter().position(|c| *c == self).unwrap()
    }

    pub fn name(self) -> &'static str {
        match self {
            Class::Warrior => "Warrior",
            Class::Cleric => "Cleric",
            Class::Paladin => "Paladin",
            Class::Ranger => "Ranger",
            Class::ShadowKnight => "Shadow Knight",
            Class::Druid => "Druid",
            Class::Monk => "Monk",
            Class::Bard => "Bard",
            Class::Rogue => "Rogue",
            Class::Shaman => "Shaman",
            Class::Necromancer => "Necromancer",
            Class::Wizard => "Wizard",
            Class::Magician => "Magician",
            Class::Enchanter => "Enchanter",
            Class::Beastlord => "Beastlord",
            Class::Berserker => "Berserker",
        }
    }

    pub fn abbrev(self) -> &'static str {
        match self {
            Class::Warrior => "WAR",
            Class::Cleric => "CLR",
            Class::Paladin => "PAL",
            Class::Ranger => "RNG",
            Class::ShadowKnight => "SHD",
            Class::Druid => "DRU",
            Class::Monk => "MNK",
            Class::Bard => "BRD",
            Class::Rogue => "ROG",
            Class::Shaman => "SHM",
            Class::Necromancer => "NEC",
            Class::Wizard => "WIZ",
            Class::Magician => "MAG",
            Class::Enchanter => "ENC",
            Class::Beastlord => "BST",
            Class::Berserker => "BER",
        }
    }

    /// Match a token from a player-written spell set name against this class.
    ///
    /// The canonical abbreviation is not the only thing people type. "ZERK" is
    /// what berserkers are actually called, "SK" and "SHM"/"SHA" are ordinary
    /// shorthand, and a set named `WAR / DRU / ZERK` would otherwise name only
    /// two classes and be discarded as not a trio — leaving the old loadout
    /// asserted long after it was replaced.
    pub fn matches_token(self, token: &str) -> bool {
        if token.eq_ignore_ascii_case(self.abbrev()) || token.eq_ignore_ascii_case(self.name()) {
            return true;
        }
        // Kept deliberately short. Every alias here is a word players type for
        // a class and nothing else; ambiguous ones are left out rather than
        // risk a set called "Naggy - CHA buffs" naming an enchanter.
        let aliases: &[&str] = match self {
            Class::Berserker => &["ZERK", "ZRK"],
            Class::ShadowKnight => &["SK", "SHK"],
            Class::Shaman => &["SHA"],
            Class::Magician => &["MGE"],
            Class::Paladin => &["PLD"],
            Class::Necromancer => &["NECRO"],
            Class::Enchanter => &["ENCH"],
            _ => &[],
        };
        aliases.iter().any(|a| token.eq_ignore_ascii_case(a))
    }

    /// True for classes that cast no spells at all, and so can only ever be
    /// detected from skills.
    pub fn is_pure_melee(self) -> bool {
        matches!(
            self,
            Class::Warrior | Class::Rogue | Class::Monk | Class::Berserker
        )
    }
}

/// Join classes as "WAR/BRD/SHM".
pub fn abbrev_list(classes: &[Class]) -> String {
    classes
        .iter()
        .map(|c| c.abbrev())
        .collect::<Vec<_>>()
        .join("/")
}

/// Skills that are strongly diagnostic of a class.
///
/// Curated rather than derived: unlike spells, the game ships no machine
/// readable skill/class table. Entries are limited to skills whose class
/// association is unambiguous, weighted by how exclusive they are. Skills that
/// nearly every melee class trains (Defense, Offense, Dodge, Parry, 1H
/// Slashing...) are intentionally absent — they carry no signal.
const SKILL_SIGNALS: &[(&str, Class, f32)] = &[
    // Bard instrument skills are exclusive to bards.
    ("Singing", Class::Bard, 3.0),
    ("Stringed Instruments", Class::Bard, 3.0),
    ("Brass Instruments", Class::Bard, 3.0),
    ("Percussion Instruments", Class::Bard, 3.0),
    ("Wind Instruments", Class::Bard, 3.0),
    // Rogue-defining skills.
    ("Backstab", Class::Rogue, 3.0),
    ("Pick Pockets", Class::Rogue, 2.5),
    ("Pick Lock", Class::Rogue, 2.5),
    ("Disarm Traps", Class::Rogue, 2.5),
    ("Sense Traps", Class::Rogue, 2.5),
    ("Hide", Class::Rogue, 1.0),
    ("Sneak", Class::Rogue, 1.0),
    // Monk-defining skills.
    ("Flying Kick", Class::Monk, 3.0),
    ("Mend", Class::Monk, 2.0),
    ("Feign Death", Class::Monk, 1.5),
    ("Round Kick", Class::Monk, 2.0),
    ("Tiger Claw", Class::Monk, 2.0),
    // Warrior-leaning tanking skills. Weaker: knights share several of these,
    // but no other class trains the whole set.
    ("Taunt", Class::Warrior, 1.0),
    ("Bash", Class::Warrior, 0.8),
    ("Disarm", Class::Warrior, 1.0),
    ("Riposte", Class::Warrior, 0.6),
    ("Triple Attack", Class::Warrior, 0.8),
    ("2H Slashing", Class::Warrior, 0.5),
    ("2H Blunt", Class::Warrior, 0.5),
    ("Berserking", Class::Berserker, 3.0),
    ("Frenzy", Class::Berserker, 2.0),
];

/// A class by its full name only, as a channel or EverQuest's `/who` writes
/// it: case and spaces ignored, so the channel's "Shadowknight" is a Shadow
/// Knight. Abbreviations do not count here, where "WAR" could be anything.
pub fn parse_class_name(name: &str) -> Option<Class> {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    let want = squash(name);
    Class::ALL.into_iter().find(|c| squash(c.name()) == want)
}

/// Parse a `/who` class list such as "WAR/BRD/SHM" into classes.
///
/// Order is preserved: the first entry is the primary class, which is the one
/// the level in a `/who` row belongs to.
pub fn parse_class_list(list: &str) -> Vec<Class> {
    list.split('/')
        .filter_map(|tok| {
            let tok = tok.trim();
            (0..CLASS_COUNT)
                .filter_map(Class::from_index)
                .find(|c| c.matches_token(tok))
        })
        .collect()
}

/// A single timestamped observation supporting a class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassEvidence {
    pub timestamp: i64,
    pub class: Class,
    pub weight: f32,
    /// What produced this observation, for display ("spell: Chords of
    /// Dissonance", "skill: Singing").
    pub reason: String,
}

/// A class the detector believes the character holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassScore {
    pub class: Class,
    pub score: f32,
    /// Share of total score, 0.0–1.0.
    pub share: f32,
    /// A few representative reasons, for explaining the verdict in the UI.
    pub top_reasons: Vec<String>,
}

/// How much of a class's evidence must be its own for it to take a trio slot.
///
/// Tuned against this character: Warrior's skill evidence is entirely its own,
/// while Paladin and Ranger score almost wholly on spells already explained by
/// Bard and Druid. A third of the score is a wide enough margin to reject
/// those without demanding a class be unique — real overlapping classes do
/// exist.
const NOVEL_EVIDENCE_SHARE: f32 = 0.33;

/// Above this many eligible casters, a spell is treated as evidence of nothing.
///
/// EQ Legends gives every class a set of shared abilities — `Origin`,
/// `Form of the Great Wolf`, `Rampage I`, `Heroic Leap I`. Each carries a
/// weight of only 1/16, but they are cast constantly, and that trickle
/// accumulates into a baseline that every class shares equally. On this
/// character it put Paladin within one point of Warrior on nothing but noise.
/// A spell two thirds of the game can cast cannot distinguish anyone, so it is
/// discarded rather than diluted.
const UNIVERSAL_CASTER_LIMIT: usize = 10;

/// Weight of one class named in a loaded spell set.
///
/// Deliberately large: a single set name outweighs a long tail of shared
/// spells, which is exactly the correction needed. It is not infinite, so a
/// set renamed once by mistake cannot permanently rewrite the character.
const SPELL_SET_WEIGHT: f32 = 25.0;

/// Weight of the class channel EverQuest joins you to.
///
/// Heavier than a spell set name, because nobody typed it; lighter than
/// certainty, because a channel can be joined by hand and the roster does not
/// say which is which. Still loses to `/who`. From the eqparser fork.
const CLASS_CHANNEL_WEIGHT: f32 = 40.0;

/// One class's evidence within a window.
struct ClassTally {
    class: Class,
    score: f32,
    /// Reason to accumulated weight, so novelty can be measured by weight
    /// rather than by count of distinct reasons.
    reasons: std::collections::BTreeMap<String, f32>,
}

/// Accumulates class evidence over time and answers "what are they now?".
#[derive(Debug, Default)]
pub struct ClassDetector {
    evidence: Vec<ClassEvidence>,
    /// Spell sets loaded, with the classes each names.
    ///
    /// Kept apart from the weighted evidence because it answers a different
    /// question. Evidence accumulates, which is right for "what has this
    /// character been", and wrong for "what is it right now": on the day of a
    /// respec the outgoing class has a full day banked and the incoming one
    /// has minutes, so scoring alone reports the swap hours late.
    spell_sets: Vec<(i64, Vec<Class>)>,
    /// Classes held at once: 3 in Legends, 1 in EverQuest (see
    /// [`crate::profile::Profile::class_slots`]). 0 is read as 3, the
    /// default.
    slots: usize,
    /// In a one-class game, every time the game stated the class, in log
    /// order: `/who` rows and the class channel on the roster. One class at a
    /// time is not one class for life: a live EverQuest character can switch
    /// class (a persona), and one log then holds both, each levelled
    /// separately. Each statement holds until the next.
    stated: Vec<(i64, Class)>,
}

impl ClassDetector {
    pub fn new() -> Self {
        ClassDetector::default()
    }

    /// A detector for a game whose characters hold `slots` classes at once.
    ///
    /// One slot changes the rules, as the eqparser fork found: no novelty test
    /// (the top scorer is the class; a shadow class scores on borrowed
    /// evidence and cannot outscore the real one), and what the game states
    /// (a `/who` row, the class channel) holds until it states otherwise.
    pub fn with_slots(slots: usize) -> Self {
        ClassDetector { slots, ..Self::default() }
    }

    /// Note a class the game stated, keeping the timeline in order. A repeat
    /// of the class already in force adds nothing.
    fn state(&mut self, at: i64, class: Class) {
        let i = self.stated.partition_point(|(t, _)| *t <= at);
        if i > 0 && self.stated[i - 1].1 == class {
            return;
        }
        self.stated.insert(i, (at, class));
    }

    /// In a one-class game, the class the game last stated at or before `at`.
    /// Before the first statement, the first one: nothing earlier says
    /// otherwise, and a log rarely opens with a `/who`.
    fn stated_at(&self, at: i64) -> Option<Class> {
        let i = self.stated.partition_point(|(t, _)| *t <= at);
        self.stated.get(i.saturating_sub(1)).map(|(_, c)| *c)
    }

    /// The class the game itself named as in force at `at`, in a one-class
    /// game: a `/who` row or the class channel. `None` in Legends, where a
    /// declared trio may be a spell set the player typed, and when nothing
    /// was ever stated. What the apps show as "stated" rather than a share.
    pub fn stated_class(&self, at: i64) -> Option<Class> {
        if self.slots() == 1 { self.stated_at(at) } else { None }
    }

    fn slots(&self) -> usize {
        if self.slots == 0 { 3 } else { self.slots }
    }

    /// Record the chat channels the client says you are in.
    ///
    /// EverQuest auto-joins you to your class's channel, so a channel named
    /// after a class is usually your class. Weighted evidence rather than an
    /// answer, because it can be joined by hand. Only full class names count:
    /// "General", "Kunark" and zone channels name nothing. From the eqparser
    /// fork (a2d76c4).
    pub fn observe_channels(&mut self, timestamp: i64, channels: &[String]) {
        let named: Vec<Class> = channels.iter().filter_map(|n| parse_class_name(n)).collect();
        // One class channel is the game saying which class you are on: it
        // re-joins you to the new one when you switch. Two say nothing for
        // certain, since one of them was joined by hand.
        if self.slots() == 1 && named.len() == 1 {
            self.state(timestamp, named[0]);
        }
        for name in channels {
            if let Some(class) = parse_class_name(name) {
                self.evidence.push(ClassEvidence {
                    timestamp,
                    class,
                    weight: CLASS_CHANNEL_WEIGHT,
                    reason: format!("class channel: {name}"),
                });
            }
        }
    }

    /// Record that the character knows a spell (memorised, scribed or cast).
    ///
    /// `class_levels` is the spell's 16-wide block from `spells_us.txt`.
    /// Weight is inversely proportional to how many classes can cast it, so a
    /// single-class spell is decisive while a spell everyone gets barely moves
    /// the needle.
    pub fn observe_spell(
        &mut self,
        timestamp: i64,
        spell_name: &str,
        class_levels: &[u8; CLASS_COUNT],
    ) {
        let casters: Vec<usize> = (0..CLASS_COUNT)
            .filter(|i| class_levels[*i] != CLASS_CANNOT_CAST)
            .collect();
        if casters.is_empty() || casters.len() > UNIVERSAL_CASTER_LIMIT {
            return;
        }
        let weight = 1.0 / casters.len() as f32;
        for i in casters {
            if let Some(class) = Class::from_index(i) {
                self.evidence.push(ClassEvidence {
                    timestamp,
                    class,
                    weight,
                    reason: format!("spell: {spell_name}"),
                });
            }
        }
    }

    /// Record a spell set being loaded, e.g. "WAR / BRD / DRU - Buffs".
    ///
    /// The strongest signal in the log for a class that casts nothing. Warrior
    /// produces only skill evidence, and skills fall silent once capped, so on
    /// a caster-heavy build inference alone reliably picks a shadow class
    /// instead — Ranger off Druid's spells, Paladin off Cleric's. A set the
    /// player named "WAR / BRD / DRU" settles it.
    ///
    /// It is player-authored text, so it is weighted heavily but not treated
    /// as proof: only tokens that exactly match a class abbreviation count,
    /// which ignores the "- Buffs" and "- Naggy" suffixes people append.
    pub fn observe_spell_set(&mut self, timestamp: i64, set_name: &str) {
        let mut named: Vec<Class> = Vec::new();
        for token in set_name.split(|c: char| !c.is_ascii_alphabetic()) {
            if token.is_empty() {
                continue;
            }
            for i in 0..CLASS_COUNT {
                let class = Class::from_index(i).unwrap();
                if class.matches_token(token) && !named.contains(&class) {
                    named.push(class);
                }
            }
        }
        if !named.is_empty() {
            self.spell_sets.push((timestamp, named));
        }

        for token in set_name.split(|c: char| !c.is_ascii_alphabetic()) {
            if token.is_empty() {
                continue;
            }
            for i in 0..CLASS_COUNT {
                let class = Class::from_index(i).unwrap();
                if class.matches_token(token) {
                    // The reason must name the class. Novelty is measured by
                    // shared reason strings — right for a spell two classes
                    // can cast, but backwards here: a set that names three
                    // classes is separate evidence for each, and a shared
                    // string would let the first pick claim it and make the
                    // other two look like its shadows.
                    self.evidence.push(ClassEvidence {
                        timestamp,
                        class,
                        weight: SPELL_SET_WEIGHT,
                        reason: format!("spell set: {set_name} [{}]", class.abbrev()),
                    });
                }
            }
        }
    }

    /// The game's own statement of the loadout, from a `/who` row.
    ///
    /// Stronger than a spell set name, which is whatever the player typed and
    /// may be months out of date, and stronger than inference from casts. It is
    /// recorded as a spell set so it takes part in the same trio selection,
    /// under a name that makes its origin obvious in the evidence list.
    pub fn observe_who(&mut self, timestamp: i64, classes: &[Class]) {
        if classes.is_empty() {
            return;
        }
        if self.slots() == 1 {
            self.state(timestamp, classes[0]);
        }
        self.spell_sets.push((timestamp, classes.to_vec()));
        for class in classes {
            self.evidence.push(ClassEvidence {
                timestamp,
                class: *class,
                weight: SPELL_SET_WEIGHT,
                reason: format!("/who: {}", abbrev_list(classes)),
            });
        }
    }

    /// Record a skill increase. Ignored unless the skill is diagnostic.
    pub fn observe_skill(&mut self, timestamp: i64, skill: &str) {
        for (name, class, weight) in SKILL_SIGNALS {
            if skill.eq_ignore_ascii_case(name) {
                self.evidence.push(ClassEvidence {
                    timestamp,
                    class: *class,
                    weight: *weight,
                    reason: format!("skill: {skill}"),
                });
            }
        }
    }

    pub fn evidence_count(&self) -> usize {
        self.evidence.len()
    }

    /// Rank classes using only evidence within `[from, to]`.
    ///
    /// Scoping matters: a character's old spells and skills never leave the
    /// log, so an all-time query blends every class they have ever held.
    pub fn ranked_in_window(&self, from: i64, to: i64) -> Vec<ClassScore> {
        let mut totals = [0.0f32; CLASS_COUNT];
        let mut reasons: Vec<Vec<String>> = vec![Vec::new(); CLASS_COUNT];

        for e in self
            .evidence
            .iter()
            .filter(|e| e.timestamp >= from && e.timestamp <= to)
        {
            let i = e.class.index();
            totals[i] += e.weight;
            if !reasons[i].contains(&e.reason) && reasons[i].len() < 8 {
                reasons[i].push(e.reason.clone());
            }
        }

        let sum: f32 = totals.iter().sum();
        let mut out: Vec<ClassScore> = (0..CLASS_COUNT)
            .filter(|i| totals[*i] > 0.0)
            .map(|i| ClassScore {
                class: Class::from_index(i).unwrap(),
                score: totals[i],
                share: if sum > 0.0 { totals[i] / sum } else { 0.0 },
                top_reasons: reasons[i].clone(),
            })
            .collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    /// Rank classes over all evidence.
    pub fn ranked_all_time(&self) -> Vec<ClassScore> {
        self.ranked_in_window(i64::MIN, i64::MAX)
    }

    /// The character's current class trio, judged from the most recent
    /// `window_seconds` of evidence.
    ///
    /// Falls back to widening the window when the recent slice is too thin to
    /// support three classes — a quiet stretch of log should not erase a class.
    /// Rank classes in a window, keeping each class's evidence by reason.
    fn tally(&self, from: i64, to: i64) -> Vec<ClassTally> {
        let mut totals = [0.0f32; CLASS_COUNT];
        let mut reasons: Vec<std::collections::BTreeMap<String, f32>> =
            vec![Default::default(); CLASS_COUNT];
        for e in self
            .evidence
            .iter()
            .filter(|e| e.timestamp >= from && e.timestamp <= to)
        {
            totals[e.class.index()] += e.weight;
            *reasons[e.class.index()]
                .entry(e.reason.clone())
                .or_insert(0.0) += e.weight;
        }
        let mut out: Vec<ClassTally> = (0..CLASS_COUNT)
            .filter(|i| totals[*i] > 0.0)
            .map(|i| ClassTally {
                class: Class::from_index(i).unwrap(),
                score: totals[i],
                reasons: std::mem::take(&mut reasons[i]),
            })
            .collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    /// Pick three classes that each explain evidence the others do not.
    ///
    /// Plain ranking picks shadow classes. Ranger shares Druid's `Chloroplast`
    /// and `Shield of Thorns`; Paladin shares much of Cleric's list. On this
    /// character those score above Warrior — which casts nothing at all and
    /// can only ever produce skill evidence, most of it long since capped and
    /// therefore silent. Ranking alone would report a build the player has
    /// never played.
    ///
    /// So a class must bring its *own* evidence to take a slot: at least
    /// [`NOVEL_EVIDENCE_SHARE`] of its score has to come from observations no
    /// already-chosen class explains. Anything less is a reflection of a class
    /// we have already picked, and is only used to backfill if the character
    /// genuinely has nothing else.
    fn select_trio(&self, from: i64, to: i64) -> Vec<ClassScore> {
        let tallied = self.tally(from, to);
        let sum: f32 = tallied.iter().map(|t| t.score).sum();

        let mut claimed: std::collections::BTreeSet<&str> = Default::default();
        let mut picked: Vec<usize> = Vec::new();
        let mut shadows: Vec<usize> = Vec::new();

        for (i, t) in tallied.iter().enumerate() {
            if picked.len() == self.slots() {
                break;
            }
            let novel: f32 = t
                .reasons
                .iter()
                .filter(|(r, _)| !claimed.contains(r.as_str()))
                .map(|(_, w)| *w)
                .sum();
            if picked.is_empty() || novel >= t.score * NOVEL_EVIDENCE_SHARE {
                claimed.extend(t.reasons.keys().map(|r| r.as_str()));
                picked.push(i);
            } else {
                shadows.push(i);
            }
        }
        // Nothing better on offer: a shadow class still beats a gap.
        for i in shadows {
            if picked.len() == self.slots() {
                break;
            }
            picked.push(i);
        }
        picked.sort_by(|a, b| {
            tallied[*b]
                .score
                .partial_cmp(&tallied[*a].score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        picked
            .into_iter()
            .map(|i| {
                let t = &tallied[i];
                ClassScore {
                    class: t.class,
                    score: t.score,
                    share: if sum > 0.0 { t.score / sum } else { 0.0 },
                    top_reasons: t.reasons.keys().take(8).cloned().collect(),
                }
            })
            .collect()
    }

    /// Every class that has ever appeared in a loaded spell set.
    ///
    /// The record of loadouts actually played, as opposed to classes that
    /// merely share an item or a spell with one. A set is only ever named by
    /// the player for a build they run.
    pub fn classes_ever_declared(&self) -> Vec<Class> {
        let mut out: Vec<Class> = Vec::new();
        for (_, classes) in &self.spell_sets {
            for c in classes {
                if !out.contains(c) {
                    out.push(*c);
                }
            }
        }
        out.sort_by_key(|c| c.index());
        out
    }

    /// The trio named by the spell set loaded nearest `at`.
    ///
    /// A loaded set is the player stating their loadout outright, so it beats
    /// inference for the moment it was loaded — and it changes the instant
    /// they swap, which accumulated evidence cannot. Only a set naming exactly
    /// three classes is trusted: anything else is a partial or oddly-named set
    /// rather than a declaration of the build.
    ///
    /// The catch is that it is player-authored: a set left with its old name
    /// keeps asserting a class no longer played. Nothing in the log can tell
    /// the difference, so this defers to it and the naming is the user's
    /// responsibility.
    fn trio_from_spell_set(&self, at: i64, window: i64) -> Option<Vec<ClassScore>> {
        let classes = self.declared_trio(at, window)?;

        let scored = self.tally(i64::MIN, i64::MAX);
        let total: f32 = scored.iter().map(|t| t.score).sum();
        Some(
            classes
                .iter()
                .map(|c| {
                    let t = scored.iter().find(|t| t.class == *c);
                    ClassScore {
                        class: *c,
                        score: t.map(|t| t.score).unwrap_or(0.0),
                        share: t
                            .map(|t| if total > 0.0 { t.score / total } else { 0.0 })
                            .unwrap_or(0.0),
                        top_reasons: t
                            .map(|t| t.reasons.keys().take(8).cloned().collect())
                            .unwrap_or_default(),
                    }
                })
                .collect(),
        )
    }

    /// Just the classes a declared loadout names, with no scoring.
    ///
    /// [`trio_from_spell_set`](Self::trio_from_spell_set) tallies every piece
    /// of evidence ever recorded to attach scores and reasons for display, at a
    /// cost of a string clone per entry — tens of thousands of allocations on a
    /// long log. Callers that only need to know *which* classes are loaded, and
    /// there are several on hot paths (labelling each kill, and the snapshot
    /// rebuilt four times a second), should not pay for that.
    pub fn declared_trio(&self, at: i64, window: i64) -> Option<Vec<Class>> {
        // One class at a time: the game's last word on it, which holds until
        // the next, however long ago it was.
        if let Some(c) = self.stated_at(at) {
            return Some(vec![c]);
        }
        let slots = self.slots();
        self.spell_sets
            .iter()
            .filter(|(t, c)| c.len() == slots && (at - t).abs() <= window)
            .min_by_key(|(t, _)| (at - t).abs())
            .map(|(_, c)| c.clone())
    }

    /// The trio in effect *around* `at`, using evidence from both sides.
    ///
    /// [`current_trio`](Self::current_trio) looks only backwards, which is
    /// right for "what am I now" but wrong for labelling a past event: a kill
    /// early in the log would be judged on the handful of casts that happened
    /// to precede it. What you played at a moment is better evidenced by the
    /// hours either side of it.
    pub fn trio_around(&self, at: i64, half_window: i64) -> Vec<ClassScore> {
        if let Some(t) = self.trio_from_spell_set(at, half_window) {
            return t;
        }
        for factor in [1, 4, 16, 64] {
            let ranked = self.select_trio(at - half_window * factor, at + half_window * factor);
            if ranked.len() >= self.slots() {
                return ranked;
            }
        }
        self.select_trio(i64::MIN, i64::MAX)
    }

    pub fn current_trio(&self, now: i64, window_seconds: i64) -> Vec<ClassScore> {
        if let Some(t) = self.trio_from_spell_set(now, window_seconds) {
            return t;
        }
        for factor in [1, 4, 16, 64] {
            let ranked = self.select_trio(now - window_seconds * factor, now);
            if ranked.len() >= self.slots() {
                return ranked;
            }
        }
        self.select_trio(i64::MIN, i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(pairs: &[(Class, u8)]) -> [u8; CLASS_COUNT] {
        let mut l = [CLASS_CANNOT_CAST; CLASS_COUNT];
        for (c, lv) in pairs {
            l[c.index()] = *lv;
        }
        l
    }

    /// Swapping one caster for another must be picked up from the new class's
    /// own spells, and must not be blocked by the outgoing class's history.
    #[test]
    fn a_class_swap_is_reflected_once_the_new_class_casts() {
        let mut d = ClassDetector::new();
        // A day as WAR/BRD/DRU.
        for t in 0..200 {
            d.observe_spell(t, "Chords of Dissonance", &levels(&[(Class::Bard, 10)]));
            d.observe_spell(t, "Efflorescing Heal", &levels(&[(Class::Druid, 20)]));
            d.observe_skill(t, "Taunt");
        }
        d.observe_spell_set(100, "WAR / BRD / DRU");

        // Then the swap: shaman spells, and a renamed set.
        for t in 1000..1200 {
            d.observe_spell(t, "Chords of Dissonance", &levels(&[(Class::Bard, 10)]));
            d.observe_spell(t, "Spirit of Bear", &levels(&[(Class::Shaman, 20)]));
            d.observe_skill(t, "Taunt");
        }
        d.observe_spell_set(1100, "WAR / BRD / SHM");

        let now: Vec<&str> = d
            .current_trio(1200, 3600)
            .iter()
            .map(|c| c.class.abbrev())
            .collect();
        assert!(now.contains(&"SHM"), "new class must appear: {now:?}");
        assert!(!now.contains(&"DRU"), "old class must fall away: {now:?}");

        // The past is still read as the past: a kill back then was WAR/BRD/DRU.
        let then: Vec<&str> = d
            .trio_around(100, 3600)
            .iter()
            .map(|c| c.class.abbrev())
            .collect();
        assert!(
            then.contains(&"DRU"),
            "history must not be rewritten: {then:?}"
        );
        assert!(!then.contains(&"SHM"));
    }

    /// A spell set left with its old name keeps asserting the class that is no
    /// longer played — the one way a swap can be reported wrongly.
    #[test]
    fn a_stale_spell_set_name_still_asserts_its_classes() {
        let mut d = ClassDetector::new();
        for t in 1000..1100 {
            d.observe_spell(t, "Chords of Dissonance", &levels(&[(Class::Bard, 10)]));
            d.observe_spell(t, "Spirit of Bear", &levels(&[(Class::Shaman, 20)]));
            d.observe_skill(t, "Taunt");
        }
        // Loaded repeatedly under the old name after the swap.
        for t in 1000..1030 {
            d.observe_spell_set(t, "WAR / BRD / DRU");
        }
        let now: Vec<&str> = d
            .current_trio(1100, 3600)
            .iter()
            .map(|c| c.class.abbrev())
            .collect();
        // Documents the behaviour rather than endorsing it: the set name is
        // player-authored, so a stale one is stale evidence.
        assert!(
            now.contains(&"DRU"),
            "a stale set name still asserts its classes: {now:?}"
        );
    }

    /// The set name is the only place a non-casting class reliably appears.
    #[test]
    fn a_loaded_spell_set_names_the_build() {
        let mut d = ClassDetector::new();
        // Heavy caster evidence, plus the shadow class it drags along.
        for t in 0..40 {
            d.observe_spell(
                t,
                "Chloroplast",
                &levels(&[(Class::Druid, 20), (Class::Ranger, 30)]),
            );
            d.observe_spell(t, "Chords of Dissonance", &levels(&[(Class::Bard, 10)]));
            d.observe_spell(t, "Efflorescing Heal", &levels(&[(Class::Druid, 20)]));
        }
        d.observe_spell_set(50, "WAR / BRD / DRU - Buffs");

        let trio: Vec<&str> = d
            .current_trio(100, 3600)
            .iter()
            .map(|c| c.class.abbrev())
            .collect();
        assert!(
            trio.contains(&"WAR"),
            "set name should carry Warrior: {trio:?}"
        );
        assert!(trio.contains(&"BRD"));
        assert!(trio.contains(&"DRU"));
    }

    /// Every class the set names must keep its own claim on that evidence.
    #[test]
    fn a_set_name_is_separate_evidence_for_each_class_it_names() {
        let mut d = ClassDetector::new();
        d.observe_spell_set(0, "WAR / BRD / DRU");
        let reasons: Vec<String> = d
            .ranked_all_time()
            .iter()
            .flat_map(|c| c.top_reasons.clone())
            .collect();
        let unique: std::collections::BTreeSet<&String> = reasons.iter().collect();
        assert_eq!(
            unique.len(),
            3,
            "one distinct reason per class: {reasons:?}"
        );
    }

    /// Suffixes people append to set names must not be read as classes.
    #[test]
    fn only_exact_class_abbreviations_count_in_a_set_name() {
        let mut d = ClassDetector::new();
        d.observe_spell_set(0, "WAR / BRD / DRU - Naggy");
        let got: Vec<&str> = d
            .ranked_all_time()
            .iter()
            .map(|c| c.class.abbrev())
            .collect();
        assert_eq!(got.len(), 3, "got {got:?}");
    }

    /// A near-universal ability is noise, and noise cast often enough will
    /// outweigh a class's real but infrequent evidence.
    #[test]
    fn spells_almost_everyone_can_cast_are_not_evidence() {
        let mut d = ClassDetector::new();
        let universal = {
            let mut l = [20u8; CLASS_COUNT];
            l[Class::Bard.index()] = 20;
            l
        };
        for t in 0..500 {
            d.observe_spell(t, "Origin", &universal);
        }
        assert_eq!(
            d.evidence_count(),
            0,
            "universal ability recorded as evidence"
        );

        // A spell a handful of classes share is still evidence.
        d.observe_spell(
            1,
            "Superior Healing",
            &levels(&[(Class::Cleric, 20), (Class::Druid, 25)]),
        );
        assert_eq!(d.evidence_count(), 2);
    }

    /// Ranger casts Druid spells; on a Druid character that is not evidence of
    /// being a Ranger, and must not displace a class that casts nothing.
    #[test]
    fn a_class_explaining_nothing_new_loses_to_one_with_its_own_evidence() {
        let mut d = ClassDetector::new();
        // Two spells both classes share, several times over.
        for t in 0..8 {
            d.observe_spell(
                t,
                "Chloroplast",
                &levels(&[(Class::Druid, 20), (Class::Ranger, 30)]),
            );
            d.observe_spell(
                t,
                "Shield of Thorns",
                &levels(&[(Class::Druid, 20), (Class::Ranger, 30)]),
            );
        }
        // Druid-only spells: the reason Druid outranks its own shadow.
        for t in 0..8 {
            d.observe_spell(t, "Efflorescing Heal", &levels(&[(Class::Druid, 20)]));
        }
        // Bard's own songs.
        for t in 0..8 {
            d.observe_spell(t, "Chords of Dissonance", &levels(&[(Class::Bard, 10)]));
        }
        // Warrior evidence is skills only, and thinner.
        d.observe_skill(1, "Taunt");
        d.observe_skill(2, "Disarm");

        let trio: Vec<&str> = d
            .current_trio(100, 3600)
            .iter()
            .map(|c| c.class.abbrev())
            .collect();
        assert!(trio.contains(&"WAR"), "expected WAR in {trio:?}");
        assert!(
            !trio.contains(&"RNG"),
            "RNG adds nothing Druid does not explain: {trio:?}"
        );
    }

    #[test]
    fn class_order_matches_spell_file_layout() {
        // The order is load-bearing: it maps field 36+i to a class.
        assert_eq!(Class::from_index(0), Some(Class::Warrior));
        assert_eq!(Class::from_index(5), Some(Class::Druid));
        assert_eq!(Class::from_index(7), Some(Class::Bard));
        assert_eq!(Class::from_index(13), Some(Class::Enchanter));
        assert_eq!(Class::ALL.len(), CLASS_COUNT);
    }

    #[test]
    fn single_class_spell_is_decisive() {
        let mut d = ClassDetector::new();
        d.observe_spell(100, "Chords of Dissonance", &levels(&[(Class::Bard, 2)]));
        let r = d.ranked_all_time();
        assert_eq!(r[0].class, Class::Bard);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn shared_spell_contributes_less_than_exclusive_one() {
        let mut d = ClassDetector::new();
        // A spell six classes share.
        let shared = levels(&[
            (Class::Cleric, 1),
            (Class::Druid, 1),
            (Class::Shaman, 1),
            (Class::Wizard, 1),
            (Class::Magician, 1),
            (Class::Enchanter, 1),
        ]);
        d.observe_spell(100, "Gate", &shared);
        d.observe_spell(100, "Chords of Dissonance", &levels(&[(Class::Bard, 2)]));
        let r = d.ranked_all_time();
        assert_eq!(
            r[0].class,
            Class::Bard,
            "exclusive spell should outrank shared"
        );
        assert!(r[0].score > r[1].score * 3.0);
    }

    #[test]
    fn skills_detect_classes_that_have_no_spells() {
        let mut d = ClassDetector::new();
        d.observe_skill(100, "Backstab");
        d.observe_skill(100, "Pick Pockets");
        let r = d.ranked_all_time();
        assert_eq!(r[0].class, Class::Rogue);
        assert!(Class::Rogue.is_pure_melee());
    }

    #[test]
    fn undiagnostic_skills_are_ignored() {
        let mut d = ClassDetector::new();
        d.observe_skill(100, "Defense");
        d.observe_skill(100, "1H Slashing");
        assert_eq!(d.evidence_count(), 0);
    }

    /// The headline case: a character who changed classes must report the
    /// *current* trio, not an all-time blend.
    #[test]
    fn window_isolates_the_current_trio() {
        let mut d = ClassDetector::new();
        // Era 1: warrior/enchanter.
        d.observe_skill(1_000, "Taunt");
        d.observe_skill(1_000, "Disarm");
        d.observe_spell(1_000, "Chaotic Feedback", &levels(&[(Class::Enchanter, 7)]));
        d.observe_spell(1_050, "Color Flux", &levels(&[(Class::Enchanter, 2)]));
        // Era 2: warrior/druid/bard, much later.
        d.observe_skill(500_000, "Taunt");
        d.observe_skill(500_100, "Singing");
        d.observe_skill(500_200, "Stringed Instruments");
        d.observe_spell(
            500_300,
            "Chords of Dissonance",
            &levels(&[(Class::Bard, 2)]),
        );
        d.observe_spell(500_400, "Thorncoat", &levels(&[(Class::Druid, 20)]));
        d.observe_spell(500_500, "Chloroplast", &levels(&[(Class::Druid, 30)]));

        let trio = d.current_trio(500_600, 10_000);
        let classes: Vec<Class> = trio.iter().map(|c| c.class).collect();
        assert!(classes.contains(&Class::Bard), "got {classes:?}");
        assert!(classes.contains(&Class::Druid), "got {classes:?}");
        assert!(classes.contains(&Class::Warrior), "got {classes:?}");
        assert!(
            !classes.contains(&Class::Enchanter),
            "old enchanter era must not leak into the current trio: {classes:?}"
        );

        // All-time still remembers the enchanter phase.
        let all: Vec<Class> = d.ranked_all_time().iter().map(|c| c.class).collect();
        assert!(all.contains(&Class::Enchanter));
    }

    /// "ZERK" is what a berserker is actually called; the abbreviation is BER.
    /// Missing it left `WAR / DRU / ZERK` naming only two classes, so it was
    /// discarded as not a trio and the previous loadout stayed asserted.
    #[test]
    fn zerk_names_the_berserker() {
        let mut d = ClassDetector::new();
        d.observe_spell_set(1000, "WAR / DRU / ZERK");
        let trio = d.current_trio(1100, 3600);
        let mut names: Vec<&str> = trio.iter().map(|c| c.class.abbrev()).collect();
        names.sort();
        assert_eq!(names, vec!["BER", "DRU", "WAR"]);
    }

    /// A set name is player prose; only whole class words count. "CHA" in a
    /// buff set must not conjure an enchanter.
    #[test]
    fn set_names_do_not_match_stray_words() {
        let mut d = ClassDetector::new();
        d.observe_spell_set(1000, "Naggy - CHA and AGI buffs");
        assert!(d.spell_sets.is_empty() || d.spell_sets[0].1.len() < 3);
    }

    #[test]
    fn current_trio_widens_window_when_evidence_is_thin() {
        let mut d = ClassDetector::new();
        d.observe_spell(1_000, "Chords of Dissonance", &levels(&[(Class::Bard, 2)]));
        d.observe_spell(1_000, "Thorncoat", &levels(&[(Class::Druid, 20)]));
        d.observe_skill(1_000, "Taunt");
        // Query a window containing no evidence at all; it must widen rather
        // than report nothing.
        let trio = d.current_trio(2_000, 10);
        assert_eq!(trio.len(), 3, "should widen to find the trio");
    }

    #[test]
    fn shares_sum_to_one() {
        let mut d = ClassDetector::new();
        d.observe_spell(1, "A", &levels(&[(Class::Bard, 2)]));
        d.observe_spell(1, "B", &levels(&[(Class::Druid, 2)]));
        let r = d.ranked_all_time();
        let total: f32 = r.iter().map(|c| c.share).sum();
        assert!((total - 1.0).abs() < 1e-5, "shares summed to {total}");
    }

    /// One slot (EverQuest): the top scorer takes it,
    /// and a `/who` row answers back to the start of the log when nothing
    /// was stated before it.
    #[test]
    fn one_slot_reads_one_class_and_who_settles_the_log() {
        let mut d = ClassDetector::with_slots(1);
        for t in 0..20 {
            d.observe_spell(t, "Grim Aura", &levels(&[(Class::Necromancer, 4), (Class::ShadowKnight, 9)]));
        }
        let now: Vec<Class> = d.current_trio(100, 3600).into_iter().map(|c| c.class).collect();
        assert_eq!(now.len(), 1, "one class, not a trio: {now:?}");
        d.observe_who(10_000_000, &[Class::ShadowKnight]);
        assert_eq!(d.declared_trio(0, 60), Some(vec![Class::ShadowKnight]), "back to the start, not a window");
    }

    /// The class channel is evidence a one-class log usually has; zone and
    /// general channels name nothing, and `/who` beats the channel.
    #[test]
    fn the_class_channel_names_the_class_and_loses_to_who() {
        let mut d = ClassDetector::with_slots(1);
        for t in 0..20 {
            d.observe_spell(t, "Grim Aura", &levels(&[(Class::Necromancer, 4), (Class::ShadowKnight, 9)]));
        }
        d.observe_channels(100, &["General".into(), "Necromancer".into(), "Antonica".into()]);
        assert_eq!(d.current_trio(200, 3600)[0].class, Class::Necromancer);
        let mut q = ClassDetector::with_slots(1);
        q.observe_channels(0, &["General1".into(), "Kunark".into(), "Housing".into()]);
        assert_eq!(q.evidence_count(), 0);
        assert_eq!(parse_class_name("Shadowknight"), Some(Class::ShadowKnight));
        assert_eq!(parse_class_name("WAR"), None, "abbreviations are not channel names");
        d.observe_who(300, &[Class::Wizard]);
        assert_eq!(d.declared_trio(400, 60), Some(vec![Class::Wizard]));
    }

    /// One class at a time, not for life: a live EverQuest character can
    /// switch class, and the client re-joins it to the new class channel.
    /// Each statement holds until the next, so a level-up between them goes
    /// to the class in force, not to whichever the log stated last. From the
    /// Garrow log, a Warrior at 130 levelling a Shaman from 42.
    #[test]
    fn a_class_switch_holds_until_the_next_statement() {
        let mut d = ClassDetector::with_slots(1);
        d.observe_who(100, &[Class::Warrior]);
        d.observe_channels(200, &["General".into(), "Shaman".into()]);
        d.observe_channels(300, &["General".into(), "Warrior".into()]);
        d.observe_channels(400, &["General".into(), "Warrior".into(), "Shaman".into()]);
        let at = |t| d.declared_trio(t, 60).unwrap()[0];
        assert_eq!(at(0), Class::Warrior, "before anything is stated, the first statement");
        assert_eq!(at(150), Class::Warrior);
        assert_eq!(at(250), Class::Shaman, "long after any window, still the Shaman");
        assert_eq!(at(10_000), Class::Warrior, "two class channels state nothing");
    }

    /// Legends keeps three slots and its window: one `/who` does not settle
    /// a log whose loadout can change.
    #[test]
    fn three_slots_keep_the_window() {
        let mut d = ClassDetector::new();
        d.observe_who(0, &[Class::Warrior, Class::Bard, Class::Shaman]);
        assert_eq!(d.declared_trio(10, 60).map(|t| t.len()), Some(3));
        assert_eq!(d.declared_trio(10_000, 60), None);
    }
}
