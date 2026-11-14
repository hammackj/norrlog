//! Where an EverQuest Legends or EverQuest install is, without asking.

use norrlog_core::game::Game;
use std::path::{Path, PathBuf};

/// The client's folder name for `game`, inside `Installed Games`.
fn folder(game: Game) -> &'static str {
    match game {
        Game::EverQuest => "EverQuest",
        _ => "EverQuest Legends",
    }
}

/// Where the launcher installs inside a Wine prefix's `drive_c`.
const INSTALLED: &str = "drive_c/users/Public/Daybreak Game Company/Installed Games";

/// When an install was last played: its newest `eqlog_*.txt` write, or the
/// Logs folder's own time when it holds none. The folder's time alone moves
/// only when a file is added, so a log being played into all day leaves it
/// behind a bottle where a new character was merely made.
fn played_at(dir: &Path) -> Option<std::time::SystemTime> {
    let logs = dir.join("Logs");
    let folder = std::fs::metadata(&logs).ok()?.modified().ok()?;
    let newest = std::fs::read_dir(&logs)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.starts_with("eqlog_") && n.ends_with(".txt")
        })
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max();
    Some(newest.unwrap_or(folder))
}

/// Of `candidates`, the install played most recently.
fn newest(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates
        .into_iter()
        .filter_map(|d| Some((played_at(&d)?, d)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, d)| d)
}

/// An install of `game`, looked for first beside `known` (an install of
/// either game, or a folder inside one) and then where [`guess_install`]
/// looks. Beside means the same `Installed Games` folder, or the same folder
/// in every sibling Wine prefix: CrossOver keeps one bottle per game, often
/// somewhere other than its default, and the bottle the user already chose
/// says where the others are.
pub fn install_near(known: Option<&Path>, game: Game) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(known) = known {
        for dir in known.ancestors() {
            if dir.file_name().is_some_and(|n| n == "Installed Games") {
                candidates.push(dir.join(folder(game)));
            }
            if dir.file_name().is_some_and(|n| n == "drive_c") {
                let tail = Path::new(INSTALLED).join(folder(game));
                if let Some(bottles) = dir.parent().and_then(Path::parent) {
                    candidates.extend(
                        std::fs::read_dir(bottles)
                            .into_iter()
                            .flatten()
                            .filter_map(|e| e.ok())
                            .map(|e| e.path().join(&tail)),
                    );
                }
                break;
            }
        }
    }
    newest(candidates).or_else(|| guess_install(game))
}

/// An install of `game` found without asking, as [`guess_game_dir`] finds
/// Legends (without the environment override, which names one install).
pub fn guess_install(game: Game) -> Option<PathBuf> {
    let home = ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|k| std::env::var_os(k).map(PathBuf::from))
        .find(|p| p.is_absolute());
    guess_in(home.as_deref(), game)
}

/// An EverQuest Legends install, found without asking. The eqldb collector
/// looks in the same places.
///
/// `$NORRLOG_GAME_DIR` (or the old `$EQLPARSER_GAME_DIR`) first, then the client's default folder inside every
/// Wine prefix the usual tools make (CrossOver bottles on Linux and macOS,
/// Steam's Proton prefixes, Lutris prefixes under ~/Games, ~/.wine), then a
/// plain ~/Games folder and the Windows default. Of several installs with a
/// Logs folder, the one written to most recently.
pub fn guess_game_dir() -> Option<PathBuf> {
    if let Some(d) = norrlog_core::env::var_os("GAME_DIR").map(PathBuf::from) {
        if d.join("Logs").is_dir() {
            return Some(d);
        }
    }
    // Windows sets USERPROFILE, not HOME. With neither, only the absolute
    // Windows locations are tried: joining onto an empty home would probe
    // paths relative to wherever the app was started.
    let home = ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|k| std::env::var_os(k).map(PathBuf::from))
        .find(|p| p.is_absolute());
    guess_game_dir_in(home.as_deref())
}

fn guess_game_dir_in(home: Option<&Path>) -> Option<PathBuf> {
    guess_in(home, Game::EverQuestLegends)
}

fn guess_in(home: Option<&Path>, game: Game) -> Option<PathBuf> {
    let mut candidates = home.map(|h| prefix_candidates(h, game)).unwrap_or_default();
    // The client's default, under `%PUBLIC%` where Windows has moved it.
    let windows = Path::new("Daybreak Game Company")
        .join("Installed Games")
        .join(folder(game));
    if let Some(public) = std::env::var_os("PUBLIC")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        candidates.push(public.join(&windows));
    }
    candidates.push(Path::new(r"C:\Users\Public").join(&windows));
    newest(candidates)
}

/// Every place under `home` an install can be: inside each Wine prefix the
/// usual tools make, then a plain ~/Games folder.
fn prefix_candidates(home: &Path, game: Game) -> Vec<PathBuf> {
    let tail = Path::new(INSTALLED).join(folder(game));
    let tail = tail.as_path();
    let subdirs = |dir: PathBuf| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    };
    let mut prefixes = subdirs(home.join(".cxoffice"));
    prefixes.extend(subdirs(
        home.join("Library/Application Support/CrossOver/Bottles"),
    ));
    for steam in [
        ".local/share/Steam/steamapps/compatdata",
        ".steam/steam/steamapps/compatdata",
    ] {
        prefixes.extend(subdirs(home.join(steam)).into_iter().map(|d| d.join("pfx")));
    }
    prefixes.extend(subdirs(home.join("Games")));
    prefixes.push(home.join(".wine"));
    let mut candidates: Vec<PathBuf> = prefixes.into_iter().map(|p| p.join(tail)).collect();
    candidates.push(home.join("Games").join(folder(game)));
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_other_game_is_found_in_a_sibling_bottle() {
        let root = std::env::temp_dir().join(format!("client-near-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let legends = root
            .join("EverquestLegends")
            .join(INSTALLED)
            .join("EverQuest Legends");
        let live = root.join("Everquest").join(INSTALLED).join("EverQuest");
        // A bottle with the folder but no Logs: never played.
        let unplayed = root.join("Everquest_2").join(INSTALLED).join("EverQuest");
        std::fs::create_dir_all(legends.join("Logs")).unwrap();
        std::fs::create_dir_all(live.join("Logs")).unwrap();
        std::fs::create_dir_all(&unplayed).unwrap();
        let from_log = legends.join("Logs/eqlog_Tharn_freeport.txt");
        assert_eq!(
            install_near(Some(&from_log), Game::EverQuest),
            Some(live.clone())
        );
        assert_eq!(
            install_near(Some(&live), Game::EverQuestLegends),
            Some(legends.clone())
        );
        // Both games in one bottle.
        let shared = root.join("Both").join(INSTALLED);
        std::fs::create_dir_all(shared.join("EverQuest Legends/Logs")).unwrap();
        std::fs::create_dir_all(shared.join("EverQuest/Logs")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::create_dir_all(shared.join("EverQuest/Logs/x")).unwrap();
        assert_eq!(
            install_near(Some(&shared.join("EverQuest Legends")), Game::EverQuest),
            Some(shared.join("EverQuest"))
        );
        // A log written to today beats a folder where a file was only added.
        let garrow = live.join("Logs/eqlog_Garrow_drinal.txt");
        std::fs::write(&garrow, "").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let created = root.join("Everquest_3").join(INSTALLED).join("EverQuest");
        std::fs::create_dir_all(created.join("Logs")).unwrap();
        std::fs::write(created.join("Logs/eqlog_New_teek.txt"), "").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&garrow, "played").unwrap();
        assert_eq!(
            install_near(Some(&from_log), Game::EverQuest),
            Some(live.clone())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_legends_in_any_crossover_bottle_and_prefers_the_live_one() {
        let home = std::env::temp_dir().join(format!("client-guess-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let tail = "drive_c/users/Public/Daybreak Game Company/Installed Games/EverQuest Legends";
        // No Logs folder: not an install anybody plays.
        std::fs::create_dir_all(home.join(".cxoffice/Empty").join(tail)).unwrap();
        assert_eq!(guess_game_dir_in(Some(&home)), None);
        let old = home.join(".wine").join(tail);
        std::fs::create_dir_all(old.join("Logs")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let bottle = home.join(".cxoffice/EverquestLegends").join(tail);
        std::fs::create_dir_all(bottle.join("Logs")).unwrap();
        assert_eq!(guess_game_dir_in(Some(&home)), Some(bottle));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// No home at all: nothing relative is probed, so a `Games/EverQuest
    /// Legends` beside wherever the app was started is not mistaken for one.
    #[cfg(not(windows))]
    #[test]
    fn without_a_home_nothing_relative_is_tried() {
        assert_eq!(guess_game_dir_in(None), None);
    }
}
