//! Renders the README screenshot (`docs/screenshot.png`) without a terminal.
//!
//! `--demo` only simulates the sensors and the CPU panel; memory, disks,
//! network and GPU would come from the machine it runs on and end up in a
//! public picture. So this draws the demo with made-up figures for those
//! too, into ratatui's test backend, and writes the cells as an HTML page:
//!
//! ```sh
//! ORYN_SCREENSHOT=target/screenshot.html cargo test screenshot -- --ignored
//! chromium --headless --hide-scrollbars --force-device-scale-factor=1 \
//!     --window-size=1437,802 --screenshot=docs/screenshot.png target/screenshot.html
//! ```
//!
//! The test prints the exact `--window-size` for the page.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::Duration;

use ratatui::{backend::TestBackend, buffer::Buffer, style::Color, style::Modifier, Terminal};

use crate::gpustats::GpuView;
use crate::sensors::DemoSource;
use crate::sysstats::{DiskInfo, Mem, NetInfo};
use crate::{draw, themes, App, PanelLayout, NP, P_PROC, P_SPECS};

const COLS: u16 = 168;
const ROWS: u16 = 46;
/// DejaVu Sans Mono: advance width 1233/2048 em.
const FONT_PX: f64 = 14.0;
const CELL_W: f64 = FONT_PX * 1233.0 / 2048.0;
const LINE_PX: f64 = 17.0;
const PAD: f64 = 10.0;
const GIB: u64 = 1 << 30;

/// A wave between `lo` and `hi`, for the made-up history graphs.
fn wave(n: usize, lo: f64, hi: f64, period: f64) -> VecDeque<f64> {
    (0..n)
        .map(|i| {
            let x = i as f64 / period * std::f64::consts::TAU;
            lo + (hi - lo) * (0.5 + 0.35 * x.sin() + 0.15 * (x * 3.7).sin())
        })
        .collect()
}

fn demo_app() -> App {
    crate::sysstats::set_demo();
    // Everything except the process list and the specs, which would show
    // this machine's processes, host name and hardware.
    let mut show = [true; NP];
    show[P_PROC] = false;
    show[P_SPECS] = false;
    let theme = themes::find("tokyo-night").expect("Tokyo Night theme");
    let mut app = App::new(Box::new(DemoSource::new()), Duration::from_millis(1000), theme, show, PanelLayout::default());
    // Fill the history graphs.
    for _ in 0..130 {
        app.sample();
    }

    let s = &mut app.stats;
    s.uptime = 3 * 86400 + 4 * 3600 + 17 * 60;
    s.load = (1.42, 1.18, 0.97);
    s.mem = Mem {
        total: 62 * GIB + GIB / 4,
        used: 14 * GIB + GIB / 3,
        available: 47 * GIB + GIB / 2,
        cached: 18 * GIB + GIB / 5,
        free: 29 * GIB + GIB / 3,
        swap_total: 16 * GIB,
        swap_used: 0,
    };
    s.disks = vec![
        DiskInfo {
            label: "root".into(),
            mount: "/".into(),
            total: 1863 * GIB,
            used: 612 * GIB,
            read_rate: 2.4e6,
            write_rate: 860e3,
        },
        DiskInfo {
            label: "data".into(),
            mount: "/mnt/data".into(),
            total: 7452 * GIB,
            used: 4980 * GIB,
            read_rate: 0.0,
            write_rate: 0.0,
        },
    ];
    let hist = crate::sysstats::HIST;
    s.nets = vec![NetInfo {
        name: "enp5s0".into(),
        // TEST-NET-1: an address reserved for documentation.
        ip: Some("192.0.2.24".into()),
        rx_rate: 3.1e6,
        tx_rate: 940e3,
        rx_total: 41 * GIB / 10,
        tx_total: 612 << 20,
        rx_hist: wave(hist, 0.4e6, 4.2e6, 23.0),
        tx_hist: wave(hist, 0.2e6, 1.6e6, 17.0),
    }];
    s.net_sel = 0;

    app.gpu.gpus = vec![GpuView {
        name: "AMD Radeon RX 7800 XT".into(),
        util: Some(64.0),
        vram_used: Some(9.3 * GIB as f64),
        vram_total: Some(16.0 * GIB as f64),
        temp: Some(61.0),
        hotspot: Some(78.0),
        vram_temp: Some(70.0),
        vram_crit: Some(100.0),
        power_w: Some(212.0),
        power_limit_w: Some(263.0),
        fan_pct: Some(41.0),
        core_mhz: Some(2430.0),
        mem_mhz: Some(2438.0),
        hist: wave(hist, 40.0, 80.0, 29.0).into_iter().map(|v| v as f32).collect(),
        ..Default::default()
    }];
    app
}

fn css(c: Color) -> Option<String> {
    Some(match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Reset => return None,
        Color::Black => "#15161e".into(),
        Color::Red => "#f7768e".into(),
        Color::Green => "#9ece6a".into(),
        Color::Yellow => "#e0af68".into(),
        Color::Blue => "#7aa2f7".into(),
        Color::Magenta => "#bb9af7".into(),
        Color::Cyan => "#7dcfff".into(),
        Color::Gray => "#a9b1d6".into(),
        Color::DarkGray => "#414868".into(),
        Color::White => "#c0caf5".into(),
        other => panic!("screenshot: no CSS for {other:?}"),
    })
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The buffer as a page of colored text: one fixed-size box per cell, so
/// glyphs from a fallback font (braille, arrows) can't shift the grid.
fn html(buf: &Buffer, bg: &str, fg: &str) -> String {
    let w = buf.area.width as usize;
    let mut rows = String::new();
    for y in 0..buf.area.height as usize {
        rows.push_str("<div>");
        // Oryn draws no double-width characters, so every cell is one column.
        for cell in &buf.content()[y * w..(y + 1) * w] {
            let (mut f, mut b) = (css(cell.fg), css(cell.bg));
            if cell.modifier.contains(Modifier::REVERSED) {
                (f, b) = (b.or(Some(bg.into())), f.or(Some(fg.into())));
            }
            let mut style = String::new();
            if let Some(f) = f {
                let _ = write!(style, "color:{f};");
            }
            if let Some(b) = b {
                let _ = write!(style, "background:{b};");
            }
            if cell.modifier.contains(Modifier::BOLD) {
                style.push_str("font-weight:bold;");
            }
            if cell.modifier.contains(Modifier::DIM) {
                style.push_str("opacity:.6;");
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                style.push_str("font-style:italic;");
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                style.push_str("text-decoration:underline;");
            }
            let sym = cell.symbol();
            let sym = escape(if sym.is_empty() { " " } else { sym });
            if style.is_empty() {
                let _ = write!(rows, "<i>{sym}</i>");
            } else {
                let _ = write!(rows, "<i style=\"{style}\">{sym}</i>");
            }
        }
        rows.push_str("</div>");
    }
    format!(
        "<!doctype html><meta charset=utf-8><style>\
         html,body{{margin:0;background:{bg}}}\
         body{{padding:{PAD}px;color:{fg};font:{FONT_PX}px/{LINE_PX}px 'DejaVu Sans Mono',monospace}}\
         div{{height:{LINE_PX}px;white-space:nowrap}}\
         i{{display:inline-block;width:{CELL_W:.4}px;height:{LINE_PX}px;overflow:hidden;\
         font-style:normal;text-align:center;white-space:pre;vertical-align:top}}\
         </style><body>{rows}</body>"
    )
}

#[test]
#[ignore = "writes the README screenshot page; run on request"]
fn screenshot() {
    let mut app = demo_app();
    let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let th = app.th();
    let bg = css(th.bg).unwrap_or_else(|| "#1a1b26".into());
    let fg = css(th.fg).unwrap_or_else(|| "#c0caf5".into());
    let page = html(term.backend().buffer(), &bg, &fg);
    let out = crate::env_str("SCREENSHOT").unwrap_or_else(|| "target/screenshot.html".into());
    std::fs::write(&out, page).unwrap();
    let w = (COLS as f64 * CELL_W + 2.0 * PAD).ceil();
    let h = (ROWS as f64 * LINE_PX + 2.0 * PAD).ceil();
    println!("wrote {out}; screenshot it with --window-size={w},{h}");
}
