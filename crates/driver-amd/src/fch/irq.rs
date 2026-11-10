// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared FCH PCI_INTR indexed transport (C00/C01). Source lists and
//! routing/control policy belong to the actual FCH/platform, not this access.
use super::{Bus, IndexedIo, PCI_INTR_INDEX, indexed_io};
use tock_registers::{
    RegisterLongName,
    interfaces::{Readable, Writeable},
    register_bitfields,
};
register_bitfields![u8, Index [SOURCE OFFSET(0) NUMBITS(7) [],MODE OFFSET(7) NUMBITS(1) [Pic=0,Apic=1]]];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Pic,
    Apic,
}
pub fn register<B: Bus, R: RegisterLongName>(
    bus: &B,
    source: u8,
    mode: Mode,
) -> IndexedIo<'_, B, R> {
    let mode = match mode {
        Mode::Pic => Index::MODE::Pic,
        Mode::Apic => Index::MODE::Apic,
    };
    indexed_io(
        bus,
        PCI_INTR_INDEX,
        (Index::SOURCE.val(source) + mode).value,
    )
}
pub fn read(bus: &impl Bus, source: u8, mode: Mode) -> u8 {
    register::<_, ()>(bus, source, mode).get()
}
pub fn write(bus: &impl Bus, source: u8, mode: Mode, value: u8) {
    register::<_, ()>(bus, source, mode).set(value);
}

/// PCI_INTR 08h/09h as the AMD FCH legacy init leaves them (Picasso
/// AGESA, openSIL FCH, NFX FchSongshanPei; both Rs vendor firmwares read
/// back FAh/91h): the keyboard, mouse and IRQ14/15 IOAPIC inputs come from
/// their internal sources; index 09h is (old & 99h) | 10h.
pub fn route_legacy(bus: &impl Bus) {
    use super::{IntrMiscMap, PciIntr09};
    use tock_registers::interfaces::ReadWriteable;
    register::<_, IntrMiscMap::Register>(bus, 0x08, Mode::Pic).modify(
        IntrMiscMap::IRQ1_ROUTED::SET
            + IntrMiscMap::IRQ12_ROUTED::SET
            + IntrMiscMap::IRQ14::Routed
            + IntrMiscMap::IRQ15::Routed,
    );
    register::<_, PciIntr09::Register>(bus, 0x09, Mode::Pic)
        .modify(PciIntr09::CONTROL_2_1.val(0) + PciIntr09::CONTROL_6_4.val(1));
}
