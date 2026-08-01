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
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::broadcast;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
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
        .route("/api/classes", get(classes))
        .route("/api/experience", get(experience))
        .route("/api/lockouts", get(lockouts))
        .route("/api/tradeskills", get(tradeskills))
        .route("/api/tasks", get(tasks).post(define_task))
        .route("/api/builds", get(builds))
        .route("/api/aa", get(aa))
        .route("/api/sky", get(sky).post(define_sky_quest))
        .route("/api/incoming", get(incoming))
        .route("/api/buff-marks", get(get_marks).post(set_mark))
        .route("/ws", get(ws_upgrade))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Static assets
// ---------------------------------------------------------------------------

/// Assets are embedded so the binary runs from anywhere, including when
/// launched as a child process by Electron with an arbitrary CWD.
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
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], OVERLAY_CSS).into_response()
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
}

/// The historical browser: filter and page through every parsed encounter.
async fn encounters(State(s): State<AppState>, Query(q): Query<ListQuery>) -> Json<serde_json::Value> {
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
            true
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

/// Full detail for one fight, including per-actor and per-ability breakdowns.
async fn encounter_detail(State(s): State<AppState>, AxPath(id): AxPath<u64>) -> Response {
    let engine = s.engine.lock().unwrap();
    match engine.encounter(id) {
        Some(e) => Json(serde_json::json!({
            "summary": engine.summarise(e, true),
            "encounter": e,
            "charmed_pets": e.charmed_pets(),
            "your_damage_with_pets": e.your_damage_with_pets(),
        }))
        .into_response(),
        None => (StatusCode::NOT_FOUND, "no such encounter").into_response(),
    }
}

async fn loot(State(s): State<AppState>) -> Json<serde_json::Value> {
    let engine = s.engine.lock().unwrap();
    let tally = engine.loot_tally();
    Json(serde_json::json!({ "total": tally.len(), "items": tally }))
}

/// Level-progression analysis: per-level history, best zones, best mobs.
async fn experience(State(s): State<AppState>) -> Json<crate::engine::ExperienceReport> {
    let engine = s.engine.lock().unwrap();
    let now = engine.now();
    Json(engine.experience_report(now))
}

/// Weekly/daily boss lockouts, as a boss x difficulty grid.
async fn lockouts(
    State(s): State<AppState>,
) -> Json<eqlparser_core::lockout::LockoutReport> {
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

/// The AA catalogue read from `/alt list`.
async fn aa(State(s): State<AppState>) -> Json<eqlparser_core::aa::AaReport> {
    let engine = s.engine.lock().unwrap();
    Json(engine.aa_report())
}

/// Plane of Sky quest progress.
async fn sky(State(s): State<AppState>) -> Json<eqlparser_core::skyquest::SkyReport> {
    let engine = s.engine.lock().unwrap();
    Json(engine.sky_report())
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
async fn builds(
    State(s): State<AppState>,
) -> Json<Vec<eqlparser_core::lockout::BuildRecord>> {
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
        let Json(v) = encounters(State(s), Query(ListQuery {
            limit: None, offset: None, search: None, zone: None,
            since: None, until: None, min_duration: None,
        }))
        .await;
        assert_eq!(v["total"], 2);
        let list = v["encounters"].as_array().unwrap();
        assert_eq!(list[0]["title"], "a giant rat", "newest fight should lead");
    }

    #[tokio::test]
    async fn encounters_endpoint_filters_by_search() {
        let s = state_with(SAMPLE);
        let Json(v) = encounters(State(s), Query(ListQuery {
            limit: None, offset: None, search: Some("mummy".into()), zone: None,
            since: None, until: None, min_duration: None,
        }))
        .await;
        assert_eq!(v["total"], 1);
        assert_eq!(v["encounters"][0]["title"], "a lesser mummy");
    }

    #[tokio::test]
    async fn encounters_endpoint_paginates() {
        let s = state_with(SAMPLE);
        let Json(v) = encounters(State(s), Query(ListQuery {
            limit: Some(1), offset: Some(1), search: None, zone: None,
            since: None, until: None, min_duration: None,
        }))
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
        let Json(v) = loot(State(s)).await;
        assert_eq!(v["total"], 1);
        assert_eq!(v["items"][0]["item"], "Mummy Wrap");
    }
}
