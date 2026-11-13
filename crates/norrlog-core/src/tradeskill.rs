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
//!
//! # Attributing components to a combine
//!
//! The `Consumed` lines are written in the same two orders, for the same
//! reason. A failure announces its outcome and then what it ate; a success
//! announces what it ate and then the outcome:
//!
//! ```text
//! You lacked the skills to fashion Metal Bits.
//! Consumed 1 x Small Piece of Ore (leaving 693) from your personal depot.
//!
//! Consumed 1 x Small Piece of Ore (leaving 691) from your personal depot.
//! You have fashioned the items together to create something new: Metal Bits.
//! ```
//!
//! Across two characters' logs — 5,178 `Consumed` lines — the two orders are
//! perfectly separated: every line preceded by a same-second outcome follows a
//! failure, every line followed by one precedes a success, and nothing falls in
//! between. So a component belongs to the combine it shares a second with,
//! whichever side of it that combine is announced on.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

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
    // A skill like the others: it has a trivial, needs a container and bait,
    // and yields items. The recipe book carries hundreds of fishing recipes,
    // and leaving it out would mean combines the book can attribute to a skill
    // the rest of the app refuses to recognise.
    "Fishing",
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

/// Recipes whose tradeskill cannot be inferred from the log are supplied by
/// the recipe book instead — see [`crate::recipe`], and
/// `crates/norrlog-core/assets/recipes.json` for the data itself. A mapping
/// can also be declared at runtime with
/// [`TradeskillTracker::set_item_skill`].
pub use crate::recipe::RecipeBook;

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
    /// Tradeskills whose recipes were seen consuming this, alphabetically.
    ///
    /// A list rather than one skill because a component is rarely exclusive —
    /// a water flask goes into most of them — and empty when every recipe that
    /// consumed it is itself unattributed.
    #[serde(default)]
    pub skills: Vec<String>,
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
    /// What the stretch cost: every attempt in it at this recipe's per-combine
    /// price. A failed combine eats its components like any other, so this
    /// counts attempts, not successes. Zero when the recipe has no observed
    /// price, which is not the same as free — see [`SkillCost`].
    #[serde(default)]
    pub copper: u64,
    pub started: i64,
    pub ended: i64,
}

/// A recipe the character learned.
///
/// The client names the recipe and nothing else, so its tradeskill is only
/// known when the same name has been combined and attributed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedRecipe {
    pub recipe: String,
    pub skill: Option<String>,
}

/// What raising one tradeskill has cost, recipe by recipe.
///
/// # Why attempts and not successes
///
/// A combine consumes its components whether or not it works, so what a skill
/// cost is what every attempt at it cost. In EverQuest Legends that is the
/// whole story. In classic EverQuest the Salvage AA hands some components back
/// from a failure at random, so the same figure is an upper bound there — and
/// since nothing in the log announces a salvage, that gap cannot be measured
/// from here. The game is carried on the report so a reader can say which of
/// the two they are looking at rather than guess.
///
/// # Why it is still a floor in both
///
/// Per-combine prices come from components the log announced and a merchant
/// priced. Anything taken from inventory, or never bought, is not in the
/// figure at all. `priced_attempts` against `attempts` is how much of the
/// effort the total actually covers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCost {
    pub skill: String,
    /// Sum over recipes of attempts times per-combine price.
    pub copper: u64,
    /// Every combine attributed to this skill.
    pub attempts: u64,
    /// Attempts at recipes that have a per-combine price at all. The rest
    /// contribute nothing, which is why the total is a floor.
    pub priced_attempts: u64,
    /// Of those, attempts priced from the recipe book rather than from
    /// components the log announced. Reported separately because the two are
    /// different kinds of claim, and a total that blurs them cannot be
    /// checked.
    #[serde(default)]
    pub book_priced_attempts: u64,
    /// Per recipe, dearest first.
    pub recipes: Vec<RecipeSpend>,
}

/// What one recipe cost across every attempt at it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeSpend {
    pub item: String,
    pub attempts: u64,
    pub successes: u64,
    /// Per-combine price, or `None` when nothing could price it.
    pub unit_copper: Option<u64>,
    /// Priced from the recipe book because the log announced nothing.
    #[serde(default)]
    pub from_book: bool,
    pub copper: u64,
}

/// Everything the tradeskill tab shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeskillReport {
    /// Which game these observations are from. Decides whether the cost
    /// totals are what was spent (Legends) or an upper bound before Salvage
    /// (classic).
    #[serde(default)]
    pub game: crate::game::Game,
    /// What each tradeskill cost to raise, dearest first.
    pub skill_costs: Vec<SkillCost>,
    pub skills: Vec<SkillProgress>,
    /// Per-skill history, oldest first.
    pub segments: Vec<SkillSegment>,
    pub crafts: Vec<CraftRecord>,
    pub components: Vec<ComponentUse>,
    pub recipes_learned: Vec<LearnedRecipe>,
    pub total_successes: u32,
    pub total_failures: u32,
    /// What each recipe cost per combine, as far as can be shown.
    pub costs: Vec<RecipeCost>,
    /// Merchant prices learned from purchases, dearest first.
    pub prices: Vec<ItemPrice>,
}

fn observed_basis() -> CostBasis {
    CostBasis::Observed
}

/// Where a per-combine price came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CostBasis {
    /// Components the log announced going in, priced from purchases. What
    /// actually happened, but only ever a subset: inventory is silent.
    Observed,
    /// Components the recipe book lists, priced from purchases. Complete as to
    /// what a combine takes, but it assumes the book's recipe is the one that
    /// was used, and assumes the price paid still applied.
    Recipe,
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
    /// Whether the components behind this figure were seen in the log or read
    /// out of the recipe book.
    #[serde(default = "observed_basis")]
    pub basis: CostBasis,
    /// Copper per combine, from the components seen and priced.
    pub unit_copper: u64,
    /// Every attempt at this recipe at that price. Failures included: they
    /// eat their components too.
    #[serde(default)]
    pub total_copper: u64,
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
    /// Average number used per combine that announced this component.
    ///
    /// Not per costed combine. A recipe's third component is silent whenever
    /// it came out of your bags, and averaging it over every combine that
    /// announced anything reported `Sheet Metal ×0.30` for a recipe that
    /// takes one of them — a coverage figure wearing a quantity's clothes.
    /// Divided by the combines that actually named it, the same evidence
    /// says ×1, and `seen_in` is what carries how much of it there was.
    pub per_combine: f64,
    /// Combines that announced this component, against the recipe's `costed`.
    #[serde(default)]
    pub seen_in: u64,
    /// Copper per combine for this component.
    ///
    /// Still spread over every costed combine, unlike `per_combine`: this is
    /// what the component was observed to cost a combine, and a component
    /// that was only announced a third of the time was only *seen* to cost
    /// anything a third of the time. Assuming it into the other two thirds
    /// would turn a floor into an estimate.
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
    /// Times a merchant quoted a price for it without a sale — clicking the
    /// item in their window. The same number as a purchase establishes, and
    /// counted apart so a page can say which it saw.
    #[serde(default)]
    pub quoted: u64,
    pub last_bought: i64,
    /// Who sold it and where, dearest first.
    ///
    /// Kept per merchant rather than collapsed into one price: the same
    /// reagent costs different amounts from different vendors and at different
    /// factions, and "where do I buy this" is a question the single figure
    /// above cannot answer.
    #[serde(default)]
    pub vendors: Vec<VendorPrice>,
}

/// One merchant's price for an item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorPrice {
    pub merchant: String,
    /// The zone you were in when you bought it, which is where the merchant
    /// is. Absent only if the log had not yet said which zone that was.
    pub zone: Option<String>,
    pub unit_copper: u64,
    pub bought: u64,
    /// Times the merchant quoted this price without a sale — one click in
    /// their window each. Kept apart from `bought` because they are different
    /// evidence of the same number: a purchase proves the price *and* that
    /// the item was in stock and affordable, a quote proves only the price.
    /// Nothing here is worth less for having been quoted: across 643 pairs
    /// seen both ways the two agreed every time.
    pub quoted: u64,
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
    /// Component key -> combines that announced *that component*.
    ///
    /// The denominator a quantity should be divided by, and not the same as
    /// `costed`. A combine announces the components it drew from a depot and
    /// says nothing about the ones that came out of your bags, so a combine
    /// can announce two of a recipe's three components and be counted as
    /// costed on the strength of those two. Dividing the third's quantity by
    /// `costed` then measures how often it happened to come from the depot,
    /// which is a fact about where things were stored rather than about the
    /// recipe.
    #[serde(default)]
    pub seen_in: HashMap<String, u64>,
    /// Component key -> the combine `seen_in` was last incremented for.
    #[serde(default)]
    comp_seq: HashMap<String, u64>,
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
    /// (item key, merchant key) -> that merchant's price, so "where do I buy
    /// this" survives alongside "what does it cost".
    vendors: HashMap<(String, String), VendorPrice>,
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
    /// Components announced before the combine that ate them — the success
    /// ordering — waiting for it to be named: (second, component key, qty).
    pending_components: Vec<(i64, String, u64)>,
    /// Recipes known independently of the log.
    book: &'static RecipeBook,
}

impl Default for TradeskillTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TradeskillTracker {
    pub fn new() -> Self {
        Self::with_book(RecipeBook::embedded())
    }

    /// A tracker reading a particular recipe book, for tests that need to
    /// control what is known in advance.
    pub fn with_book(book: &'static RecipeBook) -> Self {
        let mut t = TradeskillTracker {
            book,
            pending_components: Vec::new(),
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
            vendors: HashMap::new(),
            craft_seq: 0,
            item_skill: HashMap::new(),
            declared: std::collections::HashSet::new(),
        };
        let known: Vec<(String, String)> = book
            .products()
            .map(|(i, s)| (i.to_string(), s.to_string()))
            .collect();
        for (item, skill) in known {
            t.set_item_skill(&item, &skill);
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
        // A combine hands its tools back, and the client announces that with
        // the same line it uses for what you made. Counting it would invent a
        // combine, and — because it would become the most recent one — would
        // hand the real combine's components to the tool as well.
        if self.book.is_tool(item) {
            return;
        }

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
        let seq = self.craft_seq;
        self.last_craft = Some(PendingCraft {
            at: timestamp,
            item: key.clone(),
            seq,
        });
        self.consumed.entry(key.clone()).or_default().combines += 1;

        // Components announced ahead of this line are this combine's — the
        // success ordering. Anything left over was announced in a second that
        // never produced an outcome, and is discarded rather than handed to
        // the next combine along.
        for (at, comp, qty) in std::mem::take(&mut self.pending_components) {
            if at == timestamp {
                self.attribute_component(&key, seq, comp, qty);
            }
        }

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
                // Filled in at report time, from the recipes seen consuming it.
                skills: Vec::new(),
            });
        e.used += qty;
        e.remaining = remaining;
        e.last_used = timestamp;

        // A combine already announced this second is a failure announcing what
        // it ate; otherwise the combine is still to come, and this waits for
        // it. Reads `last_craft` rather than `pending` because a skill-up in
        // between will have spent `pending` already.
        //
        // Same second exactly, not the skill-up's one-second slack: a combine
        // one second earlier is the *previous* attempt, and attributing to it
        // is precisely the mistake this replaced.
        match self.last_craft.clone() {
            Some(p) if p.at == timestamp => self.attribute_component(&p.item, p.seq, key, qty),
            _ => self.pending_components.push((timestamp, key, qty)),
        }
    }

    /// Charge a component to a combine, counting the combine as costed the
    /// first time one of its components is seen.
    fn attribute_component(&mut self, recipe: &str, seq: u64, component: String, qty: u64) {
        let c = self.consumed.entry(recipe.to_string()).or_default();
        *c.per_combine.entry(component.clone()).or_insert(0) += qty;
        // Per component, because one combine can announce some of a recipe
        // and stay silent about the rest. Several `Consumed` lines for the
        // same component in one combine are one combine, the same way
        // `costed` treats the recipe as a whole.
        if c.comp_seq.get(&component) != Some(&seq) {
            c.comp_seq.insert(component.clone(), seq);
            *c.seen_in.entry(component).or_insert(0) += 1;
        }
        if c.last_seq != Some(seq) {
            c.last_seq = Some(seq);
            c.costed += 1;
        }
    }

    /// Record a merchant purchase. The unit price is what makes a combine
    /// costable; prices move with faction and merchant, so the most recent one
    /// paid is the one used.
    pub fn on_purchase(&mut self, timestamp: i64, item: &str, qty: u64, total_copper: u64) {
        self.on_purchase_from(timestamp, item, qty, total_copper, None, None)
    }

    /// A purchase with its provenance: who sold it and where they stand.
    ///
    /// "Where do I buy this" is a different question from "what does it cost",
    /// and only this form can answer it.
    /// A merchant's asking price, with nothing bought.
    ///
    /// Same shelf as a purchase — it is the same number, established the same
    /// way — but counted separately so a page can say which it saw. The price
    /// is overwritten rather than averaged: merchants charge by charisma and
    /// faction, so if it has changed, the newer reading is the true one for
    /// whoever is looking.
    pub fn on_quote_from(
        &mut self,
        timestamp: i64,
        item: &str,
        unit_copper: u64,
        merchant: Option<&str>,
        zone: Option<&str>,
    ) {
        let Some(m) = merchant.filter(|m| !m.is_empty()) else {
            return;
        };
        if item.is_empty() {
            return;
        }
        let v = self
            .vendors
            .entry((item.to_lowercase(), m.to_lowercase()))
            .or_insert_with(|| VendorPrice {
                merchant: m.to_string(),
                zone: None,
                unit_copper: 0,
                bought: 0,
                quoted: 0,
                last_bought: timestamp,
            });
        v.unit_copper = unit_copper;
        v.quoted += 1;
        v.last_bought = timestamp;
        if let Some(z) = zone.filter(|z| !z.is_empty()) {
            v.zone = Some(z.to_string());
        }

        // The report is built by walking `prices` and hanging each item's
        // merchants off it, so an item that has only ever been quoted needs an
        // entry here too. Without one its vendor row exists and is never
        // reachable — which is exactly how the first version of this silently
        // recorded nothing.
        let e = self
            .prices
            .entry(item.to_lowercase())
            .or_insert_with(|| ItemPrice {
                item: item.to_string(),
                vendors: Vec::new(),
                unit_copper: 0,
                bought: 0,
                quoted: 0,
                last_bought: timestamp,
            });
        e.unit_copper = unit_copper;
        e.quoted += 1;
        e.last_bought = timestamp;
    }

    pub fn on_purchase_from(
        &mut self,
        timestamp: i64,
        item: &str,
        qty: u64,
        total_copper: u64,
        merchant: Option<&str>,
        zone: Option<&str>,
    ) {
        if qty == 0 {
            return;
        }
        if let Some(m) = merchant.filter(|m| !m.is_empty()) {
            let v = self
                .vendors
                .entry((item.to_lowercase(), m.to_lowercase()))
                .or_insert_with(|| VendorPrice {
                    merchant: m.to_string(),
                    zone: None,
                    unit_copper: 0,
                    bought: 0,
                    quoted: 0,
                    last_bought: timestamp,
                });
            v.unit_copper = total_copper / qty;
            v.bought += qty;
            v.last_bought = timestamp;
            // Where you were standing is where the merchant is. Only
            // overwritten by a real zone, so an early purchase before the log
            // named one is corrected rather than blanked.
            if let Some(z) = zone.filter(|z| !z.is_empty()) {
                v.zone = Some(z.to_string());
            }
        }
        let e = self
            .prices
            .entry(item.to_lowercase())
            .or_insert_with(|| ItemPrice {
                item: item.to_string(),
                vendors: Vec::new(),
                unit_copper: 0,
                bought: 0,
                quoted: 0,
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
                    // Filled in once per-combine prices are known.
                    copper: 0,
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
        // Every sort in this report is made total by a name, and none of
        // them were. The rows come out of HashMaps, so ties held whatever
        // order the map happened to yield — which differs between runs of the
        // same binary over the same log, because Rust seeds its hasher per
        // process. A report written to a file therefore produced a spurious
        // diff on every rebuild, and a comparison against an older copy could
        // not tell a change from a reshuffle.
        skills.sort_by(|a, b| {
            b.current
                .cmp(&a.current)
                .then_with(|| a.skill.cmp(&b.skill))
        });

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
        crafts.sort_by(|a, b| {
            b.attempts()
                .cmp(&a.attempts())
                .then_with(|| a.item.cmp(&b.item))
        });

        // Which tradeskills each component serves, from the recipes seen
        // consuming it. A component whose recipes are all unattributed carries
        // no skill rather than a guessed one.
        let mut component_skills: HashMap<&str, BTreeSet<&str>> = HashMap::new();
        for (recipe, c) in &self.consumed {
            let Some(skill) = self.item_skill.get(recipe) else {
                continue;
            };
            for comp in c.per_combine.keys() {
                component_skills
                    .entry(comp.as_str())
                    .or_default()
                    .insert(skill.as_str());
            }
        }

        let mut components: Vec<ComponentUse> = self
            .components
            .iter()
            .map(|(key, c)| ComponentUse {
                skills: component_skills
                    .get(key.as_str())
                    .map(|s| s.iter().map(|s| s.to_string()).collect())
                    .unwrap_or_default(),
                ..c.clone()
            })
            .collect();
        components.sort_by(|a, b| b.used.cmp(&a.used).then_with(|| a.item.cmp(&b.item)));

        let mut costs: Vec<RecipeCost> = self
            .consumed
            .iter()
            .filter(|(_, c)| c.costed > 0)
            .map(|(key, c)| {
                let mut parts: Vec<CostComponent> = Vec::new();
                let mut unpriced: Vec<String> = Vec::new();
                for (comp, qty) in &c.per_combine {
                    // The quantity is per combine that named this component.
                    // A recipe from before `seen_in` existed has none, and
                    // `costed` is what it used to divide by.
                    let named = c.seen_in.get(comp).copied().unwrap_or(c.costed).max(1);
                    let per = *qty as f64 / named as f64;
                    // The cost is still per costed combine: the combines that
                    // said nothing about their components cannot be averaged
                    // in without inventing a figure for them.
                    let share = *qty as f64 / c.costed.max(1) as f64;
                    match self.prices.get(comp) {
                        Some(p) => parts.push(CostComponent {
                            item: p.item.clone(),
                            per_combine: per,
                            seen_in: named,
                            copper: (share * p.unit_copper as f64).round() as u64,
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
                let unit: u64 = parts.iter().map(|p| p.copper).sum();
                RecipeCost {
                    item: self
                        .crafts
                        .get(key)
                        .map(|c| c.item.clone())
                        .unwrap_or_else(|| key.clone()),
                    skill: self.item_skill.get(key).cloned(),
                    basis: CostBasis::Observed,
                    unit_copper: unit,
                    // Every attempt, not every success: a failed combine eats
                    // its components just the same.
                    total_copper: unit * c.combines,
                    components: parts,
                    costed: c.costed,
                    combines: c.combines,
                    unpriced,
                }
            })
            .collect();

        // A recipe whose components never came from the depot announced
        // nothing at all, so the log alone can never price it — Tharn's 356
        // Elixir combines drew from his bags and are silent to the last one.
        // The book can still price them, from what a merchant charged for the
        // components it lists.
        //
        // Only where the book is unambiguous: one recipe for the product, and
        // nothing observed to contradict it. Guessing which of twenty-one ways
        // of making an ore brick was used would be inventing evidence, not
        // recovering it.
        for (key, c) in &self.consumed {
            if costs.iter().any(|x| x.item.to_lowercase() == *key) {
                continue;
            }
            let mut recipes = self.book.recipes_for(key);
            let (Some(recipe), None) = (recipes.next(), recipes.next()) else {
                continue;
            };
            let Some(components) = recipe.components.as_ref() else {
                continue;
            };
            let mut parts: Vec<CostComponent> = Vec::new();
            let mut unpriced: Vec<String> = Vec::new();
            for comp in components {
                // A tool is handed back, not consumed, so it is not a cost.
                if self.book.is_tool(&comp.item) {
                    continue;
                }
                match self.prices.get(&comp.item.to_lowercase()) {
                    Some(p) => parts.push(CostComponent {
                        item: p.item.clone(),
                        per_combine: comp.qty as f64,
                        // Zero, because no combine announced it: this arm is
                        // the recipe book stating the quantity, and the book
                        // is not an observation to count.
                        seen_in: 0,
                        copper: p.unit_copper * comp.qty as u64,
                    }),
                    None => unpriced.push(comp.item.clone()),
                }
            }
            if parts.is_empty() {
                continue;
            }
            parts.sort_by_key(|p| std::cmp::Reverse(p.copper));
            unpriced.sort();
            let unit: u64 = parts.iter().map(|p| p.copper).sum();
            costs.push(RecipeCost {
                item: self
                    .crafts
                    .get(key)
                    .map(|c| c.item.clone())
                    .unwrap_or_else(|| recipe.product.clone()),
                skill: self.item_skill.get(key).cloned(),
                basis: CostBasis::Recipe,
                unit_copper: unit,
                total_copper: unit * c.combines,
                components: parts,
                // Nothing was observed: that is the whole reason this row
                // exists, and the zero says so.
                costed: 0,
                combines: c.combines,
                unpriced,
            });
        }
        costs.sort_by(|a, b| {
            b.unit_copper
                .cmp(&a.unit_copper)
                .then_with(|| a.item.cmp(&b.item))
        });

        let mut prices: Vec<ItemPrice> = self
            .prices
            .iter()
            .map(|(key, p)| {
                let mut vendors: Vec<VendorPrice> = self
                    .vendors
                    .iter()
                    .filter(|((item, _), _)| item == key)
                    .map(|(_, v)| v.clone())
                    .collect();
                // Dearest first, then by name so the order is total. Price
                // alone leaves two merchants charging the same amount in
                // whatever order the HashMap yielded them, which differs
                // between runs of the same binary over the same log — so a
                // report written to a file produced a spurious diff on every
                // rebuild, and a comparison against it could not tell a
                // change from a reshuffle. 42 of 713 prices were in that
                // state.
                vendors.sort_by(|a, b| {
                    b.unit_copper
                        .cmp(&a.unit_copper)
                        .then_with(|| a.merchant.cmp(&b.merchant))
                        .then_with(|| a.zone.cmp(&b.zone))
                });
                ItemPrice {
                    vendors,
                    ..p.clone()
                }
            })
            .collect();
        prices.sort_by(|a, b| {
            b.unit_copper
                .cmp(&a.unit_copper)
                .then_with(|| a.item.cmp(&b.item))
        });

        // Per-combine price by recipe, so effort can be turned into spend
        // wherever attempts are counted.
        let unit: HashMap<String, (u64, CostBasis)> = costs
            .iter()
            .filter(|c| c.unit_copper > 0)
            .map(|c| (c.item.to_lowercase(), (c.unit_copper, c.basis)))
            .collect();

        let mut segments: Vec<SkillSegment> = self
            .skills
            .iter()
            .flat_map(|(key, p)| self.segments_for(key, &p.skill))
            .collect();
        // Built by iterating a HashMap, so with no order of its own at all.
        segments.sort_by(|a, b| {
            a.skill
                .cmp(&b.skill)
                .then_with(|| a.from.cmp(&b.from))
                .then_with(|| a.item.cmp(&b.item))
        });
        for s in &mut segments {
            if let Some(item) = s.item.as_ref() {
                if let Some((u, _)) = unit.get(&item.to_lowercase()) {
                    s.copper = u * s.attempts as u64;
                }
            }
        }

        // What each tradeskill cost to raise. Built from the combine record
        // rather than from the segments: a recipe worked long after the last
        // skill-up still cost what it cost.
        let mut by_skill: HashMap<String, SkillCost> = HashMap::new();
        for c in &crafts {
            let Some(skill) = c.skill.as_ref() else {
                continue;
            };
            let attempts = c.attempts() as u64;
            let priced = unit.get(&c.item.to_lowercase()).copied();
            let unit_copper = priced.map(|(u, _)| u);
            let spend = RecipeSpend {
                item: c.item.clone(),
                attempts,
                successes: c.successes as u64,
                unit_copper,
                from_book: matches!(priced, Some((_, CostBasis::Recipe))),
                copper: unit_copper.unwrap_or(0) * attempts,
            };
            let e = by_skill
                .entry(skill.to_lowercase())
                .or_insert_with(|| SkillCost {
                    skill: skill.clone(),
                    copper: 0,
                    attempts: 0,
                    priced_attempts: 0,
                    book_priced_attempts: 0,
                    recipes: Vec::new(),
                });
            e.copper += spend.copper;
            e.attempts += attempts;
            if unit_copper.is_some() {
                e.priced_attempts += attempts;
            }
            if spend.from_book {
                e.book_priced_attempts += attempts;
            }
            e.recipes.push(spend);
        }
        let mut skill_costs: Vec<SkillCost> = by_skill.into_values().collect();
        for s in &mut skill_costs {
            s.recipes
                .sort_by_key(|r| std::cmp::Reverse((r.copper, r.attempts)));
        }
        skill_costs.sort_by(|a, b| b.copper.cmp(&a.copper).then_with(|| a.skill.cmp(&b.skill)));

        TradeskillReport {
            game: crate::game::Game::Unknown,
            skill_costs,
            total_successes: crafts.iter().map(|c| c.successes).sum(),
            total_failures: crafts.iter().map(|c| c.failures).sum(),
            segments,
            skills,
            crafts,
            components,
            recipes_learned: self
                .recipes
                .iter()
                .map(|r| LearnedRecipe {
                    // The recipe name and the item it makes are the same string
                    // in every line observed, so a recipe that has been
                    // combined inherits that combine's attribution.
                    skill: self.item_skill.get(&r.to_lowercase()).cloned(),
                    recipe: r.clone(),
                })
                .collect(),
            costs,
            prices,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tracker that knows no recipes at all.
    ///
    /// Tests about attribution and costing are about the *logic*, and must not
    /// change meaning as the shipped recipe book grows — an item name that is
    /// unknown today can be in the book tomorrow, which would silently turn a
    /// test of "the log said nothing, so we know nothing" into a test of the
    /// book. Only tests that are about the book itself use the real one.
    fn bookless() -> TradeskillTracker {
        static EMPTY: std::sync::OnceLock<RecipeBook> = std::sync::OnceLock::new();
        TradeskillTracker::with_book(EMPTY.get_or_init(RecipeBook::default))
    }

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

    /// A success announces what it consumed *before* saying what it made, so
    /// costing has to look forwards as well as back. Reading only backwards
    /// hands a success's components to the previous attempt — and since
    /// combines are about three seconds apart, usually to nothing at all,
    /// which left costs resting on failed combines alone.
    #[test]
    fn components_announced_before_their_combine_are_still_costed() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Water Flask", 10, 2000); // 0.2p each

        t.on_craft(100, "Metal Bits", true); // an earlier, unrelated attempt
        t.on_component(103, "Water Flask", 1, Some(9));
        t.on_craft(103, "Sheet Metal", true);

        let c = t.report().costs;
        let sheet = c.iter().find(|c| c.item == "Sheet Metal").unwrap();
        assert_eq!(
            sheet.costed, 1,
            "the flask belongs to the combine it preceded"
        );
        assert_eq!(sheet.unit_copper, 200);
        assert!(
            !c.iter().any(|c| c.item == "Metal Bits" && c.costed > 0),
            "and not to the attempt three seconds earlier: {c:?}"
        );
    }

    /// A combine one second earlier is the previous attempt, not this one.
    #[test]
    fn a_component_does_not_reach_back_a_second() {
        let mut t = bookless();
        t.on_purchase(50, "Water Flask", 10, 2000);
        t.on_craft(100, "Metal Bits", false);
        t.on_component(101, "Water Flask", 1, Some(9));

        let c = t.report().costs;
        assert!(c.is_empty(), "nothing to charge it to yet: {c:?}");
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
        assert_eq!(
            c.unit_copper, 200,
            "still one flask per combine, not a quarter"
        );
        assert_eq!(
            (c.costed, c.combines),
            (1, 4),
            "one of four revealed itself"
        );
    }

    /// What a skill cost to raise is what every attempt cost — a failed
    /// combine eats its components exactly like one that worked, so counting
    /// successes would understate the bill by the failure rate, which on a
    /// skill being pushed is most of it.
    #[test]
    fn levelling_a_skill_costs_every_attempt_not_every_success() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Water Flask", 10, 2000); // 0.2p each

        // Four attempts, one of which worked, each eating one flask.
        for (i, ok) in [false, false, true, false].into_iter().enumerate() {
            let at = 100 + i as i64 * 10;
            t.on_component(at, "Water Flask", 1, Some(9 - i as u64));
            t.on_craft(at, "Sheet Metal", ok);
        }
        // Same second as the last attempt, which is what names the tradeskill.
        t.on_skill_up(130, "Blacksmithing", 21);

        let r = t.report();
        let cost = &r.skill_costs[0];
        assert_eq!(cost.skill, "Blacksmithing");
        assert_eq!(cost.attempts, 4);
        assert_eq!(cost.copper, 800, "four flasks, not the one that worked");
        assert_eq!(cost.priced_attempts, 4);
        assert_eq!(cost.recipes[0].item, "Sheet Metal");
        assert_eq!(cost.recipes[0].unit_copper, Some(200));

        let c = &r.costs[0];
        assert_eq!((c.unit_copper, c.total_copper), (200, 800));
    }

    /// The client announces only what a combine drew from the depot, so a
    /// recipe worked out of your bags is silent however many times you run it
    /// — 356 combines on one character's log, not one `Consumed` line. The
    /// book can price it anyway, from what a merchant charged for the
    /// components the recipe lists.
    #[test]
    fn a_recipe_the_log_never_itemises_is_priced_from_the_book() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Violet Tri-Tube Sap", 100, 32_700); // 0.327p each
        t.on_purchase(50, "Duskglow Vine", 100, 32_700);

        // Straight from the bags: no components are ever announced.
        for at in [100i64, 110, 120] {
            t.on_craft(at, "Elixir of Greater Concentration", false);
        }

        let c = t
            .report()
            .costs
            .into_iter()
            .find(|c| c.item == "Elixir of Greater Concentration")
            .expect("the book should price what the log cannot");
        assert_eq!(c.basis, CostBasis::Recipe);
        assert_eq!(c.unit_copper, 654, "one of each reagent");
        assert_eq!(c.total_copper, 654 * 3);
        assert_eq!(c.costed, 0, "nothing was observed, and it says so");
        assert_eq!(c.combines, 3);
    }

    /// A price with no idea where it came from is half an answer. The same
    /// reagent costs different amounts from different merchants, so vendors
    /// are kept apart rather than averaged.
    #[test]
    fn a_purchase_records_who_sold_it_and_where() {
        let mut t = TradeskillTracker::new();
        t.on_purchase_from(
            100,
            "Duskglow Vine",
            100,
            65_400,
            Some("Sithgok"),
            Some("Innothule Swamp"),
        );
        t.on_purchase_from(
            200,
            "Duskglow Vine",
            100,
            70_000,
            Some("Someone Else"),
            Some("Grobb"),
        );
        t.on_purchase_from(
            300,
            "Duskglow Vine",
            100,
            65_400,
            Some("Sithgok"),
            Some("Innothule Swamp"),
        );

        let p = t
            .report()
            .prices
            .into_iter()
            .find(|p| p.item == "Duskglow Vine")
            .unwrap();
        assert_eq!(p.bought, 300);
        assert_eq!(p.unit_copper, 654, "the most recent price paid");
        assert_eq!(p.vendors.len(), 2, "two merchants, not one blended price");
        let dearest = &p.vendors[0];
        assert_eq!(dearest.merchant, "Someone Else");
        assert_eq!(dearest.unit_copper, 700);
        let sithgok = p.vendors.iter().find(|v| v.merchant == "Sithgok").unwrap();
        assert_eq!(sithgok.zone.as_deref(), Some("Innothule Swamp"));
        assert_eq!(sithgok.bought, 200, "both trips counted");
    }

    /// A purchase before the log has named a zone still records the merchant,
    /// and a later one fills the zone in rather than the blank sticking.
    #[test]
    fn a_zone_learned_later_corrects_an_earlier_purchase() {
        let mut t = TradeskillTracker::new();
        t.on_purchase_from(100, "Water Flask", 10, 2000, Some("Clork"), None);
        t.on_purchase_from(
            200,
            "Water Flask",
            10,
            2000,
            Some("Clork"),
            Some("Innothule Swamp"),
        );
        let p = &t.report().prices[0];
        assert_eq!(p.vendors[0].zone.as_deref(), Some("Innothule Swamp"));
    }

    /// Twenty-one ways to make an ore brick and no way to tell which was used:
    /// picking one would be inventing evidence rather than recovering it.
    #[test]
    fn an_ambiguous_recipe_is_not_priced_from_the_book() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Water Flask", 100, 20_000);
        t.on_craft(100, "Small Brick of High Quality Ore", true);

        let priced = t
            .report()
            .costs
            .into_iter()
            .any(|c| c.item == "Small Brick of High Quality Ore");
        assert!(
            !priced,
            "the book lists many recipes for it; none can be assumed"
        );
    }

    /// A tool is handed back, so it is not part of what a combine costs even
    /// though the recipe lists it.
    #[test]
    fn a_tool_is_not_charged_for_in_a_book_priced_recipe() {
        let book: &'static RecipeBook = Box::leak(Box::new(
            RecipeBook::parse(
                r#"{"tools":["Smithing Chisel"],
                    "recipes":[{"product":"Widget","skill":"Blacksmithing","components":[
                      {"item":"Smithing Chisel","qty":1},{"item":"Water Flask","qty":2}]}]}"#,
            )
            .unwrap(),
        ));
        let mut t = TradeskillTracker::with_book(book);
        t.on_purchase(50, "Water Flask", 10, 2000); // 0.2p each
        t.on_purchase(50, "Smithing Chisel", 1, 79_480); // dear, and returned
        t.on_craft(100, "Widget", true);

        let c = &t.report().costs[0];
        assert_eq!(c.unit_copper, 400, "two flasks; the chisel comes back");
    }

    /// A recipe nothing is known about contributes nothing, and the total has
    /// to admit that rather than read as complete.
    #[test]
    fn unpriced_recipes_are_counted_as_effort_but_not_as_spend() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Water Flask", 10, 2000);

        t.on_component(100, "Water Flask", 1, Some(9));
        t.on_craft(100, "Sheet Metal", true);
        t.on_skill_up(100, "Blacksmithing", 21);
        // Components straight from inventory: the log says nothing at all.
        for at in [200i64, 210] {
            t.on_craft(at, "Banded Gorget", true);
            t.on_skill_up(at, "Blacksmithing", 22);
        }

        let cost = &t.report().skill_costs[0];
        assert_eq!(cost.attempts, 3);
        assert_eq!(cost.priced_attempts, 1, "two attempts we cannot price");
        assert_eq!(cost.copper, 200);
        let gorget = cost
            .recipes
            .iter()
            .find(|r| r.item == "Banded Gorget")
            .unwrap();
        assert_eq!(gorget.unit_copper, None, "unknown, not free");
        assert_eq!(gorget.attempts, 2);
    }

    /// A stretch of levelling carries its own bill, so "30 to 92 on Banded
    /// Gorget" can be read as a price as well as a range.
    #[test]
    fn each_stretch_of_a_skill_carries_what_it_cost() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Water Flask", 10, 2000);
        for (i, ts) in [100i64, 110, 120].iter().enumerate() {
            t.on_component(*ts, "Water Flask", 1, Some(9 - i as u64));
            t.on_craft(*ts, "Sheet Metal", true);
            t.on_skill_up(ts + 1, "Blacksmithing", 21 + i as u32);
        }
        let segs = t.report().segments;
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].attempts, 3);
        assert_eq!(segs[0].copper, 600, "three combines at 0.2p");
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
        let mut t = bookless();
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

    /// Showing one tradeskill at a time means every row has to know which
    /// tradeskill it belongs to — including components, which the log never
    /// labels. The recipe that consumed them does.
    #[test]
    fn components_carry_the_skills_of_the_recipes_that_used_them() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Potion of Accuracy", false);
        t.on_skill_up(100, "Alchemy", 52);
        t.on_component(100, "Water Flask", 1, Some(9));
        t.on_component(100, "Fenugreek", 1, Some(20));

        t.on_craft(200, "Sheet Metal", false);
        t.on_skill_up(200, "Blacksmithing", 21);
        t.on_component(200, "Water Flask", 1, Some(8));

        // Never near a combine, so nothing says what it was for.
        t.on_component(9000, "Kiola Nut", 1, Some(3));

        let r = t.report();
        let by = |name: &str| {
            r.components
                .iter()
                .find(|c| c.item == name)
                .unwrap()
                .skills
                .clone()
        };
        assert_eq!(by("Fenugreek"), vec!["Alchemy"]);
        assert_eq!(
            by("Water Flask"),
            vec!["Alchemy", "Blacksmithing"],
            "a shared component belongs to both, not to whichever came last"
        );
        assert!(by("Kiola Nut").is_empty(), "no recipe, no skill");
    }

    /// A learned recipe is attributed by having been combined; one that was
    /// only ever learned stays unattributed rather than being guessed at.
    #[test]
    fn learned_recipes_inherit_the_skill_of_their_combines() {
        let mut t = TradeskillTracker::new();
        t.on_recipe_learned("Potion of Accuracy");
        t.on_recipe_learned("Some Unmade Thing");
        t.on_craft(100, "Potion of Accuracy", false);
        t.on_skill_up(100, "Alchemy", 52);

        let r = t.report();
        let find = |name: &str| {
            r.recipes_learned
                .iter()
                .find(|x| x.recipe == name)
                .unwrap()
                .skill
                .clone()
        };
        assert_eq!(find("Potion of Accuracy").as_deref(), Some("Alchemy"));
        assert_eq!(find("Some Unmade Thing"), None);
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

    /// A combine hands its tools back, and the client says so with the same
    /// line it uses for what you made:
    ///
    /// ```text
    /// You have fashioned the items together to create something new: Small Piece of High Quality Ore.
    /// You have fashioned the items together to create something new: Smithing Chisel.
    /// ```
    ///
    /// Counting that invents a combine — and because it is then the most
    /// recent one, it also takes the real combine's components with it.
    #[test]
    fn a_returned_tool_is_not_a_combine() {
        let mut t = TradeskillTracker::new();
        t.on_purchase(50, "Water Flask", 10, 2000);
        t.on_craft(100, "Small Piece of High Quality Ore", true);
        t.on_craft(100, "Smithing Chisel", true);
        t.on_component(100, "Water Flask", 1, Some(9));

        let r = t.report();
        assert!(
            !r.crafts.iter().any(|c| c.item == "Smithing Chisel"),
            "the chisel is a tool, not something made: {:?}",
            r.crafts
        );
        assert_eq!(r.total_successes, 1, "one combine, not two");
        assert_eq!(r.costs.len(), 1);
        assert_eq!(
            r.costs[0].item, "Small Piece of High Quality Ore",
            "the flask went into the ore, not the chisel"
        );
    }

    /// The recipes that prompted the book: ore that is long past skilling
    /// Tharn up, so the log offers nothing to attribute it by.
    #[test]
    fn the_book_attributes_a_recipe_the_log_cannot() {
        let mut t = TradeskillTracker::new();
        t.on_craft(100, "Small Brick of High Quality Ore", true);
        let c = &t.report().crafts[0];
        assert_eq!(c.skill.as_deref(), Some("Blacksmithing"));
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
