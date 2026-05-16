//! Cache-as-RAM teardown and post-CAR stage loading for x86 platforms.
//!
//! Mirrors coreboot's Intel non-evict postcar flow:
//! 1. switch to a DRAM stack
//! 2. disable cache, disable MTRRs, clear NEM RUN/SETUP
//! 3. program post-CAR MTRRs while cache/MTRRs are disabled
//! 4. re-enable MTRRs, re-enable cache, `invd`
//! 5. load ramstage from FFS while DRAM and ROM are cacheable

use core::arch::{asm, global_asm};

use fstart_arch_x86::{msr, mtrr};

global_asm!(
    ".text",
    ".code64",
    ".global _car_teardown",
    // ------------------------------------------------------------------
    // _car_teardown — Intel non-evict CAR teardown.
    //
    // Keep this deliberately close to coreboot
    // cpu/intel/car/non-evict/exit_car.S. Do not WBINVD dirty CAR lines:
    // NEM CAR has no backing memory and coreboot uses INVD later.
    // ------------------------------------------------------------------
    "_car_teardown:",
    // Disable cache: CR0.CD=1. Leave NW unchanged here, like coreboot.
    "movq %cr0, %rax",
    "orq $0x40000000, %rax",
    "movq %rax, %cr0",
    // Disable MTRRs.
    "movl $0x2ff, %ecx",
    "rdmsr",
    "andl $0xfffff7ff, %eax",
    "wrmsr",
    // Disable no-evict mode RUN then SETUP only on Atom/NEM models.
    "movl $1, %eax",
    "cpuid",
    "movl %eax, %edx",
    "shrl $4, %edx",
    "andl $0x0f, %edx",
    "movl %eax, %ebx",
    "shrl $12, %ebx",
    "andl $0xf0, %ebx",
    "orl %ebx, %edx",
    "cmpl $0x1c, %edx",
    "je 1f",
    "cmpl $0x26, %edx",
    "je 1f",
    "cmpl $0x27, %edx",
    "je 1f",
    "cmpl $0x35, %edx",
    "je 1f",
    "cmpl $0x36, %edx",
    "jne 2f",
    "1:",
    "movl $0x2e0, %ecx",
    "rdmsr",
    "andl $0xfffffffd, %eax",
    "wrmsr",
    "andl $0xfffffffe, %eax",
    "wrmsr",
    "2:",
    "ret",
    options(att_syntax),
);

unsafe extern "C" {
    fn _car_teardown();
}

/// One physical range from the board memory map.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct PhysicalRange {
    /// Physical base address.
    pub base: u64,
    /// Range size in bytes.
    pub size: u64,
}

/// Data block, stored in ROM by generated code, that describes post-CAR work.
#[repr(C)]
pub struct PostcarConfig {
    /// Static RAM ranges from board configuration.
    pub ram_ranges: &'static [PhysicalRange],
    /// Memory-mapped boot media range, if any.
    pub rom_range: Option<PhysicalRange>,
}

impl PostcarConfig {
    /// Choose a temporary post-CAR stack in DRAM.
    pub fn stack_top(&self) -> Option<usize> {
        let ram = self.ram_ranges.first()?;
        let end = ram.base.checked_add(ram.size)?;
        let preferred = ram.base.saturating_add(0x0300_0000);
        let top = if preferred > ram.base && preferred <= end {
            preferred
        } else {
            end
        };
        Some((top & !0xf) as usize)
    }

    fn low_dram_mtrr_size(&self) -> Option<u64> {
        Some(
            self.ram_ranges
                .iter()
                .map(|r| r.base.saturating_add(r.size))
                .max()?
                .next_power_of_two()
                .max(0x0010_0000),
        )
    }
}

/// Tear down Cache-as-RAM non-evict mode.
///
/// # Safety
///
/// Caller must already be executing on a DRAM stack and must not return to
/// CAR-backed data after this call.
pub unsafe fn car_teardown() {
    unsafe { _car_teardown() }
}

unsafe fn invalidate_cache_after_reenable() {
    unsafe {
        asm!("invd", options(nostack, preserves_flags));
    }
}

/// Re-enable normal caching and install post-CAR MTRRs.
///
/// # Safety
///
/// Must be called after [`car_teardown`] while still on a DRAM stack and before
/// any large DRAM or memory-mapped flash copies. This function runs the MTRR
/// update with cache and MTRRs still disabled by [`car_teardown`].
pub unsafe fn postcar_mtrr_setup(config: &PostcarConfig) {
    unsafe {
        let count = mtrr::variable_count();
        for index in 0..count {
            mtrr::clear_variable(index);
        }

        if let (true, Some(size)) = (count > 0, config.low_dram_mtrr_size()) {
            mtrr::set_variable(0, 0, size, mtrr::MTRR_TYPE_WRITE_BACK);
        }

        if let Some(rom) = config.rom_range {
            if rom.size != 0 && count > 1 {
                mtrr::set_variable(1, rom.base, rom.size, mtrr::MTRR_TYPE_WRITE_PROTECT);
            }
        }

        let def_type = msr::rdmsr(mtrr::IA32_MTRR_DEF_TYPE);
        msr::wrmsr(mtrr::IA32_MTRR_DEF_TYPE, (def_type & 0xff) | (1 << 11));
        mtrr::enable_cache();
        invalidate_cache_after_reenable();
    }
}

/// Switch to a DRAM stack, tear down CAR, then load a stage from a
/// memory-mapped FFS image.
///
/// # Safety
///
/// `config` must describe trained DRAM and memory-mapped boot media. `anchor`
/// and `next_stage` must remain readable after the stack switch; generated code
/// stores both in ROM. This function never returns.
#[cfg(feature = "postcar-stage-load")]
pub unsafe fn stage_load_mmio(
    config: &'static PostcarConfig,
    next_stage: &str,
    anchor: &'static [u8],
    base: u64,
    size: u64,
) -> ! {
    let Some(stack_top) = config.stack_top() else {
        crate::halt();
    };

    unsafe {
        asm!(
            "mov rsp, {stack}",
            "and rsp, -16",
            "sub rsp, 8",
            "mov qword ptr [rsp + 8], {image_size}",
            "jmp {tramp}",
            stack = in(reg) stack_top,
            image_size = in(reg) size,
            tramp = sym stage_load_mmio_trampoline,
            in("rdi") config as *const PostcarConfig,
            in("rsi") next_stage.as_ptr(),
            in("rdx") next_stage.len(),
            in("rcx") anchor.as_ptr(),
            in("r8") anchor.len(),
            in("r9") base,
            options(noreturn),
        );
    }
}

#[cfg(feature = "postcar-stage-load")]
extern "C" fn stage_load_mmio_trampoline(
    config: *const PostcarConfig,
    next_ptr: *const u8,
    next_len: usize,
    anchor_ptr: *const u8,
    anchor_len: usize,
    base: u64,
    size: u64,
) -> ! {
    // SAFETY: generated code passes pointers derived from ROM-resident strings
    // and anchor bytes with their original lengths.
    let next_stage =
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(next_ptr, next_len)) };
    // SAFETY: generated code passes the embedded anchor slice pointer/length.
    let anchor = unsafe { core::slice::from_raw_parts(anchor_ptr, anchor_len) };
    // SAFETY: `config` points at the ROM-resident generated config block.
    let config = unsafe { &*config };

    // SAFETY: we are now on a DRAM stack and will never return to CAR-backed
    // state. MTRRs are installed before the large FFS/ramstage copy.
    unsafe {
        car_teardown();
        postcar_mtrr_setup(config);
    }

    let entry = quiet_stage_load(next_stage, anchor, base, size);
    crate::jump_to(entry)
}

#[cfg(feature = "postcar-stage-load")]
fn quiet_stage_load(next_stage: &str, anchor_data: &[u8], base: u64, size: u64) -> u64 {
    use fstart_types::ffs::{Compression, EntryContent, SegmentKind};

    // SAFETY: generated code passes the memory-mapped FFS base/size from the
    // board's BootMedia capability.
    let image = unsafe { core::slice::from_raw_parts(base as *const u8, size as usize) };
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => a,
        Err(_) => loop {},
    };
    let image_size = (anchor.total_image_size as usize).min(size as usize);
    let reader = fstart_ffs::FfsReader::new(&image[..image_size]);
    let manifest = match reader.read_manifest(&anchor) {
        Ok(m) => m,
        Err(_) => loop {},
    };

    for region in &manifest.regions {
        let Ok(entry) = fstart_ffs::FfsReader::find_entry(region, next_stage) else {
            continue;
        };
        let EntryContent::File { segments, .. } = &entry.content else {
            continue;
        };

        let mut entry_addr = 0;
        for seg in segments {
            if entry_addr == 0 && seg.kind == SegmentKind::Code {
                entry_addr = seg.load_addr;
            }

            if seg.kind == SegmentKind::Bss {
                // SAFETY: the manifest segment declares a RAM destination that
                // the next stage owns.
                unsafe {
                    core::ptr::write_bytes(seg.load_addr as *mut u8, 0, seg.loaded_size as usize)
                };
                continue;
            }

            let src_off = (region.offset + entry.offset + seg.offset) as usize;
            let stored = seg.stored_size as usize;
            if src_off.saturating_add(stored) > image_size {
                loop {}
            }

            match seg.compression {
                Compression::None => unsafe {
                    core::ptr::copy(
                        image.as_ptr().add(src_off),
                        seg.load_addr as *mut u8,
                        stored,
                    );
                },
                Compression::Lz4 => {
                    let buf_size = seg.in_place_size as usize;
                    let loaded = seg.loaded_size as usize;
                    if buf_size < loaded || buf_size < stored {
                        loop {}
                    }

                    let dest = seg.load_addr as *mut u8;
                    let comp_offset = buf_size - stored;
                    unsafe {
                        let buf = core::slice::from_raw_parts_mut(dest, buf_size);
                        core::ptr::copy(
                            image.as_ptr().add(src_off),
                            buf.as_mut_ptr().add(comp_offset),
                            stored,
                        );
                        let src =
                            core::slice::from_raw_parts(buf.as_ptr().add(comp_offset), stored);
                        let dst = core::slice::from_raw_parts_mut(buf.as_mut_ptr(), loaded);
                        match fstart_ffs::lz4::decompress_block(src, dst) {
                            Ok(_) => {}
                            Err(_) => loop {},
                        }
                    }
                }
            }
        }
        if entry_addr != 0 {
            return entry_addr;
        }
    }

    loop {}
}
