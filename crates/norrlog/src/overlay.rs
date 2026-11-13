//! The in-game overlay: a transparent window over the game that draws the
//! same panels as the daemon's `/overlay` page.
//!
//! # Why a native window
//!
//! The daemon's page stays what OBS loads. Over the game, though, a window
//! has to stay on top of a fullscreen client, let clicks through, and take no
//! focus, and only a native window manages that everywhere: as a wlr-layer-
//! shell surface on the overlay layer under Wayland, and as an
//! override-redirect popup under X11, which the window manager can neither
//! lower nor decorate. A webview window around the page (the other way
//! tried) asked for "keep above" and went under a fullscreen game, took
//! focus, and would have cost the daemon WebKitGTK. Two drawings of the same
//! panels is the price, so this module follows `overlay.js` closely and
//! names the function it mirrors.
//!
//! # Settings
//!
//! The windows read and write the config's `overlayOptions`,
//! `overlayBounds`, `overlayClickThrough`, and `overlay2…` for the charm
//! overlay; the earlier Electron app wrote the same keys, so a layout made
//! there carried over. The options are the page's
//! query string, parsed the way `overlay.js` parses it.

use crate::format::{dur, num};
use gpui::*;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---- which overlay ---------------------------------------------------------

/// The main overlay, or the second one Electron opens for charm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Main,
    Charm,
}

impl Kind {
    fn options_key(self) -> &'static str {
        match self {
            Kind::Main => "overlayOptions",
            Kind::Charm => "overlay2Options",
        }
    }
    fn bounds_key(self) -> &'static str {
        match self {
            Kind::Main => "overlayBounds",
            Kind::Charm => "overlay2Bounds",
        }
    }
    fn click_key(self) -> &'static str {
        match self {
            Kind::Main => "overlayClickThrough",
            Kind::Charm => "overlay2ClickThrough",
        }
    }
    fn default_options(self) -> &'static str {
        match self {
            Kind::Main => "panels=meter,buffs&rows=6",
            Kind::Charm => "panels=charm&rows=5",
        }
    }
    fn default_bounds(self) -> Geometry {
        match self {
            Kind::Main => Geometry {
                x: 40.,
                y: 40.,
                width: 380.,
                height: 460.,
            },
            Kind::Charm => Geometry {
                x: 460.,
                y: 40.,
                width: 300.,
                height: 220.,
            },
        }
    }
}

/// Where the window sits, in the Electron config's shape.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Geometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// An overlay's saved settings.
#[derive(Debug, Clone)]
pub struct Saved {
    pub options: String,
    pub bounds: Geometry,
    pub click_through: bool,
}

impl Saved {
    /// From the config file, with Electron's defaults for anything missing.
    pub fn load(kind: Kind) -> Self {
        let cfg: Value = crate::config::path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        Saved {
            options: cfg
                .get(kind.options_key())
                .and_then(Value::as_str)
                .unwrap_or(kind.default_options())
                .to_string(),
            bounds: cfg
                .get(kind.bounds_key())
                .and_then(|b| serde_json::from_value::<Geometry>(b.clone()).ok())
                .filter(|g| g.width >= 40. && g.height >= 40.)
                .unwrap_or(kind.default_bounds()),
            click_through: cfg
                .get(kind.click_key())
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }

    pub fn save(&self, kind: Kind) -> std::io::Result<()> {
        let mut patch = serde_json::Map::new();
        patch.insert(kind.options_key().into(), self.options.clone().into());
        patch.insert(
            kind.bounds_key().into(),
            serde_json::to_value(self.bounds).map_err(std::io::Error::other)?,
        );
        patch.insert(kind.click_key().into(), self.click_through.into());
        crate::config::save_patch(patch)
    }
}

// ---- options, as overlay.js reads its query string -------------------------

/// The page's `opt`.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub panels: Vec<String>,
    pub rows: usize,
    pub buffs: usize,
    pub scale: f32,
    pub self_only: bool,
    pub show_enemies: bool,
    pub show_crit: bool,
    pub scope: Option<String>,
    pub keep_idle: bool,
}

impl Options {
    /// `panels=meter,buffs&rows=6`, read with the page's defaults. An empty
    /// `panels=` is no panels, not the default (`??` in the page).
    pub fn parse(query: &str) -> Self {
        let mut get = std::collections::HashMap::new();
        for pair in query
            .trim_start_matches('?')
            .split('&')
            .filter(|p| !p.is_empty())
        {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            get.insert(k.to_string(), decode(v));
        }
        let int = |k: &str, d: usize| get.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        let panels = get
            .get("panels")
            .map(String::as_str)
            .unwrap_or("meter,buffs")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            // `wanted` is the old name for `apps`.
            .map(|s| {
                if s == "wanted" {
                    "apps".to_string()
                } else {
                    s.to_string()
                }
            })
            .collect();
        Options {
            panels,
            rows: int("rows", 6),
            buffs: int("buffs", 8),
            scale: get
                .get("scale")
                .and_then(|v| v.parse().ok())
                .filter(|s: &f32| *s > 0.)
                .unwrap_or(1.),
            self_only: get.get("self").is_some_and(|v| v == "1"),
            show_enemies: get.get("enemies").is_none_or(|v| v != "0"),
            show_crit: get.get("crit").is_none_or(|v| v != "0"),
            scope: get
                .get("scope")
                .filter(|v| *v == "session" || *v == "zone")
                .cloned(),
            keep_idle: get.get("idle").is_some_and(|v| v == "1"),
        }
    }

    fn has(&self, panel: &str) -> bool {
        self.panels.iter().any(|p| p == panel)
    }
}

/// `%2C` and `+` as a browser would read them; nothing else in these
/// options needs more.
fn decode(v: &str) -> String {
    let v = v.replace('+', " ");
    let mut out = Vec::with_capacity(v.len());
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // Bytes, not a `&str` slice, which would panic on a character
        // that is not ASCII straight after a `%`.
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(n) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(n);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---- colours, from overlay.css ---------------------------------------------

const TEXT: u32 = 0xe8eaf0;
const DIM: u32 = 0x9aa2b0;
const ACCENT: u32 = 0xe0ad46;
const YOU: u32 = 0x22d3ee;
const ENEMY: u32 = 0xdd7076;
const PET: u32 = 0xb195e0;
const HEAL: u32 = 0x86cc74;
const WASH: u32 = 0xe8eaf0;
const SCRIM: u32 = 0x0e1014;

fn rgba_of(rgb: u32, a: f32) -> Rgba {
    let mut c = gpui::rgb(rgb);
    c.a = a;
    c
}

/// `.row.you` / `.pet` / `.enemy`, or a plain row.
#[derive(Clone, Copy)]
enum Side {
    You,
    Pet,
    Enemy,
    Plain,
}

impl Side {
    fn of(r: &Value) -> Self {
        if flag(r, "is_you") {
            Side::You
        } else if flag(r, "is_pet") {
            Side::Pet
        } else if flag(r, "is_enemy") {
            Side::Enemy
        } else {
            Side::Plain
        }
    }
    fn bar(self) -> Rgba {
        match self {
            Side::You => rgba_of(YOU, 0.4),
            Side::Enemy => rgba_of(ENEMY, 0.3),
            Side::Pet => rgba_of(PET, 0.34),
            Side::Plain => rgba_of(WASH, 0.15),
        }
    }
    fn name(self) -> u32 {
        match self {
            Side::You => YOU,
            Side::Pet => PET,
            _ => TEXT,
        }
    }
}

fn flag(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}
fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.)
}
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// A length in the page's CSS pixels. The window's rem is 16 of them times
/// `scale=`, so the option grows the whole layout, as the page's transform
/// does, and not only its text.
fn u(v: f32) -> Rems {
    rems(v / 16.)
}

// ---- the view --------------------------------------------------------------

/// Where the user dragged an overlay to. The shell saves it and reopens the
/// window there: neither a layer surface nor an override-redirect popup can
/// be moved by the window manager, or by GPUI once it exists.
pub struct Moved(pub Geometry);

impl EventEmitter<Moved> for OverlayView {}

/// The height the overlay fitted itself to, for the placement map in
/// Settings to draw it at its real size.
pub struct Fitted(pub f32);

impl EventEmitter<Fitted> for OverlayView {}

/// Movement under this is a click, not a drag.
const DRAG_SLOP: f32 = 4.;

pub struct OverlayView {
    /// What it was opened with, for the shell to tell what changed.
    pub saved: Saved,
    opts: Options,
    snap: Option<Arc<Value>>,
    /// When the snapshot arrived, so buff timers count down between pushes,
    /// as the page's one-second tick does.
    at: Instant,
    /// The daemon is gone; the page shows "Reconnecting…".
    lost: bool,
    /// What the page polls for, each at the page's own interval, and only
    /// while its panel is shown: `/api/charm`, `/api/incoming`, `/api/sky`,
    /// `/api/panels` and `/api/session`.
    charm: Option<Value>,
    incoming: Option<Value>,
    sky: Option<Value>,
    apps: Vec<Value>,
    session: Option<Value>,
    dps: Dps,
    /// Seconds since the overlay opened, to space the polls.
    ticks: u64,
    /// More panels than the screen has room for; the bottom edge says so.
    clipped: bool,
    /// A drag in progress: where the button went down, and where the
    /// pointer is now, in the window's coordinates.
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
    api: Option<crate::api::Api>,
    _tick: Task<()>,
}

/// Damage per log-second for the dps panel (`renderDps`): the difference
/// between successive snapshots' running totals, put in the second the log
/// reported it. On the log clock, so a backlog caught up in one instant is
/// not read as a burst.
#[derive(Default)]
struct Dps {
    fight: Option<u64>,
    prev: Option<f64>,
    per_second: std::collections::BTreeMap<i64, f64>,
}

const DPS_ROLL: i64 = 10;
const DPS_SPARK: i64 = 60;

impl Dps {
    fn on_snapshot(&mut self, now: Option<i64>, enc: Option<&Value>) {
        let (Some(now), Some(enc)) = (now, enc) else {
            return;
        };
        let id = enc.get("id").and_then(Value::as_u64);
        // A new fight starts a new history.
        if id != self.fight {
            *self = Dps {
                fight: id,
                ..Default::default()
            };
        }
        let total = f(enc, "your_damage");
        if let Some(prev) = self.prev
            && total > prev
        {
            *self.per_second.entry(now).or_default() += total - prev;
        }
        self.prev = Some(total);
        self.per_second.retain(|t, _| now - t <= DPS_SPARK);
    }

    /// The last minute, a bucket a second, idle seconds as real zeroes.
    fn buckets(&self, now: i64) -> Vec<f64> {
        (now - DPS_SPARK + 1..=now)
            .map(|t| self.per_second.get(&t).copied().unwrap_or(0.))
            .collect()
    }
}

/// How long a meter row stays at full strength once it goes quiet, how long
/// it then takes to fade, and how far (`STALE_*` in the page).
const STALE_AFTER: f64 = 8.;
const STALE_FADE: f64 = 22.;
const STALE_FLOOR: f64 = 0.34;

impl OverlayView {
    pub fn new(saved: Saved, cx: &mut Context<Self>) -> Self {
        let opts = Options::parse(&saved.options);
        // The page's one-second tick for buff timers, and the charm poll.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(()) = this.update(cx, |v, cx| {
                    v.poll(cx);
                    v.ticks += 1;
                    cx.notify();
                }) else {
                    break;
                };
            }
        });
        OverlayView {
            saved,
            opts,
            snap: None,
            at: Instant::now(),
            lost: false,
            charm: None,
            incoming: None,
            sky: None,
            apps: Vec::new(),
            session: None,
            dps: Dps::default(),
            ticks: 0,
            clipped: false,
            drag: None,
            api: None,
            _tick: tick,
        }
    }

    pub fn set_api(&mut self, api: Option<crate::api::Api>, cx: &mut Context<Self>) {
        self.lost = api.is_none();
        self.api = api;
        cx.notify();
    }

    pub fn on_snapshot(&mut self, snap: Arc<Value>, cx: &mut Context<Self>) {
        if self.opts.has("dps") {
            // Not under a scope: a session has no "now".
            let enc = if self.opts.scope.is_some() {
                None
            } else {
                snap.get("current").filter(|c| !c.is_null())
            };
            self.dps
                .on_snapshot(snap.get("log_time").and_then(Value::as_i64), enc);
        }
        self.snap = Some(snap);
        self.at = Instant::now();
        self.lost = false;
        cx.notify();
    }

    /// New options from Settings.
    pub fn set_options(&mut self, options: String, cx: &mut Context<Self>) {
        self.opts = Options::parse(&options);
        self.saved.options = options;
        // A panel that polls starts at once, not on its interval.
        self.ticks = 0;
        cx.notify();
    }

    /// The page's polls, on its intervals: charm every second, incoming
    /// and the session every two, other apps' panels every five, Sky every
    /// twenty. Each starts on the first tick.
    fn poll(&mut self, cx: &mut Context<Self>) {
        let n = self.ticks;
        let o = &self.opts;
        if o.has("charm") {
            self.fetch("/api/charm".into(), cx, |me, v| me.charm = Some(v));
        }
        if o.has("incoming") && n.is_multiple_of(2) {
            // The fight in progress, or nothing: between fights the panel
            // hides rather than show a dead one.
            match self
                .snap
                .as_ref()
                .and_then(|s| s.get("current"))
                .and_then(|c| c.get("id"))
                .and_then(Value::as_u64)
            {
                Some(id) => self.fetch(format!("/api/incoming?encounter={id}"), cx, |me, v| {
                    me.incoming = Some(v)
                }),
                None => self.incoming = None,
            }
        }
        if let Some(scope) = o.scope.clone()
            && n.is_multiple_of(2)
        {
            self.fetch(format!("/api/session?scope={scope}"), cx, |me, v| {
                me.session = Some(v)
            });
        }
        if o.has("apps") && n.is_multiple_of(5) {
            self.fetch("/api/panels".into(), cx, |me, v| {
                me.apps = v
                    .get("panels")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
            });
        }
        if o.has("sky") && n.is_multiple_of(20) && crate::features::has("sky") {
            self.fetch("/api/sky".into(), cx, |me, v| me.sky = Some(v));
        }
    }

    /// A GET whose answer `put` keeps. A failure is silent, as in the page:
    /// the last answer stands until the next tick.
    fn fetch(
        &self,
        path: String,
        cx: &mut Context<Self>,
        put: impl FnOnce(&mut Self, Value) + 'static,
    ) {
        let Some(api) = self.api.clone() else { return };
        cx.spawn(async move |this, cx| {
            if let Ok(v) = api.get::<Value>(path).await {
                let _ = this.update(cx, |me, cx| {
                    put(me, v);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Seconds since the snapshot, for timers counting down between pushes.
    fn elapsed(&self) -> f64 {
        self.at.elapsed().as_secs_f64()
    }
}

impl Render for OverlayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(16. * self.opts.scale));
        let me = cx.entity().downgrade();
        let col = div()
            .flex()
            .flex_col()
            .gap(u(6.))
            .w_full()
            .text_color(gpui::rgb(TEXT))
            .text_size(u(13.))
            .on_children_prepainted(move |children, window, cx| {
                let (over, height) = fit_height(children, window, cx);
                let _ = me.update(cx, |v, cx| {
                    if v.clipped != over {
                        v.clipped = over;
                        cx.notify();
                    }
                    if let Some(h) = height
                        && (v.saved.bounds.height - h).abs() > 4.
                    {
                        v.saved.bounds.height = h;
                        cx.emit(Fitted(h));
                    }
                });
            });
        let col = self.panels(col);
        let root = div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(col)
            .children(self.clipped.then(more_below));
        // With click-through on the mouse never reaches the window.
        if self.saved.click_through {
            return root;
        }
        root.cursor(if self.drag.is_some() {
            CursorStyle::ClosedHand
        } else {
            CursorStyle::OpenHand
        })
        .children(self.drag_hint())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|v, e: &MouseDownEvent, _, cx| {
                v.drag = Some((e.position, e.position));
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(|v, e: &MouseMoveEvent, _, cx| {
            if let Some((_, at)) = v.drag.as_mut() {
                if e.pressed_button == Some(MouseButton::Left) {
                    *at = e.position;
                } else {
                    // The release happened somewhere we did not hear it.
                    v.drag = None;
                }
                cx.notify();
            }
        }))
        .on_mouse_up(MouseButton::Left, cx.listener(Self::drop_at))
        .on_mouse_up_out(MouseButton::Left, cx.listener(Self::drop_at))
    }
}

impl OverlayView {
    /// Where the window goes once the drag ends.
    fn dragged_to(&self) -> Option<Geometry> {
        let (from, to) = self.drag?;
        let (dx, dy) = (f32::from(to.x - from.x), f32::from(to.y - from.y));
        (dx.abs() >= DRAG_SLOP || dy.abs() >= DRAG_SLOP).then(|| Geometry {
            x: (self.saved.bounds.x + dx).round(),
            y: (self.saved.bounds.y + dy).round(),
            ..self.saved.bounds
        })
    }

    fn drop_at(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, at)) = self.drag.as_mut() {
            *at = e.position;
        }
        let to = self.dragged_to();
        self.drag = None;
        cx.notify();
        if let Some(g) = to {
            cx.emit(Moved(g));
        }
    }

    /// An outline and where the overlay will land, while it is dragged.
    fn drag_hint(&self) -> Option<Div> {
        let g = self.dragged_to()?;
        Some(
            div()
                .absolute()
                .inset_0()
                .border_2()
                .border_color(rgba_of(ACCENT, 0.9))
                .rounded(u(6.))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .px(u(8.))
                        .py(u(4.))
                        .rounded(u(4.))
                        .bg(rgba_of(SCRIM, 0.92))
                        .text_color(gpui::rgb(ACCENT))
                        .child(format!("release to move to {}, {}", g.x, g.y)),
                ),
        )
    }

    /// The panels, in the page's order, then the status line.
    fn panels(&self, mut col: Div) -> Div {
        let o = &self.opts;
        let Some(snap) = self.snap.clone() else {
            // "Connecting…", with nothing else drawn yet.
            if o.has("meter") {
                col = col.child(panel("Connecting…".into(), None, true, vec![]));
            }
            return col.child(status(self.lost.then_some(RECONNECTING)));
        };
        let snap = &*snap;
        // The page's fixed order, whatever order `panels=` names them in.
        for p in [
            "meter", "xp", "heals", "taken", "motes", "dps", "outgoing", "incoming", "auras",
            "debuffs", "buffs", "charm", "apps", "sky",
        ] {
            if !o.has(p) {
                continue;
            }
            let el = match p {
                "meter" => Some(self.meter(snap)),
                "xp" => self.xp(snap),
                "heals" => Some(self.metric(
                    snap,
                    "Healing",
                    "healing",
                    Some("hps"),
                    "heal_share",
                    "No healing yet",
                )),
                "taken" => Some(self.metric(
                    snap,
                    "Damage taken",
                    "damage_taken",
                    None,
                    "taken_share",
                    "No damage taken",
                )),
                "motes" => Some(self.motes(snap)),
                "auras" => Some(self.auras(snap)),
                "debuffs" => Some(self.debuffs(snap)),
                "buffs" => Some(self.buffs(snap)),
                "charm" => self.charm_panel(),
                "dps" => self.dps_panel(snap),
                "outgoing" => self.outgoing(snap),
                "incoming" => self.incoming_panel(),
                "apps" => self.app_panels(),
                "sky" => self.sky_panel(),
                _ => None,
            };
            if let Some(el) = el {
                col = col.child(el);
            }
        }
        let live = snap.get("live").and_then(Value::as_bool) != Some(false);
        col.child(status(if self.lost {
            Some(RECONNECTING)
        } else if !live {
            Some(READING)
        } else {
            None
        }))
    }
}

impl OverlayView {
    /// The encounter the meter shows: the session or zone total under a
    /// scope, else the fight in progress, or the last one with `idle=1`
    /// (`render` in the page).
    fn encounter<'a>(&'a self, snap: &'a Value) -> Option<&'a Value> {
        if self.opts.scope.is_some() {
            let s = self.session.as_ref()?;
            return (s.get("fights").and_then(Value::as_u64).unwrap_or(0) > 0)
                .then(|| s.get("summary"))
                .flatten();
        }
        snap.get("current").filter(|c| !c.is_null()).or_else(|| {
            self.opts
                .keep_idle
                .then(|| snap.get("recent").and_then(|r| r.get(0)))
                .flatten()
        })
    }

    /// `render`'s meter half and `renderRows`, `renderSub`.
    fn meter(&self, snap: &Value) -> Div {
        let scoped = self.opts.scope.is_some();
        let Some(enc) = self.encounter(snap) else {
            let title = if scoped {
                "Nothing fought yet"
            } else {
                "Waiting for combat"
            };
            return panel(title.into(), None, true, vec![empty("No active encounter")]);
        };
        // A session total is never idle: it is not waiting on anything.
        let idle = !scoped && snap.get("current").is_none_or(Value::is_null);
        let meta = if scoped {
            // Combat time, said as such, so it does not read as time played.
            let fights = self
                .session
                .as_ref()
                .and_then(|s| s.get("fights"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            format!(
                "{} dps · {} fighting · {fights} fights",
                num(Some(f(enc, "your_dps"))),
                dur(f(enc, "duration"))
            )
        } else {
            format!(
                "{} dps · {}",
                num(Some(f(enc, "your_dps"))),
                dur(f(enc, "duration"))
            )
        };
        let mut rows: Vec<Value> = fold_pets(
            enc.get("meter")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        if self.opts.self_only {
            rows.retain(|r| flag(r, "is_you"));
        }
        if !self.opts.show_enemies {
            rows.retain(|r| !flag(r, "is_enemy") || flag(r, "is_pet"));
        }
        rows.truncate(self.opts.rows);
        let end = f(enc, "end");
        let mut out: Vec<AnyElement> = if rows.is_empty() {
            vec![empty("No damage yet")]
        } else {
            rows.iter()
                .map(|r| {
                    let side = Side::of(r);
                    let quiet = r
                        .get("last_action")
                        .and_then(Value::as_f64)
                        .map_or(0., |la| (end - la).max(0.));
                    let fade = if quiet <= STALE_AFTER {
                        1.
                    } else {
                        (1. - (quiet - STALE_AFTER) / STALE_FADE).max(STALE_FLOOR)
                    };
                    let mut nums = Vec::new();
                    if self.opts.show_crit {
                        nums.push((format!("{:.0}%", f(r, "crit_rate") * 100.), DIM));
                    }
                    nums.push((num(Some(f(r, "damage"))), TEXT));
                    nums.push((num(Some(f(r, "dps"))), DIM));
                    bar_row(
                        name_with_tag(r, side),
                        nums,
                        f(r, "share"),
                        side.bar(),
                        fade,
                    )
                    .into_any_element()
                })
                .collect()
        };
        out.push(sub_chips(snap, enc).into_any_element());
        let title = s(enc, "title");
        panel(
            if title.is_empty() {
                "Encounter".into()
            } else {
                title.to_string()
            },
            Some(meta),
            idle,
            out,
        )
    }

    /// `renderMetric`: healing and damage taken.
    fn metric(
        &self,
        snap: &Value,
        title: &str,
        value: &str,
        rate: Option<&str>,
        share: &str,
        empty_text: &str,
    ) -> Div {
        let Some(enc) = self.encounter(snap) else {
            return panel(title.into(), None, false, vec![empty("—")]);
        };
        let mut rows: Vec<&Value> = enc
            .get("meter")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter(|r| f(r, value) > 0.).collect())
            .unwrap_or_default();
        rows.sort_by(|a, b| f(b, value).total_cmp(&f(a, value)));
        rows.truncate(self.opts.rows);
        if rows.is_empty() {
            return panel(title.into(), None, false, vec![empty(empty_text)]);
        }
        let out = rows
            .iter()
            .map(|r| {
                let side = Side::of(r);
                let mut nums = vec![(num(Some(f(r, value))), TEXT)];
                if let Some(k) = rate {
                    nums.push((num(Some(f(r, k))), DIM));
                }
                bar_row(
                    div()
                        .text_color(gpui::rgb(side.name()))
                        .child(s(r, "name").to_string()),
                    nums,
                    f(r, share),
                    side.bar(),
                    1.,
                )
                .into_any_element()
            })
            .collect();
        panel(title.into(), None, false, out)
    }

    /// `renderXp`, with `potionRow`.
    fn xp(&self, snap: &Value) -> Option<Div> {
        let Some(x) = snap.get("exp").filter(|x| !x.is_null()) else {
            return Some(panel("Experience".into(), None, false, vec![]));
        };
        let lvl = snap.get("level").and_then(Value::as_i64).unwrap_or(0);
        let levelling = s(snap, "levelling");
        let title = if levelling.is_empty() {
            format!("Level {lvl}")
        } else {
            format!("Level {lvl} ({levelling})")
        };
        let cap = snap.get("level_cap").and_then(Value::as_i64).unwrap_or(50);
        let capped = lvl >= cap;
        let rate = f(x, "rate_per_hour");
        let mut bits = Vec::new();
        if !capped && rate > 0. {
            bits.push(format!("{rate:.1} %/hr"));
        }
        if f(x, "aa_per_hour") > 0. {
            bits.push(format!("{:.1} AA/hr", f(x, "aa_per_hour")));
        }
        if !capped && rate > 0. {
            bits.push(format!("{}/level", long_dur(100. / rate * 3600., true)));
        }
        let mut rows = Vec::new();
        if !capped {
            let pct = f(x, "percent").clamp(0., 100.);
            let eta = x
                .get("seconds_to_next_level")
                .and_then(Value::as_f64)
                .map_or("—".to_string(), |s| long_dur(s, false));
            let kills = x.get("kills").and_then(Value::as_u64).unwrap_or(0);
            rows.push(
                bar_row(
                    div()
                        .text_color(gpui::rgb(YOU))
                        .child(format!("{pct:.1}% → {}", lvl + 1)),
                    vec![(format!("{kills} kills"), TEXT), (eta, DIM)],
                    pct / 100.,
                    Side::You.bar(),
                    1.,
                )
                .into_any_element(),
            );
        }
        rows.push(
            bar_row(
                div().child("AA"),
                vec![(
                    format!(
                        "{} unspent",
                        x.get("aa_unspent").and_then(Value::as_u64).unwrap_or(0)
                    ),
                    DIM,
                )],
                0.,
                Side::Plain.bar(),
                1.,
            )
            .into_any_element(),
        );
        if flag(x, "aa_potion_active") {
            let c = x
                .get("aa_potion_charges")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            // Amber on the last charge, as a cue to drink another.
            let bar = if c <= 1 {
                rgba_of(ACCENT, 0.4)
            } else {
                Side::Pet.bar()
            };
            rows.push(
                bar_row(
                    div().child("XP potion"),
                    vec![(format!("{c} left"), DIM)],
                    c.min(5) as f64 / 5.,
                    bar,
                    1.,
                )
                .into_any_element(),
            );
        }
        Some(panel(
            title,
            (!bits.is_empty()).then(|| bits.join(" · ")),
            false,
            rows,
        ))
    }

    /// `renderMotes`.
    fn motes(&self, snap: &Value) -> Div {
        let rows: Vec<(String, f64)> = snap
            .get("session_motes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| Some((p.get(0)?.as_str()?.to_string(), p.get(1)?.as_f64()?)))
                    .collect()
            })
            .unwrap_or_default();
        let total = f(snap, "session_motes_total");
        let hours = f(snap, "session_seconds") / 3600.;
        let meta = (total > 0.).then(|| {
            // A rate only once the sitting is long enough to mean one.
            if hours >= 0.25 {
                format!("{total} this session · {:.1}/hr", total / hours)
            } else {
                format!("{total} this session")
            }
        });
        if rows.is_empty() {
            return panel("Motes".into(), meta, false, vec![empty("None yet")]);
        }
        let max = rows.iter().map(|r| r.1).fold(1., f64::max);
        let out = rows
            .iter()
            .map(|(name, n)| {
                let short = name
                    .strip_prefix("Mote of ")
                    .or_else(|| name.strip_prefix("mote of "))
                    .unwrap_or(name);
                bar_row(
                    div().child(short.to_string()),
                    vec![(format!("{n}"), TEXT)],
                    n / max,
                    Side::Plain.bar(),
                    1.,
                )
                .into_any_element()
            })
            .collect();
        panel("Motes".into(), meta, false, out)
    }

    /// `renderAuras`.
    fn auras(&self, snap: &Value) -> Div {
        let auras = snap
            .get("auras")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if auras.is_empty() {
            return panel("Auras".into(), None, false, vec![empty("No auras")]);
        }
        let out = auras
            .iter()
            .take(5)
            .map(|a| {
                bar_row(
                    div()
                        .text_color(gpui::rgb(PET))
                        .child(s(a, "name").to_string()),
                    vec![("up".into(), DIM)],
                    1.,
                    Side::Pet.bar(),
                    1.,
                )
                .into_any_element()
            })
            .collect();
        panel(
            "Auras".into(),
            Some(format!("{}/5", auras.len())),
            false,
            out,
        )
    }

    /// `renderDebuffs`.
    fn debuffs(&self, snap: &Value) -> Div {
        let list = self.timed(snap, "debuffs");
        if list.is_empty() {
            return panel("Debuffs".into(), None, false, vec![empty("None")]);
        }
        let out = list
            .into_iter()
            .map(|(b, left, share)| {
                timer_row(&b, left, share, rgba_of(ENEMY, 0.4), TEXT).into_any_element()
            })
            .collect();
        panel("Debuffs".into(), None, false, out)
    }

    /// `renderBuffs`: timed first, permanents last; amber at 30 s, red at 10.
    fn buffs(&self, snap: &Value) -> Div {
        let mut list = self.timed(snap, "buffs");
        list.sort_by_key(|(b, _, _)| flag(b, "permanent"));
        if list.is_empty() {
            return panel("Buffs".into(), None, false, vec![empty("No tracked buffs")]);
        }
        let out = list
            .into_iter()
            .map(|(b, left, share)| {
                let cat = s(&b, "category");
                let bar = match cat {
                    "heal" => rgba_of(HEAL, 0.32),
                    "buff" | "cure" => rgba_of(ACCENT, 0.3),
                    "detrimental" | "damage" => rgba_of(ENEMY, 0.34),
                    _ => rgba_of(WASH, 0.14),
                };
                let time = if flag(&b, "permanent") || b.get("overrun").is_some() {
                    TEXT
                } else if left <= 10. {
                    ENEMY
                } else if left <= 30. {
                    ACCENT
                } else {
                    TEXT
                };
                timer_row(&b, left, share, bar, time).into_any_element()
            })
            .collect();
        panel("Buffs".into(), None, false, out)
    }

    /// Effects with a known remaining time, counted down since the snapshot,
    /// as the page's local tick does between pushes.
    fn timed(&self, snap: &Value, key: &str) -> Vec<(Value, f64, f64)> {
        let gone = self.elapsed();
        snap.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|b| {
                        let rem = b.get("remaining").and_then(Value::as_f64)?;
                        let perm = flag(b, "permanent");
                        let over = b.get("overrun").is_some();
                        let left = if perm || over {
                            rem
                        } else {
                            (rem - gone).max(0.)
                        };
                        let total = b
                            .get("total")
                            .and_then(Value::as_f64)
                            .filter(|t| *t > 0.)
                            .unwrap_or(1.);
                        let share = if perm {
                            1.
                        } else if over {
                            0.
                        } else {
                            (left / total).clamp(0., 1.)
                        };
                        Some((b.clone(), left, share))
                    })
                    .take(self.opts.buffs)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The charm overlay's panel: held charms, then recent failures. Hidden
    /// when there is neither.
    fn charm_panel(&self) -> Option<Div> {
        let c = self.charm.as_ref()?;
        let held: Vec<&str> = c
            .get("charmed")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let fails: Vec<&Value> = c
            .get("failures")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        if held.is_empty() && fails.is_empty() {
            return None;
        }
        let now = crate::format::now_log();
        let mut rows: Vec<AnyElement> = held
            .iter()
            .map(|n| {
                bar_row(
                    div().text_color(gpui::rgb(PET)).child(n.to_string()),
                    vec![("charmed".into(), DIM)],
                    1.,
                    Side::Pet.bar(),
                    1.,
                )
                .into_any_element()
            })
            .collect();
        for fl in fails.iter().take(self.opts.rows) {
            let ago = fl
                .get("at")
                .and_then(Value::as_i64)
                .map_or(0, |at| (now - at).max(0));
            rows.push(
                bar_row(
                    div()
                        .flex()
                        .gap_1()
                        .child(s(fl, "target").to_string())
                        .child(
                            div()
                                .text_color(gpui::rgb(ENEMY))
                                .child(s(fl, "kind").to_string()),
                        ),
                    vec![(format!("{ago}s ago"), DIM)],
                    1.,
                    Side::Enemy.bar(),
                    1.,
                )
                .into_any_element(),
            );
        }
        let meta = if held.is_empty() {
            "no charm".to_string()
        } else {
            let breaks = c.get("breaks").and_then(Value::as_u64).unwrap_or(0);
            let up = format!("{:.0}% uptime", f(c, "uptime") * 100.);
            if breaks == 0 {
                up
            } else {
                format!(
                    "{up} · {breaks} break{}",
                    if breaks == 1 { "" } else { "s" }
                )
            }
        };
        Some(panel("Charm".into(), Some(meta), held.is_empty(), rows))
    }
}

impl OverlayView {
    /// `renderDps`: damage over the last ten seconds, against the fight's
    /// average and the minute's peak, over a bar a second.
    fn dps_panel(&self, snap: &Value) -> Option<Div> {
        let enc = snap
            .get("current")
            .filter(|c| !c.is_null() && self.opts.scope.is_none())?;
        let now = snap.get("log_time").and_then(Value::as_i64)?;
        let buckets = self.dps.buckets(now);
        let rolled: f64 = buckets[buckets.len() - DPS_ROLL as usize..].iter().sum();
        // Divided by the seconds the window covers, so a fight three seconds
        // old is not averaged over ten.
        let covered = f(enc, "duration").clamp(1., DPS_ROLL as f64);
        let peak = buckets.iter().copied().fold(1., f64::max);
        let meta = format!(
            "{} now · {} avg · peak {}",
            num(Some((rolled / covered).round())),
            num(Some(f(enc, "your_dps"))),
            num(Some(peak))
        );
        let spark = div()
            .w_full()
            .flex()
            .items_end()
            .gap(u(1.))
            .h(u(24.))
            .children(buckets.into_iter().map(|v| {
                div()
                    .flex_1()
                    .h(relative((v / peak) as f32))
                    .bg(rgba_of(YOU, if v > 0. { 0.7 } else { 0. }))
            }));
        Some(panel(
            "Damage".into(),
            Some(meta),
            false,
            vec![spark.into_any_element()],
        ))
    }

    /// `renderOutgoing`: your abilities across their groups, ranked together,
    /// each a share of everything you dealt.
    fn outgoing(&self, snap: &Value) -> Option<Div> {
        let enc = self.encounter(snap)?;
        let total = f(enc, "your_damage");
        let mut abilities: Vec<&Value> = enc
            .get("outgoing_groups")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|g| g.get("abilities").and_then(Value::as_array))
            .flatten()
            .collect();
        if abilities.is_empty() || total <= 0. {
            return None;
        }
        abilities.sort_by(|a, b| f(b, "damage").total_cmp(&f(a, "damage")));
        abilities.truncate(self.opts.rows);
        let rows = abilities
            .iter()
            .map(|a| {
                bar_row(
                    div().child(s(a, "name").to_string()),
                    vec![
                        (
                            format!(
                                "{}× max {}",
                                num(Some(f(a, "hits"))),
                                num(Some(f(a, "max")))
                            ),
                            DIM,
                        ),
                        (num(Some(f(a, "damage"))), TEXT),
                    ],
                    f(a, "damage") / total,
                    Side::You.bar(),
                    1.,
                )
                .into_any_element()
            })
            .collect();
        Some(panel(
            "Your damage".into(),
            Some(format!(
                "{} dealt · {} dps",
                num(Some(total)),
                num(Some(f(enc, "your_dps")))
            )),
            false,
            rows,
        ))
    }

    /// `pollIncoming`: what is landing on you in this fight.
    fn incoming_panel(&self) -> Option<Div> {
        let d = self.incoming.as_ref()?;
        let total = f(d, "total");
        if total <= 0. {
            return None;
        }
        let rows = d
            .get("by_ability")
            .and_then(Value::as_array)
            .map(|a| a.iter().take(self.opts.rows).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|a| {
                bar_row(
                    div().child(s(a, "name").to_string()),
                    vec![
                        (
                            format!(
                                "{}× max {}",
                                num(Some(f(a, "hits"))),
                                num(Some(f(a, "max")))
                            ),
                            DIM,
                        ),
                        (num(Some(f(a, "total"))), TEXT),
                    ],
                    f(a, "share"),
                    Side::Enemy.bar(),
                    1.,
                )
                .into_any_element()
            })
            .collect();
        let worst = f(d, "biggest_hit");
        let meta = if worst > 0. {
            format!("{} taken · worst {}", num(Some(total)), num(Some(worst)))
        } else {
            format!("{} taken", num(Some(total)))
        };
        Some(panel("Hitting you".into(), Some(meta), false, rows))
    }

    /// `pollSky`: turn-ins that are ready, with who takes them and what to
    /// say. Hidden while none is.
    fn sky_panel(&self) -> Option<Div> {
        let d = self.sky.as_ref()?;
        let ready: Vec<&Value> = d
            .get("quests")
            .and_then(Value::as_array)?
            .iter()
            .filter(|q| flag(q, "relevant") && flag(q, "complete"))
            .collect();
        if ready.is_empty() {
            return None;
        }
        let meta = format!(
            "{} of {}",
            ready.len(),
            d.get("relevant_count").and_then(Value::as_u64).unwrap_or(0)
        );
        let mut rows = Vec::new();
        for q in ready.iter().take(self.opts.rows) {
            rows.push(
                bar_row(
                    div()
                        .text_color(gpui::rgb(YOU))
                        .child(s(q, "name").to_string()),
                    vec![(s(q, "giver").to_string(), TEXT)],
                    1.,
                    Side::You.bar(),
                    1.,
                )
                .into_any_element(),
            );
            rows.push(
                bar_row(
                    div()
                        .text_color(gpui::rgb(DIM))
                        .child(format!("→ {}", s(q, "reward"))),
                    vec![(format!("say \"{}\"", s(q, "phrase")), TEXT)],
                    0.,
                    Side::Plain.bar(),
                    1.,
                )
                .into_any_element(),
            );
        }
        Some(panel("Sky turn-ins ready".into(), Some(meta), false, rows))
    }

    /// `pollAppPanels`: what other apps put up through `/api/panels`, such
    /// as the eqldb collector's "worth a picture".
    fn app_panels(&self) -> Option<Div> {
        let panels: Vec<&Value> = self
            .apps
            .iter()
            .map(|p| p.get("panel").unwrap_or(p))
            .filter(|p| {
                p.get("rows")
                    .and_then(Value::as_array)
                    .is_some_and(|r| !r.is_empty())
            })
            .collect();
        if panels.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(u(6.))
                .children(panels.into_iter().map(|p| {
                    let mut rows = Vec::new();
                    for r in p
                        .get("rows")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let side = if flag(r, "highlight") {
                            Side::Enemy
                        } else {
                            Side::You
                        };
                        rows.push(
                            bar_row(
                                div().child(s(r, "name").to_string()),
                                vec![(s(r, "value").to_string(), TEXT)],
                                1.,
                                side.bar(),
                                1.,
                            )
                            .into_any_element(),
                        );
                        if !s(r, "note").is_empty() || !s(r, "note_value").is_empty() {
                            rows.push(
                                bar_row(
                                    div()
                                        .text_color(gpui::rgb(DIM))
                                        .child(s(r, "note").to_string()),
                                    vec![(s(r, "note_value").to_string(), TEXT)],
                                    0.,
                                    Side::Plain.bar(),
                                    1.,
                                )
                                .into_any_element(),
                            );
                        }
                    }
                    let meta = s(p, "meta");
                    panel(
                        s(p, "title").to_string(),
                        (!meta.is_empty()).then(|| meta.to_string()),
                        false,
                        rows,
                    )
                })),
        )
    }
}

/// `foldPets`: a creature's summoned pet folded into the creature, before
/// the row cap. Your own pets stay apart, and a charmed one is its own
/// creature.
fn fold_pets(rows: Vec<Value>) -> Vec<Value> {
    let mut out = rows;
    let names: Vec<String> = out.iter().map(|r| s(r, "name").to_string()).collect();
    let mut absorbed = vec![false; out.len()];
    for i in 0..out.len() {
        let r = out[i].clone();
        let owner = s(&r, "pet_owner");
        if owner.is_empty()
            || flag(&r, "is_you")
            || flag(&r, "is_pet")
            || s(&r, "pet_kind") != "summoned"
        {
            continue;
        }
        let Some(j) = names.iter().position(|n| n == owner).filter(|&j| j != i) else {
            continue;
        };
        let o = out[j].as_object_mut().expect("a meter row is an object");
        for k in ["damage", "dps", "share"] {
            let sum = o.get(k).and_then(Value::as_f64).unwrap_or(0.) + f(&r, k);
            o.insert(k.into(), sum.into());
        }
        let n = o.get("folded_pets").and_then(Value::as_u64).unwrap_or(0) + 1;
        o.insert("folded_pets".into(), n.into());
        absorbed[i] = true;
    }
    out.into_iter()
        .zip(absorbed)
        .filter(|(_, a)| !a)
        .map(|(r, _)| r)
        .collect()
}

/// A row's name and its tag: "+N pets", "charmed", "pet", or whose pet.
fn name_with_tag(r: &Value, side: Side) -> Div {
    let mut d = div().flex().items_center().gap_1().child(
        div()
            .text_color(gpui::rgb(side.name()))
            .child(s(r, "name").to_string()),
    );
    let folded = r.get("folded_pets").and_then(Value::as_u64).unwrap_or(0);
    let tag = if folded > 0 {
        Some(format!(
            "+{folded} pet{}",
            if folded == 1 { "" } else { "s" }
        ))
    } else {
        match s(r, "pet_kind") {
            "" => None,
            k => {
                let what = if k == "charmed" { "charmed" } else { "pet" };
                let owner = s(r, "pet_owner");
                Some(if !flag(r, "is_pet") && !owner.is_empty() {
                    format!("{what} {owner}")
                } else {
                    what.to_string()
                })
            }
        }
    };
    if let Some(t) = tag {
        d = d.child(
            div()
                .px(u(4.))
                .rounded(u(3.))
                .bg(rgba_of(PET, 0.25))
                .text_size(u(10.))
                .text_color(gpui::rgb(TEXT))
                .child(t),
        );
    }
    d
}

/// `renderSub`: zone, damage to enemies, hps, taken, deaths.
fn sub_chips(snap: &Value, enc: &Value) -> Div {
    let mut bits = Vec::new();
    if !s(snap, "zone").is_empty() {
        bits.push(s(snap, "zone").to_string());
    }
    if f(enc, "damage_to_enemies") > 0. {
        bits.push(format!(
            "to enemy {}",
            num(Some(f(enc, "damage_to_enemies")))
        ));
    }
    if f(enc, "your_hps") > 1. {
        bits.push(format!("{} hps", num(Some(f(enc, "your_hps")))));
    }
    if f(enc, "your_damage_taken") > 0. {
        bits.push(format!("taken {}", num(Some(f(enc, "your_damage_taken")))));
    }
    if f(enc, "your_deaths") > 0. {
        bits.push(format!("deaths {}", f(enc, "your_deaths")));
    }
    div()
        .flex()
        .flex_wrap()
        .gap_1()
        .children(bits.into_iter().map(|b| {
            div()
                .px(u(6.))
                .rounded(u(8.))
                .bg(rgba_of(WASH, 0.08))
                .text_size(u(10.))
                .text_color(gpui::rgb(DIM))
                .child(b)
        }))
}

/// `longDur`: hours and days past an hour, minutes for a coarse estimate.
pub fn long_dur(sec: f64, coarse: bool) -> String {
    let sec = sec.max(0.).round() as i64;
    if sec < 3600 {
        return if coarse {
            format!("{}m", (sec as f64 / 60.).round().max(1.))
        } else {
            dur(sec as f64)
        };
    }
    let h = sec / 3600;
    if h < 24 {
        let m = ((sec % 3600) as f64 / 60.).round() as i64;
        return if m > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{h}h")
        };
    }
    let (d, r) = (h / 24, h % 24);
    if r > 0 {
        format!("{d}d {r}h")
    } else {
        format!("{d}d")
    }
}

/// Keep the window as tall as what it shows (`fitOverlayToContent`): the
/// user picks where it sits and how wide it is, the panels decide how tall.
/// Never past the bottom of the screen, and with a dead band so a rounding
/// pixel does not resize it forever. Says whether the panels were cut, and
/// the height it wants.
fn fit_height(
    children: Vec<Bounds<Pixels>>,
    window: &mut Window,
    cx: &mut App,
) -> (bool, Option<f32>) {
    let Some(bottom) = children.iter().map(|b| b.bottom()).max() else {
        return (false, None);
    };
    let have = window.viewport_size();
    let room = window
        .display(cx)
        .map(|d| d.bounds().bottom() - window.bounds().top() - px(8.))
        .unwrap_or(px(2000.))
        .max(px(120.));
    let want = bottom.max(px(60.)).min(room).ceil();
    if (want - have.height).abs() > px(4.) {
        window.resize(size(have.width, want));
    }
    // The children are measured from the window's top.
    (bottom.max(px(60.)) > room + px(1.), Some(f32::from(want)))
}

/// The page's `body.clipped::after`: a panel that is simply absent reads as
/// a bug, so the bottom edge says the window is too small.
fn more_below() -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .px(u(8.))
        .py(u(2.))
        .flex()
        .justify_end()
        .bg(rgba_of(SCRIM, 0.92))
        .text_size(u(10.))
        .text_color(gpui::rgb(DIM))
        .child("more below — fewer panels, or a taller window")
}

// ---- pieces ------------------------------------------------------------------

/// A panel: title and meta over its rows. A title in the idle style when the
/// fight has ended.
fn panel(title: String, meta: Option<String>, idle: bool, rows: Vec<AnyElement>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(u(3.))
        .p(u(6.))
        .rounded(u(6.))
        .bg(rgba_of(SCRIM, 0.74))
        .border_1()
        .border_color(rgba_of(WASH, 0.1))
        .child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(gpui::rgb(if idle { DIM } else { TEXT }))
                        .child(title),
                )
                .children(
                    meta.map(|m| div().text_size(u(11.)).text_color(gpui::rgb(DIM)).child(m)),
                ),
        )
        .children(rows)
}

/// A bar row: the share as a filled background, the name, and its numbers.
fn bar_row(name: Div, nums: Vec<(String, u32)>, share: f64, bar: Rgba, opacity: f64) -> Div {
    div()
        .relative()
        .h(u(20.))
        .rounded(u(3.))
        .bg(rgba_of(WASH, 0.05))
        .opacity(opacity as f32)
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .rounded(u(3.))
                .w(relative(share.clamp(0., 1.) as f32))
                .bg(bar),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .px(u(6.))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(name.overflow_hidden().whitespace_nowrap())
                .child(
                    div().flex().gap(u(8.)).children(
                        nums.into_iter()
                            .map(|(t, c)| div().text_color(gpui::rgb(c)).child(t)),
                    ),
                ),
        )
}

/// A buff or debuff: the time left as the bar, "∞" for a permanent one, and
/// a "?" on a name the resolver is not sure of.
fn timer_row(b: &Value, left: f64, share: f64, bar: Rgba, time: u32) -> Div {
    let unsure = b
        .get("confidence")
        .and_then(Value::as_f64)
        .is_some_and(|c| c < 0.8);
    let name = format!("{}{}", s(b, "name"), if unsure { " ?" } else { "" });
    // Past the time the spell file gives it, and still taken to be on:
    // how long past, dimmed, rather than a timer stuck at zero.
    let overrun = b.get("overrun").and_then(Value::as_f64);
    let (t, colour) = if flag(b, "permanent") {
        ("∞".to_string(), time)
    } else if let Some(o) = overrun {
        (format!("+{}", long_dur(o, true)), DIM)
    } else {
        (dur(left), time)
    };
    bar_row(div().child(name), vec![(t, colour)], share, bar, 1.)
}

fn empty(text: &str) -> AnyElement {
    div()
        .px(u(6.))
        .text_size(u(12.))
        .text_color(gpui::rgb(DIM))
        .child(text.to_string())
        .into_any_element()
}

/// The daemon is gone, and the window is trying it again.
const RECONNECTING: &str = "Reconnecting…";
/// The daemon is there but still reading the log's backlog (`live: false`),
/// so the numbers are not current yet. Not "Reconnecting…": nothing is lost,
/// and a long log can take a minute to read.
const READING: &str = "Reading the log…";

/// A line saying why the panels are not current, or nothing when they are.
fn status(text: Option<&'static str>) -> Div {
    let d = div();
    let Some(text) = text else {
        return d;
    };
    d.px(u(6.))
        .py(u(2.))
        .rounded(u(4.))
        .bg(rgba_of(SCRIM, 0.74))
        .text_size(u(11.))
        .text_color(gpui::rgb(ACCENT))
        .child(text)
}

// ---- the window ------------------------------------------------------------

/// Open an overlay window. On Wayland it is a layer-shell surface on the
/// overlay layer, placed by margins from the top left of the output; failing
/// that, or under X11, an undecorated popup.
pub fn open(
    kind: Kind,
    api: Option<crate::api::Api>,
    cx: &mut App,
) -> Option<(AnyWindowHandle, Entity<OverlayView>)> {
    let mut saved = Saved::load(kind);
    let displays: Vec<(DisplayId, Bounds<Pixels>)> =
        cx.displays().iter().map(|d| (d.id(), d.bounds())).collect();
    let (display, g) = place(
        saved.bounds,
        &displays,
        cx.primary_display().map(|d| d.id()),
    );
    if g != saved.bounds {
        eprintln!(
            "[norrlog] overlay: saved position {},{} is on no display; moving to {},{}",
            saved.bounds.x, saved.bounds.y, g.x, g.y
        );
        saved.bounds = g;
        let _ = saved.save(kind);
    }
    // A layer surface is placed on its output, by margins from that
    // output's corner; a popup by the desktop's coordinates.
    let origin = displays
        .iter()
        .find(|d| Some(d.0) == display)
        .map(|d| d.1.origin)
        .unwrap_or_default();
    let margin = (f32::from(origin.x), f32::from(origin.y));
    eprintln!(
        "[norrlog] overlay at {},{} {}x{}, on display {:?} at {},{} (displays: {})",
        g.x,
        g.y,
        g.width,
        g.height,
        display,
        margin.0,
        margin.1,
        displays
            .iter()
            .map(|(id, b)| format!(
                "{id:?} {},{} {}x{}",
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width),
                f32::from(b.size.height)
            ))
            .collect::<Vec<_>>()
            .join("; ")
    );
    let click = saved.click_through;
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some() && cfg!(target_os = "linux");
    let mut attempts = Vec::new();
    if wayland {
        attempts.push(true);
    }
    attempts.push(false);
    for layer in attempts {
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(g.x), px(g.y)),
                size(px(g.width), px(g.height)),
            ))),
            titlebar: None,
            focus: false,
            show: true,
            kind: window_kind(
                kind,
                layer,
                Geometry {
                    x: g.x - margin.0,
                    y: g.y - margin.1,
                    ..g
                },
            ),
            display_id: display,
            is_movable: false,
            is_resizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some("norrlog-overlay".into()),
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        };
        let saved = saved.clone();
        let api = api.clone();
        let mut view = None;
        let opened = cx.open_window(opts, |window, cx| {
            set_click_through(window, click);
            let v = cx.new(|cx| {
                let mut o = OverlayView::new(saved, cx);
                o.api = api;
                o
            });
            view = Some(v.clone());
            v
        });
        match (opened, view) {
            (Ok(h), Some(v)) => return Some((h.into(), v)),
            (Err(e), _) => eprintln!(
                "[norrlog] overlay window ({}): {e}",
                if layer { "layer shell" } else { "popup" }
            ),
            _ => {}
        }
    }
    None
}

/// Where an overlay goes: where it was saved if a useful part of it is on
/// some display, or else just inside the fallback display
/// (`visibleOverlayBounds`). Monitors can be arranged with gaps, and a
/// window placed in one is created and never seen.
fn place(
    g: Geometry,
    displays: &[(DisplayId, Bounds<Pixels>)],
    fallback: Option<DisplayId>,
) -> (Option<DisplayId>, Geometry) {
    const MIN_VISIBLE: f32 = 40.;
    let overlap = |d: &Bounds<Pixels>| {
        let (dx, dy, dw, dh) = (
            f32::from(d.origin.x),
            f32::from(d.origin.y),
            f32::from(d.size.width),
            f32::from(d.size.height),
        );
        let w = (g.x + g.width).min(dx + dw) - g.x.max(dx);
        let h = (g.y + g.height).min(dy + dh) - g.y.max(dy);
        (w >= MIN_VISIBLE && h >= MIN_VISIBLE).then_some(w * h)
    };
    // The display holding most of it.
    if let Some((id, _)) = displays
        .iter()
        .filter_map(|(id, b)| Some((*id, overlap(b)?)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
    {
        return (Some(id), g);
    }
    let Some((id, d)) = displays
        .iter()
        .find(|d| Some(d.0) == fallback)
        .or(displays.first())
    else {
        // Nothing to check against; trust it.
        return (None, g);
    };
    let (dx, dy, dw, dh) = (
        f32::from(d.origin.x),
        f32::from(d.origin.y),
        f32::from(d.size.width),
        f32::from(d.size.height),
    );
    (
        Some(*id),
        Geometry {
            x: dx + 40.,
            y: dy + 40.,
            width: g.width.min(dw),
            height: g.height.min(dh),
        },
    )
}

#[cfg(target_os = "linux")]
fn window_kind(kind: Kind, layer: bool, g: Geometry) -> WindowKind {
    if !layer {
        return WindowKind::PopUp;
    }
    use gpui::layer_shell::*;
    WindowKind::LayerShell(LayerShellOptions {
        namespace: match kind {
            Kind::Main => "norrlog-overlay".into(),
            Kind::Charm => "norrlog-charm".into(),
        },
        layer: Layer::Overlay,
        anchor: Anchor::TOP | Anchor::LEFT,
        margin: Some((px(g.y), px(0.), px(0.), px(g.x))),
        keyboard_interactivity: KeyboardInteractivity::None,
        ..Default::default()
    })
}

#[cfg(not(target_os = "linux"))]
fn window_kind(_kind: Kind, _layer: bool, _g: Geometry) -> WindowKind {
    WindowKind::PopUp
}

/// Let clicks through to the game, or take them.
///
/// Wayland has an input region for this. GPUI's is implemented only there,
/// so under X11 the window's input shape is emptied directly, which is what
/// Electron's `setIgnoreMouseEvents` does underneath.
pub fn set_click_through(window: &mut Window, on: bool) {
    #[cfg(target_os = "linux")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Wayland(_)) => {
                window.set_input_region(if on { Some(&[]) } else { None })
            }
            Ok(RawWindowHandle::Xlib(h)) => x11_click_through(h.window as u32, on),
            Ok(RawWindowHandle::Xcb(h)) => x11_click_through(h.window.get(), on),
            _ => {}
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, on);
    }
}

#[cfg(target_os = "linux")]
fn x11_click_through(id: u32, on: bool) {
    use x11rb::connection::Connection;
    use x11rb::protocol::shape::{self, ConnectionExt as _};
    use x11rb::protocol::xproto::{ClipOrdering, Rectangle};
    let r = (|| -> Result<(), String> {
        let (conn, _) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let rects: &[Rectangle] = if on {
            &[]
        } else {
            &[Rectangle {
                x: 0,
                y: 0,
                width: u16::MAX,
                height: u16::MAX,
            }]
        };
        conn.shape_rectangles(
            shape::SO::SET,
            shape::SK::INPUT,
            ClipOrdering::UNSORTED,
            id,
            0,
            0,
            rects,
        )
        .map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())
    })();
    if let Err(e) = r {
        eprintln!("[norrlog] overlay click-through: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The glob import of `gpui` brings its own `test` attribute.
    use core::prelude::v1::test;

    #[test]
    fn options_read_as_the_page_reads_them() {
        let o = Options::parse("panels=meter,xp,motes,wanted&rows=5&buffs=5");
        assert_eq!(o.panels, ["meter", "xp", "motes", "apps"]);
        assert_eq!((o.rows, o.buffs), (5, 5));
        assert!(o.show_enemies && o.show_crit && !o.self_only && !o.keep_idle);
        assert_eq!(o.scope, None);

        let d = Options::parse("");
        assert_eq!(d.panels, ["meter", "buffs"], "absent is the default");
        assert_eq!((d.rows, d.buffs, d.scale), (6, 8, 1.));

        assert!(
            Options::parse("panels=").panels.is_empty(),
            "empty is no panels, not the default"
        );
        let e = Options::parse("enemies=0&crit=0&self=1&idle=1&scope=zone&panels=meter%2Cbuffs");
        assert!(!e.show_enemies && !e.show_crit && e.self_only && e.keep_idle);
        assert_eq!(e.scope.as_deref(), Some("zone"));
        assert_eq!(e.panels, ["meter", "buffs"]);
        assert_eq!(Options::parse("scope=week").scope, None);
        assert_eq!(
            decode("a%2Cb+c%é%"),
            "a,b c%é%",
            "a stray % is kept, and never panics"
        );
    }

    #[test]
    fn a_creatures_summoned_pet_folds_into_it_and_yours_stays() {
        let rows = vec![
            serde_json::json!({"name": "King", "damage": 100.0, "dps": 10.0, "share": 0.5, "is_enemy": true}),
            serde_json::json!({"name": "King`s pet", "damage": 40.0, "dps": 4.0, "share": 0.2, "is_enemy": true, "pet_owner": "King", "pet_kind": "summoned"}),
            serde_json::json!({"name": "Tharn", "damage": 50.0, "dps": 5.0, "share": 0.25, "is_you": true}),
            serde_json::json!({"name": "Xabarn", "damage": 10.0, "dps": 1.0, "share": 0.05, "is_pet": true, "pet_owner": "Tharn", "pet_kind": "summoned"}),
        ];
        let out = fold_pets(rows);
        assert_eq!(out.len(), 3);
        assert_eq!(f(&out[0], "damage"), 140.);
        assert_eq!(out[0]["folded_pets"], 1);
        assert!(
            out.iter().any(|r| s(r, "name") == "Xabarn"),
            "your own pet is left alone"
        );
    }

    #[test]
    fn dps_goes_in_the_second_the_log_reported_it_and_a_new_fight_starts_over() {
        let mut d = Dps::default();
        let enc = |id: u64, dmg: f64| serde_json::json!({"id": id, "your_damage": dmg});
        d.on_snapshot(Some(100), Some(&enc(1, 50.)));
        d.on_snapshot(Some(100), Some(&enc(1, 80.)));
        d.on_snapshot(Some(100), Some(&enc(1, 100.)));
        d.on_snapshot(Some(102), Some(&enc(1, 160.)));
        let b = d.buckets(102);
        assert_eq!(b.len(), 60);
        // The first snapshot only sets the baseline; two more in one second
        // add into one bucket; the idle second between is a zero.
        assert_eq!(&b[57..], &[50., 0., 60.]);
        d.on_snapshot(Some(200), Some(&enc(2, 10.)));
        assert!(
            d.buckets(200).iter().all(|v| *v == 0.),
            "the last fight's burst is not carried over"
        );
    }

    #[test]
    fn an_overlay_in_a_gap_between_monitors_is_brought_back() {
        let screen = |x: f32, w: f32| Bounds::new(point(px(x), px(0.)), size(px(w), px(1080.)));
        // Two monitors with a gap between them, at 1920..2560.
        let ds = [
            (DisplayId::new(1), screen(0., 1920.)),
            (DisplayId::new(2), screen(2560., 1920.)),
        ];
        let g = |x: f32, y: f32| Geometry {
            x,
            y,
            width: 380.,
            height: 460.,
        };
        assert_eq!(
            place(g(100., 100.), &ds, Some(ds[0].0)),
            (Some(ds[0].0), g(100., 100.))
        );
        assert_eq!(
            place(g(3000., 100.), &ds, Some(ds[0].0)),
            (Some(ds[1].0), g(3000., 100.)),
            "on the second, kept"
        );
        assert_eq!(
            place(g(2000., 100.), &ds, Some(ds[0].0)),
            (Some(ds[0].0), g(40., 40.)),
            "in the gap"
        );
        assert_eq!(
            place(g(1900., 100.), &ds, None).1,
            g(40., 40.),
            "20 px showing is not enough to find it by"
        );
        assert_eq!(place(g(2000., 100.), &[], None), (None, g(2000., 100.)));
    }

    #[test]
    fn long_spans_coarsen() {
        assert_eq!(long_dur(90., true), "2m");
        assert_eq!(long_dur(3600. * 2. + 60. * 5., false), "2h 5m");
        assert_eq!(long_dur(3600. * 50., false), "2d 2h");
    }

    #[test]
    fn electrons_saved_layout_is_read() {
        let o = Options::parse("panels=charm&rows=5");
        assert_eq!(o.panels, ["charm"]);
        assert_eq!(Kind::Charm.default_bounds().width, 300.);
    }
}
