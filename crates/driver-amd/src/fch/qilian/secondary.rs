// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the vendor does on each secondary Qilian FCH, 1P (socket 0 die 1)
//! and 2P (socket 1 dies 0 and 1), after relocating its ACPI MMIO
//! ([`super::init_secondary_fch`]): `FchMultiFchDispatchSecondaryInits
//! 0xf48db0` runs HwAcpi → A-Link → SD → USB per FCH (Dell SilVenicePei;
//! GIGABYTE identical) [D] (FCH.md §1.4/8, §2.4).
//!
//! - [`hw_acpi`] (`0xf74a50` → `0xf26a20`);
//! - [`alink`] (`0xf74b10` → `FchSecondaryFchInitAB 0xf481b0`): the BSP's
//!   legacy I/O is redirected to the FCH through the DF while its A-Link
//!   pair answers at `CD8h` (`SMN 0x02D0_13E0`), then both are put back;
//! - [`sd_off`] (`0xf75030`);
//! - USB on socket 1's die-0 FCH only ([`has_usb`], `0xf74de0`): the same
//!   messages as the primary's ([`super::usb`]) to that IOD's SMU. The die-1
//!   FCHs have no USB init.
//!
//! [`init`] runs the first three in order; USB goes with the primary's.

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::hwacpi::{AcpiConfig2, PM_ACPI_CONFIG2};
use crate::df::v4::{IO_BASE, IO_STRIDE, IoBase};
use crate::df::wh::{self as df_wh, IO_LIMIT, IO_PAIRS, IoLimit, fabric_id_offset};
use crate::fch::acpimmio::{
    AoacControl, AoacDevice, MISC, PM, PM_SD_ENABLE, SdEnable, aoac_control,
};
use crate::fch::alink::{self as ab, MISC_CTL, RESET_CPU_ON_SYNC_FLOOD};
use crate::smn::{Smn, SmnReg};
use crate::window::{At, Window};
use fstart_pci::ConfigRegionAccess;

register_bitfields![u32,
    /// SMN 0x02D0_13E0 (32-bit, a secondary FCH's IOHC): the A-Link
    /// index/data I/O base the FCH answers at (its PMxE0).
    pub AbDecode [
        PORT OFFSET(0) NUMBITS(16) []
    ],
    /// MISCx6C (32-bit): bits 27 and 21, set on a secondary (unnamed).
    pub Misc6c [
        BIT21 OFFSET(21) NUMBITS(1) [],
        BIT27 OFFSET(27) NUMBITS(1) []
    ],
    /// PMx08 (32-bit): bit 20, set on a secondary (unnamed).
    pub Pm08 [
        BIT20 OFFSET(20) NUMBITS(1) []
    ],
    /// PMx94 (32-bit): bit 16, set on a secondary (unnamed).
    pub Pm94 [
        BIT16 OFFSET(16) NUMBITS(1) []
    ]
];

/// SMN 0x02D0_13E0 of a secondary FCH's IOHC.
pub const AB_DECODE: SmnReg<AbDecode::Register> = SmnReg::new(0x02d0_13e0);
const MISC_6C: At<u32, Misc6c::Register> = At::new(MISC + 0x6c);
const PM_08: At<u32, Pm08::Register> = At::new(PM + 0x08);
const PM_94: At<u32, Pm94::Register> = At::new(PM + 0x94);

/// The secondary FCHs: socket 0 die 1, socket 1 dies 0 and 1.
pub const fn is_secondary(socket: u8, die: u8) -> bool {
    socket < df_wh::MAX_SOCKETS && die < df_wh::MAX_DIES && (socket | die) != 0
}

/// The FCHs with USB: socket 0's and socket 1's on die 0.
pub const fn has_usb(socket: u8, die: u8) -> bool {
    socket < df_wh::MAX_SOCKETS && die == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not a secondary FCH.
    Fch,
    /// Its ACPI MMIO reads all-ones.
    Unavailable,
    /// No DF I/O pair routes port 0 (the legacy I/O) on the primary IOD.
    LegacyIo,
}

/// What the HwAcpi step sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// PMx74\[17\]: the vendor's AcpiShutdownMessage / ResetCpuOnSyncFlood
    /// flag.
    pub shutdown_message: bool,
}

impl Policy {
    /// openSIL's defaults (sync flood resets).
    pub const DEFAULT: Self = Self {
        shutdown_message: true,
    };
}

/// HwAcpi on a secondary's ACPI MMIO: MISCx6C `|= 0x0820_0000`,
/// PMx74\[17\], PMx08\[20\], PMx94\[16\].
pub fn hw_acpi(acpi: &impl Window, policy: Policy) {
    acpi.at(MISC_6C)
        .modify(Misc6c::BIT21::SET + Misc6c::BIT27::SET);
    acpi.at(PM_ACPI_CONFIG2)
        .modify(AcpiConfig2::SHUTDOWN_MESSAGE.val(policy.shutdown_message.into()));
    acpi.at(PM_08).modify(Pm08::BIT20::SET);
    acpi.at(PM_94).modify(Pm94::BIT16::SET);
}

/// The legacy SD controller off: PMxE8\[0\] and its AOAC power
/// (`0xf75030`: AOACx72\[3\]).
pub fn sd_off(acpi: &impl Window) {
    acpi.at(PM_SD_ENABLE).modify(SdEnable::ENABLE::CLEAR);
    acpi.at(aoac_control(AoacDevice::SD.0))
        .modify(AoacControl::POWER::CLEAR);
}

/// Run `f` with the BSP's legacy I/O routed to FCH `(socket, die)`: the DF
/// I/O pair of node (0, 0) that routes port 0 gets the FCH's fabric ID
/// offset ORed into its destination, base `RE|WE` at 0 (the vendor's I2I
/// `GetLegacyIo`/`SetLegacyIo`, `+0x74`/`+0x78`), and is restored after.
/// No port I/O (the console) reaches the primary FCH meanwhile.
pub fn with_legacy_io<P: ConfigRegionAccess, R>(
    df: &df_wh::Node<P>,
    socket: u8,
    die: u8,
    f: impl FnOnce() -> R,
) -> Result<R, Error> {
    let pair = (0..IO_PAIRS)
        .find(|&pair| {
            let base = df.df().reg(IO_BASE.at(pair, IO_STRIDE)).extract();
            base.get() != u32::MAX
                && base.is_set(IoBase::READ)
                && base.is_set(IoBase::WRITE)
                && base.read(IoBase::ADDRESS) == 0
        })
        .ok_or(Error::LegacyIo)?;
    let (base, limit) = (
        df.df().reg(IO_BASE.at(pair, IO_STRIDE)),
        df.df().reg(IO_LIMIT.at(pair, IO_STRIDE)),
    );
    let saved = (base.get(), limit.get());
    let destination = limit.read(IoLimit::DESTINATION) | u32::from(fabric_id_offset(socket, die));
    limit.modify(IoLimit::DESTINATION.val(destination));
    base.write(IoBase::READ::SET + IoBase::WRITE::SET);
    let result = f();
    limit.set(saved.1);
    base.set(saved.0);
    Ok(result)
}

/// `FchSecondaryFchInitAB`: `io` reaches the secondary's A-Link pair
/// ([`with_legacy_io`]), `acpi` is its ACPI MMIO, `smn` its IOD's SMN.
pub fn alink<P: ConfigRegionAccess>(io: &impl Window, acpi: &impl Window, smn: &Smn<P>) {
    smn.reg(AB_DECODE)
        .write(AbDecode::PORT.val(ab::INDEX.into()));
    ab::modify(io, 0x00090, 0, 1 << 21);
    ab::modify(io, 0x10054, 0x00ff_ffff, 0x7ff);
    ab::modify(io, 0x10060, 1 << 26, 1 << 25);
    ab::modify(io, 0x10090, 0, 1 << 16);
    ab::memory_power_saving(io, acpi);
    // `FchAbEnvCommon 0xf47ae0` at the defaults (every gate on).
    ab::env_common(io, acpi, smn, &ab::QILIAN);
    ab::modify(io, MISC_CTL, RESET_CPU_ON_SYNC_FLOOD, 0);
    smn.reg(AB_DECODE).set(0);
}

/// HwAcpi, A-Link and SD off on secondary FCH `(socket, die)`, in the
/// vendor's order. `df` is DF node (0, 0) (the BSP's), `io` the host's
/// ports, `acpi` the FCH's relocated ACPI MMIO, `smn` its IOD's SMN.
pub fn init<P: ConfigRegionAccess, Q: ConfigRegionAccess>(
    df: &df_wh::Node<P>,
    socket: u8,
    die: u8,
    io: &impl Window,
    acpi: &impl Window,
    smn: &Smn<Q>,
    policy: Policy,
) -> Result<(), Error> {
    if !is_secondary(socket, die) || (df.socket(), df.die()) != (0, 0) {
        return Err(Error::Fch);
    }
    if acpi.read32(PM) == u32::MAX {
        return Err(Error::Unavailable);
    }
    hw_acpi(acpi, policy);
    with_legacy_io(df, socket, die, || alink(io, acpi, smn))?;
    sd_off(acpi);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::Mem;
    use crate::smn::model::Model;

    fn df_with_legacy_pair(pci: &Model) {
        let set = |off: u16, v: u32| {
            pci.0.borrow_mut().config.insert((0, 0x18, 0, off), v);
        };
        // Pair 0: 0x1000-0xFFFF to another root; pair 1: legacy to 0x20.
        set(0xd00, 0x0001_0003);
        set(0xd04, 0x000f_0021);
        set(0xd08, 0x0000_0003);
        set(0xd0c, 0x0000_0020);
    }

    #[test]
    fn redirects_the_legacy_pair_and_restores_it() {
        let pci = Model::default();
        df_with_legacy_pair(&pci);
        let df = unsafe { df_wh::Node::new(pci.clone(), 0, 0) }.unwrap();
        let cfg = |off| pci.0.borrow().config[&(0, 0x18, 0, off)];
        let during = with_legacy_io(&df, 1, 0, || (cfg(0xd08), cfg(0xd0c))).unwrap();
        assert_eq!(during, (0x3, 0xa0));
        assert_eq!((cfg(0xd08), cfg(0xd0c)), (0x3, 0x20));
        let during = with_legacy_io(&df, 1, 1, || cfg(0xd0c)).unwrap();
        assert_eq!(during, 0xe0);
        assert_eq!(with_legacy_io(&df, 0, 1, || cfg(0xd0c)), Ok(0x60));
        // Pair 0 is not the legacy one and is left alone.
        assert_eq!(cfg(0xd04), 0x000f_0021);
        let empty = Model::default();
        let df = unsafe { df_wh::Node::new(empty, 0, 0) }.unwrap();
        assert_eq!(with_legacy_io(&df, 0, 1, || ()), Err(Error::LegacyIo));
    }

    #[test]
    fn the_secondary_sequence_in_order() {
        let pci = Model::default();
        df_with_legacy_pair(&pci);
        let df = unsafe { df_wh::Node::new(pci.clone(), 0, 0) }.unwrap();
        let acpi = Mem::default();
        acpi.put(MISC + 0x68, 1, 0x06);
        acpi.put(0x1e72, 1, 0x0f);
        acpi.put(PM + 0xe8, 1, 0x01);
        let io = Mem::ports();
        io.set_at(0xcdc, 6 << 29 | 0x10050, 0x0c);
        let smn = unsafe { Smn::on_bus(pci.clone(), 0x40) };
        init(&df, 0, 1, &io, &acpi, &smn, Policy::DEFAULT).unwrap();
        // HwAcpi.
        assert_eq!(acpi.get(MISC + 0x6c, 4), 0x0820_0000);
        assert_eq!(acpi.get(PM + 0x76, 1), 0x02);
        assert_eq!(acpi.get(PM + 0x08, 4), 1 << 20);
        assert_eq!(acpi.get(PM + 0x94, 4), 1 << 16);
        // A-Link: the decode was opened and closed on the FCH's IOD.
        let writes: Vec<_> = crate::fch::model::smn_writes(&pci, 0x40)
            .into_iter()
            .filter(|w| w.0 == 0x02d0_13e0)
            .collect();
        assert_eq!(writes, [(0x02d0_13e0, 0xcd8), (0x02d0_13e0, 0)]);
        assert_eq!(crate::fch::model::smn(&pci, 0x40, 0x02d0_001c), 1 << 31);
        let abcfg = |r: u32| io.at(0xcdc, 6 << 29 | r);
        assert_eq!(abcfg(0x90), 1 << 21);
        assert_eq!(abcfg(0x10054), 0x0300_07ff);
        assert_eq!(abcfg(0x10060), 1 << 25);
        assert_eq!(abcfg(0x54), 0x0100_0014);
        assert_eq!(abcfg(0x58), 0xa000_0000);
        assert_eq!(abcfg(0x208), 0x40_8280);
        assert_eq!(abcfg(0x10050), 0x08);
        assert_eq!(acpi.get(MISC + 0x68, 1), 0x00);
        // SD off.
        assert_eq!(acpi.get(PM + 0xe8, 1), 0);
        assert_eq!(acpi.get(0x1e72, 1), 0x07);
        // The legacy route is back.
        assert_eq!(pci.0.borrow().config[&(0, 0x18, 0, 0xd0c)], 0x20);
    }

    #[test]
    fn refuses_the_primary_and_dead_fchs() {
        let pci = Model::default();
        df_with_legacy_pair(&pci);
        let df = unsafe { df_wh::Node::new(pci.clone(), 0, 0) }.unwrap();
        let smn = unsafe { Smn::on_bus(pci.clone(), 0x40) };
        let io = Mem::ports();
        let acpi = Mem::default();
        assert_eq!(
            init(&df, 0, 0, &io, &acpi, &smn, Policy::DEFAULT),
            Err(Error::Fch)
        );
        acpi.put(PM, 4, u32::MAX);
        assert_eq!(
            init(&df, 1, 1, &io, &acpi, &smn, Policy::DEFAULT),
            Err(Error::Unavailable)
        );
        assert!(acpi.writes().is_empty());
        assert!(has_usb(1, 0) && has_usb(0, 0) && !has_usb(1, 1) && !has_usb(0, 1));
    }
}
