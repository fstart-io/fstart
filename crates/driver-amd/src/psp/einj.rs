// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PSP's own ACPI error-injection service (the RS PSP; SP3's Rome
//! injects through SMM instead, [`super::ras`]).
//!
//! The PSP injects the errors an OS asks for itself: the EINJ table's
//! actions read and write the PSP's command buffer ([`Buffer`], in the
//! ABL's `ReservedRasEinj` APOB hole, [`crate::apob::rs::HOLE_RAS_EINJ`])
//! and EXECUTE_OPERATION rings `MP0_C2PMSG_31` ([`Mailbox`]); no SMI. The
//! x86 side asks the same register for the buffer's address, 16 bits at a
//! time. AmdPlatformRasRsDxe `ApeiEinjInstall` and
//! `AdjustInjectionInstEntries` (S4056 V241 Genoa
//! `AmdPlatformRasRsDxe...pe.c:4340-4556`, S8040 V2.00 the same).
//!
//! The PSP opens the service only for RasEinj ENABLE ([`RasEinj`]) from
//! SMM, with the SMM flag set: from normal mode it answers status 11h, and
//! `MP0_C2PMSG_31` reads all-ones until then.
//!
//! The WH PSP (Venice) runs the same service with its mailbox moved with
//! the rest of its C2P registers ([`mailbox_offset`]: `PSP_BAR + 1097Ch`,
//! Dell AmdPlatformRasWhDxe `0x1f570`) and a larger buffer ([`WhBuffer`]:
//! the EINJv2 error types at `+0Ch`, the vendor error type extension at
//! `+180h`), and an EINJ table of revision 2 with an eleventh action,
//! EINJv2 GET_ERROR_TYPE ([`entries_wh`]; `ApeiEinjInstallBuffer`, Dell
//! `0x1fa70`, template at `0xc06d0`). The vendor takes the buffer's
//! address from the PSP without checking it against an APOB hole.

use core::ops::Range;
use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use fstart_core::services::timer::Timer;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields, register_structs};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use super::Variant;
use super::command::Command;

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
    ],
    /// GET_ERROR_TYPE's answer: ACPI EINJ error-type bits.
    pub ERROR_TYPES [
        /// The vendor-defined types of [`VendorExtension`] are supported.
        VENDOR OFFSET(31) NUMBITS(1) []
    ],
    /// WH: EINJv2 GET_ERROR_TYPE's answer at `+0Ch`.
    pub EINJV2_TYPES [
        /// EINJv2 is supported: the action reads this register.
        SUPPORTED OFFSET(31) NUMBITS(1) []
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

/// `MP0_C2PMSG_31` in the PSP BAR (PSP_BASE + `1057Ch`, `ApeiEinjInstall`).
pub const MAILBOX_OFFSET: u64 = 0x1_057c;
/// WH `MP0_C2PMSG_31` in the PSP BAR (`psp_base + 0x1097c`, Dell
/// AmdPlatformRasWhDxe `0x1f570`).
pub const MAILBOX_OFFSET_WH: u64 = 0x1_097c;

/// `MP0_C2PMSG_31` of the `generation` PSP in its BAR.
pub const fn mailbox_offset(generation: Variant) -> u64 {
    match generation {
        Variant::Wh => MAILBOX_OFFSET_WH,
        Variant::Zp | Variant::Rv | Variant::Ssp | Variant::Gn => MAILBOX_OFFSET,
    }
}
/// EXECUTE_OPERATION's write: command 4, data 83h (the vendor template).
pub const EXECUTE: u32 = 0x0004_0083;
/// Commands 0..=3 return bits `16n..16n+16` of the buffer's address.
const ADDRESS_WORDS: u8 = 4;
const TIMEOUT_US: u64 = 100_000;
const POLL_US: u64 = 10;

register_structs! {
    /// The command buffer as the vendor's EINJ table addresses it, in DRAM
    /// (the ABL's `ReservedRasEinj` hole).
    pub Buffer {
        /// GET_ERROR_TYPE.
        (0x000 => pub error_types: MmioReadOnly<u32, ERROR_TYPES::Register>),
        (0x004 => _reserved0),
        /// SET_ERROR_TYPE.
        (0x008 => pub error_type: MmioReadWrite<u32>),
        /// WH: EINJv2 GET_ERROR_TYPE (reserved on the RS PSP).
        (0x00c => pub einj_v2_types: MmioReadOnly<u32, EINJV2_TYPES::Register>),
        /// BEGIN writes 1; CHECK_BUSY_STATUS and GET_COMMAND_STATUS read it.
        (0x010 => pub command: MmioReadWrite<u32, COMMAND::Register>),
        (0x014 => _reserved2),
        /// GET_TRIGGER_ACTION_TABLE: the trigger table's address.
        (0x020 => pub trigger_pointer: MmioReadWrite<u64>),
        /// The trigger action writes 1 here.
        (0x028 => pub trigger_doorbell: MmioReadWrite<u32>),
        /// END_OPERATION writes 1.
        (0x02c => pub end: MmioReadWrite<u32>),
        /// SET_ERROR_TYPE_WITH_ADDRESS: the OS's parameter block.
        (0x030 => pub parameters: Parameters),
        /// The vendor-type extension, right after the parameters.
        (0x054 => pub vendor_extension: VendorExtension),
        (0x080 => pub trigger_table: [MmioReadWrite<u8>; 0x80]),
        /// GET_EXECUTE_OPERATION_TIMINGS.
        (0x100 => pub timings: MmioReadOnly<u64>),
        (0x108 => @END),
    },

    /// ACPI's SET_ERROR_TYPE_WITH_ADDRESS data structure. Its 64-bit fields
    /// are dword pairs: the structure, as placed, is only dword aligned.
    pub Parameters {
        (0x00 => pub error_type: MmioReadWrite<u32>),
        /// From this structure's start; 0 without a vendor extension.
        (0x04 => pub vendor_extension_offset: MmioReadWrite<u32>),
        (0x08 => pub flags: MmioReadWrite<u32>),
        (0x0c => pub processor: MmioReadWrite<u32>),
        (0x10 => pub memory_address: [MmioReadWrite<u32>; 2]),
        (0x18 => pub memory_mask: [MmioReadWrite<u32>; 2]),
        (0x20 => pub pcie_sbdf: MmioReadWrite<u32, SBDF::Register>),
        (0x24 => @END),
    },

    /// ACPI's vendor error type extension structure, as the vendor fills it.
    pub VendorExtension {
        (0x00 => pub length: MmioReadWrite<u32>),
        (0x04 => pub sbdf: MmioReadWrite<u32, SBDF::Register>),
        /// Vendor ID (low half) and device ID, as config dword 0 reads.
        (0x08 => pub id: MmioReadWrite<u32>),
        /// Revision ID; the rest is reserved.
        (0x0c => pub revision: MmioReadWrite<u32>),
        /// OEM qword the PSP owns; firmware leaves it alone.
        (0x10 => pub psp: [MmioReadWrite<u32>; 2]),
        (0x18 => pub oem: [MmioReadWrite<u32>; 5]),
        (0x2c => @END),
    },

    /// The WH PSP's command buffer: the RS layout, then the vendor error
    /// type extension at `+180h` (`ApeiEinjInstallBuffer`, Dell
    /// `0x1fa70`), which the PSP's own vendor error section and logs
    /// follow (`+190h` on) up to its length, 4 KiB from the start.
    pub WhBuffer {
        (0x000 => pub base: Buffer),
        (0x108 => _reserved0),
        (0x180 => pub vendor_extension: WhVendorExtension),
        (0x190 => _psp),
        (0x1000 => @END),
    },

    /// The head of the WH vendor error type extension, the fields the
    /// vendor writes.
    pub WhVendorExtension {
        (0x00 => pub length: MmioReadWrite<u32>),
        (0x04 => pub sbdf: MmioReadWrite<u32, SBDF::Register>),
        (0x08 => pub vendor_id: MmioReadWrite<u16>),
        (0x0a => pub device_id: MmioReadWrite<u16>),
        (0x0c => pub revision: MmioReadWrite<u8>),
        (0x0d => _reserved),
        (0x10 => @END),
    }
}

/// What the WH vendor writes into the extension when the PSP offers
/// vendor types: its length (to the end of the 4 KiB buffer), 00:18.0,
/// AMD's vendor ID, the device ID and revision it names (constants in
/// `0x1fa70`; not compared with 00:18.0's own ID here).
pub const WH_VENDOR_EXTENSION_LENGTH: u32 = 0xe80;
pub const WH_VENDOR_DEVICE_ID: u16 = 0x12c8;
pub const WH_VENDOR_REVISION: u8 = 0x10;

/// GET_ERROR_TYPE's and EINJv2 GET_ERROR_TYPE's answers.
pub type WhTypes = (
    LocalRegisterCopy<u32, ERROR_TYPES::Register>,
    LocalRegisterCopy<u32, EINJV2_TYPES::Register>,
);

impl WhBuffer {
    /// The WH command buffer at `address`.
    ///
    /// # Safety
    /// As [`Buffer::at`], for the 4 KiB of a [`WhBuffer`].
    pub unsafe fn at(address: u64) -> &'static Self {
        // SAFETY: the caller's contract.
        unsafe { &*(address as *const Self) }
    }

    /// Lay the buffer out as `ApeiEinjInstallBuffer` leaves it: the RS
    /// layout ([`Buffer::prepare`]), and with vendor types the extension
    /// head and the parameters pointing at it. Returns the error types and
    /// the EINJv2 types.
    pub fn prepare(&self, trigger: &[u8]) -> Result<WhTypes, Error> {
        let types = self.base.prepare(trigger)?;
        if types.is_set(ERROR_TYPES::VENDOR) {
            let e = &self.vendor_extension;
            e.length.set(WH_VENDOR_EXTENSION_LENGTH);
            e.sbdf.write(SBDF::DEVICE.val(0x18) + SBDF::FUNCTION.val(0));
            e.vendor_id.set(0x1022);
            e.device_id.set(WH_VENDOR_DEVICE_ID);
            e.revision.set(WH_VENDOR_REVISION);
            self.base.parameters.vendor_extension_offset.set(
                (core::mem::offset_of!(WhBuffer, vendor_extension)
                    - core::mem::offset_of!(Buffer, parameters)) as u32,
            );
        }
        Ok((types, self.base.einj_v2_types.extract()))
    }
}

impl Buffer {
    /// The command buffer at `address`.
    ///
    /// # Safety
    /// `address` is the PSP's EINJ command buffer (see [`Mailbox::buffer`]),
    /// identity mapped, and the caller is its only user until the OS runs.
    pub unsafe fn at(address: u64) -> &'static Self {
        // SAFETY: the caller's contract; `Mailbox::buffer` checked its
        // alignment and that all of it is in the hole.
        unsafe { &*(address as *const Self) }
    }

    /// Lay the buffer out as `ApeiEinjInstall` leaves it: no command
    /// pending, `trigger` (the trigger action table) in place and pointed
    /// to, the parameters cleared. Returns the supported types.
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
    /// `device`.`function` with config dword 0 `id` (the vendor names DF F0,
    /// 00:18.0). Points the parameters at the extension; the PSP's OEM
    /// qword stays.
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
/// PSP's EINJ service. AmdPlatformRasRsSmm `RasEnablePspEinj` (S4056 V241
/// Genoa `AmdPlatformRasRsSmm...pe.c:7945-7980`: size 0Ch, action 1, the
/// flag set around it). Milan's AmdPlatformRasGnSmm takes 41h actions too
/// (sp3 `decomp/milan-s8030-v404/AmdPlatformRasGnSmm...pe.c:6084`); Rome's
/// RAS commands are 22h ([`super::ras`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct RasEinj {
    pub action: u32,
}

impl RasEinj {
    pub const ENABLE: Self = Self { action: 1 };
}

impl Command for RasEinj {
    const ID: u8 = 0x41;
    const NAME: &'static str = "RasEinj";
    /// WH: AmdPlatformRasWhSmm `PspMboxBiosRasEinj` (`0x5a0f0`, the same
    /// 0Ch buffer and action, sent with the SMM flag).
    fn supported(generation: Variant) -> bool {
        matches!(generation, Variant::Gn | Variant::Wh)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The register reads all-ones: no PSP BAR, or the service is closed.
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

impl Mailbox<&'static MmioReadWrite<u32, RAS_MBOX::Register>> {
    /// The RAS mailbox of the `generation` PSP whose BAR is mapped at
    /// `bar` (the generation places it: [`mailbox_offset`]).
    ///
    /// # Safety
    /// `bar` is an enabled PSP BAR, identity mapped and uncached, and the
    /// caller is the only user of the RAS mailbox.
    pub unsafe fn mmio(bar: u64, generation: Variant) -> Self {
        // SAFETY: the caller's contract; a dword-aligned MMIO register.
        Self::new(unsafe { &*((bar + mailbox_offset(generation)) as *const MmioReadWrite<u32, _>) })
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

    /// One request: `command` and `data`, then wait for `DONE`
    /// (`ExecutePspRasEinjCmd`). Returns the reply's data; a non-zero
    /// status is an error.
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

    /// The command buffer, which must be in the ABL's `ReservedRasEinj`
    /// hole, all of it, and aligned as a [`Buffer`] ([`Buffer::at`]).
    pub fn buffer(&self, timer: &impl Timer, reserved: &Range<u64>) -> Result<u64, Error> {
        self.buffer_of::<Buffer>(timer, reserved)
    }

    /// [`Self::buffer`] for a buffer laid out as `B` ([`WhBuffer`]).
    pub fn buffer_of<B>(&self, timer: &impl Timer, reserved: &Range<u64>) -> Result<u64, Error> {
        let mut address = 0;
        for word in 0..ADDRESS_WORDS {
            address |= u64::from(self.request(timer, word, 0)?) << (16 * word);
        }
        if address < reserved.start
            || !address.is_multiple_of(align_of::<B>() as u64)
            || address
                .checked_add(size_of::<B>() as u64)
                .is_none_or(|end| end > reserved.end)
        {
            return Err(Error::Placement(address));
        }
        Ok(address)
    }
}

/// The EINJ table's actions over the command buffer at `buffer` and the RAS
/// mailbox at `mailbox` (the vendor template, `AdjustInjectionInstEntries`).
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

/// The WH EINJ actions over the [`WhBuffer`] at `buffer` and the RAS
/// mailbox at `mailbox`: [`entries`], then EINJv2 GET_ERROR_TYPE reading
/// `+0Ch` if the PSP supports it (`v2`), a NOOP otherwise (the vendor
/// template's eleventh entry, `0xc0840`). Publish them in a revision-2
/// table (`fstart_acpi::einj::table_revision`).
#[cfg(feature = "acpi")]
pub fn entries_wh(mailbox: u64, buffer: u64, v2: bool) -> [fstart_acpi::einj::Entry; 11] {
    use core::mem::offset_of;
    use fstart_acpi::einj::{Action, Entry, Instruction, Register};
    let common = entries(mailbox, buffer);
    let einj_v2 = Entry::new(
        Action::Einjv2GetErrorType,
        if v2 {
            Instruction::ReadRegister
        } else {
            Instruction::Noop
        },
        Register::memory(buffer + offset_of!(Buffer, einj_v2_types) as u64, 32),
        0,
        0xffff_ffff,
    );
    core::array::from_fn(|i| common.get(i).copied().unwrap_or(einj_v2))
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
        // In the hole, but not where a `Buffer` can be.
        let misaligned = Psp {
            value: Cell::new(0x3000_0000),
            address: 0x60_4e90_0002,
            status: 0,
        };
        assert!(align_of::<Buffer>() > 2);
        assert_eq!(
            Mailbox::new(&misaligned).buffer(&Clock, &hole),
            Err(Error::Placement(0x60_4e90_0002))
        );
        // The end of the hole: its start is in it, the rest is not.
        let tail = Psp {
            address: hole.end - 4,
            ..misaligned
        };
        assert_eq!(
            Mailbox::new(&tail).buffer(&Clock, &hole),
            Err(Error::Placement(hole.end - 4))
        );
        let failing = Psp { status: 2, ..psp };
        assert_eq!(
            Mailbox::new(&failing).buffer(&Clock, &hole),
            Err(Error::Status {
                command: 0,
                status: 2
            })
        );
        let closed = Psp {
            value: Cell::new(u32::MAX),
            ..failing
        };
        assert_eq!(
            Mailbox::new(&closed).buffer(&Clock, &hole),
            Err(Error::Unavailable)
        );
        assert_eq!(RAS_MBOX::COMMAND.read(EXECUTE), 4);
    }

    #[test]
    fn wh_buffer_follows_the_vendor() {
        use core::mem::offset_of;
        assert_eq!(mailbox_offset(Variant::Wh), 0x1_097c);
        assert_eq!(mailbox_offset(Variant::Gn), MAILBOX_OFFSET);
        assert_eq!(offset_of!(Buffer, einj_v2_types), 0x0c);
        assert_eq!(offset_of!(WhBuffer, vendor_extension), 0x180);
        assert_eq!(size_of::<WhBuffer>(), 0x1000);

        let mut memory = [0xa5a5_a5a5_a5a5_a5a5u64; 0x1000 / 8];
        // Vendor types; EINJv2 offered.
        memory[0] = 0x8000_01f8;
        memory[1] = 0x8000_0001_0000_0000;
        let base = memory.as_mut_ptr() as u64;
        // SAFETY: host memory of the buffer's size and alignment.
        let buffer = unsafe { WhBuffer::at(base) };
        let (types, v2) = buffer.prepare(&[0x5a; 0x30]).unwrap();
        assert!(types.is_set(ERROR_TYPES::VENDOR));
        assert!(v2.is_set(EINJV2_TYPES::SUPPORTED));
        let bytes: &[u8] = zerocopy::IntoBytes::as_bytes(&memory[..]);
        let dword =
            |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        // Parameters cleared but for the extension's offset, 150h.
        assert_eq!(dword(0x34), 0x150);
        assert_eq!([dword(0x30), dword(0x38), dword(0x50)], [0, 0, 0]);
        assert_eq!(
            [dword(0x180), dword(0x184), dword(0x188)],
            [0xe80, 0xc000, 0x12c8_1022]
        );
        assert_eq!(bytes[0x18c], 0x10);
        // The bytes after it stay the PSP's.
        assert_eq!(bytes[0x18d], 0xa5);
        assert_eq!(dword(0x190), 0xa5a5_a5a5);

        // No vendor types: no extension.
        let mut memory = [0u64; 0x1000 / 8];
        let base = memory.as_mut_ptr() as u64;
        // SAFETY: as above.
        let (types, v2) = unsafe { WhBuffer::at(base) }.prepare(&[]).unwrap();
        assert!(!types.is_set(ERROR_TYPES::VENDOR) && !v2.is_set(EINJV2_TYPES::SUPPORTED));
        assert!(memory[0x180 / 8..].iter().all(|q| *q == 0));
        assert_eq!(memory[0x30 / 8], 0);
    }

    #[cfg(feature = "acpi")]
    #[test]
    fn wh_actions_add_einj_v2() {
        let e = entries_wh(0xd031_097c, 0x1000, true);
        assert_eq!(e[5].bytes()[8..16], 0xd031_097cu64.to_le_bytes());
        let v2 = e[10].bytes();
        assert_eq!(v2[..8], [0x11, 0, 0, 0, 0, 32, 0, 3]);
        assert_eq!(v2[8..16], 0x100cu64.to_le_bytes());
        assert_eq!(entries_wh(0, 0x1000, false)[10].bytes()[1], 4, "NOOP");
    }

    #[test]
    fn ras_einj_is_a_twelve_byte_enable() {
        let buffer = super::super::C2pBuffer::new(RasEinj::ENABLE);
        assert_eq!(buffer.header.size, 12);
        assert!(RasEinj::supported(Variant::Gn));
        assert!(RasEinj::supported(Variant::Wh));
        assert!(!RasEinj::supported(Variant::Ssp));
    }

    #[cfg(feature = "acpi")]
    #[test]
    fn the_actions_follow_the_vendor_template() {
        let e = entries(0xd031_057c, 0x1000);
        let bytes = |i: usize| e[i].bytes();
        assert_eq!(bytes(5)[8..16], 0xd031_057cu64.to_le_bytes());
        assert_eq!(bytes(5)[16..24], u64::from(EXECUTE).to_le_bytes());
        assert_eq!(bytes(0)[8..16], 0x1010u64.to_le_bytes());
        assert_eq!(bytes(1)[8..16], 0x1020u64.to_le_bytes());
        assert_eq!(bytes(4)[8..16], 0x102cu64.to_le_bytes());
        assert_eq!(bytes(8)[8..16], 0x1030u64.to_le_bytes());
        assert_eq!(bytes(9)[8..16], 0x1100u64.to_le_bytes());
        // GET_COMMAND_STATUS reads bits 31:1 of the command word.
        assert_eq!(bytes(7)[4..8], [0, 31, 1, 3]);
        let t = trigger(0x1000).bytes();
        assert_eq!(t[..4], [0xff, 3, 1, 0]);
        assert_eq!(t[8..16], 0x1028u64.to_le_bytes());
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
        // The vendor's extension: 2Ch bytes, 00:18.0 (C000h), DF F0's IDs.
        assert_eq!(
            [dword(0x54), dword(0x58), dword(0x5c), dword(0x60)],
            [0x2c, 0xc000, 0x14ad_1022, 0]
        );
        // The PSP's OEM qword is untouched; the rest of the extension is cleared.
        assert_eq!(dword(0x64), 0xa5a5_a5a5);
        assert!(bytes[0x6c..0x80].iter().all(|b| *b == 0));
        assert_eq!(&bytes[0x80..0xb0], &[0x5a; 0x30]);
        assert_eq!(bytes[0xb0], 0xa5);
    }
}
