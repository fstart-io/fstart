//! IDT CK505 clock generator driver (SMBus-attached).
//!
//! The CK505 is a common clock source on Atom D4xx / D5xx / NM10
//! reference boards. It is programmed over SMBus: a variable number
//! of registers select reference and bus clock dividers,
//! spread-spectrum options, and output enables.
//!
//! The part exposes its register file as one SMBus **block** at command 0: the
//! device returns a count byte followed by its registers, and the same shape
//! is written back. It has no byte-addressable registers — byte reads all
//! return the same junk value and byte writes are ignored — so the driver uses
//! the block transfer coreboot's CK505 driver uses for this board. Board tables
//! are written in that same shape, with entry 0 as the count byte.
//!
//! After writing, the file is read back and compared, so a programming failure
//! is reported instead of silently leaving a wrong clock tree behind.

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
    /// Reads the register file, applies `(old & !mask) | regs` to the
    /// configured entries, writes the file back and verifies it.
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
        if count == 0 {
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

        let nregs = configured.min(count);
        for idx in 0..nregs {
            block[idx] = (block[idx] & !self.config.mask[idx]) | self.config.regs[idx];
        }
        // The device keeps the same length it reported, count byte included.
        bus.block_write(self.addr, BLOCK_CMD, &block[..count])
            .map_err(|_| DeviceError::BusError)?;

        let mut verify = [0u8; BLOCK_LEN];
        let back = bus
            .block_read(self.addr, BLOCK_CMD, &mut verify)
            .map_err(|_| DeviceError::BusError)?;
        if back < nregs || verify[..nregs] != block[..nregs] {
            fstart_log::error!("i2c-ck505: register file did not hold the written values");
            return Err(DeviceError::BusError);
        }
        fstart_log::info!(
            "i2c-ck505: {} registers programmed and verified",
            nregs as u32
        );
        Ok(())
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::services::ServiceError;

    /// Block mock: a register file served at command 0, count byte first.
    struct FakeBus {
        regs: [u8; 8],
        count: u8,
        writes: u32,
        /// Accept the write but keep the old values (silent chip failure).
        drop_writes: bool,
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
            let len = (self.count as usize).min(buf.len());
            buf[..len].copy_from_slice(&self.regs[..len]);
            Ok(len)
        }

        fn block_write(&mut self, _addr: u8, cmd: u8, data: &[u8]) -> Result<(), ServiceError> {
            assert_eq!(cmd, 0, "the register file is only reachable at command 0");
            self.writes += 1;
            if !self.drop_writes {
                self.regs[..data.len()].copy_from_slice(data);
            }
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
            writes: 0,
            drop_writes: false,
        }
    }

    /// Entry 0 is the count byte, so entry 1 is register 0.
    #[test]
    fn programs_the_register_file_in_one_block() {
        let mut bus = bus([0x05, 0xf0, 0x00, 0x11, 0x22, 0, 0, 0], 6);
        let mut ck505 =
            I2cCk505::new_at_address(config(&[0x00, 0x0f, 0xf0], &[0x00, 0x05, 0xa0]), 0x69)
                .expect("CK505 should accept I2C/SMBus address");

        ck505.init_on_smbus(&mut bus).unwrap();

        assert_eq!(bus.regs[0], 0x05, "count byte preserved");
        assert_eq!(bus.regs[1], 0xf5);
        assert_eq!(bus.regs[2], 0xa0);
        assert_eq!(bus.regs[3], 0x11, "unconfigured registers untouched");
        assert_eq!(bus.writes, 1);
    }

    #[test]
    fn init_fails_when_the_device_drops_the_block() {
        let mut bus = bus([0x05, 0x00, 0, 0, 0, 0, 0, 0], 6);
        bus.drop_writes = true;
        let mut ck505 = I2cCk505::new_at_address(config(&[0x00, 0xff], &[0x00, 0x5a]), 0x69)
            .expect("CK505 should accept I2C/SMBus address");

        assert!(ck505.init_on_smbus(&mut bus).is_err());
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
