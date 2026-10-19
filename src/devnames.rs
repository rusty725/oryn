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
            // Network and Wi-Fi chips: the card's own name ("Realtek RTL8125
            // 2.5GbE Controller", "Intel Raptor Lake PCH CNVi WiFi") is
            // better if found (an Ethernet chip's hwmon sits on a PHY below it).
            if matches!(tag, "NET" | "WIFI")
                && let Some(p) = self.pci(&dev).or_else(|| self.pci_by_driver(&chip_l)) {
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
        match generic(&k) {
            // A Wi-Fi card's zone ("iwlwifi_1") isn't linked to the card.
            Some((_, "WIFI")) if let Some((name, _)) = self.pci_by_driver(&k) => (name, "WIFI"),
            Some(g) => g,
            None => (kind.to_string(), ""),
        }
    }

    /// The PCI device bound to the driver a sensor is named after
    /// ("iwlwifi_1" → iwlwifi), for sensors with no link to their card
    /// (thermal zones). Only when exactly one device uses that driver.
    fn pci_by_driver(&mut self, chip: &str) -> Option<(String, &'static str)> {
        let drivers = self.root.join("sys/bus/pci/drivers");
        let driver = fs::read_dir(&drivers)
            .ok()?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|d| chip.starts_with(d.as_str()))
            .max_by_key(|d| d.len())?;
        // Bound devices appear as links named by PCI address ("0000:00:14.3").
        let devs: Vec<PathBuf> = fs::read_dir(drivers.join(&driver))
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().matches(':').count() == 2)
            .map(|e| e.path())
            .collect();
        match devs.as_slice() {
            [d] => self.pci(d),
            _ => None,
        }
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
    if let Some(b) = spd_eeprom(dev) {
        note_spd(&b);
        if let Some(name) = parse_spd(&b) {
            return name;
        }
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

pub fn parse_spd(b: &[u8]) -> Option<String> {
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

/// One speed rating from a DDR5 SPD: the JEDEC base profile or a vendor
/// overclocking profile (AMD EXPO, Intel XMP 3.0).
#[derive(Clone, Debug, PartialEq)]
pub struct SpdProfile {
    pub profile: &'static str,
    /// Data rate in MT/s.
    pub mts: u32,
}

/// Start of the XMP 3.0 block (magic 0x0C 0x4A, enable bits at +3, then
/// 64-byte header and profile blocks) and of the EXPO block ("EXPO", 10-byte
/// header, 40-byte profiles, one CRC over the 128-byte block). Layouts as
/// pinned by the spdr crate (JESD400-5 decoder, Apache-2.0) against
/// memtest86plus and DDR5SPDEditor and confirmed by each block's CRC.
pub const SPD_XMP: usize = 640;
pub const SPD_EXPO: usize = 832;

/// Every speed profile a DDR5 SPD image holds, vendor profiles first. A
/// vendor block counts only when its CRC matches, so a misread never shows
/// up as a speed.
pub fn spd_profiles(b: &[u8]) -> Vec<SpdProfile> {
    if b.get(2) != Some(&0x12) {
        return Vec::new(); // DDR5 only: other generations lay this out differently
    }
    let word = |off: usize| b.get(off..off + 2).map(|w| u16::from_le_bytes([w[0], w[1]]) as u32);
    // tCK (ps, little-endian) at the given offset.
    let profile = |profile, tck: usize| -> Option<SpdProfile> {
        let tck = word(tck).filter(|c| (200..=1000).contains(c))?;
        Some(SpdProfile { profile, mts: (2_000_000 / tck + 50) / 100 * 100 })
    };
    let crc_ok = |start, len| block_crc_ok(b, start, len);
    let mut out = Vec::new();
    if b.get(SPD_EXPO..SPD_EXPO + 4) == Some(b"EXPO") && crc_ok(SPD_EXPO, 128) {
        for p in [SPD_EXPO + 10, SPD_EXPO + 50] {
            out.extend(profile("EXPO", p + 4));
        }
    }
    if b.get(SPD_XMP..SPD_XMP + 2) == Some(&[0x0C, 0x4A][..]) && crc_ok(SPD_XMP, 64) {
        let enabled = b.get(SPD_XMP + 3).copied().unwrap_or(0);
        for (i, p) in [SPD_XMP + 64, SPD_XMP + 128].into_iter().enumerate() {
            if enabled & (1 << i) != 0 && crc_ok(p, 64) {
                out.extend(profile("XMP", p + 5));
            }
        }
    }
    out.extend(profile("JEDEC", 20));
    out
}

/// Whether the `len`-byte block at `start` ends in the CRC of the rest.
fn block_crc_ok(b: &[u8], start: usize, len: usize) -> bool {
    b.get(start..start + len).is_some_and(|blk| crc16_xmodem(&blk[..len - 2]) == u16::from_le_bytes([blk[len - 2], blk[len - 1]]))
}

/// For `--ram-info`: which vendor blocks an SPD image has and whether their
/// CRCs match ("XMP 3.0: absent · EXPO: present, CRC ok").
pub fn spd_block_status(b: &[u8]) -> String {
    let crc = |ok| if ok { "CRC ok" } else { "CRC MISMATCH" };
    let xmp = if b.get(SPD_XMP..SPD_XMP + 2) == Some(&[0x0C, 0x4A][..]) {
        format!(
            "present, header {}, enable bits {:#04x}, profile 1 {}, profile 2 {}",
            crc(block_crc_ok(b, SPD_XMP, 64)),
            b.get(SPD_XMP + 3).copied().unwrap_or(0),
            crc(block_crc_ok(b, SPD_XMP + 64, 64)),
            crc(block_crc_ok(b, SPD_XMP + 128, 64)),
        )
    } else {
        "absent".into()
    };
    let expo = if b.get(SPD_EXPO..SPD_EXPO + 4) == Some(b"EXPO") {
        format!("present, {}", crc(block_crc_ok(b, SPD_EXPO, 128)))
    } else {
        "absent".into()
    };
    format!("XMP 3.0: {xmp} · EXPO: {expo}")
}

/// CRC-16/XMODEM (poly 0x1021, init 0), which guards the XMP/EXPO blocks.
fn crc16_xmodem(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |mut crc, &byte| {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
        crc
    })
}

/// Linux `oryn --ram-info`: each step behind the RAM names and speed, and
/// where it stops.
#[cfg(target_os = "linux")]
pub fn ram_report(root: &Path) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let sudo = if crate::nvapi::is_root() { "" } else { " (run with sudo)" };

    let mut chips: Vec<(String, PathBuf)> = fs::read_dir(root.join("sys/class/hwmon"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = read_trim(&e.path().join("name"))?;
            matches!(name.as_str(), "spd5118" | "ee1004" | "jc42").then(|| (name, e.path()))
        })
        .collect();
    chips.sort_by(|a, b| a.1.cmp(&b.1));
    let _ = writeln!(out, "SPD chips: {}", if chips.is_empty() {
        "none (soldered memory has none; otherwise load the spd5118 or ee1004 driver)".to_string()
    } else {
        format!("{} found", chips.len())
    });
    for (name, dir) in &chips {
        let dev = dir.join("device");
        let at = fs::canonicalize(&dev).ok().and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()));
        let at = at.as_deref().unwrap_or("?");
        let Some(spd) = spd_eeprom(&dev) else {
            let _ = writeln!(out, "  {name} at {at}: SPD not readable{sudo}");
            continue;
        };
        let _ = writeln!(out, "  {name} at {at}: memory type {:#04x} · {}", spd[2], parse_spd(&spd).unwrap_or_else(|| "no name".into()));
        let _ = writeln!(out, "        {}", spd_block_status(&spd));
        for t in spd_profiles(&spd) {
            let _ = writeln!(out, "        {} {} MT/s", t.profile, t.mts);
        }
        note_spd(&spd);
    }

    let dmi = dmi_memory(root);
    let _ = writeln!(out, "SMBIOS memory devices: {}", if dmi.sticks.is_empty() && dmi.speed.is_none() {
        format!("none read{sudo}")
    } else {
        format!("{} named", dmi.sticks.len())
    });
    let _ = writeln!(out, "  sticks: {:?}", dmi.sticks);
    let _ = writeln!(out, "  speed: {}", dmi.speed.as_deref().unwrap_or("unknown"));
    let _ = writeln!(out, "Speed shown in specs: {}", ram_speed_line(dmi.speed, dmi.mts).unwrap_or_else(|| "none".into()));
    out
}

/// Speed profiles of the sticks read so far (their SPD is read while naming
/// them, on Linux from sysfs, on FreeBSD over SMBus); kits share one set.
static SPD_PROFILES: std::sync::Mutex<Vec<SpdProfile>> = std::sync::Mutex::new(Vec::new());

pub fn note_spd(b: &[u8]) {
    let t = spd_profiles(b);
    if !t.is_empty() {
        *SPD_PROFILES.lock().unwrap() = t;
    }
}

/// The specs panel's Speed line: the speed the BIOS set (`dmi_speed`,
/// "DDR5 · 6000 MT/s"), then the top SPD profile's when that's different:
/// "DDR5 · 6000 MT/s · XMP 6400 MT/s". Without SMBIOS (no root), the
/// rating alone: "DDR5 · XMP 6400 MT/s (rated)".
pub fn ram_speed_line(dmi_speed: Option<String>, mts: Option<u32>) -> Option<String> {
    speed_for(&SPD_PROFILES.lock().unwrap(), dmi_speed, mts)
}

/// Highest speed; on a tie the first (EXPO before XMP before JEDEC).
fn top_profile(sets: &[SpdProfile]) -> Option<&SpdProfile> {
    sets.iter().rev().max_by_key(|t| t.mts)
}

fn speed_for(sets: &[SpdProfile], dmi_speed: Option<String>, mts: Option<u32>) -> Option<String> {
    let top = top_profile(sets);
    match (dmi_speed, top) {
        // Running at the top profile's speed: say which profile it is.
        (Some(s), Some(t)) if mts.is_some_and(|m| t.mts.abs_diff(m) <= 50) => Some(format!("{s} ({})", t.profile)),
        (Some(s), Some(t)) => Some(format!("{s} · {} {} MT/s", t.profile, t.mts)),
        (Some(s), None) => Some(s),
        // SPD profiles are only read from DDR5.
        (None, Some(t)) => Some(format!("DDR5 · {} {} MT/s (rated)", t.profile, t.mts)),
        (None, None) => None,
    }
}

/// Each installed memory module as the BIOS describes it (SMBIOS type 17,
/// `/sys/firmware/dmi/entries/17-*/raw`): "Samsung K3KL8L80CM-MGCT". This
/// covers soldered memory (most laptops), which has no SPD chip to read.
/// The entries are readable by root only; without root this is empty.
pub fn dmi_memory(root: &Path) -> DmiRam {
    let mut entries: Vec<PathBuf> = fs::read_dir(root.join("sys/firmware/dmi/entries"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("17-"))
        .map(|e| e.path().join("raw"))
        .collect();
    entries.sort();
    let raw: Vec<Vec<u8>> = entries.iter().filter_map(|p| fs::read(p).ok()).collect();
    dmi_ram(&raw)
}

/// The installed memory as SMBIOS describes it.
#[derive(Default)]
pub struct DmiRam {
    /// One name per populated slot: "Kingston KF560C36BBE-32".
    pub sticks: Vec<String>,
    /// "DDR5 · 6000 MT/s", or "DDR5 · 4800 MT/s (rated 6000)".
    pub speed: Option<String>,
    /// The speed it runs at in MT/s (configured, else rated).
    pub mts: Option<u32>,
}

/// Names and speed from raw SMBIOS type 17 entries (formatted area plus strings).
pub fn dmi_ram(entries: &[Vec<u8>]) -> DmiRam {
    let speeds: Vec<_> = entries.iter().filter_map(|b| parse_dmi_speed(b)).collect();
    DmiRam {
        sticks: entries.iter().filter_map(|b| parse_dmi_memory(b)).collect(),
        speed: speeds.iter().find_map(|&(kind, configured, rated)| ram_speed_text(kind, configured, rated)),
        mts: speeds.iter().find_map(|&(_, configured, rated)| configured.or(rated)),
    }
}

/// (memory type, configured speed, rated speed) of a populated type 17 entry.
/// Speeds are MT/s; 0xFFFF means "see the 32-bit extended field".
fn parse_dmi_speed(b: &[u8]) -> Option<(Option<&'static str>, Option<u32>, Option<u32>)> {
    let len = *b.get(1)? as usize;
    if *b.first()? != 17 || len < 0x17 || b.len() < len || u16::from_le_bytes([b[0x0C], b[0x0D]]) == 0 {
        return None;
    }
    let word = |off: usize| (len >= off + 2).then(|| u16::from_le_bytes([b[off], b[off + 1]]) as u32);
    let dword = |off: usize| (len >= off + 4).then(|| u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]));
    let speed = |short: usize, ext: usize| match word(short) {
        Some(0xFFFF) => dword(ext),
        w => w,
    }
    .filter(|s| *s > 0);
    let kind = match b[0x12] {
        0x18 => Some("DDR3"),
        0x1A => Some("DDR4"),
        0x1D => Some("LPDDR3"),
        0x1E => Some("LPDDR4"),
        0x22 => Some("DDR5"),
        0x23 => Some("LPDDR5"),
        _ => None,
    };
    Some((kind, speed(0x20, 0x58), speed(0x15, 0x54)))
}

/// "DDR5 · 6000 MT/s"; the rated speed is added when the memory runs slower.
pub fn ram_speed_text(kind: Option<&str>, configured: Option<u32>, rated: Option<u32>) -> Option<String> {
    let mts = match (configured.or(rated), rated) {
        (Some(c), Some(r)) if c < r => format!("{c} MT/s (rated {r})"),
        (Some(c), _) => format!("{c} MT/s"),
        (None, _) => return kind.map(str::to_string),
    };
    Some(match kind {
        Some(k) => format!("{k} · {mts}"),
        None => mts,
    })
}

fn parse_dmi_memory(b: &[u8]) -> Option<String> {
    // Formatted area: type 17, its length at byte 1; size (word) at 0x0C,
    // manufacturer and part number string numbers at 0x17 and 0x1A. The
    // numbered strings follow the formatted area, each NUL-terminated.
    let len = *b.get(1)? as usize;
    if *b.first()? != 17 || len < 0x1B || b.len() < len {
        return None;
    }
    let size = u16::from_le_bytes([b[0x0C], b[0x0D]]);
    if size == 0 {
        return None; // empty slot
    }
    let strings: Vec<&[u8]> = b[len..].split(|&c| c == 0).take_while(|s| !s.is_empty()).collect();
    let string = |n: u8| -> Option<String> {
        Some(String::from_utf8_lossy(strings.get((n as usize).checked_sub(1)?)?).to_string())
    };
    dmi_stick_name(string(b[0x17]).as_deref(), string(b[0x1A]).as_deref())
}

/// "Kingston KF560C36BBE-32" from SMBIOS's manufacturer and part number
/// strings, ignoring the placeholders BIOSes fill empty fields with.
pub fn dmi_stick_name(maker: Option<&str>, part: Option<&str>) -> Option<String> {
    let clean = |s: Option<&str>| -> Option<String> {
        let s = s?.trim().to_string();
        let junk = ["", "Unknown", "Not Specified", "NO DIMM", "Undefined", "0000", "00000000"];
        (!junk.iter().any(|j| s.eq_ignore_ascii_case(j)) && !s.starts_with("Manufacturer") && !s.starts_with("PartNum"))
            .then_some(s)
    };
    let part = clean(part);
    let maker = clean(maker).map(|m| dmi_maker(&m));
    match (maker, part) {
        (Some(m), Some(p)) if !p.to_ascii_lowercase().starts_with(&m.to_ascii_lowercase()) => Some(format!("{m} {p}")),
        (_, Some(p)) => Some(p),
        (Some(m), None) => Some(format!("{m} RAM")),
        (None, None) => None,
    }
}

/// Some BIOSes give the maker as a JEDEC ID in hex ("80CE000080CE",
/// "Unknown (80AD)"); turn those into names where known.
fn dmi_maker(s: &str) -> String {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() >= 4 && (hex.len() == s.len() || s.contains('(')) {
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        if let (Some(bank), Some(id)) = (byte(0), byte(2))
            && let Some(name) = jedec_maker(bank & 0x7f, id & 0x7f) {
                return name.to_string();
            }
    }
    s.to_string()
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
    fn wifi_card_named_from_pci_ids() {
        let tmp = std::env::temp_dir().join(format!("oryn-wifi-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let card = tmp.join("sys/devices/pci0000:00/0000:00:14.3");
        let hwmon = card.join("hwmon/hwmon5");
        fs::create_dir_all(&hwmon).unwrap();
        for (f, v) in [("vendor", "0x8086"), ("device", "0x51f1"), ("class", "0x028000")] {
            fs::write(card.join(f), format!("{v}\n")).unwrap();
        }
        std::os::unix::fs::symlink(&card, hwmon.join("device")).unwrap();
        // Without pci.ids: the generic name.
        assert_eq!(Namer::new(&tmp).hwmon("iwlwifi_1", &hwmon), ("Intel Wi-Fi card".to_string(), "WIFI"));
        // With it: the card's own name, like the specs panel.
        fs::create_dir_all(tmp.join("usr/share/hwdata")).unwrap();
        fs::write(tmp.join("usr/share/hwdata/pci.ids"), "8086  Intel Corporation\n\t51f1  Raptor Lake PCH CNVi WiFi\n").unwrap();
        assert_eq!(Namer::new(&tmp).hwmon("iwlwifi_1", &hwmon), ("Intel Raptor Lake PCH CNVi WiFi".to_string(), "WIFI"));

        // Its thermal zone has no link to the card: found through the driver.
        assert_eq!(Namer::new(&tmp).zone("iwlwifi_1"), ("Intel Wi-Fi card".to_string(), "WIFI"), "driver unknown");
        let drv = tmp.join("sys/bus/pci/drivers/iwlwifi");
        fs::create_dir_all(&drv).unwrap();
        std::os::unix::fs::symlink(&card, drv.join("0000:00:14.3")).unwrap();
        assert_eq!(Namer::new(&tmp).zone("iwlwifi_1"), ("Intel Raptor Lake PCH CNVi WiFi".to_string(), "WIFI"));
        // Two cards on the same driver: can't tell which, keep it generic.
        std::os::unix::fs::symlink(&card, drv.join("0000:05:00.0")).unwrap();
        assert_eq!(Namer::new(&tmp).zone("iwlwifi_1"), ("Intel Wi-Fi card".to_string(), "WIFI"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dmi_memory_modules() {
        // A type 17 entry: 0x28-byte formatted area, then its strings.
        let entry = |size: u16, maker: &str, part: &str| {
            let mut b = vec![0u8; 0x28];
            b[0] = 17;
            b[1] = 0x28;
            b[0x0C..0x0E].copy_from_slice(&size.to_le_bytes());
            b[0x10] = 1; // device locator
            b[0x17] = 2; // manufacturer
            b[0x1A] = 3; // part number
            for s in ["Controller0-ChannelA", maker, part] {
                b.extend_from_slice(s.as_bytes());
                b.push(0);
            }
            b.push(0);
            b
        };
        assert_eq!(parse_dmi_memory(&entry(2048, "Samsung", "K3KL8L80CM-MGCT")).as_deref(), Some("Samsung K3KL8L80CM-MGCT"));
        // JEDEC IDs in hex, trailing spaces, maker already in the part number.
        assert_eq!(parse_dmi_memory(&entry(8192, "80AD000080AD", "HMCG88AGBSA092N     ")).as_deref(), Some("SK hynix HMCG88AGBSA092N"));
        assert_eq!(parse_dmi_memory(&entry(16384, "Kingston", "Kingston KF560C36-32")).as_deref(), Some("Kingston KF560C36-32"));
        // Micron, by name or by JEDEC ID (with and without the "Unknown (…)" wrapper).
        assert_eq!(parse_dmi_memory(&entry(2048, "Micron Technology", "MT62F1G32D4DR-031 WT:B")).as_deref(), Some("Micron Technology MT62F1G32D4DR-031 WT:B"));
        assert_eq!(parse_dmi_memory(&entry(2048, "802C0000802C", "MT62F1G32D4DR-031")).as_deref(), Some("Micron MT62F1G32D4DR-031"));
        assert_eq!(parse_dmi_memory(&entry(2048, "802C", "MT62F1G32D4DR-031")).as_deref(), Some("Micron MT62F1G32D4DR-031"));
        assert_eq!(parse_dmi_memory(&entry(2048, "Unknown (802C)", "MT62F1G32D4DR-031")).as_deref(), Some("Micron MT62F1G32D4DR-031"));
        // Empty slot, and placeholder strings.
        assert_eq!(parse_dmi_memory(&entry(0, "Samsung", "X")), None);
        assert_eq!(parse_dmi_memory(&entry(4096, "Not Specified", "Not Specified")), None);
        assert_eq!(parse_dmi_memory(&[1, 4, 0, 0]), None, "not type 17");

        let tmp = std::env::temp_dir().join(format!("oryn-dmi-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        for (i, size) in [(0, 2048), (1, 0), (2, 2048)] {
            let d = tmp.join(format!("sys/firmware/dmi/entries/17-{i}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("raw"), entry(size, "Micron", "MT62F1G32D4DR-031")).unwrap();
        }
        assert_eq!(dmi_memory(&tmp).sticks, ["Micron MT62F1G32D4DR-031", "Micron MT62F1G32D4DR-031"]);
        assert!(dmi_memory(&tmp.join("nothing")).sticks.is_empty());
        let _ = fs::remove_dir_all(&tmp);
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

    /// A DDR5 SPD: JEDEC 4800 40-39-39-77 in the base block, plus an
    /// EXPO 6000 profile (30-36-36-76) with a valid block CRC.
    fn ddr5_spd_image() -> Vec<u8> {
        let mut b = vec![0u8; 1024];
        b[2] = 0x12;
        let put = |b: &mut Vec<u8>, off: usize, ps: u16| b[off..off + 2].copy_from_slice(&ps.to_le_bytes());
        put(&mut b, 20, 416); // JEDEC tCK: 4800 MT/s
        b[SPD_EXPO..SPD_EXPO + 4].copy_from_slice(b"EXPO");
        put(&mut b, SPD_EXPO + 10 + 4, 333); // EXPO profile 1 tCK: 6000 MT/s
        let crc = crc16_xmodem(&b[SPD_EXPO..SPD_EXPO + 126]);
        put(&mut b, SPD_EXPO + 126, crc);
        b
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ram_report_says_where_it_stops() {
        let r = ram_report(Path::new("/nonexistent"));
        assert!(r.starts_with("SPD chips: none"), "{r}");
        assert!(r.contains("\nSpeed shown in specs: "), "{r}");
    }

    #[test]
    fn ddr5_profiles_from_spd() {
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3); // the XMODEM check value
        let b = ddr5_spd_image();
        let t = spd_profiles(&b);
        let expo = SpdProfile { profile: "EXPO", mts: 6000 };
        let jedec = SpdProfile { profile: "JEDEC", mts: 4800 };
        assert_eq!(t, vec![expo, jedec]);
        // Speed: what the BIOS set, then the top profile's when different.
        let dmi = |s: &str| Some(s.to_string());
        assert_eq!(speed_for(&t, dmi("DDR5 · 6000 MT/s"), Some(6000)).as_deref(), Some("DDR5 · 6000 MT/s (EXPO)"));
        assert_eq!(speed_for(&t, dmi("DDR5 · 5600 MT/s"), Some(5600)).as_deref(), Some("DDR5 · 5600 MT/s · EXPO 6000 MT/s"));
        assert_eq!(speed_for(&t, None, None).as_deref(), Some("DDR5 · EXPO 6000 MT/s (rated)"));
        assert_eq!(speed_for(&[], dmi("DDR4 · 3200 MT/s"), Some(3200)).as_deref(), Some("DDR4 · 3200 MT/s"));
        assert_eq!(speed_for(&[], None, None), None);
        // A corrupted EXPO block (CRC mismatch) is ignored.
        let mut bad = b.clone();
        bad[SPD_EXPO + 20] ^= 1;
        assert_eq!(spd_profiles(&bad).iter().map(|t| t.profile).collect::<Vec<_>>(), ["JEDEC"]);
        // Not DDR5: nothing.
        let mut ddr4 = b;
        ddr4[2] = 0x0c;
        assert!(spd_profiles(&ddr4).is_empty());
    }

    #[test]
    fn xmp_profile_needs_enable_bit_and_crcs() {
        let mut b = ddr5_spd_image();
        b[SPD_EXPO] = 0; // no EXPO
        b[SPD_XMP] = 0x0C;
        b[SPD_XMP + 1] = 0x4A;
        let p = SPD_XMP + 64;
        b[p + 5..p + 7].copy_from_slice(&312u16.to_le_bytes()); // tCK: 6400 MT/s
        let seal = |b: &mut Vec<u8>, start: usize| {
            let crc = crc16_xmodem(&b[start..start + 62]);
            b[start + 62..start + 64].copy_from_slice(&crc.to_le_bytes());
        };
        seal(&mut b, SPD_XMP);
        seal(&mut b, p);
        assert_eq!(spd_profiles(&b).len(), 1); // profile 1 not enabled: JEDEC only
        b[SPD_XMP + 3] = 0x01;
        seal(&mut b, SPD_XMP);
        assert_eq!(spd_profiles(&b)[0], SpdProfile { profile: "XMP", mts: 6400 });
    }
}
