//! The SMN SMU mailbox shared by V9 and V11 (NBIO_SMU §3.8,
//! `smu.v9.mailbox`, `smu.v11.mailbox`).
//!
//! Sequence: (V9 only: wait for a non-zero response) clear the response,
//! write the six arguments, write the message ID, poll the response, read
//! the arguments back. AGESA polls forever and loops on 0xFC; here every
//! poll is bounded and any response other than OK is an error.

use fstart_core::services::Timer;
use fstart_pci::ConfigRegionAccess;

use super::{SMU_ARGS, SmuError, SmuMessages};
use crate::smn::{Smn, SmnAddr};

/// Mailbox responses (names as in Oxide's public `zen/smu.h`).
pub mod response {
    pub const OK: u32 = 0x01;
    /// AGESA reports it as "SMU Assert AccessFlags" and spins.
    pub const BUSY: u32 = 0xFC;
    pub const PREREQ: u32 = 0xFD;
    pub const UNKNOWN_CMD: u32 = 0xFE;
    pub const FAILED: u32 = 0xFF;
}

/// The SMN registers of one mailbox generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registers {
    pub message: SmnAddr,
    pub response: SmnAddr,
    /// Argument 0; arguments 1..5 follow at 4-byte stride.
    pub args: SmnAddr,
    /// V9 waits for the previous message to complete before starting.
    pub wait_before: bool,
}

/// Default poll budget. The donor has none; DcBtc-type calibrations take
/// milliseconds, so allow a generous second.
pub const DEFAULT_TIMEOUT_US: u64 = 1_000_000;
/// Delay between response polls.
const POLL_US: u64 = 10;

/// One SMU mailbox reached over SMN.
pub struct Mailbox<P, T> {
    smn: Smn<P>,
    regs: Registers,
    timer: T,
    timeout_us: u64,
    /// Message IDs at or above this are rejected without being sent
    /// (AGESA's V11 service range check: Rome < 0x3E, Milan < 0x4A).
    id_limit: u32,
}

impl<P: ConfigRegionAccess, T: Timer> Mailbox<P, T> {
    /// A mailbox through `smn` (Naples: the die's root bus; Rome/Milan: any
    /// root bus of the socket).
    ///
    /// # Safety
    /// The caller owns the SMN index/data pair and the mailbox (no other
    /// agent sends messages concurrently) while this value is used.
    pub unsafe fn new(smn: Smn<P>, regs: Registers, timer: T, id_limit: u32) -> Self {
        Self {
            smn,
            regs,
            timer,
            timeout_us: DEFAULT_TIMEOUT_US,
            id_limit,
        }
    }

    /// Replace the poll budget.
    pub fn with_timeout_us(self, timeout_us: u64) -> Self {
        Self { timeout_us, ..self }
    }

    fn read(&self, addr: SmnAddr) -> u32 {
        // SAFETY: constructor contract.
        unsafe { self.smn.read32(addr) }
    }

    fn write(&self, addr: SmnAddr, value: u32) {
        // SAFETY: constructor contract.
        unsafe { self.smn.write32(addr, value) }
    }

    /// Poll the response register until non-zero, bounded by the timeout.
    fn wait(&self) -> Result<u32, SmuError> {
        let start = self.timer.timestamp_us();
        loop {
            let r = self.read(self.regs.response);
            if r != 0 {
                return Ok(r);
            }
            if self.timer.timestamp_us().wrapping_sub(start) >= self.timeout_us {
                return Err(SmuError::Timeout);
            }
            self.timer.delay_us(POLL_US);
        }
    }

    fn arg(&self, i: usize) -> SmnAddr {
        self.regs.args + 4 * i as u32
    }
}

impl<P: ConfigRegionAccess, T: Timer> SmuMessages for Mailbox<P, T> {
    fn send(&self, id: u32, args: &mut [u32; SMU_ARGS]) -> Result<(), SmuError> {
        if id >= self.id_limit {
            return Err(SmuError::Status(response::UNKNOWN_CMD));
        }
        if self.regs.wait_before {
            self.wait()?;
        }
        self.write(self.regs.response, 0);
        args.iter()
            .enumerate()
            .for_each(|(i, &a)| self.write(self.arg(i), a));
        self.write(self.regs.message, id);
        match self.wait()? {
            response::OK => {
                args.iter_mut()
                    .enumerate()
                    .for_each(|(i, a)| *a = self.read(self.arg(i)));
                Ok(())
            }
            other => Err(SmuError::Status(other)),
        }
    }
}

#[cfg(test)]
pub(crate) mod model {
    //! An SMU behind the SMN model: answers every message with a fixed
    //! response and records `(id, args)`.
    use std::sync::atomic::{AtomicU64, Ordering};

    use fstart_core::services::Timer;
    use fstart_pci::{ConfigRegionAccess, PciAddress};

    use super::Registers;
    use crate::smn::SMN_DATA;
    use crate::smn::model::Model;

    /// A timer that advances 1 µs per read and per delayed µs.
    #[derive(Default)]
    pub struct FakeTimer(pub AtomicU64);

    impl Timer for FakeTimer {
        fn delay_us(&self, us: u64) {
            self.0.fetch_add(us, Ordering::Relaxed);
        }
        fn timestamp_us(&self) -> u64 {
            self.0.fetch_add(1, Ordering::Relaxed)
        }
    }

    /// Messages received: `(id, args)`.
    pub type Sent = std::rc::Rc<std::cell::RefCell<Vec<(u32, [u32; 6])>>>;

    #[derive(Clone)]
    pub struct SmuModel {
        pub smn: Model,
        pub regs: Registers,
        /// Response written when a message arrives (0 = never answer).
        pub reply: u32,
        /// Added to arg 0 in the reply (0 = arguments unchanged).
        pub echo: u32,
        pub sent: Sent,
    }

    impl SmuModel {
        pub fn new(regs: Registers, reply: u32) -> Self {
            let smn = Model::default();
            // A previous message completed (V9 pre-wait).
            smn.0.borrow_mut().smn.insert((0, regs.response), 1);
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
            if offset == SMN_DATA && index == self.regs.message && self.reply != 0 {
                let args = core::array::from_fn(|i| {
                    s.smn
                        .get(&(a.bus(), self.regs.args + 4 * i as u32))
                        .copied()
                        .unwrap_or(0)
                });
                self.sent.borrow_mut().push((value, args));
                s.smn.insert((a.bus(), self.regs.response), self.reply);
                s.smn
                    .insert((a.bus(), self.regs.args), args[0].wrapping_add(self.echo));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::model::{FakeTimer, SmuModel};
    use super::*;

    const REGS: Registers = Registers {
        message: 0x03B1_0530,
        response: 0x03B1_057C,
        args: 0x03B1_09C4,
        wait_before: false,
    };

    fn mailbox(m: &SmuModel) -> Mailbox<SmuModel, FakeTimer> {
        // SAFETY: model.
        unsafe { Mailbox::new(Smn::new(m.clone(), 0), REGS, FakeTimer::default(), 0x4A) }
    }

    #[test]
    fn round_trip() {
        let m = SmuModel {
            echo: 1,
            ..SmuModel::new(REGS, response::OK)
        };
        let mut args = [5, 6, 0, 0, 0, 0];
        mailbox(&m).send(0x34, &mut args).unwrap();
        assert_eq!(m.sent.borrow()[0], (0x34, [5, 6, 0, 0, 0, 0]));
        assert_eq!(args[..2], [6, 6]);
    }

    #[test]
    fn errors() {
        let busy = SmuModel::new(REGS, response::BUSY);
        assert_eq!(
            mailbox(&busy).send(3, &mut [0; 6]),
            Err(SmuError::Status(response::BUSY))
        );
        let dead = SmuModel::new(REGS, 0);
        assert_eq!(
            mailbox(&dead).with_timeout_us(100).send(3, &mut [0; 6]),
            Err(SmuError::Timeout)
        );
        let ok = SmuModel::new(REGS, response::OK);
        assert_eq!(
            mailbox(&ok).send(0x4A, &mut [0; 6]),
            Err(SmuError::Status(response::UNKNOWN_CMD))
        );
        assert!(ok.sent.borrow().is_empty());
    }
}
