//! Native, deliberately unversioned SMM image header.
//!
//! The image is an x86 little-endian ABI; the wire structs below have no
//! padding, and zerocopy reads them without requiring aligned input.

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

#[cfg(target_endian = "big")]
compile_error!("The native x86 SMM image format requires a little-endian build host");

/// Magic value at the start of every native fstart SMM image: `FSMM`.
pub const SMM_IMAGE_MAGIC: u32 = u32::from_le_bytes(*b"FSMM");

/// `SmmImageHeader.flags`: image contains a coreboot module-args block.
pub const FLAG_COREBOOT_MODULE_ARGS: u32 = 1 << 0;
/// `SmmImageHeader.flags`: image build requested coreboot C header output.
pub const FLAG_COREBOOT_HEADER: u32 = 1 << 1;

/// Errors returned while validating an SMM image header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    /// The byte slice is shorter than the fixed header.
    TooSmall,
    /// The magic value is not [`SMM_IMAGE_MAGIC`].
    BadMagic,
    /// A fixed-size field does not match this build's ABI.
    BadHeaderSize,
    /// A file range points outside the image or overflows.
    RangeOutOfBounds,
    /// The descriptor table does not contain enough entries.
    NotEnoughEntries,
    /// A handler-memory range is outside the memory image or overlaps the
    /// initialized bytes.
    BadMemoryImage,
}

/// Description of one relocatable handler memory image.
///
/// `handler_offset` and entry-stub offsets address bytes in the file. Every
/// other handler/runtime offset is relative to the copied handler memory base.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub struct SmmImageHeader {
    /// [`SMM_IMAGE_MAGIC`].
    pub magic: u32,
    /// Size of this fixed header in bytes.
    pub header_size: u16,
    /// Size of each [`EntryDescriptor`].
    pub entry_desc_size: u16,
    /// Feature bits (`FLAG_*`).
    pub flags: u32,
    /// Total image size in bytes.
    pub image_size: u32,
    /// Number of precompiled entry stubs.
    pub entry_count: u16,
    /// Reserved; zero.
    pub reserved: u16,
    /// File offset of the descriptor table.
    pub entries_offset: u32,
    /// File offset of the initialized handler bytes.
    pub handler_offset: u32,
    /// Initialized `.text/.rodata/.data` bytes present in the file.
    pub handler_load_size: u32,
    /// Complete in-SMRAM extent, including `.bss` and loader-owned blocks.
    pub handler_mem_size: u32,
    /// Handler-memory offset of `fstart_smm_handler`.
    pub handler_entry_offset: u32,
    /// Handler-memory offset of the [`SmmRuntime`](crate::SmmRuntime) block.
    pub runtime_offset: u32,
    /// Bytes reserved for the runtime block.
    pub runtime_size: u32,
    /// Handler-memory offset of the handler configuration block.
    pub handler_config_offset: u32,
    /// Bytes reserved for the handler configuration.
    pub handler_config_capacity: u32,
    /// Handler-memory offset of the coreboot module-args block, or 0.
    pub module_args_offset: u32,
    /// Size of the coreboot module-args block, or 0.
    pub module_args_size: u32,
    /// Per-CPU SMM stack size expected by the stubs.
    pub stack_size: u32,
}

impl SmmImageHeader {
    /// Construct a header with the ABI constants filled in.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        flags: u32,
        image_size: u32,
        entry_count: u16,
        entries_offset: u32,
        handler_offset: u32,
        handler_load_size: u32,
        handler_mem_size: u32,
        handler_entry_offset: u32,
        runtime_offset: u32,
        runtime_size: u32,
        handler_config_offset: u32,
        handler_config_capacity: u32,
        module_args_offset: u32,
        module_args_size: u32,
        stack_size: u32,
    ) -> Self {
        Self {
            magic: SMM_IMAGE_MAGIC,
            header_size: core::mem::size_of::<Self>() as u16,
            entry_desc_size: core::mem::size_of::<EntryDescriptor>() as u16,
            flags,
            image_size,
            entry_count,
            reserved: 0,
            entries_offset,
            handler_offset,
            handler_load_size,
            handler_mem_size,
            handler_entry_offset,
            runtime_offset,
            runtime_size,
            handler_config_offset,
            handler_config_capacity,
            module_args_offset,
            module_args_size,
            stack_size,
        }
    }

    /// Parse and validate the fixed header from little-endian bytes.
    pub fn parse(image: &[u8]) -> Result<Self, HeaderError> {
        let (h, _) = Self::read_from_prefix(image).map_err(|_| HeaderError::TooSmall)?;
        if h.magic != SMM_IMAGE_MAGIC {
            return Err(HeaderError::BadMagic);
        }
        if h.header_size as usize != core::mem::size_of::<Self>()
            || h.entry_desc_size as usize != core::mem::size_of::<EntryDescriptor>()
        {
            return Err(HeaderError::BadHeaderSize);
        }
        if h.image_size as usize > image.len() {
            return Err(HeaderError::RangeOutOfBounds);
        }
        h.check_file_range(
            h.entries_offset,
            (h.entry_count as u32)
                .checked_mul(h.entry_desc_size as u32)
                .ok_or(HeaderError::RangeOutOfBounds)?,
        )?;
        h.check_file_range(h.handler_offset, h.handler_load_size)?;
        if h.handler_load_size > h.handler_mem_size || h.handler_entry_offset >= h.handler_load_size
        {
            return Err(HeaderError::BadMemoryImage);
        }
        h.check_memory_range(h.runtime_offset, h.runtime_size)?;
        h.check_memory_range(h.handler_config_offset, h.handler_config_capacity)?;
        if h.module_args_offset != 0 || h.module_args_size != 0 {
            h.check_memory_range(h.module_args_offset, h.module_args_size)?;
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
        EntryDescriptor::read_from_prefix(&image[off..end])
            .map(|(descriptor, _)| descriptor)
            .map_err(|_| HeaderError::RangeOutOfBounds)
    }

    fn check_file_range(&self, offset: u32, size: u32) -> Result<(), HeaderError> {
        let end = offset
            .checked_add(size)
            .ok_or(HeaderError::RangeOutOfBounds)?;
        if offset < self.header_size as u32 || end > self.image_size {
            return Err(HeaderError::RangeOutOfBounds);
        }
        Ok(())
    }

    fn check_memory_range(&self, offset: u32, size: u32) -> Result<(), HeaderError> {
        let end = offset
            .checked_add(size)
            .ok_or(HeaderError::BadMemoryImage)?;
        if size == 0 || offset < self.handler_load_size || end > self.handler_mem_size {
            return Err(HeaderError::BadMemoryImage);
        }
        Ok(())
    }
}

/// One precompiled PIC SMM entry stub.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub struct EntryDescriptor {
    /// File offset of this stub's bytes.
    pub stub_offset: u32,
    /// Number of bytes to copy to `SMBASE + 0x8000`.
    pub stub_size: u32,
    /// Offset within the stub where CPU entry begins; usually zero.
    pub entry_offset: u32,
    /// Offset within the stub of its [`SmmEntryParams`](crate::SmmEntryParams).
    pub params_offset: u32,
}

/// Offsets emitted as C preprocessor constants for coreboot loaders.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorebootOffsets {
    pub native_header: u32,
    pub entries: u32,
    pub handler: u32,
    pub handler_entry: u32,
    pub handler_load_size: u32,
    pub handler_mem_size: u32,
    pub runtime: u32,
    pub module_args: u32,
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
    let _ = writeln!(out, "/* Generated by fstart-image-build. */");
    let _ = writeln!(out, "#pragma once");
    for (name, value) in [
        ("NATIVE_HEADER_OFFSET", offsets.native_header),
        ("ENTRY_COUNT", u32::from(offsets.entry_count)),
        ("ENTRY_DESC_SIZE", u32::from(entry_desc_size)),
        ("ENTRIES_OFFSET", offsets.entries),
        ("HANDLER_OFFSET", offsets.handler),
        ("HANDLER_ENTRY_OFFSET", offsets.handler_entry),
        ("HANDLER_LOAD_SIZE", offsets.handler_load_size),
        ("HANDLER_MEM_SIZE", offsets.handler_mem_size),
        ("RUNTIME_OFFSET", offsets.runtime),
        ("MODULE_ARGS_OFFSET", offsets.module_args),
    ] {
        let _ = writeln!(out, "#define FSTART_SMM_{name} {value}u");
    }
    out
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn unversioned_header_validates_file_and_memory_ranges() {
        let hs = core::mem::size_of::<SmmImageHeader>() as u32;
        let ds = core::mem::size_of::<EntryDescriptor>() as u32;
        let handler = hs + ds;
        let h = SmmImageHeader::new(
            0,
            handler + 16,
            1,
            hs,
            handler,
            16,
            128,
            0,
            16,
            64,
            80,
            16,
            96,
            16,
            0x400,
        );
        let mut image = std::vec![0u8; h.image_size as usize];
        h.write_to_prefix(&mut image).unwrap();
        EntryDescriptor {
            stub_offset: handler,
            stub_size: 16,
            entry_offset: 0,
            params_offset: 8,
        }
        .write_to_prefix(&mut image[hs as usize..])
        .unwrap();
        assert_eq!(SmmImageHeader::parse(&image).unwrap(), h);
        image[0..4].copy_from_slice(b"FSM1");
        assert_eq!(SmmImageHeader::parse(&image), Err(HeaderError::BadMagic));
    }
}
