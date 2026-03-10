//! SMBIOS table generation for fstart firmware.
//!
//! Provides minimal SMBIOS 3.0 (64-bit entry point) table generation
//! for ARM SBSA platforms. Currently a placeholder; full implementation
//! will generate Type 0 (BIOS Info), Type 1 (System Info), Type 4
//! (Processor), Type 16/17/19 (Memory), and Type 127 (End-of-Table).

#![no_std]
