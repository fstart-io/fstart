//! Shared ICH8/ICH9 southbridge helpers: IFD SPIBAR descriptor inspection,
//! PCIe root-port slot/ASPM setup, PCI-to-PCI bridge init, and legacy DMA.

use fstart_core::memory::{FlashLayout, IntelIfdFlashLayout, IntelIfdRegion};
use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use fstart_core::services::{FirmwareImage, FirmwareWindow, ServiceError};
use fstart_pci::ecam;
use fstart_pci::{PCI_COMMAND_BITS, PCI_EXPRESS_CAPABILITIES, PciType0Config, PciType1Config};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields, register_structs};

use super::PciePowerLimit;

/// ICH PCIe root-port PCI device number (`D28:F0..F5`).
const PCIE_DEV: u8 = 0x1c;
/// ICH PCI-to-PCI bridge PCI device/function (`D30:F0`).
const PCI_BRIDGE_DEV: u8 = 0x1e;
const PCI_BRIDGE_FUNC: u8 = 0;

/// PCIe root-port config offsets (`D28:Fx`).
const D28FX_XCAP: u16 = 0x42;
const D28FX_LCAP: u16 = 0x4c;
const D28FX_SLCAP: u16 = 0x54;

/// PCI-to-PCI bridge secondary master latency timer (`D30:F0` offset `0x1b`).
const D30F0_SMLT: u16 = 0x1b;

/// Shift for 4 KiB-granular FREG base/limit fields.
const SPI_FREG_SHIFT: u32 = 12;

register_bitfields![u32,
    /// ICH SPIBAR Hardware Sequencing Flash Status (`HSFS`, 16-bit at offset `0x04`,
    /// adjacent to 16-bit `HSFC` at `0x06`, read as a 32-bit word).
    SPI_HSFS [
        /// Flash Descriptor Valid (`FDV`, bit 14).
        DESCRIPTOR_VALID OFFSET(14) NUMBITS(1) []
    ],
    /// Intel Flash Descriptor `FLMAP0` (descriptor offset `0x04`, 32-bit).
    SPI_FLMAP0 [
        /// Number of flash components minus one (`NC`, bits 9:8).
        COMPONENTS OFFSET(8) NUMBITS(2) []
    ],
    /// Intel Flash Descriptor `FLCOMP` (component section offset `0x00`, 32-bit).
    SPI_FLCOMP [
        /// Component 0 density encoding (`1 << (19 + val)` bytes, bits 2:0).
        DENSITY0 OFFSET(0) NUMBITS(3) [],
        /// Component 1 density encoding (`1 << (19 + val)` bytes, bits 5:3).
        DENSITY1 OFFSET(3) NUMBITS(3) []
    ],
    /// ICH SPIBAR Flash Region `FREG0..4` (offsets `0x54..=0x64`, 32-bit).
    SPI_FREG [
        /// Region base in 4 KiB units (bits 12:0).
        BASE OFFSET(0) NUMBITS(13) [],
        /// Region limit (inclusive) in 4 KiB units (bits 28:16).
        LIMIT OFFSET(16) NUMBITS(13) []
    ],
    /// PCIe Slot Capabilities (`D28:Fx` offset `0x54`, 32-bit).
    PCIE_SLCAP [
        /// Slot Power Limit Value (bits 14:7).
        POWER_LIMIT_VALUE OFFSET(7) NUMBITS(8) [],
        /// Slot Power Limit Scale (bits 16:15).
        POWER_LIMIT_SCALE OFFSET(15) NUMBITS(2) [],
        /// Physical Slot Number (bits 31:19).
        PHYSICAL_SLOT_NUMBER OFFSET(19) NUMBITS(13) []
    ]
];

register_structs! {
    /// ICH8/ICH9 SPIBAR descriptor-inspection registers.
    IchSpiDescRegs {
        (0x00 => _res0: [u8; 0x04]),
        /// Hardware Sequencing Flash Status/Control (`HSFS`/`HSFC`, offset `0x04`, 32-bit).
        (0x04 => hsfs: MmioReadOnly<u32, SPI_HSFS::Register>),
        (0x08 => _res1: [u8; 0x4c]),
        /// Flash Region 0..4 registers (offsets `0x54..=0x64`, 5 × 32-bit).
        (0x54 => freg: [MmioReadOnly<u32, SPI_FREG::Register>; 5]),
        (0x68 => _res2: [u8; 0x48]),
        /// Flash Descriptor Observability Control (offset `0xb0`, 32-bit).
        (0xb0 => fdoc: MmioReadWrite<u32>),
        /// Flash Descriptor Observability Data (offset `0xb4`, 32-bit).
        (0xb4 => fdod: MmioReadOnly<u32>),
        (0xb8 => @END),
    }
}

/// Read one 32-bit word from the Intel Flash Descriptor via `FDOC`/`FDOD`.
#[inline]
fn spi_descriptor_word(regs: &IchSpiDescRegs, fdoc: u32) -> u32 {
    regs.fdoc.set(fdoc);
    regs.fdod.get()
}

/// Decode total SPI flash component size from `FLMAP0` + `FLCOMP`.
fn spi_flash_component_size(regs: &IchSpiDescRegs) -> u32 {
    // Match coreboot southbridge/intel/common/spi.c: observe descriptor
    // component section 0 (FDOC=0x1000) and decode density fields.
    let flmap0 = LocalRegisterCopy::<u32, SPI_FLMAP0::Register>::new(spi_descriptor_word(regs, 4));
    let flcomp =
        LocalRegisterCopy::<u32, SPI_FLCOMP::Register>::new(spi_descriptor_word(regs, 0x1000));
    let mut size = 1u32 << (19 + flcomp.read(SPI_FLCOMP::DENSITY0));
    if flmap0.read(SPI_FLMAP0::COMPONENTS) != 0 {
        size = size.saturating_add(1u32 << (19 + flcomp.read(SPI_FLCOMP::DENSITY1)));
    }
    size
}

/// Decode `(offset, size)` in bytes for `FREG[index]` (ICH8/ICH9 implement FREG0..4).
fn spi_ifd_region(regs: &IchSpiDescRegs, index: usize) -> (u32, u32) {
    let Some(freg) = regs.freg.get(index) else {
        return (0, 0);
    };
    let reg = LocalRegisterCopy::<u32, SPI_FREG::Register>::new(freg.get());
    let base = reg.read(SPI_FREG::BASE) << SPI_FREG_SHIFT;
    let limit = reg.read(SPI_FREG::LIMIT) << SPI_FREG_SHIFT;
    if limit < base {
        return (0, 0);
    }
    (base, limit + (1 << SPI_FREG_SHIFT) - base)
}

/// Verify that the hardware SPI flash descriptor at `spibar_base` matches the
/// board's build-time [`IntelIfdFlashLayout`].
///
/// # Safety
///
/// `spibar_base` must point to a 4-byte-aligned, mapped ICH8/ICH9 SPIBAR
/// register block, and callers must serialize access to the `FDOC`/`FDOD`
/// selector pair.
pub(crate) unsafe fn verify_ifd_flash_layout(
    spibar_base: usize,
    label: &str,
    expected: &IntelIfdFlashLayout,
) -> Result<(), ServiceError> {
    // SAFETY: caller guarantees `spibar_base` is a mapped ICH SPIBAR block.
    let regs = unsafe { &*(spibar_base as *const IchSpiDescRegs) };
    if !regs.hsfs.is_set(SPI_HSFS::DESCRIPTOR_VALID) {
        fstart_log::error!("{}: SPI descriptor valid bit is clear", label);
        return Err(ServiceError::HardwareError);
    }

    let runtime_size = spi_flash_component_size(regs);
    if runtime_size != expected.size() {
        fstart_log::error!(
            "{}: SPI flash size mismatch: configured={:#x} runtime={:#x}",
            label,
            expected.size(),
            runtime_size
        );
        return Err(ServiceError::HardwareError);
    }

    for region in expected.regions.as_slice() {
        let Some(index) = region.kind.flreg_index() else {
            continue;
        };
        let (runtime_offset, runtime_size) = spi_ifd_region(regs, index);
        if runtime_offset != region.offset || runtime_size != region.size {
            fstart_log::error!(
                "{}: SPI {} region mismatch: configured off={:#x} size={:#x} runtime off={:#x} size={:#x}",
                label,
                region.kind.as_str(),
                region.offset,
                region.size,
                runtime_offset,
                runtime_size
            );
            return Err(ServiceError::HardwareError);
        }
    }

    if let Some(bios) = expected.bios_region() {
        let expected_host_base = expected.base() + u64::from(bios.offset);
        let runtime_host_base = 0x1_0000_0000u64 - u64::from(bios.size);
        if expected_host_base != runtime_host_base {
            fstart_log::error!(
                "{}: BIOS host window mismatch: configured={:#x} runtime={:#x}",
                label,
                expected_host_base,
                runtime_host_base
            );
            return Err(ServiceError::HardwareError);
        }
    }

    fstart_log::info!("{}: SPI descriptor matches board flash_layout", label);
    Ok(())
}

/// Compare the flash descriptor at `spibar_base` against `expected`.
///
/// # Safety
///
/// `spibar_base` must point to a mapped ICH8/ICH9 SPIBAR register block.
pub(crate) unsafe fn verify_flash_layout(
    spibar_base: usize,
    label: &str,
    expected: &FlashLayout,
) -> Result<(), ServiceError> {
    match expected {
        // SAFETY: forwarded from caller.
        FlashLayout::IntelIfd(layout) => unsafe {
            verify_ifd_flash_layout(spibar_base, label, layout)
        },
        FlashLayout::X86Legacy(_) => Err(ServiceError::NotSupported),
    }
}

/// Build the top-aligned 4 GiB BIOS [`FirmwareImage`] from the hardware SPI
/// descriptor at `spibar_base`.
///
/// # Safety
///
/// `spibar_base` must point to a mapped ICH8/ICH9 SPIBAR register block.
pub(crate) unsafe fn ifd_firmware_image(
    spibar_base: usize,
    label: &str,
) -> Result<FirmwareImage, ServiceError> {
    // SAFETY: caller guarantees `spibar_base` is a mapped ICH SPIBAR block.
    let regs = unsafe { &*(spibar_base as *const IchSpiDescRegs) };
    if !regs.hsfs.is_set(SPI_HSFS::DESCRIPTOR_VALID) {
        fstart_log::error!("{}: SPI descriptor valid bit is clear", label);
        return Err(ServiceError::HardwareError);
    }

    let (_bios_offset, bios_size) = spi_ifd_region(
        regs,
        IntelIfdRegion::Bios
            .flreg_index()
            .ok_or(ServiceError::HardwareError)?,
    );
    if bios_size == 0 {
        fstart_log::error!("{}: SPI descriptor has no BIOS region", label);
        return Err(ServiceError::HardwareError);
    }

    // ICH8/ICH9 expose the BIOS region top-aligned below 4 GiB regardless of
    // where it sits in the physical SPI descriptor. The boot firmware image
    // is the BIOS region, so expose it as logical offset 0.
    let mut image = FirmwareImage::EMPTY;
    image.size = u64::from(bios_size);
    image.windows[0] = FirmwareWindow::new(
        0,
        0x1_0000_0000u64 - u64::from(bios_size),
        u64::from(bios_size),
    );
    image.window_count = 1;
    Ok(image)
}

/// Program `XCAP.SlotImplemented` and `SLCAP` power limits on ICH8/ICH9 PCIe
/// root ports (`D28:F0..F5`).
pub(crate) fn configure_pcie_slots(pcie_slots: &[bool; 6], power_limits: &[PciePowerLimit; 6]) {
    let mut slot_number = 1u32;
    for func in 0usize..6 {
        let port = ecam::EcamDevice::new(0, PCIE_DEV, func as u8);
        if port.read16(0) == 0xffff {
            continue;
        }
        // XCAP is a word at 0x42. Coreboot's config32(0x42) rounds down to
        // 0x40; copying it would modify the capability header.
        let mut capabilities = LocalRegisterCopy::<u16, PCI_EXPRESS_CAPABILITIES::Register>::new(
            port.read16(D28FX_XCAP),
        );
        capabilities
            .modify(PCI_EXPRESS_CAPABILITIES::SLOT_IMPLEMENTED.val(pcie_slots[func].into()));
        port.write16(D28FX_XCAP, capabilities.get());
        if pcie_slots[func] {
            let limit = power_limits[func];
            let mut slcap =
                LocalRegisterCopy::<u32, PCIE_SLCAP::Register>::new(port.read32(D28FX_SLCAP));
            slcap.modify(
                PCIE_SLCAP::PHYSICAL_SLOT_NUMBER.val(slot_number)
                    + PCIE_SLCAP::POWER_LIMIT_SCALE.val(u32::from(limit.scale) & 0x03)
                    + PCIE_SLCAP::POWER_LIMIT_VALUE.val(u32::from(limit.value)),
            );
            port.write32(D28FX_SLCAP, slcap.get());
            slot_number += 1;
        }
    }
}

/// Write-once lock `LCAP` ASPM capability on present ICH8/ICH9 PCIe root ports.
pub(crate) fn lock_pcie_aspm() {
    for func in 0u8..6 {
        let port = ecam::EcamDevice::new(0, PCIE_DEV, func);
        if port.read16(0) != 0xffff {
            port.write32(D28FX_LCAP, port.read32(D28FX_LCAP));
        }
    }
}

/// Initialize the ICH8/ICH9 DMI-to-PCI bridge (`D30:F0`).
pub(crate) fn init_pci_bridge() {
    let bridge = ecam::EcamDevice::new(0, PCI_BRIDGE_DEV, PCI_BRIDGE_FUNC);
    if bridge.read16(0) == 0xffff {
        return;
    }
    // SAFETY: ECAM is established by northbridge init; D30:F0 is a Type 1 bridge.
    let bridge_regs = unsafe { bridge.regs::<PciType1Config>() };
    bridge_regs.interrupt_line.set(0xff);
    bridge.and8_or8(D30F0_SMLT, 0x07, 0x04 << 3);
    bridge_regs.status_raw.set(bridge_regs.status_raw.get());
    bridge_regs
        .secondary_status_raw
        .set(bridge_regs.secondary_status_raw.get());
}

/// Clear I/O, memory, and bus-master enables on a bus-0 PCI device before
/// hiding it via Function Disable.
pub(crate) fn clear_pci_command(dev: u8, func: u8) {
    let pci = ecam::EcamDevice::new(0, dev, func);
    if pci.read16(0) != 0xffff {
        // SAFETY: ECAM is established by northbridge init.
        unsafe { pci.regs::<PciType0Config>() }.command.modify(
            PCI_COMMAND_BITS::IO_SPACE::CLEAR
                + PCI_COMMAND_BITS::MEMORY_SPACE::CLEAR
                + PCI_COMMAND_BITS::BUS_MASTER::CLEAR,
        );
    }
}

/// Initialize legacy ISA DMA controllers on x86 targets.
pub(crate) fn init_isa_dma() {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    fstart_arch::x86::legacy_pc::initialize_isa_dma();
}
