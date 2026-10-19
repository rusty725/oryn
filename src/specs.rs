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

/// A GPU as the GPU panel knows it.
pub struct GpuSpec {
    pub name: String,
    pub vram: Option<f64>,
    pub nvidia: bool,
    /// Video BIOS version, when the driver reports it in sysfs (AMD).
    pub vbios: Option<String>,
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
    /// `gpus` from the GPU panel; `dimms`: RAM stick names from the sensors;
    /// `cpu_name` from sysinfo.
    /// `ram_speed`: "DDR5 · 6000 MT/s · XMP 6400 MT/s", from SMBIOS and the
    /// sticks' SPD.
    pub fn collect(
        cpu_name: &str,
        threads: usize,
        mem_total: u64,
        gpus: &[GpuSpec],
        dimms: &[String],
        ram_speed: Option<&str>,
    ) -> Self {
        use sysinfo::System;
        let r = root();
        let mut lines = Vec::new();
        let mut add = |label: &'static str, kind: &'static str, value: String| {
            if !value.trim().is_empty() {
                lines.push(Line { label, kind, value });
            }
        };

        // OS / host.
        // On FreeBSD sysinfo's long version is all of kern.version (build
        // path, newline) and its kernel version is __FreeBSD_version.
        #[cfg(target_os = "freebsd")]
        let (os, kernel) = {
            use crate::freebsd::sysctl_str;
            let release = sysctl_str("kern.osrelease").unwrap_or_default();
            let os = format!("{} {release}", sysctl_str("kern.ostype").unwrap_or_else(|| "FreeBSD".into()));
            let kernel = [Some(release), sysctl_str("kern.ident")].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            (os, kernel)
        };
        #[cfg(not(target_os = "freebsd"))]
        let (os, kernel) = (
            System::long_os_version().or_else(System::name).unwrap_or_default(),
            System::kernel_version().unwrap_or_default(),
        );
        add("OS", "", format!("{os} {}", System::cpu_arch()).trim().to_string());
        add("Kernel", "", kernel);
        add("Host", "", System::host_name().unwrap_or_default());
        add("Uptime", "", "live".into()); // value replaced live by the panel

        // Motherboard / BIOS (readable without root).
        // FreeBSD: the loader puts the same SMBIOS strings in kenv.
        #[cfg(target_os = "freebsd")]
        let dmi_get = |linux: &str, kenv: &str| {
            let _ = linux;
            crate::freebsd::kenv(&format!("smbios.{kenv}"))
        };
        #[cfg(not(target_os = "freebsd"))]
        let dmi = r.join("sys/class/dmi/id");
        #[cfg(not(target_os = "freebsd"))]
        let dmi_get = |linux: &str, kenv: &str| {
            let _ = kenv;
            read(&dmi.join(linux))
        };
        let board = [dmi_get("board_vendor", "planar.maker"), dmi_get("board_name", "planar.product")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        add("Board", "MB", board);
        let bios = [
            dmi_get("bios_vendor", "bios.vendor"),
            dmi_get("bios_version", "bios.version"),
            dmi_get("bios_date", "bios.reldate"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        add("BIOS", "MB", bios);

        // CPU.
        #[cfg(target_os = "freebsd")]
        let (cores, max_mhz) =
            (crate::freebsd::physical_cores(), crate::freebsd::max_mhz().map(|m| m as f64));
        #[cfg(not(target_os = "freebsd"))]
        let (cores, max_mhz) = (
            System::physical_core_count(),
            read(&r.join("sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq"))
                .and_then(|k| k.parse::<f64>().ok())
                .map(|k| k / 1000.0),
        );
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

        // GPU(s). nvidia-smi lists NVIDIA cards in the same order as the panel.
        let nv = if gpus.iter().any(|g| g.nvidia) { nvidia_info() } else { Vec::new() };
        let mut nv_iter = nv.iter();
        for g in gpus {
            let mut v = g.name.clone();
            if let Some(b) = g.vram {
                v.push_str(&format!(" · {:.0} GB VRAM", b / 1024f64.powi(3)));
            }
            let mut vbios = g.vbios.clone();
            if g.nvidia
                && let Some((driver, nv_vbios)) = nv_iter.next() {
                    v.push_str(&format!(" · driver {driver}"));
                    vbios = vbios.or_else(|| nv_vbios.clone());
                }
            if let Some(b) = vbios {
                v.push_str(&format!(" · VBIOS {b}"));
            }
            add("GPU", "GPU", v);
        }
        for m in monitors(&r) {
            add("Monitor", "GPU", m);
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
        add("Speed", "RAM", ram_speed.unwrap_or_default().to_string());

        // Drives: model, size, type.
        let mut drives: Vec<(String, f64, &'static str, Option<String>)> = fs::read_dir(r.join("sys/block"))
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
                // NVMe: the controller's firmware revision. SATA: the full one
                // from the drive's ATA identify data. Else (USB, SAS) `rev`,
                // which the kernel cuts to its last 4 characters.
                let firmware = read(&dir.join("device/firmware_rev"))
                    .or_else(|| ata_firmware(&dir.join("device")))
                    .or_else(|| read(&dir.join("device/rev")));
                Some((model, sectors * 512.0, kind, firmware))
            })
            .collect();
        #[cfg(target_os = "freebsd")]
        drives.extend(crate::freebsd::disks().into_iter().map(|g| {
            let kind = match (g.name.starts_with("nda") || g.name.starts_with("nvd"), g.rotational) {
                (true, _) => "NVME",
                (_, Some(true)) => "HDD",
                _ => "SSD",
            };
            let firmware = g.model.as_deref().and_then(|m| crate::freebsd::disk_firmware(&g.name, m));
            (g.model.unwrap_or(g.name), g.bytes as f64, kind, firmware)
        }));
        drives.sort_by(|a, b| a.2.cmp(b.2).then(a.0.cmp(&b.0)));
        for (model, bytes, kind, firmware) in drives {
            let fw = firmware.map(|f| format!(" · firmware {f}")).unwrap_or_default();
            add("Disk", kind, format!("{model} · {} {}{fw}", human_size(bytes), kind_word(kind)));
        }

        #[cfg(target_os = "freebsd")]
        let adapters = crate::freebsd::nics();
        #[cfg(not(target_os = "freebsd"))]
        let adapters = nets(&r);
        for n in adapters {
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

/// A display's current mode: (width, height, refresh in Hz).
type Mode = (u32, u32, f64);

/// Connected monitors, from each display connector's EDID (readable without
/// root) plus the mode it's showing now, from the DRM driver or, where X11
/// sets modes past it (the NVIDIA driver), from xrandr:
/// "DP-3 · LG ULTRAGEAR+ · 3840×2160 · 240 Hz (max 240 Hz) · 31.5″".
/// Without DRM in sysfs (FreeBSD), the monitors xrandr shows.
fn monitors(root: &Path) -> Vec<String> {
    let pnp = ["usr/share/hwdata/pnp.ids", "usr/share/misc/pnp.ids", "usr/local/share/hwdata/pnp.ids"]
        .iter()
        .find_map(|p| fs::read_to_string(root.join(p)).ok());
    // Run at most once, and only when DRM didn't know a monitor's mode.
    let mut xrandr: Option<Vec<(String, Vec<u8>, Mode)>> = None;
    let mut run_xrandr = || {
        if std::env::var_os("DISPLAY").is_none() {
            return Vec::new();
        }
        run_quick("xrandr", &["--current", "--prop"]).map(|t| parse_xrandr(&t)).unwrap_or_default()
    };
    let has_drm = fs::read_dir(root.join("sys/class/drm")).is_ok();
    let mut out: Vec<(String, String)> = fs::read_dir(root.join("sys/class/drm"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            if read(&dir.join("status")).as_deref() != Some("connected") {
                return None;
            }
            let edid = fs::read(dir.join("edid")).ok()?;
            // "card1-DP-3" -> card "card1", connector "DP-3".
            let file = e.file_name().to_string_lossy().to_string();
            let (card, name) = file.split_once('-')?;
            #[cfg(target_os = "linux")]
            let current = read(&dir.join("connector_id"))
                .and_then(|id| id.parse().ok())
                .and_then(|id| current_mode(&root.join("dev/dri").join(card), id));
            #[cfg(not(target_os = "linux"))]
            let current: Option<Mode> = { let _ = card; None };
            let current = current.or_else(|| {
                let x = xrandr.get_or_insert_with(&mut run_xrandr);
                x.iter().find(|(_, e, _)| edid.get(..128) == Some(e.as_slice())).map(|(_, _, m)| *m)
            });
            // Else the preferred mode, which the kernel lists first.
            let mode = current
                .map(|(w, h, _)| format!("{w}×{h}"))
                .or_else(|| read(&dir.join("modes")).and_then(|m| m.lines().next().map(|l| l.replace('x', "×"))));
            let desc = describe_edid(&edid, mode, current.map(|c| c.2), pnp.as_deref())?;
            Some((name.to_string(), format!("{name} · {desc}")))
        })
        .collect();
    if !has_drm {
        out = run_xrandr()
            .into_iter()
            .filter_map(|(name, edid, (w, h, hz))| {
                let desc = describe_edid(&edid, Some(format!("{w}×{h}")), Some(hz), pnp.as_deref())?;
                Some((name.clone(), format!("{name} · {desc}")))
            })
            .collect();
    }
    out.sort();
    out.into_iter().map(|(_, v)| v).collect()
}

/// "LG ULTRAGEAR+ · 3840×2160 · 144 Hz (max 240 Hz) · 31.5″" from an EDID
/// block (VESA E-EDID 1.4). `mode`: the current or preferred resolution,
/// else it's taken from the first detailed timing; `hz`: the current
/// refresh rate, if known.
fn describe_edid(b: &[u8], mode: Option<String>, hz: Option<f64>, pnp: Option<&str>) -> Option<String> {
    if b.len() < 128 || b[..8] != [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0] {
        return None;
    }
    // Maker: three 5-bit letters at bytes 8–9 ("GSM"), named from pnp.ids.
    let id = u16::from_be_bytes([b[8], b[9]]);
    let code: String = [10, 5, 0].iter().map(|s| (b'A' - 1 + ((id >> s) & 0x1f) as u8) as char).collect();
    let maker = match code.as_str() {
        "ACI" => "ASUS".to_string(), // registered as "Ancor Communications"
        _ => pnp
            .and_then(|db| db.lines().find_map(|l| l.strip_prefix(&format!("{code}\t"))))
            .and_then(|n| n.split_whitespace().next())
            .map(|n| n.trim_end_matches([',', '.']).to_string())
            .unwrap_or(code),
    };
    // Four 18-byte descriptors: a detailed timing (pixel clock ≠ 0) or a
    // tagged one: 0xFC monitor name, 0xFE text (laptop panels), 0xFD limits.
    let descriptors: Vec<&[u8]> = (0..4).map(|i| &b[54 + 18 * i..72 + 18 * i]).collect();
    let text = |tag: u8| {
        descriptors
            .iter()
            .filter(|d| d[..3] == [0, 0, 0] && d[3] == tag)
            .map(|d| String::from_utf8_lossy(&d[5..]).split('\n').next().unwrap_or("").trim().to_string())
            .rfind(|s| !s.is_empty())
    };
    let model = text(0xfc).or_else(|| text(0xfe)).unwrap_or_else(|| format!("{:04X}", u16::from_le_bytes([b[10], b[11]])));
    let mut parts = vec![if model.to_ascii_lowercase().contains(&maker.to_ascii_lowercase()) { model } else { format!("{maker} {model}") }];

    let timing = descriptors.iter().find(|d| d[0] != 0 || d[1] != 0);
    let mode = mode.or_else(|| {
        let d = timing?;
        let h = d[2] as u32 | ((d[4] as u32 >> 4) << 8);
        let v = d[5] as u32 | ((d[7] as u32 >> 4) << 8);
        Some(format!("{h}×{v}"))
    });
    parts.extend(mode);
    // Range limits: max vertical rate at byte 6, +255 when flag bit 1 is set.
    let max = descriptors
        .iter()
        .find(|d| d[..3] == [0, 0, 0] && d[3] == 0xfd)
        .map(|d| d[6] as u32 + if d[4] & 0x02 != 0 { 255 } else { 0 })
        .filter(|&m| m > 0);
    // 59.94 Hz stays exact; 239.99 is 240.
    let rate = |h: f64| if (h - h.round()).abs() < 0.05 { format!("{h:.0} Hz") } else { format!("{h:.2} Hz") };
    match (hz, max) {
        (Some(c), Some(m)) => parts.push(format!("{} (max {m} Hz)", rate(c))),
        (Some(c), None) => parts.push(rate(c)),
        (None, Some(m)) => parts.push(format!("max {m} Hz")),
        (None, None) => {}
    }
    // Size: the detailed timing's mm, else the basic cm at bytes 21–22.
    let mm = timing
        .map(|d| (d[12] as f64 + ((d[14] >> 4) as f64) * 256.0, d[13] as f64 + ((d[14] & 0x0f) as f64) * 256.0))
        .filter(|&(w, h)| w > 0.0 && h > 0.0)
        .or_else(|| (b[21] > 0 && b[22] > 0).then(|| (b[21] as f64 * 10.0, b[22] as f64 * 10.0)));
    if let Some((w, h)) = mm {
        let inches = (w * w + h * h).sqrt() / 25.4;
        parts.push(if (inches - inches.round()).abs() < 0.05 { format!("{:.0}″", inches) } else { format!("{inches:.1}″") });
    }
    Some(parts.join(" · "))
}

/// The mode each X11 output is showing, from `xrandr --current --prop`
/// (`--current` doesn't re-probe the displays): (output name, EDID base
/// block, (width, height, refresh)). The current rate is the one marked `*`
/// in the output's mode list ("3840x2160    240.08 + 144.05*   60.00").
fn parse_xrandr(text: &str) -> Vec<(String, Vec<u8>, Mode)> {
    let mut out = Vec::new();
    let (mut name, mut edid, mut mode) = (String::new(), String::new(), None);
    let mut in_edid = false;
    let mut flush = |name: &str, edid: &mut String, mode: &mut Option<Mode>| {
        let bytes: Vec<u8> = (0..edid.len() / 2).filter_map(|i| u8::from_str_radix(&edid[2 * i..2 * i + 2], 16).ok()).collect();
        if let (Some(m), Some(base)) = (mode.take(), bytes.get(..128)) {
            out.push((name.to_string(), base.to_vec(), m));
        }
        edid.clear();
    };
    for line in text.lines() {
        if !line.starts_with([' ', '\t']) {
            flush(&name, &mut edid, &mut mode); // next output (or "Screen 0:")
            name = line.split_whitespace().next().unwrap_or("").to_string();
            in_edid = false;
            continue;
        }
        let t = line.trim();
        if t == "EDID:" {
            in_edid = true;
        } else if in_edid && !t.is_empty() && t.chars().all(|c| c.is_ascii_hexdigit()) {
            edid.push_str(t);
        } else {
            in_edid = false;
            // A mode line: "WxH" then its rates.
            let mut words = t.split_whitespace();
            let Some((w, h)) = words.next().and_then(|r| r.split_once('x')) else { continue };
            let (Ok(w), Ok(h)) = (w.parse(), h.trim_end_matches('i').parse()) else { continue };
            if let Some(hz) = words.find(|r| r.contains('*')).and_then(|r| r.trim_end_matches(['*', '+']).parse().ok()) {
                mode = Some((w, h, hz));
            }
        }
    }
    flush(&name, &mut edid, &mut mode);
    out
}

/// The mode a display connector is showing now: (width, height, refresh
/// in Hz), from the DRM driver's read-only mode queries (connector ->
/// encoder -> CRTC) on `card` (/dev/dri/cardN, which the logged-in user can
/// open). None when the display is off or the device can't be opened.
#[cfg(target_os = "linux")]
fn current_mode(card: &Path, connector_id: u32) -> Option<Mode> {
    use std::os::fd::AsRawFd;

    // Structures from the kernel's include/uapi/drm/drm_mode.h.
    #[repr(C)]
    #[derive(Default)]
    struct GetConnector {
        encoders_ptr: u64,
        modes_ptr: u64,
        props_ptr: u64,
        prop_values_ptr: u64,
        count_modes: u32,
        count_props: u32,
        count_encoders: u32,
        encoder_id: u32,
        connector_id: u32,
        connector_type: u32,
        connector_type_id: u32,
        connection: u32,
        mm_width: u32,
        mm_height: u32,
        subpixel: u32,
        pad: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct GetEncoder {
        encoder_id: u32,
        encoder_type: u32,
        crtc_id: u32,
        possible_crtcs: u32,
        possible_clones: u32,
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct ModeInfo {
        clock: u32,
        hdisplay: u16,
        hsync_start: u16,
        hsync_end: u16,
        htotal: u16,
        hskew: u16,
        vdisplay: u16,
        vsync_start: u16,
        vsync_end: u16,
        vtotal: u16,
        vscan: u16,
        vrefresh: u32,
        flags: u32,
        kind: u32,
        name: [u8; 32],
    }
    #[repr(C)]
    #[derive(Default)]
    struct Crtc {
        set_connectors_ptr: u64,
        count_connectors: u32,
        crtc_id: u32,
        fb_id: u32,
        x: u32,
        y: u32,
        gamma_size: u32,
        mode_valid: u32,
        mode: ModeInfo,
    }
    const _: () = assert!(
        std::mem::size_of::<GetConnector>() == 80 && std::mem::size_of::<GetEncoder>() == 20 && std::mem::size_of::<Crtc>() == 104
    );
    // DRM_IOWR('d', nr, T).
    fn iowr<T>(nr: u64) -> u64 {
        (3 << 30) | ((std::mem::size_of::<T>() as u64) << 16) | ((b'd' as u64) << 8) | nr
    }
    fn ioctl<T>(fd: i32, nr: u64, arg: &mut T) -> Option<()> {
        // SAFETY: `arg` is the #[repr(C)] struct this request reads and writes.
        (unsafe { libc::ioctl(fd, iowr::<T>(nr) as _, arg as *mut T) } == 0).then_some(())
    }

    let file = fs::File::open(card).ok()?;
    let fd = file.as_raw_fd();
    // Room for one mode, so count_modes isn't 0: a 0 there asks the driver
    // to re-probe the display, which can make it flicker.
    let mut one = ModeInfo::default();
    let mut conn = GetConnector { connector_id, count_modes: 1, modes_ptr: &mut one as *mut ModeInfo as u64, ..Default::default() };
    ioctl(fd, 0xA7, &mut conn)?;
    let mut enc = GetEncoder { encoder_id: conn.encoder_id, ..Default::default() };
    if conn.encoder_id == 0 {
        return None;
    }
    ioctl(fd, 0xA6, &mut enc)?;
    let mut crtc = Crtc { crtc_id: enc.crtc_id, ..Default::default() };
    if enc.crtc_id == 0 {
        return None;
    }
    ioctl(fd, 0xA1, &mut crtc)?;
    let m = crtc.mode;
    if crtc.mode_valid == 0 || m.htotal == 0 || m.vtotal == 0 {
        return None;
    }
    // Pixel clock (kHz) over pixels per frame; interlaced modes show two
    // fields per frame, double-scanned ones draw each line twice.
    let mut hz = m.clock as f64 * 1000.0 / (m.htotal as f64 * m.vtotal as f64);
    if m.flags & (1 << 4) != 0 {
        hz *= 2.0;
    }
    if m.flags & (1 << 5) != 0 {
        hz /= 2.0;
    }
    if m.vscan > 1 {
        hz /= m.vscan as f64;
    }
    Some((m.hdisplay as u32, m.vdisplay as u32, hz))
}

/// Network adapters (not virtual ones), sorted: "enp8s0 · Realtek RTL8126
/// 5GbE Controller · r8169 driver · 5 Gb/s", plus the state when not up.
#[cfg(not(target_os = "freebsd"))]
fn nets(root: &Path) -> Vec<String> {
    #[cfg(target_os = "linux")]
    let mut namer = crate::devnames::Namer::new(root);
    let mut nets: Vec<String> = fs::read_dir(root.join("sys/class/net"))
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
            let dev = e.path().join("device");
            let driver = fs::read_link(dev.join("driver")).ok().and_then(|p| Some(p.file_name()?.to_string_lossy().to_string()));
            #[cfg(target_os = "linux")]
            if let Some(model) = nic_model(&mut namer, &dev, driver.as_deref()) {
                v.push_str(&format!(" · {model}"));
            }
            if let Some(d) = &driver {
                v.push_str(&format!(" · {d} driver"));
            }
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
    nets
}

/// A SATA drive's full firmware revision ("01.01A01"), from the ATA
/// identify data libata publishes, readable without root. `dev` is the
/// disk's SCSI device; the ATA port ("ataN") is above it. Used only when
/// the port has one drive (not behind a port multiplier), so it can't be
/// mixed up with another disk's.
fn ata_firmware(dev: &Path) -> Option<String> {
    let dev = fs::canonicalize(dev).ok()?;
    let port = dev.ancestors().find(|d| {
        d.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("ata")).is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    })?;
    let ls = |d: &Path, prefix: &str| -> Vec<PathBuf> {
        fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
            .map(|e| e.path())
            .collect()
    };
    let ids: Vec<PathBuf> = ls(port, "link")
        .iter()
        .flat_map(|l| ls(l, "dev"))
        .flat_map(|d| ls(&d.join("ata_device"), "dev"))
        .map(|d| d.join("id"))
        .filter(|f| f.is_file())
        .collect();
    match ids.as_slice() {
        [id] => identify_string(&fs::read_to_string(id).ok()?, 23..27),
        _ => None,
    }
}

/// ASCII text from ATA identify data (256 16-bit words written as hex,
/// e.g. "427a 3fff …"); each word holds two characters, high byte first.
fn identify_string(id: &str, words: std::ops::Range<usize>) -> Option<String> {
    let all: Vec<u16> = id.split_whitespace().filter_map(|w| u16::from_str_radix(w, 16).ok()).collect();
    let bytes: Vec<u8> = all.get(words)?.iter().flat_map(|w| w.to_be_bytes()).collect();
    let s = String::from_utf8_lossy(&bytes).trim().to_string();
    (!s.is_empty() && s.chars().all(|c| c.is_ascii_graphic() || c == ' ')).then_some(s)
}

/// The network adapter at `dev` (the interface's `device` link): the USB
/// product for USB adapters, else the PCI name from `pci.ids`.
#[cfg(target_os = "linux")]
fn nic_model(namer: &mut crate::devnames::Namer, dev: &Path, driver: Option<&str>) -> Option<String> {
    let subsystem = fs::read_link(dev.join("subsystem")).ok()?;
    if subsystem.file_name()? == "usb" {
        // `dev` is the USB interface; the device above it has the names.
        let usb = fs::canonicalize(dev).ok()?.parent()?.to_path_buf();
        let product = read(&usb.join("product"))?;
        return Some(match read(&usb.join("manufacturer")) {
            Some(m) if !product.to_ascii_lowercase().starts_with(&m.to_ascii_lowercase()) => format!("{m} {product}"),
            _ => product,
        });
    }
    let name = namer.pci_name(dev)?;
    // Without pci.ids the name ends in "(driver)", which is shown separately.
    Some(match driver {
        Some(d) => name.strip_suffix(&format!(" ({d})")).unwrap_or(&name).to_string(),
        None => name,
    })
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
    let mut found: Vec<(usize, u32)> = process_names(root)
        .into_iter()
        .filter_map(|(pid, comm)| {
            let i = DESKTOPS.iter().position(|d| d.procs.contains(&comm.as_str()))?;
            Some((i, pid))
        })
        .collect();
    // Full desktops before bare window managers, then the oldest process.
    found.sort();
    found.first().map(|&(i, pid)| (&DESKTOPS[i], pid))
}

/// (PID, command name) of every process. FreeBSD's procfs (when mounted at
/// all) has no `comm` or `environ`, so there they come from sysinfo.
fn process_names(root: &Path) -> Vec<(u32, String)> {
    #[cfg(target_os = "freebsd")]
    if root == Path::new("/") {
        use sysinfo::{ProcessRefreshKind, RefreshKind, System};
        let _guard = crate::sysstats::libc_buffers();
        let sys = System::new_with_specifics(RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()));
        return sys.processes().iter().map(|(pid, p)| (pid.as_u32(), p.name().to_string_lossy().into_owned())).collect();
    }
    fs::read_dir(root.join("proc"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.parse::<u32>().ok()?;
            let comm = fs::read_to_string(e.path().join("comm")).ok()?;
            Some((pid, comm.trim().to_string()))
        })
        .collect()
}

/// A process's environment variables (readable for your own processes, or
/// any as root).
fn process_env(root: &Path, pid: u32) -> Vec<(String, String)> {
    #[cfg(target_os = "freebsd")]
    if root == Path::new("/") {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let _guard = crate::sysstats::libc_buffers();
        let mut sys = System::new();
        let pid = Pid::from_u32(pid);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_environ(UpdateKind::Always),
        );
        return sys
            .process(pid)
            .map(|p| p.environ().iter().filter_map(|kv| split_env(kv.as_encoded_bytes())).collect())
            .unwrap_or_default();
    }
    fs::read(root.join(format!("proc/{pid}/environ")))
        .unwrap_or_default()
        .split(|&b| b == 0)
        .filter_map(split_env)
        .collect()
}

/// "KEY=value" -> (KEY, value).
fn split_env(kv: &[u8]) -> Option<(String, String)> {
    let kv = String::from_utf8_lossy(kv);
    let (k, v) = kv.split_once('=')?;
    Some((k.to_string(), v.to_string()))
}

/// Run `cmd arg` (e.g. "gnome-shell --version") and pick the version number
/// out of what it prints.
fn command_version(cmd: &str, arg: &str) -> Option<String> {
    parse_version(&run_quick(cmd, &[arg])?)
}

/// Run a command and return what it printed (stdout, then stderr). Gives
/// up after a second.
fn run_quick(cmd: &str, args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new(cmd)
        .args(args)
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
    // Some print their version on stderr (river, bspwm on some builds).
    Some(format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
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

/// (driver version, VBIOS version) per NVIDIA GPU, in nvidia-smi's order,
/// if nvidia-smi is installed.
fn nvidia_info() -> Vec<(String, Option<String>)> {
    let exe = crate::env_str("NVIDIA_SMI").unwrap_or_else(|| "nvidia-smi".into());
    let Ok(out) = std::process::Command::new(exe)
        .args(["--query-gpu=driver_version,vbios_version", "--format=csv,noheader"])
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    parse_nvidia_info(&String::from_utf8_lossy(&out.stdout))
}

fn parse_nvidia_info(text: &str) -> Vec<(String, Option<String>)> {
    // A field nvidia-smi can't read is "[N/A]" or similar: anything with a space or bracket.
    let ok = |s: &str| (!s.is_empty() && !s.contains([' ', '['])).then(|| s.to_string());
    text.lines()
        .filter_map(|l| {
            let mut f = l.split(',').map(str::trim);
            Some((ok(f.next()?)?, f.next().and_then(ok)))
        })
        .collect()
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

    #[cfg(target_os = "linux")]
    #[test]
    fn network_adapter_model_and_driver() {
        use std::os::unix::fs::symlink;
        let tmp = std::env::temp_dir().join(format!("oryn-nets-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let mk = |p: &Path, files: &[(&str, &str)]| {
            fs::create_dir_all(p).unwrap();
            for (f, v) in files {
                fs::write(p.join(f), format!("{v}\n")).unwrap();
            }
        };
        let bus = |b: &str, drv: &str| {
            let d = tmp.join(format!("sys/bus/{b}/drivers/{drv}"));
            fs::create_dir_all(&d).unwrap();
            (tmp.join(format!("sys/bus/{b}")), d)
        };
        let iface = |name: &str, dev: &Path, files: &[(&str, &str)]| {
            let n = tmp.join("sys/class/net").join(name);
            mk(&n, files);
            symlink(dev, n.join("device")).unwrap();
        };
        mk(&tmp.join("usr/share/hwdata"), &[("pci.ids", "10ec  Realtek Semiconductor Co., Ltd.\n\t8126  RTL8126 5GbE Controller")]);

        // PCI Ethernet, named from pci.ids.
        let eth = tmp.join("sys/devices/pci0000:00/0000:08:00.0");
        mk(&eth, &[("vendor", "0x10ec"), ("device", "0x8126"), ("class", "0x020000")]);
        let (pci, r8169) = bus("pci", "r8169");
        symlink(&pci, eth.join("subsystem")).unwrap();
        symlink(&r8169, eth.join("driver")).unwrap();
        iface("enp8s0", &eth, &[("speed", "5000"), ("operstate", "up")]);

        // Wi-Fi card that pci.ids doesn't list: no "(iwlwifi)" twice.
        let wifi = tmp.join("sys/devices/pci0000:00/0000:00:14.3");
        mk(&wifi, &[("vendor", "0x8086"), ("device", "0x51f1"), ("class", "0x028000")]);
        let (_, iwl) = bus("pci", "iwlwifi");
        symlink(&pci, wifi.join("subsystem")).unwrap();
        symlink(&iwl, wifi.join("driver")).unwrap();
        iface("wlan0", &wifi, &[("speed", "-1"), ("operstate", "up")]);

        // USB adapter: the interface's parent has the names.
        let usbdev = tmp.join("sys/devices/pci0000:00/0000:00:14.0/usb1/1-2");
        mk(&usbdev, &[("manufacturer", "Realtek"), ("product", "USB 10/100/1000 LAN")]);
        let intf = usbdev.join("1-2:1.0");
        mk(&intf, &[]);
        let (usb, r8152) = bus("usb", "r8152");
        symlink(&usb, intf.join("subsystem")).unwrap();
        symlink(&r8152, intf.join("driver")).unwrap();
        iface("enx00e04c", &intf, &[("operstate", "down")]);

        // Virtual: left out.
        mk(&tmp.join("sys/class/net/virbr0"), &[("operstate", "up")]);

        assert_eq!(
            nets(&tmp),
            [
                "enp8s0 · Realtek RTL8126 5GbE Controller · r8169 driver · 5 Gb/s",
                "enx00e04c · Realtek USB 10/100/1000 LAN · r8152 driver · down",
                "wlan0 · Intel Wi-Fi card · iwlwifi driver",
            ]
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn full_sata_firmware_from_identify_data() {
        // The start of a WD4006FZBX's identify data: firmware "01.01A01"
        // (words 23–26), where the kernel's `rev` says only "1A01".
        let id = "427a 3fff c837 0010 0000 0000 003f 0000\n\
                  0000 0000 2020 2020 2020 2020 2057 442d\n\
                  4153 3036 3330 3952 0000 0000 0000 3031\n\
                  2e30 3141 3031 5744 4320 5744 3430 3036\n";
        assert_eq!(identify_string(id, 23..27).as_deref(), Some("01.01A01"));
        assert_eq!(identify_string(id, 23..40), None, "past the end");
        assert_eq!(identify_string("0000 0000", 0..2), None, "no text");

        let tmp = std::env::temp_dir().join(format!("oryn-ata-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let port = tmp.join("pci/ata1");
        let scsi = port.join("host0/target0:0:0/0:0:0:0");
        fs::create_dir_all(&scsi).unwrap();
        let ata_dev = port.join("link1/dev1.0/ata_device/dev1.0");
        fs::create_dir_all(&ata_dev).unwrap();
        fs::write(ata_dev.join("id"), id).unwrap();
        assert_eq!(ata_firmware(&scsi).as_deref(), Some("01.01A01"));
        // A second drive on the port (port multiplier): don't guess.
        let other = port.join("link1.1/dev1.1.0/ata_device/dev1.1.0");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("id"), id).unwrap();
        assert_eq!(ata_firmware(&scsi), None);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn nvidia_driver_and_vbios() {
        assert_eq!(
            parse_nvidia_info("580.95.05, 98.03.3B.00.A8\n580.95.05, [N/A]\nNVIDIA-SMI has failed\n"),
            [("580.95.05".to_string(), Some("98.03.3B.00.A8".to_string())), ("580.95.05".to_string(), None)]
        );
    }

    #[test]
    fn monitor_from_edid() {
        // An LG 32GS95UE's EDID base block (serial numbers zeroed).
        let mut b = vec![0u8; 128];
        b[..8].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
        b[8..12].copy_from_slice(&[0x1e, 0x6d, 0x46, 0x78]); // "GSM", product 0x7846
        b[21] = 70;
        b[22] = 39;
        b[54..72].copy_from_slice(&[0x4d, 0xd0, 0x00, 0xa0, 0xf0, 0x70, 0x3e, 0x80, 0x30, 0x20, 0x35, 0x00, 0xb9, 0x88, 0x21, 0, 0, 0x1a]);
        b[72..80].copy_from_slice(&[0, 0, 0, 0xfd, 0x0c, 0x30, 0xf0, 0xff]);
        b[90..95].copy_from_slice(&[0, 0, 0, 0xfc, 0]);
        b[95..108].copy_from_slice(b"LG ULTRAGEAR+");
        let pnp = "GSL\tGeneral Standards\nGSM\tLG Electronics\n";
        assert_eq!(describe_edid(&b, None, None, Some(pnp)).as_deref(), Some("LG ULTRAGEAR+ · 3840×2160 · max 240 Hz · 31.5″"));
        assert_eq!(
            describe_edid(&b, Some("2560×1440".into()), Some(143.998), None).as_deref(),
            Some("GSM LG ULTRAGEAR+ · 2560×1440 · 144 Hz (max 240 Hz) · 31.5″")
        );
        // Laptop panel: no name or limits, only text; maker added in front.
        b[72..108].fill(0);
        b[90..95].copy_from_slice(&[0, 0, 0, 0xfe, 0]);
        b[95..108].copy_from_slice(b"LP140WF7-SPH1");
        assert_eq!(describe_edid(&b, None, None, Some(pnp)).as_deref(), Some("LG LP140WF7-SPH1 · 3840×2160 · 31.5″"));
        assert_eq!(describe_edid(&b, None, Some(59.94), Some(pnp)).as_deref(), Some("LG LP140WF7-SPH1 · 3840×2160 · 59.94 Hz · 31.5″"));
        assert_eq!(describe_edid(&b[..100], None, None, None), None, "truncated");
        b[0] = 1;
        assert_eq!(describe_edid(&b, None, None, None), None, "bad header");
    }

    #[test]
    fn current_mode_from_xrandr() {
        let edid = |id: u8| format!("00ffffffffffff00{id:02x}{}", "00".repeat(119));
        let text = format!(
            "Screen 0: minimum 8 x 8, current 6400 x 2160, maximum 32767 x 32767\n\
             HDMI-0 disconnected (normal left inverted right x axis y axis)\n\
             \tnon-desktop: 0 \n\
             DP-4 connected primary 3840x2160+0+0 (normal left inverted right x axis y axis) 700mm x 390mm\n\
             \tEDID: \n\t\t{}\n\t\t{}\n\
             \tnon-desktop: 0 \n\t\tsupported: 0, 1\n\
             \x20  3840x2160    240.08 + 144.05*   95.03    60.00  \n\
             \x20  1920x1080    143.98   119.88    60.00    59.94  \n\
             DP-5 connected 2560x1440+3840+0 (normal left inverted right x axis y axis) 600mm x 340mm\n\
             \tEDID: \n\t\t{}\n\
             \x20  2560x1440     59.95*+ 164.80  \n\
             DP-6 connected (normal left inverted right x axis y axis)\n\
             \tEDID: \n\t\t{}\n\
             \x20  1920x1080     60.00 +\n",
            &edid(1)[..128],
            &edid(1)[128..],
            edid(2),
            edid(3),
        );
        let modes = parse_xrandr(&text);
        assert_eq!(modes.len(), 2, "the switched-off DP-6 has no current mode");
        assert_eq!((modes[0].0.as_str(), modes[0].1[8], modes[0].2), ("DP-4", 1, (3840, 2160, 144.05)));
        assert_eq!((modes[1].0.as_str(), modes[1].1[8], modes[1].2), ("DP-5", 2, (2560, 1440, 59.95)));
    }

    #[test]
    fn drive_sizes_in_decimal_units() {
        assert_eq!(human_size(4.0e12), "4 TB");
        assert_eq!(human_size(512.11e9), "512 GB");
        assert_eq!(human_size(240.06e9), "240 GB");
        assert_eq!(human_size(2.5e12), "2.5 TB");
    }
}
