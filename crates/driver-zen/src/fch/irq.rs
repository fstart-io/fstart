//! Legacy interrupt routing: the PCI_INTR index/data pair at `C00h/C01h`.
//!
//! Indices 0x00–0x7F route to the PIC, 0x80–0xFF to the IOAPIC (FCH_SP3
//! §3.3). The late task writes the generation's table ([`routes`]); the
//! S3 resume path writes all 256 bytes back (§3.4, SW SMI b3).
//!
//! The Naples table differs from the SP4 (sp4r2) one at F2–F5/F8/F9: the
//! SP3 donor routes F2/F3 to 0C/0D and F4/F5/F8/F9 to 04/03/04/03.

use fstart_core::indexed::Indexed;
use tock_registers::interfaces::{Readable, Writeable};

use super::{Reg, Window};
use crate::soc::Generation;

pub const INDEX: u32 = 0xC00;
pub const DATA: u32 = 0xC01;

/// PCI_INTR index 0x08/0x09 (written by ResetHwAcpi and the SATA env task;
/// the public PPRs do not name them).
pub const INTR_08: u8 = 0x08;
pub const INTR_09: u8 = 0x09;
/// The SCI routes (PIC 0x10, IOAPIC 0x90), set to IRQ 9 by the ACPI-enable
/// SW SMI (FCH_SP3 §5).
pub const SCI_PIC: u8 = 0x10;
pub const SCI_IOAPIC: u8 = 0x90;

fn entry<I: Window>(io: &I, index: u8) -> Indexed<Reg<'_, I, u8>, Reg<'_, I, u8>> {
    Indexed::new(io.reg(INDEX), io.reg(DATA), index)
}

pub fn read<I: Window>(io: &I, index: u8) -> u8 {
    entry(io, index).get()
}

pub fn write<I: Window>(io: &I, index: u8, irq: u8) {
    entry(io, index).set(irq)
}

pub fn update<I: Window>(io: &I, index: u8, clear: u8, set: u8) {
    let e = entry(io, index);
    e.set((e.get() & !clear) | set);
}

/// The late-task routes of `generation`, `(index, irq)` in donor order.
pub const fn routes(generation: Generation) -> &'static [(u8, u8)] {
    match generation {
        #[cfg(feature = "naples")]
        Generation::Naples => super::tables::PCI_INTR_NAPLES,
        #[cfg(feature = "rome")]
        Generation::Rome => super::tables::PCI_INTR_ROME,
        #[cfg(feature = "milan")]
        Generation::Milan => super::tables::PCI_INTR_MILAN,
        #[allow(unreachable_patterns)]
        _ => &[],
    }
}

/// Write a route table (board overrides go after it, in the same call
/// order the donor uses for the I2C/UART routes F0–F9).
pub fn program<I: Window>(io: &I, routes: &[(u8, u8)]) {
    routes
        .iter()
        .for_each(|&(index, irq)| write(io, index, irq));
}

/// Save all 256 routes (for the S3 resume restore).
pub fn save<I: Window>(io: &I) -> [u8; 256] {
    core::array::from_fn(|i| read(io, i as u8))
}

/// Write all 256 routes back (SW SMI b3, FCH_SP3 §3.4).
pub fn restore<I: Window>(io: &I, saved: &[u8; 256]) {
    saved
        .iter()
        .enumerate()
        .for_each(|(i, &irq)| write(io, i as u8, irq));
}

#[cfg(test)]
mod tests {
    use super::super::model::Mem;
    use super::*;

    #[test]
    fn index_then_data() {
        let io = Mem::default();
        program(&io, &[(0xf0, 0x0a)]);
        assert_eq!(io.writes(), vec![(0xc00, 1, 0xf0), (0xc01, 1, 0x0a)]);
    }

    #[cfg(all(feature = "naples", feature = "rome", feature = "milan"))]
    #[test]
    fn generation_tables_differ_at_f2_to_f9() {
        let get = |g, i| routes(g).iter().find(|r| r.0 == i).unwrap().1;
        // FCH_SP3 §3.3 table.
        let f = [0xf2, 0xf3, 0xf4, 0xf5, 0xf8, 0xf9];
        let naples: Vec<_> = f.iter().map(|&i| get(Generation::Naples, i)).collect();
        let rome: Vec<_> = f.iter().map(|&i| get(Generation::Rome, i)).collect();
        let milan: Vec<_> = f.iter().map(|&i| get(Generation::Milan, i)).collect();
        assert_eq!(naples, [0x0c, 0x0d, 0x04, 0x03, 0x04, 0x03]);
        assert_eq!(rome, [0x04, 0x06, 0x03, 0x04, 0x05, 0x0c]);
        assert_eq!(milan, [0x04, 0x06, 0x03, 0x04, 0x03, 0x04]);
    }
}
