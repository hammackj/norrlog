//! Filing a screenshot out of the client's folder by the time it was taken.
//!
//! This is `pipeline/archive_shots.py`'s mechanics, in the daemon, so a field
//! machine can do it. That tree is not on a field machine and never will be:
//! it is a workstation's dataset, and a contributor running eqlparser has the
//! client, the daemon and nothing else.
//!
//! # What has to be reproduced exactly
//!
//! The archive already holds 4,530 screenshots filed by the Python, and every
//! reading in the dataset cites one by its *original* name. So this is not a
//! new scheme that happens to be similar --- the names, the day folders and
//! the index records have to come out identical, or `lib.shot()` stops
//! resolving citations the moment both have run.
//!
//! ```text
//! processed/2026-08-14/eq-20260814-195757.jpg
//! index.json  ->  {"by_original": {"EQ001234.jpg": {archived, mtime, captured, bytes}}}
//! ```
//!
//! # The two silent failures it refuses
//!
//! The client reuses screenshot numbers once the folder empties, so a fresh
//! `EQ000052.jpg` can arrive alongside one already filed. Archiving the second
//! would either overwrite the first's index entry or bury two pictures under
//! one key, and both are quiet. A name already in the index whose content
//! differs is therefore left where it is and reported.
//!
//! Two shots can also share a second --- six pairs do in the current archive
//! --- so a taken name gets `-2`, `-3` before the extension rather than
//! landing on top of its twin.

use anyhow::{bail, Context, Result};
use eqlparser_vision::Kind;
use std::path::{Path, PathBuf};
use time::{OffsetDateTime, UtcOffset};

/// Which question a screenshot answers, which is which tree it belongs in.
///
/// Three rather than one because "what did a reading come from" and "what is
/// there left to look at" are different questions, and one pile answered both
/// until 285 AA windows were found sitting among the item screenshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tree {
    /// It produced a reading.
    Processed,
    /// It holds an Alternate Advancement panel, which never produces an item
    /// reading and is read by a different pass.
    Aa,
    /// A pass read it and found nothing in it: scenery, portraits, maps, UI.
    NotItems,
}

impl Tree {
    pub fn dir(self) -> &'static str {
        match self {
            Tree::Processed => "processed",
            Tree::Aa => "aa_processed",
            Tree::NotItems => "not_items",
        }
    }

    /// The `note` a new index is created with, matching the Python's wording
    /// so a tree created by either reads the same.
    fn note(self) -> &'static str {
        match self {
            Tree::Processed => {
                "Screenshots that produced an item reading, moved out of the inbox \
                 after processing."
            }
            Tree::Aa => {
                "Screenshots holding an Alternate Advancement window, filed like \
                 processed/ and read by ocr/aa.py."
            }
            Tree::NotItems => {
                "Screenshots a pass looked at and found no item window and no AA \
                 window in: scenery, creature portraits, maps, inventory and UI \
                 captures."
            }
        }
    }
}

/// This machine's UTC offset, from the log clock's disagreement with it.
///
/// The archive's names are local wall time because the Python's were, and
/// `time` is built here without `local-offset` --- which is also unsound in a
/// process with threads, and the daemon has them from startup. So the offset
/// is measured rather than asked for: the client writes local wall time into
/// its log with no zone on it, so `Engine::clock_offset`, which is that
/// timestamp read as UTC minus the real epoch, *is* the offset.
///
/// Snapped to the nearest quarter hour, and that is not tidying. The measured
/// value carries however long it took the line to reach the daemon, and a
/// name is built from `mtime + offset`: an offset three seconds out moves a
/// screenshot taken at 19:57:57 to `eq-20260814-195800.jpg`, which is a
/// different file from the one the Python would have written. Every real UTC
/// offset is a whole number of quarter hours, so rounding to one throws away
/// the drift and nothing else.
pub fn offset_from_clock(seconds: i64) -> Option<UtcOffset> {
    const QUARTER: i64 = 15 * 60;
    let snapped = ((seconds as f64) / QUARTER as f64).round() as i64 * QUARTER;
    UtcOffset::from_whole_seconds(snapped as i32).ok()
}

/// Where one screenshot should go, given what was read out of it and what
/// the classifier saw in it.
///
/// A third answer besides the two trees, and it is the point of the type. The
/// Python sorted every screenshot it could not read into `not_items/`, and
/// that is how 26 Actions windows, 20 Spell Display windows and a Quest
/// Journal came to be filed as scenery: `newpass` reads item windows, so
/// everything else is "nothing" to it. A window this daemon cannot read is
/// not scenery, and the only honest thing to do with one is leave it in the
/// inbox where the next pass will still find it.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// File it here.
    File(Tree),
    /// Leave it in the inbox, for this reason.
    Leave(String),
}

/// Decide, from a reading and a classification.
///
/// `read_item` is whether the reader got an item name out of the shot ---
/// not whether it tried. `seen` is what matched, which is a list of what was
/// recognised and never a claim that nothing else is there: see
/// `eqlparser_vision::Windows::seen`.
pub fn decide(read_item: bool, seen: &[Kind]) -> Plan {
    // A reading is the strongest evidence there is, and it settles the
    // question regardless of what else the classifier noticed --- a shot can
    // hold an item window and a spell window at once, and the item window is
    // why it was taken.
    if read_item {
        return Plan::File(Tree::Processed);
    }
    // An item window the reader could not turn into a name. Filing this is
    // the expensive mistake: `not_items/` is where screenshots go to stop
    // being looked at, and this one holds an item.
    if seen.contains(&Kind::Item) || seen.contains(&Kind::ItemSkin2) {
        return Plan::Leave(
            "an item window the reader could not read --- filing it would \
             bury an item in with the scenery"
                .into(),
        );
    }
    // AA panels have their own tree and their own pass.
    if seen == [Kind::Aa] {
        return Plan::File(Tree::Aa);
    }
    // Anything else that matched is a window some pass reads and this daemon
    // does not.
    let unread: Vec<Kind> = seen.iter().copied().filter(|k| *k != Kind::Aa).collect();
    if !unread.is_empty() {
        let names: Vec<&str> = unread.iter().map(|k| k.window()).collect();
        return Plan::Leave(format!(
            "holds {} --- nothing here reads {} yet, and not_items/ is where \
             a screenshot stops being looked at",
            names.join(" and "),
            if names.len() == 1 { "that" } else { "those" }
        ));
    }
    // Nothing matched. This is the one place the answer is genuinely
    // "scenery", and it is only worth trusting because every window the
    // client draws now has a template --- the same sentence was true when
    // three of them did, and it was wrong 47 times in one folder.
    Plan::File(Tree::NotItems)
}

/// What happened to one screenshot.
#[derive(Debug, Clone, PartialEq)]
pub enum Filed {
    /// Moved (or, under a dry run, would be) into `tree`, at `archived`.
    ///
    /// `archived` is relative to the tree and not to the archive, because
    /// that is what goes into `index.json` and the Python's records are
    /// written that way. The tree is carried alongside rather than folded in:
    /// a report that says only `2026-09-09/eq-20260909-133617.jpg` does not
    /// say whether the screenshot was kept or set aside, which is the half a
    /// person reading the line actually wants.
    Moved { original: String, tree: Tree, archived: String },
    /// Left in the inbox, with the reason a person needs to act on.
    Refused { original: String, why: String },
}

/// One tree's `index.json`, edited in place.
///
/// Held as a `Value` rather than a struct so that keys this does not know
/// about survive a write. The file is shared with a Python tree that has
/// grown fields before and may again, and a round trip that quietly dropped
/// one would be discovered much later.
struct Index {
    path: PathBuf,
    root: serde_json::Value,
}

impl Index {
    fn open(dir: &Path, tree: Tree) -> Result<Index> {
        let path = dir.join("index.json");
        let root = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b)
                .with_context(|| format!("{} is not JSON", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                serde_json::json!({ "note": tree.note(), "by_original": {} })
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if !root.get("by_original").map(|v| v.is_object()).unwrap_or(false) {
            bail!("{} has no by_original object", path.display());
        }
        Ok(Index { path, root })
    }

    fn by_original(&self) -> &serde_json::Map<String, serde_json::Value> {
        self.root["by_original"].as_object().unwrap()
    }

    /// Every archive-relative path already spoken for, so a new name can
    /// avoid landing on one.
    fn taken(&self) -> std::collections::HashSet<String> {
        self.by_original()
            .values()
            .filter_map(|v| v.get("archived")?.as_str().map(str::to_string))
            .collect()
    }

    fn record(&self, original: &str) -> Option<&serde_json::Value> {
        self.by_original().get(original)
    }

    fn insert(&mut self, original: &str, rec: serde_json::Value) {
        self.root["by_original"]
            .as_object_mut()
            .unwrap()
            .insert(original.to_string(), rec);
    }

    /// `indent=1`, which is what `json.dump` was called with. Not cosmetic:
    /// the file is in a directory a person browses, and a whole-file diff
    /// between a Python write and a Rust one should be the records that
    /// changed rather than every line.
    fn write(&self) -> Result<()> {
        let mut out = Vec::new();
        let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
        serde::Serialize::serialize(&self.root, &mut ser)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &out).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming into {}", self.path.display()))?;
        Ok(())
    }
}

/// The client's screenshot folder, and the three trees under it.
pub struct Archive {
    root: PathBuf,
    /// Captured once by the caller. `UtcOffset::current_local_offset()` fails
    /// in a process that has threads, which the daemon does from startup, and
    /// a test that depended on the machine's zone would pass in one place and
    /// not another. The names are local wall time because the Python's are.
    offset: UtcOffset,
}

impl Archive {
    pub fn new(root: impl Into<PathBuf>, offset: UtcOffset) -> Archive {
        Archive { root: root.into(), offset }
    }

    /// File one screenshot. `dry` decides everything and moves nothing.
    pub fn file(&self, src: &Path, tree: Tree, dry: bool) -> Result<Filed> {
        let original = src
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .context("a screenshot with no filename")?;
        let dir = self.root.join(tree.dir());
        let mut index = Index::open(&dir, tree)?;

        // A name already filed, whose bytes are not the ones filed under it.
        if let Some(rec) = index.record(&original) {
            if let Some(prev) = rec.get("archived").and_then(|v| v.as_str()) {
                let old = dir.join(prev);
                if old.is_file() && digest(&old)? != digest(src)? {
                    return Ok(Filed::Refused {
                        original,
                        why: format!(
                            "already in {}/ as {prev} with different content \
                             --- the client reuses numbers, so this needs a look \
                             rather than an overwrite",
                            tree.dir()
                        ),
                    });
                }
            }
        }

        let meta = std::fs::metadata(src).with_context(|| format!("stat {}", src.display()))?;
        let mtime = meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let (day, name) = self.archived_name(src, mtime, &index.taken());
        let rel = format!("{day}/{name}");

        if dry {
            return Ok(Filed::Moved { original, tree, archived: rel });
        }

        let dst_dir = dir.join(&day);
        std::fs::create_dir_all(&dst_dir)
            .with_context(|| format!("creating {}", dst_dir.display()))?;
        let dst = dst_dir.join(&name);
        if dst.exists() {
            bail!("refusing to overwrite {}", dst.display());
        }
        rename_or_copy(src, &dst)?;
        // The name is derived from the capture time, so the file has to keep
        // it --- a move that reset mtime would leave the two disagreeing and
        // any later re-derivation would pick a different name.
        let mt = filetime_from(mtime);
        set_mtime(&dst, mt)?;
        let size = std::fs::metadata(&dst)?.len();
        if size != meta.len() {
            bail!(
                "{} changed size moving into place ({} -> {})",
                original,
                meta.len(),
                size
            );
        }

        index.insert(
            &original,
            serde_json::json!({
                "archived": rel,
                "mtime": mtime,
                "captured": self.captured(mtime)?,
                "bytes": size,
            }),
        );
        index.write()?;
        Ok(Filed::Moved { original, tree, archived: rel })
    }

    /// `(day folder, filename)`.
    fn archived_name(
        &self,
        src: &Path,
        mtime: i64,
        taken: &std::collections::HashSet<String>,
    ) -> (String, String) {
        let t = self.local(mtime);
        let day = format!("{:04}-{:02}-{:02}", t.year(), t.month() as u8, t.day());
        let stem = format!(
            "eq-{:04}{:02}{:02}-{:02}{:02}{:02}",
            t.year(),
            t.month() as u8,
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        );
        // The source extension, lowered. This was `.jpg` outright until the
        // first hand-cropped PNG arrived, which would have been filed under a
        // name its bytes disagreed with.
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| "jpg".into());
        let mut name = format!("{stem}.{ext}");
        let mut n = 1;
        while taken.contains(&format!("{day}/{name}")) {
            n += 1;
            name = format!("{stem}-{n}.{ext}");
        }
        (day, name)
    }

    fn local(&self, mtime: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(mtime)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
            .to_offset(self.offset)
    }

    fn captured(&self, mtime: i64) -> Result<String> {
        let t = self.local(mtime);
        Ok(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.year(),
            t.month() as u8,
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        ))
    }
}

/// The first 16 hex of a SHA-256 over the whole file, which is what
/// `archive_shots.sha` compares.
fn digest(p: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(p).with_context(|| format!("opening {}", p.display()))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(format!("{:x}", h.finalize())[..16].to_string())
}

/// `rename` where the two are on one filesystem, copy-then-remove where they
/// are not.
///
/// The archive sits under the client's own folder, so a rename is the normal
/// case --- but the folder can be a mount of its own, and `EXDEV` there would
/// otherwise stop the daemon filing anything at all.
fn rename_or_copy(src: &Path, dst: &Path) -> Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(18) => {
            std::fs::copy(src, dst).with_context(|| format!("copying to {}", dst.display()))?;
            std::fs::remove_file(src).with_context(|| format!("removing {}", src.display()))?;
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("moving {} to {}", src.display(), dst.display())),
    }
}

fn filetime_from(secs: i64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs.max(0) as u64)
}

fn set_mtime(p: &Path, t: std::time::SystemTime) -> Result<()> {
    let f = std::fs::OpenOptions::new().write(true).open(p)?;
    f.set_modified(t)
        .with_context(|| format!("keeping the capture time on {}", p.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured offset is drift plus a real zone; only the zone survives.
    #[test]
    fn the_clock_offset_is_rounded_to_a_real_zone() {
        let h = |n: i64| n * 3600;
        // -5h, as measured on the machine this was written on, with the
        // three seconds it took the line to arrive still in it.
        assert_eq!(offset_from_clock(-h(5) + 3), UtcOffset::from_hms(-5, 0, 0).ok());
        assert_eq!(offset_from_clock(-h(5) - 3), UtcOffset::from_hms(-5, 0, 0).ok());
        assert_eq!(offset_from_clock(-h(5)), UtcOffset::from_hms(-5, 0, 0).ok());
        // UTC, and the half-hour and quarter-hour zones that exist.
        assert_eq!(offset_from_clock(12), UtcOffset::from_hms(0, 0, 0).ok());
        assert_eq!(offset_from_clock(h(5) + 1800 - 4), UtcOffset::from_hms(5, 30, 0).ok());
        assert_eq!(offset_from_clock(h(5) + 2700 + 9), UtcOffset::from_hms(5, 45, 0).ok());
    }

    /// Three seconds of drift is a different filename.
    ///
    /// This is why the snapping is there rather than a nicety. The name comes
    /// out of `mtime + offset` to the second.
    #[test]
    fn unrounded_drift_would_move_the_second() {
        let drifted = UtcOffset::from_whole_seconds((-5 * 3600 + 3) as i32).unwrap();
        let snapped = offset_from_clock(-5 * 3600 + 3).unwrap();
        let at = OffsetDateTime::from_unix_timestamp(AT).unwrap();
        assert_ne!(
            at.to_offset(drifted).second(),
            at.to_offset(snapped).second(),
            "the rounding is what stops the drift reaching the filename"
        );
    }

    /// A reading settles it, whatever else is in the picture.
    #[test]
    fn a_shot_that_produced_a_reading_is_processed() {
        assert_eq!(decide(true, &[Kind::Item]), Plan::File(Tree::Processed));
        // Both windows present. The item window is why the picture exists.
        assert_eq!(decide(true, &[Kind::Item, Kind::Spell]), Plan::File(Tree::Processed));
        // A reading with nothing recognised is still a reading. The
        // classifier is evidence, not a veto.
        assert_eq!(decide(true, &[]), Plan::File(Tree::Processed));
    }

    /// Nothing recognised, nothing read: the one honest "scenery".
    #[test]
    fn a_shot_with_no_window_in_it_is_not_items() {
        assert_eq!(decide(false, &[]), Plan::File(Tree::NotItems));
    }

    #[test]
    fn an_aa_panel_has_its_own_tree() {
        assert_eq!(decide(false, &[Kind::Aa]), Plan::File(Tree::Aa));
    }

    /// The 47 misfiled screenshots, as a test.
    ///
    /// Every one of these was sitting in `not_items/` when the classifier
    /// could only recognise three window types. Each has a pass that reads
    /// it; none of those passes is in this daemon yet. Filing them is what
    /// went wrong, so the answer is to leave them alone.
    #[test]
    fn a_window_nothing_here_reads_stays_in_the_inbox() {
        for k in [Kind::Actions, Kind::Merchant, Kind::Book, Kind::Quest, Kind::Spell] {
            match decide(false, &[k]) {
                Plan::Leave(why) => {
                    assert!(why.contains(k.window()), "{k:?} refusal did not say what it saw: {why}");
                }
                other => panic!("{k:?} was filed as {other:?}"),
            }
        }
    }

    /// An AA panel next to a window nothing reads is not an AA screenshot.
    ///
    /// `aa_processed/` is read by a pass that cuts AA entries out of it. A
    /// shot filed there because of its AA panel takes the spell window beside
    /// it out of circulation just as quietly as `not_items/` would.
    #[test]
    fn aa_only_means_only_aa() {
        assert_eq!(decide(false, &[Kind::Aa]), Plan::File(Tree::Aa));
        match decide(false, &[Kind::Aa, Kind::Spell]) {
            Plan::Leave(why) => assert!(why.contains("a Spell Display window"), "{why}"),
            other => panic!("filed as {other:?}"),
        }
    }

    /// An item window the reader could not read is the expensive one.
    ///
    /// This is the case the whole three-way answer exists for. `not_items/`
    /// is where a screenshot goes to stop being looked at; an item window
    /// that defeated the reader is exactly the thing somebody has to come
    /// back to.
    #[test]
    fn an_unread_item_window_is_never_filed() {
        for k in [Kind::Item, Kind::ItemSkin2] {
            match decide(false, &[k]) {
                Plan::Leave(why) => assert!(why.contains("bury an item"), "{k:?}: {why}"),
                other => panic!("{k:?} was filed as {other:?}"),
            }
        }
        // Even alongside an AA panel, which would otherwise claim it.
        assert!(matches!(decide(false, &[Kind::Item, Kind::Aa]), Plan::Leave(_)));
    }

    /// A directory that removes itself, because the crate has no `tempfile`
    /// and one test's leftovers are the next test's collision.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let p = std::env::temp_dir().join(format!(
                "eqlp-archive-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }

        fn shot(&self, name: &str, bytes: &[u8], mtime: i64) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).unwrap();
            set_mtime(&p, filetime_from(mtime)).unwrap();
            p
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Fixed, so the names in these tests are the names everywhere.
    fn utc() -> UtcOffset {
        UtcOffset::UTC
    }

    /// 2026-08-14 19:57:57 UTC.
    const AT: i64 = 1_786_737_477;

    fn index(dir: &Path, tree: Tree) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join(tree.dir()).join("index.json")).unwrap())
            .unwrap()
    }

    #[test]
    fn a_screenshot_is_filed_under_the_day_and_second_it_was_taken() {
        let d = Dir::new("name");
        let src = d.shot("EQ001234.jpg", b"pixels", AT);
        let a = Archive::new(&d.0, utc());
        let got = a.file(&src, Tree::Processed, false).unwrap();
        assert_eq!(
            got,
            Filed::Moved {
                original: "EQ001234.jpg".into(),
                tree: Tree::Processed,
                archived: "2026-08-14/eq-20260814-195757.jpg".into(),
            }
        );
        assert!(!src.exists(), "the original was left in the inbox");
        let landed = d.0.join("processed/2026-08-14/eq-20260814-195757.jpg");
        assert_eq!(std::fs::read(&landed).unwrap(), b"pixels");

        // The name is derived from the capture time, so the file has to keep
        // it or a re-derivation would name it something else.
        let mt = std::fs::metadata(&landed)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(mt, AT, "the capture time did not survive the move");

        let idx = index(&d.0, Tree::Processed);
        let rec = &idx["by_original"]["EQ001234.jpg"];
        assert_eq!(rec["archived"], "2026-08-14/eq-20260814-195757.jpg");
        assert_eq!(rec["mtime"], AT);
        assert_eq!(rec["captured"], "2026-08-14 19:57:57");
        assert_eq!(rec["bytes"], 6);
    }

    /// The client reuses numbers once the folder empties. Archiving the second
    /// would either overwrite the first's entry or bury two pictures under one
    /// key, and both are silent.
    #[test]
    fn a_reused_name_with_different_content_is_refused_not_overwritten() {
        let d = Dir::new("reuse");
        let a = Archive::new(&d.0, utc());
        a.file(&d.shot("EQ000052.jpg", b"the first one", AT), Tree::Processed, false)
            .unwrap();

        let again = d.shot("EQ000052.jpg", b"a different picture", AT + 3600);
        let got = a.file(&again, Tree::Processed, false).unwrap();
        match got {
            Filed::Refused { original, why } => {
                assert_eq!(original, "EQ000052.jpg");
                assert!(why.contains("eq-20260814-195757.jpg"), "{why}");
                assert!(why.contains("reuses numbers"), "{why}");
            }
            other => panic!("the second copy was filed: {other:?}"),
        }
        assert!(again.exists(), "a refused screenshot must stay in the inbox");
        let idx = index(&d.0, Tree::Processed);
        assert_eq!(idx["by_original"].as_object().unwrap().len(), 1);
        assert_eq!(
            std::fs::read(d.0.join("processed/2026-08-14/eq-20260814-195757.jpg")).unwrap(),
            b"the first one"
        );
    }

    /// The same name and the same bytes is the same picture, which is a
    /// re-run rather than a collision.
    #[test]
    fn the_same_picture_under_the_same_name_is_not_a_collision() {
        let d = Dir::new("same");
        let a = Archive::new(&d.0, utc());
        a.file(&d.shot("EQ7.jpg", b"same", AT), Tree::Processed, false).unwrap();
        let got = a.file(&d.shot("EQ7.jpg", b"same", AT), Tree::Processed, false).unwrap();
        assert!(matches!(got, Filed::Moved { .. }), "{got:?}");
    }

    /// Six pairs in the real archive share a second.
    #[test]
    fn two_shots_in_one_second_do_not_land_on_each_other() {
        let d = Dir::new("tie");
        let a = Archive::new(&d.0, utc());
        let one = a.file(&d.shot("EQ1.jpg", b"a", AT), Tree::Processed, false).unwrap();
        let two = a.file(&d.shot("EQ2.jpg", b"b", AT), Tree::Processed, false).unwrap();
        let three = a.file(&d.shot("EQ3.jpg", b"c", AT), Tree::Processed, false).unwrap();
        let names: Vec<String> = [one, two, three]
            .iter()
            .map(|f| match f {
                Filed::Moved { archived, .. } => archived.clone(),
                o => panic!("{o:?}"),
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "2026-08-14/eq-20260814-195757.jpg",
                "2026-08-14/eq-20260814-195757-2.jpg",
                "2026-08-14/eq-20260814-195757-3.jpg",
            ]
        );
        for n in &names {
            assert!(d.0.join("processed").join(n).is_file(), "{n} is not on disk");
        }
    }

    /// A hand-cropped PNG filed under a `.jpg` name would leave the bytes and
    /// the name disagreeing for anything that opened it by extension.
    #[test]
    fn the_source_extension_is_kept() {
        let d = Dir::new("ext");
        let a = Archive::new(&d.0, utc());
        let got = a.file(&d.shot("window.PNG", b"p", AT), Tree::NotItems, false).unwrap();
        assert_eq!(
            got,
            Filed::Moved {
                original: "window.PNG".into(),
                tree: Tree::NotItems,
                archived: "2026-08-14/eq-20260814-195757.png".into(),
            }
        );
    }

    #[test]
    fn a_dry_run_decides_everything_and_moves_nothing() {
        let d = Dir::new("dry");
        let src = d.shot("EQ9.jpg", b"x", AT);
        let a = Archive::new(&d.0, utc());
        let got = a.file(&src, Tree::Aa, true).unwrap();
        assert_eq!(
            got,
            Filed::Moved {
                original: "EQ9.jpg".into(),
                tree: Tree::Aa,
                archived: "2026-08-14/eq-20260814-195757.jpg".into(),
            }
        );
        assert!(src.exists(), "a dry run moved the file");
        assert!(!d.0.join("aa_processed/index.json").exists(), "a dry run wrote an index");
    }

    /// The index is shared with a Python tree that has grown fields before.
    #[test]
    fn keys_this_does_not_know_about_survive_a_write() {
        let d = Dir::new("keys");
        let dir = d.0.join("not_items");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("index.json"),
            br#"{"note":"mine","by_original":{},"someday":{"deep":[1,2]}}"#,
        )
        .unwrap();
        let a = Archive::new(&d.0, utc());
        a.file(&d.shot("EQ4.jpg", b"z", AT), Tree::NotItems, false).unwrap();
        let idx = index(&d.0, Tree::NotItems);
        assert_eq!(idx["note"], "mine", "the note was rewritten");
        assert_eq!(idx["someday"]["deep"][1], 2, "an unknown key was dropped");
        assert!(idx["by_original"]["EQ4.jpg"].is_object());
    }


    /// The zone the archive was written in, from any one record: `captured` is
    /// `mtime` in local wall time, so the difference between them is the
    /// offset. Rounded to a whole minute, which every real zone is.
    fn offset_of(root: &Path) -> Option<UtcOffset> {
        for tree in [Tree::Processed, Tree::Aa, Tree::NotItems] {
            let p = root.join(tree.dir()).join("index.json");
            let Ok(b) = std::fs::read(&p) else { continue };
            let Ok(idx) = serde_json::from_slice::<serde_json::Value>(&b) else { continue };
            for rec in idx["by_original"].as_object()?.values() {
                let (Some(mtime), Some(cap)) =
                    (rec["mtime"].as_i64(), rec["captured"].as_str())
                else {
                    continue;
                };
                // "YYYY-MM-DD HH:MM:SS" read as if it were UTC, which turns
                // the wall clock back into a number the offset can come from.
                let f = time::macros::format_description!(
                    "[year]-[month]-[day] [hour]:[minute]:[second]"
                );
                let Ok(naive) = time::PrimitiveDateTime::parse(cap, &f) else { continue };
                let secs = naive.assume_utc().unix_timestamp() - mtime;
                return UtcOffset::from_whole_seconds(secs as i32).ok();
            }
        }
        None
    }

    /// The naming replayed against the archive the Python built.
    ///
    /// Not a fixture: every record in the three real `index.json` files says
    /// what mtime a screenshot had and what name it was given. Re-deriving the
    /// name from the mtime has to produce the record's own `archived` value
    /// for all 4,502 of them, or the two disagree and `lib.shot()` stops
    /// resolving citations the moment both have run.
    ///
    /// The collision suffixes are the part this really tests --- six pairs in
    /// the current archive share a second, and which of them got `-2` depends
    /// on the order they were filed in, which is why the replay walks the
    /// records in the order the index lists them.
    ///
    /// ```text
    /// EQL_SHOTS=/path/to/Screenshots cargo test -p eqlparserd -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs the real archive on disk"]
    fn every_name_the_python_chose_comes_back_the_same() {
        let root = PathBuf::from(std::env::var("EQL_SHOTS").expect("EQL_SHOTS"));
        // The archive says what zone it was written in, so the replay reads it
        // out of the data rather than asking the machine.
        //
        // Two reasons. `UtcOffset::current_local_offset()` needs the
        // `local-offset` feature, which is unsound in a process with threads
        // --- the daemon has them from startup, which is why `Archive` takes
        // the offset rather than finding it. And a replay that used *this*
        // machine's zone would silently pass on a copy of the archive opened
        // somewhere else.
        let offset = offset_of(&root).expect("no record to read the zone from");
        let a = Archive::new(&root, offset);
        let (mut n, mut bad) = (0usize, Vec::new());
        for tree in [Tree::Processed, Tree::Aa, Tree::NotItems] {
            let p = root.join(tree.dir()).join("index.json");
            if !p.is_file() {
                continue;
            }
            let idx: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            let mut taken = std::collections::HashSet::new();
            for (orig, rec) in idx["by_original"].as_object().unwrap() {
                let want = rec["archived"].as_str().unwrap();
                let mtime = rec["mtime"].as_i64().unwrap();
                let ext = std::path::Path::new(want);
                let (day, name) = a.archived_name(ext, mtime, &taken);
                taken.insert(format!("{day}/{name}"));
                n += 1;
                if format!("{day}/{name}") != want {
                    bad.push(format!("{orig}: python {want}, rust {day}/{name}"));
                }
                // The `captured` string is the other derived field.
                let cap = a.captured(mtime).unwrap();
                if rec["captured"].as_str() != Some(cap.as_str()) {
                    bad.push(format!("{orig}: captured {} vs {cap}", rec["captured"]));
                }
            }
        }
        println!("replayed {n} record(s), {} disagreement(s)", bad.len());
        for b in bad.iter().take(20) {
            println!("   {b}");
        }
        assert!(bad.is_empty(), "{} name(s) would differ", bad.len());
    }

    /// Each tree keeps its own names, so the same second in two trees is two
    /// files rather than one refusing the other.
    #[test]
    fn the_trees_do_not_share_an_index() {
        let d = Dir::new("trees");
        let a = Archive::new(&d.0, utc());
        for (t, n) in [(Tree::Processed, "A.jpg"), (Tree::Aa, "B.jpg"), (Tree::NotItems, "C.jpg")] {
            let got = a.file(&d.shot(n, n.as_bytes(), AT), t, false).unwrap();
            assert_eq!(
                got,
                Filed::Moved {
                    original: n.into(),
                    tree: t,
                    archived: "2026-08-14/eq-20260814-195757.jpg".into(),
                },
                "{t:?} did not name its own file"
            );
        }
        for t in [Tree::Processed, Tree::Aa, Tree::NotItems] {
            assert_eq!(index(&d.0, t)["by_original"].as_object().unwrap().len(), 1);
        }
    }
}
