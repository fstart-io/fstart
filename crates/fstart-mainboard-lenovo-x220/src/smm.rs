//! Lenovo ThinkPad X220 SMM policy notes.
//!
//! Coreboot's `mainboard/lenovo/x220/smihandler.c` handles EC GPI/APMC/S3
//! routing:
//! - GPI1 is the EC SCI/query source and GPI13 is EC wake.
//! - ACPI enable switches EC ports to command/data 0x1604/0x1600 to avoid
//!   userspace races, routes GPI1 to SCI, and writes EC register 0x80 = 0x01.
//! - ACPI disable switches back to 0x66/0x62 because 0x1600/0x1604 cannot do EC
//!   query, routes GPI1 to SMI, and again discards/enables events via 0x80.
//! - S3 checks EC register 0x32 bits 0x14 before routing GPI13 as wake SCI.
//!
//! Permanent SMM stays disabled in the initial board RON until the reusable
//! Sandy Bridge/PCH SMM plumbing and Lenovo H8 EC accessors are ported.

#![allow(dead_code)]

/// PCH GPI number for EC SCI/query events in coreboot X220 SMM.
pub const GPE_EC_SCI: u8 = 1;
/// PCH GPI number for EC wake events in coreboot X220 SMM.
pub const GPE_EC_WAKE: u8 = 13;
/// ACPI-mode EC data port used by coreboot to avoid userspace races.
pub const EC_ACPI_DATA: u16 = 0x1600;
/// ACPI-mode EC command port used by coreboot to avoid userspace races.
pub const EC_ACPI_CMD: u16 = 0x1604;
/// Legacy EC data port required for EC query.
pub const EC_LEGACY_DATA: u16 = 0x0062;
/// Legacy EC command port required for EC query.
pub const EC_LEGACY_CMD: u16 = 0x0066;
