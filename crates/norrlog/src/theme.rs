//! The palette: the Electron app's tokens (`gui/renderer/app.css`), shared
//! with the eqldb collector so the two read as one family. Every view takes
//! its colours from here, never from a literal.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use gpui::{App, Div, Hsla, IntoElement, ParentElement, Pixels, ScrollHandle, Styled, div, rgb};
use gpui_component::scroll::{Scrollbar, ScrollbarMode};
use gpui_component::{Theme, ThemeMode};

/// A scroller that takes only its own axis from the wheel. Unset, GPUI turns
/// a sideways wheel into vertical scrolling for a container that scrolls only
/// vertically (and the reverse), so the timeline's sideways scroll also ran
/// the page down, and wheeling the page over the timeline panned it.
pub trait OwnAxis: gpui::Styled + Sized {
    fn own_axis(mut self) -> Self {
        self.style().restrict_scroll_to_axis = Some(true);
        self
    }
}
impl<T: gpui::Styled> OwnAxis for T {}

pub fn hex(c: u32) -> Hsla {
    rgb(resolve(c)).into()
}

/// The two palettes Settings offers, as the Electron app names them: the
/// empty string is the house palette.
pub const THEMES: [(&str, &str); 2] = [
    ("", "hammackj — the house palette"),
    ("spellbook", "Spellbook — indigo and gold leaf"),
];

/// Settings' scale steps, as the Electron app offers them.
pub const SCALES: [(f32, &str); 5] = [
    (0.85, "85% — denser"),
    (1., "100% — default"),
    (1.15, "115%"),
    (1.3, "130%"),
    (1.5, "150% — larger"),
];

static SPELLBOOK: AtomicBool = AtomicBool::new(false);
/// The UI scale's bits; 1.0 to start.
static SCALE: AtomicU32 = AtomicU32::new(0x3f80_0000);

/// Choose the palette by its config name. Unknown names fall back to the
/// house palette, as the Electron app's missing `data-theme` block would.
pub fn set_palette(name: &str) {
    SPELLBOOK.store(name == "spellbook", Ordering::Relaxed);
}

pub fn palette() -> &'static str {
    if SPELLBOOK.load(Ordering::Relaxed) {
        "spellbook"
    } else {
        ""
    }
}

/// Set the UI scale, kept to the range Settings offers.
pub fn set_scale(v: f32) {
    let v = if v.is_finite() {
        v.clamp(0.85, 1.5)
    } else {
        1.
    };
    SCALE.store(v.to_bits(), Ordering::Relaxed);
}

pub fn scale() -> f32 {
    f32::from_bits(SCALE.load(Ordering::Relaxed))
}

/// A length in the layout's own pixels, which UI scale multiplies, as the
/// Electron app's rem-based layout follows `--ui-scale`. Every view imports
/// this in place of `gpui::px`; paint code working in measured bounds keeps
/// `gpui::px`, since those pixels are already scaled.
pub fn px(v: f32) -> Pixels {
    gpui::px(v * scale())
}

/// A token in the active palette. The views name the house palette's
/// constants; the spellbook block (`app.css`, `[data-theme="spellbook"]`)
/// swaps each for its own, so no view needs to know which is showing.
fn resolve(c: u32) -> u32 {
    if !SPELLBOOK.load(Ordering::Relaxed) {
        return c;
    }
    match c {
        BG => 0x14121f,
        PANEL => 0x1c1930,
        PANEL_2 => 0x24203c,
        BORDER => 0x322c52,
        TEXT => 0xe8e2d4,
        MUTED => 0x918aa8,
        ACCENT => 0xe0b64a,
        YOU => 0x79b6d9,
        ENEMY => 0xd16a72,
        HEAL => 0x86c08a,
        PET => 0xb48ce0,
        LINK => 0x7f6bc4,
        LINK_HOVER => 0x9a86e0,
        RAISED => 0x282348,
        SUNKEN => 0x0d0b16,
        other => other,
    }
}

// The parser's palette (gui/renderer/app.css), so the two apps read as one
// family: eqldb's surfaces and text, gold for the accent, cyan for you.
pub const BG: u32 = 0x14161a;
/// `--bg-2`: header, tabs, section cards.
pub const PANEL: u32 = 0x1b1e24;
/// `--bg-3`: inputs, buttons, hover.
pub const PANEL_2: u32 = 0x22262e;
pub const BORDER: u32 = 0x2e333d;
pub const TEXT: u32 = 0xd8dae0;
pub const MUTED: u32 = 0x8b93a1;
/// Gold: the active tab, a sent row.
pub const ACCENT: u32 = 0xd9a441;
/// Cyan: you, and where you are.
pub const YOU: u32 = 0x22d3ee;
/// Red: something only you know, or something wrong.
pub const ENEMY: u32 = 0xd1666a;
/// Green: live, and your bags.
pub const HEAL: u32 = 0x7bbf6a;
/// Violet: your pet.
pub const PET: u32 = 0xa389d4;
/// One step of blue off `YOU`, which it matches in the house palette, so the
/// spellbook can tell the two apart: its links are violet, "you" stays blue.
pub const LINK: u32 = 0x22d3ef;
pub const LINK_HOVER: u32 = 0x67e8f9;
pub const RAISED: u32 = 0x2a2f38;
pub const SUNKEN: u32 = 0x0e1013;
pub const OK: u32 = HEAL;
pub const WARN: u32 = ACCENT;
pub const ERR: u32 = ENEMY;
pub const INFO: u32 = YOU;

/// A colour at an alpha, for the washes the parser writes as
/// `rgb(var(--x-rgb) / 0.1)`.
pub fn wash(c: u32, a: f32) -> Hsla {
    let mut h: Hsla = rgb(resolve(c)).into();
    h.a = a;
    h
}

pub fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |t| {
        let c = &mut t.colors;
        c.background = hex(BG);
        c.foreground = hex(TEXT);
        c.border = hex(BORDER);
        c.muted = hex(PANEL_2);
        c.muted_foreground = hex(MUTED);
        c.popover = hex(PANEL);
        c.popover_foreground = hex(TEXT);
        c.accent = hex(PANEL_2);
        c.accent_foreground = hex(TEXT);
        c.ring = hex(YOU);
        c.primary = hex(YOU);
        c.title_bar = hex(PANEL);
        c.title_bar_border = hex(BORDER);
        c.scrollbar = gpui::transparent_black();
        c.scrollbar_thumb = wash(MUTED, 0.45);
        c.scrollbar_thumb_hover = wash(MUTED, 0.7);
        c.success = hex(OK);
        c.warning = hex(WARN);
        c.danger = hex(ERR);
        c.info = hex(INFO);
        // Always drawn, as the Electron app's were: a bar that shows only
        // while scrolling hides that there is more below.
        t.scrollbar_mode = ScrollbarMode::Always;
        // The root's rem, which every text size follows; `px` scales the rest.
        t.font_size = gpui::px(16. * scale());
        t.mono_font_size = gpui::px(13. * scale());
    });
}

/// A vertical scroll bar laid over `scroller`, for a scroll area that keeps
/// its own handle (to jump to the bottom, or to keep its place), which
/// `overflow_y_scrollbar` would replace with one of its own. The wrapper
/// takes the scroller's place in the layout, so size it as the scroller was.
pub fn vbar(handle: &ScrollHandle, scroller: impl IntoElement) -> Div {
    div().relative().child(scroller).child(
        div()
            .absolute()
            .inset_0()
            .child(Scrollbar::vertical(handle).viewport_from_layout()),
    )
}
