//! `eqlparserd` — the EverQuest Legends log analysis daemon.
//!
//! Parses a character's log, follows it live, and serves both a REST/WebSocket
//! API and the overlay page. The Electron GUI and OBS both talk to this one
//! process, so there is a single source of truth for what is happening in game.

mod archive;
mod bundle;
mod catchup;
mod dumps;
mod engine;
mod itemdb;
mod locks;
mod paths;
mod record;
mod server;
mod shots;
mod site;
mod store;
mod tail;

use anyhow::Context;
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
    /// Exit this many seconds after the last client has gone and nothing has
    /// been asked of the daemon. `None` keeps running.
    idle_exit: Option<u64>,
    /// The client's `Screenshots` folder, when it is not under `--game-dir`.
    shots: Option<PathBuf>,
    /// Do not read screenshots at all, even where everything for it is here.
    no_shots: bool,
    no_archive: bool,
    archive_dry_run: bool,
    /// Read what is already in the Screenshots folder, once, at startup.
    backlog: bool,
    /// A previously accumulated item-id record to take on, once.
    adopt_item_ids: Option<PathBuf>,
    /// Read the log once, write the record's reports as JSON, and exit.
    record_out: Option<PathBuf>,
    /// Every `--log` given, in order. `log` above is the one the daemon
    /// follows; this is the whole list, which only the one-shot bundle modes
    /// read. A contributor has one log per character per server and the
    /// record is the union of all of them.
    logs: Vec<PathBuf>,
    /// Build the bundle from the record and write it here, then exit.
    bundle_out: Option<PathBuf>,
    /// Build the bundle, say what sending it would do, send it, then exit.
    send: bool,
    /// Whose rows these are, for this run. Falls back to the name on file.
    contributor: Option<String>,
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
    --ocr <dir>        Accepted and ignored. Screenshots are read by this
                       binary; there is no longer a Python tree to point at.
                       Reading needs only a token, from $EQLDB_TOKEN or
                       ~/.config/eqlparser/eqldb-token at mode 600, never from
                       a flag: /proc/<pid>/cmdline is world-readable. With one
                       set, every screenshot the client announces is read, and
                       any item window in it that eqdb wants — or has never
                       heard of — is sent to eqdb's review queue.
    --shots <dir>      The client's Screenshots folder. Defaults to
                       $EQL_SHOTS_DIR, else <game-dir>/Screenshots.
    --no-shots         Do not read screenshots, whatever else is configured.
    --no-archive       Read screenshots but leave them in the client's folder.
    --archive-dry-run  Decide where every screenshot belongs and move none.
    --backlog          Read what is already in the Screenshots folder, once, on
                       startup, rather than only what arrives after it. Needs
                       the archive on: filing is what empties the inbox, so
                       without it the same folder would be re-read on every
                       restart.
    --adopt-item-ids <file>
                       Fold a previously accumulated item-id record into this
                       machine's, sweep the game folder once, and exit without
                       following anything. For the record the Python capture
                       tree kept: its ids include items whose dumps have since
                       been overwritten, which no rescan can recover. Ids
                       already known are left alone, so running it twice
                       changes nothing.
    --bundle-out <file>
                       Parse, build the contribution bundle, print what
                       sending it would do to the site, write it here and
                       exit. Follows nothing and opens no port.
    --send             The same, and then send it to eqdb. Needs a token --
                       see --ocr for where one is kept -- and a name, from
                       --contributor or ~/.config/eqlparser/contributor.
                       Refuses whatever the printed verdicts call unsafe,
                       which is the same list the desktop app refuses on.

                       Both read *every* log they are given, not the newest:
                       pass --log more than once, or --logs-dir on its own to
                       read the whole folder. A bundle says what you know,
                       and that is the union of your characters. The record
                       accumulates in the same history the app uses, so a log
                       read today still counts after it has rotated away.
    --contributor <name>
                       Whose rows these are, for this run. A name, given by
                       the person it belongs to -- never taken from the
                       token, which would be a credential filing rows under
                       a name nobody chose.
    --record-out <dir> Parse the log once, write the record — considers,
                       faction, players, skills, carto, spots — into <dir> as
                       <what>.<log stem>.json, and exit. The file names and
                       shapes eqldb's rebuild has always written, so the two
                       readings of one log can be diffed against each other.
                       Touches no history database and follows nothing.
    --exit-with-parent Exit when the process that started this one does, by
                       watching for the parent pid to change. For the desktop
                       app, which owns the daemon and must not leave it running
                       when it is killed rather than asked to quit.
    --idle-exit <secs> Exit once no client has held a socket open and nothing
                       has been asked of the daemon for <secs> seconds. For a
                       daemon shared by several apps: whichever starts it, it
                       outlives any one of them and goes when all are done.
                       The OBS overlay holds a socket, so a stream keeps it up.
    --eqdb <url>       Origin of the eqdb companion site, used for item
                       tooltips (default https://eqldb.hammackj.com, or
                       $EQLPARSER_EQDB). May be given more than once, or as a
                       comma-separated list, and is tried in order — pass a
                       local instance to develop against one. When none can be
                       reached the bundled item data is used, so this is never
                       required.

EXIT STATUS:
    3                  The port is already taken -- most likely by another
                       eqlparserd. Nothing was started; attach to that one
                       (GET /api/hello says who holds the port).

ENDPOINTS:
    /                  Status page with overlay setup instructions
    /api/hello         Who this daemon is, what it follows, and its API version
    /api/events        Live typed events (WebSocket)
    /api/pickups       What you picked up since ?since= (log clock; default an hour)
    /api/bags          What the followed character's inventory dumps hold
    /api/bundle        The contribution bundle itself, for the collector to send
    /overlay           The overlay (add to OBS as a Browser Source)
    /api/snapshot      Current state
    /api/shots/recent  What the screenshot reader made of the last few pictures
    /api/items/held    What the site still wants that is in your bags right now
    /api/bundle/preview  The contribution bundle, and what sending it would do
    /api/bundle/send   Send it. Refuses whatever the preview calls unsafe.
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
        idle_exit: None,
        shots: None,
        no_shots: false,
        no_archive: false,
        archive_dry_run: false,
        backlog: false,
        adopt_item_ids: None,
        record_out: None,
        logs: Vec::new(),
        bundle_out: None,
        send: false,
        contributor: None,
    };
    let mut saw_eqdb = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--log" => {
                // Kept in both: `log` is what the daemon follows and `logs`
                // is everything that was named. Given twice, a daemon can
                // only follow one and takes the last -- which is what it did
                // before this list existed.
                if let Some(p) = args.next().map(PathBuf::from) {
                    cfg.log = Some(p.clone());
                    cfg.logs.push(p);
                }
            }
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
            // Gone: the reader is in this binary now and needs no tree
            // beside it. Still swallowed, with its argument, so that a saved
            // command line or a desktop launcher carrying it keeps working
            // rather than dying on an unknown flag.
            "--ocr" => {
                let _ = args.next();
                eprintln!(
                    "[eqlparserd] --ocr is no longer used: screenshots are read by this \
                     binary, not by a Python tree beside it"
                );
            }
            "--shots" => cfg.shots = args.next().map(PathBuf::from),
            "--no-shots" => cfg.no_shots = true,
            "--no-archive" => cfg.no_archive = true,
            "--archive-dry-run" => cfg.archive_dry_run = true,
            "--backlog" => cfg.backlog = true,
            "--adopt-item-ids" => cfg.adopt_item_ids = args.next().map(PathBuf::from),
            "--record-out" => cfg.record_out = args.next().map(PathBuf::from),
            "--bundle-out" => cfg.bundle_out = args.next().map(PathBuf::from),
            "--send" => cfg.send = true,
            "--contributor" => cfg.contributor = args.next(),
            "--exit-with-parent" => cfg.exit_with_parent = true,
            "--idle-exit" => match args.next().and_then(|v| v.parse().ok()) {
                Some(secs) => cfg.idle_exit = Some(secs),
                None => {
                    eprintln!("--idle-exit takes a number of seconds");
                    usage()
                }
            },
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
    // The migration, and nothing else. It needs no log, no history database
    // and no port — and a second daemon opening the history database beside
    // the one the desktop app is running is a worse thing to ask of somebody
    // than a flag that does one job and stops.
    if let Some(legacy) = cfg.adopt_item_ids.clone() {
        return adopt_item_ids_once(&cfg, &legacy);
    }

    let log_path = resolve_log(&cfg)?;

    // The other one-shot. Unlike the migration above this needs a log, so it
    // sits after `resolve_log` and before anything opens a database or a port.
    if let Some(dir) = cfg.record_out.clone() {
        return record_out_once(&cfg, &log_path, &dir);
    }

    // The one-shot a contributor runs. Above the identity line below because
    // it reads every log rather than one, and resolves its own.
    if cfg.bundle_out.is_some() || cfg.send {
        return bundle_once(&cfg).await;
    }

    // The port, before anything slow. The initial parse takes the better part
    // of a minute on a long log, and a second daemon started meanwhile used to
    // parse all of it before finding the port taken. Bound here, the second
    // one is refused at once, and a client probing the port meets a listener
    // that answers when the parse is done rather than a refused connection
    // that looks like nobody is there.
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!(
                "[eqlparserd] port {} is already in use, most likely by another eqlparserd; \
                 not starting a second (GET http://{addr}/api/hello says who holds it)",
                cfg.port
            );
            std::process::exit(3);
        }
        Err(e) => return Err(e.into()),
    };

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

    // Item ids, out of the client's own inventory dumps and loot filters.
    //
    // Before the daemon reads a line: the fold is fast, and a record that only
    // updates once somebody plays is a record that is stale exactly when
    // somebody is looking at it after a session.
    let ids = dumps::Ids::open(cfg.game_dir.as_deref(), game).map(|mut ids| {
        let sweep = ids.sweep(&dumps::today());
        report_sweep(&sweep, ids.registry().items.len(), ids.path());
        Arc::new(Mutex::new(ids))
    });
    if ids.is_none() {
        eprintln!(
            "[eqlparserd] item ids: not recorded — no config directory to keep the record in"
        );
    } else if cfg.game_dir.is_none() {
        eprintln!(
            "[eqlparserd] item ids: nothing to read — pass --game-dir to find the client's \
             inventory dumps and loot filters"
        );
    }

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
    let (events, _) = broadcast::channel::<eqlparser_api::Event>(256);

    spawn_follower_since(
        log_path.clone(),
        cfg.since,
        Arc::clone(&engine),
        Arc::clone(&snapshot),
        updates.clone(),
        events.clone(),
        history.clone(),
    );

    let itemdb = std::sync::Arc::new(crate::itemdb::ItemDb::new(cfg.eqdb.clone()));

    // Reading screenshots is the one thing here that needs a second program
    // and a credential, so it is the one thing that is off unless everything
    // it needs is present. Whichever way that goes, it is said out loud: a
    // feature that is silently not running is indistinguishable from one that
    // is running and finding nothing.
    let shots = shots::Shots::new(shots::Setup {
        shots: cfg.shots.clone(),
        game_dir: cfg.game_dir.clone(),
        itemdb: std::sync::Arc::clone(&itemdb),
        off: cfg.no_shots,
        no_archive: cfg.no_archive,
        archive_dry_run: cfg.archive_dry_run,
        backlog: cfg.backlog,
    });
    if let Some(why) = shots.refused_backlog() {
        eprintln!("[eqlparserd] {why}");
    }
    match shots.running() {
        true => eprintln!("[eqlparserd] screenshots: {}", shots.why_running()),
        false => eprintln!("[eqlparserd] screenshots: not read — {}", shots.why()),
    }
    // Spawned whether or not there is a reader right now: a token pasted into
    // the settings box builds one, and something has to be watching for the
    // shots it is then meant to read.
    shots::spawn(Arc::clone(&shots), Arc::clone(&engine));
    if let Some(ids) = &ids {
        spawn_id_sweep(Arc::clone(ids), events.clone());
    }
    // The other characters' logs. Only where there is a history database to
    // put them in -- without one there is nothing for a catch-up to be for.
    match (
        history_path(),
        catchup::folder(cfg.logs_dir.as_deref(), &log_path),
    ) {
        (Some(db), Some(dir)) => {
            eprintln!(
                "[eqlparserd] catching up other logs in {} every {} min",
                dir.display(),
                catchup::SWEEP_INTERVAL.as_secs() / 60
            );
            spawn_log_catchup(dir, log_path.clone(), db, cfg.game_dir.clone(), game);
        }
        (None, _) => eprintln!("[eqlparserd] other logs: no history database to read them into"),
        (_, None) => eprintln!("[eqlparserd] other logs: no folder to look in"),
    }

    let screenshots_dir = cfg
        .shots
        .clone()
        .or_else(|| std::env::var_os("EQL_SHOTS_DIR").map(PathBuf::from))
        .or_else(|| cfg.game_dir.as_ref().map(|g| g.join("Screenshots")))
        .filter(|d| d.is_dir());
    let life = Arc::new(crate::server::Life::new(eqlparser_api::Hello {
        identity: eqlparser_api::IDENTITY.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        api_version: eqlparser_api::API_VERSION,
        pid: std::process::id(),
        port: cfg.port,
        started_at: crate::server::unix_now(),
        game: game.slug().into(),
        log: log_path.to_string_lossy().into_owned(),
        character: if player == "You" { String::new() } else { player.clone() },
        server: server.clone(),
        game_dir: cfg.game_dir.as_ref().map(|d| d.to_string_lossy().into_owned()),
        screenshots_dir: screenshots_dir.map(|d| d.to_string_lossy().into_owned()),
        clients: 0,
        idle_exit_secs: cfg.idle_exit,
        // Filled per request by the handler: it changes with every live line.
        clock_offset: None,
    }));
    if let Some(secs) = cfg.idle_exit {
        crate::server::spawn_idle_exit(Arc::clone(&life), secs);
    }

    let state = AppState {
        life,
        log_path: log_path.clone(),
        logs_dir: cfg.logs_dir.clone(),
        engine,
        snapshot,
        updates,
        events,
        store: history,
        itemdb,
        shots,
        ids: ids.clone(),
    };

    eprintln!("[eqlparserd] listening on http://{addr}");
    eprintln!("[eqlparserd] overlay:  http://{addr}/overlay");

    axum::serve(listener, server::router(state)).await?;
    Ok(())
}

/// Fold a previously accumulated item-id record in, sweep once, and stop.
///
/// One-shot rather than a mode the daemon runs in, because adopting is a
/// migration: it happens once per machine, the record it reads is going away,
/// and running it twice must be — and is — a no-op, since an id already known
/// is left alone.
/// Parse one log and write the record's reports, then stop.
///
/// The proof that moving the collectors into the daemon changed nothing: the
/// same six answers, from the same lines, under the same file names the
/// rebuild has always written. Nothing here opens the history database — this
/// is a reading of one file, not a contribution to the record.
/// Build the bundle from a person's own logs, and write it or send it.
///
/// # Why this exists beside a daemon that already does it
///
/// The panel and the button need the desktop app, and most of the people who
/// contribute to this site have never run it: they send a log file and
/// somebody parses it on the server. That works and it is the wrong shape --
/// a 366 MB upload for facts that compress to 670 KB, and a parse that has to
/// happen on the machine serving the site rather than on the one that has the
/// game installed.
///
/// So this is the same three steps the button performs, with no window: parse
/// what you have, say what sending it would do, send it. `--bundle-out`
/// stops after the second, for somebody who would rather look first or has no
/// token yet.
///
/// # Every log, not the newest one
///
/// The daemon follows one log because it is showing you the fight you are in.
/// A bundle is a different claim -- it says *this is what I know* -- and the
/// record behind it is the union of every character on every server. That
/// distinction is not academic: declaring a source over a record built from
/// one log is how four turn-ins recorded on another character came to be
/// marked for deletion, which is written up in `crate::bundle`'s `OURS`.
///
/// So `--log` may be given more than once, and with none of them a
/// `--logs-dir` means all of it.
///
/// # It writes to the same history every other run uses
///
/// Deliberately, and unlike `--record-out`, which touches no database. The
/// record accumulates: a log rotates, an old one is deleted, and what was
/// parsed out of it stays. A one-shot that built its own throwaway store
/// would send less than the same machine sent last week and call it an
/// update.
async fn bundle_once(cfg: &Config) -> anyhow::Result<()> {
    let logs: Vec<PathBuf> = if !cfg.logs.is_empty() {
        cfg.logs.clone()
    } else if let Some(dir) = &cfg.logs_dir {
        let all = tail::logs_in(dir);
        if all.is_empty() {
            anyhow::bail!("no eqlog_*.txt found in {}", dir.display());
        }
        all
    } else {
        anyhow::bail!(
            "specify --log <file> (more than once for more than one) or \
             --logs-dir <dir> to read all of them"
        );
    };

    let db = history_path().ok_or_else(|| {
        anyhow::anyhow!("no absolute home directory; set HOME, or XDG_DATA_HOME")
    })?;
    let game = cfg.game.unwrap_or_else(|| {
        Game::detect(
            [cfg.game_dir.as_deref(), logs.first().map(|p| p.as_path())]
                .into_iter()
                .flatten(),
        )
    });

    // Parsed one at a time rather than into one engine: the engine is built
    // around a character, and history rows are keyed on character and server
    // so an alt keeps its own. One log, one identity, one flush.
    let mut identity = None;
    for path in &logs {
        let Some((player, server)) =
            Parser::identity_from_filename(&path.to_string_lossy())
        else {
            eprintln!(
                "[eqlparserd] skipping {}: the client names a log \
                 eqlog_<Character>_<server>.txt and nothing inside states either",
                path.display()
            );
            continue;
        };
        let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        eprint!(
            "[eqlparserd] reading {} ({} MB, {player} on {server})… ",
            path.display(),
            bytes / 1_048_576
        );
        let spells = Engine::load_spell_db(cfg.game_dir.as_deref());
        let engine = Arc::new(Mutex::new(Engine::with_game(
            player.clone(),
            spells,
            game,
        )));
        let file = std::fs::File::open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let started = std::time::Instant::now();
        let mut lines = 0u64;
        {
            use std::io::BufRead;
            let mut e = engine.locked("parse");
            for line in std::io::BufReader::new(file).lines() {
                e.push_line(&line?);
                lines += 1;
            }
        }
        let store = Arc::new(Mutex::new(
            store::Store::open_as(&db, &player, &server)
                .with_context(|| format!("opening {}", db.display()))?,
        ));
        flush_history(&engine, &store, path);
        eprintln!("{lines} lines in {:.1}s", started.elapsed().as_secs_f64());
        identity = Some((player, server));
    }
    let Some((player, server)) = identity else {
        anyhow::bail!("none of the logs given could be attributed to a character");
    };

    // The client's own files, on the same sweep the daemon runs every minute.
    // They are where item ids come from, and they are not in any log.
    let ids = dumps::Ids::open(cfg.game_dir.as_deref(), game).map(|mut ids| {
        let sweep = ids.sweep(&dumps::today());
        report_sweep(&sweep, ids.registry().items.len(), ids.path());
        ids
    });
    if ids.is_none() {
        eprintln!("[eqlparserd] item ids: no config directory to keep the record in");
    } else if cfg.game_dir.is_none() {
        eprintln!("[eqlparserd] item ids: no --game-dir, so no dumps were read this run");
    }

    let who = cfg
        .contributor
        .clone()
        .unwrap_or_else(crate::server::contributor);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let store = store::Store::open_as(&db, &player, &server)?;
    let b = crate::bundle::build(&store, ids.as_ref().map(|i| i.registry()), &who, now)?;
    let p = crate::bundle::preview(&b);
    print_verdicts(&p);

    if let Some(out) = &cfg.bundle_out {
        // Compact, not pretty. This file is meant to *be* the upload -- sent
        // by hand, or mailed to somebody who will send it -- and the site
        // files a bundle under the digest of the bytes it received. Pretty
        // printing would give the same document a different id from the one
        // `--send` produces, which is a difference nobody would look for.
        let json = serde_json::to_vec(&b)?;
        std::fs::write(out, &json).with_context(|| format!("writing {}", out.display()))?;
        eprintln!(
            "\n[eqlparserd] wrote {} ({} KB of JSON)",
            out.display(),
            json.len().div_ceil(1024)
        );
    }
    if !cfg.send {
        if cfg.bundle_out.is_none() {
            eprintln!("\n[eqlparserd] nothing written and nothing sent — pass --bundle-out <file> or --send");
        }
        return Ok(());
    }

    // The same refusal the endpoint makes, from the same list, so a bundle
    // this says is unsafe cannot be sent by going round the window.
    if !p.blocked.is_empty() {
        for why in &p.blocked {
            eprintln!("[eqlparserd] {why}");
        }
        anyhow::bail!("this bundle is not safe to send, so it was not sent");
    }
    let site = crate::site::Site::new(
        cfg.eqdb
            .first()
            .cloned()
            .unwrap_or_else(|| DEFAULT_EQDB[0].to_string()),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let json = serde_json::to_vec(&b)?;
    let body = crate::site::gzip(&json).map_err(|e| anyhow::anyhow!("{e}"))?;
    let sent_bytes = body.len();
    let from: String = b
        .contributor
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' ' | '\''))
        .take(60)
        .collect();
    eprintln!(
        "\n[eqlparserd] sending {} rows to {} ({} KB gzipped)…",
        p.rows,
        site.base(),
        sent_bytes.div_ceil(1024)
    );
    let reply = site
        .post(
            "/api/bundle",
            reqwest::Body::from(body),
            "application/octet-stream",
            &[("X-Contributor", from.trim())],
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!(
        "[eqlparserd] filed{}. It is not ingested: somebody runs \
         `deploy/worker.sh --bundles` on the server next.",
        reply
            .get("bundle_id")
            .and_then(|v| v.as_str())
            .map(|id| format!(" as {id}"))
            .unwrap_or_default()
    );
    Ok(())
}

/// The panel's four groups, as text.
///
/// Ordered by consequence rather than by table name, and the deletions first,
/// for the reason `crate::bundle` gives: a table carrying nothing that the
/// bundle has claimed is a deletion, and a tidy zero in an alphabetical list
/// has technically shown that and in practice hidden it.
fn print_verdicts(p: &crate::bundle::Preview) {
    use crate::bundle::Verdict;
    eprintln!();
    eprintln!(
        "  contributor  {}",
        if p.contributor.is_empty() {
            "— not set —"
        } else {
            &p.contributor
        }
    );
    eprintln!("  game         {}", p.game);
    eprintln!(
        "  document     {} rows, {} KB of JSON, {} KB gzipped",
        p.rows,
        p.bytes.div_ceil(1024),
        p.gzip_bytes.div_ceil(1024)
    );
    eprintln!(
        "  claims       {}",
        if p.sources.is_empty() {
            "nothing — every table is added to rather than replaced".to_string()
        } else {
            p.sources.join(", ")
        }
    );
    for (verdict, title) in [
        (Verdict::Deletes, "WOULD DELETE"),
        (Verdict::Replaces, "replaces"),
        (Verdict::Merges, "merges"),
    ] {
        let rows: Vec<&crate::bundle::TableView> =
            p.tables.iter().filter(|t| t.verdict == verdict).collect();
        if rows.is_empty() {
            continue;
        }
        eprintln!("\n  {title}");
        for t in rows {
            eprintln!("    {:<22} {:>8}", t.table, t.rows);
        }
    }
    let absent = p
        .tables
        .iter()
        .filter(|t| t.verdict == Verdict::Absent)
        .count();
    eprintln!("\n  {absent} table(s) left alone: nothing here claims them.");
    for why in &p.blocked {
        eprintln!("\n  NOT SAFE TO SEND: {why}");
    }
}

fn record_out_once(cfg: &Config, log_path: &Path, dir: &Path) -> anyhow::Result<()> {
    use std::io::BufRead;

    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let (player, _server) = Parser::identity_from_filename(&log_path.to_string_lossy())
        .with_context(|| {
            format!(
                "cannot tell whose log {} is. The client names a log \
                 eqlog_<Character>_<server>.txt and nothing inside it states \
                 either.",
                log_path.display()
            )
        })?;
    let game = cfg.game.unwrap_or_else(|| {
        Game::detect(
            [cfg.game_dir.as_deref(), Some(log_path)]
                .into_iter()
                .flatten(),
        )
    });
    // The spell database matters here even though none of the six collectors
    // reads one: class detection does, and the Sky report is gated on which
    // classes you have played. Skipping it would make this run answer a
    // different question from the daemon's.
    let spells = Engine::load_spell_db(cfg.game_dir.as_deref());
    let mut engine = Engine::with_game(player, spells, game);
    let file = std::fs::File::open(log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let start = std::time::Instant::now();
    let mut lines = 0u64;
    for line in std::io::BufReader::new(file).lines() {
        engine.push_line(&line?);
        lines += 1;
    }
    // The tag is the log's stem, as it was when a script fetched these off
    // this daemon's API.
    let tag = log_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let reports = engine.record_reports(&tag);
    // Straight from the struct, never through `serde_json::Value`. A `Value`
    // is a sorted map here, so a round trip through one alphabetises every
    // object and the file stops matching the one the rebuild wrote — same
    // bytes, different order, and a diff that is all noise.
    for (what, json) in [
        ("players", serde_json::to_string_pretty(&reports.players)?),
        ("consider", serde_json::to_string_pretty(&reports.consider)?),
        ("faction", serde_json::to_string_pretty(&reports.faction)?),
        ("spots", serde_json::to_string_pretty(&reports.spots)?),
        ("carto", serde_json::to_string_pretty(&reports.carto)?),
        // The other three the rebuild writes per log. These come off the
        // engine's own trackers rather than the record's collectors — it has
        // always had them — and are written here so the same one command
        // covers everything a log gives up.
        ("sky", serde_json::to_string_pretty(&engine.sky_report())?),
        ("tasks", serde_json::to_string_pretty(&engine.task_report())?),
        ("quests", serde_json::to_string_pretty(&engine.task_report())?),
        (
            "tradeskills",
            serde_json::to_string_pretty(&engine.tradeskill_report())?,
        ),
    ] {
        let p = dir.join(format!("{what}.{tag}.json"));
        std::fs::write(&p, json + "\n")
            .with_context(|| format!("writing {}", p.display()))?;
    }
    // A ceiling is the highest anybody has reached, so it has no log's stem
    // in its name and is folded into whatever is already there — the same
    // rule, and for the same reason, as the rebuild that wrote it before.
    let skills_path = dir.join("skills.json");
    let mut best: std::collections::BTreeMap<String, u32> = reports
        .skills
        .skills
        .iter()
        .map(|s| (s.skill.clone(), s.at_least))
        .collect();
    if let Ok(text) = std::fs::read_to_string(&skills_path) {
        if let Ok(old) = serde_json::from_str::<serde_json::Value>(&text) {
            for row in old
                .get("skills")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                let (Some(name), Some(v)) = (
                    row.get("skill").and_then(|v| v.as_str()),
                    row.get("at_least").and_then(|v| v.as_u64()),
                ) else {
                    continue;
                };
                let e = best.entry(name.to_string()).or_insert(0);
                *e = (*e).max(v as u32);
            }
        }
    }
    let skills = record::SkillsReport {
        note: record::SKILLS_NOTE,
        source: "logs",
        game: record::GAME_TAG,
        skills: best
            .into_iter()
            .map(|(skill, at_least)| record::SkillRow {
                skill,
                at_least,
                game: record::GAME_TAG,
            })
            .collect(),
    };
    std::fs::write(
        &skills_path,
        serde_json::to_string_pretty(&skills)? + "\n",
    )
    .with_context(|| format!("writing {}", skills_path.display()))?;

    eprintln!(
        "[eqlparserd] record: {tag} — {lines} lines in {:.2}s; {} consider(s), \
         {} faction hit(s), {} turn-in(s), {} player(s), {} marker(s), {} spot(s), \
         {} skill(s) into {}",
        start.elapsed().as_secs_f64(),
        reports.consider.considers.len(),
        reports.faction.hits.len(),
        reports.faction.turnins.len(),
        reports.players.players.len(),
        reports.carto.markers.len(),
        reports.spots.spots.len(),
        skills.skills.len(),
        dir.display()
    );
    Ok(())
}

fn adopt_item_ids_once(cfg: &Config, legacy: &Path) -> anyhow::Result<()> {
    let game = cfg.game.unwrap_or_else(|| {
        Game::detect(
            [cfg.game_dir.as_deref(), cfg.log.as_deref()]
                .into_iter()
                .flatten(),
        )
    });
    let Some(mut ids) = dumps::Ids::open(cfg.game_dir.as_deref(), game) else {
        anyhow::bail!(
            "no config directory to keep an item-id record in; set HOME or XDG_CONFIG_HOME"
        );
    };
    let (taken, corrected) = ids
        .adopt(legacy)
        .with_context(|| format!("adopting {}", legacy.display()))?;
    eprintln!(
        "[eqlparserd] item ids: adopted {taken} id(s) from {} — {corrected} of them had grades \
         a loot filter saw on a corpse filed as grades somebody held, and now say seen",
        legacy.display()
    );
    if cfg.game_dir.is_none() {
        eprintln!(
            "[eqlparserd] item ids: nothing swept — pass --game-dir to fold the client's \
             current dumps and loot filters on top"
        );
    }
    let sweep = ids.sweep(&dumps::today());
    report_sweep(&sweep, ids.registry().items.len(), ids.path());
    Ok(())
}

/// Say what a sweep of the game folder found.
///
/// Always, including when it found nothing new: a fold that quietly does
/// nothing and a fold that is not running look identical from outside, and
/// this record is one nobody checks until an id is missing from it.
fn report_sweep(sweep: &dumps::Sweep, held: usize, path: &Path) {
    if sweep.changed_anything() {
        eprintln!(
            "[eqlparserd] item ids: folded {} — {} new, {} updated, {held} on file in {}",
            sweep.folded.join(", "),
            sweep.added,
            sweep.updated,
            path.display()
        );
    } else {
        eprintln!(
            "[eqlparserd] item ids: {held} on file, {} source(s) already folded",
            sweep.looked_at
        );
    }
    for note in &sweep.notes {
        eprintln!("[eqlparserd] item ids: {note}");
    }
    for bad in &sweep.failures {
        eprintln!("[eqlparserd] item ids: {bad}");
    }
}

/// Re-examine the game folder while the daemon runs.
///
/// A player types `/outputfile inventory` mid-session and expects the record
/// to know about it without restarting anything. Quiet unless something was
/// actually folded — see `report_sweep` for why the startup pass is not.
/// Catch the logs the daemon is not following up, on a timer.
///
/// See [`crate::catchup`] for why this is a sweep rather than a tailer per
/// log. Runs once at startup after a short delay and then on the interval:
/// the delay is so a daemon that has just started does not compete with its
/// own initial parse of the followed log, which is the expensive one.
///
/// Failures are printed and the loop continues. A log that cannot be read is
/// one log, and taking the sweep down with it would silently stop the others.
fn spawn_log_catchup(
    dir: PathBuf,
    followed: PathBuf,
    db: PathBuf,
    game_dir: Option<PathBuf>,
    game: Game,
) {
    tokio::spawn(async move {
        tokio::time::sleep(catchup::FIRST_SWEEP_AFTER).await;
        loop {
            let (dir, followed, db, game_dir) =
                (dir.clone(), followed.clone(), db.clone(), game_dir.clone());
            // On a blocking thread: this parses a file, which is CPU-bound and
            // synchronous, and a long catch-up on the async runtime would stall
            // every request the daemon is serving.
            let caught = tokio::task::spawn_blocking(move || {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                // The watermarks are read once, through a store of their own,
                // rather than borrowing the daemon's -- that one is locked by
                // the follower for the length of every batch it ingests.
                let marks = catchup::watermarks(&db);
                catchup::sweep(
                    &dir,
                    &followed,
                    &db,
                    game_dir.as_deref(),
                    game,
                    &|p| marks.get(p).copied().unwrap_or(0),
                    now,
                )
            })
            .await
            .unwrap_or_default();
            for c in &caught {
                eprintln!(
                    "[eqlparserd] caught up {} ({} on {}): {} line(s), {} MB, {:.1}s",
                    c.path,
                    c.player,
                    c.server,
                    c.lines,
                    (c.to - c.from) / 1_048_576,
                    c.seconds,
                );
            }
            tokio::time::sleep(catchup::SWEEP_INTERVAL).await;
        }
    });
}

fn spawn_id_sweep(ids: Arc<Mutex<dumps::Ids>>, events: broadcast::Sender<eqlparser_api::Event>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(dumps::SWEEP_INTERVAL).await;
            let mut guard = ids.locked("item ids");
            let sweep = guard.sweep(&dumps::today());
            if sweep.changed_anything() || !sweep.failures.is_empty() {
                report_sweep(&sweep, guard.registry().items.len(), guard.path());
            }
            if sweep.changed_anything() {
                let _ = events.send(eqlparser_api::Event::BagsChanged);
            }
        }
    });
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
pub(crate) fn flush_history(engine: &Arc<Mutex<Engine>>, store: &Arc<Mutex<store::Store>>, path: &Path) {
    let (kills, encs, levels, aa) = { engine.locked("engine").history_to_persist() };
    let sky_turn_ins = { engine.locked("engine").sky_turn_ins() };
    let grants = { engine.locked("engine").grants_to_persist() };
    let (drops, prices, game, bestiary, health, record) = {
        let e = engine.locked("engine");
        (
            e.drop_report(),
            e.tradeskill_report().prices,
            e.game(),
            e.bestiary_report(),
            e.bestiary_health_samples(),
            // Settled but not disturbed: a faction burst straddling this
            // flush stays open in the live collector. See `crate::record`.
            e.record(),
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
        .and_then(|_| st.put_record(record::GAME_TAG, &source, &record))
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
    events: broadcast::Sender<eqlparser_api::Event>,
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
        publish(&engine, &snapshot, &updates, &events);
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
            publish(&engine, &snapshot, &updates, &events);

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
    events: &broadcast::Sender<eqlparser_api::Event>,
) {
    let (snap, news) = {
        let mut eng = engine.locked("engine");
        (eng.snapshot(SNAPSHOT_RECENT), eng.take_events())
    };
    *snapshot.writing("snapshot") = snap;
    // No receivers is normal (nothing connected yet), so ignore send errors.
    let _ = updates.send(());
    for e in news {
        let _ = events.send(e);
    }
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
            idle_exit: None,
            shots: None,
            no_shots: false,
            no_archive: false,
            archive_dry_run: false,
            backlog: false,
            adopt_item_ids: None,
            record_out: None,
            logs: Vec::new(),
            bundle_out: None,
            send: false,
            contributor: None,
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
            idle_exit: None,
            shots: None,
            no_shots: false,
            no_archive: false,
            archive_dry_run: false,
            backlog: false,
            adopt_item_ids: None,
            record_out: None,
            logs: Vec::new(),
            bundle_out: None,
            send: false,
            contributor: None,
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
