//! Folding the client's id-bearing files into an accumulator this machine owns.
//!
//! `norrlog_core::itemids` knows the two file formats. This knows where they
//! are, which of them have something new to say, and where the answer is kept.
//!
//! # Why an accumulator and not a scan
//!
//! Both files are snapshots. `/outputfile inventory` writes what a character
//! is carrying at that moment, and the client overwrites the same filename
//! next time; the loot filter loses a row when the player edits their
//! preferences. So the set of ids on disk is not the set of ids this machine
//! has learned — it is the set that happened to be in front of somebody the
//! last time they typed the command.
//!
//! Measured rather than assumed: re-folding only the seven files on disk in
//! September 2026 yields 1,472 ids against the 1,986 already on file, because
//! two of the five previously-read sources had since been overwritten. 523 ids
//! exist only in the accumulated record.
//!
//! Which is why this file is not in the history database. `crate::paths` draws
//! the line at rebuildability — history can be thrown away and re-derived from
//! the logs, at the cost of time and no information — and an id read out of a
//! dump that has since been overwritten cannot be re-derived from anything.
//! It belongs with the transcriptions, and it is written atomically, because
//! the accumulator it replaces was once torn in half by two writers and the
//! only symptom was a JSON error naming a file nobody was looking at.
//!
//! # What it does not do
//!
//! It does not delete. A fold adds ids, adds grades, adds names and adds
//! icons; it has no path that removes any of them. An item sold between two
//! dumps keeps its id here, which is the entire point.

use norrlog_core::game::Game;
use norrlog_core::itemids::{Change, Registry, Source};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

/// How often the game folder is re-examined while the daemon runs.
///
/// The files change when a player types `/outputfile inventory` or edits their
/// loot filter, which is a handful of times a session at most. A minute is far
/// more often than that and still nothing beside the log poll, which runs four
/// times a second.
pub const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// The accumulator's filename, beside the other things that cannot be rebuilt.
const FILE: &str = "item-ids.json";

/// One file, as this sweep found it.
pub struct Candidate {
    pub path: PathBuf,
    pub name: String,
    pub kind: Source,
    pub change: Change,
}

/// What a sweep did, for the line it prints.
#[derive(Debug, Default, Clone)]
pub struct Sweep {
    pub looked_at: usize,
    pub folded: Vec<String>,
    pub added: usize,
    pub updated: usize,
    /// Disagreements the fold recorded rather than resolved.
    pub notes: Vec<String>,
    /// Files that are the wrong shape for their name.
    pub failures: Vec<String>,
}

impl Sweep {
    pub fn changed_anything(&self) -> bool {
        !self.folded.is_empty()
    }
}

/// The registry, and where it came from.
pub struct Ids {
    path: PathBuf,
    game_dir: Option<PathBuf>,
    reg: Registry,
}

impl Ids {
    /// Open the accumulator, creating nothing until there is something to say.
    ///
    /// `None` when there is no config directory to anchor it to — the same
    /// answer `crate::paths` gives, for the same reason: a registry at a
    /// relative path would follow the working directory and quietly become a
    /// different registry per run.
    pub fn open(game_dir: Option<&Path>, game: Game) -> Option<Ids> {
        let path = crate::paths::config_file(FILE)?;
        let mut reg = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Registry>(&text) {
                Ok(r) => r,
                Err(e) => {
                    // Loudly, and without overwriting it. The file is the only
                    // copy of ids whose dumps are gone; starting fresh here
                    // would delete them on the next fold.
                    eprintln!(
                        "[norrlogd] {} will not parse ({e}); item ids are not being \
                         folded until it is fixed or moved aside",
                        path.display()
                    );
                    return None;
                }
            },
            Err(_) => Registry::default(),
        };
        reg.declare_game(game);
        Some(Ids {
            path,
            game_dir: game_dir.map(Path::to_path_buf),
            reg,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn registry(&self) -> &Registry {
        &self.reg
    }

    /// Every id-bearing file the client writes, in the two places it writes
    /// them.
    ///
    /// The dumps land in the game folder itself and the loot filters under
    /// `userdata/`. Found by walking the directories rather than by being
    /// told: `eqldb.json` names two inventory dumps while three have been
    /// folded and a fourth character's pair has been sitting on disk unread,
    /// which is what a list of paths maintained by hand does over time.
    pub fn sources(&self) -> Vec<(PathBuf, Source)> {
        let Some(game) = &self.game_dir else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for dir in [game.clone(), game.join("userdata")] {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
                    continue;
                };
                if let Some(kind) = Source::of_filename(&name) {
                    out.push((path, kind));
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Read every source and say which have something new in them.
    ///
    /// The digest and the row count are both taken, because the reason a file
    /// is being re-folded is worth printing: a changed row count is a new
    /// capture, an identical count with a different digest is an edit in
    /// place, and the second used to be invisible.
    pub fn survey(&self) -> (Vec<Candidate>, Vec<String>) {
        let mut found = Vec::new();
        let mut failures = Vec::new();
        for (path, kind) in self.sources() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let Ok(bytes) = std::fs::read(&path) else {
                failures.push(format!("{name}: could not be read"));
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let rows = match norrlog_core::itemids::read(kind, &text) {
                Ok(rows) => rows,
                Err(e) => {
                    failures.push(format!("{name}: {e}"));
                    continue;
                }
            };
            let sha = digest(&bytes);
            let change = self.reg.change(&name, rows.len(), &sha);
            found.push(Candidate {
                path,
                name,
                kind,
                change,
            });
        }
        (found, failures)
    }

    /// Fold in whatever has changed, and write the result.
    ///
    /// Writes only when something was folded. An unchanged sweep must not
    /// rewrite the file: readers watch its mtime, and a registry that appears
    /// to change every minute is a registry nobody can tell has changed.
    pub fn sweep(&mut self, today: &str) -> Sweep {
        let (found, failures) = self.survey();
        let mut sweep = Sweep {
            looked_at: found.len(),
            failures,
            ..Default::default()
        };
        for c in found.into_iter().filter(|c| c.change.worth_folding()) {
            let Ok(bytes) = std::fs::read(&c.path) else {
                sweep
                    .failures
                    .push(format!("{}: vanished mid-sweep", c.name));
                continue;
            };
            // Re-read rather than carried from the survey: the file may have
            // been rewritten between the two, and folding rows under a digest
            // they did not come from would mark the new version as read.
            let sha = digest(&bytes);
            let rows = match norrlog_core::itemids::read(c.kind, &String::from_utf8_lossy(&bytes)) {
                Ok(rows) => rows,
                Err(e) => {
                    sweep.failures.push(format!("{}: {e}", c.name));
                    continue;
                }
            };
            let stats = self.reg.fold(&c.name, c.kind, &rows, &sha, today);
            // The reason travels with the file. A changed row count is a new
            // capture and an identical count with a different digest is an
            // edit in place, and the second was invisible until it was said.
            let why = match &c.change {
                Change::New => "first time".to_string(),
                Change::Changed(reason) => reason.clone(),
                Change::Unchanged => unreachable!("filtered above"),
            };
            sweep.folded.push(format!(
                "{} ({}, {} rows, {why}, {} new)",
                c.name,
                c.kind.label(),
                stats.rows,
                stats.added
            ));
            sweep.added += stats.added;
            sweep.updated += stats.updated;
            sweep.notes.extend(stats.notes);
        }
        if sweep.changed_anything() {
            if let Err(e) = self.save() {
                sweep.failures.push(format!("{}: {e}", self.path.display()));
            }
        }
        sweep
    }

    /// Take on an accumulator written before this one, once.
    ///
    /// The record it replaces held every grade in one `grades_held` list,
    /// whichever file stated it — so it claimed 336 items were *held* at
    /// grades that only a loot filter had ever seen, on a corpse. Where a row
    /// has only ever been read out of a loot filter that is provably what
    /// happened, and those grades move to `grades_seen`.
    ///
    /// Rows that also have an inventory behind them are left as they are. The
    /// provenance was not recorded and cannot be recovered from the result,
    /// and dropping the grades instead would throw away 1,371 of them across
    /// 983 items that no dump on disk still restates. Re-folding the live
    /// files over the top corrects every row still in evidence.
    pub fn adopt(&mut self, legacy: &Path) -> anyhow::Result<(usize, usize)> {
        let text = std::fs::read_to_string(legacy)?;
        let old: Registry = serde_json::from_str(&text)?;
        let mut corrected = 0;
        let mut taken = 0;
        for mut e in old.items {
            if self.reg.items.iter().any(|h| h.item_id == e.item_id) {
                continue;
            }
            let only_filters = !e.sources.is_empty()
                && e.sources
                    .iter()
                    .all(|s| Source::of_filename(s) == Some(Source::LootFilter));
            if only_filters && !e.grades_held.is_empty() {
                e.grades_seen.extend(e.grades_held.drain(..));
                e.grades_seen.sort_unstable();
                e.grades_seen.dedup();
                corrected += 1;
            }
            self.reg.items.push(e);
            taken += 1;
        }
        self.reg.items.sort_by_key(|e| e.item_id);
        self.reg.reindex();
        for s in old.sources {
            if !self.reg.sources.iter().any(|h| h.file == s.file) {
                self.reg.sources.push(s);
            }
        }
        self.reg.sources.sort_by(|a, b| a.file.cmp(&b.file));
        self.save()?;
        Ok((taken, corrected))
    }

    /// Replace the registry in one step, or not at all.
    ///
    /// `fs::write` truncates and then writes, which leaves the file wrong for
    /// as long as the write takes and wrong for good if the process dies
    /// part-way. The accumulator this replaces was torn exactly that way when
    /// two folders ran at once, and what landed did not parse — so every
    /// window the item reader looked at afterwards failed with a JSON error
    /// naming a line in a file nobody was looking at.
    ///
    /// Beside the target rather than in the temp directory, because a rename
    /// is only atomic within a filesystem and across one it is a copy — which
    /// is the thing being avoided.
    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.new");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(serde_json::to_string_pretty(&self.reg)?.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// The same digest the record has always used: sha256, truncated. Short enough
/// to read in a listing, and only ever compared against itself.
pub fn digest(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())[..16].to_string()
}

/// Today, as the record writes dates.
///
/// UTC, because `time` is built here without the local-offset feature and a
/// fold's date is a note in a history block rather than anything a
/// calculation rests on. It is off by at most a day, and says so by being the
/// same everywhere.
pub fn today() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!("{:04}-{:02}-{:02}", t.year(), t.month() as u8, t.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "norrlogd-ids-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn ids_at(dir: &Path, game: &Path) -> Ids {
        let mut reg = Registry::default();
        reg.declare_game(Game::EverQuestLegends);
        Ids {
            path: dir.join(FILE),
            game_dir: Some(game.to_path_buf()),
            reg,
        }
    }

    fn a_game_folder(tag: &str) -> PathBuf {
        let game = tmpdir(tag);
        std::fs::create_dir_all(game.join("userdata")).unwrap();
        std::fs::write(
            game.join("Tharn_halas-Inventory.txt"),
            "Location\tName\tID\tCount\tSlots\r\n\
             Any Slot\tBoots of the Long Road +1\t177708\t1\t10\r\n\
             Any Slot-Slot1\tEmpty\t0\t0\t0\r\n",
        )
        .unwrap();
        std::fs::write(
            game.join("userdata").join("LF_Tharn_halas.ini"),
            "#ITEM_ID^FILTER_ID^ICON_ID^ITEM_NAME\r\n13408^2^817^Rat Meat\r\n",
        )
        .unwrap();
        // Neither of these carries item ids, and both sit in the same folders.
        std::fs::write(game.join("eqlog_Tharn_halas.txt"), "[Sun] Hello\n").unwrap();
        std::fs::write(game.join("userdata").join("UI_Tharn_halas.ini"), "x=1\n").unwrap();
        game
    }

    #[test]
    fn both_folders_are_searched_and_nothing_else_is_taken() {
        let game = a_game_folder("sources");
        let dir = tmpdir("sources-cfg");
        let ids = ids_at(&dir, &game);
        let found = ids.sources();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().any(|(_, k)| *k == Source::Inventory));
        assert!(found.iter().any(|(_, k)| *k == Source::LootFilter));
    }

    #[test]
    fn a_sweep_folds_once_and_then_says_nothing() {
        let game = a_game_folder("sweep");
        let dir = tmpdir("sweep-cfg");
        let mut ids = ids_at(&dir, &game);

        let first = ids.sweep("2026-09-06");
        assert_eq!(first.looked_at, 2);
        assert_eq!(first.folded.len(), 2);
        assert_eq!(first.added, 2);
        assert!(ids.path().exists());

        let again = ids.sweep("2026-09-06");
        assert!(!again.changed_anything());
        assert_eq!(again.added, 0);
    }

    #[test]
    fn a_new_dump_that_lost_an_item_still_adds_to_the_record() {
        let game = a_game_folder("shrink");
        let dir = tmpdir("shrink-cfg");
        let mut ids = ids_at(&dir, &game);
        ids.sweep("2026-09-06");

        // The character sold the boots and looted a hammer.
        std::fs::write(
            game.join("Tharn_halas-Inventory.txt"),
            "Location\tName\tID\tCount\tSlots\r\nAny Slot\tRusty Hammer\t9001\t1\t10\r\n",
        )
        .unwrap();
        let second = ids.sweep("2026-09-07");
        assert_eq!(second.added, 1);
        let held: Vec<u32> = ids.registry().items.iter().map(|e| e.item_id).collect();
        assert!(
            held.contains(&177708),
            "the sold item kept its id: {held:?}"
        );
        assert!(held.contains(&9001));
    }

    #[test]
    fn a_file_of_the_wrong_shape_is_reported_and_the_rest_still_fold() {
        let game = a_game_folder("bad");
        let dir = tmpdir("bad-cfg");
        std::fs::write(
            game.join("Broken_halas-Inventory.txt"),
            "not a dump at all\r\n",
        )
        .unwrap();
        let mut ids = ids_at(&dir, &game);
        let s = ids.sweep("2026-09-06");
        assert_eq!(s.failures.len(), 1, "{:?}", s.failures);
        assert!(s.failures[0].starts_with("Broken_halas-Inventory.txt"));
        assert_eq!(s.folded.len(), 2);
    }

    #[test]
    fn the_registry_is_replaced_whole() {
        let game = a_game_folder("atomic");
        let dir = tmpdir("atomic-cfg");
        let mut ids = ids_at(&dir, &game);
        ids.sweep("2026-09-06");
        let text = std::fs::read_to_string(ids.path()).unwrap();
        serde_json::from_str::<Registry>(&text).expect("a saved registry parses");
        assert!(
            !dir.join("item-ids.json.new").exists(),
            "no leftover temp file"
        );
    }

    /// Adoption is the one place a grade moves between the two lists.
    #[test]
    fn adopting_the_old_record_files_filter_only_grades_as_seen() {
        let game = a_game_folder("adopt");
        let dir = tmpdir("adopt-cfg");
        let legacy = dir.join("legacy.json");
        std::fs::write(
            &legacy,
            r#"{"note":"","game":"eqlegends","key":"item_id","sources":[],"items":[
                {"item_id":1002,"item":"Cloth Veil","grades_held":[0,1,2],
                 "sources":["LF_Tharn_freeport.ini"]},
                {"item_id":5214,"item":"Morning Star","grades_held":[0,4],
                 "sources":["LF_Tharn_freeport.ini","Tharn_freeport-Inventory.txt"]}]}"#,
        )
        .unwrap();
        let mut ids = ids_at(&dir, &game);
        let (taken, corrected) = ids.adopt(&legacy).unwrap();
        assert_eq!((taken, corrected), (2, 1));

        let veil = ids
            .registry()
            .items
            .iter()
            .find(|e| e.item_id == 1002)
            .unwrap();
        assert!(
            veil.grades_held.is_empty(),
            "never held, only seen on a corpse"
        );
        assert_eq!(veil.grades_seen, vec![0, 1, 2]);

        let star = ids
            .registry()
            .items
            .iter()
            .find(|e| e.item_id == 5214)
            .unwrap();
        assert_eq!(
            star.grades_held,
            vec![0, 4],
            "an inventory row is behind these"
        );
    }

    #[test]
    fn adoption_never_overwrites_an_id_already_known() {
        let game = a_game_folder("adopt-twice");
        let dir = tmpdir("adopt-twice-cfg");
        let mut ids = ids_at(&dir, &game);
        ids.sweep("2026-09-06");
        let legacy = dir.join("legacy.json");
        std::fs::write(
            &legacy,
            r#"{"note":"","game":"eqlegends","key":"item_id","sources":[],"items":[
                {"item_id":177708,"item":"Something Else","grades_held":[9],
                 "sources":["LF_Old_server.ini"]}]}"#,
        )
        .unwrap();
        let (taken, _) = ids.adopt(&legacy).unwrap();
        assert_eq!(taken, 0);
        let boots = ids
            .registry()
            .items
            .iter()
            .find(|e| e.item_id == 177708)
            .unwrap();
        assert_eq!(boots.item, "Boots of the Long Road");
    }
}
