//! The game's own number for an item, out of the two files the client writes
//! it into.
//!
//! Everything else this parser knows about an item keys on its name, which
//! varies by article, by case and by the `+N` grade rolled onto it, and which
//! does not survive a rename. The client states a number beside the name in
//! two places, and a number is the one key that holds still.
//!
//! **`/outputfile inventory`** writes a tab-separated snapshot per character:
//!
//! ```text
//! Location        Name                     ID      Count  Slots
//! Any Slot        Boots of the Long Road +1 177708  1      10
//! Any Slot-Slot1  Empty                     0       0      0
//! ```
//!
//! **`userdata/LF_<Character>_<server>.ini`** is the loot filter, and names
//! its own columns:
//!
//! ```text
//! #ITEM_ID^FILTER_ID^ICON_ID^ITEM_NAME
//! 13408^2^817^Rat Meat
//! ```
//!
//! Of the nine columns across the two files, four describe the game. Location,
//! Count and Slots describe one character's bags on one afternoon, and
//! `FILTER_ID` is that player's own keep-or-ignore preference — dropped here
//! for the same reason character progression is dropped everywhere else.
//!
//! # Held is not the same claim as seen
//!
//! The two files answer different questions, and the difference is the whole
//! reason [`Source`] travels beside the readings rather than being forgotten
//! once the file is open.
//!
//! An inventory row is a character *holding* the item: a `+2` in a bag is
//! proof that a `+2` exists. A loot-filter row is an item that has been on a
//! corpse in front of this character, which is proof the item exists and no
//! proof at all that anyone carried that grade.
//!
//! The accumulator these two feed kept both in one `grades_held` list, and by
//! September 2026 that list claimed 336 items were held at grades nobody has
//! ever carried — `Cloth Veil` at `+1` and `+2`, `Cloth Cord` at `+2` and
//! `+4` — and those claims had already reached the site. So the grades are
//! filed apart: [`Entry::grades_held`] from an inventory, [`Entry::grades_seen`]
//! from a filter, and a consumer that wants the stronger claim asks for the
//! first.
//!
//! # An id does not identify an item on its own
//!
//! The obvious key is the id, and it is not sufficient. An item and its
//! Exaltation augment share one, in the client's own dump:
//!
//! ```text
//! Primary          Fangol +10                  11675
//! Augmentation     Fangol (Exaltation)         11675
//! ```
//!
//! Those are different items and the game gives them the same number. The
//! registry is still keyed on the id, because that is what is unique about a
//! *row* in these files, and the second name is kept in
//! [`Entry::also_known_as`] rather than resolved: a rename and a shared id
//! look identical from here, and this module is not the place that can tell
//! them apart.

use crate::drops::split_grade;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The highest grade the game rolls, and so the highest a trailing `+N` can
/// mean.
///
/// Beyond it the suffix is part of the name. Without the bound, an item
/// legitimately called `... +12` would be filed as a twelfth grade of an item
/// that does not exist, and its real name would never appear.
const MAX_GRADE: u32 = 10;

/// Which of the client's two files a reading came out of.
///
/// Carried rather than discarded because it decides what the reading is
/// evidence *of* — see the module note on held against seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `<Character>_<server>-Inventory.txt`, from `/outputfile inventory`.
    Inventory,
    /// `userdata/LF_<Character>_<server>.ini`, the loot filter.
    LootFilter,
}

impl Source {
    /// Which kind a file is, by the client's own naming convention.
    ///
    /// The convention is the only thing that distinguishes them — the two
    /// formats share no header, no delimiter and no extension — so a file
    /// that matches neither is not guessed at.
    pub fn of_filename(name: &str) -> Option<Source> {
        let n = name.rsplit('/').next().unwrap_or(name);
        if n.starts_with("LF_") && n.ends_with(".ini") {
            Some(Source::LootFilter)
        } else if n.ends_with("-Inventory.txt") {
            Some(Source::Inventory)
        } else {
            None
        }
    }

    /// The word this kind is recorded under.
    pub fn label(self) -> &'static str {
        match self {
            Source::Inventory => "inventory",
            Source::LootFilter => "loot-filter",
        }
    }
}

/// The character and server a source file belongs to, where its name says so.
///
/// Both files carry them in the filename and nowhere in the contents, so a
/// file renamed by hand loses them. That is why they are optional rather than
/// a parse failure: the ids in the file are still good.
pub fn who(name: &str) -> Option<(String, String)> {
    let n = name.rsplit('/').next().unwrap_or(name);
    let stem = if let Some(rest) = n.strip_prefix("LF_") {
        rest.strip_suffix(".ini")?
    } else {
        n.strip_suffix("-Inventory.txt")?
    };
    let (character, server) = stem.split_once('_')?;
    if character.is_empty() || server.is_empty() {
        return None;
    }
    Some((character.to_string(), server.to_string()))
}

/// One item, as one line of one client file states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The name with any grade suffix removed, spelled as the client wrote it.
    pub item: String,
    /// The game's number for it.
    pub item_id: u32,
    /// The `+N` on the name, or 0 for none. What this is *evidence of*
    /// depends on the [`Source`] it came from.
    pub grade: u32,
    /// The client's flat icon number. Stated only by the loot filter; no
    /// inventory dump carries one.
    pub icon_id: Option<u32>,
}

/// Split a name into its base and grade, bounded by [`MAX_GRADE`].
fn graded(name: &str) -> (String, u32) {
    match split_grade(name) {
        (base, Some(g)) if (1..=MAX_GRADE).contains(&g) && !base.is_empty() => {
            (base.to_string(), g)
        }
        _ => (name.trim().to_string(), 0),
    }
}

/// A row naming no item. The client writes one per empty bag slot, and there
/// are far more of those than there are items.
fn empty(name: &str) -> bool {
    name.is_empty() || name == "Empty"
}

/// Read a `/outputfile inventory` dump.
///
/// Fails only when the file is not one: an unreadable *row* is skipped, since
/// a dump is a snapshot and a single malformed line is not a reason to lose
/// the eight hundred beside it.
pub fn read_inventory(text: &str) -> anyhow::Result<Vec<Reading>> {
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    let header: Vec<&str> = lines
        .next()
        .map(|h| h.split('\t').map(str::trim).collect())
        .unwrap_or_default();
    let col = |want: &str| header.iter().position(|h| h.eq_ignore_ascii_case(want));
    let (Some(name_at), Some(id_at)) = (col("Name"), col("ID")) else {
        anyhow::bail!("no Name and ID columns; this is not an inventory dump");
    };

    let mut out = Vec::new();
    for line in lines {
        let cells: Vec<&str> = line.split('\t').collect();
        let (Some(name), Some(id)) = (cells.get(name_at), cells.get(id_at)) else {
            continue;
        };
        let name = name.trim();
        let Ok(item_id) = id.trim().parse::<u32>() else {
            continue;
        };
        if empty(name) || item_id == 0 {
            continue;
        }
        let (item, grade) = graded(name);
        if item.is_empty() {
            continue;
        }
        out.push(Reading {
            item,
            item_id,
            grade,
            icon_id: None,
        });
    }
    Ok(out)
}

/// Read a `LF_*.ini` loot filter.
///
/// Fails when it holds no item rows at all, which is how a file that is not a
/// loot filter presents: every line is a comment or unparseable, and taking
/// that as "the player has seen nothing" would be a silently empty source.
pub fn read_loot_filter(text: &str) -> anyhow::Result<Vec<Reading>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('^').collect();
        if parts.len() < 4 {
            continue;
        }
        let Ok(item_id) = parts[0].trim().parse::<u32>() else {
            continue;
        };
        // The name is everything after the third caret, rejoined: a `^` is not
        // escaped by the client, so an item carrying one would otherwise be
        // truncated at it.
        let name = parts[3..].join("^");
        let name = name.trim();
        if item_id == 0 || empty(name) {
            continue;
        }
        let (item, grade) = graded(name);
        if item.is_empty() {
            continue;
        }
        out.push(Reading {
            item,
            item_id,
            grade,
            icon_id: parts[2].trim().parse::<u32>().ok().filter(|i| *i > 0),
        });
    }
    if out.is_empty() {
        anyhow::bail!("no item rows; this is not a loot filter");
    }
    Ok(out)
}

/// Read whichever kind the file is.
pub fn read(kind: Source, text: &str) -> anyhow::Result<Vec<Reading>> {
    match kind {
        Source::Inventory => read_inventory(text),
        Source::LootFilter => read_loot_filter(text),
    }
}

// -- The accumulator ---------------------------------------------------------

/// What one item is known to be, across every dump ever folded in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entry {
    pub item_id: u32,
    /// The first name this id was seen under, which stays canonical.
    pub item: String,
    /// Grades somebody has been seen *holding*. Evidence that those grades
    /// exist, and nothing about the ones between.
    #[serde(default)]
    pub grades_held: Vec<u32>,
    /// Grades seen on a corpse, from a loot filter. A weaker claim than
    /// `grades_held` and kept apart from it — see the module note.
    #[serde(default)]
    pub grades_seen: Vec<u32>,
    /// Other names this id has been written under: a rename, or the item's
    /// Exaltation augment sharing its number.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_known_as: Vec<String>,
    /// The client's icon number, where a loot filter has stated one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_id: Option<u32>,
    /// The files this id has been read out of, by name.
    #[serde(default)]
    pub sources: Vec<String>,
}

impl Entry {
    /// Every name this id has been written under, canonical first.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.item.as_str()).chain(self.also_known_as.iter().map(String::as_str))
    }
}

/// One fold of one version of one file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Fold {
    pub sha: String,
    pub rows: usize,
    pub new_ids: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// What is known about a source file that has been folded in.
///
/// `folds` keeps one entry per distinct version rather than one per file.
/// Keying on the filename alone meant a second fold overwrote the first and
/// took `new_ids` — the only record of what a capture contributed — with it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Folded {
    pub file: String,
    pub kind: String,
    pub rows: usize,
    pub sha: String,
    /// What this file has contributed in total, across every version folded.
    pub new_ids: usize,
    #[serde(default)]
    pub folds: Vec<Fold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

/// Whether a file on disk has anything new to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Never folded.
    New,
    /// Folded before, but not this version. Carries a reason for a human.
    Changed(String),
    /// This exact version is already in.
    Unchanged,
}

impl Change {
    pub fn worth_folding(&self) -> bool {
        !matches!(self, Change::Unchanged)
    }
}

/// The accumulated record: every id ever read, and what each file contributed.
///
/// **Nothing here is ever removed.** A dump says what a character is carrying
/// at that moment, so rebuilding from the dumps alone means selling an item
/// deletes its id. That is not a hypothetical: re-folding only the files
/// currently on disk in September 2026 yields 1,472 ids against the 1,986 on
/// file, because two of the five sources had been overwritten since. An id is
/// a permanent fact about the game, so this grows and never shrinks.
///
/// Which also means it is not derived data. It cannot be rebuilt by re-reading
/// anything, so it belongs beside the hand-made files rather than in the
/// rebuildable history database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub note: String,
    /// Which game these ids belong to, as [`crate::game::Game::slug`] writes
    /// it. One install writes one game's files, so this is a property of the
    /// record rather than of a row — and a consumer that cannot tell which
    /// game a pile of ids describes can never correct it afterwards.
    pub game: String,
    pub key: String,
    #[serde(default)]
    pub sources: Vec<Folded>,
    #[serde(default)]
    pub items: Vec<Entry>,
    /// A name to id shortcut, for the readers that hold a name and want the
    /// number. Derived from [`Entry::item`] and rebuilt on every fold, so it
    /// is never edited and never stale.
    ///
    /// Only where the name is unambiguous. A name the game gives to more than
    /// one item has no answer here, and answering with either of them would
    /// be worse than not answering — `Bixie Stinger` is 14029 in one zone and
    /// 20994 in another, and picking one silently files an item under its
    /// twin.
    #[serde(default)]
    pub by_name: BTreeMap<String, u32>,
    /// The names that carry more than one id, and all of them. The other half
    /// of `by_name`: a reader that misses there can look here and find out
    /// that the miss was a real ambiguity rather than an absence.
    ///
    /// Both are built from the canonical name alone, not from
    /// [`Entry::also_known_as`]. An alias is the *same* id written a second
    /// way, so folding aliases in would report an ambiguity where there is
    /// only a rename.
    #[serde(default)]
    pub ambiguous_names: BTreeMap<String, Vec<u32>>,
}

const NOTE: &str = "Item names paired with the game's own item id, accumulated from inventory \
dumps and loot filters. Keyed on item_id, which is unique and grade independent. grades_held \
records grades actually seen held; grades_seen records grades a loot filter saw on a corpse, \
which is a weaker claim and is kept apart from it. Nothing is ever removed.";

impl Default for Registry {
    fn default() -> Self {
        Registry {
            note: NOTE.to_string(),
            // Not guessed. The daemon knows which install it is reading and
            // sets it; a record that claimed a game nobody had established
            // would be believed by whoever read it next.
            game: crate::game::Game::Unknown.slug().to_string(),
            key: "item_id".to_string(),
            sources: Vec::new(),
            items: Vec::new(),
            by_name: BTreeMap::new(),
            ambiguous_names: BTreeMap::new(),
        }
    }
}

/// What a fold changed, for the person or log watching it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldStats {
    pub rows: usize,
    pub added: usize,
    pub updated: usize,
    /// Ids that gained a second name.
    pub renamed: usize,
    /// Things worth saying out loud: a disagreement recorded rather than
    /// resolved.
    pub notes: Vec<String>,
}

impl Registry {
    /// Whether this version of this file has already been folded.
    ///
    /// The row count is checked as well as the digest so that the reason can
    /// name what changed. A count alone would miss an edit in place — flipping
    /// a filter preference or fixing a name leaves the count identical, and the
    /// file would then be skipped forever.
    pub fn change(&self, file: &str, rows: usize, sha: &str) -> Change {
        let Some(prior) = self.sources.iter().find(|s| s.file == file) else {
            return Change::New;
        };
        if prior.sha.is_empty() {
            return Change::Changed("no digest on file, re-folding once".into());
        }
        if prior.sha == sha {
            return Change::Unchanged;
        }
        if prior.rows != rows {
            Change::Changed(format!("was {} rows, now {rows}", prior.rows))
        } else {
            Change::Changed("same row count, contents edited".into())
        }
    }

    /// Fold one file's readings in. Adds and updates; never removes.
    pub fn fold(
        &mut self,
        file: &str,
        kind: Source,
        rows: &[Reading],
        sha: &str,
        today: &str,
    ) -> FoldStats {
        let mut by_id: BTreeMap<u32, Entry> =
            std::mem::take(&mut self.items).into_iter().map(|e| (e.item_id, e)).collect();
        let mut stats = FoldStats {
            rows: rows.len(),
            ..Default::default()
        };

        for r in rows {
            let Some(entry) = by_id.get_mut(&r.item_id) else {
                let mut e = Entry {
                    item_id: r.item_id,
                    item: r.item.clone(),
                    icon_id: r.icon_id,
                    sources: vec![file.to_string()],
                    ..Default::default()
                };
                push_grade(&mut e, kind, r.grade);
                by_id.insert(r.item_id, e);
                stats.added += 1;
                continue;
            };
            let mut touched = false;
            if let Some(icon) = r.icon_id {
                if entry.icon_id != Some(icon) {
                    if let Some(had) = entry.icon_id {
                        stats.notes.push(format!(
                            "id {} had icon {had} and reads {icon} in {file} — keeping the newer",
                            r.item_id
                        ));
                    }
                    entry.icon_id = Some(icon);
                    touched = true;
                }
            }
            if r.item != entry.item && !entry.also_known_as.contains(&r.item) {
                stats.notes.push(format!(
                    "id {} is {:?} on file and {:?} in {file} — keeping both",
                    r.item_id, entry.item, r.item
                ));
                entry.also_known_as.push(r.item.clone());
                stats.renamed += 1;
                touched = true;
            }
            touched |= push_grade(entry, kind, r.grade);
            if !entry.sources.iter().any(|s| s == file) {
                entry.sources.push(file.to_string());
                touched = true;
            }
            stats.updated += usize::from(touched);
        }

        for e in by_id.values_mut() {
            e.grades_held.sort_unstable();
            e.grades_held.dedup();
            e.grades_seen.sort_unstable();
            e.grades_seen.dedup();
            e.sources.sort();
            e.sources.dedup();
        }
        self.items = by_id.into_values().collect();
        self.reindex();
        self.record_fold(file, kind, rows.len(), sha, stats.added, today);
        stats
    }

    /// Rebuild the derived name maps. Cheap, and called wherever `items`
    /// changes, so there is no state in which they disagree with it.
    pub fn reindex(&mut self) {
        let mut names: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
        for e in &self.items {
            names.entry(e.item.as_str()).or_default().push(e.item_id);
        }
        self.by_name = names
            .iter()
            .filter(|(_, ids)| ids.len() == 1)
            .map(|(n, ids)| (n.to_string(), ids[0]))
            .collect();
        self.ambiguous_names = names
            .iter()
            .filter(|(_, ids)| ids.len() > 1)
            .map(|(n, ids)| {
                let mut ids = ids.clone();
                ids.sort_unstable();
                (n.to_string(), ids)
            })
            .collect();
    }

    /// Note that this version of this file has been folded.
    fn record_fold(
        &mut self,
        file: &str,
        kind: Source,
        rows: usize,
        sha: &str,
        fresh: usize,
        today: &str,
    ) {
        let prior = self.sources.iter().position(|s| s.file == file);
        let mut folds = prior
            .map(|i| self.sources[i].folds.clone())
            .unwrap_or_default();
        // A version already on file appends nothing, so re-folding an
        // unchanged dump is the no-op here that it is everywhere else.
        if !folds.iter().any(|f| f.sha == sha) {
            folds.push(Fold {
                sha: sha.to_string(),
                rows,
                new_ids: fresh,
                at: Some(today.to_string()),
            });
        }
        let (character, server) = match who(file) {
            Some((c, s)) => (Some(c), Some(s)),
            None => (None, None),
        };
        let entry = Folded {
            file: file.to_string(),
            kind: kind.label().to_string(),
            rows,
            sha: sha.to_string(),
            new_ids: folds.iter().map(|f| f.new_ids).sum(),
            folds,
            character,
            server,
        };
        match prior {
            Some(i) => self.sources[i] = entry,
            None => self.sources.push(entry),
        }
        self.sources.sort_by(|a, b| a.file.cmp(&b.file));
    }

    /// Say which game this record describes, without overwriting an answer
    /// already on file with a worse one.
    pub fn declare_game(&mut self, game: crate::game::Game) {
        if game != crate::game::Game::Unknown || self.game.is_empty() {
            self.game = game.slug().to_string();
        }
    }

    /// Ids that carry more than one name, and the names.
    ///
    /// Reported rather than resolved: an item sharing its number with its
    /// Exaltation augment and an item that was renamed look the same from
    /// here.
    pub fn shared_ids(&self) -> Vec<(u32, Vec<String>)> {
        self.items
            .iter()
            .filter(|e| !e.also_known_as.is_empty())
            .map(|e| (e.item_id, e.names().map(str::to_string).collect()))
            .collect()
    }
}

/// File a grade under the claim its source actually supports. Returns whether
/// anything was added.
fn push_grade(entry: &mut Entry, kind: Source, grade: u32) -> bool {
    let list = match kind {
        Source::Inventory => &mut entry.grades_held,
        Source::LootFilter => &mut entry.grades_seen,
    };
    if list.contains(&grade) {
        return false;
    }
    list.push(grade);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "Location\tName\tID\tCount\tSlots\r\n\
                        Any Slot\tBoots of the Long Road +1\t177708\t1\t10\r\n\
                        Any Slot-Slot1\tEmpty\t0\t0\t0\r\n\
                        Ear\tDiamondine Earring +5\t10165\t1\t10\r\n";

    const FILTER: &str = "#ITEM_ID^FILTER_ID^ICON_ID^ITEM_NAME\r\n\
                          13408^2^817^Rat Meat\r\n\
                          5040^4^768^Rusty Mining Pick +2\r\n";

    #[test]
    fn an_inventory_dump_gives_name_id_and_grade() {
        let rows = read_inventory(DUMP).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].item, "Boots of the Long Road");
        assert_eq!(rows[0].item_id, 177708);
        assert_eq!(rows[0].grade, 1);
        assert_eq!(rows[0].icon_id, None);
    }

    #[test]
    fn empty_bag_slots_are_not_items() {
        assert!(read_inventory(DUMP).unwrap().iter().all(|r| r.item != "Empty"));
    }

    #[test]
    fn a_loot_filter_gives_an_icon_and_drops_the_preference() {
        let rows = read_loot_filter(FILTER).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].item, "Rusty Mining Pick");
        assert_eq!(rows[1].grade, 2);
        assert_eq!(rows[1].icon_id, Some(768));
    }

    #[test]
    fn a_file_of_the_wrong_kind_fails_rather_than_reading_empty() {
        assert!(read_inventory("hello\r\nworld\r\n").is_err());
        assert!(read_loot_filter("# nothing but a comment\r\n").is_err());
    }

    #[test]
    fn the_kind_comes_from_the_client_s_own_naming() {
        assert_eq!(
            Source::of_filename("LF_Tharn_freeport.ini"),
            Some(Source::LootFilter)
        );
        assert_eq!(
            Source::of_filename("/games/eq/Tharn_halas-Inventory.txt"),
            Some(Source::Inventory)
        );
        assert_eq!(Source::of_filename("eqlog_Tharn_freeport.txt"), None);
        assert_eq!(
            who("LF_Tharn_freeport.ini"),
            Some(("Tharn".into(), "freeport".into()))
        );
        assert_eq!(
            who("Tharn_halas-Inventory.txt"),
            Some(("Tharn".into(), "halas".into()))
        );
    }

    #[test]
    fn a_grade_beyond_the_game_s_range_is_part_of_the_name() {
        let rows = read_loot_filter("1^1^500^Potion +12\r\n").unwrap();
        assert_eq!(rows[0].item, "Potion +12");
        assert_eq!(rows[0].grade, 0);
    }

    /// The 336-row defect: a filter says the item was on a corpse, which is
    /// not a claim that anybody carried that grade.
    #[test]
    fn a_loot_filter_grade_is_seen_and_not_held() {
        let mut reg = Registry::default();
        reg.fold(
            "LF_Tharn_freeport.ini",
            Source::LootFilter,
            &read_loot_filter(FILTER).unwrap(),
            "abc",
            "2026-09-06",
        );
        let pick = reg.items.iter().find(|e| e.item_id == 5040).unwrap();
        assert_eq!(pick.grades_seen, vec![2]);
        assert!(pick.grades_held.is_empty());

        reg.fold(
            "Tharn_freeport-Inventory.txt",
            Source::Inventory,
            &[Reading {
                item: "Rusty Mining Pick".into(),
                item_id: 5040,
                grade: 3,
                icon_id: None,
            }],
            "def",
            "2026-09-06",
        );
        let pick = reg.items.iter().find(|e| e.item_id == 5040).unwrap();
        assert_eq!(pick.grades_held, vec![3]);
        assert_eq!(pick.grades_seen, vec![2]);
    }

    #[test]
    fn a_name_the_game_gives_to_two_items_gets_no_shortcut() {
        let mut reg = Registry::default();
        reg.fold(
            "Tharn_freeport-Inventory.txt",
            Source::Inventory,
            &[
                Reading { item: "Bixie Stinger".into(), item_id: 14029, grade: 0, icon_id: None },
                Reading { item: "Bixie Stinger".into(), item_id: 20994, grade: 0, icon_id: None },
                Reading { item: "Rat Meat".into(), item_id: 13408, grade: 0, icon_id: None },
            ],
            "x",
            "2026-09-06",
        );
        assert_eq!(reg.by_name.get("Rat Meat"), Some(&13408));
        assert!(!reg.by_name.contains_key("Bixie Stinger"));
        assert_eq!(reg.ambiguous_names.get("Bixie Stinger"), Some(&vec![14029, 20994]));
    }

    #[test]
    fn an_alias_is_not_an_ambiguity() {
        let mut reg = Registry::default();
        reg.fold(
            "Tharn_freeport-Inventory.txt",
            Source::Inventory,
            &[
                Reading { item: "Fangol".into(), item_id: 11675, grade: 0, icon_id: None },
                Reading { item: "Fangol (Exaltation)".into(), item_id: 11675, grade: 0, icon_id: None },
            ],
            "x",
            "2026-09-06",
        );
        assert_eq!(reg.by_name.get("Fangol"), Some(&11675));
        assert!(reg.ambiguous_names.is_empty());
    }

    #[test]
    fn a_shrinking_dump_removes_nothing() {
        let mut reg = Registry::default();
        reg.fold(
            "Tharn_freeport-Inventory.txt",
            Source::Inventory,
            &read_inventory(DUMP).unwrap(),
            "one",
            "2026-09-06",
        );
        assert_eq!(reg.items.len(), 2);
        // The same character, having sold one of them.
        reg.fold(
            "Tharn_freeport-Inventory.txt",
            Source::Inventory,
            &[Reading {
                item: "Boots of the Long Road".into(),
                item_id: 177708,
                grade: 1,
                icon_id: None,
            }],
            "two",
            "2026-09-07",
        );
        assert_eq!(reg.items.len(), 2, "selling an item must not delete its id");
    }

    #[test]
    fn two_names_on_one_id_are_both_kept() {
        let mut reg = Registry::default();
        let rows = [
            Reading { item: "Fangol".into(), item_id: 11675, grade: 10, icon_id: None },
            Reading { item: "Fangol (Exaltation)".into(), item_id: 11675, grade: 0, icon_id: None },
        ];
        let stats = reg.fold("Tharn_freeport-Inventory.txt", Source::Inventory, &rows, "x", "2026-09-06");
        assert_eq!(reg.items.len(), 1);
        assert_eq!(stats.renamed, 1);
        let e = &reg.items[0];
        assert_eq!(e.item, "Fangol");
        assert_eq!(e.also_known_as, vec!["Fangol (Exaltation)"]);
        assert_eq!(e.names().collect::<Vec<_>>(), ["Fangol", "Fangol (Exaltation)"]);
    }

    #[test]
    fn a_version_already_folded_is_not_folded_again() {
        let mut reg = Registry::default();
        let rows = read_inventory(DUMP).unwrap();
        reg.fold("Tharn_halas-Inventory.txt", Source::Inventory, &rows, "sha1", "2026-09-06");
        assert_eq!(reg.change("Tharn_halas-Inventory.txt", rows.len(), "sha1"), Change::Unchanged);
        assert_eq!(reg.change("Tharn_halas-Inventory.txt", rows.len(), "sha2"),
                   Change::Changed("same row count, contents edited".into()));
        assert_eq!(reg.change("Mirela_freeport-Inventory.txt", 3, "sha3"), Change::New);

        // Re-folding the same version appends no history and no ids.
        reg.fold("Tharn_halas-Inventory.txt", Source::Inventory, &rows, "sha1", "2026-09-07");
        let src = &reg.sources[0];
        assert_eq!(src.folds.len(), 1);
        assert_eq!(src.new_ids, 2);
        assert_eq!(src.character.as_deref(), Some("Tharn"));
        assert_eq!(src.server.as_deref(), Some("halas"));
    }

    #[test]
    fn an_item_name_may_carry_a_caret() {
        let rows = read_loot_filter("7^1^500^Odd^Name\r\n").unwrap();
        assert_eq!(rows[0].item, "Odd^Name");
    }
}
