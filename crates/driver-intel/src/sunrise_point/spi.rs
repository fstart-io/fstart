// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native fast_spi.c configuration/locks; no flash cycle or download engine.

use super::{
    early::{PowerState, SPI_BASE},
    pcr::{self, Pcr, Port},
    pmc::MemoryBootMode,
};
use fstart_core::mmio::{MmioReadOnly as RO, MmioReadWrite as RW, MmioRegister};
use fstart_pci::{EcamDevice, PCI_COMMAND_BITS};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
};

mod media;
pub(crate) use media::Observation;

/// Who may write the BIOS region once firmware hands off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BiosWrites {
    /// The OS may flash in band (flashrom/rflasher `internal`), as on the
    /// other Intel southbridges. BIOS write enable stays set, without EISS or
    /// BIOS Lock Enable.
    Open,
    /// coreboot's `BOOTMEDIA_SMM_BWP`: EISS plus BIOS Lock Enable, and the
    /// SMM handler denies every write enable. fstart has no SMM flash
    /// service, so after handoff nothing can write the BIOS region.
    SmmOnly,
}

pub const BIOS_CONTROL: u16 = 0xdc;
register_bitfields![u32,
    pub BiosControl [
        WRITE_ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK_ENABLE OFFSET(1) NUMBITS(1) [],
        CACHE_DISABLE OFFSET(2) NUMBITS(1) [],
        PREFETCH OFFSET(3) NUMBITS(1) [],
        EISS OFFSET(5) NUMBITS(1) [],
        INTERFACE_LOCK OFFSET(7) NUMBITS(1) [],
        SYNC_STATUS OFFSET(8) NUMBITS(1) [],
        EXT_ENABLE OFFSET(27) NUMBITS(1) [],
        EXT_LOCK OFFSET(28) NUMBITS(1) []
    ],
    FlashRegion [
        BASE OFFSET(0) NUMBITS(15) [],
        LIMIT OFFSET(16) NUMBITS(15) []
    ],
    HardwareSequence [
        STATUS_W1C OFFSET(0) NUMBITS(9) [],
        IN_PROGRESS OFFSET(5) NUMBITS(1) [],
        WRITE_STATUS_DISABLE OFFSET(11) NUMBITS(1) [],
        RANGE34_LOCK OFFSET(12) NUMBITS(1) [],
        DESCRIPTOR_VALID OFFSET(14) NUMBITS(1) [],
        LOCK OFFSET(15) NUMBITS(1) [],
        GO OFFSET(16) NUMBITS(1) []
    ],
    DiscreteLock [ RANGES OFFSET(8) NUMBITS(5) [] ],
    VendorCapability [ LOCK OFFSET(30) NUMBITS(1) [] ],
    NativeClock [
        LOW OFFSET(0) NUMBITS(6) [],
        MID OFFSET(8) NUMBITS(3) [],
        BIT29 OFFSET(29) NUMBITS(1) []
    ],
    NativePower [ GATING OFFSET(0) NUMBITS(3) [] ],
    OpcodeMenu [
        S0 OFFSET(0) NUMBITS(8) [],
        S1 OFFSET(8) NUMBITS(8) [],
        S2 OFFSET(16) NUMBITS(8) [],
        S3 OFFSET(24) NUMBITS(8) []
    ]
];
register_bitfields![u16,
    BiosHide [
        SYNC_STATUS OFFSET(8) NUMBITS(1) [],
        OS_HIDE OFFSET(9) NUMBITS(1) []
    ],
    Prefix [
        WREN OFFSET(0) NUMBITS(8) [],
        EWSR OFFSET(8) NUMBITS(8) []
    ],
    OpcodeType [
        S0 OFFSET(0) NUMBITS(2) [],
        S1 OFFSET(2) NUMBITS(2) [],
        S2 OFFSET(4) NUMBITS(2) [],
        S3 OFFSET(6) NUMBITS(2) [],
        S4 OFFSET(8) NUMBITS(2) [],
        S5 OFFSET(10) NUMBITS(2) [],
        S6 OFFSET(12) NUMBITS(2) [],
        S7 OFFSET(14) NUMBITS(2) []
    ]
];
register_structs! {
    Registers {
        (0x00 => primary_region: RO<u32, FlashRegion::Register>),
        (0x04 => sequencing: RW<u32, HardwareSequence::Register>),
        (0x08 => _reserved1),
        (0x0c => discrete_lock: RW<u32, DiscreteLock::Register>),
        (0x10 => _reserved2),
        // Intel 332691-002 sections 7.2.22..27: the six documented FREGs.
        (0x54 => regions: [RO<u32, FlashRegion::Register>; 6]),
        (0x6c => _reserved_regions),
        (0xa4 => prefix: RW<u16, Prefix::Register>),
        (0xa6 => opcode_type: RW<u16, OpcodeType::Register>),
        (0xa8 => menu_low: RW<u32, OpcodeMenu::Register>),
        (0xac => menu_high: RW<u32, OpcodeMenu::Register>),
        (0xb0 => _reserved3),
        (0xb4 => descriptor_control: RW<u32, media::DescriptorSelect::Register>),
        (0xb8 => descriptor_data: RO<u32>),
        (0xbc => _reserved_observer),
        (0xc4 => vendor_capability: RW<u32, VendorCapability::Register>),
        (0xc8 => _reserved4),
        (0x1000 => @END),
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Device,
    Decode,
    Retained,
    Locked,
    Busy,
    Readback,
    Descriptor,
    Pcr(pcr::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}

/// Native pch.c ESPISPI power recipe; no flash transaction or lock change.
/// # Safety
/// Actual mapped SPI/PCR owned exclusively before final P2SB masks. Host
/// sequencer must be idle; that is not proof of CSE/ME flash or DMA drain.
pub(crate) unsafe fn configure_power(device: &EcamDevice, pcr: &Pcr) -> Result<(), Error> {
    require(device)?;
    if device.memory_bar(0x10) != Some(SPI_BASE as u64) || device.read16(4) & 2 == 0 {
        return Err(Error::Decode);
    }
    let r = unsafe { &*(SPI_BASE as *const Registers) };
    if r.sequencing.get() == u32::MAX {
        return Err(Error::Decode);
    }
    if r.sequencing.is_set(HardwareSequence::IN_PROGRESS) {
        return Err(Error::Busy);
    }
    let clocks = pcr.register::<NativeClock::Register>(Port::EspiSpi, 0xc004)?;
    let power = pcr.register::<NativePower::Register>(Port::EspiSpi, 0xc008)?;
    if clocks.get() == u32::MAX || power.get() == u32::MAX {
        return Err(Error::Decode);
    }
    let fields = NativeClock::LOW.val(0x3f) + NativeClock::MID.val(7) + NativeClock::BIT29::SET;
    clocks.modify(fields);
    power.modify(NativePower::GATING.val(0));
    if !clocks.matches_all(fields) || power.read(NativePower::GATING) != 0 {
        return Err(Error::Readback);
    }
    Ok(())
}

// Intel descriptor regions use 4-KiB inclusive base/limit pages. Disabled
// base>limit is absence, but inaccessible/all-ones and out-of-flash words
// must not authorize a boot mapping or LAN gating/reset decision.
fn decode_region(word: u32, flash_bytes: u32) -> Result<Option<core::ops::Range<u32>>, Error> {
    if !(0x100000..=0x08000000).contains(&flash_bytes)
        || !flash_bytes.is_power_of_two()
        || word & 0x80008000 != 0
    {
        return Err(Error::Descriptor);
    }
    let value = tock_registers::LocalRegisterCopy::<u32, FlashRegion::Register>::new(word);
    let first = value.read(FlashRegion::BASE);
    let last = value.read(FlashRegion::LIMIT);
    if first > last {
        return Ok(None);
    }
    let start = first << 12;
    let end = (last + 1) << 12;
    if end > flash_bytes {
        return Err(Error::Descriptor);
    }
    Ok(Some(start..end))
}
/// Read-only descriptor observation through the already owned FastSPI BAR.
/// No flash cycle, NVM/MAC rewrite, all-ones sizing or identity substitution.
pub(crate) fn gbe_region(
    device: &EcamDevice,
    flash_bytes: u32,
) -> Result<Option<core::ops::Range<u32>>, Error> {
    require(device)?;
    if device.memory_bar(0x10) != Some(SPI_BASE as u64) || device.read16(4) & 2 == 0 {
        return Err(Error::Decode);
    }
    let r = unsafe { &*(SPI_BASE as *const Registers) };
    media::require_idle_descriptor(r.sequencing.get())?;
    let region = decode_region(r.regions[3].get(), flash_bytes)?;
    if region.as_ref().is_some_and(|region| region.start == 0) {
        return Err(Error::Descriptor);
    }
    Ok(region)
}

fn require(device: &EcamDevice) -> Result<(), Error> {
    // The owning PCH driver/SMM binding validates actual LPC silicon first;
    // no stage-global ECAM accessor is used by the permanent SMM image.
    if device.address().segment() != 0
        || device.bus() != 0
        || device.dev() != 0x1f
        || device.func() != 5
    {
        return Err(Error::Device);
    }
    // The BIOS SPI function is identified by VID/DID, not a PCI serial-bus
    // class. The Intel PCH register spec reports zero for BCC and SCC.
    let id = device.try_read32(0x00).ok_or(Error::Device)?;
    match id {
        0xa124_8086 | 0xa2a4_8086 => Ok(()), // Sunrise Point-H / 200 Series
        // 332691 BC[9]: once OSFH is set, VID/DID reads return invalid data
        // and no other config register changes. Finalize lockdown and the
        // SMM write-protect handler run after the cold owner hid it.
        _ => {
            let bios = unsafe { device.try_register::<u16, BiosHide::Register>(BIOS_CONTROL) }
                .ok_or(Error::Device)?;
            if bios.get() != u16::MAX && bios.is_set(BiosHide::OS_HIDE) {
                Ok(())
            } else {
                Err(Error::Device)
            }
        }
    }
}

/// # Safety
/// Saved Cold classification must precede this SPI write-enable setup. Own the
/// actual mapped PCH SPI/PCI engines exclusively; no transaction may be active.
pub(crate) unsafe fn initialize_cold(device: &EcamDevice, power: PowerState) -> Result<(), Error> {
    if power.mode() != MemoryBootMode::Cold {
        return Err(Error::Retained);
    }
    if let Err(error) = require(device) {
        fstart_log::error!(
            "SPI 00:1f.5: unexpected PCI ID {}",
            fstart_log::Hex(device.read32(0).into())
        );
        return Err(error);
    }
    let bios = unsafe { device.register::<u32, BiosControl::Register>(BIOS_CONTROL) };
    let control = bios.extract();
    if control.any_matching_bits_set(
        BiosControl::LOCK_ENABLE::SET
            + BiosControl::INTERFACE_LOCK::SET
            + BiosControl::EXT_ENABLE::SET,
    ) {
        fstart_log::error!(
            "SPI: locked or unsupported BIOS control {}",
            fstart_log::Hex(control.get().into())
        );
        return Err(Error::Locked);
    }
    let command = unsafe { device.register::<u16, PCI_COMMAND_BITS::Register>(0x04) };
    command.modify(PCI_COMMAND_BITS::MEMORY_SPACE::CLEAR + PCI_COMMAND_BITS::BUS_MASTER::CLEAR);
    unsafe {
        device.register::<u32, ()>(0x10).set(SPI_BASE as u32);
    }
    command.modify(PCI_COMMAND_BITS::MEMORY_SPACE::SET + PCI_COMMAND_BITS::BUS_MASTER::SET);
    if device.memory_bar(0x10) != Some(SPI_BASE as u64)
        || !command
            .matches_all(PCI_COMMAND_BITS::MEMORY_SPACE::SET + PCI_COMMAND_BITS::BUS_MASTER::SET)
    {
        use fstart_log::Hex;
        fstart_log::error!(
            "SPI decode readback: BAR0={} command={}",
            Hex(device.read32(0x10).into()),
            Hex(command.get().into())
        );
        return Err(Error::Readback);
    }
    bios.modify(
        BiosControl::WRITE_ENABLE::SET
            + BiosControl::PREFETCH::SET
            + BiosControl::CACHE_DISABLE::CLEAR
            + BiosControl::EISS::CLEAR
            + BiosControl::SYNC_STATUS::CLEAR,
    );
    let control = bios.extract();
    if !control.matches_all(
        BiosControl::WRITE_ENABLE::SET
            + BiosControl::PREFETCH::SET
            + BiosControl::CACHE_DISABLE::CLEAR
            + BiosControl::EISS::CLEAR,
    ) {
        fstart_log::error!(
            "SPI BIOS control readback: {}",
            fstart_log::Hex(control.get().into())
        );
        return Err(Error::Readback);
    }
    Ok(())
}

/// SPI function as (bus, device, function) for CF8 access.
const SPI_CF8: (u8, u8, u8) = (0, 0x1f, 5);

/// Called inside the actual permanent TCO SMI handler. Acknowledge only the
/// observed synchronous status. Deny BIOS write enable only when BIOS Lock
/// Enable is set, i.e. under [`BiosWrites::SmmOnly`]; with writes open the
/// OS keeps its write enable.
pub(crate) fn service_write_protection() -> Result<(), Error> {
    // Through CF8 like the ICH handlers: the SMM image keeps no ECAM state.
    // SAFETY: the SMM handler owns configuration space.
    let (bus, device, function) = SPI_CF8;
    let observed = unsafe { fstart_pci::cf8::read32(bus, device, function, BIOS_CONTROL as u8) };
    if observed == u32::MAX {
        return Err(Error::Readback);
    }
    // Writing back acknowledges the SYNC_SS status that raised the SMI.
    unsafe { fstart_pci::cf8::write32(bus, device, function, BIOS_CONTROL as u8, observed) };
    if observed & BiosControl::LOCK_ENABLE::SET.value == 0 {
        return Ok(());
    }
    deny_bios_write()
}

/// Clear BIOS write enable through a byte access that cannot accidentally
/// acknowledge a newly arriving SYNC_STATUS.
fn deny_bios_write() -> Result<(), Error> {
    // SAFETY: the caller (SMM handler or finalize) owns configuration space.
    let (bus, device, function) = SPI_CF8;
    let enable = BiosControl::WRITE_ENABLE::SET.value as u8;
    let low = unsafe { fstart_pci::cf8::read8(bus, device, function, BIOS_CONTROL as u8) };
    if low & enable != 0 {
        unsafe {
            fstart_pci::cf8::write8(bus, device, function, BIOS_CONTROL as u8, low & !enable)
        };
        if unsafe { fstart_pci::cf8::read8(bus, device, function, BIOS_CONTROL as u8) } & enable
            != 0
        {
            return Err(Error::Readback);
        }
    }
    Ok(())
}

/// Native OSFH must precede BILD. This 16-bit update never echoes SYNC_SS
/// and therefore cannot accidentally acknowledge a pending flash-write SMI.
pub(crate) fn hide_from_os(device: &EcamDevice) -> Result<(), Error> {
    require(device)?;
    let bios = unsafe { device.register::<u16, BiosHide::Register>(BIOS_CONTROL) };
    bios.modify(BiosHide::SYNC_STATUS::CLEAR + BiosHide::OS_HIDE::SET);
    if !bios.is_set(BiosHide::OS_HIDE) {
        return Err(Error::Readback);
    }
    Ok(())
}

/// # Safety
/// Actual complete allocated SPI BAR is mapped UC, the engine is quiescent,
/// and the permanent SMM/TCO write-protection handler has actually installed.
/// Errors after writes are irreversible partial lockdown, never retryable.
pub(crate) unsafe fn lockdown(
    device: &EcamDevice,
    disable_write_status: bool,
    writes: BiosWrites,
) -> Result<(), Error> {
    require(device)?;
    let base = device.memory_bar(0x10).ok_or(Error::Decode)?;
    let base = usize::try_from(base).map_err(|_| Error::Decode)?;
    if base & 0xfff != 0 {
        return Err(Error::Decode);
    }
    let registers = unsafe { &*(base as *const Registers) };
    if registers.sequencing.is_set(HardwareSequence::IN_PROGRESS)
        || registers.sequencing.is_set(HardwareSequence::GO)
    {
        return Err(Error::Busy);
    }
    if !registers.sequencing.is_set(HardwareSequence::LOCK) {
        registers
            .prefix
            .write(Prefix::WREN.val(0x06) + Prefix::EWSR.val(0x50));
        registers.opcode_type.write(
            OpcodeType::S0.val(1)
                + OpcodeType::S1.val(3)
                + OpcodeType::S2.val(2)
                + OpcodeType::S3.val(0)
                + OpcodeType::S4.val(3)
                + OpcodeType::S5.val(0)
                + OpcodeType::S6.val(3)
                + OpcodeType::S7.val(2),
        );
        registers.menu_low.write(
            OpcodeMenu::S0.val(0x01)
                + OpcodeMenu::S1.val(0x02)
                + OpcodeMenu::S2.val(0x03)
                + OpcodeMenu::S3.val(0x05),
        );
        registers.menu_high.write(
            OpcodeMenu::S0.val(0x20)
                + OpcodeMenu::S1.val(0x9f)
                + OpcodeMenu::S2.val(0xd8)
                + OpcodeMenu::S3.val(0x0b),
        );
    }
    if registers.prefix.get() != 0x5006
        || registers.opcode_type.get() != 0xb32d
        || registers.menu_low.get() != 0x0503_0201
        || registers.menu_high.get() != 0x0bd8_9f20
    {
        return Err(Error::Readback);
    }
    registers
        .discrete_lock
        .modify(DiscreteLock::RANGES.val(0x1f));
    if registers.sequencing.is_set(HardwareSequence::IN_PROGRESS)
        || registers.sequencing.is_set(HardwareSequence::GO)
    {
        return Err(Error::Busy);
    }
    registers
        .sequencing
        .write(HardwareSequence::STATUS_W1C.val(0x1ff));
    // Native 16-bit literal write: no RMW of W1C status, no access to upper
    // cycle controls, and PRR3/4 plus optional WRSDIS lock in the SAME cycle.
    let flags = HardwareSequence::LOCK::SET.value
        | HardwareSequence::RANGE34_LOCK::SET.value
        | if disable_write_status {
            HardwareSequence::WRITE_STATUS_DISABLE::SET.value
        } else {
            0
        };
    unsafe { MmioRegister::<u16>::new(base + 4) }.set(flags as u16);
    registers
        .vendor_capability
        .modify(VendorCapability::LOCK::SET);
    if !registers
        .sequencing
        .matches_all(HardwareSequence::LOCK::SET + HardwareSequence::RANGE34_LOCK::SET)
        || disable_write_status
            && !registers
                .sequencing
                .is_set(HardwareSequence::WRITE_STATUS_DISABLE)
        || registers.discrete_lock.read(DiscreteLock::RANGES) != 0x1f
        || !registers.vendor_capability.is_set(VendorCapability::LOCK)
    {
        return Err(Error::Readback);
    }
    let bios = unsafe { device.register::<u32, BiosControl::Register>(BIOS_CONTROL) };
    // Native BILD -> EISS -> deny WPD -> LE order. Never acknowledge a newly
    // arriving synchronous SMI through a lock-publication RMW.
    bios.modify(BiosControl::INTERFACE_LOCK::SET + BiosControl::SYNC_STATUS::CLEAR);
    if !bios.is_set(BiosControl::INTERFACE_LOCK) {
        return Err(Error::Readback);
    }
    if writes == BiosWrites::Open {
        // Leave the OS a writable BIOS region: no EISS, no LE.
        bios.modify(
            BiosControl::WRITE_ENABLE::SET
                + BiosControl::EISS::CLEAR
                + BiosControl::SYNC_STATUS::CLEAR,
        );
        if !bios.matches_all(BiosControl::WRITE_ENABLE::SET + BiosControl::EISS::CLEAR) {
            return Err(Error::Readback);
        }
        return Ok(());
    }
    bios.modify(BiosControl::EISS::SET + BiosControl::SYNC_STATUS::CLEAR);
    if !bios.is_set(BiosControl::EISS) {
        return Err(Error::Readback);
    }
    deny_bios_write()?;
    bios.modify(BiosControl::LOCK_ENABLE::SET + BiosControl::SYNC_STATUS::CLEAR);
    if !bios.is_set(BiosControl::LOCK_ENABLE) {
        return Err(Error::Readback);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_regions_distinguish_disabled_from_invalid_geometry() {
        assert_eq!(
            decode_region(0x00050003, 16 << 20),
            Ok(Some(0x3000..0x6000))
        );
        assert_eq!(decode_region(0x7fff, 16 << 20), Ok(None));
        assert_eq!(decode_region(u32::MAX, 16 << 20), Err(Error::Descriptor));
        assert_eq!(decode_region(0x10000003, 16 << 20), Err(Error::Descriptor));
        // Page zero is legitimate ONLY for the descriptor. The GbE owner
        // rejects it separately; full layout checks reject cross-region aliases.
        assert_eq!(decode_region(0, 16 << 20), Ok(Some(0..0x1000)));
    }
}
