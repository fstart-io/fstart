//! Generate linker scripts from board memory map.

use std::fmt::Write;

use fstart_types::{BoardConfig, RegionKind, StageLayout};

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

    let ram_region = config
        .memory
        .regions
        .iter()
        .find(|r| r.kind == RegionKind::Ram)
        .or_else(|| {
            // If no RAM region, find the region containing load_addr
            config
                .memory
                .regions
                .iter()
                .find(|r| load_addr >= r.base && load_addr < r.base + r.size)
        });

    let (ram_origin, ram_length) = ram_region
        .map(|r| (r.base, r.size))
        .unwrap_or((0x8000_0000, 0x0800_0000));

    writeln!(
        out,
        "/* Auto-generated linker script for board: {} */\n",
        config.name
    )
    .unwrap();
    writeln!(out, "OUTPUT_ARCH({arch})").unwrap();
    writeln!(out, "ENTRY(_start)\n").unwrap();

    if let Some(rom) = rom_region {
        // XIP layout: code in ROM, data/bss/stack in RAM.
        // When data_addr is set, place writable sections at that address
        // instead of ram_origin (e.g., to avoid QEMU's DTB at RAM base).
        generate_xip_layout(
            &mut out, rom.base, rom.size, ram_origin, ram_length, stack_size, data_addr,
        );
    } else {
        // RAM-only layout: everything in RAM at load_addr.
        //
        // For multi-stage bootblocks that share their load address with the
        // FFS image (flash_base == load_addr), BSS and stack must be placed
        // beyond the image area. Otherwise the entry-point BSS clearing and
        // stack writes would corrupt the manifest and other stages.
        let bss_origin = match (config.memory.flash_base, config.memory.flash_size) {
            (Some(fb), Some(fs)) if fb == load_addr && fs > 0 => {
                let bss_addr = fb + fs;
                if bss_addr < ram_origin || bss_addr >= ram_origin + ram_length {
                    panic!(
                        "flash_base ({fb:#x}) + flash_size ({fs:#x}) = {bss_addr:#x} \
                         falls outside RAM region [{ram_origin:#x}..{:#x}]",
                        ram_origin + ram_length
                    );
                }
                Some(bss_addr)
            }
            _ => None,
        };
        generate_ram_layout(&mut out, ram_origin, ram_length, stack_size, bss_origin);
    }

    out
}

/// Generate linker script for XIP from ROM with data/bss/stack in RAM.
///
/// When `data_addr` is `Some(addr)`, writable sections (`.data`, `.bss`,
/// stack) are placed at `addr` instead of the start of the RAM region.
/// This is used on AArch64 QEMU where the platform places the DTB at the
/// base of RAM (0x40000000) — BSS clearing would destroy it if placed there.
fn generate_xip_layout(
    out: &mut String,
    rom_origin: u64,
    rom_length: u64,
    ram_origin: u64,
    ram_length: u64,
    stack_size: u64,
    data_addr: Option<u64>,
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

    // Code in ROM
    writeln!(out, "    .text : {{").unwrap();
    writeln!(out, "        *(.text.entry)").unwrap();
    writeln!(out, "        *(.text .text.*)").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    // FFS anchor block (embedded in bootblock, 8-byte aligned for scanning)
    writeln!(out, "    .fstart.anchor : ALIGN(8) {{").unwrap();
    writeln!(out, "        *(.fstart.anchor)").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    // Read-only data in ROM
    writeln!(out, "    .rodata : ALIGN(8) {{").unwrap();
    writeln!(out, "        *(.rodata .rodata.*)").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    // Initialized data: stored in ROM (AT > ROM), copied to RAM at startup.
    // _data_load is the ROM address of the initializers (load-memory address).
    // _data_start / _data_end are the RAM addresses (virtual-memory addresses).
    // The _start assembly copies [_data_load .. _data_load + size) to
    // [_data_start .. _data_end) before entering Rust code.
    writeln!(out, "    .data : ALIGN(8) {{").unwrap();
    writeln!(out, "        _data_start = .;").unwrap();
    writeln!(out, "        *(.data .data.*)").unwrap();
    writeln!(out, "        _data_end = .;").unwrap();
    writeln!(out, "    }} > RAM AT > ROM").unwrap();
    writeln!(out, "    _data_load = LOADADDR(.data);\n").unwrap();

    // BSS in RAM
    writeln!(out, "    .bss (NOLOAD) : ALIGN(8) {{").unwrap();
    writeln!(out, "        _bss_start = .;").unwrap();
    writeln!(out, "        *(.bss .bss.*)").unwrap();
    writeln!(out, "        *(COMMON)").unwrap();
    writeln!(out, "        _bss_end = .;").unwrap();
    writeln!(out, "    }} > RAM\n").unwrap();

    // Stack in RAM
    writeln!(out, "    . = ALIGN(16);").unwrap();
    writeln!(out, "    . = . + {stack_size:#x};").unwrap();
    writeln!(out, "    _stack_top = .;").unwrap();
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
        write_text_section(out, "CODE");
        write_anchor_section(out, "CODE");
        write_rodata_section(out, "CODE");
        write_data_section(out, "CODE");
        write_bss_section(out, "RWDATA");
        write_stack(out, stack_size);
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
        write_text_section(out, "RAM");
        write_anchor_section(out, "RAM");
        write_rodata_section(out, "RAM");
        write_data_section(out, "RAM");
        write_bss_section(out, "RAM");
        write_stack(out, stack_size);
        writeln!(out, "}}").unwrap();
    }
}

// Shared section helpers to avoid duplicating the identical section
// definitions between the split-RAM and single-RAM layout branches.

fn write_text_section(out: &mut String, region: &str) {
    writeln!(out, "    .text : {{").unwrap();
    writeln!(out, "        *(.text.entry)").unwrap();
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

fn write_stack(out: &mut String, stack_size: u64) {
    writeln!(out, "    . = ALIGN(16);").unwrap();
    writeln!(out, "    . = . + {stack_size:#x};").unwrap();
    writeln!(out, "    _stack_top = .;").unwrap();
}
