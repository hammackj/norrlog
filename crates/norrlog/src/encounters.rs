//! Encounters: every fight in the log, filterable, newest first, and one
//! fight opened up. `docs/parity/encounters-and-procs.md` is the checklist.
//!
//! The list is fetched when the tab is opened and when a filter changes
//! (text after 220 ms of quiet, the rest at once), and again when a fight
//! closes, but only if you are looking at the top of the first page: rows
//! do not move under someone reading them. They are told instead, and get
//! the new fights by scrolling back up.

use crate::api::{Api, query};
use crate::format::{dur, num, when};
use crate::table::{self, Col, col, muted, tag};
use crate::theme::*;
use crate::fight::FightUi;
use eqlparser_api::{EncounterBrief, EncounterDetail, EncounterPage, MobPage, MobRow};
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Sizable, h_flex, v_flex};
use std::time::Duration;

/// Rows per page: the renderer's, and as much as is worth drawing at once.
const PAGE: usize = 300;
const DEBOUNCE: Duration = Duration::from_millis(220);

const FIGHT_COLS: &[Col] = &[
    col("Encounter", None, false),
    col("Zone", Some(190.), false),
    col("Build", Some(110.), false),
    col("When", Some(132.), false),
    col("Time", Some(60.), true),
    col("Your DPS", Some(70.), true),
    col("Your dmg", Some(70.), true),
    col("Total", Some(70.), true),
    col("Loot", Some(44.), true),
];
const MOB_COLS: &[Col] = &[
    col("Mob", None, false),
    col("Zone", Some(190.), false),
    col("Build", Some(110.), false),
    col("When", Some(132.), false),
    col("Fight", Some(60.), true),
    col("Dmg to it", Some(76.), true),
    col("Dmg it did", Some(76.), true),
    col("Kills", Some(44.), true),
    col("Loot", Some(44.), true),
];

const DIFFICULTIES: &[(Option<u8>, &str)] = &[
    (None, "Difficulty: any"),
    (Some(0), "d0 open"),
    (Some(1), "d1 Awakened"),
    (Some(2), "d2 Adaptive"),
    (Some(3), "d3 Fused"),
    (Some(4), "d4 Refined"),
];

enum Rows {
    Fights(Vec<EncounterBrief>),
    Mobs(Vec<MobRow>),
}

impl Rows {
    fn len(&self) -> usize {
        match self {
            Rows::Fights(v) => v.len(),
            Rows::Mobs(v) => v.len(),
        }
    }
}

enum Detail {
    Loading(u64),
    Shown(Box<EncounterDetail>),
    Failed(String),
}

pub struct EncountersView {
    api: Option<Api>,
    search: Entity<InputState>,
    zone: Entity<InputState>,
    build: Entity<InputState>,
    min_dur: Entity<InputState>,
    difficulty: Option<u8>,
    by_mob: bool,
    rows: Rows,
    total: usize,
    error: Option<String>,
    /// Bumped by every fresh load, so a slow answer to an older one is
    /// dropped instead of drawn over a newer.
    generation: u64,
    debounce: Option<Task<()>>,
    loading: Option<Task<()>>,
    last_closed: Option<u64>,
    new_waiting: bool,
    scroll: ScrollHandle,
    detail: Option<Detail>,
    /// The open fight's subtab and drilled-open meter rows.
    pub(crate) fight: FightUi,
    /// Spell hover cards, each name asked about once.
    pub(crate) spells: Entity<crate::spell_tip::SpellCards>,
    pub(crate) items: Entity<crate::item_tip::ItemCards>,
    detail_scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl EncountersView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        let search = input("Search by mob name…", window, cx);
        let zone = input("Zone…", window, cx);
        let build = input("Build, e.g. WAR/DRU/BRD…", window, cx);
        let min_dur = input("Min duration (s)", window, cx);
        let mut subs = Vec::new();
        for i in [&search, &zone, &build, &min_dur] {
            subs.push(cx.subscribe(i, |v, _, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    v.debounced(cx);
                }
            }));
        }
        EncountersView {
            api: None,
            search,
            zone,
            build,
            min_dur,
            difficulty: None,
            by_mob: false,
            rows: Rows::Fights(Vec::new()),
            total: 0,
            error: None,
            generation: 0,
            debounce: None,
            loading: None,
            last_closed: None,
            new_waiting: false,
            scroll: ScrollHandle::new(),
            detail: None,
            fight: FightUi::default(),
            spells: cx.new(|_| crate::spell_tip::SpellCards::new()),
            items: cx.new(|_| crate::item_tip::ItemCards::new()),
            detail_scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.spells.update(cx, |c, _| c.set_api(api.clone()));
        self.items.update(cx, |c, _| c.set_api(api.clone()));
        self.api = api;
        cx.notify();
    }

    /// The filters as query pairs: the list and the export ask the same.
    fn filters(&self, cx: &App) -> Vec<(&'static str, String)> {
        let text = |i: &Entity<InputState>| i.read(cx).value().trim().to_string();
        let mut q = Vec::new();
        for (k, v) in [("search", text(&self.search)), ("zone", text(&self.zone)), ("build", text(&self.build))] {
            if !v.is_empty() {
                q.push((k, v));
            }
        }
        if let Some(d) = self.difficulty {
            q.push(("difficulty", d.to_string()));
        }
        if let Ok(d) = text(&self.min_dur).parse::<i64>()
            && d > 0
        {
            q.push(("min_duration", d.to_string()));
        }
        q
    }

    fn debounced(&mut self, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _ = this.update(cx, |v, cx| v.load(false, cx));
        }));
    }

    /// The first page again (`append` false), or the next one.
    pub fn load(&mut self, append: bool, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        if !append {
            self.generation += 1;
            self.new_waiting = false;
        }
        let generation = self.generation;
        let mut q = self.filters(cx);
        q.push(("limit", PAGE.to_string()));
        q.push(("offset", if append { self.rows.len() } else { 0 }.to_string()));
        let by_mob = self.by_mob;
        let path = format!("{}?{}", if by_mob { "/api/mobs" } else { "/api/encounters" }, query(&q));
        self.loading = Some(cx.spawn(async move |this, cx| {
            let got: Result<(Rows, usize), String> = if by_mob {
                api.get::<MobPage>(path).await.map(|p| (Rows::Mobs(p.mobs), p.total))
            } else {
                api.get::<EncounterPage>(path).await.map(|p| (Rows::Fights(p.encounters), p.total))
            };
            let _ = this.update(cx, |v, cx| {
                if v.generation != generation || v.by_mob != by_mob {
                    return;
                }
                match got {
                    Ok((rows, total)) => {
                        v.total = total;
                        v.error = None;
                        match (append, &mut v.rows, rows) {
                            (true, Rows::Fights(have), Rows::Fights(more)) => have.extend(more),
                            (true, Rows::Mobs(have), Rows::Mobs(more)) => have.extend(more),
                            (_, slot, rows) => *slot = rows,
                        }
                    }
                    // An error while appending is quiet: what is shown stays
                    // right, and the button is still there to try again.
                    Err(e) if !append => {
                        v.error = Some(e);
                        v.rows = if by_mob { Rows::Mobs(Vec::new()) } else { Rows::Fights(Vec::new()) };
                        v.total = 0;
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
        }));
    }

    /// From every snapshot: the newest closed fight, and whether this tab
    /// is the one showing.
    pub fn newest_closed(&mut self, id: Option<u64>, visible: bool, cx: &mut Context<Self>) {
        let Some(id) = id else { return };
        let first = self.last_closed.is_none();
        let changed = self.last_closed != Some(id);
        self.last_closed = Some(id);
        if first || !changed || !visible || self.detail.is_some() {
            return;
        }
        if self.undisturbed() {
            self.load(false, cx);
        } else if !self.new_waiting {
            self.new_waiting = true;
            cx.notify();
        }
    }

    fn undisturbed(&self) -> bool {
        self.rows.len() <= PAGE && self.scroll.offset().y > px(-8.)
    }

    fn open(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.detail = Some(Detail::Loading(id));
        self.fight = FightUi::default();
        self.detail_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let got = api.get::<EncounterDetail>(format!("/api/encounters/{id}")).await;
            let _ = this.update(cx, |v, cx| {
                if matches!(v.detail, Some(Detail::Loading(want)) if want == id) {
                    v.detail = Some(match got {
                        Ok(d) => Detail::Shown(Box::new(d)),
                        Err(e) => Detail::Failed(format!("Could not load encounter: {e}")),
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn export(&self, csv: bool, cx: &mut Context<Self>) {
        let Some(api) = &self.api else { return };
        let mut q = self.filters(cx);
        if csv {
            q.push(("format", "csv".into()));
        }
        // Opened rather than fetched: a download, named by the daemon's
        // Content-Disposition.
        cx.open_url(&format!("{}/api/encounters/export?{}", api.base(), query(&q)));
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let cur = self.difficulty;
        let label = DIFFICULTIES.iter().find(|d| d.0 == cur).map(|d| d.1).unwrap_or("Difficulty: any");
        let w = this.clone();
        let difficulty = Button::new("enc-difficulty").label(label).small().dropdown_menu(move |mut menu, _, _| {
            for &(d, label) in DIFFICULTIES {
                let w = w.clone();
                menu = menu.item(PopupMenuItem::new(label).checked(cur == d).on_click(move |_, _, cx| {
                    let _ = w.update(cx, |v, cx| {
                        v.difficulty = d;
                        v.load(false, cx);
                    });
                }));
            }
            menu
        });
        let by_mob = self.by_mob;
        let w = this.clone();
        let split = Button::new("enc-split")
            .label(if by_mob { "Show: each mob" } else { "Show: whole fights" })
            .small()
            .dropdown_menu(move |mut menu, _, _| {
                for (mob, label) in [(false, "whole fights"), (true, "each mob")] {
                    let w = w.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(by_mob == mob).on_click(move |_, _, cx| {
                        let _ = w.update(cx, |v, cx| {
                            v.by_mob = mob;
                            v.rows = if mob { Rows::Mobs(Vec::new()) } else { Rows::Fights(Vec::new()) };
                            v.load(false, cx);
                        });
                    }));
                }
                menu
            });
        let input = |i: &Entity<InputState>, w: f32| div().w(px(w)).child(Input::new(i).small());
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(input(&self.search, 220.))
            .child(input(&self.zone, 150.))
            .child(input(&self.build, 190.))
            .child(difficulty)
            .child(input(&self.min_dur, 120.))
            .child(split)
            .child(Button::new("enc-md").label("Export .md").small().ghost().on_click(cx.listener(|v, _, _, cx| v.export(false, cx))))
            .child(Button::new("enc-csv").label("Export .csv").small().ghost().on_click(cx.listener(|v, _, _, cx| v.export(true, cx))))
            .child(muted(self.count_text()))
    }

    fn count_text(&self) -> String {
        count_text(self.rows.len(), self.total, self.by_mob, self.new_waiting)
    }

    fn render_list(&self, cx: &mut Context<Self>) -> Div {
        let needle = self.search.read(cx).value().trim().to_lowercase();
        let (cols, body): (&[Col], Vec<AnyElement>) = match &self.rows {
            Rows::Fights(v) => {
                (FIGHT_COLS, v.iter().map(|e| self.fight_row(e, &needle, cx).into_any_element()).collect())
            }
            Rows::Mobs(v) => (MOB_COLS, v.iter().map(|m| self.mob_row(m, cx).into_any_element()).collect()),
        };
        let mut t = v_flex().rounded(px(8.)).border_1().border_color(hex(BORDER)).bg(hex(PANEL)).overflow_hidden().child(table::head(cols));
        if let Some(e) = &self.error {
            t = t.child(table::empty_row(e.clone()));
        } else if body.is_empty() {
            t = t.child(table::empty_row(if self.by_mob { "No matching mobs" } else { "No matching encounters" }));
        } else {
            t = t.children(body);
        }
        let mut out = v_flex().gap_3().child(t);
        if self.error.is_none() && self.rows.len() < self.total {
            out = out.child(
                div().flex().justify_center().child(
                    Button::new("enc-more").label("Load more").small().on_click(cx.listener(|v, _, _, cx| v.load(true, cx))),
                ),
            );
        }
        out
    }

    fn fight_row(&self, e: &EncounterBrief, needle: &str, cx: &mut Context<Self>) -> Stateful<Div> {
        // The fight is titled after the creature that took most damage; the
        // one searched for is often among the rest, so say which matched.
        let matched = matched_opponent(&e.title, &e.opponents, needle);
        let mut name = div().id(SharedString::from(format!("enc-t-{}", e.id))).flex().flex_row().gap_1().child(e.title.clone());
        if let Some(m) = matched {
            name = name.child(muted(format!("· {m}")));
        }
        if e.opponents.len() > 1 {
            let all = e.opponents.join(", ");
            name = name.tooltip(move |window, cx| Tooltip::new(all.clone()).build(window, cx));
        }
        let id = e.id;
        table::row(
            SharedString::from(format!("enc-{}", e.id)),
            FIGHT_COLS,
            vec![
                name.into_any_element(),
                zone_cell(&e.zone, e.difficulty).into_any_element(),
                muted(if e.build.is_empty() { "—".into() } else { e.build.clone() }).into_any_element(),
                muted(when(Some(e.start))).into_any_element(),
                div().child(dur(e.duration as f64)).into_any_element(),
                div().child(num(Some(e.your_dps))).into_any_element(),
                div().child(num(Some(e.your_damage as f64))).into_any_element(),
                muted(num(Some(e.total_damage as f64))).into_any_element(),
                muted(blank_zero(e.loot_count as u64)).into_any_element(),
            ],
        )
        .cursor_pointer()
        .on_click(cx.listener(move |v, _, _, cx| v.open(id, cx)))
    }

    fn mob_row(&self, m: &MobRow, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut name = div().flex().flex_row().gap_1().child(m.name.clone());
        if m.shared_with > 0 {
            name = name.child(muted(format!("· with {} more", m.shared_with)));
        }
        let id = m.encounter_id;
        table::row(
            SharedString::from(format!("mob-{}-{}", m.encounter_id, m.name)),
            MOB_COLS,
            vec![
                name.into_any_element(),
                zone_cell(&m.zone, m.difficulty).into_any_element(),
                muted(if m.build.is_empty() { "—".into() } else { m.build.clone() }).into_any_element(),
                muted(when(Some(m.start))).into_any_element(),
                muted(dur(m.duration as f64)).into_any_element(),
                div().child(num(Some(m.damage_done_to_it as f64))).into_any_element(),
                muted(num(Some(m.damage_it_did as f64))).into_any_element(),
                muted(blank_zero(m.kills as u64)).into_any_element(),
                muted(blank_zero(m.loot as u64)).into_any_element(),
            ],
        )
        .cursor_pointer()
        .on_click(cx.listener(move |v, _, _, cx| v.open(id, cx)))
    }

    /// One fight, from `fight.rs`, under a way back to the list. The list
    /// is kept as it was, so going back costs no fetch.
    fn render_detail(&self, cx: &mut Context<Self>) -> Div {
        let back = Button::new("enc-back").label("← Back to list").small().ghost().on_click(cx.listener(|v, _, _, cx| {
            v.detail = None;
            cx.notify();
        }));
        let body = match &self.detail {
            Some(Detail::Loading(_)) | None => muted("Loading…"),
            Some(Detail::Failed(e)) => div().text_color(hex(ENEMY)).child(e.clone()),
            Some(Detail::Shown(d)) => self.render_fight(d, cx),
        };
        vbar(
            &self.detail_scroll,
            div()
                .id("fight-scroll")
                .size_full()
                .overflow_y_scroll()
                .own_axis()
                .track_scroll(&self.detail_scroll)
                .child(v_flex().gap_3().pb_4().child(h_flex().child(back)).child(body)),
        )
        .size_full()
    }
}

impl Render for EncountersView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.detail.is_some() {
            return div().size_full().child(self.render_detail(cx)).into_any_element();
        }
        v_flex()
            .size_full()
            .gap_3()
            .child(self.render_toolbar(cx))
            .child(vbar(
                &self.scroll,
                div()
                    .id("enc-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .own_axis()
                    .track_scroll(&self.scroll)
                    // Back at the top with fights waiting: bring them in.
                    .on_scroll(cx.listener(|v, _, _, cx| {
                        if v.new_waiting && v.undisturbed() {
                            v.load(false, cx);
                        }
                    }))
                    .child(self.render_list(cx)),
            )
            .flex_1()
            .min_h_0())
            .into_any_element()
    }
}

fn zone_cell(zone: &Option<String>, difficulty: u8) -> Div {
    let mut d = div().flex().flex_row().items_center().gap_1().child(muted(zone.clone().unwrap_or_else(|| "—".into())));
    if difficulty > 0 {
        d = d.child(tag(format!("d{difficulty}"), PET));
    }
    d
}

/// What the count line says: what is on screen as well as what exists, since
/// the count alone read as a promise the table was not keeping.
fn count_text(shown: usize, total: usize, by_mob: bool, waiting: bool) -> String {
    let unit = if by_mob { "mob" } else { "encounter" };
    let noun = format!("{unit}{}", if total == 1 { "" } else { "s" });
    let mut s = if shown < total { format!("showing {shown} of {total} {noun}") } else { format!("{total} {noun}") };
    if waiting {
        s.push_str(" · new fights, scroll up to refresh");
    }
    s
}

/// The opponent a search matched, when it is not the one the fight is
/// titled after. `needle` is lower-case.
fn matched_opponent<'a>(title: &str, opponents: &'a [String], needle: &str) -> Option<&'a String> {
    if needle.is_empty() || title.to_lowercase().contains(needle) {
        return None;
    }
    opponents.iter().find(|o| o.to_lowercase().contains(needle))
}

fn blank_zero(n: u64) -> String {
    if n == 0 { String::new() } else { n.to_string() }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings in gpui's `test` macro over the standard one.
    use super::{count_text, matched_opponent, query};

    #[test]
    fn the_count_says_what_is_shown_and_what_exists() {
        assert_eq!(count_text(300, 884, false, false), "showing 300 of 884 encounters");
        assert_eq!(count_text(1, 1, false, false), "1 encounter");
        assert_eq!(count_text(0, 0, true, false), "0 mobs");
        assert_eq!(count_text(300, 885, false, true), "showing 300 of 885 encounters · new fights, scroll up to refresh");
    }

    #[test]
    fn a_search_names_the_opponent_it_matched_only_when_the_title_hides_it() {
        let foes = vec!["a lurking mummy".to_string(), "a skeletal monk".to_string()];
        assert_eq!(matched_opponent("a lurking mummy (+1 more)", &foes, "monk").map(String::as_str), Some("a skeletal monk"));
        assert_eq!(matched_opponent("a lurking mummy (+1 more)", &foes, "mummy"), None);
        assert_eq!(matched_opponent("a lurking mummy (+1 more)", &foes, ""), None);
        assert_eq!(matched_opponent("a lurking mummy (+1 more)", &foes, "gnoll"), None);
    }

    #[test]
    fn filters_go_out_as_the_renderer_sends_them() {
        let q = query(&[("search", "skeletal monk".into()), ("difficulty", "4".into()), ("min_duration", "30".into())]);
        assert_eq!(q, "search=skeletal+monk&difficulty=4&min_duration=30");
    }
}
