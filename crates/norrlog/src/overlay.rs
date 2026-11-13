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
//! The windows read and write the Electron app's keys (`overlayOptions`,
//! `overlayBounds`, `overlayClickThrough`, and `overlay2…` for the charm
//! overlay), so a layout made there carries over. The options are the page's
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
            Kind::Main => Geometry { x: 40., y: 40., width: 380., height: 460. },
            Kind::Charm => Geometry { x: 460., y: 40., width: 300., height: 220. },
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
            options: cfg.get(kind.options_key()).and_then(Value::as_str).unwrap_or(kind.default_options()).to_string(),
            bounds: cfg
                .get(kind.bounds_key())
                .and_then(|b| serde_json::from_value::<Geometry>(b.clone()).ok())
                .filter(|g| g.width >= 40. && g.height >= 40.)
                .unwrap_or(kind.default_bounds()),
            click_through: cfg.get(kind.click_key()).and_then(Value::as_bool).unwrap_or(true),
        }
    }

    pub fn save(&self, kind: Kind) -> std::io::Result<()> {
        let mut patch = serde_json::Map::new();
        patch.insert(kind.options_key().into(), self.options.clone().into());
        patch.insert(kind.bounds_key().into(), serde_json::to_value(self.bounds).map_err(std::io::Error::other)?);
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
        for pair in query.trim_start_matches('?').split('&').filter(|p| !p.is_empty()) {
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
            .map(|s| if s == "wanted" { "apps".to_string() } else { s.to_string() })
            .collect();
        Options {
            panels,
            rows: int("rows", 6),
            buffs: int("buffs", 8),
            scale: get.get("scale").and_then(|v| v.parse().ok()).filter(|s: &f32| *s > 0.).unwrap_or(1.),
            self_only: get.get("self").is_some_and(|v| v == "1"),
            show_enemies: get.get("enemies").is_none_or(|v| v != "0"),
            show_crit: get.get("crit").is_none_or(|v| v != "0"),
            scope: get.get("scope").filter(|v| *v == "session" || *v == "zone").cloned(),
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
            && let Some(n) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
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

// ---- the view --------------------------------------------------------------

pub struct OverlayView {
    pub kind: Kind,
    pub saved: Saved,
    opts: Options,
    snap: Option<Arc<Value>>,
    /// When the snapshot arrived, so buff timers count down between pushes,
    /// as the page's one-second tick does.
    at: Instant,
    /// The daemon is gone; the page shows "Reconnecting…".
    lost: bool,
    /// `/api/charm`, polled while the charm panel is shown.
    charm: Option<Value>,
    api: Option<crate::api::Api>,
    _tick: Task<()>,
}

/// How long a meter row stays at full strength once it goes quiet, how long
/// it then takes to fade, and how far (`STALE_*` in the page).
const STALE_AFTER: f64 = 8.;
const STALE_FADE: f64 = 22.;
const STALE_FLOOR: f64 = 0.34;

impl OverlayView {
    pub fn new(kind: Kind, saved: Saved, cx: &mut Context<Self>) -> Self {
        let opts = Options::parse(&saved.options);
        // The page's one-second tick for buff timers, and the charm poll.
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let Ok(()) = this.update(cx, |v, cx| {
                v.poll_charm(cx);
                cx.notify();
            }) else {
                break;
            };
        });
        OverlayView { kind, saved, opts, snap: None, at: Instant::now(), lost: false, charm: None, api: None, _tick: tick }
    }

    pub fn set_api(&mut self, api: Option<crate::api::Api>, cx: &mut Context<Self>) {
        self.lost = api.is_none();
        self.api = api;
        cx.notify();
    }

    pub fn on_snapshot(&mut self, snap: Arc<Value>, cx: &mut Context<Self>) {
        self.snap = Some(snap);
        self.at = Instant::now();
        self.lost = false;
        cx.notify();
    }

    /// New options from Settings.
    pub fn set_options(&mut self, options: String, cx: &mut Context<Self>) {
        self.opts = Options::parse(&options);
        self.saved.options = options;
        cx.notify();
    }

    fn poll_charm(&mut self, cx: &mut Context<Self>) {
        if !self.opts.has("charm") {
            return;
        }
        let Some(api) = self.api.clone() else { return };
        cx.spawn(async move |this, cx| {
            // Fetch failures are silent, as in the page: the last answer stands.
            if let Ok(v) = api.get::<Value>("/api/charm".into()).await {
                let _ = this.update(cx, |me, cx| {
                    me.charm = Some(v);
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
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let o = &self.opts;
        let mut col = div().flex().flex_col().gap(px(6.)).w_full().text_color(gpui::rgb(TEXT)).text_size(px(13. * o.scale));
        let Some(snap) = self.snap.clone() else {
            // "Connecting…", with nothing else drawn yet.
            if o.has("meter") {
                col = col.child(panel("Connecting…".into(), None, true, vec![]));
            }
            return col.child(status(self.lost));
        };
        let snap = &*snap;
        // The page's fixed order, whatever order `panels=` names them in.
        for p in ["meter", "xp", "heals", "taken", "motes", "auras", "debuffs", "buffs", "charm"] {
            if !o.has(p) {
                continue;
            }
            let el = match p {
                "meter" => Some(self.meter(snap)),
                "xp" => self.xp(snap),
                "heals" => Some(self.metric(snap, "Healing", "healing", Some("hps"), "heal_share", "No healing yet")),
                "taken" => Some(self.metric(snap, "Damage taken", "damage_taken", None, "taken_share", "No damage taken")),
                "motes" => Some(self.motes(snap)),
                "auras" => Some(self.auras(snap)),
                "debuffs" => Some(self.debuffs(snap)),
                "buffs" => Some(self.buffs(snap)),
                "charm" => self.charm_panel(),
                _ => None,
            };
            if let Some(el) = el {
                col = col.child(el);
            }
        }
        let live = snap.get("live").and_then(Value::as_bool) != Some(false);
        col.child(status(self.lost || !live))
    }
}

impl OverlayView {
    /// The encounter the meter shows: the fight in progress, or the last one
    /// with `idle=1` (`render` in the page).
    fn encounter<'a>(&self, snap: &'a Value) -> Option<&'a Value> {
        snap.get("current").filter(|c| !c.is_null()).or_else(|| {
            self.opts.keep_idle.then(|| snap.get("recent").and_then(|r| r.get(0))).flatten()
        })
    }

    /// `render`'s meter half and `renderRows`, `renderSub`.
    fn meter(&self, snap: &Value) -> Div {
        let Some(enc) = self.encounter(snap) else {
            return panel("Waiting for combat".into(), None, true, vec![empty("No active encounter")]);
        };
        let idle = snap.get("current").is_none_or(Value::is_null);
        let meta = format!("{} dps · {}", num(Some(f(enc, "your_dps"))), dur(f(enc, "duration")));
        let mut rows: Vec<Value> = fold_pets(enc.get("meter").and_then(Value::as_array).cloned().unwrap_or_default());
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
                    let quiet = r.get("last_action").and_then(Value::as_f64).map_or(0., |la| (end - la).max(0.));
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
                    bar_row(name_with_tag(r, side), nums, f(r, "share"), side.bar(), fade).into_any_element()
                })
                .collect()
        };
        out.push(sub_chips(snap, enc).into_any_element());
        let title = s(enc, "title");
        panel(if title.is_empty() { "Encounter".into() } else { title.to_string() }, Some(meta), idle, out)
    }

    /// `renderMetric`: healing and damage taken.
    fn metric(&self, snap: &Value, title: &str, value: &str, rate: Option<&str>, share: &str, empty_text: &str) -> Div {
        let Some(enc) = self.encounter(snap) else {
            return panel(title.into(), None, false, vec![empty("—")]);
        };
        let mut rows: Vec<&Value> =
            enc.get("meter").and_then(Value::as_array).map(|a| a.iter().filter(|r| f(r, value) > 0.).collect()).unwrap_or_default();
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
                bar_row(div().text_color(gpui::rgb(side.name())).child(s(r, "name").to_string()), nums, f(r, share), side.bar(), 1.)
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
        let title = if levelling.is_empty() { format!("Level {lvl}") } else { format!("Level {lvl} ({levelling})") };
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
            let eta = x.get("seconds_to_next_level").and_then(Value::as_f64).map_or("—".to_string(), |s| long_dur(s, false));
            let kills = x.get("kills").and_then(Value::as_u64).unwrap_or(0);
            rows.push(
                bar_row(
                    div().text_color(gpui::rgb(YOU)).child(format!("{pct:.1}% → {}", lvl + 1)),
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
                vec![(format!("{} unspent", x.get("aa_unspent").and_then(Value::as_u64).unwrap_or(0)), DIM)],
                0.,
                Side::Plain.bar(),
                1.,
            )
            .into_any_element(),
        );
        if flag(x, "aa_potion_active") {
            let c = x.get("aa_potion_charges").and_then(Value::as_u64).unwrap_or(0);
            // Amber on the last charge, as a cue to drink another.
            let bar = if c <= 1 { rgba_of(ACCENT, 0.4) } else { Side::Pet.bar() };
            rows.push(
                bar_row(div().child("XP potion"), vec![(format!("{c} left"), DIM)], c.min(5) as f64 / 5., bar, 1.)
                    .into_any_element(),
            );
        }
        Some(panel(title, (!bits.is_empty()).then(|| bits.join(" · ")), false, rows))
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
            if hours >= 0.25 { format!("{total} this session · {:.1}/hr", total / hours) } else { format!("{total} this session") }
        });
        if rows.is_empty() {
            return panel("Motes".into(), meta, false, vec![empty("None yet")]);
        }
        let max = rows.iter().map(|r| r.1).fold(1., f64::max);
        let out = rows
            .iter()
            .map(|(name, n)| {
                let short = name.strip_prefix("Mote of ").or_else(|| name.strip_prefix("mote of ")).unwrap_or(name);
                bar_row(div().child(short.to_string()), vec![(format!("{n}"), TEXT)], n / max, Side::Plain.bar(), 1.)
                    .into_any_element()
            })
            .collect();
        panel("Motes".into(), meta, false, out)
    }

    /// `renderAuras`.
    fn auras(&self, snap: &Value) -> Div {
        let auras = snap.get("auras").and_then(Value::as_array).cloned().unwrap_or_default();
        if auras.is_empty() {
            return panel("Auras".into(), None, false, vec![empty("No auras")]);
        }
        let out = auras
            .iter()
            .take(5)
            .map(|a| bar_row(div().text_color(gpui::rgb(PET)).child(s(a, "name").to_string()), vec![("up".into(), DIM)], 1., Side::Pet.bar(), 1.).into_any_element())
            .collect();
        panel("Auras".into(), Some(format!("{}/5", auras.len())), false, out)
    }

    /// `renderDebuffs`.
    fn debuffs(&self, snap: &Value) -> Div {
        let list = self.timed(snap, "debuffs");
        if list.is_empty() {
            return panel("Debuffs".into(), None, false, vec![empty("None")]);
        }
        let out = list.into_iter().map(|(b, left, share)| timer_row(&b, left, share, rgba_of(ENEMY, 0.4), TEXT).into_any_element()).collect();
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
                let time = if flag(&b, "permanent") {
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
                        let left = if perm { rem } else { (rem - gone).max(0.) };
                        let total = b.get("total").and_then(Value::as_f64).filter(|t| *t > 0.).unwrap_or(1.);
                        let share = if perm { 1. } else { (left / total).clamp(0., 1.) };
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
        let held: Vec<&str> = c.get("charmed").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let fails: Vec<&Value> = c.get("failures").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
        if held.is_empty() && fails.is_empty() {
            return None;
        }
        let now = crate::format::now_log();
        let mut rows: Vec<AnyElement> = held
            .iter()
            .map(|n| bar_row(div().text_color(gpui::rgb(PET)).child(n.to_string()), vec![("charmed".into(), DIM)], 1., Side::Pet.bar(), 1.).into_any_element())
            .collect();
        for fl in fails.iter().take(self.opts.rows) {
            let ago = fl.get("at").and_then(Value::as_i64).map_or(0, |at| (now - at).max(0));
            rows.push(
                bar_row(
                    div().flex().gap_1().child(s(fl, "target").to_string()).child(div().text_color(gpui::rgb(ENEMY)).child(s(fl, "kind").to_string())),
                    vec![(format!("{ago}s ago"), DIM)],
                    0.,
                    Side::Plain.bar(),
                    1.,
                )
                .into_any_element(),
            );
        }
        let meta = if held.is_empty() {
            "no charm".to_string()
        } else {
            let breaks = c.get("breaks").and_then(Value::as_u64).unwrap_or(0);
            format!("{:.0}% uptime · {breaks} break{}", f(c, "uptime") * 100., if breaks == 1 { "" } else { "s" })
        };
        Some(panel("Charm".into(), Some(meta), held.is_empty(), rows))
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
        if owner.is_empty() || flag(&r, "is_you") || flag(&r, "is_pet") || s(&r, "pet_kind") != "summoned" {
            continue;
        }
        let Some(j) = names.iter().position(|n| n == owner).filter(|&j| j != i) else { continue };
        let o = out[j].as_object_mut().expect("a meter row is an object");
        for k in ["damage", "dps", "share"] {
            let sum = o.get(k).and_then(Value::as_f64).unwrap_or(0.) + f(&r, k);
            o.insert(k.into(), sum.into());
        }
        let n = o.get("folded_pets").and_then(Value::as_u64).unwrap_or(0) + 1;
        o.insert("folded_pets".into(), n.into());
        absorbed[i] = true;
    }
    out.into_iter().zip(absorbed).filter(|(_, a)| !a).map(|(r, _)| r).collect()
}

/// A row's name and its tag: "+N pets", "charmed", "pet", or whose pet.
fn name_with_tag(r: &Value, side: Side) -> Div {
    let mut d = div().flex().items_center().gap_1().child(div().text_color(gpui::rgb(side.name())).child(s(r, "name").to_string()));
    let folded = r.get("folded_pets").and_then(Value::as_u64).unwrap_or(0);
    let tag = if folded > 0 {
        Some(format!("+{folded} pet{}", if folded == 1 { "" } else { "s" }))
    } else {
        match s(r, "pet_kind") {
            "" => None,
            k => {
                let what = if k == "charmed" { "charmed" } else { "pet" };
                let owner = s(r, "pet_owner");
                Some(if !flag(r, "is_pet") && !owner.is_empty() { format!("{what} {owner}") } else { what.to_string() })
            }
        }
    };
    if let Some(t) = tag {
        d = d.child(div().px(px(4.)).rounded(px(3.)).bg(rgba_of(PET, 0.25)).text_size(px(10.)).text_color(gpui::rgb(TEXT)).child(t));
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
        bits.push(format!("to enemy {}", num(Some(f(enc, "damage_to_enemies")))));
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
    div().flex().flex_wrap().gap_1().children(bits.into_iter().map(|b| {
        div().px(px(6.)).rounded(px(8.)).bg(rgba_of(WASH, 0.08)).text_size(px(10.)).text_color(gpui::rgb(DIM)).child(b)
    }))
}

/// `longDur`: hours and days past an hour, minutes for a coarse estimate.
pub fn long_dur(sec: f64, coarse: bool) -> String {
    let sec = sec.max(0.).round() as i64;
    if sec < 3600 {
        return if coarse { format!("{}m", (sec as f64 / 60.).round().max(1.)) } else { dur(sec as f64) };
    }
    let h = sec / 3600;
    if h < 24 {
        let m = ((sec % 3600) as f64 / 60.).round() as i64;
        return if m > 0 { format!("{h}h {m}m") } else { format!("{h}h") };
    }
    let (d, r) = (h / 24, h % 24);
    if r > 0 { format!("{d}d {r}h") } else { format!("{d}d") }
}

// ---- pieces ------------------------------------------------------------------

/// A panel: title and meta over its rows. A title in the idle style when the
/// fight has ended.
fn panel(title: String, meta: Option<String>, idle: bool, rows: Vec<AnyElement>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(3.))
        .p(px(6.))
        .rounded(px(6.))
        .bg(rgba_of(SCRIM, 0.74))
        .border_1()
        .border_color(rgba_of(WASH, 0.1))
        .child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap_2()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(gpui::rgb(if idle { DIM } else { TEXT })).child(title))
                .children(meta.map(|m| div().text_size(px(11.)).text_color(gpui::rgb(DIM)).child(m))),
        )
        .children(rows)
}

/// A bar row: the share as a filled background, the name, and its numbers.
fn bar_row(name: Div, nums: Vec<(String, u32)>, share: f64, bar: Rgba, opacity: f64) -> Div {
    div()
        .relative()
        .h(px(20.))
        .rounded(px(3.))
        .bg(rgba_of(WASH, 0.05))
        .opacity(opacity as f32)
        .child(div().absolute().top_0().bottom_0().left_0().rounded(px(3.)).w(relative(share.clamp(0., 1.) as f32)).bg(bar))
        .child(
            div()
                .absolute()
                .inset_0()
                .px(px(6.))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(name.overflow_hidden().whitespace_nowrap())
                .child(div().flex().gap(px(8.)).children(nums.into_iter().map(|(t, c)| div().text_color(gpui::rgb(c)).child(t)))),
        )
}

/// A buff or debuff: the time left as the bar, "∞" for a permanent one, and
/// a "?" on a name the resolver is not sure of.
fn timer_row(b: &Value, left: f64, share: f64, bar: Rgba, time: u32) -> Div {
    let unsure = b.get("confidence").and_then(Value::as_f64).is_some_and(|c| c < 0.8);
    let name = format!("{}{}", s(b, "name"), if unsure { " ?" } else { "" });
    let t = if flag(b, "permanent") { "∞".to_string() } else { dur(left) };
    bar_row(div().child(name), vec![(t, time)], share, bar, 1.)
}

fn empty(text: &str) -> AnyElement {
    div().px(px(6.)).text_size(px(12.)).text_color(gpui::rgb(DIM)).child(text.to_string()).into_any_element()
}

/// "Reconnecting…", while the daemon is gone or still reading the backlog.
fn status(show: bool) -> Div {
    let d = div();
    if !show {
        return d;
    }
    d.px(px(6.)).py(px(2.)).rounded(px(4.)).bg(rgba_of(SCRIM, 0.74)).text_size(px(11.)).text_color(gpui::rgb(ACCENT)).child("Reconnecting…")
}

// ---- the window ------------------------------------------------------------

/// Open an overlay window. On Wayland it is a layer-shell surface on the
/// overlay layer, placed by margins from the top left of the output; failing
/// that, or under X11, an undecorated popup.
pub fn open(kind: Kind, api: Option<crate::api::Api>, cx: &mut App) -> Option<(AnyWindowHandle, Entity<OverlayView>)> {
    let saved = Saved::load(kind);
    let g = saved.bounds;
    let click = saved.click_through;
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some() && cfg!(target_os = "linux");
    let mut attempts = Vec::new();
    if wayland {
        attempts.push(true);
    }
    attempts.push(false);
    for layer in attempts {
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(g.x), px(g.y)), size(px(g.width), px(g.height))))),
            titlebar: None,
            focus: false,
            show: true,
            kind: window_kind(kind, layer, g),
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
                let mut o = OverlayView::new(kind, saved, cx);
                o.api = api;
                o
            });
            view = Some(v.clone());
            v
        });
        match (opened, view) {
            (Ok(h), Some(v)) => return Some((h.into(), v)),
            (Err(e), _) => eprintln!("[norrlog] overlay window ({}): {e}", if layer { "layer shell" } else { "popup" }),
            _ => {}
        }
    }
    None
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
            Ok(RawWindowHandle::Wayland(_)) => window.set_input_region(if on { Some(&[]) } else { None }),
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
        let rects: &[Rectangle] = if on { &[] } else { &[Rectangle { x: 0, y: 0, width: u16::MAX, height: u16::MAX }] };
        conn.shape_rectangles(shape::SO::SET, shape::SK::INPUT, ClipOrdering::UNSORTED, id, 0, 0, rects)
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

        assert!(Options::parse("panels=").panels.is_empty(), "empty is no panels, not the default");
        let e = Options::parse("enemies=0&crit=0&self=1&idle=1&scope=zone&panels=meter%2Cbuffs");
        assert!(!e.show_enemies && !e.show_crit && e.self_only && e.keep_idle);
        assert_eq!(e.scope.as_deref(), Some("zone"));
        assert_eq!(e.panels, ["meter", "buffs"]);
        assert_eq!(Options::parse("scope=week").scope, None);
        assert_eq!(decode("a%2Cb+c%é%"), "a,b c%é%", "a stray % is kept, and never panics");
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
        assert!(out.iter().any(|r| s(r, "name") == "Xabarn"), "your own pet is left alone");
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
