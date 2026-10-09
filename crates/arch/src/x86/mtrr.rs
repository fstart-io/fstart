//! Physical cacheability planning and per-CPU MTRR installation.
//!
//! The BSP plans once from physical RAM, before firmware allocations split the
//! OS memory map. UC holes and explicit WC apertures are part of the solution;
//! an insufficient MSR budget is an error, never a partial RAM mapping.

use crate::x86::msr::{rdmsr, wrmsr};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use fstart_core::services::ServiceError;
use heapless::Vec;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub const IA32_MTRR_PHYSBASE0: u32 = 0x200;
pub const IA32_MTRR_PHYSMASK0: u32 = 0x201;
pub const IA32_MTRR_CAP: u32 = 0xfe;
pub const IA32_MTRR_DEF_TYPE: u32 = 0x2ff;
pub const IA32_MTRR_FIX64K_00000: u32 = 0x250;
pub const IA32_MTRR_FIX16K_80000: u32 = 0x258;
pub const IA32_MTRR_FIX16K_A0000: u32 = 0x259;
pub const IA32_MTRR_FIX4K_C0000: u32 = 0x268;
pub const MTRR_TYPE_UNCACHEABLE: u64 = 0;
pub const MTRR_TYPE_WRITE_COMBINING: u64 = 1;
pub const MTRR_TYPE_WRITE_PROTECT: u64 = 5;
pub const MTRR_TYPE_WRITE_BACK: u64 = 6;
const FIXED_ENABLE: u64 = 1 << 10;
const ENABLE: u64 = 1 << 11;
const VALID: u64 = 1 << 11;
const ONE_MIB: u64 = 0x100000;
const MAX_MTRRS: usize = 32;
const MAX_RANGES: usize = 64;
const MAX_RAM_RANGES: usize = 16;
const FIXED_MSRS: [u32; 11] = [
    0x250, 0x258, 0x259, 0x268, 0x269, 0x26a, 0x26b, 0x26c, 0x26d, 0x26e, 0x26f,
];

/// Cache types supported by the physical-map planner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CacheType {
    Uncacheable = 0,
    WriteCombining = 1,
    WriteBack = 6,
}
impl CacheType {
    fn name(self) -> &'static str {
        match self {
            Self::Uncacheable => "UC",
            Self::WriteCombining => "WC",
            Self::WriteBack => "WB",
        }
    }
}

/// CPU limits, also usable by host-side planner tests without privileged I/O.
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub variable_count: usize,
    pub physical_bits: u8,
    pub fixed: bool,
    pub write_combining: bool,
}
impl Capabilities {
    /// # Safety
    /// The current CPU must implement MTRRs.
    pub unsafe fn current() -> Self {
        let cap = unsafe { rdmsr(IA32_MTRR_CAP) };
        Self {
            variable_count: (cap & 0xff) as usize,
            physical_bits: crate::x86::physical_address_bits() as u8,
            fixed: cap & (1 << 8) != 0,
            write_combining: cap & (1 << 10) != 0,
        }
    }
    fn limit(self) -> u64 {
        1u64 << self.physical_bits
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Range {
    base: u64,
    end: u64,
    kind: CacheType,
}

/// Complete immutable solution; applying it does not run the planner again.
#[derive(Clone, Debug)]
pub struct Plan {
    caps: Capabilities,
    default: CacheType,
    ranges: Vec<Range, MAX_MTRRS>,
}

fn chunks(base: u64, size: u64) -> impl Iterator<Item = (u64, u64)> {
    let mut next = base;
    let mut remaining = size;
    core::iter::from_fn(move || {
        if remaining == 0 {
            return None;
        }
        let max_size = 1u64 << (63 - remaining.leading_zeros());
        let size = if next == 0 {
            max_size
        } else {
            max_size.min(next & next.wrapping_neg())
        };
        let base = next;
        next += size;
        remaining -= size;
        Some((base, size))
    })
}

impl Plan {
    /// Build a complete map. Variable RAM/WC ranges must be page-aligned and
    /// not overlap. With fixed MTRRs, conventional memory uses the PC fixed
    /// policy (including sub-page EBDA reservations). Unspecified memory is UC.
    pub fn build(
        caps: Capabilities,
        ram: impl IntoIterator<Item = (u64, u64)>,
        wc: impl IntoIterator<Item = (u64, u64)>,
    ) -> Result<Self, ServiceError> {
        if !(32..=52).contains(&caps.physical_bits) || caps.variable_count > MAX_MTRRS {
            return Err(ServiceError::NotSupported);
        }
        let mut occupied: Vec<Range, MAX_RANGES> = Vec::new();
        for (mut base, size, kind) in ram
            .into_iter()
            .map(|(base, size)| (base, size, CacheType::WriteBack))
            .chain(
                wc.into_iter()
                    .map(|(base, size)| (base, size, CacheType::WriteCombining)),
            )
        {
            if size == 0 {
                continue;
            }
            let end = base.checked_add(size).ok_or(ServiceError::InvalidParam)?;
            if caps.fixed && kind == CacheType::WriteBack && end <= ONE_MIB {
                continue;
            }
            if base & 0xfff != 0
                || end & 0xfff != 0
                || end > caps.limit()
                || (caps.fixed && kind == CacheType::WriteCombining && base < ONE_MIB)
            {
                return Err(ServiceError::InvalidParam);
            }
            if kind == CacheType::WriteCombining && !caps.write_combining {
                return Err(ServiceError::NotSupported);
            }
            if caps.fixed && kind == CacheType::WriteBack {
                if base <= ONE_MIB {
                    base = 0;
                }
            }
            occupied
                .push(Range { base, end, kind })
                .map_err(|_| ServiceError::NotSupported)?;
        }
        if !occupied
            .iter()
            .any(|range| range.kind == CacheType::WriteBack)
        {
            return Err(ServiceError::NoDevice);
        }
        occupied.sort_unstable_by_key(|range| range.base);
        let mut map: Vec<Range, MAX_RANGES> = Vec::new();
        let mut cursor = 0;
        for range in occupied {
            if range.base < cursor {
                return Err(ServiceError::InvalidParam);
            }
            if range.base > cursor {
                map.push(Range {
                    base: cursor,
                    end: range.base,
                    kind: CacheType::Uncacheable,
                })
                .map_err(|_| ServiceError::NotSupported)?;
            }
            if let Some(previous) = map.last_mut()
                && previous.end == range.base
                && previous.kind == range.kind
            {
                previous.end = range.end;
            } else {
                map.push(range).map_err(|_| ServiceError::NotSupported)?;
            }
            cursor = range.end;
        }
        if cursor < caps.limit() {
            map.push(Range {
                base: cursor,
                end: caps.limit(),
                kind: CacheType::Uncacheable,
            })
            .map_err(|_| ServiceError::NotSupported)?;
        }
        let uc = Self::candidate(caps, &map, CacheType::Uncacheable);
        let wb = Self::candidate(caps, &map, CacheType::WriteBack);
        match (uc, wb) {
            (Ok(uc), Ok(wb)) => Ok(if wb.ranges.len() < uc.ranges.len() {
                wb
            } else {
                uc
            }),
            (Ok(plan), Err(_)) | (Err(_), Ok(plan)) => Ok(plan),
            (Err(_), Err(_)) => Err(ServiceError::NotSupported),
        }
    }

    fn candidate(
        caps: Capabilities,
        map: &[Range],
        default: CacheType,
    ) -> Result<Self, ServiceError> {
        let mut plan = Self {
            caps,
            default,
            ranges: Vec::new(),
        };
        for (index, range) in map
            .iter()
            .enumerate()
            .filter(|(_, range)| range.kind != default)
        {
            let mut base = range.base;
            if caps.fixed && base < ONE_MIB {
                if range.end <= ONE_MIB {
                    continue;
                }
                if range.kind != CacheType::WriteBack {
                    base = ONE_MIB;
                }
            }
            let mut end = range.end;
            if default == CacheType::Uncacheable && range.kind == CacheType::WriteBack {
                let limit = map[index + 1..]
                    .iter()
                    .find(|next| next.kind != CacheType::Uncacheable)
                    .map_or(caps.limit(), |next| next.base);
                let mut cost = chunks(base, end - base).count();
                // UC carve-outs may optimize high RAM too, not just low RAM.
                for shift in range.end.trailing_zeros() + 1..=caps.physical_bits as u32 {
                    let Some(rounded) = range.end.checked_next_multiple_of(1u64 << shift) else {
                        break;
                    };
                    if rounded > limit {
                        break;
                    }
                    let next_cost = chunks(base, rounded - base).count()
                        + chunks(range.end, rounded - range.end).count();
                    if next_cost < cost {
                        cost = next_cost;
                        end = rounded;
                    }
                }
            }
            plan.add_chunks(base, end, range.kind)?;
            if end != range.end {
                plan.add_chunks(range.end, end, CacheType::Uncacheable)?;
            }
        }
        Ok(plan)
    }

    fn add_chunks(&mut self, base: u64, end: u64, kind: CacheType) -> Result<(), ServiceError> {
        for (base, size) in chunks(base, end - base) {
            if self.ranges.len() == self.caps.variable_count {
                return Err(ServiceError::NotSupported);
            }
            self.ranges
                .push(Range {
                    base,
                    end: base + size,
                    kind,
                })
                .map_err(|_| ServiceError::NotSupported)?;
        }
        Ok(())
    }

    /// Print the chosen default and every programmed variable range once.
    pub fn print(&self) {
        fstart_log::info!(
            "mtrr: solution default={} used={}/{} fixed={}",
            self.default.name(),
            self.ranges.len(),
            self.caps.variable_count,
            self.caps.fixed
        );
        if self.caps.fixed {
            fstart_log::info!(
                "mtrr: fixed 0x0..0xa0000 WB, 0xa0000..0xc0000 UC, 0xc0000..0x100000 WB"
            );
        }
        for (index, range) in self.ranges.iter().enumerate() {
            fstart_log::info!(
                "mtrr: {} {:#x}..{:#x} {}",
                index,
                range.base,
                range.end,
                range.kind.name()
            );
        }
    }

    /// # Safety
    /// BSP-only RAM-stage operation, with interrupts disabled and APs not yet
    /// running. This must never execute from CAR. APs receive the resulting MSRs.
    pub unsafe fn apply(&self) -> Result<(), ServiceError> {
        let current = unsafe { Capabilities::current() };
        if current.variable_count != self.caps.variable_count
            || current.physical_bits != self.caps.physical_bits
            || current.fixed != self.caps.fixed
            || current.write_combining != self.caps.write_combining
        {
            return Err(ServiceError::NotSupported);
        }
        unsafe {
            // Flush dirty RAM before CD=1, never afterwards (i945/Atom).
            if !x86::controlregs::cr0().contains(x86::controlregs::Cr0::CR0_CACHE_DISABLE) {
                crate::x86::writeback_invalidate_caches();
            }
            disable_cache();
            let cr4 = x86::controlregs::cr4();
            x86::controlregs::cr4_write(cr4 & !x86::controlregs::Cr4::CR4_ENABLE_GLOBAL_PAGES);
            let cr3 = x86::controlregs::cr3();
            x86::controlregs::cr3_write(cr3);
            wrmsr(IA32_MTRR_DEF_TYPE, 0);
            setup_fixed_low_memory();
            for index in 0..current.variable_count {
                clear_variable(index as u32);
            }
            for (index, range) in self.ranges.iter().enumerate() {
                set_variable(
                    index as u32,
                    range.base,
                    range.end - range.base,
                    range.kind as u64,
                );
            }
            wrmsr(
                IA32_MTRR_DEF_TYPE,
                self.default as u64 | ENABLE | if current.fixed { FIXED_ENABLE } else { 0 },
            );
            x86::controlregs::cr3_write(cr3);
            x86::controlregs::cr4_write(cr4);
            enable_cache();
        }
        ROM_SLOT.store(0, Ordering::Release);
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct WbRange {
    base: u64,
    size: u64,
}
// Atomics keep the safe publication API free of mutable-static references.
// Logical updates remain BSP-only and precede AP startup.
static WB_RANGES: [(AtomicU64, AtomicU64); MAX_RAM_RANGES] =
    [const { (AtomicU64::new(0), AtomicU64::new(0)) }; MAX_RAM_RANGES];
static WB_RANGE_COUNT: AtomicUsize = AtomicUsize::new(0);
static FINAL_READY: AtomicBool = AtomicBool::new(false);
// Zero means no temporary ROM slot; otherwise the owned slot plus one.
static ROM_SLOT: AtomicUsize = AtomicUsize::new(0);

/// Publish hardware RAM before firmware/table allocations split the OS map.
/// BSP-only, before MP initialization. Invalid input is rejected, not truncated.
pub fn set_ram_wb_ranges(ranges: &[(u64, u64)]) -> Result<(), ServiceError> {
    set_ram_wb_ranges_from(ranges.iter().copied())
}
pub fn set_ram_wb_ranges_from(
    ranges: impl IntoIterator<Item = (u64, u64)>,
) -> Result<(), ServiceError> {
    let mut checked: Vec<WbRange, MAX_RAM_RANGES> = Vec::new();
    for (base, size) in ranges.into_iter().filter(|(_, size)| *size != 0) {
        let end = base.checked_add(size).ok_or(ServiceError::InvalidParam)?;
        if end > ONE_MIB && (base & 0xfff != 0 || end & 0xfff != 0) {
            return Err(ServiceError::InvalidParam);
        }
        checked
            .push(WbRange { base, size })
            .map_err(|_| ServiceError::NotSupported)?;
    }
    for ((base, size), range) in WB_RANGES.iter().zip(checked.iter()) {
        base.store(range.base, Ordering::Relaxed);
        size.store(range.size, Ordering::Relaxed);
    }
    WB_RANGE_COUNT.store(checked.len(), Ordering::Release);
    FINAL_READY.store(false, Ordering::Release);
    Ok(())
}
fn ram_ranges() -> impl Iterator<Item = (u64, u64)> {
    (0..WB_RANGE_COUNT.load(Ordering::Acquire)).map(|index| {
        let (base, size) = &WB_RANGES[index];
        (base.load(Ordering::Relaxed), size.load(Ordering::Relaxed))
    })
}

/// Final BSP plan. WC is optional; RAM coverage is not.
/// # Safety
/// Same requirements as [`Plan::apply`].
pub unsafe fn install_final_solution(wc: &[(u64, u64)]) -> Result<(), ServiceError> {
    let caps = unsafe { Capabilities::current() };
    let plan = Plan::build(caps, ram_ranges(), wc.iter().copied()).or_else(|error| {
        if wc.is_empty() || error != ServiceError::NotSupported {
            return Err(error);
        }
        fstart_log::warn!("mtrr: WC apertures do not fit; preserving complete RAM coverage");
        Plan::build(caps, ram_ranges(), [])
    })?;
    plan.print();
    unsafe {
        plan.apply()?;
    }
    FINAL_READY.store(true, Ordering::Release);
    Ok(())
}

/// Bootstrap RAM caching, before PCI resources are known. Not an MP plan.
/// # Safety
/// Same requirements as [`Plan::apply`].
pub unsafe fn setup_ram_wb() -> Result<(), ServiceError> {
    let plan = Plan::build(unsafe { Capabilities::current() }, ram_ranges(), [])?;
    unsafe { plan.apply() }
}

/// Ensure the BSP solution exists even on platforms without a PCI-phase hook.
/// # Safety
/// BSP-only, with the requirements of [`Plan::apply`], before AP startup.
pub(crate) unsafe fn prepare_for_mp() -> Result<(), ServiceError> {
    if !FINAL_READY.load(Ordering::Acquire) {
        unsafe {
            install_final_solution(&[])?;
        }
    }
    unsafe { set_boot_rom_wp(false) }
}

/// Packed SIPI MSR record. The assembly consumes three little-endian u32s.
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub(crate) struct MsrEntry {
    pub index: u32,
    pub low: u32,
    pub high: u32,
}
const MAX_SIPI_MSRS: usize = 2 * MAX_MTRRS + 11 + 2;

/// Capture the final BSP state, including PAT, for APs to replay before caching.
/// # Safety
/// BSP-only, before SIPI; all CPUs must have compatible MTRR/PAT capabilities.
pub(crate) unsafe fn snapshot_for_sipi() -> Result<Vec<MsrEntry, MAX_SIPI_MSRS>, ServiceError> {
    unsafe {
        prepare_for_mp()?;
    }
    snapshot_msrs(
        unsafe { Capabilities::current() },
        crate::x86::cpuid(1).3 & (1 << 16) != 0,
        |index| unsafe { rdmsr(index) },
    )
}

fn snapshot_msrs(
    cap: Capabilities,
    pat: bool,
    mut read: impl FnMut(u32) -> u64,
) -> Result<Vec<MsrEntry, MAX_SIPI_MSRS>, ServiceError> {
    if cap.variable_count > MAX_MTRRS {
        return Err(ServiceError::NotSupported);
    }
    let mut entries = Vec::new();
    let mut save = |index| {
        let value = read(index);
        entries
            .push(MsrEntry {
                index,
                low: value as u32,
                high: (value >> 32) as u32,
            })
            .map_err(|_| ServiceError::NotSupported)
    };
    if cap.fixed {
        for index in FIXED_MSRS {
            save(index)?;
        }
    }
    for index in 0..cap.variable_count as u32 {
        save(IA32_MTRR_PHYSBASE0 + index * 2)?;
        save(IA32_MTRR_PHYSMASK0 + index * 2)?;
    }
    if pat {
        save(0x277)?;
    }
    // Enable/default type last, after all fixed/variable MTRRs and PAT.
    save(IA32_MTRR_DEF_TYPE)?;
    Ok(entries)
}

/// # Safety
/// CPU must support MTRRs.
pub unsafe fn variable_count() -> u32 {
    unsafe { (rdmsr(IA32_MTRR_CAP) & 0xff) as u32 }
}
/// # Safety
/// CPU must support MTRRs.
pub unsafe fn fixed_supported() -> bool {
    unsafe { rdmsr(IA32_MTRR_CAP) & (1 << 8) != 0 }
}
pub fn encode_variable(base: u64, size: u64, ty: u64) -> (u64, u64) {
    let mask = crate::x86::physical_address_mask();
    ((base & mask) | ty, (!size.wrapping_sub(1) & mask) | VALID)
}
/// # Safety
/// Naturally aligned power-of-two range, valid CPU index; caller coordinates caches.
pub unsafe fn set_variable(index: u32, base: u64, size: u64, ty: u64) {
    let (base, mask) = encode_variable(base, size, ty);
    unsafe {
        wrmsr(IA32_MTRR_PHYSBASE0 + index * 2, base);
        wrmsr(IA32_MTRR_PHYSMASK0 + index * 2, mask);
    }
}
/// # Safety
/// Valid MTRR index; caller coordinates cacheability updates.
pub unsafe fn clear_variable(index: u32) {
    unsafe {
        wrmsr(IA32_MTRR_PHYSMASK0 + index * 2, 0);
        wrmsr(IA32_MTRR_PHYSBASE0 + index * 2, 0);
    }
}
/// # Safety
/// Valid MTRR index on the current CPU.
pub unsafe fn read_variable(index: u32) -> (u64, u64) {
    unsafe {
        (
            rdmsr(IA32_MTRR_PHYSBASE0 + index * 2),
            rdmsr(IA32_MTRR_PHYSMASK0 + index * 2),
        )
    }
}
pub const fn is_valid_mask(mask: u64) -> bool {
    mask & VALID != 0
}
pub fn decode_base(base: u64) -> u64 {
    base & crate::x86::physical_address_mask()
}
pub const fn decode_type(base: u64) -> u64 {
    base & 0xff
}
pub fn decode_size(mask: u64) -> u64 {
    let phys = crate::x86::physical_address_mask();
    (!(mask & phys) & phys).wrapping_add(0x1000)
}

/// # Safety
/// Only in RAM, never in CAR. Caller uses the architectural MTRR sequence.
pub unsafe fn disable_cache() {
    unsafe {
        let cr0 = x86::controlregs::cr0();
        x86::controlregs::cr0_write(
            (cr0 | x86::controlregs::Cr0::CR0_CACHE_DISABLE)
                & !x86::controlregs::Cr0::CR0_NOT_WRITE_THROUGH,
        );
        // Callers flush dirty data before this operation. Never WBINVD under
        // CD=1: it hangs on supported i945/Atom hardware.
    }
}
/// # Safety
/// MTRRs/PAT must describe a valid, coherent cacheability state.
pub unsafe fn enable_cache() {
    unsafe {
        x86::controlregs::cr0_write(
            x86::controlregs::cr0()
                & !(x86::controlregs::Cr0::CR0_CACHE_DISABLE
                    | x86::controlregs::Cr0::CR0_NOT_WRITE_THROUGH),
        );
    }
}
/// Legacy VGA window, the only uncached part of the fixed-MTRR range.
const VGA_WINDOW: core::ops::Range<u64> = 0xa0000..0xc0000;

/// Fixed-MTRR policy for the first MiB, as coreboot programs it: everything
/// is DRAM and write-back except the legacy VGA window. That includes the
/// PAM-shadowed BIOS area (0xc0000..1 MiB), where option ROMs and BIOS
/// payloads such as SeaBIOS run.
fn fixed_type(address: u64) -> CacheType {
    if VGA_WINDOW.contains(&address) {
        CacheType::Uncacheable
    } else {
        CacheType::WriteBack
    }
}

/// Value of the `index`th entry of [`FIXED_MSRS`]: eight ranges, one type
/// byte each.
fn fixed_msr_value(index: usize) -> u64 {
    let (base, step) = match index {
        0 => (0, 0x1_0000),
        1 => (0x8_0000, 0x4000),
        2 => (0xa_0000, 0x4000),
        n => (0xc_0000 + (n as u64 - 3) * 0x8000, 0x1000),
    };
    (0..8).fold(0, |value, range| {
        value | (fixed_type(base + range * step) as u64) << (range * 8)
    })
}

/// # Safety
/// CPU supports fixed MTRRs; all active CPUs must receive identical values.
pub unsafe fn setup_fixed_low_memory() {
    if !unsafe { fixed_supported() } {
        return;
    }
    for (index, msr) in FIXED_MSRS.into_iter().enumerate() {
        unsafe { wrmsr(msr, fixed_msr_value(index)) };
    }
}

/// Optional BSP-only ROM acceleration. Never overwrite a RAM range or use an
/// overlapping UC/WB range with WP. Removing it clears only the slot we own.
/// # Safety
/// BSP-only; caller obeys stage-specific MTRR update requirements and removes
/// this optimization before AP mirroring or payload handoff.
pub unsafe fn set_boot_rom_wp(enable: bool) -> Result<(), ServiceError> {
    if !enable {
        if let Some(index) = ROM_SLOT.swap(0, Ordering::AcqRel).checked_sub(1) {
            unsafe {
                clear_variable(index as u32);
            }
        }
        return Ok(());
    }
    if ROM_SLOT.load(Ordering::Acquire) != 0 {
        return Ok(());
    }
    if unsafe { rdmsr(IA32_MTRR_DEF_TYPE) } & 0xff != MTRR_TYPE_UNCACHEABLE {
        return Err(ServiceError::NotSupported);
    }
    let mut free = None;
    for index in 0..unsafe { variable_count() } {
        let (base, mask) = unsafe { read_variable(index) };
        if !is_valid_mask(mask) {
            free = Some(index);
            continue;
        }
        let start = decode_base(base);
        let end = start + decode_size(mask);
        if start < 0x100000000 && end > 0xff000000 {
            return Err(ServiceError::NotSupported);
        }
    }
    let index = free.ok_or(ServiceError::NotSupported)?;
    unsafe {
        set_variable(index, 0xff000000, 0x1000000, MTRR_TYPE_WRITE_PROTECT);
    }
    ROM_SLOT.store(index as usize + 1, Ordering::Release);
    Ok(())
}

#[cfg(test)]
mod tests;
