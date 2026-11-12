//! The daemon's wire types, shared by the daemon and every client.
//!
//! The daemon serialises these and clients deserialise them, so the two cannot
//! drift: a field renamed here is renamed on both sides at once, and a client
//! built against an older copy is refused by [`API_VERSION`] rather than left
//! to misread a payload. Until this crate the API was shaped for one
//! JavaScript renderer — ninety-odd responses built with `json!` and a socket
//! that said only "something changed" — which was fine for that renderer and
//! would have meant every Rust client re-declaring the same structs by hand.
//!
//! Started with what the eqldb collector needs (see the *Daemon / collector
//! boundary* wiki page). The parser's own endpoints move onto types here as
//! each tab is ported; the untyped ones keep working meanwhile.

use serde::{Deserialize, Serialize};

/// What `/api/hello` names itself, so a client can tell this daemon from
/// anything else that happens to hold the port.
pub const IDENTITY: &str = "eqlparserd";

/// Bumped when a type here changes incompatibly. A client refuses a daemon
/// whose number differs from its own and says which side to update.
pub const API_VERSION: u32 = 1;

/// `GET /api/hello`: who this daemon is and what it is following.
///
/// The first thing every client asks, and the only endpoint guaranteed to keep
/// its shape across API versions — so a client too old or too new can still
/// read enough of it to say so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    /// Always [`IDENTITY`].
    pub identity: String,
    /// The daemon's crate version.
    pub version: String,
    /// The daemon's [`API_VERSION`].
    pub api_version: u32,
    pub pid: u32,
    pub port: u16,
    /// When the daemon started, Unix seconds.
    pub started_at: i64,
    /// `everquest-legends` or `everquest`: never pool the two.
    pub game: String,
    /// The log being followed.
    pub log: String,
    /// From the log's filename; empty when it carries none.
    pub character: String,
    pub server: String,
    /// The game install, when known.
    pub game_dir: Option<String>,
    /// The client's Screenshots folder, when known — so the collector finds
    /// the pictures without configuration of its own.
    pub screenshots_dir: Option<String>,
    /// Clients holding a socket open right now (`/ws` or `/api/events`).
    pub clients: usize,
    /// Seconds the daemon waits, once the last client has gone and nothing
    /// has been asked of it, before exiting. `None` when it never does.
    pub idle_exit_secs: Option<u64>,
    /// The log's clock minus this machine's, in seconds, once a live line has
    /// been seen to measure it; `None` before. The client writes local wall
    /// time with no zone, so anything pairing a log timestamp with a file's
    /// mtime needs this --- the collector filing a screenshot taken while it
    /// was closed, for one. Added within API 1: defaulted, so a client that
    /// predates it and a daemon that predates it both still read the other.
    #[serde(default)]
    pub clock_offset: Option<i64>,
    /// Where the followed character is, as a place name: the zone with any
    /// instance mode or tier parsed off, which is how eqldb names zones.
    /// `None` before the log has said, and from daemons older than the field.
    #[serde(default)]
    pub zone: Option<String>,
    /// The game-specific parts this game has (`sky`, `lockouts`, `motes`),
    /// from [`eqlparser_core::profile`]; an app hides the rest. `None` from a
    /// daemon older than the field, which an app reads as all of them, as
    /// every log was read as Legends then.
    #[serde(default)]
    pub features: Option<Vec<String>>,
}

impl Hello {
    /// Whether this game has `feature`. All of them, when the daemon is too
    /// old to say.
    pub fn has(&self, feature: &str) -> bool {
        self.features.as_ref().is_none_or(|f| f.iter().any(|x| x == feature))
    }
}

/// One thing that happened, pushed on `/api/events` as it happens.
///
/// Only live lines produce events. The initial parse replays the whole log,
/// and announcing three months of loot to a client that has just connected
/// would be noise, not news.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// "Taking a screenshot..." appeared in the log.
    ScreenshotTaken {
        /// The line's timestamp, in the log's clock.
        log_ts: i64,
        /// The same moment in this machine's clock — what the file's
        /// modification time will say. Converted with the offset in force
        /// when the line was seen, because the offset is remeasured on every
        /// live line.
        local_ts: i64,
    },
    /// You picked something up.
    Pickup(Pickup),
    /// A zone-in line. The parsed place name, `None` when the client's text
    /// did not parse to one.
    ZoneChanged { zone: Option<String>, raw: String },
    /// An inventory dump was folded in, so `/api/bags` has changed.
    BagsChanged,
}

/// A pickup, as `/api/pickups` lists them and [`Event::Pickup`] announces them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pickup {
    /// The name as the client wrote it.
    pub item: String,
    /// When it was last picked up, in the log's clock.
    pub at: i64,
    /// How many arrived (for `/api/pickups`, inside the window asked for).
    pub count: u64,
    /// The corpse it came off, when the log named one.
    pub source: Option<String>,
    /// Whether it is still in your bags, as far as the log can tell: an item
    /// sold on pickup is gone, though its link can still be opened.
    pub held: bool,
    /// Where you were standing, parsed; `None` makes no claim.
    pub zone: Option<String>,
}

/// `GET /api/pickups?since=<log ts>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pickups {
    pub since: i64,
    pub pickups: Vec<Pickup>,
}

/// `GET /api/bags`: what the followed character's inventory dumps hold.
///
/// Read from the dump files themselves rather than the item-id record, which
/// remembers every id ever seen and so lists things long since sold. Each
/// file's age goes with it: a dump is evidence about the moment it was taken.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bags {
    pub character: String,
    pub server: String,
    pub sources: Vec<BagSource>,
    pub items: Vec<BagItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BagSource {
    pub file: String,
    pub age_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BagItem {
    /// The ungraded name, as the client wrote it.
    pub name: String,
    /// The key names are matched on: see `eqlparser_core::drops::match_key`.
    pub key: String,
    /// Which grades are held: `0` for the plain item, `n` for `+n`.
    pub grades: Vec<u32>,
}

/// A panel another app asks the overlay to draw: `PUT /api/panels/{id}`.
///
/// The overlay's extension slot. The daemon stores and serves it and knows
/// nothing of what it says, so an app can put its prompt over the game
/// without the daemon taking on that app's job. A panel lapses `ttl_secs`
/// after it was last put, so an app that closes or crashes takes its panel
/// with it; an app keeps one up by putting it again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Panel {
    pub title: String,
    /// A short line beside the title: counts, mostly.
    #[serde(default)]
    pub meta: String,
    pub rows: Vec<PanelRow>,
    /// Seconds the panel stays without being put again. Clamped to 5..=600.
    #[serde(default = "Panel::default_ttl")]
    pub ttl_secs: u32,
}

impl Panel {
    pub const fn default_ttl() -> u32 {
        60
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelRow {
    /// Left: the name of the thing.
    pub name: String,
    /// Right: what about it.
    #[serde(default)]
    pub value: String,
    /// An optional second line, left and right, in the quieter style.
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub note_value: Option<String>,
    /// Drawn in the highlight colour: something only you know.
    #[serde(default)]
    pub highlight: bool,
}

/// `GET /api/panels`: every panel still up, oldest put first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Panels {
    pub panels: Vec<PanelUp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelUp {
    pub id: String,
    #[serde(flatten)]
    pub panel: Panel,
}

/// What a non-2xx answer carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tag is part of the contract: a client in another language reads
    /// `"type": "screenshot_taken"`, and a rename here would break it silently.
    #[test]
    fn events_are_tagged_in_snake_case() {
        let e = Event::ScreenshotTaken { log_ts: 10, local_ts: 12 };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "screenshot_taken");
        assert_eq!(v["local_ts"], 12);
        let p = Event::Pickup(Pickup {
            item: "Bone Chips".into(),
            at: 1,
            count: 1,
            source: None,
            held: true,
            zone: None,
        });
        assert_eq!(serde_json::to_value(&p).unwrap()["type"], "pickup");
        assert_eq!(serde_json::to_value(Event::BagsChanged).unwrap()["type"], "bags_changed");
        // A hello from before `clock_offset` existed still reads.
        let old = r#"{"identity":"eqlparserd","version":"0.1.0","api_version":1,"pid":1,"port":1,
            "started_at":0,"game":"g","log":"l","character":"","server":"","game_dir":null,
            "screenshots_dir":null,"clients":0,"idle_exit_secs":null}"#;
        assert_eq!(serde_json::from_str::<Hello>(old).unwrap().clock_offset, None);
        assert_eq!(serde_json::from_str::<Hello>(old).unwrap().zone, None);
        for e in [e, p, Event::BagsChanged] {
            let back: Event = serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
            assert_eq!(back, e);
        }
    }
}

/// `GET /api/daemon-log?since=N`: what the daemon has written to stderr,
/// from line `since` on.
///
/// Served by the daemon itself because nothing else is guaranteed to have
/// it: a daemon one app started outlives that app, and whoever opens a log
/// view later is not its parent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonLog {
    pub lines: Vec<DaemonLogLine>,
    /// Pass back as `since` to get only what came after.
    pub next: u64,
    /// Whether lines older than `since` had already been dropped.
    pub dropped: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonLogLine {
    pub seq: u64,
    pub text: String,
}

// ---- Dashboard --------------------------------------------------------------

/// `GET /api/lockouts`: the weekly and daily grid, and the void currency.
pub use eqlparser_core::lockout::{LockoutCell, LockoutReport, VoidCurrency, VoidGrant, VoidTask};

/// `GET /api/session?scope=session|zone`: every fight since the session
/// began (or in this zone), merged into one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReply {
    pub summary: EncounterSummary,
    pub fights: usize,
    /// Seconds of combat, the denominator behind every rate in `summary`.
    pub combat_seconds: i64,
    pub session_start: i64,
}

// ---- Settings ---------------------------------------------------------------

/// `GET /api/logs`: the logs beside the followed one, and whose history is on
/// file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogsReply {
    pub dir: Option<String>,
    pub current: String,
    /// Null until the history database has been opened.
    pub following: Option<Following>,
    pub characters: Vec<KnownCharacter>,
    /// Most recently written first.
    pub logs: Vec<LogFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Following {
    pub character: Option<String>,
    pub server: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownCharacter {
    pub character: String,
    pub server: Option<String>,
    pub kills: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogFile {
    pub path: String,
    pub name: String,
    pub character: Option<String>,
    pub server: Option<String>,
    pub bytes: u64,
    /// Unix seconds.
    pub modified: i64,
    pub current: bool,
}

/// `GET /api/logs/status`: every log in the folder and how far behind each is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogsStatusReply {
    pub dir: Option<String>,
    /// Seconds between sweeps of the logs that are not followed.
    pub every: Option<u64>,
    /// Why nothing was looked at, when nothing was.
    pub why: Option<String>,
    /// Worst gap first.
    pub logs: Vec<LogStatusRow>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogStatusRow {
    pub path: String,
    pub name: String,
    pub player: String,
    pub server: String,
    pub followed: bool,
    pub bytes: u64,
    pub parsed: u64,
    pub behind: u64,
    pub last_seen: i64,
}

/// `GET /api/history`, as far as Settings reads it: which loadouts the kills
/// on file were made with. The kills carry more than this; it is ignored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryBrief {
    pub available: bool,
    pub reason: Option<String>,
    pub kills: Vec<KillBuild>,
    pub counts: HistoryCounts,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KillBuild {
    pub build: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryCounts {
    pub kills: u64,
    pub encounters: u64,
    pub levels: u64,
}

// ---- Encounters -----------------------------------------------------------

/// `GET /api/encounters`: one page of fights, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncounterPage {
    /// Fights matching the filters, of which this is `offset..`.
    pub total: usize,
    pub offset: usize,
    pub encounters: Vec<EncounterBrief>,
}

/// A fight as the list shows it. The daemon sends more (meters, tick series
/// and the like, empty in a list); a client reading a list needs only this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncounterBrief {
    pub id: u64,
    /// Named after whichever creature took the most damage; the rest are
    /// "+N more".
    pub title: String,
    /// Everything fought, hardest-hit first: what `title` hides, and what
    /// `search` matches.
    #[serde(default)]
    pub opponents: Vec<String>,
    pub zone: Option<String>,
    pub start: i64,
    pub end: i64,
    pub duration: i64,
    /// Both sides' damage, so not a meter's headline.
    pub total_damage: u64,
    pub damage_to_enemies: u64,
    pub your_damage: u64,
    pub your_dps: f64,
    pub your_healing: u64,
    pub your_damage_taken: u64,
    pub your_deaths: u64,
    pub kills: usize,
    pub loot_count: usize,
    pub coin_copper: u64,
    /// The class trio active at the time, e.g. "WAR/DRU/BRD".
    pub build: String,
    /// Instance tier from the zone name: 0 open world to 4 Refined.
    pub difficulty: u8,
}

/// `GET /api/mobs`: the same history, one row per creature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MobPage {
    pub total: usize,
    pub offset: usize,
    pub mobs: Vec<MobRow>,
}

/// One creature within one fight: "how did I do against a skeletal monk".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MobRow {
    /// The fight it belongs to, so a row still opens the whole encounter.
    pub encounter_id: u64,
    pub name: String,
    pub zone: Option<String>,
    pub start: i64,
    /// The *fight's* duration. A creature inside a pull has no clock of its
    /// own: the log records when an actor last acted but never when it first
    /// did, and the timeline does not name targets. Reporting a share of the
    /// fight's seconds as "its" duration, or dividing damage by it to make a
    /// per-mob DPS, would be inventing the denominator. `shared_with` says
    /// how far off the fight's own figures are instead.
    pub duration: i64,
    pub build: String,
    pub difficulty: u8,
    pub damage_done_to_it: u64,
    pub damage_it_did: u64,
    pub kills: u32,
    pub loot: usize,
    pub resists: u64,
    /// Other creatures in the same fight. 0 means this row *is* the fight.
    pub shared_with: usize,
}

// ---------------------------------------------------------------------------
// One fight in full: `GET /api/encounters/{id}`, and the live meter in the
// snapshot. Moved here from the daemon's engine so the GPUI parser reads the
// detail through the same structs the daemon writes it with.

pub use eqlparser_core::encounter::{
    AbilityStats, ActorStats, DeathRecord, Encounter, LootRecord, MobBreakdown, Outcome, ResistTally, TimelineEffect,
    TimelineEvent, TimelineMark,
};

/// `GET /api/procs`: how often each proc fired, per minute of combat.
pub use eqlparser_core::procs::{ProcKind, ProcReport, ProcRow};
pub use eqlparser_core::task::{Achievement, DialogueLine, StepCompletion, TaskDef, TaskRecord, TaskReport, TaskStepDef};

// ---------------------------------------------------------------------------
// Two parts of the snapshot (`/ws`, `GET /api/snapshot`) that the Character
// and Classes tabs read. Moved here from the daemon's engine.

/// Detected class with supporting evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassInfo {
    pub name: String,
    pub abbrev: String,
    pub score: f32,
    pub share: f32,
    pub reasons: Vec<String>,
    /// True when the game named this class (a `/who` row, or in live
    /// EverQuest the class channel) rather than it being read off the
    /// evidence. A share is how much of the evidence pointed one way;
    /// beside a line that says the word outright it is not a confidence in
    /// anything, so the apps say "stated" instead. Absent from older
    /// daemons, which read as not stated.
    #[serde(default)]
    pub stated: bool,
}

/// Career totals across every parsed encounter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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

// Experience: `GET /api/experience`, `GET /api/class-levels` and `GET /api/aa`.
// The report is moved here from the daemon's engine; its parts, and the
// other two replies, are core's own types.

pub use eqlparser_core::aa::{AaAbility, AaRank, AaReport};
pub use eqlparser_core::classlevel::{ClassLevel, ClassLevelReport, LevelSource};
pub use eqlparser_core::experience::{AaPurchase, ExpSource, LevelRecord, ZoneShare};

/// Level-progression analysis.
///
/// All experience figures are "percent of a level", because that is the only
/// unit the client reports. It is comparable across levels as *progress*, but
/// deliberately not as raw experience: 1% at level 50 is far more than 1% at
/// level 10.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Ranks the character demonstrably holds but was never seen buying.
    pub aa_unlogged: Vec<AaPurchase>,
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

// Loot: `GET /api/loot[?session=1]` and `GET /api/loot/item?item=…`. Moved
// here from the daemon's engine, which writes them.

/// `GET /api/loot`, every item or (`?session=1`) this session's.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LootList {
    pub total: usize,
    pub items: Vec<LootTally>,
    /// When the current session began, in the log's clock; 0 before one has.
    /// Always reported, so a client can label the session from either list.
    pub session_start: i64,
    pub session_coin_copper: u64,
}

/// Everything the log can say about one item.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LootSource {
    pub mob: String,
    /// Times this creature dropped it and you took it.
    pub drops: u64,
    /// Times it dropped and you left it on the corpse. Still the creature
    /// dropping it, which is what a rate is about. 0 from older daemons.
    #[serde(default)]
    pub left: u64,
    /// Items received from it in total.
    pub quantity: u64,
    /// Times this creature died in a fight of yours — the denominator.
    pub kills: u64,
    /// `(drops + left) / kills`. Both go on top, because the question is how
    /// often the creature drops the item, not how often you fancied picking
    /// it up. Only meaningful with a denominator to divide by, so it is
    /// `None` when no kill of this creature was observed, rather than a
    /// fabricated 100%.
    pub rate: Option<f64>,
    pub zones: Vec<String>,
    pub last: i64,
}

/// A looted item aggregated across the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
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


// ---------------------------------------------------------------------------
// `GET /api/item?name=…`: an item's hover card. eqdb's answer and the copy
// bundled in the daemon ride together, and a card prefers eqdb field by
// field. Read leniently, as the spell card is: eqdb's reply is passed
// through untouched.

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemLookup {
    /// The name asked about, graded `+N` folded off.
    #[serde(default)]
    pub name: String,
    /// `eqdb`, `bundled` or `unknown` (no card).
    #[serde(default)]
    pub source: String,
    /// The eqdb origin the daemon is set to use; absent when the lookup is off.
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub eqdb: Option<EqdbItemPage>,
    #[serde(default)]
    pub wiki: Option<WikiItem>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EqdbItemPage {
    #[serde(default)]
    pub matches: Vec<EqdbItem>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EqdbItem {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub icon: Option<SpriteIcon>,
    #[serde(default)]
    pub stats: Option<EqdbItemStats>,
    /// Lists whose entries name an npc, a merchant or an item depending on
    /// the list; kept loose and read by [`EqdbItem::named`].
    #[serde(default)]
    pub sources: Vec<serde_json::Value>,
    #[serde(default)]
    pub vendors: Vec<serde_json::Value>,
    #[serde(default)]
    pub rewarded_by: Vec<serde_json::Value>,
    #[serde(default)]
    pub used_in: Vec<serde_json::Value>,
}

impl EqdbItem {
    /// What an entry in one of the lists is called: the npc for a drop, the
    /// merchant for a vendor, the name for a quest or recipe.
    pub fn named(v: &serde_json::Value) -> String {
        for k in ["npc", "merchant", "name"] {
            if let Some(s) = v.get(k).and_then(|s| s.as_str()) {
                return s.to_string();
            }
        }
        v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
    }
}

/// One cell of an icon sheet, served by `/api/item-icon?path=…`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpriteIcon {
    pub path: String,
    /// The cell's offset into the sheet, as CSS writes it: zero or negative.
    pub x: i32,
    pub y: i32,
    #[serde(default = "icon_cell")]
    pub size: u32,
}

fn icon_cell() -> u32 {
    40
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EqdbItemStats {
    /// Stat name to value, in the site's order.
    #[serde(default)]
    pub stats: serde_json::Map<String, serde_json::Value>,
}

/// The copy bundled in the daemon (`/api/wiki` holds them all).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WikiItem {
    #[serde(default)]
    pub name: String,
    /// A `data:image/png;base64,…` URI.
    #[serde(default)]
    pub icon: Option<String>,
    /// The game client's own tooltip text.
    #[serde(default)]
    pub stats: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// The 16 classes, in the order the spell file lists their levels.
pub use eqlparser_core::class::{CLASS_CANNOT_CAST, Class};

// ---------------------------------------------------------------------------
// `GET /api/spell?name=…`: a spell's hover card. Read leniently: eqdb's reply
// is passed through by the daemon, so every field it might leave out is
// defaulted rather than failing the whole card.

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpellLookup {
    #[serde(default)]
    pub name: String,
    /// `eqdb`, `local` (the spell file only) or `unknown` (no card).
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub eqdb: Option<EqdbSpell>,
    #[serde(default)]
    pub local: Option<LocalSpell>,
}

/// eqdb's spell, or a page of name matches when it was asked by name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EqdbSpell {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mana: u64,
    #[serde(default)]
    pub endurance: u64,
    #[serde(default)]
    pub cast_time: Option<SpellTime>,
    #[serde(default)]
    pub recast: Option<SpellTime>,
    #[serde(default)]
    pub duration: Option<SpellTime>,
    #[serde(default)]
    pub classes: Vec<SpellClass>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub matches: Vec<EqdbSpell>,
}

impl EqdbSpell {
    /// The spell itself, or the first match when this is a page of them.
    pub fn resolved(&self) -> Option<&EqdbSpell> {
        if self.id.is_some() { Some(self) } else { self.matches.first() }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpellTime {
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpellClass {
    pub class: String,
    pub level: u32,
}

/// What the daemon's own spell file says.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalSpell {
    #[serde(default)]
    pub id: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_charm: bool,
    /// Level per class in `Class::ALL` order; `CLASS_CANNOT_CAST` for never.
    #[serde(default)]
    pub classes: Vec<u8>,
}

/// `GET /api/encounters/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncounterDetail {
    pub summary: EncounterSummary,
    pub encounter: Encounter,
    /// Creatures you charmed in this fight.
    pub charmed_pets: Vec<ActorStats>,
    /// Everything that fought for you, summoned pets included. Kept beside
    /// `charmed_pets` rather than replacing it: uptime and breaks only mean
    /// something for charms.
    pub your_pets: Vec<ActorStats>,
    pub your_damage_with_pets: u64,
}

/// One row of a live damage meter.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub pet_kind: Option<String>,
    /// The player this pet fights for, including other people's pets. Absent
    /// for anything that is not a pet.
    pub pet_owner: Option<String>,
    /// Damage this actor dealt while charmed by you.
    pub pet_damage: u64,
}

/// A compact view of one fight, for lists and the overlay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncounterSummary {
    pub id: u64,
    pub title: String,
    /// Everything you fought, hardest-hit first. `title` names one of these
    /// and hides the rest behind "+N more"; this is what makes those findable,
    /// and it is what `search` matches against.
    #[serde(default)]
    pub opponents: Vec<String>,
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
    pub resists: Vec<ResistTally>,
    /// Per-creature split, populated alongside `meter`.
    #[serde(default)]
    pub mobs: Vec<MobBreakdown>,
    /// The fight's damage on a time axis, one number per second.
    #[serde(default)]
    pub tick_series: TickSeries,
}

/// Damage per second of the fight, split by side.
///
/// Four runs of the same length, index 0 being the fight's first second, so a
/// reader can add them or subtract them without lining anything up. Every run
/// is zero-filled to the fight's length: a second nobody did anything in is a
/// reading, and leaving it out would draw a lull as a straight line between
/// the peaks either side of it.
///
/// Raw per-second totals, not a smoothed rate. A one-second bucket is spiky
/// enough to be unreadable on its own -- a two-handed swing lands in one
/// second and nothing lands in the next -- so the chart rolls a window over
/// it. Which window is a display choice and belongs where the display is;
/// what happened in each second is a fact and belongs here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TickSeries {
    /// Whether these describe one continuous fight; see
    /// [`eqlparser_core::encounter::Encounter::ticks_retained`].
    pub retained: bool,
    /// Your own damage.
    pub you: Vec<u32>,
    /// Your pets', charmed or summoned.
    pub pets: Vec<u32>,
    /// Everyone else on your side, and their pets.
    pub allies: Vec<u32>,
    /// What landed on you.
    pub incoming: Vec<u32>,
}

/// Your damage under one mechanism — Spell, Melee or Other.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityGroup {
    pub group: String,
    pub damage: u64,
    /// Share of your damage in this fight, 0.0–1.0.
    pub share: f64,
    /// This group's damage over the fight's whole length.
    ///
    /// Over the *fight*, not over the time the group was being used, so the
    /// three groups add up to `your_dps` and can be read against it and
    /// against each other. A melee figure divided by the seconds you were
    /// actually swinging would be a bigger, truer-sounding number that no
    /// longer answers "where is my damage coming from" — it would rank a
    /// weapon you drew twice above the one you held all fight.
    pub dps: f64,
    /// Abilities within the group, biggest first.
    pub abilities: Vec<GroupedAbility>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupedAbility {
    pub name: String,
    pub damage: u64,
    pub hits: u64,
    pub crits: u64,
    pub max: u64,
    pub average: f64,
    /// Times it was cast, which is not `hits`; see [`AbilityStats::casts`](eqlparser_core::encounter::AbilityStats::casts).
    /// Zero for anything swung rather than cast.
    pub casts: u64,
    /// Share of its own group, so bars read within the group.
    pub share: f64,
    /// This ability's damage over the fight's whole length.
    ///
    /// Same denominator as [`AbilityGroup::dps`] and for the same reason: a
    /// group's abilities sum to the group, and the groups sum to `your_dps`.
    /// A dot's damage spread over the fight is what it contributed to the
    /// fight; spreading it over its own uptime instead would say something
    /// about the spell rather than about this pull.
    pub dps: f64,
}
// ---------------------------------------------------------------------------
// Tradeskills: `GET /api/tradeskills` is core's report; `GET /api/drops`
// supplies where a component drops. Forage: `GET /api/forage`, moved here
// from the daemon's record.

pub use eqlparser_core::drops::{DropReport, DropSighting, ItemDrops};
pub use eqlparser_core::game::Game;
pub use eqlparser_core::tradeskill::{
    ComponentUse, CostBasis, CostComponent, CraftRecord, ItemPrice, LearnedRecipe, RecipeCost, RecipeSpend, SkillCost,
    SkillProgress, SkillSegment, TradeskillReport, VendorPrice,
};

/// What forage has yielded, per zone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForageReport {
    pub note: String,
    pub source: String,
    pub zones: Vec<ForageZoneRow>,
    /// Attempts made before the log announced any zone.
    pub unplaced: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForageZoneRow {
    pub zone: String,
    pub game: String,
    pub attempts: u64,
    pub hits: u64,
    pub empty: u64,
    pub first_seen: i64,
    pub last_seen: i64,
    pub items: Vec<ForagedItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForagedItem {
    pub item: String,
    pub times: u64,
}

// ---------------------------------------------------------------------------
// Sky: `GET /api/sky[?refresh=1]` is core's `SkyReport` with the item site's
// account of the same quests merged in under `site`. `GET /api/item-brief`
// answers the icons and links a row needs.

pub use eqlparser_core::skyquest::{BossKills, GiverLine, IslandProgress, SkyItemProgress, SkyQuestProgress, SkyReport};

/// The `/api/sky` reply: the report, and the site beside it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkyReply {
    #[serde(flatten)]
    pub report: SkyReport,
    #[serde(default)]
    pub site: Option<SkySite>,
}

/// The item site's record of the Plane of Sky, or why there is none.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkySite {
    #[serde(default)]
    pub ok: bool,
    /// Lookups are switched off.
    #[serde(default)]
    pub off: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// The site's origin, which item links are built from.
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub age_seconds: i64,
    /// Our quest names the site has no record of.
    #[serde(default)]
    pub missing: Vec<String>,
    #[serde(default)]
    pub quests: std::collections::HashMap<String, SiteQuest>,
    #[serde(default)]
    pub drops: std::collections::HashMap<String, SiteDrop>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SiteQuest {
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub disagrees: Vec<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub contributors: Option<u64>,
    #[serde(default)]
    pub dialogue: Option<Vec<SiteLine>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteLine {
    #[serde(default)]
    pub speaker: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SiteDrop {
    #[serde(default)]
    pub looted: Option<u64>,
    #[serde(default)]
    pub sources: Vec<SiteDropSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteDropSource {
    pub npc: String,
    #[serde(default)]
    pub island: Option<String>,
    #[serde(default)]
    pub looted: Option<u64>,
}

/// `GET /api/item-brief?names=a|b`: per name, an icon cell, the slug and
/// whether the site has a page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemBriefReply {
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub items: std::collections::HashMap<String, ItemBrief>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemBrief {
    #[serde(default)]
    pub icon: Option<SpriteIcon>,
    #[serde(default)]
    pub on_site: bool,
    #[serde(default)]
    pub slug: String,
}

// Tells: `GET /api/tells`, core's report.
pub use eqlparser_core::tells::{Conversation, TellMessage, TellReport};

// ---------------------------------------------------------------------------
// Alerts: `GET /api/alerts`, `POST /api/alerts`, `POST /api/alerts/delete`
// and the feed, `GET /api/lines?since=N`. Fires ride on the snapshot.

pub use eqlparser_core::alert::{AlertFire, AlertRule, SignalKind, Sound, Trigger};

/// `GET /api/alerts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlertsReply {
    #[serde(default)]
    pub rules: Vec<AlertRule>,
    /// False when the daemon has no database: rules die with it.
    #[serde(default)]
    pub persisted: bool,
}

/// One line as the alerts view sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    /// Monotonic, so a client can ask for only what it has not seen.
    pub seq: u64,
    pub at: i64,
    /// The line with the client's timestamp prefix removed.
    pub text: String,
}

/// `GET /api/lines?since=N`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinesReply {
    #[serde(default)]
    pub lines: Vec<LogLine>,
    #[serde(default)]
    pub live: bool,
}
