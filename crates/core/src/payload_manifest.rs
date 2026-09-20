//! Build-generated payload policy consumed by runtime launchers.
//!
//! Wire format: an 8-byte magic (`FSLNX001`, trailing `001` is the version),
//! then the [`Header`] fields little-endian (kernel load address, zero-page
//! address, flags, bootargs length, two reserved zero bytes), then the raw
//! UTF-8 bootargs with no NUL terminator. Total length must equal exactly
//! [`HEADER_LEN`] plus the declared bootargs length; trailing bytes are
//! rejected so two different manifests can never decode from the same prefix.
//! Unknown flag bits and nonzero reserved bytes fail closed to `None` so a
//! newer writer cannot silently change launch semantics.

use zerocopy::byteorder::{LE, U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Verified FFS asset containing direct x86 Linux launch policy.
pub const X86_LINUX_MANIFEST_ASSET: &str = "x86-linux-manifest";

/// Family default: physical address at which the bzImage protected-mode
/// payload is loaded when `--linux-kernel-load-addr` is omitted.
pub const X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR: u64 = 0x1000_0000;
/// Family default: physical address of the Linux boot-parameter zero page
/// when `--linux-zero-page-addr` is omitted.
pub const X86_LINUX_DEFAULT_ZERO_PAGE_ADDR: u64 = 0x0009_0000;
/// Maximum accepted x86 Linux command-line length in bytes.
///
/// Mirrors the `PayloadConfig::bootargs` (`HString<256>`) capacity so the
/// manifest, the board schema, and the CLI agree on one bound.
pub const X86_LINUX_MAX_BOOTARGS: usize = 256;
/// Shared error when x86 Linux boot arguments exceed [`X86_LINUX_MAX_BOOTARGS`].
pub const X86_LINUX_BOOTARGS_TOO_LONG: &str = "x86 Linux boot arguments exceed 256 bytes";

const MAGIC: [u8; 8] = *b"FSLNX001";
const HEADER_LEN: usize = core::mem::size_of::<Header>();
const PRINT_MTRRS_FLAG: u32 = 1;

const _: () = assert!(HEADER_LEN == 32);

/// Fixed-size manifest header shared verbatim between encoder and decoder.
#[derive(Clone, Copy, Debug, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct Header {
    magic: [u8; 8],
    kernel_load_addr: U64<LE>,
    zero_page_addr: U64<LE>,
    flags: U32<LE>,
    bootargs_len: U16<LE>,
    _reserved: [u8; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86LinuxManifest<'a> {
    pub kernel_load_addr: u64,
    pub zero_page_addr: u64,
    pub bootargs: &'a str,
    pub print_x86_mtrrs: bool,
}

impl X86LinuxManifest<'_> {
    pub fn encode(
        kernel_load_addr: u64,
        zero_page_addr: u64,
        bootargs: &str,
        print_x86_mtrrs: bool,
    ) -> Result<heapless::Vec<u8, { HEADER_LEN + X86_LINUX_MAX_BOOTARGS }>, &'static str> {
        if bootargs.len() > X86_LINUX_MAX_BOOTARGS {
            return Err(X86_LINUX_BOOTARGS_TOO_LONG);
        }
        let header = Header {
            magic: MAGIC,
            kernel_load_addr: U64::new(kernel_load_addr),
            zero_page_addr: U64::new(zero_page_addr),
            flags: U32::new(if print_x86_mtrrs { PRINT_MTRRS_FLAG } else { 0 }),
            bootargs_len: U16::new(bootargs.len() as u16),
            _reserved: [0; 2],
        };
        let mut bytes = heapless::Vec::new();
        bytes
            .extend_from_slice(header.as_bytes())
            .map_err(|_| "x86 Linux manifest exceeds its fixed capacity")?;
        bytes
            .extend_from_slice(bootargs.as_bytes())
            .map_err(|_| "x86 Linux manifest exceeds its fixed capacity")?;
        Ok(bytes)
    }

    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<X86LinuxManifest<'_>> {
        let (header, _rest) = Header::read_from_prefix(bytes).ok()?;
        if header.magic != MAGIC || header._reserved != [0; 2] {
            return None;
        }
        let flags = header.flags.get();
        if flags & !PRINT_MTRRS_FLAG != 0 {
            return None;
        }
        let bootargs_len = usize::from(header.bootargs_len.get());
        if bytes.len() != HEADER_LEN.checked_add(bootargs_len)? {
            return None;
        }
        let bootargs = core::str::from_utf8(bytes.get(HEADER_LEN..)?).ok()?;
        Some(X86LinuxManifest {
            kernel_load_addr: header.kernel_load_addr.get(),
            zero_page_addr: header.zero_page_addr.get(),
            bootargs,
            print_x86_mtrrs: flags & PRINT_MTRRS_FLAG != 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_default(bootargs: &str, print_mtrrs: bool) -> heapless::Vec<u8, 288> {
        X86LinuxManifest::encode(
            X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR,
            X86_LINUX_DEFAULT_ZERO_PAGE_ADDR,
            bootargs,
            print_mtrrs,
        )
        .unwrap()
    }

    #[test]
    fn x86_linux_manifest_round_trips_and_rejects_ambiguous_bytes() {
        let bytes = X86LinuxManifest::encode(0x1000000, 0x90000, "console=ttyS0", true).unwrap();
        assert_eq!(
            X86LinuxManifest::decode(&bytes),
            Some(X86LinuxManifest {
                kernel_load_addr: 0x1000000,
                zero_page_addr: 0x90000,
                bootargs: "console=ttyS0",
                print_x86_mtrrs: true,
            })
        );

        let mut trailing = bytes.clone();
        trailing.push(0).unwrap();
        assert_eq!(X86LinuxManifest::decode(&trailing), None);
        let mut unknown_flags = bytes;
        unknown_flags[24] = 2;
        assert_eq!(X86LinuxManifest::decode(&unknown_flags), None);
    }

    #[test]
    fn x86_linux_manifest_round_trips_empty_and_max_bootargs() {
        for (bootargs, print_mtrrs) in [("", false), ("console=ttyS0", false)] {
            let bytes = encode_default(bootargs, print_mtrrs);
            let decoded = X86LinuxManifest::decode(&bytes).expect("valid manifest decodes");
            assert_eq!(decoded.bootargs, bootargs);
            assert_eq!(decoded.print_x86_mtrrs, print_mtrrs);
            assert_eq!(decoded.kernel_load_addr, X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR);
            assert_eq!(decoded.zero_page_addr, X86_LINUX_DEFAULT_ZERO_PAGE_ADDR);
        }

        let mut long = heapless::String::<256>::new();
        for _ in 0..X86_LINUX_MAX_BOOTARGS {
            long.push('a').unwrap();
        }
        let bytes = encode_default(&long, true);
        assert_eq!(bytes.len(), HEADER_LEN + X86_LINUX_MAX_BOOTARGS);
        assert_eq!(
            X86LinuxManifest::decode(&bytes)
                .expect("max bootargs decode")
                .bootargs,
            long.as_str()
        );
    }

    #[test]
    fn x86_linux_manifest_rejects_overlong_bootargs_with_shared_error() {
        let mut long = heapless::String::<300>::new();
        for _ in 0..=X86_LINUX_MAX_BOOTARGS {
            long.push('b').unwrap();
        }
        assert_eq!(
            X86LinuxManifest::encode(0x1000_0000, 0x90000, &long, false),
            Err(X86_LINUX_BOOTARGS_TOO_LONG)
        );
    }

    #[test]
    fn x86_linux_manifest_rejects_malformed_inputs() {
        // Empty and truncated headers never decode.
        assert_eq!(X86LinuxManifest::decode(&[]), None);
        let bytes = encode_default("console=ttyS0", true);
        assert_eq!(X86LinuxManifest::decode(&bytes[..HEADER_LEN - 1]), None);

        // Bad magic.
        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert_eq!(X86LinuxManifest::decode(&bad_magic), None);

        // Nonzero reserved bytes fail closed for forward compatibility.
        for reserved in [30, 31] {
            let mut bytes = bytes.clone();
            bytes[reserved] = 1;
            assert_eq!(X86LinuxManifest::decode(&bytes), None);
        }

        // Declared length longer or shorter than the actual body.
        let mut truncated = bytes.clone();
        truncated.pop().unwrap();
        assert_eq!(X86LinuxManifest::decode(&truncated), None);
        let mut lengthened = bytes.clone();
        lengthened[28] += 1;
        assert_eq!(X86LinuxManifest::decode(&lengthened), None);

        // Non-UTF8 bootargs.
        let mut invalid_utf8 = heapless::Vec::<u8, 288>::new();
        invalid_utf8
            .extend_from_slice(&bytes[..HEADER_LEN])
            .unwrap();
        invalid_utf8.extend_from_slice(&[0xff, 0xfe]).unwrap();
        let mut header_fix = invalid_utf8.clone();
        header_fix[28] = 2;
        header_fix[29] = 0;
        assert_eq!(X86LinuxManifest::decode(&header_fix), None);
    }
}
