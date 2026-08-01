//! Tradeskill tracking: combines, skill-ups and component use.
//!
//! # Attributing a combine to a tradeskill
//!
//! The client never says which tradeskill a combine belongs to. It writes only
//! the outcome:
//!
//! ```text
//! You have fashioned the items together to create something new: Potion of Accuracy.
//! You lacked the skills to fashion Potion of Accuracy.
//! ```
//!
//! What *does* name the skill is the skill-up that sometimes follows:
//!
//! ```text
//! You lacked the skills to fashion Potion of Accuracy.
//! You have become better at Alchemy! (52)
//! ```
//!
//! So attribution works forwards: a combine is held briefly, and a skill-up
//! arriving just after names its tradeskill. That association is then
//! remembered per item, letting the many combines that yield no skill-up be
//! attributed too. This matters because skill-ups become rare as a skill caps,
//! which is exactly when most combines happen.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How long after a combine a skill-up is still taken to belong to it.
const SKILL_ATTRIBUTION_WINDOW: i64 = 5;

/// Skills that are tradeskills rather than combat or utility skills.
///
/// Curated because the client does not distinguish them; everything else that
/// reports a skill-up ("Bash", "Meditate") is ignored here.
pub const TRADESKILLS: &[&str] = &[
    "Alchemy",
    "Baking",
    "Blacksmithing",
    "Brewing",
    "Fletching",
    "Jewelry Making",
    "Make Poison",
    "Pottery",
    "Research",
    "Tailoring",
    "Tinkering",
];

pub fn is_tradeskill(skill: &str) -> bool {
    TRADESKILLS.iter().any(|s| s.eq_ignore_ascii_case(skill))
}

/// Recipes whose tradeskill cannot be inferred from the log.
///
/// Attribution normally comes from an adjacent skill-up, but a recipe that
/// never skills up leaves no evidence at all — and component-based inference
/// does not help either, because reagents taken from inventory rather than the
/// depot produce no "Consumed" line. Such recipes are seeded here.
///
/// Extend this list, or call [`TradeskillTracker::set_item_skill`] at runtime.
pub const KNOWN_RECIPE_SKILLS: &[(&str, &str)] = &[("Kilva's Skin of Flame", "Alchemy")];

/// Progress in one tradeskill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillProgress {
    pub skill: String,
    /// Highest value the client has reported.
    pub current: u32,
    /// Lowest value seen, i.e. where this log picks the skill up.
    pub first_seen_at: u32,
    /// Number of skill-up messages.
    pub ups: u32,
    pub first_up: i64,
    pub last_up: i64,
}

impl SkillProgress {
    /// Skill points gained within this log.
    pub fn gained(&self) -> u32 {
        self.current.saturating_sub(self.first_seen_at)
    }

    /// Skill-ups per hour over the period they occurred in.
    pub fn ups_per_hour(&self) -> f64 {
        let span = (self.last_up - self.first_up).max(1);
        self.ups as f64 * 3600.0 / span as f64
    }
}

/// Attempts at one recipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CraftRecord {
    pub item: String,
    /// The tradeskill this item belongs to, once known.
    pub skill: Option<String>,
    pub successes: u32,
    pub failures: u32,
    pub last_attempt: i64,
    /// Skill-ups that happened on an attempt at this item.
    pub skill_ups: u32,
}

impl CraftRecord {
    pub fn attempts(&self) -> u32 {
        self.successes + self.failures
    }

    pub fn success_rate(&self) -> f64 {
        if self.attempts() == 0 {
            0.0
        } else {
            self.successes as f64 / self.attempts() as f64
        }
    }
}

/// A component consumed by combines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentUse {
    pub item: String,
    pub used: u64,
    /// Stock reported by the client after the most recent use.
    pub remaining: Option<u64>,
    pub last_used: i64,
}

/// Everything the tradeskill tab shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeskillReport {
    pub skills: Vec<SkillProgress>,
    pub crafts: Vec<CraftRecord>,
    pub components: Vec<ComponentUse>,
    pub recipes_learned: Vec<String>,
    pub total_successes: u32,
    pub total_failures: u32,
}

/// A combine awaiting the skill-up that would name its tradeskill.
#[derive(Debug, Clone)]
struct PendingCraft {
    at: i64,
    item: String,
}

/// Accumulates tradeskill activity.
#[derive(Debug)]
pub struct TradeskillTracker {
    skills: HashMap<String, SkillProgress>,
    crafts: HashMap<String, CraftRecord>,
    components: HashMap<String, ComponentUse>,
    recipes: Vec<String>,
    pending: Option<PendingCraft>,
    /// Learned item -> tradeskill, from combines that did produce a skill-up.
    item_skill: HashMap<String, String>,
    /// Mappings declared rather than inferred. A skill-up that merely happens
    /// to land near one of these combines must not relabel it.
    declared: std::collections::HashSet<String>,
}

impl Default for TradeskillTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TradeskillTracker {
    pub fn new() -> Self {
        let mut t = TradeskillTracker {
            skills: HashMap::new(),
            crafts: HashMap::new(),
            components: HashMap::new(),
            recipes: Vec::new(),
            pending: None,
            item_skill: HashMap::new(),
            declared: std::collections::HashSet::new(),
        };
        for (item, skill) in KNOWN_RECIPE_SKILLS {
            t.set_item_skill(item, skill);
        }
        t
    }

    /// Declare which tradeskill a recipe belongs to.
    ///
    /// Takes precedence over inference, and applies retroactively to attempts
    /// already recorded.
    pub fn set_item_skill(&mut self, item: &str, skill: &str) {
        let key = item.to_lowercase();
        self.declared.insert(key.clone());
        self.item_skill.insert(key.clone(), skill.to_string());
        if let Some(c) = self.crafts.get_mut(&key) {
            c.skill = Some(skill.to_string());
        }
    }

    /// Record a combine attempt.
    pub fn on_craft(&mut self, timestamp: i64, item: &str, success: bool) {
        let key = item.to_lowercase();
        let known = self.item_skill.get(&key).cloned();
        let e = self.crafts.entry(key.clone()).or_insert_with(|| CraftRecord {
            item: item.to_string(),
            skill: None,
            successes: 0,
            failures: 0,
            last_attempt: timestamp,
            skill_ups: 0,
        });
        if success {
            e.successes += 1;
        } else {
            e.failures += 1;
        }
        e.last_attempt = timestamp;
        if e.skill.is_none() {
            e.skill = known;
        }
        // Hold it briefly: a skill-up arriving next names the tradeskill.
        self.pending = Some(PendingCraft {
            at: timestamp,
            item: key,
        });
    }

    /// Record a skill-up. Ignored unless it is a tradeskill.
    pub fn on_skill_up(&mut self, timestamp: i64, skill: &str, value: u32) {
        if !is_tradeskill(skill) {
            return;
        }
        let e = self
            .skills
            .entry(skill.to_lowercase())
            .or_insert_with(|| SkillProgress {
                skill: skill.to_string(),
                current: value,
                first_seen_at: value,
                ups: 0,
                first_up: timestamp,
                last_up: timestamp,
            });
        e.current = e.current.max(value);
        e.first_seen_at = e.first_seen_at.min(value);
        e.ups += 1;
        e.last_up = timestamp;

        // Name the tradeskill of the combine that just happened, and remember
        // the association for future attempts at the same item.
        if let Some(p) = self.pending.clone() {
            // A declared mapping is authoritative; an adjacent skill-up may
            // simply belong to a different combine done around the same time.
            if timestamp - p.at <= SKILL_ATTRIBUTION_WINDOW && !self.declared.contains(&p.item) {
                self.item_skill.insert(p.item.clone(), skill.to_string());
                if let Some(c) = self.crafts.get_mut(&p.item) {
                    c.skill = Some(skill.to_string());
                    c.skill_ups += 1;
                }
            }
        }
    }

    /// Record a component consumed by a combine.
    pub fn on_component(&mut self, timestamp: i64, item: &str, qty: u64, remaining: Option<u64>) {
        let e = self
            .components
            .entry(item.to_lowercase())
            .or_insert_with(|| ComponentUse {
                item: item.to_string(),
                used: 0,
                remaining: None,
                last_used: timestamp,
            });
        e.used += qty;
        e.remaining = remaining;
        e.last_used = timestamp;
    }

    pub fn on_recipe_learned(&mut self, recipe: &str) {
        if !self.recipes.iter().any(|r| r == recipe) {
            self.recipes.push(recipe.to_string());
        }
    }

    /// Build the report, newest activity first.
    pub fn report(&self) -> TradeskillReport {
        let mut skills: Vec<SkillProgress> = self.skills.values().cloned().collect();
        skills.sort_by_key(|s| std::cmp::Reverse(s.current));

        let mut crafts: Vec<CraftRecord> = self
            .crafts
            .values()
            .cloned()
            .map(|mut c| {
                // Fill in any attribution learned after the first attempt.
                if c.skill.is_none() {
                    c.skill = self.item_skill.get(&c.item.to_lowercase()).cloned();
                }
                c
            })
            .collect();
        crafts.sort_by_key(|c| std::cmp::Reverse(c.attempts()));

        let mut components: Vec<ComponentUse> = self.components.values().cloned().collect();
        components.sort_by_key(|c| std::cmp::Reverse(c.used));

        TradeskillReport {
            total_successes: crafts.iter().map(|c| c.successes).sum(),
            total_failures: crafts.iter().map(|c| c.failures).sum(),
            skills,
            crafts,
            components,
            recipes_learned: self.recipes.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tradeskills_are_tracked() {
        assert!(is_tradeskill("Alchemy"));
        assert!(is_tradeskill("blacksmithing"));
        assert!(!is_tradeskill("Bash"));
        assert!(!is_tradeskill("Meditate"));

        let mut t = TradeskillTracker::new();
        t.on_skill_up(100, "Bash", 50);
        assert!(t.report().skills.is_empty());
    }

    #[test]
    fn skill_progress_tracks_range_and_count() {
        let mut t = TradeskillTracker::new();
        t.on_skill_up(0, "Alchemy", 10);
        t.on_skill_up(1800, "Alchemy", 11);
        t.on_skill_up(3600, "Alchemy", 12);
        let s = &t.report().skills[0];
        assert_eq!(s.current, 12);
        assert_eq!(s.first_seen_at, 10);
        assert_eq!(s.gained(), 2);
        assert_eq!(s.ups, 3);
        assert!((s.ups_per_hour() - 3.0).abs() < 0.01);
    }

    /// The combine never names its tradeskill; the skill-up that follows does.
    #[test]
    fn a_following_skill_up_names_the_tradeskill() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Potion of Accuracy", false);
        t.on_skill_up(101, "Alchemy", 52);
        let c = &t.report().crafts[0];
        assert_eq!(c.item, "Potion of Accuracy");
        assert_eq!(c.skill.as_deref(), Some("Alchemy"));
        assert_eq!(c.failures, 1);
        assert_eq!(c.skill_ups, 1);
    }

    /// Skill-ups get rare as a skill caps, so most combines have none. The
    /// association learned earlier must carry them.
    #[test]
    fn learned_association_attributes_later_combines() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Potion of Accuracy", false);
        t.on_skill_up(101, "Alchemy", 52);
        // Many later attempts with no skill-up at all.
        for i in 1..=5 {
            t.on_craft(200 + i * 10, "Potion of Accuracy", true);
        }
        let c = &t.report().crafts[0];
        assert_eq!(c.attempts(), 6);
        assert_eq!(c.successes, 5);
        assert_eq!(
            c.skill.as_deref(),
            Some("Alchemy"),
            "attribution must persist once learned"
        );
        assert!((c.success_rate() - 5.0 / 6.0).abs() < 1e-9);
    }

    /// A skill-up long after a combine is unrelated to it.
    #[test]
    fn a_distant_skill_up_does_not_attribute() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Metal Bits", true);
        t.on_skill_up(9999, "Blacksmithing", 18);
        assert_eq!(t.report().crafts[0].skill, None);
    }

    #[test]
    fn components_accumulate_and_track_stock() {
        let mut t = TradeskillTracker::new();
        t.on_component(100, "Fenugreek", 1, Some(314));
        t.on_component(200, "Fenugreek", 2, Some(312));
        t.on_component(300, "Blue Vervain Bulb", 1, Some(99));
        let r = t.report();
        let fen = r.components.iter().find(|c| c.item == "Fenugreek").unwrap();
        assert_eq!(fen.used, 3);
        assert_eq!(fen.remaining, Some(312), "latest stock wins");
        // Ordered by most used.
        assert_eq!(r.components[0].item, "Fenugreek");
    }

    #[test]
    fn recipes_are_recorded_once() {
        let mut t = TradeskillTracker::new();
        t.on_recipe_learned("Potion of Accuracy");
        t.on_recipe_learned("Potion of Accuracy");
        t.on_recipe_learned("Elixir of Greater Concentration");
        assert_eq!(t.report().recipes_learned.len(), 2);
    }

    /// Some recipes never skill up and consume nothing from the depot, so the
    /// log holds no evidence of their tradeskill at all. Those are declared.
    #[test]
    fn declared_recipes_are_attributed_without_evidence() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Kilva's Skin of Flame", true);
        let c = &t.report().crafts[0];
        assert_eq!(
            c.skill.as_deref(),
            Some("Alchemy"),
            "seeded mapping should apply with no skill-up at all"
        );
    }

    /// A declaration must not be overwritten by a skill-up that merely happens
    /// to land nearby.
    #[test]
    fn a_declaration_outranks_an_adjacent_skill_up() {
        let mut t = TradeskillTracker::new();
        t.set_item_skill("Mystery Widget", "Alchemy");
        t.on_craft(100, "Mystery Widget", false);
        t.on_skill_up(101, "Blacksmithing", 20);
        let c = t
            .report()
            .crafts
            .into_iter()
            .find(|c| c.item == "Mystery Widget")
            .unwrap();
        assert_eq!(c.skill.as_deref(), Some("Alchemy"));
    }

    /// Declaring after the fact must fix attempts already recorded.
    #[test]
    fn declaring_applies_retroactively() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Unknown Thing", true);
        assert_eq!(t.report().crafts[0].skill, None);
        t.set_item_skill("Unknown Thing", "Pottery");
        let c = t
            .report()
            .crafts
            .into_iter()
            .find(|c| c.item == "Unknown Thing")
            .unwrap();
        assert_eq!(c.skill.as_deref(), Some("Pottery"));
    }

    #[test]
    fn totals_sum_across_recipes() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "A", true);
        t.on_craft(200, "A", false);
        t.on_craft(300, "B", true);
        let r = t.report();
        assert_eq!(r.total_successes, 2);
        assert_eq!(r.total_failures, 1);
    }
}
