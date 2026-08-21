//! Log file following.
//!
//! Reads the existing file to build history, then follows it for new lines.
//!
//! EverQuest appends to a single file per character, but the file can be
//! rotated or truncated between sessions, and the client writes partial lines.
//! The follower handles all three: it detects shrinkage as truncation, detects
//! replacement by identity, and never emits a line until its terminator
//! arrives.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often to check for new data. Fast enough that the overlay feels live
/// without spinning the disk.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Identity of a file, used to notice replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    len: u64,
    #[cfg(unix)]
    inode: u64,
}

impl FileId {
    fn of(path: &Path) -> Option<FileId> {
        let md = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(FileId {
                len: md.len(),
                inode: md.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            Some(FileId { len: md.len() })
        }
    }
}

/// Follows a log file, invoking a callback for each complete line.
pub struct Follower {
    path: PathBuf,
    reader: BufReader<File>,
    position: u64,
    #[cfg(unix)]
    inode: u64,
}

impl Follower {
    pub fn open(path: impl Into<PathBuf>) -> anyhow::Result<Follower> {
        let path = path.into();
        let file = File::open(&path)?;
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            file.metadata()?.ino()
        };
        Ok(Follower {
            path,
            reader: BufReader::with_capacity(1 << 20, file),
            position: 0,
            #[cfg(unix)]
            inode,
        })
    }

    /// Open, skipping everything written before `cutoff`.
    ///
    /// The log is chronological, so the first line at or after the cutoff can
    /// be found by bisecting the file rather than reading it: at 163 MB and
    /// 2.4 million lines, reading to find the boundary would cost most of what
    /// skipping it saves.
    ///
    /// The seek lands mid-line, so it winds forward to the next newline before
    /// reading a timestamp — a partial line has no timestamp to compare, and
    /// treating one as the boundary would cut a fight in half.
    ///
    /// Falls back to the whole file if nothing can be read or no line carries
    /// a timestamp. A window that cannot be honoured is better served by too
    /// much history than by none.
    pub fn open_since(path: impl Into<PathBuf>, cutoff: i64) -> anyhow::Result<Follower> {
        let mut f = Follower::open(path)?;
        let len = f.reader.get_ref().metadata()?.len();
        if len == 0 {
            return Ok(f);
        }

        // Bisect on byte offset for the first line at or after the cutoff.
        let (mut lo, mut hi) = (0u64, len);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match Follower::first_timestamp_at(f.reader.get_mut(), mid)? {
                // No timestamp between here and the end: everything at or
                // after `mid` is unusable, so the answer is to its left.
                None => hi = mid,
                Some((at, _)) if at >= cutoff => hi = mid,
                Some(_) => lo = mid + 1,
            }
        }
        let start = match Follower::first_timestamp_at(f.reader.get_mut(), lo)? {
            Some((_, offset)) => offset,
            None => 0,
        };
        f.reader.get_mut().seek(SeekFrom::Start(start))?;
        f.reader = BufReader::with_capacity(1 << 20, f.reader.into_inner());
        f.position = start;
        Ok(f)
    }

    /// The timestamp of the last line carrying one, without reading the file.
    ///
    /// A window is measured from the end of the log rather than from the wall
    /// clock: log times are the client's own clock read as UTC and sit a
    /// timezone away from `SystemTime::now()`, so "the last two hours" against
    /// the system clock could select nothing at all.
    pub fn last_timestamp(path: &Path) -> anyhow::Result<Option<i64>> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok(None);
        }
        // The tail is enough: a line is a few hundred bytes and this only has
        // to find one that parses.
        let window = len.min(1 << 16);
        file.seek(SeekFrom::Start(len - window))?;
        let buf = {
            use std::io::Read;
            let mut raw = Vec::new();
            file.take(window).read_to_end(&mut raw)?;
            String::from_utf8_lossy(&raw).into_owned()
        };
        Ok(buf
            .lines()
            .rev()
            .find_map(|l| eqlparser_core::parser::split_timestamp(l.trim_end()).map(|(ts, _)| ts)))
    }

    /// The timestamp of the first complete line at or after `offset`, and
    /// where that line begins.
    fn first_timestamp_at(file: &mut File, offset: u64) -> anyhow::Result<Option<(i64, u64)>> {
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::with_capacity(1 << 16, file);
        let mut line = String::new();
        let mut at = offset;
        // Anything but the very start begins mid-line; discard the remainder.
        if offset > 0 {
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                return Ok(None);
            }
            at += n as u64;
            line.clear();
        }
        for _ in 0..64 {
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                return Ok(None);
            }
            if let Some((ts, _)) = eqlparser_core::parser::split_timestamp(line.trim_end()) {
                return Ok(Some((ts, at)));
            }
            at += n as u64;
            line.clear();
        }
        Ok(None)
    }

    /// Read every complete line currently available, calling `on_line` for each.
    ///
    /// Returns the number of lines consumed. A trailing partial line is left
    /// unconsumed so it can be completed on a later poll — emitting half a line
    /// would corrupt whatever event it belongs to.
    pub fn read_available(&mut self, mut on_line: impl FnMut(&str)) -> anyhow::Result<usize> {
        self.handle_rotation()?;

        let mut count = 0;
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = self.reader.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break; // EOF
            }
            if !buf.ends_with(b"\n") {
                // Partial line: rewind so the next poll re-reads it whole.
                self.reader.seek(SeekFrom::Start(self.position))?;
                break;
            }
            self.position += n as u64;

            // The client writes CRLF; trim both terminators.
            while matches!(buf.last(), Some(b'\n' | b'\r')) {
                buf.pop();
            }
            if buf.is_empty() {
                continue;
            }
            on_line(&String::from_utf8_lossy(&buf));
            count += 1;
        }
        Ok(count)
    }

    /// Detect truncation or replacement and reopen from the start when needed.
    fn handle_rotation(&mut self) -> anyhow::Result<()> {
        let Some(id) = FileId::of(&self.path) else {
            return Ok(()); // File temporarily missing; try again next poll.
        };

        let replaced = {
            #[cfg(unix)]
            {
                id.inode != self.inode
            }
            #[cfg(not(unix))]
            {
                false
            }
        };
        // A file that shrank was truncated in place — a new session reusing the
        // same name. Either way we must start over rather than seek past the end.
        let truncated = id.len < self.position;

        if replaced || truncated {
            let file = File::open(&self.path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                self.inode = file.metadata()?.ino();
            }
            self.reader = BufReader::with_capacity(1 << 20, file);
            self.position = 0;
        }
        Ok(())
    }
}

/// Pick the most recently modified EverQuest log in a directory.
///
/// Characters each get their own `eqlog_<name>_<server>.txt`, so "the log the
/// user means" is almost always the one just written to.
pub fn newest_log_in(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy().to_string();
        if !name.starts_with("eqlog_") || !name.ends_with(".txt") {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        let Ok(mtime) = md.modified() else { continue };
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "eqlparser-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn reads_existing_lines_then_follows_appends() {
        let dir = tmpdir();
        let path = dir.join("follow.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();

        let mut f = Follower::open(&path).unwrap();
        let mut got = Vec::new();
        f.read_available(|l| got.push(l.to_string())).unwrap();
        assert_eq!(got, vec!["one", "two"]);

        // Append and poll again; only the new line should arrive.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "three").unwrap();
        got.clear();
        f.read_available(|l| got.push(l.to_string())).unwrap();
        assert_eq!(got, vec!["three"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn strips_crlf_written_by_the_client() {
        let dir = tmpdir();
        let path = dir.join("crlf.txt");
        std::fs::write(&path, "alpha\r\nbeta\r\n").unwrap();
        let mut f = Follower::open(&path).unwrap();
        let mut got = Vec::new();
        f.read_available(|l| got.push(l.to_string())).unwrap();
        assert_eq!(
            got,
            vec!["alpha", "beta"],
            "carriage returns must be trimmed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A half-written line must not be emitted; the client flushes in pieces.
    #[test]
    fn partial_line_is_withheld_until_complete() {
        let dir = tmpdir();
        let path = dir.join("partial.txt");
        std::fs::write(&path, "complete\npar").unwrap();

        let mut f = Follower::open(&path).unwrap();
        let mut got = Vec::new();
        f.read_available(|l| got.push(l.to_string())).unwrap();
        assert_eq!(got, vec!["complete"], "partial tail must be withheld");

        // Finish the line.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "tial").unwrap();
        got.clear();
        f.read_available(|l| got.push(l.to_string())).unwrap();
        assert_eq!(got, vec!["partial"], "line should arrive whole");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn truncation_restarts_from_the_beginning() {
        let dir = tmpdir();
        let path = dir.join("trunc.txt");
        std::fs::write(&path, "aaa\nbbb\nccc\n").unwrap();
        let mut f = Follower::open(&path).unwrap();
        let mut got = Vec::new();
        f.read_available(|l| got.push(l.to_string())).unwrap();
        assert_eq!(got.len(), 3);

        // New session truncates and starts over.
        std::fs::write(&path, "fresh\n").unwrap();
        got.clear();
        f.read_available(|l| got.push(l.to_string())).unwrap();
        assert_eq!(
            got,
            vec!["fresh"],
            "truncation should rewind, not seek past EOF"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn newest_log_is_selected() {
        let dir = tmpdir().join("logs");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("eqlog_Old_server.txt"), "x\n").unwrap();
        std::fs::write(dir.join("notalog.txt"), "x\n").unwrap();
        // Ensure a distinct, later mtime.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(dir.join("eqlog_New_server.txt"), "x\n").unwrap();

        let picked = newest_log_in(&dir).unwrap();
        assert_eq!(picked.file_name().unwrap(), "eqlog_New_server.txt");
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn non_eqlog_files_are_ignored() {
        let dir = tmpdir().join("logs2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dbg.txt"), "x\n").unwrap();
        assert!(newest_log_in(&dir).is_none());
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }
}
