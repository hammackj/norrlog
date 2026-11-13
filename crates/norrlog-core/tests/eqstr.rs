//! The melee rules, checked against the client's own string table.
//!
//! The client ships every message it can print as a template in
//! `eqstr_us.txt`, and the melee vocabulary is ids 12163–12202: every verb,
//! second person and third, plus `frenzy on` / `frenzies on` at 5838–5839.
//! The parser's rules are written by hand against observed lines; this walks
//! the table and builds every sentence the templates can make, so a verb the
//! rules do not know fails here instead of inventing a creature. The frenzy
//! bug, which read "frenzies on a cracked skeleton" as a creature named "on a
//! cracked skeleton", is exactly the kind of thing it catches.
//!
//! The file belongs to the game and is not in the repository. Point
//! `NORRLOG_EQSTR` at one or more copies (a path list, as `PATH` is: both
//! games ship their own) to run it; without it the test says so and passes.
//!
//!     NORRLOG_EQSTR="<EverQuest>/eqstr_us.txt:<EverQuest Legends>/eqstr_us.txt" \
//!         cargo test -p norrlog-core --test eqstr

use norrlog_core::event::{Actor, DamageKind, Event};
use norrlog_core::parser::Parser;
use std::collections::BTreeMap;

/// The table's `id message` lines.
fn read(path: &std::path::Path) -> BTreeMap<u32, String> {
    let text = std::fs::read(path).expect("reading eqstr_us.txt");
    let text = String::from_utf8_lossy(&text);
    text.lines()
        .skip(2)
        .filter_map(|l| {
            let (id, msg) = l.split_once(' ')?;
            Some((id.parse().ok()?, msg.trim_end().to_string()))
        })
        .collect()
}

/// (second person, third person) for every melee verb in the table.
fn verbs(table: &BTreeMap<u32, String>) -> Vec<(String, String)> {
    // 12185–12190 are the pronouns sitting in the middle of the block.
    let words: Vec<&str> = (12163..=12202)
        .filter(|id| !(12185..=12190).contains(id))
        .filter_map(|id| table.get(&id).map(String::as_str))
        .collect();
    let mut out: Vec<(String, String)> = words
        .iter()
        .filter_map(|w| {
            let third = [format!("{w}s"), format!("{w}es")]
                .into_iter()
                .find(|t| words.contains(&t.as_str()))?;
            Some((w.to_string(), third))
        })
        .collect();
    if let (Some(a), Some(b)) = (table.get(&5838), table.get(&5839)) {
        out.push((a.clone(), b.clone()));
    }
    out
}

fn named(a: &Actor, want: &str) -> bool {
    matches!(a, Actor::Named(n) if n.eq_ignore_ascii_case(want))
}

#[test]
fn every_melee_verb_in_the_string_table_parses() {
    let Some(paths) = std::env::var_os("NORRLOG_EQSTR") else {
        eprintln!("NORRLOG_EQSTR is not set; skipping the string-table check");
        return;
    };
    let p = Parser::new("Tharn");
    let mut failures = Vec::new();
    let mut checked = 0;
    for path in std::env::split_paths(&paths) {
        let table = read(&path);
        let vs = verbs(&table);
        assert!(
            vs.len() >= 18,
            "{}: expected the melee block, found {} verbs: {vs:?}",
            path.display(),
            vs.len()
        );
        for (second, third) in &vs {
            // The base form is what follows "tries to", in either person.
            let base = second.as_str();
            let cases: Vec<(String, Box<dyn Fn(&Event) -> bool>)> = vec![
                (
                    format!("You {second} a gnoll scout for 12 points of damage."),
                    Box::new(|e| {
                        matches!(e, Event::Damage { source: Actor::You, target, amount: 12, kind: DamageKind::Melee, .. }
                            if named(target, "a gnoll scout"))
                    }),
                ),
                (
                    format!("A gnoll scout {third} YOU for 1 point of damage."),
                    Box::new(|e| {
                        matches!(e, Event::Damage { source, target: Actor::You, amount: 1, kind: DamageKind::Melee, .. }
                            if named(source, "a gnoll scout"))
                    }),
                ),
                (
                    format!("A gnoll scout {third} a cracked skeleton for 30 points of damage."),
                    Box::new(|e| {
                        matches!(e, Event::Damage { source, target, amount: 30, .. }
                            if named(source, "a gnoll scout") && named(target, "a cracked skeleton"))
                    }),
                ),
                (
                    format!("You try to {base} a gnoll scout, but miss!"),
                    Box::new(|e| {
                        matches!(e, Event::Miss { source: Actor::You, target, .. }
                            if named(target, "a gnoll scout"))
                    }),
                ),
                (
                    format!("A gnoll scout tries to {base} YOU, but misses!"),
                    Box::new(|e| {
                        matches!(e, Event::Miss { source, target: Actor::You, .. }
                            if named(source, "a gnoll scout"))
                    }),
                ),
                (
                    format!("A gnoll scout tries to {base} YOU, but YOU dodge!"),
                    Box::new(|e| {
                        matches!(e, Event::Avoided { source, target: Actor::You, .. }
                            if named(source, "a gnoll scout"))
                    }),
                ),
            ];
            for (line, ok) in cases {
                checked += 1;
                let e = p.parse_body(&line);
                if !ok(&e) {
                    failures.push(format!("{}: {line:?} -> {e:?}", path.display()));
                }
            }
        }
    }
    eprintln!("checked {checked} lines built from the string table");
    assert!(
        failures.is_empty(),
        "{} line(s) the rules do not read as the table means them:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
