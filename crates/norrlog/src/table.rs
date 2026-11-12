//! Tables, as the Electron app draws them: an uppercase dim header, rows
//! that highlight on hover, numbers right-aligned. Columns are either fixed
//! width or share what is left.

use crate::theme::*;
use gpui::*;
use crate::theme::px;

#[derive(Clone, Copy)]
pub struct Col {
    pub label: &'static str,
    /// Fixed width; `None` shares the remaining space.
    pub width: Option<f32>,
    /// Numbers: right-aligned.
    pub num: bool,
}

pub const fn col(label: &'static str, width: Option<f32>, num: bool) -> Col {
    Col { label, width, num }
}

/// About how wide a character draws, rounded up: table text (digits, commas,
/// `%`), and the small capitals of a header.
const CHAR_W: f32 = 10.;
const HEAD_CHAR_W: f32 = 7.5;

/// `cols` with each fixed-width number column widened to fit the longest of
/// its texts. A number cell is right-aligned and clips on the left, so a
/// column one digit too narrow shows `00%` for `100%` and drops a fight's
/// leading digit without a sign. The fixed widths stay as minimums, and the
/// header takes the same widths, so the columns still line up.
pub fn fit(cols: &[Col], rows: &[Vec<String>]) -> Vec<Col> {
    cols.iter()
        .enumerate()
        .map(|(i, c)| match c.width {
            Some(w) if c.num => {
                let longest = rows.iter().filter_map(|r| r.get(i)).map(|t| t.chars().count()).max().unwrap_or(0);
                let head = c.label.chars().count() as f32 * HEAD_CHAR_W;
                Col { width: Some(w.max(longest as f32 * CHAR_W).max(head)), ..*c }
            }
            _ => *c,
        })
        .collect()
}

fn cell_box(c: &Col) -> Div {
    let d = div().overflow_hidden().whitespace_nowrap().text_ellipsis();
    // When the window is too narrow for every column (a large UI scale, a
    // small window), the fixed columns give up to a third of their width
    // before the flexible one drops below a readable floor; the cells
    // ellipsize rather than spill.
    let d = match c.width {
        Some(w) => d.w(px(w)).flex_shrink(1.).min_w(px(w * 0.7)),
        None => d.flex_1().min_w(px(120.)),
    };
    if c.num { d.flex().justify_end() } else { d }
}

pub fn head(cols: &[Col]) -> Div {
    head_spaced(cols, px(12.))
}

/// A table inside a card, where the columns have to be packed tighter than
/// in a page-wide list.
pub fn head_dense(cols: &[Col]) -> Div {
    head_spaced(cols, px(8.))
}

fn head_spaced(cols: &[Col], gap: Pixels) -> Div {
    let mut r = div()
        .flex()
        .flex_row()
        .gap(gap)
        .px(gap)
        .py(px(6.))
        .bg(hex(PANEL))
        .border_b_1()
        .border_color(hex(BORDER))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hex(MUTED));
    for c in cols {
        r = r.child(cell_box(c).child(c.label.to_uppercase()));
    }
    r
}

/// One row. Cells line up with `cols`; extra cells are ignored.
pub fn row(id: impl Into<ElementId>, cols: &[Col], cells: Vec<AnyElement>) -> Stateful<Div> {
    row_spaced(id, cols, cells, px(12.))
}

/// [`row`] for a [`head_dense`] table.
pub fn row_dense(id: impl Into<ElementId>, cols: &[Col], cells: Vec<AnyElement>) -> Stateful<Div> {
    row_spaced(id, cols, cells, px(8.))
}

fn row_spaced(id: impl Into<ElementId>, cols: &[Col], cells: Vec<AnyElement>, gap: Pixels) -> Stateful<Div> {
    let mut r = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(gap)
        .px(gap)
        .py(px(5.))
        .border_b_1()
        .border_color(wash(BORDER, 0.6))
        .hover(|s| s.bg(hex(PANEL_2)));
    for (c, cell) in cols.iter().zip(cells) {
        r = r.child(cell_box(c).child(cell));
    }
    r
}

/// A row with nothing but a message, spanning the table.
pub fn empty_row(text: impl Into<SharedString>) -> Div {
    div().px_3().py_4().text_color(hex(MUTED)).child(text.into())
}

/// Dim text, for the quieter columns.
pub fn muted(text: impl Into<SharedString>) -> Div {
    div().text_color(hex(MUTED)).child(text.into())
}

/// A small inline tag, the renderer's `.tag`.
pub fn tag(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .px(px(5.))
        .rounded(px(4.))
        .bg(wash(color, 0.15))
        .text_color(hex(color))
        .text_size(px(11.))
        .child(text.into())
}

#[cfg(test)]
mod tests {
    use super::{col, fit};

    #[test]
    fn a_number_column_widens_to_its_longest_text_and_no_further_down() {
        let cols = [col("Ability", None, false), col("Crit", Some(32.), true), col("Kind", Some(40.), false)];
        let rows = vec![
            vec!["kick".to_string(), "100%".to_string(), "a long kind".to_string()],
            vec!["shoot".to_string(), "40%".to_string(), "x".to_string()],
        ];
        let w: Vec<Option<f32>> = fit(&cols, &rows).iter().map(|c| c.width).collect();
        assert_eq!(w, [None, Some(40.), Some(40.)]);
        let short = vec![vec!["a".to_string(), "1%".to_string(), "x".to_string()]];
        assert_eq!(fit(&cols, &short)[1].width, Some(32.));
        // The header counts too.
        let wide = [col("% of group", Some(68.), true)];
        assert_eq!(fit(&wide, &[]).first().and_then(|c| c.width), Some(75.));
    }
}
