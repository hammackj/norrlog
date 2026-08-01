//! Encounter report: runs the full pipeline over a log and prints the biggest
//! fights with their damage breakdowns.
//!
//! Usage: `eqlreport <logfile> [game-dir]`

use eqlparser_core::class::ClassDetector;
use eqlparser_core::encounter::{Encounter, EncounterTracker};
use eqlparser_core::spell::{SpellDb, SpellResolver};
use eqlparser_core::{Event, Parser, SpellbookChangeKind};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: eqlreport <logfile> [game-dir]"))?;
    let game_dir = args.next();

    let player = Parser::player_from_filename(&path).unwrap_or_else(|| "You".into());
    let parser = Parser::new(&player);
    let mut tracker = EncounterTracker::new(&player);
    let mut classes = ClassDetector::new();

    let mut resolver = match &game_dir {
        Some(dir) => {
            let d = Path::new(dir);
            SpellResolver::new(
                SpellDb::load(&d.join("spells_us.txt"), &d.join("spells_us_str.txt"))?,
                1,
            )
        }
        None => SpellResolver::new(SpellDb::empty(), 1),
    };

    let reader = BufReader::with_capacity(1 << 20, std::fs::File::open(&path)?);
    let mut encounters: Vec<Encounter> = Vec::new();
    let mut last_ts = 0i64;

    let start = Instant::now();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(entry) = parser.parse_line(&line) else {
            continue;
        };
        last_ts = entry.timestamp;

        match &entry.event {
            Event::CastStart { source, spell, .. } if source.is_you() => {
                resolver.note_cast(entry.timestamp, spell);
                if let Some(l) = resolver.db().class_levels_for_name(spell) {
                    classes.observe_spell(entry.timestamp, spell, &l);
                }
            }
            Event::SpellbookChange { spell, change }
                if !matches!(change, SpellbookChangeKind::Forgotten) =>
            {
                resolver.note_known_spell(spell);
                if let Some(l) = resolver.db().class_levels_for_name(spell) {
                    classes.observe_spell(entry.timestamp, spell, &l);
                }
            }
            Event::SkillUp { skill, .. } => classes.observe_skill(entry.timestamp, skill),
            Event::LevelUp { level } => resolver.set_level(*level),
            _ => {}
        }

        if let Some(done) = tracker.push(&entry) {
            encounters.push(done);
        }
    }
    if let Some(done) = tracker.flush() {
        encounters.push(done);
    }
    let elapsed = start.elapsed();

    println!("player     : {player}  (level {})", resolver.level());
    println!("encounters : {}", encounters.len());
    println!("parsed in  : {:.2}s", elapsed.as_secs_f64());

    let trio = classes.current_trio(last_ts, 24 * 3600);
    let trio_s: Vec<String> = trio.iter().map(|c| c.class.abbrev().to_string()).collect();
    println!("classes    : {}", trio_s.join("/"));

    // Longest fights are the interesting ones; short trash pulls dominate by
    // count but say little.
    let mut by_damage = encounters.clone();
    by_damage.sort_by_key(|e| std::cmp::Reverse(e.total_damage()));

    println!("\n=== top 8 encounters by total damage ===");
    for e in by_damage.iter().take(8) {
        println!(
            "\n#{}  {}  [{}]  {}s   total {} dmg",
            e.id,
            e.title(),
            e.zone.as_deref().unwrap_or("?"),
            e.duration(),
            e.total_damage()
        );
        for (key, s) in e.damage_ranking().iter().take(5) {
            println!(
                "    {:<28} {:>9} dmg  {:>8.1} dps  {:>5.1}% crit  {:>5.1}% acc",
                s.name,
                s.damage_done,
                e.dps(key),
                100.0 * s.crit_rate(),
                100.0 * s.accuracy()
            );
        }
        if let Some(you) = e.actors.get("you") {
            let abilities = you.abilities_by_damage();
            if !abilities.is_empty() {
                println!("    your top abilities:");
                for (name, st) in abilities.iter().take(5) {
                    println!(
                        "      {:<26} {:>8} ({:>5.1}%)  avg {:>6.1}  max {:>5}",
                        name,
                        st.total,
                        100.0 * st.total as f64 / you.damage_done.max(1) as f64,
                        st.average(),
                        st.max
                    );
                }
            }
            if you.deaths > 0 {
                println!("    you died {}x", you.deaths);
            }
        }
        let pets = e.charmed_pets();
        if !pets.is_empty() {
            let with = e.your_damage_with_pets();
            let own = e.actors.get("you").map(|s| s.damage_done).unwrap_or(0);
            println!(
                "    charmed pets: {}  (+{} dmg -> your total {} = {:.1} dps)",
                pets.iter()
                    .map(|p| format!("{} [{}]", p.name, p.damage_while_charmed))
                    .collect::<Vec<_>>()
                    .join(", "),
                with - own,
                with,
                with as f64 / e.duration() as f64
            );
        }
        if !e.loot.is_empty() {
            let items: Vec<String> = e.loot.iter().map(|l| l.item.clone()).collect();
            println!("    loot: {}", items.join(", "));
        }
    }

    // Aggregate view across the whole log.
    let total_dmg: u64 = encounters.iter().map(|e| e.total_damage()).sum();
    let your_dmg: u64 = encounters
        .iter()
        .filter_map(|e| e.actors.get("you"))
        .map(|s| s.damage_done)
        .sum();
    let your_deaths: u64 = encounters
        .iter()
        .filter_map(|e| e.actors.get("you"))
        .map(|s| s.deaths)
        .sum();
    let kills: usize = encounters
        .iter()
        .map(|e| {
            e.deaths
                .iter()
                .filter(|d| d.killer.as_deref() == Some(player.as_str()))
                .count()
        })
        .sum();
    let coin: u64 = encounters.iter().map(|e| e.coin_copper).sum();
    let loot_count: usize = encounters.iter().map(|e| e.loot.len()).sum();

    println!("\n=== totals ===");
    println!("damage (all actors) : {total_dmg}");
    println!("damage (you)        : {your_dmg}");
    println!("kills by you        : {kills}");
    println!("your deaths         : {your_deaths}");
    println!("items looted        : {loot_count}");
    println!("coin                : {:.2} platinum", coin as f64 / 1000.0);

    Ok(())
}
