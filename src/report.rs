//! The `l` / `L` key: write everything Oryn is showing to a plain-text log in
//! `/tmp` (`oryn-20261017-164503.log`), for sharing or comparing later.
//!
//! The log has the header figures, every sensor (not only the rows that fit
//! on screen) and one section per panel that is switched on. Hidden panels
//! are left out because their figures aren't being refreshed. In compact
//! mode that means only the temperatures and the GPUs.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::sysstats::{human, human_bits};
use crate::{cpu_temps, power, App, Sensor, PANELS, P_CPU, P_DISKS, P_GPU, P_MEM, P_NET, P_PROC, P_SPECS};

/// Write the log and return where it went.
pub fn save(app: &App) -> io::Result<PathBuf> {
    let now = chrono::Local::now();
    #[cfg(unix)]
    let dir = PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let dir = std::env::temp_dir();
    write_in(&dir, &now.format("%Y%m%d-%H%M%S").to_string(), &render(app, &now))
}

/// Create `oryn-<stamp>.log` in `dir` (`-2`, `-3`… if pressed twice in a
/// second). `create_new` never follows or overwrites an existing file, so a
/// link planted in the shared /tmp can't redirect the write. Readable by
/// its owner only: process command lines can hold things like tokens.
fn write_in(dir: &Path, stamp: &str, text: &str) -> io::Result<PathBuf> {
    for n in 1.. {
        let name = if n == 1 { format!("oryn-{stamp}.log") } else { format!("oryn-{stamp}-{n}.log") };
        let path = dir.join(name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        match opts.open(&path) {
            Ok(mut f) => {
                f.write_all(text.as_bytes())?;
                // Under sudo, the log belongs to the user who ran it.
                crate::themes::give_back(&path);
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 100 => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

fn opt<T>(v: Option<T>, f: impl Fn(T) -> String) -> String {
    v.map(f).unwrap_or_else(|| "n/a".into())
}

fn section(out: &mut String, title: &str) {
    let _ = write!(out, "\n== {title} ==\n");
}

fn render(app: &App, now: &chrono::DateTime<chrono::Local>) -> String {
    let unit = app.unit;
    let deg = |t: f32| unit.fmt(t);
    let shown = |p: usize| app.show[p] && !app.compact;
    let mut out = String::new();
    let o = &mut out;

    let _ = writeln!(o, "Oryn {} log — {}", env!("CARGO_PKG_VERSION"), now.format("%Y-%m-%d %H:%M:%S %:z"));
    let _ = writeln!(
        o,
        "Source: {} · {} sensors · refresh {} ms{}",
        app.source.name(),
        app.sensors.len(),
        app.interval.as_millis(),
        if app.compact { " · compact mode" } else { "" }
    );
    let shown_names: Vec<&str> = (0..PANELS.len()).filter(|&p| shown(p)).map(|p| PANELS[p].0).collect();
    let _ = writeln!(o, "Panels: {}", if shown_names.is_empty() { "none".into() } else { shown_names.join(", ") });

    // Header figures.
    section(o, "Summary");
    let watts = |m: &power::PowerMeter| match (m.watts, m.note) {
        (Some(w), _) => power::fmt_watts(w),
        (None, Some(note)) => format!("power n/a ({note})"),
        (None, None) => "power n/a".into(),
    };
    let _ = writeln!(o, "CPU      {:<10} {}", opt(app.cpu_temp(), |(_, t)| deg(t)), watts(&app.cpu_power));
    if !app.gpu.gpus.is_empty() {
        let t = app.gpu.gpus.iter().filter_map(|g| g.temp).reduce(f32::max);
        let w: Vec<f32> = app.gpu.gpus.iter().filter_map(|g| g.power_w).collect();
        let w = if w.is_empty() { "power n/a".into() } else { power::fmt_watts(w.iter().sum()) };
        let _ = writeln!(o, "GPU      {:<10} {}", opt(t, deg), w);
    }
    if let Some((_, t)) = app.ram_temp() {
        let _ = writeln!(o, "RAM      {:<10} {}", deg(t), watts(&app.ram_power));
    }
    if let Some(h) = app.hottest() {
        let _ = writeln!(o, "Hottest  {} ({}) {}", h.label, h.device, deg(h.current.unwrap()));
    }

    // Every sensor, in the table's current order.
    section(o, "Sensors");
    let _ = writeln!(
        o,
        "{:<5} {:<34} {:<24} {:>9} {:>9} {:>9} {:>9}  NOTE",
        "KIND", "DEVICE", "SENSOR", "NOW", "MIN", "MAX", "CRIT"
    );
    let row = |s: &Sensor| {
        let now = if s.gone_since.is_some() { "gone".into() } else { opt(s.current, deg) };
        let seen = s.min.is_finite();
        let note = if s.gone_since.is_some() { Some("disconnected") } else { s.note.as_deref() };
        format!(
            "{:<5} {:<34} {:<24} {:>9} {:>9} {:>9} {:>9}  {}",
            s.kind,
            s.device,
            s.label,
            now,
            if seen { deg(s.min) } else { "n/a".into() },
            if seen { deg(s.max) } else { "n/a".into() },
            deg(s.crit_threshold()),
            note.unwrap_or("")
        )
    };
    for &i in &app.order {
        let _ = writeln!(o, "{}", row(&app.sensors[i]).trim_end());
    }

    if shown(P_CPU) {
        let s = &app.stats;
        section(o, "CPU");
        let _ = writeln!(o, "Model    {}", s.cpu_name);
        let _ = writeln!(o, "Usage    {:.1}%", s.cpu_total);
        let _ = writeln!(o, "Clock    {} MHz (fastest core {} MHz)", s.cpu_freq_mhz, s.cpu_freq_max_mhz);
        let _ = writeln!(o, "Power    {}", watts(&app.cpu_power));
        let _ = writeln!(o, "Load     {:.2} {:.2} {:.2}", s.load.0, s.load.1, s.load.2);
        let _ = writeln!(o, "Uptime   {}", uptime(s.uptime));
        let temps = cpu_temps(app);
        if let Some((name, t)) = &temps.package {
            let _ = writeln!(o, "Temp     {name} {}", deg(t.c));
        }
        for (name, t, cores) in &temps.ccds {
            let _ = writeln!(o, "         {name} {} ({cores})", deg(t.c));
        }
        for (i, u) in s.cpu_cores.iter().enumerate() {
            let t = temps.per_core.get(i).copied().flatten().map(|t| format!("  {}", deg(t.c)));
            let _ = writeln!(o, "C{i:<3}     {u:>5.1}%{}", t.unwrap_or_default());
        }
    }

    if shown(P_MEM) {
        let m = &app.stats.mem;
        section(o, "Memory");
        for (name, v) in [
            ("Total", m.total),
            ("Used", m.used),
            ("Available", m.available),
            ("Cached", m.cached),
            ("Free", m.free),
        ] {
            let _ = writeln!(o, "{name:<10} {}", human(v as f64));
        }
        let _ = writeln!(o, "{:<10} {} of {}", "Swap", human(m.swap_used as f64), human(m.swap_total as f64));
    }

    if shown(P_DISKS) {
        section(o, "Disks");
        for d in &app.stats.disks {
            let _ = writeln!(
                o,
                "{:<24} {:<12} {} of {} used · read {}/s · write {}/s",
                d.mount,
                d.label,
                human(d.used as f64),
                human(d.total as f64),
                human(d.read_rate),
                human(d.write_rate)
            );
        }
    }

    if shown(P_NET) {
        section(o, "Network");
        for n in &app.stats.nets {
            let _ = writeln!(o, "{}{}", n.name, n.ip.as_deref().map(|ip| format!(" ({ip})")).unwrap_or_default());
            let _ = writeln!(
                o,
                "  down {}/s ({}) · total {}",
                human(n.rx_rate),
                human_bits(n.rx_rate),
                human(n.rx_total as f64)
            );
            let _ = writeln!(
                o,
                "  up   {}/s ({}) · total {}",
                human(n.tx_rate),
                human_bits(n.tx_rate),
                human(n.tx_total as f64)
            );
        }
    }

    // The GPU panel, or in compact mode the GPU temperatures it lists.
    if (app.show[P_GPU] || app.compact) && !app.gpu.gpus.is_empty() {
        section(o, "GPU");
        for g in &app.gpu.gpus {
            let _ = writeln!(o, "{}", g.name);
            let _ = writeln!(
                o,
                "  temp {} · hotspot {} · VRAM {}",
                opt(g.temp, deg),
                opt(g.hotspot, deg) + &g.hotspot_note.map(|n| format!(" ({n})")).unwrap_or_default(),
                opt(g.vram_temp, deg)
            );
            if !g.vram_chips.is_empty() {
                let chips: Vec<String> = g.vram_chips.iter().map(|(l, t)| format!("{l} {}", deg(*t))).collect();
                let _ = writeln!(o, "  VRAM chips {}", chips.join("  "));
            }
            if app.compact {
                continue;
            }
            let vram = match (g.vram_used, g.vram_total) {
                (Some(u), Some(t)) => format!("{} of {}", human(u), human(t)),
                _ => "n/a".into(),
            };
            let _ = writeln!(o, "  usage {} · VRAM {vram}", opt(g.util, |u| format!("{u:.0}%")));
            let _ = writeln!(
                o,
                "  power {} (limit {}) · fan {} · core {} · memory {}",
                opt(g.power_w, power::fmt_watts),
                opt(g.power_limit_w, power::fmt_watts),
                opt(g.fan_pct, |f| format!("{f:.0}%")),
                opt(g.core_mhz, |m| format!("{m:.0} MHz")),
                opt(g.mem_mhz, |m| format!("{m:.0} MHz"))
            );
        }
    }

    if shown(P_PROC) {
        let procs = &app.stats.procs;
        let vis = app.pv.visible(procs);
        section(o, "Processes");
        let filter = if app.pv.filter.is_empty() { String::new() } else { format!(", filter \"{}\"", app.pv.filter) };
        let _ = writeln!(o, "{} of {} listed{filter}", vis.len(), app.stats.proc_count);
        let _ = writeln!(o, "{:>8} {:<12} {:>4} {:>7} {:>10}  COMMAND", "PID", "USER", "NI", "CPU%", "MEM");
        for &i in &vis {
            let p = &procs[i];
            let cmd = if p.cmd.is_empty() { &p.name } else { &p.cmd };
            let _ = writeln!(
                o,
                "{:>8} {:<12} {:>4} {:>7.1} {:>10}  {}",
                p.pid,
                p.user,
                opt(p.nice, |n| n.to_string()),
                p.cpu,
                human(p.mem as f64),
                cmd
            );
        }
    }

    if let Some((specs, _)) = app.specs.as_ref().filter(|_| shown(P_SPECS)) {
        section(o, "System");
        for l in &specs.lines {
            let _ = writeln!(o, "{:<10} {}", l.label, l.value);
        }
    }
    out
}

fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60);
    if d > 0 { format!("{d}d {h}h {m}m") } else { format!("{h}h {m}m") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{sensors::DemoSource, PanelLayout, NP};
    use std::time::Duration;

    fn demo_app() -> App {
        crate::sysstats::set_demo();
        let mut a = App::new(Box::new(DemoSource::new()), Duration::from_millis(1000), 1, [true; NP], PanelLayout::default());
        a.stats.refresh(crate::sysstats::Want { cpu: true, mem: true, disks: true, net: true, procs: true });
        a.sample();
        a
    }

    #[test]
    fn every_visible_panel_and_every_sensor() {
        let a = demo_app();
        let text = render(&a, &chrono::Local::now());
        for s in ["== Summary ==", "== Sensors ==", "== CPU ==", "== Memory ==", "== Processes =="] {
            assert!(text.contains(s), "missing {s}:\n{text}");
        }
        for s in &a.sensors {
            assert!(text.contains(&s.label), "missing sensor {}", s.label);
        }
        assert!(text.contains("RAM "), "RAM line in the summary");
    }

    #[test]
    fn hidden_panels_and_compact_mode_are_left_out() {
        let mut a = demo_app();
        a.show[P_PROC] = false;
        let text = render(&a, &chrono::Local::now());
        assert!(!text.contains("== Processes =="));
        a.compact = true;
        let text = render(&a, &chrono::Local::now());
        assert!(text.contains("== Sensors ==") && !text.contains("== CPU =="));
    }

    #[test]
    fn never_overwrites() {
        let dir = std::env::temp_dir().join(format!("oryn-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = write_in(&dir, "20261017-164503", "one").unwrap();
        let b = write_in(&dir, "20261017-164503", "two").unwrap();
        assert_eq!(a.file_name().unwrap(), "oryn-20261017-164503.log");
        assert_eq!(b.file_name().unwrap(), "oryn-20261017-164503-2.log");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
