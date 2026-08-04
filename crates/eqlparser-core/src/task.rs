//! Task and achievement tracking, with quest dialogue capture.
//!
//! # What the client tells us, and what it does not
//!
//! ```text
//! You have been assigned the task 'Renouncing Your Faith'.
//! Your task 'Renouncing Your Faith' has been updated.
//! ```
//!
//! That is all. No objective text, no "step 2 of 5", no completion message.
//! Each "has been updated" is exactly one step completed — verified against
//! independent data, since the weekly boss tasks tick at the same second as the
//! parsed kill.
//!
//! # Working out *which* step
//!
//! A bare step count cannot say which objective was met. But most steps are
//! "kill X", and kills are already parsed — so when a step definition names a
//! target, the update is matched against kills in the seconds before it. That
//! turns "1 step done" into "step 2 (kill Innoruuk) done, step 1 outstanding",
//! which is the difference between a counter and something you can write a
//! guide from.
//!
//! Step definitions cannot be derived from the log; they are seeded by hand
//! (see [`TaskTracker::define`]) and persisted by the caller.
//!
//! # Dialogue
//!
//! Quest text is ordinary say-channel chat, so it is captured in a rolling
//! buffer and attached to whichever task is assigned or advanced next. NPC
//! prompts carry `[bracketed keywords]`, which are extracted separately as
//! they are the actual steps a guide would list.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How far back to look for the kill that satisfied a step.
const STEP_KILL_WINDOW: i64 = 30;

/// How long quest dialogue stays eligible to attach to a task.
///
/// Generous: a hail-and-keyword conversation runs over a minute or two before
/// the task is finally handed out.
const DIALOGUE_WINDOW: i64 = 300;

/// A defined objective within a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStepDef {
    pub description: String,
    /// The creature whose death satisfies this step, when it is a kill step.
    /// Matched case-insensitively against the victim's name.
    #[serde(default)]
    pub kill_target: Option<String>,
}

/// A hand-seeded description of a task's objectives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDef {
    pub name: String,
    pub steps: Vec<TaskStepDef>,
}

/// One completed step, with whatever we could work out about which it was.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepCompletion {
    pub at: i64,
    /// Index into the task definition, when it could be identified.
    pub step_index: Option<usize>,
    /// What was killed at that moment, if anything.
    pub killed: Option<String>,
}

/// A line of quest dialogue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogueLine {
    pub at: i64,
    pub speaker: String,
    pub text: String,
    /// `[bracketed keywords]` offered by an NPC — the prompts a guide lists.
    pub keywords: Vec<String>,
}

/// One task and its progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub name: String,
    pub assigned: Option<i64>,
    pub completions: Vec<StepCompletion>,
    pub weekly: bool,
    /// Objectives, when the task has been defined.
    pub steps: Vec<TaskStepDef>,
    /// Quest text captured around assignment and progress.
    pub dialogue: Vec<DialogueLine>,
}

impl TaskRecord {
    pub fn steps_done(&self) -> usize {
        self.completions.len()
    }

    pub fn last_progress(&self) -> Option<i64> {
        self.completions.last().map(|c| c.at)
    }

    /// Whether each defined step has been completed. Empty when undefined.
    pub fn step_states(&self) -> Vec<bool> {
        let mut done = vec![false; self.steps.len()];
        for c in &self.completions {
            if let Some(i) = c.step_index {
                if i < done.len() {
                    done[i] = true;
                }
            }
        }
        done
    }

    /// Completions that could not be tied to a specific step.
    pub fn unidentified(&self) -> usize {
        self.completions
            .iter()
            .filter(|c| c.step_index.is_none())
            .count()
    }
}

/// An achievement earned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Achievement {
    pub name: String,
    pub at: i64,
}

/// Everything the tasks view shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReport {
    pub tasks: Vec<TaskRecord>,
    pub achievements: Vec<Achievement>,
    /// Definitions currently seeded, including for tasks not yet assigned.
    pub definitions: Vec<TaskDef>,
}

/// Task definitions known ahead of any player input.
///
/// The client never states objectives, so these are transcribed by hand. They
/// are only a starting point — anything the caller loads from disk replaces
/// them, and the player can define more.
pub fn seed_definitions() -> Vec<TaskDef> {
    vec![TaskDef {
        name: "Renouncing Your Faith".to_string(),
        steps: vec![
            TaskStepDef {
                description: "Kill Cazic-Thule".to_string(),
                kill_target: Some("Cazic-Thule".to_string()),
            },
            TaskStepDef {
                description: "Kill Innoruuk, the Prince of Hate".to_string(),
                kill_target: Some("Innoruuk".to_string()),
            },
        ],
    }]
}

/// Accumulates task activity, dialogue and achievements.
#[derive(Debug, Default)]
pub struct TaskTracker {
    tasks: HashMap<String, TaskRecord>,
    achievements: Vec<Achievement>,
    defs: HashMap<String, TaskDef>,
    /// Recent kills, for identifying which step an update satisfied.
    recent_kills: Vec<(i64, String)>,
    /// Rolling say-channel buffer awaiting a task to attach to.
    recent_dialogue: Vec<DialogueLine>,
}

impl TaskTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a task's objectives. Applies to an already-assigned task too.
    pub fn define(&mut self, def: TaskDef) {
        let key = def.name.to_lowercase();
        if let Some(t) = self.tasks.get_mut(&key) {
            t.steps = def.steps.clone();
            // Re-identify completions now that targets are known.
            let steps = def.steps.clone();
            for c in t.completions.iter_mut() {
                if c.step_index.is_none() {
                    if let Some(killed) = &c.killed {
                        c.step_index = match_step(&steps, killed);
                    }
                }
            }
        }
        self.defs.insert(key, def);
    }

    pub fn definitions(&self) -> Vec<TaskDef> {
        let mut v: Vec<TaskDef> = self.defs.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Note a kill, so a following step update can be identified.
    pub fn on_kill(&mut self, timestamp: i64, victim: &str) {
        self.recent_kills.push((timestamp, victim.to_string()));
        let cutoff = timestamp - STEP_KILL_WINDOW;
        self.recent_kills.retain(|(t, _)| *t >= cutoff);

        // The server credits the task as it resolves the death, which it does
        // *before* writing the death message. Observed on Innoruuk:
        //   22:36:42  Your task 'Renouncing Your Faith' has been updated.
        //   22:36:43  Innoruuk, the Prince of Hate has been slain by ...
        // So a completion recorded moments ago may only now be explainable.
        // The same one-line inversion shows up in experience attribution.
        for rec in self.tasks.values_mut() {
            let steps = rec.steps.clone();
            for c in rec.completions.iter_mut().rev() {
                if timestamp - c.at > STEP_KILL_WINDOW {
                    break;
                }
                if c.step_index.is_some() {
                    continue;
                }
                // Only claim it when the kill matches an objective this task
                // actually lists — otherwise any nearby death would be
                // credited to any unexplained step.
                if let Some(i) = match_step(&steps, victim) {
                    c.step_index = Some(i);
                    c.killed = Some(victim.to_string());
                }
            }
        }
    }

    /// Note a line of say-channel dialogue.
    pub fn on_dialogue(&mut self, timestamp: i64, speaker: &str, text: &str) {
        self.recent_dialogue.push(DialogueLine {
            at: timestamp,
            speaker: speaker.to_string(),
            text: text.to_string(),
            keywords: extract_keywords(text),
        });
        let cutoff = timestamp - DIALOGUE_WINDOW;
        self.recent_dialogue.retain(|d| d.at >= cutoff);
    }

    /// Note that a task was assigned.
    ///
    /// Weekly tasks are handed out again each reset under the same name, so
    /// assignment clears prior progress rather than accumulating across weeks.
    /// Definitions and captured dialogue are deliberately kept: they describe
    /// the task itself, not one run of it.
    pub fn on_assigned(&mut self, timestamp: i64, name: &str) {
        let key = name.to_lowercase();
        let weekly = key.contains("weekly");
        let steps = self
            .defs
            .get(&key)
            .map(|d| d.steps.clone())
            .unwrap_or_default();
        let kept_dialogue = self
            .tasks
            .get(&key)
            .map(|t| t.dialogue.clone())
            .unwrap_or_default();

        let mut rec = TaskRecord {
            name: name.to_string(),
            assigned: Some(timestamp),
            completions: Vec::new(),
            weekly,
            steps,
            dialogue: kept_dialogue,
        };
        Self::absorb_dialogue(&mut rec, &self.recent_dialogue, timestamp);
        self.tasks.insert(key, rec);
    }

    /// Note that a task advanced by one step.
    pub fn on_updated(&mut self, timestamp: i64, name: &str) {
        let key = name.to_lowercase();
        let weekly = key.contains("weekly");
        let defs = self
            .defs
            .get(&key)
            .map(|d| d.steps.clone())
            .unwrap_or_default();

        // Prefer a recent kill that matches a listed objective; fall back to
        // the most recent kill of any kind, which is still the best guess at
        // what the step was when the task is undefined.
        let in_window = |t: &i64| timestamp - t <= STEP_KILL_WINDOW && *t <= timestamp;
        let matched = self
            .recent_kills
            .iter()
            .rev()
            .filter(|(t, _)| in_window(t))
            .find_map(|(_, v)| match_step(&defs, v).map(|i| (v.clone(), i)));
        let (killed, step_index) = match matched {
            Some((v, i)) => (Some(v), Some(i)),
            None => (
                self.recent_kills
                    .iter()
                    .rev()
                    .find(|(t, _)| in_window(t))
                    .map(|(_, v)| v.clone()),
                None,
            ),
        };

        let rec = self.tasks.entry(key).or_insert_with(|| TaskRecord {
            name: name.to_string(),
            // Assigned before logging began.
            assigned: None,
            completions: Vec::new(),
            weekly,
            steps: defs,
            dialogue: Vec::new(),
        });
        rec.completions.push(StepCompletion {
            at: timestamp,
            step_index,
            killed,
        });
        Self::absorb_dialogue(rec, &self.recent_dialogue, timestamp);
    }

    /// Attach recent dialogue to a task, keeping it in order and unique.
    ///
    /// Filtered against the task event's own timestamp rather than trusting the
    /// buffer: the buffer is only pruned when new dialogue arrives, so after a
    /// long silence it still holds lines far too old to be related.
    fn absorb_dialogue(rec: &mut TaskRecord, recent: &[DialogueLine], now: i64) {
        for d in recent.iter().filter(|d| now - d.at <= DIALOGUE_WINDOW) {
            if !rec
                .dialogue
                .iter()
                .any(|x| x.at == d.at && x.text == d.text)
            {
                rec.dialogue.push(d.clone());
            }
        }
        rec.dialogue.sort_by_key(|d| d.at);
    }

    pub fn on_achievement(&mut self, timestamp: i64, name: &str) {
        self.achievements.push(Achievement {
            name: name.to_string(),
            at: timestamp,
        });
    }

    /// Tasks with the most recent activity first.
    pub fn report(&self) -> TaskReport {
        let mut tasks: Vec<TaskRecord> = self.tasks.values().cloned().collect();
        tasks.sort_by_key(|t| std::cmp::Reverse(t.last_progress().or(t.assigned).unwrap_or(0)));
        let mut achievements = self.achievements.clone();
        achievements.sort_by_key(|a| std::cmp::Reverse(a.at));
        TaskReport {
            tasks,
            achievements,
            definitions: self.definitions(),
        }
    }
}

/// Find the step a kill satisfies, by target name.
fn match_step(steps: &[TaskStepDef], killed: &str) -> Option<usize> {
    let k = killed.to_lowercase();
    steps.iter().position(|s| {
        s.kill_target.as_ref().is_some_and(|t| {
            let t = t.to_lowercase();
            k == t || k.contains(&t) || t.contains(&k)
        })
    })
}

/// Pull `[bracketed keywords]` out of NPC dialogue.
fn extract_keywords(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let kw = after[..close].trim();
        if !kw.is_empty() {
            out.push(kw.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renouncing() -> TaskDef {
        TaskDef {
            name: "Renouncing Your Faith".into(),
            steps: vec![
                TaskStepDef {
                    description: "Kill Cazic-Thule".into(),
                    kill_target: Some("Cazic-Thule".into()),
                },
                TaskStepDef {
                    description: "Kill Innoruuk, the Prince of Hate".into(),
                    kill_target: Some("Innoruuk, the Prince of Hate".into()),
                },
            ],
        }
    }

    #[test]
    fn each_update_is_one_step() {
        let mut t = TaskTracker::new();
        t.on_assigned(100, "Renouncing Your Faith");
        t.on_updated(200, "Renouncing Your Faith");
        let r = t.report();
        assert_eq!(r.tasks[0].steps_done(), 1);
        assert_eq!(r.tasks[0].assigned, Some(100));
    }

    /// The whole point of definitions: turn "1 step done" into which step.
    #[test]
    fn a_kill_identifies_which_step_completed() {
        let mut t = TaskTracker::new();
        t.define(renouncing());
        t.on_assigned(100, "Renouncing Your Faith");
        t.on_kill(200, "Innoruuk, the Prince of Hate");
        t.on_updated(201, "Renouncing Your Faith");

        let task = &t.report().tasks[0];
        assert_eq!(task.steps_done(), 1);
        assert_eq!(task.completions[0].step_index, Some(1), "the Innoruuk step");
        assert_eq!(
            task.step_states(),
            vec![false, true],
            "Cazic still outstanding"
        );
        assert_eq!(task.unidentified(), 0);
    }

    /// A kill *before* the task was assigned does not count toward it — which
    /// is exactly what happened on the real log, and is the kind of thing a
    /// guide needs to warn about.
    #[test]
    fn a_kill_before_assignment_leaves_its_step_outstanding() {
        let mut t = TaskTracker::new();
        t.define(renouncing());
        t.on_kill(50, "Cazic-Thule"); // killed before picking the task up
        t.on_assigned(100, "Renouncing Your Faith");
        t.on_kill(200, "Innoruuk, the Prince of Hate");
        t.on_updated(201, "Renouncing Your Faith");

        let task = &t.report().tasks[0];
        assert_eq!(task.step_states(), vec![false, true]);
    }

    /// Definitions seeded after the fact must re-identify past completions.
    #[test]
    fn defining_later_backfills_step_identity() {
        let mut t = TaskTracker::new();
        t.on_assigned(100, "Renouncing Your Faith");
        t.on_kill(200, "Innoruuk, the Prince of Hate");
        t.on_updated(201, "Renouncing Your Faith");
        assert_eq!(t.report().tasks[0].completions[0].step_index, None);

        t.define(renouncing());
        let task = &t.report().tasks[0];
        assert_eq!(task.completions[0].step_index, Some(1));
        assert_eq!(task.step_states(), vec![false, true]);
    }

    #[test]
    fn a_stale_kill_is_not_credited_to_a_step() {
        let mut t = TaskTracker::new();
        t.define(renouncing());
        t.on_kill(100, "Cazic-Thule");
        // Far later — unrelated.
        t.on_updated(9999, "Renouncing Your Faith");
        assert_eq!(t.report().tasks[0].completions[0].killed, None);
    }

    #[test]
    fn quest_dialogue_is_captured_with_keywords() {
        let mut t = TaskTracker::new();
        t.on_dialogue(90, "You", "Hail, Emissary of Zebuxoruk");
        t.on_dialogue(
            95,
            "Emissary of Zebuxoruk",
            "If you wish to [follow this path], you must face the [gods themselves].",
        );
        t.on_assigned(100, "Renouncing Your Faith");

        let task = &t.report().tasks[0];
        assert_eq!(task.dialogue.len(), 2);
        assert_eq!(task.dialogue[0].speaker, "You");
        assert_eq!(
            task.dialogue[1].keywords,
            vec!["follow this path", "gods themselves"]
        );
    }

    /// The server writes the task credit one second before the death message.
    #[test]
    fn a_kill_logged_just_after_the_update_still_identifies_the_step() {
        let mut t = TaskTracker::new();
        t.define(TaskDef {
            name: "Renouncing Your Faith".into(),
            steps: vec![
                TaskStepDef {
                    description: "Kill Cazic-Thule".into(),
                    kill_target: Some("Cazic-Thule".into()),
                },
                TaskStepDef {
                    description: "Kill Innoruuk, the Prince of Hate".into(),
                    kill_target: Some("Innoruuk".into()),
                },
            ],
        });
        t.on_assigned(1000, "Renouncing Your Faith");
        t.on_updated(2000, "Renouncing Your Faith");
        // Death message lands a second later.
        t.on_kill(2001, "Innoruuk, the Prince of Hate");

        let rec = &t.report().tasks[0];
        assert_eq!(rec.completions[0].step_index, Some(1), "step 2, not step 1");
        assert_eq!(rec.step_states(), vec![false, true]);
        assert_eq!(rec.unidentified(), 0);
    }

    /// A death that is not one of the task's objectives must not be claimed.
    #[test]
    fn an_unrelated_later_kill_does_not_claim_a_step() {
        let mut t = TaskTracker::new();
        t.define(TaskDef {
            name: "Renouncing Your Faith".into(),
            steps: vec![TaskStepDef {
                description: "Kill Cazic-Thule".into(),
                kill_target: Some("Cazic-Thule".into()),
            }],
        });
        t.on_updated(2000, "Renouncing Your Faith");
        t.on_kill(2001, "a decaying skeleton");
        assert_eq!(t.report().tasks[0].completions[0].step_index, None);
    }

    #[test]
    fn dialogue_far_from_a_task_is_not_attached() {
        let mut t = TaskTracker::new();
        t.on_dialogue(0, "Someone", "idle chatter");
        t.on_assigned(100_000, "Renouncing Your Faith");
        assert!(t.report().tasks[0].dialogue.is_empty());
    }

    /// Weekly reassignment resets progress but must keep the guide material.
    #[test]
    fn reassignment_resets_progress_but_keeps_dialogue_and_steps() {
        let mut t = TaskTracker::new();
        t.define(TaskDef {
            name: "Potential of the Void - Lady Vox - Weekly".into(),
            steps: vec![TaskStepDef {
                description: "Kill Lady Vox".into(),
                kill_target: Some("Lady Vox".into()),
            }],
        });
        t.on_dialogue(90, "Someone", "Go [slay the dragon].");
        t.on_assigned(100, "Potential of the Void - Lady Vox - Weekly");
        t.on_kill(150, "Lady Vox");
        t.on_updated(151, "Potential of the Void - Lady Vox - Weekly");
        assert_eq!(t.report().tasks[0].steps_done(), 1);

        t.on_assigned(700_000, "Potential of the Void - Lady Vox - Weekly");
        let task = &t.report().tasks[0];
        assert_eq!(task.steps_done(), 0, "progress resets");
        assert_eq!(task.steps.len(), 1, "definition survives");
        assert_eq!(task.dialogue.len(), 1, "guide material survives");
    }

    #[test]
    fn keyword_extraction_handles_odd_text() {
        assert!(extract_keywords("no brackets here").is_empty());
        assert_eq!(extract_keywords("an [unclosed"), Vec::<String>::new());
        assert_eq!(extract_keywords("[a] and [b]"), vec!["a", "b"]);
    }

    #[test]
    fn achievements_are_recorded_newest_first() {
        let mut t = TaskTracker::new();
        t.on_achievement(100, "Level 49");
        t.on_achievement(200, "Level 50");
        assert_eq!(t.report().achievements[0].name, "Level 50");
    }
}
