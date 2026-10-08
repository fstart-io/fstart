pub mod acpi;
pub mod gpio_ich;
pub mod hda;
pub mod ide;
pub mod lpc;
pub mod pirq;
pub mod pmio_ich;
pub mod rtc;
pub mod smbus;
pub mod smi;
#[cfg(all(feature = "spi", target_arch = "x86_64"))]
pub mod spi;
