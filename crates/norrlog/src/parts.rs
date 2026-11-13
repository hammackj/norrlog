//! The small pieces the tabs are built from: `app.css`' `.card`, `.stat`,
//! `.hint`, `details.note` and `.rows .row`.

use crate::theme::px;
use crate::theme::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{h_flex, v_flex};

/// A titled card: `<div class="card"><h3>`.
pub fn card(title: &str) -> Div {
    v_flex()
        .gap_2()
        .p_3()
        .rounded(px(8.))
        .border_1()
        .border_color(hex(BORDER))
        .bg(hex(PANEL))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(hex(MUTED))
                .child(title.to_uppercase()),
        )
}

/// A card whose title carries a suffix in plain case, as the tradeskill
/// cards' ` — Alchemy` while filtered.
pub fn card_suffixed(title: &str, suffix: Option<String>) -> Div {
    v_flex()
        .gap_2()
        .p_3()
        .rounded(px(8.))
        .border_1()
        .border_color(hex(BORDER))
        .bg(hex(PANEL))
        .child(
            h_flex()
                .gap_1()
                .items_baseline()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(hex(MUTED))
                        .child(title.to_uppercase()),
                )
                .when_some(suffix, |d, s| {
                    d.child(div().text_sm().text_color(hex(MUTED)).child(s))
                }),
        )
}

/// Wrapping text with some runs in bold: the renderer's `<b>` inside a
/// sentence. Each part is (text, bold).
pub fn rich(parts: &[(&str, bool)]) -> StyledText {
    let mut text = String::new();
    let mut bold = Vec::new();
    for (t, b) in parts {
        let start = text.len();
        text.push_str(t);
        if *b {
            bold.push((
                start..text.len(),
                HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
            ));
        }
    }
    StyledText::new(text).with_highlights(bold)
}

/// Two cards side by side, sharing the width: app.css' `.split`.
pub fn split(a: Div, b: Div) -> Div {
    h_flex()
        .items_start()
        .gap_3()
        .child(a.flex_1().min_w_0())
        .child(b.flex_1().min_w_0())
}

/// A stat card: label, big value, optional small line under it.
pub fn stat(label: &str, value: impl Into<SharedString>, note: Option<String>) -> Div {
    v_flex()
        .gap_1()
        .p_3()
        .min_w(px(180.))
        .flex_1()
        .rounded(px(8.))
        .border_1()
        .border_color(hex(BORDER))
        .bg(hex(PANEL))
        .child(
            div()
                .text_xs()
                .text_color(hex(MUTED))
                .child(label.to_uppercase()),
        )
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .child(value.into()),
        )
        .when_some(note, |c, n| {
            c.child(div().text_xs().text_color(hex(MUTED)).child(n))
        })
}

pub fn hint(text: impl Into<SharedString>) -> Div {
    div().text_sm().text_color(hex(MUTED)).child(text.into())
}

/// A list's empty text: `<div class="empty">`.
pub fn empty(text: impl Into<SharedString>) -> Div {
    div().py_2().text_color(hex(MUTED)).child(text.into())
}

/// A collapsed `<details class="note"><summary>Notes</summary>`.
pub fn note(
    id: impl Into<ElementId>,
    open: bool,
    text: impl Into<SharedString>,
    toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let text = text.into();
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .id(id)
                .gap_1()
                .cursor_pointer()
                .text_sm()
                .text_color(hex(MUTED))
                .child(if open { "▾" } else { "▸" })
                .child("Notes")
                .on_click(toggle),
        )
        .when(open, |d| d.child(hint(text)))
}

/// A `.rows .row`: a name on the left, figures on the right, and `fill`
/// behind it `share` of the way across. The fills app.css gives each kind of
/// row are [`ROW`], [`ROW_YOU`], [`ROW_ENEMY`] and [`ROW_PET`], through [`fill`].
pub fn bar_row(share: f64, fill: Hsla, name: impl IntoElement, nums: impl IntoElement) -> Div {
    h_flex()
        .relative()
        .justify_between()
        .gap_3()
        .px(px(9.))
        .py(px(5.))
        .rounded(px(5.))
        .bg(hex(PANEL_2))
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(share.clamp(0., 1.) as f32))
                .bg(fill),
        )
        // A flex slot, so a name made of parts (text, then tags) can let its
        // text give way while the tags keep their size.
        .child(
            h_flex()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_weight(FontWeight::MEDIUM)
                .child(name),
        )
        .child(h_flex().gap_3().flex_none().child(nums))
}

pub const ROW: (u32, f32) = (TEXT, 0.1);
pub const ROW_YOU: (u32, f32) = (YOU, 0.3);
pub const ROW_ENEMY: (u32, f32) = (ENEMY, 0.22);
pub const ROW_PET: (u32, f32) = (PET, 0.28);

/// A row fill from one of the pairs above.
pub fn fill((c, a): (u32, f32)) -> Hsla {
    wash(c, a)
}

/// The dim text in a row's figures.
pub fn dim(text: impl Into<SharedString>) -> Div {
    div().text_color(hex(MUTED)).child(text.into())
}

/// A row's headline figure.
pub fn figure(text: impl Into<SharedString>) -> Div {
    div()
        .min_w(px(60.))
        .flex()
        .justify_end()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

/// `.band`: a heading, a quiet note beside it, and a hairline to the edge.
pub fn band(title: &str, note: &str) -> Div {
    h_flex()
        .items_center()
        .gap(px(10.))
        .mt_2()
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_sm()
                .text_color(hex(MUTED))
                .child(note.to_string()),
        )
        .child(div().flex_1().h(px(1.)).bg(hex(BORDER)))
}

/// Meter bars that ease to a new width over 220 ms, as the Electron
/// dashboard's `transition: width` does, rather than jumping.
///
/// Each row remembers where its bar was heading; a new target starts an
/// animation (keyed by the target, so GPUI restarts it) from the old one.
#[derive(Default)]
pub struct BarAnim {
    rows: std::collections::HashMap<String, (f32, f32)>,
}

impl BarAnim {
    pub fn fill(&mut self, key: String, to: f32, color: Hsla) -> AnyElement {
        // Rows come and go with every fight; a long session must not keep
        // every one it has seen.
        if self.rows.len() > 400 {
            self.rows.clear();
        }
        let e = self.rows.entry(key.clone()).or_insert((to, to));
        if e.1 != to {
            *e = (e.1, to);
        }
        let (from, to) = *e;
        let d = div().absolute().top_0().bottom_0().left_0().bg(color);
        if from == to {
            return d.w(relative(to)).into_any_element();
        }
        d.with_animation(
            SharedString::from(format!("bar-{key}-{}", to.to_bits())),
            Animation::new(std::time::Duration::from_millis(220)).with_easing(ease_in_out),
            move |d, t| d.w(relative(from + (to - from) * t)),
        )
        .into_any_element()
    }
}

/// Hand a web address to the system browser: only `http:` and `https:`. The addresses come from the daemon and
/// the item site, and anything else — `file:`, a custom scheme — would have
/// the desktop run whatever handles it.
pub fn open_web(cx: &App, url: &str) -> bool {
    if is_web(url) {
        cx.open_url(url);
        true
    } else {
        eprintln!("[norrlog] refused to open {url:?}: not an http(s) address");
        false
    }
}

fn is_web(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

#[cfg(test)]
mod web_tests {
    #[test]
    fn only_http_and_https_open() {
        assert!(super::is_web("https://eqldb.example/item/x"));
        assert!(super::is_web("HTTP://127.0.0.1:8777/api/encounters/export"));
        assert!(!super::is_web("file:///etc/passwd"));
        assert!(!super::is_web("javascript:alert(1)"));
        assert!(!super::is_web("steam://run/1"));
        assert!(!super::is_web(""));
    }
}

thread_local! {
    /// Where the pointer was at the last scroll, until it next moves.
    static SCROLLED_AT: std::cell::Cell<Option<Point<Pixels>>> = const { std::cell::Cell::new(None) };
}

/// Item and spell cards hide from a scroll until the pointer moves again.
///
/// GPUI clears a visible tooltip on scroll, but not one still waiting out
/// its show delay. That one opens after the scroll, judged against where
/// its row was *before* the scroll, and stays open over whatever row is
/// there now, since the pointer is still inside the stale bounds.
///
/// Mount once in the window: it registers window-level handlers each frame.
pub fn scroll_watch() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        |_, _, window, _| {
            window.on_mouse_event(|e: &ScrollEvent, phase, window, _| {
                if phase.capture() {
                    SCROLLED_AT.with(|s| s.set(Some(e.position)));
                    window.refresh();
                }
            });
            window.on_mouse_event(|e: &MouseMoveEvent, phase, window, _| {
                // GPUI may send a move at the same spot after a scroll;
                // only a real move counts.
                if phase.capture()
                    && SCROLLED_AT
                        .with(|s| s.get())
                        .is_some_and(|p| p != e.position)
                {
                    SCROLLED_AT.with(|s| s.set(None));
                    window.refresh();
                }
            });
        },
    )
    .absolute()
    .size_0()
}

/// True from a scroll until the pointer moves: draw no card.
pub fn cards_hidden() -> bool {
    SCROLLED_AT.with(|s| s.get()).is_some()
}
