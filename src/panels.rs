//! btop-style system panels: CPU, memory, disks, network and processes.
//! Each panel's title starts with its toggle key in superscript (³cpu, ⁴mem…).

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, BorderType, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table},
    Frame,
};

use crate::sysstats::{human, human_bits, SysStats, HIST};
use crate::themes::Theme;

const SUP: [&str; 10] = ["⁰", "¹", "²", "³", "⁴", "⁵", "⁶", "⁷", "⁸", "⁹"];

/// A theme-colored rounded panel whose title starts with its toggle key.
pub fn block<'a>(th: &Theme, key: usize, title: Vec<Span<'a>>) -> Block<'a> {
    // Key 0 = a view without a toggle key (compact mode): no superscript.
    let mut spans = if key == 0 { Vec::new() } else { vec![Span::styled(SUP[key % 10], Style::new().fg(th.crit))] };
    spans.extend(title);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(th.border))
        .title(Line::from(spans).style(Style::new().fg(th.accent).add_modifier(Modifier::BOLD)))
}

/// Green → yellow → red as usage rises.
pub fn usage_color(pct: f32, th: &Theme) -> Color {
    if pct >= 85.0 {
        th.crit
    } else if pct >= 60.0 {
        th.warn
    } else {
        th.ok
    }
}

/// A row of `width` small squares, the first `frac` of them filled.
fn meter(frac: f64, width: usize, color: Color, th: &Theme) -> Vec<Span<'static>> {
    let n = ((frac.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    vec![
        Span::styled("■".repeat(n), Style::new().fg(color)),
        Span::styled("■".repeat(width - n), Style::new().fg(th.border)),
    ]
}

fn pct_of(part: u64, total: u64) -> f64 {
    if total == 0 { 0.0 } else { part as f64 / total as f64 }
}

// ---------------------------------------------------------------------------
// CPU
// ---------------------------------------------------------------------------

const CORE_CELL: u16 = 17;
/// Core cells are wider when each core also shows its own temperature.
/// "C5  ■■■■■■   3%  27°C  " — the plain cell plus the temperature.
const CORE_CELL_TEMP: u16 = 23;

/// A temperature with the warn/crit thresholds of the sensor it came from,
/// so it's colored exactly like that sensor's "Now" value in the table.
#[derive(Clone, Copy)]
pub struct TempVal {
    pub c: f32,
    pub warn: f32,
    pub crit: f32,
}

/// CPU temperatures for the CPU panel.
#[derive(Default)]
pub struct CpuTemps {
    /// Tctl / Package / hottest sensor.
    pub package: Option<(String, TempVal)>,
    /// AMD: one temperature per CCD with the cores it covers:
    /// ("CCD1", 52 °C, "C0–7, C16–23").
    pub ccds: Vec<(String, TempVal, String)>,
    /// A temperature for each logical CPU: its own core's sensor on Intel,
    /// its CCD's sensor on AMD. Empty when there are none.
    pub per_core: Vec<Option<TempVal>>,
    /// Show °F instead of °C (the `u` key).
    pub fahrenheit: bool,
}

impl CpuTemps {
    fn has_line(&self) -> bool {
        self.package.is_some() || !self.ccds.is_empty()
    }
    fn has_per_core(&self) -> bool {
        self.per_core.iter().any(|t| t.is_some())
    }
}

fn cell_w(t: &CpuTemps) -> u16 {
    if t.has_per_core() { CORE_CELL_TEMP } else { CORE_CELL }
}

/// Core columns that fit: the separator isn't needed after the last one.
fn core_cols(inner_w: u16, temps: &CpuTemps) -> usize {
    (inner_w / cell_w(temps)).max(1) as usize
}

pub fn cpu_height(width: u16, cores: usize, temps: &CpuTemps) -> u16 {
    let cols = core_cols(width.saturating_sub(2), temps);
    let rows = cores.div_ceil(cols).max(1) as u16;
    2 + 1 + temps.has_line() as u16 + 2 + rows
}

/// Same rule as the sensor table: crit at the sensor's critical temperature,
/// warn from 80 % of it (or 70/90 °C when the sensor reports no limit).
fn temp_color(t: TempVal, th: &Theme) -> Color {
    if t.c >= t.crit { th.crit } else if t.c >= t.warn { th.warn } else { th.ok }
}

/// "27°C" or "81°F".
fn fmt_temp(t: TempVal, fahrenheit: bool) -> String {
    if fahrenheit { format!("{:.0}°F", t.c * 9.0 / 5.0 + 32.0) } else { format!("{:.0}°C", t.c) }
}

/// `power`: CPU power draw in watts, or why it's missing ("needs root").
pub fn draw_cpu(
    f: &mut Frame,
    area: Rect,
    s: &SysStats,
    temps: &CpuTemps,
    power: (Option<f32>, Option<&str>),
    th: &Theme,
    key: usize,
) {
    // Live clock speed: the average over all cores, and the fastest core
    // (what a boosting core is doing while the rest idle).
    let ghz = |m: u64| m as f64 / 1000.0;
    let mhz = match (s.cpu_freq_mhz, s.cpu_freq_max_mhz) {
        (0, _) => String::new(),
        (avg, max) if max > avg + 50 => format!(" · {:.2} GHz avg · {:.2} max", ghz(avg), ghz(max)),
        (avg, _) => format!(" · {:.2} GHz", ghz(avg)),
    };
    let up = s.uptime;
    let block = block(
        th,
        key,
        vec![
            Span::raw("cpu "),
            Span::styled(format!("· {}{mhz} ", s.cpu_name), Style::new().fg(th.fg)),
            Span::styled(
                format!("· up {}d {}h {}m ", up / 86400, (up / 3600) % 24, (up / 60) % 60),
                Style::new().fg(th.dim),
            ),
        ],
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    // Total line, temperatures, the usage graph (gets every spare row, min 2), cores.
    let cols = core_cols(inner.width, temps);
    let core_rows = s.cpu_cores.len().div_ceil(cols).max(1) as u16;
    let temp_h = temps.has_line() as u16;
    let graph_h = inner.height.saturating_sub(1 + temp_h + core_rows).max(2);
    let [total, temp_line, spark, grid] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(temp_h),
        Constraint::Length(graph_h),
        Constraint::Min(0),
    ])
    .areas(inner);

    // Temperatures: "Temps  Tctl 55°C · CCD1 52°C (C0–7, C16–23) · CCD2 …"
    if temps.has_line() {
        let d = Style::new().fg(th.dim);
        let mut spans = vec![Span::styled("Temps ", Style::new().fg(th.fg).add_modifier(Modifier::BOLD))];
        let mut first = true;
        let mut sep = |spans: &mut Vec<Span>| {
            if !std::mem::take(&mut first) {
                spans.push(Span::styled("  ·  ", d));
            }
        };
        if let Some((label, t)) = &temps.package {
            sep(&mut spans);
            spans.push(Span::styled(format!("{label} "), d));
            spans.push(Span::styled(fmt_temp(*t, temps.fahrenheit), Style::new().fg(temp_color(*t, th)).add_modifier(Modifier::BOLD)));
        }
        for (label, t, cores) in &temps.ccds {
            sep(&mut spans);
            spans.push(Span::styled(format!("{label} "), d));
            spans.push(Span::styled(fmt_temp(*t, temps.fahrenheit), Style::new().fg(temp_color(*t, th))));
            if !cores.is_empty() {
                spans.push(Span::styled(format!(" ({cores})"), d));
            }
        }
        // On AMD the per-core values are the CCD sensors already listed.
        if temps.has_per_core() && temps.ccds.is_empty()
            && let Some((i, t)) = temps
                .per_core
                .iter()
                .enumerate()
                .filter_map(|(i, t)| t.map(|t| (i, t)))
                .max_by(|a, b| a.1.c.total_cmp(&b.1.c))
            {
                sep(&mut spans);
                spans.push(Span::styled("hottest core ", d));
                spans.push(Span::styled(fmt_temp(t, temps.fahrenheit), Style::new().fg(temp_color(t, th))));
                spans.push(Span::styled(format!(" (C{i})"), d));
            }
        f.render_widget(Paragraph::new(Line::from(spans)), temp_line);
    }

    // Total usage + load average.
    let load = format!("  load {:.2} {:.2} {:.2}", s.load.0, s.load.1, s.load.2);
    // Power draw after the usage: "  65 W", or why it's missing.
    let (watts, why) = match power {
        (Some(w), _) => (format!("  {}", crate::power::fmt_watts(w)), None),
        (None, Some(note)) => ("  power".to_string(), Some(format!(" — ({})", if note == "needs root" { "run with sudo" } else { note }))),
        (None, None) => (String::new(), None),
    };
    let watts_w = watts.chars().count() + why.as_ref().map_or(0, |w| w.chars().count());
    let mw = (inner.width as usize).saturating_sub(6 + 5 + watts_w + load.len()).max(4);
    let c = usage_color(s.cpu_total, th);
    let mut line = vec![Span::styled("Total ", Style::new().fg(th.fg).add_modifier(Modifier::BOLD))];
    line.extend(meter(s.cpu_total as f64 / 100.0, mw, c, th));
    line.push(Span::styled(format!("{:>4.0}%", s.cpu_total), Style::new().fg(c).add_modifier(Modifier::BOLD)));
    if why.is_some() {
        line.push(Span::styled(watts, Style::new().fg(th.dim)));
    } else {
        line.push(Span::styled(watts, Style::new().fg(th.fg).add_modifier(Modifier::BOLD)));
    }
    if let Some(w) = why {
        line.push(Span::styled(w, Style::new().fg(th.warn)));
    }
    line.push(Span::styled(load, Style::new().fg(th.dim)));
    f.render_widget(Paragraph::new(Line::from(line)), total);

    // Usage history as a braille line (still visible at 1%), 0–100 %.
    let off = HIST.saturating_sub(s.cpu_hist.len());
    let pts: Vec<(f64, f64)> =
        s.cpu_hist.iter().enumerate().map(|(i, v)| ((i + off) as f64, *v as f64)).collect();
    let peak = s.cpu_hist.iter().cloned().fold(0.0f32, f32::max);
    // Zoom in when usage is low so small changes show, but never below 10 %.
    let top = if peak < 10.0 { 10.0 } else if peak < 25.0 { 25.0 } else if peak < 50.0 { 50.0 } else { 100.0 };
    let chart = Chart::new(vec![
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(th.accent))
            .data(&pts),
    ])
    .x_axis(Axis::default().bounds([0.0, HIST as f64]))
    .y_axis(
        Axis::default()
            .bounds([0.0, top])
            .labels(vec![
                Span::styled("  0%", Style::new().fg(th.dim)),
                Span::styled(format!("{top:>3.0}%"), Style::new().fg(th.dim)),
            ]),
    );
    f.render_widget(chart, spark);

    // Per-core grid.
    let mut lines: Vec<Line> = Vec::new();
    for chunk in s.cpu_cores.chunks(cols).take(grid.height as usize) {
        let mut spans = Vec::new();
        for (j, u) in chunk.iter().enumerate() {
            let i = lines.len() * cols + j;
            let c = usage_color(*u, th);
            spans.push(Span::styled(format!("C{i:<3}"), Style::new().fg(th.dim)));
            spans.extend(meter(*u as f64 / 100.0, 6, c, th));
            spans.push(Span::styled(format!("{:>4.0}%", u), Style::new().fg(c)));
            match temps.per_core.get(i).copied().flatten() {
                Some(t) => spans.push(Span::styled(
                    format!("{:>6}  ", fmt_temp(t, temps.fahrenheit)),
                    Style::new().fg(temp_color(t, th)),
                )),
                None if temps.has_per_core() => spans.push(Span::raw("        ")),
                None => spans.push(Span::raw("  ")),
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), grid);
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

pub const MEM_HEIGHT: u16 = 8;

pub fn draw_mem(f: &mut Frame, area: Rect, s: &SysStats, th: &Theme, key: usize) {
    let block = block(th, key, vec![Span::raw("mem")]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let m = &s.mem;
    let rows: [(&str, u64, u64, Color); 5] = [
        ("Used", m.used, m.total, th.crit),
        ("Available", m.available, m.total, th.ok),
        ("Cached", m.cached, m.total, th.cool),
        ("Free", m.free, m.total, th.hot),
        ("Swap", m.swap_used, m.swap_total, th.warn),
    ];
    let w = inner.width as usize;
    let mw = w.saturating_sub(10 + 5 + 10).max(3);
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{:<10}", "Total"), Style::new().fg(th.fg).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:>w$}", human(m.total as f64), w = w.saturating_sub(10)), Style::new().fg(th.fg).add_modifier(Modifier::BOLD)),
    ])];
    for (label, part, total, color) in rows {
        let p = pct_of(part, total);
        let mut l = vec![Span::styled(format!("{label:<10}"), Style::new().fg(th.dim))];
        l.extend(meter(p, mw, color, th));
        l.push(Span::styled(format!("{:>4.0}%", p * 100.0), Style::new().fg(color)));
        l.push(Span::styled(format!("{:>10}", human(part as f64)), Style::new().fg(th.fg)));
        lines.push(Line::from(l));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Disks
// ---------------------------------------------------------------------------

pub fn disks_height(n: usize) -> u16 {
    2 + 2 * n.max(1) as u16
}

pub fn draw_disks(f: &mut Frame, area: Rect, s: &SysStats, th: &Theme, key: usize) {
    let block = block(th, key, vec![Span::raw("disks")]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let mut lines = Vec::new();
    for d in s.disks.iter().take((inner.height / 2) as usize) {
        let io = format!("▼{} ▲{}", short(d.read_rate), short(d.write_rate));
        let size = human(d.total as f64);
        let name_w = w.saturating_sub(io.chars().count() + size.len() + 2);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<name_w$}", ellipsize(&d.label, name_w)),
                Style::new().fg(th.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(io, Style::new().fg(th.dim)),
            Span::raw("  "),
            Span::styled(size, Style::new().fg(th.fg).add_modifier(Modifier::BOLD)),
        ]));
        let p = pct_of(d.used, d.total);
        let used = human(d.used as f64);
        let c = usage_color((p * 100.0) as f32, th);
        let mw = w.saturating_sub(6 + 5 + used.len() + 1).max(3);
        let mut l = vec![Span::styled("Used  ", Style::new().fg(th.dim))];
        l.extend(meter(p, mw, c, th));
        l.push(Span::styled(format!("{:>4.0}%", p * 100.0), Style::new().fg(c)));
        l.push(Span::styled(format!(" {used}"), Style::new().fg(th.fg)));
        lines.push(Line::from(l));
    }
    if s.disks.is_empty() {
        lines.push(Line::styled("no disks found", Style::new().fg(th.dim)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Compact rate for the disk IO column: "224K", "1.2M", "0".
fn short(bytes_per_s: f64) -> String {
    if bytes_per_s < 1.0 {
        return "0".into();
    }
    human(bytes_per_s).replace(" B", "").replace(" KiB", "K").replace(" MiB", "M").replace(" GiB", "G")
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

pub const NET_HEIGHT: u16 = 10;

pub fn draw_net(f: &mut Frame, area: Rect, s: &SysStats, th: &Theme, key: usize) {
    let Some(n) = s.nets.get(s.net_sel) else {
        let b = block(th, key, vec![Span::raw("net")]);
        let inner = b.inner(area);
        f.render_widget(b, area);
        f.render_widget(Paragraph::new(Line::styled("no network interfaces", Style::new().fg(th.dim))), inner);
        return;
    };
    let mut title = vec![
        Span::raw("net "),
        Span::styled(format!("· {} ", n.name), Style::new().fg(th.fg)),
    ];
    if let Some(ip) = &n.ip {
        title.push(Span::styled(format!("· {ip} "), Style::new().fg(th.dim)));
    }
    if s.nets.len() > 1 {
        title.push(Span::styled(format!("· n: next ({}/{}) ", s.net_sel + 1, s.nets.len()), Style::new().fg(th.dim)));
    }
    let b = block(th, key, title);
    let inner = b.inner(area);
    f.render_widget(b, area);

    let [graph, stats] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(26)]).areas(inner);

    // Download above the axis, upload mirrored below it, like btop.
    let off = HIST.saturating_sub(n.rx_hist.len());
    let rx: Vec<(f64, f64)> = n.rx_hist.iter().enumerate().map(|(i, v)| ((i + off) as f64, *v)).collect();
    let tx: Vec<(f64, f64)> = n.tx_hist.iter().enumerate().map(|(i, v)| ((i + off) as f64, -*v)).collect();
    let peak = n.rx_hist.iter().chain(n.tx_hist.iter()).cloned().fold(1024.0, f64::max) * 1.1;
    let chart = Chart::new(vec![
        Dataset::default().marker(symbols::Marker::Braille).graph_type(GraphType::Line).style(Style::new().fg(th.cool)).data(&rx),
        Dataset::default().marker(symbols::Marker::Braille).graph_type(GraphType::Line).style(Style::new().fg(th.hot)).data(&tx),
    ])
    .x_axis(Axis::default().bounds([0.0, HIST as f64]))
    .y_axis(
        Axis::default()
            .bounds([-peak, peak])
            .labels(vec![
                Span::styled(format!("{}/s", short(peak)), Style::new().fg(th.dim)),
                Span::raw(""),
                Span::styled(format!("{}/s", short(peak)), Style::new().fg(th.dim)),
            ]),
    );
    f.render_widget(chart, graph);

    let dl = Style::new().fg(th.cool).add_modifier(Modifier::BOLD);
    let ul = Style::new().fg(th.hot).add_modifier(Modifier::BOLD);
    let v = Style::new().fg(th.fg);
    let d = Style::new().fg(th.dim);
    let lines = vec![
        Line::from(vec![Span::styled(" ▼ download", dl)]),
        Line::from(vec![Span::styled(format!("   {}/s", human(n.rx_rate)), v)]),
        Line::from(vec![Span::styled(format!("   ({})", human_bits(n.rx_rate)), d)]),
        Line::from(vec![Span::styled("   Total ", d), Span::styled(human(n.rx_total as f64), v)]),
        Line::from(vec![Span::styled(" ▲ upload", ul)]),
        Line::from(vec![Span::styled(format!("   {}/s", human(n.tx_rate)), v)]),
        Line::from(vec![Span::styled(format!("   ({})", human_bits(n.tx_rate)), d)]),
        Line::from(vec![Span::styled("   Total ", d), Span::styled(human(n.tx_total as f64), v)]),
    ];
    f.render_widget(Paragraph::new(lines), stats);
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

/// What the process panel is doing besides listing: an open prompt.
#[derive(Clone, Debug, PartialEq)]
pub enum Prompt {
    /// Typing the search text (edits `ProcView::filter` live).
    Filter,
    /// Typing a command line to start.
    Exec(String),
    /// Typing a new nice value for a process.
    Nice { pid: u32, name: String, buf: String },
    /// "terminate/kill PID name? (y/n)".
    Confirm { sig: crate::procctl::Sig, pid: u32, name: String },
}

/// Selection, scrolling, search and prompt state of the process panel.
#[derive(Default)]
pub struct ProcView {
    /// The selected process. Followed by PID across refreshes and re-sorts.
    pub sel_pid: Option<u32>,
    /// Its row in the (filtered) list: used when that process exits.
    pub sel_idx: usize,
    /// First list row on screen.
    pub offset: usize,
    pub filter: String,
    pub prompt: Option<Prompt>,
    /// A short message (text, is-error) and when it was set.
    pub status: Option<(String, bool, std::time::Instant)>,
    /// Arrow keys and action keys go to this panel.
    pub focused: bool,
    /// Rows of processes that fit, from the last draw (for PgUp/PgDn).
    pub page: usize,
    /// Screen row of the first process, from the last draw (for clicks).
    pub first_row_y: u16,
}

impl ProcView {
    /// Indices into `procs` that pass the search.
    pub fn visible(&self, procs: &[crate::sysstats::ProcInfo]) -> Vec<usize> {
        (0..procs.len()).filter(|&i| crate::procctl::matches(&procs[i], &self.filter)).collect()
    }

    /// Row of the selected process in `vis` (falls back to the old row number
    /// when that process is gone), or None when the list is empty.
    pub fn cursor(&self, procs: &[crate::sysstats::ProcInfo], vis: &[usize]) -> Option<usize> {
        if vis.is_empty() {
            return None;
        }
        self.sel_pid
            .and_then(|pid| vis.iter().position(|&i| procs[i].pid == pid))
            .or(Some(self.sel_idx.min(vis.len() - 1)))
    }

    /// Select row `row` of `vis` (clamped) and keep it on screen.
    pub fn select(&mut self, procs: &[crate::sysstats::ProcInfo], vis: &[usize], row: isize) {
        if vis.is_empty() {
            self.sel_pid = None;
            self.sel_idx = 0;
            return;
        }
        let row = row.clamp(0, vis.len() as isize - 1) as usize;
        self.sel_idx = row;
        self.sel_pid = Some(procs[vis[row]].pid);
        self.scroll_to(row);
    }

    fn scroll_to(&mut self, row: usize) {
        let page = self.page.max(1);
        if row < self.offset {
            self.offset = row;
        } else if row >= self.offset + page {
            self.offset = row + 1 - page;
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>, err: bool) {
        self.status = Some((msg.into(), err, std::time::Instant::now()));
    }
}

/// A text-input caret.
const CARET: &str = "█";

pub fn draw_procs(f: &mut Frame, area: Rect, s: &SysStats, th: &Theme, key: usize, pv: &mut ProcView) {
    let vis = pv.visible(&s.procs);
    let mut title = vec![
        Span::raw("proc "),
        Span::styled(format!("· sort: {} (p) ", s.proc_sort.label()), Style::new().fg(th.fg)),
    ];
    if !pv.filter.is_empty() || pv.prompt == Some(Prompt::Filter) {
        title.push(Span::styled("· filter: ", Style::new().fg(th.dim)));
        title.push(Span::styled(pv.filter.clone(), Style::new().fg(th.warn)));
        if pv.prompt == Some(Prompt::Filter) {
            title.push(Span::styled(CARET, Style::new().fg(th.fg)));
        }
        title.push(Span::raw(" "));
    }
    let title_w: usize = 1 + title.iter().map(|s| s.content.chars().count()).sum::<usize>();
    let mut b = block(th, key, title);
    if pv.focused {
        b = b.border_style(Style::new().fg(th.accent));
    }
    // Status message (top right) for a few seconds after an action.
    // (Too little room next to the title: it goes on the bottom bar instead.)
    let mut bottom_status = None;
    if let Some((msg, err, at)) = &pv.status {
        let w = (area.width as usize).saturating_sub(title_w + 4);
        let style = Style::new().fg(if *err { th.crit } else { th.ok }).add_modifier(Modifier::BOLD);
        if at.elapsed() >= std::time::Duration::from_secs(5) {
            pv.status = None;
        } else if w < 30 || msg.chars().count() + 2 > w {
            bottom_status = Some(Span::styled(format!(" {msg} "), style));
        } else {
            b = b.title(
                Line::from(Span::styled(
                    format!(" {} ", ellipsize(msg, w.saturating_sub(2))),
                    Style::new().fg(if *err { th.crit } else { th.ok }).add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
            );
        }
    }

    let inner = b.inner(area);
    let rows_fit = inner.height.saturating_sub(1) as usize;
    pv.page = rows_fit;
    pv.first_row_y = inner.y + 1;

    // Keep the selection valid and on screen.
    let cur = pv.cursor(&s.procs, &vis);
    if let Some(c) = cur {
        pv.sel_idx = c;
        pv.sel_pid = Some(s.procs[vis[c]].pid);
        pv.offset = pv.offset.min(vis.len().saturating_sub(rows_fit.max(1)));
        pv.scroll_to(c);
    } else {
        pv.offset = 0;
    }

    // Bottom border: the action bar (or the open prompt), and n/total.
    let total = s.proc_count;
    let count = if pv.filter.is_empty() {
        format!(" {}/{} ", cur.map_or(0, |c| c + 1), total)
    } else {
        format!(" {}/{} of {} ", cur.map_or(0, |c| c + 1), vis.len(), total)
    };
    b = b.title_bottom(Line::from(Span::styled(count.clone(), Style::new().fg(th.fg))).right_aligned());
    let bar_w = area.width.saturating_sub(count.chars().count() as u16 + 4) as usize;
    b = b.title_bottom(match bottom_status {
        Some(sp) if pv.prompt.is_none() => {
            let mut l = Line::from(sp);
            let w = l.width();
            if w > bar_w {
                l = Line::from(Span::styled(ellipsize(&l.spans[0].content, bar_w), l.spans[0].style));
            }
            l
        }
        _ => action_bar(pv, th, bar_w),
    });

    let mut cmd_w = inner.width.saturating_sub(8 + 16 + 11 + 9 + 7 + 4 + 5) as usize;
    if cmd_w < 8 {
        cmd_w = 0;
    }
    let rows: Vec<Row> = vis
        .iter()
        .enumerate()
        .skip(pv.offset)
        .take(rows_fit)
        .map(|(row, &i)| {
            let p = &s.procs[i];
            let nice = p.nice.map_or_else(|| "  ".to_string(), |n| format!("{n:>3}"));
            let nice_color = match p.nice {
                Some(n) if n < 0 => th.warn,
                Some(n) if n > 0 => th.cool,
                _ => th.dim,
            };
            let r = Row::new(vec![
                Cell::from(format!("{:>7}", p.pid)).style(Style::new().fg(th.dim)),
                Cell::from(ellipsize(&p.name, 15)).style(Style::new().fg(th.ok)),
                Cell::from(ellipsize(&p.cmd, cmd_w)).style(Style::new().fg(th.dim)),
                Cell::from(nice).style(Style::new().fg(nice_color)),
                Cell::from(ellipsize(&p.user, 10)).style(Style::new().fg(th.fg)),
                Cell::from(format!("{:>8}", human(p.mem as f64).replace("iB", "").replace(" B", "B").replace(" ", ""))).style(Style::new().fg(th.cool)),
                Cell::from(format!("{:>6.1}", p.cpu)).style(Style::new().fg(usage_color(p.cpu.min(100.0), th))),
            ]);
            if Some(row) == cur {
                // Bright when the panel has focus, faint otherwise.
                let st = Style::new().bg(th.sel_bg);
                r.style(if pv.focused { st.add_modifier(Modifier::BOLD) } else { st })
            } else {
                r
            }
        })
        .collect();
    let header = Row::new(vec!["    Pid", "Program", "Command", " Ni", "User", "     Mem", "  Cpu%"])
        .style(Style::new().fg(th.accent).add_modifier(Modifier::BOLD));
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(15),
            Constraint::Fill(1),
            Constraint::Length(3),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(6),
        ],
    )
    .header(header)
    .block(b);
    f.render_widget(table, area);

    if vis.is_empty() && rows_fit > 0 {
        let msg = if pv.filter.is_empty() { "no processes" } else { "no process matches the filter" };
        f.render_widget(
            Paragraph::new(Line::styled(msg, Style::new().fg(th.dim))).centered(),
            Rect { y: inner.y + 1 + (rows_fit as u16) / 2, height: 1, ..inner },
        );
    }
}

/// The process panel's bottom-border bar: key actions like btop's
/// (`↑ select ↓ │ f filter │ e exec │ t terminate │ k kill │ n nice`), or the
/// open prompt. Items that don't fit in `width` are dropped from the end.
fn action_bar(pv: &ProcView, th: &Theme, width: usize) -> Line<'static> {
    let key = Style::new().fg(th.accent).add_modifier(Modifier::BOLD);
    let txt = Style::new().fg(th.fg);
    let dim = Style::new().fg(th.dim);
    let caret = Span::styled(CARET, Style::new().fg(th.fg));
    let spans: Vec<Span<'static>> = match &pv.prompt {
        Some(Prompt::Filter) => vec![
            Span::styled(
                if width >= 62 { " type to search name, command, user or PID │ " } else { " search │ " },
                dim,
            ),
            Span::styled("↵", key),
            Span::styled(" keep │ ", txt),
            Span::styled("esc", key),
            Span::styled(" clear ", txt),
        ],
        Some(Prompt::Exec(buf)) => vec![
            Span::styled(" run: ", key),
            Span::styled(buf.clone(), txt),
            caret,
            Span::styled("  ↵ start │ esc cancel ", dim),
        ],
        Some(Prompt::Nice { pid, name, buf }) => vec![
            Span::styled(format!(" nice for {pid} {name} (-20…19): "), key),
            Span::styled(buf.clone(), txt),
            caret,
            Span::styled("  ↵ set │ esc cancel ", dim),
        ],
        Some(Prompt::Confirm { sig, pid, name }) => {
            let verb = match sig {
                crate::procctl::Sig::Term => "terminate (SIGTERM)",
                crate::procctl::Sig::Kill => "kill (SIGKILL)",
            };
            vec![
                Span::styled(format!(" {verb} {pid} {name}? "), Style::new().fg(th.crit).add_modifier(Modifier::BOLD)),
                Span::styled("y", key),
                Span::styled(" yes │ ", txt),
                Span::styled("n", key),
                Span::styled(" no ", txt),
            ]
        }
        None => {
            // (key, label) items; the first is the focus hint when unfocused.
            // Full labels when they fit, else short ones, else drop from the end.
            let full: [(&str, &str); 6] = [
                ("↑", " select ↓"),
                ("f", if pv.filter.is_empty() { " filter" } else { " filter (esc clear)" }),
                ("e", " exec"),
                ("t", " terminate"),
                ("k", " kill"),
                ("n", " nice"),
            ];
            let short: [(&str, &str); 6] =
                [("↑↓", ""), ("f", " filter"), ("e", " exec"), ("t", " term"), ("k", " kill"), ("n", " nice")];
            let tab: &[(&str, &str)] = if pv.focused { &[] } else { &[("tab", " focus")] };
            let len = |v: &[(&str, &str)]| -> usize {
                2 + v.iter().map(|(k, l)| k.chars().count() + l.chars().count()).sum::<usize>() + 3 * v.len().saturating_sub(1)
            };
            let mut items: Vec<(&str, &str)> = tab.iter().chain(full.iter()).copied().collect();
            if len(&items) > width {
                items = tab.iter().chain(short.iter()).copied().collect();
            }
            let mut out = vec![Span::raw(" ")];
            let mut used = 1;
            let st = |s: Style| if pv.focused { s } else { dim };
            for (n, (k, label)) in items.iter().enumerate() {
                let w = k.chars().count() + label.chars().count() + if n > 0 { 3 } else { 0 };
                if used + w + 1 > width {
                    break;
                }
                if n > 0 {
                    out.push(Span::styled(" │ ", dim));
                }
                // The tab hint stays bright so it's noticed.
                out.push(Span::styled(k.to_string(), if *k == "tab" { key } else { st(key) }));
                out.push(Span::styled(label.to_string(), st(txt)));
                used += w;
            }
            out.push(Span::raw(" "));
            return Line::from(out);
        }
    };
    // Prompts: trim from the start of the input if too long (keep the caret).
    let mut line = Line::from(spans);
    let width_of = |l: &Line| l.spans.iter().map(|s| s.content.chars().count()).sum::<usize>();
    // First drop the trailing key hint, then cut from the left.
    if width_of(&line) > width && matches!(pv.prompt, Some(Prompt::Exec(_)) | Some(Prompt::Nice { .. })) {
        line.spans.pop();
    }
    let w = width_of(&line);
    if w > width {
        let mut over = w - width;
        for sp in line.spans.iter_mut() {
            let n = sp.content.chars().count();
            if over == 0 {
                break;
            }
            let cut = over.min(n);
            sp.content = sp.content.chars().skip(cut).collect::<String>().into();
            over -= cut;
        }
    }
    line
}

fn ellipsize(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else if w == 0 {
        String::new()
    } else {
        s.chars().take(w - 1).chain(std::iter::once('…')).collect()
    }
}

// ---------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------

/// Rows per GPU: usage line, graph (≥2), VRAM line, stats line.
const GPU_ROWS: u16 = 1 + 2 + 1 + 1;

/// Width of one "A0 56°C" cell in the VRAM chip rows, for these labels.
fn chip_cell_w(chips: &[(String, f32)]) -> usize {
    chips.iter().map(|(l, _)| l.chars().count()).max().unwrap_or(2) + 1 + 5 + 2
}

/// How many rows the VRAM chip list needs at this inner width.
fn chip_rows(chips: &[(String, f32)], inner_w: u16) -> u16 {
    if chips.is_empty() {
        return 0;
    }
    let per_row = ((inner_w as usize).saturating_sub(6) / chip_cell_w(chips)).max(1);
    chips.len().div_ceil(per_row) as u16
}

pub fn gpu_height(gpus: &[crate::gpustats::GpuView], width: u16) -> u16 {
    let inner = width.saturating_sub(2);
    2 + gpus.iter().map(|g| GPU_ROWS + chip_rows(&g.vram_chips, inner)).sum::<u16>().max(GPU_ROWS)
}

pub fn draw_gpu(f: &mut Frame, area: Rect, gpus: &[crate::gpustats::GpuView], th: &Theme, key: usize, fahrenheit: bool) {
    let title_name = match gpus {
        [g] => g.name.clone(),
        _ => format!("{} GPUs", gpus.len()),
    };
    let block = block(
        th,
        key,
        vec![Span::raw("gpu "), Span::styled(format!("· {title_name} "), Style::new().fg(th.fg))],
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if gpus.is_empty() || inner.height == 0 {
        return;
    }
    // Share the height between GPUs; each gets its fixed rows and the graph
    // takes whatever is left over.
    let each = Layout::vertical(
        gpus.iter().map(|g| Constraint::Min(GPU_ROWS + chip_rows(&g.vram_chips, inner.width))),
    )
    .split(inner);
    for (g, area) in gpus.iter().zip(each.iter()) {
        draw_one_gpu(f, *area, g, gpus.len() > 1, th, fahrenheit);
    }
}

fn draw_one_gpu(f: &mut Frame, area: Rect, g: &crate::gpustats::GpuView, show_name: bool, th: &Theme, fahrenheit: bool) {
    // Temperatures follow the °C/°F setting (`u`); thresholds stay in °C.
    let deg = |c: f32| if fahrenheit { format!("{:.0}°F", c * 9.0 / 5.0 + 32.0) } else { format!("{c:.0}°C") };
    let chips_h = chip_rows(&g.vram_chips, area.width);
    let graph_h = area.height.saturating_sub(3 + chips_h).max(1);
    let [usage, graph, vram, stats, chips] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(graph_h),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(chips_h),
    ])
    .areas(area);
    let w = area.width as usize;

    // Usage line: like the CPU "Total" line.
    let util = g.util.unwrap_or(0.0);
    let c = usage_color(util, th);
    let label = if show_name { format!("{:<6}", "GPU") } else { "Usage ".to_string() };
    let name_tail = if show_name { format!("  {}", g.name) } else { String::new() };
    let mw = w.saturating_sub(label.len() + 5 + name_tail.chars().count()).max(4);
    let mut line = vec![Span::styled(label, Style::new().fg(th.fg).add_modifier(Modifier::BOLD))];
    line.extend(meter(util as f64 / 100.0, mw, c, th));
    line.push(Span::styled(
        match g.util {
            Some(u) => format!("{u:>4.0}%"),
            None => "  n/a".into(),
        },
        Style::new().fg(c).add_modifier(Modifier::BOLD),
    ));
    if show_name {
        line.push(Span::styled(name_tail, Style::new().fg(th.dim)));
    }
    f.render_widget(Paragraph::new(Line::from(line)), usage);

    // Usage history, same zooming line graph as the CPU panel.
    let off = HIST.saturating_sub(g.hist.len());
    let pts: Vec<(f64, f64)> = g.hist.iter().enumerate().map(|(i, v)| ((i + off) as f64, *v as f64)).collect();
    let peak = g.hist.iter().cloned().fold(0.0f32, f32::max);
    let top = if peak < 10.0 { 10.0 } else if peak < 25.0 { 25.0 } else if peak < 50.0 { 50.0 } else { 100.0 };
    let chart = Chart::new(vec![
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(crate::themes::kind_color("GPU", th)))
            .data(&pts),
    ])
    .x_axis(Axis::default().bounds([0.0, HIST as f64]))
    .y_axis(
        Axis::default().bounds([0.0, top]).labels(vec![
            Span::styled("  0%", Style::new().fg(th.dim)),
            Span::styled(format!("{top:>3.0}%"), Style::new().fg(th.dim)),
        ]),
    );
    f.render_widget(chart, graph);

    // VRAM.
    if let (Some(used), Some(total)) = (g.vram_used, g.vram_total) {
        let p = if total > 0.0 { used / total } else { 0.0 };
        let txt = format!(" {} / {}", human(used), human(total));
        let mw = w.saturating_sub(6 + 5 + txt.len()).max(3);
        let c = usage_color((p * 100.0) as f32, th);
        let mut l = vec![Span::styled("VRAM  ", Style::new().fg(th.dim))];
        l.extend(meter(p, mw, c, th));
        l.push(Span::styled(format!("{:>4.0}%", p * 100.0), Style::new().fg(c)));
        l.push(Span::styled(txt, Style::new().fg(th.fg)));
        f.render_widget(Paragraph::new(Line::from(l)), vram);
    } else {
        f.render_widget(Paragraph::new(Line::styled("VRAM  n/a", Style::new().fg(th.dim))), vram);
    }

    // Temperature · power · fan · clocks, whatever the driver reports.
    let d = Style::new().fg(th.dim);
    let v = Style::new().fg(th.fg);
    let mut parts: Vec<Span> = Vec::new();
    let mut add = |label: &str, value: String, style: Style| {
        if !parts.is_empty() {
            parts.push(Span::styled("  ·  ", d));
        }
        parts.push(Span::styled(format!("{label} "), d));
        parts.push(Span::styled(value, style));
    };
    if let Some(t) = g.temp {
        let c = if t >= 85.0 { th.crit } else if t >= 70.0 { th.warn } else { th.ok };
        let src = if g.on_cpu_die { " (iGPU)" } else { "" };
        add("Temp", format!("{}{src}", deg(t)), Style::new().fg(c));
    }
    match g.hotspot {
        Some(t) => {
            let c = if t >= 95.0 { th.crit } else if t >= 80.0 { th.warn } else { th.ok };
            add("Hotspot", deg(t), Style::new().fg(c));
        }
        None if g.nvidia => match g.hotspot_note {
            // RTX 50 hotspot registers are root-only in NVIDIA's driver.
            Some("needs root") => add("Hotspot", "— (run with sudo)".into(), Style::new().fg(th.warn)),
            _ => add("Hotspot", "—".into(), d),
        },
        None => {}
    }
    match g.vram_temp {
        Some(t) => {
            // GDDR6X/GDDR7 are rated to ~95–105 °C; use the card's limit if known.
            let crit = g.vram_crit.unwrap_or(100.0);
            let c = if t >= crit { th.crit } else if t >= crit - 10.0 { th.warn } else { th.ok };
            add("VRAM", deg(t), Style::new().fg(c));
        }
        // NVIDIA GeForce: not published by the driver (see README).
        None if g.nvidia => add("VRAM", "—".into(), d),
        None => {}
    }
    if let Some(pw) = g.power_w {
        let txt = match g.power_limit_w {
            Some(l) => format!("{pw:.0} / {l:.0} W"),
            None => format!("{pw:.0} W"),
        };
        add("Power", txt, v);
    }
    if let Some(fan) = g.fan_pct {
        add("Fan", format!("{fan:.0}%"), v);
    }
    if let Some(mhz) = g.core_mhz {
        // 0 while the GPU sleeps between frames.
        add("Core", if mhz > 0.0 { format!("{mhz:.0} MHz") } else { "idle".into() }, v);
    }
    if let Some(mhz) = g.mem_mhz {
        add("Mem", format!("{mhz:.0} MHz"), v);
    }
    // When NVIDIA's VRAM/hotspot aren't readable, say where to find out why.
    if g.nvidia && (g.vram_temp.is_none() || g.hotspot.is_none()) && g.hotspot_note != Some("needs root") {
        parts.push(Span::styled("   (why —? oryn --gpu-info)", d));
    }
    f.render_widget(Paragraph::new(Line::from(parts)), stats);

    // Every VRAM chip, like LACT: "Chips  A0 56°C  A1 56°C  B0 62°C …".
    if !g.vram_chips.is_empty() {
        let cell = chip_cell_w(&g.vram_chips);
        let per_row = ((area.width as usize).saturating_sub(6) / cell).max(1);
        let lw = cell - 8;
        let crit = g.vram_crit.unwrap_or(100.0);
        let hottest = g.vram_chips.iter().map(|c| c.1).fold(f32::MIN, f32::max);
        let lines: Vec<Line> = g
            .vram_chips
            .chunks(per_row)
            .enumerate()
            .map(|(row, chunk)| {
                let mut spans =
                    vec![Span::styled(if row == 0 { "Chips " } else { "      " }, d)];
                for (label, t) in chunk {
                    let c = if *t >= crit { th.crit } else if *t >= crit - 10.0 { th.warn } else { th.ok };
                    let mut style = Style::new().fg(c);
                    if *t == hottest {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    spans.push(Span::styled(format!("{label:<lw$} "), d));
                    spans.push(Span::styled(format!("{:>5}  ", deg(*t)), style));
                }
                Line::from(spans)
            })
            .collect();
        f.render_widget(Paragraph::new(lines), chips);
    }
}

// ---------------------------------------------------------------------------
// System specs
// ---------------------------------------------------------------------------

pub fn draw_specs(f: &mut Frame, area: Rect, specs: Option<&crate::specs::Specs>, th: &Theme, key: usize) {
    let b = block(th, key, vec![Span::raw("specs")]);
    let inner = b.inner(area);
    f.render_widget(b, area);
    let Some(specs) = specs else {
        f.render_widget(Paragraph::new(Line::styled("gathering…", Style::new().fg(th.dim))), inner);
        return;
    };
    let up = sysinfo::System::uptime();
    let w = inner.width as usize;
    let lines: Vec<Line> = specs
        .lines
        .iter()
        .map(|l| {
            let color = if l.kind.is_empty() { th.accent } else { crate::themes::kind_color(l.kind, th) };
            let value = if l.label == "Uptime" {
                format!("{}d {}h {}m", up / 86400, (up / 3600) % 24, (up / 60) % 60)
            } else {
                l.value.clone()
            };
            Line::from(vec![
                Span::styled(format!("{:<9}", l.label), Style::new().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(ellipsize(&value, w.saturating_sub(9)), Style::new().fg(th.fg)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn integrated_gpu_temp_and_idle_clock() {
        let g = crate::gpustats::GpuView {
            name: "Intel Iris Xe Graphics".into(),
            util: Some(6.0),
            temp: Some(44.0),
            on_cpu_die: true,
            core_mhz: Some(0.0),
            ..Default::default()
        };
        let mut term = Terminal::new(TestBackend::new(80, 9)).unwrap();
        term.draw(|f| draw_gpu(f, f.area(), &[g], &crate::themes::THEMES[0], 8, false)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Temp 44°C (iGPU)"), "{text}");
        assert!(text.contains("Core idle"), "{text}");
    }
}
