// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native common/block/smbus/tco.c, independent of ACPI PMBASE.
use super::{
    early::PowerState,
    pcr::{Pcr, Port},
    pmc::MemoryBootMode,
};
use crate::southbridge::pmio_ich::{TCO1_CNT, TcoIo};
use fstart_core::pio::PioRegister;
use fstart_pci::EcamDevice;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

pub const BASE: u16 = 0x400;
register_bitfields![u32,
    // SMBus PCI TCOCTL, offset 0x54 (32-bit). LOCK protects the base,
    // not the TCO SMI enable; that is locked through TCO1_CNT later.
    Control [
        LOCK OFFSET(0) NUMBITS(1) [],
        ENABLE OFFSET(8) NUMBITS(1) []
    ],
    DmiDecode [
        ENABLE OFFSET(1) NUMBITS(1) [],
        BASE OFFSET(5) NUMBITS(11) []
    ]
];
register_bitfields![u16, Intrusion [ ROUTE OFFSET(1) NUMBITS(2) [
    Disabled = 0,
    Smi = 2
] ]];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Device,
    Locked,
    Retained,
    Readback,
    Pcr(super::pcr::Error),
}
impl From<super::pcr::Error> for Error {
    fn from(error: super::pcr::Error) -> Self {
        Self::Pcr(error)
    }
}
fn require(device: &EcamDevice) -> Result<(), Error> {
    if device.address().segment() != 0
        || device.bus() != 0
        || device.dev() != 0x1f
        || device.func() != 4
        || device.try_read16(0x00).ok_or(Error::Device)? != 0x8086
    {
        return Err(Error::Device);
    }
    Ok(())
}
/// Non-writing decode check, also usable by the permanent SMM image without
/// a stage-global ECAM binding. Family ownership belongs to the PCH owner.
pub fn validate_existing(device: &EcamDevice, pcr: &Pcr) -> Result<(), Error> {
    require(device)?;
    let control =
        unsafe { device.try_register::<u32, Control::Register>(0x54) }.ok_or(Error::Device)?;
    let dmi = pcr.register::<DmiDecode::Register>(Port::Dmi, 0x2778)?;
    if device.try_read32(0x50).ok_or(Error::Device)? & 0xffff_ffe0 != u32::from(BASE)
        || !control.is_set(Control::ENABLE)
        || !dmi.matches_all(DmiDecode::BASE.val(u32::from(BASE) >> 5) + DmiDecode::ENABLE::SET)
    {
        return Err(Error::Readback);
    }
    Ok(())
}
/// # Safety
/// Own actual PCH-H SMBus PCI config, DMI PCR and the reserved TCO I/O window;
/// saved classification must precede watchdog/status programming. Not WDAT.
pub(crate) unsafe fn configure_cold(
    device: &EcamDevice,
    pcr: &Pcr,
    power: PowerState,
    intrusion_smi: bool,
) -> Result<(), Error> {
    if power.mode() != MemoryBootMode::Cold {
        return Err(Error::Retained);
    }
    require(device)?;
    let dmi = pcr.register::<DmiDecode::Register>(Port::Dmi, 0x2778)?;
    let control = unsafe { device.register::<u32, Control::Register>(0x54) };
    // 332691 6.1.18: TCO_BASE_LOCK is RW/O. On the M900 the first TCOCTL
    // write consumes it even when it writes 0 to bit 0, so the native
    // clear-EN/set-EN sequence leaves the base unlockable. PLTRST# leaves
    // TCOCTL at 0 (decode off), so program TCOBASE first and make the one
    // TCOCTL write enable and lock together. TCO1_CNT stays unlocked for SMM.
    let reset = control.get();
    if reset & Control::LOCK::SET.value != 0 {
        return Err(Error::Locked);
    }
    if reset != 0 {
        fstart_log::warn!("TCO: TCOCTL={:#010x} before first write", reset);
    }
    let base = unsafe { device.register::<u32, ()>(0x50) };
    base.set(u32::from(BASE));
    control.write(Control::ENABLE::SET + Control::LOCK::SET);
    dmi.write(DmiDecode::BASE.val(u32::from(BASE) >> 5) + DmiDecode::ENABLE::SET);
    validate_existing(device, pcr)?;
    let tco = TcoIo::new(BASE);
    tco.halt_and_clear_timeouts();
    let intrusion = PioRegister::<u16, Intrusion::Register>::new(BASE + 0x0a);
    let route = if intrusion_smi { 2 } else { 0 };
    intrusion.modify(Intrusion::ROUTE.val(route));
    if !tco.is_halted() || intrusion.read(Intrusion::ROUTE) != route {
        return Err(Error::Readback);
    }
    let readback = control.extract();
    if readback.get() == u32::MAX || !readback.is_set(Control::LOCK) {
        // Finalization still requires both locks and never hands off with
        // just the TCO1_CNT lock.
        // Distinguish an unreadable lock from a missing one: a locked
        // TCOBASE ignores this write. Restore at once if it moved.
        let probe = u32::from(BASE) + 0x20;
        base.set(probe);
        let moved = base.get() & 0xffff_ffe0;
        if moved != u32::from(BASE) {
            base.set(u32::from(BASE));
        }
        fstart_log::warn!(
            "TCO: early base lock did not latch (TCOCTL {:#010x}->{:#010x}, TCOBASE probe {:#x} read {:#x})",
            reset,
            readback.get(),
            probe,
            moved
        );
        validate_existing(device, pcr)?;
    } else {
        fstart_log::debug!("TCO: fixed base locked during cold decode setup");
    }
    Ok(())
}
/// Verify the base lock and lock TCO SMI policy after permanent SMM is installed.
pub(crate) fn lockdown(device: &EcamDevice, pcr: &Pcr) -> Result<(), Error> {
    if let Err(error) = validate_existing(device, pcr) {
        let dmi = pcr
            .register::<DmiDecode::Register>(Port::Dmi, 0x2778)?
            .get();
        fstart_log::error!(
            "TCO decode before lock: TCOBASE={:#010x} TCOCTL={:#010x} DMI={:#010x}",
            device.read32(0x50),
            device.read32(0x54),
            dmi
        );
        return Err(error);
    }
    let control = unsafe { device.register::<u32, Control::Register>(0x54) };
    let tco = TcoIo::new(BASE);
    let pci_before = control.extract();
    let io_before = tco.read16(TCO1_CNT);
    if pci_before.get() == u32::MAX || io_before == u16::MAX || !tco.is_halted() {
        fstart_log::error!(
            "TCO not ready to lock: TCOCTL={:#010x} TCO1_CNT={:#06x}",
            pci_before.get(),
            io_before
        );
        return Err(Error::Readback);
    }
    if !pci_before.is_set(Control::LOCK) {
        control.modify(Control::LOCK::SET);
    }
    tco.lock();
    let pci_after = control.extract();
    let io_after = tco.read16(TCO1_CNT);
    if pci_after.get() == u32::MAX
        || !pci_after.matches_all(Control::ENABLE::SET + Control::LOCK::SET)
        || io_after == u16::MAX
        || !tco.is_locked()
        || !tco.is_halted()
    {
        fstart_log::error!(
            "TCO lock readback: TCOCTL {:#010x}->{:#010x} TCO1_CNT {:#06x}->{:#06x}",
            pci_before.get(),
            pci_after.get(),
            io_before,
            io_after
        );
        return Err(Error::Readback);
    }
    Ok(())
}
