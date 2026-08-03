//! Durable history across log files.
//!
//! # Why this exists
//!
//! The engine holds everything in memory and only ever sees the log it is
//! following. That is fine for a live meter, and fatal for the thing this tool
//! is actually for: the client starts a new log file periodically, and when it
//! does, every boss kill, level record and build attribution from the previous
//! file is gone the next time the daemon starts. A build history that resets
//! every few days cannot back a guide.
//!
//! # Idempotence, not bookkeeping
//!
//! Re-parsing the same log must not double-count. Rather than track how far
//! into each file we have read — which breaks the moment a file is truncated
//! or rewritten — every row carries a natural key derived from the event
//! itself, and writes are `INSERT OR REPLACE`. Parsing the same log twice
//! produces the same rows; parsing a longer version of it adds the new ones.
//! There is no offset state to get wrong.
//!
//! The natural keys assume one character per database, which is how the daemon
//! runs: a log file names its character, and a different character is a
//! different history.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A boss kill worth remembering across sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredKill {
    pub timestamp: i64,
    pub boss: String,
    pub difficulty: u8,
    pub zone: String,
    pub solo: bool,
    pub build: String,
    /// Seconds the fight took, when it could be matched to an encounter.
    pub duration: i64,
    /// Your damage in that fight.
    pub your_damage: u64,
    pub your_dps: f64,
}

/// One completed fight, kept for per-raid averages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEncounter {
    pub start: i64,
    pub end: i64,
    pub title: String,
    pub zone: String,
    pub duration: i64,
    pub your_damage: u64,
    pub your_dps: f64,
    pub total_damage: u64,
    pub kills: u32,
    pub deaths: u32,
    pub build: String,
}

/// A level, with the build that earned it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredLevel {
    pub level: u32,
    pub started: i64,
    pub ended: i64,
    pub seconds: i64,
    pub kills: u32,
    pub percent: f64,
    pub build: String,
    /// Zone breakdown, as JSON, so the shape can change without a migration.
    pub zones: String,
}

/// Per-raid, per-build aggregate — the shape a build guide needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaidStat {
    pub boss: String,
    pub difficulty: u8,
    pub build: String,
    pub kills: u32,
    /// Seconds. Kills with no matched encounter are excluded from timing and
    /// damage figures rather than counted as zero, which would drag averages
    /// down and make a build look faster and weaker than it was.
    pub avg_seconds: f64,
    pub best_seconds: i64,
    pub avg_dps: f64,
    pub best_dps: f64,
    pub first_kill: i64,
    pub last_kill: i64,
}

/// One quest, as written up for a guide.
///
/// Deliberately loose. This is reference material a person authors, not
/// something derived from the log: half the fields are prose, the shape
/// changes as more is learned about a quest, and a rigid schema would fight
/// that. Stored as JSON against the name so the structure can grow without a
/// migration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QuestRecord {
    pub name: String,
    #[serde(default)]
    pub giver: String,
    #[serde(default)]
    pub zone: String,
    /// Classes it applies to. Empty means any.
    #[serde(default)]
    pub classes: Vec<String>,
    /// How to start it — the hail, the keyword, the prerequisite.
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub reward: String,
    #[serde(default)]
    pub notes: String,
    /// Quest text captured from the log, verbatim.
    #[serde(default)]
    pub dialogue: Vec<QuestLine>,
    /// "unknown", "in progress" or "done".
    #[serde(default)]
    pub status: String,
    /// Where the record came from: "manual", "task" or "sky".
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub updated: i64,
}

/// A line of captured quest text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestLine {
    pub at: i64,
    pub speaker: String,
    pub text: String,
}

/// The persistent history database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) the history database.
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        // The daemon writes in bursts while catching up on a log and then
        // trickles; WAL keeps those writes from blocking API reads.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let s = Store { conn };
        s.migrate()?;
        Ok(s)
    }

    /// An in-memory database, for tests.
    pub fn in_memory() -> Result<Store> {
        let s = Store {
            conn: Connection::open_in_memory()?,
        };
        s.migrate()?;
        Ok(s)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS boss_kills (
                timestamp   INTEGER NOT NULL,
                boss        TEXT    NOT NULL,
                difficulty  INTEGER NOT NULL,
                zone        TEXT    NOT NULL DEFAULT '',
                solo        INTEGER NOT NULL DEFAULT 0,
                build       TEXT    NOT NULL DEFAULT '',
                duration    INTEGER NOT NULL DEFAULT 0,
                your_damage INTEGER NOT NULL DEFAULT 0,
                your_dps    REAL    NOT NULL DEFAULT 0,
                PRIMARY KEY (timestamp, boss)
            );
            CREATE INDEX IF NOT EXISTS boss_kills_boss ON boss_kills(boss);
            CREATE INDEX IF NOT EXISTS boss_kills_build ON boss_kills(build);

            CREATE TABLE IF NOT EXISTS encounters (
                start        INTEGER NOT NULL,
                end          INTEGER NOT NULL,
                title        TEXT    NOT NULL,
                zone         TEXT    NOT NULL DEFAULT '',
                duration     INTEGER NOT NULL DEFAULT 0,
                your_damage  INTEGER NOT NULL DEFAULT 0,
                your_dps     REAL    NOT NULL DEFAULT 0,
                total_damage INTEGER NOT NULL DEFAULT 0,
                kills        INTEGER NOT NULL DEFAULT 0,
                deaths       INTEGER NOT NULL DEFAULT 0,
                build        TEXT    NOT NULL DEFAULT '',
                PRIMARY KEY (start, title)
            );
            CREATE INDEX IF NOT EXISTS encounters_title ON encounters(title);

            CREATE TABLE IF NOT EXISTS levels (
                level   INTEGER NOT NULL,
                started INTEGER NOT NULL,
                ended   INTEGER NOT NULL DEFAULT 0,
                seconds INTEGER NOT NULL DEFAULT 0,
                kills   INTEGER NOT NULL DEFAULT 0,
                percent REAL    NOT NULL DEFAULT 0,
                build   TEXT    NOT NULL DEFAULT '',
                zones   TEXT    NOT NULL DEFAULT '[]',
                PRIMARY KEY (level, started)
            );

            CREATE TABLE IF NOT EXISTS aa_purchases (
                ability TEXT    NOT NULL,
                rank    INTEGER NOT NULL,
                spent   INTEGER NOT NULL DEFAULT 0,
                at      INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (ability, rank)
            );

            -- Sky quest turn-ins. Keyed by quest and time so re-parsing a log
            -- is idempotent, and so the count survives the log rotating away.
            CREATE TABLE IF NOT EXISTS sky_turnins (
                quest TEXT    NOT NULL,
                at    INTEGER NOT NULL,
                PRIMARY KEY (quest, at)
            );

            -- Hand-authored quest write-ups. JSON rather than columns: the
            -- content is prose and its shape changes as a quest is
            -- understood, which a fixed schema would only get in the way of.
            CREATE TABLE IF NOT EXISTS quests (
                name    TEXT PRIMARY KEY,
                data    TEXT NOT NULL,
                updated INTEGER NOT NULL DEFAULT 0
            );

            -- Which log files have been seen, purely so the UI can say what
            -- the history is built from. Never used to decide what to parse:
            -- that is what the natural keys are for.
            CREATE TABLE IF NOT EXISTS sources (
                path      TEXT PRIMARY KEY,
                last_seen INTEGER NOT NULL,
                bytes     INTEGER NOT NULL DEFAULT 0
            );
            ",
        )?;
        Ok(())
    }

    /// Record boss kills. Existing rows are replaced, so a re-parse that now
    /// knows more (a build label, a matched encounter) improves them.
    pub fn put_kills(&mut self, kills: &[StoredKill]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO boss_kills
                 (timestamp, boss, difficulty, zone, solo, build, duration, your_damage, your_dps)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            )?;
            for k in kills {
                stmt.execute(params![
                    k.timestamp,
                    k.boss,
                    k.difficulty,
                    k.zone,
                    k.solo as i64,
                    k.build,
                    k.duration,
                    k.your_damage as i64,
                    k.your_dps,
                ])?;
            }
        }
        tx.commit()?;
        Ok(kills.len())
    }

    pub fn put_encounters(&mut self, encs: &[StoredEncounter]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO encounters
                 (start, end, title, zone, duration, your_damage, your_dps, total_damage, kills, deaths, build)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?;
            for e in encs {
                stmt.execute(params![
                    e.start,
                    e.end,
                    e.title,
                    e.zone,
                    e.duration,
                    e.your_damage as i64,
                    e.your_dps,
                    e.total_damage as i64,
                    e.kills,
                    e.deaths,
                    e.build,
                ])?;
            }
        }
        tx.commit()?;
        Ok(encs.len())
    }

    pub fn put_levels(&mut self, levels: &[StoredLevel]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO levels
                 (level, started, ended, seconds, kills, percent, build, zones)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )?;
            for l in levels {
                stmt.execute(params![
                    l.level, l.started, l.ended, l.seconds, l.kills, l.percent, l.build, l.zones
                ])?;
            }
        }
        tx.commit()?;
        Ok(levels.len())
    }

    pub fn put_aa(&mut self, purchases: &[(String, u32, u32, i64)]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO aa_purchases (ability, rank, spent, at)
                 VALUES (?1,?2,?3,?4)",
            )?;
            for (ability, rank, spent, at) in purchases {
                stmt.execute(params![ability, rank, spent, at])?;
            }
        }
        tx.commit()?;
        Ok(purchases.len())
    }

    /// Record Sky turn-ins. Existing (quest, time) pairs are left alone.
    pub fn put_sky_turnins(&mut self, rows: &[(String, i64)]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt =
                tx.prepare("INSERT OR IGNORE INTO sky_turnins (quest, at) VALUES (?1,?2)")?;
            for (quest, at) in rows {
                stmt.execute(params![quest, at])?;
            }
        }
        tx.commit()?;
        Ok(rows.len())
    }

    /// Turn-ins per quest, as (count, most recent), across every log ever
    /// parsed.
    pub fn sky_turnin_counts(&self) -> Result<Vec<(String, u32, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT quest, COUNT(*), MAX(at) FROM sky_turnins GROUP BY quest",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Write a quest, replacing any existing record of the same name.
    pub fn put_quest(&mut self, q: &QuestRecord) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO quests (name, data, updated) VALUES (?1,?2,?3)",
            params![q.name, serde_json::to_string(q)?, q.updated],
        )?;
        Ok(())
    }

    /// Add a quest only if it is not already there.
    ///
    /// Used by the importer, so re-importing never overwrites what someone has
    /// since written. Returns whether it was inserted.
    pub fn add_quest_if_new(&mut self, q: &QuestRecord) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO quests (name, data, updated) VALUES (?1,?2,?3)",
            params![q.name, serde_json::to_string(q)?, q.updated],
        )?;
        Ok(n > 0)
    }

    pub fn quests(&self) -> Result<Vec<QuestRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM quests ORDER BY name ASC")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .iter()
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect())
    }

    pub fn quest(&self, name: &str) -> Result<Option<QuestRecord>> {
        let mut stmt = self.conn.prepare("SELECT data FROM quests WHERE name = ?1")?;
        let mut rows = stmt.query_map([name], |r| r.get::<_, String>(0))?;
        match rows.next() {
            Some(Ok(d)) => Ok(serde_json::from_str(&d).ok()),
            _ => Ok(None),
        }
    }

    pub fn delete_quest(&mut self, name: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM quests WHERE name = ?1", params![name])?;
        Ok(n > 0)
    }

    pub fn note_source(&mut self, path: &str, bytes: u64, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sources (path, last_seen, bytes) VALUES (?1,?2,?3)",
            params![path, now, bytes as i64],
        )?;
        Ok(())
    }

    pub fn sources(&self) -> Result<Vec<(String, i64, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, last_seen, bytes FROM sources ORDER BY last_seen DESC")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every boss kill ever recorded, newest first.
    pub fn kills(&self) -> Result<Vec<StoredKill>> {
        let mut stmt = self.conn.prepare(
            "SELECT timestamp, boss, difficulty, zone, solo, build, duration, your_damage, your_dps
             FROM boss_kills ORDER BY timestamp DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(StoredKill {
                    timestamp: r.get(0)?,
                    boss: r.get(1)?,
                    difficulty: r.get(2)?,
                    zone: r.get(3)?,
                    solo: r.get::<_, i64>(4)? != 0,
                    build: r.get(5)?,
                    duration: r.get(6)?,
                    your_damage: r.get::<_, i64>(7)? as u64,
                    your_dps: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn levels(&self) -> Result<Vec<StoredLevel>> {
        let mut stmt = self.conn.prepare(
            "SELECT level, started, ended, seconds, kills, percent, build, zones
             FROM levels ORDER BY level ASC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(StoredLevel {
                    level: r.get(0)?,
                    started: r.get(1)?,
                    ended: r.get(2)?,
                    seconds: r.get(3)?,
                    kills: r.get(4)?,
                    percent: r.get(5)?,
                    build: r.get(6)?,
                    zones: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Encounters matching a boss name, for per-raid averages.
    pub fn encounters_named(&self, like: &str) -> Result<Vec<StoredEncounter>> {
        let mut stmt = self.conn.prepare(
            "SELECT start, end, title, zone, duration, your_damage, your_dps, total_damage, kills, deaths, build
             FROM encounters WHERE title LIKE ?1 ORDER BY start ASC",
        )?;
        let rows = stmt
            .query_map([format!("%{like}%")], |r| {
                Ok(StoredEncounter {
                    start: r.get(0)?,
                    end: r.get(1)?,
                    title: r.get(2)?,
                    zone: r.get(3)?,
                    duration: r.get(4)?,
                    your_damage: r.get::<_, i64>(5)? as u64,
                    your_dps: r.get(6)?,
                    total_damage: r.get::<_, i64>(7)? as u64,
                    kills: r.get(8)?,
                    deaths: r.get(9)?,
                    build: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Kills grouped by boss, difficulty and build.
    ///
    /// The unit a guide is written in: "as WAR/DRU/BRD, Nagafen d4, five
    /// kills, 8m12s average, 149 dps".
    pub fn raid_stats(&self) -> Result<Vec<RaidStat>> {
        let mut stmt = self.conn.prepare(
            "SELECT boss, difficulty, build,
                    COUNT(*),
                    AVG(NULLIF(duration, 0)),
                    MIN(NULLIF(duration, 0)),
                    AVG(NULLIF(your_dps, 0)),
                    MAX(your_dps),
                    MIN(timestamp), MAX(timestamp)
             FROM boss_kills
             GROUP BY boss, difficulty, build
             ORDER BY boss ASC, difficulty DESC, build ASC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RaidStat {
                    boss: r.get(0)?,
                    difficulty: r.get(1)?,
                    build: r.get(2)?,
                    kills: r.get(3)?,
                    avg_seconds: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    best_seconds: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    avg_dps: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                    best_dps: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                    first_kill: r.get(8)?,
                    last_kill: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn counts(&self) -> Result<(i64, i64, i64)> {
        let k: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM boss_kills", [], |r| r.get(0))?;
        let e: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM encounters", [], |r| r.get(0))?;
        let l: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM levels", [], |r| r.get(0))?;
        Ok((k, e, l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kill(ts: i64, boss: &str, build: &str) -> StoredKill {
        StoredKill {
            timestamp: ts,
            boss: boss.into(),
            difficulty: 2,
            zone: "Nagafen's Lair - Solo 2".into(),
            solo: true,
            build: build.into(),
            duration: 100,
            your_damage: 5000,
            your_dps: 50.0,
        }
    }

    /// The whole point: re-reading a log must not double-count.
    #[test]
    fn writing_the_same_kill_twice_stores_it_once() {
        let mut s = Store::in_memory().unwrap();
        s.put_kills(&[kill(1000, "Lord Nagafen", "WAR/DRU/BRD")]).unwrap();
        s.put_kills(&[kill(1000, "Lord Nagafen", "WAR/DRU/BRD")]).unwrap();
        assert_eq!(s.kills().unwrap().len(), 1);
        assert_eq!(s.counts().unwrap().0, 1);
    }

    /// A later parse that knows more must improve the row, not be ignored:
    /// the build label depends on evidence that may only arrive afterwards.
    #[test]
    fn a_later_parse_replaces_a_row_with_better_information() {
        let mut s = Store::in_memory().unwrap();
        s.put_kills(&[kill(1000, "Lord Nagafen", "")]).unwrap();
        s.put_kills(&[kill(1000, "Lord Nagafen", "WAR/DRU/BRD")]).unwrap();
        let k = s.kills().unwrap();
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].build, "WAR/DRU/BRD");
    }

    /// Two bosses dying in the same second are different kills.
    #[test]
    fn kills_at_the_same_instant_are_kept_apart_by_name() {
        let mut s = Store::in_memory().unwrap();
        s.put_kills(&[
            kill(1000, "Lord Nagafen", "x"),
            kill(1000, "Lady Vox", "x"),
        ])
        .unwrap();
        assert_eq!(s.kills().unwrap().len(), 2);
    }

    #[test]
    fn history_survives_reopening_the_database() {
        let dir = std::env::temp_dir().join("eqlparser-store-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("history.db");
        {
            let mut s = Store::open(&path).unwrap();
            s.put_kills(&[kill(1000, "Cazic-Thule", "WAR/DRU/BRD")]).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.kills().unwrap()[0].boss, "Cazic-Thule");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encounters_can_be_looked_up_by_boss_name() {
        let mut s = Store::in_memory().unwrap();
        s.put_encounters(&[
            StoredEncounter {
                start: 100,
                end: 200,
                title: "Lord Nagafen".into(),
                zone: "Nagafen's Lair".into(),
                duration: 100,
                your_damage: 9000,
                your_dps: 90.0,
                total_damage: 12000,
                kills: 1,
                deaths: 0,
                build: "WAR/DRU/BRD".into(),
            },
            StoredEncounter {
                start: 300,
                end: 380,
                title: "a fire giant".into(),
                zone: "Nagafen's Lair".into(),
                duration: 80,
                your_damage: 1000,
                your_dps: 12.5,
                total_damage: 1000,
                kills: 1,
                deaths: 0,
                build: "WAR/DRU/BRD".into(),
            },
        ])
        .unwrap();
        let found = s.encounters_named("Nagafen").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].your_dps, 90.0);
    }

    /// The guide unit: one row per boss, difficulty and build.
    #[test]
    fn raid_stats_group_by_boss_difficulty_and_build() {
        let mut s = Store::in_memory().unwrap();
        let mut a = kill(1000, "Lord Nagafen", "WAR/DRU/BRD");
        a.duration = 300;
        a.your_dps = 100.0;
        let mut b = kill(2000, "Lord Nagafen", "WAR/DRU/BRD");
        b.duration = 200;
        b.your_dps = 150.0;
        let mut c = kill(3000, "Lord Nagafen", "WAR/DRU/ROG");
        c.duration = 600;
        c.your_dps = 50.0;
        s.put_kills(&[a, b, c]).unwrap();

        let stats = s.raid_stats().unwrap();
        assert_eq!(stats.len(), 2, "one row per build");
        let brd = stats.iter().find(|r| r.build == "WAR/DRU/BRD").unwrap();
        assert_eq!(brd.kills, 2);
        assert_eq!(brd.avg_seconds, 250.0);
        assert_eq!(brd.best_seconds, 200);
        assert_eq!(brd.avg_dps, 125.0);
        assert_eq!(brd.best_dps, 150.0);
    }

    /// A kill whose fight could not be bounded must not drag averages to zero.
    #[test]
    fn kills_without_a_matched_fight_are_excluded_from_averages() {
        let mut s = Store::in_memory().unwrap();
        let mut good = kill(1000, "Lady Vox", "WAR/DRU/BRD");
        good.duration = 200;
        good.your_dps = 100.0;
        let mut orphan = kill(2000, "Lady Vox", "WAR/DRU/BRD");
        orphan.duration = 0;
        orphan.your_dps = 0.0;
        s.put_kills(&[good, orphan]).unwrap();

        let r = &s.raid_stats().unwrap()[0];
        assert_eq!(r.kills, 2, "both kills counted");
        assert_eq!(r.avg_seconds, 200.0, "unbounded fight excluded from timing");
        assert_eq!(r.avg_dps, 100.0);
    }

    #[test]
    fn levels_are_keyed_by_level_and_start() {
        let mut s = Store::in_memory().unwrap();
        let l = StoredLevel {
            level: 42,
            started: 1000,
            ended: 2000,
            seconds: 1000,
            kills: 50,
            percent: 100.0,
            build: "WAR/DRU/BRD".into(),
            zones: "[]".into(),
        };
        s.put_levels(&[l.clone()]).unwrap();
        s.put_levels(&[l]).unwrap();
        assert_eq!(s.levels().unwrap().len(), 1);
    }
}
