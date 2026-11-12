//! Forage: `loadForage` and `draw` (forage.js). The checklist is the Forage
//! section of `docs/parity/tradeskills-and-forage.md`.
//!
//! `/api/forage` is fetched when the tab opens and every ten seconds while it
//! is on screen. Clicking a zone narrows "What it yields" to it.

use crate::api::Api;
use crate::parts::{card, hint, note, split};
use crate::theme::*;
use norrlog_api::{ForageReport, ForageZoneRow};
use gpui::prelude::FluentBuilder;
use gpui::*;
use crate::theme::px;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::button::Button;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Sizable, h_flex, v_flex};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(10);

pub struct ForageView {
    api: Option<Api>,
    report: Option<ForageReport>,
    /// The zone "What it yields" is narrowed to. In memory only.
    open: Option<String>,
    note_open: bool,
    last_load: Option<Instant>,
    loading: Option<Task<()>>,
}

impl ForageView {
    pub fn new() -> Self {
        ForageView { api: None, report: None, open: None, note_open: false, last_load: None, loading: None }
    }

    pub fn set_api(&mut self, api: Option<Api>, cx: &mut Context<Self>) {
        self.api = api;
        self.last_load = None;
        cx.notify();
    }

    pub fn on_snapshot(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.load(cx);
        }
    }

    /// A failure leaves the page as it was: `loadForage` has no catch.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        self.last_load = Some(Instant::now());
        self.loading = Some(cx.spawn(async move |this, cx| {
            let Ok(r) = api.get::<ForageReport>("/api/forage".to_string()).await else { return };
            let _ = this.update(cx, |v, cx| {
                v.report = Some(r);
                cx.notify();
            });
        }));
    }

    fn select(&mut self, zone: Option<String>, cx: &mut Context<Self>) {
        self.open = if self.open == zone { None } else { zone };
        cx.notify();
    }
}

const NOTE: &str = "Read from \"You have scrounged up <item>.\" and \"You fail to locate any food nearby.\" The line \
    names the item and nothing else, so the zone is whichever one the log last announced — the same rule drops and \
    creature spots use.\n\nFailures are counted because they are the denominator. A zone whose row says nothing, in 4 \
    attempts was foraged in and gave up nothing, which is a statement about the zone; without the count it would be \
    indistinguishable from a zone nobody has tried.\n\nForage is self-only in the log, so this is your own record and \
    nobody else's. A zone announced as — Group is folded into the plain zone: it is the same place to forage in.";

impl Render for ForageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let zones: &[ForageZoneRow] = self.report.as_ref().map_or(&[], |r| &r.zones);
        let filter = self.open.clone().map(|z| {
            h_flex().gap_2().child(format!("Showing {z}")).child(
                Button::new("forage-filter-clear")
                    .label("Show all zones")
                    .small()
                    .on_click(cx.listener(|v, _, _, cx| v.select(None, cx))),
            )
        });
        div().id("forage-scroll").size_full().overflow_y_scrollbar().child(
            v_flex()
                .gap_3()
                .child(card("Forage").child(hint(self.report.as_ref().map(summary).unwrap_or_default())))
                .children(filter)
                .child(split(
                    card("Zones").child(self.render_zones(zones, cx)),
                    card("What it yields").child(self.render_items(zones)),
                ))
                .child(note(
                    "forage-note",
                    self.note_open,
                    NOTE,
                    cx.listener(|v, _, _, cx| {
                        v.note_open = !v.note_open;
                        cx.notify();
                    }),
                )),
        )
    }
}

impl ForageView {
    /// Most-foraged first. A zone that yielded nothing is kept, and says so.
    fn render_zones(&self, zones: &[ForageZoneRow], cx: &mut Context<Self>) -> Div {
        let mut zs: Vec<&ForageZoneRow> = zones.iter().collect();
        zs.sort_by(|a, b| b.attempts.cmp(&a.attempts).then_with(|| a.zone.cmp(&b.zone)));
        v_flex().gap_1().children(zs.into_iter().enumerate().map(|(i, z)| {
            let mut items: Vec<_> = z.items.iter().collect();
            items.sort_by(|a, b| b.times.cmp(&a.times).then_with(|| a.item.cmp(&b.item)));
            let items = if items.is_empty() {
                format!("nothing, in {} attempt{}", z.attempts, if z.attempts == 1 { "" } else { "s" })
            } else {
                items
                    .iter()
                    .map(|i| if i.times > 1 { format!("{} ×{}", i.item, i.times) } else { i.item.clone() })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let title: SharedString = format!("{} of {} attempt(s) yielded something", z.hits, z.attempts).into();
            let zone = z.zone.clone();
            row(
                SharedString::from(format!("forage-zone-{i}")),
                z.zone.clone(),
                items,
                h_flex()
                    .id(SharedString::from(format!("forage-rate-{i}")))
                    .gap_1()
                    .items_baseline()
                    .child(rate(z.hits, z.attempts))
                    .child(div().text_xs().text_color(hex(MUTED)).child(format!("{}/{}", z.hits, z.attempts)))
                    .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                    .into_any_element(),
                None,
            )
            .cursor_pointer()
            // The renderer sets an `on` class here that nothing styles
            // (a3bphvj); the selected zone is marked.
            .when(self.open.as_deref() == Some(z.zone.as_str()), |d| d.border_1().border_color(hex(ACCENT)))
            .on_click(cx.listener(move |v, _, _, cx| v.select(Some(zone.clone()), cx)))
        }))
    }

    /// Every item, pooled across zones (or the selected one), with where it
    /// came from.
    fn render_items(&self, zones: &[ForageZoneRow]) -> Div {
        let pool = pool(zones, self.open.as_deref());
        if pool.is_empty() {
            return hint(match &self.open {
                Some(z) => format!("No forage has been seen in {z} yet."),
                None => "No forage has been seen yet.".into(),
            });
        }
        v_flex().gap_1().children(pool.into_iter().enumerate().map(|(i, (item, times, mut where_))| {
            where_.sort_by(|a, b| b.1.cmp(&a.1));
            let all: SharedString = where_.iter().map(|(z, n)| format!("{z} ×{n}")).collect::<Vec<_>>().join("\n").into();
            let shown = where_.iter().take(3).map(|(z, _)| z.as_str()).collect::<Vec<_>>().join(", ");
            let more = if where_.len() > 3 { format!(", +{} more", where_.len() - 3) } else { String::new() };
            row(
                SharedString::from(format!("forage-item-{i}")),
                item,
                format!("{shown}{more}"),
                div().child(times.to_string()).into_any_element(),
                Some(all),
            )
        }))
    }
}

/// A `.row` as forage.js draws it: a bold line over a small muted one, and a
/// figure on the right. `sub_title` is the muted line's hover text.
fn row(id: SharedString, name: String, sub: String, num: AnyElement, sub_title: Option<SharedString>) -> Stateful<Div> {
    let sub_id = SharedString::from(format!("{id}-sub"));
    h_flex()
        .id(id)
        .gap_3()
        .px(px(9.))
        .py(px(5.))
        .rounded(px(5.))
        .bg(hex(PANEL_2))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().font_weight(FontWeight::BOLD).child(name))
                .child(
                    div()
                        .id(sub_id)
                        .text_xs()
                        .text_color(hex(MUTED))
                        .child(sub)
                        .when_some(sub_title, |d, t| d.tooltip(move |window, cx| Tooltip::new(t.clone()).build(window, cx))),
                ),
        )
        .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(num))
}

/// (item, total, [(zone, times)]), most found first, then by name.
fn pool(zones: &[ForageZoneRow], only: Option<&str>) -> Vec<(String, u64, Vec<(String, u64)>)> {
    let mut pool: HashMap<&str, (u64, Vec<(String, u64)>)> = HashMap::new();
    for z in zones {
        if only.is_some_and(|o| o != z.zone) {
            continue;
        }
        for i in &z.items {
            let e = pool.entry(&i.item).or_default();
            e.0 += i.times;
            e.1.push((z.zone.clone(), i.times));
        }
    }
    let mut out: Vec<_> = pool.into_iter().map(|(k, (n, w))| (k.to_string(), n, w)).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The rate, rounded, or `—` with no attempts. Its denominator travels with it.
fn rate(hits: u64, attempts: u64) -> String {
    if attempts == 0 { "—".into() } else { format!("{}%", (hits as f64 / attempts as f64 * 100.).round()) }
}

fn summary(r: &ForageReport) -> String {
    let attempts: u64 = r.zones.iter().map(|z| z.attempts).sum();
    let hits: u64 = r.zones.iter().map(|z| z.hits).sum();
    let items = r.zones.iter().flat_map(|z| z.items.iter().map(|i| i.item.as_str())).collect::<BTreeSet<_>>().len();
    if attempts == 0 {
        return "Nothing foraged yet.".into();
    }
    let s = |n: u64| if n == 1 { "" } else { "s" };
    let mut out = format!(
        "{hits} of {attempts} attempt{} ({}) yielded {items} distinct item{} across {} zone{}.",
        s(attempts),
        rate(hits, attempts),
        s(items as u64),
        r.zones.len(),
        s(r.zones.len() as u64)
    );
    if r.unplaced > 0 {
        out += &format!(
            " {} attempt{} arrived before any zone was announced and {} not counted here.",
            r.unplaced,
            s(r.unplaced),
            if r.unplaced == 1 { "is" } else { "are" }
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{pool, rate, summary};
    use norrlog_api::{ForageReport, ForageZoneRow, ForagedItem};

    fn zone(name: &str, attempts: u64, items: &[(&str, u64)]) -> ForageZoneRow {
        let hits = items.iter().map(|i| i.1).sum();
        ForageZoneRow {
            zone: name.into(),
            game: "eql".into(),
            attempts,
            hits,
            empty: attempts - hits,
            first_seen: 0,
            last_seen: 0,
            items: items.iter().map(|(i, n)| ForagedItem { item: (*i).into(), times: *n }).collect(),
        }
    }

    #[test]
    fn the_summary_names_its_denominator_and_the_unplaced() {
        let mut r = ForageReport { note: String::new(), source: String::new(), zones: vec![], unplaced: 0 };
        assert_eq!(summary(&r), "Nothing foraged yet.");
        r.zones = vec![zone("Befallen", 4, &[]), zone("Ak'Anon", 3, &[("Vegetables", 1), ("Roots", 1)])];
        r.unplaced = 1;
        assert_eq!(
            summary(&r),
            "2 of 7 attempts (29%) yielded 2 distinct items across 2 zones. \
             1 attempt arrived before any zone was announced and is not counted here."
        );
        assert_eq!(rate(0, 0), "—");
    }

    #[test]
    fn items_pool_across_zones_unless_one_is_selected() {
        let zs = vec![zone("A", 5, &[("Roots", 2), ("Berries", 1)]), zone("B", 5, &[("Berries", 3)])];
        let all = pool(&zs, None);
        assert_eq!(all[0].0, "Berries");
        assert_eq!(all[0].1, 4);
        assert_eq!(all[0].2.len(), 2);
        let b = pool(&zs, Some("B"));
        assert_eq!(b.len(), 1);
    }
}
