// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use zerocopy::FromBytes;
fn image() -> PicassoImage {
    PicassoImage {
        flash_size: 0x1000000,
        efs_offset: 0xe20000,
        partition_end: 0x1000000,
        spi_read_mode: 4,
        spi_speed: 0,
        micron: false,
        soft_fuse: 0x30000000,
        psp: vec![Blob {
            key: Key::new(0, 0, 0),
            bytes: vec![0x12; 257],
        }],
        bios: vec![Blob {
            key: Key::new(0x60, 0, 0),
            bytes: vec![0x34; 4097],
        }],
        reset_image: vec![0x56; 0x20000],
        reset_destination: 0x2030000,
        apob_destination: 0x2001000,
        shared_memory: Some((0x2011000, 0x8000)),
        apob_nv: None,
        psp_nv: None,
    }
}
#[test]
fn typed_operation_packs_real_reset_input_without_a_fictitious_ffs_image() {
    use crate::build_plan::FileBinding;
    use assembly::{FileBlob, PicassoAssembly, PicassoFiles};
    let raw = image();
    let files = PicassoFiles {
        flash_size: raw.flash_size,
        efs_offset: raw.efs_offset,
        partition_end: raw.partition_end,
        spi_read_mode: raw.spi_read_mode,
        spi_speed: raw.spi_speed,
        micron: raw.micron,
        soft_fuse: raw.soft_fuse,
        psp: vec![FileBlob {
            key: raw.psp[0].key,
            path: "explicit/psp.bin".into(),
        }],
        bios: vec![FileBlob {
            key: raw.bios[0].key,
            path: "explicit/apcb.bin".into(),
        }],
        apob_nv: raw.apob_nv,
        psp_nv: raw.psp_nv,
    };
    let operation = PicassoAssembly {
        reset_unit: "reset".into(),
        configuration: FileBinding {
            environment: "IMAGE_CONFIG".into(),
            path: "image.json".into(),
            capacity: 65536,
        },
        files,
        reset_destination: raw.reset_destination,
        reset_size: raw.reset_image.len() as u32,
        apob_destination: raw.apob_destination,
        shared_memory: raw.shared_memory,
    };
    let json = serde_json::to_vec(&operation.files).unwrap();
    let sources = [
        json.as_slice(),
        raw.psp[0].bytes.as_slice(),
        raw.bios[0].bytes.as_slice(),
    ];
    let packed = operation.pack(&raw.reset_image, &sources).unwrap();
    let expected = raw.pack().unwrap();
    assert_eq!(packed.offset, 0xe20000);
    assert_eq!(packed.bytes, expected.bytes);
    assert_eq!(
        operation
            .bindings()
            .unwrap()
            .iter()
            .map(|file| file.environment.as_str())
            .collect::<Vec<_>>(),
        ["IMAGE_CONFIG", "PSP_00_00_00", "BIOS_60_00_00"]
    );
    assert!(
        operation
            .pack(&raw.reset_image[..0x1ffff], &sources)
            .is_err()
    );
    let mut changed = operation.clone();
    changed.files.soft_fuse ^= 1;
    assert!(
        changed
            .pack(&raw.reset_image, &sources)
            .unwrap_err()
            .contains("configuration changed")
    );
    let mut bad = operation.clone();
    bad.files.bios.push(bad.files.bios[0].clone());
    assert!(bad.bindings().is_err());
    bad = operation.clone();
    bad.files.bios[0].key.subprogram = 1;
    assert!(bad.bindings().is_err());
}

#[test]
fn directories_and_copy_to_dram() {
    let image = image();
    let packed = image.pack().unwrap();
    assert_eq!(packed.psp_directory, 0xe30000); // initial 64-KiB quirk
    let start = (packed.psp_directory - packed.offset) as usize;
    let h = Header::read_from_bytes(&packed.bytes[start..start + 16]).unwrap();
    assert_eq!(h.cookie.get(), PSP_COOKIE);
    assert_eq!(h.count.get(), 2);
    assert_eq!(
        h.checksum.get(),
        fletcher32(&packed.bytes[start + 8..start + 16 + 2 * 16])
    );
    let start = (packed.bios_directory - packed.offset) as usize;
    let h = Header::read_from_bytes(&packed.bytes[start..start + 16]).unwrap();
    let count = h.count.get() as usize;
    assert_eq!(count, 6); // PSB, APCB, backup APCB, APOB, reset image, shared memory
    assert_eq!(
        h.checksum.get(),
        fletcher32(&packed.bytes[start + 8..start + 16 + count * 24])
    );
    let entries: Vec<_> = packed.bytes[start + 16..start + 16 + count * 24]
        .chunks_exact(24)
        .map(|b| BiosEntry::read_from_bytes(b).unwrap())
        .collect();
    let bin = entries.iter().find(|e| e.kind == 0x62).unwrap();
    assert_eq!(bin.destination.get(), 0x2030000);
    assert_eq!(bin.size.get(), 0x20000);
    assert_eq!(bin.flags_instance, 3);
    let src = (bin.source.get() - 0xff000000 - u64::from(packed.offset)) as usize;
    assert_eq!(&packed.bytes[src..src + 0x20000], &image.reset_image);
    let apcb = entries.iter().find(|e| e.kind == 0x60).unwrap();
    let backup = entries.iter().find(|e| e.kind == 0x68).unwrap();
    assert_eq!(apcb.source, backup.source);
    assert_eq!(apcb.size.get(), 0x2000);
    let shared = entries.iter().find(|e| e.kind == 0x6b).unwrap();
    assert_eq!(shared.source.get(), u64::MAX);
    assert_eq!(
        &packed.bytes[0x14..0x18],
        &(0xff000000u32 + packed.psp_directory).to_le_bytes()
    );
    assert_eq!(&packed.bytes[0x24..0x28], &[0xff; 4]);
}
#[test]
fn bounds_and_duplicates_fail_closed() {
    let mut i = image();
    i.partition_end = i.efs_offset + 0x100;
    assert!(i.pack().is_err());
    let mut i = image();
    i.psp.push(i.psp[0].clone());
    assert!(i.pack().is_err());
    let mut i = image();
    i.apob_nv = Some(FlashRegion {
        offset: 0xe40000,
        size: 0x1000,
    });
    assert!(i.pack().is_err());
    let mut i = image();
    i.reset_image.pop();
    assert!(i.pack().is_err());
}
#[test]
fn apcb_subprograms_cannot_alias_generated_backup_selectors() {
    let mut i = image();
    i.bios.push(Blob {
        key: Key::new(0x60, 1, 0),
        bytes: vec![0x78; 0x1000],
    });
    assert!(i.pack().unwrap_err().contains("APCB selector"));
    i.bios[1].key.kind = 0x68;
    assert!(i.pack().unwrap_err().contains("APCB selector"));
}
#[test]
fn nv_physical_addresses_and_reserved_bits() {
    let mut i = image();
    i.apob_nv = Some(FlashRegion {
        offset: 0x200000,
        size: 0x1000,
    });
    i.psp_nv = Some(FlashRegion {
        offset: 0x202000,
        size: 0x1000,
    });
    let p = i.pack().unwrap();
    let s = (p.psp_directory - p.offset) as usize;
    let e = PspEntry::read_from_bytes(&p.bytes[s + 32..s + 48]).unwrap();
    assert_eq!(e.kind, 4);
    assert_eq!(e.address.get(), 0xff202000);
    assert_eq!(e.reserved, [0; 2]); // no Gen2 ROM Armor attributes
}
#[test]
fn config_is_strict_and_optional_files_are_not_read() {
    use std::path::Path;
    let text = "FIRMWARE_LOCATION blobs\nSOC_NAME Picasso\nPSP_SMUFW1_SUB2_FILE pco\nPSP_MP2FW1_FILE missing\nPSP_PMUI_FILE_SUB1_INS4 pmu\n";
    let files = config::parse(text, Path::new("root"), None, config::Options::default()).unwrap();
    assert_eq!(
        files.psp,
        vec![(Key::new(8, 2, 0), Path::new("root/blobs/pco").to_path_buf())]
    );
    assert_eq!(files.bios[0].0, Key::new(0x64, 1, 4));
    assert!(
        config::parse(
            &text.replace("Picasso", "Renoir"),
            Path::new("root"),
            None,
            config::Options::default()
        )
        .is_err()
    );
    assert!(
        config::parse(
            &(text.to_string() + "UNKNOWN_FILE x\n"),
            Path::new("root"),
            None,
            config::Options::default()
        )
        .is_err()
    );
}
#[test]
fn fletcher_reference_vectors() {
    assert_eq!(fletcher32(&[0; 8]), 0xffffffff);
    assert_eq!(fletcher32(&[1, 0, 2, 0, 3, 0, 4, 0]), 0x0014000a);
    assert_eq!(fletcher32(&vec![0xff; 1024]), 0xffffffff);
}
