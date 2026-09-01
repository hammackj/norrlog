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
        Ok(s)
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
            -- Figures the player read off their own character sheet, which
            -- the log cannot always produce. Keyed by name so new ones need no
            -- migration.
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

            -- Hand-authored quest write-ups. JSON rather than columns: the
            -- content is prose and its shape changes as a quest is
            -- understood, which a fixed schema would only get in the way of.
            CREATE TABLE IF NOT EXISTS quests (
                name    TEXT PRIMARY KEY,
                data    TEXT NOT NULL,
                updated INTEGER NOT NULL DEFAULT 0
            );

            -- Alert rules. JSON for the same reason as quests: the trigger is
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
                quantity  INTEGER NOT NULL,
                first_seen INTEGER NOT NULL,
                last_seen  INTEGER NOT NULL,
                PRIMARY KEY (game, character, server, source, item, npc)
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
            ",
        )?;
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
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
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
        }
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

    /// Record a figure the player stated.
    pub fn put_stated(&mut self, key: &str, value: i64, at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO stated_values (character, server, key, value, at)
             VALUES (?1,?2,?3,?4,?5)",
            params![self.character, self.server, key, value, at],
        )?;
        Ok(())
    }

    pub fn stated(&self, key: &str) -> Result<Option<i64>> {
        let v = self
            .conn
            .query_row(
                "SELECT value FROM stated_values
                 WHERE character = ?1 AND server = ?2 AND key = ?3",
                params![self.character, self.server, key],
                |r| r.get(0),
            )
            .ok();
        Ok(v)
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
    /// Write a quest only if its name is new, leaving an existing one
    /// alone. Nothing calls this: `save_quest` upserts, which is what the
    /// import path wants. Kept because the two have genuinely different
    /// semantics and the choice will come back the first time a crawl and a
    /// hand edit disagree.
    #[allow(dead_code)]
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

    /// One quest by name. Nothing calls this — the server reads them all
    /// and filters — but a single-quest read is the obvious next need and
    /// the query is trivial.
    #[allow(dead_code)]
    pub fn quest(&self, name: &str) -> Result<Option<QuestRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM quests WHERE name = ?1")?;
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

    /// Write an alert rule, replacing any existing rule of the same id.
    pub fn put_alert(&mut self, rule: &eqlparser_core::alert::AlertRule, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO alerts (id, data, updated) VALUES (?1,?2,?3)",
            params![rule.id, serde_json::to_string(rule)?, now],
        )?;
        Ok(())
    }

    /// Every alert rule, oldest first so the list keeps the order they were
    /// written in rather than reshuffling on each load.
    pub fn alerts(&self) -> Result<Vec<eqlparser_core::alert::AlertRule>> {
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
    /// Rows are keyed by log, so this REPLACEs only what the same log said
    /// last time. Re-parsing changes nothing; a different log adds to the
    /// pile rather than overwriting it.
    pub fn put_drops(
        &mut self,
        game: &str,
        source: &str,
        report: &eqlparser_core::drops::DropReport,
    ) -> Result<usize> {
        let (c, sv) = (self.character.clone(), self.server.clone());
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut drop_stmt = tx.prepare(
                "INSERT OR REPLACE INTO item_drops
                 (game, character, server, source, item, npc, zones, looted, quantity,
                  first_seen, last_seen)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
                        s.quantity as i64,
                        s.first_seen,
                        s.last_seen,
                    ])?;
                    n += 1;
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

    /// Every drop observation on file, summed across logs and characters.
    ///
    /// Returns `(game, item, npc, zones, looted, quantity, first_seen,
    /// last_seen)`. The game stays in the key: totals are per game, never
    /// pooled across the two.
    #[allow(clippy::type_complexity)]
    pub fn all_drops(
        &self,
    ) -> Result<Vec<(String, String, String, Vec<String>, u64, u64, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT game, item, npc, group_concat(zones, '|'),
                    SUM(looted), SUM(quantity), MIN(first_seen), MAX(last_seen)
             FROM item_drops GROUP BY game, lower(item), lower(npc)
             ORDER BY SUM(looted) DESC",
        )?;
        let rows: Vec<(String, String, String, Vec<String>, u64, u64, i64, i64)> = stmt
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
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    zones,
                    r.get::<_, i64>(4)? as u64,
                    r.get::<_, i64>(5)? as u64,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // Drops are keyed on the ungraded name now, but rows written before
        // that are still on file under "Woven Skull Cap +1" and SQL groups
        // them apart. Folding on the way out as well as on the way in means
        // the history reads correctly without rewriting it: every stored
        // observation stays exactly as it was recorded.
        let mut folded: std::collections::HashMap<(String, String, String), usize> =
            std::collections::HashMap::new();
        let mut out: Vec<(String, String, String, Vec<String>, u64, u64, i64, i64)> = Vec::new();
        for (game, item, npc, zones, looted, quantity, first, last) in rows {
            let base = eqlparser_core::drops::canonical_item(&item).to_string();
            let key = (game.clone(), base.to_lowercase(), npc.to_lowercase());
            match folded.get(&key) {
                Some(&idx) => {
                    let e = &mut out[idx];
                    e.3.extend(zones);
                    e.3.sort();
                    e.3.dedup();
                    e.4 += looted;
                    e.5 += quantity;
                    e.6 = e.6.min(first);
                    e.7 = e.7.max(last);
                }
                None => {
                    folded.insert(key, out.len());
                    out.push((game, base, npc, zones, looted, quantity, first, last));
                }
            }
        }
        out.sort_by(|a, b| b.4.cmp(&a.4).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        Ok(out)
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
    /// than summarised, because [`eqlparser_core::bestiary::merge`] has to
    /// recompute the spread over every log at once and cannot do that from
    /// per-log medians.
    pub fn put_bestiary(
        &mut self,
        game: &str,
        source: &str,
        report: &eqlparser_core::bestiary::BestiaryReport,
        health: &[eqlparser_core::bestiary::HealthSample],
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
            for p in &report.npcs {
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
    /// them is [`eqlparser_core::bestiary::merge`]'s job, so that one rule
    /// governs it whether the caller is the API or a future export.
    pub fn bestiary_excluding(
        &self,
        game: &str,
        source: &str,
    ) -> Result<(
        Vec<eqlparser_core::bestiary::NpcProfile>,
        Vec<eqlparser_core::bestiary::HealthSample>,
    )> {
        use eqlparser_core::bestiary::{HealthSample, MeleeAgainst, NpcProfile};
        use eqlparser_core::game::Game;
        use eqlparser_core::zone::Mode;

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
            profiles.push(NpcProfile {
                melee: melee
                    .remove(&(src, zone.clone(), mode, difficulty, npc.clone()))
                    .unwrap_or_default(),
                // The daemon's table stores the canonical name and has no
                // column for the article, so the two are the same here. A
                // profile read back out of it is identified correctly and
                // simply does not know how the client spelt it.
                key: eqlparser_core::drops::canonical_npc(&npc).to_string(),
                name: npc,
                zone,
                mode: parsed_mode,
                difficulty,
                game: parsed_game,
                // Empty for every row written before body types were recorded,
                // and for every creature nobody has proved one for.
                kind: eqlparser_core::bestiary::CreatureKind::from_str(&kind),
                kills,
                deaths_to_it: deaths,
                // Recomputed by the merge from the samples; whatever is put
                // here would be overwritten.
                health_samples: 0,
                health_min: 0,
                health_median: 0,
                health_max: 0,
                max_hit,
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
        prices: &[eqlparser_core::tradeskill::ItemPrice],
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A bestiary built the way the engine builds one, so the test exercises
    /// the real report rather than hand-written rows.
    fn bestiary(
        character: &str,
        zone: &str,
        level: u32,
        kills: &[(i64, u64)],
    ) -> (
        eqlparser_core::bestiary::BestiaryReport,
        Vec<eqlparser_core::bestiary::HealthSample>,
    ) {
        use eqlparser_core::bestiary::{Bestiary, Swing};
        let mut b = Bestiary::new(eqlparser_core::game::Game::EverQuestLegends, character);
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
        use eqlparser_core::bestiary::CreatureKind;
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
        let merged = eqlparser_core::bestiary::merge(profiles, &health);
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
        let merged = eqlparser_core::bestiary::merge(profiles, &health);

        assert_eq!(merged.len(), 1, "one creature across both logs: {merged:?}");
        let p = &merged[0];
        assert_eq!(
            p.kills, 3,
            "a second flush of the same log must not double it"
        );
        assert_eq!(p.zone, "Nagafen's Lair");
        assert_eq!(
            (p.mode, p.difficulty),
            (eqlparser_core::zone::Mode::Solo, 4)
        );
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
            eqlparser_core::bestiary::merge(profiles.clone(), &health)[0].kills,
            1
        );

        // ...so adding the engine's own view gives each kill exactly once.
        let combined = eqlparser_core::bestiary::merge(
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
        let dir = std::env::temp_dir().join("eqlparser-bestiary-level-test");
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
        assert_eq!(profiles[0].game, eqlparser_core::game::Game::EverQuest);
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
        let merged = eqlparser_core::bestiary::merge(profiles, &health);
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
    ) -> eqlparser_core::tradeskill::ItemPrice {
        eqlparser_core::tradeskill::ItemPrice {
            item: item.into(),
            unit_copper: copper,
            bought,
            quoted: 0,
            last_bought: at,
            vendors: vec![eqlparser_core::tradeskill::VendorPrice {
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
        let dir = std::env::temp_dir().join("eqlparser-store-test");
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
        let dir = std::env::temp_dir().join("eqlparser-aa-game-test");
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
            stmt.query_row(params![who], |r| r.get::<_, String>(0)).unwrap()
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
        let dir = std::env::temp_dir().join("eqlparser-identity-test");
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
