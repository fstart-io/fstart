//! HiFive Unmatched board support package.

#![no_std]

pub mod config;
mod ddr_regs;

/// HiFive Unmatched board marker.
pub struct Board;

pub use config::*;

/// Pulse the Ethernet PHY's reset (GPIO 12) and wait for it to come up
/// unmanaged, before the SoC enables the MAC clock.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
pub fn reset_ethernet_phy() {
    use embedded_hal::digital::OutputPin;
    use fstart_driver_sifive::gpio::{FU740_GPIO_BASE, Gpio};
    use fstart_driver_sifive::spin_delay_us;
    // SAFETY: the FU740's GPIO controller.
    let mut reset = unsafe { Gpio::new(FU740_GPIO_BASE) }.output(12);
    let _ = reset.set_high();
    spin_delay_us(1);
    let _ = reset.set_low();
    spin_delay_us(1);
    let _ = reset.set_high();
    spin_delay_us(15_000);
}
