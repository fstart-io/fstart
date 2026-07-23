//! IDT CK505 clock generator driver (SMBus-attached).
//!
//! The CK505 is a common clock source on Atom D4xx / D5xx / NM10
//! reference boards. It is programmed over SMBus: a variable number
//! of registers select reference and bus clock dividers,
//! spread-spectrum options, and output enables. Board authors provide
//! `regs` and `mask` vectors — the driver applies
//! `(new_val & mask) | (read_val & !mask)` using byte-at-a-time
//! SMBus read-modify-writes for each register.

use fstart_core::services::device::DeviceError;
use fstart_core::services::SmBus;
use heapless::Vec;
use serde::{Deserialize, Serialize};

/// Configuration for the CK505 clock generator.
///
/// `mask` and `regs` must have the same length — one entry per
/// clock register. Typical clock generators have 5–21 registers;
/// the `Vec<u8, 32>` capacity handles all known parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct I2cCk505Config {
    /// Mask bytes — bit set = register position is written from `regs`.
    pub mask: Vec<u8, 32>,
    /// Register values to apply (AND-masked by `mask`).
    pub regs: Vec<u8, 32>,
}

/// CK505 driver state.
pub struct I2cCk505 {
    /// 7-bit SMBus slave address (supplied by the bus attachment).
    addr: u8,
    config: I2cCk505Config,
}

// SAFETY: state is CPU-exclusive during firmware phase.
unsafe impl Send for I2cCk505 {}
unsafe impl Sync for I2cCk505 {}

impl I2cCk505 {
    /// Construct a CK505 at a fixed SMBus address.
    pub fn new_at_address(config: I2cCk505Config, addr: u8) -> Result<Self, DeviceError> {
        if config.mask.len() != config.regs.len() {
            return Err(DeviceError::MissingResource(
                "ck505: mask/regs length mismatch",
            ));
        }
        Ok(Self { addr, config })
    }

    /// Initialize through a concrete SMBus provider.
    pub fn init_on_smbus<B: SmBus + ?Sized>(&mut self, bus: &mut B) -> Result<(), DeviceError> {
        fstart_log::info!(
            "i2c-ck505: programming {} registers at addr={:#x}",
            self.config.regs.len(),
            self.addr,
        );
        for (idx, (&mask, &value)) in self
            .config
            .mask
            .iter()
            .zip(self.config.regs.iter())
            .enumerate()
        {
            let cmd = idx as u8;
            let old = bus
                .read_byte(self.addr, cmd)
                .map_err(|_| DeviceError::BusError)?;
            let new = (value & mask) | (old & !mask);
            if new != old {
                bus.write_byte(self.addr, cmd, new)
                    .map_err(|_| DeviceError::BusError)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeBus {
        regs: [u8; 8],
        writes: heapless::Vec<(u8, u8, u8), 8>,
    }

    impl SmBus for FakeBus {
        fn read_byte(
            &mut self,
            addr: u8,
            cmd: u8,
        ) -> Result<u8, fstart_core::services::ServiceError> {
            let _ = addr;
            Ok(self.regs[cmd as usize])
        }

        fn write_byte(
            &mut self,
            addr: u8,
            cmd: u8,
            value: u8,
        ) -> Result<(), fstart_core::services::ServiceError> {
            self.regs[cmd as usize] = value;
            self.writes.push((addr, cmd, value)).unwrap();
            Ok(())
        }
    }

    #[test]
    fn init_on_bus_applies_masked_writes() {
        let cfg = I2cCk505Config {
            mask: heapless::Vec::from_slice(&[0x0f, 0xf0]).unwrap(),
            regs: heapless::Vec::from_slice(&[0x05, 0xa0]).unwrap(),
        };
        let mut bus = FakeBus {
            regs: [0xf0, 0x0f, 0, 0, 0, 0, 0, 0],
            writes: heapless::Vec::new(),
        };
        let mut ck505 =
            I2cCk505::new_at_address(cfg, 0x69).expect("CK505 should accept I2C/SMBus address");

        ck505.init_on_smbus(&mut bus).unwrap();

        assert_eq!(bus.regs[0], 0xf5);
        assert_eq!(bus.regs[1], 0xaf);
        assert_eq!(bus.writes.as_slice(), &[(0x69, 0, 0xf5), (0x69, 1, 0xaf)]);
    }

    #[test]
    fn construction_rejects_mask_reg_len_mismatch() {
        let cfg = I2cCk505Config {
            mask: heapless::Vec::from_slice(&[0xff]).unwrap(),
            regs: heapless::Vec::new(),
        };

        assert!(I2cCk505::new_at_address(cfg, 0x69).is_err());
    }
}
