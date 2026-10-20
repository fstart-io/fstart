// SPDX-License-Identifier: MIT OR Apache-2.0
//! Six-dword SMU (MP1) mailbox protocol. The register
//! addresses differ per SoC; the protocol does not.
use super::registers::Smn;

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

/// SMN addresses of one SoC's SMU mailbox.
#[derive(Debug, Clone, Copy)]
pub struct Mailbox {
    pub message: u32,
    pub response: u32,
    pub arguments: u32,
}
impl Mailbox {
    fn poll(&self, smn: &mut impl Smn, delay_us: &mut impl FnMut(u32)) -> Result<Option<u32>, Error> {
        for _ in 0..TIMEOUT_POLLS {
            let response = smn.read(self.response);
            // All-ones is an unavailable aperture, never previous readiness or
            // a completed nonstandard response. Native common timeout is zero.
            if response == u32::MAX {
                return Err(Error::Unavailable);
            }
            if response != 0 {
                return Ok(Some(response));
            }
            delay_us(100);
        }
        Ok(None)
    }

    /// Return the raw completed response without declaring a nonstandard status
    /// successful. Previous nonzero responses, except all-ones, count as
    /// readiness. Clear the response BEFORE the arguments, submit the command LAST.
    pub fn request_raw(
        &self,
        smn: &mut impl Smn,
        message: u32,
        arguments: [u32; ARGUMENT_COUNT],
        delay_us: &mut impl FnMut(u32),
    ) -> Result<Reply, Error> {
        self.poll(smn, delay_us)?.ok_or(Error::BusyTimeout)?;
        smn.write(self.response, 0);
        for (i, value) in arguments.into_iter().enumerate() {
            smn.write(self.arguments + i as u32 * 4, value);
        }
        smn.write(self.message, message);
        let response = self.poll(smn, delay_us)?.ok_or(Error::CompletionTimeout)?;
        Ok(Reply {
            status: response,
            arguments: core::array::from_fn(|i| smn.read(self.arguments + i as u32 * 4)),
        })
    }

    pub fn request(
        &self,
        smn: &mut impl Smn,
        message: u32,
        arguments: [u32; ARGUMENT_COUNT],
        delay_us: &mut impl FnMut(u32),
    ) -> Result<[u32; ARGUMENT_COUNT], Error> {
        let reply = self.request_raw(smn, message, arguments, delay_us)?;
        if reply.status != OK {
            return Err(Error::Rejected(reply.status));
        }
        Ok(reply.arguments)
    }

    /// The 47-character CPU name string, `get_name` being the SoC's message.
    pub fn cpu_name(
        &self,
        smn: &mut impl Smn,
        get_name: u32,
        delay_us: &mut impl FnMut(u32),
    ) -> Result<[u8; 48], Error> {
        let mut name = [0; 48];
        for (i, word) in name.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let response = self.request(smn, get_name, [i as u32, 0, 0, 0, 0, 0], delay_us)?;
            word.copy_from_slice(&response[0].to_le_bytes());
        }
        name[47] = 0;
        Ok(name)
    }
}
