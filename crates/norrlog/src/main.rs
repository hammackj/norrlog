//! Norrlog: an EverQuest log parser.
//!
//! A window onto the shared `eqlparserd`, which does the parsing: this
//! attaches to it (starting it if nobody has) and draws what it reports.
//!
//!     norrlog [--port N] [--daemon PATH]
//!
//! What the daemon follows comes from the Electron app's config while the
//! two live side by side, so whichever starts it starts it the same way;
//! Settings writes it.

mod alerts;
mod api;
mod character;
mod classes;
mod config;
mod charts;
mod daemon;
mod dashboard;
mod encounters;
mod experience;
mod fight;
// The shared toolkit the tab ports draw on; ahead of its users for now.
#[allow(dead_code)]
mod forage;
mod format;
mod item_tip;
mod loot;
mod parts;
mod procs;
mod sky;
mod settings;
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
    let mut port = cfg.port.unwrap_or(eqlparser_client::DEFAULT_PORT);
    let mut daemon_bin: Option<PathBuf> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--port") => match args.next().and_then(|v| v.to_str().and_then(|v| v.parse().ok())) {
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
    let setup = daemon::Setup { port, daemon: daemon_bin.clone(), args: cfg.daemon_args() };
    // Bounded, and snapshots displace the oldest waiting message: a window
    // that falls behind wants the newest state, not a queue of old ones.
    let (tx, rx) = async_channel::bounded(16);
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
                if state.maximized { WindowBounds::Maximized(b) } else { WindowBounds::Windowed(b) }
            }
            None => WindowBounds::centered(size(px(1180.), px(800.)), cx),
        };
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions { title: Some("Norrlog".into()), ..Default::default() }),
            window_bounds: Some(bounds),
            window_min_size: Some(size(px(900.), px(600.))),
            app_id: Some("norrlog".into()),
            ..Default::default()
        };
        cx.open_window(options, move |window, cx| {
            let restart = settings::Restart { ctl: ctl_tx, daemon: daemon_bin };
            let view = cx.new(|cx| view::Shell::new(rx, rt, state, restart, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("could not open the window");
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
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
