//! The daemon thread: attach to the shared eqlparserd (starting it if nobody
//! has), hold its snapshot stream, and say what happens to the window.
//!
//! Reconnects as the Electron renderer does: after `min(500 ms × n, 5 s)`,
//! `n` capped at 10 and reset on a good attach. Quitting never stops the
//! daemon; one this app started exits a minute after its last client.

use eqlparser_api::Hello;
use eqlparser_client::{Client, Error, Spawn};
use futures_util::StreamExt;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Setup {
    pub port: u16,
    pub daemon: PathBuf,
    pub args: Vec<OsString>,
}

pub enum Msg {
    Attaching,
    Attached { hello: Box<Hello>, started: bool },
    Snapshot(Arc<serde_json::Value>),
    Lost(String),
}

/// Long enough that closing one app and opening another within a minute
/// does not reparse the whole log; the same as Electron and the collector.
const IDLE_EXIT: Duration = Duration::from_secs(60);

pub fn spawn(setup: Setup, tx: async_channel::Sender<Msg>) {
    std::thread::Builder::new()
        .name("daemon".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("a tokio runtime");
            rt.block_on(run(setup, tx));
        })
        .expect("the daemon thread");
}

async fn run(setup: Setup, tx: async_channel::Sender<Msg>) {
    let mut spawn = Spawn::new(&setup.daemon);
    spawn.args = setup.args.clone();
    spawn.idle_exit = IDLE_EXIT;
    let mut retry: u64 = 0;
    loop {
        if tx.send(Msg::Attaching).await.is_err() {
            return;
        }
        let why = match session(&setup, &spawn, &tx, &mut retry).await {
            Ok(()) => return, // the window has gone
            Err(why) => why,
        };
        if tx.send(Msg::Lost(why)).await.is_err() {
            return;
        }
        retry = (retry + 1).min(10);
        tokio::time::sleep(Duration::from_millis((500 * retry).min(5000))).await;
    }
}

/// One attachment. `Ok` only when the window has closed.
async fn session(setup: &Setup, spawn: &Spawn, tx: &async_channel::Sender<Msg>, retry: &mut u64) -> Result<(), String> {
    let client = Client::attach_or_spawn(setup.port, spawn).await.map_err(|e| describe(&e, setup))?;
    let hello = Box::new(client.hello().clone());
    if tx.send(Msg::Attached { hello, started: client.started_daemon() }).await.is_err() {
        return Ok(());
    }
    let mut snaps = client.snapshots().await.map_err(|e| describe(&e, setup))?;
    *retry = 0;
    while let Some(got) = snaps.next().await {
        match got {
            // A snapshot is complete in itself: when the window is behind,
            // the oldest waiting one is the one to drop.
            Ok(v) => {
                if tx.force_send(Msg::Snapshot(Arc::new(v))).is_err() {
                    return Ok(());
                }
            }
            Err(Error::Decode(e)) => eprintln!("[norrlog] a snapshot did not decode: {e}"),
            Err(e) => return Err(describe(&e, setup)),
        }
    }
    Err("the daemon closed the connection".into())
}

fn describe(e: &Error, setup: &Setup) -> String {
    match e {
        Error::Spawn(io) => format!("could not start {}: {io}", setup.daemon.display()),
        other => other.to_string(),
    }
}

/// Where `eqlparserd` is: `--daemon`, `$EQLPARSER_DAEMON`, beside this
/// executable, this checkout's `target/`, then whatever is on `PATH`.
pub fn find_daemon(given: Option<PathBuf>) -> PathBuf {
    let exe = if cfg!(windows) { "eqlparserd.exe" } else { "eqlparserd" };
    if let Some(p) = given.or_else(|| std::env::var_os("EQLPARSER_DAEMON").map(PathBuf::from)) {
        return p;
    }
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join(exe));
    }
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    candidates.push(target.join("release").join(exe));
    candidates.push(target.join("debug").join(exe));
    candidates.into_iter().find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from(exe))
}
