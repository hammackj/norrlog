//! Tradeskills: combines, skill-ups and what each recipe cost to raise.
//!
//! `/api/tradeskills` is fetched when the tab opens and every ten seconds
//! while it is on screen, and `/api/drops` after each of those. Selecting a
//! skill card narrows every panel below it to that tradeskill, from the
//! report already in hand: no fetch.
//!
//! "What it cost to raise" counts only recipes that earned a skill-up. What
//! went on recipes that raised nothing, a potion made to use, is shown beside
//! it rather than mixed in.

use crate::api::Api;
use crate::parts::{
    ROW, ROW_PET, ROW_YOU, bar_row, card_suffixed, dim, empty, figure, fill, hint, note, rich,
    split,
};
use crate::table::tag;
use crate::theme::px;
use crate::theme::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Sizable, h_flex, v_flex};
use norrlog_api::{CostBasis, DropReport, DropSighting, Game, ItemPrice, TradeskillReport};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(10);

pub struct TradeskillsView {
    api: Option<Api>,
    report: Option<TradeskillReport>,
    /// A failed `/api/tradeskills`, shown in the Combines card; everything
    /// else keeps what it had.
    error: Option<String>,
    /// Item (lowercased) -> creatures it was seen dropping from.
    drops: HashMap<String, Vec<DropSighting>>,
    /// Item (lowercased) -> its merchant prices.
    prices: HashMap<String, ItemPrice>,
    /// The skill the page is narrowed to. In memory only.
    open: Option<String>,
    /// History, Combines, Components, Costs.
    notes: [bool; 4],
    last_load: Option<Instant>,
    loading: Option<Task<()>>,
}

impl TradeskillsView {
    pub fn new() -> Self {
        TradeskillsView {
            api: None,
            report: None,
            error: None,
            drops: HashMap::new(),
            prices: HashMap::new(),
            open: None,
            notes: [false; 4],
            last_load: None,
            loading: None,
        }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.api = api;
        self.last_load = None;
        cx.notify();
    }

    /// A snapshot arrived: refresh if on screen and ten seconds have passed.
    pub fn on_snapshot(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.load(cx);
        }
    }

    pub fn load(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_load = Some(Instant::now());
        self.loading = Some(cx.spawn(async move |this, cx| {
            let got = api
                .get::<TradeskillReport>("/api/tradeskills".to_string())
                .await;
            let report = match got {
                Ok(r) => r,
                Err(e) => {
                    let _ = this.update(cx, |v, cx| {
                        v.error = Some(e.to_string());
                        cx.notify();
                    });
                    return;
                }
            };
            // Sources are a nicety, not the page: a failure leaves no drops.
            let drops = api
                .get::<DropReport>("/api/drops".to_string())
                .await
                .map(|d| d.items)
                .unwrap_or_default();
            let _ = this.update(cx, |v, cx| {
                v.drops = drops
                    .into_iter()
                    .map(|i| (i.item.to_lowercase(), i.sources))
                    .collect();
                v.prices = report
                    .prices
                    .iter()
                    .map(|p| (p.item.to_lowercase(), p.clone()))
                    .collect();
                // A skill gone from the report must not leave the page
                // filtered to nothing.
                if v.open
                    .as_ref()
                    .is_some_and(|o| !report.skills.iter().any(|s| &s.skill == o))
                {
                    v.open = None;
                }
                v.report = Some(report);
                v.error = None;
                cx.notify();
            });
        }));
    }

    fn select(&mut self, skill: Option<String>, cx: &mut Context<Self>) {
        self.open = if self.open == skill { None } else { skill };
        cx.notify();
    }

    /// Whether a row belongs to the selected skill. Unfiltered, everything
    /// does; filtered, a row with no skill does not.
    fn matches(&self, skill: Option<&str>) -> bool {
        match &self.open {
            None => true,
            Some(o) => skill.is_some_and(|s| s.eq_ignore_ascii_case(o)),
        }
    }

    fn suffix(&self) -> Option<String> {
        self.open.as_ref().map(|o| format!(" — {o}"))
    }

    fn toggle(i: usize) -> impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) {
        move |v, _, _, cx| {
            v.notes[i] = !v.notes[i];
            cx.notify();
        }
    }
}

const HISTORY_NOTE: &str = "Consecutive points on the same recipe collapse into one stretch, so alternating between two \
    recipes reads as alternating stretches rather than one merged range — the order you did them in is what the ranges \
    mean. Effort is counted over the interval each stretch covers, since the combine that earns a point happens before \
    the point is announced.";
const COMBINES_NOTE: &str = "The client never names the tradeskill on a combine line. It is attributed from the \
    adjacent skill-up — which the client prints before a success but after a failure, so both orders are read — and \
    that association is remembered per item, so combines that yield no skill-up (most of them, once a skill nears its \
    cap) are still attributed.";
const COMPONENTS_NOTE: &str = "Where a component was seen dropping is shown beneath it, with how many were looted \
    against how many of that creature were killed. That pair is a floor on the drop rate rather than the rate itself: \
    corpses you never looted are counted as kills too.";
const COMPONENTS_HINT: &str = "A component is attributed by the recipe that consumed it, so one used by several \
    tradeskills appears under each of them.";
const COSTS_NOTE: &str = "A floor, not a total. Components taken from the depot are announced; components taken from \
    inventory are consumed silently, and there is no way to tell a complete observation from a partial one — so every \
    figure counts only what the log actually revealed, and the real cost is at least this much. Prices are what you \
    last paid a merchant for the item; a component you never bought is listed rather than guessed at. The count on \
    each row is how many of that recipe's combines said anything about their components. A row marked from the recipe \
    announced nothing at all — its components came from your bags rather than the depot — so it is priced from the \
    recipe book instead, which is complete as to what a combine takes but assumes that recipe is the one you used.";
const FROM_BOOK: &str = "Nothing this recipe consumed was announced — its components came from your bags, not the \
    depot. Priced from the recipe book instead, at what you last paid a merchant for each component.";

impl Render for TradeskillsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = self.open.clone().map(|o| {
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(div().child(rich(&[
                    ("Showing ", false),
                    (&o, true),
                    (" only — recipes, components, costs and learned recipes below are this tradeskill's.", false),
                ])))
                .child(
                    Button::new("ts-filter-clear")
                        .label("Show all tradeskills")
                        .small()
                        .on_click(cx.listener(|v, _, _, cx| v.select(None, cx))),
                )
        });
        div()
            .id("ts-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .gap_3()
                    .child(self.render_skills(cx))
                    .children(filter)
                    .child(self.render_history(cx))
                    .child(split(self.render_crafts(cx), self.render_components(cx)))
                    .child(self.render_spend())
                    .child(self.render_costs(cx))
                    .child(self.render_recipes()),
            )
    }
}

impl TradeskillsView {
    fn render_skills(&self, cx: &mut Context<Self>) -> Div {
        let Some(r) = &self.report else { return div() };
        if r.skills.is_empty() {
            return div()
                .p_3()
                .rounded(px(8.))
                .border_1()
                .border_color(hex(BORDER))
                .bg(hex(PANEL))
                .child(empty("No tradeskill activity yet"));
        }
        // app.css' `.grid`: rows stretch, so cards in a row share a height.
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_3()
            .children(r.skills.iter().enumerate().map(|(i, s)| {
                let on = self.open.as_deref() == Some(s.skill.as_str());
                let practice = s.from_practice().to_string();
                let trained = format!("{} bought from a trainer", s.trained);
                let ups = format!("{} skill-ups", s.ups);
                let from = format!(", from {}", s.first_seen_at);
                let how = if s.trained > 0 {
                    rich(&[
                        (&practice, false),
                        (" by practice, ", false),
                        (&trained, true),
                        (&from, false),
                    ])
                } else {
                    rich(&[(&ups, false), (&from, false)])
                };
                let skill = s.skill.clone();
                v_flex()
                    .id(SharedString::from(format!("ts-skill-{i}")))
                    .min_w(px(208.))
                    .flex_1()
                    .gap_1()
                    .p_3()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hex(if on { ACCENT } else { BORDER }))
                    .when(!on, |d| d.hover(|d| d.border_color(hex(MUTED))))
                    .bg(hex(PANEL))
                    .cursor_pointer()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(hex(MUTED))
                            .child(s.skill.to_uppercase()),
                    )
                    .child(
                        h_flex()
                            .gap(px(5.))
                            .items_baseline()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child(s.current.to_string()),
                            )
                            .child(dim(format!("+{} this log", s.gained())).text_sm()),
                    )
                    .child(div().text_sm().text_color(hex(MUTED)).child(how))
                    .on_click(cx.listener(move |v, _, _, cx| v.select(Some(skill.clone()), cx)))
            }))
    }

    fn render_history(&self, cx: &mut Context<Self>) -> Div {
        let body = match (&self.report, &self.open) {
            (Some(r), Some(o)) => {
                let mut segs: Vec<_> = r.segments.iter().filter(|x| &x.skill == o).collect();
                segs.sort_by_key(|x| (x.from, x.started));
                if segs.is_empty() {
                    empty("No point history recorded for this skill")
                } else {
                    let max = segs.iter().map(|x| x.ups).max().unwrap_or(1).max(1) as f64;
                    v_flex().gap_1().children(segs.into_iter().map(|x| {
                        let what = if x.trained {
                            tag("trainer", ACCENT)
                        } else if let Some(item) = &x.item {
                            div().child(item.clone())
                        } else {
                            dim("unattributed")
                        };
                        let effort = if x.attempts > 0 {
                            format!(
                                "{}/{} made ({:.0}%)",
                                x.successes,
                                x.attempts,
                                (x.successes as f64 / x.attempts as f64 * 100.).round()
                            )
                        } else {
                            String::new()
                        };
                        bar_row(
                            x.ups as f64 / max,
                            fill(if x.trained { ROW } else { ROW_YOU }),
                            what,
                            h_flex()
                                .gap_3()
                                .when(x.copper > 0, |d| {
                                    d.child(dim(format!("≥ {}", plat(x.copper))))
                                })
                                .child(dim(effort))
                                .child(dim(format!("{} → {}", x.from, x.to)))
                                .child(figure(format!("+{}", x.ups))),
                        )
                    }))
                }
            }
            (Some(_), None) => empty(
                "Click a skill above to see how it was raised, and to show only that tradeskill below",
            ),
            (None, _) => div(),
        };
        card_suffixed("How it was raised", self.suffix())
            .child(body)
            .child(note(
                "ts-history-note",
                self.notes[0],
                HISTORY_NOTE,
                cx.listener(Self::toggle(0)),
            ))
    }

    fn render_crafts(&self, cx: &mut Context<Self>) -> Div {
        let card = card_suffixed("Combines", self.suffix());
        let card = if let Some(e) = &self.error {
            card.child(empty(e.clone()))
        } else if let Some(r) = &self.report {
            let only = self.open.as_deref();
            let crafts: Vec<_> = r
                .crafts
                .iter()
                .filter(|c| self.matches(c.skill.as_deref()))
                .collect();
            let hidden = self.hidden_note(
                r.crafts
                    .iter()
                    .filter(|c| c.skill.is_none())
                    .map(|c| c.item.as_str()),
                "combined recipe",
            );
            if crafts.is_empty() {
                card.child(empty(match only {
                    Some(o) => format!("No combines attributed to {o}"),
                    None => "No combines yet".into(),
                }))
                .children(hidden)
            } else {
                let max = crafts
                    .iter()
                    .map(|c| c.attempts())
                    .max()
                    .unwrap_or(1)
                    .max(1) as f64;
                let made: u32 = crafts.iter().map(|c| c.successes).sum();
                let failed: u32 = crafts.iter().map(|c| c.failures).sum();
                card.child(v_flex().gap_1().children(crafts.iter().map(|c| {
                    let att = c.attempts();
                    let rate = if att > 0 {
                        c.successes as f64 / att as f64 * 100.
                    } else {
                        0.
                    };
                    bar_row(
                        att as f64 / max,
                        fill(ROW),
                        h_flex()
                            .child(c.item.clone())
                            .when_some(c.skill.as_ref().filter(|_| only.is_none()), |d, s| {
                                d.child(dim(format!(" · {s}")))
                            }),
                        h_flex()
                            .gap_3()
                            .child(dim(format!("{}/{att}", c.successes)))
                            .child(figure(format!("{rate:.0}%"))),
                    )
                })))
                .child(hint(format!(
                    "{} {made} made, {failed} failed — {:.0}% success.",
                    only.unwrap_or("Overall"),
                    made as f64 / (made + failed).max(1) as f64 * 100.
                )))
                .children(hidden)
            }
        } else {
            card
        };
        card.child(note(
            "ts-crafts-note",
            self.notes[1],
            COMBINES_NOTE,
            cx.listener(Self::toggle(1)),
        ))
    }

    fn render_components(&self, cx: &mut Context<Self>) -> Div {
        let card = card_suffixed("Components used", self.suffix());
        let card = if let Some(r) = &self.report {
            let only = self.open.as_deref();
            let comps: Vec<_> = r
                .components
                .iter()
                .filter(|c| only.is_none() || c.skills.iter().any(|s| self.matches(Some(s))))
                .collect();
            if comps.is_empty() {
                card.child(empty(match only {
                    Some(o) => format!("No components attributed to {o}"),
                    None => "No components consumed".into(),
                }))
            } else {
                let max = comps.iter().map(|c| c.used).max().unwrap_or(1).max(1) as f64;
                card.child(
                    v_flex()
                        .gap_1()
                        .children(comps.iter().enumerate().map(|(i, c)| {
                            let name = v_flex()
                                .min_w_0()
                                .child(c.item.clone())
                                .children(self.vendor_line(i, &c.item))
                                .children(self.drop_line(i, &c.item));
                            bar_row(
                                c.used as f64 / max,
                                fill(ROW_PET),
                                name,
                                h_flex()
                                    .gap_3()
                                    .when_some(c.remaining, |d, n| {
                                        d.child(dim(format!("{n} left")))
                                    })
                                    .child(figure(c.used.to_string())),
                            )
                        })),
                )
                .children(
                    self.hidden_note(
                        r.components
                            .iter()
                            .filter(|c| c.skills.is_empty())
                            .map(|c| c.item.as_str()),
                        "consumed component",
                    ),
                )
            }
        } else {
            card
        };
        card.when(self.open.is_some(), |d| d.child(hint(COMPONENTS_HINT)))
            .child(note(
                "ts-components-note",
                self.notes[2],
                COMPONENTS_NOTE,
                cx.listener(Self::toggle(2)),
            ))
    }

    /// Who sells a component: the cheapest vendor, the rest on hover.
    fn vendor_line(&self, i: usize, item: &str) -> Option<Stateful<Div>> {
        let v = &self.prices.get(&item.to_lowercase())?.vendors;
        let cheapest = v.iter().min_by_key(|x| x.unit_copper)?;
        let one = |x: &norrlog_api::VendorPrice| match &x.zone {
            Some(z) => format!("{} ({z})", x.merchant),
            None => x.merchant.clone(),
        };
        let all: SharedString = v
            .iter()
            .map(|x| {
                format!(
                    "{} — {} each, {} bought",
                    one(x),
                    plat(x.unit_copper),
                    x.bought
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
            .into();
        let more = if v.len() > 1 {
            format!(" +{} more", v.len() - 1)
        } else {
            String::new()
        };
        Some(
            dim(format!(
                "buy: {} at {}{more}",
                one(cheapest),
                plat(cheapest.unit_copper)
            ))
            .text_sm()
            .id(SharedString::from(format!("ts-buy-{i}")))
            .tooltip(move |window, cx| Tooltip::new(all.clone()).build(window, cx)),
        )
    }

    /// Where a component drops: two creatures, the rest on hover.
    fn drop_line(&self, i: usize, item: &str) -> Option<Stateful<Div>> {
        let sources = self
            .drops
            .get(&item.to_lowercase())
            .filter(|s| !s.is_empty())?;
        let one = |s: &DropSighting| {
            let kills = if s.kills > 0 {
                s.kills.to_string()
            } else {
                "?".into()
            };
            // Sightings over kills: an item left on the corpse dropped just the same.
            format!("{} {}/{kills}", s.npc, s.looted + s.left)
        };
        let all: SharedString = sources
            .iter()
            .map(|s| {
                let zones = if s.zones.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", s.zones.join(", "))
                };
                format!("{} killed{zones}", one(s))
            })
            .collect::<Vec<_>>()
            .join("\n")
            .into();
        let shown: Vec<String> = sources.iter().take(2).map(one).collect();
        let more = if sources.len() > 2 {
            format!(" +{} more", sources.len() - 2)
        } else {
            String::new()
        };
        Some(
            dim(format!("drops: {}{more}", shown.join(", ")))
                .text_sm()
                .id(SharedString::from(format!("ts-drops-{i}")))
                .tooltip(move |window, cx| Tooltip::new(all.clone()).build(window, cx)),
        )
    }

    fn render_spend(&self) -> Div {
        let card = card_suffixed("What it cost to raise", self.suffix());
        let Some(r) = &self.report else { return card };
        let only = self.open.as_deref();
        let shown: Vec<_> = r
            .skill_costs
            .iter()
            .filter(|s| self.matches(Some(&s.skill)))
            .collect();
        if shown.is_empty() {
            return card.child(empty(match only {
                Some(o) => format!("Nothing priced for {o} yet"),
                None => "No combines have been priced yet".into(),
            }));
        }
        // One row per recipe for a single skill, else one per skill. The bar
        // and the figure are what went on raising it; `other` is what went on
        // recipes that earned no skill-up, shown beside it rather than mixed
        // in -- a potion made to use is not part of the raise.
        struct Row<'a> {
            name: &'a str,
            from_book: bool,
            raising: u64,
            other: u64,
            attempts: u64,
            unknown: bool,
            raised_nothing: bool,
        }
        let rows: Vec<Row> = if only.is_some() {
            shown[0]
                .recipes
                .iter()
                .map(|x| Row {
                    name: x.item.as_str(),
                    from_book: x.from_book,
                    raising: if x.skill_ups > 0 { x.copper } else { 0 },
                    other: if x.skill_ups > 0 { 0 } else { x.copper },
                    attempts: x.attempts,
                    unknown: x.unit_copper.is_none(),
                    raised_nothing: x.skill_ups == 0,
                })
                .collect()
        } else {
            shown
                .iter()
                .map(|s| Row {
                    name: s.skill.as_str(),
                    from_book: false,
                    raising: s.raising_copper,
                    other: s.copper - s.raising_copper,
                    attempts: s.attempts,
                    unknown: s.priced_attempts == 0,
                    raised_nothing: false,
                })
                .collect()
        };
        let max = rows
            .iter()
            .map(|r| r.raising.max(r.other))
            .max()
            .unwrap_or(1)
            .max(1) as f64;
        let raising: u64 = shown.iter().map(|s| s.raising_copper).sum();
        let raising_attempts: u64 = shown.iter().map(|s| s.raising_attempts).sum();
        let copper: u64 = shown.iter().map(|s| s.copper).sum();
        let attempts: u64 = shown.iter().map(|s| s.attempts).sum();
        let priced: u64 = shown.iter().map(|s| s.priced_attempts).sum();
        let book: u64 = shown.iter().map(|s| s.book_priced_attempts).sum();
        let how = if book > 0 {
            format!(
                "{} of all {attempts} combines priced from components the log announced, and {book} from the recipe book",
                priced - book
            )
        } else {
            format!("{priced} of all {attempts} combines priced from components the log announced")
        };
        let at_least = format!("at least {}", plat(raising));
        let plural = |n: u64| if n == 1 { "" } else { "s" };
        let rest = format!(
            " to raise, over {raising_attempts} combine{} at recipes that earned a skill-up",
            plural(raising_attempts)
        );
        let besides = if copper > raising {
            let n = attempts - raising_attempts;
            format!(
                ". Another {} went on {n} combine{} at recipes that raised nothing — made to use or sell. {how}.",
                plat(copper - raising),
                plural(n)
            )
        } else {
            format!(". {how}.")
        };
        let who = format!("{} cost ", only.unwrap_or("Every tradeskill"));
        card.child(v_flex().gap_1().children(rows.iter().map(|r| {
            let share = if r.raised_nothing { r.other } else { r.raising };
            bar_row(
                share as f64 / max,
                fill(if r.raised_nothing { ROW } else { ROW_YOU }),
                h_flex()
                    .gap_1()
                    .child(r.name.to_string())
                    .when(r.from_book, |d| {
                        d.child(tag("from the recipe", ACCENT).flex_none())
                    })
                    .when(r.raised_nothing, |d| {
                        d.child(tag("raised nothing", MUTED).flex_none())
                    }),
                h_flex()
                    .gap_3()
                    .child(dim(format!("{} combine{}", r.attempts, plural(r.attempts))))
                    .when(!r.raised_nothing && r.other > 0, |d| {
                        d.child(dim(format!("+ {} raised nothing", plat(r.other))))
                    })
                    .child(if r.unknown {
                        figure("").child(dim("no price"))
                    } else if r.raised_nothing {
                        figure("").child(dim(format!("≥ {}", plat(r.other))))
                    } else {
                        figure(format!("≥ {}", plat(r.raising)))
                    }),
            )
        })))
        .child(div().text_sm().text_color(hex(MUTED)).child(rich(&[
            (&who, false),
            (&at_least, true),
            (&rest, false),
            (&besides, false),
        ])))
        .child(
            div()
                .text_sm()
                .text_color(hex(MUTED))
                .child(cost_note(r.game)),
        )
    }

    fn render_costs(&self, cx: &mut Context<Self>) -> Div {
        let card = card_suffixed("What a combine cost", self.suffix());
        let card = if let Some(r) = &self.report {
            let only = self.open.as_deref();
            let costs: Vec<_> = r
                .costs
                .iter()
                .filter(|c| self.matches(c.skill.as_deref()))
                .collect();
            let hidden = self.hidden_note(
                r.costs
                    .iter()
                    .filter(|c| c.skill.is_none())
                    .map(|c| c.item.as_str()),
                "costed recipe",
            );
            if costs.is_empty() {
                card.child(empty(match only {
                    Some(o) => format!("No {o} combine announced its components yet"),
                    None => "No combine announced its components yet".into(),
                }))
                .children(hidden)
            } else {
                let max = costs
                    .iter()
                    .map(|c| c.unit_copper)
                    .max()
                    .unwrap_or(1)
                    .max(1) as f64;
                card.child(
                    v_flex()
                        .gap_1()
                        .children(costs.iter().enumerate().map(|(i, c)| {
                            let parts: Vec<String> = c
                                .components
                                .iter()
                                .map(|p| {
                                    if p.per_combine > 1. {
                                        format!("{} ×{}", p.item, trim2(p.per_combine))
                                    } else {
                                        p.item.clone()
                                    }
                                })
                                .collect();
                            let missing = if c.unpriced.is_empty() {
                                String::new()
                            } else {
                                format!(" · no price for {}", c.unpriced.join(", "))
                            };
                            let evidence = if c.basis == CostBasis::Recipe {
                                tag("from the recipe", ACCENT)
                                    .id(SharedString::from(format!("ts-book-{i}")))
                                    .tooltip(|window, cx| Tooltip::new(FROM_BOOK).build(window, cx))
                                    .into_any_element()
                            } else {
                                dim(format!("{} of {} combines", c.costed, c.combines))
                                    .into_any_element()
                            };
                            bar_row(
                                c.unit_copper as f64 / max,
                                fill(ROW_YOU),
                                v_flex()
                                    .min_w_0()
                                    .child(h_flex().child(c.item.clone()).when_some(
                                        c.skill.as_ref().filter(|_| only.is_none()),
                                        |d, s| d.child(dim(format!(" · {s}"))),
                                    ))
                                    .child(dim(format!("{}{missing}", parts.join(", "))).text_sm()),
                                h_flex()
                                    .gap_3()
                                    .child(evidence)
                                    .child(dim(format!("≥ {} all told", plat(c.total_copper))))
                                    .child(figure(format!("≥ {}", plat(c.unit_copper)))),
                            )
                        })),
                )
                .children(hidden)
            }
        } else {
            card
        };
        card.child(note(
            "ts-costs-note",
            self.notes[3],
            COSTS_NOTE,
            cx.listener(Self::toggle(3)),
        ))
    }

    fn render_recipes(&self) -> Div {
        let card = card_suffixed("Recipes learned", self.suffix());
        let Some(r) = &self.report else { return card };
        let only = self.open.as_deref();
        let recipes: Vec<_> = r
            .recipes_learned
            .iter()
            .filter(|x| self.matches(x.skill.as_deref()))
            .collect();
        let hidden = self.hidden_note(
            r.recipes_learned
                .iter()
                .filter(|x| x.skill.is_none())
                .map(|x| x.recipe.as_str()),
            "learned recipe",
        );
        if recipes.is_empty() {
            return card
                .child(empty(match only {
                    Some(o) => format!("None learned for {o} in this log"),
                    None => "None learned in this log".into(),
                }))
                .children(hidden);
        }
        card.child(v_flex().gap_1().children(recipes.iter().map(|x| {
            bar_row(
                0.,
                fill(ROW),
                h_flex()
                    .child(x.recipe.clone())
                    .when_some(x.skill.as_ref().filter(|_| only.is_none()), |d, s| {
                        d.child(dim(format!(" · {s}")))
                    }),
                div(),
            )
        })))
        .children(hidden)
    }

    /// What the filter left out because no tradeskill could be attributed:
    /// said plainly, while filtered, rather than silently dropped.
    fn hidden_note<'a>(&self, names: impl Iterator<Item = &'a str>, noun: &str) -> Option<Div> {
        self.open.as_ref()?;
        let names: Vec<&str> = names.collect();
        hidden_text(&names, noun).map(hint)
    }
}

fn hidden_text(names: &[&str], noun: &str) -> Option<String> {
    let n = names.len();
    if n == 0 {
        return None;
    }
    let (subject, verb) = if n == 1 {
        (noun.to_string(), "is")
    } else {
        (format!("{noun}s"), "are")
    };
    let listed = names.iter().take(6).copied().collect::<Vec<_>>().join(", ");
    let more = if n > 6 { ", …" } else { "" };
    Some(format!(
        "{n} {subject} {verb} not attributed to any tradeskill and {verb} hidden while filtering: {listed}{more}. \
         The client never names the tradeskill on a combine, so attribution needs an adjacent skill-up — a recipe \
         that has never skilled you up leaves none."
    ))
}

/// The same arithmetic means different things in the two games.
fn cost_note(game: Game) -> StyledText {
    match game {
        Game::EverQuest => rich(&[
            (
                "Every attempt is charged in full, including failures. Classic EverQuest's ",
                false,
            ),
            ("Salvage", true),
            (
                " AA returns some of a failed combine's components at random and no log line announces it, so the real \
                 figure is lower than this by an amount that cannot be measured from here.",
                false,
            ),
        ]),
        Game::EverQuestLegends => rich(&[(
            "Every attempt is charged in full: in EverQuest Legends a failed combine consumes its components exactly \
             like one that worked. Still a floor, though — only components drawn from the depot are announced, and \
             only ones you have bought have a price.",
            false,
        )]),
        Game::Unknown => rich(&[
            (
                "Every attempt is charged in full, including failures. The game this log came from could not be \
                 identified, so it is worth knowing that classic EverQuest's Salvage AA would return some failed \
                 components and make this an over-estimate; in Legends it would not. Pass ",
                false,
            ),
            ("--game eq", true),
            (" or ", false),
            ("--game eql", true),
            (" to settle it.", false),
        ]),
    }
}

/// Copper as platinum, two decimals: a real combine cost rounds to "0p" at
/// anything coarser.
fn plat(copper: u64) -> String {
    format!("{:.2}p", copper as f64 / 1000.)
}

/// A count per combine, as JavaScript's `+n.toFixed(2)` prints it.
fn trim2(n: f64) -> String {
    let s = format!("{n:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::{hidden_text, plat, trim2};

    #[test]
    fn costs_read_as_platinum_and_counts_lose_trailing_zeros() {
        assert_eq!(plat(125474), "125.47p");
        assert_eq!(plat(0), "0.00p");
        assert_eq!(trim2(2.0), "2");
        assert_eq!(trim2(1.5), "1.5");
        assert_eq!(trim2(1.333), "1.33");
    }

    #[test]
    fn the_hidden_note_counts_names_six_and_agrees_in_number() {
        assert_eq!(hidden_text(&[], "learned recipe"), None);
        assert!(
            hidden_text(&["A"], "learned recipe")
                .unwrap()
                .starts_with("1 learned recipe is not attributed")
        );
        let many: Vec<String> = (1..=8).map(|n| format!("r{n}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let t = hidden_text(&many, "costed recipe").unwrap();
        assert!(t.starts_with("8 costed recipes are not attributed to any tradeskill and are hidden while filtering: r1, r2, r3, r4, r5, r6, …."));
    }
}
