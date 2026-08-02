//! The analysis engine: owns every stateful piece of the pipeline and
//! publishes an immutable [`Snapshot`] for the API and overlay to read.
//!
//! Keeping all mutable state behind one struct means the HTTP layer never
//! touches a parser or tracker directly; it reads a cheap clone of the latest
//! snapshot. That keeps request handling lock-free apart from a brief read.

use eqlparser_core::class::{ClassDetector, ClassScore};
use eqlparser_core::encounter::{Encounter, EncounterTracker};
use eqlparser_core::experience::{AaPurchase, ExpSource, ExpTracker, LevelRecord};
use eqlparser_core::aa::{AaCatalog, AaReport};
use eqlparser_core::lockout::{BuildRecord, LockoutReport, LockoutTracker};
use eqlparser_core::skyquest::{SkyQuestDef, SkyReport, SkyTracker};
use eqlparser_core::task::{TaskDef, TaskReport, TaskTracker};
use eqlparser_core::tradeskill::{TradeskillReport, TradeskillTracker};
use eqlparser_core::spell::{BuffMark, SpellDb, SpellResolver};
use eqlparser_core::{Event, LogEntry, Parser, SpellbookChangeKind};
use serde::Serialize;
use std::path::Path;

/// One row of a live damage meter.
#[derive(Debug, Clone, Serialize)]
pub struct MeterRow {
    pub name: String,
    pub key: String,
    pub damage: u64,
    pub dps: f64,
    /// Fraction of the encounter's total damage, 0.0–1.0. Drives bar widths.
    pub share: f64,
    /// Healing done, and its share of all healing in the fight.
    pub healing: u64,
    pub hps: f64,
    pub heal_share: f64,
    /// Damage absorbed, and its share of all damage absorbed.
    pub damage_taken: u64,
    pub taken_share: f64,
    pub healing_received: u64,
    /// Combined crit rate across all mechanisms. Diluted by damage shields,
    /// which never crit — prefer `crit_by_kind` when the mechanism matters.
    pub crit_rate: f64,
    /// Per-mechanism crit: (kind, hits, crits, rate).
    pub crit_by_kind: Vec<(String, u64, u64, f64)>,
    pub accuracy: f64,
    /// True when this actor is the logging character.
    pub is_you: bool,
    /// True when the logging character has damaged this actor, i.e. it is an
    /// opponent rather than an ally.
    pub is_enemy: bool,
    /// True when this actor was charmed by you during the fight. Takes
    /// precedence over `is_enemy` in the UI: a charmed creature is an enemy by
    /// origin but fights for you, and its damage counts toward your total.
    pub is_pet: bool,
    /// Damage this actor dealt while charmed by you.
    pub pet_damage: u64,
}

/// One of your deaths, with its context.
#[derive(Debug, Clone, Serialize)]
pub struct DeathReportRow {
    pub at: i64,
    pub killer: Option<String>,
    pub zone: String,
    /// The fight it happened in, so a death traces back to an encounter.
    pub encounter_id: u64,
    pub encounter_title: String,
    pub damage_in_window: u64,
    pub blows: Vec<eqlparser_core::encounter::IncomingHit>,
}

/// Everything the deaths view shows.
#[derive(Debug, Clone, Serialize)]
pub struct DeathReport {
    pub deaths: Vec<DeathReportRow>,
    /// Who has killed you most often.
    pub by_killer: Vec<(String, u32)>,
    /// Which zones you die in.
    pub by_zone: Vec<(String, u32)>,
    pub total: usize,
}

/// A charm that failed to take, for the resist overlay.
#[derive(Debug, Clone, Serialize)]
pub struct CharmFailure {
    pub at: i64,
    pub target: String,
    /// "resisted", "immune" or "unaffected".
    pub kind: &'static str,
    pub spell: String,
}

/// What the charm overlay shows.
#[derive(Debug, Clone, Serialize)]
pub struct CharmReport {
    /// Most recent failures first.
    pub failures: Vec<CharmFailure>,
    /// Creatures charmed right now.
    pub charmed: Vec<String>,
    /// Charm uptime for the fight in progress, 0.0–1.0.
    pub uptime: f64,
    pub breaks: u32,
    /// Failures within the last minute, for an at-a-glance "it is not landing".
    pub recent_failures: usize,
}

/// A compact view of one fight, for lists and the overlay.
#[derive(Debug, Clone, Serialize)]
pub struct EncounterSummary {
    pub id: u64,
    pub title: String,
    pub zone: Option<String>,
    pub start: i64,
    pub end: i64,
    pub duration: i64,
    /// Every point dealt by both sides. Includes enemies hitting you, so it is
    /// not the figure a damage meter usually wants.
    pub total_damage: u64,
    /// Damage your side put into the enemy — the meaningful headline number.
    pub damage_to_enemies: u64,
    pub your_damage: u64,
    pub your_dps: f64,
    pub your_hps: f64,
    pub your_healing: u64,
    /// Damage you absorbed. Surfaced alongside output so a fight that went
    /// badly is visible, not just one that went well.
    pub your_damage_taken: u64,
    pub your_deaths: u64,
    /// Share of the fight your charms held, 0.0–1.0.
    pub charm_uptime: f64,
    pub charm_breaks: u32,
    pub kills: usize,
    pub loot_count: usize,
    pub coin_copper: u64,
    /// Populated for the live encounter and on demand; empty in long lists.
    pub meter: Vec<MeterRow>,
}

/// An active buff with its remaining time.
#[derive(Debug, Clone, Serialize)]
pub struct BuffInfo {
    pub spell_id: u32,
    pub name: String,
    pub remaining: Option<i64>,
    pub total: Option<i64>,
    /// How sure we are this is the right spell; the UI dims low values because
    /// landing text alone is ambiguous.
    pub confidence: f32,
    /// Lasts until you zone; shown as permanent rather than counted down.
    pub permanent: bool,
    /// Self-sustaining aura (a passively recast bard song), rather than a
    /// timed buff. Reported separately so pulsing songs do not bury real timers.
    pub is_aura: bool,
    /// A user-declared classification: "aura", "permanent", or absent.
    pub mark: Option<String>,
    /// What the effect does — "heal", "buff", "cure", "damage", ... — so the
    /// UI can colour by function rather than by guessing from the name.
    pub category: String,
    /// Times this effect has re-landed. High counts are what identify an aura.
    pub refreshes: u32,
    /// False for detrimental effects, which are listed as debuffs.
    pub beneficial: bool,
}

/// Detected class with supporting evidence.
#[derive(Debug, Clone, Serialize)]
pub struct ClassInfo {
    pub name: String,
    pub abbrev: String,
    pub score: f32,
    pub share: f32,
    pub reasons: Vec<String>,
}

/// Career totals across every parsed encounter.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub encounters: usize,
    /// Every point dealt by both sides. Includes enemies hitting you, so it is
    /// not the figure a damage meter usually wants.
    pub total_damage: u64,
    /// Damage your side put into the enemy — the meaningful headline number.
    pub damage_to_enemies: u64,
    pub your_damage: u64,
    pub your_healing: u64,
    pub your_damage_taken: u64,
    pub kills: usize,
    pub deaths: u64,
    pub items_looted: usize,
    /// Coin taken directly from corpses.
    pub coin_copper: u64,
    /// Coin from items auto-sold on pickup — a large share of real income.
    pub coin_from_sales_copper: u64,
    /// Corpse coin plus sales.
    pub coin_total_copper: u64,
}

/// A looted item aggregated across the log.
#[derive(Debug, Clone, Serialize)]
pub struct LootTally {
    pub item: String,
    pub count: u64,
    pub sources: Vec<String>,
    pub last_seen: i64,
    pub sold_total_copper: u64,
    /// How many of this item met each fate. The same item is routinely both
    /// kept and auto-sold across a log, so this is a breakdown rather than a
    /// single verdict.
    pub kept: u64,
    pub sold: u64,
    pub stored: u64,
    pub transmuted: u64,
    /// Containers it was auto-routed into.
    pub stored_in: Vec<String>,
    /// Items it was combined into on pickup.
    ///
    /// Kept apart from `stored_in` because the two are different facts that
    /// happen to share a field in the log line: "Dragon Hoard" is where an
    /// item sits, "Whitened Treant Fists +2" is what it became. Two items in
    /// this log met both fates, and merging them read as nonsense.
    pub transmuted_into: Vec<String>,
}

/// Level-progression analysis.
///
/// All experience figures are "percent of a level", because that is the only
/// unit the client reports. It is comparable across levels as *progress*, but
/// deliberately not as raw experience: 1% at level 50 is far more than 1% at
/// level 10.
#[derive(Debug, Clone, Serialize)]
pub struct ExperienceReport {
    pub current_level: Option<u32>,
    pub current_percent: f64,
    pub current_kills: u32,
    pub current_seconds: i64,
    pub rate_per_hour: f64,
    pub seconds_to_next_level: Option<i64>,
    pub average_seconds_per_level: Option<i64>,
    pub average_kills_per_level: Option<f64>,
    pub levels: Vec<LevelRecord>,
    pub best_zones: Vec<ExpSource>,
    pub best_mobs: Vec<ExpSource>,
    /// Alternate advancement ("ability points" in this client).
    pub aa_earned: u32,
    pub aa_unspent: u32,
    /// Points spent on purchases visible in the log. A floor on the
    /// character's lifetime spend: ranks bought before logging began cannot
    /// be seen, so this reads low by exactly that amount.
    pub aa_spent: u32,
    /// Recent rate, over `aa_window_seconds`.
    pub aa_per_hour: f64,
    pub aa_window_seconds: i64,
    /// Rate across the whole period in which points were earned.
    pub aa_lifetime_per_hour: f64,
    pub aa_zones: Vec<ExpSource>,
    pub aa_purchases: Vec<AaPurchase>,
    /// Experience potions. These are charge-based rather than timed: each
    /// grants five doubled awards, and charges stack.
    pub aa_potions_used: u32,
    pub aa_potion_charges: u32,
    pub aa_potion_active: bool,
    pub aa_gains_doubled: u32,
    pub aa_gains_normal: u32,
    /// Share of awards that were doubled, 0.0–1.0.
    pub aa_doubled_share: f64,
    /// Average points per award: 1.0 with no potion, 2.0 with full coverage.
    pub aa_effective_multiplier: f64,
    /// Doubled awards from a potion that predated the log.
    pub aa_doubled_unaccounted: u32,
}

/// One row of an incoming-damage breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct IncomingRow {
    pub name: String,
    pub total: u64,
    pub hits: u64,
    pub max: u64,
    pub avg: f64,
    pub share: f64,
}

/// What has been hitting you, and how.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IncomingReport {
    pub total: u64,
    /// Melee, spell, dot, damage shield.
    pub by_kind: Vec<IncomingRow>,
    /// Which attack or spell.
    pub by_ability: Vec<IncomingRow>,
    /// Which creature.
    pub by_source: Vec<IncomingRow>,
    pub hits_taken: u64,
    pub attacks_missed: u64,
    pub attacks_avoided: u64,
    /// Share of attacks against you that failed to land.
    pub avoidance_rate: f64,
    pub deaths: u64,
    /// Largest single hit you have taken, and what caused it.
    pub biggest_hit: u64,
    pub biggest_hit_from: Option<String>,
    /// Encounters the figures cover; `None` means every encounter.
    pub encounter_id: Option<u64>,
}

/// Live experience summary carried in every snapshot.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ExpSummary {
    pub percent: f64,
    pub kills: u32,
    pub rate_per_hour: f64,
    pub seconds_to_next_level: Option<i64>,
    pub aa_earned: u32,
    pub aa_unspent: u32,
    pub aa_per_hour: f64,
    /// Doubled awards remaining from experience potions.
    pub aa_potion_charges: u32,
    pub aa_potion_active: bool,
}

/// Everything the UI needs, as of the last processed line.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub player: String,
    pub level: u32,
    pub zone: Option<String>,
    pub classes: Vec<ClassInfo>,
    /// The fight in progress, if any.
    pub current: Option<EncounterSummary>,
    /// Completed fights, newest first.
    pub recent: Vec<EncounterSummary>,
    pub buffs: Vec<BuffInfo>,
    /// Passively recast songs. Bards run up to five at once.
    pub auras: Vec<BuffInfo>,
    /// Detrimental effects currently on you.
    pub debuffs: Vec<BuffInfo>,
    /// Progress through the current level, and the rate it is moving at.
    pub exp: ExpSummary,
    pub totals: Totals,
    /// Log timestamp of the most recent line processed.
    pub log_time: i64,
    /// True once the initial backlog has been read and we are following live.
    pub live: bool,
}

/// Window for the "AA per hour" figure.
///
/// Wider than the experience window: points arrive as whole units and far less
/// often, so a short window reads zero most of the time.
/// How long a charm failure stays on the overlay.
const CHARM_FAILURE_WINDOW: i64 = 300;
/// Hard cap on retained failures, so a pathological fight cannot grow the list
/// without bound.
const CHARM_FAILURE_MAX: usize = 50;

const AA_RATE_WINDOW: i64 = 2 * 3600;

/// Wall-clock seconds since the Unix epoch.
fn system_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Owns the parsing pipeline and derives snapshots from it.
pub struct Engine {
    parser: Parser,
    tracker: EncounterTracker,
    resolver: SpellResolver,
    classes: ClassDetector,
    exp: ExpTracker,
    lockouts: LockoutTracker,
    sky: SkyTracker,
    aa: AaCatalog,
    /// Recent charm failures, for the resist overlay.
    charm_failures: Vec<CharmFailure>,
    /// The last charm we tried to land, so nameless failure lines can be
    /// attributed to something.
    last_charm_attempt: Option<(String, String)>,
    /// The description line awaiting its cost, so a rank keeps its text.
    aa_pending_desc: String,
    trades: TradeskillTracker,
    tasks: TaskTracker,
    encounters: Vec<Encounter>,
    /// Coin taken from corpses, and coin from items auto-sold on pickup.
    ///
    /// Tracked here rather than only inside encounters for the same reason as
    /// loot: coin can arrive after a fight has closed. Sales are counted
    /// separately from corpse coin because they are a distinct income stream —
    /// and omitting them understated total income by more than half.
    coin_from_corpses: u64,
    coin_from_sales: u64,
    /// Every loot event, recorded independently of encounters.
    ///
    /// Loot must not depend on a fight being open. Corpses are looted *after*
    /// combat ends, and on a boss that can be well past the encounter's idle
    /// timeout — a real Innoruuk kill dropped an item 22 seconds after the
    /// fight closed, and it was silently discarded. Encounters still keep their
    /// own copy for per-fight display; this list is the authoritative tally.
    all_loot: Vec<eqlparser_core::encounter::LootRecord>,
    player: String,
    last_timestamp: i64,
    /// Difference between log time and system time, learned once we are
    /// following the file live.
    ///
    /// EverQuest writes local time with no zone, which we parse as if it were
    /// UTC. Rather than guess the machine's offset we measure it: when tailing
    /// reaches the end of the file, the newest line is approximately "now".
    clock_offset: Option<i64>,
    live: bool,
}

impl Engine {
    pub fn new(player: String, spell_db: SpellDb) -> Engine {
        let mut e = Engine {
            parser: Parser::new(&player),
            tracker: EncounterTracker::new(&player),
            resolver: SpellResolver::new(spell_db, 1),
            classes: ClassDetector::new(),
            exp: ExpTracker::new(),
            lockouts: LockoutTracker::new(),
            sky: SkyTracker::new(),
            aa: AaCatalog::new(),
            charm_failures: Vec::new(),
            last_charm_attempt: None,
            aa_pending_desc: String::new(),
            trades: TradeskillTracker::new(),
            tasks: TaskTracker::new(),
            encounters: Vec::new(),
            coin_from_corpses: 0,
            coin_from_sales: 0,
            all_loot: Vec::new(),
            player,
            last_timestamp: 0,
            clock_offset: None,
            live: false,
        };
        e.load_marks();
        e.load_task_defs();
        e.load_sky_quests();
        e
    }

    /// Load the spell database from the game directory, if present.
    pub fn load_spell_db(game_dir: Option<&Path>) -> SpellDb {
        let Some(dir) = game_dir else {
            return SpellDb::empty();
        };
        match SpellDb::load(&dir.join("spells_us.txt"), &dir.join("spells_us_str.txt")) {
            Ok(db) => {
                eprintln!("[eqlparserd] spell database loaded: {} spells", db.len());
                db
            }
            Err(e) => {
                eprintln!("[eqlparserd] no spell database ({e}); buff timers disabled");
                SpellDb::empty()
            }
        }
    }

    /// Mark the backlog as consumed; subsequent lines are live.
    pub fn set_live(&mut self, live: bool) {
        self.live = live;
        if live {
            self.calibrate_clock();
        }
    }

    /// Learn the offset between log time and system time.
    ///
    /// Re-run on every line while following live, not just once. Calibrating
    /// only at startup leaves the offset stale by however long the app sat
    /// running before play began: `now()` would then resolve to earlier than
    /// the newest line, get clamped to it, and stop advancing between lines —
    /// freezing every buff timer during exactly the lulls you would check them
    /// in. Recalibrating makes `now()` equal "newest line plus real time
    /// elapsed since it arrived", which is self-correcting.
    fn calibrate_clock(&mut self) {
        if self.last_timestamp == 0 {
            return;
        }
        self.clock_offset = Some(self.last_timestamp - system_now());
    }

    /// Current time in the log's clock domain.
    ///
    /// While following live this advances with the wall clock, so buff timers
    /// keep counting down during a quiet stretch instead of freezing on the
    /// last line seen.
    pub fn now(&self) -> i64 {
        match (self.live, self.clock_offset) {
            (true, Some(off)) => (system_now() + off).max(self.last_timestamp),
            _ => self.last_timestamp,
        }
    }

    /// Feed one raw log line.
    pub fn push_line(&mut self, line: &str) {
        let Some(entry) = self.parser.parse_line(line) else {
            return;
        };
        self.push_entry(entry);
    }

    fn push_entry(&mut self, entry: LogEntry) {
        self.last_timestamp = entry.timestamp;
        // Keep the log/system clock relationship current while following live;
        // see `calibrate_clock` for why once-at-startup is not enough.
        if self.live {
            self.calibrate_clock();
        }

        // A `/alt list` dump has no end marker; it simply stops and ordinary
        // lines resume. Any non-dump line therefore closes the run so its
        // ranks can be merged.
        if !matches!(
            entry.event,
            Event::AbilityListEntry { .. }
                | Event::AbilityListDescription { .. }
                | Event::AbilityListCost { .. }
        ) {
            self.aa.on_other();
        }

        match &entry.event {
            Event::CastStart { source, spell, .. } if source.is_you() => {
                self.resolver.note_cast(entry.timestamp, spell);
                self.observe_class_spell(entry.timestamp, spell);
                // Remember the charm being sung so a nameless failure line can
                // be attributed. The target is unknown at cast time, so the
                // creature currently being fought stands in for it.
                if self.spell_is_charm(spell) {
                    let target = self
                        .tracker
                        .current()
                        .and_then(|e| {
                            e.actors
                                .values()
                                .filter(|a| a.damaged_by_you && !a.charmed_by_you)
                                .max_by_key(|a| a.damage_taken)
                                .map(|a| a.name.clone())
                        })
                        .unwrap_or_else(|| "your target".to_string());
                    self.last_charm_attempt = Some((target, spell.clone()));
                }
                // Drinking an experience potion is logged as casting it.
                if eqlparser_core::experience::is_aa_potion(spell) {
                    self.exp.on_aa_potion(entry.timestamp);
                }
            }
            Event::SpellbookChange { spell, change }
                if !matches!(change, SpellbookChangeKind::Forgotten) =>
            {
                self.resolver.note_known_spell(spell);
                self.observe_class_spell(entry.timestamp, spell);
            }
            Event::SkillUp { skill, value } => {
                self.classes.observe_skill(entry.timestamp, skill);
                self.trades.on_skill_up(entry.timestamp, skill, *value);
            }
            Event::Craft { item, success } => {
                self.trades.on_craft(entry.timestamp, item, *success)
            }
            Event::ComponentConsumed { item, quantity, remaining } => {
                self.trades
                    .on_component(entry.timestamp, item, *quantity, Some(*remaining))
            }
            Event::RecipeLearned { recipe } => self.trades.on_recipe_learned(recipe),
            Event::Chat { speaker, channel, message } => {
                // Quest text arrives as ordinary say-channel chat; other
                // channels are player conversation and would only be noise.
                if channel.eq_ignore_ascii_case("say") || channel.eq_ignore_ascii_case("says") {
                    self.tasks.on_dialogue(
                        entry.timestamp,
                        &speaker.display(&self.player),
                        message,
                    );
                    // Sky quests start by saying a trigger word, and only your
                    // own utterance starts one.
                    if speaker.is_you() {
                        self.sky.on_say(entry.timestamp, message);
                    } else {
                        self.sky.on_giver_line(
                            entry.timestamp,
                            &speaker.display(&self.player),
                            message,
                        );
                    }
                }
            }
            Event::TaskAssigned { task } => self.tasks.on_assigned(entry.timestamp, task),
            Event::TaskUpdated { task } => self.tasks.on_updated(entry.timestamp, task),
            Event::EffectWoreOffTarget { spell, target } => {
                if self.fading_effect_ends_a_charm(spell) {
                    self.tracker.set_charmed(entry.timestamp, target, false);
                }
            }
            Event::Resisted { target, spell } => {
                // Only charm resists reach the overlay. Everything resists
                // everything constantly; the whole value of the panel is that
                // it stays quiet until the thing you care about fails.
                if self.spell_is_charm(spell) {
                    self.note_charm_failure(
                        entry.timestamp,
                        &target.display(&self.player),
                        "resisted",
                        spell,
                    );
                }
            }
            Event::CharmImmune => {
                // The line names nobody, so the target is the one we were last
                // trying to charm.
                let (t, sp) = self.last_charm_attempt.clone().unwrap_or_default();
                self.note_charm_failure(entry.timestamp, &t, "immune", &sp);
            }
            Event::EffectLandedOnTarget { target, text } => {
                // The line names no spell, and eleven share this text — six
                // charms, the rest mez and damage. So it counts as a charm
                // only when a charm spell of yours landed moments earlier.
                // Guessing from the text alone would hand an enemy's damage to
                // you the instant a mez broke.
                if self.landed_effect_is_your_charm(entry.timestamp, text) {
                    self.tracker.set_charmed(entry.timestamp, target, true);
                }
            }
            Event::TradeOffer { item, quantity, .. } => self.sky.on_offer(item, *quantity),
            Event::TradeComplete { npc } => self.sky.on_trade_complete(entry.timestamp, npc),
            Event::SkillTrained { skill, .. } => self.trades.on_trained(skill),
            Event::SpellSetLoaded { name } => {
                self.classes.observe_spell_set(entry.timestamp, name);
            }
            Event::AbilityListEntry { id, ability } => {
                self.aa.on_entry(*id, ability);
                self.aa_pending_desc.clear();
            }
            Event::AbilityListDescription { text } => {
                self.aa_pending_desc = text.clone();
            }
            Event::AbilityListCost { cost } => {
                let desc = std::mem::take(&mut self.aa_pending_desc);
                self.aa.on_cost(*cost, &desc);
            }
            Event::AchievementEarned { name } => {
                self.tasks.on_achievement(entry.timestamp, name)
            }
            Event::LevelUp { level } => {
                self.resolver.set_level(*level);
                self.exp.on_level_up(entry.timestamp, *level);
            }
            Event::Experience { percent } => self.exp.on_experience(entry.timestamp, *percent),
            Event::CoinLoot { coin } => self.coin_from_corpses += coin.as_copper(),
            // Selling to a vendor is income just like an auto-sale.
            Event::MerchantSale { coin, .. } => self.coin_from_sales += coin.as_copper(),
            Event::Loot {
                item,
                quantity,
                source,
                disposition,
            } => {
                // Recorded here as well as inside the encounter, because loot
                // frequently arrives after the fight has closed.
                self.sky.on_loot(item, *quantity);
                let (fate, destination, sold) =
                    eqlparser_core::encounter::LootFate::split(disposition);
                let record = eqlparser_core::encounter::LootRecord {
                    timestamp: entry.timestamp,
                    item: item.clone(),
                    quantity: *quantity,
                    source: source.as_ref().map(|s| s.display(&self.player)),
                    sold_for_copper: sold,
                    fate,
                    destination,
                };
                if let Some(c) = record.sold_for_copper {
                    self.coin_from_sales += c;
                }
                self.all_loot.push(record);
            }
            Event::AbilityPointsGained { points, unspent } => {
                self.exp.on_ability_points(entry.timestamp, *points, *unspent)
            }
            Event::AbilityPointsSpent { ability, rank, cost } => {
                self.exp
                    .on_ability_spent(entry.timestamp, ability, *rank, *cost)
            }
            Event::Death { victim, killer } => {
                // Your own death strips your buffs. The client logs no fade
                // lines for it, so this is the only chance to notice.
                if victim.is_you() {
                    self.resolver.clear_on_death();
                }
                // Credit is by *participation*, not the killing blow. The game
                // attributes the final hit to whoever landed it, which for a
                // charming class is routinely your own charmed pet:
                //   "Innoruuk, the Prince of Hate has been slain by Innoruuk`s Chosen!"
                //   "Lady Vox has been slain by a priest of Nagafen!"
                // Requiring your own killing blow silently discarded those
                // kills from both experience attribution and boss lockouts.
                let fought_it = self
                    .tracker
                    .current()
                    .and_then(|e| e.actors.get(&victim.key()))
                    .is_some_and(|s| s.damaged_by_you);
                if killer.as_ref().is_some_and(|k| k.is_you()) || fought_it {
                    let name = victim.display(&self.player);
                    self.exp.on_kill(entry.timestamp, &name);
                    // Difficulty comes from the zone, so use the zone we were
                    // in when it died, not wherever we ended up later.
                    self.lockouts
                        .on_kill(entry.timestamp, &name, self.tracker.current_zone());
                    // Lets a following "task has been updated" say which step.
                    self.tasks.on_kill(entry.timestamp, &name);
                }
            }
            Event::ZoneChange { zone } => {
                // Most buffs do not survive a zone line in practice, but rather
                // than drop them we simply let them expire naturally.
                self.resolver.expire(entry.timestamp);
                self.exp.on_zone(entry.timestamp, zone);
            }
            Event::Unknown => {
                // Effect flavour text is only recognisable via the spell
                // database, so it arrives here rather than as a typed event.
                let body = entry
                    .raw
                    .split_once("] ")
                    .map(|(_, b)| b)
                    .unwrap_or(&entry.raw);
                // Fade first, but only when it would end something actually
                // active. Many strings are both a landing and a wear-off
                // message, and reading a buff's expiry as a debuff landing
                // conjures effects the player never had.
                if self.resolver.is_fade_of_active(body) {
                    self.resolver.resolve_fade(body);
                } else if self.resolver.db().is_landing_text(body) {
                    self.resolver.resolve_landing(entry.timestamp, body);
                } else if self.resolver.db().is_fade_text(body) {
                    self.resolver.resolve_fade(body);
                }
            }
            _ => {}
        }

        if let Some(done) = self.tracker.push(&entry) {
            self.encounters.push(done);
        }

        // Loot picked up after its fight closed. This runs *after* the tracker
        // sees the line, because the tracker closes a stale encounter lazily on
        // the next event — checking beforehand would still see it as open and
        // the loot would be lost from the per-fight view.
        if matches!(entry.event, Event::Loot { .. }) && self.tracker.current().is_none() {
            if let Some(rec) = self.all_loot.last().cloned() {
                if let Some(last) = self.encounters.last_mut() {
                    let within_grace = rec.timestamp - last.end
                        <= eqlparser_core::encounter::LOOT_GRACE_SECONDS;
                    let already = last
                        .loot
                        .iter()
                        .any(|l| l.timestamp == rec.timestamp && l.item == rec.item);
                    if within_grace && !already {
                        last.loot.push(rec);
                    }
                }
            }
        }
    }

    fn observe_class_spell(&mut self, timestamp: i64, spell: &str) {
        if let Some(levels) = self.resolver.db().class_levels_for_name(spell) {
            self.classes.observe_spell(timestamp, spell, &levels);
        }
    }

    pub fn encounters(&self) -> &[Encounter] {
        &self.encounters
    }

    /// Declare how an effect should be classified, and persist it.
    ///
    /// Held by the daemon rather than the GUI so the overlay — which has no
    /// access to GUI config — classifies identically.
    pub fn set_buff_mark(&mut self, name: &str, mark: Option<BuffMark>) {
        self.resolver.set_mark(name, mark);
        self.save_marks();
    }

    pub fn buff_marks(&self) -> Vec<(String, BuffMark)> {
        self.resolver.marks()
    }

    /// Where declarations are stored between runs.
    fn marks_path() -> Option<std::path::PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(
            std::path::PathBuf::from(home)
                .join(".config/eqlparser/buff-marks.json"),
        )
    }

    fn save_marks(&self) {
        let Some(path) = Self::marks_path() else { return };
        let map: std::collections::BTreeMap<String, BuffMark> =
            self.resolver.marks().into_iter().collect();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(&map) {
            let _ = std::fs::write(&path, json);
        }
    }

    fn load_marks(&mut self) {
        let Some(path) = Self::marks_path() else { return };
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(map) = serde_json::from_str::<std::collections::BTreeMap<String, BuffMark>>(&text)
        else {
            eprintln!("[eqlparserd] buff-marks.json is unreadable; ignoring it");
            return;
        };
        for (name, mark) in map {
            self.resolver.set_mark(&name, Some(mark));
        }
    }

    /// What has been hitting you, aggregated across every fight or one of them.
    ///
    /// Built from the *receiving* side, so a mob that split its attention only
    /// contributes what actually landed on you.
    pub fn incoming_report(&self, encounter_id: Option<u64>) -> IncomingReport {
        use std::collections::HashMap;
        let mut by_kind: HashMap<String, (u64, u64, u64)> = HashMap::new();
        let mut by_ability: HashMap<String, (u64, u64, u64)> = HashMap::new();
        let mut by_source: HashMap<String, (u64, u64, u64)> = HashMap::new();
        let mut r = IncomingReport {
            encounter_id,
            ..Default::default()
        };

        let selected: Vec<&Encounter> = match encounter_id {
            Some(id) => self.encounters.iter().filter(|e| e.id == id).collect(),
            None => self
                .encounters
                .iter()
                .chain(self.tracker.current())
                .collect(),
        };

        for e in selected {
            let Some(you) = e.actors.get("you") else { continue };
            r.total += you.damage_taken;
            r.hits_taken += you.damage_taken_hits();
            r.attacks_missed += you.attacks_missed;
            r.attacks_avoided += you.avoided;
            r.deaths += you.deaths;

            for (k, v) in &you.damage_taken_by_kind {
                by_kind.entry(k.clone()).or_default().0 += v;
            }
            for (k, a) in &you.damage_taken_by_ability {
                let e = by_ability.entry(k.clone()).or_default();
                e.0 += a.total;
                e.1 += a.hits;
                e.2 = e.2.max(a.max);
                if a.max > r.biggest_hit {
                    r.biggest_hit = a.max;
                    r.biggest_hit_from = Some(k.clone());
                }
            }
            for (k, v) in &you.damage_taken_by_source {
                by_source.entry(k.clone()).or_default().0 += v;
            }
        }

        let total = r.total.max(1) as f64;
        let to_rows = |m: HashMap<String, (u64, u64, u64)>| -> Vec<IncomingRow> {
            let mut v: Vec<IncomingRow> = m
                .into_iter()
                .map(|(name, (t, hits, max))| IncomingRow {
                    name,
                    total: t,
                    hits,
                    max,
                    avg: if hits > 0 { t as f64 / hits as f64 } else { 0.0 },
                    share: t as f64 / total,
                })
                .collect();
            v.sort_by_key(|x| std::cmp::Reverse(x.total));
            v
        };
        r.by_kind = to_rows(by_kind);
        r.by_ability = to_rows(by_ability);
        r.by_source = to_rows(by_source);

        let swings = r.hits_taken + r.attacks_missed + r.attacks_avoided;
        r.avoidance_rate = if swings > 0 {
            (r.attacks_missed + r.attacks_avoided) as f64 / swings as f64
        } else {
            0.0
        };
        r
    }

    /// The class trio in effect at `now`, as "WAR/DRU/BRD".
    ///
    /// Empty until the detector has three classes' worth of evidence, so early
    /// kills stay honestly unattributed rather than being credited to whatever
    /// build the character later became.
    /// A day either side, matching the window the live class panel uses.
    ///
    /// Narrower windows read as more precise but are not: a build changes
    /// every few days at most, while the evidence for a class that casts
    /// nothing — Warrior, whose skills are long since capped and therefore
    /// silent — arrives only every few hours. Too tight a window simply loses
    /// it, and a shared-spell class takes the slot instead.
    fn build_label(&self, at: i64) -> String {
        let mut trio = self.classes.trio_around(at, 24 * 3600);
        if trio.len() < 3 {
            return String::new();
        }
        // Canonical order, not the order the evidence happened to arrive in.
        // The label is a grouping key: a spell set named "WAR / BRD / DRU" and
        // another named "WAR / DRU / BRD - Buffs" describe the same build, and
        // ordering by whichever was loaded last split one build into two in
        // the kill history.
        trio.sort_by_key(|c| c.class.index());
        trio.iter()
            .map(|c| c.class.abbrev())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Every class with evidence in the last day, ranked, including the ones
    /// the trio selection rejected.
    pub fn class_ranking(&self) -> Vec<eqlparser_core::class::ClassScore> {
        let now = self.now();
        self.classes.ranked_in_window(now - 24 * 3600, now)
    }

    /// History worth keeping between sessions.
    ///
    /// Only boss kills, the fights that contained them, levels and AA are
    /// persisted. Ordinary trash fights are deliberately left out: there are
    /// thousands, they are re-derivable from the log, and none of them backs a
    /// build guide. Each kill is matched to the encounter it happened in so
    /// the stored row carries the fight's duration and your damage, which is
    /// what per-raid averages are built from.
    pub fn history_to_persist(
        &self,
    ) -> (
        Vec<crate::store::StoredKill>,
        Vec<crate::store::StoredEncounter>,
        Vec<crate::store::StoredLevel>,
        Vec<(String, u32, u32, i64)>,
    ) {
        let mut kills = Vec::new();
        let mut encs = Vec::new();

        for k in self.lockouts.kills() {
            let summary = self
                .encounter_containing(k.timestamp)
                .map(|e| self.summarise(e, false));
            let (duration, your_damage, your_dps) = summary
                .as_ref()
                .map(|s| (s.duration, s.your_damage, s.your_dps))
                .unwrap_or((0, 0, 0.0));
            let build = self.build_label(k.timestamp);
            kills.push(crate::store::StoredKill {
                timestamp: k.timestamp,
                boss: k.boss.clone(),
                difficulty: k.difficulty,
                zone: k.zone.clone(),
                solo: k.solo,
                build: build.clone(),
                duration,
                your_damage,
                your_dps,
            });
            if let Some(e) = summary {
                encs.push(crate::store::StoredEncounter {
                    start: e.start,
                    end: e.end,
                    title: e.title,
                    zone: e.zone.unwrap_or_default(),
                    duration: e.duration,
                    your_damage: e.your_damage,
                    your_dps: e.your_dps,
                    total_damage: e.damage_to_enemies,
                    kills: e.kills as u32,
                    deaths: e.your_deaths as u32,
                    build,
                });
            }
        }

        let levels = self
            .levels_with_build()
            .into_iter()
            .filter(|l| l.level.is_some() && l.complete)
            .map(|l| crate::store::StoredLevel {
                level: l.level.unwrap_or(0),
                started: l.started,
                ended: l.ended.unwrap_or(0),
                seconds: l.seconds,
                kills: l.kills,
                percent: l.percent,
                build: l.build.clone(),
                zones: serde_json::to_string(&l.zones).unwrap_or_else(|_| "[]".into()),
            })
            .collect();

        let aa = self
            .exp
            .aa_purchases()
            .into_iter()
            .map(|p| (p.ability, p.rank, p.spent, p.last_seen))
            .collect();

        (kills, encs, levels, aa)
    }

    /// The completed encounter a timestamp falls inside, if any.
    fn encounter_containing(&self, ts: i64) -> Option<&Encounter> {
        self.encounters
            .iter()
            .find(|e| ts >= e.start && ts <= e.end + eqlparser_core::encounter::LOOT_GRACE_SECONDS)
    }

    /// Which bosses each build has cleared.
    pub fn build_history(&self) -> Vec<BuildRecord> {
        self.lockouts.builds(&|ts| self.build_label(ts))
    }

    /// Whether a named effect wearing off a creature ends its charm.
    ///
    /// True for a charm spell, and also when the database has never heard of
    /// the spell — without the data to rule it out, ending the charm is the
    /// safer error, since the alternative credits an enemy's own damage to
    /// you. A spell that is known and is not a charm leaves the charm alone.
    fn fading_effect_ends_a_charm(&self, spell: &str) -> bool {
        let db = self.resolver.db();
        let base = eqlparser_core::spell::base_spell_name(spell);
        let candidates = db.candidates_for_name(base);
        if candidates.is_empty() {
            return true;
        }
        candidates
            .iter()
            .filter_map(|id| db.get(*id))
            .any(|sp| sp.is_charm)
    }

    /// Whether a spell name, with or without its rank, is a charm.
    fn spell_is_charm(&self, spell: &str) -> bool {
        let db = self.resolver.db();
        db.candidates_for_name(eqlparser_core::spell::base_spell_name(spell))
            .iter()
            .filter_map(|id| db.get(*id))
            .any(|sp| sp.is_charm)
    }

    fn note_charm_failure(&mut self, at: i64, target: &str, kind: &'static str, spell: &str) {
        self.charm_failures.push(CharmFailure {
            at,
            target: target.to_string(),
            kind,
            spell: spell.to_string(),
        });
        // Bound the list, but do not age it here: pruning against the newest
        // failure's own timestamp means a lone entry never expires, because
        // nothing later ever arrives to push it out. Age is applied when the
        // report is read, against the current time.
        if self.charm_failures.len() > CHARM_FAILURE_MAX {
            let excess = self.charm_failures.len() - CHARM_FAILURE_MAX;
            self.charm_failures.drain(0..excess);
        }
    }

    /// Your deaths, newest first, each with the blows that caused it.
    ///
    /// Only your own: a mob dying is a kill, and the buffer of incoming hits
    /// only ever held damage aimed at you.
    pub fn death_report(&self) -> DeathReport {
        use std::collections::HashMap;
        let mut rows: Vec<DeathReportRow> = Vec::new();
        let mut by_killer: HashMap<String, u32> = HashMap::new();
        let mut by_zone: HashMap<String, u32> = HashMap::new();

        let all = self
            .encounters
            .iter()
            .chain(self.tracker.current().into_iter());
        for enc in all {
            let title = self.summarise(enc, false).title;
            for d in &enc.deaths {
                if !self.is_player(&d.victim) {
                    continue;
                }
                let zone = enc.zone.clone().unwrap_or_default();
                *by_killer
                    .entry(d.killer.clone().unwrap_or_else(|| "unknown".into()))
                    .or_default() += 1;
                *by_zone.entry(zone.clone()).or_default() += 1;
                rows.push(DeathReportRow {
                    at: d.timestamp,
                    killer: d.killer.clone(),
                    zone,
                    encounter_id: enc.id,
                    encounter_title: title.clone(),
                    damage_in_window: d.damage_in_window,
                    blows: d.final_blows.clone(),
                });
            }
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.at));

        let mut by_killer: Vec<(String, u32)> = by_killer.into_iter().collect();
        by_killer.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut by_zone: Vec<(String, u32)> = by_zone.into_iter().collect();
        by_zone.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        DeathReport {
            total: rows.len(),
            deaths: rows,
            by_killer,
            by_zone,
        }
    }

    /// Whether a display name refers to the logging character.
    fn is_player(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case(&self.player) || name.eq_ignore_ascii_case("you")
    }

    /// Charm state and recent failures.
    pub fn charm_report(&self) -> CharmReport {
        let now = self.now();
        let cur = self.tracker.current();
        // Aged here rather than on insert: the overlay must go quiet on its
        // own after a fight, not sit showing a resist from hours ago because
        // it happened to be the last one.
        let mut failures: Vec<CharmFailure> = self
            .charm_failures
            .iter()
            .filter(|f| now - f.at <= CHARM_FAILURE_WINDOW)
            .cloned()
            .collect();
        failures.reverse();
        CharmReport {
            recent_failures: failures.iter().filter(|f| now - f.at <= 60).count(),
            charmed: cur
                .map(|e| {
                    e.actors
                        .values()
                        .filter(|s| s.charmed_by_you)
                        .map(|s| s.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
            uptime: cur.map(|e| e.charm_uptime()).unwrap_or(0.0),
            breaks: cur.map(|e| e.charm_breaks()).unwrap_or(0),
            failures,
        }
    }

    /// Whether a landing on someone else was a charm you cast.
    ///
    /// Requires both that the text belongs to a charm spell and that you cast
    /// that same spell within the resolver's association window — the pairing
    /// is what disambiguates a charm from a mez sharing the phrase.
    fn landed_effect_is_your_charm(&self, timestamp: i64, text: &str) -> bool {
        self.resolver
            .db()
            .candidates_for_landing_on_other(text)
            .iter()
            .filter_map(|id| self.resolver.db().get(*id))
            .any(|sp| sp.is_charm && self.resolver.cast_recently(timestamp, &sp.name))
    }

    /// The AA catalogue from `/alt list`, matched against what has been bought.
    pub fn aa_report(&self) -> AaReport {
        let owned: std::collections::HashMap<String, (u32, u32)> = self
            .exp
            .aa_purchases()
            .into_iter()
            .map(|p| (p.ability.to_lowercase(), (p.rank, p.spent)))
            .collect();
        self.aa.report(&owned)
    }

    /// Plane of Sky quest progress, flagged against the character's classes.
    pub fn sky_report(&self) -> SkyReport {
        let now = self.current_class_names();
        // Every class ever put in a loadout, so a respec does not hide the
        // sets you were part-way through collecting for the old one.
        let mut played: Vec<String> = self
            .classes
            .classes_ever_declared()
            .into_iter()
            .map(|c| c.name().to_string())
            .collect();
        for c in &now {
            if !played.contains(c) {
                played.push(c.clone());
            }
        }
        self.sky.report_for(&now, &played)
    }

    /// The character's current classes by full name, for matching against
    /// data keyed on class ("Warrior", not "WAR").
    fn current_class_names(&self) -> Vec<String> {
        self.classes
            .current_trio(self.now(), 24 * 3600)
            .into_iter()
            .map(|c| c.class.name().to_string())
            .collect()
    }

    /// Add or replace a Sky quest definition, and persist the catalogue.
    pub fn define_sky_quest(&mut self, def: SkyQuestDef) {
        self.sky.define(def);
        self.save_sky_quests();
    }

    fn sky_path() -> Option<std::path::PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(std::path::PathBuf::from(home).join(".config/eqlparser/sky-quests.json"))
    }

    fn save_sky_quests(&self) {
        let Some(path) = Self::sky_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self.sky.definitions()) {
            let _ = std::fs::write(&path, json);
        }
    }

    fn load_sky_quests(&mut self) {
        let Some(path) = Self::sky_path() else { return };
        let Ok(text) = std::fs::read_to_string(&path) else {
            // No file yet: keep the seeded table.
            return;
        };
        match serde_json::from_str::<Vec<SkyQuestDef>>(&text) {
            Ok(defs) => self.sky.set_definitions(defs),
            Err(_) => eprintln!("[eqlparserd] sky-quests.json is unreadable; using the built-in table"),
        }
    }

    /// Tasks and achievements.
    pub fn task_report(&self) -> TaskReport {
        self.tasks.report()
    }

    /// Seed a task's objectives, and persist them.
    pub fn define_task(&mut self, def: TaskDef) {
        self.tasks.define(def);
        self.save_task_defs();
    }

    /// Where hand-seeded task definitions live between runs.
    ///
    /// Separate from the log entirely: the client never states a task's
    /// objectives, so this is knowledge the player supplies and we must not
    /// lose on restart.
    fn task_defs_path() -> Option<std::path::PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(std::path::PathBuf::from(home).join(".config/eqlparser/tasks.json"))
    }

    fn save_task_defs(&self) {
        let Some(path) = Self::task_defs_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.tasks.definitions()) {
            let _ = std::fs::write(&path, json);
        }
    }

    fn load_task_defs(&mut self) {
        let Some(path) = Self::task_defs_path() else { return };
        let defs: Vec<TaskDef> = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(d) => d,
                Err(_) => {
                    eprintln!("[eqlparserd] tasks.json is unreadable; using defaults");
                    Vec::new()
                }
            },
            // No file yet: ship the known task so a fresh install is useful.
            Err(_) => eqlparser_core::task::seed_definitions(),
        };
        for d in defs {
            self.tasks.define(d);
        }
    }

    /// Tradeskill activity: skill progress, combines and components.
    pub fn tradeskill_report(&self) -> TradeskillReport {
        self.trades.report()
    }

    /// Weekly and daily boss lockout grid.
    pub fn lockout_report(&self, now: i64) -> LockoutReport {
        let mut r = self.lockouts.report(now);
        r.current_build = self.build_label(now);
        r
    }

    /// Completed levels, each labelled with the build that earned it.
    ///
    /// The label is derived here rather than stamped at level-up so it can use
    /// evidence from the whole log, including after the level ended. Taken
    /// from the level's midpoint, which is more representative than either
    /// edge — a level that straddles a respec is reported as whichever build
    /// it mostly was, and the neighbouring levels show the change.
    fn levels_with_build(&self) -> Vec<LevelRecord> {
        self.exp
            .completed_levels()
            .iter()
            .cloned()
            .map(|mut r| {
                let mid = r.ended.map_or(r.started, |e| r.started + (e - r.started) / 2);
                r.build = self.build_label(mid);
                r
            })
            .collect()
    }

    /// Full experience report: per-level history plus the best zones and mobs.
    pub fn experience_report(&self, now: i64) -> ExperienceReport {
        let rate = self.exp.current_rate_per_hour(now);
        let cur = self.exp.current_record();
        ExperienceReport {
            current_level: self.exp.current_level(),
            current_percent: cur.percent,
            current_kills: cur.kills,
            current_seconds: cur.seconds,
            rate_per_hour: rate,
            seconds_to_next_level: self.exp.seconds_to_next_level(now),
            average_seconds_per_level: self.exp.average_seconds_per_level(),
            average_kills_per_level: self.exp.average_kills_per_level(),
            levels: self.levels_with_build(),
            // A zone visited only briefly yields a rate built on noise.
            best_zones: self.exp.zones_by_rate(10 * 60),
            best_mobs: self.exp.mobs_by_exp_per_kill(5),
            aa_earned: self.exp.aa_earned(),
            aa_unspent: self.exp.aa_unspent(),
            aa_spent: self.exp.aa_spent_total(),
            aa_per_hour: self.exp.aa_per_hour(now, AA_RATE_WINDOW),
            aa_window_seconds: AA_RATE_WINDOW,
            aa_lifetime_per_hour: self.exp.aa_lifetime_per_hour(),
            aa_zones: self.exp.zones_by_aa_rate(10 * 60),
            aa_purchases: self.exp.aa_purchases(),
            aa_potions_used: self.exp.aa_potions_used(),
            aa_potion_charges: self.exp.aa_potion_charges(),
            aa_potion_active: self.exp.aa_potion_likely_active(),
            aa_gains_doubled: self.exp.aa_gains_doubled(),
            aa_gains_normal: self.exp.aa_gains_normal(),
            aa_doubled_share: self.exp.aa_doubled_share(),
            aa_effective_multiplier: self.exp.aa_effective_multiplier(),
            aa_doubled_unaccounted: self.exp.aa_doubled_unaccounted(),
        }
    }

    /// Look up a completed encounter by id.
    pub fn encounter(&self, id: u64) -> Option<&Encounter> {
        self.encounters.iter().find(|e| e.id == id)
    }

    /// Aggregate every looted item across the log, most recent first.
    pub fn loot_tally(&self) -> Vec<LootTally> {
        use std::collections::HashMap;
        let mut map: HashMap<String, LootTally> = HashMap::new();
        {
            for l in &self.all_loot {
                let t = map.entry(l.item.clone()).or_insert_with(|| LootTally {
                    item: l.item.clone(),
                    count: 0,
                    sources: Vec::new(),
                    last_seen: 0,
                    sold_total_copper: 0,
                    kept: 0,
                    sold: 0,
                    stored: 0,
                    transmuted: 0,
                    stored_in: Vec::new(),
                    transmuted_into: Vec::new(),
                });
                t.count += l.quantity;
                t.last_seen = t.last_seen.max(l.timestamp);
                t.sold_total_copper += l.sold_for_copper.unwrap_or(0);
                use eqlparser_core::encounter::LootFate;
                match l.fate {
                    LootFate::Kept => t.kept += l.quantity,
                    LootFate::Sold => t.sold += l.quantity,
                    LootFate::Stored => t.stored += l.quantity,
                    LootFate::Transmuted => t.transmuted += l.quantity,
                }
                if let Some(d) = &l.destination {
                    let bucket = match l.fate {
                        LootFate::Transmuted => &mut t.transmuted_into,
                        _ => &mut t.stored_in,
                    };
                    if !bucket.contains(d) {
                        bucket.push(d.clone());
                    }
                }
                if let Some(src) = &l.source {
                    if !t.sources.contains(src) {
                        t.sources.push(src.clone());
                    }
                }
            }
        }
        let mut v: Vec<LootTally> = map.into_values().collect();
        v.sort_by_key(|t| std::cmp::Reverse(t.last_seen));
        v
    }

    /// Build the meter rows for an encounter.
    ///
    /// Includes every actor that did *anything* — dealt damage, healed, or took
    /// damage — rather than only damage dealers, so the same row set can back a
    /// damage meter, a healing meter, and a damage-taken meter. Each panel
    /// filters and sorts client-side; shares are precomputed per metric so bar
    /// widths are correct in all three.
    fn meter_for(&self, enc: &Encounter) -> Vec<MeterRow> {
        let total_dmg = enc.total_damage().max(1) as f64;
        let total_heal: u64 = enc.actors.values().map(|s| s.healing_done).sum();
        let total_taken: u64 = enc.actors.values().map(|s| s.damage_taken).sum();
        let total_heal_f = total_heal.max(1) as f64;
        let total_taken_f = total_taken.max(1) as f64;
        let dur = enc.duration() as f64;

        enc.active_actors()
            .into_iter()
            .map(|(key, s)| MeterRow {
                name: s.name.clone(),
                key: key.to_string(),
                damage: s.damage_done,
                dps: enc.dps(key),
                share: s.damage_done as f64 / total_dmg,
                healing: s.healing_done,
                hps: s.healing_done as f64 / dur,
                heal_share: s.healing_done as f64 / total_heal_f,
                damage_taken: s.damage_taken,
                taken_share: s.damage_taken as f64 / total_taken_f,
                healing_received: s.healing_received,
                crit_rate: s.crit_rate(),
                crit_by_kind: s
                    .crit_breakdown()
                    .into_iter()
                    .map(|(k, h, c, r)| (k.to_string(), h, c, r))
                    .collect(),
                accuracy: s.accuracy(),
                is_you: key == "you",
                is_enemy: s.damaged_by_you && !s.charmed_by_you,
                is_pet: s.charmed_by_you,
                pet_damage: s.damage_while_charmed,
            })
            .collect()
    }

    pub fn summarise(&self, enc: &Encounter, with_meter: bool) -> EncounterSummary {
        let you = enc.actors.get("you");
        EncounterSummary {
            id: enc.id,
            title: enc.title(),
            zone: enc.zone.clone(),
            start: enc.start,
            end: enc.end,
            duration: enc.duration(),
            total_damage: enc.total_damage(),
            damage_to_enemies: enc.damage_to_enemies(),
            your_damage: you.map(|s| s.damage_done).unwrap_or(0),
            your_dps: enc.dps("you"),
            your_hps: enc.hps("you"),
            your_healing: you.map(|s| s.healing_done).unwrap_or(0),
            your_damage_taken: enc.your_damage_taken(),
            your_deaths: you.map(|s| s.deaths).unwrap_or(0),
            charm_uptime: enc.charm_uptime(),
            charm_breaks: enc.charm_breaks(),
            kills: enc
                .deaths
                .iter()
                .filter(|d| d.killer.as_deref() == Some(self.player.as_str()))
                .count(),
            loot_count: enc.loot.len(),
            coin_copper: enc.coin_copper,
            meter: if with_meter { self.meter_for(enc) } else { Vec::new() },
        }
    }

    /// Produce the current snapshot.
    ///
    /// `recent_limit` bounds the encounter list; the historical browser pages
    /// through the full set via the REST API instead.
    pub fn snapshot(&self, recent_limit: usize) -> Snapshot {
        let now = self.now();

        let classes = self
            .classes
            .current_trio(now, 24 * 3600)
            .into_iter()
            .map(|c: ClassScore| ClassInfo {
                name: c.class.name().to_string(),
                abbrev: c.class.abbrev().to_string(),
                score: c.score,
                share: c.share,
                reasons: c.top_reasons,
            })
            .collect();

        let to_info = |b: eqlparser_core::ActiveBuff| BuffInfo {
            spell_id: b.spell_id,
            remaining: b.remaining(now),
            total: b.expires.map(|e| e - b.started),
            confidence: b.confidence,
            permanent: b.permanent,
            is_aura: b.is_aura(),
            category: serde_json::to_value(b.category)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "other".to_string()),
            mark: b.mark.map(|m| match m {
                BuffMark::Aura => "aura".to_string(),
                BuffMark::Permanent => "permanent".to_string(),
            }),
            refreshes: b.refreshes,
            beneficial: b.beneficial,
            name: b.name,
        };
        let buffs = self.resolver.active_buffs(now).into_iter().map(to_info).collect();
        let auras = self.resolver.active_auras(now).into_iter().map(to_info).collect();
        let debuffs = self.resolver.active_debuffs(now).into_iter().map(to_info).collect();

        // Include the fight in progress. Counting only completed encounters
        // leaves every headline figure frozen for the whole of a long boss
        // fight — exactly when they are being watched.
        let in_progress = self.tracker.current();
        let mut totals = Totals {
            encounters: self.encounters.len() + usize::from(in_progress.is_some()),
            ..Default::default()
        };
        totals.coin_copper = self.coin_from_corpses;
        totals.coin_from_sales_copper = self.coin_from_sales;
        totals.coin_total_copper = self.coin_from_corpses + self.coin_from_sales;
        for e in self.encounters.iter().chain(in_progress) {
            totals.total_damage += e.total_damage();
            totals.damage_to_enemies += e.damage_to_enemies();

            totals.items_looted += e.loot.len();
            totals.kills += e
                .deaths
                .iter()
                .filter(|d| d.killer.as_deref() == Some(self.player.as_str()))
                .count();
            if let Some(you) = e.actors.get("you") {
                totals.your_damage += you.damage_done;
                totals.your_healing += you.healing_done;
                totals.your_damage_taken += you.damage_taken;
                totals.deaths += you.deaths;
            }
        }

        // The newest encounter carries its meter so the overlay's idle mode can
        // keep the last fight on screen between pulls; the rest stay light,
        // since a full meter per row would bloat every push.
        let recent = self
            .encounters
            .iter()
            .rev()
            .take(recent_limit)
            .enumerate()
            .map(|(i, e)| self.summarise(e, i == 0))
            .collect();

        Snapshot {
            player: self.player.clone(),
            level: self.resolver.level(),
            zone: self.tracker.current_zone().map(str::to_string),
            classes,
            current: self.tracker.current().map(|e| self.summarise(e, true)),
            recent,
            buffs,
            auras,
            debuffs,
            exp: ExpSummary {
                percent: self.exp.current_percent(),
                kills: self.exp.current_record().kills,
                rate_per_hour: self.exp.current_rate_per_hour(now),
                seconds_to_next_level: self.exp.seconds_to_next_level(now),
                aa_earned: self.exp.aa_earned(),
                aa_unspent: self.exp.aa_unspent(),
                aa_per_hour: self.exp.aa_per_hour(now, AA_RATE_WINDOW),
                aa_potion_charges: self.exp.aa_potion_charges(),
                aa_potion_active: self.exp.aa_potion_likely_active(),
            },
            totals,
            log_time: self.last_timestamp,
            live: self.live,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        Engine::new("Tharn".to_string(), SpellDb::empty())
    }

    fn feed(e: &mut Engine, lines: &[&str]) {
        for l in lines {
            e.push_line(l);
        }
    }

    #[test]
    fn snapshot_reports_live_encounter_and_meter() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash a lesser mummy for 100 points of damage.",
                "[Tue Jul 28 14:00:10 2026] A lesser mummy hits YOU for 20 points of damage.",
            ],
        );
        let s = e.snapshot(10);
        assert_eq!(s.player, "Tharn");
        let cur = s.current.expect("an encounter should be open");
        assert_eq!(cur.total_damage, 120);
        assert_eq!(cur.your_damage, 100);
        assert_eq!(cur.meter.len(), 2);
        assert_eq!(cur.meter[0].name, "Tharn");
        assert!(cur.meter[0].is_you);
        assert!((cur.meter[0].share - 100.0 / 120.0).abs() < 1e-9);
        // The mummy is an enemy because we damaged it.
        assert!(cur.meter.iter().any(|r| r.is_enemy && !r.is_you));
    }

    /// Headline totals must move during a fight, not only once it closes.
    #[test]
    fn totals_include_the_fight_in_progress() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash a giant for 500 points of damage.",
                "[Tue Jul 28 14:00:01 2026] A giant hits YOU for 90 points of damage.",
                "[Tue Jul 28 14:00:02 2026] You healed Tharn for 40 hit points by Healing Water.",
            ],
        );
        let s = e.snapshot(5);
        assert!(s.current.is_some(), "fight should still be open");
        assert_eq!(s.totals.your_damage, 500, "in-progress damage must count");
        assert_eq!(s.totals.your_damage_taken, 90);
        assert_eq!(s.totals.your_healing, 40);
        assert_eq!(s.totals.encounters, 1, "the open fight counts as one");
    }

    /// ...and must not be double counted once it closes.
    #[test]
    fn in_progress_totals_are_not_double_counted_on_close() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash a giant for 500 points of damage.",
                // Long gap closes the fight and opens a new one.
                "[Tue Jul 28 15:00:00 2026] You slash a rat for 7 points of damage.",
            ],
        );
        let s = e.snapshot(5);
        assert_eq!(s.totals.your_damage, 507, "closed 500 + open 7, counted once each");
        assert_eq!(s.totals.encounters, 2);
    }

    #[test]
    fn completed_encounters_feed_totals() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash a lesser mummy for 100 points of damage.",
                "[Tue Jul 28 14:00:01 2026] You have slain a lesser mummy!",
                "[Tue Jul 28 14:00:02 2026] You receive 1 platinum from the corpse.",
                // Long gap closes the encounter.
                "[Tue Jul 28 14:30:00 2026] You slash a rat for 5 points of damage.",
            ],
        );
        let s = e.snapshot(10);
        // One closed fight plus the one still open.
        assert_eq!(s.totals.encounters, 2);
        assert_eq!(s.totals.your_damage, 105, "100 closed + 5 in progress");
        assert_eq!(s.totals.kills, 1);
        assert_eq!(s.totals.coin_copper, 1000);
        // `recent` lists only completed fights; the open one is `current`.
        assert_eq!(s.recent.len(), 1);
        assert_eq!(s.recent[0].title, "a lesser mummy");
        assert!(s.current.is_some());
    }

    /// A database with one charm song and one mez song that share a landing
    /// phrase — the exact ambiguity the real data contains.
    fn charm_db() -> SpellDb {
        use eqlparser_core::spell::Spell;
        use eqlparser_core::class::{CLASS_CANNOT_CAST, CLASS_COUNT};
        let mk = |id: u32, name: &str, is_charm: bool| Spell {
            id,
            name: name.into(),
            cast_time_ms: 0,
            recovery_ms: 0,
            recast_ms: 0,
            duration_formula: 0,
            duration_cap: 0,
            mana: 0,
            landed_on_you: None,
            landed_on_other: Some("'s eyes glaze over.".into()),
            fades: None,
            class_levels: [CLASS_CANNOT_CAST; CLASS_COUNT],
            good_effect: 0,
            category_id: 0,
            is_charm,
            linked: Vec::new(),
        };
        let mut m = std::collections::HashMap::new();
        m.insert(1, mk(1, "Solon's Bewitching Bravura", true));
        m.insert(2, mk(2, "Crission's Pixie Strike", false));
        SpellDb::from_spells(m)
    }

    /// A charmed pet only says "Attacking X Master" when ordered to, which on
    /// a real boss fight routinely never happens — 29.5k of pet damage went
    /// uncredited on an Innoruuk kill because of it. The charm landing itself
    /// has to be enough.
    #[test]
    fn a_charm_landing_credits_pet_damage_without_an_attack_order() {
        let mut e = Engine::new("Tharn".to_string(), charm_db());
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash Innoruuk for 100 points of damage.",
                "[Tue Jul 28 14:00:01 2026] You begin singing Solon's Bewitching Bravura.",
                "[Tue Jul 28 14:00:03 2026] Innoruuk`s Chosen's eyes glaze over.",
                "[Tue Jul 28 14:00:05 2026] Innoruuk`s Chosen slashes Innoruuk for 500 points of damage.",
            ],
        );
        let cur = e.snapshot(5).current.expect("open encounter");
        let pet = cur
            .meter
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case("innoruuk`s chosen"))
            .expect("charmed pet should be in the meter");
        assert!(pet.is_pet, "charm landing alone must mark the pet");
        assert_eq!(pet.pet_damage, 500, "damage dealt while charmed");
    }

    /// The same phrase is used by mez songs. Reading one as a charm would hand
    /// an enemy's damage to you the moment it broke.
    #[test]
    fn a_mez_sharing_the_charm_phrase_does_not_create_a_pet() {
        let mut e = Engine::new("Tharn".to_string(), charm_db());
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash Innoruuk for 100 points of damage.",
                "[Tue Jul 28 14:00:01 2026] You begin singing Crission's Pixie Strike.",
                "[Tue Jul 28 14:00:03 2026] Innoruuk`s Chosen's eyes glaze over.",
                "[Tue Jul 28 14:00:05 2026] Innoruuk`s Chosen slashes YOU for 500 points of damage.",
            ],
        );
        let cur = e.snapshot(5).current.expect("open encounter");
        let mobs = cur
            .meter
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case("innoruuk`s chosen"))
            .expect("row exists");
        assert!(!mobs.is_pet, "a mez must not be read as a charm");
        assert_eq!(mobs.pet_damage, 0);
    }

    /// A resist that is not a charm must never reach the charm overlay, and a
    /// charm resist must age out on its own rather than sitting there.
    #[test]
    fn the_charm_overlay_shows_only_charm_failures_and_forgets_them() {
        let mut e = Engine::new("Tharn".to_string(), charm_db());
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash Innoruuk for 100 points of damage.",
                // A mez resisting is not a charm failure.
                "[Tue Jul 28 14:00:02 2026] Innoruuk resisted your Crission's Pixie Strike!",
                // A charm resisting is, and the rank suffix must not hide it.
                "[Tue Jul 28 14:00:04 2026] Innoruuk resisted your Solon's Bewitching Bravura V!",
            ],
        );
        let r = e.charm_report();
        assert_eq!(r.failures.len(), 1, "only the charm: {:?}", r.failures);
        assert_eq!(r.failures[0].kind, "resisted");
        assert_eq!(r.failures[0].target, "Innoruuk");

        // Long after the fight, the panel must be empty rather than showing a
        // stale resist as though it had just happened.
        feed(
            &mut e,
            &["[Tue Jul 28 16:00:00 2026] You slash a rat for 1 point of damage."],
        );
        assert!(
            e.charm_report().failures.is_empty(),
            "failures must age out"
        );
    }

    /// The UIs style charmed pets from `is_pet`; if the meter stops carrying
    /// it, charm tracking silently vanishes from the display even though the
    /// numbers underneath are still right.
    #[test]
    fn meter_marks_charmed_pets() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash a fire giant for 100 points of damage.",
                "[Tue Jul 28 14:00:01 2026] An imp protector told you, 'Attacking a fire giant Master.'",
                "[Tue Jul 28 14:00:02 2026] An imp protector slashes a fire giant for 500 points of damage.",
            ],
        );
        let cur = e.snapshot(5).current.expect("open encounter");
        // Display names take the casing of whichever line first mentioned the
        // actor, so match case-insensitively.
        let imp = cur
            .meter
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case("an imp protector"))
            .expect("pet should appear in the meter");
        assert!(imp.is_pet, "charmed pet must be flagged for the UI");
        assert_eq!(imp.pet_damage, 500);
        assert!(!imp.is_you);

        // The fire giant dealt no damage but absorbed plenty, so it belongs in
        // the row set (the damage panel filters it out client-side, while the
        // damage-taken panel needs it).
        let giant = cur
            .meter
            .iter()
            .find(|r| r.name.to_lowercase().contains("fire giant"))
            .expect("a damage sponge still needs a row");
        assert_eq!(giant.damage, 0);
        assert_eq!(giant.damage_taken, 600);
        assert!(giant.is_enemy);
    }

    /// A charmed creature is an enemy by origin, but while charmed it must not
    /// be styled as one — otherwise it reads as a hostile row on the meter.
    #[test]
    fn charmed_pet_is_not_also_marked_enemy() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                // The imp is hostile first, so `damaged_by_you` gets set.
                "[Tue Jul 28 14:00:00 2026] You slash an imp protector for 10 points of damage.",
                "[Tue Jul 28 14:00:01 2026] An imp protector told you, 'Attacking a rat Master.'",
                "[Tue Jul 28 14:00:02 2026] An imp protector slashes a rat for 50 points of damage.",
            ],
        );
        let cur = e.snapshot(5).current.expect("open encounter");
        let imp = cur
            .meter
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case("an imp protector"))
            .expect("pet row");
        assert!(imp.is_pet);
        assert!(!imp.is_enemy, "charm should override the enemy classification");
    }

    #[test]
    fn meter_carries_healing_and_damage_taken() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash a giant for 100 points of damage.",
                "[Tue Jul 28 14:00:01 2026] A giant hits YOU for 400 points of damage.",
                "[Tue Jul 28 14:00:02 2026] You healed Tharn for 250 hit points by Healing Water.",
                "[Tue Jul 28 14:00:10 2026] You healed Tharn for 250 hit points by Healing Water.",
            ],
        );
        let cur = e.snapshot(5).current.expect("open encounter");
        assert_eq!(cur.your_damage_taken, 400);
        assert_eq!(cur.your_healing, 500);
        assert!(cur.your_hps > 0.0);

        let you = cur.meter.iter().find(|r| r.is_you).expect("your row");
        assert_eq!(you.healing, 500);
        assert_eq!(you.damage_taken, 400);
        assert_eq!(you.healing_received, 500);
        // Shares are per-metric, so each panel's bars are scaled correctly.
        assert!((you.heal_share - 1.0).abs() < 1e-9, "you did all the healing");
        // 500 damage was absorbed in total: 400 by you, 100 by the giant.
        assert!(
            (you.taken_share - 0.8).abs() < 1e-9,
            "expected 400/500, got {}",
            you.taken_share
        );

        let giant = cur
            .meter
            .iter()
            .find(|r| !r.is_you)
            .expect("the giant should have a row");
        assert_eq!(giant.damage_taken, 100);
        assert!((giant.taken_share - 0.2).abs() < 1e-9);
    }

    #[test]
    fn totals_include_damage_taken() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash a rat for 10 points of damage.",
                "[Tue Jul 28 14:00:01 2026] A rat hits YOU for 70 points of damage.",
                "[Tue Jul 28 15:00:00 2026] You slash a bat for 1 point of damage.",
            ],
        );
        assert_eq!(e.snapshot(5).totals.your_damage_taken, 70);
    }

    #[test]
    fn zone_is_tracked() {
        let mut e = engine();
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] You have entered West Commonlands."],
        );
        assert_eq!(e.snapshot(5).zone.as_deref(), Some("West Commonlands"));
    }

    #[test]
    fn level_is_tracked() {
        let mut e = engine();
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] You have gained a level! Welcome to level 42!"],
        );
        assert_eq!(e.snapshot(5).level, 42);
    }

    /// Corpses are looted after the fight ends. On a boss the gap can exceed
    /// the encounter idle timeout, and such loot used to be dropped entirely —
    /// a real Innoruuk kill lost an item picked up 22 seconds late.
    /// A charmed pet or ally often lands the killing blow on a boss. Credit
    /// must follow participation, or those kills vanish from lockouts and
    /// experience attribution alike.
    #[test]
    fn a_kill_by_your_charmed_pet_still_counts() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Fri Jul 31 22:00:00 2026] You have entered The Plane of Hate - Solo.",
                "[Fri Jul 31 22:30:00 2026] You slash Innoruuk, the Prince of Hate for 500 points of damage.",
                // The game credits the pet, not you.
                "[Fri Jul 31 22:36:43 2026] Innoruuk, the Prince of Hate has been slain by Innoruuk`s Chosen!",
            ],
        );
        let r = e.lockout_report(1_785_280_000);
        let cell = r
            .cells
            .iter()
            .find(|c| c.boss == "Innoruuk" && c.difficulty == 0)
            .expect("Innoruuk row");
        assert!(
            cell.last_kill.is_some(),
            "a boss you fought counts even when the pet lands the blow"
        );
    }

    /// A boss dying with no involvement from you must not clear your lockout.
    #[test]
    fn a_kill_you_had_no_part_in_does_not_count() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Fri Jul 31 22:00:00 2026] You have entered The Plane of Hate - Solo.",
                "[Fri Jul 31 22:30:00 2026] You slash a rat for 10 points of damage.",
                "[Fri Jul 31 22:30:05 2026] Lady Vox has been slain by a priest of Nagafen!",
            ],
        );
        let r = e.lockout_report(1_785_280_000);
        let cell = r.cells.iter().find(|c| c.boss == "Lady Vox").unwrap();
        assert!(cell.last_kill.is_none(), "you never touched it");
    }

    /// Auto-sold loot is real income and was previously ignored entirely,
    /// understating total platinum by more than half on a real log.
    #[test]
    fn sold_loot_counts_toward_coin_totals() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Fri Jul 31 10:00:00 2026] You slash a rat for 10 points of damage.",
                "[Fri Jul 31 10:00:01 2026] You have slain a rat!",
                "[Fri Jul 31 10:00:02 2026] You receive 2 platinum from the corpse.",
                "[Fri Jul 31 10:00:03 2026] You looted a Rat Ear from a rat's corpse and sold it for 3 platinum.",
            ],
        );
        let t = e.snapshot(5).totals;
        assert_eq!(t.coin_copper, 2_000, "corpse coin");
        assert_eq!(t.coin_from_sales_copper, 3_000, "sale proceeds");
        assert_eq!(t.coin_total_copper, 5_000, "both together");
    }

    /// Coin, like loot, can arrive after the fight has closed.
    #[test]
    fn coin_after_the_fight_closes_is_still_counted() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Fri Jul 31 10:00:00 2026] You slash a rat for 10 points of damage.",
                "[Fri Jul 31 10:00:01 2026] You have slain a rat!",
                // Well past the idle timeout.
                "[Fri Jul 31 10:05:00 2026] You receive 7 platinum from the corpse.",
            ],
        );
        assert_eq!(e.snapshot(5).totals.coin_copper, 7_000);
    }

    #[test]
    fn loot_after_the_fight_closes_is_still_recorded() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Fri Jul 31 22:38:00 2026] You slash Innoruuk for 100 points of damage.",
                "[Fri Jul 31 22:38:05 2026] You have slain Innoruuk!",
                // Well past the 12s idle timeout, so no encounter is open.
                "[Fri Jul 31 22:38:32 2026] --You have looted a Bloodstar Pendant from Innoruuk's corpse.--",
            ],
        );
        let tally = e.loot_tally();
        let found = tally.iter().find(|t| t.item == "Bloodstar Pendant");
        assert!(found.is_some(), "late loot must not be discarded");
        assert_eq!(found.unwrap().sources, vec!["Innoruuk"]);

        // It should also be attached to the fight it came from.
        let enc = e.encounters().last().expect("the closed encounter");
        assert!(
            enc.loot.iter().any(|l| l.item == "Bloodstar Pendant"),
            "late loot should attach to the fight that produced it"
        );
    }

    /// Loot far removed from any fight is still tallied, just not attributed.
    #[test]
    fn loot_long_after_a_fight_is_tallied_but_unattached() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Fri Jul 31 10:00:00 2026] You slash a rat for 10 points of damage.",
                "[Fri Jul 31 10:00:01 2026] You have slain a rat!",
                // An hour later — nothing to do with that fight.
                "[Fri Jul 31 11:00:00 2026] --You have looted a Random Trinket from a chest.--",
            ],
        );
        assert!(e.loot_tally().iter().any(|t| t.item == "Random Trinket"));
        let enc = e.encounters().last().unwrap();
        assert!(
            !enc.loot.iter().any(|l| l.item == "Random Trinket"),
            "unrelated loot must not be attributed to an old fight"
        );
    }

    #[test]
    fn loot_tally_aggregates_across_encounters() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash a rat for 10 points of damage.",
                "[Tue Jul 28 14:00:01 2026] You have slain a rat!",
                "[Tue Jul 28 14:00:02 2026] --You have looted a Rat Ear from a rat's corpse.--",
                "[Tue Jul 28 15:00:00 2026] You slash a rat for 10 points of damage.",
                "[Tue Jul 28 15:00:01 2026] You have slain a rat!",
                "[Tue Jul 28 15:00:02 2026] --You have looted a Rat Ear from a rat's corpse.--",
                "[Tue Jul 28 16:00:00 2026] You slash a bat for 10 points of damage.",
            ],
        );
        let tally = e.loot_tally();
        let ear = tally.iter().find(|t| t.item == "Rat Ear").expect("Rat Ear");
        assert_eq!(ear.count, 2);
        assert_eq!(ear.sources, vec!["a rat"]);
    }

    #[test]
    fn now_uses_last_log_time_when_not_live() {
        let mut e = engine();
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] You slash a rat for 10 points of damage."],
        );
        assert_eq!(e.now(), e.snapshot(1).log_time);
        assert!(!e.snapshot(1).live);
    }

    /// The live-session case: the app sits running for a while before play
    /// starts, so the startup calibration is stale by that gap. Once new lines
    /// arrive, `now()` must track them rather than staying pinned to the old
    /// backlog — otherwise buff timers stop counting down between lines.
    #[test]
    fn clock_recalibrates_as_new_lines_arrive() {
        let mut e = engine();
        // Backlog ends well in the past relative to the eventual play session.
        feed(
            &mut e,
            &["[Tue Jul 28 10:00:00 2026] You slash a rat for 10 points of damage."],
        );
        e.set_live(true);
        let stale_offset = e.clock_offset.expect("calibrated at startup");

        // Play begins hours later; the game writes much newer timestamps.
        feed(
            &mut e,
            &["[Tue Jul 28 18:00:00 2026] You slash a giant for 10 points of damage."],
        );
        let fresh_offset = e.clock_offset.expect("recalibrated");
        assert_ne!(
            stale_offset, fresh_offset,
            "offset must follow the log's clock, not stay at the startup value"
        );

        // `now` should sit at the newest line, not behind it, and must never
        // resolve earlier than the last line seen.
        let now = e.now();
        assert!(
            now >= e.last_timestamp,
            "now ({now}) must not precede the newest line ({})",
            e.last_timestamp
        );
        assert!(
            now - e.last_timestamp < 5,
            "now should track the newest line closely, drifted {}s",
            now - e.last_timestamp
        );
    }

    #[test]
    fn going_live_calibrates_the_clock_forward() {
        let mut e = engine();
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] You slash a rat for 10 points of damage."],
        );
        let before = e.now();
        e.set_live(true);
        // Once calibrated, `now` tracks the wall clock and never runs backwards.
        assert!(e.now() >= before);
        assert!(e.snapshot(1).live);
    }
}
