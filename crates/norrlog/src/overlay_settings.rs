//! The Overlay and Charm overlay cards in Settings (settings.js).
//!
//! The controls only edit the saved settings and say what changed. The shell
//! holds the windows, so it opens, closes and reshapes them when it hears
//! [`OverlayRequest`].

use crate::overlay::{Geometry, Kind, Saved};
use crate::parts::{card, hint};
use crate::theme::px;
use crate::theme::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::{Sizable, h_flex, v_flex};

/// Every panel the overlay can show, as settings.js lists them. A panel
/// missing here can only be had by typing it into the options.
pub const PANELS: &[(&str, &str)] = &[
    ("meter", "Damage meter"),
    ("dps", "Damage per second"),
    ("outgoing", "Your damage, by attack"),
    ("buffs", "Buffs"),
    ("debuffs", "Debuffs"),
    ("auras", "Auras"),
    ("heals", "Healing"),
    ("taken", "Damage taken"),
    ("incoming", "Hitting you"),
    ("xp", "Experience"),
    ("motes", "Motes this session"),
    ("sky", "Sky turn-ins"),
    ("apps", "Other apps (the eqldb collector's prompt)"),
    ("charm", "Charm"),
];

/// What the shell is asked to do.
pub enum OverlayRequest {
    Toggle(Kind),
    /// The settings changed and are saved; an open window takes them.
    Apply(Kind, Saved),
}

struct Card {
    kind: Kind,
    saved: Saved,
    options: Entity<InputState>,
    x: Entity<InputState>,
    y: Entity<InputState>,
    w: Entity<InputState>,
    open: bool,
    /// The fields are behind `saved`, after a move the controls did not
    /// make; they catch up on the next render, which has a window.
    stale: bool,
}

pub struct OverlayControls {
    cards: Vec<Card>,
    /// The daemon's origin, for the OBS URL.
    base: Option<String>,
    err: Option<String>,
}

impl EventEmitter<OverlayRequest> for OverlayControls {}

impl OverlayControls {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cards = [Kind::Main, Kind::Charm]
            .into_iter()
            .map(|kind| {
                let saved = Saved::load(kind);
                let mut input =
                    |v: String| cx.new(|cx| InputState::new(window, cx).default_value(v));
                Card {
                    kind,
                    options: input(saved.options.clone()),
                    x: input(saved.bounds.x.to_string()),
                    y: input(saved.bounds.y.to_string()),
                    w: input(saved.bounds.width.to_string()),
                    saved,
                    open: false,
                    stale: false,
                }
            })
            .collect();
        OverlayControls {
            cards,
            base: None,
            err: None,
        }
    }

    pub fn set_base(&mut self, base: Option<String>, cx: &mut Context<Self>) {
        self.base = base;
        cx.notify();
    }

    /// Whether the overlay is open, and the settings it opened with, which
    /// differ from the fields when it had to be moved onto a display.
    pub fn set_open(
        &mut self,
        kind: Kind,
        open: bool,
        saved: Option<Saved>,
        cx: &mut Context<Self>,
    ) {
        let c = self.card_mut(kind);
        c.open = open;
        if let Some(s) = saved
            && s.bounds != c.saved.bounds
        {
            c.saved = s;
            c.stale = true;
        }
        cx.notify();
    }

    fn card_mut(&mut self, kind: Kind) -> &mut Card {
        self.cards
            .iter_mut()
            .find(|c| c.kind == kind)
            .expect("a card for each kind")
    }

    /// Save, then tell the shell.
    fn apply(&mut self, kind: Kind, saved: Saved, cx: &mut Context<Self>) {
        self.err = saved
            .save(kind)
            .err()
            .map(|e| format!("Could not save: {e}"));
        self.card_mut(kind).saved = saved.clone();
        cx.emit(OverlayRequest::Apply(kind, saved));
        cx.notify();
    }

    fn tick(
        &mut self,
        kind: Kind,
        panel: &'static str,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let c = self.card_mut(kind);
        let now = c.options.read(cx).value().to_string();
        let mut picked = panels_of(&now);
        picked.retain(|p| *p != panel);
        if on {
            picked.push(panel.to_string());
        }
        // Rebuilt in the list's order, so the string does not depend on
        // which box was clicked first.
        let ordered: Vec<&str> = PANELS
            .iter()
            .map(|p| p.0)
            .filter(|k| picked.iter().any(|p| p == k))
            .collect();
        let next = with_panels(&now, &ordered);
        c.options
            .update(cx, |s, cx| s.set_value(next.clone(), window, cx));
        let mut saved = c.saved.clone();
        saved.options = next;
        self.apply(kind, saved, cx);
    }

    fn apply_options(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let c = self.card_mut(kind);
        let mut saved = c.saved.clone();
        saved.options = c.options.read(cx).value().trim().to_string();
        self.apply(kind, saved, cx);
    }

    fn apply_bounds(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let c = self.card_mut(kind);
        let num = |e: &Entity<InputState>, cx: &App| {
            e.read(cx)
                .value()
                .trim()
                .parse::<f32>()
                .ok()
                .filter(|n| n.is_finite())
        };
        let b = c.saved.bounds;
        let bounds = Geometry {
            x: num(&c.x, cx).unwrap_or(b.x).round(),
            y: num(&c.y, cx).unwrap_or(b.y).round(),
            // Narrower than this is unreadable.
            width: num(&c.w, cx).unwrap_or(b.width).round().max(120.),
            height: b.height,
        };
        let mut saved = c.saved.clone();
        saved.bounds = bounds;
        self.apply(kind, saved, cx);
    }

    /// Back to the top left of the primary display, for an overlay that has
    /// ended up somewhere it cannot be seen.
    fn reset(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let origin = cx
            .primary_display()
            .map(|d| d.bounds().origin)
            .unwrap_or_default();
        let (x, y) = (f32::from(origin.x) + 40., f32::from(origin.y) + 40.);
        let c = self.card_mut(kind);
        c.x.update(cx, |s, cx| s.set_value(x.to_string(), window, cx));
        c.y.update(cx, |s, cx| s.set_value(y.to_string(), window, cx));
        self.apply_bounds(kind, cx);
    }

    fn click_through(&mut self, kind: Kind, on: bool, cx: &mut Context<Self>) {
        let mut saved = self.card_mut(kind).saved.clone();
        saved.click_through = on;
        self.apply(kind, saved, cx);
    }
}

/// The `panels` parameter, as the overlay reads it (`panelsOf`).
pub fn panels_of(options: &str) -> Vec<String> {
    crate::overlay::Options::parse(options)
        .panels
        .into_iter()
        .fold(Vec::new(), |mut v, p| {
            if !v.contains(&p) {
                v.push(p);
            }
            v
        })
}

/// Rewrite only `panels`, keeping the rest of the string as it was
/// (`withPanels`).
pub fn with_panels(options: &str, panels: &[&str]) -> String {
    let joined = format!("panels={}", panels.join(","));
    let mut parts: Vec<String> = options
        .trim_start_matches('?')
        .split('&')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    match parts
        .iter()
        .position(|p| p == "panels" || p.starts_with("panels="))
    {
        Some(i) => parts[i] = joined,
        None => parts.insert(0, joined),
    }
    parts.join("&")
}

const OPTIONS_HINT: &str = "The same settings as text, plus what the tickboxes do not cover: rows, buffs, scale, \
    self=1, enemies=0, crit=0, idle=1, scope=session or scope=zone.";
const BOUNDS_HINT: &str = "Where the overlay sits and how wide it is. Its height follows what it shows. A position \
    on no display is moved onto the one the app is on when the overlay opens.";
const CLICK_HINT: &str =
    "With click-through on, clicks go to the game underneath. Off, the overlay takes them.";
const CHARM_HINT: &str = "A second overlay, placed apart from the first. Its default, panels=charm, shows charm \
    uptime, what is charmed, and any charm that was resisted or refused. It hides itself when there is neither.";

impl Render for OverlayControls {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for c in self.cards.iter_mut().filter(|c| c.stale) {
            c.stale = false;
            let b = c.saved.bounds;
            for (e, v) in [(&c.x, b.x), (&c.y, b.y), (&c.w, b.width)] {
                e.update(cx, |s, cx| s.set_value(v.to_string(), window, cx));
            }
        }
        let mut col = v_flex().gap_3();
        for c in &self.cards {
            col = col.child(self.render_card(c, cx));
        }
        col.when_some(self.err.clone(), |d, e| {
            d.child(div().text_sm().text_color(hex(ERR)).child(e))
        })
    }
}

impl OverlayControls {
    fn render_card(&self, c: &Card, cx: &mut Context<Self>) -> Div {
        let kind = c.kind;
        let tag = match kind {
            Kind::Main => "ov",
            Kind::Charm => "ovc",
        };
        let id = |s: &str| SharedString::from(format!("{tag}-{s}"));
        let on: Vec<String> = panels_of(c.options.read(cx).value().as_ref());
        let mut boxes = h_flex().flex_wrap().gap_x_3().gap_y_1();
        for &(key, label) in PANELS {
            boxes = boxes.child(
                Checkbox::new(id(&format!("p-{key}")))
                    .label(label)
                    .checked(on.iter().any(|p| p == key))
                    .on_click(cx.listener(move |v, checked: &bool, window, cx| {
                        v.tick(kind, key, *checked, window, cx)
                    })),
            );
        }
        let geom = |label: &'static str, e: &Entity<InputState>| {
            h_flex()
                .gap_1()
                .items_center()
                .child(div().text_sm().text_color(hex(MUTED)).child(label))
                .child(div().w(px(72.)).child(Input::new(e).small()))
        };
        let title = match kind {
            Kind::Main => "Overlay",
            Kind::Charm => "Charm overlay",
        };
        card(title)
            .when(kind == Kind::Charm, |d| d.child(hint(CHARM_HINT)))
            .child(field("Panels shown").child(boxes))
            .child(
                field("Options").child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&c.options).small()))
                        .child(Button::new(id("apply-opts")).label("Apply").small().on_click(cx.listener(move |v, _, _, cx| v.apply_options(kind, cx)))),
                ),
            )
            .child(hint(OPTIONS_HINT))
            .child(
                Checkbox::new(id("click"))
                    .label("Click-through")
                    .checked(c.saved.click_through)
                    .on_click(cx.listener(move |v, checked: &bool, _, cx| v.click_through(kind, *checked, cx))),
            )
            .child(hint(CLICK_HINT))
            .child(
                field("Position and width").child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(geom("X", &c.x))
                        .child(geom("Y", &c.y))
                        .child(geom("W", &c.w))
                        .child(Button::new(id("apply-bounds")).label("Apply").small().on_click(cx.listener(move |v, _, _, cx| v.apply_bounds(kind, cx))))
                        .child(Button::new(id("reset")).label("Reset position").small().on_click(cx.listener(move |v, _, window, cx| v.reset(kind, window, cx)))),
                ),
            )
            .child(hint(BOUNDS_HINT))
            .when(kind == Kind::Main, |d| {
                let url = self.base.as_ref().map_or_else(|| "Waiting for the daemon…".to_string(), |b| format!("{b}/overlay?{}", c.saved.options));
                d.child(field("OBS Browser Source URL").child(div().text_sm().child(url)))
                    .child(hint("Add a Browser Source in OBS with this URL, about 360×420, with a transparent background."))
            })
            .child(
                h_flex().child(
                    Button::new(id("toggle"))
                        .label(if c.open { "Hide overlay" } else { "Show overlay" })
                        .small()
                        .when(!c.open, |b| b.primary())
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(OverlayRequest::Toggle(kind)))),
                ),
            )
    }
}

fn field(label: &str) -> Div {
    v_flex().gap_1().child(
        div()
            .text_sm()
            .text_color(hex(MUTED))
            .child(label.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn ticking_a_panel_rewrites_only_panels() {
        assert_eq!(
            with_panels("panels=meter,buffs&rows=6", &["meter"]),
            "panels=meter&rows=6"
        );
        assert_eq!(
            with_panels("rows=5&buffs=5", &["meter", "xp"]),
            "panels=meter,xp&rows=5&buffs=5"
        );
        assert_eq!(with_panels("", &[]), "panels=");
        assert_eq!(
            panels_of("panels=meter,wanted,apps"),
            ["meter", "apps"],
            "wanted ticks Other apps, once"
        );
        assert_eq!(panels_of(""), ["meter", "buffs"]);
    }

    #[test]
    fn every_panel_the_overlay_draws_has_a_box() {
        for p in [
            "meter", "xp", "heals", "taken", "motes", "dps", "outgoing", "incoming", "auras",
            "debuffs", "buffs", "charm", "apps", "sky",
        ] {
            assert!(PANELS.iter().any(|(k, _)| *k == p), "{p}");
        }
    }
}
