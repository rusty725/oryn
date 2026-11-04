//! Line graphs drawn with box-drawing characters (─ │ ╭ ╮ ╰ ╯) on a faint
//! grid: one sample per column, the newest on the right, a bright dot on
//! the newest point. Used by the CPU, GPU, network and sensor history
//! panels. Graphs lower than [`MIN_LINE_ROWS`] fall back to braille dots
//! ([`graph`] picks), where one level per row would only flip between rows.
//!
//! Everything draws straight into the buffer and stays inside the given
//! `Rect` (clipped to the buffer), so tiny or empty areas never panic.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    symbols::Marker,
    widgets::{Axis, Chart, Dataset, GraphType, Widget},
};

use crate::themes::Theme;

/// `c` faded `k` (0..1) of the way to `bg`, for RGB colors (named ones stay).
pub fn fade(c: Color, bg: Color, k: f64) -> Color {
    match (c, bg) {
        (Color::Rgb(r, g, b), Color::Rgb(br, bgg, bb)) => {
            let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * k).round().clamp(0.0, 255.0) as u8;
            Color::Rgb(m(r, br), m(g, bgg), m(b, bb))
        }
        _ => c,
    }
}

/// A faint `c`: faded toward the background, or the terminal's dim
/// attribute when either isn't an RGB color (the "Terminal" scheme).
fn faint(c: Color, th: &Theme, k: f64) -> Style {
    match (c, th.bg) {
        (Color::Rgb(..), Color::Rgb(..)) => Style::new().fg(fade(c, th.bg, k)),
        _ => Style::new().fg(c).add_modifier(Modifier::DIM),
    }
}

/// One line of a [`lines`] graph: its samples (oldest first) and
/// `color(x, y)`, both 0..1 of the plot.
pub type LineSeries<'a> = (&'a [f64], &'a dyn Fn(f64, f64) -> Color);

/// The row (0 = top) of `v` in a plot `h` rows high spanning `lo..hi`.
/// Out-of-range values stick to the edges; a NaN lands on the bottom row.
fn row(v: f64, (lo, hi): (f64, f64), h: usize) -> usize {
    let span = (hi - lo).max(1e-9);
    let k = ((v - lo) / span).clamp(0.0, 1.0);
    // `as usize` turns a NaN into 0 (the bottom), never panics.
    let up = (k * h.saturating_sub(1) as f64).round() as usize;
    h.saturating_sub(1).saturating_sub(up)
}

/// Plots at least this many rows high get box-drawing lines; lower ones
/// braille dots (four levels per row instead of one).
pub const MIN_LINE_ROWS: u16 = 4;

/// The series in `r` as [`lines`], or as [`dots`] when `r` is lower than
/// [`MIN_LINE_ROWS`].
pub fn graph(buf: &mut Buffer, r: Rect, series: &[LineSeries], range: (f64, f64), marks: &[(f64, Color)], th: &Theme) {
    if r.height < MIN_LINE_ROWS {
        dots(buf, r, series, range, marks, th);
    } else {
        lines(buf, r, series, range, marks, th);
    }
}

/// A faint grid in `r`: horizontal lines at a quarter, half and three
/// quarters of the height.
fn grid(buf: &mut Buffer, r: Rect, th: &Theme) {
    let style = faint(th.border, th, 0.35);
    for q in 1..4u32 {
        let y = r.y + (r.height as u32 * q / 4) as u16;
        if y <= r.y || y >= r.y + r.height {
            continue;
        }
        for x in r.x..r.x + r.width {
            let c = &mut buf[(x, y)];
            c.set_char('┈');
            c.set_style(style.bg(th.bg));
        }
    }
}

/// Lines on a faint grid in `r`, values from `range.0` (bottom row) to
/// `range.1` (top row): each series as one unbroken line of box-drawing
/// characters, one sample per column, the newest on the right (older
/// samples than fit are left out). NaN samples break the line. `marks`
/// are dashed horizontal lines (thresholds) in a dimmed color, drawn under
/// the series and only when inside the range. A bright dot marks the
/// newest point of each series.
pub fn lines(buf: &mut Buffer, r: Rect, series: &[LineSeries], range: (f64, f64), marks: &[(f64, Color)], th: &Theme) {
    let r = r.intersection(buf.area);
    let (w, h) = (r.width as usize, r.height as usize);
    if w == 0 || h == 0 {
        return;
    }
    grid(buf, r, th);
    let (lo, hi) = range;
    for &(v, color) in marks {
        if !(lo..=hi).contains(&v) {
            continue;
        }
        let y = r.y + row(v, range, h) as u16;
        for x in r.x..r.x + r.width {
            let c = &mut buf[(x, y)];
            c.set_char('┄');
            c.set_style(Style::new().fg(color).bg(th.bg).add_modifier(Modifier::DIM));
        }
    }
    for (samples, color) in series {
        let n = samples.len().min(w);
        let s = &samples[samples.len() - n..];
        let first = w - n;
        let at = |x: usize, y: usize| color((x as f64 + 0.5) / w as f64, 1.0 - (y as f64 + 0.5) / h as f64);
        // `set_style` adds modifiers to the cell's own, so the DIM of a
        // threshold or Terminal-scheme grid cell is cleared first.
        let mut put = |x: usize, y: usize, ch: char| {
            let c = &mut buf[(r.x + x as u16, r.y + y as u16)];
            c.set_char(ch);
            c.modifier = Modifier::empty();
            c.set_style(Style::new().fg(at(x, y)).bg(th.bg));
        };
        let mut prev: Option<usize> = None;
        for (i, &v) in s.iter().enumerate() {
            if !v.is_finite() {
                prev = None;
                continue;
            }
            let (x, y) = (first + i, row(v, range, h));
            match prev {
                // Up: leave the old row, climb, turn onto the new one.
                Some(py) if y < py => {
                    put(x, py, '╯');
                    (y + 1..py).for_each(|yy| put(x, yy, '│'));
                    put(x, y, '╭');
                }
                // Down.
                Some(py) if y > py => {
                    put(x, py, '╮');
                    (py + 1..y).for_each(|yy| put(x, yy, '│'));
                    put(x, y, '╰');
                }
                _ => put(x, y, '─'),
            }
            prev = Some(y);
        }
        // The newest point (when the newest sample is a number), in the
        // series' own color.
        if let Some(y) = prev {
            let c = &mut buf[(r.x + (w - 1) as u16, r.y + y as u16)];
            c.set_char('●');
            c.modifier = Modifier::empty();
            c.set_style(Style::new().fg(at(w - 1, y)).bg(th.bg).add_modifier(Modifier::BOLD));
        }
    }
}

/// The same graph as [`lines`] in braille dots (ratatui's `Chart`), for
/// plots too low for one level per row: one sample per column, the newest
/// on the right, `marks` as dotted lines under the series. No grid and no
/// end dot. NaN samples are left out; values outside the range stick to
/// its edges.
pub fn dots(buf: &mut Buffer, r: Rect, series: &[LineSeries], range: (f64, f64), marks: &[(f64, Color)], th: &Theme) {
    let r = r.intersection(buf.area);
    let w = r.width as usize;
    if w == 0 || r.height == 0 {
        return;
    }
    // A Chart needs a real, non-empty range.
    let (lo, hi) = match range {
        (lo, hi) if lo.is_finite() && hi.is_finite() && hi > lo => (lo, hi),
        (lo, _) if lo.is_finite() => (lo, lo + 1.0),
        _ => (0.0, 1.0),
    };
    let right = (w - 1).max(1) as f64;
    let mark_pts: Vec<([(f64, f64); 2], Color)> = marks
        .iter()
        .filter(|(v, _)| (lo..=hi).contains(v))
        .map(|&(v, c)| ([(0.0, v), (right, v)], c))
        .collect();
    let pts: Vec<(Vec<(f64, f64)>, Color)> = series
        .iter()
        .map(|(samples, color)| {
            let n = samples.len().min(w);
            let first = w - n;
            let p: Vec<(f64, f64)> = samples[samples.len() - n..]
                .iter()
                .enumerate()
                .filter(|(_, v)| v.is_finite())
                .map(|(i, v)| ((first + i) as f64, v.clamp(lo, hi)))
                .collect();
            // One color per line: the one at its newest point.
            let c = p.last().map_or(color(1.0, 0.0), |&(_, v)| color(1.0, (v - lo) / (hi - lo)));
            (p, c)
        })
        .collect();
    let mut sets: Vec<Dataset> = mark_pts
        .iter()
        .map(|(p, c)| {
            Dataset::default()
                .marker(Marker::Dot)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(*c).add_modifier(Modifier::DIM))
                .data(p)
        })
        .collect();
    sets.extend(pts.iter().map(|(p, c)| {
        Dataset::default().marker(Marker::Braille).graph_type(GraphType::Line).style(Style::new().fg(*c)).data(p)
    }));
    Chart::new(sets)
        .style(Style::new().bg(th.bg))
        .x_axis(Axis::default().bounds([0.0, right]))
        .y_axis(Axis::default().bounds([lo, hi]))
        .render(r, buf);
}

/// The y labels (top first, spread evenly down to the bottom row, right
/// aligned, in the dim color) and the axis line `│` in the border color at
/// the left of `area`. Returns the plot area right of the axis, which is
/// empty when `area` is too small for a graph.
pub fn y_axis(buf: &mut Buffer, area: Rect, labels: &[String], th: &Theme) -> Rect {
    let area = area.intersection(buf.area);
    let lw = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let gutter = lw.saturating_add(1);
    if area.height == 0 || area.width < gutter.saturating_add(2) {
        return Rect { width: 0, height: 0, ..area };
    }
    let dim = Style::new().fg(th.dim);
    let last = area.height - 1;
    let mut used: Vec<u16> = Vec::new();
    for (i, l) in labels.iter().enumerate() {
        let dy = match labels.len() {
            1 => 0,
            n => (last as usize * i / (n - 1)) as u16,
        };
        // Too few rows for every label: the higher ones win.
        if used.contains(&dy) {
            continue;
        }
        used.push(dy);
        buf.set_stringn(area.x, area.y + dy, format!("{l:>w$}", w = lw as usize), lw as usize, dim);
    }
    for y in area.y..area.y + area.height {
        let c = &mut buf[(area.x + gutter - 1, y)];
        c.set_char('│');
        c.set_fg(th.border);
    }
    Rect { x: area.x + gutter, width: area.width - gutter, ..area }
}

/// Time labels in the row `r` under a plot: `left` at its left edge, `mid`
/// centred, `right` at its right edge (the ones that don't fit are left
/// out; the middle one keeps a space to both others or is left out too).
pub fn x_labels(buf: &mut Buffer, r: Rect, [left, mid, right]: [&str; 3], style: Style) {
    let r = r.intersection(buf.area);
    if r.width == 0 || r.height == 0 {
        return;
    }
    let w = r.width as usize;
    let (lw, mw, rw) = (left.chars().count(), mid.chars().count(), right.chars().count());
    let mx = w.saturating_sub(mw) / 2;
    if mw > 0 && mx > lw && mx + mw < w.saturating_sub(rw) {
        buf.set_stringn(r.x + mx as u16, r.y, mid, mw, style);
    }
    if lw + rw < w {
        buf.set_stringn(r.x, r.y, left, lw, style);
    }
    if rw <= w {
        buf.set_stringn(r.x + (w - rw) as u16, r.y, right, rw, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::themes::THEMES;

    /// A dark RGB scheme (Andromeda).
    fn th() -> Theme {
        THEMES[2]
    }

    const RED: Color = Color::Rgb(255, 0, 0);
    const BLUE: Color = Color::Rgb(0, 0, 255);

    fn buf(w: u16, h: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, w, h))
    }

    fn row_str(b: &Buffer, y: u16) -> String {
        (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect()
    }

    #[test]
    fn known_series_draws_the_expected_line() {
        let th = th();
        let mut b = buf(5, 3);
        let a = b.area;
        let red = |_: f64, _: f64| RED;
        lines(&mut b, a, &[(&[0.0, 0.0, 2.0, 2.0, 1.0], &red)], (0.0, 2.0), &[], &th);
        // Rows 1 and 2 are grid rows (a half and three quarters of 3).
        assert_eq!(row_str(&b, 0), "  ╭─╮");
        assert_eq!(row_str(&b, 1), "┈┈│┈●");
        assert_eq!(row_str(&b, 2), "──╯┈┈");
        // Line cells in the series color, grid cells faint, the dot bold in
        // the series color too.
        assert_eq!(b[(0, 2)].fg, RED);
        assert_eq!(b[(2, 1)].fg, RED);
        assert_eq!(b[(3, 1)].fg, fade(th.border, th.bg, 0.35));
        assert_eq!(b[(4, 1)].fg, RED);
        assert_eq!(b[(4, 1)].modifier, Modifier::BOLD);
    }

    #[test]
    fn series_cells_never_inherit_dim() {
        // The Terminal scheme draws its grid with DIM, and thresholds are
        // always DIM: a line or dot crossing them must not stay dimmed.
        for th in [THEMES[0], th()] {
            let mut b = buf(4, 4);
            let a = b.area;
            let red = |_: f64, _: f64| RED;
            // Grid on rows 1, 2 (and 3); the threshold on row 2.
            lines(&mut b, a, &[(&[2.0, 2.0, 2.0, 1.0], &red)], (0.0, 3.0), &[(1.0, BLUE)], &th);
            assert_eq!(row_str(&b, 1), "───╮", "{}", th.name);
            assert_eq!(row_str(&b, 2), "┄┄┄●", "{}", th.name);
            for x in 0..3 {
                assert_eq!(b[(x, 1)].modifier, Modifier::empty(), "{} col {x}", th.name);
            }
            assert_eq!(b[(3, 1)].modifier, Modifier::empty());
            assert_eq!(b[(3, 2)].modifier, Modifier::BOLD, "{}: the dot is bold, not dim", th.name);
        }
    }

    #[test]
    fn low_plots_fall_back_to_braille() {
        let th = th();
        let red = |_: f64, _: f64| RED;
        let s = [0.0, 3.0, 1.0, 9.0, 3.0, 5.0];
        let braille = |b: &Buffer, h: u16| {
            (0..h).flat_map(|y| row_str(b, y).chars().collect::<Vec<_>>()).filter(|c| ('\u{2801}'..='\u{28ff}').contains(c)).count()
        };
        let boxes = |b: &Buffer, h: u16| (0..h).any(|y| row_str(b, y).chars().any(|c| "─│╭╮╰╯●┈".contains(c)));
        for h in 1..MIN_LINE_ROWS {
            let mut b = buf(6, h);
            let a = b.area;
            graph(&mut b, a, &[(&s, &red)], (0.0, 10.0), &[(5.0, BLUE)], &th);
            assert!(braille(&b, h) > 0, "{h} rows: braille dots");
            assert!(!boxes(&b, h), "{h} rows: no box lines");
            // The dots are in the series color.
            let c = (0..h).flat_map(|y| (0..6).map(move |x| (x, y))).find(|&p| b[p].symbol().starts_with(|c| ('\u{2801}'..='\u{28ff}').contains(&c)));
            assert_eq!(b[c.unwrap()].fg, RED);
        }
        let mut b = buf(6, MIN_LINE_ROWS);
        let a = b.area;
        graph(&mut b, a, &[(&s, &red)], (0.0, 10.0), &[], &th);
        assert_eq!(braille(&b, MIN_LINE_ROWS), 0);
        assert!(boxes(&b, MIN_LINE_ROWS));
        // Odd input in the braille path: no panic.
        for range in [(0.0, 0.0), (0.0, -5.0), (f64::NAN, f64::NAN), (0.0, f64::INFINITY)] {
            let mut b = buf(4, 2);
            let a = b.area;
            dots(&mut b, a, &[(&[f64::NAN, 1.0, f64::INFINITY, -3.0], &red), (&[], &red)], range, &[(1.0, BLUE)], &th);
        }
    }

    #[test]
    fn net_dots_in_each_series_color() {
        let th = th();
        let mut b = buf(4, 5);
        let a = b.area;
        let rx = |_: f64, _: f64| RED;
        let tx = |_: f64, _: f64| BLUE;
        lines(&mut b, a, &[(&[4.0; 4], &rx), (&[0.0; 4], &tx)], (0.0, 4.0), &[], &th);
        assert_eq!((b[(3, 0)].symbol(), b[(3, 0)].fg), ("●", RED));
        assert_eq!((b[(3, 4)].symbol(), b[(3, 4)].fg), ("●", BLUE));
        assert!(b[(3, 4)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn grid_at_quarters() {
        let th = th();
        let mut b = buf(4, 8);
        let a = b.area;
        lines(&mut b, a, &[], (0.0, 1.0), &[], &th);
        for y in 0..8 {
            let want = if [2, 4, 6].contains(&y) { "┈┈┈┈" } else { "    " };
            assert_eq!(row_str(&b, y), want, "row {y}");
        }
    }

    #[test]
    fn newest_samples_on_the_right() {
        let th = th();
        let mut b = buf(3, 2);
        let a = b.area;
        let red = |_: f64, _: f64| RED;
        // Five samples, three columns: only the last three show; the oldest
        // (high) ones are cut off.
        lines(&mut b, a, &[(&[1.0, 1.0, 0.0, 0.0, 0.0], &red)], (0.0, 1.0), &[], &th);
        assert_eq!(row_str(&b, 0), "   ");
        assert_eq!(row_str(&b, 1), "──●");
        // Fewer samples than columns: they start right-aligned.
        let mut b = buf(4, 1);
        let a = b.area;
        lines(&mut b, a, &[(&[0.0], &red)], (0.0, 1.0), &[], &th);
        assert_eq!(row_str(&b, 0), "   ●");
    }

    #[test]
    fn empty_and_nan_series() {
        let th = th();
        let mut b = buf(4, 4);
        let a = b.area;
        let red = |_: f64, _: f64| RED;
        lines(&mut b, a, &[(&[], &red)], (0.0, 1.0), &[], &th);
        assert!(!(0..4).any(|y| row_str(&b, y).contains('●')));
        // A NaN breaks the line; a NaN at the end leaves no dot.
        let mut b = buf(4, 5);
        let a = b.area;
        lines(&mut b, a, &[(&[0.0, f64::NAN, 0.0, f64::NAN], &red)], (0.0, 1.0), &[], &th);
        assert_eq!(row_str(&b, 4), "─ ─ ");
        assert!(!(0..5).any(|y| row_str(&b, y).contains('●')));
        // All-NaN, infinite range, top <= 0, reversed range: no panic.
        for range in [(0.0, 0.0), (0.0, -5.0), (f64::NAN, f64::NAN), (0.0, f64::INFINITY)] {
            let mut b = buf(4, 4);
            let a = b.area;
            lines(&mut b, a, &[(&[f64::NAN, 1.0, f64::INFINITY, -3.0], &red)], range, &[(1.0, BLUE)], &th);
        }
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let th = th();
        let red = |_: f64, _: f64| RED;
        let s = [0.0, 5.0, 1.0, 9.0, 3.0];
        for w in 0..6 {
            for h in 0..4 {
                let mut b = buf(6, 4);
                let area = Rect::new(0, 0, w, h);
                let plot = y_axis(&mut b, area, &["100%".into(), "0%".into()], &th);
                lines(&mut b, plot, &[(&s, &red)], (0.0, 10.0), &[(5.0, BLUE)], &th);
                lines(&mut b, area, &[(&s, &red)], (0.0, 10.0), &[(5.0, BLUE)], &th);
                x_labels(&mut b, Rect::new(0, h.saturating_sub(1), w, h.min(1)), ["-120s", "-60s", "now"], Style::new());
            }
        }
        // A rect partly outside the buffer is clipped, not indexed.
        let mut b = buf(3, 3);
        lines(&mut b, Rect::new(1, 1, 50, 50), &[(&s, &red)], (0.0, 10.0), &[], &th);
        y_axis(&mut b, Rect::new(2, 2, 40, 40), &["x".into()], &th);
    }

    #[test]
    fn two_series_share_one_chart() {
        // The network graph: rx and tx as two lines from 0 up to the peak,
        // nothing mirrored below a middle axis.
        let th = th();
        let mut b = buf(4, 5);
        let a = b.area;
        let rx = |_: f64, _: f64| RED;
        let tx = |_: f64, _: f64| BLUE;
        lines(&mut b, a, &[(&[4.0; 4], &rx), (&[0.0; 4], &tx)], (0.0, 4.0), &[], &th);
        assert_eq!(row_str(&b, 0), "───●");
        assert_eq!(row_str(&b, 4), "───●");
        assert_eq!(b[(0, 0)].fg, RED);
        assert_eq!(b[(0, 4)].fg, BLUE);
        // Rows between are only the faint grid.
        assert_eq!(b[(0, 2)].fg, fade(th.border, th.bg, 0.35));
    }

    #[test]
    fn threshold_marks_dashed_inside_the_range() {
        let th = th();
        let mut b = buf(5, 5);
        let a = b.area;
        let red = |_: f64, _: f64| RED;
        // 30..70: warn at 60 (row 1) shows, crit at 90 is above the range.
        lines(&mut b, a, &[(&[40.0; 5], &red)], (30.0, 70.0), &[(60.0, th.warn), (90.0, th.crit)], &th);
        assert_eq!(row_str(&b, 1), "┄┄┄┄┄");
        assert_eq!(b[(0, 1)].fg, th.warn);
        assert!(b[(0, 1)].modifier.contains(Modifier::DIM));
        assert!(!(0..5).any(|y| y != 1 && row_str(&b, y).contains('┄')));
        // The data line (40 → row 3) is drawn over the grid.
        assert_eq!(row_str(&b, 3), "────●");
        assert_eq!(b[(0, 3)].fg, RED);
    }

    #[test]
    fn y_axis_labels_and_axis() {
        let th = th();
        let mut b = buf(10, 5);
        let a = b.area;
        let plot = y_axis(&mut b, a, &["90°C".into(), "60°C".into(), "30°C".into()], &th);
        assert_eq!(plot, Rect::new(5, 0, 5, 5));
        assert_eq!(row_str(&b, 0), "90°C│     ");
        assert_eq!(row_str(&b, 2), "60°C│     ");
        assert_eq!(row_str(&b, 4), "30°C│     ");
        assert_eq!(b[(4, 1)].symbol(), "│");
        assert_eq!(b[(4, 1)].fg, th.border);
        assert_eq!(b[(0, 0)].fg, th.dim);
    }

    #[test]
    fn x_labels_at_the_edges_and_middle() {
        let mut b = buf(20, 1);
        let a = b.area;
        x_labels(&mut b, a, ["-60s", "-30s", "now"], Style::new());
        assert_eq!(row_str(&b, 0), "-60s    -30s     now");
    }

    #[test]
    fn x_labels_middle_never_touches_the_others() {
        // Centred, it would overlap "-1234s": left out.
        let mut b = buf(12, 1);
        let a = b.area;
        x_labels(&mut b, a, ["-1234s", "-6s", "now"], Style::new());
        assert_eq!(row_str(&b, 0), "-1234s   now");
        // Room for a space on both sides: shown.
        let mut b = buf(17, 1);
        let a = b.area;
        x_labels(&mut b, a, ["-1234s", "-6s", "now"], Style::new());
        assert_eq!(row_str(&b, 0), "-1234s -6s    now");
        // Too close to the right one.
        let mut b = buf(10, 1);
        let a = b.area;
        x_labels(&mut b, a, ["", "-12345", "now"], Style::new());
        assert_eq!(row_str(&b, 0), "       now");
    }
}
