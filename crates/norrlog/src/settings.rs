//! Settings: what the daemon follows, the export, and the daemon's own log.
//!
//! The config is `config.json`, patched rather than rewritten (see
//! `config`). A change to something the daemon reads only at
//! startup restarts it through the daemon thread, which stops the old one
//! on the old port, whoever started it, and attaches to or starts the new
//! one; every other app attached, the collector among them, reconnects.
//!
//! `/api/logs` and `/api/history` are fetched on every attach and on every
//! visit to the tab, `/api/logs/status` on every visit only. The daemon's
//! log is polled every 1.5 s for as long as there is a daemon, so it is all
//! there when the tab is opened.
//!
//! The overlay cards are `overlay_settings`, rendered here and heard by the
//! shell, which holds the overlay windows.

use crate::api::{Api, query};
use crate::config::{self, ParserConfig};
use crate::daemon::Setup;
use crate::parts::{card, hint, split};
use crate::theme::*;
use crate::theme::{self, px};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use norrlog_api::{DaemonLog, Game, HistoryBrief, LogFile, LogsReply, LogsStatusReply};
use std::path::PathBuf;
use std::time::Duration;

const LOG_POLL: Duration = Duration::from_millis(1500);
/// The pane keeps this many characters, as the renderer's does.
const LOG_KEEP: usize = 4000;

/// `#cfg-since`, in its order.
const SINCE: &[(&str, &str)] = &[
    ("all", "Everything — the whole log"),
    ("2d", "Last 2 days"),
    ("1d", "Last day"),
    ("8h", "Last 8 hours"),
    ("4h", "Last 4 hours"),
    ("2h", "Last 2 hours"),
];

/// `#cfg-eqdb-mode`: the stored value is `""`, `off`, or the URL itself.
const EQDB: &[(&str, &str)] = &[
    ("", "On — eqldb.hammackj.com"),
    ("url", "On — another site"),
    ("off", "Off — bundled data only"),
];

/// What a restart needs beyond the config: where to send it, and which
/// daemon binary to start.
pub struct Restart {
    pub ctl: async_channel::Sender<Setup>,
    pub daemon: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    LogFile,
    LogsDir,
    GameDir,
}

struct Inputs {
    log_file: Entity<InputState>,
    logs_dir: Entity<InputState>,
    game_dir: Entity<InputState>,
    eqdb_url: Entity<InputState>,
    port: Entity<InputState>,
}

pub struct SettingsView {
    api: Option<Api>,
    /// No log chosen and no daemon: the first run. Nothing is waiting on a
    /// daemon then, so nothing should say it is.
    unset: bool,
    visible: bool,
    restart: Restart,
    inputs: Inputs,
    eqdb_mode: &'static str,
    since: String,
    /// `None` before the daemon has answered; `Some(Err)` when it did not.
    logs: Option<Result<LogsReply, ()>>,
    /// The log chosen in the dropdown, by path.
    pick: Option<String>,
    status: Option<Result<LogsStatusReply, ()>>,
    history: Option<HistoryBrief>,
    /// The export's loadout; `None` is all of them.
    build: Option<String>,
    cfg_status: String,
    exp_status: String,
    /// Effects on you now and those already marked, with their marks.
    marks: Vec<MarkRow>,
    /// Why the last theme or scale change could not be saved.
    display_err: Option<String>,
    /// Bumped by every status message, so a timer clears only its own.
    cfg_gen: u64,
    exp_gen: u64,
    daemon_log: String,
    log_next: u64,
    /// The daemon the log is being read from.
    log_pid: Option<u32>,
    log_scroll: ScrollHandle,
    tasks: Vec<Task<()>>,
    /// The Browse dialog's task, kept apart from `tasks`: the log poll adds
    /// one of those every 1.5 s, and a dialog left open longer than the list
    /// lasts was dropped with it, cancelling whatever was picked.
    picking: Option<Task<()>>,
    _poll: Option<Task<()>>,
    /// The overlay cards. The shell listens to them.
    pub overlay: Entity<crate::overlay_settings::OverlayControls>,
}

/// Plane of Sky tracking was turned on or off: the views that show it redraw.
pub struct QuestsChanged;

impl EventEmitter<QuestsChanged> for SettingsView {}

fn input(
    window: &mut Window,
    cx: &mut Context<SettingsView>,
    placeholder: &str,
    value: &str,
) -> Entity<InputState> {
    let (p, v) = (placeholder.to_string(), value.to_string());
    cx.new(|cx| InputState::new(window, cx).placeholder(p).default_value(v))
}

fn eqdb_mode_of(eqdb: &str) -> &'static str {
    match eqdb {
        "" => "",
        "off" => "off",
        _ => "url",
    }
}

impl SettingsView {
    pub fn new(restart: Restart, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cfg = ParserConfig::load();
        let port = cfg.port.unwrap_or(norrlog_client::DEFAULT_PORT).to_string();
        // An empty field means "guessed": say what the guess is.
        let guess = norrlog_client::install::guess_game_dir();
        let logs_guess = guess.as_ref().map(|g| g.join("Logs").display().to_string());
        let inputs = Inputs {
            log_file: input(window, cx, "…/Logs/eqlog_Tharn_freeport.txt", &cfg.log_file),
            logs_dir: input(
                window,
                cx,
                logs_guess.as_deref().unwrap_or(""),
                &cfg.logs_dir,
            ),
            game_dir: input(
                window,
                cx,
                &guess.map(|g| g.display().to_string()).unwrap_or_default(),
                &cfg.game_dir,
            ),
            eqdb_url: input(
                window,
                cx,
                "https://your-eqldb.example",
                if eqdb_mode_of(&cfg.eqdb) == "url" {
                    &cfg.eqdb
                } else {
                    ""
                },
            ),
            port: input(window, cx, "", &port),
        };
        SettingsView {
            api: None,
            unset: false,
            visible: false,
            restart,
            inputs,
            eqdb_mode: eqdb_mode_of(&cfg.eqdb),
            since: if cfg.since.is_empty() {
                "all".into()
            } else {
                cfg.since
            },
            logs: None,
            pick: None,
            status: None,
            history: None,
            build: None,
            cfg_status: String::new(),
            exp_status: String::new(),
            marks: Vec::new(),
            display_err: None,
            cfg_gen: 0,
            exp_gen: 0,
            daemon_log: "waiting for daemon…".into(),
            log_next: 0,
            log_pid: None,
            log_scroll: ScrollHandle::new(),
            tasks: Vec::new(),
            picking: None,
            _poll: None,
            overlay: cx.new(|cx| crate::overlay_settings::OverlayControls::new(window, cx)),
        }
    }

    /// Attached (with the daemon's hello and whether this app started it)
    /// or lost.
    /// The first run: no log is chosen, so no daemon was started.
    pub fn set_unset(&mut self, cx: &mut Context<Self>) {
        self.unset = true;
        cx.notify();
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        if api.is_some() {
            self.unset = false;
        }
        if let Some(a) = &api {
            let h = a.client.hello();
            let what = if a.client.started_daemon() {
                "started"
            } else {
                "attached to"
            };
            self.append_log(&format!(
                "[norrlog] {what} norrlogd {} (pid {}), following {}\n",
                h.version, h.pid, h.log
            ));
            // A different daemon numbers its lines from zero again.
            if self.log_pid != Some(h.pid) {
                self.log_pid = Some(h.pid);
                self.log_next = 0;
            }
        }
        self.api = api;
        self._poll = self.api.as_ref().map(|_| {
            cx.spawn(async move |this, cx| {
                loop {
                    if this.update(cx, |v, cx| v.poll_log(cx)).is_err() {
                        break;
                    }
                    cx.background_executor().timer(LOG_POLL).await;
                }
            })
        });
        if self.api.is_some() {
            self.load_logs(cx);
            self.load_history(cx);
            if self.visible {
                self.load_status(cx);
                self.load_marks(cx);
            }
        } else {
            self.logs = None;
            self.marks.clear();
        }
        cx.notify();
    }

    /// The tab opened or closed. Opening it asks for everything again.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        let opened = visible && !self.visible;
        self.visible = visible;
        if opened {
            self.load_logs(cx);
            self.load_history(cx);
            self.load_status(cx);
            self.load_marks(cx);
        }
    }

    /// What is on you now, and what is marked: a mark is set while the
    /// effect is up, and kept after it fades.
    fn load_marks(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.track(cx.spawn(async move |this, cx| {
            let snap = api.get::<serde_json::Value>("/api/snapshot".into()).await;
            let marks = api.get::<serde_json::Value>("/api/buff-marks".into()).await;
            let _ = this.update(cx, |v, cx| {
                v.marks = mark_rows(snap.ok().as_ref(), marks.ok().as_ref());
                cx.notify();
            });
        }));
    }

    fn set_mark(&mut self, name: String, mark: Option<&'static str>, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.track(cx.spawn(async move |this, cx| {
            let body = serde_json::json!({ "name": name, "mark": mark });
            let _ = api.post_json("/api/buff-marks".into(), body).await;
            let _ = this.update(cx, |v, cx| v.load_marks(cx));
        }));
    }

    fn track(&mut self, t: Task<()>) {
        self.tasks.push(t);
        if self.tasks.len() > 12 {
            self.tasks.drain(..self.tasks.len() - 12);
        }
    }

    fn load_logs(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.track(cx.spawn(async move |this, cx| {
            let got = api.get::<LogsReply>("/api/logs".into()).await;
            let _ = this.update(cx, |v, cx| {
                if let Ok(r) = &got {
                    // Keep a choice still on the list; otherwise the log
                    // being followed, as `selected` does.
                    if !v
                        .pick
                        .as_ref()
                        .is_some_and(|p| r.logs.iter().any(|l| &l.path == p))
                    {
                        v.pick = r.logs.iter().find(|l| l.current).map(|l| l.path.clone());
                    }
                }
                v.logs = Some(got.map_err(|_| ()));
                cx.notify();
            });
        }));
    }

    fn load_history(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.track(cx.spawn(async move |this, cx| {
            let got = api.get::<HistoryBrief>("/api/history".into()).await;
            let _ = this.update(cx, |v, cx| {
                match got {
                    Err(_) => v.exp_status = "The daemon is not answering yet.".into(),
                    Ok(h) if !h.available => v.exp_status = "History database unavailable.".into(),
                    Ok(h) => {
                        v.exp_status.clear();
                        if v.build.as_ref().is_some_and(|b| !builds(&h).contains(b)) {
                            v.build = None;
                        }
                        v.history = Some(h);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn load_status(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else {
            self.status = Some(Err(()));
            return;
        };
        self.track(cx.spawn(async move |this, cx| {
            let got = api.get::<LogsStatusReply>("/api/logs/status".into()).await;
            let _ = this.update(cx, |v, cx| {
                v.status = Some(got.map_err(|_| ()));
                cx.notify();
            });
        }));
    }

    fn poll_log(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        let since = self.log_next;
        self.track(cx.spawn(async move |this, cx| {
            let Ok(d) = api
                .get::<DaemonLog>(format!("/api/daemon-log?since={since}"))
                .await
            else {
                return;
            };
            let _ = this.update(cx, |v, cx| {
                // A different daemon (restarted, or replaced by another
                // app's) numbers its lines from zero again.
                if d.next < v.log_next {
                    v.log_next = 0;
                    return;
                }
                v.log_next = d.next;
                if !d.lines.is_empty() {
                    let text: String = d.lines.iter().map(|l| format!("{}\n", l.text)).collect();
                    v.append_log(&text);
                    cx.notify();
                }
            });
        }));
    }

    fn append_log(&mut self, text: &str) {
        if self.daemon_log == "waiting for daemon…" {
            self.daemon_log.clear();
        }
        self.daemon_log.push_str(text);
        let n = self.daemon_log.chars().count();
        if n > LOG_KEEP {
            let cut = self
                .daemon_log
                .char_indices()
                .nth(n - LOG_KEEP)
                .map_or(0, |(i, _)| i);
            self.daemon_log.drain(..cut);
        }
        self.log_scroll.scroll_to_bottom();
    }

    fn say(&mut self, text: impl Into<String>, secs: u64, cx: &mut Context<Self>) {
        self.cfg_status = text.into();
        self.cfg_gen += 1;
        let gen_ = self.cfg_gen;
        self.track(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(secs))
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.cfg_gen == gen_ {
                    v.cfg_status.clear();
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn say_export(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.exp_status = text.into();
        self.exp_gen += 1;
        let gen_ = self.exp_gen;
        self.track(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let _ = this.update(cx, |v, cx| {
                if v.exp_gen == gen_ {
                    v.exp_status.clear();
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// Write `patch` and, when `restart`, start the daemon again from the
    /// whole config as it now stands. `Err` is what to say.
    fn apply(
        &mut self,
        patch: serde_json::Map<String, serde_json::Value>,
        restart: bool,
    ) -> Result<bool, String> {
        config::save_patch(patch).map_err(|e| format!("Could not save: {e}"))?;
        if restart {
            let cfg = ParserConfig::load();
            let setup = Setup {
                port: cfg.port.unwrap_or(norrlog_client::DEFAULT_PORT),
                daemon: self.restart.daemon.clone(),
                args: cfg.daemon_args(),
            };
            self.restart
                .ctl
                .try_send(setup)
                .map_err(|e| format!("Saved, but the restart was not sent: {e}"))?;
            self.logs = None;
        }
        Ok(restart)
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let text = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let port = match text(&self.inputs.port).parse::<u16>() {
            Ok(p) if p >= 1024 => p,
            Ok(_) => return self.say("The port must be between 1024 and 65535.", 4, cx),
            // `parseInt(…) || 8777`.
            Err(_) => norrlog_client::DEFAULT_PORT,
        };
        // "" is the default site, "off" none, anything else a site of your own.
        let eqdb = match self.eqdb_mode {
            "url" => text(&self.inputs.eqdb_url)
                .trim_end_matches('/')
                .to_string(),
            m => m.to_string(),
        };
        let (log_file, logs_dir, game_dir) = (
            text(&self.inputs.log_file),
            text(&self.inputs.logs_dir),
            text(&self.inputs.game_dir),
        );
        if let Some(e) = (ParserConfig {
            log_file: log_file.clone(),
            game_dir: game_dir.clone(),
            ..Default::default()
        })
        .mismatch()
        {
            return self.say(e, 8, cx);
        }
        let old = ParserConfig::load();
        let restart = log_file != old.log_file
            || logs_dir != old.logs_dir
            || game_dir != old.game_dir
            || Some(port) != old.port
            || eqdb != old.eqdb;
        let mut patch = serde_json::Map::new();
        patch.insert("logFile".into(), log_file.into());
        patch.insert("logsDir".into(), logs_dir.into());
        patch.insert("gameDir".into(), game_dir.into());
        patch.insert("port".into(), port.into());
        patch.insert("eqdb".into(), eqdb.into());
        match self.apply(patch, restart) {
            Ok(true) => self.say("Saved — daemon restarting…", 4, cx),
            Ok(false) => self.say("Saved.", 4, cx),
            Err(e) => self.say(e, 6, cx),
        }
    }

    fn set_since(&mut self, since: &str, cx: &mut Context<Self>) {
        let changed = since != self.since;
        self.since = since.to_string();
        let mut patch = serde_json::Map::new();
        patch.insert("since".into(), since.into());
        // The window is read once, at startup, so changing it means starting
        // again — the same as changing which log is being followed.
        if let Err(e) = self.apply(patch, changed) {
            self.say(e, 6, cx);
        }
        cx.notify();
    }

    /// Point the daemon at a log. Shared by the dropdown and by Browse so
    /// the two ways of naming a file cannot mean different things.
    fn switch_to_log(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if path.is_empty() {
            return;
        }
        if path == self.inputs.log_file.read(cx).value().trim() {
            return self.say("Already following that log.", 3, cx);
        }
        // The folders come along with the log, so the game folder never
        // names the other game; see `ParserConfig::follow_log`.
        let cfg = ParserConfig::load();
        let (next, patch) = match cfg.follow_log(std::path::Path::new(&path)) {
            Ok(x) => x,
            Err(e) => return self.say(e, 8, cx),
        };
        let restart = next.log_file != cfg.log_file
            || next.logs_dir != cfg.logs_dir
            || next.game_dir != cfg.game_dir;
        for (input, value) in [
            (&self.inputs.log_file, next.log_file),
            (&self.inputs.logs_dir, next.logs_dir),
            (&self.inputs.game_dir, next.game_dir),
        ] {
            input.update(cx, |s, cx| s.set_value(value, window, cx));
        }
        match self.apply(patch, restart) {
            Ok(true) => self.say("Switched — daemon restarting…", 6, cx),
            Ok(false) => self.say("Switched.", 6, cx),
            Err(e) => self.say(e, 6, cx),
        }
    }

    /// Follow the other game: its paths from last time, or the install found
    /// beside this one and its newest log. The current paths are kept for
    /// the switch back.
    fn switch_game(&mut self, game: Game, window: &mut Window, cx: &mut Context<Self>) {
        let cfg = ParserConfig::load();
        let (next, patch) = match cfg.switch_to(game, |near| {
            norrlog_client::install::install_near(near, game)
        }) {
            Ok(x) => x,
            Err(e) => return self.say(e, 6, cx),
        };
        for (input, value) in [
            (&self.inputs.log_file, next.log_file),
            (&self.inputs.logs_dir, next.logs_dir),
            (&self.inputs.game_dir, next.game_dir),
        ] {
            input.update(cx, |s, cx| s.set_value(value, window, cx));
        }
        self.pick = None;
        match self.apply(patch, true) {
            Ok(_) => self.say(
                format!("Switched to {} — daemon restarting…", game.label()),
                6,
                cx,
            ),
            Err(e) => self.say(e, 6, cx),
        }
    }

    /// The game the paths in the fields name, if they name one.
    fn shown_game(&self, cx: &App) -> Game {
        let text = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let paths = [
            text(&self.inputs.game_dir),
            text(&self.inputs.log_file),
            text(&self.inputs.logs_dir),
        ];
        Game::detect(
            paths
                .iter()
                .filter(|s| !s.is_empty())
                .map(|s| std::path::Path::new(s.as_str())),
        )
    }

    /// Browse. Choosing a log file is an instruction to follow it, not a
    /// draft; the directories only fill their fields, to apply on Save.
    fn browse(&mut self, what: Pick, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: what == Pick::LogFile,
            directories: what != Pick::LogFile,
            multiple: false,
            prompt: Some(
                if what == Pick::LogFile {
                    "Follow this log"
                } else {
                    "Choose"
                }
                .into(),
            ),
        });
        self.picking = Some(cx.spawn_in(window, async move |this, cx| {
            let got = rx.await;
            let _ = this.update_in(cx, |v, window, cx| match got {
                Ok(Ok(Some(paths))) => {
                    let Some(p) = paths.into_iter().next() else {
                        return;
                    };
                    let p = p.display().to_string();
                    match what {
                        Pick::LogFile => v.switch_to_log(p, window, cx),
                        Pick::LogsDir => v
                            .inputs
                            .logs_dir
                            .update(cx, |s, cx| s.set_value(p, window, cx)),
                        Pick::GameDir => v
                            .inputs
                            .game_dir
                            .update(cx, |s, cx| s.set_value(p, window, cx)),
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => v.say(format!("No file picker: {e}"), 6, cx),
                Err(_) => {}
            });
        }));
    }

    fn export_path(&self, format: &str) -> String {
        let mut q = Vec::new();
        if !format.is_empty() {
            q.push(("format", format.to_string()));
        }
        if let Some(b) = &self.build {
            q.push(("build", b.clone()));
        }
        format!("/api/export?{}", query(&q))
    }

    fn copy_markdown(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else {
            return self.say_export("Copy failed: the daemon is not answering", cx);
        };
        let path = self.export_path("");
        self.track(cx.spawn(async move |this, cx| {
            let got = api.get_bytes(path).await;
            let _ = this.update(cx, |v, cx| match got {
                Ok(b) => {
                    let text = String::from_utf8_lossy(&b).into_owned();
                    let rows = markdown_rows(&text);
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    v.say_export(format!("Copied — {rows} rows of Markdown"), cx);
                }
                Err(e) => v.say_export(format!("Copy failed: {e}"), cx),
            });
        }));
    }

    fn save_csv(&mut self, cx: &mut Context<Self>) {
        let Some(api) = &self.api else { return };
        // Content-Disposition makes this a download rather than a page.
        crate::parts::open_web(cx, &format!("{}{}", api.base(), self.export_path("csv")));
        self.say_export("Opened CSV download in your browser", cx);
    }
}

/// Every loadout with a kill on file, sorted; a kill with none is `unknown`.
fn builds(h: &HistoryBrief) -> Vec<String> {
    let mut b: Vec<String> = h
        .kills
        .iter()
        .map(|k| {
            k.build
                .clone()
                .filter(|b| !b.is_empty())
                .unwrap_or_else(|| "unknown".into())
        })
        .collect();
    b.sort();
    b.dedup();
    b
}

/// Table rows in an export: every `|` line less the header and its rule.
fn markdown_rows(text: &str) -> usize {
    text.lines()
        .filter(|l| l.starts_with('|'))
        .count()
        .saturating_sub(2)
}

/// Bytes at the scale a log is read at: `0`, `N B`, `N KB`, `N.N MB`.
fn bytes(n: u64) -> String {
    match n {
        0 => "0".into(),
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{} KB", (n as f64 / 1024.).round()),
        n => format!("{:.1} MB", n as f64 / (1024. * 1024.)),
    }
}

/// A log's line in the dropdown: `● ` if followed, who if known, then the
/// file, its size and when it was last written.
fn log_option(l: &LogFile) -> String {
    log_option_at(l, &file_time(l.modified))
}

/// A file's modification time, in this machine's zone. Unlike a log
/// timestamp, which is local wall time read as UTC, this is a real instant:
/// the renderer formats it as UTC and shows a log written at 11:17 in
/// Chicago as 17:17.
fn file_time(secs: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local
        .timestamp_opt(secs, 0)
        .single()
        .filter(|_| secs != 0)
    {
        Some(d) => d.format("%Y-%m-%d %H:%M").to_string(),
        None => "—".into(),
    }
}

fn log_option_at(l: &LogFile, when: &str) -> String {
    let who = match (&l.character, &l.server) {
        (Some(c), Some(s)) if !c.is_empty() && !s.is_empty() => format!("{c} ({s}) — "),
        (Some(c), _) if !c.is_empty() => format!("{c} — "),
        _ => String::new(),
    };
    format!(
        "{}{who}{} ({:.1} MB, {})",
        if l.current { "● " } else { "" },
        l.name,
        l.bytes as f64 / (1024. * 1024.),
        when
    )
}

const LOG_HINT: &str = "Every eqlog_*.txt in the logs directory, newest first. Switching restarts the daemon against \
    that file — the live views start over from it, while the history database keeps everything already recorded. \
    Leave the path empty to follow whichever log is newest.";
const DIR_HINT: &str = "One log is followed live; the rest are swept on a timer into the same history database, so a \
    character you are not playing still contributes its kills, loot and screenshots. Behind is how much of each file \
    nothing has read yet. Empty means the install's Logs folder, shown greyed when one was found.";
const SWITCH_HINT: &str = "Switches the log, logs directory and game directory together. Each game's last log is \
    remembered, so switching back returns to it; the first switch finds the install beside this one and follows its \
    newest log.";
const GAME_HINT: &str = "Needs spells_us.txt. Without it, buff timers and class detection are unavailable. Empty \
    means the install found by looking, shown greyed when there is one.";
const SINCE_HINT: &str = "A shorter window starts far faster, but every figure derived from the log shrinks with it: \
    loot, kills, encounters and totals cover the window, not a lifetime. The lockout grid, Sky progress and turn-in \
    history are kept in the database and are unaffected. Changing this restarts the parser.";
const EQDB_HINT: &str = "Item tooltips, icons and the Quest Tracker's Plane of Sky record are asked of an eqldb site, with the item \
    data bundled in the app behind it. Off asks no site anything; tooltips then come from the bundled data alone. \
    Saved with Save & restart daemon, which restarts the shared daemon with it, whichever app started it.";
const EXPORT_HINT: &str = "Boss kills grouped by loadout, with kill counts, average and best times, and your DPS. \
    Drawn from the durable history, so it spans every log file ever parsed — not just the one being followed. Timing \
    and DPS averages exclude kills whose fight could not be bounded, rather than counting them as zero.";
/// A scale as the Electron app writes it: `1.3`, not the `1.2999999523`
/// an `f32` widens to, which its dropdown would not recognise.
fn scale_json(v: f32) -> f64 {
    (f64::from(v) * 100.).round() / 100.
}

const SKY_HINT: &str = "The Quest Tracker tab, the dashboard's Plane of Sky bars and the overlay's turn-in panel. \
    Off stops the app asking for them; the daemon keeps reading the lines, so nothing is missed if you turn it back on.";

const THEME_HINT: &str = "The default matches eqldb and hammackj.com, so the tools read as one family. Applies to \
    this window; the overlay keeps its own palette, tuned against game footage rather than a desktop.";
const SCALE_HINT: &str =
    "Scales the whole window, text and spacing alike, to taste. Applies immediately.";
const MARKS_HINT: &str = "How the overlay lists an effect. Auto leaves it to detection, \
which recognises an aura or song only after it has pulsed a few times. Aura lists it \
with auras from the start, never counting down; Until zoned shows it as permanent. \
Effects on you now are bright; marked ones not on you are dim.";

/// One effect in the marks list.
#[derive(Debug, Clone, PartialEq)]
struct MarkRow {
    name: String,
    /// "aura", "permanent", or none.
    mark: Option<String>,
    /// On you right now.
    live: bool,
}

/// Effects on you now (buffs and auras, as the snapshot has them), then
/// marked ones that are not, each named once.
fn mark_rows(snap: Option<&serde_json::Value>, marks: Option<&serde_json::Value>) -> Vec<MarkRow> {
    let marked: Vec<(String, String)> = marks
        .and_then(|m| m.get("marks"))
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some((
                        m.get("name")?.as_str()?.to_string(),
                        m.get("mark")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let mark_of = |name: &str| {
        marked
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, m)| m.clone())
    };
    let mut rows: Vec<MarkRow> = Vec::new();
    for key in ["auras", "buffs"] {
        let Some(list) = snap.and_then(|s| s.get(key)).and_then(|v| v.as_array()) else {
            continue;
        };
        for b in list {
            let Some(name) = b.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            if rows.iter().any(|r| r.name.eq_ignore_ascii_case(name)) {
                continue;
            }
            rows.push(MarkRow {
                name: name.to_string(),
                mark: mark_of(name),
                live: true,
            });
        }
    }
    for (name, mark) in &marked {
        if !rows.iter().any(|r| r.name.eq_ignore_ascii_case(name)) {
            rows.push(MarkRow {
                name: name.clone(),
                mark: Some(mark.clone()),
                live: false,
            });
        }
    }
    rows
}

fn field(label: &str) -> Div {
    v_flex().gap_1().child(
        div()
            .text_sm()
            .text_color(hex(MUTED))
            .child(label.to_string()),
    )
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .gap_3()
                    .child(split(
                        self.render_paths(cx),
                        v_flex()
                            .gap_3()
                            .child(self.render_display(cx))
                            .children(self.render_quests(cx))
                            .child(self.render_marks(cx))
                            .child(self.render_export(cx))
                            .child(self.overlay.clone()),
                    ))
                    .child(self.render_daemon_log(cx)),
            )
    }
}

impl SettingsView {
    /// Theme and UI scale: both apply at once and are saved on the spot, with
    /// no daemon restart, as the Electron app does.
    fn render_display(&self, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let current = theme::palette();
        let w = this.clone();
        let theme_menu = Button::new("set-theme")
            .label(
                theme::THEMES
                    .iter()
                    .find(|t| t.0 == current)
                    .map_or("", |t| t.1),
            )
            .small()
            .w_full()
            .dropdown_menu(move |mut menu, _, _| {
                for &(val, label) in &theme::THEMES {
                    let w = w.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(current == val).on_click(
                        move |_, _, cx| {
                            theme::set_palette(val);
                            theme::apply(cx);
                            let _ = w.update(cx, |v, cx| {
                                v.save_display("theme", serde_json::json!(val), cx)
                            });
                        },
                    ));
                }
                menu
            });
        let scale = theme::scale();
        let w = this;
        let scale_menu = Button::new("set-scale")
            .label(
                theme::SCALES
                    .iter()
                    .find(|t| t.0 == scale)
                    .map_or_else(|| format!("{:.0}%", scale * 100.), |t| t.1.to_string()),
            )
            .small()
            .w_full()
            .dropdown_menu(move |mut menu, _, _| {
                for &(val, label) in &theme::SCALES {
                    let w = w.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(scale == val).on_click(
                        move |_, _, cx| {
                            theme::set_scale(val);
                            theme::apply(cx);
                            let _ = w.update(cx, |v, cx| {
                                v.save_display("uiScale", serde_json::json!(scale_json(val)), cx)
                            });
                        },
                    ));
                }
                menu
            });
        card("Display")
            .child(field("Theme").child(theme_menu).child(hint(THEME_HINT)))
            .child(field("UI scale").child(scale_menu).child(hint(SCALE_HINT)))
            .when_some(self.display_err.clone(), |d, e| {
                d.child(div().text_sm().text_color(hex(ERR)).child(e))
            })
    }

    /// The Quest Tracker's sections, each on or off. Only for a game that
    /// has one: Plane of Sky quests are Legends'.
    fn render_quests(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !crate::features::has("sky") {
            return None;
        }
        Some(
            card("Quest Tracker")
                .child(
                    Checkbox::new("track-sky")
                        .label("Track Plane of Sky quests")
                        .checked(crate::features::sky_wanted())
                        .on_click(cx.listener(|v, on: &bool, _, cx| {
                            crate::features::set_sky_wanted(*on);
                            v.save_display("trackSkyQuests", serde_json::json!(*on), cx);
                            // Its panel comes and goes from the overlay list.
                            v.overlay.update(cx, |_, cx| cx.notify());
                            cx.emit(QuestsChanged);
                        })),
                )
                .child(hint(SKY_HINT)),
        )
    }

    fn save_display(&mut self, key: &str, value: serde_json::Value, cx: &mut Context<Self>) {
        let mut patch = serde_json::Map::new();
        patch.insert(key.to_string(), value);
        self.display_err = config::save_patch(patch)
            .err()
            .map(|e| format!("Could not save: {e}"));
        cx.notify();
    }

    fn render_paths(&self, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let browse = |id: &'static str, what: Pick| {
            Button::new(id)
                .label("Browse")
                .small()
                .on_click(cx.listener(move |v, _, window, cx| v.browse(what, window, cx)))
        };

        // The log dropdown.
        let (pick_label, logs): (String, Vec<LogFile>) = match &self.logs {
            None | Some(Err(_)) if self.unset => {
                ("Choose a log below, then save".into(), Vec::new())
            }
            None | Some(Err(_)) => ("Waiting for the daemon…".into(), Vec::new()),
            Some(Ok(r)) if r.logs.is_empty() => ("No eqlog_*.txt found".into(), Vec::new()),
            Some(Ok(r)) => (
                r.logs
                    .iter()
                    .find(|l| Some(&l.path) == self.pick.as_ref())
                    .or(r.logs.first())
                    .map(log_option)
                    .unwrap_or_default(),
                r.logs.clone(),
            ),
        };
        let picked = self.pick.clone();
        let w = this.clone();
        let log_menu = Button::new("set-logpick")
            .label(pick_label)
            .small()
            .w_full()
            .dropdown_menu(move |mut menu, _, _| {
                for l in &logs {
                    let (w, path) = (w.clone(), l.path.clone());
                    menu = menu.item(
                        PopupMenuItem::new(log_option(l))
                            .checked(picked.as_ref() == Some(&l.path))
                            .on_click(move |_, _, cx| {
                                let _ = w.update(cx, |v, cx| {
                                    v.pick = Some(path.clone());
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            });

        let since = self.since.clone();
        let w = this.clone();
        let since_menu = Button::new("set-since")
            .label(
                SINCE
                    .iter()
                    .find(|s| s.0 == since)
                    .map_or(since.as_str(), |s| s.1)
                    .to_string(),
            )
            .small()
            .w_full()
            .dropdown_menu(move |mut menu, _, _| {
                for &(val, label) in SINCE {
                    let w = w.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(since == val).on_click(
                        move |_, _, cx| {
                            let _ = w.update(cx, |v, cx| v.set_since(val, cx));
                        },
                    ));
                }
                menu
            });

        let mode = self.eqdb_mode;
        let w = this.clone();
        let eqdb_menu = Button::new("set-eqdb")
            .label(EQDB.iter().find(|m| m.0 == mode).map_or("", |m| m.1))
            .small()
            .w_full()
            .dropdown_menu(move |mut menu, _, _| {
                for &(val, label) in EQDB {
                    let w = w.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(mode == val).on_click(
                        move |_, _, cx| {
                            let _ = w.update(cx, |v, cx| {
                                v.eqdb_mode = val;
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });

        let shown = self.shown_game(cx);
        let game_button = |id: &'static str, game: Game| {
            let b = Button::new(id)
                .label(game.label())
                .small()
                .on_click(cx.listener(move |v, _, window, cx| v.switch_game(game, window, cx)));
            if shown == game { b.primary() } else { b }
        };
        card("Paths")
            .child(
                field("Game")
                    .child(
                        h_flex()
                            .gap_2()
                            .child(game_button("set-game-eql", Game::EverQuestLegends))
                            .child(game_button("set-game-eq", Game::EverQuest)),
                    )
                    .child(hint(SWITCH_HINT)),
            )
            .child(
                field("Log file")
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(log_menu))
                            .child(
                                Button::new("set-switch")
                                    .label("Switch to it")
                                    .small()
                                    .on_click(cx.listener(|v, _, window, cx| {
                                        if let Some(p) = v.pick.clone() {
                                            v.switch_to_log(p, window, cx);
                                        }
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.inputs.log_file).small()),
                            )
                            .child(browse("set-browse-log", Pick::LogFile)),
                    )
                    .child(hint(LOG_HINT))
                    .children(self.render_characters()),
            )
            .child(
                field("Logs directory")
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.inputs.logs_dir).small()),
                            )
                            .child(browse("set-browse-logs", Pick::LogsDir)),
                    )
                    .child(hint(DIR_HINT))
                    .child(self.render_status()),
            )
            .child(
                field("Game directory")
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.inputs.game_dir).small()),
                            )
                            .child(browse("set-browse-game", Pick::GameDir)),
                    )
                    .child(hint(GAME_HINT)),
            )
            .child(
                field("Read on startup")
                    .child(since_menu)
                    // A saved window the daemon would refuse (an old date, a
                    // hand edit) is left out when it starts; say so here, not
                    // only in the app's output.
                    .when(!crate::config::window_ok(&self.since), |d| {
                            d.child(div().text_color(hex(WARN)).child(format!(
                                "The saved window {:?} is not one the daemon takes, so the whole log is read. Pick one above.",
                                self.since.trim()
                            )))
                        },
                    )
                    .child(hint(SINCE_HINT)),
            )
            .child(
                field("Item info from eqldb")
                    .child(eqdb_menu)
                    .when(mode == "url", |d| {
                        d.child(Input::new(&self.inputs.eqdb_url).small())
                    })
                    .child(hint(EQDB_HINT)),
            )
            .child(
                field("Daemon port").child(
                    div()
                        .w(px(120.))
                        .child(Input::new(&self.inputs.port).small()),
                ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("set-save")
                            .label("Save & restart daemon")
                            .small()
                            .primary()
                            .on_click(cx.listener(|v, _, _, cx| v.save(cx))),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(hex(MUTED))
                            .child(self.cfg_status.clone()),
                    ),
            )
    }

    /// Whose history is whose, so switching to an alt's log does not look
    /// like the recorded kills and levels have vanished.
    fn render_characters(&self) -> Option<Div> {
        let Some(Ok(r)) = &self.logs else { return None };
        let now = r.following.as_ref()?;
        let me = now.character.as_deref().filter(|c| !c.is_empty())?;
        let server = now
            .server
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("?");
        let others: Vec<String> = r
            .characters
            .iter()
            .filter(|c| {
                c.character != me
                    || c.server.as_deref().unwrap_or("") != now.server.as_deref().unwrap_or("")
            })
            .map(|c| {
                format!(
                    "{} on {} ({} boss {})",
                    c.character,
                    c.server.as_deref().filter(|s| !s.is_empty()).unwrap_or("?"),
                    c.kills,
                    if c.kills == 1 { "kill" } else { "kills" }
                )
            })
            .collect();
        let tail = if others.is_empty() {
            String::new()
        } else {
            format!(" Also on file, kept separately: {}.", others.join(", "))
        };
        let parts = [
            ("Recording history as ", false),
            (me, true),
            (" on ", false),
            (server, true),
            (".", false),
            (&tail, false),
        ];
        Some(
            div()
                .text_sm()
                .text_color(hex(MUTED))
                .child(crate::parts::rich(&parts)),
        )
    }

    /// Every log in the folder and how far behind each is, worst first.
    fn render_status(&self) -> Div {
        let r = match &self.status {
            None => return div(),
            Some(Err(_)) if self.unset => return div(),
            Some(Err(_)) => {
                return hint("The daemon is not answering, so how far it has read is unknown.");
            }
            Some(Ok(r)) => r,
        };
        if r.logs.is_empty() {
            return hint(match &r.why {
                Some(why) => format!("No logs read: {why}."),
                None => "No logs found in that folder.".into(),
            });
        }
        let every = r
            .every
            .filter(|&e| e > 0)
            .map(|e| format!(" Others are swept every {} min.", (e as f64 / 60.).round()))
            .unwrap_or_default();
        let n = r.logs.len();
        v_flex()
            .gap_1()
            .children(r.logs.iter().map(|l| {
                // A log with nothing left to read says so in a word rather
                // than a zero, which would read as a smaller version of the
                // others and not as "done".
                let behind = if l.behind > 0 {
                    format!("{} behind", bytes(l.behind))
                } else {
                    "read".into()
                };
                h_flex()
                    .gap_3()
                    .text_sm()
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(4.))
                    .bg(hex(PANEL_2))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(if l.player.is_empty() {
                                l.name.clone()
                            } else {
                                l.player.clone()
                            })
                            .when(!l.server.is_empty(), |d| {
                                d.child(div().text_color(hex(MUTED)).child(l.server.clone()))
                            }),
                    )
                    .child(
                        div()
                            .w(px(110.))
                            .text_color(hex(if l.behind > 0 { WARN } else { HEAL }))
                            .child(behind),
                    )
                    .child(
                        div()
                            .w(px(90.))
                            .text_color(hex(MUTED))
                            .child(if l.followed { "followed live" } else { "swept" }),
                    )
                    .child(
                        div()
                            .w(px(70.))
                            .text_color(hex(MUTED))
                            .child(bytes(l.bytes)),
                    )
            }))
            .child(hint(format!(
                "{n} log{} in {}.{every}",
                if n == 1 { "" } else { "s" },
                r.dir.as_deref().unwrap_or("")
            )))
    }

    /// Declare an effect an aura or until-zoned, for the overlay's buff
    /// panels. Detection gets there on its own, but late: a song is not known
    /// for an aura until it has pulsed a few times.
    fn render_marks(&self, cx: &mut Context<Self>) -> Div {
        let mut list = v_flex().gap_1();
        if self.marks.is_empty() {
            list = list.child(hint("Nothing on you, and nothing marked."));
        }
        for (i, row) in self.marks.iter().enumerate() {
            let choice = |id: &str, label: &'static str, value: Option<&'static str>| {
                let name = row.name.clone();
                let on = row.mark.as_deref() == value;
                // The ghost style's selected state is too faint to read as
                // "this one"; the chosen mark is drawn filled.
                Button::new(SharedString::from(format!("mark-{id}-{i}")))
                    .label(label)
                    .xsmall()
                    .when(on, |b| b.primary())
                    .when(!on, |b| b.ghost())
                    .on_click(cx.listener(move |v, _, _, cx| v.set_mark(name.clone(), value, cx)))
            };
            list = list.child(
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(hex(if row.live { TEXT } else { MUTED }))
                            .child(row.name.clone()),
                    )
                    .child(choice("a", "Aura", Some("aura")))
                    .child(choice("p", "Until zoned", Some("permanent")))
                    .child(choice("n", "Auto", None)),
            );
        }
        card("Buff marks")
            .child(list)
            .child(hint(MARKS_HINT))
            .child(
                h_flex().child(
                    Button::new("marks-reload")
                        .label("Refresh")
                        .xsmall()
                        .on_click(cx.listener(|v, _, _, cx| v.load_marks(cx))),
                ),
            )
    }

    fn render_export(&self, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let all = format!(
            "All loadouts ({} kills)",
            self.history.as_ref().map_or(0, |h| h.counts.kills)
        );
        let options = self.history.as_ref().map(builds).unwrap_or_default();
        let build = self.build.clone();
        let label = build.clone().unwrap_or_else(|| all.clone());
        let menu = Button::new("set-exp-build")
            .label(label)
            .small()
            .w_full()
            .dropdown_menu(move |mut menu, _, _| {
                let choices = std::iter::once((None, all.clone()))
                    .chain(options.iter().map(|b| (Some(b.clone()), b.clone())));
                for (val, label) in choices {
                    let w = this.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(build == val).on_click(
                        move |_, _, cx| {
                            let _ = w.update(cx, |v, cx| {
                                v.build = val.clone();
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });
        card("Export")
            .child(
                field("Raid history for build guides")
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(menu))
                            .child(
                                Button::new("set-exp-md")
                                    .label("Copy Markdown")
                                    .small()
                                    .on_click(cx.listener(|v, _, _, cx| v.copy_markdown(cx))),
                            )
                            .child(
                                Button::new("set-exp-csv")
                                    .label("Save CSV")
                                    .small()
                                    .on_click(cx.listener(|v, _, _, cx| v.save_csv(cx))),
                            ),
                    )
                    .child(hint(EXPORT_HINT)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(hex(MUTED))
                    .child(self.exp_status.clone()),
            )
    }

    fn render_daemon_log(&self, cx: &mut Context<Self>) -> Div {
        card("Daemon log").child(vbar(
            &self.log_scroll,
            div()
                .id("set-daemon-log")
                .track_scroll(&self.log_scroll)
                .max_h(px(260.))
                .overflow_y_scroll()
                .p(px(8.))
                .rounded(px(5.))
                .bg(hex(PANEL_2))
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .text_color(hex(MUTED))
                .child(self.daemon_log.clone()),
        ))
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings in `gpui::test`, which shadows `#[test]`.
    use super::{
        HistoryBrief, LogFile, builds, bytes, eqdb_mode_of, file_time, log_option_at,
        markdown_rows, scale_json,
    };

    #[test]
    fn formats_follow_settings_js() {
        assert_eq!(bytes(0), "0");
        assert_eq!(bytes(900), "900 B");
        assert_eq!(bytes(412 * 1024 + 300), "412 KB");
        assert_eq!(bytes(5 * 1024 * 1024 + 400 * 1024), "5.4 MB");
        assert_eq!(
            markdown_rows("# t\n\n| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n"),
            2
        );
        assert_eq!(markdown_rows("no table"), 0);
        let l = LogFile {
            path: "/l/eqlog_Tharn_freeport.txt".into(),
            name: "eqlog_Tharn_freeport.txt".into(),
            character: Some("Tharn".into()),
            server: Some("freeport".into()),
            bytes: 3 * 1024 * 1024 + 200 * 1024,
            modified: 1_789_000_000,
            current: true,
        };
        assert_eq!(
            log_option_at(&l, "2026-09-09 19:26"),
            "● Tharn (freeport) — eqlog_Tharn_freeport.txt (3.2 MB, 2026-09-09 19:26)"
        );
        assert_eq!(file_time(0), "—");
        let h = HistoryBrief {
            available: true,
            kills: vec![
                norrlog_api::KillBuild {
                    build: Some("SHD/BRD".into()),
                },
                norrlog_api::KillBuild { build: None },
                norrlog_api::KillBuild {
                    build: Some("SHD/BRD".into()),
                },
            ],
            ..Default::default()
        };
        assert_eq!(builds(&h), ["SHD/BRD", "unknown"]);
        assert_eq!(eqdb_mode_of("https://x.example"), "url");
    }

    #[test]
    fn scale_is_saved_as_written() {
        for (v, want) in [(1.3f32, 1.3f64), (0.85, 0.85), (1.15, 1.15), (1.5, 1.5)] {
            assert_eq!(scale_json(v), want);
        }
    }
}

#[cfg(test)]
mod mark_tests {
    // Not `super::*`: that brings in gpui's own `test` attribute.
    use super::mark_rows;

    /// On you now first (auras, then buffs), each once, with its mark; then
    /// what is marked but not up, dim.
    #[test]
    fn rows_are_what_is_on_you_then_what_is_marked() {
        let snap: serde_json::Value = serde_json::from_str(
            r#"{"auras": [{"name": "Aura of the Muse"}],
                "buffs": [{"name": "Chant of Battle"}, {"name": "aura of the muse"}]}"#,
        )
        .unwrap();
        let marks: serde_json::Value = serde_json::from_str(
            r#"{"marks": [{"name": "Chant of Battle", "mark": "aura"},
                          {"name": "Illusion: Skeleton", "mark": "permanent"}]}"#,
        )
        .unwrap();
        let rows = mark_rows(Some(&snap), Some(&marks));
        let got: Vec<(&str, Option<&str>, bool)> = rows
            .iter()
            .map(|r| (r.name.as_str(), r.mark.as_deref(), r.live))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Aura of the Muse", None, true),
                ("Chant of Battle", Some("aura"), true),
                ("Illusion: Skeleton", Some("permanent"), false),
            ]
        );
        assert!(mark_rows(None, None).is_empty());
    }
}
