//! The fight's two charts: DPS over time and the timeline. Ports
//! `dpsOverTimeHtml` and `timelineHtml` from panels.js; the rules are in the
//! Formatting section of `docs/parity/encounters-and-procs.md`.
//!
//! Electron draws both as SVG. Here the marks are painted on a canvas (a long
//! fight is thousands of them) and everything that wants text or a tooltip,
//! such as axis labels, effect spans and stance lines, is an ordinary element laid
//! over it.

use crate::format::{dur, num};
use crate::theme::*;
use eqlparser_api::{Encounter, EncounterSummary, Outcome, TickSeries, TimelineEvent, TimelineMark};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};

/// The rolling window, in seconds.
const DPS_WINDOW: usize = 5;
const DPS_H: f32 = 150.;

const TIMELINE_LANES: usize = 40;
const LANE_H: f32 = 22.;
const TL_PX_PER_SEC: f64 = 8.;
const TL_MIN_W: f64 = 640.;
const TL_MAX_W: f64 = 12000.;
const TL_AXIS_H: f32 = 20.;
const LANE_NAME_W: f32 = 210.;
/// Seconds between ticks of a damage- or heal-over-time effect.
const DOT_TICK: i64 = 6;

// ---------------------------------------------------------------------------
// DPS over time.

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub run: Vec<f64>,
    pub colour: u32,
    pub fill: bool,
    pub label: &'static str,
}

/// A centred rolling mean that shortens at both ends rather than padding
/// with zeroes, so a fight that opens at full output draws no false ramp.
pub fn rolling(run: &[u32], window: usize) -> Vec<f64> {
    let half = window / 2;
    (0..run.len())
        .map(|i| {
            let from = i.saturating_sub(half);
            let to = (i + half).min(run.len() - 1);
            run[from..=to].iter().map(|&v| v as f64).sum::<f64>() / (to - from + 1) as f64
        })
        .collect()
}

/// The lines to draw, smoothed, without the ones that never leave zero.
/// Solo, "you + pet + group" *is* you, so the total only gets a line when
/// something else contributed, and otherwise your line takes the fill.
pub fn dps_lines(t: &TickSeries) -> Vec<Line> {
    if !t.retained || t.you.is_empty() {
        return Vec::new();
    }
    let n = t.you.len();
    let at = |v: &[u32], i: usize| v.get(i).copied().unwrap_or(0);
    let helpers: Vec<u32> = (0..n).map(|i| at(&t.allies, i) + at(&t.pets, i)).collect();
    let mut raw: Vec<(Vec<u32>, u32, bool, &'static str)> = if helpers.iter().any(|&v| v > 0) {
        vec![
            ((0..n).map(|i| t.you[i] + helpers[i]).collect(), ACCENT, true, "you + pet + group"),
            (t.you.clone(), YOU, false, "you"),
            (helpers, PET, false, "pet + group"),
        ]
    } else {
        vec![(t.you.clone(), YOU, true, "you")]
    };
    raw.push(((0..n).map(|i| at(&t.incoming, i)).collect(), ENEMY, false, "incoming"));
    raw.into_iter()
        .map(|(run, colour, fill, label)| Line { run: rolling(&run, DPS_WINDOW), colour, fill, label })
        .filter(|l| l.run.iter().any(|&v| v > 0.))
        .collect()
}

/// Seconds at the five axis labels: the ends and three between.
fn axis_stops(n: usize) -> Vec<usize> {
    (0..=4).map(|i| ((n.saturating_sub(1) * i) as f64 / 4.).round() as usize).collect()
}

fn mark_label(m: &TimelineMark) -> String {
    if m.kind == "invocation" { format!("Invocation: {}", m.label) } else { format!("Stance: {}", m.label) }
}

fn mark_colour(m: &TimelineMark) -> u32 {
    if m.kind == "invocation" { PET } else { ACCENT }
}

pub fn dps_card(e: &Encounter, s: &EncounterSummary) -> Option<Div> {
    let lines = dps_lines(&s.tick_series);
    if lines.is_empty() {
        return None;
    }
    let n = s.tick_series.you.len();
    let peak = lines.iter().flat_map(|l| l.run.iter().copied()).fold(1f64, f64::max);
    let swaps: Vec<(f32, u32, String)> = e
        .marks
        .iter()
        .filter(|m| m.at >= e.start && m.at <= e.end)
        .map(|m| {
            let i = ((m.at - e.start) as usize).min(n - 1);
            (frac(i, n), mark_colour(m), mark_label(m))
        })
        .collect();

    let paint_lines = lines.clone();
    let paint_swaps: Vec<(f32, u32)> = swaps.iter().map(|s| (s.0, s.1)).collect();
    let chart = canvas(
        |_, _, _| {},
        move |b, _, window, _| window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
            let w = f32::from(b.size.width);
            let pt = |i: usize, v: f64| {
                let y = DPS_H - 6. - (v / peak) as f32 * (DPS_H - 14.);
                b.origin + point(px(frac(i, n) * w), px(y))
            };
            for l in &paint_lines {
                if l.fill {
                    let mut p = PathBuilder::fill();
                    p.move_to(b.origin + point(px(0.), px(DPS_H)));
                    for (i, &v) in l.run.iter().enumerate() {
                        p.line_to(pt(i, v));
                    }
                    p.line_to(b.origin + point(px(w), px(DPS_H)));
                    p.close();
                    if let Ok(path) = p.build() {
                        window.paint_path(path, wash(l.colour, 0.14));
                    }
                }
            }
            for l in &paint_lines {
                let mut p = PathBuilder::stroke(px(1.5));
                for (i, &v) in l.run.iter().enumerate() {
                    if i == 0 { p.move_to(pt(i, v)) } else { p.line_to(pt(i, v)) }
                }
                if l.run.len() == 1 {
                    p.line_to(pt(0, l.run[0]) + point(px(1.), px(0.)));
                }
                if let Ok(path) = p.build() {
                    window.paint_path(path, hex(l.colour));
                }
            }
            for &(f, c) in &paint_swaps {
                dashed_vline(window, b.origin.x + px(f * w), b.origin.y, px(DPS_H), wash(c, 0.7), 1.);
            }
        }),
    )
    .size_full();

    // Stance lines get a hover strip of their own, to carry the label.
    let mut plot = div().relative().w_full().h(px(DPS_H)).child(chart).child(
        div().absolute().top_0().left_1().text_xs().text_color(hex(MUTED)).child(format!("{} dps", num(Some(peak)))),
    );
    for (i, (f, _, label)) in swaps.into_iter().enumerate() {
        plot = plot.child(hover_strip(SharedString::from(format!("dps-swap-{i}")), f, px(DPS_H), label));
    }

    let mut axis = div().relative().h(px(16.)).text_xs().text_color(hex(MUTED));
    let stops = axis_stops(n);
    let last = stops.len() - 1;
    for (k, at) in stops.into_iter().enumerate() {
        axis = axis.child(axis_label(frac_of(at, n), dur(at as f64), k == 0, k == last));
    }

    let mut legend = legend_row();
    for l in &lines {
        legend = legend.child(key(l.colour, l.label));
    }
    if !e.marks.iter().all(|m| m.at < e.start || m.at > e.end) {
        legend = legend.child(swap_key(ACCENT, "stance")).child(swap_key(PET, "invocation"));
    }
    legend = legend.child(div().text_color(hex(MUTED)).child(format!("{DPS_WINDOW}s rolling")));

    Some(
        chart_card()
            .child(
                h_flex()
                    .justify_between()
                    .child(card_title("DPS over time"))
                    .child(div().text_sm().text_color(hex(MUTED)).child(format!("{} dps peak", num(Some(peak))))),
            )
            .child(v_flex().child(plot).child(axis))
            .child(legend),
    )
}

/// Where second `i` of `n` falls across the chart, 0.0–1.0. A one-second
/// fight sits in the middle.
fn frac(i: usize, n: usize) -> f32 {
    if n <= 1 { 0.5 } else { i as f32 / (n - 1) as f32 }
}

fn frac_of(at: usize, n: usize) -> f32 {
    at as f32 / (n.max(2) - 1) as f32
}

// ---------------------------------------------------------------------------
// Timeline.

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub from: i64,
    pub to: i64,
    pub predicted: bool,
    pub target: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Lane {
    pub name: String,
    pub events: Vec<TimelineEvent>,
    /// Measured effect spans, or else spans inferred from over-time ticks.
    pub spans: Vec<Span>,
    pub damage: u64,
    pub heal: bool,
    /// Whether the spans were measured (effects the log saw land), which
    /// ranks a lane ahead of one with the same damage.
    pub measured: bool,
}

/// Stretches an over-time effect ran, read off its ticks: one every six
/// seconds, a gap over two of them is a lapse, and a bar runs one tick
/// either side of what was seen.
pub fn dot_spans(events: &[TimelineEvent]) -> Vec<Span> {
    let mut ticks: Vec<i64> =
        events.iter().filter(|e| (e.kind == "dot" || e.kind == "hot") && e.amount > 0).map(|e| e.at).collect();
    ticks.sort_unstable();
    let mut out: Vec<(Span, i64)> = Vec::new();
    for at in ticks {
        match out.last_mut() {
            Some((sp, last)) if at - *last <= DOT_TICK * 2 => {
                *last = at;
                sp.to = at + DOT_TICK;
            }
            _ => out.push((Span { from: at - DOT_TICK, to: at + DOT_TICK, predicted: false, target: None }, at)),
        }
    }
    out.into_iter().map(|(s, _)| s).collect()
}

/// One lane per ability (effects join their spell's lane), ranked by damage,
/// with an effect lane ahead of an equally damaging one. Returns the lanes
/// shown and how many were cut.
pub fn lanes(e: &Encounter) -> (Vec<Lane>, usize) {
    let mut order: Vec<Lane> = Vec::new();
    let mut idx = std::collections::HashMap::<String, usize>::new();
    let mut lane = |name: &str, order: &mut Vec<Lane>| -> usize {
        *idx.entry(name.to_string()).or_insert_with(|| {
            order.push(Lane { name: name.to_string(), events: vec![], spans: vec![], damage: 0, heal: false, measured: false });
            order.len() - 1
        })
    };
    for ev in &e.timeline {
        let i = lane(&ev.ability, &mut order);
        order[i].damage += ev.amount;
        order[i].events.push(ev.clone());
    }
    for fx in &e.effects {
        let i = lane(&fx.spell, &mut order);
        order[i].spans.push(Span {
            from: fx.at,
            to: fx.ended.unwrap_or(fx.until).min(e.end),
            predicted: fx.ended.is_none(),
            target: Some(fx.target.clone()),
        });
    }
    for l in &mut order {
        l.measured = !l.spans.is_empty();
        l.heal = !l.events.is_empty() && l.events.iter().all(|ev| ev.kind == "hot" || ev.kind == "heal");
        if l.spans.is_empty() && l.events.iter().any(|ev| ev.kind == "dot" || ev.kind == "hot") {
            l.spans = dot_spans(&l.events);
        }
    }
    // Stable, as Array.prototype.sort is: equal lanes keep first-seen order.
    order.sort_by(|a, b| b.damage.cmp(&a.damage).then_with(|| b.measured.cmp(&a.measured)));
    let hidden = order.len().saturating_sub(TIMELINE_LANES);
    order.truncate(TIMELINE_LANES);
    (order, hidden)
}

/// The chart's width: eight pixels a second, held between 640 and 12000.
pub fn tl_width(span: i64) -> f64 {
    (span as f64 * TL_PX_PER_SEC).round().clamp(TL_MIN_W, TL_MAX_W)
}

fn outcome_style(o: Outcome) -> (u32, f32, f32) {
    match o {
        Outcome::Hit => (YOU, 11., 0.85),
        Outcome::Crit => (ACCENT, 18., 1.),
        Outcome::Miss => (MUTED, 6., 0.7),
        Outcome::Resist => (ENEMY, 13., 0.9),
    }
}

pub fn timeline_card(e: &Encounter, s: &EncounterSummary) -> Div {
    let card = chart_card();
    if !e.timeline_retained {
        return card.child(card_title("Timeline")).child(empty(
            "Not kept for this fight — the per-event detail is held for the most recent fights only. Its totals \
             above are complete.",
        ));
    }
    if e.timeline.is_empty() && e.effects.is_empty() {
        return card.child(card_title("Timeline")).child(empty("Nothing of yours landed in this fight"));
    }
    let (shown, hidden) = lanes(e);
    let start = e.start;
    let span = (e.end - e.start).max(1);
    let width = tl_width(span) as f32;
    let height = shown.len() as f32 * LANE_H + TL_AXIS_H;
    let x = move |at: i64| 4. + ((at - start) as f32 / span as f32) * (width - 8.);
    let tick_count = ((width / 150.).round() as usize).clamp(4, 24);
    let swaps: Vec<&TimelineMark> = e.marks.iter().filter(|m| m.at >= start && m.at <= e.end).collect();

    // Lane names stay put while the chart scrolls under them.
    let names = v_flex().w(px(LANE_NAME_W)).flex_none().pb(px(TL_AXIS_H)).children(shown.iter().enumerate().map(|(i, l)| {
        let full = l.name.clone();
        h_flex()
            .id(SharedString::from(format!("tl-name-{i}")))
            .h(px(LANE_H))
            .px_2()
            .gap_2()
            .justify_between()
            .text_xs()
            .when(i % 2 == 1, |d| d.bg(wash(TEXT, 0.03)))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(l.name.clone()))
            .child(
                div()
                    .id(SharedString::from(format!("tl-total-{i}")))
                    .flex_none()
                    .text_color(hex(if l.heal { HEAL } else { MUTED }))
                    .child(num(Some(l.damage as f64)))
                    .when(l.heal, |d| d.tooltip(|window, cx| Tooltip::new("healed").build(window, cx))),
            )
            .tooltip(move |window, cx| Tooltip::new(if full.is_empty() { "—".to_string() } else { full.clone() }).build(window, cx))
    }));

    let paint_lanes = shown.clone();
    let paint_swaps: Vec<(i64, u32)> = swaps.iter().map(|m| (m.at, mark_colour(m))).collect();
    let chart = canvas(
        |_, _, _| {},
        // Clipped to the chart, as the SVG is: an inferred span runs a tick
        // past the fight's end.
        move |b, _, window, _| window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
            let o = b.origin;
            let lanes_h = height - TL_AXIS_H;
            for k in 0..=tick_count {
                let at = start + (span as f32 * k as f32 / tick_count as f32) as i64;
                let gx = o.x + px(x(at));
                window.paint_quad(fill(Bounds::new(point(gx, o.y), size(px(1.), px(lanes_h))), wash(TEXT, 0.08)));
            }
            for (i, l) in paint_lanes.iter().enumerate() {
                let y = o.y + px(i as f32 * LANE_H);
                if i % 2 == 1 {
                    window.paint_quad(fill(Bounds::new(point(o.x, y), size(px(width), px(LANE_H))), wash(TEXT, 0.03)));
                }
                let tint = if l.heal { Some(HEAL) } else { None };
                for sp in &l.spans {
                    let bx = x(sp.from);
                    let bw = (x(sp.to) - bx).max(2.);
                    let r = Bounds::new(point(o.x + px(bx), y + px(3.)), size(px(bw), px(LANE_H - 6.)));
                    let c = tint.unwrap_or(YOU);
                    if sp.predicted {
                        hatch(window, r, c);
                    } else {
                        window.paint_quad(fill(r, wash(c, 0.28)).corner_radii(px(2.)));
                    }
                }
                for ev in &l.events {
                    let (c, h, a) = outcome_style(ev.outcome);
                    let c = tint.unwrap_or(c);
                    let r = Bounds::new(
                        point(o.x + px(x(ev.at)), y + px((LANE_H - h) / 2.)),
                        size(px(3.), px(h)),
                    );
                    window.paint_quad(fill(r, wash(c, a)).corner_radii(px(1.)));
                }
            }
            for &(at, c) in &paint_swaps {
                dashed_vline(window, o.x + px(x(at)), o.y, px(lanes_h), wash(c, 0.85), 1.5);
            }
        }),
    )
    .w(px(width))
    .h(px(height));

    // What carries a label: effect spans with a target, stance lines, and
    // the axis numbers.
    let mut plot = div().relative().w(px(width)).h(px(height)).flex_none().child(chart);
    for (i, l) in shown.iter().enumerate() {
        for (j, sp) in l.spans.iter().enumerate() {
            let Some(target) = &sp.target else { continue };
            let tip = format!(
                "{} on {} — {}{}",
                l.name,
                target,
                dur((sp.to - sp.from) as f64),
                if sp.predicted { " (expected; no wear-off seen)" } else { "" }
            );
            let bx = x(sp.from);
            plot = plot.child(
                div()
                    .id(SharedString::from(format!("tl-span-{i}-{j}")))
                    .absolute()
                    .left(px(bx))
                    .top(px(i as f32 * LANE_H + 3.))
                    .w(px((x(sp.to) - bx).max(2.)))
                    .h(px(LANE_H - 6.))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
            );
        }
    }
    for (i, m) in swaps.iter().enumerate() {
        plot = plot.child(hover_strip(
            SharedString::from(format!("tl-swap-{i}")),
            x(m.at) / width,
            px(height - TL_AXIS_H),
            mark_label(m),
        ));
    }
    for k in 0..=tick_count {
        let f = k as f32 / tick_count as f32;
        let at = start + (span as f32 * f) as i64;
        plot = plot.child(
            div()
                .absolute()
                .top(px(height - TL_AXIS_H + 3.))
                .left(px((x(at) - 30.).clamp(0., width - 60.)))
                .w(px(60.))
                .flex()
                .justify_center()
                .text_xs()
                .text_color(hex(MUTED))
                .child(dur((span as f32 * f).round() as f64)),
        );
    }

    let mut legend = legend_row()
        .child(key(YOU, "hit"))
        .child(key(ACCENT, "crit"))
        .child(key(MUTED, "miss"))
        .child(key(ENEMY, "resist"))
        .child(band_key(false, "effect running"));
    if shown.iter().any(|l| l.heal) {
        legend = legend.child(key(HEAL, "healing"));
    }
    if e.effects.iter().any(|f| f.ended.is_none()) {
        legend = legend.child(band_key(true, "expected (no wear-off seen)"));
    }
    legend = legend.child(swap_key(ACCENT, "stance")).child(swap_key(PET, "invocation"));

    let mut note = "Your own actions only — a charmed pet's damage counts toward your total but is not a skill you \
                    pressed, and mixing the two makes the lanes unreadable."
        .to_string();
    if hidden > 0 {
        note.push_str(&format!(" {hidden} further {} are not shown.", if hidden == 1 { "ability" } else { "abilities" }));
    }
    if e.timeline_dropped > 0 {
        note.push_str(&format!(
            " The first {} of your actions are shown; {} more happened after that and were not kept.",
            num(Some(e.timeline.len() as f64)),
            num(Some(e.timeline_dropped as f64))
        ));
    }

    card.child(
        h_flex().gap_2().child(card_title("Timeline")).child(div().text_sm().text_color(hex(MUTED)).child(format!(
            "— {} actions over {}",
            num(Some(e.timeline.len() as f64)),
            dur(s.duration as f64)
        ))),
    )
    .child(legend)
    .child(
        h_flex()
            .items_start()
            .child(names)
            .child(div().id("tl-scroll").flex_1().min_w_0().overflow_x_scroll().own_axis().child(plot)),
    )
    .child(div().text_sm().text_color(hex(MUTED)).child(note))
}

// ---------------------------------------------------------------------------
// Shared pieces.

fn chart_card() -> Div {
    v_flex().gap_2().p_3().rounded(px(8.)).border_1().border_color(hex(BORDER)).bg(hex(PANEL)).min_w_0()
}

fn card_title(t: &str) -> Div {
    div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(hex(MUTED)).child(t.to_uppercase())
}

fn empty(t: &'static str) -> Div {
    div().py_2().text_color(hex(MUTED)).child(t)
}

fn legend_row() -> Div {
    h_flex().flex_wrap().gap_3().text_xs().text_color(hex(TEXT))
}

fn key(colour: u32, label: &str) -> Div {
    h_flex().gap_1().child(div().w(px(10.)).h(px(10.)).rounded(px(2.)).bg(hex(colour))).child(label.to_string())
}

fn band_key(predicted: bool, label: &str) -> Div {
    let sw = div().w(px(14.)).h(px(10.)).rounded(px(2.));
    let sw = if predicted { sw.border_1().border_dashed().border_color(hex(YOU)).bg(wash(YOU, 0.2)) } else { sw.bg(wash(YOU, 0.28)) };
    h_flex().gap_1().child(sw).child(label.to_string())
}

fn swap_key(colour: u32, label: &str) -> Div {
    h_flex().gap_1().child(div().w(px(0.)).h(px(12.)).border_l_2().border_dashed().border_color(hex(colour))).child(label.to_string())
}

fn axis_label(f: f32, text: String, first: bool, last: bool) -> Div {
    let d = div().absolute().top(px(2.));
    let d = if first {
        d.left_0()
    } else if last {
        d.right_0()
    } else {
        d.left(relative(f)).ml(px(-30.)).w(px(60.)).flex().justify_center()
    };
    d.child(text)
}

/// An invisible strip over a vertical line, so hovering the line names it.
fn hover_strip(id: SharedString, f: f32, h: Pixels, label: String) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .top_0()
        .h(h)
        .left(relative(f))
        .ml(px(-3.))
        .w(px(6.))
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
}

fn dashed_vline(window: &mut Window, x: Pixels, top: Pixels, h: Pixels, colour: Hsla, w: f32) {
    let mut y = 0.;
    while y < f32::from(h) {
        let seg = 3f32.min(f32::from(h) - y);
        window.paint_quad(fill(Bounds::new(point(x - px(w / 2.), top + px(y)), size(px(w), px(seg))), colour));
        y += 5.;
    }
}

/// An expected tail: diagonal stripes, clipped to the bar.
fn hatch(window: &mut Window, r: Bounds<Pixels>, c: u32) {
    window.paint_quad(fill(r, wash(c, 0.12)).corner_radii(px(2.)));
    window.with_content_mask(Some(ContentMask { bounds: r }), |window| {
        let h = f32::from(r.size.height);
        let mut x = -h;
        while x < f32::from(r.size.width) {
            let mut p = PathBuilder::stroke(px(2.));
            p.move_to(r.origin + point(px(x), px(h)));
            p.line_to(r.origin + point(px(x + h), px(0.)));
            if let Ok(path) = p.build() {
                window.paint_path(path, wash(c, 0.45));
            }
            x += 6.;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{TickSeries, dot_spans, dps_lines, rolling, tl_width};
    use eqlparser_api::{Outcome, TimelineEvent};

    #[test]
    fn the_rolling_mean_is_centred_and_shortens_at_the_ends() {
        let r = rolling(&[10, 0, 0, 0, 10], 5);
        assert_eq!(r, vec![10. / 3., 2.5, 4., 2.5, 10. / 3.]);
        assert_eq!(rolling(&[7], 5), vec![7.]);
    }

    #[test]
    fn solo_your_line_takes_the_fill_and_the_total_is_not_drawn() {
        let t = TickSeries { retained: true, you: vec![5, 5], pets: vec![0, 0], allies: vec![0, 0], incoming: vec![0, 0] };
        let l = dps_lines(&t);
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].label, l[0].fill), ("you", true));

        let t = TickSeries { allies: vec![1, 0], incoming: vec![0, 3], ..t };
        let labels: Vec<_> = dps_lines(&t).iter().map(|l| (l.label, l.fill)).collect();
        assert_eq!(labels, [("you + pet + group", true), ("you", false), ("pet + group", false), ("incoming", false)]);

        let t = TickSeries { retained: false, ..t };
        assert!(dps_lines(&t).is_empty(), "a merged view keeps no curve");
    }

    #[test]
    fn dot_ticks_become_spans_that_break_on_a_lapse() {
        let tick = |at| TimelineEvent { at, ability: "x".into(), outcome: Outcome::Hit, amount: 5, kind: "dot".into() };
        let s = dot_spans(&[tick(100), tick(106), tick(112), tick(130)]);
        assert_eq!(s.iter().map(|s| (s.from, s.to)).collect::<Vec<_>>(), [(94, 118), (124, 136)]);
    }

    #[test]
    fn the_timeline_is_eight_pixels_a_second_within_bounds() {
        assert_eq!(tl_width(10), 640.);
        assert_eq!(tl_width(100), 800.);
        assert_eq!(tl_width(10_000), 12000.);
    }
}
