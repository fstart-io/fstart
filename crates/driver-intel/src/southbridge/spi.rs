//! Allocation-free rflasher adapter for firmware ICH7/ICH8/ICH9, 5/8 Series,
//! Bay Trail, and Sunrise Point SPI access.
//!
//! The controller is not a general firmware flasher: all accesses are bounded
//! to a caller-supplied, linked mutable extent. Construct/use it only from RAM;
//! erasing the flash while executing XIP controller code is not safe.

use crate::southbridge::lpc::{self, BIOS_CONTROL, ROOT_COMPLEX_BASE};
use fstart_core::services::{FlashRegion, ServiceError};
use fstart_pci::ecam::EcamDevice;
use rflasher_core::spi::{SpiCommand, opcodes};
use rflasher_internal::chipset::IchChipset;
use rflasher_internal::controller::Controller;
use rflasher_internal::{
    DetectedChipset, HostAccess, IchSpiController, InternalError, MmioAccess, PciAddress,
    PciConfigAccess, SpiMode,
};
use tock_registers::interfaces::{ReadWriteable, Readable};

/// Where SPIBAR lives: RCBA+0x3020 on ICH7/ICH8, RCBA+0x3800 on ICH9 and the
/// 5/8 Series PCH, LPC SBASE (0x54) on Silvermont/Bay Trail, and BAR0 of the
/// D31:F5 SPI function on Sunrise Point.
#[derive(Clone, Copy)]
enum FirmwareHost {
    Rcba { spi_offset: u64 },
    SilvermontSbase,
    Pci(EcamDevice),
}

impl FirmwareHost {
    fn device(address: PciAddress) -> EcamDevice {
        // Translate the external library's address type only. All mapping,
        // segment, boundary and width checks belong to the canonical accessor.
        EcamDevice::new_address(fstart_pci::PciAddress::new(
            address.segment(),
            address.bus(),
            address.device(),
            address.function(),
        ))
    }
}

impl PciConfigAccess for FirmwareHost {
    type Error = InternalError;
    fn read8(&self, a: PciAddress, o: u16) -> Result<u8, InternalError> {
        Self::device(a)
            .try_read8(o)
            .ok_or(InternalError::Io("invalid firmware ECAM access"))
    }
    fn read16(&self, a: PciAddress, o: u16) -> Result<u16, InternalError> {
        Self::device(a)
            .try_read16(o)
            .ok_or(InternalError::Io("invalid firmware ECAM access"))
    }
    fn read32(&self, a: PciAddress, o: u16) -> Result<u32, InternalError> {
        Self::device(a)
            .try_read32(o)
            .ok_or(InternalError::Io("invalid firmware ECAM access"))
    }
    fn write8(&self, a: PciAddress, o: u16, v: u8) -> Result<(), InternalError> {
        Self::device(a)
            .try_write8(o, v)
            .ok_or(InternalError::Io("invalid firmware ECAM access"))
    }
    fn write16(&self, a: PciAddress, o: u16, v: u16) -> Result<(), InternalError> {
        Self::device(a)
            .try_write16(o, v)
            .ok_or(InternalError::Io("invalid firmware ECAM access"))
    }
    fn write32(&self, a: PciAddress, o: u16, v: u32) -> Result<(), InternalError> {
        Self::device(a)
            .try_write32(o, v)
            .ok_or(InternalError::Io("invalid firmware ECAM access"))
    }
}

#[derive(Clone, Copy)]
struct SpiRegisters(fstart_core::mmio::MmioWindow);

// rflasher requires this external trait; keep it a forwarding adapter, not
// another implementation of MMIO transactions or mapped-window validation.
impl MmioAccess for SpiRegisters {
    fn read8(&self, o: usize) -> u8 {
        self.0.read8(o.try_into().expect("MMIO offset overflow"))
    }
    fn read16(&self, o: usize) -> u16 {
        self.0.read16(o.try_into().expect("MMIO offset overflow"))
    }
    fn read32(&self, o: usize) -> u32 {
        self.0.read32(o.try_into().expect("MMIO offset overflow"))
    }
    fn write8(&self, o: usize, v: u8) {
        self.0
            .write8(o.try_into().expect("MMIO offset overflow"), v)
    }
    fn write16(&self, o: usize, v: u16) {
        self.0
            .write16(o.try_into().expect("MMIO offset overflow"), v)
    }
    fn write32(&self, o: usize, v: u32) {
        self.0
            .write32(o.try_into().expect("MMIO offset overflow"), v)
    }
}
const SPI_WINDOW_SIZE: u32 = 0x200;

/// Decode Silvermont LPC SBASE (0x54): bits 31:9 are the 512-byte-aligned
/// MMIO base, bit 1 is the decode enable.
fn silvermont_sbase_addr(raw: u32) -> Option<u64> {
    (raw & crate::baytrail::lpc::BASE::ENABLE::SET.value != 0)
        .then_some(u64::from(raw & 0xffff_fe00))
}

impl HostAccess for FirmwareHost {
    type MmioRegion = SpiRegisters;
    unsafe fn map_mmio(&self, address: u64, size: usize) -> Result<SpiRegisters, InternalError> {
        let expected = match self {
            Self::Rcba { spi_offset } => {
                // SAFETY: inherited from map_mmio's enabled ECAM/RCBA contract.
                let rcba = unsafe { lpc::flash_config() }.rcba.extract();
                rcba.is_set(ROOT_COMPLEX_BASE::ENABLE)
                    .then(|| (u64::from(rcba.read(ROOT_COMPLEX_BASE::BASE)) << 14) + spi_offset)
            }
            Self::SilvermontSbase => {
                let raw = crate::baytrail::lpc::device().read32(crate::baytrail::lpc::SBASE);
                silvermont_sbase_addr(raw)
            }
            Self::Pci(spi) => spi.memory_bar(0x10),
        };
        if expected != Some(address) || size != SPI_WINDOW_SIZE as usize {
            return Err(InternalError::Io("invalid ICH SPI window"));
        }
        // SAFETY: `address` is the verified southbridge SPIBAR window.
        Ok(SpiRegisters(unsafe {
            fstart_core::mmio::MmioWindow::new(address as usize, SPI_WINDOW_SIZE)
        }))
    }
    fn delay_us(&self, us: u32) {
        fstart_arch::x86::udelay(us);
    }
}

/// Physical SPI extent, not an offset within the top-aligned BIOS mapping.
#[derive(Debug, Clone, Copy)]
pub struct FlashExtent {
    pub offset: u32,
    pub size: u32,
    pub chip_size: u32,
}
impl FlashExtent {
    pub fn address(self, relative: u32, len: usize) -> Result<u32, ServiceError> {
        let len = u32::try_from(len).map_err(|_| ServiceError::InvalidParam)?;
        let end = relative
            .checked_add(len)
            .ok_or(ServiceError::InvalidParam)?;
        let absolute = self
            .offset
            .checked_add(relative)
            .ok_or(ServiceError::InvalidParam)?;
        if len == 0
            || end > self.size
            || absolute.checked_add(len).is_none_or(|e| e > self.chip_size)
        {
            return Err(ServiceError::InvalidParam);
        }
        Ok(absolute)
    }
}

/// Bounded controller for mutable firmware data. Write methods restore the
/// entry BIOSWE value and refuse BLE/SMM-only protection present at call time.
/// They do not establish or lock the platform's global flash policy: Ibex Peak
/// explicitly opens that policy for development cache/log and payload writes
/// through `LpcFlashConfig::prepare_firmware_writes` before this driver runs.
/// Software sequencing requires positively identified page-program NOR
/// geometry; unsupported chips remain read-only rather than guessing opcodes.
/// Sunrise Point only has hardware sequencing, which programs and erases with
/// the descriptor-selected opcodes and the controller's own erase block.
pub struct CacheSpi {
    controller: IchSpiController<FirmwareHost>,
    /// The function whose 0xdc holds BIOS_CNTL: LPC, or the PCH100 SPI.
    bios: EcamDevice,
    extent: FlashExtent,
    /// Block protection of the identified part; `None` keeps it read-only.
    writable: Option<Protection>,
    /// Mapped SBASE for Silvermont chipsets whose BIOS control lives at SPIBAR+0xfc.
    silvermont_sbase: Option<usize>,
}
impl CacheSpi {
    /// # Safety
    /// ECAM and RCBA/SBASE must be mapped as device memory. `extent` must be a
    /// linked reservation disjoint from all code, roots/directories and vendor
    /// data. The caller and all reachable code/data must execute from RAM, not
    /// flash.
    pub unsafe fn new(extent: FlashExtent) -> Result<Self, ServiceError> {
        Self::check_extent(&extent)?;
        // SAFETY: new's caller guarantees live ECAM and ICH register mappings.
        let lpc = unsafe { lpc::flash_config() };
        let entry = rflasher_internal::find_chipset(
            lpc.vendor_id.get(),
            lpc.device_id.get(),
            Some(lpc.revision_id.get()),
        )
        .ok_or(ServiceError::NotSupported)?;
        let host = match entry.chipset {
            IchChipset::Ich7 | IchChipset::Ich8 => FirmwareHost::Rcba { spi_offset: 0x3020 },
            // The 5 and 8 Series PCHs keep ICH9's SPI block and offset.
            IchChipset::Ich9
            | IchChipset::Series5IbexPeak
            | IchChipset::Series8LynxPoint
            | IchChipset::Series8LynxPointLp => FirmwareHost::Rcba { spi_offset: 0x3800 },
            IchChipset::BayTrail => FirmwareHost::SilvermontSbase,
            _ => return Err(ServiceError::NotSupported),
        };
        let silvermont_sbase = match host {
            FirmwareHost::SilvermontSbase => {
                let raw = crate::baytrail::lpc::device().read32(crate::baytrail::lpc::SBASE);
                let base = silvermont_sbase_addr(raw).ok_or(ServiceError::HardwareError)?;
                Some(base as usize)
            }
            FirmwareHost::Rcba { .. } | FirmwareHost::Pci(_) => None,
        };
        let chipset = DetectedChipset {
            enable: entry,
            domain: 0,
            bus: 0,
            device: lpc::LPC_DEV,
            function: lpc::LPC_FUNC,
            revision_id: lpc.revision_id.get(),
        };
        let mut controller =
            IchSpiController::new_with_host(host, &chipset, SpiMode::SoftwareSequencing)
                .map_err(|_| ServiceError::HardwareError)?;
        if let Some((base, limit)) = controller.get_bios_region() {
            if extent.offset < base || extent.offset + extent.size - 1 > limit {
                return Err(ServiceError::InvalidParam);
            }
        } else if entry.chipset != IchChipset::Ich7 {
            // Never infer an IFD-relative writable extent from a missing descriptor.
            return Err(ServiceError::HardwareError);
        }
        let mut id = [0u8; 3];
        let identified = controller
            .execute(&mut SpiCommand::read_reg(opcodes::RDID, &mut id))
            .is_ok();
        let bios_cntl = match silvermont_sbase {
            Some(sbase) => {
                // SAFETY: SBASE was verified enabled and mapped above.
                unsafe { &*(sbase as *const crate::baytrail::lpc::SpiRegs) }
                    .bcr
                    .get()
            }
            None => u32::from(lpc.bios_control.get()),
        };
        // Construction is outside the console's write callback. Keep status
        // diagnostics here, never in program/erase/unlock (logger re-entry).
        fstart_log::info!(
            "spi-cache: JEDEC {:#x}:{:#x}:{:#x}, BIOS_CNTL={:#x}",
            id[0],
            id[1],
            id[2],
            bios_cntl
        );
        let writable = identified
            .then(|| supported_geometry(id, extent.chip_size))
            .flatten()
            // Require the known 4-KiB erase protocol: `erase` names SE_20
            // and its sector size for the geometry this whitelist verified.
            .filter(|protection| {
                [opcodes::SE_20, opcodes::PP, opcodes::RDSR]
                    .into_iter()
                    .chain(protection.opcodes().iter().copied())
                    .all(|opcode| controller.probe_opcode(opcode))
            });
        Ok(Self {
            controller,
            bios: lpc::device(),
            extent,
            writable,
            silvermont_sbase,
        })
    }

    /// The SPI controller of a 100 Series or later PCH: its own PCI function
    /// (D31:F5), hardware sequencing only. The controller issues the
    /// descriptor-selected opcodes itself and only takes addresses.
    ///
    /// # Safety
    /// As [`Self::new`].
    pub unsafe fn new_pch(extent: FlashExtent) -> Result<Self, ServiceError> {
        Self::check_extent(&extent)?;
        // SAFETY: new_pch's caller guarantees live ECAM.
        let lpc = unsafe { lpc::flash_config() };
        let entry = rflasher_internal::find_chipset(
            lpc.vendor_id.get(),
            lpc.device_id.get(),
            Some(lpc.revision_id.get()),
        )
        .filter(|entry| entry.chipset == IchChipset::Series100SunrisePoint)
        .ok_or(ServiceError::NotSupported)?;
        let spi = EcamDevice::new(0, lpc::LPC_DEV, 5);
        let chipset = DetectedChipset {
            enable: entry,
            domain: 0,
            bus: 0,
            device: lpc::LPC_DEV,
            function: lpc::LPC_FUNC,
            revision_id: lpc.revision_id.get(),
        };
        let controller = IchSpiController::new_with_host(
            FirmwareHost::Pci(spi),
            &chipset,
            SpiMode::HardwareSequencing,
        )
        .map_err(|_| ServiceError::HardwareError)?;
        let (base, limit) = controller
            .get_bios_region()
            .ok_or(ServiceError::HardwareError)?;
        if extent.offset < base || extent.offset + extent.size - 1 > limit {
            return Err(ServiceError::InvalidParam);
        }
        let block = controller
            .hwseq_erase_geometry(extent.chip_size)
            .map_err(|_| ServiceError::NotSupported)?;
        // SAFETY: BIOS_CNTL is a byte at 0xdc of the live SPI function.
        let control = unsafe { spi.register::<u8, BIOS_CONTROL::Register>(0xdc) };
        fstart_log::info!(
            "spi-cache: hwseq, erase block {:#x}, BIOS_CNTL={:#x}",
            block,
            control.get()
        );
        // `erase` works in whole 64-KiB units; they must be whole blocks.
        if block == 0 || 0x10000 % block != 0 {
            return Err(ServiceError::NotSupported);
        }
        Ok(Self {
            controller,
            bios: spi,
            extent,
            // The controller owns the opcodes; nothing to unlock.
            writable: Some(Protection::None),
            silvermont_sbase: None,
        })
    }

    fn check_extent(extent: &FlashExtent) -> Result<(), ServiceError> {
        extent.address(0, extent.size as usize)?;
        if extent.chip_size > 0x1000000
            || !extent.offset.is_multiple_of(0x10000)
            || !extent.size.is_multiple_of(0x10000)
        {
            return Err(ServiceError::InvalidParam);
        }
        Ok(())
    }

    /// Whether the chip was identified as a part this controller programs.
    pub fn writable(&self) -> bool {
        self.writable.is_some()
    }

    fn read_status(&mut self) -> Result<u8, ServiceError> {
        let mut status = [0];
        self.controller
            .execute(&mut SpiCommand::read_reg(opcodes::RDSR, &mut status))
            .map_err(|_| ServiceError::HardwareError)?;
        Ok(status[0])
    }

    /// rflasher `protection::write_status`: SST needs EWSR; Macronix uses
    /// WREN (the controller's atomic prefix), then WRSR and WIP polling.
    fn write_status(&mut self, value: u8) -> Result<(), ServiceError> {
        if self.writable == Some(Protection::SstStatus) {
            self.controller
                .execute(&mut SpiCommand::simple(opcodes::EWSR))
                .map_err(|_| ServiceError::HardwareError)?;
        }
        self.controller
            .execute(&mut SpiCommand::write_reg(opcodes::WRSR, &[value & !3]))
            .map_err(|_| ServiceError::HardwareError)?;
        for _ in 0..1000 {
            if self.read_status()? & 1 == 0 {
                return Ok(());
            }
            fstart_arch::x86::udelay(100);
        }
        Err(ServiceError::Timeout)
    }

    /// Run `operation` with the block-protect bits cleared, then restore them
    /// (rflasher `Unlock::Status`). Returns early, untouched, when clear.
    fn unprotected(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<(), ServiceError>,
    ) -> Result<(), ServiceError> {
        if !matches!(
            self.writable,
            Some(Protection::SstStatus | Protection::MacronixStatus)
        ) {
            return operation(self);
        }
        let status = self.read_status()?;
        if status & STATUS_BP == 0 {
            return operation(self);
        }
        let result = self.write_status(status & !STATUS_BP).and_then(|()| {
            if self.read_status()? & STATUS_BP != 0 {
                // BPL with WP# asserted ignores WRSR.
                return Err(ServiceError::HardwareError);
            }
            operation(self)
        });
        let restored = self.write_status(status).and_then(|()| {
            if self.read_status()? & STATUS_BP != status & STATUS_BP {
                return Err(ServiceError::HardwareError);
            }
            Ok(())
        });
        result.and(restored)
    }

    // This is also the flash console's backend. Never log from the write
    // path: the caller may already hold fstart-log's console lock.
    fn with_write(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<(), ServiceError>,
    ) -> Result<(), ServiceError> {
        if self.writable.is_none() {
            return Err(ServiceError::NotSupported);
        }
        if let Some(sbase) = self.silvermont_sbase {
            use crate::baytrail::lpc::{BCR, SpiRegs};
            // SAFETY: construction established the live SBASE mapping; operations run from RAM.
            let bcr = &unsafe { &*(sbase as *const SpiRegs) }.bcr;
            let before = bcr.extract();
            if before.is_set(BCR::LE) || before.is_set(BCR::EISS) {
                return Err(ServiceError::HardwareError);
            }
            let result = self
                .controller
                .enable_bios_write()
                .map_err(|_| ServiceError::HardwareError)
                .and_then(|()| self.unprotected(operation));
            // Restore only WPD; preserve unrelated and newly latched policy bits.
            bcr.modify(BCR::WPD.val(before.read(BCR::WPD)));
            if bcr.read(BCR::WPD) != before.read(BCR::WPD) {
                return Err(ServiceError::HardwareError);
            }
            return result;
        }
        // SAFETY: construction established the live function; code runs from RAM.
        let control = unsafe { self.bios.register::<u8, BIOS_CONTROL::Register>(0xdc) };
        let before = control.extract();
        if before.is_set(BIOS_CONTROL::LOCK_ENABLE)
            || before.is_set(BIOS_CONTROL::SMM_WRITE_PROTECT)
        {
            return Err(ServiceError::HardwareError);
        }
        let result = self
            .controller
            .enable_bios_write()
            .map_err(|_| ServiceError::HardwareError)
            .and_then(|()| self.unprotected(operation));
        // Restore only BIOSWE; preserve unrelated and newly latched policy bits.
        control.modify(BIOS_CONTROL::WRITE_ENABLE.val(before.read(BIOS_CONTROL::WRITE_ENABLE)));
        if control.read(BIOS_CONTROL::WRITE_ENABLE) != before.read(BIOS_CONTROL::WRITE_ENABLE) {
            return Err(ServiceError::HardwareError);
        }
        result
    }
}

impl FlashRegion for CacheSpi {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError> {
        let address = self.extent.address(offset, bytes.len())?;
        self.controller
            .controller_read(address, bytes, self.extent.chip_size as usize)
            .map_err(|_| ServiceError::HardwareError)
    }

    fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError> {
        let address = self.extent.address(offset, bytes.len())?;
        self.with_write(|this| {
            let mut done = 0;
            while done < bytes.len() {
                let current = address + done as u32;
                let count = (bytes.len() - done)
                    .min(64)
                    .min(256 - (current as usize & 255));
                this.controller
                    .controller_write(current, &bytes[done..done + count])
                    .map_err(|_| ServiceError::HardwareError)?;
                done += count;
            }
            Ok(())
        })
    }

    /// Erase complete 64-KiB units only, as the verified chip's 4-KiB sectors:
    /// rflasher erases and polls, with the geometry `new` verified. Neither
    /// can cross the extent.
    fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
        const SECTOR: u32 = 0x1000;
        let address = self.extent.address(offset, length as usize)?;
        if address % 0x10000 != 0 || !length.is_multiple_of(0x10000) {
            return Err(ServiceError::InvalidParam);
        }
        self.with_write(|this| {
            if this.controller.mode() == SpiMode::HardwareSequencing {
                return this
                    .controller
                    .controller_erase(address, length)
                    .map_err(|_| ServiceError::HardwareError);
            }
            this.controller
                .swseq_erase_blocks(opcodes::SE_20, SECTOR, address, length)
                .map_err(|_| ServiceError::HardwareError)
        })
    }
}

/// The SPI flash console's region (`fstart-driver-flash-console`). The
/// console warms an XIP caller's image before every program cycle; see
/// [`CacheSpi::new`] for the rest of the contract.
impl fstart_driver_flash_console::Medium for CacheSpi {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError> {
        FlashRegion::read(self, offset, bytes)
    }
    fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError> {
        FlashRegion::program(self, offset, bytes)
    }
}

/// Block-protection procedure of a whitelisted part (rflasher `Unlock`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protection {
    /// Writes need no unlock.
    None,
    /// SST25: BP0-3 in SR1, rewritten through EWSR + WRSR.
    SstStatus,
    /// MX25L: BP0-3 in SR1, rewritten through WREN + WRSR.
    MacronixStatus,
}

/// SST25/MX25L SR1 BP0-BP3 (rflasher `Unlock::Status`).
const STATUS_BP: u8 = 0x3c;

impl Protection {
    /// Opcodes the procedure needs beyond program/erase/RDSR.
    fn opcodes(self) -> &'static [u8] {
        match self {
            Self::None => &[],
            Self::SstStatus => &[opcodes::EWSR, opcodes::WRSR],
            Self::MacronixStatus => &[opcodes::WRSR],
        }
    }
}

// W25Q16/32/64/128, W25X16/32/64, MX25L16/32/64/128, AT25DF321(A) and
// SST25VF064C use 256-byte page programming and 4-KiB/64-KiB erases.
// Other SST25 parts program with AAI or single bytes and are excluded.
fn supported_geometry(id: [u8; 3], chip_size: u32) -> Option<Protection> {
    let supported = match id {
        [0xef, 0x30, capacity @ 0x15..=0x17] => chip_size == 1u32 << capacity,
        [0xef, 0x40, capacity] => {
            (0x15..=0x18).contains(&capacity) && chip_size == 1u32 << capacity
        }
        [0xc2, 0x20, capacity @ 0x15..=0x18] => {
            return (chip_size == 1u32 << capacity).then_some(Protection::MacronixStatus);
        }
        [0x1f, 0x47, 0x00 | 0x01] => chip_size == 0x400000,
        // Ships in the ThinkPad X200; powers up block-protected.
        [0xbf, 0x25, 0x4b] => return (chip_size == 0x800000).then_some(Protection::SstStatus),
        _ => false,
    };
    supported.then_some(Protection::None)
}

#[cfg(test)]
mod tests {
    #[test]
    fn geometry_requires_known_program_protocol_and_exact_capacity() {
        use super::{Protection, supported_geometry};
        let none = Some(Protection::None);
        assert_eq!(supported_geometry([0xef, 0x40, 0x18], 0x1000000), none);
        for (capacity, size) in [(0x16, 0x400000), (0x17, 0x800000)] {
            assert_eq!(
                supported_geometry([0xc2, 0x20, capacity], size),
                Some(Protection::MacronixStatus)
            );
        }
        assert_eq!(supported_geometry([0xef, 0x30, 0x17], 0x800000), none);
        assert_eq!(supported_geometry([0x1f, 0x47, 0x00], 0x400000), none);
        assert_eq!(supported_geometry([0x1f, 0x47, 0x01], 0x400000), none);
        assert_eq!(
            supported_geometry([0xbf, 0x25, 0x4b], 0x800000),
            Some(Protection::SstStatus)
        );
        // Wrong capacity, unknown parts, and AAI-only SST25VF032B.
        for (id, size) in [
            ([0xef, 0x30, 0x18], 0x1000000),
            ([0x1f, 0x47, 0x00], 0x800000),
            ([0x1f, 0x47, 0x02], 0x400000),
            ([0xef, 0x40, 0x18], 0x400000),
            ([0xbf, 0x25, 0x4b], 0x400000),
            ([0xbf, 0x25, 0x4a], 0x400000),
            ([0xef, 0x40, 0xff], 0),
        ] {
            assert_eq!(supported_geometry(id, size), None);
        }
    }
    use super::*;
    #[test]
    fn physical_bounds_reject_wrap_and_ro_escape() {
        let extent = FlashExtent {
            offset: 0x280000,
            size: 0x20000,
            chip_size: 0x400000,
        };
        assert_eq!(extent.address(0x1ffff, 1).unwrap(), 0x29ffff);
        assert!(extent.address(0x20000, 1).is_err());
        assert!(extent.address(u32::MAX, 2).is_err());
        assert!(extent.address(0, usize::MAX).is_err());
        assert!(
            FlashExtent {
                offset: u32::MAX,
                ..extent
            }
            .address(0, 2)
            .is_err()
        );
    }

    #[test]
    fn silvermont_sbase_decodes_enabled_512b_aligned_address() {
        // Bit 1 is decode enable; low 9 bits are status/enable flags.
        assert_eq!(silvermont_sbase_addr(0xfed0_1000), None);
        assert_eq!(silvermont_sbase_addr(0xfed0_1001), None);
        assert_eq!(silvermont_sbase_addr(0xfed0_1002), Some(0xfed0_1000));
        assert_eq!(silvermont_sbase_addr(0xfed0_11ff), Some(0xfed0_1000));
        // W25Q64FV (MinnowBoard MAX 8 MiB flash) is whitelisted.
        assert_eq!(
            supported_geometry([0xef, 0x40, 0x17], 0x80_0000),
            Some(Protection::None)
        );
    }
}
