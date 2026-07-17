//! SiFive FU540/FU740 UART.

use fstart_core::services::{Console, DeviceError, ServiceError};
use fstart_core::{Mmio32, MmioAddr};
use tock_registers::register_bitfields;
use tock_registers::LocalRegisterCopy;

const REG_TXDATA: usize = 0x00;
const REG_RXDATA: usize = 0x04;
const REG_TXCTRL: usize = 0x08;
const REG_RXCTRL: usize = 0x0c;
const REG_IE: usize = 0x10;
const REG_DIV: usize = 0x18;

register_bitfields! [u32,
    TXDATA [ DATA OFFSET(0) NUMBITS(8) [], FULL OFFSET(31) NUMBITS(1) [] ],
    RXDATA [ DATA OFFSET(0) NUMBITS(8) [], EMPTY OFFSET(31) NUMBITS(1) [] ],
    TXCTRL [ TXEN OFFSET(0) NUMBITS(1) [], TXCNT OFFSET(16) NUMBITS(3) [] ],
    RXCTRL [ RXEN OFFSET(0) NUMBITS(1) [], RXCNT OFFSET(16) NUMBITS(3) [] ],
];

/// Static SiFive UART wiring and precomputed baud divisor.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SifiveUartConfig {
    pub base: MmioAddr<Mmio32>,
    pub divisor: u32,
}

impl SifiveUartConfig {
    /// Const-build a UART policy so the early path never calculates a divisor.
    #[must_use]
    pub const fn new(base: MmioAddr<Mmio32>, clock_freq: u32, baud_rate: u32) -> Self {
        if clock_freq == 0 || baud_rate == 0 {
            panic!("SiFive UART clock and baud rate must not be zero");
        }
        let divisor = (clock_freq as u64 + baud_rate as u64 - 1) / baud_rate as u64;
        if divisor == 0 || divisor > u32::MAX as u64 + 1 {
            panic!("SiFive UART divisor is invalid");
        }
        Self {
            base,
            divisor: divisor as u32 - 1,
        }
    }
}

/// Polled SiFive UART.
pub struct SifiveUart {
    config: &'static SifiveUartConfig,
}

unsafe impl Send for SifiveUart {}
unsafe impl Sync for SifiveUart {}

impl SifiveUart {
    /// Construct without touching the UART.
    pub fn new(config: &'static SifiveUartConfig) -> Result<Self, DeviceError> {
        Ok(Self { config })
    }

    #[inline(always)]
    fn read(&self, offset: usize) -> u32 {
        // SAFETY: board-owned typed MMIO configuration names this UART block.
        unsafe {
            fstart_core::mmio::read32((self.config.base.raw() as usize + offset) as *const u32)
        }
    }

    #[inline(always)]
    fn write(&self, offset: usize, value: u32) {
        // SAFETY: board-owned typed MMIO configuration names this UART block.
        unsafe {
            fstart_core::mmio::write32(
                (self.config.base.raw() as usize + offset) as *mut u32,
                value,
            )
        }
    }

    /// Enable the polled 8-bit UART.
    pub fn init(&mut self) -> Result<(), DeviceError> {
        self.write(REG_DIV, self.config.divisor);
        self.write(REG_TXCTRL, (TXCTRL::TXEN::SET + TXCTRL::TXCNT.val(1)).value);
        self.write(REG_RXCTRL, (RXCTRL::RXEN::SET + RXCTRL::RXCNT.val(0)).value);
        self.write(REG_IE, 0);
        Ok(())
    }
}

impl Console for SifiveUart {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        while LocalRegisterCopy::<u32, TXDATA::Register>::new(self.read(REG_TXDATA))
            .is_set(TXDATA::FULL)
        {
            core::hint::spin_loop();
        }
        self.write(REG_TXDATA, u32::from(byte));
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        let value = LocalRegisterCopy::<u32, RXDATA::Register>::new(self.read(REG_RXDATA));
        Ok((!value.is_set(RXDATA::EMPTY)).then(|| value.read(RXDATA::DATA) as u8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::mmio32;

    #[test]
    fn const_builds_baud_divisor() {
        const CONFIG: SifiveUartConfig = SifiveUartConfig::new(mmio32(0), 130_000_000, 115_200);
        assert_eq!(CONFIG.divisor, 1_128);
    }
}
