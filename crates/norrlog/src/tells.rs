//! Tells: `loadTells` and `renderThread` (tells.js), threaded by
//! correspondent. The checklist is the Tells section of
//! `docs/parity/tells-alerts-settings.md`.
//!
//! `/api/tells` is fetched when the tab opens and every ten seconds while it
//! is on screen. A failure is silent.

use crate::api::Api;
use crate::format::when_short;
use crate::parts::{ROW_YOU, bar_row, card_suffixed, dim, empty, figure, fill, note, split};
use crate::table::tag;
use crate::theme::*;
use eqlparser_api::{Conversation, TellReport};
use gpui::prelude::FluentBuilder;
use gpui::*;
use crate::theme::px;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{h_flex, v_flex};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(10);

pub struct TellsView {
    api: Option<Api>,
    report: Option<TellReport>,
    /// The person whose thread is open. In memory only.
    open: Option<String>,
    note_open: bool,
    last_load: Option<Instant>,
    loading: Option<Task<()>>,
}

impl TellsView {
    pub fn new() -> Self {
        TellsView { api: None, report: None, open: None, note_open: false, last_load: None, loading: None }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.api = api;
        self.last_load = None;
        cx.notify();
    }

    pub fn on_snapshot(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.load(cx);
        }
    }

    pub fn load(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_load = Some(Instant::now());
        self.loading = Some(cx.spawn(async move |this, cx| {
            let Ok(r) = api.get::<TellReport>("/api/tells".to_string()).await else { return };
            let _ = this.update(cx, |v, cx| {
                // The most recent thread rather than an empty pane.
                if v.open.is_none() {
                    v.open = r.conversations.first().map(|c| c.who.clone());
                }
                v.report = Some(r);
                cx.notify();
            });
        }));
    }
}

const NOTE: &str = "Person-to-person tells only. Channel chat has no counterpart, and folding it in would bury a \
    handful of real conversations under thousands of channel lines. These stay in memory for as long as the daemon \
    runs — they are never written to the history database and never included in an export.";

impl Render for TellsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let convs: &[Conversation] = self.report.as_ref().map_or(&[], |r| &r.conversations);
        let count = self.report.as_ref().filter(|r| !r.conversations.is_empty()).map(|r| {
            format!("— {} people · {} sent, {} received", r.conversations.len(), r.total_sent, r.total_received)
        });
        let people = if self.report.is_none() {
            div()
        } else if convs.is_empty() {
            empty("No tells in this log")
        } else {
            v_flex().gap_1().children(convs.iter().enumerate().map(|(i, c)| {
                let owed = c.awaiting_reply();
                let on = self.open.as_deref() == Some(c.who.as_str());
                let who = c.who.clone();
                div()
                    .id(SharedString::from(format!("tell-{i}")))
                    .cursor_pointer()
                    .rounded(px(5.))
                    // The open person: the renderer's `.row.you` colours the bar,
                    // and a bar only shows when it is their turn.
                    .when(on, |d| d.border_1().border_color(hex(YOU)))
                    .child(bar_row(
                        if owed { 1. } else { 0. },
                        fill(if on { ROW_YOU } else { (TEXT, 0.1) }),
                        h_flex().gap_1().child(c.who.clone()).when(owed, |d| d.child(tag("their turn", ACCENT).flex_none())),
                        h_flex().gap_3().child(dim(when_short(Some(c.last)))).child(figure(c.total().to_string())),
                    ))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.open = Some(who.clone());
                        cx.notify();
                    }))
            }))
        };
        let thread = convs.iter().find(|c| Some(c.who.as_str()) == self.open.as_deref());
        let who = thread.map(|c| format!(" — {}, {} sent / {} received", c.who, c.sent, c.received));
        let messages = match thread {
            None => empty("Pick someone on the left").into_any_element(),
            Some(c) => div()
                .id("tell-thread")
                .max_h(px(560.))
                .overflow_y_scrollbar()
                .child(v_flex().gap(px(5.)).children(c.messages.iter().map(|m| {
                    h_flex()
                        .items_start()
                        .gap_2()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(4.))
                        .bg(hex(PANEL_2))
                        .border_l_2()
                        .border_color(hex(if m.outgoing { YOU } else { BORDER }))
                        .when(m.outgoing, |d| d.ml(px(24.)))
                        .when(!m.outgoing, |d| d.mr(px(24.)))
                        .child(
                            div()
                                .flex_none()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(hex(if m.outgoing { YOU } else { ACCENT }))
                                .child(if m.outgoing { "You".to_string() } else { c.who.clone() }),
                        )
                        .child(div().flex_1().min_w_0().child(m.text.clone()))
                        .child(dim(when_short(Some(m.at))).text_sm().flex_none())
                })))
                .into_any_element(),
        };
        div().id("tells-scroll").size_full().overflow_y_scrollbar().child(split(
            card_suffixed("People", count).child(people),
            card_suffixed("Conversation", who).child(messages).child(note(
                "tells-note",
                self.note_open,
                NOTE,
                cx.listener(|v, _, _, cx| {
                    v.note_open = !v.note_open;
                    cx.notify();
                }),
            )),
        ))
    }
}
