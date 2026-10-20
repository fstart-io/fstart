// SPDX-License-Identifier: MIT OR Apache-2.0
//! One-shot permanent DRAM staging, not patch authentication or loader execution.
#[cfg(all(feature = "linked-microcode", target_os = "none"))]
pub mod linked;
