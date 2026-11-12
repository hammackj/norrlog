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
        use eqlparser_core::game::Game;
        match Game::parse(&h.game) {
            Some(g @ (Game::EverQuest | Game::EverQuestLegends)) => format!("Following {who} ({})", g.label()),
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
        format!("eqlparserd-{}", self.port())
    }

    fn title(&self) -> String {
        format!("eqlparserd on port {}", self.port())
    }

    fn icon_name(&self) -> String {
        "eqlparser".into()
    }

    fn status(&self) -> ksni::Status {
        if self.log_ok() { ksni::Status::Active } else { ksni::Status::NeedsAttention }
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
        ksni::ToolTip { title: "eqlparserd".into(), description: lines.join("\n"), ..Default::default() }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let info = |text: String| -> MenuItem<Self> {
            StandardItem { label: label(text), enabled: false, ..Default::default() }.into()
        };
        let h = self.life.hello();
        let log = std::path::Path::new(&h.log)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| h.log.clone());
        let mut m: Vec<MenuItem<Self>> = vec![
            info(self.following()),
            info(format!("Log: {log}{}", if self.log_ok() { "" } else { " (not found)" })),
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
                    eprintln!("[eqlparserd] quit from the tray");
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
        Err(e) => eprintln!("[eqlparserd] tray: could not run {program}: {e}"),
    }
}

/// Replace this process with a fresh copy, same arguments. The listening
/// socket is close-on-exec, so the new one binds the same port.
fn restart() {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("[eqlparserd] tray: cannot restart, the executable path is unknown");
        return;
    };
    eprintln!("[eqlparserd] restarting from the tray");
    let err = std::process::Command::new(exe).args(std::env::args_os().skip(1)).exec();
    eprintln!("[eqlparserd] tray: restart failed: {err}");
}

/// The parser's desktop entry, if one is installed.
fn find_parser() -> Option<Launch> {
    let id = "eqlparser";
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        dirs.push(d);
    } else if let Some(h) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(h.join(".local/share"));
    }
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs.iter()
        .any(|d| d.join("applications").join(format!("{id}.desktop")).is_file())
        .then(|| Launch { id: id.into() })
}

/// The collector: `EQLDB_COLLECTOR`, else `eqldb-collector` on the PATH.
fn find_collector() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("EQLDB_COLLECTOR").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join("eqldb-collector"))
        .find(|p| p.is_file())
}

/// Start the tray, or say why not. Never fails the daemon.
pub fn spawn(life: Arc<Life>, zone: Zone) {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        eprintln!("[eqlparserd] tray: no session bus, so no tray icon");
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
                eprintln!("[eqlparserd] tray: not shown ({e})");
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
    fn underscores_survive_a_menu_label() {
        assert_eq!(super::label("eqlog_Tharn_freeport.txt"), "eqlog__Tharn__freeport.txt");
    }
}
