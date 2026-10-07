//! Allocation-free rflasher adapter for firmware ICH7/ICH8 SPI access.
//!
//! The controller is not a general firmware flasher: all accesses are bounded
//! to a caller-supplied, linked mutable extent. Construct/use it only from RAM;
//! erasing the flash while executing XIP controller code is not safe.

use crate::MmioBar;
use crate::southbridge::lpc::{self, BIOS_CONTROL, ROOT_COMPLEX_BASE};
use fstart_core::services::ServiceError;
use fstart_pci::ecam::EcamDevice;
use rflasher_core::spi::SpiCommand;
use rflasher_internal::chipset::IchChipset;
use rflasher_internal::controller::Controller;
use rflasher_internal::{
    DetectedChipset, HostAccess, IchSpiController, InternalError, MmioAccess, PciAddress,
    PciConfigAccess, SpiMode,
};
use tock_registers::interfaces::{ReadWriteable, Readable};

#[derive(Clone, Copy)]
struct FirmwareHost;

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
struct SpiRegisters(usize);
impl MmioBar for SpiRegisters {
    fn base(self) -> usize {
        self.0
    }
    fn mapped_size(self) -> Option<u32> {
        Some(SPI_WINDOW_SIZE)
    }
}
// rflasher requires this external trait; keep it a forwarding adapter, not
// another implementation of MMIO transactions or mapped-window validation.
impl MmioAccess for SpiRegisters {
    fn read8(&self, o: usize) -> u8 {
        MmioBar::read8(*self, o.try_into().expect("MMIO offset overflow"))
    }
    fn read16(&self, o: usize) -> u16 {
        MmioBar::read16(*self, o.try_into().expect("MMIO offset overflow"))
    }
    fn read32(&self, o: usize) -> u32 {
        MmioBar::read32(*self, o.try_into().expect("MMIO offset overflow"))
    }
    fn write8(&self, o: usize, v: u8) {
        MmioBar::write8(*self, o.try_into().expect("MMIO offset overflow"), v)
    }
    fn write16(&self, o: usize, v: u16) {
        MmioBar::write16(*self, o.try_into().expect("MMIO offset overflow"), v)
    }
    fn write32(&self, o: usize, v: u32) {
        MmioBar::write32(*self, o.try_into().expect("MMIO offset overflow"), v)
    }
}
const SPI_OFFSET: u64 = 0x3020;
const SPI_WINDOW_SIZE: u32 = 0x200;

impl HostAccess for FirmwareHost {
    type MmioRegion = SpiRegisters;
    unsafe fn map_mmio(&self, address: u64, size: usize) -> Result<SpiRegisters, InternalError> {
        // SAFETY: inherited from map_mmio's enabled ECAM/RCBA contract.
        let rcba = unsafe { lpc::flash_config() }.rcba.extract();
        let expected = (u64::from(rcba.read(ROOT_COMPLEX_BASE::BASE)) << 14) + SPI_OFFSET;
        if !rcba.is_set(ROOT_COMPLEX_BASE::ENABLE)
            || address != expected
            || size != SPI_WINDOW_SIZE as usize
        {
            return Err(InternalError::Io("invalid ICH7/ICH8 SPI window"));
        }
        Ok(SpiRegisters(address as usize))
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

/// Bounded controller for mutable firmware data. Write methods restore BIOSWE
/// on every exit and refuse locked/SMM-only configurations rather than bypassing
/// protection. Writes require a positively identified, known page-program NOR
/// geometry; unsupported chips remain read-only rather than guessing opcodes.
pub struct CacheSpi {
    controller: IchSpiController<FirmwareHost>,
    extent: FlashExtent,
    write_geometry_verified: bool,
}
impl CacheSpi {
    /// # Safety
    /// ECAM/RCBA must be mapped as device memory. `extent` must be a linked
    /// reservation disjoint from all code, roots/directories and vendor data.
    /// The caller and all reachable code/data must execute from RAM, not flash.
    pub unsafe fn new(extent: FlashExtent) -> Result<Self, ServiceError> {
        extent.address(0, extent.size as usize)?;
        if extent.chip_size > 0x1000000
            || extent.offset % 0x10000 != 0
            || extent.size % 0x10000 != 0
        {
            return Err(ServiceError::InvalidParam);
        }
        // SAFETY: new's caller guarantees live ECAM and ICH register mappings.
        let lpc = unsafe { lpc::flash_config() };
        let entry = rflasher_internal::find_chipset(
            lpc.vendor_id.get(),
            lpc.device_id.get(),
            Some(lpc.revision_id.get()),
        )
        .ok_or(ServiceError::NotSupported)?;
        if !matches!(entry.chipset, IchChipset::Ich7 | IchChipset::Ich8) {
            return Err(ServiceError::NotSupported);
        }
        let chipset = DetectedChipset {
            enable: entry,
            domain: 0,
            bus: 0,
            device: lpc::LPC_DEV,
            function: lpc::LPC_FUNC,
            revision_id: lpc.revision_id.get(),
        };
        let mut controller =
            IchSpiController::new_with_host(FirmwareHost, &chipset, SpiMode::SoftwareSequencing)
                .map_err(|_| ServiceError::HardwareError)?;
        if let Some((base, limit)) = controller.get_bios_region() {
            if extent.offset < base || extent.offset + extent.size - 1 > limit {
                return Err(ServiceError::InvalidParam);
            }
        } else if entry.chipset == IchChipset::Ich8 {
            // Never infer an IFD-relative writable extent from a missing descriptor.
            return Err(ServiceError::HardwareError);
        }
        let mut id = [0u8; 3];
        let identified = controller
            .execute(&mut SpiCommand::read_reg(0x9f, &mut id))
            .is_ok();
        fstart_log::info!("spi-cache: JEDEC {:#x}:{:#x}:{:#x}", id[0], id[1], id[2]);
        let write_geometry_verified = identified
            && supported_geometry(id, extent.chip_size)
            // Require the known 4-KiB erase protocol so rflasher cannot select
            // a different menu opcode whose geometry this whitelist lacks.
            && controller.probe_opcode(0x20)
            && controller.probe_opcode(0x02)
            && controller.probe_opcode(0x05);
        Ok(Self {
            controller,
            extent,
            write_geometry_verified,
        })
    }

    pub fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError> {
        let address = self.extent.address(offset, bytes.len())?;
        self.controller
            .controller_read(address, bytes, self.extent.chip_size as usize)
            .map_err(|_| ServiceError::HardwareError)
    }

    fn with_write(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<(), ServiceError>,
    ) -> Result<(), ServiceError> {
        if !self.write_geometry_verified {
            return Err(ServiceError::NotSupported);
        }
        // SAFETY: construction established the live mapping; operations run from RAM.
        let control = &unsafe { lpc::flash_config() }.bios_control;
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
            .and_then(|()| operation(self));
        // Restore only BIOSWE; preserve unrelated and newly latched policy bits.
        control.modify(BIOS_CONTROL::WRITE_ENABLE.val(before.read(BIOS_CONTROL::WRITE_ENABLE)));
        if control.read(BIOS_CONTROL::WRITE_ENABLE) != before.read(BIOS_CONTROL::WRITE_ENABLE) {
            return Err(ServiceError::HardwareError);
        }
        result
    }

    pub fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError> {
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

    /// Erase complete 64-KiB units only. The controller may split them into
    /// the verified chip's supported 4-KiB sectors; neither can cross the extent.
    pub fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
        let address = self.extent.address(offset, length as usize)?;
        if address % 0x10000 != 0 || length % 0x10000 != 0 {
            return Err(ServiceError::InvalidParam);
        }
        self.with_write(|this| {
            this.controller
                .controller_erase(address, length)
                .map_err(|_| ServiceError::HardwareError)
        })
    }
}

// W25Q16/32/64/128 and MX25L16/32/64/128 use 256-byte page programming
// and both 4-KiB/64-KiB erases. Do not include SST AAI/byte-program parts.
fn supported_geometry(id: [u8; 3], chip_size: u32) -> bool {
    matches!((id[0], id[1]), (0xef, 0x40) | (0xc2, 0x20))
        && (0x15..=0x18).contains(&id[2])
        && chip_size == 1u32 << id[2]
}

#[cfg(test)]
mod tests {
    #[test]
    fn geometry_requires_known_program_protocol_and_exact_capacity() {
        assert!(super::supported_geometry([0xef, 0x40, 0x18], 0x1000000));
        assert!(super::supported_geometry([0xc2, 0x20, 0x16], 0x400000));
        assert!(!super::supported_geometry([0xef, 0x40, 0x18], 0x400000));
        assert!(!super::supported_geometry([0xbf, 0x25, 0x4a], 0x400000));
        assert!(!super::supported_geometry([0xef, 0x40, 0xff], 0));
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
}
