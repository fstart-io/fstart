// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vilboz's EC fact acquisition. All command transport lives in the shared
//! ChromeEC driver; board failure policy retains original CBI fallbacks.
use crate::policy::Cbi;
use fstart_driver_chromeec::{CbiTag, Clock, Ec, Error, Io};

pub fn read_cbi(
    ec: &mut Ec<impl Io>,
    clock: &mut impl Clock,
    diagnostic: &mut impl FnMut(CbiTag, Error),
) -> Cbi {
    let mut read = |tag| {
        ec.cbi_u32(tag, clock)
            .inspect_err(|&error| diagnostic(tag, error))
            .ok()
    };
    let revision = read(CbiTag::BoardVersion);
    // Zork INCLUDE_SSFC_IN_FW_CONFIG puts the independent SSFC word in the
    // upper half. This is not a single 64-bit CBI read. Missing SSFC leaves
    // valid lower config intact; lower-word failure invalidates the config.
    let firmware_config = read(CbiTag::FirmwareConfig)
        .map(|low| u64::from(low) | u64::from(read(CbiTag::Ssfc).unwrap_or(0)) << 32);
    Cbi::new(revision, firmware_config)
}

/// Read the legacy board ID separately from schematic CBI tag0.
pub fn read_legacy_board_version(
    ec: &mut Ec<impl Io>,
    clock: &mut impl Clock,
    diagnostic: &mut impl FnMut(Error),
) -> Option<u16> {
    ec.board_version(clock)
        .inspect_err(|&error| diagnostic(error))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Timer;
    impl Clock for Timer {
        fn now_us(&mut self) -> u64 {
            0
        }
        fn delay_us(&mut self, _: u32) {}
    }
    struct Ports {
        packet: [u8; 256],
        tag: u32,
        fail_ssfc: bool,
        command: u16,
        legacy_version: Option<u16>,
        revision: u32,
        config: u32,
    }
    impl Ports {
        fn new(fail_ssfc: bool) -> Self {
            Self {
                packet: [0; 256],
                tag: 0,
                fail_ssfc,
                command: 0,
                legacy_version: Some(0),
                revision: 0,
                config: 0,
            }
        }
    }
    impl Io for Ports {
        fn read8(&mut self, port: u16) -> u8 {
            match port {
                0x204 => 0,
                0x200 => {
                    u8::from(if self.command == 6 {
                        self.legacy_version.is_none()
                    } else {
                        self.fail_ssfc && self.tag == 8
                    }) * 3
                }
                0x920 => b'E',
                0x921 => b'C',
                0x927 => 2,
                0x800..=0x8ff => self.packet[usize::from(port - 0x800)],
                _ => 0xff,
            }
        }
        fn write8(&mut self, port: u16, value: u8) {
            if (0x800..=0x8ff).contains(&port) {
                self.packet[usize::from(port - 0x800)] = value;
            }
            if port == 0x204 {
                self.command = u16::from_le_bytes(self.packet[2..4].try_into().unwrap());
                self.tag = u32::from_le_bytes(self.packet[8..12].try_into().unwrap());
                self.packet.fill(0);
                self.packet[0] = 3;
                let length = if self.command == 6 { 2 } else { 4 };
                self.packet[4] = length;
                let data = if self.command == 6 {
                    u32::from(self.legacy_version.unwrap_or(0))
                } else {
                    match self.tag {
                        0 => self.revision,
                        6 => self.config,
                        8 => 8,
                        _ => 0,
                    }
                }; // SSFC bit3 is amp-auto
                self.packet[8..8 + usize::from(length)]
                    .copy_from_slice(&data.to_le_bytes()[..usize::from(length)]);
                let sum = self.packet[..8 + usize::from(length)]
                    .iter()
                    .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
                self.packet[1] = sum.wrapping_neg();
            }
        }
    }
    #[test]
    fn legacy_board_id_is_independent_of_cbi_revision_and_unknown_is_suppressed() {
        for (revision, legacy_version, quirk) in [
            (3, None, false),
            (3, Some(1), false),
            (3, Some(2), false),
            (1, Some(3), true),
        ] {
            let mut ports = Ports::new(false);
            ports.revision = revision;
            ports.legacy_version = legacy_version;
            ports.config = 1 << 29;
            let mut ec = Ec::discover(ports).unwrap();
            let cbi = read_cbi(&mut ec, &mut Timer, &mut |_, _| {
                panic!("unexpected CBI failure")
            });
            let mut errors = 0;
            let board_id = read_legacy_board_version(&mut ec, &mut Timer, &mut |_| errors += 1);
            assert_eq!(cbi.board_version, Some(revision));
            assert_eq!(board_id, legacy_version);
            assert_eq!(cbi.active_low_wifi_power(), revision >= 2);
            assert_eq!(cbi.external_audio_clock(board_id), quirk);
            assert_eq!(errors, usize::from(legacy_version.is_none()));
        }
    }
    #[test]
    fn fw_config_combines_ssfc_and_preserves_valid_zero_and_optional_failure() {
        let mut ec = Ec::discover(Ports::new(false)).unwrap();
        let cbi = read_cbi(&mut ec, &mut Timer, &mut |_, _| {
            panic!("unexpected CBI error")
        });
        assert_eq!(cbi.board_version, Some(0));
        assert_eq!(cbi.firmware_config, Some(8u64 << 32));
        assert!(cbi.audio_amp_auto());
        let mut ec = Ec::discover(Ports::new(true)).unwrap();
        let mut errors = 0;
        let cbi = read_cbi(&mut ec, &mut Timer, &mut |tag, error| {
            assert!(matches!(tag, CbiTag::Ssfc));
            assert_eq!(error, Error::Rejected(3));
            errors += 1;
        });
        assert_eq!(cbi.board_version, Some(0));
        assert_eq!(cbi.firmware_config, Some(0));
        assert_eq!(errors, 1);
    }
}
