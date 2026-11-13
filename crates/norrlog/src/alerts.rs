//! Alerts: the rule list and editor, what went off, and the log feed rules
//! are built from (alerts.js). The checklist is the Alerts section of
//! the parity inventory.
//!
//! The daemon decides whether something happened and puts each fire on the
//! snapshot; this view plays every fire newer than the last one played,
//! whichever tab is open. On the first snapshot it only records where the
//! sequence stands, so connecting never replays a backlog.
//!
//! `/api/alerts` is fetched when the tab opens and after every save or
//! delete. The feed (`/api/lines?since=N`) is polled every 1.5 s while the
//! tab is on screen.

use crate::api::Api;
use crate::format::when_short;
use crate::parts::{ROW, ROW_YOU, bar_row, card_suffixed, dim, empty, fill, hint, note, split};
use crate::table::tag;
use crate::theme::px;
use crate::theme::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use norrlog_api::{
    AlertFire, AlertRule, AlertsReply, LinesReply, LogLine, SignalKind, Sound, Trigger,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Duration;

const FEED_POLL: Duration = Duration::from_millis(1500);
const FEED_MAX: usize = 400;
const FIRES_MAX: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Contains,
    Pattern,
    Signal,
    BuffExpiring,
}

const KINDS: &[(Kind, &str)] = &[
    (Kind::Contains, "the log line contains…"),
    (Kind::Pattern, "the log line matches a pattern…"),
    (Kind::Signal, "something happens…"),
    (Kind::BuffExpiring, "a buff is about to expire…"),
];

/// The editor's happenings, with the editor's labels.
const SIGNALS: &[(SignalKind, &str)] = &[
    (SignalKind::CharmBroken, "a charm broke"),
    (SignalKind::CharmResisted, "a charm failed to land"),
    (SignalKind::YouDied, "you died"),
    (SignalKind::BossKilled, "a raid target died"),
    (SignalKind::LevelUp, "you levelled"),
    (SignalKind::AaGained, "you gained AA"),
    (SignalKind::Loot, "you looted something"),
    (SignalKind::Tell, "someone sent you a tell"),
    (SignalKind::SpellResisted, "your spell was resisted"),
    (SignalKind::TaskUpdated, "a task advanced"),
    (SignalKind::Achievement, "an achievement landed"),
];

const SOUNDS: &[(Sound, &str)] = &[
    (Sound::Chime, "chime"),
    (Sound::Bell, "bell"),
    (Sound::Alarm, "alarm"),
    (Sound::Buzz, "buzz"),
    (Sound::Blip, "blip"),
    (Sound::Thud, "thud"),
    (Sound::None, "silent"),
];

/// The list's shorter labels (`SIGNAL_LABELS`).
fn signal_label(k: SignalKind) -> &'static str {
    match k {
        SignalKind::CharmBroken => "a charm broke",
        SignalKind::CharmResisted => "a charm failed",
        SignalKind::YouDied => "you died",
        SignalKind::BossKilled => "a raid target died",
        SignalKind::LevelUp => "you levelled",
        SignalKind::AaGained => "you gained AA",
        SignalKind::Loot => "you looted something",
        SignalKind::Tell => "someone tells you",
        SignalKind::SpellResisted => "your spell resisted",
        SignalKind::TaskUpdated => "a task advanced",
        SignalKind::Achievement => "an achievement",
        SignalKind::BuffExpiring => "buff_expiring",
    }
}

fn sound_name(s: Sound) -> &'static str {
    match s {
        Sound::None => "none",
        s => SOUNDS.iter().find(|x| x.0 == s).map_or("", |x| x.1),
    }
}

/// Mute and volume, this app's own: `~/.config/norrlog/alerts.json`.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct Prefs {
    mute: bool,
    volume: u32,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            mute: false,
            volume: 70,
        }
    }
}

impl Prefs {
    fn path() -> Option<std::path::PathBuf> {
        crate::config::config_home().map(|d| d.join("norrlog/alerts.json"))
    }

    fn load() -> Self {
        let Some(p) = Self::path() else {
            return Prefs::default();
        };
        Self::load_from(&p, crate::config::path().as_deref())
    }

    /// This app's file if there is one; else, once, the Electron app's
    /// `alertMute` and `alertVolume`, saved as this app's own, so an install
    /// that was muted does not start ringing after the move.
    ///
    /// A file that is there but unreadable is not "no preference": the
    /// Electron values are used if there are any, and otherwise alerts stay
    /// silent, and the file is left for the user to see rather than
    /// overwritten.
    fn load_from(path: &std::path::Path, legacy: Option<&std::path::Path>) -> Self {
        let old = || -> Option<Prefs> {
            let v: serde_json::Value =
                serde_json::from_slice(&std::fs::read(legacy?).ok()?).ok()?;
            let mute = v.get("alertMute").and_then(serde_json::Value::as_bool);
            let volume = v.get("alertVolume").and_then(serde_json::Value::as_f64);
            if mute.is_none() && volume.is_none() {
                return None;
            }
            let d = Prefs::default();
            Some(Prefs {
                mute: mute.unwrap_or(d.mute),
                volume: volume.map_or(d.volume, |v| v.clamp(0., 100.).round() as u32),
            })
        };
        match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                eprintln!("[norrlog] {} is unreadable ({e}); alerts are silent until it is fixed or removed", path.display());
                old().unwrap_or(Prefs { mute: true, ..Prefs::default() })
            }),
            Err(_) => match old() {
                Some(p) => {
                    p.save_to(path);
                    p
                }
                None => Prefs::default(),
            },
        }
    }

    fn save(&self) {
        if let Some(p) = Self::path() {
            self.save_to(&p);
        }
    }

    fn save_to(&self, p: &std::path::Path) {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = p.with_extension("json.tmp");
        if let Ok(b) = serde_json::to_vec_pretty(self)
            && std::fs::write(&tmp, b).is_ok()
        {
            let _ = std::fs::rename(&tmp, p);
        }
    }

    /// The playing gain: nothing when muted.
    fn gain(&self) -> f32 {
        if self.mute {
            0.
        } else {
            self.volume.min(100) as f32 / 100.
        }
    }
}

struct Inputs {
    name: Entity<InputState>,
    text: Entity<InputState>,
    pattern: Entity<InputState>,
    seconds: Entity<InputState>,
    subject: Entity<InputState>,
    cooldown: Entity<InputState>,
    say: Entity<InputState>,
}

pub struct AlertsView {
    api: Option<Api>,
    visible: bool,
    rules: Vec<AlertRule>,
    loaded: bool,
    status: String,
    /// The rule in the editor, by id; `None` for a new one.
    editing: Option<String>,
    /// The title suffix: blank at start, `— new`, or why a rule is broken.
    editing_label: String,
    kind: Kind,
    signal: SignalKind,
    sound: Sound,
    speak: bool,
    inputs: Inputs,
    prefs: Prefs,
    volume: Entity<SliderState>,
    /// Newest first; this session only.
    fires: Vec<AlertFire>,
    /// How far the fires and the feed have been taken, in this daemon's
    /// numbering.
    cursors: Cursors,
    /// Newest first.
    feed: VecDeque<LogLine>,
    feed_live: bool,
    voices: Option<bool>,
    notes: [bool; 2],
    tasks: Vec<Task<()>>,
    _poll: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

fn input(
    window: &mut Window,
    cx: &mut Context<AlertsView>,
    placeholder: &str,
    value: &str,
) -> Entity<InputState> {
    let (p, v) = (placeholder.to_string(), value.to_string());
    cx.new(|cx| InputState::new(window, cx).placeholder(p).default_value(v))
}

impl AlertsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prefs = Prefs::load();
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(100.)
                .step(1.)
                .default_value(prefs.volume as f32)
        });
        // Saved on release, not while dragging.
        let subs = vec![
            cx.subscribe(&volume, |v: &mut Self, _, ev: &SliderEvent, cx| {
                if let SliderEvent::Release(val) = ev {
                    v.prefs.volume = val.start().round().clamp(0., 100.) as u32;
                    v.prefs.save();
                    cx.notify();
                }
            }),
        ];
        let inputs = Inputs {
            name: input(window, cx, "Charm broke", ""),
            text: input(window, cx, "Your charm spell has worn off", ""),
            pattern: input(window, cx, r"hits YOU for (\d{3,})", ""),
            seconds: input(window, cx, "", "30"),
            subject: input(window, cx, "any", ""),
            cooldown: input(window, cx, "", "10"),
            say: input(window, cx, "words to say (defaults to the name)", ""),
        };
        // Whether speech can be heard, asked once and off the UI thread.
        let voices = cx.spawn(async move |this, cx| {
            let ok = cx
                .background_spawn(async { crate::sound::voices_available() })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.voices = Some(ok);
                cx.notify();
            });
        });
        AlertsView {
            api: None,
            visible: false,
            rules: Vec::new(),
            loaded: false,
            status: String::new(),
            editing: None,
            editing_label: String::new(),
            kind: Kind::Contains,
            signal: SignalKind::CharmBroken,
            sound: Sound::Chime,
            speak: false,
            inputs,
            prefs,
            volume,
            fires: Vec::new(),
            cursors: Cursors::default(),
            feed: VecDeque::new(),
            feed_live: true,
            voices: None,
            notes: [false; 2],
            tasks: vec![voices],
            _poll: None,
            _subs: subs,
        }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        // A different daemon counts from one again; what it says is news even
        // where its numbers are below the old one's.
        if let Some(a) = &api
            && self.cursors.attach(a.client.hello().pid)
        {
            self.feed.clear();
        }
        self.api = api;
        self._poll = self.api.as_ref().map(|_| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(FEED_POLL).await;
                    if this.update(cx, |v, cx| v.poll_feed(cx)).is_err() {
                        break;
                    }
                }
            })
        });
        cx.notify();
    }

    /// The tab opened or closed. Opening it fetches the rules and the feed.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        let opened = visible && !self.visible;
        self.visible = visible;
        if opened {
            self.load(cx);
            self.poll_feed(cx);
        }
    }

    fn track(&mut self, t: Task<()>) {
        self.tasks.push(t);
        if self.tasks.len() > 8 {
            self.tasks.drain(..self.tasks.len() - 8);
        }
    }

    pub fn load(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.track(cx.spawn(async move |this, cx| {
            let got = api.get::<AlertsReply>("/api/alerts".to_string()).await;
            let _ = this.update(cx, |v, cx| {
                match got {
                    Ok(r) => {
                        v.rules = r.rules;
                        v.loaded = true;
                        v.status = if r.persisted {
                            String::new()
                        } else {
                            "no database — alerts will be lost when the app closes".into()
                        };
                    }
                    Err(_) => v.status = "daemon not reachable".into(),
                }
                cx.notify();
            });
        }));
    }

    fn poll_feed(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        let Some(api) = self.api.clone() else { return };
        let since = self.cursors.line_seq;
        let asked = self.cursors.daemon;
        self.track(cx.spawn(async move |this, cx| {
            let Ok(r) = api
                .get::<LinesReply>(format!("/api/lines?since={since}"))
                .await
            else {
                return;
            };
            let _ = this.update(cx, |v, cx| {
                // An answer from a daemon since replaced is not this one's.
                if v.cursors.daemon != asked {
                    return;
                }
                let was = v.feed_live;
                v.feed_live = r.live;
                if let Some(last) = r.lines.last() {
                    v.cursors.line_seq = v.cursors.line_seq.max(last.seq);
                    for l in r.lines {
                        v.feed.push_front(l);
                    }
                    v.feed.truncate(FEED_MAX);
                    cx.notify();
                } else if was != v.feed_live {
                    cx.notify();
                }
            });
        }));
    }

    /// A snapshot arrived, whichever tab is open: play what is new.
    pub fn on_fires(&mut self, fires: Vec<AlertFire>, seq: u64, cx: &mut Context<Self>) {
        let fresh = self.cursors.fresh(fires, seq);
        if !fresh.is_empty() {
            let gain = self.prefs.gain();
            for f in &fresh {
                crate::sound::play(f.sound, gain);
                if let Some(say) = &f.say {
                    // A failure is on stderr; an alert firing is no moment
                    // for a dialog.
                    let _ = crate::sound::speak(say, gain);
                }
            }
            for f in fresh {
                self.fires.insert(0, f);
            }
            self.fires.truncate(FIRES_MAX);
            cx.notify();
        }
    }

    fn value(&self, i: &Entity<InputState>, cx: &App) -> String {
        i.read(cx).value().to_string()
    }

    fn set(&self, i: &Entity<InputState>, v: &str, window: &mut Window, cx: &mut Context<Self>) {
        let v = v.to_string();
        i.update(cx, |s, cx| s.set_value(v, window, cx));
    }

    fn blank(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.editing_label = "— new".into();
        for (i, v) in [
            (&self.inputs.name, ""),
            (&self.inputs.text, ""),
            (&self.inputs.pattern, ""),
            (&self.inputs.seconds, "30"),
            (&self.inputs.subject, ""),
            (&self.inputs.cooldown, "10"),
            (&self.inputs.say, ""),
        ] {
            self.set(i, v, window, cx);
        }
        self.kind = Kind::Contains;
        self.signal = SignalKind::CharmBroken;
        self.sound = Sound::Chime;
        self.speak = false;
        cx.notify();
    }

    fn edit(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.rules.iter().find(|r| r.id == id).cloned() else {
            return;
        };
        self.editing = Some(r.id.clone());
        self.editing_label = match &r.error {
            Some(e) => format!("— will never fire: {}", e.lines().next().unwrap_or("")),
            None => String::new(),
        };
        let (kind, text, pattern, signal, seconds, subject) = match &r.trigger {
            Trigger::Contains { text } => (
                Kind::Contains,
                text.clone(),
                String::new(),
                SignalKind::CharmBroken,
                30,
                None,
            ),
            Trigger::Pattern { pattern } => (
                Kind::Pattern,
                String::new(),
                pattern.clone(),
                SignalKind::CharmBroken,
                30,
                None,
            ),
            Trigger::Signal { kind, name } => (
                Kind::Signal,
                String::new(),
                String::new(),
                *kind,
                30,
                name.clone(),
            ),
            Trigger::BuffExpiring { seconds, name } => (
                Kind::BuffExpiring,
                String::new(),
                String::new(),
                SignalKind::CharmBroken,
                *seconds,
                name.clone(),
            ),
        };
        self.kind = kind;
        self.signal = signal;
        self.sound = r.sound;
        self.speak = r.speak;
        let (n, t, p, sec, sub, cd, say) = (
            r.name.clone(),
            text,
            pattern,
            seconds.to_string(),
            subject.unwrap_or_default(),
            r.cooldown.to_string(),
            r.say.clone().unwrap_or_default(),
        );
        self.set(&self.inputs.name.clone(), &n, window, cx);
        self.set(&self.inputs.text.clone(), &t, window, cx);
        self.set(&self.inputs.pattern.clone(), &p, window, cx);
        self.set(&self.inputs.seconds.clone(), &sec, window, cx);
        self.set(&self.inputs.subject.clone(), &sub, window, cx);
        self.set(&self.inputs.cooldown.clone(), &cd, window, cx);
        self.set(&self.inputs.say.clone(), &say, window, cx);
        cx.notify();
    }

    /// Start a `contains` rule from a line the player just watched go past.
    fn from_line(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.blank(window, cx);
        self.kind = Kind::Contains;
        self.set(&self.inputs.text.clone(), &text, window, cx);
        self.set(&self.inputs.name.clone(), &line_name(&text), window, cx);
        // Focused, with the whole line selected to trim down.
        self.inputs.text.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
    }

    fn trigger(&self, cx: &App) -> Trigger {
        let name =
            Some(self.value(&self.inputs.subject, cx).trim().to_string()).filter(|s| !s.is_empty());
        match self.kind {
            Kind::Contains => Trigger::Contains {
                text: self.value(&self.inputs.text, cx),
            },
            Kind::Pattern => Trigger::Pattern {
                pattern: self.value(&self.inputs.pattern, cx),
            },
            Kind::Signal => Trigger::Signal {
                kind: self.signal,
                name,
            },
            Kind::BuffExpiring => Trigger::BuffExpiring {
                seconds: parse_int(&self.value(&self.inputs.seconds, cx))
                    .filter(|&n| n != 0)
                    .unwrap_or(30)
                    .max(1),
                name,
            },
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.value(&self.inputs.name, cx).trim().to_string();
        if name.is_empty() {
            self.status = "an alert needs a name".into();
            cx.notify();
            return;
        }
        let trigger = self.trigger(cx);
        match &trigger {
            Trigger::Contains { text } if text.trim().is_empty() => {
                self.status = "an alert needs some text to look for".into();
                cx.notify();
                return;
            }
            Trigger::Pattern { pattern } if pattern.trim().is_empty() => {
                self.status = "an alert needs a pattern".into();
                cx.notify();
                return;
            }
            _ => {}
        }
        // Minted here and never reused, so a renamed rule keeps its place.
        let id = self.editing.clone().unwrap_or_else(new_id);
        let enabled = self
            .rules
            .iter()
            .find(|r| r.id == id)
            .is_none_or(|r| r.enabled);
        let rule = AlertRule {
            id: id.clone(),
            name,
            enabled,
            trigger,
            sound: self.sound,
            speak: self.speak,
            say: Some(self.value(&self.inputs.say, cx).trim().to_string())
                .filter(|s| !s.is_empty()),
            cooldown: parse_int(&self.value(&self.inputs.cooldown, cx))
                .unwrap_or(0)
                .max(0),
            error: None,
        };
        self.put(rule, Some(id), window, cx);
    }

    /// POST a rule, reload, and (when `then_edit`) open it in the editor.
    fn put(
        &mut self,
        rule: AlertRule,
        then_edit: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(api) = self.api.clone() else { return };
        let body = serde_json::to_value(&rule).unwrap_or_default();
        self.track(cx.spawn_in(window, async move |this, cx| {
            let got = api.post_json("/api/alerts".to_string(), body).await;
            let reloaded = match &got {
                Ok(()) => api.get::<AlertsReply>("/api/alerts".to_string()).await.ok(),
                Err(_) => None,
            };
            let _ = this.update_in(cx, |v, window, cx| {
                match got {
                    Err(e) => v.status = e,
                    Ok(()) => {
                        v.status.clear();
                        if let Some(r) = reloaded {
                            v.rules = r.rules;
                            if !r.persisted {
                                v.status =
                                    "no database — alerts will be lost when the app closes".into();
                            }
                        }
                        if let Some(id) = then_edit {
                            v.edit(&id, window, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.editing.clone() else {
            self.blank(window, cx);
            return;
        };
        let Some(api) = self.api.clone() else { return };
        self.blank(window, cx);
        self.track(cx.spawn(async move |this, cx| {
            let _ = api
                .post_json(
                    "/api/alerts/delete".to_string(),
                    serde_json::json!({ "id": id }),
                )
                .await;
            let _ = this.update(cx, |v, cx| v.load(cx));
        }));
    }

    /// Deliberately audible even when muted: you pressed the button. A
    /// voice that could not be started is said in the tab.
    fn hear(&mut self, cx: &mut Context<Self>) {
        let gain = self.prefs.gain().max(0.2);
        crate::sound::play(self.sound, gain);
        if self.speak {
            let say = self.value(&self.inputs.say, cx);
            let text = if say.is_empty() {
                self.value(&self.inputs.name, cx)
            } else {
                say
            };
            if let Err(e) = crate::sound::speak(&text, gain) {
                self.status = format!("speech: {e}");
                cx.notify();
            }
        }
    }
}

const RULES_NOTE: &str = "Sounds play in this window, so the app has to be running — but it does not have to be in \
    front. Alerts stay silent while the log is being read at startup; only what happens after that makes a noise.";
const FEED_NOTE: &str = "The last few hundred lines, newest first. Click one to build an alert from it — the text is \
    filled in for you, and you trim it down to the part that matters. Where a typed happening covers the same \
    ground, prefer it: \"a charm broke\" survives wording the client varies, a substring does not.";
const NO_VOICES: &str = "No speech voices are installed, so spoken alerts will be silent. Sounds still work. On \
    Linux, install speech-dispatcher and a voice such as espeak-ng.";

impl Render for AlertsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toolbar = h_flex()
            .gap_3()
            .flex_wrap()
            .child(
                Button::new("al-new")
                    .label("New alert")
                    .small()
                    .on_click(cx.listener(|v, _, window, cx| v.blank(window, cx))),
            )
            .child(
                Checkbox::new("al-mute")
                    .label("Mute everything")
                    .checked(self.prefs.mute)
                    .on_click(cx.listener(|v, checked: &bool, _, cx| {
                        v.prefs.mute = *checked;
                        v.prefs.save();
                        cx.notify();
                    })),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child("Volume")
                    .child(div().w(px(110.)).child(Slider::new(&self.volume))),
            )
            .child(dim(self.status.clone()));
        let count = (!self.rules.is_empty()).then(|| format!("— {}", self.rules.len()));
        let fired = (!self.fires.is_empty()).then(|| format!("— {}", self.fires.len()));
        let left = card_suffixed("Alerts", count)
            .child(self.render_rules(cx))
            .child(note(
                "al-note",
                self.notes[0],
                RULES_NOTE,
                cx.listener(|v, _, _, cx| {
                    v.notes[0] = !v.notes[0];
                    cx.notify();
                }),
            ))
            .child(
                h_flex()
                    .mt_2()
                    .gap_1()
                    .items_baseline()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(hex(MUTED))
                            .child("WENT OFF"),
                    )
                    .when_some(fired, |d, f| d.child(dim(f).text_sm())),
            )
            .child(if self.fires.is_empty() {
                empty("Nothing yet this session")
            } else {
                v_flex().gap_1().children(self.fires.iter().map(|f| {
                    bar_row(
                        0.,
                        fill(ROW),
                        h_flex()
                            .gap_2()
                            .child(f.name.clone())
                            .child(dim(f.detail.clone())),
                        dim(when_short(Some(f.at))),
                    )
                }))
            });
        let label = (!self.editing_label.is_empty()).then(|| self.editing_label.clone());
        let right = card_suffixed("Alert", label).child(self.render_form(cx));
        let feed_state = (!self.feed_live)
            .then(|| "— still reading the log; lines appear once it is following live".to_string());
        let feed = card_suffixed("Log", feed_state)
            .child(self.render_feed(cx))
            .child(note(
                "al-feed-note",
                self.notes[1],
                FEED_NOTE,
                cx.listener(|v, _, _, cx| {
                    v.notes[1] = !v.notes[1];
                    cx.notify();
                }),
            ));
        div()
            .id("alerts-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .gap_3()
                    .child(toolbar)
                    .child(split(left, right))
                    .child(feed),
            )
    }
}

impl AlertsView {
    fn render_rules(&self, cx: &mut Context<Self>) -> Div {
        if !self.loaded {
            return div();
        }
        if self.rules.is_empty() {
            return empty("No alerts yet. Click a log line below to make one.");
        }
        v_flex()
            .gap_1()
            .children(self.rules.iter().enumerate().map(|(i, r)| {
                let on = self.editing.as_deref() == Some(r.id.as_str());
                let (id, id2, rule) = (r.id.clone(), r.id.clone(), r.clone());
                let name = h_flex()
                    .gap_2()
                    .child(
                        Checkbox::new(SharedString::from(format!("al-en-{i}")))
                            .checked(r.enabled)
                            .on_click(cx.listener(move |v, checked: &bool, window, cx| {
                                cx.stop_propagation();
                                let mut r = rule.clone();
                                r.enabled = *checked;
                                let _ = &id2;
                                v.put(r, None, window, cx);
                            })),
                    )
                    .child(r.name.clone())
                    .when(r.error.is_some(), |d| {
                        d.child(tag("broken", ENEMY).flex_none())
                    })
                    .when(r.sound == Sound::None && !r.speak, |d| {
                        d.child(tag("silent", PET).flex_none())
                    })
                    .child(
                        dim(summary(&r.trigger))
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis(),
                    );
                div()
                    .id(SharedString::from(format!("al-rule-{i}")))
                    .cursor_pointer()
                    .when(!r.enabled, |d| d.opacity(0.5))
                    .child(bar_row(
                        if on { 1. } else { 0. },
                        fill(if on { ROW_YOU } else { ROW }),
                        name,
                        dim(if r.speak {
                            "spoken"
                        } else {
                            sound_name(r.sound)
                        }),
                    ))
                    .on_click(cx.listener(move |v, _, window, cx| v.edit(&id, window, cx)))
            }))
    }

    fn render_form(&self, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let field = |label: &str, el: AnyElement| {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(hex(MUTED))
                        .child(label.to_string()),
                )
                .child(el)
        };
        let kind = self.kind;
        let w = this.clone();
        let kind_menu = Button::new("al-kind")
            .label(KINDS.iter().find(|k| k.0 == kind).map_or("", |k| k.1))
            .small()
            .w_full()
            .dropdown_menu(move |mut menu, _, _| {
                for &(k, label) in KINDS {
                    let w = w.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(kind == k).on_click(
                        move |_, _, cx| {
                            let _ = w.update(cx, |v, cx| {
                                v.kind = k;
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });
        let signal = self.signal;
        let w = this.clone();
        let signal_menu = Button::new("al-signal")
            .label(SIGNALS.iter().find(|s| s.0 == signal).map_or("", |s| s.1))
            .small()
            .w_full()
            .dropdown_menu(move |mut menu, _, _| {
                for &(s, label) in SIGNALS {
                    let w = w.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(signal == s).on_click(
                        move |_, _, cx| {
                            let _ = w.update(cx, |v, cx| {
                                v.signal = s;
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });
        let sound = self.sound;
        let w = this.clone();
        let sound_menu = Button::new("al-sound")
            .label(format!(
                "Sound: {}",
                SOUNDS.iter().find(|s| s.0 == sound).map_or("", |s| s.1)
            ))
            .small()
            .dropdown_menu(move |mut menu, _, _| {
                for &(s, label) in SOUNDS {
                    let w = w.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(sound == s).on_click(
                        move |_, _, cx| {
                            let _ = w.update(cx, |v, cx| {
                                v.sound = s;
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });
        let named = matches!(kind, Kind::Signal | Kind::BuffExpiring);
        v_flex()
            .gap_3()
            .child(field("Name (this is also what gets spoken)", Input::new(&self.inputs.name).small().into_any_element()))
            .child(field("Fire when", kind_menu.into_any_element()))
            .when(kind == Kind::Contains, |d| {
                d.child(
                    field("Text", Input::new(&self.inputs.text).small().into_any_element())
                        .child(hint("Case-insensitive. Any part of the line.")),
                )
            })
            .when(kind == Kind::Pattern, |d| {
                d.child(
                    field("Pattern (regular expression)", Input::new(&self.inputs.pattern).small().into_any_element())
                        .child(hint("Also case-insensitive. A pattern that will not compile is reported rather than ignored.")),
                )
            })
            .when(kind == Kind::Signal, |d| d.child(field("Happening", signal_menu.into_any_element())))
            .when(kind == Kind::BuffExpiring, |d| {
                d.child(field("Seconds left", div().w(px(90.)).child(Input::new(&self.inputs.seconds).small()).into_any_element()).child(hint(
                    "Warns once as the timer crosses this mark, and again only if the buff is recast. Auras and \
                     permanent effects are left alone: neither has a countdown worth watching.",
                )))
            })
            .when(named, |d| {
                d.child(
                    field("Only when it names (optional)", Input::new(&self.inputs.subject).small().into_any_element())
                        .child(hint("Part of the creature, item, buff or person's name.")),
                )
            })
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(sound_menu)
                    .child(Button::new("al-test").label("Hear it").small().on_click(cx.listener(|v, _, _, cx| v.hear(cx))))
                    .child(
                        h_flex()
                            .gap_2()
                            .child("Repeat after")
                            // Expiry warnings are edge-triggered: a cooldown would only confuse.
                            .child(div().w(px(70.)).child(Input::new(&self.inputs.cooldown).small().disabled(kind == Kind::BuffExpiring)))
                            .child("s"),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(Checkbox::new("al-speak").label("Say it out loud").checked(self.speak).on_click(cx.listener(
                        |v, checked: &bool, _, cx| {
                            v.speak = *checked;
                            cx.notify();
                        },
                    )))
                    .child(Input::new(&self.inputs.say).small())
                    .when(self.voices == Some(false), |d| d.child(hint(NO_VOICES))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("al-save").label("Save").small().primary().on_click(cx.listener(|v, _, window, cx| v.save(window, cx))))
                    .child(Button::new("al-delete").label("Delete").small().on_click(cx.listener(|v, _, window, cx| v.delete(window, cx)))),
            )
    }

    fn render_feed(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.feed.is_empty() {
            return div().into_any_element();
        }
        let mono = cx.theme().mono_font_family.clone();
        div()
            .id("al-feed")
            .max_h(px(352.))
            .overflow_y_scrollbar()
            .font_family(mono)
            .text_sm()
            .child(
                v_flex()
                    .gap(px(2.))
                    .children(self.feed.iter().enumerate().map(|(i, l)| {
                        let text = l.text.clone();
                        h_flex()
                            .id(SharedString::from(format!("al-line-{i}")))
                            .justify_between()
                            .gap_3()
                            .px(px(7.))
                            .py(px(3.))
                            .rounded(px(4.))
                            .bg(hex(PANEL_2))
                            .cursor_pointer()
                            .hover(|d| d.bg(wash(ACCENT, 0.12)))
                            .child(div().min_w_0().child(l.text.clone()))
                            .child(dim(when_short(Some(l.at))).flex_none())
                            .on_click(cx.listener(move |v, _, window, cx| {
                                v.from_line(text.clone(), window, cx)
                            }))
                    })),
            )
            .into_any_element()
    }
}

/// The rule list's one-line account of a trigger.
fn summary(t: &Trigger) -> String {
    match t {
        Trigger::Contains { text } => format!("line contains “{text}”"),
        Trigger::Pattern { pattern } => format!("line matches /{pattern}/"),
        Trigger::Signal { kind, name } => match name {
            Some(n) => format!("{} · {n}", signal_label(*kind)),
            None => signal_label(*kind).to_string(),
        },
        Trigger::BuffExpiring { seconds, name } => {
            format!("{} under {seconds}s", name.as_deref().unwrap_or("any buff"))
        }
    }
}

/// A first guess at a name for a rule made from a line: the line itself,
/// or its first 40 characters trimmed and an ellipsis.
fn line_name(text: &str) -> String {
    if text.chars().count() > 40 {
        format!("{}…", text.chars().take(40).collect::<String>().trim())
    } else {
        text.to_string()
    }
}

/// JavaScript's `parseInt(s, 10)`: leading whitespace, a sign, digits.
fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim_start();
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: i64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// `a` and the time in base 36, as the renderer mints ids.
fn new_id() -> String {
    let mut n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut out = Vec::new();
    while n > 0 {
        out.push(std::char::from_digit((n % 36) as u32, 36).unwrap());
        n /= 36;
    }
    out.reverse();
    format!("a{}", out.into_iter().collect::<String>())
}

/// How far the fires and the log feed have been taken.
///
/// Both are numbered by the daemon from one, so the numbers mean something
/// only for the daemon that gave them. A daemon restarted (a log switch, a
/// port change, a crash) starts again from one, and a cursor carried over
/// would hide its first hundred alerts behind an old one's hundred.
#[derive(Default)]
struct Cursors {
    /// The daemon the numbers are from, by process id.
    daemon: Option<u32>,
    /// The highest fire played. `None` until the first snapshot.
    alert_seq: Option<u64>,
    line_seq: u64,
}

impl Cursors {
    /// Attached to `pid`. True when it is a different daemon, and the cursors
    /// were reset; a reconnect to the same one keeps them.
    fn attach(&mut self, pid: u32) -> bool {
        if self.daemon == Some(pid) {
            return false;
        }
        *self = Cursors {
            daemon: Some(pid),
            ..Default::default()
        };
        true
    }

    /// The fires not yet played. The first snapshot from a daemon only sets
    /// the baseline: what fired before the app was looking is not played.
    fn fresh(&mut self, fires: Vec<AlertFire>, seq: u64) -> Vec<AlertFire> {
        let Some(played) = self.alert_seq else {
            self.alert_seq = Some(seq);
            return Vec::new();
        };
        self.alert_seq = Some(played.max(seq));
        fires.into_iter().filter(|f| f.seq > played).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursors, line_name, new_id, parse_int, summary};

    #[test]
    fn electrons_mute_and_volume_carry_over_once() {
        use super::Prefs;
        let d = std::env::temp_dir().join(format!("norrlog-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (new, old) = (d.join("alerts.json"), d.join("config.json"));
        let load = || Prefs::load_from(&new, Some(&old));

        assert!(
            !load().mute && load().volume == 70,
            "neither file: the defaults"
        );
        std::fs::write(&old, r#"{"alertMute":true,"alertVolume":12}"#).unwrap();
        let p = load();
        assert!(p.mute && p.volume == 12);
        assert!(new.exists(), "saved as this app's own");
        std::fs::write(&old, r#"{"alertMute":false,"alertVolume":0}"#).unwrap();
        assert!(load().mute, "this app's own file wins once there is one");
        std::fs::remove_file(&new).unwrap();
        assert_eq!(load().gain(), 0., "volume 0 is silence");
        std::fs::remove_file(&new).unwrap();
        std::fs::write(&old, r#"{"alertVolume":250}"#).unwrap();
        assert_eq!(load().volume, 100, "clamped");
        std::fs::write(&new, "{not json").unwrap();
        std::fs::write(&old, "{}").unwrap();
        assert!(
            load().mute,
            "an unreadable file is not taken as permission to ring"
        );
        assert_eq!(
            std::fs::read_to_string(&new).unwrap(),
            "{not json",
            "and is not overwritten"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    fn fire(seq: u64) -> norrlog_api::AlertFire {
        serde_json::from_value(serde_json::json!({
            "seq": seq, "at": 0, "rule_id": "r", "name": "n", "detail": "", "sound": "chime"
        }))
        .unwrap()
    }

    #[test]
    fn a_new_daemons_alerts_play_though_its_numbers_start_again() {
        let mut c = Cursors::default();
        assert!(c.attach(10));
        assert!(
            c.fresh(vec![fire(100)], 100).is_empty(),
            "the first snapshot is the baseline"
        );
        assert_eq!(c.fresh(vec![fire(100), fire(101)], 101).len(), 1);
        c.line_seq = 5_000_000;
        assert!(
            !c.attach(10),
            "a reconnect to the same daemon keeps its place"
        );
        assert!(
            c.fresh(vec![fire(101)], 101).is_empty(),
            "and does not replay"
        );
        assert!(c.attach(11), "a restarted daemon");
        assert_eq!(c.line_seq, 0, "the feed starts from its first line");
        assert!(c.fresh(vec![], 0).is_empty());
        assert_eq!(c.fresh(vec![fire(1)], 1).len(), 1, "its first alert plays");
        assert!(c.fresh(vec![fire(1)], 1).is_empty(), "once");
    }
    use norrlog_api::{SignalKind, Trigger};

    #[test]
    fn summaries_names_and_numbers_read_as_the_renderer_writes_them() {
        assert_eq!(
            summary(&Trigger::Contains {
                text: "worn off".into()
            }),
            "line contains “worn off”"
        );
        assert_eq!(
            summary(&Trigger::Signal {
                kind: SignalKind::Tell,
                name: Some("Bob".into())
            }),
            "someone tells you · Bob"
        );
        assert_eq!(
            summary(&Trigger::BuffExpiring {
                seconds: 30,
                name: None
            }),
            "any buff under 30s"
        );
        assert_eq!(line_name("short"), "short");
        assert_eq!(
            line_name("Your charm spell has worn off, and it was a very long line"),
            "Your charm spell has worn off, and it wa…"
        );
        assert_eq!(parse_int(" 12s"), Some(12));
        assert_eq!(parse_int("-3"), Some(-3));
        assert_eq!(parse_int("x"), None);
        assert!(new_id().starts_with('a') && new_id().len() > 5);
    }
}
