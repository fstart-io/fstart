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
}
