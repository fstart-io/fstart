// SPDX-License-Identifier: MIT OR Apache-2.0
//! The P2C (PSP to CPU) mailbox the SMM handler answers once SmmInfo has
//! registered it (PSP_APOB §3.4). The PSP raises its "fake SMI" for each
//! request; the handler must answer and then drop SmiTrig0\[25\], or the
//! source stays asserted and the FCH raises SMIs forever.
//!
//! Layout and answers from AmdPspP2CmboxV2Ssp `0x720` (Rome a9724bd1,
//! instruction-checked); the Rs donors use the same buffer protocol
//! (`reports/PSP_SMM_ABI.md`). The SPI NV, RPMC and ARS services are not
//! implemented: they get the donors' RPMC answer, "unsupported".

use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite},
};

register_bitfields![u32,
    pub Flags [
        /// The byte-sum checksum the PSP asked for.
        SUM OFFSET(0) NUMBITS(8) [],
        /// The PSP asks for a byte-sum checksum in SUM.
        CHECKSUM OFFSET(8) NUMBITS(1) [],
        /// x86 is ready for the next request. Every donor sets it with
        /// COMMAND = 0 before SmmInfo (Naples d8a42c95 `0xbef`, Rome
        /// a9724bd1 `0xcef`), clears it while it works and sets it again
        /// when done.
        READY OFFSET(31) NUMBITS(1) [],
    ],
];

register_structs! {
    /// The P2C buffer: the PSP's command and flags, then the command buffer
    /// `{size, status, data}`.
    pub P2cBuffer {
        (0x0 => pub command: ReadWrite<u32>),
        (0x4 => pub flags: ReadWrite<u32, Flags::Register>),
        (0x8 => pub size: ReadWrite<u32>),
        (0xc => pub status: ReadWrite<u32>),
        (0x10 => pub data: [ReadOnly<u8>; DATA_SIZE]),
        (0x2000 => @END),
    }
}
const _: () = assert!(size_of::<P2cBuffer>() as u64 == super::command::P2C_BUFFER_SIZE);

/// Bytes of the command buffer after its size and status.
const DATA_SIZE: usize = super::command::P2C_BUFFER_SIZE as usize - 0x10;
/// Largest command buffer that stays inside the P2C buffer (the donor
/// accepts 0x2000, 8 bytes more than it has).
pub const MAX_SIZE: u32 = super::command::P2C_BUFFER_SIZE as u32 - 8;
/// Statuses the donor writes: done, invalid request, and what its RPMC
/// services return ("unsupported").
pub const OK: u32 = 0;
pub const INVALID: u32 = 1;
pub const UNSUPPORTED: u32 = 4;

/// Idle and ready, before SmmInfo registers the buffer.
pub fn ready(buffer: &P2cBuffer) {
    buffer.command.set(0);
    buffer.flags.write(Flags::READY::SET);
}

/// Answer the PSP's request the way AGESA's mailbox does: busy, status,
/// checksum when asked, write back, command 0, ready. 0x81/0x82 need
/// nothing from x86; SPI NV (0x83–0x86), RPMC (0x88/0x89) and ARS (0x8a)
/// get "unsupported", anything else "invalid".
///
/// # Safety
/// The PSP only writes `buffer` while READY is set. Ring 0: the answer is
/// written back with WBINVD.
pub unsafe fn answer(buffer: &P2cBuffer) {
    // SAFETY: the caller's contract.
    unsafe { answer_with(buffer, fstart_arch::x86::writeback_invalidate_caches) }
}

/// [`answer`], writing the buffer back with `writeback`.
///
/// # Safety
/// As [`answer`]; `writeback` makes the buffer visible to the PSP.
pub unsafe fn answer_with(buffer: &P2cBuffer, writeback: unsafe fn()) {
    buffer.flags.modify(Flags::READY::CLEAR);
    let size = buffer.size.get();
    let status = match buffer.command.get() {
        _ if size > MAX_SIZE => INVALID,
        0x81 | 0x82 => OK,
        0x83..=0x86 | 0x88..=0x8a => UNSUPPORTED,
        _ => INVALID,
    };
    buffer.status.set(status);
    if status == OK && buffer.flags.is_set(Flags::CHECKSUM) {
        // The byte sum of the `size` bytes of the command buffer.
        let header = [buffer.size.get().to_le_bytes(), status.to_le_bytes()];
        let sum = header
            .iter()
            .flatten()
            .copied()
            .chain(buffer.data.iter().map(Readable::get))
            .take(size as usize)
            .fold(0u8, u8::wrapping_add);
        buffer.flags.modify(Flags::SUM.val(sum.into()));
    }
    // SAFETY: the caller's contract.
    unsafe { writeback() };
    buffer.command.set(0);
    buffer.flags.modify(Flags::READY::SET);
    // SAFETY: the caller's contract.
    unsafe { writeback() };
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn nothing() {}

    fn buffer() -> std::boxed::Box<P2cBuffer> {
        // SAFETY: all-zero bytes are a valid buffer of plain registers.
        unsafe { std::boxed::Box::new_zeroed().assume_init() }
    }

    #[test]
    fn rpmc_is_unsupported_and_ready_returns() {
        let buffer = buffer();
        ready(&buffer);
        assert_eq!(buffer.flags.get(), 1 << 31);
        buffer.command.set(0x88);
        buffer.size.set(8);
        unsafe { answer_with(&buffer, nothing) };
        assert_eq!(buffer.status.get(), UNSUPPORTED);
        assert_eq!(
            (buffer.command.get(), buffer.flags.is_set(Flags::READY)),
            (0, true)
        );
        buffer.command.set(0x99);
        unsafe { answer_with(&buffer, nothing) };
        assert_eq!(buffer.status.get(), INVALID);
    }

    #[test]
    fn the_checksum_covers_size_status_and_data() {
        let mut buffer = buffer();
        // SAFETY: the test owns the buffer; the PSP's write of a data byte.
        unsafe { (&raw mut buffer.data[0]).cast::<u8>().write(5) };
        buffer.command.set(0x81);
        buffer.flags.write(Flags::CHECKSUM::SET);
        buffer.size.set(9);
        unsafe { answer_with(&buffer, nothing) };
        assert_eq!(buffer.status.get(), OK);
        assert_eq!(buffer.flags.read(Flags::SUM), 9 + 5);
        assert!(buffer.flags.is_set(Flags::READY));
    }
}
