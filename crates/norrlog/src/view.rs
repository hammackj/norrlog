//! The shell: header, navigation and the frame each tab draws into.
//!
//! Tabs arrive one port card at a time. Until a tab is ported it says so,
//! and names the file in `docs/parity/` that lists what it has to do.

use crate::daemon::Msg;
use crate::format::num;
use crate::state::{Rect, State};
use crate::theme::*;
use eqlparser_api::Hello;
use gpui::*;
use gpui_component::v_flex;
use serde_json::Value;
use std::sync::Arc;

/// Nav order, as the Electron app has it: (id, label, parity file).
pub const TABS: &[(&str, &str, &str)] = &[
    ("dashboard", "Dashboard", "shell-and-dashboard.md"),
    ("encounters", "Encounters", "encounters-and-procs.md"),
    ("loot", "Loot", "loot-and-tooltips.md"),
    ("experience", "Experience", "experience-character-classes.md"),
    ("procs", "Procs", "encounters-and-procs.md"),
    ("tradeskills", "Tradeskills", "tradeskills-and-forage.md"),
    ("forage", "Forage", "tradeskills-and-forage.md"),
    ("sky", "Sky", "sky-and-quests.md"),
    ("quests", "Quests", "sky-and-quests.md"),
    ("tells", "Tells", "tells-alerts-settings.md"),
    ("alerts", "Alerts", "tells-alerts-settings.md"),
    ("character", "Character", "experience-character-classes.md"),
    ("classes", "Classes", "experience-character-classes.md"),
    ("settings", "Settings", "tells-alerts-settings.md"),
];

enum Link {
    Attaching,
    Attached { hello: Box<Hello>, started: bool },
    Lost(String),
}

pub struct Shell {
    link: Link,
    rt: tokio::runtime::Handle,
    encounters: Entity<crate::encounters::EncountersView>,
    procs: Entity<crate::procs::ProcsView>,
    tab: &'static str,
    snapshot: Option<Arc<Value>>,
    state: State,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl Shell {
    pub fn new(
        rx: async_channel::Receiver<Msg>,
        rt: tokio::runtime::Handle,
        state: State,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                if this.update_in(cx, |v, _, cx| v.on_msg(msg, cx)).is_err() {
                    break;
                }
            }
        });
        // Where the window is, remembered as it moves rather than on close:
        // a window killed with the session never gets to say goodbye.
        let subs = vec![cx.observe_window_bounds(window, |v, window, _| v.remember_bounds(window))];
        let tab = TABS.iter().find(|t| t.0 == state.tab).map(|t| t.0).unwrap_or("dashboard");
        let encounters = cx.new(|cx| crate::encounters::EncountersView::new(window, cx));
        let procs = cx.new(|_| crate::procs::ProcsView::new());
        Shell { link: Link::Attaching, rt, encounters, procs, tab, snapshot: None, state, _tasks: vec![task], _subs: subs }
    }

    fn on_msg(&mut self, msg: Msg, cx: &mut Context<Self>) {
        match msg {
            Msg::Attaching => {
                if !matches!(self.link, Link::Lost(_)) {
                    self.link = Link::Attaching;
                }
            }
            Msg::Attached { hello, started, client } => {
                self.link = Link::Attached { hello, started };
                let api = crate::api::Api { rt: self.rt.clone(), client: *client };
                let open = self.tab;
                self.encounters.update(cx, |v, cx| {
                    v.set_api(Some(api.clone()), cx);
                    if open == "encounters" {
                        v.load(false, cx);
                    }
                });
                self.procs.update(cx, |v, cx| {
                    v.set_api(Some(api), cx);
                    if open == "procs" {
                        v.load(cx);
                    }
                });
            }
            Msg::Snapshot(s) => {
                // The id of the newest *closed* fight: `totals.encounters`
                // counts the one in progress too, and so moves at the wrong
                // moment.
                let newest = s.get("recent").and_then(|r| r.get(0)).and_then(|e| e.get("id")).and_then(Value::as_u64);
                let visible = self.tab == "encounters";
                self.encounters.update(cx, |v, cx| v.newest_closed(newest, visible, cx));
                let procs_open = self.tab == "procs";
                self.procs.update(cx, |v, cx| v.on_snapshot(procs_open, cx));
                self.snapshot = Some(s);
            }
            Msg::Lost(why) => {
                eprintln!("[norrlog] lost the daemon: {why}");
                self.link = Link::Lost(why);
                self.snapshot = None;
                self.encounters.update(cx, |v, cx| v.set_api(None, cx));
                self.procs.update(cx, |v, cx| v.set_api(None, cx));
            }
        }
        cx.notify();
    }

    fn remember_bounds(&mut self, window: &Window) {
        let (b, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(b) => (b, false),
            WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
        };
        let r = Rect { x: b.origin.x.into(), y: b.origin.y.into(), w: b.size.width.into(), h: b.size.height.into() };
        if self.state.bounds != Some(r) || self.state.maximized != maximized {
            self.state.bounds = Some(r);
            self.state.maximized = maximized;
            self.state.save();
        }
    }

    fn select(&mut self, tab: &'static str, cx: &mut Context<Self>) {
        self.tab = tab;
        // As the renderer does: opening a tab fetches it afresh.
        if tab == "encounters" {
            self.encounters.update(cx, |v, cx| v.load(false, cx));
        }
        if tab == "procs" {
            self.procs.update(cx, |v, cx| v.load(cx));
        }
        self.state.tab = tab.to_string();
        self.state.save();
        cx.notify();
    }

    fn render_header(&self) -> Div {
        let s = self.snapshot.as_deref();
        let mut pills = div().flex().flex_row().items_center().gap_2().ml_auto();
        let str_of = |k: &str| s.and_then(|s| s.get(k)).and_then(Value::as_str).unwrap_or("");
        let character = match (str_of("player"), s.and_then(|s| s.get("level")).and_then(Value::as_i64)) {
            ("", _) => "—".to_string(),
            (p, Some(l)) => format!("{p} · lvl {l}"),
            (p, None) => p.to_string(),
        };
        pills = pills.child(pill(character, TEXT, BORDER));
        let classes: Vec<&str> = s
            .and_then(|s| s.get("classes"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|c| c.get("abbrev").and_then(Value::as_str)).collect())
            .unwrap_or_default();
        if !classes.is_empty() {
            pills = pills.child(pill(classes.join("/"), YOU, YOU));
        }
        if !str_of("zone").is_empty() {
            pills = pills.child(pill(str_of("zone").to_string(), TEXT, BORDER));
        }
        let (status, color) = match (&self.link, s.and_then(|s| s.get("live")).and_then(Value::as_bool)) {
            (Link::Attached { .. }, Some(true)) => ("following live", HEAL),
            (Link::Attached { .. }, Some(false)) => ("parsing…", MUTED),
            (Link::Attached { .. }, None) | (Link::Attaching, _) => ("connecting", ACCENT),
            (Link::Lost(_), _) => ("offline", ENEMY),
        };
        pills = pills.child(pill(status, color, color));

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_4()
            .py(px(8.))
            .bg(hex(PANEL))
            .border_b_1()
            .border_color(hex(BORDER))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_2()
                    .child(div().font_weight(FontWeight::BOLD).child("Norrlog"))
                    .child(div().text_xs().text_color(hex(MUTED)).child(match &self.link {
                        Link::Attached { hello, started } => format!(
                            "eqlparserd {} · port {}{}",
                            hello.version,
                            hello.port,
                            if *started { " · started here" } else { "" }
                        ),
                        _ => String::new(),
                    })),
            )
            .child(pills)
    }

    fn render_nav(&self, cx: &mut Context<Self>) -> Div {
        let mut nav = div().flex().flex_row().flex_wrap().px_3().bg(hex(PANEL)).border_b_1().border_color(hex(BORDER));
        for &(id, label, _) in TABS {
            let on = self.tab == id;
            nav = nav.child(
                div()
                    .id(SharedString::from(format!("tab-{id}")))
                    .px(px(12.))
                    .py(px(8.))
                    .text_sm()
                    .cursor_pointer()
                    .border_b_2()
                    .border_color(if on { hex(ACCENT) } else { wash(ACCENT, 0.) })
                    .text_color(hex(if on { ACCENT } else { MUTED }))
                    .hover(|s| s.text_color(hex(if on { ACCENT } else { TEXT })))
                    .child(label)
                    .on_click(cx.listener(move |v, _, _, cx| v.select(id, cx))),
            );
        }
        nav
    }

    fn render_body(&self) -> AnyElement {
        if let Link::Lost(why) = &self.link {
            return notice("The daemon is not answering", why.clone()).into_any_element();
        }
        match self.tab {
            "dashboard" => self.render_totals().into_any_element(),
            "encounters" => self.encounters.clone().into_any_element(),
            "procs" => self.procs.clone().into_any_element(),
            id => {
                let (_, label, file) = TABS.iter().find(|t| t.0 == id).copied().unwrap_or(TABS[0]);
                notice(
                    &format!("{label} is not ported yet"),
                    format!("What it has to do is listed in docs/parity/{file}. The Electron app still has it."),
                )
                .into_any_element()
            }
        }
    }

    /// The Dashboard's totals strip: enough of the dashboard to show the
    /// snapshot stream driving the window. The rest comes with its port.
    fn render_totals(&self) -> Div {
        let t = self.snapshot.as_deref().and_then(|s| s.get("totals"));
        let n = |k: &str| t.and_then(|t| t.get(k)).and_then(Value::as_f64);
        let pp = |k: &str| {
            let c = n(k).unwrap_or(0.0) / 1000.0;
            crate::format::num(Some(c.floor()))
        };
        let stat = |label: &'static str, value: String, sub: Option<String>| {
            let mut v = v_flex()
                .gap_1()
                .p_3()
                .min_w(px(130.))
                .rounded(px(8.))
                .border_1()
                .border_color(hex(BORDER))
                .bg(hex(PANEL))
                .child(div().text_xs().text_color(hex(MUTED)).child(label.to_uppercase()))
                .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(value));
            if let Some(sub) = sub {
                v = v.child(div().text_xs().text_color(hex(MUTED)).child(sub));
            }
            v
        };
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_3()
            .child(stat("Your damage", num(n("your_damage")), None))
            .child(stat("Kills", num(n("kills")), None))
            .child(stat("Deaths", num(n("deaths")), None))
            .child(stat("Encounters", num(n("encounters")), None))
            .child(stat("Healing done", num(n("your_healing")), None))
            .child(stat("Damage taken", num(n("your_damage_taken")), None))
            .child(stat(
                "Platinum earned",
                pp("coin_total_copper"),
                Some(format!("{} looted · {} sold", pp("coin_copper"), pp("coin_from_sales_copper"))),
            ))
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(hex(BG))
            .text_color(hex(TEXT))
            .text_sm()
            .child(self.render_header())
            .child(self.render_nav(cx))
            .child(if self.tab == "encounters" && !matches!(self.link, Link::Lost(_)) {
                // Scrolls its own table, so its toolbar stays put.
                div().id("main").flex_1().min_h_0().p_4().child(self.render_body())
            } else {
                div().id("main").flex_1().min_h_0().overflow_y_scroll().own_axis().p_4().child(self.render_body())
            })
    }
}

fn pill(text: impl Into<SharedString>, color: u32, border: u32) -> Div {
    div()
        .px(px(9.))
        .py(px(1.))
        .rounded_full()
        .bg(hex(PANEL_2))
        .border_1()
        .border_color(wash(border, if border == BORDER { 1.0 } else { 0.35 }))
        .text_xs()
        .text_color(hex(color))
        .child(text.into())
}

/// A panel with a title and a sentence: the parser's `.empty`, with reason.
fn notice(title: &str, body: String) -> Div {
    v_flex()
        .gap_2()
        .p_4()
        .rounded(px(8.))
        .border_1()
        .border_color(hex(BORDER))
        .bg(hex(PANEL))
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
        .child(div().text_color(hex(MUTED)).child(body))
}
