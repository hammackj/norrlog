//! Bestiary: every creature the logs on file have fought, per zone variant.
//!
//! `/api/bestiary` is fetched when the tab opens and every thirty seconds
//! while it is on screen. It is merged across every log the daemon has
//! stored for the game, so it is everyone's record, not only the log being
//! followed.
//!
//! Three things are kept apart on purpose, because the data is shaped that
//! way: one creature at two tiers is two entries; health is a spread with its
//! sample count beside it; and melee stays split by the character, level and
//! stance it was observed against.

use crate::api::Api;
use crate::format::{num, when};
use crate::parts::{card, empty, hint, note, stat};
use crate::table::{Col, col, head_dense, muted, row_dense, tag};
use crate::theme::px;
use crate::theme::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{Sizable, h_flex, v_flex};
use norrlog_api::{BestiaryReport, MeleeAgainst, NpcProfile, TIER_NAMES};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(30);

/// Rows drawn in the list before it asks for a search. The report runs to
/// thousands on a long history, and nobody scrolls that far.
const SHOWN: usize = 150;

pub struct BestiaryView {
    api: Option<Api>,
    report: Option<BestiaryReport>,
    search: Entity<InputState>,
    /// The open entry, by [`entry_key`]. In memory only.
    open: Option<String>,
    note_open: bool,
    last_load: Option<Instant>,
    loading: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl BestiaryView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Creature or zone…"));
        let subs = vec![cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                cx.notify();
            }
        })];
        BestiaryView {
            api: None,
            report: None,
            search,
            open: None,
            note_open: false,
            last_load: None,
            loading: None,
            _subs: subs,
        }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        if api.is_none() {
            self.report = None;
        }
        self.api = api;
        self.last_load = None;
        cx.notify();
    }

    pub fn on_snapshot(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.load(cx);
        }
    }

    /// A failure leaves the page as it was.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_load = Some(Instant::now());
        self.loading = Some(cx.spawn(async move |this, cx| {
            let Ok(r) = api.get::<BestiaryReport>("/api/bestiary".to_string()).await else {
                return;
            };
            let _ = this.update(cx, |v, cx| {
                v.report = Some(r);
                cx.notify();
            });
        }));
    }

    fn select(&mut self, key: String, cx: &mut Context<Self>) {
        self.open = if self.open.as_deref() == Some(key.as_str()) {
            None
        } else {
            Some(key)
        };
        cx.notify();
    }
}

const NOTE: &str = "Merged across every log on file for this game, so a creature someone else's character fought is \
    here too. One creature in two places, or at two tiers of one instance, is two entries: they are different fights, \
    and pooling them would describe neither.\n\nHealth is the damage a creature absorbed between being first hit and \
    dying, over every death that was seen whole. It is shown as a median with the lowest and highest beside it and \
    the number of deaths it came from; a low figure among them is usually a creature someone else had already \
    damaged, which is why the median and not the mean.\n\nMelee is split by the character it hit, that character's \
    level and the stance they fought in. A hit rate is as much your armour and stance as the creature's accuracy, so \
    rates from different pairings are never added together. A stance of unknown means the log had not said; it is \
    not the same as balanced. Avoided is your dodge, parry, riposte and block, kept apart from a plain miss.\n\nA \
    body type appears only when the game proved it: a spell that lands only on undead, summoned creatures, animals or \
    plants landed on it.";

impl Render for BestiaryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let needle = self.search.read(cx).value().trim().to_lowercase();
        let npcs: &[NpcProfile] = self.report.as_ref().map_or(&[], |r| &r.npcs);
        let matched: Vec<&NpcProfile> = npcs.iter().filter(|n| matches(n, &needle)).collect();
        let open = self
            .open
            .as_deref()
            .and_then(|k| npcs.iter().find(|n| entry_key(n) == k));
        // The list and the profile scroll apart, so a creature picked far
        // down the list opens beside it rather than off the top of the page.
        let note = note(
            "bestiary-note",
            self.note_open,
            NOTE,
            cx.listener(|v, _, _, cx| {
                v.note_open = !v.note_open;
                cx.notify();
            }),
        );
        v_flex()
            .size_full()
            .gap_3()
            .child(
                card("Bestiary").flex_none().child(
                    h_flex()
                        .gap_3()
                        .flex_wrap()
                        .items_center()
                        .child(div().w(px(260.)).child(Input::new(&self.search).small()))
                        .child(hint(match &self.report {
                            Some(r) => summary(r, matched.len(), &needle),
                            None => "Loading…".into(),
                        })),
                ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .gap_3()
                    .child(
                        div()
                            .id("bestiary-list")
                            .flex_1()
                            .min_w_0()
                            .max_h_full()
                            .overflow_y_scrollbar()
                            .child(card("Creatures").child(self.render_list(&matched, cx))),
                    )
                    .child(
                        div()
                            .id("bestiary-profile")
                            .flex_1()
                            .min_w_0()
                            .max_h_full()
                            .overflow_y_scrollbar()
                            .child(
                                v_flex()
                                    .gap_3()
                                    .child(card("Profile").child(match open {
                                        Some(n) => self.render_profile(n, npcs, cx),
                                        None => hint(
                                            "Choose a creature to see what has been measured of it.",
                                        ),
                                    }))
                                    .child(note),
                            ),
                    ),
            )
    }
}

impl BestiaryView {
    fn render_list(&self, npcs: &[&NpcProfile], cx: &mut Context<Self>) -> Div {
        if npcs.is_empty() {
            return empty(
                if self.report.as_ref().is_some_and(|r| !r.npcs.is_empty()) {
                    "Nothing matches that search."
                } else {
                    "No creature has been fought yet."
                },
            );
        }
        v_flex()
            .gap_1()
            .children(npcs.iter().take(SHOWN).enumerate().map(|(i, n)| {
                let key = entry_key(n);
                let on = self.open.as_deref() == Some(key.as_str());
                h_flex()
                    .id(SharedString::from(format!("bestiary-row-{i}")))
                    .gap_3()
                    .px(px(9.))
                    .py(px(5.))
                    .rounded(px(5.))
                    .bg(hex(PANEL_2))
                    .cursor_pointer()
                    .when(on, |d| d.border_1().border_color(hex(ACCENT)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_weight(FontWeight::BOLD).child(n.name.clone()))
                            .child(div().text_xs().text_color(hex(MUTED)).child(place(n))),
                    )
                    .child(
                        v_flex()
                            .flex_none()
                            .items_end()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(kills(n.kills)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hex(MUTED))
                                    .child(health_short(n)),
                            ),
                    )
                    .on_click(cx.listener(move |v, _, _, cx| v.select(key.clone(), cx)))
            }))
            .when(npcs.len() > SHOWN, |d| {
                d.child(hint(format!(
                    "{} more; search to narrow the list.",
                    npcs.len() - SHOWN
                )))
            })
    }

    fn render_profile(&self, n: &NpcProfile, all: &[NpcProfile], cx: &mut Context<Self>) -> Div {
        let elsewhere: Vec<&NpcProfile> = all
            .iter()
            .filter(|o| o.identity() == n.identity() && entry_key(o) != entry_key(n))
            .collect();
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(n.name.clone()),
                            )
                            .when_some(n.kind, |d, k| d.child(tag(k.as_str(), ACCENT))),
                    )
                    .child(hint(place(n)))
                    .child(hint(format!(
                        "Seen {} to {}",
                        when(Some(n.first_seen)),
                        when(Some(n.last_seen))
                    ))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .items_stretch()
                    .child(stat("Health", health_figure(n), Some(health_spread(n))))
                    .child(stat("Killed", n.kills.to_string(), None))
                    .child(stat("Killed you", n.deaths_to_it.to_string(), None))
                    .child(stat(
                        "Largest hit",
                        if n.max_hit == 0 {
                            "—".into()
                        } else {
                            num(Some(n.max_hit as f64))
                        },
                        None,
                    )),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Its melee"))
                    .child(melee_table(&n.melee)),
            )
            .when(!elsewhere.is_empty(), |d| {
                d.child(
                    v_flex()
                        .gap_1()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Also seen"))
                        .children(elsewhere.into_iter().enumerate().map(|(i, o)| {
                            let key = entry_key(o);
                            h_flex()
                                .id(SharedString::from(format!("bestiary-else-{i}")))
                                .gap_2()
                                .cursor_pointer()
                                .text_color(hex(ACCENT))
                                .child(place(o))
                                .child(muted(format!("{}, {}", kills(o.kills), health_short(o))))
                                .on_click(cx.listener(move |v, _, _, cx| {
                                    v.open = Some(key.clone());
                                    cx.notify();
                                }))
                        })),
                )
            })
    }
}

const MELEE_COLS: &[Col] = &[
    col("Against", None, false),
    col("Level", Some(44.), true),
    col("Stance", Some(80.), false),
    col("Swings", Some(56.), true),
    col("Hit", Some(44.), true),
    col("Missed", Some(56.), true),
    col("Avoided", Some(62.), true),
    col("Avg", Some(44.), true),
    col("Max", Some(44.), true),
];

/// One row per (character, level, stance), as observed. Never summed.
fn melee_table(rows: &[MeleeAgainst]) -> Div {
    if rows.is_empty() {
        return hint("It has not swung at anyone in a log on file.");
    }
    let mut rows: Vec<&MeleeAgainst> = rows.iter().collect();
    rows.sort_by(|a, b| {
        a.character
            .cmp(&b.character)
            .then(a.level.cmp(&b.level))
            .then(a.stance.cmp(&b.stance))
    });
    let texts: Vec<Vec<String>> = rows.iter().map(|m| melee_cells(m)).collect();
    let cols = crate::table::fit(MELEE_COLS, &texts);
    v_flex()
        .child(head_dense(&cols))
        .children(texts.into_iter().enumerate().map(|(i, t)| {
            let cells = t
                .into_iter()
                .enumerate()
                .map(|(c, s)| {
                    // An unknown level or stance is said, dimly, not left blank.
                    if c == 1 && s == "?" || c == 2 && s == "unknown" {
                        muted(s).into_any_element()
                    } else {
                        div().child(s).into_any_element()
                    }
                })
                .collect();
            row_dense(
                SharedString::from(format!("bestiary-melee-{i}")),
                &cols,
                cells,
            )
        }))
}

fn melee_cells(m: &MeleeAgainst) -> Vec<String> {
    vec![
        m.character.clone(),
        if m.level == 0 {
            "?".into()
        } else {
            m.level.to_string()
        },
        if m.stance.is_empty() {
            "unknown".into()
        } else {
            m.stance.clone()
        },
        m.swings.to_string(),
        percent(m.hits, m.swings),
        m.misses.to_string(),
        m.avoided.to_string(),
        if m.hits == 0 {
            "—".into()
        } else {
            format!("{:.0}", m.damage as f64 / m.hits as f64)
        },
        if m.max_hit == 0 {
            "—".into()
        } else {
            m.max_hit.to_string()
        },
    ]
}

/// What tells one entry from another: the creature and the zone variant.
fn entry_key(n: &NpcProfile) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}",
        n.identity(),
        n.zone,
        n.mode.as_str(),
        n.difficulty
    )
}

/// The zone, and for an instance its mode and tier: "Nagafen's Lair · Solo,
/// Refined". The world zone is just its name.
fn place(n: &NpcProfile) -> String {
    let zone = if n.zone.is_empty() {
        "Unknown zone"
    } else {
        n.zone.as_str()
    };
    match tier_label(n.mode.as_str(), n.difficulty) {
        Some(t) => format!("{zone} · {t}"),
        None => zone.to_string(),
    }
}

/// An instance's mode and tier as words, "Solo, Refined"; `None` for the
/// open world, or for a mode nobody recorded. Shared with the loot drill-down,
/// which splits its sources the same way.
pub(crate) fn tier_label(mode: &str, difficulty: u8) -> Option<String> {
    let mode = match mode {
        "solo" => Some("Solo"),
        "group" => Some("Group"),
        "instance" => Some("Instance"),
        _ => None,
    };
    let tier = TIER_NAMES
        .iter()
        .find(|t| t.0 == difficulty)
        .map(|t| t.1.to_string())
        .or_else(|| (difficulty > 0).then(|| format!("tier {difficulty}")));
    match (mode, tier) {
        (None, None) => None,
        (Some(m), None) => Some(m.to_string()),
        (None, Some(t)) => Some(t),
        (Some(m), Some(t)) => Some(format!("{m}, {t}")),
    }
}

fn matches(n: &NpcProfile, needle: &str) -> bool {
    needle.is_empty()
        || n.name.to_lowercase().contains(needle)
        || n.zone.to_lowercase().contains(needle)
}

fn kills(k: u64) -> String {
    format!("{k} kill{}", if k == 1 { "" } else { "s" })
}

/// The median, or `—` when no death was measured.
fn health_figure(n: &NpcProfile) -> String {
    if n.health_samples == 0 {
        "—".into()
    } else {
        format!("{} HP", num(Some(n.health_median as f64)))
    }
}

/// The spread and its denominator, under the figure.
fn health_spread(n: &NpcProfile) -> String {
    let s = n.health_samples;
    if s == 0 {
        return "no death measured".into();
    }
    let from = format!("from {s} death{}", if s == 1 { "" } else { "s" });
    if n.health_min == n.health_max {
        from
    } else {
        format!(
            "{}–{}, {from}",
            num(Some(n.health_min as f64)),
            num(Some(n.health_max as f64))
        )
    }
}

/// The list's second figure: the median and how many deaths it rests on.
fn health_short(n: &NpcProfile) -> String {
    if n.health_samples == 0 {
        "health unmeasured".into()
    } else {
        format!(
            "~{} HP ({})",
            num(Some(n.health_median as f64)),
            n.health_samples
        )
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        "—".into()
    } else {
        format!("{:.0}%", part as f64 / whole as f64 * 100.)
    }
}

fn summary(r: &BestiaryReport, matched: usize, needle: &str) -> String {
    let total = r.npcs.len();
    if total == 0 {
        return "No creature has been fought yet.".into();
    }
    let kills: u64 = r.npcs.iter().map(|n| n.kills).sum();
    let s = |n: usize| if n == 1 { "" } else { "s" };
    if needle.is_empty() {
        format!(
            "{total} creature{} across every log on file, {} kill{} between them. Most killed first.",
            s(total),
            num(Some(kills as f64)),
            s(kills as usize)
        )
    } else {
        format!("{matched} of {total} match.")
    }
}

#[cfg(test)]
mod tests {
    use super::{entry_key, health_short, health_spread, melee_cells, place};
    use norrlog_api::{Game, MeleeAgainst, Mode, NpcProfile};

    fn npc(zone: &str, mode: Mode, difficulty: u8) -> NpcProfile {
        NpcProfile {
            name: "a fire goblin".into(),
            key: "fire goblin".into(),
            zone: zone.into(),
            mode,
            difficulty,
            game: Game::default(),
            kind: None,
            kills: 3,
            deaths_to_it: 0,
            health_samples: 0,
            health_min: 0,
            health_median: 0,
            health_max: 0,
            max_hit: 0,
            melee: vec![],
            casts: vec![],
            first_seen: 0,
            last_seen: 0,
        }
    }

    #[test]
    fn a_zone_variant_names_its_mode_and_tier() {
        assert_eq!(place(&npc("Befallen", Mode::Open, 0)), "Befallen");
        assert_eq!(
            place(&npc("Nagafen's Lair", Mode::Solo, 4)),
            "Nagafen's Lair · Solo, Refined"
        );
        assert_eq!(
            place(&npc("Nagafen's Lair", Mode::Instance, 1)),
            "Nagafen's Lair · Instance, Awakened"
        );
        assert_eq!(place(&npc("", Mode::Group, 0)), "Unknown zone · Group");
    }

    #[test]
    fn two_tiers_of_one_creature_are_two_entries() {
        let a = npc("Nagafen's Lair", Mode::Solo, 1);
        let b = npc("Nagafen's Lair", Mode::Solo, 4);
        assert_ne!(entry_key(&a), entry_key(&b));
        assert_eq!(a.identity(), b.identity());
    }

    #[test]
    fn health_never_shows_without_its_denominator() {
        let mut n = npc("Befallen", Mode::Open, 0);
        assert_eq!(health_spread(&n), "no death measured");
        assert_eq!(health_short(&n), "health unmeasured");
        n.health_samples = 61;
        n.health_min = 1;
        n.health_median = 29;
        n.health_max = 334;
        assert_eq!(health_spread(&n), "1–334, from 61 deaths");
        assert_eq!(health_short(&n), "~29 HP (61)");
        n.health_samples = 1;
        n.health_min = 29;
        n.health_max = 29;
        assert_eq!(health_spread(&n), "from 1 death");
    }

    #[test]
    fn an_unknown_stance_and_level_say_so() {
        let m = MeleeAgainst {
            character: "Tharn".into(),
            level: 0,
            stance: String::new(),
            swings: 4,
            hits: 3,
            misses: 1,
            avoided: 0,
            damage: 30,
            max_hit: 12,
        };
        assert_eq!(
            melee_cells(&m),
            ["Tharn", "?", "unknown", "4", "75%", "1", "0", "10", "12"]
        );
    }
}
