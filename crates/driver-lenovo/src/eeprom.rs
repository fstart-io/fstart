//! ThinkPad DMI fields in the AT24RF08C EEPROM, matching coreboot's
//! `drivers/i2c/at24rf08c/lenovo_serials.c`. The board owns SMBus routing.
//! Reading identity never changes EEPROM contents or its protection registers.

use fstart_core::services::{ServiceError, SmBus};

/// Validated, boot-lifetime baseboard identity. UUID bytes are already in
/// SMBIOS wire order (little-endian first three components).
#[derive(Debug)]
pub struct Identity {
    part: [u8; 7],
    serial: [u8; 7],
    version: [u8; 89],
    version_len: usize,
    pub uuid: [u8; 16],
}

impl Identity {
    /// Read the Lenovo fields, failing rather than publishing invalid strings.
    pub fn read(bus: &mut (impl SmBus + ?Sized)) -> Result<Self, ServiceError> {
        let mut identity = Self {
            part: [0; 7],
            serial: [0; 7],
            version: [0; 89],
            version_len: 0,
            uuid: [0; 16],
        };
        read_string(bus, 0x54, 0x27, &mut identity.part)?;
        read_string(bus, 0x54, 0x2e, &mut identity.serial)?;
        let length = bus.read_byte(0x56, 0x26)?;
        identity.version_len =
            usize::from(length.checked_sub(2).ok_or(ServiceError::InvalidParam)?);
        if identity.version_len > identity.version.len() {
            return Err(ServiceError::InvalidParam);
        }
        read_string(
            bus,
            0x56,
            0x27,
            &mut identity.version[..identity.version_len],
        )?;
        let mut uuid = [0; 16];
        for (index, byte) in uuid.iter_mut().enumerate() {
            *byte = bus.read_byte(0x56, 0x12 + index as u8)?;
        }
        // SMBIOS 2.6+ stores the first 4/2/2-byte UUID components little-endian.
        identity.uuid =
            [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15].map(|index| uuid[index]);
        Ok(identity)
    }

    pub fn part_number(&self) -> &str {
        core::str::from_utf8(&self.part).expect("validated EEPROM ASCII")
    }

    pub fn serial_number(&self) -> &str {
        core::str::from_utf8(&self.serial).expect("validated EEPROM ASCII")
    }

    pub fn version(&self) -> &str {
        core::str::from_utf8(&self.version[..self.version_len]).expect("validated EEPROM ASCII")
    }
}

fn read_string(
    bus: &mut (impl SmBus + ?Sized),
    address: u8,
    start: u8,
    buffer: &mut [u8],
) -> Result<(), ServiceError> {
    if usize::from(start) + buffer.len() > 128 {
        return Err(ServiceError::InvalidParam);
    }
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = bus.read_byte(address, start + index as u8)?;
        if !(0x20..=0x7e).contains(byte) {
            return Err(ServiceError::InvalidParam);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Eeprom {
        banks: [[u8; 128]; 3],
        fail: Option<(u8, u8)>,
    }

    impl SmBus for Eeprom {
        fn read_byte(&mut self, address: u8, command: u8) -> Result<u8, ServiceError> {
            if self.fail == Some((address, command)) {
                return Err(ServiceError::IoError);
            }
            Ok(self.banks[usize::from(address - 0x54)][usize::from(command)])
        }
        fn write_byte(&mut self, _: u8, _: u8, _: u8) -> Result<(), ServiceError> {
            panic!("identity reads must never write EEPROM")
        }
    }

    fn eeprom() -> Eeprom {
        let mut banks = [[0; 128]; 3];
        banks[0][0x27..0x2e].copy_from_slice(b"42W7651");
        banks[0][0x2e..0x35].copy_from_slice(b"L3ABCDE");
        banks[2][0x12..0x22].copy_from_slice(&core::array::from_fn::<_, 16, _>(|i| i as u8));
        banks[2][0x26] = 14;
        banks[2][0x27..0x33].copy_from_slice(b"ThinkPad X61");
        Eeprom { banks, fail: None }
    }

    #[test]
    fn reads_coreboot_offsets_and_smbios_uuid_order() {
        let identity = Identity::read(&mut eeprom()).unwrap();
        assert_eq!(identity.part_number(), "42W7651");
        assert_eq!(identity.serial_number(), "L3ABCDE");
        assert_eq!(identity.version(), "ThinkPad X61");
        assert_eq!(
            identity.uuid,
            [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15]
        );
    }

    #[test]
    fn rejects_bad_lengths_strings_and_transport_failures() {
        for length in [0, 1, 92, 255] {
            let mut bus = eeprom();
            bus.banks[2][0x26] = length;
            assert!(Identity::read(&mut bus).is_err());
        }
        let mut bus = eeprom();
        bus.banks[0][0x2e] = 0xff;
        assert!(Identity::read(&mut bus).is_err());
        let mut bus = eeprom();
        bus.fail = Some((0x56, 0x12));
        assert!(matches!(
            Identity::read(&mut bus),
            Err(ServiceError::IoError)
        ));
    }
}
