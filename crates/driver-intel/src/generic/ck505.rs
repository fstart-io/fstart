//! IDT CK505 clock generator driver (SMBus-attached).
//!
//! The CK505 is a common clock source on Atom D4xx / D5xx / NM10
//! reference boards. It is programmed over SMBus: a variable number
//! of registers select reference and bus clock dividers,
//! spread-spectrum options, and output enables.
//!
//! The part exposes its register file as one SMBus **block** at command 0: the
//! wire transfer contains a count byte followed by registers. The SMBus host
//! handles that count separately: entry 0 in the returned data and board table
//! is register 0, not the count. Like coreboot, program the complete returned
//! register block rather than issuing byte-data transactions.
//!
//! As in coreboot, successful programming means the block write completed.
//! Register bytes may include read-only/status bits, so an exact readback echo
//! is not required. SMBus transaction errors still propagate to the caller.

use fstart_core::services::SmBus;
use fstart_core::services::device::DeviceError;
use heapless::Vec;

/// Configuration for the CK505 clock generator.
///
/// `mask` and `regs` must have the same length — one entry per
/// clock register. Typical clock generators have 5–21 registers;
/// the `Vec<u8, 32>` capacity handles all known parts.
#[derive(Debug, Clone)]
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
    ///
    /// Reads the register file, updates only configured mask bits and writes
    /// the complete file back. A successful SMBus write is not an assurance
    /// that every bit is writable or reads back as written.
    pub fn init_on_smbus<B: SmBus + ?Sized>(&mut self, bus: &mut B) -> Result<(), DeviceError> {
        const BLOCK_CMD: u8 = 0;
        const BLOCK_LEN: usize = 32;

        let configured = self.config.mask.len().min(self.config.regs.len());
        if configured == 0 {
            return Err(DeviceError::MissingResource("ck505: empty register table"));
        }

        let mut block = [0u8; BLOCK_LEN];
        let count = bus
            .block_read(self.addr, BLOCK_CMD, &mut block)
            .map_err(|_| DeviceError::BusError)?;
        if !(configured..=BLOCK_LEN).contains(&count) {
            return Err(DeviceError::BusError);
        }
        fstart_log::info!(
            "i2c-ck505: register file at addr={:#x}: {} bytes, {} configured",
            self.addr,
            count as u32,
            configured as u32
        );
        // The values decide the board's clock tree; log them so a wrong table
        // is visible in the boot log rather than only in a blank screen.
        for (index, chunk) in block[..count].chunks(4).enumerate() {
            let mut word = 0u32;
            for (byte, value) in chunk.iter().enumerate() {
                word |= u32::from(*value) << (8 * byte);
            }
            fstart_log::info!("i2c-ck505: regs[{}..] = {:#010x}", (index * 4) as u32, word);
        }

        let nregs = configured;
        for (idx, register) in block[..nregs].iter_mut().enumerate() {
            let mask = self.config.mask[idx];
            *register = (*register & !mask) | (self.config.regs[idx] & mask);
        }
        // SMBus supplies the wire count; the slice contains only registers.
        bus.block_write(self.addr, BLOCK_CMD, &block[..count])
            .map_err(|_| DeviceError::BusError)?;

        fstart_log::info!("i2c-ck505: {} registers programmed", nregs as u32);
        Ok(())
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::services::ServiceError;

    /// Block mock: register data at command 0, with a separate byte count.
    struct FakeBus {
        regs: [u8; 8],
        count: u8,
        reads: u32,
        writes: u32,
        /// Register-zero bits that retain their old value despite the write.
        read_only: u8,
        fail_read: bool,
        fail_write: bool,
    }

    impl SmBus for FakeBus {
        fn read_byte(&mut self, _addr: u8, _cmd: u8) -> Result<u8, ServiceError> {
            Err(ServiceError::InvalidParam)
        }

        fn write_byte(&mut self, _addr: u8, _cmd: u8, _value: u8) -> Result<(), ServiceError> {
            Err(ServiceError::InvalidParam)
        }

        fn block_read(
            &mut self,
            _addr: u8,
            cmd: u8,
            buf: &mut [u8],
        ) -> Result<usize, ServiceError> {
            assert_eq!(cmd, 0, "the register file is only reachable at command 0");
            assert_eq!(self.writes, 0, "programming must not require a readback");
            self.reads += 1;
            if self.fail_read {
                return Err(ServiceError::IoError);
            }
            let len = (self.count as usize).min(buf.len());
            buf[..len].copy_from_slice(&self.regs[..len]);
            Ok(len)
        }

        fn block_write(&mut self, _addr: u8, cmd: u8, data: &[u8]) -> Result<(), ServiceError> {
            assert_eq!(cmd, 0, "the register file is only reachable at command 0");
            self.writes += 1;
            if self.fail_write {
                return Err(ServiceError::IoError);
            }
            let retained = self.regs[0] & self.read_only;
            self.regs[..data.len()].copy_from_slice(data);
            self.regs[0] = (self.regs[0] & !self.read_only) | retained;
            Ok(())
        }
    }

    fn config(mask: &[u8], regs: &[u8]) -> I2cCk505Config {
        I2cCk505Config {
            mask: heapless::Vec::from_slice(mask).unwrap(),
            regs: heapless::Vec::from_slice(regs).unwrap(),
        }
    }

    fn bus(regs: [u8; 8], count: u8) -> FakeBus {
        FakeBus {
            regs,
            count,
            reads: 0,
            writes: 0,
            read_only: 0,
            fail_read: false,
            fail_write: false,
        }
    }

    #[test]
    fn programs_register_zero_and_preserves_unconfigured_data() {
        let mut bus = bus([0x41, 0xff, 0xff, 0xfe, 0, 0, 0x65, 0x06], 8);
        let mut ck505 = I2cCk505::new_at_address(config(&[0xff], &[0x11]), 0x69)
            .expect("CK505 should accept I2C/SMBus address");

        ck505.init_on_smbus(&mut bus).unwrap();

        assert_eq!(bus.regs, [0x11, 0xff, 0xff, 0xfe, 0, 0, 0x65, 0x06]);
        assert_eq!(bus.count, 8, "register zero is not the transport count");
        assert_eq!(bus.writes, 1);
    }

    #[test]
    fn masks_input_values() {
        let mut bus = bus([0xf0, 0, 0, 0, 0, 0, 0, 0], 8);
        let mut ck505 = I2cCk505::new_at_address(config(&[0x0f], &[0xa5]), 0x69).unwrap();
        ck505.init_on_smbus(&mut bus).unwrap();
        assert_eq!(bus.regs[0], 0xf5);
    }

    #[test]
    fn short_register_file_is_rejected_before_writing() {
        let mut bus = bus([0; 8], 1);
        let mut ck505 =
            I2cCk505::new_at_address(config(&[0xff, 0xff], &[0x11, 0x22]), 0x69).unwrap();
        assert!(ck505.init_on_smbus(&mut bus).is_err());
        assert_eq!(bus.writes, 0);
    }

    #[test]
    fn successful_write_does_not_require_read_only_bits_to_change() {
        let mut bus = bus([0x41, 0xff, 0xff, 0xfe, 0, 0, 0x65, 0x06], 8);
        bus.read_only = 0x40;
        let mut ck505 = I2cCk505::new_at_address(config(&[0xff], &[0x11]), 0x69).unwrap();
        ck505.init_on_smbus(&mut bus).unwrap();
        assert_eq!(bus.regs[0], 0x51);
        assert_eq!(bus.reads, 1);
        assert_eq!(bus.writes, 1);
    }

    #[test]
    fn read_and_write_transaction_failures_are_reported() {
        for fail_read in [true, false] {
            let mut bus = bus([0x41, 0, 0, 0, 0, 0, 0, 0], 8);
            bus.fail_read = fail_read;
            bus.fail_write = !fail_read;
            let mut ck505 = I2cCk505::new_at_address(config(&[0xff], &[0x11]), 0x69).unwrap();
            assert!(matches!(
                ck505.init_on_smbus(&mut bus),
                Err(DeviceError::BusError)
            ));
            assert_eq!(bus.writes, u32::from(!fail_read));
        }
    }

    #[test]
    fn empty_register_table_is_rejected() {
        let mut bus = bus([0x05, 0, 0, 0, 0, 0, 0, 0], 6);
        let mut ck505 = I2cCk505::new_at_address(config(&[], &[]), 0x69)
            .expect("CK505 should accept I2C/SMBus address");

        assert!(ck505.init_on_smbus(&mut bus).is_err());
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
