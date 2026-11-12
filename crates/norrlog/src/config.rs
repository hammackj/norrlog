//! What to follow: read from the Electron app's own config while the two
//! apps live side by side, so both start the shared daemon the same way.
//! This app does not write it yet; that comes with the Settings port.

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
}

/// `$XDG_CONFIG_HOME`, else `~/.config`: where Electron keeps `userData`.
pub fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

impl ParserConfig {
    /// The Electron app's `config.json`, or defaults when there is none.
    pub fn load() -> Self {
        config_home()
            .map(|d| d.join("eqlparser-gui/config.json"))
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// The daemon's arguments after `--port` and `--idle-exit`, as
    /// `ensureDaemon` in `gui/main.js` builds them. With nothing configured,
    /// the install is guessed and its Logs folder followed.
    pub fn daemon_args(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = Vec::new();
        let game_dir = (!self.game_dir.is_empty())
            .then(|| PathBuf::from(&self.game_dir))
            .or_else(eqlparser_client::install::guess_game_dir);
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

#[cfg(test)]
mod tests {
    use super::*;

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
