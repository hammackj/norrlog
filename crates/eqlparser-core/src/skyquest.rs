//! Plane of Sky quest tracking.
//!
//! # Why this is a catalogue and not a parser
//!
//! The Sky quests are repeatable turn-ins: you hail a class-specific NPC, say
//! a trigger phrase, and hand over a Wind Rune plus a set of drops from
//! specific islands. The client logs none of that structure — no objective
//! text, no "you need N more", and turn-ins are not distinguishable from any
//! other item leaving your inventory.
//!
//! What the log *does* tell us reliably is what you looted and what you said.
//! So the quest definitions are a hand-transcribed table, and progress is
//! derived by matching looted item names against it. That yields the question
//! actually worth answering mid-raid — *which quest can I turn in right now*
//! — without pretending to know more than the log supports.
//!
//! # Counting
//!
//! Counts are what you have looted minus what you have handed in. Turn-ins
//! *are* logged, as a run of offers closed by a trade:
//!
//! ```text
//! You offered 1 Efreeti Scimitar to Strandar Pinemist.
//! You offered 1 Storm Sky Opal to Strandar Pinemist.
//! You offered 1 Wind Rune Izah to Strandar Pinemist.
//! You complete the trade with Strandar Pinemist.
//! ```
//!
//! Offers only count once the trade closes — an offer with no completion is a
//! trade backed out of, and consuming those items would report you as poorer
//! than you are. A completed trade whose items satisfy one of the giver's
//! quests also records that quest as turned in.
//!
//! # What cannot be known
//!
//! The reward is not one of them. It arrives on the cursor with nothing
//! written to the log at all — the turn-in above produced only
//! `You gain experience!` and the trade closing. So a quest is reported as
//! *turned in*, never as *rewarded*: the completion count is the honest
//! measure, and a "rewards received" tally could only ever read zero and imply
//! the reward never came.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One item a quest requires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkyItem {
    pub name: String,
    /// Where it drops, as written on the quest table ("3-Gorga", "7-Trash").
    /// Free text: it is a note for the player, not something we match on.
    #[serde(default)]
    pub source: Option<String>,
}

/// One repeatable Plane of Sky quest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkyQuestDef {
    pub name: String,
    /// The class the quest belongs to, e.g. "Warrior".
    pub class: String,
    pub giver: String,
    /// The word said to the giver to start it.
    pub phrase: String,
    /// The Wind Rune it consumes. Required alongside `items`.
    pub rune: String,
    pub items: Vec<SkyItem>,
    pub reward: String,
}

impl SkyQuestDef {
    /// Everything the turn-in consumes: the rune plus the drops.
    fn required(&self) -> Vec<SkyItem> {
        let mut v = vec![SkyItem {
            name: self.rune.clone(),
            source: Some("rune".into()),
        }];
        v.extend(self.items.iter().cloned());
        v
    }
}

/// Progress against one required item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkyItemProgress {
    pub name: String,
    pub source: Option<String>,
    /// Times this item has been looted in the log.
    pub looted: u32,
    /// Times it has been handed to an NPC.
    pub turned_in: u32,
    /// Looted minus handed in — what you should actually be holding.
    pub held: u32,
    pub have: bool,
}

/// A quest with its progress filled in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkyQuestProgress {
    pub name: String,
    pub class: String,
    pub giver: String,
    pub phrase: String,
    pub reward: String,
    pub items: Vec<SkyItemProgress>,
    pub have_count: usize,
    pub need_count: usize,
    /// Every required item has been looted at least once.
    pub complete: bool,
    /// Last time the trigger phrase was said, if ever.
    ///
    /// Phrases are shared between classes, so this can be true for a quest you
    /// have never taken. Only meaningful alongside `relevant`.
    pub last_started: Option<i64>,
    /// Shown by default: either a class you play, or one you have progress on.
    pub relevant: bool,
    /// Belongs to a class you are playing right now.
    pub current_class: bool,
    /// Times this quest's requirements have been handed to its giver.
    pub completed: u32,
    pub last_completed: Option<i64>,
    /// What the giver has said, for writing the quest up later.
    pub giver_text: Vec<GiverLine>,
}

/// Everything the Sky view shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkyReport {
    pub quests: Vec<SkyQuestProgress>,
    /// Quests whose every requirement is met, counting only relevant ones.
    pub ready: usize,
    /// How many quests belong to the character's own classes.
    pub relevant_count: usize,
    /// Sky items looted that no defined quest asks for. Surfaced rather than
    /// dropped: an unrecognised drop usually means a quest we have not
    /// transcribed yet.
    pub unmatched_runes: Vec<String>,
}

/// A line a quest giver said.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GiverLine {
    pub at: i64,
    pub text: String,
    /// `[bracketed keywords]` offered — the branch points of the conversation.
    pub keywords: Vec<String>,
}

/// Tracks Sky quest progress from loot and say-channel chat.
#[derive(Debug, Default)]
pub struct SkyTracker {
    defs: Vec<SkyQuestDef>,
    /// Lowercased item name to times looted.
    looted: HashMap<String, u32>,
    /// Lowercased phrase to when it was last said.
    said: HashMap<String, i64>,
    /// Lowercased item name to how many have been handed over.
    turned_in: HashMap<String, u32>,
    /// Offers awaiting the line that closes the trade.
    pending_offers: Vec<(String, u64)>,
    /// Quest name to (times completed, last completion).
    completions: HashMap<String, (u32, i64)>,
    /// Lowercased giver name to what they have told you.
    ///
    /// The quest text is the part a written guide needs, and it exists only
    /// while you are standing in front of the NPC — hailing them is the only
    /// way it ever reaches the log.
    giver_text: HashMap<String, Vec<GiverLine>>,
}

impl SkyTracker {
    pub fn new() -> Self {
        let mut t = Self::default();
        t.defs = seed_quests();
        t
    }

    /// Replace the catalogue. Used when the caller loads it from disk.
    pub fn set_definitions(&mut self, defs: Vec<SkyQuestDef>) {
        self.defs = defs;
    }

    /// Add or replace one quest, keyed by name.
    pub fn define(&mut self, def: SkyQuestDef) {
        match self.defs.iter_mut().find(|d| d.name == def.name) {
            Some(existing) => *existing = def,
            None => self.defs.push(def),
        }
    }

    pub fn definitions(&self) -> &[SkyQuestDef] {
        &self.defs
    }

    /// Note an item entering your inventory.
    pub fn on_loot(&mut self, item: &str, quantity: u64) {
        *self.looted.entry(item.to_lowercase()).or_insert(0) += quantity.max(1) as u32;
    }

    /// Note an item offered to an NPC.
    ///
    /// Held rather than consumed: the trade may still be cancelled, and
    /// deducting an item that never left your bags would under-report what you
    /// are holding.
    pub fn on_offer(&mut self, item: &str, quantity: u64) {
        self.pending_offers
            .push((item.to_lowercase(), quantity.max(1)));
    }

    /// Note that a trade closed, consuming everything offered into it.
    ///
    /// If the items satisfy one of `npc`'s quests, that quest is recorded as
    /// turned in — matching on the giver as well as the items, so handing the
    /// same rune to a different class's NPC cannot be read as your completion.
    pub fn on_trade_complete(&mut self, timestamp: i64, npc: &str) {
        let offered: Vec<(String, u64)> = std::mem::take(&mut self.pending_offers);
        if offered.is_empty() {
            return;
        }
        for (item, qty) in &offered {
            *self.turned_in.entry(item.clone()).or_insert(0) += *qty as u32;
        }

        let names: std::collections::BTreeSet<&str> =
            offered.iter().map(|(i, _)| i.as_str()).collect();
        let matched = self.defs.iter().find(|d| {
            d.giver.eq_ignore_ascii_case(npc)
                && d.required()
                    .iter()
                    .all(|i| names.contains(i.name.to_lowercase().as_str()))
        });
        if let Some(d) = matched {
            let e = self.completions.entry(d.name.clone()).or_insert((0, 0));
            e.0 += 1;
            e.1 = timestamp;
        }
    }

    /// Note a line spoken by an NPC.
    ///
    /// Kept only for known quest givers: everything else on the say channel is
    /// other players and passing NPCs, and storing it would bury the text that
    /// matters. Repeats are dropped, so re-hailing does not duplicate.
    pub fn on_giver_line(&mut self, timestamp: i64, speaker: &str, text: &str) {
        let key = speaker.to_lowercase();
        if !self.defs.iter().any(|d| d.giver.eq_ignore_ascii_case(speaker)) {
            return;
        }
        let lines = self.giver_text.entry(key).or_default();
        if lines.iter().any(|l| l.text == text) {
            return;
        }
        lines.push(GiverLine {
            at: timestamp,
            text: text.to_string(),
            keywords: extract_sky_keywords(text),
        });
    }

    /// Everything a giver has told you, oldest first.
    pub fn giver_lines(&self, giver: &str) -> Vec<GiverLine> {
        self.giver_text
            .get(&giver.to_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    /// Note something you said, which may be a quest trigger phrase.
    ///
    /// Matched as the whole utterance rather than a substring: the phrases are
    /// bare words like "skill" and "force" that appear constantly in ordinary
    /// chat, and a substring match would claim you had started every quest.
    pub fn on_say(&mut self, timestamp: i64, text: &str) {
        let t = text.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
        self.said.insert(t, timestamp);
    }

    /// Progress for every quest, flagged against the character's own classes.
    ///
    /// `active_classes` are full class names ("Warrior"). An empty slice marks
    /// everything relevant, which is the honest default before the class
    /// detector has an opinion.
    pub fn report(&self, active_classes: &[String]) -> SkyReport {
        let active: std::collections::BTreeSet<String> =
            active_classes.iter().map(|c| c.to_lowercase()).collect();
        let mut quests: Vec<SkyQuestProgress> = self
            .defs
            .iter()
            .map(|d| {
                let items: Vec<SkyItemProgress> = d
                    .required()
                    .into_iter()
                    .map(|i| {
                        let key = i.name.to_lowercase();
                        let looted = *self.looted.get(&key).unwrap_or(&0);
                        let turned_in = *self.turned_in.get(&key).unwrap_or(&0);
                        let held = looted.saturating_sub(turned_in);
                        SkyItemProgress {
                            name: i.name,
                            source: i.source,
                            looted,
                            turned_in,
                            held,
                            have: held > 0,
                        }
                    })
                    .collect();
                let have_count = items.iter().filter(|i| i.have).count();
                let completed = self.completions.get(&d.name).map(|c| c.0).unwrap_or(0);
                // Work already done keeps a quest visible after a respec.
                // Hiding it because the class is no longer current would drop
                // half-collected sets and finished turn-ins out of the view
                // the moment you swapped, which is exactly when you would want
                // to know what you were holding for them.
                let mine = active.is_empty() || active.contains(&d.class.to_lowercase());
                SkyQuestProgress {
                    relevant: mine || have_count > 0 || completed > 0,
                    current_class: mine,
                    complete: have_count == items.len(),
                    need_count: items.len() - have_count,
                    have_count,
                    last_started: self.said.get(&d.phrase.to_lowercase()).copied(),
                    completed,
                    last_completed: self.completions.get(&d.name).map(|c| c.1),
                    giver_text: self
                        .giver_text
                        .get(&d.giver.to_lowercase())
                        .cloned()
                        .unwrap_or_default(),
                    name: d.name.clone(),
                    class: d.class.clone(),
                    giver: d.giver.clone(),
                    phrase: d.phrase.clone(),
                    reward: d.reward.clone(),
                    items,
                }
            })
            .collect();

        // Your own classes first, then closest to completion — fewest items
        // still missing.
        //
        // Ranked by what is *left*, not by what is held: the question this
        // list answers is "what can I finish soonest". Ties break toward the
        // quest with more already collected, which is the one more work has
        // gone into.
        quests.sort_by(|a, b| {
            b.relevant
                .cmp(&a.relevant)
                .then(b.complete.cmp(&a.complete))
                .then(a.need_count.cmp(&b.need_count))
                .then(b.have_count.cmp(&a.have_count))
                .then(a.name.cmp(&b.name))
        });

        let known: std::collections::BTreeSet<String> = self
            .defs
            .iter()
            .flat_map(|d| d.required())
            .map(|i| i.name.to_lowercase())
            .collect();
        let unmatched_runes: Vec<String> = self
            .looted
            .keys()
            .filter(|k| k.contains("wind rune") && !known.contains(*k))
            .cloned()
            .collect();

        SkyReport {
            ready: quests.iter().filter(|q| q.complete && q.relevant).count(),
            relevant_count: quests.iter().filter(|q| q.relevant).count(),
            quests,
            unmatched_runes,
        }
    }
}

/// Pull `[bracketed keywords]` out of a giver's line.
fn extract_sky_keywords(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let kw = after[..close].trim();
        if !kw.is_empty() {
            out.push(kw.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

/// The transcribed quest tables, shipped so a fresh install is useful.
///
/// Every class, as written on the quest tables. Item `source` strings are
/// the island shorthand from those tables (`3-Gorga`, `7-SotS`, `8-EoV`) and
/// are notes for the player, never matched against anything.
///
/// Trigger phrases are not unique across classes — Warrior and Cleric both
/// answer to `skill`, Bard and Beastlord both to `wind` — so a phrase marks
/// every quest that uses it as started. Filtering to your own classes is
/// what keeps that from being misleading.
pub fn seed_quests() -> Vec<SkyQuestDef> {
    fn q(
        class: &str,
        giver: &str,
        name: &str,
        phrase: &str,
        rune: &str,
        items: &[(&str, &str)],
        reward: &str,
    ) -> SkyQuestDef {
        SkyQuestDef {
            name: name.to_string(),
            class: class.to_string(),
            giver: giver.to_string(),
            phrase: phrase.to_string(),
            rune: rune.to_string(),
            items: items
                .iter()
                .map(|(n, s)| SkyItem {
                    name: n.to_string(),
                    source: (!s.is_empty()).then(|| s.to_string()),
                })
                .collect(),
            reward: reward.to_string(),
        }
    }

    vec![
        // ---- Warrior (Torgon Blademaster) ----
        q("Warrior", "Torgon Blademaster", "Warrior Test of Skill", "skill", "Wind Rune Neza",
            &[("Azure Ring", "3-Gorga")], "Azure Ruby Ring"),
        q("Warrior", "Torgon Blademaster", "Warrior Test of Strength", "strength", "Wind Rune Azia",
            &[("Stone Amulet", "4-KoS")], "Runed Wind Amulet"),
        q("Warrior", "Torgon Blademaster", "Warrior Test of Force", "force", "Wind Rune Beza",
            &[("Spiroc Air Totem", "5-SL")], "Pauldrons of the Blue Sky"),
        q("Warrior", "Torgon Blademaster", "Warrior Test of Think", "think", "Wind Rune Fana",
            &[("Wind Tablet", "6-BZ"), ("Efreeti Belt", "")], "Belt of the Four Winds"),
        q("Warrior", "Torgon Blademaster", "Warrior Test of Smash", "smash", "Wind Rune Jaka",
            &[("Djinni War Blade", "7-SotS"), ("Gem of Invigoration", "7-Trash")], "Dagas"),
        q("Warrior", "Torgon Blademaster", "Warrior Test of Bash", "bash", "Wind Rune Dena",
            &[("Ethereal Emerald", "8-EoV"), ("Efreeti Battle Axe", "")], "Fangol"),
        // ---- Wizard (Wizard Schrock) ----
        q("Wizard", "Wizard Schrock", "Wizard Test of Concentration", "concentration", "Wind Rune Dena",
            &[("Grey Damask Cloak", "3-Gorga")], "Augmentor's Mask"),
        q("Wizard", "Wizard Schrock", "Wizard Test of Focus", "focus", "Wind Rune Fana",
            &[("Woven Skull Cap", "4-KoS")], "Al`Kabor's Cap of Binding"),
        q("Wizard", "Wizard Schrock", "Wizard Test of Meditation", "meditation", "Wind Rune Geza",
            &[("High Quality Raiment", "5-SL")], "Raiment of Thunder"),
        q("Wizard", "Wizard Schrock", "Wizard Test of Conception", "conception", "Wind Rune Izah",
            &[("Box of Winds", "6-BZ"), ("Efreeti Statuette", "")], "Solidate Mithril Ring"),
        q("Wizard", "Wizard Schrock", "Wizard Test of Visualization", "visualization", "Wind Rune Jaka",
            &[("Amethyst Amulet", "7-SotS")], "Amulet of the Void"),
        q("Wizard", "Wizard Schrock", "Wizard Test of Preparation", "preparation", "Wind Rune Caza",
            &[("Large Sky Lapis", "8-EoV"), ("Efreeti War Staff", "")], "Nargon's Staff"),
        // ---- Bard (Cilin Spellsinger) ----
        q("Bard", "Cilin Spellsinger", "Bard Test of Tone", "tone", "Wind Rune Meda",
            &[("Light Woolen Mask", "3-Gorga")], "Mask of Song"),
        q("Bard", "Cilin Spellsinger", "Bard Test of Voice", "voice", "Wind Rune Kala",
            &[("Light Woolen Mantle", "4-KoS")], "Mantle of the Songweaver"),
        q("Bard", "Cilin Spellsinger", "Bard Test of Pitch", "pitch", "Wind Rune Azia",
            &[("Crude Wooden Flute", "5-SL")], "Ervaj's Flute of Flight"),
        q("Bard", "Cilin Spellsinger", "Bard Test of Wind", "wind", "Wind Rune Caza",
            &[("Amulet of Woven Hair", "6-BZ")], "Fae Amulet"),
        q("Bard", "Cilin Spellsinger", "Bard Test of Brass", "brass", "Wind Rune Fana",
            &[("Glowing Diamond", "7-SotS"), ("Efreeti War Horn", "")], "Denon's Horn of Disaster"),
        q("Bard", "Cilin Spellsinger", "Bard Test of Harmony", "harmony", "Wind Rune Heda",
            &[("Nebulous Diamond", "8-EoV"), ("Efreeti War Spear", "")], "Harmonic Spear"),
        // ---- Beastlord (Animist Kratho) ----
        q("Beastlord", "Animist Kratho", "Beastlord Test of Aviak", "aviak", "Wind Rune Beza",
            &[("Spiroc Elder's Totem", "5-SL")], "Spiroc Beak Earcuff"),
        q("Beastlord", "Animist Kratho", "Beastlord Test of Azarack", "azarack", "Wind Rune Heda",
            &[("Azarack Skin", "2-PoS")], "Azarack Skin Wristwraps"),
        q("Beastlord", "Animist Kratho", "Beastlord Test of Claw", "claw", "Wind Rune Izah",
            &[("Sphinx Claw", "7-SotS"), ("Mithril Bands", "8-EoV"), ("Brass Knuckles", "")], "Windhowl, Spirit Render"),
        q("Beastlord", "Animist Kratho", "Beastlord Test of Harpy", "harpy", "Wind Rune Kala",
            &[("Leather Cord", "3-Gorga")], "Griffin-Hide Armguards"),
        q("Beastlord", "Animist Kratho", "Beastlord Test of Wind", "wind", "Wind Rune Geza",
            &[("Silken Wrap", "6-BZ")], "Diaphonous Waistband"),
        // ---- Berserker (Stragen The Hewer) ----
        q("Berserker", "Stragen The Hewer", "Berserker Test of Sharpness", "sharpness", "Wind Rune Jaka",
            &[("Djinni War Blade", "7-SotS"), ("Efreeti Standard", "")], "Skycleaver"),
        q("Berserker", "Stragen The Hewer", "Berserker Test of Will", "will", "Wind Rune Ena",
            &[("Pulsating Ruby", "6-BZ")], "Molten Coil"),
        q("Berserker", "Stragen The Hewer", "Berserker Test of Ferocity", "ferocity", "Wind Rune Ozah",
            &[("High Quality Raiment", "5-SL")], "Sash of Ferocity"),
        q("Berserker", "Stragen The Hewer", "Berserker Test of Burden", "burden", "Wind Rune Azia",
            &[("Feathered Cape", "3-Gorga")], "Shroud of the Sky"),
        q("Berserker", "Stragen The Hewer", "Berserker Test of Blood", "blood", "Wind Rune Lena",
            &[("Azarack Blood", "2-PoS")], "Blood-Drawn Runes"),
        q("Berserker", "Stragen The Hewer", "Berserker Test of Fools Errand", "fools errand", "Wind Rune Dena",
            &[("Jester's Mask", "4-KoS"), ("Efreeti Great Staff", "")], "Cudgel of the Fool"),
        // ---- Cleric (Josin Faithbringer) ----
        q("Cleric", "Josin Faithbringer", "Cleric Test of Courage", "courage", "Wind Rune Lena",
            &[("Silver Hoop", "3-Gorga")], "Truewind Earring"),
        q("Cleric", "Josin Faithbringer", "Cleric Test of Skill", "skill", "Wind Rune Meda",
            &[("Small Shield", "4-KoS")], "Aegis of the Wind"),
        q("Cleric", "Josin Faithbringer", "Cleric Test of Protection", "protection", "Wind Rune Caza",
            &[("Shiny Pauldrons", "5-SL")], "Pauldrons of Piety"),
        q("Cleric", "Josin Faithbringer", "Cleric Test of Resolution", "resolution", "Wind Rune Neza",
            &[("Silvered Spiroc Necklace", "6-BZ")], "Necklace of Resolution"),
        q("Cleric", "Josin Faithbringer", "Cleric Test of Theurgy", "theurgy", "Wind Rune Kala",
            &[("Djinni Aura", "7-SotS"), ("Efreeti Mace", "")], "Theurgist's Star"),
        q("Cleric", "Josin Faithbringer", "Cleric Test of The Weak", "weak", "Wind Rune Ena",
            &[("Mithril Bands", "8-EoV"), ("Efreeti Standard", "")], "Baton of the Sky"),
        // ---- Druid (Strandar Pinemist) ----
        q("Druid", "Strandar Pinemist", "Druid Test of Wolf", "wolf", "Wind Rune Meda",
            &[("Worn Leather Mask", "3-Gorga")], "Drake-Hide Mask"),
        q("Druid", "Strandar Pinemist", "Druid Test of Bear", "bear", "Wind Rune Kala",
            &[("Mantle of Woven Grass", "4-KoS")], "Nature Walker's Mantle"),
        q("Druid", "Strandar Pinemist", "Druid Test of Tree", "tree", "Wind Rune Azia",
            &[("Spiroc Battle Staff", "5-SL"), ("Efreeti Statuette", "")], "Shillelagh"),
        q("Druid", "Strandar Pinemist", "Druid Test of The Bee", "bee", "Wind Rune Dena",
            &[("Divine Honeycomb", "6-BZ")], "Honeycomb Belt"),
        q("Druid", "Strandar Pinemist", "Druid Test of Eagle", "eagle", "Wind Rune Ena",
            &[("Ethereal Ruby", "7-SotS"), ("Spiroc Elder's Totem", "5-SL")], "Spiroc Banisher Focus"),
        q("Druid", "Strandar Pinemist", "Druid Test of Nature", "nature", "Wind Rune Izah",
            &[("Storm Sky Opal", "8-EoV"), ("Efreeti Scimitar", "")], "Espri"),
        // ---- Enchanter (Enchanter Jolas) ----
        q("Enchanter", "Enchanter Jolas", "Enchanter Test of Illusion", "illusion", "Wind Rune Meda",
            &[("Finely Woven Cloth Cord", "3-Gorga")], "Sphinx Hair Cord"),
        q("Enchanter", "Enchanter Jolas", "Enchanter Test of Metamorphism", "metamorphism", "Wind Rune Ozah",
            &[("Light Cloth Mantle", "4-KoS")], "Wind Walker's Mantle"),
        q("Enchanter", "Enchanter Jolas", "Enchanter Test of Deception", "deception", "Wind Rune Beza",
            &[("Silken Mask", "5-SL")], "Ivory Mask"),
        q("Enchanter", "Enchanter Jolas", "Enchanter Test of Disillusion", "disillusion", "Wind Rune Caza",
            &[("Adamantium Earring", "6-BZ")], "Earring of Displacement"),
        q("Enchanter", "Enchanter Jolas", "Enchanter Test of Memorization", "memorization", "Wind Rune Fana",
            &[("Glowing Necklace", "7-SotS")], "Necklace of Whispering Winds"),
        q("Enchanter", "Enchanter Jolas", "Enchanter Test of Incapacitation", "incapacitation", "Wind Rune Izah",
            &[("Large Sky Sapphire", "8-EoV"), ("Efreeti Wind Staff", "")], "Rod of the Protecting Winds"),
        // ---- Magician (Magus Frinon) ----
        q("Magician", "Magus Frinon", "Magician Test of Clarification", "clarification", "Wind Rune Lena",
            &[("Feathered Cape", "3-Gorga")], "Bracelet of Clarification"),
        q("Magician", "Magus Frinon", "Magician Test of Empowerment", "empowerment", "Wind Rune Neza",
            &[("Ceramic Mask", "4-KoS")], "Mask of Empowerment"),
        q("Magician", "Magus Frinon", "Magician Test of Shielding", "shielding", "Wind Rune Azia",
            &[("Golden Coffer", "5-SL")], "Gold White Pendant"),
        q("Magician", "Magus Frinon", "Magician Test of Summoning", "summoning", "Wind Rune Dena",
            &[("Large Diamond", "6-BZ")], "Drake-Hide Amice"),
        q("Magician", "Magus Frinon", "Magician Test of Interpretation", "interpretation", "Wind Rune Ena",
            &[("Golden Efreeti Ring", "7-SotS")], "Duennan Shielding Ring"),
        q("Magician", "Magus Frinon", "Magician Test of Gesticulation", "gesticulation", "Wind Rune Jaka",
            &[("Hazy Opal", "8-EoV"), ("Efreeti Magi Staff", "")], "Staff of The Magister"),
        q("Magician", "Magus Frinon", "Magician Test of Displacement", "displacement", "Wind Rune Heda",
            &[("Crown Of Elemental Mastery", "7-Trash"), ("Large Opal", "8-EoV"), ("Djinni Stave", "7-SotS")], "Staff of Elemental Mastery: Air"),
        // ---- Monk (Holwin) ----
        q("Monk", "Holwin", "Monk Test of Strength", "strength", "Wind Rune Caza",
            &[("Silken Strands", "3-Gorga")], "Back Straps of Mastery"),
        q("Monk", "Holwin", "Monk Test of Sight", "sight", "Wind Rune Geza",
            &[("Cracked Leather Eyepatch", "4-KoS")], "Ton Po's Eye Patch"),
        q("Monk", "Holwin", "Monk Test of Speed", "speed", "Wind Rune Jaka",
            &[("Dove Slippers", "5-SL")], "Sandals of Alacrity"),
        q("Monk", "Holwin", "Monk Test of Tears", "tears", "Wind Rune Beza",
            &[("Silken Wrap", "6-BZ")], "Ton Po's Shoulder Wraps"),
        q("Monk", "Holwin", "Monk Test of Fists", "fists", "Wind Rune Neza",
            &[("Nebulous Sapphire", "7-SotS"), ("Brass Knuckles", "")], "Wu's Fist of Mastery"),
        q("Monk", "Holwin", "Monk Test of Tranquility", "tranquility", "Wind Rune Lena",
            &[("Tear of Quellious", "8-EoV")], "Golden Sash of Tranquility"),
        // ---- Necromancer (Drakis Bloodcaster) ----
        q("Necromancer", "Drakis Bloodcaster", "Necromancer Test of Flight", "flight", "Wind Rune Lena",
            &[("Griffon's Beak", "3-Gorga")], "Bloody Griffon-Hide Wrist Guard"),
        q("Necromancer", "Drakis Bloodcaster", "Necromancer Test of Power", "power", "Wind Rune Neza",
            &[("Black Silk Cape", "4-KoS")], "Cloak of Spiroc Feathers"),
        q("Necromancer", "Drakis Bloodcaster", "Necromancer Test of Mind", "mind", "Wind Rune Ozah",
            &[("Fine Cloth Raiment", "5-SL")], "Bloodsoaked Raiment"),
        q("Necromancer", "Drakis Bloodcaster", "Necromancer Test of Heart", "heart", "Wind Rune Azia",
            &[("Pulsating Ruby", "6-BZ")], "Sphinx Heart Amulet"),
        q("Necromancer", "Drakis Bloodcaster", "Necromancer Test of Finger", "finger", "Wind Rune Caza",
            &[("Ring of Veeshan", "7-SotS")], "Band of Wailing Winds"),
        q("Necromancer", "Drakis Bloodcaster", "Necromancer Test of Hands", "hands", "Wind Rune Fana",
            &[("Gorgon Head", "3-Gorga"), ("Efreeti Great Staff", "")], "Gorgon Head Staff"),
        // ---- Paladin (Dason Goldblade) ----
        q("Paladin", "Dason Goldblade", "Paladin Test of Spirit", "spirit", "Wind Rune Lena",
            &[("Ivory Sky Diamond", "5-SL")], "Girdle of Faith"),
        q("Paladin", "Dason Goldblade", "Paladin Test of Sacrifice", "sacrifice", "Wind Rune Ozah",
            &[("Bixie Sword Blade", "6-BZ")], "Aldryn, Blade of the Ocean"),
        q("Paladin", "Dason Goldblade", "Paladin Test of Love", "love", "Wind Rune Geza",
            &[("Golden Hilt", "7"), ("Sphinx Claw", "7-SotS")], "Thelvorn, Blade of Light"),
        q("Paladin", "Dason Goldblade", "Paladin Test of Compassion", "compassion", "Wind Rune Izah",
            &[("Large Sky Diamond", "8-EoV"), ("Efreeti Zweihander", "")], "Truvinan"),
        // ---- Ranger (Ranger Spirit) ----
        q("Ranger", "Ranger Spirit", "Ranger Test of Body", "body", "Wind Rune Meda",
            &[("Griffon Talon", "3-Gorga")], "Griffon Talon Necklace"),
        q("Ranger", "Ranger Spirit", "Ranger Test of Defense", "defense", "Wind Rune Neza",
            &[("Fine Velvet Cloak", "4-KoS")], "Dark Cloak of the Sky"),
        q("Ranger", "Ranger Spirit", "Ranger Test of The Earth", "elemental earth", "Wind Rune Kala",
            &[("Spiroc Earth Totem", "5-SL")], "Earthshaker's Mantle"),
        q("Ranger", "Ranger Spirit", "Ranger Test of Thunder", "elemental thunder", "Wind Rune Azia",
            &[("White Gold Earring", "6-BZ")], "Thunderforged Earring"),
        q("Ranger", "Ranger Spirit", "Ranger Test of Blade", "blade", "Wind Rune Ena",
            &[("Circlet of Brambles", "7-SotS"), ("Efreeti Long Sword", "")], "Arydryidriyorn"),
        q("Ranger", "Ranger Spirit", "Ranger Test of Ranged Attack", "ranged attack", "Wind Rune Heda",
            &[("Shimmering Pearl", "8-EoV"), ("Efreeti War Bow", "")], "Windstriker"),
        // ---- Rogue (Thalik Silenthand) ----
        q("Rogue", "Thalik Silenthand", "Rogue Test of Thievery", "thievery", "Wind Rune Ozah",
            &[("Inlaid Choker", "3-Gorga")], "Wispy Choker of Vigor"),
        q("Rogue", "Thalik Silenthand", "Rogue Test of Trickery", "trickery", "Wind Rune Izah",
            &[("Sphinxian Circlet", "7-SotS")], "Renard's Belt of Quickness"),
        q("Rogue", "Thalik Silenthand", "Rogue Test of Silence", "silence", "Wind Rune Ena",
            &[("Spiroc Sky Totem", "5-SL")], "Griffon Wing Spauldors"),
        q("Rogue", "Thalik Silenthand", "Rogue Test of Cunning", "cunning", "Wind Rune Dena",
            &[("Jester's Mask", "4-KoS")], "Crystal Mask"),
        q("Rogue", "Thalik Silenthand", "Rogue Test of Stealth", "stealth", "Wind Rune Geza",
            &[("Fine Wool Cloak", "6-BZ")], "Scintillating Bracer of Protection"),
        q("Rogue", "Thalik Silenthand", "Rogue Test of Deception", "deception", "Wind Rune Jaka",
            &[("Bixie Stinger", "6-BZ"), ("Bloodsky Sapphire", "8-EoV")], "Thornstinger"),
        // ---- Shadow Knight (Sarkis Ebonblade) ----
        q("Shadow Knight", "Sarkis Ebonblade", "Shadow Knight Test of Bash", "bash", "Wind Rune Ozah",
            &[("Finely Crafted Amulet", "3-Gorga")], "Amulet of the Sphinx Eye"),
        q("Shadow Knight", "Sarkis Ebonblade", "Shadow Knight Test of Smash", "smash", "Wind Rune Beza",
            &[("Silvery Ring", "4-KoS")], "Crimson Ring of the Djinni"),
        q("Shadow Knight", "Sarkis Ebonblade", "Shadow Knight Test of Slash", "slash", "Wind Rune Dena",
            &[("Finely Woven Cloth Belt", "5-SL")], "Pegasus-Hide Belt"),
        q("Shadow Knight", "Sarkis Ebonblade", "Shadow Knight Test of Disempowerment", "disempowerment", "Wind Rune Fana",
            &[("Rusted Pauldrons", "6-BZ")], "Blood Sky Face Plate"),
        q("Shadow Knight", "Sarkis Ebonblade", "Shadow Knight Test of Envenoming", "envenoming", "Wind Rune Heda",
            &[("Efreeti War Shield", "")], "Obtenebrate Mithril Guard"),
        q("Shadow Knight", "Sarkis Ebonblade", "Shadow Knight Raising of the Dead", "raising of the dead", "Wind Rune Izah",
            &[("Sphinxian Ring", "7-SotS"), ("Fae Pauldrons", "8-EoV")], "Pearlescent Pauldrons"),
        q("Shadow Knight", "Sarkis Ebonblade", "Shadow Knight Test of Necropotence", "necropotence", "Wind Rune Kala",
            &[("Blood Sky Ruby", "8-EoV"), ("Efreeti War Axe", "")], "Khyldorn the Blood Drinker"),
        // ---- Shaman (Medicine Man Veetra) ----
        q("Shaman", "Medicine Man Veetra", "Shaman Test of Might", "might", "Wind Rune Meda",
            &[("Leather Cord", "3-Gorga")], "Amulet of the Fang"),
        q("Shaman", "Medicine Man Veetra", "Shaman Test of Health", "health", "Wind Rune Kala",
            &[("Ceremonial Belt", "5-SL")], "Bracelet of the Spirits"),
        q("Shaman", "Medicine Man Veetra", "Shaman Test of Sight", "sight", "Wind Rune Beza",
            &[("Light Damask Mantle", "5-SL")], "Fairy-Hide Mantle"),
        q("Shaman", "Medicine Man Veetra", "Shaman Test of Shrink", "shrink", "Wind Rune Ena",
            &[("Corrosive Venom", "6-BZ"), ("Efreeti War Club", "")], "Warhammer of the Wind"),
        q("Shaman", "Medicine Man Veetra", "Shaman Test of Snake", "snake", "Wind Rune Heda",
            &[("Bixie Essence", "6"), ("Spiritualist`s Ring", "7-SotS")], "Vermilion Sky Ring"),
        q("Shaman", "Medicine Man Veetra", "Shaman Test of The Witch Doctor", "witch doctor", "Wind Rune Geza",
            &[("Symbol of Veeshan", "8-EoV"), ("Efreeti War Maul", "")], "Garduk"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spot-checks across the transcription, including the rows most easily
    /// mangled: three-item quests, and quests whose only requirement is the
    /// rune plus one no-source item.
    #[test]
    fn the_seeded_table_matches_the_transcribed_source() {
        let q = seed_quests();
        assert_eq!(q.len(), 95, "every class transcribed");

        let by = |n: &str| q.iter().find(|d| d.name == n).unwrap().clone();

        let smash = by("Warrior Test of Smash");
        assert_eq!(smash.giver, "Torgon Blademaster");
        assert_eq!(smash.rune, "Wind Rune Jaka");
        assert_eq!(smash.reward, "Dagas");
        assert_eq!(smash.items.len(), 2);

        let claw = by("Beastlord Test of Claw");
        assert_eq!(claw.items.len(), 3);
        assert_eq!(claw.reward, "Windhowl, Spirit Render");
        // A listed item with no island shorthand keeps a None source.
        assert_eq!(claw.items[2].name, "Brass Knuckles");
        assert_eq!(claw.items[2].source, None);

        let disp = by("Magician Test of Displacement");
        assert_eq!(disp.items.len(), 3);
        assert_eq!(disp.reward, "Staff of Elemental Mastery: Air");

        // Shadow Knight's envenoming quest lists a single sourceless item.
        let env = by("Shadow Knight Test of Envenoming");
        assert_eq!(env.items.len(), 1);
        assert_eq!(env.items[0].name, "Efreeti War Shield");

        // Multi-word phrases survive transcription.
        assert_eq!(by("Ranger Test of The Earth").phrase, "elemental earth");
        assert_eq!(by("Berserker Test of Fools Errand").phrase, "fools errand");

        assert_eq!(
            q.iter().filter(|d| d.class == "Paladin").count(),
            4,
            "Paladin has four quests, not six"
        );
    }

    /// Swapping a class must not hide the sets you were part-way through
    /// collecting for it.
    #[test]
    fn a_quest_with_progress_stays_visible_after_a_respec() {
        let mut t = SkyTracker::new();
        t.on_loot("Wind Rune Meda", 1); // Druid Test of Wolf
        // Now playing Shaman, not Druid.
        let r = t.report(&["Shaman".to_string(), "Warrior".to_string()]);

        let wolf = r.quests.iter().find(|q| q.name == "Druid Test of Wolf").unwrap();
        assert!(wolf.relevant, "held items keep it in view");
        assert!(!wolf.current_class, "but it is not a class you play");

        // An untouched quest of a class you no longer play stays hidden.
        let bee = r.quests.iter().find(|q| q.name == "Druid Test of The Bee").unwrap();
        assert!(!bee.relevant);
    }

    /// A finished turn-in is history worth keeping in view too.
    #[test]
    fn a_completed_quest_stays_visible_after_a_respec() {
        let mut t = SkyTracker::new();
        for i in ["Wind Rune Izah", "Storm Sky Opal", "Efreeti Scimitar"] {
            t.on_loot(i, 1);
            t.on_offer(i, 1);
        }
        t.on_trade_complete(5000, "Strandar Pinemist");
        let r = t.report(&["Shaman".to_string()]);
        let nature = r.quests.iter().find(|q| q.name == "Druid Test of Nature").unwrap();
        assert_eq!(nature.have_count, 0, "items were handed over");
        assert!(nature.relevant, "completed quests stay visible");
        assert!(!nature.current_class);
    }

    /// A completed turn-in consumes the items and records the quest.
    #[test]
    fn a_completed_trade_consumes_items_and_records_the_quest() {
        let mut t = SkyTracker::new();
        for i in ["Wind Rune Izah", "Storm Sky Opal", "Efreeti Scimitar"] {
            t.on_loot(i, 1);
        }
        let me = ["Druid".to_string()];
        assert_eq!(t.report(&me).ready, 1, "ready before handing in");

        for i in ["Efreeti Scimitar", "Storm Sky Opal", "Wind Rune Izah"] {
            t.on_offer(i, 1);
        }
        t.on_trade_complete(5000, "Strandar Pinemist");

        let r = t.report(&me);
        let q = r
            .quests
            .iter()
            .find(|q| q.name == "Druid Test of Nature")
            .unwrap();
        assert_eq!(q.completed, 1);
        assert_eq!(q.last_completed, Some(5000));
        assert_eq!(q.have_count, 0, "the items were handed over");
        assert!(q.items.iter().all(|i| i.turned_in == 1 && i.held == 0));
        assert_eq!(r.ready, 0, "no longer ready — you gave them away");
    }

    /// A trade backed out of must not consume anything.
    #[test]
    fn offers_without_a_completed_trade_are_not_consumed() {
        let mut t = SkyTracker::new();
        t.on_loot("Wind Rune Izah", 1);
        t.on_offer("Wind Rune Izah", 1);
        // No completion line: the window was closed.
        let r = t.report(&["Druid".to_string()]);
        let rune = r
            .quests
            .iter()
            .flat_map(|q| q.items.iter())
            .find(|i| i.name == "Wind Rune Izah")
            .unwrap();
        assert_eq!(rune.held, 1, "still yours until the trade closes");
        assert_eq!(rune.turned_in, 0);
    }

    /// Handing a rune to another class's NPC is not your completion.
    #[test]
    fn a_trade_with_the_wrong_giver_completes_nothing() {
        let mut t = SkyTracker::new();
        for i in ["Wind Rune Izah", "Storm Sky Opal", "Efreeti Scimitar"] {
            t.on_loot(i, 1);
            t.on_offer(i, 1);
        }
        t.on_trade_complete(5000, "Torgon Blademaster");
        let r = t.report(&[]);
        assert!(r.quests.iter().all(|q| q.completed == 0));
        // The items still left your bags, whoever took them.
        let rune = r
            .quests
            .iter()
            .flat_map(|q| q.items.iter())
            .find(|i| i.name == "Wind Rune Izah")
            .unwrap();
        assert_eq!(rune.held, 0);
    }

    /// Different classes share trigger phrases; the class filter is what stops
    /// that being misread as progress on a quest you never took.
    #[test]
    fn quests_for_other_classes_are_marked_and_ranked_below_your_own() {
        let mut t = SkyTracker::new();
        t.on_say(100, "skill"); // Warrior *and* Cleric answer to this
        let r = t.report(&["Warrior".to_string()]);

        let war = r.quests.iter().find(|q| q.name == "Warrior Test of Skill").unwrap();
        let cle = r.quests.iter().find(|q| q.name == "Cleric Test of Skill").unwrap();
        assert!(war.relevant);
        assert!(!cle.relevant, "Cleric quest is not yours");
        assert!(war.last_started.is_some() && cle.last_started.is_some());

        assert_eq!(r.relevant_count, 6);
        assert!(r.quests[0].relevant, "your own classes lead the list");
    }

    #[test]
    fn with_no_known_classes_everything_is_relevant() {
        let t = SkyTracker::new();
        let r = t.report(&[]);
        assert_eq!(r.relevant_count, r.quests.len());
    }

    /// The rune is consumed alongside the drops, so it is a requirement even
    /// though the table lists it in its own column.
    #[test]
    fn the_rune_counts_as_a_required_item() {
        let t = SkyTracker::new();
        let r = t.report(&[]);
        let skill = r.quests.iter().find(|q| q.name == "Warrior Test of Skill").unwrap();
        assert_eq!(skill.items.len(), 2, "rune plus one drop");
        assert!(skill.items.iter().any(|i| i.name == "Wind Rune Neza"));
    }

    #[test]
    fn a_quest_is_ready_only_when_every_piece_is_looted() {
        let mut t = SkyTracker::new();
        let me = ["Warrior".to_string()];
        t.on_loot("Azure Ring", 1);
        assert_eq!(t.report(&me).ready, 0, "rune still missing");
        t.on_loot("wind rune neza", 1); // case-insensitive
        let r = t.report(&me);
        assert_eq!(r.ready, 1);
        assert_eq!(r.quests[0].name, "Warrior Test of Skill", "ready quest leads");
    }

    /// Ordered by what is still missing, so the quest you can finish soonest
    /// leads — even if a longer one has more pieces collected.
    #[test]
    fn quests_are_ordered_by_how_close_to_finished_they_are() {
        let mut t = SkyTracker::new();
        // Bard Test of Harmony: rune + 2 items, two of three held → 1 missing.
        t.on_loot("Wind Rune Heda", 1);
        t.on_loot("Nebulous Diamond", 1);
        // Bard Test of Pitch: rune + 1 item, rune only → 1 missing, shorter.
        t.on_loot("Wind Rune Azia", 1);
        // Bard Test of Brass: rune + 2 items, rune only → 2 missing.
        t.on_loot("Wind Rune Fana", 1);

        let r = t.report(&["Bard".to_string()]);
        let started: Vec<(&str, usize)> = r
            .quests
            .iter()
            .filter(|q| q.relevant && q.have_count > 0)
            .map(|q| (q.name.as_str(), q.need_count))
            .collect();
        // Ordered by what is left, fewest first, all the way down.
        assert!(
            started.windows(2).all(|w| w[0].1 <= w[1].1),
            "not ordered by need: {started:?}"
        );
        assert_eq!(started[0].1, 1, "a one-away quest must lead: {started:?}");
        // Among the one-away quests, the further-along one leads.
        assert_eq!(started[0].0, "Bard Test of Harmony");
    }

    #[test]
    fn multi_item_quests_report_what_is_still_needed() {
        let mut t = SkyTracker::new();
        t.on_loot("Wind Rune Jaka", 1);
        t.on_loot("Djinni War Blade", 1);
        let r = t.report(&[]);
        let smash = r.quests.iter().find(|q| q.name == "Warrior Test of Smash").unwrap();
        assert_eq!(smash.have_count, 2);
        assert_eq!(smash.need_count, 1);
        assert!(!smash.complete);
        let missing: Vec<&str> = smash
            .items
            .iter()
            .filter(|i| !i.have)
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(missing, vec!["Gem of Invigoration"]);
    }

    /// The phrases are bare words, so a substring match would fire constantly.
    #[test]
    fn only_the_exact_phrase_marks_a_quest_started() {
        let mut t = SkyTracker::new();
        t.on_say(500, "I have no skill at this");
        assert!(t.report(&[]).quests.iter().all(|q| q.last_started.is_none()));
        t.on_say(900, "Skill");
        let r = t.report(&[]);
        let skill = r.quests.iter().find(|q| q.name == "Warrior Test of Skill").unwrap();
        assert_eq!(skill.last_started, Some(900));
    }

    #[test]
    fn an_unrecognised_rune_is_surfaced_rather_than_dropped() {
        let mut t = SkyTracker::new();
        t.on_loot("Wind Rune Xyzzy", 1);
        assert_eq!(t.report(&[]).unmatched_runes, vec!["wind rune xyzzy"]);
    }

    /// Defining by name both adds new quests and corrects transcribed ones.
    #[test]
    fn definitions_can_be_added_or_corrected() {
        let mut t = SkyTracker::new();
        let before = t.definitions().len();

        // A name already in the table replaces that entry rather than
        // duplicating it — this is how a transcription error gets fixed.
        t.define(SkyQuestDef {
            name: "Druid Test of Nature".into(),
            class: "Druid".into(),
            giver: "Strandar Pinemist".into(),
            phrase: "nature".into(),
            rune: "Wind Rune Izah".into(),
            items: vec![],
            reward: "Espri".into(),
        });
        assert_eq!(t.definitions().len(), before, "corrected, not duplicated");

        // A new name is appended.
        t.define(SkyQuestDef {
            name: "Druid Test of Something New".into(),
            class: "Druid".into(),
            giver: "Strandar Pinemist".into(),
            phrase: "something".into(),
            rune: "Wind Rune Neza".into(),
            items: vec![],
            reward: "Something Green".into(),
        });
        assert_eq!(t.definitions().len(), before + 1);

        // The rune alone is enough when a quest lists no drops.
        t.on_loot("Wind Rune Neza", 1);
        let r = t.report(&[]);
        let d = r
            .quests
            .iter()
            .find(|q| q.name == "Druid Test of Something New")
            .unwrap();
        assert!(d.complete);
    }
}
