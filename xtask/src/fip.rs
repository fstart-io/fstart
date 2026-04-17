//! Sophgo Mango FIP (Firmware Image Package) container writer.
//!
//! Produces a minimal FIP image that the SG2042 silicon boot ROM accepts.
//! The FIP contains a single BL2 payload identified by UUID_MANGO_BL2.
//!
//! # FIP layout
//!
//! | Offset | Size | Content |
//! |--------|------|---------|
//! | 0x00   | 4    | TOC magic (`0xAA64_0001` LE) |
//! | 0x04   | 4    | Serial number (0) |
//! | 0x08   | 8    | TOC flags (0) |
//! | 0x10   | 40   | BL2 entry (UUID_MANGO_BL2, offset=0x58, size=N) |
//! | 0x38   | 40   | Null UUID end-of-table marker |
//! | 0x58   | N    | Raw BL2 binary |
//!
//! # References
//!
//! - TF-A `include/tools_share/firmware_image_package.h`
//! - Sophgo `plat/sophgo/mango/include/platform_def.h` (UUID_MANGO_BL2)

use std::io::Write as _;
use std::path::Path;

/// FIP TOC header magic — first 4 bytes of any valid FIP image (little-endian).
pub const FIP_TOC_MAGIC: u32 = 0xAA64_0001;

/// Byte offset at which the BL2 payload starts in the FIP file.
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

/// Write a minimal FIP containing a single BL2 payload to `out_path`.
///
/// Suitable for flashing to SPI flash offset `0x30000` (copy A) on
/// Milk-V Pioneer.
///
/// # Errors
///
/// Returns `Err(String)` on I/O failure or self-check mismatch.
pub fn write_fip(payload: &[u8], out_path: &Path) -> Result<(), String> {
    let mut buf: Vec<u8> = Vec::with_capacity(FIP_PAYLOAD_OFFSET as usize + payload.len());

    // TOC header: 4-byte magic LE + 4-byte serial (0) + 8-byte flags (0)
    buf.write_all(&FIP_TOC_MAGIC.to_le_bytes())
        .map_err(|e| e.to_string())?;
    buf.write_all(&0u32.to_le_bytes())
        .map_err(|e| e.to_string())?;
    buf.write_all(&0u64.to_le_bytes())
        .map_err(|e| e.to_string())?;

    // BL2 entry: UUID + offset + size + flags
    buf.write_all(&UUID_MANGO_BL2).map_err(|e| e.to_string())?;
    buf.write_all(&FIP_PAYLOAD_OFFSET.to_le_bytes())
        .map_err(|e| e.to_string())?;
    buf.write_all(&(payload.len() as u64).to_le_bytes())
        .map_err(|e| e.to_string())?;
    buf.write_all(&0u64.to_le_bytes())
        .map_err(|e| e.to_string())?; // flags

    // Null end-of-table sentinel: all-zero UUID + zero fields (40 bytes)
    buf.write_all(&[0u8; 40]).map_err(|e| e.to_string())?;

    assert_eq!(
        buf.len(),
        FIP_PAYLOAD_OFFSET as usize,
        "FIP header size must be exactly {:#x} bytes",
        FIP_PAYLOAD_OFFSET
    );

    // Payload
    buf.write_all(payload).map_err(|e| e.to_string())?;

    // Self-check: magic at offset 0
    if buf[0..4] != FIP_TOC_MAGIC.to_le_bytes() {
        return Err("FIP self-check failed: magic mismatch".into());
    }
    // Self-check: UUID_MANGO_BL2 at offset 16
    if buf[16..32] != UUID_MANGO_BL2 {
        return Err("FIP self-check failed: UUID mismatch".into());
    }
    // Self-check: payload offset field at bytes 32..40
    let recorded_offset = u64::from_le_bytes(buf[32..40].try_into().unwrap());
    if recorded_offset != FIP_PAYLOAD_OFFSET {
        return Err(format!(
            "FIP self-check failed: payload offset {recorded_offset:#x} != {FIP_PAYLOAD_OFFSET:#x}"
        ));
    }

    std::fs::write(out_path, &buf).map_err(|e| format!("write FIP to {}: {e}", out_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn build_and_read(payload: &[u8]) -> Vec<u8> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_fip(payload, tmp.path()).unwrap();
        let mut buf = Vec::new();
        std::fs::File::open(tmp.path())
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        buf
    }

    #[test]
    fn test_fip_magic() {
        let buf = build_and_read(b"hello");
        assert_eq!(&buf[0..4], &FIP_TOC_MAGIC.to_le_bytes());
    }

    #[test]
    fn test_fip_uuid_at_offset_16() {
        let buf = build_and_read(b"hello");
        assert_eq!(&buf[16..32], &UUID_MANGO_BL2);
    }

    #[test]
    fn test_fip_payload_offset_field() {
        let buf = build_and_read(b"hello");
        let offset = u64::from_le_bytes(buf[32..40].try_into().unwrap());
        assert_eq!(offset, FIP_PAYLOAD_OFFSET);
    }

    #[test]
    fn test_fip_payload_size_field() {
        let payload = b"12345678";
        let buf = build_and_read(payload);
        let size = u64::from_le_bytes(buf[40..48].try_into().unwrap());
        assert_eq!(size, payload.len() as u64);
    }

    #[test]
    fn test_fip_payload_at_correct_offset() {
        let payload = b"TEST_PAYLOAD";
        let buf = build_and_read(payload);
        assert_eq!(&buf[FIP_PAYLOAD_OFFSET as usize..], payload);
    }

    #[test]
    fn test_fip_total_length() {
        let payload = b"ABCD";
        let buf = build_and_read(payload);
        assert_eq!(buf.len(), FIP_PAYLOAD_OFFSET as usize + payload.len());
    }

    #[test]
    fn test_fip_null_terminator_at_offset_56() {
        // Null UUID sentinel starts at byte 0x38 = 56
        // (right after the 16-byte TOC header + 40-byte BL2 entry)
        let buf = build_and_read(b"x");
        assert_eq!(&buf[0x38..0x38 + 16], &[0u8; 16]);
    }
}
