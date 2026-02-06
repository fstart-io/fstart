//! Generate linker scripts from board memory map.

use fstart_types::{BoardConfig, RegionKind, StageLayout};

/// Generate a linker script for the given board and (optional) stage.
pub fn generate_linker_script(config: &BoardConfig, stage_name: Option<&str>) -> String {
    let mut out = String::new();

    let arch = match config.platform.as_str() {
        "riscv64" => "riscv",
        "aarch64" => "aarch64",
        other => other,
    };

    // Determine load address and stack size from stage config
    let (load_addr, stack_size) = match (&config.stages, stage_name) {
        (StageLayout::Monolithic(mono), _) => (mono.load_addr, mono.stack_size as u64),
        (StageLayout::MultiStage(stages), Some(name)) => {
            if let Some(stage) = stages.iter().find(|s| s.name.as_str() == name) {
                (stage.load_addr, stage.stack_size as u64)
            } else {
                (0x8000_0000, 0x10000) // fallback
            }
        }
        _ => (0x8000_0000, 0x10000),
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

    out.push_str(&format!(
        "/* Auto-generated linker script for board: {} */\n\n",
        config.name
    ));
    out.push_str(&format!("OUTPUT_ARCH({arch})\n"));
    out.push_str("ENTRY(_start)\n\n");

    if let Some(rom) = rom_region {
        // XIP layout: code in ROM, data/bss/stack in RAM
        generate_xip_layout(
            &mut out, rom.base, rom.size, ram_origin, ram_length, stack_size,
        );
    } else {
        // RAM-only layout: everything in RAM at load_addr
        generate_ram_layout(&mut out, ram_origin, ram_length, stack_size);
    }

    out
}

/// Generate linker script for XIP from ROM with data/bss/stack in RAM.
fn generate_xip_layout(
    out: &mut String,
    rom_origin: u64,
    rom_length: u64,
    ram_origin: u64,
    ram_length: u64,
    stack_size: u64,
) {
    out.push_str("MEMORY\n{\n");
    out.push_str(&format!(
        "    ROM (rx)  : ORIGIN = {rom_origin:#x}, LENGTH = {rom_length:#x}\n"
    ));
    out.push_str(&format!(
        "    RAM (rwx) : ORIGIN = {ram_origin:#x}, LENGTH = {ram_length:#x}\n"
    ));
    out.push_str("}\n\n");

    out.push_str("SECTIONS\n{\n");

    // Code in ROM
    out.push_str("    .text : {\n");
    out.push_str("        *(.text.entry)\n");
    out.push_str("        *(.text .text.*)\n");
    out.push_str("    } > ROM\n\n");

    // FFS anchor block (embedded in bootblock, 8-byte aligned for scanning)
    out.push_str("    .fstart.anchor : ALIGN(8) {\n");
    out.push_str("        *(.fstart.anchor)\n");
    out.push_str("    } > ROM\n\n");

    // Read-only data in ROM
    out.push_str("    .rodata : ALIGN(8) {\n");
    out.push_str("        *(.rodata .rodata.*)\n");
    out.push_str("    } > ROM\n\n");

    // Initialized data: stored in ROM, copied to RAM at startup
    // For now, place in RAM directly (QEMU loads entire bios image,
    // and our monolithic stage doesn't need a copy loop yet)
    out.push_str("    .data : ALIGN(8) {\n");
    out.push_str("        _data_start = .;\n");
    out.push_str("        *(.data .data.*)\n");
    out.push_str("        _data_end = .;\n");
    out.push_str("    } > RAM\n\n");

    // BSS in RAM
    out.push_str("    .bss (NOLOAD) : ALIGN(8) {\n");
    out.push_str("        _bss_start = .;\n");
    out.push_str("        *(.bss .bss.*)\n");
    out.push_str("        *(COMMON)\n");
    out.push_str("        _bss_end = .;\n");
    out.push_str("    } > RAM\n\n");

    // Stack in RAM
    out.push_str("    . = ALIGN(16);\n");
    out.push_str(&format!("    . = . + {stack_size:#x};\n"));
    out.push_str("    _stack_top = .;\n");
    out.push_str("}\n");
}

/// Generate linker script with everything in RAM (load_addr is in a RAM region).
fn generate_ram_layout(out: &mut String, ram_origin: u64, ram_length: u64, stack_size: u64) {
    out.push_str("MEMORY\n{\n");
    out.push_str(&format!(
        "    RAM (rwx) : ORIGIN = {ram_origin:#x}, LENGTH = {ram_length:#x}\n"
    ));
    out.push_str("}\n\n");

    out.push_str("SECTIONS\n{\n");
    out.push_str("    .text : {\n");
    out.push_str("        *(.text.entry)\n");
    out.push_str("        *(.text .text.*)\n");
    out.push_str("    } > RAM\n\n");

    // FFS anchor block (embedded in bootblock, 8-byte aligned for scanning)
    out.push_str("    .fstart.anchor : ALIGN(8) {\n");
    out.push_str("        *(.fstart.anchor)\n");
    out.push_str("    } > RAM\n\n");

    out.push_str("    .rodata : ALIGN(8) {\n");
    out.push_str("        *(.rodata .rodata.*)\n");
    out.push_str("    } > RAM\n\n");

    out.push_str("    .data : ALIGN(8) {\n");
    out.push_str("        *(.data .data.*)\n");
    out.push_str("    } > RAM\n\n");

    out.push_str("    .bss (NOLOAD) : ALIGN(8) {\n");
    out.push_str("        _bss_start = .;\n");
    out.push_str("        *(.bss .bss.*)\n");
    out.push_str("        *(COMMON)\n");
    out.push_str("        _bss_end = .;\n");
    out.push_str("    } > RAM\n\n");

    out.push_str("    . = ALIGN(16);\n");
    out.push_str(&format!("    . = . + {stack_size:#x};\n"));
    out.push_str("    _stack_top = .;\n");
    out.push_str("}\n");
}
