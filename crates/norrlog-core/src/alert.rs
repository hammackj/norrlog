//! Rules that watch the log and make a noise when something happens.
//!
//! The value of an alert is that it reaches you when your eyes are elsewhere —
//! a charm that drops while you are reading chat, a snare wearing off mid-pull.
//! A panel cannot do that; a sound can. Everything here is about deciding
//! *when* to make one. What the noise sounds like is the client's business.
//!
//! # Two ways in
//!
//! [`AlertBook::on_line`] sees every parsed line and handles the triggers that
//! a line alone can answer: a substring, a regular expression, and the event
//! kinds that are one-to-one with a log line (you died, you levelled, an item
//! dropped).
//!
//! [`AlertBook::on_signal`] takes the conclusions only the engine can reach.
//! "Charm broke" is not a line — it is a fading effect that the spell database
//! says was a charm, on a creature the tracker knows you had charmed. Asking
//! this module to re-derive that would mean duplicating the pipeline, so the
//! engine states it instead.
//!
//! # Why substring matching is the headline feature
//!
//! Anyone can point at a line they just saw and say "tell me when that
//! happens". Nobody wants to write a regular expression to do it. The typed
//! triggers are more robust and should be preferred where one exists, but the
//! substring rule is what makes the feature usable without reading this file.
//!
//! # Firing only while live
//!
//! The daemon parses the entire log at startup — often hundreds of thousands of
//! lines. Every one of them would match something. [`AlertBook::arm`] gates all
//! firing, and the engine arms the book only once it is following live, so
//! starting the app is silent no matter what the backlog contains.

use crate::event::Event;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How many past fires are kept for clients to catch up on.
///
/// Clients replay by sequence number, so this only has to cover the gap
/// between two snapshots. It is generous because the cost is a few strings.
const FIRE_HISTORY: usize = 50;

/// Default gap before the same rule may fire again for the same subject.
const DEFAULT_COOLDOWN: i64 = 10;

/// A conclusion the engine reached that no single line states.
///
/// Kept separate from [`Event`] because these are derived facts, not parses:
/// they depend on the spell database, the encounter tracker and the buff
/// timers agreeing with each other.
#[derive(Debug, Clone)]
pub enum Signal<'a> {
    /// A creature you had charmed is no longer yours.
    CharmBroken { target: &'a str },
    /// A charm failed to take at all.
    CharmResisted { target: &'a str, how: &'a str },
    /// A raid target died and the kill was credited to you.
    BossKilled { boss: &'a str },
    /// A tracked buff is close to expiring. Emitted once per crossing; see
    /// [`AlertBook::on_buffs`].
    BuffExpiring { name: &'a str, remaining: i64 },
}

impl Signal<'_> {
    fn kind(&self) -> SignalKind {
        match self {
            Signal::CharmBroken { .. } => SignalKind::CharmBroken,
            Signal::CharmResisted { .. } => SignalKind::CharmResisted,
            Signal::BossKilled { .. } => SignalKind::BossKilled,
            Signal::BuffExpiring { .. } => SignalKind::BuffExpiring,
        }
    }

    /// The thing the signal is about — a creature, a buff, a boss. Used both
    /// for the optional name filter and to scope the cooldown, so two charms
    /// breaking at once produce two alerts rather than one.
    fn subject(&self) -> &str {
        match self {
            Signal::CharmBroken { target } => target,
            Signal::CharmResisted { target, .. } => target,
            Signal::BossKilled { boss } => boss,
            Signal::BuffExpiring { name, .. } => name,
        }
    }

    fn detail(&self) -> String {
        // Two of the three charm failure lines name nobody, and the engine can
        // only supply a target if it saw the cast. An unnamed one still needs
        // to read as a sentence.
        let named = |t: &str| {
            if t.trim().is_empty() {
                "your target".to_string()
            } else {
                t.to_string()
            }
        };
        match self {
            Signal::CharmBroken { target } => format!("Charm broke: {}", named(target)),
            Signal::CharmResisted { target, how } => format!("Charm {how}: {}", named(target)),
            Signal::BossKilled { boss } => format!("Killed {boss}"),
            Signal::BuffExpiring { name, remaining } => format!("{name} — {remaining}s left"),
        }
    }
}

/// The typed happenings a rule can watch for.
///
/// Deliberately a short list. Every entry here is something a person would ask
/// for out loud; anything rarer is served by a substring rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    /// A creature you charmed came loose. The subject is the creature.
    CharmBroken,
    /// A charm was resisted, was immune, or landed on nothing.
    CharmResisted,
    /// You died. The subject is the killer, when the log names one.
    YouDied,
    /// A raid target you were credited with died.
    BossKilled,
    /// You gained a level.
    LevelUp,
    /// You gained ability points.
    AaGained,
    /// An item entered your inventory. The subject is the item.
    Loot,
    /// Someone sent you a tell. The subject is the sender.
    Tell,
    /// One of your spells was resisted. The subject is the spell.
    SpellResisted,
    /// A task advanced a step. The subject is the task.
    TaskUpdated,
    /// An achievement was earned.
    Achievement,
    /// A tracked buff is running out. The subject is the buff.
    BuffExpiring,
}

impl SignalKind {
    /// Whether this kind is raised from a signal rather than read off a line.
    /// Used only to keep the two dispatch paths from double-firing.
    fn is_derived(self) -> bool {
        matches!(
            self,
            SignalKind::CharmBroken
                | SignalKind::CharmResisted
                | SignalKind::BossKilled
                | SignalKind::BuffExpiring
        )
    }
}

/// What a rule watches for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    /// Case-insensitive substring of the log line. The plain-language option,
    /// and the one the "alert me about this line" button produces.
    Contains { text: String },
    /// A regular expression over the log line, for when a substring is too
    /// blunt — a number range, alternatives, an anchored name.
    Pattern { pattern: String },
    /// A typed happening, optionally narrowed to one subject.
    ///
    /// `name` is a case-insensitive substring of the subject, so a rule can
    /// watch every charm break or only the one creature that matters.
    Signal {
        kind: SignalKind,
        #[serde(default)]
        name: Option<String>,
    },
    /// A tracked buff dropping below `seconds` remaining.
    ///
    /// Its own variant rather than a [`Trigger::Signal`] because the threshold
    /// belongs to the rule: two people want warnings at different times, and
    /// one of them wants it only for a single song.
    BuffExpiring {
        seconds: i64,
        #[serde(default)]
        name: Option<String>,
    },
}

/// The noise to make. Synthesised by the client, so this is a name, not a file.
///
/// Sounds are shipped as tone recipes rather than audio assets on purpose: no
/// licensing, no download, and the alert stays under a tenth of a second from
/// log line to speaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    #[default]
    Chime,
    Bell,
    Alarm,
    Buzz,
    Blip,
    Thud,
    /// Silent. For a rule that only speaks, or only wants the on-screen entry.
    None,
}

fn default_enabled() -> bool {
    true
}

fn default_cooldown() -> i64 {
    DEFAULT_COOLDOWN
}

/// One alert the player set up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    /// Stable identity. Also the cooldown key, so renaming a rule does not
    /// reset it mid-fight.
    pub id: String,
    /// What to call it on screen, and what to say aloud unless `say` overrides.
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub trigger: Trigger,
    #[serde(default)]
    pub sound: Sound,
    /// Speak the alert as well as sounding it.
    #[serde(default)]
    pub speak: bool,
    /// What to say, when the rule's name is not the right words. Ignored
    /// unless `speak`.
    #[serde(default)]
    pub say: Option<String>,
    /// Seconds before this rule may fire again *for the same subject*.
    ///
    /// Per subject rather than per rule: a rule watching charm breaks should
    /// still announce the second creature that came loose a moment after the
    /// first, but should not repeat itself about the same one.
    #[serde(default = "default_cooldown")]
    pub cooldown: i64,
    /// Why this rule can never fire, when that is the case — a regular
    /// expression that would not compile. Reported rather than swallowed, so a
    /// typo shows up as a broken rule instead of silence.
    ///
    /// Read in as well as out, so a client can show it. Whatever a client
    /// sends back is ignored: compiling a rule resets it.
    #[serde(default)]
    pub error: Option<String>,
}

/// One alert that went off.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertFire {
    /// Monotonic across the life of the daemon. Clients play everything newer
    /// than the last sequence they saw, which makes delivery idempotent across
    /// reconnects and duplicate snapshots.
    pub seq: u64,
    pub at: i64,
    pub rule_id: String,
    pub name: String,
    /// What actually happened: the log line, or the signal in words.
    pub detail: String,
    pub sound: Sound,
    /// Text to speak, already resolved from the rule.
    pub say: Option<String>,
}

/// The rule set, its compiled patterns, and what has fired recently.
#[derive(Default)]
pub struct AlertBook {
    rules: Vec<AlertRule>,
    /// Compiled patterns by rule id. Absent for rules that are not regular
    /// expressions, or whose pattern failed to compile.
    patterns: HashMap<String, Regex>,
    /// (rule id, subject) -> when it last fired.
    last_fired: HashMap<(String, String), i64>,
    /// (rule id, buff name) -> the remaining time we warned at, so a refreshed
    /// buff can warn again but a ticking one cannot.
    warned: HashMap<(String, String), i64>,
    fires: Vec<AlertFire>,
    seq: u64,
    armed: bool,
}

impl AlertBook {
    pub fn new() -> AlertBook {
        AlertBook::default()
    }

    /// Allow or forbid firing. The engine arms the book when it starts
    /// following live; until then every match is discarded.
    pub fn arm(&mut self, armed: bool) {
        self.armed = armed;
    }

    pub fn rules(&self) -> &[AlertRule] {
        &self.rules
    }

    /// Replace the whole rule set, compiling any patterns.
    pub fn set_rules(&mut self, rules: Vec<AlertRule>) {
        self.patterns.clear();
        self.rules = rules;
        for rule in &mut self.rules {
            rule.error = None;
            if let Trigger::Pattern { pattern } = &rule.trigger {
                // Case-insensitive by default: the log's casing varies with
                // sentence position, and nobody writing "gate" means only the
                // lowercase one.
                match Regex::new(&format!("(?i){pattern}")) {
                    Ok(re) => {
                        self.patterns.insert(rule.id.clone(), re);
                    }
                    Err(e) => rule.error = Some(e.to_string()),
                }
            }
        }
        // A rule that is gone should not hold a cooldown or a warning against
        // its id, or re-adding it would start it in a fired state.
        let live: Vec<&str> = self.rules.iter().map(|r| r.id.as_str()).collect();
        self.last_fired
            .retain(|(id, _), _| live.iter().any(|l| l == id));
        self.warned
            .retain(|(id, _), _| live.iter().any(|l| l == id));
    }

    /// Add or replace one rule, keeping the rest.
    pub fn put(&mut self, rule: AlertRule) {
        let mut rules = self.rules.clone();
        match rules.iter().position(|r| r.id == rule.id) {
            Some(i) => rules[i] = rule,
            None => rules.push(rule),
        }
        self.set_rules(rules);
    }

    pub fn remove(&mut self, id: &str) {
        let rules = self.rules.iter().filter(|r| r.id != id).cloned().collect();
        self.set_rules(rules);
    }

    /// Fires newer than `since`, oldest first, so a client can play them in
    /// order.
    pub fn since(&self, since: u64) -> Vec<AlertFire> {
        self.fires
            .iter()
            .filter(|f| f.seq > since)
            .cloned()
            .collect()
    }

    /// The highest sequence issued. A client records this on its first
    /// snapshot without playing anything, so connecting mid-session does not
    /// replay the last few minutes at you.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Evaluate every line-level trigger against one parsed line.
    pub fn on_line(&mut self, at: i64, raw: &str, event: &Event, player: &str) {
        if !self.armed || self.rules.is_empty() {
            return;
        }
        let text = strip_timestamp(raw);
        let lowered = text.to_lowercase();
        let derived = signal_from_event(event, player);

        for i in 0..self.rules.len() {
            let rule = &self.rules[i];
            if !rule.enabled || rule.error.is_some() {
                continue;
            }
            let hit: Option<(String, String)> = match &rule.trigger {
                Trigger::Contains { text: needle } => {
                    let needle = needle.trim().to_lowercase();
                    // An empty substring matches every line; treat it as a rule
                    // that is not finished being written rather than as a rule
                    // that fires constantly.
                    if !needle.is_empty() && lowered.contains(&needle) {
                        Some((String::new(), text.to_string()))
                    } else {
                        None
                    }
                }
                Trigger::Pattern { .. } => self
                    .patterns
                    .get(&rule.id)
                    .filter(|re| re.is_match(text))
                    .map(|_| (String::new(), text.to_string())),
                Trigger::Signal { kind, name } => match &derived {
                    Some((k, subject)) if k == kind && !kind.is_derived() => {
                        if name_matches(name.as_deref(), subject) {
                            Some((subject.clone(), text.to_string()))
                        } else {
                            None
                        }
                    }
                    _ => None,
                },
                Trigger::BuffExpiring { .. } => None,
            };
            if let Some((subject, detail)) = hit {
                self.fire(i, at, subject, detail);
            }
        }
    }

    /// Evaluate the signal triggers against one derived conclusion.
    pub fn on_signal(&mut self, at: i64, signal: Signal<'_>) {
        if !self.armed || self.rules.is_empty() {
            return;
        }
        let kind = signal.kind();
        let subject = signal.subject().to_string();
        for i in 0..self.rules.len() {
            let rule = &self.rules[i];
            if !rule.enabled || rule.error.is_some() {
                continue;
            }
            let matched = match &rule.trigger {
                Trigger::Signal { kind: k, name } => {
                    *k == kind && name_matches(name.as_deref(), &subject)
                }
                _ => false,
            };
            if matched {
                self.fire(i, at, subject.clone(), signal.detail());
            }
        }
    }

    /// Check every buff against the expiry rules.
    ///
    /// Called on each publish with the currently tracked buffs and their
    /// remaining seconds. Warning is edge-triggered per buff: crossing the
    /// threshold fires once, and only a refresh — remaining going back up —
    /// re-arms it. That is exact, so these rules need no cooldown at all.
    pub fn on_buffs(&mut self, at: i64, buffs: &[(String, i64)]) {
        if !self.armed || self.rules.is_empty() {
            return;
        }
        for i in 0..self.rules.len() {
            let rule = &self.rules[i];
            let Trigger::BuffExpiring { seconds, name } = &rule.trigger else {
                continue;
            };
            if !rule.enabled {
                continue;
            }
            let (threshold, filter) = (*seconds, name.clone());
            for (buff, remaining) in buffs {
                if !name_matches(filter.as_deref(), buff) {
                    continue;
                }
                let key = (self.rules[i].id.clone(), buff.clone());
                if *remaining > threshold {
                    // Back above the line: either it was refreshed or it never
                    // got close. Either way the next crossing is news again.
                    self.warned.remove(&key);
                    continue;
                }
                // A buff at zero has already gone; warning then is just noise.
                if *remaining <= 0 || self.warned.contains_key(&key) {
                    continue;
                }
                self.warned.insert(key, *remaining);
                let detail = format!("{buff} — {remaining}s left");
                self.fire(i, at, buff.clone(), detail);
            }
        }
        // Forget buffs that are no longer up, so the same song landing again
        // later can warn once more.
        let present: Vec<&String> = buffs.iter().map(|(n, _)| n).collect();
        self.warned
            .retain(|(_, buff), _| present.iter().any(|p| *p == buff));
    }

    /// Record a fire, honouring the per-subject cooldown.
    fn fire(&mut self, index: usize, at: i64, subject: String, detail: String) {
        let rule = &self.rules[index];
        let key = (rule.id.clone(), subject);
        if let Some(prev) = self.last_fired.get(&key) {
            // `at` is log time, which can step backwards when a log is
            // replayed or a clock adjusts; a negative gap counts as elapsed.
            if at >= *prev && at - *prev < rule.cooldown {
                return;
            }
        }
        self.last_fired.insert(key, at);
        self.seq += 1;
        let say = if rule.speak {
            Some(rule.say.clone().unwrap_or_else(|| rule.name.clone()))
        } else {
            None
        };
        self.fires.push(AlertFire {
            seq: self.seq,
            at,
            rule_id: rule.id.clone(),
            name: rule.name.clone(),
            detail,
            sound: rule.sound,
            say,
        });
        if self.fires.len() > FIRE_HISTORY {
            let drop = self.fires.len() - FIRE_HISTORY;
            self.fires.drain(0..drop);
        }
    }
}

/// Does an optional name filter accept this subject?
///
/// Substring and case-insensitive: people type "clarity", not the exact rank
/// the client logged.
fn name_matches(filter: Option<&str>, subject: &str) -> bool {
    match filter {
        None => true,
        Some(f) if f.trim().is_empty() => true,
        Some(f) => subject.to_lowercase().contains(&f.trim().to_lowercase()),
    }
}

/// The line-level signal an event represents, with its subject.
fn signal_from_event(event: &Event, player: &str) -> Option<(SignalKind, String)> {
    match event {
        Event::Death { victim, killer } if victim.is_you() => Some((
            SignalKind::YouDied,
            killer
                .as_ref()
                .map(|k| k.display(player))
                .unwrap_or_else(|| "something".to_string()),
        )),
        Event::LevelUp { level } => Some((SignalKind::LevelUp, level.to_string())),
        Event::AbilityPointsGained { points, .. } => {
            Some((SignalKind::AaGained, points.to_string()))
        }
        Event::Loot { item, .. } => Some((SignalKind::Loot, item.clone())),
        // Only tells *to* you. Your own outgoing message is not news.
        Event::Tell { from, to, .. } if to.is_you() && !from.is_you() => {
            Some((SignalKind::Tell, from.display(player)))
        }
        Event::Resisted { spell, .. } => Some((SignalKind::SpellResisted, spell.clone())),
        Event::TaskUpdated { task } => Some((SignalKind::TaskUpdated, task.clone())),
        Event::AchievementEarned { name } => Some((SignalKind::Achievement, name.clone())),
        _ => None,
    }
}

/// Drop the client's `[Wed Aug 06 21:15:03 2025]` prefix.
///
/// Alerts are read and spoken, and the timestamp is neither: the UI already
/// shows when it happened, and hearing a date read out is worse than useless.
pub fn strip_timestamp(line: &str) -> &str {
    let t = line.trim();
    if let Some(rest) = t.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return rest[end + 1..].trim_start();
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Parser;

    fn rule(id: &str, trigger: Trigger) -> AlertRule {
        AlertRule {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            trigger,
            sound: Sound::Chime,
            speak: false,
            say: None,
            cooldown: DEFAULT_COOLDOWN,
            error: None,
        }
    }

    fn armed(rules: Vec<AlertRule>) -> AlertBook {
        let mut b = AlertBook::new();
        b.set_rules(rules);
        b.arm(true);
        b
    }

    #[test]
    fn substring_matches_case_insensitively() {
        let mut b = armed(vec![rule(
            "r1",
            Trigger::Contains {
                text: "your charm spell has worn off".to_string(),
            },
        )]);
        b.on_line(
            100,
            "[Wed Aug 06 21:15:03 2025] Your charm spell has worn off.",
            &Event::Unknown,
            "Tharn",
        );
        assert_eq!(b.since(0).len(), 1);
        // The timestamp is stripped from what gets shown and spoken.
        assert_eq!(b.since(0)[0].detail, "Your charm spell has worn off.");
    }

    /// An unfinished rule with an empty substring would otherwise match every
    /// line in the log.
    #[test]
    fn empty_substring_never_matches() {
        let mut b = armed(vec![rule(
            "r1",
            Trigger::Contains {
                text: "  ".to_string(),
            },
        )]);
        b.on_line(100, "[x] anything at all", &Event::Unknown, "Tharn");
        assert!(b.since(0).is_empty());
    }

    #[test]
    fn nothing_fires_until_armed() {
        let mut b = AlertBook::new();
        b.set_rules(vec![rule(
            "r1",
            Trigger::Contains {
                text: "hits you".to_string(),
            },
        )]);
        b.on_line(
            100,
            "[x] A mummy hits YOU for 40 points",
            &Event::Unknown,
            "V",
        );
        assert!(b.since(0).is_empty(), "backlog must be silent");
        b.arm(true);
        b.on_line(
            101,
            "[x] A mummy hits YOU for 40 points",
            &Event::Unknown,
            "V",
        );
        assert_eq!(b.since(0).len(), 1);
    }

    #[test]
    fn cooldown_is_per_subject() {
        let mut b = armed(vec![rule(
            "r1",
            Trigger::Signal {
                kind: SignalKind::CharmBroken,
                name: None,
            },
        )]);
        b.on_signal(100, Signal::CharmBroken { target: "a mummy" });
        // Same creature again inside the cooldown: silence.
        b.on_signal(102, Signal::CharmBroken { target: "a mummy" });
        // A different creature is a different problem, and still gets said.
        b.on_signal(102, Signal::CharmBroken { target: "a scarab" });
        let fires = b.since(0);
        assert_eq!(fires.len(), 2);
        assert_eq!(fires[1].detail, "Charm broke: a scarab");
    }

    #[test]
    fn cooldown_expires() {
        let mut r = rule(
            "r1",
            Trigger::Signal {
                kind: SignalKind::CharmBroken,
                name: None,
            },
        );
        r.cooldown = 5;
        let mut b = armed(vec![r]);
        b.on_signal(100, Signal::CharmBroken { target: "a mummy" });
        b.on_signal(104, Signal::CharmBroken { target: "a mummy" });
        b.on_signal(106, Signal::CharmBroken { target: "a mummy" });
        assert_eq!(b.since(0).len(), 2);
    }

    #[test]
    fn signal_name_filter_narrows_to_one_subject() {
        let mut b = armed(vec![rule(
            "r1",
            Trigger::Signal {
                kind: SignalKind::Loot,
                name: Some("mummy wrap".to_string()),
            },
        )]);
        let p = |s: &str| Parser::new("Tharn").parse_line(s).unwrap();
        let looted = |item: &str| {
            format!(
                "[Wed Aug 06 21:15:03 2025] --You have looted a {item} from a mummy's corpse.--"
            )
        };
        b.on_line(1, "x", &p(&looted("Rusty Dagger")).event, "V");
        assert!(b.since(0).is_empty());
        b.on_line(2, "x", &p(&looted("Mummy Wrap")).event, "V");
        assert_eq!(b.since(0).len(), 1);
    }

    /// Your own outgoing tell is not something to be alerted about.
    #[test]
    fn only_incoming_tells_fire() {
        let mut b = armed(vec![rule(
            "r1",
            Trigger::Signal {
                kind: SignalKind::Tell,
                name: None,
            },
        )]);
        let p = |s: &str| Parser::new("Tharn").parse_line(s).unwrap();
        b.on_line(
            1,
            "x",
            &p("[Wed Aug 06 21:15:03 2025] You told Bob, 'hi'").event,
            "Tharn",
        );
        assert!(b.since(0).is_empty());
        b.on_line(
            2,
            "x",
            &p("[Wed Aug 06 21:15:04 2025] Bob tells you, 'hi'").event,
            "Tharn",
        );
        assert_eq!(b.since(0).len(), 1);
    }

    /// The whole point of the edge trigger: a buff counting down past the
    /// threshold warns once, not on every tick for the rest of its life.
    #[test]
    fn buff_expiry_warns_once_per_crossing() {
        let mut b = armed(vec![rule(
            "r1",
            Trigger::BuffExpiring {
                seconds: 30,
                name: None,
            },
        )]);
        b.on_buffs(100, &[("Clarity".to_string(), 40)]);
        assert!(b.since(0).is_empty());
        b.on_buffs(110, &[("Clarity".to_string(), 29)]);
        b.on_buffs(120, &[("Clarity".to_string(), 20)]);
        b.on_buffs(130, &[("Clarity".to_string(), 10)]);
        assert_eq!(b.since(0).len(), 1, "one warning per crossing");

        // Recast: it goes back up, and the next crossing is news again.
        b.on_buffs(140, &[("Clarity".to_string(), 600)]);
        b.on_buffs(150, &[("Clarity".to_string(), 25)]);
        assert_eq!(b.since(0).len(), 2);
    }

    /// A buff that has already gone is not "about to expire".
    #[test]
    fn buff_at_zero_does_not_warn() {
        let mut b = armed(vec![rule(
            "r1",
            Trigger::BuffExpiring {
                seconds: 30,
                name: None,
            },
        )]);
        b.on_buffs(100, &[("Clarity".to_string(), 0)]);
        assert!(b.since(0).is_empty());
    }

    /// A dropped buff clears its warning, so the same song landing later warns
    /// again rather than staying permanently silent.
    #[test]
    fn buff_warning_resets_when_the_buff_drops() {
        let mut b = armed(vec![rule(
            "r1",
            Trigger::BuffExpiring {
                seconds: 30,
                name: None,
            },
        )]);
        b.on_buffs(100, &[("Clarity".to_string(), 10)]);
        b.on_buffs(110, &[]);
        b.on_buffs(120, &[("Clarity".to_string(), 10)]);
        assert_eq!(b.since(0).len(), 2);
    }

    #[test]
    fn bad_pattern_is_reported_not_swallowed() {
        let mut b = AlertBook::new();
        b.set_rules(vec![rule(
            "r1",
            Trigger::Pattern {
                pattern: "hits you for (".to_string(),
            },
        )]);
        b.arm(true);
        assert!(b.rules()[0].error.is_some());
        b.on_line(
            1,
            "[x] A mummy hits YOU for 40 points",
            &Event::Unknown,
            "V",
        );
        assert!(b.since(0).is_empty());
    }

    #[test]
    fn pattern_matches() {
        let mut b = armed(vec![rule(
            "r1",
            Trigger::Pattern {
                pattern: r"hits YOU for (\d{3,})".to_string(),
            },
        )]);
        b.on_line(
            1,
            "[x] A mummy hits YOU for 40 points",
            &Event::Unknown,
            "V",
        );
        assert!(b.since(0).is_empty(), "two digits is under the threshold");
        b.on_line(
            2,
            "[x] A mummy hits YOU for 480 points",
            &Event::Unknown,
            "V",
        );
        assert_eq!(b.since(0).len(), 1);
    }

    #[test]
    fn since_replays_only_what_is_new() {
        let mut b = armed(vec![rule(
            "r1",
            Trigger::Signal {
                kind: SignalKind::CharmBroken,
                name: None,
            },
        )]);
        b.on_signal(100, Signal::CharmBroken { target: "a" });
        let seq = b.seq();
        assert_eq!(b.since(seq).len(), 0);
        b.on_signal(200, Signal::CharmBroken { target: "b" });
        assert_eq!(b.since(seq).len(), 1);
    }

    #[test]
    fn disabled_rules_are_inert() {
        let mut r = rule(
            "r1",
            Trigger::Contains {
                text: "anything".to_string(),
            },
        );
        r.enabled = false;
        let mut b = armed(vec![r]);
        b.on_line(1, "[x] anything", &Event::Unknown, "V");
        assert!(b.since(0).is_empty());
    }

    /// Removing a rule must not leave its cooldown behind: re-adding it should
    /// start clean rather than silently swallowing the first fire.
    #[test]
    fn removing_a_rule_clears_its_cooldown() {
        let r = rule(
            "r1",
            Trigger::Signal {
                kind: SignalKind::CharmBroken,
                name: None,
            },
        );
        let mut b = armed(vec![r.clone()]);
        b.on_signal(100, Signal::CharmBroken { target: "a mummy" });
        b.remove("r1");
        b.put(r);
        b.on_signal(101, Signal::CharmBroken { target: "a mummy" });
        assert_eq!(b.since(0).len(), 2);
    }
}
