//! Native SMM image installation helpers.
//!
//! Platform adapters open SMRAM, call [`install_pic_image`] to copy the common
//! blob and per-CPU PIC entry stubs, then close/lock SMRAM and trigger SMBASE
//! relocation.  The helper only writes bytes/data into an already-accessible
//! SMRAM mapping; chipset-specific open/close/lock and SMI triggering stay in
//! the platform drivers.

use core::mem::size_of;
use core::ptr;

use crate::header::{EntryDescriptor, HeaderError, SmmImageHeader};
use crate::layout::{
    compute_common_base, compute_cpu_layout, CpuSmmLayout, LayoutError, SmramLayout,
    SMM_ENTRY_OFFSET,
};
use crate::runtime::{CorebootModuleArgs, SmmEntryParams, SmmRuntime};

/// Inputs needed to copy a native PIC SMM image into SMRAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallConfig {
    /// Permanent SMRAM/TSEG base.
    pub smram_base: u64,
    /// Permanent SMRAM/TSEG size.
    pub smram_size: u64,
    /// Number of active logical CPUs to install.
    pub num_cpus: u16,
    /// Size of each CPU save-state area.
    pub save_state_size: u32,
    /// Optional page-table bytes reserved below the handler/data region.
    pub page_table_size: u32,
    /// CR3 value patched into every entry parameter block.
    pub cr3: u64,
    /// Platform SMI dispatch kind patched into every entry parameter block.
    pub platform_kind: u32,
    /// Platform SMI dispatch flags patched into every entry parameter block.
    pub platform_flags: u32,
    /// Opaque platform SMI dispatch data patched into every entry parameter block.
    pub platform_data: [u64; 4],
}

/// Result of installing a native PIC SMM image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstalledSmmImage<'a> {
    /// Parsed image header.
    pub header: SmmImageHeader,
    /// Address where the handler/data region was copied.
    pub common_base: u64,
    /// Absolute address of the SMM handler entry.
    pub common_entry: u64,
    /// Absolute address of the runtime block, or 0 when absent.
    pub runtime_addr: u64,
    /// Per-CPU permanent SMRAM layout used for this install.
    pub cpus: &'a [CpuSmmLayout],
}

/// Inputs for installing a minimal default-SMRAM relocation handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultRelocationConfig {
    /// Current/default SMBASE.  On x86 this is normally `0x30000`.
    pub default_smbase: u64,
    /// Permanent SMBASE to write into the current CPU save state.
    pub target_smbase: u64,
    /// Offset of the SMBASE field from `default_smbase` in this CPU's
    /// architectural save-state format (for example `0xff00` on QEMU's
    /// AMD64/legacy format, `0xfef8` on Intel EM64T101/Pineview).
    pub save_state_smbase_offset: u16,
}

/// Inputs for installing an APIC-ID-indexed default-SMRAM relocation handler.
pub struct DefaultRelocationTableConfig<'a> {
    /// Current/default SMBASE.  On x86 this is normally `0x30000`.
    pub default_smbase: u64,
    /// Permanent SMBASE table indexed by the CPU's initial xAPIC ID.
    pub target_smbases: &'a [u64],
    /// Offset of the SMBASE field from `default_smbase` in this CPU's save state.
    pub save_state_smbase_offset: u16,
}

/// Errors from [`install_pic_image`] and related installer helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallError {
    /// The native image header failed validation.
    Header(HeaderError),
    /// SMRAM placement failed.
    Layout(LayoutError),
    /// The image does not contain enough entry descriptors.
    NotEnoughEntries,
    /// An entry descriptor references bytes outside the image.
    BadEntryRange,
    /// An entry parameter block does not fit inside its copied stub.
    BadParams,
    /// Coreboot module-args storage is enabled but too small for all CPUs.
    BadModuleArgs,
    /// Address arithmetic overflowed.
    Overflow,
    /// The requested target SMBASE cannot be represented in the x86 save state.
    SmbaseOutOfRange,
}

impl From<HeaderError> for InstallError {
    fn from(value: HeaderError) -> Self {
        Self::Header(value)
    }
}

impl From<LayoutError> for InstallError {
    fn from(value: LayoutError) -> Self {
        Self::Layout(value)
    }
}

/// Copy a native PIC SMM image into accessible SMRAM and patch data blocks.
///
/// # Safety
///
/// `config.smram_base..smram_base + smram_size` must be mapped, writable, and
/// exclusively owned by the caller for the duration of this function.  The
/// caller must have opened SMRAM/TSEG in the chipset before calling and must
/// close/lock it afterwards according to platform policy.
pub unsafe fn install_pic_image<'a>(
    image: &[u8],
    config: InstallConfig,
    cpu_layouts: &'a mut [CpuSmmLayout],
) -> Result<InstalledSmmImage<'a>, InstallError> {
    let header = SmmImageHeader::parse(image)?;
    if header.entry_count < config.num_cpus {
        return Err(InstallError::NotEnoughEntries);
    }

    let mut max_stub_size = 0u32;
    for i in 0..config.num_cpus {
        let entry = header.entry(image, i)?;
        check_entry_range(&header, image, &entry)?;
        max_stub_size = max_stub_size.max(entry.stub_size);
    }

    let layout = SmramLayout {
        smram_base: config.smram_base,
        smram_size: config.smram_size,
        entry_count: config.num_cpus,
        save_state_size: config.save_state_size,
        stack_size: header.stack_size,
        entry_stub_size: max_stub_size,
        common_size: header.common_size,
        page_table_size: config.page_table_size,
    };
    let common_base = compute_common_base(&layout)?;
    let cpus = compute_cpu_layout(&layout, cpu_layouts)?;

    ptr::copy_nonoverlapping(
        image.as_ptr().add(header.common_offset as usize),
        common_base as *mut u8,
        header.common_size as usize,
    );

    let runtime_addr = if header.runtime_offset != 0 {
        let mut runtime = SmmRuntime::new(
            config.smram_base,
            config.smram_size,
            config.num_cpus,
            config.save_state_size as u16,
            header.stack_size,
            header.common_offset,
            header.entries_offset,
        );
        for (i, cpu) in cpus.iter().enumerate() {
            runtime.save_state_top[i] = cpu.save_state_top;
        }
        let addr = common_base
            .checked_add(header.runtime_offset as u64)
            .ok_or(InstallError::Overflow)?;
        ptr::write_unaligned(addr as *mut SmmRuntime, runtime);
        addr
    } else {
        0
    };

    let module_args_base = if header.module_args_offset != 0 {
        let needed = size_of::<CorebootModuleArgs>()
            .checked_mul(config.num_cpus as usize)
            .ok_or(InstallError::Overflow)?;
        if (header.module_args_size as usize) < needed {
            return Err(InstallError::BadModuleArgs);
        }
        Some(
            common_base
                .checked_add((header.module_args_offset - header.common_offset) as u64)
                .ok_or(InstallError::Overflow)?,
        )
    } else {
        None
    };

    let common_entry = common_base
        .checked_add(header.common_entry_offset as u64)
        .ok_or(InstallError::Overflow)?;

    for (i, cpu) in cpus.iter().enumerate() {
        let entry = header.entry(image, i as u16)?;
        ptr::copy_nonoverlapping(
            image.as_ptr().add(entry.stub_offset as usize),
            cpu.entry_addr as *mut u8,
            entry.stub_size as usize,
        );

        let coreboot_module_args = if let Some(base) = module_args_base {
            let addr = base
                .checked_add((i * size_of::<CorebootModuleArgs>()) as u64)
                .ok_or(InstallError::Overflow)?;
            ptr::write_unaligned(
                addr as *mut CorebootModuleArgs,
                CorebootModuleArgs {
                    cpu: i as u64,
                    canary: cpu.stack_bottom,
                },
            );
            ptr::write_unaligned(cpu.stack_bottom as *mut u64, cpu.stack_bottom);
            addr
        } else {
            0
        };

        if entry.params_offset != 0 {
            let params_end = entry
                .params_offset
                .checked_add(size_of::<SmmEntryParams>() as u32)
                .ok_or(InstallError::Overflow)?;
            if params_end > entry.stub_size {
                return Err(InstallError::BadParams);
            }
            let params_addr = cpu
                .entry_addr
                .checked_add(entry.params_offset as u64)
                .ok_or(InstallError::Overflow)?;
            ptr::write_unaligned(
                params_addr as *mut SmmEntryParams,
                SmmEntryParams {
                    cpu: i as u32,
                    stack_size: header.stack_size,
                    stack_top: cpu.stack_top,
                    common_entry,
                    runtime: runtime_addr,
                    coreboot_module_args,
                    cr3: config.cr3,
                    entry_base: cpu.entry_addr,
                    platform_kind: config.platform_kind,
                    platform_flags: config.platform_flags,
                    platform_data: config.platform_data,
                },
            );
        }
    }

    Ok(InstalledSmmImage {
        header,
        common_base,
        common_entry,
        runtime_addr,
        cpus,
    })
}

/// Install a tiny 16-bit default-SMRAM relocation handler.
///
/// This single-target helper is kept for BSP-only flows and tests.  Multi-CPU
/// relocation should use [`install_default_relocation_table_handler`].
///
/// # Safety
///
/// The caller must have opened the chipset's default SMRAM/ASEG window, and
/// `default_smbase + 0x8000` must be writable.
pub unsafe fn install_default_relocation_handler(
    config: DefaultRelocationConfig,
) -> Result<(), InstallError> {
    install_default_relocation_table_handler(DefaultRelocationTableConfig {
        default_smbase: config.default_smbase,
        target_smbases: core::slice::from_ref(&config.target_smbase),
        save_state_smbase_offset: config.save_state_smbase_offset,
    })
}

/// Install an APIC-ID-indexed 16-bit default-SMRAM relocation handler.
///
/// The handler runs at the architectural default SMM entry point, reads the
/// CPU's initial xAPIC ID with CPUID leaf 1, masks it to the current 64-entry
/// ABI cap, looks up the permanent SMBASE in a patched table, writes that value
/// into the current CPU save state, and RSMs.
/// This avoids coreboot's relocatable-module trick while still allowing all CPUs
/// to relocate through one default-SMRAM entry during the MP flight plan.
///
/// # Safety
///
/// The caller must have opened the chipset's default SMRAM/ASEG window, and
/// `default_smbase + 0x8000` must be writable.  `target_smbases` is indexed by
/// initial xAPIC ID modulo 64; platforms with sparse or high APIC IDs should
/// prefill unused entries with a safe fallback SMBASE.
pub unsafe fn install_default_relocation_table_handler(
    config: DefaultRelocationTableConfig<'_>,
) -> Result<(), InstallError> {
    if config.target_smbases.is_empty() {
        return Err(InstallError::NotEnoughEntries);
    }

    let entry = config
        .default_smbase
        .checked_add(SMM_ENTRY_OFFSET)
        .ok_or(InstallError::Overflow)? as *mut u8;

    let table_offset = align_up(DEFAULT_RELOCATION_HANDLER.len(), 4)?;
    let table_bytes = config
        .target_smbases
        .len()
        .checked_mul(size_of::<u32>())
        .ok_or(InstallError::Overflow)?;
    let total = table_offset
        .checked_add(table_bytes)
        .ok_or(InstallError::Overflow)?;
    if total as u64 >= SMM_ENTRY_OFFSET {
        return Err(InstallError::BadEntryRange);
    }

    let mut code = DEFAULT_RELOCATION_HANDLER;
    let table_disp = (SMM_ENTRY_OFFSET as usize)
        .checked_add(table_offset)
        .ok_or(InstallError::Overflow)?;
    code[DEFAULT_RELOCATION_TABLE_PATCH..DEFAULT_RELOCATION_TABLE_PATCH + 2]
        .copy_from_slice(&(table_disp as u16).to_le_bytes());
    code[DEFAULT_RELOCATION_SAVE_STATE_PATCH..DEFAULT_RELOCATION_SAVE_STATE_PATCH + 2]
        .copy_from_slice(&config.save_state_smbase_offset.to_le_bytes());

    ptr::copy_nonoverlapping(code.as_ptr(), entry, code.len());
    for i in code.len()..table_offset {
        ptr::write(entry.add(i), 0x90);
    }
    for (i, smbase) in config.target_smbases.iter().enumerate() {
        let target = u32::try_from(*smbase).map_err(|_| InstallError::SmbaseOutOfRange)?;
        ptr::write_unaligned(entry.add(table_offset + i * 4) as *mut u32, target);
    }
    Ok(())
}

const DEFAULT_RELOCATION_TABLE_PATCH: usize = 28;
const DEFAULT_RELOCATION_SAVE_STATE_PATCH: usize = 32;

// 16-bit code:
//   push cs; pop ds
//   mov eax, 1; cpuid
//   shr ebx, 24              ; EBX = initial xAPIC ID
//   and ebx, 0x3f             ; current ABI cap is 64 entries
//   shl ebx, 2               ; table index
//   mov eax, [bx + table]
//   mov [save_state_smbase], eax
//   rsm
//   hlt; jmp $-1             ; defensive fallthrough
const DEFAULT_RELOCATION_HANDLER: [u8; 39] = [
    0x0e, 0x1f, 0x66, 0xb8, 0x01, 0x00, 0x00, 0x00, 0x0f, 0xa2, 0x66, 0xc1, 0xeb, 0x18, 0x66, 0x81,
    0xe3, 0x3f, 0x00, 0x00, 0x00, 0x66, 0xc1, 0xe3, 0x02, 0x66, 0x8b, 0x87, 0x00, 0x00, 0x66, 0xa3,
    0x00, 0x00, 0x0f, 0xaa, 0xf4, 0xeb, 0xfd,
];

fn align_up(value: usize, align: usize) -> Result<usize, InstallError> {
    debug_assert!(align.is_power_of_two());
    value
        .checked_add(align - 1)
        .map(|v| v & !(align - 1))
        .ok_or(InstallError::Overflow)
}

fn check_entry_range(
    header: &SmmImageHeader,
    image: &[u8],
    entry: &EntryDescriptor,
) -> Result<(), InstallError> {
    let end = entry
        .stub_offset
        .checked_add(entry.stub_size)
        .ok_or(InstallError::BadEntryRange)?;
    if entry.stub_offset < header.header_size as u32
        || end > header.image_size
        || end as usize > image.len()
    {
        return Err(InstallError::BadEntryRange);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;

    use super::*;
    use crate::layout::SMM_ENTRY_OFFSET;

    fn put_u32(image: &mut [u8], off: usize, v: u32) {
        image[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn put_u16(image: &mut [u8], off: usize, v: u16) {
        image[off..off + 2].copy_from_slice(&v.to_le_bytes());
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

    #[test]
    fn installs_common_stub_runtime_and_params() {
        let header_size = size_of::<SmmImageHeader>() as u32;
        let entries_offset = header_size;
        let common_offset = 0x80;
        let common_size = 0x300;
        let stub_offset = 0x400;
        let stub_size = 0x80;
        let params_offset = 0x20;
        let image_size = stub_offset + stub_size;
        let header = SmmImageHeader::new(
            0,
            image_size,
            1,
            entries_offset,
            common_offset,
            common_size,
            0,
            0x20,
            0,
            0,
            0x400,
        );
        let mut image = vec![0u8; image_size as usize];
        put_header(&mut image, &header);
        put_u32(&mut image, entries_offset as usize, stub_offset);
        put_u32(&mut image, entries_offset as usize + 4, stub_size);
        put_u32(&mut image, entries_offset as usize + 8, 0);
        put_u32(&mut image, entries_offset as usize + 12, params_offset);
        image[common_offset as usize] = 0xaa;
        image[stub_offset as usize] = 0xbb;

        let mut smram = vec![0u8; 0x4_0000];
        let smram_base = smram.as_mut_ptr() as u64;
        let mut cpus = [CpuSmmLayout {
            smbase: 0,
            entry_addr: 0,
            save_state_base: 0,
            save_state_top: 0,
            stack_bottom: 0,
            stack_top: 0,
        }; 1];

        let installed = unsafe {
            install_pic_image(
                &image,
                InstallConfig {
                    smram_base,
                    smram_size: smram.len() as u64,
                    num_cpus: 1,
                    save_state_size: 0x400,
                    page_table_size: 0,
                    cr3: 0x1234,
                    platform_kind: crate::runtime::SMM_PLATFORM_INTEL_ICH,
                    platform_flags: 0,
                    platform_data: [0x600, 0x20, 0, 0],
                },
                &mut cpus,
            )
        }
        .unwrap();

        assert_eq!(unsafe { *(installed.common_base as *const u8) }, 0xaa);
        assert_eq!(
            unsafe { *(installed.cpus[0].entry_addr as *const u8) },
            0xbb
        );
        assert_eq!(
            installed.cpus[0].entry_addr,
            installed.cpus[0].smbase + SMM_ENTRY_OFFSET
        );

        let params = unsafe {
            ptr::read_unaligned(
                (installed.cpus[0].entry_addr + params_offset as u64) as *const SmmEntryParams,
            )
        };
        assert_eq!(params.cpu, 0);
        assert_eq!(params.common_entry, installed.common_entry);
        assert_eq!(params.runtime, installed.runtime_addr);
        assert_eq!(params.cr3, 0x1234);
        assert_eq!(params.platform_kind, crate::runtime::SMM_PLATFORM_INTEL_ICH);
        assert_eq!(params.platform_data[0], 0x600);
        assert_eq!(params.platform_data[1], 0x20);
    }

    #[test]
    fn installs_default_relocation_handler_bytes() {
        let mut smram = vec![0u8; 0x1_0000 + 16];
        let default_smbase = smram.as_mut_ptr() as u64;
        unsafe {
            install_default_relocation_handler(DefaultRelocationConfig {
                default_smbase,
                target_smbase: 0x7ff8_0000,
                save_state_smbase_offset: 0xff00,
            })
        }
        .unwrap();

        let entry = SMM_ENTRY_OFFSET as usize;
        assert_eq!(&smram[entry..entry + 2], &[0x0e, 0x1f]);
        assert_eq!(&smram[entry + 28..entry + 30], &[0x28, 0x80]);
        assert_eq!(&smram[entry + 32..entry + 34], &[0x00, 0xff]);
        assert_eq!(
            &smram[entry + 40..entry + 44],
            &0x7ff8_0000u32.to_le_bytes()
        );
    }
}
