use fstart_core::mmio::MmioWindow;

/// Wrap the 16 KiB root-complex register block (`RCBA`) on ICH/PCH.
///
/// # Safety
/// `base` must be the mapped 16 KiB RCBA MMIO window.
pub unsafe fn rcba_window(base: usize) -> MmioWindow {
    // SAFETY: forwarded from caller.
    unsafe { MmioWindow::new(base, 0x4000) }
}

/// Latch the PCH's read/write-once fields at finalize, preserving their values.
pub fn complete_pch_rwo(rcba: MmioWindow, pci: impl fstart_pci::ConfigRegionAccess) {
    use fstart_pci::{ConfigRegister, PciAddress};
    use tock_registers::interfaces::{Readable, Writeable};
    /// RCBA + 0x21a4 (32 bits), DMI link configuration.
    const DMI_LCFG: u32 = 0x21a4;
    /// HDA PCI 0x74 (32 bits), read/write-once configuration.
    const HDA_RWO: u16 = 0x74;
    rcba.write32(DMI_LCFG, rcba.read32(DMI_LCFG));
    let hda = PciAddress::new(0, 0, 27, 0);
    // SAFETY: ramstage owns bus zero. Disabled HDA must not be written.
    if unsafe { pci.read(hda, 0) } as u16 == 0x8086 {
        let reg = unsafe { ConfigRegister::<_, ()>::new(pci, hda, HDA_RWO) };
        reg.set(reg.get());
    }
}

/// Unlock EHCI subsystem ID and HCSPARAMS (PCI 0x80, 8 bits) for one operation.
pub fn with_ehci_unlocked<T>(ehci: fstart_pci::ecam::EcamDevice, f: impl FnOnce() -> T) -> T {
    let access = ehci.read8(0x80);
    ehci.write8(0x80, access | 1);
    let result = f();
    ehci.write8(0x80, access);
    result
}

pub mod acpi;
pub mod ahci;
pub mod ehci_debug;
pub mod gpio_ich;
pub mod hda;
pub(crate) mod ich;
pub mod ide;
pub mod lpc;
pub mod mei;
pub mod options;
pub mod pirq;
pub mod pmio_ich;
pub mod smbus;
pub mod smi;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod spi;
mod spi_menu;
pub use spi_menu::spi_menu;
pub mod spibar;
pub mod xhci;

/// PCIe root-port slot power-limit fields shared by ICH8/ICH9.
#[derive(Debug, Clone, Copy, Default)]
pub struct PciePowerLimit {
    /// Power-limit value encoded in PCIe Slot Capabilities.
    pub value: u8,
    /// Power-limit scale encoded in PCIe Slot Capabilities.
    pub scale: u8,
}
