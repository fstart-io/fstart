//! SiFive GPIO controller (FU540, FU740).

use embedded_hal::digital::{ErrorType, OutputPin};
use fstart_core::mmio::MmioReadWrite;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_structs;

/// The FU740's GPIO controller.
pub const FU740_GPIO_BASE: usize = 0x1006_0000;

register_structs! {
    /// One bit per pin in every register.
    GpioRegs {
        (0x00 => _input_val),
        (0x04 => input_en: MmioReadWrite<u32>),
        (0x08 => output_en: MmioReadWrite<u32>),
        (0x0c => output_val: MmioReadWrite<u32>),
        (0x10 => _reserved0),
        (0x38 => iof_en: MmioReadWrite<u32>),
        (0x3c => iof_sel: MmioReadWrite<u32>),
        (0x40 => @END),
    }
}

/// A SiFive GPIO controller.
#[derive(Debug, Clone, Copy)]
pub struct Gpio {
    base: usize,
}

impl Gpio {
    /// # Safety
    /// `base` must be a SiFive GPIO controller's register block.
    #[must_use]
    pub const unsafe fn new(base: usize) -> Self {
        Self { base }
    }

    fn regs(&self) -> &GpioRegs {
        // SAFETY: `new` was given the controller's register block.
        unsafe { &*(self.base as *const GpioRegs) }
    }

    /// Route `pin` to peripheral function `sel` (IOF0/IOF1) instead of
    /// software GPIO. Nothing at reset routes e.g. the UART0 pads
    /// (GPIO 16/17, IOF0), so early firmware must do this itself rather
    /// than inherit whatever a previous boot stage left behind.
    pub fn route_iof(self, pin: u8, sel: bool) {
        let mask = 1u32 << pin;
        let regs = self.regs();
        if sel {
            regs.iof_sel.set(regs.iof_sel.get() | mask);
        } else {
            regs.iof_sel.set(regs.iof_sel.get() & !mask);
        }
        regs.iof_en.set(regs.iof_en.get() | mask);
    }

    /// Drive `pin` as an output, starting from its current output level.
    #[must_use]
    pub fn output(self, pin: u8) -> Pin {
        let mask = 1u32 << pin;
        let regs = self.regs();
        regs.input_en.set(regs.input_en.get() & !mask);
        regs.output_en.set(regs.output_en.get() | mask);
        Pin { gpio: self, mask }
    }
}

/// One GPIO driven as an output.
#[derive(Debug)]
pub struct Pin {
    gpio: Gpio,
    mask: u32,
}

impl ErrorType for Pin {
    type Error = core::convert::Infallible;
}

impl OutputPin for Pin {
    fn set_high(&mut self) -> Result<(), Self::Error> {
        let regs = self.gpio.regs();
        regs.output_val.set(regs.output_val.get() | self.mask);
        Ok(())
    }

    fn set_low(&mut self) -> Result<(), Self::Error> {
        let regs = self.gpio.regs();
        regs.output_val.set(regs.output_val.get() & !self.mask);
        Ok(())
    }
}
