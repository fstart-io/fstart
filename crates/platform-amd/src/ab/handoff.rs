// SPDX-License-Identifier: MIT OR Apache-2.0
//! Wire-safe DRAM handoff page written by the AMD RO selector stage at
//! [`super::AB_HANDOFF_ADDR`] and consumed by the authenticated RW stage.

use fstart_core::ffs::trust::TRUST_SIZE;
use fstart_ffs::slot_state::Slot;
use fstart_stage::slot::ActiveSlotHandoff;
use zerocopy::byteorder::{LE, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub const HANDOFF_MAGIC: [u8; 4] = *b"AMAB";
pub const HANDOFF_VERSION: u32 = 1;

/// ROM3 discovery mode passed from RO to RW so RW doesn't need to re-probe
/// ROM3 on SP3 (and Siena always reports `Absent`).
pub const ROM3_ABSENT: u8 = 0;
pub const ROM3_LINEAR: u8 = 1;
pub const ROM3_PAGED: u8 = 2;

/// Fixed-layout RO -> RW handoff placed at [`super::AB_HANDOFF_ADDR`].
#[derive(Clone, Copy, Debug, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
pub struct AmdAbHandoff {
    pub magic: [u8; 4],
    pub version: U32<LE>,
    /// `1` if RO probed a FIFO TPM 2.0 and locked its rollback floor NV index.
    pub tpm_present: u8,
    /// Booted SP3 ROM2 page (`SPIx5C`, 0 or 1; always 0 on Siena).
    pub rom_page: u8,
    /// Discovered ROM3 mapping mode ([`ROM3_ABSENT`], [`ROM3_LINEAR`], [`ROM3_PAGED`]).
    pub rom3_mode: u8,
    pub reserved: [u8; 5],
    /// Total SPI flash size in bytes (16 MiB or 32 MiB).
    pub flash_size: U32<LE>,
    /// BSP BIST captured by RO's `amd_psp_reset.S`.
    pub bsp_bist: U32<LE>,
    /// Physical address of RO's `_sp3_ap_route` word in `.amd.entry16`.
    pub ap_route_addr: U64<LE>,
    /// CPU physical base of the lower 16 MiB flash window (`0xff00_0000` or `0xfd_0000_0000`).
    pub rom_window_base: U64<LE>,
    /// Monotonic TPM 2.0 rollback floor read/advanced by RO (0 if no TPM).
    pub tpm_floor: U64<LE>,
    /// Elapsed microseconds RO spent verifying + loading RW (with SPI100 + WP MTRR).
    pub ro_verify_us: U64<LE>,
    /// Active RW slot metadata, root digest, effective floor, and trial/sync flags.
    pub slot: ActiveSlotHandoff,
    /// Copy of RO's build-patched [`TrustBlock`](fstart_core::ffs::trust::TrustBlock).
    pub trust_bytes: [u8; TRUST_SIZE],
}

const _: () = assert!(core::mem::size_of::<AmdAbHandoff>() <= 4096);

/// Read and validate [`AmdAbHandoff`] from [`super::AB_HANDOFF_ADDR`].
#[cfg(target_os = "none")]
pub fn read_handoff() -> Option<AmdAbHandoff> {
    let mut bytes = [0u8; core::mem::size_of::<AmdAbHandoff>()];
    // SAFETY: RO wrote `AmdAbHandoff` at `AB_HANDOFF_ADDR` inside the reserved
    // early DRAM window before jumping to RW.
    unsafe {
        core::ptr::copy_nonoverlapping(
            super::AB_HANDOFF_ADDR as *const u8,
            bytes.as_mut_ptr(),
            bytes.len(),
        );
    }
    let handoff = AmdAbHandoff::read_from_bytes(&bytes).ok()?;
    if handoff.magic != HANDOFF_MAGIC || handoff.version.get() != HANDOFF_VERSION {
        return None;
    }
    Some(handoff)
}

/// Read [`AmdAbHandoff`] or halt if missing/corrupt.
#[cfg(target_os = "none")]
pub fn handoff() -> AmdAbHandoff {
    read_handoff().unwrap_or_else(|| {
        fstart_log::error!("amd-ab: missing or invalid RO handoff");
        fstart_log::flush();
        fstart_arch::x86::boot::halt()
    })
}

pub fn slot_name(slot: Slot) -> &'static str {
    match slot {
        Slot::A => "A",
        Slot::B => "B",
        Slot::Recovery => "Recovery",
    }
}
