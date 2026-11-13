//! Procs: how often weapon, buff and worn-item effects fire, per minute of
//! combat. Ports `loadProcs` (experience.js); the checklist is the Procs
//! section of the parity inventory.
//!
//! Fetched when the tab opens and again as snapshots arrive, at most every
//! ten seconds, while it is the tab on screen: the renderer's shared refresh.

use crate::api::Api;
use crate::format::{dur, num};
use crate::theme::*;
use norrlog_api::{ProcKind, ProcReport, ProcRow};
use gpui::prelude::FluentBuilder;
use gpui::*;
use crate::theme::px;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{h_flex, v_flex};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(10);
const ROWS: usize = 30;

pub struct ProcsView {
    api: Option<Api>,
    report: Option<ProcReport>,
    /// A failed fetch. Shown in the weapon list; the cards keep what they had.
    error: Option<String>,
    last_load: Option<Instant>,
    loading: Option<Task<()>>,
    /// The "How these are counted" note.
    note_open: bool,
}

impl ProcsView {
    pub fn new() -> Self {
        ProcsView { api: None, report: None, error: None, last_load: None, loading: None, note_open: false }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.api = api;
        cx.notify();
    }

    pub fn load(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_load = Some(Instant::now());
        self.loading = Some(cx.spawn(async move |this, cx| {
            let got = api.get::<ProcReport>("/api/procs".to_string()).await;
            let _ = this.update(cx, |v, cx| {
                match got {
                    Ok(r) => {
                        v.report = Some(r);
                        v.error = None;
                    }
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        }));
    }

    /// A snapshot arrived: refresh if this tab is on screen and the last
    /// fetch is ten seconds old.
    pub fn on_snapshot(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.load(cx);
        }
    }
}

impl Render for ProcsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let r = self.report.as_ref();
        let dash = || "—".to_string();
        let cards = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_3()
            .child(stat("Procs", r.map_or_else(dash, |r| num(Some(r.total as f64))), r.map(known)))
            .child(stat(
                "Per minute of combat",
                r.map_or_else(dash, |r| format!("{:.2}/min", r.per_minute)),
                r.map(|r| format!("over {} of combat, out of {} played", dur(r.combat_seconds as f64), dur(r.logged_seconds as f64))),
            ))
            .child(stat(
                "Damage from procs",
                r.map_or_else(dash, |r| num(Some(r.damage as f64))),
                Some("Across every creature they hit.".into()),
            ));

        let (weapon, items): (Vec<&ProcRow>, Vec<&ProcRow>) =
            r.map(|r| r.rows.iter().partition(|p| p.kind != ProcKind::Item)).unwrap_or_default();
        let have_spells = r.is_none_or(|r| r.have_spell_data);
        let weapon_body = match &self.error {
            Some(e) => empty(e.clone()),
            None => rows(&weapon, if have_spells { "Nothing has procced yet" } else { "Needs the spell database" }),
        };

        let mut out = v_flex().gap_3().child(cards);
        if !have_spells {
            out = out.child(hint(
                "No spell database loaded, so weapon and buff procs cannot be told apart from spells you cast. Only \
                 worn item procs are counted below. Set the game directory in Settings to see the rest.",
            ));
        }
        out = out
            .child(card("Weapon and buff procs").child(weapon_body).when(!weapon.is_empty(), |d| d.child(hint(subtotal(&weapon)))))
            .child(
                card("Worn item procs")
                    .child(rows(&items, "No worn item has procced yet"))
                    .when(!items.is_empty(), |d| d.child(hint(subtotal(&items)))),
            );
        if let Some(r) = r.filter(|r| !r.ambiguous.is_empty()) {
            let amb: Vec<&ProcRow> = r.ambiguous.iter().collect();
            out = out.child(card("Could be either").child(rows(&amb, "")).child(hint(
                "Effects that are procs on some gear but are also ordinary spells one of your own classes can cast. \
                 The log writes both the same way, so these are listed rather than counted.",
            )));
        }
        let open = self.note_open;
        let out = out.child(
            panel()
                .child(
                    h_flex()
                        .id("pr-note")
                        .gap_1()
                        .cursor_pointer()
                        .child(div().text_color(hex(MUTED)).child(if open { "▾" } else { "▸" }))
                        .child("How these are counted")
                        .on_click(cx.listener(|v, _, _, cx| {
                            v.note_open = !v.note_open;
                            cx.notify();
                        })),
                )
                .when(open, |d| {
                    d.child(hint(
                        "Which spells are procs is read from spells_us.txt, which states outright what each buff and \
                         weapon grants — so nothing here is inferred from how a name looks or how often it appears.",
                    ))
                    .child(hint(
                        "A proc is counted from its cast line, never its damage. An area proc writes one damage line \
                         per creature it catches: Earthquake once fired 21 times in a night and produced 2,825 damage \
                         lines. A proc that buffs rather than hurts writes no damage line at all. Damage is still \
                         totalled — it is simply not the count.",
                    ))
                    .child(hint(
                        "Rates are per minute of combat, the summed length of every fight. An evening is mostly \
                         running and buffing, and a proc rate charged for that time describes nothing.",
                    ))
                }),
        );
        div().id("procs-scroll").size_full().overflow_y_scrollbar().child(out)
    }
}

fn known(r: &ProcReport) -> String {
    if r.have_spell_data { format!("{} procs known from the spell file", num(Some(r.known_procs as f64))) } else { "item procs only".into() }
}

/// A list's rows, at most 30, bars scaled to the list's own fastest: the two
/// lists are on different scales, and one maximum would flatten the weapon
/// procs to nothing.
fn rows(list: &[&ProcRow], empty_text: &'static str) -> Div {
    if list.is_empty() {
        return empty(empty_text);
    }
    let max = list.iter().map(|r| r.per_minute).fold(1e-9, f64::max);
    v_flex().gap_1().children(list.iter().take(ROWS).map(|r| {
        h_flex()
            .relative()
            .justify_between()
            .gap_3()
            .px(px(9.))
            .py(px(5.))
            .rounded(px(5.))
            .bg(hex(PANEL_2))
            .overflow_hidden()
            .child(div().absolute().top_0().bottom_0().left_0().w(relative((r.per_minute / max) as f32)).bg(wash(TEXT, 0.1)))
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .overflow_hidden()
                    .child(div().font_weight(FontWeight::MEDIUM).whitespace_nowrap().child(r.name.clone()))
                    .child(div().text_color(hex(MUTED)).whitespace_nowrap().text_ellipsis().child(r.sources.join(", "))),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_none()
                    .child(div().text_color(hex(MUTED)).child(counts(r)))
                    .child(div().min_w(px(80.)).flex().justify_end().font_weight(FontWeight::SEMIBOLD).child(rate(r.per_minute))),
            )
    }))
}

/// `{count}×[ · {hits} hit(s)][ · {dmg} dmg]`. Hits only when they differ
/// from the count: more for an area proc, fewer when it missed or does not
/// damage at all.
fn counts(r: &ProcRow) -> String {
    let mut s = format!("{}×", num(Some(r.count as f64)));
    if r.hits > 0 && r.hits != r.count {
        s.push_str(&format!(" · {} hit{}", num(Some(r.hits as f64)), if r.hits == 1 { "" } else { "s" }));
    }
    if r.damage > 0 {
        s.push_str(&format!(" · {} dmg", num(Some(r.damage as f64))));
    }
    s
}

/// Per minute to two places, or per hour for anything rarer than one every
/// hundred minutes, which "0.00 /min" would round away.
fn rate(per_min: f64) -> String {
    if per_min >= 0.01 { format!("{per_min:.2} /min") } else { format!("{:.2} /hr", per_min * 60.) }
}

fn subtotal(list: &[&ProcRow]) -> String {
    if list.is_empty() {
        return String::new();
    }
    let n: u64 = list.iter().map(|r| r.count).sum();
    let m: f64 = list.iter().map(|r| r.per_minute).sum();
    format!(
        "{} in total, {m:.2} a minute across {} source{}",
        num(Some(n as f64)),
        list.len(),
        if list.len() == 1 { "" } else { "s" }
    )
}

fn panel() -> Div {
    v_flex().gap_2().p_3().rounded(px(8.)).border_1().border_color(hex(BORDER)).bg(hex(PANEL))
}

fn card(title: &str) -> Div {
    panel().child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child(title.to_uppercase()))
}

fn stat(label: &'static str, value: String, note: Option<String>) -> Div {
    let mut c = v_flex()
        .gap_1()
        .p_3()
        .min_w(px(220.))
        .flex_1()
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

fn empty(text: impl Into<SharedString>) -> Div {
    div().py_2().text_color(hex(MUTED)).child(text.into())
}

fn hint(text: impl Into<SharedString>) -> Div {
    div().text_sm().text_color(hex(MUTED)).child(text.into())
}

#[cfg(test)]
mod tests {
    use super::{ProcRow, counts, rate, subtotal};
    use norrlog_api::ProcKind;

    fn row(count: u64, hits: u64, damage: u64, per_minute: f64) -> ProcRow {
        ProcRow { name: "x".into(), kind: ProcKind::Melee, sources: vec![], count, per_minute, damage, hits, first: 0, last: 0 }
    }

    #[test]
    fn a_rare_proc_reads_per_hour_rather_than_rounding_to_zero() {
        assert_eq!(rate(0.5), "0.50 /min");
        assert_eq!(rate(0.01), "0.01 /min");
        assert_eq!(rate(0.005), "0.30 /hr");
    }

    #[test]
    fn hits_show_only_when_they_differ_from_the_count() {
        assert_eq!(counts(&row(21, 2825, 40000, 1.)), "21× · 2,825 hits · 40.0k dmg");
        assert_eq!(counts(&row(5, 5, 0, 1.)), "5×");
        assert_eq!(counts(&row(5, 1, 0, 1.)), "5× · 1 hit");
    }

    #[test]
    fn the_subtotal_names_its_sources() {
        let a = row(3, 0, 0, 0.25);
        let b = row(4, 0, 0, 0.5);
        assert_eq!(subtotal(&[&a, &b]), "7 in total, 0.75 a minute across 2 sources");
        assert_eq!(subtotal(&[&a]), "3 in total, 0.25 a minute across 1 source");
        assert_eq!(subtotal(&[]), "");
    }
}
