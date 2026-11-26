//! The C2P mailbox (PSP_APOB §3.1).
//!
//! Three dwords at `PSP_BAR + 0x10570`: command/status, buffer address low
//! and high. The x86 side waits for the PSP to be ready, writes the physical
//! address of a command buffer, writes the command id and waits for the PSP
//! to clear it again.
//!
//! Per PSP generation (AGESA, instruction-checked except where noted; RV
//! as the native Picasso flow found it on the Vilboz):
//!
//! | | Naples, Zeppelin (ZP) | Picasso (RV) | Rome (SSP) | Milan (GN) |
//! |---|---|---|---|---|
//! | precheck | wait for bit 29, 30 (or 31); recovery iff only 30 | bit 30 set: recovery | as RV | as RV |
//! | done | `READY` and no command | as ZP | no command | as SSP |
//! | status | `cmd[15:0]`, the header status or a changed header size is an error (AGESA Naples ignores them) | as ZP | copied into the buffer header, `!= 0` is an error | `cmd[15:0] != 0` is an error |
//!
//! The WH PSP (Venice, [`Variant::Wh`]) has its mailbox at `PSP_BAR +
//! 0x1_0970` (SMN `0x0381_0970`): the vendor modules' profile table maps
//! CPUIDs to the C2P base and has no WH row, so they use their default
//! base `0x1_0900` (Dell AmdPspDxeV2Wh `0x26a40`, MD25 `0x16550`). Its
//! sender (Dell AmdPspDxeV2Wh `0x10390`, MD25 `0x9354`, decompiler and
//! instruction-checked IDs) refuses while bit 30 (recovery) or bit 28
//! (an asynchronous command in progress) is set, waits for `READY`, writes
//! `id << 16 | old & 0x7f00_0000` (bits 30:24 kept), waits for
//! `cmd[23:16] == 0` and copies `cmd[15:0]` into the header status;
//! non-zero is an error. Its PEI checks bit 29 (reset required, with bit
//! 30 clear) once `READY` shows and warm-resets ([`Mailbox::reset_required`],
//! AmdPspPeiV2Wh `0xe4fde0`).
//!
//! Every poll checks for an all-ones register and the recovery firmware.
//! [`Session`] owns a mailbox and its buffer for a boot flow.
//!
//! AGESA waits forever in every loop; here each wait is bounded by
//! [`Mailbox::with_timeout`] (default 1 s, PSP_APOB §5).

use core::marker::PhantomData;
use core::sync::atomic::{Ordering, fence};

use fstart_core::services::timer::Timer;
use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{RegisterLongName, register_bitfields};
use zerocopy::IntoBytes;

use super::Variant;
use super::command::{C2pBuffer, Command};
use crate::smn::{Smn, SmnAddr};

register_bitfields![u32,
    /// C2P command/status register (`MP0_C2PMSG_28`).
    pub C2P_CMD [
        /// Command result, valid once `COMMAND` reads 0 again.
        STATUS OFFSET(0) NUMBITS(16) [],
        /// Command id; the PSP clears it when done.
        COMMAND OFFSET(16) NUMBITS(8) [],
        /// WH: bits 30:24, which the vendor's sender writes back with the
        /// command.
        PRESERVED OFFSET(24) NUMBITS(7) [],
        /// WH: an asynchronous command is in progress; the vendor sends
        /// nothing while it is set.
        ASYNC_IN_PROGRESS OFFSET(28) NUMBITS(1) [],
        /// Bit 29, by generation. Naples: AGESA waits for this or
        /// `RECOVERY` before the first command; set in normal mode, meaning
        /// not public (PSP_APOB §7). WH: with `RECOVERY` clear, the PSP asks
        /// for a warm reset before the boot goes on.
        NORMAL_OR_RESET_REQUIRED OFFSET(29) NUMBITS(1) [],
        /// The PSP booted its recovery firmware; no commands are accepted.
        RECOVERY OFFSET(30) NUMBITS(1) [],
        /// The PSP accepts a command.
        READY OFFSET(31) NUMBITS(1) [],
    ]
];

/// Offset of the command register in the PSP BAR.
pub const C2P_MMIO_OFFSET: u64 = 0x1_0570;

/// SMN address of the command register: MP0 `0x380_0000 + 0x1_0500 + 28 * 4`
/// (Oxide's public Milan driver, PSP_APOB §3.1). AGESA never uses it; it is
/// proven for Milan only, Naples and Rome are unproven.
pub const C2P_SMN: SmnAddr = 0x0381_0570;

/// The WH command register in the PSP BAR: `0x1_0900 + 28 * 4` (see the
/// module documentation).
pub const C2P_MMIO_OFFSET_WH: u64 = 0x1_0970;

/// SMN address of the WH command register, which the vendor's SMN path
/// (HSTI) reads (`0x0381_0970/74/78`).
pub const C2P_SMN_WH: SmnAddr = 0x0381_0970;

impl Variant {
    /// Offset of the C2P command register in the PSP BAR.
    pub const fn c2p_mmio_offset(self) -> u64 {
        match self {
            Self::Zp | Self::Rv | Self::Ssp | Self::Gn => C2P_MMIO_OFFSET,
            Self::Wh => C2P_MMIO_OFFSET_WH,
        }
    }

    /// SMN address of the C2P command register.
    pub const fn c2p_smn(self) -> SmnAddr {
        match self {
            Self::Zp | Self::Rv | Self::Ssp | Self::Gn => C2P_SMN,
            Self::Wh => C2P_SMN_WH,
        }
    }
}

/// The three mailbox dwords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum C2pReg {
    /// Command/status ([`C2P_CMD`]).
    Cmd = 0,
    /// Buffer physical address, bits 31:0.
    BufLo = 4,
    /// Buffer physical address, bits 63:32 (AGESA always writes 0).
    BufHi = 8,
}

/// Access to the mailbox dwords; implemented for MMIO and SMN, and by test
/// models.
pub trait C2pTransport {
    fn read(&self, reg: C2pReg) -> u32;
    fn write(&self, reg: C2pReg, value: u32);
}

/// A mailbox dword with the bitfields `R`, for the tock-registers
/// interfaces.
struct Reg<'a, T, R: RegisterLongName = ()> {
    transport: &'a T,
    reg: C2pReg,
    _reg: PhantomData<R>,
}

impl<'a, T, R: RegisterLongName> Reg<'a, T, R> {
    fn new(transport: &'a T, reg: C2pReg) -> Self {
        Self {
            transport,
            reg,
            _reg: PhantomData,
        }
    }
}

impl<T: C2pTransport, R: RegisterLongName> Readable for Reg<'_, T, R> {
    type T = u32;
    type R = R;

    fn get(&self) -> u32 {
        self.transport.read(self.reg)
    }
}

impl<T: C2pTransport, R: RegisterLongName> Writeable for Reg<'_, T, R> {
    type T = u32;
    type R = R;

    fn set(&self, value: u32) {
        self.transport.write(self.reg, value)
    }
}

/// The mailbox through the PSP BAR (the AGESA path).
#[derive(Debug, Clone, Copy)]
pub struct MmioTransport {
    cmd: usize,
}

impl MmioTransport {
    /// The mailbox of the `generation` PSP whose BAR is at `bar` (the
    /// generation places it: [`Variant::c2p_mmio_offset`]).
    ///
    /// # Safety
    /// `bar` is an enabled PSP BAR mapped uncached and identity mapped.
    pub const unsafe fn new(bar: u64, generation: Variant) -> Self {
        Self {
            cmd: (bar + generation.c2p_mmio_offset()) as usize,
        }
    }
}

impl C2pTransport for MmioTransport {
    fn read(&self, reg: C2pReg) -> u32 {
        // SAFETY: the constructor's contract.
        unsafe { fstart_core::mmio::read32((self.cmd + reg as usize) as *const u32) }
    }

    fn write(&self, reg: C2pReg, value: u32) {
        // SAFETY: the constructor's contract.
        unsafe { fstart_core::mmio::write32((self.cmd + reg as usize) as *mut u32, value) }
    }
}

/// The mailbox through SMN, usable before the BAR exists. Proven on Milan
/// only (see [`C2P_SMN`]).
#[derive(Clone, Copy)]
pub struct SmnTransport<P> {
    smn: Smn<P>,
    cmd: SmnAddr,
}

impl<P: ConfigRegionAccess> SmnTransport<P> {
    /// The mailbox of the `generation` PSP of the socket whose SMN `smn`
    /// reaches ([`Variant::c2p_smn`]).
    ///
    /// # Safety
    /// The caller owns the SMN index/data pair while the transport is used.
    pub const unsafe fn new(smn: Smn<P>, generation: Variant) -> Self {
        Self {
            smn,
            cmd: generation.c2p_smn(),
        }
    }
}

impl<P: ConfigRegionAccess> C2pTransport for SmnTransport<P> {
    fn read(&self, reg: C2pReg) -> u32 {
        self.smn.read32(self.cmd + reg as u32)
    }

    fn write(&self, reg: C2pReg, value: u32) {
        self.smn.write32(self.cmd + reg as u32, value)
    }
}

/// Which wait timed out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Naples: waiting for bit 29 or 30 before the first command.
    Boot,
    /// Waiting for `READY` with no command pending.
    Ready,
    /// Waiting for the PSP to clear the command.
    Done,
}

/// Mailbox errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PspError {
    /// The PSP runs its recovery firmware and takes no commands.
    Recovery,
    /// A bounded wait expired (AGESA waits forever).
    Timeout(Wait),
    /// The PSP reported a non-zero status for `command`.
    Status { command: u8, status: u32 },
    /// The command register reads all-ones.
    Unavailable,
    /// No donor of this generation sends `command`; refused (fail closed).
    Unsupported { command: u8, generation: Variant },
    /// The PSP changed the buffer size of `command` (RV, ZP).
    Size { command: u8, size: u32 },
    /// An earlier transaction of the [`Session`] failed: the PSP may still
    /// own its buffer.
    Poisoned,
    /// The [`Session`] sent BootDone.
    Finished,
    /// The [`Session`] storage isn't below 4 GiB.
    Placement,
    /// WH: an asynchronous command is in progress
    /// ([`C2P_CMD::ASYNC_IN_PROGRESS`]); nothing was sent.
    Busy,
}

impl PspError {
    /// Whether the PSP may still own the command's buffer: a wait ended
    /// without the command's completion (a timeout, an aperture that went
    /// away, the recovery firmware). A refused or completed command, or one
    /// never sent, left it.
    pub const fn uncertain(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Unavailable | Self::Recovery)
    }

    /// A name and a number (the status, the command), for logs without
    /// `Debug`.
    pub const fn parts(&self) -> (&'static str, u32) {
        match *self {
            Self::Recovery => ("recovery firmware", 0),
            Self::Timeout(Wait::Boot) => ("boot wait timeout", 0),
            Self::Timeout(Wait::Ready) => ("ready timeout", 0),
            Self::Timeout(Wait::Done) => ("completion timeout", 0),
            Self::Status { status, .. } => ("status", status),
            Self::Unavailable => ("unavailable", 0),
            Self::Unsupported { command, .. } => ("unsupported command", command as u32),
            Self::Size { size, .. } => ("buffer size", size),
            Self::Poisoned => ("poisoned by an earlier failure", 0),
            Self::Finished => ("after BootDone", 0),
            Self::Placement => ("buffer placement", 0),
            Self::Busy => ("asynchronous command in progress", 0),
        }
    }
}

/// Default bound for each wait.
pub const DEFAULT_TIMEOUT_US: u64 = 1_000_000;
const POLL_US: u64 = 10;

/// One PSP's C2P mailbox.
pub struct Mailbox<T> {
    transport: T,
    generation: Variant,
    timeout_us: u64,
}

impl Mailbox<MmioTransport> {
    /// The C2P mailbox of the `generation` PSP whose BAR is at `bar`: the
    /// transport at the generation's offset.
    ///
    /// # Safety
    /// As [`MmioTransport::new`].
    pub const unsafe fn mmio(bar: u64, generation: Variant) -> Self {
        // SAFETY: the caller's contract.
        Self::new(unsafe { MmioTransport::new(bar, generation) }, generation)
    }
}

impl<P: ConfigRegionAccess> Mailbox<SmnTransport<P>> {
    /// The C2P mailbox of the `generation` PSP of the socket `smn` reaches.
    ///
    /// # Safety
    /// As [`SmnTransport::new`].
    pub const unsafe fn smn(smn: Smn<P>, generation: Variant) -> Self {
        // SAFETY: the caller's contract.
        Self::new(unsafe { SmnTransport::new(smn, generation) }, generation)
    }
}

impl<T: C2pTransport> Mailbox<T> {
    /// A mailbox over `transport`, which must be `generation`'s (the
    /// generation places the register: [`Self::mmio`], [`Self::smn`]).
    pub const fn new(transport: T, generation: Variant) -> Self {
        Self {
            transport,
            generation,
            timeout_us: DEFAULT_TIMEOUT_US,
        }
    }

    /// Bound each wait to `timeout_us` microseconds.
    pub const fn with_timeout(mut self, timeout_us: u64) -> Self {
        self.timeout_us = timeout_us;
        self
    }

    pub const fn silicon(&self) -> Variant {
        self.generation
    }

    fn cmd(&self) -> Reg<'_, T, C2P_CMD::Register> {
        Reg::new(&self.transport, C2pReg::Cmd)
    }

    /// The value of the command register once `done` holds, checked on
    /// every poll: all-ones is an unavailable aperture and the recovery
    /// firmware takes no commands.
    fn wait(
        &self,
        timer: &impl Timer,
        what: Wait,
        done: impl Fn(u32) -> bool,
    ) -> Result<u32, PspError> {
        let mut waited = 0;
        loop {
            let value = self.cmd().get();
            if value == u32::MAX {
                return Err(PspError::Unavailable);
            }
            if self.recovery(value) {
                return Err(PspError::Recovery);
            }
            if done(value) {
                return Ok(value);
            }
            if waited >= self.timeout_us {
                return Err(PspError::Timeout(what));
            }
            timer.delay_us(POLL_US);
            waited += POLL_US;
        }
    }

    /// Whether `value` says the PSP runs its recovery firmware: on ZP iff
    /// only bit 30 of bits 29/30 is set, elsewhere bit 30 (PSP_APOB §3.1).
    fn recovery(&self, value: u32) -> bool {
        let recovery = C2P_CMD::RECOVERY.is_set(value);
        match self.generation {
            Variant::Zp => recovery && !C2P_CMD::NORMAL_OR_RESET_REQUIRED.is_set(value),
            Variant::Rv | Variant::Ssp | Variant::Gn | Variant::Wh => recovery,
        }
    }

    /// WH: whether the PSP asks for a warm reset before the boot goes on
    /// (bit 29 with `RECOVERY` clear, once `READY` shows), as the vendor's
    /// PEI checks it (AmdPspPeiV2Wh `0xe4fde0`, then a warm
    /// reset, `0xe4fef0`). Other generations: `Ok(false)`.
    pub fn reset_required(&self, timer: &impl Timer) -> Result<bool, PspError> {
        if self.generation != Variant::Wh {
            return Ok(false);
        }
        // `wait` fails on the recovery firmware, which the vendor treats
        // as no reset.
        match self.wait(timer, Wait::Ready, |v| C2P_CMD::READY.is_set(v)) {
            Ok(value) => Ok(C2P_CMD::NORMAL_OR_RESET_REQUIRED.is_set(value)),
            Err(PspError::Recovery) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The generation's precheck. ZP (`ffe04f85`) spins until bit 29 or 30
    /// is set; a Zeppelin PSP that only shows READY passes as well (the
    /// native MJ11 flow saw nothing else).
    fn precheck(&self, timer: &impl Timer) -> Result<(), PspError> {
        let any = |v| {
            C2P_CMD::NORMAL_OR_RESET_REQUIRED.is_set(v)
                || C2P_CMD::RECOVERY.is_set(v)
                || C2P_CMD::READY.is_set(v)
        };
        match self.generation {
            Variant::Zp => self.wait(timer, Wait::Boot, any),
            Variant::Rv | Variant::Ssp | Variant::Gn => self.wait(timer, Wait::Boot, |_| true),
            // The vendor refuses outright while an asynchronous command
            // runs.
            Variant::Wh => match self.wait(timer, Wait::Boot, |_| true)? {
                v if C2P_CMD::ASYNC_IN_PROGRESS.is_set(v) => Err(PspError::Busy),
                v => Ok(v),
            },
        }
        .map(drop)
    }

    /// Send command `id` with the buffer at physical address `buffer` and
    /// return `cmd[15:0]` after completion. Status policy is the caller's.
    fn transact(&self, timer: &impl Timer, id: u8, buffer: u64) -> Result<u16, PspError> {
        self.precheck(timer)?;
        let ready = |v| C2P_CMD::READY.is_set(v) && C2P_CMD::COMMAND.read(v) == 0;
        self.wait(timer, Wait::Ready, ready)?;
        Reg::<T>::new(&self.transport, C2pReg::BufLo).set(buffer as u32);
        Reg::<T>::new(&self.transport, C2pReg::BufHi).set((buffer >> 32) as u32);
        // The buffer must be in memory before the PSP sees the command.
        fence(Ordering::SeqCst);
        match self.generation {
            // READY is written 0 ("clear the ready bit", GN `ffcf4e08`).
            Variant::Zp | Variant::Rv | Variant::Ssp | Variant::Gn => {
                self.cmd().write(C2P_CMD::COMMAND.val(id.into()))
            }
            // WH: READY and the status written 0, bits 30:24 kept.
            Variant::Wh => {
                let kept = self.cmd().read(C2P_CMD::PRESERVED);
                self.cmd()
                    .write(C2P_CMD::PRESERVED.val(kept) + C2P_CMD::COMMAND.val(id.into()))
            }
        }
        // The native RV/ZP flows also wait for READY again.
        let value = match self.generation {
            Variant::Rv | Variant::Zp => self.wait(timer, Wait::Done, ready)?,
            Variant::Ssp | Variant::Gn | Variant::Wh => {
                self.wait(timer, Wait::Done, |v| C2P_CMD::COMMAND.read(v) == 0)?
            }
        };
        fence(Ordering::SeqCst);
        Ok(C2P_CMD::STATUS.read(value) as u16)
    }

    /// Send the command in `buffer` and apply the generation's status rule:
    /// SSP copies the status into the header (as AGESA PEI does), GN checks
    /// `cmd[15:0]`, and RV and ZP check `cmd[15:0]` and the status and size
    /// the PSP writes into the header (the native flows; AGESA Naples
    /// ignores the status, fail closed here). A non-zero status is an error.
    ///
    /// The buffer is passed by its address: firmware runs identity mapped.
    pub fn send<C: Command>(
        &self,
        timer: &impl Timer,
        buffer: &mut C2pBuffer<C>,
    ) -> Result<(), PspError> {
        if !C::supported(self.generation) {
            return Err(PspError::Unsupported {
                command: C::ID,
                generation: self.generation,
            });
        }
        let address = core::ptr::from_mut(buffer) as u64;
        let size = buffer.header.size;
        let status = self.transact(timer, C::ID, address)?;
        // The PSP may have written the buffer behind the compiler's back.
        let bytes = buffer.header.as_mut_bytes().iter_mut();
        for byte in bytes.chain(buffer.request.as_mut_bytes()) {
            // SAFETY: a byte of the exclusively borrowed buffer.
            *byte = unsafe { fstart_core::mmio::read8(byte) };
        }
        let error = |status: u32| PspError::Status {
            command: C::ID,
            status,
        };
        match self.generation {
            // WH copies the status into the header (`0x10390`).
            Variant::Ssp | Variant::Wh => {
                buffer.header.status = status.into();
                match status {
                    0 => Ok(()),
                    s => Err(error(s.into())),
                }
            }
            Variant::Gn => match status {
                0 => Ok(()),
                s => Err(error(s.into())),
            },
            Variant::Rv | Variant::Zp => match (status, buffer.header.status) {
                (0, 0) if buffer.header.size == size => Ok(()),
                (0, 0) => Err(PspError::Size {
                    command: C::ID,
                    size: buffer.header.size,
                }),
                (0, s) => Err(error(s)),
                (s, _) => Err(error(s.into())),
            },
        }
    }

    /// [`send`](Self::send) from SMM with the SMM flag set around it, as
    /// AGESA does for SxInfo and LockFCHReg (PSP_APOB §3.4: the flag byte is
    /// at `+0xc00` of the C2P SMM buffer page).
    ///
    /// # Safety
    /// `flag` is the SMM flag byte registered with SmmInfo.
    pub unsafe fn send_in_smm<C: Command>(
        &self,
        timer: &impl Timer,
        buffer: &mut C2pBuffer<C>,
        flag: *mut u8,
    ) -> Result<(), PspError> {
        // SAFETY: the caller's contract.
        unsafe { fstart_core::mmio::write8(flag, 1) };
        let result = self.send(timer, buffer);
        // SAFETY: as above.
        unsafe { fstart_core::mmio::write8(flag, 0) };
        result
    }
}

/// The bytes a [`Session`] lends its commands: firmware storage below
/// 4 GiB, retained through reset. Room for the largest command, SmmInfo.
#[repr(C, align(32))]
pub struct Storage(core::mem::MaybeUninit<[u8; STORAGE_BYTES]>);
const STORAGE_BYTES: usize = 0x120;
impl Storage {
    pub const fn new() -> Self {
        Self(core::mem::MaybeUninit::uninit())
    }
}
impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Poisoned,
    Finished,
}

/// The one owner of a PSP's mailbox in a boot flow. Its commands use one
/// retained buffer: a failed transaction may leave the PSP owning it, so
/// the session refuses all later commands, and none follow BootDone.
pub struct Session<T> {
    mailbox: Mailbox<T>,
    storage: &'static mut Storage,
    state: State,
    not_send_sync: PhantomData<*mut ()>,
}

impl<T: C2pTransport> Session<T> {
    /// `storage` is identity mapped and reserved from the OS until reset.
    pub fn new(mailbox: Mailbox<T>, storage: &'static mut Storage) -> Result<Self, PspError> {
        let start = core::ptr::from_mut(storage) as u64;
        // Host tests run above 4 GiB.
        if start == 0 || (cfg!(not(test)) && start + size_of::<Storage>() as u64 > 1 << 32) {
            return Err(PspError::Placement);
        }
        Ok(Self {
            mailbox,
            storage,
            state: State::Ready,
            not_send_sync: PhantomData,
        })
    }

    /// Send `request` and return the buffer as the PSP left it.
    pub fn send<C: Command>(
        &mut self,
        timer: &impl Timer,
        request: C,
    ) -> Result<C2pBuffer<C>, PspError> {
        const {
            assert!(
                size_of::<C2pBuffer<C>>() <= STORAGE_BYTES
                    && align_of::<C2pBuffer<C>>() <= align_of::<Storage>()
            )
        };
        match self.state {
            State::Poisoned => return Err(PspError::Poisoned),
            State::Finished => return Err(PspError::Finished),
            State::Ready => {}
        }
        // Sticky until the transaction completed: the PSP may still read or
        // write the buffer.
        self.state = State::Poisoned;
        let buffer = self.storage.0.as_mut_ptr().cast::<C2pBuffer<C>>();
        // SAFETY: the storage is aligned and large enough (checked above),
        // exclusively ours and no transaction is pending on it.
        let buffer = unsafe {
            buffer.write(C2pBuffer::new(request));
            &mut *buffer
        };
        if C::ID == super::command::BootDone::ID {
            // Never retried, whatever the outcome, unless it was never
            // sent (the WH PSP was busy).
            self.state = State::Finished;
            return self
                .mailbox
                .send(timer, buffer)
                .map(|()| *buffer)
                .inspect_err(|error| {
                    if *error == PspError::Busy {
                        self.state = State::Ready;
                    }
                });
        }
        match self.mailbox.send(timer, buffer) {
            Ok(()) => self.state = State::Ready,
            // Not sent: the buffer stays ours.
            Err(PspError::Busy) => {
                self.state = State::Ready;
                return Err(PspError::Busy);
            }
            Err(error) => return Err(error),
        }
        Ok(*buffer)
    }

    /// [`Self::send`] for a command whose refusal the vendor ignores: an
    /// answer the PSP completed with an error status (or a refusal before
    /// sending) leaves the session usable; only an uncertain completion
    /// ([`PspError::uncertain`]) poisons it.
    pub fn send_unchecked<C: Command>(
        &mut self,
        timer: &impl Timer,
        request: C,
    ) -> Result<C2pBuffer<C>, PspError> {
        let ready = self.state == State::Ready && C::ID != super::command::BootDone::ID;
        let result = self.send(timer, request);
        if let Err(error) = &result
            && ready
            && !error.uncertain()
        {
            self.state = State::Ready;
        }
        result
    }

    /// The generation of its PSP.
    pub const fn silicon(&self) -> Variant {
        self.mailbox.silicon()
    }

    /// Whether BootDone was sent.
    pub fn finished(&self) -> bool {
        self.state == State::Finished
    }
}

#[cfg(test)]
pub(crate) mod model {
    //! A PSP mailbox model: completes each command through a handler.
    use std::cell::RefCell;

    use super::*;

    pub struct Psp {
        pub cmd: u32,
        pub buf: u64,
        /// Commands seen, with the buffer address.
        pub log: Vec<(u8, u64)>,
        /// The raw values written to the command register.
        pub writes: Vec<u32>,
        /// Completion: `(command, buffer) -> status`.
        pub handler: Box<dyn Fn(u8, u64) -> u16>,
        /// Polls until `READY` shows (Naples boot wait / busy PSP).
        pub busy_polls: u32,
        /// Never complete commands.
        pub hang: bool,
    }

    pub struct Model(pub RefCell<Psp>);

    impl Model {
        pub fn new(cmd: u32, handler: impl Fn(u8, u64) -> u16 + 'static) -> Self {
            Self(RefCell::new(Psp {
                cmd,
                buf: 0,
                log: Vec::new(),
                writes: Vec::new(),
                handler: Box::new(handler),
                busy_polls: 0,
                hang: false,
            }))
        }
    }

    impl C2pTransport for &Model {
        fn read(&self, reg: C2pReg) -> u32 {
            let mut p = self.0.borrow_mut();
            match reg {
                C2pReg::Cmd if p.busy_polls > 0 => {
                    p.busy_polls -= 1;
                    0
                }
                C2pReg::Cmd => p.cmd,
                C2pReg::BufLo => p.buf as u32,
                C2pReg::BufHi => (p.buf >> 32) as u32,
            }
        }

        fn write(&self, reg: C2pReg, value: u32) {
            let mut p = self.0.borrow_mut();
            match reg {
                C2pReg::BufLo => p.buf = (p.buf & !0xffff_ffff) | u64::from(value),
                C2pReg::BufHi => p.buf = (p.buf & 0xffff_ffff) | u64::from(value) << 32,
                C2pReg::Cmd => {
                    let id = (value >> 16) as u8;
                    let buf = p.buf;
                    p.log.push((id, buf));
                    p.writes.push(value);
                    p.cmd = if p.hang {
                        value
                    } else {
                        let status = (p.handler)(id, buf);
                        p.cmd & 0x7000_0000 | 0x8000_0000 | u32::from(status)
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::model::Model;
    use super::*;
    use crate::psp::command::{BootDone, DisableDrtm, Header, LockDfReg};
    use fstart_core::services::ManualTimer;

    const READY: u32 = 0x8000_0000;

    #[test]
    fn sends_buffer_and_command() {
        let model = Model::new(READY, |_, _| 0);
        let mbox = Mailbox::new(&model, Variant::Ssp);
        let mut buf = C2pBuffer::new(LockDfReg);
        mbox.send(&ManualTimer::new(), &mut buf).unwrap();
        let addr = core::ptr::from_ref(&buf) as u64;
        assert_eq!(model.0.borrow().log, [(0x1b, addr)]);
        assert_eq!(addr % 32, 0, "AGESA aligns buffers to 32 bytes");
        assert_eq!(buf.header, Header { size: 8, status: 0 });
    }

    #[test]
    fn recovery_is_refused() {
        for generation in [Variant::Ssp, Variant::Gn] {
            let model = Model::new(READY | 1 << 30, |_, _| 0);
            let mbox = Mailbox::new(&model, generation);
            let err = mbox.send(&ManualTimer::new(), &mut C2pBuffer::new(BootDone));
            assert_eq!(err, Err(PspError::Recovery));
            assert!(model.0.borrow().log.is_empty());
        }
        // Naples: recovery iff bit 30 without bit 29.
        let model = Model::new(READY | 1 << 30, |_, _| 0);
        let mbox = Mailbox::new(&model, Variant::Zp);
        assert_eq!(
            mbox.send(&ManualTimer::new(), &mut C2pBuffer::new(BootDone)),
            Err(PspError::Recovery)
        );
        let model = Model::new(READY | 3 << 29, |_, _| 0);
        let mbox = Mailbox::new(&model, Variant::Zp);
        assert_eq!(
            mbox.send(&ManualTimer::new(), &mut C2pBuffer::new(BootDone)),
            Ok(())
        );
    }

    #[test]
    fn naples_waits_for_boot_bits_bounded() {
        let model = Model::new(READY | 1 << 29, |_, _| 0);
        model.0.borrow_mut().busy_polls = 5;
        let mbox = Mailbox::new(&model, Variant::Zp);
        mbox.send(&ManualTimer::new(), &mut C2pBuffer::new(BootDone))
            .unwrap();

        // A Zeppelin PSP showing only READY passes.
        let model = Model::new(READY, |_, _| 0);
        let mbox = Mailbox::new(&model, Variant::Zp);
        mbox.send(&ManualTimer::new(), &mut C2pBuffer::new(BootDone))
            .unwrap();

        // Bits 29-31 never set: bounded.
        let model = Model::new(0, |_, _| 0);
        let mbox = Mailbox::new(&model, Variant::Zp).with_timeout(100);
        assert_eq!(
            mbox.send(&ManualTimer::new(), &mut C2pBuffer::new(BootDone)),
            Err(PspError::Timeout(Wait::Boot))
        );
    }

    #[test]
    fn done_wait_is_bounded() {
        let model = Model::new(READY, |_, _| 0);
        model.0.borrow_mut().hang = true;
        let mbox = Mailbox::new(&model, Variant::Gn).with_timeout(1000);
        assert_eq!(
            mbox.send(&ManualTimer::new(), &mut C2pBuffer::new(BootDone)),
            Err(PspError::Timeout(Wait::Done))
        );
    }

    #[test]
    fn status_rules_per_generation() {
        let failing = |generation| {
            let model = Model::new(READY | 1 << 29, |_, _| 5);
            let mbox = Mailbox::new(&model, generation);
            let mut buf = C2pBuffer::new(LockDfReg);
            (mbox.send(&ManualTimer::new(), &mut buf), buf.header.status)
        };
        let status = Err(PspError::Status {
            command: 0x1b,
            status: 5,
        });
        // AGESA Naples ignores it; fail closed as the native Zeppelin flow.
        assert_eq!(failing(Variant::Zp), (status, 0));
        assert_eq!(failing(Variant::Ssp), (status, 5));
        assert_eq!(failing(Variant::Gn), (status, 0));
    }

    #[test]
    fn a_session_poisons_on_failure_and_ends_with_boot_done() {
        use crate::psp::command::{BootDone, Default32, LockDfReg};
        fn storage() -> &'static mut Storage {
            std::boxed::Box::leak(std::boxed::Box::new(Storage::new()))
        }
        let model = Model::new(READY, |_, _| 0);
        let mut session = Session::new(Mailbox::new(&model, Variant::Rv), storage()).unwrap();
        let buffer = session
            .send(&ManualTimer::new(), Default32::<LockDfReg>::new())
            .unwrap();
        // The native 32-byte default buffer.
        assert_eq!(buffer.header.size, 32);
        session
            .send(&ManualTimer::new(), Default32::<BootDone>::new())
            .unwrap();
        assert!(session.finished());
        assert_eq!(
            session
                .send(&ManualTimer::new(), Default32::<LockDfReg>::new())
                .err(),
            Some(PspError::Finished)
        );
        assert_eq!(model.0.borrow().log.len(), 2);

        let hung = Model::new(READY, |_, _| 0);
        hung.0.borrow_mut().hang = true;
        let mailbox = Mailbox::new(&hung, Variant::Rv).with_timeout(100);
        let mut session = Session::new(mailbox, storage()).unwrap();
        assert_eq!(
            session
                .send(&ManualTimer::new(), Default32::<LockDfReg>::new())
                .err(),
            Some(PspError::Timeout(Wait::Done))
        );
        assert_eq!(
            session
                .send(&ManualTimer::new(), Default32::<LockDfReg>::new())
                .err(),
            Some(PspError::Poisoned)
        );
    }

    #[test]
    fn recovery_or_an_unavailable_aperture_during_completion_fails() {
        // Recovery firmware shows no pending command, which a completion
        // wait on COMMAND alone would take as done.
        for (value, error) in [
            (1 << 30, PspError::Recovery),
            (u32::MAX, PspError::Unavailable),
        ] {
            let model = Model::new(value, |_, _| 0);
            for variant in [Variant::Rv, Variant::Ssp, Variant::Gn] {
                let mbox = Mailbox::new(&model, variant);
                let done = |v| C2P_CMD::COMMAND.read(v) == 0;
                assert_eq!(mbox.wait(&ManualTimer::new(), Wait::Done, done), Err(error));
            }
        }
    }

    #[test]
    fn rv_checks_the_command_and_the_header_status() {
        let send = |cmd, header_status: u32| {
            let model = Model::new(cmd, move |_, buffer| {
                // The PSP writes the header status of the buffer it got.
                unsafe { fstart_core::mmio::write32((buffer as *mut u32).add(1), header_status) };
                0
            });
            let mbox = Mailbox::new(&model, Variant::Rv);
            mbox.send(&ManualTimer::new(), &mut C2pBuffer::new(LockDfReg))
        };
        assert_eq!(send(READY, 0), Ok(()));
        assert_eq!(
            send(READY, 7),
            Err(PspError::Status {
                command: 0x1b,
                status: 7
            })
        );
        assert_eq!(send(u32::MAX, 0), Err(PspError::Unavailable));
        assert_eq!(send(READY | 1 << 30, 0), Err(PspError::Recovery));
    }

    #[test]
    fn milan_only_commands_fail_closed() {
        let model = Model::new(READY, |_, _| 0);
        let mbox = Mailbox::new(&model, Variant::Ssp);
        assert_eq!(
            mbox.send(&ManualTimer::new(), &mut C2pBuffer::new(DisableDrtm::new())),
            Err(PspError::Unsupported {
                command: 0x62,
                generation: Variant::Ssp
            })
        );
    }

    #[test]
    fn wh_mailbox_is_at_0x10970() {
        assert_eq!(Variant::Wh.c2p_mmio_offset(), 0x1_0970);
        assert_eq!(Variant::Wh.c2p_smn(), 0x0381_0970);
        assert_eq!(Variant::Gn.c2p_mmio_offset(), C2P_MMIO_OFFSET);
        // SAFETY: never dereferenced.
        let (wh, gn) = unsafe {
            (
                Mailbox::mmio(0xd000_0000, Variant::Wh),
                Mailbox::mmio(0xd000_0000, Variant::Gn),
            )
        };
        assert_eq!(
            (wh.transport.cmd, gn.transport.cmd),
            (0xd001_0970, 0xd001_0570)
        );
    }

    #[test]
    fn wh_keeps_bits_30_24_and_copies_the_status() {
        // Bit 26 is one of the kept bits the vendor writes back.
        let model = Model::new(READY | 1 << 26, |_, _| 0);
        let mbox = Mailbox::new(&model, Variant::Wh);
        let mut buf = C2pBuffer::new(LockDfReg);
        mbox.send(&ManualTimer::new(), &mut buf).unwrap();
        assert_eq!(model.0.borrow().writes, [1 << 26 | 0x1b << 16]);

        let model = Model::new(READY, |_, _| 7);
        let mbox = Mailbox::new(&model, Variant::Wh);
        let mut buf = C2pBuffer::new(LockDfReg);
        assert_eq!(
            mbox.send(&ManualTimer::new(), &mut buf),
            Err(PspError::Status {
                command: 0x1b,
                status: 7
            })
        );
        assert_eq!(buf.header.status, 7);
    }

    #[test]
    fn wh_sends_nothing_while_an_async_command_runs() {
        fn storage() -> &'static mut Storage {
            std::boxed::Box::leak(std::boxed::Box::new(Storage::new()))
        }
        let model = Model::new(READY | 1 << 28, |_, _| 0);
        let mut session = Session::new(Mailbox::new(&model, Variant::Wh), storage()).unwrap();
        let timer = ManualTimer::new();
        assert_eq!(session.send(&timer, LockDfReg).err(), Some(PspError::Busy));
        assert_eq!(session.send(&timer, BootDone).err(), Some(PspError::Busy));
        assert!(model.0.borrow().log.is_empty());
        assert!(!PspError::Busy.uncertain());
        // Not sent, so the session goes on once the PSP is done.
        model.0.borrow_mut().cmd = READY;
        session.send(&timer, LockDfReg).unwrap();
        // Bit 28 means nothing to the other generations.
        let model = Model::new(READY | 1 << 28, |_, _| 0);
        let mbox = Mailbox::new(&model, Variant::Gn);
        mbox.send(&ManualTimer::new(), &mut C2pBuffer::new(LockDfReg))
            .unwrap();
    }

    #[test]
    fn wh_reset_required_is_bit_29_without_recovery() {
        let timer = ManualTimer::new();
        let reset =
            |cmd| Mailbox::new(&Model::new(cmd, |_, _| 0), Variant::Wh).reset_required(&timer);
        assert_eq!(reset(READY | 1 << 29), Ok(true));
        assert_eq!(reset(READY), Ok(false));
        assert_eq!(reset(READY | 3 << 29), Ok(false));
        assert_eq!(reset(u32::MAX), Err(PspError::Unavailable));
        let model = Model::new(READY | 1 << 29, |_, _| 0);
        assert_eq!(
            Mailbox::new(&model, Variant::Gn).reset_required(&timer),
            Ok(false)
        );
    }

    #[test]
    fn an_unchecked_refusal_leaves_the_session_usable() {
        use crate::psp::command::Default32;
        let model = Model::new(READY, |id, _| if id == 0x1b { 5 } else { 0 });
        let storage = std::boxed::Box::leak(std::boxed::Box::new(Storage::new()));
        let mut session = Session::new(Mailbox::new(&model, Variant::Wh), storage).unwrap();
        let timer = ManualTimer::new();
        assert!(session.send_unchecked(&timer, LockDfReg).is_err());
        session.send(&timer, Default32::<BootDone>::new()).unwrap();
        // A poisoned session stays poisoned.
        let hung = Model::new(READY, |_, _| 0);
        hung.0.borrow_mut().hang = true;
        let storage = std::boxed::Box::leak(std::boxed::Box::new(Storage::new()));
        let mut session =
            Session::new(Mailbox::new(&hung, Variant::Wh).with_timeout(100), storage).unwrap();
        assert!(session.send_unchecked(&timer, LockDfReg).is_err());
        assert_eq!(
            session.send_unchecked(&timer, LockDfReg).err(),
            Some(PspError::Poisoned)
        );
    }

    #[test]
    fn smn_transport_addresses() {
        use crate::smn::model::Model as Pci;
        let pci = Pci::default();
        pci.0.borrow_mut().smn.insert((0, C2P_SMN), READY);
        let transport = unsafe { SmnTransport::new(Smn::on_bus(pci.clone(), 0), Variant::Gn) };
        transport.write(C2pReg::BufLo, 0x1234_5000);
        transport.write(C2pReg::BufHi, 0);
        assert_eq!(transport.read(C2pReg::Cmd), READY);
        let s = pci.0.borrow();
        assert_eq!(s.smn[&(0, 0x0381_0574)], 0x1234_5000);
        assert_eq!(s.smn[&(0, 0x0381_0578)], 0);
    }
}
