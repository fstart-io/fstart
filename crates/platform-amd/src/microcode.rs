// SPDX-License-Identifier: MIT OR Apache-2.0
//! One-shot permanent DRAM staging, not patch authentication or loader execution.
#[cfg(all(feature = "linked-microcode", target_os = "none"))]
pub mod linked;

use crate::{layout, memory::Memory};
use core::{
    mem::MaybeUninit,
    ptr::addr_of_mut,
    sync::atomic::{AtomicBool, Ordering},
};
use fstart_driver_amd::picasso::cpu::microcode::{self, PATCH_BYTES, Patch};

const PAGE_BYTES: usize = (layout::MICROCODE_END - layout::MICROCODE_START) as usize;
#[repr(C, align(16))]
struct Storage(MaybeUninit<[u8; PAGE_BYTES]>);
#[used]
#[unsafe(link_section = ".picasso_microcode")]
static mut STORAGE: Storage = Storage(MaybeUninit::uninit());
static CLAIMED: AtomicBool = AtomicBool::new(false);
const _: () = assert!(PAGE_BYTES == 4096 && PATCH_BYTES <= PAGE_BYTES);
const _: () = assert!(core::mem::size_of::<Storage>() == PAGE_BYTES);

#[derive(Debug)]
pub enum Error {
    Patch(microcode::Error),
    MemoryNotReserved,
    CacheGeometry,
    Placement,
    SourceRange,
    SourceOverlap,
    AlreadyClaimed,
}

/// No reclaim operation or mutable view. Dropping this value never releases the
/// backing page; the patch, unused tail and its map exclusion survive to reset.
/// This is software ownership metadata, not physical isolation/authentication.
pub struct ResidentPatch {
    bytes: &'static [u8],
}
impl ResidentPatch {
    pub fn bytes(&self) -> &'static [u8] {
        self.bytes
    }
    pub fn allocation_range(&self) -> core::ops::Range<u64> {
        layout::MICROCODE_START..layout::MICROCODE_END
    }
}

fn placement(storage: u64, source: u64, length: usize) -> Result<(), Error> {
    if storage != layout::MICROCODE_START
        || storage.checked_add(PAGE_BYTES as u64) != Some(layout::MICROCODE_END)
    {
        return Err(Error::Placement);
    }
    Patch::validate_address(storage).map_err(Error::Patch)?;
    let end = source
        .checked_add(length as u64)
        .ok_or(Error::SourceRange)?;
    if source < layout::MICROCODE_END && end > layout::MICROCODE_START {
        return Err(Error::SourceOverlap);
    }
    Ok(())
}
fn claim(owner: &AtomicBool) -> Result<(), Error> {
    owner
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| Error::AlreadyClaimed)
}

// Only called after exact-length/whole-placement/disjoint-source preflight and
// one-way ownership. Raw writes initialize every page byte without a page-sized
// initialized BSP-stack temporary or an uninitialized u8 reference.
unsafe fn materialize(destination: *mut u8, source: &[u8]) {
    unsafe {
        core::ptr::copy_nonoverlapping(source.as_ptr(), destination, source.len());
        core::ptr::write_bytes(destination.add(source.len()), 0, PAGE_BYTES - source.len());
    }
}

/// Stage the selected trusted BSP patch before AP release. Parser/map/placement
/// errors precede ownership/writes. After the claim, all results retain the whole
/// allocation until reset; there is no retry/reclaim/borrow withdrawal.
///
/// # Safety
/// Cold BSP at ring0, intended APs held, no concurrent access to this storage,
/// and supported CLFLUSH. Establish physical RAM decoding, coherent identity WB
/// mapping and cache/event/reset/SMM exclusion for the entire destination page.
/// The source is the exact independently authenticated/trusted target vendor
/// patch, immutable through copying, readable ordinary memory, and disjoint from
/// the entire allocation. Parsing does not establish signature/authenticity or
/// freshness. The map is retained and consumed by ALL allocators/payloads until
/// reset, even on error. No PSP scratch, SMM or other DMA agent may overwrite it.
pub unsafe fn stage(
    source: &[u8],
    signature: u32,
    memory: &Memory,
) -> Result<ResidentPatch, Error> {
    Patch::parse(source, signature).map_err(Error::Patch)?;
    let (_, ebx, _, features) = fstart_arch::x86::cpuid(1);
    let line = ((ebx >> 8) & 0xff) as usize * 8;
    // The generic helper's WBINVD fallback is not an accepted staging path.
    // Cache-line rounding must stay within our whole-page reservation.
    if features & (1 << 19) == 0 || !line.is_power_of_two() || !PAGE_BYTES.is_multiple_of(line) {
        return Err(Error::CacheGeometry);
    }
    if !memory.microcode_reserved() {
        return Err(Error::MemoryNotReserved);
    }
    let pointer = addr_of_mut!(STORAGE).cast::<u8>();
    placement(
        pointer as usize as u64,
        source.as_ptr() as usize as u64,
        source.len(),
    )?;
    claim(&CLAIMED)?;
    unsafe {
        materialize(pointer, source);
        // Include unused tail, not merely the patch's 3200 bytes, before exposing
        // an immutable view to NativeCpu or its eventual hardware loader.
        fstart_arch::x86::writeback_cache_range(pointer.cast_const(), PAGE_BYTES);
        let bytes = core::slice::from_raw_parts(pointer.cast_const(), PATCH_BYTES);
        Patch::parse(bytes, signature).map_err(Error::Patch)?;
        Ok(ResidentPatch { bytes })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    #[test]
    fn staging_initializes_exact_bytes_and_entire_unused_tail() {
        let source = vec![0x5a; PATCH_BYTES];
        let mut destination = [MaybeUninit::new(0xa5); PAGE_BYTES];
        // Host RAM-only copy/zero test; no stage entry, CLFLUSH or loader.
        unsafe { materialize(destination.as_mut_ptr().cast(), &source) };
        let bytes =
            unsafe { core::slice::from_raw_parts(destination.as_ptr().cast::<u8>(), PAGE_BYTES) };
        assert_eq!(&bytes[..PATCH_BYTES], source);
        assert!(bytes[PATCH_BYTES..].iter().all(|&byte| byte == 0));
    }
    #[test]
    fn whole_page_geometry_and_claim_are_one_shot() {
        let start = layout::MICROCODE_START;
        assert!(placement(start, layout::IMAGE_END - PATCH_BYTES as u64, PATCH_BYTES).is_ok());
        assert!(matches!(
            placement(start + 16, 0x100000, PATCH_BYTES),
            Err(Error::Placement)
        ));
        for source in [start - 16, start, layout::MICROCODE_END - 1] {
            assert!(matches!(
                placement(start, source, PATCH_BYTES),
                Err(Error::SourceOverlap)
            ));
        }
        assert!(matches!(
            placement(start, u64::MAX - 1, PATCH_BYTES),
            Err(Error::SourceRange)
        ));
        let owner = AtomicBool::new(false);
        assert!(claim(&owner).is_ok());
        assert!(matches!(claim(&owner), Err(Error::AlreadyClaimed)));
        assert!(owner.load(Ordering::Acquire));
    }
}
