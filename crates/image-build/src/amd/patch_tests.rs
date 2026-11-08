// SPDX-License-Identifier: MIT OR Apache-2.0
//! Synthetic mid-bank compressed reset, with data immediately after its
//! stored allocation but INSIDE the uncompressed entry's nominal range.
use super::{
    FlashRegion,
    assembly::{self, AmdAssembly, Firmware, VendorRegion},
};
use crate::build_plan::FileBinding;
use fstart_amdfw_format::{
    Profile,
    directory::{self, BiosEntry, PspEntry, bios, psp},
    efs::Efs,
};
use zerocopy::IntoBytes;

const RESET: usize = 0x404000;
const L1: usize = 0x410000;
const L2: usize = 0x400000;
fn fixture() -> (AmdAssembly, Vec<u8>) {
    let mut vendor = vec![0x69; 0x2000000];
    let mut original = vec![0; 0xc000];
    for (i, b) in original[..0x4000].iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    let stored = assembly::amd_compress(&original);
    vendor[RESET..RESET + stored.len()].copy_from_slice(&stored);
    let next = RESET + stored.len();
    let entries = [
        BiosEntry::new(
            bios::RESET_IMAGE,
            0,
            0,
            0xb,
            0xc000,
            RESET as u64,
            0x7044000,
        ),
        BiosEntry::new(bios::APOB, 0, 0, 0, 0, 0, 0x7001000),
        BiosEntry::new(bios::PMU_DATA, 0, 0, 0, 0x80, next as u64, u64::MAX),
    ];
    directory::write_table(&mut vendor[L2..], *b"$BL2", (1 << 29) | 0x10, &entries).unwrap();
    let mut l1 = entries.to_vec();
    l1.push(BiosEntry::new(
        bios::LEVEL2,
        0,
        0,
        0,
        0x10000,
        L2 as u64,
        u64::MAX,
    ));
    directory::write_table(&mut vendor[L1..], *b"$BHD", 1 << 29, &l1).unwrap();
    for offset in [0x30000, 0x31000] {
        directory::write_table(
            &mut vendor[offset..],
            *b"$PSP",
            1 << 29,
            &[
                PspEntry::new(1, 0, 0, 0x100, 0x50000),
                PspEntry::new(psp::BIOS_TABLE, 0, 0, 0x10000, L2 as u64),
            ],
        )
        .unwrap();
    }
    let mut efs = Efs::erased();
    efs.generation.set(0);
    efs.new_psp_directory.set(0x30000);
    efs.psp_backup_directory.set(0x31000);
    efs.bios_directory_mut(Profile::GENOA.bios).set(L1 as u32);
    vendor[0x20000..0x20060].copy_from_slice(efs.as_bytes());
    let plan = AmdAssembly {
        firmware: Firmware::Preserved {
            image: FileBinding {
                environment: "VENDOR_FLASH".into(),
                path: "explicit.bin".into(),
                capacity: vendor.len() as u64,
            },
            region: VendorRegion {
                origin: 0,
                size: 0x1000000,
                prefix_size: 0x1000000,
                profile: Profile::GENOA,
            },
            reset: Some(FlashRegion {
                offset: RESET as u32,
                size: stored.len() as u32,
            }),
            relocate: None,
        },
        ffs: None,
        stage_unit: "reset".into(),
        flash_size: vendor.len() as u32,
        bios_directory: Profile::GENOA.bios,
        reset_size: 0xc000,
        reset_destination: 0x7044000,
        apob_destination: 0x7001000,
        shared_memory: None,
        psp: Default::default(),
    };
    (plan, vendor)
}
#[test]
fn compressed_patch_preserves_directories_and_every_byte_outside_the_new_stream() {
    let (plan, vendor) = fixture();
    let reset = vec![0x5a; plan.reset_size as usize];
    plan.check_vendor(&vendor).unwrap();
    let result = plan.assemble(&[&vendor], &reset, None).unwrap();
    assert!(result.is_whole_flash(plan.flash_size));
    let stored = assembly::amd_compress(&reset);
    assert_eq!(&result.bytes[..RESET], &vendor[..RESET]);
    assert_eq!(
        &result.bytes[RESET + stored.len()..],
        &vendor[RESET + stored.len()..]
    );
    assert_eq!(
        assembly::amd_decompress(&result.bytes[RESET..], reset.len()).unwrap(),
        reset
    );
    assert_eq!(result.facts.reset_destination, 0x7044000);
    assert_eq!(result.facts.apob_destination, 0x7001000);
    let json = serde_json::to_string(&plan).unwrap();
    serde_json::from_str::<AmdAssembly>(&json)
        .unwrap()
        .check_vendor(&vendor)
        .unwrap();
}
#[test]
fn all_psp_branches_and_other_bios_slots_must_preserve_their_data() {
    let (plan, vendor) = fixture();
    for offset in [0x30000, 0x31000] {
        let mut bad = vendor.clone();
        directory::modify_entries::<PspEntry>(&mut bad, offset, |e| {
            if e.kind == 1 {
                e.address.set(RESET as u64);
            }
        })
        .unwrap();
        assert!(
            plan.check_vendor(&bad)
                .unwrap_err()
                .contains("PatchOverlap")
        );
    }
    let mut other = vendor.clone();
    let mut efs = Efs::read_from_bytes(&other[0x20000..0x20060]).unwrap();
    efs.bios0.set(0x440000);
    other[0x20000..0x20060].copy_from_slice(efs.as_bytes());
    directory::write_table(
        &mut other[0x440000..],
        *b"$BHD",
        1 << 29,
        &[BiosEntry::new(
            bios::MICROCODE,
            0,
            0,
            0,
            0x100,
            RESET as u64,
            u64::MAX,
        )],
    )
    .unwrap();
    assert!(
        plan.check_vendor(&other)
            .unwrap_err()
            .contains("PatchOverlap")
    );
}
#[test]
fn bad_alias_corrupt_stream_unknown_header_and_wrong_capacity_fail_closed() {
    let (plan, vendor) = fixture();
    let mut bad = vendor.clone();
    directory::modify_entries::<BiosEntry>(&mut bad, L1, |e| {
        if e.kind == bios::RESET_IMAGE {
            e.destination.set(0x7054000);
        }
    })
    .unwrap();
    assert!(plan.check_vendor(&bad).is_err());
    for offset in [0, 0x14, 256] {
        let mut bad = vendor.clone();
        bad[RESET + offset] ^= 1;
        assert!(plan.check_vendor(&bad).is_err());
    }
    let mut changed = plan.clone();
    let Firmware::Preserved {
        reset: Some(ref mut allocation),
        ..
    } = changed.firmware
    else {
        unreachable!()
    };
    allocation.size += 1;
    assert!(changed.check_vendor(&vendor).is_err());
    let mut random = vec![0; plan.reset_size as usize];
    let mut state = 0x12345678u32;
    for b in &mut random {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *b = state as u8;
    }
    assert!(
        plan.assemble(&[&vendor], &random, None)
            .unwrap_err()
            .contains("allocation")
    );
}

use zerocopy::FromBytes;

#[test]
fn explicit_relocation_keeps_old_reset_and_only_updates_selected_handoff_tables() {
    let (mut plan, mut vendor) = fixture();
    vendor[0x500000..0x600000].fill(0xff);
    let Firmware::Preserved { relocate, .. } = &mut plan.firmware else {
        unreachable!()
    };
    *relocate = Some(assembly::ResetRelocation {
        original_size: plan.reset_size,
        original_destination: plan.reset_destination,
        storage: FlashRegion {
            offset: 0x500000,
            size: 0x100000,
        },
    });
    plan.reset_size = 0x100000;
    plan.reset_destination = 0x8000000;
    let stage = vec![0xa5; plan.reset_size as usize];
    let image = plan.assemble(&[&vendor], &stage, None).unwrap();
    assert_eq!(image.facts.reset_offset, 0x500000);
    assert_eq!(image.facts.reset_size, 0x100000);
    assert_eq!(image.facts.reset_destination, 0x8000000);
    assert_eq!(
        assembly::amd_decompress(&image.bytes[0x500000..], stage.len()).unwrap(),
        stage
    );
    assert_eq!(
        &image.bytes[RESET..RESET + 0xc000],
        &vendor[RESET..RESET + 0xc000]
    );
    for (i, (&a, &b)) in image.bytes.iter().zip(&vendor).enumerate() {
        assert!(
            a == b
                || (0x500000..0x600000).contains(&i)
                || (L1..L1 + 0x100).contains(&i)
                || (L2..L2 + 0x100).contains(&i)
        );
    }
    let mut bad = vendor.clone();
    bad[0x500042] = 1;
    assert!(plan.check_vendor(&bad).unwrap_err().contains("erased"));
    let mut wrong = plan.clone();
    let Firmware::Preserved {
        relocate: Some(ref mut relocation),
        ..
    } = wrong.firmware
    else {
        unreachable!()
    };
    relocation.original_destination += 0x10000;
    assert!(wrong.check_vendor(&vendor).is_err());
}
