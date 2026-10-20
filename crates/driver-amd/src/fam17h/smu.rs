// SPDX-License-Identifier: MIT OR Apache-2.0
//! Six-dword SMU (MP1) mailbox protocol. The register
//! addresses differ per SoC; the protocol does not.
use crate::fam17h::tsc::DelayNs;
use super::registers::{Smn, SmnReg};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

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

const TIMEOUT_POLLS: usize = 100_000; // 10 seconds, 100 us per poll.
pub const ARGUMENT_COUNT: usize = 6;
/// Response of a successfully completed message.
pub const OK: u32 = 1;

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
}
impl Mailbox {
    pub const fn argument(&self, i: usize) -> SmnReg {
        self.arguments.offset(4 * i as u32)
    }
    fn poll(&self, smn: &Smn<impl ConfigRegionAccess>, delay: &mut impl DelayNs) -> Result<Option<u32>, Error> {
        for _ in 0..TIMEOUT_POLLS {
            let response = smn.reg(self.response).get();
            // All-ones is an unavailable aperture, never previous readiness or
            // a completed nonstandard response. Native common timeout is zero.
            if response == u32::MAX {
                return Err(Error::Unavailable);
            }
            if response != 0 {
                return Ok(Some(response));
            }
            delay.delay_us(100);
        }
        Ok(None)
    }

    /// Return the raw completed response without declaring a nonstandard status
    /// successful. Previous nonzero responses, except all-ones, count as
    /// readiness. Clear the response BEFORE the arguments, submit the command LAST.
    pub fn request_raw(
        &self,
        smn: &Smn<impl ConfigRegionAccess>,
        message: u32,
        arguments: [u32; ARGUMENT_COUNT],
        delay: &mut impl DelayNs,
    ) -> Result<Reply, Error> {
        self.poll(smn, delay)?.ok_or(Error::BusyTimeout)?;
        smn.reg(self.response).set(0);
        for (i, value) in arguments.into_iter().enumerate() {
            smn.reg(self.argument(i)).set(value);
        }
        smn.reg(self.message).set(message);
        let response = self.poll(smn, delay)?.ok_or(Error::CompletionTimeout)?;
        Ok(Reply {
            status: response,
            arguments: core::array::from_fn(|i| smn.reg(self.argument(i)).get()),
        })
    }

    pub fn request(
        &self,
        smn: &Smn<impl ConfigRegionAccess>,
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
    pub fn test(&self, smn: &Smn<impl ConfigRegionAccess>, delay: &mut impl DelayNs) -> Result<(), Error> {
        self.request(smn, TEST, [0xaa55_aa55, 0, 0, 0, 0, 0], delay).map(drop)
    }

    /// The 47-character CPU name string, `get_name` being the SoC's message.
    pub fn cpu_name(
        &self,
        smn: &Smn<impl ConfigRegionAccess>,
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
