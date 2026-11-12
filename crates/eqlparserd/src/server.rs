//! HTTP + WebSocket API, and static hosting for the overlay.
//!
//! The overlay is served from this process rather than from a file:// URL so
//! that OBS can point a Browser Source at a plain http://127.0.0.1 address, and
//! so the Electron windows and OBS all consume exactly the same page.

use crate::engine::{Engine, Snapshot};
use crate::locks::{Guarded, Shared};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::broadcast;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    /// Who this daemon is, and who is using it: `/api/hello` and idle exit.
    pub life: Arc<Life>,
    /// The log being followed, and the directory to look for others in.
    pub log_path: std::path::PathBuf,
    pub logs_dir: Option<std::path::PathBuf>,
    /// Durable history; absent when the database could not be opened.
    pub store: Option<Arc<Mutex<crate::store::Store>>>,
    /// The engine, locked briefly for detail queries. The follower thread holds
    /// this while ingesting a batch of lines.
    pub engine: Arc<Mutex<Engine>>,
    /// Latest snapshot, published by the follower so the hot path (overlay
    /// polling, WebSocket fan-out) never contends on the engine lock.
    pub snapshot: Arc<RwLock<Snapshot>>,
    /// Fires whenever a new snapshot is published.
    pub updates: broadcast::Sender<()>,
    /// Things that happened, as they happen: `/api/events`.
    pub events: broadcast::Sender<eqlparser_api::Event>,
    /// Item reference data from the eqdb companion site, with the bundled
    /// data behind it.
    pub itemdb: Arc<crate::itemdb::ItemDb>,
    /// The screenshot reader, which may not be running. Always present, so
    /// that "off, and here is what is missing" is an answer this can give —
    /// and so that a token arriving through `/api/eqdb/token` has something
    /// to switch on.
    pub shots: Arc<crate::shots::Shots>,
    /// The accumulated item-id record. Absent when there is nowhere to keep
    /// it; present but empty when there is no game folder to read.
    pub ids: Option<Arc<Mutex<crate::dumps::Ids>>>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/overlay", get(overlay_page))
        .route("/api/shots/recent", get(shots_recent))
        .route("/api/logs/status", get(logs_status))
        .route("/api/item-ids", get(item_ids))
        .route("/api/items/held", get(items_held))
        .route("/api/bundle/preview", get(bundle_preview))
        .route("/api/bundle/send", post(bundle_send))
        .route(
            "/api/bundle/contributor",
            get(get_contributor).post(set_contributor),
        )
        .route("/api/eqdb/token", axum::routing::post(set_eqdb_token))
        .route("/overlay.css", get(overlay_css))
        .route("/overlay.js", get(overlay_js))
        .route("/api/snapshot", get(snapshot))
        .route("/api/encounters", get(encounters))
        .route("/api/encounters/{id}", get(encounter_detail))
        .route("/api/mobs", get(mobs))
        .route("/api/loot", get(loot))
        .route("/api/loot/item", get(loot_item))
        .route("/api/classes", get(classes))
        .route("/api/experience", get(experience))
        .route("/api/procs", get(procs))
        .route("/api/lockouts", get(lockouts))
        .route("/api/tradeskills", get(tradeskills))
        .route("/api/forage", get(forage))
        .route("/api/items", get(items))
        .route("/api/drops", get(drops))
        .route("/api/bestiary", get(bestiary))
        .route("/api/tasks", get(tasks).post(define_task))
        .route("/api/builds", get(builds))
        .route("/api/history", get(history))
        .route("/api/raid-kills", get(raid_kills))
        .route("/api/logs", get(logs))
        .route("/api/class-levels", get(class_levels).post(set_class_level))
        .route("/api/aa-spent", post(set_aa_spent))
        .route("/api/export", get(export))
        .route("/api/encounters/export", get(export_encounters))
        .route("/api/charm", get(charm))
        .route("/api/deaths", get(deaths))
        .route("/api/tells", get(tells))
        .route("/api/npcs", get(npcs))
        .route("/api/quests", get(quests).post(put_quest))
        .route("/api/quests/delete", post(delete_quest))
        .route("/api/quests/export", get(export_quests))
        .route("/api/aa", get(aa))
        .route("/api/item", get(item_lookup))
        .route("/api/item-icon", get(item_icon))
        .route("/api/spell", get(spell_lookup))
        .route("/api/sky", get(sky).post(define_sky_quest))
        .route("/api/wiki", get(wiki))
        .route("/api/incoming", get(incoming))
        .route("/api/buff-marks", get(get_marks).post(set_mark))
        .route("/api/item-brief", get(item_brief))
        .route("/api/wanted", get(wanted))
        .route("/api/wanted/mark", post(set_wanted_mark))
        .route("/api/wanted/recent", get(wanted_recent))
        .route("/api/alerts", get(alerts).post(put_alert))
        .route("/api/alerts/delete", post(delete_alert))
        .route("/api/lines", get(lines))
        .route("/api/session", get(session))
        .route("/ws", get(ws_upgrade))
        .route("/api/hello", get(hello))
        .route("/api/events", get(events_upgrade))
        .route("/api/pickups", get(api_pickups))
        .route("/api/bags", get(api_bags))
        .route("/api/bundle", get(api_bundle))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state.life),
            touch,
        ))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Lifecycle: who this is, and whether anybody still needs it
// ---------------------------------------------------------------------------

/// The daemon's identity and its clients.
///
/// One daemon is shared by the parser app and the eqldb collector, and
/// whichever starts first starts it. So nothing owns it, and it has to know
/// for itself when it is no longer wanted: no socket open, and nothing asked
/// of it for a while. Both halves count, because the OBS overlay holds a
/// socket but a script polling `/api/snapshot` does not.
pub struct Life {
    hello: eqlparser_api::Hello,
    clients: std::sync::atomic::AtomicUsize,
    /// Unix seconds of the last request or socket close.
    last_activity: std::sync::atomic::AtomicI64,
}

impl Life {
    pub fn new(hello: eqlparser_api::Hello) -> Life {
        Life {
            hello,
            clients: Default::default(),
            last_activity: std::sync::atomic::AtomicI64::new(unix_now()),
        }
    }

    pub fn hello(&self) -> eqlparser_api::Hello {
        eqlparser_api::Hello {
            clients: self.clients(),
            ..self.hello.clone()
        }
    }

    pub fn clients(&self) -> usize {
        self.clients.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn touch(&self) {
        self.last_activity
            .store(unix_now(), std::sync::atomic::Ordering::SeqCst);
    }

    /// Seconds since anybody needed this daemon, or `None` while a client is
    /// connected.
    pub fn idle_for(&self, now: i64) -> Option<i64> {
        (self.clients() == 0)
            .then(|| now - self.last_activity.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// Count a socket for as long as the returned guard lives.
    fn connect(self: &Arc<Self>) -> ClientGuard {
        self.clients.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.touch();
        ClientGuard(Arc::clone(self))
    }
}

/// Uncounts a client when its socket task ends, however it ends.
struct ClientGuard(Arc<Life>);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.clients.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        // From the close, not from the open: the idle wait starts when the
        // last client *leaves*.
        self.0.touch();
    }
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Every request is a sign of life.
async fn touch(
    State(life): State<Arc<Life>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    life.touch();
    next.run(req).await
}

/// Exit once nobody has needed this daemon for `secs` seconds.
///
/// Checked on a timer rather than at the moment the last client leaves, so a
/// client that reconnects — an app reloading its window, OBS switching scenes
/// — is not a reason to exit. The exit is the same one `--exit-with-parent`
/// makes: history is flushed on its own timer, and whatever the last flush
/// wrote is what a kill would have left.
pub fn spawn_idle_exit(life: Arc<Life>, secs: u64) {
    tokio::spawn(async move {
        let tick = std::time::Duration::from_secs(secs.clamp(1, 5));
        loop {
            tokio::time::sleep(tick).await;
            if life.idle_for(unix_now()).is_some_and(|idle| idle >= secs as i64) {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr(),
                    "[eqlparserd] no client for {secs}s; exiting"
                );
                std::process::exit(0);
            }
        }
    });
}

/// `GET /api/hello`.
async fn hello(State(s): State<AppState>) -> Json<eqlparser_api::Hello> {
    let (clock_offset, zone) = {
        let e = s.engine.locked("engine");
        (e.clock_offset(), e.zone_place())
    };
    Json(eqlparser_api::Hello { clock_offset, zone, ..s.life.hello() })
}

// ---------------------------------------------------------------------------
// Static assets
// ---------------------------------------------------------------------------

/// Assets are embedded so the binary runs from anywhere, including when
/// launched as a child process by Electron with an arbitrary CWD.
/// Item reference data imported from eqlwiki.com by `tools/import-wiki.py`.
///
/// Embedded rather than fetched: the daemon does no network I/O, so the data
/// is as available offline as the rest of the app and cannot fail mid-session.
/// Re-run the importer to refresh it.
///
/// Only *item* pages are used. The wiki's quest pages describe classic
/// EverQuest — different givers, different requirements — while this server
/// runs its own variant, verified against what the quest NPCs actually say
/// in-game. Item stats and icons match; quest structure does not.
const WIKI_ITEMS: &str = include_str!("../assets/wiki-items.json");

const OVERLAY_HTML: &str = include_str!("../assets/overlay.html");
const OVERLAY_CSS: &str = include_str!("../assets/overlay.css");
const OVERLAY_JS: &str = include_str!("../assets/overlay.js");
const INDEX_HTML: &str = include_str!("../assets/index.html");

fn html(body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
}

async fn index() -> Response {
    html(INDEX_HTML)
}

async fn overlay_page() -> Response {
    html(OVERLAY_HTML)
}

async fn overlay_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        OVERLAY_CSS,
    )
        .into_response()
}

async fn overlay_js() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        OVERLAY_JS,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

async fn snapshot(State(s): State<AppState>) -> Json<Snapshot> {
    Json(s.snapshot.reading("snapshot").clone())
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<usize>,
    offset: Option<usize>,
    /// Case-insensitive substring match on the encounter title.
    search: Option<String>,
    zone: Option<String>,
    /// Only encounters at or after this unix timestamp.
    since: Option<i64>,
    /// Only encounters at or before this unix timestamp.
    until: Option<i64>,
    /// Drop fights shorter than this many seconds; filters out trash pulls.
    min_duration: Option<i64>,
    /// Class trio that fought it, e.g. "WAR/DRU/BRD". Order-insensitive: the
    /// label is canonical, but nobody types it that way.
    build: Option<String>,
    /// Instance tier, 0-4. "all d4 fights" is the question this answers.
    difficulty: Option<u8>,
}

/// Does this fight involve a creature the searcher named?
///
/// Matched against every opponent, not only the title. `title` names the
/// creature that took the most damage and folds the rest into "+N more", so
/// on a log where two thirds of fights have more than one opponent, a name
/// search over titles alone found 32% of the fights that actually contained
/// the creature. The one you are looking for is more often inside the "+N".
fn search_matches(enc: &eqlparser_core::encounter::Encounter, needle: &str) -> bool {
    enc.title().to_lowercase().contains(needle)
        || enc
            .opponents()
            .iter()
            .any(|o| o.to_lowercase().contains(needle))
}

/// Compare a requested build against a canonical label, ignoring order and
/// separators, so "brd/dru/war", "WAR DRU BRD" and "WAR/DRU/BRD" all match.
fn build_matches(want: &str, label: &str) -> bool {
    let norm = |s: &str| {
        let mut parts: Vec<String> = s
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|p| !p.is_empty())
            .map(|p| p.to_uppercase())
            .collect();
        parts.sort();
        parts
    };
    let (w, l) = (norm(want), norm(label));
    // A partial request is a filter, not an equality test: "WAR" finds every
    // build containing a warrior, which is what someone comparing builds wants.
    !w.is_empty() && w.iter().all(|p| l.contains(p))
}

/// The historical browser: filter and page through every parsed encounter.
async fn encounters(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Json<serde_json::Value> {
    let engine = s.engine.locked("engine");
    let search = q.search.map(|v| v.to_lowercase());
    let zone = q.zone.map(|v| v.to_lowercase());

    // Newest first: the most recent fight is nearly always the one wanted.
    let filtered: Vec<_> = engine
        .encounters()
        .iter()
        .rev()
        .filter(|e| {
            if let Some(sc) = &search {
                if !search_matches(e, sc) {
                    return false;
                }
            }
            if let Some(z) = &zone {
                if !e
                    .zone
                    .as_ref()
                    .is_some_and(|ez| ez.to_lowercase().contains(z))
                {
                    return false;
                }
            }
            if q.since.is_some_and(|t| e.end < t) {
                return false;
            }
            if q.until.is_some_and(|t| e.start > t) {
                return false;
            }
            if q.min_duration.is_some_and(|d| e.duration() < d) {
                return false;
            }
            if let Some(d) = q.difficulty {
                let tier = e
                    .zone
                    .as_deref()
                    .map(eqlparser_core::lockout::difficulty_of)
                    .unwrap_or(0);
                if tier != d {
                    return false;
                }
            }
            true
        })
        // Build last and separately: resolving it means scanning class evidence
        // around the fight, which is far dearer than any other test here, so it
        // only runs on what survived them.
        .filter(|e| {
            q.build.as_deref().is_none_or(|want| {
                build_matches(want, &engine.build_label(e.start + (e.end - e.start) / 2))
            })
        })
        .collect();

    let total = filtered.len();
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(100).min(1000);
    let page: Vec<_> = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|e| engine.summarise(e, false))
        .collect();

    Json(serde_json::json!({ "total": total, "offset": offset, "encounters": page }))
}

/// One creature within one fight.
///
/// A fight is bounded by lulls, not by opponents, so a pull arrives as a
/// single encounter with "+N more" in its title. That is the right unit for a
/// meter — your damage over a pull is one number — and the wrong one for
/// "how did I do against a skeletal monk", which is what this answers.
#[derive(Debug, Clone, serde::Serialize)]
struct MobRow {
    /// The fight it belongs to, so a row still opens the whole encounter.
    encounter_id: u64,
    name: String,
    zone: Option<String>,
    start: i64,
    /// The *fight's* duration. A creature inside a pull has no clock of its
    /// own: the log records when an actor last acted but never when it first
    /// did, and the timeline does not name targets. Reporting a share of the
    /// fight's seconds as "its" duration, or dividing damage by it to make a
    /// per-mob DPS, would be inventing the denominator. `shared_with` says
    /// how far off the fight's own figures are instead.
    duration: i64,
    build: String,
    difficulty: u8,
    damage_done_to_it: u64,
    damage_it_did: u64,
    kills: u32,
    loot: usize,
    resists: u64,
    /// Other creatures in the same fight. 0 means this row *is* the fight.
    shared_with: usize,
}

/// The historical browser, one row per creature instead of per fight.
///
/// Same filters as `/api/encounters`, so the two are interchangeable views of
/// the same question; `search` matches the creature's own name here, which is
/// the whole point of the view.
async fn mobs(State(s): State<AppState>, Query(q): Query<ListQuery>) -> Json<serde_json::Value> {
    let engine = s.engine.locked("engine");
    let search = q.search.map(|v| v.to_lowercase());
    let zone = q.zone.map(|v| v.to_lowercase());

    let mut rows: Vec<MobRow> = Vec::new();
    for e in engine.encounters().iter().rev() {
        if let Some(z) = &zone {
            if !e
                .zone
                .as_ref()
                .is_some_and(|ez| ez.to_lowercase().contains(z))
            {
                continue;
            }
        }
        if q.since.is_some_and(|t| e.end < t) || q.until.is_some_and(|t| e.start > t) {
            continue;
        }
        if q.min_duration.is_some_and(|d| e.duration() < d) {
            continue;
        }
        let difficulty = e
            .zone
            .as_deref()
            .map(eqlparser_core::lockout::difficulty_of)
            .unwrap_or(0);
        if q.difficulty.is_some_and(|d| difficulty != d) {
            continue;
        }
        let per_mob = e.per_mob();
        // Matching creatures only. The rest of the pull is not what was asked
        // for, and listing it would put the old problem back one level down.
        let wanted: Vec<_> = per_mob
            .iter()
            .filter(|m| {
                search
                    .as_deref()
                    .is_none_or(|sc| m.name.to_lowercase().contains(sc))
            })
            .collect();
        if wanted.is_empty() {
            continue;
        }
        // Last and separately, as in `encounters`: resolving a build is dearer
        // than any other test here, so it only runs on what survived them.
        let build = engine.build_label(e.start + (e.end - e.start) / 2);
        if q
            .build
            .as_deref()
            .is_some_and(|want| !build_matches(want, &build))
        {
            continue;
        }
        let shared_with = per_mob.len().saturating_sub(1);
        for m in wanted {
            rows.push(MobRow {
                encounter_id: e.id,
                name: m.name.clone(),
                zone: e.zone.clone(),
                start: e.start,
                duration: e.duration(),
                build: build.clone(),
                difficulty,
                damage_done_to_it: m.damage_done_to_it,
                damage_it_did: m.damage_it_did,
                kills: m.kills,
                loot: m.loot,
                resists: m.resists,
                shared_with,
            });
        }
    }

    let total = rows.len();
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(100).min(1000);
    let page: Vec<_> = rows.into_iter().skip(offset).take(limit).collect();
    Json(serde_json::json!({ "total": total, "offset": offset, "mobs": page }))
}

/// Export chosen fights as a write-up.
///
/// Takes the same filters as the browser plus an explicit `ids` list, so what
/// you exported is what you were looking at. Markdown by default because the
/// output is meant to be pasted into a guide; CSV when it is meant to be
/// totalled up somewhere else.
async fn export_encounters(
    State(s): State<AppState>,
    Query(q): Query<ExportEncountersQuery>,
) -> Response {
    let engine = s.engine.locked("engine");
    let ids: Option<Vec<u64>> = q.ids.as_deref().map(|raw| {
        raw.split(',')
            .filter_map(|p| p.trim().parse::<u64>().ok())
            .collect()
    });

    let mut chosen: Vec<_> = engine
        .encounters()
        .iter()
        .filter(|e| match &ids {
            Some(list) => list.contains(&e.id),
            None => true,
        })
        .filter(|e| {
            q.search
                .as_deref()
                .is_none_or(|sc| search_matches(e, &sc.to_lowercase()))
        })
        .filter(|e| {
            q.difficulty.is_none_or(|d| {
                e.zone
                    .as_deref()
                    .map(eqlparser_core::lockout::difficulty_of)
                    .unwrap_or(0)
                    == d
            })
        })
        .filter(|e| {
            q.build.as_deref().is_none_or(|want| {
                build_matches(want, &engine.build_label(e.start + (e.end - e.start) / 2))
            })
        })
        .collect();
    chosen.sort_by_key(|e| e.start);

    // With the meter: an export exists to be read by someone who was not
    // there, so the per-ability and per-creature detail is the point.
    let full: Vec<_> = chosen.iter().map(|e| engine.summarise(e, true)).collect();

    let csv = q.format.as_deref() == Some("csv");
    let body = if csv {
        render_encounters_csv(&full)
    } else {
        render_encounters_markdown(&full)
    };
    let (ctype, name) = if csv {
        ("text/csv", "encounters.csv")
    } else {
        ("text/markdown", "encounters.md")
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, format!("{ctype}; charset=utf-8")),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        body,
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct ExportEncountersQuery {
    /// Comma-separated encounter ids. Absent means everything the other
    /// filters allow.
    ids: Option<String>,
    search: Option<String>,
    build: Option<String>,
    difficulty: Option<u8>,
    format: Option<String>,
}

fn fmt_hms(seconds: i64) -> String {
    let (m, s) = (seconds / 60, seconds % 60);
    if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// The item-id record, whole.
///
/// Served rather than left as a file for other programs to find, because a
/// file is a path every reader has to be told and this daemon is already the
/// thing that knows where the record lives. The capture tree reads it to
/// decide which items still want photographing; `eqldb-export` reads it into
/// the bundle.
///
/// Whole rather than paged: it is a couple of thousand rows and every caller
/// so far wants all of them.
async fn item_ids(State(state): State<AppState>) -> impl IntoResponse {
    let Some(ids) = &state.ids else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "no item-id record: this daemon has no config directory to keep one in"
            })),
        )
            .into_response();
    };
    let guard = ids.locked("item ids");
    Json(serde_json::json!({
        "path": guard.path().to_string_lossy(),
        "registry": guard.registry(),
    }))
    .into_response()
}

/// What is in the bags now, by match key, from the newest dump of each
/// character.
///
/// # Not the item-id record, which is a different question
///
/// `grades_held` in the accumulator means *has been held*, and it never
/// forgets: `dumps` says so in its own header --- "an item sold between two
/// dumps keeps its id here, which is the entire point." That is right for a
/// record of which ids exist and wrong for this. Measured against one machine:
/// the accumulator holds 1,631 items as held and 883 of them are in no current
/// dump at all. Built from it, this list said 318 items were in the bags when
/// 126 were, and would have sent somebody hunting through their inventory for
/// items they sold weeks ago.
///
/// So the dumps are read directly. They are snapshots and go stale the moment
/// something is sold --- which is why the answer carries each file's age, and
/// the caller can see that a character last dumped in August is not evidence
/// about this afternoon.
fn bags(dumps: &[(String, String)]) -> std::collections::BTreeMap<String, std::collections::BTreeSet<u32>> {
    let mut out: std::collections::BTreeMap<String, std::collections::BTreeSet<u32>> =
        Default::default();
    for (_, text) in dumps {
        let Ok(rows) = eqlparser_core::itemids::read_inventory(text) else {
            continue;
        };
        for r in rows {
            let k = crate::itemdb::match_key(&r.item);
            if !k.is_empty() {
                out.entry(k).or_default().insert(r.grade);
            }
        }
    }
    out
}

// An earlier draft let a carried item answer to the other names its id has been
// written under, on the reasoning that the site might use a different spelling.
// It is wrong here, and the real data said so: `also_known_as` conflates a
// rename, which is the same item, with an item's Exaltation augment, which
// shares its number and is a *different* item. This machine carries
// `Brell's Girdle` at +3 and +5 and a `Brell's Girdle (Exaltation)` at 0;
// pooling those grades claimed an ungraded girdle was in the bag and unblocked
// it, when photographing the augment yields a reading of the augment. It moved
// 9 items out of `blocked` that belong there. Nothing in the registry tells the
// two kinds of alias apart, so names are matched as `match_key` folds them ---
// case, the `+N` grade, and whitespace, which is every difference actually
// observed between what the client writes and what the site stores.
//
// `match_key` does not fold the backtick into the apostrophe and does not need
// to: the site stores an item under the client's own spelling, so both sides
// write the same character. Measured against the live wanted list and four
// dumps -- 14 backtick names on each side -- folding it gains 0 matches of 156.

/// The inventory dumps of the character this daemon follows: who that is,
/// each file's text, and each file's age.
///
/// Whose bags: the client writes one dump per character and this daemon
/// follows one log, so "in your bags" means the character on that log and not
/// every character who has ever typed the command. hammackj was on Tharn and the
/// wanted list was offering things sitting in Mirela's bags 44 hours ago --- not
/// a prompt but a trip to the bank and a character select.
///
/// Read from the files rather than the id record, which remembers every id
/// ever seen. Sorted newest first.
fn my_dumps(
    state: &AppState,
    ids: &Arc<Mutex<crate::dumps::Ids>>,
) -> (Option<(String, String)>, Vec<(String, String)>, Vec<eqlparser_api::BagSource>) {
    let me: Option<(String, String)> = state.store.as_ref().and_then(|st| {
        let st = st.lock().ok()?;
        let (c, sv) = st.identity();
        (!c.is_empty() && !sv.is_empty()).then(|| (c.to_string(), sv.to_string()))
    });
    let guard = ids.locked("item ids");
    let mut read = Vec::new();
    let mut seen = Vec::new();
    for (path, kind) in guard.sources() {
        if kind != eqlparser_core::itemids::Source::Inventory {
            continue;
        }
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !mine(&file, me.as_ref()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let age = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs());
        seen.push(eqlparser_api::BagSource { file: file.clone(), age_seconds: age });
        read.push((file, text));
    }
    seen.sort_by_key(|b| b.age_seconds.unwrap_or(u64::MAX));
    (me, read, seen)
}

/// Items in the dumps, one per name as [`crate::itemdb::match_key`] folds it,
/// with every grade held.
fn bag_items(dumps: &[(String, String)]) -> Vec<eqlparser_api::BagItem> {
    let mut out: std::collections::BTreeMap<String, (String, std::collections::BTreeSet<u32>)> =
        Default::default();
    for (_, text) in dumps {
        let Ok(rows) = eqlparser_core::itemids::read_inventory(text) else {
            continue;
        };
        for r in rows {
            let k = crate::itemdb::match_key(&r.item);
            if k.is_empty() {
                continue;
            }
            let name = eqlparser_core::drops::split_grade(&r.item).0.trim().to_string();
            out.entry(k).or_insert_with(|| (name, Default::default())).1.insert(r.grade);
        }
    }
    out.into_iter()
        .map(|(key, (name, grades))| eqlparser_api::BagItem { name, key, grades: grades.into_iter().collect() })
        .collect()
}

/// Is this dump the bags of the character the daemon is following?
///
/// Both halves have to be known before a file is refused. The daemon learns
/// who it is from the log's filename and whose a dump is from the dump's, and
/// either can be missing --- a renamed file parses to nothing, and a daemon
/// started on a log with no identity in its name knows nobody. Refusing on a
/// half-known answer would empty the list for a reason invisible from the
/// outside, so an unknown on either side lets the file through and the answer
/// says which case it is in.
fn mine(file: &str, me: Option<&(String, String)>) -> bool {
    let (Some(me), Some(whose)) = (me, eqlparser_core::itemids::who(file)) else {
        return true;
    };
    &whose == me
}

/// The wanted rows this player is carrying, and which of them a photograph
/// cannot help.
///
/// # A graded copy is not worth photographing, whatever the item needs
///
/// The blocked rule is `!grades.contains(&0)`: you hold this item, but every
/// copy of it is a `+N`, and a photograph of a `+N` goes nowhere.
///
/// That is the reader's rule, not a guess at it. `eqlparser_parse::held_reason`
/// refuses a
/// graded window *before* anything asks what the site wants --- the site
/// derives graded stats from a base reading and its own grade rule, so a
/// window shot at +4 is an output of that rule rather than an input to it ---
/// and the verdict comes back `held: graded +4`.
///
/// This first shipped as `needs == ["base"] && !grades.contains(&0)`, on my
/// own reasoning that an item still wanting a tooltip or an icon was worth
/// photographing at any rung because the reading fills those. It is not: the
/// reading never happens. hammackj found it the way it had to be found ---
/// the panel asked for `Refugee Shroud` and `Apothic Kilt`, he photographed
/// the `+3`s in his bags, and both came back `held: graded +3`. A prompt whose
/// answer is refused is worse than no prompt, because it costs the trip and
/// then blames the picture.
///
/// An `(Exaltation)` is exempt from the reader's hold and needs no exemption
/// here: it carries no `+N`, so it is already grade 0 under its own name.
///
/// Measured against the live list and four dumps: 151 wanted items in bags,
/// 58 blocked by this rule where the old one blocked 30, leaving 93 that a
/// photograph can reach and 65 not yet photographed.
///
fn held_and_wanted(
    wanted: &serde_json::Value,
    held: &std::collections::BTreeMap<String, std::collections::BTreeSet<u32>>,
    marks: &dyn Fn(&str) -> (bool, bool),
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for it in wanted.get("items").and_then(|v| v.as_array()).into_iter().flatten() {
        let Some(name) = it.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(grades) = held.get(&crate::itemdb::match_key(name)) else {
            continue;
        };
        // Every copy is a `+N`. What the item needs does not enter into it: the
        // reader refuses the window before it asks. `needs` still travels on
        // the row, because it is what the row says and what the list sorts on.
        let blocked = !grades.contains(&0);
        let mut row = serde_json::Map::new();
        row.insert("name".into(), name.into());
        for f in ["slug", "needs", "zones", "sources", "sold_by"] {
            if let Some(v) = it.get(f) {
                row.insert(f.into(), v.clone());
            }
        }
        // The marks, which are the engine's and are not in the site's payload
        // at all --- `/api/wanted` attaches them to each row as it answers, and
        // the cached copy this reads has never carried them. Taken as an
        // argument rather than copied off `it` for exactly that reason: copying
        // measured whether the payload happened to have a field, which it never
        // does, and the overlay was offered every item it had already been
        // shown. They are here because the overlay draws from this and has no
        // tickbox: a panel still naming the item you photographed an hour ago
        // is one you stop reading.
        let (have, sent) = marks(name);
        row.insert("have".into(), have.into());
        row.insert("sent".into(), sent.into());
        row.insert(
            "grades_held".into(),
            grades.iter().copied().collect::<Vec<u32>>().into(),
        );
        row.insert("blocked".into(), blocked.into());
        out.push(serde_json::Value::Object(row));
    }
    // By what it needs, then by name. Deliberately not the site's order, which
    // is how often an item drops --- "worth the trip" is the wrong axis for a
    // list whose whole premise is that no trip is needed.
    out.sort_by(|a, b| {
        let key = |v: &serde_json::Value| {
            (
                v.get("blocked").and_then(serde_json::Value::as_bool).unwrap_or(false),
                v.get("needs")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(serde_json::Value::as_str)
                            .collect::<Vec<_>>()
                            .join("+")
                    })
                    .unwrap_or_default(),
                v.get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        };
        key(a).cmp(&key(b))
    });
    out
}

/// `GET /api/items/held` --- the wanted list, narrowed to what is in the bags.
///
/// The site's wanted list is a travel plan: every item it still needs, ordered
/// by how often that item drops, tagged with the zones to go to. That is the
/// right shape for "where should I hunt" and the wrong one for the items
/// already in the player's inventory, which need no trip at all. On the dump
/// this was written against they were scattered from position 3 to position
/// 1714 of a 1,719-row list, which is indistinguishable from not being on it.
///
/// The site cannot answer this. It has never seen an inventory and has no way
/// to; this daemon folds `/outputfile inventory` into the item-id record within
/// a minute of the player typing it, so both halves are already here and
/// nothing has ever crossed them.
async fn items_held(State(state): State<AppState>) -> impl IntoResponse {
    let Some(ids) = &state.ids else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "no item-id record: this daemon has no config directory to keep one in"
            })),
        )
            .into_response();
    };
    // No answer is not an empty answer. `wanted` returns the last list it got
    // and how old it is, and `None` only when it has never reached the site at
    // all --- and an empty list here would read as "nothing to photograph",
    // which is the opposite of "nobody knows".
    let Some((wanted, age)) = state.shots.itemdb().wanted(false).await else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "the site has never answered with a wanted list, so what it needs is unknown"
            })),
        )
            .into_response();
    };
    // Whose bags, read from the files: see `my_dumps`.
    let (me, read, seen) = my_dumps(&state, ids);
    let (rows, sources) = {
        let seen: Vec<serde_json::Value> = seen
            .iter()
            .map(|b| serde_json::json!({ "file": b.file, "age_seconds": b.age_seconds }))
            .collect();
        // Locked only to read the marks, and never across the fetch above: the
        // follower thread holds this lock while ingesting lines.
        let engine = state.engine.locked("engine");
        let marks = |n: &str| (engine.has_screenshot(n), engine.has_sent(n));
        (held_and_wanted(&wanted, &bags(&read), &marks), seen)
    };
    let blocked = rows
        .iter()
        .filter(|r| r.get("blocked").and_then(serde_json::Value::as_bool) == Some(true))
        .count();
    Json(serde_json::json!({
        "count": rows.len() - blocked,
        "blocked": blocked,
        "wanted_total": wanted.get("count").cloned().unwrap_or(serde_json::Value::Null),
        "list_age_seconds": age,
        // Which snapshots this is built from and how old each one is. A
        // character who last dumped in August is not evidence about today, and
        // the caller cannot tell that from the item list alone.
        "dumps": sources,
        // Whose bags these are, so a caller can say so rather than implying
        // that every dump on the disk is one pile. Null when the daemon does
        // not know who it is following, which is also when the filter above
        // does nothing --- the two have to be reported together or the answer
        // silently changes meaning.
        "character": me.as_ref().map(|(c, _)| c.clone()),
        "server": me.as_ref().map(|(_, sv)| sv.clone()),
        "items": rows,
    }))
    .into_response()
}

fn render_encounters_markdown(encs: &[crate::engine::EncounterSummary]) -> String {
    let mut out = String::from("# Encounters\n");
    for e in encs {
        let when = time::OffsetDateTime::from_unix_timestamp(e.start)
            .map(|t| {
                format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}",
                    t.year(),
                    t.month() as u8,
                    t.day(),
                    t.hour(),
                    t.minute()
                )
            })
            .unwrap_or_default();
        out.push_str(&format!("\n## {}\n\n", e.title));
        out.push_str(&format!(
            "- **When** {when} · **{}**\n- **Zone** {}{}\n- **Build** {}\n",
            fmt_hms(e.duration),
            e.zone.as_deref().unwrap_or("unknown"),
            if e.difficulty > 0 {
                format!(" (d{})", e.difficulty)
            } else {
                String::new()
            },
            if e.build.is_empty() {
                "unknown"
            } else {
                &e.build
            },
        ));
        out.push_str(&format!(
            "- **You** {} damage, {:.0} dps · {} taken · {} deaths\n",
            e.your_damage, e.your_dps, e.your_damage_taken, e.your_deaths
        ));
        if e.kills > 0 || e.loot_count > 0 {
            out.push_str(&format!(
                "- **Result** {} kills, {} items\n",
                e.kills, e.loot_count
            ));
        }

        if !e.mobs.is_empty() {
            out.push_str("\n### What was fought\n\n| Creature | Killed | Damage into it | Its output | Loot |\n|---|--:|--:|--:|--:|\n");
            for m in &e.mobs {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    m.name, m.kills, m.damage_done_to_it, m.damage_it_did, m.loot
                ));
            }
        }

        for g in &e.outgoing_groups {
            out.push_str(&format!(
                "\n### {} — {} ({:.0} dps, {:.0}% of your damage)\n\n| Ability | Damage | DPS | Hits | Crits | Max | Avg |\n|---|--:|--:|--:|--:|--:|--:|\n",
                g.group,
                g.damage,
                g.dps,
                g.share * 100.0
            ));
            for a in &g.abilities {
                out.push_str(&format!(
                    "| {} | {} | {:.1} | {} | {} | {} | {:.0} |\n",
                    a.name, a.damage, a.dps, a.hits, a.crits, a.max, a.average
                ));
            }
        }

        if !e.resists.is_empty() {
            out.push_str(
                "\n### Resisted\n\n| Spell | Resisted | Landed | Rate |\n|---|--:|--:|--:|\n",
            );
            for r in &e.resists {
                out.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    r.spell,
                    r.resisted,
                    r.landed,
                    if r.resisted + r.landed == 0 {
                        "—".to_string()
                    } else {
                        format!("{:.0}%", r.rate() * 100.0)
                    }
                ));
            }
        }
    }
    out
}

fn render_encounters_csv(encs: &[crate::engine::EncounterSummary]) -> String {
    let mut out = String::from(
        "start,title,zone,difficulty,build,duration,your_damage,your_dps,damage_taken,deaths,kills,loot\n",
    );
    for e in encs {
        out.push_str(&format!(
            "{},\"{}\",\"{}\",{},{},{},{},{:.1},{},{},{},{}\n",
            e.start,
            e.title.replace('"', "'"),
            e.zone.as_deref().unwrap_or("").replace('"', "'"),
            e.difficulty,
            e.build,
            e.duration,
            e.your_damage,
            e.your_dps,
            e.your_damage_taken,
            e.your_deaths,
            e.kills,
            e.loot_count
        ));
    }
    out
}

/// Full detail for one fight, including per-actor and per-ability breakdowns.
async fn encounter_detail(State(s): State<AppState>, AxPath(id): AxPath<u64>) -> Response {
    let engine = s.engine.locked("engine");
    match engine.encounter(id) {
        Some(e) => Json(serde_json::json!({
            "summary": engine.summarise(e, true),
            "encounter": e,
            "charmed_pets": e.charmed_pets(),
            // Everything that fought for you, summoned pets included. Kept
            // beside `charmed_pets` rather than replacing it: the charm
            // figures — uptime, breaks — are only meaningful for charms.
            "your_pets": e.your_pets(),
            "your_damage_with_pets": e.your_damage_with_pets(),
        }))
        .into_response(),
        None => (StatusCode::NOT_FOUND, "no such encounter").into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct LootQuery {
    /// Restrict to the current play session.
    ///
    /// Taken as a string rather than a bool so `session=1` works as well as
    /// `session=true`. A strict bool rejects the whole request for a URL a
    /// person would reasonably type, and the other overlay options here
    /// already use `1`.
    #[serde(default)]
    session: Option<String>,
}

impl LootQuery {
    fn session(&self) -> bool {
        matches!(
            self.session.as_deref(),
            Some("1") | Some("true") | Some("yes") | Some("")
        )
    }
}

async fn loot(State(s): State<AppState>, Query(q): Query<LootQuery>) -> Json<serde_json::Value> {
    let engine = s.engine.locked("engine");
    let start = engine.session_start();
    let tally = engine.loot_tally_since(q.session().then_some(start));
    Json(serde_json::json!({
        "total": tally.len(),
        "items": tally,
        // Always reported, so the UI can label the session without a second
        // request and can say when it began even while showing everything.
        "session_start": start,
        "session_coin_copper": engine.session_coin(),
    }))
}

#[derive(Debug, Deserialize)]
struct LootItemQuery {
    item: String,
}

/// Drill into one item: which creatures dropped it, how often against how many
/// kills, where, and what it was worth.
///
/// Takes the name as a query parameter rather than a path segment: item names
/// contain slashes, apostrophes and plus signs, and routing on them means
/// fighting the encoding for no benefit.
async fn loot_item(State(s): State<AppState>, Query(q): Query<LootItemQuery>) -> Response {
    let engine = s.engine.locked("engine");
    match engine.loot_detail(&q.item) {
        Some(d) => Json(d).into_response(),
        None => (StatusCode::NOT_FOUND, "no such item in this log").into_response(),
    }
}

/// Level-progression analysis: per-level history, best zones, best mobs.
/// Everything that fired on its own, and how often per minute of combat.
async fn procs(State(s): State<AppState>) -> Json<eqlparser_core::procs::ProcReport> {
    Json(s.engine.locked("engine").proc_report())
}

async fn experience(State(s): State<AppState>) -> Json<crate::engine::ExperienceReport> {
    let engine = s.engine.locked("engine");
    let now = engine.now();
    Json(engine.experience_report(now))
}

/// What level each class in the loadout is.
///
/// Separate from `/api/experience`, which reports progress through whatever
/// level the client last announced. Classes level independently here, so one
/// number cannot describe a loadout.
async fn class_levels(
    State(s): State<AppState>,
) -> Json<eqlparser_core::classlevel::ClassLevelReport> {
    let engine = s.engine.locked("engine");
    Json(engine.class_level_report())
}

#[derive(Debug, Deserialize)]
struct AaSpentBody {
    spent: u32,
}

/// Record the lifetime points spent, as the character sheet shows it.
///
/// The log can only ever floor this: ranks bought before it began leave no
/// trace. The difference is a constant per character, so one correction fixes
/// the figure permanently rather than needing to be reapplied.
async fn set_aa_spent(
    State(s): State<AppState>,
    Json(body): Json<AaSpentBody>,
) -> Json<serde_json::Value> {
    let at = {
        let mut engine = s.engine.locked("engine");
        engine.set_stated_aa_spent(body.spent);
        engine.now()
    };
    if let Some(store) = &s.store {
        if let Ok(mut store) = store.lock() {
            if let Err(e) = store.put_stated("aa_spent", body.spent as i64, at) {
                return Json(serde_json::json!({"ok": false, "error": e.to_string()}));
            }
        }
    }
    Json(serde_json::json!({"ok": true, "spent": body.spent}))
}

#[derive(Debug, Deserialize)]
struct ClassLevelBody {
    /// Class abbreviation, e.g. "WAR".
    class: String,
    level: u32,
}

/// Record a level the player read off their own character sheet.
///
/// Persisted, because it is the answer to the cases inference cannot reach —
/// and because retyping it after every restart would be absurd.
async fn set_class_level(
    State(s): State<AppState>,
    Json(body): Json<ClassLevelBody>,
) -> Json<serde_json::Value> {
    let Some(class) = eqlparser_core::class::parse_class_list(&body.class)
        .into_iter()
        .next()
    else {
        return Json(serde_json::json!({"ok": false, "error": "unknown class"}));
    };
    if body.level == 0 || body.level > eqlparser_core::classlevel::LEVEL_CAP {
        return Json(serde_json::json!({"ok": false, "error": "level out of range"}));
    }
    let at = {
        let mut engine = s.engine.locked("engine");
        let at = engine.now();
        engine.set_stated_level(class, body.level, at);
        at
    };
    if let Some(store) = &s.store {
        if let Ok(mut store) = store.lock() {
            if let Err(e) = store.put_class_level(class.abbrev(), body.level, at) {
                return Json(serde_json::json!({"ok": false, "error": e.to_string()}));
            }
        }
    }
    Json(serde_json::json!({"ok": true, "class": class.abbrev(), "level": body.level}))
}

/// Weekly/daily boss lockouts, as a boss x difficulty grid.
async fn lockouts(State(s): State<AppState>) -> Json<eqlparser_core::lockout::LockoutReport> {
    let engine = s.engine.locked("engine");
    let now = engine.now();
    Json(engine.lockout_report(now))
}

/// Tradeskill progress, combines and component use.
async fn tradeskills(
    State(s): State<AppState>,
) -> Json<eqlparser_core::tradeskill::TradeskillReport> {
    let engine = s.engine.locked("engine");
    Json(engine.tradeskill_report())
}

/// The item database: recipes, drop sources, prices and combine counts, one
/// record per item, tagged with the game the observations came from.
///
/// This is the export a reference site would consume. Curated fields (recipes,
/// tools) and observed fields (drops, prices, combines) are kept in separate
/// keys so a reader can always tell which is which.
async fn items(State(s): State<AppState>) -> Json<crate::engine::ItemDatabase> {
    let mut db = { s.engine.locked("engine").item_database() };

    // Everything on file, not only what this log happens to contain — the
    // point of collecting is that it accumulates. Rows for other games are
    // left out rather than pooled: the same creature need not drop the same
    // things in both.
    if let Some(store) = &s.store {
        if let Ok(st) = store.lock() {
            let kills: std::collections::HashMap<String, u64> = st
                .all_kills()
                .unwrap_or_default()
                .into_iter()
                .filter(|(g, _, _)| *g == db.game.slug())
                .map(|(_, npc, n)| (npc.to_lowercase(), n))
                .collect();
            if let Ok(rows) = st.all_drops() {
                for (game, item, npc, zones, looted, quantity, first_seen, last_seen) in rows {
                    if game != db.game.slug() {
                        continue;
                    }
                    let sighting = eqlparser_core::drops::DropSighting {
                        kills: kills.get(&npc.to_lowercase()).copied().unwrap_or(0),
                        npc,
                        zones,
                        looted,
                        quantity,
                        // The stored table never recorded which grades were
                        // seen, only the counts, so history cannot supply a
                        // breakdown. Empty says that; a fabricated all-grade-0
                        // row would claim every historical drop was ungraded.
                        grades: Vec::new(),
                        // Same reasoning, and the same shape of answer: the
                        // stored table holds one count and a set of zone
                        // names, so it cannot say how many came from where.
                        // Empty is "not recorded", which is true of every
                        // row written before drops carried a place.
                        places: Vec::new(),
                        first_seen,
                        last_seen,
                    };
                    match db
                        .items
                        .iter_mut()
                        .find(|e| e.item.eq_ignore_ascii_case(&item))
                    {
                        Some(e) => {
                            // The stored row already includes this log's own
                            // contribution, flushed earlier, so it replaces
                            // rather than adds to what the engine reported.
                            match e
                                .dropped_by
                                .iter_mut()
                                .find(|d| d.npc.eq_ignore_ascii_case(&sighting.npc))
                            {
                                Some(d) if d.looted >= sighting.looted => {}
                                Some(d) => *d = sighting,
                                None => e.dropped_by.push(sighting),
                            }
                        }
                        None => db.items.push(crate::engine::ItemEntry {
                            item,
                            game: db.game,
                            dropped_by: vec![sighting],
                            ..Default::default()
                        }),
                    }
                }
            }
            // Merchants on file, same treatment as drops: the stored row
            // already contains this log's contribution, so it replaces rather
            // than adds to what the engine reported.
            if let Ok(rows) = st.all_vendors() {
                for (game, item, merchant, zone, unit_copper, bought, quoted, last_bought) in rows {
                    if game != db.game.slug() {
                        continue;
                    }
                    let vendor = eqlparser_core::tradeskill::VendorPrice {
                        merchant,
                        zone: Some(zone).filter(|z| !z.is_empty()),
                        unit_copper,
                        bought,
                        quoted,
                        last_bought,
                    };
                    match db
                        .items
                        .iter_mut()
                        .find(|e| e.item.eq_ignore_ascii_case(&item))
                    {
                        Some(e) => match e
                            .sold_by
                            .iter_mut()
                            .find(|v| v.merchant.eq_ignore_ascii_case(&vendor.merchant))
                        {
                            Some(v) if v.bought >= vendor.bought => {}
                            Some(v) => *v = vendor,
                            None => e.sold_by.push(vendor),
                        },
                        None => db.items.push(crate::engine::ItemEntry {
                            item,
                            game: db.game,
                            merchant_copper: Some(vendor.unit_copper),
                            bought: vendor.bought,
                            sold_by: vec![vendor],
                            ..Default::default()
                        }),
                    }
                }
            }
            db.kills = kills.into_iter().collect();
        }
    }
    for e in &mut db.items {
        e.sold_by.sort_by_key(|v| std::cmp::Reverse(v.unit_copper));
    }
    for e in &mut db.items {
        e.dropped_by.sort_by_key(|d| std::cmp::Reverse(d.looted));
    }
    db.items.sort_by_key(|e| e.item.to_lowercase());
    db.kills.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Json(db)
}

/// Creature profiles: one per (zone, mode, difficulty, name).
///
/// Every log on file, not only the one being followed. The stored side
/// deliberately excludes the current log — those rows exist too, flushed on a
/// timer, and counting them as well as the engine's live view would double
/// every kill in the file open right now. So: history from every *other* log,
/// plus the engine for this one, which also keeps the figures current between
/// flushes rather than up to a flush interval stale.
async fn bestiary(State(s): State<AppState>) -> Json<eqlparser_core::bestiary::BestiaryReport> {
    let (live, live_health, game) = {
        let e = s.engine.locked("engine");
        (e.bestiary_report(), e.bestiary_health_samples(), e.game())
    };

    let Some(store) = &s.store else {
        return Json(live);
    };
    let Ok(st) = store.lock() else {
        return Json(live);
    };
    let source = s.log_path.to_string_lossy().to_string();
    // Rows for other games are left out rather than pooled: the same creature
    // need not have the same health or hit as hard in both.
    let Ok((stored, stored_health)) = st.bestiary_excluding(game.slug(), &source) else {
        return Json(live);
    };

    let health = [live_health, stored_health].concat();
    Json(eqlparser_core::bestiary::BestiaryReport {
        game,
        npcs: eqlparser_core::bestiary::merge(live.npcs.into_iter().chain(stored), &health),
    })
}

/// What was seen dropping from what, with the kill counts behind it.
async fn drops(State(s): State<AppState>) -> Json<eqlparser_core::drops::DropReport> {
    let engine = s.engine.locked("engine");
    Json(engine.drop_report())
}

#[derive(Debug, Deserialize)]
struct IncomingQuery {
    /// Restrict to one fight; omit for every fight.
    encounter: Option<u64>,
}

/// What has been hitting you: by mechanism, ability and attacker.
async fn incoming(
    State(s): State<AppState>,
    Query(q): Query<IncomingQuery>,
) -> Json<crate::engine::IncomingReport> {
    let engine = s.engine.locked("engine");
    Json(engine.incoming_report(q.encounter))
}

#[derive(Debug, Deserialize)]
struct MarkRequest {
    name: String,
    /// "aura", "permanent", or anything else to clear.
    mark: Option<String>,
}

async fn get_marks(State(s): State<AppState>) -> Json<serde_json::Value> {
    let engine = s.engine.locked("engine");
    let marks: Vec<_> = engine
        .buff_marks()
        .into_iter()
        .map(|(n, m)| serde_json::json!({ "name": n, "mark": m }))
        .collect();
    Json(serde_json::json!({ "marks": marks }))
}

/// Declare an effect an aura or permanent, or clear the declaration.
async fn set_mark(State(s): State<AppState>, Json(req): Json<MarkRequest>) -> Response {
    let mark = match req.mark.as_deref() {
        Some("aura") => Some(eqlparser_core::spell::BuffMark::Aura),
        Some("permanent") => Some(eqlparser_core::spell::BuffMark::Permanent),
        _ => None,
    };
    {
        let mut engine = s.engine.locked("engine");
        engine.set_buff_mark(&req.name, mark);
    }
    // Republish so every client reflects it without waiting for the next line.
    let snap = { s.engine.locked("engine").snapshot(40) };
    *s.snapshot.writing("snapshot") = snap;
    let _ = s.updates.send(());
    (StatusCode::OK, "ok").into_response()
}

/// How many item lookups may be in flight at once.
///
/// The site is a small program on somebody's server, and a page listing two
/// hundred items would otherwise open two hundred connections to it at once.
/// Eight keeps a cold fetch of the whole Plane of Sky item set to a few
/// seconds without behaving like a load test.
const BRIEF_CONCURRENCY: usize = 8;

/// The most names one request may ask about, so a caller cannot turn a single
/// URL into an unbounded fan-out. The renderer chunks past this.
const BRIEF_MAX: usize = 150;

#[derive(Debug, Deserialize)]
struct BriefQuery {
    /// Item names, `|`-separated. Not commas: item names contain those, and
    /// `|` appears in none of them.
    names: Option<String>,
}

/// Icons and site links for a batch of items, in one request.
///
/// # Why a batch
///
/// A list of quest requirements wants an icon per row, and eqdb answers about
/// one item at a time. Asking from the renderer would be one request per row
/// with the window's own connection limit in the way; asking here means the
/// lookups share this process's cache — already warm for anything hovered —
/// and run a few at a time against the site.
///
/// The reply is deliberately thin. Everything needed to draw a row and link it
/// is here, and nothing else: the full record is a hover away.
async fn item_brief(State(s): State<AppState>, Query(q): Query<BriefQuery>) -> Response {
    let names: Vec<String> = q
        .names
        .unwrap_or_default()
        .split('|')
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .take(BRIEF_MAX)
        .collect();

    let mut out = serde_json::Map::new();
    for chunk in names.chunks(BRIEF_CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for name in chunk {
            let db = s.itemdb.clone();
            let name = name.clone();
            set.spawn(async move {
                // Graded drops are the same item; the site only knows the
                // ungraded name.
                let base = eqlparser_core::drops::canonical_item(&name).to_string();
                let found = db.item(&base).await;
                (name, base, found)
            });
        }
        while let Some(Ok((name, base, found))) = set.join_next().await {
            let m = found
                .as_ref()
                .and_then(|v| v.get("matches"))
                .and_then(|v| v.as_array())
                .and_then(|a| a.first());
            out.insert(
                name,
                serde_json::json!({
                    "on_site": m.is_some(),
                    // The slug the site files it under. Derived rather than
                    // read back: `/api/item` echoes the name it was asked for,
                    // and the site accepts either spelling of an apostrophe.
                    "slug": crate::itemdb::slug(&base),
                    "icon": m.and_then(|m| m.get("icon")).filter(|i| !i.is_null()),
                }),
            );
        }
    }

    Json(serde_json::json!({
        "ok": true,
        "base": s.itemdb.base(),
        "count": out.len(),
        "items": out,
    }))
    .into_response()
}

/// `GET /api/shots/recent` — what the screenshot reader has made of the last
/// few pictures.
///
/// `running: false` is the interesting case and is stated rather than implied
/// by an empty list. The reader needs a second program and a credential, so
/// "off" is a normal state and one a person will want to see named: an empty
/// `shots` array with no `running` flag reads as "nothing photographed
/// lately", which is the wrong thing to believe when the truth is that
/// nothing has been *read*.
async fn shots_recent(State(state): State<AppState>) -> impl IntoResponse {
    let (recent, waiting, tally) = match state.shots.log().lock() {
        Ok(l) => (l.recent(), l.waiting(), l.tally()),
        Err(_) => (Vec::new(), 0, crate::shots::Tally::default()),
    };
    let (have_token, token_from) = state.shots.token_state();
    Json(serde_json::json!({
        "running": state.shots.running(),
        // The sentence, not a flag. "Off" and "off because the OCR tree is
        // not where I looked" are different amounts of help, and a panel that
        // can only say the first sends somebody to read startup logs.
        "why": state.shots.why(),
        // Said separately from `why` because it is the one piece a person can
        // fix from where they are standing, and the panel offers the box.
        "token": have_token,
        "token_from": token_from,
        // Shots the client announced whose file has not turned up yet. Never
        // more than a few seconds' worth; a number that stays put means the
        // pictures are not landing where the reader is looking.
        "waiting": waiting,
        // Noticed against read, this run. `unannounced` climbing is the
        // signal: those are pictures the followed log never mentioned, which
        // is what a screenshot taken on another character looks like from
        // here. Before this the only way to see it was to count files.
        "found": tally,
        "shots": recent,
    }))
}

/// `GET /api/logs/status` — every log in the folder and how much of it has been read.
///
/// One log is followed and the rest are swept on a timer, so "is this log
/// being read" no longer has a single answer, and the number that matters is
/// per log: how far behind the parser is on each. `behind` is the whole point
/// — a log nothing has ever opened reports its own size, which is the loudest
/// possible way to say so.
///
/// The watermarks come from the history database rather than from the running
/// engine, because that is what the sweep reads and writes; an engine's own
/// position would answer for the followed log and invent zeroes for the rest.
/// An unreadable database yields no marks, so every log reads as unparsed —
/// wrong in the direction that sends somebody to look, which is the right one.
async fn logs_status(State(state): State<AppState>) -> impl IntoResponse {
    let Some(dir) = crate::catchup::folder(state.logs_dir.as_deref(), &state.log_path) else {
        return Json(serde_json::json!({ "logs": [], "why": "no folder to look in" }));
    };
    let marks = match crate::history_path() {
        Some(db) => crate::catchup::watermarks_seen(&db),
        None => Default::default(),
    };
    let logs = crate::catchup::status(&dir, &state.log_path, &|p| {
        marks.get(p).copied().unwrap_or((0, 0))
    });
    Json(serde_json::json!({
        "dir": dir.to_string_lossy(),
        "every": crate::catchup::SWEEP_INTERVAL.as_secs(),
        "logs": logs,
    }))
}

/// `POST /api/eqdb/token` — take a token and start using it.
///
/// Write-only on purpose. This daemon's API is unauthenticated and bound to
/// localhost, which is the same trust boundary as the file it writes: anything
/// that can reach this could already read `~/.config/eqlparser/eqldb-token`.
/// What it must not do is *hand the value back*, because then a stray local
/// page could take a credential rather than merely break a reader.
// ---------------------------------------------------------------------------
// The bundle
// ---------------------------------------------------------------------------
//
// What this daemon would send to the site, and what sending it would do. See
// `crate::bundle` for why the answer is a set of verdicts rather than a set of
// row counts: an ingest deletes before it writes, so an empty table this
// bundle has declared authority over is a deletion, and it looks like a
// perfectly good bundle from the outside.

/// Where the contributor's name is kept.
///
/// The config directory, with the hand-made state, because that is what it is:
/// nothing derives it and nothing can regenerate it. Deliberately not taken
/// from the upload token — a name assigned from a credential is not an
/// attribution, and rows filed under the wrong one cannot be told apart
/// afterwards.
fn contributor_path() -> Option<std::path::PathBuf> {
    crate::paths::config_file("contributor")
}

pub fn contributor() -> String {
    contributor_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

async fn get_contributor() -> impl IntoResponse {
    Json(serde_json::json!({ "contributor": contributor() }))
}

async fn set_contributor(Json(body): Json<ContributorBody>) -> impl IntoResponse {
    let name = body.contributor.trim();
    let Some(path) = contributor_path() else {
        return Json(serde_json::json!({
            "ok": false,
            "said": "nowhere to keep a config file on this machine"
        }));
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::write(&path, format!("{name}\n")) {
        Ok(()) => Json(serde_json::json!({"ok": true, "contributor": name})),
        Err(e) => Json(serde_json::json!({"ok": false, "said": e.to_string()})),
    }
}

#[derive(serde::Deserialize)]
struct ContributorBody {
    #[serde(default)]
    contributor: String,
}

/// The bundle as it stands right now.
///
/// Built for the panel and built again for the send, rather than built once
/// and held. A bundle is a statement about what this machine knows *at the
/// moment it was written*, and sending one a person looked at ten minutes ago
/// would send a document that no longer describes anything. The cost is one
/// pass over the record; the alternative is a cache whose staleness nobody
/// can see.
fn build_current(state: &AppState) -> Result<eqlparser_core::bundle::Bundle, String> {
    let store = state
        .store
        .as_ref()
        .ok_or("no history database, so there is nothing to send")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // The registry is cloned out from under its lock rather than held across
    // the build: the sweep thread wants it back every minute.
    let ids = state
        .ids
        .as_ref()
        .map(|i| i.locked("item ids").registry().clone());
    let st = store.locked("bundle");
    crate::bundle::build(&st, ids.as_ref(), &contributor(), now).map_err(|e| e.to_string())
}

async fn bundle_preview(State(state): State<AppState>) -> impl IntoResponse {
    match build_current(&state) {
        Ok(b) => Json(serde_json::to_value(crate::bundle::preview(&b)).unwrap_or_default()),
        Err(said) => Json(serde_json::json!({"ok": false, "said": said})),
    }
}

/// `POST /api/bundle/send` — build it, check it, and hand it to the site.
///
/// # The panel's verdicts are the gate, not a display
///
/// `preview` already decides whether a document is safe to send and says why
/// in `blocked`; this refuses on exactly that, so the answer a person reads
/// and the answer the machine acts on cannot disagree. A panel that advises
/// against sending and a button that sends anyway is two rules, and the one
/// that runs is the one nobody read.
///
/// Today `blocked` catches an unattributed bundle and any table that would be
/// emptied. Neither can arise from this daemon as it stands — no source it
/// declares covers a table it leaves empty — which is the point: the guard is
/// for the change that has not been made yet.
///
/// # What this does not do
///
/// It does not ingest. `POST /api/bundle` files the document and answers 202,
/// and somebody runs `deploy/worker.sh --bundles` beside the database
/// afterwards. That boundary is deliberate on the site's side — the web
/// process cannot write `game` — and nothing here should read as though a
/// send has changed a page.
async fn bundle_send(State(state): State<AppState>) -> impl IntoResponse {
    let b = match build_current(&state) {
        Ok(b) => b,
        Err(said) => return Json(serde_json::json!({"ok": false, "said": said})),
    };
    let p = crate::bundle::preview(&b);
    if !p.blocked.is_empty() {
        return Json(serde_json::json!({
            "ok": false,
            "said": "this bundle is not safe to send, so it was not sent",
            "blocked": p.blocked,
        }));
    }
    let site = match crate::site::Site::new(state.itemdb.base()) {
        Ok(s) => s,
        Err(said) => return Json(serde_json::json!({"ok": false, "said": said})),
    };
    let json = match serde_json::to_vec(&b) {
        Ok(v) => v,
        Err(e) => return Json(serde_json::json!({"ok": false, "said": e.to_string()})),
    };
    let body = match crate::site::gzip(&json) {
        Ok(v) => v,
        Err(said) => return Json(serde_json::json!({"ok": false, "said": said})),
    };
    // `X-Contributor` is provenance beside the document, not authority over
    // it: the site checks the name *in* the bundle against the token's and
    // 403s a mismatch. See `api_upload_bundle`.
    //
    // Narrowed to the characters the site keeps anyway, because a header
    // value is not a string: a name with an accent in it makes `reqwest`
    // refuse to build the request, and the whole send would fail with
    // "invalid header value" over a field nothing depends on. The name that
    // matters travelled inside the document, where it is bytes.
    let from: String = b
        .contributor
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' ' | '\''))
        .take(60)
        .collect();
    let sent_bytes = body.len();
    let sent = site
        .post(
            "/api/bundle",
            reqwest::Body::from(body),
            "application/octet-stream",
            &[("X-Contributor", from.trim())],
        )
        .await;
    match sent {
        Ok(v) => Json(serde_json::json!({
            "ok": true,
            // What happens next is a person reading it, not a command. The
            // message used to name `deploy/worker.sh --bundles`, which is
            // wrong twice: it skips the gate -- an upload is admitted by
            // somebody logged in at /bundles, and the worker ingests only
            // what has been admitted -- and it hands an operator command to
            // a contributor who has no shell on that server and could do
            // nothing with it.
            "said": format!(
                "{} rows sent to {} as {} KB. Pending review: it is filed and waiting \
                 for somebody to accept it at {}/bundles before anything is ingested.",
                p.rows,
                site.base(),
                sent_bytes.div_ceil(1024),
                site.base(),
            ),
            "rows": p.rows,
            "bytes": json.len(),
            "sent_bytes": sent_bytes,
            "reply": v,
        })),
        Err(said) => Json(serde_json::json!({"ok": false, "said": said})),
    }
}

async fn set_eqdb_token(
    State(state): State<AppState>,
    Json(body): Json<TokenBody>,
) -> impl IntoResponse {
    match state.shots.set_token(&body.token) {
        Ok(said) => Json(serde_json::json!({"ok": true, "said": said})),
        Err(why) => Json(serde_json::json!({"ok": false, "said": why})),
    }
}

#[derive(serde::Deserialize)]
struct TokenBody {
    #[serde(default)]
    token: String,
}

// ---------------------------------------------------------------------------
// Screenshots wanted
// ---------------------------------------------------------------------------
//
// eqdb publishes the items it still has no tooltip or icon for. It takes no
// filters and answers with the whole list, so it is fetched here — once, and
// cached — rather than by each panel that wants it. Going through the daemon
// also keeps the window's content-security policy closed to remote origins,
// the same reason item lookups come this way.
//
// Cross-referencing against loot happens here too, so the window and the
// overlay cannot disagree about whether an item is worth photographing.

/// Pull the item list out of the site's payload, keyed for matching.
///
/// Anything without a usable name is dropped rather than carried as a row that
/// can never match anything.
fn wanted_index(payload: &serde_json::Value) -> HashMap<String, &serde_json::Value> {
    let mut out = HashMap::new();
    let Some(items) = payload.get("items").and_then(|v| v.as_array()) else {
        return out;
    };
    for it in items {
        let Some(name) = it.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let key = crate::itemdb::match_key(name);
        if !key.is_empty() {
            out.insert(key, it);
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct WantedQuery {
    /// `refresh=1` re-asks the site instead of using the cached list. The
    /// button behind it exists because the list changes when *you* upload a
    /// screenshot, which is exactly when waiting ten minutes feels broken.
    #[serde(default)]
    refresh: Option<String>,
}

/// The whole wanted list, each item carrying whether it is already ticked off.
///
/// `age_seconds` is how old the copy is. It is reported rather than hidden
/// because a list that could not be refreshed is still the best list there is,
/// and the caller should be able to say so.
async fn wanted(State(s): State<AppState>, Query(q): Query<WantedQuery>) -> Response {
    let force = matches!(q.refresh.as_deref(), Some("1") | Some("true"));
    let got = s.itemdb.wanted(force).await;
    let Some((payload, age)) = got else {
        return Json(serde_json::json!({
            "ok": false,
            "base": s.itemdb.base(),
            "error": "the item site could not be reached, and nothing has been fetched yet",
            "items": [],
            "count": 0,
            "have_count": 0,
        }))
        .into_response();
    };

    // The engine is locked only to read the marks, and never across the fetch
    // above: the follower thread holds this lock while ingesting lines.
    let (items, marked_total, sent_total): (Vec<serde_json::Value>, usize, usize) = {
        let engine = s.engine.locked("engine");
        let marked_total = engine.screenshot_marks().len();
        let sent_total = engine.sent_marks().len();
        let items = payload
            .get("items")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|it| {
                        let mut row = it.clone();
                        let name = it.get("name").and_then(|v| v.as_str());
                        let have = name.is_some_and(|n| engine.has_screenshot(n));
                        // Whether a reading of it actually reached the queue,
                        // which `have` does not say: `have` is set the moment
                        // a window is read, and by hand, and neither means
                        // the site has the picture.
                        let sent = name.is_some_and(|n| engine.has_sent(n));
                        if let Some(o) = row.as_object_mut() {
                            o.insert("have".into(), serde_json::Value::Bool(have));
                            o.insert("sent".into(), serde_json::Value::Bool(sent));
                        }
                        row
                    })
                    .collect()
            })
            .unwrap_or_default();
        (items, marked_total, sent_total)
    };
    let have_count = items
        .iter()
        .filter(|i| i.get("have").and_then(|v| v.as_bool()).unwrap_or(false))
        .count();

    Json(serde_json::json!({
        "ok": true,
        "base": s.itemdb.base(),
        "age_seconds": age,
        "count": items.len(),
        "have_count": have_count,
        // Every item ever ticked off, including those the site has since
        // published and dropped from the list. The difference between this and
        // `have_count` is how many of your screenshots have landed.
        "marked_total": marked_total,
        // Every item a reading has been sent for, including those the site
        // has since published and dropped from the list.
        "sent_total": sent_total,
        "needing_tooltip": payload.get("needing_tooltip"),
        "needing_icon": payload.get("needing_icon"),
        // The levels of the zones this list sends somebody to, keyed by
        // zone name. Forwarded verbatim rather than re-modelled: eqldb owns
        // what a zone's levels are, and this end has no business having an
        // opinion about them.
        //
        // Forwarded at all because this handler rebuilds the payload rather
        // than relaying it, so anything not named here is dropped -- which is
        // the right default and does mean a new block on the site is invisible
        // until it is named.
        "zones": payload.get("zones"),
        "items": items,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
struct WantedMarkRequest {
    item: String,
    /// Absent means "ticked".
    have: Option<bool>,
}

/// Tick an item off the screenshot list, or untick it.
async fn set_wanted_mark(
    State(s): State<AppState>,
    Json(req): Json<WantedMarkRequest>,
) -> Response {
    let mut engine = s.engine.locked("engine");
    engine.set_screenshot_mark(&req.item, req.have.unwrap_or(true));
    let have = engine.has_screenshot(&req.item);
    Json(serde_json::json!({ "ok": true, "item": req.item, "have": have })).into_response()
}

#[derive(Debug, Deserialize)]
struct WantedRecentQuery {
    /// How far back to look, in seconds.
    window: Option<i64>,
    /// How many rows to return. The total is reported regardless.
    limit: Option<usize>,
}

/// Items you have picked up recently that the site still wants a picture of.
///
/// The window is deliberately short and the default is an hour: this answers
/// "photograph this before you forget", not "what have you ever looted". An
/// item from three days ago has very likely been sold, banked or destroyed,
/// and a list full of things you no longer have is one nobody reads.
async fn wanted_recent(State(s): State<AppState>, Query(q): Query<WantedRecentQuery>) -> Response {
    let window = q.window.unwrap_or(3600).clamp(60, 86_400);
    let limit = q.limit.unwrap_or(5).clamp(1, 50);

    let Some((payload, age)) = s.itemdb.wanted(false).await else {
        // No list means no opinion. Saying "nothing wanted" would be a claim,
        // and the panel would hide itself on the strength of it.
        return Json(serde_json::json!({
            "ok": false,
            "window_seconds": window,
            "total": 0,
            "items": [],
        }))
        .into_response();
    };
    let index = wanted_index(&payload);

    let (pickups, marks): (Vec<crate::engine::RecentPickup>, Vec<bool>) = {
        let engine = s.engine.locked("engine");
        let since = engine.now() - window;
        let p = engine.recent_pickups(since);
        let m = p.iter().map(|x| engine.has_screenshot(&x.item)).collect();
        (p, m)
    };

    let unmarked: Vec<&crate::engine::RecentPickup> = pickups
        .iter()
        .zip(&marks)
        .filter(|(_, marked)| !**marked)
        .map(|(p, _)| p)
        .collect();

    // Already on the list: the site knows the item and is asking for a picture.
    let mut wanted_rows: Vec<serde_json::Value> = Vec::new();
    // Everything else is a candidate for being brand new, which the list
    // cannot tell us about by construction — it only names items eqdb has
    // already seen.
    let mut unlisted: Vec<&crate::engine::RecentPickup> = Vec::new();
    for p in unmarked {
        match index.get(&crate::itemdb::match_key(&p.item)) {
            Some(it) => {
                let known = zone_list(it.get("zones"));
                wanted_rows.push(serde_json::json!({
                    "item": p.item,
                    "at": p.at,
                    "count": p.count,
                    "source": p.source,
                    // Where to point the camera: your bags, or the item link the
                    // client wrote into chat as it sold the thing.
                    "held": p.held,
                    "new": false,
                    "slug": it.get("slug"),
                    "needs": it.get("needs"),
                    "seen": it.get("seen"),
                    "zone": p.zone,
                    "new_zone": new_zone(p.zone.as_deref(), &known),
                    "known_zones": known,
                }))
            }
            None => unlisted.push(p),
        }
    }

    let (new_rows, zone_rows) = unlisted_rows(&s, unlisted).await;

    /* The order, and why the new zones are not second.

       Undiscovered first. An item the site has never seen is the one nobody
       else can contribute, and the window for it is however long the thing
       stays in your bags or the link stays in your chat.

       Then the wanted list, because a picture is what this panel is for and an
       item eqldb is asking about is one you can act on in ten seconds. Within
       it, the ones that also dropped somewhere unrecorded lead: same job, more
       to say.

       Last, the items that are only a new drop location. They are real — a
       zone nobody has contributed makes every drop in it one — and that is
       exactly why they cannot go second: standing in such a zone produced
       eleven of them against two discoveries on the log this was built
       against, which in a five-row overlay would leave no room for the thing
       the panel exists to prompt. Nothing is photographable in them, so they
       are the ones to lose to the cap. */
    let (unrecorded, listed): (Vec<serde_json::Value>, Vec<serde_json::Value>) =
        wanted_rows.into_iter().partition(|r| {
            r.get("new_zone").and_then(|v| v.as_bool()).unwrap_or(false)
        });
    let total = new_rows.len() + zone_rows.len() + unrecorded.len() + listed.len();
    let items: Vec<serde_json::Value> = new_rows
        .into_iter()
        .chain(unrecorded)
        .chain(listed)
        .chain(zone_rows)
        .take(limit)
        .collect();

    Json(serde_json::json!({
        "ok": true,
        "window_seconds": window,
        "age_seconds": age,
        // The full count, so a capped list can say what it is hiding rather
        // than quietly presenting five as the whole answer.
        "total": total,
        "items": items,
    }))
    .into_response()
}

/// The zones a wanted-list row names, as plain strings.
fn zone_list(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(|z| z.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|z| z.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Did you loot this somewhere the site has never recorded it?
///
/// Three things have to be true, and the two guards are the point: a pickup
/// the log could not place says nothing, and neither does an item the site
/// knows but has placed nowhere at all. Both would otherwise read as "new
/// here" about every item in the game, which is the way to make the flag
/// worthless.
fn new_zone(here: Option<&str>, known: &[String]) -> bool {
    let Some(here) = here else {
        return false;
    };
    !known.is_empty() && !known.iter().any(|z| z.eq_ignore_ascii_case(here))
}

/// The most lookups one request will spend asking the site about drops the
/// wanted list does not name.
///
/// A busy hour in a new zone can turn over a lot of distinct drops, and each
/// unlisted one costs a request to the site. The cap bounds that; anything
/// past it waits for the next poll, by which time the earlier answers are
/// cached.
const DISCOVERY_MAX: usize = 40;

/// Of the recent drops the wanted list does not name, the two kinds worth
/// saying something about: the ones the site has never heard of, and the ones
/// it knows but has never seen drop where you found them.
///
/// # Why this is separate from the wanted list
///
/// eqdb can only ask for a picture of an item it knows exists. A drop nobody
/// has contributed yet is absent from the list for the same reason it is worth
/// photographing: it is new. Waiting for the site to learn about it means
/// waiting for someone else to loot it, write it up, and publish — by which
/// time yours has been sold.
///
/// The second kind is absent from the list for the opposite reason: the item
/// is fully written up, so nobody is asking for anything. What the site is
/// missing is smaller and invisible from the list — an item it has only ever
/// been shown in Befallen, dropping for you in Paineel. It costs the same one
/// lookup to notice, since the answer to "has the site heard of this" carries
/// the zones with it.
///
/// A lookup that cannot reach the site returns `None` and the item is skipped.
/// "The site did not answer" is not evidence of discovery, and a panel that
/// announced a dozen discoveries every time the network blinked would be worth
/// less than no panel.
///
/// Returned as two lists rather than one so the caller can order them without
/// re-reading a flag off the rows it was handed.
async fn unlisted_rows(
    s: &AppState,
    candidates: Vec<&crate::engine::RecentPickup>,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let mut fresh = Vec::new();
    let mut elsewhere = Vec::new();
    for chunk in candidates.chunks(BRIEF_CONCURRENCY).take(DISCOVERY_MAX / BRIEF_CONCURRENCY + 1) {
        let mut set = tokio::task::JoinSet::new();
        for p in chunk {
            let db = s.itemdb.clone();
            let name = p.item.clone();
            let row = serde_json::json!({
                "item": p.item,
                "at": p.at,
                "count": p.count,
                "source": p.source,
                "held": p.held,
                "new": true,
                "slug": crate::itemdb::slug(&p.item),
                // The site has nothing at all, so it needs everything.
                "needs": ["tooltip", "icon"],
                "seen": 0,
                "zone": p.zone,
                // Nowhere to be new to: the item itself is the news.
                "new_zone": false,
                "known_zones": [],
            });
            let here = p.zone.clone();
            set.spawn(async move { (db.item_zones(&name).await, here, row) });
        }
        while let Some(Ok((zones, here, mut row))) = set.join_next().await {
            match zones {
                // The site answered, and has no record of the item.
                Some(None) => fresh.push(row),
                // Known — interesting only if you found it somewhere new.
                Some(Some(known)) => {
                    if new_zone(here.as_deref(), &known) {
                        if let Some(o) = row.as_object_mut() {
                            o.insert("new".into(), serde_json::Value::Bool(false));
                            o.insert("new_zone".into(), serde_json::Value::Bool(true));
                            o.insert("known_zones".into(), serde_json::json!(known));
                            // It is on no wanted list, so the site is asking
                            // for no picture of it. Saying it needs a tooltip
                            // would be a claim about eqdb that is not true.
                            o.insert("needs".into(), serde_json::json!([]));
                        }
                        elsewhere.push(row);
                    }
                }
                // The site did not answer. No claim either way.
                None => {}
            }
        }
    }
    // Newest first, as with the rest of the panel.
    let newest = |out: &mut Vec<serde_json::Value>| {
        out.sort_by_key(|r| std::cmp::Reverse(r.get("at").and_then(|v| v.as_i64()).unwrap_or(0)));
    };
    newest(&mut fresh);
    newest(&mut elsewhere);
    (fresh, elsewhere)
}

// ---------------------------------------------------------------------------
// Alerts
// ---------------------------------------------------------------------------

/// The rule set, as the engine holds it.
///
/// Served from the engine rather than the database so a rule whose regular
/// expression failed to compile comes back carrying its error — the store only
/// knows what was written, not whether it works.
async fn alerts(State(s): State<AppState>) -> Json<serde_json::Value> {
    let engine = s.engine.locked("engine");
    Json(serde_json::json!({
        "rules": engine.alert_rules(),
        "persisted": s.store.is_some(),
    }))
}

/// Add or replace one rule.
async fn put_alert(
    State(s): State<AppState>,
    Json(rule): Json<eqlparser_core::alert::AlertRule>,
) -> Response {
    if rule.id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "an alert needs an id").into_response();
    }
    if rule.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "an alert needs a name").into_response();
    }
    {
        let mut engine = s.engine.locked("engine");
        engine.put_alert_rule(rule.clone());
    }
    if let Some(store) = &s.store {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Err(e) = store.locked("history").put_alert(&rule, now) {
            // The rule is live either way; say plainly that it will not
            // survive a restart rather than pretend it saved.
            return (StatusCode::OK, format!("active, but not saved: {e}")).into_response();
        }
    }
    // Hand back the stored rule so the client sees a pattern error straight
    // away rather than on the next load.
    let engine = s.engine.locked("engine");
    match engine.alert_rules().iter().find(|r| r.id == rule.id) {
        Some(r) => Json(r).into_response(),
        None => (StatusCode::OK, "ok").into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct AlertIdRequest {
    id: String,
}

async fn delete_alert(State(s): State<AppState>, Json(req): Json<AlertIdRequest>) -> Response {
    {
        let mut engine = s.engine.locked("engine");
        engine.remove_alert_rule(&req.id);
    }
    if let Some(store) = &s.store {
        let _ = store.locked("history").delete_alert(&req.id);
    }
    (StatusCode::OK, "ok").into_response()
}

#[derive(Debug, Deserialize)]
struct SinceQuery {
    #[serde(default)]
    since: Option<u64>,
}

/// The last lines to arrive, for building a rule from something just seen.
///
/// `since` is the highest sequence the client already has, so polling costs
/// nothing when the log is quiet. Live lines only — see the engine.
async fn lines(State(s): State<AppState>, Query(q): Query<SinceQuery>) -> Json<serde_json::Value> {
    let engine = s.engine.locked("engine");
    let lines = engine.lines_since(q.since.unwrap_or(0));
    Json(serde_json::json!({
        "lines": lines,
        "live": engine.is_live(),
    }))
}

#[derive(Debug, Deserialize)]
struct ScopeQuery {
    /// "session" (default) or "zone".
    #[serde(default)]
    scope: Option<String>,
}

/// Every fight of this session as one meter.
///
/// Served on request rather than carried on the snapshot: it merges every
/// encounter held in memory, and paying that on each publish would put it on
/// the path the overlay and the WebSocket share. The dashboard asks for it
/// only while the scope switch is on it.
async fn session(
    State(s): State<AppState>,
    Query(q): Query<ScopeQuery>,
) -> Json<serde_json::Value> {
    let zone_only = q.scope.as_deref() == Some("zone");
    let engine = s.engine.locked("engine");
    let (summary, fights, combat) = engine.session_summary(zone_only);
    Json(serde_json::json!({
        "summary": summary,
        "fights": fights,
        // The denominator behind every rate above, so the UI can state it
        // rather than let it be read as wall-clock time.
        "combat_seconds": combat,
        "session_start": engine.session_start(),
        "zone_only": zone_only,
    }))
}

/// Assigned tasks with step counts, and achievements earned.
async fn tasks(State(s): State<AppState>) -> Json<eqlparser_core::task::TaskReport> {
    let engine = s.engine.locked("engine");
    Json(engine.task_report())
}

/// Seed a task's objectives. The client never reports them, so this is the
/// only way they can ever be known.
async fn define_task(
    State(s): State<AppState>,
    Json(def): Json<eqlparser_core::task::TaskDef>,
) -> Response {
    let mut engine = s.engine.locked("engine");
    engine.define_task(def);
    (StatusCode::OK, "ok").into_response()
}

#[derive(Debug, Deserialize)]
struct ExportQuery {
    /// "md" (default) or "csv".
    #[serde(default)]
    format: Option<String>,
    /// Restrict to one build, e.g. "WAR/DRU/BRD".
    #[serde(default)]
    build: Option<String>,
}

/// Raid statistics for writing a build guide.
///
/// Served from the durable store rather than the live engine, so it covers
/// every log file ever parsed rather than just the one being followed — which
/// is the whole point of writing a guide from it.
async fn export(State(s): State<AppState>, Query(q): Query<ExportQuery>) -> Response {
    let Some(store) = &s.store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "history database unavailable",
        )
            .into_response();
    };
    let stats = {
        let st = store.locked("history");
        match st.raid_stats() {
            Ok(v) => v,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    };
    let stats: Vec<_> = match &q.build {
        Some(b) if !b.is_empty() => stats.into_iter().filter(|r| &r.build == b).collect(),
        _ => stats,
    };

    let csv = q.format.as_deref() == Some("csv");
    let body = if csv {
        render_export_csv(&stats)
    } else {
        render_export_markdown(&stats)
    };
    let ctype = if csv { "text/csv" } else { "text/markdown" };
    let name = if csv { "raids.csv" } else { "raids.md" };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, format!("{ctype}; charset=utf-8")),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        body,
    )
        .into_response()
}

/// `1h02m03s`, or `—` when there is no timing to report.
fn hms(seconds: f64) -> String {
    if seconds <= 0.0 {
        return "—".to_string();
    }
    let s = seconds.round() as i64;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h{m:02}m{sec:02}s")
    } else {
        format!("{m}m{sec:02}s")
    }
}

fn render_export_markdown(stats: &[crate::store::RaidStat]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    out.push_str("# Raid history\n\n");
    if stats.is_empty() {
        out.push_str("No boss kills recorded yet.\n");
        return out;
    }

    // Grouped by build, because a guide is written about one loadout.
    let mut builds: Vec<&str> = stats.iter().map(|r| r.build.as_str()).collect();
    builds.sort_unstable();
    builds.dedup();

    for build in builds {
        let rows: Vec<_> = stats.iter().filter(|r| r.build == build).collect();
        let total: u32 = rows.iter().map(|r| r.kills).sum();
        let label = if build.is_empty() {
            "unknown build"
        } else {
            build
        };
        let _ = writeln!(out, "## {label}\n");
        let _ = writeln!(out, "{total} boss kills across {} entries.\n", rows.len());
        out.push_str("| Boss | Diff | Kills | Avg time | Best time | Avg DPS | Best DPS |\n");
        out.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
        for r in rows {
            let _ = writeln!(
                out,
                "| {} | d{} | {} | {} | {} | {:.0} | {:.0} |",
                r.boss,
                r.difficulty,
                r.kills,
                hms(r.avg_seconds),
                hms(r.best_seconds as f64),
                r.avg_dps,
                r.best_dps,
            );
        }
        out.push('\n');
    }
    out.push_str(
        "_DPS is your own damage; charmed-pet damage is tracked separately. \
         Timing and DPS averages exclude kills whose fight could not be bounded._\n",
    );
    out
}

fn render_export_csv(stats: &[crate::store::RaidStat]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    out.push_str("build,boss,difficulty,kills,avg_seconds,best_seconds,avg_dps,best_dps,first_kill,last_kill\n");
    for r in stats {
        // Boss names contain no commas today, but quoting costs nothing and
        // stops a future one silently shifting every column.
        let _ = writeln!(
            out,
            "\"{}\",\"{}\",{},{},{:.0},{},{:.1},{:.1},{},{}",
            r.build,
            r.boss,
            r.difficulty,
            r.kills,
            r.avg_seconds,
            r.best_seconds,
            r.avg_dps,
            r.best_dps,
            r.first_kill,
            r.last_kill
        );
    }
    out
}

/// The log files available to follow.
///
/// Listed from the directory of the log currently being followed, so it works
/// whether the daemon was pointed at a file or a directory. Only `eqlog_*.txt`
/// counts: the client writes several other files into the same folder and none
/// of them are combat logs.
async fn logs(State(s): State<AppState>) -> Json<serde_json::Value> {
    let dir = s
        .logs_dir
        .clone()
        .or_else(|| s.log_path.parent().map(|p| p.to_path_buf()));
    let mut out: Vec<serde_json::Value> = Vec::new();
    if let Some(dir) = &dir {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let path = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if !(name.starts_with("eqlog_") && name.ends_with(".txt")) {
                    continue;
                }
                let md = e.metadata().ok();
                let modified = md
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                // The client names the file after the character and server,
                // which is the only place the daemon learns either from.
                let ident = eqlparser_core::Parser::identity_from_filename(&name);
                out.push(serde_json::json!({
                    "path": path.to_string_lossy(),
                    "name": name,
                    "character": ident.as_ref().map(|(c, _)| c.clone()),
                    "server": ident.as_ref().map(|(_, sv)| sv.clone()),
                    "bytes": md.as_ref().map(|m| m.len()).unwrap_or(0),
                    "modified": modified,
                    "current": path == s.log_path,
                }));
            }
        }
    }
    // Most recently written first: the one you just played is the one you
    // most likely want.
    out.sort_by(|a, b| b["modified"].as_i64().cmp(&a["modified"].as_i64()));
    // Who the history already holds records for, so switching logs shows what
    // is kept separately rather than implying one merged pile.
    let (mut known, mut following) = (Vec::new(), serde_json::Value::Null);
    if let Some(store) = &s.store {
        if let Ok(store) = store.lock() {
            let (c, sv) = store.identity();
            following = serde_json::json!({"character": c, "server": sv});
            if let Ok(rows) = store.characters() {
                known = rows
                    .into_iter()
                    .map(|(c, sv, n)| {
                        serde_json::json!({
                            "character": c, "server": sv, "kills": n,
                        })
                    })
                    .collect();
            }
        }
    }
    Json(serde_json::json!({
        "dir": dir.map(|d| d.to_string_lossy().to_string()),
        "current": s.log_path.to_string_lossy(),
        "following": following,
        "characters": known,
        "logs": out,
    }))
}

/// Durable history: everything kept across log files.
///
/// Distinct from `/api/builds` and `/api/lockouts`, which only ever see the
/// log currently being followed. This is the one that survives a rotation.
async fn history(State(s): State<AppState>) -> Json<serde_json::Value> {
    let Some(store) = &s.store else {
        return Json(serde_json::json!({
            "available": false,
            "reason": "history database could not be opened",
        }));
    };
    let st = store.locked("history");
    let kills = st.kills().unwrap_or_default();
    let levels = st.levels().unwrap_or_default();
    let sources = st.sources().unwrap_or_default();
    let (nk, ne, nl) = st.counts().unwrap_or((0, 0, 0));
    Json(serde_json::json!({
        "available": true,
        "kills": kills,
        "levels": levels,
        "sources": sources.iter().map(|(p, seen, bytes)| serde_json::json!({
            "path": p, "last_seen": seen, "bytes": bytes,
        })).collect::<Vec<_>>(),
        "counts": { "kills": nk, "encounters": ne, "levels": nl },
    }))
}

/// Every raid boss kill on file, for the site's leaderboard.
///
/// # Why this is served and not exported to a file
///
/// Because the daemon owns `history.db` and is holding it open in WAL mode.
/// Reading it from another process while this one is appending is the kind of
/// thing that works until it does not, and the failure is a short read rather
/// than an error. Asking the owner is free.
///
/// # Why it is unscoped
///
/// Every other reader here wants the character being followed. This one wants
/// all of them: the leaderboard compares builds, and on this machine four of
/// the nine builds on file belong to a character nobody is playing. Scoping it
/// to the current one would silently publish a leaderboard missing half its
/// history, which is worse than one that is plainly incomplete.
///
/// The boss names are the parser's own. Reconciling them with the bestiary's
/// spelling — `Innoruuk` against `Innoruuk, the Prince of Hate` — happens at
/// the other end, in `eqldb-ingest`, where the bestiary is. A daemon that knew
/// the site's spellings would be a daemon that has to be redeployed when the
/// site renames a creature.
///
///     curl -s localhost:PORT/api/raid-kills > kills.json
///     eqldb-ingest --raid-kills kills.json
async fn raid_kills(State(s): State<AppState>) -> Response {
    let Some(store) = &s.store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "history database unavailable, so there is nothing durable to export",
        )
            .into_response();
    };
    let st = store.locked("history");
    // The same rule a bundle obeys: nothing from the test server or a scratch
    // log reaches the site. See `Store::played_only`.
    let _played = match st.played_only() {
        Ok(g) => g,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let kills = match st.exported_kills() {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let scopes = match st.kill_scopes() {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // How many kills could not be placed in a fight. Reported rather than left
    // to be counted at the far end, because this is the side that knows why:
    // the encounter was pruned, or the kill predates the encounter table.
    let orphans = kills.iter().filter(|k| k.enc_start == 0).count();
    Json(serde_json::json!({
        "covers": scopes.iter().map(|(c, sv)| serde_json::json!({
            "character": c, "server": sv,
        })).collect::<Vec<_>>(),
        "kills": kills,
        "orphans": orphans,
    }))
    .into_response()
}

/// Captured NPC interactions, the raw material for a quest write-up.
async fn npcs(State(s): State<AppState>) -> Json<eqlparser_core::npc::NpcReport> {
    let engine = s.engine.locked("engine");
    Json(engine.npc_report())
}

/// What forage has yielded, per zone.
///
/// The log's own stem is the source tag, the same one every other record
/// report carries.
async fn forage(State(s): State<AppState>) -> Json<crate::record::ForageReport> {
    // The history, not the live engine: a forage record is the whole of what
    // has ever been seen, and the engine holds only the log it is following.
    // The engine is the fallback for a daemon started without a store.
    if let Some(store) = &s.store {
        let st = store.locked("history");
        if let Ok(r) = st.record_reports(crate::record::GAME_TAG) {
            return Json(r.forage);
        }
    }
    let engine = s.engine.locked("engine");
    Json(engine.record().forage("live"))
}

/// The quest database.
async fn quests(State(s): State<AppState>) -> Json<serde_json::Value> {
    let Some(store) = &s.store else {
        return Json(serde_json::json!({ "available": false, "quests": [] }));
    };
    let st = store.locked("history");
    Json(serde_json::json!({
        "available": true,
        "quests": st.quests().unwrap_or_default(),
    }))
}

/// Create or replace one quest.
async fn put_quest(
    State(s): State<AppState>,
    Json(mut q): Json<crate::store::QuestRecord>,
) -> Response {
    if q.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "a quest needs a name").into_response();
    }
    let Some(store) = &s.store else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no quest database").into_response();
    };
    q.updated = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match store.locked("history").put_quest(&q) {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct NameBody {
    name: String,
}

async fn delete_quest(State(s): State<AppState>, Json(b): Json<NameBody>) -> Response {
    let Some(store) = &s.store else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no quest database").into_response();
    };
    match store.locked("history").delete_quest(&b.name) {
        Ok(true) => (StatusCode::OK, "deleted").into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such quest").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The quest database as Markdown, for pasting into a written guide.
async fn export_quests(State(s): State<AppState>) -> Response {
    let Some(store) = &s.store else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no quest database").into_response();
    };
    let quests = store.locked("history").quests().unwrap_or_default();
    let body = render_quests_markdown(&quests);
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "text/markdown; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"quests.md\"".to_string(),
            ),
        ],
        body,
    )
        .into_response()
}

fn render_quests_markdown(quests: &[crate::store::QuestRecord]) -> String {
    use std::fmt::Write;
    let mut out = String::from("# Quests\n\n");
    if quests.is_empty() {
        out.push_str("Nothing recorded yet.\n");
        return out;
    }
    for q in quests {
        let _ = writeln!(out, "## {}\n", q.name);
        let mut meta: Vec<String> = Vec::new();
        if !q.giver.is_empty() {
            meta.push(format!("**Giver:** {}", q.giver));
        }
        if !q.zone.is_empty() {
            meta.push(format!("**Zone:** {}", q.zone));
        }
        if !q.classes.is_empty() {
            meta.push(format!("**Classes:** {}", q.classes.join(", ")));
        }
        if !meta.is_empty() {
            let _ = writeln!(out, "{}\n", meta.join(" · "));
        }
        if !q.start.is_empty() {
            let _ = writeln!(out, "**How to start:** {}\n", q.start);
        }
        if !q.steps.is_empty() {
            out.push_str("### Steps\n\n");
            for (i, st) in q.steps.iter().enumerate() {
                let _ = writeln!(out, "{}. {}", i + 1, st);
            }
            out.push('\n');
        }
        if !q.items.is_empty() {
            out.push_str("### Items\n\n");
            for it in &q.items {
                let _ = writeln!(out, "- {it}");
            }
            out.push('\n');
        }
        if !q.reward.is_empty() {
            let _ = writeln!(out, "**Reward:** {}\n", q.reward);
        }
        if !q.notes.is_empty() {
            let _ = writeln!(out, "{}\n", q.notes);
        }
        if !q.dialogue.is_empty() {
            // Verbatim and blockquoted: the exact words are the part a written
            // guide cannot reconstruct later.
            out.push_str("### Quest text\n\n");
            for l in &q.dialogue {
                let _ = writeln!(out, "> **{}:** {}\n", l.speaker, l.text);
            }
        }
        out.push_str("---\n\n");
    }
    out
}

/// Private conversations, threaded by correspondent.
async fn tells(State(s): State<AppState>) -> Json<eqlparser_core::tells::TellReport> {
    let engine = s.engine.locked("engine");
    Json(engine.tell_report())
}

/// Your deaths, with what killed you.
async fn deaths(State(s): State<AppState>) -> Json<crate::engine::DeathReport> {
    let engine = s.engine.locked("engine");
    Json(engine.death_report())
}

/// Charm state and recent charm failures, for the resist overlay.
async fn charm(State(s): State<AppState>) -> Json<crate::engine::CharmReport> {
    let engine = s.engine.locked("engine");
    Json(engine.charm_report())
}

/// The AA catalogue read from `/alt list`.
async fn aa(State(s): State<AppState>) -> Json<eqlparser_core::aa::AaReport> {
    let engine = s.engine.locked("engine");
    Json(engine.aa_report())
}

/// Item reference data: name, stats, icon and a link back to the wiki.
///
/// Served as one map rather than folded into the Sky payload, so rewards and
/// quest items share a single lookup and the quest response stays about
/// progress.
/// One item, for a tooltip.
///
/// Answers from eqdb when it can and from the bundled data when it cannot, and
/// says which in `source` — a tooltip drawn from a file inside this binary and
/// one drawn from the live site are not the same claim, and the panel should
/// be able to say so.
///
/// Always 200. A name nothing knows about is a normal outcome, not an error:
/// the client writes plenty of item names no reference has ever heard of.
async fn item_lookup(
    State(s): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let raw = q.get("name").cloned().unwrap_or_default();
    // Graded drops are the same item; the tracker already folds them and the
    // site only knows the ungraded name.
    let name = eqlparser_core::drops::canonical_item(&raw).to_string();

    let live = s.itemdb.item(&name).await;
    // The bundled entry rides along even on a hit. The two sources do not
    // cover the same ground: eqdb knows where an item drops and what it is
    // used in, while the shipped data has an icon for items eqdb has none for
    // yet. Handing both over lets the tooltip prefer the live answer field by
    // field without this endpoint deciding which fields matter.
    let bundled: Option<serde_json::Value> = serde_json::from_str::<serde_json::Value>(WIKI_ITEMS)
        .ok()
        .and_then(|m| m.get(&name.to_lowercase()).cloned());
    let source = match (&live, &bundled) {
        (Some(_), _) => "eqdb",
        (None, Some(_)) => "bundled",
        (None, None) => "unknown",
    };
    Json(serde_json::json!({
        "name": name,
        "source": source,
        "base": s.itemdb.base(),
        "eqdb": live,
        "wiki": bundled,
    }))
    .into_response()
}

/// One spell, for a tooltip.
///
/// The name is resolved against the local spell file first so the site can be
/// asked by id: ranks share a display name, and "Puma Maw II" and "Puma Maw
/// III" are one word to a text search but different spells to a player reading
/// a tooltip. Only when the file cannot place the name does this fall back to
/// asking by name.
///
/// The local file answers on its own too — which classes can cast it and at
/// what level — so a tooltip still has something to show when the site is
/// unreachable.
async fn spell_lookup(
    State(s): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let name = q.get("name").cloned().unwrap_or_default();
    if name.trim().is_empty() {
        return Json(serde_json::json!({ "name": name, "source": "unknown" })).into_response();
    }

    // What this binary knows by itself, from spells_us.txt.
    let (id, local) = {
        let engine = s.engine.locked("engine");
        let db = engine.spell_db();
        let base = eqlparser_core::spell::base_spell_name(&name);
        let mut ids = db.candidates_for_name(&name).to_vec();
        if ids.is_empty() {
            ids = db.candidates_for_name(base).to_vec();
        }
        let first = ids.first().copied();
        let local = first.and_then(|i| db.get(i)).map(|sp| {
            serde_json::json!({
                "id": sp.id,
                "name": sp.name,
                "is_charm": sp.is_charm,
                "classes": db.class_levels_for_name(&sp.name),
            })
        });
        (first, local)
    };

    let live = s.itemdb.spell(id, &name).await;
    let source = match (&live, &local) {
        (Some(_), _) => "eqdb",
        (None, Some(_)) => "local",
        (None, None) => "unknown",
    };
    Json(serde_json::json!({
        "name": name,
        "source": source,
        "base": s.itemdb.base(),
        "eqdb": live,
        "local": local,
    }))
    .into_response()
}

/// An item icon sheet, fetched from the companion site and passed through.
///
/// Exists so the window's content-security policy names this daemon and
/// nothing else. The site is moving from localhost to a public domain, and a
/// policy that lists origins would need editing — and a rebuild — every time
/// that changed.
async fn item_icon(
    State(s): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let path = q.get("path").cloned().unwrap_or_default();
    match s.itemdb.icon(&path).await {
        Some((content_type, bytes)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                // The sheets are static and the daemon caches them anyway;
                // this stops the window re-asking as panels redraw.
                (header::CACHE_CONTROL, "public, max-age=3600".to_string()),
            ],
            bytes,
        )
            .into_response(),
        // A missing icon is not an error worth a red line in the console: the
        // card simply renders without one.
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn wiki() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        WIKI_ITEMS,
    )
        .into_response()
}

/// Plane of Sky quest progress.
/// The zone eqdb files the Plane of Sky under.
const SKY_ZONE_SLUG: &str = "the_plane_of_sky";

#[derive(Debug, Deserialize)]
struct SkyQuery {
    /// `refresh=1` re-asks the site for the zone record.
    #[serde(default)]
    refresh: Option<String>,
}

async fn sky(State(s): State<AppState>, Query(q): Query<SkyQuery>) -> Response {
    let mut report = { s.engine.locked("engine").sky_report() };
    // History spans every log ever parsed; the tracker only knows this one.
    if let Some(store) = &s.store {
        if let Ok(stored) = store.locked("history").sky_turnin_counts() {
            Engine::apply_stored_turn_ins(&mut report, &stored);
        }
    }
    let force = matches!(q.refresh.as_deref(), Some("1") | Some("true"));
    let site = sky_site_overlay(&s, &report, force).await;

    // Merged rather than nested under a new type: the report is what the page
    // has always read, and the site's account of the same quests rides
    // alongside it as a second opinion rather than replacing anything.
    let mut body = serde_json::to_value(&report).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(o) = body.as_object_mut() {
        o.insert("site".into(), site);
    }
    Json(body).into_response()
}

/// What eqdb says about the Plane of Sky, keyed to line up with our own table.
///
/// # Why an overlay and not a replacement
///
/// The quest table in `skyquest.rs` is hand-transcribed, and the site is
/// assembled from parsed logs. Neither is authoritative over the other, and
/// right now they agree exactly on every quest both know — so the interesting
/// output is not a merged record but the *difference*: which quests the site
/// has yet to hear about, which it marks as still a draft, and any field where
/// the two have drifted apart. The first of those is a list of things worth
/// contributing; the last is the alarm that would fire if either side went
/// wrong.
///
/// Replacing the table outright was considered and rejected: the site has 84
/// of our 95 quests, so taking its list wholesale would silently drop eleven.
async fn sky_site_overlay(
    s: &AppState,
    report: &eqlparser_core::skyquest::SkyReport,
    force: bool,
) -> serde_json::Value {
    let Some((zone, age)) = s.itemdb.zone(SKY_ZONE_SLUG, force).await else {
        return serde_json::json!({
            "ok": false,
            "base": s.itemdb.base(),
            "error": "the item site could not be reached, and its Sky record has never been fetched",
        });
    };

    let empty = Vec::new();
    let site_quests = zone
        .get("quests")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    let by_name: HashMap<String, &serde_json::Value> = site_quests
        .iter()
        .filter_map(|q| {
            let n = q.get("name").and_then(|v| v.as_str())?;
            Some((n.trim().to_lowercase(), q))
        })
        .collect();

    let mut quests = serde_json::Map::new();
    let mut missing: Vec<&str> = Vec::new();
    for q in &report.quests {
        let Some(sq) = by_name.get(&q.name.trim().to_lowercase()) else {
            missing.push(&q.name);
            continue;
        };
        let start = sq.get("start").and_then(|v| v.as_str()).unwrap_or("");
        let site_items: Vec<(String, Option<String>)> = sq
            .get("items")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|i| i.as_str())
                    .map(split_site_item)
                    .collect()
            })
            .unwrap_or_default();

        quests.insert(
            q.name.clone(),
            serde_json::json!({
                "verified": sq.get("verified").and_then(|v| v.as_bool()).unwrap_or(false),
                "draft": sq.get("draft").and_then(|v| v.as_bool()).unwrap_or(false),
                "start": start,
                "notes": sq.get("notes").and_then(|v| v.as_str()).unwrap_or(""),
                "updated": sq.get("updated"),
                "contributors": sq.get("contributors"),
                "dialogue": sq.get("dialogue"),
                "items": site_items.iter().map(|(n, src)| serde_json::json!({
                    "name": n, "source": src
                })).collect::<Vec<_>>(),
                "reward": sq.get("reward"),
                "giver": sq.get("giver"),
                "disagrees": sky_disagreements(q, sq, start, &site_items),
            }),
        );
    }

    // What drops here, as the site has actually observed it — but only for the
    // items some quest asks for. The zone record lists everything that has
    // ever dropped in Sky, and a panel about quest requirements has no use for
    // the other hundred-odd.
    let wanted_items: std::collections::HashSet<String> = report
        .quests
        .iter()
        .flat_map(|q| q.items.iter())
        .map(|i| crate::itemdb::match_key(&i.name))
        .collect();
    let mut drops = serde_json::Map::new();
    for d in zone.get("drops").and_then(|v| v.as_array()).unwrap_or(&empty) {
        let Some(item) = d.get("item").and_then(|v| v.as_str()) else {
            continue;
        };
        if !wanted_items.contains(&crate::itemdb::match_key(item)) {
            continue;
        }
        let sources: Vec<serde_json::Value> = d
            .get("sources")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|src| {
                        let npc = src.get("npc").and_then(|v| v.as_str())?;
                        Some(serde_json::json!({
                            "npc": npc,
                            "looted": src.get("looted"),
                            // Named bosses map to an island; trash does not,
                            // and is left unplaced rather than guessed at.
                            "island": island_of_npc(npc),
                        }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        drops.insert(
            item.to_string(),
            serde_json::json!({ "looted": d.get("looted"), "sources": sources }),
        );
    }

    serde_json::json!({
        "ok": true,
        "base": s.itemdb.base(),
        "age_seconds": age,
        "zone": zone.get("name"),
        "count": site_quests.len(),
        "matched": quests.len(),
        // Ours that the site has never heard of — the contribution list.
        "missing": missing,
        "quests": quests,
        "drops": drops,
    })
}

/// Split the site's `"Azure Ring (3-Gorga)"` into name and source note.
///
/// The site writes a requirement as one string with the note in brackets;
/// our table keeps the two apart. Only a trailing bracket counts, so an item
/// whose name contains brackets survives intact.
fn split_site_item(s: &str) -> (String, Option<String>) {
    let t = s.trim();
    let Some(body) = t.strip_suffix(')') else {
        return (t.to_string(), None);
    };
    // The matching `(` for that final `)`, not merely the last one in the
    // string: `rfind` picks the wrong bracket the moment a name contains one.
    let Some(open) = body.rfind('(') else {
        return (t.to_string(), None);
    };
    let note = &body[open + 1..];
    let name = body[..open].trim_end();
    // A note is a short aside — an island code, or the word "rune". Anything
    // with a bracket of its own is part of the name, not a note about it.
    if name.is_empty() || note.contains('(') || note.contains(')') {
        return (t.to_string(), None);
    }
    (name.to_string(), Some(note.to_string()))
}

/// The word said to a giver, out of the site's `start` sentence.
///
/// The site writes it as prose — `Hail Torgon Blademaster, then say "skill".`
/// — and the quoted word is the part our table stores as `phrase`.
fn site_phrase(start: &str) -> Option<&str> {
    let rest = start.split_once("say \"")?.1;
    rest.split_once('"').map(|(w, _)| w)
}

/// Where our table and the site's record differ, in words.
///
/// Empty today — the two agree on every quest both know — which is exactly why
/// it is worth computing. A transcription that drifts, or a site entry written
/// from a different server's requirements, shows up here rather than silently
/// contradicting whichever source a given panel happened to read.
fn sky_disagreements(
    ours: &eqlparser_core::skyquest::SkyQuestProgress,
    theirs: &serde_json::Value,
    start: &str,
    site_items: &[(String, Option<String>)],
) -> Vec<String> {
    let mut out = Vec::new();
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());

    if let Some(g) = theirs.get("giver").and_then(|v| v.as_str()) {
        if !same(g, &ours.giver) {
            out.push(format!("giver: ours \"{}\", site \"{g}\"", ours.giver));
        }
    }
    if let Some(r) = theirs.get("reward").and_then(|v| v.as_str()) {
        if !same(r, &ours.reward) {
            out.push(format!("reward: ours \"{}\", site \"{r}\"", ours.reward));
        }
    }
    if let Some(p) = site_phrase(start) {
        if !same(p, &ours.phrase) {
            out.push(format!("phrase: ours \"{}\", site \"{p}\"", ours.phrase));
        }
    }
    // Compared as sets: the site lists the rune among the items and so do we,
    // but neither promises an order.
    let norm = |v: &str| crate::itemdb::match_key(v);
    let ours_items: std::collections::BTreeSet<String> =
        ours.items.iter().map(|i| norm(&i.name)).collect();
    let their_items: std::collections::BTreeSet<String> =
        site_items.iter().map(|(n, _)| norm(n)).collect();
    if !their_items.is_empty() && ours_items != their_items {
        let only_ours: Vec<&str> = ours_items
            .difference(&their_items)
            .map(String::as_str)
            .collect();
        let only_theirs: Vec<&str> = their_items
            .difference(&ours_items)
            .map(String::as_str)
            .collect();
        out.push(format!(
            "items: only ours [{}], only site [{}]",
            only_ours.join(", "),
            only_theirs.join(", ")
        ));
    }
    out
}

/// The island a named belongs to, or `None` for trash and anything unplaced.
fn island_of_npc(npc: &str) -> Option<&'static str> {
    eqlparser_core::skyquest::SKY_ISLANDS
        .iter()
        .find(|(_, _, bosses)| bosses.iter().any(|b| b.eq_ignore_ascii_case(npc.trim())))
        .map(|(code, _, _)| *code)
}

/// Add or replace one Sky quest, for classes not in the built-in table.
async fn define_sky_quest(
    State(s): State<AppState>,
    Json(def): Json<eqlparser_core::skyquest::SkyQuestDef>,
) -> Response {
    let mut engine = s.engine.locked("engine");
    engine.define_sky_quest(def);
    (StatusCode::OK, "ok").into_response()
}

/// Which bosses each class build has cleared.
async fn builds(State(s): State<AppState>) -> Json<Vec<eqlparser_core::lockout::BuildRecord>> {
    let engine = s.engine.locked("engine");
    Json(engine.build_history())
}

#[derive(Debug, Deserialize)]
struct ClassQuery {
    /// Return every class with evidence, not just the chosen trio. The trio is
    /// a judgement call; seeing what it rejected is how you check it.
    #[serde(default)]
    all: bool,
}

async fn classes(
    State(s): State<AppState>,
    Query(q): Query<ClassQuery>,
) -> Json<serde_json::Value> {
    if q.all {
        let engine = s.engine.locked("engine");
        return Json(serde_json::json!({ "ranked": engine.class_ranking() }));
    }
    let snap = s.snapshot.reading("snapshot");
    Json(serde_json::json!({ "current": snap.classes }))
}

// ---------------------------------------------------------------------------
// Typed endpoints (eqlparser-api)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PickupsQuery {
    since: Option<i64>,
}

fn api_error(status: StatusCode, error: impl Into<String>) -> Response {
    (status, Json(eqlparser_api::ApiError { error: error.into() })).into_response()
}

/// `GET /api/pickups?since=<log ts>`: what you picked up, one row per item.
///
/// `since` is in the log's clock; without it, the last hour before the
/// engine's `now`. Graded copies are left out, as `recent_pickups` explains.
async fn api_pickups(State(s): State<AppState>, Query(q): Query<PickupsQuery>) -> Json<eqlparser_api::Pickups> {
    let engine = s.engine.locked("engine");
    let since = q.since.unwrap_or_else(|| engine.now() - 3600);
    let pickups = engine
        .recent_pickups(since)
        .into_iter()
        .map(|p| eqlparser_api::Pickup {
            item: p.item,
            at: p.at,
            count: p.count,
            source: p.source,
            held: p.held,
            zone: p.zone,
        })
        .collect();
    Json(eqlparser_api::Pickups { since, pickups })
}

/// `GET /api/bags`: what the followed character's inventory dumps hold.
async fn api_bags(State(s): State<AppState>) -> Response {
    let Some(ids) = &s.ids else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no item-id record: this daemon has no config directory to keep one in",
        );
    };
    let (me, read, sources) = my_dumps(&s, ids);
    let (character, server) = me.unwrap_or_default();
    Json(eqlparser_api::Bags { character, server, sources, items: bag_items(&read) }).into_response()
}

/// `GET /api/bundle`: the export document as it stands, for the collector to
/// check and send. The daemon builds it because the history it is built from
/// is the daemon's; sending it is the collector's business.
async fn api_bundle(State(s): State<AppState>) -> Response {
    match build_current(&s) {
        Ok(b) => Json(b).into_response(),
        Err(e) => api_error(StatusCode::SERVICE_UNAVAILABLE, e),
    }
}

async fn events_upgrade(ws: WebSocketUpgrade, State(s): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| events_loop(socket, s))
}

/// Push each [`eqlparser_api::Event`] as a JSON text frame.
///
/// Nothing is sent on connect: events are news, and the state they change is
/// a request away. A client that falls behind by more than the channel holds
/// is told how many it missed in a log line, not on the wire, and should
/// re-read whatever it keeps.
async fn events_loop(mut socket: WebSocket, state: AppState) {
    let _client = state.life.connect();
    let mut rx = state.events.subscribe();
    loop {
        tokio::select! {
            got = rx.recv() => match got {
                Ok(e) => {
                    let body = serde_json::to_string(&e).unwrap_or_else(|_| "{}".into());
                    if socket.send(Message::Text(body.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[eqlparserd] events: a client fell {n} behind");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            // Read only to notice the client going: otherwise a quiet stream
            // finds out at the next event, which may be an hour away, and the
            // daemon counts a client that left as still here.
            msg = socket.recv() => match msg {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

async fn ws_upgrade(ws: WebSocketUpgrade, State(s): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, s))
}

/// Push a fresh snapshot on every update.
///
/// Clients get the current state immediately on connect so the overlay renders
/// without waiting for the next combat line.
async fn ws_loop(mut socket: WebSocket, state: AppState) {
    let _client = state.life.connect();
    let mut rx = state.updates.subscribe();

    let initial = { state.snapshot.reading("snapshot").clone() };
    if send_snapshot(&mut socket, &initial).await.is_err() {
        return;
    }

    loop {
        // A lagging client misses intermediate updates, which is harmless: the
        // next snapshot is complete in itself. The socket is read only to
        // notice a close, which otherwise went unseen until the next send.
        tokio::select! {
            got = rx.recv() => match got {
                Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = socket.recv() => match msg {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => continue,
            },
        }
        let snap = { state.snapshot.reading("snapshot").clone() };
        if send_snapshot(&mut socket, &snap).await.is_err() {
            break;
        }
    }
}

async fn send_snapshot(socket: &mut WebSocket, snap: &Snapshot) -> Result<(), axum::Error> {
    let body = serde_json::to_string(snap).unwrap_or_else(|_| "{}".to_string());
    socket.send(Message::Text(body.into())).await
}

/// Unused today, but kept so query parsing stays honest if handlers grow.
#[allow(dead_code)]
type QueryMap = HashMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;
    use eqlparser_core::spell::SpellDb;

    use eqlparser_core::itemids::{Entry, Registry};

    /// One `/outputfile inventory` dump, as the client writes it.
    fn dump(rows: &[&str]) -> Vec<(String, String)> {
        let mut t = String::from("Location\tName\tID\tCount\tSlots\n");
        for (i, n) in rows.iter().enumerate() {
            t.push_str(&format!("General1\t{n}\t{}\t1\t0\n", 400 + i));
        }
        vec![("Tharn_freeport-Inventory.txt".to_string(), t)]
    }

    /// Nothing photographed and nothing sent, which is what most of these
    /// tests are about and none of them are testing.
    fn unmarked(_: &str) -> (bool, bool) {
        (false, false)
    }

    fn wants(rows: &[(&str, &[&str])]) -> serde_json::Value {
        serde_json::json!({
            "count": rows.len(),
            "items": rows.iter().map(|(n, needs)| serde_json::json!({
                "name": n, "slug": n.to_lowercase().replace(' ', "_"),
                "needs": needs, "zones": [], "sources": [], "sold_by": []
            })).collect::<Vec<_>>(),
        })
    }

    /// The rule the whole endpoint turns on: a `+5` in a bag is not the base
    /// reading the site is asking for, and shooting it fills nothing.
    #[test]
    fn a_graded_copy_cannot_fill_a_base_reading() {
        let b = bags(&dump(&["Brell's Girdle +5"]));
        let rows = held_and_wanted(&wants(&[("Brell's Girdle", &["base"])]), &b, &unmarked);
        assert_eq!(rows.len(), 1, "it is still reported, so nobody hunts for it twice");
        assert_eq!(rows[0]["blocked"], serde_json::json!(true));
        assert_eq!(rows[0]["grades_held"], serde_json::json!([5]));
    }

    /// The marks do not come from the site.
    ///
    /// This is where the first cut of the endpoint was wrong, and the shape of
    /// the mistake is worth keeping: it copied `have` and `sent` off the wanted
    /// payload, which reads perfectly and is never true --- `/api/wanted`
    /// attaches them per request from the engine, and the cached payload this
    /// crosses has never held them. Every row came back unmarked, so the
    /// overlay offered all 123 items in the bags including the ones already
    /// photographed. Asserting against a payload that *does* carry the fields
    /// is what hid it, so this asserts the opposite: the payload states a mark
    /// and it is ignored, because the caller is the only thing that knows.
    #[test]
    fn the_marks_come_from_the_caller_not_the_payload() {
        let b = bags(&dump(&["Onyx Earring", "Bone Chips"]));
        let mut w = wants(&[("Onyx Earring", &["icon"]), ("Bone Chips", &["icon"])]);
        w["items"][0]["have"] = serde_json::json!(false);
        w["items"][0]["sent"] = serde_json::json!(false);
        let marks = |n: &str| (n == "Onyx Earring", false);
        let rows = held_and_wanted(&w, &b, &marks);
        let by = |n: &str| {
            rows.iter().find(|r| r["name"] == serde_json::json!(n)).unwrap().clone()
        };
        assert_eq!(by("Onyx Earring")["have"], serde_json::json!(true));
        assert_eq!(by("Bone Chips")["have"], serde_json::json!(false));
        // Stated on every row, never left absent: the overlay filters on it,
        // and a missing field there reads as "not photographed".
        assert_eq!(by("Bone Chips")["sent"], serde_json::json!(false));
    }

    /// And carrying the ungraded one is what unblocks it.
    #[test]
    fn the_ungraded_copy_is_the_one_that_fills_it() {
        let b = bags(&dump(&["Brell's Girdle", "Brell's Girdle +5"]));
        let rows = held_and_wanted(&wants(&[("Brell's Girdle", &["base"])]), &b, &unmarked);
        assert_eq!(rows[0]["blocked"], serde_json::json!(false));
    }

    /// One character's bags, not every character's.
    ///
    /// hammackj was playing Tharn and the list offered items sitting in Mirela's
    /// bags in a dump 44 hours old. That is not a prompt to take a picture ---
    /// it is a trip to the bank and a character select --- and nothing on the
    /// row said whose it was.
    #[test]
    fn another_characters_dump_is_not_your_bags() {
        let tharn = ("Tharn".to_string(), "freeport".to_string());
        assert!(mine("Tharn_freeport-Inventory.txt", Some(&tharn)));
        assert!(!mine("Mirela_freeport-Inventory.txt", Some(&tharn)));
        // The same name on another server is another character.
        assert!(!mine("Tharn_halas-Inventory.txt", Some(&tharn)));
        // Neither half known is not a refusal: a renamed dump still holds ids,
        // and a daemon that knows nobody must not answer with nothing.
        assert!(mine("inventory.txt", Some(&tharn)));
        assert!(mine("Mirela_freeport-Inventory.txt", None));
    }

    /// The rule is the reader's, and this is the test that says so.
    ///
    /// It replaces one asserting the opposite: that a graded copy was still
    /// worth photographing while the item wanted a tooltip or an icon, because
    /// the reading would fill those. It would, if it happened. It does not ---
    /// `held_reason` refuses a graded window before anything asks what the site
    /// wants --- so the panel prompted for items whose pictures came back
    /// `held: graded +3`, which is a trip spent and then blamed on the picture.
    ///
    /// Both halves are asserted here rather than only the endpoint's, so the
    /// day the reader learns to accept a graded window this fails and says
    /// where the other half is.
    #[test]
    fn a_graded_copy_is_blocked_whatever_the_item_needs() {
        for needs in [&["base"][..], &["tooltip"][..], &["tooltip", "icon"][..]] {
            let b = bags(&dump(&["Refugee Shroud +3"]));
            let rows = held_and_wanted(
                &wants(&[("Refugee Shroud", needs)]),
                &b,
                &unmarked,
            );
            assert_eq!(
                rows[0]["blocked"],
                serde_json::json!(true),
                "needs {needs:?}: the reader will not send a graded window for any of them"
            );
        }
        // The other half of the same fact, in the crate that owns it.
        let graded: eqlparser_parse::lines::Record = [
            ("item".to_string(), serde_json::Value::from("Refugee Shroud +3")),
            ("tier".to_string(), serde_json::Value::from(3)),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            eqlparser_parse::held_reason(&graded),
            Some("graded +3".into()),
            "if this changes, the blocked rule above is now too strict"
        );
    }

    /// And an exaltation needs no exemption. It carries no `+N`, so it is
    /// already grade 0 under its own name -- which is the same reason the
    /// reader lets its window through.
    #[test]
    fn an_exaltation_is_not_a_graded_copy() {
        let b = bags(&dump(&["Refugee Shroud (Exaltation)"]));
        let rows = held_and_wanted(
            &wants(&[("Refugee Shroud (Exaltation)", &["tooltip"])]),
            &b,
            &unmarked,
        );
        assert_eq!(rows[0]["blocked"], serde_json::json!(false));
    }

    /// An item nobody is carrying is not on this list, however much the site
    /// wants it. That is the entire narrowing.
    #[test]
    fn what_is_not_in_a_bag_is_not_here() {
        let b = bags(&dump(&["Bottle of Milk"]));
        let rows = held_and_wanted(
            &wants(&[("Bottle of Milk", &["icon"]), ("Onyx Earring", &["tooltip"])]),
            &b,
            &unmarked,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], serde_json::json!("Bottle of Milk"));
    }

    /// The empty bag slots the client writes one row each for are not items.
    #[test]
    fn empty_slots_are_not_carried_items() {
        let b = bags(&dump(&["Empty", "Bottle of Milk"]));
        assert_eq!(b.len(), 1);
    }

    /// The KeyRing block carries its own header row mid-file. `ID` does not
    /// parse as a number, which is what keeps a column heading from becoming an
    /// item called `Name`.
    #[test]
    fn an_embedded_sub_header_is_not_an_item() {
        let mut d = dump(&["Bottle of Milk"]);
        d[0].1.push_str("KeyRing\tName\tID\t\n");
        let b = bags(&d);
        assert_eq!(b.len(), 1);
        assert!(!b.contains_key("name"));
    }

    /// What the accumulator remembers is not what is in a bag. It never
    /// deletes -- an item sold last month is still `grades_held` there -- so a
    /// list built from it sends somebody looking for what they no longer have.
    #[test]
    fn the_record_of_what_was_held_is_not_the_bag() {
        let mut reg = Registry::default();
        reg.items.push(Entry {
            item_id: 700,
            item: "Sold Last Month".to_string(),
            grades_held: vec![0],
            grades_seen: Vec::new(),
            also_known_as: Vec::new(),
            icon_id: None,
            sources: Vec::new(),
        });
        let _ = &reg;
        let b = bags(&dump(&["Bottle of Milk"]));
        let rows = held_and_wanted(&wants(&[("Sold Last Month", &["tooltip"])]), &b, &unmarked);
        assert!(rows.is_empty(), "the accumulator remembers it; the bag does not hold it");
    }

    /// An Exaltation augment shares its item's id and is a different item, so
    /// the two must not pool grades. Carrying the augment ungraded says nothing
    /// about whether the item itself is in the bag at its base rung -- and a
    /// reading of the augment is a reading of the augment.
    #[test]
    fn an_augment_sharing_an_id_does_not_unblock_its_item() {
        let b = bags(&dump(&["Brell's Girdle +5", "Brell's Girdle (Exaltation)"]));
        let rows = held_and_wanted(&wants(&[("Brell's Girdle", &["base"])]), &b, &unmarked);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["blocked"], serde_json::json!(true));
        assert_eq!(rows[0]["grades_held"], serde_json::json!([5]));
    }

    /// The augment is its own row when the site asks for it by its own name.
    #[test]
    fn the_augment_is_matched_under_its_own_name() {
        let b = bags(&dump(&["Brell's Girdle (Exaltation)"]));
        let rows = held_and_wanted(
            &wants(&[("Brell's Girdle (Exaltation)", &["icon"])]),
            &b,
            &unmarked,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["blocked"], serde_json::json!(false));
    }

    /// Blocked rows sort last, and the rest by what they need. The site's own
    /// order is how often an item drops, which is meaningless once the answer
    /// is "it is in your bags".
    #[test]
    fn the_order_is_what_it_needs_not_what_it_is_worth() {
        let held = bags(&dump(&["Alpha +5", "Beta", "Gamma"]));
        let rows = held_and_wanted(
            &wants(&[
                ("Alpha", &["base"]),
                ("Gamma", &["tooltip"]),
                ("Beta", &["icon"]),
            ]),
            &held,
            &unmarked,
        );
        let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Beta", "Gamma", "Alpha"]);
    }

    fn test_hello() -> eqlparser_api::Hello {
        eqlparser_api::Hello {
            identity: eqlparser_api::IDENTITY.into(),
            version: "test".into(),
            api_version: eqlparser_api::API_VERSION,
            pid: 1,
            port: 8777,
            started_at: 0,
            game: "everquest-legends".into(),
            log: "eqlog_Test_freeport.txt".into(),
            character: "Test".into(),
            server: "freeport".into(),
            game_dir: None,
            screenshots_dir: None,
            clients: 0,
            idle_exit_secs: Some(60),
            clock_offset: None,
            zone: None,
        }
    }

    /// A connected client holds the daemon up; its leaving starts the clock.
    #[test]
    fn idle_counts_from_the_last_client_leaving() {
        let life = Arc::new(Life::new(test_hello()));
        let now = unix_now();
        assert!(life.idle_for(now).is_some(), "nobody connected yet");
        let a = life.connect();
        let b = life.connect();
        assert_eq!(life.clients(), 2);
        assert_eq!(life.hello().clients, 2);
        assert_eq!(life.idle_for(now + 1000), None, "a client is connected");
        drop(a);
        assert_eq!(life.idle_for(now + 1000), None, "one is still connected");
        drop(b);
        assert_eq!(life.clients(), 0);
        // The clock starts at the close, not at the open.
        let idle = life.idle_for(unix_now() + 30).unwrap();
        assert!((29..=31).contains(&idle), "idle {idle}");
    }

    /// `/api/hello` names the daemon and its API version, and every request
    /// counts as activity.
    #[tokio::test]
    async fn hello_says_who_this_is() {
        let st = state_with(&[]);
        let life = Arc::clone(&st.life);
        life.last_activity.store(0, std::sync::atomic::Ordering::SeqCst);
        let app = router(st);
        let resp = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::get("/api/hello").body(axum::body::Body::empty()).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let h: eqlparser_api::Hello = serde_json::from_slice(&body).unwrap();
        assert_eq!(h.identity, "eqlparserd");
        assert_eq!(h.api_version, eqlparser_api::API_VERSION);
        assert!(life.last_activity.load(std::sync::atomic::Ordering::SeqCst) > 0);
    }

    fn state_with(lines: &[&str]) -> AppState {
        let mut engine = Engine::new("Tharn".to_string(), SpellDb::empty());
        for l in lines {
            engine.push_line(l);
        }
        let snap = engine.snapshot(50);
        let (tx, _) = broadcast::channel(4);
        let (events, _) = broadcast::channel(4);
        AppState {
            life: Arc::new(Life::new(test_hello())),
            events,
            store: None,
            log_path: std::path::PathBuf::from("eqlog_Test_freeport.txt"),
            logs_dir: None,
            engine: Arc::new(Mutex::new(engine)),
            snapshot: Arc::new(RwLock::new(snap)),
            updates: tx,
            // An origin nothing is listening on: the tests must never depend
            // on the companion site being up, and this proves the fallback is
            // what they exercise.
            itemdb: Arc::new(crate::itemdb::ItemDb::new(vec![
                "http://127.0.0.1:1".to_string()
            ])),
            ids: None,
            shots: crate::shots::Shots::new(crate::shots::Setup {
                shots: None,
                game_dir: None,
                itemdb: Arc::new(crate::itemdb::ItemDb::new(vec![])),
                // The tests must not read anybody's screenshots, send
                // anything anywhere, or notice a token that happens to be in
                // the environment they run in.
                off: true,
                // Belt and braces: `off` already means no reader and so no
                // filing, but a test suite that could move somebody's
                // screenshots if a flag changed meaning is not one to leave
                // depending on that.
                no_archive: true,
                archive_dry_run: true,
                backlog: false,
            }),
        }
    }

    /// "Nobody has recorded this here" is a claim, and it is only worth making
    /// when both halves are actually known.
    #[test]
    fn a_new_drop_location_needs_both_halves_to_be_known() {
        let known = ["Befallen".to_string(), "Paineel".to_string()];
        assert!(new_zone(Some("Solusek's Eye"), &known));
        assert!(!new_zone(Some("Befallen"), &known));
        // The site writes zones one way and the client another often enough
        // that case cannot be the difference between "known" and "new".
        assert!(!new_zone(Some("befallen"), &known));
        // The log could not place the pickup.
        assert!(!new_zone(None, &known));
        // The site knows the item but has placed none of its sources, so
        // everywhere would read as new.
        assert!(!new_zone(Some("Befallen"), &[]));
    }

    /// Every named import across the renderer resolves.
    ///
    /// The same failure as the id test and a different cause: a module that
    /// imports a name its target does not export does not load, and every
    /// listener it wires goes with it. It is easy to reach for — adding one
    /// function to an import line is how a feature usually starts — and the
    /// only report is a console nobody has open.
    #[test]
    fn every_named_import_in_the_renderer_resolves() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root");
        let dir = root.join("gui/renderer");
        // The daemon is usable without the desktop app beside it.
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        let files: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "js"))
            .collect();
        if files.is_empty() {
            return;
        }

        /// What a module exports, by the two forms this renderer uses:
        /// `export const/let/function/class NAME`, and `export { a, b }`.
        fn exports(src: &str) -> std::collections::BTreeSet<String> {
            let mut out = std::collections::BTreeSet::new();
            let word = |s: &str| -> String {
                s.chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                    .collect()
            };
            let mut rest = src;
            while let Some(i) = rest.find("export ") {
                rest = &rest[i + 7..];
                let mut t = rest.trim_start();
                for kw in ["async ", "function ", "const ", "let ", "var ", "class "] {
                    t = t.strip_prefix(kw).unwrap_or(t).trim_start();
                }
                if let Some(list) = t.strip_prefix('{') {
                    if let Some(end) = list.find('}') {
                        for n in list[..end].split(',') {
                            // `export { a as b }` exports `b`.
                            let n = n.rsplit(" as ").next().unwrap_or("").trim();
                            if !n.is_empty() {
                                out.insert(n.to_string());
                            }
                        }
                    }
                    continue;
                }
                let name = word(t);
                if !name.is_empty() {
                    out.insert(name);
                }
            }
            out
        }

        let mut broken: Vec<String> = Vec::new();
        for path in &files {
            let Ok(src) = std::fs::read_to_string(path) else { continue };
            let me = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let mut rest = src.as_str();
            while let Some(i) = rest.find("import {") {
                rest = &rest[i + 8..];
                let Some(close) = rest.find('}') else { break };
                let names = &rest[..close];
                let after = &rest[close..];
                let Some(q) = after.find("\"./") else { continue };
                let tail = &after[q + 3..];
                let Some(e) = tail.find('"') else { continue };
                let target = &tail[..e];
                let Ok(tsrc) = std::fs::read_to_string(dir.join(target)) else {
                    broken.push(format!("{me} imports from {target}, which is not there"));
                    continue;
                };
                let has = exports(&tsrc);
                for n in names.split(',') {
                    let n = n.split(" as ").next().unwrap_or("").trim();
                    if !n.is_empty() && !has.contains(n) {
                        broken.push(format!("{me} imports {n:?} from {target}, which does not export it"));
                    }
                }
            }
        }
        assert!(
            broken.is_empty(),
            "the renderer will not load: {broken:#?}"
        );
    }

    /// Every `$("id")` the renderer reaches for is an id something makes.
    ///
    /// The renderer wires its listeners at module scope, so `$("cfg-save")`
    /// against an id that is not there is not a dead button — `$` returns
    /// null, `.addEventListener` throws, the module fails to load, and every
    /// feature in that file goes with it. A typo in a settings field takes
    /// out the settings tab.
    ///
    /// An id counts as made if `index.html` has it or if some renderer module
    /// writes it into an `innerHTML` template, which is where the encounters
    /// sub-tabs and the tasks toggle come from.
    #[test]
    fn every_id_the_renderer_reaches_for_is_one_something_makes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root");
        let dir = root.join("gui/renderer");
        // The daemon is usable without the desktop app beside it.
        let Ok(html) = std::fs::read_to_string(dir.join("index.html")) else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        let js: String = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "js"))
            .filter_map(|p| std::fs::read_to_string(&p).map(|s| format!("//@{}\n{s}", p.display())).ok())
            .collect::<Vec<_>>()
            .join("\n");
        if js.is_empty() {
            return;
        }

        // Every `id="..."` anywhere -- the page's own, and the ones the
        // renderer builds into its templates.
        let mut made = std::collections::BTreeSet::new();
        for src in [&html, &js] {
            let mut rest = src.as_str();
            while let Some(i) = rest.find("id=\"") {
                rest = &rest[i + 4..];
                if let Some(j) = rest.find('"') {
                    made.insert(rest[..j].to_string());
                }
            }
        }

        let mut missing: Vec<String> = Vec::new();
        let mut rest = js.as_str();
        while let Some(i) = rest.find("$(\"") {
            rest = &rest[i + 3..];
            let Some(j) = rest.find('"') else { break };
            // Only a literal: `$("view-" + name)` names no id here, and the
            // half in the source is not one to look for.
            if rest[j + 1..].starts_with(')') {
                let id = &rest[..j];
                if !made.contains(id) {
                    missing.push(id.to_string());
                }
            }
        }
        assert!(
            missing.is_empty(),
            "the renderer reaches for {missing:?}, which nothing makes -- \
             the module holding it will throw at load and take its whole tab \
             with it"
        );
    }

    /// The overlay's panels and the Settings tickboxes are two lists in two
    /// files, and they drift. A comment above the second one already said
    /// "adding one to the overlay means adding it here", and the `motes` panel
    /// was still added to one and not the other — leaving it reachable only by
    /// typing it into the options string by hand.
    ///
    /// So the lists are checked against each other rather than trusted.
    #[test]
    fn every_overlay_panel_is_offered_in_settings() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root");
        // Every renderer module, not one named file: the list used to live in
        // app.js and moved to settings.js when the renderer was split, which
        // broke this test without anything being wrong with the app. What is
        // being asked is "can the user switch this panel on anywhere", so ask
        // it of the whole renderer.
        let Ok(dir) = std::fs::read_dir(root.join("gui/renderer")) else {
            // The daemon is usable without the desktop app beside it; a
            // missing renderer is not this test's business to fail over.
            return;
        };
        let app_js: String = dir
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "js"))
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .collect::<Vec<_>>()
            .join("\n");
        if app_js.is_empty() {
            return;
        }

        let panels = |src: &str, pat: &str| -> std::collections::BTreeSet<String> {
            let mut out = std::collections::BTreeSet::new();
            let mut rest = src;
            while let Some(i) = rest.find(pat) {
                rest = &rest[i + pat.len()..];
                if let Some(j) = rest.find('"') {
                    let name = &rest[..j];
                    if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase()) {
                        out.insert(name.to_string());
                    }
                }
            }
            out
        };

        let in_overlay = panels(OVERLAY_JS, "panels.includes(\"");
        let in_settings = panels(&app_js, "[\"");
        assert!(!in_overlay.is_empty(), "found no panels in the overlay");

        let missing: Vec<&String> = in_overlay.difference(&in_settings).collect();
        assert!(
            missing.is_empty(),
            "the overlay has panels Settings cannot switch on: {missing:?} \
             — add them to OVERLAY_PANELS in gui/renderer/settings.js"
        );
    }

    const SAMPLE: &[&str] = &[
        "[Tue Jul 28 14:00:00 2026] You have entered West Commonlands.",
        "[Tue Jul 28 14:00:01 2026] You slash a lesser mummy for 100 points of damage.",
        "[Tue Jul 28 14:00:02 2026] You have slain a lesser mummy!",
        "[Tue Jul 28 14:00:03 2026] --You have looted a Mummy Wrap from a lesser mummy's corpse.--",
        "[Tue Jul 28 15:00:00 2026] You slash a giant rat for 40 points of damage.",
        "[Tue Jul 28 15:00:01 2026] You have slain a giant rat!",
        "[Tue Jul 28 16:00:00 2026] You slash a bat for 5 points of damage.",
    ];

    #[tokio::test]
    async fn snapshot_endpoint_returns_state() {
        let s = state_with(SAMPLE);
        let Json(snap) = snapshot(State(s)).await;
        assert_eq!(snap.player, "Tharn");
        // Two closed fights plus the one still open; totals count the fight in
        // progress so headline figures move during combat.
        assert_eq!(snap.totals.encounters, 3);
        assert_eq!(snap.zone.as_deref(), Some("West Commonlands"));
    }

    #[tokio::test]
    async fn encounters_endpoint_is_newest_first() {
        let s = state_with(SAMPLE);
        let Json(v) = encounters(
            State(s),
            Query(ListQuery {
                limit: None,
                offset: None,
                search: None,
                zone: None,
                since: None,
                until: None,
                min_duration: None,
                build: None,
                difficulty: None,
            }),
        )
        .await;
        assert_eq!(v["total"], 2);
        let list = v["encounters"].as_array().unwrap();
        assert_eq!(list[0]["title"], "a giant rat", "newest fight should lead");
    }

    #[tokio::test]
    async fn encounters_endpoint_filters_by_search() {
        let s = state_with(SAMPLE);
        let Json(v) = encounters(
            State(s),
            Query(ListQuery {
                limit: None,
                offset: None,
                search: Some("mummy".into()),
                zone: None,
                since: None,
                until: None,
                min_duration: None,
                build: None,
                difficulty: None,
            }),
        )
        .await;
        assert_eq!(v["total"], 1);
        assert_eq!(v["encounters"][0]["title"], "a lesser mummy");
    }

    /// A creature folded into "+N more" is still findable by name.
    ///
    /// A fight is titled after whichever creature took the most damage, so a
    /// pull with adds hides the rest behind a count. Searching titles alone
    /// found 32% of the fights that actually contained the named creature on
    /// a real log, where two thirds of fights have more than one opponent —
    /// which reads as the parser having missed them.
    #[tokio::test]
    async fn search_finds_a_mob_the_title_folded_away() {
        // One fight, two opponents: the rat took more damage, so it is the
        // title and the bat is the "+1 more".
        let s = state_with(&[
            "[Tue Jul 28 14:00:00 2026] You have entered West Commonlands.",
            "[Tue Jul 28 14:00:01 2026] You slash a giant rat for 100 points of damage.",
            "[Tue Jul 28 14:00:02 2026] You slash a dusty bat for 10 points of damage.",
            "[Tue Jul 28 14:00:03 2026] You have slain a dusty bat!",
            "[Tue Jul 28 14:00:04 2026] You have slain a giant rat!",
            // Far enough later to close the fight above: the list serves
            // closed encounters, and nothing in a log says a pull is over.
            "[Tue Jul 28 15:00:00 2026] You slash a lesser mummy for 5 points of damage.",
        ]);
        let query = |search: &str| ListQuery {
            limit: None,
            offset: None,
            search: Some(search.into()),
            zone: None,
            since: None,
            until: None,
            min_duration: None,
            build: None,
            difficulty: None,
        };

        let Json(v) = encounters(State(s.clone()), Query(query("giant rat"))).await;
        assert_eq!(v["total"], 1, "the titled mob was always findable");
        assert_eq!(v["encounters"][0]["title"], "a giant rat (+1 more)");

        let Json(v) = encounters(State(s), Query(query("dusty bat"))).await;
        assert_eq!(v["total"], 1, "the folded-away mob must be findable too");
        // And the row can say which creature matched, rather than showing a
        // title with the searched name nowhere in it.
        let foes = v["encounters"][0]["opponents"].as_array().unwrap();
        assert!(
            foes.iter().any(|o| o == "a dusty bat"),
            "opponents should name every creature: {foes:?}"
        );
    }

    /// A pull arrives as one row; asking for mobs breaks it into its creatures.
    ///
    /// The unit a fight is measured in and the unit it is looked for in are
    /// not the same. A meter wants the pull — your damage over it is one
    /// number — and "how did I do against a skeletal monk" wants the creature.
    #[tokio::test]
    async fn the_mobs_view_splits_a_pull_into_its_creatures() {
        let s = state_with(&[
            "[Tue Jul 28 14:00:00 2026] You have entered West Commonlands.",
            "[Tue Jul 28 14:00:01 2026] You slash a giant rat for 100 points of damage.",
            "[Tue Jul 28 14:00:02 2026] You slash a dusty bat for 10 points of damage.",
            "[Tue Jul 28 14:00:03 2026] You have slain a dusty bat!",
            "[Tue Jul 28 14:00:04 2026] You have slain a giant rat!",
            "[Tue Jul 28 15:00:00 2026] You slash a lesser mummy for 5 points of damage.",
        ]);
        let query = |search: Option<&str>| ListQuery {
            limit: None,
            offset: None,
            search: search.map(Into::into),
            zone: None,
            since: None,
            until: None,
            min_duration: None,
            build: None,
            difficulty: None,
        };

        // One fight, two creatures.
        let Json(v) = encounters(State(s.clone()), Query(query(None))).await;
        assert_eq!(v["total"], 1, "the pull is still one fight");
        let Json(v) = mobs(State(s.clone()), Query(query(None))).await;
        assert_eq!(v["total"], 2, "and two mobs");

        // Asking for one creature returns that creature, not its whole pull.
        let Json(v) = mobs(State(s), Query(query(Some("dusty bat")))).await;
        assert_eq!(v["total"], 1);
        let row = &v["mobs"][0];
        assert_eq!(row["name"], "a dusty bat");
        assert_eq!(row["damage_done_to_it"], 10, "its own damage, not the pull's");
        assert_eq!(row["kills"], 1);
        assert_eq!(row["shared_with"], 1, "and it says it shared the fight");
    }

    #[tokio::test]
    async fn encounters_endpoint_paginates() {
        let s = state_with(SAMPLE);
        let Json(v) = encounters(
            State(s),
            Query(ListQuery {
                limit: Some(1),
                offset: Some(1),
                search: None,
                zone: None,
                since: None,
                until: None,
                min_duration: None,
                build: None,
                difficulty: None,
            }),
        )
        .await;
        assert_eq!(v["total"], 2, "total counts all matches, not the page");
        assert_eq!(v["encounters"].as_array().unwrap().len(), 1);
        assert_eq!(v["encounters"][0]["title"], "a lesser mummy");
    }

    #[tokio::test]
    async fn encounter_detail_returns_404_for_unknown_id() {
        let s = state_with(SAMPLE);
        let resp = encounter_detail(State(s), AxPath(9999)).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn encounter_detail_returns_breakdown() {
        let s = state_with(SAMPLE);
        let resp = encounter_detail(State(s.clone()), AxPath(1)).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn loot_endpoint_aggregates() {
        let s = state_with(SAMPLE);
        let Json(v) = loot(State(s), Query(LootQuery { session: None })).await;
        assert_eq!(v["total"], 1);
        assert_eq!(v["items"][0]["item"], "Mummy Wrap");
    }

    /// The site writes a requirement as one string with the source in
    /// brackets; our table keeps the two apart.
    #[test]
    fn a_site_requirement_splits_into_item_and_note() {
        assert_eq!(
            split_site_item("Azure Ring (3-Gorga)"),
            ("Azure Ring".into(), Some("3-Gorga".into()))
        );
        assert_eq!(
            split_site_item("Wind Rune Neza (rune)"),
            ("Wind Rune Neza".into(), Some("rune".into()))
        );
        // No bracket at all, and a bracket that is part of the name.
        assert_eq!(split_site_item("Efreeti Belt"), ("Efreeti Belt".into(), None));
        assert_eq!(
            split_site_item("Some Item (with (nested) brackets)"),
            ("Some Item (with (nested) brackets)".into(), None)
        );
    }

    /// The word said to a giver is quoted inside the site's prose.
    #[test]
    fn the_phrase_comes_out_of_the_sites_start_sentence() {
        assert_eq!(
            site_phrase("Hail Torgon Blademaster, then say \"skill\"."),
            Some("skill")
        );
        assert_eq!(
            site_phrase("Hail Cilin Spellsinger, then say \"ready to be tested\"."),
            Some("ready to be tested")
        );
        // Nothing quoted, and nothing at all: both mean "the site does not say".
        assert_eq!(site_phrase("Hail the giver."), None);
        assert_eq!(site_phrase(""), None);
    }

    /// Only nameds place a drop on an island. Trash is left unplaced rather
    /// than guessed at from a similar name.
    #[test]
    fn a_named_places_a_drop_on_its_island() {
        assert_eq!(island_of_npc("Gorgalosk"), Some("3-Gorga"));
        assert_eq!(island_of_npc("gorgalosk"), Some("3-Gorga"));
        assert_eq!(island_of_npc("The Spiroc Lord"), Some("5-SL"));
        assert_eq!(island_of_npc("Eye of Veeshan"), Some("8-EoV"));
        // `a gorgalask` is trash that merely shares a root with the named.
        assert_eq!(island_of_npc("a gorgalask"), None);
        assert_eq!(island_of_npc("an azarack"), None);
        assert_eq!(island_of_npc(""), None);
    }

    /// The overlay's whole job is to notice drift, so the comparison has to
    /// fire on a real difference and stay quiet on a cosmetic one.
    #[test]
    fn the_site_and_the_table_are_compared_field_by_field() {
        let report = Engine::new("Tharn".to_string(), SpellDb::empty()).sky_report();
        let ours = report
            .quests
            .iter()
            .find(|q| q.name == "Warrior Test of Skill")
            .expect("the seeded table has this quest");

        // The site as it actually answers today: same giver, phrase, reward
        // and items, written in its own shapes.
        let start = "Hail Torgon Blademaster, then say \"skill\".";
        let agrees = serde_json::json!({
            "giver": "Torgon Blademaster",
            "reward": "Azure Ruby Ring",
        });
        let items = vec![
            ("Wind Rune Neza".to_string(), Some("rune".to_string())),
            ("Azure Ring".to_string(), Some("3-Gorga".to_string())),
        ];
        assert!(
            sky_disagreements(ours, &agrees, start, &items).is_empty(),
            "the two agree today; a false alarm here would train the eye to ignore it"
        );

        // Case and spacing are not differences.
        let loose = serde_json::json!({
            "giver": "  torgon blademaster ",
            "reward": "AZURE RUBY RING",
        });
        assert!(sky_disagreements(ours, &loose, start, &items).is_empty());

        // A real drift on every field it checks.
        let drifted = serde_json::json!({
            "giver": "Someone Else",
            "reward": "A Different Ring",
        });
        let wrong_items = vec![("Azure Ring".to_string(), None)];
        let out = sky_disagreements(
            ours,
            &drifted,
            "Hail Torgon Blademaster, then say \"strength\".",
            &wrong_items,
        );
        assert_eq!(out.len(), 4, "giver, reward, phrase and items: {out:?}");
        assert!(out.iter().any(|l| l.starts_with("giver:")));
        assert!(out.iter().any(|l| l.starts_with("reward:")));
        assert!(out.iter().any(|l| l.starts_with("phrase:")));
        assert!(out.iter().any(|l| l.contains("wind rune neza")));

        // An empty item list is the site not saying, not the site disagreeing.
        assert!(
            !sky_disagreements(ours, &agrees, start, &[])
                .iter()
                .any(|l| l.starts_with("items:"))
        );
    }
}
