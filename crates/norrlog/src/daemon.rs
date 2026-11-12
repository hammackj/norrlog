//! The daemon thread: attach to the shared eqlparserd (starting it if nobody
//! has), hold its snapshot stream, and say what happens to the window.
//!
//! Reconnects as the Electron renderer does: after `min(500 ms × n, 5 s)`,
//! `n` capped at 10 and reset on a good attach. Quitting never stops the
//! daemon; one this app started exits a minute after its last client.
//!
//! Settings can ask for a restart with a new [`Setup`]: the daemon on the
//! old port is stopped, whoever started it, as `restartDaemon` in
//! `gui/main.js` does, and the new one is attached to or started.

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
    Attached { hello: Box<Hello>, started: bool, client: Box<Client> },
    Snapshot(Arc<serde_json::Value>),
    Lost(String),
}

/// Long enough that closing one app and opening another within a minute
/// does not reparse the whole log; the same as Electron and the collector.
const IDLE_EXIT: Duration = Duration::from_secs(60);

/// Starts the daemon thread and its runtime; the handle is for views'
/// requests (see `api`), which run on the same runtime. A [`Setup`] sent on
/// `ctl` restarts the daemon with it.
pub fn spawn(setup: Setup, tx: async_channel::Sender<Msg>, ctl: async_channel::Receiver<Setup>) -> tokio::runtime::Handle {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("a tokio runtime");
    let handle = rt.handle().clone();
    std::thread::Builder::new()
        .name("daemon".into())
        .spawn(move || rt.block_on(run(setup, tx, ctl)))
        .expect("the daemon thread");
    handle
}

/// How a session ended.
enum End {
    /// The window has gone.
    Closed,
    Lost(String),
    /// Settings asked for a restart with this.
    Restart(Setup),
}

async fn run(mut setup: Setup, tx: async_channel::Sender<Msg>, ctl: async_channel::Receiver<Setup>) {
    let mut retry: u64 = 0;
    loop {
        if tx.send(Msg::Attaching).await.is_err() {
            return;
        }
        let next = match session(&setup, &tx, &ctl, &mut retry).await {
            End::Closed => return,
            End::Restart(next) => Some(next),
            End::Lost(why) => {
                if tx.send(Msg::Lost(why)).await.is_err() {
                    return;
                }
                retry = (retry + 1).min(10);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis((500 * retry).min(5000))) => None,
                    Ok(next) = ctl.recv() => Some(next),
                }
            }
        };
        if let Some(next) = next {
            stop(setup.port).await;
            setup = next;
            retry = 0;
        }
    }
}

/// One attachment.
async fn session(
    setup: &Setup,
    tx: &async_channel::Sender<Msg>,
    ctl: &async_channel::Receiver<Setup>,
    retry: &mut u64,
) -> End {
    let mut spawn = Spawn::new(&setup.daemon);
    spawn.args = setup.args.clone();
    spawn.idle_exit = IDLE_EXIT;
    let client = tokio::select! {
        got = Client::attach_or_spawn(setup.port, &spawn) => match got {
            Ok(c) => c,
            Err(e) => return End::Lost(describe(&e, setup)),
        },
        Ok(next) = ctl.recv() => return End::Restart(next),
    };
    let hello = Box::new(client.hello().clone());
    let started = client.started_daemon();
    if tx.send(Msg::Attached { hello, started, client: Box::new(client.clone()) }).await.is_err() {
        return End::Closed;
    }
    let mut snaps = match client.snapshots().await {
        Ok(s) => s,
        Err(e) => return End::Lost(describe(&e, setup)),
    };
    *retry = 0;
    loop {
        let got = tokio::select! {
            got = snaps.next() => got,
            Ok(next) = ctl.recv() => return End::Restart(next),
        };
        match got {
            // A snapshot is complete in itself: when the window is behind,
            // the oldest waiting one is the one to drop.
            Some(Ok(v)) => {
                if tx.force_send(Msg::Snapshot(Arc::new(v))).is_err() {
                    return End::Closed;
                }
            }
            Some(Err(Error::Decode(e))) => eprintln!("[norrlog] a snapshot did not decode: {e}"),
            Some(Err(e)) => return End::Lost(describe(&e, setup)),
            None => return End::Lost("the daemon closed the connection".into()),
        }
    }
}

/// Stop the daemon on `port`, if one answers, and wait up to five seconds
/// for the port to come free. Whoever started it: every app attached to it
/// reconnects to the new one, as they do after Electron's restart.
async fn stop(port: u16) {
    let Ok(old) = Client::attach(port).await else { return };
    terminate(old.hello().pid).await;
    for _ in 0..50 {
        if matches!(Client::attach(port).await, Err(Error::Unreachable { .. })) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    eprintln!("[norrlog] the daemon on port {port} did not stop within five seconds");
}

async fn terminate(pid: u32) {
    #[cfg(unix)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("kill");
        c.arg("-TERM").arg(pid.to_string());
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("taskkill");
        c.arg("/PID").arg(pid.to_string());
        c
    };
    if let Err(e) = cmd.stdin(std::process::Stdio::null()).status().await {
        eprintln!("[norrlog] could not stop pid {pid}: {e}");
    }
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
