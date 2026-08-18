//! Collectors: observers that accumulate a record rather than a live picture.
//!
//! These are the things a rebuild wants and a running client does not — what
//! has been seen ever, folded across a whole log, rather than what is
//! happening now. They subscribe through [`crate::feed`] like anything else,
//! so both programs can have them and neither has to parse a line twice.

use std::collections::BTreeMap;

use crate::event::{Actor, Event, LogEntry};
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

/// What a faction adjustment was for: a creature killed, or something handed in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum FactionCause {
    /// Killed in a place. The zone is part of the key, not decoration: an orc
    /// centurion in West Commonlands moves Freeport's factions and one in
    /// Crushbone moves Faydwer's, and pooling them invents a creature that
    /// moves nine factions when neither does.
    ///
    /// Both are lower-cased, because the client varies the case of a name
    /// with where in the sentence it lands. Keying on what was printed files
    /// "The froglok king" and "the froglok king" as two creatures — which it
    /// did, splitting nine kills into eight and one.
    Kill { creature: String, zone: String },
    /// Handed to somebody. Keyed on both, because what a hand-in is worth is a
    /// fact about the item and who takes it.
    TurnIn { npc: String, item: String },
}

/// One faction moved by one cause, and by how much.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactionMoves {
    /// Every distinct adjustment seen, and how often.
    ///
    /// Not an average. A creature whose hit is always the same has one entry;
    /// one that varies keeps both readings, because a figure nobody observed
    /// is worse than two that somebody did.
    pub amounts: BTreeMap<i64, u64>,
}

impl FactionMoves {
    pub fn times(&self) -> u64 {
        self.amounts.values().sum()
    }
}

/// Which creatures and hand-ins move which factions.
///
/// # Why this needs to look forward, and how it does so in one pass
///
/// The client prints faction *before* the death it belongs to:
///
/// ```text
/// Your faction standing with Ashen Order has been adjusted by 5.
/// You gain experience! (0.242%)
/// You have slain orc pawn!
/// ```
///
/// So a run of adjacent faction lines belongs to the *next* death within a
/// few lines. Looking backwards instead finds the wrong kill or none at all —
/// on a real log that rule attributes 171 bursts out of 5,061 where looking
/// forward attributes 3,045.
///
/// A streaming collector cannot look forward, so it holds the burst instead
/// and counts entries until a death arrives or [`LOOKAHEAD`] have passed. The
/// waiting is why [`Observer::finish`] matters here: a burst still pending
/// when the log ends has to be settled rather than dropped on the floor.
///
/// # And why hand-ins are the other way round
///
/// A turn-in prints the offer first and the faction after:
///
/// ```text
/// You offered 200 Small Piece of High Quality Ore to Crusader Iktra.
/// Crusader Iktra says, 'Thank you, Ogre...'
/// Your faction standing with New Sebilisian Expedition has been adjusted by 10.
/// ```
///
/// So a burst with no death after it goes to the offer still standing. The
/// kill rule is tried first and a death closes the open offer, which is what
/// keeps the two apart: a hand-in made in the middle of a fight would
/// otherwise collect the fight's faction, and did — the Wind Rune turn-ins
/// sit among Crushbone kills.
#[derive(Debug, Clone, Default)]
pub struct FactionCollector {
    moves: BTreeMap<(FactionCause, String), FactionMoves>,
    /// Names as first printed, for keys that are lower-cased.
    display: BTreeMap<String, String>,
    zone_display: BTreeMap<String, String>,
    /// Hand-ins seen, and how many items went across, keyed as the cause is.
    pub offered: BTreeMap<(String, String), u64>,
    pub handins: BTreeMap<(String, String), u64>,
    /// The burst being read right now.
    burst: Vec<(String, i64)>,
    /// A finished burst waiting for the death that explains it.
    pending: Option<Pending>,
    /// The offer a later burst would belong to, and when it was made.
    offer: Option<((String, String), i64)>,
    /// When the last entry was. A burst closed by the end of the log has no
    /// line of its own to take a time from, and closing it at zero puts it an
    /// epoch away from any offer — which silently unattributes it.
    last_at: i64,
    pub bursts: u64,
    pub to_kill: u64,
    pub to_turnin: u64,
    /// Bursts with neither a death after them nor an offer before them.
    pub unattributed: u64,
}

#[derive(Debug, Clone)]
struct Pending {
    lines: Vec<(String, i64)>,
    /// Entries seen since the burst closed.
    waited: usize,
    when: i64,
    zone: String,
}

/// How many entries past a burst to wait for the death that caused it.
///
/// The lines between are experience, coin and the corpse's parting words.
const LOOKAHEAD: usize = 8;

/// How long after an offer its faction can still be arriving, in seconds.
///
/// A four-hundred item hand-in is acknowledged one line at a time and takes a
/// while; an hour later is a different event.
const TURNIN_WINDOW: i64 = 300;

impl FactionCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every (cause, faction) pair seen.
    pub fn moves(&self) -> &BTreeMap<(FactionCause, String), FactionMoves> {
        &self.moves
    }

    fn record(&mut self, cause: FactionCause, lines: &[(String, i64)]) {
        for (faction, adj) in lines {
            *self
                .moves
                .entry((cause.clone(), faction.clone()))
                .or_default()
                .amounts
                .entry(*adj)
                .or_default() += 1;
        }
    }

    /// Close the burst being read, if any, and stand it up to wait.
    fn close_burst(&mut self, at: i64, zone: &str) {
        if self.burst.is_empty() {
            return;
        }
        self.bursts += 1;
        // Two bursts in flight cannot happen: a burst only closes on a
        // non-faction line, and that same line is the first the pending one
        // waits through. Settle the older one rather than lose it.
        if self.pending.is_some() {
            self.settle_pending();
        }
        self.pending = Some(Pending {
            lines: std::mem::take(&mut self.burst),
            waited: 0,
            when: at,
            zone: zone.to_string(),
        });
    }

    /// The wait ran out, or the log ended: no death is coming.
    fn settle_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        match &self.offer {
            Some((key, at)) if p.when - *at >= 0 && p.when - *at <= TURNIN_WINDOW => {
                let cause = FactionCause::TurnIn {
                    npc: key.0.clone(),
                    item: key.1.clone(),
                };
                self.to_turnin += 1;
                self.record(cause, &p.lines);
            }
            _ => self.unattributed += 1,
        }
    }

    fn attribute_to_kill(&mut self, victim: &str) {
        let Some(p) = self.pending.take() else { return };
        let creature = strip_article(victim).to_lowercase();
        // Kept so a page can print the name as the client writes it. First
        // spelling wins; they differ only in case.
        self.display
            .entry(creature.clone())
            .or_insert_with(|| strip_article(victim).to_string());
        self.to_kill += 1;
        self.record(
            FactionCause::Kill {
                creature,
                zone: p.zone.to_lowercase(),
            },
            &p.lines,
        );
        self.zone_display
            .entry(p.zone.to_lowercase())
            .or_insert(p.zone);
    }

    /// The creature name as first printed, for a key.
    pub fn creature_display(&self, key: &str) -> Option<&str> {
        self.display.get(key).map(String::as_str)
    }

    /// The zone name as first printed, for a key.
    pub fn zone_display(&self, key: &str) -> Option<&str> {
        self.zone_display.get(key).map(String::as_str)
    }
}

impl Observer for FactionCollector {
    fn name(&self) -> &'static str {
        "faction"
    }

    fn on_entry(&mut self, entry: &LogEntry, cx: &Context<'_>) {
        let zone = cx.zone.unwrap_or_default().to_string();
        self.last_at = entry.timestamp;

        // A faction line either opens a burst or extends the one being read.
        if let Event::Faction { faction, adjustment } = &entry.event {
            // A capped standing states no number. It says the faction did not
            // move, which is not an adjustment and would be recorded as one.
            //
            // It does not end the burst, though. The client prints it among
            // the adjustments for the same kill:
            //
            //     ... with Clurg has been adjusted by -7.
            //     ... with Storm Guard could not possibly get any better.
            //     ... with Shadowknights of Night Keep has been adjusted by 3.
            //
            // tools/import-faction.py breaks there, because its pattern wants
            // a number, and reads that kill as two bursts. Both halves still
            // reach the same death, so the recorded facts are identical —
            // it is the burst count that differs, and one event is the
            // truthful reading of one event.
            if let Some(adj) = adjustment {
                self.burst.push((faction.clone(), *adj));
            }
            return;
        }

        // Any other line ends the burst and starts the wait.
        self.close_burst(entry.timestamp, &zone);

        match &entry.event {
            Event::Death { victim, .. } => {
                // Your own death is not a creature. Attributing a burst to it
                // files faction under a creature called "You".
                let named = match victim {
                    Actor::Named(n) => Some(n.clone()),
                    Actor::You => None,
                };
                match named {
                    Some(n) => self.attribute_to_kill(&n),
                    None => self.settle_pending(),
                }
                // Faction printed after a kill is the kill's. Without this an
                // offer stays open across an evening and collects whatever it
                // happens to precede.
                self.offer = None;
                return;
            }
            Event::TradeOffer { item, quantity, npc } => {
                let key = (npc.trim().to_lowercase(), item.trim().to_lowercase());
                *self.offered.entry(key.clone()).or_default() += quantity.max(&1);
                *self.handins.entry(key.clone()).or_default() += 1;
                self.offer = Some((key, entry.timestamp));
            }
            _ => {}
        }

        if let Some(p) = &mut self.pending {
            p.waited += 1;
            if p.waited >= LOOKAHEAD {
                self.settle_pending();
            }
        }
    }

    fn finish(&mut self, cx: &Context<'_>) {
        // Whatever is still being read, and whatever is still waiting. A log
        // ending mid-burst is ordinary — it is what a live log always looks
        // like — and dropping it silently is how a rebuild quietly loses the
        // most recent evening's faction.
        let zone = cx.zone.unwrap_or_default().to_string();
        let at = self.last_at;
        self.close_burst(at, &zone);
        self.settle_pending();
    }
}

#[cfg(test)]
mod faction_tests {
    use super::*;
    use crate::feed::Feed;
    use crate::game::Game;

    fn collect(text: &str) -> FactionCollector {
        let mut c = FactionCollector::new();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut c];
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs).unwrap();
        }
        c
    }

    const KILL: &str = "\
[Thu Jul 30 11:08:54 2026] You have entered West Commonlands.
[Thu Jul 30 11:08:54 2026] Your faction standing with Guardians of the Vale has been adjusted by 5.
[Thu Jul 30 11:08:54 2026] Your faction standing with Ashen Order has been adjusted by 5.
[Thu Jul 30 11:08:54 2026] You gain experience! (0.242%)
[Thu Jul 30 11:08:54 2026] You receive 1 copper from the corpse.
[Thu Jul 30 11:08:54 2026] You have slain an orc pawn!
";

    /// The burst belongs to the death that comes *after* it.
    #[test]
    fn faction_before_a_kill_belongs_to_that_kill() {
        let c = collect(KILL);
        let cause = FactionCause::Kill {
            creature: "orc pawn".into(),
            zone: "west commonlands".into(),
        };
        assert_eq!(c.to_kill, 1);
        assert_eq!(c.unattributed, 0);
        assert_eq!(
            c.moves()[&(cause.clone(), "Ashen Order".to_string())].amounts[&5],
            1
        );
        assert!(c.moves().contains_key(&(cause, "Guardians of the Vale".to_string())));
    }

    /// The zone is part of the creature's key, not decoration.
    #[test]
    fn the_same_creature_in_two_zones_is_two_rows() {
        let mut text = String::from(KILL);
        text.push_str(
            "[Thu Jul 30 12:00:00 2026] You have entered Crushbone.\n\
             [Thu Jul 30 12:00:01 2026] Your faction standing with Emerald Warriors has been adjusted by 3.\n\
             [Thu Jul 30 12:00:01 2026] You have slain an orc pawn!\n",
        );
        let c = collect(&text);
        let zones: Vec<&str> = c
            .moves()
            .keys()
            .filter_map(|(cause, _)| match cause {
                FactionCause::Kill { creature, zone } if creature == "orc pawn" => {
                    Some(zone.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(zones.contains(&"west commonlands"), "{zones:?}");
        assert!(zones.contains(&"crushbone"), "{zones:?}");
        // And the printed spelling survives alongside the key.
        assert_eq!(c.zone_display("west commonlands"), Some("West Commonlands"));
        assert_eq!(c.creature_display("orc pawn"), Some("orc pawn"));
    }

    /// A hand-in prints the offer first, so its burst looks backwards.
    #[test]
    fn faction_after_an_offer_belongs_to_the_hand_in() {
        let c = collect(
            "[Thu Jul 30 11:00:00 2026] You offered 200 Small Piece of High Quality Ore to Crusader Iktra.\n\
             [Thu Jul 30 11:00:01 2026] Crusader Iktra says, 'Thank you, Ogre.'\n\
             [Thu Jul 30 11:00:02 2026] Your faction standing with New Sebilisian Expedition has been adjusted by 10.\n\
             [Thu Jul 30 11:00:03 2026] You gain experience! (0.100%)\n",
        );
        let cause = FactionCause::TurnIn {
            npc: "crusader iktra".into(),
            item: "small piece of high quality ore".into(),
        };
        assert_eq!(c.to_turnin, 1);
        assert_eq!(c.moves()[&(cause, "New Sebilisian Expedition".into())].amounts[&10], 1);
        assert_eq!(c.offered[&("crusader iktra".into(), "small piece of high quality ore".into())], 200);
    }

    /// A kill in the middle of a hand-in does not feed the hand-in.
    ///
    /// The bug the ordering exists for: the Wind Rune turn-ins sit among
    /// Crushbone kills, and a naive "everything after the offer" rule
    /// credited an orc's four factions to the rune.
    #[test]
    fn a_kill_during_a_hand_in_does_not_go_to_the_item() {
        let c = collect(
            "[Thu Jul 30 11:00:00 2026] You have entered Crushbone.\n\
             [Thu Jul 30 11:00:01 2026] You offered 1 Wind Rune Fana to Cilin Spellsinger.\n\
             [Thu Jul 30 11:00:02 2026] Your faction standing with Emerald Warriors has been adjusted by 4.\n\
             [Thu Jul 30 11:00:03 2026] You have slain an orc pawn!\n",
        );
        assert_eq!(c.to_kill, 1, "the burst is the kill's");
        assert_eq!(c.to_turnin, 0, "and not the rune's");
    }

    /// Your own death is not a creature.
    #[test]
    fn dying_does_not_create_a_creature_called_you() {
        let c = collect(
            "[Thu Jul 30 11:00:00 2026] Your faction standing with Ashen Order has been adjusted by -5.\n\
             [Thu Jul 30 11:00:01 2026] You have been slain by a gnoll pup!\n",
        );
        let bad: Vec<_> = c
            .moves()
            .keys()
            .filter(|(cause, _)| matches!(cause, FactionCause::Kill { creature, .. } if creature == "You"))
            .collect();
        assert!(bad.is_empty(), "{bad:?}");
    }

    /// A burst still waiting when the log ends is settled, not dropped.
    ///
    /// A live log always ends mid-something. Dropping it silently is how a
    /// rebuild loses the most recent evening.
    #[test]
    fn a_burst_at_the_end_of_the_log_is_not_lost() {
        let c = collect(
            "[Thu Jul 30 11:00:00 2026] You offered 1 Metal Bits to Crusader Iktra.\n\
             [Thu Jul 30 11:00:02 2026] Your faction standing with New Sebilisian Expedition has been adjusted by 5.\n",
        );
        assert_eq!(c.bursts, 1);
        assert_eq!(c.to_turnin, 1, "the offer still standing takes it");
    }

    /// A capped standing states no number, so there is nothing to record.
    #[test]
    fn a_capped_faction_is_not_an_adjustment_of_zero() {
        let c = collect(
            "[Thu Jul 30 11:00:00 2026] Your faction standing with Ashen Order could not possibly get any better.\n\
             [Thu Jul 30 11:00:01 2026] You have slain an orc pawn!\n",
        );
        assert!(c.moves().is_empty(), "{:?}", c.moves());
        assert_eq!(c.bursts, 0);
    }
}

#[cfg(test)]
mod faction_capped_tests {
    use super::*;
    use crate::feed::Feed;
    use crate::game::Game;

    /// A capped line among the adjustments is part of the same burst.
    ///
    /// One kill, printed as three faction lines of which one states no
    /// number. Reading it as two bursts is what the Python does and is an
    /// artifact of its pattern wanting a digit; both halves reach the same
    /// death either way, so only the count of events differs.
    #[test]
    fn a_capped_line_does_not_split_a_kills_faction_in_two() {
        let mut c = FactionCollector::new();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        let text = "\
[Tue Aug 11 23:02:03 2026] You have entered Neriak Third Gate.
[Tue Aug 11 23:02:03 2026] Your faction standing with Clurg has been adjusted by -7.
[Tue Aug 11 23:02:03 2026] Your faction standing with Storm Guard could not possibly get any better.
[Tue Aug 11 23:02:03 2026] Your faction standing with Shadowknights of Night Keep has been adjusted by 3.
[Tue Aug 11 23:02:04 2026] You have slain a dark elf spy!
";
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut c];
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs).unwrap();
        }
        assert_eq!(c.bursts, 1, "one kill is one burst");
        assert_eq!(c.to_kill, 1);
        let cause = FactionCause::Kill {
            creature: "dark elf spy".into(),
            zone: "neriak third gate".into(),
        };
        assert_eq!(c.moves()[&(cause.clone(), "Clurg".into())].amounts[&-7], 1);
        assert_eq!(
            c.moves()[&(cause.clone(), "Shadowknights of Night Keep".into())].amounts[&3],
            1
        );
        assert!(
            !c.moves().contains_key(&(cause, "Storm Guard".into())),
            "a capped standing is not an adjustment"
        );
    }
}
