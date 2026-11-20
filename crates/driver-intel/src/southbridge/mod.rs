pub mod acpi;
pub mod ahci;
pub mod ehci_debug;
pub mod gpio_ich;
pub mod hda;
pub mod ide;
pub mod lpc;
pub mod options;
pub mod pirq;
pub mod pmio_ich;
pub mod smbus;
pub mod smi;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod spi;

/// PCIe root-port slot power-limit fields shared by ICH8/ICH9.
#[derive(Debug, Clone, Copy, Default)]
pub struct PciePowerLimit {
    /// Power-limit value encoded in PCIe Slot Capabilities.
    pub value: u8,
    /// Power-limit scale encoded in PCIe Slot Capabilities.
    pub scale: u8,
}
