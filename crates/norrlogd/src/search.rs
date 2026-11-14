//! Search a log on disk.
//!
//! The engine keeps a few hundred recent lines for the alerts view; anything
//! older is only in the file. This reads the file itself, read-only, from the
//! start: Tharn's 390 MB log takes about a second, so there is no index to keep
//! in step with a file the game is still writing.
//!
//! The scan works on large chunks rather than line by line. The regex finds a
//! match in the chunk, the line around it is cut out, and newlines are counted
//! only up to each hit, so the cost is the regex's own, which for a plain word
//! is a vectorised substring search.

use norrlog_api::{SearchHit, SearchReply};
use regex::bytes::{Regex, RegexBuilder};
use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;

/// The most hits one search returns.
pub const LIMIT_MAX: usize = 1000;
const CHUNK: usize = 8 << 20;

/// The pattern for a query: the text as typed, unless `regex`.
///
/// Multi-line, so `^` and `$` mean the start and end of a log line, and
/// without Unicode classes: the log is bytes, mostly ASCII, and a line that
/// is not UTF-8 still has to be searchable.
pub fn pattern(q: &str, regex: bool, case: bool) -> Result<Regex, String> {
    let src = if regex {
        q.to_string()
    } else {
        regex::escape(q)
    };
    RegexBuilder::new(&src)
        .case_insensitive(!case)
        .multi_line(true)
        .unicode(false)
        .size_limit(1 << 22)
        .build()
        .map_err(|e| e.to_string())
}

/// The newest `limit` lines of `path` that `re` matches, among lines before
/// `before` (1-based) when given, and how many matched in all.
pub fn search(path: &Path, re: &Regex, limit: usize, before: Option<u64>) -> SearchReply {
    let started = std::time::Instant::now();
    let log = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return SearchReply {
                log,
                error: Some(e.to_string()),
                ..Default::default()
            };
        }
    };
    let limit = limit.clamp(1, LIMIT_MAX);
    let mut hits: VecDeque<SearchHit> = VecDeque::with_capacity(limit);
    let mut total = 0u64;
    let mut bytes = 0u64;
    // Lines before the start of `buf`.
    let mut lines_before = 0u64;
    let mut buf: Vec<u8> = Vec::with_capacity(CHUNK + 4096);
    let mut chunk = vec![0u8; CHUNK];
    let mut eof = false;
    'read: while !eof {
        let n = match file.read(&mut chunk) {
            Ok(n) => n,
            Err(e) => {
                return SearchReply {
                    log,
                    error: Some(e.to_string()),
                    ..Default::default()
                };
            }
        };
        bytes += n as u64;
        eof = n == 0;
        buf.extend_from_slice(&chunk[..n]);
        // Whole lines only, unless the file has ended without a newline.
        let end = if eof {
            buf.len()
        } else {
            match memchr::memrchr(b'\n', &buf) {
                Some(i) => i + 1,
                None => continue,
            }
        };
        let block = &buf[..end];
        let (mut pos, mut counted, mut line_no) = (0usize, 0usize, lines_before);
        while pos < block.len() {
            let Some(m) = re.find_at(block, pos) else {
                break;
            };
            let start = memchr::memrchr(b'\n', &block[..m.start()]).map_or(0, |i| i + 1);
            let stop =
                memchr::memchr(b'\n', &block[m.start()..]).map_or(block.len(), |i| m.start() + i);
            line_no += memchr::memchr_iter(b'\n', &block[counted..start]).count() as u64;
            counted = start;
            let number = line_no + 1;
            if before.is_some_and(|b| number >= b) {
                break 'read;
            }
            total += 1;
            if hits.len() == limit {
                hits.pop_front();
            }
            hits.push_back(hit(re, number, &block[start..stop]));
            // An empty match (`^`, say) must not find the same place again.
            pos = stop + 1;
        }
        lines_before += memchr::memchr_iter(b'\n', block).count() as u64;
        buf.drain(..end);
    }
    SearchReply {
        hits: hits.into_iter().rev().collect(),
        total,
        log,
        bytes,
        seconds: started.elapsed().as_secs_f64(),
        error: None,
    }
}

/// `[Fri Nov 13 13:36:13 2026] text` split at the bracket, with where `re`
/// matches in the text. A range that would split a character is left out.
fn hit(re: &Regex, line: u64, raw: &[u8]) -> SearchHit {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim_end_matches(['\r', '\n']);
    let (stamp, text) = match s.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((stamp, text)) => (stamp.to_string(), text.to_string()),
        None => (String::new(), s.to_string()),
    };
    let marks = re
        .find_iter(text.as_bytes())
        .filter(|m| m.start() < m.end())
        .filter(|m| text.is_char_boundary(m.start()) && text.is_char_boundary(m.end()))
        .map(|m| [m.start(), m.end()])
        .collect();
    SearchHit {
        line,
        stamp,
        text,
        marks,
    }
}

/// The log a request names, resolved in `dir`: an `eqlog_*.txt` file name and
/// nothing else, so the endpoint cannot be pointed at any other file.
pub fn resolve(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    let ok = name.starts_with("eqlog_")
        && name.ends_with(".txt")
        && !name.contains(['/', '\\'])
        && !name.contains("..");
    ok.then(|| dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(lines: &[&str]) -> tempfile_path::Temp {
        tempfile_path::Temp::with(lines.join("\n") + "\n")
    }

    /// A file in the temp dir, removed on drop.
    mod tempfile_path {
        pub struct Temp(pub std::path::PathBuf);
        impl Temp {
            pub fn with(text: String) -> Temp {
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                let p = std::env::temp_dir().join(format!(
                    "norrlog-search-{}-{}.txt",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::write(&p, text).unwrap();
                Temp(p)
            }
        }
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
    }

    const LINES: &[&str] = &[
        "[Fri Nov 13 13:36:13 2026] You have entered Befallen.",
        "[Fri Nov 13 13:36:36 2026] You slash a gnoll scout for 120 points of damage.",
        "[Fri Nov 13 13:36:40 2026] A gnoll scout hits YOU for 12 points of damage.",
        "[Fri Nov 13 13:37:01 2026] You have slain a gnoll scout!",
        "[Fri Nov 13 13:37:05 2026] --You have looted a Gnoll Fang.--",
    ];

    #[test]
    fn the_newest_matches_come_first_with_their_line_numbers() {
        let f = log(LINES);
        let r = search(&f.0, &pattern("gnoll", false, false).unwrap(), 2, None);
        assert_eq!(r.total, 4, "every match is counted");
        assert_eq!(
            r.hits.iter().map(|h| h.line).collect::<Vec<_>>(),
            [5, 4],
            "the newest two, newest first"
        );
        assert_eq!(r.hits[0].stamp, "Fri Nov 13 13:37:05 2026");
        assert_eq!(r.hits[0].text, "--You have looted a Gnoll Fang.--");
        assert_eq!(r.hits[0].marks, [[20, 25]], "where it matched, in the text");
    }

    #[test]
    fn before_pages_back_and_case_and_regex_are_honoured() {
        let f = log(LINES);
        let older = search(&f.0, &pattern("gnoll", false, false).unwrap(), 2, Some(4));
        assert_eq!(
            older.hits.iter().map(|h| h.line).collect::<Vec<_>>(),
            [3, 2]
        );
        assert_eq!(older.total, 2);

        let exact = search(&f.0, &pattern("Gnoll", false, true).unwrap(), 10, None);
        assert_eq!(exact.total, 1, "only the loot line has a capital G");

        let re = search(
            &f.0,
            &pattern(r"for (\d{3}) points", true, false).unwrap(),
            10,
            None,
        );
        assert_eq!(re.hits.iter().map(|h| h.line).collect::<Vec<_>>(), [2]);

        let anchored = search(
            &f.0,
            &pattern(r"^\[Fri Nov 13 13:37", true, false).unwrap(),
            10,
            None,
        );
        assert_eq!(anchored.total, 2, "^ is the start of a line");

        let literal = search(&f.0, &pattern("Fang.--", false, false).unwrap(), 10, None);
        assert_eq!(literal.total, 1, "plain text is not a pattern");

        assert!(pattern("(", true, false).is_err());
    }

    #[test]
    fn matches_across_chunk_boundaries_keep_their_line_numbers() {
        // Enough lines to need several chunks.
        let filler = "[Fri Nov 13 13:00:00 2026] ".to_string() + &"x".repeat(200);
        let mut lines: Vec<String> = (0..(3 * CHUNK / 200)).map(|_| filler.clone()).collect();
        let marks = [1usize, lines.len() / 2, lines.len() - 1];
        for &i in &marks {
            lines[i] = "[Fri Nov 13 13:00:00 2026] needle here".into();
        }
        let f = tempfile_path::Temp::with(lines.join("\n"));
        let r = search(&f.0, &pattern("needle", false, false).unwrap(), 10, None);
        assert_eq!(
            r.hits
                .iter()
                .rev()
                .map(|h| h.line as usize - 1)
                .collect::<Vec<_>>(),
            marks,
            "the last line has no newline and is still found"
        );
    }

    #[test]
    fn only_a_log_name_resolves() {
        let d = Path::new("/logs");
        assert!(resolve(d, "eqlog_Tharn_freeport.txt").is_some());
        for bad in [
            "../eqlog_x.txt",
            "eqlog_a/../../etc.txt",
            "history.db",
            "eqlog_x.txt/..",
        ] {
            assert!(resolve(d, bad).is_none(), "{bad}");
        }
    }
}
