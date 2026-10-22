// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso FCH A-Link and clock sequencing. The index-zero writes are hardware requirements.
use super::registers::{Smn, apply, rmw};
use crate::fch::{AbRegBar, Bus, Fch, MISC_CLK_CNTL1, MiscClkCntl1, PM_AB_REG_BAR, alink};
use tock_registers::interfaces::{ReadWriteable, Writeable};

const ABCFG: &[alink::Rmw] = &[
    (0x00080, 0x00060000, 0x00060001),
    (0x00090, 0x00200000, 0x00200000),
    (0x00094, 0x00100000, 0x00100fee),
    (0x10054, 0x00ffffff, 0x000007ff),
    (0x10060, 0xfbffffff, 0x02000000),
    (0x10090, 0x00010000, 0x00010000),
    (0x10050, 0x00000004, 0x00000000),
    (0x00054, 0x00ff0000, 0x00100000),
    (0x10054, 0x00ff0000, 0x00100000),
    (0x00054, 0x00000010, 0x00000010),
    (0x00054, 0x01000000, 0x01000000),
    (0x10054, 0x03000000, 0x03000000),
    (0x00054, 0x00000001, 0x00000000),
    (0x00054, 0x00000004, 0x00000004),
    (0x00054, 0x00000001, 0x00000001),
    (0x00054, 0x00000004, 0x00000000),
    (0x000bc, 0x00000010, 0x00000010),
    (0x00058, 0x20000000, 0x20000000),
    (0x00058, 0x80000000, 0x80000000),
    (0x00208, 0x00000280, 0x00000280),
    (0x00208, 0x00408010, 0x00408010),
    (0x00180, 0x00000001, 0x00000000),
    (0x00184, 0x00000004, 0x00000000),
    (0x00180, 0x00001f00, 0x00000000),
    (0x00180, 0x00000020, 0x00000000),
    (0x0022c, 0x00000800, 0x00000000),
    (0x0022c, 0x00004000, 0x00000000),
    (0x00228, 0x0000000a, 0x0000000a),
];
/// The A-Link decode at 0xcd8 just for the ABCFG table.
pub fn alink_init(fch: &Fch<impl Bus>) {
    fch.reg(PM_AB_REG_BAR)
        .write(AbRegBar::PORT.val(alink::INDEX.into()));
    fch.reg(MISC_CLK_CNTL1)
        .modify(MiscClkCntl1::ALINK_GATE::CLEAR);
    alink::apply(fch.bus(), ABCFG);
    fch.reg(PM_AB_REG_BAR).write(AbRegBar::PORT.val(0));
}
/// The FCH clock generator through the SMN alias of the MISC bank: vendor
/// values.
pub fn clock_init(smn: &Smn<impl fstart_pci::ConfigRegionAccess>) {
    const MISC: u32 = 0x02d0_1e00;
    let table = [
        (0x10, 0x00000003, 0),
        (0x10, 0x00001ff0, 0),
        (0x34, 0xff800000, 0),
        (0x14, 0x00ffff00, 0),
        (0x18, 0xffff0000, 0),
        (0x1c, 0x0001e000, 0x00008000),
        (0x1c, 0x001e0000, 0x00060000),
        (0x1c, 0x0c000000, 0x0c000000),
        (0x1c, 0x00001e00, 0x00000200),
        (0x40, 1 << 25, 1 << 25),
        (0x40, 1 << 30, 1 << 30),
        (0x10, 1 << 29, 1 << 29),
        (0x08, 1, 1),
        (0x40, 1 << 30, 1 << 30),
    ];
    apply(
        smn,
        table.map(|(offset, clear, set)| rmw(MISC + offset, clear, set)),
    );
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::fch::{
        ACPI_MMIO, Acpimmio, Space,
        testing::{Access, World},
    };
    #[test]
    fn abcfg_transactions_reset_index_after_read_and_write_and_remove_decode() {
        let world = World::default();
        world.set(Space::Mmio, ACPI_MMIO + 0xe40, 4, u32::MAX);
        alink_init(&Fch::new(&world, Acpimmio::DIE0));
        let log = world.0.borrow().log.clone();
        assert_eq!(
            log[..3],
            [
                Access::Write(Space::Mmio, ACPI_MMIO + 0x3e0, 4, 0xcd8),
                Access::Read(Space::Mmio, ACPI_MMIO + 0xe40),
                Access::Write(Space::Mmio, ACPI_MMIO + 0xe40, 4, !2)
            ]
        );
        let mut registers = std::collections::BTreeMap::new();
        let transactions = log[3..log.len() - 1].as_chunks::<6>().0;
        for (events, &(register, clear, set)) in transactions.iter().zip(ABCFG)
        {
            let index = 0xc000_0000 | register;
            let old: u32 = registers.get(&register).copied().unwrap_or(0);
            let value = (old & !clear) | set;
            registers.insert(register, value);
            assert_eq!(
                *events,
                [
                    Access::Write(Space::Io, 0xcd8, 4, index),
                    Access::Read(Space::Io, 0xcdc),
                    Access::Write(Space::Io, 0xcd8, 4, 0),
                    Access::Write(Space::Io, 0xcd8, 4, index),
                    Access::Write(Space::Io, 0xcdc, 4, value),
                    Access::Write(Space::Io, 0xcd8, 4, 0)
                ]
            );
        }
        assert_eq!(
            log.last(),
            Some(&Access::Write(Space::Mmio, ACPI_MMIO + 0x3e0, 4, 0))
        );
    }
}
