//! Unified Cache-as-RAM (CAR) setup for Intel CPUs.
//!
//! Replaces coreboot's four separate CAR implementations (P3, Core2,
//! Non-Evict, P4 Netburst) with a single assembly sequence that detects
//! the CPU variant via CPUID at runtime and dispatches accordingly.
//!
//! ## IMPORTANT: No stack before CAR is live
//!
//! This code runs BEFORE there is any writable memory.  All subroutine
//! "calls" use `jmp` with a return address in `%esp` — never `call`/`ret`,
//! which require a stack.  This matches coreboot's approach.
//!
//! Pattern:
//! ```asm
//!   movl $1f, %esp      // save return address
//!   jmp  _helper         // jump to subroutine
//! 1:                      // helper returns here via jmp *%esp
//! ```
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
//! The entry asm (`_start32bit`) invokes CAR setup via:
//! ```asm
//!   lea 1f, %ebp          // continuation address
//!   jmp _car_setup
//! 1:                       // returns here, CAR + stack live
//! ```

core::arch::global_asm!(
    ".att_syntax prefix",
    ".section .text, \"ax\"",
    ".code32",
    ".global _car_setup",
    // =====================================================================
    // _car_setup — Unified CAR entry point
    //
    // Preconditions:  32-bit protected mode, flat segments, NO STACK.
    // Postconditions: CAR live, %esp = _ecar_stack (stack in CAR).
    //
    // Caller must store continuation address in %ebp before jumping here.
    //
    // Linker symbols: _car_base, _car_size, _ecar_stack,
    //                 _rom_mtrr_base, _rom_mtrr_mask
    // =====================================================================
    "_car_setup:",
    // ------------------------------------------------------------------
    // Phase 0: Detect CPU family/model via CPUID for variant dispatch.
    // ------------------------------------------------------------------
    "movl $1, %eax",
    "cpuid",
    // Save CPUID signature in %ebx (preserved across all paths).
    // %ebp is reserved for the outer return address.
    "movl %eax, %ebx",
    // Extract family (bits 11:8).
    "movl %ebx, %eax",
    "shrl $8, %eax",
    "andl $0x0F, %eax",
    // Family 0xF → Netburst path.
    "cmpl $0x0F, %eax",
    "je _car_netburst",
    // Family 6 — check model for Atom vs Core2 vs P3.
    "cmpl $0x06, %eax",
    "jne _car_p3", // Anything else → P3 (safest fallback)
    // Extract display model: (ext_model << 4) | model
    "movl %ebx, %eax",
    "shrl $4, %eax",
    "andl $0x0F, %eax", // base model
    "movl %ebx, %ecx",
    "shrl $12, %ecx",
    "andl $0xF0, %ecx", // ext_model << 4
    "orl %ecx, %eax",   // EAX = display model
    // Atom models (NEM): 0x1C, 0x26, 0x27, 0x35, 0x36
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
    // Subroutines — all return via jmp *%esp (no stack needed).
    //
    // Caller convention:
    //   movl $1f, %esp
    //   jmp _car_<helper>
    // 1:
    //
    // Clobbers: listed per subroutine.  %ebp and %ebx preserved.
    // ==================================================================

    // --- clear_fixed_mtrrs: zero all fixed MTRRs ---
    // Clobbers: %eax, %ecx, %edx, %esi
    "_car_clear_fixed_mtrrs:",
    "movl $_fixed_mtrr_list, %esi",
    "xorl %eax, %eax",
    "xorl %edx, %edx",
    "1:",
    "movzwl (%esi), %ecx",
    "wrmsr",
    "addl $2, %esi",
    "cmpl $_fixed_mtrr_list_end, %esi",
    "jl 1b",
    "jmp *%esp",
    // --- clear_var_mtrrs: zero all variable MTRRs ---
    // Clobbers: %eax, %ecx, %edx, %esi
    "_car_clear_var_mtrrs:",
    "movl $0xFE, %ecx", // MTRR_CAP
    "rdmsr",
    "movzbl %al, %esi",  // number of variable MTRRs
    "movl $0x200, %ecx", // MTRR_PHYS_BASE(0)
    "xorl %eax, %eax",
    "xorl %edx, %edx",
    "1:",
    "wrmsr",
    "incl %ecx",
    "wrmsr",
    "incl %ecx",
    "decl %esi",
    "jnz 1b",
    "jmp *%esp",
    // --- physmask_high: compute PHYSMASK high word ---
    // Returns result in %edx.  Clobbers: %eax, %ecx.
    "_car_physmask_high:",
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
    "jmp *%esp",
    "_car_physmask_legacy:",
    "movl $1, %eax",
    "cpuid",
    "andl $(1 << 6 | 1 << 17), %edx", // PAE or PSE36
    "jz 1f",
    "movl $0x0F, %edx",
    "jmp *%esp",
    "1:",
    "xorl %edx, %edx",
    "jmp *%esp",
    // --- setup_car_mtrrs: MTRR0 = CAR (WB) ---
    // Expects PHYSMASK high word in %edx.
    // Clobbers: %eax, %ecx (preserves %edx high-word pattern via rdmsr).
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
    "jmp *%esp",
    // --- setup_rom_mtrr: MTRR1 = ROM (WRPROT) ---
    // Clobbers: %eax, %ecx, %edx
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
    "jmp *%esp",
    // --- enable_mtrrs: set MTRR_DEF_TYPE_EN ---
    // Clobbers: %eax, %ecx, %edx
    "_car_enable_mtrrs:",
    "movl $0x2FF, %ecx", // MTRR_DEF_TYPE
    "rdmsr",
    "orl $0x800, %eax", // MTRR_DEF_TYPE_EN
    "wrmsr",
    "jmp *%esp",
    // --- enable_cache: clear CR0.CD and CR0.NW ---
    "_car_enable_cache:",
    "movl %cr0, %eax",
    "andl $0x9FFFFFFF, %eax", // ~(CD | NW)
    "invd",
    "movl %eax, %cr0",
    "jmp *%esp",
    // --- disable_cache: set CR0.CD ---
    "_car_disable_cache:",
    "movl %cr0, %eax",
    "orl $0x40000000, %eax", // CD
    "movl %eax, %cr0",
    "jmp *%esp",
    // --- fill_car_rep_stosl: zero-fill CAR region ---
    // Clobbers: %eax, %ecx, %edi
    "_car_fill_stosl:",
    "cld",
    "xorl %eax, %eax",
    "movl $_car_base, %edi",
    "movl $_car_size, %ecx",
    "shrl $2, %ecx",
    "rep stosl",
    "jmp *%esp",
    // --- try_enable_l2: BBL_CR_CTL3 MSR bit 8 ---
    "_car_try_enable_l2:",
    "movl $0x11E, %ecx", // BBL_CR_CTL3
    "rdmsr",
    "orl $0x100, %eax", // bit 8 = L2 enable
    "wrmsr",
    "jmp *%esp",
    // --- send_init_ipi: INIT IPI to all excluding self ---
    // Clobbers: %eax, %esi
    "_car_send_init_ipi:",
    "movl $0x000C4500, %eax",
    "movl $0xFEE00300, %esi", // LAPIC ICR
    "movl %eax, (%esi)",
    "1:",
    "movl (%esi), %eax",
    "btl $12, %eax",
    "jc 1b",
    "jmp *%esp",
    // ==================================================================
    // Path: NEM (Non-Evict Mode) — Atom Pineview/Cedarview
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
    "movl $10f, %esp",
    "jmp _car_clear_fixed_mtrrs",
    "10:",
    "movl $11f, %esp",
    "jmp _car_clear_var_mtrrs",
    "11:",
    // Default type = UC.
    "movl $0x2FF, %ecx",
    "xorl %eax, %eax",
    "xorl %edx, %edx",
    "wrmsr",
    "movl $12f, %esp",
    "jmp _car_physmask_high",
    "12:",
    // %edx = physmask high word
    "movl $13f, %esp",
    "jmp _car_setup_mtrrs",
    "13:",
    "movl $14f, %esp",
    "jmp _car_setup_rom_mtrr",
    "14:",
    "movl $15f, %esp",
    "jmp _car_enable_mtrrs",
    "15:",
    "movl $16f, %esp",
    "jmp _car_try_enable_l2",
    "16:",
    "movl $17f, %esp",
    "jmp _car_enable_cache",
    "17:",
    // Disable cache for NEM fill sequence.
    "movl $18f, %esp",
    "jmp _car_disable_cache",
    "18:",
    "invd",
    "movl $19f, %esp",
    "jmp _car_enable_cache",
    "19:",
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
    "3:",
    "movl %eax, (%edi)", // one write per cacheline
    "addl $64, %edi",
    "loop 3b",
    // NEM step 3: set NO_EVICT_MODE_RUN (MSR 0x2E0 bits 0+1).
    "movl $0x2E0, %ecx",
    "rdmsr",
    "orl $3, %eax",
    "wrmsr",
    // Zero the CAR region (BSS must be clean).
    "movl $20f, %esp",
    "jmp _car_fill_stosl",
    "20:",
    // Send INIT IPI to APs.
    "movl $21f, %esp",
    "jmp _car_send_init_ipi",
    "21:",
    // Set up stack (inline — can't use %esp as return reg here).
    "movl $_ecar_stack, %esp",
    "andl $0xFFFFFFF0, %esp",
    "jmp _car_done",
    // ==================================================================
    // Path: Core2 (family 6, model >= 0x0F)
    // ==================================================================
    "_car_core2:",
    "movl $30f, %esp",
    "jmp _car_send_init_ipi",
    "30:",
    "movl $31f, %esp",
    "jmp _car_clear_fixed_mtrrs",
    "31:",
    "movl $32f, %esp",
    "jmp _car_clear_var_mtrrs",
    "32:",
    // Default type = UC.
    "movl $0x2FF, %ecx",
    "rdmsr",
    "andl $0xFFFFF300, %eax", // clear type + enable bits
    "wrmsr",
    "movl $33f, %esp",
    "jmp _car_physmask_high",
    "33:",
    "movl $34f, %esp",
    "jmp _car_setup_mtrrs",
    "34:",
    "movl $35f, %esp",
    "jmp _car_enable_mtrrs",
    "35:",
    "movl $36f, %esp",
    "jmp _car_try_enable_l2",
    "36:",
    "movl $37f, %esp",
    "jmp _car_enable_cache",
    "37:",
    // Fill CAR by zeroing (fills cache as side effect).
    "movl $38f, %esp",
    "jmp _car_fill_stosl",
    "38:",
    // Disable cache to change MTRRs.
    "movl $39f, %esp",
    "jmp _car_disable_cache",
    "39:",
    // Set ROM MTRR for XIP.
    "movl $40f, %esp",
    "jmp _car_setup_rom_mtrr",
    "40:",
    // Re-enable cache.
    "movl $41f, %esp",
    "jmp _car_enable_cache",
    "41:",
    // Set up stack (inline).
    "movl $_ecar_stack, %esp",
    "andl $0xFFFFFFF0, %esp",
    "jmp _car_done",
    // ==================================================================
    // Path: P3 (family 6, model < 0x0F)
    // ==================================================================
    "_car_p3:",
    "movl $50f, %esp",
    "jmp _car_clear_fixed_mtrrs",
    "50:",
    "movl $51f, %esp",
    "jmp _car_clear_var_mtrrs",
    "51:",
    // Default type = UC.
    "movl $0x2FF, %ecx",
    "rdmsr",
    "andl $0xFFFFF300, %eax",
    "wrmsr",
    "movl $52f, %esp",
    "jmp _car_physmask_high",
    "52:",
    "movl $53f, %esp",
    "jmp _car_setup_mtrrs",
    "53:",
    "movl $54f, %esp",
    "jmp _car_enable_mtrrs",
    "54:",
    "movl $55f, %esp",
    "jmp _car_enable_cache",
    "55:",
    "movl $56f, %esp",
    "jmp _car_fill_stosl",
    "56:",
    "movl $57f, %esp",
    "jmp _car_disable_cache",
    "57:",
    "movl $58f, %esp",
    "jmp _car_setup_rom_mtrr",
    "58:",
    "movl $59f, %esp",
    "jmp _car_enable_cache",
    "59:",
    // Set up stack (inline).
    "movl $_ecar_stack, %esp",
    "andl $0xFFFFFFF0, %esp",
    "jmp _car_done",
    // ==================================================================
    // Path: Netburst / P4 (family 0xF)
    // ==================================================================
    "_car_netburst:",
    // Check if BSP.
    "movl $0x1B, %ecx", // LAPIC_BASE_MSR
    "rdmsr",
    "andl $0x100, %eax", // BSP flag
    "jz _car_nb_ap_halt",
    "movl $60f, %esp",
    "jmp _car_clear_fixed_mtrrs",
    "60:",
    "movl $61f, %esp",
    "jmp _car_clear_var_mtrrs",
    "61:",
    // Default type = UC.
    "movl $0x2FF, %ecx",
    "rdmsr",
    "andl $0xFFFFF300, %eax",
    "wrmsr",
    "movl $62f, %esp",
    "jmp _car_physmask_high",
    "62:",
    // Preload PHYSMASK high + LAPIC enable.
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
    "movl $63f, %esp",
    "jmp _car_send_init_ipi",
    "63:",
    "movl $64f, %esp",
    "jmp _car_setup_mtrrs",
    "64:",
    "movl $65f, %esp",
    "jmp _car_enable_mtrrs",
    "65:",
    // L2 enable — CPUID-gated for family 6 models only.
    "movl %ebx, %eax", // saved CPUID signature
    "andl $0x0F00, %eax",
    "cmpl $0x0600, %eax",
    "jne _car_nb_skip_l2",
    "movl $66f, %esp",
    "jmp _car_try_enable_l2",
    "66:",
    "_car_nb_skip_l2:",
    // Cache ROM for microcode fetch.
    "movl $67f, %esp",
    "jmp _car_setup_rom_mtrr",
    "67:",
    "movl $68f, %esp",
    "jmp _car_enable_cache",
    "68:",
    // Disable cache to change MTRRs safely.
    "movl $69f, %esp",
    "jmp _car_disable_cache",
    "69:",
    // Family F quirk: disable WRPROT MTRR.
    "movl %ebx, %eax",
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
    "movl $70f, %esp",
    "jmp _car_setup_rom_mtrr",
    "70:",
    "_car_nb_fill:",
    "movl $71f, %esp",
    "jmp _car_enable_cache",
    "71:",
    // Fill + zero CAR.
    "movl $72f, %esp",
    "jmp _car_fill_stosl",
    "72:",
    // Set up stack (inline).
    "movl $_ecar_stack, %esp",
    "andl $0xFFFFFFF0, %esp",
    "jmp _car_done",
    // AP halt (Netburst only — APs loop here).
    "_car_nb_ap_halt:",
    "movl %cr0, %eax",
    "andl $0x9FFFFFFF, %eax",
    "movl %eax, %cr0",
    "cli",
    "4: hlt",
    "jmp 4b",
    // ==================================================================
    // Common exit — CAR is live, %esp = stack in CAR.
    // Return to caller via %ebp (set before jmp _car_setup).
    // ==================================================================
    "_car_done:",
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

// _car_setup is a global_asm label, not an extern "C" function.
// It MUST be invoked via jmp (not call) because there is no stack
// before CAR is enabled.  The caller stores a continuation address
// in %ebp and jumps:
//
//     lea post_car, %ebp
//     jmp _car_setup
//   post_car:
//     // CAR is live, %esp = stack, proceed with BSS clear etc.
//
// CAR teardown lives in car_teardown.rs — it runs at the start of the
// *next* stage (ramstage), after the stack has moved to DRAM. It cannot
// run in the same stage that set up CAR because it destroys the cache
// backing the current stack and BSS.
