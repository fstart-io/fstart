// SPDX-License-Identifier: MIT OR Apache-2.0
//! Six-dword Picasso SMU protocol.
use super::registers::Smn;

pub const MESSAGE: u32 = 0x03b1_0528;
pub const RESPONSE: u32 = 0x03b1_0564;
pub const ARGUMENTS: u32 = 0x03b1_0998;
pub const TEST: u32 = 0x01;
pub const GET_NAME: u32 = 0x04;
pub const DXIO_SERVICE: u32 = 0x0a;
pub const GET_SILICON_ID: u32 = 0x51;
const TIMEOUT_POLLS: usize = 100_000; // 10 seconds, 100 us per poll.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    BusyTimeout,
    CompletionTimeout,
    Unavailable,
    Rejected(u32),
}

fn poll(smn: &mut impl Smn, delay_us: &mut impl FnMut(u32)) -> Result<Option<u32>, Error> {
    for _ in 0..TIMEOUT_POLLS {
        let response = smn.read(RESPONSE);
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

/// Completed protocol response, NOT a claim that the command succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    pub status: u32,
    pub arguments: [u32; 6],
}

/// Return the raw completed response without declaring a nonstandard status
/// successful. Only source-backed late-policy callers may interpret it;
/// ordinary initialization must use the strict `request` wrapper below.
/// Previous nonzero errors, except all-ones, count as readiness. Clear response BEFORE args,
/// submit the command LAST; transport failure requires fail-stop, never FSP.
pub fn request_raw(
    smn: &mut impl Smn,
    message: u32,
    arguments: [u32; 6],
    delay_us: &mut impl FnMut(u32),
) -> Result<Reply, Error> {
    poll(smn, delay_us)?.ok_or(Error::BusyTimeout)?;
    smn.write(RESPONSE, 0);
    for (i, value) in arguments.into_iter().enumerate() {
        smn.write(ARGUMENTS + i as u32 * 4, value);
    }
    smn.write(MESSAGE, message);
    let response = poll(smn, delay_us)?.ok_or(Error::CompletionTimeout)?;
    Ok(Reply {
        status: response,
        arguments: core::array::from_fn(|i| smn.read(ARGUMENTS + i as u32 * 4)),
    })
}

pub fn request(
    smn: &mut impl Smn,
    message: u32,
    arguments: [u32; 6],
    delay_us: &mut impl FnMut(u32),
) -> Result<[u32; 6], Error> {
    let reply = request_raw(smn, message, arguments, delay_us)?;
    if reply.status != 1 {
        return Err(Error::Rejected(reply.status));
    }
    Ok(reply.arguments)
}

pub fn cpu_name(smn: &mut impl Smn, delay_us: &mut impl FnMut(u32)) -> Result<[u8; 48], Error> {
    let mut name = [0; 48];
    for (i, word) in name.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let response = request(smn, GET_NAME, [i as u32, 0, 0, 0, 0, 0], delay_us)?;
        word.copy_from_slice(&response[0].to_le_bytes());
    }
    name[47] = 0;
    Ok(name)
}
