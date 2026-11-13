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
            Event::Consider {
                name, level, rare, ..
            } => {
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
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs)
                .unwrap();
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
                    npc: self
                        .display
                        .get(&key.0)
                        .cloned()
                        .unwrap_or_else(|| key.0.clone()),
                    item: self
                        .display
                        .get(&key.1)
                        .cloned()
                        .unwrap_or_else(|| key.1.clone()),
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
        if let Event::Faction {
            faction,
            adjustment,
        } = &entry.event
        {
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
            Event::TradeOffer {
                item,
                quantity,
                npc,
            } => {
                let key = (npc.trim().to_lowercase(), item.trim().to_lowercase());
                // The key is folded and the name is not, the same split the
                // kill above already makes. Without this a turn-in reached
                // the page as `tiam khonsir` handing over `kobold molar`,
                // because the folded key was doing double duty as the name.
                // First spelling wins; they differ only in case.
                self.display
                    .entry(key.0.clone())
                    .or_insert_with(|| npc.trim().to_string());
                self.display
                    .entry(key.1.clone())
                    .or_insert_with(|| item.trim().to_string());
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
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs)
                .unwrap();
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
        assert!(c
            .moves()
            .contains_key(&(cause, "Guardians of the Vale".to_string())));
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
        // The cause carries the client's own spelling. The key it is
        // counted under stays folded, which is what makes two spellings of
        // one hand-in the same hand-in.
        let cause = FactionCause::TurnIn {
            npc: "Crusader Iktra".into(),
            item: "Small Piece of High Quality Ore".into(),
        };
        assert_eq!(c.to_turnin, 1);
        assert_eq!(
            c.moves()[&(cause, "New Sebilisian Expedition".into())].amounts[&10],
            1
        );
        assert_eq!(
            c.offered[&(
                "crusader iktra".into(),
                "small piece of high quality ore".into()
            )],
            200
        );
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
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs)
                .unwrap();
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

/// One player character named by `/who`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Player {
    pub name: String,
    /// 0 when not stated, which is what `[ANONYMOUS]` means.
    pub level: u32,
    /// Empty when not stated.
    pub classes: Vec<String>,
    pub race: String,
}

/// A creature's damage, filed by what kind it was: a melee hit on you is a
/// swing, a spell landing or ticking on anyone is a cast. Shared with the
/// daemon's engine, so the two read a line the same way.
///
/// Spell damage used to be counted as a melee hit, which put a dragon's
/// breath into its hit rate.
#[allow(clippy::too_many_arguments)]
pub fn file_npc_damage(
    bestiary: &mut crate::bestiary::Bestiary,
    at: i64,
    npc: &str,
    on_you: bool,
    amount: u64,
    kind: crate::event::DamageKind,
    ability: Option<&str>,
    element: Option<&str>,
) {
    use crate::bestiary::{CastSeen, Swing};
    use crate::event::DamageKind;
    match (kind, ability) {
        (DamageKind::DirectSpell, Some(spell)) => {
            bestiary.on_npc_cast(at, npc, spell, CastSeen::Landed { amount, element }, on_you)
        }
        (DamageKind::DamageOverTime, Some(spell)) => {
            bestiary.on_npc_cast(at, npc, spell, CastSeen::Tick { amount }, on_you)
        }
        (DamageKind::Melee, _) if on_you => bestiary.on_swing(at, npc, Swing::Hit(amount)),
        _ => {}
    }
}

/// The roster of player characters a log has seen.
///
/// Its job is to keep people out of the bestiary. A melee line does not know
/// what it is looking at — somebody swinging a sword next to you produces
/// exactly the shape a monster does — so a groupmate who dies leaves the same
/// evidence a creature does, and the only reliable way to tell them apart is
/// that somebody ran `/who` and the client named them as a player.
///
/// Anonymous entries count. Somebody hiding their level is exactly as much a
/// person as somebody who is not, and it is personhood the roster is for.
#[derive(Debug, Clone, Default)]
pub struct PlayerCollector {
    seen: BTreeMap<String, Player>,
    pub rows: u64,
    pub anonymous_rows: u64,
}

impl PlayerCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Keyed on the name as printed.
    pub fn seen(&self) -> &BTreeMap<String, Player> {
        &self.seen
    }
}

impl Observer for PlayerCollector {
    fn name(&self) -> &'static str {
        "players"
    }

    fn on_entry(&mut self, entry: &LogEntry, _cx: &Context<'_>) {
        let Event::WhoEntry {
            level,
            classes,
            name,
            race,
            ..
        } = &entry.event
        else {
            return;
        };
        if name.is_empty() {
            return;
        }
        self.rows += 1;
        if *level == 0 && classes.is_empty() {
            // Anonymous: it names a player and says nothing else. It must not
            // overwrite a fuller reading of the same person taken earlier.
            self.anonymous_rows += 1;
            self.seen.entry(name.clone()).or_insert_with(|| Player {
                name: name.clone(),
                ..Default::default()
            });
            return;
        }
        // Last reading wins: a character levels, and the newest row is the
        // truest.
        self.seen.insert(
            name.clone(),
            Player {
                name: name.clone(),
                level: *level,
                classes: classes.split('/').map(str::to_string).collect(),
                race: race.clone(),
            },
        );
    }
}

/// The highest value seen for each skill.
///
/// `You have become better at Baking! (21)` is the client stating a number it
/// otherwise only shows in a window. The highest is kept because a skill only
/// goes up: a lower reading is an older one, not a disagreement.
#[derive(Debug, Clone, Default)]
pub struct SkillCollector {
    best: BTreeMap<String, u32>,
    pub rises: u64,
}

impl SkillCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn best(&self) -> &BTreeMap<String, u32> {
        &self.best
    }
}

impl Observer for SkillCollector {
    fn name(&self) -> &'static str {
        "skills"
    }

    fn on_entry(&mut self, entry: &LogEntry, _cx: &Context<'_>) {
        if let Event::SkillUp { skill, value } = &entry.event {
            let skill = skill.trim();
            if skill.is_empty() {
                return;
            }
            self.rises += 1;
            let cur = self.best.entry(skill.to_string()).or_default();
            *cur = (*cur).max(*value);
        }
    }
}

#[cfg(test)]
mod roster_tests {
    use super::*;
    use crate::feed::Feed;
    use crate::game::Game;

    fn players(text: &str) -> PlayerCollector {
        let mut c = PlayerCollector::new();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut c];
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs)
                .unwrap();
        }
        c
    }

    /// The ordinary line, with everything stated.
    #[test]
    fn a_who_line_names_a_player() {
        let c = players(
            "[Tue Jul 28 18:26:30 2026] [5 MNK/MAG] Ostrin (Iksar)  ZONE: Befallen (befallen)  \n",
        );
        let p = &c.seen()["Ostrin"];
        assert_eq!(p.level, 5);
        assert_eq!(p.classes, vec!["MNK", "MAG"]);
        assert_eq!(p.race, "Iksar");
    }

    /// Anonymous still names a player, which is the fact the roster is for.
    #[test]
    fn an_anonymous_entry_is_still_a_person() {
        let c = players("[Tue Jul 28 18:26:30 2026] [ANONYMOUS] Sneaky\n");
        assert!(c.seen().contains_key("Sneaky"));
        assert_eq!(c.seen()["Sneaky"].level, 0, "not stated, not level zero");
        assert_eq!(c.anonymous_rows, 1);
    }

    /// And it must not erase what a fuller reading already established.
    #[test]
    fn anonymity_does_not_overwrite_a_known_level() {
        let c = players(
            "[Tue Jul 28 18:26:30 2026] [42 SHM] Sneaky (Ogre)\n\
             [Tue Jul 28 19:26:30 2026] [ANONYMOUS] Sneaky\n",
        );
        assert_eq!(c.seen()["Sneaky"].level, 42);
    }

    /// A character levels, so the newest full reading wins.
    #[test]
    fn the_latest_reading_wins() {
        let c = players(
            "[Tue Jul 28 18:26:30 2026] [5 SHM] Serene (Ogre)\n\
             [Tue Jul 28 19:26:30 2026] [10 SHM/WIZ] Serene (Ogre)\n",
        );
        assert_eq!(c.seen()["Serene"].level, 10);
        assert_eq!(c.seen()["Serene"].classes, vec!["SHM", "WIZ"]);
    }
}

#[cfg(test)]
mod skill_tests {
    use super::*;
    use crate::feed::Feed;
    use crate::game::Game;

    /// A skill only goes up, so a lower reading is older rather than wrong.
    #[test]
    fn the_highest_reading_is_kept_whatever_order_they_arrive_in() {
        let mut c = SkillCollector::new();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        let text = "\
[Tue Jul 28 15:03:13 2026] You have become better at Baking! (21)
[Tue Jul 28 15:04:13 2026] You have become better at Baking! (35)
[Tue Jul 28 15:05:13 2026] You have become better at Baking! (30)
[Tue Jul 28 15:06:13 2026] You have become better at Brewing! (12)
";
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut c];
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs)
                .unwrap();
        }
        assert_eq!(c.best()["Baking"], 35);
        assert_eq!(c.best()["Brewing"], 12);
        assert_eq!(c.rises, 4);
    }
}

/// What kind of thing a marker stands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    /// Something you can hail. Recorded from `You say, 'Hail, X'`.
    Npc,
    /// The boundary to somewhere else.
    Zone,
    /// A station you use rather than talk to: a forge, a loom, an oven.
    ///
    /// Kept apart from [`Self::Npc`] because the difference is not
    /// cosmetic. A forge is never killed and never drops anything, so a page
    /// that counts kills against it is counting nothing, and the site draws
    /// and words the two differently for that reason.
    Object,
    /// A place an objective sends you to, named by the objective's own text.
    ///
    /// The odd one out: every other kind names a thing that is standing
    /// there, and this names a task. `Find a scroll that details who cursed
    /// the forest.` is not a creature and must not be filed as one.
    Quest,
    /// A label for a place — anything else the player wrote.
    Place,
}

impl MarkerKind {
    /// The word this kind is stored, published and drawn under.
    ///
    /// Here rather than at the three places that needed it — the daemon, the
    /// logscan tool and the site — because the three had grown separate
    /// copies of the same `match`, and a kind added to the enum compiled
    /// against two of them.
    ///
    /// `Object` publishes as `craft`, which is the word the site already
    /// draws and words: a marker of that kind gets no kill count and is
    /// described as standing somewhere rather than having been seen there.
    /// Both objects anybody has marked so far are crafting stations — a loom
    /// and a forge — and the distinction the site draws with the word is "a
    /// thing you use, not a person you hail", which is the same one.
    pub fn word(self) -> &'static str {
        match self {
            MarkerKind::Npc => "npc",
            MarkerKind::Zone => "zone",
            MarkerKind::Object => "craft",
            MarkerKind::Quest => "quest",
            MarkerKind::Place => "place",
        }
    }
}

/// One position somebody walked to and wrote down.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub name: String,
    pub kind: MarkerKind,
    /// What the player calls it — "Banker", "Ranger Guildmaster". The game
    /// never states this; it is the whole reason for the `--EQLCartographer--`
    /// convention.
    ///
    /// Empty for a `-- EQLDB --` marker, which has nowhere to write one. That
    /// is a gap in what was recorded, not a thing with no job.
    pub role: String,
    /// As `/who` reports it, which is the map you are standing on rather than
    /// the place a zone-line label names.
    pub zone: String,
    pub zone_short: String,
    /// **Map space, not `/loc` space.** See [`MarkerCollector`].
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// From a consider in the same block, when there was one.
    pub level: Option<u32>,
}

/// Map markers, from a convention the player follows at the spot.
///
/// ```text
/// You say, '--EQLCartographer--Ranger Guildmaster'
/// You say, 'Hail, Megosh Thistlethorn'
/// Your Location is -27.08, -440.14, -11.75
/// Megosh Thistlethorn glowers at you dubiously -- ... (Lvl: 70)
/// [50 WAR/DRU/BRD] Tharn (Ogre) ZONE: Rivervale (rivervale)
/// ```
///
/// One block gives everything a marker needs and nothing is typed twice: what
/// it is, where it stands, which zone's map that is, and often its level.
///
/// # Two conventions write this block
///
/// The one above, and the `-- EQLDB --` macros:
///
/// ```text
/// You say, '-- EQLDB -- Object : Loom'
/// You say, 'OBJECT::LOOM'
/// Your Location is 293.22, -57.08, -20.19
/// [2 SHD/WIZ] Mirela (Iksar) ZONE: New Sebilis Expedition 153 (newsebexp)
/// ```
///
/// Everything below the tag is the same and is read the same way. What
/// differs is what the tag itself carries: `--EQLCartographer--` states the
/// **role** and lets the line below settle the kind, and `-- EQLDB -- `
/// states the **kind** and has no room for a role. See [`EQLDB_TAG`].
///
/// The second line is a payload rather than prose in both: `Hail, X` names an
/// NPC, `OBJECT::X` a station, `QUEST_LOCATION:X` the objective a place
/// belongs to. That last one is a task, not a thing standing there — see
/// [`MarkerKind::Quest`].
///
/// # `/loc` is not map space
///
/// The client prints `(worldX, worldY, z)` and a map file is in
/// `(-worldY, -worldX, z)`, so a position is swapped and both values negated:
///
/// ```text
/// map(x, y, z) = (-loc2, -loc1, loc3)
/// ```
///
/// That rule is eqmaps' and was confirmed against the client rather than
/// inferred — one marker per candidate transform, and exactly one lands on
/// your own character arrow. Storing the raw numbers put 16 of 45 markers
/// outside their zone's geometry, which is how it was noticed.
///
/// Worth repeating the warning that comes with it: **distance to the nearest
/// map line is not a valid test.** It measures how densely a zone happens to
/// be contoured, and a wrong transform can score better than the right one in
/// a dense dungeon and then miss by 400 units in the next zone. Zone bounds
/// are the test that holds, because a player cannot stand outside the
/// geometry.
///
/// # The zone comes from `/who`, not from the label
///
/// Standing in the Southern Desert of Ro at the line to Innothule Swamp, the
/// label reads `Zone Line: Innothule Swamp` and `/who` reads `ZONE: The
/// Southern Desert of Ro (sro)`. The marker belongs on the map you are
/// standing on.
///
/// That line is worth more than the markers, and is collected separately: a
/// shortname is what a map file is called, a zone name is what a player calls
/// the place, and nothing in the client states the pairing.
#[derive(Debug, Clone, Default)]
pub struct MarkerCollector {
    markers: Vec<Marker>,
    /// Zone name to shortname, learned from `/who`.
    zones: BTreeMap<String, String>,
    open: Option<Open>,
    /// Blocks that never carried a `Your Location`, and so are not markers.
    pub blocks_without_a_position: u64,
}

#[derive(Debug, Clone)]
struct Open {
    role: String,
    seen: usize,
    name: Option<String>,
    kind: MarkerKind,
    loc: Option<(f64, f64, f64)>,
    zone: String,
    short: String,
    level: Option<u32>,
}

/// The tag that opens a block. Its suffix is the role.
const TAG: &str = "--EQLCartographer--";

/// The other tag that opens a block, from the `-- EQLDB --` macros.
///
/// ```text
/// You say, '-- EQLDB -- Mark NPC'
/// You say, 'Hail, Mistress Drozlen'
/// Your Location is 616.24, -168.38, -20.19
/// Mistress Drozlen judges you amiably — ... (Lvl: 70)
/// [2 SHD/WIZ] Mirela (Iksar) ZONE: New Sebilis Expedition 153 (newsebexp)
/// ```
///
/// Same block, different suffix. [`TAG`] puts the thing's *role* there and
/// leaves the kind to be worked out from the line below; this one states the
/// **kind** and leaves the role unsaid. So a marker from this convention has
/// an empty `role` — not because nobody filled it in, but because the
/// convention has nowhere to write one.
///
/// The trailing ` -- ` is load-bearing. The same logs carry two older, looser
/// uses of the word — `--EQLDB Inventory Dump` and `-- EQLDB -WAR DRU
/// Enchanter AA` — which are bookmarks for a person reading the file and have
/// no payload, no position and nothing to collect. Matching on the separator
/// keeps them out without a list of exceptions.
const EQLDB_TAG: &str = "-- EQLDB -- ";

/// `Object : Loom` -> `(Object, Some("Loom"))`.
///
/// The word before the colon says what kind of thing is being marked; what
/// follows is the player naming the individual one. `Mark NPC` and `Quest
/// Location` carry no detail because the line below names the thing.
///
/// An unrecognised kind is a `Place` rather than a refusal: the macro is a
/// text file somebody edits, and a marker filed under the wrong heading is
/// recoverable in a way that a position nobody recorded is not.
fn eqldb_tag(suffix: &str) -> (MarkerKind, Option<String>) {
    let (head, detail) = match suffix.split_once(':') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (suffix.trim(), ""),
    };
    let kind = match head.to_lowercase().as_str() {
        "mark npc" | "npc" => MarkerKind::Npc,
        "object" => MarkerKind::Object,
        "quest location" => MarkerKind::Quest,
        "zone line" | "zone" => MarkerKind::Zone,
        _ => MarkerKind::Place,
    };
    (kind, (!detail.is_empty()).then(|| detail.to_string()))
}

/// The line after the tag names the thing, and sometimes says what it is.
///
/// -> `(name, kind)`, where `None` means the line named the thing without
/// classifying it. That is not the same as "a place": under [`TAG`] the kind
/// is only ever what this line says, and under [`EQLDB_TAG`] the tag has
/// already said it — so a payload that says nothing must leave the tag's
/// answer alone rather than overwrite it with a guess.
///
/// `OBJECT::` and `QUEST_LOCATION:` come from the macros; `Hail, ` is what
/// you type at an NPC anyway and costs the player nothing.
fn payload(text: &str) -> (String, Option<MarkerKind>) {
    if let Some(who) = text.strip_prefix("Hail, ") {
        (who.trim().to_string(), Some(MarkerKind::Npc))
    } else if let Some(what) = text.strip_prefix("OBJECT::") {
        (what.trim().to_string(), Some(MarkerKind::Object))
    } else if let Some(what) = text.strip_prefix("QUEST_LOCATION:") {
        (what.trim().to_string(), Some(MarkerKind::Quest))
    } else if text.to_lowercase().starts_with("zone line:") {
        (text.to_string(), Some(MarkerKind::Zone))
    } else {
        (text.to_string(), None)
    }
}

/// How many entries past the tag to read.
///
/// Nothing closes a block, so it runs to the next tag or this many entries,
/// whichever comes first. Enough for every block on file, and it cannot run
/// into the following marker — which is the failure that would matter, since
/// it would put one creature's name on another's coordinates.
const BLOCK_WINDOW: usize = 14;

impl MarkerCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }

    /// Zone name to shortname pairs, which nothing in the client states.
    pub fn zones(&self) -> &BTreeMap<String, String> {
        &self.zones
    }

    /// One marker per name and spawn point, merged by distance.
    ///
    /// Offered rather than applied, because [`Self::markers`] is a list of
    /// things somebody observed and this is a judgement about what they were
    /// observations *of*. A caller that wants every visit can have it.
    ///
    /// Rounding to whole units was the first attempt and does not work: two
    /// visits to Arias gave -186.84 and -186.41, which round apart and
    /// survive as two markers a unit away from each other. What separates a
    /// second visit from a second spawn point is distance, not equality.
    ///
    /// 15 units is a body's width or two. The model is one row per spawn
    /// point and those are far apart where they exist — Befallen holds five
    /// `a shadowknight` hundreds of units from each other — so the threshold
    /// has a wide margin either side of it.
    ///
    /// Markers with no name or no zone shortname are left out: without either
    /// there is nothing to merge on, and nothing to put on a map.
    pub fn spawn_points(&self, near: f64) -> Vec<&Marker> {
        let mut out: Vec<&Marker> = Vec::new();
        for m in &self.markers {
            if m.name.trim().is_empty() || m.zone_short.is_empty() {
                continue;
            }
            let name = m.name.trim().to_lowercase();
            let dup = out.iter().any(|o| {
                o.name.trim().to_lowercase() == name
                    && o.zone_short == m.zone_short
                    && (o.x - m.x).powi(2) + (o.y - m.y).powi(2) <= near * near
            });
            if !dup {
                out.push(m);
            }
        }
        out
    }

    /// A block ended: keep it if it has a position, count it if not.
    fn close(&mut self) {
        let Some(o) = self.open.take() else { return };
        let Some((x, y, z)) = o.loc else {
            self.blocks_without_a_position += 1;
            return;
        };
        self.markers.push(Marker {
            name: o.name.unwrap_or_default(),
            kind: o.kind,
            role: o.role,
            zone: o.zone,
            zone_short: o.short,
            x,
            y,
            z,
            level: o.level,
        });
    }
}

/// `ZONE: The Southern Desert of Ro (sro)` — the name and the file it is in.
///
/// An instanced zone carries its instance number in the name: `ZONE: Befallen
/// 10250 (befallen)`. That is which copy of the place you are standing in,
/// not what the place is called, and keeping it files one marker under
/// "Befallen 10250" and the next under "Befallen 23553" — two zones, neither
/// of them real, and no pairing learned for the one that is.
///
/// Only a trailing run of three or more digits is taken as an instance
/// number. A zone whose name genuinely ends in a number would need more than
/// two digits to be caught by this, and none do.
fn split_zone(raw: &str) -> (String, String) {
    let t = raw.trim();
    let (name, short) = match (t.rfind('('), t.rfind(')')) {
        (Some(a), Some(b)) if b > a => (t[..a].trim(), t[a + 1..b].trim()),
        _ => (t, ""),
    };
    let name = match name.rsplit_once(' ') {
        Some((head, tail))
            if tail.len() >= 3 && tail.chars().all(|c| c.is_ascii_digit()) && !head.is_empty() =>
        {
            head.trim()
        }
        _ => name,
    };
    (name.to_string(), short.to_string())
}

impl Observer for MarkerCollector {
    fn name(&self) -> &'static str {
        "markers"
    }

    fn on_entry(&mut self, entry: &LogEntry, _cx: &Context<'_>) {
        // A tag closes whatever was open and opens the next, so two blocks
        // can never merge however short the second one is.
        if let Event::Chat {
            speaker, message, ..
        } = &entry.event
        {
            if speaker.is_you() {
                let text = message.trim();
                // Two conventions, one block. `--EQLCartographer--` names a
                // role and leaves the kind to the line below;
                // `-- EQLDB -- ` names a kind and leaves the role unsaid.
                let opened = if let Some(role) = text.strip_prefix(TAG) {
                    Some((role.trim().to_string(), MarkerKind::Place, None))
                } else if let Some(rest) = text.strip_prefix(EQLDB_TAG) {
                    let (kind, name) = eqldb_tag(rest);
                    Some((String::new(), kind, name))
                } else {
                    None
                };
                if let Some((role, kind, name)) = opened {
                    self.close();
                    self.open = Some(Open {
                        role,
                        seen: 0,
                        name,
                        kind,
                        loc: None,
                        zone: String::new(),
                        short: String::new(),
                        level: None,
                    });
                    return;
                }
            }
        }

        let Some(o) = &mut self.open else { return };
        o.seen += 1;
        if o.seen > BLOCK_WINDOW {
            self.close();
            return;
        }

        match &entry.event {
            // The line after the tag names the thing. Skipped entirely when
            // the tag already named it — `-- EQLDB -- Object : Loom` gives
            // `Loom` in the player's own casing and the payload below gives
            // `LOOM` in the macro's.
            Event::Chat {
                speaker, message, ..
            } if speaker.is_you() && o.name.is_none() => {
                let (name, kind) = payload(message.trim());
                o.name = Some(name);
                if let Some(k) = kind {
                    o.kind = k;
                }
            }
            // The only required part. Swapped and negated on the way in.
            Event::Location { x, y, z } if o.loc.is_none() => {
                o.loc = Some((-y, -x, *z));
            }
            // Which map this is. Any `/who` row in the block will do — the
            // player is the only one standing where they are standing.
            Event::WhoEntry { zone, .. } if o.short.is_empty() && !zone.is_empty() => {
                let (name, short) = split_zone(zone);
                if !short.is_empty() {
                    self.zones.insert(name.clone(), short.clone());
                }
                o.zone = name;
                o.short = short;
            }
            // A consider of the thing just named gives its level for free.
            Event::Consider { name, level, .. } if o.level.is_none() => {
                if let Some(want) = &o.name {
                    let a = strip_article(name).to_lowercase();
                    let b = strip_article(want).to_lowercase();
                    if a == b {
                        o.level = Some(*level);
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self, _cx: &Context<'_>) {
        // The last block in a file has no following tag to close it.
        self.close();
    }
}

#[cfg(test)]
mod marker_tests {
    use super::*;
    use crate::feed::Feed;
    use crate::game::Game;

    fn markers(text: &str) -> MarkerCollector {
        let mut c = MarkerCollector::new();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut c];
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs)
                .unwrap();
        }
        c
    }

    const BLOCK: &str = "\
[Thu Aug 13 18:10:21 2026] You say, '--EQLCartographer--Banker'
[Thu Aug 13 18:10:21 2026] You say, 'Hail, Andad Filla'
[Thu Aug 13 18:10:21 2026] Your Location is -3044.26, 1211.09, -21.14
[Thu Aug 13 18:10:22 2026] Andad Filla glowers at you dubiously -- looks kind of dangerous. (Lvl: 51)
[Thu Aug 13 18:10:22 2026] [50 WAR/DRU/BRD] Tharn (Ogre) <Iron Lantern> ZONE: The Southern Desert of Ro (sro)  
";

    /// One block gives everything a marker needs.
    #[test]
    fn a_block_gives_a_whole_marker() {
        let c = markers(BLOCK);
        assert_eq!(c.markers().len(), 1);
        let m = &c.markers()[0];
        assert_eq!(m.name, "Andad Filla");
        assert_eq!(m.kind, MarkerKind::Npc);
        assert_eq!(m.role, "Banker");
        assert_eq!(m.zone, "The Southern Desert of Ro");
        assert_eq!(m.zone_short, "sro");
        assert_eq!(m.level, Some(51));
    }

    /// `/loc` is not map space: swapped, and both values negated.
    #[test]
    fn a_position_is_converted_into_map_space() {
        let c = markers(BLOCK);
        let m = &c.markers()[0];
        assert!((m.x - -1211.09).abs() < 1e-9, "{}", m.x);
        assert!((m.y - 3044.26).abs() < 1e-9, "{}", m.y);
        assert!((m.z - -21.14).abs() < 1e-9, "{}", m.z);
    }

    /// The `-- EQLDB --` blocks, verbatim from the logs that carry them.
    ///
    /// One of each kind the macros write, in the order and with the stray
    /// lines they actually arrive with: an unrelated tell lands between
    /// `Klok Rento`'s payload and his position, and the block still closes on
    /// the right coordinates.
    const EQLDB: &str = "\
[Sat Sep 06 21:14:02 2026] You say, '-- EQLDB -- Mark NPC'
[Sat Sep 06 21:14:03 2026] You say, 'Hail, Mistress Drozlen'
[Sat Sep 06 21:14:04 2026] Your Location is 616.24, -168.38, -20.19
[Sat Sep 06 21:14:05 2026] Mistress Drozlen judges you amiably -- what would you like your tombstone to say? (Lvl: 70)
[Sat Sep 06 21:14:06 2026] [2 SHD/WIZ] Mirela (Iksar) <Iron Lantern> ZONE: New Sebilis Expedition 153 (newsebexp)  
[Sat Sep 06 21:20:11 2026] You say, '-- EQLDB -- Object : Loom'
[Sat Sep 06 21:20:12 2026] You say, 'OBJECT::LOOM'
[Sat Sep 06 21:20:13 2026] Your Location is 293.22, -57.08, -20.19
[Sat Sep 06 21:20:14 2026] [2 SHD/WIZ] Mirela (Iksar) <Iron Lantern> ZONE: New Sebilis Expedition 153 (newsebexp)  
[Sat Sep 06 21:31:40 2026] You say, '-- EQLDB -- Mark NPC'
[Sat Sep 06 21:31:41 2026] You say, 'Hail, Klok Rento'
[Sat Sep 06 21:31:41 2026] Neilehlm tells NewPlayers:1, 'Splitpaw has some, but it's higher level'
[Sat Sep 06 21:31:42 2026] Your Location is 298.31, -101.45, -20.19
[Sat Sep 06 21:31:43 2026] Klok Rento judges you amiably -- what would you like your tombstone to say? (Lvl: 50)
[Sat Sep 06 21:31:44 2026] [2 SHD/WIZ] Mirela (Iksar) <Iron Lantern> ZONE: New Sebilis Expedition 153 (newsebexp)  
[Sat Sep 06 22:02:55 2026] You say, '-- EQLDB -- Quest Location'
[Sat Sep 06 22:02:56 2026] You say, 'QUEST_LOCATION:Find a scroll that details who cursed the forest.'
[Sat Sep 06 22:02:57 2026] Your Location is -69.08, 213.54, -0.19
[Sat Sep 06 22:02:58 2026] [5 SHD/WIZ] Mirela (Iksar) <Iron Lantern> ZONE: New Sebilis Expedition 153 (newsebexp)  
";

    /// The second convention is read as a marker, kind and all.
    ///
    /// Before this it was not read at all: the tag did not match, so the 42
    /// blocks on file produced nothing.
    #[test]
    fn the_eqldb_macros_are_markers_too() {
        let c = markers(EQLDB);
        let got: Vec<(&str, MarkerKind)> = c
            .markers()
            .iter()
            .map(|m| (m.name.as_str(), m.kind))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Mistress Drozlen", MarkerKind::Npc),
                ("Loom", MarkerKind::Object),
                ("Klok Rento", MarkerKind::Npc),
                (
                    "Find a scroll that details who cursed the forest.",
                    MarkerKind::Quest
                ),
            ]
        );
        // Everything below the tag is read the same way as the other
        // convention's, which is the whole reason for sharing the collector.
        assert_eq!(c.markers()[0].zone_short, "newsebexp");
        assert_eq!(c.markers()[0].zone, "New Sebilis Expedition");
        assert_eq!(c.markers()[0].level, Some(70));
        assert!(
            (c.markers()[2].x - 101.45).abs() < 1e-9,
            "an unrelated tell moved the position"
        );
    }

    /// This convention states a kind and has no room for a role.
    ///
    /// Empty, rather than the tag's own words: filing `Mark NPC` as a role
    /// would put it on the map where `Banker` goes, as though somebody had
    /// said what Drozlen does.
    #[test]
    fn an_eqldb_marker_has_no_role() {
        let c = markers(EQLDB);
        assert!(c.markers().iter().all(|m| m.role.is_empty()));
    }

    /// The object's name is the player's casing, not the macro's.
    ///
    /// Both lines name it. `Object : Loom` is what a person typed and
    /// `OBJECT::LOOM` is what the macro sends, and the map shows the name.
    #[test]
    fn an_object_is_named_by_the_tag_not_the_payload() {
        let c = markers(EQLDB);
        assert_eq!(c.markers()[1].name, "Loom");
        // With no detail on the tag there is nothing else to go on, and the
        // payload is taken rather than the marker left unnamed.
        let bare = markers(
            "[Sat Sep 06 21:20:11 2026] You say, '-- EQLDB -- Object'\n\
             [Sat Sep 06 21:20:12 2026] You say, 'OBJECT::FORGE'\n\
             [Sat Sep 06 21:20:13 2026] Your Location is 296.56, -114.84, -20.18\n",
        );
        assert_eq!(bare.markers()[0].name, "FORGE");
        assert_eq!(bare.markers()[0].kind, MarkerKind::Object);
    }

    /// The older, looser uses of the word are not markers.
    ///
    /// Both sit in the same logs as the real blocks. They are bookmarks for a
    /// person reading the file: no payload, no position, nothing to collect.
    /// Opening a block on them would swallow whatever happened to follow.
    #[test]
    fn a_bookmark_is_not_a_block() {
        let c = markers(
            "[Sat Sep 06 12:00:00 2026] You say, '--EQLDB Inventory Dump'\n\
             [Sat Sep 06 12:00:01 2026] You say, '-- EQLDB -WAR DRU Enchanter AA'\n\
             [Sat Sep 06 12:00:02 2026] Your Location is 1.0, 2.0, 3.0\n",
        );
        assert!(c.markers().is_empty(), "{:?}", c.markers());
        assert_eq!(c.blocks_without_a_position, 0);
    }

    /// The pairing `/who` states for free is worth more than the marker.
    #[test]
    fn a_who_line_teaches_a_zone_its_shortname() {
        let c = markers(BLOCK);
        assert_eq!(c.zones()["The Southern Desert of Ro"], "sro");
    }

    /// An instance number is which copy you are in, not what the place is.
    #[test]
    fn an_instance_number_is_not_part_of_the_zone_name() {
        assert_eq!(
            split_zone("Befallen 10250 (befallen)"),
            ("Befallen".to_string(), "befallen".to_string())
        );
        assert_eq!(
            split_zone("The Southern Desert of Ro (sro)"),
            ("The Southern Desert of Ro".to_string(), "sro".to_string())
        );
        // Two digits is not an instance number, and a bare number is not a
        // zone with one stripped off.
        assert_eq!(split_zone("Guk 42 (guktop)").0, "Guk 42");
        assert_eq!(split_zone("1234 (x)").0, "1234");
    }

    /// A tag closes the block before it, so two markers never merge.
    ///
    /// The failure that would matter: it would put one creature's name on
    /// another's coordinates.
    #[test]
    fn a_new_tag_closes_the_block_before_it() {
        let mut text = String::from(BLOCK);
        text.push_str(
            "[Thu Aug 13 18:11:10 2026] You say, '--EQLCartographer--'\n\
             [Thu Aug 13 18:11:10 2026] You say, 'Zone Line: Innothule Swamp'\n\
             [Thu Aug 13 18:11:10 2026] Your Location is -3125.36, 1073.25, -21.12\n",
        );
        let c = markers(&text);
        assert_eq!(c.markers().len(), 2);
        assert_eq!(c.markers()[0].name, "Andad Filla");
        assert_eq!(c.markers()[1].kind, MarkerKind::Zone);
        assert_eq!(c.markers()[1].name, "Zone Line: Innothule Swamp");
    }

    /// A block with no position is not a marker.
    #[test]
    fn a_block_without_a_location_is_counted_not_kept() {
        let c = markers(
            "[Thu Aug 13 18:10:21 2026] You say, '--EQLCartographer--Banker'\n\
             [Thu Aug 13 18:10:21 2026] You say, 'Hail, Andad Filla'\n",
        );
        assert!(c.markers().is_empty());
        assert_eq!(c.blocks_without_a_position, 1);
    }

    /// Two visits to one spot are one spawn point, and rounding cannot say so.
    #[test]
    fn two_visits_to_one_spot_fold_into_one() {
        let mut text = String::from(BLOCK);
        // -186.84 and -186.41 round to -187 and -186 and would survive as two.
        text.push_str(&BLOCK.replace("-3044.26, 1211.09", "-3044.69, 1211.52"));
        let c = markers(&text);
        assert_eq!(c.markers().len(), 2, "both visits are kept as observations");
        assert_eq!(c.spawn_points(15.0).len(), 1, "and are one spawn point");
    }
}

/// Drops, kills and the bestiary — the three the killing-blow rule ties together.
///
/// These are separate trackers and they cannot be separate observers, because
/// whether a death counts at all is decided from encounter state:
///
/// ```text
/// Innoruuk, the Prince of Hate has been slain by Innoruuk`s Chosen!
/// Lady Vox has been slain by a priest of Nagafen!
/// ```
///
/// The game credits the final hit to whoever landed it, which for a charming
/// class is routinely your own charmed pet. Requiring your own killing blow
/// silently discards those kills. So credit follows *participation* — did you
/// damage it during the fight that is open right now — and that answer lives
/// in [`EncounterTracker`].
///
/// The order matters and is the daemon's: the death is judged against the
/// encounter as it stands *before* this entry is pushed into it. Pushing
/// first closes the fight and the answer becomes no.
///
/// # What each is for
///
/// [`DropTracker`] holds both halves of a drop rate. `on_drop` is what came
/// off a corpse and `on_kill` is the denominator — counted whether or not the
/// corpse was looted, which is what makes a sighting count a floor rather
/// than a rate.
///
/// [`Bestiary`] holds what fighting a creature was like: what it hit you for,
/// how often it missed, and a floor on its health from the damage it absorbed
/// before dying. It is keyed on where you were, so a zone change ends every
/// fight in the old zone — anything still accumulating there was left alive
/// and its partial damage is a floor for nothing.
pub struct CombatCollector {
    player: String,
    drops: crate::drops::DropTracker,
    bestiary: crate::bestiary::Bestiary,
}

impl CombatCollector {
    pub fn new(player: &str, game: crate::game::Game) -> Self {
        Self {
            player: player.to_string(),
            drops: crate::drops::DropTracker::new(game),
            bestiary: crate::bestiary::Bestiary::new(game, player),
        }
    }

    pub fn drops(&self) -> crate::drops::DropReport {
        self.drops.report()
    }

    pub fn bestiary(&self) -> &crate::bestiary::Bestiary {
        &self.bestiary
    }
}

impl Observer for CombatCollector {
    fn name(&self) -> &'static str {
        "combat"
    }

    fn on_entry(&mut self, entry: &LogEntry, cx: &Context<'_>) {
        use crate::bestiary::Swing;

        match &entry.event {
            Event::Damage {
                source,
                target,
                amount,
                kind,
                ability,
                damage_type,
                ..
            } => {
                if let Actor::Named(npc) = target {
                    self.bestiary.on_damage_to(entry.timestamp, npc, *amount);
                }
                if let Actor::Named(npc) = source {
                    file_npc_damage(
                        &mut self.bestiary,
                        entry.timestamp,
                        npc,
                        target.is_you(),
                        *amount,
                        *kind,
                        ability.as_deref(),
                        damage_type.as_deref(),
                    );
                }
            }
            Event::CastStart {
                source: Actor::Named(npc),
                spell,
                ..
            } => self.bestiary.on_npc_cast(
                entry.timestamp,
                npc,
                spell,
                crate::bestiary::CastSeen::Announced,
                false,
            ),
            Event::Miss { source, target, .. } => {
                if target.is_you() {
                    if let Actor::Named(npc) = source {
                        self.bestiary.on_swing(entry.timestamp, npc, Swing::Missed);
                    }
                }
            }
            Event::Avoided { source, target, .. } => {
                if target.is_you() {
                    if let Actor::Named(npc) = source {
                        self.bestiary.on_swing(entry.timestamp, npc, Swing::Avoided);
                    }
                }
            }
            Event::Loot {
                item,
                quantity,
                source,
                ..
            } => {
                // A named corpse that is not your own is a creature, and what
                // came off it is a drop. Your own corpse is items going back
                // where they came from, which says nothing about any creature.
                if let Some(Actor::Named(npc)) = source {
                    if !npc.eq_ignore_ascii_case(&self.player) {
                        self.drops
                            .on_drop(entry.timestamp, item, *quantity, npc, cx.zone);
                    }
                }
            }
            Event::StanceChanged { stance } => self.bestiary.on_stance(stance),
            Event::LevelUp { level } => self.bestiary.on_level(*level as u32),
            Event::ZoneChange { zone } => self.bestiary.on_zone(zone),
            Event::Death { victim, killer } => {
                if victim.is_you() {
                    // What actually kills you is worth knowing per creature,
                    // and it is the one statistic a death line states outright.
                    if let Some(Actor::Named(k)) = killer {
                        self.bestiary.on_your_death(entry.timestamp, k);
                    }
                } else if killer.as_ref().is_some_and(|k| k.is_you()) || cx.fought(victim) {
                    {
                        let name = victim.display(&self.player);
                        self.drops.on_kill(&name);
                        self.bestiary.on_kill(entry.timestamp, &name);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Vendor prices and tradeskill combines.
///
/// One tracker rather than two collectors, because [`TradeskillTracker`] is
/// one: a purchase, a quote, a combine and a skill rise all land in it, and
/// what a combine cost is worked out from the prices of what went into it.
///
/// [`TradeskillTracker`]: crate::tradeskill::TradeskillTracker
///
/// # The zone is where you are standing
///
/// A merchant does not say where they are. The zone comes from the feed's
/// context, which is what the daemon does with `current_zone` — where you are
/// standing is where the merchant is.
pub struct TradeCollector {
    trades: crate::tradeskill::TradeskillTracker,
    game: crate::game::Game,
}

impl TradeCollector {
    pub fn new(game: crate::game::Game) -> Self {
        Self {
            trades: crate::tradeskill::TradeskillTracker::new(),
            game,
        }
    }

    /// With the client's own recipe book behind it, so a combine can be
    /// attributed to the tradeskill that made it.
    pub fn with_book(game: crate::game::Game, book: &'static crate::recipe::RecipeBook) -> Self {
        Self {
            trades: crate::tradeskill::TradeskillTracker::with_book(book),
            game,
        }
    }

    /// The report, tagged with the game.
    ///
    /// The tracker knows nothing about which game it is reading and the cost
    /// totals mean different things in the two, so the tag is stamped on here
    /// — the same thing the daemon does after calling `report`, and the
    /// reason it is done here instead is that forgetting it produces a report
    /// labelled "unknown" and nothing complains.
    pub fn report(&self) -> crate::tradeskill::TradeskillReport {
        let mut r = self.trades.report();
        r.game = self.game;
        r
    }
}

impl Observer for TradeCollector {
    fn name(&self) -> &'static str {
        "trades"
    }

    fn on_entry(&mut self, entry: &LogEntry, cx: &Context<'_>) {
        let at = entry.timestamp;
        match &entry.event {
            Event::Craft { item, success } => self.trades.on_craft(at, item, *success),
            Event::ComponentConsumed {
                item,
                quantity,
                remaining,
            } => self
                .trades
                .on_component(at, item, *quantity, Some(*remaining)),
            Event::ItemPurchased {
                item,
                quantity,
                coin,
                merchant,
            } => self.trades.on_purchase_from(
                at,
                item,
                *quantity,
                coin.as_copper(),
                Some(merchant.as_str()),
                cx.zone,
            ),
            Event::ItemQuoted {
                item,
                merchant,
                coin,
            } => self.trades.on_quote_from(
                at,
                item,
                coin.as_copper(),
                Some(merchant.as_str()),
                cx.zone,
            ),
            Event::RecipeLearned { recipe } => self.trades.on_recipe_learned(recipe),
            Event::SkillTrained { skill, .. } => self.trades.on_trained(skill),
            Event::SkillUp { skill, value } => self.trades.on_skill_up(at, skill, *value),
            _ => {}
        }
    }
}

/// The Plane of Sky chain and the task list.
///
/// One observer for two trackers, because they read the same lines: quest
/// dialogue arrives as say-channel chat, both count kills, and both are
/// driven by definitions loaded from outside.
///
/// # Both need to know whether a death was yours
///
/// `on_kill` on either is gated on participation, for the reason
/// [`Context::fought`] gives — a Sky island named is routinely finished by a
/// charmed pet, and requiring your own killing blow discards it. The two
/// gates are not identical, and the difference is the daemon's: the task
/// list is only told about a kill that also counted for experience, while
/// Sky counts it on participation alone.
///
/// # Only your own say starts a Sky quest
///
/// The chain begins by saying a trigger word, and it has to be *your*
/// utterance: somebody else in the zone saying it starts nothing. The giver's
/// half is recorded separately, since the giver's own words are what a
/// walkthrough is built from.
///
/// Chat on any other channel is player conversation and would be noise —
/// only `say` is read.
pub struct QuestCollector {
    tasks: crate::task::TaskTracker,
    sky: crate::skyquest::SkyTracker,
}

impl Default for QuestCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl QuestCollector {
    /// Seeded with the built-in task table, which is what the daemon falls
    /// back to when no `tasks.json` has been written. A tracker with no
    /// definitions recognises no task, and says so by reporting none — which
    /// reads exactly like a character who has never taken one.
    pub fn new() -> Self {
        let mut tasks = crate::task::TaskTracker::new();
        for d in crate::task::seed_definitions() {
            tasks.define(d);
        }
        Self {
            tasks,
            sky: crate::skyquest::SkyTracker::new(),
        }
    }

    /// What a task is, which the log never says in full.
    pub fn define_task(&mut self, def: crate::task::TaskDef) {
        self.tasks.define(def);
    }

    pub fn tasks(&self) -> crate::task::TaskReport {
        self.tasks.report()
    }

    /// `active` and `played` are the classes to report against; the chain is
    /// per class and a report for nobody in particular is not meaningful.
    pub fn sky(&self, active: &[String], played: &[String]) -> crate::skyquest::SkyReport {
        self.sky.report_for(active, played)
    }

    pub fn turn_in_log(&self) -> Vec<(String, i64)> {
        self.sky.turn_in_log()
    }
}

impl Observer for QuestCollector {
    fn name(&self) -> &'static str {
        "quests"
    }

    fn on_entry(&mut self, entry: &LogEntry, cx: &Context<'_>) {
        let at = entry.timestamp;
        match &entry.event {
            Event::Chat {
                speaker,
                channel,
                message,
            } => {
                if !(channel.eq_ignore_ascii_case("say") || channel.eq_ignore_ascii_case("says")) {
                    return;
                }
                let who = speaker.display(cx.player);
                self.tasks.on_dialogue(at, &who, message);
                if speaker.is_you() {
                    self.sky.on_say(at, message);
                } else {
                    self.sky.on_giver_line(at, &who, message);
                }
            }
            Event::TaskAssigned { task } => self.tasks.on_assigned(at, task),
            Event::TaskUpdated { task } => self.tasks.on_updated(at, task),
            Event::AchievementEarned { name } => self.tasks.on_achievement(at, name),
            Event::Loot { item, quantity, .. } => self.sky.on_loot(item, *quantity),
            Event::TradeOffer { item, quantity, .. } => self.sky.on_offer(item, *quantity),
            Event::TradeComplete { npc } => self.sky.on_trade_complete(at, npc),
            Event::Death { victim, killer } if !victim.is_you() => {
                let mine = killer.as_ref().is_some_and(|k| k.is_you()) || cx.fought(victim);
                if mine {
                    let name = victim.display(cx.player);
                    // Lets a following "task has been updated" say which step.
                    self.tasks.on_kill(at, &name);
                    self.sky.on_kill(at, &name);
                }
            }
            _ => {}
        }
    }
}

/// Alternate advancement: what was bought, and what it cost.
///
/// The log states a purchase outright — the ability, the rank reached and the
/// points paid — which makes this the one place a *price* can be observed
/// rather than read out of a catalogue.
///
/// # Two ranks, and they are not interchangeable
///
/// A toggle AA is one ability the client walks up two interleaved ladders —
/// Symphonic Aura buys at "Disabled" 3, 5, 7, 9 for three points each and
/// hands back "Enabled" 2, 4, 6, 8, 10 for nothing. So the client's ladder
/// position reaches 10 where the player has five ranks.
///
/// [`AaPurchase`] keeps both. `rank` is what the player has and `client_rank`
/// is where the client counts them, and which one a consumer wants depends on
/// what it is lining the number up against: a price list numbered the way the
/// client numbers it needs `client_rank`, and anything describing the
/// character needs `rank`. Reading the wrong one lands every observation on
/// the wrong rung.
///
/// [`AaPurchase`]: crate::experience::AaPurchase
///
/// # A floor, not a total
///
/// It only sees purchases made while a log was running. A character who
/// bought half their abilities before anybody started logging shows those
/// ranks as unlogged, and [`crate::experience::AaPurchase`] keeps that
/// distinction rather than presenting a partial tally as a complete one.
pub struct AaCollector {
    exp: crate::experience::ExpTracker,
    ladder: Vec<AaStep>,
    running: BTreeMap<String, u32>,
}

/// One rung of an ability's ladder, as the log stated it.
///
/// [`crate::experience::ExpTracker`] folds every purchase of an ability into
/// one row holding the rank reached and the total paid, which is what a
/// character sheet wants. It is not what a *price list* wants: the cost of
/// going from rank 2 to rank 3 is only visible in the difference between two
/// rungs, and the fold throws the earlier ones away.
///
/// The parser's history database happened to keep them, because its rows are
/// keyed on the rank and it never forgets — so parsing the same log on four
/// different evenings left four rows and the prices could be read off them.
/// That is accumulation standing in for evidence, and it stops working the
/// moment a log is read once. The log itself states every purchase:
///
/// ```text
/// You have improved Circular Breathing 3 at a cost of 4 ability points.
/// ```
///
/// so the ladder is recoverable in one pass, which is what this is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaStep {
    /// Folded, so a toggle's two halves are one ability.
    pub ability: String,
    /// The client's ladder position, which for a toggle counts two rungs per
    /// rank — see [`AaCollector`].
    pub client_rank: u32,
    /// Points paid for this rung alone.
    pub cost: u32,
    /// Points paid for this ability up to and including this rung, which is
    /// what the history database's `spent` column holds.
    pub spent_so_far: u32,
    pub at: i64,
}

impl Default for AaCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl AaCollector {
    pub fn new() -> Self {
        Self {
            exp: crate::experience::ExpTracker::default(),
            ladder: Vec::new(),
            running: BTreeMap::new(),
        }
    }

    /// Every rung the log stated, in the order it stated them.
    pub fn ladder(&self) -> &[AaStep] {
        &self.ladder
    }

    pub fn purchases(&self) -> Vec<crate::experience::AaPurchase> {
        self.exp.aa_purchases()
    }

    /// Lifetime points earned, as the client has stated them.
    pub fn earned(&self) -> u32 {
        self.exp.aa_earned()
    }

    /// Points in hand, which is the client's own figure rather than a
    /// subtraction — earned minus spent is wrong the moment a purchase
    /// happened off-log.
    pub fn unspent(&self) -> u32 {
        self.exp.aa_unspent()
    }
}

impl Observer for AaCollector {
    fn name(&self) -> &'static str {
        "aa"
    }

    fn on_entry(&mut self, entry: &LogEntry, _cx: &Context<'_>) {
        match &entry.event {
            Event::AbilityPointsGained { points, unspent } => {
                self.exp
                    .on_ability_points(entry.timestamp, *points, *unspent)
            }
            Event::AbilityPointsSpent {
                ability,
                rank,
                cost,
            } => {
                // Folded to the one ability the player bought, the same way
                // the tracker folds it, so a toggle's two halves do not read
                // as two ladders.
                let name = match crate::aa::toggle_parts(ability) {
                    Some((base, _)) => base.to_string(),
                    None => ability.to_string(),
                };
                let run = self.running.entry(name.to_lowercase()).or_insert(0);
                *run += *cost;
                self.ladder.push(AaStep {
                    ability: name,
                    client_rank: *rank,
                    cost: *cost,
                    spent_so_far: *run,
                    at: entry.timestamp,
                });
                self.exp
                    .on_ability_spent(entry.timestamp, ability, *rank, *cost)
            }
            _ => {}
        }
    }
}

/// Where a creature was standing when it died.
///
/// A `Marker` is curated: somebody typed `--EQLCartographer--Banker` at the
/// spot because they decided that spot was worth a pin. This is the opposite
/// — every kill the player happened to `/loc` beside, kept whether or not
/// anybody has decided it matters. 45 of 593 creatures have a marker; the
/// point of this is the other 548.
///
/// **Many rows per creature is the shape, not a defect.** A creature with
/// four spawn points produces four clusters, and that is the map worth
/// drawing. Nothing here dedupes or averages: two sightings twenty units
/// apart are two sightings, and whether they are one spawn or two is a
/// question for whatever draws them.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CreatureSpot {
    pub name: String,
    pub zone: String,
    pub zone_short: String,
    /// **Map space, not `/loc` space.** Same transform a `Marker` gets.
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// When the creature died.
    pub at: i64,
    /// Seconds between the death and the `/loc`.
    ///
    /// The whole reliability of the row. A player standing on a corpse and
    /// typing `/loc` is within melee range of where the thing stood; one who
    /// walked for a minute first is somewhere else entirely. Carried rather
    /// than thresholded here so the reader can decide: the collector keeps
    /// anything inside [`SPOT_WINDOW`] and says how close it was.
    pub gap: i64,
}

/// How long after a death a `/loc` can still describe where it stood.
///
/// Ten seconds. A corpse is looted in about that, so it is the window in
/// which a player has plausibly not moved, and it is short enough that
/// walking away disqualifies itself. Measured against the log this was
/// written for: of 7,560 kills, 19 have a `/loc` within five seconds and 155
/// within thirty, so nothing here recovers a useful dataset from logs
/// already recorded. It exists so that a player who starts typing `/loc`
/// after a kill builds one from here on.
pub const SPOT_WINDOW: i64 = 10;

/// Every kill the player stood over and measured.
///
/// See [`CreatureSpot`]. The rule is one line long and the reason it is not
/// shorter is [`SPOT_WINDOW`].
#[derive(Debug, Clone, Default)]
pub struct SpotCollector {
    spots: Vec<CreatureSpot>,
    /// The last death, waiting for a `/loc` to describe it.
    pending: Option<(String, i64)>,
    zone: String,
    zone_short: String,
}

impl SpotCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spots(&self) -> &[CreatureSpot] {
        &self.spots
    }
}

impl Observer for SpotCollector {
    fn name(&self) -> &'static str {
        "spots"
    }

    fn on_entry(&mut self, entry: &LogEntry, cx: &Context<'_>) {
        match &entry.event {
            Event::ZoneChange { zone } => {
                let z = crate::zone::Zone::parse(zone);
                self.zone = z.name.clone();
                self.zone_short.clear();
                self.pending = None;
            }
            // `/who` is where a zone's shortname comes from; nothing else
            // states it. Same source `MarkerCollector` uses.
            Event::WhoEntry { zone, .. } if !zone.is_empty() => {
                let (_, short) = split_zone(zone);
                if !short.is_empty() {
                    self.zone_short = short;
                }
            }
            Event::Death { victim, killer } => {
                // Only what the player killed. A groupmate's kill is a
                // position too, but the player was not necessarily standing
                // there, and this rule rests entirely on where they stood.
                if matches!(killer, Some(Actor::You)) {
                    if let Actor::Named(n) = victim {
                        self.pending = Some((n.clone(), entry.timestamp));
                    }
                }
            }
            Event::Location { x, y, z } => {
                let Some((name, at)) = self.pending.clone() else {
                    return;
                };
                let gap = entry.timestamp - at;
                if !(0..=SPOT_WINDOW).contains(&gap) {
                    return;
                }
                self.spots.push(CreatureSpot {
                    name: strip_article(&name).to_string(),
                    zone: self.zone.clone(),
                    zone_short: self.zone_short.clone(),
                    // `/loc` prints (worldX, worldY, z) and a map file is in
                    // (-worldY, -worldX, z). The same transform a marker
                    // gets, and for the same documented reason.
                    x: -y,
                    y: -x,
                    z: *z,
                    at,
                    gap,
                });
                // One spot per death. A second `/loc` describes the same
                // corpse, not a second sighting.
                self.pending = None;
            }
            _ => {}
        }
        let _ = cx;
    }
}

#[cfg(test)]
mod spot_tests {
    use super::*;
    use crate::feed::Feed;
    use crate::game::Game;

    fn spots(text: &str) -> Vec<CreatureSpot> {
        let mut c = SpotCollector::new();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut c];
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs)
                .unwrap();
        }
        c.spots().to_vec()
    }

    /// A kill you stood over and measured, and the transform on the way in.
    #[test]
    fn a_loc_after_your_kill_places_the_creature() {
        let got = spots(
            "\
[Wed Aug 19 10:00:00 2026] You have entered Befallen.
[Wed Aug 19 10:00:01 2026] Players in EverQuest Legends:
[Wed Aug 19 10:00:01 2026] [50 WAR] Tharn (Ogre) ZONE: Befallen (befallen)
[Wed Aug 19 10:00:10 2026] You have slain a decaying skeleton!
[Wed Aug 19 10:00:12 2026] Your Location is 179.17, -144.70, -20.17
",
        );
        assert_eq!(got.len(), 1, "{got:?}");
        let s = &got[0];
        assert_eq!(
            s.name, "decaying skeleton",
            "the article goes, as everywhere else"
        );
        assert_eq!(s.zone, "Befallen");
        assert_eq!(s.zone_short, "befallen");
        // map(x, y, z) = (-loc2, -loc1, loc3). Same rule a marker gets, and
        // getting it wrong is what once put 16 of 45 markers outside their
        // own zone's geometry.
        assert_eq!((s.x, s.y, s.z), (144.70, -179.17, -20.17));
        assert_eq!(
            s.gap, 2,
            "and it says how far the reading is from the death"
        );
    }

    /// The window is the whole reliability of the row.
    #[test]
    fn a_loc_long_after_the_kill_describes_somewhere_else() {
        let got = spots(
            "\
[Wed Aug 19 10:00:00 2026] You have entered Befallen.
[Wed Aug 19 10:00:10 2026] You have slain a decaying skeleton!
[Wed Aug 19 10:01:30 2026] Your Location is 179.17, -144.70, -20.17
",
        );
        assert!(
            got.is_empty(),
            "eighty seconds of walking is not a position, {got:?}"
        );
    }

    /// Only what you killed. A groupmate's kill happened where *they* stood.
    #[test]
    fn somebody_elses_kill_is_not_your_position() {
        let got = spots(
            "\
[Wed Aug 19 10:00:00 2026] You have entered Befallen.
[Wed Aug 19 10:00:10 2026] Daveynub has slain a decaying skeleton!
[Wed Aug 19 10:00:12 2026] Your Location is 179.17, -144.70, -20.17
",
        );
        assert!(got.is_empty(), "{got:?}");
    }

    /// Many rows per creature is the shape. Four spawn points are four
    /// clusters, and nothing here averages them into one wrong answer.
    #[test]
    fn one_creature_can_stand_in_several_places() {
        let got = spots(
            "\
[Wed Aug 19 10:00:00 2026] You have entered Befallen.
[Wed Aug 19 10:00:10 2026] You have slain a decaying skeleton!
[Wed Aug 19 10:00:11 2026] Your Location is 100.00, 100.00, 0.00
[Wed Aug 19 10:05:10 2026] You have slain a decaying skeleton!
[Wed Aug 19 10:05:11 2026] Your Location is 900.00, 900.00, 0.00
",
        );
        assert_eq!(got.len(), 2, "{got:?}");
        assert_ne!((got[0].x, got[0].y), (got[1].x, got[1].y));
    }

    /// A second `/loc` describes the same corpse, not a second sighting.
    #[test]
    fn one_death_gives_one_spot() {
        let got = spots(
            "\
[Wed Aug 19 10:00:00 2026] You have entered Befallen.
[Wed Aug 19 10:00:10 2026] You have slain a decaying skeleton!
[Wed Aug 19 10:00:11 2026] Your Location is 100.00, 100.00, 0.00
[Wed Aug 19 10:00:13 2026] Your Location is 100.00, 100.00, 0.00
",
        );
        assert_eq!(got.len(), 1, "{got:?}");
    }
}

/// What one zone yields to forage, folded across a log.
///
/// Keyed on the zone's display name, which is what [`crate::zone::Zone`]
/// settles — a zone announced as `Everfrost Peaks` and as
/// `Everfrost Peaks - Group` is one place to forage in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZoneForage {
    /// As the client printed it, and how many times.
    pub found: BTreeMap<String, u64>,
    /// Attempts that named nothing.
    ///
    /// Kept because it is the denominator: `Pod of Water` seen four times
    /// says something quite different in a zone with six attempts than in one
    /// with sixty, and only this side of the pair carries that.
    pub empty: u64,
    /// First and last attempt here, successful or not.
    pub first: i64,
    pub last: i64,
}

impl ZoneForage {
    /// Attempts that yielded something.
    pub fn hits(&self) -> u64 {
        self.found.values().sum()
    }

    /// Every attempt, including the ones that found nothing.
    pub fn attempts(&self) -> u64 {
        self.hits() + self.empty
    }
}

/// What forage yields, per zone.
///
/// # Why the zone comes from the context and not the line
///
/// `You have scrounged up Pod of Water.` names the item and nothing else. It
/// does not say where, who, or which skill — forage is self-only, so the who
/// is the log's owner by construction, and the where is whatever zone the
/// feed last saw announced. That is the same arrangement drops and creature
/// spots already have, and for the same reason: a line does not know where it
/// happened.
///
/// An attempt before any zone has been announced is counted in
/// [`Self::homeless`] rather than filed under a guess. That happens when a
/// log begins mid-session, which is routine — the client announces the zone
/// on arrival, not on `/log on`.
#[derive(Debug, Clone, Default)]
pub struct ForageCollector {
    zones: BTreeMap<String, ZoneForage>,
    /// Attempts that arrived before any zone was announced.
    pub homeless: u64,
}

impl ForageCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn zones(&self) -> &BTreeMap<String, ZoneForage> {
        &self.zones
    }

    /// Every item ever foraged anywhere, and how often.
    pub fn items(&self) -> BTreeMap<&str, u64> {
        let mut out: BTreeMap<&str, u64> = BTreeMap::new();
        for z in self.zones.values() {
            for (item, n) in &z.found {
                *out.entry(item.as_str()).or_default() += n;
            }
        }
        out
    }
}

impl Observer for ForageCollector {
    fn name(&self) -> &'static str {
        "forage"
    }

    fn on_entry(&mut self, entry: &LogEntry, cx: &Context<'_>) {
        let Event::Forage { item } = &entry.event else {
            return;
        };
        let Some(zone) = cx.zone else {
            self.homeless += 1;
            return;
        };
        let name = crate::zone::Zone::parse(zone).name;
        let row = self.zones.entry(name).or_insert_with(|| ZoneForage {
            first: entry.timestamp,
            ..Default::default()
        });
        match item {
            Some(i) if !i.trim().is_empty() => {
                *row.found.entry(i.trim().to_string()).or_default() += 1;
            }
            // Both the failure line and a success whose item did not survive
            // the match land here. Counting the second as an attempt rather
            // than dropping it keeps the ratio honest.
            _ => row.empty += 1,
        }
        if row.first == 0 {
            row.first = entry.timestamp;
        }
        row.last = entry.timestamp;
    }
}

#[cfg(test)]
mod forage_tests {
    use super::*;
    use crate::feed::Feed;
    use crate::game::Game;

    fn run(text: &str) -> ForageCollector {
        let mut c = ForageCollector::new();
        let mut feed = Feed::new("Tharn", Game::EverQuestLegends);
        {
            let mut subs: Vec<&mut dyn Observer> = vec![&mut c];
            feed.read(std::io::BufReader::new(text.as_bytes()), &mut subs)
                .unwrap();
        }
        c
    }

    #[test]
    fn a_forage_is_filed_under_the_zone_last_announced() {
        let c = run(
            "[Mon Aug 03 00:37:10 2026] You have entered Everfrost Peaks.\n\
                     [Mon Aug 03 00:37:12 2026] You have scrounged up Pod of Water.\n",
        );
        let z = &c.zones()["Everfrost Peaks"];
        assert_eq!(z.found.get("Pod of Water"), Some(&1));
        assert_eq!(z.attempts(), 1);
    }

    #[test]
    fn a_failure_is_an_attempt_and_not_an_item() {
        let c = run(
            "[Mon Aug 03 00:37:10 2026] You have entered Everfrost Peaks.\n\
                     [Mon Aug 03 00:37:12 2026] You have scrounged up Roots.\n\
                     [Mon Aug 03 00:37:20 2026] You fail to locate any food nearby.\n\
                     [Mon Aug 03 00:37:28 2026] You fail to locate any food nearby.\n",
        );
        let z = &c.zones()["Everfrost Peaks"];
        assert_eq!((z.hits(), z.empty, z.attempts()), (1, 2, 3));
        assert_eq!(z.found.len(), 1);
    }

    #[test]
    fn a_mode_suffix_is_the_same_place_to_forage_in() {
        let c = run(
            "[Mon Aug 03 00:37:10 2026] You have entered Everfrost Peaks.\n\
                     [Mon Aug 03 00:37:12 2026] You have scrounged up Roots.\n\
                     [Mon Aug 03 00:40:00 2026] You have entered Everfrost Peaks - Group.\n\
                     [Mon Aug 03 00:40:12 2026] You have scrounged up Roots.\n",
        );
        assert_eq!(c.zones().len(), 1);
        assert_eq!(c.zones()["Everfrost Peaks"].found["Roots"], 2);
    }

    #[test]
    fn an_attempt_before_any_zone_is_not_filed_under_a_guess() {
        let c = run("[Mon Aug 03 00:37:12 2026] You have scrounged up Berries.\n");
        assert!(c.zones().is_empty());
        assert_eq!(c.homeless, 1);
    }

    #[test]
    fn the_item_tally_pools_every_zone() {
        let c = run(
            "[Mon Aug 03 00:00:00 2026] You have entered Everfrost Peaks.\n\
                     [Mon Aug 03 00:00:02 2026] You have scrounged up Berries.\n\
                     [Mon Aug 03 01:00:00 2026] You have entered Misty Thicket.\n\
                     [Mon Aug 03 01:00:02 2026] You have scrounged up Berries.\n\
                     [Mon Aug 03 01:00:04 2026] You have scrounged up Fruit.\n",
        );
        let items = c.items();
        assert_eq!(items["Berries"], 2);
        assert_eq!(items["Fruit"], 1);
        assert_eq!(c.zones().len(), 2);
    }
}
