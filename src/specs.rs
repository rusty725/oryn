//! System specs for the specs panel, gathered once at startup (uptime is
//! refreshed live). Everything is best-effort: lines whose source isn't
//! available on this machine are simply left out.

use std::fs;
use std::path::{Path, PathBuf};

/// One line of the panel: (section, category tag for the label color, text).
pub struct Line {
    pub label: &'static str,
    /// Category tag ("CPU", "GPU", "RAM", "NVME"…) so the label uses the same
    /// color as that category in the sensor table; "" for neutral.
    pub kind: &'static str,
    pub value: String,
}

pub struct Specs {
    pub lines: Vec<Line>,
}

fn root() -> PathBuf {
    PathBuf::from(crate::env_os("SYSFS").unwrap_or_else(|| "/".into()))
}

fn read(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != "To Be Filled By O.E.M." && s != "Default string")
}

fn human_size(bytes: f64) -> String {
    // Drives are sold in decimal units; show them that way ("4 TB", "512 GB").
    let (v, u) = if bytes >= 1e12 { (bytes / 1e12, "TB") } else { (bytes / 1e9, "GB") };
    if v >= 100.0 || (v - v.round()).abs() < 0.05 { format!("{v:.0} {u}") } else { format!("{v:.1} {u}") }
}

impl Specs {
    /// `gpus`: (name, VRAM bytes) from the GPU panel; `dimms`: RAM stick names
    /// from the sensors; `cpu_name` from sysinfo.
    pub fn collect(cpu_name: &str, threads: usize, mem_total: u64, gpus: &[(String, Option<f64>)], dimms: &[String]) -> Self {
        use sysinfo::System;
        let r = root();
        let mut lines = Vec::new();
        let mut add = |label: &'static str, kind: &'static str, value: String| {
            if !value.trim().is_empty() {
                lines.push(Line { label, kind, value });
            }
        };

        // OS / host.
        let os = System::long_os_version().or_else(System::name).unwrap_or_default();
        add("OS", "", format!("{os} {}", System::cpu_arch()).trim().to_string());
        add("Kernel", "", System::kernel_version().unwrap_or_default());
        add("Host", "", System::host_name().unwrap_or_default());
        add("Uptime", "", "live".into()); // value replaced live by the panel

        // Motherboard / BIOS (readable without root).
        let dmi = r.join("sys/class/dmi/id");
        let board = [read(&dmi.join("board_vendor")), read(&dmi.join("board_name"))]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        add("Board", "MB", board);
        let bios = [read(&dmi.join("bios_vendor")), read(&dmi.join("bios_version")), read(&dmi.join("bios_date"))]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        add("BIOS", "MB", bios);

        // CPU.
        let cores = System::physical_core_count();
        let max_mhz = read(&r.join("sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq"))
            .and_then(|k| k.parse::<f64>().ok())
            .map(|k| k / 1000.0);
        let mut cpu = cpu_name.to_string();
        if let Some(c) = cores {
            cpu.push_str(&format!(" · {c} cores / {threads} threads"));
        } else {
            cpu.push_str(&format!(" · {threads} threads"));
        }
        if let Some(m) = max_mhz {
            cpu.push_str(&format!(" · up to {:.2} GHz", m / 1000.0));
        }
        add("CPU", "CPU", cpu);

        // GPU(s).
        let driver = nvidia_driver();
        for (name, vram) in gpus {
            let mut v = name.clone();
            if let Some(b) = vram {
                v.push_str(&format!(" · {:.0} GB VRAM", b / 1024f64.powi(3)));
            }
            if name.contains("NVIDIA")
                && let Some(d) = &driver {
                    v.push_str(&format!(" · driver {d}"));
                }
            add("GPU", "GPU", v);
        }

        // RAM: total, then each stick (identical sticks grouped: "2 × …").
        let mut ram = format!("{:.0} GB", mem_total as f64 / 1024f64.powi(3));
        // Sticks whose SPD couldn't be read (no root) only have a bus address;
        // count those instead of listing addresses.
        let unnamed = dimms.iter().filter(|d| d.starts_with("RAM DIMM")).count();
        let named: Vec<&String> = dimms.iter().filter(|d| !d.starts_with("RAM DIMM")).collect();
        if unnamed > 0 && named.is_empty() {
            ram.push_str(&format!(" · {unnamed} DIMM{}", if unnamed > 1 { "s" } else { "" }));
        }
        if !named.is_empty() {
            let mut groups: Vec<(String, usize)> = Vec::new();
            for d in named {
                match groups.iter_mut().find(|g| &g.0 == d) {
                    Some(g) => g.1 += 1,
                    None => groups.push((d.clone(), 1)),
                }
            }
            let sticks: Vec<String> = groups
                .into_iter()
                .map(|(n, c)| if c > 1 { format!("{c} × {n}") } else { n })
                .collect();
            ram.push_str(&format!(" · {}", sticks.join(", ")));
        }
        add("RAM", "RAM", ram);

        // Drives: model, size, type.
        let mut drives: Vec<(String, f64, &'static str)> = fs::read_dir(r.join("sys/block"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                if ["loop", "ram", "zram", "dm-", "md", "sr", "fd"].iter().any(|p| name.starts_with(p)) {
                    return None;
                }
                let dir = e.path();
                let sectors = read(&dir.join("size"))?.parse::<f64>().ok()?;
                if sectors == 0.0 {
                    return None;
                }
                let model = read(&dir.join("device/model")).unwrap_or_else(|| name.clone());
                let vendor = read(&dir.join("device/vendor")).filter(|v| v != "ATA" && !model.starts_with(v.as_str()));
                let model = match vendor {
                    Some(v) => format!("{v} {model}"),
                    None => model,
                };
                let kind = if name.starts_with("nvme") {
                    "NVME"
                } else if read(&dir.join("queue/rotational")).as_deref() == Some("1") {
                    "HDD"
                } else {
                    "SSD"
                };
                Some((model, sectors * 512.0, kind))
            })
            .collect();
        drives.sort_by(|a, b| a.2.cmp(b.2).then(a.0.cmp(&b.0)));
        for (model, bytes, kind) in drives {
            add("Disk", kind, format!("{model} · {} {}", human_size(bytes), kind_word(kind)));
        }

        // Network adapters that are up, with IPv4 and link speed.
        let mut nets: Vec<String> = fs::read_dir(r.join("sys/class/net"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                if name == "lo" || !e.path().join("device").exists() {
                    return None; // virtual interfaces (docker, veth, bridges)
                }
                let speed = read(&e.path().join("speed")).and_then(|s| s.parse::<i64>().ok()).filter(|s| *s > 0);
                let state = read(&e.path().join("operstate")).unwrap_or_default();
                let mut v = name.clone();
                if let Some(s) = speed {
                    v.push_str(&if s >= 1000 { format!(" · {} Gb/s", s as f64 / 1000.0) } else { format!(" · {s} Mb/s") });
                }
                if state != "up" {
                    v.push_str(&format!(" · {state}"));
                }
                Some(v)
            })
            .collect();
        nets.sort();
        for n in nets {
            add("Net", "NET", n);
        }

        // Session.
        let env = |k: &str| std::env::var(k).unwrap_or_default();
        add("Desktop", "", desktop(&r));
        add("Shell", "", env("SHELL").rsplit('/').next().unwrap_or("").to_string());
        add("Terminal", "", std::env::var("TERM_PROGRAM").unwrap_or_else(|_| env("TERM")));

        Specs { lines }
    }
}

fn kind_word(kind: &str) -> &'static str {
    match kind {
        "NVME" => "NVMe",
        "HDD" => "HDD",
        _ => "SSD",
    }
}

// ---------------------------------------------------------------------------
// Desktop environment and its version
// ---------------------------------------------------------------------------

/// A desktop environment or compositor: how XDG_CURRENT_DESKTOP names it, its
/// display name, the processes that show it's running, and the command that
/// prints its version.
struct Desktop {
    keys: &'static [&'static str],
    name: &'static str,
    procs: &'static [&'static str],
    version: Option<(&'static str, &'static str)>,
}

const DESKTOPS: &[Desktop] = &[
    Desktop { keys: &["kde", "plasma"], name: "KDE Plasma", procs: &["plasmashell"], version: Some(("plasmashell", "--version")) },
    Desktop { keys: &["gnome"], name: "GNOME", procs: &["gnome-shell"], version: Some(("gnome-shell", "--version")) },
    Desktop { keys: &["cinnamon"], name: "Cinnamon", procs: &["cinnamon"], version: Some(("cinnamon", "--version")) },
    Desktop { keys: &["mate"], name: "MATE", procs: &["mate-session"], version: Some(("mate-session", "--version")) },
    Desktop { keys: &["xfce"], name: "Xfce", procs: &["xfce4-session"], version: Some(("xfce4-session", "--version")) },
    Desktop { keys: &["lxqt"], name: "LXQt", procs: &["lxqt-session"], version: Some(("lxqt-session", "--version")) },
    Desktop { keys: &["lxde"], name: "LXDE", procs: &["lxsession"], version: None },
    Desktop { keys: &["budgie"], name: "Budgie", procs: &["budgie-panel", "budgie-wm"], version: Some(("budgie-desktop", "--version")) },
    Desktop { keys: &["cosmic"], name: "COSMIC", procs: &["cosmic-session", "cosmic-comp"], version: None },
    Desktop { keys: &["pantheon"], name: "Pantheon", procs: &["gala"], version: None },
    Desktop { keys: &["deepin", "dde"], name: "Deepin", procs: &["dde-session", "startdde"], version: None },
    Desktop { keys: &["enlightenment"], name: "Enlightenment", procs: &["enlightenment"], version: None },
    Desktop { keys: &["hyprland"], name: "Hyprland", procs: &["Hyprland"], version: Some(("Hyprland", "--version")) },
    Desktop { keys: &["sway"], name: "Sway", procs: &["sway"], version: Some(("sway", "--version")) },
    Desktop { keys: &["niri"], name: "niri", procs: &["niri"], version: Some(("niri", "--version")) },
    Desktop { keys: &["river"], name: "river", procs: &["river"], version: Some(("river", "-version")) },
    Desktop { keys: &["labwc"], name: "labwc", procs: &["labwc"], version: Some(("labwc", "--version")) },
    Desktop { keys: &["wayfire"], name: "Wayfire", procs: &["wayfire"], version: Some(("wayfire", "--version")) },
    Desktop { keys: &["i3"], name: "i3", procs: &["i3"], version: Some(("i3", "--version")) },
    Desktop { keys: &["bspwm"], name: "bspwm", procs: &["bspwm"], version: Some(("bspwm", "-v")) },
    Desktop { keys: &["awesome"], name: "awesome", procs: &["awesome"], version: Some(("awesome", "--version")) },
    Desktop { keys: &["openbox"], name: "Openbox", procs: &["openbox"], version: Some(("openbox", "--version")) },
];

/// "KDE Plasma 6.2.4 · Wayland", "GNOME 47.1 · X11", "Hyprland 0.45.2 · Wayland".
///
/// The desktop comes from XDG_CURRENT_DESKTOP, or, when that isn't set (under
/// sudo, which clears it, or over SSH), from the desktop's own process
/// running on this machine; the session type likewise from
/// XDG_SESSION_TYPE or that process's environment. The version is what the
/// desktop's own program prints for --version.
fn desktop(root: &Path) -> String {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut session = env("XDG_SESSION_TYPE");
    let mut current = env("XDG_CURRENT_DESKTOP").or_else(|| env("XDG_SESSION_DESKTOP")).or_else(|| env("DESKTOP_SESSION"));
    let running = running_desktop(root);
    if (current.is_none() || session.is_none())
        && let Some((_, pid)) = &running
    {
        let penv = process_env(root, *pid);
        let get = |k: &str| penv.iter().find_map(|(key, v)| (key == k && !v.is_empty()).then(|| v.clone()));
        current = current.or_else(|| get("XDG_CURRENT_DESKTOP"));
        session = session.or_else(|| get("XDG_SESSION_TYPE"));
    }
    let known = current.as_deref().and_then(match_desktop).or(running.map(|(d, _)| d));
    let mut out = match known {
        Some(d) => {
            let version = d.version.and_then(|(cmd, arg)| command_version(cmd, arg)).or_else(|| {
                // Plasma also says its major version in the environment.
                (d.name == "KDE Plasma").then(|| env("KDE_SESSION_VERSION")).flatten()
            });
            match version {
                Some(v) => format!("{} {v}", d.name),
                None => d.name.to_string(),
            }
        }
        None => current.unwrap_or_default(),
    };
    if let Some(s) = session {
        let s = match s.to_ascii_lowercase().as_str() {
            "wayland" => "Wayland".to_string(),
            "x11" => "X11".to_string(),
            "tty" => "TTY".to_string(),
            other => other.to_string(),
        };
        if out.is_empty() {
            out = s;
        } else {
            out = format!("{out} · {s}");
        }
    }
    out
}

/// The desktop XDG_CURRENT_DESKTOP names. It can be a list, most specific
/// first ("Budgie:GNOME", "ubuntu:GNOME"), and some names carry an "X-"
/// prefix ("X-Cinnamon").
fn match_desktop(current: &str) -> Option<&'static Desktop> {
    current.to_ascii_lowercase().split([':', ';']).find_map(|part| {
        let part = part.trim().trim_start_matches("x-");
        DESKTOPS.iter().find(|d| d.keys.iter().any(|k| part.starts_with(k)))
    })
}

/// A known desktop whose process is running here, and that process's PID.
fn running_desktop(root: &Path) -> Option<(&'static Desktop, u32)> {
    let mut found: Vec<(usize, u32)> = fs::read_dir(root.join("proc"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.parse::<u32>().ok()?;
            let comm = fs::read_to_string(e.path().join("comm")).ok()?;
            let comm = comm.trim();
            let i = DESKTOPS.iter().position(|d| d.procs.contains(&comm))?;
            Some((i, pid))
        })
        .collect();
    // Full desktops before bare window managers, then the oldest process.
    found.sort();
    found.first().map(|&(i, pid)| (&DESKTOPS[i], pid))
}

/// A process's environment variables (readable for your own processes, or
/// any as root).
fn process_env(root: &Path, pid: u32) -> Vec<(String, String)> {
    fs::read(root.join(format!("proc/{pid}/environ")))
        .unwrap_or_default()
        .split(|&b| b == 0)
        .filter_map(|kv| {
            let kv = String::from_utf8_lossy(kv);
            let (k, v) = kv.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// Run `cmd arg` (e.g. "gnome-shell --version") and pick the version number
/// out of what it prints. Gives up after a second.
fn command_version(cmd: &str, arg: &str) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new(cmd)
        .arg(arg)
        // Qt programs (plasmashell, lxqt-session) want a display before they
        // even print their version; this lets them run without one (sudo, SSH).
        .env("QT_QPA_PLATFORM", "offscreen")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(1) => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    // Some print it on stderr (river, bspwm on some builds).
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    parse_version(&text)
}

/// The first version-looking word: "GNOME Shell 47.1" -> "47.1",
/// "xfce4-session 4.18.4 (Xfce 4.18)" -> "4.18.4", "awesome v4.3" -> "4.3".
fn parse_version(text: &str) -> Option<String> {
    text.lines().take(3).flat_map(str::split_whitespace).find_map(|w| {
        let w = w.trim_start_matches(['v', 'V']).trim_end_matches([',', ')', ';']);
        let mut parts = w.split('.');
        let ok = w.contains('.')
            && w.chars().next().is_some_and(|c| c.is_ascii_digit())
            && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '+'));
        ok.then(|| w.to_string())
    })
}

/// NVIDIA driver version, if nvidia-smi is installed.
fn nvidia_driver() -> Option<String> {
    let exe = crate::env_str("NVIDIA_SMI").unwrap_or_else(|| "nvidia-smi".into());
    let out = std::process::Command::new(exe)
        .args(["--query-gpu=driver_version", "--format=csv,noheader"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.lines().next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && !s.contains(' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_versions() {
        assert_eq!(parse_version("GNOME Shell 47.1\n").as_deref(), Some("47.1"));
        assert_eq!(parse_version("plasmashell 6.2.4").as_deref(), Some("6.2.4"));
        assert_eq!(parse_version("xfce4-session 4.18.4 (Xfce 4.18)\n\nCopyright (c) 2003-2024").as_deref(), Some("4.18.4"));
        assert_eq!(parse_version("Hyprland 0.45.2 built from branch  at commit 12f9a0d").as_deref(), Some("0.45.2"));
        assert_eq!(parse_version("sway version 1.10").as_deref(), Some("1.10"));
        assert_eq!(parse_version("awesome v4.3 (Too long)").as_deref(), Some("4.3"));
        assert_eq!(parse_version("niri 25.05.1 (commit 1234abc)").as_deref(), Some("25.05.1"));
        assert_eq!(parse_version("i3 version 4.23 (2023-10-29) © 2009 Michael Stapelberg").as_deref(), Some("4.23"));
        assert_eq!(parse_version("no version here"), None);
    }

    #[test]
    fn desktop_names() {
        let name = |c: &str| match_desktop(c).map(|d| d.name);
        assert_eq!(name("KDE"), Some("KDE Plasma"));
        assert_eq!(name("ubuntu:GNOME"), Some("GNOME"));
        assert_eq!(name("Budgie:GNOME"), Some("Budgie"));
        assert_eq!(name("X-Cinnamon"), Some("Cinnamon"));
        assert_eq!(name("XFCE"), Some("Xfce"));
        assert_eq!(name("Hyprland"), Some("Hyprland"));
        assert_eq!(name("Unity"), None);
    }

    #[test]
    fn desktop_found_from_its_process_when_env_is_cleared() {
        // As under sudo: no XDG variables; plasmashell is running with them set.
        let tmp = std::env::temp_dir().join(format!("oryn-desk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        for (pid, comm, env) in [(100, "bash", ""), (2345, "plasmashell", "XDG_SESSION_TYPE=wayland\0XDG_CURRENT_DESKTOP=KDE\0")] {
            let d = tmp.join(format!("proc/{pid}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
            fs::write(d.join("environ"), env).unwrap();
        }
        let (d, pid) = running_desktop(&tmp).unwrap();
        assert_eq!((d.name, pid), ("KDE Plasma", 2345));
        let env = process_env(&tmp, pid);
        assert!(env.contains(&("XDG_SESSION_TYPE".into(), "wayland".into())));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn drive_sizes_in_decimal_units() {
        assert_eq!(human_size(4.0e12), "4 TB");
        assert_eq!(human_size(512.11e9), "512 GB");
        assert_eq!(human_size(240.06e9), "240 GB");
        assert_eq!(human_size(2.5e12), "2.5 TB");
    }
}
