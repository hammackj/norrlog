//! Loot: everything picked up, this session's haul, and the mote ladder,
//! with a drill-down per item. Ports `loot.js`; the checklist is the Loot
//! section of `docs/parity/loot-and-tooltips.md`.
//!
//! Like the renderer, the three lists are separate fetches with separate
//! caches. Opening the tab fetches All whichever subtab is showing; Session
//! fetches its own on selection; Motes reads both, so it draws from what is
//! cached and fetches both behind it. Snapshots refresh the visible one at
//! most every ten seconds.

use crate::api::{Api, query};
use crate::format::when;
use crate::item_tip::{ItemCards, item_tip};
use crate::table::{self, Col, col, muted};
use crate::parts::{card, hint, note, stat};
use crate::theme::*;
use eqlparser_api::{LootDetail, LootList, LootTally};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(10);

const COLS: &[Col] = &[
    col("Item", None, false),
    col("Count", Some(64.), true),
    col("Dropped by", None, false),
    col("What happened to it", None, false),
    col("Sold for (pp)", Some(104.), true),
];

const SOURCE_COLS: &[Col] = &[
    col("Creature", None, false),
    col("Drops", Some(64.), true),
    col("Items", Some(64.), true),
    col("Kills seen", Some(84.), true),
    col("Rate", Some(96.), true),
];

const ZONE_COLS: &[Col] = &[col("Zone", None, false), col("Drops", Some(64.), true)];

#[derive(Clone, Copy, PartialEq)]
enum Sub {
    All,
    Session,
    Motes,
}

pub struct LootView {
    api: Option<Api>,
    sub: Sub,
    all: Vec<LootTally>,
    all_error: Option<String>,
    session: Vec<LootTally>,
    session_start: i64,
    session_coin: u64,
    detail: Option<LootDetail>,
    search: Entity<InputState>,
    items: Entity<ItemCards>,
    /// From the snapshot, for the mote rate.
    played_seconds: i64,
    sessions: u64,
    session_note: bool,
    motes_note: bool,
    last_load: Option<Instant>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl LootView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search items…"));
        // Filters the cached list as you type; nothing is fetched.
        let subs = vec![cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                cx.notify();
            }
        })];
        LootView {
            api: None,
            sub: Sub::All,
            all: Vec::new(),
            all_error: None,
            session: Vec::new(),
            session_start: 0,
            session_coin: 0,
            detail: None,
            search,
            items: cx.new(|_| ItemCards::new()),
            played_seconds: 0,
            sessions: 0,
            session_note: false,
            motes_note: false,
            last_load: None,
            _tasks: Vec::new(),
            _subs: subs,
        }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.items.update(cx, |c, _| c.set_api(api.clone()));
        self.api = api;
        cx.notify();
    }

    /// Every item, for All and Motes.
    pub fn load_all(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_load = Some(Instant::now());
        self._tasks.push(cx.spawn(async move |this, cx| {
            let got = api.get::<LootList>("/api/loot".into()).await;
            let _ = this.update(cx, |v, cx| {
                match got {
                    Ok(l) => {
                        v.all = l.items;
                        v.all_error = None;
                    }
                    Err(e) => v.all_error = Some(e),
                }
                cx.notify();
            });
        }));
        self.trim_tasks();
    }

    /// This session's items. A failure is silent: the tab fills on the next
    /// refresh.
    fn load_session(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_load = Some(Instant::now());
        self._tasks.push(cx.spawn(async move |this, cx| {
            let Ok(l) = api.get::<LootList>("/api/loot?session=1".into()).await else { return };
            let _ = this.update(cx, |v, cx| {
                v.session = l.items;
                v.session_start = l.session_start;
                v.session_coin = l.session_coin_copper;
                cx.notify();
            });
        }));
        self.trim_tasks();
    }

    fn trim_tasks(&mut self) {
        // Finished tasks are dropped as new ones start; a handful at most.
        if self._tasks.len() > 8 {
            self._tasks.drain(..self._tasks.len() - 8);
        }
    }

    /// A snapshot arrived: refresh what is showing, ten seconds apart.
    pub fn on_snapshot(&mut self, visible: bool, played_seconds: i64, sessions: u64, cx: &mut Context<Self>) {
        let changed = (played_seconds, sessions) != (self.played_seconds, self.sessions);
        self.played_seconds = played_seconds;
        self.sessions = sessions;
        if !visible || self.last_load.is_some_and(|t| t.elapsed() < REFRESH) {
            if changed && visible && self.sub == Sub::Motes {
                cx.notify();
            }
            return;
        }
        match self.sub {
            Sub::All => self.load_all(cx),
            Sub::Session => self.load_session(cx),
            Sub::Motes => {
                self.load_session(cx);
                self.load_all(cx);
            }
        }
    }

    fn select(&mut self, sub: Sub, cx: &mut Context<Self>) {
        self.sub = sub;
        match sub {
            Sub::Session => self.load_session(cx),
            Sub::Motes => {
                self.load_session(cx);
                self.load_all(cx);
            }
            Sub::All => {}
        }
        cx.notify();
    }

    /// The drill-down. A failed fetch leaves the list showing.
    fn open(&mut self, item: String, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self._tasks.push(cx.spawn(async move |this, cx| {
            let Ok(d) = api.get::<LootDetail>(format!("/api/loot/item?{}", query(&[("item", item)]))).await else { return };
            let _ = this.update(cx, |v, cx| {
                v.detail = Some(d);
                cx.notify();
            });
        }));
        self.trim_tasks();
    }

    fn render_toolbar(&self, count: String, cx: &mut Context<Self>) -> Div {
        let sub = self.sub;
        let tab = |id: &'static str, label: &'static str, s: Sub| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded(px(5.))
                .cursor_pointer()
                .when(sub == s, |d| d.bg(hex(PANEL_2)).text_color(hex(ACCENT)))
                .when(sub != s, |d| d.text_color(hex(MUTED)).hover(|st| st.text_color(hex(TEXT))))
                .child(label)
                .on_click(cx.listener(move |v, _, _, cx| v.select(s, cx)))
        };
        h_flex()
            .gap_2()
            .child(tab("loot-all", "All loot", Sub::All))
            .child(tab("loot-session", "This session", Sub::Session))
            .child(tab("loot-motes", "Motes", Sub::Motes))
            .when(sub == Sub::All, |d| d.child(div().w(px(220.)).child(Input::new(&self.search).small())))
            .child(muted(count))
    }

    fn row(&self, prefix: &str, i: &LootTally, hover: bool, cx: &mut Context<Self>) -> AnyElement {
        let name = i.item.clone();
        // Full width spelled out: a virtual list sizes its rows to their content.
        let r = table::row(SharedString::from(format!("{prefix}-{}", i.item)), COLS, cells(i))
            .w_full()
            .cursor_pointer()
            .on_click(cx.listener(move |v, _, _, cx| v.open(name.clone(), cx)));
        if hover { item_tip(r, &self.items, &i.item).into_any_element() } else { r.into_any_element() }
    }

    /// The All table draws only the rows in view: a long log has thousands
    /// of items, and drawing them all made every window refresh cost as much
    /// as the whole list.
    fn render_all(&self, cx: &mut Context<Self>) -> (String, Div) {
        let q = self.search.read(cx).value().trim().to_lowercase();
        let shown: Vec<usize> = (0..self.all.len()).filter(|&n| matches(&self.all[n], &q)).collect();
        let count = format!("{} of {} items", shown.len(), self.all.len());
        let card = v_flex().rounded(px(8.)).border_1().border_color(hex(BORDER)).bg(hex(PANEL)).overflow_hidden().child(table::head(COLS));
        let card = if let Some(e) = &self.all_error {
            card.child(table::empty_row(e.clone()))
        } else if shown.is_empty() {
            card.child(table::empty_row("No matching items"))
        } else {
            let n = shown.len();
            card.flex_1().min_h_0().child(
                uniform_list(
                    "loot-all",
                    n,
                    cx.processor(move |v: &mut Self, range: std::ops::Range<usize>, _, cx| {
                        // Only the All table carries item cards, as in the renderer.
                        range.filter_map(|k| shown.get(k).and_then(|&n| v.all.get(n)).cloned()).map(|i| v.row("la", &i, true, cx)).collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .own_axis(),
            )
        };
        (count, card)
    }

    fn render_session(&self, cx: &mut Context<Self>) -> (String, Div) {
        let rows = motes_first(&self.session);
        let total: u64 = rows.iter().map(|i| i.count).sum();
        let sold: u64 = rows.iter().map(|i| i.sold_total_copper).sum();
        let count = if self.session_start != 0 {
            format!("since {} — {total} items, {} kinds", when(Some(self.session_start)), rows.len())
        } else {
            "no session yet".into()
        };
        let body: Vec<AnyElement> = if rows.is_empty() {
            vec![table::empty_row("Nothing looted this session yet").into_any_element()]
        } else {
            rows.iter().map(|i| self.row("ls", i, false, cx)).collect()
        };
        let summary = format!("{:.2}pp from corpses · {:.2}pp auto-sold", self.session_coin as f64 / 1000., sold as f64 / 1000.);
        let open = self.session_note;
        let panel = v_flex()
            .gap_3()
            .child(
                v_flex()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hex(BORDER))
                    .bg(hex(PANEL))
                    .overflow_hidden()
                    .child(div().px_3().py_2().text_color(hex(MUTED)).child(summary))
                    .child(table::head(COLS))
                    .children(body),
            )
            .child(note(
                "ls-note",
                open,
                "A session runs from the last gap of 30 minutes or more in the log. That is taken from the log's own \
                 clock rather than how long the app has been open, so restarting mid-session keeps the same session \
                 and starting up after an evening off does not inherit yesterday's haul.",
                cx.listener(|v, _, _, cx| {
                    v.session_note = !v.session_note;
                    cx.notify();
                }),
            ));
        (count, panel)
    }

    fn render_motes(&self, cx: &mut Context<Self>) -> (String, Div) {
        let motes = mote_ladder(&self.all);
        let this_session: std::collections::HashMap<String, u64> =
            self.session.iter().filter(|i| is_mote(&i.item)).map(|i| (i.item.to_lowercase(), i.count)).collect();
        let summary = motes_summary(&motes, this_session.values().sum(), self.played_seconds, self.sessions);
        let count = format!("{} mote tiers", motes.len());
        let max = motes.iter().map(|m| m.count).max().unwrap_or(1).max(1) as f32;
        let rows: Vec<AnyElement> = if motes.is_empty() {
            vec![div().py_2().text_color(hex(MUTED)).child("No motes looted yet").into_any_element()]
        } else {
            motes
                .iter()
                .map(|m| {
                    let n = this_session.get(&m.item.to_lowercase()).copied().unwrap_or(0);
                    let name = m.item.clone();
                    h_flex()
                        .id(SharedString::from(format!("mote-{}", m.item)))
                        .relative()
                        .justify_between()
                        .gap_3()
                        .px(px(9.))
                        .py(px(5.))
                        .rounded(px(5.))
                        .bg(hex(PANEL_2))
                        .overflow_hidden()
                        .cursor_pointer()
                        .child(div().absolute().top_0().bottom_0().left_0().w(relative(m.count as f32 / max)).bg(wash(YOU, 0.22)))
                        .child(div().font_weight(FontWeight::MEDIUM).child(m.item.clone()))
                        .child(
                            h_flex()
                                .gap(px(19.))
                                .flex_none()
                                .child(muted(format!("{} source{}", m.sources.len(), if m.sources.len() == 1 { "" } else { "s" })))
                                .child(if n > 0 {
                                    div().min_w(px(70.)).flex().justify_end().font_weight(FontWeight::SEMIBOLD).child(format!("+{n}"))
                                } else {
                                    div().min_w(px(70.)).flex().justify_end().text_color(hex(MUTED)).child("—")
                                })
                                .child(div().min_w(px(50.)).flex().justify_end().font_weight(FontWeight::SEMIBOLD).child(m.count.to_string())),
                        )
                        .on_click(cx.listener(move |v, _, _, cx| v.open(name.clone(), cx)))
                        .into_any_element()
                })
                .collect()
        };
        let open = self.motes_note;
        let panel = v_flex()
            .gap_2()
            .p_3()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(BORDER))
            .bg(hex(PANEL))
            .child(
                h_flex()
                    .gap_1()
                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child("MOTES OF POTENTIAL"))
                    .child(div().text_sm().text_color(hex(MUTED)).child(summary)),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(19.))
                    .pr(px(9.))
                    .text_xs()
                    .text_color(hex(MUTED))
                    .child(div().min_w(px(70.)).flex().justify_end().child("this session"))
                    .child(div().min_w(px(50.)).flex().justify_end().child("all time")),
            )
            .child(v_flex().gap_1().children(rows))
            .child(note(
                "lm-note",
                open,
                "Ordered by tier rather than by count, so the ladder reads in the order the game presents it. Any mote \
                 whose tier is not one of the known six is listed last rather than dropped — an unrecognised tier \
                 means a new one worth knowing about. The session column counts from your last logon, the same \
                 boundary the session tab uses.",
                cx.listener(|v, _, _, cx| {
                    v.motes_note = !v.motes_note;
                    cx.notify();
                }),
            ));
        (count, panel)
    }

    fn render_detail(&self, d: &LootDetail, cx: &mut Context<Self>) -> Div {
        let back = div()
            .id("loot-back")
            .px_2()
            .py_1()
            .rounded(px(5.))
            .cursor_pointer()
            .hover(|s| s.bg(hex(PANEL_2)))
            .child("← Back to loot")
            .on_click(cx.listener(|v, _, _, cx| {
                v.detail = None;
                cx.notify();
            }));
        let span = if d.first_seen == d.last_seen {
            when(Some(d.first_seen))
        } else {
            format!("{} — {}", when(Some(d.first_seen)), when(Some(d.last_seen)))
        };
        let stacks = d.stacks.iter().map(|(size, n)| format!("{size}×{n}")).collect::<Vec<_>>().join(", ");
        let creatures = d.sources.len();
        let sources: Vec<AnyElement> = d
            .sources
            .iter()
            .enumerate()
            .map(|(k, s)| {
                table::row(
                    SharedString::from(format!("lds-{k}")),
                    SOURCE_COLS,
                    vec![
                        v_flex()
                            .child(s.mob.clone())
                            .child(div().text_xs().text_color(hex(MUTED)).whitespace_normal().child(s.zones.join(", ")))
                            .into_any_element(),
                        div().child(s.drops.to_string()).into_any_element(),
                        muted(if s.quantity != s.drops { s.quantity.to_string() } else { String::new() }).into_any_element(),
                        muted(if s.kills > 0 { s.kills.to_string() } else { "—".into() }).into_any_element(),
                        match s.rate {
                            Some(r) => div().child(format!("{:.0}%", r * 100.)).into_any_element(),
                            None => muted("no kills seen").into_any_element(),
                        },
                    ],
                )
                .into_any_element()
            })
            .collect();
        let zones: Vec<AnyElement> = if d.zones.is_empty() {
            vec![table::empty_row("No zone recorded").into_any_element()]
        } else {
            d.zones
                .iter()
                .enumerate()
                .map(|(k, (z, n))| {
                    table::row(SharedString::from(format!("ldz-{k}")), ZONE_COLS, vec![
                        div().child(z.clone()).into_any_element(),
                        div().child(n.to_string()).into_any_element(),
                    ])
                    .into_any_element()
                })
                .collect()
        };
        v_flex()
            .gap_3()
            .child(h_flex().child(back))
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(d.item.clone()))
                    .child(muted(format!(
                        "{} received over {} drop{} · {span}",
                        d.total,
                        d.drops,
                        if d.drops == 1 { "" } else { "s" }
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_3()
                    .child(stat("Received", d.total.to_string(), Some(format!("{} drops", d.drops))))
                    .child(stat(
                        "Dropped by",
                        creatures.to_string(),
                        Some(if creatures == 1 { "creature".into() } else { "creatures".into() }),
                    ))
                    .child(stat("Zones", d.zones.len().to_string(), None))
                    .child(stat("Stack sizes", if stacks.is_empty() { "—".into() } else { stacks }, None)),
            )
            .child(
                card("Dropped by").child(table_card(SOURCE_COLS, sources)).child(hint(
                    "Rate is drops divided by deaths of that creature in your own fights. It counts corpses you were \
                     there for, not the game's loot table: a corpse you never looted still counts as a kill, so a rate \
                     reads low rather than wrong. A creature you have no recorded kill for shows no rate at all.",
                )),
            )
            .child(card("Where and what it was worth").child(table_card(ZONE_COLS, zones)).child(hint(worth(d))))
    }
}

impl Render for LootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = if let Some(d) = self.detail.clone() {
            self.render_detail(&d, cx).into_any_element()
        } else {
            let (count, body) = match self.sub {
                Sub::All => {
                    // Scrolls inside its table, so the toolbar stays put.
                    let (count, body) = self.render_all(cx);
                    return v_flex().size_full().gap_3().child(self.render_toolbar(count, cx)).child(body).into_any_element();
                }
                Sub::Session => self.render_session(cx),
                Sub::Motes => self.render_motes(cx),
            };
            v_flex().gap_3().child(self.render_toolbar(count, cx)).child(body).into_any_element()
        };
        // Its own scroller: the shell draws this view cached at a fixed size.
        // The detail scrolls apart from the list, so going back keeps the
        // list where it was.
        let id = if self.detail.is_some() { "loot-detail-scroll" } else { "loot-scroll" };
        div().id(id).size_full().overflow_y_scroll().own_axis().child(page).into_any_element()
    }
}

// ---------------------------------------------------------------------------
// The rules, apart from drawing.

fn is_mote(name: &str) -> bool {
    name.to_lowercase().split(|c: char| !c.is_alphanumeric()).any(|w| w == "mote")
}

const MOTE_TIERS: [&str; 6] = ["Infinitesimal", "Minor", "Lesser", "Potential", "Greater", "Major"];

/// Where a mote sits on the ladder. "Mote of Potential" carries no adjective
/// and sits between Lesser and Greater; an unknown tier sorts last, so a new
/// one shows rather than vanishing.
fn mote_tier(name: &str) -> usize {
    if let Some(i) = MOTE_TIERS.iter().position(|t| *t != "Potential" && name.contains(t)) {
        return i;
    }
    if name.trim().eq_ignore_ascii_case("mote of potential") { 3 } else { MOTE_TIERS.len() }
}

fn mote_ladder(all: &[LootTally]) -> Vec<&LootTally> {
    let mut m: Vec<&LootTally> = all.iter().filter(|i| is_mote(&i.item)).collect();
    m.sort_by(|a, b| mote_tier(&a.item).cmp(&mote_tier(&b.item)).then_with(|| a.item.cmp(&b.item)));
    m
}

/// Motes first, then the daemon's order, each half as it came.
fn motes_first(v: &[LootTally]) -> Vec<&LootTally> {
    v.iter().filter(|i| is_mote(&i.item)).chain(v.iter().filter(|i| !is_mote(&i.item))).collect()
}

fn motes_summary(motes: &[&LootTally], session: u64, played_seconds: i64, sessions: u64) -> String {
    if motes.is_empty() {
        return String::new();
    }
    let total: u64 = motes.iter().map(|m| m.count).sum();
    let hours = played_seconds as f64 / 3600.;
    let mut rate = String::new();
    if total > 0 && hours >= 0.05 {
        rate = format!(" · {:.1}/hour over {hours:.1}h played", total as f64 / hours);
        if sessions > 0 {
            rate.push_str(&format!(", {:.1} per session across {sessions}", total as f64 / sessions as f64));
        }
    }
    format!(
        " — {session} this session · {total} all time, across {} tier{}{rate}",
        motes.len(),
        if motes.len() == 1 { "" } else { "s" }
    )
}

fn matches(i: &LootTally, q: &str) -> bool {
    q.is_empty() || i.item.to_lowercase().contains(q) || i.sources.iter().any(|s| s.to_lowercase().contains(q))
}

/// Up to `n` names, then `+N`.
fn first(v: &[String], n: usize) -> String {
    let mut s = v.iter().take(n).cloned().collect::<Vec<_>>().join(", ");
    if v.len() > n {
        s.push_str(&format!(" +{}", v.len() - n));
    }
    s
}

/// What became of an item, as badges: `kept`, `sold`, `stored`,
/// `transmuted`, each `×N` only when it was not every copy.
fn fates(i: &LootTally) -> Vec<(String, u32)> {
    [(i.kept, "kept", MUTED), (i.sold, "sold", ACCENT), (i.stored, "stored", YOU), (i.transmuted, "transmuted", PET)]
        .into_iter()
        .filter(|(n, _, _)| *n > 0)
        .map(|(n, label, c)| (if n == i.count { label.to_string() } else { format!("{label} ×{n}") }, c))
        .collect()
}

fn fate_cell(i: &LootTally) -> AnyElement {
    let mut d = h_flex().gap_1().min_w_0().overflow_hidden();
    for (label, c) in fates(i) {
        d = d.child(div().flex_none().px(px(5.)).rounded(px(4.)).text_xs().bg(wash(c, 0.18)).text_color(hex(c)).child(label));
    }
    if !i.stored_in.is_empty() {
        d = d.child(muted(format!("in {}", first(&i.stored_in, 2))).whitespace_nowrap());
    }
    if !i.transmuted_into.is_empty() {
        d = d.child(muted(format!("→ {}", first(&i.transmuted_into, 2))).whitespace_nowrap());
    }
    d.into_any_element()
}

fn cells(i: &LootTally) -> Vec<AnyElement> {
    vec![
        div().child(i.item.clone()).into_any_element(),
        div().child(i.count.to_string()).into_any_element(),
        muted(first(&i.sources, 3)).into_any_element(),
        fate_cell(i),
        muted(if i.sold_total_copper > 0 { format!("{:.2}", i.sold_total_copper as f64 / 1000.) } else { String::new() })
            .into_any_element(),
    ]
}

fn pp(c: f64) -> String {
    format!("{:.2}p", c / 1000.)
}

fn worth(d: &LootDetail) -> String {
    let mut w = Vec::new();
    if d.sales > 0 {
        w.push(format!(
            "sold {} time{} for {} (best {}, avg {})",
            d.sales,
            if d.sales == 1 { "" } else { "s" },
            pp(d.sold_total_copper as f64),
            pp(d.best_sale_copper as f64),
            pp(d.sold_total_copper as f64 / d.sales as f64)
        ));
    }
    if let Some(b) = d.buy_price_copper {
        w.push(format!("a merchant charges {}", pp(b as f64)));
    }
    if w.is_empty() { "Never sold, and never bought from a merchant, so there is no price on record.".into() } else { w.join(" · ") }
}

// ---------------------------------------------------------------------------
// Pieces.

fn table_card(cols: &[Col], rows: Vec<AnyElement>) -> Div {
    v_flex().rounded(px(8.)).border_1().border_color(hex(BORDER)).bg(hex(PANEL)).overflow_hidden().child(table::head(cols)).children(rows)
}




/// The renderer's `<details class="note">`.

#[cfg(test)]
mod tests {
    use super::{fates, first, is_mote, matches, mote_ladder, mote_tier, motes_first, motes_summary, worth};
    use eqlparser_api::{LootDetail, LootTally};

    fn t(item: &str, count: u64) -> LootTally {
        LootTally {
            item: item.into(),
            count,
            sources: vec![],
            last_seen: 0,
            sold_total_copper: 0,
            kept: 0,
            sold: 0,
            stored: 0,
            transmuted: 0,
            stored_in: vec![],
            transmuted_into: vec![],
        }
    }

    #[test]
    fn a_mote_is_the_word_not_the_letters() {
        assert!(is_mote("Mote of Greater Potential"));
        assert!(is_mote("mote"));
        assert!(!is_mote("Remote Control"));
        assert!(!is_mote("Motes"), "the renderer's \\bmote\\b does not match a plural either");
    }

    #[test]
    fn the_ladder_reads_in_tier_order_with_unknown_tiers_last() {
        let all = [
            t("Mote of Major Potential", 1),
            t("Mote of Potential", 5),
            t("Mote of Odd Potential", 2),
            t("Mote of Infinitesimal Potential", 9),
            t("Bone Chips", 50),
            t("Mote of Lesser Potential", 3),
        ];
        let names: Vec<&str> = mote_ladder(&all).iter().map(|m| m.item.as_str()).collect();
        assert_eq!(
            names,
            [
                "Mote of Infinitesimal Potential",
                "Mote of Lesser Potential",
                "Mote of Potential",
                "Mote of Major Potential",
                "Mote of Odd Potential"
            ]
        );
        assert_eq!(mote_tier("Mote of Potential"), 3);
    }

    #[test]
    fn a_session_lists_motes_first_and_otherwise_keeps_the_daemons_order() {
        let v = [t("Bone Chips", 9), t("Mote of Minor Potential", 1), t("Rat Ear", 2)];
        let names: Vec<&str> = motes_first(&v).iter().map(|m| m.item.as_str()).collect();
        assert_eq!(names, ["Mote of Minor Potential", "Bone Chips", "Rat Ear"]);
    }

    #[test]
    fn fates_count_only_what_was_not_every_copy() {
        let mut i = t("Bone Chips", 548);
        i.kept = 9;
        i.stored = 539;
        let labels: Vec<String> = fates(&i).into_iter().map(|f| f.0).collect();
        assert_eq!(labels, ["kept ×9", "stored ×539"]);
        let mut j = t("Rusty Battle Axe +1", 5);
        j.sold = 5;
        assert_eq!(fates(&j)[0].0, "sold");
    }

    #[test]
    fn lists_stop_at_n_and_say_how_many_more() {
        let v: Vec<String> = ["a", "b", "c", "d", "e"].iter().map(|s| s.to_string()).collect();
        assert_eq!(first(&v, 3), "a, b, c +2");
        assert_eq!(first(&v[..2], 3), "a, b");
    }

    #[test]
    fn search_matches_the_item_or_any_source() {
        let mut i = t("Bone Chips", 1);
        i.sources = vec!["a decaying skeleton".into()];
        assert!(matches(&i, "bone"));
        assert!(matches(&i, "decaying"));
        assert!(!matches(&i, "rat"));
        assert!(matches(&i, ""));
    }

    #[test]
    fn the_mote_summary_gives_a_rate_only_over_real_play() {
        let m = [t("Mote of Potential", 10), t("Mote of Minor Potential", 2)];
        let refs: Vec<&LootTally> = m.iter().collect();
        assert_eq!(
            motes_summary(&refs, 3, 7200, 4),
            " — 3 this session · 12 all time, across 2 tiers · 6.0/hour over 2.0h played, 3.0 per session across 4"
        );
        assert_eq!(motes_summary(&refs, 0, 60, 1), " — 0 this session · 12 all time, across 2 tiers");
        assert_eq!(motes_summary(&[], 0, 7200, 1), "");
    }

    #[test]
    fn worth_names_sales_and_the_merchant_or_says_there_is_none() {
        let mut d = LootDetail {
            item: "x".into(),
            total: 1,
            drops: 1,
            first_seen: 0,
            last_seen: 0,
            sources: vec![],
            zones: vec![],
            kept: 0,
            sold: 0,
            stored: 0,
            transmuted: 0,
            sold_total_copper: 0,
            sales: 0,
            best_sale_copper: 0,
            buy_price_copper: None,
            stacks: vec![],
        };
        assert_eq!(worth(&d), "Never sold, and never bought from a merchant, so there is no price on record.");
        d.sales = 2;
        d.sold_total_copper = 3000;
        d.best_sale_copper = 2000;
        d.buy_price_copper = Some(78);
        assert_eq!(worth(&d), "sold 2 times for 3.00p (best 2.00p, avg 1.50p) · a merchant charges 0.08p");
    }
}
