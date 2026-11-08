// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Rs (Genoa, Siena) PSP's ACPI error-injection service.
//!
//! The PSP itself injects the errors an OS asks for through ACPI EINJ: the
//! table's actions read and write the PSP's command buffer (the ABL's
//! `ReservedRasEinj` APOB hole) and EXECUTE_OPERATION rings `MP0_C2PMSG_31`.
//! The x86 side asks the same register for the buffer's address, 16 bits at
//! a time (AmdPlatformRasRsDxe `ExecutePspRasEinjCmd`/`ApeiEinjInstall`,
//! S8040 V2.00 and S4056 V241). The PSP serves injections only after SMM
//! sent C2P RasEinj ENABLE ([`super::command::RasEinj`]).

use core::ops::Range;
use fstart_core::services::timer::Timer;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::registers::{ReadOnly, ReadWrite};
use tock_registers::{LocalRegisterCopy, register_bitfields, register_structs};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

register_bitfields![u32,
    /// `MP0_C2PMSG_31`, the RAS mailbox.
    pub RAS_MBOX [
        DATA OFFSET(0) NUMBITS(16) [],
        COMMAND OFFSET(16) NUMBITS(8) [],
        /// The request's result, with `DONE`.
        STATUS OFFSET(24) NUMBITS(4) [],
        /// Kept across requests.
        PRESERVED OFFSET(28) NUMBITS(3) [],
        /// The PSP finished the request; written 0 with each request.
        DONE OFFSET(31) NUMBITS(1) []
    ]
];

/// `MP0_C2PMSG_31` in the PSP BAR.
pub const MAILBOX_OFFSET: u64 = 0x1_057c;
/// EXECUTE_OPERATION's write: command 4, data 83h (the vendor template).
pub const EXECUTE: u32 = 0x0004_0083;
/// Commands 0..=3 return bits `16n..16n+16` of the buffer's address.
const ADDRESS_WORDS: u8 = 4;
const TIMEOUT_US: u64 = 100_000;
const POLL_US: u64 = 10;

register_bitfields![u32,
    /// GET_ERROR_TYPE's answer: ACPI EINJ error-type bits.
    pub ERROR_TYPES [
        /// The vendor-defined types of [`VendorExtension`] are supported.
        VENDOR OFFSET(31) NUMBITS(1) []
    ],
    /// The command word BEGIN_OPERATION and the status actions use.
    pub COMMAND [
        BUSY OFFSET(0) NUMBITS(1) [],
        STATUS OFFSET(1) NUMBITS(31) []
    ],
    /// ACPI's PCIe SBDF encoding (vendor error type extension).
    pub SBDF [
        FUNCTION OFFSET(8) NUMBITS(3) [],
        DEVICE OFFSET(11) NUMBITS(5) [],
        BUS OFFSET(16) NUMBITS(8) [],
        SEGMENT OFFSET(24) NUMBITS(8) []
    ]
];

register_structs! {
    /// The command buffer as the vendor's EINJ table addresses it, in DRAM
    /// (the ABL's `ReservedRasEinj` hole).
    pub Buffer {
        /// GET_ERROR_TYPE.
        (0x000 => pub error_types: ReadOnly<u32, ERROR_TYPES::Register>),
        (0x004 => _reserved0),
        /// SET_ERROR_TYPE.
        (0x008 => pub error_type: ReadWrite<u32>),
        (0x00c => _reserved1),
        /// BEGIN writes 1; CHECK_BUSY_STATUS and GET_COMMAND_STATUS read it.
        (0x010 => pub command: ReadWrite<u32, COMMAND::Register>),
        (0x014 => _reserved2),
        /// GET_TRIGGER_ACTION_TABLE: the trigger table's address.
        (0x020 => pub trigger_pointer: ReadWrite<u64>),
        /// The trigger action writes 1 here.
        (0x028 => pub trigger_doorbell: ReadWrite<u32>),
        /// END_OPERATION writes 1.
        (0x02c => pub end: ReadWrite<u32>),
        /// SET_ERROR_TYPE_WITH_ADDRESS: the OS's parameter block.
        (0x030 => pub parameters: Parameters),
        /// The vendor-type extension, right after the parameters.
        (0x054 => pub vendor_extension: VendorExtension),
        (0x080 => pub trigger_table: [ReadWrite<u8>; 0x80]),
        /// GET_EXECUTE_OPERATION_TIMINGS.
        (0x100 => pub timings: ReadOnly<u64>),
        (0x108 => @END),
    },

    /// ACPI's SET_ERROR_TYPE_WITH_ADDRESS data structure. Its 64-bit fields
    /// are dword pairs: the structure, as placed, is only dword aligned.
    pub Parameters {
        (0x00 => pub error_type: ReadWrite<u32>),
        /// From this structure's start; 0 without a vendor extension.
        (0x04 => pub vendor_extension_offset: ReadWrite<u32>),
        (0x08 => pub flags: ReadWrite<u32>),
        (0x0c => pub processor: ReadWrite<u32>),
        (0x10 => pub memory_address: [ReadWrite<u32>; 2]),
        (0x18 => pub memory_mask: [ReadWrite<u32>; 2]),
        (0x20 => pub pcie_sbdf: ReadWrite<u32, SBDF::Register>),
        (0x24 => @END),
    },

    /// ACPI's vendor error type extension structure, as the vendor fills it.
    pub VendorExtension {
        (0x00 => pub length: ReadWrite<u32>),
        (0x04 => pub sbdf: ReadWrite<u32, SBDF::Register>),
        /// Vendor ID (low half) and device ID, as config dword 0 reads.
        (0x08 => pub id: ReadWrite<u32>),
        /// Revision ID; the rest is reserved.
        (0x0c => pub revision: ReadWrite<u32>),
        /// OEM qword the PSP owns; firmware leaves it alone.
        (0x10 => pub psp: [ReadWrite<u32>; 2]),
        (0x18 => pub oem: [ReadWrite<u32>; 5]),
        (0x2c => @END),
    }
}

impl Buffer {
    /// The command buffer at `address`.
    ///
    /// # Safety
    /// `address` is the PSP's EINJ command buffer (see [`Mailbox::buffer`]),
    /// identity mapped, and the caller is its only user until the OS runs.
    pub unsafe fn at(address: u64) -> &'static Self {
        // SAFETY: the caller's contract; the hole is page aligned.
        unsafe { &*(address as *const Self) }
    }

    /// Lay the buffer out as the vendor's `ApeiEinjInstall` leaves it: no
    /// command pending, `trigger` (the trigger action table) in place and
    /// pointed to, the parameters cleared. Returns the supported types.
    pub fn prepare(
        &self,
        trigger: &[u8],
    ) -> Result<LocalRegisterCopy<u32, ERROR_TYPES::Register>, Error> {
        if trigger.len() > self.trigger_table.len() {
            return Err(Error::TriggerTable(trigger.len()));
        }
        self.command.set(0);
        for (register, byte) in self.trigger_table.iter().zip(trigger) {
            register.set(*byte);
        }
        self.trigger_pointer.set(self.trigger_table.as_ptr() as u64);
        let p = &self.parameters;
        for register in [
            &p.error_type,
            &p.vendor_extension_offset,
            &p.flags,
            &p.processor,
        ]
        .into_iter()
        .chain(&p.memory_address)
        .chain(&p.memory_mask)
        {
            register.set(0);
        }
        p.pcie_sbdf.set(0);
        Ok(self.error_types.extract())
    }

    /// Name the device vendor-defined types inject into: segment and bus 0,
    /// `device`.`function` with config dword 0 `id`. Points the parameters
    /// at the extension; the PSP's OEM qword stays.
    pub fn describe_vendor_device(&self, device: u8, function: u8, id: u32) {
        let e = &self.vendor_extension;
        e.length.set(size_of::<VendorExtension>() as u32);
        e.sbdf
            .write(SBDF::DEVICE.val(device.into()) + SBDF::FUNCTION.val(function.into()));
        e.id.set(id);
        e.revision.set(0);
        for register in &e.oem {
            register.set(0);
        }
        self.parameters
            .vendor_extension_offset
            .set(size_of::<Parameters>() as u32);
    }
}

/// RasEinj (C2P 41h), sent from SMM with the SMM flag: action 1 opens the
/// PSP's EINJ service (AmdPlatformRasRsSmm `RasEnablePspEinj`, S4056 V241
/// `db90`: size 0Ch, action 1). Rome's RAS-EINJ is C2P 22h (`psp::ras`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct RasEinj {
    pub action: u32,
}
impl RasEinj {
    pub const ENABLE: Self = Self { action: 1 };
}
impl super::command::Command for RasEinj {
    const ID: u8 = 0x41;
    const NAME: &'static str = "RasEinj";
    fn supported(generation: super::Variant) -> bool {
        generation == super::Variant::Rs
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum Error {
    /// The register reads all-ones: no PSP BAR.
    Unavailable,
    Timeout {
        command: u8,
    },
    Status {
        command: u8,
        status: u8,
    },
    /// The buffer isn't where the ABL reserved it.
    Placement(u64),
    /// A trigger action table that does not fit the buffer's slot.
    TriggerTable(usize),
}

/// The RAS mailbox, over any `RAS_MBOX` register (MMIO: [`Mailbox::mmio`]).
pub struct Mailbox<R> {
    register: R,
}

impl Mailbox<&'static ReadWrite<u32, RAS_MBOX::Register>> {
    /// The RAS mailbox of the PSP whose BAR is mapped at `bar`.
    ///
    /// # Safety
    /// `bar` is an enabled PSP BAR, identity mapped and uncached, and the
    /// caller is the only user of the RAS mailbox.
    pub unsafe fn mmio(bar: u64) -> Self {
        // SAFETY: the caller's contract; a dword-aligned MMIO register.
        Self::new(unsafe { &*((bar + MAILBOX_OFFSET) as *const ReadWrite<u32, _>) })
    }
}

impl<R> Mailbox<R>
where
    R: core::ops::Deref,
    R::Target: Sized
        + Readable<T = u32, R = RAS_MBOX::Register>
        + Writeable<T = u32, R = RAS_MBOX::Register>,
{
    pub const fn new(register: R) -> Self {
        Self { register }
    }

    /// One request: `command` and `data`, then wait for `DONE`. Returns
    /// the reply's data; a non-zero status is an error.
    pub fn request(&self, timer: &impl Timer, command: u8, data: u16) -> Result<u16, Error> {
        let register = &*self.register;
        if register.get() == u32::MAX {
            return Err(Error::Unavailable);
        }
        register.modify(
            RAS_MBOX::DONE::CLEAR
                + RAS_MBOX::STATUS.val(0)
                + RAS_MBOX::COMMAND.val(command.into())
                + RAS_MBOX::DATA.val(data.into()),
        );
        let mut waited = 0;
        while !register.is_set(RAS_MBOX::DONE) {
            if waited >= TIMEOUT_US {
                return Err(Error::Timeout { command });
            }
            timer.delay_us(POLL_US);
            waited += POLL_US;
        }
        let reply = register.extract();
        match reply.read(RAS_MBOX::STATUS) as u8 {
            0 => Ok(reply.read(RAS_MBOX::DATA) as u16),
            status => Err(Error::Status { command, status }),
        }
    }

    /// The command buffer, which must be the ABL's `ReservedRasEinj` hole.
    pub fn buffer(&self, timer: &impl Timer, reserved: &Range<u64>) -> Result<u64, Error> {
        let mut address = 0;
        for word in 0..ADDRESS_WORDS {
            address |= u64::from(self.request(timer, word, 0)?) << (16 * word);
        }
        if address < reserved.start
            || address
                .checked_add(size_of::<Buffer>() as u64)
                .is_none_or(|end| end > reserved.end)
        {
            return Err(Error::Placement(address));
        }
        Ok(address)
    }
}

/// The EINJ table's actions over the command buffer at `buffer` and the RAS
/// mailbox at `mailbox` (the vendor template, AmdPlatformRasRsDxe
/// `AdjustInjectionInstEntries`).
#[cfg(feature = "acpi")]
pub fn entries(mailbox: u64, buffer: u64) -> [fstart_acpi::einj::Entry; 10] {
    use core::mem::offset_of;
    use fstart_acpi::einj::{Action, Entry, Instruction, Register};
    use fstart_acpi::gas::AccessSize;
    let word = |offset: usize| Register::memory(buffer + offset as u64, 32);
    let qword = |offset: usize| Register::memory(buffer + offset as u64, 64);
    [
        Entry::new(
            Action::BeginOperation,
            Instruction::WriteRegisterValue,
            word(offset_of!(Buffer, command)),
            1,
            0xffff_ffff,
        ),
        Entry::new(
            Action::GetTriggerActionTable,
            Instruction::ReadRegister,
            qword(offset_of!(Buffer, trigger_pointer)),
            0,
            u64::MAX,
        ),
        Entry::new(
            Action::SetErrorType,
            Instruction::WriteRegister,
            word(offset_of!(Buffer, error_type)),
            0,
            0xffff_ffff,
        ),
        Entry::new(
            Action::GetErrorType,
            Instruction::ReadRegister,
            word(offset_of!(Buffer, error_types)),
            0,
            0xffff_ffff,
        ),
        Entry::new(
            Action::EndOperation,
            Instruction::WriteRegisterValue,
            word(offset_of!(Buffer, end)),
            1,
            0xffff_ffff,
        ),
        Entry::new(
            Action::ExecuteOperation,
            Instruction::WriteRegisterValue,
            Register::memory(mailbox, 32),
            EXECUTE.into(),
            0xffff_ffff,
        ),
        Entry::new(
            Action::CheckBusyStatus,
            Instruction::ReadRegisterValue,
            word(offset_of!(Buffer, command)),
            1,
            1,
        ),
        Entry::new(
            Action::GetCommandStatus,
            Instruction::ReadRegister,
            word(offset_of!(Buffer, command)).field(COMMAND::STATUS.shift as u8, 31),
            0,
            0xffff_ffff,
        )
        .with_access(AccessSize::DwordAccess),
        // Linux writes its parameter block at this register's address.
        Entry::new(
            Action::SetErrorTypeWithAddress,
            Instruction::WriteRegister,
            qword(offset_of!(Buffer, parameters)),
            0,
            u64::MAX,
        ),
        Entry::new(
            Action::GetExecuteOperationTimings,
            Instruction::ReadRegister,
            qword(offset_of!(Buffer, timings)),
            0,
            u64::MAX,
        ),
    ]
}

/// The trigger action table's one entry, which firmware writes at
/// [`Buffer::trigger_table`]: a doorbell write of 1 (the vendor template).
#[cfg(feature = "acpi")]
pub fn trigger(buffer: u64) -> fstart_acpi::einj::Entry {
    use core::mem::offset_of;
    use fstart_acpi::einj::{Action, Entry, Instruction, Register};
    Entry::new(
        Action::TriggerError,
        Instruction::WriteRegisterValue,
        Register::memory(buffer + offset_of!(Buffer, trigger_doorbell) as u64, 32),
        1,
        0xffff_ffff,
    )
    .preserve()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    struct Clock;
    impl Timer for Clock {
        fn delay_us(&self, _: u64) {}
        fn timestamp_us(&self) -> u64 {
            0
        }
    }

    /// A PSP holding `address`: command n answers its 16-bit word n at once,
    /// with `status` and the preserved bits kept.
    struct Psp {
        value: Cell<u32>,
        address: u64,
        status: u32,
    }
    impl Readable for Psp {
        type T = u32;
        type R = RAS_MBOX::Register;
        fn get(&self) -> u32 {
            self.value.get()
        }
    }
    impl Writeable for Psp {
        type T = u32;
        type R = RAS_MBOX::Register;
        fn set(&self, value: u32) {
            assert_eq!(value >> 31, 0, "DONE is written 0");
            let command = RAS_MBOX::COMMAND.read(value);
            let word = (self.address >> (16 * command)) as u32 & 0xffff;
            self.value
                .set(value & 0x7000_0000 | 1 << 31 | self.status << 24 | command << 16 | word);
        }
    }

    #[test]
    fn buffer_words_assemble_and_must_be_the_reserved_hole() {
        let hole = 0x60_4e90_0000..0x60_4ea0_0000;
        let psp = Psp {
            value: Cell::new(0x3000_0000),
            address: 0x60_4e90_0000,
            status: 0,
        };
        let mailbox = Mailbox::new(&psp);
        assert_eq!(mailbox.buffer(&Clock, &hole), Ok(0x60_4e90_0000));
        assert_eq!(psp.value.get() & 0x7000_0000, 0x3000_0000);
        assert_eq!(
            mailbox.buffer(&Clock, &(0..0x1000)),
            Err(Error::Placement(0x60_4e90_0000))
        );
        let failing = Psp { status: 2, ..psp };
        assert_eq!(
            Mailbox::new(&failing).buffer(&Clock, &hole),
            Err(Error::Status {
                command: 0,
                status: 2
            })
        );
        assert_eq!(RAS_MBOX::COMMAND.read(EXECUTE), 4);
        #[cfg(feature = "acpi")]
        {
            let e = entries(0xd031_057c, 0x1000);
            assert_eq!(e[5].bytes()[8..16], 0xd031_057cu64.to_le_bytes());
            assert_eq!(e[8].bytes()[8..16], 0x1030u64.to_le_bytes());
            assert_eq!(trigger(0x1000).bytes()[8..16], 0x1028u64.to_le_bytes());
        }
    }

    #[test]
    fn buffer_is_laid_out_as_the_vendor_leaves_it() {
        let mut memory = [0xa5a5_a5a5_a5a5_a5a5u64; size_of::<Buffer>() / 8];
        memory[0] = 0x8000_01f8;
        let base = memory.as_mut_ptr() as u64;
        // SAFETY: host memory of the buffer's size and alignment.
        let buffer = unsafe { Buffer::at(base) };
        assert_eq!(
            buffer.prepare(&[0x5a; 0x81]).map(|t| t.get()),
            Err(Error::TriggerTable(0x81))
        );
        assert_eq!(
            buffer.prepare(&[0x5a; 0x30]).map(|t| t.get()),
            Ok(0x8000_01f8)
        );
        buffer.describe_vendor_device(0x18, 0, 0x14ad_1022);
        let bytes: &[u8] = zerocopy::IntoBytes::as_bytes(&memory[..]);
        let dword =
            |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        assert_eq!(dword(0x10), 0);
        assert_eq!(
            u64::from_le_bytes(bytes[0x20..0x28].try_into().unwrap()),
            base + 0x80
        );
        assert!(
            bytes[0x30..0x54]
                .iter()
                .enumerate()
                .all(|(i, b)| *b == if i == 4 { 0x24 } else { 0 })
        );
        assert_eq!(
            [dword(0x54), dword(0x58), dword(0x5c), dword(0x60)],
            [0x2c, 0x18 << 11, 0x14ad_1022, 0]
        );
        // The PSP's OEM qword is untouched; the rest of the extension is cleared.
        assert_eq!(dword(0x64), 0xa5a5_a5a5);
        assert!(bytes[0x6c..0x80].iter().all(|b| *b == 0));
        assert_eq!(&bytes[0x80..0xb0], &[0x5a; 0x30]);
        assert_eq!(bytes[0xb0], 0xa5);
    }
}
