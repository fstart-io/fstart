//! EINJ (Error Injection Table), ACPI 6.5 §18.6, and its trigger action
//! table: serialization instructions over generic address registers.

use acpi_tables::gas::{AccessSize, AddressSpace, GAS};
use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use zerocopy::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::WireGas;

/// Wire layout of one EINJ serialization instruction entry (32 bytes).
#[repr(C)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
pub struct EinjInstruction {
    pub action: u8,
    pub instruction: u8,
    pub flags: u8,
    pub reserved: u8,
    pub register: WireGas,
    pub value: U64,
    pub mask: U64,
}

/// Wire layout of the 12-byte EINJ injection header after the 36-byte SDT header.
#[repr(C)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
pub struct EinjTableBody {
    pub header_size: U32,
    pub flags: u8,
    pub reserved: [u8; 3],
    pub entry_count: U32,
}

/// Wire layout of the 16-byte EINJ trigger action table header.
#[repr(C)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
pub struct EinjTriggerHeader {
    pub header_size: U32,
    pub revision: U32,
    pub table_size: U32,
    pub entry_count: U32,
}

const _: () = assert!(core::mem::size_of::<EinjInstruction>() == 32);
const _: () = assert!(core::mem::size_of::<EinjTableBody>() == 12);
const _: () = assert!(core::mem::size_of::<EinjTriggerHeader>() == 16);

/// Injection actions (ACPI 6.5 Table 18.25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Action {
    BeginOperation = 0,
    GetTriggerActionTable = 1,
    SetErrorType = 2,
    GetErrorType = 3,
    EndOperation = 4,
    ExecuteOperation = 5,
    CheckBusyStatus = 6,
    GetCommandStatus = 7,
    SetErrorTypeWithAddress = 8,
    GetExecuteOperationTimings = 9,
    /// Only in a trigger action table.
    TriggerError = 0xff,
}

/// Serialization instructions (ACPI 6.5 Table 18.26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Instruction {
    ReadRegister = 0,
    ReadRegisterValue = 1,
    WriteRegister = 2,
    WriteRegisterValue = 3,
    Noop = 4,
}

/// A register an instruction reads or writes.
#[derive(Debug, Clone, Copy)]
pub struct Register {
    pub space: AddressSpace,
    pub address: u64,
    /// 8, 16, 32 or 64.
    pub bits: u8,
    pub offset: u8,
}

impl Register {
    /// `bits` wide at `address` in system memory.
    pub const fn memory(address: u64, bits: u8) -> Self {
        Self {
            space: AddressSpace::SystemMemory,
            address,
            bits,
            offset: 0,
        }
    }
    /// `bits` wide at `port` in system I/O.
    pub const fn io(port: u16, bits: u8) -> Self {
        Self {
            space: AddressSpace::SystemIo,
            address: port as u64,
            bits,
            offset: 0,
        }
    }
    /// The bit field `offset..offset + bits` of the register.
    pub const fn field(mut self, offset: u8, bits: u8) -> Self {
        self.offset = offset;
        self.bits = bits;
        self
    }
}

/// One injection instruction entry (32 bytes).
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub action: Action,
    pub instruction: Instruction,
    /// Bit 0: preserve the register's other bits.
    pub flags: u8,
    pub register: Register,
    pub value: u64,
    pub mask: u64,
    /// The access width, which may be wider than a field.
    pub access: AccessSize,
}

impl Entry {
    /// `instruction` on a full-width `register` (access = its width).
    pub const fn new(
        action: Action,
        instruction: Instruction,
        register: Register,
        value: u64,
        mask: u64,
    ) -> Self {
        let access = match register.bits {
            8 => AccessSize::ByteAccess,
            16 => AccessSize::WordAccess,
            64 => AccessSize::QwordAccess,
            _ => AccessSize::DwordAccess,
        };
        Self {
            action,
            instruction,
            flags: 0,
            register,
            value,
            mask,
            access,
        }
    }
    pub const fn with_access(mut self, access: AccessSize) -> Self {
        self.access = access;
        self
    }
    pub const fn preserve(mut self) -> Self {
        self.flags |= 1;
        self
    }

    pub fn wire(&self) -> EinjInstruction {
        let gas = GAS::new(
            self.register.space,
            self.register.bits,
            self.register.offset,
            self.access,
            self.register.address,
        );
        EinjInstruction {
            action: self.action as u8,
            instruction: self.instruction as u8,
            flags: self.flags,
            reserved: 0,
            register: WireGas::from(&gas),
            value: U64::new(self.value),
            mask: U64::new(self.mask),
        }
    }

    pub fn bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(self.wire().as_bytes());
        out
    }
}

const HEADER: u32 = 48;

/// The EINJ table: header, injection header and `entries`.
pub fn table(
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    entries: &[Entry],
) -> Vec<u8> {
    let mut sdt = Sdt::new(*b"EINJ", HEADER, 1, oem_id, oem_table_id, oem_revision);
    // Injection header size (from the start of the table to the entries'
    // count), flags and the entry count.
    sdt.write_bytes(
        36,
        EinjTableBody {
            header_size: U32::new(core::mem::size_of::<EinjTableBody>() as u32),
            flags: 0,
            reserved: [0; 3],
            entry_count: U32::new(entries.len() as u32),
        }
        .as_bytes(),
    );
    for entry in entries {
        sdt.append_slice(entry.wire().as_bytes());
    }
    sdt.update_checksum();
    let mut bytes = Vec::new();
    crate::Aml::to_aml_bytes(&sdt, &mut bytes);
    bytes
}

/// The trigger action table an OS gets from GET_TRIGGER_ACTION_TABLE.
pub fn trigger(entries: &[Entry]) -> Vec<u8> {
    let size = core::mem::size_of::<EinjTriggerHeader>()
        + core::mem::size_of::<EinjInstruction>() * entries.len();
    let mut bytes = Vec::with_capacity(size);
    bytes.extend_from_slice(
        EinjTriggerHeader {
            header_size: U32::new(core::mem::size_of::<EinjTriggerHeader>() as u32),
            revision: U32::ZERO,
            table_size: U32::new(size as u32),
            entry_count: U32::new(entries.len() as u32),
        }
        .as_bytes(),
    );
    for entry in entries {
        bytes.extend_from_slice(entry.wire().as_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_tables_and_checksum() {
        let entry = Entry::new(
            Action::GetCommandStatus,
            Instruction::ReadRegister,
            Register::memory(0x1000, 32).field(1, 31),
            0,
            3,
        )
        .with_access(AccessSize::DwordAccess);
        assert_eq!(
            entry.bytes()[..8],
            [7, 0, 0, 0, 0, 31, 1, AccessSize::DwordAccess as u8]
        );
        let t = table(*b"FSTART", *b"TESTEINJ", 1, &[entry, entry]);
        assert_eq!(&t[..4], b"EINJ");
        assert_eq!(t.len(), 48 + 64);
        assert_eq!(u32::from_le_bytes(t[36..40].try_into().unwrap()), 12);
        assert_eq!(u32::from_le_bytes(t[44..48].try_into().unwrap()), 2);
        assert_eq!(t.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
        let trigger = trigger(&[Entry::new(
            Action::TriggerError,
            Instruction::WriteRegisterValue,
            Register::memory(0x2028, 32),
            1,
            0xffff_ffff,
        )
        .preserve()]);
        assert_eq!(trigger.len(), 0x30);
        assert_eq!(&trigger[16..20], &[0xff, 3, 1, 0]);
    }
}
