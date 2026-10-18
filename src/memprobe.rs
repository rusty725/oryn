//! `oryn --mem-probe`: find which part of Oryn keeps memory.
//!
//! Every allocation Oryn's Rust code makes goes through a small counting
//! allocator, so the exact number of bytes currently held is always known.
//! The probe runs each part of Oryn on its own a few hundred times and
//! reports how much memory that part kept per 100 runs, measured two ways:
//!
//! * Rust heap: bytes held by Oryn's own code and its Rust libraries
//!   (sysinfo, ratatui…). Exact, unaffected by allocator caching.
//! * Process memory (RssAnon): everything, including C libraries such as
//!   NVIDIA's libnvidia-api. Growth here but not in the Rust heap points at a
//!   C library.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;

/// Bytes currently allocated by Rust code.
static LIVE: AtomicIsize = AtomicIsize::new(0);

pub struct Counting;

// SAFETY: forwards every call to the system allocator unchanged; only adds
// bookkeeping of the sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_add(new_size as isize - layout.size() as isize, Ordering::Relaxed);
        }
        p
    }
}

pub fn rust_heap() -> isize {
    LIVE.load(Ordering::Relaxed)
}

/// Anonymous resident memory of this process (heap of every library), bytes.
pub fn rss_anon() -> isize {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("RssAnon:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<isize>().ok())
        })
        .map_or(0, |kb| kb * 1024)
}

/// Memory kept per 100 runs of one part, and whether that looks like a leak.
pub struct Result {
    pub name: &'static str,
    pub runs: usize,
    pub rust_per_100: f64,
    pub rss_per_100: f64,
    pub ms_per_run: f64,
}

/// Run `f` `warmup` times (to let caches and buffers reach their size), then
/// `runs` times, measuring what was kept.
pub fn measure(name: &'static str, warmup: usize, runs: usize, mut f: impl FnMut()) -> Result {
    for _ in 0..warmup {
        f();
    }
    let (h0, r0, t0) = (rust_heap(), rss_anon(), Instant::now());
    for _ in 0..runs {
        f();
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0 / runs as f64;
    let (h1, r1) = (rust_heap(), rss_anon());
    Result {
        name,
        runs,
        rust_per_100: (h1 - h0) as f64 * 100.0 / runs as f64,
        rss_per_100: (r1 - r0) as f64 * 100.0 / runs as f64,
        ms_per_run: ms,
    }
}

fn kb(b: f64) -> String {
    let k = b / 1024.0;
    if k.abs() < 0.05 { "0".into() } else { format!("{k:+.1} KB") }
}

pub fn print_header() {
    println!("Oryn memory probe — each part run on its own; memory kept per 100 runs");
    println!("(a working part keeps ~0; small noise of a few KB in process memory is the allocator)\n");
    println!("{:<34} {:>6} {:>14} {:>16} {:>9}", "part", "runs", "Rust heap", "process memory", "ms/run");
}

pub fn print(r: &Result) {
    let leak = r.rust_per_100 > 2048.0 || r.rss_per_100 > 64.0 * 1024.0;
    println!(
        "{:<34} {:>6} {:>14} {:>16} {:>9.2}{}",
        r.name,
        r.runs,
        kb(r.rust_per_100),
        kb(r.rss_per_100),
        r.ms_per_run,
        if leak { "   <-- keeps memory" } else { "" }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_leak() {
        // Other tests allocate on other threads at the same time, so only a
        // clear 1 MB-per-100-runs leak is asserted.
        let mut kept: Vec<Vec<u8>> = Vec::new();
        let leaky = measure("leaky", 5, 100, || kept.push(vec![1u8; 100_000]));
        assert!(leaky.rust_per_100 > 5_000_000.0, "{}", leaky.rust_per_100);
        assert!(rust_heap() > 10_000_000);
    }
}
