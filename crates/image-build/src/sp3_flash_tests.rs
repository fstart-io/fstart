// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use fstart_driver_zen::psp::directory::fletcher32;
use std::path::Path;

const ROME: ResetSlot = ResetSlot {
    flash_offset: 0xdb_0000,
    size: 0x25_0000,
    destination: 0x75db_0000,
};
const MILAN: ResetSlot = ResetSlot {
    flash_offset: 0xce_0000,
    size: 0x32_0000,
    destination: 0x75ce_0000,
};
const FFS_OFFSET: u32 = 0x60_0000;

/// A minimal gen2 half: EFS, `$BHD` with APOB and 0x62, `$PSP` with a
/// blob and a soft fuse value entry.
fn synthetic(generation: Generation, reset: ResetSlot) -> Vec<u8> {
    let mut h = vec![0xff; HALF_SIZE];
    let put32 =
        |h: &mut Vec<u8>, at: usize, v: u32| h[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 =
        |h: &mut Vec<u8>, at: usize, v: u64| h[at..at + 8].copy_from_slice(&v.to_le_bytes());
    let (psp_dir, bios_dir) = (0x10_0000usize, 0x20_0000usize);
    put32(&mut h, 0x2_0000, 0x55aa_55aa);
    put32(&mut h, 0x2_0014, psp_dir as u32);
    put32(&mut h, 0x2_0024, generation_word(generation));
    let bios_ptr = match generation {
        Generation::Naples => unreachable!(),
        Generation::Rome => 0x2_0020,
        Generation::Milan => 0x2_0028,
    };
    put32(&mut h, bios_ptr, bios_dir as u32);
    // $PSP: blob 0x01 at 0x11_0000 (0x8000 B), soft fuse value.
    h[psp_dir..psp_dir + 4].copy_from_slice(b"$PSP");
    put32(&mut h, psp_dir + 8, 2);
    put32(&mut h, psp_dir + 12, 0x10); // 64 KiB reserved
    let e = psp_dir + 16;
    h[e] = 0x01;
    put32(&mut h, e + 4, 0x8000);
    put64(&mut h, e + 8, 0x11_0000);
    h[e + 16] = 0x0b;
    put32(&mut h, e + 16 + 4, 0xffff_ffff);
    put64(&mut h, e + 16 + 8, 1);
    let sum = fletcher32(&h[psp_dir + 8..psp_dir + 16 + 32]);
    put32(&mut h, psp_dir + 4, sum);
    // $BHD: APOB value entry, reset image.
    h[bios_dir..bios_dir + 4].copy_from_slice(b"$BHD");
    put32(&mut h, bios_dir + 8, 2);
    put32(&mut h, bios_dir + 12, 0x1 | 1 << 29);
    let e = bios_dir + 16;
    h[e] = bios::APOB;
    put32(&mut h, e + 4, 0);
    put64(&mut h, e + 8, 0);
    put64(&mut h, e + 16, 0x3000_0000);
    let e = e + 24;
    h[e] = bios::RESET_IMAGE;
    h[e + 2] = 0x3; // reset + copy
    put32(&mut h, e + 4, reset.size);
    put64(&mut h, e + 8, 1 << 62 | u64::from(reset.flash_offset));
    put64(&mut h, e + 16, reset.destination);
    let sum = fletcher32(&h[bios_dir + 8..bios_dir + 16 + 48]);
    put32(&mut h, bios_dir + 4, sum);
    h[0x11_0000..0x11_8000].fill(0x5a);
    h
}

fn reset_image(slot: ResetSlot) -> Vec<u8> {
    let mut r = vec![0u8; slot.size as usize];
    let n = r.len();
    r[n - 16..n - 12].copy_from_slice(&[0xfa, 0xe9, 0x00, 0x00]);
    r
}

/// The linked image: the smallest donor slot (Naples KNPP-D32).
const IMAGE: ResetSlot = ResetSlot {
    flash_offset: 0xde_0000,
    size: 0x22_0000,
    destination: 0x75de_0000,
};
/// The platform's early region ends here.
const DRAM_FLOOR: u64 = 0x75c0_0000;

fn plan(generations: &[Generation]) -> FlashPlan {
    FlashPlan {
        generations: generations.to_vec(),
        reset: IMAGE,
        ffs_offset: FFS_OFFSET,
        dram_floor: DRAM_FLOOR,
    }
}

fn trust() -> [u8; TRUST_SIZE] {
    let mut t = TrustBlock::placeholder();
    t.set_key_count(1);
    t.keys[0] = fstart_core::ffs::VerificationKey::ed25519(0, [7; 32]);
    t.image_family = [3; 16];
    let mut out = [0; TRUST_SIZE];
    t.write_to(&mut out);
    out
}

/// An FFS stand-in: a finalized trust copy at 0x100 and its own locator
/// at 0x800.
fn ffs(len: usize) -> Vec<u8> {
    let mut f = vec![0x11; len];
    f[0x100..0x100 + TRUST_SIZE].copy_from_slice(&trust());
    let mut l = LocatorBlock::placeholder();
    l.total_image_size.set(len as u32);
    l.anchor_offset.set(0x800);
    l.manifest_offset.set(0x900);
    l.write_to(&mut f[0x800..0x800 + LOCATOR_SIZE]);
    f
}

/// A reset image with both patch sites.
fn linked_reset(slot: ResetSlot) -> Vec<u8> {
    let mut r = reset_image(slot);
    TrustBlock::placeholder().write_to(&mut r[0x1000..0x1000 + TRUST_SIZE]);
    LocatorBlock::placeholder().write_to(&mut r[0x2000..0x2000 + LOCATOR_SIZE]);
    r
}

#[test]
fn half_gets_the_image_at_the_end_of_its_vendor_slot() {
    let vendor = synthetic(Generation::Milan, MILAN);
    let layout = VendorLayout::parse(&vendor).unwrap();
    assert_eq!(layout.reset, MILAN);
    // The BIOS directory reserves 4 KiB (its max_size), beyond the PSP
    // blob at [0x110000, 0x118000).
    assert_eq!(layout.prefix_end, 0x20_1000);

    let p = plan(&[Generation::Milan]);
    let ffs = vec![0xa5; 0x1000];
    let image = reset_image(IMAGE);
    let half = FlashHalf::Upper;
    let out = compose_half(half, &vendor, &p, &image, &ffs).unwrap();
    // Directories and everything else the PSP reads are untouched.
    assert_eq!(out[..layout.prefix_end], vendor[..layout.prefix_end]);
    assert!(out[layout.prefix_end..0x60_0000].iter().all(|&b| b == 0xff));
    assert_eq!(out[0x60_0000..0x60_1000], ffs[..]);
    // Slot front erased, the image in its last 0x220000 bytes.
    let (slot, at) = (MILAN.flash_offset as usize, IMAGE.flash_offset as usize);
    assert!(out[slot..at].iter().all(|&b| b == 0xff));
    assert_eq!(out[at..], image[..]);

    let refused = |vendor: &[u8], p: &FlashPlan, image: &[u8], ffs: &[u8]| {
        compose_half(half, vendor, p, image, ffs).unwrap_err()
    };
    let big = vec![0; MILAN.flash_offset as usize - 0x60_0000 + 1];
    assert!(matches!(
        refused(&vendor, &p, &image, &big),
        ImageError::FfsTooLarge { .. }
    ));
    assert!(matches!(
        refused(&vendor, &p, &reset_image(MILAN), &ffs),
        ImageError::ResetImage(_)
    ));
    assert!(matches!(
        refused(&vendor, &p, &vec![0; IMAGE.size as usize], &ffs),
        ImageError::ResetImage(_)
    ));
    // The FFS may not start below the vendor prefix.
    let low = FlashPlan {
        ffs_offset: 0x20_0000,
        ..p.clone()
    };
    assert!(matches!(
        refused(&vendor, &low, &image, &ffs),
        ImageError::PrefixOverlap { .. }
    ));
}

#[test]
fn vendor_slots_that_cannot_carry_the_image_are_refused() {
    let p = plan(&[Generation::Rome]);
    let image = reset_image(IMAGE);
    let reason = |slot: ResetSlot, p: &FlashPlan| match compose_half(
        FlashHalf::Lower,
        &synthetic(Generation::Rome, slot),
        p,
        &image,
        &[],
    ) {
        Err(ImageError::Slot { reason, .. }) => reason,
        other => panic!("{other:x?}"),
    };
    // Smaller than every donor slot, and so than the common image.
    let small = ResetSlot {
        flash_offset: 0xe0_0000,
        size: 0x20_0000,
        destination: 0x75e0_0000,
    };
    assert_eq!(reason(small, &p), "slot smaller than the reset image");
    let shifted = ResetSlot {
        destination: 0x75cf_0000,
        ..MILAN
    };
    assert_eq!(reason(shifted, &p), "DRAM copy ends elsewhere");
    let high_floor = FlashPlan {
        dram_floor: 0x75d0_0000,
        reset: ResetSlot {
            flash_offset: 0xe0_0000,
            size: 0x20_0000,
            destination: 0x75e0_0000,
        },
        ..p.clone()
    };
    assert_eq!(
        reason(MILAN, &high_floor),
        "slot reaches into the early region"
    );
    // Milan v4.00 geometry carries it.
    let v400 = ResetSlot {
        flash_offset: 0xd0_0000,
        size: 0x30_0000,
        destination: 0x75d0_0000,
    };
    let vendor = synthetic(Generation::Rome, v400);
    let out = compose_half(FlashHalf::Lower, &vendor, &p, &image, &[]).unwrap();
    assert_eq!(out[0x20_0000..0x20_1000], vendor[0x20_0000..0x20_1000]);
}

#[test]
fn dual_image_order_follows_the_boot_rom_rule() {
    let rome = synthetic(Generation::Rome, ROME);
    let milan = synthetic(Generation::Milan, MILAN);
    let dual = [&rome[..], &milan[..]].concat();
    check_dual(&dual).unwrap();
    assert_eq!(booted_half(&dual, Generation::Rome), Some(FlashHalf::Lower));
    assert_eq!(
        booted_half(&dual, Generation::Milan),
        Some(FlashHalf::Upper)
    );
    // A Milan EFS below Rome would be taken by Rome's boot ROM.
    assert_eq!(
        check_dual(&[&milan[..], &rome[..]].concat()),
        Err(ImageError::BootOrder {
            half: FlashHalf::Upper,
            generation: Generation::Rome,
            booted: Some(FlashHalf::Lower)
        })
    );
    assert!(check_dual(&[&rome[..], &rome[..]].concat()).is_err());
    let erased = vec![0xff; HALF_SIZE];
    assert!(check_dual(&[&rome[..], &erased[..]].concat()).is_ok());
}

#[test]
fn reset_image_gets_trust_and_locator_from_the_ffs() {
    let ffs = ffs(0x1000);
    let mut reset = linked_reset(ROME);
    let patched = patch_reset_image(&mut reset, &ffs).unwrap();
    assert_eq!(
        patched,
        Patched {
            trust: true,
            locator: true
        }
    );
    assert_eq!(reset[0x1000..0x1000 + TRUST_SIZE], trust());
    assert_eq!(
        reset[0x2000..0x2000 + LOCATOR_SIZE],
        ffs[0x800..0x800 + LOCATOR_SIZE]
    );

    // No sites, nothing to do; a site without an FFS counterpart fails.
    let mut plain = reset_image(ROME);
    assert_eq!(
        patch_reset_image(&mut plain, &ffs).unwrap(),
        Patched {
            trust: false,
            locator: false
        }
    );
    let mut reset = linked_reset(ROME);
    assert!(matches!(
        patch_reset_image(&mut reset, &[0; 0x1000]),
        Err(ImageError::Patch(_))
    ));
    // A locator for a different image size is not this FFS's anchor.
    assert!(patch_reset_image(&mut linked_reset(ROME), &ffs[..0xff8]).is_err());
}

/// One build into a Rome (lower) + Milan (upper) 32 MiB ROM.
fn check_both_halves(rom: &[u8], out: &Sp3Flash, ffs: &[u8]) {
    let image = &out.image;
    assert_eq!(image.len(), 2 * HALF_SIZE);
    check_dual(image).unwrap();
    let at = IMAGE.flash_offset as usize;
    let (lower, upper) = (&image[..HALF_SIZE], &image[HALF_SIZE..]);
    // The identical (patched) reset image at the top of both halves.
    assert_eq!(lower[at..], upper[at..]);
    assert_eq!(lower[at..at + 0x1000], linked_reset(IMAGE)[..0x1000]);
    assert_eq!(lower[at + 0x1000..at + 0x1000 + TRUST_SIZE], trust());
    for (half, bytes) in [(FlashHalf::Lower, lower), (FlashHalf::Upper, upper)] {
        let vendor = &rom[half.range()];
        let layout = VendorLayout::parse(vendor).unwrap();
        assert_eq!(bytes[..layout.prefix_end], vendor[..layout.prefix_end]);
        assert_eq!(VendorLayout::parse(bytes).unwrap(), layout);
        let slot = layout.reset.flash_offset as usize;
        assert!(bytes[slot..at].iter().all(|&b| b == 0xff), "{half:?}");
        assert_eq!(bytes[0x60_0000..0x60_0000 + ffs.len()], ffs[..]);
    }
}

#[test]
fn one_build_composes_every_supported_half() {
    let rom = [
        synthetic(Generation::Rome, ROME),
        synthetic(Generation::Milan, MILAN),
    ]
    .concat();
    let ffs = ffs(0x1000);
    let both = plan(&[Generation::Rome, Generation::Milan]);
    let out = compose(&rom, &both, &linked_reset(IMAGE), &ffs).unwrap();
    assert_eq!(out.survey.ffs_end, MILAN.flash_offset);
    assert_eq!(
        out.patched,
        Patched {
            trust: true,
            locator: true
        }
    );
    check_both_halves(&rom, &out, &ffs);

    // The common window ends at the lowest slot (Milan's), not Rome's.
    let window = (MILAN.flash_offset - FFS_OFFSET) as usize;
    assert!(matches!(
        compose(&rom, &both, &linked_reset(IMAGE), &self::ffs(window + 8)),
        Err(ImageError::FfsTooLarge { capacity, .. }) if capacity == window
    ));

    // A Rome-only board keeps the Milan half's vendor bytes.
    let rome = compose(&rom, &plan(&[Generation::Rome]), &linked_reset(IMAGE), &ffs).unwrap();
    assert_eq!(rome.survey.ffs_end, ROME.flash_offset);
    assert_eq!(
        rome.survey.halves[1],
        (FlashHalf::Upper, HalfAction::Keep(Some(Generation::Milan)))
    );
    assert_eq!(rome.image[HALF_SIZE..], rom[HALF_SIZE..]);
    assert_eq!(rome.image[..HALF_SIZE], out.image[..HALF_SIZE]);
    // A 16 MiB vendor half yields a 16 MiB image.
    let half = compose(&rom[..HALF_SIZE], &both, &linked_reset(IMAGE), &ffs).unwrap();
    assert_eq!(half.image, out.image[..HALF_SIZE]);
    assert_eq!(
        compose(
            &rom,
            &plan(&[Generation::Naples]),
            &linked_reset(IMAGE),
            &ffs
        ),
        Err(ImageError::NoSupportedHalf)
    );
}

#[test]
fn flash_plan_serializes_generations_by_name() {
    let p = plan(&[Generation::Rome, Generation::Milan]);
    let json = serde_json::to_string(&p).unwrap();
    assert!(
        json.contains("\"generations\":[\"rome\",\"milan\"]"),
        "{json}"
    );
    let back: FlashPlan = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
    assert!(serde_json::from_str::<FlashPlan>(&json.replace("milan", "genoa")).is_err());
}

// ---- donor ROMs (skipped when the analysis workspace is absent) ----

const DONORS: &str = "../../../sp3/firmware";

fn donor(path: &str) -> Option<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(DONORS)
        .join(path);
    std::fs::read(&path).ok().or_else(|| {
        eprintln!("skipping: {} not present", path.display());
        None
    })
}

#[test]
fn donor_halves_match_the_firmware_image_report() {
    // (donor, generation, reset slot, prefix end) per FIRMWARE_IMAGE §2.1/§4.
    let rows = [
        (
            "naples-s8026-v202.bin",
            Generation::Naples,
            (0xdb_0000, 0x25_0000, 0x75db_0000),
            0x27_3000,
        ),
        (
            "naples2p-knppd32-1801.bin",
            Generation::Naples,
            (0xde_0000, 0x22_0000, 0x75de_0000),
            0x4b_a000,
        ),
        (
            "rome-s8026-v202.bin",
            Generation::Rome,
            (0xdb_0000, 0x25_0000, 0x75db_0000),
            0x50_b000,
        ),
        (
            "rome-s8030-v404.bin",
            Generation::Rome,
            (0xdb_0000, 0x25_0000, 0x75db_0000),
            0x54_b000,
        ),
        (
            "rome2p-s8253-v203.bin",
            Generation::Rome,
            (0xdb_0000, 0x25_0000, 0x75db_0000),
            0x54_b000,
        ),
        (
            "milan-s8030-v404.bin",
            Generation::Milan,
            (0xce_0000, 0x32_0000, 0x75ce_0000),
            0x58_6000,
        ),
        (
            "milan2p-s8253-v203.bin",
            Generation::Milan,
            (0xce_0000, 0x32_0000, 0x75ce_0000),
            0x58_6000,
        ),
    ];
    for (name, generation, (flash_offset, size, destination), prefix_end) in rows {
        let Some(half) = donor(&format!("donors/{name}")) else {
            continue;
        };
        let layout = VendorLayout::parse(&half).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(layout.generation, generation, "{name}");
        assert_eq!(
            layout.reset,
            ResetSlot {
                flash_offset,
                size,
                destination
            },
            "{name}"
        );
        assert_eq!(layout.prefix_end, prefix_end, "{name}");
    }
}

#[test]
fn donor_s8030_one_build_in_both_halves() {
    let Some(rom) = donor("tyan-s8030gm2ne/S8030GM2NE_BIOS_V4.04/8030V404.ROM") else {
        return;
    };
    check_dual(&rom).unwrap();
    let swapped = [&rom[HALF_SIZE..], &rom[..HALF_SIZE]].concat();
    assert!(matches!(
        check_dual(&swapped),
        Err(ImageError::BootOrder {
            generation: Generation::Rome,
            ..
        })
    ));

    let ffs = ffs(0x10_0000);
    let both = plan(&[Generation::Rome, Generation::Milan]);
    let out = compose(&rom, &both, &linked_reset(IMAGE), &ffs).unwrap();
    assert_eq!(out.survey.ffs_end, 0xce_0000);
    check_both_halves(&rom, &out, &ffs);
    // The AMI multi-tank descriptor (Rome half, 0x21000) is in the prefix.
    assert_eq!(out.image[0x2_1000..0x2_1058], rom[0x2_1000..0x2_1058]);

    // Milan v4.00: a 0xd00000 slot carries the same image.
    let Some(v400) = donor("tyan-s8030gm2ne/S8030GM2NE_bios_v400/8030V400.ROM") else {
        return;
    };
    let out = compose(&v400, &both, &linked_reset(IMAGE), &ffs).unwrap();
    assert_eq!(out.survey.ffs_end, 0xd0_0000);
    check_both_halves(&v400, &out, &ffs);
}

#[test]
fn donor_s8026_naples_and_rome_halves() {
    let Some(rom) = donor("tyan-s8026/x/8026V202.ROM") else {
        return;
    };
    check_dual(&rom).unwrap();
    assert_eq!(
        booted_half(&rom, Generation::Naples),
        Some(FlashHalf::Lower)
    );
    assert_eq!(booted_half(&rom, Generation::Rome), Some(FlashHalf::Upper));
    assert_eq!(booted_half(&rom, Generation::Milan), None);

    let ffs = ffs(0x10_0000);
    let out = compose(
        &rom,
        &plan(&[Generation::Naples, Generation::Rome]),
        &linked_reset(IMAGE),
        &ffs,
    )
    .unwrap();
    assert!(
        out.survey
            .halves
            .iter()
            .all(|(_, a)| matches!(a, HalfAction::Compose(_)))
    );
    assert_eq!(out.survey.ffs_end, ROME.flash_offset);
    check_dual(&out.image).unwrap();
    let at = IMAGE.flash_offset as usize;
    assert_eq!(out.image[at..HALF_SIZE], out.image[HALF_SIZE + at..]);
    for half in [FlashHalf::Lower, FlashHalf::Upper] {
        let vendor = &rom[half.range()];
        let prefix = VendorLayout::parse(vendor).unwrap().prefix_end;
        assert_eq!(out.image[half.range()][..prefix], vendor[..prefix]);
    }
}

#[test]
fn donor_knpp_naples_slot_is_exactly_the_image() {
    let Some(half) = donor("donors/naples2p-knppd32-1801.bin") else {
        return;
    };
    let ffs = ffs(0x10_0000);
    let out = compose(
        &half,
        &plan(&[Generation::Naples]),
        &linked_reset(IMAGE),
        &ffs,
    )
    .unwrap();
    assert_eq!(out.image.len(), HALF_SIZE);
    assert_eq!(out.survey.ffs_end, IMAGE.flash_offset);
    let layout = VendorLayout::parse(&half).unwrap();
    assert_eq!(layout.generation, Generation::Naples);
    assert_eq!(layout.reset, IMAGE);
    assert_eq!(out.image[..layout.prefix_end], half[..layout.prefix_end]);
    assert_eq!(VendorLayout::parse(&out.image).unwrap(), layout);
    assert_eq!(out.image[0x60_0000..0x70_0000], ffs[..]);
    let at = IMAGE.flash_offset as usize;
    assert_eq!(out.image[at + 0x1000..at + 0x1000 + TRUST_SIZE], trust());
}
