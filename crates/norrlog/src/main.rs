//! Norrlog: an EverQuest log parser.
//!
//! A window onto the shared `norrlogd`, which does the parsing: this
//! attaches to it (starting it if nobody has) and draws what it reports.
//!
//!     norrlog [--port N] [--daemon PATH]
//!
//! What the daemon follows comes from the app's config (see `config`), so
//! whichever app starts it starts it the same way; Settings writes it.

// No console window behind the app on Windows. Debug builds keep theirs, for
// the log lines.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod alerts;
mod api;
mod bestiary;
mod character;
mod charts;
mod classes;
mod config;
mod daemon;
mod dashboard;
mod encounters;
mod experience;
mod features;
mod fight;
// The shared toolkit the tab ports draw on; ahead of its users for now.
#[allow(dead_code)]
mod forage;
mod format;
mod item_tip;
mod loot;
mod overlay;
mod overlay_settings;
mod parts;
mod procs;
mod settings;
mod sky;
mod sound;
mod spell_tip;
mod state;
mod table;
#[allow(dead_code)]
mod tells;
mod theme;
mod tradeskills;
mod view;

use gpui::*;
use gpui_component::Root;
use std::path::PathBuf;

fn main() {
    let cfg = config::ParserConfig::load();
    let mut port = cfg.port.unwrap_or(norrlog_client::DEFAULT_PORT);
    let mut daemon_bin: Option<PathBuf> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--port") => match args
                .next()
                .and_then(|v| v.to_str().and_then(|v| v.parse().ok()))
            {
                Some(p) => port = p,
                None => die("--port takes a number"),
            },
            Some("--daemon") => daemon_bin = args.next().map(PathBuf::from),
            Some("-h" | "--help") => {
                println!("norrlog [--port N] [--daemon PATH]");
                return;
            }
            _ => die(&format!("unknown argument {}", a.to_string_lossy())),
        }
    }

    let daemon_bin = daemon::find_daemon(daemon_bin);
    let setup = daemon::Setup {
        port,
        daemon: daemon_bin.clone(),
        args: cfg.daemon_args(),
    };
    // A window that falls behind gets the newest snapshot, and every
    // lifecycle message; see `daemon::Inbox`.
    let (tx, rx) = daemon::mailbox();
    // Settings' restarts.
    let (ctl_tx, ctl_rx) = async_channel::bounded(4);
    let rt = daemon::spawn(setup, tx, ctl_rx);
    let state = state::State::load();

    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        theme::set_palette(&cfg.theme);
        theme::set_scale(cfg.ui_scale.unwrap_or(1.));
        theme::apply(cx);
        let bounds = match state.bounds {
            Some(r) => {
                let b = Bounds::new(point(px(r.x), px(r.y)), size(px(r.w), px(r.h)));
                if state.maximized {
                    WindowBounds::Maximized(b)
                } else {
                    WindowBounds::Windowed(b)
                }
            }
            None => WindowBounds::centered(size(px(1180.), px(800.)), cx),
        };
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Norrlog".into()),
                ..Default::default()
            }),
            window_bounds: Some(bounds),
            window_min_size: Some(size(px(900.), px(600.))),
            app_id: Some("norrlog".into()),
            ..Default::default()
        };
        let main = cx
            .open_window(options, move |window, cx| {
                let restart = settings::Restart {
                    ctl: ctl_tx,
                    daemon: daemon_bin,
                };
                let view = cx.new(|cx| view::Shell::new(rx, rt, state, restart, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("could not open the window");
        // Closing the main window quits, overlays or not: an overlay left
        // over a game with nothing to close it from would be stranded.
        let main_id = main.window_id();
        cx.on_window_closed(move |cx, closed| {
            if closed == main_id || cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}

fn die(msg: &str) -> ! {
    eprintln!("norrlog: {msg}");
    std::process::exit(2);
}
