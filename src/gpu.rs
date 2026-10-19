//! NVIDIA GPU temperatures via `nvidia-smi`.
//!
//! NVIDIA's proprietary driver doesn't publish temperatures through Linux
//! hwmon (AMD's `amdgpu`, Intel's `xe` and the open `nouveau` driver do, and
//! those are read by the sysfs source). So for NVIDIA we start ONE long-lived
//! `nvidia-smi --query-gpu=… -lms <interval>` process that prints a CSV line
//! per GPU every interval, and read it on a background thread. This works the
//! same on Linux and Windows and needs no root.
//!
//! Slowdown/shutdown limits come from a single `nvidia-smi -q -d TEMPERATURE`
//! at startup and are used as the "crit" threshold.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::sensors::{Reading, Source};

#[derive(Clone, Default)]
pub struct Gpu {
    pub name: String,
    pub bus: String,
    pub core: Option<f32>,
    /// HBM memory temperature: data-center cards only; GeForce reports N/A.
    pub memory: Option<f32>,
    pub slowdown: Option<f32>,
    pub mem_max: Option<f32>,
    // Load figures for the GPU panel (None when the driver reports N/A).
    pub util: Option<f32>,
    pub vram_used_mib: Option<f32>,
    pub vram_total_mib: Option<f32>,
    pub power_w: Option<f32>,
    pub power_limit_w: Option<f32>,
    pub fan_pct: Option<f32>,
    pub core_mhz: Option<f32>,
    pub mem_mhz: Option<f32>,
}

/// The latest NVIDIA readings, shared with the GPU panel.
pub static SHARED: std::sync::OnceLock<Arc<Mutex<Vec<Gpu>>>> = std::sync::OnceLock::new();

pub struct NvidiaPoller {
    gpus: Arc<Mutex<Vec<Gpu>>>,
    stopped: Arc<Mutex<Option<String>>>,
}

fn exe() -> String {
    crate::env_str("NVIDIA_SMI").unwrap_or_else(|| "nvidia-smi".into())
}

/// Temperatures plus load figures. Very old drivers reject unknown fields and
/// print nothing, in which case the poller falls back to FIELDS_BASIC.
const FIELDS: &str = "index,name,pci.bus_id,temperature.gpu,temperature.memory,utilization.gpu,memory.used,memory.total,power.draw,power.limit,fan.speed,clocks.gr,clocks.mem";
const FIELDS_BASIC: &str = "index,name,pci.bus_id,temperature.gpu,temperature.memory";

fn num(s: &str) -> Option<f32> {
    s.trim().parse::<f32>().ok().filter(|t| t.is_finite() && *t > -40.0 && *t < 150.0)
}

/// Any finite non-negative number ("N/A", "[Not Supported]" -> None).
fn any(s: Option<&&str>) -> Option<f32> {
    s?.trim().parse::<f32>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

/// One CSV line: "0, NVIDIA GeForce RTX 4090, 00000000:01:00.0, 54, N/A, 7, 1234, 24564, 35.2, 450.00, 30, 210, 405"
fn parse_line(line: &str) -> Option<(usize, Gpu)> {
    let f: Vec<&str> = line.split(',').map(str::trim).collect();
    if f.len() < 4 {
        return None;
    }
    let idx = f[0].parse::<usize>().ok()?;
    Some((
        idx,
        Gpu {
            name: f[1].to_string(),
            bus: f[2].to_string(),
            core: num(f[3]),
            memory: f.get(4).and_then(|m| num(m)),
            util: any(f.get(5)).map(|u| u.min(100.0)),
            vram_used_mib: any(f.get(6)),
            vram_total_mib: any(f.get(7)),
            power_w: any(f.get(8)),
            power_limit_w: any(f.get(9)),
            fan_pct: any(f.get(10)),
            core_mhz: any(f.get(11)),
            mem_mhz: any(f.get(12)),
            ..Default::default()
        },
    ))
}

/// Start `nvidia-smi --query-gpu=<fields> -lms <ms>`, printing CSV lines.
fn spawn_stream(fields: &str, ms: &str) -> Option<(std::process::Child, std::process::ChildStdout)> {
    let mut child = Command::new(exe())
        .args([&format!("--query-gpu={fields}"), "--format=csv,noheader,nounits", "-lms", ms])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    Some((child, stdout))
}

/// Per-GPU thresholds from `nvidia-smi -q -d TEMPERATURE`, in GPU order.
/// Looks for "GPU Slowdown Temp : 87 C" and "Memory Max Operating Temp : 95 C".
fn parse_limits(text: &str) -> Vec<(Option<f32>, Option<f32>)> {
    let mut out = Vec::new();
    for block in text.split("\nGPU ").skip(1) {
        let get = |key: &str| {
            block.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                (k.trim() == key).then(|| num(v.trim().trim_end_matches('C')))?
            })
        };
        out.push((get("GPU Slowdown Temp"), get("Memory Max Operating Temp")));
    }
    out
}

impl NvidiaPoller {
    /// None if nvidia-smi isn't installed or reports no GPUs.
    pub fn new(interval: Duration) -> Option<Self> {
        // Quick presence check; `-L` lists GPUs and exits.
        let list = Command::new(exe()).arg("-L").stderr(Stdio::null()).output().ok()?;
        if !list.status.success() || !String::from_utf8_lossy(&list.stdout).contains("GPU ") {
            return None;
        }
        let limits = Command::new(exe())
            .args(["-q", "-d", "TEMPERATURE"])
            .stderr(Stdio::null())
            .output()
            .map(|o| parse_limits(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default();

        let gpus: Arc<Mutex<Vec<Gpu>>> = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(Mutex::new(None));
        let ms = interval.as_millis().clamp(200, 10_000).to_string();
        let (mut child, mut stdout) = spawn_stream(FIELDS, &ms)?;

        let (g, s) = (Arc::clone(&gpus), Arc::clone(&stopped));
        thread::spawn(move || {
            let mut latest: HashMap<usize, Gpu> = HashMap::new();
            let mut tried_basic = false;
            loop {
                let mut got_any = false;
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let Some((idx, mut gpu)) = parse_line(&line) else { continue };
                    got_any = true;
                    if let Some((slow, mem)) = limits.get(idx) {
                        gpu.slowdown = *slow;
                        gpu.mem_max = *mem;
                    }
                    latest.insert(idx, gpu);
                    let mut v: Vec<(usize, Gpu)> = latest.iter().map(|(i, g)| (*i, g.clone())).collect();
                    v.sort_by_key(|(i, _)| *i);
                    *g.lock().unwrap() = v.into_iter().map(|(_, g)| g).collect();
                }
                let _ = child.wait();
                // Ended without a single line: this driver rejected a field.
                // Retry once with temperatures only.
                if !got_any && !tried_basic {
                    tried_basic = true;
                    if let Some((c, out)) = spawn_stream(FIELDS_BASIC, &ms) {
                        (child, stdout) = (c, out);
                        continue;
                    }
                }
                break;
            }
            // nvidia-smi exited (driver reload, GPU fell off the bus, …).
            // Keep the cards listed, but drop their last figures so the GPU
            // panel and header don't keep showing them as if live.
            for gpu in g.lock().unwrap().iter_mut() {
                *gpu = Gpu {
                    name: std::mem::take(&mut gpu.name),
                    bus: std::mem::take(&mut gpu.bus),
                    slowdown: gpu.slowdown,
                    mem_max: gpu.mem_max,
                    ..Default::default()
                };
            }
            *s.lock().unwrap() = Some("nvidia-smi stopped".into());
        });
        let _ = SHARED.set(Arc::clone(&gpus));
        Some(Self { gpus, stopped })
    }
}

impl Source for NvidiaPoller {
    fn read(&mut self) -> Vec<Reading> {
        let note = self.stopped.lock().unwrap().clone();
        let gpus = self.gpus.lock().unwrap().clone();
        let mut out = Vec::new();
        for (i, g) in gpus.iter().enumerate() {
            // Two cards of the same model get their index appended.
            let same = gpus.iter().filter(|x| x.name == g.name).count() > 1;
            let device = if same { format!("{} #{i}", g.name) } else { g.name.clone() };
            let id = if g.bus.is_empty() { format!("nvidia:{i}") } else { format!("nvidia:{}", g.bus) };
            out.push(Reading {
                id: format!("{id}:core"),
                label: "nvidia GPU core".into(),
                device: device.clone(),
                temp: if note.is_some() { None } else { g.core },
                critical: g.slowdown.or(Some(90.0)),
                cpu: false,
                kind: "GPU",
                note: note.clone(),
            });
            if g.memory.is_some() {
                out.push(Reading {
                    id: format!("{id}:mem"),
                    label: "nvidia GPU memory".into(),
                    device,
                    temp: if note.is_some() { None } else { g.memory },
                    critical: g.mem_max.or(Some(95.0)),
                    cpu: false,
                    kind: "GPU",
                    note: note.clone(),
                });
            }
        }
        out
    }
    fn settle(&self) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5)
            && self.gpus.lock().unwrap().is_empty()
            && self.stopped.lock().unwrap().is_none()
        {
            thread::sleep(Duration::from_millis(50));
        }
    }
    fn name(&self) -> &'static str {
        "nvidia-smi"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_lines() {
        let (i, g) = parse_line("0, NVIDIA GeForce RTX 4090, 00000000:01:00.0, 54, N/A").unwrap();
        assert_eq!(i, 0);
        assert_eq!(g.name, "NVIDIA GeForce RTX 4090");
        assert_eq!(g.core, Some(54.0));
        assert_eq!(g.memory, None);
        let (_, g) = parse_line("1, NVIDIA H100 80GB HBM3, 00000000:41:00.0, 38, 46").unwrap();
        assert_eq!(g.memory, Some(46.0));
        assert!(parse_line("No devices were found").is_none());
        let (_, g) = parse_line("0, NVIDIA GeForce RTX 5090, 00000000:01:00.0, 41, N/A, 37, 2345, 32607, 88.50, 575.00, 30, 2407, 14001").unwrap();
        assert_eq!(g.util, Some(37.0));
        assert_eq!(g.vram_total_mib, Some(32607.0));
        assert_eq!(g.power_limit_w, Some(575.0));
        assert_eq!(g.mem_mhz, Some(14001.0));
        let (_, g) = parse_line("0, Tesla T4, 00000000:01:00.0, 41, N/A, 0, 0, 15360, [N/A], 70.00, [N/A], 300, 5000").unwrap();
        assert_eq!(g.power_w, None);
        assert_eq!(g.fan_pct, None);
    }

    #[test]
    fn limits() {
        let q = "==============NVSMI LOG==============\n\nAttached GPUs : 1\nGPU 00000000:01:00.0\n    Temperature\n        GPU Current Temp                  : 54 C\n        GPU Shutdown Temp                 : 92 C\n        GPU Slowdown Temp                 : 89 C\n        GPU Max Operating Temp            : 87 C\n        Memory Max Operating Temp         : N/A\n";
        assert_eq!(parse_limits(q), vec![(Some(89.0), None)]);
    }
}
