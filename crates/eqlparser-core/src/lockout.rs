//! Boss lockout tracking.
//!
//! EverQuest Legends gates its named encounters behind two independent
//! lockouts: a **weekly** one granting full loot, and a **daily** one granting
//! a single item. Each boss exists at five difficulties, so what a player
//! actually wants to know is a grid — which of boss × difficulty still has
//! loot available, on each timer.
//!
//! Difficulty is not stated directly; it is encoded in the zone name, which the
//! client writes on entry:
//!
//! ```text
//! Nagafen's Lair - Solo              -> difficulty 0
//! Nagafen's Lair - Solo 1 (Awakened) -> difficulty 1
//! The Plane of Hate 2 (Adaptive)     -> difficulty 2
//! Nagafen's Lair - Solo 3 (Fused)    -> difficulty 3
//! Nagafen's Lair - Solo 4 (Refined)  -> difficulty 4
//! ```

use serde::{Deserialize, Serialize};

/// Difficulty tiers, 0 (base) through 4.
pub const MAX_DIFFICULTY: u8 = 4;

/// Weekday the weekly lockout resets on. The game launched on a Tuesday and
/// resets weekly on Tuesdays.
pub const WEEKLY_RESET_WEEKDAY: u8 = 2; // 1 = Monday ... 7 = Sunday

/// Hour of day both lockouts reset at.
///
/// **Assumed midnight.** The log gives no reset message to derive this from, so
/// if the server actually rolls over at some other hour this is the single
/// value to change.
pub const RESET_HOUR: i64 = 0;

/// Seconds in a day.
const DAY: i64 = 86_400;

/// The named encounters worth tracking, with the spellings the log uses.
///
/// Matching is on the canonical name, so "Cazic-Thule" and "Cazic Thule" both
/// resolve to one row.
pub const TRACKED_BOSSES: &[(&str, &[&str])] = &[
    ("Master Yael", &["master yael", "yael"]),
    ("Cazic-Thule", &["cazic-thule", "cazic thule"]),
    ("Lord Nagafen", &["lord nagafen", "nagafen"]),
    ("Lady Vox", &["lady vox", "vox"]),
    // The log writes the full title, "Innoruuk, the Prince of Hate".
    (
        "Innoruuk",
        &["innoruuk", "innoruuk, the prince of hate", "the prince of hate"],
    ),
];

/// Resolve a slain creature to a tracked boss, if it is one.
pub fn canonical_boss(name: &str) -> Option<&'static str> {
    let n = name.trim().to_lowercase();
    TRACKED_BOSSES
        .iter()
        .find(|(_, aliases)| aliases.iter().any(|a| *a == n))
        .map(|(canonical, _)| *canonical)
}

/// Extract the difficulty tier from a zone name.
///
/// The tier is the number immediately before the parenthesised name. A zone
/// with no such marker is difficulty 0.
pub fn difficulty_of(zone: &str) -> u8 {
    // e.g. "Nagafen's Lair - Solo 3 (Fused)"
    let Some(open) = zone.rfind(" (") else {
        return 0;
    };
    zone[..open]
        .rsplit(' ')
        .next()
        .and_then(|t| t.parse::<u8>().ok())
        .filter(|d| *d <= MAX_DIFFICULTY)
        .unwrap_or(0)
}

/// Whether a zone is a solo instance rather than a group one.
pub fn is_solo_zone(zone: &str) -> bool {
    zone.contains("- Solo")
}

/// One recorded boss kill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BossKill {
    pub boss: String,
    pub difficulty: u8,
    pub timestamp: i64,
    pub zone: String,
    pub solo: bool,
}

/// Everything one build has killed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildRecord {
    pub build: String,
    /// Distinct boss + difficulty pairs cleared, highest difficulty first.
    pub clears: Vec<BuildClear>,
    pub total_kills: u32,
    pub first_kill: i64,
    pub last_kill: i64,
}

/// One boss at one difficulty, as cleared by a build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildClear {
    pub boss: String,
    pub difficulty: u8,
    pub kills: u32,
    pub first: i64,
    pub last: i64,
}

/// Lockout status of one boss at one difficulty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockoutCell {
    pub boss: String,
    pub difficulty: u8,
    /// Killed since the weekly reset — full loot already taken.
    pub weekly_done: bool,
    /// Killed since the daily reset — the single daily item already taken.
    pub daily_done: bool,
    /// Most recent kill at this difficulty, ever.
    pub last_kill: Option<i64>,
    /// Kills at this difficulty since the weekly reset.
    pub kills_this_week: u32,
}

/// The full grid, plus the period boundaries it was computed against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockoutReport {
    pub week_start: i64,
    pub week_end: i64,
    pub day_start: i64,
    pub day_end: i64,
    pub bosses: Vec<String>,
    pub max_difficulty: u8,
    /// Row-major: one entry per boss per difficulty.
    pub cells: Vec<LockoutCell>,
    /// Weekly cells cleared, out of the total.
    pub weekly_cleared: usize,
    pub daily_cleared: usize,
    pub total_cells: usize,
    /// Raw boss kills in the period. Distinct from `*_cleared`, which counts
    /// grid cells: killing the same boss five times clears one cell but is
    /// five kills, and both numbers are worth seeing.
    pub kills_this_week: usize,
    pub kills_today: usize,
    /// The class build currently doing the killing, e.g. "WAR/DRU/BRD".
    pub current_build: String,
}

/// Days since the Unix epoch for a timestamp.
fn days_since_epoch(ts: i64) -> i64 {
    ts.div_euclid(DAY)
}

/// ISO weekday, 1 = Monday through 7 = Sunday.
///
/// 1970-01-01 was a Thursday, so day 0 is weekday 4.
fn weekday(ts: i64) -> u8 {
    (days_since_epoch(ts) + 3).rem_euclid(7) as u8 + 1
}

/// Start of the daily lockout period containing `now`.
pub fn day_start(now: i64) -> i64 {
    let midnight = days_since_epoch(now) * DAY + RESET_HOUR * 3600;
    if midnight > now {
        midnight - DAY
    } else {
        midnight
    }
}

/// Start of the weekly lockout period containing `now`.
pub fn week_start(now: i64) -> i64 {
    let today = day_start(now);
    let wd = weekday(today) as i64;
    // Days back to the most recent reset weekday.
    let back = (wd - WEEKLY_RESET_WEEKDAY as i64).rem_euclid(7);
    today - back * DAY
}

/// Accumulates boss kills and answers lockout questions.
#[derive(Debug, Default)]
pub struct LockoutTracker {
    kills: Vec<BossKill>,
}

impl LockoutTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a kill, if the victim is a tracked boss.
    ///
    /// Returns true when it was recorded.
    pub fn on_kill(&mut self, timestamp: i64, victim: &str, zone: Option<&str>) -> bool {
        let Some(boss) = canonical_boss(victim) else {
            return false;
        };
        let zone = zone.unwrap_or("");
        self.kills.push(BossKill {
            boss: boss.to_string(),
            difficulty: difficulty_of(zone),
            timestamp,
            zone: zone.to_string(),
            solo: is_solo_zone(zone),
        });
        true
    }

    /// What each build has cleared, most recently active build first.
    ///
    /// The build label is supplied by the caller per kill timestamp rather
    /// than stored on the kill, so it can be derived from the *whole* log
    /// rather than only from evidence that happened to precede the kill.
    /// Kills the labeller cannot place are grouped under "unknown" rather
    /// than being attributed to whatever the character happens to be now.
    pub fn builds(&self, label: &dyn Fn(i64) -> String) -> Vec<BuildRecord> {
        use std::collections::HashMap;
        let mut by_build: HashMap<String, HashMap<(String, u8), BuildClear>> = HashMap::new();
        for k in &self.kills {
            let build = match label(k.timestamp) {
                s if s.is_empty() => "unknown".to_string(),
                s => s,
            };
            let e = by_build
                .entry(build)
                .or_default()
                .entry((k.boss.clone(), k.difficulty))
                .or_insert(BuildClear {
                    boss: k.boss.clone(),
                    difficulty: k.difficulty,
                    kills: 0,
                    first: k.timestamp,
                    last: k.timestamp,
                });
            e.kills += 1;
            e.first = e.first.min(k.timestamp);
            e.last = e.last.max(k.timestamp);
        }

        let mut out: Vec<BuildRecord> = by_build
            .into_iter()
            .map(|(build, clears)| {
                let mut clears: Vec<BuildClear> = clears.into_values().collect();
                clears.sort_by(|a, b| {
                    b.difficulty
                        .cmp(&a.difficulty)
                        .then(a.boss.cmp(&b.boss))
                });
                BuildRecord {
                    total_kills: clears.iter().map(|c| c.kills).sum(),
                    first_kill: clears.iter().map(|c| c.first).min().unwrap_or(0),
                    last_kill: clears.iter().map(|c| c.last).max().unwrap_or(0),
                    build,
                    clears,
                }
            })
            .collect();
        out.sort_by_key(|b| std::cmp::Reverse(b.last_kill));
        out
    }

    /// Kills within a period, for the daily and weekly counters.
    pub fn kills_between(&self, from: i64, to: i64) -> usize {
        self.kills
            .iter()
            .filter(|k| k.timestamp >= from && k.timestamp <= to)
            .count()
    }

    pub fn kills(&self) -> &[BossKill] {
        &self.kills
    }

    /// Build the lockout grid as of `now`.
    pub fn report(&self, now: i64) -> LockoutReport {
        let ws = week_start(now);
        let ds = day_start(now);
        let bosses: Vec<String> = TRACKED_BOSSES.iter().map(|(n, _)| n.to_string()).collect();

        let mut cells = Vec::new();
        for boss in &bosses {
            for difficulty in 0..=MAX_DIFFICULTY {
                let mine = self
                    .kills
                    .iter()
                    .filter(|k| &k.boss == boss && k.difficulty == difficulty);
                let mut weekly_done = false;
                let mut daily_done = false;
                let mut last_kill = None;
                let mut kills_this_week = 0;
                for k in mine {
                    if k.timestamp >= ws {
                        weekly_done = true;
                        kills_this_week += 1;
                    }
                    if k.timestamp >= ds {
                        daily_done = true;
                    }
                    last_kill = Some(last_kill.map_or(k.timestamp, |p: i64| p.max(k.timestamp)));
                }
                cells.push(LockoutCell {
                    boss: boss.clone(),
                    difficulty,
                    weekly_done,
                    daily_done,
                    last_kill,
                    kills_this_week,
                });
            }
        }

        let weekly_cleared = cells.iter().filter(|c| c.weekly_done).count();
        let daily_cleared = cells.iter().filter(|c| c.daily_done).count();
        let total_cells = cells.len();

        LockoutReport {
            week_start: ws,
            week_end: ws + 7 * DAY,
            day_start: ds,
            day_end: ds + DAY,
            bosses,
            max_difficulty: MAX_DIFFICULTY,
            cells,
            weekly_cleared,
            daily_cleared,
            total_cells,
            kills_this_week: self.kills_between(ws, ws + 7 * DAY),
            kills_today: self.kills_between(ds, ds + DAY),
            // Filled in by the caller, which owns the class detector.
            current_build: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-07-28T00:00:00Z — a Tuesday, the day the game launched.
    const TUE: i64 = 1_785_196_800;

    #[test]
    fn difficulty_parses_from_the_zone_name() {
        assert_eq!(difficulty_of("Nagafen's Lair - Solo"), 0);
        assert_eq!(difficulty_of("Nagafen's Lair - Solo 1 (Awakened)"), 1);
        assert_eq!(difficulty_of("The Plane of Hate 2 (Adaptive)"), 2);
        assert_eq!(difficulty_of("Nagafen's Lair - Solo 3 (Fused)"), 3);
        assert_eq!(difficulty_of("Nagafen's Lair - Solo 4 (Refined)"), 4);
        // A plain zone, and a parenthesised name with no tier.
        assert_eq!(difficulty_of("The Plane of Fear - Solo"), 0);
        assert_eq!(difficulty_of("Somewhere (Odd)"), 0);
    }

    #[test]
    fn solo_zones_are_recognised() {
        assert!(is_solo_zone("Nagafen's Lair - Solo 3 (Fused)"));
        assert!(!is_solo_zone("The Plane of Hate 2 (Adaptive)"));
    }

    #[test]
    fn boss_aliases_resolve_to_one_row() {
        assert_eq!(canonical_boss("Master Yael"), Some("Master Yael"));
        assert_eq!(canonical_boss("Cazic-Thule"), Some("Cazic-Thule"));
        assert_eq!(canonical_boss("cazic thule"), Some("Cazic-Thule"));
        assert_eq!(canonical_boss("Lord Nagafen"), Some("Lord Nagafen"));
        assert_eq!(canonical_boss("a fire giant warrior"), None);
        // The log uses the full title for Innoruuk.
        assert_eq!(
            canonical_boss("Innoruuk, the Prince of Hate"),
            Some("Innoruuk")
        );
        // Trash that merely mentions the god must not match.
        assert_eq!(canonical_boss("Cleric of Innoruuk"), None);
        assert_eq!(canonical_boss("Innoruuk`s Chosen"), None);
    }

    #[test]
    fn weekday_matches_the_calendar() {
        // 2026-07-28 is a Tuesday.
        assert_eq!(weekday(TUE), 2);
        assert_eq!(weekday(TUE + DAY), 3); // Wednesday
        assert_eq!(weekday(TUE - DAY), 1); // Monday
        assert_eq!(weekday(TUE + 5 * DAY), 7); // Sunday
    }

    #[test]
    fn week_starts_on_the_reset_tuesday() {
        // Midweek Friday resolves back to Tuesday.
        let fri = TUE + 3 * DAY + 15 * 3600;
        assert_eq!(week_start(fri), TUE);
        // Tuesday itself is its own week start.
        assert_eq!(week_start(TUE + 3600), TUE);
        // The following Monday still belongs to that week.
        assert_eq!(week_start(TUE + 6 * DAY), TUE);
        // The next Tuesday starts a new one.
        assert_eq!(week_start(TUE + 7 * DAY), TUE + 7 * DAY);
    }

    #[test]
    fn day_start_is_the_preceding_reset_hour() {
        let mid_afternoon = TUE + 15 * 3600;
        assert_eq!(day_start(mid_afternoon), TUE);
        assert_eq!(day_start(TUE + DAY + 60), TUE + DAY);
    }

    #[test]
    fn a_kill_marks_both_lockouts() {
        let mut t = LockoutTracker::new();
        let killed_at = TUE + 3 * DAY + 12 * 3600; // Friday lunchtime
        assert!(t.on_kill(killed_at, "Lord Nagafen", Some("Nagafen's Lair - Solo 3 (Fused)")));

        let r = t.report(killed_at + 3600);
        let cell = r
            .cells
            .iter()
            .find(|c| c.boss == "Lord Nagafen" && c.difficulty == 3)
            .unwrap();
        assert!(cell.weekly_done);
        assert!(cell.daily_done);
        assert_eq!(cell.kills_this_week, 1);

        // A different difficulty of the same boss is untouched.
        let other = r
            .cells
            .iter()
            .find(|c| c.boss == "Lord Nagafen" && c.difficulty == 4)
            .unwrap();
        assert!(!other.weekly_done);
    }

    /// The daily clears overnight while the weekly holds until Tuesday.
    #[test]
    fn daily_resets_before_the_weekly_does() {
        let mut t = LockoutTracker::new();
        let killed_at = TUE + 12 * 3600; // Tuesday midday
        t.on_kill(killed_at, "Master Yael", Some("The Ruins of Old Paineel - Solo"));

        // Next day: daily has rolled over, weekly has not.
        let r = t.report(TUE + DAY + 12 * 3600);
        let cell = r.cells.iter().find(|c| c.boss == "Master Yael" && c.difficulty == 0).unwrap();
        assert!(cell.weekly_done, "weekly holds until the next Tuesday");
        assert!(!cell.daily_done, "daily should have reset overnight");

        // Following Tuesday: both clear.
        let r2 = t.report(TUE + 7 * DAY + 3600);
        let cell2 = r2.cells.iter().find(|c| c.boss == "Master Yael" && c.difficulty == 0).unwrap();
        assert!(!cell2.weekly_done);
        assert!(!cell2.daily_done);
    }

    #[test]
    fn untracked_creatures_are_ignored() {
        let mut t = LockoutTracker::new();
        assert!(!t.on_kill(TUE, "a fire giant warrior", Some("Nagafen's Lair")));
        assert!(t.kills().is_empty());
    }

    /// Builds change over a character's life; a kill must be remembered
    /// against the build that actually made it.
    #[test]
    fn kills_are_grouped_by_the_build_that_made_them() {
        let mut t = LockoutTracker::new();
        t.on_kill(TUE, "Lord Nagafen", Some("Nagafen's Lair - Solo 4 (Refined)"));
        t.on_kill(TUE + 100, "Master Yael", Some("The Ruins of Old Paineel - Solo"));
        // An older build, earlier.
        t.on_kill(TUE - 500_000, "Cazic-Thule", Some("The Plane of Fear - Solo"));

        let label = |ts: i64| {
            if ts < TUE - 1000 {
                "WAR/DRU/ROG".to_string()
            } else {
                "WAR/DRU/BRD".to_string()
            }
        };
        let builds = t.builds(&label);
        assert_eq!(builds.len(), 2);
        assert_eq!(builds[0].build, "WAR/DRU/BRD", "most recent build first");
        assert_eq!(builds[0].total_kills, 2);
        // Hardest clear leads.
        assert_eq!(builds[0].clears[0].boss, "Lord Nagafen");
        assert_eq!(builds[0].clears[0].difficulty, 4);
        assert_eq!(builds[1].build, "WAR/DRU/ROG");
    }

    #[test]
    fn kills_without_a_known_build_group_separately() {
        let mut t = LockoutTracker::new();
        t.on_kill(TUE, "Lady Vox", Some("The Permafrost Caverns - Solo"));
        assert_eq!(t.builds(&|_| String::new())[0].build, "unknown");
    }

    #[test]
    fn kills_between_counts_a_period() {
        let mut t = LockoutTracker::new();
        t.on_kill(TUE + 100, "Lady Vox", Some("x"));
        t.on_kill(TUE + DAY + 100, "Master Yael", Some("x"));
        assert_eq!(t.kills_between(TUE, TUE + DAY), 1);
        assert_eq!(t.kills_between(TUE, TUE + 2 * DAY), 2);
    }

    #[test]
    fn grid_covers_every_boss_and_difficulty() {
        let t = LockoutTracker::new();
        let r = t.report(TUE);
        assert_eq!(r.total_cells, TRACKED_BOSSES.len() * (MAX_DIFFICULTY as usize + 1));
        assert_eq!(r.weekly_cleared, 0);
        assert_eq!(r.bosses.len(), 5);
    }
}
