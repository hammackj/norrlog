//! The event model: the typed vocabulary every log line is reduced to.
//!
//! EverQuest writes combat from the perspective of the logging character, so
//! the same physical event has several textual forms ("You slash X" vs
//! "X slashes YOU" vs "A slashes B"). Parsing normalises all of them into a
//! single [`Event`] whose `source`/`target` are [`Actor`]s, so downstream
//! aggregation never has to care which sentence form produced it.

use serde::{Deserialize, Serialize};

/// Stands in for an attacker the log declines to name.
///
/// A few lines report damage to you with no source at all. The damage is real
/// and belongs in your totals, but nothing in the log says what dealt it, and
/// a breakdown by attacker that quietly omitted it would not add up. So it is
/// carried under a name that says plainly that nobody knows, rather than being
/// dropped or pinned on the nearest plausible creature.
pub const UNSEEN_ATTACKER: &str = "an unseen attacker";

/// A participant in an event.
///
/// The log refers to the logging character as "You"/"YOU"/"YOUR"/"you" and to
/// everyone else by name. We resolve the self-references at parse time so that
/// aggregation can treat every actor uniformly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Actor {
    /// The character whose log this is.
    You,
    /// Any other named entity (mob, player, pet, NPC).
    Named(String),
}

impl Actor {
    /// Build an actor from raw log text, resolving self-references.
    pub fn parse(raw: &str) -> Actor {
        let t = raw.trim();
        match t {
            "You" | "YOU" | "you" | "Your" | "YOUR" | "your" | "yourself" | "YOURSELF" => {
                Actor::You
            }
            _ => Actor::Named(clean_name(t)),
        }
    }

    /// A stable lowercase key for grouping. Mob names appear both sentence-cased
    /// ("A lesser mummy") and mid-sentence ("a lesser mummy"); folding case
    /// keeps them from splitting into two actors.
    pub fn key(&self) -> String {
        match self {
            Actor::You => "you".to_string(),
            Actor::Named(n) => n.to_lowercase(),
        }
    }

    /// Human-readable form for display.
    pub fn display(&self, player: &str) -> String {
        match self {
            Actor::You => player.to_string(),
            Actor::Named(n) => n.clone(),
        }
    }

    pub fn is_you(&self) -> bool {
        matches!(self, Actor::You)
    }
}

/// Nouns the client uses for a pet named after its owner.
///
/// `warder` is the only one this variant has actually been seen to write —
/// 4,359 lines of it across the logs on hand — but the construction is the
/// game's, not the class's, so the siblings are included rather than waiting
/// for a log to prove each one. The list is kept deliberately short: a
/// creature legitimately called "Innoruuk`s Chosen" must not become
/// Innoruuk's pet, and only an exact match on one of these words prevents it.
const PET_NOUNS: [&str; 5] = ["warder", "pet", "companion", "familiar", "minion"];

/// The owner named inside a pet's own name, as in "Brennagh`s warder".
///
/// This attributes a pet with no help from any command, which matters because
/// `/pet leader` has to be typed and mostly is not. Returns the owner's name,
/// or `None` when the name is not of that form.
pub fn pet_owner_in_name(name: &str) -> Option<&str> {
    // The possessive form, "Brennagh`s warder". The client writes a backtick
    // where an apostrophe belongs; accept both.
    if let Some((owner, noun)) = name.split_once("`s ").or_else(|| name.split_once("'s ")) {
        let noun = noun.trim();
        if !owner.is_empty() && PET_NOUNS.contains(&noun.to_lowercase().as_str()) {
            return Some(owner.trim());
        }
    }

    // The bare form, "King Tranix pet", which is how the client names the pet
    // of a creature rather than of a player. Without this every such pet is a
    // second row belonging to nobody — 590 lines of "A dracoliche pet" in one
    // log, sitting beside the dracoliche it dies with.
    //
    // Only `pet` is accepted bare, not the rest of PET_NOUNS: "A ghoul minion"
    // and "An imp familiar" are entirely plausible names for creatures that
    // belong to nothing, while a creature genuinely called "... pet" is not.
    if let Some(owner) = name.strip_suffix(" pet") {
        let owner = owner.trim();
        if !owner.is_empty() {
            return Some(owner);
        }
    }

    None
}

/// Strip possessives and trailing corpse markers that leak into name captures.
fn clean_name(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_suffix("'s corpse").unwrap_or(s);
    let s = s.strip_suffix("`s corpse").unwrap_or(s);
    s.trim().to_string()
}

/// Modifiers the client appends in parentheses, e.g. `(Critical)`,
/// `(Riposte)`, `(Crippling Blow)`, `(Strikethrough)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flags(pub Vec<String>);

impl Flags {
    pub fn parse(raw: Option<&str>) -> Flags {
        match raw {
            None => Flags::default(),
            Some(s) => Flags(
                s.split(&[',', '/'][..])
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect(),
            ),
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.0.iter().any(|f| f.eq_ignore_ascii_case(name))
    }

    pub fn is_crit(&self) -> bool {
        self.has("Critical") || self.has("Crippling Blow") || self.has("Deadly Strike")
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// How a chunk of damage was delivered. Kept separate from the ability name so
/// aggregation can split melee vs. spell vs. damage-shield without string
/// matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DamageKind {
    /// Weapon/skill swing that landed.
    Melee,
    /// Direct spell damage ("... for N points of fire damage by Spell").
    DirectSpell,
    /// Damage-over-time tick ("X has taken N damage from ...").
    DamageOverTime,
    /// Damage shield / thorns / flames procs.
    DamageShield,
}

/// A coin amount, normalised to copper for arithmetic but retaining the
/// original denominations for display.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coin {
    pub platinum: u64,
    pub gold: u64,
    pub silver: u64,
    pub copper: u64,
}

impl Coin {
    /// Total value expressed in copper (1p = 10g = 100s = 1000c).
    pub fn as_copper(&self) -> u64 {
        self.platinum * 1000 + self.gold * 100 + self.silver * 10 + self.copper
    }

    /// Parse the client's prose form.
    ///
    /// Separators vary: corpse coin reads "12 platinum, 3 silver and 4 copper",
    /// while a merchant sale reads "1 gold 9 copper" with no punctuation at
    /// all. Rather than assume a separator, scan for `<number> <denomination>`
    /// pairs — splitting on commas silently dropped the copper from the
    /// unpunctuated form.
    pub fn parse(raw: &str) -> Coin {
        let mut c = Coin::default();
        let tokens: Vec<&str> = raw.split_whitespace().collect();
        for pair in tokens.windows(2) {
            let Ok(n) = pair[0].trim_end_matches(',').parse::<u64>() else {
                continue;
            };
            match pair[1].trim_end_matches([',', '.']) {
                "platinum" => c.platinum = n,
                "gold" => c.gold = n,
                "silver" => c.silver = n,
                "copper" => c.copper = n,
                _ => {}
            }
        }
        c
    }
}

/// What happened to an item the moment it was looted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LootDisposition {
    /// Went into inventory.
    Kept,
    /// Auto-sold; carries the proceeds. Zero when the client says "for free".
    Sold(Coin),
    /// Auto-routed into a container rather than your inventory. The client has
    /// several of these ("your tradeskill depot", "your currency", "your Dragon
    /// Hoard"), so the destination is captured rather than enumerated —
    /// an unrecognised one would otherwise be swallowed into the corpse name.
    Stored { destination: String },
    /// Combined into another item on pickup.
    Transmuted { into: String },
}

/// How the spellbook changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpellbookChangeKind {
    /// Finished memorising into a gem slot.
    Memorized,
    /// Removed from a gem slot.
    Forgotten,
    /// Newly granted/scribed, e.g. on levelling.
    Granted,
}

/// The parsed meaning of a single log line.
///
/// `Unknown` is deliberately part of the model rather than a parse failure:
/// EverQuest emits thousands of cosmetic strings, and measuring how many lines
/// land here is how we track parser coverage over time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// Any successful damage application.
    Damage {
        source: Actor,
        target: Actor,
        amount: u64,
        kind: DamageKind,
        /// Melee verb ("slash") or spell name, depending on `kind`.
        ability: Option<String>,
        /// Resist type for spell damage: fire, cold, magic, disease, poison...
        damage_type: Option<String>,
        flags: Flags,
    },
    /// A swing that failed to land. Tracked separately so hit rate is derivable.
    Miss {
        source: Actor,
        target: Actor,
        ability: Option<String>,
        flags: Flags,
    },
    /// Damage fully absorbed by a rune / magical skin.
    Absorbed { source: Actor, target: Actor },
    /// Healing applied.
    Heal {
        source: Actor,
        target: Actor,
        amount: u64,
        spell: Option<String>,
        /// True for regen-style ticks ("healed X over time for N").
        over_time: bool,
    },
    /// Something died. `killer` is absent when the log does not name one.
    Death {
        victim: Actor,
        killer: Option<Actor>,
    },
    /// An item entered your possession.
    Loot {
        item: String,
        quantity: u64,
        /// The corpse it came from, when named.
        source: Option<Actor>,
        disposition: LootDisposition,
    },
    /// Coin taken from a corpse.
    CoinLoot { coin: Coin },
    /// Coin received for selling an item to a merchant.
    MerchantSale { coin: Coin, item: String },
    /// Zone transition.
    ZoneChange { zone: String },
    /// You changed combat stance: balanced, defensive, offensive, evasive.
    ///
    /// A confounder for anything measured about being hit, which is why it is
    /// tracked rather than ignored: the same creature lands a very different
    /// share of its swings against a defensive stance than an offensive one.
    StanceChanged { stance: String },
    /// A cast or song started. This is the anchor used to disambiguate the
    /// buff-landed flavour text that follows.
    CastStart {
        source: Actor,
        spell: String,
        /// Bard songs use "begins singing" instead of "begins casting".
        singing: bool,
    },
    /// Cast did not complete.
    CastInterrupted { source: Actor },
    /// Flavour text indicating a spell effect landed. The spell is not named by
    /// the client, so resolution happens later against the spell database.
    EffectLanded {
        target: Actor,
        /// Verbatim flavour text, used as the lookup key.
        text: String,
    },
    /// Flavour text indicating a spell effect expired.
    EffectFaded {
        target: Actor,
        /// Verbatim flavour text, used as the lookup key.
        text: String,
    },
    /// Experience gained, as a percentage of the current level.
    Experience { percent: f64 },
    /// Faction adjustment.
    Faction {
        faction: String,
        adjustment: Option<i64>,
    },
    /// A private message, in either direction.
    ///
    /// Distinct from [`Event::Chat`] because a conversation needs both ends:
    /// channel chat has a speaker and nothing else, while a tell has a
    /// counterpart, and which of the two is you decides the direction.
    Tell {
        from: Actor,
        to: Actor,
        message: String,
    },
    /// Player chat on any channel.
    Chat {
        speaker: Actor,
        channel: String,
        message: String,
    },
    /// Group roster change.
    GroupChange { who: Actor, joined: bool },
    /// An activated ability is still cooling down. Useful for the overlay: it
    /// is the only place the client reports remaining recast time.
    AbilityCooldown {
        ability: String,
        /// Remaining cooldown in seconds.
        remaining: u64,
    },
    /// A worn item's proc or click effect fired.
    ItemProc {
        item: String,
        /// The effect name the client shows in parentheses, e.g. "Exaltation".
        effect: String,
    },
    /// Cosmetic combat feedback ("X winces.", "X is bound by...").
    Emote { actor: Actor, text: String },
    /// The logging character gained a level. Tracked because buff durations
    /// scale with caster level.
    LevelUp { level: u32 },
    /// One row of `/who` output: a level and a class loadout together.
    WhoEntry {
        level: u32,
        /// As written, e.g. "WAR/BRD/SHM".
        classes: String,
        name: String,
        race: String,
        /// Empty when unguilded.
        guild: String,
        zone: String,
    },

    /// An attack was actively avoided by the defender, as opposed to simply
    /// missing. Tracked separately so mitigation can be reported.
    Avoided {
        source: Actor,
        target: Actor,
        /// "block", "parry", "dodge", "riposte".
        how: String,
    },
    /// A spell was resisted by its target.
    Resisted { target: Actor, spell: String },
    /// The character logged in. Starts a play session.
    LoggedOn,
    /// An item handed to an NPC. Several may precede one [`Event::TradeComplete`].
    TradeOffer {
        quantity: u64,
        item: String,
        npc: String,
    },
    /// The trade was accepted, consuming everything offered.
    TradeComplete { npc: String },
    /// A skill point bought from an NPC trainer rather than earned by use.
    SkillTrained { trainer: String, skill: String },
    /// The target cannot be charmed at all. Names nothing — the client says
    /// only "This NPC cannot be charmed." — so the caller must supply the
    /// target from whatever it was last trying to charm.
    CharmImmune,
    /// A spell landed on nothing. Also nameless, for the same reason.
    TargetUnaffected,
    /// A pet under your control announced its target.
    ///
    /// This is how a charmed creature identifies itself: EverQuest pets answer
    /// orders with "Attacking <target> Master.", and only something you control
    /// calls you Master. For a charming class this is the primary evidence that
    /// a mob has switched to your side.
    ///
    /// A summoned pet answers exactly the same way, so this says "mine", not
    /// "charmed" — see [`Event::PetLeader`] and the pet naming convention for
    /// how the two are told apart.
    PetAttack { pet: Actor, target: Actor },
    /// A pet naming its owner, in reply to `/pet leader`.
    ///
    /// The only line in the log that attributes a pet to a player outright.
    /// Summoned pets carry generated names with nothing in them to go on, so
    /// without this a magician's pet is just another actor in the meter — and
    /// it works for other people's pets as well as your own.
    PetLeader { pet: Actor, leader: Actor },
    /// One of your spells expired *from a named target*, as opposed to from
    /// yourself. Charm breaks are reported this way, naming the creature.
    EffectWoreOffTarget { spell: String, target: Actor },
    /// A spell landed on someone else. `text` is the raw landing phrase, which
    /// is shared by several spells — the caller must resolve which.
    EffectLandedOnTarget { target: Actor, text: String },
    /// A skill increased.
    SkillUp { skill: String, value: u32 },
    /// A tradeskill combine finished.
    ///
    /// The client never names the tradeskill here — see [`crate::tradeskill`]
    /// for how it is attributed.
    Craft { item: String, success: bool },
    /// A component was consumed by a combine.
    ComponentConsumed {
        item: String,
        quantity: u64,
        /// Stock left afterwards, as reported by the client.
        remaining: u64,
    },
    /// Items bought from a merchant. The only place the log states what
    /// anything costs, which is what makes a combine costable at all.
    ItemPurchased {
        item: String,
        quantity: u64,
        merchant: String,
        /// Paid for the whole stack, not per unit.
        coin: Coin,
    },
    /// A merchant's asking price, from clicking an item in their window.
    ///
    /// No coin moves, which is what makes this worth having: a shop's whole
    /// stock can be priced with one click per item instead of buying it. The
    /// price is per single item in both wordings the client uses, and matched
    /// the unit price actually paid in all 643 cases seen both ways.
    ///
    /// Like a purchase price, it is **this character's** price — merchants
    /// charge according to charisma and faction — so it is what one person
    /// was quoted rather than a universal figure.
    ItemQuoted {
        item: String,
        merchant: String,
        /// For one item, not for a stack.
        coin: Coin,
    },
    /// A new recipe became available.
    RecipeLearned { recipe: String },
    /// A task was assigned.
    TaskAssigned { task: String },
    /// A task advanced. The client gives no objective text, so each of these
    /// is simply one step completed.
    TaskUpdated { task: String },
    /// An achievement was earned.
    AchievementEarned { name: String },
    /// Alternate advancement points earned. EverQuest Legends calls these
    /// "ability points".
    AbilityPointsGained {
        /// Points earned by this event.
        points: u32,
        /// Unspent balance afterwards — *not* lifetime earned, since points are
        /// spent as they accumulate.
        unspent: u32,
    },
    /// Ability points spent improving an alternate advancement.
    AbilityPointsSpent {
        ability: String,
        /// The rank reached.
        rank: u32,
        cost: u32,
    },
    /// A spell set was loaded. The name is player-authored.
    SpellSetLoaded { name: String },
    /// One entry from an `/alt list` dump. Repeated for each rank of an
    /// ability, so entries must be counted, not deduplicated.
    AbilityListEntry { id: u32, ability: String },
    /// The description line between an entry and its cost.
    AbilityListDescription { text: String },
    /// The cost line that follows an [`Event::AbilityListEntry`].
    AbilityListCost { cost: u32 },
    /// The character's spellbook changed. This is the strongest available
    /// signal for both buff disambiguation and class inference, because it
    /// names spells the character definitively owns — including ones they are
    /// never seen to cast.
    SpellbookChange {
        spell: String,
        change: SpellbookChangeKind,
    },
    /// A recognised client/system message that carries no combat meaning.
    System { text: String },
    /// A line we recognised as uninteresting or do not yet model.
    Unknown,
}

/// A log line after parsing: the timestamp, the typed event, and the original
/// text (retained so the UI can show raw context and so unparsed lines can be
/// inspected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Unix seconds. EverQuest timestamps have no timezone, so these are
    /// interpreted as local time.
    pub timestamp: i64,
    pub event: Event,
    pub raw: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coin_parses_prose_and_converts() {
        let c = Coin::parse("12 platinum, 3 silver and 4 copper");
        assert_eq!(c.platinum, 12);
        assert_eq!(c.silver, 3);
        assert_eq!(c.copper, 4);
        assert_eq!(c.gold, 0);
        assert_eq!(c.as_copper(), 12_034);
    }

    #[test]
    fn coin_parses_single_denomination() {
        assert_eq!(Coin::parse("2 silver").as_copper(), 20);
    }

    /// Merchant sales omit punctuation entirely; splitting on commas dropped
    /// everything after the first denomination.
    #[test]
    fn coin_parses_unpunctuated_form() {
        assert_eq!(Coin::parse("1 gold 9 copper").as_copper(), 109);
        assert_eq!(Coin::parse("5 silver 6 copper").as_copper(), 56);
        assert_eq!(
            Coin::parse("2 platinum 3 gold 4 silver 5 copper").as_copper(),
            2_345
        );
    }

    #[test]
    fn actor_folds_self_references() {
        assert_eq!(Actor::parse("YOU"), Actor::You);
        assert_eq!(Actor::parse("your"), Actor::You);
        assert_eq!(
            Actor::parse("Lord Nagafen"),
            Actor::Named("Lord Nagafen".into())
        );
    }

    #[test]
    fn actor_key_folds_article_case() {
        assert_eq!(
            Actor::parse("A lesser mummy").key(),
            Actor::parse("a lesser mummy").key()
        );
    }

    #[test]
    fn actor_strips_corpse_suffix() {
        assert_eq!(
            Actor::parse("Lord Nagafen's corpse"),
            Actor::Named("Lord Nagafen".into())
        );
    }

    #[test]
    fn flags_detect_crits() {
        assert!(Flags::parse(Some("Critical")).is_crit());
        assert!(Flags::parse(Some("Crippling Blow")).is_crit());
        assert!(!Flags::parse(Some("Riposte")).is_crit());
        assert!(Flags::parse(None).is_empty());
    }
}
