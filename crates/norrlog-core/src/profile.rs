//! What differs between the games, in one place.
//!
//! EverQuest and EverQuest Legends share a client and a log format, and most
//! of this crate reads both the same way. The rules that do differ live here
//! instead of as Legends assumptions scattered through the code: ask
//! [`profile`] for the game a log belongs to, rather than testing the game
//! by hand at each place that cares.
//!
//! A new difference is a new field here and one line per game, so what the
//! two games disagree about can be read off this file.
//!
//! [`Game::Unknown`] gets the Legends profile. That is what every log was
//! read as before the games were told apart, so an install nobody could
//! identify keeps behaving the way it always has.

use crate::game::Game;

/// One game's rules, as far as the parser needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// Which game this is. `Unknown` maps to Legends' rules but keeps its
    /// own tag, so nothing it records is filed under a game it may not be.
    pub game: Game,
    /// The highest level a class reaches. A capped class never emits another
    /// level-up message, which the class tracker has to know.
    pub level_cap: u32,
    /// How many classes a character holds at once: three in Legends' loadouts,
    /// one at a time in EverQuest. One at a time, not one for life: a live
    /// character can switch class, each one levelled on its own.
    pub class_slots: u8,
    /// The level an unplayed class sits at: 10 in Legends, where every class
    /// a character holds starts there. `None` in EverQuest, where there is no
    /// such thing and a level nothing has stated is unknown.
    pub unplayed_level: Option<u32>,
    /// The Plane of Sky quest tracker (Sky tab, the Dashboard's Sky bars).
    pub sky: bool,
    /// Weekly and daily lockouts, with Void-Touched (the Dashboard's grid).
    pub lockouts: bool,
    /// Motes as loot (the Loot tab's Motes view).
    pub motes: bool,
    /// The item and quest site that describes this game. Not used yet: the
    /// daemon still asks eqldb for every game, because eqdb does not answer
    /// eqldb's `/api/item` JSON (it serves an HTML page there).
    pub item_site: &'static str,
}

const LEGENDS: Profile = Profile {
    game: Game::EverQuestLegends,
    level_cap: 50,
    class_slots: 3,
    unplayed_level: Some(10),
    sky: true,
    lockouts: true,
    motes: true,
    item_site: "https://eqldb.hammackj.com",
};

const EVERQUEST: Profile = Profile {
    game: Game::EverQuest,
    level_cap: 130,
    class_slots: 1,
    unplayed_level: None,
    sky: false,
    lockouts: false,
    motes: false,
    item_site: "https://eqdb.hammackj.com",
};

/// The rules for `game`.
pub fn profile(game: Game) -> Profile {
    match game {
        Game::EverQuest => EVERQUEST,
        Game::EverQuestLegends => LEGENDS,
        Game::Unknown => Profile { game: Game::Unknown, ..LEGENDS },
    }
}

impl Profile {
    /// The features this game has, by the names the API and the apps use.
    pub fn features(&self) -> Vec<&'static str> {
        [("sky", self.sky), ("lockouts", self.lockouts), ("motes", self.motes)]
            .into_iter()
            .filter_map(|(name, on)| on.then_some(name))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_game_reads_as_legends_but_keeps_its_tag() {
        let p = profile(Game::Unknown);
        assert_eq!(p.game, Game::Unknown);
        assert_eq!((p.level_cap, p.class_slots), (50, 3));
        assert_eq!(p.features(), ["sky", "lockouts", "motes"]);
    }

    #[test]
    fn live_everquest_is_one_class_to_130_with_none_of_legends_features() {
        let p = profile(Game::EverQuest);
        assert_eq!((p.level_cap, p.class_slots), (130, 1));
        assert!(p.features().is_empty());
        assert_ne!(p.item_site, profile(Game::EverQuestLegends).item_site);
    }
}
