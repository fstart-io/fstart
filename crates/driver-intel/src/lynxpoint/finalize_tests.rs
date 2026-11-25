//! Host unit tests for PCH finalization: SPI menu + LPC lock ordering,
//! read/write-once write-backs, and unconditional `GEN_PMCON_1.SMI_LOCK`.

use super::*;
use alloc::vec::Vec;
use core::cell::RefCell;
use fstart_pci::{ConfigRegionAccess, PciAddress};

struct HdaTrace<'a>(&'a RefCell<Vec<(u16, u32)>>);

impl ConfigRegionAccess for HdaTrace<'_> {
    unsafe fn read(&self, at: PciAddress, off: u16) -> u32 {
        assert_eq!(at, PciAddress::new(0, 0, 27, 0));
        match off {
            0 => 0x9c20_8086,
            0x74 => 0x4321_0000,
            _ => unreachable!("unexpected HDA read at {off:#x}"),
        }
    }

    unsafe fn write(&self, at: PciAddress, off: u16, val: u32) {
        assert_eq!(at, PciAddress::new(0, 0, 27, 0));
        self.0.borrow_mut().push((off, val));
    }
}

#[test]
fn menu_programs_spi_opcodes_and_latches_lpc_lock_and_rwo_registers() {
    let mut rcba_buf = alloc::vec![0u32; 0x4000 / 4];
    // Seed existing R/WO and LPC lock state before finalize.
    rcba_buf[0x21a4 / 4] = 0x1234_5678;
    rcba_buf[0x3a6c / 4] = 0x8000;
    // SAFETY: aligned 16 KiB buffer modeling RCBA.
    let rcba_win = unsafe { crate::southbridge::rcba_window(rcba_buf.as_mut_ptr() as usize) };

    finalize_lpc_menu(rcba_win);

    assert_eq!(rcba_win.read16(0x3894), 0x5006);
    assert_eq!(rcba_win.read8(0x3898), 0x01); // WRSR
    assert_eq!(rcba_win.read8(0x389f), 0x0b); // fast read
    assert_eq!(rcba_win.read16(0x3896), 0xb32d);
    assert_eq!(rcba_win.read32(0x3a6c), 0x8001);

    let hda_writes = RefCell::new(Vec::new());
    crate::southbridge::complete_pch_rwo(rcba_win, HdaTrace(&hda_writes));
    assert_eq!(rcba_win.read32(0x21a4), 0x1234_5678);
    assert_eq!(*hda_writes.borrow(), [(0x74, 0x4321_0000)]);
}

#[test]
fn finalize_locks_set_smi_lock_unconditionally_and_verify_all_bits() {
    let mut rcba_buf = alloc::vec![0u32; 0x4000 / 4];
    let mut lpc_buf = alloc::vec![0u32; core::mem::size_of::<LynxPointLpcConfig>() / 4];
    // SAFETY: aligned backing buffers matching RcbaRegs and LynxPointLpcConfig.
    let rcba_win = unsafe { crate::southbridge::rcba_window(rcba_buf.as_mut_ptr() as usize) };
    let rcba = unsafe { &*rcba_buf.as_mut_ptr().cast::<RcbaRegs>() };
    let lpc = unsafe { &*lpc_buf.as_mut_ptr().cast::<LynxPointLpcConfig>() };

    // Simulate CF9GR still set prior to finalize and SMI_LOCK clear (no SMM).
    lpc.etr3.modify(ETR3::CF9GR::SET);
    assert!(!chipset_locks_held(rcba, lpc));

    finalize_lpc_menu(rcba_win);
    apply_chipset_locks(rcba, lpc);

    // Bug 3b regression check: GEN_PMCON_1.SMI_LOCK is set unconditionally.
    assert!(lpc.gen_pmcon_1.is_set(GEN_PMCON_1::SMI_LOCK));
    assert!(lpc.etr3.is_set(ETR3::CF9LOCK));
    assert!(!lpc.etr3.is_set(ETR3::CF9GR));
    assert!(chipset_locks_held(rcba, lpc));

    // Clearing SMI_LOCK must fail the lockdown verification check.
    lpc.gen_pmcon_1.modify(GEN_PMCON_1::SMI_LOCK::CLEAR);
    assert!(!chipset_locks_held(rcba, lpc));
}
