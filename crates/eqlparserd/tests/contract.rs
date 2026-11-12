//! The daemon's answers decode into `eqlparser-api` types, through the client
//! the other apps use, against the real binary.
//!
//! Unit tests in the daemon check that handlers build the right values; these
//! check the thing that breaks between processes — a field serialised under
//! one name and read under another, a 503 where a client expects a body, a
//! socket the daemon never counts — and the shared-daemon lifecycle: two apps
//! racing to start it get one daemon, and it exits after the last one leaves.
//!
//! Each test runs its own daemon on a free port with `HOME` and the XDG
//! directories pointed into a scratch folder, so nothing here reads or writes
//! anybody's history, item ids or screenshots.

use eqlparser_client::{api::Event, Client, Spawn};
use futures_util::StreamExt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const START: &[&str] = &[
    "[Tue Jul 28 12:00:00 2026] You have entered West Commonlands.",
    "[Tue Jul 28 12:00:05 2026] --You have looted a Stale Bread from a rat's corpse.--",
];

struct Scratch {
    dir: PathBuf,
    log: PathBuf,
    game: PathBuf,
}

fn scratch(name: &str) -> Scratch {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("contract-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let game = dir.join("game");
    std::fs::create_dir_all(game.join("Logs")).unwrap();
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let log = game.join("Logs").join("eqlog_Tharn_freeport.txt");
    std::fs::write(&log, START.join("\n") + "\n").unwrap();
    std::fs::write(
        game.join("Tharn_freeport-Inventory.txt"),
        "Location\tName\tID\tCount\tSlots\n\
         General1\tRusty Dagger\t400\t1\t0\n\
         General2\tRusty Dagger +2\t400\t1\t0\n\
         General3\tSpider Silk\t401\t3\t0\n",
    )
    .unwrap();
    Scratch { dir, log, game }
}

fn spawn(s: &Scratch) -> Spawn {
    let home = s.dir.join("home");
    let mut sp = Spawn::new(env!("CARGO_BIN_EXE_eqlparserd"));
    sp.args = ["--log".into(), s.log.clone().into_os_string(), "--game-dir".into(), s.game.clone().into_os_string()]
        .into_iter()
        .chain(["--no-shots", "--no-tray", "--eqdb", "http://127.0.0.1:1"].map(Into::into))
        .collect();
    sp.env = vec![
        ("HOME".into(), home.clone().into()),
        ("XDG_CONFIG_HOME".into(), home.join(".config").into()),
        ("XDG_DATA_HOME".into(), home.join(".local/share").into()),
    ];
    sp.idle_exit = Duration::from_secs(2);
    sp.wait = Duration::from_secs(30);
    sp
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Wait for a condition, failing the test after `secs`.
async fn eventually<F: AsyncFnMut() -> bool>(secs: u64, what: &str, mut f: F) {
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what} did not happen within {secs}s");
}

/// Wait for the daemon to exit on its own.
///
/// Watched by connecting a bare socket, not by asking `/api/hello`: every
/// request counts as activity, so polling with one keeps the daemon alive —
/// which is what the first version of this test did.
async fn gone(port: u16) {
    eventually(15, "the daemon exiting once idle", async || {
        tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_endpoints_and_events() {
    let s = scratch("typed");
    let port = free_port();
    let c = Client::attach_or_spawn(port, &spawn(&s)).await.unwrap();
    assert!(c.started_daemon());
    assert_eq!(c.hello().character, "Tharn");
    assert_eq!(c.hello().idle_exit_secs, Some(2));

    let mut events = c.events().await.unwrap();
    // The backlog is parsed, and the stream counted as a client.
    let mut c2 = c.clone();
    eventually(10, "the event socket counting as a client", async || {
        c2.refresh_hello().await.map(|h| h.clients == 1).unwrap_or(false)
    })
    .await;
    eventually(10, "the backlog being parsed", async || {
        c.pickups(Some(0)).await.map(|p| !p.pickups.is_empty()).unwrap_or(false)
    })
    .await;

    let p = c.pickups(Some(0)).await.unwrap();
    assert_eq!(p.pickups[0].item, "Stale Bread");
    assert_eq!(p.pickups[0].zone.as_deref(), Some("West Commonlands"));

    eventually(10, "the dump being swept", async || {
        c.bags().await.map(|b| !b.items.is_empty()).unwrap_or(false)
    })
    .await;
    let b = c.bags().await.unwrap();
    assert_eq!((b.character.as_str(), b.server.as_str()), ("Tharn", "freeport"));
    assert_eq!(b.sources.len(), 1);
    let dagger = b.items.iter().find(|i| i.name == "Rusty Dagger").expect("dagger");
    assert_eq!(dagger.grades, vec![0, 2]);

    // Decodes as the core `Bundle`, which is the point.
    c.bundle().await.unwrap();

    let mut f = std::fs::OpenOptions::new().append(true).open(&s.log).unwrap();
    writeln!(f, "[Tue Jul 28 12:01:00 2026] You have entered East Commonlands.").unwrap();
    writeln!(f, "[Tue Jul 28 12:01:05 2026] --You have looted a Rusty Dagger from a rat's corpse.--").unwrap();
    writeln!(f, "[Tue Jul 28 12:01:06 2026] Taking a screenshot...").unwrap();
    drop(f);

    let mut got = Vec::new();
    while got.len() < 3 {
        let e = tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .expect("an event within 10s")
            .expect("the stream open")
            .expect("an event that decodes");
        got.push(e);
    }
    assert!(matches!(&got[0], Event::ZoneChanged { zone: Some(z), .. } if z == "East Commonlands"), "{got:?}");
    assert!(matches!(&got[1], Event::Pickup(p) if p.item == "Rusty Dagger" && p.held), "{got:?}");
    assert!(matches!(got[2], Event::ScreenshotTaken { .. }), "{got:?}");

    // The zone, now that the log has said one.
    let mut c3 = c.clone();
    eventually(10, "hello naming the zone", async || {
        c3.refresh_hello().await.map(|h| h.zone.as_deref() == Some("East Commonlands")).unwrap_or(false)
    })
    .await;

    // The overlay's panel slot: put, list, refresh in place, refuse, take down.
    use eqlparser_client::api::{Panel, PanelRow, Panels};
    let row = |n: &str| PanelRow { name: n.into(), value: "icon".into(), note: None, note_value: None, highlight: false };
    let panel = |rows: Vec<PanelRow>| Panel { title: "Worth a picture".into(), meta: String::new(), rows, ttl_secs: 30 };
    c.put_panel("eqldb", &panel(vec![row("Rusty Dagger")])).await.unwrap();
    c.put_panel("other", &panel(vec![])).await.unwrap();
    c.put_panel("eqldb", &panel(vec![row("Bone Chips")])).await.unwrap();
    let up: Panels = c.get("/api/panels").await.unwrap();
    let ids: Vec<&str> = up.panels.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["eqldb", "other"], "a refresh keeps its place");
    assert_eq!(up.panels[0].panel.rows[0].name, "Bone Chips");
    assert!(c.put_panel("no/slash", &panel(vec![])).await.is_err());
    assert!(c.put_panel("long", &panel((0..9).map(|i| row(&i.to_string())).collect())).await.is_err());
    c.delete_panel("other").await.unwrap();
    c.delete_panel("never-was").await.unwrap();
    let up: Panels = c.get("/api/panels").await.unwrap();
    assert_eq!(up.panels.len(), 1);

    // Let go, and the daemon goes: 2 s idle, checked every 2 s.
    drop(events);
    let port = c.port();
    drop(c);
    gone(port).await;
    let _ = std::fs::remove_dir_all(&s.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_apps_starting_at_once_share_one_daemon() {
    let s = scratch("race");
    let port = free_port();
    let sp = spawn(&s);
    let (a, b) = tokio::join!(Client::attach_or_spawn(port, &sp), Client::attach_or_spawn(port, &sp));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.hello().pid, b.hello().pid, "one daemon");
    assert_eq!(
        [a.started_daemon(), b.started_daemon()].iter().filter(|x| **x).count(),
        1,
        "exactly one of them started it"
    );
    // A third arrives later and attaches without starting anything.
    let c = Client::attach_or_spawn(port, &sp).await.unwrap();
    assert!(!c.started_daemon());
    assert_eq!(c.hello().pid, a.hello().pid);
    drop((a, b, c));
    gone(port).await;
    let _ = std::fs::remove_dir_all(&s.dir);
}
