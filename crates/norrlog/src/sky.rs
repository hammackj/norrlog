//! Sky: the Plane of Sky quests, `loadSky` and `renderSky` (sky.js), with
//! the item icons and links of itemdata.js. The checklist is the Sky section
//! of `docs/parity/sky-and-quests.md`.
//!
//! `/api/sky` is fetched when the tab opens and every ten seconds while it is
//! on screen; `?refresh=1` from the Refresh button. The bundled item data is
//! fetched once, and item briefs (icon, slug, whether the site has a page)
//! after each draw for any name not asked about yet.

use crate::api::Api;
use crate::format::{num, when_short};
use crate::item_tip::{ItemCards, item_tip};
use crate::parts::{ROW, ROW_YOU, bar_row, card, card_suffixed, dim, empty, figure, fill, note};
use crate::table::tag;
use crate::theme::*;
use norrlog_api::{IslandProgress, SiteQuest, SkyItemProgress, SkyQuestProgress, SkyReply, SkySite};
use gpui::prelude::FluentBuilder;
use gpui::*;
use crate::theme::px;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable, Sizable, h_flex, v_flex};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(10);

/// The Show filter. `complete` is holding everything; `completed` is having
/// handed it in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    All,
    Ready,
    Progress,
    Completed,
    Never,
    Untouched,
    Offsite,
}

const STATUSES: &[(Status, &str)] = &[
    (Status::All, "Everything"),
    (Status::Ready, "Ready to turn in"),
    (Status::Progress, "Started, not ready"),
    (Status::Completed, "Turned in before"),
    (Status::Never, "Never turned in"),
    (Status::Untouched, "Not started"),
    (Status::Offsite, "Not on eqldb yet"),
];

/// A fresh `/api/sky`, for the dashboard.
pub struct SkyLoaded(pub norrlog_api::SkyReport);

impl EventEmitter<SkyLoaded> for SkyView {}

pub struct SkyView {
    api: Option<Api>,
    reply: Option<SkyReply>,
    /// `mine`, `touched`, `all` or a class name. In memory only.
    scope: String,
    status: Status,
    /// The island whose drops are open.
    island: Option<String>,
    /// Quests whose giver text is open, by name: kept across redraws.
    open_quests: HashSet<String>,
    site_drops_open: bool,
    note_open: bool,
    refreshing: bool,
    cards: Entity<ItemCards>,
    last_load: Option<Instant>,
    loading: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl SkyView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let cards = cx.new(|_| ItemCards::new());
        // Icons and links fill in as briefs arrive.
        let subs = vec![cx.observe(&cards, |_, _, cx| cx.notify())];
        SkyView {
            api: None,
            reply: None,
            scope: "mine".into(),
            status: Status::All,
            island: None,
            open_quests: HashSet::new(),
            site_drops_open: false,
            note_open: false,
            refreshing: false,
            cards,
            last_load: None,
            loading: None,
            _subs: subs,
        }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.cards.update(cx, |c, _| c.set_api(api.clone()));
        self.api = api;
        self.last_load = None;
        cx.notify();
    }

    pub fn on_snapshot(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.load(false, cx);
        }
    }

    /// A failure is silent: the last draw stays.
    pub fn load(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.cards.update(cx, |c, cx| c.load_wiki(cx));
        self.last_load = Some(Instant::now());
        let path = if refresh { "/api/sky?refresh=1" } else { "/api/sky" };
        self.loading = Some(cx.spawn(async move |this, cx| {
            let got = api.get::<SkyReply>(path.to_string()).await;
            let _ = this.update(cx, |v, cx| {
                v.refreshing = false;
                if let Ok(r) = got {
                    // The dashboard's progress bars redraw from every load
                    // here, as `renderSkyProgress` is called from sky.js.
                    cx.emit(SkyLoaded(r.report.clone()));
                    v.reply = Some(r);
                }
                cx.notify();
            });
        }));
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refreshing = true;
        // Re-asked too: an item the site has just learned about would
        // otherwise keep its old icon.
        self.cards.update(cx, |c, _| c.clear_briefs());
        self.load(true, cx);
        cx.notify();
    }

    fn site(&self) -> Option<&SkySite> {
        self.reply.as_ref().and_then(|r| r.site.as_ref())
    }

    fn site_ok(&self) -> Option<&SkySite> {
        self.site().filter(|s| s.ok)
    }

    fn site_quest(&self, name: &str) -> Option<&SiteQuest> {
        self.site_ok().and_then(|s| s.quests.get(name))
    }

    fn base(&self) -> &str {
        self.site().map_or("", |s| s.base.as_str())
    }

    /// The quests the class and Show filters leave.
    fn rows(&self) -> Vec<&SkyQuestProgress> {
        let Some(r) = &self.reply else { return Vec::new() };
        r.report
            .quests
            .iter()
            .filter(|q| match self.scope.as_str() {
                "mine" => q.current_class,
                "touched" => q.relevant,
                "all" => true,
                c => q.class == c,
            })
            .filter(|q| match self.status {
                Status::All => true,
                Status::Ready => q.complete,
                Status::Completed => q.completed > 0,
                Status::Never => q.completed == 0,
                Status::Progress => q.have_count > 0 && !q.complete,
                Status::Untouched => q.have_count == 0 && q.completed == 0,
                Status::Offsite => self.site_quest(&q.name).is_none(),
            })
            .collect()
    }

    /// Where the site has seen an item drop: two creatures, `+N`, and the
    /// count it was seen, as a floor.
    fn drop_note(&self, name: &str) -> Option<String> {
        let d = self.site_ok()?.drops.get(name)?;
        if d.sources.is_empty() {
            return None;
        }
        let top: Vec<&str> = d.sources.iter().take(2).map(|s| s.npc.as_str()).collect();
        let more = if d.sources.len() > 2 { format!(" +{}", d.sources.len() - 2) } else { String::new() };
        let seen = match d.looted {
            Some(n) if n > 0 => format!(" seen {}×", num(Some(n as f64))),
            _ => String::new(),
        };
        Some(format!("{}{more}{seen}", top.join(", ")))
    }
}

const NOTE: &str = "Counts are items looted in this log, not current inventory — the client never reports a \
    turn-in, so a handed-in item still shows here. The quests are repeatable, so treat a ready quest as \"you have \
    seen every piece drop\", and check your bags before running to the island. Never turned in means no turn-in was \
    seen in this log, so a quest you finished before you started parsing still counts as missing. Trigger phrases \
    are matched only as a whole line you said, since words like skill and force are far too common to match inside \
    ordinary chat.\n\nWhere the facts come from. The quest table is hand-transcribed and lives in this app; eqldb \
    keeps its own record, assembled from parsed logs. Both are shown rather than one being picked: a quest tagged \
    eqldb is one the site has verified, draft is one it is still unsure of, and not on eqldb is one only this table \
    knows — the list worth contributing. If the two ever disagree about a giver, phrase, reward or item, the quest \
    says so in red rather than quietly preferring either.\n\nItem icons, links and the drop counts under an island \
    come from eqldb, with the bundled wiki data behind them for anything the site has not reached yet. Drop counts \
    are what the site has actually observed across contributed logs, so they are a floor: a source with no count \
    has simply never been recorded, not proved empty.";

impl Render for SkyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(reply) = self.reply.clone() else {
            return div().id("sky-scroll").size_full().child(card("Plane of Sky quests")).into_any_element();
        };
        let d = &reply.report;
        let rows = self.rows();

        let ready = rows.iter().filter(|q| q.complete).count();
        let turn_ins: u32 = rows.iter().map(|q| q.completed).sum();
        let mut summary = format!(" — {ready} of {} ready", rows.len());
        if turn_ins > 0 {
            summary += &format!(" · {turn_ins} turn-in{} recorded", if turn_ins == 1 { "" } else { "s" });
        }

        // Icons and links for what is on screen, asked after the draw.
        let mut names: Vec<String> = rows.iter().flat_map(|q| std::iter::once(q.reward.clone()).chain(q.items.iter().map(|i| i.name.clone()))).collect();
        let island = self.island.clone().and_then(|c| d.islands.iter().find(|i| i.code == c).cloned());
        let panel = island.map(|i| {
            let (panel, wanted) = self.render_island(&i, &rows, cx);
            names.extend(wanted);
            panel
        });
        self.cards.update(cx, |c, cx| c.load_briefs(names, cx));

        let body = v_flex()
            .gap_3()
            .child(self.render_toolbar(d.unmatched_runes.len(), cx))
            .children(self.site_line(d.quests.len()))
            .child(self.render_islands(&d.islands, &rows, cx))
            .children(panel)
            .child(if rows.is_empty() {
                empty("Nothing matches this filter")
            } else {
                v_flex().gap_4().children(rows.iter().enumerate().map(|(i, q)| self.render_quest(i, q, cx)))
            })
            .child(note(
                "sky-note",
                self.note_open,
                NOTE,
                cx.listener(|v, _, _, cx| {
                    v.note_open = !v.note_open;
                    cx.notify();
                }),
            ));
        div()
            .id("sky-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(card_suffixed("Plane of Sky quests", Some(summary)).text_base().child(body))
            .into_any_element()
    }
}

impl SkyView {
    fn render_toolbar(&self, unmatched: usize, cx: &mut Context<Self>) -> Div {
        let Some(r) = &self.reply else { return div() };
        let qs = &r.report.quests;
        let classes: BTreeSet<&str> = qs.iter().map(|q| q.class.as_str()).collect();
        let mut options: Vec<(String, String)> = vec![
            ("mine".into(), format!("My classes ({})", qs.iter().filter(|q| q.current_class).count())),
            ("touched".into(), format!("Mine, past & started ({})", r.report.relevant_count)),
            ("all".into(), format!("All classes ({})", qs.len())),
        ];
        options.extend(classes.iter().map(|c| (c.to_string(), format!("{c} ({})", qs.iter().filter(|q| q.class == *c).count()))));
        let current = options.iter().find(|o| o.0 == self.scope).map(|o| o.1.clone()).unwrap_or_default();
        let this = cx.entity().downgrade();
        let scope = self.scope.clone();
        let w = this.clone();
        let class = Button::new("sky-class").label(format!("Class: {current}")).small().dropdown_menu(move |mut menu, _, _| {
            for (value, label) in &options {
                let (w, value) = (w.clone(), value.clone());
                menu = menu.item(PopupMenuItem::new(label.clone()).checked(scope == value).on_click(move |_, _, cx| {
                    let _ = w.update(cx, |v, cx| {
                        v.scope = value.clone();
                        cx.notify();
                    });
                }));
            }
            menu
        });
        let status = self.status;
        let label = STATUSES.iter().find(|s| s.0 == status).map_or("", |s| s.1);
        let w = this.clone();
        let show = Button::new("sky-status").label(format!("Show: {label}")).small().dropdown_menu(move |mut menu, _, _| {
            for &(s, label) in STATUSES {
                let w = w.clone();
                menu = menu.item(PopupMenuItem::new(label).checked(status == s).on_click(move |_, _, cx| {
                    let _ = w.update(cx, |v, cx| {
                        v.status = s;
                        cx.notify();
                    });
                }));
            }
            menu
        });
        let off = self.site().is_some_and(|s| s.off);
        h_flex()
            .gap_2()
            .flex_wrap()
            .text_sm()
            .child(class)
            .child(show)
            .when(!off, |d| {
                d.child(
                    Button::new("sky-refresh")
                        .label(if self.refreshing { "Asking eqldb…" } else { "Refresh from eqldb" })
                        .small()
                        .ghost()
                        .disabled(self.refreshing)
                        .tooltip("Re-ask eqldb for its Plane of Sky record")
                        .on_click(cx.listener(|v, _, _, cx| v.refresh(cx))),
                )
            })
            .when(unmatched > 0, |d| d.child(dim(format!("{unmatched} unrecognised rune(s) looted"))))
    }

    /// How much of the table the site has, said once above the list.
    fn site_line(&self, total: usize) -> Option<Div> {
        let site = self.site()?;
        let line = div().text_sm().text_color(hex(MUTED));
        if !site.ok {
            let why = site.error.clone().unwrap_or_else(|| "eqldb could not be reached".into());
            return Some(line.child(Runs::new().colored(&why, ENEMY).plain(" — showing the built-in quest table only.").build()));
        }
        let on = total.saturating_sub(site.missing.len());
        let draft = site.quests.values().filter(|q| q.draft).count();
        let clash = site.quests.values().filter(|q| !q.disagrees.is_empty()).count();
        let mut r = Runs::new().plain(&format!("{on} of {total} quests on ")).bold(&host_of(&site.base), None);
        let sep = |r: Runs| r.plain(" · ");
        if draft > 0 {
            r = sep(r).plain(&format!("{draft} still draft"));
        }
        if clash > 0 {
            r = sep(r).bold(&format!("{clash} disagree with this table"), Some(ENEMY));
        }
        if !site.missing.is_empty() {
            r = sep(r).plain(&format!("{} not on the site yet", site.missing.len()));
        }
        let age = if site.age_seconds < 60 {
            "just fetched".to_string()
        } else {
            format!("fetched {}m ago", (site.age_seconds as f64 / 60.).round())
        };
        r = sep(r).plain(&age);
        Some(line.child(r.build()))
    }

    /// The islands as the game lays them out, with what each still owes the
    /// quests on screen.
    fn render_islands(&self, islands: &[IslandProgress], rows: &[&SkyQuestProgress], cx: &mut Context<Self>) -> Div {
        let mut wanted: HashMap<&str, usize> = HashMap::new();
        for q in rows.iter().filter(|q| !q.complete) {
            for it in q.items.iter().filter(|i| !i.have) {
                if let Some(c) = &it.island {
                    *wanted.entry(c.as_str()).or_default() += 1;
                }
            }
        }
        h_flex().flex_wrap().gap(px(6.)).children(islands.iter().map(|i| {
            let n = wanted.get(i.code.as_str()).copied().unwrap_or(0);
            let mut title = i
                .bosses
                .iter()
                .map(|b| if b.kills > 0 { format!("{} ×{}", b.boss, b.kills) } else { format!("{}: never", b.boss) })
                .collect::<Vec<_>>()
                .join("\n");
            if n > 0 {
                title += &format!(
                    "\n\n{n} item{} still needed here, across the {} quest{} listed below.",
                    if n == 1 { "" } else { "s" },
                    rows.len(),
                    if rows.len() == 1 { "" } else { "s" }
                );
            }
            let title = SharedString::from(title);
            let picked = self.island.as_deref() == Some(i.code.as_str());
            let code = i.code.clone();
            h_flex()
                .id(SharedString::from(format!("island-{}", i.code)))
                .items_baseline()
                .gap(px(6.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(4.))
                .border_1()
                .cursor_pointer()
                .border_color(if picked {
                    wash(ACCENT, 0.5)
                } else if i.kills > 0 {
                    hex(HEAL)
                } else {
                    hex(BORDER)
                })
                .bg(if picked { wash(ACCENT, 0.08) } else { hex(PANEL_2) })
                .when(i.kills == 0, |d| d.opacity(0.5))
                .child(div().text_sm().text_color(hex(MUTED)).child(i.code.clone()))
                .child(div().text_lg().child(i.name.clone()))
                .child(div().font_weight(FontWeight::BOLD).child(if i.kills > 0 { i.kills.to_string() } else { "—".into() }))
                .when(n > 0, |d| d.child(tag(format!("{n} wanted"), ACCENT).text_sm()))
                .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.island = if v.island.as_deref() == Some(code.as_str()) { None } else { Some(code.clone()) };
                    cx.notify();
                }))
        }))
    }

    /// What drops on the picked island, from the quests on screen, and below
    /// it what else the site has seen drop there. Returns the panel and the
    /// item names it draws.
    fn render_island(&self, island: &IslandProgress, rows: &[&SkyQuestProgress], cx: &mut Context<Self>) -> (Div, Vec<String>) {
        struct Seen {
            name: String,
            have: bool,
            held: u32,
            quests: Vec<String>,
        }
        let mut seen: Vec<Seen> = Vec::new();
        for q in rows {
            for it in q.items.iter().filter(|i| i.island.as_deref() == Some(island.code.as_str())) {
                let k = it.name.to_lowercase();
                let e = match seen.iter().position(|s| s.name.to_lowercase() == k) {
                    Some(p) => &mut seen[p],
                    None => {
                        seen.push(Seen { name: it.name.clone(), have: it.have, held: it.held, quests: Vec::new() });
                        seen.last_mut().unwrap()
                    }
                };
                e.have |= it.have;
                e.held = e.held.max(it.held);
                e.quests.push(format!("{}: {}", q.class, strip_first_word(&q.name)));
            }
        }
        seen.sort_by(|a, b| a.have.cmp(&b.have).then_with(|| a.name.cmp(&b.name)));
        let names: Vec<String> = seen.iter().map(|s| s.name.clone()).collect();
        let shown: HashSet<String> = names.iter().map(|n| n.to_lowercase()).collect();

        let bosses = island
            .bosses
            .iter()
            .map(|b| if b.kills > 0 { format!("{} ×{}", b.boss, b.kills) } else { b.boss.clone() })
            .collect::<Vec<_>>()
            .join(" · ");
        let title = h_flex()
            .gap_1()
            .items_baseline()
            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child(island.name.to_uppercase()))
            .child(dim(island.code.clone()).text_xs())
            .child(dim(format!(" — {bosses}")).text_sm());
        let mut panel = v_flex().gap_2().p_3().rounded(px(8.)).border_1().border_color(hex(BORDER)).bg(hex(PANEL)).child(title);
        panel = if seen.is_empty() {
            panel.child(empty("Nothing the quests on screen want drops here"))
        } else {
            panel.child(v_flex().gap(px(2.)).children(seen.iter().enumerate().map(|(i, s)| {
                let more = if s.quests.len() > 2 { format!(" +{}", s.quests.len() - 2) } else { String::new() };
                let src = format!("{}{more}", s.quests.iter().take(2).cloned().collect::<Vec<_>>().join(" · "));
                self.item_row(
                    SharedString::from(format!("isl-{i}")),
                    &s.name,
                    Some(s.have),
                    Some(src),
                    self.drop_note(&s.name),
                    (s.held > 1).then(|| format!("×{}", s.held)),
                    cx,
                )
            })))
        };

        // Everything else the site has seen drop here.
        let mut extra: Vec<(String, Vec<String>, u64)> = Vec::new();
        if let Some(site) = self.site_ok() {
            for (name, d) in &site.drops {
                if shown.contains(&name.to_lowercase()) {
                    continue;
                }
                let here: Vec<_> = d.sources.iter().filter(|s| s.island.as_deref() == Some(island.code.as_str())).collect();
                if here.is_empty() {
                    continue;
                }
                let looted = here.iter().map(|s| s.looted.unwrap_or(0)).sum();
                extra.push((name.clone(), here.iter().map(|s| s.npc.clone()).collect(), looted));
            }
        }
        extra.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        let mut all_names = names;
        if !extra.is_empty() {
            let open = self.site_drops_open;
            panel = panel.child(
                h_flex()
                    .id("sky-site-drops")
                    .gap_1()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(hex(MUTED))
                    .hover(|d| d.text_color(hex(TEXT)))
                    .child(if open { "▾" } else { "▸" })
                    .child(format!("{} more item{} eqldb has seen drop here", extra.len(), if extra.len() == 1 { "" } else { "s" }))
                    .on_click(cx.listener(|v, _, _, cx| {
                        v.site_drops_open = !v.site_drops_open;
                        cx.notify();
                    })),
            );
            if open {
                panel = panel.child(v_flex().gap(px(2.)).children(extra.iter().enumerate().map(|(i, (name, npcs, looted))| {
                    let more = if npcs.len() > 2 { format!(" +{}", npcs.len() - 2) } else { String::new() };
                    self.item_row(
                        SharedString::from(format!("isl-x-{i}")),
                        name,
                        None,
                        Some(format!("{}{more}", npcs.iter().take(2).cloned().collect::<Vec<_>>().join(", "))),
                        None,
                        Some(format!("seen {}×", num(Some(*looted as f64)))),
                        cx,
                    )
                })));
                all_names.extend(extra.into_iter().map(|e| e.0));
            }
        }
        (panel, all_names)
    }

    fn render_quest(&self, qi: usize, q: &SkyQuestProgress, cx: &mut Context<Self>) -> Div {
        let line = if q.complete { format!("say \"{}\" to {}", q.phrase, q.giver) } else { format!("{} to go", q.need_count) };
        let last = if q.completed > 0 { format!("last {} · ", when_short(q.last_completed)) } else { String::new() };
        let mut name = h_flex().gap_1().child(div().min_w_0().overflow_hidden().text_ellipsis().child(q.name.clone())).child(tag(q.class.clone(), ACCENT).flex_none());
        if let Some(t) = self.site_tag(qi, &q.name) {
            name = name.child(t);
        }
        if q.completed > 0 {
            name = name.child(tag("done", HEAL).flex_none()).child(tag(format!("{}× turned in", q.completed), HEAL).flex_none());
        }
        let share = if q.items.is_empty() { 0. } else { q.have_count as f64 / q.items.len() as f64 };
        let header = bar_row(
            share,
            fill(if q.complete { ROW_YOU } else { ROW }),
            name,
            h_flex().gap_3().child(dim(format!("{last}{line}"))).child(figure(format!("{}/{}", q.have_count, q.items.len()))),
        );
        let items = v_flex().gap(px(2.)).children(q.items.iter().enumerate().map(|(ii, it)| self.quest_item(qi, ii, it, cx)));
        let earned = if q.completed > 0 {
            let last = q.last_completed.map(|t| format!(", last {}", when_short(Some(t)))).unwrap_or_default();
            div().text_color(hex(YOU)).child(format!("earned {}×{last}", q.completed))
        } else {
            dim("not yet earned")
        };
        let reward = h_flex()
            .gap_1()
            .px(px(10.))
            .py(px(3.))
            .border_l_2()
            .border_color(hex(BORDER))
            .text_color(hex(MUTED))
            .child("Reward:")
            .child(self.item_name(SharedString::from(format!("rw-{qi}")), &q.reward, cx).text_color(hex(ACCENT)).font_weight(FontWeight::SEMIBOLD))
            .child(earned);
        v_flex()
            .gap_1()
            .rounded(px(5.))
            .when(q.complete, |d| d.bg(wash(HEAL, 0.09)))
            .when(!q.current_class && !q.complete, |d| d.opacity(0.55))
            .child(header)
            .child(items)
            .child(reward)
            .children(self.giver_text(qi, q, cx))
    }

    /// Provenance beside the class: verified, draft, disagreeing, or not on
    /// the site at all. None when the site is not answering.
    fn site_tag(&self, qi: usize, name: &str) -> Option<AnyElement> {
        self.site_ok()?;
        let (text, colour, title): (&str, u32, String) = match self.site_quest(name) {
            None => ("not on eqldb", ACCENT, "Only this app knows this quest. eqldb has no record of it yet.".into()),
            Some(sq) if !sq.disagrees.is_empty() => ("disagrees with eqldb", ENEMY, sq.disagrees.join("\n")),
            Some(sq) if sq.draft => ("draft on eqldb", MUTED, "eqldb has this quest but has not verified it.".into()),
            Some(_) => ("eqldb", HEAL, "eqldb has verified this quest, and it matches this table.".into()),
        };
        let title = SharedString::from(title);
        Some(
            tag(text, colour)
                .flex_none()
                .when(colour == HEAL, |d| d.opacity(0.75))
                .id(SharedString::from(format!("prov-{qi}")))
                .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                .into_any_element(),
        )
    }

    fn quest_item(&self, qi: usize, ii: usize, it: &SkyItemProgress, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut tail = Vec::new();
        if it.held > 1 {
            tail.push(format!("×{}", it.held));
        }
        if it.turned_in > 0 {
            tail.push(format!("{} handed in", it.turned_in));
        }
        self.item_row(
            SharedString::from(format!("it-{qi}-{ii}")),
            &it.name,
            Some(it.have),
            it.source.clone(),
            self.drop_note(&it.name),
            (!tail.is_empty()).then(|| tail.join("  ")),
            cx,
        )
    }

    /// An `.items .item`: icon, name, where it comes from, and a tail.
    #[allow(clippy::too_many_arguments)]
    fn item_row(
        &self,
        id: SharedString,
        name: &str,
        // Held, missing, or neither (the site's extra drops).
        have: Option<bool>,
        source: Option<String>,
        site_src: Option<String>,
        tail: Option<String>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let icon = self.cards.update(cx, |c, cx| c.row_icon(name, cx));
        let src = (source.is_some() || site_src.is_some()).then(|| {
            v_flex()
                .px(px(5.))
                .rounded(px(3.))
                .bg(wash(TEXT, 0.05))
                .text_sm()
                .text_color(hex(MUTED))
                .when_some(source, |d, s| d.child(s))
                .when_some(site_src, |d, s| d.child(div().text_xs().child(s)))
        });
        h_flex()
            .id(id.clone())
            .gap_2()
            .px(px(8.))
            .py(px(4.))
            .border_l_2()
            .border_color(hex(match have {
                Some(true) => HEAL,
                Some(false) => ENEMY,
                None => BORDER,
            }))
            .rounded_r(px(3.))
            .text_lg()
            .when(have == Some(false), |d| d.text_color(hex(MUTED)))
            .child(match icon {
                Some(i) => img(i).size(px(32.)).flex_none().rounded(px(3.)).into_any_element(),
                None => div().size(px(32.)).flex_none().into_any_element(),
            })
            .child(self.item_name(SharedString::from(format!("{id}-n")), name, cx).flex_none())
            .children(src)
            .child(div().flex_1())
            .when_some(tail, |d, t| d.child(dim(t).whitespace_nowrap()))
    }

    /// An item's name: a link to the site's page, or the wiki's when the
    /// site has none, with the item card on hover either way.
    fn item_name(&self, id: SharedString, name: &str, cx: &mut Context<Self>) -> Stateful<Div> {
        let link = self.cards.read(cx).link(name, self.base());
        let el = div().id(id).whitespace_nowrap().child(name.to_string());
        let el = match link {
            Some((url, wiki_only)) => el
                .cursor_pointer()
                .border_b_1()
                .border_color(wash(MUTED, if wiki_only { 0.5 } else { 0.8 }))
                .hover(|d| d.text_color(hex(YOU)).border_color(hex(YOU)))
                .on_click(move |_, _, cx| { crate::parts::open_web(cx, &url); }),
            None => el,
        };
        item_tip(el, &self.cards, name)
    }

    /// What the giver says: this character's capture and the site's
    /// transcript, kept apart. Open state survives redraws.
    fn giver_text(&self, qi: usize, q: &SkyQuestProgress, cx: &mut Context<Self>) -> Option<Div> {
        let sq = self.site_quest(&q.name);
        let theirs = sq.and_then(|s| s.dialogue.clone()).unwrap_or_default();
        let mine = &q.giver_text;
        if mine.is_empty() && theirs.is_empty() {
            return None;
        }
        let n = mine.len() + theirs.len();
        let host = host_of(self.base());
        let whose = match (mine.is_empty(), theirs.is_empty()) {
            (false, false) => String::new(),
            (false, true) => " (yours)".into(),
            _ => format!(" ({host})"),
        };
        let open = self.open_quests.contains(&q.name);
        let key = q.name.clone();
        let summary = h_flex()
            .id(SharedString::from(format!("gt-{qi}")))
            .gap_1()
            .cursor_pointer()
            .text_sm()
            .text_color(hex(MUTED))
            .hover(|d| d.text_color(hex(TEXT)))
            .child(if open { "▾" } else { "▸" })
            .child(format!("{n} line{} from {}{whose}", if n == 1 { "" } else { "s" }, q.giver))
            .on_click(cx.listener(move |v, _, _, cx| {
                if !v.open_quests.remove(&key) {
                    v.open_quests.insert(key.clone());
                }
                cx.notify();
            }));
        let mut out = v_flex().gap_1().child(summary);
        if open {
            if let Some(start) = sq.and_then(|s| s.start.clone()).filter(|s| !s.is_empty()) {
                out = out.child(qline().italic().child(start));
            }
            if !mine.is_empty() {
                out = out.child(qsrc("Heard by this character".into())).children(mine.iter().map(|l| {
                    // The chips under the text rather than after it: a flex
                    // row would stop the text wrapping.
                    qline().child(l.text.clone()).when(!l.keywords.is_empty(), |d| {
                        d.child(h_flex().flex_wrap().gap_1().mt_1().children(l.keywords.iter().map(|k| {
                            div().px(px(4.)).rounded(px(3.)).bg(wash(YOU, 0.18)).text_color(hex(YOU)).text_sm().child(k.clone())
                        })))
                    })
                }));
            }
            if !theirs.is_empty() {
                let by = match sq.and_then(|s| s.contributors).filter(|&c| c > 0) {
                    Some(c) => format!(" by {c} contributor{}", if c == 1 { "" } else { "s" }),
                    None => String::new(),
                };
                out = out.child(qsrc(format!("Recorded on {host}{by}"))).children(theirs.iter().map(|l| {
                    qline().child(Runs::new().colored(&l.speaker, MUTED).plain(" ").plain(&l.text).build())
                }));
            }
        }
        Some(out)
    }
}

fn qline() -> Div {
    div().my(px(2.)).px(px(10.)).py(px(3.)).border_l_2().border_color(hex(BORDER)).text_color(hex(MUTED))
}

fn qsrc(text: String) -> Div {
    div().mt_2().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child(text.to_uppercase())
}

/// Wrapping text built from runs, some coloured or bold.
struct Runs {
    text: String,
    styles: Vec<(std::ops::Range<usize>, HighlightStyle)>,
}

impl Runs {
    fn new() -> Self {
        Runs { text: String::new(), styles: Vec::new() }
    }

    fn push(mut self, t: &str, style: Option<HighlightStyle>) -> Self {
        let start = self.text.len();
        self.text.push_str(t);
        if let Some(s) = style {
            self.styles.push((start..self.text.len(), s));
        }
        self
    }

    fn plain(self, t: &str) -> Self {
        self.push(t, None)
    }

    fn colored(self, t: &str, c: u32) -> Self {
        self.push(t, Some(HighlightStyle { color: Some(hex(c)), ..Default::default() }))
    }

    fn bold(self, t: &str, c: Option<u32>) -> Self {
        self.push(t, Some(HighlightStyle { font_weight: Some(FontWeight::BOLD), color: c.map(hex), ..Default::default() }))
    }

    fn build(self) -> StyledText {
        StyledText::new(self.text).with_highlights(self.styles)
    }
}

/// The host part of an origin, for saying where a fact came from.
fn host_of(url: &str) -> String {
    match url.split_once("://") {
        Some((_, rest)) => rest.split('/').next().unwrap_or(rest).to_string(),
        None if url.is_empty() => "eqldb".into(),
        None => url.to_string(),
    }
}

/// A quest's name without its first word (the class): `/^\w+ /`.
fn strip_first_word(name: &str) -> &str {
    match name.split_once(' ') {
        Some((first, rest)) if !first.is_empty() && first.chars().all(|c| c.is_alphanumeric() || c == '_') => rest,
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::{host_of, strip_first_word};

    #[test]
    fn hosts_and_quest_names_are_trimmed_as_the_renderer_does() {
        assert_eq!(host_of("https://eqldb.hammackj.com"), "eqldb.hammackj.com");
        assert_eq!(host_of("http://127.0.0.1:8799/x"), "127.0.0.1:8799");
        assert_eq!(host_of(""), "eqldb");
        assert_eq!(strip_first_word("Bard Test of Brass"), "Test of Brass");
        assert_eq!(strip_first_word("Shadow Knight Test of Hate"), "Knight Test of Hate");
        assert_eq!(strip_first_word("Solo"), "Solo");
    }
}
