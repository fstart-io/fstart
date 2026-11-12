//! Device attachment address types.

/// How a device physically attaches to its parent bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusAddress {
    /// PCI or PCIe device: (device number, function number).
    Pci(u8, u8),
    /// LPC (Low Pin Count) / ISA Plug-and-Play config index port.
    Lpc(u16),
    /// I2C / SMBus 7-bit address.
    I2c(u8),
    /// SPI chip-select index.
    Spi(u8),
    /// Plug-and-Play logical device number below a SuperIO config-port device.
    Pnp(u8),
}
