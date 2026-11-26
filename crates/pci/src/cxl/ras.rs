//! The CXL RAS capability (CXL 3.1 §8.2.4.17) of a CXL port or device:
//! the CXL.cache/mem error status, masks, severity and header log in its
//! component registers, where a CPER CXL protocol error section takes its
//! error log from.
//!
//! The capability is found through the function's Register Locator DVSEC
//! (component register block, [`super::dvsec::block::COMPONENT`]) and the
//! block's capability array; the accesses go through
//! [`crate::aer::Errors`] and [`super::Registers`], free of panics, for an
//! SMM handler.

use super::Registers;
use super::component::{self, CACHE_MEM, CACHE_MEM_SIZE};
use super::dvsec::{self, RegisterBlock};
use crate::aer::Errors;
use crate::{ConfigRegionAccess, PciAddress};

/// RAS capability registers (offsets from the capability).
pub mod reg {
    pub const UNCORRECTABLE_STATUS: u32 = 0x00;
    pub const UNCORRECTABLE_MASK: u32 = 0x04;
    pub const UNCORRECTABLE_SEVERITY: u32 = 0x08;
    pub const CORRECTABLE_STATUS: u32 = 0x0c;
    pub const CORRECTABLE_MASK: u32 = 0x10;
    pub const CAPABILITY_CONTROL: u32 = 0x14;
    pub const HEADER_LOG: u32 = 0x18;
}

/// Dwords of the capability: its six registers and the 64-byte header
/// log (the CPER CXL protocol section's error log, 0x58 bytes).
pub const DWORDS: usize = 22;

/// Uncorrectable Error Status bit 10, Poison Received, and Correctable
/// Error Status bit 5, Memory Poison Received: what the AMD platform masks
/// on a CXL root port (GIGABYTE AmdPlatformRasWhDxe `Cxl2p0AerConfig
/// 0x5ff8`; FIRMWARE_FIRST.md §2.4).
pub const UNCORRECTABLE_POISON_RECEIVED: u32 = 1 << 10;
pub const CORRECTABLE_MEMORY_POISON_RECEIVED: u32 = 1 << 5;

/// The component register block of the function at `address` (from its
/// Register Locator and the memory BAR it names) and the offset of its RAS
/// capability in that block: wholly inside the block's CXL.cache/mem
/// range, so that [`snapshot`], [`clear`] and [`mask`] stay in the block
/// whatever the device's capability array says. `registers` decides
/// whether the block's address may be reached at all: the BAR is the
/// OS's at run time, so a handler's must check it (`smm::aer` on AMD).
pub fn locate<P: ConfigRegionAccess, R: Registers>(
    errors: &Errors<P>,
    address: PciAddress,
    registers: impl Fn(u64) -> Option<R>,
) -> Option<(R, u32)> {
    let block = component_block(errors, address)?;
    let base = errors
        .memory_bar(address, block.bar())?
        .checked_add(block.offset)?;
    let regs = registers(base)?;
    let (pointer, _) =
        component::find(|offset| regs.read32(CACHE_MEM + offset), component::id::RAS)?;
    (pointer.checked_add(4 * DWORDS as u32)? <= CACHE_MEM_SIZE)
        .then_some((regs, CACHE_MEM + pointer))
}

/// The component register block entry of the function's Register Locator.
fn component_block<P: ConfigRegionAccess>(
    errors: &Errors<P>,
    address: PciAddress,
) -> Option<RegisterBlock> {
    let locator = errors.dvsec(address, super::VENDOR, dvsec::id::REGISTER_LOCATOR)?;
    let bytes = (errors.config(address, locator + dvsec::HEADER2) >> 20) as u16;
    let count = bytes.saturating_sub(dvsec::REGISTER_BLOCKS) / 8;
    (0..count.min(16)).find_map(|n| {
        let entry = locator + dvsec::REGISTER_BLOCKS + 8 * n;
        let block = RegisterBlock::decode(
            errors.config(address, entry),
            errors.config(address, entry + 4),
        );
        (block.id == dvsec::block::COMPONENT && block.bir <= 5).then_some(block)
    })
}

/// The capability at `at` in `regs`, all its dwords.
pub fn snapshot(regs: &impl Registers, at: u32) -> [u32; DWORDS] {
    let mut log = [0; DWORDS];
    let mut offset = at;
    for dword in log.iter_mut() {
        *dword = regs.read32(offset);
        offset = offset.wrapping_add(4);
    }
    log
}

/// Unmasked uncorrectable and correctable errors in a [`snapshot`].
pub fn pending(log: &[u32; DWORDS]) -> (u32, u32) {
    let [ue, ue_mask, _, ce, ce_mask, ..] = *log;
    (ue & !ue_mask, ce & !ce_mask)
}

/// Clear the uncorrectable and correctable status (RW1C).
pub fn clear(regs: &impl Registers, at: u32) {
    for offset in [reg::UNCORRECTABLE_STATUS, reg::CORRECTABLE_STATUS] {
        let status = regs.read32(at.wrapping_add(offset));
        if status != 0 && status != u32::MAX {
            regs.write32(at.wrapping_add(offset), status);
        }
    }
}

/// Mask `uncorrectable` and `correctable` errors besides those masked.
pub fn mask(regs: &impl Registers, at: u32, uncorrectable: u32, correctable: u32) {
    for (offset, bits) in [
        (reg::UNCORRECTABLE_MASK, uncorrectable),
        (reg::CORRECTABLE_MASK, correctable),
    ] {
        let value = regs.read32(at.wrapping_add(offset));
        regs.write32(at.wrapping_add(offset), value | bits);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use std::collections::BTreeMap;

    /// Memory of dwords; the status registers RW1C.
    #[derive(Default)]
    struct Block(RefCell<BTreeMap<u32, u32>>, u32);

    impl Registers for &Block {
        fn read32(&self, offset: u32) -> u32 {
            *self.0.borrow().get(&offset).unwrap_or(&0)
        }
        fn write32(&self, offset: u32, value: u32) {
            let w1c = [
                self.1 + reg::UNCORRECTABLE_STATUS,
                self.1 + reg::CORRECTABLE_STATUS,
            ];
            let old = self.read32(offset);
            let new = if w1c.contains(&offset) {
                old & !value
            } else {
                value
            };
            self.0.borrow_mut().insert(offset, new);
        }
    }

    /// Config space of one function: a Register Locator at 0x100 naming
    /// the component block at BAR0 + 0x1_0000; BAR0 64-bit at
    /// 0x2_0000_0000.
    struct Config(BTreeMap<u16, u32>);

    impl ConfigRegionAccess for &Config {
        unsafe fn read(&self, _: PciAddress, offset: u16) -> u32 {
            *self.0.get(&offset).unwrap_or(&0)
        }
        unsafe fn write(&self, _: PciAddress, _: u16, _: u32) {}
    }

    #[test]
    fn the_ras_capability_of_a_root_port() {
        let config = Config(BTreeMap::from([
            (0x00, 0x1483_1022),
            (0x10, 0x0000_0004),
            (0x14, 0x0000_0002),
            // DVSEC, next 0; CXL vendor, 0x24 bytes; Register Locator.
            (0x100, 0x0001_0023),
            (0x104, 0x0241_1e98),
            (0x108, 0x0000_0008),
            // Entry 0: BAR0, component block, offset 1 << 16.
            (0x10c, 0x0001_0100),
            (0x110, 0x0000_0000),
        ]));
        let errors = unsafe { Errors::new(&config) };
        let a = PciAddress::new(0, 0x80, 1, 1);
        let ras = CACHE_MEM + 0x40;
        let block = Block(RefCell::new(BTreeMap::new()), ras);
        let regs = &block;
        regs.write32(CACHE_MEM, component::array_header(1));
        regs.write32(
            CACHE_MEM + 4,
            component::capability_header(component::id::RAS, 2, 0x40),
        );
        let bases = RefCell::new(std::vec::Vec::new());
        let (regs, at) = locate(&errors, a, |base| {
            bases.borrow_mut().push(base);
            Some(regs)
        })
        .unwrap();
        assert_eq!(*bases.borrow(), [0x2_0001_0000]);
        assert_eq!(at, ras);
        regs.0
            .borrow_mut()
            .insert(ras + reg::UNCORRECTABLE_STATUS, 0x400);
        regs.0
            .borrow_mut()
            .insert(ras + reg::CORRECTABLE_STATUS, 0x21);
        assert_eq!(pending(&snapshot(&regs, ras)), (0x400, 0x21));
        mask(
            &regs,
            ras,
            UNCORRECTABLE_POISON_RECEIVED,
            CORRECTABLE_MEMORY_POISON_RECEIVED,
        );
        assert_eq!(pending(&snapshot(&regs, ras)), (0, 1));
        clear(&regs, ras);
        let log = snapshot(&regs, ras);
        assert_eq!((log[0], log[3]), (0, 0));
        assert_eq!((log[1], log[4]), (0x400, 0x20));
    }

    /// A capability array whose RAS pointer leaves no room for the
    /// capability's 22 dwords in the CXL.cache/mem range is refused: the
    /// accesses would leave the component block.
    #[test]
    fn a_ras_pointer_at_the_end_of_the_range_is_refused() {
        let config = Config(BTreeMap::from([
            (0x10, 0x0000_0004),
            (0x14, 0x0000_0002),
            (0x100, 0x0001_0023),
            (0x104, 0x0241_1e98),
            (0x108, 0x0000_0008),
            (0x10c, 0x0001_0100),
            (0x110, 0x0000_0000),
        ]));
        let errors = unsafe { Errors::new(&config) };
        let a = PciAddress::new(0, 0x80, 1, 1);
        for (pointer, found) in [(0xfa8, true), (0xfac, false), (0xffc, false)] {
            let block = Block(RefCell::new(BTreeMap::new()), 0);
            let regs = &block;
            regs.write32(CACHE_MEM, component::array_header(1));
            regs.write32(
                CACHE_MEM + 4,
                component::capability_header(component::id::RAS, 2, pointer),
            );
            assert_eq!(
                locate(&errors, a, |_| Some(regs)).is_some(),
                found,
                "{pointer:#x}"
            );
        }
    }
}
