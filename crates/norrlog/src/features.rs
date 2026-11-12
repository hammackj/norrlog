//! Which game-specific parts the followed game has.
//!
//! The daemon says, in its hello, from the game's profile
//! (`eqlparser_core::profile`): Legends has the Plane of Sky, weekly
//! lockouts and motes; live EverQuest has none of them. Views ask here
//! rather than each holding the hello, and hide what the game does not have.

use std::sync::RwLock;

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
    FEATURES.read().map_or(true, |f| f.as_ref().is_none_or(|f| f.iter().any(|x| x == name)))
}

/// The feature a tab needs, if any.
pub fn of_tab(tab: &str) -> Option<&'static str> {
    match tab {
        "sky" => Some("sky"),
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
        super::set(None);
    }
}
