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
    DotTickUnattributed,
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
    LootLeft,
    LootSold,
    LootStored,
    LootTransmute,
    LootPlain,
    CoinFromCorpse,
    CoinSplit,
    MerchantSale,
    ZoneRuleNotice,
    ZoneEntered,
    CastStartYou,
    SingStartYou,
    CastStartOther,
    MelodyInterrupted,
    SpellInterrupted,
    SpellInterruptedNamed,
    SpellInterruptedOther,
    CastResumedOther,
    Experience,
    FactionAdjusted,
    FactionCapped,
    ChatChannel,
    ChatTellYou,
    ChatTellByYou,
    ChatSays,
    GroupJoined,
    GroupLeft,
    LevelUp,
    WhoEntry,
    WhoAnonymous,
    AbilityPointsGained,
    AbilityFirstRank,
    AbilityPointsSpent,
    SpellSetLoaded,
    AbilityListEntry,
    AbilityListDescription,
    AbilityListCost,
    Avoided,
    AvoidedByYou,
    ResistedYours,
    ResistedGeneric,
    ResistedByYou,
    SkillUp,
    Stunned,
    StunEnded,
    StunAvoided,
    TaskAssigned,
    ItemGranted,
    TaskUpdated,
    AchievementEarned,
    CraftSuccess,
    CraftFailure,
    ForageSuccess,
    ForageFailure,
    ComponentConsumed,
    ItemPurchased,
    MerchantQuotePer,
    MerchantQuoteOne,
    RecipeLearned,
    SongFizzle,
    SpellFizzle,
    SpellMemorized,
    StanceChanged,
    SpellForgotten,
    SpellGranted,
    PetAttack,
    PetLeader,
    EffectWoreOffTarget,
    PetEffectWoreOff,
    EffectLandedOnTarget,
    LoggedOn,
    TradeOffer,
    TradeComplete,
    SkillTrained,
    CharmImmune,
    TargetUnaffected,
    AbilityCooldown,
    ItemProc,
    EmoteWince,
    EmoteGeneric,
    Consider,
    ConsiderArmy,
    Targeted,
    ServerVictory,
    Location,
    SystemAutoAttack,
    SystemMessage,
}

/// How a creature regards you, as the client words it.
///
/// Enumerated rather than matched loosely, because the attitude is what
/// separates the creature's name from the rest of the line and there is
/// nothing else to anchor on: "A woodland lion scowls at you" has no
/// punctuation between the name and the verb, so a lazy pattern would call
/// the creature "A".
///
/// Complete against 454 consider lines in a 151 MB log — every one matches
/// one of these eight. A ninth added by a patch does not corrupt anything: the
/// line stays [`Event::Unknown`], which is exactly what `eqldb-logscan
/// --census` exists to surface, rather than being parsed into a creature
/// called "A".
const ATTITUDES: &str = "scowls at you, ready to attack\
    |regards you indifferently\
    |glowers at you dubiously\
    |judges you amiably\
    |regards you as an ally\
    |looks upon you warmly\
    |glares at you threateningly\
    |looks your way apprehensively";

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
    // "A glare lord has taken 100 damage by Rotting Flesh."
    //
    // The same tick with no caster named. Note that `by` introduces the *spell*
    // here, where in the line above it introduces the caster — the client uses
    // the word for both and only the presence of `from` tells them apart, which
    // is why this rule has to sit after that one.
    //
    // 1,578 lines across the two logs on hand, every one of them previously
    // dropped, so the damage appeared in no total anywhere.
    (
        RuleId::DotTickUnattributed,
        r"^(?P<tgt>.+?) ha[sv]e? taken (?P<amt>\d+) damage by (?P<spell>.+?)\.(?: \((?P<flags>[^)]*)\))?$",
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
    // "You were hit by non-melee for 118 damage."
    //
    // Both persons and both phrasings: the second person drops "points of"
    // entirely, and requiring it dropped every one of these on the floor — 229
    // hits and 4,421 damage across the logs on hand, incoming damage that
    // appeared in no total anywhere.
    (
        RuleId::NonMeleeGeneric,
        r"^(?P<tgt>.+?) (?:was|were) hit by non-melee for (?P<amt>\d+)(?: points? of)? damage\.$",
    ),
    // ---- Melee --------------------------------------------------------------
    // Second person uses the bare verb; third person uses the inflected form.
    // The verb alternation is substituted in at compile time (see `{VERB}`).
    // It must live *inside* the pattern rather than being checked afterwards:
    // with a bare `[a-z]+` the engine commits to the leftmost lazy split, so
    // "A giant fire beetle bites YOU" would yield verb="giant" and a later
    // rejection could not make it backtrack to the correct split.
    // One verb takes a preposition: you *frenzy on* something, where you
    // *slash* it. Without the optional `on` the lazy target capture keeps the
    // word — "Morgok frenzies on a cracked skeleton" files a creature called
    // "on a cracked skeleton", and every mob anyone ever frenzied on gets a
    // shadow twin. It cost 37 invented creatures in one log, including "on
    // YOU", before anybody noticed; the names are only visibly wrong if you
    // happen to read the bestiary.
    //
    // Optional rather than a second rule, and matched inside the pattern
    // rather than trimmed afterwards, for the reason above: the engine has to
    // be able to reach the right split by backtracking. A target genuinely
    // beginning with "on " does not exist, and would have to survive the same
    // ambiguity in the client's own output if it did.
    // "You slash Lord Nagafen for 160 points of damage. (Critical)"
    (
        RuleId::MeleeYou,
        r"^You (?P<verb>{VERB})(?: on)? (?P<tgt>.+?) for (?P<amt>\d+) points? of damage\.(?: \((?P<flags>[^)]*)\))?$",
    ),
    // "An imp protector hits YOU for 33 points of damage. (Riposte)"
    (
        RuleId::MeleeOther,
        r"^(?P<src>.+?) (?P<verb>{VERB})(?: on)? (?P<tgt>.+?) for (?P<amt>\d+) points? of damage\.(?: \((?P<flags>[^)]*)\))?$",
    ),
    // ---- Misses -------------------------------------------------------------
    (
        RuleId::MissYou,
        r"^You try to (?P<verb>[a-z]+)(?: on)? (?P<tgt>.+?), but miss!(?: \((?P<flags>[^)]*)\))?$",
    ),
    (
        RuleId::MissOther,
        r"^(?P<src>.+?) tries to (?P<verb>[a-z]+)(?: on)? (?P<tgt>.+?), but misses!(?: \((?P<flags>[^)]*)\))?$",
    ),
    // ---- Active avoidance ---------------------------------------------------
    // Distinct from a plain miss: the defender did something.
    // "An imp protector tries to hit YOU, but YOU block!"
    (
        RuleId::Avoided,
        r"^(?P<src>.+?) tries to {VERB}(?: on)? (?P<tgt>.+?), but (?P<defender>.+?) (?P<how>blocks?|parr(?:y|ies)|dodges?|ripostes?)!(?: \((?P<flags>[^)]*)\))?$",
    ),
    // The same line with you as the attacker. The client conjugates for the
    // second person — "You try to" against "X tries to" — exactly as it does
    // for a miss, which has had two rules from the start. This one did not,
    // so every swing of yours that was dodged, parried or blocked parsed as
    // Unknown: 21,889 lines in the reference log, and your own avoidance
    // figures had nothing in them.
    (
        RuleId::AvoidedByYou,
        r"^You try to {VERB}(?: on)? (?P<tgt>.+?), but (?P<defender>.+?) (?P<how>blocks?|parr(?:y|ies)|dodges?|ripostes?)!(?: \((?P<flags>[^)]*)\))?$",
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
    // "You resist a goblin soothsayer's Root!"
    //
    // The other direction: something cast at you and failed. The caster is
    // named but [`Event::Resisted`] has nowhere to put it, and for a resist
    // you made the spell is the part that matters.
    (
        RuleId::ResistedByYou,
        r"^You resist (?:a |an |the )?.+?[`\u{2019}']s (?P<spell>.+?)!$",
    ),
    // ---- Absorbs ------------------------------------------------------------
    // "A dar ghoul knight tries to cleave X, but X's magical skin absorbs the blow!"
    (
        RuleId::AbsorbBlow,
        r"^(?P<src>.+?) tries to [a-z]+(?: on)? (?P<tgt>.+?), but .+?[`']s magical skin absorbs the blow!$",
    ),
    // "A fire giant warrior's magical skin absorbs the damage of YOUR thorns."
    (
        RuleId::AbsorbThorns,
        r"^(?P<tgt>.+?)[`']s magical skin absorbs the damage of (?P<src>.+?) thorns\.$",
    ),
    // ---- Heals --------------------------------------------------------------
    // Reflexive form must precede the general one; "himself/herself/itself"
    // would otherwise be captured as a distinct target named "himself".
    //
    // Two optional pieces neither form had, both of which cost whole lines
    // rather than detail — a line that fails to match is not a heal with a
    // missing flag, it is a heal that never happened as far as any total is
    // concerned:
    //
    //   `(?P<full>\d+)` — "healed X for 47 (241) hit points". The second number
    //   is what the heal was worth before the target's missing health capped
    //   it; the first is what actually landed. 9,328 lines across the two logs.
    //
    //   the trailing flag group — "... by Efflorescing Heal. (Critical)".
    //   1,161 lines. Heals crit, and the client says so the same way it does
    //   for damage.
    //
    // "Persephone healed herself for 4 hit points by Blessing of the Knight."
    (
        RuleId::HealSelf,
        r"^(?P<src>.+?) healed (?:himself|herself|itself|yourself)(?P<hot> over time)? for (?P<amt>\d+)(?: \((?P<full>\d+)\))? hit points?(?: by (?P<spell>.+?))?\.(?: \((?P<flags>[^)]*)\))?$",
    ),
    // "You healed Tharn over time for 153 hit points by Efflorescing Heal."
    (
        RuleId::HealOther,
        r"^(?P<src>.+?) healed (?P<tgt>.+?)(?P<hot> over time)? for (?P<amt>\d+)(?: \((?P<full>\d+)\))? hit points?(?: by (?P<spell>.+?))?\.(?: \((?P<flags>[^)]*)\))?$",
    ),
    // ---- Deaths -------------------------------------------------------------
    (RuleId::SlainByYou, r"^You have slain (?P<victim>.+?)!$"),
    // Two wordings for one happening. "has been slain by" is what the client
    // writes when you are grouped with the killer; "was slain by" is the form
    // everything else gets, including your own pet and your own mercenary.
    // Only matching the first credited a pet class with none of its kills.
    (
        RuleId::SlainByOther,
        r"^(?P<victim>.+?) (?:has been|was) slain by (?P<killer>.+?)!$",
    ),
    (
        RuleId::YouWereSlain,
        r"^You have been slain by (?P<killer>.+?)!$",
    ),
    // ---- Loot ---------------------------------------------------------------
    // "--You have looted a Selo`s Drums of the March +3 from Lord Nagafen's corpse.--"
    (
        RuleId::LootMarked,
        r"^--You have looted (?:a |an |the )?(?P<qty>\d+ )?(?P<item>.+?) from (?P<corpse>.+?)\.--$",
    ),
    // "--You left a Rusty Two Handed Sword on a snow orc trooper.--"
    //
    // Deliberately not folded into the looted rules: the item was on the
    // corpse, which is what a drop table wants to know, but it never entered
    // your inventory.
    (
        RuleId::LootLeft,
        r"^--You left (?:a |an |the )?(?P<qty>\d+ )?(?P<item>.+?) on (?P<corpse>.+?)\.--$",
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
    // "You receive 2 gold, 1 silver and 9 copper as your split (with a bonus)."
    //
    // Grouped coin, already divided. It is still coin off a corpse, so it
    // counts the same way — the bonus clause is the server's own topping-up
    // and is part of what arrived.
    (
        RuleId::CoinSplit,
        r"^You receive (?P<coin>.+?) as your split(?: \(with a bonus\))?\.$",
    ),
    // "You receive 1 gold 9 copper from Klok Koglin for the Belt Pouch(s)."
    (
        RuleId::MerchantSale,
        r"^You receive (?P<coin>.+?) from (?P<merchant>.+?) for the (?P<item>.+?)\.$",
    ),
    // ---- World / progression ------------------------------------------------
    // The client reuses this sentence for rule announcements that are not
    // places: "You have entered an area where Bind Affinity is allowed."
    // Taking one as a zone files kills under somewhere that does not exist
    // and splits the real zone's tally in two.
    //
    // Caught by an earlier rule rather than by a negative lookahead, which
    // this regex engine does not have, and rather than by every reader
    // filtering the zone name in turn — which is what they were all doing.
    // Rule order is precedence, so this wins.
    (
        RuleId::ZoneRuleNotice,
        r"^You have entered an? (?:area|zone) [^.]*\.$",
    ),
    (RuleId::ZoneEntered, r"^You have entered (?P<zone>.+?)\.$"),
    (
        RuleId::CastStartYou,
        r"^You begin casting (?P<spell>.+?)\.$",
    ),
    (
        RuleId::SingStartYou,
        r"^You begin singing (?P<spell>.+?)\.$",
    ),
    (
        RuleId::CastStartOther,
        r"^(?P<src>.+?) begins (?:casting|singing) (?P<spell>.+?)\.$",
    ),
    (
        RuleId::MelodyInterrupted,
        r"^Your melody has been interrupted!$",
    ),
    (
        RuleId::SpellInterrupted,
        r"^Your (?:spell is interrupted|casting has been interrupted)\.?!?$",
    ),
    // "Your Touch of Night spell is interrupted." — same happening, but the
    // client names the spell, so the bare rule above never sees it.
    (
        RuleId::SpellInterruptedNamed,
        r"^Your (?P<spell>.+?) spell is interrupted\.$",
    ),
    // "a goblin soothsayer's Root spell is interrupted."
    (
        RuleId::SpellInterruptedOther,
        r"^(?P<src>.+?)[`\u{2019}']s (?P<spell>.+?) spell is interrupted\.$",
    ),
    // "A goblin soothsayer regains concentration and continues casting."
    // Recorded because it undoes the interrupt above: a cast counted as lost
    // and then resumed was never lost.
    (
        RuleId::CastResumedOther,
        r"^(?P<src>.+?) regains concentration and continues casting\.$",
    ),
    // `(with a bonus)` is the server saying a bonus event is running. It sits
    // between the noun and the exclamation mark, so an anchored pattern
    // without it does not match a weaker version of the line -- it does not
    // match at all, and every gain for the length of the event is dropped on
    // the floor. 2,129 of them were, across the two Freeport logs, before this
    // group was added.
    (
        RuleId::Experience,
        r"^You gain (?:party )?experience(?P<bonus> \(with a bonus\))?!+(?: \((?P<pct>[\d.]+)%\))?$",
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
    // "You told Exit, 'war 2hander'"
    //
    // The other half of a conversation. Without it a thread reads as though
    // the other person were talking to themselves.
    (
        RuleId::ChatTellByYou,
        r"^You told (?P<tgt>[^,]+), '(?P<msg>.*)'$",
    ),
    // "Xabarn says, 'My leader is Brennagh.'" — the reply to `/pet leader`.
    //
    // The only line that names a pet's owner outright, and the only way to
    // attribute one whose name carries no clue. Works for other people's pets
    // as well as your own, which is what makes a group meter add up.
    //
    // Must precede `ChatSays`: it is a say line, and the generic rule would
    // otherwise swallow it as ordinary conversation.
    (
        RuleId::PetLeader,
        r"^(?P<pet>.+?) says, 'My leader is (?P<leader>.+?)\.'$",
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
    // "Welcome to EverQuest Legends!"
    //
    // Written once per logon, so it marks the start of a play session
    // outright. Better than inferring one from a gap in the log: a quick
    // relog leaves almost no gap at all, and a long silence mid-session looks
    // identical to having logged off.
    (RuleId::LoggedOn, r"^Welcome to EverQuest Legends!$"),
    // "You offered 1 Efreeti Scimitar to Strandar Pinemist."
    // "You complete the trade with Strandar Pinemist."
    //
    // A quest turn-in. The offers arrive first and the trade closes them, so
    // an offer that is never completed is a trade backed out of.
    (
        RuleId::TradeOffer,
        r"^You offered (?P<qty>\d+) (?P<item>.+?) to (?P<npc>.+?)\.$",
    ),
    (
        RuleId::TradeComplete,
        r"^You complete the trade with (?P<npc>.+?)\.$",
    ),
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
    // identifies a pet of yours by name. It does *not* say which kind: a
    // charmed mob and a summoned pet answer identically, and telling them
    // apart is the caller's job.
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
    // "Your pet's Sicken spell has worn off."
    //
    // Named only as "your pet", which is enough: whichever creature is yours
    // right now is the one it expired on.
    (
        RuleId::PetEffectWoreOff,
        r"^Your pet[`\u{2019}']s (?P<spell>.+?) spell has worn off\.$",
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
    (
        RuleId::TargetUnaffected,
        r"^Your target looks unaffected\.$",
    ),
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
    // "You assume a defensive stance." — the stance you fight in changes how
    // often a creature's swings land, so every melee observation is recorded
    // against the stance it was made in rather than pooled across all of them.
    (
        RuleId::StanceChanged,
        r"^You assume an? (?P<stance>[a-z]+) stance\.$",
    ),
    (
        RuleId::SpellGranted,
        r"^You have been granted the following spell: (?P<spell>.+?)\.$",
    ),
    // ---- Stuns --------------------------------------------------------------
    // Start and end are a matched pair in the log — 845 of each across the
    // reference log — so the two bound a span. Time stunned is time not
    // attacking, which is the one thing a damage meter cannot see any other
    // way.
    (RuleId::Stunned, r"^You are stunned!$"),
    (
        RuleId::StunEnded,
        r"^(?:You are no longer stunned\.|You overcome the stun!)$",
    ),
    (RuleId::StunAvoided, r"^You avoid the stunning blow\.$"),
    // ---- Tasks and achievements ---------------------------------------------
    (
        RuleId::TaskAssigned,
        r"^You have been assigned the task '(?P<task>.+)'\.$",
    ),
    // No trailing period on this one, unlike almost every other line the
    // client writes.
    (
        RuleId::ItemGranted,
        r"^You have been given: (?P<item>.+?)\.?$",
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
    // ---- Forage -------------------------------------------------------------
    // "You have scrounged up Pod of Water."
    //
    // No article, on any of the 33 in this log: the client prints the item's
    // name exactly as the database holds it, which is what makes these worth
    // recording against a zone without a cleaning pass first.
    (
        RuleId::ForageSuccess,
        r"^You have scrounged up (?P<item>.+?)\.$",
    ),
    // "You fail to locate any food nearby." — the attempt that found nothing,
    // and the denominator for everything the success line says.
    (
        RuleId::ForageFailure,
        r"^You fail to locate any food nearby\.$",
    ),
    // "Consumed 1 x Fenugreek (leaving 314) from your personal depot."
    (
        RuleId::ComponentConsumed,
        r"^Consumed (?P<qty>\d+) x (?P<item>.+?) \(leaving (?P<left>\d+)\) from your .+?\.$",
    ),
    // "You purchased 20 Water Flask from Lheron Drast for  4 platinum."
    // Two spaces after "for" in every observed line; matched loosely in case
    // that is an accident of the client rather than a guarantee.
    (
        RuleId::ItemPurchased,
        r"^You purchased (?P<qty>\d+) (?P<item>.+?) from (?P<merchant>.+?) for\s+(?P<coin>.+?)\.$",
    ),
    // Clicking an item in a merchant's window makes them quote a price, with
    // no coin changing hands:
    //
    //   Blackeye told you, 'That'll be 7 copper per Muffin.'
    //   a clockwork jeweler told you, 'That'll be 1 platinum 4 silver 9 copper
    //     for the Jeweler's Kit.'
    //
    // Worth as much as a purchase and far cheaper to collect — one click per
    // item instead of buying the shop out. Both wordings give the price of a
    // single item: across 643 pairs seen quoted *and* bought, the quote
    // matched the unit price paid every single time.
    //
    // Two rules rather than one alternation because "per" and "for the" have
    // to be tried in that order. An item whose name begins with "the" would
    // otherwise let the looser pattern win and swallow the word.
    (
        RuleId::MerchantQuotePer,
        r"^(?P<merchant>.+?) told you, 'That'll be (?P<coin>.+?) per (?P<item>.+?)\.'$",
    ),
    (
        RuleId::MerchantQuoteOne,
        r"^(?P<merchant>.+?) told you, 'That'll be (?P<coin>.+?) for the (?P<item>.+?)\.'$",
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
    // "Your Herikol's Soothing spell fizzles!" — the named form. The bare
    // "Your spell fizzles" is swept up as system noise further down, which is
    // why this has to come first: a fizzle that names its spell is a cast
    // that cost mana and produced nothing, and worth attributing.
    (
        RuleId::SpellFizzle,
        r"^Your (?P<spell>.+?) spell fizzles!$",
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
    // "Spell set WAR / DRU / ZERK saved."
    // Player-authored set names, so not authoritative — but a character names
    // their spell sets after the classes they actually play, which is far
    // better evidence than inferring a class that casts nothing.
    //
    // Saving counts as much as loading: you save a set at the moment you have
    // finished assembling the build, which is often before it is ever loaded.
    // A new loadout otherwise goes unnoticed until the next zone-in.
    (
        RuleId::SpellSetLoaded,
        r"^Spell set (?P<name>.+?) (?:loaded|saved)\.$",
    ),
    (
        RuleId::AbilityListEntry,
        r"^Ability #(?P<id>\d+): (?P<ability>.+)$",
    ),
    (
        RuleId::AbilityListDescription,
        r"^Description: (?P<text>.*)$",
    ),
    (RuleId::AbilityListCost, r"^Cost per Level: (?P<cost>\d+)$"),
    // "You have gained a level! Welcome to level 42!"
    (
        RuleId::LevelUp,
        r"^You have gained a level!\s*Welcome to level (?P<level>\d+)!$",
    ),
    // "[50 WAR/BRD/SHM] Cream (Ogre) <Sparkle Motion> ZONE: The Plane of Sky (airplane)"
    // "[10 MNK/NEC] Darmoss (Iksar)  ZONE: Befallen (befallen)"
    //
    // The only line in the log that states a level and a class loadout
    // together, and for the player's own row it is the game's own answer to
    // both. Everything else has to be inferred: level-up messages name no
    // class, and spell set names are whatever the player typed.
    //
    // Guild and zone are optional — the unguilded have neither the angle
    // brackets nor, in some client versions, the trailing zone.
    (
        RuleId::WhoEntry,
        r"^(?:AFK +)?\[(?P<level>\d+) (?P<classes>[A-Z]{2,3}(?:/[A-Z]{2,3})*)\] (?P<name>[A-Za-z`']+) \((?P<race>[^)]+)\)\s*(?:<(?P<guild>[^>]+)>)?\s*(?:ZONE: (?P<zone>.+?))?\s*$",
    ),
    // "[ANONYMOUS] Sneaky" — level, class and race withheld, name still
    // given. Still a WhoEntry, because the fact that matters is that this
    // name belongs to a player: the roster exists to keep people out of the
    // bestiary, and somebody hiding their level is exactly as much a person
    // as somebody who is not. Level 0 is "not stated", as it is elsewhere.
    //
    // Two of these in a 151 MB log, and both were lost.
    (
        RuleId::WhoAnonymous,
        r"^(?:AFK +)?\[ANONYMOUS\] (?P<name>[A-Za-z`']+)\s*(?:<(?P<guild>[^>]+)>)?\s*(?:ZONE: (?P<zone>.+?))?\s*$",
    ),
    // ---- Abilities / items --------------------------------------------------
    // "You can use the ability Rampage again in 8 minute(s) 54 seconds."
    (
        RuleId::AbilityCooldown,
        r"^You can use the ability (?P<ability>.+?) again in (?:(?P<h>\d+) hour\(s\) )?(?P<m>\d+) minute\(s\) (?P<s>\d+) seconds?\.$",
    ),
    // "Your Djarn's Amethyst Ring (Exaltation) shimmers briefly."
    //
    // The item must begin with a capital, which is what keeps this off other
    // sentences of the same shape. Guild achievements are written
    // "Your guildmate Joseywales has completed Brewing (100) achievement." and
    // were being read as the item "guildmate Joseywales has completed Brewing"
    // proccing an effect called "100" — harmless while nothing consumed the
    // event, and a row in the proc dashboard the moment something did.
    (
        RuleId::ItemProc,
        r"^Your (?P<item>[A-Z][^()]*?) \((?P<effect>[^)]+)\) .+\.$",
    ),
    // ---- Cosmetic combat feedback ------------------------------------------
    (RuleId::EmoteWince, r"^(?P<actor>.+?) winces\.$"),
    (
        RuleId::EmoteGeneric,
        r"^(?P<actor>.+?) (?P<text>is bound by strands of solid music|begins to chant|begins to choke|staggers|looks sick|has been (?:diseased|poisoned)|is (?:enveloped|engulfed) .+)\.$",
    ),
    // ---- Consider and position ---------------------------------------------
    // "A noxious spider - a rare creature - scowls at you, ready to attack
    //  -- looks kind of dangerous. (Lvl: 24)"
    //
    // The name is lazy and the attitude is a fixed alternation, which is what
    // makes the split unambiguous — see [`ATTITUDES`]. The rare marker sits
    // between them and carries its own spaces, so it is optional here rather
    // than a second rule.
    (
        RuleId::Consider,
        r"^(?P<name>.+?)(?P<rare> - a rare creature -)? (?P<attitude>{ATTITUDE}) -- (?P<difficulty>.+?) \(Lvl: *(?P<level>\d+)\)(?: \(Encounter Lock: (?P<lock>[^)]*)\))?\s*$",
    ),
    (
        RuleId::ConsiderArmy,
        r"^This creature would take an army to defeat!\s*$",
    ),
    // "Targeted (Player): Garrow"  "Targeted (NPC): a greater skeleton"
    // "Targeted (Banker): Dogle Pitt"
    (
        RuleId::Targeted,
        r"^Targeted \((?P<kind>[A-Za-z ]+)\): (?P<name>.+?)\s*$",
    ),
    // "Lord Yelinak has been defeated by a group of hardy adventurers! Please
    //  join us in congratulating Draken along with everyone else who
    //  participated in this achievement!"
    //
    // Must precede nothing in particular — no other rule matches it — but it
    // is worth a rule of its own rather than being left unknown, because the
    // shape is one keystroke away from a death line and somebody will
    // eventually be tempted to widen the death rule far enough to swallow it.
    (
        RuleId::ServerVictory,
        r"^(?P<target>.+?) has been defeated by a group of hardy adventurers! Please join us in congratulating (?P<who>.+?) along with .+$",
    ),
    (
        RuleId::Location,
        r"^Your Location is (?P<x>-?[\d.]+), (?P<y>-?[\d.]+), (?P<z>-?[\d.]+)\s*$",
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
        .map(|(_, p)| p.replace("{VERB}", &verbs).replace("{ATTITUDE}", ATTITUDES))
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
    "hit",
    "hits",
    "slash",
    "slashes",
    "kick",
    "kicks",
    "bash",
    "bashes",
    "cleave",
    "cleaves",
    "crush",
    "crushes",
    "pierce",
    "pierces",
    "punch",
    "punches",
    "bite",
    "bites",
    "claw",
    "claws",
    "slice",
    "slices",
    "sting",
    "stings",
    "maul",
    "mauls",
    "gore",
    "gores",
    "smash",
    "smashes",
    "rend",
    "rends",
    "burn",
    "burns",
    "shoot",
    "shoots",
    "strike",
    "strikes",
    "backstab",
    "backstabs",
    "frenzy",
    "frenzies",
    "slam",
    "slams",
    "tail rake",
    "tail rakes",
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
        Self::identity_from_filename(path).map(|(name, _)| name)
    }

    /// The character and server a log belongs to.
    ///
    /// The client names its logs `eqlog_<Character>_<server>.txt`, which is
    /// the only place either appears — nothing inside the file states which
    /// server it came from. History is keyed on both, so an alt or a second
    /// server does not silently merge into the same records.
    pub fn identity_from_filename(path: &str) -> Option<(String, String)> {
        let base = path.rsplit(['/', '\\']).next()?;
        let rest = base.strip_prefix("eqlog_")?;
        let rest = rest.strip_suffix(".txt").unwrap_or(rest);
        let (name, server) = rest.split_once('_')?;
        if name.is_empty() {
            return None;
        }
        Some((name.to_string(), server.to_string()))
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
                // Nobody is named, and unlike `NonMeleeGeneric` there is no
                // arguing the damage back to you: the same sentence covers your
                // dots, a group member's, and a mob's dot ticking on your ally.
                // Checked rather than assumed — of the 35 distinct spells in
                // this form in the main log, 6 are ever seen as "from your", so
                // crediting you would be wrong most of the time. It goes down
                // as damage nobody can be shown to have dealt, which keeps it
                // in the encounter and out of anyone's output.
                RuleId::DotTickUnattributed => Event::Damage {
                    source: Actor::Named(UNSEEN_ATTACKER.to_string()),
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
                RuleId::NonMeleeGeneric => {
                    let target = self.actor(g("tgt").unwrap_or_default());
                    Event::Damage {
                        // The line names nobody. When a creature took the
                        // damage it was almost certainly yours — that is what
                        // this line is, a nuke of yours the client did not name.
                        // When *you* took it, that reasoning inverts: you did
                        // not hit yourself, and saying you did would add the
                        // damage to your own output and list you among your own
                        // attackers. There is nothing in the log to name the
                        // real one, so it is named as unknown instead.
                        source: if target.is_you() {
                            Actor::Named(UNSEEN_ATTACKER.to_string())
                        } else {
                            Actor::You
                        },
                        target,
                        amount: num("amt"),
                        kind: DamageKind::DirectSpell,
                        ability: None,
                        damage_type: Some("non-melee".into()),
                        flags: Flags::default(),
                    }
                }
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
                        attempted: g("full").and_then(|s| s.parse().ok()),
                        spell: g("spell").map(str::to_string),
                        over_time: g("hot").is_some(),
                        flags: Flags::parse(g("flags")),
                    }
                }
                RuleId::HealOther => Event::Heal {
                    source: self.actor(g("src").unwrap_or_default()),
                    target: self.actor(g("tgt").unwrap_or_default()),
                    amount: num("amt"),
                    attempted: g("full").and_then(|s| s.parse().ok()),
                    spell: g("spell").map(str::to_string),
                    over_time: g("hot").is_some(),
                    flags: Flags::parse(g("flags")),
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
                RuleId::LootLeft => Event::LootLeft {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: g("qty").and_then(|q| q.trim().parse().ok()).unwrap_or(1),
                    source: g("corpse").map(|c| self.actor(c)),
                },
                RuleId::LootMarked | RuleId::LootPlain => Event::Loot {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: g("qty").and_then(|q| q.trim().parse().ok()).unwrap_or(1),
                    source: g("corpse").map(|c| self.actor(c)),
                    disposition: LootDisposition::Kept,
                },
                RuleId::LootSold => Event::Loot {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: g("qty").and_then(|q| q.trim().parse().ok()).unwrap_or(1),
                    source: g("corpse").map(|c| self.actor(c)),
                    disposition: LootDisposition::Sold(Coin::parse(g("coin").unwrap_or_default())),
                },
                RuleId::LootStored => Event::Loot {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: g("qty").and_then(|q| q.trim().parse().ok()).unwrap_or(1),
                    source: g("corpse").map(|c| self.actor(c)),
                    disposition: LootDisposition::Stored {
                        destination: g("dest").unwrap_or_default().to_string(),
                    },
                },
                RuleId::LootTransmute => Event::Loot {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: g("qty").and_then(|q| q.trim().parse().ok()).unwrap_or(1),
                    source: g("corpse").map(|c| self.actor(c)),
                    disposition: LootDisposition::Transmuted {
                        into: g("into").unwrap_or_default().to_string(),
                    },
                },
                RuleId::CoinFromCorpse | RuleId::CoinSplit => Event::CoinLoot {
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
                    bonus: g("bonus").is_some(),
                },
                RuleId::Consider => Event::Consider {
                    name: g("name").unwrap_or_default().trim().to_string(),
                    level: num("level") as u32,
                    rare: g("rare").is_some(),
                    attitude: g("attitude").unwrap_or_default().to_string(),
                    difficulty: g("difficulty").unwrap_or_default().trim().to_string(),
                },
                RuleId::ConsiderArmy => Event::ConsiderArmy,
                RuleId::Targeted => Event::Targeted {
                    name: self.actor(g("name").unwrap_or_default()),
                    kind: g("kind").unwrap_or_default().to_string(),
                },
                RuleId::ServerVictory => Event::ServerVictory {
                    target: g("target").unwrap_or_default().to_string(),
                    credited: g("who").unwrap_or_default().to_string(),
                },
                RuleId::Location => Event::Location {
                    x: g("x").and_then(|s| s.parse().ok()).unwrap_or(0.0),
                    y: g("y").and_then(|s| s.parse().ok()).unwrap_or(0.0),
                    z: g("z").and_then(|s| s.parse().ok()).unwrap_or(0.0),
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
                RuleId::ChatTellYou => Event::Tell {
                    from: self.actor(g("src").unwrap_or_default()),
                    to: Actor::You,
                    message: g("msg").unwrap_or_default().to_string(),
                },
                RuleId::ChatTellByYou => Event::Tell {
                    from: Actor::You,
                    to: self.actor(g("tgt").unwrap_or_default()),
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
                RuleId::WhoAnonymous => Event::WhoEntry {
                    level: 0,
                    classes: String::new(),
                    name: g("name").unwrap_or_default().to_string(),
                    race: String::new(),
                    guild: g("guild").unwrap_or_default().to_string(),
                    zone: g("zone").unwrap_or_default().to_string(),
                },
                RuleId::WhoEntry => Event::WhoEntry {
                    level: num("level") as u32,
                    classes: g("classes").unwrap_or_default().to_string(),
                    name: g("name").unwrap_or_default().to_string(),
                    race: g("race").unwrap_or_default().to_string(),
                    guild: g("guild").unwrap_or_default().to_string(),
                    zone: g("zone").unwrap_or_default().trim().to_string(),
                },
                RuleId::LevelUp => Event::LevelUp {
                    level: num("level") as u32,
                },
                RuleId::AbilityPointsGained => Event::AbilityPointsGained {
                    // The "an ability point" wording carries no number.
                    points: if g("points").is_some() {
                        num("points") as u32
                    } else {
                        1
                    },
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
                    flags: Flags::parse(g("flags")),
                },
                RuleId::AvoidedByYou => Event::Avoided {
                    source: Actor::You,
                    target: self.actor(g("tgt").unwrap_or_default()),
                    how: normalise_avoidance(g("how").unwrap_or_default()),
                    flags: Flags::parse(g("flags")),
                },
                RuleId::ResistedYours => Event::Resisted {
                    target: self.actor(g("tgt").unwrap_or_default()),
                    spell: g("spell").unwrap_or_default().to_string(),
                },
                RuleId::ResistedGeneric => Event::Resisted {
                    target: Actor::Named("your target".into()),
                    spell: g("spell").unwrap_or_default().to_string(),
                },
                RuleId::ResistedByYou => Event::Resisted {
                    target: Actor::You,
                    spell: g("spell").unwrap_or_default().to_string(),
                },
                RuleId::SkillUp => Event::SkillUp {
                    skill: g("skill").unwrap_or_default().to_string(),
                    value: num("value") as u32,
                },
                RuleId::Stunned => Event::Stunned,
                RuleId::StunEnded => Event::StunEnded,
                RuleId::StunAvoided => Event::StunAvoided,
                RuleId::TaskAssigned => Event::TaskAssigned {
                    task: g("task").unwrap_or_default().to_string(),
                },
                RuleId::ItemGranted => Event::ItemGranted {
                    item: g("item").unwrap_or_default().trim().to_string(),
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
                RuleId::ForageSuccess => Event::Forage {
                    item: Some(g("item").unwrap_or_default().trim().to_string()),
                },
                RuleId::ForageFailure => Event::Forage { item: None },
                RuleId::ComponentConsumed => Event::ComponentConsumed {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: num("qty"),
                    remaining: num("left"),
                },
                RuleId::MerchantQuotePer | RuleId::MerchantQuoteOne => {
                    Event::ItemQuoted {
                        item: g("item").unwrap_or_default().to_string(),
                        merchant: g("merchant").unwrap_or_default().to_string(),
                        coin: Coin::parse(g("coin").unwrap_or_default()),
                    }
                }
                RuleId::ItemPurchased => Event::ItemPurchased {
                    item: g("item").unwrap_or_default().to_string(),
                    quantity: num("qty"),
                    merchant: g("merchant").unwrap_or_default().to_string(),
                    coin: Coin::parse(g("coin").unwrap_or_default()),
                },
                RuleId::RecipeLearned => Event::RecipeLearned {
                    recipe: g("recipe").unwrap_or_default().to_string(),
                },
                RuleId::SongFizzle
                | RuleId::SpellFizzle
                | RuleId::SpellInterruptedNamed => Event::CastInterrupted { source: Actor::You },
                RuleId::SpellInterruptedOther => Event::CastInterrupted {
                    source: self.actor(g("src").unwrap_or_default()),
                },
                // Resuming is the opposite of interrupting, and the model has
                // no word for it. Reported as a start of the same cast, which
                // is what it is: the creature is casting again from here.
                RuleId::CastResumedOther => Event::CastStart {
                    source: self.actor(g("src").unwrap_or_default()),
                    spell: String::new(),
                    singing: false,
                },
                RuleId::SpellMemorized => Event::SpellbookChange {
                    spell: g("spell").unwrap_or_default().to_string(),
                    change: SpellbookChangeKind::Memorized,
                },
                RuleId::StanceChanged => Event::StanceChanged {
                    stance: g("stance").unwrap_or_default().to_string(),
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
                RuleId::PetLeader => Event::PetLeader {
                    pet: self.actor(g("pet").unwrap_or_default()),
                    leader: self.actor(g("leader").unwrap_or_default()),
                },
                RuleId::LoggedOn => Event::LoggedOn,
                RuleId::TradeOffer => Event::TradeOffer {
                    quantity: num("qty"),
                    item: g("item").unwrap_or_default().to_string(),
                    npc: g("npc").unwrap_or_default().to_string(),
                },
                RuleId::TradeComplete => Event::TradeComplete {
                    npc: g("npc").unwrap_or_default().to_string(),
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
                RuleId::PetEffectWoreOff => Event::EffectWoreOffTarget {
                    spell: g("spell").unwrap_or_default().to_string(),
                    target: Actor::Named("your pet".into()),
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
                RuleId::ZoneRuleNotice
                | RuleId::SystemAutoAttack
                | RuleId::SystemMessage => Event::System {
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
///
/// # The one rule this imposes
///
/// The result is not a real instant. It is the client's wall clock read as
/// though it were UTC, so it sits a whole timezone offset away from
/// `SystemTime::now()`. Comparing the two directly is wrong by hours and looks
/// plausible, which is the dangerous combination: every finished fight reads
/// as still running, every lockout as freshly reset.
///
/// Two bridges exist and one of them must be used:
///
/// * `Engine::now()` — wall clock carried into this domain by a calibrated
///   offset, which is also what makes timers keep counting during a lull.
/// * `nowLog()` in the renderer — the same conversion, using the browser's
///   timezone offset.
///
/// Formatting follows the same rule: the window prints these with
/// `toISOString()`, so what is on screen is the clock the player was looking
/// at when the line was written.
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

    /// The client conjugates for the second person, and only the third-person
    /// form was ever matched — so every swing of yours that was dodged,
    /// parried or blocked parsed as Unknown.
    #[test]
    fn your_own_swings_can_be_avoided_too() {
        let both = |body: &str| p().parse_body(body);
        match both("You try to slash Maestro of Rancor, but Maestro of Rancor parries!") {
            Event::Avoided {
                source,
                target,
                how,
                ..
            } => {
                assert!(source.is_you());
                assert_eq!(target.display("Tharn"), "Maestro of Rancor");
                assert_eq!(how, "parry");
            }
            other => panic!("expected Avoided, got {other:?}"),
        }
        // The third-person form still works, and still names its defender.
        match both("A giant tries to hit YOU, but YOU parry!") {
            Event::Avoided { source, how, .. } => {
                assert_eq!(source.display("Tharn"), "A giant");
                assert_eq!(how, "parry");
            }
            other => panic!("expected Avoided, got {other:?}"),
        }
        // A flurry that gets parried still flurried.
        match both("You try to slash a giant, but a giant dodges! (Flurry)") {
            Event::Avoided { how, flags, .. } => {
                assert_eq!(how, "dodge");
                assert!(flags.has("Flurry"));
            }
            other => panic!("expected Avoided, got {other:?}"),
        }
        // And a plain miss is still a miss, not an avoidance.
        assert!(matches!(
            both("You try to slash a giant, but miss!"),
            Event::Miss { .. }
        ));
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

    /// A set is saved the moment a build is finished — often long before it is
    /// first loaded. Both are the player naming their loadout.
    #[test]
    fn spell_set_saved_counts_as_much_as_loaded() {
        let p = Parser::new("Tharn");
        for (line, want) in [
            ("Spell set WAR / DRU / ZERK saved.", "WAR / DRU / ZERK"),
            (
                "Spell set WAR / BRD / DRU - Eye loaded.",
                "WAR / BRD / DRU - Eye",
            ),
        ] {
            let ev = p
                .parse_line(&format!("[Tue Aug 04 00:44:13 2026] {line}"))
                .expect("parsed");
            match ev.event {
                Event::SpellSetLoaded { name } => assert_eq!(name, want),
                other => panic!("expected a spell set, got {other:?}"),
            }
        }
    }

    #[test]
    fn player_inferred_from_filename() {
        assert_eq!(
            Parser::player_from_filename("/logs/eqlog_Tharn_freeport.txt").as_deref(),
            Some("Tharn")
        );
        assert_eq!(Parser::player_from_filename("notalog.txt"), None);

        // Character and server both, since history is keyed on the pair.
        assert_eq!(
            Parser::identity_from_filename("/logs/eqlog_Tharn_freeport.txt"),
            Some(("Tharn".to_string(), "freeport".to_string()))
        );
        // A server name containing an underscore stays whole.
        assert_eq!(
            Parser::identity_from_filename("eqlog_Alt_test_server.txt"),
            Some(("Alt".to_string(), "test_server".to_string()))
        );
        assert_eq!(Parser::identity_from_filename("eqlog_NoServer.txt"), None);
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

    /// Frenzy is the one melee verb that takes a preposition. Left in the
    /// target, it invents a creature per victim: a log with 484 frenzy lines
    /// produced 37 of them, "on YOU" among the names.
    #[test]
    fn frenzy_takes_a_preposition_and_the_target_does_not_keep_it() {
        // Third person, damage.
        match p().parse_body("Morgok frenzies on a cracked skeleton for 12 points of damage.") {
            Event::Damage { source, target, amount, kind, ability, .. } => {
                assert_eq!(source, Actor::Named("Morgok".into()));
                assert_eq!(target, Actor::Named("a cracked skeleton".into()));
                assert_eq!(amount, 12);
                assert_eq!(kind, DamageKind::Melee);
                assert_eq!(ability.as_deref(), Some("frenzies"));
            }
            other => panic!("expected melee damage, got {other:?}"),
        }
        // Second person, damage.
        match p().parse_body("You frenzy on a fire giant warrior for 40 points of damage.") {
            Event::Damage { source, target, .. } => {
                assert_eq!(source, Actor::You);
                assert_eq!(target, Actor::Named("a fire giant warrior".into()));
            }
            other => panic!("expected melee damage, got {other:?}"),
        }
        // Misses, both persons. "on YOU" was the most common invented name.
        match p().parse_body("Lord of Ire tries to frenzy on YOU, but misses! (Flurry)") {
            Event::Miss { source, target, .. } => {
                assert_eq!(source, Actor::Named("Lord of Ire".into()));
                assert_eq!(target, Actor::You);
            }
            other => panic!("expected a miss, got {other:?}"),
        }
        match p().parse_body("You try to frenzy on a lesser mummy, but miss!") {
            Event::Miss { source, target, .. } => {
                assert_eq!(source, Actor::You);
                assert_eq!(target, Actor::Named("a lesser mummy".into()));
            }
            other => panic!("expected a miss, got {other:?}"),
        }
        // A verb that does NOT take one is unaffected: "on" here would be
        // part of a name, and the optional group must not eat it.
        match p().parse_body("Jaranab pierces a lesser mummy for 1 point of damage.") {
            Event::Damage { target, .. } => {
                assert_eq!(target, Actor::Named("a lesser mummy".into()));
            }
            other => panic!("expected melee damage, got {other:?}"),
        }
    }

    /// The unattributed non-melee line, in both persons. The second person
    /// drops "points of", which is why none of these were parsed at all.
    #[test]
    fn an_unattributed_non_melee_hit_is_counted_and_its_attacker_named_unknown() {
        // Damage to a creature with nobody named is a nuke of yours the client
        // did not name, and stays credited to you.
        match p().parse_body("Lord Nagafen was hit by non-melee for 500 points of damage.") {
            Event::Damage { source, target, amount, .. } => {
                assert_eq!(source, Actor::You);
                assert_eq!(target, Actor::Named("Lord Nagafen".into()));
                assert_eq!(amount, 500);
            }
            other => panic!("expected damage, got {other:?}"),
        }
        // Damage to you is not something you did to yourself. Nothing names
        // the attacker, so the attacker is named as unknown.
        match p().parse_body("You were hit by non-melee for 118 damage.") {
            Event::Damage { source, target, amount, .. } => {
                assert_eq!(source, Actor::Named(UNSEEN_ATTACKER.into()));
                assert_eq!(target, Actor::You);
                assert_eq!(amount, 118);
            }
            other => panic!("expected damage, got {other:?}"),
        }
    }

    #[test]
    fn melee_singular_point_is_handled() {
        let e = p().parse_body("Jaranab pierces a lesser mummy for 1 point of damage.");
        assert!(matches!(
            e,
            Event::Damage {
                amount: 1,
                kind: DamageKind::Melee,
                ..
            }
        ));
    }

    #[test]
    fn melee_against_you_with_riposte() {
        let e = p().parse_body("A giant fire beetle bites YOU for 2 points of damage. (Riposte)");
        match e {
            Event::Damage {
                source,
                target,
                flags,
                ..
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
        let e = p()
            .parse_body("You hit a lesser mummy for 4 points of magic damage by Shallow Breath.");
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

        match p().parse_body(
            "You hit a mummy for 40 points of magic damage by Shallow Breath. (Critical)",
        ) {
            Event::Damage {
                amount,
                kind,
                flags,
                ..
            } => {
                assert_eq!(amount, 40);
                assert_eq!(kind, DamageKind::DirectSpell);
                assert!(flags.is_crit());
            }
            other => panic!("expected a critical spell hit, got {other:?}"),
        }

        // A dot cast by someone else, also critting.
        match p()
            .parse_body("A rat has taken 8 damage from Clinging Darkness by Sticky. (Critical)")
        {
            Event::Damage { source, flags, .. } => {
                assert_eq!(source, Actor::Named("Sticky".into()));
                assert!(flags.is_crit());
            }
            other => panic!("expected a dot tick, got {other:?}"),
        }
    }

    #[test]
    fn dot_tick_yours_attributes_to_you() {
        let e = p()
            .parse_body("An imp protector has taken 37 damage from your Chords of Dissonance III.");
        match e {
            Event::Damage {
                source,
                kind,
                ability,
                ..
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
        let e =
            p().parse_body("A young kodiak has taken 8 damage from Clinging Darkness by Sticky.");
        match e {
            Event::Damage {
                source,
                target,
                ability,
                ..
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
                source,
                kind,
                amount,
                ..
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
                source,
                target,
                kind,
                ..
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
            Event::Miss {
                source: Actor::You,
                ..
            }
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
        let e =
            p().parse_body("Persephone healed herself for 4 hit points by Blessing of the Knight.");
        match e {
            Event::Heal {
                source,
                target,
                amount,
                spell,
                over_time,
                ..
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

    /// The shape that used to match no rule at all, so the healing went
    /// missing rather than merely losing its crit marker.
    #[test]
    fn heal_can_crit() {
        let e = p().parse_body(
            "You healed Tharn over time for 306 hit points by Efflorescing Heal. (Critical)",
        );
        match e {
            Event::Heal {
                amount,
                spell,
                over_time,
                flags,
                ..
            } => {
                assert_eq!(amount, 306);
                assert_eq!(spell.as_deref(), Some("Efflorescing Heal"));
                assert!(over_time);
                assert!(flags.is_crit());
            }
            other => panic!("expected heal, got {other:?}"),
        }
    }

    /// "for 47 (241) hit points": the first number landed, the second is what
    /// the heal was worth before the target's missing health capped it.
    #[test]
    fn heal_reports_the_pre_cap_amount() {
        match p().parse_body("Jabarab healed himself for 47 (241) hit points by Lay on Hands.") {
            Event::Heal {
                amount,
                attempted,
                spell,
                ..
            } => {
                assert_eq!(amount, 47, "only what actually landed counts as healing");
                assert_eq!(attempted, Some(241));
                assert_eq!(spell.as_deref(), Some("Lay on Hands"));
            }
            other => panic!("expected heal, got {other:?}"),
        }

        // Both extras at once, and a heal that landed nothing at all.
        match p().parse_body(
            "You healed Brennagh for 2405 (12600) hit points by Promised Renewal Trigger I. (Critical)",
        ) {
            Event::Heal {
                amount,
                attempted,
                flags,
                ..
            } => {
                assert_eq!(amount, 2405);
                assert_eq!(attempted, Some(12600));
                assert!(flags.is_crit());
            }
            other => panic!("expected heal, got {other:?}"),
        }

        match p().parse_body("Jobeker healed itself for 0 (6) hit points by Lifetap.") {
            Event::Heal {
                amount, attempted, ..
            } => {
                assert_eq!(amount, 0);
                assert_eq!(attempted, Some(6));
            }
            other => panic!("expected heal, got {other:?}"),
        }
    }

    /// A plain heal must still report no pre-cap figure, rather than repeating
    /// the amount and implying nothing was wasted.
    #[test]
    fn heal_without_a_cap_figure_states_nothing() {
        match p().parse_body("You healed Tharn for 250 hit points by Healing Water.") {
            Event::Heal {
                amount, attempted, ..
            } => {
                assert_eq!(amount, 250);
                assert_eq!(attempted, None);
            }
            other => panic!("expected heal, got {other:?}"),
        }
    }

    /// A dot tick that names the spell but no caster. It must not be credited
    /// to anyone — the same sentence covers your dots and a mob's.
    #[test]
    fn dot_tick_without_a_caster_is_unattributed() {
        match p().parse_body("A glare lord has taken 100 damage by Rotting Flesh.") {
            Event::Damage {
                source,
                target,
                amount,
                kind,
                ability,
                ..
            } => {
                assert_eq!(source, Actor::Named(UNSEEN_ATTACKER.into()));
                assert_eq!(target, Actor::Named("A glare lord".into()));
                assert_eq!(amount, 100);
                assert_eq!(kind, DamageKind::DamageOverTime);
                assert_eq!(ability.as_deref(), Some("Rotting Flesh"));
            }
            other => panic!("expected damage, got {other:?}"),
        }
    }

    /// The form that names a caster still wins: `by` means the caster there and
    /// the spell in the rule above, and only `from` tells them apart.
    #[test]
    fn dot_tick_with_a_caster_still_wins() {
        match p().parse_body("A young kodiak has taken 8 damage from Clinging Darkness by Sticky.") {
            Event::Damage {
                source, ability, ..
            } => {
                assert_eq!(source, Actor::Named("Sticky".into()));
                assert_eq!(ability.as_deref(), Some("Clinging Darkness"));
            }
            other => panic!("expected damage, got {other:?}"),
        }
    }

    /// A paladin's slay is a critical, and the compound flag must still read as
    /// a riposte.
    #[test]
    fn slay_undead_counts_as_a_crit() {
        match p().parse_body("You slash Lord Nagafen for 900 points of damage. (Slay Undead)") {
            Event::Damage { flags, amount, .. } => {
                assert_eq!(amount, 900);
                assert!(flags.is_crit(), "a slay is a critical hit");
                assert_eq!(flags.crit_kinds(), vec!["Slay Undead"]);
            }
            other => panic!("expected damage, got {other:?}"),
        }

        match p().parse_body("You slash Lord Nagafen for 900 points of damage. (Riposte Slay Undead)")
        {
            Event::Damage { flags, .. } => {
                assert!(flags.has("Riposte"), "the riposte must register too");
                assert!(flags.is_crit());
            }
            other => panic!("expected damage, got {other:?}"),
        }
    }

    // ---- Live EverQuest wordings ------------------------------------------
    //
    // Every line below was taken verbatim from a real log. They are grouped
    // because they share one cause: EverQuest and EverQuest Legends word the
    // same happening differently, and each of these was silently unknown
    // until it was read against the live client rather than assumed from the
    // other one.

    /// "was slain by" is the wording for a kill you were not grouped for —
    /// which includes every kill your own pet makes. Without it a pet class
    /// parsed as though it had killed nothing all night.
    #[test]
    fn a_pet_kill_is_a_death() {
        match p().parse_body("A forest giant greenwood was slain by Tharn`s warder!") {
            Event::Death { victim, killer } => {
                assert_eq!(victim, Actor::Named("A forest giant greenwood".into()));
                assert_eq!(killer, Some(Actor::Named("Tharn`s warder".into())));
            }
            other => panic!("expected a death, got {other:?}"),
        }
        // The grouped wording still parses; both forms mean the same thing.
        match p().parse_body("Orc pawn has been slain by Reklar!") {
            Event::Death { killer, .. } => {
                assert_eq!(killer, Some(Actor::Named("Reklar".into())));
            }
            other => panic!("expected a death, got {other:?}"),
        }
    }

    /// The server-wide boast is one word away from a death line and names a
    /// creature nobody here fought. It must not become a kill.
    #[test]
    fn a_server_announcement_is_not_a_kill() {
        let e = p().parse_body(
            "Lord Yelinak has been defeated by a group of hardy adventurers! \
             Please join us in congratulating Draken along with everyone else \
             who participated in this achievement!",
        );
        match e {
            Event::ServerVictory { target, credited } => {
                assert_eq!(target, "Lord Yelinak");
                assert_eq!(credited, "Draken");
            }
            other => panic!("expected a server victory, got {other:?}"),
        }
    }

    /// An item on the corpse you chose not to take. It is a drop sighting,
    /// not a piece of loot, and the two are kept apart deliberately.
    #[test]
    fn an_item_left_behind_is_not_looted() {
        match p().parse_body("--You left a Rusty Two Handed Sword on a snow orc trooper.--") {
            Event::LootLeft {
                item,
                quantity,
                source,
            } => {
                assert_eq!(item, "Rusty Two Handed Sword");
                assert_eq!(quantity, 1);
                assert_eq!(source, Some(Actor::Named("a snow orc trooper".into())));
            }
            other => panic!("expected an item left behind, got {other:?}"),
        }
    }

    /// Grouped coin arrives already divided, and the bonus clause is part of
    /// what arrived rather than a separate payment.
    #[test]
    fn a_coin_split_is_coin() {
        match p().parse_body("You receive 2 gold, 1 silver and 9 copper as your split (with a bonus).") {
            Event::CoinLoot { coin } => assert_eq!(coin.as_copper(), 219),
            other => panic!("expected coin, got {other:?}"),
        }
        match p().parse_body("You receive 3 silver as your split.") {
            Event::CoinLoot { coin } => assert_eq!(coin.as_copper(), 30),
            other => panic!("expected coin, got {other:?}"),
        }
    }

    /// The live client appends who holds the encounter. The consider is the
    /// same consider, and the level still has to come out of it.
    #[test]
    fn an_encounter_lock_does_not_hide_the_consider() {
        match p().parse_body(
            "A large heartsting scorpion scowls at you, ready to attack -- \
             You could probably win this fight. (Lvl: 17) (Encounter Lock: Tharn)",
        ) {
            Event::Consider { name, level, .. } => {
                assert_eq!(name, "A large heartsting scorpion");
                assert_eq!(level, 17);
            }
            other => panic!("expected a consider, got {other:?}"),
        }
    }

    /// The only line that says outright whether a name is a person. The kind
    /// is carried verbatim because the client writes more than two of them.
    #[test]
    fn targeting_says_what_kind_of_thing_it_is() {
        match p().parse_body("Targeted (Player): Garrow") {
            Event::Targeted { name, kind } => {
                assert_eq!(name, Actor::Named("Garrow".into()));
                assert_eq!(kind, "Player");
            }
            other => panic!("expected a target, got {other:?}"),
        }
        match p().parse_body("Targeted (Banker): Dogle Pitt") {
            Event::Targeted { kind, .. } => assert_eq!(kind, "Banker"),
            other => panic!("expected a target, got {other:?}"),
        }
    }

    /// A fizzle and an interruption both end a cast, and the live client
    /// names the spell in each. The bare wordings are swept up as system
    /// noise, so a named one that fell through was a lost cast.
    #[test]
    fn a_named_fizzle_ends_the_cast() {
        for line in [
            "Your Herikol's Soothing spell fizzles!",
            "Your Touch of Night spell is interrupted.",
        ] {
            match p().parse_body(line) {
                Event::CastInterrupted { source } => assert_eq!(source, Actor::You),
                other => panic!("expected an interrupted cast for {line:?}, got {other:?}"),
            }
        }
        match p().parse_body("a goblin soothsayer's Root spell is interrupted.") {
            Event::CastInterrupted { source } => {
                assert_eq!(source, Actor::Named("a goblin soothsayer".into()));
            }
            other => panic!("expected an interrupted cast, got {other:?}"),
        }
    }

    /// Resisting something aimed at you, rather than something you cast
    /// being resisted. Only the spell survives: the event has nowhere to
    /// record the caster.
    #[test]
    fn a_resist_you_made_names_the_spell() {
        match p().parse_body("You resist a goblin soothsayer's Root!") {
            Event::Resisted { target, spell } => {
                assert_eq!(target, Actor::You);
                assert_eq!(spell, "Root");
            }
            other => panic!("expected a resist, got {other:?}"),
        }
    }

    /// A buff expiring on a pet the client will not name.
    #[test]
    fn a_pet_buff_expiry_lands_on_the_pet() {
        match p().parse_body("Your pet's Sicken spell has worn off.") {
            Event::EffectWoreOffTarget { spell, target } => {
                assert_eq!(spell, "Sicken");
                assert_eq!(target, Actor::Named("your pet".into()));
            }
            other => panic!("expected a buff expiry, got {other:?}"),
        }
    }

    #[test]
    fn heal_over_time_flagged() {
        let e =
            p().parse_body("You healed Tharn over time for 153 hit points by Efflorescing Heal.");
        match e {
            Event::Heal {
                over_time,
                amount,
                spell,
                ..
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
            Event::Death {
                killer: Some(Actor::You),
                ..
            }
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

    /// Clicking an item in a merchant's window, which prices a whole shop
    /// without buying anything. Both wordings give the price of one item.
    #[test]
    fn a_merchant_quote_is_a_price_without_a_sale() {
        match p().parse_body("Blackeye told you, 'That'll be 7 copper per Muffin.'") {
            Event::ItemQuoted { item, merchant, coin } => {
                assert_eq!(item, "Muffin");
                assert_eq!(merchant, "Blackeye");
                assert_eq!(coin.as_copper(), 7);
            }
            other => panic!("expected a quote, got {other:?}"),
        }
        // The other wording, and a merchant whose name is several lowercase
        // words rather than a proper noun.
        match p().parse_body(
            "a clockwork jeweler told you, 'That'll be 1 platinum 4 silver 9 copper \
             for the Jeweler's Kit.'",
        ) {
            Event::ItemQuoted { item, merchant, coin } => {
                assert_eq!(item, "Jeweler's Kit");
                assert_eq!(merchant, "a clockwork jeweler");
                assert_eq!(coin.as_copper(), 1049);
            }
            other => panic!("expected a quote, got {other:?}"),
        }
    }

    /// A merchant offering to buy is quoting a different number, and must not
    /// be read as what they sell for.
    #[test]
    fn an_offer_to_buy_is_not_an_asking_price() {
        let e = p().parse_body(
            "Blackeye told you, 'I'll give you 1 copper for the Blackburrow Cask.'",
        );
        assert!(
            !matches!(e, Event::ItemQuoted { .. }),
            "an offer to buy was read as an asking price: {e:?}"
        );
    }

    /// The only line that says what anything costs. Note the two spaces after
    /// "for", which the client writes every time.
    #[test]
    fn purchase_captures_quantity_and_price() {
        match p()
            .parse_body("You purchased 20 Water Flask from Lheron Drast for  4 platinum.")
        {
            Event::ItemPurchased {
                item,
                quantity,
                merchant,
                coin,
            } => {
                assert_eq!(item, "Water Flask");
                assert_eq!(quantity, 20);
                assert_eq!(merchant, "Lheron Drast");
                assert_eq!(coin.as_copper(), 4000);
            }
            other => panic!("expected a purchase, got {other:?}"),
        }
    }

    /// A merchant whose name is several words, and a price in every
    /// denomination — the shape most purchases actually take.
    #[test]
    fn purchase_handles_multi_word_merchant_and_mixed_coin() {
        match p()
            .parse_body(
                "You purchased 1 Backpack from a clockwork merchant for  9 gold 5 silver 2 copper.",
            )
        {
            Event::ItemPurchased {
                item,
                quantity,
                merchant,
                coin,
            } => {
                assert_eq!(item, "Backpack");
                assert_eq!(quantity, 1);
                assert_eq!(merchant, "a clockwork merchant");
                assert_eq!(coin.as_copper(), 952);
            }
            other => panic!("expected a purchase, got {other:?}"),
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
        match p().parse_body("You looted a Rusty Dagger from a rat's corpse and sold it for free.")
        {
            Event::Loot {
                disposition,
                source,
                ..
            } => {
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
            Event::Experience { percent, bonus } => {
                assert!((percent - 1.585).abs() < 1e-9);
                assert!(!bonus);
            }
            other => panic!("expected experience, got {other:?}"),
        }
    }

    /// Every shape the two Freeport logs actually hold.
    ///
    /// Counted rather than chosen: 11,809 plain with a figure, 4,324 plain
    /// without, 2,061 bonused with a figure, 68 bonused without, 60 party.
    /// The two bonused shapes -- 2,129 lines -- matched nothing at all until
    /// the pattern learned `(with a bonus)`, and an unmatched line is not a
    /// gain recorded badly, it is a gain not recorded.
    #[test]
    fn every_experience_line_the_logs_hold() {
        let cases: &[(&str, f64, bool)] = &[
            ("You gain experience! (1.585%)", 1.585, false),
            ("You gain experience!", 0.0, false),
            ("You gain experience (with a bonus)! (0.087%)", 0.087, true),
            ("You gain experience (with a bonus)!", 0.0, true),
            ("You gain party experience! (0.242%)", 0.242, false),
        ];
        for (line, want, marked) in cases {
            match p().parse_body(line) {
                Event::Experience { percent, bonus } => {
                    assert!((percent - want).abs() < 1e-9, "{line}: got {percent}");
                    assert_eq!(bonus, *marked, "{line}");
                }
                other => panic!("{line}: expected experience, got {other:?}"),
            }
        }
    }

    /// The bonus group is the client's phrase and not a wildcard.
    ///
    /// Spelled out because the group sits inside an anchored pattern, where a
    /// looser one would swallow whatever the client puts there next and report
    /// it as a bonus rather than as a line nobody has looked at.
    #[test]
    fn an_unknown_parenthetical_is_not_a_bonus() {
        assert!(!matches!(
            p().parse_body("You gain experience (with a penalty)! (1.0%)"),
            Event::Experience { .. }
        ));
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
                    Event::Damage { .. }
                        | Event::Miss { .. }
                        | Event::Heal { .. }
                        | Event::Death { .. }
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
        match p()
            .parse_body("You have gained 2 ability point(s)!  You now have 5 ability point(s).")
        {
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
        match p().parse_body(
            "You have improved Symphonic Aura: Enabled 2 at a cost of 0 ability points.",
        ) {
            Event::AbilityPointsSpent {
                ability,
                rank,
                cost,
            } => {
                assert_eq!(ability, "Symphonic Aura: Enabled");
                assert_eq!(
                    rank, 2,
                    "trailing digits are the rank, not part of the name"
                );
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
            p().parse_body(
                "You have gained the ability \"Banestrike\" at a cost of 0 ability points."
            ),
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

    /// `/pet leader` is a say line, so it has to beat the generic chat rule to
    /// the match or it arrives as ordinary conversation.
    #[test]
    fn pet_leader_beats_the_chat_rule() {
        match p().parse_body("Xabarn says, 'My leader is Brennagh.'") {
            Event::PetLeader { pet, leader } => {
                assert_eq!(pet.display("Tharn"), "Xabarn");
                assert_eq!(leader.display("Tharn"), "Brennagh");
            }
            other => panic!("expected a pet leader line, got {other:?}"),
        }
    }

    /// An ordinary say that merely mentions a leader is still conversation.
    #[test]
    fn ordinary_say_is_not_a_pet_leader_line() {
        match p().parse_body("Trux says, 'need to be in a town no to guild leader'") {
            Event::Chat { .. } => {}
            other => panic!("expected chat, got {other:?}"),
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

    /// A guildmate's achievement is the same shape as an item proc and is not
    /// one. Read as one it invented an item and an effect, both from a
    /// sentence about somebody else.
    #[test]
    fn a_guild_achievement_is_not_an_item_proc() {
        for line in [
            "Your guildmate Joseywales has completed Brewing (100) achievement.",
            "Your guildmate Ixthy has completed Race Unlock - Human (Qeynos) achievement.",
        ] {
            if let Event::ItemProc { item, effect } = p().parse_body(line) {
                panic!("read {line:?} as item {item:?} proccing {effect:?}")
            }
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

#[cfg(test)]
mod consider_tests {
    use super::*;
    use crate::event::Event;

    fn ev(body: &str) -> Event {
        Parser::new("Tharn").parse_body(body)
    }

    /// The plain case, and the point of the whole rule: the creature is named
    /// in full rather than cut at its first word.
    #[test]
    fn a_consider_names_the_creature_not_its_first_word() {
        let e = ev("A woodland lion scowls at you, ready to attack -- \
                    what would you like your tombstone to say? (Lvl: 12)");
        match e {
            Event::Consider { name, level, rare, attitude, difficulty } => {
                assert_eq!(name, "A woodland lion");
                assert_eq!(level, 12);
                assert!(!rare);
                assert_eq!(attitude, "scowls at you, ready to attack");
                assert_eq!(difficulty, "what would you like your tombstone to say?");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The rare marker is the client's own word, and it sits between the name
    /// and the attitude rather than at either end.
    #[test]
    fn the_rare_marker_is_read_and_kept_out_of_the_name() {
        match ev("a noxious spider - a rare creature - judges you amiably -- \
                  You could probably win this fight. (Lvl: 24)")
        {
            Event::Consider { name, rare, level, .. } => {
                assert_eq!(name, "a noxious spider");
                assert!(rare);
                assert_eq!(level, 24);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A hyphen in the creature's own name is not the rare marker.
    #[test]
    fn a_hyphenated_name_survives() {
        match ev("Cazic-Thule glares at you threateningly -- \
                  looks like it would wipe the floor with you! (Lvl: 55)")
        {
            Event::Consider { name, rare, .. } => {
                assert_eq!(name, "Cazic-Thule");
                assert!(!rare, "the hyphen in the name is not a rare marker");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The raid line names nothing, which is why it is its own event.
    #[test]
    fn the_army_line_is_its_own_event() {
        assert_eq!(ev("This creature would take an army to defeat!"), Event::ConsiderArmy);
    }

    /// Position, in the client's own order and sign.
    #[test]
    fn a_location_keeps_the_clients_own_numbers() {
        match ev("Your Location is 179.17, -144.70, -20.17") {
            Event::Location { x, y, z } => {
                assert!((x - 179.17).abs() < 1e-9);
                assert!((y + 144.70).abs() < 1e-9);
                assert!((z + 20.17).abs() < 1e-9);
            }
            other => panic!("{other:?}"),
        }
    }

    /// An attitude the table does not know leaves the line alone rather than
    /// inventing a creature out of its first word.
    ///
    /// The failure mode that matters. A patch adding a ninth attitude should
    /// cost a consider, which `eqldb-logscan --census` will show, and not
    /// produce a creature called "A".
    #[test]
    fn an_unknown_attitude_is_not_parsed_into_a_wrong_creature() {
        let e = ev("A woodland lion sneers at you contemptuously -- \
                    looks kind of dangerous. (Lvl: 12)");
        assert_eq!(e, Event::Unknown, "{e:?}");
    }
}

#[cfg(test)]
mod zone_notice_tests {
    use super::*;
    use crate::event::Event;

    fn ev(body: &str) -> Event {
        Parser::new("Tharn").parse_body(body)
    }

    /// A place is a place.
    #[test]
    fn entering_a_zone_is_a_zone_change() {
        match ev("You have entered The Oasis of Marr.") {
            Event::ZoneChange { zone } => assert_eq!(zone, "The Oasis of Marr"),
            other => panic!("{other:?}"),
        }
    }

    /// And a rule announcement is not, however much it reads like one.
    ///
    /// Two of these are in the log. Read as zones they file kills under a
    /// place that does not exist, and split the real zone's tally in two.
    #[test]
    fn entering_an_area_rule_is_not_a_place() {
        for line in [
            "You have entered an area where Bind Affinity is allowed.",
            "You have entered an area where levitation effects do not function.",
        ] {
            assert!(
                matches!(ev(line), Event::System { .. }),
                "{line} became {:?}",
                ev(line)
            );
        }
    }

    /// A zone whose own name starts with "area" would still be one, but none
    /// do; the guard is deliberately narrow to the client's two sentences.
    #[test]
    fn the_guard_does_not_swallow_an_ordinary_name() {
        assert!(matches!(ev("You have entered Areana.") , Event::ZoneChange { .. }));
    }
}
