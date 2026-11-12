//! Subscribing to a parsed log.
//!
//! Two programs read the same logs for different reasons. `norrlogd` wants
//! a live picture — encounters, alerts, what is happening now. eqldb wants a
//! record — what has been seen, ever, so a site can be built from it. Until
//! now that was one dispatch inside the daemon and a handful of Python
//! scripts re-parsing the same text somewhere else, which meant a rule
//! existed twice and only one copy got fixed.
//!
//! A [`Feed`] parses a log once and hands every entry to each [`Observer`] in
//! turn. What an observer does with it is its own business; what it must not
//! do is parse the line again.
//!
//! # Why observers are borrowed rather than owned
//!
//! The obvious shape is a feed holding `Vec<Box<dyn Observer>>`, and it is
//! wrong in the way that matters: the caller then has to get its collectors
//! back out to read their results, which means downcasting or a second
//! accessor per collector. Passing `&mut [&mut dyn Observer]` per run keeps
//! every collector's concrete type in the caller's hands, so reading the
//! answer afterwards is a field access.
//!
//! # What the context is for
//!
//! A line does not say where it happened. "You have looted a Bone Chip" is
//! only worth recording alongside the zone, and the zone was announced
//! hundreds of lines earlier. Rather than have every observer keep its own
//! copy of that — five collectors each tracking zone changes, four of them
//! subtly differently — the feed keeps it once and passes it along.
//!
//! The zone is updated *before* the entry is dispatched, so an observer
//! seeing `ZoneChange { zone: "Befallen" }` also sees `cx.zone == "Befallen"`.
//! The client prints that line on arrival, so by the time it is read the
//! character is already there.

use std::io::BufRead;

use crate::encounter::{Encounter, EncounterTracker};
use crate::event::{Actor, Event, LogEntry};
use crate::game::Game;
use crate::parser::Parser;

/// What the feed knows that a single line does not.
#[derive(Debug, Clone)]
pub struct Context<'a> {
    /// Whose log this is. The parser needs it to tell "you" from everybody
    /// else, and so does anything recording who did something.
    pub player: &'a str,
    /// Which game. Not optional and not inferable from a line: one machine
    /// plays both, the same creature names occur in each, and a fact filed
    /// under the wrong one is worse than one not filed at all.
    pub game: Game,
    /// The zone last announced, if any has been.
    pub zone: Option<&'a str>,
    /// Seconds since the previous entry. `None` on the first.
    ///
    /// Enough to notice a session boundary without the feed deciding what one
    /// is — that threshold differs between a live view and a rebuild, so it
    /// belongs to the observer.
    pub gap: Option<i64>,
    /// The fight open right now, as it stood *before* this entry.
    ///
    /// Four collectors need the same question answered — drops, the
    /// bestiary, the Sky chain and the task list all ask whether a death
    /// counts as yours — and the answer is not in the death line. The game
    /// credits the final hit to whoever landed it, which for a charming class
    /// is routinely your own charmed pet:
    ///
    /// ```text
    /// Innoruuk, the Prince of Hate has been slain by Innoruuk`s Chosen!
    /// ```
    ///
    /// So credit follows participation, and participation is encounter state.
    /// Keeping it here rather than in each collector is the difference
    /// between one encounter tracker and four — and four would not merely be
    /// wasteful, they would be four chances to disagree about when a fight
    /// began.
    ///
    /// Use [`Context::fought`] rather than reading this directly.
    pub encounter: Option<&'a Encounter>,
}

impl Context<'_> {
    /// Whether you took part in killing this, by the game's own standard.
    ///
    /// True when you damaged it during the fight that is open. Deliberately
    /// not "did you land the killing blow": requiring that silently discards
    /// every kill a charmed pet finished, which on a bard is most of them.
    pub fn fought(&self, victim: &Actor) -> bool {
        self.encounter
            .and_then(|e| e.actors.get(&victim.key()))
            .is_some_and(|s| s.damaged_by_you)
    }
}

/// Something that wants to see a log go past.
///
/// Observers are called in the order they are given and must not depend on
/// each other: two collectors reading the same event are two readings of it,
/// not a pipeline. Anything that genuinely needs another's conclusion should
/// run as a second pass over the first's output.
pub trait Observer {
    /// A short name, so a report can say which collector saw what.
    fn name(&self) -> &'static str;

    /// One parsed line, in context.
    fn on_entry(&mut self, entry: &LogEntry, cx: &Context<'_>);

    /// A fight finished, with everything it accumulated.
    ///
    /// `EncounterTracker::push` has always returned the encounter it just
    /// closed and the feed threw it away, so an observer could see the fight
    /// in progress through [`Context::encounter`] and never the finished
    /// article. Anything wanting a whole fight -- its timeline, its actors,
    /// what died -- had to keep a second tracker and re-derive it, which is
    /// the duplication this feed exists to prevent.
    ///
    /// Called for every fight, including the last one, which closes when the
    /// log ends rather than because anything in it said so.
    fn on_encounter_end(&mut self, _enc: &crate::encounter::Encounter, _cx: &Context<'_>) {}

    /// The log ended.
    ///
    /// Anything being held pending has to be settled or dropped here. A
    /// collector that waits for a line to explain an earlier one — and
    /// several do — otherwise silently loses whatever was still waiting when
    /// the file ran out.
    fn finish(&mut self, _cx: &Context<'_>) {}
}

/// Parses a log once and feeds it to a set of observers.
pub struct Feed {
    parser: Parser,
    player: String,
    game: Game,
    zone: Option<String>,
    last: Option<i64>,
    encounters: EncounterTracker,
}

impl Feed {
    pub fn new(player: impl Into<String>, game: Game) -> Self {
        let player = player.into();
        Self {
            parser: Parser::new(player.clone()),
            encounters: EncounterTracker::new(&player),
            player,
            game,
            zone: None,
            last: None,
        }
    }

    /// The zone the feed currently believes it is in.
    pub fn zone(&self) -> Option<&str> {
        self.zone.as_deref()
    }

    /// Start in a known zone, for a run resuming where an earlier one stopped.
    ///
    /// Without this a resumed read has no zone until the character next moves,
    /// and everything looted in between is filed nowhere.
    pub fn resume_in(&mut self, zone: Option<String>) {
        self.zone = zone;
    }

    /// Read a whole log, handing each entry to every observer.
    ///
    /// Returns the number of lines read. Lines the parser cannot make an entry
    /// of are counted and not dispatched; a line that becomes
    /// [`Event::Unknown`] *is* dispatched, because "the grammar does not model
    /// this" is a fact an observer may want.
    pub fn read(
        &mut self,
        reader: impl BufRead,
        to: &mut [&mut dyn Observer],
    ) -> std::io::Result<u64> {
        let mut lines = 0u64;
        for line in reader.lines() {
            let line = line?;
            lines += 1;
            let Some(entry) = self.parser.parse_line(&line) else { continue };
            self.dispatch(&entry, to);
        }
        // The last fight ends because the file did, not because anything in
        // it said so. Without this every log silently loses its final
        // encounter -- and the final encounter of a raid log is usually the
        // one somebody wanted.
        if let Some(done) = self.encounters.flush() {
            let cx = self.context();
            for o in to.iter_mut() {
                o.on_encounter_end(&done, &cx);
            }
        }
        let cx = self.context();
        for o in to.iter_mut() {
            o.finish(&cx);
        }
        Ok(lines)
    }

    /// One entry, for a caller driving the lines itself — following a file
    /// live, or replaying a span it has already read.
    pub fn push(&mut self, line: &str, to: &mut [&mut dyn Observer]) {
        if let Some(entry) = self.parser.parse_line(line) {
            self.dispatch(&entry, to);
        }
    }

    fn context(&self) -> Context<'_> {
        Context {
            player: &self.player,
            game: self.game,
            zone: self.zone.as_deref(),
            gap: None,
            encounter: self.encounters.current(),
        }
    }

    fn dispatch(&mut self, entry: &LogEntry, to: &mut [&mut dyn Observer]) {
        let gap = self.last.map(|t| entry.timestamp - t);
        self.last = Some(entry.timestamp);
        // Before dispatch: see the module docs. The line announces an arrival
        // that has already happened.
        if let Event::ZoneChange { zone } = &entry.event {
            self.zone = Some(zone.clone());
        }
        {
            let cx = Context {
                player: &self.player,
                game: self.game,
                zone: self.zone.as_deref(),
                gap,
                // Before the push: a death has to be judged against the fight
                // as it stood when the line was written. Push first and the
                // fight closes and the answer becomes no.
                encounter: self.encounters.current(),
            };
            for o in to.iter_mut() {
                o.on_entry(entry, &cx);
            }
        }
        if let Some(done) = self.encounters.push(entry) {
            // A fresh context: the one above borrowed the encounter that has
            // just closed, and what an observer wants here is the state the
            // *next* line will be read against.
            let cx = Context {
                player: &self.player,
                game: self.game,
                zone: self.zone.as_deref(),
                gap,
                encounter: self.encounters.current(),
            };
            for o in to.iter_mut() {
                o.on_encounter_end(&done, &cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what it was given, which is all a test needs.
    #[derive(Default)]
    struct Spy {
        zones: Vec<Option<String>>,
        kinds: Vec<String>,
        finished: bool,
    }

    impl Observer for Spy {
        fn name(&self) -> &'static str {
            "spy"
        }
        fn on_entry(&mut self, entry: &LogEntry, cx: &Context<'_>) {
            self.zones.push(cx.zone.map(str::to_string));
            self.kinds.push(format!("{:?}", entry.event));
        }
        fn finish(&mut self, _cx: &Context<'_>) {
            self.finished = true;
        }
    }

    const LOG: &str = "\
[Tue Jul 28 15:03:10 2026] You have entered Befallen.
[Tue Jul 28 15:03:11 2026] Your Location is 1.00, 2.00, 3.00
[Tue Jul 28 15:03:12 2026] You have entered The Oasis of Marr.
[Tue Jul 28 15:03:13 2026] Your Location is 4.00, 5.00, 6.00
";

    fn run(text: &str) -> Spy {
        let mut spy = Spy::default();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut spy];
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs).unwrap();
        }
        spy
    }

    /// The zone reaches an observer without the observer tracking it.
    ///
    /// The whole reason a context exists: otherwise every collector keeps its
    /// own copy and they disagree at the boundaries.
    #[test]
    fn the_zone_travels_with_the_entry() {
        let spy = run(LOG);
        assert_eq!(
            spy.zones,
            vec![
                Some("Befallen".into()),
                Some("Befallen".into()),
                Some("The Oasis of Marr".into()),
                Some("The Oasis of Marr".into()),
            ]
        );
    }

    /// A zone-change line is itself in the zone it announces.
    #[test]
    fn arriving_counts_as_being_there() {
        let spy = run("[Tue Jul 28 15:03:10 2026] You have entered Befallen.\n");
        assert_eq!(spy.zones, vec![Some("Befallen".into())]);
    }

    /// Before anything says otherwise, the zone is honestly unknown.
    #[test]
    fn a_log_starting_mid_session_has_no_zone_until_told() {
        let spy = run("[Tue Jul 28 15:03:11 2026] Your Location is 1.00, 2.00, 3.00\n");
        assert_eq!(spy.zones, vec![None]);
    }

    /// A resumed read starts where the last one left off.
    #[test]
    fn resuming_carries_the_zone_in() {
        let mut spy = Spy::default();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        feed.resume_in(Some("Befallen".into()));
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut spy];
            feed.push("[Tue Jul 28 15:03:11 2026] Your Location is 1.00, 2.00, 3.00", &mut subs);
        }
        assert_eq!(spy.zones, vec![Some("Befallen".into())]);
    }

    /// Every observer is told the log ended, so nothing held pending is lost.
    #[test]
    fn the_end_of_the_log_is_announced() {
        assert!(run(LOG).finished);
    }
}

#[cfg(test)]
mod encounter_end_tests {
    use super::*;
    use crate::game::Game;

    #[derive(Default)]
    struct Fights {
        ended: Vec<(u64, usize, i64)>,
    }

    impl Observer for Fights {
        fn name(&self) -> &'static str {
            "fights"
        }
        fn on_entry(&mut self, _e: &LogEntry, _cx: &Context<'_>) {}
        fn on_encounter_end(&mut self, enc: &crate::encounter::Encounter, _cx: &Context<'_>) {
            self.ended.push((enc.id, enc.timeline.len(), enc.end - enc.start));
        }
    }

    /// A finished fight reaches an observer, with its timeline on it.
    ///
    /// The tracker has always returned the encounter it closed and the feed
    /// discarded it, so the only fight an observer could see was the one in
    /// progress. Anything wanting the whole article had to run a second
    /// tracker — the duplication this feed was built to remove.
    #[test]
    fn a_finished_fight_reaches_the_observers() {
        let mut f = Fights::default();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        let log = "\
[Mon Aug 03 16:47:52 2026] You hit a dracoliche for 100 points of damage.
[Mon Aug 03 16:47:53 2026] You hit a dracoliche for 120 points of damage.
[Mon Aug 03 16:47:54 2026] You have slain a dracoliche!
";
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut f];
            feed.read(std::io::Cursor::new(log), &mut subs).unwrap();
        }
        assert_eq!(f.ended.len(), 1, "one fight, closed by the log ending");
        assert!(f.ended[0].1 > 0, "and it carries its timeline: {:?}", f.ended[0]);
    }

    /// The last fight is not lost when the file simply stops.
    ///
    /// Nothing in a log says "the raid is over". Without a flush at the end
    /// every log drops its final encounter, which in a raid log is usually
    /// the one somebody meant to keep.
    #[test]
    fn the_last_fight_is_closed_by_the_file_ending() {
        let mut f = Fights::default();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        // No death, no idle gap: nothing here closes the fight but the EOF.
        let log = "[Mon Aug 03 16:47:52 2026] You hit a dracoliche for 100 points of damage.\n";
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut f];
            feed.read(std::io::Cursor::new(log), &mut subs).unwrap();
        }
        assert_eq!(f.ended.len(), 1, "the open fight is closed and handed over");
    }
}
