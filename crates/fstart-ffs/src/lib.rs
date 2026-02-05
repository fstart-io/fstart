//! Firmware Filesystem: format, read, and verify.
//!
//! Core reader is no_std + no_alloc (reads from a byte slice).
//! Builder requires std (used by xtask).

#![cfg_attr(not(feature = "std"), no_std)]

// TODO Phase 4: reader, builder, verify modules
