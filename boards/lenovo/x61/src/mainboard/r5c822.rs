//! R5C822 SD policy used by the X61, not the different RCE822/RCE823 register map.
//!
//! Keep the PCI-byte erratum sequence explicit; a config overlay RMW would
//! preserve controls that the board's exact policy deliberately clears.

use fstart_core::services::ServiceError;
use fstart_pci::{EcamDevice, PCI_DEVICE_ID, PCI_VENDOR_ID};
use tock_registers::register_bitfields;

const RICOH_VENDOR: u16 = 0x1180;
const R5C822_DEVICE: u16 = 0x0822;
const WRITE_PROTECT_KEY: u16 = 0xf9;
const SD_CONTROL: u16 = 0xfa;
const KEY_UNLOCK: u8 = 0xfc;
const KEY_LOCK: u8 = 0;

register_bitfields![u8,
    SD_CONTROL_FIELDS [SD_WRITE_PROTECT_POLARITY OFFSET(5) NUMBITS(1) []]
];

/// Program the board-selected SDWPPol value, keeping CLKRUNDis/SDPWRPol clear.
/// Identity verification must precede any vendor-specific register access.
pub(super) fn configure_sd_write_protect(
    sd: EcamDevice,
    sdwp_pol: bool,
) -> Result<(), ServiceError> {
    let vendor = sd
        .try_read16(PCI_VENDOR_ID)
        .ok_or(ServiceError::HardwareError)?;
    let device = sd
        .try_read16(PCI_DEVICE_ID)
        .ok_or(ServiceError::HardwareError)?;
    program_sd_policy(
        (vendor, device),
        sdwp_pol,
        |reg| sd.try_read8(reg),
        |reg, value| sd.try_write8(reg, value),
    )
}

fn program_sd_policy(
    identity: (u16, u16),
    sdwp_pol: bool,
    mut read: impl FnMut(u16) -> Option<u8>,
    mut write: impl FnMut(u16, u8) -> Option<()>,
) -> Result<(), ServiceError> {
    if identity != (RICOH_VENDOR, R5C822_DEVICE) {
        return Err(ServiceError::HardwareError);
    }
    // Exact policy byte, not an RMW: all other controls remain clear.
    let policy = SD_CONTROL_FIELDS::SD_WRITE_PROTECT_POLARITY
        .val(sdwp_pol.into())
        .value;
    if read(SD_CONTROL).ok_or(ServiceError::HardwareError)? == policy {
        return Ok(());
    }
    write(WRITE_PROTECT_KEY, KEY_UNLOCK).ok_or(ServiceError::HardwareError)?;
    let programmed = write(SD_CONTROL, policy);
    // Explicitly restore protection even if the policy write failed.
    let locked = write(WRITE_PROTECT_KEY, KEY_LOCK);
    programmed.and(locked).ok_or(ServiceError::HardwareError)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use core::cell::RefCell;
    use std::vec::Vec;

    #[test]
    fn sd_policy_uses_exact_byte_transactions_and_skips_matching_state() {
        let accesses = RefCell::new(Vec::new());
        program_sd_policy(
            (0x1180, 0x0822),
            true,
            |reg| {
                accesses.borrow_mut().push((false, reg, 0xff));
                Some(0xff)
            },
            |reg, value| {
                accesses.borrow_mut().push((true, reg, value));
                Some(())
            },
        )
        .unwrap();
        assert_eq!(
            accesses.into_inner(),
            [
                (false, 0xfa, 0xff),
                (true, 0xf9, 0xfc),
                (true, 0xfa, 0x20),
                (true, 0xf9, 0)
            ]
        );

        for (polarity, current) in [(true, 0x20), (false, 0)] {
            program_sd_policy(
                (0x1180, 0x0822),
                polarity,
                |_| Some(current),
                |_, _| panic!("matching SD policy must not unlock or write"),
            )
            .unwrap();
        }
    }

    #[test]
    fn other_ricoh_variants_and_absent_functions_are_not_programmed() {
        for identity in [(0x1180, 0xe822), (0x1180, 0xe823), (0xffff, 0xffff)] {
            assert!(
                program_sd_policy(
                    identity,
                    true,
                    |_| panic!("wrong identity must not read vendor registers"),
                    |_, _| panic!("wrong identity must not write vendor registers"),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn access_failures_are_reported_and_failed_programming_still_relocks() {
        assert!(
            program_sd_policy(
                (0x1180, 0x0822),
                true,
                |_| None,
                |_, _| panic!("failed read must not unlock"),
            )
            .is_err()
        );

        for failed_register in [0xfa, 0xf9] {
            let mut writes = Vec::new();
            let result = program_sd_policy(
                (0x1180, 0x0822),
                true,
                |_| Some(0),
                |reg, value| {
                    writes.push((reg, value));
                    // Fail either programming or the final relock, not unlock.
                    (reg != failed_register || value == KEY_UNLOCK).then_some(())
                },
            );
            assert!(result.is_err());
            assert_eq!(writes, [(0xf9, 0xfc), (0xfa, 0x20), (0xf9, 0)]);
        }
    }
}
