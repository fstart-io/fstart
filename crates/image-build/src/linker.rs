use std::fmt::Write;

use fstart_core::board::MicrocodeConfig;
use fstart_core::memory::FlashLayout;
use fstart_core::stage::POSTCAR_STAGE_NAME;
use fstart_core::{
    BoardConfig, FirmwareImagePolicy, Platform, RegionKind, SocImageFormat, StageBuildConfig,
    StageLayout, effective_stage_load_addr,
};

/// Fixed XIP/copy geometry consumed by reusable linker and ELF helpers.
/// Platforms supply the already-validated reservations and descriptor bytes.
pub struct Xip {
    pub platform: Platform,
    pub boot_hart_id: u32,
    pub image: crate::plan::Span,
    pub execution: Option<crate::plan::Span>,
    pub writable: crate::plan::Span,
    pub heap: crate::plan::Span,
    pub stack: crate::plan::Span,
    pub descriptor: crate::layout::EncodedLayout,
}

impl Xip {
    pub fn validate(&self) -> Result<(), String> {
        for range in [self.image, self.writable, self.heap, self.stack]
            .into_iter()
            .chain(self.execution)
        {
            range.end()?;
            if range.base % 16 != 0 || range.size % 16 != 0 {
                return Err("unaligned XIP reservation".into());
            }
        }
        if self.image.overlaps(self.writable)
            || self
                .execution
                .is_some_and(|r| r.overlaps(self.image) || r.overlaps(self.writable))
            || !self.writable.contains(self.heap.base, self.heap.size)
            || !self.writable.contains(self.stack.base, self.stack.size)
            || self.heap.base <= self.writable.base
            || self.heap.end()? != self.stack.base
            || self.stack.end()? != self.writable.end()?
        {
            return Err(
                "XIP code, writable, heap and stack reservations overlap or are inconsistent"
                    .into(),
            );
        }
        Ok(())
    }
    pub fn code_reservation(&self) -> crate::plan::Span {
        self.execution.unwrap_or(self.image)
    }
    pub fn elf_expectations(&self) -> Result<crate::elf::Expectations, String> {
        use crate::elf::{Architecture, CopyMapping, Descriptor, Expectations};
        self.validate()?;
        let architecture = match self.platform {
            Platform::Armv7 => Architecture::Arm,
            Platform::Aarch64 => Architecture::Aarch64,
            Platform::Riscv64 => Architecture::Riscv64,
            _ => return Err("unsupported XIP ELF architecture".into()),
        };
        Ok(Expectations {
            architecture,
            elf64: self.platform != Platform::Armv7,
            little_endian: true,
            stored: vec![self.image],
            runtime: vec![self.code_reservation(), self.writable],
            identity_mapping: false,
            entry: self.execution.map(|r| r.base),
            copy: self.execution.map(|execution| CopyMapping {
                storage: self.image,
                execution,
                end_symbol: "_binary_end".into(),
            }),
            descriptor: Descriptor {
                section: ".fstart.layout".into(),
                start_symbol: "_fstart_layout_start".into(),
                end_symbol: "_fstart_layout_end".into(),
                bytes: self.descriptor.as_bytes().to_vec(),
                reservation: self.code_reservation(),
            },
            symbols: std::collections::BTreeMap::from([
                ("_stack_bottom".into(), self.stack.base),
                ("_stack_top".into(), self.stack.end()?),
                ("_FSTART_HEAP".into(), self.heap.base),
            ]),
        })
    }
}

/// Emit inside SECTIONS, in the selected stage's initialized read-only load
/// region. The region name is linker-owned, not arbitrary board input. BYTE
/// emission preserves the wire format even on a big-endian target; it creates
/// real section contents independently of input-section garbage collection.
/// With LLD it must immediately follow a loaded read-only section: BYTE-only
/// sections inherit preceding flags, not the MEMORY region's attributes.
///
/// Not wired into legacy layouts: callers must first resolve fixed capacities.
pub fn layout_section(layout: &crate::layout::EncodedLayout, region: &str) -> String {
    let mut out =
        String::from("    .fstart.layout : ALIGN(8) {\n        _fstart_layout_start = .;\n");
    for byte in layout.as_bytes() {
        writeln!(out, "        BYTE({byte:#04x});").unwrap();
    }
    writeln!(out, "        _fstart_layout_end = .;\n    }} > {region}").unwrap();
    out
}

/// Fixed-budget XIP placement. Image/BSS growth never moves the heap or stack.
pub fn resolved_xip(layout: &Xip) -> Result<String, String> {
    layout.validate()?;
    let platform = layout.platform;
    let mut out = format!(
        "OUTPUT_ARCH({})\nENTRY(_start)\n_boot_hart_id = {};\nMEMORY {{\n",
        platform.linker_arch(),
        layout.boot_hart_id
    );
    for (name, flags, base, size) in [
        ("ROM", "rx", layout.image.base, layout.image.size),
        (
            "RAM",
            "rw",
            layout.writable.base,
            layout.heap.base - layout.writable.base,
        ),
        ("HEAP", "rw", layout.heap.base, layout.heap.size),
        ("STACK", "rw", layout.stack.base, layout.stack.size),
    ] {
        writeln!(
            out,
            " {name} ({flags}) : ORIGIN = {base:#x}, LENGTH = {size:#x}"
        )
        .unwrap();
    }
    let code = if let Some(execution) = layout.execution {
        writeln!(
            out,
            " EXEC (rx) : ORIGIN = {:#x}, LENGTH = {:#x}",
            execution.base, execution.size
        )
        .unwrap();
        "EXEC AT > ROM"
    } else {
        "ROM"
    };
    out.push_str("}\nSECTIONS {\n");
    write_text_section(&mut out, code);
    // LLD's BYTE-only sections inherit flags from the preceding section.
    // Emit immediately after executable text, before potentially writable
    // keep anchors; MEMORY attributes alone do not clear SHF_WRITE.
    out.push_str(&layout_section(&layout.descriptor, code));
    write_anchor_section(&mut out, code, platform, layout.heap.size);
    write_rodata_section(&mut out, code);
    // Explicit placement prevents LLD's orphan sections from acquiring RAM
    // physical addresses in a relocated image.
    for section in [".fstart.keep", ".eh_frame_hdr", ".eh_frame"] {
        writeln!(out, " {section} : ALIGN(8) {{ *({section}) }} > {code}").unwrap();
    }
    out.push_str(" .data : ALIGN(16) { _data_start = .; *(.data .data.* .ldata .ldata.*) _data_end = .; } > RAM AT > ROM\n _data_load = LOADADDR(.data);\n");
    write_bss_section(&mut out, "RAM");
    write_page_tables_section(&mut out, "RAM", platform);
    write_heap(&mut out, layout.heap.size, "HEAP");
    write_stack(&mut out, layout.stack.size, "STACK");
    if layout.execution.is_some() {
        // The unchanged reset copier uses _binary_end - _start. Include the
        // initialized data's flash bytes in the RAM copy, not its final VMA.
        let stored_end = [".text", ".fstart.layout", ".fstart.anchor", ".rodata", ".fstart.keep", ".eh_frame_hdr", ".eh_frame", ".data"].iter().fold("ORIGIN(ROM)".to_owned(), |end, section| format!("MAX({end}, SIZEOF({section}) == 0 ? ORIGIN(ROM) : LOADADDR({section}) + SIZEOF({section}))"));
        writeln!(
            out,
            " _binary_end = ORIGIN(EXEC) + {stored_end} - ORIGIN(ROM);"
        )
        .unwrap();
        // The reset copier has already brought these initializers into RAM.
        // Keep the common entry and running-stage bounds in that same address space.
        out.push_str(" _data_load = ORIGIN(EXEC) + LOADADDR(.data) - ORIGIN(ROM);\n");
        out.push_str(" ASSERT(_binary_end <= ORIGIN(EXEC) + LENGTH(EXEC), \"relocated image exceeds execution capacity\")\n ASSERT(_page_tables_end <= ORIGIN(HEAP), \"page tables exceed writable capacity\")\n");
    }
    out.push_str(" ASSERT(_bss_end <= ORIGIN(HEAP), \"stage data/BSS exceed resolved capacity\")\n ASSERT(LOADADDR(.data) + SIZEOF(.data) <= ORIGIN(ROM) + LENGTH(ROM), \"stage image exceeds resolved capacity\")\n}\n");
    Ok(out)
}

/// Fixed Intel stage placement. No section size feeds back into the bootblock
/// base, heap base or stack top. The board build path opts in only after its
/// complete metadata/assembler boundary is migrated.
pub fn resolved_intel(
    layout: &crate::intel_plan::IntelReservations,
    role: crate::intel_plan::IntelStage,
    early_microcode: bool,
) -> Result<String, String> {
    use crate::intel_plan::IntelStage;
    layout.validate()?;
    let stage = layout.stage(role);
    let bootblock = matches!(role, IntelStage::Bootblock);
    let entry = match role {
        IntelStage::Bootblock => "_start",
        IntelStage::Postcar => "_start_postcar",
        IntelStage::Ramstage => "_start_ram",
    };
    let stack = stage.stack_span();
    let heap_base = stage.heap_span().map_or(stack.base, |heap| heap.base);
    let mut out = format!("OUTPUT_ARCH(i386:x86-64)\nENTRY({entry})\nMEMORY {{\n");
    for (name, flags, base, size) in [
        ("IMAGE", "rx", stage.image.base, stage.image.size),
        (
            "DATA",
            "rw",
            stage.writable.base,
            heap_base - stage.writable.base,
        ),
        ("HEAP", "rw", heap_base, stage.heap),
        ("STACK", "rw", stack.base, stack.size),
    ] {
        writeln!(
            out,
            " {name} ({flags}) : ORIGIN = {base:#x}, LENGTH = {size:#x}"
        )
        .unwrap();
    }
    out.push_str("}\nSECTIONS {\n");
    write_text_section(&mut out, "IMAGE");
    out.push_str(&layout_section(&layout.descriptor(role)?, "IMAGE"));
    out.push_str(" .fstart.anchor : ALIGN(16) { _fstart_anchor_early = .; *(.fstart.anchor) _fstart_early_microcode_enabled = .;\n");
    writeln!(out, " LONG({})", u8::from(bootblock && early_microcode)).unwrap();
    write_heap_size_constant(&mut out, Platform::X86_64, stage.heap);
    out.push_str(" } > IMAGE\n");
    write_rodata_section(&mut out, "IMAGE");
    for section in [".fstart.keep", ".eh_frame_hdr", ".eh_frame"] {
        writeln!(out, " {section} : ALIGN(8) {{ *({section}) }} > IMAGE").unwrap();
    }
    write_data_section(&mut out, if bootblock { "DATA AT > IMAGE" } else { "DATA" });
    write_bss_section(&mut out, "DATA");
    write_heap(&mut out, stage.heap, "HEAP");
    write_stack(&mut out, stage.stack, "STACK");
    if bootblock {
        let end = stage.image.end()?;
        writeln!(
            out,
            " _bootblock = ORIGIN(IMAGE); _bootblock_base = ORIGIN(IMAGE); _bootblock_top = {:#x};",
            end - 4096
        )
        .unwrap();
        writeln!(
            out,
            " _has_car = 1; _car_base = {:#x}; _car_size = {:#x}; _ecar_stack = _stack_top;",
            stage.writable.base, stage.writable.size
        )
        .unwrap();
        writeln!(
            out,
            " _rom_mtrr_base = {:#x}; _rom_mtrr_mask = {:#x};",
            layout.flash.base,
            !(layout.flash.size - 1) & 0xffff_ffff
        )
        .unwrap();
        writeln!(
            out,
            " .x86boot {0:#x} : AT({0:#x}) {{ KEEP(*(.x86boot)) }} > IMAGE",
            end - 4096
        )
        .unwrap();
        writeln!(
            out,
            " .reset {0:#x} : AT({0:#x}) {{ KEEP(*(.reset)) }} > IMAGE",
            end - 16
        )
        .unwrap();
        writeln!(out, " _binary_end = {end:#x};").unwrap();
        out.push_str(" ASSERT(LOADADDR(.data) + SIZEOF(.data) <= _bootblock_top, \"bootblock overlaps reset page\")\n");
    } else {
        write_x86_car_symbols(&mut out, Platform::X86_64, false);
    }
    out.push_str(" ASSERT(_bss_end <= ORIGIN(HEAP), \"data/BSS exceed fixed capacity\")\n}\n");
    Ok(out)
}

pub fn generate_linker_script(config: &BoardConfig, stage_name: Option<&str>) -> String {
    let mut out = String::new();

    let arch = config.platform.linker_arch();

    let (load_addr, stack_size, heap_size, data_addr, _page_table_addr) =
        match (&config.stages, stage_name) {
            (StageLayout::Monolithic(mono), _) => (
                mono.load_addr,
                mono.stack_size as u64,
                u64::from(mono.heap_size.unwrap_or(0)),
                mono.data_addr,
                mono.page_table_addr,
            ),
            (StageLayout::MultiStage(stages), Some(name)) => {
                if let Some((index, stage)) = stages
                    .iter()
                    .enumerate()
                    .find(|(_, s)| s.name.as_str() == name)
                {
                    (
                        effective_stage_load_addr(config, index, stage),
                        stage.stack_size as u64,
                        u64::from(stage.heap_size.unwrap_or(0)),
                        stage.data_addr,
                        stage.page_table_addr,
                    )
                } else {
                    (0x8000_0000, 0x10000, 0, None, None)
                }
            }
            _ => (0x8000_0000, 0x10000, 0, None, None),
        };

    let rom_region =
        config.memory.regions.iter().find(|r| {
            r.kind == RegionKind::Rom && load_addr >= r.base && load_addr < r.base + r.size
        });

    let car_config = if rom_region.is_some() {
        config.memory.car.as_ref().map(|c| (c.base, c.size))
    } else {
        None
    };

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
        })
        .or_else(|| {
            config
                .memory
                .regions
                .iter()
                .find(|r| load_addr >= r.base && load_addr < r.base + r.size)
        });

    let (ram_origin, ram_length) = if let Some((car_base, car_size)) = car_config {
        (car_base, car_size)
    } else {
        ram_region
            .map(|r| (r.base, r.size))
            .unwrap_or((0x8000_0000, 0x0800_0000))
    };

    let is_first_stage = match (&config.stages, stage_name) {
        (StageLayout::Monolithic(_), _) => true,
        (StageLayout::MultiStage(stages), Some(name)) => {
            stages.first().is_some_and(|s| s.name.as_str() == name)
        }
        (StageLayout::MultiStage(_), None) => true,
    };
    let needs_egon_header =
        is_first_stage && matches!(config.soc_image_format, SocImageFormat::AllwinnerEgon);

    let has_x86_car =
        config.platform == Platform::X86_64 && config.memory.car.is_some() && is_first_stage;

    writeln!(
        out,
        "/* Auto-generated linker script for board: {} */\n",
        config.name
    )
    .unwrap();
    writeln!(out, "OUTPUT_ARCH({arch})").unwrap();

    if needs_egon_header {
        writeln!(out, "ENTRY(_head_jump)\n").unwrap();
    } else if config.platform == Platform::X86_64 && !is_first_stage {
        // Intel Cut-B postcar runs its CAR-teardown entry; every other
        // non-first x86_64 stage enters with caching already on.
        if stage_name == Some(POSTCAR_STAGE_NAME) {
            writeln!(out, "ENTRY(_start_postcar)\n").unwrap();
        } else {
            writeln!(out, "ENTRY(_start_ram)\n").unwrap();
        }
    } else {
        writeln!(out, "ENTRY(_start)\n").unwrap();
    }

    writeln!(out, "_boot_hart_id = {};", config.boot_hart_id).unwrap();

    if let Some(rom) = rom_region {
        let (x86_rom_mtrr_base, x86_rom_mtrr_size) = if config.platform == Platform::X86_64 {
            match &config.memory.flash_layout {
                Some(FlashLayout::IntelIfd(layout)) => (layout.base(), u64::from(layout.size())),
                Some(FlashLayout::X86Legacy(layout)) => (layout.base(), u64::from(layout.size())),
                None => config
                    .memory
                    .firmware_window()
                    .unwrap_or((rom.base, rom.size)),
            }
        } else {
            (rom.base, rom.size)
        };
        generate_xip_layout(
            &mut out,
            rom.base,
            rom.size,
            ram_origin,
            ram_length,
            stack_size,
            heap_size,
            data_addr,
            needs_egon_header,
            is_first_stage,
            config.platform,
            car_config,
            x86_rom_mtrr_base,
            x86_rom_mtrr_size,
            x86_early_microcode_enabled(config),
        );
    } else {
        let region_end = ram_origin + ram_length;
        let effective_origin = load_addr;
        let effective_length = region_end - effective_origin;

        let bss_origin =
            stage_memory_mapped_boot_media(config, stage_name).and_then(|(base, size)| {
                if base != load_addr || size == 0 {
                    return None;
                }
                let bss_addr = base.checked_add(size)?;
                if bss_addr < effective_origin || bss_addr >= effective_origin + effective_length {
                    panic!(
                        "firmware image window ({base:#x}, {size:#x}) ends at {bss_addr:#x}, \
                         outside RAM region [{effective_origin:#x}..{:#x}]",
                        effective_origin + effective_length
                    );
                }
                Some(bss_addr)
            });
        generate_ram_layout(
            &mut out,
            effective_origin,
            effective_length,
            stack_size,
            heap_size,
            bss_origin,
            needs_egon_header,
            config.platform,
            has_x86_car,
        );
    }

    out
}

fn stage_memory_mapped_boot_media(
    config: &BoardConfig,
    stage_name: Option<&str>,
) -> Option<(u64, u64)> {
    stage_build(config, stage_name)?.firmware_image.as_ref()?;
    config
        .memory
        .firmware_window()
        .or(match config.build.firmware_image {
            FirmwareImagePolicy::MemoryMapped { cpu_base, size } => Some((cpu_base, size)),
            FirmwareImagePolicy::Auto | FirmwareImagePolicy::None => None,
        })
}

fn stage_build<'a>(
    config: &'a BoardConfig,
    stage_name: Option<&str>,
) -> Option<&'a StageBuildConfig> {
    match (&config.stages, stage_name) {
        (StageLayout::Monolithic(mono), _) => Some(&mono.build),
        (StageLayout::MultiStage(stages), Some(name)) => stages
            .iter()
            .find(|stage| stage.name.as_str() == name)
            .map(|stage| &stage.build),
        (StageLayout::MultiStage(stages), None) => stages.first().map(|stage| &stage.build),
    }
}

fn x86_early_microcode_enabled(config: &BoardConfig) -> bool {
    match &config.microcode {
        Some(MicrocodeConfig::Intel(intel)) => intel.early,
        None => false,
    }
}

fn write_x86_car_symbols(out: &mut String, platform: Platform, has_x86_car: bool) {
    if platform != Platform::X86_64 {
        return;
    }
    writeln!(out).unwrap();
    writeln!(out, "    /* x86 CAR/postcar symbols */").unwrap();
    writeln!(out, "    _has_car = {};", if has_x86_car { 1 } else { 0 }).unwrap();
    writeln!(out, "    _car_base = 0;").unwrap();
    writeln!(out, "    _car_size = 0;").unwrap();
    writeln!(out, "    _ecar_stack = _stack_top;").unwrap();
    writeln!(out, "    _rom_mtrr_base = 0;").unwrap();
    writeln!(out, "    _rom_mtrr_mask = 0;").unwrap();
}

#[allow(clippy::too_many_arguments)]
fn generate_xip_layout(
    out: &mut String,
    rom_origin: u64,
    rom_length: u64,
    ram_origin: u64,
    ram_length: u64,
    stack_size: u64,
    heap_size: u64,
    data_addr: Option<u64>,
    needs_egon_header: bool,
    is_first_stage: bool,
    platform: Platform,
    car_config: Option<(u64, u64)>,
    x86_rom_mtrr_base: u64,
    x86_rom_mtrr_size: u64,
    x86_early_microcode_enabled: bool,
) {
    let rw_origin = data_addr.unwrap_or(ram_origin);
    let rw_length = ram_length - (rw_origin - ram_origin);

    let xip_stack_region = if car_config.is_some() {
        if stack_size >= rw_length {
            panic!(
                "stack_size ({stack_size:#x}) does not fit in writable XIP region \
                 [{rw_origin:#x}..{:#x}]",
                rw_origin + rw_length
            );
        }
        let stack_origin = rw_origin + rw_length - stack_size;
        Some((stack_origin, stack_size, rw_length - stack_size))
    } else {
        None
    };

    writeln!(out, "MEMORY\n{{").unwrap();
    writeln!(
        out,
        "    ROM (rx)  : ORIGIN = {rom_origin:#x}, LENGTH = {rom_length:#x}"
    )
    .unwrap();
    let ram_decl_length = xip_stack_region
        .map(|(_, _, data_length)| data_length)
        .unwrap_or(rw_length);
    writeln!(
        out,
        "    RAM (rwx) : ORIGIN = {rw_origin:#x}, LENGTH = {ram_decl_length:#x}"
    )
    .unwrap();
    if let Some((stack_origin, stack_length, _)) = xip_stack_region {
        writeln!(
            out,
            "    STACK (rw) : ORIGIN = {stack_origin:#x}, LENGTH = {stack_length:#x}"
        )
        .unwrap();
    }
    writeln!(out, "}}\n").unwrap();

    writeln!(out, "SECTIONS\n{{").unwrap();

    let x86_top_aligned_bootblock = platform == Platform::X86_64 && is_first_stage;
    if x86_top_aligned_bootblock {
        let bootblock_top = rom_origin + rom_length - 0x1000;
        writeln!(
            out,
            "    /* x86 bootblock: place the C environment at the top of ROM. */"
        )
        .unwrap();
        writeln!(out, "    _bootblock_top = {bootblock_top:#x};").unwrap();
        writeln!(out, "    _bootblock_program_size = SIZEOF(.text) + SIZEOF(.fstart.anchor) + SIZEOF(.rodata) + SIZEOF(.data);").unwrap();
        writeln!(out, "    _bootblock_base = ((_bootblock_top - _bootblock_program_size) & ~0xfff) - 0x1000;\n").unwrap();
    }

    if needs_egon_header {
        writeln!(out, "    .head : {{").unwrap();
        writeln!(out, "        KEEP(*(.head.text))").unwrap();
        writeln!(out, "        KEEP(*(.head.egon))").unwrap();
        writeln!(out, "    }} > ROM\n").unwrap();
    }

    if x86_top_aligned_bootblock {
        writeln!(out, "    .text _bootblock_base : {{").unwrap();
        writeln!(out, "        _bootblock = .;").unwrap();
    } else {
        writeln!(out, "    .text : {{").unwrap();
    }
    writeln!(out, "        _text_start = .;").unwrap();
    writeln!(out, "        KEEP(*(.text.entry))").unwrap();
    writeln!(out, "        *(.text .text.* .ltext .ltext.*)").unwrap();
    writeln!(out, "        _text_end = .;").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    writeln!(out, "    .fstart.anchor : ALIGN(16) {{").unwrap();
    if platform == Platform::X86_64 {
        writeln!(out, "        _fstart_anchor_early = .;").unwrap();
    }
    writeln!(out, "        *(.fstart.anchor)").unwrap();
    if platform == Platform::X86_64 {
        writeln!(out, "        _fstart_early_microcode_enabled = .;").unwrap();
        writeln!(
            out,
            "        LONG({})",
            u8::from(x86_early_microcode_enabled)
        )
        .unwrap();
    }
    write_heap_size_constant(out, platform, heap_size);
    writeln!(out, "    }} > ROM\n").unwrap();

    writeln!(out, "    .rodata : ALIGN(16) {{").unwrap();
    writeln!(out, "        _rodata_start = .;").unwrap();
    writeln!(out, "        *(.eh_frame_hdr .eh_frame)").unwrap();
    writeln!(out, "        *(.rodata .rodata.* .lrodata .lrodata.*)").unwrap();
    writeln!(out, "    }} > ROM\n").unwrap();

    writeln!(out, "    .data : ALIGN(16) {{").unwrap();
    writeln!(out, "        _data_start = .;").unwrap();
    writeln!(out, "        *(.data .data.* .ldata .ldata.*)").unwrap();
    writeln!(out, "        _data_end = .;").unwrap();
    writeln!(out, "    }} > RAM AT > ROM").unwrap();
    writeln!(out, "    _data_load = LOADADDR(.data);\n").unwrap();

    writeln!(out, "    .bss (NOLOAD) : ALIGN(16) {{").unwrap();
    writeln!(out, "        _bss_start = .;").unwrap();
    writeln!(out, "        *(.bss .bss.* .lbss .lbss.*)").unwrap();
    writeln!(out, "        *(COMMON)").unwrap();
    writeln!(out, "        _bss_end = .;").unwrap();
    writeln!(out, "    }} > RAM\n").unwrap();

    write_page_tables_section(out, "RAM", platform);

    write_heap(out, heap_size, "RAM");

    let stack_region = if xip_stack_region.is_some() {
        "STACK"
    } else {
        "RAM"
    };
    write_stack(out, stack_size, stack_region);

    if let Some((car_base, car_size)) = car_config {
        writeln!(out).unwrap();
        writeln!(out, "    /* Cache-as-RAM symbols for car.rs */").unwrap();
        writeln!(out, "    _car_base = {car_base:#x};").unwrap();
        writeln!(out, "    _car_size = {car_size:#x};").unwrap();
        writeln!(out, "    _ecar_stack = _stack_top;").unwrap();
        writeln!(out, "    _rom_mtrr_base = {x86_rom_mtrr_base:#x};").unwrap();
        let rom_mask = !(x86_rom_mtrr_size - 1) & 0xFFFF_FFFF;
        writeln!(out, "    _rom_mtrr_mask = {rom_mask:#x};").unwrap();
        writeln!(out, "    _has_car = 1;").unwrap();
    } else {
        writeln!(out).unwrap();
        writeln!(out, "    /* x86 CAR/postcar symbols (CAR disabled) */").unwrap();
        writeln!(out, "    _has_car = 0;").unwrap();
        writeln!(out, "    _car_base = 0;").unwrap();
        writeln!(out, "    _car_size = 0;").unwrap();
        writeln!(out, "    _ecar_stack = _stack_top;").unwrap();
        writeln!(out, "    _rom_mtrr_base = 0;").unwrap();
        writeln!(out, "    _rom_mtrr_mask = 0;").unwrap();
    }

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
        writeln!(
            out,
            "    ASSERT(_bootblock >= ORIGIN(ROM), \"x86 bootblock does not fit in ROM\")"
        )
        .unwrap();
        writeln!(out, "    ASSERT(_data_load + SIZEOF(.data) <= _bootblock_top, \"x86 bootblock overlaps reset entry area\")")
            .unwrap();
    }

    writeln!(out, "}}").unwrap();
}

#[allow(clippy::too_many_arguments)]
fn generate_ram_layout(
    out: &mut String,
    ram_origin: u64,
    ram_length: u64,
    stack_size: u64,
    heap_size: u64,
    bss_origin: Option<u64>,
    needs_egon_header: bool,
    platform: Platform,
    has_x86_car: bool,
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
        write_anchor_section(out, "CODE", platform, heap_size);
        write_rodata_section(out, "CODE");
        write_data_section(out, "CODE");
        write_bss_section(out, "RWDATA");
        write_page_tables_section(out, "RWDATA", platform);
        write_heap(out, heap_size, "RWDATA");
        write_stack(out, stack_size, "RWDATA");
        write_x86_car_symbols(out, platform, has_x86_car);
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
        write_anchor_section(out, "RAM", platform, heap_size);
        write_rodata_section(out, "RAM");
        write_data_section(out, "RAM");
        write_bss_section(out, "RAM");
        write_page_tables_section(out, "RAM", platform);
        write_heap(out, heap_size, "RAM");
        write_stack(out, stack_size, "RAM");
        write_x86_car_symbols(out, platform, has_x86_car);
        writeln!(out, "}}").unwrap();
    }
}

fn write_text_section(out: &mut String, region: &str) {
    writeln!(out, "    .text : {{").unwrap();
    writeln!(out, "        _text_start = .;").unwrap();
    writeln!(out, "        KEEP(*(.text.entry))").unwrap();
    writeln!(out, "        *(.text .text.* .ltext .ltext.*)").unwrap();
    writeln!(out, "        KEEP(*(.vectors .vectors.*))").unwrap();
    writeln!(out, "        _text_end = .;").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_anchor_section(out: &mut String, region: &str, platform: Platform, heap_size: u64) {
    writeln!(out, "    .fstart.anchor : ALIGN(8) {{").unwrap();
    if platform == Platform::X86_64 {
        writeln!(out, "        _fstart_anchor_early = .;").unwrap();
    }
    writeln!(out, "        *(.fstart.anchor)").unwrap();
    if platform == Platform::X86_64 {
        writeln!(out, "        _fstart_early_microcode_enabled = .;").unwrap();
        writeln!(out, "        LONG(0)").unwrap();
    }
    write_heap_size_constant(out, platform, heap_size);
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_heap_size_constant(out: &mut String, platform: Platform, heap_size: u64) {
    let word = match platform {
        Platform::Armv7 => "LONG",
        _ => "QUAD",
    };
    writeln!(out, "        . = ALIGN(8);").unwrap();
    writeln!(out, "        _FSTART_HEAP_SIZE = .;").unwrap();
    writeln!(out, "        {word}({heap_size:#x})").unwrap();
}

fn write_heap(out: &mut String, heap_size: u64, region: &str) {
    writeln!(out, "    .fstart.heap (NOLOAD) : ALIGN(16) {{").unwrap();
    writeln!(out, "        _FSTART_HEAP = .;").unwrap();
    writeln!(out, "        . = . + {heap_size:#x};").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_rodata_section(out: &mut String, region: &str) {
    writeln!(out, "    .rodata : ALIGN(8) {{").unwrap();
    writeln!(out, "        _rodata_start = .;").unwrap();
    writeln!(out, "        KEEP(*(.fstart.bootstrap_pin))").unwrap();
    writeln!(out, "        *(.rodata .rodata.* .lrodata .lrodata.*)").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_data_section(out: &mut String, region: &str) {
    writeln!(out, "    .data : ALIGN(8) {{").unwrap();
    writeln!(out, "        _data_start = .;").unwrap();
    writeln!(out, "        *(.data .data.* .ldata .ldata.*)").unwrap();
    writeln!(out, "        _data_end = .;").unwrap();
    writeln!(out, "    }} > {region}").unwrap();
    writeln!(out, "    _data_load = LOADADDR(.data);\n").unwrap();
}

fn write_page_tables_section(out: &mut String, region: &str, platform: Platform) {
    if platform != Platform::Aarch64 {
        return;
    }
    writeln!(out, "    .page_tables (NOLOAD) : ALIGN(4096) {{").unwrap();
    writeln!(out, "        _page_tables_start = .;").unwrap();
    writeln!(out, "        KEEP(*(.page_tables))").unwrap();
    writeln!(out, "        _page_tables_end = .;").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_bss_section(out: &mut String, region: &str) {
    writeln!(out, "    .bss (NOLOAD) : ALIGN(8) {{").unwrap();
    writeln!(out, "        _bss_start = .;").unwrap();
    writeln!(out, "        *(.bss .bss.* .lbss .lbss.*)").unwrap();
    writeln!(out, "        *(COMMON)").unwrap();
    writeln!(out, "        _bss_end = .;").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_allwinner_egon_section(out: &mut String, region: &str) {
    writeln!(out, "    .head : {{").unwrap();
    writeln!(out, "        KEEP(*(.head.text))").unwrap();
    writeln!(out, "        KEEP(*(.head.egon))").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
}

fn write_stack(out: &mut String, stack_size: u64, region: &str) {
    writeln!(out, "    .stack (NOLOAD) : ALIGN(16) {{").unwrap();
    writeln!(out, "        _stack_bottom = .;").unwrap();
    writeln!(out, "        . = . + {stack_size:#x};").unwrap();
    writeln!(out, "        . = ALIGN(16);").unwrap();
    writeln!(out, "        _stack_top = .;").unwrap();
    writeln!(out, "        _writable_end = .;").unwrap();
    writeln!(out, "    }} > {region}\n").unwrap();
    writeln!(
        out,
        "    ASSERT(_stack_top <= ORIGIN({region}) + LENGTH({region}), \"insufficient stack space\")"
    )
    .unwrap();

    writeln!(out, "    _binary_end = _data_end;").unwrap();
}

#[cfg(test)]
mod tests {
    use fstart_core::{
        BoardBuildPolicy, FirmwareImageConfig, MemoryMap, MemoryRegion, MonolithicConfig, Platform,
        RegionKind, SecurityConfig, SignatureAlgorithm, StageBuildConfig, StageLayout, hstr, hvec,
    };

    use super::generate_linker_script;

    #[test]
    fn ram_loaded_ffs_reserves_its_build_policy_window() {
        let config = fstart_core::BoardConfig {
            name: hstr("qemu-sifive-u"),
            platform: Platform::Riscv64,
            memory: MemoryMap {
                regions: hvec([MemoryRegion {
                    name: hstr("dram"),
                    base: 0x8000_0000,
                    size: 0x4000_0000,
                    kind: RegionKind::Ram,
                }]),
                flash_layout: None,
                car: None,
            },
            stages: StageLayout::Monolithic(MonolithicConfig {
                build: StageBuildConfig {
                    firmware_image: Some(FirmwareImageConfig {
                        temp_ram_buffer: None,
                    }),
                    ..StageBuildConfig::default()
                },
                load_addr: 0x8000_0000,
                stack_size: 0x4000,
                heap_size: None,
                data_addr: None,
                page_table_addr: None,
                page_size: Default::default(),
            }),
            security: SecurityConfig {
                signing_algorithm: SignatureAlgorithm::Ed25519,
                pubkey_file: hstr("keys/dev-signing.pub"),
                required_digests: hvec([]),
            },
            payload: None,
            microcode: None,
            soc_image_format: Default::default(),
            full_flash_image: false,
            build: BoardBuildPolicy {
                firmware_image: fstart_core::FirmwareImagePolicy::memory_mapped(
                    0x8000_0000,
                    0x1000_0000,
                ),
                ..BoardBuildPolicy::default()
            },
            acpi: None,
            smbios: None,
            smm: None,
            boot_hart_id: 0,
        };

        let script = generate_linker_script(&config, None);
        assert!(script.contains("RWDATA (rwx) : ORIGIN = 0x90000000"));
        assert!(script.contains(".bss (NOLOAD) : ALIGN(8) {"));
    }
}
