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
