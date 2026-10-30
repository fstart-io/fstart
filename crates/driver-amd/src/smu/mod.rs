// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMU (MP1): the six-dword mailbox protocol,
//! and the thread release ([`threads`]). The register addresses and the
//! message numbers differ per SMU version; the protocol does not. The
//! versions, by their AGESA names: [`v9`] (AmdNbioSmuV9, Zeppelin and
//! Naples), [`v10`] (AmdNbioSmuV10, Raven/Picasso) and `v11` (Rome, Milan).
//!
//! [`SmuMessages`] is the contract the blocks that talk to the SMU program
//! against; [`Smu`] implements it for one mailbox.
use crate::ccx::tsc::DelayNs;
use crate::smn::{Smn, SmnAccess, SmnReg};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod threads;
#[cfg(feature = "picasso")]
pub mod v10;
#[cfg(any(feature = "rome", feature = "milan"))]
pub mod v11;
#[cfg(any(feature = "zeppelin", feature = "naples"))]
pub mod v9;

register_bitfields![u32,
    pub ThreadConfig [
        /// Cores per complex - 1.
        CORE_COUNT OFFSET(0) NUMBITS(4) [],
        /// Complexes - 1.
        COMPLEX_COUNT OFFSET(4) NUMBITS(4) [],
        NOT_SMT OFFSET(8) NUMBITS(1) [],
        /// Request SMT off; takes effect after a reset.
        DISABLE_SMT OFFSET(9) NUMBITS(1) []
    ]
];
/// ThreadEn, bit complex * 8 + thread of the complex, the thread topology
/// and the core disable mask the SMU reads at a reset (per die).
pub const THREAD_ENABLE: SmnReg = SmnReg::new(0x0005_a818);
pub const THREAD_CONFIG: SmnReg<ThreadConfig::Register> = SmnReg::new(0x0005_a81c);
pub const DOWNCORE: SmnReg = SmnReg::new(0x0005_a860);

/// Argument 0xaa55aa55; also makes the SMU pick up the ThreadEn settings.
pub const TEST: u32 = 0x01;

const POLL_US: u32 = 100;
/// How long a message may take by default.
pub const DEFAULT_TIMEOUT_US: u32 = 10_000_000;
/// A message that takes longer is logged.
const SLOW_MS: u32 = 100;
pub const ARGUMENT_COUNT: usize = 6;
/// Response of a successfully completed message.
pub const OK: u32 = 1;

/// Mailbox responses (names as in Oxide's public `zen/smu.h`).
pub mod response {
    pub const OK: u32 = super::OK;
    /// AGESA reports it as "SMU Assert AccessFlags" and spins.
    pub const BUSY: u32 = 0xfc;
    pub const PREREQ: u32 = 0xfd;
    pub const UNKNOWN_CMD: u32 = 0xfe;
    pub const FAILED: u32 = 0xff;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    BusyTimeout,
    CompletionTimeout,
    Unavailable,
    Rejected(u32),
}

/// Completed protocol response, NOT a claim that the command succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    pub status: u32,
    pub arguments: [u32; ARGUMENT_COUNT],
}

/// The SMN registers of one SoC's SMU mailbox; the arguments are
/// consecutive registers.
#[derive(Clone, Copy)]
pub struct Mailbox {
    pub message: SmnReg,
    pub response: SmnReg,
    pub arguments: SmnReg,
    /// Wait for the previous message's response before sending (V9, V10).
    /// The V11 response reads zero until the first message completes.
    pub wait_before: bool,
}
impl Mailbox {
    pub const fn argument(&self, i: usize) -> SmnReg {
        self.arguments.offset(4 * i as u32)
    }
    /// The nonzero response and the polls it took, `None` at the timeout.
    fn poll(
        &self,
        smn: &impl SmnAccess,
        delay: &mut impl DelayNs,
        timeout_us: u32,
        poll_us: u32,
    ) -> Result<Option<(u32, u32)>, Error> {
        let poll_us = poll_us.max(1);
        for polls in 0..timeout_us / poll_us {
            let response = smn.reg(self.response).get();
            // All-ones is an unavailable aperture, never previous readiness or
            // a completed nonstandard response. Native common timeout is zero.
            if response == u32::MAX {
                return Err(Error::Unavailable);
            }
            if response != 0 {
                return Ok(Some((response, polls)));
            }
            delay.delay_us(poll_us);
        }
        Ok(None)
    }

    /// Return the raw completed response without declaring a nonstandard status
    /// successful. Previous nonzero responses, except all-ones, count as
    /// readiness. Clear the response BEFORE the arguments, submit the command LAST.
    pub fn request_raw(
        &self,
        smn: &impl SmnAccess,
        message: u32,
        arguments: [u32; ARGUMENT_COUNT],
        delay: &mut impl DelayNs,
    ) -> Result<Reply, Error> {
        self.request_raw_within(smn, message, arguments, delay, DEFAULT_TIMEOUT_US, POLL_US)
    }

    /// [`Self::request_raw`], each wait bounded by `timeout_us` of polls
    /// `poll_us` apart (a nominal bound: the accesses take time too).
    pub fn request_raw_within(
        &self,
        smn: &impl SmnAccess,
        message: u32,
        arguments: [u32; ARGUMENT_COUNT],
        delay: &mut impl DelayNs,
        timeout_us: u32,
        poll_us: u32,
    ) -> Result<Reply, Error> {
        if self.wait_before {
            self.poll(smn, delay, timeout_us, poll_us)?
                .ok_or(Error::BusyTimeout)?;
        }
        self.post(smn, message, arguments);
        let (response, polls) = self
            .poll(smn, delay, timeout_us, poll_us)?
            .ok_or(Error::CompletionTimeout)?;
        let took_ms = polls * poll_us.max(1) / 1000;
        if took_ms > SLOW_MS {
            fstart_log::info!("smu: message {:#x} took {} ms", message, took_ms);
        }
        Ok(Reply {
            status: response,
            arguments: core::array::from_fn(|i| smn.reg(self.argument(i)).get()),
        })
    }

    /// Submit `message` without waiting for the mailbox or the response:
    /// for a message the SMU never answers because it powers the system
    /// down (Sx entry). Inlined: SMM handlers can't take a constant
    /// mailbox's address.
    #[inline(always)]
    pub fn post(&self, smn: &impl SmnAccess, message: u32, arguments: [u32; ARGUMENT_COUNT]) {
        // The registers by value, not through `self`.
        let (response, first, command) = (self.response, self.arguments, self.message);
        smn.reg(response).set(0);
        for (i, value) in (0..).zip(arguments) {
            smn.reg(first.offset(4 * i)).set(value);
        }
        smn.reg(command).set(message);
    }

    pub fn request(
        &self,
        smn: &impl SmnAccess,
        message: u32,
        arguments: [u32; ARGUMENT_COUNT],
        delay: &mut impl DelayNs,
    ) -> Result<[u32; ARGUMENT_COUNT], Error> {
        let reply = self.request_raw(smn, message, arguments, delay)?;
        if reply.status != OK {
            return Err(Error::Rejected(reply.status));
        }
        Ok(reply.arguments)
    }

    /// [`TEST`].
    pub fn test(&self, smn: &impl SmnAccess, delay: &mut impl DelayNs) -> Result<(), Error> {
        self.request(smn, TEST, [0xaa55_aa55, 0, 0, 0, 0, 0], delay)
            .map(drop)
    }

    /// The 47-character CPU name string, `get_name` being the SoC's message.
    pub fn cpu_name(
        &self,
        smn: &impl SmnAccess,
        get_name: u32,
        delay: &mut impl DelayNs,
    ) -> Result<[u8; 48], Error> {
        let mut name = [0; 48];
        for (i, word) in name.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let response = self.request(smn, get_name, [i as u32, 0, 0, 0, 0, 0], delay)?;
            word.copy_from_slice(&response[0].to_le_bytes());
        }
        name[47] = 0;
        Ok(name)
    }
}

/// Sends SMU messages to one SMU instance (a die on Naples, Zeppelin and
/// Picasso, an IOD on Rome and Milan).
pub trait SmuMessages {
    /// Send message `id` with `args`; on success `args` holds the response
    /// arguments.
    fn send(&self, id: u32, args: &mut [u32; ARGUMENT_COUNT]) -> Result<(), Error>;
}

/// Send `id` with `arg0` (other arguments zero); returns response arg 0.
pub fn send1<M: SmuMessages + ?Sized>(smu: &M, id: u32, arg0: u32) -> Result<u32, Error> {
    let mut args = [arg0, 0, 0, 0, 0, 0];
    smu.send(id, &mut args)?;
    Ok(args[0])
}

/// Send `id` with a 64-bit address as `(lo, hi)` in args 0/1 (V11 DRAM
/// address messages).
pub fn send_addr<M: SmuMessages + ?Sized>(smu: &M, id: u32, addr: u64) -> Result<(), Error> {
    let mut args = [addr as u32, (addr >> 32) as u32, 0, 0, 0, 0];
    smu.send(id, &mut args)
}

/// One SMU's mailbox, reached through `smn`.
pub struct Smu<P, D> {
    smn: Smn<P>,
    mailbox: Mailbox,
    /// Copied in and out per message: a `RefCell` would link a panic path
    /// into the SMM handler.
    delay: core::cell::Cell<D>,
    timeout_us: u32,
    poll_us: u32,
    /// Message IDs at or above this are rejected without being sent
    /// (AGESA's V11 service range check: Rome < 0x3E, Milan < 0x4A).
    id_limit: u32,
}
impl<P: ConfigRegionAccess, D: DelayNs + Copy> Smu<P, D> {
    /// `smn` reaches the SMU: the die's root bus (Naples, Zeppelin,
    /// Picasso) or any root bus of the socket (Rome, Milan). Nothing else
    /// sends messages while this value is used.
    pub fn new(smn: Smn<P>, mailbox: Mailbox, delay: D, id_limit: u32) -> Self {
        Self {
            smn,
            mailbox,
            delay: core::cell::Cell::new(delay),
            timeout_us: DEFAULT_TIMEOUT_US,
            poll_us: POLL_US,
            id_limit,
        }
    }
    /// Poll every `poll_us` instead.
    pub fn with_poll_us(self, poll_us: u32) -> Self {
        Self { poll_us, ..self }
    }
    /// Replace the bound of each wait.
    pub fn with_timeout_us(self, timeout_us: u32) -> Self {
        Self { timeout_us, ..self }
    }
    pub fn smn(&self) -> &Smn<P> {
        &self.smn
    }
}
impl<P: ConfigRegionAccess, D: DelayNs + Copy> SmuMessages for Smu<P, D> {
    fn send(&self, id: u32, args: &mut [u32; ARGUMENT_COUNT]) -> Result<(), Error> {
        if id >= self.id_limit {
            fstart_log::error!(
                "smu: message {:#x} exceeds ID limit {:#x}",
                id,
                self.id_limit
            );
            return Err(Error::Rejected(response::UNKNOWN_CMD));
        }
        let mut delay = self.delay.get();
        let reply = self.mailbox.request_raw_within(
            &self.smn,
            id,
            *args,
            &mut delay,
            self.timeout_us,
            self.poll_us,
        );
        self.delay.set(delay);
        let reply = reply.inspect_err(|error| match error {
            Error::BusyTimeout => fstart_log::error!("smu: timed out before message {:#x}", id),
            Error::CompletionTimeout => fstart_log::error!("smu: message {:#x} timed out", id),
            Error::Unavailable => fstart_log::error!("smu: message {:#x} mailbox unavailable", id),
            Error::Rejected(code) => {
                fstart_log::error!("smu: message {:#x} response {:#x}", id, *code)
            }
        })?;
        if reply.status != OK {
            fstart_log::error!("smu: message {:#x} response {:#x}", id, reply.status);
            return Err(Error::Rejected(reply.status));
        }
        *args = reply.arguments;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod model {
    //! An SMU behind the SMN model: answers every message with a fixed
    //! response and records `(id, args)`.

    extern crate std;
    use fstart_pci::{ConfigRegionAccess, PciAddress};
    use std::vec::Vec;

    use super::{Mailbox, Smu};
    use crate::smn::model::Model;
    use crate::smn::{SMN_DATA, Smn};

    /// A timer that advances 1 µs per read and per delayed µs.
    #[derive(Default)]
    pub struct FakeTimer(pub std::sync::atomic::AtomicU64);
    impl fstart_core::services::Timer for FakeTimer {
        fn delay_us(&self, us: u64) {
            self.0.fetch_add(us, std::sync::atomic::Ordering::Relaxed);
        }
        fn timestamp_us(&self) -> u64 {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    }

    /// No delay: the model answers at once or never.
    pub type NoDelay = crate::ccx::tsc::testing::Delay<fn(u32)>;
    pub fn no_delay() -> NoDelay {
        crate::ccx::tsc::testing::Delay(|_| {})
    }

    /// Messages received: `(id, args)`.
    pub type Sent = std::rc::Rc<std::cell::RefCell<Vec<(u32, [u32; 6])>>>;

    #[derive(Clone)]
    pub struct SmuModel {
        pub smn: Model,
        pub regs: Mailbox,
        /// Response written when a message arrives (0 = never answer).
        pub reply: u32,
        /// Added to arg 0 in the reply (0 = arguments unchanged).
        pub echo: u32,
        pub sent: Sent,
    }

    impl SmuModel {
        pub fn new(regs: Mailbox, reply: u32) -> Self {
            let smn = Model::default();
            // A previous message completed (V9 pre-wait).
            smn.0
                .borrow_mut()
                .smn
                .insert((0, regs.response.address()), 1);
            Self {
                smn,
                regs,
                reply,
                echo: 0,
                sent: Default::default(),
            }
        }

        pub fn ids(&self) -> Vec<u32> {
            self.sent.borrow().iter().map(|s| s.0).collect()
        }
    }

    impl ConfigRegionAccess for SmuModel {
        unsafe fn read(&self, a: PciAddress, offset: u16) -> u32 {
            // SAFETY: model.
            unsafe { self.smn.read(a, offset) }
        }

        unsafe fn write(&self, a: PciAddress, offset: u16, value: u32) {
            // SAFETY: model.
            unsafe { self.smn.write(a, offset, value) };
            let mut s = self.smn.0.borrow_mut();
            let index = s.index.get(&a.bus()).copied().unwrap_or(0);
            if offset == SMN_DATA && index == self.regs.message.address() && self.reply != 0 {
                let args = core::array::from_fn(|i| {
                    s.smn
                        .get(&(a.bus(), self.regs.argument(i).address()))
                        .copied()
                        .unwrap_or(0)
                });
                self.sent.borrow_mut().push((value, args));
                s.smn
                    .insert((a.bus(), self.regs.response.address()), self.reply);
                s.smn.insert(
                    (a.bus(), self.regs.arguments.address()),
                    args[0].wrapping_add(self.echo),
                );
            }
        }
    }

    impl SmuModel {
        /// An [`Smu`] on this model.
        pub fn smu(&self, id_limit: u32) -> Smu<Self, NoDelay> {
            // SAFETY: a host model.
            let smn = unsafe { Smn::new(self.clone()) };
            Smu::new(smn, self.regs, no_delay(), id_limit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::model::SmuModel;
    use super::*;

    const MAILBOX: Mailbox = Mailbox {
        message: SmnReg::new(0x03b1_0530),
        response: SmnReg::new(0x03b1_057c),
        arguments: SmnReg::new(0x03b1_09c4),
        wait_before: false,
    };

    #[test]
    fn round_trip() {
        let m = SmuModel {
            echo: 1,
            ..SmuModel::new(MAILBOX, response::OK)
        };
        let mut args = [5, 6, 0, 0, 0, 0];
        m.smu(0x4a).send(0x34, &mut args).unwrap();
        assert_eq!(m.sent.borrow()[0], (0x34, [5, 6, 0, 0, 0, 0]));
        assert_eq!(args[..2], [6, 6]);
    }

    #[test]
    fn errors() {
        let busy = SmuModel::new(MAILBOX, response::BUSY);
        assert_eq!(
            busy.smu(0x4a).send(3, &mut [0; 6]),
            Err(Error::Rejected(response::BUSY))
        );
        let dead = SmuModel::new(MAILBOX, 0);
        assert_eq!(
            dead.smu(0x4a).send(3, &mut [0; 6]),
            Err(Error::CompletionTimeout)
        );
        let ok = SmuModel::new(MAILBOX, response::OK);
        assert_eq!(
            ok.smu(0x4a).send(0x4a, &mut [0; 6]),
            Err(Error::Rejected(response::UNKNOWN_CMD))
        );
        assert!(ok.sent.borrow().is_empty());
    }
}
