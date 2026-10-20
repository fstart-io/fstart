// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso SMU mailbox.
use crate::fam17h::tsc::DelayNs;
use crate::smn::{Smn, SmnReg};
pub use crate::fam17h::smu::{Error, Reply};
use crate::fam17h::smu::Mailbox;
use fstart_pci::ConfigRegionAccess;

pub const MESSAGE: SmnReg = SmnReg::new(0x03b1_0528);
pub const RESPONSE: SmnReg = SmnReg::new(0x03b1_0564);
pub const ARGUMENTS: SmnReg = SmnReg::new(0x03b1_0998);
pub use crate::fam17h::smu::TEST;
pub const GET_NAME: u32 = 0x04;
pub const DXIO_SERVICE: u32 = 0x0a;
pub const GET_SILICON_ID: u32 = 0x51;
pub const MAILBOX: Mailbox = Mailbox {
    message: MESSAGE,
    response: RESPONSE,
    arguments: ARGUMENTS,
};

/// Raw completed response; only source-backed late-policy callers may
/// interpret a nonstandard status. Ordinary initialization uses `request`.
pub fn request_raw(
    smn: &Smn<impl ConfigRegionAccess>,
    message: u32,
    arguments: [u32; 6],
    delay: &mut impl DelayNs,
) -> Result<Reply, Error> {
    MAILBOX.request_raw(smn, message, arguments, delay)
}

pub fn request(
    smn: &Smn<impl ConfigRegionAccess>,
    message: u32,
    arguments: [u32; 6],
    delay: &mut impl DelayNs,
) -> Result<[u32; 6], Error> {
    MAILBOX.request(smn, message, arguments, delay)
}

pub fn cpu_name(smn: &Smn<impl ConfigRegionAccess>, delay: &mut impl DelayNs) -> Result<[u8; 48], Error> {
    MAILBOX.cpu_name(smn, GET_NAME, delay)
}
