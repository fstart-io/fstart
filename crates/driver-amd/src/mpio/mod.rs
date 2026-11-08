// SPDX-License-Identifier: MIT OR Apache-2.0
//! MPIO, the retained PCIe/SATA firmware of the Rs SoCs (Genoa, Siena).
//! Not SP3 DXIO.
//!
//! [`mailbox`] owns the six-argument transport; [`rs`] is the donor-checked
//! Rs lane translation and 52-byte ASK format. The native board model
//! need not recreate AGESA's 0x21b-byte host descriptors. The training
//! flow that composes them with the live DF topology is
//! `fstart_platform_amd::rs_mpio`.

pub mod ancillary;
pub mod mailbox;
pub mod ports;
pub mod rs;
pub mod session;
