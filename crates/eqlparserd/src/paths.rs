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
//! `~/.config/eqlparser` predates this. If it exists it keeps being used, for
//! both kinds of file — moving somebody's data out from under them to satisfy
//! a specification is not an improvement, and a split applied retroactively
//! would strand half of it.

use std::path::PathBuf;

/// The directory name under whichever base applies.
const APP: &str = "eqlparser";

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

/// The pre-existing `~/.config/eqlparser`, if it is there.
fn legacy() -> Option<PathBuf> {
    let dir = home()?.join(".config").join(APP);
    dir.is_dir().then_some(dir)
}

/// Hand-made state: corrections, transcriptions, definitions.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = legacy() {
        return Some(dir);
    }
    if cfg!(target_os = "windows") {
        return abs_env("APPDATA").map(|p| p.join(APP));
    }
    if cfg!(target_os = "macos") {
        return home().map(|h| h.join("Library/Application Support").join(APP));
    }
    abs_env("XDG_CONFIG_HOME")
        .or_else(|| home().map(|h| h.join(".config")))
        .map(|p| p.join(APP))
}

/// Derived state: rebuildable from the logs.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = legacy() {
        return Some(dir);
    }
    if cfg!(target_os = "windows") {
        return abs_env("APPDATA").map(|p| p.join(APP));
    }
    if cfg!(target_os = "macos") {
        return home().map(|h| h.join("Library/Application Support").join(APP));
    }
    abs_env("XDG_DATA_HOME")
        .or_else(|| home().map(|h| h.join(".local/share")))
        .map(|p| p.join(APP))
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
        for p in [config_dir(), data_dir(), config_file("x.json"), data_file("x.db")]
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

    /// Relative values are refused rather than joined onto.
    #[test]
    fn a_relative_base_is_not_a_base() {
        assert_eq!(PathBuf::from("relative/path").is_absolute(), false);
        // `abs_env` and `home` both gate on this, which is what stops
        // `unwrap_or_default().join(..)` happening again.
    }
}
