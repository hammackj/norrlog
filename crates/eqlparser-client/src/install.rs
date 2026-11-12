//! Where an EverQuest Legends install is, without asking.

use std::path::{Path, PathBuf};

/// An EverQuest Legends install, found without asking: the same places the
/// Electron app looks (`guessPaths` in `gui/main.js`), and the eqldb
/// collector, which had this copy first.
///
/// `$EQLPARSER_GAME_DIR` first, then the client's default folder inside every
/// Wine prefix the usual tools make (CrossOver bottles on Linux and macOS,
/// Steam's Proton prefixes, Lutris prefixes under ~/Games, ~/.wine), then a
/// plain ~/Games folder and the Windows default. Of several installs with a
/// Logs folder, the one written to most recently.
pub fn guess_game_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("EQLPARSER_GAME_DIR").map(PathBuf::from) {
        if d.join("Logs").is_dir() {
            return Some(d);
        }
    }
    guess_game_dir_in(&std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default())
}

fn guess_game_dir_in(home: &Path) -> Option<PathBuf> {
    let tail = Path::new("drive_c/users/Public/Daybreak Game Company/Installed Games/EverQuest Legends");
    let subdirs = |dir: PathBuf| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    };
    let mut prefixes = subdirs(home.join(".cxoffice"));
    prefixes.extend(subdirs(home.join("Library/Application Support/CrossOver/Bottles")));
    for steam in [".local/share/Steam/steamapps/compatdata", ".steam/steam/steamapps/compatdata"] {
        prefixes.extend(subdirs(home.join(steam)).into_iter().map(|d| d.join("pfx")));
    }
    prefixes.extend(subdirs(home.join("Games")));
    prefixes.push(home.join(".wine"));
    let mut candidates: Vec<PathBuf> = prefixes.into_iter().map(|p| p.join(tail)).collect();
    candidates.push(home.join("Games/EverQuest Legends"));
    candidates.push(PathBuf::from(r"C:\Users\Public\Daybreak Game Company\Installed Games\EverQuest Legends"));
    candidates
        .into_iter()
        .filter_map(|d| {
            let at = std::fs::metadata(d.join("Logs")).ok()?.modified().ok()?;
            Some((at, d))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, d)| d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_legends_in_any_crossover_bottle_and_prefers_the_live_one() {
        let home = std::env::temp_dir().join(format!("client-guess-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let tail = "drive_c/users/Public/Daybreak Game Company/Installed Games/EverQuest Legends";
        // No Logs folder: not an install anybody plays.
        std::fs::create_dir_all(home.join(".cxoffice/Empty").join(tail)).unwrap();
        assert_eq!(guess_game_dir_in(&home), None);
        let old = home.join(".wine").join(tail);
        std::fs::create_dir_all(old.join("Logs")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let bottle = home.join(".cxoffice/EverquestLegends").join(tail);
        std::fs::create_dir_all(bottle.join("Logs")).unwrap();
        assert_eq!(guess_game_dir_in(&home), Some(bottle));
        let _ = std::fs::remove_dir_all(&home);
    }
}
