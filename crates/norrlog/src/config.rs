//! What to follow: the Electron app's own config while the two apps live
//! side by side, so both start the shared daemon the same way.
//!
//! Settings writes it by patching: the file is read as JSON, only the keys
//! this app owns are replaced, and the rest (the overlay's, the theme) are
//! written back untouched. Electron holds its config in memory, so a save
//! there while this app is open puts back what it last read; the two are not
//! meant to be driven at once.

use serde::Deserialize;
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ParserConfig {
    pub log_file: String,
    pub logs_dir: String,
    pub game_dir: String,
    pub port: Option<u16>,
    /// `all`, or a date the daemon reads from.
    pub since: String,
    /// eqldb lookups: empty for the default, a URL, or `off`.
    pub eqdb: String,
    /// The palette: empty for the house one, or `spellbook`.
    pub theme: String,
    /// A multiplier on the whole layout; absent means 1.
    pub ui_scale: Option<f32>,
}

/// Where Electron keeps `userData` (its `appData`): `%APPDATA%` on Windows,
/// `~/Library/Application Support` on macOS, else `$XDG_CONFIG_HOME` or
/// `~/.config`. Only an absolute base counts: a relative one would follow
/// the working directory and save somewhere different each run.
pub fn config_home() -> Option<PathBuf> {
    let abs = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
    if cfg!(target_os = "windows") {
        return abs("APPDATA");
    }
    if cfg!(target_os = "macos") {
        return abs("HOME").map(|h| h.join("Library/Application Support"));
    }
    abs("XDG_CONFIG_HOME").or_else(|| abs("HOME").map(|h| h.join(".config")))
}

/// Electron's `userData` config.
pub fn path() -> Option<PathBuf> {
    config_home().map(|d| d.join("eqlparser-gui/config.json"))
}

/// Replace `patch`'s keys in the config file and keep every other key.
/// Written to a sibling and renamed into place, so a crash mid-write leaves
/// the old file rather than half of a new one.
pub fn save_patch(patch: serde_json::Map<String, serde_json::Value>) -> std::io::Result<()> {
    let p = path().ok_or_else(|| std::io::Error::other("no home directory"))?;
    save_patch_at(&p, patch)
}

fn save_patch_at(p: &std::path::Path, patch: serde_json::Map<String, serde_json::Value>) -> std::io::Result<()> {
    let mut v = match std::fs::read(p) {
        Ok(b) => serde_json::from_slice::<serde_json::Value>(&b)
            .map_err(|e| std::io::Error::other(format!("{} is not JSON: {e}", p.display())))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e),
    };
    let obj = v.as_object_mut().ok_or_else(|| std::io::Error::other(format!("{} is not an object", p.display())))?;
    obj.extend(patch);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.norrlog-tmp");
    // Two-space indent, as `saveConfig` writes it.
    std::fs::write(&tmp, serde_json::to_string_pretty(&v).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, p)
}

impl ParserConfig {
    /// The Electron app's `config.json`, or defaults when there is none.
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// The daemon's arguments after `--port` and `--idle-exit`, as
    /// `ensureDaemon` in `gui/main.js` builds them. With nothing configured,
    /// the install is guessed and its Logs folder followed.
    pub fn daemon_args(&self) -> Vec<OsString> {
        self.daemon_args_with(norrlog_client::install::guess_game_dir)
    }

    /// The game folder: the one set, else the install the chosen log sits
    /// in, else, only when no log was chosen either, a guess. A guess beside
    /// a chosen log could be the other game's install (it looks for Legends),
    /// and the daemon takes the game and its spell file from the game
    /// folder before the log, so a live EverQuest log was read as Legends.
    fn daemon_args_with(&self, guess: impl FnOnce() -> Option<PathBuf>) -> Vec<OsString> {
        let mut a: Vec<OsString> = Vec::new();
        let source = if !self.log_file.is_empty() {
            std::path::Path::new(&self.log_file).parent().map(PathBuf::from)
        } else if !self.logs_dir.is_empty() {
            Some(PathBuf::from(&self.logs_dir))
        } else {
            None
        };
        let game_dir = if !self.game_dir.is_empty() {
            Some(PathBuf::from(&self.game_dir))
        } else if let Some(logs) = &source {
            install_of(logs)
        } else {
            guess()
        };
        if !self.log_file.is_empty() {
            a.extend(["--log".into(), (&self.log_file).into()]);
        } else if !self.logs_dir.is_empty() {
            a.extend(["--logs-dir".into(), (&self.logs_dir).into()]);
        } else if let Some(g) = &game_dir {
            a.extend(["--logs-dir".into(), g.join("Logs").into_os_string()]);
        }
        if let Some(g) = game_dir {
            a.extend(["--game-dir".into(), g.into_os_string()]);
        }
        if !self.since.is_empty() && self.since != "all" {
            a.extend(["--since".into(), (&self.since).into()]);
        }
        if !self.eqdb.is_empty() {
            a.extend(["--eqdb".into(), (&self.eqdb).into()]);
        }
        a
    }
}

/// The install a Logs folder belongs to: its parent, when that holds the
/// game's spell file. Anything else is not taken as an install.
fn install_of(logs: &std::path::Path) -> Option<PathBuf> {
    let name = logs.file_name()?.to_string_lossy().to_lowercase();
    let dir = logs.parent()?;
    (name == "logs" && dir.join("spells_us.txt").is_file()).then(|| dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_log_is_not_given_some_other_install() {
        let root = std::env::temp_dir().join(format!("norrlog-gamedir-{}", std::process::id()));
        let live = root.join("EverQuest");
        std::fs::create_dir_all(live.join("Logs")).unwrap();
        let log = live.join("Logs/eqlog_Dorrak_drinal.txt");
        let legends = || Some(PathBuf::from("/somewhere/EverQuest Legends"));
        let args = |c: &ParserConfig| c.daemon_args_with(legends).into_iter().map(|s| s.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let mut c = ParserConfig { log_file: log.display().to_string(), ..Default::default() };

        // No spell file beside the log: no game folder at all, not the guess.
        assert!(!args(&c).iter().any(|a| a == "--game-dir"), "{:?}", args(&c));
        // The install the log sits in, once it looks like one.
        std::fs::write(live.join("spells_us.txt"), "").unwrap();
        let a = args(&c);
        assert_eq!(a[a.iter().position(|x| x == "--game-dir").unwrap() + 1], live.display().to_string());
        // A folder that was set still wins.
        c.game_dir = "/set/by/hand".into();
        assert!(args(&c).contains(&"/set/by/hand".to_string()));
        // Nothing chosen at all: the guess, and its Logs folder.
        let a = args(&ParserConfig::default());
        assert!(a.contains(&"/somewhere/EverQuest Legends".to_string()), "{a:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_patch_keeps_the_keys_it_does_not_name() {
        let dir = std::env::temp_dir().join(format!("norrlog-cfg-{}", std::process::id()));
        let p = dir.join("eqlparser-gui/config.json");
        let mut patch = serde_json::Map::new();
        patch.insert("logFile".into(), "/a".into());
        save_patch_at(&p, patch.clone()).unwrap();
        std::fs::write(&p, r#"{"logFile":"/b","theme":"spellbook","overlayOptions":"panels=meter"}"#).unwrap();
        patch.insert("port".into(), 8800.into());
        save_patch_at(&p, patch).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({"logFile":"/a","port":8800,"theme":"spellbook","overlayOptions":"panels=meter"}));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn args_follow_the_electron_rules() {
        let c: ParserConfig = serde_json::from_str(
            r#"{"logFile":"/l/eqlog_A_b.txt","logsDir":"/l","gameDir":"/g","port":8777,"since":"all","eqdb":"off","uiScale":1}"#,
        )
        .unwrap();
        let a: Vec<String> = c.daemon_args().iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(a, ["--log", "/l/eqlog_A_b.txt", "--game-dir", "/g", "--eqdb", "off"]);
        let c = ParserConfig { logs_dir: "/l".into(), game_dir: "/g".into(), since: "2026-09-01".into(), ..Default::default() };
        let a: Vec<String> = c.daemon_args().iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(a, ["--logs-dir", "/l", "--game-dir", "/g", "--since", "2026-09-01"]);
    }
}
