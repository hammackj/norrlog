//! Dashboard: the Combat box (one fight, the session or this zone, by
//! damage, healing or damage taken), the week's lockouts and Sky progress by
//! class. Ports `renderScope`/`showMetric`/`setTabCounts` (app.js),
//! `lockouts.js` and `skyprogress.js`; the checklist is the Dashboard
//! section of `docs/parity/shell-and-dashboard.md`.
//!
//! Every snapshot, whichever tab is open: the live fight is taken from it,
//! `/api/session` is asked again (at most every 2 s, and only while Session
//! or This zone is selected), and `/api/lockouts` and `/api/sky` at most
//! every 15 s. The Sky tab's own loads redraw the progress bars too. A failed
//! fetch keeps what was there.

use crate::api::Api;
use crate::fight::{Metric, metric, plain_meter};
use crate::format::{dur, num, now_log, until, when};
use crate::item_tip::ItemCards;
use crate::parts::{band, fill, hint, note, BarAnim, ROW, ROW_YOU};
use crate::table::tag;
use crate::theme::*;
use eqlparser_api::{EncounterSummary, LockoutReport, SessionReply, SkyReport, SpriteIcon};
use gpui::prelude::FluentBuilder;
use gpui::*;
use crate::theme::px;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const SESSION_POLL: Duration = Duration::from_secs(2);
const SLOW_POLL: Duration = Duration::from_secs(15);

/// The Void-Touched Potential icon: drawn before any item lookup has
/// happened, so its sheet and cell are fixed, as `VOID_ICON_SHEET` is.
const VOID_ICON: (&str, i32, i32) = ("/static/items/042.png", -160, -80);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Fight,
    Session,
    Zone,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Damage,
    Healing,
    Taken,
}

pub struct DashboardView {
    api: Option<Api>,
    /// The meter's bars, eased between snapshots.
    bars: std::cell::RefCell<BarAnim>,
    scope: Scope,
    pane: Pane,
    /// Whether a snapshot has arrived yet: before one, the panes say
    /// "Waiting for a fight".
    seen: bool,
    current: Option<EncounterSummary>,
    session: Option<SessionReply>,
    lockouts: Option<LockoutReport>,
    sky: Option<SkyReport>,
    last_session: Option<Instant>,
    last_lockouts: Option<Instant>,
    last_sky: Option<Instant>,
    notes: [bool; 2],
    cards: Entity<ItemCards>,
    tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl DashboardView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let cards = cx.new(|_| ItemCards::new());
        let subs = vec![cx.observe(&cards, |_, _, cx| cx.notify())];
        DashboardView {
            api: None,
            scope: Scope::Fight,
            pane: Pane::Damage,
            bars: Default::default(),
            seen: false,
            current: None,
            session: None,
            lockouts: None,
            sky: None,
            last_session: None,
            last_lockouts: None,
            last_sky: None,
            notes: [false; 2],
            cards,
            tasks: Vec::new(),
            _subs: subs,
        }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.cards.update(cx, |c, _| c.set_api(api.clone()));
        self.api = api;
        self.last_session = None;
        self.last_lockouts = None;
        self.last_sky = None;
        cx.notify();
    }

    fn track(&mut self, t: Task<()>) {
        self.tasks.push(t);
        if self.tasks.len() > 6 {
            self.tasks.drain(..self.tasks.len() - 6);
        }
    }

    /// A snapshot arrived, whichever tab is open.
    pub fn on_snapshot(&mut self, current: Option<EncounterSummary>, cx: &mut Context<Self>) {
        self.seen = true;
        self.current = current;
        if self.scope != Scope::Fight && self.last_session.is_none_or(|t| t.elapsed() >= SESSION_POLL) {
            self.load_session(cx);
        }
        if self.last_lockouts.is_none_or(|t| t.elapsed() >= SLOW_POLL) {
            self.load_lockouts(cx);
        }
        if self.last_sky.is_none_or(|t| t.elapsed() >= SLOW_POLL) {
            self.load_sky(cx);
        }
        cx.notify();
    }

    /// The Sky tab loaded `/api/sky`; its bars here follow.
    pub fn set_sky(&mut self, r: SkyReport, cx: &mut Context<Self>) {
        self.sky = Some(r);
        cx.notify();
    }

    fn load_session(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_session = Some(Instant::now());
        let scope = self.scope;
        let path = format!("/api/session?scope={}", if scope == Scope::Zone { "zone" } else { "session" });
        self.track(cx.spawn(async move |this, cx| {
            let Ok(r) = api.get::<SessionReply>(path).await else { return };
            let _ = this.update(cx, |v, cx| {
                // An answer for a scope since switched away from is dropped.
                if v.scope == scope {
                    v.session = Some(r);
                    cx.notify();
                }
            });
        }));
    }

    fn load_lockouts(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_lockouts = Some(Instant::now());
        self.track(cx.spawn(async move |this, cx| {
            let Ok(r) = api.get::<LockoutReport>("/api/lockouts".into()).await else { return };
            let _ = this.update(cx, |v, cx| {
                v.lockouts = Some(r);
                cx.notify();
            });
        }));
    }

    fn load_sky(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_sky = Some(Instant::now());
        self.track(cx.spawn(async move |this, cx| {
            let Ok(r) = api.get::<eqlparser_api::SkyReply>("/api/sky".into()).await else { return };
            let _ = this.update(cx, |v, cx| v.set_sky(r.report, cx));
        }));
    }

    fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if scope == self.scope {
            return;
        }
        self.scope = scope;
        self.session = None;
        if scope != Scope::Fight {
            self.load_session(cx);
        }
        cx.notify();
    }
}

impl Render for DashboardView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sky_meta = self.sky.as_ref().map(|r| sky_meta(&sky_classes(r))).unwrap_or_default();
        div().id("dashboard-scroll").size_full().overflow_y_scrollbar().child(
            v_flex()
                .gap_3()
                .child(band("Combat", "what the parser makes of the fighting"))
                .child(self.render_combat(cx))
                .child(band("This week", "resets Tuesday 10:00"))
                .child(self.render_lockouts(cx))
                // The meta replaces the placeholder note, as `#sky-prog-meta` does.
                .child(band("Plane of Sky", if sky_meta.is_empty() { "progress by class" } else { &sky_meta }))
                .child(self.render_sky(cx)),
        )
    }
}

/// A row of segmented tabs, `.subtabs`.
fn subtabs() -> Div {
    h_flex().gap_1().p(px(3.)).rounded(px(7.)).bg(hex(PANEL_2)).self_start()
}

fn subtab(id: &'static str, label: &'static str, count: Option<String>, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .gap_1()
        .px_3()
        .py_1()
        .rounded(px(5.))
        .cursor_pointer()
        .when(active, |d| d.bg(hex(BG)).text_color(hex(ACCENT)))
        .when(!active, |d| d.text_color(hex(MUTED)).hover(|s| s.bg(wash(TEXT, 0.05)).text_color(hex(TEXT))))
        .child(label)
        .when_some(count, |d, c| d.child(div().text_color(hex(MUTED)).child(c)))
}

/// A tab's count: blank when nothing.
fn count(n: u64) -> Option<String> {
    (n > 0).then(|| num(Some(n as f64)))
}

impl DashboardView {
    fn render_combat(&self, cx: &mut Context<Self>) -> Div {
        let live = self.scope == Scope::Fight;
        let enc = if live { self.current.as_ref() } else { self.session.as_ref().filter(|s| s.fights > 0).map(|s| &s.summary) };

        let scope_tab = |id, label, s: Scope| {
            subtab(id, label, None, self.scope == s).on_click(cx.listener(move |v, _, _, cx| v.set_scope(s, cx)))
        };
        let scopes = subtabs()
            .child(scope_tab("dash-fight", "This fight", Scope::Fight))
            .child(scope_tab("dash-session", "Session", Scope::Session))
            .child(scope_tab("dash-zone", "This zone", Scope::Zone));

        let (dmg, heal, taken) = enc.map_or((0, 0, 0), |e| {
            (
                e.damage_to_enemies,
                e.meter.iter().map(|r| r.healing).sum::<u64>(),
                e.meter.iter().map(|r| r.damage_taken).sum::<u64>(),
            )
        });
        let pane_tab = |id, label, p: Pane, n: u64| {
            subtab(id, label, count(n), self.pane == p).on_click(cx.listener(move |v, _, _, cx| {
                v.pane = p;
                cx.notify();
            }))
        };
        let panes = subtabs()
            .child(pane_tab("dash-damage", "Damage", Pane::Damage, dmg))
            .child(pane_tab("dash-healing", "Healing", Pane::Healing, heal))
            .child(pane_tab("dash-taken", "Damage taken", Pane::Taken, taken));

        let (head, body) = match enc {
            None => {
                let title = if live {
                    "No active encounter"
                } else if self.scope == Scope::Zone {
                    "Nothing fought in this zone yet"
                } else {
                    "Nothing fought this session yet"
                };
                let body = match self.pane {
                    Pane::Damage if !self.seen => empty("Waiting for a fight"),
                    Pane::Damage if live => empty("Waiting for combat…"),
                    Pane::Damage => empty("No fights to add up"),
                    Pane::Healing => empty("No healing yet"),
                    Pane::Taken => empty("Nothing has hit you yet"),
                };
                (h_flex().child(div().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child(title)), body)
            }
            Some(e) => {
                // A fight running is the snapshot carrying one, not a
                // comparison with the clock: log times are local wall time
                // read as UTC, hours away from the machine's.
                let running = live && self.current.is_some();
                let title = h_flex()
                    .gap_2()
                    .items_center()
                    .when(running, |d| {
                        d.child(
                            div().size(px(8.)).rounded_full().bg(hex(HEAL)).with_animation(
                                "dash-live",
                                Animation::new(Duration::from_millis(1800)).repeat().with_easing(pulsating_between(0.35, 1.)),
                                |d, t| d.opacity(t),
                            ),
                        )
                    })
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(e.title.clone()));
                let meta = format!(
                    "{} dps · {} hps · taken {} · {}",
                    num(Some(e.your_dps)),
                    num(Some(e.your_hps)),
                    num(Some(e.your_damage_taken as f64)),
                    dur(e.duration as f64)
                );
                let body = match self.pane {
                    Pane::Damage => plain_meter(&e.meter, "No damage recorded", Some(&mut self.bars.borrow_mut())),
                    Pane::Healing => metric(&e.meter, Metric::Healing, "No healing yet", Some(&mut self.bars.borrow_mut())),
                    Pane::Taken => metric(&e.meter, Metric::Taken, "No damage taken", Some(&mut self.bars.borrow_mut())),
                };
                (
                    h_flex().gap_3().items_baseline().flex_wrap().child(title).child(div().text_color(hex(MUTED)).child(meta)),
                    body,
                )
            }
        };

        // State the denominator: these rates are per second of combat.
        let scope_note = self.session.as_ref().filter(|s| !live && s.fights > 0).map(|s| {
            let fights = s.fights.to_string();
            let combat = dur(s.combat_seconds as f64);
            let tail = format!(" of combat since {}. Rates are per second of combat, not of elapsed time.", when(Some(s.session_start)));
            div().text_sm().text_color(hex(MUTED)).child(crate::parts::rich(&[
                ("Added up over ", false),
                (&fights, true),
                (if s.fights == 1 { " fight and " } else { " fights and " }, false),
                (&combat, true),
                (&tail, false),
            ]))
        });

        lead().child(scopes).child(head).child(panes).child(body).children(scope_note)
    }

    fn render_lockouts(&self, cx: &mut Context<Self>) -> Div {
        let toggle = cx.listener(|v, _, _, cx| {
            v.notes[0] = !v.notes[0];
            cx.notify();
        });
        let notes = note(
            "dash-lock-note",
            self.notes[0],
            "W = weekly cleared (full loot taken, resets Tuesday 10:00) · D = daily cleared (single item, resets 10:00). \
             Difficulty comes from the zone name. Credit follows participation, not the killing blow — the game often \
             credits a charmed pet.",
            toggle,
        );
        let Some(d) = &self.lockouts else {
            return lead().child(lock_title(None)).child(empty("No boss kills recorded yet")).child(notes);
        };
        let v = &d.void_currency;
        let cells: HashMap<(&str, u8), &eqlparser_api::LockoutCell> =
            d.cells.iter().map(|c| ((c.boss.as_str(), c.difficulty), c)).collect();
        let diffs = 0..=d.max_difficulty;
        const BOSS_W: f32 = 210.;
        const CELL_W: f32 = 66.;

        let head = h_flex()
            .gap(px(7.))
            .text_xs()
            .text_color(hex(MUTED))
            .child(div().w(px(BOSS_W)).child("Boss"))
            .children(diffs.clone().map(|i| div().w(px(CELL_W)).flex().justify_center().child(format!("d{i}"))))
            .child(
                div()
                    .id("dash-vt-head")
                    .w(px(40.))
                    .flex()
                    .justify_center()
                    .child("VT")
                    .tooltip(|window, cx| Tooltip::new("Void-Touched Potential earned from this raid this week").build(window, cx)),
            );

        let icon = if v.this_week.is_empty() {
            None
        } else {
            let (path, x, y) = VOID_ICON;
            let s = SpriteIcon { path: path.into(), x, y, size: 40 };
            self.cards.update(cx, |c, cx| c.sprite(&s, cx))
        };

        let rows = d.bosses.iter().enumerate().map(|(bi, b)| {
            let tds = diffs.clone().map(|i| {
                let c = cells.get(&(b.as_str(), i));
                let (w, dd) = c.map_or((false, false), |c| (c.weekly_done, c.daily_done));
                let title: SharedString = match c.and_then(|c| c.last_kill.map(|t| (t, c.kills_this_week))) {
                    Some((t, n)) => {
                        format!("last killed {}{}", when(Some(t)), if n > 0 { format!(" · {n} this week") } else { String::new() })
                    }
                    None => "not killed".into(),
                }
                .into();
                h_flex()
                    .id(SharedString::from(format!("dash-lock-{bi}-{i}")))
                    .w(px(CELL_W))
                    .justify_center()
                    .gap(px(5.))
                    .py(px(4.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(hex(BORDER))
                    .bg(hex(PANEL_2))
                    .when(!w && !dd, |d| d.opacity(0.45))
                    .child(mark("W", w, ACCENT))
                    .child(mark("D", dd, HEAL))
                    .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
            });
            // The reward is per boss per week, so it sits on the boss's row.
            let won = v.this_week.iter().find(|g| g.boss.as_deref() == Some(b.as_str()));
            let badge = won.map(|g| {
                let title: SharedString = format!("Void-Touched Potential earned here {}", when(Some(g.at))).into();
                div()
                    .id(SharedString::from(format!("dash-vt-{bi}")))
                    .size(px(28.))
                    .rounded(px(4.))
                    .when_some(icon.clone(), |d, i| d.child(img(i).size_full()))
                    .when(icon.is_none(), |d| d.child(tag("VT", ACCENT)))
                    .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
            });
            h_flex()
                .gap(px(7.))
                .child(div().w(px(BOSS_W)).text_sm().font_weight(FontWeight::MEDIUM).whitespace_nowrap().text_ellipsis().child(b.clone()))
                .children(tds)
                .child(h_flex().w(px(40.)).justify_center().children(badge))
        });

        let now = now_log();
        lead()
            .child(lock_title(Some(d)))
            .child(
                div().id("dash-lock-scroll").overflow_x_scroll().child(
                    v_flex().gap(px(5.)).child(head).children(rows).when(d.bosses.is_empty(), |t| t.child(empty("No boss kills recorded yet"))),
                ),
            )
            .child(hint(format!(
                "Weekly reset in {} ({}) · daily in {}",
                until(d.week_end, now),
                when(Some(d.week_end)),
                until(d.day_end, now)
            )))
            .child(notes)
    }

    fn render_sky(&self, cx: &mut Context<Self>) -> Div {
        let toggle = cx.listener(|v, _, _, cx| {
            v.notes[1] = !v.notes[1];
            cx.notify();
        });
        let notes = note("dash-sky-note", self.notes[1], SKY_NOTE, toggle);
        let Some(r) = &self.sky else {
            return lead().child(empty("Reading the log for Sky turn-ins…")).child(notes);
        };
        let list = sky_classes(r);
        if list.is_empty() {
            return lead().child(empty("No Sky quests defined")).child(notes);
        }
        lead()
            .child(v_flex().gap_1().children(list.iter().map(|c| {
                let share = if c.total > 0 { c.earned as f64 / c.total as f64 } else { 0. };
                crate::parts::bar_row(
                    share,
                    fill(if c.mine { ROW_YOU } else { ROW }),
                    h_flex()
                        .gap_1()
                        .child(c.name.clone())
                        .when(c.mine, |d| d.child(tag("yours", ACCENT))),
                    h_flex()
                        .gap_3()
                        .child(div().text_color(hex(MUTED)).child(c.note()))
                        .child(div().min_w(px(44.)).flex().justify_end().font_weight(FontWeight::SEMIBOLD).child(format!("{}/{}", c.earned, c.total))),
                )
                .when(!c.mine, |d| d.opacity(0.55))
            })))
            .child(notes)
    }
}

/// `.card.lead`.
fn lead() -> Div {
    v_flex().gap_3().p_3().rounded(px(8.)).border_1().border_color(hex(BORDER)).bg(hex(PANEL))
}

fn empty(text: &'static str) -> Div {
    div().py_2().text_color(hex(MUTED)).child(text)
}

/// One of a cell's two lockouts.
fn mark(letter: &'static str, on: bool, color: u32) -> Div {
    div()
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .when(on, |d| d.bg(hex(color)).text_color(hex(BG)))
        .when(!on, |d| d.border_1().border_color(hex(BORDER)).text_color(hex(MUTED)))
        .child(letter)
}

/// The card title and its period line, whose hover names which boss paid
/// each void currency and which tasks are taken but unpaid.
fn lock_title(d: Option<&LockoutReport>) -> Div {
    let title = div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child("WEEKLY & DAILY LOCKOUTS");
    let Some(d) = d else { return h_flex().child(title) };
    let tip = (d.void_currency.cap > 0).then(|| void_tip(d));
    h_flex().gap_1().items_baseline().flex_wrap().child(title).child(
        div()
            .id("dash-lock-period")
            .text_sm()
            .text_color(hex(MUTED))
            .child(period_line(d))
            .when_some(tip, |el, t| {
                let t: SharedString = t.into();
                el.tooltip(move |window, cx| Tooltip::new(t.clone()).build(window, cx))
            }),
    )
}

/// ` — week of …`, as `lock-period` reads.
fn period_line(d: &LockoutReport) -> String {
    let v = &d.void_currency;
    let void = if v.cap > 0 {
        format!(
            " · void-touched {}/{}{}{}",
            v.earned_this_week,
            v.cap,
            if v.remaining > 0 { format!(" ({} left)", v.remaining) } else { " (capped)".into() },
            if v.open_this_week.is_empty() { String::new() } else { format!(", {} open", v.open_this_week.len()) }
        )
    } else {
        String::new()
    };
    let week = when(Some(d.week_start));
    format!(
        " — week of {} · weekly {}/{}, daily {}/{} · {} kills this week, {} today{void}{}",
        week.get(..10).unwrap_or(&week),
        d.weekly_cleared,
        d.total_cells,
        d.daily_cleared,
        d.total_cells,
        d.kills_this_week,
        d.kills_today,
        if d.current_build.is_empty() { String::new() } else { format!(" · as {}", d.current_build) }
    )
}

fn void_tip(d: &LockoutReport) -> String {
    let v = &d.void_currency;
    let paid: Vec<String> = v
        .this_week
        .iter()
        .map(|g| format!("  {} — {}", when(Some(g.at)), g.boss.as_deref().unwrap_or("source unknown")))
        .collect();
    let open: Vec<String> =
        v.open_this_week.iter().map(|o| format!("  {} — taken {}, not yet paid", o.boss, when(Some(o.assigned)))).collect();
    let mut parts = Vec::new();
    if !paid.is_empty() {
        parts.push(format!("Earned this week:\n{}", paid.join("\n")));
    }
    if !open.is_empty() {
        parts.push(format!("Still open:\n{}", open.join("\n")));
    }
    parts.push(format!("Cap is {} a week, read off play rather than stated by the game.", v.cap));
    parts.join("\n\n")
}

const SKY_NOTE: &str = "One bar per class: how many of that class's quests you have handed in, out of the quests \
    transcribed for it. The log never records receiving a reward — only the hand-in — so a turn-in is the closest \
    thing to proof a piece was earned, and anything turned in before this parser read your log cannot be counted at \
    all. Classes you are playing now come first and carry a note of what is left; the rest are dimmed and show only \
    the fraction, because held items are counted per item name rather than reserved to a class — a shared drop \
    would otherwise make a class you have never rolled look part-collected.";

/// One class's Sky set.
#[derive(Debug, Clone, PartialEq)]
struct SkyClass {
    name: String,
    total: u32,
    earned: u32,
    ready: u32,
    started: u32,
    mine: bool,
}

impl SkyClass {
    /// What is left, in the order you would act on it. Only for a class you
    /// are playing: held items are not reserved to a class.
    fn note(&self) -> String {
        if !self.mine {
            return String::new();
        }
        if self.earned == self.total {
            return "all handed in".into();
        }
        let mut bits = Vec::new();
        if self.ready > 0 {
            bits.push(format!("{} ready", self.ready));
        }
        if self.started > 0 {
            bits.push(format!("{} part-collected", self.started));
        }
        if bits.is_empty() { "nothing started".into() } else { bits.join(" · ") }
    }
}

/// Per class: your classes first, then furthest along, then by name.
fn sky_classes(r: &SkyReport) -> Vec<SkyClass> {
    let mut by: Vec<SkyClass> = Vec::new();
    for q in &r.quests {
        let i = match by.iter().position(|c| c.name == q.class) {
            Some(i) => i,
            None => {
                by.push(SkyClass { name: q.class.clone(), total: 0, earned: 0, ready: 0, started: 0, mine: false });
                by.len() - 1
            }
        };
        let c = &mut by[i];
        c.total += 1;
        // Three states that do not overlap: handed in, holding everything,
        // or part-way.
        if q.completed > 0 {
            c.earned += 1;
        } else if q.complete {
            c.ready += 1;
        } else if q.have_count > 0 {
            c.started += 1;
        }
        c.mine |= q.current_class;
    }
    let frac = |c: &SkyClass| if c.total > 0 { c.earned as f64 / c.total as f64 } else { 0. };
    by.sort_by(|a, b| {
        b.mine.cmp(&a.mine).then_with(|| frac(b).total_cmp(&frac(a))).then_with(|| a.name.cmp(&b.name))
    });
    by
}

/// ` — e/t on your classes · E/T across N classes`.
fn sky_meta(list: &[SkyClass]) -> String {
    let sum = |it: &mut dyn Iterator<Item = &SkyClass>| it.fold((0, 0), |(e, t), c| (e + c.earned, t + c.total));
    let (e, t) = sum(&mut list.iter());
    let all = format!("{e}/{t} across {} classes", list.len());
    let mine: Vec<&SkyClass> = list.iter().filter(|c| c.mine).collect();
    if mine.is_empty() {
        format!(" — {all}")
    } else {
        let (me, mt) = sum(&mut mine.into_iter());
        format!(" — {me}/{mt} on your classes · {all}")
    }
}

#[cfg(test)]
mod tests {
    use super::{SkyClass, sky_meta};

    fn class(name: &str, total: u32, earned: u32, ready: u32, started: u32, mine: bool) -> SkyClass {
        SkyClass { name: name.into(), total, earned, ready, started, mine }
    }

    #[test]
    fn sky_notes_and_meta_follow_skyprogress_js() {
        assert_eq!(class("Bard", 6, 6, 0, 0, true).note(), "all handed in");
        assert_eq!(class("Bard", 6, 1, 2, 1, true).note(), "2 ready · 1 part-collected");
        assert_eq!(class("Bard", 6, 0, 0, 0, true).note(), "nothing started");
        assert_eq!(class("Bard", 6, 0, 2, 0, false).note(), "");
        let list = [class("Bard", 6, 2, 0, 0, true), class("Cleric", 6, 1, 0, 0, false)];
        assert_eq!(sky_meta(&list), " — 2/6 on your classes · 3/12 across 2 classes");
        assert_eq!(sky_meta(&list[1..]), " — 1/6 across 1 classes");
    }
}
