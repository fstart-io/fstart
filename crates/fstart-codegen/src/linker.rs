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

    // Determine load address and stack size
    let (load_addr, stack_size) = match (&config.stages, stage_name) {
        (StageLayout::Monolithic(mono), _) => (mono.load_addr, config.memory.stack_size),
        (StageLayout::MultiStage(stages), Some(name)) => {
            if let Some(stage) = stages.iter().find(|s| s.name.as_str() == name) {
                (stage.load_addr, config.memory.stack_size)
            } else {
                (0x8000_0000, 0x10000) // fallback
            }
        }
        _ => (0x8000_0000, 0x10000),
    };

    // Find the RAM region that contains load_addr (or the first RAM region)
    let ram_region = config
        .memory
        .regions
        .iter()
        .find(|r| r.kind == RegionKind::Ram && load_addr >= r.base && load_addr < r.base + r.size)
        .or_else(|| {
            config
                .memory
                .regions
                .iter()
                .find(|r| r.kind == RegionKind::Ram)
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

    out
}
