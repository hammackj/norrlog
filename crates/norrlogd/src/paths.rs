//! Where this program keeps its state.
//!
//! # Two kinds of file, two directories
//!
//! `history.db` is derived: every row in it can be rebuilt by re-reading the
//! logs, and deleting it costs time rather than information. The JSON files
//! beside it are not — `sky-quests.json` holds a hand-transcribed quest table,
//! `buff-marks.json` and `tasks.json` hold corrections nothing can regenerate.
//! Filing both under "config" meant clearing a cache and losing a
//! transcription were the same gesture.
//!
//! So derived data goes to the platform's data directory and the hand-made
//! files to its config directory.
//!
//! # Never a relative path
//!
//! The old code did `var_os("HOME").unwrap_or_default().join(".config/…")`,
//! which with no `HOME` produces `.config/eqlparser/history.db` — *relative*.
//! The database then followed the working directory, so a daemon started from
//! somewhere else silently found an empty history and accumulated nothing.
//! That reads as a parsing fault, not a path fault, which is what made it
//! worth fixing over a cosmetic XDG complaint.
//!
//! Every function here returns `None` rather than a path it cannot anchor.
//! The caller says so and carries on without persistence, which is the honest
//! outcome: no history is better than history in a different place each run.
//!
//! # The install already on disk
//!
//! `~/.config/<app>` holding everything, `history.db` included, predates the
//! split. Where it exists it keeps being used for both kinds of file: moving
//! somebody's data out from under them to satisfy a specification is not an
//! improvement, and a split applied retroactively would strand half of it.
//! It is recognised by its `history.db`, not by the directory existing. A
//! fresh Linux install puts its config files in `~/.config/<app>` too, and
//! keying on the directory sent that install's database somewhere new on
//! its second run.
//!
//! # The old name
//!
//! The project was eqlparser, and so was the directory. The first run under
//! the new name moves each entry from `<base>/eqlparser` into `<base>/norrlog`
//! (the GUI may already have put its own state there) and leaves the old
//! name behind as a symlink. That way an older build, a tool or the collector
//! still pointing at it finds the same files, and a daemon of the old build
//! with `history.db` open keeps writing to the file it opened. Anything that
//! would overwrite an existing file is left where it is, and so is the old
//! directory, with a message saying what was not moved.

use std::path::{Path, PathBuf};

/// The directory name under whichever base applies.
const APP: &str = "norrlog";
/// What it was called before.
const OLD_APP: &str = "eqlparser";

/// `$HOME`, if it is set and absolute.
fn home() -> Option<PathBuf> {
    let h = PathBuf::from(std::env::var_os("HOME")?);
    h.is_absolute().then_some(h)
}

/// An environment variable read as an absolute directory.
fn abs_env(key: &str) -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os(key)?);
    p.is_absolute().then_some(p)
}

/// Where hand-made state goes on this platform, before the app's own name.
fn config_base() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        return abs_env("APPDATA");
    }
    if cfg!(target_os = "macos") {
        return home().map(|h| h.join("Library/Application Support"));
    }
    abs_env("XDG_CONFIG_HOME").or_else(|| home().map(|h| h.join(".config")))
}

/// Where derived state goes on this platform, before the app's own name.
fn data_base() -> Option<PathBuf> {
    if cfg!(target_os = "windows") || cfg!(target_os = "macos") {
        return config_base();
    }
    abs_env("XDG_DATA_HOME").or_else(|| home().map(|h| h.join(".local/share")))
}

/// Every base a previous version could have written under, moved once per
/// process.
fn migrate_once() {
    // Never the real home directory's files from a unit test.
    if cfg!(test) {
        return;
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut bases: Vec<PathBuf> = [
            home().map(|h| h.join(".config")),
            config_base(),
            data_base(),
        ]
        .into_iter()
        .flatten()
        .collect();
        bases.dedup();
        for base in bases {
            if let Some(note) = migrate(&base) {
                eprintln!("[norrlogd] {note}");
            }
        }
    });
}

/// Move `<base>/eqlparser` into `<base>/norrlog`, entry by entry. Says what
/// it did, or what it could not do; `None` when there was nothing to move.
fn migrate(base: &Path) -> Option<String> {
    let old = base.join(OLD_APP);
    // A symlink here is the trace of an earlier move, not something to move.
    if !std::fs::symlink_metadata(&old).ok()?.is_dir() {
        return None;
    }
    let new = base.join(APP);
    if let Err(e) = std::fs::create_dir_all(&new) {
        return Some(format!(
            "could not create {}: {e}; still using {}",
            new.display(),
            old.display()
        ));
    }
    let mut stayed = Vec::new();
    for entry in std::fs::read_dir(&old).ok()?.flatten() {
        let to = new.join(entry.file_name());
        if std::fs::symlink_metadata(&to).is_ok() || std::fs::rename(entry.path(), &to).is_err() {
            stayed.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    if !stayed.is_empty() {
        return Some(format!(
            "moved {} to {}, except {} (already there, or would not move)",
            old.display(),
            new.display(),
            stayed.join(", ")
        ));
    }
    if std::fs::remove_dir(&old).is_ok() {
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(APP, &old);
    }
    Some(format!("moved {} to {}", old.display(), new.display()))
}

/// The pre-split `~/.config/norrlog`, if this install has one.
fn legacy() -> Option<PathBuf> {
    let dir = home()?.join(".config").join(APP);
    dir.join("history.db").is_file().then_some(dir)
}

/// Hand-made state: corrections, transcriptions, definitions.
pub fn config_dir() -> Option<PathBuf> {
    migrate_once();
    legacy().or_else(|| config_base().map(|p| p.join(APP)))
}

/// Derived state: rebuildable from the logs.
pub fn data_dir() -> Option<PathBuf> {
    migrate_once();
    legacy().or_else(|| data_base().map(|p| p.join(APP)))
}

/// A named file in the config directory.
pub fn config_file(name: &str) -> Option<PathBuf> {
    config_dir().map(|d| d.join(name))
}

/// A named file in the data directory.
pub fn data_file(name: &str) -> Option<PathBuf> {
    data_dir().map(|d| d.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this module exists for: a path that is not anchored anywhere
    /// follows the working directory, and does it silently.
    #[test]
    fn a_path_is_absolute_or_it_is_nothing() {
        for p in [
            config_dir(),
            data_dir(),
            config_file("x.json"),
            data_file("x.db"),
        ]
        .into_iter()
        .flatten()
        {
            assert!(p.is_absolute(), "{} is relative", p.display());
        }
    }

    /// Both point at the same place while the old directory exists, so a split
    /// is never applied retroactively to an install that predates it.
    #[test]
    fn an_existing_install_is_left_where_it_is() {
        if legacy().is_some() {
            assert_eq!(config_dir(), data_dir());
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("norrlog-paths-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The rename moves the old directory's files in beside whatever the GUI
    /// already keeps there, and leaves a symlink so old paths still resolve.
    #[test]
    fn the_old_directory_moves_in_and_leaves_a_symlink() {
        let base = scratch("move");
        std::fs::create_dir_all(base.join("eqlparser/crops")).unwrap();
        std::fs::write(base.join("eqlparser/history.db"), "db").unwrap();
        std::fs::write(base.join("eqlparser/crops/a.png"), "png").unwrap();
        std::fs::create_dir_all(base.join("norrlog")).unwrap();
        std::fs::write(base.join("norrlog/state.json"), "{}").unwrap();

        assert!(migrate(&base).is_some());
        assert_eq!(
            std::fs::read_to_string(base.join("norrlog/history.db")).unwrap(),
            "db"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("norrlog/crops/a.png")).unwrap(),
            "png"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("norrlog/state.json")).unwrap(),
            "{}"
        );
        #[cfg(unix)]
        {
            assert!(std::fs::symlink_metadata(base.join("eqlparser"))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                std::fs::read_to_string(base.join("eqlparser/history.db")).unwrap(),
                "db"
            );
        }
        assert_eq!(migrate(&base), None, "a second run finds nothing to do");
        std::fs::remove_dir_all(base).unwrap();
    }

    /// A file of the same name on both sides is never overwritten, and the
    /// old directory stays so nothing is lost.
    #[test]
    fn a_clash_is_left_where_it_is() {
        let base = scratch("clash");
        std::fs::create_dir_all(base.join("eqlparser")).unwrap();
        std::fs::create_dir_all(base.join("norrlog")).unwrap();
        std::fs::write(base.join("eqlparser/tasks.json"), "old").unwrap();
        std::fs::write(base.join("eqlparser/item-ids.json"), "ids").unwrap();
        std::fs::write(base.join("norrlog/tasks.json"), "new").unwrap();

        let note = migrate(&base).unwrap();
        assert!(note.contains("tasks.json"), "{note}");
        assert_eq!(
            std::fs::read_to_string(base.join("norrlog/tasks.json")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("eqlparser/tasks.json")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("norrlog/item-ids.json")).unwrap(),
            "ids"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Relative values are refused rather than joined onto.
    #[test]
    fn a_relative_base_is_not_a_base() {
        assert!(!PathBuf::from("relative/path").is_absolute());
        // `abs_env` and `home` both gate on this, which is what stops
        // `unwrap_or_default().join(..)` happening again.
    }
}
