//! The palette: the Electron app's tokens (`gui/renderer/app.css`), shared
//! with the eqldb collector so the two read as one family. Every view takes
//! its colours from here, never from a literal.

use gpui::{App, Hsla, rgb};
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
    rgb(c).into()
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
pub const LINK: u32 = 0x22d3ee;
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
    let mut h: Hsla = rgb(c).into();
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
        c.scrollbar = hex(BG);
        c.scrollbar_thumb = hex(BORDER);
        c.success = hex(OK);
        c.warning = hex(WARN);
        c.danger = hex(ERR);
        c.info = hex(INFO);
    });
}
