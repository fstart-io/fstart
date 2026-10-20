// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h mechanisms shared by Raven/Picasso and Zeppelin: the MSRs, the
//! per-CPU CCX init, the DRAM-first cache plan, the TSC, the APOB, the DXIO
//! descriptors, microcode patches and the PSP mailbox.
pub mod cache;
pub mod ccx;
pub mod dxio;
pub mod iohc;
pub mod microcode;
pub mod msr;
pub mod nbio;
pub mod tsc;
