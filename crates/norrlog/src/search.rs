//! Search: lines in the log that match, newest first.
//!
//! Every other tab shows what the parser made of the log. This one shows the
//! log itself, for the line nothing else counts: who said what in a channel,
//! when a named spawned, the exact wording a new alert should catch. The
//! daemon scans the file on disk (`/api/search`), which for a 390 MB log is
//! a quarter of a second, so there is no index to fall behind the game.

use crate::api::{Api, query};
use crate::format::grouped;
use crate::parts::{card_suffixed, empty, hint, note};
use crate::theme::px;
use crate::theme::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{Disableable, Sizable, h_flex, v_flex};
use norrlog_api::{SearchHit, SearchReply};

/// Lines per page; "Show older" fetches the next page back.
const PAGE: usize = 200;

pub struct SearchView {
    api: Option<Api>,
    input: Entity<InputState>,
    regex: bool,
    case: bool,
    /// What the hits on screen were searched with, so "Show older" asks the
    /// same question even after the box is edited.
    asked: Option<(String, bool, bool)>,
    hits: Vec<SearchHit>,
    reply: Option<SearchReply>,
    error: Option<String>,
    busy: bool,
    note_open: bool,
    _subs: Vec<Subscription>,
    task: Option<Task<()>>,
}

impl SearchView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search the log: Lord Nagafen, tells you, a word…")
        });
        let subs =
            vec![
                cx.subscribe_in(&input, window, |v: &mut Self, _, ev: &InputEvent, _, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. }) {
                        v.run(cx);
                    }
                }),
            ];
        SearchView {
            api: None,
            input,
            regex: false,
            case: false,
            asked: None,
            hits: Vec::new(),
            reply: None,
            error: None,
            busy: false,
            note_open: false,
            _subs: subs,
            task: None,
        }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.api = api;
        cx.notify();
    }

    /// A new search from what is in the box.
    fn run(&mut self, cx: &mut Context<Self>) {
        let q = self.input.read(cx).value().to_string();
        if q.trim().is_empty() {
            return;
        }
        self.asked = Some((q, self.regex, self.case));
        self.fetch(None, cx);
    }

    /// The page before the oldest hit on screen.
    fn older(&mut self, cx: &mut Context<Self>) {
        let before = self.hits.last().map(|h| h.line);
        if before.is_some() {
            self.fetch(before, cx);
        }
    }

    fn fetch(&mut self, before: Option<u64>, cx: &mut Context<Self>) {
        let (Some(api), Some((q, regex, case))) = (self.api.clone(), self.asked.clone()) else {
            return;
        };
        let mut pairs = vec![("q", q), ("limit", PAGE.to_string())];
        if regex {
            pairs.push(("regex", "1".into()));
        }
        if case {
            pairs.push(("case", "1".into()));
        }
        if let Some(b) = before {
            pairs.push(("before", b.to_string()));
        }
        let path = format!("/api/search?{}", query(&pairs));
        self.busy = true;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let got = api.get::<SearchReply>(path).await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match got {
                    Ok(r) => {
                        v.error = r.error.clone();
                        if before.is_none() {
                            v.hits.clear();
                        }
                        v.hits.extend(r.hits.iter().cloned());
                        // The first page's total is the whole count; a page
                        // further back counts only what lies before it.
                        if before.is_none() || v.reply.is_none() {
                            v.reply = Some(r);
                        }
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        }));
    }
}

const NOTE: &str = "The followed log, read from disk as it is now, so a line written a second ago is already \
    searchable. Plain text matches anywhere in the line, the timestamp included, without regard to case. \
    With \"Regular expression\" on, ^ and $ are the start and end of a line. Nothing here is saved.";

/// `Fri Nov 13 13:36:13 2026` as `Nov 13 13:36:13`, with the year only when
/// it is not `this_year`. Anything else is shown as written.
fn short_stamp(stamp: &str, this_year: i32) -> String {
    let parts: Vec<&str> = stamp.split_whitespace().collect();
    match parts.as_slice() {
        [_, month, day, clock, year] => match year.parse::<i32>() {
            Ok(y) if y == this_year => format!("{month} {day} {clock}"),
            Ok(_) => format!("{month} {day} {year} {clock}"),
            Err(_) => stamp.to_string(),
        },
        _ => stamp.to_string(),
    }
}

/// The line's text with the matched ranges picked out.
fn marked(h: &SearchHit) -> StyledText {
    let style = HighlightStyle {
        color: Some(hex(GOLD)),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    let ranges = h
        .marks
        .iter()
        .filter(|[a, b]| a < b && *b <= h.text.len())
        .filter(|[a, b]| h.text.is_char_boundary(*a) && h.text.is_char_boundary(*b))
        .map(|[a, b]| (*a..*b, style))
        .collect::<Vec<_>>();
    StyledText::new(h.text.clone()).with_highlights(ranges)
}

impl Render for SearchView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = h_flex()
            .gap_3()
            .flex_wrap()
            .child(
                div()
                    .flex_1()
                    .min_w(px(260.))
                    .child(Input::new(&self.input).small()),
            )
            .child(
                Button::new("se-go")
                    .label(if self.busy { "Searching…" } else { "Search" })
                    .small()
                    .primary()
                    .disabled(self.busy || self.api.is_none())
                    .on_click(cx.listener(|v, _, _, cx| v.run(cx))),
            )
            .child(
                Checkbox::new("se-regex")
                    .label("Regular expression")
                    .checked(self.regex)
                    .on_click(cx.listener(|v, on: &bool, _, cx| {
                        v.regex = *on;
                        cx.notify();
                    })),
            )
            .child(
                Checkbox::new("se-case")
                    .label("Match case")
                    .checked(self.case)
                    .on_click(cx.listener(|v, on: &bool, _, cx| {
                        v.case = *on;
                        cx.notify();
                    })),
            );

        let summary = self
            .reply
            .as_ref()
            .filter(|_| self.error.is_none())
            .map(|r| {
                let shown = self.hits.len() as u64;
                if r.total == 0 {
                    format!("— nothing in {}", r.log)
                } else if shown >= r.total {
                    format!(
                        "— {} line{} in {}",
                        grouped(r.total as i64),
                        if r.total == 1 { "" } else { "s" },
                        r.log
                    )
                } else {
                    format!(
                        "— the newest {} of {} lines in {}",
                        grouped(shown as i64),
                        grouped(r.total as i64),
                        r.log
                    )
                }
            });
        let more = self
            .reply
            .as_ref()
            .is_some_and(|r| (self.hits.len() as u64) < r.total);

        // The log's own "now": the year of its newest match.
        let this_year = self
            .hits
            .first()
            .and_then(|h| h.stamp.rsplit(' ').next()?.parse().ok())
            .unwrap_or(0);
        let body = if let Some(e) = &self.error {
            div()
                .text_color(hex(ENEMY))
                .child(e.clone())
                .into_any_element()
        } else if self.reply.is_none() {
            empty("Type something and press Enter").into_any_element()
        } else if self.hits.is_empty() {
            empty("No line matches").into_any_element()
        } else {
            v_flex()
                .gap(px(2.))
                .children(self.hits.iter().map(|h| {
                    h_flex()
                        .items_start()
                        .gap_3()
                        .px(px(8.))
                        .py(px(3.))
                        .rounded(px(4.))
                        .bg(hex(PANEL_2))
                        .child(
                            div()
                                .flex_none()
                                .w(px(150.))
                                .whitespace_nowrap()
                                .text_color(hex(MUTED))
                                .child(short_stamp(&h.stamp, this_year)),
                        )
                        .child(div().flex_1().min_w_0().child(marked(h)))
                }))
                .when(more, |d| {
                    d.child(
                        h_flex().pt_2().child(
                            Button::new("se-older")
                                .label("Show older")
                                .small()
                                .ghost()
                                .disabled(self.busy)
                                .on_click(cx.listener(|v, _, _, cx| v.older(cx))),
                        ),
                    )
                })
                .into_any_element()
        };

        div()
            .id("search-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(
                v_flex().gap_3().child(
                    card_suffixed("Search the log", summary)
                        .child(bar)
                        .child(body)
                        .child(note(
                            "search-note",
                            self.note_open,
                            NOTE,
                            cx.listener(|v, _, _, cx| {
                                v.note_open = !v.note_open;
                                cx.notify();
                            }),
                        ))
                        .when(self.api.is_none(), |d| {
                            d.child(hint("Waiting for the daemon"))
                        }),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::short_stamp;

    #[test]
    fn a_stamp_loses_the_weekday_and_this_years_year() {
        assert_eq!(
            short_stamp("Fri Nov 13 13:36:13 2026", 2026),
            "Nov 13 13:36:13"
        );
        assert_eq!(
            short_stamp("Fri Nov 13 13:36:13 2025", 2026),
            "Nov 13 2025 13:36:13"
        );
        assert_eq!(short_stamp("", 2026), "");
        assert_eq!(short_stamp("not a stamp", 2026), "not a stamp");
    }
}
