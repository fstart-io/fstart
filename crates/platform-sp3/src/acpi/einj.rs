//! ACPI 6.5 §18.6 EINJ, backed by the native fixed-buffer SMM service.

extern crate alloc;
use crate::einj::*;
use alloc::vec::Vec;
use fstart_acpi::{Aml, sdt::Sdt};

/// One serialization instruction (ACPI WHEA header). No preserved bits:
/// each memory GAS refers to a dedicated full-width parameter word.
fn instruction(action: u8, opcode: u8, io: bool, address: u64, value: u64) -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[0] = action;
    bytes[1] = opcode;
    bytes[4] = u8::from(io);
    bytes[5] = if io { 8 } else { 64 };
    bytes[7] = if io { 1 } else { 4 };
    bytes[8..16].copy_from_slice(&address.to_le_bytes());
    bytes[16..24].copy_from_slice(&value.to_le_bytes());
    bytes[24..32].copy_from_slice(&(if io { 0xffu64 } else { u64::MAX }).to_le_bytes());
    bytes
}

pub fn table() -> Vec<u8> {
    let mut sdt = Sdt::new(*b"EINJ", 48, 1, *b"FSTART", *b"SP3 EINJ", 1);
    sdt.write_u32(36, 48);
    sdt.write_u32(44, 9);
    for entry in [
        instruction(0, 3, true, 0xb2, SW_BEGIN.into()),
        instruction(1, 0, false, BASE + TRIGGER_POINTER, 0),
        instruction(2, 2, false, BASE + LEGACY_TYPE, 0),
        instruction(3, 0, false, BASE + SUPPORTED, 0),
        instruction(4, 3, true, 0xb2, SW_END.into()),
        instruction(5, 3, true, 0xb2, SW_EXECUTE.into()),
        instruction(6, 0, false, BASE + BUSY, 0),
        instruction(7, 0, false, BASE + STATUS, 0),
        // Linux discovers this GAS and writes the ACPI 5+ parameter
        // structure directly; no firmware pointer is read from shared RAM.
        instruction(8, 2, false, BASE + PARAMETERS, 0),
    ] {
        sdt.append_slice(&entry);
    }
    let mut bytes = Vec::new();
    sdt.to_aml_bytes(&mut bytes);
    bytes
}

/// Single READ_REGISTER from disposable scratch, just as the donor uses
/// for correctable memory errors. The SMM handler patches the selected
/// line before returning EXECUTE_OPERATION.
pub fn trigger(address: u64) -> [u8; 48] {
    let mut bytes = [0; 48];
    bytes[..4].copy_from_slice(&16u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&48u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&1u32.to_le_bytes());
    bytes[16..].copy_from_slice(&instruction(0xff, 0, false, address, 0));
    bytes
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
        assert_eq!(u32::from_le_bytes(t[36..40].try_into().unwrap()), 48);
        assert_eq!(u32::from_le_bytes(t[44..48].try_into().unwrap()), 9);
        assert_eq!(&t[48..56], &[0, 3, 0, 0, 1, 8, 0, 1]);
        let tr = trigger(SCRATCH);
        assert_eq!(&tr[16..24], &[0xff, 0, 0, 0, 0, 64, 0, 4]);
        assert_eq!(&tr[24..32], &SCRATCH.to_le_bytes());
    }
}
