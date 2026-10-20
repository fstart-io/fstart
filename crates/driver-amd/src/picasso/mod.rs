// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h Raven/Picasso/Pollock register geometry, not Intel-compatible IP.
pub mod acp;
pub use crate::fam17h::{apob, cache, tsc};
pub mod cpu;
#[cfg(test)]
mod cpu_tests;
pub mod dxio;
mod dxio_hardware;
pub mod emmc;
pub mod espi;
#[cfg(test)]
mod espi_tests;
pub mod fabric;
pub mod legacy;
#[cfg(test)]
mod legacy_tests;
#[cfg(test)]
mod native_tests;
pub mod nbio;
pub mod nbio_late;
mod nbio_tables;
pub mod pinmux;
pub mod preinit;
pub mod psp;
pub mod southbridge;
pub mod usb;
#[cfg(test)]
mod usb_tests;
pub use crate::fch::ACPI_MMIO;

/// The Raven/Picasso SoC: one die.
pub enum Picasso {}
impl crate::fam17h::topology::Soc for Picasso {
    const MAX_DIES: usize = 1;
    const SMU: crate::smu::Mailbox = crate::smu::v10::MAILBOX;
    const DXIO_SERVICE: u32 = crate::smu::v10::DXIO_SERVICE;
    fn ioms_fabric_id(_: u8) -> u8 {
        9
    }
}
pub type Die<P> = crate::fam17h::topology::Die<Picasso, P>;
pub const ECAM: u64 = 0xf800_0000;
pub const ECAM_BUSES: u8 = 64;
pub const UART_BASES: [usize; 4] = [0xfedc9000, 0xfedca000, 0xfedce000, 0xfedcf000];
pub const UART_AOAC: [u8; 4] = [11, 12, 16, 26];
pub const EARLY_AOAC: &[u8] = &[17, 7, 8, 9, 27];
/// GPIO bank 0: pins 0-144, no pad 63.
pub const GPIO_BANK: crate::gpio::Bank = crate::gpio::Bank {
    pins: 145,
    reserved: &[63],
    gpio_function: pinmux::gpio_function,
    events: crate::gpio::FAMILY17H_EVENTS,
};
