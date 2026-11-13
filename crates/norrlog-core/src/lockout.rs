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

/// Hour of day both lockouts reset at, on the player's clock.
///
/// The log carries no reset message, so this cannot be derived from it — it is
/// read off the in-game timer. Two readings were taken and they disagreed by
/// an hour: 8h50m remaining at 01:10 gives 10:00, while 17m remaining at 10:45
/// gives 11:00. The second was trusted at first, being a seventeen-minute
/// estimate rather than a nine-hour one.
///
/// That was wrong, and play settled it. On Tuesday 18 August the player
/// cleared Innoruuk at difficulty 0, 1, 2 and 3 in one run, at 10:19, 10:29,
/// 10:45 and 11:05. An 11:00 reset splits that sweep in three, filing the
/// first three kills under the expiring week — which is not what clearing four
/// difficulties back to back means. A 10:00 reset puts the whole run in the
/// week it obviously belongs to, and the player confirmed the timer reads
/// 10:00.
///
/// The lesson is that a short countdown is not automatically the better
/// reading: both were estimates of the same instant, and only behaviour over a
/// reset boundary could tell them apart.
///
/// Local rather than UTC because the server keeps US time and shifts with US
/// daylight saving, as does this machine; a fixed UTC hour would be an hour out
/// for half the year. A player in another DST regime would need this adjusted.
///
/// Midnight was the original assumption, which cleared the grid eleven hours
/// early — bosses read as available while their loot was still locked.
pub const RESET_HOUR: i64 = 10;

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
    // Kedge Keep's named. "Phinigel Autropos pet" is a different creature and
    // is left out by the exact-name match, as it must be — killing the pet is
    // not killing the boss.
    ("Phinigel Autropos", &["phinigel autropos", "phinigel"]),
    // The log writes the full title, "Innoruuk, the Prince of Hate".
    (
        "Innoruuk",
        &[
            "innoruuk",
            "innoruuk, the prince of hate",
            "the prince of hate",
        ],
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
    /// Progress against the weekly cap on the void reward currency.
    pub void_currency: VoidCurrency,
}

/// The reward currency the void bosses hand out, and how many a week you can
/// earn.
///
/// The item line itself says nothing about where it came from — the client
/// writes "You have been given: Void-Touched Potential" with no source and no
/// quantity. The task does. Zoning into a raid zone assigns
/// `Potential of the Void - <boss> - Weekly`, and killing the boss updates it
/// at the same second the item arrives, so the pair together says both that
/// one was earned and what earned it.
///
/// The cap is not stated anywhere. It is read off play: four consecutive weeks
/// produced exactly three each, and further boss kills in those weeks produced
/// none.
pub const VOID_CURRENCY: &str = "Void-Touched Potential";

/// How many [`VOID_CURRENCY`] can be earned in one weekly period.
pub const VOID_CURRENCY_CAP: u32 = 3;

/// The task family that pays out [`VOID_CURRENCY`], as the client names it.
const VOID_TASK_PREFIX: &str = "Potential of the Void - ";
const VOID_TASK_SUFFIX: &str = " - Weekly";

/// Which boss a void task is for, if it is one.
///
/// The task writes the boss the way the quest text does — "Cazic Thule"
/// without the hyphen, "Innoruuk" without the title — which is exactly what
/// the alias table already resolves.
pub fn void_task_boss(task: &str) -> Option<&'static str> {
    let t = task.trim();
    let inner = t
        .strip_prefix(VOID_TASK_PREFIX)?
        .strip_suffix(VOID_TASK_SUFFIX)?;
    canonical_boss(inner)
}

/// One void reward, and the boss whose task paid it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoidGrant {
    pub at: i64,
    /// The boss whose task this was. `None` when a reward arrived with no void
    /// task completing alongside it — the item is still evidence it was
    /// earned, so it is counted, but nothing says what paid it.
    pub boss: Option<String>,
}

/// A void task taken but not yet paid out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoidTask {
    pub boss: String,
    /// When you zoned in and picked it up.
    pub assigned: i64,
}

/// Progress against the weekly cap on [`VOID_CURRENCY`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoidCurrency {
    /// Earned since the current weekly reset.
    pub earned_this_week: u32,
    pub cap: u32,
    /// Still available before the cap, this week.
    pub remaining: u32,
    /// What arrived this week, oldest first.
    pub this_week: Vec<VoidGrant>,
    /// Tasks taken this week that have not paid out — you zoned in and the
    /// boss is still standing.
    pub open_this_week: Vec<VoidTask>,
    /// Every grant ever seen. The cap is weekly, so the history is what shows
    /// a week was missed.
    pub all_time: usize,
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
    /// When each [`VOID_CURRENCY`] grant arrived.
    void_grants: Vec<i64>,
    /// Void tasks taken, as (when, boss).
    void_taken: Vec<(i64, String)>,
    /// Void tasks completed, as (when, boss). The completion lands on the same
    /// second as the reward, which is what ties a grant to a boss.
    void_done: Vec<(i64, String)>,
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
                clears.sort_by(|a, b| b.difficulty.cmp(&a.difficulty).then(a.boss.cmp(&b.boss)));
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
            void_currency: self.void_currency(ws),
        }
    }

    /// Note a [`VOID_CURRENCY`] grant.
    ///
    /// Only that one item. Everything is handed over with the same line, and a
    /// tally that counted quest rewards and tradeskill returns alongside it
    /// would answer no question at all.
    pub fn on_item_granted(&mut self, timestamp: i64, item: &str) {
        if !item.trim().eq_ignore_ascii_case(VOID_CURRENCY) {
            return;
        }
        self.void_grants.push(timestamp);
        self.void_grants.sort_unstable();
    }

    /// Note a void task being taken — which is what zoning into its raid
    /// zone does.
    pub fn on_task_assigned(&mut self, timestamp: i64, task: &str) {
        if let Some(boss) = void_task_boss(task) {
            self.void_taken.push((timestamp, boss.to_string()));
        }
    }

    /// Note a void task advancing.
    ///
    /// These tasks have one step, and it lands on the same second as the
    /// reward, so an update is a completion. Recorded separately from the
    /// reward rather than counted as one: the item is what the cap is on, and
    /// inferring one from the other would double-count the moment a task ever
    /// gains a second step.
    pub fn on_task_updated(&mut self, timestamp: i64, task: &str) {
        if let Some(boss) = void_task_boss(task) {
            self.void_done.push((timestamp, boss.to_string()));
        }
    }

    /// The boss whose void task completed at `at`, if one did.
    ///
    /// Matched on the second, because the completion and the reward are
    /// written together. A window would be worse than useless here: two of
    /// these can be finished minutes apart, and a loose match would credit the
    /// wrong boss rather than admit it did not know.
    fn boss_paying_at(&self, at: i64) -> Option<String> {
        self.void_done
            .iter()
            .find(|(t, _)| *t == at)
            .map(|(_, b)| b.clone())
    }

    fn void_currency(&self, week_start: i64) -> VoidCurrency {
        let week_end = week_start + 7 * DAY;
        let in_week = |t: i64| t >= week_start && t < week_end;

        let this_week: Vec<VoidGrant> = self
            .void_grants
            .iter()
            .copied()
            .filter(|t| in_week(*t))
            .map(|at| VoidGrant {
                boss: self.boss_paying_at(at),
                at,
            })
            .collect();
        let earned = this_week.len() as u32;

        // A task taken whose boss has not paid this week. Keyed on the boss
        // rather than the task instance because re-entering the zone assigns
        // it again, and three visits to a boss you have not killed is one
        // piece of unfinished business, not three.
        let paid: std::collections::BTreeSet<&str> = self
            .void_done
            .iter()
            .filter(|(t, _)| in_week(*t))
            .map(|(_, b)| b.as_str())
            .collect();
        let mut open: Vec<VoidTask> = Vec::new();
        for (at, boss) in self.void_taken.iter().filter(|(t, _)| in_week(*t)) {
            if paid.contains(boss.as_str()) || open.iter().any(|o| o.boss == *boss) {
                continue;
            }
            open.push(VoidTask {
                boss: boss.clone(),
                assigned: *at,
            });
        }

        VoidCurrency {
            earned_this_week: earned,
            cap: VOID_CURRENCY_CAP,
            // Saturating, because the cap is read off play rather than stated:
            // if a week ever produces a fourth, the right answer is "none
            // left", not a negative.
            remaining: VOID_CURRENCY_CAP.saturating_sub(earned),
            this_week,
            open_this_week: open,
            all_time: self.void_grants.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-07-28T00:00:00Z — a Tuesday, the day the game launched.
    const TUE: i64 = 1_785_196_800;

    /// The reset instant itself: Tuesday at [`RESET_HOUR`], which is where a
    /// lockout period actually begins. Tuesday *midnight* still belongs to the
    /// week before.
    const TUE_RESET: i64 = TUE + RESET_HOUR * 3600;

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
        // Midweek Friday resolves back to Tuesday's reset.
        let fri = TUE + 3 * DAY + 15 * 3600;
        assert_eq!(week_start(fri), TUE_RESET);
        // The reset instant is its own week start.
        assert_eq!(week_start(TUE_RESET), TUE_RESET);
        assert_eq!(week_start(TUE_RESET + 3600), TUE_RESET);
        // The following Monday still belongs to that week.
        assert_eq!(week_start(TUE + 6 * DAY), TUE_RESET);
        // The next Tuesday's reset starts a new one.
        assert_eq!(week_start(TUE_RESET + 7 * DAY), TUE_RESET + 7 * DAY);
    }

    /// The case that fixed the reset hour: a player clearing four difficulties
    /// back to back on reset day must land in one week, not two.
    ///
    /// Innoruuk at d0/d1/d2/d3 on Tuesday 18 August, at 10:19, 10:29, 10:45
    /// and 11:05. With the reset modelled an hour late the first three fell
    /// into the expiring week and the board showed them as never killed, which
    /// is what a player noticed and reported.
    #[test]
    fn a_sweep_across_reset_day_belongs_to_one_week() {
        let sweep = [
            TUE + 10 * 3600 + 19 * 60,
            TUE + 10 * 3600 + 29 * 60,
            TUE + 10 * 3600 + 45 * 60,
            TUE + 11 * 3600 + 5 * 60,
        ];
        let weeks: Vec<i64> = sweep.iter().map(|t| week_start(*t)).collect();
        assert!(
            weeks.iter().all(|w| *w == TUE_RESET),
            "the whole sweep is this week's: {weeks:?}"
        );
        // And it is genuinely the new week, not the one that just ended.
        assert_ne!(weeks[0], TUE_RESET - 7 * DAY);
    }

    /// The hours before the reset on reset day are the *end* of the old week,
    /// not the start of the new one. Getting this backwards cleared the grid
    /// ten hours early, showing bosses as lootable while they were still
    /// locked.
    #[test]
    fn tuesday_before_the_reset_is_still_last_week() {
        assert_eq!(week_start(TUE + 3600), TUE_RESET - 7 * DAY);
        assert_eq!(week_start(TUE_RESET - 1), TUE_RESET - 7 * DAY);
    }

    #[test]
    fn day_start_is_the_preceding_reset_hour() {
        let mid_afternoon = TUE + 15 * 3600;
        assert_eq!(day_start(mid_afternoon), TUE_RESET);
        assert_eq!(day_start(TUE_RESET + DAY + 60), TUE_RESET + DAY);
        // Early morning belongs to the day before, which is the whole point of
        // a reset hour that is not midnight.
        assert_eq!(day_start(TUE + 3600), TUE_RESET - DAY);
    }

    #[test]
    fn a_kill_marks_both_lockouts() {
        let mut t = LockoutTracker::new();
        let killed_at = TUE + 3 * DAY + 12 * 3600; // Friday lunchtime
        assert!(t.on_kill(
            killed_at,
            "Lord Nagafen",
            Some("Nagafen's Lair - Solo 3 (Fused)")
        ));

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
        t.on_kill(
            killed_at,
            "Master Yael",
            Some("The Ruins of Old Paineel - Solo"),
        );

        // Next day: daily has rolled over, weekly has not.
        let r = t.report(TUE + DAY + 12 * 3600);
        let cell = r
            .cells
            .iter()
            .find(|c| c.boss == "Master Yael" && c.difficulty == 0)
            .unwrap();
        assert!(cell.weekly_done, "weekly holds until the next Tuesday");
        assert!(!cell.daily_done, "daily should have reset overnight");

        // The following Tuesday morning, still before the reset hour, the
        // weekly is very much not clear yet.
        let early = t.report(TUE_RESET + 7 * DAY - 3600);
        assert!(
            early
                .cells
                .iter()
                .find(|c| c.boss == "Master Yael" && c.difficulty == 0)
                .unwrap()
                .weekly_done,
            "an hour before the reset the lockout still holds"
        );

        // After the reset hour: both clear.
        let r2 = t.report(TUE_RESET + 7 * DAY + 3600);
        let cell2 = r2
            .cells
            .iter()
            .find(|c| c.boss == "Master Yael" && c.difficulty == 0)
            .unwrap();
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
        t.on_kill(
            TUE,
            "Lord Nagafen",
            Some("Nagafen's Lair - Solo 4 (Refined)"),
        );
        t.on_kill(
            TUE + 100,
            "Master Yael",
            Some("The Ruins of Old Paineel - Solo"),
        );
        // An older build, earlier.
        t.on_kill(
            TUE - 500_000,
            "Cazic-Thule",
            Some("The Plane of Fear - Solo"),
        );

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
        assert_eq!(
            r.total_cells,
            TRACKED_BOSSES.len() * (MAX_DIFFICULTY as usize + 1)
        );
        assert_eq!(r.weekly_cleared, 0);
        assert_eq!(r.bosses.len(), TRACKED_BOSSES.len());
        assert!(
            r.bosses.iter().any(|b| b == "Phinigel Autropos"),
            "Kedge Keep's named is tracked too: {:?}",
            r.bosses
        );
    }

    /// The void currency is capped weekly, and the log states neither the cap
    /// nor what earned a grant — only that one arrived.
    #[test]
    fn void_currency_counts_against_the_weekly_cap() {
        let mut t = LockoutTracker::new();
        // Three in the week, which is the cap.
        for at in [TUE_RESET + 3600, TUE_RESET + 2 * DAY, TUE_RESET + 3 * DAY] {
            t.on_item_granted(at, VOID_CURRENCY);
        }
        // Last week's does not count toward this one.
        t.on_item_granted(TUE_RESET - DAY, VOID_CURRENCY);
        // Nor does anything else handed to you.
        t.on_item_granted(TUE_RESET + 4 * DAY, "Mote of Major Potential");

        let v = t.report(TUE_RESET + 4 * DAY).void_currency;
        assert_eq!(v.earned_this_week, 3);
        assert_eq!(v.cap, VOID_CURRENCY_CAP);
        assert_eq!(v.remaining, 0, "capped out");
        assert_eq!(v.this_week.len(), 3);
        assert_eq!(v.all_time, 4, "last week's is still on the record");

        // The following week starts clean.
        let next = t.report(TUE_RESET + 7 * DAY + 3600).void_currency;
        assert_eq!(next.earned_this_week, 0);
        assert_eq!(next.remaining, VOID_CURRENCY_CAP);
    }

    /// The reward line names no source; the task does. Zoning in assigns
    /// `Potential of the Void - <boss> - Weekly`, and it completes on the same
    /// second the item arrives.
    #[test]
    fn a_void_reward_is_attributed_to_the_task_that_paid_it() {
        let mut t = LockoutTracker::new();
        let at = TUE_RESET + 3600;
        t.on_task_assigned(at - 600, "Potential of the Void - Master Yael - Weekly");
        t.on_task_updated(at, "Potential of the Void - Master Yael - Weekly");
        t.on_item_granted(at, VOID_CURRENCY);

        // A second zone-in with the boss still standing.
        t.on_task_assigned(at + 1000, "Potential of the Void - Lady Vox - Weekly");

        let v = t.report(at + 2000).void_currency;
        assert_eq!(v.earned_this_week, 1);
        assert_eq!(v.remaining, 2);
        assert_eq!(v.this_week[0].boss.as_deref(), Some("Master Yael"));
        assert_eq!(
            v.open_this_week
                .iter()
                .map(|o| o.boss.as_str())
                .collect::<Vec<_>>(),
            vec!["Lady Vox"],
            "taken, not yet paid"
        );

        // Re-entering the zone re-assigns the same task; that is still one
        // piece of unfinished business.
        t.on_task_assigned(at + 1500, "Potential of the Void - Lady Vox - Weekly");
        assert_eq!(t.report(at + 2000).void_currency.open_this_week.len(), 1);

        // Once it pays, it is no longer open.
        t.on_task_updated(at + 3000, "Potential of the Void - Lady Vox - Weekly");
        t.on_item_granted(at + 3000, VOID_CURRENCY);
        let v = t.report(at + 4000).void_currency;
        assert!(v.open_this_week.is_empty());
        assert_eq!(v.earned_this_week, 2);
        assert_eq!(v.this_week[1].boss.as_deref(), Some("Lady Vox"));
    }

    /// The task writes bosses the way the quest text does, not the way the
    /// kill line does.
    #[test]
    fn void_task_names_resolve_to_tracked_bosses() {
        let b = |s: &str| void_task_boss(s);
        assert_eq!(
            b("Potential of the Void - Cazic Thule - Weekly"),
            Some("Cazic-Thule"),
            "the task drops the hyphen"
        );
        assert_eq!(
            b("Potential of the Void - Innoruuk - Weekly"),
            Some("Innoruuk"),
            "and the title"
        );
        assert_eq!(
            b("Potential of the Void - Phinigel Autropos - Weekly"),
            Some("Phinigel Autropos")
        );
        // Anything else assigned that week is not a void task.
        assert_eq!(b("Slay the Gnolls - Weekly"), None);
        assert_eq!(b("Potential of the Void - Somebody Else - Weekly"), None);
    }

    /// A reward with no task completing beside it is still a reward.
    #[test]
    fn an_unattributed_reward_still_counts_against_the_cap() {
        let mut t = LockoutTracker::new();
        t.on_item_granted(TUE_RESET + 60, VOID_CURRENCY);
        let v = t.report(TUE_RESET + 120).void_currency;
        assert_eq!(v.earned_this_week, 1, "the cap is on the item");
        assert_eq!(v.this_week[0].boss, None, "but nothing says what paid it");
    }

    /// Killing the boss's pet is not killing the boss.
    #[test]
    fn phinigels_pet_is_not_phinigel() {
        assert_eq!(
            canonical_boss("Phinigel Autropos"),
            Some("Phinigel Autropos")
        );
        assert_eq!(canonical_boss("phinigel"), Some("Phinigel Autropos"));
        assert_eq!(canonical_boss("Phinigel Autropos pet"), None);
    }
}
