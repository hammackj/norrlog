//! `eqlparserd` — the EverQuest Legends log analysis daemon.
//!
//! Parses a character's log, follows it live, and serves both a REST/WebSocket
//! API and the overlay page. The Electron GUI and OBS both talk to this one
//! process, so there is a single source of truth for what is happening in game.

mod engine;
mod server;
mod store;
mod tail;

use engine::Engine;
use eqlparser_core::Parser;
use server::AppState;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::broadcast;

/// How many recent encounters to include in the pushed snapshot. The full set
/// is available through the REST API; this only bounds the live payload.
const SNAPSHOT_RECENT: usize = 40;

const DEFAULT_PORT: u16 = 8777;

/// How often durable history is written while following live.
const HISTORY_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

struct Config {
    log: Option<PathBuf>,
    logs_dir: Option<PathBuf>,
    game_dir: Option<PathBuf>,
    port: u16,
}

fn usage() -> ! {
    eprintln!(
        "eqlparserd — EverQuest Legends log parser daemon

USAGE:
    eqlparserd [--log <file>] [--logs-dir <dir>] [--game-dir <dir>] [--port <n>]

OPTIONS:
    --log <file>       Log file to follow. If omitted, the newest eqlog_*.txt
                       in --logs-dir is used.
    --logs-dir <dir>   Directory of EverQuest logs.
    --game-dir <dir>   Game install directory, for spells_us.txt. Without it,
                       buff timers and class detection are unavailable.
    --port <n>         HTTP port (default 8777).

ENDPOINTS:
    /                  Status page with overlay setup instructions
    /overlay           The overlay (add to OBS as a Browser Source)
    /api/snapshot      Current state
    /api/encounters    Historical encounters (filter, search, paginate)
    /api/loot          Aggregated loot
    /ws                Live snapshot stream
"
    );
    std::process::exit(2)
}

fn parse_args() -> Config {
    let mut cfg = Config {
        log: None,
        logs_dir: None,
        game_dir: None,
        port: DEFAULT_PORT,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--log" => cfg.log = args.next().map(PathBuf::from),
            "--logs-dir" => cfg.logs_dir = args.next().map(PathBuf::from),
            "--game-dir" => cfg.game_dir = args.next().map(PathBuf::from),
            "--port" => {
                cfg.port = args
                    .next()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(DEFAULT_PORT)
            }
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown argument: {other}");
                usage()
            }
        }
    }
    cfg
}

/// Resolve which log file to follow.
///
/// An explicit `--log` always wins. Otherwise we take the most recently written
/// log in `--logs-dir`, which is the character the user was last playing.
fn resolve_log(cfg: &Config) -> anyhow::Result<PathBuf> {
    if let Some(p) = &cfg.log {
        return Ok(p.clone());
    }
    if let Some(dir) = &cfg.logs_dir {
        if let Some(p) = tail::newest_log_in(dir) {
            return Ok(p);
        }
        anyhow::bail!("no eqlog_*.txt found in {}", dir.display());
    }
    anyhow::bail!("specify --log <file> or --logs-dir <dir>")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = parse_args();
    let log_path = resolve_log(&cfg)?;

    let player = Parser::player_from_filename(&log_path.to_string_lossy())
        .unwrap_or_else(|| "You".to_string());
    eprintln!(
        "[eqlparserd] following {} (character: {player})",
        log_path.display()
    );

    let spell_db = Engine::load_spell_db(cfg.game_dir.as_deref());
    let engine = Engine::new(player, spell_db);

    // History outlives any one log file. Opened before the follower starts so
    // the first flush after catch-up has somewhere to go.
    let history = match store::Store::open(&history_path()) {
        Ok(s) => {
            if let Ok((k, e, l)) = s.counts() {
                eprintln!("[eqlparserd] history: {k} boss kills, {e} raid fights, {l} levels");
            }
            Some(Arc::new(Mutex::new(s)))
        }
        Err(e) => {
            eprintln!("[eqlparserd] history unavailable ({e}); running without it");
            None
        }
    };

    let engine = Arc::new(Mutex::new(engine));
    let snapshot = Arc::new(RwLock::new(engine.lock().unwrap().snapshot(SNAPSHOT_RECENT)));
    let (updates, _) = broadcast::channel(64);

    spawn_follower(
        log_path,
        Arc::clone(&engine),
        Arc::clone(&snapshot),
        updates.clone(),
        history.clone(),
    );

    let state = AppState {
        engine,
        snapshot,
        updates,
        store: history,
    };

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("[eqlparserd] listening on http://{addr}");
    eprintln!("[eqlparserd] overlay:  http://{addr}/overlay");

    axum::serve(listener, server::router(state)).await?;
    Ok(())
}

/// Run the log follower on its own thread.
///
/// Parsing is CPU-bound and synchronous, so it lives on a dedicated OS thread
/// rather than competing with the async runtime. It publishes a snapshot after
/// each batch and signals listeners.
/// Where durable history lives. One database per machine, keyed internally by
/// nothing — a different character means a different log and, in practice, a
/// different install; mixing two would need a character column and has no user
/// asking for it yet.
fn history_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".config/eqlparser/history.db")
}

/// Write derived history to the store.
///
/// Every row carries a natural key and is written with REPLACE, so this is
/// safe to call as often as we like: re-flushing unchanged history is a no-op
/// in effect, and a flush that now knows more (a build label that needed later
/// evidence) improves the stored row.
fn flush_history(engine: &Arc<Mutex<Engine>>, store: &Arc<Mutex<store::Store>>, path: &Path) {
    let (kills, encs, levels, aa) = { engine.lock().unwrap().history_to_persist() };
    let sky_turn_ins = { engine.lock().unwrap().sky_turn_ins() };
    let mut st = store.lock().unwrap();
    if let Err(e) = st
        .put_kills(&kills)
        .and_then(|_| st.put_encounters(&encs))
        .and_then(|_| st.put_levels(&levels))
        .and_then(|_| st.put_aa(&aa))
        .and_then(|_| st.put_sky_turnins(&sky_turn_ins))
    {
        eprintln!("[eqlparserd] history write failed: {e}");
        return;
    }
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let _ = st.note_source(&path.to_string_lossy(), bytes, now);
}

fn spawn_follower(
    path: PathBuf,
    engine: Arc<Mutex<Engine>>,
    snapshot: Arc<RwLock<engine::Snapshot>>,
    updates: broadcast::Sender<()>,
    store: Option<Arc<Mutex<store::Store>>>,
) {
    std::thread::spawn(move || {
        let mut follower = match tail::Follower::open(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[eqlparserd] cannot open {}: {e}", path.display());
                return;
            }
        };

        // Initial pass over the whole file builds history.
        let start = std::time::Instant::now();
        let mut backlog = 0usize;
        {
            let mut eng = engine.lock().unwrap();
            if let Err(e) = follower.read_available(|line| {
                eng.push_line(line);
                backlog += 1;
            }) {
                eprintln!("[eqlparserd] read error: {e}");
            }
            eng.set_live(true);
        }
        publish(&engine, &snapshot, &updates);
        eprintln!(
            "[eqlparserd] parsed {backlog} lines in {:.2}s; following live",
            start.elapsed().as_secs_f64()
        );
        if let Some(st) = &store {
            flush_history(&engine, st, &path);
        }

        // Follow.
        let mut last_flush = std::time::Instant::now();
        loop {
            std::thread::sleep(tail::POLL_INTERVAL);
            {
                let mut eng = engine.lock().unwrap();
                if let Err(e) = follower.read_available(|line| eng.push_line(line)) {
                    eprintln!("[eqlparserd] read error: {e}");
                }
            }
            // Always republish: buff timers count down even with no new lines.
            publish(&engine, &snapshot, &updates);

            // History changes on the scale of minutes, so flushing on the poll
            // interval would be pure write amplification.
            if last_flush.elapsed() >= HISTORY_FLUSH_INTERVAL {
                last_flush = std::time::Instant::now();
                if let Some(st) = &store {
                    flush_history(&engine, st, &path);
                }
            }
        }
    });
}

fn publish(
    engine: &Arc<Mutex<Engine>>,
    snapshot: &Arc<RwLock<engine::Snapshot>>,
    updates: &broadcast::Sender<()>,
) {
    let snap = { engine.lock().unwrap().snapshot(SNAPSHOT_RECENT) };
    *snapshot.write().unwrap() = snap;
    // No receivers is normal (nothing connected yet), so ignore send errors.
    let _ = updates.send(());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_log_wins_over_directory() {
        let cfg = Config {
            log: Some(PathBuf::from("/tmp/eqlog_Tharn_test.txt")),
            logs_dir: Some(PathBuf::from("/nonexistent")),
            game_dir: None,
            port: DEFAULT_PORT,
        };
        assert_eq!(
            resolve_log(&cfg).unwrap(),
            PathBuf::from("/tmp/eqlog_Tharn_test.txt")
        );
    }

    #[test]
    fn missing_log_and_dir_is_an_error() {
        let cfg = Config {
            log: None,
            logs_dir: None,
            game_dir: None,
            port: DEFAULT_PORT,
        };
        assert!(resolve_log(&cfg).is_err());
    }

    #[test]
    fn player_name_comes_from_the_filename() {
        assert_eq!(
            Parser::player_from_filename("/x/eqlog_Tharn_freeport.txt").as_deref(),
            Some("Tharn")
        );
    }
}
