//! Unified Cache-as-RAM (CAR) setup for Intel CPUs.
//!
//! Replaces coreboot's four separate CAR implementations (P3, Core2,
//! Non-Evict, P4 Netburst) with a single assembly sequence that detects
//! the CPU variant via CPUID at runtime and dispatches accordingly.
//!
//! ## Decision tree (all determined by CPUID at runtime)
//!
//! | Feature | P3 (Fam6 <0F) | Core2 (Fam6 0F+) | Atom/NEM | Netburst (FamF) |
//! |---|---|---|---|---|
//! | PHYSMASK high | PAE check | leaf 0x80000008 | leaf 0x80000008 | PAE fallback |
//! | INIT IPI | no | yes | yes | yes + SIPI |
//! | NEM MSR 0x2E0 | no | no | yes | no |
//! | L2 MSR 0x11E | no | yes | conditional | CPUID-gated |
//! | Fill method | rep stosl | rep stosl | per-cacheline + NEM | rep stosl |
//! | check_mtrr | no | no | yes | no |
//!
//! ## How this integrates
//!
//! The linker script provides `_car_base`, `_car_size`, `_ecar_stack`
//! from the board RON's `memory.car` config. The assembly reads these
//! symbols directly.
//!
//! Call order:
//! 1. Reset vector → `_start16bit` → `_start32bit` (existing)
//! 2. `_start32bit` calls `_car_setup` (this module) — still in 32-bit mode
//! 3. `_car_setup` sets up MTRRs + fills cache + sets stack
//! 4. Falls through to page table setup → long mode → `fstart_main`

// ---------------------------------------------------------------------------
// Unified CAR setup — 32-bit protected mode assembly
// ---------------------------------------------------------------------------

// MSR and register constants — documented here for reference; the actual
// values are inlined in the global_asm! blocks below since Rust consts
// cannot be interpolated into asm strings.
//
//   MTRR_DEF_TYPE  = 0x2FF      MTRR_CAP          = 0xFE
//   MTRR_PHYS_BASE(n) = 0x200+2n  MTRR_PHYS_MASK(n) = 0x201+2n
//   NEM_MSR        = 0x2E0      BBL_CR_CTL3        = 0x11E
//   LAPIC_BASE_MSR = 0x1B
//   MTRR_TYPE_WB   = 0x06       MTRR_TYPE_WP       = 0x05
//   MTRR_DEF_TYPE_EN = 1<<11    MTRR_MASK_VALID    = 1<<11
//   CR0_CD = 1<<30              CR0_NW = 1<<29

core::arch::global_asm!(
    ".att_syntax prefix",
    // =====================================================================
    // _car_setup — Unified CAR entry point (called from _start32bit)
    //
    // Preconditions: 32-bit protected mode, flat segments, no stack yet.
    // Postconditions: CAR region is live, %esp set to _ecar_stack.
    //
    // Uses linker symbols: _car_base, _car_size, _ecar_stack, _rom_mtrr_base,
    //                      _rom_mtrr_mask
    // =====================================================================
    ".section .text, \"ax\"",
    ".code32",
    ".global _car_setup",
    "_car_setup:",
    // ------------------------------------------------------------------
    // Phase 0: Detect CPU family/model for variant dispatch.
    //
    //   CPUID leaf 1 → EAX = signature
    //   Family = bits [11:8], ExtFamily = bits [27:20]
    //   Model  = bits [7:4],  ExtModel  = bits [19:16]
    //
    //   DisplayFamily = (family == 0xF) ? family + ext_family : family
    //   DisplayModel  = (family == 6 || 0xF) ? (ext_model << 4) | model : model
    //
    //   Pineview (Atom): family 6, model 0x1C → NEM path
    //   Core 2:          family 6, model 0x0F..0x17 → Core2 path
    //   Netburst (P4):   family 0xF → Netburst path
    //   P3:              family 6, model < 0x0F → P3 path
    // ------------------------------------------------------------------
    "movl $1, %eax",
    "cpuid",
    "movl %eax, %ebp", // save signature in EBP for later
    // Extract family (bits 11:8).
    "movl %ebp, %eax",
    "shrl $8, %eax",
    "andl $0x0F, %eax", // EAX = base family
    // Family 0xF → Netburst path
    "cmpl $0x0F, %eax",
    "je _car_netburst",
    // Family 6 — need to check model for Atom vs Core2 vs P3.
    "cmpl $0x06, %eax",
    "jne _car_p3", // Anything else → P3 (safest fallback)
    // Extract display model: (ext_model << 4) | model
    "movl %ebp, %eax",
    "shrl $4, %eax",
    "andl $0x0F, %eax", // base model
    "movl %ebp, %ecx",
    "shrl $12, %ecx",
    "andl $0xF0, %ecx", // ext_model << 4
    "orl %ecx, %eax",   // EAX = display model
    // Atom models (NEM): 0x1C (Pineview/Bonnell), 0x26 (Lincroft),
    //                     0x27 (Penwell), 0x35 (Cloverview), 0x36 (Cedarview)
    "cmpl $0x1C, %eax",
    "je _car_nem",
    "cmpl $0x26, %eax",
    "je _car_nem",
    "cmpl $0x27, %eax",
    "je _car_nem",
    "cmpl $0x35, %eax",
    "je _car_nem",
    "cmpl $0x36, %eax",
    "je _car_nem",
    // Core2: model >= 0x0F
    "cmpl $0x0F, %eax",
    "jge _car_core2",
    // Fall through to P3 for older family 6.
    "jmp _car_p3",
    // ==================================================================
    // Common subroutines (called from all paths)
    // ==================================================================

    // --- clear_fixed_mtrrs: zero all fixed MTRRs ---
    "_car_clear_fixed_mtrrs:",
    "movl $_fixed_mtrr_list, %ebx",
    "xorl %eax, %eax",
    "xorl %edx, %edx",
    "1:",
    "movzwl (%ebx), %ecx",
    "wrmsr",
    "addl $2, %ebx",
    "cmpl $_fixed_mtrr_list_end, %ebx",
    "jl 1b",
    "ret",
    // --- clear_var_mtrrs: zero all variable MTRRs ---
    "_car_clear_var_mtrrs:",
    "movl $0xFE, %ecx", // MTRR_CAP
    "rdmsr",
    "movzbl %al, %ebx",  // number of variable MTRRs
    "movl $0x200, %ecx", // MTRR_PHYS_BASE(0)
    "xorl %eax, %eax",
    "xorl %edx, %edx",
    "1:",
    "wrmsr",
    "incl %ecx",
    "wrmsr",
    "incl %ecx",
    "decl %ebx",
    "jnz 1b",
    "ret",
    // --- physmask_high: compute PHYSMASK high word based on CPU ---
    // Returns result in %edx. Uses %eax, %ecx.
    "_car_physmask_high:",
    // Try extended leaf 0x80000008 first (Core2, Atom, modern CPUs).
    "movl $0x80000000, %eax",
    "cpuid",
    "cmpl $0x80000008, %eax",
    "jc _car_physmask_legacy",
    "movl $0x80000008, %eax",
    "cpuid",
    "movb %al, %cl",
    "subb $32, %cl",
    "movl $1, %edx",
    "shll %cl, %edx",
    "subl $1, %edx",
    "ret",
    // Legacy fallback (P3, old P4): check PAE/PSE36.
    "_car_physmask_legacy:",
    "movl $1, %eax",
    "cpuid",
    "andl $(1 << 6 | 1 << 17), %edx", // PAE or PSE36
    "jz 1f",
    "movl $0x0F, %edx",
    "ret",
    "1:",
    "xorl %edx, %edx",
    "ret",
    // --- setup_car_mtrrs: program MTRR0 = CAR (WB), preload MASK high ---
    // Expects PHYSMASK high word in %edx on entry.
    "_car_setup_mtrrs:",
    // Preload PHYSMASK high word for MTRR0 and MTRR1.
    "xorl %eax, %eax",
    "movl $0x201, %ecx", // MTRR_PHYS_MASK(0)
    "wrmsr",
    "movl $0x203, %ecx", // MTRR_PHYS_MASK(1)
    "wrmsr",
    // MTRR0 BASE = _car_base | WB
    "movl $0x200, %ecx", // MTRR_PHYS_BASE(0)
    "movl $_car_base, %eax",
    "orl $0x06, %eax", // MTRR_TYPE_WRBACK
    "xorl %edx, %edx",
    "wrmsr",
    // MTRR0 MASK = ~(car_size - 1) | VALID
    "movl $0x201, %ecx", // MTRR_PHYS_MASK(0)
    "rdmsr",             // high word was preloaded above
    "movl $_car_size, %eax",
    "negl %eax",        // ~(size - 1) for power-of-2 size
    "orl $0x800, %eax", // MTRR_PHYS_MASK_VALID
    "wrmsr",
    "ret",
    // --- setup_rom_mtrr: program MTRR1 = ROM (WRPROT) ---
    "_car_setup_rom_mtrr:",
    "movl $0x202, %ecx", // MTRR_PHYS_BASE(1)
    "xorl %edx, %edx",
    "movl $_rom_mtrr_base, %eax",
    "orl $0x05, %eax", // MTRR_TYPE_WRPROT
    "wrmsr",
    "movl $0x203, %ecx", // MTRR_PHYS_MASK(1)
    "rdmsr",
    "movl $_rom_mtrr_mask, %eax",
    "orl $0x800, %eax", // MTRR_PHYS_MASK_VALID
    "wrmsr",
    "ret",
    // --- enable_mtrrs: set MTRR_DEF_TYPE_EN ---
    "_car_enable_mtrrs:",
    "movl $0x2FF, %ecx", // MTRR_DEF_TYPE
    "rdmsr",
    "orl $0x800, %eax", // MTRR_DEF_TYPE_EN
    "wrmsr",
    "ret",
    // --- enable_cache: clear CR0.CD and CR0.NW ---
    "_car_enable_cache:",
    "movl %cr0, %eax",
    "andl $0x9FFFFFFF, %eax", // ~(CD | NW)
    "invd",
    "movl %eax, %cr0",
    "ret",
    // --- disable_cache: set CR0.CD ---
    "_car_disable_cache:",
    "movl %cr0, %eax",
    "orl $0x40000000, %eax", // CD
    "movl %eax, %cr0",
    "ret",
    // --- fill_car_rep_stosl: zero-fill CAR region via rep stosl ---
    "_car_fill_stosl:",
    "cld",
    "xorl %eax, %eax",
    "movl $_car_base, %edi",
    "movl $_car_size, %ecx",
    "shrl $2, %ecx",
    "rep stosl",
    "ret",
    // --- setup_stack: set ESP to _ecar_stack, aligned ---
    "_car_setup_stack:",
    "movl $_ecar_stack, %esp",
    "andl $0xFFFFFFF0, %esp",
    "ret",
    // --- try_enable_l2: enable L2 via BBL_CR_CTL3 MSR if supported ---
    // CPUID-gated: only family 6 models 0x00-0x1F and family 0xF
    "_car_try_enable_l2:",
    "movl $0x11E, %ecx", // BBL_CR_CTL3
    "rdmsr",
    "orl $0x100, %eax", // bit 8 = L2 enable
    "wrmsr",
    "ret",
    // --- send_init_ipi: INIT IPI to all excluding self ---
    "_car_send_init_ipi:",
    "movl $0x000C4500, %eax",
    "movl $0xFEE00300, %esi", // LAPIC ICR
    "movl %eax, (%esi)",
    // Wait for delivery.
    "1:",
    "movl (%esi), %eax",
    "btl $12, %eax",
    "jc 1b",
    "ret",
    // ==================================================================
    // Path: NEM (Non-Evict Mode) — Atom Pineview/Cedarview
    //
    // The key difference: instead of filling the cache by zeroing the
    // region (which requires the region to be RAM), NEM pins cache lines
    // by writing one byte per cacheline with eviction disabled.
    // ==================================================================
    "_car_nem:",
    // Check MTRR_DEF_TYPE for warm reset (must be clean).
    "movl $0x2FF, %ecx",
    "rdmsr",
    "andl $0xC00, %eax", // DEF_TYPE_EN | FIX_EN
    "jz 1f",
    // Warm reset detected — write 0x06 to CF9.
    "movw $0xCF9, %dx",
    "movb $0x06, %al",
    "outb %al, %dx",
    "2: hlt",
    "jmp 2b",
    "1:",
    // Set up a temporary stack in the return address register for calls.
    "movl $_car_nem_ret, %esp",
    "call _car_clear_fixed_mtrrs",
    "call _car_clear_var_mtrrs",
    // Configure default type to uncacheable.
    "movl $0x2FF, %ecx",
    "xorl %eax, %eax",
    "xorl %edx, %edx",
    "wrmsr",
    "call _car_physmask_high",
    // %edx = physmask high word
    "call _car_setup_mtrrs",
    "call _car_setup_rom_mtrr",
    "call _car_enable_mtrrs",
    // Enable L2 if supported (CPU_HAS_L2_ENABLE_MSR).
    "call _car_try_enable_l2",
    // Enable cache.
    "call _car_enable_cache",
    // Disable cache for NEM fill sequence.
    "call _car_disable_cache",
    "invd",
    "call _car_enable_cache",
    // NEM step 1: set NO_EVICT_MODE_SETUP (MSR 0x2E0 bit 0).
    "movl $0x2E0, %ecx",
    "rdmsr",
    "orl $1, %eax",
    "andl $0xFFFFFFFD, %eax", // clear bit 1 (RUN)
    "wrmsr",
    // NEM step 2: fill CAR by writing one dword per 64-byte cacheline.
    "movl $_car_base, %edi",
    "movl $_car_size, %ecx",
    "shrl $6, %ecx", // count = size / 64
    "movl $64, %ebx",
    "3:",
    "movl %eax, (%edi)", // one write per cacheline
    "addl %ebx, %edi",
    "loop 3b",
    // NEM step 3: set NO_EVICT_MODE_RUN (MSR 0x2E0 bits 0+1).
    "movl $0x2E0, %ecx",
    "rdmsr",
    "orl $3, %eax",
    "wrmsr",
    // Zero the CAR region (BSS must be clean).
    "call _car_fill_stosl",
    // Send INIT IPI to APs.
    "call _car_send_init_ipi",
    // Set up stack and continue.
    "call _car_setup_stack",
    "jmp _car_done",
    "_car_nem_ret:",
    // ==================================================================
    // Path: Core2 (family 6, model >= 0x0F) — Sandy Bridge, etc.
    //
    // Standard fill method: enable cache, rep stosl to fill & zero,
    // disable cache, set ROM MTRR, re-enable cache.
    // ==================================================================
    "_car_core2:",
    "movl $_car_core2_ret, %esp",
    "call _car_send_init_ipi",
    "call _car_clear_fixed_mtrrs",
    "call _car_clear_var_mtrrs",
    // Default type = UC.
    "movl $0x2FF, %ecx",
    "rdmsr",
    "andl $0xFFFFF300, %eax", // clear type + enable bits
    "wrmsr",
    "call _car_physmask_high",
    "call _car_setup_mtrrs",
    "call _car_enable_mtrrs",
    "call _car_try_enable_l2",
    "call _car_enable_cache",
    // Fill CAR by zeroing (fills cache as side effect).
    "call _car_fill_stosl",
    // Disable cache to change MTRRs.
    "call _car_disable_cache",
    // Set ROM MTRR for XIP.
    "call _car_setup_rom_mtrr",
    // Re-enable cache.
    "call _car_enable_cache",
    "call _car_setup_stack",
    "jmp _car_done",
    "_car_core2_ret:",
    // ==================================================================
    // Path: P3 (family 6, model < 0x0F)
    //
    // Simplest variant: no INIT IPI, no L2 MSR, legacy PHYSMASK.
    // ==================================================================
    "_car_p3:",
    "movl $_car_p3_ret, %esp",
    "call _car_clear_fixed_mtrrs",
    "call _car_clear_var_mtrrs",
    // Default type = UC.
    "movl $0x2FF, %ecx",
    "rdmsr",
    "andl $0xFFFFF300, %eax",
    "wrmsr",
    "call _car_physmask_high",
    "call _car_setup_mtrrs",
    "call _car_enable_mtrrs",
    "call _car_enable_cache",
    "call _car_fill_stosl",
    "call _car_disable_cache",
    "call _car_setup_rom_mtrr",
    "call _car_enable_cache",
    "call _car_setup_stack",
    "jmp _car_done",
    "_car_p3_ret:",
    // ==================================================================
    // Path: Netburst / P4 (family 0xF)
    //
    // Like Core2 but with HT AP handling and family-F ROM MTRR quirk
    // (disable WRPROT region on family F to avoid speculative invalidation).
    // ==================================================================
    "_car_netburst:",
    "movl $_car_nb_ret, %esp",
    // Check if BSP.
    "movl $0x1B, %ecx", // LAPIC_BASE_MSR
    "rdmsr",
    "andl $0x100, %eax", // BSP flag
    "jz _car_nb_ap_halt",
    "call _car_clear_fixed_mtrrs",
    "call _car_clear_var_mtrrs",
    // Default type = UC.
    "movl $0x2FF, %ecx",
    "rdmsr",
    "andl $0xFFFFF300, %eax",
    "wrmsr",
    "call _car_physmask_high",
    // Preload + LAPIC enable.
    "xorl %eax, %eax",
    "movl $0x201, %ecx",
    "wrmsr",
    "movl $0x203, %ecx",
    "wrmsr",
    // Enable LAPIC at default base.
    "movl $0x1B, %ecx",
    "rdmsr",
    "andl $0xFFFFF000, %eax",
    "orl $0xFEE00900, %eax", // default base + enable
    "wrmsr",
    // Send INIT IPI.
    "call _car_send_init_ipi",
    "call _car_setup_mtrrs",
    "call _car_enable_mtrrs",
    // L2 enable — CPUID-gated for family 6 models.
    // On family F, BBL_CR_CTL3 applies to some steppings.
    "movl %ebp, %eax", // saved CPUID signature
    "andl $0x0F00, %eax",
    "cmpl $0x0600, %eax",
    "jne _car_nb_skip_l2",
    "call _car_try_enable_l2",
    "_car_nb_skip_l2:",
    // Cache ROM for microcode fetch.
    "call _car_setup_rom_mtrr",
    "call _car_enable_cache",
    // Disable cache to change MTRRs safely.
    "call _car_disable_cache",
    // Family F quirk: disable WRPROT MTRR to avoid speculative
    // invalidation on Netburst microarchitecture.
    "movl %ebp, %eax",
    "shrl $8, %eax",
    "andl $0x0F, %eax",
    "cmpl $0x0F, %eax",
    "jne _car_nb_keep_rom",
    // Disable ROM MTRR for family F.
    "movl $0x203, %ecx", // MTRR_PHYS_MASK(1)
    "rdmsr",
    "andl $0xFFFFF7FF, %eax", // clear VALID
    "wrmsr",
    "jmp _car_nb_fill",
    "_car_nb_keep_rom:",
    "call _car_setup_rom_mtrr",
    "_car_nb_fill:",
    "call _car_enable_cache",
    // Fill + zero CAR.
    "call _car_fill_stosl",
    "call _car_setup_stack",
    "jmp _car_done",
    // AP halt (Netburst only — APs loop here).
    "_car_nb_ap_halt:",
    "movl %cr0, %eax",
    "andl $0x9FFFFFFF, %eax",
    "movl %eax, %cr0",
    "cli",
    "4: hlt",
    "jmp 4b",
    "_car_nb_ret:",
    // ==================================================================
    // Common exit point — CAR is live, stack is set.
    // ==================================================================
    "_car_done:",
    // CAR is now live.  Return to the caller via %ebp.
    //
    // _car_setup runs BEFORE there is a stack (the whole point is to
    // create the stack in cache).  It cannot be called with `call` /
    // returned from with `ret`.  Instead the entry asm does:
    //
    //     lea 1f(%rip), %ebp
    //     jmp _car_setup
    //   1:
    //     /* post-CAR: set %esp, clear BSS, etc. */
    //
    // %ebp is callee-preserved across the NEM/evict paths above.
    "jmp *%ebp",
    // Fixed MTRR list (shared by all paths).
    "_fixed_mtrr_list:",
    ".word 0x250", // MTRR_FIX_64K_00000
    ".word 0x258", // MTRR_FIX_16K_80000
    ".word 0x259", // MTRR_FIX_16K_A0000
    ".word 0x268", // MTRR_FIX_4K_C0000
    ".word 0x269", // MTRR_FIX_4K_C8000
    ".word 0x26A", // MTRR_FIX_4K_D0000
    ".word 0x26B", // MTRR_FIX_4K_D8000
    ".word 0x26C", // MTRR_FIX_4K_E0000
    ".word 0x26D", // MTRR_FIX_4K_E8000
    ".word 0x26E", // MTRR_FIX_4K_F0000
    ".word 0x26F", // MTRR_FIX_4K_F8000
    "_fixed_mtrr_list_end:",
);

// ---------------------------------------------------------------------------
// Rust-callable wrapper
// ---------------------------------------------------------------------------

// _car_setup is a global_asm label, not an extern "C" function.
// It MUST be invoked via jmp (not call) because there is no stack
// before CAR is enabled.  The caller stores a continuation address
// in %ebp and jumps:
//
//     lea post_car(%rip), %ebp
//     jmp _car_setup
//   post_car:
//     /* now CAR is live, set %esp, clear BSS, etc. */
//
// This matches coreboot's approach — CAR setup happens before ANY
// stack operations.  The Rust wrapper is gone because Rust's
// extern "C" fn call convention requires a stack.

// CAR teardown lives in car_teardown.rs — it runs at the start of the
// *next* stage (ramstage), after the stack has moved to DRAM. It cannot
// run in the same stage that set up CAR because it destroys the cache
// backing the current stack and BSS.
