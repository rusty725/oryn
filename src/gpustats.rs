//! GPU load for the GPU panel: usage, VRAM, power, fan, clocks.
//!
//! * NVIDIA: from the `nvidia-smi` stream the sensor source already runs
//!   (see gpu.rs), so no extra process is started.
//! * AMD: straight from sysfs (`/sys/class/drm/cardN/device`): gpu_busy_percent,
//!   mem_info_vram_*, and the card's hwmon for power, fan, clocks, temperature.
//! * Intel: clock and awake time from the `xe` / `i915` sysfs files (see `intel`).

use std::collections::{HashMap, VecDeque};

use crate::sysstats::HIST;

#[derive(Clone, Default)]
pub struct GpuView {
    pub name: String,
    pub util: Option<f32>,
    pub vram_used: Option<f64>,
    pub vram_total: Option<f64>,
    pub temp: Option<f32>,
    /// GPU hotspot / junction temperature.
    pub hotspot: Option<f32>,
    /// Memory (VRAM) temperature: the hottest module when there are several.
    pub vram_temp: Option<f32>,
    /// The card's own VRAM temperature limit, if it reports one.
    pub vram_crit: Option<f32>,
    /// Individual VRAM chip temperatures, labelled ("A0", "B1"…), when known.
    pub vram_chips: Vec<(String, f32)>,
    /// True for NVIDIA cards, where VRAM temperature needs an extra module.
    pub nvidia: bool,
    /// Why hotspot is missing ("needs root"…), when known.
    pub hotspot_note: Option<&'static str>,
    /// An integrated GPU with no sensor of its own: `temp` is filled in with
    /// the CPU package temperature (same chip) once the sensors are read.
    pub on_cpu_die: bool,
    pub power_w: Option<f32>,
    pub power_limit_w: Option<f32>,
    pub fan_pct: Option<f32>,
    pub core_mhz: Option<f32>,
    pub mem_mhz: Option<f32>,
    /// Video BIOS version, where the driver publishes it in sysfs (AMD).
    /// NVIDIA's comes from nvidia-smi when the specs panel is filled in.
    pub vbios: Option<String>,
    pub hist: VecDeque<f32>,
}

#[derive(Default)]
pub struct GpuStats {
    pub gpus: Vec<GpuView>,
    hist: HashMap<String, VecDeque<f32>>,
    /// Extra NVIDIA sensors (VRAM / hotspot) published through hwmon by a
    /// kernel module such as gddr7-temp; rescanned now and then in case the
    /// module is loaded while Oryn runs.
    #[cfg(target_os = "linux")]
    nv_extra: Vec<nv_hwmon::Extra>,
    #[cfg(target_os = "linux")]
    nv_scan_in: u32,
    /// NvAPI, loaded on first use when an NVIDIA GPU is present.
    #[cfg(target_os = "linux")]
    nvapi: Option<Result<crate::nvapi::NvApi, String>>,
    /// NVIDIA's registers through BAR0 (FreeBSD has no NvAPI), mapped on
    /// first use.
    #[cfg(target_os = "freebsd")]
    nv_regs: Option<crate::freebsd::NvRegs>,
    #[cfg(target_os = "linux")]
    amd: Option<Vec<amd::Card>>,
    #[cfg(target_os = "linux")]
    intel: Option<Vec<intel::Card>>,
}

impl GpuStats {
    pub fn refresh(&mut self) {
        let mut list: Vec<(String, GpuView)> = Vec::new();

        if let Some(shared) = crate::gpu::SHARED.get() {
            for (i, g) in shared.lock().unwrap().iter().enumerate() {
                list.push((
                    format!("nv:{}", if g.bus.is_empty() { i.to_string() } else { g.bus.clone() }),
                    GpuView {
                        name: g.name.clone(),
                        util: g.util,
                        vram_used: g.vram_used_mib.map(|m| m as f64 * 1024.0 * 1024.0),
                        vram_total: g.vram_total_mib.map(|m| m as f64 * 1024.0 * 1024.0),
                        temp: g.core,
                        // Data-center cards with HBM report it via nvidia-smi.
                        vram_temp: g.memory,
                        vram_crit: g.mem_max,
                        hotspot: None,
                        nvidia: true,
                        hotspot_note: None,
                        on_cpu_die: false,
                        vram_chips: Vec::new(),
                        power_w: g.power_w,
                        power_limit_w: g.power_limit_w,
                        fan_pct: g.fan_pct,
                        core_mhz: g.core_mhz,
                        mem_mhz: g.mem_mhz,
                        vbios: None,
                        hist: VecDeque::new(),
                    },
                ));
            }
        }

        #[cfg(target_os = "linux")]
        {
            if self.nv_scan_in == 0 {
                self.nv_extra = nv_hwmon::find();
                self.nv_scan_in = 30;
            }
            self.nv_scan_in -= 1;
            let nv_count = list.len();
            if nv_count > 0 && self.nvapi.is_none() && !crate::nvapi::disabled() {
                self.nvapi = Some(crate::nvapi::NvApi::load());
            }
            for (key, g) in list.iter_mut() {
                let bus = key.trim_start_matches("nv:");
                let (vram, hot) = nv_hwmon::read(&self.nv_extra, bus, nv_count == 1);
                if g.vram_temp.is_none() {
                    g.vram_temp = vram;
                }
                g.hotspot = hot;
                // NvAPI (NVIDIA's own library) for whatever is still missing.
                if let Some(Ok(api)) = &mut self.nvapi
                    && let Some(busno) = crate::nvapi::bus_number(bus) {
                        let id = pci_device_id(bus).unwrap_or(0);
                        let t = api.read(busno, crate::nvapi::is_blackwell(id), crate::nvapi::has_gddr_regs(id));
                        if g.hotspot.is_none() {
                            g.hotspot = t.hotspot;
                            g.hotspot_note = t.hotspot_note;
                        }
                        if g.vram_temp.is_none() {
                            g.vram_temp = t.vram;
                        }
                        if g.vram_chips.is_empty() {
                            g.vram_chips = t.vram_chips;
                        }
                    }
            }
            let cards = self.amd.get_or_insert_with(amd::find);
            for c in cards.iter() {
                list.push((format!("amd:{}", c.id), c.read()));
            }
            let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
            let cards = self.intel.get_or_insert_with(|| intel::find(std::path::Path::new(&root)));
            for c in cards.iter_mut() {
                list.push((format!("intel:{}", c.id), c.read()));
            }
        }

        #[cfg(target_os = "freebsd")]
        if !list.is_empty() && !crate::nvapi::disabled() {
            let regs = self.nv_regs.get_or_insert_with(crate::freebsd::NvRegs::open);
            for (key, g) in list.iter_mut() {
                let Some(bus) = crate::nvapi::bus_number(key.trim_start_matches("nv:")) else { continue };
                match regs.read(bus) {
                    Some(t) => {
                        g.hotspot = t.hotspot;
                        g.hotspot_note = t.hotspot_note;
                        if g.vram_temp.is_none() {
                            g.vram_temp = t.vram;
                        }
                        g.vram_chips = t.vram_chips;
                    }
                    None => g.hotspot_note = regs.note,
                }
            }
        }

        // Keep a usage history per GPU across refreshes.
        for (key, g) in &mut list {
            let h = self.hist.entry(key.clone()).or_default();
            if let Some(u) = g.util {
                if h.len() == HIST {
                    h.pop_front();
                }
                h.push_back(u);
            }
            g.hist = h.clone();
        }
        self.gpus = list.into_iter().map(|(_, g)| g).collect();
    }
}

/// A PCI address the way sysfs writes it: nvidia-smi's "00000000:01:00.0"
/// has an 8-digit domain, sysfs's "0000:01:00.0" a 4-digit one.
#[cfg(target_os = "linux")]
fn sysfs_pci_addr(bus: &str) -> String {
    let b = bus.to_lowercase();
    match b.split_once(':') {
        Some((dom, rest)) if dom.len() > 4 => format!("{}:{rest}", &dom[dom.len() - 4..]),
        _ => b,
    }
}

/// PCI device ID of the GPU at `bus` ("00000000:01:00.0" or "0000:01:00.0").
#[cfg(target_os = "linux")]
fn pci_device_id(bus: &str) -> Option<u16> {
    let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
    let p = std::path::Path::new(&root).join("sys/bus/pci/devices").join(sysfs_pci_addr(bus)).join("device");
    let v = std::fs::read_to_string(p).ok()?;
    u16::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()
}

#[cfg(target_os = "linux")]
mod amd {
    use super::GpuView;
    use std::fs;
    use std::path::{Path, PathBuf};

    pub struct Card {
        pub id: String,
        name: String,
        dev: PathBuf,
        hwmon: Option<PathBuf>,
        vbios: Option<String>,
    }

    fn read(p: &Path) -> Option<f64> {
        fs::read_to_string(p).ok()?.trim().parse::<f64>().ok()
    }

    /// AMD GPUs: DRM cards whose PCI vendor is 0x1002 and that report load.
    pub fn find() -> Vec<Card> {
        let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
        let root = Path::new(&root);
        let mut cards: Vec<Card> = fs::read_dir(root.join("sys/class/drm"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                // "card0", not "card0-DP-1" (connectors).
                let n = name.strip_prefix("card")?;
                if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                let dev = e.path().join("device");
                let vendor = fs::read_to_string(dev.join("vendor")).ok()?;
                if vendor.trim() != "0x1002" || !dev.join("gpu_busy_percent").exists() {
                    return None;
                }
                let hwmon = fs::read_dir(dev.join("hwmon"))
                    .ok()
                    .and_then(|mut d| d.next())
                    .and_then(|e| e.ok())
                    .map(|e| e.path());
                let display = hwmon
                    .as_ref()
                    .map(|h| crate::devnames::Namer::new(root).hwmon("amdgpu", h).0)
                    .unwrap_or_else(|| "AMD GPU".into());
                let vbios = fs::read_to_string(dev.join("vbios_version")).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
                Some(Card { id: name, name: display, dev, hwmon, vbios })
            })
            .collect();
        cards.sort_by(|a, b| a.id.cmp(&b.id));
        cards
    }

    impl Card {
        pub fn read(&self) -> GpuView {
            let h = |f: &str| self.hwmon.as_ref().and_then(|h| read(&h.join(f)));
            // amdgpu labels its channels: temp1 "edge", temp2 "junction", temp3 "mem".
            let find_label = |want: &[&str]| -> Option<u32> {
                let hw = self.hwmon.as_ref()?;
                (1..=8).find(|n| {
                    fs::read_to_string(hw.join(format!("temp{n}_label")))
                        .map(|l| want.contains(&l.trim().to_ascii_lowercase().as_str()))
                        .unwrap_or(false)
                })
            };
            let by_label = |want: &[&str]| find_label(want).and_then(|n| h(&format!("temp{n}_input")));
            let crit_by_label = |want: &[&str]| find_label(want).and_then(|n| h(&format!("temp{n}_crit")));
            // Fan: percent from PWM (0–255) when available.
            let fan = h("pwm1").map(|p| (p / 255.0 * 100.0) as f32);
            GpuView {
                name: self.name.clone(),
                util: read(&self.dev.join("gpu_busy_percent")).map(|v| v as f32),
                vram_used: read(&self.dev.join("mem_info_vram_used")),
                vram_total: read(&self.dev.join("mem_info_vram_total")),
                temp: by_label(&["edge"]).or_else(|| h("temp1_input")).map(|m| (m / 1000.0) as f32),
                hotspot: by_label(&["junction", "hotspot"]).map(|m| (m / 1000.0) as f32),
                vram_temp: by_label(&["mem", "memory", "vram"]).map(|m| (m / 1000.0) as f32),
                vram_crit: crit_by_label(&["mem", "memory", "vram"]).map(|m| (m / 1000.0) as f32),
                nvidia: false,
                hotspot_note: None,
                on_cpu_die: false,
                vram_chips: Vec::new(),
                // Microwatts. Newer kernels have power1_input, older power1_average.
                power_w: h("power1_input").or_else(|| h("power1_average")).map(|u| (u / 1e6) as f32),
                power_limit_w: h("power1_cap").map(|u| (u / 1e6) as f32),
                fan_pct: fan,
                core_mhz: h("freq1_input").map(|hz| (hz / 1e6) as f32),
                mem_mhz: h("freq2_input").map(|hz| (hz / 1e6) as f32),
                vbios: self.vbios.clone(),
                hist: Default::default(),
            }
        }
    }
}

/// Intel GPUs (`xe` or `i915` driver): name, clock and how busy it is.
///
/// Neither driver reports a usage percentage in sysfs; the exact figure needs
/// root (the perf PMU, as intel_gpu_top uses). What any user can read is how
/// long the GPU has spent powered down in its idle state (RC6 / C6), so usage
/// is shown as the share of time it was awake. Integrated GPUs have no
/// temperature sensor or VRAM of their own: they get the CPU package
/// temperature (`on_cpu_die`), and VRAM stays n/a.
#[cfg(target_os = "linux")]
mod intel {
    use super::GpuView;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    pub struct Card {
        pub id: String,
        name: String,
        /// Current clock in MHz.
        freq: PathBuf,
        /// Milliseconds spent idle since boot.
        idle: PathBuf,
        /// No hwmon (discrete Arc cards have one): built into the CPU.
        integrated: bool,
        last: Option<(u64, Instant)>,
    }

    fn read(p: &Path) -> Option<u64> {
        fs::read_to_string(p).ok()?.trim().parse::<u64>().ok()
    }

    /// DRM cards whose PCI vendor is 0x8086 and that have a clock to read.
    pub fn find(root: &Path) -> Vec<Card> {
        let mut cards: Vec<Card> = fs::read_dir(root.join("sys/class/drm"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                // "card0", not "card0-eDP-1" (connectors).
                let n = name.strip_prefix("card")?;
                if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                let card = e.path();
                let dev = card.join("device");
                if fs::read_to_string(dev.join("vendor")).ok()?.trim() != "0x8086" {
                    return None;
                }
                // xe: device/tile0/gt0/{freq0,gtidle}/. i915: gt_act_freq_mhz
                // next to the card, RC6 residency under gt/gt0 (older: power/).
                let gt = dev.join("tile0/gt0");
                let (freq, idle) = if gt.join("freq0/act_freq").exists() {
                    (gt.join("freq0/act_freq"), gt.join("gtidle/idle_residency_ms"))
                } else if card.join("gt_act_freq_mhz").exists() {
                    let rc6 = card.join("gt/gt0/rc6_residency_ms");
                    (card.join("gt_act_freq_mhz"), if rc6.exists() { rc6 } else { card.join("power/rc6_residency_ms") })
                } else {
                    return None;
                };
                let display = crate::devnames::Namer::new(root).pci_name(&dev).unwrap_or_else(|| "Intel GPU".into());
                let integrated = !dev.join("hwmon").exists();
                Some(Card { id: name, name: display, freq, idle, integrated, last: None })
            })
            .collect();
        cards.sort_by(|a, b| a.id.cmp(&b.id));
        cards
    }

    impl Card {
        pub fn read(&mut self) -> GpuView {
            self.read_at(Instant::now())
        }

        /// `read` as if it were `now`, so tests control the time between reads.
        fn read_at(&mut self, now: Instant) -> GpuView {
            let mut util = None;
            if let Some(idle) = read(&self.idle) {
                if let Some((prev, at)) = self.last {
                    let dt = now.duration_since(at).as_secs_f64() * 1000.0;
                    // A counter that went backwards (wrapped) gives no reading this time.
                    if dt > 50.0 && idle >= prev {
                        util = Some((100.0 * (1.0 - (idle - prev) as f64 / dt)).clamp(0.0, 100.0) as f32);
                    }
                }
                self.last = Some((idle, now));
            }
            GpuView {
                name: self.name.clone(),
                util,
                // The clock reads 0 while the GPU sleeps.
                core_mhz: read(&self.freq).map(|m| m as f32),
                on_cpu_die: self.integrated,
                ..Default::default()
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::Duration;

        fn card(root: &Path, n: u32, vendor: &str) -> PathBuf {
            let dev = root.join(format!("pci{n}"));
            fs::create_dir_all(&dev).unwrap();
            fs::write(dev.join("vendor"), format!("{vendor}\n")).unwrap();
            let card = root.join(format!("sys/class/drm/card{n}"));
            fs::create_dir_all(&card).unwrap();
            std::os::unix::fs::symlink(&dev, card.join("device")).unwrap();
            dev
        }

        #[test]
        fn xe_and_i915_clock_and_busy_time() {
            let tmp = std::env::temp_dir().join(format!("oryn-intel-{}", std::process::id()));
            let _ = fs::remove_dir_all(&tmp);
            // xe (Raptor Lake on a recent kernel).
            let xe = card(&tmp, 0, "0x8086").join("tile0/gt0");
            fs::create_dir_all(xe.join("freq0")).unwrap();
            fs::create_dir_all(xe.join("gtidle")).unwrap();
            fs::write(xe.join("freq0/act_freq"), "1250\n").unwrap();
            fs::write(xe.join("gtidle/idle_residency_ms"), "10000\n").unwrap();
            // i915, RC6 counter in the older place.
            card(&tmp, 1, "0x8086");
            let c1 = tmp.join("sys/class/drm/card1");
            fs::create_dir_all(c1.join("power")).unwrap();
            fs::write(c1.join("gt_act_freq_mhz"), "300\n").unwrap();
            fs::write(c1.join("power/rc6_residency_ms"), "500\n").unwrap();
            // Not Intel, and an Intel card without clock files: both skipped.
            card(&tmp, 2, "0x1002");
            card(&tmp, 3, "0x8086");
            fs::create_dir_all(tmp.join("sys/class/drm/card0-eDP-1")).unwrap();

            let mut cards = find(&tmp);
            assert_eq!(cards.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["card0", "card1"]);
            let t0 = Instant::now();
            let g = cards[0].read_at(t0);
            assert_eq!((g.util, g.core_mhz), (None, Some(1250.0)), "first read is only the baseline");
            assert_eq!(cards[1].read_at(t0).core_mhz, Some(300.0));
            assert!(g.on_cpu_die, "no hwmon: integrated");

            // 2 s later: idle for 1.5 s of them = awake 25 % of the time.
            fs::write(xe.join("gtidle/idle_residency_ms"), "11500\n").unwrap();
            fs::write(c1.join("power/rc6_residency_ms"), "2500\n").unwrap();
            let t1 = t0 + Duration::from_secs(2);
            assert_eq!(cards[0].read_at(t1).util, Some(25.0));
            assert_eq!(cards[1].read_at(t1).util, Some(0.0));

            // A counter that went backwards gives no figure rather than nonsense.
            fs::write(xe.join("gtidle/idle_residency_ms"), "5\n").unwrap();
            assert_eq!(cards[0].read_at(t1 + Duration::from_secs(1)).util, None);
            let _ = fs::remove_dir_all(&tmp);
        }
    }
}

/// NVIDIA VRAM / hotspot temperatures from hwmon.
///
/// NVIDIA's driver doesn't publish these for GeForce cards. Community kernel
/// modules such as gddr7-temp (RTX 5090, RTX 30/40) read them and export them
/// as hwmon devices ("vramhotspot", "vrammod0"…, "thermhotspot"). We pick up
/// any hwmon sensor that belongs to an NVIDIA PCI device, or whose name/label
/// says vram/hotspot, and attach it to the matching GPU by PCI address.
#[cfg(target_os = "linux")]
mod nv_hwmon {
    use std::fs;
    use std::path::{Path, PathBuf};

    #[derive(Clone, PartialEq, Debug)]
    pub enum Kind {
        /// The module's own "hottest VRAM" sensor — preferred when present.
        VramHot,
        VramModule,
        Hotspot,
    }

    #[derive(Clone, Debug)]
    pub struct Extra {
        /// "0000:01:00.0", or None if the device isn't linked to a PCI card.
        pub pci: Option<String>,
        pub input: PathBuf,
        pub kind: Kind,
    }

    fn trim(p: &Path) -> Option<String> {
        fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// The NVIDIA PCI device a hwmon belongs to (the hwmon's device or a parent).
    fn nvidia_pci(hwmon: &Path) -> Option<String> {
        let dev = fs::canonicalize(hwmon.join("device")).ok()?;
        dev.ancestors().take(5).find_map(|d| {
            let vendor = trim(&d.join("vendor"))?;
            if vendor != "0x10de" {
                return None;
            }
            Some(d.file_name()?.to_string_lossy().to_lowercase())
        })
    }

    pub fn classify(name: &str, label: &str) -> Option<Kind> {
        let n = name.to_ascii_lowercase();
        let l = label.to_ascii_lowercase();
        let both = format!("{n} {l}");
        if n == "vramhotspot" || (both.contains("vram") && both.contains("hotspot")) {
            Some(Kind::VramHot)
        } else if both.contains("vram") || l == "mem" || l == "memory" {
            Some(Kind::VramModule)
        } else if both.contains("hotspot") || both.contains("junction") {
            Some(Kind::Hotspot)
        } else {
            None
        }
    }

    pub fn find() -> Vec<Extra> {
        let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
        let mut out = Vec::new();
        for e in fs::read_dir(Path::new(&root).join("sys/class/hwmon")).into_iter().flatten().flatten() {
            let dir = e.path();
            let name = trim(&dir.join("name")).unwrap_or_default();
            if name == "amdgpu" {
                continue; // AMD cards are read separately.
            }
            let pci = nvidia_pci(&dir);
            let looks_gpu = name.to_ascii_lowercase().contains("vram") || name.to_ascii_lowercase().starts_with("therm");
            if pci.is_none() && !looks_gpu {
                continue;
            }
            for f in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let file = f.file_name().to_string_lossy().to_string();
                let Some(n) = file.strip_prefix("temp").and_then(|r| r.strip_suffix("_input")) else { continue };
                let label = trim(&dir.join(format!("temp{n}_label"))).unwrap_or_default();
                if let Some(kind) = classify(&name, &label) {
                    out.push(Extra { pci: pci.clone(), input: f.path(), kind });
                }
            }
        }
        out
    }

    /// (vram, hotspot) for the GPU at `bus` ("00000000:01:00.0" from
    /// nvidia-smi). Sensors without a PCI link are used only when there is a
    /// single NVIDIA GPU, so they can't be attached to the wrong card.
    pub fn read(extras: &[Extra], bus: &str, only_gpu: bool) -> (Option<f32>, Option<f32>) {
        let want = super::sysfs_pci_addr(bus);
        let mine = extras.iter().filter(|e| match &e.pci {
            Some(p) => *p == want,
            None => only_gpu,
        });
        let val = |e: &Extra| -> Option<f32> {
            let v = trim(&e.input)?.parse::<f64>().ok()? / 1000.0;
            (v > 0.0 && v < 150.0).then_some(v as f32)
        };
        let (mut hot_vram, mut max_module, mut hotspot) = (None, None::<f32>, None::<f32>);
        for e in mine {
            let Some(v) = val(e) else { continue };
            match e.kind {
                Kind::VramHot => hot_vram = Some(v),
                Kind::VramModule => max_module = Some(max_module.map_or(v, |m| m.max(v))),
                Kind::Hotspot => hotspot = Some(hotspot.map_or(v, |m| m.max(v))),
            }
        }
        (hot_vram.or(max_module), hotspot)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn classifies_module_sensors() {
            assert_eq!(classify("vramhotspot", "hotspot"), Some(Kind::VramHot));
            assert_eq!(classify("vrammod0", "vrammod0"), Some(Kind::VramModule));
            assert_eq!(classify("thermhotspot", "hotspot_max"), Some(Kind::Hotspot));
            assert_eq!(classify("thermch0", "therm_ch0"), None);
            assert_eq!(classify("nvidia", "mem"), Some(Kind::VramModule));
        }
    }
}

/// `oryn --gpu-info`: explains, per NVIDIA GPU, whether VRAM / hotspot
/// temperatures can be shown and what's missing.
#[cfg(target_os = "linux")]
pub fn gpu_info_report() -> String {
    use std::fmt::Write;
    use std::fs;
    use std::path::Path;

    /// gddr7-temp's supported cards: (PCI device id, name, VRAM, hotspot).
    /// From the project's README; RTX 50 support beyond these two is not listed.
    const GDDR7_TEMP: &[(u16, &str, bool, bool)] = &[
        (0x2b85, "RTX 5090", true, true),
        (0x2c05, "RTX 5070 Ti", false, true),
        (0x2684, "RTX 4090", true, true),
        (0x2685, "RTX 4090 D", true, true),
        (0x2702, "RTX 4080 Super", true, true),
        (0x2704, "RTX 4080", true, true),
        (0x2705, "RTX 4070 Ti Super", true, true),
        (0x2782, "RTX 4070 Ti", true, true),
        (0x2783, "RTX 4070 Super", true, true),
        (0x2786, "RTX 4070", true, true),
        (0x2860, "RTX 4070 Mobile", true, true),
        (0x28e0, "RTX 4060 Mobile", true, true),
        (0x2203, "RTX 3090 Ti", true, true),
        (0x2204, "RTX 3090", true, true),
        (0x2208, "RTX 3080 Ti", true, true),
        (0x2206, "RTX 3080", true, true),
        (0x2216, "RTX 3080 LHR", true, true),
        (0x2484, "RTX 3070", true, true),
        (0x2488, "RTX 3070 LHR", true, true),
    ];

    let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
    let root = Path::new(&root);
    let read = |p: &Path| fs::read_to_string(p).ok().map(|s| s.trim().to_string());
    let mut out = String::new();
    let module_loaded = root.join("sys/module/gddr7_temp").exists();
    let extras = nv_hwmon::find();
    let mut namer = crate::devnames::Namer::new(root);
    let mut nvapi = (!crate::nvapi::disabled()).then(crate::nvapi::NvApi::load);
    let is_root = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).map(|l| l.split_whitespace().nth(1) == Some("0")))
        .unwrap_or(false);

    let mut found = 0;
    let mut devs: Vec<_> = fs::read_dir(root.join("sys/bus/pci/devices"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    devs.sort();
    for dev in devs {
        if read(&dev.join("vendor")).as_deref() != Some("0x10de")
            || !read(&dev.join("class")).is_some_and(|c| c.starts_with("0x03"))
        {
            continue;
        }
        found += 1;
        let addr = dev.file_name().unwrap().to_string_lossy().to_string();
        let id = read(&dev.join("device"))
            .and_then(|d| u16::from_str_radix(d.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        let name = namer.pci_name(&dev).unwrap_or_else(|| "NVIDIA GPU".into());
        let _ = writeln!(out, "{name}  (PCI {addr}, device id 0x{id:04x})");

        let (mut vram, mut hot) = nv_hwmon::read(&extras, &addr, true);
        // NvAPI, NVIDIA's own library (same method as LACT).
        match &mut nvapi {
            Some(Ok(api)) => {
                let busno = crate::nvapi::bus_number(&addr).unwrap_or(0);
                let t = api.read(busno, crate::nvapi::is_blackwell(id), crate::nvapi::has_gddr_regs(id));
                let _ = writeln!(out, "  NvAPI (libnvidia-api.so.1): hotspot {}, VRAM {}{}",
                    t.hotspot.map(|v| format!("{v:.0}°C")).unwrap_or_else(|| "none".into()),
                    t.vram.map(|v| format!("{v:.0}°C")).unwrap_or_else(|| "none".into()),
                    if t.vram_chips.is_empty() { String::new() } else {
                        format!(" (chips: {})", t.vram_chips.iter().map(|(l, c)| format!("{l} {c:.0}")).collect::<Vec<_>>().join(", "))
                    });
                if let Some(e) = &t.error {
                    // "NvAPI error -5": add the driver's own words for it.
                    let detail = e
                        .strip_prefix("NvAPI error ")
                        .and_then(|c| c.trim().parse::<i32>().ok())
                        .map(|c| api.error_message(c))
                        .unwrap_or_else(|| e.clone());
                    let _ = writeln!(out, "  NvAPI error: {detail}");
                    if !is_root {
                        let _ = writeln!(out, "  (not running as root: try `sudo oryn --gpu-info`)");
                    }
                }
                hot = hot.or(t.hotspot);
                vram = vram.or(t.vram);
            }
            Some(Err(e)) => {
                let _ = writeln!(out, "  NvAPI: {e}");
            }
            None => {
                let _ = writeln!(out, "  NvAPI: disabled (--no-nvapi)");
            }
        }
        let show = |v: Option<f32>| v.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "not available".into());
        let _ = writeln!(out, "  VRAM temperature:    {}", show(vram));
        let _ = writeln!(out, "  Hotspot temperature: {}", show(hot));
        if vram.is_some() && hot.is_some() {
            let _ = writeln!(out, "  OK: both are shown in the GPU panel.");
            continue;
        }
        let _ = writeln!(out, "  Neither NvAPI nor a kernel module provided both readings.");
        let _ = writeln!(out, "  NVIDIA's driver does not publish these for GeForce cards (and RTX 50");
        let _ = writeln!(out, "  cards no longer report hotspot at all), so Oryn needs the community");
        let _ = writeln!(out, "  kernel module gddr7-temp: https://github.com/sunnyyangyangyang/gddr7-temp");
        match GDDR7_TEMP.iter().find(|(d, ..)| *d == id) {
            Some((_, model, v, h)) => {
                let what = match (v, h) {
                    (true, true) => "VRAM and hotspot",
                    (false, true) => "hotspot only (no VRAM sensor)",
                    _ => "VRAM only",
                };
                let _ = writeln!(out, "  gddr7-temp supports this card ({model}): {what}.");
                if module_loaded {
                    let _ = writeln!(out, "  The module is loaded but no matching sensors were found; check `sensors`.");
                } else {
                    let _ = writeln!(out, "  The module is not loaded. Fedora: sudo dnf copr enable sunnyyang/gddr7-temp");
                    let _ = writeln!(out, "  && sudo dnf install gddr7_temp. Other distros: build it from source; it is");
                    let _ = writeln!(out, "  written in Rust, so your kernel must be built with Rust support (CONFIG_RUST).");
                }
            }
            None => {
                let _ = writeln!(out, "  gddr7-temp does not list this card (device id 0x{id:04x}), so VRAM and hotspot");
                let _ = writeln!(out, "  temperatures can't currently be read on Linux. Oryn shows them as \"—\".");
            }
        }
    }
    if found == 0 {
        out.push_str("No NVIDIA GPU found. (AMD cards show VRAM and hotspot temperatures without extra setup.)\n");
    }
    out
}
