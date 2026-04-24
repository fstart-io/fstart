//! Generate linker scripts from board memory map.

use std::fmt::Write;

use fstart_types::stage::PageSize;
use fstart_types::{BoardConfig, Platform, RegionKind, SocImageFormat, StageLayout};

/// Generate a linker script for the given board and (optional) stage.
pub fn generate_linker_script(config: &BoardConfig, stage_name: Option<&str>) -> String {
    let mut out = String::new();

    let arch = config.platform.linker_arch();

    // Load address, stack size, and optional data / page-table
    // reservations — pulled from either the monolithic stage or the
    // named multi-stage entry.
    let (load_addr, stack_size, data_addr, page_table_addr, page_size) =
        match (&config.stages, stage_name) {
            (StageLayout::Monolithic(mono), _) => (
                mono.load_addr,
                mono.stack_size as u64,
                mono.data_addr,
                mono.page_table_addr,
                mono.page_size,
            ),
            (StageLayout::MultiStage(stages), Some(name)) => {
                if let Some(stage) = stages.iter().find(|s| s.name.as_str() == name) {
                    (
                        stage.load_addr,
                        stage.stack_size as u64,
                        stage.data_addr,
                        stage.page_table_addr,
                        stage.page_size,
                    )
                } else {
                    (0x8000_0000, 0x10000, None, None, PageSize::default())
                }
            }
            _ => (0x8000_0000, 0x10000, None, None, PageSize::default()),
        };

    // Check if load_addr falls within a ROM region (XIP) or RAM region.
    let rom_region =
        config.memory.regions.iter().find(|r| {
            r.kind == RegionKind::Rom && load_addr >= r.base && load_addr < r.base + r.size
        });

    // Cache-as-RAM landing decision.
    //
    // An XIP stage (load_addr inside a ROM region) automatically uses
    // `memory.car` for writable sections if the board declares it.
    // This captures the x86 pre-DRAM pattern — bootblock / romstage
    // use CAR because DRAM isn't trained yet — without any per-stage
    // flag: the distinction between "writable in RAM" (ARM, RISC-V,
    // post-DRAM x86) and "writable in CAR" (pre-DRAM x86) is fully
    // determined by whether the board declares `memory.car`.
    let car_config = if rom_region.is_some() {
        config.memory.car.as_ref().map(|c| (c.base, c.size))
    } else {
        None
    };

    // Find the appropriate RAM region. For XIP builds (load_addr in ROM),
    // the first RAM region is used for writable sections. For RAM builds
    // (load_addr in RAM), use the RAM region containing load_addr — this
    // matters for multi-stage boards with SRAM + DRAM where different
    // stages run from different RAM regions.
    let ram_region = config
        .memory
        .regions
        .iter()
        .find(|r| r.kind == RegionKind::Ram && load_addr >= r.base && load_addr < r.base + r.size)
        .or_else(|| {
            // load_addr not in any RAM region (XIP) — use first RAM region
            config
                .memory
                .regions
                .iter()
                .find(|r| r.kind == RegionKind::Ram)
        })
        .or_else(|| {
            // No RAM region at all — find any region containing load_addr
            config
                .memory
                .regions
                .iter()
                .find(|r| load_addr >= r.base && load_addr < r.base + r.size)
        });

    // In CAR mode, writable sections land in the CAR region instead
    // of the board's RAM region. This decouples pre-DRAM stages from
    // DRAM being available.
    let (ram_origin, ram_length) = if let Some((car_base, car_size)) = car_config {
        (car_base, car_size)
    } else {
        ram_region
            .map(|r| (r.base, r.size))
            .unwrap_or((0x8000_0000, 0x0800_0000))
    };

    // eGON header is only needed for the first stage (or monolithic). The
    // BROM loads the first-stage binary with the eGON.BT0 header at offset 0.
    // Later stages are loaded by fstart and don't need the header.
    let is_first_stage = match (&config.stages, stage_name) {
        (StageLayout::Monolithic(_), _) => true,
        (StageLayout::MultiStage(stages), Some(name)) => {
            stages.first().is_some_and(|s| s.name.as_str() == name)
        }
        (StageLayout::MultiStage(_), None) => true,
    };
    let needs_egon_header =
        is_first_stage && matches!(config.soc_image_format, SocImageFormat::AllwinnerEgon);

    // x86 CAR/postcar symbols. First stages use `_has_car` to decide
    // whether to enter CAR setup; non-first RAM stages use the same
    // symbol to decide whether to tear CAR down before Rust code runs.
    let has_x86_car = config.platform == Platform::X86_64 && config.memory.car.is_some();
    let (dram_mtrr_base, dram_mtrr_mask) = x86_dram_mtrr(config);

    writeln!(
        out,
        "/* Auto-generated linker script for board: {} */\n",
        config.name
    )
    .unwrap();
    writeln!(out, "OUTPUT_ARCH({arch})").unwrap();

    // Allwinner eGON: the entry point is the eGON header's branch
    // instruction (_head_jump) at offset 0, not the platform _start.
    // Only for the first stage (which has the eGON header).
    if needs_egon_header {
        writeln!(out, "ENTRY(_head_jump)\n").unwrap();
    } else if config.platform == Platform::X86_64 && !is_first_stage {
        // Non-first x86_64 stages use a 64-bit-only entry point.
        // The bootblock already transitioned to long mode; the RAM stage
        // just needs to set up stack, zero BSS, and call fstart_main.
        writeln!(out, "ENTRY(_start_ram)\n").unwrap();
    } else {
        writeln!(out, "ENTRY(_start)\n").unwrap();
    }

    // Boot hart ID for multi-hart parking. On multi-hart SoCs, all harts
    // start executing _start simultaneously. Only the hart matching this
    // value continues; all others enter WFI. Default 0 is correct for
    // single-hart platforms and QEMU virt.
    writeln!(out, "_boot_hart_id = {};", config.boot_hart_id).unwrap();

    if let Some(rom) = rom_region {
        // XIP layout: code in ROM, data/bss/stack in RAM.
        // When data_addr is set, place writable sections at that address
        // instead of ram_origin (e.g., to avoid QEMU's DTB at RAM base).
        generate_xip_layout(
            &mut out,
            rom.base,
            rom.size,
            ram_origin,
            ram_length,
            stack_size,
            data_addr,
            page_table_addr,
            needs_egon_header,
            is_first_stage,
            config.platform,
            page_size,
            car_config,
            dram_mtrr_base,
            dram_mtrr_mask,
        );
    } else {
        // RAM-only layout: everything in RAM at load_addr.
        //
        // Use load_addr as the linker origin so the entry point is
        // placed at the correct address. Available length extends to
        // the end of the containing RAM region.
        let region_end = ram_origin + ram_length;
        let effective_origin = load_addr;
        let effective_length = region_end - effective_origin;

        // For multi-stage bootblocks that share their load address with the
        // FFS image (flash_base == load_addr), BSS and stack must be placed
        // beyond the image area. Otherwise the entry-point BSS clearing and
        // stack writes would corrupt the manifest and other stages.
        let bss_origin = match (config.memory.flash_base, config.memory.flash_size) {
            (Some(fb), Some(fs)) if fb == load_addr && fs > 0 => {
                let bss_addr = fb + fs;
                if bss_addr < effective_origin || bss_addr >= effective_origin + effective_length {
                    panic!(
                        "flash_base ({fb:#x}) + flash_size ({fs:#x}) = {bss_addr:#x} \
                         falls outside RAM region [{effective_origin:#x}..{:#x}]",
                        effective_origin + effective_length
                    );
                }
                Some(bss_addr)
            }
            _ => None,
        };
        generate_ram_layout(
            &mut out,
            effective_origin,
            effective_length,
            stack_size,
            bss_origin,
            needs_egon_header,
            config.platform,
            is_first_stage,
            has_x86_car,
            dram_mtrr_base,
            dram_mtrr_mask,
        );
    }

    out
}

/// Compute a conservative x86 WB DRAM MTRR covering the configured low RAM.
///
/// Pineview ramstage follows coreboot's postcar model: the CAR MTRR is
/// repurposed as a normal DRAM write-back MTRR before high-level code runs.
/// Board RON only carries the upper-bound DRAM region, so use a
/// power-of-two range from 0 to the top of the first RAM bank; this matches
/// the classic low-DRAM WB MTRR shape used before the runtime memory map is
/// fully refined.
fn x86_dram_mtrr(config: &BoardConfig) -> (u64, u64) {
    let top = config
        .memory
        .regions
        .iter()
        .filter(|r| r.kind == RegionKind::Ram)
        .map(|r| r.base.saturating_add(r.size))
        .max()
        .unwrap_or(0x4000_0000);
    let size = top.next_power_of_two().max(0x0010_0000);
    (0, !(size - 1) & 0xFFFF_FFFF)
}

fn write_x86_car_symbols(
    out: &mut String,
    platform: Platform,
    has_x86_car: bool,
    dram_mtrr_base: u64,
    dram_mtrr_mask: u64,
) {
    if platform != Platform::X86_64 {
        return;
    }
    writeln!(out).unwrap();
    writeln!(out, "    /* x86 CAR/postcar symbols */").unwrap();
    writeln!(out, "    _has_car = {};", if has_x86_car { 1 } else { 0 }).unwrap();
    // The platform crate always links the CAR setup object. Non-first
    // RAM stages never execute `_car_setup`, but the labels it references
    // still need absolute definitions so the object can link.
    writeln!(out, "    _car_base = 0;").unwrap();
    writeln!(out, "    _car_size = 0;").unwrap();
    writeln!(out, "    _ecar_stack = _stack_top;").unwrap();
    writeln!(out, "    _rom_mtrr_base = 0;").unwrap();
    writeln!(out, "    _rom_mtrr_mask = 0;").unwrap();
    writeln!(out, "    _dram_mtrr_base = {dram_mtrr_base:#x};").unwrap();
    writeln!(out, "    _dram_mtrr_mask = {dram_mtrr_mask:#x};").unwrap();
}

/// Generate linker script for XIP from ROM with data/bss/stack in RAM.
///
/// When `data_addr` is `Some(addr)`, writable sections (`.data`, `.bss`,
/// stack) are placed at `addr` instead of the start of the RAM region.
/// This is used on AArch64 QEMU where the platform places the DTB at the
/// base of RAM (0x40000000) — BSS clearing would destroy it if placed there.
#[allow(clippy::too_many_arguments)]
fn generate_xip_layout(
    out: &mut String,
    rom_origin: u64,
    rom_length: u64,
    ram_origin: u64,
    ram_length: u64,
    stack_size: u64,
    data_addr: Option<u64>,
    page_table_addr: Option<(u64, u64)>,
    needs_egon_header: bool,
    is_first_stage: bool,
    platform: Platform,
    page_size: PageSize,
    car_config: Option<(u64, u64)>,
    dram_mtrr_base: u64,
    dram_mtrr_mask: u64,
) {
    // When data_addr is set, split RAM into two memory regions:
    // RAMRO for read-only data (unused currently, but reserved),
    // and RAMRW for writable sections starting at data_addr.
    let rw_origin = data_addr.unwrap_or(ram_origin);
    let rw_length = ram_length - (rw_origin - ram_origin);

    writeln!(out, "MEMORY\n{{").unwrap();
    writeln!(
        out,
        "    ROM (rx)  : ORIGIN = {rom_origin:#x}, LENGTH = {rom_length:#x}"
    )
    .unwrap();
    if let Some((pt_origin, pt_length)) = page_table_addr {
        // Page tables and IDT live in a separate LOW region below the
        // main RAM. This prevents the linker from merging them into
        // the BSS PT_LOAD segment (which would produce a wrapped MemSiz).
        //
        // On QEMU x86_64 this is conventional memory at 0x1000-0x8000.
        // On real Intel/AMD platforms with proper flash mapping, page
        // tables can live in ROM — set page_table_addr to None and the
        // .pagetables section goes in the normal RAM region.
        writeln!(
            out,
            "    LOW (rwx) : ORIGIN = {pt_origin:#x}, LENGTH = {pt_length:#x}"
        )
        .unwrap();
    }
    writeln!(
        out,
        "    RAM (rwx) : ORIGIN = {rw_origin:#x}, LENGTH = {rw_length:#x}"
    )
    .unwrap();
    writeln!(out, "}}\n").unwrap();

    writeln!(out, "SECTIONS\n{{").unwrap();

    // Allwinner eGON header — placed before code, contains a branch
    // instruction at offset 0 that jumps over the header to _start.
    // KEEP() ensures --gc-sections doesn't strip these; the raw
    // machine-code branch from .head.text to .text.entry is opaque
    // to the linker.
    if needs_egon_header {
        writeln!(out, "    .head : {{").unwrap();
        writeln!(out, "        KEEP(*(.head.text))").unwrap();
        writeln!(out, "        KEEP(*(.head.egon))").unwrap();
        writeln!(out, "    }} > ROM\n").unwrap();
    }

    // Code in ROM.
    writeln!(out, "    .text : {{").unwrap();
    writeln!(out, "        KEEP(*(.text.entry))").unwrap();
    writeln!(out, "        *(.text .text.*)").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    // FFS anchor block (embedded in bootblock, 8-byte aligned for scanning)
    writeln!(out, "    .fstart.anchor : ALIGN(16) {{").unwrap();
    writeln!(out, "        *(.fstart.anchor)").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    // Read-only data in ROM.
    // .lrodata: large code model (x86_64) puts read-only data in .lrodata.
    writeln!(out, "    .rodata : ALIGN(16) {{").unwrap();
    writeln!(out, "        *(.rodata .rodata.* .lrodata .lrodata.*)").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    // Initialized data: stored in ROM (AT > ROM), copied to RAM at startup.
    // _data_load is the ROM address of the initializers (load-memory address).
    // _data_start / _data_end are the RAM addresses (virtual-memory addresses).
    // The _start assembly copies [_data_load .. _data_load + size) to
    // [_data_start .. _data_end) before entering Rust code.
    // .ldata: large code model (x86_64) puts initialized data in .ldata.
    writeln!(out, "    .data : ALIGN(16) {{").unwrap();
    writeln!(out, "        _data_start = .;").unwrap();
    writeln!(out, "        *(.data .data.* .ldata .ldata.*)").unwrap();
    writeln!(out, "        _data_end = .;").unwrap();
    writeln!(out, "    }} > RAM AT > ROM").unwrap();
    writeln!(out, "    _data_load = LOADADDR(.data);\n").unwrap();

    // BSS in RAM.
    // .lbss: large code model (x86_64) puts uninitialized data in .lbss.
    writeln!(out, "    .bss (NOLOAD) : ALIGN(16) {{").unwrap();
    writeln!(out, "        _bss_start = .;").unwrap();
    writeln!(out, "        *(.bss .bss.* .lbss .lbss.*)").unwrap();
    writeln!(out, "        *(COMMON)").unwrap();
    writeln!(out, "        _bss_end = .;").unwrap();
    writeln!(out, "    }} > RAM\n").unwrap();

    // Page tables: isolated from BSS to prevent corruption.
    write_page_tables_section(
        out,
        "RAM",
        platform,
        page_table_addr.is_some(),
        page_size,
        is_first_stage,
    );

    // Stack: grows downward from top of RAM region.
    write_stack(out, stack_size, "RAM");

    // CAR symbols — consumed by car.rs global_asm.
    // Only emitted when the board declares memory.car.
    if let Some((car_base, car_size)) = car_config {
        writeln!(out).unwrap();
        writeln!(out, "    /* Cache-as-RAM symbols for car.rs */").unwrap();
        writeln!(out, "    _car_base = {car_base:#x};").unwrap();
        writeln!(out, "    _car_size = {car_size:#x};").unwrap();
        // Stack top inside CAR (same as _stack_top for CAR boards).
        writeln!(out, "    _ecar_stack = _stack_top;").unwrap();
        // ROM MTRR: cover the entire ROM region as write-protect.
        writeln!(out, "    _rom_mtrr_base = {rom_origin:#x};").unwrap();
        // MTRR mask for ROM size (power-of-2 size → negate for mask).
        let rom_mask = !(rom_length - 1) & 0xFFFF_FFFF;
        writeln!(out, "    _rom_mtrr_mask = {rom_mask:#x};").unwrap();
        // Flag consumed by entry asm to decide whether to jmp _car_setup.
        writeln!(out, "    _has_car = 1;").unwrap();
        writeln!(out, "    _dram_mtrr_base = {dram_mtrr_base:#x};").unwrap();
        writeln!(out, "    _dram_mtrr_mask = {dram_mtrr_mask:#x};").unwrap();
    } else {
        writeln!(out, "    _has_car = 0;").unwrap();
        writeln!(out, "    _dram_mtrr_base = {dram_mtrr_base:#x};").unwrap();
        writeln!(out, "    _dram_mtrr_mask = {dram_mtrr_mask:#x};").unwrap();
    }

    // x86 bootblock entry code: only the first stage (bootblock) has the
    // 16-bit reset vector and mode transition code. Later stages in a
    // multi-stage build start in 64-bit long mode (jumped to by the
    // bootblock or previous stage) and don't need .x86boot or .reset.
    //
    // The CPU starts at 0xFFFFFFF0 (reset vector). The .x86boot section
    // (16-bit GDT load, mode transitions) must be within 64KB of the
    // reset vector (CS base = 0xFFFF0000 at reset).
    if platform == Platform::X86_64 && is_first_stage {
        let boot_block_addr = rom_origin + rom_length - 0x1000; // last 4K
        let reset_addr = rom_origin + rom_length - 16;
        writeln!(out).unwrap();
        writeln!(
            out,
            "    /* x86: 16-bit/32-bit/64-bit entry code + reset vector */"
        )
        .unwrap();
        writeln!(
            out,
            "    .x86boot {boot_block_addr:#x} : AT({boot_block_addr:#x}) {{"
        )
        .unwrap();
        writeln!(out, "        KEEP(*(.x86boot))").unwrap();
        writeln!(out, "    }} > ROM").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "    .reset {reset_addr:#x} : AT({reset_addr:#x}) {{").unwrap();
        writeln!(out, "        KEEP(*(.reset))").unwrap();
        writeln!(out, "    }} > ROM").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "    _binary_end = .;").unwrap();
    }

    writeln!(out, "}}").unwrap();
}

/// Generate linker script with everything in RAM (load_addr is in a RAM region).
///
/// `bss_origin` optionally specifies a fixed starting address for BSS and stack.
/// When the bootblock shares its address space with the FFS image, BSS/stack
/// must be placed after the entire image area to avoid corruption.
#[allow(clippy::too_many_arguments)]
fn generate_ram_layout(
    out: &mut String,
    ram_origin: u64,
    ram_length: u64,
    stack_size: u64,
    bss_origin: Option<u64>,
    needs_egon_header: bool,
    platform: Platform,
    is_first_stage: bool,
    has_x86_car: bool,
    dram_mtrr_base: u64,
    dram_mtrr_mask: u64,
) {
    if let Some(bss_addr) = bss_origin {
        let code_length = bss_addr - ram_origin;
        let rw_length = ram_length - code_length;

        writeln!(out, "MEMORY\n{{").unwrap();
        writeln!(
            out,
            "    CODE  (rwx) : ORIGIN = {ram_origin:#x}, LENGTH = {code_length:#x}"
        )
        .unwrap();
        writeln!(
            out,
            "    RWDATA (rwx) : ORIGIN = {bss_addr:#x}, LENGTH = {rw_length:#x}"
        )
        .unwrap();
        writeln!(out, "}}\n").unwrap();

        writeln!(out, "SECTIONS\n{{").unwrap();
        if needs_egon_header {
            write_allwinner_egon_section(out, "CODE");
        }
        write_text_section(out, "CODE");
        write_anchor_section(out, "CODE");
        write_rodata_section(out, "CODE");
        write_data_section(out, "CODE");
        write_bss_section(out, "RWDATA");
        write_page_tables_section(
            out,
            "RWDATA",
            platform,
            false,
            PageSize::default(),
            is_first_stage,
        );
        write_stack(out, stack_size, "RWDATA");
        write_x86_car_symbols(out, platform, has_x86_car, dram_mtrr_base, dram_mtrr_mask);
        writeln!(out, "}}").unwrap();
    } else {
        writeln!(out, "MEMORY\n{{").unwrap();
        writeln!(
            out,
            "    RAM (rwx) : ORIGIN = {ram_origin:#x}, LENGTH = {ram_length:#x}"
        )
        .unwrap();
        writeln!(out, "}}\n").unwrap();

        writeln!(out, "SECTIONS\n{{").unwrap();
        if needs_egon_header {
            write_allwinner_egon_section(out, "RAM");
        }
        write_text_section(out, "RAM");
        write_anchor_section(out, "RAM");
        write_rodata_section(out, "RAM");
        write_data_section(out, "RAM");
        write_bss_section(out, "RAM");
        write_page_tables_section(
            out,
            "RAM",
            platform,
            false,
            PageSize::default(),
            is_first_stage,
        );
        write_stack(out, stack_size, "RAM");
        write_x86_car_symbols(out, platform, has_x86_car, dram_mtrr_base, dram_mtrr_mask);
        writeln!(out, "}}").unwrap();
    }
}

// Shared section helpers to avoid duplicating the identical section
// definitions between the split-RAM and single-RAM layout branches.

fn write_text_section(out: &mut String, region: &str) {
    // .ltext: large code model (x86_64) puts function bodies in .ltext
    // sections — capture them alongside normal .text so all executable
    // code is contiguous and _text_start/_text_end span everything.
    writeln!(out, "    .text : {{").unwrap();
    writeln!(out, "        _text_start = .;").unwrap();
    writeln!(out, "        KEEP(*(.text.entry))").unwrap();
    writeln!(out, "        *(.text .text.* .ltext .ltext.*)").unwrap();
    writeln!(out, "        _text_end = .;").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_anchor_section(out: &mut String, region: &str) {
    writeln!(out, "    .fstart.anchor : ALIGN(8) {{").unwrap();
    writeln!(out, "        *(.fstart.anchor)").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_rodata_section(out: &mut String, region: &str) {
    // .lrodata: large code model (x86_64) puts read-only data in separate
    // .lrodata sections — capture them here alongside normal .rodata.
    writeln!(out, "    .rodata : ALIGN(8) {{").unwrap();
    writeln!(out, "        _rodata_start = .;").unwrap();
    writeln!(out, "        *(.rodata .rodata.* .lrodata .lrodata.*)").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_data_section(out: &mut String, region: &str) {
    // .ldata: large code model (x86_64) puts initialized data in .ldata
    // sections — capture them with .data so LMA is set correctly via
    // AT > ROM on XIP layouts.
    writeln!(out, "    .data : ALIGN(8) {{").unwrap();
    writeln!(out, "        _data_start = .;").unwrap();
    writeln!(out, "        *(.data .data.* .ldata .ldata.*)").unwrap();
    writeln!(out, "        _data_end = .;").unwrap();
    writeln!(out, "    }} > {region}").unwrap();
    // For RAM-only layouts, _data_load == _data_start (no ROM-to-RAM copy
    // needed). The _start assembly's copy loop will skip when src == dst.
    writeln!(out, "    _data_load = LOADADDR(.data);\n").unwrap();
}

fn write_bss_section(out: &mut String, region: &str) {
    // .lbss: large code model (x86_64) puts uninitialized data in .lbss
    // sections — capture them with .bss so they are NOLOAD.
    writeln!(out, "    .bss (NOLOAD) : ALIGN(8) {{").unwrap();
    writeln!(out, "        _bss_start = .;").unwrap();
    writeln!(out, "        *(.bss .bss.* .lbss .lbss.*)").unwrap();
    writeln!(out, "        *(COMMON)").unwrap();
    writeln!(out, "        _bss_end = .;").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

/// Allwinner eGON .head section: branch instruction + eGON.BT0 struct.
///
/// `KEEP()` ensures these sections survive `--gc-sections` even when the
/// entry point symbol (`_head_jump`) is the only reference — the raw
/// machine-code branch from `.head.text` to `.text.entry` is opaque to
/// the linker and wouldn't count as a reference without `KEEP`.
fn write_allwinner_egon_section(out: &mut String, region: &str) {
    writeln!(out, "    .head : {{").unwrap();
    writeln!(out, "        KEEP(*(.head.text))").unwrap();
    writeln!(out, "        KEEP(*(.head.egon))").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

/// MMU page tables: isolated from BSS in a dedicated section.
///
/// Space reservation rules for x86_64:
///
/// - **First stage** (bootblock or monolithic): the 32-bit→64-bit entry
///   code builds PML4 / PDPT / PDTs at `[_page_tables_start, _page_tables_end)`.
///   The linker must reserve real bytes sized according to [`PageSize`],
///   otherwise CR3 ends up pointing at collapsed memory and the mode
///   switch faults silently.
/// - **Non-first x86_64 stages** (e.g., ramstage after bootblock): no
///   page-table construction — `_start_ram` inherits the tables already
///   programmed by the bootblock. Zero-byte reservation is fine, and
///   `page_size` has no effect.
///
/// AArch64 entry code stores page table descriptors in static arrays
/// placed in `.page_tables` via attribute — the linker only needs the
/// symbol bookends there.
#[allow(clippy::too_many_arguments)]
fn write_page_tables_section(
    out: &mut String,
    region: &str,
    platform: Platform,
    has_low_region: bool,
    page_size: PageSize,
    is_first_stage: bool,
) {
    // Size depends on page size (x86_64 only):
    //   1 GiB pages: 2 pages (PML4 + PDPT), 512 GiB coverage
    //   2 MiB pages: 6 pages (PML4 + PDPT + 4xPD), 4 GiB coverage
    let (pt_size, pt_comment) = match page_size {
        PageSize::Size1GiB => (0x2000u64, "2 pages: PML4 + PDPT (1 GiB pages, 512 GiB)"),
        PageSize::Size2MiB => (
            0x6000u64,
            "6 pages: PML4 + PDPT + 4xPD (2 MiB pages, 4 GiB)",
        ),
    };

    if has_low_region {
        // Page tables and IDT placed in the LOW region (separate from
        // BSS/stack). Used on platforms where page tables must live at
        // specific addresses (e.g., QEMU x86_64 conventional memory).
        writeln!(out, "    .page_tables (NOLOAD) : {{").unwrap();
        writeln!(out, "        _page_tables_start = .;").unwrap();
        writeln!(out, "        . += {pt_size:#x};  /* {pt_comment} */").unwrap();
        writeln!(out, "        _page_tables_end = .;").unwrap();
        writeln!(out, "    }} > LOW\n").unwrap();
        writeln!(out, "    .idt_table (NOLOAD) : {{").unwrap();
        writeln!(out, "        *(.idt_table)").unwrap();
        writeln!(out, "    }} > LOW\n").unwrap();
    } else {
        // Non-LOW path: reserve real space on x86_64 first stage (the
        // entry asm builds tables here). Other stages / platforms get
        // a zero-size placeholder so `_page_tables_{start,end}` symbols
        // exist for consistency.
        let needs_reservation = platform == Platform::X86_64 && is_first_stage;
        writeln!(out, "    .page_tables (NOLOAD) : ALIGN(4096) {{").unwrap();
        writeln!(out, "        _page_tables_start = .;").unwrap();
        if needs_reservation {
            writeln!(out, "        . += {pt_size:#x};  /* {pt_comment} */").unwrap();
        }
        writeln!(out, "        *(.page_tables .page_tables.*)").unwrap();
        writeln!(out, "        _page_tables_end = .;").unwrap();
        writeln!(out, "    }} > {region}\n").unwrap();

        // x86_64 RAM stages inherit page tables from the bootblock but
        // still define their own IDT. Place it in the main RAM region
        // alongside BSS (no LOW region for RAM stages).
        if platform == Platform::X86_64 {
            writeln!(out, "    .idt_table (NOLOAD) : ALIGN(4096) {{").unwrap();
            writeln!(out, "        *(.idt_table)").unwrap();
            writeln!(out, "    }} > {region}\n").unwrap();
        }
    }
}

fn write_stack(out: &mut String, stack_size: u64, region: &str) {
    // Stack grows downward from the top of the memory region.
    // The ASSERT verifies there is at least stack_size bytes between
    // the end of the last NOLOAD section and the top of the region.
    // _page_tables_end is always defined (the section may be empty on
    // architectures without MMU page table statics).
    writeln!(out, "    _stack_top = ORIGIN({region}) + LENGTH({region});").unwrap();
    writeln!(
        out,
        "    ASSERT(_stack_top - _page_tables_end >= {stack_size:#x}, \"insufficient stack space\")"
    )
    .unwrap();

    // _binary_end marks the end of all loadable content. Used by entry
    // stubs that need to copy the entire binary (e.g., SBSA flash→DRAM).
    // For XIP layouts this is the end of .rodata in ROM; for RAM layouts
    // it's the end of .data (BSS is not stored).
    writeln!(out, "    _binary_end = _data_end;").unwrap();
}
