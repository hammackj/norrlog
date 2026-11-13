//! Where an EverQuest Legends install is, without asking.

use std::path::{Path, PathBuf};

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
    let mut candidates = home.map(prefix_candidates).unwrap_or_default();
    // The client's default, under `%PUBLIC%` where Windows has moved it.
    if let Some(public) = std::env::var_os("PUBLIC")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        candidates.push(public.join(r"Daybreak Game Company\Installed Games\EverQuest Legends"));
    }
    candidates.push(PathBuf::from(
        r"C:\Users\Public\Daybreak Game Company\Installed Games\EverQuest Legends",
    ));
    candidates
        .into_iter()
        .filter_map(|d| {
            let at = std::fs::metadata(d.join("Logs")).ok()?.modified().ok()?;
            Some((at, d))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, d)| d)
}

/// Every place under `home` an install can be: inside each Wine prefix the
/// usual tools make, then a plain ~/Games folder.
fn prefix_candidates(home: &Path) -> Vec<PathBuf> {
    let tail =
        Path::new("drive_c/users/Public/Daybreak Game Company/Installed Games/EverQuest Legends");
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
    candidates.push(home.join("Games/EverQuest Legends"));
    candidates
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
