//! Sophgo Mango FIP (Firmware Image Package) constants.
//!
//! Shared between the SoC crate and xtask. The actual FIP writer lives in
//! `xtask/src/fip.rs` which uses these constants to produce the on-disk image.
//!
//! # References
//!
//! - TF-A `include/tools_share/firmware_image_package.h`
//! - Sophgo `plat/sophgo/mango/include/platform_def.h`

/// FIP TOC header magic — first 4 bytes of any valid FIP image (little-endian).
pub const FIP_TOC_MAGIC: u32 = 0xAA64_0001;

/// Byte offset at which the single BL2 payload starts in our minimal FIP.
///
/// Layout: 16-byte TOC header + 40-byte BL2 entry + 40-byte null terminator
/// = 96 bytes = 0x60.
pub const FIP_PAYLOAD_OFFSET: u64 = 0x60;

/// UUID identifying this image as Sophgo Mango BL2.
///
/// Custom Sophgo UUID — NOT the standard TF-A BL2 UUID.
/// Source: Sophgo TF-A `plat/sophgo/mango/include/platform_def.h`.
pub const UUID_MANGO_BL2: [u8; 16] = [
    0x5f, 0xf9, 0xec, 0x0b, // field1 little-endian
    0x4d, 0x22, // field2 little-endian
    0x3e, 0x4d, // field3 little-endian
    0xa5, 0x44, // field4 big-endian
    0xc3, 0x9d, 0x81, 0xc7, 0x3f, 0x0a,
];
