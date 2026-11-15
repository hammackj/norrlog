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
//! # Whose history
//!
//! Every per-character table is keyed on `(character, server, …)`. The client
//! names its logs `eqlog_<Character>_<server>.txt`, which is the only place
//! either appears — nothing inside a log says which server it came from.
//!
//! Without that, following an alt's log would merge their kills, levels and
//! turn-ins into the same rows as the main's, with nothing left to tell them
//! apart afterwards. Quest write-ups are deliberately *not* scoped: a guide to
//! a quest is true whoever is reading it.

use anyhow::Result;
use norrlog_core::game::Game;
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

/// One boss kill, with the fight it happened in, for export to eqldb.
///
/// # Why this exists beside `StoredKill`
///
/// `StoredKill` is scoped to the character the daemon is currently following,
/// because every other reader of this store wants exactly that. An export
/// wants the opposite: every character on file, because the leaderboard is a
/// comparison across builds and half the builds on this machine belong to a
/// character nobody is playing today.
///
/// It also carries the containing encounter. The site it goes to does not hold
/// fights — they are on the never-copy list there, on purpose — so the figures
/// a kill page can show have to travel with the kill or not at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedKill {
    pub character: String,
    pub server: String,
    /// The moment the boss died. Not the fight's start, which is `enc_start`.
    pub at: i64,
    pub boss: String,
    pub difficulty: u8,
    pub solo: bool,
    pub zone: String,
    pub build: String,
    /// The *finished* encounter's length. Deliberately not `boss_kills.duration`
    /// -- that is `parser_seconds` below, and it is frequently a snapshot taken
    /// while the fight was still running.
    pub duration: i64,
    /// What `boss_kills` recorded, carried so the far end can see the two
    /// disagree rather than being handed one number and no way to check it.
    pub parser_seconds: i64,
    pub your_damage: i64,
    pub your_dps: f64,
    pub enc_start: i64,
    pub enc_end: i64,
    pub enc_title: String,
    pub total_damage: i64,
    pub enc_kills: i64,
    pub deaths: i64,
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
    /// Instance tier from the zone name, so history can be filtered by it
    /// without re-deriving it from the zone on every query.
    #[serde(default)]
    pub difficulty: u8,
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

/// A spell the game handed over, rather than one that was bought or looted.
///
/// EverQuest Legends grants spells outright — "You have been granted the
/// following spell: Gate." — which is a fact about the game worth keeping:
/// it says a spell is real, reachable, and costs nothing to get, none of
/// which the client's own spell file says.
/// One item-from-creature observation, summed over every log on file.
///
/// A struct rather than the tuple this used to be: nine positional fields,
/// two of which are counts of similar-sounding things, is a call site nobody
/// can read and a swap nothing would catch.
#[derive(Debug, Clone)]
pub struct StoredDrop {
    pub game: String,
    /// Ungraded item name — see [`norrlog_core::drops::canonical_item`].
    pub item: String,
    pub npc: String,
    pub zones: Vec<String>,
    /// Times looted from this creature.
    pub looted: u64,
    /// Times seen on its corpse and left there. Rows written before the
    /// parser read that line are 0, which is true of them rather than a
    /// stand-in for unknown.
    pub left: u64,
    pub quantity: u64,
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGrant {
    pub spell: String,
    /// The level gained at that moment, or 0 when none was.
    ///
    /// Both cases are grants and both are worth having, but they mean
    /// different things. A grant with a level says *this spell is given at
    /// this level*. A grant without one came from a change of invocation,
    /// which hands over everything the new classes already qualify for; it
    /// says the spell is granted, and nothing about when.
    pub level: u32,
    pub at: i64,
    pub build: String,
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

/// Servers whose logs are not the game being played. The test server runs
/// changes before they ship, and not all of them ship. The same list as
/// eqldb's `UNPLAYED_SERVERS`, which refuses a bundle built from one.
pub const UNPLAYED_SERVERS: &[&str] = &["test"];

/// What [`Store::played_only`] returns. Dropping it drops the views.
pub struct PlayedOnly<'a> {
    conn: &'a Connection,
    views: Vec<String>,
}

impl Drop for PlayedOnly<'_> {
    fn drop(&mut self) {
        for v in &self.views {
            let _ = self
                .conn
                .execute_batch(&format!("DROP VIEW IF EXISTS temp.\"{v}\""));
        }
    }
}

/// The persistent history database.
pub struct Store {
    conn: Connection,
    /// Whose history is being read and written.
    character: String,
    server: String,
}

impl Store {
    /// Open (creating if needed) the history database, scoped to one character
    /// on one server. There is no unscoped open: every read and write has to
    /// say whose it is, or an alt's records merge into the main's.
    pub fn open_as(path: &Path, character: &str, server: &str) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        // The daemon writes in bursts while catching up on a log and then
        // trickles; WAL keeps those writes from blocking API reads.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Five seconds, not the default of zero. At zero a connection that
        // meets a writer gets SQLITE_BUSY *immediately* rather than waiting,
        // and under WAL that turns a slow write into a failed one. It has
        // never bitten because until now only one connection ever wrote:
        // `catchup` opens a second Store per swept log, under its own
        // `(character, server)`, while the follower is still writing the
        // log it tails.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut s = Store {
            conn,
            character: character.to_string(),
            server: server.to_string(),
        };
        s.migrate()?;
        s.add_identity_columns()?;
        s.add_aa_game()?;
        s.add_encounter_difficulty()?;
        s.add_vendor_quoted()?;
        s.add_npc_kind()?;
        s.add_drops_left_behind()?;
        let inferred = s.inferred_group_sources()?;
        if !inferred.is_empty() || s.repairs_pending()? {
            s.back_up_before_repair(path)?;
        }
        s.repair_rule_notice_zones()?;
        s.drop_graded_duplicates()?;
        s.repair_inferred_group_mode(&inferred)?;
        s.reread_logs_with_drops_once()?;
        Ok(s)
    }

    /// Have every log with drops on file read again from its start, once.
    ///
    /// `put_drops` used to replace rows only under the same key, and the key
    /// is how the item and creature are spelled. A fold that respelled them
    /// left the old rows beside the new: Tharn's log held 35,965 loots on file
    /// against the 22,817 lines in it. `put_drops` now clears a log's rows
    /// before writing, but only a whole read calls it, and a log that is not
    /// followed is read again only when it grows. Zeroing the watermark makes
    /// the next catch-up sweep read it whole; the followed log is read whole
    /// at every start anyway. Nothing is deleted here, and a log that no
    /// longer exists keeps what it has.
    fn reread_logs_with_drops_once(&mut self) -> Result<()> {
        const NAME: &str = "reread-logs-with-drops";
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS store_repairs (
                name TEXT PRIMARY KEY,
                at   INTEGER NOT NULL
            )",
        )?;
        let done: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM store_repairs WHERE name = ?1)",
            [NAME],
            |r| r.get(0),
        )?;
        if !done {
            let n = tx.execute(
                "UPDATE sources SET bytes = 0
                 WHERE bytes > 0 AND path IN (SELECT DISTINCT source FROM item_drops)",
                [],
            )?;
            if n > 0 {
                eprintln!("[norrlogd] {n} log(s) will be read again to recount their drops");
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            tx.execute(
                "INSERT INTO store_repairs (name, at) VALUES (?1, ?2)",
                params![NAME, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// An in-memory database, for tests.
    /// A store with no file behind it, for tests.
    #[cfg(test)]
    pub fn in_memory() -> Result<Store> {
        let mut s = Store {
            conn: Connection::open_in_memory()?,
            character: "Test".to_string(),
            server: "test".to_string(),
        };
        s.migrate()?;
        s.add_identity_columns()?;
        s.add_aa_game()?;
        s.add_encounter_difficulty()?;
        s.add_vendor_quoted()?;
        s.add_npc_kind()?;
        s.add_drops_left_behind()?;
        s.repair_rule_notice_zones()?;
        s.drop_graded_duplicates()?;
        Ok(s)
    }

    /// Put the game in the AA table, once.
    ///
    /// `aa_purchases` was the last per-character table with no game on it,
    /// and the only history table eqdb-export could not filter by game --
    /// not because the query forgot to, but because there was no column to
    /// filter on. One history holding a Legends character and an EverQuest
    /// one therefore published both games' AA purchases to whichever site
    /// asked.
    ///
    /// # Existing rows are not stamped with the current game
    ///
    /// `add_identity_columns` could backfill character and server from the
    /// log in front of it, because back then the table could only hold that
    /// log's rows. This one cannot: by the time a database needs this
    /// migration it provably holds several characters on several servers,
    /// and stamping them all with whichever log happens to be open is the
    /// precise inference that caused the bug.
    ///
    /// So the game is *looked up* instead. The tables that already carry it
    /// are keyed by character and server too, so a purchase by Tharn on
    /// freeport takes the game from Tharn-on-freeport's own drops and kills.
    /// Where those agree on exactly one game, that is evidence rather than a
    /// guess. Where they say nothing, or disagree, the row keeps an empty
    /// game: it is then skipped by anything filtering on one -- and comes
    /// back correctly stamped the next time its log is parsed, since every
    /// row here is re-derived from the log on each parse.
    fn add_aa_game(&mut self) -> Result<()> {
        let already: bool = self
            .conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('aa_purchases') WHERE name='game'")?
            .query_row([], |r| r.get::<_, i64>(0))?
            > 0;
        if already {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "
            CREATE TABLE aa_purchases_v3 (
                game TEXT NOT NULL DEFAULT '',
                character TEXT NOT NULL, server TEXT NOT NULL,
                ability TEXT NOT NULL, rank INTEGER NOT NULL,
                spent INTEGER NOT NULL DEFAULT 0, at INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (game, character, server, ability, rank)
            );
            ",
        )?;
        // One row per (character, server) that some game-carrying table can
        // speak for, and only where it speaks with one voice.
        tx.execute_batch(
            "
            CREATE TEMP TABLE aa_game_evidence AS
            SELECT character, server, MIN(game) AS game
            FROM (
                SELECT DISTINCT character, server, game FROM item_drops
                UNION SELECT DISTINCT character, server, game FROM npc_kills
                UNION SELECT DISTINCT character, server, game FROM npc_profiles
            )
            WHERE game <> ''
            GROUP BY lower(character), lower(server)
            HAVING COUNT(DISTINCT game) = 1;
            ",
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO aa_purchases_v3
                 (game, character, server, ability, rank, spent, at)
             SELECT COALESCE(
                        (SELECT e.game FROM aa_game_evidence e
                         WHERE lower(e.character) = lower(p.character)
                           AND lower(e.server) = lower(p.server)),
                        ''),
                    p.character, p.server, p.ability, p.rank, p.spent, p.at
             FROM aa_purchases p",
            [],
        )?;
        tx.execute_batch(
            "
            DROP TABLE aa_purchases;
            ALTER TABLE aa_purchases_v3 RENAME TO aa_purchases;
            DROP TABLE aa_game_evidence;
            ",
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Add the encounter difficulty column, once.
    ///
    /// `CREATE TABLE IF NOT EXISTS` cannot widen a table that already exists,
    /// so a database written before encounters carried a tier needs the column
    /// added. Existing rows default to 0 and are corrected on the next flush,
    /// which re-derives the tier from the zone and REPLACEs the row.
    fn add_encounter_difficulty(&mut self) -> Result<()> {
        let present: bool = self
            .conn
            .prepare(
                "SELECT COUNT(*) FROM pragma_table_info('encounters') WHERE name='difficulty'",
            )?
            .query_row([], |r| r.get::<_, i64>(0))?
            > 0;
        if !present {
            self.conn.execute_batch(
                "ALTER TABLE encounters ADD COLUMN difficulty INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        Ok(())
    }

    /// Add the vendor quote counter, once.
    ///
    /// Existing rows predate price quotes and can only have come from actual
    /// purchases, so a default of 0 is not a placeholder — it is the true
    /// count for every row already there.
    fn add_vendor_quoted(&mut self) -> Result<()> {
        let present: bool = self
            .conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('item_vendors') WHERE name='quoted'")?
            .query_row([], |r| r.get::<_, i64>(0))?
            > 0;
        if !present {
            self.conn.execute_batch(
                "ALTER TABLE item_vendors ADD COLUMN quoted INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        Ok(())
    }

    /// Add the left-behind counter to drop sightings, once.
    ///
    /// Existing rows predate the parser reading "--You left ... on ...--" at
    /// all, so nothing was ever counted for them and a default of 0 is the
    /// truth about those rows rather than a placeholder. They fill in as logs
    /// are re-read.
    fn add_drops_left_behind(&mut self) -> Result<()> {
        let present: bool = self
            .conn
            .prepare(
                "SELECT COUNT(*) FROM pragma_table_info('item_drops') WHERE name='left_behind'",
            )?
            .query_row([], |r| r.get::<_, i64>(0))?
            > 0;
        if !present {
            self.conn.execute_batch(
                "ALTER TABLE item_drops ADD COLUMN left_behind INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        Ok(())
    }

    /// Whether opening this history will change anything already on file:
    /// a place that is a rule announcement, or a graded drop counted twice.
    fn repairs_pending(&self) -> Result<bool> {
        let notice = |col: &str| {
            format!(
                "({col} GLOB 'an area *' OR {col} GLOB 'a area *' \
                 OR {col} GLOB 'an zone *' OR {col} GLOB 'a zone *')"
            )
        };
        let mut checks: Vec<String> = [
            "npc_profiles",
            "npc_melee",
            "npc_health_samples",
            "item_drop_places",
            "forage",
            "markers",
            "faction_hits",
            "encounters",
            "boss_kills",
            "spots",
            "map_zones",
        ]
        .iter()
        .map(|t| format!("SELECT 1 FROM {t} WHERE {}", notice("zone")))
        .collect();
        for t in ["item_drops", "levels"] {
            checks.push(format!(
                "SELECT 1 FROM {t} WHERE zones LIKE '%area %' OR zones LIKE '%zone %'"
            ));
        }
        for q in &checks {
            if self
                .conn
                .query_row(&format!("SELECT EXISTS({q})"), [], |r| r.get(0))?
            {
                return Ok(true);
            }
        }
        Ok(!self.graded_duplicates()?.is_empty())
    }

    /// A copy of the history beside it, taken once before a repair deletes
    /// anything: `history.db.before-repair-<unix seconds>`. The repairs only
    /// remove what they can prove is a duplicate, but a deleted row cannot
    /// be read back, and this is somebody's whole history.
    fn back_up_before_repair(&self, path: &Path) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = format!(
            "{}.before-repair-{now}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("history.db")
        );
        let to = path.with_file_name(name);
        self.conn
            .execute("VACUUM INTO ?1", params![to.to_string_lossy()])?;
        eprintln!(
            "[norrlogd] repairing history; a copy from before is at {}",
            to.display()
        );
        Ok(())
    }

    /// The logs whose `group` rows are the old inference, not the client's.
    ///
    /// Before f93d9a6 a tier with no mode on it (`Nagafen's Lair 2`) was
    /// read as a group instance, because no log on file had ever written
    /// `- Group`. The client does write it, so an unmarked tier is now an
    /// instance -- but history written before then still files those kills,
    /// swings and health samples under `group`, a mode the player was never
    /// in, and once the same log was read again it has them twice: under
    /// `group` from the first read and `instance` from the second.
    ///
    /// Whether a log's `group` rows are inference is a fact about the log:
    /// one that never says `- Group` cannot have produced a real one. So each
    /// log with `group` rows is read once, for that, and the answer kept in
    /// `group_mode_checked`. A log that does say it is left alone, inferred
    /// and real rows being indistinguishable there; so is a log that is no
    /// longer on disk, until it is.
    fn inferred_group_sources(&self) -> Result<Vec<String>> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS group_mode_checked (
                source TEXT PRIMARY KEY,
                marked INTEGER NOT NULL
            )",
        )?;
        let candidates: Vec<String> = {
            let mut st = self.conn.prepare(
                "SELECT DISTINCT source FROM (
                    SELECT source FROM npc_profiles WHERE mode = 'group'
                    UNION SELECT source FROM npc_melee WHERE mode = 'group'
                    UNION SELECT source FROM npc_health_samples WHERE mode = 'group'
                    UNION SELECT source FROM item_drop_places WHERE mode = 'group'
                 ) WHERE source NOT IN (SELECT source FROM group_mode_checked)",
            )?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut out = Vec::new();
        for source in candidates {
            let Some(marked) = log_marks_group(Path::new(&source)) else {
                continue;
            };
            if marked {
                self.conn.execute(
                    "INSERT OR REPLACE INTO group_mode_checked (source, marked) VALUES (?1, 1)",
                    params![source],
                )?;
            } else {
                out.push(source);
            }
        }
        Ok(out)
    }

    /// Refile the inferred `group` rows of these logs as `instance`.
    ///
    /// A row with no `instance` twin is the only record of what it counts
    /// and is renamed. A row with one was read twice -- every twin in the
    /// histories on file has its sightings inside the twin's -- so it is not
    /// added in; the twin keeps the larger of each count instead, because a
    /// handful of first reads counted a kill or a swing more than the
    /// re-read, and a repair should not make a number smaller than the log
    /// once showed.
    fn repair_inferred_group_mode(&mut self, sources: &[String]) -> Result<()> {
        if sources.is_empty() {
            return Ok(());
        }
        const ID: &str = "t.game = g.game AND t.character = g.character AND t.server = g.server \
             AND t.source = g.source AND t.zone = g.zone AND t.difficulty = g.difficulty \
             AND t.npc = g.npc AND t.mode = 'instance'";
        let tx = self.conn.transaction()?;
        for source in sources {
            tx.execute(
                &format!(
                    "UPDATE npc_profiles AS t SET
                        kills = MAX(t.kills, g.kills),
                        deaths_to_it = MAX(t.deaths_to_it, g.deaths_to_it),
                        max_hit = MAX(t.max_hit, g.max_hit),
                        first_seen = MIN(t.first_seen, g.first_seen),
                        last_seen = MAX(t.last_seen, g.last_seen)
                     FROM npc_profiles AS g
                     WHERE g.source = ?1 AND g.mode = 'group' AND {ID}"
                ),
                params![source],
            )?;
            // A melee row's counts belong together (hits and misses make the
            // swings), so the twin with more swings is kept whole.
            tx.execute(
                &format!(
                    "UPDATE npc_melee AS t SET
                        swings = g.swings, hits = g.hits, misses = g.misses,
                        avoided = g.avoided, damage = g.damage,
                        max_hit = MAX(t.max_hit, g.max_hit)
                     FROM npc_melee AS g
                     WHERE g.source = ?1 AND g.mode = 'group' AND {ID}
                       AND t.level = g.level AND t.stance = g.stance
                       AND g.swings > t.swings"
                ),
                params![source],
            )?;
            tx.execute(
                "UPDATE item_drop_places AS t SET
                    looted = MAX(t.looted, g.looted),
                    quantity = MAX(t.quantity, g.quantity)
                 FROM item_drop_places AS g
                 WHERE g.source = ?1 AND g.mode = 'group'
                   AND t.game = g.game AND t.character = g.character
                   AND t.server = g.server AND t.source = g.source
                   AND t.item = g.item AND t.npc = g.npc AND t.zone = g.zone
                   AND t.difficulty = g.difficulty AND t.mode = 'instance'",
                params![source],
            )?;
            // Health samples are keyed to the second and the hit points, so a
            // twin is the same sample and there is nothing to merge.
            for t in [
                "npc_profiles",
                "npc_melee",
                "npc_health_samples",
                "item_drop_places",
            ] {
                tx.execute(
                    &format!(
                        "UPDATE OR IGNORE {t} SET mode = 'instance' \
                         WHERE source = ?1 AND mode = 'group'"
                    ),
                    params![source],
                )?;
                // What is left had a twin, merged above.
                tx.execute(
                    &format!("DELETE FROM {t} WHERE source = ?1 AND mode = 'group'"),
                    params![source],
                )?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO group_mode_checked (source, marked) VALUES (?1, 0)",
                params![source],
            )?;
        }
        tx.commit()?;
        eprintln!(
            "[norrlogd] refiled the inferred group rows of {} log(s) as instance",
            sources.len()
        );
        Ok(())
    }

    /// Take the client's rule announcements back out of the places on file.
    ///
    /// "You have entered an area where Bind Affinity is allowed." was once
    /// read as entering a zone called "an area where Bind Affinity is
    /// allowed", so history written then files kills, drops and fights under
    /// a place that does not exist. The parser no longer does that
    /// (`ZoneRuleNotice`); this repairs what it left behind, and runs on
    /// every open because it costs one indexed-free scan and finds nothing
    /// once the history is clean.
    ///
    /// A log is read once, from a watermark, so a stretch filed under a
    /// notice is usually never read again. Whether it was decides the
    /// repair, per log and per notice:
    ///
    /// - **Read again since the fix:** the same creatures are on file again
    ///   under the real zone. A health sample, keyed to the second and the
    ///   hit points, filed twice proves it; failing that, every creature under
    ///   the notice having a real-zone row whose first and last sightings
    ///   span its own. The rows under the notice are duplicates and go.
    /// - **Not read again:** they are the only record, so they stay, under
    ///   the empty zone, which every reader shows as an unknown place.
    ///   Nothing says which zone it really was. Should a row with the empty
    ///   zone and the same key already exist, the notice row is dropped
    ///   rather than summed into it -- a creature already filed under no zone
    ///   in the same log, at the same mode and tier, which no history on
    ///   file has.
    ///
    /// Fights carry no log path, so each is judged alone: one that a fight
    /// in a real zone overlaps is a duplicate, and the rest lose the zone.
    /// Rows whose key leaves the zone out (`item_drops`, `spots`, `levels`)
    /// were rewritten in place by any re-read, so only their zone value needs
    /// cleaning.
    fn repair_rule_notice_zones(&mut self) -> Result<()> {
        // The SQL form of `norrlog_core::zone::is_rule_notice`; a test holds
        // the two to the same answers.
        fn notice(col: &str) -> String {
            format!(
                "({col} GLOB 'an area *' OR {col} GLOB 'a area *' \
                 OR {col} GLOB 'an zone *' OR {col} GLOB 'a zone *')"
            )
        }
        // Per-log tables with the zone in the key.
        const KEYED: &[&str] = &[
            "npc_profiles",
            "npc_melee",
            "npc_health_samples",
            "item_drop_places",
            "forage",
            "markers",
            "faction_hits",
        ];
        const LOG: &str = "game, character, server, source";

        let tx = self.conn.transaction()?;

        // Which (log, notice) stretches there are.
        let union = KEYED
            .iter()
            .map(|t| format!("SELECT {LOG}, zone FROM {t} WHERE {}", notice("zone")))
            .collect::<Vec<_>>()
            .join(" UNION ");
        let stretches: Vec<[String; 5]> = {
            let mut st = tx.prepare(&union)?;
            let rows = st.query_map([], |r| {
                Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?])
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };

        let same_log = "o.game = n.game AND o.character = n.character \
                        AND o.server = n.server AND o.source = n.source";
        let at = "n.game = ?1 AND n.character = ?2 AND n.server = ?3 \
                  AND n.source = ?4 AND n.zone = ?5";
        for key in &stretches {
            let p = rusqlite::params![key[0], key[1], key[2], key[3], key[4]];
            let sample_twice: bool = tx.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM npc_health_samples n WHERE {at} AND EXISTS(
                         SELECT 1 FROM npc_health_samples o WHERE {same_log}
                           AND o.mode = n.mode AND o.difficulty = n.difficulty
                           AND o.npc = n.npc AND o.at = n.at AND o.hp = n.hp
                           AND NOT {}))",
                    notice("o.zone")
                ),
                p,
                |r| r.get(0),
            )?;
            let (creatures, spanned): (i64, i64) = tx.query_row(
                &format!(
                    "SELECT COUNT(*), COALESCE(SUM(EXISTS(
                         SELECT 1 FROM npc_profiles o WHERE {same_log}
                           AND o.npc = n.npc AND NOT {}
                           AND o.first_seen <= n.first_seen
                           AND o.last_seen >= n.last_seen)), 0)
                     FROM npc_profiles n WHERE {at}",
                    notice("o.zone")
                ),
                p,
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let read_again = sample_twice || (creatures > 0 && spanned == creatures);
            let here = "game = ?1 AND character = ?2 AND server = ?3 \
                        AND source = ?4 AND zone = ?5";
            for t in KEYED {
                if !read_again {
                    tx.execute(
                        &format!("UPDATE OR IGNORE {t} SET zone = '' WHERE {here}"),
                        p,
                    )?;
                }
                // Read again: all of it. Not: only what collided with an
                // existing unknown-zone row and so could not be moved.
                tx.execute(&format!("DELETE FROM {t} WHERE {here}"), p)?;
            }
        }

        // Fights.
        tx.execute(
            &format!(
                "DELETE FROM encounters WHERE {} AND EXISTS(
                     SELECT 1 FROM encounters o
                     WHERE o.character = encounters.character
                       AND o.server = encounters.server
                       AND o.start <= encounters.end AND o.end >= encounters.start
                       AND o.zone <> '' AND NOT {})",
                notice("zone"),
                notice("o.zone")
            ),
            [],
        )?;
        tx.execute(
            &format!(
                "UPDATE OR IGNORE encounters SET zone = '' WHERE {}",
                notice("zone")
            ),
            [],
        )?;
        tx.execute(
            &format!("UPDATE boss_kills SET zone = '' WHERE {}", notice("zone")),
            [],
        )?;
        tx.execute(
            &format!(
                "UPDATE spots SET zone = '', zone_short = '' WHERE {}",
                notice("zone")
            ),
            [],
        )?;
        tx.execute(
            &format!("DELETE FROM map_zones WHERE {}", notice("zone")),
            [],
        )?;

        // Sets of places, `|`-joined in item_drops and a JSON list in levels.
        let drops: Vec<(i64, String)> = {
            let mut st = tx.prepare(
                "SELECT rowid, zones FROM item_drops \
                 WHERE zones LIKE '%area %' OR zones LIKE '%zone %'",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for (id, zones) in drops {
            let kept: Vec<&str> = zones
                .split('|')
                .filter(|z| !norrlog_core::zone::is_rule_notice(z))
                .collect();
            if kept.len() != zones.split('|').count() {
                tx.execute(
                    "UPDATE item_drops SET zones = ?1 WHERE rowid = ?2",
                    rusqlite::params![kept.join("|"), id],
                )?;
            }
        }
        let levels: Vec<(i64, String)> = {
            let mut st = tx.prepare(
                "SELECT rowid, zones FROM levels \
                 WHERE zones LIKE '%area %' OR zones LIKE '%zone %'",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for (id, zones) in levels {
            let Ok(list) = serde_json::from_str::<Vec<String>>(&zones) else {
                continue;
            };
            let kept: Vec<&String> = list
                .iter()
                .filter(|z| !norrlog_core::zone::is_rule_notice(z))
                .collect();
            if kept.len() != list.len() {
                tx.execute(
                    "UPDATE levels SET zones = ?1 WHERE rowid = ?2",
                    rusqlite::params![serde_json::to_string(&kept)?, id],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete drop rows still filed under a graded name that a later read of
    /// the same log has counted again under the item's own name.
    ///
    /// Before f24fa31 a drop was keyed on the roll, `White Satin Gloves +1`,
    /// so a log read then left one row per grade. Since then the same log
    /// read again writes one row, `White Satin Gloves`, counting every grade
    /// -- and the graded rows, keyed differently, stay. `all_drops` folds
    /// names on the way out and sums both, so those loots counted twice.
    ///
    /// A graded row goes only when that is proved:
    ///
    /// - the same log has a row under the ungraded name with places behind it
    ///   in `item_drop_places`, which only code newer than the fold writes
    ///   (1e5fa68), so that row counts every grade it read; and
    /// - that row's first and last sightings span the graded row's, so the
    ///   read it came from covered those loots.
    ///
    /// Anything else stays: a graded row with no such twin is the only record
    /// of its loots, and the fold on the way out counts it once. Before the
    /// fold an ungraded row was a separate fact, the drops with no roll, and
    /// it has no places, so it is never mistaken for a twin.
    fn drop_graded_duplicates(&mut self) -> Result<()> {
        let ids = self.graded_duplicates()?;
        let tx = self.conn.transaction()?;
        for id in ids {
            tx.execute("DELETE FROM item_drops WHERE rowid = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The rows `drop_graded_duplicates` deletes, by rowid.
    fn graded_duplicates(&self) -> Result<Vec<i64>> {
        let graded: Vec<(i64, [String; 6], i64, i64)> = {
            let mut st = self.conn.prepare(
                "SELECT rowid, game, character, server, source, item, npc, first_seen, last_seen
                 FROM item_drops WHERE item GLOB '*+[0-9]*'",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    [
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ],
                    r.get(7)?,
                    r.get(8)?,
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut twin = self.conn.prepare(
            "SELECT EXISTS(SELECT 1 FROM item_drops d
                 WHERE d.game = ?1 AND d.character = ?2 AND d.server = ?3
                   AND d.source = ?4 AND lower(d.item) = lower(?5)
                   AND lower(d.npc) = lower(?6)
                   AND d.first_seen <= ?7 AND d.last_seen >= ?8
                   AND EXISTS(SELECT 1 FROM item_drop_places p
                       WHERE p.game = d.game AND p.character = d.character
                         AND p.server = d.server AND p.source = d.source
                         AND p.item = d.item AND p.npc = d.npc))",
        )?;
        let mut out = Vec::new();
        for (id, [game, character, server, source, item, npc], first, last) in graded {
            let (base, Some(_)) = norrlog_core::drops::split_grade(&item) else {
                continue;
            };
            let found: bool = twin.query_row(
                params![game, character, server, source, base, npc, first, last],
                |r| r.get(0),
            )?;
            if found {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Add the creature body type to the bestiary, once.
    ///
    /// Existing rows predate the evidence entirely, and the empty default says
    /// exactly the right thing about them: nothing has been proved. They fill
    /// in on their own as logs are re-read.
    fn add_npc_kind(&mut self) -> Result<()> {
        let present: bool = self
            .conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('npc_profiles') WHERE name='kind'")?
            .query_row([], |r| r.get::<_, i64>(0))?
            > 0;
        if !present {
            self.conn.execute_batch(
                "ALTER TABLE npc_profiles ADD COLUMN kind TEXT NOT NULL DEFAULT ''",
            )?;
        }
        Ok(())
    }

    /// Add character and server to the per-character tables, once.
    ///
    /// Existing rows predate the distinction and can only have come from the
    /// log in front of us, so they are backfilled with the current identity.
    /// SQLite cannot alter a primary key in place, so each table is rebuilt;
    /// `INSERT OR IGNORE` into the new shape keeps that safe to re-run.
    fn add_identity_columns(&mut self) -> Result<()> {
        let already: bool = self
            .conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('boss_kills') WHERE name='character'")?
            .query_row([], |r| r.get::<_, i64>(0))?
            > 0;
        if already {
            return Ok(());
        }

        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "
            CREATE TABLE boss_kills_v2 (
                character TEXT NOT NULL, server TEXT NOT NULL,
                timestamp INTEGER NOT NULL, boss TEXT NOT NULL,
                difficulty INTEGER NOT NULL, zone TEXT NOT NULL DEFAULT '',
                solo INTEGER NOT NULL DEFAULT 0, build TEXT NOT NULL DEFAULT '',
                duration INTEGER NOT NULL DEFAULT 0,
                your_damage INTEGER NOT NULL DEFAULT 0,
                your_dps REAL NOT NULL DEFAULT 0,
                PRIMARY KEY (character, server, timestamp, boss)
            );
            CREATE TABLE encounters_v2 (
                character TEXT NOT NULL, server TEXT NOT NULL,
                start INTEGER NOT NULL, end INTEGER NOT NULL, title TEXT NOT NULL,
                zone TEXT NOT NULL DEFAULT '', duration INTEGER NOT NULL DEFAULT 0,
                your_damage INTEGER NOT NULL DEFAULT 0, your_dps REAL NOT NULL DEFAULT 0,
                total_damage INTEGER NOT NULL DEFAULT 0, kills INTEGER NOT NULL DEFAULT 0,
                deaths INTEGER NOT NULL DEFAULT 0, build TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (character, server, start, title)
            );
            CREATE TABLE levels_v2 (
                character TEXT NOT NULL, server TEXT NOT NULL,
                level INTEGER NOT NULL, started INTEGER NOT NULL,
                ended INTEGER NOT NULL DEFAULT 0, seconds INTEGER NOT NULL DEFAULT 0,
                kills INTEGER NOT NULL DEFAULT 0, percent REAL NOT NULL DEFAULT 0,
                build TEXT NOT NULL DEFAULT '', zones TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (character, server, level, started)
            );
            CREATE TABLE aa_purchases_v2 (
                character TEXT NOT NULL, server TEXT NOT NULL,
                ability TEXT NOT NULL, rank INTEGER NOT NULL,
                spent INTEGER NOT NULL DEFAULT 0, at INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (character, server, ability, rank)
            );
            CREATE TABLE sky_turnins_v2 (
                character TEXT NOT NULL, server TEXT NOT NULL,
                quest TEXT NOT NULL, at INTEGER NOT NULL,
                PRIMARY KEY (character, server, quest, at)
            );
            ",
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO boss_kills_v2
             SELECT ?1, ?2, timestamp, boss, difficulty, zone, solo, build, duration,
                    your_damage, your_dps FROM boss_kills",
            params![c, sv],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO encounters_v2
             SELECT ?1, ?2, start, end, title, zone, duration, your_damage, your_dps,
                    total_damage, kills, deaths, build FROM encounters",
            params![c, sv],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO levels_v2
             SELECT ?1, ?2, level, started, ended, seconds, kills, percent, build, zones
             FROM levels",
            params![c, sv],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO aa_purchases_v2
             SELECT ?1, ?2, ability, rank, spent, at FROM aa_purchases",
            params![c, sv],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO sky_turnins_v2 SELECT ?1, ?2, quest, at FROM sky_turnins",
            params![c, sv],
        )?;
        tx.execute_batch(
            "
            DROP TABLE boss_kills;   ALTER TABLE boss_kills_v2   RENAME TO boss_kills;
            DROP TABLE encounters;   ALTER TABLE encounters_v2   RENAME TO encounters;
            DROP TABLE levels;       ALTER TABLE levels_v2       RENAME TO levels;
            DROP TABLE aa_purchases; ALTER TABLE aa_purchases_v2 RENAME TO aa_purchases;
            DROP TABLE sky_turnins;  ALTER TABLE sky_turnins_v2  RENAME TO sky_turnins;
            CREATE INDEX IF NOT EXISTS boss_kills_who ON boss_kills(character, server);
            CREATE INDEX IF NOT EXISTS encounters_who ON encounters(character, server);
            ",
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Who this store is reading and writing as.
    pub fn identity(&self) -> (&str, &str) {
        (&self.character, &self.server)
    }

    /// Characters seen in the history, as (character, server, boss kills).
    pub fn characters(&self) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT character, server, COUNT(*) FROM boss_kills
             GROUP BY character, server ORDER BY COUNT(*) DESC",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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
                difficulty   INTEGER NOT NULL DEFAULT 0,
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
            -- Spells the game handed over. Keyed by spell and time so that
            -- re-parsing a log is idempotent and so the record survives the
            -- log rotating away -- the same reason sky_turnins is keyed that
            -- way. The same spell can be granted twice: once on levelling and
            -- again after a change of invocation.
            CREATE TABLE IF NOT EXISTS spell_grants (
                character TEXT NOT NULL,
                server TEXT NOT NULL,
                spell TEXT NOT NULL,
                level INTEGER NOT NULL DEFAULT 0,
                at INTEGER NOT NULL DEFAULT 0,
                build TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (character, server, spell, at)
            );

            -- Levels the player entered by hand. Per character and server from
            -- the start: an alt's classes are their own.
            CREATE TABLE IF NOT EXISTS class_levels (
                character TEXT NOT NULL,
                server TEXT NOT NULL,
                class TEXT NOT NULL,
                level INTEGER NOT NULL,
                at INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (character, server, class)
            );
            -- Figures the player read off their own character sheet. Nothing
            -- reads or writes it since the stated AA spend was dropped; kept so
            -- a figure that needs it later costs no migration.
            CREATE TABLE IF NOT EXISTS stated_values (
                character TEXT NOT NULL,
                server TEXT NOT NULL,
                key TEXT NOT NULL,
                value INTEGER NOT NULL,
                at INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (character, server, key)
            );
            CREATE TABLE IF NOT EXISTS sky_turnins (
                quest TEXT    NOT NULL,
                at    INTEGER NOT NULL,
                PRIMARY KEY (quest, at)
            );

            -- Alert rules. JSON rather than columns: the trigger is
            -- a tagged union whose arms grow, and columns would have to be
            -- migrated every time one is added.
            --
            -- Deliberately not scoped to a character. An alert is a habit of
            -- the person playing — you want to hear about a charm break on
            -- every character that can charm, not to rebuild the rule set
            -- after each log switch.
            CREATE TABLE IF NOT EXISTS alerts (
                id      TEXT PRIMARY KEY,
                data    TEXT NOT NULL,
                updated INTEGER NOT NULL DEFAULT 0
            );

            -- What was seen dropping from what, and the kills behind it.
            --
            -- Keyed by the log it came from as well as the character, because
            -- these are running totals rather than events: a re-parse of one
            -- log REPLACEs that log's rows, while a second log adds its own.
            -- Summing across the rows is therefore correct however many times
            -- anything is re-read, which a single per-character total would
            -- not be.
            --
            -- `game` is part of the key, not a detail hung off it. The same
            -- creature in EverQuest and EverQuest Legends may not drop the
            -- same things, and data that cannot say which game it describes
            -- cannot be corrected later.
            CREATE TABLE IF NOT EXISTS item_drops (
                game      TEXT NOT NULL,
                character TEXT NOT NULL,
                server    TEXT NOT NULL,
                source    TEXT NOT NULL,
                item      TEXT NOT NULL,
                npc       TEXT NOT NULL,
                zones     TEXT NOT NULL DEFAULT '',
                looted    INTEGER NOT NULL,
                -- Times seen on the corpse and left there. Same evidence
                -- about the creature, a different fact about you, so it is
                -- its own column rather than folded into `looted`.
                left_behind INTEGER NOT NULL DEFAULT 0,
                quantity  INTEGER NOT NULL,
                first_seen INTEGER NOT NULL,
                last_seen  INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, item, npc)
            );

            -- Where those loots happened, split out of the row above.
            --
            -- `item_drops.zones` is the *set* of places a pairing was seen in
            -- and `looted` is one count across all of them, which is not an
            -- attribution: a black wolf dropped 832 Chunk of Meat over three
            -- zones as a single number, and nothing on file could say which
            -- zone produced how many. The engine has always known -- the
            -- sighting carries `places` -- and this table is where that stops
            -- being thrown away on the way to disk.
            --
            -- Mode and difficulty are part of the key rather than columns on
            -- a zone, because `solo T1` and `instance T1` in one zone are
            -- different fights. `creatures` already keys kills that way and a
            -- drop breakdown that did not would not join to it.
            --
            -- A pairing with no rows here is *unknown*, not open-world at
            -- tier 0: rows written before this table existed have none, and
            -- the bundle omits an empty breakdown rather than inventing one.
            CREATE TABLE IF NOT EXISTS item_drop_places (
                game       TEXT NOT NULL,
                character  TEXT NOT NULL,
                server     TEXT NOT NULL,
                source     TEXT NOT NULL,
                item       TEXT NOT NULL,
                npc        TEXT NOT NULL,
                zone       TEXT NOT NULL,
                mode       TEXT NOT NULL,
                difficulty INTEGER NOT NULL,
                looted     INTEGER NOT NULL,
                quantity   INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, item, npc,
                             zone, mode, difficulty)
            );

            -- Kills per creature, the denominator behind a sighting count.
            -- Separate from item_drops because a creature that dropped
            -- nothing still has to be counted: it is evidence about
            -- everything it did *not* drop.
            CREATE TABLE IF NOT EXISTS npc_kills (
                game      TEXT NOT NULL,
                character TEXT NOT NULL,
                server    TEXT NOT NULL,
                source    TEXT NOT NULL,
                npc       TEXT NOT NULL,
                kills     INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, npc)
            );

            -- The bestiary: one row per creature per log.
            --
            -- Keyed on the zone variant, not just the name, because that is
            -- what a creature *is* here — the same name at Awakened and at
            -- Refined has different health and different loot, and a table
            -- that cannot tell them apart can never be corrected into one
            -- that can. (npc_kills above predates this and is still keyed on
            -- the bare name; it pools tiers, and should follow.)
            --
            -- `source` is in the key for the usual reason: these are running
            -- totals, so re-parsing a log must replace its own rows while a
            -- second log adds to them.
            CREATE TABLE IF NOT EXISTS npc_profiles (
                game       TEXT NOT NULL,
                character  TEXT NOT NULL,
                server     TEXT NOT NULL,
                source     TEXT NOT NULL,
                zone       TEXT NOT NULL,
                mode       TEXT NOT NULL,
                difficulty INTEGER NOT NULL,
                npc        TEXT NOT NULL,
                kills        INTEGER NOT NULL,
                deaths_to_it INTEGER NOT NULL,
                max_hit      INTEGER NOT NULL,
                first_seen   INTEGER NOT NULL,
                last_seen    INTEGER NOT NULL,
                -- Body type, where a body-restricted spell has proved one.
                -- Empty for the great majority: it is only ever set by someone
                -- casting such a spell at this creature.
                kind         TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (game, character, server, source, zone, mode, difficulty, npc)
            );

            -- Melee, split by the pairing that produced it.
            --
            -- A hit rate is a fact about a creature swinging at one character,
            -- at one level, in one stance. Level and stance are in the key so
            -- that levelling up or switching stance opens a new row instead of
            -- quietly contaminating the old one — the same rule the in-memory
            -- bestiary follows, carried into storage.
            CREATE TABLE IF NOT EXISTS npc_melee (
                game       TEXT NOT NULL,
                character  TEXT NOT NULL,
                server     TEXT NOT NULL,
                source     TEXT NOT NULL,
                zone       TEXT NOT NULL,
                mode       TEXT NOT NULL,
                difficulty INTEGER NOT NULL,
                npc        TEXT NOT NULL,
                level      INTEGER NOT NULL,
                stance     TEXT NOT NULL,
                swings   INTEGER NOT NULL,
                hits     INTEGER NOT NULL,
                misses   INTEGER NOT NULL,
                avoided  INTEGER NOT NULL,
                damage   INTEGER NOT NULL,
                max_hit  INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, zone, mode, difficulty, npc,
                             level, stance)
            );

            -- What a creature cast, per spell: announced casts, direct hits
            -- and damage-over-time ticks, each counted apart. They are
            -- different events and are never summed (see NpcCast).
            CREATE TABLE IF NOT EXISTS npc_casts (
                game         TEXT NOT NULL,
                character    TEXT NOT NULL,
                server       TEXT NOT NULL,
                source       TEXT NOT NULL,
                zone         TEXT NOT NULL,
                mode         TEXT NOT NULL,
                difficulty   INTEGER NOT NULL,
                npc          TEXT NOT NULL,
                spell        TEXT NOT NULL,
                casts        INTEGER NOT NULL,
                hits         INTEGER NOT NULL,
                damage_total INTEGER NOT NULL,
                damage_min   INTEGER NOT NULL,
                damage_max   INTEGER NOT NULL,
                element      TEXT NOT NULL DEFAULT '',
                ticks        INTEGER NOT NULL,
                tick_damage  INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, zone, mode, difficulty, npc,
                             spell)
            );

            -- One row per death that measured anything.
            --
            -- The summary is deliberately *not* stored. A median cannot be
            -- merged with another log's median, so keeping min/median/max per
            -- log would make the combined figure unrecoverable; keeping the
            -- samples makes it exact. `at` is the kill's own timestamp, which
            -- both keys the row and lets any figure be traced back to the
            -- kills behind it. Two of a name dying in the same second having
            -- absorbed exactly the same damage would key alike and count
            -- once; a lost sample is the right way to lose that argument,
            -- since the alternative is a synthetic id that stops a re-parse
            -- from replacing its own rows.
            CREATE TABLE IF NOT EXISTS npc_health_samples (
                game       TEXT NOT NULL,
                character  TEXT NOT NULL,
                server     TEXT NOT NULL,
                source     TEXT NOT NULL,
                zone       TEXT NOT NULL,
                mode       TEXT NOT NULL,
                difficulty INTEGER NOT NULL,
                npc        TEXT NOT NULL,
                at         INTEGER NOT NULL,
                hp         INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, zone, mode, difficulty, npc,
                             at, hp)
            );

            -- What a merchant charged, and where they stand. The buying half
            -- of where an item comes from; item_drops is the killing half.
            --
            -- Per merchant, never blended: the same reagent costs different
            -- amounts from different vendors and at different factions, and
            -- an average would destroy the one fact worth keeping — which
            -- vendor is cheapest, and in which zone.
            --
            -- Keyed by log for the same reason as item_drops: these are
            -- running totals, so a re-parse must replace its own rows while a
            -- second log adds to them.
            CREATE TABLE IF NOT EXISTS item_vendors (
                game      TEXT NOT NULL,
                character TEXT NOT NULL,
                server    TEXT NOT NULL,
                source    TEXT NOT NULL,
                item      TEXT NOT NULL,
                merchant  TEXT NOT NULL,
                zone      TEXT NOT NULL DEFAULT '',
                unit_copper INTEGER NOT NULL,
                bought      INTEGER NOT NULL,
                -- Times the merchant quoted the price without a sale. A
                -- price can be known from clicking an item in the shop
                -- window, which costs nothing; `bought` and `quoted` are the
                -- two ways the same number gets established.
                quoted      INTEGER NOT NULL DEFAULT 0,
                last_bought INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, item, merchant)
            );

            -- Which log files have been seen, purely so the UI can say what
            -- the history is built from. Never used to decide what to parse:
            -- that is what the natural keys are for.
            CREATE TABLE IF NOT EXISTS sources (
                path      TEXT PRIMARY KEY,
                last_seen INTEGER NOT NULL,
                bytes     INTEGER NOT NULL DEFAULT 0
            );

            -- ---- the record ------------------------------------------------
            --
            -- Six tables for what `crate::record` collects. They follow the
            -- same rule as `item_drops` above and for the same reason:
            -- `source` is in every key, so re-reading a log replaces that
            -- log's own rows while a second log adds to them. Summing across
            -- rows is therefore correct however many times anything is
            -- re-read, which a single running total would not be.

            -- Levels a creature has been seen conning at.
            --
            -- One row per level rather than a packed map, so a merge across
            -- logs is a SUM rather than a parse. `rare` and `raid` are facts
            -- about the creature and so repeat across its level rows; merge
            -- them with MAX.
            --
            -- A creature with no levels at all is written at level **-1**,
            -- with a count of zero, purely so its rare and raid marks
            -- survive. Minus one rather than zero because zero is a
            -- *representable* level -- `Event::Consider` carries a `u32` and
            -- the collector files whatever it is given -- and a sentinel that
            -- collides with a value the column can hold is a count silently
            -- dropped the first time the grammar changes. Today's grammar
            -- requires `(Lvl: N)`, so the placeholder is defensive and the
            -- rows it guards do not yet occur.
            -- What forage yielded, per zone, per log.
            --
            -- One row per (zone, item), plus one with an empty `item` for the
            -- attempts that yielded nothing. That row is not a placeholder to
            -- be skipped: it is the denominator, and a zone whose only row is
            -- the empty one is a zone somebody foraged in and got nothing
            -- from, which is a fact about the zone.
            CREATE TABLE IF NOT EXISTS forage (
                game      TEXT NOT NULL,
                character TEXT NOT NULL,
                server    TEXT NOT NULL,
                source    TEXT NOT NULL,
                zone      TEXT NOT NULL,
                item      TEXT NOT NULL,
                times     INTEGER NOT NULL,
                first_seen INTEGER NOT NULL DEFAULT 0,
                last_seen  INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (game, character, server, source, zone, item)
            );

            CREATE TABLE IF NOT EXISTS considers (
                game      TEXT NOT NULL,
                character TEXT NOT NULL,
                server    TEXT NOT NULL,
                source    TEXT NOT NULL,
                creature  TEXT NOT NULL,
                level     INTEGER NOT NULL,
                times     INTEGER NOT NULL,
                rare      INTEGER NOT NULL DEFAULT 0,
                raid      INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (game, character, server, source, creature, level)
            );

            -- Faction moved by killing something, one row per distinct
            -- adjustment. Not an average: a creature whose hit varies keeps
            -- both readings. The kill count is the sum of `times`, so it is
            -- not stored.
            CREATE TABLE IF NOT EXISTS faction_hits (
                game      TEXT NOT NULL,
                character TEXT NOT NULL,
                server    TEXT NOT NULL,
                source    TEXT NOT NULL,
                creature  TEXT NOT NULL,
                zone      TEXT NOT NULL,
                faction   TEXT NOT NULL,
                amount    INTEGER NOT NULL,
                times     INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, creature, zone, faction, amount)
            );

            -- The same, for handing an item to somebody.
            CREATE TABLE IF NOT EXISTS faction_turnins (
                game      TEXT NOT NULL,
                character TEXT NOT NULL,
                server    TEXT NOT NULL,
                source    TEXT NOT NULL,
                npc       TEXT NOT NULL,
                item      TEXT NOT NULL,
                faction   TEXT NOT NULL,
                amount    INTEGER NOT NULL,
                times     INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, npc, item, faction, amount)
            );

            -- How many of an item were offered and how many went across.
            --
            -- Its own table rather than columns on `faction_turnins`: those
            -- rows are per adjustment, and a count repeated across four
            -- adjustment rows would be quadrupled by the first SUM anyone
            -- writes. A hand-in that moved no faction at all is a row here
            -- and nowhere else, which is also the point.
            CREATE TABLE IF NOT EXISTS faction_offers (
                game      TEXT NOT NULL,
                character TEXT NOT NULL,
                server    TEXT NOT NULL,
                source    TEXT NOT NULL,
                npc       TEXT NOT NULL,
                item      TEXT NOT NULL,
                offered   INTEGER NOT NULL,
                handins   INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, npc, item)
            );

            -- How well the attribution did, per log. Kept because it is the
            -- only measure of whether the faction reading is worth trusting:
            -- bursts seen against bursts anything explained.
            CREATE TABLE IF NOT EXISTS faction_bursts (
                game        TEXT NOT NULL,
                character   TEXT NOT NULL,
                server      TEXT NOT NULL,
                source      TEXT NOT NULL,
                seen        INTEGER NOT NULL,
                attributed  INTEGER NOT NULL,
                to_turnins  INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source)
            );

            -- Player characters named by /who.
            --
            -- Here to keep people out of the bestiary; see
            -- `norrlog_core::collect::PlayerCollector`. `level` 0 means
            -- [ANONYMOUS], and `classes` is empty for the same reason -- both
            -- are somebody hiding, not somebody absent.
            CREATE TABLE IF NOT EXISTS players (
                game      TEXT NOT NULL,
                character TEXT NOT NULL,
                server    TEXT NOT NULL,
                source    TEXT NOT NULL,
                name      TEXT NOT NULL,
                level     INTEGER NOT NULL,
                classes   TEXT NOT NULL DEFAULT '',
                race      TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (game, character, server, source, name)
            );

            -- The highest each skill has been seen at.
            --
            -- No `source` column, unlike everything else here, because a
            -- ceiling is not a running total: it is the highest anybody has
            -- reached, and a lower reading is simply an older one. Written
            -- with MAX rather than REPLACE so re-reading a log that stops
            -- earlier cannot lower it.
            CREATE TABLE IF NOT EXISTS skills (
                game      TEXT NOT NULL,
                character TEXT NOT NULL,
                server    TEXT NOT NULL,
                skill     TEXT NOT NULL,
                at_least  INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, skill)
            );

            -- Map markers, from the convention the player types at the spot.
            --
            -- The position is in the key, which is unusual for a float and is
            -- deliberate: two markers of the same name in the same zone are
            -- two markers -- a zone with four guildmasters is four rows --
            -- and the position is the only thing that distinguishes them.
            -- Safe as a key because re-parsing the same line produces the
            -- same bits, so a re-read replaces its own row rather than
            -- doubling it.
            --
            -- **Map space, not /loc space.** See `MarkerCollector` for the
            -- transform and for why distance-to-nearest-map-line is not a
            -- valid test of it.
            CREATE TABLE IF NOT EXISTS markers (
                game       TEXT NOT NULL,
                character  TEXT NOT NULL,
                server     TEXT NOT NULL,
                source     TEXT NOT NULL,
                name       TEXT NOT NULL,
                kind       TEXT NOT NULL,
                role       TEXT NOT NULL DEFAULT '',
                zone       TEXT NOT NULL,
                zone_short TEXT NOT NULL DEFAULT '',
                x          REAL NOT NULL,
                y          REAL NOT NULL,
                z          REAL NOT NULL,
                -- From a consider in the same block, when there was one.
                level      INTEGER,
                PRIMARY KEY (game, character, server, source, zone, name, role, x, y)
            );

            -- Zone name to map shortname, learned from /who.
            --
            -- Nothing in the client states this pairing, and a shortname is
            -- what a map file is called. Not keyed on a log: it is a fact
            -- about the world rather than an observation, so a later reading
            -- replaces an earlier one.
            CREATE TABLE IF NOT EXISTS map_zones (
                game       TEXT NOT NULL,
                zone       TEXT NOT NULL,
                zone_short TEXT NOT NULL,
                PRIMARY KEY (game, zone)
            );

            -- Where a creature stood when it died, from a /loc taken over the
            -- corpse. Keyed on the death time, which is what makes it one
            -- observation. `gap` is the seconds between the death and the
            -- reading and is the whole reliability of the row -- carried
            -- rather than thresholded, so the reader decides.
            CREATE TABLE IF NOT EXISTS spots (
                game       TEXT NOT NULL,
                character  TEXT NOT NULL,
                server     TEXT NOT NULL,
                source     TEXT NOT NULL,
                name       TEXT NOT NULL,
                zone       TEXT NOT NULL,
                zone_short TEXT NOT NULL DEFAULT '',
                x          REAL NOT NULL,
                y          REAL NOT NULL,
                z          REAL NOT NULL,
                at         INTEGER NOT NULL,
                gap        INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, name, at)
            );
            ",
        )?;
        self.retag_live_record()?;
        Ok(())
    }

    /// Move live EverQuest's record rows off the Legends tag.
    ///
    /// Until live EverQuest was folded in, every record row was tagged `eql`
    /// whatever game its log came from (see `record::tag`). A row whose
    /// source is a log under a live EverQuest install is that game's, and is
    /// moved to `eq`; that is read from the path, not guessed. Rows from a
    /// path that names neither game stay where they are, as do the two
    /// tables with no source (`skills`, `map_zones`), which cannot be
    /// attributed. Cheap and idempotent, so it runs on every open.
    fn retag_live_record(&self) -> Result<()> {
        const SOURCED: &[&str] = &[
            "considers",
            "faction_bursts",
            "faction_hits",
            "faction_offers",
            "faction_turnins",
            "forage",
            "markers",
            "players",
            "spots",
        ];
        let (from, to) = (
            crate::record::tag(Game::EverQuestLegends),
            crate::record::tag(Game::EverQuest),
        );
        for table in SOURCED {
            let sources: Vec<String> = self
                .conn
                .prepare(&format!(
                    "SELECT DISTINCT source FROM {table} WHERE game = ?1"
                ))?
                .query_map(params![from], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for source in sources {
                if Game::from_path(Path::new(&source)) == Game::EverQuest {
                    self.conn.execute(
                        &format!("UPDATE OR REPLACE {table} SET game = ?1 WHERE game = ?2 AND source = ?3"),
                        params![to, from, source],
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Record boss kills. Existing rows are replaced, so a re-parse that now
    /// knows more (a build label, a matched encounter) improves them.
    pub fn put_kills(&mut self, kills: &[StoredKill]) -> Result<usize> {
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO boss_kills
                 (character, server, timestamp, boss, difficulty, zone, solo, build, duration, your_damage, your_dps)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?;
            for k in kills {
                stmt.execute(params![
                    c,
                    sv,
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
        insert_encounters(&tx, &self.character, &self.server, encs)?;
        tx.commit()?;
        Ok(encs.len())
    }

    /// Store a log's fights read from its start, replacing what is on file
    /// for the span they cover.
    ///
    /// A whole read is every fight the log holds, so anything else stored in
    /// that span for this character came from an older reading of the same
    /// lines: a fight since dropped (other people's damage you were not in),
    /// or one since renamed (titled after you, or "Unknown encounter"), which
    /// the title in the key left behind as a twin. A partial read proves
    /// nothing about what it skipped, and goes through `put_encounters`.
    pub fn replace_encounters(&mut self, encs: &[StoredEncounter]) -> Result<usize> {
        let (Some(lo), Some(hi)) = (
            encs.iter().map(|e| e.start).min(),
            encs.iter().map(|e| e.start).max(),
        ) else {
            return Ok(0);
        };
        // One transaction for the delete and the inserts: a failed insert
        // used to leave the old fights already deleted, and a log gone by
        // the retry could not bring them back.
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM encounters
             WHERE character = ?1 AND server = ?2 AND start >= ?3 AND start <= ?4",
            params![self.character, self.server, lo, hi],
        )?;
        insert_encounters(&tx, &self.character, &self.server, encs)?;
        tx.commit()?;
        Ok(encs.len())
    }

    pub fn put_levels(&mut self, levels: &[StoredLevel]) -> Result<usize> {
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO levels
                 (character, server, level, started, ended, seconds, kills, percent, build, zones)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            )?;
            for l in levels {
                stmt.execute(params![
                    c, sv, l.level, l.started, l.ended, l.seconds, l.kills, l.percent, l.build,
                    l.zones
                ])?;
            }
        }
        tx.commit()?;
        Ok(levels.len())
    }

    /// Record bought AA ranks, for one game.
    ///
    /// The game is passed in like it is for drops and vendors, and for the
    /// same reason: this table is shared by every log the daemon has ever
    /// been pointed at. Without it, a Legends character's 153 purchases were
    /// indistinguishable from an EverQuest character's 30 and got exported
    /// as one pile -- 69 abilities reached an EverQuest site that no
    /// EverQuest client has ever listed.
    pub fn put_aa(&mut self, game: &str, purchases: &[(String, u32, u32, i64)]) -> Result<usize> {
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        {
            // Toggle abilities used to be stored as the client's two halves —
            // "Symphonic Aura: Enabled" beside "Symphonic Aura: Disabled" —
            // and are now folded under the base name. The old rows are keyed by
            // that old name, so nothing overwrites them and they would sit here
            // forever, presenting one ability as three to anything reading this
            // table. Safe to delete outright: every row is re-derived from the
            // log on each parse, so anything still real comes straight back.
            tx.execute(
                "DELETE FROM aa_purchases
                 WHERE character = ?1 AND server = ?2
                   AND (ability LIKE '%: Enabled' OR ability LIKE '%: Disabled')",
                params![c, sv],
            )?;
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO aa_purchases
                 (game, character, server, ability, rank, spent, at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
            )?;
            for (ability, rank, spent, at) in purchases {
                stmt.execute(params![game, c, sv, ability, rank, spent, at])?;
            }
        }
        tx.commit()?;
        Ok(purchases.len())
    }

    /// Record granted spells. Existing (spell, time) pairs are left alone.
    pub fn put_grants(&mut self, rows: &[StoredGrant]) -> Result<usize> {
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO spell_grants
                     (character, server, spell, level, at, build)
                 VALUES (?1,?2,?3,?4,?5,?6)",
            )?;
            for g in rows {
                stmt.execute(params![c, sv, g.spell, g.level, g.at, g.build])?;
            }
        }
        tx.commit()?;
        Ok(rows.len())
    }

    /// Record Sky turn-ins. Existing (quest, time) pairs are left alone.
    pub fn put_sky_turnins(&mut self, rows: &[(String, i64)]) -> Result<usize> {
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO sky_turnins (character, server, quest, at)
                 VALUES (?1,?2,?3,?4)",
            )?;
            for (quest, at) in rows {
                stmt.execute(params![c, sv, quest, at])?;
            }
        }
        tx.commit()?;
        Ok(rows.len())
    }

    /// Turn-ins per quest, as (count, most recent), across every log ever
    /// parsed.
    pub fn sky_turnin_counts(&self) -> Result<Vec<(String, u32, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT quest, COUNT(*), MAX(at) FROM sky_turnins
             WHERE character = ?1 AND server = ?2 GROUP BY quest",
        )?;
        let rows = stmt
            .query_map(params![self.character, self.server], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Record a level the player stated for one class.
    pub fn put_class_level(&mut self, class: &str, level: u32, at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO class_levels (character, server, class, level, at)
             VALUES (?1,?2,?3,?4,?5)",
            params![self.character, self.server, class, level, at],
        )?;
        Ok(())
    }

    /// Stated class levels, as (class abbreviation, level, when).
    pub fn class_levels(&self) -> Result<Vec<(String, u32, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT class, level, at FROM class_levels
             WHERE character = ?1 AND server = ?2",
        )?;
        let rows = stmt
            .query_map(params![self.character, self.server], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Write an alert rule, replacing any existing rule of the same id.
    pub fn put_alert(&mut self, rule: &norrlog_core::alert::AlertRule, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO alerts (id, data, updated) VALUES (?1,?2,?3)",
            params![rule.id, serde_json::to_string(rule)?, now],
        )?;
        Ok(())
    }

    /// Every alert rule, oldest first so the list keeps the order they were
    /// written in rather than reshuffling on each load.
    pub fn alerts(&self) -> Result<Vec<norrlog_core::alert::AlertRule>> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM alerts ORDER BY updated ASC, id ASC")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .iter()
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect())
    }

    pub fn delete_alert(&mut self, id: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM alerts WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Record what this log has seen dropping, and the kills behind it.
    ///
    /// Only ever called with the whole log read, so the report is
    /// everything that log says: its rows are cleared and rewritten, and
    /// re-parsing changes nothing. A different log adds to the pile rather
    /// than overwriting it.
    pub fn put_drops(
        &mut self,
        game: &str,
        source: &str,
        report: &norrlog_core::drops::DropReport,
    ) -> Result<usize> {
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            // Cleared per source first. The rows are keyed on how the item
            // and creature are spelled, and a fold that changes the spelling
            // -- "lizardman broodling" becoming "a lizardman broodling", a
            // graded name folding to its base -- would otherwise leave the old
            // row beside the new one, counting the same loots twice. Tharn's
            // log held 35,965 loots on file against 22,817 lines.
            for table in ["item_drops", "item_drop_places", "npc_kills"] {
                tx.execute(
                    &format!(
                        "DELETE FROM {table}
                         WHERE game = ?1 AND character = ?2 AND server = ?3 AND source = ?4"
                    ),
                    params![game, c, sv, source],
                )?;
            }
            let mut drop_stmt = tx.prepare(
                "INSERT OR REPLACE INTO item_drops
                 (game, character, server, source, item, npc, zones, looted, left_behind,
                  quantity, first_seen, last_seen)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            )?;
            for item in &report.items {
                for s in &item.sources {
                    drop_stmt.execute(params![
                        game,
                        c,
                        sv,
                        source,
                        item.item,
                        s.npc,
                        s.zones.join("|"),
                        s.looted as i64,
                        s.left as i64,
                        s.quantity as i64,
                        s.first_seen,
                        s.last_seen,
                    ])?;
                    n += 1;
                }
            }
            let mut place_stmt = tx.prepare(
                "INSERT OR REPLACE INTO item_drop_places
                 (game, character, server, source, item, npc, zone, mode,
                  difficulty, looted, quantity)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?;
            for item in &report.items {
                for s in &item.sources {
                    for pl in &s.places {
                        place_stmt.execute(params![
                            game,
                            c,
                            sv,
                            source,
                            item.item,
                            s.npc,
                            pl.zone,
                            pl.mode,
                            pl.difficulty as i64,
                            pl.looted as i64,
                            pl.quantity as i64,
                        ])?;
                    }
                }
            }

            let mut kill_stmt = tx.prepare(
                "INSERT OR REPLACE INTO npc_kills
                 (game, character, server, source, npc, kills) VALUES (?1,?2,?3,?4,?5,?6)",
            )?;
            for (npc, kills) in &report.kills {
                kill_stmt.execute(params![game, c, sv, source, npc, *kills as i64])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Write the record — considers, faction, players, skills, markers and
    /// spots — for one log.
    ///
    /// Every row carries `source`, so this replaces this log's own rows and
    /// leaves every other log's alone. Safe to call as often as the history
    /// flush likes.
    pub fn put_record(
        &mut self,
        game: Game,
        source: &str,
        r: &crate::record::Settled,
    ) -> Result<usize> {
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tag = crate::record::tag(game);
        let tx = self.conn.transaction()?;
        let mut n = 0usize;
        {
            // Considers. A creature with no levels at all still gets a row,
            // at level -1 and with `times` 0, so its rare and raid marks
            // survive. See the schema for why -1 rather than 0, and for why
            // this path does not fire against today's grammar. `times` is
            // zero because the collector's own count is the sum over levels,
            // so a placeholder carrying a count would inflate every total.
            tx.execute(
                "DELETE FROM considers WHERE game=?1 AND character=?2 AND server=?3 AND source=?4",
                params![tag, c, sv, source],
            )?;
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO considers
                 (game, character, server, source, creature, level, times, rare, raid)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            )?;
            for con in r.consider.seen().values() {
                if con.levels.is_empty() {
                    if con.rare || con.raid {
                        stmt.execute(params![
                            tag,
                            c,
                            sv,
                            source,
                            con.name,
                            -1i64,
                            0i64,
                            con.rare as i64,
                            con.raid as i64
                        ])?;
                        n += 1;
                    }
                    continue;
                }
                for (level, times) in &con.levels {
                    stmt.execute(params![
                        tag,
                        c,
                        sv,
                        source,
                        con.name,
                        *level as i64,
                        *times as i64,
                        con.rare as i64,
                        con.raid as i64
                    ])?;
                    n += 1;
                }
            }
        }
        {
            // Forage. The empty-item row carries the failures — see the
            // schema. Written even when a zone yielded nothing at all, which
            // is the only way "foraged here, got nothing" is recorded.
            tx.execute(
                "DELETE FROM forage WHERE game=?1 AND character=?2 AND server=?3 AND source=?4",
                params![tag, c, sv, source],
            )?;
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO forage
                 (game, character, server, source, zone, item, times, first_seen, last_seen)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            )?;
            for (zone, z) in r.forage.zones() {
                for (item, times) in &z.found {
                    stmt.execute(params![
                        tag,
                        c,
                        sv,
                        source,
                        zone,
                        item,
                        *times as i64,
                        z.first,
                        z.last
                    ])?;
                    n += 1;
                }
                if z.empty > 0 {
                    stmt.execute(params![
                        tag,
                        c,
                        sv,
                        source,
                        zone,
                        "",
                        z.empty as i64,
                        z.first,
                        z.last
                    ])?;
                    n += 1;
                }
            }
        }
        {
            // Faction. Three tables; see the schema for why the offer counts
            // are not columns on the adjustments.
            for t in [
                "faction_hits",
                "faction_turnins",
                "faction_offers",
                "faction_bursts",
            ] {
                tx.execute(
                    &format!(
                        "DELETE FROM {t} WHERE game=?1 AND character=?2 AND server=?3 AND source=?4"
                    ),
                    params![tag, c, sv, source],
                )?;
            }
            let f = &r.faction;
            let mut hit = tx.prepare(
                "INSERT OR REPLACE INTO faction_hits
                 (game, character, server, source, creature, zone, faction, amount, times)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            )?;
            let mut turn = tx.prepare(
                "INSERT OR REPLACE INTO faction_turnins
                 (game, character, server, source, npc, item, faction, amount, times)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            )?;
            for ((cause, faction), moves) in f.moves() {
                for (amount, times) in &moves.amounts {
                    match cause {
                        norrlog_core::collect::FactionCause::Kill { creature, zone } => {
                            hit.execute(params![
                                tag,
                                c,
                                sv,
                                source,
                                f.creature_display(creature).unwrap_or(creature),
                                f.zone_display(zone).unwrap_or(zone),
                                faction,
                                *amount,
                                *times as i64
                            ])?;
                        }
                        norrlog_core::collect::FactionCause::TurnIn { npc, item } => {
                            turn.execute(params![
                                tag,
                                c,
                                sv,
                                source,
                                npc,
                                item,
                                faction,
                                *amount,
                                *times as i64
                            ])?;
                        }
                    }
                    n += 1;
                }
            }
            let mut offer = tx.prepare(
                "INSERT OR REPLACE INTO faction_offers
                 (game, character, server, source, npc, item, offered, handins)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )?;
            let mut pairs: std::collections::BTreeSet<(String, String)> =
                f.offered.keys().cloned().collect();
            pairs.extend(f.handins.keys().cloned());
            for (npc, item) in pairs {
                offer.execute(params![
                    tag,
                    c,
                    sv,
                    source,
                    npc,
                    item,
                    f.offered
                        .get(&(npc.clone(), item.clone()))
                        .copied()
                        .unwrap_or(0) as i64,
                    f.handins
                        .get(&(npc.clone(), item.clone()))
                        .copied()
                        .unwrap_or(0) as i64
                ])?;
                n += 1;
            }
            tx.execute(
                "INSERT OR REPLACE INTO faction_bursts
                 (game, character, server, source, seen, attributed, to_turnins)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    tag,
                    c,
                    sv,
                    source,
                    f.bursts as i64,
                    f.to_kill as i64,
                    f.to_turnin as i64
                ],
            )?;
        }
        {
            tx.execute(
                "DELETE FROM players WHERE game=?1 AND character=?2 AND server=?3 AND source=?4",
                params![tag, c, sv, source],
            )?;
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO players
                 (game, character, server, source, name, level, classes, race)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )?;
            for p in r.players.seen().values() {
                stmt.execute(params![
                    tag,
                    c,
                    sv,
                    source,
                    p.name,
                    p.level as i64,
                    p.classes.join("|"),
                    p.race
                ])?;
                n += 1;
            }
        }
        {
            // MAX, not REPLACE: a skill only goes up, so a lower reading is an
            // older one. This is also why there is no DELETE above it -- a
            // ceiling is not a per-log running total.
            let mut stmt = tx.prepare(
                "INSERT INTO skills (game, character, server, skill, at_least)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT (game, character, server, skill)
                 DO UPDATE SET at_least = MAX(at_least, excluded.at_least)",
            )?;
            for (skill, at_least) in r.skills.best() {
                stmt.execute(params![tag, c, sv, skill, *at_least as i64])?;
                n += 1;
            }
        }
        {
            tx.execute(
                "DELETE FROM markers WHERE game=?1 AND character=?2 AND server=?3 AND source=?4",
                params![tag, c, sv, source],
            )?;
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO markers
                 (game, character, server, source, name, kind, role, zone, zone_short,
                  x, y, z, level)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            )?;
            for m in r.markers.spawn_points(crate::record::SPAWN_RADIUS) {
                stmt.execute(params![
                    tag,
                    c,
                    sv,
                    source,
                    m.name,
                    crate::record::marker_kind(m.kind),
                    m.role,
                    m.zone,
                    m.zone_short,
                    m.x,
                    m.y,
                    m.z,
                    m.level.map(|l| l as i64)
                ])?;
                n += 1;
            }
            let mut zone = tx.prepare(
                "INSERT OR REPLACE INTO map_zones (game, zone, zone_short) VALUES (?1,?2,?3)",
            )?;
            for (z, short) in r.markers.zones() {
                zone.execute(params![tag, z, short])?;
                n += 1;
            }
        }
        {
            tx.execute(
                "DELETE FROM spots WHERE game=?1 AND character=?2 AND server=?3 AND source=?4",
                params![tag, c, sv, source],
            )?;
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO spots
                 (game, character, server, source, name, zone, zone_short, x, y, z, at, gap)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            )?;
            for sp in r.spots.spots() {
                stmt.execute(params![
                    tag,
                    c,
                    sv,
                    source,
                    sp.name,
                    sp.zone,
                    sp.zone_short,
                    sp.x,
                    sp.y,
                    sp.z,
                    sp.at,
                    sp.gap
                ])?;
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// The whole record on file, merged across every log and character.
    ///
    /// This is what a bundle is built from. The per-log reports the engine
    /// serves are one log's reading; this is the accumulation, which is the
    /// only thing that can honestly be sent — the daemon follows one log at a
    /// time and the site is owed everything ever seen.
    ///
    /// `source` on each report reads `history` rather than a log's stem, for
    /// the same reason: there is no one log this came from.
    pub fn record_reports(&self, game: Game) -> Result<crate::record::Reports> {
        let tag = crate::record::tag(game);
        use crate::record::*;
        const SOURCE: &str = "history";

        // Forage, pooled across every log. The empty-item row is the
        // failures; see the schema.
        let mut forage_zones: Vec<ForageZoneRow> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT zone, item, SUM(times), MIN(NULLIF(first_seen,0)), MAX(last_seen)
                 FROM forage WHERE game = ?1
                 GROUP BY zone, item ORDER BY zone, item",
            )?;
            let rows = stmt
                .query_map(params![tag], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        r.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (zone, item, times, first, last) in rows {
                let row = match forage_zones.last_mut() {
                    Some(r) if r.zone == zone => r,
                    _ => {
                        forage_zones.push(ForageZoneRow {
                            zone: zone.clone(),
                            game: tag.into(),
                            attempts: 0,
                            hits: 0,
                            empty: 0,
                            first_seen: first,
                            last_seen: last,
                            items: Vec::new(),
                        });
                        forage_zones.last_mut().expect("just pushed")
                    }
                };
                if item.is_empty() {
                    row.empty += times as u64;
                } else {
                    row.hits += times as u64;
                    row.items.push(ForagedItem {
                        item,
                        times: times as u64,
                    });
                }
                row.attempts = row.hits + row.empty;
                if first != 0 && (row.first_seen == 0 || first < row.first_seen) {
                    row.first_seen = first;
                }
                row.last_seen = row.last_seen.max(last);
            }
        }

        let mut considers: Vec<ConsiderRow> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT creature, level, SUM(times), MAX(rare), MAX(raid)
                 FROM considers WHERE game = ?1
                 GROUP BY creature, level ORDER BY creature, level",
            )?;
            let rows = stmt
                .query_map(params![tag], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)? != 0,
                        r.get::<_, i64>(4)? != 0,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (creature, level, times, rare, raid) in rows {
                let row = match considers.last_mut() {
                    Some(r) if r.creature == creature => r,
                    _ => {
                        considers.push(ConsiderRow {
                            creature,
                            game: tag,
                            levels: Vec::new(),
                            rare: false,
                            raid: false,
                            considers: 0,
                        });
                        considers.last_mut().expect("just pushed")
                    }
                };
                row.rare |= rare;
                row.raid |= raid;
                // Minus one is the placeholder for a creature with no levels
                // at all; it carries the marks above and nothing else. Every
                // value the column can actually hold is a real level. See
                // `put_record`.
                if level >= 0 {
                    row.levels.push(LevelSeen {
                        level: level as u32,
                        times: times as u64,
                    });
                    row.considers += times as u64;
                }
            }
        }

        let mut hits: Vec<FactionHit> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT creature, zone, faction, amount, SUM(times)
                 FROM faction_hits WHERE game = ?1
                 GROUP BY creature, zone, faction, amount
                 ORDER BY creature, zone, faction, amount",
            )?;
            let rows = stmt
                .query_map(params![tag], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (creature, zone, faction, amount, times) in rows {
                let row = match hits.last_mut() {
                    Some(h) if h.creature == creature && h.zone == zone && h.faction == faction => {
                        h
                    }
                    _ => {
                        hits.push(FactionHit {
                            creature,
                            zone,
                            faction,
                            adjustments: Vec::new(),
                            kills: 0,
                            game: tag,
                        });
                        hits.last_mut().expect("just pushed")
                    }
                };
                row.adjustments.push(Adjustment {
                    amount,
                    times: times as u64,
                });
                row.kills += times as u64;
            }
        }

        let mut turnins: Vec<FactionTurnIn> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT npc, item, faction, amount, SUM(times)
                 FROM faction_turnins WHERE game = ?1
                 GROUP BY npc, item, faction, amount
                 ORDER BY npc, item, faction, amount",
            )?;
            let rows = stmt
                .query_map(params![tag], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (npc, item, faction, amount, times) in rows {
                let row = match turnins.last_mut() {
                    Some(t) if t.npc == npc && t.item == item && t.faction == faction => t,
                    _ => {
                        turnins.push(FactionTurnIn {
                            npc,
                            item,
                            faction,
                            adjustments: Vec::new(),
                            offered: 0,
                            handins: 0,
                            game: tag,
                        });
                        turnins.last_mut().expect("just pushed")
                    }
                };
                row.adjustments.push(Adjustment {
                    amount,
                    times: times as u64,
                });
            }
            let mut stmt = self.conn.prepare(
                "SELECT npc, item, SUM(offered), SUM(handins)
                 FROM faction_offers WHERE game = ?1 GROUP BY npc, item",
            )?;
            let counts = stmt
                .query_map(params![tag], |r| {
                    Ok((
                        (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                        (r.get::<_, i64>(2)?, r.get::<_, i64>(3)?),
                    ))
                })?
                .collect::<rusqlite::Result<std::collections::BTreeMap<_, _>>>()?;
            for t in &mut turnins {
                if let Some((offered, handins)) = counts.get(&(t.npc.clone(), t.item.clone())) {
                    t.offered = *offered as u64;
                    t.handins = *handins as u64;
                }
            }
        }

        let (bursts_seen, bursts_attributed, bursts_to_turnins) = self.conn.query_row(
            "SELECT COALESCE(SUM(seen),0), COALESCE(SUM(attributed),0),
                    COALESCE(SUM(to_turnins),0) FROM faction_bursts WHERE game = ?1",
            params![tag],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )?;

        let players = {
            // The highest reading of each player wins. A groupmate seen at 51
            // in one log and 52 in another is one person who levelled, and the
            // later reading is the true one; [ANONYMOUS] reads as 0 and so
            // never displaces a stated level.
            let mut stmt = self.conn.prepare(
                "SELECT name, level, classes, race FROM players WHERE game = ?1
                 ORDER BY name, level",
            )?;
            let rows = stmt
                .query_map(params![tag], |r| {
                    Ok(PlayerRow {
                        name: r.get(0)?,
                        level: r.get::<_, i64>(1)? as u32,
                        classes: split_packed(&r.get::<_, String>(2)?),
                        race: r.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut best: std::collections::BTreeMap<String, PlayerRow> =
                std::collections::BTreeMap::new();
            for row in rows {
                match best.get_mut(&row.name) {
                    Some(prev) => {
                        if row.level >= prev.level {
                            *prev = row;
                        }
                    }
                    None => {
                        best.insert(row.name.clone(), row);
                    }
                }
            }
            best.into_values().collect()
        };

        let skills = {
            let mut stmt = self.conn.prepare(
                "SELECT skill, MAX(at_least) FROM skills WHERE game = ?1
                 GROUP BY skill ORDER BY skill",
            )?;
            let rows = stmt
                .query_map(params![tag], |r| {
                    Ok(SkillRow {
                        skill: r.get(0)?,
                        at_least: r.get::<_, i64>(1)? as u32,
                        game: tag,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };

        let markers = {
            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT name, kind, role, zone, zone_short, x, y, z, level
                 FROM markers WHERE game = ?1 ORDER BY zone, name, x, y",
            )?;
            let rows = stmt
                .query_map(params![tag], |r| {
                    Ok(MarkerRow {
                        name: r.get(0)?,
                        kind: crate::record::kind_of(&r.get::<_, String>(1)?),
                        role: r.get(2)?,
                        zone: r.get(3)?,
                        zone_short: r.get(4)?,
                        x: r.get(5)?,
                        y: r.get(6)?,
                        z: r.get(7)?,
                        level: r.get::<_, Option<i64>>(8)?.map(|l| l as u32),
                        game: tag,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };

        let zones = {
            let mut stmt = self
                .conn
                .prepare("SELECT zone, zone_short FROM map_zones WHERE game = ?1 ORDER BY zone")?;
            let rows = stmt
                .query_map(params![tag], |r| {
                    Ok(ZoneRow {
                        zone: r.get(0)?,
                        zone_short: r.get(1)?,
                        game: tag,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };

        let spots = {
            let mut stmt = self.conn.prepare(
                "SELECT name, zone, zone_short, x, y, z, at, gap FROM spots
                 WHERE game = ?1 ORDER BY name, at",
            )?;
            let rows = stmt
                .query_map(params![tag], |r| {
                    Ok(SpotRow {
                        name: r.get(0)?,
                        zone: r.get(1)?,
                        zone_short: r.get(2)?,
                        x: r.get(3)?,
                        y: r.get(4)?,
                        z: r.get(5)?,
                        at: r.get(6)?,
                        gap: r.get(7)?,
                        game: tag,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };

        Ok(Reports {
            players: PlayersReport {
                note: PLAYERS_NOTE,
                source: SOURCE.to_string(),
                game: tag,
                players,
            },
            forage: ForageReport {
                note: FORAGE_NOTE.into(),
                source: SOURCE.to_string(),
                zones: forage_zones,
                // The history has no record of an attempt it could not place,
                // because an unplaced one was never written: there is no zone
                // to key it on.
                unplaced: 0,
            },
            consider: ConsiderReport {
                note: CONSIDER_NOTE,
                source: SOURCE.to_string(),
                considers,
            },
            faction: FactionReport {
                note: FACTION_NOTE,
                source: SOURCE.to_string(),
                bursts_seen: bursts_seen as u64,
                bursts_attributed: bursts_attributed as u64,
                bursts_to_turnins: bursts_to_turnins as u64,
                hits,
                turnins,
            },
            spots: SpotsReport {
                note: SPOTS_NOTE,
                source: SOURCE.to_string(),
                spots,
            },
            carto: CartoReport {
                note: CARTO_NOTE,
                source: SOURCE.to_string(),
                zones,
                markers,
            },
            skills: SkillsReport {
                note: SKILLS_NOTE,
                source: "logs",
                game: tag,
                skills,
            },
        })
    }

    /// Every drop observation on file, summed across logs and characters.
    ///
    /// The game stays in the key: totals are per game, never pooled across
    /// the two.
    pub fn all_drops(&self) -> Result<Vec<StoredDrop>> {
        let mut stmt = self.conn.prepare(
            "SELECT game, item, npc, group_concat(zones, '|'),
                    SUM(looted), SUM(left_behind), SUM(quantity),
                    MIN(first_seen), MAX(last_seen)
             FROM item_drops GROUP BY game, lower(item), lower(npc)
             ORDER BY SUM(looted) + SUM(left_behind) DESC",
        )?;
        let rows: Vec<StoredDrop> = stmt
            .query_map([], |r| {
                let zones: Option<String> = r.get(3)?;
                let mut zones: Vec<String> = zones
                    .unwrap_or_default()
                    .split('|')
                    .filter(|z| !z.is_empty())
                    .map(str::to_string)
                    .collect();
                zones.sort();
                zones.dedup();
                Ok(StoredDrop {
                    game: r.get(0)?,
                    item: r.get(1)?,
                    npc: r.get(2)?,
                    zones,
                    looted: r.get::<_, i64>(4)? as u64,
                    left: r.get::<_, i64>(5)? as u64,
                    quantity: r.get::<_, i64>(6)? as u64,
                    first_seen: r.get(7)?,
                    last_seen: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // Drops are keyed on the ungraded name now, but rows written before
        // that are still on file under "Woven Skull Cap +1" and SQL groups
        // them apart. Folding on the way out as well as on the way in means
        // the history reads correctly without rewriting it: every stored
        // observation stays exactly as it was recorded.
        let mut folded: std::collections::HashMap<(String, String, String), usize> =
            std::collections::HashMap::new();
        let mut out: Vec<StoredDrop> = Vec::new();
        for row in rows {
            let base = norrlog_core::drops::canonical_item(&row.item).to_string();
            let key = (
                row.game.clone(),
                base.to_lowercase(),
                row.npc.to_lowercase(),
            );
            match folded.get(&key) {
                Some(&idx) => {
                    let e = &mut out[idx];
                    e.zones.extend(row.zones);
                    e.zones.sort();
                    e.zones.dedup();
                    e.looted += row.looted;
                    e.left += row.left;
                    e.quantity += row.quantity;
                    e.first_seen = e.first_seen.min(row.first_seen);
                    e.last_seen = e.last_seen.max(row.last_seen);
                }
                None => {
                    folded.insert(key, out.len());
                    out.push(StoredDrop { item: base, ..row });
                }
            }
        }
        out.sort_by(|a, b| {
            (b.looted + b.left)
                .cmp(&(a.looted + a.left))
                .then(a.item.cmp(&b.item))
                .then(a.npc.cmp(&b.npc))
        });
        Ok(out)
    }

    /// Where every drop was looted, folded the same way `all_drops` folds.
    ///
    /// Keyed on the *base* item name and the creature, both lowercased, so a
    /// caller that has an `all_drops` row can look its breakdown up directly.
    /// The fold has to happen here for the same reason it happens there: rows
    /// written before drops were keyed on the ungraded name are still on file
    /// under `Woven Skull Cap +1`, and SQL groups them apart from the base.
    /// A breakdown that folded differently from the row it belongs to would
    /// report places summing to more loots than the drop records.
    #[allow(clippy::type_complexity)]
    pub fn all_drop_places(
        &self,
    ) -> Result<
        std::collections::HashMap<(String, String, String), Vec<(String, String, u8, u64, u64)>>,
    > {
        let mut stmt = self.conn.prepare(
            "SELECT game, item, npc, zone, mode, difficulty,
                    SUM(looted), SUM(quantity)
             FROM item_drop_places
             GROUP BY game, lower(item), lower(npc), zone, mode, difficulty",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)? as u8,
                r.get::<_, i64>(6)? as u64,
                r.get::<_, i64>(7)? as u64,
            ))
        })?;
        // Two folds, not one: the graded-name fold above, and then places
        // that were separate rows only because they came off different
        // characters or logs. Summed on the way out, as the parent row is.
        let mut out: std::collections::HashMap<
            (String, String, String),
            std::collections::HashMap<(String, String, u8), (u64, u64)>,
        > = std::collections::HashMap::new();
        for row in rows {
            let (game, item, npc, zone, mode, difficulty, looted, quantity) = row?;
            let base = norrlog_core::drops::canonical_item(&item).to_lowercase();
            let e = out
                .entry((game, base, npc.to_lowercase()))
                .or_default()
                .entry((zone, mode, difficulty))
                .or_insert((0, 0));
            e.0 += looted;
            e.1 += quantity;
        }
        Ok(out
            .into_iter()
            .map(|(k, places)| {
                let mut v: Vec<(String, String, u8, u64, u64)> = places
                    .into_iter()
                    .map(|((zone, mode, difficulty), (looted, quantity))| {
                        (zone, mode, difficulty, looted, quantity)
                    })
                    .collect();
                // Biggest first, so a page showing two of five shows the two
                // that account for most of the drops.
                v.sort_by(|a, b| b.3.cmp(&a.3).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
                (k, v)
            })
            .collect())
    }

    /// What level to assume for a log that has not said yet.
    ///
    /// Nothing in a log states your level, so a file that opens mid-session
    /// leaves the bestiary at zero until the next level-up — hours of swings
    /// filed against an "unknown level" pairing that no later log will rejoin.
    /// Three sources answer it, and the largest wins because levels only go
    /// up:
    ///
    /// * `npc_melee` — the level the bestiary itself last filed rows under.
    ///   The most direct evidence there is: not an inference about the
    ///   character, but a record of what this exact code last believed.
    /// * `levels` **plus one** — those rows are *completed* level spans, so
    ///   the highest one finished means the next was reached. The `+1` is why
    ///   this cannot be read off directly; a store written before the bestiary
    ///   existed has nothing else to offer.
    /// * `class_levels` — what the player entered by hand, and the only
    ///   evidence at all for a character who capped before any of this was
    ///   recorded.
    ///
    /// Returns 0 when nothing is known, which the caller must treat as "leave
    /// it alone" rather than as level zero.
    pub fn last_known_level(&self) -> Result<u32> {
        let filed: Option<u32> = self.conn.query_row(
            "SELECT MAX(level) FROM npc_melee WHERE character = ?1 AND server = ?2",
            params![self.character, self.server],
            |r| r.get(0),
        )?;
        let completed: Option<u32> = self.conn.query_row(
            "SELECT MAX(level) FROM levels WHERE character = ?1 AND server = ?2",
            params![self.character, self.server],
            |r| r.get(0),
        )?;
        let stated: Option<u32> = self.conn.query_row(
            "SELECT MAX(level) FROM class_levels WHERE character = ?1 AND server = ?2",
            params![self.character, self.server],
            |r| r.get(0),
        )?;
        Ok(filed
            .unwrap_or(0)
            .max(completed.map(|l| l + 1).unwrap_or(0))
            .max(stated.unwrap_or(0)))
    }

    /// Record the bestiary this log produced: profiles, melee per pairing, and
    /// every health sample.
    ///
    /// Same keying as [`Store::put_drops`] — a re-parse replaces this log's
    /// rows and nothing else. The health samples are written in full rather
    /// than summarised, because [`norrlog_core::bestiary::merge`] has to
    /// recompute the spread over every log at once and cannot do that from
    /// per-log medians.
    pub fn put_bestiary(
        &mut self,
        game: &str,
        source: &str,
        report: &norrlog_core::bestiary::BestiaryReport,
        health: &[norrlog_core::bestiary::HealthSample],
    ) -> Result<usize> {
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            // ON CONFLICT DO UPDATE, not INSERT OR REPLACE: see the note on
            // `put_vendors` for the column-erasing hazard that distinction
            // avoids. `kind` is exactly the kind of column it bites — it is
            // written only when a body-restricted spell happened to land, so a
            // writer that reverted it would undo evidence that may not come
            // round again for weeks.
            //
            // Which is also why `kind` is the one column that will not go back
            // to empty: a re-parse of a log whose casts have scrolled out of
            // range must not erase what an earlier parse proved.
            let mut npc_stmt = tx.prepare(
                "INSERT INTO npc_profiles
                 (game, character, server, source, zone, mode, difficulty, npc,
                  kills, deaths_to_it, max_hit, first_seen, last_seen, kind)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
                 ON CONFLICT(game, character, server, source, zone, mode, difficulty, npc)
                 DO UPDATE SET
                   kills        = excluded.kills,
                   deaths_to_it = excluded.deaths_to_it,
                   max_hit      = excluded.max_hit,
                   first_seen   = excluded.first_seen,
                   last_seen    = excluded.last_seen,
                   kind         = CASE WHEN excluded.kind <> '' THEN excluded.kind
                                       ELSE npc_profiles.kind END",
            )?;
            let mut melee_stmt = tx.prepare(
                "INSERT OR REPLACE INTO npc_melee
                 (game, character, server, source, zone, mode, difficulty, npc,
                  level, stance, swings, hits, misses, avoided, damage, max_hit)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            )?;
            let mut cast_stmt = tx.prepare(
                "INSERT OR REPLACE INTO npc_casts
                 (game, character, server, source, zone, mode, difficulty, npc, spell,
                  casts, hits, damage_total, damage_min, damage_max, element, ticks,
                  tick_damage)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            )?;
            for p in &report.npcs {
                for k in &p.casts {
                    cast_stmt.execute(params![
                        game,
                        c,
                        sv,
                        source,
                        p.zone,
                        p.mode.as_str(),
                        p.difficulty,
                        p.name,
                        k.spell,
                        k.casts as i64,
                        k.hits as i64,
                        k.damage_total as i64,
                        k.damage_min as i64,
                        k.damage_max as i64,
                        k.element.as_deref().unwrap_or(""),
                        k.ticks as i64,
                        k.tick_damage as i64,
                    ])?;
                }
                npc_stmt.execute(params![
                    game,
                    c,
                    sv,
                    source,
                    p.zone,
                    p.mode.as_str(),
                    p.difficulty,
                    p.name,
                    p.kills as i64,
                    p.deaths_to_it as i64,
                    p.max_hit as i64,
                    p.first_seen,
                    p.last_seen,
                    p.kind.map(|k| k.as_str()).unwrap_or(""),
                ])?;
                n += 1;
                for m in &p.melee {
                    melee_stmt.execute(params![
                        game,
                        // The pairing's own character, not the store's: a
                        // merged report can carry rows from another log.
                        m.character,
                        sv,
                        source,
                        p.zone,
                        p.mode.as_str(),
                        p.difficulty,
                        p.name,
                        m.level as i64,
                        m.stance,
                        m.swings as i64,
                        m.hits as i64,
                        m.misses as i64,
                        m.avoided as i64,
                        m.damage as i64,
                        m.max_hit as i64,
                    ])?;
                }
            }
            let mut hp_stmt = tx.prepare(
                "INSERT OR REPLACE INTO npc_health_samples
                 (game, character, server, source, zone, mode, difficulty, npc, at, hp)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            )?;
            for s in health {
                hp_stmt.execute(params![
                    game,
                    c,
                    sv,
                    source,
                    s.zone,
                    s.mode.as_str(),
                    s.difficulty,
                    s.name,
                    s.at,
                    s.hp as i64,
                ])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// What considers said in every log but `source`, for `game`, keyed to
    /// join the bestiary. The record's game tag, not the slug: see
    /// `record::tag`.
    pub fn considers_excluding(
        &self,
        game: norrlog_core::game::Game,
        source: &str,
    ) -> Result<norrlog_core::bestiary::ConsiderSeen> {
        let mut out = norrlog_core::bestiary::ConsiderSeen::new();
        let mut st = self.conn.prepare(
            "SELECT creature, level, times, rare FROM considers
             WHERE game = ?1 AND source <> ?2",
        )?;
        let rows = st.query_map(params![crate::record::tag(game), source], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        for row in rows {
            let (creature, level, times, rare) = row?;
            let e = out
                .entry(norrlog_core::bestiary::consider_key(&creature))
                .or_default();
            // Minus one is the placeholder row for a creature with marks and
            // no level; see `put_record`.
            if level >= 0 {
                *e.0.entry(level as u32).or_default() += times as u64;
            }
            e.1 |= rare;
        }
        Ok(out)
    }

    /// Every creature on file for one game, except those from one log.
    ///
    /// The exclusion is the whole point. The log being followed is flushed
    /// here periodically, so its rows are already stored *and* still live in
    /// the engine; a caller that wants both history and an up-to-the-second
    /// view has to take the stored rows from every log but this one and let
    /// the engine supply this one. Adding the two unfiltered would double
    /// every kill in the current file.
    ///
    /// Returns profiles as they were written, per log — not merged. Folding
    /// them is [`norrlog_core::bestiary::merge`]'s job, so that one rule
    /// governs it whether the caller is the API or a future export.
    pub fn bestiary_excluding(
        &self,
        game: &str,
        source: &str,
    ) -> Result<(
        Vec<norrlog_core::bestiary::NpcProfile>,
        Vec<norrlog_core::bestiary::HealthSample>,
    )> {
        use norrlog_core::bestiary::{HealthSample, MeleeAgainst, NpcCast, NpcProfile};
        use norrlog_core::game::Game;
        use norrlog_core::zone::Mode;

        let parsed_game = match game {
            "everquest" => Game::EverQuest,
            "everquest-legends" => Game::EverQuestLegends,
            _ => Game::Unknown,
        };

        // Melee first, so each profile can be handed its own rows.
        //
        // `source` is part of the grouping key, not just the creature: a
        // profile row exists per log, and handing one log's profile the melee
        // of every log would attach the same swings to a row whose counts do
        // not include them. The merge would then see one already-pooled row
        // rather than one row per log, and the pairing split would be lost.
        type MeleeKey = (String, String, String, u8, String);
        let mut melee: std::collections::HashMap<MeleeKey, Vec<MeleeAgainst>> =
            std::collections::HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT source, zone, mode, difficulty, npc, character, level, stance,
                    swings, hits, misses, avoided, damage, max_hit
             FROM npc_melee WHERE game = ?1 AND source <> ?2",
        )?;
        let rows = stmt.query_map(params![game, source], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u8>(3)?,
                r.get::<_, String>(4)?,
                MeleeAgainst {
                    character: r.get(5)?,
                    level: r.get::<_, i64>(6)? as u32,
                    stance: r.get(7)?,
                    swings: r.get::<_, i64>(8)? as u64,
                    hits: r.get::<_, i64>(9)? as u64,
                    misses: r.get::<_, i64>(10)? as u64,
                    avoided: r.get::<_, i64>(11)? as u64,
                    damage: r.get::<_, i64>(12)? as u64,
                    max_hit: r.get::<_, i64>(13)? as u64,
                },
            ))
        })?;
        for row in rows {
            let (src, zone, mode, difficulty, npc, m) = row?;
            melee
                .entry((src, zone, mode, difficulty, npc))
                .or_default()
                .push(m);
        }

        // Casts, grouped the same way and for the same reason.
        let mut casts: std::collections::HashMap<MeleeKey, Vec<NpcCast>> =
            std::collections::HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT source, zone, mode, difficulty, npc, spell, casts, hits, damage_total,
                    damage_min, damage_max, element, ticks, tick_damage
             FROM npc_casts WHERE game = ?1 AND source <> ?2",
        )?;
        let rows = stmt.query_map(params![game, source], |r| {
            let element: String = r.get(11)?;
            Ok((
                (
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, u8>(3)?,
                    r.get::<_, String>(4)?,
                ),
                NpcCast {
                    spell: r.get(5)?,
                    casts: r.get::<_, i64>(6)? as u64,
                    hits: r.get::<_, i64>(7)? as u64,
                    damage_total: r.get::<_, i64>(8)? as u64,
                    damage_min: r.get::<_, i64>(9)? as u64,
                    damage_max: r.get::<_, i64>(10)? as u64,
                    element: (!element.is_empty()).then_some(element),
                    ticks: r.get::<_, i64>(12)? as u64,
                    tick_damage: r.get::<_, i64>(13)? as u64,
                },
            ))
        })?;
        for row in rows {
            let (key, k) = row?;
            casts.entry(key).or_default().push(k);
        }

        let mut stmt = self.conn.prepare(
            "SELECT source, zone, mode, difficulty, npc, kills, deaths_to_it, max_hit,
                    first_seen, last_seen, kind
             FROM npc_profiles WHERE game = ?1 AND source <> ?2",
        )?;
        let rows = stmt
            .query_map(params![game, source], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, u8>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)? as u64,
                    r.get::<_, i64>(6)? as u64,
                    r.get::<_, i64>(7)? as u64,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, String>(10)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut profiles = Vec::with_capacity(rows.len());
        for (src, zone, mode, difficulty, npc, kills, deaths, max_hit, first, last, kind) in rows {
            // A mode nothing recognises is dropped rather than guessed at:
            // filing an instance as the open world would merge it into a
            // different creature entirely.
            let Some(parsed_mode) = Mode::from_str(&mode) else {
                continue;
            };
            let key = (src, zone.clone(), mode, difficulty, npc.clone());
            profiles.push(NpcProfile {
                casts: casts.remove(&key).unwrap_or_default(),
                melee: melee.remove(&key).unwrap_or_default(),
                // The daemon's table stores the canonical name and has no
                // column for the article, so the two are the same here. A
                // profile read back out of it is identified correctly and
                // simply does not know how the client spelt it.
                key: norrlog_core::drops::canonical_npc(&npc).to_string(),
                name: npc,
                zone,
                mode: parsed_mode,
                difficulty,
                game: parsed_game,
                // Empty for every row written before body types were recorded,
                // and for every creature nobody has proved one for.
                kind: norrlog_core::bestiary::CreatureKind::from_str(&kind),
                kills,
                deaths_to_it: deaths,
                // Recomputed by the merge from the samples; whatever is put
                // here would be overwritten.
                health_samples: 0,
                health_min: 0,
                health_median: 0,
                health_max: 0,
                max_hit,
                // Attached after the merge, from the record.
                considered: Vec::new(),
                rare: false,
                first_seen: first,
                last_seen: last,
            });
        }

        let mut stmt = self.conn.prepare(
            "SELECT zone, mode, difficulty, npc, at, hp
             FROM npc_health_samples WHERE game = ?1 AND source <> ?2",
        )?;
        let health = stmt
            .query_map(params![game, source], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u8>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)? as u64,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(zone, mode, difficulty, name, at, hp)| {
                Some(HealthSample {
                    zone,
                    mode: Mode::from_str(&mode)?,
                    difficulty,
                    name,
                    at,
                    hp,
                })
            })
            .collect();

        Ok((profiles, health))
    }

    /// Record what merchants charged, per merchant.
    ///
    /// Same keying as [`Store::put_drops`]: a re-parse replaces this log's
    /// rows and nothing else.
    pub fn put_vendors(
        &mut self,
        game: &str,
        source: &str,
        prices: &[norrlog_core::tradeskill::ItemPrice],
    ) -> Result<usize> {
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            // ON CONFLICT DO UPDATE rather than INSERT OR REPLACE, and the
            // difference is not stylistic.
            //
            // REPLACE deletes the conflicting row and inserts a new one, so
            // every column the statement does not name reverts to its default.
            // That makes adding a column a live hazard: any writer still
            // running the previous build keeps working, keeps looking correct,
            // and quietly zeroes the new column on every row it touches. It
            // happened the day `quoted` was added — a daemon left over from an
            // earlier build sat there resetting it to 0 while a freshly built
            // one wrote it, and nothing anywhere reported a conflict.
            //
            // Naming the columns to update confines a writer to the columns it
            // knows about, which is the property that makes an old binary
            // merely out of date instead of destructive.
            let mut stmt = tx.prepare(
                "INSERT INTO item_vendors
                 (game, character, server, source, item, merchant, zone,
                  unit_copper, bought, quoted, last_bought)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                 ON CONFLICT(game, character, server, source, item, merchant)
                 DO UPDATE SET
                   zone        = excluded.zone,
                   unit_copper = excluded.unit_copper,
                   bought      = excluded.bought,
                   quoted      = excluded.quoted,
                   last_bought = excluded.last_bought",
            )?;
            for p in prices {
                for v in &p.vendors {
                    stmt.execute(params![
                        game,
                        c,
                        sv,
                        source,
                        p.item,
                        v.merchant,
                        v.zone.clone().unwrap_or_default(),
                        v.unit_copper as i64,
                        v.bought as i64,
                        v.quoted as i64,
                        v.last_bought,
                    ])?;
                    n += 1;
                }
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Every merchant price on file, summed across logs and characters.
    ///
    /// Returns `(game, item, merchant, zone, unit_copper, bought, quoted,
    /// last_bought)`. Quantities add up across logs; the price is the most
    /// recent one seen rather than a sum or an average, since it is what that
    /// merchant charges today.
    #[allow(clippy::type_complexity)]
    pub fn all_vendors(&self) -> Result<Vec<(String, String, String, String, u64, u64, u64, i64)>> {
        // SQLite resolves the bare columns beside a MAX() aggregate from the
        // row that produced the maximum, which is exactly the "price as of the
        // most recent sighting" wanted here.
        let mut stmt = self.conn.prepare(
            "SELECT game, item, merchant, zone, unit_copper, SUM(bought),
                    SUM(quoted), MAX(last_bought)
             FROM item_vendors GROUP BY game, lower(item), lower(merchant)
             ORDER BY unit_copper DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    r.get::<_, i64>(4)? as u64,
                    r.get::<_, i64>(5)? as u64,
                    r.get::<_, i64>(6)? as u64,
                    r.get(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Kills per creature per game, summed across logs and characters.
    pub fn all_kills(&self) -> Result<Vec<(String, String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT game, npc, SUM(kills) FROM npc_kills
             GROUP BY game, lower(npc) ORDER BY SUM(kills) DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn note_source(&mut self, path: &str, bytes: u64, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sources (path, last_seen, bytes) VALUES (?1,?2,?3)",
            params![path, now, bytes as i64],
        )?;
        Ok(())
    }

    /// Hide what nobody played from this connection, until the guard drops.
    ///
    /// For building a bundle. The history follows every log it is pointed at,
    /// and once that was the test server and six fixture logs beside the real
    /// ones -- all of which early bundles sent to eqldb as the operator's own
    /// play: 2,728 drop
    /// pairings and 522 kill tallies from `eqlog_Brennagh_test.txt` alone.
    /// eqldb now refuses such a bundle outright, because once built its totals
    /// cannot be taken apart.
    ///
    /// Done as TEMP views named after the tables, rather than a filter added
    /// to each query. A temp view shadows the main table for every
    /// unqualified name on this connection, so every statement the builder
    /// runs -- and any added later -- reads only what was played, while the
    /// daemon's own pages, which read the same functions on other calls, keep
    /// seeing everything. The views are this connection's alone and go when
    /// the guard does.
    ///
    /// Two rules, the same two eqldb refuses on: a server in
    /// [`UNPLAYED_SERVERS`], and a log read out of `/tmp`. Tables are found by
    /// their `server` column rather than listed, so a new one is covered.
    pub fn played_only(&self) -> Result<PlayedOnly<'_>> {
        let servers = UNPLAYED_SERVERS
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let tables: Vec<(String, bool)> = self
            .conn
            .prepare(
                "SELECT m.name, SUM(p.name = 'source') > 0
                   FROM sqlite_master m JOIN pragma_table_info(m.name) p
                  WHERE m.type = 'table' AND p.name IN ('server', 'source')
                  GROUP BY m.name HAVING SUM(p.name = 'server') > 0",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut guard = PlayedOnly {
            conn: &self.conn,
            views: Vec::new(),
        };
        for (table, has_source) in tables {
            let scratch = if has_source {
                " AND source NOT LIKE '/tmp/%'"
            } else {
                ""
            };
            self.conn.execute_batch(&format!(
                "DROP VIEW IF EXISTS temp.\"{table}\";
                 CREATE TEMP VIEW \"{table}\" AS SELECT * FROM main.\"{table}\"
                  WHERE lower(server) NOT IN ({servers}){scratch}"
            ))?;
            guard.views.push(table);
        }
        // `sources` has only the path. The server is in the file name, which
        // is where `identity_from_filename` reads it for the bundle.
        let test_names: String = UNPLAYED_SERVERS
            .iter()
            .map(|s| format!(" AND lower(path) NOT LIKE '%\\_{s}.txt' ESCAPE '\\'"))
            .collect();
        self.conn.execute_batch(&format!(
            "DROP VIEW IF EXISTS temp.sources;
             CREATE TEMP VIEW sources AS SELECT * FROM main.sources
              WHERE path NOT LIKE '/tmp/%'{test_names}"
        ))?;
        guard.views.push("sources".to_string());
        Ok(guard)
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
             FROM boss_kills WHERE character = ?1 AND server = ?2
             ORDER BY timestamp DESC",
        )?;
        let rows = stmt
            .query_map(params![self.character, self.server], |r| {
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

    /// Every boss kill on file, every character, with its fight.
    ///
    /// Unscoped, unlike [`Self::kills`], and that is the whole point — see
    /// [`ExportedKill`]. Oldest first, because an export is read as a record
    /// and a record reads forwards.
    ///
    /// # The join
    ///
    /// A kill is matched to the encounter whose span contains it. The parser
    /// already did this once, when the kill was recorded — `duration`,
    /// `your_damage` and `your_dps` on the row came from exactly this match —
    /// so this is not a second opinion; it is recovering the rest of the
    /// columns from the same row. `LEFT JOIN`, because a kill whose encounter
    /// has since been pruned is still a kill, and dropping it here would make
    /// the export quietly smaller than the history it claims to be.
    ///
    /// # Picking one encounter out of several
    ///
    /// A kill can sit inside more than one stored encounter, and on this
    /// machine 46 of 198 do. Two causes, and they need different care.
    ///
    /// The first is re-titling: a growing pull is stored again under a new
    /// title -- `a Pickclaw Arroweater`, then `(+3 more)`, then `(+7 more)` --
    /// and the primary key is `(character, server, start, title)`, so every
    /// re-title is a new row at the same start. Sixteen of them on one fight.
    ///
    /// The second is overlap, and it is the dangerous one. A fight beginning
    /// at the instant the boss dies contains the kill by a `BETWEEN`: the
    /// Innoruuk kill at 1786543026 is inside `Innoruuk\`s Chosen`, which
    /// starts exactly then and is plainly the *next* fight. An obvious
    /// `MAX(start)` rule picks it every time.
    ///
    /// So the pick prefers, in order: the encounter whose own figures match
    /// what the kill row recorded -- which identifies it exactly, since those
    /// figures were copied from it; then one ending at the kill; then one that
    /// had already started; then the tightest span. 169 of 198 are settled by
    /// the first test alone, and the rest are the rows whose stored figure was
    /// a mid-fight snapshot and matches nothing.
    pub fn exported_kills(&self) -> Result<Vec<ExportedKill>> {
        let mut stmt = self.conn.prepare(
            "SELECT k.character, k.server, k.timestamp, k.boss, k.difficulty,
                    k.solo, k.zone, k.build,
                    COALESCE(e.duration, k.duration), k.duration,
                    k.your_damage, k.your_dps,
                    COALESCE(e.start, 0), COALESCE(e.end, 0),
                    COALESCE(e.title, ''), COALESCE(e.total_damage, 0),
                    COALESCE(e.kills, 0), COALESCE(e.deaths, 0)
               FROM boss_kills k
               LEFT JOIN encounters e
                 ON e.character = k.character AND e.server = k.server
                AND (e.start, e.title) = (
                      SELECT x.start, x.title FROM encounters x
                       WHERE x.character = k.character AND x.server = k.server
                         AND k.timestamp BETWEEN x.start AND x.end
                       ORDER BY (x.duration = k.duration
                                 AND x.your_damage = k.your_damage) DESC,
                                (x.end = k.timestamp) DESC,
                                (x.start < k.timestamp) DESC,
                                (x.end - x.start) ASC,
                                x.title ASC
                       LIMIT 1)
              ORDER BY k.timestamp ASC, k.boss ASC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ExportedKill {
                    character: r.get(0)?,
                    server: r.get(1)?,
                    at: r.get(2)?,
                    boss: r.get(3)?,
                    difficulty: r.get(4)?,
                    solo: r.get::<_, i64>(5)? != 0,
                    zone: r.get(6)?,
                    build: r.get(7)?,
                    duration: r.get(8)?,
                    parser_seconds: r.get(9)?,
                    your_damage: r.get(10)?,
                    your_dps: r.get(11)?,
                    enc_start: r.get(12)?,
                    enc_end: r.get(13)?,
                    enc_title: r.get(14)?,
                    total_damage: r.get(15)?,
                    enc_kills: r.get(16)?,
                    deaths: r.get(17)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every (character, server) that has a boss kill on file.
    ///
    /// The export declares this as its scope, so the ingest knows what it is
    /// allowed to replace. Derived from the kills rather than from the logs:
    /// the claim being made is "this is the whole of what I have for these
    /// characters", and only the kills can support it.
    pub fn kill_scopes(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT character, server FROM boss_kills ORDER BY character, server",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn levels(&self) -> Result<Vec<StoredLevel>> {
        let mut stmt = self.conn.prepare(
            "SELECT level, started, ended, seconds, kills, percent, build, zones
             FROM levels WHERE character = ?1 AND server = ?2 ORDER BY level ASC",
        )?;
        let rows = stmt
            .query_map(params![self.character, self.server], |r| {
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
    /// Encounters whose title matches, for tests. The server searches
    /// through its own paginated query rather than this.
    #[cfg(test)]
    pub fn encounters_named(&self, like: &str) -> Result<Vec<StoredEncounter>> {
        let mut stmt = self.conn.prepare(
            "SELECT start, end, title, zone, duration, your_damage, your_dps, total_damage, kills, deaths, build, difficulty
             FROM encounters WHERE character = ?1 AND server = ?2 AND title LIKE ?3
             ORDER BY start ASC",
        )?;
        let rows = stmt
            .query_map(
                params![self.character, self.server, format!("%{like}%")],
                |r| {
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
                        difficulty: r.get(11)?,
                    })
                },
            )?
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
             FROM boss_kills WHERE character = ?1 AND server = ?2
             GROUP BY boss, difficulty, build
             ORDER BY boss ASC, difficulty DESC, build ASC",
        )?;
        let rows = stmt
            .query_map(params![self.character, self.server], |r| {
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
        let who = params![self.character, self.server];
        let k: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM boss_kills WHERE character = ?1 AND server = ?2",
            who,
            |r| r.get(0),
        )?;
        let e: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM encounters WHERE character = ?1 AND server = ?2",
            who,
            |r| r.get(0),
        )?;
        let l: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM levels WHERE character = ?1 AND server = ?2",
            who,
            |r| r.get(0),
        )?;
        Ok((k, e, l))
    }
}

/// Whether a log ever names a group instance as one (`You have entered
/// Nagafen's Lair - Group 2.`), or `None` when it cannot be read. Read in
/// full, once per log; a 400 MB log takes about a second.
fn log_marks_group(path: &Path) -> Option<bool> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut r = std::io::BufReader::with_capacity(1 << 20, file);
    let entered = memchr::memmem::Finder::new(b"] You have entered ");
    let group = memchr::memmem::Finder::new(b" - Group");
    let mut line = Vec::new();
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line).ok()? == 0 {
            return Some(false);
        }
        if let Some(i) = entered.find(&line) {
            if group.find(&line[i..]).is_some() {
                return Some(true);
            }
        }
    }
}

/// The encounters insert, inside whatever transaction the caller holds.
fn insert_encounters(
    conn: &rusqlite::Connection,
    c: &str,
    sv: &str,
    encs: &[StoredEncounter],
) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO encounters
         (character, server, start, end, title, zone, duration, your_damage, your_dps, total_damage, kills, deaths, build, difficulty)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
    )?;
    for e in encs {
        stmt.execute(params![
            c,
            sv,
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
            e.difficulty,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bundle carries only what was played, and the daemon's own reads do
    /// not change.
    #[test]
    fn a_bundle_leaves_out_the_test_server_and_scratch_logs() {
        let s = Store::in_memory().unwrap();
        for (server, source, item, n) in [
            (
                "freeport",
                "/mnt/Logs/eqlog_Tharn_freeport.txt",
                "Rusty Dagger",
                3,
            ),
            (
                "test",
                "/mnt/Logs/eqlog_Brennagh_test.txt",
                "Rusty Dagger",
                5,
            ),
            (
                "freeport",
                "/tmp/scratch/eqlog_Capped_freeport.txt",
                "Bone Chips",
                7,
            ),
        ] {
            s.conn
                .execute(
                    "INSERT INTO item_drops (game, character, server, source, item, npc,
                                             zones, looted, quantity, first_seen, last_seen)
                     VALUES ('everquest-legends', 'X', ?1, ?2, ?3, 'a skeleton', '', ?4, ?4, 1, 2)",
                    params![server, source, item, n],
                )
                .unwrap();
            s.conn
                .execute(
                    "INSERT INTO npc_kills (game, character, server, source, npc, kills)
                     VALUES ('everquest-legends', 'X', ?1, ?2, 'a skeleton', ?3)",
                    params![server, source, n],
                )
                .unwrap();
            s.conn
                .execute(
                    "INSERT INTO sources (path, last_seen, bytes) VALUES (?1, 2, 1)",
                    params![source],
                )
                .unwrap();
        }
        let b = crate::bundle::build(&s, None, "operator", 10).unwrap();
        let drops: Vec<(String, u64)> =
            b.drops.iter().map(|d| (d.item.clone(), d.looted)).collect();
        assert_eq!(drops, [("Rusty Dagger".to_string(), 3)]);
        assert_eq!(b.kills.iter().map(|k| k.kills).sum::<u64>(), 3);
        let logs: Vec<&str> = b.logs.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(logs, ["/mnt/Logs/eqlog_Tharn_freeport.txt"]);

        // Gone with the guard: the daemon's own pages still see every log.
        let all: u64 = s.all_drops().unwrap().iter().map(|d| d.looted).sum();
        assert_eq!(all, 15);
        assert_eq!(s.sources().unwrap().len(), 3);
    }

    /// The raid-kill export obeys the same guard as a bundle.
    ///
    /// One history held 46 test-server kills, and the first export sent every
    /// one of them to the site.
    #[test]
    fn the_raid_kill_export_leaves_out_the_test_server() {
        let s = Store::in_memory().unwrap();
        for (at, character, server) in [(100, "Tharn", "freeport"), (200, "Brennagh", "test")] {
            s.conn
                .execute(
                    "INSERT INTO boss_kills (timestamp, boss, difficulty, character, server)
                     VALUES (?1, 'Lady Vox', 0, ?2, ?3)",
                    params![at, character, server],
                )
                .unwrap();
        }
        {
            let _played = s.played_only().unwrap();
            let kills: Vec<String> = s
                .exported_kills()
                .unwrap()
                .into_iter()
                .map(|k| k.character)
                .collect();
            assert_eq!(kills, ["Tharn"]);
            assert_eq!(
                s.kill_scopes().unwrap(),
                [("Tharn".to_string(), "freeport".to_string())]
            );
        }
        assert_eq!(
            s.exported_kills().unwrap().len(),
            2,
            "unguarded, the daemon sees both"
        );
    }

    /// A record built the way the engine builds one, so these exercise the
    /// real collectors rather than hand-written rows.
    fn record_over(text: &str) -> crate::record::Settled {
        let mut e = crate::engine::Engine::with_game(
            "Tharn".to_string(),
            norrlog_core::spell::SpellDb::empty(),
            norrlog_core::game::Game::EverQuestLegends,
        );
        for line in text.lines() {
            e.push_line(line);
        }
        e.record()
    }

    const RIVERVALE: &str = "\
[Tue Jul 28 15:03:10 2026] You have entered Rivervale.
[Tue Jul 28 15:03:11 2026] You say, '--EQLCartographer--Ranger Guildmaster'
[Tue Jul 28 15:03:12 2026] You say, 'Hail, Megosh Thistlethorn'
[Tue Jul 28 15:03:13 2026] Your Location is -27.08, -440.14, -11.75
[Tue Jul 28 15:03:14 2026] Megosh Thistlethorn glowers at you dubiously -- what would you like your tombstone to say? (Lvl: 70)
[Tue Jul 28 15:03:15 2026] [50 WAR/DRU/BRD] Tharn (Ogre) ZONE: Rivervale (rivervale)
[Tue Jul 28 15:03:16 2026] You have become better at Baking! (42)
";

    /// The same log read as each game files two sets of rows, and rows filed
    /// as Legends before the games were told apart move when their log's
    /// path says it was live EverQuest.
    #[test]
    fn the_two_games_records_are_kept_apart_and_old_live_rows_move() {
        let dir = std::env::temp_dir().join(format!("eqlp-retag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("history.db");
        let live = "/games/Everquest/Logs/eqlog_Dorrak_drinal.txt";
        let legends = "/games/EverquestLegends/Logs/eqlog_Dorrak_drinal.txt";
        let nameless = "/tmp/eqlog_Dorrak_drinal.txt";
        {
            let mut st = Store::open_as(&db, "Dorrak", "drinal").unwrap();
            let rec = record_over(RIVERVALE);
            st.put_record(Game::EverQuest, live, &rec).unwrap();
            st.put_record(Game::EverQuestLegends, legends, &rec)
                .unwrap();
            let n = |g| st.record_reports(g).unwrap().consider.considers.len();
            assert!(n(Game::EverQuest) > 0);
            assert_eq!(
                n(Game::EverQuest),
                n(Game::EverQuestLegends),
                "the same inputs, kept apart"
            );
            assert_eq!(n(Game::Unknown), 0, "neither game's rows read as unknown");
            // Rows filed before the games were told apart: all `eql`.
            st.put_record(Game::EverQuestLegends, live, &rec).unwrap();
            st.put_record(Game::EverQuestLegends, nameless, &rec)
                .unwrap();
        }
        let st = Store::open_as(&db, "Dorrak", "drinal").unwrap();
        let sources = |g: &str| -> Vec<String> {
            st.conn
                .prepare("SELECT DISTINCT source FROM considers WHERE game = ?1 ORDER BY source")
                .unwrap()
                .query_map(params![g], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(
            sources("eq"),
            [live],
            "the live install's rows moved off eql"
        );
        assert_eq!(
            sources("eql"),
            [legends, nameless],
            "a path naming neither game is not guessed at"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What went in comes back out.
    #[test]
    fn the_record_survives_a_round_trip() {
        let mut st = Store::in_memory().unwrap();
        st.put_record(Game::EverQuestLegends, "log-a", &record_over(RIVERVALE))
            .unwrap();
        let r = st.record_reports(Game::EverQuestLegends).unwrap();

        assert_eq!(r.players.source, "history");
        assert!(r.players.players.iter().any(|p| p.name == "Tharn"));
        assert!(r
            .consider
            .considers
            .iter()
            .any(|c| c.creature.contains("Megosh")));
        assert_eq!(
            r.skills
                .skills
                .iter()
                .find(|s| s.skill == "Baking")
                .map(|s| s.at_least),
            Some(42)
        );
        assert_eq!(
            r.carto
                .markers
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Megosh Thistlethorn"]
        );
        assert_eq!(r.carto.zones.len(), 1);
    }

    /// Re-reading a log replaces that log's rows and nothing else. Without the
    /// per-source key a second parse would double every count.
    #[test]
    fn re_reading_one_log_replaces_only_its_own_rows() {
        let mut st = Store::in_memory().unwrap();
        let rec = record_over(RIVERVALE);
        st.put_record(Game::EverQuestLegends, "log-a", &rec)
            .unwrap();
        let once = st.record_reports(Game::EverQuestLegends).unwrap();
        st.put_record(Game::EverQuestLegends, "log-a", &rec)
            .unwrap();
        let twice = st.record_reports(Game::EverQuestLegends).unwrap();
        assert_eq!(
            once.consider.considers[0].considers, twice.consider.considers[0].considers,
            "a second read of the same log is the same reading, not twice as much"
        );
        assert_eq!(once.carto.markers.len(), twice.carto.markers.len());

        // A second log adds to it.
        st.put_record(Game::EverQuestLegends, "log-b", &rec)
            .unwrap();
        let both = st.record_reports(Game::EverQuestLegends).unwrap();
        assert_eq!(
            both.consider.considers[0].considers,
            twice.consider.considers[0].considers * 2,
            "two logs that saw the same thing saw it twice"
        );
    }

    /// Where a drop happened survives the trip to disk, folds a graded name
    /// onto its base the way the parent row does, and does not double when
    /// the same log is read twice.
    ///
    /// The fold is the part worth a test. `all_drops` folds
    /// `Woven Skull Cap +1` onto `Woven Skull Cap`; if the breakdown did not
    /// fold identically, a row would carry places summing to more loots than
    /// the row itself records, and the difference would look like a parser
    /// fault rather than a join.
    #[test]
    fn drop_places_survive_the_round_trip_and_fold_like_their_row() {
        use norrlog_core::drops::{DropPlace, DropReport, DropSighting, ItemDrops};
        use norrlog_core::game::Game;

        let sighting = |zone: &str, mode: &str, difficulty: u8, looted: u64| DropSighting {
            npc: "a black wolf".into(),
            zones: vec![zone.to_string()],
            places: vec![DropPlace {
                zone: zone.to_string(),
                mode: mode.to_string(),
                difficulty,
                looted,
                quantity: looted,
            }],
            looted,
            left: 0,
            quantity: looted,
            kills: looted,
            grades: Vec::new(),
            first_seen: 1,
            last_seen: 2,
        };
        let report = DropReport {
            game: Game::EverQuestLegends,
            kills: Vec::new(),
            items: vec![
                ItemDrops {
                    item: "Woven Skull Cap".into(),
                    game: Game::EverQuestLegends,
                    sources: vec![sighting("rivervale", "open", 0, 5)],
                    looted: 5,
                    left: 0,
                    quantity: 5,
                    grades: Vec::new(),
                },
                // The same item under its graded name, as rows written before
                // the ungraded key are still on file.
                ItemDrops {
                    item: "Woven Skull Cap +1".into(),
                    game: Game::EverQuestLegends,
                    sources: vec![sighting("misty thicket", "instance", 1, 3)],
                    looted: 3,
                    left: 0,
                    quantity: 3,
                    grades: Vec::new(),
                },
            ],
        };

        let mut st = Store::in_memory().unwrap();
        st.put_drops("everquest-legends", "log-a", &report).unwrap();
        st.put_drops("everquest-legends", "log-a", &report).unwrap();

        let places = st.all_drop_places().unwrap();
        let key = (
            "everquest-legends".to_string(),
            "woven skull cap".to_string(),
            "a black wolf".to_string(),
        );
        let got = places
            .get(&key)
            .expect("both names fold onto the base item");
        assert_eq!(got.len(), 2, "two places, not four: {got:?}");
        assert_eq!(
            got.iter().map(|p| p.3).sum::<u64>(),
            8,
            "reading one log twice is not twice as many loots: {got:?}"
        );
        // Biggest first.
        assert_eq!(got[0].0, "rivervale");
        assert_eq!(got[0].1, "open");
        assert_eq!(got[1].2, 1, "the instance tier is kept, not flattened");
    }

    /// A place that stops being reported is gone, not left behind.
    ///
    /// The parent row is keyed on the pairing so a rewrite replaces it. A
    /// place is keyed on the place, so `INSERT OR REPLACE` alone would leave
    /// a zone nothing supports any more sitting on file forever.
    #[test]
    fn a_place_that_stops_being_reported_is_removed() {
        use norrlog_core::drops::{DropPlace, DropReport, DropSighting, ItemDrops};
        use norrlog_core::game::Game;

        let report = |zone: &str| DropReport {
            game: Game::EverQuestLegends,
            kills: Vec::new(),
            items: vec![ItemDrops {
                item: "Chunk of Meat".into(),
                game: Game::EverQuestLegends,
                sources: vec![DropSighting {
                    npc: "a black wolf".into(),
                    zones: vec![zone.to_string()],
                    places: vec![DropPlace {
                        zone: zone.to_string(),
                        mode: "open".into(),
                        difficulty: 0,
                        looted: 1,
                        quantity: 1,
                    }],
                    looted: 1,
                    left: 0,
                    quantity: 1,
                    kills: 1,
                    grades: Vec::new(),
                    first_seen: 1,
                    last_seen: 2,
                }],
                looted: 1,
                left: 0,
                quantity: 1,
                grades: Vec::new(),
            }],
        };

        let mut st = Store::in_memory().unwrap();
        st.put_drops("everquest-legends", "log-a", &report("rivervale"))
            .unwrap();
        st.put_drops("everquest-legends", "log-a", &report("misty thicket"))
            .unwrap();
        let places = st.all_drop_places().unwrap();
        let got = places
            .get(&(
                "everquest-legends".to_string(),
                "chunk of meat".to_string(),
                "a black wolf".to_string(),
            ))
            .expect("the pairing is on file");
        assert_eq!(
            got.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(),
            vec!["misty thicket"],
            "the zone the re-read no longer reports is gone: {got:?}"
        );
    }

    /// A pairing whose spelling changed is replaced, not kept beside the new
    /// one.
    ///
    /// A build up to September wrote "lizardman broodling" where the current
    /// one writes "a lizardman broodling" for the same loot line. Keyed on
    /// the spelling, the re-read added a row instead of replacing one, and
    /// Tharn's log counted 35,965 loots against the 22,817 lines it holds.
    #[test]
    fn a_respelled_pairing_replaces_the_old_row() {
        use norrlog_core::drops::{DropPlace, DropReport, DropSighting, ItemDrops};
        use norrlog_core::game::Game;

        let report = |npc: &str| DropReport {
            game: Game::EverQuestLegends,
            kills: vec![(npc.to_string(), 4)],
            items: vec![ItemDrops {
                item: "Lizardman Scale".into(),
                game: Game::EverQuestLegends,
                sources: vec![DropSighting {
                    npc: npc.into(),
                    zones: vec!["the feerrott".into()],
                    places: vec![DropPlace {
                        zone: "the feerrott".into(),
                        mode: "open".into(),
                        difficulty: 0,
                        looted: 3,
                        quantity: 3,
                    }],
                    looted: 3,
                    left: 0,
                    quantity: 3,
                    kills: 4,
                    grades: Vec::new(),
                    first_seen: 1,
                    last_seen: 2,
                }],
                looted: 3,
                left: 0,
                quantity: 3,
                grades: Vec::new(),
            }],
        };

        let mut st = Store::in_memory().unwrap();
        st.put_drops("everquest-legends", "log-a", &report("lizardman broodling"))
            .unwrap();
        // Another log's rows are not this log's to clear.
        st.put_drops("everquest-legends", "log-b", &report("lizardman broodling"))
            .unwrap();
        st.put_drops(
            "everquest-legends",
            "log-a",
            &report("a lizardman broodling"),
        )
        .unwrap();

        let rows = |table: &str, source: &str| -> Vec<String> {
            let mut q = st
                .conn
                .prepare(&format!(
                    "SELECT npc FROM {table} WHERE source = ?1 ORDER BY npc"
                ))
                .unwrap();
            q.query_map([source], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            rows("item_drops", "log-a"),
            ["a lizardman broodling"],
            "the old spelling is gone"
        );
        assert_eq!(rows("npc_kills", "log-a"), ["a lizardman broodling"]);
        assert_eq!(rows("item_drops", "log-b"), ["lizardman broodling"]);
        assert_eq!(rows("npc_kills", "log-b"), ["lizardman broodling"]);
    }

    /// Logs with drops on file are read again once, so the respelled rows
    /// `put_drops` used to leave behind are cleared by the next whole read.
    #[test]
    fn logs_with_drops_are_read_again_once() {
        let mut st = Store::in_memory().unwrap();
        st.note_source("/logs/a.txt", 500, 1).unwrap();
        st.note_source("/logs/b.txt", 700, 1).unwrap();
        st.conn
            .execute(
                "INSERT INTO item_drops (game, character, server, source, item, npc, zones,
                 looted, quantity, first_seen, last_seen)
                 VALUES ('everquest-legends','Test','test','/logs/a.txt','Bone Chips',
                 'a decaying skeleton','',1,1,1,1)",
                [],
            )
            .unwrap();
        st.reread_logs_with_drops_once().unwrap();
        let bytes = |st: &Store| {
            st.sources()
                .unwrap()
                .into_iter()
                .map(|(p, _, b)| (p, b))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let got = bytes(&st);
        assert_eq!(got["/logs/a.txt"], 0, "a log with drops is read again");
        assert_eq!(got["/logs/b.txt"], 700, "one without is left alone");

        st.note_source("/logs/a.txt", 500, 2).unwrap();
        st.reread_logs_with_drops_once().unwrap();
        assert_eq!(bytes(&st)["/logs/a.txt"], 500, "and only once");
    }

    /// A skill ceiling is the highest anybody reached, so a shorter re-read
    /// cannot lower it — the one table here that is not a running total.
    #[test]
    fn a_skill_ceiling_never_falls() {
        let mut st = Store::in_memory().unwrap();
        st.put_record(Game::EverQuestLegends, "log-a", &record_over(RIVERVALE))
            .unwrap();
        let lower = "[Tue Jul 28 15:03:16 2026] You have become better at Baking! (7)\n";
        st.put_record(Game::EverQuestLegends, "log-a", &record_over(lower))
            .unwrap();
        let r = st.record_reports(Game::EverQuestLegends).unwrap();
        assert_eq!(
            r.skills
                .skills
                .iter()
                .find(|s| s.skill == "Baking")
                .map(|s| s.at_least),
            Some(42),
            "a lower reading is an older one"
        );
    }

    /// Levels and their counts come back exactly, one row per level.
    ///
    /// The shape that makes this worth asserting is a creature seen at two
    /// levels: the store holds one row per level and the reader has to fold
    /// them back into one creature without summing them into a single number.
    /// A level that changed between patches is two readings, not one
    /// overwriting the other.
    #[test]
    fn a_creature_seen_at_two_levels_keeps_both() {
        let log = "\
[Tue Jul 28 15:03:14 2026] a rat glowers at you dubiously -- what would you like your tombstone to say? (Lvl: 12)
[Tue Jul 28 15:04:14 2026] a rat glowers at you dubiously -- what would you like your tombstone to say? (Lvl: 12)
[Tue Jul 28 15:05:14 2026] a rat glowers at you dubiously -- what would you like your tombstone to say? (Lvl: 13)
";
        let settled = record_over(log);
        let mut st = Store::in_memory().unwrap();
        st.put_record(Game::EverQuestLegends, "log-a", &settled)
            .unwrap();
        let r = st.record_reports(Game::EverQuestLegends).unwrap();

        let row = r.consider.considers.first().expect("the rat");
        assert_eq!(row.creature, "rat");
        assert_eq!(
            row.levels
                .iter()
                .map(|l| (l.level, l.times))
                .collect::<Vec<_>>(),
            vec![(12, 2), (13, 1)],
            "two levels, each with its own count"
        );
        assert_eq!(row.considers, 3, "and the total is their sum");
    }

    /// A bestiary built the way the engine builds one, so the test exercises
    /// the real report rather than hand-written rows.
    fn bestiary(
        character: &str,
        zone: &str,
        level: u32,
        kills: &[(i64, u64)],
    ) -> (
        norrlog_core::bestiary::BestiaryReport,
        Vec<norrlog_core::bestiary::HealthSample>,
    ) {
        use norrlog_core::bestiary::{Bestiary, Swing};
        let mut b = Bestiary::new(norrlog_core::game::Game::EverQuestLegends, character);
        b.on_level(level);
        b.on_stance("defensive");
        b.on_zone(zone);
        for (at, hp) in kills {
            b.on_damage_to(*at, "a fire giant", *hp);
            b.on_kill(*at, "a fire giant");
            b.on_swing(*at, "a fire giant", Swing::Hit(*hp / 100));
        }
        (b.report(), b.health_samples())
    }

    /// A body type is proved by a single spell landing, which may happen once
    /// in one log and never again. So it has to survive the round trip, survive
    /// a re-parse of a log that no longer reaches that cast, and reach a
    /// creature the other log only ever hit with a sword.
    #[test]
    fn a_proved_body_type_outlives_the_cast_that_proved_it() {
        use norrlog_core::bestiary::CreatureKind;
        let mut s = Store::in_memory().unwrap();
        let zone = "Nagafen's Lair - Solo 4 (Refined)";

        let (mut proved, proved_hp) = bestiary("Tharn", zone, 50, &[(100, 20000)]);
        proved.npcs[0].kind = Some(CreatureKind::Undead);
        s.put_bestiary("everquest-legends", "/logs/one.txt", &proved, &proved_hp)
            .unwrap();

        // The same log flushed again after the casts scrolled out of range,
        // and a second log that never saw one at all.
        let (bare, bare_hp) = bestiary("Tharn", zone, 50, &[(100, 20000)]);
        assert_eq!(bare.npcs[0].kind, None, "this one proves nothing");
        s.put_bestiary("everquest-legends", "/logs/one.txt", &bare, &bare_hp)
            .unwrap();
        s.put_bestiary("everquest-legends", "/logs/two.txt", &bare, &bare_hp)
            .unwrap();

        let (profiles, health) = s
            .bestiary_excluding("everquest-legends", "/no/such/log.txt")
            .unwrap();
        let merged = norrlog_core::bestiary::merge(profiles, &health);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].kind,
            Some(CreatureKind::Undead),
            "nothing may unprove it"
        );
    }

    /// The whole reason the bestiary is persisted: a creature's record has to
    /// outlive the log file it was seen in, and two logs have to add up.
    ///
    /// Re-reading one log replaces its own rows rather than doubling them —
    /// the same property the drop and vendor tables need, and the one that
    /// makes a periodic flush safe to run as often as we like.
    #[test]
    fn a_creature_survives_its_log_and_two_logs_add_up() {
        let mut s = Store::in_memory().unwrap();
        let zone = "Nagafen's Lair - Solo 4 (Refined)";
        let (first, first_hp) = bestiary("Tharn", zone, 50, &[(100, 20000), (200, 22000)]);
        let (second, second_hp) = bestiary("Tharn", zone, 50, &[(900, 24000)]);

        s.put_bestiary("everquest-legends", "/logs/one.txt", &first, &first_hp)
            .unwrap();
        // Flushed twice, as the daemon really does on its timer.
        s.put_bestiary("everquest-legends", "/logs/one.txt", &first, &first_hp)
            .unwrap();
        s.put_bestiary("everquest-legends", "/logs/two.txt", &second, &second_hp)
            .unwrap();

        let (profiles, health) = s
            .bestiary_excluding("everquest-legends", "/no/such/log.txt")
            .unwrap();
        let merged = norrlog_core::bestiary::merge(profiles, &health);

        assert_eq!(merged.len(), 1, "one creature across both logs: {merged:?}");
        let p = &merged[0];
        assert_eq!(
            p.kills, 3,
            "a second flush of the same log must not double it"
        );
        assert_eq!(p.zone, "Nagafen's Lair");
        assert_eq!((p.mode, p.difficulty), (norrlog_core::zone::Mode::Solo, 4));
        assert_eq!((p.first_seen, p.last_seen), (100, 900));
        assert_eq!(
            (
                p.health_samples,
                p.health_min,
                p.health_median,
                p.health_max
            ),
            (3, 20000, 22000, 24000),
            "every sample kept, and the median taken over all of them"
        );
        // Melee round-trips with the pairing intact, stance and all.
        assert_eq!(p.melee.len(), 1);
        assert_eq!(
            (
                p.melee[0].character.as_str(),
                p.melee[0].level,
                p.melee[0].stance.as_str()
            ),
            ("Tharn", 50, "defensive")
        );
        assert_eq!(p.melee[0].swings, 3);
    }

    /// The exclusion the API depends on. The followed log is stored *and*
    /// live in the engine, so the stored side must be able to leave it out —
    /// otherwise every kill in the open file counts twice.
    #[test]
    fn the_followed_log_can_be_left_out_of_the_stored_side() {
        let mut s = Store::in_memory().unwrap();
        let zone = "Befallen 2 (Adaptive)";
        let (old, old_hp) = bestiary("Tharn", zone, 30, &[(100, 1000)]);
        let (live, live_hp) = bestiary("Tharn", zone, 30, &[(500, 1200), (600, 1100)]);
        s.put_bestiary("everquest-legends", "/logs/old.txt", &old, &old_hp)
            .unwrap();
        s.put_bestiary("everquest-legends", "/logs/live.txt", &live, &live_hp)
            .unwrap();

        let (profiles, health) = s
            .bestiary_excluding("everquest-legends", "/logs/live.txt")
            .unwrap();
        // Only the old log came back...
        assert_eq!(
            norrlog_core::bestiary::merge(profiles.clone(), &health)[0].kills,
            1
        );

        // ...so adding the engine's own view gives each kill exactly once.
        let combined = norrlog_core::bestiary::merge(
            live.npcs.clone().into_iter().chain(profiles),
            &[health, live_hp].concat(),
        );
        assert_eq!(combined[0].kills, 3);
        assert_eq!(combined[0].health_samples, 3);
    }

    /// The level a rolled log has to be told, and where each answer comes
    /// from. Getting this wrong files hours of swings under a pairing that
    /// never rejoins the real one.
    #[test]
    fn the_last_known_level_prefers_direct_evidence_over_inference() {
        let mut s = Store::in_memory().unwrap();
        assert_eq!(
            s.last_known_level().unwrap(),
            0,
            "nothing on file is not level zero, and the caller must not seed it"
        );

        // A completed level 49 means 50 was reached — the row itself never
        // says 50, which is exactly the trap.
        s.put_levels(&[StoredLevel {
            level: 49,
            started: 10,
            ended: 20,
            seconds: 10,
            kills: 5,
            percent: 100.0,
            build: String::new(),
            zones: "[]".into(),
        }])
        .unwrap();
        assert_eq!(s.last_known_level().unwrap(), 50);

        // The bestiary's own rows outrank that inference.
        let (report, health) = bestiary("Test", "Befallen 2 (Adaptive)", 52, &[(100, 900)]);
        s.put_bestiary("everquest-legends", "/logs/one.txt", &report, &health)
            .unwrap();
        assert_eq!(s.last_known_level().unwrap(), 52);

        // And a level stated by hand still counts when it is the highest.
        s.put_class_level("WAR", 60, 30).unwrap();
        assert_eq!(s.last_known_level().unwrap(), 60);
    }

    /// Another character's level must not leak into this one's — an alt who
    /// has never left Qeynos would otherwise be seeded at the main's cap.
    #[test]
    fn the_last_known_level_is_per_character() {
        let dir = std::env::temp_dir().join("norrlog-bestiary-level-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("history.db");

        let mut main = Store::open_as(&path, "Tharn", "freeport").unwrap();
        let (report, health) = bestiary("Tharn", "Befallen 2 (Adaptive)", 52, &[(100, 900)]);
        main.put_bestiary("everquest-legends", "/logs/one.txt", &report, &health)
            .unwrap();
        assert_eq!(main.last_known_level().unwrap(), 52);
        drop(main);

        let alt = Store::open_as(&path, "Grimhide", "freeport").unwrap();
        assert_eq!(
            alt.last_known_level().unwrap(),
            0,
            "an alt starts from nothing, not from the main's level"
        );
    }

    /// Two games are two sets of creatures. Pooling them would assert that a
    /// fire giant in EverQuest has the health of one in Legends.
    #[test]
    fn one_game_never_reads_back_anothers_creatures() {
        let mut s = Store::in_memory().unwrap();
        let (eql, eql_hp) = bestiary("Tharn", "Nagafen's Lair", 50, &[(100, 20000)]);
        s.put_bestiary("everquest-legends", "/logs/eql.txt", &eql, &eql_hp)
            .unwrap();
        s.put_bestiary("everquest", "/logs/eq.txt", &eql, &eql_hp)
            .unwrap();

        let (profiles, health) = s.bestiary_excluding("everquest", "/none").unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(health.len(), 1);
        assert_eq!(profiles[0].game, norrlog_core::game::Game::EverQuest);
    }

    /// A creature's identity includes the tier, in storage as much as in
    /// memory. If the table pooled them, the harder one's health would be
    /// averaged away with the easier one's.
    #[test]
    fn stored_tiers_stay_separate_creatures() {
        let mut s = Store::in_memory().unwrap();
        let (t1, t1_hp) = bestiary(
            "Tharn",
            "Nagafen's Lair - Solo 1 (Awakened)",
            50,
            &[(100, 5000)],
        );
        let (t4, t4_hp) = bestiary(
            "Tharn",
            "Nagafen's Lair - Solo 4 (Refined)",
            50,
            &[(200, 30000)],
        );
        s.put_bestiary("everquest-legends", "/logs/one.txt", &t1, &t1_hp)
            .unwrap();
        s.put_bestiary("everquest-legends", "/logs/one.txt", &t4, &t4_hp)
            .unwrap();

        let (profiles, health) = s.bestiary_excluding("everquest-legends", "/none").unwrap();
        let merged = norrlog_core::bestiary::merge(profiles, &health);
        assert_eq!(
            merged.len(),
            2,
            "same name and place, two tiers: {merged:?}"
        );
        let easy = merged.iter().find(|p| p.difficulty == 1).unwrap();
        let hard = merged.iter().find(|p| p.difficulty == 4).unwrap();
        assert_eq!((easy.health_median, hard.health_median), (5000, 30000));
    }

    fn price(
        item: &str,
        merchant: &str,
        zone: &str,
        copper: u64,
        bought: u64,
        at: i64,
    ) -> norrlog_core::tradeskill::ItemPrice {
        norrlog_core::tradeskill::ItemPrice {
            item: item.into(),
            unit_copper: copper,
            bought,
            quoted: 0,
            last_bought: at,
            vendors: vec![norrlog_core::tradeskill::VendorPrice {
                merchant: merchant.into(),
                zone: Some(zone.into()),
                unit_copper: copper,
                bought,
                quoted: 0,
                last_bought: at,
            }],
        }
    }

    /// A writer that has never heard of a column must not be able to erase it.
    ///
    /// The reason this is a test and not a comment: for one afternoon a
    /// leftover daemon from a previous build sat resetting `quoted` to 0 on
    /// every row it touched, while a newly built one wrote it correctly.
    /// Nothing errored, nothing conflicted, and the count simply never moved.
    /// The statement is confined to the columns it names now, so a stale
    /// writer is out of date rather than destructive.
    #[test]
    fn an_older_writer_cannot_blank_a_column_it_does_not_know() {
        let mut s = Store::in_memory().unwrap();
        // The same identity put_vendors writes under, or the stale-writer
        // statement below lands on a different row and proves nothing.
        let (who, server) = {
            let (c, sv) = s.identity();
            (c.to_string(), sv.to_string())
        };

        // A current writer records a price it was quoted but never bought.
        let mut quoted_only = price("Elf Leaf", "Isslana", "The Oasis of Marr", 500, 0, 10);
        quoted_only.quoted = 3;
        quoted_only.vendors[0].quoted = 3;
        s.put_vendors("everquest-legends", "/logs/a.txt", &[quoted_only])
            .unwrap();

        // Exactly what a build predating the column writes: the same row,
        // every column it knows about, and no mention of `quoted`.
        s.conn
            .execute(
                "INSERT INTO item_vendors
                   (game, character, server, source, item, merchant, zone,
                    unit_copper, bought, last_bought)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(game, character, server, source, item, merchant)
                 DO UPDATE SET
                   zone = excluded.zone,
                   unit_copper = excluded.unit_copper,
                   bought = excluded.bought,
                   last_bought = excluded.last_bought",
                params![
                    "everquest-legends",
                    who,
                    server,
                    "/logs/a.txt",
                    "Elf Leaf",
                    "Isslana",
                    "The Oasis of Marr",
                    500i64,
                    0i64,
                    20i64
                ],
            )
            .unwrap();

        let rows: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM item_vendors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the stale write should have hit the same row");
        let (quoted, last): (i64, i64) = s
            .conn
            .query_row(
                "SELECT quoted, last_bought FROM item_vendors WHERE item = 'Elf Leaf'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            quoted, 3,
            "an older writer erased a column it never knew about"
        );
        assert_eq!(
            last, 20,
            "the columns it does know about should still update"
        );
    }

    /// Merchant prices have to survive the log they came from. Re-reading one
    /// log replaces its own rows; a second log adds to them. Getting this
    /// wrong either doubles the totals or throws away everything but the log
    /// currently open.
    #[test]
    fn vendor_prices_accumulate_across_logs_and_survive_a_reparse() {
        let mut s = Store::in_memory().unwrap();
        let log_a = "/logs/eqlog_Tharn_freeport.txt";
        let log_b = "/logs/eqlog_Alt_freeport.txt";

        s.put_vendors(
            "everquest-legends",
            log_a,
            &[price(
                "Duskglow Vine",
                "Sithgok",
                "Innothule Swamp",
                654,
                1000,
                100,
            )],
        )
        .unwrap();
        // The same log read again: same keys, so the row is replaced.
        s.put_vendors(
            "everquest-legends",
            log_a,
            &[price(
                "Duskglow Vine",
                "Sithgok",
                "Innothule Swamp",
                654,
                1000,
                100,
            )],
        )
        .unwrap();

        let v = s.all_vendors().unwrap();
        assert_eq!(v.len(), 1, "a re-parse is not a second set of purchases");
        assert_eq!(v[0].5, 1000, "still a thousand, not two");

        // A different log is genuinely more data, and adds.
        s.put_vendors(
            "everquest-legends",
            log_b,
            &[price(
                "Duskglow Vine",
                "Sithgok",
                "Innothule Swamp",
                700,
                500,
                200,
            )],
        )
        .unwrap();
        let v = s.all_vendors().unwrap();
        assert_eq!(v.len(), 1, "one merchant, one item");
        assert_eq!(v[0].5, 1500, "both logs' purchases");
        assert_eq!(v[0].4, 700, "the price as of the most recent purchase");
        assert_eq!(v[0].3, "Innothule Swamp");
    }

    /// Two games are two datasets. The same merchant name in each must not be
    /// pooled, because the price need not be the same.
    #[test]
    fn vendor_prices_are_not_pooled_across_games() {
        let mut s = Store::in_memory().unwrap();
        s.put_vendors(
            "everquest",
            "/logs/a.txt",
            &[price(
                "Water Flask",
                "Clork",
                "Innothule Swamp",
                10,
                100,
                100,
            )],
        )
        .unwrap();
        s.put_vendors(
            "everquest-legends",
            "/logs/b.txt",
            &[price(
                "Water Flask",
                "Clork",
                "Innothule Swamp",
                99,
                100,
                100,
            )],
        )
        .unwrap();

        let v = s.all_vendors().unwrap();
        assert_eq!(v.len(), 2, "one row per game: {v:?}");
        let games: Vec<&str> = v.iter().map(|r| r.0.as_str()).collect();
        assert!(games.contains(&"everquest") && games.contains(&"everquest-legends"));
    }

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
        s.put_kills(&[kill(1000, "Lord Nagafen", "WAR/DRU/BRD")])
            .unwrap();
        s.put_kills(&[kill(1000, "Lord Nagafen", "WAR/DRU/BRD")])
            .unwrap();
        assert_eq!(s.kills().unwrap().len(), 1);
        assert_eq!(s.counts().unwrap().0, 1);
    }

    /// A later parse that knows more must improve the row, not be ignored:
    /// the build label depends on evidence that may only arrive afterwards.
    #[test]
    fn a_later_parse_replaces_a_row_with_better_information() {
        let mut s = Store::in_memory().unwrap();
        s.put_kills(&[kill(1000, "Lord Nagafen", "")]).unwrap();
        s.put_kills(&[kill(1000, "Lord Nagafen", "WAR/DRU/BRD")])
            .unwrap();
        let k = s.kills().unwrap();
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].build, "WAR/DRU/BRD");
    }

    /// Two bosses dying in the same second are different kills.
    #[test]
    fn kills_at_the_same_instant_are_kept_apart_by_name() {
        let mut s = Store::in_memory().unwrap();
        s.put_kills(&[kill(1000, "Lord Nagafen", "x"), kill(1000, "Lady Vox", "x")])
            .unwrap();
        assert_eq!(s.kills().unwrap().len(), 2);
    }

    #[test]
    fn history_survives_reopening_the_database() {
        let dir = std::env::temp_dir().join("norrlog-store-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("history.db");
        {
            let mut s = Store::open_as(&path, "Tharn", "freeport").unwrap();
            s.put_kills(&[kill(1000, "Cazic-Thule", "WAR/DRU/BRD")])
                .unwrap();
        }
        let s = Store::open_as(&path, "Tharn", "freeport").unwrap();
        assert_eq!(s.kills().unwrap()[0].boss, "Cazic-Thule");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A history written before the AA table carried a game gets one filled
    /// in from what each character's *own* kills say — never from whichever
    /// log happens to be open when the migration runs.
    #[test]
    fn the_aa_game_is_backfilled_from_evidence() {
        let dir = std::env::temp_dir().join("norrlog-aa-game-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("history.db");

        {
            let s = Store::open_as(&path, "Orrin", "teek").unwrap();
            // Two characters' purchases, and kills that say which game each
            // of them plays. A third character has purchases and no kills:
            // nothing can speak for it.
            s.conn
                .execute_batch(
                    "
                INSERT INTO npc_kills VALUES
                    ('everquest-legends','Tharn','freeport','a.txt','a bat',1),
                    ('everquest','Orrin','teek','b.txt','a rat',1);
                INSERT INTO aa_purchases VALUES
                    ('','Tharn','freeport','Unbound Cascade',1,0,10),
                    ('','Orrin','teek','Mental Clarity',1,2,20),
                    ('','Ghost','nowhere','Innate Run Speed',1,1,30);
                -- Back to the shape it had before the game was a column.
                CREATE TABLE aa_old (
                    character TEXT NOT NULL, server TEXT NOT NULL,
                    ability TEXT NOT NULL, rank INTEGER NOT NULL,
                    spent INTEGER NOT NULL DEFAULT 0, at INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (character, server, ability, rank)
                );
                INSERT INTO aa_old
                    SELECT character, server, ability, rank, spent, at FROM aa_purchases;
                DROP TABLE aa_purchases;
                ALTER TABLE aa_old RENAME TO aa_purchases;
                ",
                )
                .unwrap();
        }

        // Reopening as the EverQuest character runs the migration. The
        // Legends rows must not become EverQuest ones just because that is
        // who opened the database.
        let s = Store::open_as(&path, "Orrin", "teek").unwrap();
        let mut stmt = s
            .conn
            .prepare("SELECT game FROM aa_purchases WHERE character = ?1")
            .unwrap();
        let mut game_of = |who: &str| -> String {
            stmt.query_row(params![who], |r| r.get::<_, String>(0))
                .unwrap()
        };
        assert_eq!(game_of("Tharn"), "everquest-legends");
        assert_eq!(game_of("Orrin"), "everquest");
        // Nothing could speak for this one, so it says nothing rather than
        // claiming the game of the log that opened the database.
        assert_eq!(game_of("Ghost"), "");
        drop(stmt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two characters sharing one database must not share one history.
    #[test]
    fn characters_do_not_see_each_others_history() {
        let dir = std::env::temp_dir().join("norrlog-identity-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("history.db");

        let mut a = Store::open_as(&path, "Tharn", "freeport").unwrap();
        a.put_kills(&[StoredKill {
            timestamp: 100,
            boss: "Innoruuk".into(),
            difficulty: 4,
            zone: "Plane of Hate".into(),
            solo: false,
            build: "WAR/BRD/DRU".into(),
            duration: 300,
            your_damage: 24_274,
            your_dps: 80.9,
        }])
        .unwrap();
        a.put_sky_turnins(&[("Nature".to_string(), 100)]).unwrap();
        drop(a);

        let mut b = Store::open_as(&path, "Alt", "freeport").unwrap();
        assert_eq!(b.kills().unwrap().len(), 0, "an alt starts empty");
        assert_eq!(b.sky_turnin_counts().unwrap().len(), 0);
        b.put_kills(&[StoredKill {
            timestamp: 200,
            boss: "Cazic Thule".into(),
            difficulty: 1,
            zone: "Plane of Fear".into(),
            solo: true,
            build: "ROG".into(),
            duration: 60,
            your_damage: 10,
            your_dps: 1.0,
        }])
        .unwrap();
        // Same character name on a different server is a different character.
        let c = Store::open_as(&path, "Tharn", "other").unwrap();
        assert_eq!(c.kills().unwrap().len(), 0);
        drop(b);
        drop(c);

        let a = Store::open_as(&path, "Tharn", "freeport").unwrap();
        let kills = a.kills().unwrap();
        assert_eq!(kills.len(), 1);
        assert_eq!(kills[0].boss, "Innoruuk");
        assert_eq!(a.counts().unwrap().0, 1);
        assert_eq!(a.sky_turnin_counts().unwrap().len(), 1);

        let mut seen = a.characters().unwrap();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("Alt".to_string(), "freeport".to_string(), 1),
                ("Tharn".to_string(), "freeport".to_string(), 1),
            ]
        );
        drop(a);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A log read again from its start replaces its span: the fight titled
    /// after you, and the one you were never in, go; a fight outside the
    /// span, from another log, stays.
    #[test]
    fn a_whole_read_replaces_its_span_of_fights() {
        let fight = |start: i64, title: &str| StoredEncounter {
            start,
            end: start + 10,
            title: title.into(),
            zone: "Guild Lobby".into(),
            duration: 10,
            your_damage: 0,
            your_dps: 0.0,
            total_damage: 10,
            kills: 0,
            deaths: 0,
            build: String::new(),
            difficulty: 0,
        };
        let mut s = Store::in_memory().unwrap();
        s.put_encounters(&[
            fight(50, "a rat"),
            fight(100, "Unknown encounter"),
            fight(200, "Tharn"),
            fight(300, "a bat"),
        ])
        .unwrap();
        s.replace_encounters(&[fight(200, "a goblin"), fight(300, "a bat")])
            .unwrap();
        let mut got: Vec<(i64, String)> = s
            .conn
            .prepare("SELECT start, title FROM encounters ORDER BY start")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                (50, "a rat".to_string()),
                (100, "Unknown encounter".to_string()),
                (200, "a goblin".to_string()),
                (300, "a bat".to_string()),
            ],
            "100 is before this read's first fight, so it is not this read's to judge"
        );
        s.replace_encounters(&[fight(100, "a goblin"), fight(300, "a bat")])
            .unwrap();
        let n: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM encounters WHERE title IN ('Unknown encounter', 'Tharn')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "both stale rows sat inside the span");
    }

    fn a_fight(start: i64, title: &str) -> StoredEncounter {
        StoredEncounter {
            start,
            end: start + 10,
            title: title.into(),
            zone: "Guild Lobby".into(),
            duration: 10,
            your_damage: 0,
            your_dps: 0.0,
            total_damage: 10,
            kills: 0,
            deaths: 0,
            build: String::new(),
            difficulty: 0,
        }
    }

    fn fights(s: &Store) -> Vec<(String, i64, String)> {
        s.conn
            .prepare("SELECT character, start, title FROM encounters ORDER BY character, start")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The delete and the inserts were two commits: an insert that failed
    /// left the old fights deleted, and nothing brought them back.
    #[test]
    fn a_replacement_that_fails_keeps_the_fights_it_would_have_replaced() {
        let mut s = Store::in_memory().unwrap();
        s.put_encounters(&[a_fight(100, "a rat"), a_fight(200, "a bat")])
            .unwrap();
        let before = fights(&s);
        // Fault injection: every insert into encounters now fails.
        s.conn
            .execute_batch(
                "CREATE TRIGGER refuse BEFORE INSERT ON encounters
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        assert!(s
            .replace_encounters(&[a_fight(100, "a goblin"), a_fight(200, "a bat")])
            .is_err());
        assert_eq!(fights(&s), before, "the failed replacement deleted fights");
    }

    /// A replacement that succeeds changes only its own span, for its own
    /// character: another character's fight at the same moment stays.
    #[test]
    fn a_replacement_touches_only_its_own_span_and_character() {
        let mut s = Store::in_memory().unwrap();
        let me = s.character.clone();
        s.character = "Someone".into();
        s.put_encounters(&[a_fight(150, "their fight")]).unwrap();
        s.character = me.clone();
        s.put_encounters(&[
            a_fight(50, "before"),
            a_fight(150, "stale"),
            a_fight(400, "after"),
        ])
        .unwrap();
        s.replace_encounters(&[a_fight(100, "a goblin"), a_fight(300, "a bat")])
            .unwrap();
        let mut want = vec![
            (me.clone(), 50, "before".to_string()),
            (me.clone(), 100, "a goblin".to_string()),
            (me.clone(), 300, "a bat".to_string()),
            (me.clone(), 400, "after".to_string()),
            ("Someone".to_string(), 150, "their fight".to_string()),
        ];
        want.sort();
        assert_eq!(fights(&s), want);
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
                difficulty: 0,
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
                difficulty: 0,
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

/// Split a `|`-packed column back into its parts.
///
/// Empty means empty, not one empty string — which for a player's classes is
/// the difference between "[ANONYMOUS], no classes stated" and "one class,
/// named the empty string".
fn split_packed(packed: &str) -> Vec<String> {
    if packed.is_empty() {
        return Vec::new();
    }
    packed.split('|').map(str::to_string).collect()
}

#[cfg(test)]
mod forage_tests {
    use super::*;

    fn record_over(text: &str) -> crate::record::Settled {
        let mut e = crate::engine::Engine::with_game(
            "Tharn".to_string(),
            norrlog_core::spell::SpellDb::empty(),
            norrlog_core::game::Game::EverQuestLegends,
        );
        for line in text.lines() {
            e.push_line(line);
        }
        e.record()
    }

    const FORAGING: &str = "\
[Tue Jul 28 15:03:10 2026] You have entered Everfrost Peaks.
[Tue Jul 28 15:03:12 2026] You have scrounged up Pod of Water.
[Tue Jul 28 15:03:20 2026] You have scrounged up Pod of Water.
[Tue Jul 28 15:03:28 2026] You fail to locate any food nearby.
[Tue Jul 28 16:00:00 2026] You have entered Nagafen's Lair.
[Tue Jul 28 16:00:10 2026] You fail to locate any food nearby.
[Tue Jul 28 16:00:20 2026] You fail to locate any food nearby.
";

    #[test]
    fn the_forage_record_survives_a_round_trip() {
        let mut st = Store::in_memory().unwrap();
        st.put_record(Game::EverQuestLegends, "log-a", &record_over(FORAGING))
            .unwrap();
        let r = st.record_reports(Game::EverQuestLegends).unwrap();

        let ef = r
            .forage
            .zones
            .iter()
            .find(|z| z.zone == "Everfrost Peaks")
            .unwrap();
        assert_eq!((ef.hits, ef.empty, ef.attempts), (2, 1, 3));
        assert_eq!(ef.items.len(), 1);
        assert_eq!(ef.items[0].item, "Pod of Water");
        assert_eq!(ef.items[0].times, 2);
    }

    /// A zone somebody foraged in and got nothing from is a fact about the
    /// zone, so it has to come back as a row rather than as an absence.
    #[test]
    fn a_zone_that_yielded_nothing_still_comes_back() {
        let mut st = Store::in_memory().unwrap();
        st.put_record(Game::EverQuestLegends, "log-a", &record_over(FORAGING))
            .unwrap();
        let r = st.record_reports(Game::EverQuestLegends).unwrap();

        let nl = r
            .forage
            .zones
            .iter()
            .find(|z| z.zone == "Nagafen's Lair")
            .unwrap();
        assert_eq!((nl.hits, nl.empty, nl.attempts), (0, 2, 2));
        assert!(nl.items.is_empty());
    }

    /// Two logs of the same zone add up rather than replacing each other.
    #[test]
    fn two_logs_pool_into_one_zone() {
        let mut st = Store::in_memory().unwrap();
        st.put_record(Game::EverQuestLegends, "log-a", &record_over(FORAGING))
            .unwrap();
        st.put_record(Game::EverQuestLegends, "log-b", &record_over(FORAGING))
            .unwrap();
        let r = st.record_reports(Game::EverQuestLegends).unwrap();

        let ef = r
            .forage
            .zones
            .iter()
            .find(|z| z.zone == "Everfrost Peaks")
            .unwrap();
        assert_eq!((ef.hits, ef.empty, ef.attempts), (4, 2, 6));
        assert_eq!(ef.items.len(), 1, "the same item from two logs is one row");
        assert_eq!(ef.items[0].times, 4);
    }

    /// Re-reading one log replaces its own rows and nothing else's.
    #[test]
    fn a_second_pass_over_the_same_log_does_not_double_it() {
        let mut st = Store::in_memory().unwrap();
        st.put_record(Game::EverQuestLegends, "log-a", &record_over(FORAGING))
            .unwrap();
        st.put_record(Game::EverQuestLegends, "log-a", &record_over(FORAGING))
            .unwrap();
        let r = st.record_reports(Game::EverQuestLegends).unwrap();

        let ef = r
            .forage
            .zones
            .iter()
            .find(|z| z.zone == "Everfrost Peaks")
            .unwrap();
        assert_eq!((ef.hits, ef.empty, ef.attempts), (2, 1, 3));
    }
}

#[cfg(test)]
mod notice_repair_tests {
    use super::*;

    const BIND: &str = "an area where Bind Affinity is allowed";
    const LOG: &str = "/logs/eqlog_Test_test.txt";

    fn profile(s: &Store, source: &str, zone: &str, npc: &str, kills: i64, span: (i64, i64)) {
        s.conn
            .execute(
                "INSERT INTO npc_profiles (game, character, server, source, zone, mode,
                     difficulty, npc, kills, deaths_to_it, max_hit, first_seen, last_seen, kind)
                 VALUES ('everquest-legends','Test','test',?1,?2,'open',0,?3,?4,0,0,?5,?6,'')",
                params![source, zone, npc, kills, span.0, span.1],
            )
            .unwrap();
    }

    fn sample(s: &Store, source: &str, zone: &str, npc: &str, at: i64) {
        s.conn
            .execute(
                "INSERT INTO npc_health_samples (game, character, server, source, zone, mode,
                     difficulty, npc, at, hp)
                 VALUES ('everquest-legends','Test','test',?1,?2,'open',0,?3,?4,100)",
                params![source, zone, npc, at],
            )
            .unwrap();
    }

    fn zones(s: &Store, table: &str) -> Vec<(String, i64)> {
        let mut st = s
            .conn
            .prepare(&format!(
                "SELECT zone, COUNT(*) FROM {table} GROUP BY zone ORDER BY zone"
            ))
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn repair(s: &mut Store) {
        s.repair_rule_notice_zones().unwrap();
    }

    /// The SQL and the Rust say the same thing about every shape.
    #[test]
    fn the_sql_test_agrees_with_is_rule_notice() {
        let s = Store::in_memory().unwrap();
        for z in [
            BIND,
            "an area where levitation effects do not function",
            "a zone where x",
            "an zone y",
            "a area z",
            "West Commonlands",
            "Arena",
            "an",
            "an area",
            "An area where capitals differ",
            "",
        ] {
            let sql: bool = s
                .conn
                .query_row(
                    "SELECT ?1 GLOB 'an area *' OR ?1 GLOB 'a area *'
                         OR ?1 GLOB 'an zone *' OR ?1 GLOB 'a zone *'",
                    params![z],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(sql, norrlog_core::zone::is_rule_notice(z), "{z:?}");
        }
    }

    /// A stretch read again since the fix is on file twice; the copy under
    /// the notice goes, and the real one is left exactly as it was.
    #[test]
    fn a_stretch_read_again_loses_its_duplicate() {
        let mut s = Store::in_memory().unwrap();
        profile(&s, LOG, BIND, "black bear", 7, (100, 200));
        sample(&s, LOG, BIND, "black bear", 150);
        profile(&s, LOG, "West Commonlands", "black bear", 9, (50, 300));
        sample(&s, LOG, "West Commonlands", "black bear", 150);
        repair(&mut s);
        assert_eq!(
            zones(&s, "npc_profiles"),
            vec![("West Commonlands".into(), 1)]
        );
        assert_eq!(
            zones(&s, "npc_health_samples"),
            vec![("West Commonlands".into(), 1)]
        );
        let kills: i64 = s
            .conn
            .query_row("SELECT kills FROM npc_profiles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kills, 9);
    }

    /// A stretch never read again is the only record of those kills. They
    /// stay, under no zone, rather than under a place that does not exist.
    #[test]
    fn a_stretch_not_read_again_is_kept_under_no_zone() {
        let mut s = Store::in_memory().unwrap();
        profile(&s, LOG, BIND, "black bear", 7, (100, 200));
        sample(&s, LOG, BIND, "black bear", 150);
        // The same creature elsewhere in the log, at other times: not a
        // re-read, and not a reason to throw the seven kills away.
        profile(&s, LOG, "East Commonlands", "black bear", 2, (400, 500));
        repair(&mut s);
        assert_eq!(
            zones(&s, "npc_profiles"),
            vec![(String::new(), 1), ("East Commonlands".into(), 1)]
        );
        assert_eq!(zones(&s, "npc_health_samples"), vec![(String::new(), 1)]);
        let kills: i64 = s
            .conn
            .query_row("SELECT kills FROM npc_profiles WHERE zone = ''", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kills, 7);
    }

    /// One log's re-read says nothing about another's.
    #[test]
    fn each_log_is_judged_on_its_own() {
        let mut s = Store::in_memory().unwrap();
        let other = "/logs/eqlog_Alt_test.txt";
        profile(&s, LOG, BIND, "black bear", 7, (100, 200));
        profile(&s, LOG, "West Commonlands", "black bear", 9, (50, 300));
        profile(&s, other, BIND, "black bear", 3, (100, 200));
        repair(&mut s);
        let mut st = s
            .conn
            .prepare("SELECT source, zone FROM npc_profiles ORDER BY source, zone")
            .unwrap();
        let rows: Vec<(String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![
                (other.into(), String::new()),
                (LOG.into(), "West Commonlands".into()),
            ]
        );
    }

    /// A fight overlapped by one in a real zone is that fight twice; one
    /// nothing overlaps keeps its numbers and loses the place.
    #[test]
    fn fights_under_a_notice_are_dropped_or_unplaced() {
        let mut s = Store::in_memory().unwrap();
        let fight = |start: i64, end: i64, title: &str, zone: &str| {
            s.conn
                .execute(
                    "INSERT INTO encounters (character, server, start, end, title, zone)
                     VALUES ('Test','test',?1,?2,?3,?4)",
                    params![start, end, title, zone],
                )
                .unwrap();
        };
        fight(100, 200, "Bzzzt (+2 more)", BIND);
        fight(100, 150, "Bzzzt", "The Plane of Sky");
        fight(500, 508, "Unknown encounter", BIND);
        repair(&mut s);
        let mut st = s
            .conn
            .prepare("SELECT title, zone FROM encounters ORDER BY start")
            .unwrap();
        let rows: Vec<(String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![
                ("Bzzzt".into(), "The Plane of Sky".into()),
                ("Unknown encounter".into(), String::new()),
            ]
        );
    }

    /// A set of places loses the notice and keeps the rest.
    #[test]
    fn a_drop_forgets_the_notice_among_its_zones() {
        let mut s = Store::in_memory().unwrap();
        s.conn
            .execute(
                "INSERT INTO item_drops (game, character, server, source, item, npc, zones,
                     looted, quantity, first_seen, last_seen)
                 VALUES ('everquest-legends','Test','test',?1,'Giant Snake Fang',
                         'darkweed snake',?2,1,1,1,1)",
                params![LOG, format!("East Commonlands|{BIND}")],
            )
            .unwrap();
        repair(&mut s);
        let z: String = s
            .conn
            .query_row("SELECT zones FROM item_drops", [], |r| r.get(0))
            .unwrap();
        assert_eq!(z, "East Commonlands");
    }

    /// Opening a repaired history again changes nothing.
    #[test]
    fn the_repair_is_idempotent() {
        let mut s = Store::in_memory().unwrap();
        profile(&s, LOG, BIND, "black bear", 7, (100, 200));
        profile(&s, LOG, "West Commonlands", "black bear", 9, (50, 300));
        profile(&s, "/logs/b.txt", BIND, "orc pawn", 1, (1, 2));
        repair(&mut s);
        let once = (zones(&s, "npc_profiles"), zones(&s, "encounters"));
        repair(&mut s);
        assert_eq!((zones(&s, "npc_profiles"), zones(&s, "encounters")), once);
    }
}

#[cfg(test)]
mod graded_duplicate_tests {
    use super::*;

    const LOG: &str = "/logs/eqlog_Test_test.txt";

    fn put_drop(s: &Store, item: &str, looted: i64, span: (i64, i64)) {
        s.conn
            .execute(
                "INSERT INTO item_drops (game, character, server, source, item, npc, zones,
                     looted, quantity, first_seen, last_seen)
                 VALUES ('everquest-legends','Test','test',?1,?2,'Bazzt Zzzt',
                         'The Plane of Sky',?3,?3,?4,?5)",
                params![LOG, item, looted, span.0, span.1],
            )
            .unwrap();
    }

    fn places(s: &Store, item: &str, looted: i64) {
        s.conn
            .execute(
                "INSERT INTO item_drop_places (game, character, server, source, item, npc,
                     zone, mode, difficulty, looted, quantity)
                 VALUES ('everquest-legends','Test','test',?1,?2,'Bazzt Zzzt',
                         'The Plane of Sky','open',0,?3,?3)",
                params![LOG, item, looted],
            )
            .unwrap();
    }

    fn looted(s: &Store) -> u64 {
        s.all_drops()
            .unwrap()
            .iter()
            .filter(|d| d.item.starts_with("White Satin Gloves"))
            .map(|d| d.looted)
            .sum()
    }

    /// The case on file: one +1 loot from before the fold, and the same log
    /// read again since, counting it among four. Four, not five.
    #[test]
    fn a_graded_row_read_again_is_not_counted_twice() {
        let mut s = Store::in_memory().unwrap();
        put_drop(&s, "White Satin Gloves +1", 1, (150, 150));
        put_drop(&s, "White Satin Gloves", 4, (100, 300));
        places(&s, "White Satin Gloves", 4);
        assert_eq!(looted(&s), 5, "the double count this repairs");
        s.drop_graded_duplicates().unwrap();
        assert_eq!(looted(&s), 4);
    }

    /// From before the fold, an ungraded row is the loots with no roll and
    /// the graded row is the rest. No places, so both stay.
    #[test]
    fn rows_from_before_the_fold_are_both_kept() {
        let mut s = Store::in_memory().unwrap();
        put_drop(&s, "White Satin Gloves +1", 1, (150, 150));
        put_drop(&s, "White Satin Gloves", 4, (100, 300));
        s.drop_graded_duplicates().unwrap();
        assert_eq!(looted(&s), 5);
    }

    /// A later read that began after the graded loot did not count it.
    #[test]
    fn a_read_that_did_not_cover_it_keeps_it() {
        let mut s = Store::in_memory().unwrap();
        put_drop(&s, "White Satin Gloves +1", 1, (50, 50));
        put_drop(&s, "White Satin Gloves", 4, (100, 300));
        places(&s, "White Satin Gloves", 4);
        s.drop_graded_duplicates().unwrap();
        assert_eq!(looted(&s), 5);
    }

    /// The open that repairs leaves a copy from before; an open with nothing
    /// to repair leaves none.
    #[test]
    fn a_repair_is_backed_up_once() {
        let dir = std::env::temp_dir().join(format!("norrlog-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("history.db");
        let copies = || {
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains(".before-repair-"))
                .count()
        };
        {
            let s = Store::open_as(&db, "Test", "test").unwrap();
            assert_eq!(copies(), 0, "a new history has nothing to repair");
            put_drop(&s, "White Satin Gloves +1", 1, (150, 150));
            put_drop(&s, "White Satin Gloves", 4, (100, 300));
            places(&s, "White Satin Gloves", 4);
        }
        let s = Store::open_as(&db, "Test", "test").unwrap();
        assert_eq!(copies(), 1);
        assert_eq!(looted(&s), 4);
        drop(s);
        let s = Store::open_as(&db, "Test", "test").unwrap();
        assert_eq!(copies(), 1, "nothing left to repair, so no second copy");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod inferred_group_mode_tests {
    use super::*;

    fn profile(s: &Store, source: &str, mode: &str, npc: &str, kills: i64, seen: (i64, i64)) {
        s.conn
            .execute(
                "INSERT INTO npc_profiles (game, character, server, source, zone, mode,
                    difficulty, npc, kills, deaths_to_it, max_hit, first_seen, last_seen)
                 VALUES ('legends', 'Test', 'test', ?1, 'Nagafen''s Lair', ?2, 2, ?3, ?4, 0,
                    40, ?5, ?6)",
                params![source, mode, npc, kills, seen.0, seen.1],
            )
            .unwrap();
    }

    fn sample(s: &Store, source: &str, mode: &str, at: i64) {
        s.conn
            .execute(
                "INSERT INTO npc_health_samples (game, character, server, source, zone, mode,
                    difficulty, npc, at, hp)
                 VALUES ('legends', 'Test', 'test', ?1, 'Nagafen''s Lair', ?2, 2,
                    'a lava drake', ?3, 900)",
                params![source, mode, at],
            )
            .unwrap();
    }

    fn rows(s: &Store, sql: &str) -> Vec<(String, String, i64)> {
        let mut st = s.conn.prepare(sql).unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// An unmarked log's group rows become instance rows, a twin counted
    /// once at the larger count; a marked log and a missing one keep theirs.
    #[test]
    fn inferred_group_rows_are_refiled_as_instance() {
        let dir = std::env::temp_dir().join(format!("norrlog-groupmode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let unmarked = dir.join("eqlog_A_test.txt");
        let marked = dir.join("eqlog_B_test.txt");
        let gone = dir.join("eqlog_C_test.txt");
        std::fs::write(
            &unmarked,
            "[Sat Aug 16 10:00:00 2025] You have entered Nagafen's Lair 2.\n\
             [Sat Aug 16 10:00:05 2025] Grimble tells the group, 'Group up - Group 2 is here'\n",
        )
        .unwrap();
        std::fs::write(
            &marked,
            "[Sat Aug 16 10:00:00 2025] You have entered Nagafen's Lair - Group 2.\n",
        )
        .unwrap();
        let (a, b, c) = (
            unmarked.to_string_lossy().to_string(),
            marked.to_string_lossy().to_string(),
            gone.to_string_lossy().to_string(),
        );
        let db = dir.join("history.db");
        {
            let s = Store::open_as(&db, "Test", "test").unwrap();
            // Read twice: the first read counted one kill more.
            profile(&s, &a, "group", "a wan ghoul knight", 113, (100, 200));
            profile(&s, &a, "instance", "a wan ghoul knight", 112, (100, 200));
            // Read once, under the inference only.
            profile(&s, &a, "group", "a lava drake", 5, (150, 160));
            sample(&s, &a, "group", 150);
            sample(&s, &a, "instance", 150);
            sample(&s, &a, "group", 155);
            profile(&s, &b, "group", "a lava drake", 3, (100, 110));
            profile(&s, &c, "group", "a lava drake", 2, (100, 110));
        }
        let s = Store::open_as(&db, "Test", "test").unwrap();
        let kills = rows(
            &s,
            "SELECT npc, mode, kills FROM npc_profiles WHERE source = ?1 ORDER BY npc"
                .replace("?1", &format!("'{a}'"))
                .as_str(),
        );
        assert_eq!(
            kills,
            vec![
                ("a lava drake".into(), "instance".into(), 5),
                ("a wan ghoul knight".into(), "instance".into(), 113),
            ]
        );
        let samples = rows(
            &s,
            &format!(
                "SELECT npc, mode, at FROM npc_health_samples WHERE source = '{a}' ORDER BY at"
            ),
        );
        assert_eq!(
            samples,
            vec![
                ("a lava drake".into(), "instance".into(), 150),
                ("a lava drake".into(), "instance".into(), 155),
            ]
        );
        for (src, why) in [(&b, "it says - Group"), (&c, "it is not on disk")] {
            let left = rows(
                &s,
                &format!("SELECT npc, mode, kills FROM npc_profiles WHERE source = '{src}'"),
            );
            assert_eq!(left[0].1, "group", "kept because {why}");
        }
        let copies = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".before-repair-"))
            .count();
        assert_eq!(copies, 1, "backed up before the repair");
        drop(s);
        // Once is enough: the next open neither scans nor copies again.
        let _s = Store::open_as(&db, "Test", "test").unwrap();
        let copies = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".before-repair-"))
            .count();
        assert_eq!(copies, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
