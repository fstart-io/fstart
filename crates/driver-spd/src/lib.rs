//! JEDEC SPD inventory, independent of the EEPROM transport and CPU vendor.
//!
//! Decodes module facts only, not memory-controller training parameters.
//! Callers supply the controller's configured rate separately and must not
//! confuse the SPD's rated maximum with the rate actually programmed.
#![no_std]

pub mod ddr4;

/// The SPD CRC: CRC-16/XMODEM (polynomial 0x1021, initial value 0), as
/// JEDEC defines it for the base and module-specific blocks of DDR3, DDR4
/// and DDR5 SPD. The result is stored little-endian after the block.
pub fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0u16, |crc, byte| {
        (0..8).fold(crc ^ (u16::from(*byte) << 8), |crc, _| {
            if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            }
        })
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc16_check_value() {
        // The CRC-16/XMODEM check value.
        assert_eq!(super::crc16(b"123456789"), 0x31c3);
    }
}
