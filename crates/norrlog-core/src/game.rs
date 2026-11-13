//! Which game a piece of data came from.
//!
//! EverQuest and EverQuest Legends share a client, a log format and most of
//! their content, which is exactly why the distinction has to be recorded
//! rather than assumed: two logs are indistinguishable line by line, but a
//! recipe, a merchant price or a drop may not be the same in both. Data
//! gathered now is worth little later if nobody can say which game it
//! describes.
//!
//! Nothing in a log states the game, so it comes from the install being
//! followed, or is declared. When neither answers, the data is tagged
//! [`Game::Unknown`] rather than guessed into one — a wrong tag is worse than
//! an absent one, because it cannot be spotted afterwards.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Serialised as the same slugs [`Game::slug`] returns, spelled out per
/// variant: a derived rename would produce `ever-quest`, and the wire format
/// is shared with the JSON assets and the Python importer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Game {
    #[serde(rename = "everquest")]
    EverQuest,
    #[serde(rename = "everquest-legends")]
    EverQuestLegends,
    #[default]
    #[serde(rename = "unknown")]
    Unknown,
}

impl Game {
    /// The stable slug used in the API, the database and the recipe files.
    pub fn slug(self) -> &'static str {
        match self {
            Game::EverQuest => "everquest",
            Game::EverQuestLegends => "everquest-legends",
            Game::Unknown => "unknown",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Game::EverQuest => "EverQuest",
            Game::EverQuestLegends => "EverQuest Legends",
            Game::Unknown => "unknown game",
        }
    }

    /// Parse a slug, a name, or one of the obvious short forms.
    pub fn parse(s: &str) -> Option<Game> {
        match s.trim().to_lowercase().replace(['-', '_'], " ").as_str() {
            "eq" | "everquest" | "classic" => Some(Game::EverQuest),
            "eql" | "legends" | "everquest legends" | "eq legends" => Some(Game::EverQuestLegends),
            "unknown" => Some(Game::Unknown),
            _ => None,
        }
    }

    /// Infer from an install directory or log path.
    ///
    /// "EverQuest Legends" is checked first because it contains "EverQuest":
    /// testing the other way round would label every Legends install as
    /// classic.
    pub fn from_path(path: &Path) -> Game {
        let p = path.to_string_lossy().to_lowercase();
        if p.contains("everquest legends") || p.contains("everquestlegends") {
            Game::EverQuestLegends
        } else if p.contains("everquest") {
            Game::EverQuest
        } else {
            Game::Unknown
        }
    }

    /// Infer from whichever paths are available, in order of authority, and
    /// stop at the first that answers.
    pub fn detect<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Game {
        for p in paths {
            match Game::from_path(p) {
                Game::Unknown => continue,
                g => return g,
            }
        }
        Game::Unknown
    }
}

impl std::fmt::Display for Game {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The trap: "EverQuest Legends" contains "EverQuest", so the more
    /// specific name has to win.
    #[test]
    fn legends_is_not_mistaken_for_classic() {
        let legends = PathBuf::from(
            "/games/bottles/Legends/drive_c/users/Public/\
             Daybreak Game Company/Installed Games/EverQuest Legends",
        );
        assert_eq!(Game::from_path(&legends), Game::EverQuestLegends);
        assert_eq!(
            Game::from_path(Path::new("C:/EverQuest/Logs/eqlog_Tharn_bertox.txt")),
            Game::EverQuest
        );
    }

    /// A path that says nothing must not be resolved to a guess.
    #[test]
    fn an_unrecognised_path_stays_unknown() {
        assert_eq!(
            Game::from_path(Path::new("/home/x/logs/log.txt")),
            Game::Unknown
        );
        assert_eq!(
            Game::detect([Path::new("/tmp"), Path::new("/var")]),
            Game::Unknown
        );
    }

    /// The first path that can answer wins, so a game directory outranks a log
    /// file kept somewhere else.
    #[test]
    fn detection_takes_the_first_path_that_answers() {
        let paths = [
            Path::new("/some/backup/dir"),
            Path::new("/games/EverQuest Legends"),
            Path::new("/games/EverQuest"),
        ];
        assert_eq!(Game::detect(paths), Game::EverQuestLegends);
    }

    #[test]
    fn slugs_round_trip() {
        for g in [Game::EverQuest, Game::EverQuestLegends, Game::Unknown] {
            assert_eq!(Game::parse(g.slug()), Some(g));
        }
        assert_eq!(Game::parse("EQL"), Some(Game::EverQuestLegends));
        assert_eq!(Game::parse("nonsense"), None);
    }

    /// The JSON assets, the API and the Python importer all speak these
    /// strings, so serde has to agree with [`Game::slug`] exactly — a derived
    /// rename would silently write `ever-quest` instead.
    #[test]
    fn serde_uses_the_same_slugs() {
        for g in [Game::EverQuest, Game::EverQuestLegends, Game::Unknown] {
            let json = serde_json::to_string(&g).unwrap();
            assert_eq!(json, format!("\"{}\"", g.slug()));
            assert_eq!(serde_json::from_str::<Game>(&json).unwrap(), g);
        }
    }
}
