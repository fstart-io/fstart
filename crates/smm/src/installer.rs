//! Native SMM image installation helpers.

use core::mem::{align_of, size_of};
use core::ptr;

use crate::header::{EntryDescriptor, FLAG_COREBOOT_MODULE_ARGS, HeaderError, SmmImageHeader};
use crate::layout::{
    CpuSmmLayout, LayoutError, SMM_ENTRY_OFFSET, SMM_IDENTITY_TABLE_SIZE,
    SMM_RELOCATION_TABLE_OFFSET, SmramLayout, build_identity_tables, compute_common_base,
    compute_cpu_layout, compute_page_table_base,
};
use crate::runtime::{
    CorebootModuleArgs, HANDLER_CONFIG_ALIGNMENT, HANDLER_CONFIG_CAPACITY, SmmEntryParams,
    SmmRuntime,
};

/// Placement and handler inputs for [`install_pic_image`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallConfig<'a, T: Copy> {
    /// Permanent SMRAM/TSEG base.
    pub smram_base: u64,
    /// Permanent SMRAM/TSEG size.
    pub smram_size: u64,
    /// Number of CPUs to give a permanent entry.
    pub num_cpus: u16,
    /// Per-CPU save-state size reserved at the top of each SMBASE window.
    pub save_state_size: u32,
    /// Configuration of the concrete handler, [`SmmHandler::Config`](crate::SmmHandler::Config).
    pub handler_config: &'a T,
}

/// Addresses of an installed image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstalledSmmImage<'a> {
    /// Parsed image header.
    pub header: SmmImageHeader,
    /// SMRAM address of the copied handler memory image.
    pub common_base: u64,
    /// SMRAM address of `fstart_smm_handler`.
    pub common_entry: u64,
    /// SMRAM address of the [`SmmRuntime`] block.
    pub runtime_addr: u64,
    /// Permanent page tables inside SMRAM. Every entry stub loads this CR3.
    pub cr3: u64,
    /// Per-CPU SMBASE, entry, save-state and stack placement.
    pub cpus: &'a [CpuSmmLayout],
}

/// Inputs for the temporary default-SMBASE relocation stub.
pub struct DefaultRelocationCallbackConfig {
    /// Architectural default SMBASE (normally `0x30000`).
    pub default_smbase: u64,
    /// Identity-map CR3 loaded by the stub.
    pub cr3: u64,
    /// Normal-mode function called from SMM.
    pub callback: u64,
    /// Argument passed to `callback` through [`SmmEntryParams::runtime`].
    pub callback_arg: u64,
    /// Stack top used by the serialized relocation SMI.
    pub stack_top: u64,
}

/// Installation failure. Every variant is reported before the first SMRAM write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallError {
    Header(HeaderError),
    Layout(LayoutError),
    NotEnoughEntries,
    BadEntryRange,
    BadParams,
    BadRuntime,
    BadHandlerConfig,
    BadModuleArgs,
    BadAlignment,
    AddressAbove4G,
    Overflow,
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

/// Copy and initialize one SMM image in an open SMRAM window.
///
/// # Safety
///
/// The configured SMRAM range must be writable and exclusively owned. The
/// caller must close and lock it only after every CPU has relocated.
pub unsafe fn install_pic_image<'a, T: Copy>(
    image: &[u8],
    config: InstallConfig<'_, T>,
    cpu_layouts: &'a mut [CpuSmmLayout],
) -> Result<InstalledSmmImage<'a>, InstallError> {
    let header = SmmImageHeader::parse(image)?;
    if header.entry_count < config.num_cpus || config.num_cpus as usize > cpu_layouts.len() {
        return Err(InstallError::NotEnoughEntries);
    }
    validate_memory_blocks(&header)?;
    if size_of::<T>() > header.handler_config_capacity as usize
        || align_of::<T>() > HANDLER_CONFIG_ALIGNMENT
    {
        return Err(InstallError::BadHandlerConfig);
    }

    // Validate every descriptor and its parameter block before the first SMRAM
    // write. A malformed later CPU entry must not leave a partial install.
    let mut max_stub_size = 0u32;
    for i in 0..config.num_cpus {
        let entry = header.entry(image, i)?;
        check_entry_range(&header, image, &entry)?;
        check_entry_params(&entry)?;
        max_stub_size = max_stub_size.max(entry.stub_size);
    }

    let smram_top = config
        .smram_base
        .checked_add(config.smram_size)
        .ok_or(InstallError::Overflow)?;
    if smram_top & 0xfff != 0 {
        return Err(InstallError::BadAlignment);
    }
    let layout = SmramLayout {
        smram_base: config.smram_base,
        smram_size: config.smram_size,
        entry_count: config.num_cpus,
        save_state_size: config.save_state_size,
        stack_size: header.stack_size,
        entry_stub_size: max_stub_size,
        handler_mem_size: header.handler_mem_size,
        page_table_size: SMM_IDENTITY_TABLE_SIZE,
    };
    let common_base = compute_common_base(&layout)?;
    let page_table_base = compute_page_table_base(&layout)?;
    if page_table_base & 0xfff != 0 {
        return Err(InstallError::BadAlignment);
    }
    let cpus = compute_cpu_layout(&layout, cpu_layouts)?;

    let runtime_addr = checked_add(common_base, header.runtime_offset)?;
    let config_addr = checked_add(common_base, header.handler_config_offset)?;
    let module_args_base = if header.module_args_offset != 0 {
        Some(checked_add(common_base, header.module_args_offset)?)
    } else {
        None
    };
    let common_entry = checked_add(common_base, header.handler_entry_offset)?;
    let config_relative = header
        .handler_config_offset
        .checked_sub(header.runtime_offset)
        .ok_or(InstallError::BadHandlerConfig)?;
    if !(config_relative as usize).is_multiple_of(HANDLER_CONFIG_ALIGNMENT) {
        return Err(InstallError::BadHandlerConfig);
    }
    let runtime = SmmRuntime::new(
        config.smram_base,
        config.smram_size,
        config.num_cpus,
        header.stack_size,
        config_relative,
        size_of::<T>() as u32,
    );

    // The entry stub carries these addresses through 32-bit registers and the
    // permanent tables map only the low 4 GiB. Validate the complete permanent
    // placement before writing either the handler or an entry stub.
    check_low_range(common_base, header.handler_mem_size as u64)?;
    check_low_range(page_table_base, SMM_IDENTITY_TABLE_SIZE as u64)?;
    for address in [common_entry, runtime_addr, config_addr, page_table_base] {
        check_low_address(address)?;
    }
    if let Some(address) = module_args_base {
        check_low_address(address)?;
    }
    for cpu in cpus.iter() {
        for address in [
            cpu.smbase,
            cpu.entry_addr,
            cpu.save_state_base,
            cpu.save_state_top,
            cpu.stack_bottom,
            cpu.stack_top,
        ] {
            check_low_address(address)?;
        }
        check_low_range(cpu.entry_addr, max_stub_size as u64)?;
        check_low_range(cpu.stack_bottom, header.stack_size as u64)?;
    }

    // Start from a deterministic image: initialized bytes are copied and every
    // alignment gap, BSS byte, and loader-owned block is zeroed first.
    unsafe {
        ptr::write_bytes(common_base as *mut u8, 0, header.handler_mem_size as usize);
        ptr::copy_nonoverlapping(
            image.as_ptr().add(header.handler_offset as usize),
            common_base as *mut u8,
            header.handler_load_size as usize,
        );
        ptr::write(runtime_addr as *mut SmmRuntime, runtime);
        // Alignment was checked against HANDLER_CONFIG_ALIGNMENT above.
        ptr::write(config_addr as *mut T, *config.handler_config);
    }

    // Permanent tables are built inside the same SMRAM allocation and remain
    // inaccessible to the OS after chipset lock.
    let cr3 = unsafe { build_identity_tables(page_table_base) };

    for (i, cpu) in cpus.iter().enumerate() {
        let entry = header.entry(image, i as u16)?;
        unsafe {
            ptr::copy_nonoverlapping(
                image.as_ptr().add(entry.stub_offset as usize),
                cpu.entry_addr as *mut u8,
                entry.stub_size as usize,
            );
        }

        let coreboot_module_args = if let Some(base) = module_args_base {
            let addr = base
                .checked_add((i * size_of::<CorebootModuleArgs>()) as u64)
                .ok_or(InstallError::Overflow)?;
            unsafe {
                ptr::write_unaligned(
                    addr as *mut CorebootModuleArgs,
                    CorebootModuleArgs {
                        cpu: i as u64,
                        canary: cpu.stack_bottom,
                    },
                );
                ptr::write_unaligned(cpu.stack_bottom as *mut u64, cpu.stack_bottom);
            }
            addr
        } else {
            0
        };

        patch_entry_params(
            cpu.entry_addr,
            &entry,
            SmmEntryParams {
                cpu: i as u32,
                stack_size: header.stack_size,
                stack_top: cpu.stack_top,
                common_entry,
                runtime: runtime_addr,
                coreboot_module_args,
                cr3,
                entry_base: cpu.entry_addr,
            },
        )?;
    }

    Ok(InstalledSmmImage {
        header,
        common_base,
        common_entry,
        runtime_addr,
        cr3,
        cpus,
    })
}

/// Install the temporary default-SMBASE trampoline.
///
/// # Safety
///
/// The default SMRAM window must be open and writable, and `callback` must be
/// executable under `cr3` with the entry-stub ABI.
pub unsafe fn install_default_relocation_callback_stub(
    image: &[u8],
    config: DefaultRelocationCallbackConfig,
) -> Result<(), InstallError> {
    let header = SmmImageHeader::parse(image)?;
    if header.entry_count == 0 {
        return Err(InstallError::NotEnoughEntries);
    }
    let stub = header.entry(image, 0)?;
    check_entry_range(&header, image, &stub)?;
    check_entry_params(&stub)?;
    let entry_addr = config
        .default_smbase
        .checked_add(SMM_ENTRY_OFFSET)
        .ok_or(InstallError::Overflow)?;
    let entry_end = entry_addr
        .checked_add(stub.stub_size as u64)
        .ok_or(InstallError::Overflow)?;
    let page_tables = config
        .default_smbase
        .checked_add(SMM_RELOCATION_TABLE_OFFSET)
        .ok_or(InstallError::Overflow)?;
    if entry_end > page_tables {
        return Err(InstallError::BadEntryRange);
    }
    if config.cr3 & 0xfff != 0 {
        return Err(InstallError::BadAlignment);
    }
    for address in [
        entry_addr,
        config.cr3,
        config.callback,
        config.callback_arg,
        config.stack_top,
    ] {
        check_low_address(address)?;
    }
    unsafe {
        ptr::copy_nonoverlapping(
            image.as_ptr().add(stub.stub_offset as usize),
            entry_addr as *mut u8,
            stub.stub_size as usize,
        );
    }
    patch_entry_params(
        entry_addr,
        &stub,
        SmmEntryParams {
            cpu: 0,
            stack_size: 0,
            stack_top: config.stack_top,
            common_entry: config.callback,
            runtime: config.callback_arg,
            coreboot_module_args: 0,
            cr3: config.cr3,
            entry_base: entry_addr,
        },
    )
}

fn validate_memory_blocks(header: &SmmImageHeader) -> Result<(), InstallError> {
    if !(header.runtime_offset as usize).is_multiple_of(align_of::<SmmRuntime>())
        || (header.runtime_size as usize) < size_of::<SmmRuntime>()
    {
        return Err(InstallError::BadRuntime);
    }
    if !(header.handler_config_offset as usize).is_multiple_of(HANDLER_CONFIG_ALIGNMENT)
        || header.handler_config_capacity as usize != HANDLER_CONFIG_CAPACITY
    {
        return Err(InstallError::BadHandlerConfig);
    }
    let runtime_end = header
        .runtime_offset
        .checked_add(header.runtime_size)
        .ok_or(InstallError::Overflow)?;
    let config_end = header
        .handler_config_offset
        .checked_add(header.handler_config_capacity)
        .ok_or(InstallError::Overflow)?;
    if runtime_end > header.handler_config_offset {
        return Err(InstallError::BadHandlerConfig);
    }

    let module_args_flag = header.flags & FLAG_COREBOOT_MODULE_ARGS != 0;
    match (header.module_args_offset, header.module_args_size) {
        (0, 0) if !module_args_flag => {}
        (0, _) | (_, 0) => return Err(InstallError::BadModuleArgs),
        (offset, size) => {
            if !module_args_flag {
                return Err(InstallError::BadModuleArgs);
            }
            let needed = size_of::<CorebootModuleArgs>()
                .checked_mul(header.entry_count as usize)
                .ok_or(InstallError::Overflow)?;
            if !(offset as usize).is_multiple_of(align_of::<CorebootModuleArgs>())
                || (size as usize) < needed
                || config_end > offset
            {
                return Err(InstallError::BadModuleArgs);
            }
        }
    }
    Ok(())
}

fn checked_add(base: u64, offset: u32) -> Result<u64, InstallError> {
    base.checked_add(offset as u64)
        .ok_or(InstallError::Overflow)
}

const LOW_4G_END: u64 = 1u64 << 32;

fn check_low_address(address: u64) -> Result<(), InstallError> {
    if address >= LOW_4G_END {
        Err(InstallError::AddressAbove4G)
    } else {
        Ok(())
    }
}

fn check_low_range(base: u64, size: u64) -> Result<(), InstallError> {
    let end = base.checked_add(size).ok_or(InstallError::Overflow)?;
    if base >= LOW_4G_END || end > LOW_4G_END {
        Err(InstallError::AddressAbove4G)
    } else {
        Ok(())
    }
}

fn check_entry_params(entry: &EntryDescriptor) -> Result<(), InstallError> {
    let params_end = entry
        .params_offset
        .checked_add(size_of::<SmmEntryParams>() as u32)
        .ok_or(InstallError::Overflow)?;
    if entry.params_offset == 0
        || !(entry.params_offset as usize).is_multiple_of(align_of::<SmmEntryParams>())
        || params_end > entry.stub_size
    {
        return Err(InstallError::BadParams);
    }
    Ok(())
}

fn patch_entry_params(
    entry_addr: u64,
    entry: &EntryDescriptor,
    params: SmmEntryParams,
) -> Result<(), InstallError> {
    check_entry_params(entry)?;
    let params_addr = entry_addr
        .checked_add(entry.params_offset as u64)
        .ok_or(InstallError::Overflow)?;
    unsafe { ptr::write_unaligned(params_addr as *mut SmmEntryParams, params) };
    Ok(())
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
    use super::*;
    use crate::header::{EntryDescriptor, FLAG_COREBOOT_MODULE_ARGS};
    use std::vec;
    use std::vec::Vec;
    use zerocopy::IntoBytes;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestConfig {
        pm_base: u16,
        gpe0: u16,
    }
    const TEST_CONFIG: TestConfig = TestConfig {
        pm_base: 0x600,
        gpe0: 0x20,
    };

    struct TestSmram {
        mapping: *mut core::ffi::c_void,
        mapping_size: usize,
        base: u64,
        size: usize,
    }

    impl TestSmram {
        fn new(size: usize, fill: u8) -> Self {
            const PROT_READ: i32 = 1;
            const PROT_WRITE: i32 = 2;
            const MAP_PRIVATE: i32 = 2;
            const MAP_ANONYMOUS: i32 = 0x20;
            const MAP_32BIT: i32 = 0x40;
            unsafe extern "C" {
                fn mmap(
                    address: *mut core::ffi::c_void,
                    length: usize,
                    protection: i32,
                    flags: i32,
                    fd: i32,
                    offset: isize,
                ) -> *mut core::ffi::c_void;
            }
            let mapping_size = size + 0x1000;
            let mapping = unsafe {
                mmap(
                    core::ptr::null_mut(),
                    mapping_size,
                    PROT_READ | PROT_WRITE,
                    MAP_PRIVATE | MAP_ANONYMOUS | MAP_32BIT,
                    -1,
                    0,
                )
            };
            assert_ne!(mapping as usize, usize::MAX);
            let base = ((mapping as usize + 0xfff) & !0xfff) as u64;
            assert!(base + size as u64 <= mapping as u64 + mapping_size as u64);
            unsafe { core::ptr::write_bytes(base as *mut u8, fill, size) };
            Self {
                mapping,
                mapping_size,
                base,
                size,
            }
        }

        fn bytes(&self) -> &[u8] {
            unsafe { core::slice::from_raw_parts(self.base as *const u8, self.size) }
        }
    }

    impl Drop for TestSmram {
        fn drop(&mut self) {
            unsafe extern "C" {
                fn munmap(address: *mut core::ffi::c_void, length: usize) -> i32;
            }
            assert_eq!(unsafe { munmap(self.mapping, self.mapping_size) }, 0);
        }
    }

    fn align(value: u32, alignment: usize) -> u32 {
        (value + alignment as u32 - 1) & !(alignment as u32 - 1)
    }

    fn test_image_with_entries(entry_count: u16) -> (Vec<u8>, SmmImageHeader) {
        let header_size = size_of::<SmmImageHeader>() as u32;
        let entries_offset = header_size;
        let handler_offset = 0x80;
        let handler_load_size = 0x20;
        let runtime_offset = 0x40;
        let runtime_size = size_of::<SmmRuntime>() as u32;
        let config_offset = align(runtime_offset + runtime_size, HANDLER_CONFIG_ALIGNMENT);
        let module_offset = align(
            config_offset + HANDLER_CONFIG_CAPACITY as u32,
            align_of::<CorebootModuleArgs>(),
        );
        let module_args_size = size_of::<CorebootModuleArgs>() as u32 * u32::from(entry_count);
        let handler_mem_size = module_offset + module_args_size;
        let stub_offset = handler_offset + handler_load_size;
        let stub_size = 0x80;
        let image_size = stub_offset + stub_size * u32::from(entry_count);
        let header = SmmImageHeader::new(
            FLAG_COREBOOT_MODULE_ARGS,
            image_size,
            entry_count,
            entries_offset,
            handler_offset,
            handler_load_size,
            handler_mem_size,
            0,
            runtime_offset,
            runtime_size,
            config_offset,
            HANDLER_CONFIG_CAPACITY as u32,
            module_offset,
            module_args_size,
            0x400,
        );
        let mut image = vec![0u8; image_size as usize];
        header.write_to_prefix(&mut image).unwrap();
        for i in 0..entry_count {
            let desc = EntryDescriptor {
                stub_offset: stub_offset + u32::from(i) * stub_size,
                stub_size,
                entry_offset: 0,
                params_offset: 0x20,
            };
            let desc_offset =
                entries_offset as usize + usize::from(i) * size_of::<EntryDescriptor>();
            desc.write_to_prefix(&mut image[desc_offset..]).unwrap();
            image[desc.stub_offset as usize] = 0xbb;
        }
        image[handler_offset as usize] = 0xaa;
        (image, header)
    }

    fn test_image() -> (Vec<u8>, SmmImageHeader) {
        test_image_with_entries(1)
    }

    fn install_for_test(
        image: &[u8],
        smram_base: u64,
        smram_size: u64,
    ) -> Result<(), InstallError> {
        let header = SmmImageHeader::parse(image)?;
        let zero = CpuSmmLayout {
            smbase: 0,
            entry_addr: 0,
            save_state_base: 0,
            save_state_top: 0,
            stack_bottom: 0,
            stack_top: 0,
        };
        let mut cpus = vec![zero; header.entry_count as usize];
        unsafe {
            install_pic_image(
                image,
                InstallConfig {
                    smram_base,
                    smram_size,
                    num_cpus: header.entry_count,
                    save_state_size: 0x400,
                    handler_config: &TEST_CONFIG,
                },
                &mut cpus,
            )
            .map(|_| ())
        }
    }

    #[test]
    fn installs_initialized_data_zeros_bss_and_uses_smram_cr3() {
        let (image, header) = test_image();
        let smram_size = 0x8_0000usize;
        let smram = TestSmram::new(smram_size, 0x5a);
        let smram_base = smram.base;
        let mut cpus = [CpuSmmLayout {
            smbase: 0,
            entry_addr: 0,
            save_state_base: 0,
            save_state_top: 0,
            stack_bottom: 0,
            stack_top: 0,
        }];
        let installed = unsafe {
            install_pic_image(
                &image,
                InstallConfig {
                    smram_base,
                    smram_size: smram_size as u64,
                    num_cpus: 1,
                    save_state_size: 0x400,
                    handler_config: &TEST_CONFIG,
                },
                &mut cpus,
            )
        }
        .unwrap();
        assert_eq!(unsafe { *(installed.common_base as *const u8) }, 0xaa);
        assert_eq!(unsafe { *((installed.common_base + 0x30) as *const u8) }, 0);
        assert!(installed.cr3 >= smram_base);
        assert!(installed.cr3 < smram_base + smram_size as u64);
        assert_eq!(installed.cr3 % 4096, 0);
        let params = unsafe {
            ptr::read_unaligned((installed.cpus[0].entry_addr + 0x20) as *const SmmEntryParams)
        };
        assert_eq!(params.cr3, installed.cr3);
        assert_eq!(params.runtime, installed.runtime_addr);
        let runtime = unsafe { ptr::read(installed.runtime_addr as *const SmmRuntime) };
        assert_eq!(runtime.num_cpus, 1);
        assert_eq!(runtime.handler_config_size, size_of::<TestConfig>() as u32);
        let config = unsafe {
            ptr::read(
                (installed.common_base + u64::from(header.handler_config_offset))
                    as *const TestConfig,
            )
        };
        assert_eq!(config, TEST_CONFIG);
    }

    fn assert_early_failure(image: &[u8], expected: InstallError) {
        let smram_size = 0x8_0000usize;
        let storage = TestSmram::new(smram_size, 0x5a);
        let base = storage.base;
        assert_eq!(
            install_for_test(image, base, smram_size as u64),
            Err(expected)
        );
        assert!(storage.bytes().iter().all(|&byte| byte == 0x5a));
    }

    #[test]
    fn rejects_overlapping_misaligned_and_bad_late_entry_before_writes() {
        let (image, header) = test_image();

        let mut overlap = image.clone();
        SmmImageHeader {
            handler_config_offset: header.runtime_offset + 8,
            ..header
        }
        .write_to_prefix(&mut overlap)
        .unwrap();
        assert_early_failure(&overlap, InstallError::BadHandlerConfig);

        let mut wrong_capacity = image.clone();
        SmmImageHeader {
            handler_config_capacity: header.handler_config_capacity
                + HANDLER_CONFIG_ALIGNMENT as u32,
            ..header
        }
        .write_to_prefix(&mut wrong_capacity)
        .unwrap();
        assert_early_failure(&wrong_capacity, InstallError::BadHandlerConfig);

        let mut misaligned = image.clone();
        SmmImageHeader {
            runtime_offset: header.runtime_offset + 1,
            ..header
        }
        .write_to_prefix(&mut misaligned)
        .unwrap();
        assert_early_failure(&misaligned, InstallError::BadRuntime);

        let (mut bad_params, two_entry_header) = test_image_with_entries(2);
        let second_descriptor =
            two_entry_header.entries_offset as usize + size_of::<EntryDescriptor>();
        let mut descriptor = two_entry_header.entry(&bad_params, 1).unwrap();
        descriptor.params_offset = 0x21;
        descriptor
            .write_to_prefix(&mut bad_params[second_descriptor..])
            .unwrap();
        assert_early_failure(&bad_params, InstallError::BadParams);
    }

    #[test]
    fn rejects_config_misaligned_relative_to_runtime() {
        let (mut image, header) = test_image();
        // Both absolute addresses are aligned, but the runtime-relative
        // offset is not. The handler must never silently skip this config.
        SmmImageHeader {
            runtime_offset: header.runtime_offset + 8,
            ..header
        }
        .write_to_prefix(&mut image)
        .unwrap();
        assert_early_failure(&image, InstallError::BadHandlerConfig);
    }

    #[test]
    fn rejects_unaligned_smram_top_and_addresses_above_four_gib() {
        let (image, _) = test_image();
        assert_eq!(
            install_for_test(&image, 0x10_0000, 0x8_0001),
            Err(InstallError::BadAlignment)
        );
        assert_eq!(
            install_for_test(&image, 0x1_0000_0000, 0x8_0000),
            Err(InstallError::AddressAbove4G)
        );
    }
}
