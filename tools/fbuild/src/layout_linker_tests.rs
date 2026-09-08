//! Real LLD/ELF/flat-binary proof for the descriptor emission, without a board
//! build or target stdlib. The script itself emits a tiny initialized text
//! section, so this tests storage/endianness/GC, not executable entry or boot.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use fstart_core::layout::{Layout, Region, RegionKind};
use fstart_image_build::layout::EncodedLayout;
use object::{Object, ObjectSection, ObjectSymbol};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn layout_survives_gc_and_flat_extraction_on_both_endiannesses() {
    // rust-lld is shipped with this project's pinned Rust toolchain. Resolving
    // the host libdir avoids depending on a system linker or cross-binutils.
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--print", "target-libdir"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libdir = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let linker = libdir.parent().unwrap().join("bin/rust-lld");
    assert!(
        linker.is_file(),
        "pinned toolchain linker missing: {}",
        linker.display()
    );

    let scratch =
        Scratch(std::env::temp_dir().join(format!("fstart-layout-link-{}", std::process::id())));
    fs::create_dir_all(&scratch.0).unwrap();
    let flash = Region {
        kind: RegionKind::Flash,
        base: 0x2000_0000,
        size: 0x1000,
    };
    let encoded = EncodedLayout::encode(3, &[flash]).unwrap();

    for (arch, emulation, little_endian) in [
        ("riscv", "elf64lriscv", true),
        ("aarch64", "aarch64elfb", false),
    ] {
        let script = scratch.0.join(format!("{arch}.ld"));
        let elf = scratch.0.join(format!("{arch}.elf"));
        let flat = scratch.0.join(format!("{arch}.bin"));
        fs::write(
            &script,
            format!(
                "OUTPUT_ARCH({arch})\nENTRY(_start)\n\
             MEMORY {{ ROM (rx) : ORIGIN = 0x20000000, LENGTH = 0x1000 }}\n\
             SECTIONS {{\n\
             .text : {{ _start = .; BYTE(0x13); BYTE(0); BYTE(0); BYTE(0); }} > ROM\n\
             {}\n}}\n",
                crate::linker::layout_section(&encoded, "ROM"),
            ),
        )
        .unwrap();
        let result = Command::new(&linker)
            .args(["-flavor", "gnu", "-m", emulation, "--gc-sections"])
            .arg(&script)
            .arg("-o")
            .arg(&elf)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );

        let elf_bytes = fs::read(&elf).unwrap();
        let object = object::File::parse(&*elf_bytes).unwrap();
        assert_eq!(object.is_little_endian(), little_endian);
        let section = object.section_by_name(".fstart.layout").unwrap();
        assert_eq!(section.address() % 8, 0);
        assert_eq!(section.data().unwrap(), encoded.as_bytes());
        let object::SectionFlags::Elf { sh_flags } = section.flags() else {
            panic!("not ELF")
        };
        assert_ne!(sh_flags & u64::from(object::elf::SHF_ALLOC), 0);
        assert_eq!(sh_flags & u64::from(object::elf::SHF_WRITE), 0);
        assert!(section.file_range().is_some());

        for (name, address) in [
            ("_fstart_layout_start", section.address()),
            ("_fstart_layout_end", section.address() + section.size()),
        ] {
            let symbol = object
                .symbols()
                .find(|symbol| symbol.name() == Ok(name))
                .unwrap();
            assert_eq!(symbol.address(), address);
        }
        super::write_flat_binary(&elf, &flat).unwrap();
        let flat_bytes = fs::read(&flat).unwrap();
        let offset = usize::try_from(section.address() - flash.base).unwrap();
        let bytes = &flat_bytes[offset..offset + encoded.as_bytes().len()];
        assert_eq!(bytes, encoded.as_bytes());
        let view = Layout::parse(bytes).unwrap();
        assert_eq!(view.stage_index(), 3);
        assert_eq!(view.region(RegionKind::Flash), Some(flash));
    }

    // Exercise the actual resolved script with a retained writable anchor.
    // BYTE-only output sections can inherit its SHF_WRITE despite ROM (rx).
    for (architecture, profile, emulation, relocation) in [
        (
            object::Architecture::Riscv64,
            "riscv64-xip",
            "elf64lriscv",
            object::elf::R_RISCV_64,
        ),
        (
            object::Architecture::Arm,
            "armv7-xip",
            "armelf",
            object::elf::R_ARM_ABS32,
        ),
        (
            object::Architecture::Aarch64,
            "aarch64-relocate",
            "aarch64elf",
            object::elf::R_AARCH64_ABS64,
        ),
    ] {
        let (script_text, expectations) = if architecture == object::Architecture::Aarch64 {
            let plan = fstart_platform_qemu::host::resolve(
                fstart_platform_qemu::facts::Aarch64ImageFacts::new(0x0800_0000),
                fstart_image_build::plan::BuildSelection {
                    payload: Some("halt".into()),
                },
            )
            .unwrap();
            let unit = plan.units.into_iter().next().unwrap();
            let fstart_image_build::build_plan::UnitOutput::Executable { expectations, .. } =
                unit.output
            else {
                panic!("expected executable")
            };
            (unit.linker_script.unwrap(), expectations)
        } else {
            let resolved = crate::resolved::tests::resolved_for(profile, "halt");
            (
                crate::linker::resolved_xip(&resolved).unwrap(),
                resolved.elf_expectations().unwrap(),
            )
        };
        let mut input = object::write::Object::new(
            object::BinaryFormat::Elf,
            architecture,
            object::Endianness::Little,
        );
        let text = input.add_section(
            Vec::new(),
            b".text.entry".to_vec(),
            object::SectionKind::Text,
        );
        input.append_section_data(text, &[0; 16], 8);
        input.add_symbol(object::write::Symbol {
            name: b"_start".to_vec(),
            value: 0,
            size: 16,
            kind: object::SymbolKind::Text,
            scope: object::SymbolScope::Linkage,
            weak: false,
            section: object::write::SymbolSection::Section(text),
            flags: object::SymbolFlags::None,
        });
        let anchor = input.add_section(
            Vec::new(),
            b".fstart.anchor".to_vec(),
            object::SectionKind::Data,
        );
        input.append_section_data(anchor, &[1; 8], 8);
        let symbol = input.section_symbol(anchor);
        input
            .add_relocation(
                text,
                object::write::Relocation {
                    offset: 0,
                    symbol,
                    addend: 0,
                    flags: object::RelocationFlags::Elf { r_type: relocation },
                },
            )
            .unwrap();
        let data = input.add_section(Vec::new(), b".data".to_vec(), object::SectionKind::Data);
        input.append_section_data(data, &[7; 16], 16);
        let data_symbol = input.section_symbol(data);
        input
            .add_relocation(
                text,
                object::write::Relocation {
                    offset: 8,
                    symbol: data_symbol,
                    addend: 0,
                    flags: object::RelocationFlags::Elf { r_type: relocation },
                },
            )
            .unwrap();
        if architecture == object::Architecture::Aarch64 {
            let vectors =
                input.add_section(Vec::new(), b".vectors".to_vec(), object::SectionKind::Text);
            input.append_section_data(vectors, &[0; 32], 4096);
            let tables = input.add_section(
                Vec::new(),
                b".page_tables".to_vec(),
                object::SectionKind::UninitializedData,
            );
            input.append_section_bss(tables, 0x4000, 4096);
        }
        let object_path = scratch.0.join("anchors.o");
        let script = scratch.0.join("resolved.ld");
        let elf = scratch.0.join("resolved.elf");
        fs::write(&object_path, input.write().unwrap()).unwrap();
        fs::write(&script, &script_text).unwrap();
        let link = || {
            Command::new(&linker)
                .args(["-flavor", "gnu", "-m", emulation, "--gc-sections", "-T"])
                .arg(&script)
                .arg(&object_path)
                .arg("-o")
                .arg(&elf)
                .output()
                .unwrap()
        };
        let result = link();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = fs::read(&elf).unwrap();
        let linked = object::File::parse(&*bytes).unwrap();
        let object::SectionFlags::Elf { sh_flags } =
            linked.section_by_name(".fstart.anchor").unwrap().flags()
        else {
            panic!("not ELF")
        };
        assert_ne!(sh_flags & u64::from(object::elf::SHF_WRITE), 0);
        fstart_image_build::elf::validate(&bytes, &expectations).unwrap();
        if let Some(copy) = &expectations.copy {
            let execution = copy.execution;
            let flat = scratch.0.join("relocated.bin");
            super::write_flat_binary(&elf, &flat).unwrap();
            let flat_bytes = fs::read(&flat).unwrap();
            let descriptor = linked.section_by_name(".fstart.layout").unwrap();
            let offset = usize::try_from(descriptor.address() - execution.base).unwrap();
            assert_eq!(
                &flat_bytes[offset..offset + descriptor.size() as usize],
                &expectations.descriptor.bytes
            );
            let binary_end = linked
                .symbols()
                .find(|s| s.name() == Ok("_binary_end"))
                .unwrap()
                .address();
            assert_eq!(binary_end - execution.base, flat_bytes.len() as u64);
            assert_eq!(
                linked.section_by_name(".page_tables").unwrap().size(),
                0x4000
            );
            assert!(
                expectations.runtime[1]
                    .contains(linked.section_by_name(".data").unwrap().address(), 16)
            );
            assert_eq!(&flat_bytes[flat_bytes.len() - 16..], &[7; 16]);
            assert_eq!(
                linked
                    .symbols()
                    .find(|s| s.name() == Ok("_data_load"))
                    .unwrap()
                    .address(),
                binary_end - 16
            );
            assert!(linked.section_by_name(".text").unwrap().size() >= 4096 + 32);
            fs::write(
                &script,
                format!("{}\n_binary_end = {};\n", script_text, binary_end - 8),
            )
            .unwrap();
            assert!(link().status.success());
            assert!(
                fstart_image_build::elf::validate(&fs::read(&elf).unwrap(), &expectations)
                    .unwrap_err()
                    .contains("copy extent")
            );
            let small = script_text.replace(
                &format!(
                    "EXEC (rx) : ORIGIN = {:#x}, LENGTH = {:#x}",
                    execution.base, execution.size
                ),
                &format!("EXEC (rx) : ORIGIN = {:#x}, LENGTH = 0x10", execution.base),
            );
            assert_ne!(small, script_text);
            fs::write(&script, small).unwrap();
            let overflow = link();
            assert!(!overflow.status.success());
            assert!(String::from_utf8_lossy(&overflow.stderr).contains("EXEC"));
        }
        if architecture == object::Architecture::Arm {
            assert!(!linked.is_64());
            assert!(
                crate::resolved_build::validate_elf(
                    &bytes,
                    &crate::resolved::tests::resolved("halt")
                )
                .unwrap_err()
                .contains("architecture")
            );
        }
    }
}
