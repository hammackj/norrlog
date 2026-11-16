//! What to follow, and how the app looks: `eqlparser-gui/config.json` in the
//! config directory. The name is the earlier Electron app's, kept so nobody's
//! settings move.
//!
//! Settings writes it by patching: the file is read as JSON, only the keys
//! being changed are replaced, and the rest are written back untouched, so
//! a key this version does not know survives a save.

use norrlog_api::Game;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What is followed for one game: kept per game under `games`, so switching
/// back returns to the log that was being followed, not to a guess.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Paths {
    pub log_file: String,
    pub logs_dir: String,
    pub game_dir: String,
}

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
    /// Whether the Quest Tracker follows Plane of Sky quests; absent means
    /// yes, as it always did.
    pub track_sky_quests: Option<bool>,
    /// Whether an installed copy asks for a newer release on launch; absent
    /// means yes.
    pub check_for_updates: Option<bool>,
    /// The paths last followed for each game, by slug.
    pub games: BTreeMap<String, Paths>,
}

/// Where Electron keeps `userData` (its `appData`): `%APPDATA%` on Windows,
/// `~/Library/Application Support` on macOS, else `$XDG_CONFIG_HOME` or
/// `~/.config`. Only an absolute base counts: a relative one would follow
/// the working directory and save somewhere different each run.
pub fn config_home() -> Option<PathBuf> {
    let abs = |k: &str| {
        std::env::var_os(k)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
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

fn save_patch_at(
    p: &std::path::Path,
    patch: serde_json::Map<String, serde_json::Value>,
) -> std::io::Result<()> {
    let mut v = match std::fs::read(p) {
        Ok(b) => serde_json::from_slice::<serde_json::Value>(&b)
            .map_err(|e| std::io::Error::other(format!("{} is not JSON: {e}", p.display())))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e),
    };
    let obj = v
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other(format!("{} is not an object", p.display())))?;
    obj.extend(patch);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.norrlog-tmp");
    // Two-space indent, as `saveConfig` writes it.
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&v).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&tmp, p)
}

impl ParserConfig {
    fn paths(&self) -> Paths {
        Paths {
            log_file: self.log_file.clone(),
            logs_dir: self.logs_dir.clone(),
            game_dir: self.game_dir.clone(),
        }
    }

    /// The game being followed, from its paths, the game folder first.
    /// `Unknown` when nothing is set and the install is left to a guess.
    pub fn game(&self) -> Game {
        let p = self.paths();
        Game::detect(
            [&p.game_dir, &p.log_file, &p.logs_dir]
                .into_iter()
                .filter(|s| !s.is_empty())
                .map(Path::new),
        )
    }

    /// The patch that follows `game` instead: the paths last used for it
    /// when its install is still there, else the install `find` turns up
    /// (given a path of the current one to look beside) and its newest log.
    /// The current paths are kept under `games` for the switch back. `Err`
    /// is what to say.
    pub fn switch_to(
        &self,
        game: Game,
        find: impl FnOnce(Option<&Path>) -> Option<PathBuf>,
    ) -> Result<(Paths, serde_json::Map<String, serde_json::Value>), String> {
        let now = self.game();
        if now == game {
            return Err(format!("Already following {}.", game.label()));
        }
        let mut next = self
            .games
            .get(game.slug())
            .filter(|p| !p.game_dir.is_empty() && Path::new(&p.game_dir).is_dir())
            .cloned();
        if next.is_none() {
            let cur = self.paths();
            let here = [&cur.game_dir, &cur.log_file, &cur.logs_dir]
                .into_iter()
                .find(|s| !s.is_empty())
                .map(Path::new);
            let install = find(here).ok_or_else(|| {
                format!(
                    "No {} install was found. Set its game directory below and Save.",
                    game.label()
                )
            })?;
            let logs = install.join("Logs");
            next = Some(Paths {
                log_file: String::new(),
                logs_dir: logs.display().to_string(),
                game_dir: install.display().to_string(),
            });
        }
        let mut next = next.unwrap_or_default();
        // A remembered log that has since gone, or none yet: the newest.
        if next.log_file.is_empty() || !Path::new(&next.log_file).is_file() {
            next.log_file = newest_log(Path::new(&next.logs_dir))
                .map(|p| p.display().to_string())
                .unwrap_or_default();
        }
        let mut games = self.games.clone();
        if now != Game::Unknown {
            games.insert(now.slug().into(), self.paths());
        }
        let mut patch = serde_json::Map::new();
        patch.insert("logFile".into(), next.log_file.clone().into());
        patch.insert("logsDir".into(), next.logs_dir.clone().into());
        patch.insert("gameDir".into(), next.game_dir.clone().into());
        patch.insert(
            "games".into(),
            serde_json::to_value(games).map_err(|e| e.to_string())?,
        );
        Ok((next, patch))
    }

    /// The Electron app's `config.json`, or defaults when there is none.
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// The patch that follows `log`, chosen by name (Browse or the list).
    ///
    /// The game folder decides the game, its spells and its site, and the
    /// daemon takes it before the log, so changing the log alone could pair
    /// a live EverQuest log with the Legends install. The rule:
    /// - A log in an install's `Logs` folder brings that install with it:
    ///   its `Logs` folder and its game folder.
    /// - A log anywhere else (a copy, a backup) keeps the folders set,
    ///   unless its path names the other game than the game folder does.
    ///   Then the folders last used for that game are taken, and with none
    ///   remembered it is refused with what to do instead.
    ///
    /// When the game changes, the outgoing paths are kept under `games` for
    /// the switch back, as [`ParserConfig::switch_to`] keeps them.
    pub fn follow_log(
        &self,
        log: &Path,
    ) -> Result<(Paths, serde_json::Map<String, serde_json::Value>), String> {
        let file = log.display().to_string();
        let keep = || Paths {
            log_file: file.clone(),
            logs_dir: self.logs_dir.clone(),
            game_dir: self.game_dir.clone(),
        };
        let next = if let Some(dir) = log.parent().and_then(install_of) {
            Paths {
                log_file: file.clone(),
                logs_dir: log.parent().unwrap_or(log).display().to_string(),
                game_dir: dir.display().to_string(),
            }
        } else {
            let named = Game::from_path(log);
            let set = Game::from_path(Path::new(&self.game_dir));
            if self.game_dir.is_empty()
                || named == Game::Unknown
                || set == Game::Unknown
                || named == set
            {
                keep()
            } else {
                let p = self
                    .games
                    .get(named.slug())
                    .filter(|p| !p.game_dir.is_empty() && Path::new(&p.game_dir).is_dir())
                    .ok_or_else(|| {
                        format!(
                            "That looks like a {} log, but the game directory is {}'s. Switch game first, or set the {} game directory and Save.",
                            named.label(),
                            set.label(),
                            named.label()
                        )
                    })?;
                Paths {
                    log_file: file.clone(),
                    logs_dir: p.logs_dir.clone(),
                    game_dir: p.game_dir.clone(),
                }
            }
        };
        let now = self.game();
        let then = Game::detect(
            [&next.game_dir, &next.log_file, &next.logs_dir]
                .into_iter()
                .filter(|s| !s.is_empty())
                .map(Path::new),
        );
        let mut patch = serde_json::Map::new();
        patch.insert("logFile".into(), next.log_file.clone().into());
        patch.insert("logsDir".into(), next.logs_dir.clone().into());
        patch.insert("gameDir".into(), next.game_dir.clone().into());
        if now != Game::Unknown && then != now {
            let mut games = self.games.clone();
            games.insert(now.slug().into(), self.paths());
            patch.insert(
                "games".into(),
                serde_json::to_value(games).map_err(|e| e.to_string())?,
            );
        }
        Ok((next, patch))
    }

    /// What is wrong with these paths together, if anything: a log whose
    /// path names one game with a game folder that names the other. The
    /// daemon would read it by the folder's rules, and file what it saw
    /// under the wrong game.
    pub fn mismatch(&self) -> Option<String> {
        if self.log_file.is_empty() || self.game_dir.is_empty() {
            return None;
        }
        let named = Game::from_path(Path::new(&self.log_file));
        let set = Game::from_path(Path::new(&self.game_dir));
        (named != Game::Unknown && set != Game::Unknown && named != set).then(|| {
            format!(
                "The log is {}'s and the game directory {}'s. Point both at one game.",
                named.label(),
                set.label()
            )
        })
    }

    /// The saved load window, when it is one the daemon takes: a whole
    /// number of hours or days (`2h`, `1d`). Anything else, a date or a
    /// hand-edited typo, would stop the daemon at startup, and the app would
    /// start it again to the same end; it is left out, so the whole log is
    /// read, and said so in the app's output.
    pub fn since_arg(&self) -> Option<&str> {
        let s = self.since.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("all") {
            return None;
        }
        let ok = window_ok(s);
        if !ok {
            eprintln!(
                "[norrlog] ignoring the saved load window {s:?}; expected 2h, 4h, 8h, 1d, 2d or all"
            );
        }
        ok.then_some(s)
    }

    /// The daemon's arguments after `--port` and `--idle-exit`. With nothing
    /// configured,
    /// the install is guessed and its Logs folder followed.
    pub fn daemon_args(&self) -> Vec<OsString> {
        self.daemon_args_with(norrlog_client::install::guess_game_dir)
    }

    /// The game folder: the one set, else the install the chosen log sits
    /// in, else, only when no log was chosen either, a guess. A guess beside
    /// a chosen log could be the other game's install (it looks for Legends),
    /// and the daemon takes the game and its spell file from the game
    /// folder before the log, so a live EverQuest log was read as Legends.
    pub(crate) fn daemon_args_with(
        &self,
        guess: impl FnOnce() -> Option<PathBuf>,
    ) -> Vec<OsString> {
        let mut a: Vec<OsString> = Vec::new();
        let source = if !self.log_file.is_empty() {
            std::path::Path::new(&self.log_file)
                .parent()
                .map(PathBuf::from)
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
        if let Some(since) = self.since_arg() {
            a.extend(["--since".into(), since.into()]);
        }
        if !self.eqdb.is_empty() {
            a.extend(["--eqdb".into(), (&self.eqdb).into()]);
        }
        a
    }
}

/// The `eqlog_*.txt` in `dir` written to most recently.
fn newest_log(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.starts_with("eqlog_") && n.ends_with(".txt")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(at, _)| *at)
        .map(|(_, p)| p)
}

/// The install a Logs folder belongs to: its parent, when that holds the
/// game's spell file. Anything else is not taken as an install.
/// Whether `raw` is a load window the daemon takes: "all", nothing, or a
/// whole number of hours or days.
pub fn window_ok(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("all") {
        return true;
    }
    let num = s.strip_suffix(['h', 'd', 'H', 'D']).unwrap_or("");
    !num.is_empty()
        && num.len() <= 6
        && num.bytes().all(|b| b.is_ascii_digit())
        && num.bytes().any(|b| b != b'0')
}

fn install_of(logs: &std::path::Path) -> Option<PathBuf> {
    let name = logs.file_name()?.to_string_lossy().to_lowercase();
    let dir = logs.parent()?;
    (name == "logs" && dir.join("spells_us.txt").is_file()).then(|| dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_games_follows_the_newest_log_and_comes_back_to_the_old_one() {
        let root = std::env::temp_dir().join(format!("norrlog-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let legends = root.join("EverquestLegends/EverQuest Legends");
        let live = root.join("Everquest/EverQuest");
        std::fs::create_dir_all(legends.join("Logs")).unwrap();
        std::fs::create_dir_all(live.join("Logs")).unwrap();
        let tharn = legends.join("Logs/eqlog_Tharn_freeport.txt");
        std::fs::write(&tharn, "").unwrap();
        std::fs::write(live.join("Logs/eqlog_Old_firiona.txt"), "").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(live.join("Logs/eqlog_New_teek.txt"), "").unwrap();
        std::fs::write(live.join("Logs/dbg.txt"), "").unwrap();
        let s = |p: &Path| p.display().to_string();
        let mut c = ParserConfig {
            log_file: s(&tharn),
            logs_dir: s(&legends.join("Logs")),
            game_dir: s(&legends),
            ..Default::default()
        };
        assert_eq!(c.game(), Game::EverQuestLegends);
        assert!(c.switch_to(Game::EverQuestLegends, |_| None).is_err());
        assert!(
            c.switch_to(Game::EverQuest, |_| None)
                .unwrap_err()
                .contains("No EverQuest install")
        );
        let (next, patch) = c
            .switch_to(Game::EverQuest, |near| {
                assert_eq!(near, Some(legends.as_path()));
                Some(live.clone())
            })
            .unwrap();
        assert_eq!(next.log_file, s(&live.join("Logs/eqlog_New_teek.txt")));
        assert_eq!(next.game_dir, s(&live));
        // Apply it as the file would be, and pick another log meanwhile.
        c = serde_json::from_value(serde_json::Value::Object(patch)).unwrap();
        c.log_file = s(&live.join("Logs/eqlog_Old_firiona.txt"));
        assert_eq!(c.game(), Game::EverQuest);
        let (back, patch) = c.switch_to(Game::EverQuestLegends, |_| None).unwrap();
        assert_eq!(
            back.log_file,
            s(&tharn),
            "the remembered log, without a search"
        );
        let c: ParserConfig = serde_json::from_value(serde_json::Value::Object(patch)).unwrap();
        let (again, _) = c.switch_to(Game::EverQuest, |_| None).unwrap();
        assert_eq!(again.log_file, s(&live.join("Logs/eqlog_Old_firiona.txt")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_chosen_log_is_not_given_some_other_install() {
        let root = std::env::temp_dir().join(format!("norrlog-gamedir-{}", std::process::id()));
        let live = root.join("EverQuest");
        std::fs::create_dir_all(live.join("Logs")).unwrap();
        let log = live.join("Logs/eqlog_Dorrak_drinal.txt");
        let legends = || Some(PathBuf::from("/somewhere/EverQuest Legends"));
        let args = |c: &ParserConfig| {
            c.daemon_args_with(legends)
                .into_iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let mut c = ParserConfig {
            log_file: log.display().to_string(),
            ..Default::default()
        };

        // No spell file beside the log: no game folder at all, not the guess.
        assert!(
            !args(&c).iter().any(|a| a == "--game-dir"),
            "{:?}",
            args(&c)
        );
        // The install the log sits in, once it looks like one.
        std::fs::write(live.join("spells_us.txt"), "").unwrap();
        let a = args(&c);
        assert_eq!(
            a[a.iter().position(|x| x == "--game-dir").unwrap() + 1],
            live.display().to_string()
        );
        // A folder that was set still wins.
        c.game_dir = "/set/by/hand".into();
        assert!(args(&c).contains(&"/set/by/hand".to_string()));
        // Nothing chosen at all: the guess, and its Logs folder.
        let a = args(&ParserConfig::default());
        assert!(
            a.contains(&"/somewhere/EverQuest Legends".to_string()),
            "{a:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Picking a log by name brings its install along, so the game folder
    /// never contradicts the log; the outgoing game's paths are kept.
    #[test]
    fn a_chosen_log_brings_its_install() {
        let root = std::env::temp_dir().join(format!("norrlog-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let legends = root.join("EverquestLegends/EverQuest Legends");
        let live = root.join("Everquest/EverQuest");
        for g in [&legends, &live] {
            std::fs::create_dir_all(g.join("Logs")).unwrap();
            std::fs::write(g.join("spells_us.txt"), "").unwrap();
        }
        let tharn = legends.join("Logs/eqlog_Tharn_freeport.txt");
        let mirela = live.join("Logs/eqlog_Mirela_freeport.txt");
        std::fs::write(&tharn, "").unwrap();
        std::fs::write(&mirela, "").unwrap();
        let s = |p: &Path| p.display().to_string();
        let apply = |c: &ParserConfig, patch: serde_json::Map<String, serde_json::Value>| {
            let mut v = serde_json::to_value(Paths::default()).unwrap();
            v.as_object_mut().unwrap().extend(patch);
            let mut n: ParserConfig = serde_json::from_value(v).unwrap();
            if n.games.is_empty() {
                n.games = c.games.clone();
            }
            n
        };
        let c = ParserConfig {
            log_file: s(&tharn),
            logs_dir: s(&legends.join("Logs")),
            game_dir: s(&legends),
            ..Default::default()
        };

        // Legends set up, a live log chosen: the live install comes along.
        let (next, patch) = c.follow_log(&mirela).unwrap();
        assert_eq!(next.game_dir, s(&live));
        assert_eq!(next.logs_dir, s(&live.join("Logs")));
        let c2 = apply(&c, patch);
        assert_eq!(c2.game(), Game::EverQuest);
        assert!(c2.mismatch().is_none());
        assert!(
            c2.daemon_args().contains(&live.clone().into_os_string()),
            "{:?}",
            c2.daemon_args()
        );
        assert_eq!(
            c2.games["everquest-legends"].log_file,
            s(&tharn),
            "kept for the way back"
        );

        // And back: the Legends install again, and live's paths kept.
        let (back, patch) = c2.follow_log(&tharn).unwrap();
        assert_eq!(back.game_dir, s(&legends));
        let c3 = apply(&c2, patch);
        assert_eq!(c3.game(), Game::EverQuestLegends);
        assert_eq!(c3.games["everquest"].game_dir, s(&live));

        // A copy outside any install keeps the folders set...
        let copies = root.join("copies");
        std::fs::create_dir_all(&copies).unwrap();
        let copy = copies.join("eqlog_Tharn_freeport.txt");
        let (kept, _) = c.follow_log(&copy).unwrap();
        assert_eq!(kept.game_dir, s(&legends));
        // ...and a custom game folder that names no game is not second-guessed.
        let custom = ParserConfig {
            game_dir: "/opt/eq".into(),
            ..Default::default()
        };
        assert_eq!(custom.follow_log(&copy).unwrap().0.game_dir, "/opt/eq");

        // A copy that names the other game: that game's remembered folders,
        // and with none, refused rather than read by the wrong rules.
        let other = root.join("Everquest/backup/eqlog_Mirela_freeport.txt");
        assert!(
            c.follow_log(&other)
                .unwrap_err()
                .contains("Switch game first")
        );
        let (from_memory, _) = c3.follow_log(&other).unwrap();
        assert_eq!(from_memory.game_dir, s(&live));

        // Save refuses the contradiction by hand too.
        let bad = ParserConfig {
            log_file: s(&mirela),
            game_dir: s(&legends),
            ..Default::default()
        };
        assert!(bad.mismatch().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_patch_keeps_the_keys_it_does_not_name() {
        let dir = std::env::temp_dir().join(format!("norrlog-cfg-{}", std::process::id()));
        let p = dir.join("eqlparser-gui/config.json");
        let mut patch = serde_json::Map::new();
        patch.insert("logFile".into(), "/a".into());
        save_patch_at(&p, patch.clone()).unwrap();
        std::fs::write(
            &p,
            r#"{"logFile":"/b","theme":"spellbook","overlayOptions":"panels=meter"}"#,
        )
        .unwrap();
        patch.insert("port".into(), 8800.into());
        save_patch_at(&p, patch).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"logFile":"/a","port":8800,"theme":"spellbook","overlayOptions":"panels=meter"})
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn args_follow_the_electron_rules() {
        let c: ParserConfig = serde_json::from_str(
            r#"{"logFile":"/l/eqlog_A_b.txt","logsDir":"/l","gameDir":"/g","port":8777,"since":"all","eqdb":"off","uiScale":1}"#,
        )
        .unwrap();
        let a: Vec<String> = c
            .daemon_args()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            a,
            [
                "--log",
                "/l/eqlog_A_b.txt",
                "--game-dir",
                "/g",
                "--eqdb",
                "off"
            ]
        );
        let c = ParserConfig {
            logs_dir: "/l".into(),
            game_dir: "/g".into(),
            since: "2d".into(),
            ..Default::default()
        };
        let a: Vec<String> = c
            .daemon_args()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(a, ["--logs-dir", "/l", "--game-dir", "/g", "--since", "2d"]);
        // A saved window the daemon would refuse is left out, not passed on
        // to stop it at every start.
        for bad in ["2026-09-01", "é", "0h", "-2h", "9999999999h", "h"] {
            let c = ParserConfig {
                since: bad.into(),
                ..Default::default()
            };
            assert_eq!(c.since_arg(), None, "{bad}");
        }
        for good in ["2h", "1D", " 8h "] {
            let c = ParserConfig {
                since: good.into(),
                ..Default::default()
            };
            assert_eq!(c.since_arg(), Some(good.trim()), "{good}");
        }
    }
}
