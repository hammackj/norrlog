//! Read the logs the daemon is *not* following, from where they were left.
//!
//! # Why a sweep and not a tailer per log
//!
//! One tailer per log was asked for, and measuring says that is the
//! wrong shape for the same reason it was wrong for screenshots: which log is
//! followed is a choice about the *live view*, and it must not decide whether
//! a character's kills, loot and factions are ever read.
//!
//! A tailer per log would also buy nothing a sweep does not. You can only
//! play one character at a time, so an unfollowed log **does not grow while
//! you are elsewhere** — there is no stream to follow, only a gap to close
//! whenever you come back. `sources` already holds a byte watermark per log
//! and the one-shot bundle modes already read every log; what is missing is
//! that nothing closes the gap unless somebody runs a one-shot by hand.
//!
//! Measured on the workstation, 9 Sep 2026:
//!
//! ```text
//! eqlog_Tharn_freeport.txt   352,075,483 parsed of 352,087,061   <- followed
//! eqlog_Mirela_freeport.txt     6,522,524 parsed of   6,522,524   <- by hand
//! ```
//!
//! Mirela is complete only because a `--bundle-out` run happened to include it.
//! Nothing guarantees that, and the failure is silent: the alt's log simply
//! stops contributing, and looks identical to an alt nobody played.
//!
//! # What this is not
//!
//! It does not touch the live [`Engine`]. Each log is parsed into an engine of
//! its own and flushed under its own `(character, server)`, exactly as
//! `bundle_once` does — one log, one identity, one flush. Blending a second
//! character into the running engine would corrupt the one view this daemon
//! exists to give.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use norrlog_core::game::Game;
use norrlog_core::Parser;

use crate::engine::Engine;
use crate::locks::Guarded;
use crate::store;
use std::sync::Mutex;

/// How often the other logs are caught up.
///
/// Five minutes rather than the shot sweep's minute. There is no urgency: an
/// unfollowed log does not grow while you are elsewhere, so this is closing a
/// gap that opened the last time you played that character, not following a
/// stream. What it costs is one `stat` per log per tick when there is nothing
/// to do, which is the ordinary case.
pub const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// How long to wait before the first sweep.
///
/// The daemon's own initial parse of the followed log is the expensive thing
/// that happens at startup -- 352 MB on this machine -- and a catch-up racing
/// it would put two parses on one disk for no reason. By the time this fires
/// the follower is normally live and trickling.
pub const FIRST_SWEEP_AFTER: std::time::Duration = std::time::Duration::from_secs(90);

/// Every log's watermark, by path.
///
/// Read in one go through a store of the caller's own rather than borrowing
/// the daemon's: the follower holds that one for the length of every batch it
/// ingests, and a sweep is not worth making a live view wait for.
///
/// An unreadable database answers with an empty map, which makes every log
/// look unread. That is the safe direction: the worst case is re-reading a
/// log from the start, which is slow and correct, rather than skipping one,
/// which is fast and wrong.
pub fn watermarks(db: &Path) -> std::collections::HashMap<String, u64> {
    let Ok(st) = store::Store::open_as(db, "", "") else {
        return Default::default();
    };
    st.sources()
        .unwrap_or_default()
        .into_iter()
        .map(|(path, _seen, bytes)| (path, bytes.max(0) as u64))
        .collect()
}

/// The same, with the time each was last read, for [`status`].
pub fn watermarks_seen(db: &Path) -> std::collections::HashMap<String, (u64, i64)> {
    let Ok(st) = store::Store::open_as(db, "", "") else {
        return Default::default();
    };
    st.sources()
        .unwrap_or_default()
        .into_iter()
        .map(|(path, seen, bytes)| (path, (bytes.max(0) as u64, seen)))
        .collect()
}

/// What one log's catch-up did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Caught {
    pub path: String,
    pub player: String,
    pub server: String,
    /// Where the new bytes began: the watermark, or zero for a log never
    /// read or one that shrank (see [`resume_at`]). The whole log is read
    /// either way; see [`catch_up_one`].
    pub from: u64,
    /// Where it now stands, which is the file's length.
    pub to: u64,
    pub lines: u64,
    /// Seconds spent parsing, so a sweep that is quietly costing minutes is
    /// visible rather than inferred from the daemon feeling slow.
    pub seconds: f64,
}

/// Where to start reading `path`, given what was read last time.
///
/// Three cases, and the middle one is the whole reason this is a function.
///
/// * **Shorter than the watermark** — the log was rotated, or the client was
///   reinstalled, and byte 900 of the new file has nothing to do with byte 900
///   of the old one. Start over. This mirrors `Follower`'s own truncation
///   rule, which rewinds rather than seeking past the end.
/// * **Not on a line boundary** — a watermark is written after whole lines, so
///   this should not happen; if it ever does, resuming mid-line would feed the
///   parser half a timestamp and it would drop the line silently. Winding
///   forward to the next newline costs one read and removes the whole class.
/// * **Exactly where we left off** — the ordinary case.
fn resume_at(file: &mut std::fs::File, watermark: u64, len: u64) -> std::io::Result<u64> {
    if watermark == 0 || watermark > len {
        return Ok(0);
    }
    if watermark == len {
        return Ok(len);
    }
    // The byte before the mark. If it closed a line, the mark is a boundary.
    file.seek(SeekFrom::Start(watermark - 1))?;
    let mut one = [0u8; 1];
    if file.read_exact(&mut one).is_ok() && one[0] == b'\n' {
        return Ok(watermark);
    }
    file.seek(SeekFrom::Start(watermark))?;
    let mut skip = Vec::new();
    BufReader::new(file.try_clone()?).read_until(b'\n', &mut skip)?;
    Ok(watermark + skip.len() as u64)
}

/// Catch one log up, or say why it was left alone.
///
/// `None` means there was nothing to do — no new bytes, or a name the client
/// did not write. A log that cannot be attributed to a character is skipped
/// rather than guessed at: history rows are keyed on `(character, server)`
/// and a wrong guess files an alt's kills under somebody else.
pub fn catch_up_one(
    path: &Path,
    db: &Path,
    game_dir: Option<&Path>,
    // Stated rather than detected per log. `Engine::new` is test-only for a
    // reason -- the daemon always knows which game a log came from, and what
    // an engine observes is recorded as being about that game. Re-detecting
    // it here from each filename could file an alt's kills under the other
    // game on a machine that has both installed, which this one does.
    game: Game,
    watermark: u64,
    now: i64,
) -> Option<Caught> {
    let (player, server) = Parser::identity_from_filename(&path.to_string_lossy())?;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let from = resume_at(&mut file, watermark, len).ok()?;
    if from >= len {
        return None;
    }
    // Read from the start, not from the watermark. The history writers for
    // drops, vendors, the bestiary and the record replace everything stored
    // for a source, so an engine that saw only the new tail would swap a
    // log's totals for the tail's: three sightings became one. An unfollowed
    // log grows only when that character is played elsewhere, so this is one
    // parse per session played, not one per sweep.
    file.seek(SeekFrom::Start(0)).ok()?;

    let started = std::time::Instant::now();
    let spells = Engine::load_spell_db(game_dir);
    let engine = Arc::new(Mutex::new(Engine::with_game(player.clone(), spells, game)));
    // A line that is not UTF-8 is read like any other. A read that fails
    // stops here, before anything is written: the history writers replace
    // what is stored for this log, and a half-read log stored as the whole
    // of it lost every total after the failure. The watermark stays put, so
    // the next sweep tries again.
    let read = {
        let mut e = engine.locked("catch-up");
        norrlog_core::lines::each_line(BufReader::with_capacity(1 << 20, file), |l| e.push_line(l))
    };
    let lines = match read {
        Ok(n) => n,
        Err(e) => {
            eprintln!(
                "[norrlogd] catch-up: could not read {}: {e}",
                path.display()
            );
            return None;
        }
    };
    let st = store::Store::open_as(db, &player, &server).ok()?;
    let store = Arc::new(Mutex::new(st));
    // A failed write leaves the watermark where it was, so the next sweep
    // tries again rather than taking the log as read.
    crate::flush_history(&engine, &store, path, true).ok()?;
    // The watermark is the file's length as measured *before* the read, not
    // its length now. The log may have grown while this ran -- an alt's log
    // will not, but a log that turns out to be the one being played would --
    // and claiming those bytes were read would skip them forever.
    if let Err(e) = store
        .locked("catch-up")
        .note_source(&path.to_string_lossy(), len, now)
    {
        eprintln!(
            "[norrlogd] catch-up: could not mark {}: {e}",
            path.display()
        );
    }
    Some(Caught {
        path: path.to_string_lossy().to_string(),
        player,
        server,
        from,
        to: len,
        lines,
        seconds: started.elapsed().as_secs_f64(),
    })
}

/// Every log beside the followed one, caught up from its watermark.
///
/// `followed` is skipped because the tailer owns it and writes its own
/// watermark; reading it here would race that and re-parse the same lines
/// into a second engine.
pub fn sweep(
    dir: &Path,
    followed: &Path,
    db: &Path,
    game_dir: Option<&Path>,
    game: Game,
    marks: &dyn Fn(&str) -> u64,
    now: i64,
) -> Vec<Caught> {
    let followed = followed
        .canonicalize()
        .unwrap_or_else(|_| followed.to_path_buf());
    let mut out = Vec::new();
    for path in crate::tail::logs_in(dir) {
        let same = path.canonicalize().map(|p| p == followed).unwrap_or(false);
        if same {
            continue;
        }
        let mark = marks(&path.to_string_lossy());
        if let Some(c) = catch_up_one(&path, db, game_dir, game, mark, now) {
            out.push(c);
        }
    }
    out
}

/// How far behind every log in the folder is, followed or not.
///
/// The reporting half of the sweep. Without it nothing says that a log is being
/// read by nothing — the symptom is a folder that looks exactly like a folder
/// whose files have all been handled, which is the same shape of silence
/// eqldb and the screenshot detector each turned up once before.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogStatus {
    pub path: String,
    pub name: String,
    pub player: String,
    pub server: String,
    /// True for the one the daemon is tailing; the rest are swept.
    pub followed: bool,
    pub bytes: u64,
    pub parsed: u64,
    /// `bytes - parsed`, said rather than left to be subtracted, because the
    /// number a person wants is "how much of this has nobody read".
    pub behind: u64,
    pub last_seen: i64,
}

pub fn status(dir: &Path, followed: &Path, marks: &dyn Fn(&str) -> (u64, i64)) -> Vec<LogStatus> {
    let canon = followed
        .canonicalize()
        .unwrap_or_else(|_| followed.to_path_buf());
    let mut out = Vec::new();
    for path in crate::tail::logs_in(dir) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let (player, server) = Parser::identity_from_filename(&path.to_string_lossy())
            .unwrap_or_else(|| (String::new(), String::new()));
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let (parsed, last_seen) = marks(&path.to_string_lossy());
        out.push(LogStatus {
            followed: path.canonicalize().map(|p| p == canon).unwrap_or(false),
            path: path.to_string_lossy().to_string(),
            name,
            player,
            server,
            bytes,
            parsed,
            behind: bytes.saturating_sub(parsed.min(bytes)),
            last_seen,
        });
    }
    // Furthest behind first: the answer to "is anything being missed" should
    // be the first row, not something to scan for.
    out.sort_by(|a, b| b.behind.cmp(&a.behind).then(a.name.cmp(&b.name)));
    out
}

/// The folder to sweep: whatever was configured, else the followed log's own.
pub fn folder(logs_dir: Option<&Path>, followed: &Path) -> Option<PathBuf> {
    logs_dir
        .map(Path::to_path_buf)
        .or_else(|| followed.parent().map(Path::to_path_buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eqlp-catchup-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, text: &str) {
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    const A: &str = "[Tue Jul 28 14:00:00 2026] You slash a giant for 300 points of damage.\n";
    const B: &str = "[Tue Jul 28 14:00:05 2026] You slash a giant for 100 points of damage.\n";

    /// The ordinary case: resume exactly where the watermark says.
    #[test]
    fn a_watermark_on_a_line_boundary_is_taken_as_it_is() {
        let d = scratch("boundary");
        let p = d.join("eqlog_Mirela_freeport.txt");
        write(&p, &format!("{A}{B}"));
        let mut f = std::fs::File::open(&p).unwrap();
        let len = f.metadata().unwrap().len();
        assert_eq!(
            resume_at(&mut f, A.len() as u64, len).unwrap(),
            A.len() as u64
        );
    }

    /// A watermark that landed mid-line winds forward rather than feeding the
    /// parser half a timestamp.
    ///
    /// `note_source` writes whole-line counts, so this should be unreachable
    /// — which is exactly why it is worth pinning. A resume that silently
    /// drops the line it lands in the middle of would lose one kill per
    /// interrupted run and never say so.
    #[test]
    fn a_watermark_inside_a_line_winds_to_the_next_one() {
        let d = scratch("midline");
        let p = d.join("eqlog_Mirela_freeport.txt");
        write(&p, &format!("{A}{B}"));
        let mut f = std::fs::File::open(&p).unwrap();
        let len = f.metadata().unwrap().len();
        // Ten bytes into the first line.
        assert_eq!(resume_at(&mut f, 10, len).unwrap(), A.len() as u64);
    }

    /// A log shorter than its watermark was rotated or reinstalled, and byte
    /// 900 of the new file has nothing to do with byte 900 of the old one.
    #[test]
    fn a_log_that_shrank_is_read_from_the_start() {
        let d = scratch("shrank");
        let p = d.join("eqlog_Mirela_freeport.txt");
        write(&p, A);
        let mut f = std::fs::File::open(&p).unwrap();
        let len = f.metadata().unwrap().len();
        assert_eq!(
            resume_at(&mut f, 9_000, len).unwrap(),
            0,
            "a rotated log was resumed mid-file"
        );
    }

    /// Nothing new is nothing done — no engine, no store, no write.
    #[test]
    fn a_log_with_nothing_new_is_left_alone() {
        let d = scratch("nothing");
        let p = d.join("eqlog_Mirela_freeport.txt");
        write(&p, A);
        let db = d.join("history.db");
        let len = std::fs::metadata(&p).unwrap().len();
        assert!(catch_up_one(&p, &db, None, Game::Unknown, len, 0).is_none());
        assert!(
            !db.exists(),
            "a log with nothing new still opened a database"
        );
    }

    /// A name the client did not write is skipped rather than guessed at.
    ///
    /// History rows are keyed on `(character, server)`. A guess files an
    /// alt's kills under somebody else, which is worse than not reading the
    /// log at all because it is wrong rather than absent.
    #[test]
    fn a_log_that_names_no_character_is_skipped() {
        let d = scratch("noname");
        let p = d.join("notalog.txt");
        write(&p, A);
        assert!(catch_up_one(&p, &d.join("history.db"), None, Game::Unknown, 0, 0).is_none());
    }

    /// The followed log is the tailer's, and the sweep does not touch it.
    #[test]
    fn the_sweep_leaves_the_followed_log_to_its_tailer() {
        let d = scratch("followed");
        let followed = d.join("eqlog_Tharn_freeport.txt");
        let other = d.join("eqlog_Mirela_freeport.txt");
        write(&followed, &format!("{A}{B}"));
        write(&other, &format!("{A}{B}"));
        let caught = sweep(
            &d,
            &followed,
            &d.join("history.db"),
            None,
            Game::Unknown,
            &|_| 0,
            0,
        );
        let names: Vec<&str> = caught.iter().map(|c| c.player.as_str()).collect();
        assert_eq!(
            names,
            vec!["Mirela"],
            "the sweep re-read the log the follower owns"
        );
        assert_eq!(caught[0].from, 0);
        assert_eq!(caught[0].to, (A.len() + B.len()) as u64);
        assert_eq!(caught[0].lines, 2);
    }

    /// A log that grew is read again from the start, so what is stored for it
    /// is the whole log's, not the new tail's.
    #[test]
    fn a_grown_log_is_read_whole() {
        let d = scratch("tail");
        let p = d.join("eqlog_Mirela_freeport.txt");
        write(&p, &format!("{A}{B}"));
        let c = catch_up_one(
            &p,
            &d.join("history.db"),
            None,
            Game::Unknown,
            A.len() as u64,
            0,
        )
        .expect("something to read");
        assert_eq!(c.lines, 2);
        assert_eq!(c.from, A.len() as u64, "where the new bytes began");
    }

    /// Two sightings, then one more appended and caught up, are three; and a
    /// sweep with nothing new leaves them at three.
    #[test]
    fn catching_up_keeps_what_was_stored_before() {
        let d = scratch("keeps");
        let p = d.join("eqlog_Mirela_freeport.txt");
        let db = d.join("history.db");
        let loot = |s: u32| {
            format!("[Tue Jul 28 14:00:{s:02} 2026] --You have looted a Stale Bread from a rat's corpse.--\n")
        };
        let zone = "[Tue Jul 28 13:59:00 2026] You have entered West Commonlands.\n";
        write(&p, &format!("{zone}{}{}", loot(1), loot(2)));
        let first = catch_up_one(&p, &db, None, Game::Unknown, 0, 0).expect("a first read");
        let looted = || {
            let st = store::Store::open_as(&db, "Mirela", "freeport").unwrap();
            st.all_drops()
                .unwrap()
                .iter()
                .filter(|r| r.item == "Stale Bread")
                .map(|r| r.looted)
                .sum::<u64>()
        };
        assert_eq!(looted(), 2);

        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(loot(3).as_bytes())
            .unwrap();
        catch_up_one(&p, &db, None, Game::Unknown, first.to, 0).expect("the appended line");
        assert_eq!(
            looted(),
            3,
            "the catch-up replaced the log's totals with the tail's"
        );

        let len = std::fs::metadata(&p).unwrap().len();
        assert!(catch_up_one(&p, &db, None, Game::Unknown, len, 0).is_none());
        assert_eq!(looted(), 3);
    }

    /// The status readout says how far behind each log is, worst first.
    fn stale_bread(db: &Path) -> u64 {
        let st = store::Store::open_as(db, "Mirela", "freeport").unwrap();
        st.all_drops()
            .unwrap()
            .iter()
            .filter(|r| r.item == "Stale Bread")
            .map(|r| r.looted)
            .sum()
    }

    fn bread(s: u32) -> String {
        format!("[Tue Jul 28 14:00:{s:02} 2026] --You have looted a Stale Bread from a rat's corpse.--\n")
    }

    /// A line that is not UTF-8 was taken for the end of the log: what came
    /// after it was never read, the log's totals were replaced with the part
    /// before it, and the watermark claimed the whole file.
    #[test]
    fn a_line_that_is_not_utf8_does_not_end_the_log() {
        let d = scratch("not-utf8");
        let p = d.join("eqlog_Mirela_freeport.txt");
        let db = d.join("history.db");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(bread(1).as_bytes()).unwrap();
        f.write_all(b"[Tue Jul 28 14:00:01 2026] Mirela says, 'bad: \xff'\n")
            .unwrap();
        f.write_all(bread(2).as_bytes()).unwrap();
        drop(f);
        let first = catch_up_one(&p, &db, None, Game::Unknown, 0, 0).expect("a first read");
        assert_eq!(first.lines, 3);
        assert_eq!(stale_bread(&db), 2);

        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(bread(3).as_bytes())
            .unwrap();
        catch_up_one(&p, &db, None, Game::Unknown, first.to, 0).expect("the appended line");
        assert_eq!(stale_bread(&db), 3, "the totals stopped at the bad line");
    }

    /// A read that fails writes nothing: the totals stored before stay, and
    /// so does the watermark, so the next sweep tries again.
    #[test]
    fn a_failed_read_keeps_the_stored_history_and_the_watermark() {
        let d = scratch("read-fails");
        let p = d.join("eqlog_Mirela_freeport.txt");
        let db = d.join("history.db");
        write(&p, &format!("{}{}", bread(1), bread(2)));
        let first = catch_up_one(&p, &db, None, Game::Unknown, 0, 0).expect("a first read");
        assert_eq!(stale_bread(&db), 2);
        let key = p.to_string_lossy().to_string();
        assert_eq!(watermarks(&db).get(&key), Some(&first.to));

        // A directory where the log was: it opens, and every read fails.
        std::fs::remove_file(&p).unwrap();
        std::fs::create_dir(&p).unwrap();
        assert!(catch_up_one(&p, &db, None, Game::Unknown, 0, 0).is_none());
        assert_eq!(stale_bread(&db), 2, "a failed read replaced the totals");
        assert_eq!(watermarks(&db).get(&key), Some(&first.to));
    }

    #[test]
    fn the_status_puts_the_worst_gap_first_and_names_the_followed_log() {
        let d = scratch("status");
        let followed = d.join("eqlog_Tharn_freeport.txt");
        let behind = d.join("eqlog_Mirela_freeport.txt");
        write(&followed, &format!("{A}{B}"));
        write(&behind, &format!("{A}{B}"));
        // The followed log is fully read; the other has never been touched.
        let rows = status(&d, &followed, &|p| {
            if p.contains("Tharn") {
                ((A.len() + B.len()) as u64, 99)
            } else {
                (0, 0)
            }
        });
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].player, "Mirela", "the worst gap is not first");
        assert_eq!(rows[0].behind, (A.len() + B.len()) as u64);
        assert!(!rows[0].followed);
        assert_eq!(rows[1].player, "Tharn");
        assert_eq!(rows[1].behind, 0, "the followed log reads as behind");
        assert!(
            rows[1].followed,
            "the followed log is not marked as followed"
        );
    }

    /// A watermark larger than the file does not report a negative gap.
    #[test]
    fn a_shrunken_log_is_not_reported_as_read_ahead() {
        let d = scratch("negative");
        let p = d.join("eqlog_Mirela_freeport.txt");
        write(&p, A);
        let rows = status(&d, &d.join("eqlog_Tharn_freeport.txt"), &|_| (9_000, 5));
        assert_eq!(
            rows[0].behind, 0,
            "a rotated log reported a gap it does not have"
        );
    }
}
