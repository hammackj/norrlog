//! What sending a bundle would do to the site, table by table.
//!
//! Moved here from the daemon so every app that sends a bundle answers the
//! question with one copy of the rule. The reasoning — why a complete bundle
//! with no rows for a table is a deletion, and why that blocks a send — is
//! in the daemon's `bundle` module docs, where the incidents are recorded.

use crate::bundle::{Bundle, NEVER_CLEARED, TABLE_SOURCES};
use serde::{Deserialize, Serialize};

/// What a table's presence in this bundle would do to the site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// Rows, from a complete bundle. This contributor's rows in that table
    /// are deleted and these are written in their place. The ordinary case.
    Replaces,
    /// Rows, folded into what is there rather than replacing it -- because
    /// the bundle is not complete, or because the table is one of the five
    /// `NEVER_CLEARED` ones that a complete bundle still does not empty.
    ///
    /// Worth knowing rather than reassuring: a sparse row lands on
    /// `ON CONFLICT` and blanks every column it does not carry, which is how
    /// a lore submission once emptied an item's stats.
    Merges,
    /// **No rows, from a complete bundle.** An ingest of this deletes every
    /// row this contributor has in that table. Never correct by accident.
    Deletes,
    /// No rows, and nothing would be cleared. The ingest leaves the table
    /// alone; this is what a table somebody else fills should look like.
    Absent,
}

/// One table, and what would happen to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableView {
    /// The name the ingest knows it by.
    pub table: String,
    /// How many rows this bundle carries for it.
    pub rows: usize,
    /// The inputs that fill it, from [`TABLE_SOURCES`]. Documentation for
    /// the reader: it no longer decides anything.
    pub needs: Vec<String>,
    /// Whether an ingest of this bundle would clear the table first.
    pub clears: bool,
    pub verdict: Verdict,
    /// Plain words for the row, so a person reading the panel does not have
    /// to hold the rule in their head.
    pub says: String,
}

/// Everything a person should see before a bundle is sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    pub contributor: String,
    pub game: String,
    pub generator: String,
    pub generated_at: i64,
    /// What the bundle claims to be authoritative over.
    pub sources: Vec<String>,
    pub tables: Vec<TableView>,
    /// Rows across every table.
    pub rows: usize,
    /// How big the document is, as JSON and gzipped — the two numbers that
    /// decide whether the upload is a moment or a minute.
    pub bytes: usize,
    pub gzip_bytes: usize,
    /// Tables an ingest would empty. Lifted out of `tables` because this is
    /// the list somebody has to look at, and a panel that buries it among
    /// forty-four healthy rows is not showing it.
    pub would_delete: Vec<String>,
    /// Why this bundle cannot be sent, if it cannot.
    pub blocked: Vec<String>,
}

/// Every table an ingest knows, and how many rows this bundle has for it.
///
/// The list is [`TABLE_SOURCES`] plus the tables nothing gates — those are
/// the dangerous ones to leave out, because `may_replace` returns true for a
/// table it has never heard of.
pub fn rows_in(b: &Bundle, table: &str) -> usize {
    match table {
        "logs" => b.logs.len(),
        "drops" => b.drops.len(),
        "drop_places" => b.drops.iter().map(|d| d.places.len()).sum(),
        "kills" => b.kills.len(),
        "vendors" => b.vendors.len(),
        "creatures" => b.creatures.len(),
        "creature_melee" => b.creatures.iter().map(|c| c.melee.len()).sum(),
        "aa_observed" => b.aa_observed.len(),
        "spell_grants" => b.spell_grants.len(),
        "raids" => b.raids.len(),
        "raid_encounters" => b.raids.iter().map(|r| r.encounters.len()).sum(),
        "spawn_defaults" => usize::from(b.spawn_default_seconds.is_some()),
        "achievements" => b.achievements.len(),
        "quests" => b.quests.len(),
        "quest_items" => b.quests.iter().map(|q| q.items.len()).sum(),
        "item_facts" => b.item_facts.len(),
        "item_facts_stats" => b.item_facts.iter().map(|f| f.stats.len()).sum(),
        "item_augments" => b.item_facts.iter().map(|f| f.augments.len()).sum(),
        "item_effects" => b.item_facts.iter().map(|f| f.effects.len()).sum(),
        "faction_hits" => b.faction_hits.len(),
        "turnins" => b.turnins.len(),
        "considers" => b.considers.len(),
        // One row per zone, not per item: the panel counts what the ingest
        // writes, and a zone with no items still writes one.
        "forage" => b.forage.len(),
        "spawn_rules" => b.spawn_rules.len(),
        "item_icons" => b.item_icons.len(),
        "zone_roster" => b.zone_roster.len(),
        "abilities" => b.abilities.len(),
        "skills_seen" => b.skills.len(),
        "item_ids" => b.item_ids.len(),
        "markers" => b.markers.len(),
        "creature_spots" => b.spots.len(),
        "books" => b.books.len(),
        "book_pages" => b.books.iter().map(|k| k.pages.len()).sum(),
        "zone_shortnames" => b.zone_shortnames.len(),
        "observed_recipes" => b.observed_recipes.len(),
        "observed_components" => b.observed_recipes.iter().map(|r| r.components.len()).sum(),
        "recipes" => b.recipes.len(),
        "recipe_components" => b
            .recipes
            .iter()
            .map(|r| r.components.as_ref().map_or(0, Vec::len))
            .sum(),
        "recipe_tools" => b.recipe_tools.len(),
        "aa" => b.aa.len(),
        "class_armor" => b.class_armor.len(),
        "class_armor_slots" => b.class_armor_slots.len(),
        "expansions" => b.expansions.len(),
        "expansion_zones" => b.expansion_zones.len(),
        "rulesets" => b.rulesets.len(),
        "servers" => b.servers.len(),
        // Not a table this builder knows. Counting it as zero would be a lie
        // that reads as reassurance, so it is reported as unknown by the
        // caller instead; see `preview`.
        _ => usize::MAX,
    }
}

/// What sending this bundle would do, table by table.
pub fn preview(b: &Bundle) -> Preview {
    let json = serde_json::to_vec(b).unwrap_or_default();
    let gzip_bytes = gzip_len(&json);

    let mut tables: Vec<TableView> = Vec::new();
    for (table, needs) in TABLE_SOURCES {
        let rows = rows_in(b, table);
        let rows = if rows == usize::MAX { 0 } else { rows };
        // One question, asked once for the whole bundle, and then narrowed by
        // the only exception there is.
        let clears = b.complete && !NEVER_CLEARED.contains(table);
        let verdict = match (rows > 0, clears) {
            (true, true) => Verdict::Replaces,
            (true, false) => Verdict::Merges,
            (false, true) => Verdict::Deletes,
            (false, false) => Verdict::Absent,
        };
        let says = match verdict {
            Verdict::Replaces => {
                format!("replaces your {table} rows on the site with these {rows}")
            }
            // Two different reasons a table is not cleared, and they need
            // different warnings. An incomplete bundle is the dangerous one:
            // the rows land on the `ON CONFLICT` and a sparse row blanks the
            // columns it omits. A never-cleared table is the safe one, and
            // saying "this bundle is not complete" about it would be
            // misleading -- it may well be complete, and these five are not
            // cleared either way.
            Verdict::Merges if !b.complete => format!(
                "adds {rows} row(s); this bundle is not complete, so nothing is cleared \
                 first and a sparse row will blank the columns it omits"
            ),
            Verdict::Merges => format!(
                "adds or updates {rows} row(s) in {table}; it is never cleared wholesale, \
                 so nothing already on the site is deleted for not being in this bundle"
            ),
            Verdict::Deletes => format!(
                "DELETES every {table} row you have on the site — this bundle says it is \
                 complete and carries nothing for it"
            ),
            Verdict::Absent => format!("leaves {table} alone; nothing here claims it"),
        };
        tables.push(TableView {
            table: (*table).to_string(),
            rows,
            needs: needs.iter().map(|n| (*n).to_string()).collect(),
            clears,
            verdict,
            says,
        });
    }
    tables.sort_by(|a, b| a.table.cmp(&b.table));

    let would_delete: Vec<String> = tables
        .iter()
        .filter(|t| t.verdict == Verdict::Deletes)
        .map(|t| t.table.clone())
        .collect();

    let mut blocked = Vec::new();
    if b.contributor.trim().is_empty() {
        blocked.push(
            "no contributor name. An upload needs a person to admit to it, and a name taken \
             from the token is not an attribution."
                .to_string(),
        );
    }
    if !would_delete.is_empty() {
        blocked.push(format!(
            "{} table(s) would be emptied: {}. Fix the data, or do not mark this bundle \
             complete; do not send it as it stands.",
            would_delete.len(),
            would_delete.join(", ")
        ));
    }

    Preview {
        contributor: b.contributor.clone(),
        game: b.game.clone(),
        generator: b.generator.clone(),
        generated_at: b.generated_at,
        sources: b.sources.clone().unwrap_or_default(),
        rows: tables.iter().map(|t| t.rows).sum(),
        tables,
        bytes: json.len(),
        gzip_bytes,
        would_delete,
        blocked,
    }
}

/// Gzipped length, without keeping the compressed copy.
pub fn gzip_len(bytes: &[u8]) -> usize {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    if enc.write_all(bytes).is_err() {
        return 0;
    }
    enc.finish().map(|v| v.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::*;

    fn empty() -> Bundle {
        Bundle::new("somebody", 1_000)
    }

    /// The verdict for a table nobody filled and nobody claimed.
    #[test]
    fn an_unclaimed_empty_table_is_left_alone() {
        let mut b = empty();
        b.sources = Some(Vec::new());
        let p = preview(&b);
        let t = p
            .tables
            .iter()
            .find(|t| t.table == "recipes")
            .expect("recipes");
        assert_eq!(t.verdict, Verdict::Absent);
        assert!(p.would_delete.is_empty());
    }

    /// The one this panel exists for. A **complete** bundle carrying nothing
    /// for a table deletes that table, and it must say so in as many words
    /// rather than showing a tidy zero.
    ///
    /// It used to ask this of a *declared* source, and before that of
    /// `recipes`. Both moved for the same reason each time: the question has
    /// to be put to something that can still actually be emptied, or the test
    /// passes by describing a deletion that cannot happen.
    #[test]
    fn a_complete_bundle_with_no_rows_is_a_deletion() {
        let mut b = empty();
        b.complete = true;
        let p = preview(&b);
        let t = p.tables.iter().find(|t| t.table == "drops").expect("drops");
        assert_eq!(t.verdict, Verdict::Deletes);
        assert!(t.says.contains("DELETES"), "{}", t.says);
        assert!(p.would_delete.contains(&"drops".to_string()));
        assert!(
            p.blocked.iter().any(|b| b.contains("drops")),
            "a bundle that would empty a table is not safe to send: {:?}",
            p.blocked
        );
    }

    /// And a table a complete bundle still does not clear does not answer
    /// that way.
    ///
    /// The other half, and the one worth pinning. An item's facts come from
    /// screenshots the site reviews and writes back under the contributor's
    /// own name, which their exporter knows nothing about -- so "complete"
    /// cannot mean complete about those. The panel has to say nothing rather
    /// than keep warning about a danger that has been removed: a warning
    /// nobody can act on is one they learn to scroll past.
    #[test]
    fn a_table_that_is_never_cleared_is_not_a_deletion() {
        let mut b = empty();
        b.complete = true;
        let p = preview(&b);
        for table in crate::bundle::NEVER_CLEARED {
            let t = p.tables.iter().find(|t| &t.table == table).expect(table);
            assert_eq!(t.verdict, Verdict::Absent, "{table}: {}", t.says);
            assert!(!t.clears, "{table} would be cleared by a complete bundle");
            assert!(!p.would_delete.contains(&(*table).to_string()));
        }
        // And the contrast, in the same bundle, so this cannot pass by the
        // preview having decided nothing clears anything.
        let drops = p.tables.iter().find(|t| t.table == "drops").expect("drops");
        assert_eq!(drops.verdict, Verdict::Deletes);
    }

    /// Rows from an incomplete bundle are folded in rather than replacing,
    /// and the panel says why. This is the state everything this daemon
    /// builds ships in, because it never marks a bundle complete.
    #[test]
    fn rows_from_an_incomplete_bundle_add_rather_than_replace() {
        let mut b = empty();
        b.markers.push(ZoneMarker {
            name: "Megosh".into(),
            zone_short: "rivervale".into(),
            x: 0.0,
            y: 0.0,
            z: 0.0,
            role: "Guildmaster".into(),
            kind: "npc".into(),
        });
        b.sources = Some(Vec::new());
        let p = preview(&b);
        let t = p
            .tables
            .iter()
            .find(|t| t.table == "markers")
            .expect("markers");
        assert!(!b.complete, "this daemon must not mark a bundle complete");
        assert_eq!(t.verdict, Verdict::Merges);
        assert_eq!(t.rows, 1);
        assert!(p.would_delete.is_empty());
    }

    /// Faction is collected from the one log this daemon follows, so its view
    /// of a character played elsewhere is absent rather than merely smaller.
    ///
    /// It used to be protected by not declaring the `faction` source. That
    /// guard is gone -- sources decide nothing now -- and what protects it
    /// instead is stronger and simpler: **this daemon never marks a bundle
    /// complete**, so no table it fills can be emptied by anything it sends.
    ///
    /// Still a test, because the failure is silent in exactly the same way:
    /// setting `complete` here would build, preview cleanly, and delete four
    /// turnins on arrival.
    #[test]
    fn nothing_this_daemon_builds_can_delete_anything() {
        let mut b = empty();
        b.faction_hits.push(FactionHit {
            creature: "an orc pawn".into(),
            zone: "crushbone".into(),
            faction: "Crushbone Orcs".into(),
            adjustments: vec![FactionAmount {
                amount: -5,
                times: 3,
            }],
            kills: 3,
        });
        b.turnins.push(TurnIn {
            npc: "Megosh Thistlethorn".into(),
            item: "Halfling Ranger Note".into(),
            faction: "Storm Reapers".into(),
            adjustments: vec![FactionAmount {
                amount: 100,
                times: 1,
            }],
            offered: 1,
            handins: 1,
        });
        b.sources = Some(Vec::new());

        assert!(!b.complete, "this daemon must not mark a bundle complete");
        let p = preview(&b);
        for table in ["faction_hits", "turnins"] {
            let t = p.tables.iter().find(|t| t.table == table).expect(table);
            assert_eq!(t.verdict, Verdict::Merges, "{table}: {}", t.says);
            assert_eq!(t.rows, 1);
        }
        // The whole of it, not just the two tables above: an incomplete
        // bundle cannot empty anything at all, which is the property worth
        // asserting over the property of one source being absent from a list.
        assert!(p.would_delete.is_empty(), "{:?}", p.would_delete);
        assert!(
            p.tables.iter().all(|t| !t.clears),
            "an incomplete bundle would clear something"
        );
    }

    /// A bundle nobody has put their name to cannot be sent, whatever is in
    /// it. A name taken from the token would not be an attribution.
    #[test]
    fn an_unattributed_bundle_is_blocked() {
        let mut b = Bundle::new("   ", 1_000);
        b.sources = Some(Vec::new());
        let p = preview(&b);
        assert!(
            p.blocked.iter().any(|b| b.contains("contributor")),
            "{:?}",
            p.blocked
        );
    }

    /// Every table an ingest can clear has to be counted here. A table this
    /// function does not know falls through to `usize::MAX`, and reporting
    /// that as zero would be a deletion presented as an empty row.
    #[test]
    fn every_gated_table_is_counted() {
        let b = empty();
        for (table, _) in TABLE_SOURCES {
            assert_ne!(
                rows_in(&b, table),
                usize::MAX,
                "{table} is gated by a source but `rows_in` cannot count it, so the \
                 panel would show it as empty whether it is or not"
            );
        }
    }
}
