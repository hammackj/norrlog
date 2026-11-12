//! Spell database and buff resolution.
//!
//! # Why this module exists
//!
//! The client never names the spell when an effect lands on you. It writes
//! flavour text — "Your feet move faster." — and nothing else. To show a buff
//! timer we must work backwards from that text to a spell, then compute how
//! long that spell lasts.
//!
//! Two data files shipped with the game make this possible:
//!
//! * `spells_us.txt` — 173 caret-separated fields per spell. We read the id,
//!   name, cast/recast times and, critically, the **duration formula** and
//!   **duration cap**.
//! * `spells_us_str.txt` — the display strings per spell, including the text
//!   shown when the effect lands on you and when it wears off.
//!
//! # The ambiguity problem
//!
//! Flavour text is *not* unique. In the shipped data ~54,000 spells share only
//! ~13,000 distinct landing strings; "Your wounds begin to heal." alone maps to
//! five different spells. So a text lookup yields a candidate *set*, and
//! [`SpellResolver`] scores that set using surrounding context — chiefly the
//! `You begin casting <name>.` line that immediately precedes a self-cast.

use crate::bestiary::CreatureKind;
use crate::class::{CLASS_CANNOT_CAST, CLASS_COUNT, CLASS_FIELD_BASE};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// One EverQuest tick, the unit spell durations are expressed in.
pub const TICK_SECONDS: i64 = 6;

/// A proc one spell grants, and how it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcGrant {
    /// The spell that goes off.
    pub spell: u32,
    /// True when it fires off a weapon swing, false when off a cast.
    pub on_melee: bool,
}

/// The subset of `spells_us.txt` we care about.
#[derive(Debug, Clone)]
pub struct Spell {
    pub id: u32,
    pub name: String,
    pub cast_time_ms: u32,
    pub recovery_ms: u32,
    pub recast_ms: u32,
    /// Selects the level-scaling rule; see [`duration_ticks`].
    pub duration_formula: u32,
    /// Upper bound on duration, in ticks. Most formulas scale with level and
    /// are then clamped to this value.
    pub duration_cap: u32,
    pub mana: u32,
    /// Text shown when the effect lands on you.
    pub landed_on_you: Option<String>,
    /// Text shown when the effect lands on someone else.
    pub landed_on_other: Option<String>,
    /// Text shown when the effect expires.
    pub fades: Option<String>,
    /// Level at which each of the 16 classes may cast this spell, indexed by
    /// [`Class::index`]. 255 means that class never can. Drives class
    /// inference; see [`crate::class`].
    pub class_levels: [u8; CLASS_COUNT],
    /// The game's own effect category; see [`Spell::category`].
    pub category_id: u8,
    /// Spells this one applies to you as a side effect.
    ///
    /// A single cast can grant several buffs. `Form of the Great Wolf` is the
    /// wolf illusion *and* `Illusion Benefit Dena`, the stat buff that comes
    /// with it — two entries in the game's buff window from one cast.
    ///
    /// This has to be read from the effect table because the linked spell has
    /// no landing text of its own: nothing is ever written to the log when it
    /// takes hold, so following the link is the only way it can be known
    /// about at all.
    pub linked: Vec<u32>,
    /// The body type this spell may only be cast at, if it is restricted.
    ///
    /// The game refuses the cast otherwise, so one of these landing proves what
    /// the target is. See [`crate::bestiary::CreatureKind`].
    pub restricted_to: Option<CreatureKind>,
    /// True when any effect slot is SPA 22, the charm effect.
    ///
    /// Needed because the landing text cannot be trusted on its own: eleven
    /// spells share `'s eyes glaze over.`, of which six are charms (SPA 22)
    /// and the rest are mez and damage effects. Reading a mez as a charm would
    /// credit an enemy's damage to you the moment it broke.
    pub is_charm: bool,
    /// The game's own beneficial flag: 0 detrimental, 1 beneficial,
    /// 2 beneficial-group-only.
    ///
    /// Verified against 31 known spells with no mismatches, so debuffs are
    /// classified from shipped data rather than guessed at from names.
    pub good_effect: u8,
    /// Spells this one fires on its own: the procs it grants.
    ///
    /// A buff that adds a weapon proc names the proc in its effect table, so
    /// which spells are procs and what granted them is shipped data rather
    /// than something to be inferred from how often a name appears. Spirit of
    /// the Puma carries `5|85|6908|400|100|0`, and 6908 is Puma Maw — exactly
    /// the name its damage lands under.
    pub procs: Vec<ProcGrant>,
}

/// Broad kind of effect, from the game's own categorisation.
///
/// Taken from field 146 of `spells_us.txt`. Verified against 24 known spells
/// with no overlap between categories, and spot-checked on random unseen
/// spells: category 5 returns Feral Restoration, Cascade of Life, Elixir of
/// Healing and similar, i.e. genuinely restorative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectCategory {
    /// Restores hit points, over time or at once.
    Heal,
    /// A general beneficial enhancement.
    Buff,
    /// Removes poison, disease and similar.
    Cure,
    /// Deals damage.
    Damage,
    /// Hinders its target without dealing damage.
    Detrimental,
    Summon,
    Teleport,
    Other,
}

impl Spell {
    /// Broad effect kind, used to colour the UI by what a buff actually does.
    pub fn category(&self) -> EffectCategory {
        match self.category_id {
            1 => EffectCategory::Buff,
            2 => EffectCategory::Cure,
            3 => EffectCategory::Damage,
            4 => EffectCategory::Detrimental,
            5 => EffectCategory::Heal,
            7 => EffectCategory::Summon,
            8 => EffectCategory::Teleport,
            _ => EffectCategory::Other,
        }
    }

    /// Whether this effect helps its target.
    ///
    /// Detrimental effects — snares, roots, damage-over-time, life drains —
    /// land on you constantly and belong in their own list, not among your
    /// buffs.
    pub fn is_beneficial(&self) -> bool {
        self.good_effect != 0
    }

    /// Whether this spell can be tracked as a timed buff at all.
    pub fn is_timed_buff(&self) -> bool {
        self.duration_formula != 0 && (self.duration_cap > 0 || self.duration_formula >= 50)
    }

    /// Effects that last until you zone or die rather than for a real duration.
    ///
    /// The data expresses these as 72,000 ticks (120 hours). Counting that down
    /// on screen is technically accurate and completely useless, so callers
    /// display them as permanent instead.
    pub fn is_permanent(&self) -> bool {
        matches!(self.duration_formula, 50 | 51)
    }

    /// Duration in seconds for a given caster level.
    pub fn duration_seconds(&self, level: u32) -> i64 {
        duration_ticks(self.duration_formula, self.duration_cap, level) as i64 * TICK_SECONDS
    }
}

/// Resolve a spell's duration, in ticks, from its formula, cap and caster level.
///
/// These are the long-standing EverQuest duration formulas. Most compute a
/// level-scaled value and clamp it to the spell's cap; formulas 50/51 mean
/// "effectively permanent" and are represented as a very large tick count.
/// Whether two spell names refer to the same spell, ignoring rank.
///
/// EverQuest Legends writes the rank in the log ("Tuyen's Chant of Frost IV")
/// but ships the base name in `spells_us.txt`. Comparing them literally means
/// no ranked spell ever matches its own cast, which silently costs the
/// resolver its single strongest disambiguator.
/// Bytes of a landing phrase used as its index key. Five, because that is the
/// length of the shortest phrase the spell file ships.
const LANDING_TAIL: usize = 5;

/// The last [`LANDING_TAIL`] bytes of a string, when it has that many.
fn landing_tail(s: &str) -> Option<[u8; LANDING_TAIL]> {
    let b = s.as_bytes();
    b.len()
        .checked_sub(LANDING_TAIL)
        .map(|from| <[u8; LANDING_TAIL]>::try_from(&b[from..]).unwrap())
}

pub fn same_spell(a: &str, b: &str) -> bool {
    base_spell_name(a).eq_ignore_ascii_case(base_spell_name(b))
}

/// A spell name with any trailing rank token removed.
///
/// Handles both the Roman numerals the client uses and a plain integer. A name
/// that is only a numeral is left alone — stripping it would leave nothing to
/// compare.
pub fn base_spell_name(name: &str) -> &str {
    let trimmed = name.trim_end_matches('.').trim();
    let Some((head, last)) = trimmed.rsplit_once(' ') else {
        return trimmed;
    };
    let is_rank = !last.is_empty()
        && (last
            .chars()
            .all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C' | 'D' | 'M'))
            || last.chars().all(|c| c.is_ascii_digit()));
    if is_rank && !head.trim().is_empty() {
        head.trim()
    } else {
        trimmed
    }
}

pub fn duration_ticks(formula: u32, cap: u32, level: u32) -> u32 {
    let level = level.max(1);
    let cap_or = |v: u32| -> u32 {
        // A zero cap means "no cap": use the computed value.
        if cap == 0 {
            v.max(1)
        } else {
            v.min(cap).max(1)
        }
    };
    match formula {
        0 => 0,
        1 | 6 => cap_or(level.div_ceil(2)),
        2 => cap_or((level * 3).div_ceil(5)),
        3 => cap_or(level * 30),
        4 => {
            if cap == 0 {
                50
            } else {
                cap
            }
        }
        5 => {
            if cap == 0 {
                3
            } else {
                cap
            }
        }
        7 => cap_or(level),
        8 => cap_or(level + 10),
        9 => cap_or(level * 2 + 10),
        10 => cap_or(level * 3 + 10),
        11 => cap_or(level * 30 + 90),
        12 => cap_or((level / 4).max(1)),
        13 => cap_or(level * 4 + 10),
        14 => cap_or(level * 5 + 10),
        15 => cap,
        // "Permanent" / until-zone. 72000 ticks = 120 hours.
        50 | 51 => 72_000,
        // Unknown formula: fall back to the declared cap so we still show
        // something sensible rather than dropping the buff entirely.
        _ => cap,
    }
}

/// The loaded spell database plus reverse indices from display text to spell.
pub struct SpellDb {
    spells: HashMap<u32, Spell>,
    by_landed_on_you: HashMap<String, Vec<u32>>,
    /// Landing text shown when a spell hits someone else, to spell ids. Shared
    /// by several spells, so callers must disambiguate.
    by_landed_on_other: HashMap<String, Vec<u32>>,
    /// Landing phrases grouped by their last few bytes.
    ///
    /// What makes [`SpellDb::split_landing_on_other`] cheap enough to run on
    /// every line the parser did not recognise — one in six of them, and a
    /// three-million-line log has half a million. Scanning for the split point
    /// instead cost four seconds of the initial parse. A line's own last bytes
    /// are one hash lookup, and all but a handful of lines match no phrase at
    /// all and stop there.
    by_landed_tail: HashMap<[u8; LANDING_TAIL], Vec<String>>,
    by_fades: HashMap<String, Vec<u32>>,
    by_name: HashMap<String, Vec<u32>>,
}

impl SpellDb {
    /// Load from the two game data files.
    ///
    /// `spells_us.txt` is ~38 MB; we keep only the handful of fields we need,
    /// so the resident database is a few MB rather than the full file.
    pub fn load(spells_path: &Path, strings_path: &Path) -> anyhow::Result<SpellDb> {
        let mut spells = Self::load_spells(spells_path)?;
        Self::apply_strings(&mut spells, strings_path)?;
        Self::verify_restrictions(&mut spells);
        // The effects column is found by position too, and a client file has
        // thousands of charms. None at all means the column moved again.
        if !spells.is_empty() && !spells.values().any(|s| s.is_charm) {
            eprintln!(
                "[spells] no charm spell found in {} spells: the effects column has moved. \
                 Procs, charms and linked spells will not be recognised.",
                spells.len()
            );
        }
        Ok(Self::from_spells(spells))
    }

    /// Check the target-type column still is one, and disown every restriction
    /// if it is not.
    ///
    /// The column is read by position out of a 173-field record, so a patch
    /// that inserts a field ahead of it leaves this reading something else
    /// entirely — and the failure is silent and confident, filing whatever
    /// creature happens to be hit next under a body type from nowhere. Since
    /// the whole point of these restrictions is that the game vouches for
    /// them, evidence that cannot be trusted is worth less than none.
    ///
    /// Three spells whose names state their own restriction serve as the
    /// canaries. If they do not agree, the restrictions are dropped wholesale
    /// and the bestiary simply learns nothing, which is the failure this is
    /// allowed to have.
    fn verify_restrictions(spells: &mut HashMap<u32, Spell>) {
        const CANARIES: [(&str, CreatureKind); 3] = [
            ("dismiss undead", CreatureKind::Undead),
            ("dismiss summoned", CreatureKind::Summoned),
            ("beguile animals", CreatureKind::Animal),
        ];
        let mut checked = 0;
        for (name, want) in CANARIES {
            let Some(s) = spells.values().find(|s| s.name.to_lowercase() == name) else {
                continue;
            };
            checked += 1;
            if s.restricted_to == Some(want) {
                continue;
            }
            eprintln!(
                "[spells] '{}' reads as {:?}, not {:?}: the target-type column has moved. \
                 Creature body types will not be recorded from this spell file.",
                s.name,
                s.restricted_to,
                Some(want)
            );
            for s in spells.values_mut() {
                s.restricted_to = None;
            }
            return;
        }
        if checked == 0 {
            eprintln!(
                "[spells] none of the canary spells are present; creature body types \
                 are being read from an unverified column."
            );
        }
    }

    /// Build a database from spells already in memory, indexing their texts.
    ///
    /// Public so callers can construct a database without the game files —
    /// principally tests, which need a handful of known spells rather than the
    /// shipped 74,000.
    pub fn from_spells(spells: HashMap<u32, Spell>) -> SpellDb {
        let mut by_landed_on_other: HashMap<String, Vec<u32>> = HashMap::new();
        let mut by_landed_on_you: HashMap<String, Vec<u32>> = HashMap::new();
        let mut by_fades: HashMap<String, Vec<u32>> = HashMap::new();
        let mut by_name: HashMap<String, Vec<u32>> = HashMap::new();

        for s in spells.values() {
            if let Some(t) = &s.landed_on_you {
                by_landed_on_you.entry(t.clone()).or_default().push(s.id);
            }
            if let Some(t) = &s.landed_on_other {
                by_landed_on_other.entry(t.clone()).or_default().push(s.id);
            }
            if let Some(t) = &s.fades {
                by_fades.entry(t.clone()).or_default().push(s.id);
            }
            by_name.entry(s.name.to_lowercase()).or_default().push(s.id);
        }
        // `spells` is a HashMap, so the order above is different every time
        // the daemon starts. Every lookup that takes the first or the best of
        // several candidates inherited that, and the same fight named its
        // effect "Harm Touch XII" on one run and "Harm Touch XXXIII" on the
        // next. In id order the answer is the same every run.
        for index in [&mut by_landed_on_other, &mut by_landed_on_you, &mut by_fades, &mut by_name] {
            for ids in index.values_mut() {
                ids.sort_unstable();
            }
        }

        SpellDb {
            spells,
            by_landed_on_you,
            by_landed_tail: {
                let mut m: HashMap<[u8; LANDING_TAIL], Vec<String>> = HashMap::new();
                for phrase in by_landed_on_other.keys() {
                    if let Some(tail) = landing_tail(phrase) {
                        m.entry(tail).or_default().push(phrase.clone());
                    }
                }
                for phrases in m.values_mut() {
                    phrases.sort_unstable();
                }
                m
            },
            by_landed_on_other,
            by_fades,
            by_name,
        }
    }

    /// Build an empty database, for tests and for running without game files.
    pub fn empty() -> SpellDb {
        SpellDb {
            spells: HashMap::new(),
            by_landed_on_you: HashMap::new(),
            by_landed_on_other: HashMap::new(),
            by_landed_tail: HashMap::new(),
            by_fades: HashMap::new(),
            by_name: HashMap::new(),
        }
    }

    /// The body type a target-type value confines a spell to, if any.
    ///
    /// Only these four of the game's target types name a body; the rest
    /// describe how a spell is aimed and say nothing about what it may be
    /// aimed at. 10 and 21 are the single-target and area forms of the same
    /// undead restriction.
    fn target_type_restriction(v: &str) -> Option<CreatureKind> {
        match v {
            "9" => Some(CreatureKind::Animal),
            "10" | "21" => Some(CreatureKind::Undead),
            "11" => Some(CreatureKind::Summoned),
            "16" => Some(CreatureKind::Plant),
            _ => None,
        }
    }

    fn load_spells(path: &Path) -> anyhow::Result<HashMap<u32, Spell>> {
        // Field positions within the 173-column record. Verified against the
        // shipped data: e.g. Spirit of Wolf is formula 3 / cap 360 (36 min),
        // Clarity formula 3 / cap 270 (27 min).
        const F_ID: usize = 0;
        const F_NAME: usize = 1;
        const F_CAST_TIME: usize = 8;
        const F_RECOVERY: usize = 9;
        const F_RECAST: usize = 10;
        const F_DUR_FORMULA: usize = 11;
        const F_DUR_CAP: usize = 12;
        const F_MANA: usize = 14;
        // The beneficial flag; see `Spell::is_beneficial`.
        const F_GOOD_EFFECT: usize = 28;
        // Who the spell may be aimed at. Most values say only how it is
        // targeted — single, group, self, pet — but four name a body type the
        // game will not let you cast it at anything else. See
        // `target_type_restriction`.
        const F_TARGET_TYPE: usize = 30;
        // The effect category; see `Spell::category`. Both this and
        // `F_EFFECTS` are positions in the 173-field layout. The 2026-11
        // client file has 174: a column was inserted somewhere between the
        // class levels (36-51, unmoved) and this one, so both read one
        // further along. `shift` below is how many columns a row has past
        // 173. Reading the old positions from the new file is silent: the
        // effects column held a bare number, and every proc grant, charm
        // and linked spell quietly came back empty.
        const F_CATEGORY: usize = 146;
        const FIELDS_V1: usize = 173;
        // The effect table: slots separated by `$`, fields within a slot by
        // `|`, the second field being the SPA id. e.g. Solon's Bewitching
        // Bravura is `1|22|1|0|100|51$2|50|-1|0|119|0`.
        const F_EFFECTS: usize = 172;
        // Server Packet Argument for Charm.
        const SPA_CHARM: &str = "22";
        // SPAs that apply another spell to the same target. 475 is the
        // illusion-benefit link; 374 is the general "cast this too" effect.
        // Both carry the linked spell id in the slot's second base value.
        const SPA_APPLY: [&str; 2] = ["374", "475"];
        // SPAs that grant a proc, and where in the slot the proc's spell id
        // sits. The two groups disagree, and getting it wrong is silent:
        //
        //   85 / 419  add a melee proc, id in `base`; `base2` is a rate
        //             modifier. Spirit of the Puma: `5|85|6908|400|100|0`.
        //   340 / 383 cast on spell use, id in `base2`; `base` is a chance.
        //             Tuyen's Chant of Flame: `4|340|100|22487|100|0`.
        //
        // Reading `base` for the second group yields 100, 50 or 1000 — all of
        // which are real spell ids, so the mistake resolves to a plausible
        // name (100 is Summon Throwing Dagger) rather than to nothing.
        // Verified across the shipped file: every one of the 1,410 melee slots
        // resolves from `base`, and every one of the 826 spell-use slots from
        // `base2`.
        const SPA_PROC_BASE: [&str; 2] = ["85", "419"];
        const SPA_PROC_BASE2: [&str; 2] = ["340", "383"];

        let file = std::fs::File::open(path)?;
        let reader = BufReader::with_capacity(1 << 20, file);
        let mut out = HashMap::new();

        for line in reader.lines() {
            let line = line?;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split('^').collect();
            if f.len() <= F_MANA {
                continue;
            }
            let Ok(id) = f[F_ID].parse::<u32>() else {
                continue;
            };
            let num = |i: usize| f.get(i).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            let shift = f.len().saturating_sub(FIELDS_V1);
            let (f_category, f_effects) = (F_CATEGORY + shift, F_EFFECTS + shift);
            let mut class_levels = [CLASS_CANNOT_CAST; CLASS_COUNT];
            for (i, slot) in class_levels.iter_mut().enumerate() {
                *slot = f
                    .get(CLASS_FIELD_BASE + i)
                    .and_then(|v| v.parse::<u8>().ok())
                    .unwrap_or(CLASS_CANNOT_CAST);
            }
            out.insert(
                id,
                Spell {
                    id,
                    class_levels,
                    good_effect: f
                        .get(F_GOOD_EFFECT)
                        .and_then(|v| v.parse::<u8>().ok())
                        .unwrap_or(1),
                    category_id: f
                        .get(f_category)
                        .and_then(|v| v.parse::<u8>().ok())
                        .unwrap_or(0),
                    is_charm: f
                        .get(f_effects)
                        .map(|e| {
                            e.split('$')
                                .any(|slot| slot.split('|').nth(1) == Some(SPA_CHARM))
                        })
                        .unwrap_or(false),
                    linked: f
                        .get(f_effects)
                        .map(|e| {
                            e.split('$')
                                .filter_map(|slot| {
                                    let p: Vec<&str> = slot.split('|').collect();
                                    let spa = *p.get(1)?;
                                    if !SPA_APPLY.contains(&spa) {
                                        return None;
                                    }
                                    p.get(3)?.parse::<u32>().ok().filter(|v| *v > 0)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    procs: f
                        .get(f_effects)
                        .map(|e| {
                            e.split('$')
                                .filter_map(|slot| {
                                    let p: Vec<&str> = slot.split('|').collect();
                                    let spa = *p.get(1)?;
                                    let (at, on_melee) = if SPA_PROC_BASE.contains(&spa) {
                                        (2, true)
                                    } else if SPA_PROC_BASE2.contains(&spa) {
                                        (3, false)
                                    } else {
                                        return None;
                                    };
                                    let spell =
                                        p.get(at)?.parse::<u32>().ok().filter(|v| *v > 0)?;
                                    Some(ProcGrant { spell, on_melee })
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    restricted_to: f
                        .get(F_TARGET_TYPE)
                        .and_then(|v| Self::target_type_restriction(v)),
                    name: f[F_NAME].to_string(),
                    cast_time_ms: num(F_CAST_TIME),
                    recovery_ms: num(F_RECOVERY),
                    recast_ms: num(F_RECAST),
                    duration_formula: num(F_DUR_FORMULA),
                    duration_cap: num(F_DUR_CAP),
                    mana: num(F_MANA),
                    landed_on_you: None,
                    landed_on_other: None,
                    fades: None,
                },
            );
        }
        Ok(out)
    }

    fn apply_strings(spells: &mut HashMap<u32, Spell>, path: &Path) -> anyhow::Result<()> {
        // Layout: SPELLINDEX^CASTERMETXT^CASTEROTHERTXT^CASTEDMETXT^CASTEDOTHERTXT^SPELLGONE^
        const F_ID: usize = 0;
        const F_CASTED_ME: usize = 3;
        const F_CASTED_OTHER: usize = 4;
        const F_GONE: usize = 5;

        let file = std::fs::File::open(path)?;
        let reader = BufReader::with_capacity(1 << 20, file);

        for line in reader.lines() {
            let line = line?;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split('^').collect();
            if f.len() <= F_GONE {
                continue;
            }
            let Ok(id) = f[F_ID].parse::<u32>() else {
                continue;
            };
            let Some(spell) = spells.get_mut(&id) else {
                continue;
            };
            let opt = |s: &str| {
                let t = s.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            };
            spell.landed_on_you = opt(f[F_CASTED_ME]);
            spell.landed_on_other = opt(f[F_CASTED_OTHER]);
            spell.fades = opt(f[F_GONE]);
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.spells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spells.is_empty()
    }

    /// Every spell in the database, in no particular order.
    pub fn all(&self) -> impl Iterator<Item = &Spell> {
        self.spells.values()
    }

    pub fn get(&self, id: u32) -> Option<&Spell> {
        self.spells.get(&id)
    }

    /// Candidate spells whose "landed on you" text matches exactly.
    pub fn candidates_for_landing(&self, text: &str) -> &[u32] {
        self.by_landed_on_you
            .get(text)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Candidate spells whose "fades" text matches exactly.
    /// Spells whose "landed on someone else" text matches, e.g.
    /// `'s eyes glaze over.`.
    pub fn candidates_for_landing_on_other(&self, text: &str) -> &[u32] {
        self.by_landed_on_other
            .get(text)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Split `a will pillager's body starts to slow.` into the creature and
    /// the spells that phrase could belong to.
    ///
    /// The index is keyed on the exact phrase, because that is what the spell
    /// file ships; a log line is that phrase with a creature's name stuck on
    /// the front. Rather than test the line against every phrase in the file —
    /// tens of thousands of them, on every line that reached this far — the
    /// split can only fall where the phrase begins, and a landing phrase
    /// always begins either with the possessive or with a space. So the
    /// candidate splits are few and each one is a single hash lookup.
    ///
    /// The longest match wins: `'s body starts to slow.` and ` starts to
    /// slow.` would both match a creature named `Xicotl's body`, and the
    /// longer phrase is the one that leaves a sensible name behind.
    pub fn split_landing_on_other<'a>(&self, line: &'a str) -> Option<(&'a str, &[u32])> {
        // Every landing phrase is a sentence, and its last bytes are the
        // cheapest thing to reject a line on.
        if !line.ends_with('.') {
            return None;
        }
        let candidates = self.by_landed_tail.get(&landing_tail(line)?)?;
        // The longest phrase that fits wins: `'s body starts to slow.` and
        // ` starts to slow.` would both match a creature called `Xicotl's
        // body`, and the longer one leaves a sensible name behind.
        let mut best: Option<(&'a str, &[u32])> = None;
        for phrase in candidates {
            // Something has to be left over to be the target.
            if phrase.len() >= line.len() || !line.ends_with(phrase.as_str()) {
                continue;
            }
            let target = &line[..line.len() - phrase.len()];
            // The split has to fall on a word boundary, or `a will pillager`
            // could be cut mid-name by a phrase that merely happens to end the
            // same way.
            if !(target.ends_with(' ') || target.ends_with('\'') || phrase.starts_with([' ', '\''])) {
                continue;
            }
            if best.is_none_or(|(t, _)| target.len() < t.len()) {
                let ids = self.by_landed_on_other.get(phrase.as_str())?;
                best = Some((target.trim_end(), ids.as_slice()));
            }
        }
        best
    }

    pub fn candidates_for_fade(&self, text: &str) -> &[u32] {
        self.by_fades.get(text).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Candidate spells with the given name (case-insensitive). Names repeat
    /// across ranks, so this is also a set.
    pub fn candidates_for_name(&self, name: &str) -> &[u32] {
        self.by_name
            .get(&name.to_lowercase())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Merged class requirements for every spell sharing this name.
    ///
    /// Names repeat across ranks, and different ranks can be available to
    /// different classes, so we take the lowest (most permissive) level any
    /// rank offers each class.
    pub fn class_levels_for_name(&self, name: &str) -> Option<[u8; CLASS_COUNT]> {
        let ids = self.candidates_for_name(name);
        if ids.is_empty() {
            return None;
        }
        let mut merged = [CLASS_CANNOT_CAST; CLASS_COUNT];
        for id in ids {
            let Some(s) = self.get(*id) else { continue };
            for (slot, &lvl) in merged.iter_mut().zip(s.class_levels.iter()) {
                if lvl != CLASS_CANNOT_CAST && lvl < *slot {
                    *slot = lvl;
                }
            }
        }
        if merged.iter().all(|v| *v == CLASS_CANNOT_CAST) {
            None
        } else {
            Some(merged)
        }
    }

    /// True if this text is known to be a spell landing message.
    pub fn is_landing_text(&self, text: &str) -> bool {
        self.by_landed_on_you.contains_key(text)
    }

    /// True if this text is known to be a spell wear-off message.
    pub fn is_fade_text(&self, text: &str) -> bool {
        self.by_fades.contains_key(text)
    }
}

/// A cast we have seen recently, used to disambiguate the landing text that
/// follows it.
#[derive(Debug, Clone)]
struct RecentCast {
    timestamp: i64,
    spell_name: String,
}

/// A user-declared classification for an effect.
///
/// Detection is behavioural and therefore needs evidence to accumulate — an
/// aura is not recognised until it has pulsed a few times, so a song spends its
/// first seconds looking like a timed buff. A mark states the answer up front
/// and never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuffMark {
    /// Self-sustaining; list with auras, never count down.
    Aura,
    /// Lasts until you zone; show as permanent.
    Permanent,
}

/// A buff currently believed to be active.
#[derive(Debug, Clone)]
pub struct ActiveBuff {
    pub spell_id: u32,
    pub name: String,
    pub started: i64,
    /// Absolute expiry timestamp. `None` when the duration is unknown.
    pub expires: Option<i64>,
    /// How confident the resolver is in the spell identification, 0.0–1.0.
    /// Landing text alone is ambiguous, so this is surfaced to the UI rather
    /// than hidden.
    pub confidence: f32,
    /// True for until-you-zone effects, which should be shown as permanent
    /// rather than as a multi-day countdown.
    pub permanent: bool,
    /// A user-declared classification, if one was set.
    pub mark: Option<BuffMark>,
    /// What the effect does, for colouring.
    pub category: EffectCategory,
    /// When this effect was first seen, kept across refreshes so pulse rate can
    /// be measured.
    pub first_seen: i64,
    /// How many times it has re-landed since first seen.
    pub refreshes: u32,
    /// False for detrimental effects — snares, roots, drains — which are
    /// reported apart from buffs so a debuff is never mistaken for one.
    pub beneficial: bool,
}

impl ActiveBuff {
    /// Whether this looks like a self-sustaining aura rather than a timed buff.
    ///
    /// Judged from behaviour, not from a hard-coded song list: anything that
    /// keeps re-landing every few seconds is being auto-recast and has no
    /// meaningful countdown.
    pub fn is_aura(&self) -> bool {
        // A declaration is final; behaviour is only consulted without one.
        match self.mark {
            Some(BuffMark::Aura) => return true,
            Some(BuffMark::Permanent) => return false,
            None => {}
        }
        if self.refreshes < AURA_MIN_REFRESHES {
            return false;
        }
        let span = (self.started - self.first_seen).max(1);
        span / self.refreshes as i64 <= AURA_MAX_INTERVAL
    }

    pub fn remaining(&self, now: i64) -> Option<i64> {
        self.expires.map(|e| (e - now).max(0))
    }
}

/// How long after a cast begins we will still associate a landing message with
/// it. Generous enough to cover long cast times plus client lag.
const CAST_ASSOCIATION_WINDOW: i64 = 20;

/// Re-landings needed before an effect is judged to be a self-sustaining aura.
///
/// Bard songs run passively via `/melody` and re-land every few seconds — one
/// song in a real log pulsed 19,174 times. Timing those is meaningless and
/// they crowd out the buffs that genuinely need watching, so they are
/// classified separately rather than shown as countdowns.
const AURA_MIN_REFRESHES: u32 = 3;

/// Longest average gap between re-landings that still counts as an aura.
///
/// Comfortably above a bard song's pulse but well below any real buff's
/// duration, so a buff that merely gets recast occasionally is not misfiled.
const AURA_MAX_INTERVAL: i64 = 45;

/// Resolves flavour text to spells and maintains the active buff set.
///
/// # Disambiguation strategy
///
/// Ranked strongest to weakest:
///
/// 1. **Unique candidate** — no ambiguity at all.
/// 2. **Recent cast** — a `You begin casting/singing <name>.` inside the
///    association window naming one of the candidates.
/// 3. **Repertoire** — a candidate the character has cast at *any* point this
///    session. This matters enormously for bards: a song's landing text pulses
///    thousands of times while the "begins singing" line appears once, so
///    without repertoire memory almost every pulse would fall back to a guess.
/// 4. **Shared name** — all candidates are ranks of one spell, so the name is
///    certain even though the rank is not.
/// 5. **Fallback** — lowest spell id, reported with low confidence.
pub struct SpellResolver {
    db: SpellDb,
    /// Casts started by the logging character, newest last.
    recent_casts: Vec<RecentCast>,
    /// Every spell name the logging character has been seen to cast or sing,
    /// lowercased. Never pruned: it is a model of what this character *can*
    /// do, which does not expire.
    repertoire: std::collections::HashSet<String>,
    /// Cache of landing text to the spell we confidently resolved it to, so
    /// repeated pulses stay stable and cheap.
    learned: HashMap<String, u32>,
    /// User-declared classifications, keyed by lowercased spell name.
    marks: HashMap<String, BuffMark>,
    /// Current character level, kept up to date from level-up messages. Buff
    /// durations scale with it.
    level: u32,
    active: Vec<ActiveBuff>,
    /// Confidence of the most recent [`Self::resolve_landing`]. Diagnostic
    /// only — the authoritative per-buff value lives on [`ActiveBuff`].
    last_confidence: f32,
}

impl SpellResolver {
    pub fn new(db: SpellDb, level: u32) -> Self {
        SpellResolver {
            db,
            recent_casts: Vec::new(),
            repertoire: std::collections::HashSet::new(),
            learned: HashMap::new(),
            marks: HashMap::new(),
            level,
            active: Vec::new(),
            last_confidence: 0.0,
        }
    }

    /// Declare how an effect should be classified, or clear the declaration.
    pub fn set_mark(&mut self, name: &str, mark: Option<BuffMark>) {
        let key = name.to_lowercase();
        match mark {
            Some(m) => {
                self.marks.insert(key.clone(), m);
            }
            None => {
                self.marks.remove(&key);
            }
        }
        // Apply to anything currently active so the UI updates at once.
        let applied = self.marks.get(&key).copied();
        for b in self.active.iter_mut() {
            if b.name.to_lowercase() == key {
                b.mark = applied;
                if applied == Some(BuffMark::Permanent) {
                    b.permanent = true;
                }
            }
        }
    }

    /// All declared classifications.
    pub fn marks(&self) -> Vec<(String, BuffMark)> {
        let mut v: Vec<(String, BuffMark)> =
            self.marks.iter().map(|(k, m)| (k.clone(), *m)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Confidence of the most recent landing resolution, for diagnostics.
    pub fn last_confidence(&self) -> f32 {
        self.last_confidence
    }

    /// Whether you cast a named spell within the cast-association window.
    ///
    /// Compared on the base name: the client writes the rank ("Solon's
    /// Bewitching Bravura V") while the database carries only the base
    /// ("Solon's Bewitching Bravura"), so an exact match never fires for any
    /// ranked spell.
    ///
    /// Used to decide which of several spells sharing a landing phrase
    /// actually landed.
    pub fn cast_recently(&self, timestamp: i64, spell_name: &str) -> bool {
        let cutoff = timestamp - CAST_ASSOCIATION_WINDOW;
        self.recent_casts
            .iter()
            .any(|c| c.timestamp >= cutoff && same_spell(&c.spell_name, spell_name))
    }

    /// Whether you cast exactly this rank recently, not just a spell of the
    /// same name.
    pub fn cast_this_rank_recently(&self, timestamp: i64, spell_name: &str) -> bool {
        self.recent_cast_names(timestamp).any(|n| n.eq_ignore_ascii_case(spell_name))
    }

    /// The names you cast within the association window, rank included.
    /// A handful at most, so searching from these is cheaper than testing
    /// every candidate a landing phrase could belong to.
    pub fn recent_cast_names(&self, timestamp: i64) -> impl Iterator<Item = &str> {
        let cutoff = timestamp - CAST_ASSOCIATION_WINDOW;
        self.recent_casts.iter().filter(move |c| c.timestamp >= cutoff).map(|c| c.spell_name.as_str())
    }

    pub fn db(&self) -> &SpellDb {
        &self.db
    }

    pub fn level(&self) -> u32 {
        self.level
    }

    pub fn set_level(&mut self, level: u32) {
        self.level = level;
    }

    /// Record that the logging character began casting something.
    pub fn note_cast(&mut self, timestamp: i64, spell_name: &str) {
        self.repertoire.insert(spell_name.to_lowercase());
        self.recent_casts.push(RecentCast {
            timestamp,
            spell_name: spell_name.to_string(),
        });
        // Keep the window small; this list is scanned on every landing.
        let cutoff = timestamp - CAST_ASSOCIATION_WINDOW;
        self.recent_casts.retain(|c| c.timestamp >= cutoff);
    }

    /// Record that the character owns a spell, without implying they just cast
    /// it. Memorised and granted spells are the strongest repertoire signal:
    /// they cover spells whose "begins casting" line never appears, such as
    /// bard songs whose only trace in the log is their pulsing effect text.
    pub fn note_known_spell(&mut self, spell_name: &str) {
        self.repertoire.insert(spell_name.to_lowercase());
    }

    /// Number of distinct spells the character is known to cast.
    pub fn repertoire_size(&self) -> usize {
        self.repertoire.len()
    }

    /// Try to interpret `text` as an effect landing on the logging character.
    ///
    /// Returns the chosen spell id when the text is recognised. Ambiguity is
    /// resolved by preferring a candidate whose name matches a cast started
    /// within the association window.
    /// Put a spell into the active list, or refresh it if already there.
    ///
    /// Shared by direct landings and by companion buffs pulled in through a
    /// spell's effect links, so both are tracked identically.
    fn activate(&mut self, id: u32, timestamp: i64, confidence: f32) {
        let Some(spell) = self.db.get(id) else { return };

        // An instant effect — a direct nuke, a heal — is an event, not a
        // state. Tracking it would leave it in the list forever, since it has
        // no duration to expire.
        if !spell.is_timed_buff() {
            return;
        }
        let secs = spell.duration_seconds(self.level);
        let expires = (secs > 0).then(|| timestamp + secs);

        let mark = self.marks.get(&spell.name.to_lowercase()).copied();
        let category = spell.category();
        let permanent = spell.is_permanent() || mark == Some(BuffMark::Permanent);
        let beneficial = spell.is_beneficial();
        let name = spell.name.clone();
        let spell_id = spell.id;

        // Re-casting refreshes rather than stacks. Update in place so the
        // refresh history survives — discarding and re-inserting would reset
        // the pulse count that identifies auras.
        match self.active.iter_mut().find(|b| b.name == name) {
            Some(existing) => {
                existing.spell_id = spell_id;
                existing.started = timestamp;
                existing.expires = expires;
                existing.confidence = confidence;
                existing.permanent = permanent;
                existing.beneficial = beneficial;
                existing.mark = mark;
                existing.category = category;
                existing.refreshes += 1;
            }
            None => self.active.push(ActiveBuff {
                spell_id,
                name,
                started: timestamp,
                expires,
                confidence,
                permanent,
                beneficial,
                mark,
                category,
                first_seen: timestamp,
                refreshes: 0,
            }),
        }
    }

    pub fn resolve_landing(&mut self, timestamp: i64, text: &str) -> Option<u32> {
        // Copied out so the mutable updates below do not conflict with the
        // immutable borrow of the database.
        let candidates: Vec<u32> = self.db.candidates_for_landing(text).to_vec();
        if candidates.is_empty() {
            return None;
        }

        let (chosen, confidence) = self.score_candidates(&candidates, timestamp, text);
        self.last_confidence = confidence;
        // Remember confident resolutions so repeated pulses of the same text
        // stay stable even after the originating cast falls out of the window.
        if confidence >= 0.9 {
            self.learned.insert(text.to_string(), chosen);
        }
        self.activate(chosen, timestamp, confidence);

        // A single cast can grant more than one buff, and the extra ones have
        // no landing text — nothing is written to the log when they take
        // hold. `Form of the Great Wolf` is the wolf illusion plus
        // `Illusion Benefit Dena`, the stats that come with it. Following the
        // link is the only way the second one can be known about.
        let linked: Vec<u32> = self
            .db
            .get(chosen)
            .map(|s| s.linked.clone())
            .unwrap_or_default();
        for id in linked {
            // Confidence is inherited: the link is certain, so a companion
            // buff is exactly as trustworthy as the landing that implied it.
            self.activate(id, timestamp, confidence);
        }

        Some(chosen)
    }

    /// Pick the best candidate and report confidence. See the type-level docs
    /// for the ranking of evidence used here.
    fn score_candidates(&self, candidates: &[u32], timestamp: i64, text: &str) -> (u32, f32) {
        if candidates.len() == 1 {
            return (candidates[0], 1.0);
        }

        // 2. A cast naming one of the candidates, close in time.
        let cutoff = timestamp - CAST_ASSOCIATION_WINDOW;
        for cast in self.recent_casts.iter().rev() {
            if cast.timestamp < cutoff {
                continue;
            }
            for &id in candidates {
                if let Some(s) = self.db.get(id) {
                    if same_spell(&s.name, &cast.spell_name) {
                        return (id, 0.95);
                    }
                }
            }
        }

        // 3a. A previously learned binding for this exact text.
        if let Some(&id) = self.learned.get(text) {
            if candidates.contains(&id) {
                return (id, 0.9);
            }
        }

        // 3b. Candidates the character is known to cast.
        let in_repertoire: Vec<u32> = candidates
            .iter()
            .copied()
            .filter(|id| {
                self.db
                    .get(*id)
                    .is_some_and(|s| self.repertoire.contains(&s.name.to_lowercase()))
            })
            .collect();
        match in_repertoire.len() {
            1 => return (in_repertoire[0], 0.9),
            2.. => {
                // Several known spells share this text; the name is still
                // narrowed to the character's own repertoire.
                return (in_repertoire.into_iter().min().unwrap(), 0.7);
            }
            _ => {}
        }

        // 4. All candidates are ranks of the same spell: name is certain.
        let first_name = self.db.get(candidates[0]).map(|s| s.name.to_lowercase());
        let all_same_name = first_name.as_ref().is_some_and(|n| {
            candidates.iter().all(|id| {
                self.db
                    .get(*id)
                    .is_some_and(|s| &s.name.to_lowercase() == n)
            })
        });
        let fallback = candidates.iter().copied().min().unwrap_or(candidates[0]);
        if all_same_name {
            return (fallback, 0.8);
        }

        // 5. No evidence.
        (fallback, 0.3)
    }

    /// Whether this text is the wear-off message of something currently up.
    ///
    /// Decisive when a string serves as both a landing and a fade message —
    /// "You slow down." lands Languid Pace but also ends Spirit of Wolf and
    /// every Quick Time rank. If a spell it would end is actually active, the
    /// fade reading is the correct one; treating it as a landing invents a
    /// debuff out of a buff expiring.
    pub fn is_fade_of_active(&self, text: &str) -> bool {
        let candidates = self.db.candidates_for_fade(text);
        if candidates.is_empty() {
            return false;
        }
        self.active.iter().any(|b| candidates.contains(&b.spell_id))
    }

    /// Try to interpret `text` as an effect wearing off. Removes the matching
    /// active buff if one is tracked.
    pub fn resolve_fade(&mut self, text: &str) -> Option<u32> {
        let candidates = self.db.candidates_for_fade(text);
        if candidates.is_empty() {
            return None;
        }
        // Prefer removing a buff we actually believe is active.
        if let Some(pos) = self
            .active
            .iter()
            .position(|b| candidates.contains(&b.spell_id))
        {
            return Some(self.active.remove(pos).spell_id);
        }
        // Otherwise, still recognise the line so it counts as parsed.
        candidates.first().copied()
    }

    /// Drop buffs whose expiry has passed.
    pub fn expire(&mut self, now: i64) {
        self.active.retain(|b| b.expires.is_none_or(|e| e > now));
    }

    /// Drop every buff, because you died.
    ///
    /// Death strips buffs, but the client says nothing when it happens — no
    /// fade lines follow a death, verified across all 37 in a real log. So the
    /// death itself is the only signal, and without acting on it a permanent
    /// buff never comes off: Firefist and Thorncoat carry duration formula 50,
    /// so they would sit on the overlay claiming five days remaining through
    /// every death for the rest of the session.
    ///
    /// Everything goes, not just the permanent ones. Death takes timed buffs
    /// too, and anything still genuinely running re-announces itself quickly —
    /// auras re-pulse within a tick or two of standing up.
    pub fn clear_on_death(&mut self) {
        self.active.clear();
    }

    /// Whether an effect is one of *your* auras.
    ///
    /// Pulsing alone is not enough: hostile spells land on you repeatedly too —
    /// one mob nuke re-landed 584 times in a real log — and classifying those
    /// as auras is plainly wrong. An aura must also be a spell this character
    /// is known to cast, which the repertoire records.
    fn is_own_aura(&self, b: &ActiveBuff) -> bool {
        if b.mark == Some(BuffMark::Aura) {
            return true;
        }
        // A detrimental effect is never one of your auras, however it pulses.
        b.beneficial && b.is_aura() && self.repertoire.contains(&b.name.to_lowercase())
    }

    /// Detrimental effects currently on you, soonest expiry first.
    ///
    /// Snares, roots, damage-over-time and drains land constantly in combat.
    /// They matter — but as things to cure or wait out, not as buffs.
    pub fn active_debuffs(&self, now: i64) -> Vec<ActiveBuff> {
        let mut v: Vec<ActiveBuff> = self
            .active
            .iter()
            .filter(|b| !b.beneficial && b.expires.is_none_or(|e| e > now))
            .cloned()
            .collect();
        v.sort_by_key(|b| b.expires.unwrap_or(i64::MAX));
        v
    }

    /// Self-sustaining auras, most recently refreshed first.
    ///
    /// Bards run up to five songs passively via `/melody`; they re-land
    /// constantly and are reported apart from timed buffs so they do not bury
    /// the timers that matter.
    pub fn active_auras(&self, now: i64) -> Vec<ActiveBuff> {
        let mut v: Vec<ActiveBuff> = self
            .active
            .iter()
            .filter(|b| self.is_own_aura(b) && b.expires.is_none_or(|e| e > now))
            .cloned()
            .collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.started));
        v
    }

    /// Currently active buffs, soonest expiry first. Buffs with unknown
    /// duration sort last.
    /// Auras are reported by [`Self::active_auras`] instead, so a handful of
    /// constantly-pulsing songs cannot bury the timers worth watching.
    pub fn active_buffs(&self, now: i64) -> Vec<ActiveBuff> {
        let mut v: Vec<ActiveBuff> = self
            .active
            .iter()
            .filter(|b| b.beneficial && !self.is_own_aura(b) && b.expires.is_none_or(|e| e > now))
            .cloned()
            .collect();
        v.sort_by_key(|b| b.expires.unwrap_or(i64::MAX));
        v
    }

    pub fn clear_buffs(&mut self) {
        self.active.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_3_scales_then_clamps() {
        // Spirit of Wolf: formula 3, cap 360 ticks (36 minutes).
        // level*30 exceeds the cap from level 12 upward.
        assert_eq!(duration_ticks(3, 360, 50), 360);
        assert_eq!(duration_ticks(3, 360, 5), 150);
    }

    #[test]
    fn formula_10_matches_regeneration() {
        // Regeneration: formula 10, cap 205. level*3+10 at level 50 = 160.
        assert_eq!(duration_ticks(10, 205, 50), 160);
        // Above the cap it clamps.
        assert_eq!(duration_ticks(10, 205, 200), 205);
    }

    #[test]
    fn formula_0_is_instant() {
        assert_eq!(duration_ticks(0, 0, 50), 0);
    }

    #[test]
    fn permanent_formulas_are_long() {
        assert_eq!(duration_ticks(50, 0, 1), 72_000);
        assert_eq!(duration_ticks(51, 0, 1), 72_000);
    }

    #[test]
    fn zero_cap_means_uncapped() {
        assert_eq!(duration_ticks(7, 0, 42), 42);
    }

    fn db_with(spells: Vec<Spell>) -> SpellDb {
        let mut db = SpellDb::empty();
        for s in spells {
            if let Some(t) = &s.landed_on_you {
                db.by_landed_on_you.entry(t.clone()).or_default().push(s.id);
            }
            if let Some(t) = &s.fades {
                db.by_fades.entry(t.clone()).or_default().push(s.id);
            }
            db.by_name
                .entry(s.name.to_lowercase())
                .or_default()
                .push(s.id);
            db.spells.insert(s.id, s);
        }
        db
    }

    fn spell(id: u32, name: &str, landed: &str, fades: &str, formula: u32, cap: u32) -> Spell {
        Spell {
            id,
            name: name.into(),
            cast_time_ms: 0,
            recovery_ms: 0,
            recast_ms: 0,
            duration_formula: formula,
            duration_cap: cap,
            mana: 0,
            landed_on_you: Some(landed.into()),
            landed_on_other: None,
            fades: Some(fades.into()),
            restricted_to: None,
            class_levels: [CLASS_CANNOT_CAST; CLASS_COUNT],
            good_effect: 1,
            category_id: 1,
            is_charm: false,
            linked: Vec::new(),
            procs: Vec::new(),
        }
    }

    /// Build a detrimental spell for tests.
    fn debuff(id: u32, name: &str, landed: &str, fades: &str, formula: u32, cap: u32) -> Spell {
        let mut s = spell(id, name, landed, fades, formula, cap);
        s.good_effect = 0;
        s
    }

    #[test]
    fn unique_landing_text_resolves_with_full_confidence() {
        let db = db_with(vec![spell(
            1,
            "Spirit of Wolf",
            "Your feet move faster.",
            "You slow down.",
            3,
            360,
        )]);
        let mut r = SpellResolver::new(db, 50);
        assert_eq!(r.resolve_landing(1000, "Your feet move faster."), Some(1));
        let buffs = r.active_buffs(1000);
        assert_eq!(buffs.len(), 1);
        assert_eq!(buffs[0].confidence, 1.0);
        // 360 ticks * 6s = 2160s
        assert_eq!(buffs[0].expires, Some(1000 + 2160));
        assert_eq!(buffs[0].remaining(1000), Some(2160));
    }

    #[test]
    fn ambiguous_landing_disambiguated_by_recent_cast() {
        let db = db_with(vec![
            spell(
                1,
                "Minor Healing",
                "Your wounds begin to heal.",
                "Your wounds stop healing.",
                10,
                100,
            ),
            spell(
                2,
                "Celestial Cleansing",
                "Your wounds begin to heal.",
                "Your wounds stop healing.",
                10,
                200,
            ),
        ]);
        let mut r = SpellResolver::new(db, 50);
        r.note_cast(995, "Celestial Cleansing");
        assert_eq!(
            r.resolve_landing(1000, "Your wounds begin to heal."),
            Some(2)
        );
        assert_eq!(r.active_buffs(1000)[0].confidence, 0.95);
    }

    #[test]
    fn ambiguous_landing_without_context_is_low_confidence() {
        let db = db_with(vec![
            spell(
                7,
                "Minor Healing",
                "Your wounds begin to heal.",
                "Your wounds stop healing.",
                10,
                100,
            ),
            spell(
                694,
                "Pact",
                "Your wounds begin to heal.",
                "Your pact has faded.",
                10,
                200,
            ),
        ]);
        let mut r = SpellResolver::new(db, 50);
        let id = r
            .resolve_landing(1000, "Your wounds begin to heal.")
            .unwrap();
        assert_eq!(id, 7, "should fall back to the lowest id");
        assert!(r.active_buffs(1000)[0].confidence < 0.5);
    }

    /// The bard case: a song's landing text pulses long after the single
    /// "begins singing" line has fallen out of the association window.
    /// Repertoire memory must keep resolving it correctly.
    #[test]
    fn repertoire_resolves_pulses_long_after_the_cast() {
        let db = db_with(vec![
            spell(
                1,
                "Item Benefit: Jonthan's Whistling Warsong",
                "You whistle an ancient warsong.",
                "You stop whistling.",
                5,
                2,
            ),
            spell(
                2,
                "Jonthan's Whistling Warsong",
                "You whistle an ancient warsong.",
                "You stop whistling.",
                5,
                2,
            ),
        ]);
        let mut r = SpellResolver::new(db, 50);
        r.note_cast(1000, "Jonthan's Whistling Warsong");
        assert_eq!(
            r.resolve_landing(1001, "You whistle an ancient warsong."),
            Some(2)
        );

        // Thousands of seconds later, far outside the cast window, the pulse
        // must still resolve to the song the character actually sings.
        let id = r
            .resolve_landing(99_000, "You whistle an ancient warsong.")
            .unwrap();
        assert_eq!(id, 2, "repertoire should outlive the cast window");
        let buff = r.active_buffs(99_000).into_iter().next().unwrap();
        assert!(buff.confidence >= 0.9, "confidence was {}", buff.confidence);
    }

    /// The client writes ranks; the database does not. Comparing literally
    /// means no ranked spell ever matches its own cast.
    #[test]
    fn spell_names_match_across_rank_suffixes() {
        assert!(same_spell(
            "Solon's Bewitching Bravura",
            "Solon's Bewitching Bravura V"
        ));
        assert!(same_spell(
            "Tuyen's Chant of Frost IV",
            "Tuyen's Chant of Frost"
        ));
        assert!(same_spell("Forest's Renewal 3", "Forest's Renewal"));
        // Different spells must still not collide.
        assert!(!same_spell("Chloroplast", "Chords of Dissonance"));
        // A trailing word that merely looks like a numeral is not stripped
        // when it is the whole name.
        assert_eq!(base_spell_name("V"), "V");
        // Nor is a real word ending the name.
        assert_eq!(base_spell_name("Shield of Thorns"), "Shield of Thorns");
    }

    /// One cast, two buffs. The companion has no landing text of its own, so
    /// following the effect link is the only way it can ever be tracked.
    #[test]
    fn a_linked_spell_is_activated_alongside_its_parent() {
        let mut m = HashMap::new();
        // The illusion: permanent, and it applies the stat buff.
        let mut wolf = spell(
            427,
            "Form of the Great Wolf",
            "You are now a wolf.",
            "You are no longer a wolf.",
            50,
            0,
        );
        wolf.linked = vec![40593];
        m.insert(427, wolf);
        // The companion: a real duration, and no text anywhere.
        let mut benefit = spell(40593, "Illusion Benefit Dena", "", "", 3, 720);
        benefit.landed_on_you = None;
        benefit.fades = None;
        m.insert(40593, benefit);

        let mut r = SpellResolver::new(SpellDb::from_spells(m), 50);
        r.resolve_landing(1000, "You are now a wolf.");

        let names: Vec<String> = r
            .active_buffs(1000)
            .iter()
            .map(|b| b.name.clone())
            .collect();
        assert!(names.contains(&"Form of the Great Wolf".to_string()));
        assert!(
            names.contains(&"Illusion Benefit Dena".to_string()),
            "the linked stat buff must come with it: {names:?}"
        );

        // Each keeps its own duration: the illusion outlasts the benefit.
        let benefit = r
            .active_buffs(1000)
            .into_iter()
            .find(|b| b.name == "Illusion Benefit Dena")
            .unwrap();
        assert!(
            benefit.expires.is_some(),
            "the companion is timed, not permanent"
        );
    }

    /// Death strips buffs and the client says nothing, so a permanent buff
    /// would otherwise sit on the overlay for the rest of the session.
    #[test]
    fn death_clears_buffs_including_permanent_ones() {
        let mut db_spells = HashMap::new();
        // Formula 50 is the "permanent" form Firefist and Thorncoat carry.
        db_spells.insert(
            254,
            spell(
                254,
                "Firefist",
                "Your fist bursts into flame.",
                "gone",
                50,
                0,
            ),
        );
        db_spells.insert(
            423,
            spell(
                423,
                "Skin like Nature",
                "Your skin hardens.",
                "gone2",
                3,
                720,
            ),
        );
        let mut r = SpellResolver::new(SpellDb::from_spells(db_spells), 50);

        r.resolve_landing(1000, "Your fist bursts into flame.");
        r.resolve_landing(1000, "Your skin hardens.");
        assert_eq!(r.active_buffs(1000).len(), 2);
        // A day later the timed one has lapsed and the "permanent" one has
        // not: formula 50 is 432,000 seconds, which outlasts any session.
        assert_eq!(r.active_buffs(1000 + 86_400).len(), 1);

        r.clear_on_death();
        assert!(
            r.active_buffs(1000).is_empty(),
            "death must take everything, permanent included"
        );
    }

    /// Candidates that are merely different ranks of one spell are not really
    /// ambiguous: the displayed name is correct either way.
    #[test]
    fn same_name_ranks_report_high_confidence() {
        let db = db_with(vec![
            spell(
                10,
                "Spirit of Wolf",
                "Your feet move faster.",
                "You slow down.",
                3,
                360,
            ),
            spell(
                20,
                "Spirit of Wolf",
                "Your feet move faster.",
                "You slow down.",
                3,
                360,
            ),
        ]);
        let mut r = SpellResolver::new(db, 50);
        r.resolve_landing(1000, "Your feet move faster.");
        let b = &r.active_buffs(1000)[0];
        assert_eq!(b.name, "Spirit of Wolf");
        assert!(b.confidence >= 0.8, "confidence was {}", b.confidence);
    }

    /// A cast outside the timing window no longer counts as *temporal*
    /// evidence, but it has permanently taught us what this character casts,
    /// so the candidate is still chosen — at repertoire confidence, not
    /// recent-cast confidence.
    #[test]
    fn stale_cast_still_informs_via_repertoire() {
        let db = db_with(vec![
            spell(1, "Alpha", "Text.", "Gone.", 3, 10),
            spell(2, "Beta", "Text.", "Gone.", 3, 10),
        ]);
        let mut r = SpellResolver::new(db, 50);
        r.note_cast(100, "Beta"); // far outside the association window
        let id = r.resolve_landing(1000, "Text.").unwrap();
        assert_eq!(id, 2, "repertoire should still identify the spell");
        let c = r.active_buffs(1000)[0].confidence;
        assert!(
            (0.85..0.95).contains(&c),
            "expected repertoire confidence, got {c}"
        );
    }

    /// A fresh cast must outrank repertoire when the two disagree.
    #[test]
    fn recent_cast_outranks_repertoire() {
        let db = db_with(vec![
            spell(1, "Alpha", "Text.", "Gone.", 3, 10),
            spell(2, "Beta", "Text.", "Gone.", 3, 10),
        ]);
        let mut r = SpellResolver::new(db, 50);
        r.note_cast(100, "Beta"); // learned long ago
        r.note_cast(995, "Alpha"); // just cast
        assert_eq!(r.resolve_landing(1000, "Text."), Some(1));
    }

    /// One string can be a landing message for one spell and a wear-off
    /// message for another. "You slow down." lands a snare, and also ends
    /// Spirit of Wolf. Read as a landing, a speed buff expiring becomes a
    /// phantom debuff the player never had.
    #[test]
    fn a_fade_of_an_active_buff_is_not_read_as_a_landing() {
        let db = db_with(vec![
            spell(
                1,
                "Spirit of Wolf",
                "Your feet move faster.",
                "You slow down.",
                3,
                360,
            ),
            debuff(
                2,
                "Languid Pace",
                "You slow down.",
                "You speed back up.",
                3,
                100,
            ),
        ]);
        let mut r = SpellResolver::new(db, 50);
        r.note_known_spell("Languid Pace"); // in the spellbook, so repertoire would favour it

        r.resolve_landing(1000, "Your feet move faster.");
        assert_eq!(r.active_buffs(1000).len(), 1);

        // Speed buff expires.
        assert!(
            r.is_fade_of_active("You slow down."),
            "should be recognised as ending the active buff"
        );
        r.resolve_fade("You slow down.");
        assert!(r.active_buffs(1000).is_empty(), "the buff is gone");
        assert!(
            r.active_debuffs(1000).is_empty(),
            "and no snare was invented from its expiry"
        );
    }

    /// With nothing to expire, the same text is a genuine landing.
    #[test]
    fn the_same_text_still_lands_when_nothing_is_active() {
        let db = db_with(vec![
            spell(
                1,
                "Spirit of Wolf",
                "Your feet move faster.",
                "You slow down.",
                3,
                360,
            ),
            debuff(
                2,
                "Languid Pace",
                "You slow down.",
                "You speed back up.",
                3,
                100,
            ),
        ]);
        let mut r = SpellResolver::new(db, 50);
        assert!(!r.is_fade_of_active("You slow down."));
        r.resolve_landing(1000, "You slow down.");
        assert_eq!(
            r.active_debuffs(1000).len(),
            1,
            "a real snare still registers"
        );
    }

    #[test]
    fn fade_text_removes_active_buff() {
        let db = db_with(vec![spell(
            1,
            "Spirit of Wolf",
            "Your feet move faster.",
            "You slow down.",
            3,
            360,
        )]);
        let mut r = SpellResolver::new(db, 50);
        r.resolve_landing(1000, "Your feet move faster.");
        assert_eq!(r.active_buffs(1000).len(), 1);
        assert_eq!(r.resolve_fade("You slow down."), Some(1));
        assert!(r.active_buffs(1000).is_empty());
    }

    #[test]
    fn buffs_expire_on_their_own() {
        let db = db_with(vec![spell(1, "Short", "On.", "Off.", 15, 1)]);
        let mut r = SpellResolver::new(db, 50);
        r.resolve_landing(1000, "On.");
        assert_eq!(r.active_buffs(1000).len(), 1);
        // 1 tick = 6 seconds
        assert!(r.active_buffs(1007).is_empty());
    }

    /// A bard song running under `/melody` re-lands every few seconds. It must
    /// be classified as an aura, not shown as a countdown.
    #[test]
    fn a_pulsing_song_is_detected_as_an_aura() {
        let db = db_with(vec![spell(
            1,
            "Jonthan's Whistling Warsong",
            "You whistle an ancient warsong.",
            "You stop whistling.",
            5,
            2,
        )]);
        let mut r = SpellResolver::new(db, 50);
        r.note_known_spell("Jonthan's Whistling Warsong");
        // Pulses every 6 seconds, as songs do.
        for i in 0..10 {
            r.resolve_landing(1000 + i * 6, "You whistle an ancient warsong.");
        }
        let auras = r.active_auras(1060);
        assert_eq!(auras.len(), 1, "the song should be classified as an aura");
        assert_eq!(auras[0].name, "Jonthan's Whistling Warsong");
        assert!(auras[0].refreshes >= 3);
    }

    /// A buff cast once is not an aura, however long it lasts.
    #[test]
    fn a_single_cast_buff_is_not_an_aura() {
        let db = db_with(vec![spell(
            1,
            "Spirit of Wolf",
            "Your feet move faster.",
            "You slow down.",
            3,
            360,
        )]);
        let mut r = SpellResolver::new(db, 50);
        r.resolve_landing(1000, "Your feet move faster.");
        assert!(r.active_auras(1000).is_empty());
        assert_eq!(r.active_buffs(1000).len(), 1);
    }

    /// Re-applying a long buff occasionally must not be mistaken for an aura.
    #[test]
    fn an_occasionally_recast_buff_is_not_an_aura() {
        let db = db_with(vec![spell(
            1,
            "Regeneration",
            "Your wounds begin to heal.",
            "Your wounds stop healing.",
            10,
            205,
        )]);
        let mut r = SpellResolver::new(db, 50);
        r.note_known_spell("Regeneration");
        // Recast every 15 minutes.
        for i in 0..4 {
            r.resolve_landing(1000 + i * 900, "Your wounds begin to heal.");
        }
        assert!(
            r.active_auras(3700).is_empty(),
            "a 15-minute recast is not a pulsing aura"
        );
    }

    /// Auras and timed buffs are reported through separate lists so one cannot
    /// bury the other.
    #[test]
    fn auras_are_excluded_from_the_timed_buff_list() {
        let db = db_with(vec![
            spell(1, "Warsong", "You whistle.", "You stop.", 5, 2),
            spell(
                2,
                "Spirit of Wolf",
                "Your feet move faster.",
                "You slow down.",
                3,
                360,
            ),
        ]);
        let mut r = SpellResolver::new(db, 50);
        r.note_known_spell("Warsong");
        for i in 0..8 {
            r.resolve_landing(1000 + i * 6, "You whistle.");
        }
        r.resolve_landing(1000, "Your feet move faster.");

        let auras = r.active_auras(1050);
        let buffs = r.active_buffs(1050);
        assert_eq!(auras.len(), 1);
        assert_eq!(auras[0].name, "Warsong");
        assert!(
            buffs.iter().all(|b| b.name != "Warsong"),
            "an aura must not also appear among timed buffs"
        );
        assert!(buffs.iter().any(|b| b.name == "Spirit of Wolf"));
    }

    /// Hostile spells land on you repeatedly too. Pulsing alone must not
    /// classify a mob's nuke as one of your auras.
    #[test]
    fn an_enemy_spell_pulsing_on_you_is_not_your_aura() {
        let db = db_with(vec![spell(
            1,
            "Dry Bone Fire Burst",
            "You feel your skin smolder.",
            "The smoldering fades.",
            5,
            2,
        )]);
        let mut r = SpellResolver::new(db, 50);
        // Never taught to the character: it is a mob's spell.
        for i in 0..20 {
            r.resolve_landing(1000 + i * 6, "You feel your skin smolder.");
        }
        // Query inside the effect's own duration (2 ticks from the last pulse
        // at t=1114), or it has simply expired and proves nothing.
        let now = 1120;
        assert!(
            r.active_auras(now).is_empty(),
            "a spell the character cannot cast is not their aura"
        );
        assert_eq!(
            r.active_buffs(now).len(),
            1,
            "it should still be tracked as an effect on you"
        );
    }

    /// Detrimental effects must never be listed as buffs. The flag comes from
    /// the game's own data, not from guessing at names.
    /// Instant spells have no duration. Tracking them as active effects leaves
    /// them on screen permanently — a real log accumulated 55 "debuffs" that
    /// were mostly one-shot nukes.
    #[test]
    fn instant_effects_are_not_tracked_as_lasting() {
        let db = db_with(vec![
            // Formula 0 means instant.
            debuff(1, "Bolt of Flame", "You are engulfed in flame.", "", 0, 0),
            debuff(2, "Root", "You are rooted.", "You are free.", 3, 100),
        ]);
        let mut r = SpellResolver::new(db, 50);
        assert_eq!(
            r.resolve_landing(1000, "You are engulfed in flame."),
            Some(1),
            "the line is still recognised"
        );
        r.resolve_landing(1000, "You are rooted.");

        let debuffs = r.active_debuffs(1000);
        assert_eq!(debuffs.len(), 1, "only the lasting effect is tracked");
        assert_eq!(debuffs[0].name, "Root");
    }

    #[test]
    fn debuffs_are_reported_separately_from_buffs() {
        let db = db_with(vec![
            spell(
                1,
                "Spirit of Wolf",
                "Your feet move faster.",
                "You slow down.",
                3,
                360,
            ),
            debuff(
                2,
                "Dooming Darkness",
                "You are covered in darkness.",
                "The darkness fades.",
                3,
                100,
            ),
            debuff(
                3,
                "Shadow Vortex",
                "A vortex of shadows surrounds you.",
                "The vortex fades.",
                3,
                100,
            ),
        ]);
        let mut r = SpellResolver::new(db, 50);
        r.resolve_landing(1000, "Your feet move faster.");
        r.resolve_landing(1000, "You are covered in darkness.");
        r.resolve_landing(1000, "A vortex of shadows surrounds you.");

        let buffs = r.active_buffs(1000);
        let debuffs = r.active_debuffs(1000);
        assert_eq!(buffs.len(), 1);
        assert_eq!(buffs[0].name, "Spirit of Wolf");
        assert_eq!(debuffs.len(), 2);
        assert!(debuffs.iter().any(|b| b.name == "Dooming Darkness"));
        assert!(debuffs.iter().any(|b| b.name == "Shadow Vortex"));
        assert!(
            buffs.iter().all(|b| b.beneficial),
            "no detrimental effect may appear among buffs"
        );
    }

    /// A detrimental effect that pulses is still a debuff, never an aura —
    /// even if the character happens to know the spell.
    #[test]
    fn a_pulsing_debuff_is_not_an_aura() {
        let db = db_with(vec![debuff(
            1,
            "Clinging Darkness",
            "You are covered in darkness.",
            "The darkness fades.",
            5,
            20,
        )]);
        let mut r = SpellResolver::new(db, 50);
        r.note_known_spell("Clinging Darkness");
        for i in 0..10 {
            r.resolve_landing(1000 + i * 6, "You are covered in darkness.");
        }
        assert!(
            r.active_auras(1060).is_empty(),
            "a detrimental effect is never one of your auras"
        );
        assert_eq!(r.active_debuffs(1060).len(), 1);
    }

    /// Detection needs a few pulses before it recognises an aura, so a song
    /// spends its first seconds in the timer list. A mark settles it at once.
    /// Colouring a heal-over-time like any other buff hides what it is doing.
    /// The category comes from the game's own data, not from the name.
    #[test]
    fn effect_category_distinguishes_heals_from_buffs() {
        let mut heal = spell(
            1,
            "Efflorescing Heal",
            "The heal effloresces.",
            "It fades.",
            10,
            100,
        );
        heal.category_id = 5;
        let mut buff = spell(
            2,
            "Spirit of Wolf",
            "Your feet move faster.",
            "You slow down.",
            3,
            360,
        );
        buff.category_id = 1;
        assert_eq!(heal.category(), EffectCategory::Heal);
        assert_eq!(buff.category(), EffectCategory::Buff);

        let mut r = SpellResolver::new(db_with(vec![heal, buff]), 50);
        r.resolve_landing(1000, "The heal effloresces.");
        r.resolve_landing(1000, "Your feet move faster.");
        let buffs = r.active_buffs(1000);
        let h = buffs
            .iter()
            .find(|b| b.name == "Efflorescing Heal")
            .unwrap();
        let w = buffs.iter().find(|b| b.name == "Spirit of Wolf").unwrap();
        assert_eq!(h.category, EffectCategory::Heal);
        assert_eq!(w.category, EffectCategory::Buff);
    }

    #[test]
    fn a_marked_aura_is_classified_immediately() {
        let db = db_with(vec![spell(
            1,
            "Jonthan's Whistling Warsong",
            "You whistle an ancient warsong.",
            "You stop whistling.",
            5,
            2,
        )]);
        let mut r = SpellResolver::new(db, 50);
        r.set_mark("Jonthan's Whistling Warsong", Some(BuffMark::Aura));

        // One landing, no pulse history at all.
        r.resolve_landing(1000, "You whistle an ancient warsong.");
        assert_eq!(
            r.active_auras(1000).len(),
            1,
            "marked aura from the first land"
        );
        assert!(r.active_buffs(1000).is_empty(), "and not among timed buffs");
    }

    /// A mark applies to something already active, not only to future lands.
    #[test]
    fn marking_reclassifies_an_active_buff() {
        let db = db_with(vec![spell(
            1,
            "Some Song",
            "It begins.",
            "It ends.",
            3,
            100,
        )]);
        let mut r = SpellResolver::new(db, 50);
        r.resolve_landing(1000, "It begins.");
        assert_eq!(r.active_buffs(1000).len(), 1);

        r.set_mark("Some Song", Some(BuffMark::Aura));
        assert_eq!(
            r.active_auras(1000).len(),
            1,
            "moves without waiting to re-land"
        );
        assert!(r.active_buffs(1000).is_empty());
    }

    #[test]
    fn marking_permanent_stops_the_countdown() {
        let db = db_with(vec![spell(1, "Short Buff", "On.", "Off.", 3, 10)]);
        let mut r = SpellResolver::new(db, 50);
        r.set_mark("Short Buff", Some(BuffMark::Permanent));
        r.resolve_landing(1000, "On.");
        let b = &r.active_buffs(1000)[0];
        assert!(b.permanent);
        assert_eq!(b.mark, Some(BuffMark::Permanent));
    }

    /// Clearing a mark hands classification back to detection.
    #[test]
    fn clearing_a_mark_restores_detection() {
        let db = db_with(vec![spell(
            1,
            "Some Song",
            "It begins.",
            "It ends.",
            3,
            100,
        )]);
        let mut r = SpellResolver::new(db, 50);
        r.set_mark("Some Song", Some(BuffMark::Aura));
        r.resolve_landing(1000, "It begins.");
        assert_eq!(r.active_auras(1000).len(), 1);

        r.set_mark("Some Song", None);
        assert!(r.active_auras(1000).is_empty(), "no pulses, so not an aura");
        assert_eq!(r.active_buffs(1000).len(), 1);
        assert!(r.marks().is_empty());
    }

    /// A permanent mark must override behavioural aura detection, not lose to it.
    #[test]
    fn a_permanent_mark_beats_pulse_detection() {
        let db = db_with(vec![spell(1, "Pulsy", "Tick.", "Done.", 5, 2)]);
        let mut r = SpellResolver::new(db, 50);
        r.note_known_spell("Pulsy");
        r.set_mark("Pulsy", Some(BuffMark::Permanent));
        for i in 0..10 {
            r.resolve_landing(1000 + i * 6, "Tick.");
        }
        assert!(
            r.active_auras(1060).is_empty(),
            "declaration wins over pulsing"
        );
        assert_eq!(r.active_buffs(1060).len(), 1);
    }

    #[test]
    fn recast_refreshes_rather_than_stacks() {
        let db = db_with(vec![spell(
            1,
            "Spirit of Wolf",
            "Your feet move faster.",
            "You slow down.",
            3,
            360,
        )]);
        let mut r = SpellResolver::new(db, 50);
        r.resolve_landing(1000, "Your feet move faster.");
        r.resolve_landing(1500, "Your feet move faster.");
        let buffs = r.active_buffs(1500);
        assert_eq!(buffs.len(), 1);
        assert_eq!(buffs[0].expires, Some(1500 + 2160));
    }

    #[test]
    fn level_affects_duration() {
        let db = db_with(vec![spell(1, "Regen", "On.", "Off.", 10, 205)]);
        let mut r = SpellResolver::new(db, 10);
        r.resolve_landing(0, "On.");
        // level 10: 10*3+10 = 40 ticks = 240s
        assert_eq!(r.active_buffs(0)[0].expires, Some(240));
        r.clear_buffs();
        r.set_level(50);
        r.resolve_landing(0, "On.");
        // level 50: 160 ticks = 960s
        assert_eq!(r.active_buffs(0)[0].expires, Some(960));
    }

    #[test]
    fn unknown_text_resolves_to_nothing() {
        let mut r = SpellResolver::new(SpellDb::empty(), 50);
        assert_eq!(r.resolve_landing(0, "Something unrelated."), None);
        assert_eq!(r.resolve_fade("Also unrelated."), None);
    }

    /// Ranks share a landing phrase, and the candidates used to come back in
    /// whatever order the HashMap held them, so the same fight named a
    /// different rank on each daemon start.
    #[test]
    fn candidates_for_a_shared_phrase_come_back_in_id_order() {
        for _ in 0..8 {
            let mut m = HashMap::new();
            for (id, name) in [(907, "Harm Touch XXXIII"), (88, "Harm Touch XII"), (412, "Harm Touch XX")] {
                let mut s = spell(id, name, "", "", 0, 0);
                s.landed_on_other = Some(" is struck by a harmful touch.".into());
                m.insert(id, s);
            }
            let db = SpellDb::from_spells(m);
            assert_eq!(db.candidates_for_landing_on_other(" is struck by a harmful touch."), [88, 412, 907]);
        }
    }

    #[test]
    fn the_rank_you_cast_is_told_apart_from_its_siblings() {
        let mut r = SpellResolver::new(SpellDb::from_spells(HashMap::new()), 50);
        r.note_cast(1000, "Harm Touch XII");
        assert!(r.cast_recently(1001, "Harm Touch XXXIII"), "a sibling rank still counts as cast");
        assert!(r.cast_this_rank_recently(1001, "Harm Touch XII"));
        assert!(!r.cast_this_rank_recently(1001, "Harm Touch XXXIII"));
    }

    /// The 2026-11 client file inserted a column between the class levels and
    /// the category. Read by the old positions, every charm, proc grant and
    /// linked spell came back empty without a word.
    #[test]
    fn a_spell_file_with_an_inserted_column_still_reads_its_effects() {
        let row = |extra: bool| {
            let mut f = vec!["0".to_string(); 173];
            f[0] = "750".into();
            f[1] = "Solon's Bewitching Bravura".into();
            f[146] = "4".into();
            f[172] = "1|22|1|0|100|51$2|85|6908|400|100|0".into();
            if extra {
                f.insert(100, "0".into());
            }
            f.join("^")
        };
        for extra in [false, true] {
            let path = std::env::temp_dir().join(format!("eqlparser-spells-{}-{extra}.txt", std::process::id()));
            std::fs::write(&path, row(extra) + "\n").unwrap();
            let spells = SpellDb::load_spells(&path).unwrap();
            std::fs::remove_file(&path).ok();
            let s = &spells[&750];
            assert!(s.is_charm, "charm read with {} fields", 173 + extra as usize);
            assert_eq!(s.procs, [ProcGrant { spell: 6908, on_melee: true }]);
            assert_eq!(s.category_id, 4);
        }
    }
}
