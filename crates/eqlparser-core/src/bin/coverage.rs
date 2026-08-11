//! Parser coverage report.
//!
//! Runs the parser across a real log and reports how many lines were
//! classified, broken down by event type, plus the most frequent *unmatched*
//! line shapes. The unmatched histogram is the working tool for growing the
//! rule table: it shows what to teach the parser next, in priority order.
//!
//! Usage: `eqlcoverage <logfile> [top-n]`

use eqlparser_core::class::ClassDetector;
use eqlparser_core::spell::{SpellDb, SpellResolver};
use eqlparser_core::{Event, Parser};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: eqlcoverage <logfile> [top-n] [game-dir]"))?;
    let top_n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);
    let game_dir = args.next();

    let player = Parser::player_from_filename(&path).unwrap_or_else(|| "You".to_string());
    let parser = Parser::new(&player);
    eprintln!("player: {player}");

    // The spell database turns otherwise-unclassifiable flavour text into
    // buff landings and fades, so coverage is reported with and without it.
    let mut resolver = match &game_dir {
        Some(dir) => {
            let d = Path::new(dir);
            let db = SpellDb::load(&d.join("spells_us.txt"), &d.join("spells_us_str.txt"))?;
            eprintln!("spell database: {} spells", db.len());
            SpellResolver::new(db, 1)
        }
        None => SpellResolver::new(SpellDb::empty(), 1),
    };

    let file = std::fs::File::open(&path)?;
    let reader = BufReader::with_capacity(1 << 20, file);

    let mut total = 0u64;
    let mut no_timestamp = 0u64;
    let mut by_kind: HashMap<&'static str, u64> = HashMap::new();
    let mut unknown_shapes: HashMap<String, (u64, String)> = HashMap::new();
    let mut buff_landings = 0u64;
    let mut buff_fades = 0u64;
    let mut low_confidence = 0u64;
    let mut low_conf_texts: HashMap<String, u64> = HashMap::new();
    let mut max_level = 0u32;
    let mut classes = ClassDetector::new();
    let mut last_timestamp = 0i64;

    let start = Instant::now();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        let Some(entry) = parser.parse_line(&line) else {
            no_timestamp += 1;
            continue;
        };
        last_timestamp = entry.timestamp;

        // Feed the resolver and class detector the context they need before
        // asking them to resolve anything.
        match &entry.event {
            Event::CastStart { source, spell, .. } if source.is_you() => {
                resolver.note_cast(entry.timestamp, spell);
                observe_spell(&mut classes, resolver.db(), entry.timestamp, spell);
            }
            Event::SpellbookChange { spell, change } => {
                if !matches!(change, eqlparser_core::SpellbookChangeKind::Forgotten) {
                    resolver.note_known_spell(spell);
                    observe_spell(&mut classes, resolver.db(), entry.timestamp, spell);
                }
            }
            Event::SkillUp { skill, .. } => classes.observe_skill(entry.timestamp, skill),
            Event::LevelUp { level } => {
                resolver.set_level(*level);
                max_level = max_level.max(*level);
            }
            _ => {}
        }

        let mut kind = kind_name(&entry.event);

        if matches!(entry.event, Event::Unknown) {
            let body = line.split_once("] ").map(|(_, b)| b).unwrap_or(&line);

            if resolver.db().is_landing_text(body) {
                if resolver.resolve_landing(entry.timestamp, body).is_some() {
                    buff_landings += 1;
                    kind = "EffectLanded";
                    // Confidence, not candidate count, is what matters: many
                    // multi-candidate texts are resolved decisively by context.
                    if resolver.last_confidence() < 0.8 {
                        low_confidence += 1;
                        *low_conf_texts.entry(body.to_string()).or_default() += 1;
                    }
                }
            } else if resolver.db().is_fade_text(body) {
                resolver.resolve_fade(body);
                buff_fades += 1;
                kind = "EffectFaded";
            } else {
                let shape = normalise(body);
                let e = unknown_shapes.entry(shape).or_insert((0, body.to_string()));
                e.0 += 1;
            }
        }

        *by_kind.entry(kind).or_default() += 1;
    }
    let elapsed = start.elapsed();

    let unknown = by_kind.get("Unknown").copied().unwrap_or(0);
    let classified = total - unknown - no_timestamp;

    println!("\n=== coverage ===");
    println!("lines            : {total}");
    println!("no timestamp     : {no_timestamp}");
    println!(
        "classified       : {classified}  ({:.2}%)",
        100.0 * classified as f64 / total as f64
    );
    println!(
        "unknown          : {unknown}  ({:.2}%)",
        100.0 * unknown as f64 / total as f64
    );
    println!(
        "parse time       : {:.2}s  ({:.0} lines/sec)",
        elapsed.as_secs_f64(),
        total as f64 / elapsed.as_secs_f64()
    );
    if !resolver.db().is_empty() {
        println!("\n=== spell resolution ===");
        println!("buff landings    : {buff_landings}");
        println!("buff fades       : {buff_fades}");
        println!(
            "low confidence   : {low_confidence}  ({:.1}% of landings are guesses)",
            100.0 * low_confidence as f64 / buff_landings.max(1) as f64
        );
        println!("repertoire       : {} spells", resolver.repertoire_size());
        println!("final level      : {}", resolver.level());

        // Classes change over a character's life, so report the recent trio
        // separately from the all-time picture.
        println!("\n=== class detection ===");
        println!("evidence points  : {}", classes.evidence_count());
        println!("current trio (last 24h of log):");
        for c in classes.current_trio(last_timestamp, 24 * 3600) {
            println!(
                "  {:<14} score {:>7.2}  ({:>5.1}%)  e.g. {}",
                c.class.name(),
                c.score,
                100.0 * c.share,
                c.top_reasons.first().map(String::as_str).unwrap_or("-")
            );
        }
        println!("all-time (every class ever held):");
        for c in classes.ranked_all_time().iter().take(8) {
            println!(
                "  {:<14} score {:>7.2}  ({:>5.1}%)",
                c.class.name(),
                c.score,
                100.0 * c.share
            );
        }

        println!("\n=== most frequent low-confidence landings ===");
        let mut lc: Vec<_> = low_conf_texts.iter().collect();
        lc.sort_by(|a, b| b.1.cmp(a.1));
        for (text, count) in lc.iter().take(12) {
            let cands = resolver.db().candidates_for_landing(text);
            let names: std::collections::BTreeSet<&str> = cands
                .iter()
                .filter_map(|id| resolver.db().get(*id))
                .map(|s| s.name.as_str())
                .collect();
            println!(
                "{count:>7}  {:<48} {} spells / {} names",
                text.chars().take(46).collect::<String>(),
                cands.len(),
                names.len()
            );
        }
    }

    println!("\n=== events by type ===");
    let mut kinds: Vec<_> = by_kind.iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(a.1));
    for (k, c) in kinds {
        println!("{c:>9}  {k}");
    }

    println!("\n=== top {top_n} unmatched shapes ===");
    let mut shapes: Vec<_> = unknown_shapes.into_iter().collect();
    shapes.sort_by_key(|(_, (count, _))| std::cmp::Reverse(*count));
    for (_, (count, example)) in shapes.iter().take(top_n) {
        let ex: String = example.chars().take(110).collect();
        println!("{count:>9}  {ex}");
    }

    Ok(())
}

/// Feed a spell the character is known to own into the class detector.
fn observe_spell(classes: &mut ClassDetector, db: &SpellDb, timestamp: i64, spell_name: &str) {
    if let Some(levels) = db.class_levels_for_name(spell_name) {
        classes.observe_spell(timestamp, spell_name, &levels);
    }
}

fn kind_name(e: &Event) -> &'static str {
    match e {
        Event::Damage { .. } => "Damage",
        Event::SpellSetLoaded { .. } => "SpellSetLoaded",
        Event::LoggedOn => "LoggedOn",
        Event::WhoEntry { .. } => "WhoEntry",
        Event::Tell { .. } => "Tell",
        Event::TradeOffer { .. } => "TradeOffer",
        Event::TradeComplete { .. } => "TradeComplete",
        Event::SkillTrained { .. } => "SkillTrained",
        Event::CharmImmune => "CharmImmune",
        Event::TargetUnaffected => "TargetUnaffected",
        Event::EffectLandedOnTarget { .. } => "EffectLandedOnTarget",
        Event::AbilityListEntry { .. } => "AbilityListEntry",
        Event::AbilityListCost { .. } => "AbilityListCost",
        Event::AbilityListDescription { .. } => "AbilityListDescription",
        Event::Miss { .. } => "Miss",
        Event::Absorbed { .. } => "Absorbed",
        Event::Heal { .. } => "Heal",
        Event::Death { .. } => "Death",
        Event::Loot { .. } => "Loot",
        Event::CoinLoot { .. } => "CoinLoot",
        Event::MerchantSale { .. } => "MerchantSale",
        Event::ZoneChange { .. } => "ZoneChange",
        Event::CastStart { .. } => "CastStart",
        Event::CastInterrupted { .. } => "CastInterrupted",
        Event::EffectLanded { .. } => "EffectLanded",
        Event::EffectFaded { .. } => "EffectFaded",
        Event::Experience { .. } => "Experience",
        Event::Faction { .. } => "Faction",
        Event::Chat { .. } => "Chat",
        Event::GroupChange { .. } => "GroupChange",
        Event::AbilityCooldown { .. } => "AbilityCooldown",
        Event::ItemProc { .. } => "ItemProc",
        Event::Emote { .. } => "Emote",
        Event::LevelUp { .. } => "LevelUp",
        Event::Avoided { .. } => "Avoided",
        Event::Resisted { .. } => "Resisted",
        Event::SkillUp { .. } => "SkillUp",
        Event::Craft { .. } => "Craft",
        Event::ComponentConsumed { .. } => "ComponentConsumed",
        Event::ItemPurchased { .. } => "ItemPurchased",
        Event::ItemQuoted { .. } => "ItemQuoted",
        Event::RecipeLearned { .. } => "RecipeLearned",
        Event::StanceChanged { .. } => "StanceChanged",
        Event::TaskAssigned { .. } => "TaskAssigned",
        Event::TaskUpdated { .. } => "TaskUpdated",
        Event::AchievementEarned { .. } => "AchievementEarned",
        Event::AbilityPointsGained { .. } => "AbilityPointsGained",
        Event::AbilityPointsSpent { .. } => "AbilityPointsSpent",
        Event::SpellbookChange { .. } => "SpellbookChange",
        Event::PetAttack { .. } => "PetAttack",
        Event::PetLeader { .. } => "PetLeader",
        Event::EffectWoreOffTarget { .. } => "EffectWoreOffTarget",
        Event::System { .. } => "System",
        Event::Unknown => "Unknown",
    }
}

/// Collapse digits so that lines differing only by numbers group together.
/// Names are left intact, so the histogram still over-counts distinct mobs —
/// but for finding *missing rules* the frequent shapes surface regardless.
fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_num = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            if !in_num {
                out.push('#');
                in_num = true;
            }
        } else {
            in_num = false;
            out.push(c);
        }
    }
    out
}
