// SPDX-License-Identifier: MIT OR Apache-2.0
//! The FCH PCI interrupt router: the PCI_INTR index/data table at I/O
//! 0xc00/0xc01 with a PIC and an IOAPIC value per source.
use crate::fch::{Bus, IndexedIo, indexed_io};
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

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

/// PCI_INTR index/data (data at 0xc01).
const PCI_INTR_INDEX: u16 = 0xc00;

register_bitfields![u8,
    Index [SOURCE OFFSET(0) NUMBITS(7) [], MODE OFFSET(7) NUMBITS(1) [Pic = 0, Apic = 1]]
];

/// Which of the two values of a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Pic,
    Apic,
}
fn entry<B: Bus>(bus: &B, source: u8, mode: Mode) -> IndexedIo<'_, B> {
    let mode = match mode {
        Mode::Pic => Index::MODE::Pic,
        Mode::Apic => Index::MODE::Apic,
    };
    indexed_io(bus, PCI_INTR_INDEX, (Index::SOURCE.val(source) + mode).value)
}
/// read_pci_int_idx.
pub fn read(bus: &impl Bus, source: u8, mode: Mode) -> u8 {
    entry(bus, source, mode).get()
}
/// write_pci_int_idx.
pub fn write(bus: &impl Bus, source: u8, mode: Mode, value: u8) {
    entry(bus, source, mode).set(value);
}

/// One board routing entry.
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
pub fn write_routing(bus: &impl Bus, routes: &[Route]) -> Result<(), Error> {
    if let Some(bad) = routes.iter().position(|r| r.source >= ENTRIES) {
        return Err(Error::InvalidSource(bad));
    }
    let mut table = [(NC, NC); ENTRIES as usize];
    for route in routes {
        table[usize::from(route.source)] = (route.pic, route.apic);
    }
    for source in MISC..=MISC2 {
        table[usize::from(source)] = (read(bus, source, Mode::Pic), read(bus, source, Mode::Apic));
    }
    fstart_log::debug!("PCI_INTR: index name PIC APIC");
    for &(source, name) in SOURCES {
        let (pic, apic) = table[usize::from(source)];
        write(bus, source, Mode::Pic, pic);
        let pic = read(bus, source, Mode::Pic);
        write(bus, source, Mode::Apic, apic);
        let apic = read(bus, source, Mode::Apic);
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
pub fn pci_int_line(bus: &impl Bus, source: u8) -> Option<u8> {
    let line = read(bus, source, Mode::Pic);
    (line != NC).then_some(line)
}
