// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h mechanisms shared by Raven/Picasso and Zeppelin: MSR access,
//! the DRAM-first cache plan, the TSC, the APOB, the DXIO descriptors, the
//! SMN/Data Fabric register transports, microcode patches and the PSP mailbox.
pub mod apob;
pub mod cache;
pub mod dxio;
pub mod microcode;
pub mod msr;
pub mod psp;
pub mod registers;
pub mod smu;
pub mod tsc;
