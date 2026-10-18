//! Sensor sources.
//!
//! On Linux we read sysfs directly instead of going through sysinfo, because
//! sysinfo identifies sensors by their display label: two chips with the same
//! name (e.g. two `spd5118` DDR5 DIMM sensors) collapse into one, and thermal
//! zones such as `x86_pkg_temp` are ignored whenever any hwmon device exists.
//! Here every sensor is keyed by its sysfs location, so nothing is merged.

pub struct Reading {
    /// Stable unique key (sysfs path based); never shown to the user.
    pub id: String,
    pub label: String,
    /// Human-readable hardware name, e.g. "AMD Ryzen 9 7950X" or "Samsung SSD 990 PRO 2TB".
    pub device: String,
    pub temp: Option<f32>,
    pub critical: Option<f32>,
    pub cpu: bool,
    /// Category tag shown in front of the device: "CPU", "RAM", "NVME", "HDD", "USB"…
    pub kind: &'static str,
    /// Shown instead of a temperature when there is none ("standby", "needs root"…).
    pub note: Option<String>,
}

pub trait Source {
    fn read(&mut self) -> Vec<Reading>;
    fn name(&self) -> &'static str;
    /// Block until slow background readings (SMART) have a first value. Used by --list.
    fn settle(&self) {}
}

/// A main source plus extra ones (SMART disks, NVIDIA GPUs) read alongside it.
pub struct Combined {
    pub base: Box<dyn Source>,
    pub extras: Vec<Box<dyn Source>>,
}

impl Source for Combined {
    fn read(&mut self) -> Vec<Reading> {
        let mut r = self.base.read();
        for e in &mut self.extras {
            r.extend(e.read());
        }
        r
    }
    fn settle(&self) {
        for e in &self.extras {
            e.settle();
        }
    }
    fn name(&self) -> &'static str {
        self.base.name()
    }
}

/// Chip / zone names that mean "this is the CPU".
#[cfg_attr(target_os = "freebsd", allow(dead_code))]
pub fn is_cpu_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "coretemp" | "k10temp" | "k8temp" | "zenpower" | "x86_pkg_temp" | "tcpu" | "b0d4"
            | "soc_thermal" | "cpu_thermal" | "cpu-thermal"
    ) || n.starts_with("cpu")
}

#[cfg_attr(target_os = "freebsd", allow(dead_code))]
fn clean_crit(c: Option<f32>) -> Option<f32> {
    c.filter(|c| c.is_finite() && *c > 0.0 && *c < 250.0)
}

// ---------------------------------------------------------------------------
// Linux: direct sysfs reader
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub mod linux {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    struct Channel {
        id: String,
        label: String,
        device: String,
        kind: &'static str,
        input: PathBuf,
        critical: Option<f32>,
        cpu: bool,
    }

    pub struct SysfsSource {
        channels: Vec<Channel>,
        /// Block devices whose temperature comes from `drivetemp`, e.g. "/dev/sda".
        disks: HashSet<String>,
        root: PathBuf,
        /// What the hwmon / thermal directories looked like at the last scan,
        /// and when it was checked. Sensors come and go (USB/NVMe drives
        /// plugged in, drivers loaded or reloaded, which also renumbers
        /// hwmonN), so the list is rebuilt when this changes.
        layout: Vec<String>,
        pub(crate) checked: std::time::Instant,
    }

    /// How often to look for added or removed sensor devices.
    const RESCAN: std::time::Duration = std::time::Duration::from_secs(5);
    #[cfg(test)]
    pub const RESCAN_FOR_TESTS: std::time::Duration = RESCAN;

    /// A cheap fingerprint of the sensor devices: every hwmonN with its chip
    /// name and device link, and every thermal zone.
    fn device_layout(root: &Path) -> Vec<String> {
        let mut out: Vec<String> = numbered_dirs(&root.join("sys/class/hwmon"), "hwmon")
            .into_iter()
            .map(|(_, d)| {
                let link = fs::read_link(d.join("device")).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                format!("{}|{}|{link}", d.display(), read_trim(&d.join("name")).unwrap_or_default())
            })
            .collect();
        out.extend(numbered_dirs(&root.join("sys/class/thermal"), "thermal_zone").into_iter().map(|(_, d)| d.display().to_string()));
        out
    }

    fn read_trim(p: &Path) -> Option<String> {
        fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// sysfs temperatures are in millidegrees Celsius.
    fn read_milli(p: &Path) -> Option<f32> {
        read_trim(p)?.parse::<i64>().ok().map(|v| v as f32 / 1000.0)
    }

    /// Directory entries `prefix<N>`, sorted numerically by N.
    fn numbered_dirs(dir: &Path, prefix: &str) -> Vec<(u32, PathBuf)> {
        let mut out: Vec<(u32, PathBuf)> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let n = name.strip_prefix(prefix)?.parse::<u32>().ok()?;
                Some((n, e.path()))
            })
            .collect();
        out.sort_by_key(|(n, _)| *n);
        out
    }

    impl SysfsSource {
        /// `root` is normally `/`; tests point it at a fake tree.
        pub fn new(root: &Path) -> Self {
            let mut channels = Vec::new();
            let mut namer = crate::devnames::Namer::new(root);
            let mut hwmon_names = HashSet::new();
            let mut disks = HashSet::new();

            // --- hwmon -----------------------------------------------------
            let hwmon_root = root.join("sys/class/hwmon");
            for (_, dir) in numbered_dirs(&hwmon_root, "hwmon") {
                let dir_name = dir.file_name().unwrap().to_string_lossy().to_string();
                let chip = read_trim(&dir.join("name")).unwrap_or_else(|| dir_name.clone());
                hwmon_names.insert(chip.to_ascii_lowercase());
                // e.g. "1-0051" for an i2c DIMM sensor, "0000:00:18.3" for k10temp.
                let device = fs::read_link(dir.join("device"))
                    .ok()
                    .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()));

                // Collect tempN_input files.
                let mut idx: Vec<u32> = fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| {
                        let f = e.file_name().into_string().ok()?;
                        f.strip_prefix("temp")?.strip_suffix("_input")?.parse().ok()
                    })
                    .collect();
                idx.sort_unstable();
                let single = idx.len() == 1;
                let (device_name, kind) = namer.hwmon(&chip, &dir);

                // drivetemp (SATA disks): name the row after the disk, e.g. "drivetemp (sda)".
                let mut disk_crit = None;
                let mut chip_label = chip.clone();
                if chip == "drivetemp" {
                    let block = fs::read_dir(dir.join("device/block"))
                        .into_iter()
                        .flatten()
                        .flatten()
                        .find_map(|e| e.file_name().into_string().ok());
                    if let Some(b) = block {
                        let rotational = read_trim(&root.join(format!("sys/block/{b}/queue/rotational")));
                        disk_crit = Some(if rotational.as_deref() == Some("0") { 70.0 } else { 60.0 });
                        disks.insert(format!("/dev/{b}"));
                        chip_label = format!("drivetemp ({b})");
                    } else {
                        disk_crit = Some(60.0);
                    }
                }

                // A stable key for this chip: its name plus the device it
                // belongs to ("spd5118@1-0050", "amdgpu@0000:03:00.0"), so
                // history survives the hwmonN number changing. Without a
                // device link, the directory name is all there is.
                let key = match &device {
                    Some(d) => format!("{chip}@{d}"),
                    None => dir_name.clone(),
                };
                for n in idx {
                    let chan_label = read_trim(&dir.join(format!("temp{n}_label")));
                    let label = match chan_label {
                        // "vrammod0" labelled "vrammod0": say it once.
                        Some(l) if l == chip => chip_label.clone(),
                        Some(l) => format!("{chip_label} {l}"),
                        None if single => chip_label.clone(),
                        None => format!("{chip_label} temp{n}"),
                    };
                    channels.push(Channel {
                        id: format!("{key}/temp{n}"),
                        label,
                        device: device_name.clone(),
                        kind,
                        input: dir.join(format!("temp{n}_input")),
                        critical: clean_crit(read_milli(&dir.join(format!("temp{n}_crit"))))
                            .or(disk_crit),
                        cpu: is_cpu_name(&chip),
                    });
                }
            }

            // --- thermal zones ---------------------------------------------
            // Added unless a hwmon chip with the same name already covers it
            // (e.g. `acpitz`), so CPU zones like `x86_pkg_temp` show up even
            // when DIMM/NVMe hwmon sensors exist.
            let thermal_root = root.join("sys/class/thermal");
            for (_, dir) in numbered_dirs(&thermal_root, "thermal_zone") {
                let Some(kind) = read_trim(&dir.join("type")) else { continue };
                if hwmon_names.contains(&kind.to_ascii_lowercase()) {
                    continue;
                }
                let input = dir.join("temp");
                if !input.exists() {
                    continue;
                }
                // The trip point whose type is "critical", if any.
                let critical = (0..16).find_map(|k| {
                    let t = read_trim(&dir.join(format!("trip_point_{k}_type")))?;
                    (t == "critical")
                        .then(|| read_milli(&dir.join(format!("trip_point_{k}_temp"))))
                        .flatten()
                });
                let dir_name = dir.file_name().unwrap().to_string_lossy().to_string();
                let (zone_name, zone_kind) = namer.zone(&kind);
                channels.push(Channel {
                    id: dir_name,
                    cpu: is_cpu_name(&kind),
                    device: zone_name,
                    kind: zone_kind,
                    label: kind,
                    input,
                    critical: clean_crit(critical),
                });
            }

            // --- make labels unique ----------------------------------------
            // Identical chips (two spd5118 DIMMs, two NVMe drives…) get their
            // bus address appended, falling back to #1, #2.
            let mut counts: HashMap<String, usize> = HashMap::new();
            for c in &channels {
                *counts.entry(c.label.clone()).or_default() += 1;
            }
            let mut seen: HashMap<String, usize> = HashMap::new();
            for c in &mut channels {
                if counts[&c.label] > 1 {
                    let k = seen.entry(c.label.clone()).or_default();
                    *k += 1;
                    // The device part of "chip@device/tempN", else a counter.
                    let suffix = c
                        .id
                        .split_once('@')
                        .and_then(|(_, rest)| rest.rsplit_once('/'))
                        .map(|(dev, _)| dev.to_string())
                        .unwrap_or_else(|| format!("#{k}"));
                    c.label = format!("{} ({suffix})", c.label);
                }
            }

            Self {
                channels,
                disks,
                root: root.to_path_buf(),
                layout: device_layout(root),
                checked: std::time::Instant::now(),
            }
        }

        pub fn is_empty(&self) -> bool {
            self.channels.is_empty()
        }

        pub fn drivetemp_disks(&self) -> &HashSet<String> {
            &self.disks
        }
    }

    impl Source for SysfsSource {
        fn read(&mut self) -> Vec<Reading> {
            if self.checked.elapsed() >= RESCAN {
                self.checked = std::time::Instant::now();
                if device_layout(&self.root) != self.layout {
                    *self = SysfsSource::new(&self.root.clone());
                }
            }
            self.channels
                .iter()
                .map(|c| Reading {
                    id: c.id.clone(),
                    label: c.label.clone(),
                    device: c.device.clone(),
                    kind: c.kind,
                    // Unreadable sensors (EIO/ENODATA) stay listed as n/a.
                    // Out-of-range values are "no reading" codes, e.g. -128 or
                    // 255 from unconnected motherboard sensor inputs.
                    temp: read_milli(&c.input).filter(|t| t.is_finite() && *t > -100.0 && *t < 200.0),
                    critical: c.critical,
                    cpu: c.cpu,
                    note: None,
                })
                .collect()
        }
        fn name(&self) -> &'static str {
            "sysfs"
        }
    }
}

// ---------------------------------------------------------------------------
// Other OSes: sysinfo
// ---------------------------------------------------------------------------

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
pub struct SysinfoSource {
    components: sysinfo::Components,
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
impl SysinfoSource {
    pub fn new() -> Self {
        Self { components: sysinfo::Components::new_with_refreshed_list() }
    }
    pub fn is_empty(&self) -> bool {
        self.components.list().is_empty()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
impl Source for SysinfoSource {
    fn read(&mut self) -> Vec<Reading> {
        self.components.refresh(true);
        self.components
            .list()
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let label = c.label().to_string();
                let l = label.to_ascii_lowercase();
                let cpu = l.contains("cpu")
                    || l.contains("package")
                    || l.contains("tctl")
                    || l.contains("tdie")
                    || l.split_whitespace().any(is_cpu_name);
                Reading {
                    id: c.id().map(str::to_string).unwrap_or_else(|| format!("{i}:{label}")),
                    device: if cpu { "CPU".into() } else { String::new() },
                    kind: if cpu { "CPU" } else { "" },
                    label,
                    temp: c.temperature().filter(|t| t.is_finite()),
                    critical: clean_crit(c.critical()),
                    cpu,
                    note: None,
                }
            })
            .collect()
    }
    fn name(&self) -> &'static str {
        "sysinfo"
    }
}

// ---------------------------------------------------------------------------
// Demo
// ---------------------------------------------------------------------------

/// Simulated sensors driven by sine waves + noise, for demo/testing.
pub struct DemoSource {
    tick: f64,
    seed: u64,
}

impl DemoSource {
    pub fn new() -> Self {
        Self { tick: 0.0, seed: 0x9E3779B97F4A7C15 }
    }
    fn noise(&mut self) -> f64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed % 1000) as f64 / 1000.0 - 0.5
    }
}

impl Source for DemoSource {
    fn read(&mut self) -> Vec<Reading> {
        self.tick += 1.0;
        let t = self.tick;
        let cpu = "AMD Ryzen 9 9950X3D";
        let specs: [(&str, &str, f64, f64, f64, Option<f32>, bool); 9] = [
            ("k10temp Tctl", cpu, 36.0, 3.0, 0.08, None, true),
            ("k10temp Tccd1", cpu, 27.0, 2.0, 0.11, None, true),
            ("k10temp Tccd2", cpu, 25.0, 2.0, 0.07, None, true),
            ("amdgpu edge", "AMD Radeon RX 7800 XT", 48.0, 30.0, 0.04, Some(95.0), false),
            ("nvme Composite", "Samsung SSD 990 PRO 2TB", 41.0, 8.0, 0.03, Some(84.0), false),
            ("spd5118 (1-0050)", "Kingston KF560C36BBE-32", 38.0, 3.0, 0.02, Some(85.0), false),
            ("spd5118 (1-0051)", "Kingston KF560C36BBE-32", 40.0, 3.0, 0.025, Some(85.0), false),
            ("nct6798", "Motherboard (nct6798)", 46.0, 4.0, 0.02, None, false),
            ("acpitz", "ACPI thermal zone", 32.0, 2.0, 0.01, None, false),
        ];
        let mut out: Vec<Reading> = specs
            .iter()
            .map(|&(label, device, base, amp, freq, crit, cpu)| {
                let v = base + amp * (t * freq).sin() + 3.0 * self.noise();
                let kind = match label.split([' ', '(']).next().unwrap_or("") {
                    _ if cpu => "CPU",
                    "amdgpu" => "GPU",
                    "nvme" => "NVME",
                    "spd5118" => "RAM",
                    "nct6798" => "MB",
                    "acpitz" => "ACPI",
                    _ => "",
                };
                Reading {
                    id: label.into(),
                    label: label.into(),
                    device: device.into(),
                    kind,
                    temp: Some(v as f32),
                    critical: crit,
                    cpu,
                    note: None,
                }
            })
            .collect();
        // Hard drives: one spinning, one asleep (shows its last value + "standby").
        let hdd = 36.0 + 1.5 * (t * 0.01).sin() + 0.4 * self.noise();
        out.push(Reading {
            id: "smart:/dev/sda".into(),
            label: "drivetemp (sda)".into(),
            device: "WDC WD40EFRX-68N32N0".into(),
            kind: "HDD",
            temp: Some(hdd as f32),
            critical: Some(60.0),
            cpu: false,
            note: None,
        });
        out.push(Reading {
            id: "smart:/dev/sdb".into(),
            label: "sdb (SMART)".into(),
            device: "ST8000VN004-2M2101".into(),
            kind: "HDD",
            temp: Some(33.0),
            critical: Some(60.0),
            cpu: false,
            note: Some("standby".into()),
        });
        out
    }
    fn name(&self) -> &'static str {
        "demo"
    }
}

// ---------------------------------------------------------------------------
// Tests against a fake sysfs tree
// ---------------------------------------------------------------------------

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux::SysfsSource;
    use super::Source;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    fn hwmon(root: &Path, n: u32, name: &str, dev: Option<&str>, temps: &[(u32, i64, Option<&str>)]) {
        let d = root.join(format!("sys/class/hwmon/hwmon{n}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("name"), format!("{name}\n")).unwrap();
        if let Some(dev) = dev {
            let target = root.join("devices").join(dev);
            fs::create_dir_all(&target).unwrap();
            symlink(&target, d.join("device")).unwrap();
        }
        for (i, v, label) in temps {
            fs::write(d.join(format!("temp{i}_input")), format!("{v}\n")).unwrap();
            fs::write(d.join(format!("temp{i}_crit")), "85000\n").unwrap();
            if let Some(l) = label {
                fs::write(d.join(format!("temp{i}_label")), format!("{l}\n")).unwrap();
            }
        }
    }

    #[test]
    fn two_spd5118_and_cpu() {
        let tmp = std::env::temp_dir().join(format!("oryn-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        hwmon(&tmp, 0, "spd5118", Some("1-0050"), &[(1, 38250, None)]);
        hwmon(&tmp, 1, "spd5118", Some("1-0051"), &[(1, 40500, None)]);
        hwmon(&tmp, 2, "nvme", Some("nvme0"), &[(1, 41850, Some("Composite"))]);
        // CPU exposed only as a thermal zone (coretemp not loaded).
        let tz = tmp.join("sys/class/thermal/thermal_zone0");
        fs::create_dir_all(&tz).unwrap();
        fs::write(tz.join("type"), "x86_pkg_temp\n").unwrap();
        fs::write(tz.join("temp"), "61000\n").unwrap();
        fs::write(tz.join("trip_point_0_type"), "critical\n").unwrap();
        fs::write(tz.join("trip_point_0_temp"), "100000\n").unwrap();

        let mut src = SysfsSource::new(&tmp);
        let r = src.read();
        let labels: Vec<_> = r.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(
            labels,
            ["spd5118 (1-0050)", "spd5118 (1-0051)", "nvme Composite", "x86_pkg_temp"]
        );
        assert_eq!(r[0].temp, Some(38.25));
        assert_eq!(r[1].temp, Some(40.5));
        assert!(r[3].cpu && r[3].critical == Some(100.0));
        assert!(!r[0].cpu);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn picks_up_devices_added_later_and_keeps_ids_when_renumbered() {
        let tmp = std::env::temp_dir().join(format!("oryn-test3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        hwmon(&tmp, 0, "nvme", Some("nvme0"), &[(1, 41000, Some("Composite"))]);
        let mut src = SysfsSource::new(&tmp);
        let first = src.read();
        assert_eq!(first.len(), 1);
        // A USB drive's drivetemp appears, and the NVMe's hwmon number changes.
        fs::remove_dir_all(tmp.join("sys/class/hwmon/hwmon0")).unwrap();
        hwmon(&tmp, 3, "nvme", Some("nvme0"), &[(1, 42000, Some("Composite"))]);
        hwmon(&tmp, 4, "drivetemp", Some("2:0:0:0"), &[(1, 35000, None)]);
        assert_eq!(src.read().len(), 1, "no rescan before the interval");
        src.checked -= super::linux::RESCAN_FOR_TESTS;
        let r = src.read();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].id, first[0].id, "same NVMe, same id");
        assert_eq!(r[0].temp, Some(42.0));
        let _ = fs::remove_dir_all(&tmp);
        // Bogus "no reading" values are dropped.
        hwmon(&tmp, 0, "nct6798", None, &[(1, -128000, None), (2, 255000, None), (3, 33000, None)]);
        let temps: Vec<_> = SysfsSource::new(&tmp).read().iter().map(|r| r.temp).collect();
        assert_eq!(temps, [None, None, Some(33.0)]);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn coretemp_is_cpu_and_duplicate_thermal_zone_skipped() {
        let tmp = std::env::temp_dir().join(format!("oryn-test2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        hwmon(&tmp, 0, "acpitz", None, &[(1, 27800, None)]);
        hwmon(&tmp, 1, "coretemp", Some("coretemp.0"), &[(1, 55000, Some("Package id 0")), (2, 52000, Some("Core 0"))]);
        let tz = tmp.join("sys/class/thermal/thermal_zone0");
        fs::create_dir_all(&tz).unwrap();
        fs::write(tz.join("type"), "acpitz\n").unwrap();
        fs::write(tz.join("temp"), "27800\n").unwrap();

        let r = SysfsSource::new(&tmp).read();
        let labels: Vec<_> = r.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["acpitz", "coretemp Package id 0", "coretemp Core 0"]);
        assert!(r[1].cpu && r[2].cpu && !r[0].cpu);
        let _ = fs::remove_dir_all(&tmp);
    }
}
