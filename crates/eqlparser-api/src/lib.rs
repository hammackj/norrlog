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

pub use eqlparser_core::encounter::{AbilityStats, ActorStats, DeathRecord, Encounter, LootRecord, MobBreakdown, ResistTally};

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