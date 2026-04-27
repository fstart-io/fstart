//! Native SMM image header and optional coreboot-compatible offset header.

/// Magic value at the start of every native fstart SMM image: `FSM1`.
pub const SMM_IMAGE_MAGIC: u32 = u32::from_le_bytes(*b"FSM1");
/// Current native header version.
pub const SMM_IMAGE_VERSION: u16 = 1;

/// `SmmImageHeader.flags`: image contains a coreboot module-args block.
pub const FLAG_COREBOOT_MODULE_ARGS: u32 = 1 << 0;
/// `SmmImageHeader.flags`: image build requested coreboot C header output.
pub const FLAG_COREBOOT_HEADER: u32 = 1 << 1;

/// Errors returned while validating/parsing an SMM image header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    /// The byte slice is shorter than the fixed header.
    TooSmall,
    /// The magic value is not [`SMM_IMAGE_MAGIC`].
    BadMagic,
    /// The header version is unsupported.
    BadVersion,
    /// A fixed-size field is inconsistent with the supported ABI.
    BadHeaderSize,
    /// An image-relative range points outside the image or overflows.
    RangeOutOfBounds,
    /// The descriptor table does not contain enough entries.
    NotEnoughEntries,
}

/// Native fstart SMM image header.
///
/// All offsets are relative to the start of the image.  Code bytes are PIC;
/// loaders may copy the referenced ranges into SMRAM but must not apply a
/// relocation pass to code.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmmImageHeader {
    /// [`SMM_IMAGE_MAGIC`].
    pub magic: u32,
    /// [`SMM_IMAGE_VERSION`].
    pub version: u16,
    /// Size of this fixed header in bytes.
    pub header_size: u16,
    /// Feature bits (`FLAG_*`).
    pub flags: u32,
    /// Total image size in bytes.
    pub image_size: u32,
    /// Number of valid [`EntryDescriptor`] records.
    pub entry_count: u16,
    /// Size of each descriptor record.
    pub entry_desc_size: u16,
    /// Offset of the descriptor table.
    pub entries_offset: u32,
    /// Common handler/data blob offset.
    pub common_offset: u32,
    /// Common handler/data blob size.
    pub common_size: u32,
    /// Offset inside the copied handler/data region to the SMM handler entry.
    pub common_entry_offset: u32,
    /// Offset inside the copied handler/data region to the runtime block, or 0 when
    /// the image does not expose a loader-filled runtime block.
    pub runtime_offset: u32,
    /// Optional coreboot module-args block offset, or 0 when absent.
    pub module_args_offset: u32,
    /// Optional coreboot module-args block size, or 0 when absent.
    pub module_args_size: u32,
    /// Per-CPU SMM stack size expected by the stubs.
    pub stack_size: u32,
}

impl SmmImageHeader {
    /// Construct a header with ABI constants filled in.
    pub const fn new(
        flags: u32,
        image_size: u32,
        entry_count: u16,
        entries_offset: u32,
        common_offset: u32,
        common_size: u32,
        common_entry_offset: u32,
        runtime_offset: u32,
        module_args_offset: u32,
        module_args_size: u32,
        stack_size: u32,
    ) -> Self {
        Self {
            magic: SMM_IMAGE_MAGIC,
            version: SMM_IMAGE_VERSION,
            header_size: core::mem::size_of::<Self>() as u16,
            flags,
            image_size,
            entry_count,
            entry_desc_size: core::mem::size_of::<EntryDescriptor>() as u16,
            entries_offset,
            common_offset,
            common_size,
            common_entry_offset,
            runtime_offset,
            module_args_offset,
            module_args_size,
            stack_size,
        }
    }

    /// Parse and validate the fixed header from little-endian bytes.
    pub fn parse(image: &[u8]) -> Result<Self, HeaderError> {
        if image.len() < core::mem::size_of::<Self>() {
            return Err(HeaderError::TooSmall);
        }
        let h = Self {
            magic: read_u32(image, 0),
            version: read_u16(image, 4),
            header_size: read_u16(image, 6),
            flags: read_u32(image, 8),
            image_size: read_u32(image, 12),
            entry_count: read_u16(image, 16),
            entry_desc_size: read_u16(image, 18),
            entries_offset: read_u32(image, 20),
            common_offset: read_u32(image, 24),
            common_size: read_u32(image, 28),
            common_entry_offset: read_u32(image, 32),
            runtime_offset: read_u32(image, 36),
            module_args_offset: read_u32(image, 40),
            module_args_size: read_u32(image, 44),
            stack_size: read_u32(image, 48),
        };
        if h.magic != SMM_IMAGE_MAGIC {
            return Err(HeaderError::BadMagic);
        }
        if h.version != SMM_IMAGE_VERSION {
            return Err(HeaderError::BadVersion);
        }
        if h.header_size as usize != core::mem::size_of::<Self>()
            || h.entry_desc_size as usize != core::mem::size_of::<EntryDescriptor>()
        {
            return Err(HeaderError::BadHeaderSize);
        }
        if h.image_size as usize > image.len() {
            return Err(HeaderError::RangeOutOfBounds);
        }
        h.check_range(
            h.entries_offset,
            (h.entry_count as u32) * (h.entry_desc_size as u32),
        )?;
        h.check_range(h.common_offset, h.common_size)?;
        if h.common_entry_offset >= h.common_size {
            return Err(HeaderError::RangeOutOfBounds);
        }
        if h.runtime_offset != 0 && h.runtime_offset >= h.common_size {
            return Err(HeaderError::RangeOutOfBounds);
        }
        if h.module_args_offset != 0 || h.module_args_size != 0 {
            h.check_range(h.module_args_offset, h.module_args_size)?;
        }
        Ok(h)
    }

    /// Return the `index`th entry descriptor.
    pub fn entry(&self, image: &[u8], index: u16) -> Result<EntryDescriptor, HeaderError> {
        if index >= self.entry_count {
            return Err(HeaderError::NotEnoughEntries);
        }
        let off = (self.entries_offset as usize)
            .checked_add(index as usize * core::mem::size_of::<EntryDescriptor>())
            .ok_or(HeaderError::RangeOutOfBounds)?;
        let end = off
            .checked_add(core::mem::size_of::<EntryDescriptor>())
            .ok_or(HeaderError::RangeOutOfBounds)?;
        if end > image.len() || end > self.image_size as usize {
            return Err(HeaderError::RangeOutOfBounds);
        }
        Ok(EntryDescriptor {
            stub_offset: read_u32(image, off),
            stub_size: read_u32(image, off + 4),
            entry_offset: read_u32(image, off + 8),
            params_offset: read_u32(image, off + 12),
        })
    }

    fn check_range(&self, offset: u32, size: u32) -> Result<(), HeaderError> {
        let end = offset
            .checked_add(size)
            .ok_or(HeaderError::RangeOutOfBounds)?;
        if offset < self.header_size as u32 || end > self.image_size {
            return Err(HeaderError::RangeOutOfBounds);
        }
        Ok(())
    }
}

/// One precompiled PIC SMM entry stub descriptor.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryDescriptor {
    /// Image-relative start of this stub's bytes.
    pub stub_offset: u32,
    /// Number of bytes to copy to `SMBASE + 0x8000`.
    pub stub_size: u32,
    /// Offset within the stub where CPU entry begins.  Usually zero.
    pub entry_offset: u32,
    /// Offset within the stub to its PIC parameter block, or zero if the
    /// precompiled stub discovers all state by RIP-relative addressing.
    pub params_offset: u32,
}

/// Header values emitted as C preprocessor constants for coreboot builds.
///
/// The actual `.h` generation lives in the image build script; this struct is
/// the canonical field set used by tests and tooling.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorebootOffsets {
    /// Offset of the native [`SmmImageHeader`].
    pub native_header: u32,
    /// Offset of the entry descriptor table.
    pub entries: u32,
    /// Offset of the SMM handler/data region.
    pub common: u32,
    /// Offset of the SMM handler entry inside the copied handler/data region.
    pub common_entry: u32,
    /// Offset of the runtime block inside the copied handler/data region, or 0.
    pub runtime: u32,
    /// Optional module-arguments offset, or 0 when disabled.
    pub module_args: u32,
    /// Number of precompiled entry stubs.
    pub entry_count: u16,
}

/// Render the coreboot-compatible C header body.
#[cfg(all(feature = "std", feature = "coreboot"))]
pub fn render_coreboot_header(
    offsets: CorebootOffsets,
    entry_desc_size: u16,
) -> std::string::String {
    use std::fmt::Write;

    let mut out = std::string::String::new();
    let _ = writeln!(out, "/* Generated by fstart-smm-image. */");
    let _ = writeln!(out, "#pragma once");
    let _ = writeln!(
        out,
        "#define FSTART_SMM_NATIVE_HEADER_OFFSET {}u",
        offsets.native_header
    );
    let _ = writeln!(
        out,
        "#define FSTART_SMM_ENTRY_COUNT {}u",
        offsets.entry_count
    );
    let _ = writeln!(
        out,
        "#define FSTART_SMM_ENTRY_DESC_SIZE {}u",
        entry_desc_size
    );
    let _ = writeln!(
        out,
        "#define FSTART_SMM_ENTRIES_OFFSET {}u",
        offsets.entries
    );
    let _ = writeln!(out, "#define FSTART_SMM_COMMON_OFFSET {}u", offsets.common);
    let _ = writeln!(
        out,
        "#define FSTART_SMM_COMMON_ENTRY_OFFSET {}u",
        offsets.common_entry
    );
    let _ = writeln!(
        out,
        "#define FSTART_SMM_RUNTIME_OFFSET {}u",
        offsets.runtime
    );
    let _ = writeln!(
        out,
        "#define FSTART_SMM_MODULE_ARGS_OFFSET {}u",
        offsets.module_args
    );
    out
}

fn read_u16(bytes: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([bytes[off], bytes[off + 1]])
}

fn read_u32(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn parses_header_and_entry() {
        let header_size = core::mem::size_of::<SmmImageHeader>() as u32;
        let desc_size = core::mem::size_of::<EntryDescriptor>() as u32;
        let common_off = header_size + desc_size;
        let image_size = common_off + 4;
        let h = SmmImageHeader::new(
            0,
            image_size,
            1,
            header_size,
            common_off,
            4,
            0,
            0,
            0,
            0,
            0x400,
        );
        let mut image = std::vec![0u8; image_size as usize];
        put_header(&mut image, &h);
        put_u32(&mut image, header_size as usize, common_off + 4);
        put_u32(&mut image, header_size as usize + 4, 16);
        put_u32(&mut image, header_size as usize + 8, 0);
        put_u32(&mut image, header_size as usize + 12, 8);

        let parsed = SmmImageHeader::parse(&image).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(
            parsed.entry(&image, 0).unwrap(),
            EntryDescriptor {
                stub_offset: common_off + 4,
                stub_size: 16,
                entry_offset: 0,
                params_offset: 8,
            }
        );
    }

    fn put_header(image: &mut [u8], h: &SmmImageHeader) {
        put_u32(image, 0, h.magic);
        put_u16(image, 4, h.version);
        put_u16(image, 6, h.header_size);
        put_u32(image, 8, h.flags);
        put_u32(image, 12, h.image_size);
        put_u16(image, 16, h.entry_count);
        put_u16(image, 18, h.entry_desc_size);
        put_u32(image, 20, h.entries_offset);
        put_u32(image, 24, h.common_offset);
        put_u32(image, 28, h.common_size);
        put_u32(image, 32, h.common_entry_offset);
        put_u32(image, 36, h.runtime_offset);
        put_u32(image, 40, h.module_args_offset);
        put_u32(image, 44, h.module_args_size);
        put_u32(image, 48, h.stack_size);
    }

    fn put_u16(image: &mut [u8], off: usize, v: u16) {
        image[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }

    fn put_u32(image: &mut [u8], off: usize, v: u32) {
        image[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    #[cfg(all(feature = "std", feature = "coreboot"))]
    #[test]
    fn renders_coreboot_header() {
        let h = render_coreboot_header(
            CorebootOffsets {
                native_header: 0,
                entries: 44,
                common: 108,
                common_entry: 16,
                runtime: 256,
                module_args: 512,
                entry_count: 4,
            },
            core::mem::size_of::<EntryDescriptor>() as u16,
        );
        assert!(h.contains("FSTART_SMM_ENTRY_COUNT 4u"));
        assert!(h.contains("FSTART_SMM_MODULE_ARGS_OFFSET 512u"));
    }
}
