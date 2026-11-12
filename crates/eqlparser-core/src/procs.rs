//! Procs: effects that fire on their own rather than being cast on purpose.
//!
//! Two things in the log deserve the name, and they are found in quite
//! different ways.
//!
//! A **worn item proc** announces itself and its cause in one line — "Your
//! Platinum Armband (Exaltation) flickers with a pale light." — so it needs no
//! interpretation at all.
//!
//! A **weapon or buff proc** does not. It is written exactly like a spell you
//! cast, because as far as the client is concerned you did cast it: Spirit of
//! the Puma produces "You begin casting Puma Maw II" indistinguishably from a
//! nuke. What separates them is not the line but the spell file, which states
//! outright which spells are procs and what grants them. See
//! [`crate::spell::Spell::procs`].
//!
//! # Why casts and not damage
//!
//! The cast line is counted, never the damage. Measured over a real log:
//!
//! * Earthquake, an area proc, wrote 21 cast lines and 2,825 damage lines —
//!   one per creature caught in each blast. Counting damage would report a
//!   proc rate over a hundred times the truth.
//! * Blade Dance wrote 664 cast lines and no damage at all, being a proc that
//!   buffs rather than hurts. Counting damage would not see it exist.
//! * Puma Maw wrote 2,705 casts against 2,573 damage lines, the gap being the
//!   ones that missed or were resisted — which still fired.
//!
//! Damage is still totalled per proc, because "how much did it actually do" is
//! the next question after "how often did it go off". It is just not the
//! count.

use crate::class::{Class, CLASS_CANNOT_CAST, CLASS_COUNT};
use crate::spell::{base_spell_name, SpellDb};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// How a proc reaches the log, which is also how it was identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcKind {
    /// Fires off a weapon swing. SPA 85 or 419.
    Melee,
    /// Fires off casting something else. SPA 340 or 383.
    Spell,
    /// A worn item's effect, named in its own log line.
    Item,
}

/// One proc the spell file knows about, before anything has been seen of it.
#[derive(Debug, Clone)]
struct Known {
    /// The name as the client writes it, rank stripped.
    name: String,
    kind: ProcKind,
    /// The spells that grant it. More than one is common: several ranks of a
    /// buff, or a buff and a weapon, can share a proc.
    sources: BTreeSet<String>,
    /// The level each class can cast this at, straight from the spell file.
    ///
    /// Kept because a handful of procs are also ordinary spells — Vampiric
    /// Embrace is both — and for a character who can cast one, a cast line
    /// genuinely is ambiguous. See [`ProcTracker::report`].
    class_levels: [u8; CLASS_COUNT],
}

/// Which spells are procs, and what grants them.
#[derive(Debug, Clone, Default)]
pub struct ProcRegistry {
    /// Keyed by lowercased base name, which is what the log can be matched on:
    /// the cast line carries the rank ("Puma Maw II") and the effect table
    /// carries the id of one particular rank.
    by_name: HashMap<String, Known>,
}

impl ProcRegistry {
    /// Read every proc the spell file declares.
    pub fn from_spells(db: &SpellDb) -> ProcRegistry {
        let mut by_name: HashMap<String, Known> = HashMap::new();
        for source in db.all() {
            for grant in &source.procs {
                let Some(proc) = db.get(grant.spell) else {
                    continue;
                };
                let key = base_spell_name(&proc.name).to_lowercase();
                if key.is_empty() {
                    continue;
                }
                // A proc reachable both ways is a melee proc first: that is
                // the one whose rate a player is trying to read.
                let kind = if grant.on_melee {
                    ProcKind::Melee
                } else {
                    ProcKind::Spell
                };
                let e = by_name.entry(key).or_insert_with(|| Known {
                    name: base_spell_name(&proc.name).to_string(),
                    kind,
                    sources: BTreeSet::new(),
                    class_levels: [CLASS_CANNOT_CAST; CLASS_COUNT],
                });
                if kind == ProcKind::Melee {
                    e.kind = ProcKind::Melee;
                }
                e.sources.insert(source.name.clone());
                // Take the most permissive level seen across ranks: if any
                // rank of this spell is castable, a cast line is ambiguous.
                for (slot, lv) in e.class_levels.iter_mut().zip(proc.class_levels) {
                    if lv != CLASS_CANNOT_CAST && (*slot == CLASS_CANNOT_CAST || lv < *slot) {
                        *slot = lv;
                    }
                }
            }
        }
        // A proc is counted from a cast line, and a cast line names only the
        // spell, so what matters is whether *anything* by that name is
        // castable, not just the proc's own ranks. Vampiric Embrace is a
        // shadowknight buff whose proc carries the same name: every time the
        // buff was cast it was counted as the proc firing (41 "procs" in the
        // Mirela log, all of them casts).
        for sp in db.all() {
            let Some(e) = by_name.get_mut(&base_spell_name(&sp.name).to_lowercase()) else { continue };
            for (slot, lv) in e.class_levels.iter_mut().zip(sp.class_levels) {
                if lv != CLASS_CANNOT_CAST && (*slot == CLASS_CANNOT_CAST || lv < *slot) {
                    *slot = lv;
                }
            }
        }
        ProcRegistry { by_name }
    }

    /// How many distinct procs are known. Zero means the spell file was not
    /// loaded, and only item procs can be seen.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// What has actually been seen of one proc.
#[derive(Debug, Clone, Default)]
struct Seen {
    count: u64,
    damage: u64,
    hits: u64,
    first: i64,
    last: i64,
}

impl Seen {
    fn note(&mut self, at: i64) {
        self.count += 1;
        if self.first == 0 {
            self.first = at;
        }
        self.last = at;
    }
}

/// Counts procs as they fire.
#[derive(Debug, Clone, Default)]
pub struct ProcTracker {
    registry: ProcRegistry,
    /// Keyed by lowercased base proc name.
    seen: HashMap<String, Seen>,
    /// Worn item procs, keyed by lowercased `item\u{1}effect` so one item's
    /// rate stays separate from another's sharing the same effect.
    items: HashMap<String, (String, String, Seen)>,
}

impl ProcTracker {
    pub fn new() -> ProcTracker {
        ProcTracker::default()
    }

    /// Install the proc tables. Until this is called only item procs are seen.
    pub fn set_registry(&mut self, registry: ProcRegistry) {
        self.registry = registry;
    }

    /// Whether the spell file was loaded, and so whether weapon and buff procs
    /// can be counted at all.
    pub fn has_spell_data(&self) -> bool {
        !self.registry.is_empty()
    }

    /// Note a cast by the logging character.
    ///
    /// Everything is offered here, procs and real spells alike; only the ones
    /// the spell file names as procs are kept.
    pub fn on_cast(&mut self, timestamp: i64, spell: &str) {
        let key = base_spell_name(spell).to_lowercase();
        if !self.registry.by_name.contains_key(&key) {
            return;
        }
        self.seen.entry(key).or_default().note(timestamp);
    }

    /// Note damage that landed under a proc's name.
    ///
    /// Recorded for its total only. One proc can produce many of these — an
    /// area proc writes one per creature — so it must never move the count.
    pub fn on_proc_damage(&mut self, ability: &str, amount: u64) {
        let key = base_spell_name(ability).to_lowercase();
        if !self.registry.by_name.contains_key(&key) {
            return;
        }
        let e = self.seen.entry(key).or_default();
        e.damage += amount;
        e.hits += 1;
    }

    /// Note a worn item's effect firing.
    pub fn on_item_proc(&mut self, timestamp: i64, item: &str, effect: &str) {
        let key = format!("{}\u{1}{}", item.to_lowercase(), effect.to_lowercase());
        self.items
            .entry(key)
            .or_insert_with(|| (item.to_string(), effect.to_string(), Seen::default()))
            .2
            .note(timestamp);
    }

    /// Everything seen, rated against time spent in combat.
    ///
    /// `loadout` is the character's own classes and their levels. A proc one of
    /// them can cast is set aside rather than counted: for such a spell the
    /// cast line is the same whether it fired on its own or the player pressed
    /// it, and guessing would inflate the rate of exactly the abilities a
    /// player is most likely to be using deliberately.
    pub fn report(
        &self,
        loadout: &[(Class, u32)],
        combat_seconds: i64,
        logged_seconds: i64,
    ) -> ProcReport {
        let per_min = |n: u64| {
            if combat_seconds <= 0 {
                0.0
            } else {
                n as f64 * 60.0 / combat_seconds as f64
            }
        };
        let castable = |levels: &[u8; CLASS_COUNT]| {
            loadout.iter().any(|(class, level)| {
                let need = levels[class.index()];
                need != CLASS_CANNOT_CAST && need as u32 <= *level
            })
        };

        let mut rows = Vec::new();
        let mut ambiguous = Vec::new();
        for (key, seen) in &self.seen {
            let Some(known) = self.registry.by_name.get(key) else {
                continue;
            };
            let row = ProcRow {
                name: known.name.clone(),
                kind: known.kind,
                sources: known.sources.iter().cloned().collect(),
                count: seen.count,
                per_minute: per_min(seen.count),
                damage: seen.damage,
                hits: seen.hits,
                first: seen.first,
                last: seen.last,
            };
            if castable(&known.class_levels) {
                ambiguous.push(row);
            } else {
                rows.push(row);
            }
        }
        for (item, effect, seen) in self.items.values() {
            rows.push(ProcRow {
                name: effect.clone(),
                kind: ProcKind::Item,
                sources: vec![item.clone()],
                count: seen.count,
                per_minute: per_min(seen.count),
                damage: seen.damage,
                hits: seen.hits,
                first: seen.first,
                last: seen.last,
            });
        }
        rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
        ambiguous.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));

        let total: u64 = rows.iter().map(|r| r.count).sum();
        ProcReport {
            total,
            per_minute: per_min(total),
            damage: rows.iter().map(|r| r.damage).sum(),
            combat_seconds,
            logged_seconds,
            have_spell_data: self.has_spell_data(),
            known_procs: self.registry.len(),
            rows,
            ambiguous,
        }
    }
}

/// One proc's tally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcRow {
    pub name: String,
    pub kind: ProcKind,
    /// What grants it: the buffs it comes from, or the item that carries it.
    pub sources: Vec<String>,
    /// Times it fired.
    pub count: u64,
    /// Times per minute of combat.
    pub per_minute: f64,
    /// Damage it has done, across every creature it hit.
    pub damage: u64,
    /// Damage lines it produced. Higher than `count` for an area proc, lower
    /// when it misses or does not damage at all.
    pub hits: u64,
    pub first: i64,
    pub last: i64,
}

/// Every proc seen, and the denominator its rates were built on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcReport {
    pub rows: Vec<ProcRow>,
    /// Procs that one of your own classes can also cast, so the log cannot say
    /// which happened. Reported separately and excluded from the totals.
    pub ambiguous: Vec<ProcRow>,
    pub total: u64,
    pub per_minute: f64,
    pub damage: u64,
    /// Seconds of combat, the denominator behind every rate here.
    pub combat_seconds: i64,
    /// Seconds the log covers, for contrast: the difference is time spent not
    /// fighting, which no proc rate should be charged for.
    pub logged_seconds: i64,
    /// False when the spell file was not loaded, in which case `rows` holds
    /// item procs only and says nothing about weapons or buffs.
    pub have_spell_data: bool,
    /// How many distinct procs the spell file described.
    pub known_procs: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spell::Spell;

    fn spell(id: u32, name: &str) -> Spell {
        Spell {
            id,
            name: name.into(),
            cast_time_ms: 0,
            recovery_ms: 0,
            recast_ms: 0,
            duration_formula: 0,
            duration_cap: 0,
            mana: 0,
            landed_on_you: None,
            landed_on_other: None,
            fades: None,
            restricted_to: None,
            class_levels: [CLASS_CANNOT_CAST; CLASS_COUNT],
            good_effect: 1,
            category_id: 1,
            is_charm: false,
            linked: Vec::new(),
            procs: Vec::new(),
        }
    }

    /// Spirit of the Puma grants Puma Maw, which is cast as `Puma Maw II`.
    fn db() -> SpellDb {
        let mut buff = spell(6906, "Spirit of the Puma");
        buff.procs = vec![crate::spell::ProcGrant { spell: 6908, on_melee: true }];
        let proc = spell(6908, "Puma Maw");
        let mut m = HashMap::new();
        m.insert(buff.id, buff);
        m.insert(proc.id, proc);
        SpellDb::from_spells(m)
    }

    fn tracker() -> ProcTracker {
        let mut t = ProcTracker::new();
        t.set_registry(ProcRegistry::from_spells(&db()));
        t
    }

    #[test]
    fn a_proc_is_counted_from_its_cast_line_and_named_by_its_source() {
        let mut t = tracker();
        t.on_cast(100, "Puma Maw II");
        t.on_cast(160, "Puma Maw II");
        // A real spell of the player's own is not a proc.
        t.on_cast(170, "Inferno Shock VI");

        let r = t.report(&[], 120, 3600);
        assert_eq!(r.rows.len(), 1, "only the proc");
        assert_eq!(r.rows[0].name, "Puma Maw", "named without its rank");
        assert_eq!(r.rows[0].sources, vec!["Spirit of the Puma"]);
        assert_eq!(r.total, 2);
        assert!((r.per_minute - 1.0).abs() < 1e-9, "2 procs in 2 minutes");
    }

    /// An area proc writes one damage line per creature. If those moved the
    /// count, its rate would read as many times the truth.
    #[test]
    fn damage_lines_add_damage_but_never_the_count() {
        let mut t = tracker();
        t.on_cast(100, "Puma Maw II");
        t.on_proc_damage("Puma Maw", 154);
        t.on_proc_damage("Puma Maw", 154);
        t.on_proc_damage("Puma Maw", 154);

        let r = t.report(&[], 60, 60);
        assert_eq!(r.rows[0].count, 1, "one proc");
        assert_eq!(r.rows[0].hits, 3, "three creatures caught by it");
        assert_eq!(r.rows[0].damage, 462);
    }

    /// Rates are per minute of combat, not per minute of the evening.
    #[test]
    fn the_rate_uses_combat_time_not_the_span_of_the_log() {
        let mut t = tracker();
        for i in 0..10 {
            t.on_cast(100 + i, "Puma Maw II");
        }
        let r = t.report(&[], 300, 36000);
        assert!((r.per_minute - 2.0).abs() < 1e-9, "10 procs in 5 minutes");
        assert_eq!(r.combat_seconds, 300);
        assert_eq!(r.logged_seconds, 36000, "the log's span is still reported");
    }

    /// The buff that grants a proc can share the proc's name, and casting the
    /// buff writes the same line a firing would. If you can cast the buff,
    /// the name is ambiguous even though the proc spell itself is not
    /// castable by anyone.
    #[test]
    fn a_castable_buff_named_like_its_proc_makes_the_proc_ambiguous() {
        let mut buff = spell(1, "Vampiric Embrace");
        buff.procs = vec![crate::spell::ProcGrant { spell: 2, on_melee: true }];
        buff.class_levels[Class::ShadowKnight.index()] = 17;
        let proc = spell(2, "Vampiric Embrace");
        let mut m = HashMap::new();
        m.insert(buff.id, buff);
        m.insert(proc.id, proc);

        let mut t = ProcTracker::new();
        t.set_registry(ProcRegistry::from_spells(&SpellDb::from_spells(m)));
        t.on_cast(100, "Vampiric Embrace");

        let r = t.report(&[(Class::ShadowKnight, 26)], 60, 60);
        assert!(r.rows.is_empty(), "a buff cast is not a proc firing");
        assert_eq!(r.ambiguous.len(), 1);
    }

    /// A proc the character can cast itself is set aside rather than guessed
    /// at: the cast line is identical either way.
    #[test]
    fn a_proc_the_character_can_cast_is_reported_apart() {
        let mut buff = spell(1, "Vampiric Embrace");
        buff.procs = vec![crate::spell::ProcGrant { spell: 2, on_melee: true }];
        let mut proc = spell(2, "Vampiric Embrace");
        proc.class_levels[Class::Necromancer.index()] = 34;
        let mut m = HashMap::new();
        m.insert(buff.id, buff);
        m.insert(proc.id, proc);

        let mut t = ProcTracker::new();
        t.set_registry(ProcRegistry::from_spells(&SpellDb::from_spells(m)));
        t.on_cast(100, "Vampiric Embrace");

        // A necromancer high enough to cast it: ambiguous.
        let r = t.report(&[(Class::Necromancer, 40)], 60, 60);
        assert!(r.rows.is_empty(), "not counted for someone who has it");
        assert_eq!(r.ambiguous.len(), 1);
        assert_eq!(r.total, 0);

        // Too low to have it yet, so nothing of theirs could have cast it.
        let r = t.report(&[(Class::Necromancer, 20)], 60, 60);
        assert_eq!(r.rows.len(), 1, "below the level that grants it");

        // A bard cannot cast it at all.
        let r = t.report(&[(Class::Bard, 60)], 60, 60);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.total, 1);
    }

    /// Item procs need no spell file: the line names both cause and effect.
    #[test]
    fn item_procs_are_counted_without_any_spell_data() {
        let mut t = ProcTracker::new();
        assert!(!t.has_spell_data());
        t.on_item_proc(100, "Platinum Armband", "Exaltation");
        t.on_item_proc(130, "Platinum Armband", "Exaltation");
        t.on_item_proc(160, "Kobold-Hide Boots", "Exaltation");

        let r = t.report(&[], 60, 60);
        assert!(!r.have_spell_data, "and the report says so");
        assert_eq!(r.rows.len(), 2, "one row per item, not per effect");
        assert_eq!(r.rows[0].name, "Exaltation");
        assert_eq!(r.rows[0].sources, vec!["Platinum Armband"]);
        assert_eq!(r.rows[0].count, 2);
        assert_eq!(r.total, 3);
    }
}
