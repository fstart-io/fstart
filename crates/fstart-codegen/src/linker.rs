//! Generate linker scripts from board memory map.

use std::fmt::Write;

use fstart_types::{BoardConfig, RegionKind, SocImageFormat, StageLayout};

/// Generate a linker script for the given board and (optional) stage.
pub fn generate_linker_script(config: &BoardConfig, stage_name: Option<&str>) -> String {
    let mut out = String::new();

    let arch = match config.platform.as_str() {
        "riscv64" => "riscv",
        "aarch64" => "aarch64",
        "armv7" => "arm",
        other => other,
    };

    // Determine load address, stack size, and optional data address from stage config
    let (load_addr, stack_size, data_addr) = match (&config.stages, stage_name) {
        (StageLayout::Monolithic(mono), _) => {
            (mono.load_addr, mono.stack_size as u64, mono.data_addr)
        }
        (StageLayout::MultiStage(stages), Some(name)) => {
            if let Some(stage) = stages.iter().find(|s| s.name.as_str() == name) {
                (stage.load_addr, stage.stack_size as u64, stage.data_addr)
            } else {
                (0x8000_0000, 0x10000, None) // fallback
            }
        }
        _ => (0x8000_0000, 0x10000, None),
    };

    // Check if load_addr falls within a ROM region (XIP) or RAM region
    let rom_region =
        config.memory.regions.iter().find(|r| {
            r.kind == RegionKind::Rom && load_addr >= r.base && load_addr < r.base + r.size
        });

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

    let (ram_origin, ram_length) = ram_region
        .map(|r| (r.base, r.size))
        .unwrap_or((0x8000_0000, 0x0800_0000));

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
    } else {
        writeln!(out, "ENTRY(_start)\n").unwrap();
    }

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
            needs_egon_header,
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
        );
    }

    out
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
    needs_egon_header: bool,
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

    // Code in ROM
    writeln!(out, "    .text : {{").unwrap();
    writeln!(out, "        KEEP(*(.text.entry))").unwrap();
    writeln!(out, "        *(.text .text.*)").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    // FFS anchor block (embedded in bootblock, 8-byte aligned for scanning)
    writeln!(out, "    .fstart.anchor : ALIGN(16) {{").unwrap();
    writeln!(out, "        *(.fstart.anchor)").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    // Read-only data in ROM
    writeln!(out, "    .rodata : ALIGN(16) {{").unwrap();
    writeln!(out, "        *(.rodata .rodata.*)").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    // Initialized data: stored in ROM (AT > ROM), copied to RAM at startup.
    // _data_load is the ROM address of the initializers (load-memory address).
    // _data_start / _data_end are the RAM addresses (virtual-memory addresses).
    // The _start assembly copies [_data_load .. _data_load + size) to
    // [_data_start .. _data_end) before entering Rust code.
    writeln!(out, "    .data : ALIGN(16) {{").unwrap();
    writeln!(out, "        _data_start = .;").unwrap();
    writeln!(out, "        *(.data .data.*)").unwrap();
    writeln!(out, "        _data_end = .;").unwrap();
    writeln!(out, "    }} > RAM AT > ROM").unwrap();
    writeln!(out, "    _data_load = LOADADDR(.data);\n").unwrap();

    // BSS in RAM
    writeln!(out, "    .bss (NOLOAD) : ALIGN(16) {{").unwrap();
    writeln!(out, "        _bss_start = .;").unwrap();
    writeln!(out, "        *(.bss .bss.*)").unwrap();
    writeln!(out, "        *(COMMON)").unwrap();
    writeln!(out, "        _bss_end = .;").unwrap();
    writeln!(out, "    }} > RAM\n").unwrap();

    // Stack: grows downward from top of RAM region.
    write_stack(out, stack_size, "RAM");
    writeln!(out, "}}").unwrap();
}

/// Generate linker script with everything in RAM (load_addr is in a RAM region).
///
/// `bss_origin` optionally specifies a fixed starting address for BSS and stack.
/// When the bootblock shares its address space with the FFS image, BSS/stack
/// must be placed after the entire image area to avoid corruption.
fn generate_ram_layout(
    out: &mut String,
    ram_origin: u64,
    ram_length: u64,
    stack_size: u64,
    bss_origin: Option<u64>,
    needs_egon_header: bool,
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
        write_stack(out, stack_size, "RWDATA");
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
        write_stack(out, stack_size, "RAM");
        writeln!(out, "}}").unwrap();
    }
}

// Shared section helpers to avoid duplicating the identical section
// definitions between the split-RAM and single-RAM layout branches.

fn write_text_section(out: &mut String, region: &str) {
    writeln!(out, "    .text : {{").unwrap();
    writeln!(out, "        KEEP(*(.text.entry))").unwrap();
    writeln!(out, "        *(.text .text.*)").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_anchor_section(out: &mut String, region: &str) {
    writeln!(out, "    .fstart.anchor : ALIGN(8) {{").unwrap();
    writeln!(out, "        *(.fstart.anchor)").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_rodata_section(out: &mut String, region: &str) {
    writeln!(out, "    .rodata : ALIGN(8) {{").unwrap();
    writeln!(out, "        *(.rodata .rodata.*)").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_data_section(out: &mut String, region: &str) {
    writeln!(out, "    .data : ALIGN(8) {{").unwrap();
    writeln!(out, "        _data_start = .;").unwrap();
    writeln!(out, "        *(.data .data.*)").unwrap();
    writeln!(out, "        _data_end = .;").unwrap();
    writeln!(out, "    }} > {region}").unwrap();
    // For RAM-only layouts, _data_load == _data_start (no ROM-to-RAM copy
    // needed). The _start assembly's copy loop will skip when src == dst.
    writeln!(out, "    _data_load = LOADADDR(.data);\n").unwrap();
}

fn write_bss_section(out: &mut String, region: &str) {
    writeln!(out, "    .bss (NOLOAD) : ALIGN(8) {{").unwrap();
    writeln!(out, "        _bss_start = .;").unwrap();
    writeln!(out, "        *(.bss .bss.*)").unwrap();
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

fn write_stack(out: &mut String, stack_size: u64, region: &str) {
    // Stack grows downward from the top of the memory region.
    // The ASSERT verifies there is at least stack_size bytes between
    // the end of BSS and the top of the region.
    writeln!(out, "    _stack_top = ORIGIN({region}) + LENGTH({region});").unwrap();
    writeln!(
        out,
        "    ASSERT(_stack_top - _bss_end >= {stack_size:#x}, \"insufficient stack space\")"
    )
    .unwrap();
}
