//! DDR3 SPD transport over SMBus; decoding is `fstart_driver_spd::ddr3`.

use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_spd::ddr3::SPD_SIZE_MAX_DDR3;

/// Read the DDR3 SPD payload from `addr` into a 256-byte scratch buffer.
///
/// Only a device NAK denotes an unpopulated slot, as for DDR2; bus or
/// controller failures are errors.
pub fn read_spd<B: SmBus + ?Sized>(
    smbus: &mut B,
    addr: u8,
) -> Result<Option<[u8; 256]>, ServiceError> {
    let mut spd = [0u8; 256];
    if !super::read_eeprom(smbus, addr, 0, &mut spd[..SPD_SIZE_MAX_DDR3])? {
        return Ok(None);
    }
    if spd.iter().all(|b| *b == 0) {
        return Ok(None);
    }
    Ok(Some(spd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_spd::ddr3::testing::synthetic_ddr3_spd;
    use fstart_driver_spd::ddr3::{DDR3, SPD_MEMORY_TYPE, SPD_PART_NUMBER};

    struct FakeSmBus {
        data: [u8; 256],
        fail_byte0: bool,
    }

    impl SmBus for FakeSmBus {
        fn read_byte(&mut self, _addr: u8, cmd: u8) -> Result<u8, ServiceError> {
            if self.fail_byte0 && cmd == 0 {
                Err(ServiceError::NoDevice)
            } else {
                Ok(self.data[cmd as usize])
            }
        }

        fn write_byte(&mut self, _addr: u8, _cmd: u8, _value: u8) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    #[test]
    fn read_spd_returns_none_for_absent_slot_and_reads_256_bytes() {
        let data = synthetic_ddr3_spd();
        let mut smbus = FakeSmBus {
            data,
            fail_byte0: false,
        };
        let read = read_spd(&mut smbus, 0x50)
            .expect("smbus read succeeds")
            .expect("slot populated");
        assert_eq!(read[SPD_MEMORY_TYPE as usize], DDR3);
        assert_eq!(read[SPD_PART_NUMBER as usize], b'F');

        let mut smbus = FakeSmBus {
            data: [0; 256],
            fail_byte0: true,
        };
        assert!(
            read_spd(&mut smbus, 0x50)
                .expect("absent slot is not fatal")
                .is_none()
        );
    }
}
