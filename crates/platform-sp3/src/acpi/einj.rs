//! ACPI 6.5 §18.6 EINJ, backed by the native fixed-buffer SMM service.

extern crate alloc;
use crate::einj::*;
use alloc::vec::Vec;
use fstart_acpi::einj::{self, Action, Entry, Instruction, Register};

/// The SMI command port, written a byte at a time.
const SMI_CMD: Register = Register::io(0xb2, 8);

/// No preserved bits: each memory register is a dedicated full-width
/// parameter word.
const fn word(offset: u64) -> Register {
    Register::memory(BASE + offset, 64)
}

const fn smi(action: Action, command: u8) -> Entry {
    Entry::new(
        action,
        Instruction::WriteRegisterValue,
        SMI_CMD,
        command as u64,
        0xff,
    )
}

const fn read(action: Action, register: Register) -> Entry {
    Entry::new(action, Instruction::ReadRegister, register, 0, u64::MAX)
}

const fn write(action: Action, register: Register) -> Entry {
    Entry::new(action, Instruction::WriteRegister, register, 0, u64::MAX)
}

pub fn table() -> Vec<u8> {
    einj::table(
        *b"FSTART",
        *b"SP3 EINJ",
        1,
        &[
            smi(Action::BeginOperation, SW_BEGIN),
            read(Action::GetTriggerActionTable, word(TRIGGER_POINTER)),
            write(Action::SetErrorType, word(LEGACY_TYPE)),
            read(Action::GetErrorType, word(SUPPORTED)),
            smi(Action::EndOperation, SW_END),
            smi(Action::ExecuteOperation, SW_EXECUTE),
            read(Action::CheckBusyStatus, word(BUSY)),
            read(Action::GetCommandStatus, word(STATUS)),
            // Linux discovers this register and writes the ACPI 5+ parameter
            // structure directly; no firmware pointer is read from shared RAM.
            write(Action::SetErrorTypeWithAddress, word(PARAMETERS)),
        ],
    )
}

/// Single READ_REGISTER from disposable scratch, just as the donor uses
/// for correctable memory errors. The SMM handler patches the selected
/// line (the entry's address, at byte 24) before returning
/// EXECUTE_OPERATION.
pub fn trigger(address: u64) -> Vec<u8> {
    einj::trigger(&[read(Action::TriggerError, Register::memory(address, 64))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_and_trigger_layout() {
        let t = table();
        assert_eq!(t.len(), 48 + 9 * 32);
        assert_eq!(&t[..4], b"EINJ");
        assert_eq!(t.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
        assert_eq!(u32::from_le_bytes(t[44..48].try_into().unwrap()), 9);
        assert_eq!(&t[48..56], &[0, 3, 0, 0, 1, 8, 0, 1]);
        let tr = trigger(SCRATCH);
        assert_eq!(tr.len(), 48);
        assert!(tr.len() as u64 <= BACKEND_STATUS - TRIGGER);
        assert_eq!(&tr[16..24], &[0xff, 0, 0, 0, 0, 64, 0, 4]);
        assert_eq!(&tr[24..32], &SCRATCH.to_le_bytes());
    }
}
