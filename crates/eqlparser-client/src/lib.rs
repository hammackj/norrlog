//! Attach to the shared `eqlparserd`, starting it if nobody has.
//!
//! The parser app and the eqldb collector share one daemon, and whichever
//! starts first starts it. Neither owns it: the daemon is started with
//! `--idle-exit`, counts the sockets held open on it, and exits once the last
//! one has gone and nothing has asked it anything for that long. So a client
//! does three things, and this crate is those three things:
//!
//! 1. Asks `/api/hello` whether a daemon is already there, and refuses one
//!    whose [`eqlparser_api::API_VERSION`] differs from its own, saying which
//!    side to update.
//! 2. If nothing is listening, starts one. Two clients can race to this; the
//!    loser's daemon finds the port taken and exits with status 3, and the
//!    loser then attaches to the winner like anybody else.
//! 3. Holds a socket — [`Client::events`] — for as long as it wants the daemon
//!    kept alive.
//!
//! Plain requests count as activity but not as a client: a client that only
//! polls keeps the daemon alive by polling more often than the idle timeout.


pub mod install;
use eqlparser_api::{Bags, Event, Hello, Pickups, API_VERSION, IDENTITY};
use futures_util::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub use eqlparser_api as api;

/// The daemon's default port.
pub const DEFAULT_PORT: u16 = 8777;

/// The status `eqlparserd` exits with when its port is already taken.
pub const EXIT_PORT_IN_USE: i32 = 3;

#[derive(Debug)]
pub enum Error {
    /// Nothing answered on the port.
    Unreachable { port: u16, why: String },
    /// Something took the connection and said nothing before the wait ran
    /// out. An eqlparserd binds its port before its initial parse and answers
    /// once that is done, so this is most often one still parsing.
    Busy { port: u16 },
    /// Something answered, and it is not an eqlparserd.
    NotOurs { port: u16, why: String },
    /// An eqlparserd from before `/api/hello` existed.
    Outdated { port: u16 },
    /// An eqlparserd answered, speaking another version of the API.
    Incompatible { port: u16, daemon: Box<Hello> },
    /// The daemon this started exited before it answered.
    DaemonExited { status: Option<i32> },
    /// The daemon could not be started at all.
    Spawn(std::io::Error),
    /// Started, and still not answering when the wait ran out.
    Timeout { port: u16, waited: Duration },
    /// The daemon answered a request with an error of its own.
    Api { status: u16, error: String },
    /// The answer did not have the shape this client expects.
    Decode(String),
    Http(String),
    Socket(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unreachable { port, why } => write!(f, "nothing is answering on port {port}: {why}"),
            Error::Busy { port } => write!(
                f,
                "port {port} is taken but not answering yet, most likely by an eqlparserd still on its first parse"
            ),
            Error::NotOurs { port, why } => {
                write!(f, "port {port} is held by something that is not eqlparserd ({why})")
            }
            Error::Outdated { port } => write!(
                f,
                "port {port} is held by an eqlparserd from before the shared daemon (it has no \
                 /api/hello); quit whatever started it, most likely the parser app, and start again"
            ),
            Error::Incompatible { port, daemon } => {
                let side = if daemon.api_version < API_VERSION {
                    format!(
                        "the daemon is older: quit eqlparserd {} (pid {}) and update it",
                        daemon.version, daemon.pid
                    )
                } else {
                    "this app is older than the daemon: update this app".to_string()
                };
                write!(
                    f,
                    "eqlparserd on port {port} speaks API {} and this app speaks {API_VERSION}; {side}",
                    daemon.api_version
                )
            }
            Error::DaemonExited { status } => match status {
                Some(s) => write!(f, "eqlparserd exited with status {s} before answering"),
                None => write!(f, "eqlparserd was killed before answering"),
            },
            Error::Spawn(e) => write!(f, "could not start eqlparserd: {e}"),
            Error::Timeout { port, waited } => {
                write!(f, "eqlparserd started but was not answering on port {port} after {waited:?}")
            }
            Error::Api { status, error } => write!(f, "eqlparserd answered {status}: {error}"),
            Error::Decode(e) => write!(f, "eqlparserd's answer did not decode: {e}"),
            Error::Http(e) => write!(f, "{e}"),
            Error::Socket(e) => write!(f, "event stream: {e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// How to start a daemon when none is running.
#[derive(Debug, Clone)]
pub struct Spawn {
    /// The `eqlparserd` binary.
    pub program: PathBuf,
    /// Arguments after `--port` and `--idle-exit`: `--log`, `--game-dir` and
    /// the like. Whichever app starts the daemon decides what it follows, so
    /// both apps should pass the same.
    pub args: Vec<OsString>,
    /// Environment set on the daemon on top of this process's own.
    pub env: Vec<(OsString, OsString)>,
    /// How long the daemon outlives its last client.
    pub idle_exit: Duration,
    /// How long to wait for a started daemon to answer.
    pub wait: Duration,
}

impl Spawn {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Spawn {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            idle_exit: Duration::from_secs(30),
            wait: Duration::from_secs(30),
        }
    }
}

/// A daemon this has checked is an eqlparserd speaking its API.
#[derive(Debug, Clone)]
pub struct Client {
    port: u16,
    http: reqwest::Client,
    hello: Hello,
    /// Whether this client started the daemon, as opposed to finding one.
    started: bool,
}

impl Client {
    /// Attach to the daemon on `port`, which must already be running.
    pub async fn attach(port: u16) -> Result<Client> {
        let http = http()?;
        let hello = hello_at(&http, port).await?;
        check(port, &hello)?;
        Ok(Client { port, http, hello, started: false })
    }

    /// Attach to the daemon on `port`, starting one with `spawn` if nothing
    /// is listening.
    pub async fn attach_or_spawn(port: u16, spawn: &Spawn) -> Result<Client> {
        let http = http()?;
        match hello_at(&http, port).await {
            Ok(hello) => {
                check(port, &hello)?;
                return Ok(Client { port, http, hello, started: false });
            }
            Err(Error::Unreachable { .. }) => {}
            Err(e) => return Err(e),
        }

        let mut cmd = tokio::process::Command::new(&spawn.program);
        cmd.arg("--port")
            .arg(port.to_string())
            .arg("--idle-exit")
            .arg(spawn.idle_exit.as_secs().max(1).to_string())
            .args(&spawn.args)
            .envs(spawn.env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            // Not ours to stop: the daemon outlives whoever started it for as
            // long as anybody else is attached.
            .kill_on_drop(false);
        let mut child = cmd.spawn().map_err(Error::Spawn)?;

        let started = Instant::now();
        let mut lost_race = false;
        loop {
            match hello_at(&http, port).await {
                Ok(hello) => {
                    check(port, &hello)?;
                    // Ours if ours is the one answering; a daemon that lost
                    // the race has already exited.
                    let started = !lost_race && hello.pid == child.id().unwrap_or(0);
                    return Ok(Client { port, http, hello, started });
                }
                // Busy is the daemon just started, on its initial parse.
                Err(Error::Unreachable { .. } | Error::Busy { .. }) => {}
                Err(e) => return Err(e),
            }
            if !lost_race {
                if let Ok(Some(status)) = child.try_wait() {
                    // Somebody else bound the port between our hello and our
                    // daemon's bind: theirs is the daemon, and it will answer
                    // once it is ready. Anything else is a real failure.
                    if status.code() == Some(EXIT_PORT_IN_USE) {
                        lost_race = true;
                    } else {
                        return Err(Error::DaemonExited { status: status.code() });
                    }
                }
            }
            if started.elapsed() >= spawn.wait {
                return Err(Error::Timeout { port, waited: spawn.wait });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// What `/api/hello` said when this attached.
    pub fn hello(&self) -> &Hello {
        &self.hello
    }

    /// Whether this client started the daemon it is attached to.
    pub fn started_daemon(&self) -> bool {
        self.started
    }

    /// Ask `/api/hello` again: who is attached, and what is being followed.
    pub async fn refresh_hello(&mut self) -> Result<&Hello> {
        self.hello = self.get("/api/hello").await?;
        Ok(&self.hello)
    }

    /// `GET /api/pickups`: `since` in the log's clock, or the last hour.
    pub async fn pickups(&self, since: Option<i64>) -> Result<Pickups> {
        match since {
            Some(s) => self.get(&format!("/api/pickups?since={s}")).await,
            None => self.get("/api/pickups").await,
        }
    }

    /// `GET /api/bags`.
    pub async fn bags(&self) -> Result<Bags> {
        self.get("/api/bags").await
    }

    /// `GET /api/bundle`: the export document as it stands.
    pub async fn bundle(&self) -> Result<eqlparser_core::bundle::Bundle> {
        self.get("/api/bundle").await
    }

    /// Put a panel up on the overlay, or refresh it. See
    /// [`eqlparser_api::Panel`]: it lapses unless put again within its TTL.
    pub async fn put_panel(&self, id: &str, panel: &eqlparser_api::Panel) -> Result<()> {
        let url = format!("http://127.0.0.1:{}/api/panels/{id}", self.port);
        let resp = self.http.put(&url).json(panel).send().await.map_err(|e| Error::Http(e.to_string()))?;
        Self::status_only(resp).await
    }

    /// POST a JSON body, for the few settings the daemon takes that way
    /// (`/api/class-levels`). Only the status matters.
    pub async fn post_json(&self, path: &str, body: &serde_json::Value) -> Result<()> {
        let url = format!("http://127.0.0.1:{}{path}", self.port);
        let resp = self.http.post(&url).json(body).send().await.map_err(|e| Error::Http(e.to_string()))?;
        Self::status_only(resp).await
    }

    /// Take a panel down.
    pub async fn delete_panel(&self, id: &str) -> Result<()> {
        let url = format!("http://127.0.0.1:{}/api/panels/{id}", self.port);
        let resp = self.http.delete(&url).send().await.map_err(|e| Error::Http(e.to_string()))?;
        Self::status_only(resp).await
    }

    async fn status_only(resp: reqwest::Response) -> Result<()> {
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.bytes().await.unwrap_or_default();
        let error = serde_json::from_slice::<eqlparser_api::ApiError>(&body)
            .map(|e| e.error)
            .unwrap_or_else(|_| String::from_utf8_lossy(&body).into_owned());
        Err(Error::Api { status: status.as_u16(), error })
    }

    /// Any GET, decoded as `T`. For endpoints not yet typed in
    /// [`eqlparser_api`], `T` can be `serde_json::Value`.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("http://127.0.0.1:{}{path}", self.port);
        let resp = self.http.get(&url).send().await.map_err(|e| Error::Http(e.to_string()))?;
        let status = resp.status();
        let body = resp.bytes().await.map_err(|e| Error::Http(e.to_string()))?;
        if !status.is_success() {
            let error = serde_json::from_slice::<eqlparser_api::ApiError>(&body)
                .map(|e| e.error)
                .unwrap_or_else(|_| String::from_utf8_lossy(&body).into_owned());
            return Err(Error::Api { status: status.as_u16(), error });
        }
        serde_json::from_slice(&body).map_err(|e| Error::Decode(format!("{path}: {e}")))
    }

    /// Any GET, as the raw body: an image, say `/api/item-icon`.
    pub async fn get_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let url = format!("http://127.0.0.1:{}{path}", self.port);
        let resp = self.http.get(&url).send().await.map_err(|e| Error::Http(e.to_string()))?;
        let status = resp.status();
        let body = resp.bytes().await.map_err(|e| Error::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(Error::Api { status: status.as_u16(), error: String::from_utf8_lossy(&body).into_owned() });
        }
        Ok(body.to_vec())
    }

    /// Open `/api/events`. The daemon counts this as a client for as long as
    /// the stream is held, so holding it is how an app keeps the daemon up.
    ///
    /// The stream ends when the daemon goes. An item that fails to decode is
    /// yielded as an error and the stream carries on.
    pub async fn events(&self) -> Result<impl Stream<Item = Result<Event>> + Unpin + use<>> {
        let url = format!("ws://127.0.0.1:{}/api/events", self.port);
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| Error::Socket(e.to_string()))?;
        use tokio_tungstenite::tungstenite::Message;
        Ok(Box::pin(ws.filter_map(|m| async move {
            match m {
                Ok(Message::Text(t)) => {
                    Some(serde_json::from_str::<Event>(&t).map_err(|e| Error::Decode(format!("{e}: {t}"))))
                }
                Ok(Message::Close(_)) => None,
                Ok(_) => None,
                Err(e) => Some(Err(Error::Socket(e.to_string()))),
            }
        })))
    }
}

impl Client {
    /// Open `/ws`: the full snapshot the parser's views draw from, once on
    /// connect and again on every change. Untyped, because the snapshot is
    /// the engine's own and still moves with it; read the fields a view
    /// needs. Like [`Client::events`], holding it counts as a client.
    pub async fn snapshots(&self) -> Result<impl Stream<Item = Result<serde_json::Value>> + Unpin + use<>> {
        let url = format!("ws://127.0.0.1:{}/ws", self.port);
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| Error::Socket(e.to_string()))?;
        use tokio_tungstenite::tungstenite::Message;
        Ok(Box::pin(ws.filter_map(|m| async move {
            match m {
                Ok(Message::Text(t)) => Some(serde_json::from_str(&t).map_err(|e| Error::Decode(format!("/ws: {e}")))),
                Ok(Message::Close(_)) => None,
                Ok(_) => None,
                Err(e) => Some(Err(Error::Socket(e.to_string()))),
            }
        })))
    }
}

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .no_proxy()
        .build()
        .map_err(|e| Error::Http(e.to_string()))
}

/// How long `/api/hello` gets to answer before the port counts as busy.
const HELLO_WAIT: Duration = if cfg!(test) { Duration::from_millis(300) } else { Duration::from_secs(5) };

async fn hello_at(http: &reqwest::Client, port: u16) -> Result<Hello> {
    let url = format!("http://127.0.0.1:{port}/api/hello");
    let resp = match http.get(&url).timeout(HELLO_WAIT).send().await {
        Ok(r) => r,
        Err(e) if e.is_connect() => {
            return Err(Error::Unreachable { port, why: e.to_string() })
        }
        Err(e) if e.is_timeout() => return Err(Error::Busy { port }),
        Err(e) => return Err(Error::NotOurs { port, why: e.to_string() }),
    };
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        // Every eqlparserd answers `/api/snapshot`; one that predates the
        // shared daemon has no `/api/hello`. Most likely the parser app's own
        // daemon from before the rebuild, and it needs saying as that.
        let snap = format!("http://127.0.0.1:{port}/api/snapshot");
        if http.get(&snap).timeout(Duration::from_secs(5)).send().await.is_ok_and(|r| r.status().is_success()) {
            return Err(Error::Outdated { port });
        }
    }
    if !resp.status().is_success() {
        return Err(Error::NotOurs { port, why: format!("/api/hello answered {}", resp.status()) });
    }
    resp.json::<Hello>()
        .await
        .map_err(|e| Error::NotOurs { port, why: format!("/api/hello: {e}") })
}

fn check(port: u16, hello: &Hello) -> Result<()> {
    if hello.identity != IDENTITY {
        return Err(Error::NotOurs { port, why: format!("it calls itself {:?}", hello.identity) });
    }
    if hello.api_version != API_VERSION {
        return Err(Error::Incompatible { port, daemon: Box::new(hello.clone()) });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listener that is bound and never answers, as eqlparserd is during
    /// its initial parse, is busy, not a stranger.
    #[tokio::test]
    async fn a_port_that_accepts_and_says_nothing_is_busy() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let got = hello_at(&http().unwrap(), port).await;
        assert!(matches!(got, Err(Error::Busy { port: p }) if p == port), "{got:?}");
        drop(l);
    }

    fn hello(api_version: u32) -> Hello {
        Hello {
            identity: IDENTITY.into(),
            version: "0.0.9".into(),
            api_version,
            pid: 42,
            port: 1,
            started_at: 0,
            game: "everquest-legends".into(),
            log: String::new(),
            character: String::new(),
            server: String::new(),
            game_dir: None,
            screenshots_dir: None,
            clients: 0,
            idle_exit_secs: None,
            clock_offset: None,
            zone: None,
            features: None,
        }
    }

    /// The refusal has to say which side is behind, or the person reading it
    /// updates the wrong thing.
    #[test]
    fn incompatible_names_the_side_to_update() {
        assert!(check(1, &hello(API_VERSION)).is_ok());
        let older = check(1, &hello(API_VERSION - 1)).unwrap_err().to_string();
        assert!(older.contains("the daemon is older") && older.contains("pid 42"), "{older}");
        let newer = check(1, &hello(API_VERSION + 1)).unwrap_err().to_string();
        assert!(newer.contains("update this app"), "{newer}");
        let mut other = hello(API_VERSION);
        other.identity = "nginx".into();
        assert!(matches!(check(1, &other), Err(Error::NotOurs { .. })));
    }

    #[tokio::test]
    async fn nothing_listening_is_unreachable() {
        // Port 1 is privileged and never an eqlparserd.
        assert!(matches!(Client::attach(1).await, Err(Error::Unreachable { .. })));
    }
}
