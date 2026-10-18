//! CPU and RAM power draw in watts.
//!
//! The CPU counts the energy it uses, and Linux publishes that counter in
//! the powercap "RAPL" interface: `/sys/class/powercap/intel-rapl:N`, one
//! top-level zone per CPU package, named "package-N". Despite the name it
//! covers AMD Zen 2 and newer too. Watts = energy used since the last read /
//! time since the last read.
//!
//! Some Intel CPUs also count the memory's energy in a "dram" zone (usually
//! a sub-zone, `intel-rapl:0:N`). Most desktop boards and every AMD CPU
//! don't, so RAM watts are often simply unavailable.
//!
//! Since Linux 5.10 the energy counter is readable by root only (a timing
//! side channel, CVE-2020-8694), so a normal user gets "needs root". Boards
//! using the out-of-tree `zenpower` driver also report CPU power through
//! hwmon, which needs no root; that is used when RAPL can't be read.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

struct Zone {
    energy: PathBuf,
    /// The counter wraps around after this many microjoules.
    max_range: u64,
}

pub struct PowerMeter {
    zones: Vec<Zone>,
    /// Counter values and when they were read, for the next difference.
    last: Option<(Vec<u64>, Instant)>,
    /// hwmon `powerN_input` files (microwatts) from zenpower.
    hwmon: Vec<PathBuf>,
    /// Latest reading.
    pub watts: Option<f32>,
    /// Why there's no reading ("needs root"), if that's known.
    pub note: Option<&'static str>,
}

fn read_u64(p: &Path) -> std::io::Result<u64> {
    fs::read_to_string(p)?
        .trim()
        .parse::<u64>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// RAPL zones whose name passes `want`, sorted by directory name. `top`
/// says whether the zone is top-level ("intel-rapl:0") rather than a part
/// of one ("intel-rapl:0:0" = cores). The MMIO duplicate
/// ("intel-rapl-mmio:N") is never taken.
fn rapl_zones(root: &Path, want: impl Fn(&str, bool) -> bool) -> Vec<Zone> {
    let mut zones: Vec<(String, Zone)> = fs::read_dir(root.join("sys/class/powercap"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let dir_name = e.file_name().into_string().ok()?;
            let n = dir_name.strip_prefix("intel-rapl:")?;
            let dir = e.path();
            let name = fs::read_to_string(dir.join("name")).ok()?;
            if !want(name.trim(), !n.contains(':')) {
                return None;
            }
            let max_range = read_u64(&dir.join("max_energy_range_uj")).unwrap_or(u64::MAX);
            Some((dir_name, Zone { energy: dir.join("energy_uj"), max_range }))
        })
        .collect();
    zones.sort_by(|a, b| a.0.cmp(&b.0));
    zones.into_iter().map(|(_, z)| z).collect()
}

impl PowerMeter {
    /// CPU packages. `root` is normally `/`; tests point it at a fake tree.
    pub fn cpu(root: &Path) -> Self {
        // Top-level package zones only: not "psys" (the whole platform) and
        // not their parts, which the package already includes.
        let zones = rapl_zones(root, |name, top| top && name.starts_with("package"));

        // zenpower: power1_input (cores) + power2_input (SoC), in microwatts.
        let hwmon = fs::read_dir(root.join("sys/class/hwmon"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| fs::read_to_string(e.path().join("name")).is_ok_and(|n| n.trim() == "zenpower"))
            .flat_map(|e| (1..=4).map(move |i| e.path().join(format!("power{i}_input"))))
            .filter(|p| p.exists())
            .collect();

        Self { zones, last: None, hwmon, watts: None, note: None }
    }

    /// Memory, from the "dram" zones (one per package, wherever they sit).
    pub fn dram(root: &Path) -> Self {
        let zones = rapl_zones(root, |name, _| name == "dram");
        Self { zones, last: None, hwmon: Vec::new(), watts: None, note: None }
    }

    /// Take a new reading. The first RAPL reading only sets the baseline.
    pub fn refresh(&mut self) {
        if !self.zones.is_empty() {
            let now = Instant::now();
            let mut values = Vec::with_capacity(self.zones.len());
            for z in &self.zones {
                match read_u64(&z.energy) {
                    Ok(v) => values.push(v),
                    Err(e) => {
                        self.last = None;
                        self.watts = None;
                        if e.kind() == std::io::ErrorKind::PermissionDenied {
                            self.note = Some("needs root");
                        }
                        return self.refresh_hwmon();
                    }
                }
            }
            if let Some((prev, at)) = &self.last {
                let dt = now.duration_since(*at).as_secs_f64();
                if dt > 0.05 {
                    let uj: u64 = values
                        .iter()
                        .zip(prev)
                        .zip(&self.zones)
                        .map(|((&new, &old), z)| if new >= old { new - old } else { new + z.max_range.saturating_sub(old) })
                        .sum();
                    let w = uj as f64 / 1e6 / dt;
                    // A counter reset can produce nonsense; no CPU draws 2 kW.
                    self.watts = (w.is_finite() && w < 2000.0).then_some(w as f32);
                }
            }
            self.note = None;
            self.last = Some((values, now));
            return;
        }
        self.refresh_hwmon();
    }

    fn refresh_hwmon(&mut self) {
        if self.hwmon.is_empty() {
            return;
        }
        let uw: Option<u64> = self.hwmon.iter().map(|p| read_u64(p).ok()).sum();
        if let Some(uw) = uw {
            self.watts = Some(uw as f32 / 1e6);
            self.note = None;
        }
    }

    /// `--demo`: a made-up figure.
    pub fn simulate(&mut self, watts: f32) {
        self.watts = Some(watts);
        self.note = None;
    }
}

/// "65 W", "120 W", "8.5 W".
pub fn fmt_watts(w: f32) -> String {
    if w < 10.0 { format!("{w:.1} W") } else { format!("{w:.0} W") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(root: &Path, dir: &str, name: &str, uj: u64) {
        let d = root.join("sys/class/powercap").join(dir);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("name"), format!("{name}\n")).unwrap();
        fs::write(d.join("energy_uj"), format!("{uj}\n")).unwrap();
        fs::write(d.join("max_energy_range_uj"), "262143328850\n").unwrap();
    }

    #[test]
    fn watts_from_the_energy_counter() {
        let tmp = std::env::temp_dir().join(format!("oryn-power-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        zone(&tmp, "intel-rapl:0", "package-0", 1_000_000);
        zone(&tmp, "intel-rapl:0:0", "core", 5); // part of package 0: not counted again
        zone(&tmp, "intel-rapl:1", "psys", 9); // whole platform: not the CPU
        let mut p = PowerMeter::cpu(&tmp);
        assert_eq!(p.zones.len(), 1, "only package-0");
        p.refresh();
        assert_eq!(p.watts, None, "first read is only the baseline");
        // 0.5 s later, 30 J more: 60 W.
        p.last.as_mut().unwrap().1 -= std::time::Duration::from_millis(500);
        fs::write(tmp.join("sys/class/powercap/intel-rapl:0/energy_uj"), "31000000\n").unwrap();
        p.refresh();
        let w = p.watts.unwrap();
        assert!((w - 60.0).abs() < 1.0, "{w}");
        // The counter wraps around.
        p.last.as_mut().unwrap().0[0] = 262_143_328_850 - 10_000_000;
        p.last.as_mut().unwrap().1 -= std::time::Duration::from_millis(1000);
        fs::write(tmp.join("sys/class/powercap/intel-rapl:0/energy_uj"), "35000000\n").unwrap();
        p.refresh();
        let w = p.watts.unwrap();
        assert!((w - 45.0).abs() < 1.0, "{w}");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn zenpower_hwmon_without_root() {
        let tmp = std::env::temp_dir().join(format!("oryn-power2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let d = tmp.join("sys/class/hwmon/hwmon3");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("name"), "zenpower\n").unwrap();
        fs::write(d.join("power1_input"), "52300000\n").unwrap();
        fs::write(d.join("power2_input"), "11700000\n").unwrap();
        let mut p = PowerMeter::cpu(&tmp);
        p.refresh();
        assert_eq!(p.watts, Some(64.0));
        let none = PowerMeter::cpu(&tmp.join("nothing"));
        assert!(none.zones.is_empty() && none.hwmon.is_empty());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dram_zone_wherever_it_sits() {
        let tmp = std::env::temp_dir().join(format!("oryn-power3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        zone(&tmp, "intel-rapl:0", "package-0", 1);
        zone(&tmp, "intel-rapl:0:0", "core", 2);
        zone(&tmp, "intel-rapl:0:2", "dram", 1_000_000);
        zone(&tmp, "intel-rapl:1", "dram", 2_000_000); // some servers: top-level
        let mut d = PowerMeter::dram(&tmp);
        assert_eq!(d.zones.len(), 2);
        assert_eq!(PowerMeter::cpu(&tmp).zones.len(), 1, "dram isn't counted as CPU");
        d.refresh();
        // 1 s later, 3 J + 5 J more: 8 W for both together.
        d.last.as_mut().unwrap().1 -= std::time::Duration::from_secs(1);
        fs::write(tmp.join("sys/class/powercap/intel-rapl:0:2/energy_uj"), "4000000\n").unwrap();
        fs::write(tmp.join("sys/class/powercap/intel-rapl:1/energy_uj"), "7000000\n").unwrap();
        d.refresh();
        let w = d.watts.unwrap();
        assert!((w - 8.0).abs() < 0.2, "{w}");
        let none = PowerMeter::dram(&tmp.join("nothing"));
        assert!(none.zones.is_empty());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn watt_formatting() {
        assert_eq!(fmt_watts(64.4), "64 W");
        assert_eq!(fmt_watts(8.46), "8.5 W");
    }
}
