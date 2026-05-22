//! Cache-as-RAM teardown and post-CAR stage loading for x86 platforms.
//!
//! Mirrors coreboot's Intel post-CAR flow:
//! 1. switch to a DRAM stack
//! 2. disable cache and MTRRs
//! 3. clear NEM RUN/SETUP only on Atom models that use the no-evict MSR
//! 4. program post-CAR MTRRs while cache/MTRRs are disabled
//! 5. re-enable MTRRs, re-enable cache, `invd`
//! 6. load ramstage from FFS while DRAM and ROM are cacheable

use core::arch::{asm, global_asm};

use fstart_arch_x86::{mtrr, x86::msr};

global_asm!(
    ".text",
    ".code64",
    ".global _car_teardown",
    // ------------------------------------------------------------------
    // _car_teardown — Intel CAR teardown.
    //
    // Core2/X61 uses MTRR-backed CAR and must not touch the no-evict MSR.
    // Atom/NEM systems clear MSR 0x2e0 below after CPUID model gating.
    // ------------------------------------------------------------------
    "_car_teardown:",
    // Preserve RBX for the x86_64 C ABI. CPUID below clobbers EBX, and this
    // routine returns to Rust code that may keep live state in RBX.
    "movq %rbx, %r8",
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
    "movq %r8, %rbx",
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

fn highest_power_of_two_le(value: u64) -> u64 {
    1u64 << (u64::BITS - 1 - value.leading_zeros())
}

fn mtrr_chunk_size(base: u64, remaining: u64) -> u64 {
    let base_alignment = if base == 0 {
        1u64 << 63
    } else {
        1u64 << base.trailing_zeros()
    };
    let mut size = highest_power_of_two_le(remaining);
    while size > base_alignment {
        size >>= 1;
    }
    size
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

        let mut next_mtrr = 0;
        if let (true, Some(size)) = (count > 0, config.low_dram_mtrr_size()) {
            mtrr::set_variable(next_mtrr, 0, size, mtrr::MTRR_TYPE_WRITE_BACK);
            next_mtrr += 1;
        }

        if let Some(rom) = config.rom_range {
            let mut base = rom.base;
            let mut remaining = rom.size;
            while remaining != 0 && next_mtrr < count {
                let size = mtrr_chunk_size(base, remaining);
                mtrr::set_variable(next_mtrr, base, size, mtrr::MTRR_TYPE_WRITE_PROTECT);
                base += size;
                remaining -= size;
                next_mtrr += 1;
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
    cache_base: u64,
    cache_size: u64,
) -> ! {
    let Some(stack_top) = config.stack_top() else {
        crate::halt();
    };
    let boot_path = match fstart_services::resume::boot_path() {
        fstart_types::BootPath::Normal => 0u64,
        fstart_types::BootPath::S3Resume => 1u64,
    };

    unsafe {
        asm!(
            "mov rsp, {stack}",
            "and rsp, -16",
            // Reserve a dummy return address plus four stack-passed
            // arguments. With an aligned stack top, `sub rsp, 40` leaves
            // `rsp % 16 == 8`, matching SysV function-entry alignment for
            // the jmp into `stage_load_mmio_trampoline`.
            "sub rsp, 40",
            "mov qword ptr [rsp], 0",
            "mov qword ptr [rsp + 8], {image_size}",
            "mov qword ptr [rsp + 16], {cache_base}",
            "mov qword ptr [rsp + 24], {cache_size}",
            "mov qword ptr [rsp + 32], {boot_path}",
            "jmp {tramp}",
            stack = in(reg) stack_top,
            image_size = in(reg) size,
            cache_base = in(reg) cache_base,
            cache_size = in(reg) cache_size,
            boot_path = in(reg) boot_path,
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
    cache_base: u64,
    cache_size: u64,
    boot_path: u64,
) -> ! {
    // SAFETY: generated code passes pointers derived from ROM-resident strings
    // and anchor bytes with their original lengths.
    let next_stage =
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(next_ptr, next_len)) };
    // SAFETY: generated code passes the embedded anchor slice pointer/length.
    let anchor = unsafe { core::slice::from_raw_parts(anchor_ptr, anchor_len) };
    // SAFETY: `config` points at the ROM-resident generated config block.
    let config = unsafe { &*config };
    let boot_path = match boot_path {
        1 => fstart_types::BootPath::S3Resume,
        _ => fstart_types::BootPath::Normal,
    };

    // SAFETY: we are now on a DRAM stack and will never return to CAR-backed
    // state. MTRRs are installed before the large FFS/ramstage copy.
    unsafe {
        car_teardown();
        postcar_mtrr_setup(config);
    }

    let entry = quiet_stage_load(
        next_stage, anchor, base, size, cache_base, cache_size, boot_path,
    );
    let handoff_addr = entry.saturating_sub(0x1000) as usize;
    write_stage_handoff(handoff_addr, boot_path);
    crate::jump_to_with_handoff(entry, handoff_addr)
}

#[cfg(feature = "postcar-stage-load")]
fn write_stage_handoff(handoff_addr: usize, boot_path: fstart_types::BootPath) {
    let mut handoff = fstart_types::handoff::StageHandoff::new(0);
    handoff.resume.boot_path = boot_path;
    // SAFETY: generated stage layout reserves a handoff buffer immediately below
    // the next stage load address. This mirrors non-x86 multi-stage handoff.
    let buf = unsafe {
        core::slice::from_raw_parts_mut(
            handoff_addr as *mut u8,
            fstart_types::handoff::HANDOFF_MAX_SIZE,
        )
    };
    if postcard::to_slice(&handoff, buf).is_err() {
        fstart_log::error!("post-CAR stage handoff serialization failed");
        crate::halt();
    }
}

#[cfg(feature = "postcar-stage-load")]
fn quiet_stage_load(
    next_stage: &str,
    anchor_data: &[u8],
    base: u64,
    size: u64,
    cache_base: u64,
    cache_size: u64,
    boot_path: fstart_types::BootPath,
) -> u64 {
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

        let cached_entry = if boot_path.is_s3_resume() {
            cached_entry_bytes(
                cache_base,
                cache_size,
                entry.size,
                region.offset + entry.offset,
            )
        } else {
            None
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
            let entry_seg_off = seg.offset as usize;
            let stored = seg.stored_size as usize;
            let src = if let Some(cached) = cached_entry {
                if entry_seg_off.saturating_add(stored) > cached.len() {
                    loop {}
                }
                &cached[entry_seg_off..entry_seg_off + stored]
            } else {
                if src_off.saturating_add(stored) > image_size {
                    loop {}
                }
                &image[src_off..src_off + stored]
            };

            match seg.compression {
                Compression::None => unsafe {
                    core::ptr::copy(src.as_ptr(), seg.load_addr as *mut u8, stored);
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
                        core::ptr::copy(src.as_ptr(), buf.as_mut_ptr().add(comp_offset), stored);
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
            save_compressed_entry_to_cache(
                image,
                image_size,
                region.offset + entry.offset,
                entry.size,
                cache_base,
                cache_size,
            );
            return entry_addr;
        }
    }

    loop {}
}

#[cfg(feature = "postcar-stage-load")]
fn cached_entry_bytes(
    cache_base: u64,
    cache_size: u64,
    expected_size: u32,
    expected_source_offset: u32,
) -> Option<&'static [u8]> {
    if cache_base == 0 || cache_size < 20 {
        return None;
    }
    let header = unsafe { core::slice::from_raw_parts(cache_base as *const u8, 20) };
    if &header[0..4] != b"FSC1" {
        return None;
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    let header_len = u16::from_le_bytes([header[6], header[7]]) as usize;
    let size = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
    let hash = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    let source_offset = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
    if version != 1
        || header_len != 20
        || size != expected_size as usize
        || source_offset != expected_source_offset
    {
        return None;
    }
    if header_len.saturating_add(size) > cache_size as usize {
        return None;
    }
    let data = unsafe {
        core::slice::from_raw_parts((cache_base as usize + header_len) as *const u8, size)
    };
    let actual = data.iter().fold(0x811c_9dc5u32, |mut h, b| {
        h ^= u32::from(*b);
        h.wrapping_mul(0x0100_0193)
    });
    if actual != hash {
        return None;
    }
    Some(data)
}

#[cfg(feature = "postcar-stage-load")]
fn save_compressed_entry_to_cache(
    image: &[u8],
    image_size: usize,
    entry_offset: u32,
    entry_size: u32,
    cache_base: u64,
    cache_size: u64,
) {
    if cache_base == 0 || cache_size == 0 {
        return;
    }
    let entry_offset = entry_offset as usize;
    let entry_size = entry_size as usize;
    let total = 20usize.saturating_add(entry_size);
    if total > cache_size as usize || entry_offset.saturating_add(entry_size) > image_size {
        return;
    }
    let src = &image[entry_offset..entry_offset + entry_size];
    let dst = unsafe { core::slice::from_raw_parts_mut(cache_base as *mut u8, total) };
    let hash = src.iter().fold(0x811c_9dc5u32, |mut h, b| {
        h ^= u32::from(*b);
        h.wrapping_mul(0x0100_0193)
    });
    dst[0..4].copy_from_slice(b"FSC1");
    dst[4..6].copy_from_slice(&1u16.to_le_bytes());
    dst[6..8].copy_from_slice(&20u16.to_le_bytes());
    dst[8..12].copy_from_slice(&(entry_size as u32).to_le_bytes());
    dst[12..16].copy_from_slice(&hash.to_le_bytes());
    dst[16..20].copy_from_slice(&(entry_offset as u32).to_le_bytes());
    dst[20..total].copy_from_slice(src);
}
