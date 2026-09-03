//! `eqlparserd` — the EverQuest Legends log analysis daemon.
//!
//! Parses a character's log, follows it live, and serves both a REST/WebSocket
//! API and the overlay page. The Electron GUI and OBS both talk to this one
//! process, so there is a single source of truth for what is happening in game.

mod engine;
mod itemdb;
mod locks;
mod paths;
mod server;
mod store;
mod tail;

use engine::Engine;
use locks::{Guarded, Shared};
use eqlparser_core::game::Game;
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
    /// Which game the log is from, when declared. Left `None` to infer it
    /// from the install path.
    game: Option<Game>,
    /// Origins of the eqdb companion site, tried in order.
    ///
    /// A list, because both defaults are right at different times: a local
    /// instance while the site is being worked on, and the published one
    /// otherwise. A wrong guess costs a refused connection.
    eqdb: Vec<String>,
    /// How far back to read on startup, in seconds. `None` reads everything.
    since: Option<i64>,
    /// Exit when the process that started this one goes away.
    exit_with_parent: bool,
}

/// Parse a load window: `2h`, `4h`, `8h`, `1d`, `2d`, `all`.
///
/// Deliberately a small vocabulary rather than a duration parser. The choice
/// is offered as a handful of buttons, and accepting `90m` here would mean a
/// setting the window picker cannot show.
fn parse_since(raw: &str) -> Option<Option<i64>> {
    let r = raw.trim().to_lowercase();
    if r == "all" || r.is_empty() {
        return Some(None);
    }
    let (num, unit) = r.split_at(r.len().checked_sub(1)?);
    let n: i64 = num.parse().ok()?;
    if n <= 0 {
        return None;
    }
    match unit {
        "h" => Some(Some(n * 3600)),
        "d" => Some(Some(n * 86_400)),
        _ => None,
    }
}

/// Where to look for the companion site when nothing says otherwise.
///
/// The published site, and only it, now that there is one. A local instance
/// was tried first while the site was being built, which was right then and is
/// wrong now: it meant whoever happened to be running a copy on port 8788 got
/// different answers from everyone else, and a half-built local copy silently
/// took precedence over the finished article.
///
/// Developing against a local instance is still a `--eqdb http://localhost:8788`
/// away — deliberately something you say rather than something you get.
const DEFAULT_EQDB: &[&str] = &["https://eqldb.hammackj.com"];

fn usage() -> ! {
    eprintln!(
        "eqlparserd — EverQuest Legends log parser daemon

USAGE:
    eqlparserd [--log <file>] [--logs-dir <dir>] [--game-dir <dir>] [--port <n>]
               [--game <eq|eql>]

OPTIONS:
    --log <file>       Log file to follow. If omitted, the newest eqlog_*.txt
                       in --logs-dir is used.
    --logs-dir <dir>   Directory of EverQuest logs.
    --game-dir <dir>   Game install directory, for spells_us.txt. Without it,
                       buff timers and class detection are unavailable.
    --game <name>      Which game this log is from: eq (EverQuest) or eql
                       (EverQuest Legends). Inferred from the install path
                       when omitted, and left unknown if that says nothing.
                       Everything the daemon records is tagged with it.
    --port <n>         HTTP port (default 8777).
    --since <window>   How much of the log to read on startup: 2h, 4h, 8h, 1d,
                       2d, or all (default). A shorter window starts faster,
                       at the cost of every figure derived from the log — a
                       two-hour window means two hours of loot, kills and
                       encounters, not a lifetime of them. Durable history in
                       the database is unaffected.
    --exit-with-parent Exit when the process that started this one does, by
                       watching for the parent pid to change. For the desktop
                       app, which owns the daemon and must not leave it running
                       when it is killed rather than asked to quit.
    --eqdb <url>       Origin of the eqdb companion site, used for item
                       tooltips (default https://eqldb.hammackj.com, or
                       $EQLPARSER_EQDB). May be given more than once, or as a
                       comma-separated list, and is tried in order — pass a
                       local instance to develop against one. When none can be
                       reached the bundled item data is used, so this is never
                       required.

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

/// Exit when whoever started this process goes away.
///
/// # Why the parent pid, and not a pipe or a signal
///
/// The desktop app owns this daemon: it spawns it, and its quit handlers kill
/// it. Those handlers cover every way the app can be *told* to stop. They
/// cover none of the ways it can stop existing — SIGKILL, a crash, the OOM
/// killer, a session ending out from under it — and in each of those this
/// process was reparented to init and carried on, holding the port, tailing
/// the log and answering requests for a window that is gone. The next launch
/// then finds its port taken by a daemon the user cannot see to close.
///
/// The obvious fix is a pipe from the parent, watched for end-of-file. It does
/// not work here, and the reason is worth writing down: Electron's helper
/// processes — gpu, zygote, broker — inherit the parent's descriptors, and
/// they outlive a SIGKILL of the main process. The write end therefore stays
/// open in a process nobody is tracking, no end-of-file ever arrives, and the
/// daemon waits forever on a parent that has been dead for an hour. This was
/// measured, not guessed: `/proc/<daemon>/fd/0` was still held by an orphaned
/// gpu-process.
///
/// `getppid` cannot be inherited or held open by anybody. The kernel changes
/// it to the reaper's pid the instant the real parent dies, whatever killed
/// it, so the original value being gone *is* the event — no signal handler, no
/// descriptor, and no pid-reuse race to reason about, since this is the
/// kernel's own answer to "who is my parent" rather than a number we stored
/// and compared.
///
/// Opt-in, because a daemon started from a terminal belongs to whoever started
/// it: its parent is the shell, and a shell exiting should not necessarily
/// take it down.
#[cfg(unix)]
fn exit_with_parent() {
    use std::io::Write;
    let original = std::os::unix::process::parent_id();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if std::os::unix::process::parent_id() != original {
            /* Reported with a write that is allowed to fail.
             *
             * Not a stylistic preference — `eprintln!` panics when the write
             * fails, and the write fails *here specifically*: this daemon's
             * stderr is a pipe to the app, so the process being reported dead
             * is the one that was reading this. The panic killed the watchdog
             * thread before it reached the exit below, and the daemon then ran
             * on forever having correctly noticed that it should not. The
             * failure looks exactly like the watchdog never running, which is
             * why it is written down here rather than left to be rediscovered.
             */
            let _ = writeln!(
                std::io::stderr(),
                "[eqlparserd] the process that started this one is gone; exiting"
            );
            // The follower flushes history on a timer and holds the engine
            // lock while it ingests, so there is nothing to join here that
            // would not risk hanging on the way out. Whatever the last flush
            // wrote is what survives — exactly what a kill would have left.
            std::process::exit(0);
        }
    });
}

/// No parent to watch. The flag is accepted and does nothing rather than
/// failing, so one launcher can pass it on every platform.
#[cfg(not(unix))]
fn exit_with_parent() {}

/// Split a comma-separated list of origins, dropping blanks.
fn split_origins(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().trim_end_matches('/'))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_args() -> Config {
    let mut cfg = Config {
        log: None,
        logs_dir: None,
        game_dir: None,
        port: DEFAULT_PORT,
        game: None,
        since: None,
        eqdb: std::env::var("EQLPARSER_EQDB")
            .ok()
            .map(|v| split_origins(&v))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_EQDB.iter().map(|s| s.to_string()).collect()),
        // Off by default: run from a terminal, this daemon belongs to whoever
        // started it and should outlive nothing in particular.
        exit_with_parent: false,
    };
    let mut saw_eqdb = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--log" => cfg.log = args.next().map(PathBuf::from),
            "--logs-dir" => cfg.logs_dir = args.next().map(PathBuf::from),
            "--game-dir" => cfg.game_dir = args.next().map(PathBuf::from),
            "--game" => {
                let raw = args.next().unwrap_or_default();
                match Game::parse(&raw) {
                    Some(g) => cfg.game = Some(g),
                    None => {
                        eprintln!("[eqlparserd] unknown --game {raw:?}; expected eq or eql");
                        usage()
                    }
                }
            }
            "--port" => {
                cfg.port = args
                    .next()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(DEFAULT_PORT)
            }
            "--since" => {
                let raw = args.next().unwrap_or_default();
                match parse_since(&raw) {
                    Some(v) => cfg.since = v,
                    None => {
                        eprintln!(
                            "[eqlparserd] unknown --since {raw:?}; expected 2h, 4h, 8h, 1d, 2d or all"
                        );
                        usage()
                    }
                }
            }
            "--eqdb" => {
                if let Some(u) = args.next() {
                    // Repeating the flag adds an origin rather than replacing
                    // the last one, but the first `--eqdb` replaces the
                    // defaults: having asked for a specific site, being
                    // silently fallen back to another is not what was meant.
                    if !saw_eqdb {
                        cfg.eqdb.clear();
                        saw_eqdb = true;
                    }
                    cfg.eqdb.extend(split_origins(&u));
                }
            }
            "--exit-with-parent" => cfg.exit_with_parent = true,
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
    // Before anything slow: a parent that dies during a long initial parse
    // should still take this with it.
    if cfg.exit_with_parent {
        exit_with_parent();
    }
    let log_path = resolve_log(&cfg)?;

    // The filename is the only place the character and server appear; history
    // is keyed on both so an alt or a second server keeps its own records.
    let (player, server) = Parser::identity_from_filename(&log_path.to_string_lossy())
        .unwrap_or_else(|| ("You".to_string(), String::new()));
    eprintln!(
        "[eqlparserd] following {} (character: {player}, server: {})",
        log_path.display(),
        if server.is_empty() {
            "unknown"
        } else {
            &server
        }
    );

    // Nothing in a log says which game wrote it, so this comes from the
    // install being followed, or is declared. An install directory is the
    // better evidence of the two paths — a log file can be copied anywhere.
    let game = cfg.game.unwrap_or_else(|| {
        Game::detect(
            [cfg.game_dir.as_deref(), Some(log_path.as_path())]
                .into_iter()
                .flatten(),
        )
    });
    eprintln!(
        "[eqlparserd] game: {game}{}",
        match (cfg.game.is_some(), game) {
            (true, _) => " (declared)",
            // Worth saying out loud: everything recorded from here on carries
            // this tag, and an untagged pile of observations is hard to fix
            // afterwards.
            (false, Game::Unknown) =>
                " — pass --game eq or --game eql to tag what this session records",
            (false, _) => " (from the install path)",
        }
    );

    let spell_db = Engine::load_spell_db(cfg.game_dir.as_deref());
    let engine = Engine::with_game(player.clone(), spell_db, game);

    // History outlives any one log file. Opened before the follower starts so
    // the first flush after catch-up has somewhere to go.
    // A path that cannot be anchored is an error like any other, and lands in
    // the same "running without it" branch. Before, it became the *relative*
    // path `.config/eqlparser/history.db`: the database followed the working
    // directory, so a daemon started from elsewhere found an empty history and
    // accumulated nothing — which reads as a parsing fault, not a path one.
    let history = match history_path()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no absolute home directory; set HOME, or XDG_DATA_HOME"
            )
        })
        .and_then(|db| store::Store::open_as(&db, &player, &server))
    {
        Ok(s) => {
            if let Ok((k, e, l)) = s.counts() {
                eprintln!(
                    "[eqlparserd] history for {player}: {k} boss kills, {e} raid fights, {l} levels"
                );
            }
            if let Ok(all) = s.characters() {
                if all.len() > 1 {
                    let others: Vec<String> = all
                        .iter()
                        .filter(|(c, sv, _)| c != &player || sv != &server)
                        .map(|(c, sv, n)| format!("{c} on {sv} ({n} kills)"))
                        .collect();
                    if !others.is_empty() {
                        eprintln!(
                            "[eqlparserd] other characters on file: {}",
                            others.join(", ")
                        );
                    }
                }
            }
            Some(Arc::new(Mutex::new(s)))
        }
        Err(e) => {
            eprintln!("[eqlparserd] history unavailable ({e}); running without it");
            None
        }
    };

    // Levels the player entered by hand, applied before the log is read so the
    // parse can raise them but never start from scratch. A class that capped
    // before this log began has nothing in it to say so.
    let mut engine = engine;
    if let Some(store) = &history {
        if let Ok(store) = store.lock() {
            match store.class_levels() {
                Ok(rows) => {
                    for (abbrev, level, at) in rows {
                        if let Some(class) = eqlparser_core::class::parse_class_list(&abbrev)
                            .into_iter()
                            .next()
                        {
                            engine.set_stated_level(class, level, at);
                        }
                    }
                }
                Err(e) => eprintln!("[eqlparserd] could not read stated class levels: {e}"),
            }
            if let Ok(Some(spent)) = store.stated("aa_spent") {
                engine.set_stated_aa_spent(spent as u32);
            }
            // The level the bestiary should assume until this log says
            // otherwise; see `Store::last_known_level` for where it comes from.
            match store.last_known_level() {
                Ok(level) => engine.set_known_level(level),
                Err(e) => eprintln!("[eqlparserd] could not read the last known level: {e}"),
            }
            // Alerts are not scoped to a character, so these load whichever
            // log is being followed. They stay disarmed until the backlog is
            // consumed; see `Engine::set_live`.
            match store.alerts() {
                Ok(rules) => {
                    if !rules.is_empty() {
                        eprintln!("[eqlparserd] {} alert rules loaded", rules.len());
                    }
                    engine.set_alert_rules(rules);
                }
                Err(e) => eprintln!("[eqlparserd] could not read alert rules: {e}"),
            }
        }
    }

    let engine = Arc::new(Mutex::new(engine));
    let snapshot = Arc::new(RwLock::new(
        engine.locked("engine").snapshot(SNAPSHOT_RECENT),
    ));
    let (updates, _) = broadcast::channel(64);

    spawn_follower_since(
        log_path.clone(),
        cfg.since,
        Arc::clone(&engine),
        Arc::clone(&snapshot),
        updates.clone(),
        history.clone(),
    );

    let state = AppState {
        log_path: log_path.clone(),
        logs_dir: cfg.logs_dir.clone(),
        engine,
        snapshot,
        updates,
        store: history,
        itemdb: std::sync::Arc::new(crate::itemdb::ItemDb::new(cfg.eqdb.clone())),
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
fn history_path() -> Option<PathBuf> {
    paths::data_file("history.db")
}

/// Write derived history to the store.
///
/// Every row carries a natural key and is written with REPLACE, so this is
/// safe to call as often as we like: re-flushing unchanged history is a no-op
/// in effect, and a flush that now knows more (a build label that needed later
/// evidence) improves the stored row.
fn flush_history(engine: &Arc<Mutex<Engine>>, store: &Arc<Mutex<store::Store>>, path: &Path) {
    let (kills, encs, levels, aa) = { engine.locked("engine").history_to_persist() };
    let sky_turn_ins = { engine.locked("engine").sky_turn_ins() };
    let grants = { engine.locked("engine").grants_to_persist() };
    let (drops, prices, game, bestiary, health) = {
        let e = engine.locked("engine");
        (
            e.drop_report(),
            e.tradeskill_report().prices,
            e.game(),
            e.bestiary_report(),
            e.bestiary_health_samples(),
        )
    };
    let source = path.to_string_lossy().to_string();
    let mut st = store.locked("history");
    if let Err(e) = st
        .put_kills(&kills)
        .and_then(|_| st.put_encounters(&encs))
        .and_then(|_| st.put_levels(&levels))
        .and_then(|_| st.put_aa(game.slug(), &aa))
        .and_then(|_| st.put_sky_turnins(&sky_turn_ins))
        .and_then(|_| st.put_grants(&grants))
        .and_then(|_| st.put_drops(game.slug(), &source, &drops))
        .and_then(|_| st.put_vendors(game.slug(), &source, &prices))
        .and_then(|_| st.put_bestiary(game.slug(), &source, &bestiary, &health))
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

fn spawn_follower_since(
    path: PathBuf,
    since: Option<i64>,
    engine: Arc<Mutex<Engine>>,
    snapshot: Arc<RwLock<engine::Snapshot>>,
    updates: broadcast::Sender<()>,
    store: Option<Arc<Mutex<store::Store>>>,
) {
    std::thread::spawn(move || {
        // A window is measured from the log's own end, not the wall clock:
        // the two are a timezone apart, and a log last written yesterday would
        // otherwise come back empty.
        let cutoff = since.and_then(|secs| {
            tail::Follower::last_timestamp(&path)
                .ok()
                .flatten()
                .map(|end| end - secs)
        });
        let opened = match cutoff {
            Some(c) => tail::Follower::open_since(&path, c),
            None => tail::Follower::open(&path),
        };
        let mut follower = match opened {
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
            let mut eng = engine.locked("engine");
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
                let mut eng = engine.locked("engine");
                if let Err(e) = follower.read_available(|line| eng.push_line(line)) {
                    eprintln!("[eqlparserd] read error: {e}");
                }
                // Time-based alerts. Runs on the quiet polls too — a buff
                // running out while nothing is happening is precisely when a
                // warning earns its keep, and no line marks it.
                eng.tick();
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
    let snap = { engine.locked("engine").snapshot(SNAPSHOT_RECENT) };
    *snapshot.writing("snapshot") = snap;
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
            game: None,
            since: None,
            eqdb: DEFAULT_EQDB.iter().map(|s| s.to_string()).collect(),
            exit_with_parent: false,
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
            game: None,
            since: None,
            eqdb: DEFAULT_EQDB.iter().map(|s| s.to_string()).collect(),
            exit_with_parent: false,
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
    /// A small vocabulary on purpose: the picker offers these and nothing
    /// else, so accepting `90m` would mean a setting it cannot show.
    #[test]
    fn the_load_window_accepts_what_the_picker_offers() {
        assert_eq!(parse_since("2h"), Some(Some(7_200)));
        assert_eq!(parse_since("8h"), Some(Some(28_800)));
        assert_eq!(parse_since("1d"), Some(Some(86_400)));
        assert_eq!(parse_since("2d"), Some(Some(172_800)));
        assert_eq!(parse_since("all"), Some(None));
        assert_eq!(parse_since(""), Some(None));
        assert_eq!(parse_since("ALL"), Some(None));
        // Rejected rather than quietly reinterpreted.
        assert_eq!(parse_since("90m"), None);
        assert_eq!(parse_since("0h"), None);
        assert_eq!(parse_since("-2h"), None);
        assert_eq!(parse_since("h"), None);
        assert_eq!(parse_since("banana"), None);
    }


}
