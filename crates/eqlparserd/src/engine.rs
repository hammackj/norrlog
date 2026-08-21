//! The analysis engine: owns every stateful piece of the pipeline and
//! publishes an immutable [`Snapshot`] for the API and overlay to read.
//!
//! Keeping all mutable state behind one struct means the HTTP layer never
//! touches a parser or tracker directly; it reads a cheap clone of the latest
//! snapshot. That keeps request handling lock-free apart from a brief read.

use eqlparser_core::aa::{AaCatalog, AaReport};
use eqlparser_core::alert::{AlertBook, AlertFire, AlertRule, Signal};
use eqlparser_core::bestiary::{Bestiary, BestiaryReport, CreatureKind, HealthSample, Swing};
use eqlparser_core::class::{ClassDetector, ClassScore};
use eqlparser_core::drops::{DropReport, DropTracker};
use eqlparser_core::encounter::{Encounter, EncounterTracker};
use eqlparser_core::experience::{AaPurchase, ExpSource, ExpTracker, LevelRecord};
use eqlparser_core::game::Game;
use eqlparser_core::lockout::{BuildRecord, LockoutReport, LockoutTracker};
use eqlparser_core::npc::{NpcReport, NpcTracker};
use eqlparser_core::skyquest::{SkyQuestDef, SkyReport, SkyTracker};
use eqlparser_core::spell::{BuffMark, SpellDb, SpellResolver};
use eqlparser_core::task::{TaskDef, TaskReport, TaskTracker};
use eqlparser_core::tells::{TellReport, TellTracker};
use eqlparser_core::tradeskill::{TradeskillReport, TradeskillTracker};
use eqlparser_core::{Event, LogEntry, Parser, SpellbookChangeKind};
use serde::Serialize;
use std::collections::VecDeque;
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
    /// What produced the crits: (flag, count). `Slay Undead` is a separate
    /// mechanism from an ordinary `Critical` and the melee rate alone cannot
    /// say which one a paladin's biggest hits came from.
    pub crit_by_flag: Vec<(String, u64)>,
    /// Extra-attack and reaction flags, as (flag, swings), most frequent
    /// first — Flurry, Riposte, Rampage, Finishing Blow. Counted over hits and
    /// misses alike; see `ActorStats::swings_by_flag`.
    #[serde(default)]
    pub swings_by_flag: Vec<(String, u64)>,
    /// Melee swings, the denominator those flags are a rate against.
    #[serde(default)]
    pub melee_swings: u64,
    /// What became of this actor's own swings: (outcome, count), covering
    /// landed, missed, and each way a target turned one aside.
    #[serde(default)]
    pub offence_outcomes: Vec<(String, u64)>,
    /// The same for swings aimed at this actor — its defence.
    #[serde(default)]
    pub defence_outcomes: Vec<(String, u64)>,
    /// Heals cast and how many crit. Kept apart from `crit_rate`, which is
    /// damage: a healer who never swings would otherwise have no crit rate.
    pub heals: u64,
    pub heal_crits: u64,
    pub heal_crit_rate: f64,
    /// Overhealing, over the number of heals that stated a pre-cap figure.
    ///
    /// `over_heal_measured` is below `heals` whenever the client wrote only one
    /// number, so this is a floor: the real waste is higher by whatever the
    /// unmeasured heals spilled. The denominator travels with the figure so the
    /// UI cannot present it as a share of all healing.
    pub over_healing: u64,
    pub healing_attempted: u64,
    pub over_heal_measured: u64,
    pub accuracy: f64,
    /// True when this actor is the logging character.
    pub is_you: bool,
    /// True when the logging character has damaged this actor, i.e. it is an
    /// opponent rather than an ally.
    pub is_enemy: bool,
    /// True when this actor fought for you — charmed or summoned. Takes
    /// precedence over `is_enemy` in the UI: a charmed creature is an enemy by
    /// origin but fights for you, and its damage counts toward your total.
    pub is_pet: bool,
    /// At least two creatures in the fight answered to this name, so the row's
    /// numbers pool them and cannot be read as one creature's. See
    /// `ActorStats::name_shared`.
    #[serde(default)]
    pub name_shared: bool,
    /// When this actor last dealt or took damage, in the log's clock. Compare
    /// against the encounter's `end` for seconds since; see
    /// `ActorStats::last_action`.
    #[serde(default)]
    pub last_action: i64,
    /// Seconds this actor spent stunned in the fight, and the stuns behind it.
    /// Only ever populated for you — the client states nobody else's.
    #[serde(default)]
    pub stunned_seconds: i64,
    #[serde(default)]
    pub stuns: u32,
    #[serde(default)]
    pub stuns_avoided: u32,
    /// "charmed", "summoned", "pet", or absent when this is not a pet.
    ///
    /// Charmed and summoned are worth telling apart on screen: a charm can
    /// break and a summoned pet cannot, so they demand different amounts of
    /// attention. The plain "pet" is the honest third answer — the creature
    /// fights for you, and nothing in the log says which kind it is. A pet
    /// with a generated name that was never asked `/pet leader` lands here.
    pub pet_kind: Option<&'static str>,
    /// The player this pet fights for, including other people's pets. Absent
    /// for anything that is not a pet.
    pub pet_owner: Option<String>,
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
    /// The class trio active when this was fought, e.g. "WAR/DRU/BRD".
    ///
    /// Resolved per encounter rather than looked up later: a build changes over
    /// a character's life, so which one fought a given fight is only knowable
    /// from its own timestamp.
    pub build: String,
    /// Instance tier from the zone name — 0 for open world, 4 for Refined.
    pub difficulty: u8,
    /// Populated for the live encounter and on demand; empty in long lists.
    pub meter: Vec<MeterRow>,
    /// Your output grouped by mechanism, populated alongside `meter`.
    #[serde(default)]
    pub outgoing_groups: Vec<AbilityGroup>,
    /// What was shrugged off, populated alongside `meter`.
    #[serde(default)]
    pub resists: Vec<eqlparser_core::encounter::ResistTally>,
    /// Per-creature split, populated alongside `meter`.
    #[serde(default)]
    pub mobs: Vec<eqlparser_core::encounter::MobBreakdown>,
}

/// Your damage under one mechanism — Spell, Melee or Other.
#[derive(Debug, Clone, Serialize)]
pub struct AbilityGroup {
    pub group: String,
    pub damage: u64,
    /// Share of your damage in this fight, 0.0–1.0.
    pub share: f64,
    /// Abilities within the group, biggest first.
    pub abilities: Vec<GroupedAbility>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupedAbility {
    pub name: String,
    pub damage: u64,
    pub hits: u64,
    pub crits: u64,
    pub max: u64,
    pub average: f64,
    /// Times it was cast, which is not `hits`; see [`AbilityStats::casts`].
    /// Zero for anything swung rather than cast.
    pub casts: u64,
    /// Share of its own group, so bars read within the group.
    pub share: f64,
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
    /// Sessions seen in this log, and the seconds spent inside them. The
    /// denominator for any "per hour" figure, since the log's own span counts
    /// the days the game was closed.
    #[serde(default)]
    pub sessions: u32,
    #[serde(default)]
    pub played_seconds: i64,
    /// Coin taken directly from corpses.
    pub coin_copper: u64,
    /// Coin from items auto-sold on pickup — a large share of real income.
    pub coin_from_sales_copper: u64,
    /// Corpse coin plus sales.
    pub coin_total_copper: u64,
}

/// Everything the log can say about one item.
#[derive(Debug, Clone, Serialize)]
pub struct LootDetail {
    pub item: String,
    /// Items received. A stack of five counts as five.
    pub total: u64,
    /// Times it dropped. A stack of five counts as one.
    pub drops: u64,
    pub first_seen: i64,
    pub last_seen: i64,
    /// Who dropped it, most drops first.
    pub sources: Vec<LootSource>,
    /// Where it dropped: (zone, drops).
    pub zones: Vec<(String, u64)>,
    pub kept: u64,
    pub sold: u64,
    pub stored: u64,
    pub transmuted: u64,
    /// Sale proceeds across every time it was auto-sold.
    pub sold_total_copper: u64,
    pub sales: u64,
    pub best_sale_copper: u64,
    /// What a merchant charged you for one, if you ever bought it. Lets a drop
    /// be valued at replacement cost rather than only at what a vendor pays.
    pub buy_price_copper: Option<u64>,
    /// Stack sizes it arrived in: (size, times). Anything other than a single
    /// `(1, n)` means the item stacks, which is worth seeing.
    pub stacks: Vec<(u64, u64)>,
}

/// One creature's record of dropping an item.
#[derive(Debug, Clone, Serialize)]
pub struct LootSource {
    pub mob: String,
    /// Times this creature dropped it.
    pub drops: u64,
    /// Items received from it in total.
    pub quantity: u64,
    /// Times this creature died in a fight of yours — the denominator.
    pub kills: u64,
    /// `drops / kills`. Only meaningful with a denominator to divide by, so it
    /// is `None` when no kill of this creature was observed, rather than a
    /// fabricated 100%.
    pub rate: Option<f64>,
    pub zones: Vec<String>,
    pub last: i64,
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
    /// Points spent. The player's own figure when they have given one,
    /// otherwise what the log accounts for.
    pub aa_spent: u32,
    /// Points spent on purchases visible in the log. A floor on the
    /// character's lifetime spend: ranks bought before logging began cannot
    /// be seen, so this reads low by exactly that amount.
    pub aa_spent_tracked: u32,
    /// The gap between the two — ranks that predate this log. Constant on any
    /// given character, so once it is known the total stops drifting.
    pub aa_spent_untracked: u32,
    /// Ranks the character demonstrably holds but was never seen buying.
    pub aa_unlogged: Vec<eqlparser_core::experience::AaPurchase>,
    /// The most those ranks can have cost, which bounds the gap above.
    pub aa_unlogged_cost_max: u32,
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
    /// Lifetime points spent. Shown rather than earned, because spent and
    /// unspent are the pair the character sheet shows and so the pair the
    /// player can check at a glance.
    pub aa_spent: u32,
    pub aa_per_hour: f64,
    /// Doubled awards remaining from experience potions.
    pub aa_potion_charges: u32,
    pub aa_potion_active: bool,
}

/// Everything the UI needs, as of the last processed line.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub player: String,
    /// The character's effective level — the lowest class in the loadout,
    /// which is what `/who` reports. The last level-up message is no use here:
    /// it names no class, so after a swap it keeps reporting the level of the
    /// class that left.
    pub level: u32,
    /// The class earning experience, when it can be pinned down.
    pub levelling: Option<String>,
    pub zone: Option<String>,
    pub classes: Vec<ClassInfo>,
    /// The fight in progress, if any.
    pub current: Option<EncounterSummary>,
    /// Completed fights, newest first.
    pub recent: Vec<EncounterSummary>,
    /// Motes looted this session, biggest tier last, with the total.
    ///
    /// Carried on the snapshot rather than fetched: the overlay renders from
    /// this and nothing else, and a second poll loop for one panel would be a
    /// second thing to keep in step. Kept as a running tally so the cost is a
    /// map bump per looted line rather than a walk of every item on every
    /// snapshot.
    #[serde(default)]
    pub session_motes: Vec<(String, u64)>,
    #[serde(default)]
    pub session_motes_total: u64,
    /// Seconds the current sitting has been going, so a per-hour figure can be
    /// worked out without a second request. Measured against the log's own
    /// clock — the wall clock is a timezone away from it.
    #[serde(default)]
    pub session_seconds: i64,
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
    /// Alerts that recently went off, oldest first.
    ///
    /// Carried on the snapshot rather than pushed on their own channel so a
    /// client that missed a frame still catches up: play everything newer than
    /// the sequence you last played, then remember `alert_seq`.
    pub alerts: Vec<AlertFire>,
    /// The highest alert sequence issued so far. A client records this on its
    /// first snapshot without playing anything, so connecting mid-session does
    /// not replay the last few minutes at you.
    pub alert_seq: u64,
}

/// How many past fires ride along on each snapshot.
///
/// Only has to cover the gap between two publishes, which is a poll interval.
const SNAPSHOT_ALERTS: u64 = 20;

/// Window for the "AA per hour" figure.
///
/// Wider than the experience window: points arrive as whole units and far less
/// often, so a short window reads zero most of the time.
/// How long a charm failure stays on the overlay.
/// Fallback session boundary, used only until a logon is seen.
///
/// The client announces each logon outright, which is what normally starts a
/// session. This covers the case where the log begins part-way through one —
/// there is no marker to find behind us, and treating the whole file as a
/// single session would be worse than guessing from its quiet stretches.
const SESSION_GAP_SECONDS: i64 = 30 * 60;

const CHARM_FAILURE_WINDOW: i64 = 300;
/// Hard cap on retained failures, so a pathological fight cannot grow the list
/// without bound.
const CHARM_FAILURE_MAX: usize = 50;

const AA_RATE_WINDOW: i64 = 2 * 3600;

/// Rank a mote tier, so the panel reads in the order they are earned.
///
/// Unknown adjectives sort last rather than first: a tier nobody has taught
/// this function about is more likely to be a new high one than a new low one.
fn mote_tier(name: &str) -> u8 {
    let n = name.to_lowercase();
    for (i, tier) in [
        "infinitesimal",
        "minor",
        "lesser",
        "intermediate",
        "greater",
        "major",
        "grand",
    ]
    .iter()
    .enumerate()
    {
        if n.contains(tier) {
            return i as u8;
        }
    }
    u8::MAX
}

/// Whether a looted item is a Mote of Potential.
///
/// Matched on the word rather than the full name because the tiers differ only
/// by their adjective — Infinitesimal, Minor, Lesser, Major — and a new one
/// would otherwise go uncounted until somebody noticed.
fn is_mote(item: &str) -> bool {
    item.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case("mote"))
}

/// Wall-clock seconds since the Unix epoch.
fn system_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Everything known about one item, curated and observed kept apart.
///
/// The separation is the point. A recipe comes from a reference site and is
/// true of the game; a drop, a price or a combine count comes from one
/// character's log and is true of what that character happened to see. Merging
/// them would produce a record nobody could audit later.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ItemEntry {
    pub item: String,
    /// Which game the *observations* are from. Curated recipes carry their own
    /// game tag, since a book entry may describe a game this log is not.
    pub game: eqlparser_core::game::Game,
    /// The tradeskill that makes it, when known.
    pub skill: Option<String>,
    /// Curated: recipes that produce it.
    pub made_by: Vec<eqlparser_core::recipe::Recipe>,
    /// Curated: products it is a component of.
    pub used_in: Vec<String>,
    /// Curated: a tool a combine hands back rather than consumes.
    pub tool: bool,
    /// Observed: creatures it was looted from.
    pub dropped_by: Vec<eqlparser_core::drops::DropSighting>,
    /// Observed: the last unit price a merchant charged, in copper.
    pub merchant_copper: Option<u64>,
    /// Observed: how many were bought.
    pub bought: u64,
    /// Observed: who sold it and where — the counterpart to `dropped_by`, and
    /// the other way an item enters the world.
    pub sold_by: Vec<eqlparser_core::tradeskill::VendorPrice>,
    /// Observed: combines attempted at it, and how many succeeded.
    pub combines: u64,
    pub combine_successes: u64,
    /// Observed: how many were consumed by combines.
    pub consumed: u64,
}

/// The item database this log has contributed to.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ItemDatabase {
    /// The game the observations describe. `unknown` when the install could
    /// not be identified — see [`eqlparser_core::game`].
    pub game: eqlparser_core::game::Game,
    /// Whose log the observations came from.
    pub character: String,
    pub items: Vec<ItemEntry>,
    /// Creatures killed and how often, as the denominator behind every
    /// sighting count. Not a drop rate: corpses you never looted are in here
    /// too, so a rate derived from it is a floor.
    pub kills: Vec<(String, u64)>,
}

/// Owns the parsing pipeline and derives snapshots from it.
/// How long a grant waits for the level-up that explains it.
///
/// In every log on file the two share a timestamp; a second of slack costs
/// nothing and covers a log written either side of a tick.
const GRANT_WINDOW: i64 = 1;

pub struct Engine {
    parser: Parser,
    tracker: EncounterTracker,
    resolver: SpellResolver,
    classes: ClassDetector,
    /// Per-class levels, which the single reported level cannot express.
    class_levels: eqlparser_core::classlevel::ClassLevelTracker,
    /// Lifetime points spent, as read off the character sheet by the player.
    aa_spent_stated: Option<u32>,
    exp: ExpTracker,
    /// Spells the game handed over, and the ones not yet attributed.
    ///
    /// Two fields because the log states the effect before the cause: the
    /// grant lines come first and the `Welcome to level 7!` that explains them
    /// comes after, so a grant cannot be filed the moment it is read.
    grants: Vec<crate::store::StoredGrant>,
    pending_grants: Vec<(i64, String)>,
    lockouts: LockoutTracker,
    sky: SkyTracker,
    aa: AaCatalog,
    tells: TellTracker,
    npcs: NpcTracker,
    /// Recent charm failures, for the resist overlay.
    charm_failures: Vec<CharmFailure>,
    /// The last charm we tried to land, so nameless failure lines can be
    /// attributed to something.
    last_charm_attempt: Option<(String, String)>,
    /// The description line awaiting its cost, so a rank keeps its text.
    aa_pending_desc: String,
    trades: TradeskillTracker,
    /// Where items were seen coming from, tagged with the game they were seen
    /// in — see [`eqlparser_core::game`] for why that tag is not optional.
    drops: DropTracker,
    /// Creature profiles, keyed by zone, mode and difficulty.
    bestiary: Bestiary,
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
    /// When the current play session began.
    session_start: i64,
    /// Motes looted since the current session began.
    session_motes: std::collections::HashMap<String, u64>,
    /// Sessions seen so far, and the seconds inside them.
    ///
    /// A rate needs a denominator, and the log's span is the wrong one: it
    /// includes every hour the game was not running. These count only the
    /// stretches the log was being written, so "per hour" means an hour of
    /// playing rather than an hour of the calendar.
    sessions: u32,
    played_seconds: i64,
    /// Whether a logon has been seen, after which gaps stop being used.
    seen_logon: bool,
    /// Difference between log time and system time, learned once we are
    /// following the file live.
    ///
    /// EverQuest writes local time with no zone, which we parse as if it were
    /// UTC. Rather than guess the machine's offset we measure it: when tailing
    /// reaches the end of the file, the newest line is approximately "now".
    clock_offset: Option<i64>,
    live: bool,
    /// Alert rules and what they have fired.
    alerts: AlertBook,
    /// The last lines to arrive, so the alerts view can offer "tell me when
    /// this happens" against something the player just watched scroll past.
    ///
    /// Live lines only: the backlog is hundreds of thousands of lines and none
    /// of it is what just happened.
    recent_lines: VecDeque<LogLine>,
    line_seq: u64,
}

/// One line as the alerts view sees it.
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    /// Monotonic, so a client can ask for only what it has not seen.
    pub seq: u64,
    pub at: i64,
    /// The line with the client's timestamp prefix removed.
    pub text: String,
}

/// How many recent lines are kept for the alerts view.
///
/// Combat writes tens of lines a second, so this is a few seconds of a busy
/// fight and a long while of standing still. It exists to be scrolled, not
/// searched — the log file itself is the archive.
const RECENT_LINES: usize = 400;

/// How many recent fights keep their per-event timeline.
///
/// An evening's worth of pulls, which is as far back as anyone opens a fight
/// to ask when something fired.
const TIMELINE_ENCOUNTERS: usize = 25;

impl Engine {
    /// Only the tests build an engine without declaring a game; the daemon
    /// always knows one, or knows that it does not.
    #[cfg(test)]
    pub fn new(player: String, spell_db: SpellDb) -> Engine {
        Engine::with_game(player, spell_db, Game::Unknown)
    }

    /// An engine that knows which game its log came from, so what it observes
    /// is recorded as being about that game rather than about EverQuest in
    /// general.
    pub fn with_game(player: String, spell_db: SpellDb, game: Game) -> Engine {
        let mut e = Engine {
            parser: Parser::new(&player),
            tracker: EncounterTracker::new(&player),
            resolver: SpellResolver::new(spell_db, 1),
            classes: ClassDetector::new(),
            class_levels: eqlparser_core::classlevel::ClassLevelTracker::new(),
            aa_spent_stated: None,
            exp: ExpTracker::new(),
            grants: Vec::new(),
            pending_grants: Vec::new(),
            lockouts: LockoutTracker::new(),
            sky: SkyTracker::new(),
            aa: AaCatalog::new(),
            tells: TellTracker::new(),
            npcs: NpcTracker::new(),
            charm_failures: Vec::new(),
            last_charm_attempt: None,
            aa_pending_desc: String::new(),
            trades: TradeskillTracker::new(),
            drops: DropTracker::new(game),
            bestiary: Bestiary::new(game, &player),
            tasks: TaskTracker::new(),
            encounters: Vec::new(),
            coin_from_corpses: 0,
            coin_from_sales: 0,
            all_loot: Vec::new(),
            player,
            last_timestamp: 0,
            session_start: 0,
            session_motes: std::collections::HashMap::new(),
            sessions: 0,
            played_seconds: 0,
            seen_logon: false,
            clock_offset: None,
            live: false,
            alerts: AlertBook::new(),
            recent_lines: VecDeque::new(),
            line_seq: 0,
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
        // Alerts are about what is happening now. Arming only once the backlog
        // is consumed is what keeps startup silent.
        self.alerts.arm(live);
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

    /// Start a session at `at`, closing out the one before it.
    ///
    /// Called from both boundaries — the logon line the client states, and the
    /// long silence inferred when no logon has been seen — so the count and the
    /// played total cannot disagree about what a session is.
    fn begin_session(&mut self, at: i64) {
        self.session_start = at;
    }

    /// Seconds of play.
    ///
    /// The sum of the gaps between consecutive lines that are shorter than
    /// [`SESSION_GAP_SECONDS`]. Deliberately not derived from session starts:
    /// `session_start` stops moving once a logon line has been seen — after
    /// that the client states its own boundaries — and a log whose logons are
    /// sparse would count every night the game was closed as time played. It
    /// did: 536 hours over a 23-day log, which is 23 hours a day.
    ///
    /// Counting only the short gaps needs no notion of where a session begins,
    /// which is why it cannot drift from one.
    pub fn played_seconds(&self) -> i64 {
        self.played_seconds
    }

    /// Sittings the log covers: one, plus a break for every long silence.
    pub fn sessions(&self) -> u32 {
        self.sessions.max(1)
    }

    /// The spell file this binary loaded, for callers that need to place a
    /// name without going through the resolver's cast history.
    pub fn spell_db(&self) -> &eqlparser_core::spell::SpellDb {
        self.resolver.db()
    }

    /// Feed one raw log line.
    pub fn push_line(&mut self, line: &str) {
        let Some(entry) = self.parser.parse_line(line) else {
            return;
        };
        self.push_entry(entry);
    }

    /// Record a live line for the alerts view and run the line-level rules.
    fn note_line(&mut self, entry: &LogEntry) {
        if !self.live {
            return;
        }
        // Split the borrow: the rules need the player name to render "you"
        // into something speakable, and it lives on the same struct.
        let Engine {
            alerts,
            player,
            recent_lines,
            line_seq,
            ..
        } = self;
        alerts.on_line(entry.timestamp, &entry.raw, &entry.event, player);
        *line_seq += 1;
        recent_lines.push_back(LogLine {
            seq: *line_seq,
            at: entry.timestamp,
            text: eqlparser_core::alert::strip_timestamp(&entry.raw).to_string(),
        });
        while recent_lines.len() > RECENT_LINES {
            recent_lines.pop_front();
        }
    }

    /// Whether the backlog has been consumed and we are following the file.
    /// Until then, alerts are disarmed and no lines are retained.
    pub fn is_live(&self) -> bool {
        self.live
    }

    /// Recent lines newer than `since`, oldest first.
    pub fn lines_since(&self, since: u64) -> Vec<LogLine> {
        self.recent_lines
            .iter()
            .filter(|l| l.seq > since)
            .cloned()
            .collect()
    }

    /// Evaluate the triggers that depend on elapsed time rather than on a line
    /// arriving.
    ///
    /// Called on every publish, including the quiet ones: a buff running out
    /// while you stand still is exactly the case a warning is for, and no log
    /// line marks it.
    pub fn tick(&mut self) {
        let now = self.now();
        // Permanents have no meaningful countdown, and auras re-land every few
        // seconds by design — warning about either would be constant noise.
        let watched: Vec<(String, i64)> = self
            .resolver
            .active_buffs(now)
            .into_iter()
            .filter(|b| !b.permanent && !b.is_aura())
            .filter_map(|b| b.remaining(now).map(|r| (b.name.clone(), r)))
            .collect();
        self.alerts.on_buffs(now, &watched);
    }

    // ---- alert rules ---------------------------------------------------

    pub fn alert_rules(&self) -> &[AlertRule] {
        self.alerts.rules()
    }

    pub fn set_alert_rules(&mut self, rules: Vec<AlertRule>) {
        self.alerts.set_rules(rules);
    }

    pub fn put_alert_rule(&mut self, rule: AlertRule) {
        self.alerts.put(rule);
    }

    pub fn remove_alert_rule(&mut self, id: &str) {
        self.alerts.remove(id);
    }

    fn push_entry(&mut self, entry: LogEntry) {
        // A session starts at a logon, which the client states outright; see
        // the `LoggedOn` arm below. Until one has been seen the log may have
        // begun mid-session, so fall back to inferring a boundary from a long
        // silence.
        if !self.seen_logon {
            if self.last_timestamp > 0
                && entry.timestamp - self.last_timestamp > SESSION_GAP_SECONDS
            {
                self.begin_session(entry.timestamp);
            } else if self.session_start == 0 {
                self.begin_session(entry.timestamp);
            }
        }
        // Time played, and how many sittings it took. A gap longer than the
        // threshold is the game being shut, so it is neither.
        if self.last_timestamp > 0 {
            let gap = entry.timestamp - self.last_timestamp;
            if gap > SESSION_GAP_SECONDS {
                self.sessions += 1;
                // A new sitting starts its own tally; the panel is "this
                // session", not "since the daemon started".
                self.session_motes.clear();
            } else if gap > 0 {
                self.played_seconds += gap;
            }
        } else {
            self.sessions = 1;
        }
        self.last_timestamp = entry.timestamp;
        // Keep the log/system clock relationship current while following live;
        // see `calibrate_clock` for why once-at-startup is not enough.
        if self.live {
            self.calibrate_clock();
        }

        // Grants waiting for an explanation that is not coming.
        //
        // A grant is followed by its `Welcome to level N!` in the same second.
        // Once the clock has moved past that, no level-up is going to claim
        // them and they are filed as what they are: spells handed over
        // without a level being gained, which is what a change of invocation
        // does -- 24 at once, every spell the new classes already qualify for.
        if entry.timestamp
            > self
                .pending_grants
                .first()
                .map_or(i64::MAX, |g| g.0 + GRANT_WINDOW)
        {
            self.file_grants(entry.timestamp, 0);
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
                // Memorising a spell you already own says nothing about how
                // you got it. Only a grant does.
                if matches!(change, SpellbookChangeKind::Granted) {
                    self.pending_grants.push((entry.timestamp, spell.clone()));
                }
            }
            Event::SkillUp { skill, value } => {
                self.classes.observe_skill(entry.timestamp, skill);
                self.trades.on_skill_up(entry.timestamp, skill, *value);
            }
            Event::Craft { item, success } => self.trades.on_craft(entry.timestamp, item, *success),
            Event::ComponentConsumed {
                item,
                quantity,
                remaining,
            } => self
                .trades
                .on_component(entry.timestamp, item, *quantity, Some(*remaining)),
            Event::ItemPurchased {
                item,
                quantity,
                coin,
                merchant,
            } => self.trades.on_purchase_from(
                entry.timestamp,
                item,
                *quantity,
                coin.as_copper(),
                Some(merchant.as_str()),
                // Where you are standing is where the merchant is.
                self.tracker.current_zone(),
            ),
            Event::ItemQuoted {
                item,
                merchant,
                coin,
            } => self.trades.on_quote_from(
                entry.timestamp,
                item,
                coin.as_copper(),
                Some(merchant.as_str()),
                // Where you are standing is where the merchant is.
                self.tracker.current_zone(),
            ),
            Event::RecipeLearned { recipe } => self.trades.on_recipe_learned(recipe),
            Event::Chat {
                speaker,
                channel,
                message,
            } => {
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
                        self.npcs.on_your_say(entry.timestamp, message);
                    } else {
                        self.npcs.on_npc_line(
                            entry.timestamp,
                            &speaker.display(&self.player),
                            message,
                        );
                        self.sky.on_giver_line(
                            entry.timestamp,
                            &speaker.display(&self.player),
                            message,
                        );
                    }
                }
            }
            Event::ItemGranted { item } => self.lockouts.on_item_granted(entry.timestamp, item),
            Event::TaskAssigned { task } => {
                self.tasks.on_assigned(entry.timestamp, task);
                self.lockouts.on_task_assigned(entry.timestamp, task);
            }
            Event::TaskUpdated { task } => {
                self.tasks.on_updated(entry.timestamp, task);
                self.lockouts.on_task_updated(entry.timestamp, task);
            }
            Event::EffectWoreOffTarget { spell, target } => {
                if self.fading_effect_ends_a_charm(spell) {
                    self.tracker.set_charmed(entry.timestamp, target, false);
                    // The line names a fading spell, not a charm break; only
                    // the spell database and the tracker together know it was
                    // one. So the alert cannot come from a text rule.
                    let who = target.display(&self.player);
                    self.alerts
                        .on_signal(entry.timestamp, Signal::CharmBroken { target: &who });
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
            Event::LoggedOn => {
                self.begin_session(entry.timestamp);
                self.seen_logon = true;
            }
            Event::Tell { from, to, message } => {
                // The counterpart is whichever end is not you.
                let outgoing = from.is_you();
                let other = if outgoing { to } else { from };
                self.tells.on_tell(
                    entry.timestamp,
                    &other.display(&self.player),
                    outgoing,
                    message,
                );
            }
            Event::TradeOffer { item, quantity, .. } => {
                self.sky.on_offer(item, *quantity);
                self.npcs.on_offer(item, *quantity);
            }
            Event::TradeComplete { npc } => {
                self.sky.on_trade_complete(entry.timestamp, npc);
                self.npcs.on_trade_complete(entry.timestamp, npc);
            }
            // Damage into a creature is evidence of its health; damage out of
            // one is evidence about the pairing. Both are recorded, neither is
            // confused for the other.
            Event::Damage {
                source,
                target,
                amount,
                ability,
                ..
            } => {
                if let eqlparser_core::event::Actor::Named(npc) = target {
                    self.bestiary.on_damage_to(entry.timestamp, npc, *amount);
                    // A damage line names both the spell and who it hit, so a
                    // spell the game will only cast at one body type settles
                    // what that creature is. Whose spell it was does not
                    // matter — the restriction is the game's, so a group
                    // member's cast proves it just as well as your own.
                    if let Some(kind) = ability.as_deref().and_then(|a| self.spell_restricts_to(a))
                    {
                        self.bestiary.on_kind_evidence(entry.timestamp, npc, kind);
                    }
                }
                if target.is_you() {
                    if let eqlparser_core::event::Actor::Named(npc) = source {
                        self.bestiary
                            .on_swing(entry.timestamp, npc, Swing::Hit(*amount));
                    }
                }
            }
            Event::Miss { source, target, .. } => {
                if target.is_you() {
                    if let eqlparser_core::event::Actor::Named(npc) = source {
                        self.bestiary.on_swing(entry.timestamp, npc, Swing::Missed);
                    }
                }
            }
            Event::Avoided { source, target, .. } => {
                if target.is_you() {
                    if let eqlparser_core::event::Actor::Named(npc) = source {
                        self.bestiary.on_swing(entry.timestamp, npc, Swing::Avoided);
                    }
                }
            }
            Event::Faction {
                faction,
                adjustment,
            } => {
                if let Some(adj) = adjustment {
                    self.npcs.on_faction(entry.timestamp, faction, *adj);
                }
            }
            Event::SkillTrained { skill, .. } => self.trades.on_trained(skill),
            Event::SpellSetLoaded { name } => {
                self.classes.observe_spell_set(entry.timestamp, name);
                // An invocation swap changes what you are, so a fight either
                // side of one is two different characters' damage.
                self.tracker.note_mark(entry.timestamp, "invocation", name);
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
                self.tasks.on_achievement(entry.timestamp, name);
                // "Druid's Combat Proficiency, Level 50" is the only line that
                // states a named class's level, and the only way to learn the
                // level of a class that capped before this log began.
                self.class_levels.on_achievement(entry.timestamp, name);
            }
            Event::WhoEntry {
                level,
                classes,
                name,
                ..
            } => {
                if name.eq_ignore_ascii_case(&self.player) {
                    let trio = eqlparser_core::class::parse_class_list(classes);
                    if !trio.is_empty() {
                        self.class_levels.on_who(entry.timestamp, *level, &trio);
                        // The game naming your own loadout beats any inference
                        // from spell set names or cast history.
                        self.classes.observe_who(entry.timestamp, &trio);
                    }
                }
            }
            Event::StanceChanged { stance } => {
                self.bestiary.on_stance(stance);
                // Mid-fight it is a decision worth seeing against the damage
                // either side of it.
                self.tracker.note_mark(entry.timestamp, "stance", stance);
            }
            Event::LevelUp { level } => {
                // The grants for this level were printed a moment ago, before
                // the line that says which level it is.
                self.file_grants(entry.timestamp, *level);
                self.bestiary.on_level(*level as u32);
                self.resolver.set_level(*level);
                self.exp.on_level_up(entry.timestamp, *level);
                let trio = self.current_loadout(entry.timestamp);
                self.class_levels
                    .on_level_up(entry.timestamp, *level, &trio);
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
                if is_mote(item) {
                    *self.session_motes.entry(item.clone()).or_insert(0) += (*quantity).max(1);
                }

                // A named corpse that is not your own is a creature, and what
                // came off it is a drop. Your own corpse is items going back
                // where they came from, which says nothing about any creature.
                if let Some(eqlparser_core::event::Actor::Named(npc)) = source {
                    if !npc.eq_ignore_ascii_case(&self.player) {
                        self.drops.on_drop(
                            entry.timestamp,
                            item,
                            *quantity,
                            npc,
                            self.tracker.current_zone(),
                        );
                    }
                }
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
                self.exp
                    .on_ability_points(entry.timestamp, *points, *unspent)
            }
            Event::AbilityPointsSpent {
                ability,
                rank,
                cost,
            } => self
                .exp
                .on_ability_spent(entry.timestamp, ability, *rank, *cost),
            Event::Death { victim, killer } => {
                // Your own death strips your buffs. The client logs no fade
                // lines for it, so this is the only chance to notice.
                if victim.is_you() {
                    self.resolver.clear_on_death();
                    // What actually kills you is worth knowing per creature,
                    // and it is the one statistic a death line states outright.
                    if let Some(eqlparser_core::event::Actor::Named(k)) = killer {
                        self.bestiary.on_your_death(entry.timestamp, k);
                    }
                }
                // Everything below is about a creature you killed, so your own
                // death is not it. Without this guard the credit check passed
                // on your death — a damage shield or thorns is enough to mark
                // you as damaged by you — and you were recorded as a creature
                // you had killed. That reached npc_kills and the bestiary, and
                // worst, experience attribution, where a death was counted as
                // a kill.
                //
                // Found by comparing CombatCollector against history.db over
                // the reference log: 580 creatures matched, plus one row for
                // the player. The collector has the check in an else already.
                else {
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
                        // The denominator for every drop this creature did or did
                        // not yield. Counted whether or not the corpse was looted,
                        // which is what makes a sighting count a floor.
                        self.drops.on_kill(&name);
                        // Closes out the damage accumulated into it, which becomes
                        // a lower bound on its health.
                        self.bestiary.on_kill(entry.timestamp, &name);
                        // Difficulty comes from the zone, so use the zone we were
                        // in when it died, not wherever we ended up later.
                        let was_raid_target = self.lockouts.on_kill(
                            entry.timestamp,
                            &name,
                            self.tracker.current_zone(),
                        );
                        // Only raid targets. "Something died" is every other line
                        // in the log and would make the alert worthless.
                        if was_raid_target {
                            self.alerts
                                .on_signal(entry.timestamp, Signal::BossKilled { boss: &name });
                        }
                        // Lets a following "task has been updated" say which step.
                        self.tasks.on_kill(entry.timestamp, &name);
                    }
                    // Sky island nameds are counted regardless of who landed the
                    // blow, on the same participation basis as everything else.
                    if killer.as_ref().is_some_and(|k| k.is_you()) || fought_it {
                        self.sky
                            .on_kill(entry.timestamp, &victim.display(&self.player));
                    }
                }
            }
            Event::ZoneChange { zone } => {
                // Everything the bestiary records is keyed on where you are,
                // and a zone line also ends any fight still in progress.
                self.bestiary.on_zone(zone);
                // Most buffs do not survive a zone line in practice, but rather
                // than drop them we simply let them expire naturally.
                self.resolver.expire(entry.timestamp);
                self.exp.on_zone(entry.timestamp, zone);
                self.npcs.on_zone(zone);
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
            self.trim_timelines();
        }

        // Loot picked up after its fight closed. This runs *after* the tracker
        // sees the line, because the tracker closes a stale encounter lazily on
        // the next event — checking beforehand would still see it as open and
        // the loot would be lost from the per-fight view.
        if matches!(entry.event, Event::Loot { .. }) && self.tracker.current().is_none() {
            if let Some(rec) = self.all_loot.last().cloned() {
                if let Some(last) = self.encounters.last_mut() {
                    let within_grace =
                        rec.timestamp - last.end <= eqlparser_core::encounter::LOOT_GRACE_SECONDS;
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

        // Alerts see the line *after* the trackers have digested it, so a rule
        // and the panels can never disagree about whether something happened.
        // Conclusions no single line states — a charm coming loose, a raid
        // target dying — are raised as signals from the arms above.
        self.note_line(&entry);
    }

    /// Release the per-event timeline of all but the most recent fights.
    ///
    /// Every encounter in the log stays in memory, and a full log holds
    /// thousands of them. Their summaries are small; a per-event list is not.
    /// The fights whose timeline anyone opens are the recent ones, so older
    /// fights keep their totals and give up their detail — and say so, rather
    /// than presenting an empty timeline as a quiet fight.
    fn trim_timelines(&mut self) {
        if self.encounters.len() <= TIMELINE_ENCOUNTERS {
            return;
        }
        let cut = self.encounters.len() - TIMELINE_ENCOUNTERS;
        for e in &mut self.encounters[..cut] {
            if e.timeline_retained {
                e.timeline = Vec::new();
                e.timeline_retained = false;
            }
        }
    }

    /// File every grant still waiting, against the level that explains them.
    ///
    /// `level` is 0 when nothing does. That is not a missing value to be
    /// filled in later: a spell granted on a change of invocation was granted
    /// because the character already met its requirement, so the level it
    /// arrived at says nothing about the level it is given at. Recording a 0
    /// keeps the two cases apart; guessing would merge them.
    fn file_grants(&mut self, _now: i64, level: u32) {
        for (at, spell) in std::mem::take(&mut self.pending_grants) {
            let build = self.build_label(at);
            self.grants.push(crate::store::StoredGrant {
                spell,
                level,
                at,
                build,
            });
        }
    }

    /// Every grant read so far, with the still-unexplained ones filed as
    /// having no level. Called at a flush, where waiting longer is not an
    /// option: the log may simply end here.
    pub fn grants_to_persist(&self) -> Vec<crate::store::StoredGrant> {
        let mut out = self.grants.clone();
        for (at, spell) in &self.pending_grants {
            out.push(crate::store::StoredGrant {
                spell: spell.clone(),
                level: 0,
                at: *at,
                build: self.build_label(*at),
            });
        }
        out
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
        // Hand-made state, not derived: this is a correction nothing can
        // regenerate, so it goes to the config directory rather than beside
        // the rebuildable database. See `crate::paths`.
        crate::paths::config_file("buff-marks.json")
    }

    fn save_marks(&self) {
        let Some(path) = Self::marks_path() else {
            return;
        };
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
        let Some(path) = Self::marks_path() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
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

        // The open encounter is searched alongside the closed ones. Asking for
        // a fight by id and getting nothing back because it is the one you are
        // still in makes this unusable for anything live — which is the moment
        // you most want to know what is hitting you.
        let selected: Vec<&Encounter> = match encounter_id {
            Some(id) => self
                .encounters
                .iter()
                .chain(self.tracker.current())
                .filter(|e| e.id == id)
                .collect(),
            None => self
                .encounters
                .iter()
                .chain(self.tracker.current())
                .collect(),
        };

        for e in selected {
            let Some(you) = e.actors.get("you") else {
                continue;
            };
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
                    avg: if hits > 0 {
                        t as f64 / hits as f64
                    } else {
                        0.0
                    },
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
    pub(crate) fn build_label(&self, at: i64) -> String {
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

    /// The classes believed active at a moment, in canonical order.
    fn current_loadout(&self, at: i64) -> Vec<eqlparser_core::class::Class> {
        let mut trio: Vec<_> = self
            .classes
            .trio_around(at, 24 * 3600)
            .into_iter()
            .map(|c| c.class)
            .collect();
        trio.sort_by_key(|c| c.index());
        trio
    }

    /// The lifetime spend the player read off their character sheet.
    pub fn set_stated_aa_spent(&mut self, spent: u32) {
        self.aa_spent_stated = Some(spent);
    }

    /// Apply levels the player entered, from the history database.
    pub fn set_stated_level(&mut self, class: eqlparser_core::class::Class, level: u32, at: i64) {
        self.class_levels.set_stated(class, level, at);
    }

    /// Tell the bestiary what level the character was before this log opened.
    ///
    /// Nothing in a log states your level; it is only ever learned from a
    /// level-up line. So the first log the daemon reads is fine, and every log
    /// after a roll is not — the client opens a new file mid-session and the
    /// character will not level again for hours, during which every swing
    /// would be filed against level 0.
    ///
    /// That matters more now that these rows persist. Filed at 0 they do not
    /// merely read wrong, they key wrong: `(character, level, stance)` is the
    /// pairing, so the same fights split into an "unknown level" row that no
    /// later log will ever join up with. Seeding from what the store already
    /// knows keeps one pairing one row across a roll.
    pub fn set_known_level(&mut self, level: u32) {
        if level > 0 {
            self.bestiary.on_level(level);
        }
    }

    /// Per-class levels, and whether a `/who` would settle anything.
    pub fn class_level_report(&self) -> eqlparser_core::classlevel::ClassLevelReport {
        self.class_levels.report(
            &self.current_loadout(self.now()),
            &self.classes.classes_ever_declared(),
        )
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
        }

        // Every fight, not only the ones that happened to contain a boss.
        // Encounters used to reach the store solely as a boss kill's companion,
        // which meant the build that fought an ordinary fight was lost on
        // restart and none of it could be searched by class combination.
        for e in self
            .encounters
            .iter()
            .chain(self.tracker.current())
            .filter(|e| e.is_significant())
            .map(|e| self.summarise(e, false))
        {
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
                build: e.build,
                difficulty: e.difficulty,
            });
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

        // The stored rank is the *client* ladder position, not the rank the
        // player has. Nothing here reads it back — the table exists for eqdb,
        // which lines these up against the wiki's per-rank price list, and the
        // wiki numbers a toggle's ladder the way the client does: ten entries
        // for five ranks. Storing the real rank would land every observation
        // on the wrong rung of that ladder.
        let aa = self
            .exp
            .aa_purchases()
            .into_iter()
            .map(|p| (p.ability, p.client_rank, p.spent, p.last_seen))
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

    /// The body type a named spell proves about whatever it just hit.
    ///
    /// A log names a spell without its rank, and ranks are separate rows in the
    /// spell file, so the name resolves to several candidates. Every one of
    /// them has to carry the same restriction before anything is claimed: a
    /// name shared between a restricted spell and an unrestricted one proves
    /// nothing about which was cast, and the whole value of this evidence is
    /// that it is the game's own verdict rather than an inference.
    fn spell_restricts_to(&self, spell: &str) -> Option<CreatureKind> {
        let db = self.resolver.db();
        let ids = db.candidates_for_name(eqlparser_core::spell::base_spell_name(spell));
        let mut kinds = ids
            .iter()
            .filter_map(|id| db.get(*id))
            .map(|s| s.restricted_to);
        let first = kinds.next()??;
        if kinds.all(|k| k == Some(first)) {
            Some(first)
        } else {
            None
        }
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
        // Two of the three failure lines name nobody and no spell, so a text
        // rule on them could not say what failed. This one can.
        self.alerts
            .on_signal(at, Signal::CharmResisted { target, how: kind });
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
            // The live set, not the encounter's `charmed_by_you` flag: that one
            // is sticky on purpose, so reading it here listed every creature
            // charmed during the fight and cleared none of them on a break.
            charmed: self.tracker.charmed_now(),
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
            // The client rank, because the `/alt list` ladder is numbered that
            // way — a toggle's rank 5 sits at position 10 in the dump.
            .map(|p| (p.ability.to_lowercase(), (p.client_rank, p.spent)))
            .collect();
        self.aa.report(&owned)
    }

    /// Captured NPC interactions, for writing quests up.
    pub fn npc_report(&self) -> NpcReport {
        self.npcs.report()
    }

    /// Private conversations, threaded by correspondent.
    pub fn tell_report(&self) -> TellReport {
        self.tells.report()
    }

    /// Plane of Sky quest progress, flagged against the character's classes.
    /// Sky turn-ins seen in this log, for the history database.
    pub fn sky_turn_ins(&self) -> Vec<(String, i64)> {
        self.sky.turn_in_log()
    }

    /// Fold stored turn-in counts into a report.
    ///
    /// The tracker only knows the log in front of it. Counts kept in history
    /// span every log ever parsed, so where history knows more it wins — a
    /// quest turned in five times last week should not read as new because
    /// the log rotated.
    pub fn apply_stored_turn_ins(report: &mut SkyReport, stored: &[(String, u32, i64)]) {
        for q in report.quests.iter_mut() {
            if let Some((_, count, last)) = stored.iter().find(|(n, _, _)| n == &q.name) {
                if *count > q.completed {
                    q.completed = *count;
                    q.last_completed = Some(*last);
                }
            }
        }
    }

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
        // Hand-made state, not derived: this is a correction nothing can
        // regenerate, so it goes to the config directory rather than beside
        // the rebuildable database. See `crate::paths`.
        crate::paths::config_file("sky-quests.json")
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
            Err(_) => {
                eprintln!("[eqlparserd] sky-quests.json is unreadable; using the built-in table")
            }
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
        // Hand-made state, not derived: this is a correction nothing can
        // regenerate, so it goes to the config directory rather than beside
        // the rebuildable database. See `crate::paths`.
        crate::paths::config_file("tasks.json")
    }

    fn save_task_defs(&self) {
        let Some(path) = Self::task_defs_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.tasks.definitions()) {
            let _ = std::fs::write(&path, json);
        }
    }

    fn load_task_defs(&mut self) {
        let Some(path) = Self::task_defs_path() else {
            return;
        };
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
        let mut r = self.trades.report();
        // The tracker knows nothing about which game it is reading; the cost
        // totals mean different things in the two, so the tag is stamped on
        // here where it is known.
        r.game = self.game();
        r
    }

    /// Where items were seen coming from, and how often.
    pub fn drop_report(&self) -> DropReport {
        self.drops.report()
    }

    /// Creature profiles, keyed by zone, mode and difficulty.
    pub fn bestiary_report(&self) -> BestiaryReport {
        self.bestiary.report()
    }

    /// Every health measurement this log produced, one row per death.
    ///
    /// Separate from the report because it is evidence rather than a summary:
    /// the store keeps these so a median can be taken across every log, which
    /// no amount of per-log spreads would allow.
    pub fn bestiary_health_samples(&self) -> Vec<HealthSample> {
        self.bestiary.health_samples()
    }

    /// Which game this engine's log is from.
    pub fn game(&self) -> Game {
        self.drops.game()
    }

    /// Everything known about the items this log has touched, gathered into
    /// one record per item.
    ///
    /// This is the shape a reference site wants: an item, what makes it, what
    /// it goes into, what drops it, what it sells for — with the observed
    /// parts kept apart from the curated ones, so a reader can always tell a
    /// recipe someone typed in from a drop this character actually saw.
    pub fn item_database(&self) -> ItemDatabase {
        let book = eqlparser_core::recipe::RecipeBook::embedded();
        let trades = self.trades.report();
        let drops = self.drops.report();

        let game = drops.game;
        let mut items: std::collections::BTreeMap<String, ItemEntry> =
            std::collections::BTreeMap::new();
        macro_rules! entry {
            ($name:expr) => {
                items
                    .entry($name.to_lowercase())
                    .or_insert_with(|| ItemEntry {
                        item: $name.to_string(),
                        game,
                        ..Default::default()
                    })
            };
        }

        // Curated: every recipe in the book, indexed both ways — what makes
        // this item, and what this item goes into.
        for r in &book.recipes {
            entry!(r.product).made_by.push(r.clone());
            for c in r.components.iter().flatten() {
                let u = entry!(c.item);
                if !u.used_in.iter().any(|p| p.eq_ignore_ascii_case(&r.product)) {
                    u.used_in.push(r.product.clone());
                }
            }
        }
        for t in &book.tools {
            entry!(t).tool = true;
        }

        // Observed: drops, merchant prices, and combines attempted.
        for d in &drops.items {
            entry!(d.item).dropped_by = d.sources.clone();
        }
        for p in &trades.prices {
            let e = entry!(p.item);
            e.merchant_copper = Some(p.unit_copper);
            e.bought = p.bought;
            e.sold_by = p.vendors.clone();
        }
        for c in &trades.crafts {
            let e = entry!(c.item);
            e.combines = c.attempts() as u64;
            e.combine_successes = c.successes as u64;
            if e.skill.is_none() {
                e.skill = c.skill.clone();
            }
        }
        for cu in &trades.components {
            entry!(cu.item).consumed = cu.used;
        }

        let mut items: Vec<ItemEntry> = items.into_values().collect();
        for e in &mut items {
            if e.skill.is_none() {
                e.skill = e.made_by.first().map(|r| r.skill.clone());
            }
            e.used_in.sort();
        }
        items.sort_by(|a, b| a.item.to_lowercase().cmp(&b.item.to_lowercase()));

        ItemDatabase {
            game: drops.game,
            character: self.player.clone(),
            items,
            kills: drops.kills,
        }
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
                let mid = r
                    .ended
                    .map_or(r.started, |e| r.started + (e - r.started) / 2);
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
            // Earned is short by the same ranks spent before the log began, so
            // it follows the stated total rather than drifting from it.
            aa_earned: match self.aa_spent_stated {
                Some(spent) => spent + self.exp.aa_unspent(),
                None => self.exp.aa_earned(),
            },
            aa_unspent: self.exp.aa_unspent(),
            aa_spent: self.aa_spent_stated.unwrap_or(self.exp.aa_spent_total()),
            aa_spent_tracked: self.exp.aa_spent_total(),
            aa_spent_untracked: self
                .aa_spent_stated
                .unwrap_or_default()
                .saturating_sub(self.exp.aa_spent_total()),
            aa_unlogged: self.exp.aa_unlogged().0,
            aa_unlogged_cost_max: self.exp.aa_unlogged().1,
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

    pub fn session_start(&self) -> i64 {
        self.session_start
    }

    /// Every fight of the current session as one meter, plus the fight in
    /// progress.
    ///
    /// The same panels the dashboard shows for a single pull, asked of the
    /// whole session — which is the only way to see what a build actually does
    /// over an evening rather than over one lucky burn.
    ///
    /// Rate is per second **of combat**, not of wall clock. An evening is
    /// mostly running, buffing and sitting; dividing an evening's damage by an
    /// evening's seconds produces a number that describes nothing. The count
    /// of fights and the combat time come back alongside so the UI can say
    /// what the denominator was.
    pub fn session_summary(&self, zone_only: bool) -> (EncounterSummary, usize, i64) {
        let zone = self.tracker.current_zone();
        let in_session = |e: &&Encounter| {
            e.start >= self.session_start && (!zone_only || e.zone.as_deref() == zone)
        };
        let mut merged = Encounter {
            id: 0,
            zone: zone.map(str::to_string),
            start: 0,
            end: 0,
            actors: std::collections::HashMap::new(),
            deaths: Vec::new(),
            loot: Vec::new(),
            coin_copper: 0,
            resists: std::collections::HashMap::new(),
            timeline: Vec::new(),
            // A merged view spans many fights, so a mark from one of them
            // would sit at a moment the merged chart does not have.
            marks: Vec::new(),
            timeline_dropped: 0,
            timeline_retained: false,
        };
        let mut fights = 0usize;
        let mut combat = 0i64;
        for e in self.encounters.iter().filter(in_session) {
            merged.merge_from(e);
            fights += 1;
            combat += e.duration();
        }
        if let Some(open) = self.tracker.current().filter(|e| in_session(&e)) {
            merged.merge_from(open);
            fights += 1;
            combat += open.duration();
        }

        let combat = combat.max(1);
        let mut summary = self.summarise(&merged, true);
        // Re-base every rate on combat time. `summarise` divided by the span
        // from the first pull to the last, which includes everything that was
        // not a fight.
        let span = merged.duration().max(1) as f64;
        let scale = span / combat as f64;
        summary.duration = combat;
        summary.your_dps *= scale;
        summary.your_hps *= scale;
        for row in &mut summary.meter {
            row.dps *= scale;
            row.hps *= scale;
        }
        summary.title = match (zone_only, zone) {
            (true, Some(z)) => z.to_string(),
            (true, None) => "this zone".to_string(),
            _ => "This session".to_string(),
        };
        (summary, fights, combat)
    }

    /// Coin taken from corpses during the current session.
    pub fn session_coin(&self) -> u64 {
        self.encounters
            .iter()
            .filter(|e| e.start >= self.session_start)
            .map(|e| e.coin_copper)
            .sum()
    }

    /// Aggregate every looted item across the log, most recent first.
    /// The whole log's loot. The server always asks for a window, so this
    /// unbounded form exists for the tests.
    #[cfg(test)]
    pub fn loot_tally(&self) -> Vec<LootTally> {
        self.loot_tally_since(None)
    }

    /// Aggregate looted items, most recent first.
    ///
    /// `since` bounds it to one play session; `None` covers the whole log.
    /// Everything known about one item: who dropped it, how often against how
    /// many kills, where, and what it was worth.
    ///
    /// The drop rate's denominator is deaths of that creature inside your own
    /// encounters. That is the honest denominator available — it counts corpses
    /// you were present for, which is what a drop rate from a personal log can
    /// mean. It is not the game's underlying loot table, and a corpse left
    /// unlooted still counts as a kill, so a rate reads low rather than wrong.
    pub fn loot_detail(&self, item: &str) -> Option<LootDetail> {
        use eqlparser_core::encounter::LootFate;
        use std::collections::HashMap;

        let wanted = item.to_lowercase();
        let drops: Vec<_> = self
            .all_loot
            .iter()
            .filter(|l| l.item.to_lowercase() == wanted)
            .collect();
        if drops.is_empty() {
            return None;
        }

        // Deaths per creature across every fight, built once rather than
        // rescanned for each source.
        let mut kills: HashMap<String, u64> = HashMap::new();
        for enc in self.encounters.iter().chain(self.tracker.current()) {
            for d in &enc.deaths {
                if !d.victim.eq_ignore_ascii_case(&self.player) {
                    *kills.entry(d.victim.to_lowercase()).or_default() += 1;
                }
            }
        }

        let mut by_mob: HashMap<String, LootSource> = HashMap::new();
        let mut zones: HashMap<String, u64> = HashMap::new();
        let mut stacks: HashMap<u64, u64> = HashMap::new();
        let (mut kept, mut sold, mut stored, mut transmuted) = (0u64, 0u64, 0u64, 0u64);
        let (mut sold_total, mut sales, mut best_sale) = (0u64, 0u64, 0u64);

        for l in &drops {
            *stacks.entry(l.quantity.max(1)).or_default() += 1;
            match l.fate {
                LootFate::Kept => kept += l.quantity,
                LootFate::Sold => sold += l.quantity,
                LootFate::Stored => stored += l.quantity,
                LootFate::Transmuted => transmuted += l.quantity,
            }
            if let Some(c) = l.sold_for_copper {
                sold_total += c;
                sales += 1;
                best_sale = best_sale.max(c);
            }
            let zone = self
                .encounter_containing(l.timestamp)
                .and_then(|e| e.zone.clone());
            if let Some(z) = &zone {
                *zones.entry(z.clone()).or_default() += 1;
            }
            if let Some(src) = &l.source {
                let e = by_mob
                    .entry(src.to_lowercase())
                    .or_insert_with(|| LootSource {
                        mob: src.clone(),
                        drops: 0,
                        quantity: 0,
                        kills: 0,
                        rate: None,
                        zones: Vec::new(),
                        last: 0,
                    });
                e.drops += 1;
                e.quantity += l.quantity;
                e.last = e.last.max(l.timestamp);
                if let Some(z) = zone {
                    if !e.zones.contains(&z) {
                        e.zones.push(z);
                    }
                }
            }
        }

        let mut sources: Vec<LootSource> = by_mob
            .into_iter()
            .map(|(key, mut s)| {
                s.kills = kills.get(&key).copied().unwrap_or(0);
                s.rate = (s.kills > 0).then(|| s.drops as f64 / s.kills as f64);
                s.zones.sort();
                s
            })
            .collect();
        sources.sort_by(|a, b| b.drops.cmp(&a.drops).then_with(|| a.mob.cmp(&b.mob)));

        let mut zones: Vec<(String, u64)> = zones.into_iter().collect();
        zones.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut stacks: Vec<(u64, u64)> = stacks.into_iter().collect();
        stacks.sort_by_key(|s| s.0);

        Some(LootDetail {
            item: drops[0].item.clone(),
            total: drops.iter().map(|l| l.quantity).sum(),
            drops: drops.len() as u64,
            first_seen: drops.iter().map(|l| l.timestamp).min().unwrap_or(0),
            last_seen: drops.iter().map(|l| l.timestamp).max().unwrap_or(0),
            sources,
            zones,
            kept,
            sold,
            stored,
            transmuted,
            sold_total_copper: sold_total,
            sales,
            best_sale_copper: best_sale,
            buy_price_copper: self
                .trades
                .report()
                .prices
                .iter()
                .find(|p| p.item.eq_ignore_ascii_case(item))
                .map(|p| p.unit_copper),
            stacks,
        })
    }

    pub fn loot_tally_since(&self, since: Option<i64>) -> Vec<LootTally> {
        use std::collections::HashMap;
        let mut map: HashMap<String, LootTally> = HashMap::new();
        {
            for l in self
                .all_loot
                .iter()
                .filter(|l| since.is_none_or(|s| l.timestamp >= s))
            {
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
                swings_by_flag: {
                    // Crit flags are already reported on their own, and a
                    // "Critical" here would sit beside Flurry as though they
                    // answered the same question.
                    let mut v: Vec<(String, u64)> = s
                        .swings_by_flag
                        .iter()
                        .filter(|(f, _)| {
                            !["Critical", "Crippling Blow", "Deadly Strike", "Slay Undead"]
                                .iter()
                                .any(|k| f.eq_ignore_ascii_case(k))
                        })
                        .map(|(f, n)| (f.clone(), *n))
                        .collect();
                    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                    v
                },
                // Two ledgers of the same shape: what happened to the swings
                // this actor threw, and to the swings thrown at it. A plain
                // miss is kept apart from a parry on purpose — one is failing
                // to connect, the other is the target succeeding, and a
                // defence that eats a third of your swings is a different
                // problem from poor accuracy.
                offence_outcomes: {
                    let stopped: u64 = s.swings_stopped_by.values().sum();
                    let mut v = vec![
                        ("landed".to_string(), s.hits),
                        ("missed".to_string(), s.misses.saturating_sub(stopped)),
                    ];
                    v.extend(s.swings_stopped_by.iter().map(|(k, n)| (k.clone(), *n)));
                    v.retain(|(_, n)| *n > 0);
                    v
                },
                defence_outcomes: {
                    let avoided: u64 = s.avoided_by.values().sum();
                    let mut v = vec![
                        ("landed".to_string(), s.damage_taken_hits()),
                        (
                            "missed".to_string(),
                            s.attacks_missed.saturating_sub(avoided),
                        ),
                    ];
                    v.extend(s.avoided_by.iter().map(|(k, n)| (k.clone(), *n)));
                    v.retain(|(_, n)| *n > 0);
                    v
                },
                melee_swings: s
                    .hits_by_kind
                    .get("melee")
                    .copied()
                    .unwrap_or(0)
                    + s.misses,
                crit_by_kind: s
                    .crit_breakdown()
                    .into_iter()
                    .map(|(k, h, c, r)| (k.to_string(), h, c, r))
                    .collect(),
                crit_by_flag: s.crit_causes(),
                heals: s.heals,
                heal_crits: s.heal_crits,
                heal_crit_rate: s.heal_crit_rate(),
                over_healing: s.over_healing,
                healing_attempted: s.healing_attempted,
                over_heal_measured: s.heals_measured_for_overheal,
                accuracy: s.accuracy(),
                is_you: key == "you",
                is_enemy: s.damaged_by_you && !s.charmed_by_you && !s.summoned,
                is_pet: s.is_yours(),
                name_shared: s.name_shared,
                last_action: s.last_action,
                stunned_seconds: s.stunned_seconds,
                stuns: s.stuns,
                stuns_avoided: s.stuns_avoided,
                pet_kind: match (s.summoned, s.charm_seen, s.is_yours()) {
                    (true, _, _) => Some("summoned"),
                    (false, true, _) => Some("charmed"),
                    // Ours, but the log never said how. Saying "charmed" here
                    // would invent a spell that was never cast.
                    (false, false, true) => Some("pet"),
                    _ => None,
                },
                pet_owner: s.owner.clone(),
                // What this pet contributed to your total, which for a charmed
                // mob is only the part it dealt while held and for anything
                // else of yours is all of it. Reading `damage_while_charmed`
                // directly reported zero for a summoned pet. Gated on the row
                // being a pet at all, since the figure is meaningless — and
                // equal to the whole of its output — for an enemy.
                pet_damage: if s.is_yours() { s.credited_damage() } else { 0 },
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
            // From the fight's own midpoint: a fight that straddles a respec is
            // labelled whichever build it mostly was, matching how levels do it.
            build: self.build_label(enc.start + (enc.end - enc.start) / 2),
            difficulty: enc
                .zone
                .as_deref()
                .map(eqlparser_core::lockout::difficulty_of)
                .unwrap_or(0),
            meter: if with_meter {
                self.meter_for(enc)
            } else {
                Vec::new()
            },
            // These travel with the meter: they are detail-view data, and
            // computing them for every row of a thousand-fight list would cost
            // more than the list itself.
            outgoing_groups: if with_meter {
                Self::group_outgoing(enc)
            } else {
                Vec::new()
            },
            resists: if with_meter {
                enc.resist_tallies()
            } else {
                Vec::new()
            },
            mobs: if with_meter {
                enc.per_mob()
            } else {
                Vec::new()
            },
        }
    }

    /// Split your output into Spell / Melee / Other, biggest group first and
    /// biggest ability first within each.
    ///
    /// Thirty ability rows in one flat list does not answer "where is my damage
    /// coming from"; three groups do, and the detail is still there inside them.
    fn group_outgoing(enc: &Encounter) -> Vec<AbilityGroup> {
        let Some(you) = enc.actors.get("you") else {
            return Vec::new();
        };
        let mut groups: std::collections::HashMap<&'static str, Vec<GroupedAbility>> =
            std::collections::HashMap::new();
        for (name, a) in &you.by_ability {
            if a.total == 0 {
                continue;
            }
            groups
                .entry(eqlparser_core::encounter::ability_group(
                    &a.kind,
                    a.self_inflicted,
                ))
                .or_default()
                .push(GroupedAbility {
                    name: name.clone(),
                    damage: a.total,
                    hits: a.hits,
                    crits: a.crits,
                    max: a.max,
                    average: a.average(),
                    casts: a.casts,
                    share: 0.0,
                });
        }
        let total: u64 = you.by_ability.values().map(|a| a.total).sum();
        let mut out: Vec<AbilityGroup> = groups
            .into_iter()
            .map(|(group, mut abilities)| {
                abilities.sort_by(|a, b| b.damage.cmp(&a.damage).then_with(|| a.name.cmp(&b.name)));
                let damage: u64 = abilities.iter().map(|a| a.damage).sum();
                for a in abilities.iter_mut() {
                    a.share = if damage == 0 {
                        0.0
                    } else {
                        a.damage as f64 / damage as f64
                    };
                }
                AbilityGroup {
                    group: group.to_string(),
                    damage,
                    share: if total == 0 {
                        0.0
                    } else {
                        damage as f64 / total as f64
                    },
                    abilities,
                }
            })
            .collect();
        out.sort_by_key(|g| std::cmp::Reverse(g.damage));
        out
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
        let buffs = self
            .resolver
            .active_buffs(now)
            .into_iter()
            .map(to_info)
            .collect();
        let auras = self
            .resolver
            .active_auras(now)
            .into_iter()
            .map(to_info)
            .collect();
        let debuffs = self
            .resolver
            .active_debuffs(now)
            .into_iter()
            .map(to_info)
            .collect();

        // Include the fight in progress. Counting only completed encounters
        // leaves every headline figure frozen for the whole of a long boss
        // fight — exactly when they are being watched.
        let in_progress = self.tracker.current();
        let mut totals = Totals {
            encounters: self.encounters.len() + usize::from(in_progress.is_some()),
            ..Default::default()
        };
        totals.sessions = self.sessions();
        totals.played_seconds = self.played_seconds();
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
            level: {
                let r = self.class_level_report();
                r.effective.unwrap_or_else(|| self.resolver.level())
            },
            levelling: self.class_level_report().levelling,
            zone: self.tracker.current_zone().map(str::to_string),
            classes,
            current: self.tracker.current().map(|e| self.summarise(e, true)),
            recent,
            session_motes: {
                // Cheapest tier first, which is the order they are earned in
                // and the order the desktop panel uses.
                let mut v: Vec<(String, u64)> =
                    self.session_motes.iter().map(|(k, n)| (k.clone(), *n)).collect();
                v.sort_by(|a, b| mote_tier(&a.0).cmp(&mote_tier(&b.0)).then(a.0.cmp(&b.0)));
                v
            },
            session_motes_total: self.session_motes.values().sum(),
            session_seconds: (self.last_timestamp - self.session_start).max(0),
            buffs,
            auras,
            debuffs,
            exp: ExpSummary {
                percent: self.exp.current_percent(),
                kills: self.exp.current_record().kills,
                rate_per_hour: self.exp.current_rate_per_hour(now),
                seconds_to_next_level: self.exp.seconds_to_next_level(now),
                // Same correction as the full report: earned is short by the
                // ranks bought before the log, so it follows the stated total.
                aa_earned: match self.aa_spent_stated {
                    Some(spent) => spent + self.exp.aa_unspent(),
                    None => self.exp.aa_earned(),
                },
                aa_spent: self.aa_spent_stated.unwrap_or(self.exp.aa_spent_total()),
                aa_unspent: self.exp.aa_unspent(),
                aa_per_hour: self.exp.aa_per_hour(now, AA_RATE_WINDOW),
                aa_potion_charges: self.exp.aa_potion_charges(),
                aa_potion_active: self.exp.aa_potion_likely_active(),
            },
            totals,
            log_time: self.last_timestamp,
            live: self.live,
            alerts: self
                .alerts
                .since(self.alerts.seq().saturating_sub(SNAPSHOT_ALERTS)),
            alert_seq: self.alerts.seq(),
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

    /// The log states the effect before the cause.
    ///
    /// Three grants, then the line saying which level they came with. Filing
    /// each grant as it is read would attribute all three to level 3 -- the
    /// level the character was still on -- and be wrong by one everywhere.
    #[test]
    fn a_grant_is_filed_against_the_level_that_follows_it() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 15:03:13 2026] You have been granted the following spell: Gate.",
                "[Tue Jul 28 15:03:13 2026] You have been granted the following spell: Invisibility.",
                "[Tue Jul 28 15:03:13 2026] Your spellbook has been updated!",
                "[Tue Jul 28 15:03:13 2026] You have gained a level! Welcome to level 4!",
            ],
        );
        let grants = e.grants_to_persist();
        assert_eq!(grants.len(), 2);
        assert!(grants.iter().all(|g| g.level == 4), "{grants:?}");
        assert_eq!(grants[0].spell, "Gate");
    }

    /// A change of invocation hands over every spell the new classes already
    /// qualify for -- two dozen at once, with no level gained. Those are
    /// grants too, and the missing level is the answer rather than a gap:
    /// they say the spell is given, and nothing about when.
    #[test]
    fn a_grant_with_no_level_up_keeps_no_level() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 20:32:07 2026] You have been granted the following spell: Snare.",
                "[Tue Jul 28 20:32:07 2026] You have been granted the following spell: Harmony.",
                "[Tue Jul 28 20:32:08 2026] Your spellbook has been updated!",
                // Far enough past for no level-up to be coming.
                "[Tue Jul 28 20:33:00 2026] You slash a giant spider for 10 points of damage.",
            ],
        );
        let grants = e.grants_to_persist();
        assert_eq!(grants.len(), 2);
        assert!(grants.iter().all(|g| g.level == 0), "{grants:?}");
    }

    /// Memorising a spell into a gem is not being given one.
    #[test]
    fn memorising_a_spell_is_not_a_grant() {
        let mut e = engine();
        feed(
            &mut e,
            &["[Tue Jul 28 15:00:00 2026] You have finished memorizing Gate."],
        );
        assert!(e.grants_to_persist().is_empty());
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
        assert_eq!(
            s.totals.your_damage, 507,
            "closed 500 + open 7, counted once each"
        );
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
    /// phrase — the exact ambiguity the real data contains — plus a pair of
    /// body-restricted spells for the bestiary.
    fn charm_db() -> SpellDb {
        use eqlparser_core::class::{CLASS_CANNOT_CAST, CLASS_COUNT};
        use eqlparser_core::spell::Spell;
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
            restricted_to: None,
            linked: Vec::new(),
        };
        let restricted = |id: u32, name: &str, to: CreatureKind| Spell {
            restricted_to: Some(to),
            ..mk(id, name, false)
        };
        let mut m = std::collections::HashMap::new();
        m.insert(1, mk(1, "Solon's Bewitching Bravura", true));
        m.insert(2, mk(2, "Crission's Pixie Strike", false));
        m.insert(3, restricted(3, "Expel Undead", CreatureKind::Undead));
        // Two ranks of one spell, as the real file holds them: the log names
        // neither rank, so both have to agree before anything is claimed.
        m.insert(4, restricted(4, "Dismiss Summoned", CreatureKind::Summoned));
        m.insert(
            5,
            restricted(5, "Dismiss Summoned Rk. II", CreatureKind::Summoned),
        );
        // A name shared by a restricted spell and an unrestricted one, which
        // must therefore prove nothing.
        m.insert(6, restricted(6, "Ambiguous Word", CreatureKind::Undead));
        m.insert(7, mk(7, "Ambiguous Word", false));
        SpellDb::from_spells(m)
    }

    /// The body type of the 43 creatures this finds in the shipped logs comes
    /// entirely from lines like these: the game refuses to cast Expel Undead at
    /// anything that is not undead, so one landing is its own proof.
    #[test]
    fn a_body_restricted_spell_names_what_it_hit() {
        let mut e = Engine::new("Tharn".to_string(), charm_db());
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You have entered West Commonlands.",
                "[Tue Jul 28 14:00:01 2026] You hit a wan ghoul knight for 319 points of magic damage by Expel Undead.",
                // An ordinary sword swing says nothing about a body.
                "[Tue Jul 28 14:00:02 2026] You slash a giant spider for 10 points of damage.",
            ],
        );
        let npcs = e.bestiary_report().npcs;
        let ghoul = npcs
            .iter()
            .find(|n| n.name.eq_ignore_ascii_case("wan ghoul knight"))
            .expect("the ghoul was damaged, so it is in the bestiary");
        assert_eq!(ghoul.kind, Some(CreatureKind::Undead));
        let spider = npcs
            .iter()
            .find(|n| n.name.eq_ignore_ascii_case("giant spider"))
            .expect("the spider too");
        assert_eq!(spider.kind, None, "nothing here proved anything about it");
    }

    /// Someone else's cast is the game's verdict just as much as your own, and
    /// a log carries a group's spells as well as yours.
    #[test]
    fn another_caster_proves_it_just_as_well() {
        let mut e = Engine::new("Tharn".to_string(), charm_db());
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You have entered West Commonlands.",
                "[Tue Jul 28 14:00:01 2026] Xeker hit a dar ghoul knight for 40 points of magic damage by Expel Undead.",
            ],
        );
        let npcs = e.bestiary_report().npcs;
        let ghoul = npcs
            .iter()
            .find(|n| n.name.eq_ignore_ascii_case("dar ghoul knight"))
            .expect("in the bestiary");
        assert_eq!(ghoul.kind, Some(CreatureKind::Undead));
    }

    /// A log names a spell without its rank, and ranks are separate rows. Every
    /// candidate must agree, or the line proves nothing about which was cast.
    #[test]
    fn a_name_shared_with_an_unrestricted_spell_proves_nothing() {
        let mut e = Engine::new("Tharn".to_string(), charm_db());
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You have entered West Commonlands.",
                // Every rank of this one is summoned-only, so it counts.
                "[Tue Jul 28 14:00:01 2026] You hit an air elemental for 30 points of magic damage by Dismiss Summoned.",
                // This name belongs to both a restricted spell and a plain one.
                "[Tue Jul 28 14:00:02 2026] You hit a hill giant for 30 points of magic damage by Ambiguous Word.",
            ],
        );
        let npcs = e.bestiary_report().npcs;
        let elemental = npcs
            .iter()
            .find(|n| n.name.eq_ignore_ascii_case("air elemental"))
            .expect("in the bestiary");
        assert_eq!(elemental.kind, Some(CreatureKind::Summoned));
        let giant = npcs
            .iter()
            .find(|n| n.name.eq_ignore_ascii_case("hill giant"))
            .expect("in the bestiary");
        assert_eq!(giant.kind, None, "the cast spell cannot be identified");
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

    /// The client announces each logon, so a session boundary is stated
    /// rather than inferred.
    ///
    /// Asserted as offsets: the engine reads the log's naive local time, so a
    /// hardcoded epoch would only be right in one timezone.
    #[test]
    fn a_logon_starts_a_session() {
        let mut e = engine();
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] You slash a rat for 10 points of damage."],
        );
        let opened = e.session_start();
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:05 2026] Welcome to EverQuest Legends!"],
        );
        assert_eq!(e.session_start(), opened + 5, "the logon moves it");
    }

    /// A relog two minutes later is still a new session: the marker says so,
    /// and a gap that short would never have been noticed.
    #[test]
    fn a_quick_relog_starts_a_new_session() {
        let mut e = engine();
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] Welcome to EverQuest Legends!"],
        );
        let first = e.session_start();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:10 2026] You slash a rat for 10 points of damage.",
                "[Tue Jul 28 14:02:00 2026] Welcome to EverQuest Legends!",
            ],
        );
        assert_eq!(e.session_start(), first + 120, "the later logon wins");
    }

    /// A log that begins part-way through a session has no marker behind it,
    /// so a long silence still has to serve.
    #[test]
    fn without_a_logon_a_long_gap_still_starts_a_session() {
        let mut e = engine();
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] You slash a rat for 10 points of damage."],
        );
        let opened = e.session_start();
        feed(
            &mut e,
            &["[Tue Jul 28 16:00:00 2026] You slash a rat for 10 points of damage."],
        );
        assert_eq!(e.session_start(), opened + 7200, "the gap starts a session");
    }

    /// Once a logon has been seen, a quiet stretch must not invent a boundary
    /// the client never announced — idling is not logging out.
    #[test]
    fn after_a_logon_a_quiet_stretch_does_not_split_the_session() {
        let mut e = engine();
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] Welcome to EverQuest Legends!"],
        );
        let logon = e.session_start();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:10 2026] You slash a rat for 10 points of damage.",
                "[Tue Jul 28 16:00:00 2026] You slash a rat for 10 points of damage.",
            ],
        );
        assert_eq!(e.session_start(), logon, "still the logon");
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
        assert!(
            !imp.is_enemy,
            "charm should override the enemy classification"
        );
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
        assert!(
            (you.heal_share - 1.0).abs() < 1e-9,
            "you did all the healing"
        );
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

    /// A rate needs a denominator, and the log's span is not it: it counts the
    /// days the game was closed. These count only the stretches being written.
    #[test]
    fn sessions_and_played_time_skip_the_gaps() {
        let mut e = engine();
        // Ten minutes of play.
        e.push_line("[Tue Jul 28 12:00:00 2026] You slash a rat for 1 point of damage.");
        e.push_line("[Tue Jul 28 12:10:00 2026] You slash a rat for 1 point of damage.");
        assert_eq!(e.sessions(), 1);
        assert_eq!(e.played_seconds(), 600);

        // A short pause is the same session; the gap threshold is 30 minutes.
        e.push_line("[Tue Jul 28 12:25:00 2026] You slash a rat for 1 point of damage.");
        assert_eq!(e.sessions(), 1);
        assert_eq!(e.played_seconds(), 1_500);

        // Two days off, then five more minutes: a second session, and none of
        // the two days counted.
        e.push_line("[Thu Jul 30 09:00:00 2026] You slash a rat for 1 point of damage.");
        e.push_line("[Thu Jul 30 09:05:00 2026] You slash a rat for 1 point of damage.");
        assert_eq!(e.sessions(), 2);
        assert_eq!(e.played_seconds(), 1_500 + 300, "the gap is not play");

        // And a logon partway through does not stop the accounting: the
        // client stating its own boundary is about where a session starts,
        // not about what counts as time played.
        e.push_line("[Thu Jul 30 09:06:00 2026] Welcome to EverQuest!");
        e.push_line("[Fri Jul 31 20:00:00 2026] You slash a rat for 1 point of damage.");
        e.push_line("[Fri Jul 31 20:02:00 2026] You slash a rat for 1 point of damage.");
        assert_eq!(e.sessions(), 3, "a day and a half off is another sitting");
        assert_eq!(
            e.played_seconds(),
            1_500 + 300 + 60 + 120,
            "still only the short gaps"
        );
    }

    /// Dying is not killing something. A damage shield or thorns marks you as
    /// damaged by you, which used to be enough for your own death to pass the
    /// participation check and record you as a creature you had killed —
    /// reaching npc_kills, the bestiary, and experience attribution.
    #[test]
    fn your_own_death_is_not_a_kill() {
        let mut e = engine();
        for line in [
            // The bestiary is keyed per zone, so it records nothing until one
            // is known.
            "[Tue Jul 28 11:59:59 2026] You have entered Nagafen's Lair.",
            "[Tue Jul 28 12:00:00 2026] A fire giant hits YOU for 100 points of damage.",
            "[Tue Jul 28 12:00:01 2026] A fire giant is pierced by YOUR thorns for 12 points of non-melee damage.",
            "[Tue Jul 28 12:00:02 2026] You have been slain by a fire giant!",
        ] {
            e.push_line(line);
        }
        assert!(
            !e.bestiary_report().npcs.iter().any(|c| c.name == "Tharn"),
            "the player is not a creature you killed: {:?}",
            e.bestiary_report()
                .npcs
                .iter()
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        // What killed you is still recorded — under the canonical name, which
        // drops the client's article.
        let npcs = e.bestiary_report().npcs;
        assert_eq!(
            npcs.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["fire giant"],
            "the giant, and only the giant"
        );
        assert_eq!(npcs[0].deaths_to_it, 1);
        assert_eq!(npcs[0].kills, 0, "you did not kill it; it killed you");
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

    /// The whole point of the session view: rates over time actually spent
    /// fighting, not over an evening that was mostly walking.
    #[test]
    fn session_meter_rates_by_combat_time_not_wall_clock() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                // Two ten-second fights, five minutes apart.
                "[Tue Jul 28 14:00:00 2026] You slash a giant for 500 points of damage.",
                "[Tue Jul 28 14:00:10 2026] You slash a giant for 500 points of damage.",
                "[Tue Jul 28 14:00:11 2026] You have slain a giant!",
                "[Tue Jul 28 14:05:00 2026] You slash a rat for 500 points of damage.",
                "[Tue Jul 28 14:05:10 2026] You slash a rat for 500 points of damage.",
                "[Tue Jul 28 14:05:11 2026] You have slain a rat!",
                // A third line closes the second fight.
                "[Tue Jul 28 14:20:00 2026] You have entered Freeport.",
            ],
        );
        let (summary, fights, combat) = e.session_summary(false);
        assert_eq!(fights, 2);
        assert_eq!(summary.your_damage, 2000);
        // Roughly 11 seconds of fighting each, not the five minutes between.
        assert!(
            (20..=30).contains(&combat),
            "combat seconds should cover the fights only, got {combat}"
        );
        assert_eq!(summary.duration, combat);
        let expected = 2000.0 / combat as f64;
        assert!(
            (summary.your_dps - expected).abs() < 1.0,
            "dps {} should be near {expected}",
            summary.your_dps
        );
        // The meter rows must agree with the headline.
        let you = summary.meter.iter().find(|r| r.is_you).expect("your row");
        assert!((you.dps - summary.your_dps).abs() < 1.0);
    }

    /// Merging must add fights together rather than let the last one win.
    #[test]
    fn session_meter_sums_every_fight() {
        let mut e = engine();
        feed(
            &mut e,
            &[
                "[Tue Jul 28 14:00:00 2026] You slash a giant for 100 points of damage.",
                "[Tue Jul 28 14:00:01 2026] A giant hits YOU for 30 points of damage.",
                "[Tue Jul 28 14:00:02 2026] You have slain a giant!",
                "[Tue Jul 28 14:10:00 2026] You slash a giant for 400 points of damage.",
                "[Tue Jul 28 14:10:01 2026] A giant hits YOU for 70 points of damage.",
            ],
        );
        let (summary, fights, _) = e.session_summary(false);
        // One closed fight plus the one still open.
        assert_eq!(fights, 2);
        assert_eq!(summary.your_damage, 500);
        assert_eq!(summary.your_damage_taken, 100);
        // The giant is one actor across both fights, not two rows.
        let giants = summary
            .meter
            .iter()
            .filter(|r| r.name.contains("giant"))
            .count();
        assert_eq!(giants, 1);
    }

    fn contains_rule(id: &str, text: &str) -> AlertRule {
        AlertRule {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            trigger: eqlparser_core::alert::Trigger::Contains {
                text: text.to_string(),
            },
            sound: eqlparser_core::alert::Sound::Chime,
            speak: false,
            say: None,
            cooldown: 0,
            error: None,
        }
    }

    /// Reading the backlog must not set off every rule the player owns. This
    /// is the difference between opening the app and being shouted at.
    #[test]
    fn the_backlog_fires_nothing_and_live_lines_do() {
        let mut e = engine();
        e.set_alert_rules(vec![contains_rule("r1", "hits YOU")]);
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] A giant hits YOU for 90 points of damage."],
        );
        assert!(e.snapshot(1).alerts.is_empty(), "backlog must be silent");

        e.set_live(true);
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:05 2026] A giant hits YOU for 95 points of damage."],
        );
        let s = e.snapshot(1);
        assert_eq!(s.alerts.len(), 1);
        assert_eq!(s.alerts[0].rule_id, "r1");
        assert_eq!(s.alert_seq, 1);
    }

    /// A charm break is a spell fading plus a creature the tracker knew was
    /// yours. Neither half is enough, so this rides on a signal rather than on
    /// text — and it must still reach the snapshot.
    #[test]
    fn charm_break_raises_its_signal() {
        let mut e = engine();
        e.set_alert_rules(vec![AlertRule {
            trigger: eqlparser_core::alert::Trigger::Signal {
                kind: eqlparser_core::alert::SignalKind::CharmResisted,
                name: None,
            },
            ..contains_rule("charm", "unused")
        }]);
        e.set_live(true);
        // Charm immunity names nobody; the engine supplies the target from the
        // charm it was last attempting.
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] This NPC cannot be charmed."],
        );
        let s = e.snapshot(1);
        assert_eq!(s.alerts.len(), 1, "charm failure should raise an alert");
        assert!(s.alerts[0].detail.contains("immune"));
    }

    /// The lines feed is what the "alert me about this" button reads, so it
    /// must hold live lines only — and without the timestamp prefix.
    #[test]
    fn recent_lines_start_at_go_live() {
        let mut e = engine();
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:00 2026] You slash a rat for 10 points of damage."],
        );
        assert!(e.lines_since(0).is_empty(), "backlog is not recent");
        e.set_live(true);
        feed(
            &mut e,
            &["[Tue Jul 28 14:00:01 2026] You slash a rat for 12 points of damage."],
        );
        let lines = e.lines_since(0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "You slash a rat for 12 points of damage.");
        // Asking again with the sequence just seen returns nothing, so polling
        // a quiet log costs nothing.
        assert!(e.lines_since(lines[0].seq).is_empty());
    }
}
