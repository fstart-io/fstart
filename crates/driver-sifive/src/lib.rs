//! SiFive SoC blocks: the FU740's clock controller and DDR controller, and
//! the SiFive GPIO controller.

#![no_std]

extern crate ufmt;

pub mod fu740_ddr;
pub mod fu740_prci;
pub mod gpio;

/// Busy-wait about `us` microseconds at the clocks the PRCI programs.
pub fn spin_delay_us(us: u32) {
    for _ in 0..u64::from(us) * 500 {
        core::hint::spin_loop();
    }
}
