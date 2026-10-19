//! Disk temperatures via SMART, using `smartctl` (smartmontools).
//!
//! * Disks are found with `smartctl --scan`, which works on Linux, Windows
//!   and macOS.
//! * Polling runs on a background thread every [`POLL`], because SMART
//!   queries are slow (tens to hundreds of ms per disk) and disk temperature
//!   changes slowly anyway.
//! * Every query passes `-n standby`, so a sleeping hard drive is reported
//!   as "standby" and is never spun up just to read its temperature.
//! * Reading SMART needs root/admin; without it rows say "needs root".
//!
//! On Linux, disks already covered by the `drivetemp` or `nvme` hwmon drivers
//! are skipped so they don't appear twice. If `smartctl` isn't installed,
//! uncovered SATA disks still get a row with a hint on how to enable them.

use std::collections::HashSet;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
#[cfg(unix)]
use std::sync::atomic::Ordering;

use serde_json::Value;

use crate::sensors::{Reading, Source};

pub const POLL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct Disk {
    /// e.g. "/dev/sda"
    path: String,
    /// smartctl device type from --scan, e.g. "sat", "nvme", "scsi".
    kind: Option<String>,
    /// Name to show when it isn't the path's last part (FreeBSD: "nda0"
    /// for the controller /dev/nvme0).
    name: Option<String>,
    model: Option<String>,
    rotational: Option<bool>,
    temp: Option<f32>,
    critical: Option<f32>,
    note: Option<String>,
}

impl Disk {
    fn new(path: String, kind: Option<String>) -> Self {
        Self { path, kind, name: None, model: None, rotational: None, temp: None, critical: None, note: Some("…".into()) }
    }
}

pub struct SmartPoller {
    disks: Arc<Mutex<Vec<Disk>>>,
}

fn smartctl_exe() -> String {
    // Lets tests (and odd installs) point at a different binary.
    if let Some(p) = crate::env_str("SMARTCTL") {
        return p;
    }
    // Debian, Ubuntu and Fedora install it in /usr/sbin, which a normal
    // user's PATH often lacks; look there too. An absolute path also matches
    // a sudoers rule such as `user ALL=(root) NOPASSWD: /usr/sbin/smartctl`.
    let in_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("smartctl")).collect::<Vec<_>>())
        .unwrap_or_default();
    in_path
        .into_iter()
        .chain(["/usr/sbin/smartctl", "/usr/bin/smartctl", "/sbin/smartctl", "/usr/local/sbin/smartctl"].map(std::path::PathBuf::from))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "smartctl".into())
}

fn smartctl_available() -> bool {
    Command::new(smartctl_exe()).arg("--version").output().is_ok_and(|o| o.status.success())
}

/// `smartctl --scan -j` -> [(path, type)]
fn scan() -> Vec<(String, Option<String>)> {
    let Ok(out) = Command::new(smartctl_exe()).args(["--scan", "-j"]).output() else { return vec![] };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else { return vec![] };
    v["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let name = d["name"].as_str()?.to_string();
            Some((name, d["type"].as_str().map(str::to_string)))
        })
        .collect()
}

impl SmartPoller {
    /// `skip` holds device paths (e.g. "/dev/sda") that another source already reports.
    /// Returns None when there is nothing to show.
    pub fn new(skip: &HashSet<String>) -> Option<Self> {
        let have_smartctl = smartctl_available();
        let mut disks: Vec<Disk> = if have_smartctl {
            scan()
                .into_iter()
                .filter(|(p, _)| !skip.contains(p))
                // On Linux NVMe drives already come from the nvme hwmon driver.
                .filter(|(p, _)| !(cfg!(target_os = "linux") && p.starts_with("/dev/nvme")))
                // On FreeBSD (as root) the scan lists each NVMe drive twice:
                // the controller, which smartctl can read, and its nda/nvd
                // disk, which it can't.
                .filter(|(p, _)| {
                    !(cfg!(target_os = "freebsd") && (p.starts_with("/dev/nda") || p.starts_with("/dev/nvd")))
                })
                .map(|(p, k)| Disk::new(p, k))
                .collect()
        } else {
            Vec::new()
        };

        #[cfg(target_os = "linux")]
        {
            // Fill in model / rotational from sysfs (works without root), and
            // list SATA disks even when smartctl is missing, with a hint.
            for (name, model, rot) in linux_sata_disks() {
                let path = format!("/dev/{name}");
                if skip.contains(&path) {
                    continue;
                }
                if let Some(d) = disks.iter_mut().find(|d| d.path == path) {
                    d.model = d.model.take().or(model);
                    d.rotational = rot;
                } else if !have_smartctl {
                    let mut d = Disk::new(path, None);
                    d.model = model;
                    d.rotational = rot;
                    d.note = Some("modprobe drivetemp".into());
                    disks.push(d);
                }
            }
        }

        #[cfg(target_os = "freebsd")]
        {
            // Without root `smartctl --scan` finds nothing (it opens
            // /dev/pass*), so list the drives from GEOM, which also has
            // model and rotation rate. smartctl wants the NVMe controller
            // ("/dev/nvme0") rather than the nda disk.
            for g in crate::freebsd::disks() {
                let (path, kind) = if g.name.starts_with("nda") || g.name.starts_with("nvd") {
                    match crate::freebsd::nvme_controller(&g.name) {
                        Some(c) => (format!("/dev/{c}"), Some("nvme".to_string())),
                        None => continue,
                    }
                } else if g.name.starts_with("ada") {
                    (format!("/dev/{}", g.name), Some("atacam".to_string()))
                } else {
                    (format!("/dev/{}", g.name), None)
                };
                if skip.contains(&path) {
                    continue;
                }
                let name = (!path.ends_with(&g.name)).then(|| g.name.clone());
                if let Some(d) = disks.iter_mut().find(|d| d.path == path) {
                    d.name = name;
                    d.model = d.model.take().or(g.model);
                    d.rotational = d.rotational.or(g.rotational);
                } else {
                    let mut d = Disk::new(path, kind);
                    d.name = name;
                    d.model = g.model;
                    d.rotational = g.rotational;
                    if !have_smartctl {
                        d.note = Some("pkg install smartmontools".into());
                    }
                    disks.push(d);
                }
            }
        }

        if disks.is_empty() {
            return None;
        }
        let disks = Arc::new(Mutex::new(disks));
        if have_smartctl {
            let shared = Arc::clone(&disks);
            thread::spawn(move || loop {
                let snapshot: Vec<Disk> = shared.lock().unwrap().clone();
                for d in snapshot {
                    let updated = query(d);
                    if let Some(slot) =
                        shared.lock().unwrap().iter_mut().find(|x| x.path == updated.path)
                    {
                        *slot = updated;
                    }
                }
                thread::sleep(POLL);
            });
        }
        Some(Self { disks })
    }

    /// Wait (up to `max`) until every disk has been polled once.
    pub fn wait_first_poll(&self, max: Duration) {
        let start = std::time::Instant::now();
        while start.elapsed() < max
            && self.disks.lock().unwrap().iter().any(|d| d.note.as_deref() == Some("…"))
        {
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn readings(&self) -> Vec<Reading> {
        self.disks
            .lock()
            .unwrap()
            .iter()
            .map(|d| {
                let short = d.name.clone().unwrap_or_else(|| d.path.rsplit('/').next().unwrap_or(&d.path).to_string());
                // Typical maximum operating temperature: 60 °C for hard drives,
                // 70 °C for SSDs, unless the drive reports its own limit.
                let default_crit = if d.rotational == Some(false) { 70.0 } else { 60.0 };
                Reading {
                    id: format!("smart:{}", d.path),
                    label: format!("{short} (SMART)"),
                    device: d.model.clone().unwrap_or_else(|| d.path.clone()),
                    temp: d.temp,
                    critical: d.critical.or(Some(default_crit)),
                    cpu: false,
                    kind: if d.path.contains("nvme") || d.kind.as_deref() == Some("nvme") {
                        "NVME"
                    } else {
                        match d.rotational {
                            Some(false) => "SSD",
                            Some(true) => "HDD",
                            None => "DISK",
                        }
                    },
                    note: d.note.clone(),
                }
            })
            .collect()
    }
}

/// One SMART query for one disk. Keeps the last good temperature when the
/// disk has gone to sleep, so the row shows "standby" with its last value.
fn query(d: Disk) -> Disk {
    // The type from --scan first, then smartctl's auto-detection, then plain
    // SAT (most USB enclosures). The first one that yields a temperature is
    // remembered for later polls.
    let mut kinds: Vec<Option<String>> = vec![d.kind.clone()];
    for k in [None, Some("sat".to_string())] {
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    }
    let mut first_note = None;
    for kind in kinds {
        let mut probe = d.clone();
        probe.kind = kind;
        query_once(&mut probe);
        let done = probe.note.is_none()
            || matches!(probe.note.as_deref(), Some("standby") | Some("needs root"));
        if done {
            return probe;
        }
        first_note.get_or_insert(probe);
    }
    // Nothing worked: report what the original device type said.
    first_note.unwrap_or(d)
}

fn query_once(d: &mut Disk) {
    let mut args: Vec<String> =
        ["-j", "-n", "standby", "-i", "-A"].iter().map(|s| s.to_string()).collect();
    if let Some(k) = &d.kind {
        args.extend(["-d".to_string(), k.clone()]);
    }
    args.push(d.path.clone());

    let Ok(out) = Command::new(smartctl_exe()).args(&args).output() else {
        d.note = Some("smartctl failed to run".into());
        return;
    };
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    apply(d, &v);

    // Not running as root: try `sudo -n smartctl …`. `-n` never prompts; it
    // only succeeds if a sudoers rule allows smartctl without a password
    // (see README). Once that fails, stop trying for the rest of the run.
    #[cfg(unix)]
    if d.note.as_deref() == Some("needs root") && SUDO_OK.load(Ordering::Relaxed) {
        let v = Command::new("sudo")
            .arg("-n")
            .arg(smartctl_exe())
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| !o.stdout.is_empty())
            .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok());
        match v {
            Some(v) => apply(d, &v),
            None => SUDO_OK.store(false, Ordering::Relaxed),
        }
    }
}

/// Whether `sudo -n smartctl` is still worth trying.
#[cfg(unix)]
static SUDO_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The drive temperature from wherever this smartctl/drive pair reports it.
fn find_temp(v: &Value) -> Option<f64> {
    let sane = |t: f64| (t > -40.0 && t < 150.0).then_some(t);
    // 1. smartctl's own summary (ATA 194/190/SCT, SCSI, NVMe).
    if let Some(t) = v["temperature"]["current"].as_f64().and_then(sane) {
        return Some(t);
    }
    // 2. Raw ATA attributes: 194 Temperature_Celsius, 190 Airflow_Temperature_Cel,
    //    231 Temperature on some SSDs. The raw value often packs min/max in
    //    higher bytes ("36 (Min/Max 20/45)"), so take the leading number.
    let table = v["ata_smart_attributes"]["table"].as_array();
    for id in [194, 190, 231] {
        let attr = table.into_iter().flatten().find(|a| a["id"].as_u64() == Some(id));
        if let Some(a) = attr {
            let from_str = a["raw"]["string"]
                .as_str()
                .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).find(|p| !p.is_empty()))
                .and_then(|n| n.parse::<f64>().ok());
            let from_raw = a["raw"]["value"].as_u64().map(|r| (r & 0xff) as f64);
            if let Some(t) = from_str.or(from_raw).and_then(sane) {
                return Some(t);
            }
        }
    }
    // 3. NVMe health log (Kelvin in some versions, Celsius in others).
    if let Some(t) = v["nvme_smart_health_information_log"]["temperature"].as_f64() {
        return sane(if t > 200.0 { t - 273.0 } else { t });
    }
    None
}

/// Why there's no temperature, short enough for the table.
fn reason(v: &Value) -> String {
    if v.is_null() {
        return "smartctl output unreadable (v7+ needed)".into();
    }
    let msgs: Vec<&str> = v["smartctl"]["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["string"].as_str())
        .collect();
    let all = msgs.join(" ").to_ascii_lowercase();
    if all.contains("standby") || all.contains("sleep") {
        return "standby".into();
    }
    if all.contains("permission denied")
        || all.contains("operation not permitted")
        || all.contains("administrator")
    {
        return "needs root".into();
    }
    if v["smart_support"]["available"] == Value::Bool(false) {
        return "no SMART support".into();
    }
    if v["smart_support"]["enabled"] == Value::Bool(false) {
        return "SMART off: smartctl -s on".into();
    }
    if all.contains("unknown usb bridge") {
        return "unknown USB bridge".into();
    }
    // Otherwise smartctl's own first message, trimmed, so the cause is visible.
    match msgs.first() {
        Some(m) => {
            let m = m.split(':').next_back().unwrap_or(m).trim();
            m.chars().take(40).collect()
        }
        None => "drive reports no temperature".into(),
    }
}

fn apply(d: &mut Disk, v: &Value) {
    if let Some(model) = v["model_name"].as_str().or(v["scsi_model_name"].as_str()) {
        d.model = Some(model.trim().to_string());
    }
    if let Some(rpm) = v["rotation_rate"].as_u64() {
        d.rotational = Some(rpm > 0);
    }
    if let Some(lim) = v["temperature"]["op_limit_max"]
        .as_f64()
        .or(v["temperature"]["drive_trip"].as_f64())
        .filter(|l| *l > 30.0 && *l < 120.0)
    {
        d.critical = Some(lim as f32);
    }

    match find_temp(v) {
        Some(t) => {
            d.temp = Some(t as f32);
            d.note = None;
        }
        None => {
            let r = reason(v);
            // A sleeping disk keeps showing its last reading; anything else is unknown.
            if r != "standby" {
                d.temp = None;
            }
            d.note = Some(r);
        }
    }
}

/// (block name, model, rotational) for SATA/SCSI disks on Linux.
#[cfg(target_os = "linux")]
fn linux_sata_disks() -> Vec<(String, Option<String>, Option<bool>)> {
    let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
    let block = std::path::Path::new(&root).join("sys/block");
    let read = |p: std::path::PathBuf| {
        std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    let mut out: Vec<_> = std::fs::read_dir(&block)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !name.starts_with("sd") {
                return None;
            }
            let dir = e.path();
            let model = read(dir.join("device/model"));
            let vendor = read(dir.join("device/vendor")).filter(|v| v != "ATA");
            let model = match (vendor, model) {
                (Some(v), Some(m)) if !m.starts_with(&v) => Some(format!("{v} {m}")),
                (_, m) => m,
            };
            let rot = read(dir.join("queue/rotational")).map(|r| r == "1");
            Some((name, model, rot))
        })
        .collect();
    out.sort();
    out
}

impl Source for SmartPoller {
    fn read(&mut self) -> Vec<Reading> {
        self.readings()
    }
    fn settle(&self) {
        self.wait_first_poll(Duration::from_secs(15));
    }
    fn name(&self) -> &'static str {
        "smart"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_ata_disk() {
        let mut d = Disk::new("/dev/sda".into(), Some("sat".into()));
        apply(
            &mut d,
            &json!({"model_name": "WDC WD40EFRX-68N32N0", "rotation_rate": 5400,
                    "temperature": {"current": 37, "op_limit_max": 65}}),
        );
        assert_eq!(d.temp, Some(37.0));
        assert_eq!(d.critical, Some(65.0));
        assert_eq!(d.model.as_deref(), Some("WDC WD40EFRX-68N32N0"));
        assert_eq!(d.rotational, Some(true));
        assert!(d.note.is_none());
    }

    #[test]
    fn standby_keeps_last_value() {
        let mut d = Disk::new("/dev/sdb".into(), None);
        d.temp = Some(35.0);
        apply(
            &mut d,
            &json!({"smartctl": {"exit_status": 2, "messages": [
                {"string": "Device is in STANDBY mode, exit(2)", "severity": "information"}]}}),
        );
        assert_eq!(d.temp, Some(35.0));
        assert_eq!(d.note.as_deref(), Some("standby"));
    }

    #[test]
    fn attribute_194_fallback() {
        let mut d = Disk::new("/dev/sdd".into(), None);
        apply(
            &mut d,
            &json!({"ata_smart_attributes": {"table": [
                {"id": 9, "raw": {"value": 12345, "string": "12345"}},
                {"id": 194, "raw": {"value": 193273528357u64, "string": "37 (Min/Max 18/45)"}}]}}),
        );
        assert_eq!(d.temp, Some(37.0));
    }

    #[test]
    fn attribute_190_raw_only() {
        let mut d = Disk::new("/dev/sde".into(), None);
        apply(&mut d, &json!({"ata_smart_attributes": {"table": [{"id": 190, "raw": {"value": 0x1e1a0021u64}}]}}));
        assert_eq!(d.temp, Some(33.0));
    }

    #[test]
    fn explains_missing_temperature() {
        let mut d = Disk::new("/dev/sdf".into(), None);
        apply(&mut d, &json!({"smart_support": {"available": true, "enabled": false}}));
        assert_eq!(d.note.as_deref(), Some("SMART off: smartctl -s on"));
        apply(&mut d, &json!({"smartctl": {"messages": [{"string": "/dev/sdf: Unknown USB bridge [0x152d:0x0578 (0x214)]"}]}}));
        assert_eq!(d.note.as_deref(), Some("unknown USB bridge"));
        apply(&mut d, &Value::Null);
        assert_eq!(d.note.as_deref(), Some("smartctl output unreadable (v7+ needed)"));
    }

    #[test]
    fn permission_denied() {
        let mut d = Disk::new("/dev/sdc".into(), None);
        apply(
            &mut d,
            &json!({"smartctl": {"messages": [
                {"string": "Smartctl open device: /dev/sdc failed: Permission denied"}]}}),
        );
        assert_eq!(d.note.as_deref(), Some("needs root"));
        assert!(d.temp.is_none());
    }
}
