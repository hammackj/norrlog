//! Building a contribution bundle, and saying what sending it would do.
//!
//! # Why this is not just a serialiser
//!
//! An ingest does not merge a **complete** bundle into the site. It deletes
//! this contributor's rows and writes the bundle's in their place -- which is
//! right, because a complete bundle is the whole of what that contributor's
//! exporter knows, and replacing it makes the ingest idempotent: send it
//! twice and the site lands in the same state.
//!
//! So a complete bundle carrying no rows for a table is not a smaller
//! contribution. It is a deletion, and it looks exactly like a good bundle
//! from the outside. Three separate incidents on this site have that shape: a
//! tooltip submission that emptied nine thousand drop rows, a hand-made
//! bundle with a plausible `sources` list that cleared 3,303 rows on the test
//! box, and an export run without `--raids` that emptied the raid tables.
//!
//! [`preview`] therefore does not report row counts. It reports **verdicts**:
//! for each of the forty-five tables, what an ingest of this document would
//! actually do to the site. A complete bundle with zero rows for a table is
//! marked [`Verdict::Deletes`] in as many words, because that is what it is.
//!
//! # One question, not thirty-nine
//!
//! Until 8 Sep 2026 authorisation was per table: `Bundle::sources` named the
//! inputs the export had read, `TABLE_SOURCES` mapped tables to inputs, and
//! `may_replace` answered the question once per table -- with two defaults
//! that both said *yes*, which is the shape behind every incident above.
//!
//! It is now [`Bundle::complete`](eqlparser_core::bundle::Bundle::complete),
//! asked once. A complete bundle replaces; an incomplete one merges and can
//! delete nothing at all. `sources` survives as something to read rather than
//! something to obey.
//!
//! # What this daemon fills, and what it does not
//!
//! eqlparser owns everything a log or the client's own files can state. It
//! does not own the authored material -- the hand-written walkthroughs, the
//! recipe book's prose, the wiki's AA transcription -- nor the OCR intake.
//! Those are still `eqldb-export`'s to send.
//!
//! Which is why this daemon must not mark its bundles complete unless they
//! are: a bundle that says *complete* while carrying only what a log can
//! state would delete everything else. [`preview`] shows what that would do
//! before anybody sends it.
//!
//! # What this daemon holds only half of
//!
//! Kept as a record rather than as machinery. Until the pivot these were the
//! reasons three sources could not be *declared*, when declaring one
//! authorised a clear over every table behind it. Nothing authorises a clear
//! now except `complete`, so the list no longer gates anything --- but the
//! measurements are what would have to change before this daemon could ever
//! honestly say `complete`, and they were expensive to take.
//!
//! **`labels`** covers `markers` and `zone_shortnames`, and the site merges
//! two things into those: the cartographer blocks a player typed in game,
//! which is what the record collects, and the eqmaps label store, which holds
//! every marker walked before that convention existed. This daemon has the
//! first half.
//!
//! **`item_icons`** covers icons, and the record knows only the ones the
//! client's own files state; the rest come from the OCR matcher. Measured
//! rather than assumed: the registry alone yields 1,255 icons against the
//! 2,062 in the last bundle `eqldb-export` built. Around eight hundred are
//! not this daemon's to state.
//!
//! **`faction`** covers `faction_hits` and `turnins`, and the record collects
//! both from the log this daemon *follows* --- one log. Measured 7 September
//! 2026: the record holds 42 turnins against the site's 46, and the four
//! missing are one turn-in --- `Megosh Thistlethorn` / `Halfling Ranger
//! Note`, four faction lines --- recorded in `eqlog_Tharn_erudin.txt`, which
//! this daemon has never opened.
//!
//! The item-id registry is the contrast worth holding onto: it reads every
//! character's files off disk, so it covers alts and other servers without
//! being told, while the log-derived families cover whoever is being played
//! right now. A character who played elsewhere is not under-represented in
//! those; they are absent, and a bundle cannot tell the difference.
//!
//! The cost of that is real and it is the smaller one: what this daemon
//! cannot state, it can only add to. A faction hit recorded wrongly is
//! corrected on the site by hand, not by a later bundle.

use eqlparser_core::bundle::{
    Bundle, Consider, ConsiderLevel, CreatureSpotRow, DropRow, FactionAmount,
    DropPlaceRow, FactionHit, ItemIcon, ItemId, KillRow, LogSource, SkillObserved, TurnIn,
    VendorRow,
    ForagedItem as BundleForagedItem, ZoneForaged,
    ZoneMarker, ZoneShortname,
};
use eqlparser_core::game::Game;

pub use eqlparser_core::preview::{preview, Preview, TableView, Verdict};

/// Build the bundle this daemon would send.
///
/// `contributor` is a person's name, given by that person. It is not derived
/// from the token: a name assigned from a credential is not an attribution,
/// and rows filed under the wrong one cannot be told apart afterwards.
pub fn build(
    store: &crate::store::Store,
    ids: Option<&eqlparser_core::itemids::Registry>,
    contributor: &str,
    now: i64,
) -> anyhow::Result<Bundle> {
    let mut b = Bundle::new(contributor.trim(), now)
        .generated_by(concat!("eqlparserd ", env!("CARGO_PKG_VERSION")));

    // Only what was played: not the test server, not a fixture under /tmp.
    // Everything below reads through these views; see `Store::played_only`.
    let _played = store.played_only()?;

    let reports = store.record_reports(crate::record::GAME_TAG)?;

    // Forage. Zones with no successes travel too: see `ZoneForaged`.
    b.forage = reports
        .forage
        .zones
        .into_iter()
        .map(|z| ZoneForaged {
            zone: z.zone,
            items: z
                .items
                .into_iter()
                .map(|i| BundleForagedItem {
                    item: i.item,
                    times: i.times,
                })
                .collect(),
            empty: z.empty,
            first_seen: z.first_seen,
            last_seen: z.last_seen,
        })
        .collect();

    b.considers = reports
        .consider
        .considers
        .into_iter()
        .map(|c| Consider {
            creature: c.creature,
            levels: c
                .levels
                .into_iter()
                .map(|l| ConsiderLevel {
                    level: l.level,
                    times: l.times,
                })
                .collect(),
            rare: c.rare,
            raid: c.raid,
            considers: c.considers,
        })
        .collect();

    b.faction_hits = reports
        .faction
        .hits
        .into_iter()
        .map(|h| FactionHit {
            creature: h.creature,
            zone: h.zone,
            faction: h.faction,
            adjustments: h
                .adjustments
                .into_iter()
                .map(|a| FactionAmount {
                    amount: a.amount,
                    times: a.times,
                })
                .collect(),
            kills: h.kills,
        })
        .collect();

    b.turnins = reports
        .faction
        .turnins
        .into_iter()
        .map(|t| TurnIn {
            npc: t.npc,
            item: t.item,
            faction: t.faction,
            adjustments: t
                .adjustments
                .into_iter()
                .map(|a| FactionAmount {
                    amount: a.amount,
                    times: a.times,
                })
                .collect(),
            offered: t.offered,
            handins: t.handins,
        })
        .collect();

    b.skills = reports
        .skills
        .skills
        .into_iter()
        .map(|s| SkillObserved {
            skill: s.skill,
            at_least: s.at_least as i64,
        })
        .collect();

    b.markers = reports
        .carto
        .markers
        .into_iter()
        .map(|m| ZoneMarker {
            name: m.name,
            zone_short: m.zone_short,
            x: m.x,
            y: m.y,
            z: m.z,
            role: m.role,
            kind: m.kind.to_string(),
        })
        .collect();

    b.zone_shortnames = reports
        .carto
        .zones
        .into_iter()
        .map(|z| ZoneShortname {
            zone: z.zone,
            zone_short: z.zone_short,
        })
        .collect();

    b.spots = reports
        .spots
        .spots
        .into_iter()
        .map(|s| CreatureSpotRow {
            name: s.name,
            zone: s.zone,
            zone_short: s.zone_short,
            x: s.x,
            y: s.y,
            z: s.z,
            gap: s.gap,
            at: s.at,
        })
        .collect();

    if let Some(reg) = ids {
        read_item_ids(reg, &mut b);
    }

    // The history-derived tables. Carried so the panel can show what is
    // there. Nothing here declares a source any more --- see the module doc
    // for what this daemon holds only half of, which is the record that
    // outlived the allowlist.
    b.logs = store
        .sources()?
        .into_iter()
        .map(|(path, last_seen, bytes)| {
            let (character, server) =
                eqlparser_core::Parser::identity_from_filename(&path).unwrap_or_default();
            LogSource {
                id: path,
                character,
                server,
                bytes: bytes as u64,
                last_seen,
            }
        })
        .collect();
    // The breakdown, read once and looked up per row. `zones` says where a
    // pairing was seen and `looted` counts all of them together, which is a
    // set and a total rather than an attribution; these say how many came
    // from where. Absent for anything recorded before the store kept them,
    // and `places` is skipped when empty rather than serialised as an empty
    // list -- an unknown breakdown must not arrive looking like a measured
    // one with nothing in it.
    let mut places = store.all_drop_places()?;
    b.drops = store
        .all_drops()?
        .into_iter()
        .filter(|row| row.game == Game::EverQuestLegends.slug())
        .map(
            // `left` is not sent: eqldb's import has no column for it yet,
            // and this is that site's wire format, not ours to widen alone.
            |crate::store::StoredDrop { game, item, npc, zones, looted, quantity, first_seen, last_seen, .. }| {
                let key = (game, item.to_lowercase(), npc.to_lowercase());
                let places = places
                    .remove(&key)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(zone, mode, difficulty, looted, quantity)| DropPlaceRow {
                        zone,
                        mode,
                        difficulty,
                        looted,
                        quantity,
                    })
                    .collect();
                DropRow {
                    server: String::new(),
                    ruleset: String::new(),
                    item,
                    npc,
                    grades: Vec::new(),
                    zones,
                    places,
                    looted,
                    quantity,
                    first_seen,
                    last_seen,
                }
            },
        )
        .collect();
    b.kills = store
        .all_kills()?
        .into_iter()
        .filter(|(game, ..)| game == Game::EverQuestLegends.slug())
        .map(|(_, npc, kills)| KillRow { npc, kills })
        .collect();
    b.vendors = store
        .all_vendors()?
        .into_iter()
        .filter(|(game, ..)| game == Game::EverQuestLegends.slug())
        .map(
            |(_, item, merchant, zone, unit_copper, bought, quoted, last_bought)| VendorRow {
                item,
                merchant,
                zone,
                unit_copper,
                bought,
                quoted,
                last_bought,
            },
        )
        .collect();
    // Every log's profiles: the exclusion is for a caller that wants the live
    // engine to supply the log it is following, and this one does not.
    let (profiles, health) = store.bestiary_excluding(Game::EverQuestLegends.slug(), "")?;
    b.creatures = eqlparser_core::bestiary::merge(profiles, &health);

    // Claiming nothing, deliberately.
    //
    // `sources` used to be this bundle's declaration of what it was
    // authoritative over, and the ingest cleared a table when the bundle
    // named the source behind it. That mechanism is gone --- `complete`
    // decides, once, and this builder never sets it --- so a list here would
    // be a claim nothing acts on, made by a bundle that cannot clear
    // anything. The preview says as much in words: "claims nothing --- every
    // table is added to rather than replaced".
    //
    // Empty rather than absent: absent is what an old bundle looks like, and
    // this one is stating that it claims nothing rather than failing to say.
    b.sources = Some(Vec::new());
    Ok(b)
}

/// Turn the item-id record into the two tables it feeds.
///
/// One `item_ids` row per name the record knows, because the site keys items
/// by name and an id known only under a spelling nobody uses reaches nothing.
/// The grades sent are the union of held and seen: a loot filter is weaker
/// evidence than an inventory, and the split matters to the record, but the
/// site's question is only whether a grade exists.
fn read_item_ids(reg: &eqlparser_core::itemids::Registry, b: &mut Bundle) {
    for e in &reg.items {
        let mut grades: Vec<u8> = e
            .grades_held
            .iter()
            .chain(e.grades_seen.iter())
            .map(|g| *g as u8)
            .collect();
        grades.sort_unstable();
        grades.dedup();
        for name in e.names() {
            b.item_ids.push(ItemId {
                item: name.to_string(),
                item_id: e.item_id,
                grades_held: grades.clone(),
            });
        }
    }
    // Icons, first-wins by ascending id, which is the rule the export has
    // always used: the lowest id is the base item and the higher ones its
    // graded twins.
    let mut by_id: Vec<&eqlparser_core::itemids::Entry> = reg.items.iter().collect();
    by_id.sort_by_key(|e| e.item_id);
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for e in by_id {
        let Some(icon) = e.icon_id else { continue };
        if !seen.insert(e.item.clone()) {
            continue;
        }
        b.item_icons.push(ItemIcon {
            item: e.item.clone(),
            icon,
            score: 1.0,
            gap: 1.0,
            evidence: e.sources.join(", "),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel groups tables by verdict, and it does it with one `filter`
    /// per group --- which drops anything it does not match instead of
    /// complaining. So a verdict the enum renames and the panel does not is
    /// silent by construction.
    ///
    /// That is not hypothetical. `Verdict::Merges` serialises `"merges"`, the
    /// panel filtered for `"adds-only"`, and on 10 Sep 2026 the inspector
    /// showed the 25 tables carrying nothing and hid all 17 carrying rows ---
    /// 49,383 of them, including every drop, kill and creature. The bundle it
    /// was describing was fine; the description had a hole in it.
    ///
    /// The panel has a catch-all group now, so the next one is loud rather
    /// than invisible. This is the other half: the names are checked against
    /// each other rather than trusted to stay in step.
    #[test]
    fn every_verdict_has_a_group_in_the_panel() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root");
        // The daemon is usable without the desktop app beside it.
        let Ok(js) = std::fs::read_to_string(root.join("gui/renderer/bundle.js")) else {
            return;
        };

        for v in [
            Verdict::Replaces,
            Verdict::Merges,
            Verdict::Deletes,
            Verdict::Absent,
        ] {
            // Through serde, so the test reads the name the panel actually
            // receives rather than one spelled out twice here.
            let name = serde_json::to_string(&v).expect("verdict serialises");
            let name = name.trim_matches('"');
            assert!(
                js.contains(&format!("key: \"{name}\"")),
                "the daemon sends the verdict {name:?} and gui/renderer/bundle.js \
                 has no group with that key --- every table with that verdict is \
                 dropped from the inspector without a word"
            );
        }
    }
}
