//! The daemon's tray icon (Linux): its face now that no single app owns it.
//!
//! StatusNotifierItem over D-Bus through `ksni`, running on the tokio
//! runtime the server already has, so the server keeps the main thread.
//! Windows and macOS need `tray-icon` and a main-thread event loop; not done.
//!
//! The tray is never required. `--no-tray`, no session bus, or no tray host
//! (stock GNOME without an extension) all leave a daemon that works exactly
//! as before, and everything in the menu is reachable some other way.

use crate::server::Life;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How often the tooltip and menu are refreshed: the client count and the
/// zone change without the tray being asked.
const REFRESH: Duration = Duration::from_secs(5);

/// What the tray shows that the daemon's static hello does not.
pub type Zone = Box<dyn Fn() -> Option<String> + Send + Sync>;

struct DaemonTray {
    life: Arc<Life>,
    /// Copied in on each refresh, so `menu()` does not take the engine lock
    /// on the D-Bus thread.
    here: Option<String>,
    clients: usize,
    parser: Option<Launch>,
    collector: Option<PathBuf>,
}

/// How to start the parser app: its desktop entry, by id.
#[derive(Clone)]
struct Launch {
    id: String,
}

impl DaemonTray {
    fn port(&self) -> u16 {
        self.life.hello().port
    }

    fn overlay_url(&self) -> String {
        format!("http://127.0.0.1:{}/overlay", self.port())
    }

    fn following(&self) -> String {
        let h = self.life.hello();
        let who = match (h.character.as_str(), h.server.as_str()) {
            ("", _) => "an unnamed character".to_string(),
            (c, "") => c.to_string(),
            (c, s) => format!("{c} on {s}"),
        };
        use norrlog_core::game::Game;
        match Game::parse(&h.game) {
            Some(g @ (Game::EverQuest | Game::EverQuestLegends)) => {
                format!("Following {who} ({})", g.label())
            }
            _ => format!("Following {who}"),
        }
    }

    fn log_ok(&self) -> bool {
        std::path::Path::new(&self.life.hello().log).is_file()
    }
}

/// A menu label shows `_` as an access-key marker; log names are full of
/// them.
fn label(s: impl AsRef<str>) -> String {
    s.as_ref().replace('_', "__")
}

impl ksni::Tray for DaemonTray {
    fn id(&self) -> String {
        format!("norrlogd-{}", self.port())
    }

    fn title(&self) -> String {
        format!("norrlogd on port {}", self.port())
    }

    fn icon_name(&self) -> String {
        "norrlog".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        icons()
    }

    fn status(&self) -> ksni::Status {
        if self.log_ok() {
            ksni::Status::Active
        } else {
            ksni::Status::NeedsAttention
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let mut lines = vec![self.following()];
        if let Some(z) = &self.here {
            lines.push(format!("In {z}"));
        }
        if !self.log_ok() {
            lines.push(format!("Log not found: {}", self.life.hello().log));
        }
        lines.push(format!("{} client(s) · port {}", self.clients, self.port()));
        ksni::ToolTip {
            title: "norrlogd".into(),
            description: lines.join("\n"),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let info = |text: String| -> MenuItem<Self> {
            StandardItem {
                label: label(text),
                enabled: false,
                ..Default::default()
            }
            .into()
        };
        let h = self.life.hello();
        let log = std::path::Path::new(&h.log)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| h.log.clone());
        let mut m: Vec<MenuItem<Self>> = vec![
            info(self.following()),
            info(format!(
                "Log: {log}{}",
                if self.log_ok() { "" } else { " (not found)" }
            )),
        ];
        if let Some(z) = &self.here {
            m.push(info(format!("In {z}")));
        }
        m.push(info(format!(
            "{} client(s) connected · port {}",
            self.clients,
            self.port()
        )));
        m.push(MenuItem::Separator);

        if let Some(p) = &self.parser {
            let id = p.id.clone();
            m.push(
                StandardItem {
                    label: "Open the parser".into(),
                    activate: Box::new(move |_: &mut Self| run("gtk-launch", &[id.as_str()])),
                    ..Default::default()
                }
                .into(),
            );
        }
        if let Some(c) = &self.collector {
            let c = c.clone();
            let port = self.port().to_string();
            m.push(
                StandardItem {
                    label: "Open the collector".into(),
                    activate: Box::new(move |_: &mut Self| {
                        run(&c.to_string_lossy(), &["--port", &port]);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        let url = self.overlay_url();
        m.push(
            StandardItem {
                label: "Open the OBS overlay in a browser".into(),
                activate: Box::new(move |_: &mut Self| run("xdg-open", &[url.as_str()])),
                ..Default::default()
            }
            .into(),
        );
        m.push(info(format!("Overlay URL: {}", self.overlay_url())));
        m.push(MenuItem::Separator);

        let stay = self.life.stays();
        m.push(
            CheckmarkItem {
                label: "Keep running when no app is open".into(),
                checked: stay,
                activate: Box::new(|t: &mut Self| t.life.set_stay(!t.life.stays())),
                ..Default::default()
            }
            .into(),
        );
        if h.idle_exit_secs.is_none() {
            m.push(info("Runs until quit: started without --idle-exit".into()));
        }
        m.push(
            StandardItem {
                label: "Restart".into(),
                activate: Box::new(|_: &mut Self| restart()),
                ..Default::default()
            }
            .into(),
        );
        m.push(
            StandardItem {
                label: if self.clients == 0 {
                    "Quit".into()
                } else {
                    format!("Quit ({} connected app(s) will lose it)", self.clients)
                },
                activate: Box::new(|_: &mut Self| {
                    eprintln!("[norrlogd] quit from the tray");
                    std::process::exit(0);
                }),
                ..Default::default()
            }
            .into(),
        );
        m
    }

    /// No tray host is listening (or it went away). Keep the service
    /// registered so the icon comes back when one starts.
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        true
    }
}

fn run(program: &str, args: &[&str]) {
    match std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => {}
        Err(e) => eprintln!("[norrlogd] tray: could not run {program}: {e}"),
    }
}

/// Replace this process with a fresh copy, same arguments. The listening
/// socket is close-on-exec, so the new one binds the same port.
fn restart() {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("[norrlogd] tray: cannot restart, the executable path is unknown");
        return;
    };
    eprintln!("[norrlogd] restarting from the tray");
    let err = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec();
    eprintln!("[norrlogd] tray: restart failed: {err}");
}

/// Where desktop entries live: the user's own, then the system's.
fn application_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        dirs.push(d);
    } else if let Some(h) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(h.join(".local/share"));
    }
    let sys =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs.into_iter().map(|d| d.join("applications")).collect()
}

fn desktop_entry(id: &str) -> Option<PathBuf> {
    application_dirs()
        .into_iter()
        .map(|d| d.join(format!("{id}.desktop")))
        .find(|p| p.is_file())
}

/// The program a desktop entry runs: the first word of its `Exec=`, with
/// field codes and quotes dropped. Enough for the entries the install scripts
/// write, which name one launcher by its full path.
fn exec_of(entry: &str) -> Option<PathBuf> {
    let line = entry.lines().find_map(|l| l.strip_prefix("Exec="))?;
    let first = line.split_whitespace().next()?.trim_matches('"');
    (!first.is_empty() && !first.starts_with('%')).then(|| PathBuf::from(first))
}

/// The parser's desktop entry, if one is installed.
fn find_parser() -> Option<Launch> {
    let id = "eqlparser";
    desktop_entry(id).map(|_| Launch { id: id.into() })
}

/// The collector: `EQLDB_COLLECTOR`, else `eqldb-collector` on the PATH,
/// else whatever its installed desktop entry runs. Run as a program rather
/// than through `gtk-launch` so it can be told which port this daemon is on.
fn find_collector() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("EQLDB_COLLECTOR")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        return Some(p);
    }
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("eqldb-collector"))
            .find(|p| p.is_file())
    });
    on_path.or_else(|| {
        let entry = std::fs::read_to_string(desktop_entry("eqldb-collector")?).ok()?;
        exec_of(&entry).filter(|p| p.is_file())
    })
}

/// The Norrlog icon at the sizes trays draw, built in: a tray host uses
/// these when the icon theme has no `norrlog` (nothing installed, or an
/// older install that only has the eqlparser one).
fn icons() -> Vec<ksni::Icon> {
    const PNGS: [&[u8]; 4] = [
        include_bytes!("../../../packaging/norrlog-16.png"),
        include_bytes!("../../../packaging/norrlog-32.png"),
        include_bytes!("../../../packaging/norrlog-48.png"),
        include_bytes!("../../../packaging/norrlog-64.png"),
    ];
    PNGS.iter()
        .filter_map(|png| image::load_from_memory(png).ok())
        .map(|img| {
            let rgba = img.to_rgba8();
            let (width, height) = rgba.dimensions();
            // StatusNotifierItem wants ARGB32 in network byte order.
            let data = rgba
                .pixels()
                .flat_map(|p| [p[3], p[0], p[1], p[2]])
                .collect();
            ksni::Icon {
                width: width as i32,
                height: height as i32,
                data,
            }
        })
        .collect()
}

/// Start the tray, or say why not. Never fails the daemon.
pub fn spawn(life: Arc<Life>, zone: Zone) {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        eprintln!("[norrlogd] tray: no session bus, so no tray icon");
        return;
    }
    let tray = DaemonTray {
        clients: life.clients(),
        here: zone(),
        life: Arc::clone(&life),
        parser: find_parser(),
        collector: find_collector(),
    };
    tokio::spawn(async move {
        use ksni::TrayMethods;
        let handle = match tray.spawn().await {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[norrlogd] tray: not shown ({e})");
                return;
            }
        };
        loop {
            tokio::time::sleep(REFRESH).await;
            let (clients, here) = (life.clients(), zone());
            let changed = handle
                .update(move |t| {
                    let changed = t.clients != clients || t.here != here;
                    t.clients = clients;
                    t.here = here;
                    changed
                })
                .await;
            if changed.is_none() {
                return; // the service has shut down
            }
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_program_a_desktop_entry_runs() {
        let e = "[Desktop Entry]\nName=x\nExec=/opt/x/launch.sh %U\nIcon=x\n";
        assert_eq!(super::exec_of(e), Some("/opt/x/launch.sh".into()));
        assert_eq!(
            super::exec_of("Exec=\"/opt/y/run\" --flag"),
            Some("/opt/y/run".into())
        );
        assert_eq!(super::exec_of("Name=nothing to run"), None);
    }

    #[test]
    fn the_built_in_icons_decode_at_every_size() {
        let icons = super::icons();
        let sizes: Vec<i32> = icons.iter().map(|i| i.width).collect();
        assert_eq!(sizes, [16, 32, 48, 64]);
        for i in &icons {
            assert_eq!(i.width, i.height);
            assert_eq!(i.data.len(), (i.width * i.height * 4) as usize);
        }
    }

    #[test]
    fn underscores_survive_a_menu_label() {
        assert_eq!(
            super::label("eqlog_Tharn_freeport.txt"),
            "eqlog__Tharn__freeport.txt"
        );
    }
}
