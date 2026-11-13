//! Experience: `loadExperience` and `loadClassLevels` (experience.js) and the
//! AA catalogue, `loadAa` (tasks.js). The checklist is the Experience section
//! of the parity inventory.
//!
//! `/api/experience` is fetched when the tab opens and every ten seconds
//! while it is on screen, and `/api/class-levels` with every one of those.
//! `/api/aa` is fetched when the tab opens, when "Unfinished only" changes
//! and with every ten-second refresh. The renderer never refreshes it while
//! the tab sits open; that was an oversight, not a choice.

use crate::api::Api;
use crate::format::{dur, when};
use crate::parts::{
    ROW, ROW_ENEMY, ROW_PET, ROW_YOU, bar_row, card, dim, empty, figure, fill, hint, note, split,
};
use crate::table::{self, Col, col, muted, tag};
use crate::theme::px;
use crate::theme::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Sizable, h_flex, v_flex};
use norrlog_api::{
    AaAbility, AaReport, ClassLevelReport, ExpSource, ExperienceReport, LevelRecord, ZoneShare,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(10);

pub struct ExperienceView {
    api: Option<Api>,
    exp: Option<ExperienceReport>,
    /// A failed `/api/experience`, shown in the levels table; the other
    /// cards keep what they had.
    exp_error: Option<String>,
    levels: Option<ClassLevelReport>,
    aa: Option<AaReport>,
    unfinished_only: bool,
    /// One input per class, by abbreviation, kept across reloads so a value
    /// being typed is not replaced under the cursor.
    inputs: HashMap<String, Entity<InputState>>,
    notes: [bool; 3],
    last_load: Option<Instant>,
    tasks: Vec<Task<()>>,
    subs: Vec<Subscription>,
}

impl ExperienceView {
    pub fn new() -> Self {
        ExperienceView {
            api: None,
            exp: None,
            exp_error: None,
            levels: None,
            aa: None,
            unfinished_only: true,
            inputs: HashMap::new(),
            notes: [false; 3],
            last_load: None,
            tasks: Vec::new(),
            subs: Vec::new(),
        }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.api = api;
        self.last_load = None;
        cx.notify();
    }

    /// The tab was opened: everything, the catalogue included.
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load(window, cx);
        self.load_aa(cx);
    }

    /// A snapshot arrived: refresh if on screen and ten seconds have passed.
    pub fn on_snapshot(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        if visible && self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.load(window, cx);
            // The catalogue too, so an AA bought while the tab is open shows
            // up; the renderer's `VIEW_LOADERS` leaves it out.
            self.load_aa(cx);
        }
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_load = Some(Instant::now());
        let a = api.clone();
        self.track(cx.spawn_in(window, async move |this, cx| {
            let got = a
                .get::<ExperienceReport>("/api/experience".to_string())
                .await;
            let _ = this.update_in(cx, |v, window, cx| {
                match got {
                    Ok(r) => {
                        v.exp = Some(r);
                        v.exp_error = None;
                        // Inside every experience load, as the renderer does.
                        v.load_levels(window, cx);
                    }
                    Err(e) => v.exp_error = Some(e),
                }
                cx.notify();
            });
        }));
    }

    fn load_levels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.track(cx.spawn_in(window, async move |this, cx| {
            // A failure is silent.
            let Ok(r) = api
                .get::<ClassLevelReport>("/api/class-levels".to_string())
                .await
            else {
                return;
            };
            let _ = this.update_in(cx, |v, window, cx| {
                v.sync_inputs(&r, window, cx);
                v.levels = Some(r);
                cx.notify();
            });
        }));
    }

    fn load_aa(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.track(cx.spawn(async move |this, cx| {
            let Ok(r) = api.get::<AaReport>("/api/aa".to_string()).await else {
                return;
            };
            let _ = this.update(cx, |v, cx| {
                v.aa = Some(r);
                cx.notify();
            });
        }));
    }

    fn track(&mut self, t: Task<()>) {
        self.tasks.push(t);
        if self.tasks.len() > 8 {
            self.tasks.drain(..self.tasks.len() - 8);
        }
    }

    /// An input per class, holding the daemon's level unless it is being
    /// typed in.
    fn sync_inputs(&mut self, r: &ClassLevelReport, window: &mut Window, cx: &mut Context<Self>) {
        for c in &r.classes {
            let value = c.level.to_string();
            if let Some(input) = self.inputs.get(&c.abbrev) {
                let typing = input.read(cx).focus_handle(cx).is_focused(window);
                if !typing && input.read(cx).value() != value.as_str() {
                    input.update(cx, |s, cx| s.set_value(value, window, cx));
                }
                continue;
            }
            let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
            let abbrev = c.abbrev.clone();
            self.subs.push(cx.subscribe_in(
                &input,
                window,
                move |v, input, ev: &InputEvent, window, cx| {
                    // The renderer's `change`: when the edit is committed, by
                    // Enter or by leaving the field.
                    if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let typed = input.read(cx).value().trim().parse::<u32>().ok();
                        v.commit_level(&abbrev, typed, window, cx);
                    }
                },
            ));
            self.inputs.insert(c.abbrev.clone(), input);
        }
    }

    /// POST a typed level from 1 to the game's cap (50 Legends, 130 live
    /// EverQuest, from the report), then reload. Anything else is ignored,
    /// as the renderer ignores it.
    fn commit_level(
        &mut self,
        abbrev: &str,
        typed: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cap = self.levels.as_ref().map_or(50, |r| r.cap);
        let Some(level) = typed.filter(|l| (1..=cap).contains(l)) else {
            return;
        };
        let held = self
            .levels
            .as_ref()
            .and_then(|r| r.classes.iter().find(|c| c.abbrev == abbrev))
            .map(|c| c.level);
        if held == Some(level) {
            return;
        }
        let Some(api) = self.api.clone() else { return };
        let body = serde_json::json!({ "class": abbrev, "level": level });
        self.track(cx.spawn_in(window, async move |this, cx| {
            let _ = api.post_json("/api/class-levels".to_string(), body).await;
            let _ = this.update_in(cx, |v, window, cx| v.load_levels(window, cx));
        }));
    }

    fn toggle_note(&mut self, i: usize, cx: &mut Context<Self>) {
        self.notes[i] = !self.notes[i];
        cx.notify();
    }

    fn render_class_levels(&self, cx: &mut Context<Self>) -> Div {
        let r = self.levels.as_ref();
        let rows = match r.filter(|r| !r.classes.is_empty()) {
            None => empty("No class levels established yet."),
            Some(r) => v_flex().gap_1().children(r.classes.iter().map(|c| {
                let state = if c.capped {
                    Some(tag("capped", PET))
                } else if r.levelling.as_deref() == Some(c.abbrev.as_str()) {
                    Some(tag("levelling", HEAL))
                } else {
                    None
                };
                let input = self.inputs.get(&c.abbrev).cloned();
                let source = if c.at != 0 {
                    format!("{}, {}", c.source_label, when(Some(c.at)))
                } else {
                    c.source_label.clone()
                };
                h_flex()
                    .gap_3()
                    .px(px(9.))
                    .py(px(3.))
                    .rounded(px(5.))
                    .bg(hex(PANEL_2))
                    .child(
                        div()
                            .w(px(170.))
                            .flex_none()
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("{} — {}", c.abbrev, c.name)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .w(px(150.))
                            .flex_none()
                            .when_some(input, |d, i| {
                                d.child(div().w(px(64.)).child(Input::new(&i).small()))
                            })
                            .when_some(state, |d, t| d.child(t)),
                    )
                    .child(muted(source))
            })),
        };
        let hint_text = match r {
            Some(r) if r.needs_who => {
                let mut bits = Vec::new();
                if !r.unknown.is_empty() {
                    bits.push(format!("No level known for {}.", r.unknown.join(", ")));
                }
                if r.unattributed > 0 {
                    bits.push(format!(
                        "{} level-up{} could not be pinned to a class.",
                        r.unattributed,
                        if r.unattributed == 1 { "" } else { "s" }
                    ));
                }
                format!(
                    "{} Type /who in game — the row it prints names your level and loadout together, and settles it.",
                    bits.join(" ")
                )
                .trim_start()
                .to_string()
            }
            Some(_) => "Levels come from /who rows, class proficiency achievements, and level-ups attributed to the only \
                        class that could have earned them. Type over any of them to correct it — what you enter is kept \
                        and survives restarts."
                .to_string(),
            None => String::new(),
        };
        let _ = cx;
        card("Class levels")
            .child(rows)
            .when(!hint_text.is_empty(), |d| d.child(hint(hint_text)))
    }

    fn render_stats(&self) -> Div {
        let e = self.exp.as_ref();
        let dash = || "—".to_string();
        // "Current level": the report's, overwritten by the per-class answer.
        let level = match (self.levels.as_ref().and_then(|l| l.effective), e) {
            (Some(eff), _) => match self.levels.as_ref().and_then(|l| l.levelling.clone()) {
                Some(who) => format!("{eff} ({who})"),
                None => eff.to_string(),
            },
            (None, Some(e)) => e.current_level.map_or_else(dash, |l| l.to_string()),
            (None, None) => dash(),
        };
        let big = |label: &str, v: String| {
            stat_card(
                label,
                div().text_2xl().font_weight(FontWeight::BOLD).child(v),
                None,
            )
        };
        let small = |label: &str, v: String| {
            stat_card(
                label,
                div().text_xl().font_weight(FontWeight::BOLD).child(v),
                None,
            )
        };
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_3()
            .child(big("Current level", level))
            .child(big(
                "Progress",
                e.map_or_else(dash, |e| format!("{:.1}%", e.current_percent)),
            ))
            .child(small(
                "Rate",
                e.map_or_else(dash, |e| rate_text(e.rate_per_hour)),
            ))
            .child(small(
                "Time to next level",
                e.map_or_else(dash, |e| {
                    e.seconds_to_next_level.map_or_else(dash, |s| dur(s as f64))
                }),
            ))
            .child(small("Average per level", e.map_or_else(dash, average)))
            .child(big(
                "AA per hour",
                e.map_or_else(dash, |e| format!("{:.2}", aa_rate(e))),
            ))
            .child(stat_card(
                "AA earned",
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .child(e.map_or_else(dash, aa_earned)),
                e.and_then(unlogged),
            ))
            .child(big(
                "Potion charges",
                e.map_or_else(dash, |e| {
                    if e.aa_potion_active {
                        format!("{}/5", e.aa_potion_charges)
                    } else {
                        "none".into()
                    }
                }),
            ))
            .child(small(
                "Doubled coverage",
                e.map_or_else(dash, |e| {
                    format!(
                        "{:.0}% · {:.2}x",
                        e.aa_doubled_share * 100.,
                        e.aa_effective_multiplier
                    )
                }),
            ))
    }

    fn render_potions(&self, cx: &mut Context<Self>) -> Div {
        let mut rows = v_flex().gap_1();
        if let Some(e) = &self.exp {
            let awards = e.aa_gains_doubled + e.aa_gains_normal;
            let bonus = e.aa_earned as i64 - awards as i64;
            rows = rows
                .child(bar_row(
                    e.aa_doubled_share,
                    fill(ROW_YOU),
                    "Doubled awards",
                    h_flex()
                        .gap_3()
                        .child(dim(format!("{} of {awards}", e.aa_gains_doubled)))
                        .child(figure(format!("{:.0}%", e.aa_doubled_share * 100.))),
                ))
                .child(bar_row(
                    1. - e.aa_doubled_share,
                    fill(ROW),
                    "Normal awards",
                    figure(e.aa_gains_normal.to_string()),
                ))
                .child(bar_row(
                    1.,
                    fill(ROW_PET),
                    "Potions consumed",
                    h_flex()
                        .gap_3()
                        .child(dim(format!("{} charges granted", e.aa_potions_used * 5)))
                        .child(figure(e.aa_potions_used.to_string())),
                ))
                .child(bar_row(
                    bonus as f64 / e.aa_earned.max(1) as f64,
                    fill(ROW_YOU),
                    "AA from potions",
                    h_flex()
                        .gap_3()
                        .child(dim(format!("of {} earned", e.aa_earned)))
                        .child(figure(format!("+{bonus}"))),
                ))
                .when(e.aa_doubled_unaccounted > 0, |d| {
                    d.child(bar_row(
                        0.,
                        fill(ROW),
                        div()
                            .text_color(hex(MUTED))
                            .child("Doubled before the log began"),
                        dim(e.aa_doubled_unaccounted.to_string()),
                    ))
                });
        }
        card("Experience potions").child(rows).child(note(
            "x-note-potions",
            self.notes[0],
            "Experience potions are charge-based, not timed: each grants five doubled AA awards, and charges stack if \
             you drink another before the first is spent. The client logs no duration or expiry, so charges are \
             counted from potions consumed and spent against doubled awards — a normal (undoubled) award proves the \
             potion has lapsed and clears any remainder.",
            cx.listener(|v, _, _, cx| v.toggle_note(0, cx)),
        ))
    }

    fn render_bar(&self, cx: &mut Context<Self>) -> Div {
        let row = self.exp.as_ref().map(|e| {
            let pct = e.current_percent.clamp(0., 100.);
            // No level-cap handling, as in the renderer: a capped
            // character still reads "→ N+1".
            let label = format!(
                "Level {} → {}",
                e.current_level
                    .map_or_else(|| "?".to_string(), |l| l.to_string()),
                e.current_level.unwrap_or(0) + 1
            );
            bar_row(
                pct / 100.,
                fill(ROW_YOU),
                div().text_color(hex(YOU)).child(label),
                h_flex()
                    .gap_3()
                    .child(dim(format!("{} kills", e.current_kills)))
                    .child(figure(format!("{pct:.1}%"))),
            )
        });
        card("Progress through this level").children(row).child(note(
            "x-note-bar",
            self.notes[1],
            "EverQuest reports experience only as a percentage of the level you are in, so everything here is in \
             \"% of a level\". That stays meaningful at any level, but the underlying amounts are not comparable \
             across levels — 1% at 50 is far more raw experience than 1% at 10.",
            cx.listener(|v, _, _, cx| v.toggle_note(1, cx)),
        ))
    }

    fn render_lists(&self) -> Div {
        let e = self.exp.as_ref();
        let none: &[ExpSource] = &[];
        let zones = e.map_or(none, |e| &e.best_zones[..]);
        let mobs = e.map_or(none, |e| &e.best_mobs[..]);
        let aa_zones = e.map_or(none, |e| &e.aa_zones[..]);

        let zmax = zones.iter().map(xp_rate).fold(1., f64::max);
        let zone_rows = list_or(zones.is_empty(), "Not enough time in any zone yet", || {
            zones.iter().take(10).map(|z| {
                bar_row(
                    xp_rate(z) / zmax,
                    fill(ROW),
                    z.name.clone(),
                    h_flex()
                        .gap_3()
                        .child(dim(format!(
                            "{} kills · {}",
                            z.kills,
                            dur(z.seconds as f64)
                        )))
                        .child(per(format!("{:.1}", xp_rate(z)), "%/hr")),
                )
            })
        });
        let mmax = mobs.iter().map(per_kill).fold(0.01, f64::max);
        let mob_rows = list_or(mobs.is_empty(), "No mobs killed often enough yet", || {
            mobs.iter().take(10).map(|m| {
                bar_row(
                    per_kill(m) / mmax,
                    fill(ROW_ENEMY),
                    m.name.clone(),
                    h_flex()
                        .gap_3()
                        .child(dim(format!("x{}", m.kills)))
                        .child(per(format!("{:.2}", per_kill(m)), "%/kill")),
                )
            })
        });
        let amax = aa_zones
            .iter()
            .map(|z| z.aa as f64 / z.seconds.max(1) as f64)
            .fold(1e-9, f64::max);
        let aa_rows = list_or(aa_zones.is_empty(), "No AA earned yet", || {
            aa_zones.iter().take(10).map(|z| {
                let r = z.aa as f64 * 3600. / z.seconds.max(1) as f64;
                bar_row(
                    z.aa as f64 / z.seconds.max(1) as f64 / amax,
                    fill(ROW),
                    z.name.clone(),
                    h_flex()
                        .gap_3()
                        .child(dim(format!("{} AA · {}", z.aa, dur(z.seconds as f64))))
                        .child(per(format!("{r:.2}"), "AA/hr")),
                )
            })
        });
        let bought = e.map_or(&[][..], |e| &e.aa_purchases[..]);
        let smax = bought.iter().map(|p| p.spent).max().unwrap_or(1).max(1) as f64;
        let spent_rows = list_or(bought.is_empty(), "Nothing purchased yet", || {
            bought.iter().take(12).map(|p| {
                let mut name = h_flex().min_w_0().gap(px(5.)).child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(p.ability.clone()),
                );
                match p.enabled {
                    Some(true) => name = name.child(tag("on", HEAL).flex_none()),
                    Some(false) => name = name.child(tag("off", PET).flex_none()),
                    None => {}
                }
                let ladder = if p.client_rank != 0 && p.client_rank != p.rank {
                    format!(" (ladder {})", p.client_rank)
                } else {
                    String::new()
                };
                bar_row(
                    p.spent as f64 / smax,
                    fill(ROW_PET),
                    name,
                    h_flex()
                        .gap_3()
                        .child(dim(format!("rank {}{ladder}", p.rank)))
                        .child(per(p.spent.to_string(), "pts")),
                )
            })
        });
        v_flex()
            .gap_3()
            .child(split(
                card("Best zones (experience per hour)").child(zone_rows),
                card("Best mobs (experience per kill)").child(mob_rows),
            ))
            .child(split(
                card("Best zones (AA per hour)").child(aa_rows),
                card("Ability points spent").child(spent_rows),
            ))
    }

    fn render_levels(&self) -> Div {
        let mut body = v_flex();
        if let Some(err) = &self.exp_error {
            body = body.child(table::empty_row(err.clone()));
        } else {
            let levels = self.exp.as_ref().map_or(&[][..], |e| &e.levels[..]);
            if levels.is_empty() {
                body = body.child(table::empty_row("No levels recorded yet"));
            }
            // Newest first.
            for (i, l) in levels.iter().rev().enumerate() {
                body = body.child(level_row(i, l));
            }
        }
        v_flex()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(BORDER))
            .bg(hex(PANEL))
            .overflow_hidden()
            .child(table::head(LEVEL_COLS))
            .child(
                div()
                    .id("x-levels")
                    .max_h(px(420.))
                    .overflow_y_scrollbar()
                    .child(body),
            )
    }

    fn render_aa(&self, cx: &mut Context<Self>) -> Div {
        let r = self.aa.as_ref();
        let summary = match r {
            None => String::new(),
            Some(r) if r.abilities.is_empty() => " — run /alt list in game to populate".into(),
            Some(r) => aa_summary(r),
        };
        let title = h_flex()
            .gap_1()
            .items_baseline()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(hex(MUTED))
                    .child("AA CATALOGUE"),
            )
            .child(div().text_sm().text_color(hex(MUTED)).child(summary));
        let unfinished = self.unfinished_only;
        let toolbar = Checkbox::new("aa-unfinished")
            .label("Unfinished only")
            .checked(unfinished)
            .on_click(cx.listener(|v, checked: &bool, _, cx| {
                v.unfinished_only = *checked;
                v.load_aa(cx);
                cx.notify();
            }));
        let rows = match r {
            None => v_flex(),
            Some(r) if r.abilities.is_empty() => empty("No /alt list dump seen in this log yet"),
            Some(r) => v_flex().gap_1().children(
                r.abilities
                    .iter()
                    .filter(|a| !unfinished || a.cost_to_finish != Some(0))
                    .enumerate()
                    .map(|(i, a)| aa_row(i, a)),
            ),
        };
        v_flex()
            .gap_2()
            .p_3()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(BORDER))
            .bg(hex(PANEL))
            .child(title)
            .child(toolbar)
            .child(rows)
            .child(note(
                "x-note-aa",
                self.notes[2],
                "Read from /alt list in game — run it to populate or refresh this. Repeated entries in that dump are \
                 successive ranks, so the per-rank costs here are the game's own. Every dump observed stops at exactly \
                 100 entries, so the catalogue is a prefix of the real one: abilities marked ladder cut short are ones \
                 you already own past what the dump listed, and no \"cost to finish\" is claimed for them. Running \
                 /alt list again merges in anything new rather than replacing what we have.",
                cx.listener(|v, _, _, cx| v.toggle_note(2, cx)),
            ))
    }
}

impl Render for ExperienceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = v_flex()
            .gap_3()
            .child(self.render_class_levels(cx))
            .child(self.render_stats())
            .child(self.render_potions(cx))
            .child(self.render_bar(cx))
            .child(self.render_lists())
            .child(self.render_levels())
            .child(self.render_aa(cx));
        div()
            .id("experience-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(page)
    }
}

const LEVEL_COLS: &[Col] = &[
    col("Level", Some(90.), false),
    col("Loadout", Some(120.), false),
    col("Where", None, false),
    col("Time", Some(64.), true),
    col("Kills", Some(48.), true),
    col("% / kill", Some(60.), true),
    col("% / hour", Some(64.), true),
    col("XP events", Some(72.), true),
];

fn level_row(i: usize, l: &LevelRecord) -> AnyElement {
    let ppk = if l.kills > 0 {
        l.percent / l.kills as f64
    } else {
        0.
    };
    let pph = if l.seconds > 0 {
        l.percent * 3600. / l.seconds as f64
    } else {
        0.
    };
    let label = l.level.map_or_else(|| "?".to_string(), |n| n.to_string());
    let level = h_flex()
        .gap_1()
        .child(label)
        .when(!l.complete, |d| d.child(muted("(partial)")));
    // In a flex row, so the tag keeps its own width rather than the column's.
    let loadout: AnyElement = if l.build.is_empty() {
        muted("—").into_any_element()
    } else {
        h_flex()
            .child(tag(l.build.clone(), ACCENT).ml_0())
            .into_any_element()
    };
    let (shown, all) = zone_cell(&l.zones);
    let r#where = div()
        .id(SharedString::from(format!("x-where-{i}")))
        .text_color(hex(MUTED))
        .overflow_hidden()
        .text_ellipsis()
        .child(shown)
        .when_some(all, |d, all| {
            d.tooltip(move |window, cx| Tooltip::new(all.clone()).build(window, cx))
        });
    table::row(
        SharedString::from(format!("x-level-{i}")),
        LEVEL_COLS,
        vec![
            level.into_any_element(),
            loadout,
            r#where.into_any_element(),
            div().child(dur(l.seconds as f64)).into_any_element(),
            div().child(l.kills.to_string()).into_any_element(),
            div()
                .child(if ppk > 0. {
                    format!("{ppk:.2}")
                } else {
                    "—".into()
                })
                .into_any_element(),
            div().child(format!("{pph:.1}")).into_any_element(),
            muted(l.events.to_string()).into_any_element(),
        ],
    )
    .into_any_element()
}

/// `zoneCell`: the top two zones with their share of the level, then `+N`;
/// and every zone, one per line, for the hover.
fn zone_cell(zones: &[ZoneShare]) -> (String, Option<String>) {
    if zones.is_empty() {
        return ("—".into(), None);
    }
    let total: f64 = zones.iter().map(|z| z.percent).sum::<f64>();
    let total = if total == 0. { 1. } else { total };
    let top: Vec<String> = zones
        .iter()
        .take(2)
        .map(|z| format!("{} {:.0}%", z.zone, (z.percent / total * 100.).round()))
        .collect();
    let rest = if zones.len() > 2 {
        format!(" +{}", zones.len() - 2)
    } else {
        String::new()
    };
    let all = zones
        .iter()
        .map(|z| format!("{}: {:.1}%", z.zone, z.percent))
        .collect::<Vec<_>>()
        .join("\n");
    (format!("{}{rest}", top.join(", ")), Some(all))
}

fn aa_row(i: usize, a: &AaAbility) -> AnyElement {
    let listed = a.ranks.len();
    let share = if a.truncated {
        0.
    } else {
        a.owned_rank as f64 / listed.max(1) as f64
    };
    let held = if a.is_toggle {
        a.real_owned_rank
    } else {
        a.owned_rank
    };
    let denom = if a.truncated {
        "?".to_string()
    } else if a.is_toggle {
        listed.div_ceil(2).to_string()
    } else {
        listed.to_string()
    };
    let note: AnyElement = if a.truncated {
        tag("ladder cut short", ACCENT).into_any_element()
    } else if a.cost_to_finish == Some(0) {
        dim("complete").into_any_element()
    } else {
        dim(format!("{} pts to finish", a.cost_to_finish.unwrap_or(0))).into_any_element()
    };
    // The next rank not owned, or the last when it is complete.
    let next = a
        .ranks
        .iter()
        .find(|r| r.rank > a.owned_rank)
        .or(a.ranks.last())
        .map(|r| r.description.clone())
        .unwrap_or_default();
    let mut name = h_flex().min_w_0().gap(px(5.)).child(
        div()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .child(a.name.clone()),
    );
    if a.is_toggle {
        let why = format!(
            "One ability the client splits into ': Enabled' and ': Disabled' halves sharing a ladder of {}",
            a.owned_rank
        );
        name = name.child(
            div()
                .id(SharedString::from(format!("aa-toggle-{i}")))
                .flex_none()
                .child(tag("toggle", ACCENT))
                .tooltip(move |window, cx| Tooltip::new(why.clone()).build(window, cx)),
        );
    }
    name = name.child(dim(format!("#{}", a.id)).flex_none());
    div()
        .id(SharedString::from(format!("aa-{i}")))
        .child(bar_row(
            share,
            fill(ROW),
            name,
            h_flex()
                .gap_3()
                .child(note)
                .child(figure(format!("{held}/{denom}"))),
        ))
        .when(!next.is_empty(), |d| {
            d.tooltip(move |window, cx| Tooltip::new(next.clone()).build(window, cx))
        })
        .into_any_element()
}

/// ` — N abilities · C points to finish what is listed`, and what is missing.
fn aa_summary(r: &AaReport) -> String {
    let cut = r.abilities.iter().filter(|a| a.truncated).count();
    let mut s = format!(
        " — {} abilities · {} points to finish what is listed",
        r.abilities.len(),
        r.visible_cost_to_finish
    );
    if r.dump_probably_truncated {
        s.push_str(&format!(
            " · dump capped at {}, catalogue incomplete",
            r.last_dump_entries
        ));
    }
    if cut > 0 {
        s.push_str(&format!(" · {cut} ladder(s) cut short"));
    }
    s
}

fn rate_text(per_hour: f64) -> String {
    if per_hour > 0. {
        format!("{per_hour:.1} %/hr")
    } else {
        "idle".into()
    }
}

fn average(e: &ExperienceReport) -> String {
    match e.average_seconds_per_level.filter(|s| *s != 0) {
        Some(s) => format!(
            "{} · {} kills",
            dur(s as f64),
            e.average_kills_per_level.unwrap_or(0.).round()
        ),
        None => "—".into(),
    }
}

/// The recent window's rate, or the lifetime one when that is nothing;
/// nothing says which, as in the renderer.
fn aa_rate(e: &ExperienceReport) -> f64 {
    if e.aa_per_hour > 0. {
        e.aa_per_hour
    } else {
        e.aa_lifetime_per_hour
    }
}

fn aa_earned(e: &ExperienceReport) -> String {
    format!(
        "{} · {} unspent · {} spent in log",
        e.aa_earned, e.aa_unspent, e.aa_spent
    )
}

/// The ranks held but never seen bought, which the log can name exactly.
fn unlogged(e: &ExperienceReport) -> Option<String> {
    if e.aa_unlogged.is_empty() {
        return None;
    }
    let list = e
        .aa_unlogged
        .iter()
        .map(|a| {
            let ranks: Vec<String> = a.unlogged_ranks.iter().map(|r| r.to_string()).collect();
            format!(
                "{} rank {} (holds {}, at most {})",
                a.ability,
                ranks.join(", "),
                a.rank,
                a.unlogged_cost_max
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "Bought before this log: {list}. At most {} points unaccounted.",
        e.aa_unlogged_cost_max
    ))
}

fn xp_rate(z: &ExpSource) -> f64 {
    if z.seconds > 0 {
        z.percent * 3600. / z.seconds as f64
    } else {
        0.
    }
}

fn per_kill(m: &ExpSource) -> f64 {
    m.percent / m.kills.max(1) as f64
}

/// A figure with its unit dimmed after it: `12.3 %/hr`.
fn per(value: String, unit: &'static str) -> Div {
    h_flex()
        .gap(px(3.))
        .items_baseline()
        .child(figure(value))
        .child(dim(unit).text_sm())
}

fn list_or<I: IntoIterator<Item = Div>>(
    is_empty: bool,
    empty_text: &'static str,
    rows: impl FnOnce() -> I,
) -> Div {
    if is_empty {
        empty(empty_text)
    } else {
        v_flex().gap_1().children(rows())
    }
}

fn stat_card(label: &str, value: impl IntoElement, note: Option<String>) -> Div {
    v_flex()
        .min_w(px(208.))
        .flex_1()
        .gap_1()
        .p_3()
        .rounded(px(8.))
        .border_1()
        .border_color(hex(BORDER))
        .bg(hex(PANEL))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(hex(MUTED))
                .child(label.to_uppercase()),
        )
        .child(value)
        .when_some(note, |d, n| d.child(hint(n)))
}

#[cfg(test)]
mod tests {
    use super::{aa_summary, rate_text, zone_cell};
    use norrlog_api::{AaReport, ZoneShare};

    #[test]
    fn a_level_names_its_top_two_zones_and_counts_the_rest() {
        let z = |zone: &str, percent: f64| ZoneShare {
            zone: zone.into(),
            percent,
        };
        let (shown, all) = zone_cell(&[z("Hate", 30.), z("Fear", 10.), z("Sky", 5.), z("Ro", 5.)]);
        assert_eq!(shown, "Hate 60%, Fear 20% +2");
        assert_eq!(
            all.as_deref(),
            Some("Hate: 30.0%\nFear: 10.0%\nSky: 5.0%\nRo: 5.0%")
        );
        assert_eq!(zone_cell(&[]), ("—".to_string(), None));
    }

    #[test]
    fn no_experience_reads_idle() {
        assert_eq!(rate_text(0.), "idle");
        assert_eq!(rate_text(39.99), "40.0 %/hr");
    }

    #[test]
    fn the_catalogue_summary_says_what_is_missing() {
        let r: AaReport = serde_json::from_str(
            r#"{"abilities":[{"id":1,"name":"A","ranks":[],"owned_rank":3,"spent":0,"cost_to_finish":null,"truncated":true}],
                "last_dump_entries":100,"dump_probably_truncated":true,"undescribed":[],"visible_cost_to_finish":12}"#,
        )
        .unwrap();
        assert_eq!(
            aa_summary(&r),
            " — 1 abilities · 12 points to finish what is listed · dump capped at 100, catalogue incomplete · 1 ladder(s) cut short"
        );
    }
}
