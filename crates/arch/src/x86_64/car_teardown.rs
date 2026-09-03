//! Cache-as-RAM teardown and the postcar MTRR stash for x86 platforms.
//!
//! Cut-B stage model (see coreboot `postcar` + change 95145):
//!
//! 1. The bootblock (CAR) raw-copies the tiny postcar binary into DRAM with
//!    uncached stores and writes the [`PostcarMtrrStash`] to fixed UC-DRAM
//!    scratch. Both are `INVD`-proof by construction.
//! 2. The postcar entry (fresh binary, still on the inherited CAR stack)
//!    calls [`car_teardown`], programs variable MTRRs from the stash,
//!    clears `CR0.CD`, executes `INVD`, switches to a fresh DRAM stack, and
//!    jumps to its Rust main — which loads/decompresses the ramstage cached.
//! 3. The ramstage entry never touches CAR teardown; it assumes caching is
//!    already on.
//!
//! Post-`INVD` execution happens only in fresh programs (postcar, ramstage).
//! No pre-transition program executes after the transition, so there is no
//! stale stack/BSS/heap to audit — only the UC stash and ROM constants cross
//! it.

use core::arch::global_asm;

use crate::x86::mtrr;

global_asm!(
    ".text",
    ".code64",
    ".global _car_teardown",
    // ------------------------------------------------------------------
    // _car_teardown — Intel CAR teardown.
    //
    // Core2/X61 uses MTRR-backed CAR and must not touch the no-evict MSR.
    // Atom/NEM systems clear MSR 0x2e0 below after CPUID model gating.
    //
    // Callable from the postcar entry while still on the inherited CAR
    // stack: it only touches registers, MSRs, and CR0 (plus one pushed
    // return address in still-live CAR).
    // ------------------------------------------------------------------
    "_car_teardown:",
    // Preserve RBX for the x86_64 C ABI. CPUID below clobbers EBX, and this
    // routine returns to code that may keep live state in RBX.
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

/// Fixed physical address of the post-CAR MTRR stash in low DRAM.
///
/// Written by the bootblock with uncached stores (default-UC under the
/// CAR-phase MTRRs, hence `INVD`-proof) before jumping to postcar. Read by
/// the postcar entry while still uncached, before it enables caching.
///
/// `0x2000` dodges the real-mode IVT/BDA (below `0x500`), stays clear of the
/// SIPI trampoline page at `0x8000` and default SMRAM at `0x30000`, and sits
/// far below every stage load address.
pub const POSTCAR_STASH_ADDR: u64 = 0x2000;

/// Magic at the start of [`PostcarMtrrStash`] (`"PCST"`).
pub const POSTCAR_STASH_MAGIC: u32 = 0x5453_4350;

/// Maximum variable-MTRR entries the stash can carry (Core2/Atom have 8;
/// the table needs 1x low-DRAM WB + ROM WP chunks).
pub const POSTCAR_STASH_MAX_ENTRIES: usize = 8;

/// One precomputed variable MTRR as raw MSR values.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct PostcarMtrrEntry {
    /// Value for `IA32_MTRR_PHYSBASEn` (base + type).
    pub base: u64,
    /// Value for `IA32_MTRR_PHYSMASKn` (mask + VALID).
    pub mask: u64,
}

/// Precomputed post-CAR variable-MTRR table in UC-DRAM scratch.
///
/// The only data (besides ROM constants and hardware registers) that the
/// postcar entry may depend on after `INVD`. The trailing file fields let
/// postcar raw-load the ramstage without an FFS parser or crypto: the
/// bootblock already signature-verified the manifest (ROM-immutable, so the
/// parse is sound to trust), and the ramstage re-verifies its own bytes
/// against the manifest before running tables or payloads.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PostcarMtrrStash {
    /// Must be [`POSTCAR_STASH_MAGIC`]; the postcar entry halts otherwise.
    pub magic: u32,
    /// Number of valid `entries` (rest are zero).
    pub count: u32,
    /// Raw `(base, mask)` MSR values, programmed in order as MTRR 0..n.
    pub entries: [PostcarMtrrEntry; POSTCAR_STASH_MAX_ENTRIES],
    /// FFS-image-relative byte offset of the ramstage file's data segment.
    pub file_offset: u64,
    /// Stored (possibly compressed) size of that segment.
    pub stored_size: u64,
    /// Decompressed size at `load_addr`.
    pub loaded_size: u64,
    /// Builder-verified scratch size for in-place LZ4 (`0` when uncompressed).
    pub in_place_size: u64,
    /// DRAM address the segment loads to (also the entry point).
    pub load_addr: u64,
    /// Compression tag: `0` = none, `1` = LZ4. Anything else halts postcar.
    pub compression: u32,
    /// Reserved, must be zero.
    pub _reserved: u32,
}

/// Tear down Cache-as-RAM non-evict mode.
///
/// # Safety
///
/// Caller must already be executing on a DRAM stack and must not return to
/// CAR-backed data after this call — or, for the postcar entry, must treat
/// this as the noreturn transition (fresh stack + `INVD` immediately after).
pub unsafe fn car_teardown() {
    unsafe { _car_teardown() }
}

/// Raw ramstage location for the [`PostcarMtrrStash`] file fields.
///
/// Resolved by the bootblock from the signature-verified manifest; postcar
/// copies/decompresses these bytes without parsing FFS itself.
#[derive(Debug, Clone, Copy)]
pub struct PostcarFile {
    /// FFS-image-relative byte offset of the data segment.
    pub file_offset: u64,
    /// Stored (possibly compressed) size.
    pub stored_size: u64,
    /// Decompressed size at `load_addr`.
    pub loaded_size: u64,
    /// Builder-verified scratch size for in-place LZ4 (`0` = uncompressed).
    pub in_place_size: u64,
    /// DRAM load address (also the entry point).
    pub load_addr: u64,
    /// `true` when the segment is LZ4-compressed.
    pub compressed: bool,
}

/// Write the post-CAR MTRR stash to [`POSTCAR_STASH_ADDR`].
///
/// Programs, as raw MSR values: one write-back MTRR covering low DRAM from
/// 0 to the next power of two >= `ram_end`, plus write-protect MTRRs
/// covering the memory-mapped firmware window (`rom_base`, `rom_size`).
/// This mirrors the old `postcar_mtrr_setup` layout; the ramstage later
/// refines MTRRs (fixed, per-CPU) via `setup_ram_wb`.
///
/// `file` carries the ramstage's raw location so postcar loads it without
/// an FFS parser or crypto (see [`PostcarMtrrStash`] for the trust argument).
///
/// All stores are volatile so they reach DRAM even under CAR-phase MTRRs
/// (UC), making the stash `INVD`-proof by construction.
///
/// # Safety
///
/// `ram_end` must be the end of trained low DRAM; `rom_base`/`rom_size` the
/// CPU-visible firmware window. Low DRAM below [`POSTCAR_STASH_ADDR`] + table
/// size must be writable (it always is — the stash sits in conventional
/// memory).
pub unsafe fn write_postcar_stash(ram_end: u64, rom_base: u64, rom_size: u64, file: PostcarFile) {
    let stash = POSTCAR_STASH_ADDR as *mut PostcarMtrrStash;
    let mut count = 0usize;
    // Push one raw `(base, mask)` entry; silently drops overflow past the
    // fixed array (the table needs 1x WB + ROM WP chunks — far below 8).
    let push = |count: &mut usize, base: u64, mask: u64| {
        if *count < POSTCAR_STASH_MAX_ENTRIES {
            // SAFETY: `count` bounds the write into the fixed array.
            unsafe {
                core::ptr::addr_of_mut!((*stash).entries[*count])
                    .write_volatile(PostcarMtrrEntry { base, mask });
            }
            *count += 1;
        }
    };

    // Low DRAM: single WB MTRR, next power of two covering ram_end.
    let size = ram_end.next_power_of_two().max(0x0010_0000);
    let (base, mask) = mtrr::encode_variable(0, size, mtrr::MTRR_TYPE_WRITE_BACK);
    push(&mut count, base, mask);

    // Firmware window: WB would be wrong for flash; WP chunks it.
    let mut base_addr = rom_base;
    let mut remaining = rom_size;
    while remaining != 0 && count < POSTCAR_STASH_MAX_ENTRIES {
        let size = mtrr_chunk_size(base_addr, remaining);
        let (base, mask) = mtrr::encode_variable(base_addr, size, mtrr::MTRR_TYPE_WRITE_PROTECT);
        push(&mut count, base, mask);
        base_addr += size;
        remaining -= size;
    }

    // SAFETY: stash points at writable low DRAM per the caller's contract.
    unsafe {
        core::ptr::addr_of_mut!((*stash).count).write_volatile(count as u32);
        core::ptr::addr_of_mut!((*stash).file_offset).write_volatile(file.file_offset);
        core::ptr::addr_of_mut!((*stash).stored_size).write_volatile(file.stored_size);
        core::ptr::addr_of_mut!((*stash).loaded_size).write_volatile(file.loaded_size);
        core::ptr::addr_of_mut!((*stash).in_place_size).write_volatile(file.in_place_size);
        core::ptr::addr_of_mut!((*stash).load_addr).write_volatile(file.load_addr);
        core::ptr::addr_of_mut!((*stash).compression).write_volatile(u32::from(file.compressed));
        core::ptr::addr_of_mut!((*stash)._reserved).write_volatile(0);
        // Magic last: the postcar entry validates it before trusting anything.
        core::ptr::addr_of_mut!((*stash).magic).write_volatile(POSTCAR_STASH_MAGIC);
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
