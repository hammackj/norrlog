//! Zones, and the variants a zone comes in.
//!
//! # One place, many rooms
//!
//! The client writes a single string for where you are, and in EverQuest
//! Legends that string carries three separate facts at once:
//!
//! ```text
//! Nagafen's Lair                      the place itself, out in the world
//! Nagafen's Lair 1 (Awakened)         a group instance of it, tier 1
//! Nagafen's Lair - Solo 3 (Fused)     a solo instance of it, tier 3
//! ```
//!
//! Left as one string those are three unrelated zones, and no query can ask
//! "does this drop in Nagafen's Lair at all" or "how does its loot change with
//! tier". Split apart they are one **zone** with several **variants**, which is
//! how a reference page wants to read: a page per place, and a matrix of mode
//! against tier beneath it.
//!
//! Across two characters' logs, 124 distinct zone strings collapse to 62
//! zones. Forty-three have a single form — the outdoor and city zones both
//! games share — while The Plane of Hate has all ten.
//!
//! # Classic EverQuest
//!
//! Has no instancing, so every zone there is one [`Mode::Open`] variant at
//! difficulty 0 and the same model covers it without a special case. The game
//! tag on the variant is what keeps the two apart where a zone exists in both.
//!
//! # What cannot be known
//!
//! A group instance at tier 0, if such a thing exists, is written exactly like
//! the open world zone. Nothing in the string distinguishes them, so it is read
//! as open rather than guessed at.

use crate::game::Game;
use serde::{Deserialize, Serialize};

/// The tier names the client uses, in order. The number and the name always
/// agree in every zone observed, so a disagreement means the naming has
/// changed and is worth surfacing rather than absorbing.
pub const TIER_NAMES: &[(u8, &str)] = &[
    (1, "Awakened"),
    (2, "Adaptive"),
    (3, "Fused"),
    (4, "Refined"),
];

/// The highest instance tier the client uses.
pub const MAX_TIER: u8 = 4;

/// How a zone is being played.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The world zone, shared with everyone. The only mode classic EverQuest
    /// has.
    #[default]
    Open,
    /// A solo instance.
    Solo,
    /// A multiplayer instance.
    Group,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Open => "open",
            Mode::Solo => "solo",
            Mode::Group => "group",
        }
    }

    /// Read back what [`Mode::as_str`] wrote.
    ///
    /// Returns `None` rather than defaulting to `Open`: a stored mode nobody
    /// recognises means the writer and the reader disagree, and silently
    /// filing an instance as the open world would corrupt the identity every
    /// creature and drop is keyed on.
    pub fn from_str(s: &str) -> Option<Mode> {
        match s {
            "open" => Some(Mode::Open),
            "solo" => Some(Mode::Solo),
            "group" => Some(Mode::Group),
            _ => None,
        }
    }
}

/// One variant of one zone: where it is, how it is being played, and how hard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    /// The place, with the mode and tier markers removed. This is the identity
    /// a reference page is built on.
    pub name: String,
    pub mode: Mode,
    /// 0 for a zone with no tier marker.
    pub difficulty: u8,
    /// The client's name for the tier, kept verbatim.
    pub modifier: Option<String>,
    /// Exactly what the client wrote, so a row can be traced back to the line
    /// that produced it rather than only to our reading of it.
    pub label: String,
    /// A parenthesised suffix that did not parse as a tier.
    ///
    /// Every suffix observed is a tier, so this being set means the naming has
    /// changed. Recorded rather than folded into difficulty 0, because a new
    /// kind of instance quietly filed as open world is a mistake nobody would
    /// notice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unrecognised_suffix: Option<String>,
}

impl Zone {
    /// Split a zone string into the place, the mode and the tier.
    pub fn parse(label: &str) -> Zone {
        let raw = label.trim();
        let mut rest = raw;
        let mut difficulty = 0;
        let mut modifier = None;
        let mut unrecognised = None;

        // The tier marker is last: " <n> (<Name>)".
        if let Some(open) = rest.rfind(" (") {
            if rest.ends_with(')') {
                let inside = &rest[open + 2..rest.len() - 1];
                let before = rest[..open].trim_end();
                match before.rsplit(' ').next().and_then(|t| t.parse::<u8>().ok()) {
                    Some(tier) if tier <= MAX_TIER && !inside.is_empty() => {
                        difficulty = tier;
                        modifier = Some(inside.to_string());
                        // Drop the digit as well as the parenthesis.
                        rest = before[..before.len() - before.rsplit(' ').next().unwrap().len()]
                            .trim_end();
                    }
                    _ => unrecognised = Some(inside.to_string()),
                }
            }
        }

        // The mode marker sits between the place and the tier.
        let mode = if let Some(stripped) = rest.strip_suffix(" - Solo") {
            rest = stripped;
            Mode::Solo
        } else if difficulty > 0 {
            // An instance that is not solo is a group one. A group instance at
            // tier 0 would be written exactly like the open zone, so it cannot
            // be told apart and is not guessed at.
            Mode::Group
        } else {
            Mode::Open
        };

        Zone {
            name: rest.trim().to_string(),
            mode,
            difficulty,
            modifier,
            label: raw.to_string(),
            unrecognised_suffix: unrecognised,
        }
    }

    /// Whether the tier number and the client's name for it agree.
    ///
    /// They always have. A `false` here means the game renamed a tier, and
    /// anything keyed on difficulty should be re-checked before it is trusted.
    pub fn tier_name_agrees(&self) -> bool {
        // A suffix we could not read is exactly the case this exists to catch,
        // so it can never be "consistent" — untiered and unparsed both leave
        // difficulty at 0, and only this distinguishes them.
        if self.unrecognised_suffix.is_some() {
            return false;
        }
        match (&self.modifier, self.difficulty) {
            (None, 0) => true,
            (Some(name), d) => TIER_NAMES
                .iter()
                .any(|(tier, known)| *tier == d && known.eq_ignore_ascii_case(name)),
            _ => false,
        }
    }

    /// Whether this is an instance rather than the world zone.
    pub fn is_instance(&self) -> bool {
        self.mode != Mode::Open
    }

    /// The key a variant is stored under: the place, the game, the mode and
    /// the tier.
    pub fn variant_key(&self, game: Game) -> (String, &'static str, &'static str, u8) {
        (
            self.name.to_lowercase(),
            game.slug(),
            self.mode.as_str(),
            self.difficulty,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three shapes the client writes, from a real log.
    #[test]
    fn one_place_three_variants() {
        let open = Zone::parse("Nagafen's Lair");
        assert_eq!(open.name, "Nagafen's Lair");
        assert_eq!((open.mode, open.difficulty), (Mode::Open, 0));
        assert!(!open.is_instance());

        let group = Zone::parse("Nagafen's Lair 1 (Awakened)");
        assert_eq!(group.name, "Nagafen's Lair");
        assert_eq!((group.mode, group.difficulty), (Mode::Group, 1));
        assert_eq!(group.modifier.as_deref(), Some("Awakened"));

        let solo = Zone::parse("Nagafen's Lair - Solo 3 (Fused)");
        assert_eq!(solo.name, "Nagafen's Lair");
        assert_eq!((solo.mode, solo.difficulty), (Mode::Solo, 3));

        // All three are the same place, which is the entire point.
        assert_eq!(open.name, group.name);
        assert_eq!(open.name, solo.name);
    }

    /// Solo at tier 0 is a real variant and must not collapse into the open
    /// world zone.
    #[test]
    fn solo_without_a_tier_is_still_an_instance() {
        let z = Zone::parse("The Plane of Hate - Solo");
        assert_eq!(z.name, "The Plane of Hate");
        assert_eq!((z.mode, z.difficulty), (Mode::Solo, 0));
        assert!(z.is_instance());
        assert_ne!(
            z.variant_key(Game::EverQuestLegends),
            Zone::parse("The Plane of Hate").variant_key(Game::EverQuestLegends)
        );
    }

    /// The article is part of the name the client writes; stripping it would
    /// invent a naming convention the game does not have.
    #[test]
    fn the_article_is_kept() {
        assert_eq!(Zone::parse("The Plane of Fear - Solo").name, "The Plane of Fear");
        assert_eq!(Zone::parse("The Ocean of Tears").name, "The Ocean of Tears");
    }

    /// Every tier seen in two characters' logs, and the names that go with
    /// them.
    #[test]
    fn every_observed_tier_parses() {
        for (tier, name) in TIER_NAMES {
            let label = format!("Innothule Swamp {tier} ({name})");
            let z = Zone::parse(&label);
            assert_eq!(z.name, "Innothule Swamp", "from {label}");
            assert_eq!(z.difficulty, *tier);
            assert_eq!(z.mode, Mode::Group);
            assert!(z.tier_name_agrees(), "{label} should be self-consistent");
            assert_eq!(z.label, label, "the client's string is kept verbatim");
        }
    }

    /// A zone whose name merely contains a bracket is not a tier marker, and a
    /// tier we cannot read must be reported rather than silently become 0.
    #[test]
    fn an_unreadable_suffix_is_flagged_not_swallowed() {
        let z = Zone::parse("Somewhere New (Ascendant)");
        assert_eq!(z.difficulty, 0);
        assert_eq!(z.unrecognised_suffix.as_deref(), Some("Ascendant"));
        assert!(
            !z.tier_name_agrees(),
            "an unread suffix must not pass as a consistent untiered zone"
        );
        // The name is left whole rather than half-stripped by a guess.
        assert_eq!(z.name, "Somewhere New (Ascendant)");
    }

    /// A tier beyond the known range is equally suspect.
    #[test]
    fn a_tier_out_of_range_is_not_accepted() {
        let z = Zone::parse("Innothule Swamp 9 (Unknown)");
        assert_eq!(z.difficulty, 0);
        assert_eq!(z.unrecognised_suffix.as_deref(), Some("Unknown"));
    }

    /// Every zone string two characters' logs actually produced.
    ///
    /// Hand-written cases only cover the shapes someone thought of; this
    /// covers the shapes the game produced. It also pins the collapse: 124
    /// strings are 62 places, and a change that quietly split or merged them
    /// would break here rather than in a database six months from now.
    #[test]
    fn every_observed_zone_string_parses() {
        const OBSERVED: &str = include_str!("../assets/zones-observed.txt");
        let labels: Vec<&str> = OBSERVED
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        assert_eq!(labels.len(), 124, "the fixture itself should be intact");

        let mut places = std::collections::BTreeSet::new();
        let mut modes = std::collections::BTreeMap::new();
        for label in &labels {
            let z = Zone::parse(label);
            assert!(!z.name.is_empty(), "{label} parsed to an empty place");
            assert!(
                z.unrecognised_suffix.is_none(),
                "{label} has a suffix the parser does not understand"
            );
            assert!(z.tier_name_agrees(), "{label} disagrees with its own tier name");
            assert!(
                !z.name.contains(" - Solo") && !z.name.ends_with(')'),
                "{label} left a marker behind in the place name: {}",
                z.name
            );
            places.insert(z.name.clone());
            *modes.entry(z.mode).or_insert(0) += 1;
        }

        assert_eq!(places.len(), 62, "124 strings are 62 places");
        assert_eq!(modes[&Mode::Open], 61);
        assert_eq!(modes[&Mode::Group], 38);
        assert_eq!(modes[&Mode::Solo], 25);

        // The place with every variant: open, plus solo and group at each tier.
        let hate: Vec<Zone> = labels
            .iter()
            .map(|l| Zone::parse(l))
            .filter(|z| z.name == "The Plane of Hate")
            .collect();
        assert_eq!(hate.len(), 10, "one open, five solo, four group");
    }

    /// The mode has to survive a round trip through storage, and an
    /// unrecognised one must not quietly become the open world.
    #[test]
    fn a_mode_round_trips_and_an_unknown_one_is_refused() {
        for mode in [Mode::Open, Mode::Solo, Mode::Group] {
            assert_eq!(Mode::from_str(mode.as_str()), Some(mode));
        }
        assert_eq!(Mode::from_str("raid"), None);
        assert_eq!(Mode::from_str("Solo"), None, "as_str is lowercase");
    }

    /// Classic EverQuest has no instancing, so its zones are one open variant
    /// each and need no special handling.
    #[test]
    fn classic_zones_need_no_special_case() {
        let z = Zone::parse("East Commonlands");
        assert_eq!((z.mode, z.difficulty), (Mode::Open, 0));
        assert!(z.tier_name_agrees());
        assert_eq!(
            z.variant_key(Game::EverQuest),
            ("east commonlands".to_string(), "everquest", "open", 0)
        );
        // The same place in the two games is two variants, never one.
        assert_ne!(
            z.variant_key(Game::EverQuest),
            z.variant_key(Game::EverQuestLegends)
        );
    }
}
