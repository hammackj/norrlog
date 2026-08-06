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
//! but the client writes the two outcomes in *opposite orders*. A failure is
//! announced before its skill-up; a success is announced after it:
//!
//! ```text
//! You lacked the skills to fashion Potion of Accuracy.
//! You have become better at Alchemy! (52)
//!
//! You have become better at Alchemy! (53)
//! You have fashioned the items together to create something new: Potion of Accuracy.
//! ```
//!
//! So attribution has to look both ways, and what identifies the combine that
//! actually earned the point is that it shares the skill-up's *second*.
//! Combines repeat every three seconds or so, which is close enough that a
//! wider window reaches the neighbouring attempt instead — the previous one on
//! a success, the next one on a failure. Measured over two characters' logs,
//! every causal pair is same-second and no pair is one second apart, so the
//! window is deliberately tight.
//!
//! Each combine earns at most one point, so a combine that has been credited is
//! spent and cannot also be credited to the next skill-up.
//!
//! The association is then remembered per item, letting the many combines that
//! yield no skill-up be attributed too. This matters because skill-ups become
//! rare as a skill caps, which is exactly when most combines happen.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How far from a combine a skill-up is still taken to belong to it.
///
/// One second, not five: combines repeat about every three seconds, so a wider
/// window matches the neighbouring attempt rather than the causal one. Every
/// causal pair observed is same-second; the extra second is only slack for a
/// pair that straddles a second boundary.
const SKILL_ATTRIBUTION_WINDOW: i64 = 1;

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
    /// What each recipe cost per combine, as far as can be shown.
    pub costs: Vec<RecipeCost>,
    /// Merchant prices learned from purchases, dearest first.
    pub prices: Vec<ItemPrice>,
}

/// The observable cost of one combine at a recipe.
///
/// A floor, not a total: only depot-sourced components are announced, so the
/// components behind this figure are whatever the log happened to reveal.
/// `costed` against `combines` is how much of the evidence it rests on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeCost {
    pub item: String,
    pub skill: Option<String>,
    /// Copper per combine, from the components seen and priced.
    pub unit_copper: u64,
    /// Components seen going in, dearest first: (name, qty per combine, copper).
    pub components: Vec<CostComponent>,
    /// Combines whose components the log revealed.
    pub costed: u64,
    /// Combines attempted.
    pub combines: u64,
    /// Components seen but never bought, so not counted in `unit_copper`.
    pub unpriced: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostComponent {
    pub item: String,
    /// Average number used per costed combine.
    pub per_combine: f64,
    /// Copper per combine for this component.
    pub copper: u64,
}

/// A combine awaiting the skill-up that would name its tradeskill.
#[derive(Debug, Clone)]
struct PendingCraft {
    at: i64,
    item: String,
    /// Which combine this is, so several `Consumed` lines from one combine
    /// count as one costed combine rather than several.
    seq: u64,
}

/// What a merchant charged for an item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemPrice {
    pub item: String,
    /// Price of one, in copper. Prices vary by merchant and faction; this is
    /// the most recent one paid.
    pub unit_copper: u64,
    pub bought: u64,
    pub last_bought: i64,
}

/// Components observed going into one recipe.
///
/// Only components pulled from the depot are announced — anything taken from
/// inventory is consumed silently — so this is always a subset of the real
/// recipe, and there is no way to tell a complete observation from a partial
/// one. Everything derived from it is therefore a floor, never a total.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsumedBy {
    /// Component key -> total quantity seen consumed.
    pub per_combine: HashMap<String, u64>,
    /// Combines attempted at this recipe.
    pub combines: u64,
    /// Combines that announced at least one component.
    pub costed: u64,
    /// The combine `costed` was last incremented for.
    last_seq: Option<u64>,
}

/// A skill-up awaiting the combine that earned it.
///
/// The mirror of `PendingCraft`, for the success ordering: the point is
/// announced first, so it has to wait for the combine line to name the item.
#[derive(Debug, Clone)]
struct PendingUp {
    at: i64,
    skill: String,
    /// Where the point sits in `timeline`, so attribution can be filled in.
    index: usize,
}

/// Accumulates tradeskill activity.
#[derive(Debug)]
pub struct TradeskillTracker {
    skills: HashMap<String, SkillProgress>,
    crafts: HashMap<String, CraftRecord>,
    components: HashMap<String, ComponentUse>,
    recipes: Vec<String>,
    pending: Option<PendingCraft>,
    /// A point announced before the combine that earned it.
    pending_up: Option<PendingUp>,
    /// The most recent combine, for attributing the `Consumed` lines that
    /// follow it. Unlike `pending` this is not spent by crediting a point.
    last_craft: Option<PendingCraft>,
    /// Recipe key -> components seen going into it.
    consumed: HashMap<String, ConsumedBy>,
    /// Item key -> what a merchant charged for it.
    prices: HashMap<String, ItemPrice>,
    /// Counts combines, so one combine's several `Consumed` lines are one.
    craft_seq: u64,
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
            pending_up: None,
            last_craft: None,
            consumed: HashMap::new(),
            prices: HashMap::new(),
            craft_seq: 0,
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

        // Set before any early return: the `Consumed` lines that follow belong
        // to this combine whether or not it also earned a point.
        self.craft_seq += 1;
        self.last_craft = Some(PendingCraft {
            at: timestamp,
            item: key.clone(),
            seq: self.craft_seq,
        });
        self.consumed.entry(key.clone()).or_default().combines += 1;

        // A point already announced is this combine's, if it is close enough:
        // that is the success ordering, where the point precedes the line
        // naming the item. Claiming it here spends the combine, so it is not
        // also offered to the next skill-up.
        if let Some(u) = self.pending_up.clone() {
            if timestamp - u.at <= SKILL_ATTRIBUTION_WINDOW {
                self.pending_up = None;
                if let Some(name) = self.credit(&key, &u.skill) {
                    if let Some(events) = self.timeline.get_mut(&u.skill.to_lowercase()) {
                        if let Some(ev) = events.get_mut(u.index) {
                            ev.item = Some(name);
                        }
                    }
                }
                return;
            }
            // Too far away to be the cause; that point stays unattributed.
            self.pending_up = None;
        }

        // Hold it briefly: a skill-up arriving next names the tradeskill.
        self.pending = Some(PendingCraft {
            at: timestamp,
            item: key,
            seq: self.craft_seq,
        });
    }

    /// Attribute a point to a combine, returning the item's display name if the
    /// combine earned it.
    ///
    /// Labelling and crediting are separate questions. A declared mapping is
    /// authoritative — an adjacent skill-up may belong to a different combine
    /// done around the same time and must not relabel it — but a declared
    /// recipe still earns its own skill-ups. It simply only accepts ones in the
    /// skill it was declared to belong to.
    fn credit(&mut self, item_key: &str, skill: &str) -> Option<String> {
        let declared = self.declared.contains(item_key);
        if !declared {
            self.item_skill
                .insert(item_key.to_string(), skill.to_string());
            if let Some(c) = self.crafts.get_mut(item_key) {
                c.skill = Some(skill.to_string());
            }
        }
        let belongs = !declared
            || self
                .item_skill
                .get(item_key)
                .is_some_and(|s| s.eq_ignore_ascii_case(skill));
        if !belongs {
            return None;
        }
        // Carry the display name, not the lookup key: this ends up in front of
        // the user.
        let c = self.crafts.get_mut(item_key)?;
        c.skill_ups += 1;
        Some(c.item.clone())
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

        // A combine already announced is the cause when it shares this second:
        // that is the failure ordering. Spend it, so a combine cannot be
        // credited twice.
        if let Some(p) = self.pending.clone() {
            if timestamp - p.at <= SKILL_ATTRIBUTION_WINDOW {
                self.pending = None;
                credited = self.credit(&p.item, skill);
            }
        }

        let events = self.timeline.entry(skill.to_lowercase()).or_default();
        let index = events.len();
        events.push(SkillUpEvent {
            at: timestamp,
            value,
            item: credited.clone(),
            trained: false,
        });

        // Nothing to credit it to yet — the success ordering announces the
        // point first, so wait for the combine that names the item. A point
        // still waiting when the next one arrives never found its combine.
        self.pending_up = if credited.is_none() {
            Some(PendingUp {
                at: timestamp,
                skill: skill.to_string(),
                index,
            })
        } else {
            None
        };
    }

    /// Record a component consumed by a combine.
    pub fn on_component(&mut self, timestamp: i64, item: &str, qty: u64, remaining: Option<u64>) {
        let key = item.to_lowercase();
        let e = self
            .components
            .entry(key.clone())
            .or_insert_with(|| ComponentUse {
                item: item.to_string(),
                used: 0,
                remaining: None,
                last_used: timestamp,
            });
        e.used += qty;
        e.remaining = remaining;
        e.last_used = timestamp;

        // The client prints what a combine consumed immediately after its
        // outcome, so the most recent combine is the one that used it. This
        // reads `last_craft` rather than `pending` because the skill-up in
        // between will have spent `pending` already.
        if let Some(p) = self.last_craft.clone() {
            if timestamp - p.at <= SKILL_ATTRIBUTION_WINDOW {
                let c = self.consumed.entry(p.item).or_default();
                *c.per_combine.entry(key).or_insert(0) += qty;
                if c.last_seq != Some(p.seq) {
                    c.last_seq = Some(p.seq);
                    c.costed += 1;
                }
            }
        }
    }

    /// Record a merchant purchase. The unit price is what makes a combine
    /// costable; prices move with faction and merchant, so the most recent one
    /// paid is the one used.
    pub fn on_purchase(&mut self, timestamp: i64, item: &str, qty: u64, total_copper: u64) {
        if qty == 0 {
            return;
        }
        let e = self
            .prices
            .entry(item.to_lowercase())
            .or_insert_with(|| ItemPrice {
                item: item.to_string(),
                unit_copper: 0,
                bought: 0,
                last_bought: timestamp,
            });
        e.unit_copper = total_copper / qty;
        e.bought += qty;
        e.last_bought = timestamp;
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

        let mut costs: Vec<RecipeCost> = self
            .consumed
            .iter()
            .filter(|(_, c)| c.costed > 0)
            .map(|(key, c)| {
                let mut parts: Vec<CostComponent> = Vec::new();
                let mut unpriced: Vec<String> = Vec::new();
                for (comp, qty) in &c.per_combine {
                    // Per costed combine, not per attempt: the combines that
                    // said nothing about their components cannot be averaged in
                    // without inventing a figure for them.
                    let per = *qty as f64 / c.costed as f64;
                    match self.prices.get(comp) {
                        Some(p) => parts.push(CostComponent {
                            item: p.item.clone(),
                            per_combine: per,
                            copper: (per * p.unit_copper as f64).round() as u64,
                        }),
                        None => unpriced.push(
                            self.components
                                .get(comp)
                                .map(|c| c.item.clone())
                                .unwrap_or_else(|| comp.clone()),
                        ),
                    }
                }
                parts.sort_by_key(|p| std::cmp::Reverse(p.copper));
                unpriced.sort();
                RecipeCost {
                    item: self
                        .crafts
                        .get(key)
                        .map(|c| c.item.clone())
                        .unwrap_or_else(|| key.clone()),
                    skill: self.item_skill.get(key).cloned(),
                    unit_copper: parts.iter().map(|p| p.copper).sum(),
                    components: parts,
                    costed: c.costed,
                    combines: c.combines,
                    unpriced,
                }
            })
            .collect();
        costs.sort_by_key(|c| std::cmp::Reverse(c.unit_copper));

        let mut prices: Vec<ItemPrice> = self.prices.values().cloned().collect();
        prices.sort_by_key(|p| std::cmp::Reverse(p.unit_copper));

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
            costs,
            prices,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A combine costs what its components cost, priced from what a merchant
    /// charged. The `Consumed` lines follow the outcome, and follow the
    /// skill-up too when there is one — so costing cannot rely on the combine
    /// still being pending.
    #[test]
    fn a_combine_costs_the_components_it_consumed() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Water Flask", 20, 4000); // 0.2p each
        t.on_purchase(50, "Small Piece of Ore", 10, 2400); // 0.24p each

        t.on_craft(100, "Metal Bits", false);
        t.on_skill_up(100, "Blacksmithing", 12);
        t.on_component(100, "Water Flask", 1, Some(99));
        t.on_component(100, "Small Piece of Ore", 2, Some(50));

        let c = t.report().costs;
        assert_eq!(c.len(), 1, "one costable recipe: {c:?}");
        assert_eq!(c[0].item, "Metal Bits");
        assert_eq!(c[0].unit_copper, 200 + 480, "one flask and two ore");
        assert_eq!(c[0].costed, 1);
        assert_eq!(c[0].combines, 1);
        assert!(c[0].unpriced.is_empty());
    }

    /// Components taken from inventory are never announced, so most combines
    /// reveal nothing. Those must not be averaged in as though they were free —
    /// the figure is per *costed* combine, and `costed` vs `combines` is what
    /// says how much of the picture it covers.
    #[test]
    fn uncosted_combines_do_not_dilute_the_figure() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Water Flask", 10, 2000); // 0.2p each

        t.on_craft(100, "Sheet Metal", false);
        t.on_component(100, "Water Flask", 1, Some(9));
        for at in [200i64, 300, 400] {
            t.on_craft(at, "Sheet Metal", false); // silent: from inventory
        }

        let c = &t.report().costs[0];
        assert_eq!(c.unit_copper, 200, "still one flask per combine, not a quarter");
        assert_eq!((c.costed, c.combines), (1, 4), "one of four revealed itself");
    }

    /// A component never bought has no price, so it cannot be counted. Saying
    /// so is the difference between a floor and a wrong total.
    #[test]
    fn components_without_a_price_are_named_not_guessed() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Water Flask", 10, 2000);

        t.on_craft(100, "Tumpy Tonic", false);
        t.on_component(100, "Water Flask", 1, Some(9));
        t.on_component(100, "Kiola Nut", 1, Some(4)); // never bought

        let c = &t.report().costs[0];
        assert_eq!(c.unit_copper, 200, "only what has a price");
        assert_eq!(c.unpriced, vec!["Kiola Nut".to_string()]);
    }

    /// Several `Consumed` lines are one combine, not several.
    #[test]
    fn one_combine_with_many_components_counts_once() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Clubmoss", 30, 3000);
        t.on_craft(100, "Kilva's Skin of Flame", true);
        t.on_component(100, "Clubmoss", 1, Some(29));
        t.on_component(100, "Clubmoss", 1, Some(28));

        let c = &t.report().costs[0];
        assert_eq!(c.costed, 1, "one combine");
        assert_eq!(c.unit_copper, 200, "two clubmoss at 100c");
    }

    /// A success is announced *after* the point it earned, so attribution has
    /// to look forwards as well as back. The real ordering, from the log:
    ///
    /// ```text
    /// You have become better at Blacksmithing! (21)
    /// You have fashioned the items together to create something new: Sheet Metal.
    /// ```
    #[test]
    fn a_point_announced_before_its_combine_is_still_attributed() {
        let mut t = TradeskillTracker::new();
        t.on_skill_up(100, "Blacksmithing", 21);
        t.on_craft(100, "Sheet Metal", true);

        let segs = t.report().segments;
        assert_eq!(segs.len(), 1, "one stretch: {segs:?}");
        assert_eq!(
            segs[0].item.as_deref(),
            Some("Sheet Metal"),
            "the combine that followed the point earned it"
        );
    }

    /// The neighbouring attempt is not the cause. Combines repeat every few
    /// seconds, so the previous one is close by but did not earn this point —
    /// the success landing in the same second did.
    #[test]
    fn a_nearby_earlier_combine_does_not_steal_the_point() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Water Flask", true); // an unrelated combine moments before
        t.on_skill_up(103, "Blacksmithing", 33);
        t.on_craft(103, "Banded Gorget", true);

        let segs = t.report().segments;
        assert_eq!(segs.len(), 1, "one stretch: {segs:?}");
        assert_eq!(segs[0].item.as_deref(), Some("Banded Gorget"));
    }

    /// One combine, one point: a combine already credited must not also be
    /// credited to the point that follows it.
    #[test]
    fn a_combine_is_credited_at_most_once() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Banded Gorget", false);
        t.on_skill_up(100, "Blacksmithing", 31);
        t.on_skill_up(100, "Blacksmithing", 32);

        let crafts = t.report().crafts;
        let g = crafts.iter().find(|c| c.item == "Banded Gorget").unwrap();
        assert_eq!(g.skill_ups, 1, "one combine cannot earn two points");
    }

    /// The failure ordering — the combine is announced first — still works.
    #[test]
    fn a_point_announced_after_its_combine_is_attributed() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Banded Gorget", false);
        t.on_skill_up(100, "Blacksmithing", 32);

        let segs = t.report().segments;
        assert_eq!(segs.len(), 1, "one stretch: {segs:?}");
        assert_eq!(segs[0].item.as_deref(), Some("Banded Gorget"));
    }

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
