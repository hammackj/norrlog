//! Classes: the snapshot's class evidence, redrawn on every push; it has no
//! loader of its own.

use crate::parts::{ROW, bar_row, card, dim, empty, figure, fill, note};
use crate::theme::px;
use gpui::*;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{h_flex, v_flex};
use norrlog_api::ClassInfo;

pub struct ClassesView {
    classes: Vec<ClassInfo>,
    note_open: bool,
}

impl ClassesView {
    pub fn new() -> Self {
        ClassesView {
            classes: Vec::new(),
            note_open: false,
        }
    }

    /// A snapshot arrived. Only a change redraws: the evidence moves far less
    /// often than snapshots come.
    pub fn on_snapshot(&mut self, classes: Vec<ClassInfo>, cx: &mut Context<Self>) {
        let same = classes.len() == self.classes.len()
            && classes.iter().zip(&self.classes).all(|(a, b)| {
                a.name == b.name
                    && a.stated == b.stated
                    && pct(a.share) == pct(b.share)
                    && reasons(a) == reasons(b)
            });
        if !same {
            self.classes = classes;
            cx.notify();
        }
    }
}

const NOTE: &str = "A Legends character holds three classes at once and the combination changes over time; a live \
    EverQuest character holds one at a time, and can switch. Where the game names the class — a /who row for your own \
    character, or in live EverQuest the class chat channel it joins you to on every zone-in — that is the answer, and \
    it is shown as stated rather than with a percentage: it is not a reading with a confidence. Otherwise classes are \
    inferred from the spellbook (authoritative — the game's own spell data records which classes may cast each spell) \
    and from class-defining skills, which is the only way pure-melee classes can be detected at all. In Legends, \
    evidence is scoped to the last 24 hours of log time, so a class you have since dropped does not linger.";

impl Render for ClassesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = if self.classes.is_empty() {
            empty("No class evidence yet — set the game directory in Settings.")
        } else {
            v_flex().gap_1().children(self.classes.iter().map(|c| {
                bar_row(
                    c.share as f64,
                    fill(ROW),
                    h_flex()
                        .gap_1()
                        .child(c.name.clone())
                        .child(dim(format!("({})", c.abbrev))),
                    h_flex()
                        .gap_3()
                        // Long reasons give way to the share, on one line as the renderer keeps them.
                        .child(
                            dim(reasons(c))
                                .max_w(px(640.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis(),
                        )
                        // A class the game named is not a reading with a
                        // confidence, and a percentage beside it would invite
                        // reading it as one.
                        .child(figure(if c.stated {
                            "stated".into()
                        } else {
                            pct(c.share)
                        })),
                )
            }))
        };
        // The notes come before the rows, as in the renderer.
        div()
            .id("classes-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(
                card("Detected classes")
                    .child(note(
                        "classes-note",
                        self.note_open,
                        NOTE,
                        cx.listener(|v, _, _, cx| {
                            v.note_open = !v.note_open;
                            cx.notify();
                        }),
                    ))
                    .child(rows),
            )
    }
}

/// Up to four reasons, comma-joined, or `—`. The rest are not shown.
fn reasons(c: &ClassInfo) -> String {
    let r: Vec<&str> = c.reasons.iter().take(4).map(String::as_str).collect();
    if r.is_empty() {
        "—".into()
    } else {
        r.join(", ")
    }
}

fn pct(share: f32) -> String {
    format!("{:.0}%", share * 100.)
}

#[cfg(test)]
mod tests {
    use super::{pct, reasons};
    use norrlog_api::ClassInfo;

    #[test]
    fn a_row_shows_four_reasons_at_most_and_a_rounded_share() {
        let mut c = ClassInfo {
            name: "Shaman".into(),
            abbrev: "SHM".into(),
            score: 1.,
            share: 0.179,
            reasons: vec![],
            stated: false,
        };
        assert_eq!(reasons(&c), "—");
        c.reasons = (1..=6).map(|n| format!("r{n}")).collect();
        assert_eq!(reasons(&c), "r1, r2, r3, r4");
        assert_eq!(pct(c.share), "18%");
    }
}
