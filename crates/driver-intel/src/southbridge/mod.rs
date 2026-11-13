pub mod acpi;
pub mod ahci;
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
