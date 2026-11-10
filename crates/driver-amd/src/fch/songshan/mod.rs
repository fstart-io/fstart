// SPDX-License-Identifier: MIT OR Apache-2.0
//! Songshan FCH mechanisms. Policy and wired ports belong to the board.
pub mod uart;
pub mod usb;

/// Songshan local GPIO bank 0 (ACPI MMIO `0x1500`, IOMUX `0x0d00`). The
/// IOMUX GPIO functions and GEVENT routing are not decoded: board tables
/// name functions by number. The alternate bank at `0x1200`/`0x12c0`
/// (CPM logical pins >= 256) is a separate register block.
pub const GPIO_BANK: crate::fch::gpio::Bank = crate::fch::gpio::Bank {
    pins: 148,
    reserved: &[],
    gpio_function: |_| None,
    events: &[],
};
