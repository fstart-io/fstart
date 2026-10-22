// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso SMU mailbox.
use super::registers::Smn;
pub use crate::fam17h::smu::{Error, Reply};
use crate::fam17h::smu::Mailbox;

pub const MESSAGE: u32 = 0x03b1_0528;
pub const RESPONSE: u32 = 0x03b1_0564;
pub const ARGUMENTS: u32 = 0x03b1_0998;
pub const TEST: u32 = 0x01;
pub const GET_NAME: u32 = 0x04;
pub const DXIO_SERVICE: u32 = 0x0a;
pub const GET_SILICON_ID: u32 = 0x51;
const MAILBOX: Mailbox = Mailbox {
    message: MESSAGE,
    response: RESPONSE,
    arguments: ARGUMENTS,
};

/// Raw completed response; only source-backed late-policy callers may
/// interpret a nonstandard status. Ordinary initialization uses `request`.
pub fn request_raw(
    smn: &mut impl Smn,
    message: u32,
    arguments: [u32; 6],
    delay_us: &mut impl FnMut(u32),
) -> Result<Reply, Error> {
    MAILBOX.request_raw(smn, message, arguments, delay_us)
}

pub fn request(
    smn: &mut impl Smn,
    message: u32,
    arguments: [u32; 6],
    delay_us: &mut impl FnMut(u32),
) -> Result<[u32; 6], Error> {
    MAILBOX.request(smn, message, arguments, delay_us)
}

pub fn cpu_name(smn: &mut impl Smn, delay_us: &mut impl FnMut(u32)) -> Result<[u8; 48], Error> {
    MAILBOX.cpu_name(smn, GET_NAME, delay_us)
}
