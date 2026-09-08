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
    // stack: capture the return address before disabling CAR, then touch
    // only registers, MSRs, and CR0. Never read the old stack afterward.
    // ------------------------------------------------------------------
    "_car_teardown:",
    // Like coreboot non-evict/exit_car.S: consume the return address while
    // CAR is live. R11 is caller-saved and untouched by CPUID/RDMSR/WRMSR.
    "popq %r11",
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
    "jmp *%r11",
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

/// Version of the authenticated bootstrap handoff following the MTRR prefix.
pub const POSTCAR_STASH_VERSION: u32 = 2;

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
/// postcar entry may depend on after `INVD`. The fixed MTRR prefix is consumed
/// by assembly. The versioned tail carries authenticated metadata, not a claim
/// that the executable has already been loaded: postcar verifies ramstage
/// before entry. RAM integrity across the transition is a platform assumption.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PostcarMtrrStash {
    /// Must be [`POSTCAR_STASH_MAGIC`]; the postcar entry halts otherwise.
    pub magic: u32,
    /// Number of valid `entries` (rest are zero).
    pub count: u32,
    /// Raw `(base, mask)` MSR values, programmed in order as MTRR 0..n.
    pub entries: [PostcarMtrrEntry; POSTCAR_STASH_MAX_ENTRIES],
    /// Must match [`POSTCAR_STASH_VERSION`].
    pub version: u32,
    /// Exact size of this ABI structure.
    pub size: u32,
    /// Explicit little-endian bootstrap descriptor; arch does not parse FFS.
    pub descriptor: [u8; 160],
    /// Authenticated directory reference for mainstage.
    pub directory: [u8; 64],
    /// Image family accepted by the initial verifier.
    pub image_family: [u8; 16],
    /// Signed security version, not proof of persistent rollback enforcement.
    pub security_version: u64,
    /// CPU-visible image window inherited from trusted platform configuration.
    pub image_base: u64,
    pub image_size: u64,
    /// End of the initial low-DRAM window, excluding untrained memory.
    pub ram_end: u64,
    /// Initial locator snapshot, including vendor microcode bounds. This is
    /// predecessor-owned transport, NOT newly signed directory content.
    pub locator: [u8; 40],
}

// Assembly consumes only this fixed prefix. Keep Rust/entry offsets coupled.
const _: () = {
    assert!(core::mem::offset_of!(PostcarMtrrStash, magic) == 0);
    assert!(core::mem::offset_of!(PostcarMtrrStash, count) == 4);
    assert!(core::mem::offset_of!(PostcarMtrrStash, entries) == 8);
    assert!(core::mem::size_of::<PostcarMtrrStash>() <= 0x1000);
};

impl PostcarMtrrStash {
    /// Structural checks only. The trusted predecessor and reserved RAM
    /// lifetime, not this magic value, establish the handoff's provenance.
    pub fn valid_header(&self) -> bool {
        self.magic == POSTCAR_STASH_MAGIC
            && self.version == POSTCAR_STASH_VERSION
            && self.size as usize == core::mem::size_of::<Self>()
            && self.count != 0
            && self.count as usize <= POSTCAR_STASH_MAX_ENTRIES
            && self.image_size != 0
            && self.image_base.checked_add(self.image_size).is_some()
            && self.ram_end != 0
    }
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

/// Authenticated boot metadata encoded by the image-format layer.
#[derive(Debug, Clone, Copy)]
pub struct PostcarBootContext {
    pub descriptor: [u8; 160],
    pub directory: [u8; 64],
    pub image_family: [u8; 16],
    pub security_version: u64,
    pub locator: [u8; 40],
}

/// Write the post-CAR MTRR stash to [`POSTCAR_STASH_ADDR`].
///
/// Programs, as raw MSR values: one write-back MTRR covering low DRAM from
/// 0 to the next power of two >= `ram_end`, plus write-protect MTRRs
/// covering the memory-mapped firmware window (`rom_base`, `rom_size`).
/// This mirrors the old `postcar_mtrr_setup` layout; the ramstage later
/// refines MTRRs (fixed, per-CPU) via `setup_ram_wb`.
///
/// `boot` carries the ramstage descriptor and mainstage directory reference.
/// Postcar hashes the executable before entry without a directory parser or
/// signature implementation. Returns false without publishing valid magic
/// if the MTRR program cannot cover the requested window.
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
pub unsafe fn write_postcar_stash(
    ram_end: u64,
    rom_base: u64,
    rom_size: u64,
    boot: PostcarBootContext,
) -> bool {
    let stash = POSTCAR_STASH_ADDR as *mut PostcarMtrrStash;
    // Invalidate an earlier boot's handoff before writing any new fields.
    unsafe { core::ptr::addr_of_mut!((*stash).magic).write_volatile(0) };
    let Some(size) = ram_end.checked_next_power_of_two() else {
        return false;
    };
    if ram_end == 0 || rom_size == 0 || rom_base.checked_add(rom_size).is_none() {
        return false;
    }
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
    let size = size.max(0x0010_0000);
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

    if remaining != 0 {
        return false;
    }
    // SAFETY: stash points at writable low DRAM per the caller's contract.
    unsafe {
        for index in count..POSTCAR_STASH_MAX_ENTRIES {
            core::ptr::addr_of_mut!((*stash).entries[index])
                .write_volatile(PostcarMtrrEntry::default());
        }
        core::ptr::addr_of_mut!((*stash).count).write_volatile(count as u32);
        core::ptr::addr_of_mut!((*stash).version).write_volatile(POSTCAR_STASH_VERSION);
        core::ptr::addr_of_mut!((*stash).size)
            .write_volatile(core::mem::size_of::<PostcarMtrrStash>() as u32);
        core::ptr::addr_of_mut!((*stash).descriptor).write_volatile(boot.descriptor);
        core::ptr::addr_of_mut!((*stash).directory).write_volatile(boot.directory);
        core::ptr::addr_of_mut!((*stash).image_family).write_volatile(boot.image_family);
        core::ptr::addr_of_mut!((*stash).security_version).write_volatile(boot.security_version);
        core::ptr::addr_of_mut!((*stash).image_base).write_volatile(rom_base);
        core::ptr::addr_of_mut!((*stash).image_size).write_volatile(rom_size);
        core::ptr::addr_of_mut!((*stash).ram_end).write_volatile(ram_end);
        core::ptr::addr_of_mut!((*stash).locator).write_volatile(boot.locator);
        // Magic last: structural validity is checked before consuming the ABI.
        core::ptr::addr_of_mut!((*stash).magic).write_volatile(POSTCAR_STASH_MAGIC);
    }
    true
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
