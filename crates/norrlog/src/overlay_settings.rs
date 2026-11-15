//! The Overlay and Charm overlay cards in Settings.
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

/// Every panel the overlay can show. A panel
/// missing here can only be had by typing it into the options.
pub const PANELS: &[(&str, &str)] = &[
    ("meter", "Damage meter"),
    ("dps", "Damage per second"),
    ("outgoing", "Your damage, by attack"),
    ("buffs", "Buffs"),
    ("debuffs", "Debuffs"),
    ("others", "Your spells on others (heals over time, dots)"),
    ("fct", "Floating combat text: hits, damage taken, heals"),
    ("auras", "Auras"),
    ("heals", "Healing"),
    ("taken", "Damage taken"),
    ("incoming", "Hitting you"),
    ("xp", "Experience"),
    ("motes", "Motes this session"),
    ("sky", "Plane of Sky turn-ins"),
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

/// A box on the placement map being dragged: which overlay, where the
/// pointer went down, and the geometry it started from.
struct MapDrag {
    kind: Kind,
    from: Point<Pixels>,
    start: Geometry,
    /// Map pixels per desktop pixel, as the map was drawn when it started.
    scale: f32,
}

pub struct OverlayControls {
    cards: Vec<Card>,
    drag: Option<MapDrag>,
    /// The placement map zoomed in to one display, by its index.
    zoom: Option<usize>,
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
            drag: None,
            zoom: None,
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

    /// The height an open overlay fitted itself to. Not saved: it follows
    /// the panels, and the map only draws it.
    pub fn set_height(&mut self, kind: Kind, height: f32, cx: &mut Context<Self>) {
        self.card_mut(kind).saved.bounds.height = height;
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
    self=1, enemies=0, crit=0, idle=1, alerts=0, scope=session or scope=zone.";
const BOUNDS_HINT: &str = "Where the overlay sits and how wide it is. Its height follows what it shows. A position \
    on no display, after a monitor is unplugged, say, is moved onto your main display when the overlay \
    opens.";
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
        let mut col = v_flex().gap_3().child(self.render_map(cx));
        for c in &self.cards {
            col = col.child(self.render_card(c, cx));
        }
        col.when_some(self.err.clone(), |d, e| {
            d.child(div().text_sm().text_color(hex(ERR)).child(e))
        })
    }
}

/// How wide the placement map is drawn, in layout pixels.
const MAP_WIDTH: f32 = 520.;
/// How close to a display's edge a dragged box snaps to it, in map pixels,
/// so a corner is as easy to hit zoomed out as in.
const SNAP_ON_MAP: f32 = 8.;

/// Where a dragged box lands: moved by the drag, and flush against any edge
/// of the display it is over that it comes within `snap` desktop pixels of.
fn dropped(start: Geometry, dx: f32, dy: f32, displays: &[Bounds<Pixels>], snap: f32) -> Geometry {
    let mut g = Geometry {
        x: (start.x + dx).round(),
        y: (start.y + dy).round(),
        ..start
    };
    for d in displays {
        let (l, t) = (f32::from(d.origin.x), f32::from(d.origin.y));
        let (r, b) = (l + f32::from(d.size.width), t + f32::from(d.size.height));
        // Only edges of the display the box is over.
        if g.x + g.width < l || g.x > r || g.y + g.height < t || g.y > b {
            continue;
        }
        if (g.x - l).abs() <= snap {
            g.x = l;
        } else if (g.x + g.width - r).abs() <= snap {
            g.x = r - g.width;
        }
        if (g.y - t).abs() <= snap {
            g.y = t;
        } else if (g.y + g.height - b).abs() <= snap {
            g.y = b - g.height;
        }
    }
    g
}

impl OverlayControls {
    /// Every display at its place in the desktop, scaled down, with the
    /// overlays on it as boxes to drag; or one display, zoomed, for placing
    /// one exactly. The overlays themselves can't be moved while they are
    /// open, so this is where they are placed: the window moves when the box
    /// is let go.
    fn render_map(&self, cx: &mut Context<Self>) -> Div {
        let displays: Vec<Bounds<Pixels>> = cx.displays().iter().map(|d| d.bounds()).collect();
        let Some(first) = displays.first().copied() else {
            return div();
        };
        let zoomed = self.zoom.and_then(|i| displays.get(i).copied());
        let view = zoomed.unwrap_or_else(|| displays.iter().fold(first, |a, d| a.union(d)));
        let (ox, oy) = (f32::from(view.origin.x), f32::from(view.origin.y));
        let scale = MAP_WIDTH / f32::from(view.size.width).max(1.);
        let at = |x: f32, y: f32| (px((x - ox) * scale), px((y - oy) * scale));
        let mut map = div()
            .id("overlay-map")
            .relative()
            .overflow_hidden()
            .w(px(MAP_WIDTH))
            .h(px(f32::from(view.size.height) * scale))
            .on_mouse_move(cx.listener(move |v, e: &MouseMoveEvent, _, cx| {
                let Some(d) = v.drag.as_ref() else { return };
                if e.pressed_button != Some(MouseButton::Left) {
                    v.drag = None;
                    cx.notify();
                    return;
                }
                let (dx, dy) = (
                    f32::from(e.position.x - d.from.x) / d.scale,
                    f32::from(e.position.y - d.from.y) / d.scale,
                );
                let (kind, start, snap) = (d.kind, d.start, SNAP_ON_MAP / d.scale);
                let displays: Vec<Bounds<Pixels>> =
                    cx.displays().iter().map(|d| d.bounds()).collect();
                v.card_mut(kind).saved.bounds = dropped(start, dx, dy, &displays, snap);
                v.card_mut(kind).stale = true;
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::map_drop))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::map_drop));
        for (i, d) in displays.iter().enumerate() {
            if zoomed.is_some() && self.zoom != Some(i) {
                continue;
            }
            let (x, y) = at(f32::from(d.origin.x), f32::from(d.origin.y));
            map = map.child(
                div()
                    .id(SharedString::from(format!("map-display-{i}")))
                    .absolute()
                    .left(x)
                    .top(y)
                    .w(px(f32::from(d.size.width) * scale))
                    .h(px(f32::from(d.size.height) * scale))
                    .border_1()
                    .border_color(hex(BORDER))
                    .bg(hex(PANEL_2))
                    .p(px(4.))
                    .text_xs()
                    .text_color(hex(MUTED))
                    .child(format!(
                        "{} · {}×{}",
                        i + 1,
                        f32::from(d.size.width),
                        f32::from(d.size.height)
                    ))
                    // Zoomed out, a click on a screen zooms in to it.
                    .when(zoomed.is_none(), |d| {
                        d.cursor(CursorStyle::PointingHand).on_click(cx.listener(
                            move |v, _, _, cx| {
                                v.zoom = Some(i);
                                cx.notify();
                            },
                        ))
                    }),
            );
        }
        for c in &self.cards {
            // The charm overlay only once it is wanted.
            if c.kind == Kind::Charm && !c.open {
                continue;
            }
            let kind = c.kind;
            let g = c.saved.bounds;
            let (x, y) = at(g.x, g.y);
            let dragging = self.drag.as_ref().is_some_and(|d| d.kind == kind);
            let (colour, label) = match kind {
                Kind::Main => (ACCENT, "overlay"),
                Kind::Charm => (PET, "charm"),
            };
            map = map.child(
                div()
                    .id(match kind {
                        Kind::Main => "map-main",
                        Kind::Charm => "map-charm",
                    })
                    .absolute()
                    .left(x)
                    .top(y)
                    .w(px((g.width * scale).max(6.)))
                    .h(px((g.height * scale).max(6.)))
                    .border_1()
                    .border_color(hex(colour))
                    .bg(wash(colour, if dragging { 0.55 } else { 0.3 }))
                    .overflow_hidden()
                    .px(px(2.))
                    .text_xs()
                    .text_color(hex(TEXT))
                    .child(label)
                    .when(!c.open, |d| d.opacity(0.6))
                    .cursor(if dragging {
                        CursorStyle::ClosedHand
                    } else {
                        CursorStyle::OpenHand
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |v, e: &MouseDownEvent, _, cx| {
                            // The screen under it must not zoom as well.
                            cx.stop_propagation();
                            v.drag = Some(MapDrag {
                                kind,
                                from: e.position,
                                start: v.card_mut(kind).saved.bounds,
                                scale,
                            });
                            cx.notify();
                        }),
                    ),
            );
        }
        let now = self.drag.as_ref().map(|d| {
            let g = self
                .cards
                .iter()
                .find(|c| c.kind == d.kind)
                .map(|c| c.saved.bounds);
            g.map_or(String::new(), |g| format!("{}, {}", g.x, g.y))
        });
        let help = match (now, zoomed.is_some()) {
            (Some(at), _) => format!("Release to move it to {at}."),
            (None, false) => {
                "Drag a box to place that overlay; it moves when you let go. Click a screen to \
                zoom in to it, for placing exactly."
                    .to_string()
            }
            (None, true) => {
                "Drag the box; it snaps flush to the screen's edges, and moves when you let go. \
                X and Y below are exact."
                    .to_string()
            }
        };
        card("Overlay placement")
            .when(zoomed.is_some(), |d| {
                d.child(
                    h_flex().child(
                        Button::new("map-all")
                            .label("All screens")
                            .small()
                            .on_click(cx.listener(|v, _, _, cx| {
                                v.zoom = None;
                                cx.notify();
                            })),
                    ),
                )
            })
            .child(map)
            .child(hint(help))
    }

    fn map_drop(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.drag.take() else { return };
        let c = self.card_mut(d.kind);
        if c.saved.bounds == d.start {
            cx.notify();
            return;
        }
        let saved = c.saved.clone();
        self.apply(d.kind, saved, cx);
    }

    fn render_card(&self, c: &Card, cx: &mut Context<Self>) -> Div {
        let kind = c.kind;
        let tag = match kind {
            Kind::Main => "ov",
            Kind::Charm => "ovc",
        };
        let id = |s: &str| SharedString::from(format!("{tag}-{s}"));
        let on: Vec<String> = panels_of(c.options.read(cx).value().as_ref());
        let mut boxes = h_flex().flex_wrap().gap_x_3().gap_y_1();
        // A panel the followed game has nothing for is not offered. Its tick
        // stays in the options, so switching back to Legends brings it back.
        for &(key, label) in PANELS
            .iter()
            .filter(|(k, _)| crate::features::panel_shown(k))
        {
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
    fn a_dragged_box_snaps_to_the_edges_of_the_display_it_is_on() {
        let screen = |x: f32, w: f32, h: f32| {
            Bounds::new(
                point(gpui::px(x), gpui::px(0.)),
                size(gpui::px(w), gpui::px(h)),
            )
        };
        let displays = [screen(0., 2560., 1440.), screen(2560., 1920., 1080.)];
        let g = Geometry {
            x: 100.,
            y: 100.,
            width: 400.,
            height: 300.,
        };
        // Near the top left: flush with it.
        assert_eq!(
            dropped(g, -95., -91., &displays, 12.),
            Geometry { x: 0., y: 0., ..g }
        );
        // Near the right edge of the first display: flush with it, from the
        // inside, though the second display starts there.
        let r = dropped(g, 2055., 0., &displays, 12.);
        assert_eq!((r.x, r.y), (2160., 100.));
        // Near the bottom of the second, shorter display.
        let b = dropped(Geometry { x: 2700., ..g }, 0., 675., &displays, 12.);
        assert_eq!(b.y, 780.);
        // Nowhere near an edge: where it was dropped.
        assert_eq!(
            dropped(g, 400.4, 300., &displays, 12.),
            Geometry {
                x: 500.,
                y: 400.,
                ..g
            }
        );
    }

    #[test]
    fn every_panel_the_overlay_draws_has_a_box() {
        for p in [
            "meter", "xp", "heals", "taken", "motes", "dps", "outgoing", "incoming", "auras",
            "debuffs", "others", "buffs", "fct", "charm", "apps", "sky",
        ] {
            assert!(PANELS.iter().any(|(k, _)| *k == p), "{p}");
        }
    }
}
