//! Boot timestamps, after coreboot's timestamp table.
//!
//! Stages record [`Id`] milestones as raw counter values (the TSC on x86)
//! into one table that moves along with the boot: the bootblock starts it
//! in CAR, moves it into the firmware store once DRAM is up, and later
//! stages attach to the store entry. Counter values are converted to time
//! only when the table is reported.
//!
//! Every record also carries the counter ticks spent writing the console
//! so far, so each interval splits into console output and the rest:
//! hardware initialisation and firmware logic. `fstart-log` accounts its
//! console writes through [`add_console_time`].
//!
//! Without an attached table, or on targets without a supported counter,
//! everything here is a cheap no-op.

#![no_std]

use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

/// A milestone identifier. Values below 1000 follow coreboot's
/// `timestamp_serialized.h` where the meaning matches; fstart-specific ones
/// start at 1000.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Id(pub u32);

/// Known milestones. Each marks the start of what follows it.
pub mod id {
    use super::Id;

    pub const INITRAM_START: Id = Id(2);
    pub const INITRAM_END: Id = Id(3);
    pub const LOAD_RAMSTAGE: Id = Id(8);
    pub const RAMSTAGE_START: Id = Id(10);
    pub const BOOTBLOCK_START: Id = Id(11);
    pub const LOAD_POSTCAR: Id = Id(13);
    pub const LOAD_PAYLOAD: Id = Id(90);
    pub const PAYLOAD_JUMP: Id = Id(99);
    pub const POSTCAR_START: Id = Id(100);

    pub const CONSOLE_READY: Id = Id(1000);
    pub const EARLY_INIT: Id = Id(1001);
    pub const RAMINIT_SPD: Id = Id(1002);
    pub const RAMINIT_PHY: Id = Id(1003);
    pub const RAMINIT_JEDEC: Id = Id(1004);
    pub const RAMINIT_TRAINING: Id = Id(1005);
    pub const RAMINIT_FINALIZE: Id = Id(1006);
    pub const RAMINIT_MEMORY_TEST: Id = Id(1007);
    pub const POST_DRAM_INIT: Id = Id(1008);
    pub const VERIFY_BOOT_ROOT: Id = Id(1009);
    pub const STORE_STAGE_CACHE: Id = Id(1010);

    pub const IMPORT_BOOT_CONTEXT: Id = Id(1100);
    pub const PUBLISH_BOOT_MEDIA: Id = Id(1101);
    pub const PRE_BUS_SCAN: Id = Id(1102);
    pub const RESERVE_FIRMWARE_MEMORY: Id = Id(1103);
    pub const LOAD_MEMORY_POLICY: Id = Id(1104);
    pub const COMMIT_MEMORY_CACHE: Id = Id(1105);
    pub const BUS_SCAN: Id = Id(1106);
    pub const INSTALL_PAGE_TABLES: Id = Id(1107);
    pub const INIT_DEVICES: Id = Id(1108);
    pub const MOUNT_BOOT_MEDIA: Id = Id(1109);
    pub const VERIFY_BOOT_MEDIA: Id = Id(1110);
    pub const DISPLAY_INIT: Id = Id(1111);
    pub const EMIT_TABLES: Id = Id(1112);
    pub const SEAL_STORE: Id = Id(1113);
    pub const FINALIZE: Id = Id(1114);
}

/// Human-readable milestone name.
pub fn name(id: Id) -> &'static str {
    match id {
        id::INITRAM_START => "raminit start",
        id::INITRAM_END => "raminit end",
        id::LOAD_RAMSTAGE => "load ramstage",
        id::RAMSTAGE_START => "ramstage start",
        id::BOOTBLOCK_START => "bootblock start",
        id::LOAD_POSTCAR => "load postcar",
        id::LOAD_PAYLOAD => "load payload",
        id::PAYLOAD_JUMP => "jump to payload",
        id::POSTCAR_START => "postcar start",
        id::CONSOLE_READY => "console ready",
        id::EARLY_INIT => "early chipset init",
        id::RAMINIT_SPD => "raminit: SPD and timings",
        id::RAMINIT_PHY => "raminit: DLL, RCOMP and memory map",
        id::RAMINIT_JEDEC => "raminit: JEDEC init",
        id::RAMINIT_TRAINING => "raminit: receive-enable and Vref training",
        id::RAMINIT_FINALIZE => "raminit: enhanced and power modes",
        id::RAMINIT_MEMORY_TEST => "raminit: memory test",
        id::POST_DRAM_INIT => "post-DRAM DMI/PM init",
        id::VERIFY_BOOT_ROOT => "verify boot root",
        id::STORE_STAGE_CACHE => "store S3 stage cache",
        id::IMPORT_BOOT_CONTEXT => "import boot context",
        id::PUBLISH_BOOT_MEDIA => "publish boot media",
        id::PRE_BUS_SCAN => "pre bus scan",
        id::RESERVE_FIRMWARE_MEMORY => "reserve firmware memory",
        id::LOAD_MEMORY_POLICY => "load memory policy",
        id::COMMIT_MEMORY_CACHE => "commit memory cache",
        id::BUS_SCAN => "bus scan",
        id::INSTALL_PAGE_TABLES => "install page tables",
        id::INIT_DEVICES => "init devices",
        id::MOUNT_BOOT_MEDIA => "mount boot media",
        id::VERIFY_BOOT_MEDIA => "verify boot media",
        id::DISPLAY_INIT => "display init",
        id::EMIT_TABLES => "emit tables",
        id::SEAL_STORE => "seal store",
        id::FINALIZE => "finalize",
        _ => "unknown",
    }
}

/// Table header; records follow it.
#[repr(C)]
struct Header {
    /// Counter value at the reset vector; records are relative to it.
    base: u64,
    /// Counter ticks spent in console output so far.
    console: u64,
    count: u32,
    capacity: u32,
    reserved: u64,
}

/// One milestone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Record {
    pub id: Id,
    reserved: u32,
    /// Counter value, relative to the table base.
    pub stamp: u64,
    /// Console ticks spent before this milestone.
    pub console: u64,
}

const HEADER_SIZE: usize = size_of::<Header>();

/// Bytes needed for a table of `capacity` records.
pub const fn table_size(capacity: usize) -> usize {
    HEADER_SIZE + capacity * size_of::<Record>()
}

static TABLE: AtomicPtr<Header> = AtomicPtr::new(null_mut());

/// Current counter value; zero without a supported counter.
#[inline]
pub fn now() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: RDTSC has no side effects; firmware runs at CPL0.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(target_arch = "x86")]
    {
        // SAFETY: RDTSC has no side effects; firmware runs at CPL0.
        unsafe { core::arch::x86::_rdtsc() }
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        0
    }
}

fn capacity_for(len: usize) -> Option<u32> {
    (len >= HEADER_SIZE)
        .then(|| (len - HEADER_SIZE) / size_of::<Record>())
        .and_then(|capacity| u32::try_from(capacity).ok())
}

fn table() -> Option<&'static mut Header> {
    // SAFETY: attached tables stay valid for the stage (see `init`).
    unsafe { TABLE.load(Ordering::Relaxed).as_mut() }
}

fn records_of(table: &Header) -> &[Record] {
    // SAFETY: `count` records follow the header (`init`, `attach`).
    unsafe {
        core::slice::from_raw_parts(
            (table as *const Header).add(1).cast::<Record>(),
            table.count.min(table.capacity) as usize,
        )
    }
}

/// Start an empty table of `len` bytes at `buffer`, with records relative
/// to the counter value `base`, and attach it.
///
/// # Safety
///
/// `buffer` is 8-byte aligned and `len` bytes of writable memory that
/// stay valid, and are not otherwise accessed, while the table is attached.
pub unsafe fn init(buffer: *mut u8, len: usize, base: u64) -> bool {
    let Some(capacity) = capacity_for(len) else {
        return false;
    };
    let header = buffer.cast::<Header>();
    // SAFETY: the caller provides the writable, aligned buffer.
    unsafe {
        header.write(Header {
            base,
            console: 0,
            count: 0,
            capacity,
            reserved: 0,
        })
    };
    TABLE.store(header, Ordering::Relaxed);
    true
}

/// Attach the table an earlier stage left in the `len` bytes at `buffer`.
///
/// # Safety
///
/// As for [`init`]; the buffer holds a table written by [`init`].
pub unsafe fn attach(buffer: *mut u8, len: usize) -> bool {
    let header = buffer.cast::<Header>();
    // SAFETY: the caller provides the aligned buffer.
    let capacity = unsafe { (*header).capacity };
    if capacity_for(len).is_none_or(|fits| capacity > fits) {
        return false;
    }
    TABLE.store(header, Ordering::Relaxed);
    true
}

/// Copy the attached table into the `len` bytes at `buffer`, growing its
/// capacity to fit, and attach the copy. Used to move from CAR to DRAM.
///
/// # Safety
///
/// As for [`init`]; `buffer` does not overlap the attached table.
pub unsafe fn move_to(buffer: *mut u8, len: usize) -> bool {
    let Some(old) = table() else {
        return false;
    };
    let records = records_of(old);
    let Some(capacity) = capacity_for(len).filter(|&c| c as usize >= records.len()) else {
        return false;
    };
    let header = buffer.cast::<Header>();
    // SAFETY: the caller provides the writable, aligned, disjoint buffer.
    unsafe {
        header.write(Header {
            base: old.base,
            console: old.console,
            count: records.len() as u32,
            capacity,
            reserved: 0,
        });
        core::ptr::copy_nonoverlapping(
            records.as_ptr(),
            header.add(1).cast::<Record>(),
            records.len(),
        );
    }
    TABLE.store(header, Ordering::Relaxed);
    true
}

/// Record `id` now.
#[inline]
pub fn add(id: Id) {
    add_at(id, now());
}

/// Record `id` at the counter value `stamp`. Records beyond the table's
/// capacity are dropped.
pub fn add_at(id: Id, stamp: u64) {
    let Some(table) = table() else {
        return;
    };
    if table.count >= table.capacity {
        return;
    }
    let record = Record {
        id,
        reserved: 0,
        stamp: stamp.wrapping_sub(table.base),
        console: table.console,
    };
    // SAFETY: below capacity, inside the attached buffer.
    unsafe {
        (table as *mut Header)
            .add(1)
            .cast::<Record>()
            .add(table.count as usize)
            .write(record)
    };
    table.count += 1;
}

/// Account `ticks` of console output. Callers serialise console writes.
#[inline]
pub fn add_console_time(ticks: u64) {
    if let Some(table) = table() {
        table.console = table.console.wrapping_add(ticks);
    }
}

/// The attached table's records, its base counter value and the console
/// ticks spent so far.
pub fn snapshot() -> Option<(&'static [Record], u64, u64)> {
    let table = table()?;
    Some((records_of(table), table.base, table.console))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[repr(align(8))]
    struct Buffer([u8; 256]);

    #[test]
    fn records_move_with_their_console_time() {
        let mut car = Buffer([0; 256]);
        let mut dram = Buffer([0; 256]);
        unsafe { assert!(init(car.0.as_mut_ptr(), table_size(2), 100)) };
        add_at(id::BOOTBLOCK_START, 150);
        add_console_time(30);
        add_at(id::INITRAM_START, 200);
        add_at(id::INITRAM_END, 300);
        let (records, base, console) = snapshot().unwrap();
        assert_eq!((records.len(), base, console), (2, 100, 30));

        unsafe { assert!(move_to(dram.0.as_mut_ptr(), dram.0.len())) };
        add_at(id::INITRAM_END, 300);
        let (records, _, _) = snapshot().unwrap();
        let stamps: std::vec::Vec<_> = records.iter().map(|r| (r.id, r.stamp, r.console)).collect();
        assert_eq!(
            stamps,
            [
                (id::BOOTBLOCK_START, 50, 0),
                (id::INITRAM_START, 100, 30),
                (id::INITRAM_END, 200, 30)
            ]
        );
        unsafe { assert!(attach(dram.0.as_mut_ptr(), dram.0.len())) };
        unsafe { assert!(!attach(dram.0.as_mut_ptr(), table_size(1))) };
    }
}
