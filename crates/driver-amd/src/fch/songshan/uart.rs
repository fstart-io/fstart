// SPDX-License-Identifier: MIT OR Apache-2.0
//! Genoa's Songshan MMIO UARTs, not Picasso's pin map or a BMC SIO UART.
//! Register facts: public Genoa openSIL/coreboot UART, AOAC and GPIO tables.
//! Board policy selects a UART; the BMC's own routing remains external.
use crate::fch::{self, Bus, Fch, gpio::Control, gpio_bank, iomux};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{Readable, Writeable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uart {
    Uart0,
    Uart1,
    Uart2,
}
impl Uart {
    pub const fn base(self) -> u64 {
        match self {
            Self::Uart0 => 0xfedc_9000,
            Self::Uart1 => 0xfedc_a000,
            Self::Uart2 => 0xfedc_e000,
        }
    }
    /// The UART's FCH PCI_INTR/IOAPIC source (coreboot genoa_poc
    /// `amd_pci_int_defs.h`).
    pub const fn intr_source(self) -> u8 {
        match self {
            Self::Uart0 => 0x74,
            Self::Uart1 => 0x75,
            Self::Uart2 => 0x78,
        }
    }
    const fn device(self) -> u8 {
        match self {
            Self::Uart0 => 11,
            Self::Uart1 => 12,
            Self::Uart2 => 16,
        }
    }
    const fn pins(self) -> [u8; 2] {
        match self {
            Self::Uart0 => [136, 138],
            Self::Uart1 => [141, 142],
            Self::Uart2 => [137, 135],
        }
    }
    /// BSP-exclusive, decoded FCH and mapped ACPI MMIO; no outstanding pad
    /// users. Powers only AMBA and the selected UART. Leaves eSPI, legacy
    /// decodes and unrelated GPIO/interrupt state unchanged.
    pub fn prepare(self, fch: &Fch<impl Bus>, delay: &mut impl DelayNs) -> Result<(), fch::Error> {
        // Check both pads before any write. Do not run a full GPIO-bank init
        // (which would alter global interrupt delivery) for two UART pads.
        for pin in self.pins() {
            if fch.reg(iomux::at::<u8, ()>(pin.into())).get() == u8::MAX
                || fch
                    .reg(gpio_bank::at::<u32, Control::Register>(u32::from(pin) * 4))
                    .get()
                    == u32::MAX
            {
                return Err(fch::Error::Unavailable(self.device()));
            }
        }
        fch.power_devices(&[17, self.device()], 1000, delay)?;
        for pin in self.pins() {
            let mux = fch.reg(iomux::at::<u8, ()>(pin.into()));
            mux.set(0); // UART RX/TX native function on each of these pads.
            mux.get();
            let control = fch.reg(gpio_bank::at::<u32, Control::Register>(u32::from(pin) * 4));
            let mut value = control.extract();
            value.modify(
                Control::PULL_UP::CLEAR
                    + Control::PULL_DOWN::CLEAR
                    + Control::INTERRUPT_STATUS::CLEAR
                    + Control::WAKE_STATUS::CLEAR,
            );
            control.set(value.get()); // zero W1C status bits, preserve other configuration
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::{Acpimmio, Space, aoac_control, aoac_state, testing::World};
    #[test]
    fn uart1_uses_genoa_tx142_not_picasso_tx143_and_preserves_other_bits() {
        let world = World::default();
        let fch = Fch::new(&world, Acpimmio::DIE0);
        for device in [17, 12] {
            fch.reg(aoac_state(device)).set(3);
            fch.reg(aoac_control(device)).set(0x40);
        }
        for pin in [141, 142] {
            fch.reg(gpio_bank::at::<u32, Control::Register>(pin * 4))
                .set(0x3032_0400);
        }
        world.clear_log();
        Uart::Uart1
            .prepare(&fch, &mut crate::ccx::tsc::testing::none())
            .unwrap();
        assert_eq!(
            world.writes(),
            [
                (Space::Mmio, 0xfed8_1e62, 1, 0x49),
                (Space::Mmio, 0xfed8_1e58, 1, 0x49),
                (Space::Mmio, 0xfed8_0d8d, 1, 0),
                (Space::Mmio, 0xfed8_1734, 4, 0x0002_0400),
                (Space::Mmio, 0xfed8_0d8e, 1, 0),
                (Space::Mmio, 0xfed8_1738, 4, 0x0002_0400),
            ]
        );
        assert_eq!(Uart::Uart1.base(), 0xfedc_a000);
    }
    #[test]
    fn unavailable_pad_has_no_writes_and_power_wait_is_bounded() {
        let world = World::default();
        let fch = Fch::new(&world, Acpimmio::DIE0);
        fch.reg(iomux::at::<u8, ()>(142)).set(0xff);
        world.clear_log();
        assert!(
            Uart::Uart1
                .prepare(&fch, &mut crate::ccx::tsc::testing::none())
                .is_err()
        );
        assert!(world.writes().is_empty());
        fch.reg(iomux::at::<u8, ()>(142)).set(0);
        let timer = fstart_core::services::ManualTimer::new();
        let mut delay = fstart_core::services::TimerDelay(&timer);
        assert_eq!(
            Uart::Uart1.prepare(&fch, &mut delay),
            Err(fch::Error::PowerTimeout(17))
        );
        assert_eq!(timer.now(), 100_000);
    }
}
