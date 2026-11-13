//! What a site can be built from, read off the lines the live view already reads.
//!
//! The daemon's [`Engine`](crate::engine::Engine) exists to answer "what is
//! happening now": encounters, buffs, alerts, the panel you are looking at.
//! Six facts that a *site* wants are not part of that picture at all — who
//! else was in the zone, what a creature considered at, which faction a kill
//! moved, how high a skill has climbed, where a marker was written down, and
//! where a corpse fell. Nothing on screen needs them, so the engine never
//! collected them, and until now they were read by a second program opening
//! the same file again.
//!
//! [`norrlog_core::collect`] already has all six, written as [`Observer`]s
//! for [`Feed`](norrlog_core::feed::Feed). This module is the adapter that
//! lets the engine drive them: the engine is not a feed — it owns its own
//! parser and encounter tracker and cannot be one — but it holds every field
//! a [`Context`] needs, so it can build one and dispatch.
//!
//! # Why reading clones
//!
//! Three of the six hold something pending. A faction burst is a run of
//! adjustment lines waiting for the death that explains it; a marker block is
//! open until the next tag closes it. [`Observer::finish`] settles that, and a
//! log being read from disk gets exactly one `finish`, at the end of the file.
//!
//! A live log has no end. If the flush called `finish` on the live collector,
//! a burst that happened to straddle the flush would be closed at the flush
//! and its remaining lines would open a *second* burst — one +5 faction hit
//! recorded as a +3 and a +2, which is not a smaller reading of the truth but
//! a different and wrong one.
//!
//! So [`Record::settle`] clones, finishes the clone and reports off that. The
//! live collectors keep their pending state and go on accumulating. Reading
//! the record does not disturb it.

use norrlog_core::collect::{
    ConsiderCollector, FactionCause, FactionCollector, ForageCollector, MarkerCollector,
    MarkerKind, PlayerCollector, SkillCollector, SpotCollector,
};
use norrlog_core::feed::{Context, Observer};
use norrlog_core::game::Game;
use norrlog_core::LogEntry;
use serde::Serialize;

/// The game tag the record's rows are keyed and reported on.
///
/// Not [`Game::slug`], which is `everquest-legends`. eqldb has said `eql`
/// since before the slug existed and every Legends row already filed says
/// `eql`, so Legends keeps it. The record was Legends-only until live
/// EverQuest was folded in, and every row was tagged `eql` whatever game it
/// came from; live EverQuest is `eq` now, and a log whose game is unknown is
/// `unknown`, never one of the two. See `Store::retag_live_record`.
pub fn tag(game: Game) -> &'static str {
    match game {
        Game::EverQuestLegends => "eql",
        Game::EverQuest => "eq",
        Game::Unknown => "unknown",
    }
}

/// How far apart two `/loc` readings may be and still be one spawn point.
///
/// Matches the value the rebuild has always passed to
/// [`MarkerCollector::spawn_points`].
pub const SPAWN_RADIUS: f64 = 15.0;

/// The `note` on each report.
///
/// Constants rather than literals at the point of use because two callers
/// build the same report — the engine, from one log in memory, and the store,
/// merged across every log on file — and a note that drifted between them
/// would make a diff of the two report a difference that is not there.
pub const PLAYERS_NOTE: &str = "Player characters named by /who, read straight from the log.";
pub const CONSIDER_NOTE: &str = "Levels and rare markers read from consider lines.";
pub const FORAGE_NOTE: &str =
    "What forage yielded, per zone, with the attempts that yielded nothing. A zone \
     with no items is a zone that was foraged in and gave up none.";
pub const FACTION_NOTE: &str = "Which creatures and hand-ins move which factions.";
pub const SPOTS_NOTE: &str = "Creature positions from a /loc taken over its corpse. \
                              Many rows per creature is the shape: a creature with \
                              several spawn points produces several clusters, and \
                              nothing here averages them. `gap` is the seconds \
                              between the death and the reading, which is the whole \
                              reliability of the row.";
pub const CARTO_NOTE: &str = "Map markers written down at the spot, from the log.";
pub const SKILLS_NOTE: &str = "The highest value seen for each skill, across every log read.";

/// The word a marker's kind is stored and published under.
///
/// Kept as a name here, and the table itself lives on the enum — three crates
/// had grown their own copy of it. See [`MarkerKind::word`].
pub fn marker_kind(kind: MarkerKind) -> &'static str {
    kind.word()
}

/// The inverse, for a row read back out of the store.
///
/// Anything unrecognised reads as a place, which is what the collector calls
/// a marker it could not classify — so a row written by a future version with
/// a kind this one does not know still appears, rather than vanishing.
pub fn kind_of(word: &str) -> &'static str {
    match word {
        "npc" => "npc",
        "zone" => "zone",
        "craft" => "craft",
        "quest" => "quest",
        _ => "place",
    }
}

/// The seven collectors the engine drives alongside its own trackers.
pub struct Record {
    consider: ConsiderCollector,
    forage: ForageCollector,
    faction: FactionCollector,
    players: PlayerCollector,
    skills: SkillCollector,
    markers: MarkerCollector,
    spots: SpotCollector,
}

impl Default for Record {
    fn default() -> Self {
        Self::new()
    }
}

impl Record {
    pub fn new() -> Record {
        Record {
            consider: ConsiderCollector::new(),
            forage: ForageCollector::new(),
            faction: FactionCollector::new(),
            players: PlayerCollector::new(),
            skills: SkillCollector::new(),
            markers: MarkerCollector::new(),
            spots: SpotCollector::new(),
        }
    }

    /// One parsed line, to every collector.
    ///
    /// The caller owns the [`Context`] because only the caller knows the zone
    /// and the open fight; see the module docs.
    pub fn on_entry(&mut self, entry: &LogEntry, cx: &Context<'_>) {
        for o in self.observers() {
            o.on_entry(entry, cx);
        }
    }

    /// A fight closed.
    pub fn on_encounter_end(&mut self, enc: &norrlog_core::encounter::Encounter, cx: &Context<'_>) {
        for o in self.observers() {
            o.on_encounter_end(enc, cx);
        }
    }

    fn observers(&mut self) -> [&mut dyn Observer; 7] {
        [
            &mut self.consider,
            &mut self.forage,
            &mut self.faction,
            &mut self.players,
            &mut self.skills,
            &mut self.markers,
            &mut self.spots,
        ]
    }

    /// The record as it stands, with everything pending settled.
    ///
    /// Clones first. See the module docs for why reading must not disturb the
    /// live collectors.
    pub fn settle(&self, player: &str, game: Game, zone: Option<&str>) -> Settled {
        let mut s = Settled {
            game,
            consider: self.consider.clone(),
            forage: self.forage.clone(),
            faction: self.faction.clone(),
            players: self.players.clone(),
            skills: self.skills.clone(),
            markers: self.markers.clone(),
            spots: self.spots.clone(),
        };
        let cx = Context {
            player,
            game,
            zone,
            gap: None,
            encounter: None,
        };
        for o in [
            &mut s.consider as &mut dyn Observer,
            &mut s.forage,
            &mut s.faction,
            &mut s.players,
            &mut s.skills,
            &mut s.markers,
            &mut s.spots,
        ] {
            o.finish(&cx);
        }
        s
    }
}

/// A settled copy of the record, safe to report off.
pub struct Settled {
    /// The game the log was of, which every row is tagged with.
    pub game: Game,
    pub consider: ConsiderCollector,
    pub forage: ForageCollector,
    pub faction: FactionCollector,
    pub players: PlayerCollector,
    pub skills: SkillCollector,
    pub markers: MarkerCollector,
    pub spots: SpotCollector,
}

impl Settled {
    /// Every report, under the names the rebuild has always written.
    ///
    /// `source` is the tag a row is filed under — the log's stem, as it was
    /// when a script fetched these off this daemon's API.
    pub fn reports(&self, source: &str) -> Reports {
        Reports {
            players: self.players(source),
            consider: self.consider(source),
            forage: self.forage(source),
            faction: self.faction(source),
            spots: self.spots(source),
            carto: self.carto(source),
            skills: self.skills(),
        }
    }

    pub fn players(&self, source: &str) -> PlayersReport {
        PlayersReport {
            note: PLAYERS_NOTE,
            source: source.to_string(),
            game: tag(self.game),
            players: self
                .players
                .seen()
                .values()
                .map(|p| PlayerRow {
                    name: p.name.clone(),
                    level: p.level,
                    classes: p.classes.clone(),
                    race: p.race.clone(),
                })
                .collect(),
        }
    }

    pub fn consider(&self, source: &str) -> ConsiderReport {
        ConsiderReport {
            note: CONSIDER_NOTE,
            source: source.to_string(),
            considers: self
                .consider
                .seen()
                .values()
                .map(|r| ConsiderRow {
                    creature: r.name.clone(),
                    game: tag(self.game),
                    levels: r
                        .levels
                        .iter()
                        .map(|(level, times)| LevelSeen {
                            level: *level,
                            times: *times,
                        })
                        .collect(),
                    rare: r.rare,
                    raid: r.raid,
                    considers: r.considers(),
                })
                .collect(),
        }
    }

    /// What forage has yielded, per zone.
    ///
    /// Zones with no success are kept. "Nobody has ever foraged anything in
    /// Nagafen's Lair, in four attempts" is a statement about the zone, and
    /// dropping the empty rows would turn it into silence — the same reason
    /// the failure count is collected at all.
    pub fn forage(&self, source: &str) -> ForageReport {
        ForageReport {
            note: FORAGE_NOTE.into(),
            source: source.to_string(),
            zones: self
                .forage
                .zones()
                .iter()
                .map(|(zone, z)| ForageZoneRow {
                    zone: zone.clone(),
                    game: tag(self.game).into(),
                    attempts: z.attempts(),
                    hits: z.hits(),
                    empty: z.empty,
                    first_seen: z.first,
                    last_seen: z.last,
                    items: z
                        .found
                        .iter()
                        .map(|(item, times)| ForagedItem {
                            item: item.clone(),
                            times: *times,
                        })
                        .collect(),
                })
                .collect(),
            unplaced: self.forage.homeless,
        }
    }

    pub fn faction(&self, source: &str) -> FactionReport {
        let f = &self.faction;
        let (mut hits, mut turnins) = (Vec::new(), Vec::new());
        for ((cause, faction_name), moves) in f.moves() {
            let adjustments: Vec<_> = moves
                .amounts
                .iter()
                .map(|(amount, times)| Adjustment {
                    amount: *amount,
                    times: *times,
                })
                .collect();
            match cause {
                FactionCause::Kill { creature, zone } => hits.push(FactionHit {
                    creature: f.creature_display(creature).unwrap_or(creature).to_string(),
                    zone: f.zone_display(zone).unwrap_or(zone).to_string(),
                    faction: faction_name.clone(),
                    adjustments,
                    kills: moves.times(),
                    game: tag(self.game),
                }),
                FactionCause::TurnIn { npc, item } => turnins.push(FactionTurnIn {
                    npc: npc.clone(),
                    item: item.clone(),
                    faction: faction_name.clone(),
                    adjustments,
                    offered: f
                        .offered
                        .get(&(npc.clone(), item.clone()))
                        .copied()
                        .unwrap_or(0),
                    handins: f
                        .handins
                        .get(&(npc.clone(), item.clone()))
                        .copied()
                        .unwrap_or(0),
                    game: tag(self.game),
                }),
            }
        }
        FactionReport {
            note: FACTION_NOTE,
            source: source.to_string(),
            bursts_seen: f.bursts,
            bursts_attributed: f.to_kill,
            bursts_to_turnins: f.to_turnin,
            hits,
            turnins,
        }
    }

    pub fn spots(&self, source: &str) -> SpotsReport {
        SpotsReport {
            note: SPOTS_NOTE,
            source: source.to_string(),
            spots: self
                .spots
                .spots()
                .iter()
                .map(|s| SpotRow {
                    name: s.name.clone(),
                    zone: s.zone.clone(),
                    zone_short: s.zone_short.clone(),
                    x: s.x,
                    y: s.y,
                    z: s.z,
                    at: s.at,
                    gap: s.gap,
                    game: tag(self.game),
                })
                .collect(),
        }
    }

    pub fn carto(&self, source: &str) -> CartoReport {
        CartoReport {
            note: CARTO_NOTE,
            source: source.to_string(),
            zones: self
                .markers
                .zones()
                .iter()
                .map(|(zone, short)| ZoneRow {
                    zone: zone.clone(),
                    zone_short: short.clone(),
                    game: tag(self.game),
                })
                .collect(),
            markers: self
                .markers
                .spawn_points(SPAWN_RADIUS)
                .into_iter()
                .map(|m| MarkerRow {
                    name: m.name.clone(),
                    kind: marker_kind(m.kind),
                    role: m.role.clone(),
                    zone: m.zone.clone(),
                    zone_short: m.zone_short.clone(),
                    x: m.x,
                    y: m.y,
                    z: m.z,
                    level: m.level,
                    game: tag(self.game),
                })
                .collect(),
        }
    }

    /// A ceiling is the highest anybody has reached, so this carries no
    /// `source`: it is one answer across every log, and the rebuild has
    /// always written it as one file.
    pub fn skills(&self) -> SkillsReport {
        SkillsReport {
            note: SKILLS_NOTE,
            source: "logs",
            game: tag(self.game),
            skills: self
                .skills
                .best()
                .iter()
                .map(|(skill, at_least)| SkillRow {
                    skill: skill.clone(),
                    at_least: *at_least,
                    game: tag(self.game),
                })
                .collect(),
        }
    }
}

/// Every report the record produces, in one object.
///
/// The field names are the file names the rebuild wrote — `carto.<tag>.json`,
/// `consider.<tag>.json` and so on — so a diff between this and a rebuild's
/// output is a comparison of like-named things.
#[derive(Debug, Clone, Serialize)]
pub struct Reports {
    pub players: PlayersReport,
    pub consider: ConsiderReport,
    pub forage: ForageReport,
    pub faction: FactionReport,
    pub spots: SpotsReport,
    pub carto: CartoReport,
    pub skills: SkillsReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayersReport {
    pub note: &'static str,
    pub source: String,
    pub game: &'static str,
    pub players: Vec<PlayerRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayerRow {
    pub name: String,
    pub level: u32,
    pub classes: Vec<String>,
    pub race: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsiderReport {
    pub note: &'static str,
    pub source: String,
    pub considers: Vec<ConsiderRow>,
}

pub use norrlog_api::{ForageReport, ForageZoneRow, ForagedItem};

#[derive(Debug, Clone, Serialize)]
pub struct ConsiderRow {
    pub creature: String,
    pub game: &'static str,
    pub levels: Vec<LevelSeen>,
    pub rare: bool,
    pub raid: bool,
    pub considers: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LevelSeen {
    pub level: u32,
    pub times: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FactionReport {
    pub note: &'static str,
    pub source: String,
    pub bursts_seen: u64,
    pub bursts_attributed: u64,
    pub bursts_to_turnins: u64,
    pub hits: Vec<FactionHit>,
    pub turnins: Vec<FactionTurnIn>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Adjustment {
    pub amount: i64,
    pub times: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FactionHit {
    pub creature: String,
    pub zone: String,
    pub faction: String,
    pub adjustments: Vec<Adjustment>,
    pub kills: u64,
    pub game: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct FactionTurnIn {
    pub npc: String,
    pub item: String,
    pub faction: String,
    pub adjustments: Vec<Adjustment>,
    pub offered: u64,
    pub handins: u64,
    pub game: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpotsReport {
    pub note: &'static str,
    pub source: String,
    pub spots: Vec<SpotRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpotRow {
    pub name: String,
    pub zone: String,
    pub zone_short: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub at: i64,
    pub gap: i64,
    pub game: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct CartoReport {
    pub note: &'static str,
    pub source: String,
    pub zones: Vec<ZoneRow>,
    pub markers: Vec<MarkerRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ZoneRow {
    pub zone: String,
    pub zone_short: String,
    pub game: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct MarkerRow {
    pub name: String,
    pub kind: &'static str,
    pub role: String,
    pub zone: String,
    pub zone_short: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub level: Option<u32>,
    pub game: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillsReport {
    pub note: &'static str,
    pub source: &'static str,
    pub game: &'static str,
    pub skills: Vec<SkillRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillRow {
    pub skill: String,
    pub at_least: u32,
    pub game: &'static str,
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use norrlog_core::game::Game;
    use norrlog_core::spell::SpellDb;

    /// Enough of a log to move every collector at once: a zone, a `/who` that
    /// names both the roster and the zone's shortname, a consider, a
    /// cartographer block, a skill and a kill that moves faction.
    const LOG: &str = "\
[Tue Jul 28 15:03:10 2026] You have entered Rivervale.
[Tue Jul 28 15:03:11 2026] You say, '--EQLCartographer--Ranger Guildmaster'
[Tue Jul 28 15:03:12 2026] You say, 'Hail, Megosh Thistlethorn'
[Tue Jul 28 15:03:13 2026] Your Location is -27.08, -440.14, -11.75
[Tue Jul 28 15:03:14 2026] Megosh Thistlethorn glowers at you dubiously -- what would you like your tombstone to say? (Lvl: 70)
[Tue Jul 28 15:03:15 2026] [50 WAR/DRU/BRD] Tharn (Ogre) ZONE: Rivervale (rivervale)
[Tue Jul 28 15:03:16 2026] You have become better at Baking! (42)
";

    fn engine_over(text: &str) -> Engine {
        let mut e = Engine::with_game(
            "Tharn".to_string(),
            SpellDb::empty(),
            Game::EverQuestLegends,
        );
        for line in text.lines() {
            e.push_line(line);
        }
        e
    }

    /// The dispatch reaches every collector, and the reports come back under
    /// the tag they were asked for.
    #[test]
    fn the_engine_drives_the_record() {
        let e = engine_over(LOG);
        let r = e.record_reports("eqlog_Tharn_rivervale");

        assert_eq!(r.players.source, "eqlog_Tharn_rivervale");
        assert!(
            r.players.players.iter().any(|p| p.name == "Tharn"),
            "the /who line names the roster: {:?}",
            r.players.players
        );
        assert!(
            r.consider
                .considers
                .iter()
                .any(|c| c.creature.contains("Megosh")),
            "the consider line was read: {:?}",
            r.consider.considers
        );
        assert_eq!(
            r.skills
                .skills
                .iter()
                .find(|s| s.skill == "Baking")
                .map(|s| s.at_least),
            Some(42),
            "the skill line was read: {:?}",
            r.skills.skills
        );
        assert_eq!(
            r.carto
                .zones
                .iter()
                .map(|z| (z.zone.as_str(), z.zone_short.as_str()))
                .collect::<Vec<_>>(),
            vec![("Rivervale", "rivervale")],
            "/who states the zone-name to shortname pairing"
        );
        assert_eq!(
            r.carto
                .markers
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Megosh Thistlethorn"],
            "the cartographer block became a marker"
        );
    }

    /// A marker block is open until something closes it, and a live log never
    /// ends. Reading the record settles a *copy*, so the live collector keeps
    /// its pending state — and reading twice does not produce the marker
    /// twice.
    #[test]
    fn reading_the_record_does_not_disturb_it() {
        // Everything but the trailing /who, so the marker block is still open.
        let open = LOG.lines().take(5).collect::<Vec<_>>().join("\n");
        let mut e = engine_over(&open);

        let first = e.record_reports("t");
        let second = e.record_reports("t");
        assert_eq!(
            first.carto.markers.len(),
            second.carto.markers.len(),
            "settling the record twice must not double what was pending"
        );

        // And the live collector is still open, so the zone line that follows
        // is still read into the same block rather than a new one.
        e.push_line(
            "[Tue Jul 28 15:03:15 2026] [50 WAR/DRU/BRD] Tharn (Ogre) ZONE: Rivervale (rivervale)",
        );
        let after = e.record_reports("t");
        assert_eq!(
            after.carto.markers.len(),
            1,
            "one block is one marker, however often the record was read"
        );
        assert_eq!(after.carto.markers[0].zone_short, "rivervale");
    }

    /// The zone handed to the collectors is the client's own string, not the
    /// parsed place name. A marker filed under one spelling by this reader and
    /// another by the rebuild is two markers.
    #[test]
    fn the_collectors_see_the_zone_as_announced() {
        let e = engine_over(LOG);
        let r = e.record_reports("t");
        assert_eq!(r.carto.markers[0].zone, "Rivervale");
    }
}
