//! The shell: header, navigation and the frame each tab draws into.
//!
//! Tabs arrive one port card at a time. Until a tab is ported it says so,
//! and names the file in `docs/parity/` that lists what it has to do.

use crate::daemon::Msg;
use crate::state::{Rect, State};
use crate::theme::*;
use eqlparser_api::Hello;
use gpui::*;
use gpui_component::scroll::ScrollableElement as _;
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
    loot: Entity<crate::loot::LootView>,
    classes: Entity<crate::classes::ClassesView>,
    character: Entity<crate::character::CharacterView>,
    experience: Entity<crate::experience::ExperienceView>,
    tradeskills: Entity<crate::tradeskills::TradeskillsView>,
    forage: Entity<crate::forage::ForageView>,
    sky: Entity<crate::sky::SkyView>,
    tells: Entity<crate::tells::TellsView>,
    alerts: Entity<crate::alerts::AlertsView>,
    settings: Entity<crate::settings::SettingsView>,
    dashboard: Entity<crate::dashboard::DashboardView>,
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
        restart: crate::settings::Restart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                if this.update_in(cx, |v, window, cx| v.on_msg(msg, window, cx)).is_err() {
                    break;
                }
            }
        });
        // Where the window is, remembered as it moves rather than on close:
        // a window killed with the session never gets to say goodbye.
        let mut subs = vec![cx.observe_window_bounds(window, |v, window, _| v.remember_bounds(window))];
        let tab = TABS.iter().find(|t| t.0 == state.tab).map(|t| t.0).unwrap_or("dashboard");
        let encounters = cx.new(|cx| crate::encounters::EncountersView::new(window, cx));
        let procs = cx.new(|_| crate::procs::ProcsView::new());
        let loot = cx.new(|cx| crate::loot::LootView::new(window, cx));
        let classes = cx.new(|_| crate::classes::ClassesView::new());
        let character = cx.new(|_| crate::character::CharacterView::new());
        let experience = cx.new(|_| crate::experience::ExperienceView::new());
        let tradeskills = cx.new(|_| crate::tradeskills::TradeskillsView::new());
        let forage = cx.new(|_| crate::forage::ForageView::new());
        let sky = cx.new(crate::sky::SkyView::new);
        let dashboard = cx.new(crate::dashboard::DashboardView::new);
        // The dashboard's Sky bars follow the Sky tab's own loads.
        subs.push(cx.subscribe(&sky, |v: &mut Self, _, e: &crate::sky::SkyLoaded, cx| {
            let r = e.0.clone();
            v.dashboard.update(cx, |d, cx| d.set_sky(r, cx));
        }));
        let tells = cx.new(|_| crate::tells::TellsView::new());
        let alerts = cx.new(|cx| crate::alerts::AlertsView::new(window, cx));
        let settings = cx.new(|cx| crate::settings::SettingsView::new(restart, window, cx));
        Shell {
            link: Link::Attaching,
            rt,
            encounters,
            procs,
            loot,
            classes,
            character,
            experience,
            tradeskills,
            forage,
            sky,
            tells,
            alerts,
            settings,
            dashboard,
            tab,
            snapshot: None,
            state,
            _tasks: vec![task],
            _subs: subs,
        }
    }

    fn on_msg(&mut self, msg: Msg, window: &mut Window, cx: &mut Context<Self>) {
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
                    v.set_api(Some(api.clone()), cx);
                    if open == "procs" {
                        v.load(cx);
                    }
                });
                self.character.update(cx, |v, cx| v.set_api(Some(api.clone()), cx));
                self.experience.update(cx, |v, cx| {
                    v.set_api(Some(api.clone()), cx);
                    if open == "experience" {
                        v.show(window, cx);
                    }
                });
                self.tradeskills.update(cx, |v, cx| {
                    v.set_api(Some(api.clone()), cx);
                    if open == "tradeskills" {
                        v.load(cx);
                    }
                });
                self.forage.update(cx, |v, cx| {
                    v.set_api(Some(api.clone()), cx);
                    if open == "forage" {
                        v.load(cx);
                    }
                });
                self.alerts.update(cx, |v, cx| {
                    v.set_api(Some(api.clone()), cx);
                    // A fresh attach loads the tab afresh if it is open.
                    v.set_visible(false, cx);
                    v.set_visible(open == "alerts", cx);
                });
                self.dashboard.update(cx, |v, cx| v.set_api(Some(api.clone()), cx));
                self.settings.update(cx, |v, cx| {
                    v.set_visible(open == "settings", cx);
                    v.set_api(Some(api.clone()), cx);
                });
                self.tells.update(cx, |v, cx| {
                    v.set_api(Some(api.clone()), cx);
                    if open == "tells" {
                        v.load(cx);
                    }
                });
                self.sky.update(cx, |v, cx| {
                    v.set_api(Some(api.clone()), cx);
                    if open == "sky" {
                        v.load(false, cx);
                    }
                });
                self.loot.update(cx, |v, cx| {
                    v.set_api(Some(api), cx);
                    if open == "loot" {
                        v.load_open(cx);
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
                let totals = s.get("totals");
                let played = totals.and_then(|t| t.get("played_seconds")).and_then(Value::as_i64).unwrap_or(0);
                let sessions = totals.and_then(|t| t.get("sessions")).and_then(Value::as_u64).unwrap_or(0);
                let loot_open = self.tab == "loot";
                self.loot.update(cx, |v, cx| v.on_snapshot(loot_open, played, sessions, cx));
                // Every push, whichever tab is open, as renderSnapshot does.
                let classes = s.get("classes").and_then(|c| serde_json::from_value(c.clone()).ok()).unwrap_or_default();
                self.classes.update(cx, |v, cx| v.on_snapshot(classes, cx));
                let totals = s.get("totals").and_then(|t| serde_json::from_value(t.clone()).ok());
                self.character.update(cx, |v, cx| v.on_snapshot(totals, cx));
                let experience_open = self.tab == "experience";
                self.experience.update(cx, |v, cx| v.on_snapshot(experience_open, window, cx));
                let tradeskills_open = self.tab == "tradeskills";
                self.tradeskills.update(cx, |v, cx| v.on_snapshot(tradeskills_open, cx));
                let forage_open = self.tab == "forage";
                self.forage.update(cx, |v, cx| v.on_snapshot(forage_open, cx));
                let sky_open = self.tab == "sky";
                self.sky.update(cx, |v, cx| v.on_snapshot(sky_open, cx));
                let current = s.get("current").filter(|c| !c.is_null()).and_then(|c| serde_json::from_value(c.clone()).ok());
                self.dashboard.update(cx, |v, cx| v.on_snapshot(current, cx));
                // Every snapshot, whichever tab is open: alerts are heard.
                let fires = s.get("alerts").and_then(|a| serde_json::from_value(a.clone()).ok()).unwrap_or_default();
                let seq = s.get("alert_seq").and_then(Value::as_u64).unwrap_or(0);
                self.alerts.update(cx, |v, cx| v.on_fires(fires, seq, cx));
                let tells_open = self.tab == "tells";
                self.tells.update(cx, |v, cx| v.on_snapshot(tells_open, cx));
                self.snapshot = Some(s);
            }
            Msg::Lost(why) => {
                eprintln!("[norrlog] lost the daemon: {why}");
                self.link = Link::Lost(why);
                self.snapshot = None;
                self.encounters.update(cx, |v, cx| v.set_api(None, cx));
                self.procs.update(cx, |v, cx| v.set_api(None, cx));
                self.loot.update(cx, |v, cx| v.set_api(None, cx));
                self.character.update(cx, |v, cx| v.set_api(None, cx));
                self.experience.update(cx, |v, cx| v.set_api(None, cx));
                self.tradeskills.update(cx, |v, cx| v.set_api(None, cx));
                self.forage.update(cx, |v, cx| v.set_api(None, cx));
                self.sky.update(cx, |v, cx| v.set_api(None, cx));
                self.tells.update(cx, |v, cx| v.set_api(None, cx));
                self.alerts.update(cx, |v, cx| v.set_api(None, cx));
                self.settings.update(cx, |v, cx| v.set_api(None, cx));
                self.dashboard.update(cx, |v, cx| v.set_api(None, cx));
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

    fn select(&mut self, tab: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        // As the renderer does: opening a tab fetches it afresh.
        if tab == "encounters" {
            self.encounters.update(cx, |v, cx| v.load(false, cx));
        }
        if tab == "procs" {
            self.procs.update(cx, |v, cx| v.load(cx));
        }
        // All, whichever subtab is showing (app.js:48).
        if tab == "loot" {
            self.loot.update(cx, |v, cx| v.load_open(cx));
        }
        if tab == "experience" {
            self.experience.update(cx, |v, cx| v.show(window, cx));
        }
        if tab == "tradeskills" {
            self.tradeskills.update(cx, |v, cx| v.load(cx));
        }
        if tab == "forage" {
            self.forage.update(cx, |v, cx| v.load(cx));
        }
        self.alerts.update(cx, |v, cx| v.set_visible(tab == "alerts", cx));
        self.settings.update(cx, |v, cx| v.set_visible(tab == "settings", cx));
        if tab == "tells" {
            self.tells.update(cx, |v, cx| v.load(cx));
        }
        if tab == "sky" {
            self.sky.update(cx, |v, cx| v.load(false, cx));
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
                    .on_click(cx.listener(move |v, _, window, cx| v.select(id, window, cx))),
            );
        }
        nav
    }

    fn render_body(&self) -> AnyElement {
        // Settings stays reachable without a daemon: a wrong path or port is
        // the likeliest reason there is none.
        if let Link::Lost(why) = &self.link
            && self.tab != "settings"
        {
            return notice("The daemon is not answering", why.clone()).into_any_element();
        }
        match self.tab {
            "dashboard" => self.dashboard.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "encounters" => self.encounters.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "procs" => self.procs.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "loot" => self.loot.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "experience" => self.experience.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "character" => self.character.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "classes" => self.classes.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "tradeskills" => self.tradeskills.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "forage" => self.forage.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "alerts" => self.alerts.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "tells" => self.tells.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "sky" => self.sky.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            "settings" => self.settings.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
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
            .child(if matches!(self.tab, "encounters" | "procs" | "loot" | "classes" | "character" | "experience" | "tradeskills" | "forage" | "sky" | "tells" | "alerts" | "dashboard") && !matches!(self.link, Link::Lost(_)) || self.tab == "settings" {
                // These scroll themselves, and are drawn cached: each redraws
                // only when it notifies, not on every snapshot the shell takes.
                div().id("main").flex_1().min_h_0().p_4().child(self.render_body()).into_any_element()
            } else {
                div().id("main").flex_1().min_h_0().overflow_y_scrollbar().p_4().child(self.render_body()).into_any_element()
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
