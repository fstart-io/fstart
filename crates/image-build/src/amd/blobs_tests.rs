// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::amd::{Blob, PicassoImage};

/// `[package.metadata.fstart]` of a workspace Cargo.toml.
fn fstart_table(manifest: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(manifest);
    let text = std::fs::read_to_string(&path).unwrap();
    let value: toml::Value = toml::from_str(&text).unwrap();
    serde_json::to_value(&value["package"]["metadata"]["fstart"]).unwrap()
}

#[test]
fn picasso_and_vilboz_tables_resolve_in_a_blob_directory() {
    let soc: SocBlobs = serde_json::from_value(
        fstart_table("../platform-amd/Cargo.toml")["amd"]["picasso"].clone(),
    )
    .unwrap();
    let board_table = fstart_table("../../boards/google/zork/Cargo.toml");
    let board: BoardBlobs = serde_json::from_value(board_table["amd"].clone()).unwrap();
    assert_eq!(board.soc, "picasso");
    let name = board_table["board"].as_str().unwrap();
    let resolved = resolve(&soc, &board, name, Path::new("/blobs"), 0x100_0000).unwrap();
    let files = &resolved.files;
    assert_eq!(
        (files.efs_offset, files.partition_end),
        (0xe2_0000, 0x100_0000)
    );
    assert_eq!(
        files.psp[1].path,
        "/blobs/picasso/PSP/PspBootLoader_prod_RV.sbin"
    );
    // 8 PMU files, then every APCB instance with its backup.
    assert_eq!(files.bios.len(), 8 + 2 * 13);
    assert_eq!(files.bios[8].key, Key::new(bios::APCB, 0, 0));
    assert_eq!(files.bios[9].path, "/blobs/google-vilboz/apcb/0.bin");
    assert_eq!(resolved.microcode.len(), 3);
    assert_eq!(files.assets[1].name.as_str(), "pci1002,15dd.rom");
    assert!(files.psp_app);
    let mut escaping = soc.clone();
    escaping.psp[0].file = "../outside.bin".into();
    assert!(resolve(&escaping, &board, name, Path::new("/blobs"), 0x100_0000).is_err());
}

/// A vendor image with directories, a microcode patch and a video BIOS
/// outside them.
fn vendor_image() -> Vec<u8> {
    let packed = PicassoImage {
        flash_size: 0x100_0000,
        efs_offset: 0xe2_0000,
        partition_end: 0x100_0000,
        spi_read_mode: 4,
        spi_speed: 0,
        micron: false,
        soft_fuse: 0x3000_0000,
        psp: vec![Blob {
            key: Key::new(1, 0, 0),
            bytes: vec![0x11; 0x400],
        }],
        bios: vec![
            Blob {
                key: Key::new(0x64, 1, 4),
                bytes: vec![0x22; 0x300],
            },
            Blob {
                key: Key::new(bios::APCB, 0, 0),
                bytes: vec![0x33; 0x100],
            },
            Blob {
                key: Key::new(bios::APCB, 0, 1),
                bytes: vec![0x44; 0x100],
            },
        ],
        reset_image: vec![0x55; 0x1_0000],
        reset_slot: None,
        reset_destination: 0x300_0000,
        apob_destination: 0x200_1000,
        shared_memory: None,
        apob_nv: None,
        psp_nv: None,
    }
    .pack()
    .unwrap();
    let mut image = vec![0xff; 0x100_0000];
    image[packed.offset as usize..][..packed.bytes.len()].copy_from_slice(&packed.bytes);
    // A microcode patch for 0x8201, twice (identical), and an older one.
    let patch = |id: u32| {
        let mut p = vec![0u8; 0xc80];
        p[4..8].copy_from_slice(&id.to_le_bytes());
        p[0x18..0x1a].copy_from_slice(&0x8201u16.to_le_bytes());
        p[0x20] = 0x5a;
        p
    };
    image[0x10_0040..][..0xc80].copy_from_slice(&patch(0x0820_0105));
    image[0x20_0040..][..0xc80].copy_from_slice(&patch(0x0820_0105));
    image[0x30_0000..][..0xc80].copy_from_slice(&patch(0x0820_0100));
    // A 1-block video BIOS for 1002:15dd.
    let mut rom = vec![0u8; 512];
    rom[..3].copy_from_slice(&[0x55, 0xaa, 1]);
    rom[0x18..0x1a].copy_from_slice(&0x40u16.to_le_bytes());
    rom[0x40..0x44].copy_from_slice(b"PCIR");
    rom[0x44..0x48].copy_from_slice(&[0x02, 0x10, 0xdd, 0x15]);
    let sum = rom.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    rom[511] = sum.wrapping_neg();
    image[0x40_0080..][..512].copy_from_slice(&rom);
    image
}

#[test]
fn extracts_directory_files_microcode_and_vbios_from_a_vendor_image() {
    let image = vendor_image();
    let file = |kind, subprogram, instance, file: &str| DirectoryFile {
        kind,
        subprogram,
        instance,
        file: file.into(),
    };
    let soc = SocBlobs {
        bios_directory: BiosDirectorySlot::Family17hModel10,
        efs_offset: 0xe2_0000,
        partition_end: 0x100_0000,
        spi_read_mode: 4,
        spi_speed: 0,
        micron: false,
        soft_fuse: 0x3000_0000,
        psp: vec![file(1, 0, 0, "psp/boot.bin"), file(2, 0, 0, "psp/os.bin")],
        bios: vec![file(0x64, 1, 4, "bios/pmu.bin")],
        microcode: vec![MicrocodeFile {
            equivalence_id: 0x8201,
            file: "ucode/rv2.bin".into(),
        }],
        vbios: vec![VbiosFile {
            vendor: 0x1002,
            device: 0x15dd,
            file: "vbios/rv2.bin".into(),
        }],
    };
    let board = BoardBlobs {
        soc: "picasso".into(),
        psp_app: true,
        apcb: BoardApcb::Files { instances: 2 },
    };
    let (found, missing) = extract(&soc, &board, "board", &image).unwrap();
    assert_eq!(missing, ["psp/os.bin"]);
    let get = |name: &str| found.iter().find(|f| f.file == name).unwrap();
    assert_eq!(get("psp/boot.bin").bytes, [0x11; 0x400]);
    assert_eq!(get("bios/pmu.bin").bytes, [0x22; 0x300]);
    assert_eq!(get("board/apcb/1.bin").bytes[0], 0x44);
    // The newest patch, whichever copy.
    let patch = &get("ucode/rv2.bin");
    assert_eq!(patch.bytes[4..8], 0x0820_0105u32.to_le_bytes());
    assert_eq!(patch.origin, "flash offset 0x100040");
    assert_eq!(get("vbios/rv2.bin").origin, "flash offset 0x400080");
    assert_eq!(get("vbios/rv2.bin").bytes.len(), 512);
}
