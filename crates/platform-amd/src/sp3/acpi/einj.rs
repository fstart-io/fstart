//! ACPI 6.5 §18.6 EINJ, backed by the native fixed-buffer SMM service.

extern crate alloc;
use crate::sp3::einj::*;
use alloc::vec::Vec;
use fstart_acpi::einj::{EinjInstruction, EinjTableBody, EinjTriggerHeader};
use fstart_acpi::{Aml, WireGas, sdt::Sdt};
use zerocopy::IntoBytes;
use zerocopy::little_endian::{U32, U64};

/// One serialization instruction (ACPI WHEA header). No preserved bits:
/// each memory GAS refers to a dedicated full-width parameter word.
fn instruction(action: u8, opcode: u8, io: bool, address: u64, value: u64) -> EinjInstruction {
    EinjInstruction {
        action,
        instruction: opcode,
        flags: 0,
        reserved: 0,
        register: WireGas::new(
            u8::from(io),
            if io { 8 } else { 64 },
            0,
            if io { 1 } else { 4 },
            address,
        ),
        value: U64::new(value),
        mask: U64::new(if io { 0xff } else { u64::MAX }),
    }
}

pub fn table() -> Vec<u8> {
    let mut sdt = Sdt::new(*b"EINJ", 48, 1, *b"FSTART", *b"SP3 EINJ", 1);
    sdt.write_bytes(
        36,
        EinjTableBody {
            header_size: U32::new(48),
            flags: 0,
            reserved: [0; 3],
            entry_count: U32::new(9),
        }
        .as_bytes(),
    );
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
        sdt.append_slice(entry.as_bytes());
    }
    let mut bytes = Vec::new();
    sdt.to_aml_bytes(&mut bytes);
    bytes
}

/// Single READ_REGISTER from disposable scratch, just as the donor uses
/// for correctable memory errors. The SMM handler patches the selected
/// line before returning EXECUTE_OPERATION.
pub fn trigger(address: u64) -> [u8; 48] {
    #[repr(C)]
    #[derive(Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
    struct SingleTrigger {
        header: EinjTriggerHeader,
        entry: EinjInstruction,
    }
    let wire = SingleTrigger {
        header: EinjTriggerHeader {
            header_size: U32::new(16),
            revision: U32::ZERO,
            table_size: U32::new(48),
            entry_count: U32::new(1),
        },
        entry: instruction(0xff, 0, false, address, 0),
    };
    let mut bytes = [0; 48];
    bytes.copy_from_slice(wire.as_bytes());
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
