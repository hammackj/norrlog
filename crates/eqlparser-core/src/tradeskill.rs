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
    /// Points bought from an NPC trainer rather than earned by crafting.
    ///
    /// Counted separately because they say nothing about what you made: six
    /// tradeskills on a real character reached 20 purely by paying a trainer,
    /// and reporting that as crafting progress would be a lie about how the
    /// skill was raised.
    #[serde(default)]
    pub trained: u32,
    pub first_up: i64,
    pub last_up: i64,
}

impl SkillProgress {
    /// Points earned by actually practising the skill.
    pub fn from_practice(&self) -> u32 {
        self.ups.saturating_sub(self.trained)
    }

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

/// One skill point, with what earned it.
///
/// Kept individually rather than only as a running total so a skill can be
/// read back as a history — which recipe carried you across which stretch —
/// instead of a single number that says nothing about how you got there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillUpEvent {
    pub at: i64,
    /// The value the skill reached.
    pub value: u32,
    /// The item being combined, when one could be matched.
    pub item: Option<String>,
    /// Bought from a trainer rather than earned.
    pub trained: bool,
}

/// A contiguous stretch of a skill raised by one recipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSegment {
    pub skill: String,
    /// The item, or `None` for a stretch that could not be attributed.
    pub item: Option<String>,
    pub trained: bool,
    /// The skill value on entering the stretch, and on leaving it.
    ///
    /// `from` is one below the first point gained, so a run that took you to
    /// 31 through 92 reads 30 -> 92: the level you were at when you started.
    pub from: u32,
    pub to: u32,
    pub ups: u32,
    /// Attempts at this item within the stretch, and how many worked.
    pub attempts: u32,
    pub successes: u32,
    pub started: i64,
    pub ended: i64,
}

/// Everything the tradeskill tab shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeskillReport {
    pub skills: Vec<SkillProgress>,
    /// Per-skill history, oldest first.
    pub segments: Vec<SkillSegment>,
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
    /// Every skill point in order, per skill.
    timeline: HashMap<String, Vec<SkillUpEvent>>,
    /// Every combine attempt, for counting effort within a stretch.
    attempts: Vec<(i64, String, bool)>,
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
            timeline: HashMap::new(),
            attempts: Vec::new(),
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
        let e = self
            .crafts
            .entry(key.clone())
            .or_insert_with(|| CraftRecord {
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
        self.attempts.push((timestamp, key.clone(), success));
        // Hold it briefly: a skill-up arriving next names the tradeskill.
        self.pending = Some(PendingCraft {
            at: timestamp,
            item: key,
        });
    }

    /// Record a skill-up. Ignored unless it is a tradeskill.
    /// A trainer confirmed a point it just sold you.
    ///
    /// The client writes this *after* the skill-up line, so the point has
    /// already been counted; this reclassifies it rather than adding to it.
    /// Only ever subtracts from the practice tally, never from the total.
    pub fn on_trained(&mut self, skill: &str) {
        if !is_tradeskill(skill) {
            return;
        }
        let key = skill.to_lowercase();
        if let Some(e) = self.skills.get_mut(&key) {
            // Cannot exceed the ups actually seen: a trainer line without a
            // matching skill-up would otherwise push practice below zero.
            if e.trained < e.ups {
                e.trained += 1;
            }
        }
        // The confirmation follows the point, so the most recent one not yet
        // marked is the point that was bought. Any recipe credited to it was
        // coincidence — a combine that happened to be nearby.
        if let Some(events) = self.timeline.get_mut(&key) {
            if let Some(last) = events.iter_mut().rev().find(|e| !e.trained) {
                last.trained = true;
                last.item = None;
            }
        }
    }

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
                trained: 0,
                first_up: timestamp,
                last_up: timestamp,
            });
        e.current = e.current.max(value);
        e.first_seen_at = e.first_seen_at.min(value);
        e.ups += 1;
        e.last_up = timestamp;

        let mut credited: Option<String> = None;

        // Name the tradeskill of the combine that just happened, and remember
        // the association for future attempts at the same item.
        if let Some(p) = self.pending.clone() {
            if timestamp - p.at <= SKILL_ATTRIBUTION_WINDOW {
                let declared = self.declared.contains(&p.item);
                // A declared mapping is authoritative: an adjacent skill-up
                // may belong to a different combine done around the same
                // time, and must not relabel it.
                if !declared {
                    self.item_skill.insert(p.item.clone(), skill.to_string());
                    if let Some(c) = self.crafts.get_mut(&p.item) {
                        c.skill = Some(skill.to_string());
                    }
                }
                // Crediting is a separate question from labelling. A declared
                // recipe still earns its skill-ups — it simply only accepts
                // ones in the skill it was declared to belong to, so a nearby
                // point in an unrelated skill cannot be credited to it.
                let belongs = !declared
                    || self
                        .item_skill
                        .get(&p.item)
                        .is_some_and(|s| s.eq_ignore_ascii_case(skill));
                if belongs {
                    // Carry the display name, not the lookup key: this ends up
                    // in front of the user.
                    if let Some(c) = self.crafts.get_mut(&p.item) {
                        c.skill_ups += 1;
                        credited = Some(c.item.clone());
                    }
                }
            }
        }

        self.timeline
            .entry(skill.to_lowercase())
            .or_default()
            .push(SkillUpEvent {
                at: timestamp,
                value,
                item: credited,
                trained: false,
            });
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
    /// Collapse a skill's point history into stretches, one per recipe.
    ///
    /// Consecutive points earned on the same recipe become a single stretch,
    /// so alternating between two recipes produces alternating stretches
    /// rather than one merged blur — which is the honest reading, since the
    /// order you did them in is what the ranges mean.
    ///
    /// Attempts are counted from the combine log within the stretch's own time
    /// span, so effort is measured over the period the stretch covers rather
    /// than across the whole log.
    fn segments_for(&self, key: &str, name: &str) -> Vec<SkillSegment> {
        let Some(events) = self.timeline.get(key) else {
            return Vec::new();
        };
        let mut out: Vec<SkillSegment> = Vec::new();
        for e in events {
            let same = out.last().is_some_and(|last: &SkillSegment| {
                last.item == e.item && last.trained == e.trained
            });
            if same {
                let last = out.last_mut().unwrap();
                last.to = e.value;
                last.ups += 1;
                last.ended = e.at;
            } else {
                out.push(SkillSegment {
                    skill: name.to_string(),
                    item: e.item.clone(),
                    trained: e.trained,
                    // One below the first point gained: the level you were at
                    // on starting, which is how a range reads naturally.
                    from: e.value.saturating_sub(1),
                    to: e.value,
                    ups: 1,
                    attempts: 0,
                    successes: 0,
                    started: e.at,
                    ended: e.at,
                });
            }
        }

        // Effort is counted over the interval a stretch *covers*, not the span
        // between its first and last skill-up: the combine that earns a point
        // happens before the point is announced, so the attempt that started a
        // stretch always falls outside it. Each stretch therefore takes
        // everything since the previous one ended, which partitions the log
        // without gaps or double counting.
        let mut since = i64::MIN;
        for seg in out.iter_mut() {
            let until = seg.ended;
            if let Some(item) = seg.item.as_ref().map(|i| i.to_lowercase()) {
                for (at, name, success) in &self.attempts {
                    if name == &item && *at > since && *at <= until {
                        seg.attempts += 1;
                        if *success {
                            seg.successes += 1;
                        }
                    }
                }
            }
            since = until;
        }
        out
    }

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

        let segments: Vec<SkillSegment> = self
            .skills
            .iter()
            .flat_map(|(key, p)| self.segments_for(key, &p.skill))
            .collect();

        TradeskillReport {
            total_successes: crafts.iter().map(|c| c.successes).sum(),
            total_failures: crafts.iter().map(|c| c.failures).sum(),
            segments,
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

    /// The question a skill history answers: which recipe carried me across
    /// which stretch.
    #[test]
    fn segments_collapse_consecutive_points_on_one_recipe() {
        let mut t = TradeskillTracker::new();
        for (i, ts) in [100i64, 110, 120].iter().enumerate() {
            t.on_craft(*ts, "Banded Gorget", true);
            t.on_skill_up(ts + 1, "Blacksmithing", 31 + i as u32);
        }
        let segs = t.report().segments;
        assert_eq!(segs.len(), 1, "one recipe, one stretch: {segs:?}");
        let s = &segs[0];
        assert_eq!(s.item.as_deref(), Some("Banded Gorget"));
        assert_eq!((s.from, s.to), (30, 33), "entered at 30, left at 33");
        assert_eq!(s.ups, 3);
        assert_eq!(s.attempts, 3);
        assert_eq!(s.successes, 3);
    }

    /// Alternating recipes must read as alternating stretches — merging them
    /// would invent a range that never happened.
    #[test]
    fn switching_recipe_starts_a_new_segment() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Banded Gorget", true);
        t.on_skill_up(101, "Blacksmithing", 31);
        t.on_craft(200, "Sheet Metal", false);
        t.on_skill_up(201, "Blacksmithing", 32);
        t.on_craft(300, "Banded Gorget", true);
        t.on_skill_up(301, "Blacksmithing", 33);

        let segs = t.report().segments;
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].item.as_deref(), Some("Banded Gorget"));
        assert_eq!(segs[1].item.as_deref(), Some("Sheet Metal"));
        assert_eq!(segs[2].item.as_deref(), Some("Banded Gorget"));
        // A failed combine still skills you up, and still counts as effort.
        assert_eq!(segs[1].attempts, 1);
        assert_eq!(segs[1].successes, 0);
    }

    /// Points bought from a trainer form their own stretch, and carry no
    /// recipe even if a combine happened to be nearby.
    #[test]
    fn trained_points_form_their_own_segment() {
        let mut t = TradeskillTracker::new();
        t.on_skill_up(100, "Pottery", 3);
        t.on_trained("Pottery");
        t.on_skill_up(110, "Pottery", 4);
        t.on_trained("Pottery");

        let segs = t.report().segments;
        assert_eq!(segs.len(), 1);
        assert!(segs[0].trained);
        assert_eq!(segs[0].item, None);
        assert_eq!((segs[0].from, segs[0].to), (2, 4));
    }

    /// A point bought from a trainer is progress you paid for, not progress
    /// you made — counting it as crafting misdescribes how the skill rose.
    #[test]
    fn trained_points_are_separated_from_practice() {
        let mut t = TradeskillTracker::new();
        t.on_skill_up(100, "Pottery", 3);
        t.on_trained("Pottery");
        t.on_skill_up(200, "Pottery", 4);

        let p = &t.report().skills[0];
        assert_eq!(p.ups, 2, "both points counted");
        assert_eq!(p.trained, 1);
        assert_eq!(p.from_practice(), 1);
    }

    /// A trainer line with no matching skill-up must not drive practice
    /// negative.
    #[test]
    fn a_stray_trainer_line_cannot_exceed_the_points_seen() {
        let mut t = TradeskillTracker::new();
        t.on_skill_up(100, "Pottery", 3);
        for _ in 0..5 {
            t.on_trained("Pottery");
        }
        let p = &t.report().skills[0];
        assert_eq!(p.trained, 1);
        assert_eq!(p.from_practice(), 0);
    }

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
