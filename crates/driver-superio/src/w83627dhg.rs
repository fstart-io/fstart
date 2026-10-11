//! Winbond W83627DHG resources and physical GPIO pins.
//!
//! Register definitions follow W83627DHG V1.4 §§20.1 and 20.10. Unlike
//! IT8720F, this chip has no dedicated BSEL controller: boards may wire BSEL
//! signals to ordinary GPIOs. CR2C[4] is a read-only ACPI strap, not policy.

pub use crate::w83627_gpio::{GpioDirection, GpioPinConfig, GpioUpdate};
use crate::{DmaResource, IoResource, IrqResource, SuperIo, SuperIoChip, w83627_gpio};
use tock_registers::{interfaces::ReadWriteable, register_bitfields};

register_bitfields![u8,
    MULTIFUNCTION [
        VID_GTL OFFSET(3) NUMBITS(1) [],
        UART_B_PINS OFFSET(0) NUMBITS(2) [Uart = 3],
    ],
    ACPI_POWER [ DRAM_STANDBY_GATE OFFSET(4) NUMBITS(1) [] ],
];

pub struct W83627dhgChip;
impl SuperIoChip for W83627dhgChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x87];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    const EXIT_RAW: Option<u8> = Some(0xaa);
    const CHIP_ID: u16 = 0xa020;
    const CHIP_ID_MASK: u16 = 0xfff0;
    const COM1_LDN: Option<u8> = Some(2);
    const COM2_LDN: Option<u8> = Some(3);
    const KBC_LDN: Option<u8> = Some(5);
    const MOUSE_LDN: Option<u8> = Some(5);
    const MOUSE_IRQ_SECONDARY: bool = true;
    const EC_LDN: Option<u8> = None;
    // GPIOs use virtual activation bits, not generic base/enable semantics.
    const GPIO_LDN: Option<u8> = None;
    const CIR_LDN: Option<u8> = None;
    const PARALLEL_LDN: Option<u8> = Some(1);
}
pub type W83627dhg = SuperIo<W83627dhgChip>;

impl W83627dhg {
    /// Configure GP32–34, GP40–47 and GP50–57 without replacing unlisted
    /// controls or strap bits. EHG-only GPIO1 requests fail before I/O.
    /// Return data/inversion/mux changes separately from direction/activation.
    ///
    /// GPIO4 selects its entire bank instead of UART B, so COM2 must be absent.
    /// This is initialization, not a glitch-free runtime output transition:
    /// direction must precede data on DHG. Strap users must run on cold boot
    /// and reset after data/routing changes, never reprogram CPU straps on S3.
    pub fn configure_gpio(&mut self, pins: &[GpioPinConfig]) -> GpioUpdate {
        w83627_gpio::configure(self, pins, w83627_gpio::Profile::Dhg)
    }

    /// UART B shares all eight GPIO4 pins. Call only on boards wired for UART.
    pub fn select_uart_b_pins(&mut self) {
        self.enter_config();
        self.register::<MULTIFUNCTION::Register>(0x2c)
            .modify(MULTIFUNCTION::UART_B_PINS::Uart);
        self.exit_config();
    }
    /// Select the GTL/TTL VID input threshold; this does not drive VID outputs.
    pub fn set_vid_input_gtl(&mut self, gtl: bool) {
        self.enter_config();
        self.register::<MULTIFUNCTION::Register>(0x2c)
            .modify(MULTIFUNCTION::VID_GTL.val(gtl as u8));
        self.exit_config();
    }
    /// Enable the standby DRAM power gate without replacing power-loss policy.
    pub fn enable_dram_standby_gate(&mut self) {
        self.enter_config();
        self.logical_device(10).set_irq(IrqResource::Primary(0));
        self.register::<ACPI_POWER::Register>(0xe4)
            .modify(ACPI_POWER::DRAM_STANDBY_GATE::SET);
        self.logical_device(10).set_enabled(true);
        self.exit_config();
    }
    /// Allocate the floppy controller's standard PnP resources.
    pub fn enable_floppy(&mut self, base: u16, irq: u8, dma: u8) {
        self.enter_config();
        let mut device = self.logical_device(0);
        device.set_enabled(false);
        device.set_io_base(IoResource::Primary(base));
        device.set_irq(IrqResource::Primary(irq));
        device.set_dma(DmaResource::Primary(dma));
        device.set_enabled(true);
        self.exit_config();
    }
    /// Set the parallel controller's ECP DMA channel without changing its mode.
    pub fn set_parallel_dma(&mut self, channel: u8) {
        self.enter_config();
        self.logical_device(1)
            .set_dma(DmaResource::Primary(channel));
        self.exit_config();
    }
    /// Disable the unconnected SPI logical device, not the GPIO mux registers.
    pub fn disable_spi(&mut self) {
        self.enter_config();
        self.logical_device(6).set_enabled(false);
        self.exit_config();
    }
    /// GPIO6 has a virtual activation bit; retain its strapped pin directions.
    pub fn enable_gpio6(&mut self) {
        self.enter_config();
        self.logical_device(7).set_activation_bit(3, true);
        self.exit_config();
    }
    /// Disable the watchdog counter, not merely its PnP activation.
    pub fn disable_watchdog(&mut self) {
        self.enter_config();
        self.logical_device(8).write(0xf6, 0);
        self.logical_device(8).set_enabled(false);
        self.exit_config();
    }
    /// Allocate the single-base hardware-monitor window with no IRQ.
    pub fn enable_hwmon(&mut self, base: u16) {
        self.enter_config();
        let mut device = self.logical_device(11);
        device.set_enabled(false);
        device.set_io_base(IoResource::Primary(base));
        device.set_irq(IrqResource::Primary(0));
        device.set_enabled(true);
        self.exit_config();
    }
    pub fn disable_floppy(&mut self) {
        self.enter_config();
        self.logical_device(0).set_enabled(false);
        self.exit_config();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dhg_revision_and_shared_keyboard_mouse_resources() {
        for id in 0xa020..=0xa02f {
            assert_eq!(id & W83627dhgChip::CHIP_ID_MASK, W83627dhgChip::CHIP_ID);
        }
        assert_eq!(W83627dhgChip::KBC_LDN, W83627dhgChip::MOUSE_LDN);
        const { assert!(W83627dhgChip::MOUSE_IRQ_SECONDARY) };
    }
}
