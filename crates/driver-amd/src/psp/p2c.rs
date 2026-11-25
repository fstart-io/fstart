// SPDX-License-Identifier: MIT OR Apache-2.0
//! The P2C (PSP to CPU) mailbox an SMM handler answers once SmmInfo
//! registered its buffer ([`super::command::SmmInfo::p2c_buffer`],
//! PSP_APOB §3.4). The PSP raises its SMI for each request (SmmInfo's
//! `trigger`: SmiTrig0\[25\]); the handler answers it and drops that
//! trigger again, or the source stays asserted and the FCH raises SMIs
//! forever.
//!
//! The buffer: dword 0 the PSP's command, dword 1 flags, then the command
//! buffer `{size, status, data}`. Layout and answers are AGESA's
//! (AmdPspP2CmboxV2Ssp `0x720`, Rome a9724bd1, instruction-checked); the
//! RS donors register the same 2000h-byte buffer with the same header
//! (sp5 `reports/PSP_SMM_ABI.md` §2). Neither the SPI NV, the RPMC nor
//! the ARS service is implemented: they get the donors' RPMC answer,
//! "unsupported".
//!
//! The WH PSP's handler (Dell AmdPspP2CmboxV2Mcm `0x1710`, check
//! `0x1650`, dispatch `0x3df0`; decompiler) knows 0x81–0x86, 0x88–0x8b
//! and 0x8d (0x8b an asynchronous request, sub-command 70h the runtime
//! PPR, 0x8d a PPR entry update), takes a command buffer up to 1FF0h
//! bytes and checks the PSP's checksum before it dispatches (2 if it is
//! wrong); a service that fails answers 4 ([`answer_of`]).

use fstart_core::mmio::{read8, read32, write32};

use super::Variant;

/// Dword indices of the buffer.
pub const COMMAND: usize = 0;
pub const FLAGS: usize = 1;
pub const SIZE: usize = 2;
pub const STATUS: usize = 3;
/// x86 is ready for the next request. Every donor sets it with COMMAND = 0
/// before SmmInfo (Naples d8a42c95 `0xbef`, Rome a9724bd1 `0xcef`, the RS
/// donors' registration), clears it while it works and sets it again when
/// done.
pub const READY: u32 = 1 << 31;
/// The PSP asks for a byte-sum checksum in FLAGS\[7:0\].
pub const CHECKSUM: u32 = 1 << 8;
/// Largest command buffer that stays inside the P2C buffer (the donor
/// accepts 0x2000, 8 bytes more than it has).
pub const MAX_SIZE: u32 = super::command::P2C_BUFFER_SIZE as u32 - 8;
/// Statuses the donor writes: done, invalid request, and what its RPMC
/// services return ("unsupported").
pub const OK: u32 = 0;
pub const INVALID: u32 = 1;
pub const UNSUPPORTED: u32 = 4;
/// WH: the PSP's checksum over the command buffer does not match.
pub const CHECKSUM_MISMATCH: u32 = 2;
/// WH: the largest command buffer its handler takes (`0x1650`).
pub const MAX_SIZE_WH: u32 = 0x1ff0;

/// Idle and ready, before SmmInfo registers the buffer.
///
/// # Safety
/// `buffer` is the [`super::command::P2C_BUFFER_SIZE`]-byte P2C buffer in
/// SMRAM, owned by the caller.
#[inline(always)]
pub unsafe fn ready(buffer: *mut u32) {
    // SAFETY: the caller's contract.
    unsafe {
        write32(buffer.add(COMMAND), 0);
        write32(buffer.add(FLAGS), READY);
    }
}

/// Answer the PSP's request: busy, status, checksum when asked, write
/// back, then `acknowledge` the PSP's SMI. 0x81/0x82 need nothing from
/// x86; SPI NV (0x83–0x86), RPMC (0x88/0x89) and ARS (0x8a) get
/// "unsupported", anything else "invalid". Returns the command. The
/// buffer stays busy: the caller ends the SMI, then [`release`]s it.
///
/// The order is the protocol's: the PSP sends its next request, and sets
/// its trigger (SmiTrig0\[25\]) again, once it sees READY with command 0.
/// Every host write to SmiTrig0 must come before that: the acknowledgement
/// that drops the trigger, and the end of the SMI, a read-modify-write of
/// the same register (`fch::smi::eos`), which would write back a trigger
/// the PSP set between its read and its write as 0, and the next
/// request's SMI would never come. AGESA's mailbox sets READY first and
/// drops the trigger after it (AmdPspP2CmboxV2Ssp `0x944`), then ends the
/// SMI: it has both races. The PSP reads only the buffer, so when the
/// trigger dropped, before or after READY, is not its to see.
///
/// `writeback` makes the answer visible to the PSP, which reads DRAM
/// (`fstart_arch::x86::writeback_invalidate_caches` in SMM).
///
/// # Safety
/// As [`ready`]; the PSP only writes the buffer while READY is set;
/// `writeback` is safe to call here.
#[inline(always)]
pub unsafe fn answer(buffer: *mut u32, writeback: unsafe fn(), acknowledge: impl FnOnce()) -> u32 {
    // SAFETY: the caller's contract.
    unsafe { answer_of(Variant::Gn, buffer, writeback, acknowledge) }
}

/// The byte sum of the command buffer's `size` bytes.
///
/// # Safety
/// As [`ready`], `size` checked against the buffer.
#[inline(always)]
unsafe fn checksum(buffer: *mut u32, size: u32) -> u8 {
    // SAFETY: the caller's contract.
    unsafe {
        let bytes = buffer.add(SIZE).cast::<u8>();
        (0..size as usize)
            .map(|i| read8(bytes.add(i)))
            .fold(0u8, u8::wrapping_add)
    }
}

/// [`answer`] by the rules of the `generation` PSP's handler: WH's (see
/// the module documentation), the others' as [`answer`].
///
/// # Safety
/// As [`answer`].
#[inline(always)]
pub unsafe fn answer_of(
    generation: Variant,
    buffer: *mut u32,
    writeback: unsafe fn(),
    acknowledge: impl FnOnce(),
) -> u32 {
    // SAFETY: the caller's contract.
    unsafe {
        let flags = read32(buffer.add(FLAGS)) & !READY;
        write32(buffer.add(FLAGS), flags);
        let size = read32(buffer.add(SIZE));
        let command = read32(buffer.add(COMMAND));
        let status = match generation {
            Variant::Wh => match command {
                0x81..=0x86 | 0x88..=0x8b | 0x8d if size > MAX_SIZE_WH => INVALID,
                0x81..=0x86 | 0x88..=0x8b | 0x8d
                    if flags & CHECKSUM != 0 && checksum(buffer, size) != flags as u8 =>
                {
                    CHECKSUM_MISMATCH
                }
                0x81 | 0x82 => OK,
                0x83..=0x86 | 0x88..=0x8b | 0x8d => UNSUPPORTED,
                _ => INVALID,
            },
            Variant::Zp | Variant::Rv | Variant::Ssp | Variant::Gn => match command {
                _ if size > MAX_SIZE => INVALID,
                0x81 | 0x82 => OK,
                0x83..=0x86 | 0x88..=0x8a => UNSUPPORTED,
                _ => INVALID,
            },
        };
        write32(buffer.add(STATUS), status);
        if status == OK && flags & CHECKSUM != 0 {
            let sum = checksum(buffer, size);
            write32(buffer.add(FLAGS), flags & !0xff | u32::from(sum));
        }
        writeback();
        acknowledge();
        command
    }
}

/// Command 0 and READY: the PSP may send its next request. After the SMI
/// that [`answer`]ed ended; no host write to SmiTrig0 may follow.
///
/// # Safety
/// As [`answer`].
#[inline(always)]
pub unsafe fn release(buffer: *mut u32, writeback: unsafe fn()) {
    // SAFETY: the caller's contract.
    unsafe {
        write32(buffer.add(COMMAND), 0);
        let flags = read32(buffer.add(FLAGS));
        write32(buffer.add(FLAGS), flags | READY);
        writeback();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::Mem;
    use crate::fch::smi::{self, Pending, source};
    use crate::window::Window;
    use std::cell::Cell;

    unsafe fn cached() {}

    /// The PSP: it raises its SMI by setting SmiTrig0\[25\], which latches
    /// the SMI status, and sends its next request (0x82, once) as soon as
    /// it sees READY with command 0: at the worst moment, right after the
    /// host read SmiTrig0 and before it writes it back.
    struct Psp {
        acpi: Mem,
        buffer: *mut u32,
        next: Cell<bool>,
    }
    impl Psp {
        fn request(&self, command: u32) {
            unsafe {
                write32(self.buffer.add(COMMAND), command);
                write32(self.buffer.add(SIZE), 8);
            }
            let trigger = self.acpi.get(smi::TRIGGER0.offset, 4);
            self.acpi.put(
                smi::TRIGGER0.offset,
                4,
                trigger | smi::Trigger0::PSP::SET.value,
            );
            let status = source::PSP.status().offset;
            self.acpi
                .put(status, 4, self.acpi.get(status, 4) | source::PSP.mask());
            self.acpi
                .put(smi::POINTER.offset, 2, (1 << source::PSP.reg) << 1);
        }
        fn poll(&self) {
            let (command, flags) = unsafe {
                (
                    read32(self.buffer.add(COMMAND)),
                    read32(self.buffer.add(FLAGS)),
                )
            };
            if command == 0 && flags & READY != 0 && self.next.replace(false) {
                self.request(0x82);
            }
        }
    }

    /// The FCH's ACPI registers as the host sees them, the PSP acting
    /// after each read of SmiTrig0.
    struct Acpi<'a>(&'a Psp);
    impl Window for Acpi<'_> {
        fn read32(&self, off: u32) -> u32 {
            let value = self.0.acpi.read32(off);
            if off == smi::TRIGGER0.offset {
                self.0.poll();
            }
            value
        }
        fn write32(&self, off: u32, value: u32) {
            self.0.acpi.write32(off, value)
        }
    }

    /// Two requests, the second sent as soon as READY says so, with the
    /// SMI ended (EOS) before or after READY. Whether the second
    /// request's trigger and SMI status are left.
    fn second_request_kept(ready_before_eos: bool) -> bool {
        let mut buffer = [0u32; 16];
        let psp = Psp {
            acpi: Mem::default(),
            buffer: buffer.as_mut_ptr(),
            next: Cell::new(true),
        };
        let acpi = Acpi(&psp);
        unsafe { ready(buffer.as_mut_ptr()) };
        psp.request(0x81);
        let pending = Pending::take(&acpi);
        assert!(pending.has(source::PSP));
        pending.acknowledged(source::PSP);
        let command =
            unsafe { answer(buffer.as_mut_ptr(), cached, || smi::acknowledge_psp(&acpi)) };
        assert_eq!((command, buffer[STATUS]), (0x81, OK));
        if ready_before_eos {
            unsafe { release(buffer.as_mut_ptr(), cached) };
            pending.finish(&acpi);
        } else {
            pending.finish(&acpi);
            unsafe { release(buffer.as_mut_ptr(), cached) };
            // Not before READY: the PSP sends it now.
            assert!(psp.next.get());
            psp.poll();
        }
        assert_eq!(buffer[COMMAND], 0x82);
        let trigger = smi::Trigger0::PSP::SET.value;
        let status = source::PSP.status().offset;
        psp.acpi.get(smi::TRIGGER0.offset, 4) & trigger == trigger
            && psp.acpi.get(status, 4) & source::PSP.mask() != 0
    }

    /// READY after the SMI ended: no host write to SmiTrig0 follows it, and
    /// the next request keeps its trigger. READY before, as AGESA sets it,
    /// and a request that comes while EOS reads and writes SmiTrig0 back
    /// loses its trigger: its SMI never comes.
    #[test]
    fn a_request_sent_right_after_ready_keeps_its_smi() {
        assert!(second_request_kept(false));
        assert!(!second_request_kept(true));
    }

    /// The second request, kept, is answered by the next SMI; then nothing
    /// is left pending.
    #[test]
    fn back_to_back_requests_are_both_answered() {
        let mut buffer = [0u32; 16];
        let psp = Psp {
            acpi: Mem::default(),
            buffer: buffer.as_mut_ptr(),
            next: Cell::new(true),
        };
        let acpi = Acpi(&psp);
        unsafe { ready(buffer.as_mut_ptr()) };
        psp.request(0x81);
        for expected in [0x81, 0x82] {
            let pending = Pending::take(&acpi);
            assert!(pending.has(source::PSP));
            pending.acknowledged(source::PSP);
            let command =
                unsafe { answer(buffer.as_mut_ptr(), cached, || smi::acknowledge_psp(&acpi)) };
            pending.finish(&acpi);
            unsafe { release(buffer.as_mut_ptr(), cached) };
            psp.poll();
            assert_eq!(command, expected);
        }
        let trigger = smi::Trigger0::PSP::SET.value;
        assert_eq!(psp.acpi.get(smi::TRIGGER0.offset, 4) & trigger, 0);
        assert_eq!(buffer[COMMAND], 0);
        assert_eq!(buffer[FLAGS] & READY, READY);
    }

    #[test]
    fn wh_rules() {
        let wh = |command, size, flags, data: u32| {
            let mut buffer = [0u32; 16];
            buffer[COMMAND] = command;
            buffer[FLAGS] = READY | flags;
            buffer[SIZE] = size;
            buffer[4] = data;
            unsafe { answer_of(Variant::Wh, buffer.as_mut_ptr(), cached, || {}) };
            (buffer[STATUS], buffer[FLAGS])
        };
        // Known, its service not implemented: a failed service.
        for command in [0x83, 0x8a, 0x8b, 0x8d] {
            assert_eq!(wh(command, 12, 0, 0).0, UNSUPPORTED);
        }
        assert_eq!(wh(0x8c, 12, 0, 0).0, INVALID);
        assert_eq!(wh(0x81, MAX_SIZE_WH + 1, 0, 0).0, INVALID);
        assert_eq!(wh(0x81, MAX_SIZE_WH, 0, 0).0, OK);
        // The PSP's checksum: size 12 = the size and status dwords and
        // one data dword.
        let sum = (12u32 + 0x0102_0304)
            .to_le_bytes()
            .iter()
            .fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(wh(0x82, 12, CHECKSUM | u32::from(sum), 0x0102_0304).0, OK);
        assert_eq!(
            wh(0x82, 12, CHECKSUM | u32::from(sum ^ 1), 0x0102_0304).0,
            CHECKSUM_MISMATCH
        );
        // The older rules are unchanged: 8Bh is invalid there.
        let mut buffer = [0u32; 16];
        buffer[COMMAND] = 0x8b;
        buffer[SIZE] = 8;
        unsafe { answer(buffer.as_mut_ptr(), cached, || {}) };
        assert_eq!(buffer[STATUS], INVALID);
    }

    #[test]
    fn rpmc_is_unsupported_and_ready_returns() {
        let mut buffer = [0u32; 16];
        unsafe { ready(buffer.as_mut_ptr()) };
        assert_eq!(buffer[FLAGS], READY);
        buffer[COMMAND] = 0x88;
        buffer[SIZE] = 8;
        assert_eq!(unsafe { answer(buffer.as_mut_ptr(), cached, || {}) }, 0x88);
        assert_eq!(buffer[STATUS], UNSUPPORTED);
        // Busy until released.
        assert_eq!((buffer[COMMAND], buffer[FLAGS] & READY), (0x88, 0));
        unsafe { release(buffer.as_mut_ptr(), cached) };
        assert_eq!((buffer[COMMAND], buffer[FLAGS] & READY), (0, READY));
        buffer[COMMAND] = 0x99;
        unsafe { answer(buffer.as_mut_ptr(), cached, || {}) };
        assert_eq!(buffer[STATUS], INVALID);
    }

    #[test]
    fn a_checksum_sums_the_command_buffer() {
        let mut buffer = [0u32; 16];
        buffer[COMMAND] = 0x81;
        buffer[FLAGS] = READY | CHECKSUM;
        buffer[SIZE] = 12;
        buffer[4] = 0x0102_0304;
        unsafe { answer(buffer.as_mut_ptr(), cached, || {}) };
        // 12 + status 0 + 1 + 2 + 3 + 4.
        assert_eq!(buffer[FLAGS], CHECKSUM | 22);
        buffer[COMMAND] = 0x81;
        buffer[SIZE] = MAX_SIZE + 1;
        unsafe { answer(buffer.as_mut_ptr(), cached, || {}) };
        assert_eq!(buffer[STATUS], INVALID);
    }
}
