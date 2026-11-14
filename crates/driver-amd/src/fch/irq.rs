//! Legacy interrupt routing: the PCI_INTR index/data pair at `C00h/C01h`
//! of every FCH, reached through its I/O window
//! ([`crate::fch::FchHw::io`]), with a PIC and an IOAPIC value per source.
//!
//! Indices 0x00–0x7F route to the PIC, 0x80–0xFF to the IOAPIC (FCH_SP3
//! §3.3): [`index`] of a source and a [`Mode`]. The late task writes the generation's table ([`routes`]); the
//! S3 resume path writes all 256 bytes back (§3.4, SW SMI b3).
//!
//! The Naples table differs from the SP4 (sp4r2) one at F2–F5/F8/F9: the
//! SP3 donor routes F2/F3 to 0C/0D and F4/F5/F8/F9 to 04/03/04/03.

use fstart_core::indexed::Indexed;
use tock_registers::interfaces::{Readable, Writeable};

use crate::cpu::Silicon;
use crate::window::{Reg, Window, indexed};

pub const INDEX: u32 = 0xC00;
pub const DATA: u32 = 0xC01;

/// Not connected.
pub const NC: u8 = 0x1f;
pub const A: u8 = 0x00;
pub const B: u8 = 0x01;
pub const C: u8 = 0x02;
pub const D: u8 = 0x03;
pub const E: u8 = 0x04;
pub const F: u8 = 0x05;
pub const G: u8 = 0x06;
pub const H: u8 = 0x07;
/// Miscellaneous IRQ settings: control bitfields, not IRQ numbers.
pub const MISC: u8 = 0x08;
pub const MISC0: u8 = 0x09;
pub const MISC1: u8 = 0x0a;
pub const MISC2: u8 = 0x0b;
pub const SIRQA: u8 = 0x0c;
pub const SIRQB: u8 = 0x0d;
pub const SIRQC: u8 = 0x0e;
pub const SIRQD: u8 = 0x0f;
pub const SCI: u8 = 0x10;
pub const SMBUS: u8 = 0x11;
pub const ASF: u8 = 0x12;
pub const PMON: u8 = 0x16;
pub const SD: u8 = 0x17;
pub const SDIO: u8 = 0x1a;
pub const CIR: u8 = 0x20;
pub const GPIOA: u8 = 0x21;
pub const GPIOB: u8 = 0x22;
pub const GPIOC: u8 = 0x23;
pub const SATA: u8 = 0x41;
pub const EMMC: u8 = 0x43;
pub const GPP0: u8 = 0x50;
pub const GPP1: u8 = 0x51;
pub const GPP2: u8 = 0x52;
pub const GPP3: u8 = 0x53;
pub const GPIO: u8 = 0x62;
pub const I2C0: u8 = 0x70;
pub const I2C1: u8 = 0x71;
pub const I2C2: u8 = 0x72;
pub const I2C3: u8 = 0x73;
pub const UART0: u8 = 0x74;
pub const UART1: u8 = 0x75;
pub const I2C4: u8 = 0x76;
pub const I2C5: u8 = 0x77;
pub const UART2: u8 = 0x78;
pub const UART3: u8 = 0x79;
/// I2C controllers 0-5.
pub const I2C: [u8; 6] = [I2C0, I2C1, I2C2, I2C3, I2C4, I2C5];

/// Entries of the table.
pub const ENTRIES: u8 = 0x80;

/// The sources of this FCH and their names; other indices aren't connected.
pub const SOURCES: &[(u8, &str)] = &[
    (A, "INTA#"),
    (B, "INTB#"),
    (C, "INTC#"),
    (D, "INTD#"),
    (E, "INTE#"),
    (F, "INTF#/GENINT2"),
    (G, "INTG#"),
    (H, "INTH#"),
    (MISC, "Misc"),
    (MISC0, "Misc0"),
    (MISC1, "Misc1"),
    (MISC2, "Misc2"),
    (SIRQA, "Ser IRQ INTA"),
    (SIRQB, "Ser IRQ INTB"),
    (SIRQC, "Ser IRQ INTC"),
    (SIRQD, "Ser IRQ INTD"),
    (SCI, "SCI"),
    (SMBUS, "SMBUS"),
    (ASF, "ASF"),
    (PMON, "PerMon"),
    (SD, "SD"),
    (SDIO, "SDIO"),
    (CIR, "CIR"),
    (GPIOA, "GPIOa"),
    (GPIOB, "GPIOb"),
    (GPIOC, "GPIOc"),
    (SATA, "SATA"),
    (EMMC, "eMMC"),
    (GPP0, "GPP0"),
    (GPP1, "GPP1"),
    (GPP2, "GPP2"),
    (GPP3, "GPP3"),
    (GPIO, "GPIO"),
    (I2C0, "I2C0"),
    (I2C1, "I2C1"),
    (I2C2, "I2C2"),
    (I2C3, "I2C3"),
    (UART0, "UART0"),
    (UART1, "UART1"),
    (I2C4, "I2C4"),
    (I2C5, "I2C5"),
    (UART2, "UART2"),
    (UART3, "UART3"),
];

/// Which of the two values of a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Pic,
    Apic,
}

/// The PCI_INTR index of `source`'s `mode` value.
pub const fn index(source: u8, mode: Mode) -> u8 {
    match mode {
        Mode::Pic => source,
        Mode::Apic => source | 0x80,
    }
}

/// The route at `index`.
pub fn entry<I: Window>(io: &I, index: u8) -> Indexed<Reg<&I, u8>, Reg<&I, u8>> {
    indexed(io, INDEX, DATA, index)
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
pub const fn routes(generation: Silicon) -> &'static [(u8, u8)] {
    match generation {
        Silicon::Zp => crate::fch::tables::PCI_INTR_NAPLES,
        Silicon::Ssp => crate::fch::tables::PCI_INTR_ROME,
        Silicon::Gn => crate::fch::tables::PCI_INTR_MILAN,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub source: u8,
    pub pic: u8,
    pub apic: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The board entry at this position has a source beyond the table.
    InvalidSource(usize),
}

/// write_pci_int_table: program every source of [`SOURCES`] from the board
/// `routes`; unlisted sources are not connected ([`NC`]). The MISC..MISC2
/// entries are control bitfields set up by the native init (the legacy timer
/// interrupt depends on them), so they keep their current values whatever
/// the board lists; this is the fix tested on the MJ11.
pub fn write_routing(io: &impl Window, routes: &[Route]) -> Result<(), Error> {
    if let Some(bad) = routes.iter().position(|r| r.source >= ENTRIES) {
        return Err(Error::InvalidSource(bad));
    }
    let mut table = [(NC, NC); ENTRIES as usize];
    for route in routes {
        table[usize::from(route.source)] = (route.pic, route.apic);
    }
    for source in MISC..=MISC2 {
        table[usize::from(source)] = (
            read(io, index(source, Mode::Pic)),
            read(io, index(source, Mode::Apic)),
        );
    }
    fstart_log::debug!("PCI_INTR: index name PIC APIC");
    for &(source, name) in SOURCES {
        let (pic, apic) = table[usize::from(source)];
        write(io, index(source, Mode::Pic), pic);
        let pic = read(io, index(source, Mode::Pic));
        write(io, index(source, Mode::Apic), apic);
        let apic = read(io, index(source, Mode::Apic));
        fstart_log::debug!(
            "{} {} {} {}",
            fstart_log::Hex(source.into()),
            name,
            fstart_log::Hex(pic.into()),
            fstart_log::Hex(apic.into())
        );
    }
    Ok(())
}

/// The PIC IRQ a PCI device on `source` gets in its interrupt line register,
/// `None` if the source isn't connected (write_pci_cfg_irqs).
pub fn pci_int_line(io: &impl Window, source: u8) -> Option<u8> {
    let line = read(io, index(source, Mode::Pic));
    (line != NC).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_tables_differ_at_f2_to_f9() {
        let get = |g, i| routes(g).iter().find(|r| r.0 == i).unwrap().1;
        // FCH_SP3 §3.3 table.
        let f = [0xf2, 0xf3, 0xf4, 0xf5, 0xf8, 0xf9];
        let naples: Vec<_> = f.iter().map(|&i| get(Silicon::Zp, i)).collect();
        let rome: Vec<_> = f.iter().map(|&i| get(Silicon::Ssp, i)).collect();
        let milan: Vec<_> = f.iter().map(|&i| get(Silicon::Gn, i)).collect();
        assert_eq!(naples, [0x0c, 0x0d, 0x04, 0x03, 0x04, 0x03]);
        assert_eq!(rome, [0x04, 0x06, 0x03, 0x04, 0x05, 0x0c]);
        assert_eq!(milan, [0x04, 0x06, 0x03, 0x04, 0x03, 0x04]);
    }
}
