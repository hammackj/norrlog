//! Private conversations, threaded by correspondent.
//!
//! Tells arrive interleaved with everything else in the log — a reply can be
//! hundreds of lines after the message it answers, buried under combat spam.
//! Read in the file they are unusable; grouped by the person on the other end
//! they are a conversation again.
//!
//! # What counts
//!
//! Only person-to-person messages. `Someone tells General4:1, '...'` is
//! channel chat with no counterpart, and folding it in would drown fifteen real
//! conversations under thousands of channel lines.
//!
//! # Privacy
//!
//! These are the player's own private messages, held in memory for the life of
//! the daemon and never written to the history database. Nothing here leaves
//! the machine, and the export deliberately does not include them.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One message in a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TellMessage {
    pub at: i64,
    /// True when you sent it.
    pub outgoing: bool,
    pub text: String,
}

/// Everything exchanged with one person.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    /// The other person, in the casing they last appeared as.
    pub who: String,
    pub messages: Vec<TellMessage>,
    pub sent: u32,
    pub received: u32,
    pub first: i64,
    pub last: i64,
}

impl Conversation {
    pub fn total(&self) -> u32 {
        self.sent + self.received
    }

    /// Whether the last word was theirs — a thread you may owe a reply to.
    pub fn awaiting_reply(&self) -> bool {
        self.messages.last().is_some_and(|m| !m.outgoing)
    }
}

/// Everything the tells view shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TellReport {
    /// Most recently active first.
    pub conversations: Vec<Conversation>,
    pub total_sent: u32,
    pub total_received: u32,
}

/// Threads tells by correspondent.
#[derive(Debug, Default)]
pub struct TellTracker {
    /// Lowercased name to the conversation with them.
    threads: HashMap<String, Conversation>,
}

impl TellTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a tell. `who` is the other party, whichever way it went.
    pub fn on_tell(&mut self, timestamp: i64, who: &str, outgoing: bool, text: &str) {
        let key = who.to_lowercase();
        let e = self.threads.entry(key).or_insert_with(|| Conversation {
            who: who.to_string(),
            messages: Vec::new(),
            sent: 0,
            received: 0,
            first: timestamp,
            last: timestamp,
        });
        // Keep the most recent casing: the client is inconsistent about it and
        // the latest spelling is the one the player will recognise.
        e.who = who.to_string();
        if outgoing {
            e.sent += 1;
        } else {
            e.received += 1;
        }
        e.first = e.first.min(timestamp);
        e.last = e.last.max(timestamp);
        e.messages.push(TellMessage {
            at: timestamp,
            outgoing,
            text: text.to_string(),
        });
    }

    pub fn report(&self) -> TellReport {
        let mut conversations: Vec<Conversation> = self.threads.values().cloned().collect();
        // Most recent first: an old thread is history, a recent one may still
        // be going.
        conversations.sort_by_key(|c| std::cmp::Reverse(c.last));
        for c in conversations.iter_mut() {
            c.messages.sort_by_key(|m| m.at);
        }
        TellReport {
            total_sent: conversations.iter().map(|c| c.sent).sum(),
            total_received: conversations.iter().map(|c| c.received).sum(),
            conversations,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_directions_land_in_one_thread() {
        let mut t = TellTracker::new();
        t.on_tell(100, "Exit", false, "ahh ya for the fangol");
        t.on_tell(110, "Exit", true, "war 2hander");
        t.on_tell(120, "Exit", false, "nice");

        let r = t.report();
        assert_eq!(r.conversations.len(), 1);
        let c = &r.conversations[0];
        assert_eq!(c.who, "Exit");
        assert_eq!((c.sent, c.received), (1, 2));
        assert_eq!(c.total(), 3);
        assert_eq!((c.first, c.last), (100, 120));
        assert!(c.awaiting_reply(), "they spoke last");
    }

    /// The client varies the casing of a name; one person is one thread.
    #[test]
    fn casing_does_not_split_a_thread() {
        let mut t = TellTracker::new();
        t.on_tell(100, "Mongrul", false, "hi");
        t.on_tell(110, "mongrul", true, "hello");
        let r = t.report();
        assert_eq!(r.conversations.len(), 1);
        // The latest spelling wins, being the one most recently seen.
        assert_eq!(r.conversations[0].who, "mongrul");
    }

    #[test]
    fn threads_are_ordered_by_most_recent_activity() {
        let mut t = TellTracker::new();
        t.on_tell(100, "Old", false, "a");
        t.on_tell(500, "Recent", false, "b");
        t.on_tell(300, "Middle", false, "c");
        let r = t.report();
        let names: Vec<&str> = r.conversations.iter().map(|c| c.who.as_str()).collect();
        assert_eq!(names, vec!["Recent", "Middle", "Old"]);
    }

    /// Messages arrive interleaved with everything else, so a thread has to be
    /// put back in order rather than trusted to be.
    #[test]
    fn messages_within_a_thread_read_in_time_order() {
        let mut t = TellTracker::new();
        t.on_tell(300, "Exit", false, "third");
        t.on_tell(100, "Exit", true, "first");
        t.on_tell(200, "Exit", false, "second");
        let msgs = &t.report().conversations[0].messages;
        assert_eq!(
            msgs.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
    }

    #[test]
    fn a_thread_you_answered_is_not_awaiting_a_reply() {
        let mut t = TellTracker::new();
        t.on_tell(100, "Exit", false, "you there?");
        t.on_tell(110, "Exit", true, "yes");
        assert!(!t.report().conversations[0].awaiting_reply());
    }
}
