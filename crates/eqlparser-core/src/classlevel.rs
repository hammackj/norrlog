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

/// Legends' level cap, the default. A class here stops emitting level-up
/// messages entirely. The tracker holds its own cap, from the game's
/// [`crate::profile::Profile`]; see [`ClassLevelTracker::with_cap`].
pub const LEVEL_CAP: u32 = 50;

/// Level every class sits at until it is played.
///
/// Confirmed by the player against their own character sheet: the five classes
/// they have levelled read 50/50/50/28/13 and "everything else is level 10".
/// It matches the log too — both classes swapped in reported their first gain
/// as 11, so they entered at 10 rather than 1.
///
/// It applies to a class that has not been played. A character's *first* class
/// does start lower — this one's warrior was 8 on day one, and a character
/// rolled today is level 1 — so this is an assumption about an unplayed class
/// and not a floor under the character. It used to act as one: `record` keeps
/// the highest level seen, so on a level-3 character every level-up *and* the
/// `/who` row confirming it were all discarded for being below 10, and the
/// overlay printed `lvl 10` with no evidence able to correct it. See
/// [`LevelSource::Baseline`] for the rule that stops it.
pub const STARTING_LEVEL: u32 = 10;

/// How a class's level came to be known, worst to best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LevelSource {
    /// The level every class holds until it is played.
    ///
    /// An assumption, not an observation, and the only variant here that is.
    /// So it does not take part in the highest-wins rule in [`record`]: the
    /// first thing the log actually says about a class replaces it, in either
    /// direction. Monotonicity is a property of levels, not of guesses about
    /// them, and applying it to this one made a new character unrepresentable.
    ///
    /// [`record`]: ClassLevelTracker::record
    Baseline,
    /// A level-up message attributed to this class as the only candidate.
    LevelUp,
    /// A class proficiency achievement naming the class and level.
    Achievement,
    /// A `/who` row for the player.
    Who,
    /// Entered by the player, who can read their own character sheet.
    Stated,
}

impl LevelSource {
    pub fn label(self) -> &'static str {
        match self {
            LevelSource::Baseline => "unplayed",
            LevelSource::LevelUp => "levelled",
            LevelSource::Achievement => "achievement",
            LevelSource::Who => "/who",
            LevelSource::Stated => "stated",
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
    /// Levels the player gave, with when they gave them.
    stated: std::collections::HashMap<Class, (u32, i64)>,
    /// When the loadout was last observed to change.
    loadout_changed_at: i64,
    last_loadout: Vec<Class>,
    /// The game's level cap.
    cap: u32,
}

impl ClassLevelTracker {
    pub fn new() -> Self {
        Self::with_cap(LEVEL_CAP)
    }

    /// The level a class stops at in this tracker's game.
    pub fn cap(&self) -> u32 {
        self.cap
    }

    /// A tracker for a game whose classes stop at `cap`.
    pub fn with_cap(cap: u32) -> Self {
        let mut t = Self { cap, ..Self::default() };
        // Every class starts known, not unknown: an unplayed class is level 10,
        // which is a fact rather than a guess. It also sharpens attribution —
        // a gain to 11 belongs to whichever class is sitting at 10.
        for c in (0..crate::class::CLASS_COUNT).filter_map(Class::from_index) {
            t.levels
                .insert(c, (STARTING_LEVEL, LevelSource::Baseline, 0));
        }
        t
    }

    /// A level the player entered by hand.
    ///
    /// The strongest thing available at the moment it is given — some facts the
    /// log simply cannot supply, since a class that capped before logging began
    /// emits no level-up and never will, and `/who` only reports the lowest of
    /// three. It is not frozen afterwards, though: the class carries on
    /// levelling, and highest-wins lets it.
    pub fn set_stated(&mut self, class: Class, level: u32, at: i64) {
        self.stated.insert(class, (level, at));
        self.levels.insert(class, (level, LevelSource::Stated, at));
        self.resolve_pending();
    }

    /// Record a level, keeping the highest seen.
    ///
    /// # The precedence, in one place
    ///
    /// Written out because until now it existed only as the order of the four
    /// match arms below and the order of [`LevelSource`]'s variants, and a
    /// rule that is only implied by the order of some code is one nobody can
    /// check a bug against. In order, first match wins:
    ///
    /// 1. **A statement is not retroactive.** Evidence timestamped *before* a
    ///    stated level, claiming *more* than it, is dropped. The player read
    ///    their character sheet; the log cannot have known better earlier.
    /// 2. **Anything observed replaces the baseline**, in either direction.
    ///    The baseline is a guess about an unplayed class, and monotonicity is
    ///    a property of levels rather than of guesses about them.
    /// 3. **Higher wins**, whatever it came from. A stale `/who` outranking a
    ///    later level-up is what pinned this character's warrior at 8 and made
    ///    every subsequent gain ambiguous.
    /// 4. **At equal levels, the better source wins**, ranked by
    ///    [`LevelSource`]: stated > `/who` > achievement > level-up >
    ///    baseline. Ties keep what is already held.
    ///
    /// That ranking is load-bearing and is the enum's declaration order, via
    /// the derived `Ord` --- see
    /// `the_written_precedence_matches_the_enums_order`.
    ///
    /// Levels only ever go up, so the larger number wins outright regardless of
    /// where it came from. Ranking sources above that would let a stale reading
    /// outlive the truth: this character's only `/who` is from the first day
    /// and says the warrior is 8, and treating `/who` as unassailable pinned it
    /// there — which in turn made every later level-up ambiguous, since a
    /// level-8 warrior looks like a plausible earner of a level-11 gain. Source
    /// is kept for display, and breaks ties at equal levels.
    fn record(&mut self, class: Class, level: u32, source: LevelSource, at: i64) {
        // A statement describes the character now, so it overrules anything
        // inferred from earlier in the log. Without this, stating one class's
        // level can retroactively sharpen attribution back through history and
        // credit levels to a class that was sitting at 10 the whole time.
        // Evidence from after the statement still counts: the class kept
        // playing.
        if let Some((stated, said_at)) = self.stated.get(&class) {
            if at < *said_at && level > *stated {
                return;
            }
        }
        match self.levels.get(&class) {
            // Anything observed beats the baseline outright, including a
            // lower number. The baseline is what we assume about a class
            // nobody has played; the moment the log speaks about it, the
            // assumption has been answered rather than outbid.
            Some((_, LevelSource::Baseline, _)) if source != LevelSource::Baseline => {
                self.levels.insert(class, (level, source, at));
                self.resolve_pending();
            }
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
    /// Levels arrive one at a time, so the class that just reached `level` was
    /// sitting at `level - 1`. That is far sharper than "anything below" — on
    /// a loadout of two fifties and a fresh class it identifies the earner
    /// outright — and it is what keeps a level-9 gain in an early log from
    /// being credited to whichever class happened to share the loadout.
    ///
    /// Falls back to the looser test when nothing sits at `level - 1`, which
    /// happens when a class was levelling before the log began.
    fn only_candidate(&self, level: u32, loadout: &[Class]) -> Option<Class> {
        let exact: Vec<Class> = loadout
            .iter()
            .copied()
            .filter(|c| self.level_of(*c) == level.checked_sub(1))
            .collect();
        if exact.len() == 1 {
            return Some(exact[0]);
        }
        if !exact.is_empty() {
            return None;
        }
        let loose: Vec<Class> = loadout
            .iter()
            .copied()
            .filter(|c| match self.source_of(*c) {
                // Never played, as far as anything here knows, so its number
                // is the assumption rather than a reading and cannot rule it
                // out. Without this a level-3 character's gains matched
                // nothing — not `level - 1`, since every class sat at 10, and
                // not `l < level` either — so every one of them went to
                // `pending` and no class ever acquired a level at all.
                None | Some(LevelSource::Baseline) => true,
                Some(_) => self
                    .level_of(*c)
                    .is_some_and(|l| l < self.cap && l < level),
            })
            .collect();
        (loose.len() == 1).then(|| loose[0])
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
    }

    pub fn level_of(&self, class: Class) -> Option<u32> {
        self.levels.get(&class).map(|(l, _, _)| *l)
    }

    /// Where this class's level came from, so a caller can tell an assumption
    /// from a reading. `None` for a class not in the table at all.
    pub fn source_of(&self, class: Class) -> Option<LevelSource> {
        self.levels.get(&class).map(|(_, s, _)| *s)
    }

    pub fn is_capped(&self, class: Class) -> bool {
        self.level_of(class).is_some_and(|l| l >= self.cap)
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
            .filter(|(c, (_, source, _))| {
                *source != LevelSource::Baseline
                    && (played.is_empty() || played.contains(c) || loadout.contains(c))
                    || loadout.contains(c)
            })
            .map(|(c, (level, source, at))| ClassLevel {
                class: *c,
                abbrev: c.abbrev().to_string(),
                name: c.name().to_string(),
                level: *level,
                source: *source,
                source_label: source.label().to_string(),
                at: *at,
                capped: *level >= self.cap,
            })
            .collect();
        classes.sort_by(|a, b| b.level.cmp(&a.level).then(a.abbrev.cmp(&b.abbrev)));

        // "Unknown" now means never played rather than never seen, since every
        // class carries the baseline.
        let unknown: Vec<String> = loadout
            .iter()
            .filter(|c| {
                self.levels
                    .get(c)
                    .is_none_or(|(_, s, _)| *s == LevelSource::Baseline)
            })
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

    /// The bug this module's baseline caused, from the player's own report:
    /// "im level 3 and it says im level 10".
    ///
    /// Every reading the log offered was below the baseline, and highest-wins
    /// discarded all of them -- the two level-ups and the `/who` row that
    /// confirmed them. The number shown had no evidence behind it and no
    /// evidence could dislodge it.
    #[test]
    fn a_new_character_is_not_floored_at_the_baseline() {
        let mut t = ClassLevelTracker::new();
        let loadout = [Class::Warrior];
        t.note_loadout(0, &loadout);
        assert_eq!(
            t.on_level_up(100, 2, &loadout),
            Some(Class::Warrior),
            "the only class in the loadout has to take the level"
        );
        assert_eq!(t.level_of(Class::Warrior), Some(2));
        t.on_level_up(200, 3, &loadout);
        assert_eq!(t.report(&loadout, &loadout).effective, Some(3));
        // And it keeps going up from there in the ordinary way.
        t.on_level_up(300, 4, &loadout);
        assert_eq!(t.report(&loadout, &loadout).effective, Some(4));
    }

    /// A `/who` below the baseline is the game's own answer, and was thrown
    /// away for being a smaller number than a guess.
    #[test]
    fn who_below_the_baseline_is_believed() {
        let mut t = ClassLevelTracker::new();
        let loadout = [Class::Warrior, Class::Bard];
        t.on_who(100, 3, &loadout);
        assert_eq!(t.level_of(Class::Warrior), Some(3));
        assert_eq!(t.level_of(Class::Bard), Some(3));
        assert!(
            t.report(&loadout, &loadout).unknown.is_empty(),
            "a class the game just reported the level of is not unknown"
        );
    }

    /// Only the *assumption* gives way. Two readings still go highest-wins,
    /// so a stale one cannot pull a class back down.
    #[test]
    fn one_reading_does_not_lower_another() {
        let mut t = ClassLevelTracker::new();
        let loadout = [Class::Warrior, Class::Bard, Class::Shaman];
        t.note_loadout(0, &loadout);
        t.on_achievement(100, "Warrior's Combat Proficiency, Level 50");
        // An old /who from before the warrior capped. Lower, and ignored.
        t.on_who(200, 28, &loadout);
        assert_eq!(t.level_of(Class::Warrior), Some(50));
        assert_eq!(t.level_of(Class::Shaman), Some(28), "and the floor still lands");
    }

    /// The baseline still does the job it was added for: on a mature
    /// character it says which class earned a gain to 11.
    #[test]
    fn the_baseline_still_sharpens_attribution() {
        let mut t = ClassLevelTracker::new();
        let loadout = [Class::Warrior, Class::Bard, Class::Shaman];
        t.note_loadout(0, &loadout);
        t.on_achievement(100, "Warrior's Combat Proficiency, Level 50");
        t.on_achievement(110, "Bard's Casting Proficiency, Level 50");
        assert_eq!(
            t.on_level_up(200, 11, &loadout),
            Some(Class::Shaman),
            "the class sitting at the baseline is the one that reached 11"
        );
    }

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
        t.on_who(100, 14, &load);
        assert_eq!(t.level_of(Class::Warrior), Some(14));
        assert_eq!(t.level_of(Class::Enchanter), Some(14));
        // And a later row does not drag a class back down.
        t.on_achievement(200, "Warrior's Combat Proficiency, Level 50");
        t.on_who(300, 16, &load);
        assert_eq!(t.level_of(Class::Warrior), Some(50));
        assert_eq!(t.level_of(Class::Enchanter), Some(16));
    }

    /// What the player reads off their own character sheet is final: the log
    /// cannot state the level of a class that capped before logging began, and
    /// /who only ever reports the lowest of three.
    #[test]
    fn a_stated_level_beats_every_inference() {
        let mut t = ClassLevelTracker::new();
        let load = [Class::Warrior, Class::Bard, Class::Shaman];
        t.set_stated(Class::Warrior, 50, 100);
        t.set_stated(Class::Bard, 50, 100);
        t.on_who(200, 28, &load);
        assert_eq!(
            t.level_of(Class::Warrior),
            Some(50),
            "floor cannot lower it"
        );
        assert_eq!(t.level_of(Class::Shaman), Some(28));
        // And the sole uncapped class takes the next level.
        assert_eq!(t.on_level_up(300, 29, &load), Some(Class::Shaman));
    }

    /// Stating a level describes the character now. Log lines from before the
    /// statement must not push it back up — that is how an enchanter parked at
    /// 10 acquired a level of 12 from an ambiguous early gain.
    #[test]
    fn a_statement_overrules_older_evidence_but_not_newer() {
        let mut t = ClassLevelTracker::new();
        let load = [Class::Warrior, Class::Enchanter];
        t.set_stated(Class::Enchanter, 10, 1000);
        // Replaying history cannot raise it.
        t.on_who(500, 12, &load);
        assert_eq!(t.level_of(Class::Enchanter), Some(10));
        // Playing on afterwards can.
        t.on_who(2000, 14, &load);
        assert_eq!(t.level_of(Class::Enchanter), Some(14));
    }

    /// A gain to N belongs to whoever was at N-1, which pins it even when
    /// several classes are below the cap.
    #[test]
    fn a_level_up_goes_to_the_class_one_below_it() {
        let mut t = ClassLevelTracker::new();
        let load = [Class::Warrior, Class::Bard, Class::Berserker];
        t.set_stated(Class::Warrior, 50, 0);
        t.set_stated(Class::Bard, 22, 0);
        // The berserker is on the baseline 10, so 11 is unambiguously its.
        assert_eq!(t.on_level_up(100, 11, &load), Some(Class::Berserker));
        // And 23 is unambiguously the bard's, though both are below the cap.
        assert_eq!(t.on_level_up(200, 23, &load), Some(Class::Bard));
        assert_eq!(t.level_of(Class::Berserker), Some(11));
        assert_eq!(t.level_of(Class::Bard), Some(23));
    }

    /// Two classes could have earned it, so neither is credited. A wrong
    /// attribution is worse than none: it sticks.
    #[test]
    fn an_ambiguous_level_up_is_not_attributed() {
        let mut t = ClassLevelTracker::new();
        let load = [Class::Warrior, Class::Rogue, Class::Monk];
        t.note_loadout(0, &load);
        // Nothing sits at 19, and all three are plausible under the loose
        // test, so the gain is held rather than credited to one of them.
        assert_eq!(t.on_level_up(100, 20, &load), None);
        for c in load {
            assert_eq!(t.level_of(c), Some(STARTING_LEVEL), "still unplayed");
        }
        let r = t.report(&load, &[]);
        assert!(r.needs_who);
        assert_eq!(r.unknown, vec!["WAR", "ROG", "MNK"]);
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
        t.on_who(100, 12, &[Class::Warrior, Class::Enchanter]);
        assert_eq!(t.level_of(Class::Warrior), Some(12));
        t.on_who(200, 28, &[Class::Warrior, Class::Bard, Class::Shaman]);
        assert_eq!(t.level_of(Class::Warrior), Some(28), "floor raised");
        assert_eq!(t.level_of(Class::Enchanter), Some(12), "not in the loadout");
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
    /// The ranking in `record`'s doc is the enum's declaration order.
    ///
    /// `record` breaks a tie with `*have >= source`, which is the derived
    /// `Ord`, which is the order the variants are written in. So moving a
    /// variant changes which source wins at an equal level, silently, with
    /// nothing in the type system objecting --- and the tie-break is the
    /// difference between believing a stale `/who` and believing a level-up.
    /// Stated here so the prose and the enum cannot drift apart.
    #[test]
    fn the_written_precedence_matches_the_enums_order() {
        use LevelSource::*;
        let ranked = [Baseline, LevelUp, Achievement, Who, Stated];
        for pair in ranked.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{:?} must rank below {:?}, or `record`'s tie-break means \
                 something other than what its doc says",
                pair[0],
                pair[1]
            );
        }
        // And the two ends, named rather than implied: a statement beats
        // everything, and the baseline loses to everything.
        assert_eq!(ranked.iter().copied().max(), Some(Stated));
        assert_eq!(ranked.iter().copied().min(), Some(Baseline));
    }

}

