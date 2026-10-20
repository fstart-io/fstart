// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h mechanisms shared by Raven/Picasso and Zeppelin: MSR access,
//! the DRAM-first cache plan, the TSC, the APOB and the SMN/Data Fabric
//! register transports.
pub mod apob;
pub mod cache;
pub mod msr;
pub mod registers;
pub mod smu;
pub mod tsc;
