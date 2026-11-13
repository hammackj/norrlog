//! What forage yields, per zone, out of one log.
//!
//!     cargo run -p norrlog-core --example forage -- <log>
use norrlog_core::feed::{Feed, Observer};
use norrlog_core::{game::Game, ForageCollector};

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("a log path");
    let who = std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.split('_').nth(1))
        .unwrap_or("You")
        .to_string();
    let mut c = ForageCollector::new();
    let mut feed = Feed::new(&who, Game::EverQuestLegends);
    {
        let mut subs: Vec<&mut dyn Observer> = vec![&mut c];
        feed.read(
            std::io::BufReader::new(std::fs::File::open(&path)?),
            &mut subs,
        )?;
    }
    let mut zones: Vec<_> = c.zones().iter().collect();
    zones.sort_by_key(|(_, z)| std::cmp::Reverse(z.attempts()));
    for (name, z) in &zones {
        println!(
            "{name}  {} of {} attempt(s) yielded something",
            z.hits(),
            z.attempts()
        );
        let mut found: Vec<_> = z.found.iter().collect();
        found.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (item, n) in found {
            println!("    {n:3}  {item}");
        }
    }
    println!(
        "\n{} zone(s); {} attempt(s) before any zone was announced",
        zones.len(),
        c.homeless
    );
    Ok(())
}
