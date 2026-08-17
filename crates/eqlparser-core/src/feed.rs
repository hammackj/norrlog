//! Subscribing to a parsed log.
//!
//! Two programs read the same logs for different reasons. `eqlparserd` wants
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

use crate::event::{Event, LogEntry};
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
}

impl Feed {
    pub fn new(player: impl Into<String>, game: Game) -> Self {
        let player = player.into();
        Self {
            parser: Parser::new(player.clone()),
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
        let cx = Context {
            player: &self.player,
            game: self.game,
            zone: self.zone.as_deref(),
            gap,
        };
        for o in to.iter_mut() {
            o.on_entry(entry, &cx);
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
