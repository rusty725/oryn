//! Oryn — a terminal hardware monitor: temperatures first, plus CPU, GPU,
//! memory, disks, network and processes.
//!
//! Keys (see README for all of them):
//!
//! | Key        | Action                                              |
//! |------------|-----------------------------------------------------|
//! | ↑/↓, k/j   | select sensor                                       |
//! | Tab        | process list keys: f filter, e exec, t/k stop, n nice |
//! | PgUp/PgDn  | scroll the panels when the window is too short      |
//! | u          | toggle °C / °F                                      |
//! | + / -      | faster / slower refresh                             |
//! | s          | cycle sort (name / temp)                            |
//! | r          | reset min/max stats                                 |
//! | l, L       | write everything shown to a log in /tmp             |
//! | q / Esc    | quit                                                |
//!
//! Run with `--demo` to use simulated sensors (handy on VMs/containers
//! or machines where no sensors are exposed).

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{
        Axis, Block, BorderType, Cell, Chart, Clear, Dataset, GraphType, List, ListItem,
        ListState, Paragraph, Row, Table, TableState,
    },
    DefaultTerminal, Frame,
};
#[cfg(target_os = "linux")]
mod devnames;
mod battery;
mod gpu;

/// Developer/test overrides: `ORYN_<KEY>` (or the pre-rename `TEMPMON_<KEY>`),
/// e.g. ORYN_SYSFS=/path/to/fake/root.
pub fn env_os(key: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(format!("ORYN_{key}")).or_else(|| std::env::var_os(format!("TEMPMON_{key}")))
}

pub fn env_str(key: &str) -> Option<String> {
    env_os(key).and_then(|v| v.into_string().ok())
}
mod gpustats;
mod memprobe;

// Counts the bytes Oryn's code holds, for `--mem-probe`; otherwise the
// system allocator unchanged.
#[global_allocator]
static ALLOC: memprobe::Counting = memprobe::Counting;
#[cfg(target_os = "linux")]
mod nvapi;
mod panels;
mod power;
mod procctl;
mod report;
#[cfg(test)]
mod screenshot;
mod sensors;
mod smart;
mod specs;
mod sysstats;
mod themes;
use sensors::{DemoSource, Reading, Source};
use themes::{Theme, THEMES};
use sysstats::{SysStats, Want};

/// Toggleable panels: (key name, description). Keys 1–9 follow this order.
const PANELS: [(&str, &str); 9] = [
    ("sensors", "Temperature sensor table"),
    ("history", "Temperature history chart"),
    ("cpu", "CPU usage, per core"),
    ("mem", "Memory and swap"),
    ("disks", "Disk usage and I/O"),
    ("net", "Network traffic"),
    ("proc", "Processes"),
    ("gpu", "GPU usage, VRAM, power"),
    ("specs", "System specs"),
];
const P_SENSORS: usize = 0;
const P_HISTORY: usize = 1;
const P_CPU: usize = 2;
const P_MEM: usize = 3;
const P_DISKS: usize = 4;
const P_NET: usize = 5;
const P_PROC: usize = 6;
const P_GPU: usize = 7;
const P_SPECS: usize = 8;
const NP: usize = PANELS.len();

fn panel_index(name: &str) -> Option<usize> {
    let p = name.to_ascii_lowercase();
    let alias = match p.as_str() {
        "chart" => "history",
        "memory" => "mem",
        "processes" | "procs" => "proc",
        "network" => "net",
        "temps" => "sensors",
        "gpus" => "gpu",
        "spec" | "system" | "sysinfo" => "specs",
        other => other,
    };
    PANELS.iter().position(|(n, _)| *n == alias)
}

/// "sensors,cpu,net" -> visibility flags (listed = shown, others hidden).
/// "all"/"none" work too. Used for --panels.
fn parse_panels(spec: &str) -> Option<[bool; NP]> {
    let mut show = [false; NP];
    for part in spec.split([',', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
        match part {
            // "all,-proc" = everything but processes.
            "all" => show = [true; NP],
            "none" => show = [false; NP],
            p => show[panel_index(p.trim_start_matches('-'))?] = !p.starts_with('-'),
        }
    }
    Some(show)
}

fn compact_file() -> Option<std::path::PathBuf> {
    Some(themes::config_dir()?.join("compact"))
}

fn load_compact() -> bool {
    compact_file().and_then(|p| std::fs::read_to_string(p).ok()).is_some_and(|s| s.trim() == "on")
}

fn save_compact(on: bool) {
    themes::write_config("compact", if on { "on\n" } else { "off\n" });
}

fn panels_file() -> Option<std::path::PathBuf> {
    Some(themes::config_dir()?.join("panels"))
}

/// The saved file lists every panel, hidden ones with a leading "-"
/// ("sensors,cpu,-proc"). Panels it doesn't mention — ones added in a newer
/// version — start shown.
/// User-arranged panel order and columns (the `v` menu: Ctrl+Shift+↑/↓, ←/→).
#[derive(Clone, PartialEq, Debug)]
struct PanelLayout {
    /// Panel indices, top to bottom.
    order: Vec<usize>,
    /// Left column (true) or right column (false) on wide terminals.
    left: [bool; NP],
}

impl Default for PanelLayout {
    fn default() -> Self {
        let mut left = [false; NP];
        for p in [P_SENSORS, P_SPECS, P_HISTORY] {
            left[p] = true;
        }
        Self {
            order: vec![P_SENSORS, P_SPECS, P_HISTORY, P_CPU, P_GPU, P_MEM, P_DISKS, P_NET, P_PROC],
            left,
        }
    }
}

impl PanelLayout {
    /// The default arrangement keeps Oryn's automatic balancing; a
    /// customised one is followed exactly.
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
    fn move_by(&mut self, panel: usize, delta: isize) {
        if let Some(i) = self.order.iter().position(|&p| p == panel) {
            let j = (i as isize + delta).clamp(0, self.order.len() as isize - 1) as usize;
            let p = self.order.remove(i);
            self.order.insert(j, p);
        }
    }
}

fn load_panels() -> Option<([bool; NP], PanelLayout)> {
    parse_saved(&std::fs::read_to_string(panels_file()?).ok()?)
}

/// The saved file lists every panel in display order, hidden ones with a
/// leading "-" and each with its column: "sensors:L,-history:L,cpu:R,…".
/// Older files (no ":L/:R") only recorded visibility; panels a file doesn't
/// mention — ones added in a newer version — start shown, in default position.
fn parse_saved(text: &str) -> Option<([bool; NP], PanelLayout)> {
    let mut show = [true; NP];
    let mut lay = PanelLayout::default();
    let mut order = Vec::new();
    let mut any = false;
    for part in text.trim().split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if part == "none" {
            // Old format meaning "everything hidden".
            return Some(([false; NP], lay));
        }
        let (name, side) = match part.split_once(':') {
            Some((n, side)) => (n, Some(side)),
            None => (part, None),
        };
        if let Some(i) = panel_index(name.trim_start_matches('-')) {
            show[i] = !name.starts_with('-');
            any = true;
            if let Some(side) = side {
                lay.left[i] = side.eq_ignore_ascii_case("l");
                if !order.contains(&i) {
                    order.push(i);
                }
            }
        }
    }
    // Old files listed only the shown panels: anything unlisted from back
    // then was hidden, so treat the original seven that way.
    if any && !text.contains('-') && !text.contains(':') {
        for i in 0..P_GPU {
            if !text.trim().split(',').any(|p| panel_index(p.trim()) == Some(i)) {
                show[i] = false;
            }
        }
    }
    if !order.is_empty() {
        // Panels missing from the saved order go where the default puts them.
        let default = PanelLayout::default().order;
        for (k, &p) in default.iter().enumerate() {
            if !order.contains(&p) {
                order.insert(k.min(order.len()), p);
            }
        }
        lay.order = order;
    }
    Some((show, lay))
}

fn save_panels(show: &[bool; NP], lay: &PanelLayout) {
    {
        let parts: Vec<String> = lay
            .order
            .iter()
            .map(|&i| {
                let side = if lay.left[i] { "L" } else { "R" };
                let name = PANELS[i].0;
                if show[i] { format!("{name}:{side}") } else { format!("-{name}:{side}") }
            })
            .collect();
        themes::write_config("panels", &format!("{}\n", parts.join(",")));
    }
}

const HISTORY_LEN: usize = 120;
const DEFAULT_WARN: f32 = 70.0;
const DEFAULT_CRIT: f32 = 90.0;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct Sensor {
    id: String,
    label: String,
    device: String,
    cpu: bool,
    kind: &'static str,
    note: Option<String>,
    current: Option<f32>,
    min: f32,
    max: f32,
    critical: Option<f32>,
    history: VecDeque<f32>,
    /// When the sensor stopped being reported (device unplugged), if it has.
    gone_since: Option<Instant>,
}

/// How long an unplugged sensor stays listed as "disconnected". After that
/// it's dropped, so repeatedly plugging in a USB drive (which gets a new
/// device address each time) doesn't pile up rows.
const GONE_KEEP: Duration = Duration::from_secs(60);

impl Sensor {
    fn new(r: &Reading) -> Self {
        Self {
            id: r.id.clone(),
            label: r.label.clone(),
            device: r.device.clone(),
            cpu: r.cpu,
            kind: r.kind,
            note: None,
            current: None,
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            critical: None,
            history: VecDeque::with_capacity(HISTORY_LEN),
            gone_since: None,
        }
    }
    fn push(&mut self, r: &Reading) {
        self.gone_since = None;
        self.note = r.note.clone();
        self.kind = r.kind; // SMART learns HDD vs SSD after its first poll
        if !r.device.is_empty() {
            self.device = r.device.clone(); // SMART fills in the model after its first poll
        }
        self.critical = r.critical;
        self.current = r.temp;
        if let Some(t) = r.temp {
            self.min = self.min.min(t);
            self.max = self.max.max(t);
            if self.history.len() == HISTORY_LEN {
                self.history.pop_front();
            }
            self.history.push_back(t);
        }
    }
    fn crit_threshold(&self) -> f32 {
        self.critical.unwrap_or(DEFAULT_CRIT)
    }
    fn warn_threshold(&self) -> f32 {
        // Warn at 80% of critical, or the default.
        self.critical.map(|c| c * 0.8).unwrap_or(DEFAULT_WARN)
    }
    fn level(&self, t: f32) -> Level {
        if t >= self.crit_threshold() {
            Level::Crit
        } else if t >= self.warn_threshold() {
            Level::Warn
        } else {
            Level::Ok
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Ok,
    Warn,
    Crit,
}

impl Level {
    fn color(self, th: &Theme) -> Color {
        match self {
            Level::Ok => th.ok,
            Level::Warn => th.warn,
            Level::Crit => th.crit,
        }
    }
}

#[derive(Clone, Copy)]
enum Unit {
    C,
    F,
}

impl Unit {
    fn conv(self, c: f32) -> f32 {
        match self {
            Unit::C => c,
            Unit::F => c * 9.0 / 5.0 + 32.0,
        }
    }
    fn fmt(self, c: f32) -> String {
        match self {
            Unit::C => format!("{:.1}°C", c),
            Unit::F => format!("{:.1}°F", self.conv(c)),
        }
    }
}

#[derive(Clone, Copy)]
enum Sort {
    Name,
    TempDesc,
}

struct App {
    source: Box<dyn Source>,
    sensors: Vec<Sensor>,
    order: Vec<usize>,
    table: TableState,
    unit: Unit,
    sort: Sort,
    interval: Duration,
    /// Index into THEMES.
    theme: usize,
    /// Open theme picker: the theme to go back to on Esc, and the list cursor.
    picker: Option<(usize, ListState)>,
    /// Which panels are shown (see PANELS).
    show: [bool; NP],
    /// Panel order and columns.
    lay: PanelLayout,
    /// Compact mode (`c`): only temperatures, no graphs or bars.
    compact: bool,
    /// Open panel checklist (`v`): its cursor.
    view: Option<ListState>,
    stats: SysStats,
    gpu: gpustats::GpuStats,
    /// Physical core / L3 (CCD) of each logical CPU, read once at start.
    topo: Vec<sysstats::CpuTopo>,
    /// System specs, gathered when the specs panel is first shown (and again
    /// if the GPU list changes, since NVIDIA details arrive a moment later).
    specs: Option<(specs::Specs, usize)>,
    /// CPU power draw (RAPL energy counter, or zenpower).
    cpu_power: power::PowerMeter,
    /// RAM power draw (RAPL "dram" zone; most machines don't have one).
    ram_power: power::PowerMeter,
    /// The system battery; None on desktops.
    battery: Option<battery::Battery>,
    /// Process panel: selection, scroll, search, prompts.
    pv: panels::ProcView,
    /// Tab: arrow and action keys go to the process list instead of sensors.
    focus_proc: bool,
    /// Panels that are switched on but scrolled out of the last frame because
    /// the terminal is too short for all of them.
    offscreen: std::cell::RefCell<Offscreen>,
    /// How many panels the overflowing columns are scrolled down (PgUp/PgDn,
    /// or the wheel outside the sensor table and process list).
    scroll: usize,
    /// Where the sensor table and process list were last drawn (mouse hits).
    table_rect: Rect,
    proc_rect: Rect,
    /// A short message for the footer (text, is-error) and when it was set,
    /// e.g. where `l` saved the log.
    flash: Option<(String, bool, Instant)>,
}

impl App {
    fn new(source: Box<dyn Source>, interval: Duration, theme: usize, show: [bool; NP], lay: PanelLayout) -> Self {
        let sysfs = std::path::PathBuf::from(crate::env_os("SYSFS").unwrap_or_else(|| "/".into()));
        let mut app = Self {
            source,
            sensors: Vec::new(),
            order: Vec::new(),
            table: TableState::default(),
            unit: Unit::C,
            sort: Sort::Name,
            interval,
            theme,
            picker: None,
            show,
            lay,
            compact: false,
            view: None,
            stats: SysStats::new(),
            gpu: Default::default(),
            topo: Vec::new(),
            specs: None,
            cpu_power: power::PowerMeter::cpu(&sysfs),
            ram_power: power::PowerMeter::dram(&sysfs),
            battery: None,
            pv: Default::default(),
            focus_proc: false,
            offscreen: Default::default(),
            scroll: 0,
            table_rect: Rect::default(),
            proc_rect: Rect::default(),
            flash: None,
        };
        app.topo = sysstats::cpu_topology(app.stats.cpu_cores.len());
        app.sample();
        if !app.order.is_empty() {
            app.table.select(Some(0));
        }
        app
    }

    fn sample(&mut self) {
        // Compact mode shows only temperatures: skip CPU/mem/disk/net/process polling.
        let want = if self.compact { Want::default() } else { self.want() };
        self.stats.refresh(want);
        // Every tick, not only with the GPU panel on: the header shows GPU
        // power too. Cheap: NVIDIA data comes from the running nvidia-smi,
        // AMD's from a few sysfs files.
        self.gpu.refresh();
        if sysstats::is_demo() {
            // Believable figures that follow CPU load.
            self.cpu_power.simulate(38.0 + self.stats.cpu_total * 1.6);
            self.ram_power.simulate(4.5 + self.stats.cpu_total * 0.04);
        } else {
            self.cpu_power.refresh();
            self.ram_power.refresh();
            let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
            self.battery = battery::read_battery(std::path::Path::new(&root));
        }
        let selected_id = self.selected().map(|s| s.id.clone());
        let mut seen = vec![false; self.sensors.len()];
        for r in self.source.read() {
            let idx = match self.sensors.iter().position(|s| s.id == r.id) {
                Some(i) => i,
                None => {
                    self.sensors.push(Sensor::new(&r));
                    self.sensors.len() - 1
                }
            };
            self.sensors[idx].push(&r);
            if let Some(s) = seen.get_mut(idx) {
                *s = true;
            }
        }
        // A sensor that stopped being reported (device unplugged, driver
        // unloaded, GPU gone) must not keep showing its last temperature.
        for (s, seen) in self.sensors.iter_mut().zip(seen) {
            if !seen {
                s.current = None;
                s.note = Some("disconnected".into());
                s.gone_since.get_or_insert_with(Instant::now);
            }
        }
        self.sensors.retain(|s| s.gone_since.is_none_or(|t| t.elapsed() < GONE_KEEP));
        self.resort();
        // Keep the same sensor selected even if sorting moved it (or the
        // nearest row, if it was the one dropped).
        if let Some(id) = selected_id {
            self.select_id(&id);
        }
        if let Some(i) = self.table.selected() {
            self.table.select(if self.order.is_empty() { None } else { Some(i.min(self.order.len() - 1)) });
        }
        // An integrated GPU has no sensor of its own; it sits on the CPU's chip.
        let pkg = self.cpu_temp().map(|(_, t)| t);
        for g in self.gpu.gpus.iter_mut().filter(|g| g.on_cpu_die) {
            g.temp = pkg;
        }
        // After the sensors are read, so RAM sticks can be listed.
        if self.show[P_SPECS] && !self.compact {
            self.collect_specs();
        }
    }

    fn resort(&mut self) {
        self.order = (0..self.sensors.len()).collect();
        let s = &self.sensors;
        match self.sort {
            // By category (CPU, GPU, RAM, NVME, SSD, HDD, …), then device, so
            // each piece of hardware's sensors sit together.
            Sort::Name => self.order.sort_by(|&a, &b| {
                kind_rank(s[a].kind)
                    .cmp(&kind_rank(s[b].kind))
                    .then(s[a].device.cmp(&s[b].device))
                    .then(s[a].label.cmp(&s[b].label))
            }),
            Sort::TempDesc => self.order.sort_by(|&a, &b| {
                let ta = s[a].current.unwrap_or(f32::MIN);
                let tb = s[b].current.unwrap_or(f32::MIN);
                tb.total_cmp(&ta)
            }),
        }
    }

    fn select_id(&mut self, id: &str) {
        if let Some(pos) = self.order.iter().position(|&i| self.sensors[i].id == id) {
            self.table.select(Some(pos));
        }
    }

    /// The single number to show as "CPU temp": the package / Tctl sensor
    /// when there is one, otherwise the hottest CPU sensor.
    fn cpu_temp(&self) -> Option<(&Sensor, f32)> {
        let cpu: Vec<&Sensor> =
            self.sensors.iter().filter(|s| s.cpu && s.current.is_some()).collect();
        let preferred = ["package", "tdie", "tctl", "x86_pkg_temp", "cpu"];
        preferred
            .iter()
            .find_map(|p| cpu.iter().find(|s| s.label.to_ascii_lowercase().contains(p)))
            .copied()
            .or_else(|| {
                cpu.iter()
                    .max_by(|a, b| a.current.unwrap().total_cmp(&b.current.unwrap()))
                    .copied()
            })
            .map(|s| (s, s.current.unwrap()))
    }

    /// The hottest memory stick, for the header.
    fn ram_temp(&self) -> Option<(&Sensor, f32)> {
        self.sensors
            .iter()
            .filter(|s| s.kind == "RAM")
            .filter_map(|s| s.current.map(|t| (s, t)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    fn selected(&self) -> Option<&Sensor> {
        self.table.selected().and_then(|i| self.order.get(i)).map(|&i| &self.sensors[i])
    }

    fn move_sel(&mut self, delta: isize) {
        let n = self.order.len();
        if n == 0 {
            return;
        }
        let cur = self.table.selected().unwrap_or(0) as isize;
        self.table.select(Some((cur + delta).rem_euclid(n as isize) as usize));
    }

    /// Do keys go to the process list? (Tab switches; it's automatic when
    /// the sensor table is hidden.)
    fn proc_focused(&self) -> bool {
        !self.compact && self.show[P_PROC] && (self.focus_proc || !self.show[P_SENSORS])
    }

    /// Move the process selection by `delta` rows.
    fn proc_move(&mut self, delta: isize) {
        let vis = self.pv.visible(&self.stats.procs);
        let cur = self.pv.cursor(&self.stats.procs, &vis).unwrap_or(0) as isize;
        self.pv.select(&self.stats.procs, &vis, cur + delta);
    }

    /// The selected process (pid, name), if any.
    fn proc_selected(&self) -> Option<(u32, String)> {
        let vis = self.pv.visible(&self.stats.procs);
        let c = self.pv.cursor(&self.stats.procs, &vis)?;
        let p = &self.stats.procs[vis[c]];
        Some((p.pid, p.name.clone()))
    }

    /// Keys for the focused process list. Returns false if not handled here.
    fn proc_key(&mut self, key: KeyEvent) -> bool {
        use panels::Prompt;
        let page = self.pv.page.max(1) as isize;
        match key.code {
            KeyCode::Down => self.proc_move(1),
            KeyCode::Up => self.proc_move(-1),
            // While the list itself is scrolled out of view, PgUp/PgDn scroll the panels.
            KeyCode::PageDown if !self.proc_rect.is_empty() => self.proc_move(page),
            KeyCode::PageUp if !self.proc_rect.is_empty() => self.proc_move(-page),
            KeyCode::Home => self.proc_move(isize::MIN / 2),
            KeyCode::End => self.proc_move(isize::MAX / 2),
            KeyCode::Char('f') | KeyCode::Char('/') => self.pv.prompt = Some(Prompt::Filter),
            KeyCode::Char('e') => self.pv.prompt = Some(Prompt::Exec(String::new())),
            KeyCode::Char(c @ ('t' | 'k')) => {
                let sig = if c == 't' { procctl::Sig::Term } else { procctl::Sig::Kill };
                if let Some((pid, name)) = self.proc_selected() {
                    self.pv.prompt = Some(Prompt::Confirm { sig, pid, name });
                }
            }
            KeyCode::Char('n') => {
                if let Some((pid, name)) = self.proc_selected() {
                    let cur = self.stats.procs.iter().find(|p| p.pid == pid).and_then(|p| p.nice);
                    let buf = cur.map(|n| n.to_string()).unwrap_or_default();
                    self.pv.prompt = Some(Prompt::Nice { pid, name, buf });
                }
            }
            // Esc first clears an active search; otherwise it quits as usual.
            KeyCode::Esc if !self.pv.filter.is_empty() => self.pv.filter.clear(),
            _ => return false,
        }
        true
    }

    /// Keys while a process-panel prompt is open.
    fn prompt_key(&mut self, key: KeyEvent) {
        use panels::Prompt;
        let Some(prompt) = self.pv.prompt.take() else { return };
        let edit = |buf: &mut String, allow: &dyn Fn(char) -> bool| match key.code {
            KeyCode::Char(c) if allow(c) => buf.push(c),
            KeyCode::Backspace => {
                buf.pop();
            }
            _ => {}
        };
        match prompt {
            Prompt::Filter => match key.code {
                KeyCode::Enter => {}
                KeyCode::Esc => self.pv.filter.clear(),
                _ => {
                    edit(&mut self.pv.filter, &|_| true);
                    self.pv.prompt = Some(Prompt::Filter);
                }
            },
            Prompt::Exec(mut buf) => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => match procctl::exec(&buf) {
                    Ok(pid) => {
                        self.pv.set_status(format!("started {pid}: {}", buf.trim()), false);
                        self.pv.sel_pid = Some(pid);
                        self.stats.add_started(pid, &buf);
                        // Show it even if the search would hide it.
                        if let Some(p) = self.stats.procs.iter().find(|p| p.pid == pid)
                            && !procctl::matches(p, &self.pv.filter) {
                                self.pv.filter.clear();
                            }
                    }
                    Err(e) => self.pv.set_status(e, true),
                },
                _ => {
                    edit(&mut buf, &|_| true);
                    self.pv.prompt = Some(Prompt::Exec(buf));
                }
            },
            Prompt::Nice { pid, name, mut buf } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => match buf.trim().parse::<i32>() {
                    Ok(v) => match procctl::renice(pid, v) {
                        Ok(()) => {
                            self.pv.set_status(format!("{name} ({pid}) nice → {v}"), false);
                            self.stats.update_nice(pid);
                        }
                        Err(e) => self.pv.set_status(e, true),
                    },
                    Err(_) => self.pv.set_status("enter a number from -20 to 19", true),
                },
                KeyCode::Up | KeyCode::Down => {
                    // Step the value with the arrows.
                    let v = buf.trim().parse::<i32>().unwrap_or(0) + if key.code == KeyCode::Up { 1 } else { -1 };
                    buf = v.clamp(-20, 19).to_string();
                    self.pv.prompt = Some(Prompt::Nice { pid, name, buf });
                }
                _ => {
                    edit(&mut buf, &|c| c.is_ascii_digit() || (c == '-'));
                    self.pv.prompt = Some(Prompt::Nice { pid, name, buf });
                }
            },
            Prompt::Confirm { sig, pid, name } => {
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter) {
                    let what = if sig == procctl::Sig::Term { "terminated" } else { "killed" };
                    match procctl::signal(pid, sig) {
                        Ok(()) => {
                            self.pv.set_status(format!("{what} {name} ({pid})"), false);
                            // Give it a moment to exit, then drop it from the list.
                            std::thread::sleep(Duration::from_millis(60));
                            self.stats.forget_if_gone(pid);
                        }
                        Err(e) => self.pv.set_status(e, true),
                    }
                }
            }
        }
    }

    /// Mouse: wheel scrolls and clicks select, in the sensor table and the
    /// process list; either one also moves the keyboard focus there.
    fn mouse(&mut self, m: crossterm::event::MouseEvent) {
        let at = ratatui::layout::Position { x: m.column, y: m.row };
        let in_proc = self.show[P_PROC] && !self.compact && self.proc_rect.contains(at);
        let in_table = self.show[P_SENSORS] && !self.compact && self.table_rect.contains(at);
        if self.picker.is_some() || self.view.is_some() || self.pv.prompt.is_some() {
            return;
        }
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let d = if m.kind == MouseEventKind::ScrollDown { 1 } else { -1 };
                if in_proc {
                    self.focus_proc = true;
                    self.proc_move(d * 3);
                } else if in_table {
                    self.focus_proc = false;
                    self.move_sel(d);
                } else {
                    self.scroll_by(d);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if in_proc {
                    self.focus_proc = true;
                    if m.row >= self.pv.first_row_y {
                        let row = self.pv.offset + (m.row - self.pv.first_row_y) as usize;
                        let vis = self.pv.visible(&self.stats.procs);
                        if row < vis.len() {
                            self.pv.select(&self.stats.procs, &vis, row as isize);
                        }
                    }
                } else if in_table {
                    self.focus_proc = false;
                    // Border, then header, then rows.
                    let first = self.table_rect.y + 2;
                    if m.row >= first {
                        let row = self.table.offset() + (m.row - first) as usize;
                        if row < self.order.len() {
                            self.table.select(Some(row));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Scroll the panels by `delta` whole panels, within what the last frame
    /// had left over.
    fn scroll_by(&mut self, delta: isize) {
        let max = self.offscreen.borrow().max_scroll;
        self.scroll = self.scroll.min(max).saturating_add_signed(delta).min(max);
    }

    /// Only gather data for panels that are on screen.
    fn want(&self) -> Want {
        Want {
            cpu: self.show[P_CPU],
            mem: self.show[P_MEM],
            disks: self.show[P_DISKS],
            net: self.show[P_NET],
            procs: self.show[P_PROC],
        }
    }

    fn toggle(&mut self, i: usize) {
        self.show[i] = !self.show[i];
        save_panels(&self.show, &self.lay);
        // Turning a panel on: fill it right away instead of after the next tick.
        if self.show[i] && i == P_GPU {
            self.gpu.refresh();
            return;
        }
        if self.show[i] && i == P_SPECS {
            self.gpu.refresh();
            self.collect_specs();
            return;
        }
        if self.show[i] && i >= P_CPU {
            let mut w = Want::default();
            match i {
                P_CPU => w.cpu = true,
                P_MEM => w.mem = true,
                P_DISKS => w.disks = true,
                P_NET => w.net = true,
                _ => w.procs = true,
            }
            self.stats.refresh(w);
        }
    }

    /// Keys while the panel checklist is open.
    fn view_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let Some(list) = &mut self.view else { return };
        let n = PANELS.len();
        let cur = list.selected().unwrap_or(0);
        // Rows are listed in layout order.
        let panel = self.lay.order[cur.min(n - 1)];
        let shift = mods.contains(KeyModifiers::SHIFT);
        let moved = |app: &mut App, delta: isize| {
            app.lay.move_by(panel, delta);
            let pos = app.lay.order.iter().position(|&p| p == panel).unwrap_or(0);
            if let Some(l) = &mut app.view {
                l.select(Some(pos));
            }
            save_panels(&app.show, &app.lay);
        };
        match code {
            // Move the highlighted panel: Ctrl+Shift+↑/↓ (Shift alone works
            // too, but some terminals keep Shift+↑/↓ for scrolling) or K/J.
            KeyCode::Up if shift => moved(self, -1),
            KeyCode::Down if shift => moved(self, 1),
            KeyCode::Char('K') => moved(self, -1),
            KeyCode::Char('J') => moved(self, 1),
            // Column on wide terminals.
            KeyCode::Left | KeyCode::Char('h') => {
                self.lay.left[panel] = true;
                save_panels(&self.show, &self.lay);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.lay.left[panel] = false;
                save_panels(&self.show, &self.lay);
            }
            KeyCode::Char('d') => {
                self.lay = PanelLayout::default();
                save_panels(&self.show, &self.lay);
            }
            KeyCode::Down | KeyCode::Char('j') => list.select(Some((cur + 1) % n)),
            KeyCode::Up | KeyCode::Char('k') => list.select(Some((cur + n - 1) % n)),
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle(panel),
            KeyCode::Char(c @ '1'..='9') => self.toggle(c as usize - '1' as usize),
            KeyCode::Char('a') => {
                self.show = [true; NP];
                save_panels(&self.show, &self.lay);
                self.stats.refresh(self.want());
            }
            KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('q') => self.view = None,
            _ => {}
        }
    }

    fn collect_specs(&mut self) {
        let gpus: Vec<specs::GpuSpec> = self
            .gpu
            .gpus
            .iter()
            .map(|g| specs::GpuSpec { name: g.name.clone(), vram: g.vram_total, nvidia: g.nvidia, vbios: g.vbios.clone() })
            .collect();
        if self.specs.as_ref().is_some_and(|(_, n)| *n == gpus.len()) {
            return;
        }
        #[allow(unused_mut)]
        let mut dimms: Vec<String> =
            self.sensors.iter().filter(|s| s.kind == "RAM").map(|s| s.device.clone()).collect();
        // No stick named from its SPD chip (soldered memory has none, or no
        // root): the BIOS's own list of modules, if readable.
        #[cfg(target_os = "linux")]
        if dimms.iter().all(|d| d.starts_with("RAM DIMM")) {
            let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
            let dmi = devnames::dmi_memory(std::path::Path::new(&root));
            if !dmi.is_empty() {
                dimms = dmi;
            }
        }
        let sys = sysinfo::System::new_with_specifics(
            sysinfo::RefreshKind::nothing().with_memory(sysinfo::MemoryRefreshKind::nothing().with_ram()),
        );
        let s = specs::Specs::collect(
            &self.stats.cpu_name,
            self.stats.cpu_cores.len(),
            sys.total_memory(),
            &gpus,
            &dimms,
        );
        self.specs = Some((s, gpus.len()));
    }

    fn th(&self) -> Theme {
        THEMES[self.theme].legible()
    }

    fn open_picker(&mut self) {
        self.picker = Some((self.theme, ListState::default().with_selected(Some(self.theme))));
    }

    /// Keys while the picker is open. Moving previews the theme live.
    fn picker_key(&mut self, code: KeyCode) {
        let Some((original, list)) = &mut self.picker else { return };
        let n = THEMES.len();
        let cur = list.selected().unwrap_or(0);
        let next = match code {
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => Some((cur + 1) % n),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => Some((cur + n - 1) % n),
            KeyCode::PageDown => Some((cur + 10).min(n - 1)),
            KeyCode::PageUp => Some(cur.saturating_sub(10)),
            KeyCode::Home | KeyCode::Char('g') => Some(0),
            KeyCode::End | KeyCode::Char('G') => Some(n - 1),
            _ => None,
        };
        if let Some(i) = next {
            list.select(Some(i));
            self.theme = i;
            return;
        }
        match code {
            KeyCode::Enter | KeyCode::Char(' ') => {
                themes::save(self.theme);
                self.picker = None;
            }
            KeyCode::Esc | KeyCode::Char('t') | KeyCode::Char('q') => {
                self.theme = *original;
                self.picker = None;
            }
            _ => {}
        }
    }

    fn hottest(&self) -> Option<&Sensor> {
        self.sensors
            .iter()
            .filter(|s| s.current.is_some())
            .max_by(|a, b| a.current.unwrap().total_cmp(&b.current.unwrap()))
    }
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "Oryn — terminal hardware monitor\n\n\
             USAGE: oryn [--demo] [--list] [--interval <ms>] [--theme <name>]\n\n\
             --list          print every detected sensor once and exit\n\
             --theme <name>  color scheme, e.g. dracula, nord, tokyo-night\n\
             --themes        list the available color schemes\n\
             --gpu-info      explain which GPU temperatures (VRAM, hotspot) can be read\n\
             --mem-probe     check which part of Oryn, if any, keeps memory (no UI, ~1 min)\n\
             --version       print the version\n\
             --compact       start in compact mode (temperatures only; `c` toggles)\n\
             --no-nvapi      don't use NVIDIA's NvAPI library for hotspot/VRAM temperatures\n\
             --panels <list> panels to show: sensors,history,cpu,mem,disks,net,proc,gpu,specs or all\n\n\
             KEYS: ↑/↓ select  c compact  1-9 toggle panel  v panel list  t themes  u °C/°F\n\
                   +/- rate  s sort sensors  p sort processes  n next network  r reset\n\
                   l write everything shown to a log in /tmp  q quit"
        );
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if args.iter().any(|a| a == "--no-nvapi") {
        nvapi::disable();
    }
    if args.iter().any(|a| a == "--gpu-info") {
        #[cfg(target_os = "linux")]
        print!("{}", gpustats::gpu_info_report());
        #[cfg(not(target_os = "linux"))]
        println!("--gpu-info is Linux-only.");
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("oryn {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--themes") {
        for t in THEMES.iter() {
            println!("{}", t.name);
        }
        return Ok(());
    }
    // --theme beats the saved choice; the saved choice beats the default.
    let theme = match args.iter().position(|a| a == "--theme").and_then(|i| args.get(i + 1)) {
        Some(name) => match themes::find(name) {
            Some(i) => i,
            None => {
                eprintln!("Unknown theme `{name}`. Available (oryn --themes):");
                for t in THEMES.iter() {
                    eprintln!("  {}", t.name);
                }
                std::process::exit(2);
            }
        },
        None => themes::load_saved().unwrap_or(0),
    };
    // --panels beats the saved layout; default is everything on.
    let show = match args.iter().position(|a| a == "--panels").and_then(|i| args.get(i + 1)) {
        Some(spec) => match parse_panels(spec) {
            Some(s) => s,
            None => {
                eprintln!(
                    "Unknown panel in `{spec}`. Use a comma list of: {} (or all)",
                    PANELS.iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
                );
                std::process::exit(2);
            }
        },
        None => load_panels().map(|(s, _)| s).unwrap_or([true; NP]),
    };
    let lay = load_panels().map(|(_, l)| l).unwrap_or_default();
    let demo = args.iter().any(|a| a == "--demo");
    let interval_ms = args
        .iter()
        .position(|a| a == "--interval" || a == "-i")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1000)
        .clamp(100, 10_000);

    let source: Box<dyn Source> = if demo {
        sysstats::set_demo();
        Box::new(DemoSource::new())
    } else {
        match real_source(Duration::from_millis(interval_ms)) {
            Some(src) => src,
            None => {
                eprintln!(
                    "No temperature sensors found on this system.\n\
                     On Linux, load the drivers for your hardware (`sudo sensors-detect`;\n\
                     CPU: coretemp for Intel, k10temp for AMD; DDR5: spd5118).\n\
                     Run `oryn --demo` to try the UI with simulated sensors."
                );
                std::process::exit(1);
            }
        }
    };

    if args.iter().any(|a| a == "--list") {
        let mut source = source;
        source.settle();
        for r in source.read() {
            let t = r.temp.map(|t| format!("{t:6.1}°C")).unwrap_or_else(|| "   n/a  ".into());
            let crit = r.critical.map(|c| format!("crit {c:.0}°C")).unwrap_or_default();
            let cpu = format!("{:<5}", r.kind);
            let note = r.note.map(|n| format!("  [{n}]")).unwrap_or_default();
            println!("{t}  {cpu} {:<34} {:<26} {:<12} {}{note}", r.device, r.label, crit, r.id);
        }
        return Ok(());
    }

    if args.iter().any(|a| a == "--mem-probe") {
        mem_probe(source);
        return Ok(());
    }

    let mut app = App::new(source, Duration::from_millis(interval_ms), theme, show, lay);
    app.compact = args.iter().any(|a| a == "--compact") || load_compact();
    #[cfg(unix)]
    watch_for_hangup();
    #[cfg(unix)]
    catch_sigterm();
    let mut terminal = ratatui::init();
    mouse_reporting(true);
    // On a crash, also switch mouse reporting off before ratatui's own hook
    // restores the terminal; otherwise the shell gets escape codes on every click.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        mouse_reporting(false);
        hook(info);
    }));
    let result = run(&mut terminal, &mut app);
    mouse_reporting(false);
    ratatui::restore();
    result
}

/// `--mem-probe`: run each part of Oryn on its own and report which one, if
/// any, keeps memory (see memprobe.rs). No terminal UI; takes about a minute.
fn mem_probe(source: Box<dyn Source>) {
    use ratatui::{backend::TestBackend, Terminal};
    memprobe::print_header();
    let mut app = App::new(source, Duration::from_millis(1000), 0, [true; NP], PanelLayout::default());
    let want = |f: fn(&mut Want)| {
        let mut w = Want::default();
        f(&mut w);
        w
    };
    let show = |r: memprobe::Result| memprobe::print(&r);
    let n = 500;
    show(memprobe::measure("temperature sensors", 150, n, || drop(app.source.read())));
    show(memprobe::measure("CPU usage", 150, n, || app.stats.refresh(want(|w| w.cpu = true))));
    show(memprobe::measure("memory", 150, n, || app.stats.refresh(want(|w| w.mem = true))));
    show(memprobe::measure("disks", 150, n, || app.stats.refresh(want(|w| w.disks = true))));
    show(memprobe::measure("network", 150, n, || app.stats.refresh(want(|w| w.net = true))));
    show(memprobe::measure("processes", 150, n, || app.stats.refresh(want(|w| w.procs = true))));
    show(memprobe::measure("CPU power", 150, n, || app.cpu_power.refresh()));
    show(memprobe::measure("RAM power", 150, n, || app.ram_power.refresh()));
    let gpu_name = if app.gpu.gpus.iter().any(|g| g.nvidia) && !cfg!(not(target_os = "linux")) {
        "GPU panel (nvidia-smi data, NvAPI)"
    } else {
        "GPU panel"
    };
    show(memprobe::measure(gpu_name, 150, n, || app.gpu.refresh()));
    show(memprobe::measure("system specs", 3, 20, || {
        app.specs = None;
        app.collect_specs();
    }));
    let mut term = Terminal::new(TestBackend::new(170, 50)).expect("test backend");
    show(memprobe::measure("drawing the screen", 150, n, || {
        let _ = term.draw(|f| draw(f, &mut app));
    }));
    show(memprobe::measure("everything (refresh + draw)", 150, n, || {
        app.sample();
        let _ = term.draw(|f| draw(f, &mut app));
    }));
    println!("\nGPUs: {}   processes: {}   sensors: {}", app.gpu.gpus.len(), app.stats.proc_count, app.sensors.len());
}

/// The platform's real sensor source, or None if it finds no sensors.
fn real_source(interval: Duration) -> Option<Box<dyn Source>> {
    #[cfg(target_os = "linux")]
    let (base, base_empty, disks) = {
        // ORYN_SYSFS lets you point at a copied/fake sysfs tree for testing.
        let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
        let src = sensors::linux::SysfsSource::new(std::path::Path::new(&root));
        // Disks with drivetemp are already covered; SMART handles the rest.
        let disks = smart::SmartPoller::new(src.drivetemp_disks());
        let empty = src.is_empty();
        (Box::new(src) as Box<dyn Source>, empty, disks)
    };
    #[cfg(not(target_os = "linux"))]
    let (base, base_empty, disks) = {
        let src = sensors::SysinfoSource::new();
        let disks = smart::SmartPoller::new(&Default::default());
        let empty = src.is_empty();
        (Box::new(src) as Box<dyn Source>, empty, disks)
    };

    let mut extras: Vec<Box<dyn Source>> = Vec::new();
    if let Some(d) = disks {
        extras.push(Box::new(d));
    }
    // NVIDIA's own driver doesn't use hwmon; AMD/Intel/nouveau GPUs come from sysfs.
    if let Some(g) = gpu::NvidiaPoller::new(interval) {
        extras.push(Box::new(g));
    }
    match (base_empty, extras.is_empty()) {
        (true, true) => None,
        (false, true) => Some(base),
        _ => Some(Box::new(sensors::Combined { base, extras })),
    }
}

/// Mouse reporting for wheel scrolling and click-to-select: button presses
/// and the wheel only (xterm mode 1000, SGR coordinates 1006). Unlike
/// crossterm's EnableMouseCapture this doesn't ask for every mouse movement,
/// which would wake Oryn and redraw dozens of times a second as the pointer
/// crosses the window. Hold Shift to select text with the mouse.
fn mouse_reporting(on: bool) {
    use std::io::Write;
    let seq: &[u8] = if on { b"\x1b[?1000h\x1b[?1006h" } else { b"\x1b[?1006l\x1b[?1000l" };
    let mut out = io::stdout();
    let _ = out.write_all(seq);
    let _ = out.flush();
}

/// Set by SIGTERM (`kill`, system shutdown): quit cleanly, restoring the
/// terminal, instead of dying with it still in raw mode and the alternate
/// screen.
#[cfg(unix)]
static TERMINATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
fn catch_sigterm() {
    extern "C" fn on_term(_: libc::c_int) {
        TERMINATE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
    }
}

fn terminate_requested() -> bool {
    #[cfg(unix)]
    {
        TERMINATE.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Has the terminal on `fd` gone away (window closed, tmux session killed)?
/// Its other end is closed: polling reports a hang-up.
#[cfg(unix)]
fn tty_gone(fd: i32) -> bool {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: one pollfd, zero timeout.
    let n = unsafe { libc::poll(&mut p, 1, 0) };
    n > 0 && p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// Quit when the terminal goes away (window closed, tmux session killed)
/// even if no SIGHUP reaches us, e.g. when started through runuser or sudo.
/// Otherwise crossterm keeps reading end-of-file from the dead terminal and
/// spins a CPU core forever.
#[cfg(unix)]
fn watch_for_hangup() {
    let fds: Vec<i32> = [0, 1].into_iter().filter(|&fd| unsafe { libc::isatty(fd) == 1 } && !tty_gone(fd)).collect();
    if fds.is_empty() {
        return;
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(300));
        if fds.iter().any(|&fd| tty_gone(fd)) {
            std::process::exit(0);
        }
    });
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut last = Instant::now();
    loop {
        // Sample first, so no stream of input events can ever delay it.
        if last.elapsed() >= app.interval {
            app.sample();
            last = Instant::now();
        }
        if terminate_requested() {
            return Ok(());
        }
        terminal.draw(|f| draw(f, app))?;

        // Wake at least at every new second so the clock keeps ticking.
        let to_next_second = Duration::from_millis(1000 - chrono::Local::now().timestamp_subsec_millis().min(999) as u64);
        let timeout = app.interval.saturating_sub(last.elapsed()).min(to_next_second);
        if event::poll(timeout)? {
            let ev = event::read()?;
            if let Event::Mouse(m) = ev {
                app.mouse(m);
                continue;
            }
            if let Event::Key(key) = ev {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return Ok(());
                }
                if app.pv.prompt.is_some() && app.proc_focused() {
                    app.prompt_key(key);
                    continue;
                }
                app.pv.prompt = None;
                if app.picker.is_some() {
                    app.picker_key(key.code);
                    continue;
                }
                if app.view.is_some() {
                    app.view_key(key.code, key.modifiers);
                    continue;
                }
                // Compact mode has no panels: ignore the keys that work on them,
                // rather than act on things that aren't on screen (or open the
                // panel list, which compact mode doesn't draw).
                if app.compact
                    && matches!(
                        key.code,
                        KeyCode::Char('v' | 'p' | 'n' | '1'..='9') | KeyCode::Tab | KeyCode::BackTab
                    )
                {
                    continue;
                }
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    if app.show[P_PROC] && app.show[P_SENSORS] {
                        app.focus_proc = !app.focus_proc;
                    }
                    continue;
                }
                if app.proc_focused() && app.proc_key(key) {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Down | KeyCode::Char('j') => app.move_sel(1),
                    KeyCode::Up | KeyCode::Char('k') => app.move_sel(-1),
                    KeyCode::PageDown => app.scroll_by(1),
                    KeyCode::PageUp => app.scroll_by(-1),
                    KeyCode::Char('t') => app.open_picker(),
                    KeyCode::Char('c') | KeyCode::Char('C') => {
                        app.compact = !app.compact;
                        save_compact(app.compact);
                        if !app.compact {
                            // Back to the full view: fill the panels right away.
                            app.stats.refresh(app.want());
                        }
                    }
                    KeyCode::Char(c @ '1'..='9') => app.toggle(c as usize - '1' as usize),
                    KeyCode::Char('v') => app.view = Some(ListState::default().with_selected(Some(0))),
                    KeyCode::Char('p') => {
                        app.stats.proc_sort = app.stats.proc_sort.next();
                        app.stats.resort_procs();
                    }
                    KeyCode::Char('n') => app.stats.next_net(),
                    // Quick cycle without the picker; the choice is saved.
                    KeyCode::Char('T') => {
                        app.theme = (app.theme + 1) % THEMES.len();
                        themes::save(app.theme);
                    }
                    KeyCode::Char('u') => {
                        app.unit = match app.unit {
                            Unit::C => Unit::F,
                            Unit::F => Unit::C,
                        }
                    }
                    // Refresh interval in 100 ms steps (like btop): + adds, - subtracts.
                    KeyCode::Char('+') | KeyCode::Char('=') => app.interval = step_interval(app.interval, 1),
                    KeyCode::Char('-') | KeyCode::Char('_') => app.interval = step_interval(app.interval, -1),
                    KeyCode::Char('s') => {
                        app.sort = match app.sort {
                            Sort::Name => Sort::TempDesc,
                            Sort::TempDesc => Sort::Name,
                        };
                        let id = app.selected().map(|s| s.id.clone());
                        app.resort();
                        if let Some(id) = id {
                            app.select_id(&id);
                        }
                    }
                    KeyCode::Char('l' | 'L') => {
                        let (msg, err) = match report::save(app) {
                            Ok(path) => (format!("log saved to {}", path.display()), false),
                            Err(e) => (format!("couldn't write the log: {e}"), true),
                        };
                        app.flash = Some((msg, err, Instant::now()));
                    }
                    KeyCode::Char('r') => {
                        for s in &mut app.sensors {
                            s.min = f32::INFINITY;
                            s.max = f32::NEG_INFINITY;
                            s.history.clear();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// CPU temperatures for the CPU panel, from the CPU sensors:
/// * Intel (coretemp): "Core N" is a real per-core sensor; each logical CPU
///   gets the temperature of its physical core (topology/core_id).
/// * AMD (k10temp): only Tctl and one "TccdN" per chiplet exist; each CCD is
///   shown with the logical CPUs that share its L3 cache.
fn cpu_temps(app: &App) -> panels::CpuTemps {
    let mut out = panels::CpuTemps::default();
    let cpu: Vec<&Sensor> = app.sensors.iter().filter(|s| s.cpu && s.current.is_some()).collect();
    if cpu.is_empty() {
        return out;
    }
    let after = |s: &Sensor, key: &str| -> Option<String> {
        let l = s.label.to_ascii_lowercase();
        l.find(key).map(|i| l[i + key.len()..].trim().to_string())
    };
    let val = |s: &Sensor| panels::TempVal {
        c: s.current.unwrap_or(0.0),
        warn: s.warn_threshold(),
        crit: s.crit_threshold(),
    };
    out.fahrenheit = matches!(app.unit, Unit::F);
    // Headline: Tctl / Tdie / Package, else the hottest CPU sensor.
    out.package = ["tctl", "tdie", "package id", "x86_pkg_temp"]
        .iter()
        .find_map(|k| cpu.iter().find(|s| s.label.to_ascii_lowercase().contains(k)))
        .map(|s| {
            let l = s.label.to_ascii_lowercase();
            let name = if l.contains("tctl") { "Tctl" } else if l.contains("tdie") { "Tdie" } else { "Package" };
            (name.to_string(), val(s))
        });

    let topo = &app.topo;
    let n_cpus = app.stats.cpu_cores.len();
    // AMD: TccdN. Each logical CPU shows its CCD's sensor (like btop), and
    // the Temps line says which cores each CCD covers.
    let mut ccd: Vec<(u32, panels::TempVal)> = cpu
        .iter()
        .filter_map(|s| Some((after(s, "tccd")?.parse::<u32>().ok()?, val(s))))
        .collect();
    ccd.sort_by_key(|c| c.0);
    if !ccd.is_empty() {
        // L3 ids in order -> CCD 1, 2, …
        let mut l3: Vec<u32> = topo.iter().filter_map(|t| t.l3_id).collect();
        l3.sort_unstable();
        l3.dedup();
        out.per_core = vec![None; n_cpus];
        for (n, t) in ccd {
            // Tccd1 is the first L3 (a "Tccd0" would have no match).
            let cpus: Vec<usize> = match (n as usize).checked_sub(1).and_then(|k| l3.get(k)) {
                Some(id) => topo
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.l3_id == Some(*id))
                    .map(|(i, _)| i)
                    .collect(),
                _ => Vec::new(),
            };
            for &i in &cpus {
                if let Some(slot) = out.per_core.get_mut(i) {
                    *slot = Some(t);
                }
            }
            out.ccds.push((format!("CCD{n}"), t, ranges(&cpus)));
        }
    }
    // Intel: "Core N" per physical core.
    let core_temp: std::collections::HashMap<u32, panels::TempVal> = cpu
        .iter()
        .filter_map(|s| {
            let rest = after(s, "core ")?;
            Some((rest.split_whitespace().next()?.parse::<u32>().ok()?, val(s)))
        })
        .collect();
    if !core_temp.is_empty() {
        out.per_core = (0..n_cpus)
            .map(|i| topo.get(i).and_then(|t| t.core_id).and_then(|c| core_temp.get(&c).copied()))
            .collect();
    }
    out
}

/// Next refresh interval, one 100 ms step up or down, kept within 100 ms–10 s.
/// An odd value (e.g. `--interval 250`) snaps to the 100 ms grid first:
/// 250 → + 300, - 200.
fn step_interval(cur: Duration, dir: i64) -> Duration {
    let ms = cur.as_millis() as i64;
    let next = if dir > 0 { (ms / 100 + 1) * 100 } else { ((ms + 99) / 100 - 1) * 100 };
    Duration::from_millis(next.clamp(100, 10_000) as u64)
}

/// [0,1,2,3,16,17] -> "C0–3, C16–17".
fn ranges(cpus: &[usize]) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < cpus.len() {
        let start = cpus[i];
        let mut end = start;
        while i + 1 < cpus.len() && cpus[i + 1] == end + 1 {
            i += 1;
            end = cpus[i];
        }
        parts.push(if start == end { format!("C{start}") } else { format!("C{start}–{end}") });
        i += 1;
    }
    parts.join(", ")
}

/// A slot in the layout: one panel, or memory and disks side by side.
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Panel(usize),
    MemDisks,
}

/// Where each visible panel goes. Wide terminals: temperatures on the left,
/// system panels on the right (like btop). Narrow: one stacked column.
///
/// No column is left half empty: the last panel in each column stretches to
/// its bottom, and if the left column has nothing that can stretch (history
/// hidden), the process list — or failing that the network graph — moves
/// under the sensor table.
fn layout(app: &App, body: Rect) -> Vec<(Slot, Rect)> {
    let show = app.show;
    let temps = show[P_SENSORS] || show[P_HISTORY] || show[P_SPECS];
    let system = show[P_CPU..P_GPU].iter().any(|s| *s) || (show[P_GPU] && !app.gpu.gpus.is_empty());

    let mut sys_slots: Vec<Slot> = Vec::new();
    if show[P_CPU] {
        sys_slots.push(Slot::Panel(P_CPU));
    }
    // Only when a GPU was actually found, so there's never an empty box.
    if show[P_GPU] && !app.gpu.gpus.is_empty() {
        sys_slots.push(Slot::Panel(P_GPU));
    }
    match (show[P_MEM], show[P_DISKS]) {
        (true, true) => sys_slots.push(Slot::MemDisks),
        (true, false) => sys_slots.push(Slot::Panel(P_MEM)),
        (false, true) => sys_slots.push(Slot::Panel(P_DISKS)),
        _ => {}
    }
    if show[P_NET] {
        sys_slots.push(Slot::Panel(P_NET));
    }
    if show[P_PROC] {
        sys_slots.push(Slot::Panel(P_PROC));
    }
    let mut temp_slots: Vec<Slot> = Vec::new();
    if show[P_SENSORS] {
        temp_slots.push(Slot::Panel(P_SENSORS));
    }
    if show[P_SPECS] {
        temp_slots.push(Slot::Panel(P_SPECS));
    }
    if show[P_HISTORY] {
        temp_slots.push(Slot::Panel(P_HISTORY));
    }

    // Panels that put extra height to use: graphs and the process list.
    let stretchy = |s: &Slot| {
        matches!(
            s,
            Slot::Panel(P_CPU) | Slot::Panel(P_GPU) | Slot::Panel(P_NET) | Slot::Panel(P_PROC) | Slot::Panel(P_HISTORY)
        )
    };
    // If a column would end on a panel that can't grow (memory, disks), move
    // the CPU panel (whose graph can) to the bottom of it.
    let fix_tail = |col: &mut Vec<Slot>| {
        if col.last().is_some_and(|l| !stretchy(l))
            && let Some(i) = col.iter().position(|s| *s == Slot::Panel(P_CPU)) {
                let cpu = col.remove(i);
                col.push(cpu);
            }
    };

    let mut columns: Vec<(Rect, Vec<Slot>)> = Vec::new();
    if !app.lay.is_default() {
        // Custom layout: the user's order, split into the chosen columns.
        let visible = |p: usize| show[p] && (p != P_GPU || !app.gpu.gpus.is_empty());
        let to_slots = |ps: Vec<usize>| -> Vec<Slot> {
            // Memory and disks next to each other share a row.
            let mut out: Vec<Slot> = Vec::new();
            for p in ps {
                let pair = matches!(out.last(), Some(Slot::Panel(q)) if (*q == P_MEM && p == P_DISKS) || (*q == P_DISKS && p == P_MEM));
                if pair {
                    out.pop();
                    out.push(Slot::MemDisks);
                } else {
                    out.push(Slot::Panel(p));
                }
            }
            out
        };
        let shown: Vec<usize> = app.lay.order.iter().copied().filter(|&p| visible(p)).collect();
        let left: Vec<usize> = shown.iter().copied().filter(|&p| app.lay.left[p]).collect();
        let right: Vec<usize> = shown.iter().copied().filter(|&p| !app.lay.left[p]).collect();
        if body.width >= 150 && !left.is_empty() && !right.is_empty() {
            let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);
            columns = vec![(l, to_slots(left)), (r, to_slots(right))];
        } else {
            columns = vec![(body, to_slots(shown))];
        }
    } else if temps && system && body.width >= 150 {
        let mut left = temp_slots.clone();
        let mut right = sys_slots.clone();
        let mut ok = true;
        if !show[P_HISTORY] {
            // The sensor table can't fill a column alone: borrow a panel that
            // can grow from the right, choosing one that leaves the right
            // column still ending on something that can grow.
            ok = false;
            for want in [P_PROC, P_NET, P_GPU, P_CPU] {
                let Some(i) = right.iter().position(|s| *s == Slot::Panel(want)) else { continue };
                let mut rest = right.clone();
                let moved = rest.remove(i);
                fix_tail(&mut rest);
                if rest.last().is_some_and(&stretchy) {
                    left.push(moved);
                    right = rest;
                    ok = true;
                    break;
                }
            }
        }
        fix_tail(&mut right);
        if ok {
            let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);
            columns = vec![(l, left), (r, right)];
        }
    }
    if columns.is_empty() {
        // One column: sensors, system panels, the static specs, then the
        // chart and process list (the two that use extra height best). On a
        // short terminal the column scrolls, so the live panels come first.
        let mut all = Vec::new();
        if show[P_SENSORS] {
            all.push(Slot::Panel(P_SENSORS));
        }
        all.extend(sys_slots.iter().copied().filter(|s| *s != Slot::Panel(P_PROC)));
        if show[P_SPECS] {
            all.push(Slot::Panel(P_SPECS));
        }
        if show[P_HISTORY] {
            all.push(Slot::Panel(P_HISTORY));
        }
        if show[P_PROC] {
            all.push(Slot::Panel(P_PROC));
        }
        fix_tail(&mut all);
        columns = vec![(body, all)];
    }

    let mut out = Vec::new();
    for (area, mut slots) in columns {
        scroll_window(app, &mut slots, area);
        let n = slots.len();
        let mut items: Vec<(Slot, Constraint)> = Vec::new();
        for (idx, slot) in slots.iter().enumerate() {
            let last = idx + 1 == n;
            let c = match *slot {
                Slot::Panel(P_SENSORS) => {
                    let rows = app.sensors.len() as u16 + 3;
                    if n == 1 {
                        Constraint::Fill(1)
                    } else {
                        // Every sensor if possible, leaving the other panels
                        // the height they need (they all fit: see
                        // scroll_window) — and never more than two
                        // thirds of the column.
                        let others: u16 = slots.iter().filter(|s| **s != *slot).map(|s| min_height(app, *s, area)).sum();
                        let cap = area.height.saturating_sub(others).min(area.height * 2 / 3);
                        Constraint::Length(rows.min(cap).max(min_height(app, *slot, area)))
                    }
                }
                Slot::Panel(P_HISTORY) => Constraint::Fill(1),
                Slot::Panel(P_SPECS) => {
                    let h = app.specs.as_ref().map_or(6, |(s, _)| s.lines.len() as u16 + 2);
                    if last { Constraint::Min(h) } else { Constraint::Length(h) }
                }
                Slot::Panel(P_PROC) => Constraint::Fill(2),
                Slot::Panel(P_CPU) => {
                    let h = panels::cpu_height(area.width, app.stats.cpu_cores.len(), &cpu_temps(app));
                    if last { Constraint::Min(h) } else { Constraint::Length(h) }
                }
                Slot::Panel(P_GPU) => {
                    // A little extra so a single GPU's graph has some height.
                    let h = panels::gpu_height(&app.gpu.gpus, area.width) + 2;
                    if last { Constraint::Min(h) } else { Constraint::Length(h) }
                }
                Slot::MemDisks => {
                    let h = panels::disks_height(app.stats.disks.len()).min(14).max(panels::MEM_HEIGHT);
                    if last { Constraint::Min(h) } else { Constraint::Length(h) }
                }
                Slot::Panel(P_MEM) => {
                    if last { Constraint::Min(panels::MEM_HEIGHT) } else { Constraint::Length(panels::MEM_HEIGHT) }
                }
                Slot::Panel(P_DISKS) => {
                    let h = panels::disks_height(app.stats.disks.len()).min(14);
                    if last { Constraint::Min(h) } else { Constraint::Length(h) }
                }
                Slot::Panel(P_NET) => {
                    // The graph is happy to grow, so the network panel takes
                    // spare room when nothing below it does.
                    if last { Constraint::Fill(1) } else { Constraint::Length(panels::NET_HEIGHT) }
                }
                Slot::Panel(_) => Constraint::Length(0),
            };
            items.push((*slot, c));
        }
        // If the preferred heights overrun the column, fall back to each
        // panel's minimum so the ones that grow still get theirs.
        let total: u16 = items
            .iter()
            .map(|(s, c)| match c {
                Constraint::Length(x) | Constraint::Min(x) => *x,
                _ => min_height(app, *s, area),
            })
            .sum();
        if total > area.height {
            for (s, c) in items.iter_mut() {
                if *s == Slot::Panel(P_SENSORS) {
                    continue;
                }
                if let Constraint::Length(x) = c {
                    *x = (*x).min(min_height(app, *s, area));
                }
            }
        }
        // Guarantee the column is filled: if nothing can grow, the last panel does.
        if !items.iter().any(|(_, c)| matches!(c, Constraint::Fill(_) | Constraint::Min(_)))
            && let Some(last) = items.last_mut() {
                last.1 = Constraint::Fill(1);
            }
        let rects = Layout::vertical(items.iter().map(|(_, c)| *c)).split(area);
        out.extend(items.iter().map(|(s, _)| *s).zip(rects.iter().copied()));
    }
    out
}

/// The least height at which a panel is still useful.
fn min_height(app: &App, slot: Slot, area: Rect) -> u16 {
    match slot {
        // Header plus a handful of sensors.
        Slot::Panel(P_SENSORS) => (app.sensors.len() as u16 + 3).clamp(4, 8),
        Slot::Panel(P_HISTORY) => 6,
        Slot::Panel(P_SPECS) => app.specs.as_ref().map_or(6, |(s, _)| s.lines.len() as u16 + 2),
        Slot::Panel(P_PROC) => 7,
        Slot::Panel(P_CPU) => panels::cpu_height(area.width, app.stats.cpu_cores.len(), &cpu_temps(app)),
        Slot::Panel(P_GPU) => panels::gpu_height(&app.gpu.gpus, area.width),
        Slot::Panel(P_NET) => 6,
        Slot::Panel(P_DISKS) => 4,
        Slot::MemDisks | Slot::Panel(P_MEM) => panels::MEM_HEIGHT,
        Slot::Panel(_) => 0,
    }
}

/// Panels left out of the last frame, above and below what's shown.
#[derive(Default)]
struct Offscreen {
    above: Vec<usize>,
    below: Vec<usize>,
    /// The furthest `App::scroll` can usefully go (the tallest column's).
    max_scroll: usize,
}

/// When a column is too short for all its panels, show a run of them that
/// fits, starting `app.scroll` panels down, instead of squashing every panel
/// until none of them shows anything. The footer says what's above and below.
fn scroll_window(app: &App, slots: &mut Vec<Slot>, area: Rect) {
    let need = |v: &[Slot]| v.iter().map(|s| min_height(app, *s, area)).sum::<u16>();
    if slots.len() <= 1 || need(slots) <= area.height {
        return;
    }
    // Scrolled all the way down, the last panels fill the column.
    let max = (0..slots.len()).find(|&i| need(&slots[i..]) <= area.height).unwrap_or(slots.len() - 1);
    let first = app.scroll.min(max);
    // At least one panel, even one taller than the column.
    let mut end = first + 1;
    while end < slots.len() && need(&slots[first..=end]) <= area.height {
        end += 1;
    }
    let panels = |v: &[Slot]| -> Vec<usize> {
        v.iter()
            .flat_map(|s| match *s {
                Slot::MemDisks => vec![P_MEM, P_DISKS],
                Slot::Panel(p) => vec![p],
            })
            .collect()
    };
    let mut off = app.offscreen.borrow_mut();
    off.above.extend(panels(&slots[..first]));
    off.below.extend(panels(&slots[end..]));
    off.max_scroll = off.max_scroll.max(max);
    slots.truncate(end);
    slots.drain(..first);
}

fn draw(f: &mut Frame, app: &mut App) {
    *app.offscreen.borrow_mut() = Offscreen::default();
    let th = app.th();
    // Paint the whole screen in the theme's background/foreground first.
    f.render_widget(Block::new().style(Style::new().bg(th.bg).fg(th.fg)), f.area());

    // Header and key hints keep their rows; the panels get the rest (a
    // minimum here would squeeze the header's text out on short terminals).
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    app.table_rect = Rect::default();
    app.proc_rect = Rect::default();
    app.pv.focused = app.proc_focused();

    if app.compact {
        draw_compact(f, app, body);
        draw_footer(f, app, footer, &th);
        if app.picker.is_some() {
            draw_picker(f, app);
        }
        return;
    }
    let slots = layout(app, body);
    if slots.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled("All panels are hidden. Press v to choose, or 1–9.", Style::new().fg(th.dim)))
                .centered(),
            Rect { y: body.y + body.height / 2, height: 1, ..body },
        );
    }
    for (slot, area) in slots {
        match slot {
            Slot::Panel(P_SENSORS) => {
                app.table_rect = area;
                draw_table(f, app, area)
            }
            Slot::Panel(P_HISTORY) => draw_chart(f, app, area),
            Slot::Panel(P_CPU) => {
                let pw = (app.cpu_power.watts, app.cpu_power.note);
                panels::draw_cpu(f, area, &app.stats, &cpu_temps(app), pw, &th, P_CPU + 1)
            }
            Slot::Panel(P_GPU) => {
                panels::draw_gpu(f, area, &app.gpu.gpus, &th, P_GPU + 1, matches!(app.unit, Unit::F))
            }
            Slot::Panel(P_SPECS) => panels::draw_specs(f, area, app.specs.as_ref().map(|s| &s.0), &th, P_SPECS + 1),
            Slot::Panel(P_MEM) => panels::draw_mem(f, area, &app.stats, &th, P_MEM + 1),
            Slot::Panel(P_DISKS) => panels::draw_disks(f, area, &app.stats, &th, P_DISKS + 1),
            Slot::Panel(P_NET) => panels::draw_net(f, area, &app.stats, &th, P_NET + 1),
            Slot::Panel(P_PROC) => {
                app.proc_rect = area;
                panels::draw_procs(f, area, &app.stats, &th, P_PROC + 1, &mut app.pv)
            }
            Slot::MemDisks => {
                let [m, d] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);
                panels::draw_mem(f, m, &app.stats, &th, P_MEM + 1);
                panels::draw_disks(f, d, &app.stats, &th, P_DISKS + 1);
            }
            Slot::Panel(_) => {}
        }
    }

    draw_footer(f, app, footer, &th);

    if app.picker.is_some() {
        draw_picker(f, app);
    }
    if app.view.is_some() {
        draw_view_menu(f, app);
    }
}

/// How long a footer message stays up.
const FLASH_FOR: Duration = Duration::from_secs(6);

/// The key-hint bar at the bottom.
fn draw_footer(f: &mut Frame, app: &App, footer: Rect, th: &Theme) {
    // In compact mode only some keys do anything.
    let both = app.show[P_PROC] && app.show[P_SENSORS];
    let pf = app.proc_focused();
    let useful = |k: &str| {
        if app.compact {
            return matches!(k, "c" | "t" | "u" | "s" | "+/-" | "l" | "q");
        }
        match k {
            // With the process list focused, t and n act on processes
            // (shown on its bottom bar) and T switches theme.
            "t" | "n" => !pf,
            "T" => pf,
            "tab" => both,
            _ => true,
        }
    };
    // Key hints, in display order, each with a priority (lower = kept longer)
    // so narrow terminals drop the least important ones instead of cutting off.
    let hints: [(&str, &str, u8); 14] = [
        ("tab", if pf { "sensors" } else { "processes" }, 3),
        ("↑↓", "select", 4),
        ("1-9", "panels", 1),
        ("v", "view", 0),
        ("t", "theme", 2),
        ("T", "theme", 2),
        ("u", "unit", 6),
        ("+/-", "rate", 8),
        ("s", "sort", 5),
        ("p", "proc sort", 7),
        ("n", "net", 9),
        ("c", "compact", 1),
        ("l", "log", 6),
        ("q", "quit", 0),
    ];
    let width = |h: &(&str, &str, u8)| h.0.chars().count() + 2 + h.1.len() + 3;
    // The last message (where `l` saved the log) for a few seconds, first.
    let flash = app.flash.as_ref().filter(|f| f.2.elapsed() < FLASH_FOR).map(|(msg, err, _)| {
        Span::styled(format!(" {msg}  "), Style::new().fg(if *err { th.crit } else { th.ok }).bold())
    });
    let mut budget = footer.width as usize - th.name.len().min(footer.width as usize);
    budget = budget.saturating_sub(flash.as_ref().map_or(0, |f| f.width()));
    let mut keep = [false; 14];
    let mut by_prio: Vec<usize> = (0..hints.len()).collect();
    by_prio.sort_by_key(|&i| hints[i].2);
    let take = |keep: &mut [bool; 14], budget: &mut usize, max_prio: u8| {
        for &i in &by_prio {
            if !keep[i] && hints[i].2 <= max_prio && useful(hints[i].0) && width(&hints[i]) <= *budget {
                *budget -= width(&hints[i]);
                keep[i] = true;
            }
        }
    };
    // The essential keys first (view, quit, panels, compact)…
    take(&mut keep, &mut budget, 1);
    // …then a note on panels scrolled out of view, so they don't seem lost…
    let off = app.offscreen.borrow();
    let names = |arrow: &str, v: &[usize]| {
        (!v.is_empty()).then(|| format!("{arrow} {}", v.iter().map(|&p| PANELS[p].0).collect::<Vec<_>>().join(", ")))
    };
    let counts = |arrow: &str, v: &[usize]| (!v.is_empty()).then(|| format!("{arrow}{}", v.len()));
    let join = |a: Option<String>, b: Option<String>| a.into_iter().chain(b).collect::<Vec<_>>().join(" · ");
    let note = if off.above.is_empty() && off.below.is_empty() {
        String::new()
    } else {
        let full = join(names("↑", &off.above), names("↓", &off.below));
        let short = join(counts("↑", &off.above), counts("↓", &off.below));
        [format!("{full} (PgUp/PgDn, wheel)  "), format!("{short} panels (PgUp/PgDn)  "), format!("{short}  ")]
            .into_iter()
            .find(|n| n.chars().count() <= budget)
            .unwrap_or_default()
    };
    budget -= note.chars().count();
    // …then whatever other hints still fit.
    take(&mut keep, &mut budget, u8::MAX);
    let mut spans: Vec<Span> = flash.into_iter().collect();
    for (i, (key, label, _)) in hints.iter().enumerate() {
        if keep[i] {
            spans.push(Span::styled(format!(" {key} "), Style::new().fg(th.on_badge).bg(th.dim)));
            spans.push(Span::raw(format!(" {label}  ")));
        }
    }
    if !note.is_empty() {
        spans.push(Span::styled(note, Style::new().fg(th.warn)));
    }
    spans.push(Span::styled(th.name, Style::new().fg(th.accent)));
    let help = Line::from(spans).style(Style::new().fg(th.dim));
    f.render_widget(Paragraph::new(help), footer);
}

/// One line of the compact view.
struct CompactRow {
    kind: &'static str,
    device: String,
    sensor: String,
    now: Option<f32>,
    max: Option<f32>,
    note: Option<String>,
    warn: f32,
    crit: f32,
}

/// Compact mode: every temperature in a dense, multi-column list, grouped by
/// category, with no bars or graphs.
fn draw_compact(f: &mut Frame, app: &App, area: Rect) {
    let th = app.th();
    let mut rows: Vec<CompactRow> = app
        .order
        .iter()
        .map(|&i| {
            let s = &app.sensors[i];
            CompactRow {
                kind: s.kind,
                device: s.device.clone(),
                sensor: s.label.clone(),
                now: s.current,
                max: s.max.is_finite().then_some(s.max),
                note: s.note.clone(),
                warn: s.warn_threshold(),
                crit: s.crit_threshold(),
            }
        })
        .collect();
    // GPU hotspot / VRAM come from NvAPI or kernel modules, not the sensor list.
    for g in &app.gpu.gpus {
        let vram_crit = g.vram_crit.unwrap_or(100.0);
        for (label, t, warn, crit) in [
            ("hotspot", g.hotspot, 80.0, 95.0),
            ("VRAM (hottest chip)", g.vram_temp, vram_crit - 10.0, vram_crit),
        ] {
            if t.is_some() {
                rows.push(CompactRow {
                    kind: "GPU",
                    device: g.name.clone(),
                    sensor: label.to_string(),
                    now: t,
                    max: None,
                    note: None,
                    warn,
                    crit,
                });
            }
        }
    }
    // Group by category, then device (stable: keeps the sensor sort), so the
    // GPU hotspot/VRAM rows sit with the rest of their card.
    rows.sort_by(|a, b| kind_rank(a.kind).cmp(&kind_rank(b.kind)).then(a.device.cmp(&b.device)));

    // Column width: kind 5 + device 26 + sensor 20 + now 9 + max 13 + gap 3.
    // On a short terminal, use more (narrower) columns before hiding rows.
    const COL_W: u16 = 76;
    const MIN_COL_W: u16 = 50;
    let avail_h = (area.height.saturating_sub(2) as usize).max(1);
    let wide_cols = (area.width.saturating_sub(2) / COL_W).max(1) as usize;
    let max_cols = (area.width.saturating_sub(2) / MIN_COL_W).max(1) as usize;
    let cols = wide_cols.max(rows.len().div_ceil(avail_h).min(max_cols));
    let per_col = rows.len().div_ceil(cols).max(1);
    let hidden = rows.len().saturating_sub(cols * avail_h);
    let mut title = vec![
        Span::raw("compact "),
        Span::styled(format!("· {} temperatures · c: full view ", rows.len()), Style::new().fg(th.dim)),
    ];
    if hidden > 0 {
        title.push(Span::styled(format!("· {hidden} don't fit "), Style::new().fg(th.warn)));
    }
    let block = panels::block(&th, 0, title);
    // Only as tall as the list: compact means no empty box.
    let area = Rect { height: (per_col as u16 + 2).min(area.height), ..area };
    let inner = block.inner(area);
    f.render_widget(block, area);
    let col_w = inner.width / cols as u16;
    // Narrow columns drop the "max" figure before squeezing the names.
    let show_max = col_w >= 5 + 10 + 20 + 9 + 13 + 3;
    let dev_w = (col_w as usize).saturating_sub(5 + 20 + 9 + 3 + if show_max { 13 } else { 0 }).clamp(10, 40);
    let level = |r: &CompactRow, t: f32| {
        if t >= r.crit { th.crit } else if t >= r.warn { th.warn } else { th.ok }
    };
    for (c, chunk) in rows.chunks(per_col).enumerate() {
        let x = inner.x + c as u16 * col_w;
        let lines: Vec<Line> = chunk
            .iter()
            .map(|r| {
                let now = match (r.now, &r.note) {
                    (Some(t), _) => Span::styled(format!("{:>9}", app.unit.fmt(t)), Style::new().fg(level(r, t)).bold()),
                    (None, Some(n)) => Span::styled(format!("{:>9}", ellipsize(n, 9)), Style::new().fg(th.warn)),
                    (None, None) => Span::styled(format!("{:>9}", "n/a"), Style::new().fg(th.dim)),
                };
                let max = match r.max.filter(|_| show_max) {
                    Some(m) => Span::styled(format!("  max {:>7}", app.unit.fmt(m)), Style::new().fg(th.dim)),
                    None if show_max => Span::raw(format!("{:13}", "")),
                    None => Span::raw(""),
                };
                Line::from(vec![
                    Span::styled(format!("{:<5}", r.kind), Style::new().fg(themes::kind_color(r.kind, &th)).bold()),
                    Span::styled(format!("{:<w$} ", ellipsize(&r.device, dev_w - 1), w = dev_w - 1), Style::new().fg(th.fg)),
                    Span::styled(format!("{:<20}", ellipsize(&r.sensor, 19)), Style::new().fg(th.dim)),
                    now,
                    max,
                ])
            })
            .collect();
        let rect = Rect { x, y: inner.y, width: col_w.min(inner.x + inner.width - x), height: inner.height };
        f.render_widget(Paragraph::new(lines), rect);
        // Faint divider between columns.
        if c > 0 {
            let div: Vec<Line> = (0..inner.height.min(per_col as u16)).map(|_| Line::styled("│", Style::new().fg(th.border))).collect();
            f.render_widget(Paragraph::new(div), Rect { x: x.saturating_sub(2), y: inner.y, width: 1, height: inner.height });
        }
    }
}

/// The `v` checklist: every panel with its key, on/off, and what it shows.
fn draw_view_menu(f: &mut Frame, app: &mut App) {
    let th = app.th();
    let area = f.area();
    let w = 84.min(area.width);
    let h = (PANELS.len() as u16 + 2).min(area.height);
    let popup = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let items: Vec<ListItem> = app
        .lay
        .order
        .iter()
        .map(|&i| {
            let (name, desc) = PANELS[i];
            let on = app.show[i];
            let side = if app.lay.left[i] { "◀ left " } else { " right ▶" };
            ListItem::new(Line::from(vec![
                Span::styled(if on { "[x] " } else { "[ ] " }, Style::new().fg(if on { th.ok } else { th.dim })),
                Span::styled(format!("{} ", i + 1), Style::new().fg(th.crit)),
                Span::styled(format!("{name:<8}"), Style::new().fg(if on { th.fg } else { th.dim }).bold()),
                Span::styled(format!("{side}  "), Style::new().fg(th.accent)),
                Span::styled(desc, Style::new().fg(th.dim)),
            ]))
        })
        .collect();
    let mode = if app.lay.is_default() { "default layout (auto-balanced)" } else { "custom layout" };
    let list = List::new(items)
        .style(Style::new().bg(th.bg).fg(th.fg))
        .highlight_style(Style::new().bg(th.sel_bg))
        .highlight_symbol("▶ ")
        .block(
            panel(&th, Line::from(format!(" Panels · {mode} ")))
                .title_bottom(
                    Line::from(" Space show/hide · Ctrl+Shift+↑/↓ move · ←/→ column · d default · Esc close ")
                        .style(Style::new().fg(th.dim)),
                )
                .style(Style::new().bg(th.bg)),
        );
    f.render_widget(Clear, popup);
    let state = app.view.as_mut().unwrap();
    f.render_stateful_widget(list, popup, state);
}

/// A rounded, theme-colored block.
fn panel<'a>(th: &Theme, title: Line<'a>) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(th.border))
        .title(title.style(Style::new().fg(th.accent).bold()))
}

fn draw_picker(f: &mut Frame, app: &mut App) {
    let th = app.th();
    let area = f.area();
    let w = 42.min(area.width);
    let h = (THEMES.len() as u16 + 2).min(area.height);
    let popup = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let items: Vec<ListItem> = THEMES
        .iter()
        .map(|t| {
            // A swatch of each scheme's own colors, so you can compare at a glance.
            let sw = |c: Color| Span::styled("■", Style::new().fg(c));
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:<21}", t.name)),
                sw(t.ok),
                sw(t.warn),
                sw(t.crit),
                sw(t.cool),
                sw(t.hot),
                sw(t.accent),
            ]))
        })
        .collect();
    let list = List::new(items)
        .style(Style::new().bg(th.bg).fg(th.fg))
        .highlight_style(Style::new().bg(th.sel_bg).bold())
        .highlight_symbol("▶ ")
        .block(
            panel(&th, Line::from(format!(" Color scheme {}/{} ", app.theme + 1, THEMES.len())))
                .title_bottom(Line::from(" ↑↓ preview · Enter keep · Esc cancel ").style(Style::new().fg(th.dim)))
                .style(Style::new().bg(th.bg)),
        );
    f.render_widget(Clear, popup);
    let (_, state) = app.picker.as_mut().unwrap();
    f.render_stateful_widget(list, popup, state);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let th = app.th();
    let mut spans = vec![
        Span::styled(" Oryn ", Style::new().bold().fg(th.on_badge).bg(th.accent)),
        Span::raw("  "),
    ];
    match app.cpu_temp() {
        Some((s, t)) => {
            let c = s.level(t).color(&th);
            spans.push(Span::styled(" CPU ", Style::new().bold().fg(th.on_badge).bg(c)));
            spans.push(Span::styled(format!(" {} ", app.unit.fmt(t)), Style::new().bold().fg(c)));
            if let Some(w) = app.cpu_power.watts {
                spans.push(Span::styled(format!("{} ", power::fmt_watts(w)), Style::new().fg(th.fg)));
            }
        }
        None => spans.push(Span::styled(
            " CPU: no sensor (load coretemp / k10temp) ",
            Style::new().fg(th.warn),
        )),
    }
    // GPU, like the CPU: a badge colored by temperature, the temperature and
    // the power draw. With several cards: the hottest one's temperature and
    // their power added up. An integrated GPU gets its own "iGPU" badge, so
    // on a hybrid laptop its (CPU package) temperature isn't mixed with the
    // discrete card's.
    for (integrated, badge) in [(false, " GPU "), (true, " iGPU ")] {
        let gpus = || app.gpu.gpus.iter().filter(move |g| g.on_cpu_die == integrated);
        let gpu_t = gpus().filter_map(|g| g.temp).reduce(f32::max);
        let gpu_w: Vec<f32> = gpus().filter_map(|g| g.power_w).collect();
        if gpu_t.is_none() && gpu_w.is_empty() {
            continue;
        }
        // Same thresholds as the GPU panel's Temp.
        let c = match gpu_t {
            Some(t) if t >= 85.0 => th.crit,
            Some(t) if t >= 70.0 => th.warn,
            Some(_) => th.ok,
            None => th.dim,
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(badge, Style::new().bold().fg(th.on_badge).bg(c)));
        if let Some(t) = gpu_t {
            spans.push(Span::styled(format!(" {} ", app.unit.fmt(t)), Style::new().bold().fg(c)));
        } else {
            spans.push(Span::raw(" "));
        }
        if !gpu_w.is_empty() {
            spans.push(Span::styled(format!("{} ", power::fmt_watts(gpu_w.iter().sum())), Style::new().fg(th.fg)));
        }
    }
    // RAM: the hottest stick, colored like its row in the sensor table, and
    // the DRAM power draw where the CPU reports it.
    let ram_t = app.ram_temp();
    if ram_t.is_some() || app.ram_power.watts.is_some() {
        let c = ram_t.map_or(th.dim, |(s, t)| s.level(t).color(&th));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(" RAM ", Style::new().bold().fg(th.on_badge).bg(c)));
        if let Some((_, t)) = ram_t {
            spans.push(Span::styled(format!(" {} ", app.unit.fmt(t)), Style::new().bold().fg(c)));
        } else {
            spans.push(Span::raw(" "));
        }
        if let Some(w) = app.ram_power.watts {
            spans.push(Span::styled(format!("{} ", power::fmt_watts(w)), Style::new().fg(th.fg)));
        }
    }
    // Battery, only where there is one (laptops, tablets, handhelds):
    // "BAT 54% 2h 10m left 8.4 W", "BAT 87% charging 1h 05m 25 W", "BAT 100% full".
    if let Some(b) = &app.battery {
        let c = if b.percent <= 10.0 {
            th.crit
        } else if b.percent <= 25.0 {
            th.warn
        } else {
            th.ok
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(" BAT ", Style::new().bold().fg(th.on_badge).bg(c)));
        spans.push(Span::styled(format!(" {:.0}% ", b.percent), Style::new().bold().fg(c)));
        let left = b.hours_left.map(battery::fmt_hours);
        let what = match (b.state, left) {
            (battery::State::Discharging, Some(t)) => format!("{t} left "),
            (battery::State::Charging, Some(t)) => format!("charging {t} "),
            (battery::State::Charging, None) => "charging ".into(),
            (battery::State::Full, _) => "full ".into(),
            (battery::State::NotCharging, _) => "plugged in ".into(),
            _ => String::new(),
        };
        spans.push(Span::styled(what, Style::new().fg(th.fg)));
        if let Some(w) = b.watts {
            spans.push(Span::styled(format!("{} ", power::fmt_watts(w)), Style::new().fg(th.fg)));
        }
    }
    if let Some(h) = app.hottest() {
        let t = h.current.unwrap();
        spans.push(Span::styled(" ·  hottest: ", Style::new().fg(th.dim)));
        spans.push(Span::styled(
            format!("{} {}", h.label, app.unit.fmt(t)),
            Style::new().fg(h.level(t).color(&th)).bold(),
        ));
    }
    spans.push(Span::styled(
        format!(
            "  ·  {} sensors · {} · {} ms",
            app.sensors.len(),
            app.source.name(),
            app.interval.as_millis()
        ),
        Style::new().fg(th.dim),
    ));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(th.border));
    let inner = block.inner(area);

    // Clock and version, right-aligned: "Fri 16 Oct · 15:14:07 · v0.1.4". If
    // the header text would run into it, drop the date and version; the text
    // is then cut short before the time.
    let now = chrono::Local::now();
    let time = Span::styled(now.format("%H:%M:%S").to_string(), Style::new().fg(th.accent).bold());
    let text_w = Line::from(spans.clone()).width() as u16;
    let full = Line::from(vec![
        Span::styled(now.format("%a %d %b").to_string(), Style::new().fg(th.dim)),
        Span::styled("  ·  ", Style::new().fg(th.dim)),
        time.clone(),
        Span::styled(concat!("  ·  v", env!("CARGO_PKG_VERSION")), Style::new().fg(th.dim)),
        Span::raw(" "),
    ]);
    let clock = if text_w + 2 + full.width() as u16 <= inner.width { full } else { Line::from(vec![time, Span::raw(" ")]) };
    let clock_w = clock.width() as u16;
    let text_area = Rect { width: inner.width.saturating_sub(clock_w + 1), ..inner };
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(Line::from(spans)), text_area);
    if inner.width > clock_w {
        let r = Rect { x: inner.x + inner.width - clock_w, width: clock_w, ..inner };
        f.render_widget(Paragraph::new(clock).style(Style::new().bg(th.bg)), r);
    }
}

fn bar(frac: f32, width: usize) -> String {
    let frac = frac.clamp(0.0, 1.0);
    let eighths = (frac * width as f32 * 8.0).round() as usize;
    let full = eighths / 8;
    let rem = eighths % 8;
    const PARTS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let mut s = "█".repeat(full);
    if full < width {
        s.push_str(PARTS[rem]);
    }
    let len = s.chars().count();
    s.push_str(&" ".repeat(width.saturating_sub(len)));
    s
}

fn draw_table(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.th();
    let unit = app.unit;
    // Shrink the bar on narrow terminals so the name columns keep room.
    let bar_w = if area.width >= 140 { 20 } else if area.width >= 110 { 14 } else { 8 };
    // Width left for the Device/Sensor columns (split 3:2): total minus
    // borders, highlight symbol, fixed columns and column spacing.
    let flex = area.width.saturating_sub(2 + 2 + 9 + bar_w as u16 + 2 + 9 * 3 + 7) as usize;
    let dev_w = flex * 3 / 5;
    let sensor_w = flex * 2 / 5; // floor, like ratatui, so text never overflows
    let selected = app.table.selected();
    let rows: Vec<Row> = app
        .order
        .iter()
        .enumerate()
        .map(|(pos, &i)| {
            let s = &app.sensors[i];
            let (cur, color, gauge) = match s.current {
                Some(t) => {
                    let frac = t / s.crit_threshold();
                    (unit.fmt(t), s.level(t).color(&th), bar(frac, bar_w))
                }
                None => ("n/a".to_string(), th.dim, " ".repeat(bar_w)),
            };
            let fmt_opt = |v: f32| if v.is_finite() { unit.fmt(v) } else { "—".into() };
            let crit = s.critical.map(|c| unit.fmt(c)).unwrap_or_else(|| "—".into());
            let trend = trend_arrow(&s.history, &th);
            Row::new(vec![
                Cell::from(Line::from(vec![
                    // The category word in its own color: "NVME" in orange, "RAM" in violet…
                    Span::styled(
                        format!("{:<6}", s.kind),
                        Style::new().fg(themes::kind_color(s.kind, &th)).bold(),
                    ),
                    Span::styled(
                        if s.device.is_empty() {
                            "—".to_string()
                        } else {
                            ellipsize(&s.device, dev_w.saturating_sub(6))
                        },
                        Style::new().bold(),
                    ),
                ])),
                // Notes ("needs root", "standby"…) go in the wider Sensor column so
                // they're never cut off: "sdb (SMART) · needs root".
                Cell::from(match &s.note {
                    // Too narrow for both: the note alone (the Device column names the disk).
                    Some(n) if sensor_w < n.chars().count() + 3 + 4 => {
                        Line::styled(ellipsize(n, sensor_w), Style::new().fg(th.warn))
                    }
                    Some(n) => Line::from(vec![
                        Span::styled(
                            ellipsize(&s.label, sensor_w - n.chars().count() - 3),
                            Style::new().fg(th.dim),
                        ),
                        Span::styled(format!(" · {n}"), Style::new().fg(th.warn)),
                    ]),
                    None => Line::styled(ellipsize(&s.label, sensor_w), Style::new().fg(th.dim)),
                }),
                // A sleeping disk's last value is dimmed; with no value at all, a dash.
                Cell::from(match (&s.note, s.current) {
                    (Some(_), None) => Span::styled("—", Style::new().fg(th.warn)),
                    (Some(_), Some(_)) => Span::styled(cur, Style::new().fg(th.dim)),
                    (None, _) => Span::styled(cur, Style::new().fg(color).bold()),
                }),
                Cell::from(Span::styled(gauge, Style::new().fg(color))),
                Cell::from(trend),
                Cell::from(fmt_opt(s.min)).style(Style::new().fg(th.cool)),
                Cell::from(fmt_opt(s.max)).style(Style::new().fg(th.hot)),
                Cell::from(crit).style(Style::new().fg(th.dim)),
            ])
            // The selected row's background is set here, under the cells, so
            // the colored badge stays visible on top of it.
            .style(if selected == Some(pos) {
                Style::new().bg(th.sel_bg).add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            })
        })
        .collect();

    let sort_label = match app.sort {
        Sort::Name => "name",
        Sort::TempDesc => "temp ↓",
    };
    let header = Row::new(["Device", "Sensor", "Now", "vs crit", "", "Min", "Max", "Crit"])
        .style(Style::new().bold().fg(th.accent))
        .bottom_margin(0);
    let table = Table::new(
        rows,
        [
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Length(9),
            Constraint::Length(bar_w as u16),
            Constraint::Length(2),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
        ],
    )
    .header(header)
    .row_highlight_style(Style::new())
    .highlight_symbol("▶ ")
    .block(panels::block(
        &th,
        P_SENSORS + 1,
        vec![
            Span::raw("sensors "),
            Span::styled(format!("· sort: {sort_label} (s) "), Style::new().fg(th.fg)),
        ],
    )
    // With the process list on screen too, light up whichever has the keys.
    .border_style(Style::new().fg(if app.show[P_PROC] && !app.proc_focused() { th.accent } else { th.border })));
    f.render_stateful_widget(table, area, &mut app.table);
}

/// Display order of the category groups; unknown tags go last.
const KIND_ORDER: [&str; 15] = [
    "CPU", "GPU", "RAM", "NVME", "SSD", "HDD", "DISK", "USB", "NET", "WIFI", "AUDIO", "MB",
    "CHIP", "ACPI", "BAT",
];

fn kind_rank(kind: &str) -> usize {
    KIND_ORDER.iter().position(|k| *k == kind).unwrap_or(KIND_ORDER.len())
}

/// Cut `s` to at most `w` characters, ending in "…" when shortened.
fn ellipsize(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else if w == 0 {
        String::new()
    } else {
        s.chars().take(w - 1).chain(std::iter::once('…')).collect()
    }
}

fn trend_arrow(h: &VecDeque<f32>, th: &Theme) -> Span<'static> {
    let n = h.len();
    if n < 6 {
        return Span::raw(" ");
    }
    let recent: f32 = h.iter().skip(n - 3).sum::<f32>() / 3.0;
    let before: f32 = h.iter().skip(n - 6).take(3).sum::<f32>() / 3.0;
    let d = recent - before;
    if d > 0.5 {
        Span::styled("↑", Style::new().fg(th.crit))
    } else if d < -0.5 {
        Span::styled("↓", Style::new().fg(th.cool))
    } else {
        Span::styled("→", Style::new().fg(th.dim))
    }
}

fn draw_chart(f: &mut Frame, app: &App, area: Rect) {
    let th = app.th();
    let Some(s) = app.selected() else {
        f.render_widget(panels::block(&th, P_HISTORY + 1, vec![Span::raw("history ")]), area);
        return;
    };
    let unit = app.unit;
    let data: Vec<(f64, f64)> = s
        .history
        .iter()
        .enumerate()
        .map(|(i, &t)| ((i + HISTORY_LEN - s.history.len()) as f64, unit.conv(t) as f64))
        .collect();

    let warn = unit.conv(s.warn_threshold()) as f64;
    let crit = unit.conv(s.crit_threshold()) as f64;
    let warn_line: Vec<(f64, f64)> = vec![(0.0, warn), (HISTORY_LEN as f64, warn)];
    let crit_line: Vec<(f64, f64)> = vec![(0.0, crit), (HISTORY_LEN as f64, crit)];

    let (lo, hi) = data.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &(_, y)| {
        (lo.min(y), hi.max(y))
    });
    let (lo, hi) = if data.is_empty() { (0.0, 100.0) } else { (lo, hi) };
    // Show the warn line if we're anywhere near it; pad the range a bit.
    let y_min = (lo - 5.0).floor();
    let y_max = (hi.max(warn.min(hi + 15.0)) + 5.0).ceil();

    let line_color = s.current.map(|t| s.level(t).color(&th)).unwrap_or(th.dim);
    let mut datasets = vec![Dataset::default()
        .name(s.label.clone())
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::new().fg(line_color))
        .data(&data)];
    if warn <= y_max {
        datasets.push(
            Dataset::default()
                .name("warn")
                .marker(symbols::Marker::Dot)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(th.warn).add_modifier(Modifier::DIM))
                .data(&warn_line),
        );
    }
    if crit <= y_max {
        datasets.push(
            Dataset::default()
                .name("crit")
                .marker(symbols::Marker::Dot)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(th.crit).add_modifier(Modifier::DIM))
                .data(&crit_line),
        );
    }

    let secs = |samples: f64| (samples * app.interval.as_secs_f64()).round() as i64;
    let x_labels = vec![
        Span::raw(format!("-{}s", secs(HISTORY_LEN as f64))),
        Span::raw(format!("-{}s", secs(HISTORY_LEN as f64 / 2.0))),
        Span::raw("now"),
    ];
    let sym = match unit {
        Unit::C => "°C",
        Unit::F => "°F",
    };
    let y_labels = vec![
        Span::raw(format!("{:.0}{sym}", y_min)),
        Span::raw(format!("{:.0}{sym}", (y_min + y_max) / 2.0)),
        Span::raw(format!("{:.0}{sym}", y_max)),
    ];

    let chart = Chart::new(datasets)
        .block(panels::block(
            &th,
            P_HISTORY + 1,
            vec![
                Span::raw(format!("history · {} · {} ", s.device, s.label)),
                Span::styled("·· warn ", Style::new().fg(th.warn)),
                Span::styled("·· crit ", Style::new().fg(th.crit)),
            ],
        ))
        .legend_position(None)
        .x_axis(
            Axis::default()
                .bounds([0.0, HISTORY_LEN as f64])
                .labels(x_labels)
                .style(Style::new().fg(th.dim)),
        )
        .y_axis(
            Axis::default()
                .bounds([y_min, y_max])
                .labels(y_labels)
                .style(Style::new().fg(th.dim)),
        );
    f.render_widget(chart, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_panels_round_trip_and_upgrade() {
        // Old (pre-GPU) file: only shown panels listed. History and proc were
        // hidden and must stay hidden; the new GPU panel starts shown.
        let old = parse_saved("sensors,cpu,mem,disks,net\n").unwrap().0;
        assert_eq!(old, [true, false, true, true, true, true, false, true, true]);
        // New format keeps hidden panels explicitly, including a hidden GPU.
        let new = parse_saved("sensors,-history,cpu,mem,disks,net,-proc,-gpu,-specs\n").unwrap().0;
        assert_eq!(new, [true, false, true, true, true, true, false, false, false]);
        assert_eq!(parse_saved("none").unwrap().0, [false; NP]);
        // Old files keep the default arrangement.
        assert!(parse_saved("sensors,cpu\n").unwrap().1.is_default());
        // --panels: only what's listed.
        assert_eq!(parse_panels("gpu,cpu").unwrap(), [false, false, true, false, false, false, false, true, false]);
        assert!(parse_panels("cpu,bogus").is_none());
    }

    #[test]
    fn layout_round_trip() {
        let mut lay = PanelLayout::default();
        lay.move_by(P_PROC, -10); // processes to the top
        lay.left[P_CPU] = true; // CPU into the left column
        let show = [true, false, true, true, true, true, true, true, false];
        let text = {
            let parts: Vec<String> = lay
                .order
                .iter()
                .map(|&i| {
                    let side = if lay.left[i] { "L" } else { "R" };
                    if show[i] { format!("{}:{side}", PANELS[i].0) } else { format!("-{}:{side}", PANELS[i].0) }
                })
                .collect();
            parts.join(",")
        };
        let (s2, l2) = parse_saved(&text).unwrap();
        assert_eq!(s2, show);
        assert_eq!(l2, lay);
        assert_eq!(l2.order[0], P_PROC);
        assert!(!l2.is_default());
        // A panel missing from a saved custom order is put back, not lost.
        let (_, l3) = parse_saved("proc:R,sensors:L").unwrap();
        assert_eq!(l3.order.len(), NP);
        assert_eq!(l3.order[0], P_PROC);
    }

    #[test]
    fn interval_steps_by_100ms() {
        let ms = |v| Duration::from_millis(v);
        assert_eq!(step_interval(ms(1000), 1), ms(1100));
        assert_eq!(step_interval(ms(1000), -1), ms(900));
        assert_eq!(step_interval(ms(100), -1), ms(100)); // floor
        assert_eq!(step_interval(ms(10_000), 1), ms(10_000)); // ceiling
        assert_eq!(step_interval(ms(250), 1), ms(300));
        assert_eq!(step_interval(ms(250), -1), ms(200));
    }

    #[test]
    fn core_ranges() {
        assert_eq!(ranges(&[0, 1, 2, 3, 4, 5, 6, 7, 16, 17, 18, 19, 20, 21, 22, 23]), "C0–7, C16–23");
        assert_eq!(ranges(&[5]), "C5");
        assert_eq!(ranges(&[]), "");
    }
}

#[cfg(test)]
mod render_fuzz {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn app() -> App {
        sysstats::set_demo();
        let mut a = App::new(Box::new(DemoSource::new()), Duration::from_millis(1000), 1, [true; NP], PanelLayout::default());
        a.stats.refresh(Want { cpu: true, mem: true, disks: true, net: true, procs: true });
        a
    }

    #[test]
    fn draws_at_every_size_and_panel_combo() {
        let mut a = app();
        let mut rng = 12345u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let sizes: Vec<(u16, u16)> = (1..=12u16)
            .flat_map(|w| (1..=6u16).map(move |h| (w * 17, h * 9)))
            .chain([(1, 1), (2, 2), (10, 3), (20, 5), (40, 12), (80, 24), (149, 40), (150, 40), (300, 100), (500, 20)])
            .collect();
        for combo in 0..120u64 {
            let bits = if combo < 2 { if combo == 0 { 0x1ff } else { 0 } } else { next() };
            for i in 0..NP {
                a.show[i] = bits >> i & 1 == 1;
            }
            a.compact = combo % 7 == 3;
            a.focus_proc = combo % 2 == 0;
            a.lay = if combo % 3 == 0 {
                let mut l = PanelLayout::default();
                l.move_by((next() % NP as u64) as usize, -3);
                l.left[(next() % NP as u64) as usize] ^= true;
                l
            } else {
                PanelLayout::default()
            };
            a.picker = (combo % 11 == 5).then(|| (a.theme, ListState::default().with_selected(Some(99))));
            a.view = (combo % 13 == 6).then(|| ListState::default().with_selected(Some(8)));
            a.pv.prompt = match combo % 5 {
                1 => Some(panels::Prompt::Filter),
                2 => Some(panels::Prompt::Exec("x".repeat(300))),
                3 => Some(panels::Prompt::Nice { pid: 1, name: "n".repeat(50), buf: "-5".into() }),
                4 => Some(panels::Prompt::Confirm { sig: procctl::Sig::Kill, pid: 1, name: "zz".into() }),
                _ => None,
            };
            a.pv.filter = if combo % 4 == 1 { "zzzz-no-match".into() } else { String::new() };
            a.pv.set_status("s".repeat((next() % 200) as usize), combo % 2 == 0);
            a.unit = if combo % 2 == 0 { Unit::C } else { Unit::F };
            for &(w, h) in &sizes {
                let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
                t.draw(|f| draw(f, &mut a)).unwrap_or_else(|e| panic!("{w}x{h} combo {combo}: {e}"));
                // Keys and the mouse right after drawing, like the real loop.
                a.proc_move((next() % 7) as isize - 3);
                a.mouse(crossterm::event::MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: (next() % w as u64) as u16,
                    row: (next() % h as u64) as u16,
                    modifiers: KeyModifiers::NONE,
                });
            }
        }
    }
    #[test]
    fn short_window_scrolls_through_every_panel() {
        // A 1366×768 laptop: about 134×30 cells, one column.
        let mut a = app();
        let mut seen = [false; NP];
        let mut footer = String::new();
        for _ in 0..NP {
            let mut t = Terminal::new(TestBackend::new(134, 30)).unwrap();
            t.draw(|f| draw(f, &mut a)).unwrap();
            let buf = t.backend().buffer();
            footer = (0..134).map(|x| buf[(x, 29)].symbol()).collect();
            let off = a.offscreen.borrow();
            for (p, s) in seen.iter_mut().enumerate() {
                *s |= !off.above.contains(&p) && !off.below.contains(&p);
            }
            if a.scroll == 0 {
                assert!(off.above.is_empty() && !off.below.is_empty(), "nothing left out at the top");
                assert!(footer.contains("↓ "), "{footer}");
            }
            drop(off);
            a.scroll_by(1);
        }
        assert!(seen.iter().all(|s| *s), "never on screen: {seen:?}");
        // Scrolled to the bottom: nothing more below, and it stops there.
        assert!(a.offscreen.borrow().below.is_empty() && footer.contains("↑ "), "{footer}");
        let max = a.offscreen.borrow().max_scroll;
        a.scroll_by(5);
        assert_eq!(a.scroll, max);
        a.scroll_by(-100);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn panel_menu_spells_out_the_move_keys() {
        let mut a = app();
        a.view = Some(ListState::default().with_selected(Some(0)));
        let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
        t.draw(|f| draw(f, &mut a)).unwrap();
        let text: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Space show/hide · Ctrl+Shift+↑/↓ move · ←/→ column · d default · Esc close"), "{text}");

        // Ctrl+Shift+↓ moves the highlighted panel down one place.
        let first = a.lay.order[0];
        a.view_key(KeyCode::Down, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(a.lay.order[1], first);
    }

    #[test]
    fn header_badges_igpu_separately() {
        let mut a = app();
        let gpu = |on_cpu_die, temp, power_w| gpustats::GpuView { on_cpu_die, temp: Some(temp), power_w, ..Default::default() };
        let header = |a: &App| {
            let mut t = Terminal::new(TestBackend::new(300, 3)).unwrap();
            t.draw(|f| draw_header(f, a, f.area())).unwrap();
            t.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
        };
        // Laptop with only the integrated GPU.
        a.gpu.gpus = vec![gpu(true, 44.0, None)];
        let h = header(&a);
        assert!(h.contains(" iGPU  44.0°C") && !h.contains(" GPU "), "{h}");
        // Hybrid laptop: each gets its own badge.
        a.gpu.gpus = vec![gpu(true, 44.0, None), gpu(false, 61.0, Some(35.0))];
        let h = header(&a);
        assert!(h.contains(" GPU  61.0°C 35 W") && h.contains(" iGPU  44.0°C"), "{h}");
        // No battery (a desktop): no badge. A laptop on battery: charge, time left, rate.
        assert!(!h.contains("BAT"), "{h}");
        a.battery = Some(battery::Battery { percent: 54.0, state: battery::State::Discharging, watts: Some(8.4), hours_left: Some(2.1667) });
        let h = header(&a);
        assert!(h.contains(" BAT  54% 2h 10m left 8.4 W"), "{h}");
        assert!(h.contains(concat!(" ·  v", env!("CARGO_PKG_VERSION"))), "version after the clock: {h}");
    }
}

#[cfg(test)]
mod sensor_lifetime {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A source whose list of sensors the test controls.
    struct Fake(Rc<RefCell<Vec<&'static str>>>);
    impl Source for Fake {
        fn read(&mut self) -> Vec<Reading> {
            self.0
                .borrow()
                .iter()
                .map(|id| Reading {
                    id: id.to_string(),
                    label: id.to_string(),
                    device: "dev".into(),
                    temp: Some(40.0),
                    critical: None,
                    cpu: false,
                    kind: "USB",
                    note: None,
                })
                .collect()
        }
        fn name(&self) -> &'static str {
            "fake"
        }
    }

    #[test]
    fn unplugged_sensors_are_marked_then_dropped() {
        let ids = Rc::new(RefCell::new(vec!["a", "usb@6:0:0:0"]));
        let mut app = App::new(Box::new(Fake(ids.clone())), Duration::from_millis(1000), 0, [true; NP], PanelLayout::default());
        assert_eq!(app.sensors.len(), 2);
        app.table.select(Some(1));
        // Unplugged: shown as disconnected, without its old temperature.
        ids.borrow_mut().pop();
        app.sample();
        let gone = app.sensors.iter().find(|s| s.id == "usb@6:0:0:0").unwrap();
        assert_eq!((gone.current, gone.note.as_deref()), (None, Some("disconnected")));
        // Plugged back in at a new address: a new row, the old one still listed.
        ids.borrow_mut().push("usb@7:0:0:0");
        app.sample();
        assert_eq!(app.sensors.len(), 3);
        // A minute later the old row is gone, and the selection is still valid.
        for s in &mut app.sensors {
            if s.gone_since.is_some() {
                s.gone_since = Some(Instant::now() - GONE_KEEP - Duration::from_secs(1));
            }
        }
        app.sample();
        assert_eq!(app.sensors.len(), 2);
        assert!(app.sensors.iter().all(|s| s.gone_since.is_none()));
        assert!(app.table.selected().is_some_and(|i| i < app.order.len()));
    }
}
