//! One fight opened up: the Encounters detail. Ports `showEncounter` in
//! `encounters.js` and the panels it draws with (`panels.js`); the checklist
//! is the Layout and Formatting sections of `docs/parity/encounters-and-procs.md`.
//!
//! Fetched once when opened and never refreshed: a closed fight does not
//! change. The two charts are in `charts.rs`.

use crate::encounters::EncountersView;
use crate::format::{dur, num, when};
use crate::table::{self, Col, col, muted, tag};
use crate::theme::*;
use eqlparser_api::{AbilityGroup, AbilityStats, ActorStats, EncounterDetail, MeterRow};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};
use std::collections::{HashMap, HashSet};

/// What the detail remembers while it is open. Nothing persists past it.
#[derive(Default)]
pub struct FightUi {
    /// The Incoming subtab rather than Outgoing.
    pub incoming: bool,
    /// Meter rows whose ability breakdown is open, by actor key.
    pub open: HashSet<String>,
}

const GROUP_COLS: &[Col] = &[
    col("Ability", None, false),
    col("Damage", Some(50.), true),
    col("DPS", Some(34.), true),
    col("% of group", Some(68.), true),
    col("Casts", Some(36.), true),
    col("Hits", Some(30.), true),
    col("Avg", Some(34.), true),
    col("Max", Some(34.), true),
    col("Crit", Some(32.), true),
];
const FLAT_COLS: &[Col] = &[
    col("Ability", None, false),
    col("Damage", Some(64.), true),
    col("%", Some(48.), true),
    col("Hits", Some(40.), true),
    col("Avg", Some(44.), true),
    col("Max", Some(44.), true),
    col("Crit", Some(40.), true),
];
const CRIT_COLS: &[Col] =
    &[col("Type", None, false), col("Hits", Some(60.), true), col("Crits", Some(60.), true), col("Rate", Some(60.), true)];
const LEDGER_COLS: &[Col] = &[col("", None, false), col("", Some(60.), true), col("", Some(56.), true)];
const HEAL_COLS: &[Col] = &[
    col("Heal spell", None, false),
    col("Healed", Some(64.), true),
    col("%", Some(48.), true),
    col("Casts", Some(44.), true),
    col("Ticks", Some(44.), true),
    col("Avg", Some(44.), true),
    col("Max", Some(44.), true),
];
const MOB_COLS: &[Col] = &[
    col("Creature", None, false),
    col("Killed", Some(56.), true),
    col("Damage into it", Some(110.), true),
    col("Its output", Some(80.), true),
    col("Loot", Some(44.), true),
    col("Resists", Some(56.), true),
];
const RESIST_COLS: &[Col] =
    &[col("Spell", None, false), col("Resisted", Some(70.), true), col("Landed", Some(60.), true), col("Rate", Some(50.), true)];
const DEATH_COLS: &[Col] = &[col("Victim", None, false), col("Killed by", None, false), col("When", Some(130.), false)];
const LOOT_COLS: &[Col] = &[col("Item", None, false), col("Qty", Some(40.), true), col("From", None, false)];

const NAME_SHARED: &str = "At least two creatures in this fight answered to this name — the log gives no id, so \
                           their damage, healing and deaths are pooled here and cannot be separated.";

impl EncountersView {
    pub(crate) fn render_fight(&self, d: &EncounterDetail, cx: &mut Context<Self>) -> Div {
        let s = &d.summary;
        let e = &d.encounter;
        let you = e.actors.get("you");

        let mut sub = h_flex().gap_2().text_color(hex(MUTED)).child(s.zone.clone().unwrap_or_else(|| "unknown zone".into()));
        if s.difficulty > 0 {
            sub = sub.child(tag(format!("d{}", s.difficulty), PET));
        }
        sub = sub.child(format!("· {} · {}", when(Some(s.start)), dur(s.duration as f64)));
        if !s.build.is_empty() {
            sub = sub.child("·").child(tag(s.build.clone(), ACCENT));
        }

        let mut meter_card = card("Damage meter").child(self.meter(&s.meter, &e.actors, cx));
        if let Some(line) = pet_line(d) {
            meter_card = meter_card.child(hint(line));
        }
        meter_card = meter_card.child(crit_breakdown(&s.meter, s.duration));

        let healing = {
            let mut c = card("Healing").child(metric(&s.meter, Metric::Healing, "No healing in this fight"));
            if let Some(t) = you.and_then(heal_spells) {
                c = c.child(div().mt_2().child(t));
            }
            c
        };

        let mut out = v_flex()
            .gap_3()
            .child(v_flex().gap_1().child(div().text_lg().font_weight(FontWeight::BOLD).child(s.title.clone())).child(sub))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_3()
                    .child(stat("Your DPS", num(Some(s.your_dps)), None))
                    .child(stat("Your damage", num(Some(s.your_damage as f64)), None))
                    .child(stat(
                        "Damage to enemy",
                        num(Some(s.damage_to_enemies as f64)),
                        Some(format!("{} both sides", num(Some(s.total_damage as f64)))),
                    ))
                    .child(stat(
                        "Your healing",
                        num(Some(s.your_healing as f64)),
                        Some(format!("{} hps", num(Some(s.your_hps)))),
                    ))
                    .child(stat("Damage taken", num(Some(s.your_damage_taken as f64)), None)),
            )
            .children(crate::charts::dps_card(e, s))
            .child(split(meter_card, self.damage_panes(you, &s.outgoing_groups, cx)))
            .child(split(healing, card("Damage taken").child(metric(&s.meter, Metric::Taken, "Nobody took damage"))))
            .child(crate::charts::timeline_card(e, s));

        if s.mobs.len() >= 2 {
            out = out.child(mobs_card(&s.mobs));
        }
        if !s.resists.is_empty() {
            out = out.child(resists_card(&s.resists));
        }
        out.child(split(deaths_card(&e.deaths), loot_card(e)))
    }

    /// One row per actor that dealt damage. A row whose actor has abilities
    /// opens to their breakdown, as the renderer's `wireDrilldown`.
    fn meter(&self, meter: &[MeterRow], actors: &HashMap<String, ActorStats>, cx: &mut Context<Self>) -> Div {
        let rows = fold_pets(meter.iter().filter(|r| r.damage > 0).cloned().collect());
        if rows.is_empty() {
            return empty("No damage recorded");
        }
        let mut out = v_flex().gap_1();
        for (r, folded) in rows {
            let abilities = actors.get(&r.key).filter(|_| !r.key.is_empty()).and_then(|st| ability_rows(st, r.damage));
            let open = self.fight.open.contains(&r.key);
            let mut name = h_flex().gap_1().min_w_0().overflow_hidden().font_weight(FontWeight::MEDIUM);
            if abilities.is_some() {
                name = name.child(div().text_color(hex(MUTED)).child(if open { "▾" } else { "▸" }));
            }
            name = name.child(div().text_color(hex(side_name(&r))).whitespace_nowrap().text_ellipsis().child(r.name.clone()));
            if let Some((label, color)) = pet_tag(&r, folded) {
                name = name.child(tag(label, color));
            }
            if r.name_shared {
                name = name.child(
                    div()
                        .id(SharedString::from(format!("shared-{}", r.key)))
                        .child(tag("2+", MUTED))
                        .tooltip(|window, cx| Tooltip::new(NAME_SHARED).build(window, cx)),
                );
            }
            let nums = h_flex()
                .gap_3()
                .flex_none()
                .child(muted(format!("{:.0}% crit", r.crit_rate * 100.)))
                .child(muted(num(Some(r.damage as f64))))
                .child(div().min_w(px(52.)).flex().justify_end().font_weight(FontWeight::SEMIBOLD).child(num(Some(r.dps))));
            let mut row =
                bar(r.share, side_bar(&r)).id(SharedString::from(format!("meter-{}", r.key))).child(name).child(nums);
            if abilities.is_some() {
                let key = r.key.clone();
                row = row
                    .cursor_pointer()
                    .tooltip(|window, cx| Tooltip::new("Click for the ability breakdown").build(window, cx))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        if !v.fight.open.remove(&key) {
                            v.fight.open.insert(key.clone());
                        }
                        cx.notify();
                    }));
            }
            out = out.child(row);
            if let (true, Some(list)) = (open, abilities) {
                out = out.child(list);
            }
        }
        out
    }

    /// Outgoing and Incoming, two panes of one card: the same question from
    /// opposite directions.
    fn damage_panes(&self, you: Option<&ActorStats>, groups: &[AbilityGroup], cx: &mut Context<Self>) -> Div {
        let incoming = self.fight.incoming;
        let subtab = |id: &'static str, label: &'static str, count: u64, active: bool, to_in: bool| {
            div()
                .id(id)
                .flex()
                .gap_1()
                .px_3()
                .py_1()
                .rounded(px(5.))
                .cursor_pointer()
                .when(active, |d| d.bg(hex(BG)).text_color(hex(ACCENT)))
                .when(!active, |d| d.text_color(hex(MUTED)).hover(|s| s.bg(wash(TEXT, 0.05)).text_color(hex(TEXT))))
                .child(label)
                .child(div().text_color(hex(MUTED)).child(num(Some(count as f64))))
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.fight.incoming = to_in;
                    cx.notify();
                }))
        };
        let tabs = h_flex()
            .gap_1()
            .p(px(3.))
            .rounded(px(7.))
            .bg(hex(PANEL_2))
            .child(subtab("pane-out", "Outgoing", you.map_or(0, |y| y.damage_done), !incoming, false))
            .child(subtab("pane-in", "Incoming", you.map_or(0, |y| y.damage_taken), incoming, true));
        let body = if incoming { incoming_pane(you) } else { outgoing_pane(you, groups) };
        panel().child(div().flex().child(tabs)).child(body)
    }
}

// ---------------------------------------------------------------------------
// Building blocks.

fn panel() -> Div {
    v_flex().gap_2().p_3().rounded(px(8.)).border_1().border_color(hex(BORDER)).bg(hex(PANEL)).min_w_0()
}

fn card(title: &str) -> Div {
    panel().child(heading(title))
}

fn heading(title: &str) -> Div {
    div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child(title.to_uppercase())
}

fn sub_heading(title: impl Into<SharedString>) -> Div {
    div().mt_2().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child(title.into())
}

fn empty(text: &'static str) -> Div {
    div().py_2().text_color(hex(MUTED)).child(text)
}

fn hint(text: impl Into<SharedString>) -> Div {
    div().text_sm().text_color(hex(MUTED)).child(text.into())
}

/// Two cards side by side, wrapping to one column when the window is narrow.
fn split(a: Div, b: Div) -> Div {
    div().flex().flex_row().flex_wrap().gap_3().items_start().child(a.flex_1().min_w(px(420.))).child(b.flex_1().min_w(px(420.)))
}

fn stat(label: &'static str, value: String, note: Option<String>) -> Div {
    let mut c = v_flex()
        .gap_1()
        .p_3()
        .min_w(px(150.))
        .rounded(px(8.))
        .border_1()
        .border_color(hex(BORDER))
        .bg(hex(PANEL))
        .child(div().text_xs().text_color(hex(MUTED)).child(label.to_uppercase()))
        .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(value));
    if let Some(note) = note {
        c = c.child(div().text_xs().text_color(hex(MUTED)).child(note));
    }
    c
}

/// A meter row: a bar `share` wide behind a name and its numbers, the
/// renderer's `.row` with its `::before`.
fn bar(share: f64, color: Hsla) -> Div {
    h_flex()
        .relative()
        .justify_between()
        .gap_3()
        .px(px(9.))
        .py(px(5.))
        .rounded(px(5.))
        .bg(hex(PANEL_2))
        .overflow_hidden()
        .child(div().absolute().top_0().bottom_0().left_0().w(relative(share.clamp(0., 1.) as f32)).bg(color))
}

/// A table row with the ability-table bar behind it, `share` of the whole.
fn bar_row(id: impl Into<ElementId>, cols: &[Col], cells: Vec<AnyElement>, share: f64) -> Stateful<Div> {
    table::row_dense(id, cols, cells).relative().child(
        div().absolute().top_0().bottom_0().left_0().w(relative(share.clamp(0., 1.) as f32)).bg(wash(YOU, 0.22)),
    )
}

fn table_box(cols: &[Col], rows: Vec<AnyElement>) -> Div {
    v_flex().rounded(px(6.)).border_1().border_color(hex(BORDER)).overflow_hidden().child(table::head_dense(cols)).children(rows)
}

fn cell(s: impl Into<SharedString>) -> AnyElement {
    div().child(s.into()).into_any_element()
}

fn dim(s: impl Into<SharedString>) -> AnyElement {
    muted(s).into_any_element()
}

fn side_name(r: &MeterRow) -> u32 {
    if r.is_you {
        YOU
    } else if r.is_pet {
        PET
    } else {
        TEXT
    }
}

fn side_bar(r: &MeterRow) -> Hsla {
    if r.is_you {
        wash(YOU, 0.3)
    } else if r.is_pet {
        wash(PET, 0.28)
    } else if r.is_enemy {
        wash(ENEMY, 0.22)
    } else {
        wash(TEXT, 0.1)
    }
}

// ---------------------------------------------------------------------------
// Panels.

enum Metric {
    Healing,
    Taken,
}

/// `metricHtml`: one row set, filtered and sorted per metric.
fn metric(meter: &[MeterRow], m: Metric, empty_text: &'static str) -> Div {
    let value = |r: &MeterRow| match m {
        Metric::Healing => r.healing,
        Metric::Taken => r.damage_taken,
    };
    let mut rows: Vec<&MeterRow> = meter.iter().filter(|r| value(r) > 0).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(value(r)));
    if rows.is_empty() {
        return empty(empty_text);
    }
    v_flex().gap_1().children(rows.into_iter().map(|r| {
        let (share, rate) = match m {
            Metric::Healing => (r.heal_share, Some(r.hps)),
            Metric::Taken => (r.taken_share, None),
        };
        let mut nums = h_flex().gap_3().flex_none().child(muted(num(Some(value(r) as f64))));
        if let Some(rate) = rate {
            nums = nums.child(
                h_flex().gap_1().font_weight(FontWeight::SEMIBOLD).child(num(Some(rate))).child(muted("hps")),
            );
        }
        bar(share, side_bar(r))
            .child(div().font_weight(FontWeight::MEDIUM).text_color(hex(side_name(r))).child(r.name.clone()))
            .child(nums)
    }))
}

/// `outgoingPaneHtml`: your damage grouped by mechanism, or the flat list
/// for a fight parsed before abilities carried a kind.
fn outgoing_pane(you: Option<&ActorStats>, groups: &[AbilityGroup]) -> Div {
    let Some(you) = you.filter(|y| y.damage_done > 0) else {
        return empty("No damage dealt");
    };
    let total = you.damage_done as f64;
    if groups.is_empty() {
        let mut list: Vec<(&String, &AbilityStats)> = you.by_ability.iter().collect();
        list.sort_by(|a, b| b.1.total.cmp(&a.1.total).then_with(|| a.0.cmp(b.0)));
        let rows = list
            .into_iter()
            .take(16)
            .map(|(name, a)| {
                bar_row(
                    SharedString::from(format!("flat-{name}")),
                    FLAT_COLS,
                    vec![
                        cell(name.clone()),
                        cell(num(Some(a.total as f64))),
                        dim(format!("{:.1}%", a.total as f64 / total * 100.)),
                        dim(a.hits.to_string()),
                        cell(format!("{:.0}", if a.hits > 0 { a.total as f64 / a.hits as f64 } else { 0. })),
                        cell(a.max.to_string()),
                        dim(format!("{:.0}%", crit_pct(a.crits, a.hits))),
                    ],
                    a.total as f64 / total,
                )
                .into_any_element()
            })
            .collect();
        return v_flex().child(table_box(FLAT_COLS, rows));
    }
    let mut out = v_flex().gap_1();
    for g in groups {
        out = out.child(
            h_flex()
                .mt_2()
                .justify_between()
                .gap_2()
                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(g.group.to_uppercase()))
                .child(div().text_xs().text_color(hex(MUTED)).child(format!(
                    "{} · {} dps · {:.0}% of your damage",
                    num(Some(g.damage as f64)),
                    dps_text(Some(g.dps)),
                    g.share * 100.
                ))),
        );
        let rows = g
            .abilities
            .iter()
            .map(|a| {
                bar_row(
                    SharedString::from(format!("og-{}-{}", g.group, a.name)),
                    GROUP_COLS,
                    vec![
                        cell(a.name.clone()),
                        cell(num(Some(a.damage as f64))),
                        cell(dps_text(Some(a.dps))),
                        dim(format!("{:.1}%", a.share * 100.)),
                        dim(if a.casts > 0 { num(Some(a.casts as f64)) } else { "—".into() }),
                        dim(a.hits.to_string()),
                        cell(format!("{:.0}", a.average)),
                        cell(a.max.to_string()),
                        dim(format!("{:.0}%", crit_pct(a.crits, a.hits))),
                    ],
                    a.damage as f64 / total,
                )
                .into_any_element()
            })
            .collect();
        out = out.child(table_box(GROUP_COLS, rows));
    }
    out
}

/// `incomingPaneHtml`: what hit you, three ways.
fn incoming_pane(you: Option<&ActorStats>) -> Div {
    let Some(you) = you.filter(|y| y.damage_taken > 0) else {
        return empty("You took no damage");
    };
    let total = you.damage_taken as f64;
    let section = |title: &'static str, list: Vec<(String, u64, u64, u64)>| {
        let mut list: Vec<_> = list.into_iter().filter(|x| x.1 > 0).collect();
        list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        list.truncate(12);
        let body = if list.is_empty() {
            empty("None")
        } else {
            v_flex().gap_1().children(list.into_iter().map(|(name, t, hits, max)| {
                let mut nums = h_flex().gap_3().flex_none();
                if hits > 0 {
                    nums = nums.child(muted(format!("{} hits · max {}", num(Some(hits as f64)), num(Some(max as f64)))));
                }
                bar(t as f64 / total, wash(ENEMY, 0.22))
                    .child(div().font_weight(FontWeight::MEDIUM).child(name))
                    .child(nums.child(div().font_weight(FontWeight::SEMIBOLD).child(num(Some(t as f64)))))
            }))
        };
        v_flex().gap_1().child(sub_heading(title.to_uppercase())).child(body)
    };
    let (swings, avoid) = avoidance(you);
    v_flex()
        .gap_1()
        .child(hint(format!(
            "Took {} · avoided {:.0}% of {} attacks · {}",
            num(Some(total)),
            avoid,
            num(Some(swings as f64)),
            if you.deaths > 0 { format!("died {}×", you.deaths) } else { "survived".into() }
        )))
        .child(section("By type", you.damage_taken_by_kind.iter().map(|(k, v)| (k.clone(), *v, 0, 0)).collect()))
        .child(section("By attacker", you.damage_taken_by_source.iter().map(|(k, v)| (k.clone(), *v, 0, 0)).collect()))
        .child(section(
            "By attack",
            you.damage_taken_by_ability.iter().map(|(k, a)| (k.clone(), a.total, a.hits, a.max)).collect(),
        ))
}

/// `critBreakdownHtml`: crit rate per mechanism, the swing ledgers, and the
/// lines that explain a fight's numbers (stuns, extra attacks, overhealing).
fn crit_breakdown(meter: &[MeterRow], duration: i64) -> Div {
    let Some(you) = meter.iter().find(|r| r.is_you) else { return div() };
    if you.crit_by_kind.is_empty() && you.heals == 0 {
        return div();
    }
    let mut rows: Vec<AnyElement> = you
        .crit_by_kind
        .iter()
        .map(|(kind, hits, crits, rate)| {
            table::row_dense(
                SharedString::from(format!("crit-{kind}")),
                CRIT_COLS,
                vec![cell(kind.clone()), cell(int(*hits)), cell(int(*crits)), cell(format!("{:.1}%", rate * 100.))],
            )
            .into_any_element()
        })
        .collect();
    if you.heals > 0 {
        rows.push(
            table::row_dense(
                "crit-healing",
                CRIT_COLS,
                vec![
                    div().text_color(hex(YOU)).child("healing").into_any_element(),
                    cell(int(you.heals)),
                    cell(int(you.heal_crits)),
                    cell(format!("{:.1}%", you.heal_crit_rate * 100.)),
                ],
            )
            .border_t_1()
            .border_color(hex(BORDER))
            .into_any_element(),
        );
    }
    let mut out = v_flex().gap_2().child(sub_heading("YOUR CRIT RATE BY TYPE")).child(table_box(CRIT_COLS, rows));
    for line in crit_hints(you, duration) {
        out = out.child(hint(line));
    }
    let ledgers = [
        ledger(&you.offence_outcomes, "Your swings", "Missed is you not connecting; the rest is the target stopping you."),
        ledger(&you.defence_outcomes, "Swings at you", "Everything below landed is avoidance — the part you can gear for."),
    ];
    if ledgers.iter().any(Option::is_some) {
        out = out.child(div().flex().flex_row().flex_wrap().gap_3().children(ledgers.into_iter().flatten()));
    }
    if let Some(line) = overheal_line(you) {
        out = out.child(hint(line));
    }
    out
}

/// What became of the swings one way: each outcome over the total.
fn ledger(rows: &[(String, u64)], title: &str, note: &'static str) -> Option<Div> {
    let total: u64 = rows.iter().map(|r| r.1).sum();
    if total == 0 {
        return None;
    }
    let mut sorted = rows.to_vec();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.1));
    let body = sorted
        .into_iter()
        .map(|(what, n)| {
            let share = n as f64 / total as f64;
            bar_row(
                SharedString::from(format!("ledger-{title}-{what}")),
                LEDGER_COLS,
                vec![cell(what), cell(int(n)), dim(format!("{:.1}%", share * 100.))],
                share,
            )
            .into_any_element()
        })
        .collect::<Vec<_>>();
    Some(
        v_flex()
            .flex_1()
            .min_w(px(200.))
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
                    .child(muted(format!("{} swings", int(total)))),
            )
            .child(v_flex().rounded(px(6.)).border_1().border_color(hex(BORDER)).overflow_hidden().children(body))
            .child(hint(note)),
    )
}

/// The heal-spell table under the healing meter: top 12.
fn heal_spells(you: &ActorStats) -> Option<Div> {
    let mut list: Vec<(&String, &AbilityStats)> = you.healing_by_spell.iter().collect();
    if list.is_empty() {
        return None;
    }
    list.sort_by(|a, b| b.1.total.cmp(&a.1.total).then_with(|| a.0.cmp(b.0)));
    let rows = list
        .into_iter()
        .take(12)
        .map(|(name, a)| {
            let share = if you.healing_done > 0 { a.total as f64 / you.healing_done as f64 * 100. } else { 0. };
            let mut n = h_flex().gap_1().child(name.clone());
            if a.kind == "hot" {
                n = n.child(tag("hot", PET));
            }
            table::row_dense(
                SharedString::from(format!("heal-{name}")),
                HEAL_COLS,
                vec![
                    n.into_any_element(),
                    cell(num(Some(a.total as f64))),
                    dim(format!("{share:.1}%")),
                    // A dash where the cast was never seen: a zero would
                    // claim it was never cast.
                    dim(if a.casts > 0 { num(Some(a.casts as f64)) } else { "—".into() }),
                    dim(a.hits.to_string()),
                    cell(format!("{:.0}", if a.hits > 0 { a.total as f64 / a.hits as f64 } else { 0. })),
                    cell(a.max.to_string()),
                ],
            )
            .into_any_element()
        })
        .collect();
    Some(table_box(HEAL_COLS, rows))
}

fn mobs_card(mobs: &[eqlparser_api::MobBreakdown]) -> Div {
    let rows = mobs
        .iter()
        .map(|m| {
            table::row_dense(
                SharedString::from(format!("fought-{}", m.name)),
                MOB_COLS,
                vec![
                    cell(m.name.clone()),
                    cell(blank(m.kills as u64)),
                    cell(num(Some(m.damage_done_to_it as f64))),
                    dim(num(Some(m.damage_it_did as f64))),
                    dim(blank(m.loot as u64)),
                    dim(blank(m.resists)),
                ],
            )
            .into_any_element()
        })
        .collect();
    card("What was fought").child(table_box(MOB_COLS, rows)).child(hint(
        "A fight is bounded by a lull in combat, so a pull that ran straight into the next one is a single \
         encounter. This splits it back out by creature without pretending they were separate fights.",
    ))
}

fn resists_card(resists: &[eqlparser_api::ResistTally]) -> Div {
    let rows = resists
        .iter()
        .map(|r| {
            let who = r
                .targets
                .iter()
                .map(|(t, n)| if *n > 1 { format!("{t} ×{n}") } else { t.clone() })
                .collect::<Vec<_>>()
                .join(", ");
            table::row_dense(
                SharedString::from(format!("resist-{}", r.spell)),
                RESIST_COLS,
                vec![
                    v_flex().child(r.spell.clone()).child(muted(who).text_sm()).into_any_element(),
                    cell(r.resisted.to_string()),
                    dim(r.landed.to_string()),
                    cell(resist_rate(r.resisted, r.landed)),
                ],
            )
            .into_any_element()
        })
        .collect();
    card("Resisted").child(table_box(RESIST_COLS, rows)).child(hint(
        "Landed counts come from your own ability breakdown, so a spell that sometimes worked carries a real \
         rate rather than a bare failure count. A spell that never landed shows no rate.",
    ))
}

fn deaths_card(deaths: &[eqlparser_api::DeathRecord]) -> Div {
    let c = card("Deaths");
    if deaths.is_empty() {
        return c.child(empty("Nothing died"));
    }
    let rows = deaths
        .iter()
        .enumerate()
        .map(|(i, x)| {
            table::row_dense(
                SharedString::from(format!("death-{i}")),
                DEATH_COLS,
                vec![cell(x.victim.clone()), dim(x.killer.clone().unwrap_or_else(|| "—".into())), dim(when(Some(x.timestamp)))],
            )
            .into_any_element()
        })
        .collect();
    c.child(table_box(DEATH_COLS, rows))
}

fn loot_card(e: &eqlparser_api::Encounter) -> Div {
    let c = card("Loot & coin");
    if e.loot.is_empty() && e.coin_copper == 0 {
        return c.child(empty("No loot"));
    }
    let mut c = c;
    if e.coin_copper > 0 {
        c = c.child(hint(format!("Coin: {}", coin_pp(e.coin_copper))));
    }
    let rows = e
        .loot
        .iter()
        .enumerate()
        .map(|(i, l)| {
            table::row_dense(
                SharedString::from(format!("loot-{i}")),
                LOOT_COLS,
                vec![cell(l.item.clone()), cell(l.quantity.to_string()), dim(l.source.clone().unwrap_or_else(|| "—".into()))],
            )
            .into_any_element()
        })
        .collect();
    c.child(table_box(LOOT_COLS, rows))
}

// ---------------------------------------------------------------------------
// The arithmetic and wording, kept apart from drawing so it can be tested.

/// An actor's own abilities, biggest first, as the renderer's drill list.
/// Shares are of that actor's damage: the question inside a row is where
/// *its* damage came from.
fn ability_rows(st: &ActorStats, total: u64) -> Option<Div> {
    let mut rows: Vec<(&String, &AbilityStats)> = st.by_ability.iter().filter(|(_, a)| a.total > 0).collect();
    if rows.is_empty() {
        return None;
    }
    rows.sort_by(|a, b| b.1.total.cmp(&a.1.total).then_with(|| a.0.cmp(b.0)));
    let sum = if total > 0 { total } else { rows.iter().map(|r| r.1.total).sum::<u64>().max(1) } as f64;
    Some(v_flex().gap(px(2.)).pl_4().children(rows.into_iter().map(|(name, a)| {
        bar(a.total as f64 / sum, wash(YOU, 0.18))
            .py(px(3.))
            .child(h_flex().gap_1().min_w_0().child(name.clone()).child(tag(if a.kind.is_empty() { "?".into() } else { a.kind.clone() }, ACCENT)))
            .child(
                h_flex()
                    .gap_3()
                    .flex_none()
                    .child(muted(drill_counts(a)))
                    .child(muted(format!(
                        "avg {} · max {}",
                        num(Some(if a.hits > 0 { (a.total as f64 / a.hits as f64).round() } else { 0. })),
                        num(Some(a.max as f64))
                    )))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(num(Some(a.total as f64)))),
            )
    })))
}

/// `{n} casts · {n} hits · {c}% crit`, casts only where they were recorded.
fn drill_counts(a: &AbilityStats) -> String {
    let casts = if a.casts > 0 { format!("{} cast{} · ", a.casts, plural(a.casts)) } else { String::new() };
    format!("{casts}{} hit{} · {:.0}% crit", a.hits, plural(a.hits), crit_pct(a.crits, a.hits))
}

/// Fold each creature's summoned pet into it, as `foldPets`: never your own
/// pets, never a charm, and only into an owner that is on the meter.
fn fold_pets(rows: Vec<MeterRow>) -> Vec<(MeterRow, usize)> {
    let mut out: Vec<(MeterRow, usize)> = rows.into_iter().map(|r| (r, 0)).collect();
    let mut absorbed = vec![false; out.len()];
    for i in 0..out.len() {
        let r = &out[i].0;
        if r.is_you || r.is_pet || r.pet_kind.as_deref() != Some("summoned") {
            continue;
        }
        let Some(owner) = r.pet_owner.as_deref() else { continue };
        let Some(o) = out.iter().position(|(x, _)| x.name == owner) else { continue };
        if o == i {
            continue;
        }
        let (damage, dps, share) = (r.damage, r.dps, r.share);
        let (row, n) = &mut out[o];
        row.damage += damage;
        row.dps += dps;
        row.share += share;
        *n += 1;
        absorbed[i] = true;
    }
    out.into_iter().zip(absorbed).filter(|(_, gone)| !gone).map(|(r, _)| r).collect()
}

/// `petTag`: what kind of pet a row is, and whose.
fn pet_tag(r: &MeterRow, folded: usize) -> Option<(String, u32)> {
    if folded > 0 {
        return Some((format!("+{folded} pet{}", plural(folded as u64)), MUTED));
    }
    let kind = r.pet_kind.as_deref()?;
    let label = if kind == "charmed" { "charmed" } else { "pet" };
    if r.is_pet {
        return Some((label.to_string(), PET));
    }
    Some((r.pet_owner.as_ref().map_or_else(|| label.to_string(), |o| format!("{label} {o}")), MUTED))
}

/// The sentence under the meter about what your pets added.
fn pet_line(d: &EncounterDetail) -> Option<String> {
    let pets = if d.your_pets.is_empty() { &d.charmed_pets } else { &d.your_pets };
    if pets.is_empty() {
        return None;
    }
    let s = &d.summary;
    let charmed = d.charmed_pets.len();
    // Only "charmed" when every one of them was: a magician's pet and a
    // bard's charm can share a meter.
    let all_charmed = charmed == pets.len() && charmed > 0;
    let word = match (all_charmed, charmed == 1, pets.len() == 1) {
        (true, true, _) => "Your charmed pet",
        (true, false, _) => "Charmed pets",
        (false, _, true) => "Your pet",
        (false, _, false) => "Your pets",
    };
    let own = d.encounter.actors.get("you").map_or(0, |y| y.damage_done);
    let with = d.your_damage_with_pets;
    let mut line = format!(
        "{word} contributed {} damage — your total including pets is {} ({} dps).",
        num(Some(with.saturating_sub(own) as f64)),
        num(Some(with as f64)),
        num(Some(if s.duration > 0 { with as f64 / s.duration as f64 } else { 0. })),
    );
    if charmed > 0 {
        line.push_str(&format!(" Charm held {} of the fight", charm_pct(s.charm_uptime, s.charm_breaks)));
        if s.charm_breaks > 0 {
            line.push_str(&format!(", breaking {} time{}.", s.charm_breaks, plural(s.charm_breaks as u64)));
        } else {
            line.push_str(" without breaking.");
        }
    }
    Some(line)
}

/// A broken charm did not hold all of the fight, however the rounding falls.
fn charm_pct(uptime: f64, breaks: u32) -> String {
    let pct = uptime.min(1.) * 100.;
    if breaks > 0 && pct > 99. { ">99%".into() } else { format!("{pct:.0}%") }
}

/// Swings at you and the share you avoided, as the Incoming pane's head.
fn avoidance(you: &ActorStats) -> (u64, f64) {
    let hits: u64 = you.damage_taken_by_ability.values().map(|a| a.hits).sum();
    let avoided = you.attacks_missed + you.avoided;
    let swings = hits + avoided;
    (swings, if swings > 0 { avoided as f64 / swings as f64 * 100. } else { 0. })
}

/// The cause, extra-attack and stun lines under the crit table.
fn crit_hints(you: &MeterRow, duration: i64) -> Vec<String> {
    let mut out = Vec::new();
    let causes: Vec<_> = you.crit_by_flag.iter().filter(|c| c.1 > 0).collect();
    if causes.len() > 1 {
        out.push(format!(
            "From {}. A slay is a critical against undead and only fires there, so it moves the melee rate on an \
             undead pull and not elsewhere.",
            causes.iter().map(|(f, n)| format!("{f} {}×", int(*n))).collect::<Vec<_>>().join(" · ")
        ));
    }
    let extras: Vec<_> = you.swings_by_flag.iter().filter(|c| c.1 > 0).collect();
    if !extras.is_empty() {
        let swings = you.melee_swings;
        out.push(format!(
            "Extra attacks: {}. Counted on hits and misses alike — a flurry that missed still fired.",
            extras
                .iter()
                .map(|(f, n)| {
                    let rate =
                        if swings > 0 { format!(" ({:.1}% of swings)", *n as f64 / swings as f64 * 100.) } else { String::new() };
                    format!("{f} {}×{rate}", int(*n))
                })
                .collect::<Vec<_>>()
                .join(" · ")
        ));
    }
    if you.stunned_seconds > 0 || you.stuns > 0 {
        let of = if duration > 0 {
            format!(
                " of {} ({:.0}% of the fight)",
                dur(duration as f64),
                you.stunned_seconds as f64 / duration as f64 * 100.
            )
        } else {
            String::new()
        };
        let more = if you.stuns_avoided > 0 { format!(", shrugging off {} more", you.stuns_avoided) } else { String::new() };
        out.push(format!(
            "Stunned for {}{of}, across {} stun{}{more}.",
            dur(you.stunned_seconds as f64),
            you.stuns,
            plural(you.stuns as u64)
        ));
    }
    out
}

/// Overhealing, over the heals that stated it: a floor, with its denominator.
fn overheal_line(you: &MeterRow) -> Option<String> {
    if you.over_heal_measured == 0 {
        return None;
    }
    Some(format!(
        "Overhealed {} of {} attempted ({:.0}%), measured on {} of {} heals — the client states the pre-cap figure \
         on some lines and not others, so the real waste is at least this much.",
        num(Some(you.over_healing as f64)),
        num(Some(you.healing_attempted as f64)),
        you.over_healing as f64 / you.healing_attempted.max(1) as f64 * 100.,
        int(you.over_heal_measured),
        int(you.heals)
    ))
}

/// `dps()`: one decimal under 10, where a dot at 8.4 and a proc at 8.9 are
/// different rows; a dash when there is no figure.
fn dps_text(v: Option<f64>) -> String {
    match v {
        None => "—".into(),
        Some(v) if v < 10. => format!("{v:.1}"),
        Some(v) => num(Some(v)),
    }
}

fn resist_rate(resisted: u64, landed: u64) -> String {
    let attempts = resisted + landed;
    if attempts == 0 { "—".into() } else { format!("{:.0}%", resisted as f64 / attempts as f64 * 100.) }
}

fn coin_pp(copper: u64) -> String {
    format!("{:.2} pp", copper as f64 / 1000.)
}

fn crit_pct(crits: u64, hits: u64) -> f64 {
    if hits > 0 { crits as f64 / hits as f64 * 100. } else { 0. }
}

/// `toLocaleString()` for a count: grouped, no decimals.
fn int(n: u64) -> String {
    num(Some(n as f64))
}

fn blank(n: u64) -> String {
    if n == 0 { String::new() } else { n.to_string() }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::{MeterRow, charm_pct, coin_pp, dps_text, drill_counts, fold_pets, pet_tag, resist_rate};
    use eqlparser_api::AbilityStats;

    fn row(name: &str, damage: u64) -> MeterRow {
        serde_json::from_value(serde_json::json!({
            "name": name, "key": name.to_lowercase(), "damage": damage, "dps": damage as f64 / 10.0,
            "share": 0.1, "healing": 0, "hps": 0.0, "heal_share": 0.0, "damage_taken": 0, "taken_share": 0.0,
            "healing_received": 0, "crit_rate": 0.0, "crit_by_kind": [], "crit_by_flag": [], "heals": 0,
            "heal_crits": 0, "heal_crit_rate": 0.0, "over_healing": 0, "healing_attempted": 0,
            "over_heal_measured": 0, "accuracy": 0.0, "is_you": false, "is_enemy": true, "is_pet": false,
            "pet_kind": null, "pet_owner": null, "pet_damage": 0
        }))
        .unwrap()
    }

    #[test]
    fn a_creatures_summoned_pet_folds_into_it_and_nothing_else_does() {
        let king = row("King Tranix", 100);
        let mut pet = row("Kabanab", 40);
        pet.pet_kind = Some("summoned".into());
        pet.pet_owner = Some("King Tranix".into());
        let mut charm = row("a gnoll", 30);
        charm.pet_kind = Some("charmed".into());
        charm.pet_owner = Some("King Tranix".into());
        let mut orphan = row("a familiar", 5);
        orphan.pet_kind = Some("summoned".into());
        orphan.pet_owner = Some("Nobody Here".into());

        let out = fold_pets(vec![king, pet, charm, orphan]);
        let names: Vec<_> = out.iter().map(|(r, _)| r.name.as_str()).collect();
        assert_eq!(names, ["King Tranix", "a gnoll", "a familiar"]);
        assert_eq!(out[0].0.damage, 140);
        assert_eq!(out[0].1, 1);
        assert_eq!(pet_tag(&out[0].0, out[0].1).unwrap().0, "+1 pet");
        assert_eq!(pet_tag(&out[1].0, 0).unwrap().0, "charmed King Tranix");
    }

    #[test]
    fn a_broken_charm_never_reads_as_held_all_fight() {
        assert_eq!(charm_pct(0.996, 1), ">99%");
        assert_eq!(charm_pct(0.996, 0), "100%");
        assert_eq!(charm_pct(1.4, 0), "100%");
        assert_eq!(charm_pct(0.5, 2), "50%");
    }

    #[test]
    fn numbers_read_as_the_renderer_writes_them() {
        assert_eq!(dps_text(Some(8.44)), "8.4");
        assert_eq!(dps_text(Some(412.4)), "412");
        assert_eq!(dps_text(None), "—");
        assert_eq!(resist_rate(3, 1), "75%");
        assert_eq!(resist_rate(0, 0), "—");
        assert_eq!(coin_pp(5353), "5.35 pp");
        let a = AbilityStats { total: 300, hits: 3, crits: 1, casts: 1, ..Default::default() };
        assert_eq!(drill_counts(&a), "1 cast · 3 hits · 33% crit");
        let swing = AbilityStats { total: 10, hits: 1, ..Default::default() };
        assert_eq!(drill_counts(&swing), "1 hit · 0% crit");
    }
}
