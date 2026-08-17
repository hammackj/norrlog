//! Collectors: observers that accumulate a record rather than a live picture.
//!
//! These are the things a rebuild wants and a running client does not — what
//! has been seen ever, folded across a whole log, rather than what is
//! happening now. They subscribe through [`crate::feed`] like anything else,
//! so both programs can have them and neither has to parse a line twice.

use std::collections::BTreeMap;

use crate::event::{Event, LogEntry};
use crate::feed::{Context, Observer};

/// What consider lines say about one creature, folded across a log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Considered {
    /// As first printed, article and all.
    pub name: String,
    /// Every level it has been seen at, and how often.
    ///
    /// A map rather than a number because a name can cover more than one
    /// creature, and because a level that changed between patches is worth
    /// seeing as two readings rather than as one overwriting the other.
    pub levels: BTreeMap<u32, u64>,
    /// The client has marked it rare at least once.
    ///
    /// Unmarked means nobody has seen a rare version, which is not the same
    /// as there being none.
    pub rare: bool,
    /// A `This creature would take an army to defeat!` was attributed to it.
    pub raid: bool,
}

impl Considered {
    pub fn considers(&self) -> u64 {
        self.levels.values().sum()
    }
}

/// Levels and rare markers, out of consider lines.
///
/// The attitude on the same line is not carried. It is the observer's faction
/// standing with the creature rather than a fact about the creature, and it
/// changes with who is looking.
///
/// # The raid line, and why it is attributed so narrowly
///
/// `This creature would take an army to defeat!` names nothing. It is printed
/// immediately after the consider it belongs to, so it can only be attributed
/// to the consider before it — and only when the two share a timestamp, since
/// the client stamps both in the same second. A raid line arriving with
/// anything in between, or a second later, is left unattributed and counted,
/// because guessing here means marking the wrong creature a raid target.
#[derive(Debug, Clone, Default)]
pub struct ConsiderCollector {
    /// Keyed on the lower-cased name, article stripped, which is the filing
    /// convention everything else here uses.
    seen: BTreeMap<String, Considered>,
    /// The consider immediately before, and when — for the raid line.
    last: Option<(String, i64)>,
    /// Raid lines with no consider to attach to.
    pub orphan_raid_lines: u64,
}

/// A creature name without its leading article.
///
/// The client prints "a noxious spider" and "A noxious spider" for the same
/// thing depending on where in the sentence it lands, and files nothing under
/// either. Only the article is stripped: anything cleverer is a guess about
/// which words are part of a name.
pub fn strip_article(name: &str) -> &str {
    let n = name.trim();
    for a in ["a ", "an ", "A ", "An "] {
        if let Some(rest) = n.strip_prefix(a) {
            return rest.trim_start();
        }
    }
    n
}

impl ConsiderCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every creature considered, keyed as filed.
    pub fn seen(&self) -> &BTreeMap<String, Considered> {
        &self.seen
    }
}

impl Observer for ConsiderCollector {
    fn name(&self) -> &'static str {
        "consider"
    }

    fn on_entry(&mut self, entry: &LogEntry, _cx: &Context<'_>) {
        match &entry.event {
            Event::Consider { name, level, rare, .. } => {
                let display = strip_article(name);
                if display.is_empty() {
                    return;
                }
                let key = display.to_lowercase();
                let row = self.seen.entry(key.clone()).or_insert_with(|| Considered {
                    name: display.to_string(),
                    ..Default::default()
                });
                *row.levels.entry(*level).or_default() += 1;
                row.rare |= *rare;
                self.last = Some((key, entry.timestamp));
            }
            Event::ConsiderArmy => match &self.last {
                // Same second, and nothing in between — see the type docs.
                Some((key, at)) if *at == entry.timestamp => {
                    if let Some(row) = self.seen.get_mut(key) {
                        row.raid = true;
                    }
                }
                _ => self.orphan_raid_lines += 1,
            },
            // Any other line breaks the adjacency the raid marker relies on.
            _ => self.last = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::Feed;
    use crate::game::Game;

    fn collect(text: &str) -> ConsiderCollector {
        let mut c = ConsiderCollector::new();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut c];
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs).unwrap();
        }
        c
    }

    /// The article goes and the rest of the name does not.
    #[test]
    fn a_creature_is_filed_without_its_article() {
        assert_eq!(strip_article("A woodland lion"), "woodland lion");
        assert_eq!(strip_article("an ancient guardian"), "ancient guardian");
        assert_eq!(strip_article("Cazic-Thule"), "Cazic-Thule");
        // Not every leading word that looks like an article is one.
        assert_eq!(strip_article("Animist Kratho"), "Animist Kratho");
    }

    /// Two considers of one creature are two readings, not two creatures.
    #[test]
    fn the_same_creature_folds_across_the_log() {
        let c = collect(
            "[Tue Jul 28 15:03:10 2026] A woodland lion scowls at you, ready to attack -- looks kind of dangerous. (Lvl: 12)\n\
             [Tue Jul 28 15:04:10 2026] a woodland lion scowls at you, ready to attack -- looks kind of dangerous. (Lvl: 12)\n",
        );
        assert_eq!(c.seen().len(), 1);
        let row = &c.seen()["woodland lion"];
        assert_eq!(row.considers(), 2);
        assert_eq!(row.levels[&12], 2);
    }

    /// Seen at two levels is two readings kept, not one overwriting the other.
    #[test]
    fn two_levels_are_both_kept() {
        let c = collect(
            "[Tue Jul 28 15:03:10 2026] A woodland lion scowls at you, ready to attack -- looks kind of dangerous. (Lvl: 12)\n\
             [Tue Jul 28 15:04:10 2026] A woodland lion scowls at you, ready to attack -- looks kind of dangerous. (Lvl: 14)\n",
        );
        let row = &c.seen()["woodland lion"];
        assert_eq!(row.levels.len(), 2);
        assert_eq!(row.considers(), 2);
    }

    /// The raid line attaches to the consider it follows in the same second.
    #[test]
    fn the_army_line_marks_the_creature_above_it() {
        let c = collect(
            "[Sat Aug 01 22:19:10 2026] Cazic-Thule glares at you threateningly -- looks like it would wipe the floor with you! (Lvl: 55)\n\
             [Sat Aug 01 22:19:10 2026] This creature would take an army to defeat!\n",
        );
        assert!(c.seen()["cazic-thule"].raid);
        assert_eq!(c.orphan_raid_lines, 0);
    }

    /// A second later is not the same consider, and is not guessed at.
    ///
    /// The whole point of the narrow rule: attributing this would mark
    /// whatever was considered most recently as a raid target.
    #[test]
    fn a_raid_line_a_second_late_is_not_attributed() {
        let c = collect(
            "[Sat Aug 01 22:19:10 2026] A woodland lion scowls at you, ready to attack -- looks kind of dangerous. (Lvl: 12)\n\
             [Sat Aug 01 22:19:11 2026] This creature would take an army to defeat!\n",
        );
        assert!(!c.seen()["woodland lion"].raid);
        assert_eq!(c.orphan_raid_lines, 1);
    }

    /// And a line in between breaks it too, even within the second.
    #[test]
    fn a_line_in_between_breaks_the_attribution() {
        let c = collect(
            "[Sat Aug 01 22:19:10 2026] A woodland lion scowls at you, ready to attack -- looks kind of dangerous. (Lvl: 12)\n\
             [Sat Aug 01 22:19:10 2026] You have entered Befallen.\n\
             [Sat Aug 01 22:19:10 2026] This creature would take an army to defeat!\n",
        );
        assert!(!c.seen()["woodland lion"].raid);
        assert_eq!(c.orphan_raid_lines, 1);
    }

    /// Rare is sticky: seen once is enough, and it is the client's own word.
    #[test]
    fn rare_is_remembered_from_any_sighting() {
        let c = collect(
            "[Tue Jul 28 15:03:10 2026] a noxious spider scowls at you, ready to attack -- looks kind of dangerous. (Lvl: 24)\n\
             [Tue Jul 28 15:04:10 2026] a noxious spider - a rare creature - judges you amiably -- You could probably win this fight. (Lvl: 24)\n",
        );
        assert!(c.seen()["noxious spider"].rare);
    }
}
