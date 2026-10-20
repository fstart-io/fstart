// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin SMU V9 mailbox and the messages the native init sends. The
//! same SMN addresses on every die.
use crate::fam17h::smu::Mailbox;
pub use crate::fam17h::smu::{ARGUMENT_COUNT, Error, Reply};

pub const MAILBOX: Mailbox = Mailbox {
    message: 0x03b1_0528,
    response: 0x03b1_0564,
    arguments: 0x03b1_0598,
};

/// Argument 0xaa55aa55; also makes the SMU pick up the ThreadEn settings.
pub const TEST: u32 = 0x01;
/// Argument: the expected interface version.
pub const GET_VERSION: u32 = 0x03;
/// Argument: feature mask.
pub const ENABLE_FEATURES: u32 = 0x09;
pub const DXIO_SERVICE: u32 = 0x0f;
/// Argument: index 0..20.
pub const GET_CAC_WEIGHT: u32 = 0x10;
pub const S3_ENTRY: u32 = 0x13;
pub const SET_POLICY_ADDRESS_HI: u32 = 0x1d;
pub const SET_POLICY_ADDRESS_LO: u32 = 0x1e;
pub const SET_AGM_LOG_ADDRESS_HI: u32 = 0x1f;
pub const SET_AGM_LOG_ADDRESS_LO: u32 = 0x20;
pub const TRANSFER_POLICY: u32 = 0x22;
/// Argument: 0.
pub const DC_BTC: u32 = 0x26;
/// Argument: dword index.
pub const GET_NAME_STRING: u32 = 0x28;
pub const SET_PERFMON_ADDRESS_HI: u32 = 0x2d;
pub const SET_PERFMON_ADDRESS_LO: u32 = 0x2e;
pub const MSG_38: u32 = 0x38;
pub const MSG_39: u32 = 0x39;
/// Argument: root bus of the die.
pub const MSG_3B: u32 = 0x3b;
/// Argument: root bus of the die.
pub const MSG_3D: u32 = 0x3d;
pub const CCX_DONE: u32 = 0x41;

/// ThreadEn and the thread topology (SMN, per die).
pub const THREAD_ENABLE: u32 = 0x0005_a818;
pub const THREAD_CONFIG: u32 = 0x0005_a81c;
