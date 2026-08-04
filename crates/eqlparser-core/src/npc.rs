//! What was said to and by each NPC, and what came of it.
//!
//! # Why this is separate from tasks and Sky quests
//!
//! Those two know what they are looking for: the client names a task outright,
//! and Sky quests come from a transcribed table. Most quests are neither. The
//! Iksar faction turn-in with Crusader Iktra is a hail, two keywords, a trade
//! and eighty faction — none of which the client labels as a quest at all.
//!
//! So this records the raw shape of an interaction and leaves the judgement to
//! a person: which NPC, what you said, what they said, what you handed over
//! and what changed as a result. A write-up is then promoted from it by hand.
//!
//! # What is reliable
//!
//! Verified against a real turn-in:
//!
//! - Dialogue and your own `say` lines are complete and in order.
//! - `You complete the trade with X` is written; the matching `You offered`
//!   lines are **not** when the items are stacked, so what was handed over
//!   often cannot be recovered from the log at all.
//! - Faction lines follow the trade, one per item, so they say more about the
//!   size of a turn-in than the trade line does.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How long after a trade a faction change is taken to belong to it.
const REWARD_WINDOW: i64 = 15;

/// How soon an NPC's reply must follow something you did to count as an answer
/// to it. The client writes both in the same second in practice.
const RESPONSE_WINDOW: i64 = 3;

/// A line said during an interaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Line {
    pub at: i64,
    /// True when you said it.
    pub yours: bool,
    pub text: String,
    /// `[bracketed keywords]` the NPC offered.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// The line answered something you did, rather than someone else.
    ///
    /// A busy quest NPC replies to everyone within earshot, and the log does
    /// not say who a line was aimed at. What it does show is timing: an answer
    /// to your keyword lands within a second or two of you saying it, while
    /// `Thank you, Hotdogs` arrives on its own. A guide wants only the former,
    /// so the rest is kept but marked.
    #[serde(default = "yes")]
    pub to_you: bool,
}

fn yes() -> bool {
    true
}

/// A turn-in to this NPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnIn {
    pub at: i64,
    /// Items named in `You offered` lines. Often empty — the client omits
    /// them for stacked items, and an empty list means unknown, not none.
    pub items: Vec<String>,
    /// Faction changes that followed, as (faction, total adjustment).
    pub faction: Vec<(String, i64)>,
}

/// Everything exchanged with one NPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpcRecord {
    pub npc: String,
    pub zone: String,
    pub lines: Vec<Line>,
    pub turn_ins: Vec<TurnIn>,
    /// Keywords the NPC has offered, deduplicated — the branch points a guide
    /// needs to list.
    pub keywords: Vec<String>,
    pub first: i64,
    pub last: i64,
}

/// Everything the capture view shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpcReport {
    /// Most recently spoken to first.
    pub npcs: Vec<NpcRecord>,
}

/// Records interactions with quest-giving NPCs.
#[derive(Debug, Default)]
pub struct NpcTracker {
    npcs: HashMap<String, NpcRecord>,
    /// The NPC most recently hailed or spoken to, so your own keywords and a
    /// trade can be attributed to someone.
    focus: Option<(String, i64)>,
    /// When you last said something or completed a trade, so a reply can be
    /// told from an NPC talking to somebody else.
    last_prompt: i64,
    zone: String,
    /// Offers awaiting the line that closes the trade.
    pending: Vec<String>,
    /// Faction changes since the last trade, for attributing a reward.
    recent_faction: Vec<(i64, String, i64)>,
}

impl NpcTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_zone(&mut self, zone: &str) {
        self.zone = zone.to_string();
    }

    /// Note something an NPC said.
    ///
    /// Only NPCs you have addressed are recorded. Everything on the say
    /// channel would otherwise arrive here — merchants, guards, other players
    /// — and bury the handful of conversations that were actually quests.
    pub fn on_npc_line(&mut self, timestamp: i64, npc: &str, text: &str) {
        if !self.is_engaged(npc, timestamp) {
            return;
        }
        let kws = keywords(text);
        let answered = timestamp - self.last_prompt <= RESPONSE_WINDOW;
        let zone = self.zone.clone();
        let e = self.entry(npc, timestamp, &zone);
        // The client repeats a line once per item in a stacked turn-in; the
        // same words at the same second are one utterance.
        if e.lines
            .iter()
            .any(|l| l.at == timestamp && l.text == text && !l.yours)
        {
            return;
        }
        e.last = e.last.max(timestamp);
        if answered {
            for k in &kws {
                if !e.keywords.contains(k) {
                    e.keywords.push(k.clone());
                }
            }
        }
        e.lines.push(Line {
            at: timestamp,
            yours: false,
            text: text.to_string(),
            keywords: kws,
            to_you: answered,
        });
    }

    /// Note something you said.
    ///
    /// A hail names its target and sets the focus; anything else is a keyword
    /// aimed at whoever you last hailed.
    pub fn on_your_say(&mut self, timestamp: i64, text: &str) {
        let hailed = text
            .strip_prefix("Hail, ")
            .or_else(|| text.strip_prefix("Hail "))
            .map(|n| n.trim().trim_end_matches('!').to_string());

        self.last_prompt = timestamp;
        if let Some(npc) = hailed {
            self.focus = Some((npc.to_lowercase(), timestamp));
            let zone = self.zone.clone();
            let e = self.entry(&npc, timestamp, &zone);
            e.last = e.last.max(timestamp);
            e.lines.push(Line {
                at: timestamp,
                yours: true,
                text: text.to_string(),
                keywords: Vec::new(),
                to_you: true,
            });
            return;
        }

        let Some((key, _)) = self.focus.clone() else {
            return;
        };
        if !self.is_engaged(&key, timestamp) {
            return;
        }
        if let Some(e) = self.npcs.get_mut(&key) {
            e.last = e.last.max(timestamp);
            e.lines.push(Line {
                at: timestamp,
                yours: true,
                text: text.to_string(),
                keywords: Vec::new(),
                to_you: true,
            });
        }
    }

    pub fn on_offer(&mut self, item: &str, quantity: u64) {
        let n = quantity.max(1);
        self.pending.push(if n > 1 {
            format!("{item} ×{n}")
        } else {
            item.to_string()
        });
    }

    pub fn on_faction(&mut self, timestamp: i64, faction: &str, adjustment: i64) {
        self.recent_faction
            .push((timestamp, faction.to_string(), adjustment));
        let cutoff = timestamp - REWARD_WINDOW;
        self.recent_faction.retain(|(t, _, _)| *t >= cutoff);
    }

    /// Note a completed trade, taking the offers and nearby faction with it.
    pub fn on_trade_complete(&mut self, timestamp: i64, npc: &str) {
        let items = std::mem::take(&mut self.pending);

        // Faction arrives one line per item, so it is summed per faction: the
        // total is what a guide wants, not eight separate tens.
        let mut totals: HashMap<String, i64> = HashMap::new();
        for (t, f, adj) in &self.recent_faction {
            if timestamp - t <= REWARD_WINDOW {
                *totals.entry(f.clone()).or_insert(0) += adj;
            }
        }
        let mut faction: Vec<(String, i64)> = totals.into_iter().collect();
        faction.sort_by(|a, b| b.1.abs().cmp(&a.1.abs()).then(a.0.cmp(&b.0)));

        self.last_prompt = timestamp;
        let zone = self.zone.clone();
        let e = self.entry(npc, timestamp, &zone);
        e.last = e.last.max(timestamp);
        e.turn_ins.push(TurnIn {
            at: timestamp,
            items,
            faction,
        });
        self.focus = Some((npc.to_lowercase(), timestamp));
    }

    fn entry(&mut self, npc: &str, timestamp: i64, zone: &str) -> &mut NpcRecord {
        let e = self
            .npcs
            .entry(npc.to_lowercase())
            .or_insert_with(|| NpcRecord {
                npc: npc.to_string(),
                zone: zone.to_string(),
                lines: Vec::new(),
                turn_ins: Vec::new(),
                keywords: Vec::new(),
                first: timestamp,
                last: timestamp,
            });
        e.first = e.first.min(timestamp);
        e
    }

    /// Whether an NPC is the one currently being spoken to.
    ///
    /// A conversation is bounded by the hail that started it: without that, a
    /// guard greeting a passer-by would be recorded as a quest.
    fn is_engaged(&self, npc: &str, timestamp: i64) -> bool {
        let key = npc.to_lowercase();
        match &self.focus {
            Some((f, at)) => *f == key && timestamp - at <= CONVERSATION_WINDOW,
            None => false,
        }
    }

    pub fn report(&self) -> NpcReport {
        let mut npcs: Vec<NpcRecord> = self
            .npcs
            .values()
            .filter(|n| n.lines.len() > 1 || !n.turn_ins.is_empty())
            .cloned()
            .collect();
        npcs.sort_by_key(|n| std::cmp::Reverse(n.last));
        for n in npcs.iter_mut() {
            n.lines.sort_by_key(|l| l.at);
        }
        NpcReport { npcs }
    }
}

/// How long a hail keeps a conversation open.
///
/// Generous: reading a wall of quest text and deciding which keyword to say
/// takes as long as it takes.
const CONVERSATION_WINDOW: i64 = 300;

/// Pull `[bracketed keywords]` out of a line.
fn keywords(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let kw = after[..close].trim();
        if !kw.is_empty() && !out.contains(&kw.to_string()) {
            out.push(kw.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a real quest: hail, keyword, answer, turn-in, faction.
    #[test]
    fn a_hail_and_keyword_exchange_is_captured_in_order() {
        let mut t = NpcTracker::new();
        t.on_zone("Timorous Deep");
        t.on_your_say(100, "Hail, Crusader Iktra");
        t.on_npc_line(
            101,
            "Crusader Iktra",
            "Hail yourself. I could use some [supplies].",
        );
        t.on_your_say(120, "supplies");
        t.on_npc_line(
            121,
            "Crusader Iktra",
            "Bring me bits of metal, in batches of four.",
        );
        t.on_faction(200, "New Sebilisian Expedition", 10);
        t.on_faction(200, "New Sebilisian Expedition", 10);
        t.on_trade_complete(201, "Crusader Iktra");

        let r = t.report();
        assert_eq!(r.npcs.len(), 1);
        let n = &r.npcs[0];
        assert_eq!(n.npc, "Crusader Iktra");
        assert_eq!(n.zone, "Timorous Deep");
        assert_eq!(n.lines.len(), 4);
        assert!(n.lines[0].yours);
        assert_eq!(n.keywords, vec!["supplies"]);
        assert_eq!(n.turn_ins.len(), 1);
        // Faction is summed: a guide wants "20", not two tens.
        assert_eq!(
            n.turn_ins[0].faction,
            vec![("New Sebilisian Expedition".into(), 20)]
        );
    }

    /// A busy quest NPC answers everyone in earshot. Only the replies that
    /// followed something you did belong in a write-up.
    #[test]
    fn lines_aimed_at_other_players_are_marked_not_yours() {
        let mut t = NpcTracker::new();
        t.on_your_say(100, "Hail, Crusader Iktra");
        t.on_npc_line(
            100,
            "Crusader Iktra",
            "Hail yourself. I could use some [supplies].",
        );
        // Another player's turn-in, well after anything you said.
        t.on_npc_line(140, "Crusader Iktra", "Thank you, Hotdogs. This will help.");
        t.on_your_say(160, "supplies");
        t.on_npc_line(161, "Crusader Iktra", "Bring me bits of metal.");

        let n = &t.report().npcs[0];
        let mine: Vec<&str> = n
            .lines
            .iter()
            .filter(|l| l.to_you && !l.yours)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(mine.len(), 2, "only the two answers: {mine:?}");
        assert!(mine.iter().all(|t| !t.contains("Hotdogs")));
        // The stray line is kept, just not claimed.
        assert_eq!(n.lines.len(), 5);
    }

    /// A keyword offered to somebody else is not one you were given.
    #[test]
    fn keywords_come_only_from_lines_answering_you() {
        let mut t = NpcTracker::new();
        t.on_your_say(100, "Hail, Crusader Iktra");
        t.on_npc_line(100, "Crusader Iktra", "I could use some [supplies].");
        t.on_npc_line(
            200,
            "Crusader Iktra",
            "Ask me about [something else] entirely.",
        );
        assert_eq!(t.report().npcs[0].keywords, vec!["supplies"]);
    }

    /// The client repeats a line once per item in a stacked turn-in — twenty-six
    /// times in the real log. One utterance, recorded once.
    #[test]
    fn a_line_repeated_within_a_second_is_recorded_once() {
        let mut t = NpcTracker::new();
        t.on_your_say(100, "Hail, Crusader Iktra");
        for _ in 0..26 {
            t.on_npc_line(150, "Crusader Iktra", "This will keep our equipment going.");
        }
        let n = &t.report().npcs[0];
        assert_eq!(n.lines.iter().filter(|l| !l.yours).count(), 1);
    }

    /// Without a hail there is no conversation: a passing guard's greeting is
    /// not a quest.
    #[test]
    fn an_npc_you_never_addressed_is_not_recorded() {
        let mut t = NpcTracker::new();
        t.on_npc_line(100, "a Freeport guard", "Hail, citizen.");
        assert!(t.report().npcs.is_empty());
    }

    /// A hail long past does not keep collecting whatever an NPC says later.
    #[test]
    fn a_conversation_closes_after_a_long_silence() {
        let mut t = NpcTracker::new();
        t.on_your_say(100, "Hail, Crusader Iktra");
        t.on_npc_line(101, "Crusader Iktra", "Hail yourself.");
        t.on_npc_line(100_000, "Crusader Iktra", "Another victory for the Legion.");
        let n = &t.report().npcs[0];
        assert_eq!(n.lines.len(), 2, "the late line is not part of it");
    }

    /// An NPC greeted once and never engaged further is noise.
    #[test]
    fn a_bare_hail_with_no_reply_is_not_reported() {
        let mut t = NpcTracker::new();
        t.on_your_say(100, "Hail, Some Merchant");
        assert!(t.report().npcs.is_empty());
    }
}
