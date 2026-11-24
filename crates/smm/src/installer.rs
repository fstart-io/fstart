//! Native SMM image installation helpers.

use core::mem::{align_of, size_of};
use core::ptr;

use crate::header::{
    EntryDescriptor, FLAG_COREBOOT_MODULE_ARGS, FLAG_PROTECTED_MODE, HeaderError, SmmImageHeader,
};
use crate::layout::{
    CpuSmmLayout, LayoutError, SMM_ENTRY_OFFSET, SMM_IDENTITY_TABLE_SIZE,
    SMM_RELOCATION_TABLE_OFFSET, SmramLayout, compute_common_base, compute_cpu_layout,
    compute_page_table_base,
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
    /// Configuration of the board's SMI handler: the type its handler
    /// function takes (see [`smm_bin!`](crate::smm_bin)).
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
    /// The image's entry mode does not match this build (i686 vs x86_64).
    ModeMismatch,
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
    if config.smram_size == 0 {
        return Err(InstallError::Layout(LayoutError::BadSize));
    }
    if config.smram_base == 0 {
        return Err(InstallError::Overflow);
    }
    let smram_len = usize::try_from(config.smram_size).map_err(|_| InstallError::Overflow)?;
    if smram_len > isize::MAX as usize {
        return Err(InstallError::Overflow);
    }
    // Validate address range before constructing the mutable SMRAM slice so
    // an out-of-range base or overflowing size never produces an invalid slice.
    check_low_range(config.smram_base, config.smram_size)?;
    // Also ensure the exclusive end fits in `usize` so pointer arithmetic in
    // `from_raw_parts_mut` cannot wrap on 32-bit targets (e.g. end == 4 GiB).
    let smram_base_ptr = usize::try_from(config.smram_base).map_err(|_| InstallError::Overflow)?;
    smram_base_ptr
        .checked_add(smram_len)
        .ok_or(InstallError::Overflow)?;
    // SAFETY: caller guarantees `smram_base..smram_base + smram_size` is
    // writable and exclusively owned, `smram_base != 0`, `smram_len <= isize::MAX`,
    // `smram_base + smram_len` does not wrap `usize`, and `MaybeUninit<u8>`
    // requires no prior initialization of SMRAM bytes.
    let smram = unsafe {
        core::slice::from_raw_parts_mut(
            smram_base_ptr as *mut core::mem::MaybeUninit<u8>,
            smram_len,
        )
    };
    install_pic_image_in_slice(image, config, smram, cpu_layouts)
}

/// Pure slice-backed core of [`install_pic_image`]: performs every placement
/// check against `config.smram_base` (the architectural `< 4 GiB` physical base)
/// and writes all initialized bytes into `smram` instead of raw integer
/// addresses, allowing full Miri verification without platform `mmap` calls.
fn install_pic_image_in_slice<'a, T: Copy>(
    image: &[u8],
    config: InstallConfig<'_, T>,
    smram: &mut [core::mem::MaybeUninit<u8>],
    cpu_layouts: &'a mut [CpuSmmLayout],
) -> Result<InstalledSmmImage<'a>, InstallError> {
    if smram.len() as u64 != config.smram_size {
        return Err(InstallError::Overflow);
    }
    let header = SmmImageHeader::parse(image)?;
    check_mode(&header)?;
    if header.entry_count < config.num_cpus || config.num_cpus as usize > cpu_layouts.len() {
        return Err(InstallError::NotEnoughEntries);
    }
    validate_memory_blocks(&header)?;
    for i in 0..header.fixup_count {
        header.fixup(image, i)?;
    }
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

    let smram_len = smram.len();
    let smram_off = |addr: u64, len: usize| -> Result<usize, InstallError> {
        let off = addr
            .checked_sub(config.smram_base)
            .ok_or(InstallError::Overflow)?;
        let off = usize::try_from(off).map_err(|_| InstallError::Overflow)?;
        let end = off.checked_add(len).ok_or(InstallError::Overflow)?;
        if end > smram_len {
            return Err(InstallError::Overflow);
        }
        Ok(off)
    };

    // Start from a deterministic image: initialized bytes are copied and every
    // alignment gap, BSS byte, and loader-owned block is zeroed first.
    let handler_mem_len = header.handler_mem_size as usize;
    let common_off = smram_off(common_base, handler_mem_len)?;
    let common_uninit = &mut smram[common_off..common_off + handler_mem_len];
    common_uninit.fill(core::mem::MaybeUninit::new(0));
    // SAFETY: every byte in `common_uninit` was just initialized to 0 above.
    let common_bytes = unsafe {
        core::slice::from_raw_parts_mut(common_uninit.as_mut_ptr().cast::<u8>(), handler_mem_len)
    };
    let load_src = header.handler_offset as usize;
    let load_len = header.handler_load_size as usize;
    common_bytes[..load_len].copy_from_slice(&image[load_src..load_src + load_len]);

    // A protected-mode handler is linked at 0: rebase its absolute addresses.
    // The offsets were validated above.
    for i in 0..header.fixup_count {
        let at = header.fixup(image, i)? as usize;
        let end = at.checked_add(4).ok_or(InstallError::Overflow)?;
        let dst = common_bytes
            .get_mut(at..end)
            .ok_or(InstallError::Overflow)?;
        let current = u32::from_le_bytes([dst[0], dst[1], dst[2], dst[3]]);
        dst.copy_from_slice(&current.wrapping_add(common_base as u32).to_le_bytes());
    }

    let runtime_off = smram_off(runtime_addr, size_of::<SmmRuntime>())?;
    // SAFETY: `runtime_off..runtime_off + size_of::<SmmRuntime>()` is in-bounds
    // and exclusively borrowed from `smram`.
    unsafe {
        ptr::write_unaligned(
            smram.as_mut_ptr().add(runtime_off).cast::<SmmRuntime>(),
            runtime,
        );
    }

    let config_off = smram_off(config_addr, size_of::<T>())?;
    // SAFETY: `config_off..config_off + size_of::<T>()` is in-bounds and
    // exclusively borrowed from `smram`.
    unsafe {
        ptr::write_unaligned(
            smram.as_mut_ptr().add(config_off).cast::<T>(),
            *config.handler_config,
        );
    }

    // Permanent tables are built inside the same SMRAM allocation and remain
    // inaccessible to the OS after chipset lock.
    let pt_off = smram_off(page_table_base, SMM_IDENTITY_TABLE_SIZE as usize)?;
    let cr3 = crate::layout::build_identity_tables_in_slice(
        &mut smram[pt_off..pt_off + SMM_IDENTITY_TABLE_SIZE as usize],
        page_table_base,
    );

    for (i, cpu) in cpus.iter().enumerate() {
        let entry = header.entry(image, i as u16)?;
        let entry_off = smram_off(cpu.entry_addr, entry.stub_size as usize)?;
        let stub_src = entry.stub_offset as usize;
        let stub_len = entry.stub_size as usize;
        for (dst, &src) in smram[entry_off..entry_off + stub_len]
            .iter_mut()
            .zip(&image[stub_src..stub_src + stub_len])
        {
            dst.write(src);
        }

        let coreboot_module_args = if let Some(base) = module_args_base {
            let addr = base
                .checked_add((i * size_of::<CorebootModuleArgs>()) as u64)
                .ok_or(InstallError::Overflow)?;
            let mod_off = smram_off(addr, size_of::<CorebootModuleArgs>())?;
            let stack_off = smram_off(cpu.stack_bottom, size_of::<u64>())?;
            // SAFETY: both offsets were bounds-checked against `smram`.
            unsafe {
                ptr::write_unaligned(
                    smram.as_mut_ptr().add(mod_off).cast::<CorebootModuleArgs>(),
                    CorebootModuleArgs {
                        cpu: i as u64,
                        canary: cpu.stack_bottom,
                    },
                );
            }
            for (dst, &src) in smram[stack_off..stack_off + 8]
                .iter_mut()
                .zip(&cpu.stack_bottom.to_le_bytes())
            {
                dst.write(src);
            }
            addr
        } else {
            0
        };

        let params_addr = cpu
            .entry_addr
            .checked_add(u64::from(entry.params_offset))
            .ok_or(InstallError::Overflow)?;
        let params_off = smram_off(params_addr, size_of::<SmmEntryParams>())?;
        // SAFETY: `params_off..params_off + size_of::<SmmEntryParams>()` is in-bounds.
        unsafe {
            ptr::write_unaligned(
                smram.as_mut_ptr().add(params_off).cast::<SmmEntryParams>(),
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
            );
        }
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
    check_mode(&header)?;
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

/// The stubs call the handler and the relocation callback in their own mode,
/// so the image must match the installing stage.
fn check_mode(header: &SmmImageHeader) -> Result<(), InstallError> {
    let protected = header.flags & FLAG_PROTECTED_MODE != 0;
    if protected == cfg!(target_arch = "x86") {
        Ok(())
    } else {
        Err(InstallError::ModeMismatch)
    }
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

    /// Architectural physical base used by slice-backed SMRAM tests.
    const TEST_SMRAM_BASE: u64 = 0x7f00_0000;

    struct TestSmram {
        base: u64,
        data: Vec<u8>,
    }

    impl TestSmram {
        fn new(size: usize, fill: u8) -> Self {
            Self {
                base: TEST_SMRAM_BASE,
                data: vec![fill; size],
            }
        }

        fn bytes(&self) -> &[u8] {
            &self.data
        }

        fn as_uninit_mut(&mut self) -> &mut [core::mem::MaybeUninit<u8>] {
            // SAFETY: `MaybeUninit<u8>` has the same layout as `u8`, and
            // `install_pic_image_in_slice` only writes initialized bytes.
            unsafe {
                core::slice::from_raw_parts_mut(
                    self.data.as_mut_ptr().cast::<core::mem::MaybeUninit<u8>>(),
                    self.data.len(),
                )
            }
        }

        fn read_u8(&self, phys_addr: u64) -> u8 {
            let off = usize::try_from(phys_addr.checked_sub(self.base).unwrap()).unwrap();
            self.data[off]
        }

        /// # Safety
        ///
        /// The bytes at `phys_addr..phys_addr + size_of::<T>()` must represent
        /// a valid bit pattern for `T`.
        unsafe fn read_unaligned<T: Copy>(&self, phys_addr: u64) -> T {
            let off = usize::try_from(phys_addr.checked_sub(self.base).unwrap()).unwrap();
            assert!(off + size_of::<T>() <= self.data.len());
            // SAFETY: caller guarantees the bytes are a valid `T`, and `off..off + size_of::<T>()`
            // is within `self.data`.
            unsafe { ptr::read_unaligned(self.data.as_ptr().add(off).cast::<T>()) }
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

    fn install_for_test_in_slice(
        image: &[u8],
        smram_base: u64,
        smram: &mut [core::mem::MaybeUninit<u8>],
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
        install_pic_image_in_slice(
            image,
            InstallConfig {
                smram_base,
                smram_size: smram.len() as u64,
                num_cpus: header.entry_count,
                save_state_size: 0x400,
                handler_config: &TEST_CONFIG,
            },
            smram,
            &mut cpus,
        )
        .map(|_| ())
    }

    fn install_for_test(
        image: &[u8],
        smram_base: u64,
        smram_size: u64,
    ) -> Result<(), InstallError> {
        let mut storage = vec![core::mem::MaybeUninit::new(0x5au8); smram_size as usize];
        install_for_test_in_slice(image, smram_base, &mut storage)
    }

    #[test]
    fn installs_initialized_data_zeros_bss_and_uses_smram_cr3() {
        let (image, header) = test_image();
        let smram_size = 0x8_0000usize;
        let mut smram = TestSmram::new(smram_size, 0x5a);
        let smram_base = smram.base;
        let mut cpus = [CpuSmmLayout {
            smbase: 0,
            entry_addr: 0,
            save_state_base: 0,
            save_state_top: 0,
            stack_bottom: 0,
            stack_top: 0,
        }];
        let installed = install_pic_image_in_slice(
            &image,
            InstallConfig {
                smram_base,
                smram_size: smram_size as u64,
                num_cpus: 1,
                save_state_size: 0x400,
                handler_config: &TEST_CONFIG,
            },
            smram.as_uninit_mut(),
            &mut cpus,
        )
        .unwrap();
        assert_eq!(smram.read_u8(installed.common_base), 0xaa);
        assert_eq!(smram.read_u8(installed.common_base + 0x30), 0);
        assert!(installed.cr3 >= smram_base);
        assert!(installed.cr3 < smram_base + smram_size as u64);
        assert_eq!(installed.cr3 % 4096, 0);
        // SAFETY: `SmmEntryParams` was just written at `entry_addr + 0x20`.
        let params: SmmEntryParams =
            unsafe { smram.read_unaligned(installed.cpus[0].entry_addr + 0x20) };
        assert_eq!(params.cr3, installed.cr3);
        assert_eq!(params.runtime, installed.runtime_addr);
        let runtime_off = usize::try_from(installed.runtime_addr - smram_base).unwrap();
        // SAFETY: `SmmRuntime` is `#[repr(C)]` and initialized at `runtime_off`.
        let runtime = unsafe {
            ptr::read_unaligned(smram.data.as_ptr().add(runtime_off).cast::<SmmRuntime>())
        };
        assert_eq!(runtime.num_cpus, 1);
        assert_eq!(runtime.handler_config_size, size_of::<TestConfig>() as u32);
        // SAFETY: `TestConfig` was just written at `common_base + handler_config_offset`.
        let config: TestConfig = unsafe {
            smram.read_unaligned(installed.common_base + u64::from(header.handler_config_offset))
        };
        assert_eq!(config, TEST_CONFIG);
    }

    fn assert_early_failure(image: &[u8], expected: InstallError) {
        let smram_size = 0x2_0000usize;
        let mut storage = TestSmram::new(smram_size, 0x5a);
        let base = storage.base;
        assert_eq!(
            install_for_test_in_slice(image, base, storage.as_uninit_mut()),
            Err(expected)
        );
        let fill = [0x5au8; 8];
        let (chunks, remainder) = storage.bytes().as_chunks::<8>();
        assert!(remainder.is_empty());
        assert!(chunks.iter().all(|chunk| *chunk == fill));
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
