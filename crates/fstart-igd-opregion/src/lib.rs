//! Shared Intel integrated graphics ACPI OpRegion builder.
//!
//! GM965/GM45-era northbridges use the same 8 KiB ACPI OpRegion header/mailbox
//! layout and the same VBT placement convention.  Northbridge drivers own the
//! static storage and ASLS PCI register write; this crate fills the common
//! contents from a validated VBT blob.

#![no_std]

use core::cell::UnsafeCell;

/// OpRegion base size reported in ASLE header, in bytes.
pub const BASE_SIZE: usize = 8 * 1024;
/// Backing storage size used when an extended VBT block is needed.
pub const TOTAL_SIZE: usize = 16 * 1024;
/// Inline VBT offset in the OpRegion.
pub const VBT_INLINE_OFFSET: usize = 0x400;
/// Maximum inline VBT size.
pub const VBT_INLINE_SIZE: usize = 6 * 1024;
/// Extended VBT offset in the backing storage.
pub const VBT_EXT_OFFSET: usize = BASE_SIZE;
/// `$VBT` little-endian signature.
pub const VBT_SIGNATURE: u32 = 0x5442_5624;

/// Static OpRegion backing store.
#[repr(align(4096))]
pub struct IgdOpRegionStore(UnsafeCell<[u8; TOTAL_SIZE]>);

// SAFETY: drivers initialize the store during BSP chipset init, before ASLS is
// handed to ACPI/OS graphics drivers. Afterwards it is read-mostly shared data.
unsafe impl Sync for IgdOpRegionStore {}

impl IgdOpRegionStore {
    /// Create zeroed OpRegion storage.
    pub const fn new() -> Self {
        Self(UnsafeCell::new([0; TOTAL_SIZE]))
    }

    /// Run a closure with mutable access during one-time chipset init.
    ///
    /// # Safety
    /// The caller must ensure exclusive initialization access and no concurrent
    /// OS/ACPI use of the ASLS-published region.
    pub unsafe fn with_mut<R>(&self, f: impl FnOnce(&mut [u8; TOTAL_SIZE]) -> R) -> R {
        // SAFETY: upheld by the caller; the mutable borrow does not escape.
        f(unsafe { &mut *self.0.get() })
    }
}

impl Default for IgdOpRegionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Return the valid VBT length from a candidate blob.
pub fn vbt_size(vbt: &[u8]) -> Option<usize> {
    if vbt.len() < 28 || u32::from_le_bytes([vbt[0], vbt[1], vbt[2], vbt[3]]) != VBT_SIGNATURE {
        return None;
    }
    let size = u16::from_le_bytes([vbt[24], vbt[25]]) as usize;
    if size == 0 || size > vbt.len() {
        None
    } else {
        Some(size)
    }
}

/// Find a `$VBT` blob in a legacy option-ROM window.
pub fn legacy_vbt(base: usize) -> Option<&'static [u8]> {
    // SAFETY: caller supplies a platform-specific readable legacy option ROM window.
    let rom = unsafe { core::slice::from_raw_parts(base as *const u8, 128 * 1024) };
    let mut off = 0usize;
    while off + 4 < rom.len() {
        if u32::from_le_bytes([rom[off], rom[off + 1], rom[off + 2], rom[off + 3]]) == VBT_SIGNATURE
        {
            if let Some(size) = vbt_size(&rom[off..]) {
                return Some(&rom[off..off + size]);
            }
        }
        off += 16;
    }
    None
}

/// Fill an Intel IGD OpRegion and return the ASLS address.
pub fn build_opregion(opregion: &mut [u8; TOTAL_SIZE], vbt: &[u8]) -> usize {
    opregion.fill(0);
    opregion[0..16].copy_from_slice(b"IntelGraphicsMem");
    write_u32(opregion, 16, (BASE_SIZE / 1024) as u32);
    opregion[20] = 0;
    opregion[21] = 0;
    opregion[22] = 1;
    opregion[23] = 2;
    if vbt.len() >= 82 {
        opregion[56..60].copy_from_slice(&vbt[78..82]);
    }

    // Mailboxes: public ACPI, software SCI, power conservation, backlight.
    write_u32(opregion, 88, (1 << 0) | (1 << 2) | (1 << 3) | (1 << 4));
    write_u32(opregion, 0x100 + 172, 1);
    write_u32(opregion, 0x300 + 16, 0xff);
    write_u32(opregion, 0x300 + 20, (1 << 31) | 6);
    write_u32(opregion, 0x300 + 24, (1 << 31) | 0x64);
    for (idx, level) in [
        0x0000u16, 0x0a19, 0x1433, 0x1e4c, 0x2866, 0x327f, 0x3c99, 0x46b2, 0x50cc, 0x5ae5, 0x64ff,
    ]
    .iter()
    .copied()
    .enumerate()
    {
        write_u16(opregion, 0x300 + 28 + idx * 2, 0x8000 | level);
    }

    if vbt.len() <= VBT_INLINE_SIZE {
        opregion[VBT_INLINE_OFFSET..VBT_INLINE_OFFSET + vbt.len()].copy_from_slice(vbt);
    } else {
        let ext_size = ((vbt.len() + 511) & !511).min(TOTAL_SIZE - VBT_EXT_OFFSET);
        opregion[VBT_EXT_OFFSET..VBT_EXT_OFFSET + vbt.len().min(ext_size)]
            .copy_from_slice(&vbt[..vbt.len().min(ext_size)]);
        write_u64(opregion, 0x300 + 186, BASE_SIZE as u64);
        write_u32(opregion, 0x300 + 194, ext_size as u32);
    }

    opregion.as_ptr() as usize
}

fn write_u16(buf: &mut [u8], off: usize, val: u16) {
    buf[off..off + 2].copy_from_slice(&val.to_le_bytes());
}

fn write_u32(buf: &mut [u8], off: usize, val: u32) {
    buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
}

fn write_u64(buf: &mut [u8], off: usize, val: u64) {
    buf[off..off + 8].copy_from_slice(&val.to_le_bytes());
}
