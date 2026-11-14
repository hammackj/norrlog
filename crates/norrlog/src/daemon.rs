//! The daemon thread: attach to the shared norrlogd (starting it if nobody
//! has), hold its snapshot stream, and say what happens to the window.
//!
//! Reconnects after `min(500 ms × n, 5 s)`,
//! `n` capped at 10 and reset on a good attach. Quitting never stops the
//! daemon; one this app started exits a minute after its last client.
//!
//! Settings can ask for a restart with a new [`Setup`]: the daemon on the
//! old port is stopped, whoever started it, and the new one is attached to
//! or started.

use futures_util::StreamExt;
use norrlog_api::Hello;
use norrlog_client::{Client, Error, Spawn};
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
    Attached {
        hello: Box<Hello>,
        started: bool,
        client: Box<Client>,
    },
    Snapshot(Arc<serde_json::Value>),
    Lost(String),
    /// No log is chosen and no daemon is running to attach to. Nothing is
    /// started until Settings sends a setup that names one.
    Unset,
}

/// What travels on the channel. Snapshots do not: they wait in [`Latest`],
/// and the channel carries at most one notice that one is waiting.
enum Wire {
    /// A lifecycle message, and what it does to which attachment's
    /// snapshots the window takes.
    Life(Msg, Generation),
    SnapshotWaiting,
}

enum Generation {
    Keep,
    /// Attached: from now on, this attachment's.
    Set(u64),
    /// Lost: nobody's.
    Clear,
}

/// The newest snapshot, and the attachment it came from.
///
/// Snapshots used to share the bounded channel with the lifecycle messages
/// and push out the oldest waiting message when it was full, on the
/// assumption that the oldest was an old snapshot. A window busy for a few
/// seconds after attaching lost `Attached`, the one message carrying the
/// client, and then showed snapshots from a daemon none of its views could
/// ask anything of. Now a snapshot replaces the last one here, never a
/// message, and memory stays at one snapshot however far behind the window
/// falls.
#[derive(Default)]
struct Latest {
    slot: std::sync::Mutex<Option<(u64, Arc<serde_json::Value>)>>,
    /// A notice is on the channel and not yet taken.
    noticed: std::sync::atomic::AtomicBool,
}

impl Latest {
    /// Keep `v` as the newest; true when the window needs telling.
    fn put(&self, generation: u64, v: Arc<serde_json::Value>) -> bool {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, v));
        !self.noticed.swap(true, std::sync::atomic::Ordering::SeqCst)
    }

    fn take(&self) -> Option<(u64, Arc<serde_json::Value>)> {
        // Cleared first, so a snapshot put after the take sends a notice.
        self.noticed
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn clear(&self) {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// The daemon thread's end.
#[derive(Clone)]
pub struct Outbox {
    tx: async_channel::Sender<Wire>,
    latest: Arc<Latest>,
}

/// The window's end. Hands out messages in order, snapshots only from the
/// attachment the window last heard about.
pub struct Inbox {
    rx: async_channel::Receiver<Wire>,
    latest: Arc<Latest>,
    generation: Option<u64>,
    /// A snapshot from an attachment announced after its notice was read:
    /// handed over straight after that `Attached`.
    early: Option<(u64, Arc<serde_json::Value>)>,
    after: Option<Msg>,
}

pub fn mailbox() -> (Outbox, Inbox) {
    // Lifecycle messages are few, and at most one snapshot notice is ever
    // waiting, so this never fills in practice; when it does the sender waits.
    let (tx, rx) = async_channel::bounded(16);
    let latest = Arc::new(Latest::default());
    (
        Outbox {
            tx,
            latest: latest.clone(),
        },
        Inbox {
            rx,
            latest,
            generation: None,
            early: None,
            after: None,
        },
    )
}

impl Outbox {
    async fn send(&self, w: Wire) -> Result<(), ()> {
        self.tx.send(w).await.map_err(|_| ())
    }

    async fn attached(
        &self,
        hello: Box<Hello>,
        started: bool,
        client: Box<Client>,
        generation: u64,
    ) -> Result<(), ()> {
        // A snapshot from the attachment before is not this one's.
        self.latest.clear();
        self.send(Wire::Life(
            Msg::Attached {
                hello,
                started,
                client,
            },
            Generation::Set(generation),
        ))
        .await
    }

    async fn snapshot(&self, generation: u64, v: Arc<serde_json::Value>) -> Result<(), ()> {
        if self.latest.put(generation, v) {
            self.send(Wire::SnapshotWaiting).await?;
        }
        Ok(())
    }
}

impl Inbox {
    /// The next message, or `None` once the daemon thread has gone.
    pub async fn recv(&mut self) -> Option<Msg> {
        if let Some(m) = self.after.take() {
            return Some(m);
        }
        loop {
            return Some(match self.rx.recv().await.ok()? {
                Wire::Life(msg, g) => {
                    match g {
                        Generation::Keep => {}
                        Generation::Set(n) => {
                            self.generation = Some(n);
                            if let Some((e, v)) = self.early.take()
                                && e == n
                            {
                                self.after = Some(Msg::Snapshot(v));
                            }
                        }
                        Generation::Clear => self.generation = None,
                    }
                    msg
                }
                Wire::SnapshotWaiting => match self.latest.take() {
                    Some((g, v)) if Some(g) == self.generation => Msg::Snapshot(v),
                    // From an attachment not announced yet: kept for when it
                    // is, since its notice has been used up.
                    Some((g, v)) if self.generation.is_none_or(|now| g > now) => {
                        self.early = Some((g, v));
                        continue;
                    }
                    // From one already over.
                    _ => continue,
                },
            });
        }
    }
}

/// Long enough that closing one app and opening another within a minute
/// does not reparse the whole log; the same as the collector.
const IDLE_EXIT: Duration = Duration::from_secs(60);

/// Starts the daemon thread and its runtime; the handle is for views'
/// requests (see `api`), which run on the same runtime. A [`Setup`] sent on
/// `ctl` restarts the daemon with it.
pub fn spawn(
    setup: Setup,
    tx: Outbox,
    ctl: async_channel::Receiver<Setup>,
) -> tokio::runtime::Handle {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a tokio runtime");
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

async fn run(mut setup: Setup, tx: Outbox, ctl: async_channel::Receiver<Setup>) {
    let mut retry: u64 = 0;
    let mut generation: u64 = 0;
    loop {
        if tx
            .send(Wire::Life(Msg::Attaching, Generation::Keep))
            .await
            .is_err()
        {
            return;
        }
        generation += 1;
        let next = match session(&setup, &tx, &ctl, &mut retry, generation).await {
            End::Closed => return,
            End::Restart(next) => Some(next),
            End::Lost(why) => {
                if tx
                    .send(Wire::Life(Msg::Lost(why), Generation::Clear))
                    .await
                    .is_err()
                {
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
    tx: &Outbox,
    ctl: &async_channel::Receiver<Setup>,
    retry: &mut u64,
    generation: u64,
) -> End {
    // With no log chosen the daemon refuses to start, and retrying it every
    // few seconds read to a new user as a broken install. A daemon already
    // running is still worth attaching to; otherwise wait for Settings.
    if !names_a_log(&setup.args) {
        let attached = tokio::select! {
            got = Client::attach(setup.port) => got.ok(),
            Ok(next) = ctl.recv() => return End::Restart(next),
        };
        if attached.is_none() {
            if tx
                .send(Wire::Life(Msg::Unset, Generation::Clear))
                .await
                .is_err()
            {
                return End::Closed;
            }
            return match ctl.recv().await {
                Ok(next) => End::Restart(next),
                Err(_) => End::Closed,
            };
        }
    }
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
    if tx
        .attached(hello, started, Box::new(client.clone()), generation)
        .await
        .is_err()
    {
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
            // it gets the newest and the rest are dropped.
            Some(Ok(v)) => {
                if tx.snapshot(generation, Arc::new(v)).await.is_err() {
                    return End::Closed;
                }
            }
            Some(Err(Error::Decode(e))) => eprintln!("[norrlog] a snapshot did not decode: {e}"),
            Some(Err(e)) => return End::Lost(describe(&e, setup)),
            None => return End::Lost("the daemon closed the connection".into()),
        }
    }
}

/// Whether daemon arguments tell it which log to read.
fn names_a_log(args: &[OsString]) -> bool {
    args.iter().any(|a| a == "--log" || a == "--logs-dir")
}

/// Stop the daemon on `port`, if one answers, and wait up to five seconds
/// for the port to come free. Whoever started it: every app attached to it
/// reconnects to the new one.
async fn stop(port: u16) {
    let pid = match Client::attach(port).await {
        Ok(old) => old.hello().pid,
        // Still on its initial parse, and not answering until it is done:
        // waiting for it would follow the old log for minutes, then attach
        // to it as if nothing had been asked. It recorded its pid on binding.
        Err(Error::Busy { .. }) => match norrlog_api::pidfile::holder(port) {
            Some(pid) => pid,
            None => {
                eprintln!("[norrlog] the daemon on port {port} is busy and its pid is not known");
                return;
            }
        },
        Err(_) => return,
    };
    terminate(pid).await;
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
        // Forced: a process with no window never gets the close message a
        // plain taskkill sends, and refuses it. The daemon writes history as
        // it goes and has no shutdown step, so this is what SIGTERM is on
        // Unix. No console flashing up over the game while it runs.
        let mut c = tokio::process::Command::new("taskkill");
        c.arg("/F").arg("/PID").arg(pid.to_string());
        c.creation_flags(0x0800_0000);
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

/// Where `norrlogd` is: `--daemon`, `$NORRLOG_DAEMON`, beside this
/// executable, this checkout's `target/`, then whatever is on `PATH`.
pub fn find_daemon(given: Option<PathBuf>) -> PathBuf {
    let exe = if cfg!(windows) {
        "norrlogd.exe"
    } else {
        "norrlogd"
    };
    if let Some(p) = given.or_else(|| {
        std::env::var_os("NORRLOG_DAEMON")
            .or_else(|| std::env::var_os("EQLPARSER_DAEMON"))
            .map(PathBuf::from)
    }) {
        return p;
    }
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join(exe));
    }
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    candidates.push(target.join("release").join(exe));
    candidates.push(target.join("debug").join(exe));
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(exe))
}

#[cfg(test)]
mod mailbox_tests {
    use super::*;

    /// What the window was handed, in a form a test can compare.
    async fn drain(inbox: &mut Inbox) -> Vec<String> {
        let mut got = Vec::new();
        while let Ok(Some(m)) = tokio::time::timeout(Duration::from_millis(50), inbox.recv()).await
        {
            got.push(match m {
                Msg::Attaching => "attaching".to_string(),
                Msg::Attached { .. } => "attached".to_string(),
                Msg::Lost(_) => "lost".to_string(),
                Msg::Unset => "unset".to_string(),
                Msg::Snapshot(v) => v.to_string(),
            });
        }
        got
    }

    /// The lifecycle messages go as they do, with `Attaching` standing in for
    /// `Attached`, whose client needs a daemon to make.
    async fn attach(out: &Outbox, generation: u64) {
        out.latest.clear();
        out.send(Wire::Life(Msg::Attaching, Generation::Set(generation)))
            .await
            .unwrap();
    }

    #[test]
    fn a_window_far_behind_hears_every_lifecycle_message_and_the_newest_snapshot() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (out, mut inbox) = mailbox();
            // The window is stalled: nothing is read until all of this is sent.
            attach(&out, 1).await;
            for i in 0..40 {
                out.snapshot(1, Arc::new(serde_json::json!(i)))
                    .await
                    .unwrap();
            }
            assert_eq!(
                drain(&mut inbox).await,
                ["attaching", "39"],
                "the attach was pushed out, or not the newest"
            );

            out.snapshot(1, Arc::new(serde_json::json!(1000)))
                .await
                .unwrap();
            out.send(Wire::Life(Msg::Lost("gone".into()), Generation::Clear))
                .await
                .unwrap();
            for i in 0..40 {
                out.snapshot(1, Arc::new(serde_json::json!(i)))
                    .await
                    .unwrap();
            }
            attach(&out, 2).await;
            out.snapshot(2, Arc::new(serde_json::json!(2000)))
                .await
                .unwrap();
            // The old attachment's snapshot waiting when Lost was sent is
            // taken late and dropped; nothing of attachment 1 follows Lost.
            assert_eq!(drain(&mut inbox).await, ["lost", "attaching", "2000"]);
        });
    }
}

#[cfg(test)]
mod first_run_tests {
    use super::*;

    /// A fresh install has no log, no folder and no install to guess from:
    /// nothing is started, rather than a daemon that exits and is retried.
    #[test]
    fn a_fresh_config_names_no_log() {
        let none = crate::config::ParserConfig::default().daemon_args_with(|| None);
        assert!(!names_a_log(&none), "{none:?}");
        let log = vec![OsString::from("--log"), OsString::from("/l/eqlog_A_b.txt")];
        assert!(names_a_log(&log));
        let dir = vec![OsString::from("--logs-dir"), OsString::from("/g/Logs")];
        assert!(names_a_log(&dir));
    }
}
