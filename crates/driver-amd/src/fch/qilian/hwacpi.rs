// SPDX-License-Identifier: MIT OR Apache-2.0
//! Qilian's HW-ACPI reset and env tables, its legacy interrupt routes, the
//! A-Link index decode and the sync-flood setup, from SilVenicePei
//! (openSIL xUSL; FCH.md §2.4, §2.5, §2.8, §1.6/15).
//!
//! The tables are generated from `reports/research/FCH/hwacpi-mmio-tables.json`
//! (`tools/fch_tables.py OUT dell=dell-SilVenicePei.pe gbt=gbt-SilVenicePei.pe`,
//! `FchHwAcpiWriteMmioTable` records `{block, reg, and, or}`: the byte at
//! ACPI MMIO `block << 8 | reg` becomes `(x & and) | or`, here
//! `Op::rmw(offset, 1, !and, or)`, a plain store when `and` is 0). The Dell
//! PR9XJ 1.0.8 build (sha256 `6a838139f1b5ef20`, VenicePI 1.0.0.0) and the
//! GIGABYTE MD25-CG0 F01 build (`e878d40a6ac60906`, PI 0.0.8.1) carry them
//! byte-identical [I]. Do not edit.
//!
//! Order on the primary FCH (`FchInitResetHwAcpiQl 0xf26650`, [D]): the PM
//! blocks (the flow's [`crate::fch::acpimmio::acpi_ports`]), [`RESET`], the
//! RTC extension's DST registers if invalid ([`RTC_EXT`], not applied: the
//! validity test is not recovered), [`PRE_PCIE`], [`RESET2`]; the env
//! (`0xf26cf0`) writes [`PCI_INTR`] then [`ENV2`]. [`PRELIMINARY`] runs
//! before them all (`InitializeFchHwAcpiPreliminaryQlTp1`). Not ported, the
//! semantics being open: the boot timer (`PMx44/47`), the SMBus function's
//! `x10/x11` writes and the cold-reset watchdog acknowledge (`PMxC6`) of
//! `PrePcieInit`, the spread-spectrum override.

use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::register_bitfields;

use crate::fch::FchHw;
use crate::fch::acpimmio::{AbRegBar, PM, PM_AB_REG_BAR};
use crate::fch::alink;
use crate::fch::irq;
use crate::fch::songshan::IntrMisc;
use crate::smu::{self, SmuMessages};
use crate::window::{At, Op, Window, apply};

/// `InitializeFchHwAcpiPreliminaryQlTp1` (Dell VA `0xf93df0`, GIGABYTE
/// `0x709d27c8`): AOACx95\[7:6\], PMx0A\[4\], CF9 resets on (PMxD2\[6\]),
/// the S5 reset status's power-button bit cleared.
pub const PRELIMINARY: &[Op] = &[
    Op::rmw(0x1e95, 1, 0xc0, 0x0),
    Op::rmw(0x030a, 1, 0x10, 0x10),
    Op::rmw(0x03d2, 1, 0x40, 0x0),
    Op::store(0x03c0, 1, 0x2),
];

/// `FchInitResetHwAcpiQl` reset table (Dell `0xf93db8`, GIGABYTE
/// `0x709d2790`). PMx00 gets `0x37` (Genoa's openSIL `0x77`): the HPET
/// decode is not set here.
pub const RESET: &[Op] = &[
    Op::rmw(0x0302, 1, 0x4, 0x4),
    Op::rmw(0x03d2, 1, 0x30, 0x0),
    Op::rmw(0x0300, 1, 0x8, 0x37),
    Op::rmw(0x0308, 1, 0x10, 0x10),
    Op::rmw(0x0310, 1, 0x3, 0x0),
    Op::store(0x0354, 1, 0x50),
    Op::rmw(0x0374, 1, 0x9, 0x1),
    Op::rmw(0x0377, 1, 0x0, 0x6),
    Op::rmw(0x0e6f, 1, 0x8, 0x8),
];

/// The RTC extension's DST registers cleared through PMx5E/5F, when the
/// vendor finds them invalid (Dell `0xf93d60`, GIGABYTE `0x709d2738`).
pub const RTC_EXT: &[Op] = &[
    Op::store(0x035e, 1, 0x0),
    Op::store(0x035f, 1, 0x0),
    Op::store(0x035e, 1, 0x1),
    Op::store(0x035f, 1, 0x0),
    Op::store(0x035e, 1, 0x2),
    Op::store(0x035f, 1, 0x0),
    Op::store(0x035e, 1, 0x3),
    Op::store(0x035f, 1, 0x0),
    Op::store(0x035e, 1, 0x4),
    Op::store(0x035f, 1, 0x0),
    Op::store(0x035e, 1, 0x10),
    Op::store(0x035f, 1, 0x0),
    Op::store(0x035e, 1, 0x11),
    Op::store(0x035f, 1, 0x0),
    Op::store(0x035e, 1, 0x12),
    Op::store(0x035f, 1, 0x0),
    Op::store(0x035e, 1, 0x13),
    Op::store(0x035f, 1, 0x0),
    Op::store(0x035e, 1, 0x14),
    Op::store(0x035f, 1, 0x0),
];

/// `PrePcieInit`'s table (Dell `0xfab52c`, GIGABYTE `0x709e8b74`).
pub const PRE_PCIE: &[Op] = &[
    Op::rmw(0x03bd, 1, 0x8, 0x8),
    Op::rmw(0x0396, 1, 0x20, 0x20),
    Op::rmw(0x03ca, 1, 0x1, 0x1),
    Op::rmw(0x03c8, 1, 0x2, 0x2),
];

/// The second reset table (Dell `0xf93d18`, GIGABYTE `0x709d26f0`): PMx74
/// = `17h`, PMx77, the SCI map of the GEVENTs (SMIx41..x70), SMIx08.
pub const RESET2: &[Op] = &[
    Op::store(0x0374, 1, 0x17),
    Op::rmw(0x0377, 1, 0x20, 0x0),
    Op::rmw(0x03ba, 1, 0x8, 0x8),
    Op::rmw(0x0e6e, 1, 0x20, 0x20),
    Op::store(0x0241, 1, 0x1),
    Op::store(0x0243, 1, 0x3),
    Op::store(0x0244, 1, 0x4),
    Op::store(0x0245, 1, 0x5),
    Op::store(0x0246, 1, 0x6),
    Op::store(0x0257, 1, 0x17),
    Op::store(0x0248, 1, 0x18),
    Op::rmw(0x0208, 1, 0x18, 0x0),
    Op::store(0x0270, 1, 0x9),
    Op::store(0x026c, 1, 0xd),
    Op::store(0x025d, 1, 0x10),
    Op::store(0x025e, 1, 0x11),
];

/// The env table after the interrupt routes (Dell `0xf93f0c`, GIGABYTE
/// `0x709d28ec`).
pub const ENV2: &[Op] = &[
    Op::rmw(0x033e, 1, 0x4, 0x4),
    Op::store(0x03c3, 1, 0x20),
    Op::rmw(0x0e3a, 1, 0x4, 0x0),
];

/// The A-Link RAS table (Dell `0xf93de4`, GIGABYTE `0x709d27bc`): SERR as
/// SMI (SMIxB5), with ABCFG `0x10050[3]`; not applied by default.
pub const RAS: &[Op] = &[Op::rmw(0x02b5, 1, 0x3, 0x1)];

/// The legacy interrupt routes `(PCI_INTR index, IRQ)` in the vendor's
/// order (Dell `0xf93e08`, GIGABYTE identical [I]): the IOAPIC's INTA–D not
/// routed, SCI/SMBus, GPIO, the UARTs and the I2C controllers.
pub const PCI_INTR: &[(u8, u8)] = &[
    (0x80, 0x1f),
    (0x81, 0x1f),
    (0x82, 0x1f),
    (0x83, 0x1f),
    (0x97, 0x10),
    (0x9a, 0x10),
    (0xe2, 0x07),
    (0xf4, 0x03),
    (0xf5, 0x04),
    (0xf8, 0x03),
    (0xf9, 0x04),
    (0xf0, 0x0a),
    (0xf1, 0x0b),
    (0xf2, 0x04),
    (0xf3, 0x06),
    (0xf6, 0x0e),
    (0xf7, 0x0f),
];

register_bitfields![u8,
    /// PMx76: PMx74\[23:16\] (unnamed in public; FCH.md §2.4, §1.6/15).
    pub AcpiConfig2 [
        /// PMx74\[17\]: AcpiShutdownMessage / ResetCpuOnSyncFlood flag
        /// of a secondary FCH's HW-ACPI init.
        SHUTDOWN_MESSAGE OFFSET(1) NUMBITS(1) [],
        /// PMx74\[18\]: set by `FchAbHandleSyncFloodQl`.
        SYNC_FLOOD OFFSET(2) NUMBITS(1) []
    ]
];

/// PMx76.
pub const PM_ACPI_CONFIG2: At<u8, AcpiConfig2::Register> = At::new(PM + 0x76);

/// The primary FCH's HW-ACPI reset: [`PRELIMINARY`], [`RESET`],
/// [`PRE_PCIE`], [`RESET2`], after the PM blocks are set.
pub fn reset(acpi: &impl Window) {
    for table in [PRELIMINARY, RESET, PRE_PCIE, RESET2] {
        apply(acpi, table);
    }
}

/// The env part: [`PCI_INTR`], then [`ENV2`]. A route to IRQ 14 or 15
/// takes the IOAPIC input from the IDE (PCI_INTR 08h, as the vendor).
pub fn env(fch: &impl FchHw) {
    irq::program(fch.io(), PCI_INTR);
    for (irq, routed) in [
        (14u8, IntrMisc::IRQ14::Routed),
        (15, IntrMisc::IRQ15::Routed),
    ] {
        if PCI_INTR.iter().any(|&(_, value)| value == irq) {
            irq::update(fch.io(), irq::MISC, routed.mask(), routed.value);
        }
    }
    apply(fch.acpi(), ENV2);
}

/// `FchInitResetAbSil 0xf483b0`: the A-Link index/data pair at `CD8h`
/// (PMxE0).
pub fn ab_decode(acpi: &impl Window) {
    acpi.at(PM_AB_REG_BAR)
        .write(AbRegBar::PORT.val(u32::from(alink::INDEX)));
}

/// What a sync flood does: reset the CPUs (through the SMU), after `delay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncFlood {
    pub reset_cpu: bool,
    /// Sent as 0 below 5.
    pub delay: u32,
}

impl SyncFlood {
    /// openSIL's defaults: reset, no delay.
    pub const DEFAULT: Self = Self {
        reset_cpu: true,
        delay: 0,
    };
}

/// `FchAbHandleSyncFloodQl 0xf74640` [D]: PMx74\[18\], the A-Link's own
/// reset-on-sync-flood (ABCFG `0x10050[2]`) off, the reset handed to the
/// SMU (`SetResetCpuOnSyncFlood`, then `SetDelayResetCpuOnSyncFlood` when
/// it resets). The vendor's `SetCaliptraErrorReporting` (`FFh`) is above the
/// SMU's message limit and never sent. `fch.io()` must reach the A-Link
/// pair ([`ab_decode`]).
pub fn sync_flood(
    fch: &impl FchHw,
    smu: &impl SmuMessages,
    policy: SyncFlood,
) -> Result<(), smu::Error> {
    use crate::smu::v14::msg::{SET_DELAY_RESET_CPU_ON_SYNC_FLOOD, SET_RESET_CPU_ON_SYNC_FLOOD};
    fch.acpi()
        .at(PM_ACPI_CONFIG2)
        .modify(AcpiConfig2::SYNC_FLOOD::SET);
    alink::modify(fch.io(), alink::MISC_CTL, alink::RESET_CPU_ON_SYNC_FLOOD, 0);
    let mut args = [u32::from(policy.reset_cpu), 0, 0, 0, 0, 0];
    smu.send(SET_RESET_CPU_ON_SYNC_FLOOD, &mut args)?;
    if policy.reset_cpu {
        let delay = if policy.delay < 5 { 0 } else { policy.delay };
        let mut args = [delay, 0, 0, 0, 0, 0];
        smu.send(SET_DELAY_RESET_CPU_ON_SYNC_FLOOD, &mut args)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::{Smu, fch};

    #[test]
    fn reset_tables_leave_the_vendor_values() {
        let hw = fch();
        hw.acpi.put(PM, 1, 0x48);
        hw.acpi.put(PM + 0xd2, 1, 0x7f);
        hw.acpi.put(PM + 0x74, 1, 0xff);
        reset(&hw.acpi);
        // PMx00: bit 3 off, 0x37 on, HPET (bit 6) left as it was.
        assert_eq!(hw.acpi.get(PM, 1), 0x77);
        // PMxD2: CF9 reset disable and bits 5:4 cleared.
        assert_eq!(hw.acpi.get(PM + 0xd2, 1), 0x0f);
        // RESET's PMx74 rmw, then RESET2's store.
        assert_eq!(hw.acpi.writes_to(PM + 0x74), [0xf7, 0x17]);
        assert_eq!(hw.acpi.get(0x241, 1), 1);
        assert_eq!(hw.acpi.get(0x3c0, 1), 2);
        assert_eq!(hw.acpi.get(0x3c8, 1), 2);
    }

    #[test]
    fn env_routes_the_uarts_and_i2c_and_takes_irq14_15() {
        let hw = fch();
        env(&hw);
        let route = |i: u32| hw.io.at(0xc01, i);
        assert_eq!(route(0x80), 0x1f);
        assert_eq!(route(0xf4), 3);
        assert_eq!(route(0xf9), 4);
        assert_eq!(route(0xf7), 0xf);
        assert_eq!(route(0x08), 0xf0);
        assert_eq!(hw.acpi.get(PM + 0xc3, 1), 0x20);
    }

    #[test]
    fn sync_flood_moves_the_reset_to_the_smu() {
        let hw = fch();
        hw.io.set_at(0xcdc, 6 << 29 | alink::MISC_CTL, 0x0c);
        let smu = Smu::default();
        sync_flood(&hw, &smu, SyncFlood::DEFAULT).unwrap();
        assert_eq!(hw.acpi.get(PM + 0x76, 1), 0x04);
        assert_eq!(hw.io.at(0xcdc, 6 << 29 | alink::MISC_CTL), 0x08);
        assert_eq!(
            *smu.0.borrow(),
            [(0x3b, [1, 0, 0, 0, 0, 0]), (0x4d, [0; 6])]
        );
        let smu = Smu::default();
        sync_flood(
            &hw,
            &smu,
            SyncFlood {
                reset_cpu: false,
                delay: 9,
            },
        )
        .unwrap();
        assert_eq!(smu.ids(), [0x3b]);
        let smu = Smu::default();
        sync_flood(
            &hw,
            &smu,
            SyncFlood {
                reset_cpu: true,
                delay: 9,
            },
        )
        .unwrap();
        assert_eq!(smu.0.borrow()[1].1[0], 9);
    }

    #[test]
    fn ab_decode_is_cd8() {
        let hw = fch();
        ab_decode(&hw.acpi);
        assert_eq!(hw.acpi.get(PM + 0xe0, 4), 0xcd8);
    }
}
