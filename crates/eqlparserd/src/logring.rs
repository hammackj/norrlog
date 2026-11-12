//! The daemon's own log: everything written to stderr, kept in memory and
//! served at `/api/daemon-log`.
//!
//! # Why the daemon keeps it
//!
//! The daemon is shared. Whichever app starts it is its parent, and the
//! parent's stderr is where its output went: a terminal, a pipe into the
//! Electron app, or nowhere. Any other app asking "what is the daemon saying"
//! had no way to know, and the parent quitting left the daemon writing into a
//! closed pipe, where `eprintln!` panics.
//!
//! So on Unix, stderr is replaced at startup by a pipe this process reads
//! itself. Each line goes on to the original stderr (errors ignored: a
//! parent that has gone away is not this process's problem) and into a ring
//! of the last [`KEEP`] lines. `eprintln!` then always writes into a pipe that
//! is open, because this process holds the other end.
//!
//! Elsewhere nothing is captured and the ring stays empty.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// Lines kept. A startup is about forty; this is hours of a quiet daemon.
const KEEP: usize = 2000;

#[derive(Default)]
struct Ring {
    lines: VecDeque<(u64, String)>,
    next: u64,
}

static RING: OnceLock<Mutex<Ring>> = OnceLock::new();

fn ring() -> &'static Mutex<Ring> {
    RING.get_or_init(Default::default)
}

fn push(text: String) {
    let mut r = ring().lock().unwrap_or_else(|e| e.into_inner());
    let seq = r.next;
    r.next += 1;
    r.lines.push_back((seq, text));
    while r.lines.len() > KEEP {
        r.lines.pop_front();
    }
}

/// Lines from `since` on, the sequence number to ask from next, and whether
/// some of what was asked for has already been dropped.
pub fn since(since: u64) -> eqlparser_api::DaemonLog {
    let r = ring().lock().unwrap_or_else(|e| e.into_inner());
    let oldest = r.lines.front().map(|(s, _)| *s).unwrap_or(r.next);
    eqlparser_api::DaemonLog {
        lines: r
            .lines
            .iter()
            .filter(|(s, _)| *s >= since)
            .map(|(seq, text)| eqlparser_api::DaemonLogLine { seq: *seq, text: text.clone() })
            .collect(),
        next: r.next,
        dropped: since < oldest,
    }
}

/// Start capturing stderr. Call first thing: what is written before this is
/// not kept.
#[cfg(unix)]
pub fn capture() {
    use std::io::{BufRead, Write};
    use std::os::fd::FromRawFd;

    let mut fds = [0i32; 2];
    // SAFETY: plain libc calls on fds this process owns. stderr is only
    // swapped once the reader is running, so a failure anywhere before that
    // leaves it exactly as it was, and nothing can fill a pipe nobody drains.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        let (read_end, write_end) = (fds[0], fds[1]);
        let original = libc::dup(2);
        if original < 0 {
            libc::close(read_end);
            libc::close(write_end);
            return;
        }
        // Only this process reads the pipe or writes the original: neither
        // may leak into children (the tray's launches).
        libc::fcntl(read_end, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(original, libc::F_SETFD, libc::FD_CLOEXEC);
        let reader = std::fs::File::from_raw_fd(read_end);
        let mut out = std::fs::File::from_raw_fd(original);
        let spawned = std::thread::Builder::new().name("logring".into()).spawn(move || {
            let mut lines = std::io::BufReader::new(reader);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match lines.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        let _ = out.write_all(&buf);
                        let text = String::from_utf8_lossy(&buf);
                        push(text.trim_end_matches(['\n', '\r']).to_string());
                    }
                }
            }
        });
        if spawned.is_err() {
            // The closure, and the two files in it, were dropped.
            libc::close(write_end);
            return;
        }
        libc::dup2(write_end, 2);
        libc::close(write_end);
    }
}

#[cfg(not(unix))]
pub fn capture() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_from_a_sequence_number_and_says_when_some_were_dropped() {
        for i in 0..(KEEP + 5) {
            push(format!("line {i}"));
        }
        let all = since(0);
        assert!(all.dropped, "the first five have gone");
        assert_eq!(all.lines.len(), KEEP);
        let tail = since(all.next - 2);
        assert!(!tail.dropped);
        assert_eq!(tail.lines.len(), 2);
        assert_eq!(tail.lines[1].text, format!("line {}", KEEP + 4));
        assert!(since(all.next).lines.is_empty());
    }
}
