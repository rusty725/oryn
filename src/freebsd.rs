//! FreeBSD: sensors and system facts from sysctl(3), kenv(2) and the GEOM
//! tree, all readable without root.
//!
//! * CPU: `amdtemp` (dev.amdtemp.N: Tctl as core0.sensor0, one ccdK per
//!   chiplet) or `coretemp` (dev.cpu.N.temperature, one per logical CPU,
//!   reduced to one per physical core). Labels follow the Linux drivers
//!   ("amdtemp Tctl", "amdtemp Tccd1", "coretemp Core 3") so the CPU panel
//!   maps them to cores the same way.
//! * ACPI thermal zones (hw.acpi.thermal.tzN).
//! * Temperature sysctls ("IK" format) are integers in tenths of a kelvin.

use std::ffi::CString;

use crate::sensors::{Reading, Source};
use crate::sysstats::CpuTopo;

/// Raw bytes of a sysctl, or None if it doesn't exist.
pub fn sysctl_raw(name: &str) -> Option<Vec<u8>> {
    let c = CString::new(name).ok()?;
    let mut len: libc::size_t = 0;
    // SAFETY: a size query (null buffer), then a read into a buffer of that size.
    unsafe {
        if libc::sysctlbyname(c.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null(), 0) != 0 {
            return None;
        }
        // Values such as kern.geom.confxml can grow between the two calls.
        let mut buf = vec![0u8; len + len / 4 + 64];
        let mut len = buf.len();
        if libc::sysctlbyname(c.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null(), 0) != 0 {
            return None;
        }
        buf.truncate(len);
        Some(buf)
    }
}

pub fn sysctl_str(name: &str) -> Option<String> {
    let b = sysctl_raw(name)?;
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let s = String::from_utf8_lossy(&b[..end]).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// An integer sysctl of any width (int, long, u_int…).
pub fn sysctl_int(name: &str) -> Option<i64> {
    let b = sysctl_raw(name)?;
    match b.len() {
        4 => Some(i32::from_ne_bytes(b[..4].try_into().ok()?) as i64),
        8 => Some(i64::from_ne_bytes(b[..8].try_into().ok()?)),
        _ => None,
    }
}

/// A temperature sysctl in °C. -1 (or anything absurd) means no reading.
fn sysctl_temp(name: &str) -> Option<f32> {
    let dk = sysctl_int(name)?;
    let c = (dk as f32 - 2731.5) / 10.0;
    (c > -100.0 && c < 200.0).then_some(c)
}

/// A kernel environment variable, e.g. "smbios.planar.product".
pub fn kenv(name: &str) -> Option<String> {
    let c = CString::new(name).ok()?;
    let mut buf = [0u8; 256];
    // SAFETY: KENV_GET writes at most `len` bytes into `buf`.
    let n = unsafe { libc::kenv(libc::KENV_GET, c.as_ptr(), buf.as_mut_ptr().cast(), buf.len() as _) };
    if n <= 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf8_lossy(&buf[..end]).trim().to_string();
    (!s.is_empty() && s != "To Be Filled By O.E.M." && s != "Default string").then_some(s)
}

/// "AMD Ryzen 9 9950X3D 16-Core Processor".
pub fn cpu_model() -> Option<String> {
    sysctl_str("hw.model")
}

/// Highest clock speed cpufreq offers, e.g. 5100 from "5100/4452 3000/2550".
pub fn max_mhz() -> Option<u64> {
    sysctl_str("dev.cpu.0.freq_levels")?
        .split_whitespace()
        .filter_map(|l| l.split('/').next()?.parse::<u64>().ok())
        .max()
}

/// Physical cores (kern.smp.cores), which sysinfo doesn't know here.
pub fn physical_cores() -> Option<usize> {
    sysctl_int("kern.smp.cores").filter(|c| *c > 0).map(|c| c as usize)
}

/// Memory the system can hand back on demand: the ZFS ARC plus inactive pages.
pub fn cached_bytes() -> Option<u64> {
    let page = sysctl_int("hw.pagesize")? as u64;
    let inactive = sysctl_int("vm.stats.vm.v_inactive_count").unwrap_or(0) as u64 * page;
    let arc = sysctl_int("kstat.zfs.misc.arcstats.size").unwrap_or(0) as u64;
    Some(inactive + arc)
}

// ---------------------------------------------------------------------------
// CPU power (RAPL energy MSRs through cpuctl)
// ---------------------------------------------------------------------------

/// A RAPL energy counter MSR, read through cpuctl(4) (`/dev/cpuctl0`, which
/// needs `kldload cpuctl` and root). The counter is 32 bits wide; the energy
/// per count is 1/2^ESU joules, ESU being bits 12:8 of the power unit MSR.
pub struct EnergyMsr {
    unit_msr: u32,
    msr: u32,
    uj_per_count: std::sync::OnceLock<f64>,
}

/// cpuctl_msr_args_t from <sys/cpuctl.h>.
#[repr(C)]
struct MsrArgs {
    msr: libc::c_int,
    data: u64,
}

/// CPUCTL_RDMSR = _IOWR('c', 1, cpuctl_msr_args_t).
const CPUCTL_RDMSR: libc::c_ulong =
    0xC000_0000 | ((std::mem::size_of::<MsrArgs>() as libc::c_ulong & 0x1fff) << 16) | ((b'c' as libc::c_ulong) << 8) | 1;

fn rdmsr(dev: &std::fs::File, msr: u32) -> std::io::Result<u64> {
    use std::os::fd::AsRawFd;
    let mut args = MsrArgs { msr: msr as libc::c_int, data: 0 };
    // SAFETY: CPUCTL_RDMSR fills in `args`, which outlives the call.
    let r = unsafe { libc::ioctl(dev.as_raw_fd(), CPUCTL_RDMSR, &mut args) };
    if r == 0 { Ok(args.data) } else { Err(std::io::Error::last_os_error()) }
}

impl EnergyMsr {
    /// The package counter (`dram: false`) or, on Intel, the memory one.
    /// None when this CPU has no such counter.
    pub fn new(dram: bool) -> Option<Self> {
        let model = cpu_model().unwrap_or_default();
        let (unit_msr, msr) = if model.starts_with("AMD") {
            // Zen and newer: RAPL_PWR_UNIT, PKG_ENERGY_STAT. No DRAM counter.
            (0xC001_0299, if dram { return None } else { 0xC001_029B })
        } else if model.contains("Intel") {
            (0x606, if dram { 0x619 } else { 0x611 })
        } else {
            return None;
        };
        Some(Self { unit_msr, msr, uj_per_count: std::sync::OnceLock::new() })
    }

    /// Energy used so far in microjoules (wraps at [`Self::max_range`]).
    pub fn read_uj(&self) -> std::io::Result<u64> {
        let dev = std::fs::File::open("/dev/cpuctl0")?;
        let unit = match self.uj_per_count.get() {
            Some(u) => *u,
            None => {
                let esu = (rdmsr(&dev, self.unit_msr)? >> 8) & 0x1f;
                *self.uj_per_count.get_or_init(|| 1e6 / (1u64 << esu) as f64)
            }
        };
        let count = rdmsr(&dev, self.msr)? & 0xffff_ffff;
        Ok((count as f64 * unit) as u64)
    }

    /// Where the counter wraps, in microjoules (known after the first read).
    pub fn max_range(&self) -> u64 {
        self.uj_per_count.get().map(|u| ((1u64 << 32) as f64 * u) as u64).unwrap_or(u64::MAX)
    }
}

// ---------------------------------------------------------------------------
// CPU topology
// ---------------------------------------------------------------------------

/// Core and L3 ids for logical CPUs 0..n, from kern.sched.topology_spec.
pub fn topology(n: usize) -> Vec<CpuTopo> {
    parse_topology(&sysctl_str("kern.sched.topology_spec").unwrap_or_default(), n)
}

/// The XML nests groups by cache level. A leaf group flagged "THREAD" is one
/// physical core (its SMT siblings); a group with cache-level="3" shares an
/// L3, which on AMD is one CCD. Ids are numbered in the order groups appear.
fn parse_topology(xml: &str, n: usize) -> Vec<CpuTopo> {
    let mut out = vec![CpuTopo::default(); n];
    let (mut core, mut l3) = (0u32, 0u32);
    for chunk in xml.split("<group").skip(1) {
        let cpus: Vec<usize> = chunk
            .split_once("<cpu")
            .and_then(|(_, r)| r.split_once('>'))
            .and_then(|(_, r)| r.split_once("</cpu>"))
            .map(|(list, _)| list.split(',').filter_map(|c| c.trim().parse().ok()).collect())
            .unwrap_or_default();
        let head = chunk.split('>').next().unwrap_or("");
        if head.contains("cache-level=\"3\"") {
            for &c in &cpus {
                if let Some(t) = out.get_mut(c) {
                    t.l3_id = Some(l3);
                }
            }
            l3 += 1;
        }
        // Flags of a non-leaf group come after its children, i.e. in a later
        // chunk; a THREAD group is always a leaf, so its flags are in its own.
        let own = chunk.split("<children>").next().and_then(|c| c.split("</group>").next()).unwrap_or(chunk);
        if own.contains("THREAD group") {
            for &c in &cpus {
                if let Some(t) = out.get_mut(c) {
                    t.core_id = Some(core);
                }
            }
            core += 1;
        }
    }
    // No SMT: every logical CPU is its own core.
    if out.iter().all(|t| t.core_id.is_none()) {
        for (i, t) in out.iter_mut().enumerate() {
            t.core_id = Some(i as u32);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Temperature sensors
// ---------------------------------------------------------------------------

struct Channel {
    id: String,
    label: String,
    device: String,
    oid: String,
    critical: Option<f32>,
    cpu: bool,
    kind: &'static str,
}

pub struct SysctlSource {
    channels: Vec<Channel>,
}

impl SysctlSource {
    pub fn new() -> Self {
        let model = cpu_model().unwrap_or_else(|| "CPU".into());
        let mut channels = Vec::new();
        let cpu_chan = |oid: String, label: String, critical: Option<f32>| Channel {
            id: format!("sysctl:{oid}"),
            label,
            device: model.clone(),
            oid,
            critical,
            cpu: true,
            kind: "CPU",
        };

        // AMD: Tctl, then one sensor per CCD. Older families expose
        // core0..3.sensor0..1 instead of CCDs; those get their own rows.
        for unit in 0.. {
            let base = format!("dev.amdtemp.{unit}");
            if sysctl_str(&format!("{base}.%driver")).is_none() {
                break;
            }
            let tctl = format!("{base}.core0.sensor0");
            if sysctl_temp(&tctl).is_some() {
                channels.push(cpu_chan(tctl, "amdtemp Tctl".into(), None));
            }
            for k in 0..16 {
                let oid = format!("{base}.ccd{k}");
                if sysctl_int(&oid).is_some() {
                    channels.push(cpu_chan(oid, format!("amdtemp Tccd{}", k + 1), None));
                }
            }
            for core in 0..4 {
                for s in 0..2 {
                    if (core, s) == (0, 0) {
                        continue;
                    }
                    let oid = format!("{base}.core{core}.sensor{s}");
                    if sysctl_temp(&oid).is_some() {
                        channels.push(cpu_chan(oid, format!("amdtemp core{core} sensor{s}"), None));
                    }
                }
            }
        }

        // Intel: coretemp reads each logical CPU, but SMT siblings share one
        // sensor, so keep the first CPU of every physical core.
        if sysctl_int("dev.cpu.0.coretemp.tjmax").is_some() {
            let n = sysctl_int("hw.ncpu").unwrap_or(0).max(0) as usize;
            let topo = topology(n);
            let mut seen = std::collections::HashSet::new();
            for (cpu, t) in topo.iter().enumerate() {
                let core = t.core_id.unwrap_or(cpu as u32);
                let oid = format!("dev.cpu.{cpu}.temperature");
                if !seen.insert(core) || sysctl_temp(&oid).is_none() {
                    continue;
                }
                let tjmax = sysctl_temp(&format!("dev.cpu.{cpu}.coretemp.tjmax"));
                channels.push(cpu_chan(oid, format!("coretemp Core {core}"), tjmax));
            }
        }

        // ACPI thermal zones (often the only sensor on laptops).
        for z in 0.. {
            let base = format!("hw.acpi.thermal.tz{z}");
            let oid = format!("{base}.temperature");
            if sysctl_int(&oid).is_none() {
                break;
            }
            channels.push(Channel {
                id: format!("sysctl:{oid}"),
                label: format!("acpi tz{z}"),
                device: "ACPI thermal zone".into(),
                critical: sysctl_temp(&format!("{base}._CRT")),
                oid,
                cpu: false,
                kind: "ACPI",
            });
        }
        Self { channels }
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }
}

impl Source for SysctlSource {
    fn read(&mut self) -> Vec<Reading> {
        self.channels
            .iter()
            .map(|c| Reading {
                id: c.id.clone(),
                label: c.label.clone(),
                device: c.device.clone(),
                temp: sysctl_temp(&c.oid),
                critical: c.critical,
                cpu: c.cpu,
                kind: c.kind,
                note: None,
            })
            .collect()
    }
    fn name(&self) -> &'static str {
        "sysctl"
    }
}

// ---------------------------------------------------------------------------
// Disks
// ---------------------------------------------------------------------------

pub struct GeomDisk {
    /// GEOM name: "ada0", "nda1", "da2".
    pub name: String,
    pub model: Option<String>,
    pub bytes: u64,
    /// None when the drive doesn't say (rotationrate "unknown").
    pub rotational: Option<bool>,
}

/// Physical disks from the DISK class of kern.geom.confxml (no root needed).
pub fn disks() -> Vec<GeomDisk> {
    let mut out = parse_geom_disks(&sysctl_str("kern.geom.confxml").unwrap_or_default());
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = s.find(&open)? + open.len();
    let len = s[start..].find(&format!("</{name}>"))?;
    Some(s[start..start + len].trim())
}

fn parse_geom_disks(xml: &str) -> Vec<GeomDisk> {
    let Some(class) = xml.split("<class id=").find(|c| tag(c, "name") == Some("DISK")) else {
        return Vec::new();
    };
    class
        .split("<provider ")
        .skip(1)
        .filter_map(|p| {
            let name = tag(p, "name")?.to_string();
            // CD/DVD drives and memory disks aren't worth a temperature row.
            if ["cd", "md"].iter().any(|x| name.starts_with(x)) {
                return None;
            }
            let bytes = tag(p, "mediasize")?.parse::<u64>().ok().filter(|b| *b > 0)?;
            Some(GeomDisk {
                model: tag(p, "descr").filter(|d| !d.is_empty()).map(str::to_string),
                rotational: tag(p, "rotationrate").and_then(|r| r.parse::<u64>().ok()).map(|r| r > 0),
                bytes,
                name,
            })
        })
        .collect()
}

/// The NVMe controller behind an `nda` disk ("nda0" -> "nvme0"), which is
/// what smartctl wants. The boot log says "nda0 at nvme0 bus 0 …".
pub fn nvme_controller(nda: &str) -> Option<String> {
    let boot = std::fs::read_to_string("/var/run/dmesg.boot").unwrap_or_default();
    let prefix = format!("{nda} at ");
    boot.lines()
        .find_map(|l| l.strip_prefix(&prefix)?.split_whitespace().next().map(str::to_string))
        .filter(|c| c.starts_with("nvme"))
        .or_else(|| Some(format!("nvme{}", nda.strip_prefix("nda")?)))
}

/// A disk's firmware revision, from its attach line in the boot log (no
/// root needed): "ada0: <WDC WD4006FZBX-00CAJA0 01.01A01> ACS-4 …" or
/// "nda0: <Samsung SSD 9100 PRO 4TB 0B2QNXH7 S7YANJ0Y304429V>" (firmware,
/// then serial).
pub fn disk_firmware(name: &str, model: &str) -> Option<String> {
    let boot = std::fs::read_to_string("/var/run/dmesg.boot").ok()?;
    parse_boot_firmware(&boot, name, model)
}

fn parse_boot_firmware(boot: &str, name: &str, model: &str) -> Option<String> {
    let prefix = format!("{name}: <");
    // The last attach wins: a disk swapped since boot is listed again.
    let inner = boot.lines().rev().find_map(|l| l.strip_prefix(&prefix)?.split_once('>').map(|(i, _)| i))?;
    let fw = inner.strip_prefix(model)?.strip_prefix(' ')?.split_whitespace().next()?;
    Some(fw.to_string())
}

/// The disk ("nda0", or "nvd0" with the older driver) on an NVMe controller.
fn nvme_disk_name(ctrl: &str) -> Option<String> {
    disks()
        .into_iter()
        .map(|d| d.name)
        .filter(|n| n.starts_with("nda") || n.starts_with("nvd"))
        .find(|n| nvme_controller(n).as_deref() == Some(ctrl))
}

// ---------------------------------------------------------------------------
// NVMe temperatures, straight from the controller
// ---------------------------------------------------------------------------

/// struct nvme_pt_command from <dev/nvme/nvme.h>: a 64-byte command, the
/// 16-byte completion, then the data buffer.
#[repr(C)]
struct PtCommand {
    cmd: [u32; 16],
    cpl: [u32; 4],
    buf: *mut libc::c_void,
    len: u32,
    is_read: u32,
    driver_lock: *mut libc::c_void,
}

const fn iowr(group: u8, num: u8, size: usize) -> libc::c_ulong {
    0xC000_0000 | (((size & 0x1fff) as libc::c_ulong) << 16) | ((group as libc::c_ulong) << 8) | num as libc::c_ulong
}

/// NVME_PASSTHROUGH_CMD = _IOWR('n', 0, struct nvme_pt_command).
const NVME_PASSTHROUGH_CMD: libc::c_ulong = iowr(b'n', 0, std::mem::size_of::<PtCommand>());
/// NVME_GET_CONTROLLER_DATA = _IOR('n', 4, struct nvme_controller_data).
const NVME_GET_CONTROLLER_DATA: libc::c_ulong = 0x4000_0000 | ((4096 & 0x1fff) << 16) | ((b'n' as libc::c_ulong) << 8) | 4;

/// An NVMe temperature field (kelvin, 0 = not implemented) in °C.
fn kelvin_at(buf: &[u8], off: usize) -> Option<f32> {
    let k = u16::from_le_bytes([*buf.get(off)?, *buf.get(off + 1)?]);
    (k > 0).then(|| k as f32 - 273.15).filter(|c| *c > -40.0 && *c < 150.0)
}

struct NvmeCtrl {
    dev: std::fs::File,
    /// "/dev/nvme0", what smartctl would use.
    path: String,
    /// "nda0", the name the rest of the system uses for the drive.
    name: String,
    model: String,
    critical: Option<f32>,
}

impl NvmeCtrl {
    /// The SMART / health information log page (02h), 512 bytes.
    fn health_log(&self) -> Option<[u8; 512]> {
        use std::os::fd::AsRawFd;
        let mut buf = [0u8; 512];
        let mut pt = PtCommand {
            cmd: [0; 16],
            cpl: [0; 4],
            buf: buf.as_mut_ptr().cast(),
            len: buf.len() as u32,
            is_read: 1,
            driver_lock: std::ptr::null_mut(),
        };
        pt.cmd[0] = 0x02; // Get Log Page
        pt.cmd[1] = 0xFFFF_FFFF; // whole controller
        pt.cmd[10] = ((buf.len() as u32 / 4 - 1) << 16) | 0x02; // dwords - 1, log id
        // SAFETY: the driver writes at most `len` bytes into `buf`, which outlives the call.
        let r = unsafe { libc::ioctl(self.dev.as_raw_fd(), NVME_PASSTHROUGH_CMD, &mut pt) };
        // Completion status is the top half of dword 3; bit 0 there is the phase tag.
        (r == 0 && (pt.cpl[3] >> 17) == 0).then_some(buf)
    }
}

/// NVMe drives read through the controller's own health log, the same data
/// `nvmecontrol logpage -p 2` shows: the composite temperature plus up to
/// eight extra sensors, and the drive's own critical limit. Opening
/// /dev/nvmeN needs root; controllers that can't be opened are left to
/// the SMART poller (which can still go through `sudo -n smartctl`).
pub struct NvmeSource {
    ctrls: Vec<NvmeCtrl>,
}

impl NvmeSource {
    pub fn new() -> Self {
        use std::os::fd::AsRawFd;
        let mut ctrls = Vec::new();
        for n in 0.. {
            if sysctl_str(&format!("dev.nvme.{n}.%driver")).is_none() {
                break;
            }
            let path = format!("/dev/nvme{n}");
            let Ok(dev) = std::fs::File::open(&path) else { continue };
            let mut id = [0u8; 4096];
            // SAFETY: the driver fills exactly 4096 bytes of identify data.
            let ok = unsafe { libc::ioctl(dev.as_raw_fd(), NVME_GET_CONTROLLER_DATA, id.as_mut_ptr()) } == 0;
            let ctrl = format!("nvme{n}");
            let name = nvme_disk_name(&ctrl).unwrap_or_else(|| ctrl.clone());
            let model = ok
                .then(|| String::from_utf8_lossy(&id[24..64]).trim().to_string())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| name.clone());
            // Critical composite temperature, else the warning one.
            let critical = if ok { kelvin_at(&id, 268).or_else(|| kelvin_at(&id, 266)) } else { None };
            let c = NvmeCtrl { dev, path, name, model, critical };
            if c.health_log().is_some() {
                ctrls.push(c);
            }
        }
        Self { ctrls }
    }

    /// Controllers read here, so the SMART poller can skip them.
    pub fn paths(&self) -> std::collections::HashSet<String> {
        self.ctrls.iter().map(|c| c.path.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.ctrls.is_empty()
    }
}

impl Source for NvmeSource {
    fn read(&mut self) -> Vec<Reading> {
        let mut out = Vec::new();
        for c in &self.ctrls {
            let log = c.health_log();
            let reading = |label: String, temp: Option<f32>, critical: Option<f32>| Reading {
                id: format!("nvme:{}:{label}", c.path),
                label: format!("{} {label}", c.name),
                device: c.model.clone(),
                temp,
                critical,
                cpu: false,
                kind: "NVME",
                note: None,
            };
            out.push(reading("Composite".into(), log.as_ref().and_then(|l| kelvin_at(l, 1)), c.critical));
            // Temperature Sensor 1–8 at bytes 200–215; unimplemented ones read 0.
            for s in 0..8 {
                if let Some(t) = log.as_ref().and_then(|l| kelvin_at(l, 200 + 2 * s)) {
                    out.push(reading(format!("Sensor {}", s + 1), Some(t), None));
                }
            }
        }
        out
    }
    fn name(&self) -> &'static str {
        "nvme"
    }
}

// ---------------------------------------------------------------------------
// DDR5 RAM: the SPD5118 hub on each DIMM, over SMBus
// ---------------------------------------------------------------------------

/// struct smbcmd from <dev/smbus/smb.h>.
#[repr(C)]
struct SmbCmd {
    cmd: u8,
    reserved: u8,
    op: u16,
    wdata: [u8; 2],
    rdata: [u8; 2],
    /// 7-bit address shifted left by one, as smb(4) wants it.
    slave: libc::c_int,
    wbuf: *mut libc::c_char,
    wcount: libc::c_int,
    rbuf: *mut libc::c_char,
    rcount: libc::c_int,
}

/// SMB_READB = _IOWR('i', 7, struct smbcmd); SMB_WRITEB = _IOW('i', 5, …).
const SMB_READB: libc::c_ulong = iowr(b'i', 7, std::mem::size_of::<SmbCmd>());
const SMB_WRITEB: libc::c_ulong =
    0x8000_0000 | (((std::mem::size_of::<SmbCmd>() & 0x1fff) as libc::c_ulong) << 16) | ((b'i' as libc::c_ulong) << 8) | 5;

fn smb_cmd(addr: u8, reg: u8) -> SmbCmd {
    SmbCmd {
        cmd: reg,
        reserved: 0,
        op: 0,
        wdata: [0; 2],
        rdata: [0; 2],
        slave: (addr as libc::c_int) << 1,
        wbuf: std::ptr::null_mut(),
        wcount: 0,
        rbuf: std::ptr::null_mut(),
        rcount: 0,
    }
}

fn smb_read(dev: &std::fs::File, addr: u8, reg: u8) -> Option<u8> {
    use std::os::fd::AsRawFd;
    let mut c = smb_cmd(addr, reg);
    // SAFETY: the driver fills `rdata` of `c`, which outlives the call.
    (unsafe { libc::ioctl(dev.as_raw_fd(), SMB_READB, &mut c) } == 0).then_some(c.rdata[0])
}

fn smb_write(dev: &std::fs::File, addr: u8, reg: u8, value: u8) -> Option<()> {
    use std::os::fd::AsRawFd;
    let mut c = smb_cmd(addr, reg);
    c.wdata[0] = value;
    // SAFETY: as above; the driver only reads `c`.
    (unsafe { libc::ioctl(dev.as_raw_fd(), SMB_WRITEB, &mut c) } == 0).then_some(())
}

/// An SPD5118 temperature register (MR49/50 and the limits): bits 12:2 are a
/// signed count of 0.25 °C.
fn spd5118_temp(lo: u8, hi: u8) -> f32 {
    let raw = ((u16::from(hi) << 8 | u16::from(lo)) >> 2) & 0x7ff;
    let signed = ((raw << 5) as i16) >> 5;
    signed as f32 * 0.25
}

/// SPD5118 hub registers (JEDEC JESD300-5).
const MR_DEVICE_TYPE: u8 = 0x00; // 0x51 0x18 = SPD5118
const MR_LEGACY_PAGE: u8 = 0x0B; // bits 2:0 NVM page, bit 3 two-byte addressing
const MR_TEMP_CRIT_HIGH: u8 = 0x20;
const MR_TEMP_CONFIG: u8 = 0x1A; // bit 0 set = sensor disabled
const MR_TEMP: u8 = 0x31;

struct Dimm {
    bus: usize,
    addr: u8,
    model: String,
    critical: Option<f32>,
}

/// DDR5 DIMM temperatures from each stick's SPD5118 hub, like Linux's
/// `spd5118` driver: the hub sits at SMBus address 0x50–0x57 and its MR49
/// register is the stick's temperature. The name comes from the SPD
/// EEPROM behind the hub (maker + part number). Needs `kldload smb` (for
/// /dev/smbN) and root.
pub struct RamSource {
    buses: Vec<std::fs::File>,
    dimms: Vec<Dimm>,
    /// Shown as a single row when nothing can be read and why is known.
    note: Option<&'static str>,
}

impl RamSource {
    pub fn new() -> Self {
        let mut buses = Vec::new();
        let mut dimms = Vec::new();
        let mut note = None;
        let has_smbus = sysctl_str("dev.smbus.0.%driver").is_some();
        for n in 0.. {
            if sysctl_str(&format!("dev.smb.{n}.%driver")).is_none() {
                break;
            }
            let dev = match std::fs::File::options().read(true).write(true).open(format!("/dev/smb{n}")) {
                Ok(d) => d,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::PermissionDenied {
                        note = Some("needs root");
                    }
                    continue;
                }
            };
            for addr in 0x50..=0x57u8 {
                // Only a confirmed SPD5118 hub is ever written to (page select).
                let is_hub = smb_read(&dev, addr, MR_DEVICE_TYPE) == Some(0x51)
                    && smb_read(&dev, addr, MR_DEVICE_TYPE + 1) == Some(0x18);
                if !is_hub {
                    continue;
                }
                let crit = match (smb_read(&dev, addr, MR_TEMP_CRIT_HIGH), smb_read(&dev, addr, MR_TEMP_CRIT_HIGH + 1)) {
                    (Some(lo), Some(hi)) => Some(spd5118_temp(lo, hi)).filter(|c| *c > 30.0 && *c < 150.0),
                    _ => None,
                };
                let model = spd5118_name(&dev, addr).unwrap_or_else(|| format!("RAM DIMM @0x{addr:02x}"));
                dimms.push(Dimm { bus: buses.len(), addr, model, critical: crit });
            }
            buses.push(dev);
        }
        if dimms.is_empty() && note.is_none() && has_smbus && sysctl_str("dev.smb.0.%driver").is_none() {
            note = Some("kldload smb");
        }
        Self { buses, dimms, note }
    }
}

/// Maker and part number from the SPD EEPROM behind an SPD5118 hub; its
/// timings are handed to `devnames::note_spd` for the specs panel.
fn spd5118_name(dev: &std::fs::File, addr: u8) -> Option<String> {
    let spd = spd5118_read(dev, addr).ok()?;
    crate::devnames::note_spd(&spd);
    crate::devnames::parse_spd(&spd)
}

/// The parts of the SPD EEPROM behind an SPD5118 hub that Oryn uses (the
/// rest stays 0), or why it couldn't be read. The EEPROM is read in
/// 128-byte pages through the hub's legacy mode: MR11 selects the page,
/// registers 0x80–0xFF are its bytes. MR11 is put back.
fn spd5118_read(dev: &std::fs::File, addr: u8) -> Result<Vec<u8>, String> {
    use crate::devnames::{SPD_EXPO, SPD_XMP};
    let saved = smb_read(dev, addr, MR_LEGACY_PAGE).ok_or("MR11 (page select) unreadable")?;
    if saved & 0x08 != 0 {
        return Err(format!("MR11 = {saved:#04x}: two-byte addressing, not legacy mode"));
    }
    let mut spd = vec![0u8; 1024];
    let mut page = None;
    let mut read = |spd: &mut [u8], bytes: std::ops::Range<usize>| -> Result<(), String> {
        for i in bytes {
            let p = (i / 128) as u8;
            if page != Some(p) {
                smb_write(dev, addr, MR_LEGACY_PAGE, (saved & !0x07) | p).ok_or(format!("selecting page {p} refused"))?;
                page = Some(p);
            }
            spd[i] = smb_read(dev, addr, 0x80 | (i % 128) as u8).ok_or(format!("SPD byte {i} unreadable"))?;
        }
        Ok(())
    };
    // Bytes 0–47: memory type and JEDEC timings. 512–550: maker id and part
    // number. The XMP (640–831) and EXPO (832–959) blocks only if present.
    let ok = (|| {
        read(&mut spd, 0..48)?;
        read(&mut spd, 512..551)?;
        if spd[2] == 0x12 {
            // DDR5: the vendor profiles, if their magic is there.
            read(&mut spd, SPD_XMP..SPD_XMP + 2)?;
            if spd[SPD_XMP..SPD_XMP + 2] == [0x0C, 0x4A] {
                read(&mut spd, SPD_XMP + 2..SPD_EXPO)?;
            }
            read(&mut spd, SPD_EXPO..SPD_EXPO + 4)?;
            if &spd[SPD_EXPO..SPD_EXPO + 4] == b"EXPO" {
                read(&mut spd, SPD_EXPO + 4..SPD_EXPO + 128)?;
            }
        }
        Ok(())
    })();
    let _ = smb_write(dev, addr, MR_LEGACY_PAGE, saved);
    ok.map(|_| spd)
}

/// `oryn --ram-info`: each step behind the RAM names, temperatures, speed and
/// timings, and where it stops.
pub fn ram_report() -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let loaded = |m: &str| sysctl_str(&format!("dev.{m}.0.%driver")).is_some();
    let _ = writeln!(out, "SMBus: smbus driver {}, smb(4) {}", if loaded("smbus") { "loaded" } else { "not loaded" },
        if loaded("smb") { "loaded" } else { "not loaded (sudo kldload smb)" });
    for n in 0.. {
        let Some(parent) = sysctl_str(&format!("dev.smb.{n}.%parent")) else { break };
        // smb0 on smbus0 on the controller (intsmb0, amdsmb0, ichsmb0…).
        let ctrl = parent.trim_start_matches("smbus").parse::<u32>().ok()
            .and_then(|i| sysctl_str(&format!("dev.smbus.{i}.%parent")));
        let desc = ctrl.as_deref().and_then(|c| {
            let split = c.find(|ch: char| ch.is_ascii_digit())?;
            sysctl_str(&format!("dev.{}.{}.%desc", &c[..split], &c[split..]))
        });
        let _ = writeln!(out, "/dev/smb{n}: on {parent} on {} ({})", ctrl.as_deref().unwrap_or("?"), desc.as_deref().unwrap_or("?"));
        let dev = match std::fs::File::options().read(true).write(true).open(format!("/dev/smb{n}")) {
            Ok(d) => d,
            Err(e) => {
                let _ = writeln!(out, "  can't open: {e}{}", if crate::nvapi::is_root() { "" } else { " (run with sudo)" });
                continue;
            }
        };
        for addr in 0x50..=0x57u8 {
            let id = (smb_read(&dev, addr, MR_DEVICE_TYPE), smb_read(&dev, addr, MR_DEVICE_TYPE + 1));
            let _ = match id {
                (None, None) => writeln!(out, "  {addr:#04x}: nothing"),
                (Some(0x51), Some(0x18)) => match spd5118_read(&dev, addr) {
                    Ok(spd) => {
                        let _ = writeln!(out, "  {addr:#04x}: SPD5118 hub · memory type {:#04x}{} · {}", spd[2],
                            if spd[2] == 0x12 { " (DDR5)" } else { "" },
                            crate::devnames::parse_spd(&spd).unwrap_or_else(|| "no name".into()));
                        let _ = writeln!(out, "        {}", crate::devnames::spd_block_status(&spd));
                        let sets = crate::devnames::spd_timings(&spd);
                        if sets.is_empty() {
                            let _ = writeln!(out, "        no timings decoded");
                        }
                        for t in &sets {
                            let _ = writeln!(out, "        {} {} MT/s: CL{}-{}-{}-{}", t.profile, t.mts, t.cl, t.rcd, t.rp, t.ras);
                        }
                        crate::devnames::note_spd(&spd);
                        Ok(())
                    }
                    Err(e) => writeln!(out, "  {addr:#04x}: SPD5118 hub, SPD read failed: {e}"),
                },
                (a, b) => writeln!(out, "  {addr:#04x}: not an SPD5118 hub (MR0/MR1 = {a:02x?} {b:02x?})"),
            };
        }
    }
    let entries = smbios_entries(17);
    let _ = writeln!(out, "SMBIOS memory devices: {}", match &entries {
        Some(e) => format!("{} entries from /dev/mem", e.len()),
        None => format!("/dev/mem not readable{}", if crate::nvapi::is_root() { "" } else { " (run with sudo)" }),
    });
    let dmi = match entries {
        Some(e) if !e.is_empty() => crate::devnames::dmi_ram(&e),
        _ => {
            let d = dmidecode_memory();
            let _ = writeln!(out, "  dmidecode fallback: {}", if d.is_some() { "ok" } else { "not available" });
            d.unwrap_or_default()
        }
    };
    let _ = writeln!(out, "  sticks: {:?}", dmi.sticks);
    let _ = writeln!(out, "  speed: {} (MT/s for timings: {:?})", dmi.speed.as_deref().unwrap_or("unknown"), dmi.mts);
    let _ = writeln!(out, "Timings shown in specs: {}", crate::devnames::ram_timings_text(dmi.mts)
        .unwrap_or_else(|| "none (no SPD timings read)".into()));
    out
}

impl Source for RamSource {
    fn read(&mut self) -> Vec<Reading> {
        if self.dimms.is_empty() {
            return match self.note {
                Some(note) => vec![Reading {
                    id: "smb:ram".into(),
                    label: "spd5118".into(),
                    device: "RAM".into(),
                    temp: None,
                    critical: None,
                    cpu: false,
                    kind: "RAM",
                    note: Some(note.into()),
                }],
                None => Vec::new(),
            };
        }
        self.dimms
            .iter()
            .map(|d| {
                let dev = &self.buses[d.bus];
                let off = smb_read(dev, d.addr, MR_TEMP_CONFIG).is_some_and(|c| c & 1 != 0);
                let temp = match (smb_read(dev, d.addr, MR_TEMP), smb_read(dev, d.addr, MR_TEMP + 1)) {
                    (Some(lo), Some(hi)) if !off => Some(spd5118_temp(lo, hi)).filter(|t| *t > -40.0 && *t < 150.0),
                    _ => None,
                };
                Reading {
                    // Same label scheme as Linux: "spd5118 (0-0050)".
                    id: format!("smb{}:{:02x}", d.bus, d.addr),
                    label: format!("spd5118 ({}-{:04x})", d.bus, d.addr),
                    device: d.model.clone(),
                    temp,
                    critical: d.critical,
                    cpu: false,
                    kind: "RAM",
                    note: off.then(|| "sensor off".into()),
                }
            })
            .collect()
    }
    fn name(&self) -> &'static str {
        "smbus"
    }
}

// ---------------------------------------------------------------------------
// Memory modules from SMBIOS
// ---------------------------------------------------------------------------

/// Installed memory (names, type and speed) from the SMBIOS memory device
/// entries (type 17), the data `dmidecode -t memory` shows. Read straight
/// from the table through /dev/mem (root); without root, `sudo -n
/// dmidecode` if a sudoers rule allows it. Otherwise empty.
pub fn dmi_memory() -> crate::devnames::DmiRam {
    match smbios_entries(17) {
        Some(e) if !e.is_empty() => crate::devnames::dmi_ram(&e),
        _ => dmidecode_memory().unwrap_or_default(),
    }
}

/// Every SMBIOS structure of type `kind`, each with its strings. The loader
/// leaves the entry point's physical address in kenv (hint.smbios.0.mem).
fn smbios_entries(kind: u8) -> Option<Vec<Vec<u8>>> {
    use std::os::unix::fs::FileExt;
    let ep_addr = u64::from_str_radix(kenv("hint.smbios.0.mem")?.trim_start_matches("0x"), 16).ok()?;
    let mem = std::fs::File::open("/dev/mem").ok()?;
    let mut ep = [0u8; 32];
    mem.read_exact_at(&mut ep, ep_addr).ok()?;
    let (addr, len) = if ep.starts_with(b"_SM3_") {
        // 64-bit entry point: maximum table size, then the table address.
        (u64::from_le_bytes(ep[0x10..0x18].try_into().ok()?), u32::from_le_bytes(ep[0x0C..0x10].try_into().ok()?) as usize)
    } else if ep.starts_with(b"_SM_") {
        (u32::from_le_bytes(ep[0x18..0x1C].try_into().ok()?) as u64, u16::from_le_bytes(ep[0x16..0x18].try_into().ok()?) as usize)
    } else {
        return None;
    };
    if len == 0 || len > 1 << 20 {
        return None;
    }
    let mut table = vec![0u8; len];
    mem.read_exact_at(&mut table, addr).ok()?;
    Some(parse_smbios_table(&table, kind))
}

/// Split a raw SMBIOS table into its structures of type `kind`: each is a
/// formatted area (length in byte 1) followed by strings, ending in "\0\0".
fn parse_smbios_table(t: &[u8], kind: u8) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= t.len() {
        let (typ, len) = (t[i], t[i + 1] as usize);
        if len < 4 || i + len > t.len() {
            break;
        }
        let mut end = i + len;
        while end + 1 < t.len() && !(t[end] == 0 && t[end + 1] == 0) {
            end += 1;
        }
        let next = (end + 2).min(t.len());
        if typ == kind {
            out.push(t[i..next].to_vec());
        }
        if typ == 127 {
            break; // end of table
        }
        i = next;
    }
    out
}

/// `sudo -n dmidecode -t 17`, for when /dev/mem isn't readable. `-n`
/// never prompts; it only works with a sudoers rule for dmidecode.
fn dmidecode_memory() -> Option<crate::devnames::DmiRam> {
    let exe = ["/usr/local/sbin/dmidecode", "/usr/sbin/dmidecode"].into_iter().find(|p| std::path::Path::new(p).is_file())?;
    let out = std::process::Command::new("sudo")
        .args(["-n", exe, "-t", "17"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(parse_dmidecode_memory(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_dmidecode_memory(text: &str) -> crate::devnames::DmiRam {
    let mut ram = crate::devnames::DmiRam::default();
    for dev in text.split("Memory Device").skip(1) {
        let field = |name: &str| {
            dev.lines().find_map(|l| l.trim().strip_prefix(name)?.strip_prefix(':').map(|v| v.trim().to_string()))
        };
        if field("Size").is_none_or(|s| s.starts_with("No Module") || s == "0") {
            continue;
        }
        let mts = |name: &str| field(name)?.split_whitespace().next()?.parse::<u32>().ok().filter(|s| *s > 0);
        if let Some(n) = crate::devnames::dmi_stick_name(field("Manufacturer").as_deref(), field("Part Number").as_deref()) {
            ram.sticks.push(n);
        }
        if ram.speed.is_none() {
            let kind = field("Type").filter(|t| t.contains("DDR"));
            ram.speed = crate::devnames::ram_speed_text(kind.as_deref(), mts("Configured Memory Speed"), mts("Speed"));
        }
        if ram.mts.is_none() {
            ram.mts = mts("Configured Memory Speed").or_else(|| mts("Speed"));
        }
    }
    ram
}

// ---------------------------------------------------------------------------
// Network adapters
// ---------------------------------------------------------------------------

/// Physical network adapters: "re0 · Realtek PCIe 5GbE Family Controller ·
/// re driver · 1 Gb/s". Interfaces without a device node (lo0, bridges,
/// epairs, tap) are left out.
pub fn nics() -> Vec<String> {
    let names = std::process::Command::new("ifconfig")
        .arg("-l")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let mut out: Vec<String> = names
        .split_whitespace()
        .filter_map(|ifname| {
            let split = ifname.find(|c: char| c.is_ascii_digit())?;
            let (driver, unit) = ifname.split_at(split);
            let desc = sysctl_str(&format!("dev.{driver}.{unit}.%desc"))?;
            let mut v = format!("{ifname} · {desc} · {driver} driver");
            let info = std::process::Command::new("ifconfig")
                .arg(ifname)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            if let Some(mbps) = media_mbps(&info) {
                v.push_str(&if mbps >= 1000 { format!(" · {} Gb/s", mbps as f64 / 1000.0) } else { format!(" · {mbps} Mb/s") });
            }
            if !info.contains("status: active") {
                v.push_str(" · down");
            }
            Some(v)
        })
        .collect();
    out.sort();
    out
}

/// Link speed from ifconfig's "media: Ethernet autoselect (1000baseT …)".
fn media_mbps(ifconfig: &str) -> Option<u64> {
    let media = ifconfig.lines().find(|l| l.trim_start().starts_with("media:"))?;
    let inner = media.split_once('(')?.1;
    let word = inner.split(|c: char| c == ' ' || c == ')').next()?;
    let digits: String = word.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let n: f64 = digits.parse().ok()?;
    let rest = &word[digits.len()..];
    // "1000baseT", "2500Base-T", "10Gbase-T", "25G-…".
    Some(if rest.starts_with('G') { (n * 1000.0) as u64 } else { n as u64 })
}

// ---------------------------------------------------------------------------
// NVIDIA hotspot / VRAM temperatures from the GPU's registers

/// struct pcisel from <sys/pciio.h>.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct PciSel {
    domain: u32,
    bus: u8,
    dev: u8,
    func: u8,
}

/// struct pci_bar_mmap from <sys/pciio.h>.
#[repr(C)]
struct PciBarMmap {
    map_base: *mut libc::c_void,
    map_length: libc::size_t,
    bar_length: u64,
    bar_off: libc::c_int,
    sel: PciSel,
    reg: libc::c_int,
    flags: libc::c_int,
    memattr: libc::c_int,
}

const PCIOCBARMMAP: libc::c_ulong = iowr(b'p', 8, std::mem::size_of::<PciBarMmap>());
/// PCIR_BAR(0): the GPU's register space.
const PCIR_BAR0: libc::c_int = 0x10;
/// VM_MEMATTR_UNCACHEABLE on amd64: registers must not be cached.
const MEMATTR_UNCACHEABLE: libc::c_int = 0;

/// One NVIDIA GPU's BAR0, mapped read-only through /dev/pci.
struct Bar0 {
    sel: PciSel,
    device_id: u16,
    map: *mut libc::c_void,
    map_len: usize,
    /// Where the BAR starts in the mapping, and its length.
    off: usize,
    len: usize,
    state: crate::nvapi::RegState,
}

impl crate::nvapi::Regs for Bar0 {
    fn read_regs(&self, offsets: &[u32]) -> Result<Vec<u64>, String> {
        offsets
            .iter()
            .map(|&o| {
                let o = o as usize;
                if o % 4 != 0 || o + 4 > self.len {
                    return Err(format!("register {o:#x} outside BAR0"));
                }
                // SAFETY: an aligned 32-bit read inside the mapped BAR.
                Ok(unsafe { std::ptr::read_volatile(self.map.cast::<u8>().add(self.off + o).cast::<u32>()) } as u64)
            })
            .collect()
    }
}

impl Drop for Bar0 {
    fn drop(&mut self) {
        // SAFETY: the mapping PCIOCBARMMAP made, unmapped once.
        unsafe { libc::munmap(self.map, self.map_len) };
    }
}

/// NVIDIA's hotspot and per-chip VRAM temperatures, which NvAPI provides on
/// Linux and NVML doesn't. FreeBSD has no NvAPI, so the same registers are
/// read from a read-only, uncached mapping of each GPU's BAR0 and decoded by
/// the same code (`nvapi::read_registers`). Mapping a BAR needs /dev/pci
/// open for writing, i.e. root; only reads are made.
pub struct NvRegs {
    gpus: Vec<Bar0>,
    /// Why nothing could be mapped ("needs root"), for the GPU panel.
    pub note: Option<&'static str>,
}

impl NvRegs {
    pub fn open() -> Self {
        let mut out = NvRegs { gpus: Vec::new(), note: None };
        let found = nvidia_gpus();
        if found.is_empty() {
            return out;
        }
        let pci = match std::fs::OpenOptions::new().read(true).write(true).open("/dev/pci") {
            Ok(f) => f,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    out.note = Some("needs root");
                }
                return out;
            }
        };
        for (sel, device_id) in found {
            let mut m = PciBarMmap {
                map_base: std::ptr::null_mut(),
                map_length: 0,
                bar_length: 0,
                bar_off: 0,
                sel,
                reg: PCIR_BAR0,
                flags: 0, // read-only
                memattr: MEMATTR_UNCACHEABLE,
            };
            use std::os::fd::AsRawFd;
            // SAFETY: PCIOCBARMMAP fills `m` with a new mapping of the BAR.
            if unsafe { libc::ioctl(pci.as_raw_fd(), PCIOCBARMMAP, &mut m) } != 0 || m.map_base.is_null() {
                continue;
            }
            out.gpus.push(Bar0 {
                sel,
                device_id,
                map: m.map_base,
                map_len: m.map_length,
                off: m.bar_off as usize,
                len: m.bar_length as usize,
                state: Default::default(),
            });
        }
        out
    }

    /// Temperatures for the GPU on PCI bus `bus`, if its BAR0 is mapped.
    pub fn read(&mut self, bus: u32) -> Option<crate::nvapi::Temps> {
        let g = self.gpus.iter_mut().find(|g| g.sel.bus as u32 == bus)?;
        let id = g.device_id;
        let mut t = crate::nvapi::Temps::default();
        let mut state = std::mem::take(&mut g.state);
        crate::nvapi::read_registers(&*g, &mut state, crate::nvapi::is_blackwell(id), crate::nvapi::has_gddr_regs(id), &mut t);
        g.state = state;
        Some(t)
    }
}

/// NVIDIA display controllers (vgapci) and their PCI device ids.
fn nvidia_gpus() -> Vec<(PciSel, u16)> {
    (0..16)
        .filter_map(|n| {
            let pnp = sysctl_str(&format!("dev.vgapci.{n}.%pnpinfo"))?;
            let loc = sysctl_str(&format!("dev.vgapci.{n}.%location"))?;
            parse_vgapci(&pnp, &loc)
        })
        .collect()
}

/// From "vendor=0x10de device=0x2c02 …" and "… dbsf=pci0:1:0:0 …".
fn parse_vgapci(pnp: &str, loc: &str) -> Option<(PciSel, u16)> {
    let field = |s: &str, key: &str| s.split_whitespace().find_map(|w| w.strip_prefix(key)).map(str::to_string);
    let hex = |s: String| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok();
    if hex(field(pnp, "vendor=")?)? != 0x10de {
        return None;
    }
    let device = hex(field(pnp, "device=")?)?;
    let dbsf = field(loc, "dbsf=pci")?;
    let n: Vec<u32> = dbsf.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let [domain, bus, dev, func] = n[..] else { return None };
    Some((PciSel { domain, bus: bus as u8, dev: dev as u8, func: func as u8 }, device))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topology_ccds_and_smt() {
        // Two L3s of two cores each; SMT siblings are adjacent, as FreeBSD numbers them.
        let xml = r#"<groups>
 <group level="1" cache-level="0">
  <cpu count="8" mask="ff">0, 1, 2, 3, 4, 5, 6, 7</cpu>
  <children>
   <group level="2" cache-level="3">
    <cpu count="4" mask="f">0, 1, 2, 3</cpu>
    <children>
     <group level="3" cache-level="2">
      <cpu count="2" mask="3">0, 1</cpu>
      <flags><flag name="THREAD">THREAD group</flag><flag name="SMT">SMT group</flag></flags>
     </group>
     <group level="3" cache-level="2">
      <cpu count="2" mask="c">2, 3</cpu>
      <flags><flag name="THREAD">THREAD group</flag><flag name="SMT">SMT group</flag></flags>
     </group>
    </children>
   </group>
   <group level="2" cache-level="3">
    <cpu count="4" mask="f0">4, 5, 6, 7</cpu>
    <children>
     <group level="3" cache-level="2">
      <cpu count="2" mask="30">4, 5</cpu>
      <flags><flag name="THREAD">THREAD group</flag><flag name="SMT">SMT group</flag></flags>
     </group>
     <group level="3" cache-level="2">
      <cpu count="2" mask="c0">6, 7</cpu>
      <flags><flag name="THREAD">THREAD group</flag><flag name="SMT">SMT group</flag></flags>
     </group>
    </children>
   </group>
  </children>
 </group>
</groups>"#;
        let t = parse_topology(xml, 8);
        let cores: Vec<_> = t.iter().map(|c| c.core_id.unwrap()).collect();
        let l3: Vec<_> = t.iter().map(|c| c.l3_id.unwrap()).collect();
        assert_eq!(cores, [0, 0, 1, 1, 2, 2, 3, 3]);
        assert_eq!(l3, [0, 0, 0, 0, 1, 1, 1, 1]);
    }

    #[test]
    fn topology_without_smt() {
        let xml = r#"<groups><group level="1" cache-level="3"><cpu count="2" mask="3">0, 1</cpu></group></groups>"#;
        let t = parse_topology(xml, 2);
        assert_eq!(t[1].core_id, Some(1));
        assert_eq!(t[1].l3_id, Some(0));
    }

    #[test]
    fn firmware_from_boot_log() {
        let boot = "nda0 at nvme0 bus 0 scbus8 target 0 lun 1\n\
                    nda0: <Samsung SSD 9100 PRO 4TB 0B2QNXH7 S7YANJ0Y304429V>\n\
                    ada0: <WDC WD4006FZBX-00CAJA0 01.01A01> ACS-4 ATA SATA 3.x device\n\
                    ada10: <HGST HDN724040ALE640 MJAOA5E0> ATA8-ACS SATA 3.x device\n";
        assert_eq!(parse_boot_firmware(boot, "nda0", "Samsung SSD 9100 PRO 4TB").as_deref(), Some("0B2QNXH7"));
        assert_eq!(parse_boot_firmware(boot, "ada0", "WDC WD4006FZBX-00CAJA0").as_deref(), Some("01.01A01"));
        // "ada1: <" must not match "ada10: <".
        assert_eq!(parse_boot_firmware(boot, "ada1", "HGST HDN724040ALE640"), None);
        // A model GEOM spells differently: no guess.
        assert_eq!(parse_boot_firmware(boot, "ada0", "WDC WD4006"), None);
    }

    #[test]
    fn geom_disks() {
        let xml = r#"<mesh>
  <class id="0x1"><name>PART</name><geom id="0x2"><class ref="0x1"/><name>ada0</name>
    <provider id="0x3"><name>ada0p1</name><mediasize>100</mediasize></provider></geom></class>
  <class id="0x4">
    <name>DISK</name>
    <geom id="0x5"><class ref="0x4"/><name>ada0</name>
	<provider id="0x6"><name>ada0</name><mediasize>4000787030016</mediasize>
	  <config><rotationrate>7200</rotationrate><descr>WDC WD4006FZBX-00CAJA0</descr></config></provider>
    </geom>
    <geom id="0x7"><name>nda0</name>
	<provider id="0x8"><name>nda0</name><mediasize>4000787030016</mediasize>
	  <config><rotationrate>0</rotationrate><descr>Samsung SSD 9100 PRO 4TB</descr></config></provider>
    </geom>
    <geom id="0x9"><name>cd0</name>
	<provider id="0xa"><name>cd0</name><mediasize>0</mediasize><config></config></provider>
    </geom>
  </class>
</mesh>"#;
        let d = parse_geom_disks(xml);
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].name.as_str(), d[0].rotational), ("ada0", Some(true)));
        assert_eq!(d[1].model.as_deref(), Some("Samsung SSD 9100 PRO 4TB"));
        assert_eq!(d[1].rotational, Some(false));
    }

    #[test]
    fn link_speed() {
        assert_eq!(media_mbps("\tmedia: Ethernet autoselect (1000baseT <full-duplex>)\n"), Some(1000));
        assert_eq!(media_mbps("\tmedia: Ethernet autoselect (2500Base-T <full-duplex>)\n"), Some(2500));
        assert_eq!(media_mbps("\tmedia: Ethernet autoselect (10Gbase-T <full-duplex>)\n"), Some(10000));
        assert_eq!(media_mbps("\tmedia: Ethernet autoselect\n"), None);
    }

    #[test]
    fn nvme_ioctls_and_temperatures() {
        // Values of the macros in <dev/nvme/nvme.h> on amd64.
        assert_eq!(std::mem::size_of::<PtCommand>(), 104);
        assert_eq!(NVME_PASSTHROUGH_CMD, 0xC068_6E00);
        assert_eq!(NVME_GET_CONTROLLER_DATA, 0x5000_6E04);
        let mut log = [0u8; 512];
        log[1..3].copy_from_slice(&310u16.to_le_bytes()); // composite 310 K
        log[202..204].copy_from_slice(&320u16.to_le_bytes()); // sensor 2
        assert_eq!(kelvin_at(&log, 1).map(|c| c.round()), Some(37.0));
        assert_eq!(kelvin_at(&log, 200), None, "0 = sensor not implemented");
        assert_eq!(kelvin_at(&log, 202).map(|c| c.round()), Some(47.0));
    }

    #[test]
    fn smbus_ioctls_and_ddr5_temperatures() {
        assert_eq!(std::mem::size_of::<SmbCmd>(), 48);
        assert_eq!(SMB_READB, 0xC030_6907);
        assert_eq!(SMB_WRITEB, 0x8030_6905);
        assert_eq!(spd5118_temp(0x64, 0x02), 38.25); // 153 quarter degrees
        assert_eq!(spd5118_temp(0xF0, 0x1F), -1.0); // sign bit 12
        assert_eq!(spd5118_temp(0x30, 0x05), 83.0); // a typical critical limit
    }

    #[test]
    fn pci_bar_mmap_and_vgapci() {
        // Checked against <sys/pciio.h> on FreeBSD 15 (amd64).
        assert_eq!(std::mem::size_of::<PciBarMmap>(), 48);
        assert_eq!(std::mem::offset_of!(PciBarMmap, sel), 28);
        assert_eq!(std::mem::offset_of!(PciBarMmap, memattr), 44);
        assert_eq!(PCIOCBARMMAP, 0xC030_7008);
        let (sel, id) = parse_vgapci(
            "vendor=0x10de device=0x2c02 subvendor=0x196e subdevice=0x1435 class=0x030000",
            "slot=0 function=0 dbsf=pci0:1:0:0 handle=\\_SB_.PCI0.GPP0.VGA_",
        )
        .unwrap();
        assert_eq!((sel, id), (PciSel { domain: 0, bus: 1, dev: 0, func: 0 }, 0x2c02));
        assert!(parse_vgapci("vendor=0x1002 device=0x744c", "dbsf=pci0:3:0:0").is_none());
    }

    /// The register decoding over a BAR0 stand-in (anonymous memory holding
    /// an RTX 5080's registers).
    #[test]
    fn bar0_hotspot_and_vram_chips() {
        use crate::nvapi::{read_registers, Temps};
        let len = 0x100_0000;
        // SAFETY: a fresh anonymous mapping, owned (and unmapped) by `bar`.
        let map = unsafe {
            libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_ANON | libc::MAP_PRIVATE, -1, 0)
        };
        assert_ne!(map, libc::MAP_FAILED);
        let set = |off: usize, v: u32| unsafe { map.cast::<u8>().add(off).cast::<u32>().write(v) };
        set(0x0, 0x1B30_0000); // PMC_BOOT_0: GB203
        set(0xAD0A90, 0x4000_3D80); // NV_THERM: tagged 61.5 °C
        set(0x9024D0, 1 << 24); // partition A: chip 0 present
        set(0x9024C0, 0x2E << 16); // its temperature byte: 2 * 46 - 40 = 52 °C
        let mut bar = Bar0 { sel: PciSel::default(), device_id: 0x2c02, map, map_len: len, off: 0, len, state: Default::default() };
        let mut t = Temps::default();
        let mut state = std::mem::take(&mut bar.state);
        read_registers(&bar, &mut state, true, true, &mut t);
        assert_eq!(t.hotspot, Some(61.5));
        assert_eq!(t.vram, Some(52.0));
        assert_eq!(t.vram_chips, vec![("A0".to_string(), 52.0)]);
        use crate::nvapi::Regs;
        assert!(bar.read_reg(len as u32).is_err()); // outside the BAR
    }

    /// A type 17 entry: DDR5, 32 GB, rated 6000, configured 6000, with
    /// maker "Kingston" and part "KF560C36-32" as strings 1 and 2.
    fn dimm_entry(size: u16) -> Vec<u8> {
        let mut e = vec![0u8; 0x5C];
        e[0] = 17;
        e[1] = 0x5C;
        e[0x0C..0x0E].copy_from_slice(&size.to_le_bytes());
        e[0x12] = 0x22;
        e[0x15..0x17].copy_from_slice(&6000u16.to_le_bytes());
        e[0x17] = 1;
        e[0x1A] = 2;
        e[0x20..0x22].copy_from_slice(&6000u16.to_le_bytes());
        e.extend_from_slice(b"Kingston\0KF560C36-32\0\0");
        e
    }

    #[test]
    fn smbios_table_memory_devices() {
        // A type 0 (BIOS) entry, two slots (one empty), then end-of-table.
        let mut t = vec![0u8, 4, 0, 0];
        t.extend_from_slice(b"AMI\0\0");
        t.extend(dimm_entry(32768));
        t.extend(dimm_entry(0));
        t.extend_from_slice(&[127, 4, 0, 0, 0, 0]);
        let entries = parse_smbios_table(&t, 17);
        assert_eq!(entries.len(), 2);
        let ram = crate::devnames::dmi_ram(&entries);
        assert_eq!(ram.sticks, ["Kingston KF560C36-32"]);
        assert_eq!(ram.speed.as_deref(), Some("DDR5 · 6000 MT/s"));
    }

    #[test]
    fn dmidecode_text() {
        let text = "Handle 0x0014, DMI type 17, 92 bytes\nMemory Device\n\tSize: No Module Installed\n\tPart Number: Unknown\n\n\
                    Handle 0x0015, DMI type 17, 92 bytes\nMemory Device\n\tSize: 48 GB\n\tType: DDR5\n\tSpeed: 6400 MT/s\n\
                    \tManufacturer: G Skill Intl\n\tPart Number: F5-6400J3239F48G    \n\tConfigured Memory Speed: 6000 MT/s\n";
        let ram = parse_dmidecode_memory(text);
        assert_eq!(ram.sticks, ["G Skill Intl F5-6400J3239F48G"]);
        assert_eq!(ram.speed.as_deref(), Some("DDR5 · 6000 MT/s (rated 6400)"));
    }

    #[test]
    fn reads_live_sysctls() {
        assert!(sysctl_int("hw.ncpu").unwrap() > 0);
        assert!(sysctl_str("kern.ostype").as_deref() == Some("FreeBSD"));
        assert!(sysctl_str("no.such.oid").is_none());
    }
}
