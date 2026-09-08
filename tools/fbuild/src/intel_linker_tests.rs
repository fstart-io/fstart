//! Synthetic ELF proof: fixed Intel placement, not execution or hardware proof.
use crate::intel_layout::{IntelStage, tests::candidate};
use object::{Object, ObjectSection, ObjectSymbol};
use std::{fs, path::PathBuf, process::Command};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn intel_fixed_linker_retains_descriptors_and_rejects_growth() {
    let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--print", "target-libdir"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let libdir = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let linker = libdir.parent().unwrap().join("bin/rust-lld");
    let scratch =
        Scratch(std::env::temp_dir().join(format!("fstart-intel-link-{}", std::process::id())));
    fs::create_dir_all(&scratch.0).unwrap();
    let object_path = scratch.0.join("fixture.o");
    let script = scratch.0.join("fixture.ld");
    let elf = scratch.0.join("fixture.elf");
    let flat = scratch.0.join("fixture.bin");
    let config = candidate();
    for (role, entry) in [
        (IntelStage::Bootblock, "_start"),
        (IntelStage::Postcar, "_start_postcar"),
        (IntelStage::Ramstage, "_start_ram"),
    ] {
        let stage = config.stage(role);
        let mut input = object::write::Object::new(
            object::BinaryFormat::Elf,
            object::Architecture::X86_64,
            object::Endianness::Little,
        );
        let text = input.add_section(
            Vec::new(),
            b".text.entry".to_vec(),
            object::SectionKind::Text,
        );
        input.append_section_data(text, &[0; 24], 8);

        for (offset, name, kind) in [
            (0, ".data", object::SectionKind::Data),
            (8, ".fstart.anchor", object::SectionKind::Data),
            (16, ".bss", object::SectionKind::UninitializedData),
        ] {
            let section = input.add_section(Vec::new(), name.as_bytes().to_vec(), kind);
            if kind == object::SectionKind::UninitializedData {
                input.append_section_bss(section, 64, 16);
            } else {
                input.append_section_data(section, &[7; 16], 16);
            }
            let symbol = input.section_symbol(section);
            input
                .add_relocation(
                    text,
                    object::write::Relocation {
                        offset,
                        symbol,
                        addend: 0,
                        flags: object::RelocationFlags::Elf {
                            r_type: object::elf::R_X86_64_64,
                        },
                    },
                )
                .unwrap();
        }
        let entry_section = if matches!(role, IntelStage::Bootblock) {
            let boot =
                input.add_section(Vec::new(), b".x86boot".to_vec(), object::SectionKind::Text);
            input.append_section_data(boot, &[0; 16], 16);
            let reset =
                input.add_section(Vec::new(), b".reset".to_vec(), object::SectionKind::Text);
            input.append_section_data(reset, &[0; 16], 16);
            reset
        } else {
            text
        };
        input.add_symbol(object::write::Symbol {
            name: entry.as_bytes().to_vec(),
            value: 0,
            size: 16,
            kind: object::SymbolKind::Text,
            scope: object::SymbolScope::Linkage,
            weak: false,
            section: object::write::SymbolSection::Section(entry_section),
            flags: object::SymbolFlags::None,
        });
        fs::write(&object_path, input.write().unwrap()).unwrap();
        let original = crate::linker::resolved_intel(&config, role, true).unwrap();
        fs::write(&script, &original).unwrap();
        let link = || {
            Command::new(&linker)
                .args(["-flavor", "gnu", "--gc-sections", "-T"])
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
            "{role:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = fs::read(&elf).unwrap();
        fstart_image_build::elf::validate(&bytes, &config.elf_expectations(role).unwrap()).unwrap();
        let wrong_role = if role == IntelStage::Bootblock {
            IntelStage::Postcar
        } else {
            IntelStage::Bootblock
        };
        assert!(
            fstart_image_build::elf::validate(
                &bytes,
                &config.elf_expectations(wrong_role).unwrap()
            )
            .is_err()
        );
        let linked = object::File::parse(&*bytes).unwrap();
        assert_eq!(
            linked.entry(),
            if matches!(role, IntelStage::Bootblock) {
                0xfffffff0
            } else {
                stage.image.base
            }
        );
        let symbol = |name| {
            linked
                .symbols()
                .find(|s| s.name() == Ok(name))
                .unwrap()
                .address()
        };
        assert_eq!(symbol("_stack_top"), stage.stack_span().base + stage.stack);
        assert_eq!(
            symbol("_FSTART_HEAP"),
            stage
                .heap_span()
                .map_or(stage.stack_span().base, |heap| heap.base)
        );
        assert_eq!(symbol("_data_start"), stage.writable.base);
        let descriptor = linked.section_by_name(".fstart.layout").unwrap();
        let object::SectionFlags::Elf { sh_flags } = descriptor.flags() else {
            panic!("not ELF")
        };
        assert_eq!(sh_flags & u64::from(object::elf::SHF_WRITE), 0);
        assert_eq!(
            descriptor.data().unwrap(),
            config.descriptor(role).unwrap().as_bytes()
        );
        crate::build_board::write_flat_binary(&elf, &flat).unwrap();
        let flat_bytes = fs::read(&flat).unwrap();
        let descriptor_offset = (descriptor.address() - stage.image.base) as usize;
        assert_eq!(
            &flat_bytes[descriptor_offset..descriptor_offset + descriptor.size() as usize],
            descriptor.data().unwrap()
        );
        if matches!(role, IntelStage::Bootblock) {
            assert_eq!(flat_bytes.len() as u64, stage.image.size);
            assert_eq!(
                linked.section_by_name(".reset").unwrap().address(),
                0xfffffff0
            );
        }
        // Initialized data stays in flash only for CAR; RAM stage flat images
        // include the gap up to the fixed writable initializer destination.
        let data_load = symbol("_data_load");
        let offset = (data_load - stage.image.base) as usize;
        assert_eq!(&flat_bytes[offset..offset + 16], &[7; 16]);
        assert!(if matches!(role, IntelStage::Bootblock) {
            stage.image.contains(data_load, 16)
        } else {
            data_load == stage.writable.base
        });
        let bloated = original.replace(
            "*(.bss .bss.* .lbss .lbss.*)",
            &format!(
                "*(.bss .bss.* .lbss .lbss.*) . += {:#x};",
                stage.writable.size
            ),
        );
        assert_ne!(bloated, original);
        fs::write(&script, bloated).unwrap();
        assert!(
            !link().status.success(),
            "{role:?} silently grew into heap/stack"
        );
        let bloated = original.replace(
            "*(.text .text.* .ltext .ltext.*)",
            &format!(
                "*(.text .text.* .ltext .ltext.*) . += {:#x};",
                stage.image.size
            ),
        );
        assert_ne!(bloated, original);
        fs::write(&script, bloated).unwrap();
        assert!(
            !link().status.success(),
            "{role:?} silently exceeded code capacity"
        );
    }
}
