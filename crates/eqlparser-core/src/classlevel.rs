//! What level each class is.
//!
//! # Why this is not simply "the level"
//!
//! EverQuest Legends levels each class in a loadout separately, and the client
//! reports only one number:
//!
//! ```text
//! [Fri Jul 31 13:19:15 2026] You have gained a level! Welcome to level 50!
//! [Sun Aug 02 14:07:17 2026] You have gained a level! Welcome to level 11!
//! [Tue Aug 04 00:21:30 2026] You have gained a level! Welcome to level 11!
//! ```
//!
//! Nothing went backwards. A class hit the cap, a new one was swapped in at 10
//! and levelled to 28, then a third was swapped in and started again. Read as a
//! single character level it is nonsense; read per class it is exactly right.
//!
//! # The two things the log actually states
//!
//! **`/who` on yourself.** `[50 WAR/BRD/SHM] Tharn (Ogre)` is the only line
//! carrying a level and a loadout together, and for the player's own row it is
//! the game's own answer rather than an inference. It costs a keystroke, so it
//! is rare — worth using whenever it appears, not something to depend on.
//!
//! The single number it shows is the **lowest** class's level — the character's
//! effective level, gated by the weakest of the three. Observed outright:
//!
//! ```text
//! [Tue Aug 04 16:03:50 2026] [28 WAR/BRD/SHM] Tharn (Iksar)
//! ```
//!
//! with the warrior and bard both capped at 50 and the shaman at 28. It is not
//! the primary class's level, which was the first guess and which this row
//! disproves. Other players' rows agree: one went 49 to 50 across two days on
//! an unchanged loadout, which is the lowest class finishing, not the first.
//!
//! That makes a `/who` row a floor for every class in the loadout, and exact
//! for whichever is lowest.
//!
//! **Class proficiency achievements.** `Druid's Combat Proficiency, Level 50`
//! names a class and a level outright. This matters more than it looks: 50 is
//! the cap, so a capped class never emits another level-up message. Without
//! these, a class that finished levelling before the log began is invisible.
//!
//! # Attributing a level-up
//!
//! A level-up names no class, so it is attributed to the only class in the
//! active loadout that could have gained it — one below the cap. When exactly
//! one qualifies the attribution is certain; when several do it is not, and the
//! level is recorded against the loadout as unattributed rather than guessed
//! at. Guessing here would quietly corrupt the per-class record, which is the
//! one thing this module exists to keep straight.

use crate::class::Class;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The level cap. A class here stops emitting level-up messages entirely.
pub const LEVEL_CAP: u32 = 50;

/// Level a class starts at when swapped into a loadout.
///
/// Both classes added on this character reported their first gain as 11, so
/// they entered at 10 rather than 1. Deliberately not applied as a default: a
/// class with no evidence is reported as unknown, because a level assumed onto
/// a class is sticky, and it also makes that class look like a candidate for
/// every later level-up and so destroys the attribution of the class actually
/// being played.
pub const STARTING_LEVEL: u32 = 10;

/// How a class's level came to be known, worst to best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LevelSource {
    /// A level-up message attributed to this class as the only candidate.
    LevelUp,
    /// A class proficiency achievement naming the class and level.
    Achievement,
    /// A `/who` row for the player.
    Who,
}

impl LevelSource {
    pub fn label(self) -> &'static str {
        match self {
            LevelSource::LevelUp => "levelled",
            LevelSource::Achievement => "achievement",
            LevelSource::Who => "/who",
        }
    }
}

/// One class's level, and how confident we are in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassLevel {
    pub class: Class,
    pub abbrev: String,
    pub name: String,
    pub level: u32,
    pub source: LevelSource,
    /// Source as a word, for display.
    pub source_label: String,
    /// When it was last established.
    pub at: i64,
    /// At the cap, so it will never report another level-up.
    pub capped: bool,
}

/// Everything the levels view shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassLevelReport {
    /// Highest first.
    pub classes: Vec<ClassLevel>,
    /// The class currently earning experience, when it can be pinned down.
    pub levelling: Option<String>,
    /// Classes in the active loadout whose level is not known.
    pub unknown: Vec<String>,
    /// True when the loadout changed more recently than any level evidence, so
    /// the displayed level may be stale.
    pub needs_who: bool,
    /// The character's effective level: the lowest in the loadout, which is
    /// what `/who` reports and what the game gates content on.
    pub effective: Option<u32>,
    /// Level-ups seen but not attributable to a class.
    pub unattributed: usize,
}

/// Tracks each class's level independently.
#[derive(Debug, Default)]
pub struct ClassLevelTracker {
    levels: HashMap<Class, (u32, LevelSource, i64)>,
    /// The most recent level-up, whichever class earned it.
    last_level_up: Option<(i64, u32)>,
    /// Level-ups that could not be attributed when they arrived, kept in case
    /// later evidence narrows the field.
    pending: Vec<(i64, u32, Vec<Class>)>,
    /// Guard against `record` and `resolve_pending` calling each other.
    resolving: bool,
    /// When the loadout was last observed to change.
    loadout_changed_at: i64,
    last_loadout: Vec<Class>,
}

impl ClassLevelTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a level, keeping the highest seen.
    ///
    /// Levels only ever go up, so the larger number wins outright regardless of
    /// where it came from. Ranking sources above that would let a stale reading
    /// outlive the truth: this character's only `/who` is from the first day
    /// and says the warrior is 8, and treating `/who` as unassailable pinned it
    /// there — which in turn made every later level-up ambiguous, since a
    /// level-8 warrior looks like a plausible earner of a level-11 gain. Source
    /// is kept for display, and breaks ties at equal levels.
    fn record(&mut self, class: Class, level: u32, source: LevelSource, at: i64) {
        match self.levels.get(&class) {
            Some((lv, _, _)) if *lv > level => {}
            Some((lv, have, _)) if *lv == level && *have >= source => {}
            _ => {
                self.levels.insert(class, (level, source, at));
                self.resolve_pending();
            }
        }
    }

    /// Retry level-ups that were ambiguous when they arrived.
    ///
    /// Learning one class's level often disambiguates earlier gains: a single
    /// `/who` confirming the primary is capped can retrospectively attribute
    /// every level the new class earned. Repeats until nothing more resolves,
    /// since each attribution may unlock the next.
    fn resolve_pending(&mut self) {
        if self.resolving || self.pending.is_empty() {
            return;
        }
        self.resolving = true;
        loop {
            let mut progressed = false;
            let pending = std::mem::take(&mut self.pending);
            for (at, level, loadout) in pending {
                match self.only_candidate(level, &loadout) {
                    Some(c) => {
                        self.record(c, level, LevelSource::LevelUp, at);
                        progressed = true;
                    }
                    None => self.pending.push((at, level, loadout)),
                }
            }
            if !progressed {
                break;
            }
        }
        self.resolving = false;
    }

    /// The sole class in a loadout that could have earned a level.
    ///
    /// Anything at the cap is out — it cannot gain a level. Anything already
    /// recorded at or above this level is out too: a level-11 gain cannot
    /// belong to the class sitting at 50.
    fn only_candidate(&self, level: u32, loadout: &[Class]) -> Option<Class> {
        let mut found = None;
        for c in loadout {
            let eligible = match self.level_of(*c) {
                Some(l) => l < LEVEL_CAP && l < level,
                None => true,
            };
            if eligible {
                if found.is_some() {
                    return None;
                }
                found = Some(*c);
            }
        }
        found
    }

    /// A `/who` row for the player: level and loadout, straight from the game.
    ///
    /// Being the lowest class's level, it is a floor for the whole loadout:
    /// nothing in it is below that number. Recorded against every class, which
    /// with the highest-wins rule does exactly the right thing — a capped class
    /// is untouched, and the class actually sitting at that level is set
    /// precisely, because for it the floor *is* its level.
    pub fn on_who(&mut self, at: i64, level: u32, classes: &[Class]) {
        self.note_loadout(at, classes);
        for c in classes {
            self.record(*c, level, LevelSource::Who, at);
        }
    }

    /// A class proficiency achievement, e.g. "Druid's Combat Proficiency,
    /// Level 50".
    ///
    /// Returns the class and level when the text is one of those, so the caller
    /// can report it. Other achievements are ignored.
    pub fn on_achievement(&mut self, at: i64, text: &str) -> Option<(Class, u32)> {
        let (class_part, rest) = text.split_once("'s ")?;
        if !rest.contains("Proficiency") {
            return None;
        }
        let level: u32 = rest.rsplit_once("Level ")?.1.trim().parse().ok()?;
        let class = (0..crate::class::CLASS_COUNT)
            .filter_map(Class::from_index)
            .find(|c| c.name().eq_ignore_ascii_case(class_part.trim()))?;
        self.record(class, level, LevelSource::Achievement, at);
        Some((class, level))
    }

    /// A level-up, attributed to the only class that could have earned it.
    ///
    /// Returns the class when the attribution is certain.
    pub fn on_level_up(&mut self, at: i64, level: u32, loadout: &[Class]) -> Option<Class> {
        self.last_level_up = Some((at, level));
        match self.only_candidate(level, loadout) {
            Some(c) => {
                self.record(c, level, LevelSource::LevelUp, at);
                Some(c)
            }
            None => {
                self.pending.push((at, level, loadout.to_vec()));
                None
            }
        }
    }

    /// Level-ups still waiting on evidence to say whose they were.
    pub fn unattributed(&self) -> usize {
        self.pending.len()
    }

    /// Note the loadout in effect, so a swap can be spotted.
    pub fn note_loadout(&mut self, at: i64, classes: &[Class]) {
        if classes.is_empty() {
            return;
        }
        let mut sorted = classes.to_vec();
        sorted.sort_by_key(|c| c.index());
        if sorted != self.last_loadout {
            if !self.last_loadout.is_empty() {
                self.loadout_changed_at = at;
            }
            self.last_loadout = sorted.clone();
        }
        let _ = sorted;
    }

    pub fn level_of(&self, class: Class) -> Option<u32> {
        self.levels.get(&class).map(|(l, _, _)| *l)
    }

    pub fn is_capped(&self, class: Class) -> bool {
        self.level_of(class).is_some_and(|l| l >= LEVEL_CAP)
    }

    /// The class currently earning experience: the sole uncapped one.
    pub fn levelling(&self, loadout: &[Class]) -> Option<Class> {
        let uncapped: Vec<Class> = loadout
            .iter()
            .copied()
            .filter(|c| !self.is_capped(*c))
            .collect();
        (uncapped.len() == 1).then(|| uncapped[0])
    }

    /// `played` restricts what is shown to classes the character has actually
    /// declared, which matters because attribution depends on knowing the
    /// loadout at the time. Early in a log the loadout is inferred rather than
    /// declared and can be wrong, and a level credited to a class that was
    /// never played shows up as a phantom "Wizard 4" that no amount of later
    /// evidence removes. Attribution itself is left alone — the filter is only
    /// on what is reported.
    pub fn report(&self, loadout: &[Class], played: &[Class]) -> ClassLevelReport {
        let mut classes: Vec<ClassLevel> = self
            .levels
            .iter()
            .filter(|(c, _)| played.is_empty() || played.contains(c) || loadout.contains(c))
            .map(|(c, (level, source, at))| ClassLevel {
                class: *c,
                abbrev: c.abbrev().to_string(),
                name: c.name().to_string(),
                level: *level,
                source: *source,
                source_label: source.label().to_string(),
                at: *at,
                capped: *level >= LEVEL_CAP,
            })
            .collect();
        classes.sort_by(|a, b| b.level.cmp(&a.level).then(a.abbrev.cmp(&b.abbrev)));

        let unknown: Vec<String> = loadout
            .iter()
            .filter(|c| !self.levels.contains_key(c))
            .map(|c| c.abbrev().to_string())
            .collect();

        // A swap with no level evidence since means whatever is displayed for
        // the new class predates it. One /who settles it.
        let last_evidence = self
            .levels
            .values()
            .map(|(_, _, at)| *at)
            .max()
            .unwrap_or(0);
        // Unresolved level-ups are the sharpest signal: levels demonstrably
        // happened and there is no way to say whose. One /who names the
        // primary, which is usually enough to settle all of them at once.
        // Only level-ups from the current loadout count. Ambiguities left over
        // from months ago are not something a /who now can settle, and nagging
        // about them forever would train the player to ignore the prompt.
        let stale_pending = self.pending.iter().any(|(at, _, l)| {
            *at >= self.loadout_changed_at && l.iter().any(|c| loadout.contains(c))
        });
        let needs_who = !unknown.is_empty()
            || stale_pending
            || (self.loadout_changed_at > 0 && last_evidence < self.loadout_changed_at);

        ClassLevelReport {
            levelling: self.levelling(loadout).map(|c| c.abbrev().to_string()),
            classes,
            unknown,
            needs_who,
            effective: loadout.iter().filter_map(|c| self.level_of(*c)).min(),
            unattributed: self.pending.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sequence this character actually produced: a class caps at 50, a
    /// new one is swapped in and levels from 11, then a third replaces it and
    /// starts over. Read as one character level it looks like the player
    /// de-levelled twice.
    #[test]
    fn a_swap_does_not_look_like_going_backwards() {
        let mut t = ClassLevelTracker::new();
        let main = [Class::Warrior, Class::Bard, Class::Druid];
        t.note_loadout(0, &main);
        t.on_achievement(100, "Druid's Combat Proficiency, Level 50");
        t.on_achievement(110, "Bard's Casting Proficiency, Level 50");
        // The warrior is primary, so /who is what establishes its 50.
        t.on_who(120, 50, &main);
        assert_eq!(t.level_of(Class::Warrior), Some(50));

        // Shaman replaces the druid and levels from scratch.
        let with_shm = [Class::Warrior, Class::Bard, Class::Shaman];
        t.note_loadout(200, &with_shm);
        assert_eq!(
            t.on_level_up(210, 11, &with_shm),
            Some(Class::Shaman),
            "the only class below the cap must take the level"
        );
        t.on_level_up(300, 28, &with_shm);

        // Berserker replaces the shaman.
        let with_ber = [Class::Warrior, Class::Bard, Class::Berserker];
        t.note_loadout(400, &with_ber);
        assert_eq!(t.on_level_up(410, 11, &with_ber), Some(Class::Berserker));
        t.on_level_up(420, 13, &with_ber);

        // Every class keeps its own level.
        assert_eq!(t.level_of(Class::Druid), Some(50));
        assert_eq!(t.level_of(Class::Bard), Some(50));
        assert_eq!(t.level_of(Class::Shaman), Some(28));
        assert_eq!(t.level_of(Class::Berserker), Some(13));
        assert_eq!(t.levelling(&with_ber), Some(Class::Berserker));
    }

    /// The whole point of reading the achievements: a class that capped before
    /// the log began never emits another level-up, so nothing else can say what
    /// level it is.
    #[test]
    fn a_capped_class_is_known_only_from_its_achievement() {
        let mut t = ClassLevelTracker::new();
        assert_eq!(
            t.on_achievement(100, "Druid's Combat Proficiency, Level 50"),
            Some((Class::Druid, 50))
        );
        assert!(t.is_capped(Class::Druid));
        // Achievements that name no class are not levels.
        assert_eq!(t.on_achievement(100, "Islands of Sky Keys"), None);
        assert_eq!(t.on_achievement(100, "Smithing (50)"), None);
    }

    /// The row that settled what the number means: "[28 WAR/BRD/SHM]" with two
    /// classes capped. It names the shaman's level, and must not touch theirs.
    #[test]
    fn who_names_the_lowest_class_not_the_first() {
        let mut t = ClassLevelTracker::new();
        let load = [Class::Warrior, Class::Bard, Class::Shaman];
        t.on_achievement(100, "Warrior's Combat Proficiency, Level 50");
        t.on_achievement(110, "Bard's Casting Proficiency, Level 50");
        t.on_who(120, 28, &load);
        assert_eq!(t.level_of(Class::Shaman), Some(28));
        assert_eq!(t.level_of(Class::Warrior), Some(50), "not demoted to 28");
        assert_eq!(t.level_of(Class::Bard), Some(50));
        assert_eq!(t.report(&load, &[]).levelling.as_deref(), Some("SHM"));
    }

    /// With nothing known, the number is only a floor — it says the lowest is
    /// 8, not that both are.
    #[test]
    fn who_with_nothing_known_records_a_floor() {
        let mut t = ClassLevelTracker::new();
        let load = [Class::Warrior, Class::Enchanter];
        t.on_who(100, 8, &load);
        assert_eq!(t.level_of(Class::Warrior), Some(8));
        assert_eq!(t.level_of(Class::Enchanter), Some(8));
        // And a later row does not drag a class back down.
        t.on_achievement(200, "Warrior's Combat Proficiency, Level 50");
        t.on_who(300, 12, &load);
        assert_eq!(t.level_of(Class::Warrior), Some(50));
        assert_eq!(t.level_of(Class::Enchanter), Some(12));
    }

    /// Two classes could have earned it, so neither is credited. A wrong
    /// attribution is worse than none: it sticks.
    #[test]
    fn an_ambiguous_level_up_is_not_attributed() {
        let mut t = ClassLevelTracker::new();
        let load = [Class::Warrior, Class::Rogue, Class::Monk];
        t.note_loadout(0, &load);
        assert_eq!(t.on_level_up(100, 20, &load), None);
        for c in load {
            assert_eq!(t.level_of(c), None, "unknown beats a wrong guess");
        }
        assert!(t.report(&load, &[]).needs_who);
    }

    /// Levels that could not be attributed when they happened are kept, and
    /// settled the moment later evidence rules the other candidates out.
    #[test]
    fn evidence_resolves_a_backlog_retrospectively() {
        let mut t = ClassLevelTracker::new();
        let load = [Class::Warrior, Class::Bard, Class::Shaman];
        t.note_loadout(100, &load);
        assert_eq!(t.on_level_up(200, 11, &load), None, "any of the three");
        assert_eq!(t.on_level_up(210, 12, &load), None);
        assert_eq!(t.unattributed(), 2);

        t.on_achievement(300, "Warrior's Combat Proficiency, Level 50");
        assert_eq!(t.unattributed(), 2, "two candidates left");
        t.on_achievement(310, "Bard's Casting Proficiency, Level 50");

        assert_eq!(t.level_of(Class::Shaman), Some(12));
        assert_eq!(t.unattributed(), 0);
        assert_eq!(t.report(&load, &[]).levelling.as_deref(), Some("SHM"));
    }

    /// A floor from day one must not survive as the answer.
    #[test]
    fn a_later_who_raises_a_stale_floor() {
        let mut t = ClassLevelTracker::new();
        t.on_who(100, 8, &[Class::Warrior, Class::Enchanter]);
        assert_eq!(t.level_of(Class::Warrior), Some(8));
        t.on_who(200, 28, &[Class::Warrior, Class::Bard, Class::Shaman]);
        assert_eq!(t.level_of(Class::Warrior), Some(28), "floor raised");
        assert_eq!(t.level_of(Class::Enchanter), Some(8), "not in the loadout");
    }

    /// A stronger source is never undone by a weaker one.
    #[test]
    fn evidence_quality_is_respected() {
        let mut t = ClassLevelTracker::new();
        let load = [Class::Druid, Class::Warrior, Class::Bard];
        t.on_who(100, 50, &load);
        // A level-up cannot demote what /who established.
        t.on_level_up(200, 12, &load);
        assert_eq!(t.level_of(Class::Druid), Some(50));
    }

    /// After a swap with nothing to go on, say so rather than showing the
    /// outgoing class's level as if it were the new one's.
    #[test]
    fn a_swap_without_evidence_asks_for_a_who() {
        let mut t = ClassLevelTracker::new();
        let before = [Class::Warrior, Class::Bard, Class::Druid];
        t.on_who(100, 50, &before);
        t.on_achievement(110, "Bard's Casting Proficiency, Level 50");
        t.on_achievement(120, "Druid's Combat Proficiency, Level 50");
        let after = [Class::Warrior, Class::Bard, Class::Shaman];
        t.note_loadout(200, &after);
        let r = t.report(&after, &[]);
        assert!(r.needs_who, "nothing has confirmed the shaman's level");
        assert_eq!(r.unknown, vec!["SHM"]);

        // A level-up for the only uncapped class settles it.
        assert_eq!(t.on_level_up(210, 11, &after), Some(Class::Shaman));
        let r = t.report(&after, &[]);
        assert!(!r.needs_who);
        assert!(r.unknown.is_empty());
        assert_eq!(r.levelling.as_deref(), Some("SHM"));
    }
}
