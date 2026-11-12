//! The small pieces the tabs are built from: `app.css`' `.card`, `.stat`,
//! `.hint`, `details.note` and `.rows .row`.

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
        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child(title.to_uppercase()))
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
                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child(title.to_uppercase()))
                .when_some(suffix, |d, s| d.child(div().text_sm().text_color(hex(MUTED)).child(s))),
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
            bold.push((start..text.len(), HighlightStyle { font_weight: Some(FontWeight::BOLD), ..Default::default() }));
        }
    }
    StyledText::new(text).with_highlights(bold)
}

/// Two cards side by side, sharing the width: app.css' `.split`.
pub fn split(a: Div, b: Div) -> Div {
    h_flex().items_start().gap_3().child(a.flex_1().min_w_0()).child(b.flex_1().min_w_0())
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
        .child(div().text_xs().text_color(hex(MUTED)).child(label.to_uppercase()))
        .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(value.into()))
        .when_some(note, |c, n| c.child(div().text_xs().text_color(hex(MUTED)).child(n)))
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
        .child(div().absolute().top_0().bottom_0().left_0().w(relative(share.clamp(0., 1.) as f32)).bg(fill))
        // A flex slot, so a name made of parts (text, then tags) can let its
        // text give way while the tags keep their size.
        .child(
            h_flex().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_weight(FontWeight::MEDIUM).child(name),
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
    div().min_w(px(60.)).flex().justify_end().font_weight(FontWeight::SEMIBOLD).child(text.into())
}
