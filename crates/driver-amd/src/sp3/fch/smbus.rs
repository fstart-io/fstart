//! SMBus/ASF I/O and the I2C controller enables.
//!
//! The SMBus host and ASF blocks live at the I/O base PMx00\[15:8\]
//! (`B00h`, set by ResetHwAcpi; ASF at `B20h`). The I2C controllers are
//! DesignWare masters driven by `fstart-driver-i2c`; here they are only
//! powered and routed. Controller 5 is not at a linear MMIO offset.

use fstart_driver_i2c::designware::{Speed, Timing};

use super::FchHw;
use super::acpimmio::{self, AoacDevice};
use super::gpio::{self, Direction, Pad, Pin};

/// RXM100CB AmdI2CMasterPei standard timing: 100 kHz at 150 MHz.
pub const I2C_TIMING: Timing = Timing {
    speed: Speed::Standard,
    scl_high: 0x285,
    scl_low: 0x357,
    spike_length: Some(7),
    sda_hold: None,
};

/// SMBus host I/O base after ResetHwAcpi (PMx00\[15:8\] = `0Bh`).
pub const SMBUS_IO: u32 = 0xB00;
/// ASF I/O base.
pub const ASF_IO: u32 = SMBUS_IO + 0x20;
/// DesignWare I2C MMIO base of controller `n` (sp4r2 ACPI `I2CA..I2CF`).
pub const fn i2c_base(n: u8) -> u64 {
    I2C[n as usize].3 as u64
}

/// I2C controllers 0..5. The AOAC device numbers and pads are leads from
/// the Taishan sp4r2 build (FCH_TAISHAN §7, inference): devices 5..10,
/// pads 145/146, 147/148 (function 0), 113/114, 19/20 (function 1),
/// 149/150, 151/152 (function 0). They match the S8030 `$A10` pads 145–152
/// and 113/114/19/20 (BOARD_S8030 §3.4), but were not re-checked in the SP3
/// donors' I2C PEIMs.
const I2C: [(u8, [u16; 2], u8, u32); 6] = [
    (5, [145, 146], 0, 0xfedc_2000),
    (6, [147, 148], 0, 0xfedc_3000),
    (7, [113, 114], 1, 0xfedc_4000),
    (8, [19, 20], 1, 0xfedc_5000),
    (9, [149, 150], 0, 0xfedc_6000),
    (10, [151, 152], 0, 0xfedc_b000),
];

/// Power I2C controller `n` and route its pads.
pub fn enable_i2c<H: FchHw>(hw: &H, n: u8) {
    let (dev, pins, function, _) = I2C[usize::from(n)];
    pins.iter().for_each(|&pin| {
        gpio::configure(
            hw.acpi(),
            &Pad {
                pin: Pin(pin),
                function: Some(function),
                pull: None,
                direction: Direction::Keep,
            },
        )
    });
    acpimmio::aoac_power(hw.acpi(), AoacDevice(dev), true);
}

#[cfg(test)]
mod tests {
    use super::super::model::fch;
    use super::*;

    #[test]
    fn i2c5_is_not_a_linear_offset() {
        assert_eq!(i2c_base(5), 0xfedc_b000);
    }

    #[test]
    fn i2c1_pads_and_power() {
        let hw = fch();
        enable_i2c(&hw, 1);
        assert_eq!(hw.acpi.get(0xd00 + 147, 1), 0);
        assert_eq!(hw.acpi.writes_to(0xd00 + 148), vec![0]);
        assert_eq!(hw.acpi.get(0x1e40 + 12, 1), 0x08);
    }
}
