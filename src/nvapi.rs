//! NVIDIA hotspot and VRAM temperatures through NvAPI (`libnvidia-api.so.1`,
//! shipped with NVIDIA's Linux driver).
//!
//! NVIDIA doesn't expose these through nvidia-smi/NVML for GeForce cards, but
//! its NvAPI library has undocumented calls that do:
//!
//! * `GPU_ThermChannelGetInfo` / `GetStatus`: the card's temperature channels,
//!   including the "hotspot" and "memory" channels (pre-Blackwell cards).
//! * `GPU_RegisterOp` (read only): RTX 50 / Blackwell no longer has a hotspot
//!   channel; its aggregated hotspot sits in register 0xad0aa0. GDDR6X/GDDR7
//!   cards also report each memory chip's temperature in memory-controller
//!   registers, which we read and take the maximum of.
//!
//! The call IDs, structure layouts, register offsets and decoding are ported
//! from LACT (https://github.com/ilya-zlobintsev/LACT, MIT license,
//! lact-daemon/src/server/gpu_controller/nvidia/nvapi.rs), and the RTX 50 hotspot
//! scan from nvidia-gpu-sensors (https://github.com/philipl/nvidia-gpu-sensors,
//! MIT license); see THIRD_PARTY_NOTICES.md for both notices. Only reads are
//! performed. Everything is optional: if the library or a call is missing or
//! fails, the reading is simply absent, and `--no-nvapi` turns it off.
//!
//! The register decoding (`read_registers`) works on any `Regs`; FreeBSD,
//! which has no NvAPI, reads the same registers from a mapping of BAR0.

#![allow(unsafe_op_in_unsafe_fn)]
#![cfg_attr(target_os = "freebsd", allow(dead_code))]

use std::ffi::{c_char, c_void, CStr};
use std::mem::{size_of, transmute};
use std::ptr;

type Handle = *mut c_void;
type Status = i32;

const LIBRARY: &str = "libnvidia-api.so.1";
const MAX_GPUS: usize = 64;
const SHORT_STRING_MAX: usize = 64;

const Q_INITIALIZE: u32 = 0x0150e828;
const Q_UNLOAD: u32 = 0xd22bdd7e;
const Q_ENUM_PHYSICAL_GPUS: u32 = 0xe5ac921f;
const Q_GPU_GET_BUS_ID: u32 = 0x1be0b8e5;
const Q_GET_ERROR_MESSAGE: u32 = 0x6c2d048c;
const Q_THERM_CHANNEL_GET_INFO: u32 = 0x0bc8163d;
const Q_THERM_CHANNEL_GET_STATUS: u32 = 0x65fe3aad;
const Q_GPU_REGISTER_OP: u32 = 0x2eb3c140;

const REG_BLACKWELL_HOTSPOT: u32 = 0xad0aa0;
/// Chip id register; (value >> 20) is the chip, 0x1B0–0x1BF = Blackwell GB20x.
const REG_PMC_BOOT_0: u32 = 0x0;
/// NV_THERM on-die sensor window scanned for the hotspot.
const THERM_SCAN_FIRST: u32 = 0xAD0A80;
const THERM_SCAN_LAST: u32 = 0xAD0AFC;
const THERM_TEMP_MAX: f32 = 130.0;
const REG_GDDR_TEMP_DATA_BASE: u32 = 0x009024C0;
const REG_GDDR_TEMP_STATUS_BASE: u32 = 0x009024D0;
/// Bit 22 set: chips on both sides of the board (clamshell).
const REG_GDDR_CLAMSHELL: u32 = 0x00900200;

const REG_OP_READ: u16 = 1;
const REG_OP_32BIT: u16 = 4;
const REG_OP_TYPE_GLOBAL: u16 = 16;

const CHANNEL_HOTSPOT: usize = 1;
const CHANNEL_MEMORY: usize = 3;

const fn version<T>(v: usize) -> u32 {
    (size_of::<T>() | (v << 16)) as u32
}

#[repr(C)]
struct Thermals {
    version: u32,
    mask: i32,
    rsvd: [u8; 32],
    temps: [i32; 32],
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct ThermChannel {
    class: u32,
    channel_type: u32,
    rel_loc: u32,
    target_gpu: u32,
    scaling: i32,
    offset_sw: i32,
    min_temp: i32,
    max_temp: i32,
    is_temp_sim_supported: u8,
    flags: u8,
    offset_hw: i32,
    rsvd: [u8; 28],
    data: [u8; 16],
}

#[repr(C)]
struct ThermChannelInfo {
    version: u32,
    mask: i32,
    rsvd: [u8; 32],
    channels: [ThermChannel; 32],
    primary_channel_idx: [u8; 5],
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct RegisterOp {
    flags: u16,
    status: u16,
    offset: u32,
    write_mask: u64,
    value: u64,
}

#[repr(C)]
struct RegisterOpData {
    version: u32,
    op_count: u32,
    op: [RegisterOp; 256],
}

/// What we learned about one GPU when NvAPI was loaded, plus facts found on
/// the first read that can't change while Oryn runs (chip type, memory
/// layout, whether the hotspot registers may be read). Learning them once
/// keeps each refresh down to the calls that fetch new temperatures:
/// repeating the others every second is wasted work, and NVIDIA's library
/// was seen to keep memory when a refused read is retried every refresh.
struct Gpu {
    bus: u32,
    handle: Handle,
    mask: i32,
    hotspot_ch: Option<usize>,
    vram_ch: Option<usize>,
    regs: RegState,
}

#[derive(Clone, Debug, PartialEq)]
enum HotspotSource {
    /// Registers in the NV_THERM window that carry temperatures.
    Window(Vec<u32>),
    /// LACT's single aggregated register.
    Single,
    /// The driver refuses the reads (they need root): don't ask again.
    Refused,
    /// Nothing readable on this card.
    Nothing,
}

#[derive(Clone, Debug, PartialEq)]
struct Chip {
    label: String,
    register: u32,
    /// Bit offset of the chip's byte in the register (16 = front, 24 = back).
    shift: u32,
}

#[derive(Default, Clone, Debug)]
pub struct Temps {
    pub hotspot: Option<f32>,
    pub vram: Option<f32>,
    /// Individual memory chip readings (GDDR6X/GDDR7 register path).
    pub vram_chips: Vec<(String, f32)>,
    pub error: Option<String>,
    /// Why hotspot is missing, short enough for the panel ("needs root"…).
    pub hotspot_note: Option<&'static str>,
}

pub struct NvApi {
    lib: libloading::Library,
    gpus: Vec<Gpu>,
    /// nvapi_QueryInterface, looked up once.
    qi: Option<unsafe extern "C" fn(u32) -> *const ()>,
    /// NvAPI calls already resolved through it.
    resolved: std::cell::RefCell<Vec<(u32, *const ())>>,
}

impl NvApi {
    /// Load the library and find the GPUs. Err explains why it isn't usable.
    pub fn load() -> Result<Self, String> {
        Self::load_from(LIBRARY)
    }

    /// `load` with another library name, so tests can ask for one that
    /// doesn't exist whether or not this machine has NVIDIA's driver.
    fn load_from(library: &str) -> Result<Self, String> {
        let lib = unsafe { libloading::Library::new(library) }
            .map_err(|e| format!("{library} not available ({e})"))?;
        let mut api = Self { lib, gpus: Vec::new(), qi: None, resolved: Default::default() };
        api.qi = unsafe {
            api.lib
                .get::<unsafe extern "C" fn(u32) -> *const ()>(b"nvapi_QueryInterface\0")
                .map(|f| *f)
                .map_err(|e| format!("nvapi_QueryInterface missing ({e})"))?
        }
        .into();
        unsafe {
            let init: unsafe extern "C" fn() -> Status = transmute(api.query(Q_INITIALIZE)?);
            api.check(init())?;

            let enum_fn: unsafe extern "C" fn(*mut [Handle; MAX_GPUS], *mut u32) -> Status =
                transmute(api.query(Q_ENUM_PHYSICAL_GPUS)?);
            let mut handles: [Handle; MAX_GPUS] = [ptr::null_mut(); MAX_GPUS];
            let mut count = 0u32;
            api.check(enum_fn(&mut handles, &mut count))?;

            let bus_fn: unsafe extern "C" fn(Handle, *mut u32) -> Status =
                transmute(api.query(Q_GPU_GET_BUS_ID)?);
            for &handle in handles.iter().take(count as usize) {
                let mut bus = 0u32;
                if bus_fn(handle, &mut bus) != 0 {
                    continue;
                }
                let mut gpu = Gpu {
                    bus,
                    handle,
                    mask: 0,
                    hotspot_ch: None,
                    vram_ch: None,
                    regs: RegState::default(),
                };
                // Which channels exist and which are the hotspot / memory ones.
                let mut info = ThermChannelInfo {
                    version: version::<ThermChannelInfo>(2),
                    mask: 0,
                    rsvd: [0; 32],
                    channels: [ThermChannel::default(); 32],
                    primary_channel_idx: [u8::MAX; 5],
                };
                if api.gpu_query(handle, &mut info, Q_THERM_CHANNEL_GET_INFO).is_ok() {
                    let primary = |t: usize| {
                        let c = *info.primary_channel_idx.get(t)? as usize;
                        (c < 32 && info.mask & (1 << c) != 0).then_some(c)
                    };
                    gpu.mask = info.mask;
                    gpu.hotspot_ch = primary(CHANNEL_HOTSPOT);
                    gpu.vram_ch = primary(CHANNEL_MEMORY);
                }
                api.gpus.push(gpu);
            }
        }
        if api.gpus.is_empty() {
            return Err("NvAPI found no GPUs".into());
        }
        Ok(api)
    }

    pub fn read(&mut self, bus: u32, blackwell: bool, gddr_regs: bool) -> Temps {
        let Some(i) = self.gpus.iter().position(|g| g.bus == bus) else {
            return Temps { error: Some(format!("NvAPI has no GPU on bus {bus:#x}")), ..Default::default() };
        };
        let mut t = Temps::default();
        let (handle, mask, hot_ch, vram_ch) = {
            let g = &self.gpus[i];
            (g.handle, g.mask, g.hotspot_ch, g.vram_ch)
        };
        if mask != 0 {
            let mut th = Thermals { version: version::<Thermals>(2), mask, rsvd: [0; 32], temps: [0; 32] };
            match unsafe { self.gpu_query(handle, &mut th, Q_THERM_CHANNEL_GET_STATUS) } {
                Ok(()) => {
                    let get = |c: Option<usize>| {
                        let v = th.temps.get(c?)? / 256;
                        (v > 0 && v < 255).then_some(v as f32)
                    };
                    t.hotspot = get(hot_ch);
                    t.vram = get(vram_ch);
                }
                Err(e) => t.error = Some(e),
            }
        }
        let mut state = std::mem::take(&mut self.gpus[i].regs);
        read_registers(&ApiRegs { api: self, handle }, &mut state, blackwell, gddr_regs, &mut t);
        self.gpus[i].regs = state;
        t
    }

    /// Several 32-bit register reads in one driver call.
    unsafe fn read_regs(&self, handle: Handle, offsets: &[u32]) -> Result<Vec<u64>, String> {
        let f: unsafe extern "C" fn(Handle, *mut RegisterOpData) -> Status =
            transmute(self.query(Q_GPU_REGISTER_OP)?);
        let mut ops = [RegisterOp::default(); 256];
        for (op, &offset) in ops.iter_mut().zip(offsets.iter().take(256)) {
            *op = RegisterOp { offset, flags: REG_OP_READ | REG_OP_32BIT | REG_OP_TYPE_GLOBAL, ..Default::default() };
        }
        let n = offsets.len().min(256);
        let mut data = RegisterOpData { version: version::<RegisterOpData>(1), op_count: n as u32, op: ops };
        self.check(f(handle, &mut data))?;
        // A non-zero per-op status means that register was refused.
        if data.op[..n].iter().all(|o| o.status != 0) {
            return Err("register reads refused".into());
        }
        Ok(data.op[..n].iter().map(|o| if o.status == 0 { o.value } else { 0 }).collect())
    }

    unsafe fn read_reg(&self, handle: Handle, offset: u32) -> Result<u64, String> {
        let f: unsafe extern "C" fn(Handle, *mut RegisterOpData) -> Status =
            transmute(self.query(Q_GPU_REGISTER_OP)?);
        let mut ops = [RegisterOp::default(); 256];
        ops[0] = RegisterOp { offset, flags: REG_OP_READ | REG_OP_32BIT | REG_OP_TYPE_GLOBAL, ..Default::default() };
        let mut data = RegisterOpData { version: version::<RegisterOpData>(1), op_count: 1, op: ops };
        self.check(f(handle, &mut data))?;
        Ok(data.op[0].value)
    }

    unsafe fn gpu_query<T>(&self, handle: Handle, data: &mut T, id: u32) -> Result<(), String> {
        let f: unsafe extern "C" fn(Handle, *mut T) -> Status = transmute(self.query(id)?);
        self.check(f(handle, data))
    }

    /// The function for NvAPI call `id`, resolved once and remembered.
    unsafe fn query(&self, id: u32) -> Result<*const (), String> {
        if let Some((_, p)) = self.resolved.borrow().iter().find(|(i, _)| *i == id) {
            return Ok(*p);
        }
        let qi = self.qi.ok_or("nvapi_QueryInterface missing")?;
        let p = qi(id);
        if p.is_null() {
            return Err(format!("NvAPI call {id:#x} not available in this driver"));
        }
        self.resolved.borrow_mut().push((id, p));
        Ok(p)
    }

    /// Ok, or the error code. The driver's text for it is fetched only by
    /// `error_message`, not on every failed read.
    unsafe fn check(&self, status: Status) -> Result<(), String> {
        if status == 0 { Ok(()) } else { Err(format!("NvAPI error {status}")) }
    }

    /// The driver's own description of an error, for `--gpu-info`.
    pub fn error_message(&self, status: Status) -> String {
        unsafe { self.check_verbose(status) }.err().unwrap_or_default()
    }

    unsafe fn check_verbose(&self, status: Status) -> Result<(), String> {
        if status == 0 {
            return Ok(());
        }
        let msg = self.query(Q_GET_ERROR_MESSAGE).ok().map(|p| {
            let f: unsafe extern "C" fn(Status, *mut [c_char; SHORT_STRING_MAX]) -> Status = transmute(p);
            let mut text = [0 as c_char; SHORT_STRING_MAX];
            if f(status, &mut text) == 0 {
                CStr::from_ptr(text.as_ptr()).to_string_lossy().to_string()
            } else {
                String::new()
            }
        });
        Err(format!("NvAPI error {status}: {}", msg.unwrap_or_default()))
    }
}

impl Drop for NvApi {
    fn drop(&mut self) {
        unsafe {
            if let Ok(p) = self.query(Q_UNLOAD) {
                let unload: unsafe extern "C" fn() -> Status = transmute(p);
                unload();
            }
        }
    }
}

/// 32-bit reads of GPU registers (offsets into BAR0): through NvAPI's
/// register op on Linux, through a /dev/mem mapping of BAR0 on FreeBSD.
pub trait Regs {
    fn read_regs(&self, offsets: &[u32]) -> Result<Vec<u64>, String>;
    fn read_reg(&self, offset: u32) -> Result<u64, String> {
        self.read_regs(&[offset]).map(|v| v[0])
    }
}

/// NvAPI's register op for one GPU.
struct ApiRegs<'a> {
    api: &'a NvApi,
    handle: Handle,
}

impl Regs for ApiRegs<'_> {
    fn read_regs(&self, offsets: &[u32]) -> Result<Vec<u64>, String> {
        unsafe { self.api.read_regs(self.handle, offsets) }
    }
    fn read_reg(&self, offset: u32) -> Result<u64, String> {
        unsafe { self.api.read_reg(self.handle, offset) }
    }
}

/// What the registers taught us about one GPU on the first read, which
/// can't change while Oryn runs (chip type, hotspot slots, memory layout).
#[derive(Default)]
pub struct RegState {
    /// RTX 50 (Blackwell), from the chip-id register; None until first read.
    blackwell: Option<bool>,
    /// Where its RTX 50 hotspot comes from; None until first read.
    hotspot: Option<HotspotSource>,
    /// Its GDDR6X/GDDR7 chips (label, data register, which byte); None until
    /// first read.
    chips: Option<Vec<Chip>>,
}

/// The register-based readings into `t`: the RTX 50 hotspot and the
/// GDDR6X/GDDR7 chip temperatures. `blackwell` (a guess from the PCI id) is
/// used only if the chip-id register can't be read; `gddr_regs` enables
/// per-chip readings.
pub fn read_registers(r: &dyn Regs, st: &mut RegState, blackwell: bool, gddr_regs: bool, t: &mut Temps) {
    // RTX 50 has no hotspot channel. The chip-id register (readable by
    // anyone) tells; the PCI-id guess is the fallback. Once.
    if st.blackwell.is_none() {
        let chip = match r.read_reg(REG_PMC_BOOT_0) {
            Ok(boot0) if boot0 != 0 && boot0 != u32::MAX as u64 => is_blackwell_chip(boot0 as u32),
            _ => blackwell,
        };
        st.blackwell = Some(chip);
    }
    // On RTX 50 the sensor window is the real hotspot (as in LACT); keep a
    // channel value only if the window yields nothing.
    if st.blackwell == Some(true) {
        let source = st.hotspot.get_or_insert_with(|| find_hotspot(r)).clone();
        match source {
            HotspotSource::Window(offsets) => match r.read_regs(&offsets) {
                Ok(words) => {
                    if let Some(v) = words.iter().filter_map(|w| therm_temp(*w as u32)).reduce(f32::max) {
                        t.hotspot = Some(v);
                    }
                }
                Err(e) => t.error = Some(e),
            },
            HotspotSource::Single => match r.read_reg(REG_BLACKWELL_HOTSPOT) {
                Ok(raw) => {
                    let v = (raw & 0xFFFF) as f32 / 256.0;
                    if (1.0..=THERM_TEMP_MAX).contains(&v) {
                        t.hotspot = Some(v);
                    }
                }
                Err(e) => t.error = Some(e),
            },
            HotspotSource::Refused if t.hotspot.is_none() => {
                // The driver only lets root read the NV_THERM block.
                t.hotspot_note = Some(if is_root() { "read failed" } else { "needs root" });
                t.error = Some("hotspot registers refused (they need root)".into());
            }
            HotspotSource::Nothing if t.hotspot.is_none() => t.hotspot_note = Some("no sensor"),
            _ => {}
        }
    }
    if gddr_regs {
        let chips = st.chips.get_or_insert_with(|| find_chips(r));
        if !chips.is_empty() {
            // Chips can share a register (front and back on clamshell
            // boards): read each register once, in one call.
            let mut regs: Vec<u32> = chips.iter().map(|c| c.register).collect();
            regs.dedup();
            if let Ok(words) = r.read_regs(&regs) {
                let decode = |raw: u64| (2 * raw.min(0x50)) as f32 - 40.0;
                for c in chips.iter() {
                    let Some(w) = regs.iter().position(|r| *r == c.register).map(|k| words[k]) else { continue };
                    let byte = (w >> c.shift) & 0xFF;
                    if byte != 0 && byte != 0xFF {
                        t.vram_chips.push((c.label.clone(), decode(byte)));
                    }
                }
            }
        }
        if t.vram.is_none() {
            t.vram = t.vram_chips.iter().map(|c| c.1).reduce(f32::max);
        }
    }
}

/// Where an RTX 50's hotspot can be read, worked out once: the registers
/// in the NV_THERM sensor window that carry the 0x4000 temperature tag
/// (which slots are populated differs between Blackwell models), else
/// LACT's single aggregated register. Scan bounds and decoding from
/// nvidia-gpu-sensors (MIT, Philip Langdale).
fn find_hotspot(r: &dyn Regs) -> HotspotSource {
    let offsets: Vec<u32> = (THERM_SCAN_FIRST..=THERM_SCAN_LAST).step_by(4).collect();
    match r.read_regs(&offsets) {
        Err(_) => HotspotSource::Refused,
        Ok(words) => {
            let tagged: Vec<u32> = offsets
                .iter()
                .zip(&words)
                .filter(|(_, w)| therm_temp(**w as u32).is_some())
                .map(|(o, _)| *o)
                .collect();
            if !tagged.is_empty() {
                return HotspotSource::Window(tagged);
            }
            match r.read_reg(REG_BLACKWELL_HOTSPOT) {
                Ok(raw) if (1.0..=THERM_TEMP_MAX).contains(&((raw & 0xFFFF) as f32 / 256.0)) => HotspotSource::Single,
                _ => HotspotSource::Nothing,
            }
        }
    }
}

/// The GDDR6X/GDDR7 chips on this card and the registers that report
/// them, worked out once. Labelled like LACT: partition letter + chip
/// index ("A0", "B1"…), with " F"/" B" for the front/back chip on
/// clamshell boards.
fn find_chips(r: &dyn Regs) -> Vec<Chip> {
    let clamshell = r.read_reg(REG_GDDR_CLAMSHELL).is_ok_and(|v| (v >> 22) & 1 == 1);
    let mut out = Vec::new();
    for partition in 0..=8u32 {
        let data_base = REG_GDDR_TEMP_DATA_BASE + partition * 0x4000;
        let Ok(status) = r.read_reg(REG_GDDR_TEMP_STATUS_BASE + partition * 0x4000) else {
            continue;
        };
        if status & 0xFFFF0000 == 0xBADF0000 {
            continue; // unpopulated partition
        }
        let letter = (b'A' + partition as u8) as char;
        let mut i = 0;
        for pair in [[(0x0, 24), (0x8, 26)], [(0x4, 25), (0xC, 27)]] {
            for (slot, bit) in pair {
                if (status >> bit) & 1 != 1 {
                    continue;
                }
                let register = data_base + slot;
                let Ok(data) = r.read_reg(register) else { continue };
                if data == 0 || data == u32::MAX as u64 || data & 0xFFFF0000 == 0xBADF0000 {
                    continue;
                }
                let (front, back) = ((data >> 16) & 0xFF, (data >> 24) & 0xFF);
                if front == 0 || front == 0xFF {
                    continue;
                }
                let label = format!("{letter}{i}");
                if clamshell {
                    out.push(Chip { label: format!("{label} F"), register, shift: 16 });
                    if back != 0 && back != 0xFF {
                        out.push(Chip { label: format!("{label} B"), register, shift: 24 });
                    }
                } else {
                    out.push(Chip { label, register, shift: 16 });
                }
                i += 1;
                break;
            }
        }
    }
    out
}

static DISABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--no-nvapi`: never load the library.
pub fn disable() {
    DISABLED.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn disabled() -> bool {
    DISABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// PCI bus number from nvidia-smi's "00000000:01:00.0" or sysfs "0000:01:00.0".
pub fn bus_number(bus_id: &str) -> Option<u32> {
    let mut parts = bus_id.split(':');
    let _domain = parts.next()?;
    u32::from_str_radix(parts.next()?, 16).ok()
}

fn is_blackwell_chip(boot0: u32) -> bool {
    (0x1B0..=0x1BF).contains(&(boot0 >> 20))
}

/// An NV_THERM temperature word: upper half tagged 0x4000, value/256 °C.
fn therm_temp(w: u32) -> Option<f32> {
    if w >> 16 != 0x4000 {
        return None;
    }
    let b1 = (w >> 8) & 0xFF;
    if b1 == 0 || b1 == 0xFF {
        return None;
    }
    let t = (w & 0xFFFF) as f32 / 256.0;
    (t <= THERM_TEMP_MAX).then_some(t)
}

pub fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// RTX 50 / Blackwell consumer chips (GB20x) use PCI device IDs 0x2b00–0x2fff.
pub fn is_blackwell(device_id: u16) -> bool {
    (0x2b00..=0x2fff).contains(&device_id)
}

/// Cards whose memory is GDDR6X/GDDR7 (per-chip registers apply): RTX 30
/// upper models (GA102/GA104 with GDDR6X), RTX 40 GDDR6X models, RTX 50.
/// Wrong guesses are harmless: the registers then report "no data".
pub fn has_gddr_regs(device_id: u16) -> bool {
    is_blackwell(device_id) || (0x2200..=0x22ff).contains(&device_id) || (0x2480..=0x24ff).contains(&device_id)
        || (0x2680..=0x27ff).contains(&device_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_lact() {
        // Sizes go into each struct's version field; NvAPI rejects a mismatch.
        assert_eq!(size_of::<ThermChannel>(), 4 * 8 + 2 + 2 /*pad*/ + 4 + 28 + 16);
        assert_eq!(size_of::<Thermals>(), 4 + 4 + 32 + 4 * 32);
        assert_eq!(size_of::<RegisterOp>(), 2 + 2 + 4 + 8 + 8);
        assert_eq!(size_of::<RegisterOpData>(), 4 + 4 + 24 * 256);
        assert_eq!(size_of::<ThermChannelInfo>(), 4 + 4 + 32 + 84 * 32 + 5 + 3 /*pad*/);
    }

    #[test]
    fn bus_and_arch() {
        assert_eq!(bus_number("00000000:01:00.0"), Some(1));
        assert_eq!(bus_number("0000:2b:00.0"), Some(0x2b));
        assert!(is_blackwell(0x2c02)); // RTX 5080
        assert!(!is_blackwell(0x2684)); // RTX 4090
        assert!(is_blackwell_chip(0x1B300000)); // GB203
        assert!(!is_blackwell_chip(0x19200000)); // AD102
    }

    #[test]
    fn therm_words() {
        assert_eq!(therm_temp(0x4000_3D80), Some(61.5)); // tagged 61.5 °C
        assert_eq!(therm_temp(0x0000_3D80), None); // untagged
        assert_eq!(therm_temp(0xBADF_5040), None); // poison marker
        assert_eq!(therm_temp(0x4000_0000), None); // empty slot
    }

    #[test]
    fn missing_library_is_an_error_not_a_crash() {
        let err = NvApi::load_from("libnvidia-api-oryn-test-missing.so.1").err().unwrap();
        assert!(err.contains("not available"), "{err}");
    }
}
