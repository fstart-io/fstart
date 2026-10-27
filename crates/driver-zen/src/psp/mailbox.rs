//! The C2P mailbox (PSP_APOB §3.1).
//!
//! Three dwords at `PSP_BAR + 0x10570`: command/status, buffer address low
//! and high. The x86 side waits for the PSP to be ready, writes the physical
//! address of a command buffer, writes the command id and waits for the PSP
//! to clear it again.
//!
//! Per generation (AGESA, instruction-checked except where noted):
//!
//! | | Naples (ZP) | Rome (SSP) | Milan (GN) |
//! |---|---|---|---|
//! | precheck | wait for bit 29 or 30; recovery iff only 30 | bit 30 set: recovery | as Rome |
//! | status | not checked | copied into the buffer header | `cmd[15:0] != 0` is an error |
//!
//! AGESA waits forever in every loop; here each wait is bounded by
//! [`Mailbox::with_timeout`] (default 1 s, PSP_APOB §5).

use core::marker::PhantomData;
use core::sync::atomic::{Ordering, fence};

use fstart_core::services::timer::Timer;
use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{RegisterLongName, register_bitfields};

use super::command::{C2pBuffer, Command};
use crate::smn::{Smn, SmnAddr};
use crate::soc::Generation;

register_bitfields![u32,
    /// C2P command/status register (`MP0_C2PMSG_28`).
    pub C2P_CMD [
        /// Command result, valid once `COMMAND` reads 0 again.
        STATUS OFFSET(0) NUMBITS(16) [],
        /// Command id; the PSP clears it when done.
        COMMAND OFFSET(16) NUMBITS(8) [],
        /// Naples: AGESA waits for this or `RECOVERY` before the first
        /// command; set in normal mode, meaning not public (PSP_APOB §7).
        NORMAL OFFSET(29) NUMBITS(1) [],
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
    /// The mailbox of the PSP whose BAR is at `bar`.
    ///
    /// # Safety
    /// `bar` is an enabled PSP BAR mapped uncached and identity mapped.
    pub const unsafe fn new(bar: u64) -> Self {
        Self {
            cmd: (bar + C2P_MMIO_OFFSET) as usize,
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
}

impl<P: ConfigRegionAccess> SmnTransport<P> {
    /// The mailbox of the socket whose SMN `smn` reaches.
    ///
    /// # Safety
    /// The caller owns the SMN index/data pair while the transport is used.
    pub const unsafe fn new(smn: Smn<P>) -> Self {
        Self { smn }
    }
}

impl<P: ConfigRegionAccess> C2pTransport for SmnTransport<P> {
    fn read(&self, reg: C2pReg) -> u32 {
        // SAFETY: the constructor's contract.
        unsafe { self.smn.read32(C2P_SMN + reg as u32) }
    }

    fn write(&self, reg: C2pReg, value: u32) {
        // SAFETY: the constructor's contract.
        unsafe { self.smn.write32(C2P_SMN + reg as u32, value) }
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
    /// No donor of this generation sends `command`; refused (fail closed).
    Unsupported { command: u8, generation: Generation },
}

/// Default bound for each wait.
pub const DEFAULT_TIMEOUT_US: u64 = 1_000_000;
const POLL_US: u64 = 10;

/// One PSP's C2P mailbox.
pub struct Mailbox<T> {
    transport: T,
    generation: Generation,
    timeout_us: u64,
}

impl<T: C2pTransport> Mailbox<T> {
    pub const fn new(transport: T, generation: Generation) -> Self {
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

    pub const fn generation(&self) -> Generation {
        self.generation
    }

    fn cmd(&self) -> Reg<'_, T, C2P_CMD::Register> {
        Reg::new(&self.transport, C2pReg::Cmd)
    }

    fn wait(
        &self,
        timer: &impl Timer,
        what: Wait,
        done: impl Fn(u32) -> bool,
    ) -> Result<u32, PspError> {
        let mut waited = 0;
        loop {
            let value = self.cmd().get();
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

    /// The generation's precheck: `Err(Recovery)` if the PSP runs recovery
    /// firmware (PSP_APOB §3.1).
    fn precheck(&self, timer: &impl Timer) -> Result<(), PspError> {
        let value = match self.generation {
            // ZP (`ffe04f85`): spin until bit 29 or 30, recovery iff the
            // masked value is exactly bit 30.
            Generation::Naples => self.wait(timer, Wait::Boot, |v| {
                C2P_CMD::NORMAL.is_set(v) || C2P_CMD::RECOVERY.is_set(v)
            })?,
            Generation::Rome | Generation::Milan => self.cmd().get(),
        };
        let recovery = C2P_CMD::RECOVERY.is_set(value);
        let normal = C2P_CMD::NORMAL.is_set(value);
        match self.generation {
            Generation::Naples if recovery && !normal => Err(PspError::Recovery),
            Generation::Rome | Generation::Milan if recovery => Err(PspError::Recovery),
            _ => Ok(()),
        }
    }

    /// Send command `id` with the buffer at physical address `buffer` and
    /// return `cmd[15:0]` after completion. Status policy is the caller's.
    fn transact(&self, timer: &impl Timer, id: u8, buffer: u64) -> Result<u16, PspError> {
        self.precheck(timer)?;
        self.wait(timer, Wait::Ready, |v| {
            C2P_CMD::READY.is_set(v) && C2P_CMD::COMMAND.read(v) == 0
        })?;
        Reg::<T>::new(&self.transport, C2pReg::BufLo).set(buffer as u32);
        Reg::<T>::new(&self.transport, C2pReg::BufHi).set((buffer >> 32) as u32);
        // The buffer must be in memory before the PSP sees the command.
        fence(Ordering::SeqCst);
        // READY is written 0 ("clear the ready bit", GN `ffcf4e08`).
        self.cmd().write(C2P_CMD::COMMAND.val(id.into()));
        let value = self.wait(timer, Wait::Done, |v| C2P_CMD::COMMAND.read(v) == 0)?;
        fence(Ordering::SeqCst);
        Ok(C2P_CMD::STATUS.read(value) as u16)
    }

    /// Send the command in `buffer` and apply the generation's status rule:
    /// Naples ignores the status, Rome copies it into the header (as AGESA
    /// PEI does) and Milan checks `cmd[15:0]`; a non-zero status is an error
    /// on Rome and Milan.
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
        let status = self.transact(timer, C::ID, address)?;
        // SAFETY: `buffer` is a valid, exclusively borrowed value; the PSP
        // may have written it behind the compiler's back.
        *buffer = unsafe { core::ptr::read_volatile(buffer) };
        let error = |status: u32| PspError::Status {
            command: C::ID,
            status,
        };
        match self.generation {
            Generation::Naples => Ok(()),
            Generation::Rome => {
                buffer.header.status = status.into();
                match status {
                    0 => Ok(()),
                    s => Err(error(s.into())),
                }
            }
            Generation::Milan => match status {
                0 => Ok(()),
                s => Err(error(s.into())),
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
        unsafe { flag.write_volatile(1) };
        let result = self.send(timer, buffer);
        // SAFETY: as above.
        unsafe { flag.write_volatile(0) };
        result
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
                    p.cmd = if p.hang {
                        value
                    } else {
                        let status = (p.handler)(id, buf);
                        p.cmd & 0x6000_0000 | 0x8000_0000 | u32::from(status)
                    };
                }
            }
        }
    }

    pub struct NoDelay;

    impl Timer for NoDelay {
        fn delay_us(&self, _: u64) {}
        fn timestamp_us(&self) -> u64 {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::model::{Model, NoDelay};
    use super::*;
    use crate::psp::command::{BootDone, DisableDrtm, Header, LockDfReg};

    const READY: u32 = 0x8000_0000;

    #[test]
    fn sends_buffer_and_command() {
        let model = Model::new(READY, |_, _| 0);
        let mbox = Mailbox::new(&model, Generation::Rome);
        let mut buf = C2pBuffer::new(LockDfReg);
        mbox.send(&NoDelay, &mut buf).unwrap();
        let addr = core::ptr::from_ref(&buf) as u64;
        assert_eq!(model.0.borrow().log, [(0x1b, addr)]);
        assert_eq!(addr % 32, 0, "AGESA aligns buffers to 32 bytes");
        assert_eq!(buf.header, Header { size: 8, status: 0 });
    }

    #[test]
    fn recovery_is_refused() {
        for generation in [Generation::Rome, Generation::Milan] {
            let model = Model::new(READY | 1 << 30, |_, _| 0);
            let mbox = Mailbox::new(&model, generation);
            let err = mbox.send(&NoDelay, &mut C2pBuffer::new(BootDone));
            assert_eq!(err, Err(PspError::Recovery));
            assert!(model.0.borrow().log.is_empty());
        }
        // Naples: recovery iff bit 30 without bit 29.
        let model = Model::new(READY | 1 << 30, |_, _| 0);
        let mbox = Mailbox::new(&model, Generation::Naples);
        assert_eq!(
            mbox.send(&NoDelay, &mut C2pBuffer::new(BootDone)),
            Err(PspError::Recovery)
        );
        let model = Model::new(READY | 3 << 29, |_, _| 0);
        let mbox = Mailbox::new(&model, Generation::Naples);
        assert_eq!(mbox.send(&NoDelay, &mut C2pBuffer::new(BootDone)), Ok(()));
    }

    #[test]
    fn naples_waits_for_boot_bits_bounded() {
        let model = Model::new(READY | 1 << 29, |_, _| 0);
        model.0.borrow_mut().busy_polls = 5;
        let mbox = Mailbox::new(&model, Generation::Naples);
        mbox.send(&NoDelay, &mut C2pBuffer::new(BootDone)).unwrap();

        // Bit 29/30 never set: bounded.
        let model = Model::new(READY, |_, _| 0);
        let mbox = Mailbox::new(&model, Generation::Naples).with_timeout(100);
        assert_eq!(
            mbox.send(&NoDelay, &mut C2pBuffer::new(BootDone)),
            Err(PspError::Timeout(Wait::Boot))
        );
    }

    #[test]
    fn done_wait_is_bounded() {
        let model = Model::new(READY, |_, _| 0);
        model.0.borrow_mut().hang = true;
        let mbox = Mailbox::new(&model, Generation::Milan).with_timeout(1000);
        assert_eq!(
            mbox.send(&NoDelay, &mut C2pBuffer::new(BootDone)),
            Err(PspError::Timeout(Wait::Done))
        );
    }

    #[test]
    fn status_rules_per_generation() {
        let failing = |generation| {
            let model = Model::new(READY | 1 << 29, |_, _| 5);
            let mbox = Mailbox::new(&model, generation);
            let mut buf = C2pBuffer::new(LockDfReg);
            (mbox.send(&NoDelay, &mut buf), buf.header.status)
        };
        assert_eq!(failing(Generation::Naples), (Ok(()), 0));
        let status = Err(PspError::Status {
            command: 0x1b,
            status: 5,
        });
        assert_eq!(failing(Generation::Rome), (status, 5));
        assert_eq!(failing(Generation::Milan), (status, 0));
    }

    #[test]
    fn milan_only_commands_fail_closed() {
        let model = Model::new(READY, |_, _| 0);
        let mbox = Mailbox::new(&model, Generation::Rome);
        assert_eq!(
            mbox.send(&NoDelay, &mut C2pBuffer::new(DisableDrtm::new())),
            Err(PspError::Unsupported {
                command: 0x62,
                generation: Generation::Rome
            })
        );
    }

    #[test]
    fn smn_transport_addresses() {
        use crate::smn::model::Model as Pci;
        let pci = Pci::default();
        pci.0.borrow_mut().smn.insert((0, C2P_SMN), READY);
        let transport = unsafe { SmnTransport::new(Smn::new(pci.clone(), 0)) };
        transport.write(C2pReg::BufLo, 0x1234_5000);
        transport.write(C2pReg::BufHi, 0);
        assert_eq!(transport.read(C2pReg::Cmd), READY);
        let s = pci.0.borrow();
        assert_eq!(s.smn[&(0, 0x0381_0574)], 0x1234_5000);
        assert_eq!(s.smn[&(0, 0x0381_0578)], 0);
    }
}
