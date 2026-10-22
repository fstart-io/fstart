// SPDX-License-Identifier: MIT OR Apache-2.0
//! Permanent native mailbox allocation/ordering, not SMM setup or OS handoff.
use crate::{layout, memory::Memory};
use core::{
    mem::MaybeUninit,
    ptr::addr_of_mut,
    sync::atomic::{AtomicBool, Ordering},
};
use fstart_driver_amd::picasso::{
    psp::{self, Buffer, Mailbox, Native},
    registers::Smn,
};
const PAGE_BYTES: usize = (layout::PSP_END - layout::PSP_START) as usize;
const BUFFER_BYTES: usize = size_of::<Buffer>();
#[repr(C, align(32))]
struct Storage {
    buffer: MaybeUninit<Buffer>,
    padding: MaybeUninit<[u8; PAGE_BYTES - BUFFER_BYTES]>,
}
#[used]
#[unsafe(link_section = ".picasso_psp")]
static mut STORAGE: Storage = Storage {
    buffer: MaybeUninit::uninit(),
    padding: MaybeUninit::uninit(),
};
static CLAIMED: AtomicBool = AtomicBool::new(false);
const _: () = assert!(PAGE_BYTES == 4096 && BUFFER_BYTES == 32);
const _: () = assert!(size_of::<Storage>() == PAGE_BYTES);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    MemoryNotReserved,
    Placement,
    Bar { low: u32, high: u32 },
    AlreadyClaimed,
    Mailbox(psp::Error),
}
/// Does not return the transport/storage or any reclaim/mutable-view operation.
/// Whole page remains reserved after Drop, poison and all later errors to reset.
pub struct ResidentMailbox {
    mailbox: Mailbox<Native>,
}
impl ResidentMailbox {
    pub fn allocation_range(&self) -> core::ops::Range<u64> {
        layout::PSP_START..layout::PSP_END
    }
    pub fn capabilities(&mut self, delay_us: &mut impl FnMut(u32)) -> Result<u32, psp::Error> {
        self.mailbox.capabilities(delay_us)
    }
    pub fn ftpm_capabilities(&mut self, delay_us: &mut impl FnMut(u32)) -> Result<u32, psp::Error> {
        self.mailbox.ftpm_capabilities(delay_us)
    }
    pub fn hsti_state(&mut self, delay_us: &mut impl FnMut(u32)) -> Result<u32, psp::Error> {
        self.mailbox.hsti_state(delay_us)
    }
    /// # Safety
    /// Driver boot-done CPU-completion/protected-SMM/phase contracts all hold.
    /// Error may follow irreversible effects: fail-stop, no retry/reset/reclaim.
    pub unsafe fn boot_done(&mut self, delay_us: &mut impl FnMut(u32)) -> Result<(), psp::Error> {
        unsafe { self.mailbox.boot_done(delay_us) }
    }
}
fn preflight(reserved: bool, storage: u64, smn: &mut impl Smn) -> Result<(), Error> {
    if !reserved {
        return Err(Error::MemoryNotReserved);
    }
    if storage != layout::PSP_START
        || storage.checked_add(PAGE_BYTES as u64) != Some(layout::PSP_END)
        || storage & 31 != 0
        || layout::PSP_END > 0x1_0000_0000
    {
        return Err(Error::Placement);
    }
    // Read only existing enabled+locked aperture; never repair/reset/lock it.
    // SMN reads can program an indirect selector, so this is NOT zero hardware IO.
    let low = smn.read(0x13b102e0);
    let high = smn.read(0x13b102e4);
    if low != 0xfd100101 || high != 0 {
        return Err(Error::Bar { low, high });
    }
    Ok(())
}
fn claim(flag: &AtomicBool) -> Result<(), Error> {
    flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| Error::AlreadyClaimed)
}
unsafe fn materialize(destination: *mut Storage) {
    // Initialize full page/padding without an initialized 4KiB stack temporary.
    // Zero is valid for MaybeUninit<Buffer>; driver prepares all protocol bytes
    // before any publication. No references exist until this write completes.
    unsafe {
        core::ptr::write_bytes(destination.cast::<u8>(), 0, PAGE_BYTES);
    }
}
/// Map/placement/locked-aperture preflight precede ownership and page writes.
/// After either claim there is no retry/reclamation, including constructor error.
/// # Safety
/// Cold BSP, native early flow already established correct PSP BAR and physical
/// decoding. Establish coherent identity WB mapping for the WHOLE reserved page,
/// stable identity UC PSP aperture, exclusive SMN and mailbox ownership, intended
/// APs held or excluded, and cache/reset/event/NMI/SMI/SMM/other-DMA/alias exclusion.
/// Numerical map/SMN values do NOT establish these facts. Memory map persists and
/// ALL allocators/payloads consume it until reset, also on error. CLI not exclusion.
/// Driver Transport/constructor completion/DMA/delay contracts hold throughout
/// use; no mailbox state discovery/retry/reset or SMM activation is performed.
pub unsafe fn prepare(smn: &mut impl Smn, memory: &Memory) -> Result<ResidentMailbox, Error> {
    let pointer = addr_of_mut!(STORAGE);
    preflight(memory.psp_reserved(), pointer as usize as u64, smn)?;
    claim(&CLAIMED)?;
    unsafe {
        materialize(pointer);
        // Typed MaybeUninit<Buffer> field, initialized storage, first reference
        // only after the one-way platform claim. Private static retained to reset.
        let buffer = &mut *addr_of_mut!((*pointer).buffer).cast::<Buffer>();
        let io = Native::new();
        let mailbox = Mailbox::new(io, buffer).map_err(Error::Mailbox)?;
        Ok(ResidentMailbox { mailbox })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{boxed::Box, vec::Vec};
    struct Mock {
        low: u32,
        high: u32,
        reads: Vec<u32>,
    }
    impl Smn for Mock {
        fn read(&mut self, address: u32) -> u32 {
            self.reads.push(address);
            match address {
                0x13b102e0 => self.low,
                0x13b102e4 => self.high,
                _ => panic!("unexpected read"),
            }
        }
        fn write(&mut self, _: u32, _: u32) {
            panic!("preflight must not reprogram BAR");
        }
    }
    #[test]
    fn whole_page_and_existing_locked_aperture_preflight_before_claim() {
        let mut smn = Mock {
            low: 0xfd100101,
            high: 0,
            reads: Vec::new(),
        };
        assert_eq!(
            preflight(false, layout::PSP_START, &mut smn),
            Err(Error::MemoryNotReserved)
        );
        for start in [layout::PSP_START + 32, 0, u64::MAX] {
            assert_eq!(preflight(true, start, &mut smn), Err(Error::Placement));
        }
        assert!(smn.reads.is_empty());
        for (low, high) in [(0xfd100001, 0), (0xfd100101, 1), (u32::MAX, u32::MAX)] {
            smn.low = low;
            smn.high = high;
            assert_eq!(
                preflight(true, layout::PSP_START, &mut smn),
                Err(Error::Bar { low, high })
            );
        }
        smn.low = 0xfd100101;
        smn.high = 0;
        assert_eq!(preflight(true, layout::PSP_START, &mut smn), Ok(()));
        let flag = AtomicBool::new(false);
        assert_eq!(claim(&flag), Ok(()));
        assert_eq!(claim(&flag), Err(Error::AlreadyClaimed));
    }
    #[test]
    fn materialize_zeroes_protocol_storage_and_complete_page_tail() {
        let mut storage = Box::new(MaybeUninit::<Storage>::uninit());
        let pointer = storage.as_mut_ptr();
        unsafe {
            core::ptr::write_bytes(pointer.cast::<u8>(), 0xa5, PAGE_BYTES);
            materialize(pointer);
            let bytes = core::slice::from_raw_parts(pointer.cast::<u8>(), PAGE_BYTES);
            assert!(bytes.iter().all(|byte| *byte == 0));
        }
        assert_eq!(size_of::<Storage>(), 4096);
        assert_eq!(align_of::<Storage>(), 32);
    }
}
