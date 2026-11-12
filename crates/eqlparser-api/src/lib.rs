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
