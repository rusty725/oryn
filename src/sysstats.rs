//! System statistics for the btop-style panels: CPU, memory, disks, network
//! and processes. Everything comes from the `sysinfo` crate (Linux, Windows,
//! macOS); on Linux the memory "Cached" figure is read from /proc/meminfo.
//!
//! Only the panels that are visible are refreshed, so hiding the process
//! list (the most expensive one) also saves CPU.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use sysinfo::{
    CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, Networks, Pid,
    ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind, Users,
};

pub const HIST: usize = 120;

static DEMO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--demo`: the CPU panel simulates a 32-thread AMD Ryzen 9 9950X3D.
pub fn set_demo() {
    DEMO.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_demo() -> bool {
    DEMO.load(std::sync::atomic::Ordering::Relaxed)
}

#[derive(Default, Clone)]
pub struct Mem {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub cached: u64,
    pub free: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

#[derive(Clone)]
pub struct DiskInfo {
    /// "root" for "/", otherwise the last path component ("home", "boot").
    pub label: String,
    pub mount: String,
    pub total: u64,
    pub used: u64,
    pub read_rate: f64,
    pub write_rate: f64,
}

#[derive(Clone, Default)]
pub struct NetInfo {
    pub name: String,
    pub ip: Option<String>,
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub rx_hist: VecDeque<f64>,
    pub tx_hist: VecDeque<f64>,
}

#[derive(Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub cmd: String,
    pub user: String,
    pub mem: u64,
    /// Percent of one core (can exceed 100 for multi-threaded programs).
    pub cpu: f32,
    /// Scheduling priority (-20 … 19), when known.
    pub nice: Option<i32>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ProcSort {
    Cpu,
    Mem,
    Pid,
    Name,
}

impl ProcSort {
    pub fn next(self) -> Self {
        match self {
            ProcSort::Cpu => ProcSort::Mem,
            ProcSort::Mem => ProcSort::Pid,
            ProcSort::Pid => ProcSort::Name,
            ProcSort::Name => ProcSort::Cpu,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ProcSort::Cpu => "cpu",
            ProcSort::Mem => "mem",
            ProcSort::Pid => "pid",
            ProcSort::Name => "name",
        }
    }
}

/// Which groups of data to refresh this tick.
#[derive(Clone, Copy, Default)]
pub struct Want {
    pub cpu: bool,
    pub mem: bool,
    pub disks: bool,
    pub net: bool,
    pub procs: bool,
}

pub struct SysStats {
    sys: System,
    disks_src: Disks,
    nets_src: Networks,
    users: Users,
    user_names: HashMap<String, String>,
    /// When disks / network were last read. Their byte counters cover the
    /// time since then, so each group needs its own clock: refreshing just
    /// the process list (or nothing, in compact mode) mustn't shorten it.
    last_disks: Instant,
    last_net: Instant,

    pub cpu_name: String,
    pub cpu_total: f32,
    pub cpu_cores: Vec<f32>,
    pub cpu_hist: VecDeque<f32>,
    /// Current clock speed in MHz: the average over all cores, and the
    /// fastest core (0 when unknown).
    pub cpu_freq_mhz: u64,
    pub cpu_freq_max_mhz: u64,
    pub load: (f64, f64, f64),
    pub uptime: u64,

    pub mem: Mem,
    pub disks: Vec<DiskInfo>,
    pub nets: Vec<NetInfo>,
    pub net_sel: usize,
    pub procs: Vec<ProcInfo>,
    pub proc_count: usize,
    pub proc_sort: ProcSort,
    demo_tick: u64,
}

impl SysStats {
    pub fn new() -> Self {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(cpu_refresh_kind())
                .with_memory(MemoryRefreshKind::everything()),
        );
        let cpu_name = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_string())
            .filter(|b| !b.is_empty());
        // sysinfo has no brand string on FreeBSD; hw.model does.
        #[cfg(target_os = "freebsd")]
        let cpu_name = cpu_name.or_else(crate::freebsd::cpu_model);
        let cpu_name = cpu_name.unwrap_or_else(|| "CPU".into());
        let disks_src = {
            let _guard = libc_buffers();
            Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage().with_io_usage())
        };
        let mut s = Self {
            sys,
            disks_src,
            nets_src: Networks::new_with_refreshed_list(),
            users: Users::new_with_refreshed_list(),
            user_names: HashMap::new(),
            last_disks: Instant::now(),
            last_net: Instant::now(),
            cpu_name,
            cpu_total: 0.0,
            cpu_cores: Vec::new(),
            cpu_hist: VecDeque::with_capacity(HIST),
            cpu_freq_mhz: 0,
            cpu_freq_max_mhz: 0,
            load: (0.0, 0.0, 0.0),
            uptime: 0,
            mem: Mem::default(),
            disks: Vec::new(),
            nets: Vec::new(),
            net_sel: 0,
            procs: Vec::new(),
            proc_count: 0,
            proc_sort: ProcSort::Cpu,
            demo_tick: 0,
        };
        s.refresh(Want { cpu: true, mem: true, disks: true, net: true, procs: false });
        // Default to the busiest network interface.
        s.net_sel = s
            .nets
            .iter()
            .enumerate()
            .max_by_key(|(_, n)| n.rx_total + n.tx_total)
            .map(|(i, _)| i)
            .unwrap_or(0);
        s
    }

    pub fn refresh(&mut self, want: Want) {
        // Seconds since this group was last read (its counters' window).
        let since = |t: &mut Instant| {
            let dt = t.elapsed().as_secs_f64().max(0.05);
            *t = Instant::now();
            dt
        };

        if want.cpu {
            self.sys.refresh_cpu_specifics(cpu_refresh_kind());
            self.cpu_total = self.sys.global_cpu_usage();
            self.cpu_cores = self.sys.cpus().iter().map(|c| c.cpu_usage()).collect();
            let freqs = core_freqs_mhz(self.sys.cpus());
            self.cpu_freq_mhz = if freqs.is_empty() { 0 } else { freqs.iter().sum::<u64>() / freqs.len() as u64 };
            self.cpu_freq_max_mhz = freqs.iter().copied().max().unwrap_or(0);
            if is_demo() {
                // Mostly idle desktop with the odd busy thread.
                self.demo_tick = self.demo_tick.wrapping_add(1);
                let t = self.demo_tick;
                self.cpu_cores = (0..32u64)
                    .map(|i| {
                        let h = (i.wrapping_mul(2654435761) ^ t.wrapping_mul(40503)) % 100;
                        if h < 6 { 20.0 + (h * 9) as f32 } else { (h % 4) as f32 }
                    })
                    .collect();
                self.cpu_total = self.cpu_cores.iter().sum::<f32>() / 32.0;
                self.cpu_name = "AMD Ryzen 9 9950X3D 16-Core Processor".into();
                // Busy cores boost, idle ones drift lower.
                let freqs: Vec<u64> = self
                    .cpu_cores
                    .iter()
                    .enumerate()
                    .map(|(i, u)| {
                        let wobble = ((i as u64).wrapping_mul(7919) ^ t.wrapping_mul(104729)) % 400;
                        if *u >= 20.0 { 5250 + wobble } else { 3000 + wobble * 3 }
                    })
                    .collect();
                self.cpu_freq_mhz = freqs.iter().sum::<u64>() / freqs.len() as u64;
                self.cpu_freq_max_mhz = freqs.iter().copied().max().unwrap_or(0);
            }
            push(&mut self.cpu_hist, self.cpu_total);
            let l = System::load_average();
            self.load = (l.one, l.five, l.fifteen);
            self.uptime = System::uptime();
        }

        if want.mem {
            self.sys.refresh_memory();
            let total = self.sys.total_memory();
            let available = self.sys.available_memory();
            self.mem = Mem {
                total,
                used: total.saturating_sub(available),
                available,
                cached: cached_bytes().unwrap_or(0),
                free: self.sys.free_memory(),
                swap_total: self.sys.total_swap(),
                swap_used: self.sys.used_swap(),
            };
        }

        if want.disks {
            let dt = since(&mut self.last_disks);
            let _guard = libc_buffers();
            self.disks_src.refresh_specifics(
                true,
                DiskRefreshKind::nothing().with_storage().with_io_usage(),
            );
            let mut seen = std::collections::HashSet::new();
            let mut list: Vec<DiskInfo> = self
                .disks_src
                .list()
                .iter()
                // One row per filesystem: btrfs subvolumes and bind mounts
                // repeat the same device.
                .filter(|d| seen.insert(d.name().to_os_string()))
                .filter(|d| d.total_space() > 0)
                // FreeBSD reports pseudo filesystems (fdescfs under
                // /compat/linux, procfs…) with a nominal size.
                .filter(|d| {
                    let fs = d.file_system().to_string_lossy();
                    !["devfs", "fdescfs", "procfs", "linprocfs", "linsysfs"].contains(&fs.as_ref())
                })
                .map(|d| {
                    let mount = d.mount_point().to_string_lossy().to_string();
                    let label = if mount == "/" {
                        "root".to_string()
                    } else {
                        mount.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(&mount).to_string()
                    };
                    let u = d.usage();
                    DiskInfo {
                        label,
                        mount,
                        total: d.total_space(),
                        used: d.total_space().saturating_sub(d.available_space()),
                        read_rate: u.read_bytes as f64 / dt,
                        write_rate: u.written_bytes as f64 / dt,
                    }
                })
                .collect();
            list.sort_by(|a, b| (a.mount != "/").cmp(&(b.mount != "/")).then(a.mount.cmp(&b.mount)));
            self.disks = list;
        }

        if want.net {
            let dt = since(&mut self.last_net);
            self.nets_src.refresh(true);
            let sel_name = self.nets.get(self.net_sel).map(|n| n.name.clone());
            let mut updated: Vec<NetInfo> = Vec::new();
            for (name, d) in self.nets_src.list() {
                if name == "lo" || name.starts_with("lo") && d.mac_address().0 == [0; 6] {
                    continue;
                }
                let mut n = self.nets.iter().find(|n| &n.name == name).cloned().unwrap_or_default();
                n.name = name.clone();
                n.rx_rate = d.received() as f64 / dt;
                n.tx_rate = d.transmitted() as f64 / dt;
                n.rx_total = d.total_received();
                n.tx_total = d.total_transmitted();
                n.ip = d
                    .ip_networks()
                    .iter()
                    .find(|ip| ip.addr.is_ipv4())
                    .or(d.ip_networks().first())
                    .map(|ip| ip.addr.to_string());
                push(&mut n.rx_hist, n.rx_rate);
                push(&mut n.tx_hist, n.tx_rate);
                updated.push(n);
            }
            updated.sort_by(|a, b| a.name.cmp(&b.name));
            self.nets = updated;
            if let Some(name) = sel_name
                && let Some(i) = self.nets.iter().position(|n| n.name == name) {
                    self.net_sel = i;
                }
            if self.net_sel >= self.nets.len() {
                self.net_sel = 0;
            }
        }

        if want.procs {
            self.read_procs();
        }
    }

    /// After an action, update our own copy of the process list so the
    /// change shows at once, without re-reading through sysinfo (an extra
    /// read would skew the next CPU% measurement).
    ///
    /// A stopped process is dropped once it has really exited.
    pub fn forget_if_gone(&mut self, pid: u32) {
        if !crate::procctl::alive(pid) {
            let before = self.procs.len();
            self.procs.retain(|p| p.pid != pid);
            self.proc_count = self.proc_count.saturating_sub(before - self.procs.len());
        }
    }

    /// A reniced process shows its new nice value.
    pub fn update_nice(&mut self, pid: u32) {
        let nice = crate::procctl::nice_of(pid);
        if let Some(p) = self.procs.iter_mut().find(|p| p.pid == pid) {
            p.nice = nice;
        }
    }

    /// A started process is listed straight away (usage fills in next tick).
    pub fn add_started(&mut self, pid: u32, command: &str) {
        if self.procs.iter().any(|p| p.pid == pid) {
            return;
        }
        let name = command
            .split_whitespace()
            .next()
            .map(|w| w.rsplit('/').next().unwrap_or(w))
            .unwrap_or("")
            .chars()
            .take(15)
            .collect();
        let me = std::process::id();
        let user = self.procs.iter().find(|p| p.pid == me).map(|p| p.user.clone()).unwrap_or_default();
        self.procs.push(ProcInfo {
            pid,
            name,
            cmd: command.trim().to_string(),
            user,
            mem: 0,
            cpu: 0.0,
            nice: crate::procctl::nice_of(pid),
        });
        self.proc_count += 1;
        sort_procs(&mut self.procs, self.proc_sort);
    }

    fn read_procs(&mut self) {
        // Without tasks: sysinfo would otherwise read and store every
        // thread of every process too (thousands on a desktop, constantly
        // created and destroyed by browsers and games). Oryn only lists
        // processes, so that work and memory were wasted, and on a busy
        // desktop the memory crept up over time.
        let kind = ProcessRefreshKind::nothing()
            .without_tasks()
            .with_memory()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_user(UpdateKind::OnlyIfNotSet);
        let _guard = libc_buffers();
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            kind.with_cpu(),
        );
        let users = &self.users;
        let names = &mut self.user_names;
        let mut list: Vec<ProcInfo> = self
            .sys
            .processes()
            .iter()
            // Skip threads (sysinfo lists each one too); keep real processes.
            .filter(|(_, p)| p.thread_kind().is_none())
            // Zombies have already exited and use nothing; they only wait
            // for their parent to collect them, and can't be stopped.
            .filter(|(_, p)| p.status() != sysinfo::ProcessStatus::Zombie)
            .map(|(pid, p)| {
                let user = p
                    .user_id()
                    .map(|uid| {
                        let key = uid.to_string();
                        names
                            .entry(key.clone())
                            .or_insert_with(|| {
                                users.get_user_by_id(uid).map(|u| u.name().to_string()).unwrap_or(key)
                            })
                            .clone()
                    })
                    .unwrap_or_default();
                let cmd = p
                    .cmd()
                    .iter()
                    .map(|a| a.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ");
                ProcInfo {
                    pid: pid_u32(pid),
                    name: p.name().to_string_lossy().to_string(),
                    cmd: if cmd.is_empty() { format!("[{}]", p.name().to_string_lossy()) } else { cmd },
                    user,
                    mem: p.memory(),
                    cpu: p.cpu_usage(),
                    nice: crate::procctl::nice_of(pid_u32(pid)),
                }
            })
            .collect();
        self.proc_count = list.len();
        sort_procs(&mut list, self.proc_sort);
        self.procs = list;
    }

    pub fn resort_procs(&mut self) {
        sort_procs(&mut self.procs, self.proc_sort);
    }

    pub fn next_net(&mut self) {
        if !self.nets.is_empty() {
            self.net_sel = (self.net_sel + 1) % self.nets.len();
        }
    }
}

/// On Linux Oryn reads clock speeds itself (see `core_freqs_mhz`), so
/// sysinfo only measures usage; elsewhere sysinfo's frequency is used.
fn cpu_refresh_kind() -> CpuRefreshKind {
    let k = CpuRefreshKind::nothing().with_cpu_usage();
    if cfg!(target_os = "linux") { k } else { k.with_frequency() }
}

/// Each core's current clock speed in MHz.
fn core_freqs_mhz(cpus: &[sysinfo::Cpu]) -> Vec<u64> {
    #[cfg(target_os = "linux")]
    {
        let _ = cpus;
        let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
        linux_core_freqs(std::path::Path::new(&root))
    }
    #[cfg(not(target_os = "linux"))]
    {
        cpus.iter().map(|c| c.frequency()).filter(|f| *f > 0).collect()
    }
}

/// The speed each core actually ran at, from the best source the kernel has:
/// 1. `cpufreq/cpuinfo_avg_freq` (Linux 6.14+): measured by the CPU's own
///    cycle counters (APERF/MPERF on x86) over the last moment.
/// 2. `cpu MHz` in /proc/cpuinfo, which on x86 is measured the same way.
/// 3. `cpufreq/scaling_cur_freq`: on newer kernels only the speed the driver
///    last asked for, which is why a CPU could seem stuck at one number.
#[cfg(target_os = "linux")]
fn linux_core_freqs(root: &std::path::Path) -> Vec<u64> {
    let cpu_dir = root.join("sys/devices/system/cpu");
    let khz = |p: std::path::PathBuf| std::fs::read_to_string(p).ok()?.trim().parse::<u64>().ok().filter(|v| *v > 0);
    let cpus: Vec<std::path::PathBuf> = {
        let mut v: Vec<(u32, std::path::PathBuf)> = std::fs::read_dir(&cpu_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_str()?.strip_prefix("cpu")?.parse::<u32>().ok()?;
                Some((n, e.path()))
            })
            .collect();
        v.sort();
        v.into_iter().map(|(_, p)| p).collect()
    };
    let avg: Vec<u64> = cpus.iter().filter_map(|c| khz(c.join("cpufreq/cpuinfo_avg_freq"))).map(|k| k / 1000).collect();
    if !avg.is_empty() {
        return avg;
    }
    let from_cpuinfo: Vec<u64> = std::fs::read_to_string(root.join("proc/cpuinfo"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == "cpu MHz").then(|| v.trim().parse::<f64>().ok()).flatten()
        })
        .filter(|m| *m > 0.0)
        .map(|m| m.round() as u64)
        .collect();
    if !from_cpuinfo.is_empty() {
        return from_cpuinfo;
    }
    cpus.iter().filter_map(|c| khz(c.join("cpufreq/scaling_cur_freq"))).map(|k| k / 1000).collect()
}

/// On FreeBSD, sysinfo's disk and process lists come from getmntinfo(3) and
/// kvm_getargv(3), which return buffers shared by the whole process, so two
/// refreshes at once (e.g. parallel tests) crash. Hold this around them.
/// Elsewhere it's a no-op.
pub fn libc_buffers() -> impl Drop {
    #[cfg(target_os = "freebsd")]
    {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
    #[cfg(not(target_os = "freebsd"))]
    {
        struct NoLock;
        impl Drop for NoLock {
            fn drop(&mut self) {}
        }
        NoLock
    }
}

fn sort_procs(list: &mut [ProcInfo], by: ProcSort) {
    match by {
        ProcSort::Cpu => list.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem))),
        ProcSort::Mem => list.sort_by(|a, b| b.mem.cmp(&a.mem)),
        ProcSort::Pid => list.sort_by_key(|p| p.pid),
        ProcSort::Name => list.sort_by_key(|a| a.name.to_lowercase()),
    }
}

fn pid_u32(pid: &Pid) -> u32 {
    pid.as_u32()
}

fn push<T>(h: &mut VecDeque<T>, v: T) {
    if h.len() == HIST {
        h.pop_front();
    }
    h.push_back(v);
}

/// Page cache size ("Cached" + reclaimable slab, like `free` and btop).
fn cached_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let get = |key: &str| {
            info.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
                .map(|kb| kb * 1024)
        };
        Some(get("Cached:")? + get("SReclaimable:").unwrap_or(0))
    }
    #[cfg(target_os = "freebsd")]
    {
        crate::freebsd::cached_bytes()
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        None
    }
}

/// "8.86 GiB", "224 KiB", "0 B" — three significant digits like btop.
pub fn human(bytes: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = bytes.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[i])
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

/// Network speed in bits ("52.0 Kibps"), shown next to the byte rate.
pub fn human_bits(bytes_per_s: f64) -> String {
    human(bytes_per_s * 8.0).replace('B', "b") + "ps"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes() {
        assert_eq!(human(0.0), "0 B");
        assert_eq!(human(1023.0), "1023 B");
        assert_eq!(human(9_513_000_000.0), "8.86 GiB");
        assert_eq!(human(229_376.0), "224 KiB");
        assert_eq!(human_bits(6656.0), "52.0 Kibps");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn clock_speed_sources() {
        let tmp = std::env::temp_dir().join(format!("oryn-freq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cpu = |n: u32| tmp.join(format!("sys/devices/system/cpu/cpu{n}/cpufreq"));
        for n in 0..2 {
            std::fs::create_dir_all(cpu(n)).unwrap();
            std::fs::write(cpu(n).join("scaling_cur_freq"), "5100000\n").unwrap();
        }
        std::fs::create_dir_all(tmp.join("sys/devices/system/cpu/cpufreq")).unwrap(); // not a cpuN
        // Only the driver's requested speed: used as the last resort.
        assert_eq!(linux_core_freqs(&tmp), [5100, 5100]);
        // /proc/cpuinfo's measured speed wins over it…
        std::fs::create_dir_all(tmp.join("proc")).unwrap();
        std::fs::write(tmp.join("proc/cpuinfo"), "processor\t: 0\ncpu MHz\t\t: 5612.345\n\nprocessor\t: 1\ncpu MHz\t\t: 2994.000\n").unwrap();
        assert_eq!(linux_core_freqs(&tmp), [5612, 2994]);
        // …and the kernel's measured average wins over both.
        std::fs::write(cpu(0).join("cpuinfo_avg_freq"), "5480000\n").unwrap();
        std::fs::write(cpu(1).join("cpuinfo_avg_freq"), "3020000\n").unwrap();
        assert_eq!(linux_core_freqs(&tmp), [5480, 3020]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn collects_something() {
        let mut s = SysStats::new();
        s.refresh(Want { cpu: true, mem: true, disks: true, net: true, procs: true });
        assert!(s.mem.total > 0);
        assert!(!s.cpu_cores.is_empty());
        assert!(s.proc_count > 0);
    }
}

/// For each logical CPU (in sysinfo's order, cpu0..N): its physical core id
/// and its L3 cache id. On AMD the L3 id identifies the CCD (chiplet).
#[derive(Clone, Copy, Default, Debug)]
pub struct CpuTopo {
    pub core_id: Option<u32>,
    pub l3_id: Option<u32>,
}

pub fn cpu_topology(n: usize) -> Vec<CpuTopo> {
    if is_demo() {
        // 9950X3D: threads 0–7 and 16–23 on CCD1, 8–15 and 24–31 on CCD2.
        return (0..n)
            .map(|i| CpuTopo { core_id: Some((i % 16) as u32), l3_id: Some(((i % 16) / 8) as u32 * 8) })
            .collect();
    }
    #[cfg(target_os = "linux")]
    {
        let root = crate::env_os("SYSFS").unwrap_or_else(|| "/".into());
        let base = std::path::Path::new(&root).join("sys/devices/system/cpu");
        let num = |p: std::path::PathBuf| {
            std::fs::read_to_string(p).ok().and_then(|v| v.trim().parse::<u32>().ok())
        };
        (0..n)
            .map(|i| CpuTopo {
                core_id: num(base.join(format!("cpu{i}/topology/core_id"))),
                l3_id: num(base.join(format!("cpu{i}/cache/index3/id"))),
            })
            .collect()
    }
    #[cfg(target_os = "freebsd")]
    {
        crate::freebsd::topology(n)
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        vec![CpuTopo::default(); n]
    }
}
