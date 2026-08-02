//! Line parsing: raw EverQuest log text into [`LogEntry`] values.
//!
//! # Approach
//!
//! Every rule is a regex. Matching them one at a time over a 48 MB log would
//! mean ~40 scans per line, so we compile all patterns into a [`RegexSet`],
//! which finds every candidate rule in a single pass, then re-run only the
//! candidates to extract captures.
//!
//! Rule order encodes precedence: the first candidate that yields a complete
//! match wins. Ordering matters because the sentence forms genuinely overlap —
//! for example a spell hit ("... for 4 points of *magic* damage by Shallow
//! Breath") and a melee hit ("... for 33 points of damage") differ only by the
//! damage-type word, so the spell rule must be tried first.

use crate::event::*;
use regex::{Regex, RegexSet};
use std::sync::LazyLock;

/// Identifies which rule matched, so the extraction step knows how to read the
/// capture groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleId {
    SpellDamage,
    DotTickYours,
    DotTickOther,
    DamageShieldYours,
    DamageShieldOther,
    NonMeleeGeneric,
    MeleeYou,
    MeleeOther,
    MissYou,
    MissOther,
    AbsorbBlow,
    AbsorbThorns,
    HealSelf,
    HealOther,
    SlainByYou,
    SlainByOther,
    YouWereSlain,
    LootMarked,
    LootSold,
    LootStored,
    LootTransmute,
    LootPlain,
    CoinFromCorpse,
    MerchantSale,
    ZoneEntered,
    CastStartYou,
    SingStartYou,
    CastStartOther,
    MelodyInterrupted,
    SpellInterrupted,
    Experience,
    FactionAdjusted,
    FactionCapped,
    ChatChannel,
    ChatTellYou,
    ChatSays,
    GroupJoined,
    GroupLeft,
    LevelUp,
    AbilityPointsGained,
    AbilityFirstRank,
    AbilityPointsSpent,
    SpellSetLoaded,
    AbilityListEntry,
    AbilityListDescription,
    AbilityListCost,
    Avoided,
    ResistedYours,
    ResistedGeneric,
    SkillUp,
    TaskAssigned,
    TaskUpdated,
    AchievementEarned,
    CraftSuccess,
    CraftFailure,
    ComponentConsumed,
    RecipeLearned,
    SongFizzle,
    SpellMemorized,
    SpellForgotten,
    SpellGranted,
    PetAttack,
    EffectWoreOffTarget,
    EffectLandedOnTarget,
    SkillTrained,
    CharmImmune,
    TargetUnaffected,
    AbilityCooldown,
    ItemProc,
    EmoteWince,
    EmoteGeneric,
    SystemAutoAttack,
    SystemMessage,
}

/// Rule table. **Order is precedence** — earlier rules win.
static RULES: &[(RuleId, &str)] = &[
    // ---- Spell / DoT damage -------------------------------------------------
    // Must precede melee: distinguished by the damage-type word before "damage".
    // "You hit a lesser mummy for 4 points of magic damage by Shallow Breath."
    // "a lesser mummy hit you for 4 points of disease damage by Rabies."
    (
        RuleId::SpellDamage,
        r"^(?P<src>.+?) hits? (?P<tgt>.+?) for (?P<amt>\d+) points? of (?P<dtype>[a-z]+) damage by (?P<spell>.+?)\.(?: \((?P<flags>[^)]*)\))?$",
    ),
    // "An imp protector has taken 37 damage from your Chords of Dissonance III."
    (
        RuleId::DotTickYours,
        r"^(?P<tgt>.+?) has taken (?P<amt>\d+) damage from your (?P<spell>.+?)\.(?: \((?P<flags>[^)]*)\))?$",
    ),
    // "A young kodiak has taken 8 damage from Clinging Darkness by Sticky."
    // "You have taken 1 damage from Rabies by a lesser mummy."
    (
        RuleId::DotTickOther,
        r"^(?P<tgt>.+?) ha[sv]e? taken (?P<amt>\d+) damage from (?P<spell>.+?) by (?P<src>.+?)\.(?: \((?P<flags>[^)]*)\))?$",
    ),
    // ---- Damage shields (thorns / flames) -----------------------------------
    // "A wan ghoul knight is pierced by YOUR thorns for 27 points of non-melee damage."
    (
        RuleId::DamageShieldYours,
        r"^(?P<tgt>.+?) (?:is|are) (?P<verb>\w+) by YOUR (?P<medium>thorns|flames) for (?P<amt>\d+) points? of non-melee damage[.!]$",
    ),
    // "YOU are burned by a lava guardian's flames for 48 points of non-melee damage!"
    (
        RuleId::DamageShieldOther,
        r"^(?P<tgt>.+?) (?:is|are) (?P<verb>\w+) by (?P<src>.+?)[`']s (?P<medium>thorns|flames) for (?P<amt>\d+) points? of non-melee damage[.!]$",
    ),
    // "Lord Nagafen was hit by non-melee for 500 points of damage."
    (
        RuleId::NonMeleeGeneric,
        r"^(?P<tgt>.+?) was hit by non-melee for (?P<amt>\d+) points? of damage\.$",
    ),
    // ---- Melee --------------------------------------------------------------
    // Second person uses the bare verb; third person uses the inflected form.
    // The verb alternation is substituted in at compile time (see `{VERB}`).
    // It must live *inside* the pattern rather than being checked afterwards:
    // with a bare `[a-z]+` the engine commits to the leftmost lazy split, so
    // "A giant fire beetle bites YOU" would yield verb="giant" and a later
    // rejection could not make it backtrack to the correct split.
    // "You slash Lord Nagafen for 160 points of damage. (Critical)"
    (
        RuleId::MeleeYou,
        r"^You (?P<verb>{VERB}) (?P<tgt>.+?) for (?P<amt>\d+) points? of damage\.(?: \((?P<flags>[^)]*)\))?$",
    ),
    // "An imp protector hits YOU for 33 points of damage. (Riposte)"
    (
        RuleId::MeleeOther,
        r"^(?P<src>.+?) (?P<verb>{VERB}) (?P<tgt>.+?) for (?P<amt>\d+) points? of damage\.(?: \((?P<flags>[^)]*)\))?$",
    ),
    // ---- Misses -------------------------------------------------------------
    (
        RuleId::MissYou,
        r"^You try to (?P<verb>[a-z]+) (?P<tgt>.+?), but miss!(?: \((?P<flags>[^)]*)\))?$",
    ),
    (
        RuleId::MissOther,
        r"^(?P<src>.+?) tries to (?P<verb>[a-z]+) (?P<tgt>.+?), but misses!(?: \((?P<flags>[^)]*)\))?$",
    ),
    // ---- Active avoidance ---------------------------------------------------
    // Distinct from a plain miss: the defender did something.
    // "An imp protector tries to hit YOU, but YOU block!"
    (
        RuleId::Avoided,
        r"^(?P<src>.+?) tries to {VERB} (?P<tgt>.+?), but (?P<defender>.+?) (?P<how>blocks?|parr(?:y|ies)|dodges?|ripostes?)!$",
    ),
    // ---- Resists ------------------------------------------------------------
    (
        RuleId::ResistedYours,
        r"^(?P<tgt>.+?) resisted your (?P<spell>.+?)!$",
    ),
    (
        RuleId::ResistedGeneric,
        r"^Your target resisted the (?P<spell>.+?) spell\.$",
    ),
    // ---- Absorbs ------------------------------------------------------------
    // "A dar ghoul knight tries to cleave X, but X's magical skin absorbs the blow!"
    (
        RuleId::AbsorbBlow,
        r"^(?P<src>.+?) tries to [a-z]+ (?P<tgt>.+?), but .+?[`']s magical skin absorbs the blow!$",
    ),
    // "A fire giant warrior's magical skin absorbs the damage of YOUR thorns."
    (
        RuleId::AbsorbThorns,
        r"^(?P<tgt>.+?)[`']s magical skin absorbs the damage of (?P<src>.+?) thorns\.$",
    ),
    // ---- Heals --------------------------------------------------------------
    // Reflexive form must precede the general one; "himself/herself/itself"
    // would otherwise be captured as a distinct target named "himself".
    // "Persephone healed herself for 4 hit points by Blessing of the Knight."
    (
        RuleId::HealSelf,
        r"^(?P<src>.+?) healed (?:himself|herself|itself|yourself)(?P<hot> over time)? for (?P<amt>\d+) hit points?(?: by (?P<spell>.+?))?\.$",
    ),
    // "You healed Tharn over time for 153 hit points by Efflorescing Heal."
    (
        RuleId::HealOther,
        r"^(?P<src>.+?) healed (?P<tgt>.+?)(?P<hot> over time)? for (?P<amt>\d+) hit points?(?: by (?P<spell>.+?))?\.$",
    ),
    // ---- Deaths -------------------------------------------------------------
    (RuleId::SlainByYou, r"^You have slain (?P<victim>.+?)!$"),
    (
        RuleId::SlainByOther,
        r"^(?P<victim>.+?) has been slain by (?P<killer>.+?)!$",
    ),
    (RuleId::YouWereSlain, r"^You have been slain by (?P<killer>.+?)!$"),
    // ---- Loot ---------------------------------------------------------------
    // "--You have looted a Selo`s Drums of the March +3 from Lord Nagafen's corpse.--"
    (
        RuleId::LootMarked,
        r"^--You have looted (?:a |an |the )?(?P<qty>\d+ )?(?P<item>.+?) from (?P<corpse>.+?)\.--$",
    ),
    // "You looted a Fire Beetle Eye from a fire beetle's corpse and sold it for 2 silver."
    (
        RuleId::LootSold,
        r"^You looted (?:a |an |the )?(?P<qty>\d+ )?(?P<item>.+?) from (?P<corpse>.+?) and sold it for (?P<coin>.+?)\.$",
    ),
    // "You looted 2 Spider Silk from a large spider's corpse and stored it in
    //  your tradeskill depot" — also "your currency", "your Dragon Hoard".
    // The destination is captured generically: an unmatched variant falls
    // through to the plain rule, which would absorb the whole trailing clause
    // into the corpse name and corrupt drop-source reporting.
    (
        RuleId::LootStored,
        r"^You looted (?:a |an |the )?(?P<qty>\d+ )?(?P<item>.+?) from (?P<corpse>.+?) and stored it in (?:your |the )?(?P<dest>.+?)\.?$",
    ),
    // "You looted a Cloak of Flames +3 from Lord Nagafen's corpse to create a Cloak of Flames +3"
    (
        RuleId::LootTransmute,
        r"^You looted (?:a |an |the )?(?P<qty>\d+ )?(?P<item>.+?) from (?P<corpse>.+?) to create (?:a |an |the )?(?P<into>.+?)\.?$",
    ),
    (
        RuleId::LootPlain,
        r"^You looted (?:a |an |the )?(?P<qty>\d+ )?(?P<item>.+?) from (?P<corpse>.+?)\.?$",
    ),
    (
        RuleId::CoinFromCorpse,
        r"^You receive (?P<coin>.+?) from the corpse\.$",
    ),
    // "You receive 1 gold 9 copper from Klok Koglin for the Belt Pouch(s)."
    (
        RuleId::MerchantSale,
        r"^You receive (?P<coin>.+?) from (?P<merchant>.+?) for the (?P<item>.+?)\.$",
    ),
    // ---- World / progression ------------------------------------------------
    (RuleId::ZoneEntered, r"^You have entered (?P<zone>.+?)\.$"),
    (RuleId::CastStartYou, r"^You begin casting (?P<spell>.+?)\.$"),
    (RuleId::SingStartYou, r"^You begin singing (?P<spell>.+?)\.$"),
    (
        RuleId::CastStartOther,
        r"^(?P<src>.+?) begins (?:casting|singing) (?P<spell>.+?)\.$",
    ),
    (RuleId::MelodyInterrupted, r"^Your melody has been interrupted!$"),
    (
        RuleId::SpellInterrupted,
        r"^Your (?:spell is interrupted|casting has been interrupted)\.?!?$",
    ),
    (
        RuleId::Experience,
        r"^You gain (?:party )?experience!+(?: \((?P<pct>[\d.]+)%\))?$",
    ),
    (
        RuleId::FactionAdjusted,
        r"^Your faction standing with (?P<faction>.+?) has been adjusted by (?P<adj>-?\d+)\.$",
    ),
    (
        RuleId::FactionCapped,
        r"^Your faction standing with (?P<faction>.+?) could not possibly get any (?:worse|better)\.$",
    ),
    // ---- Chat ---------------------------------------------------------------
    // "Seizure tells NewPlayers:1, 'none'"
    (
        RuleId::ChatChannel,
        r"^(?P<src>.+?) tells (?P<chan>[^,]+:\d+), '(?P<msg>.*)'$",
    ),
    (
        RuleId::ChatTellYou,
        r"^(?P<src>.+?) tells you, '(?P<msg>.*)'$",
    ),
    (
        RuleId::ChatSays,
        r"^(?P<src>.+?) (?P<verb>say|says|shouts|auctions|says out of character), '(?P<msg>.*)'$",
    ),
    (
        RuleId::GroupJoined,
        r"^(?P<who>.+?) ha(?:s|ve) joined the group\.$",
    ),
    (
        RuleId::GroupLeft,
        r"^(?P<who>.+?) ha(?:s|ve) left the group\.$",
    ),
    // ---- Charm / pets -------------------------------------------------------
    // "Guntrik told you, 'You have increased your skill in Pottery.'"
    //
    // A skill point bought from an NPC trainer, not earned by doing the thing.
    // Six tradeskills on a real character went 2 -> 20 this way with not one
    // combine involved, which a skill-up count alone reports as progress you
    // made rather than progress you paid for.
    (
        RuleId::SkillTrained,
        r"^(?P<trainer>.+?) told you, 'You have increased your skill in (?P<skill>.+?)\.'$",
    ),
    // "An imp protector told you, 'Attacking an imp protector Master.'"
    // Only a creature under your control addresses you as Master, so this
    // identifies a charmed pet by name.
    (
        RuleId::PetAttack,
        r"^(?P<pet>.+?) told you, 'Attacking (?P<target>.+?) Master\.'$",
    ),
    // "Your Solon's Bewitching Bravura spell has worn off of an imp protector."
    // Note the trailing "of <target>", which distinguishes a charm break from
    // a buff expiring on yourself.
    (
        RuleId::EffectWoreOffTarget,
        r"^Your (?P<spell>.+?) spell has worn off of (?P<target>.+?)\.$",
    ),
    // A spell landing on someone else, e.g. "an imp protector's eyes glaze
    // over." Which spell it was cannot be told from the line — eleven share
    // this text — so the raw text is carried and the caller resolves it
    // against what you actually cast. Listed here rather than as an emote
    // because it is the only signal that a charm took hold: the pet only
    // announces itself when ordered to attack, which routinely never happens.
    // Charm refused outright rather than resisted. Rare but decisive: no
    // amount of resinging will land it.
    (RuleId::CharmImmune, r"^This NPC cannot be charmed\.$"),
    // The generic "it did nothing" reply, seen when a spell fails to take.
    (RuleId::TargetUnaffected, r"^Your target looks unaffected\.$"),
    (
        RuleId::EffectLandedOnTarget,
        r"^(?P<target>.+?)(?P<text>'s eyes glaze over)\.$",
    ),
    // ---- Spellbook ----------------------------------------------------------
    (
        RuleId::SpellMemorized,
        r"^You have finished memorizing (?P<spell>.+?)\.$",
    ),
    (RuleId::SpellForgotten, r"^You forget (?P<spell>.+?)\.$"),
    (
        RuleId::SpellGranted,
        r"^You have been granted the following spell: (?P<spell>.+?)\.$",
    ),
    // ---- Tasks and achievements ---------------------------------------------
    (
        RuleId::TaskAssigned,
        r"^You have been assigned the task '(?P<task>.+)'\.$",
    ),
    (
        RuleId::TaskUpdated,
        r"^Your task '(?P<task>.+)' has been updated\.$",
    ),
    (
        RuleId::AchievementEarned,
        r"^You have completed achievement: (?P<name>.+?)$",
    ),
    // ---- Tradeskills --------------------------------------------------------
    (
        RuleId::CraftSuccess,
        r"^You have fashioned the items together to create something new: (?P<item>.+?)\.$",
    ),
    (
        RuleId::CraftFailure,
        r"^You lacked the skills to fashion (?P<item>.+?)\.$",
    ),
    // "Consumed 1 x Fenugreek (leaving 314) from your personal depot."
    (
        RuleId::ComponentConsumed,
        r"^Consumed (?P<qty>\d+) x (?P<item>.+?) \(leaving (?P<left>\d+)\) from your .+?\.$",
    ),
    (
        RuleId::RecipeLearned,
        r"^You have learned the recipe (?P<recipe>.+?)!$",
    ),
    // "You have become better at 1H Slashing! (22)"
    (
        RuleId::SkillUp,
        r"^You have become better at (?P<skill>.+?)! \((?P<value>\d+)\)$",
    ),
    // "You miss a note, bringing your Solon's Bewitching Bravura to a close!"
    (
        RuleId::SongFizzle,
        r"^You miss a note, bringing your (?P<spell>.+?) to a close!$",
    ),
    // ---- Alternate advancement ---------------------------------------------
    // "You have gained 2 ability point(s)!  You now have 5 ability point(s)."
    // "You have gained an ability point!  You now have 6 ability points."
    // Note the doubled space before "You now have".
    (
        RuleId::AbilityPointsGained,
        r"^You have gained (?:(?P<points>\d+) ability point\(s\)|an ability point)!\s+You now have (?P<unspent>\d+) ability point",
    ),
    // The *first* rank of an ability is announced differently from later ranks,
    // and quotes the name:
    // "You have gained the ability "Warriors Endurance" at a cost of 3 ability points."
    // Missing this form hides every rank-1 purchase — 32 of 67 in a real log.
    (
        RuleId::AbilityFirstRank,
        r#"^You have gained the ability "(?P<ability>[^"]+)" at a cost of (?P<cost>\d+) ability points?\.$"#,
    ),
    // "You have improved Symphonic Aura: Enabled 2 at a cost of 0 ability points."
    // Ability names may contain punctuation, so the trailing digits are the rank.
    (
        RuleId::AbilityPointsSpent,
        r"^You have improved (?P<ability>.+?) (?P<rank>\d+) at a cost of (?P<cost>\d+) ability points?\.$",
    ),
    // `/alt list` dumps the ability catalogue for your class combo, one entry
    // per rank:
    //   "Ability #3815: Destructive Cascade"
    //   "Description: ...increases the damage dealt by ... by 125%."
    //   "Cost per Level: 2"
    // The description can spill onto continuation lines carrying no timestamp,
    // so only the first line of it is captured — the rank's cost and identity
    // are what matter, and they are always on their own lines.
    // "Spell set WAR / BRD / DRU loaded."
    // "Spell set WAR / DRU / BRD - Buffs loaded."
    // Player-authored set names, so not authoritative — but a character names
    // their spell sets after the classes they actually play, which is far
    // better evidence than inferring a class that casts nothing.
    (
        RuleId::SpellSetLoaded,
        r"^Spell set (?P<name>.+?) loaded\.$",
    ),
    (
        RuleId::AbilityListEntry,
        r"^Ability #(?P<id>\d+): (?P<ability>.+)$",
    ),
    (
        RuleId::AbilityListDescription,
        r"^Description: (?P<text>.*)$",
    ),
    (
        RuleId::AbilityListCost,
        r"^Cost per Level: (?P<cost>\d+)$",
    ),
    // "You have gained a level! Welcome to level 42!"
    (
        RuleId::LevelUp,
        r"^You have gained a level!\s*Welcome to level (?P<level>\d+)!$",
    ),
    // ---- Abilities / items --------------------------------------------------
    // "You can use the ability Rampage again in 8 minute(s) 54 seconds."
    (
        RuleId::AbilityCooldown,
        r"^You can use the ability (?P<ability>.+?) again in (?:(?P<h>\d+) hour\(s\) )?(?P<m>\d+) minute\(s\) (?P<s>\d+) seconds?\.$",
    ),
    // "Your Djarn's Amethyst Ring (Exaltation) shimmers briefly."
    (
        RuleId::ItemProc,
        r"^Your (?P<item>.+?) \((?P<effect>[^)]+)\) .+\.$",
    ),
    // ---- Cosmetic combat feedback ------------------------------------------
    (RuleId::EmoteWince, r"^(?P<actor>.+?) winces\.$"),
    (
        RuleId::EmoteGeneric,
        r"^(?P<actor>.+?) (?P<text>is bound by strands of solid music|begins to chant|begins to choke|staggers|looks sick|has been (?:diseased|poisoned)|is (?:enveloped|engulfed) .+)\.$",
    ),
    // ---- Client/system feedback --------------------------------------------
    (RuleId::SystemAutoAttack, r"^Auto attack is (?:on|off)\.$"),
    (
        RuleId::SystemMessage,
        r"^(?:You cannot see your target|Your target is too far away, get closer|You already have your target's attention|You must first select a target for this spell|You must first click on the being you wish to attack|You can't cast spells while stunned|Your attempt to disarm failed|You can't use that command right now\.\.\.|You regain your concentration and continue your casting|Your spell fizzles|Insufficient Mana to cast this spell|You cannot use this ability right now|LOADING, PLEASE WAIT\.\.\.|Making all existing corpses visible|It will take you about \d+ seconds to prepare your camp|You abandon your preparations to camp)[.!]?$",
    ),
];

/// Compiled rule set: the dispatch [`RegexSet`] plus the individual regexes.
struct Compiled {
    set: RegexSet,
    regexes: Vec<Regex>,
    ids: Vec<RuleId>,
}

static COMPILED: LazyLock<Compiled> = LazyLock::new(|| {
    let verbs = melee_verb_alternation();
    let pats: Vec<String> = RULES
        .iter()
        .map(|(_, p)| p.replace("{VERB}", &verbs))
        .collect();
    Compiled {
        set: RegexSet::new(&pats).expect("rule patterns must compile"),
        regexes: pats
            .iter()
            .map(|p| Regex::new(p).expect("rule patterns must compile"))
            .collect(),
        ids: RULES.iter().map(|(id, _)| *id).collect(),
    }
});

/// Verbs that appear in melee lines. Without this constraint the generic
/// third-person rule would match any sentence shaped
/// "<x> <word> <y> for <n> points of damage."
const MELEE_VERBS: &[&str] = &[
    "hit", "hits", "slash", "slashes", "kick", "kicks", "bash", "bashes", "cleave", "cleaves",
    "crush", "crushes", "pierce", "pierces", "punch", "punches", "bite", "bites", "claw", "claws",
    "slice", "slices", "sting", "stings", "maul", "mauls", "gore", "gores", "smash", "smashes",
    "rend", "rends", "burn", "burns", "shoot", "shoots", "strike", "strikes", "backstab",
    "backstabs", "frenzy", "frenzies", "slam", "slams", "tail rake", "tail rakes",
];

/// Fold the inflected avoidance verbs onto a single lowercase stem so
/// "blocks"/"block" and "parries"/"parry" aggregate together.
fn normalise_avoidance(raw: &str) -> String {
    match raw.to_lowercase().as_str() {
        "blocks" | "block" => "block",
        "parries" | "parry" => "parry",
        "dodges" | "dodge" => "dodge",
        "ripostes" | "riposte" => "riposte",
        other => return other.to_string(),
    }
    .to_string()
}

/// Build the `(?:a|b|c)` alternation, longest first. The regex crate uses
/// leftmost-first alternation semantics, so ordering by descending length keeps
/// "slashes" from being matched as "slash" with a stray "es" left over.
fn melee_verb_alternation() -> String {
    let mut v: Vec<&str> = MELEE_VERBS.to_vec();
    v.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    format!("(?:{})", v.join("|"))
}

/// Parses raw log lines. Holds the logging character's name so `You` can be
/// resolved for display, and so self-referential lines are attributed correctly.
pub struct Parser {
    player: String,
}

impl Parser {
    pub fn new(player: impl Into<String>) -> Self {
        Parser {
            player: player.into(),
        }
    }

    /// Infer the character name from an EverQuest log filename, which follows
    /// the convention `eqlog_<Character>_<server>.txt`.
    pub fn player_from_filename(path: &str) -> Option<String> {
        let base = path.rsplit('/').next()?;
        let rest = base.strip_prefix("eqlog_")?;
        let mut parts = rest.split('_');
        let name = parts.next()?;
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }

    pub fn player(&self) -> &str {
        &self.player
    }

    /// Build an actor, folding the logging character's own name onto
    /// [`Actor::You`].
    ///
    /// The client mixes both forms freely — "You healed Tharn", "Tharn goes into
    /// a berserker frenzy" — and treating them as different actors would split
    /// one character's stats across two rows, halving self-healing and hiding
    /// damage taken.
    fn actor(&self, raw: &str) -> Actor {
        let a = Actor::parse(raw);
        match &a {
            Actor::Named(n) if n.eq_ignore_ascii_case(&self.player) => Actor::You,
            _ => a,
        }
    }

    /// Parse one full log line, including the bracketed timestamp.
    ///
    /// Returns `None` only when the line has no parseable timestamp (blank
    /// lines and the occasional un-timestamped banner the client writes).
    pub fn parse_line(&self, line: &str) -> Option<LogEntry> {
        let (timestamp, body) = split_timestamp(line)?;
        Some(LogEntry {
            timestamp,
            event: self.parse_body(body),
            raw: line.to_string(),
        })
    }

    /// Classify the message portion of a line (timestamp already stripped).
    pub fn parse_body(&self, body: &str) -> Event {
        for idx in COMPILED.set.matches(body).into_iter() {
            let Some(caps) = COMPILED.regexes[idx].captures(body) else {
                continue;
            };
            let g = |n: &str| caps.name(n).map(|m| m.as_str());
            let num = |n: &str| g(n).and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);

            let ev = match COMPILED.ids[idx] {
                RuleId::SpellDamage => Event::Damage {
                    source: self.actor(g("src").unwrap_or_default()),
                    target: self.actor(g("tgt").unwrap_or_default()),
                    amount: num("amt"),
                    kind: DamageKind::DirectSpell,
                    ability: g("spell").map(str::to_string),
                    damage_type: g("dtype").map(str::to_string),
                    flags: Flags::parse(g("flags")),
                },
                RuleId::DotTickYours => Event::Damage {
                    source: Actor::You,
                    target: self.actor(g("tgt").unwrap_or_default()),
                    amount: num("amt"),
                    kind: DamageKind::DamageOverTime,
                    ability: g("spell").map(str::to_string),
                    damage_type: None,
                    flags: Flags::parse(g("flags")),
                },
                RuleId::DotTickOther => Event::Damage {
                    source: self.actor(g("src").unwrap_or_default()),
                    target: self.actor(g("tgt").unwrap_or_default()),
                    amount: num("amt"),
                    kind: DamageKind::DamageOverTime,
                    ability: g("spell").map(str::to_string),
                    damage_type: None,
                    flags: Flags::parse(g("flags")),
                },
                RuleId::DamageShieldYours => Event::Damage {
                    source: Actor::You,
                    target: self.actor(g("tgt").unwrap_or_default()),
                    amount: num("amt"),
                    kind: DamageKind::DamageShield,
                    ability: g("medium").map(str::to_string),
                    damage_type: Some("non-melee".into()),
                    flags: Flags::default(),
                },
                RuleId::DamageShieldOther => Event::Damage {
                    source: self.actor(g("src").unwrap_or_default()),
                    target: self.actor(g("tgt").unwrap_or_default()),
                    amount: num("amt"),
                    kind: DamageKind::DamageShield,
                    ability: g("medium").map(str::to_string),
                    damage_type: Some("non-melee".into()),
                    flags: Flags::default(),
                },
                RuleId::NonMeleeGeneric => Event::Damage {
                    source: Actor::You,
                    target: self.actor(g("tgt").unwrap_or_default()),
                    amount: num("amt"),
                    kind: DamageKind::DirectSpell,
                    ability: None,
                    damage_type: Some("non-melee".into()),
                    flags: Flags::default(),
                },
                RuleId::MeleeYou => Event::Damage {
                    source: Actor::You,
                    target: self.actor(g("tgt").unwrap_or_default()),
                    amount: num("amt"),
                    kind: DamageKind::Melee,
                    ability: g("verb").map(str::to_string),
                    damage_type: None,
                    flags: Flags::parse(g("flags")),
                },
                RuleId::MeleeOther => Event::Damage {
                    source: self.actor(g("src").unwrap_or_default()),
                    target: self.actor(g("tgt").unwrap_or_default()),
                    amount: num("amt"),
                    kind: DamageKind::Melee,
                    ability: g("verb").map(str::to_string),
                    damage_type: None,
                    flags: Flags::parse(g("flags")),
                },
                RuleId::MissYou => Event::Miss {
                    source: Actor::You,
                    target: self.actor(g("tgt").unwrap_or_default()),
                    ability: g("verb").map(str::to_string),
                    flags: Flags::parse(g("flags")),
                },
                RuleId::MissOther => Event::Miss {
                    source: self.actor(g("src").unwrap_or_default()),
                    target: self.actor(g("tgt").unwrap_or_default()),
                    ability: g("verb").map(str::to_string),
                    flags: Flags::parse(g("flags")),
                },
                RuleId::AbsorbBlow | RuleId::AbsorbThorns => Event::Absorbed {
                    source: self.actor(g("src").unwrap_or_default()),
                    target: self.actor(g("tgt").unwrap_or_default()),
                },
                RuleId::HealSelf => {
                    let src = self.actor(g("src").unwrap_or_default());
                    Event::Heal {
                        target: src.clone(),
                        source: src,
                        amount: num("amt"),
                        spell: g("spell").map(str::to_string),
                        over_time: g("hot").is_some(),
                    }
                }
                RuleId::HealOther => Event::Heal {
                    source: self.actor(g("src").unwrap_or_default()),
                    target: self.actor(g("tgt").unwrap_or_default()),
                    amount: num("amt"),
                    spell: g("spell").map(str::to_string),
                    over_time: g("hot").is_some(),
                },
                RuleId::SlainByYou => Event::Death {
                    victim: self.actor(g("victim").unwrap_or_default()),
                    killer: Some(Actor::You),
                },
                RuleId::SlainByOther => Event::Death {
                    victim: self.actor(g("victim").unwrap_or_default()),
                    killer: Some(self.actor(g("killer").unwrap_or_default())),
                },
                RuleId::YouWereSlain => Event::Death {
                    victim: Actor::You,
                    killer: Some(self.actor(g("killer").unwrap_or_default())),
                },
                RuleId::LootMarked | RuleId::LootPlain => Event::Loot {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: g("qty")
                        .and_then(|q| q.trim().parse().ok())
                        .unwrap_or(1),
                    source: g("corpse").map(|c| self.actor(c)),
                    disposition: LootDisposition::Kept,
                },
                RuleId::LootSold => Event::Loot {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: g("qty")
                        .and_then(|q| q.trim().parse().ok())
                        .unwrap_or(1),
                    source: g("corpse").map(|c| self.actor(c)),
                    disposition: LootDisposition::Sold(Coin::parse(g("coin").unwrap_or_default())),
                },
                RuleId::LootStored => Event::Loot {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: g("qty")
                        .and_then(|q| q.trim().parse().ok())
                        .unwrap_or(1),
                    source: g("corpse").map(|c| self.actor(c)),
                    disposition: LootDisposition::Stored {
                        destination: g("dest").unwrap_or_default().to_string(),
                    },
                },
                RuleId::LootTransmute => Event::Loot {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: g("qty")
                        .and_then(|q| q.trim().parse().ok())
                        .unwrap_or(1),
                    source: g("corpse").map(|c| self.actor(c)),
                    disposition: LootDisposition::Transmuted {
                        into: g("into").unwrap_or_default().to_string(),
                    },
                },
                RuleId::CoinFromCorpse => Event::CoinLoot {
                    coin: Coin::parse(g("coin").unwrap_or_default()),
                },
                RuleId::MerchantSale => Event::MerchantSale {
                    coin: Coin::parse(g("coin").unwrap_or_default()),
                    item: g("item").unwrap_or_default().to_string(),
                },
                RuleId::ZoneEntered => Event::ZoneChange {
                    zone: g("zone").unwrap_or_default().to_string(),
                },
                RuleId::CastStartYou => Event::CastStart {
                    source: Actor::You,
                    spell: g("spell").unwrap_or_default().to_string(),
                    singing: false,
                },
                RuleId::SingStartYou => Event::CastStart {
                    source: Actor::You,
                    spell: g("spell").unwrap_or_default().to_string(),
                    singing: true,
                },
                RuleId::CastStartOther => Event::CastStart {
                    source: self.actor(g("src").unwrap_or_default()),
                    spell: g("spell").unwrap_or_default().to_string(),
                    singing: body.contains(" begins singing "),
                },
                RuleId::MelodyInterrupted | RuleId::SpellInterrupted => {
                    Event::CastInterrupted { source: Actor::You }
                }
                RuleId::Experience => Event::Experience {
                    percent: g("pct").and_then(|s| s.parse().ok()).unwrap_or(0.0),
                },
                RuleId::FactionAdjusted => Event::Faction {
                    faction: g("faction").unwrap_or_default().to_string(),
                    adjustment: g("adj").and_then(|s| s.parse().ok()),
                },
                RuleId::FactionCapped => Event::Faction {
                    faction: g("faction").unwrap_or_default().to_string(),
                    adjustment: None,
                },
                RuleId::ChatChannel => Event::Chat {
                    speaker: self.actor(g("src").unwrap_or_default()),
                    channel: g("chan").unwrap_or_default().to_string(),
                    message: g("msg").unwrap_or_default().to_string(),
                },
                RuleId::ChatTellYou => Event::Chat {
                    speaker: self.actor(g("src").unwrap_or_default()),
                    channel: "tell".to_string(),
                    message: g("msg").unwrap_or_default().to_string(),
                },
                RuleId::ChatSays => Event::Chat {
                    speaker: self.actor(g("src").unwrap_or_default()),
                    channel: g("verb").unwrap_or("says").to_string(),
                    message: g("msg").unwrap_or_default().to_string(),
                },
                RuleId::GroupJoined => Event::GroupChange {
                    who: self.actor(g("who").unwrap_or_default()),
                    joined: true,
                },
                RuleId::GroupLeft => Event::GroupChange {
                    who: self.actor(g("who").unwrap_or_default()),
                    joined: false,
                },
                RuleId::LevelUp => Event::LevelUp {
                    level: num("level") as u32,
                },
                RuleId::AbilityPointsGained => Event::AbilityPointsGained {
                    // The "an ability point" wording carries no number.
                    points: if g("points").is_some() { num("points") as u32 } else { 1 },
                    unspent: num("unspent") as u32,
                },
                RuleId::AbilityFirstRank => Event::AbilityPointsSpent {
                    ability: g("ability").unwrap_or_default().to_string(),
                    rank: 1,
                    cost: num("cost") as u32,
                },
                RuleId::AbilityPointsSpent => Event::AbilityPointsSpent {
                    ability: g("ability").unwrap_or_default().to_string(),
                    rank: num("rank") as u32,
                    cost: num("cost") as u32,
                },
                RuleId::SpellSetLoaded => Event::SpellSetLoaded {
                    name: g("name").unwrap_or_default().to_string(),
                },
                RuleId::AbilityListEntry => Event::AbilityListEntry {
                    id: num("id") as u32,
                    ability: g("ability").unwrap_or_default().to_string(),
                },
                RuleId::AbilityListDescription => Event::AbilityListDescription {
                    text: g("text").unwrap_or_default().to_string(),
                },
                RuleId::AbilityListCost => Event::AbilityListCost {
                    cost: num("cost") as u32,
                },
                RuleId::Avoided => Event::Avoided {
                    source: self.actor(g("src").unwrap_or_default()),
                    target: self.actor(g("tgt").unwrap_or_default()),
                    how: normalise_avoidance(g("how").unwrap_or_default()),
                },
                RuleId::ResistedYours => Event::Resisted {
                    target: self.actor(g("tgt").unwrap_or_default()),
                    spell: g("spell").unwrap_or_default().to_string(),
                },
                RuleId::ResistedGeneric => Event::Resisted {
                    target: Actor::Named("your target".into()),
                    spell: g("spell").unwrap_or_default().to_string(),
                },
                RuleId::SkillUp => Event::SkillUp {
                    skill: g("skill").unwrap_or_default().to_string(),
                    value: num("value") as u32,
                },
                RuleId::TaskAssigned => Event::TaskAssigned {
                    task: g("task").unwrap_or_default().to_string(),
                },
                RuleId::TaskUpdated => Event::TaskUpdated {
                    task: g("task").unwrap_or_default().to_string(),
                },
                RuleId::AchievementEarned => Event::AchievementEarned {
                    name: g("name").unwrap_or_default().trim().to_string(),
                },
                RuleId::CraftSuccess => Event::Craft {
                    item: g("item").unwrap_or_default().to_string(),
                    success: true,
                },
                RuleId::CraftFailure => Event::Craft {
                    item: g("item").unwrap_or_default().to_string(),
                    success: false,
                },
                RuleId::ComponentConsumed => Event::ComponentConsumed {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: num("qty"),
                    remaining: num("left"),
                },
                RuleId::RecipeLearned => Event::RecipeLearned {
                    recipe: g("recipe").unwrap_or_default().to_string(),
                },
                RuleId::SongFizzle => Event::CastInterrupted { source: Actor::You },
                RuleId::SpellMemorized => Event::SpellbookChange {
                    spell: g("spell").unwrap_or_default().to_string(),
                    change: SpellbookChangeKind::Memorized,
                },
                RuleId::SpellForgotten => Event::SpellbookChange {
                    spell: g("spell").unwrap_or_default().to_string(),
                    change: SpellbookChangeKind::Forgotten,
                },
                RuleId::SpellGranted => Event::SpellbookChange {
                    spell: g("spell").unwrap_or_default().to_string(),
                    change: SpellbookChangeKind::Granted,
                },
                RuleId::PetAttack => Event::PetAttack {
                    pet: self.actor(g("pet").unwrap_or_default()),
                    target: self.actor(g("target").unwrap_or_default()),
                },
                RuleId::SkillTrained => Event::SkillTrained {
                    trainer: g("trainer").unwrap_or_default().to_string(),
                    skill: g("skill").unwrap_or_default().to_string(),
                },
                RuleId::CharmImmune => Event::CharmImmune,
                RuleId::TargetUnaffected => Event::TargetUnaffected,
                RuleId::EffectLandedOnTarget => Event::EffectLandedOnTarget {
                    target: self.actor(g("target").unwrap_or_default()),
                    text: format!("{}.", g("text").unwrap_or_default()),
                },
                RuleId::EffectWoreOffTarget => Event::EffectWoreOffTarget {
                    spell: g("spell").unwrap_or_default().to_string(),
                    target: self.actor(g("target").unwrap_or_default()),
                },
                RuleId::AbilityCooldown => Event::AbilityCooldown {
                    ability: g("ability").unwrap_or_default().to_string(),
                    remaining: num("h") * 3600 + num("m") * 60 + num("s"),
                },
                RuleId::ItemProc => Event::ItemProc {
                    item: g("item").unwrap_or_default().to_string(),
                    effect: g("effect").unwrap_or_default().to_string(),
                },
                RuleId::EmoteWince => Event::Emote {
                    actor: self.actor(g("actor").unwrap_or_default()),
                    text: "winces".to_string(),
                },
                RuleId::EmoteGeneric => Event::Emote {
                    actor: self.actor(g("actor").unwrap_or_default()),
                    text: g("text").unwrap_or_default().to_string(),
                },
                RuleId::SystemAutoAttack | RuleId::SystemMessage => Event::System {
                    text: body.to_string(),
                },
            };
            return ev;
        }
        Event::Unknown
    }
}

/// Split `[Tue Jul 28 14:54:10 2026] message` into a unix timestamp and the
/// message body.
///
/// EverQuest timestamps carry no timezone. Rather than guess one, we interpret
/// them as UTC. Every downstream use is a difference or an ordering, both of
/// which are unaffected, and it keeps ingestion immune to DST discontinuities.
pub fn split_timestamp(line: &str) -> Option<(i64, &str)> {
    let rest = line.strip_prefix('[')?;
    let close = rest.find(']')?;
    let (ts, after) = rest.split_at(close);
    let body = after[1..].trim_start();
    Some((parse_eq_timestamp(ts)?, body))
}

/// Parse the fixed `Ddd Mmm DD HH:MM:SS YYYY` timestamp form.
fn parse_eq_timestamp(ts: &str) -> Option<i64> {
    // "Tue Jul 28 14:54:10 2026" — day-of-month may be space padded.
    let mut it = ts.split_whitespace();
    let _weekday = it.next()?;
    let month = month_from_abbrev(it.next()?)?;
    let day: i64 = it.next()?.parse().ok()?;
    let time = it.next()?;
    let year: i64 = it.next()?.parse().ok()?;

    let mut tp = time.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let m: i64 = tp.next()?.parse().ok()?;
    let s: i64 = tp.next()?.parse().ok()?;

    Some(days_from_civil(year, month, day) * 86_400 + h * 3_600 + m * 60 + s)
}

fn month_from_abbrev(m: &str) -> Option<i64> {
    Some(match m {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01 for a proleptic
/// Gregorian date. Cheap and branch-light, which matters at ~600k lines.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Parser {
        Parser::new("Tharn")
    }

    #[test]
    fn timestamp_splits_and_converts() {
        let (ts, body) = split_timestamp("[Tue Jul 28 14:54:10 2026] Auto attack is on.").unwrap();
        assert_eq!(body, "Auto attack is on.");
        // 2026-07-28T14:54:10Z
        assert_eq!(ts, 1_785_250_450);
    }

    #[test]
    fn days_from_civil_matches_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn player_inferred_from_filename() {
        assert_eq!(
            Parser::player_from_filename("/logs/eqlog_Tharn_freeport.txt").as_deref(),
            Some("Tharn")
        );
        assert_eq!(Parser::player_from_filename("notalog.txt"), None);
    }

    #[test]
    fn melee_by_you_with_crit() {
        let e = p().parse_body("You slash Lord Nagafen for 160 points of damage. (Critical)");
        match e {
            Event::Damage {
                source,
                target,
                amount,
                kind,
                ability,
                flags,
                ..
            } => {
                assert_eq!(source, Actor::You);
                assert_eq!(target, Actor::Named("Lord Nagafen".into()));
                assert_eq!(amount, 160);
                assert_eq!(kind, DamageKind::Melee);
                assert_eq!(ability.as_deref(), Some("slash"));
                assert!(flags.is_crit());
            }
            other => panic!("expected melee damage, got {other:?}"),
        }
    }

    #[test]
    fn melee_singular_point_is_handled() {
        let e = p().parse_body("Jaranab pierces a lesser mummy for 1 point of damage.");
        assert!(matches!(e, Event::Damage { amount: 1, kind: DamageKind::Melee, .. }));
    }

    #[test]
    fn melee_against_you_with_riposte() {
        let e = p().parse_body("A giant fire beetle bites YOU for 2 points of damage. (Riposte)");
        match e {
            Event::Damage {
                source, target, flags, ..
            } => {
                assert_eq!(source, Actor::Named("A giant fire beetle".into()));
                assert_eq!(target, Actor::You);
                assert!(flags.has("Riposte"));
                assert!(!flags.is_crit());
            }
            other => panic!("expected melee damage, got {other:?}"),
        }
    }

    #[test]
    fn spell_damage_beats_melee_rule() {
        let e = p().parse_body("You hit a lesser mummy for 4 points of magic damage by Shallow Breath.");
        match e {
            Event::Damage {
                kind,
                ability,
                damage_type,
                amount,
                ..
            } => {
                assert_eq!(kind, DamageKind::DirectSpell);
                assert_eq!(ability.as_deref(), Some("Shallow Breath"));
                assert_eq!(damage_type.as_deref(), Some("magic"));
                assert_eq!(amount, 4);
            }
            other => panic!("expected spell damage, got {other:?}"),
        }
    }

    /// Damage-over-time and direct spells carry the same `(Critical)` suffix
    /// as melee. Anchoring those rules on `\.$` made such lines fail to match
    /// outright, so the damage was dropped, not merely unflagged — 3,062 lines
    /// in a real log.
    #[test]
    fn spells_and_dots_can_crit() {
        match p().parse_body(
            "An imp protector has taken 98 damage from your Denon's Disruptive Discord III. (Critical)",
        ) {
            Event::Damage {
                amount, kind, ability, flags, ..
            } => {
                assert_eq!(amount, 98);
                assert_eq!(kind, DamageKind::DamageOverTime);
                assert_eq!(ability.as_deref(), Some("Denon's Disruptive Discord III"));
                assert!(flags.is_crit(), "the crit must be recorded");
            }
            other => panic!("expected a critical dot tick, got {other:?}"),
        }

        match p().parse_body("You hit a mummy for 40 points of magic damage by Shallow Breath. (Critical)") {
            Event::Damage { amount, kind, flags, .. } => {
                assert_eq!(amount, 40);
                assert_eq!(kind, DamageKind::DirectSpell);
                assert!(flags.is_crit());
            }
            other => panic!("expected a critical spell hit, got {other:?}"),
        }

        // A dot cast by someone else, also critting.
        match p().parse_body("A rat has taken 8 damage from Clinging Darkness by Sticky. (Critical)") {
            Event::Damage { source, flags, .. } => {
                assert_eq!(source, Actor::Named("Sticky".into()));
                assert!(flags.is_crit());
            }
            other => panic!("expected a dot tick, got {other:?}"),
        }
    }

    #[test]
    fn dot_tick_yours_attributes_to_you() {
        let e = p().parse_body("An imp protector has taken 37 damage from your Chords of Dissonance III.");
        match e {
            Event::Damage {
                source, kind, ability, ..
            } => {
                assert_eq!(source, Actor::You);
                assert_eq!(kind, DamageKind::DamageOverTime);
                assert_eq!(ability.as_deref(), Some("Chords of Dissonance III"));
            }
            other => panic!("expected dot tick, got {other:?}"),
        }
    }

    #[test]
    fn dot_tick_by_other_caster() {
        let e = p().parse_body("A young kodiak has taken 8 damage from Clinging Darkness by Sticky.");
        match e {
            Event::Damage {
                source, target, ability, ..
            } => {
                assert_eq!(source, Actor::Named("Sticky".into()));
                assert_eq!(target, Actor::Named("A young kodiak".into()));
                assert_eq!(ability.as_deref(), Some("Clinging Darkness"));
            }
            other => panic!("expected dot tick, got {other:?}"),
        }
    }

    #[test]
    fn dot_tick_on_you_uses_have_taken() {
        let e = p().parse_body("You have taken 1 damage from Rabies by a lesser mummy.");
        match e {
            Event::Damage { source, target, .. } => {
                assert_eq!(target, Actor::You);
                assert_eq!(source, Actor::Named("a lesser mummy".into()));
            }
            other => panic!("expected dot tick, got {other:?}"),
        }
    }

    #[test]
    fn damage_shield_yours() {
        let e = p().parse_body(
            "A wan ghoul knight is pierced by YOUR thorns for 27 points of non-melee damage.",
        );
        match e {
            Event::Damage {
                source, kind, amount, ..
            } => {
                assert_eq!(source, Actor::You);
                assert_eq!(kind, DamageKind::DamageShield);
                assert_eq!(amount, 27);
            }
            other => panic!("expected damage shield, got {other:?}"),
        }
    }

    #[test]
    fn damage_shield_against_you() {
        let e = p().parse_body(
            "YOU are burned by a lava guardian's flames for 48 points of non-melee damage!",
        );
        match e {
            Event::Damage {
                source, target, kind, ..
            } => {
                assert_eq!(target, Actor::You);
                assert_eq!(source, Actor::Named("a lava guardian".into()));
                assert_eq!(kind, DamageKind::DamageShield);
            }
            other => panic!("expected damage shield, got {other:?}"),
        }
    }

    #[test]
    fn miss_forms() {
        assert!(matches!(
            p().parse_body("You try to slash an imp protector, but miss!"),
            Event::Miss { source: Actor::You, .. }
        ));
        match p().parse_body("A giant fire beetle tries to bite Jabarab, but misses! (Riposte)") {
            Event::Miss { flags, target, .. } => {
                assert!(flags.has("Riposte"));
                assert_eq!(target, Actor::Named("Jabarab".into()));
            }
            other => panic!("expected miss, got {other:?}"),
        }
    }

    #[test]
    fn heal_reflexive_targets_the_healer() {
        let e = p().parse_body("Persephone healed herself for 4 hit points by Blessing of the Knight.");
        match e {
            Event::Heal {
                source,
                target,
                amount,
                spell,
                over_time,
            } => {
                assert_eq!(source, Actor::Named("Persephone".into()));
                assert_eq!(target, Actor::Named("Persephone".into()));
                assert_eq!(amount, 4);
                assert_eq!(spell.as_deref(), Some("Blessing of the Knight"));
                assert!(!over_time);
            }
            other => panic!("expected heal, got {other:?}"),
        }
    }

    #[test]
    fn heal_over_time_flagged() {
        let e = p().parse_body("You healed Tharn over time for 153 hit points by Efflorescing Heal.");
        match e {
            Event::Heal {
                over_time, amount, spell, ..
            } => {
                assert!(over_time);
                assert_eq!(amount, 153);
                assert_eq!(spell.as_deref(), Some("Efflorescing Heal"));
            }
            other => panic!("expected heal, got {other:?}"),
        }
    }

    #[test]
    fn heal_without_spell_name() {
        let e = p().parse_body("You healed Tharn for 28 hit points.");
        match e {
            Event::Heal { spell, amount, .. } => {
                assert_eq!(spell, None);
                assert_eq!(amount, 28);
            }
            other => panic!("expected heal, got {other:?}"),
        }
    }

    #[test]
    fn deaths_all_three_forms() {
        assert!(matches!(
            p().parse_body("You have slain a decaying skeleton!"),
            Event::Death { killer: Some(Actor::You), .. }
        ));
        match p().parse_body("Orc pawn has been slain by Reklar!") {
            Event::Death { victim, killer } => {
                assert_eq!(victim, Actor::Named("Orc pawn".into()));
                assert_eq!(killer, Some(Actor::Named("Reklar".into())));
            }
            other => panic!("expected death, got {other:?}"),
        }
        match p().parse_body("You have been slain by a lesser mummy!") {
            Event::Death { victim, killer } => {
                assert_eq!(victim, Actor::You);
                assert_eq!(killer, Some(Actor::Named("a lesser mummy".into())));
            }
            other => panic!("expected death, got {other:?}"),
        }
    }

    #[test]
    fn loot_sold_captures_proceeds() {
        let e = p().parse_body(
            "You looted a Fire Beetle Eye from a fire beetle's corpse and sold it for 2 silver.",
        );
        match e {
            Event::Loot {
                item,
                quantity,
                source,
                disposition,
            } => {
                assert_eq!(item, "Fire Beetle Eye");
                assert_eq!(quantity, 1);
                assert_eq!(source, Some(Actor::Named("a fire beetle".into())));
                assert_eq!(disposition, LootDisposition::Sold(Coin::parse("2 silver")));
            }
            other => panic!("expected loot, got {other:?}"),
        }
    }

    #[test]
    fn loot_depot_with_quantity() {
        let e = p().parse_body(
            "You looted 2 Spider Silk from a large spider's corpse and stored it in your tradeskill depot",
        );
        match e {
            Event::Loot {
                item,
                quantity,
                disposition,
                ..
            } => {
                assert_eq!(item, "Spider Silk");
                assert_eq!(quantity, 2);
                assert_eq!(
                    disposition,
                    LootDisposition::Stored {
                        destination: "tradeskill depot".into()
                    }
                );
            }
            other => panic!("expected loot, got {other:?}"),
        }
    }

    /// Regression: an unhandled "stored it in …" destination used to fall
    /// through to the plain rule, which swallowed the whole trailing clause
    /// into the corpse name — so drop sources read
    /// "a wan ghoul knight's corpse and stored it in your currency".
    #[test]
    fn stored_destinations_do_not_leak_into_the_corpse_name() {
        for (line, item, corpse, dest) in [
            (
                "You looted a Mote of Potential from a wan ghoul knight's corpse and stored it in your currency",
                "Mote of Potential",
                "a wan ghoul knight",
                "currency",
            ),
            (
                "You looted a Bracelet of Exertion +2 from Protector of Sky's corpse and stored it in your Dragon Hoard",
                "Bracelet of Exertion +2",
                "Protector of Sky",
                "Dragon Hoard",
            ),
        ] {
            match p().parse_body(line) {
                Event::Loot {
                    item: got_item,
                    source,
                    disposition,
                    ..
                } => {
                    assert_eq!(got_item, item);
                    assert_eq!(
                        source,
                        Some(Actor::Named(corpse.into())),
                        "corpse name must not absorb the trailing clause"
                    );
                    assert_eq!(
                        disposition,
                        LootDisposition::Stored {
                            destination: dest.into()
                        }
                    );
                }
                other => panic!("expected loot for {line:?}, got {other:?}"),
            }
        }
    }

    /// The client reports zero-value auto-sales as "for free".
    #[test]
    fn sold_for_free_parses_as_zero_coin() {
        match p().parse_body("You looted a Rusty Dagger from a rat's corpse and sold it for free.") {
            Event::Loot { disposition, source, .. } => {
                assert_eq!(disposition, LootDisposition::Sold(Coin::default()));
                assert_eq!(source, Some(Actor::Named("a rat".into())));
            }
            other => panic!("expected loot, got {other:?}"),
        }
    }

    #[test]
    fn loot_marked_form() {
        let e = p().parse_body(
            "--You have looted a Selo`s Drums of the March +3 from Lord Nagafen's corpse.--",
        );
        match e {
            Event::Loot { item, source, .. } => {
                assert_eq!(item, "Selo`s Drums of the March +3");
                assert_eq!(source, Some(Actor::Named("Lord Nagafen".into())));
            }
            other => panic!("expected loot, got {other:?}"),
        }
    }

    #[test]
    fn loot_transmute_form() {
        let e = p().parse_body(
            "You looted a Cloak of Flames +3 from Lord Nagafen's corpse to create a Cloak of Flames +3",
        );
        match e {
            Event::Loot { disposition, .. } => {
                assert_eq!(
                    disposition,
                    LootDisposition::Transmuted {
                        into: "Cloak of Flames +3".into()
                    }
                );
            }
            other => panic!("expected loot, got {other:?}"),
        }
    }

    #[test]
    fn coin_from_corpse() {
        let e = p().parse_body("You receive 12 platinum, 3 silver and 4 copper from the corpse.");
        match e {
            Event::CoinLoot { coin } => assert_eq!(coin.as_copper(), 12_034),
            other => panic!("expected coin, got {other:?}"),
        }
    }

    #[test]
    fn zone_and_cast_and_experience() {
        assert!(matches!(
            p().parse_body("You have entered West Commonlands."),
            Event::ZoneChange { .. }
        ));
        match p().parse_body("You begin singing Tuyen's Chant of Frost IV.") {
            Event::CastStart { singing, spell, .. } => {
                assert!(singing);
                assert_eq!(spell, "Tuyen's Chant of Frost IV");
            }
            other => panic!("expected cast, got {other:?}"),
        }
        match p().parse_body("You gain experience! (1.585%)") {
            Event::Experience { percent } => assert!((percent - 1.585).abs() < 1e-9),
            other => panic!("expected experience, got {other:?}"),
        }
    }

    #[test]
    fn chat_channel_form() {
        match p().parse_body("Seizure tells NewPlayers:1, 'none'") {
            Event::Chat {
                speaker,
                channel,
                message,
            } => {
                assert_eq!(speaker, Actor::Named("Seizure".into()));
                assert_eq!(channel, "NewPlayers:1");
                assert_eq!(message, "none");
            }
            other => panic!("expected chat, got {other:?}"),
        }
    }

    /// Cosmetic and system lines must never be scored as combat. They may be
    /// classified as Emote/System/Faction, or left Unknown for the spell
    /// database to resolve, but must not become damage, misses or heals.
    #[test]
    fn flavour_text_never_becomes_combat() {
        for line in [
            "You feel replenished.",
            "Your wounds begin to heal.",
            "an imp protector winces.",
            "Auto attack is on.",
            "Your faction standing with Nagafen could not possibly get any worse.",
            "You whistle an ancient warsong.",
            "The jig sends energy zinging through your body.",
            "Your voice booms.",
        ] {
            let e = p().parse_body(line);
            assert!(
                !matches!(
                    e,
                    Event::Damage { .. } | Event::Miss { .. } | Event::Heal { .. } | Event::Death { .. }
                ),
                "line {line:?} must not be scored as combat, got {e:?}"
            );
        }
    }

    /// Effect flavour text the spell database will resolve must be left as
    /// Unknown by the parser, not swallowed by a broad cosmetic rule.
    #[test]
    fn spell_flavour_text_left_for_the_spell_database() {
        for line in [
            "You feel replenished.",
            "Your feet move faster.",
            "Your wounds begin to heal.",
        ] {
            assert!(
                matches!(p().parse_body(line), Event::Unknown),
                "line {line:?} should be left Unknown for spell resolution"
            );
        }
    }

    #[test]
    fn ability_points_gained_both_wordings() {
        match p().parse_body("You have gained 2 ability point(s)!  You now have 5 ability point(s).") {
            Event::AbilityPointsGained { points, unspent } => {
                assert_eq!(points, 2);
                assert_eq!(unspent, 5);
            }
            other => panic!("expected AA gain, got {other:?}"),
        }
        // The singular form carries no number.
        match p().parse_body("You have gained an ability point!  You now have 6 ability points.") {
            Event::AbilityPointsGained { points, unspent } => {
                assert_eq!(points, 1);
                assert_eq!(unspent, 6);
            }
            other => panic!("expected AA gain, got {other:?}"),
        }
    }

    /// Rank 1 and later ranks use different wordings. Handling only the
    /// "improved" form hid every first-rank purchase — 32 of 67 in a real log.
    #[test]
    fn ability_purchases_cover_first_and_later_ranks() {
        match p().parse_body(
            "You have gained the ability \"Symphonic Aura: Disabled\" at a cost of 3 ability points.",
        ) {
            Event::AbilityPointsSpent { ability, rank, cost } => {
                assert_eq!(ability, "Symphonic Aura: Disabled");
                assert_eq!(rank, 1, "the quoted form is always rank 1");
                assert_eq!(cost, 3);
            }
            other => panic!("expected AA purchase, got {other:?}"),
        }
        match p().parse_body("You have improved Symphonic Aura: Enabled 2 at a cost of 0 ability points.") {
            Event::AbilityPointsSpent { ability, rank, cost } => {
                assert_eq!(ability, "Symphonic Aura: Enabled");
                assert_eq!(rank, 2, "trailing digits are the rank, not part of the name");
                assert_eq!(cost, 0);
            }
            other => panic!("expected AA purchase, got {other:?}"),
        }
    }

    /// The two "You have gained ..." forms must not be confused with each other.
    #[test]
    fn ability_gain_and_purchase_are_distinguished() {
        assert!(matches!(
            p().parse_body("You have gained 2 ability point(s)!  You now have 5 ability point(s)."),
            Event::AbilityPointsGained { .. }
        ));
        assert!(matches!(
            p().parse_body("You have gained the ability \"Banestrike\" at a cost of 0 ability points."),
            Event::AbilityPointsSpent { .. }
        ));
    }

    #[test]
    fn level_up_is_captured() {
        match p().parse_body("You have gained a level! Welcome to level 42!") {
            Event::LevelUp { level } => assert_eq!(level, 42),
            other => panic!("expected level up, got {other:?}"),
        }
    }

    #[test]
    fn ability_cooldown_converts_to_seconds() {
        match p().parse_body("You can use the ability Rampage again in 8 minute(s) 54 seconds.") {
            Event::AbilityCooldown { ability, remaining } => {
                assert_eq!(ability, "Rampage");
                assert_eq!(remaining, 8 * 60 + 54);
            }
            other => panic!("expected cooldown, got {other:?}"),
        }
    }

    #[test]
    fn item_proc_captures_item_and_effect() {
        match p().parse_body("Your Djarn's Amethyst Ring (Exaltation) shimmers briefly.") {
            Event::ItemProc { item, effect } => {
                assert_eq!(item, "Djarn's Amethyst Ring");
                assert_eq!(effect, "Exaltation");
            }
            other => panic!("expected item proc, got {other:?}"),
        }
    }

    /// The generic third-person melee rule is the most dangerous pattern in the
    /// table; verify a non-combat sentence of the same shape is rejected.
    #[test]
    fn non_combat_verb_rejected_by_melee_rule() {
        let e = p().parse_body("Someone waits patiently for 5 points of damage.");
        assert!(matches!(e, Event::Unknown), "got {e:?}");
    }
}
