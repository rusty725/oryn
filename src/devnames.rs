//! Human-readable names for the hardware behind each Linux sensor.
//!
//! Every lookup is best-effort and only reads files; anything that can't be
//! resolved falls back to a generic description rather than failing.
//!
//! | Sensor chip            | Where the name comes from                         |
//! |------------------------|---------------------------------------------------|
//! | coretemp, k10temp, …   | `/proc/cpuinfo` "model name"                      |
//! | nvme, drivetemp        | `device/model` (+ `device/vendor` for SATA)       |
//! | amdgpu, iwlwifi, …     | PCI vendor/device IDs looked up in `pci.ids`      |
//! | spd5118, ee1004        | the DIMM's own SPD EEPROM: maker + part number    |
//! | super-I/O chips        | "Motherboard (nct6798)"                           |

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::sensors::is_cpu_name;

pub struct Namer {
    root: PathBuf,
    cpu_model: Option<String>,
    pci_ids: Option<Option<String>>, // loaded lazily, at most once
}

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl Namer {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf(), cpu_model: cpu_model(root), pci_ids: None }
    }

    fn cpu(&self) -> String {
        self.cpu_model.clone().unwrap_or_else(|| "CPU".into())
    }

    /// Name and category tag (CPU, RAM, NVME, HDD, USB…) for an hwmon chip,
    /// given its `/sys/class/hwmon/hwmonN` directory.
    pub fn hwmon(&mut self, chip: &str, hwmon_dir: &Path) -> (String, &'static str) {
        let dev = hwmon_dir.join("device");
        let chip_l = chip.to_ascii_lowercase();

        if is_cpu_name(&chip_l) {
            return (self.cpu(), "CPU");
        }
        if matches!(chip_l.as_str(), "spd5118" | "ee1004" | "jc42") {
            return (dimm(&dev), "RAM");
        }
        // NVMe controllers and SATA/SCSI disks expose a model string.
        if let Some(model) = read_trim(&dev.join("model")) {
            let vendor = read_trim(&dev.join("vendor")).filter(|v| v != "ATA");
            let name = match vendor {
                Some(v) if !model.starts_with(&v) => format!("{v} {model}"),
                _ => model,
            };
            let tag = match chip_l.as_str() {
                "nvme" => "NVME",
                "drivetemp" => self.disk_tag(&dev),
                _ => "DISK",
            };
            return (name, tag);
        }
        // Chips we recognise by name (motherboard sensor chips, ACPI, …).
        // Checked before PCI so a known chip isn't renamed after its bus.
        if let Some((name, tag)) = generic(&chip_l) {
            // Network chips sit on a PHY below the PCI card; the card's own
            // name ("Realtek RTL8125 2.5GbE Controller") is better if found.
            if tag == "NET"
                && let Some(p) = self.pci(&dev) {
                    return (p.0, tag);
                }
            return (name, tag);
        }
        // PCI devices (GPUs, USB controllers, Wi-Fi cards, …).
        if let Some(p) = self.pci(&dev) {
            return p;
        }
        (chip.to_string(), "")
    }

    /// Name and tag for a `/sys/class/thermal/thermal_zoneN` of the given type.
    pub fn zone(&mut self, kind: &str) -> (String, &'static str) {
        let k = kind.to_ascii_lowercase();
        if is_cpu_name(&k) {
            return (self.cpu(), "CPU");
        }
        generic(&k).unwrap_or_else(|| (kind.to_string(), ""))
    }

    /// Name of the PCI device at `dir` (e.g. "NVIDIA GeForce RTX 5090").
    pub fn pci_name(&mut self, dir: &Path) -> Option<String> {
        self.pci(dir).map(|(name, _)| name)
    }

    /// HDD or SSD for a drivetemp disk, from the block device's rotational flag.
    fn disk_tag(&self, dev: &Path) -> &'static str {
        let block = fs::read_dir(dev.join("block"))
            .into_iter()
            .flatten()
            .flatten()
            .find_map(|e| e.file_name().into_string().ok());
        let rot = block.and_then(|b| {
            read_trim(&dev.join(format!("block/{b}/queue/rotational")))
                .or_else(|| read_trim(&self.root.join(format!("sys/block/{b}/queue/rotational"))))
        });
        if rot.as_deref() == Some("0") { "SSD" } else { "HDD" }
    }

    /// The PCI device at `dev` or up to 4 levels above it (a sensor can hang
    /// off a PHY or bus below the actual card), as (name, tag).
    fn pci(&mut self, dev: &Path) -> Option<(String, &'static str)> {
        let start = fs::canonicalize(dev).ok()?;
        let pci_dir = start
            .ancestors()
            .take(5)
            .find(|d| d.join("vendor").is_file() && d.join("class").is_file())?
            .to_path_buf();
        let hex = |f: &str| {
            read_trim(&pci_dir.join(f))
                .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        };
        let vendor = hex("vendor")?;
        let device = hex("device")?;
        let class = hex("class").unwrap_or(0);
        let is_gpu = class >> 16 == 0x03;
        let tag = match class >> 8 {
            0x0300..=0x03ff => "GPU",
            0x0c03 => "USB",
            0x0280 => "WIFI",
            0x0200..=0x02ff => "NET",
            0x0108 => "NVME",
            0x0100..=0x01ff => "DISK",
            0x0600..=0x06ff => "CHIP",
            0x0403 => "AUDIO",
            _ => "PCI",
        };

        if self.pci_ids.is_none() {
            let candidates =
                ["usr/share/hwdata/pci.ids", "usr/share/misc/pci.ids", "usr/share/pci.ids"];
            self.pci_ids = Some(
                candidates.iter().find_map(|p| fs::read_to_string(self.root.join(p)).ok()),
            );
        }
        if let Some(Some(db)) = &self.pci_ids
            && let Some((v, d)) = lookup_pci(db, vendor as u16, device as u16) {
                return Some((pci_display(&v, &d, is_gpu), tag));
            }

        // No pci.ids available: describe by class, vendor and driver.
        let vname = match vendor {
            0x1002 | 0x1022 => "AMD",
            0x10de => "NVIDIA",
            0x8086 => "Intel",
            0x14c3 => "MediaTek",
            0x10ec => "Realtek",
            _ => "",
        };
        let kind = match tag {
            "GPU" => "GPU",
            "USB" => "USB controller",
            "WIFI" => "Wi-Fi card",
            "NET" => "network card",
            _ => "PCI device",
        };
        let driver = fs::read_link(pci_dir.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()));
        let mut s = format!("{vname} {kind}").trim().to_string();
        if let Some(d) = driver {
            s.push_str(&format!(" ({d})"));
        }
        Some((s, tag))
    }
}

/// Names and tags for well-known chips that have no better source of a name.
fn generic(chip: &str) -> Option<(String, &'static str)> {
    let motherboard = ["nct", "it87", "it86", "f71", "w83", "asus", "gigabyte", "dell_smm", "thinkpad", "hp_wmi"];
    let net = ["r8169", "r8125", "igc", "e1000e", "igb", "ixgbe", "atlantic", "aq_", "tg3", "bnxt", "mlx"];
    Some(match chip {
        "acpitz" => ("ACPI thermal zone".into(), "ACPI"),
        "nvme" => ("NVMe drive".into(), "NVME"),
        "drivetemp" => ("Disk drive".into(), "HDD"),
        "int3400 thermal" => ("Intel platform thermal".into(), "ACPI"),
        c if c.starts_with("pch_") => ("Intel chipset (PCH)".into(), "CHIP"),
        c if c.starts_with("iwlwifi") => ("Intel Wi-Fi card".into(), "WIFI"),
        c if c.starts_with("mt79") || c.starts_with("mt76") => ("MediaTek Wi-Fi card".into(), "WIFI"),
        c if c.starts_with("bat") => ("Battery".into(), "BAT"),
        c if c.starts_with("r8169") || c.starts_with("r8125") => ("Realtek Ethernet".into(), "NET"),
        c if net.iter().any(|p| c.starts_with(p)) => (format!("Ethernet ({c})"), "NET"),
        c if motherboard.iter().any(|p| c.starts_with(p)) => (format!("Motherboard ({c})"), "MB"),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// CPU
// ---------------------------------------------------------------------------

fn cpu_model(root: &Path) -> Option<String> {
    let info = fs::read_to_string(root.join("proc/cpuinfo")).unwrap_or_default();
    let raw = info
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            matches!(k.trim(), "model name" | "Model" | "Hardware").then(|| v.trim().to_string())
        })
        .or_else(|| {
            // ARM boards (Raspberry Pi etc.) — NUL-terminated string.
            read_trim(&root.join("sys/firmware/devicetree/base/model"))
                .map(|s| s.trim_end_matches('\0').to_string())
        })?;
    Some(tidy_cpu(&raw)).filter(|s| !s.is_empty())
}

/// "Intel(R) Core(TM) i7-12700K CPU @ 3.60GHz" -> "Intel Core i7-12700K"
/// "AMD Ryzen 9 7950X 16-Core Processor"       -> "AMD Ryzen 9 7950X"
fn tidy_cpu(raw: &str) -> String {
    let s = raw.replace("(R)", "").replace("(TM)", "").replace("(tm)", "");
    let s = s.split(" @ ").next().unwrap_or(&s);
    s.split_whitespace()
        .filter(|w| !matches!(*w, "CPU" | "Processor") && !w.ends_with("-Core"))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// RAM (SPD EEPROM)
// ---------------------------------------------------------------------------

/// "Kingston KF560C36BBE-32" when the SPD EEPROM is readable (it may need root),
/// otherwise "RAM DIMM @0x51" from the i2c address.
fn dimm(dev: &Path) -> String {
    let addr = fs::canonicalize(dev)
        .ok()
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()))
        .and_then(|n| n.split_once('-').map(|(_, a)| a.to_string()))
        .and_then(|a| u16::from_str_radix(&a, 16).ok());
    if let Some(name) = spd_eeprom(dev).and_then(|b| parse_spd(&b)) {
        return name;
    }
    match addr {
        Some(a) => format!("RAM DIMM @0x{a:02x}"),
        None => "RAM DIMM".into(),
    }
}

/// The EEPROM file: `eeprom` (ee1004/at24) or an nvmem cell such as
/// `spd5118_0/nvmem` (spd5118, Linux 6.11+).
fn spd_eeprom(dev: &Path) -> Option<Vec<u8>> {
    let mut candidates = vec![dev.join("eeprom")];
    if let Ok(rd) = fs::read_dir(dev) {
        for e in rd.flatten() {
            let nv = e.path().join("nvmem");
            if nv.is_file() {
                candidates.push(nv);
            }
        }
    }
    candidates.into_iter().find_map(|p| {
        let mut buf = Vec::with_capacity(1024);
        File::open(p).ok()?.take(1024).read_to_end(&mut buf).ok()?;
        (buf.len() >= 350).then_some(buf)
    })
}

fn parse_spd(b: &[u8]) -> Option<String> {
    // Byte 2 is the DRAM type. Offsets come from JEDEC SPD specs.
    let (mfr_at, part) = match b.get(2)? {
        0x12 | 0x13 if b.len() >= 551 => (512, &b[521..551]), // DDR5 / LPDDR5
        0x0c | 0x0e if b.len() >= 349 => (320, &b[329..349]), // DDR4 / LPDDR4
        _ => return None,
    };
    let part: String = part
        .iter()
        .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { ' ' })
        .collect::<String>()
        .trim()
        .to_string();
    let maker = jedec_maker(b[mfr_at] & 0x7f, b[mfr_at + 1] & 0x7f);
    match (maker, part.is_empty()) {
        (Some(m), false) => Some(format!("{m} {part}")),
        (Some(m), true) => Some(format!("{m} RAM")),
        (None, false) => Some(part),
        (None, true) => None,
    }
}

/// (continuation-code count, ID without parity bit) -> name, for common RAM makers.
fn jedec_maker(bank: u8, id: u8) -> Option<&'static str> {
    Some(match (bank, id) {
        (0, 0x4e) => "Samsung",
        (0, 0x2d) => "SK hynix",
        (0, 0x2c) => "Micron",
        (1, 0x18) => "Kingston",
        (2, 0x1e) => "Corsair",
        (4, 0x4d) => "G.Skill",
        (4, 0x4b) => "ADATA",
        (4, 0x6f) => "TeamGroup",
        (5, 0x1b) => "Crucial",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// PCI
// ---------------------------------------------------------------------------

/// Look up (vendor name, device name) in a pci.ids database.
fn lookup_pci(db: &str, vendor: u16, device: u16) -> Option<(String, String)> {
    let vtag = format!("{vendor:04x}  ");
    let dtag = format!("\t{device:04x}  ");
    let mut lines = db.lines();
    let vname = lines.find_map(|l| l.strip_prefix(&vtag))?.to_string();
    for l in lines {
        if !l.starts_with('\t') && !l.starts_with('#') && !l.is_empty() {
            break; // next vendor
        }
        if let Some(d) = l.strip_prefix(&dtag) {
            return Some((vname, d.to_string()));
        }
    }
    None
}

fn bracket(s: &str) -> Option<&str> {
    let a = s.rfind('[')?;
    let b = s[a..].find(']')? + a;
    Some(&s[a + 1..b])
}

/// ("Advanced Micro Devices, Inc. [AMD/ATI]", "Navi 31 [Radeon RX 7900 XT/7900 XTX]")
///   -> "AMD/ATI Radeon RX 7900 XT/7900 XTX"
/// ("NVIDIA Corporation", "AD102 [GeForce RTX 4090]") -> "NVIDIA GeForce RTX 4090"
fn pci_display(vendor: &str, device: &str, is_gpu: bool) -> String {
    let v = bracket(vendor)
        .map(|b| if b == "AMD/ATI" { "AMD".to_string() } else { b.to_string() })
        .unwrap_or_else(|| vendor.split([' ', ',']).next().unwrap_or(vendor).to_string());
    // For GPUs the bracket holds the marketing name; elsewhere it's usually a codename.
    let d = match (is_gpu, bracket(device)) {
        (true, Some(b)) => b.to_string(),
        _ => device.split(" [").next().unwrap_or(device).trim().to_string(),
    };
    if d.to_ascii_lowercase().starts_with(&v.to_ascii_lowercase()) {
        d
    } else {
        format!("{v} {d}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_names() {
        assert_eq!(tidy_cpu("Intel(R) Core(TM) i7-12700K CPU @ 3.60GHz"), "Intel Core i7-12700K");
        assert_eq!(tidy_cpu("AMD Ryzen 9 7950X 16-Core Processor"), "AMD Ryzen 9 7950X");
        assert_eq!(tidy_cpu("13th Gen Intel(R) Core(TM) i9-13900K"), "13th Gen Intel Core i9-13900K");
    }

    #[test]
    fn pci_names() {
        let db = "# comment\n1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t744c  Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M]\n10de  NVIDIA Corporation\n\t2684  AD102 [GeForce RTX 4090]\n8086  Intel Corporation\n\t2725  Wi-Fi 6E(802.11ax) AX210/AX1675* 2x2 [Typhoon Peak]\n";
        let (v, d) = lookup_pci(db, 0x1002, 0x744c).unwrap();
        assert_eq!(pci_display(&v, &d, true), "AMD Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M");
        let (v, d) = lookup_pci(db, 0x10de, 0x2684).unwrap();
        assert_eq!(pci_display(&v, &d, true), "NVIDIA GeForce RTX 4090");
        let (v, d) = lookup_pci(db, 0x8086, 0x2725).unwrap();
        assert_eq!(pci_display(&v, &d, false), "Intel Wi-Fi 6E(802.11ax) AX210/AX1675* 2x2");
        assert!(lookup_pci(db, 0x1002, 0x2684).is_none()); // device under wrong vendor
    }

    #[test]
    fn ddr5_spd() {
        let mut b = vec![0u8; 1024];
        b[2] = 0x12;
        b[512] = 0x01 | 0x80; // Kingston: 1 continuation code (parity bit set)
        b[513] = 0x98;
        b[521..521 + 15].copy_from_slice(b"KF560C36BBE-32 ");
        for x in &mut b[536..551] {
            *x = b' ';
        }
        assert_eq!(parse_spd(&b).as_deref(), Some("Kingston KF560C36BBE-32"));
    }
}
