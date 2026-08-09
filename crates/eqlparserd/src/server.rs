//! HTTP + WebSocket API, and static hosting for the overlay.
//!
//! The overlay is served from this process rather than from a file:// URL so
//! that OBS can point a Browser Source at a plain http://127.0.0.1 address, and
//! so the Electron windows and OBS all consume exactly the same page.

use crate::engine::{Engine, Snapshot};
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
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/overlay", get(overlay_page))
        .route("/overlay.css", get(overlay_css))
        .route("/overlay.js", get(overlay_js))
        .route("/api/snapshot", get(snapshot))
        .route("/api/encounters", get(encounters))
        .route("/api/encounters/{id}", get(encounter_detail))
        .route("/api/loot", get(loot))
        .route("/api/loot/item", get(loot_item))
        .route("/api/classes", get(classes))
        .route("/api/experience", get(experience))
        .route("/api/lockouts", get(lockouts))
        .route("/api/tradeskills", get(tradeskills))
        .route("/api/items", get(items))
        .route("/api/drops", get(drops))
        .route("/api/bestiary", get(bestiary))
        .route("/api/tasks", get(tasks).post(define_task))
        .route("/api/builds", get(builds))
        .route("/api/history", get(history))
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
        .route("/api/sky", get(sky).post(define_sky_quest))
        .route("/api/wiki", get(wiki))
        .route("/api/incoming", get(incoming))
        .route("/api/buff-marks", get(get_marks).post(set_mark))
        .route("/api/alerts", get(alerts).post(put_alert))
        .route("/api/alerts/delete", post(delete_alert))
        .route("/api/lines", get(lines))
        .route("/api/session", get(session))
        .route("/ws", get(ws_upgrade))
        .with_state(state)
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
    Json(s.snapshot.read().unwrap().clone())
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
    let engine = s.engine.lock().unwrap();
    let search = q.search.map(|v| v.to_lowercase());
    let zone = q.zone.map(|v| v.to_lowercase());

    // Newest first: the most recent fight is nearly always the one wanted.
    let filtered: Vec<_> = engine
        .encounters()
        .iter()
        .rev()
        .filter(|e| {
            if let Some(sc) = &search {
                if !e.title().to_lowercase().contains(sc) {
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
                build_matches(
                    want,
                    &engine.build_label(e.start + (e.end - e.start) / 2),
                )
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
    let engine = s.engine.lock().unwrap();
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
            q.search.as_deref().is_none_or(|sc| {
                e.title().to_lowercase().contains(&sc.to_lowercase())
            })
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
            if e.build.is_empty() { "unknown" } else { &e.build },
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
                "\n### {} — {} ({:.0}% of your damage)\n\n| Ability | Damage | Hits | Crits | Max | Avg |\n|---|--:|--:|--:|--:|--:|\n",
                g.group,
                g.damage,
                g.share * 100.0
            ));
            for a in &g.abilities {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {:.0} |\n",
                    a.name, a.damage, a.hits, a.crits, a.max, a.average
                ));
            }
        }

        if !e.resists.is_empty() {
            out.push_str("\n### Resisted\n\n| Spell | Resisted | Landed | Rate |\n|---|--:|--:|--:|\n");
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
    let engine = s.engine.lock().unwrap();
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
    let engine = s.engine.lock().unwrap();
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
    let engine = s.engine.lock().unwrap();
    match engine.loot_detail(&q.item) {
        Some(d) => Json(d).into_response(),
        None => (StatusCode::NOT_FOUND, "no such item in this log").into_response(),
    }
}

/// Level-progression analysis: per-level history, best zones, best mobs.
async fn experience(State(s): State<AppState>) -> Json<crate::engine::ExperienceReport> {
    let engine = s.engine.lock().unwrap();
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
    let engine = s.engine.lock().unwrap();
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
        let mut engine = s.engine.lock().unwrap();
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
        let mut engine = s.engine.lock().unwrap();
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
    let engine = s.engine.lock().unwrap();
    let now = engine.now();
    Json(engine.lockout_report(now))
}

/// Tradeskill progress, combines and component use.
async fn tradeskills(
    State(s): State<AppState>,
) -> Json<eqlparser_core::tradeskill::TradeskillReport> {
    let engine = s.engine.lock().unwrap();
    Json(engine.tradeskill_report())
}

/// The item database: recipes, drop sources, prices and combine counts, one
/// record per item, tagged with the game the observations came from.
///
/// This is the export a reference site would consume. Curated fields (recipes,
/// tools) and observed fields (drops, prices, combines) are kept in separate
/// keys so a reader can always tell which is which.
async fn items(State(s): State<AppState>) -> Json<crate::engine::ItemDatabase> {
    let mut db = { s.engine.lock().unwrap().item_database() };

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
                for (game, item, merchant, zone, unit_copper, bought, last_bought) in rows {
                    if game != db.game.slug() {
                        continue;
                    }
                    let vendor = eqlparser_core::tradeskill::VendorPrice {
                        merchant,
                        zone: Some(zone).filter(|z| !z.is_empty()),
                        unit_copper,
                        bought,
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
        e.sold_by
            .sort_by_key(|v| std::cmp::Reverse(v.unit_copper));
    }
    for e in &mut db.items {
        e.dropped_by
            .sort_by_key(|d| std::cmp::Reverse(d.looted));
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
        let e = s.engine.lock().unwrap();
        (
            e.bestiary_report(),
            e.bestiary_health_samples(),
            e.game(),
        )
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
        npcs: eqlparser_core::bestiary::merge(
            live.npcs.into_iter().chain(stored),
            &health,
        ),
    })
}

/// What was seen dropping from what, with the kill counts behind it.
async fn drops(State(s): State<AppState>) -> Json<eqlparser_core::drops::DropReport> {
    let engine = s.engine.lock().unwrap();
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
    let engine = s.engine.lock().unwrap();
    Json(engine.incoming_report(q.encounter))
}

#[derive(Debug, Deserialize)]
struct MarkRequest {
    name: String,
    /// "aura", "permanent", or anything else to clear.
    mark: Option<String>,
}

async fn get_marks(State(s): State<AppState>) -> Json<serde_json::Value> {
    let engine = s.engine.lock().unwrap();
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
        let mut engine = s.engine.lock().unwrap();
        engine.set_buff_mark(&req.name, mark);
    }
    // Republish so every client reflects it without waiting for the next line.
    let snap = { s.engine.lock().unwrap().snapshot(40) };
    *s.snapshot.write().unwrap() = snap;
    let _ = s.updates.send(());
    (StatusCode::OK, "ok").into_response()
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
    let engine = s.engine.lock().unwrap();
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
        let mut engine = s.engine.lock().unwrap();
        engine.put_alert_rule(rule.clone());
    }
    if let Some(store) = &s.store {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Err(e) = store.lock().unwrap().put_alert(&rule, now) {
            // The rule is live either way; say plainly that it will not
            // survive a restart rather than pretend it saved.
            return (
                StatusCode::OK,
                format!("active, but not saved: {e}"),
            )
                .into_response();
        }
    }
    // Hand back the stored rule so the client sees a pattern error straight
    // away rather than on the next load.
    let engine = s.engine.lock().unwrap();
    match engine.alert_rules().iter().find(|r| r.id == rule.id) {
        Some(r) => Json(r).into_response(),
        None => (StatusCode::OK, "ok").into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct AlertIdRequest {
    id: String,
}

async fn delete_alert(
    State(s): State<AppState>,
    Json(req): Json<AlertIdRequest>,
) -> Response {
    {
        let mut engine = s.engine.lock().unwrap();
        engine.remove_alert_rule(&req.id);
    }
    if let Some(store) = &s.store {
        let _ = store.lock().unwrap().delete_alert(&req.id);
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
    let engine = s.engine.lock().unwrap();
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
async fn session(State(s): State<AppState>, Query(q): Query<ScopeQuery>) -> Json<serde_json::Value> {
    let zone_only = q.scope.as_deref() == Some("zone");
    let engine = s.engine.lock().unwrap();
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
    let engine = s.engine.lock().unwrap();
    Json(engine.task_report())
}

/// Seed a task's objectives. The client never reports them, so this is the
/// only way they can ever be known.
async fn define_task(
    State(s): State<AppState>,
    Json(def): Json<eqlparser_core::task::TaskDef>,
) -> Response {
    let mut engine = s.engine.lock().unwrap();
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
        let st = store.lock().unwrap();
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
    let st = store.lock().unwrap();
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

/// Captured NPC interactions, the raw material for a quest write-up.
async fn npcs(State(s): State<AppState>) -> Json<eqlparser_core::npc::NpcReport> {
    let engine = s.engine.lock().unwrap();
    Json(engine.npc_report())
}

/// The quest database.
async fn quests(State(s): State<AppState>) -> Json<serde_json::Value> {
    let Some(store) = &s.store else {
        return Json(serde_json::json!({ "available": false, "quests": [] }));
    };
    let st = store.lock().unwrap();
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
    match store.lock().unwrap().put_quest(&q) {
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
    match store.lock().unwrap().delete_quest(&b.name) {
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
    let quests = store.lock().unwrap().quests().unwrap_or_default();
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
    let engine = s.engine.lock().unwrap();
    Json(engine.tell_report())
}

/// Your deaths, with what killed you.
async fn deaths(State(s): State<AppState>) -> Json<crate::engine::DeathReport> {
    let engine = s.engine.lock().unwrap();
    Json(engine.death_report())
}

/// Charm state and recent charm failures, for the resist overlay.
async fn charm(State(s): State<AppState>) -> Json<crate::engine::CharmReport> {
    let engine = s.engine.lock().unwrap();
    Json(engine.charm_report())
}

/// The AA catalogue read from `/alt list`.
async fn aa(State(s): State<AppState>) -> Json<eqlparser_core::aa::AaReport> {
    let engine = s.engine.lock().unwrap();
    Json(engine.aa_report())
}

/// Item reference data: name, stats, icon and a link back to the wiki.
///
/// Served as one map rather than folded into the Sky payload, so rewards and
/// quest items share a single lookup and the quest response stays about
/// progress.
async fn wiki() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        WIKI_ITEMS,
    )
        .into_response()
}

/// Plane of Sky quest progress.
async fn sky(State(s): State<AppState>) -> Json<eqlparser_core::skyquest::SkyReport> {
    let mut report = { s.engine.lock().unwrap().sky_report() };
    // History spans every log ever parsed; the tracker only knows this one.
    if let Some(store) = &s.store {
        if let Ok(stored) = store.lock().unwrap().sky_turnin_counts() {
            Engine::apply_stored_turn_ins(&mut report, &stored);
        }
    }
    Json(report)
}

/// Add or replace one Sky quest, for classes not in the built-in table.
async fn define_sky_quest(
    State(s): State<AppState>,
    Json(def): Json<eqlparser_core::skyquest::SkyQuestDef>,
) -> Response {
    let mut engine = s.engine.lock().unwrap();
    engine.define_sky_quest(def);
    (StatusCode::OK, "ok").into_response()
}

/// Which bosses each class build has cleared.
async fn builds(State(s): State<AppState>) -> Json<Vec<eqlparser_core::lockout::BuildRecord>> {
    let engine = s.engine.lock().unwrap();
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
        let engine = s.engine.lock().unwrap();
        return Json(serde_json::json!({ "ranked": engine.class_ranking() }));
    }
    let snap = s.snapshot.read().unwrap();
    Json(serde_json::json!({ "current": snap.classes }))
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
    let mut rx = state.updates.subscribe();

    let initial = { state.snapshot.read().unwrap().clone() };
    if send_snapshot(&mut socket, &initial).await.is_err() {
        return;
    }

    loop {
        // A lagging client misses intermediate updates, which is harmless: the
        // next snapshot is complete in itself.
        match rx.recv().await {
            Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => break,
        }
        let snap = { state.snapshot.read().unwrap().clone() };
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

    fn state_with(lines: &[&str]) -> AppState {
        let mut engine = Engine::new("Tharn".to_string(), SpellDb::empty());
        for l in lines {
            engine.push_line(l);
        }
        let snap = engine.snapshot(50);
        let (tx, _) = broadcast::channel(4);
        AppState {
            store: None,
            log_path: std::path::PathBuf::from("eqlog_Test_freeport.txt"),
            logs_dir: None,
            engine: Arc::new(Mutex::new(engine)),
            snapshot: Arc::new(RwLock::new(snap)),
            updates: tx,
        }
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
}
