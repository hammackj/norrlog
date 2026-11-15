//! Which game-specific parts the followed game has.
//!
//! The daemon says, in its hello, from the game's profile
//! (`norrlog_core::profile`): Legends has the Plane of Sky, weekly
//! lockouts and motes; live EverQuest has none of them. Views ask here
//! rather than each holding the hello, and hide what the game does not have.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// `None` until a daemon is attached, and from a daemon too old to say:
/// both read as every feature, as all logs were read as Legends before.
static FEATURES: RwLock<Option<Vec<String>>> = RwLock::new(None);

pub fn set(features: Option<Vec<String>>) {
    if let Ok(mut f) = FEATURES.write() {
        *f = features;
    }
}

/// Whether the followed game has `name` (`sky`, `lockouts`, `motes`).
pub fn has(name: &str) -> bool {
    FEATURES.read().map_or(true, |f| {
        f.as_ref().is_none_or(|f| f.iter().any(|x| x == name))
    })
}

/// Whether the player wants Plane of Sky quests followed (Settings, Quest
/// Tracker). On unless turned off.
static SKY_WANTED: AtomicBool = AtomicBool::new(true);

pub fn set_sky_wanted(on: bool) {
    SKY_WANTED.store(on, Ordering::Relaxed);
}

pub fn sky_wanted() -> bool {
    SKY_WANTED.load(Ordering::Relaxed)
}

/// Whether Plane of Sky quests are shown and asked for: the game has them
/// and the player has not turned them off.
pub fn sky_tracked() -> bool {
    has("sky") && sky_wanted()
}

/// The feature an overlay panel needs, if any.
pub fn of_panel(panel: &str) -> Option<&'static str> {
    match panel {
        "motes" => Some("motes"),
        "sky" => Some("sky"),
        _ => None,
    }
}

/// Whether the followed game has what `panel` shows.
pub fn panel_shown(panel: &str) -> bool {
    of_panel(panel).is_none_or(has) && (panel != "sky" || sky_wanted())
}

/// The feature a tab needs, if any.
pub fn of_tab(tab: &str) -> Option<&'static str> {
    match tab {
        // The Quest Tracker holds only Plane of Sky quests so far, which
        // Legends has and live EverQuest does not.
        "quests" => Some("sky"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_reads_as_all_and_a_list_hides_the_rest() {
        super::set(None);
        assert!(super::has("sky"));
        super::set(Some(vec!["motes".into()]));
        assert!(super::has("motes") && !super::has("sky") && !super::has("lockouts"));
        assert!(super::panel_shown("motes") && !super::panel_shown("sky"));
        assert!(super::panel_shown("meter"));
        super::set(Some(vec![]));
        assert!(!super::panel_shown("motes") && super::panel_shown("others"));
        super::set(None);

        // One test, as both read the same globals: turning Plane of Sky off
        // hides its panel but not the Quest Tracker tab.
        super::set_sky_wanted(false);
        assert!(!super::sky_tracked() && !super::panel_shown("sky"));
        assert!(super::of_tab("quests").is_none_or(super::has));
        super::set_sky_wanted(true);
        assert!(super::sky_tracked() && super::panel_shown("sky"));
    }
}
