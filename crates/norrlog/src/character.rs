//! Character: the lifetime totals from the snapshot (app.js'
//! `renderSnapshot`), and tasks and achievements from `/api/tasks`
//! (`tasks.js`). The checklist is the Character section of
//! the parity inventory.
//!
//! Tasks are fetched on every snapshot whichever tab is open, at most every
//! fifteen seconds, as the renderer does: they change rarely, and the
//! renderer polls them on the lockout grid's relaxed cadence.

use crate::api::Api;
use crate::format::{num, pp, when_short};
use crate::parts::{ROW, ROW_YOU, band, bar_row, card, dim, empty, figure, fill, hint, note};
use crate::table::tag;
use crate::theme::*;
use norrlog_api::{TaskRecord, TaskReport, Totals};
use gpui::prelude::FluentBuilder;
use gpui::*;
use crate::theme::px;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{h_flex, v_flex};
use std::collections::HashSet;
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(15);

pub struct CharacterView {
    api: Option<Api>,
    totals: Option<Totals>,
    report: Option<TaskReport>,
    last_load: Option<Instant>,
    loading: Option<Task<()>>,
    /// "show N completed". Not remembered across runs.
    show_done: bool,
    note_open: bool,
    /// Tasks whose quest text is open, by name.
    dialogue_open: HashSet<String>,
}

impl CharacterView {
    pub fn new() -> Self {
        CharacterView {
            api: None,
            totals: None,
            report: None,
            last_load: None,
            loading: None,
            show_done: false,
            note_open: false,
            dialogue_open: HashSet::new(),
        }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.api = api;
        self.last_load = None;
        cx.notify();
    }

    /// A snapshot arrived: take its totals, and fetch tasks if fifteen
    /// seconds have passed. Whichever tab is open, as the renderer does.
    pub fn on_snapshot(&mut self, totals: Option<Totals>, cx: &mut Context<Self>) {
        if totals.as_ref().map(totals_key) != self.totals.as_ref().map(totals_key) {
            self.totals = totals;
            cx.notify();
        }
        if self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.load(cx);
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_load = Some(Instant::now());
        self.loading = Some(cx.spawn(async move |this, cx| {
            // A failure is silent: what was shown stays.
            let Ok(r) = api.get::<TaskReport>("/api/tasks".to_string()).await else { return };
            let _ = this.update(cx, |v, cx| {
                v.report = Some(r);
                cx.notify();
            });
        }));
    }

    fn render_totals(&self) -> Div {
        let t = self.totals.as_ref();
        let n = |f: fn(&Totals) -> f64| num(t.map(f));
        let big = |label: &str, value: String| totals_card(label, div().text_2xl().font_weight(FontWeight::BOLD).child(value));
        let small = |label: &str, value: String| totals_card(label, div().text_xl().font_weight(FontWeight::BOLD).child(value));
        let coin = match t {
            Some(t) => h_flex()
                .items_baseline()
                .gap(px(5.))
                .text_xl()
                .font_weight(FontWeight::BOLD)
                .child(pp(t.coin_total_copper))
                .child(div().text_sm().font_weight(FontWeight::NORMAL).text_color(hex(MUTED)).child(format!(
                    "{} looted · {} sold",
                    pp(t.coin_copper),
                    pp(t.coin_from_sales_copper)
                ))),
            None => h_flex().text_xl().font_weight(FontWeight::BOLD).child("—"),
        };
        // app.css' `.grid`: as many 13 rem columns as fit, sharing the rest.
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_3()
            .child(big("Your damage", n(|t| t.your_damage as f64)))
            .child(big("Kills", n(|t| t.kills as f64)))
            .child(big("Deaths", n(|t| t.deaths as f64)))
            .child(big("Encounters", n(|t| t.encounters as f64)))
            .child(small("Healing done", n(|t| t.your_healing as f64)))
            .child(small("Damage taken", n(|t| t.your_damage_taken as f64)))
            .child(totals_card("Platinum earned", coin))
    }

    fn render_tasks(&self, cx: &mut Context<Self>) -> Div {
        let tasks: &[TaskRecord] = self.report.as_ref().map_or(&[], |r| &r.tasks);
        let todo = tasks.iter().filter(|t| t.still_to_do).count();
        let finished = tasks.len() - todo;
        let count = task_count(tasks.len(), todo);
        let title = h_flex()
            .gap_2()
            .items_baseline()
            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child("TASKS"))
            .child(div().text_sm().text_color(hex(MUTED)).child(count));
        let mut body = v_flex().gap_1();
        if tasks.is_empty() {
            body = body.child(empty("No tasks assigned"));
        } else {
            let shown: Vec<&TaskRecord> = tasks.iter().filter(|t| self.show_done || t.still_to_do).collect();
            if shown.is_empty() {
                body = body.child(empty("Nothing outstanding"));
            }
            for (i, t) in shown.into_iter().enumerate() {
                body = body.child(self.task_block(t, i, cx));
            }
            if finished > 0 {
                body = body.child(
                    div()
                        .id("task-toggle")
                        .mt_1()
                        .cursor_pointer()
                        .text_sm()
                        .text_color(hex(LINK))
                        .hover(|s| s.text_color(hex(LINK_HOVER)))
                        .child(format!("{} {finished} completed", if self.show_done { "hide" } else { "show" }))
                        .on_click(cx.listener(|v, _, _, cx| {
                            v.show_done = !v.show_done;
                            cx.notify();
                        })),
                );
            }
        }
        v_flex()
            .gap_2()
            .p_3()
            .flex_1()
            .min_w_0()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(BORDER))
            .bg(hex(PANEL))
            .child(title)
            .child(body)
            .child(note(
                "task-note",
                self.note_open,
                "The client logs only assignment and \"has been updated\" — no objective text and no total — so \
                 progress is a count of completed steps, never a fraction. Verified: the weekly boss tasks tick at \
                 the exact second of the matching kill.",
                cx.listener(|v, _, _, cx| {
                    v.note_open = !v.note_open;
                    cx.notify();
                }),
            ))
    }

    /// `taskBlock`: a header row, the defined steps, a note on completions
    /// no step claimed, and the quest text folded away.
    fn task_block(&self, t: &TaskRecord, i: usize, cx: &mut Context<Self>) -> Div {
        let states = t.step_states();
        let done_steps = states.iter().filter(|d| **d).count();
        let done = t.completions.len();
        let share = if !t.steps.is_empty() {
            done_steps as f64 / t.steps.len() as f64
        } else if done > 0 {
            1.
        } else {
            0.
        };
        let mut name = h_flex()
            .min_w_0()
            .gap(px(5.))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().child(t.name.clone()));
        if t.weekly {
            name = name.child(tag("weekly", ACCENT).flex_none());
        }
        if !t.still_to_do {
            name = name.child(tag(if t.weekly { "done this week" } else { "done" }, HEAL).flex_none());
        }
        let last = if done > 0 { when_short(t.last_progress()) } else { "no progress yet".into() };
        let progress = if t.steps.is_empty() {
            format!("{done} step{}", if done == 1 { "" } else { "s" })
        } else {
            format!("{done_steps}/{}", t.steps.len())
        };
        let mut out = v_flex()
            .when(i > 0, |d| d.mt_3())
            .child(bar_row(share, fill(if t.weekly { ROW_YOU } else { ROW }), name, h_flex().gap_3().child(dim(last)).child(figure(progress))));
        if !t.steps.is_empty() {
            out = out.child(v_flex().mt_1().pl(px(6.)).children(t.steps.iter().enumerate().map(|(n, st)| {
                let ok = states.get(n).copied().unwrap_or(false);
                let at = t.completions.iter().find(|c| c.step_index == Some(n)).map(|c| when_short(Some(c.at)));
                h_flex()
                    .justify_between()
                    .gap_3()
                    .px(px(6.))
                    .py(px(2.))
                    .border_l_2()
                    .border_color(hex(if ok { HEAL } else { BORDER }))
                    .text_color(hex(if ok { TEXT } else { MUTED }))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(div().text_color(hex(if ok { HEAL } else { MUTED })).child(if ok { "●" } else { "○" }))
                            .child(st.description.clone()),
                    )
                    .when_some(at, |d, at| d.child(dim(at)))
            })));
        }
        let stray = t.unidentified();
        if stray > 0 && !t.steps.is_empty() {
            out = out.child(hint(format!(
                "{stray} completion{} could not be matched to a listed objective.",
                if stray == 1 { "" } else { "s" }
            )));
        }
        if !t.dialogue.is_empty() {
            let open = self.dialogue_open.contains(&t.name);
            let key = t.name.clone();
            let n = t.dialogue.len();
            out = out.child(
                v_flex()
                    .mt_1()
                    .child(
                        h_flex()
                            .id(SharedString::from(format!("quest-{}", t.name)))
                            .gap_1()
                            .px(px(6.))
                            .cursor_pointer()
                            .text_color(hex(MUTED))
                            .hover(|s| s.text_color(hex(TEXT)))
                            .child(if open { "▾" } else { "▸" })
                            .child(format!("{n} line{} of quest text", if n == 1 { "" } else { "s" }))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                if !v.dialogue_open.remove(&key) {
                                    v.dialogue_open.insert(key.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .when(open, |d| {
                        d.children(t.dialogue.iter().map(|l| {
                            // One run of text, so it wraps inside the card; the
                            // speaker is picked out as the renderer's <b> is.
                            let head = format!("{}:", l.speaker);
                            let text = format!("{head} {}", l.text);
                            let speaker = HighlightStyle {
                                color: Some(hex(ACCENT)),
                                font_weight: Some(FontWeight::SEMIBOLD),
                                ..Default::default()
                            };
                            v_flex()
                                .w_full()
                                .gap_1()
                                .my(px(2.))
                                .px(px(9.))
                                .py(px(3.))
                                .border_l_2()
                                .border_color(hex(BORDER))
                                .text_color(hex(MUTED))
                                .child(StyledText::new(text).with_highlights([(0..head.len(), speaker)]))
                                .when(!l.keywords.is_empty(), |d| {
                                    d.child(h_flex().flex_wrap().gap_1().children(l.keywords.iter().map(|k| {
                                        div().px(px(4.)).rounded(px(3.)).bg(wash(YOU, 0.18)).text_color(hex(YOU)).text_xs().child(k.clone())
                                    })))
                                })
                        }))
                    }),
            );
        }
        out
    }

    fn render_achievements(&self) -> Div {
        let list = self.report.as_ref().map_or(&[][..], |r| &r.achievements[..]);
        let rows = if list.is_empty() {
            empty("None earned in this log")
        } else {
            v_flex().gap_1().children(
                list.iter().take(10).map(|a| bar_row(0., fill(ROW), a.name.clone(), dim(when_short(Some(a.at))))),
            )
        };
        card("Achievements").flex_1().min_w_0().child(rows)
    }
}

impl Render for CharacterView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = v_flex()
            .gap_3()
            .child(band("Totals", "everything this log has recorded"))
            .child(self.render_totals())
            .child(band("Character", "what this character is carrying and has earned"))
            .child(h_flex().items_start().gap_3().child(self.render_tasks(cx)).child(self.render_achievements()));
        div().id("character-scroll").size_full().overflow_y_scrollbar().child(page)
    }
}

/// A totals card: the title over its figure.
fn totals_card(label: &str, value: impl IntoElement) -> Div {
    v_flex()
        .min_w(px(208.))
        .flex_1()
        .gap_1()
        .p_3()
        .rounded(px(8.))
        .border_1()
        .border_color(hex(BORDER))
        .bg(hex(PANEL))
        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child(label.to_uppercase()))
        .child(value)
}

/// The totals that are drawn, to tell whether a snapshot changed any.
fn totals_key(t: &Totals) -> [u64; 9] {
    [
        t.your_damage,
        t.kills as u64,
        t.deaths,
        t.encounters as u64,
        t.your_healing,
        t.your_damage_taken,
        t.coin_total_copper,
        t.coin_copper,
        t.coin_from_sales_copper,
    ]
}

/// `N to do of M`, `all M done`, or nothing when there are no tasks.
fn task_count(total: usize, todo: usize) -> String {
    match (total, todo) {
        (0, _) => String::new(),
        (m, 0) => format!("all {m} done"),
        (m, n) => format!("{n} to do of {m}"),
    }
}

#[cfg(test)]
mod tests {
    use super::task_count;

    #[test]
    fn the_task_count_says_what_is_left() {
        assert_eq!(task_count(0, 0), "");
        assert_eq!(task_count(15, 0), "all 15 done");
        assert_eq!(task_count(6, 1), "1 to do of 6");
    }
}
