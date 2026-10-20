// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
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

fn plan(generation: Generation, reset: ResetSlot) -> HalfPlan {
    HalfPlan {
        generation,
        reset,
        ffs_offset: FFS_OFFSET,
        rewrite_reset_geometry: false,
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
fn synthetic_half_prefix_geometry_and_composition() {
    let vendor = synthetic(Generation::Milan, MILAN);
    let layout = VendorLayout::parse(&vendor).unwrap();
    assert_eq!(layout.reset, MILAN);
    // The BIOS directory reserves 4 KiB (its max_size), beyond the PSP
    // blob at [0x110000, 0x118000).
    assert_eq!(layout.prefix_end, 0x20_1000);

    let ffs = vec![0xa5; 0x1000];
    let out = compose_half(
        &vendor,
        &plan(Generation::Milan, MILAN),
        &reset_image(MILAN),
        &ffs,
    )
    .unwrap();
    assert_eq!(out[..layout.prefix_end], vendor[..layout.prefix_end]);
    assert!(out[layout.prefix_end..0x60_0000].iter().all(|&b| b == 0xff));
    assert_eq!(out[0x60_0000..0x60_1000], ffs[..]);
    assert_eq!(out[HALF_SIZE - 16], 0xfa);
    // Unchanged geometry: directories are byte-identical.
    assert_eq!(out[0x20_0000..0x20_0040], vendor[0x20_0000..0x20_0040]);

    assert!(matches!(
        compose_half(
            &vendor,
            &plan(Generation::Rome, ROME),
            &reset_image(ROME),
            &ffs
        ),
        Err(ImageError::WrongGeneration { .. })
    ));
    let big = vec![0; MILAN.flash_offset as usize - 0x60_0000 + 1];
    assert!(matches!(
        compose_half(
            &vendor,
            &plan(Generation::Milan, MILAN),
            &reset_image(MILAN),
            &big
        ),
        Err(ImageError::FfsTooLarge { .. })
    ));
    assert!(matches!(
        compose_half(
            &vendor,
            &plan(Generation::Milan, MILAN),
            &vec![0; MILAN.size as usize],
            &ffs
        ),
        Err(ImageError::ResetImage(_))
    ));
    // The FFS may not start below the vendor prefix.
    let low = HalfPlan {
        ffs_offset: 0x20_0000,
        ..plan(Generation::Milan, MILAN)
    };
    assert!(matches!(
        compose_half(&vendor, &low, &reset_image(MILAN), &ffs),
        Err(ImageError::PrefixOverlap { .. })
    ));
}

#[test]
fn geometry_rewrite_is_opt_in_and_fixes_checksums() {
    // Milan v4.00 geometry in the vendor half, v4.04 in the build.
    let old = ResetSlot {
        flash_offset: 0xd0_0000,
        size: 0x30_0000,
        destination: 0x75d0_0000,
    };
    let vendor = synthetic(Generation::Milan, old);
    let mut p = plan(Generation::Milan, MILAN);
    assert_eq!(
        compose_half(&vendor, &p, &reset_image(MILAN), &[]),
        Err(ImageError::GeometryMismatch {
            vendor: old,
            build: MILAN
        })
    );
    p.rewrite_reset_geometry = true;
    let out = compose_half(&vendor, &p, &reset_image(MILAN), &[]).unwrap();
    assert_eq!(VendorLayout::parse(&out).unwrap().reset, MILAN);
    // Address-mode bits of the source survive; only the BIOS directory
    // (table and checksum) changed in the prefix.
    let e = 0x20_0000 + 16 + 24;
    assert_eq!(
        u64::from_le_bytes(out[e + 8..e + 16].try_into().unwrap()) >> 62,
        1
    );
    assert_eq!(out[..0x20_0004], vendor[..0x20_0004]);
    assert_eq!(out[0x20_0040..0x20_1000], vendor[0x20_0040..0x20_1000]);
}

#[test]
fn dual_image_order_follows_the_boot_rom_rule() {
    let rome = synthetic(Generation::Rome, ROME);
    let milan = synthetic(Generation::Milan, MILAN);
    let dual = compose_dual(&rome, &milan).unwrap();
    assert_eq!(booted_half(&dual, Generation::Rome), Some(FlashHalf::Lower));
    assert_eq!(
        booted_half(&dual, Generation::Milan),
        Some(FlashHalf::Upper)
    );
    // A Milan EFS below Rome would be taken by Rome's boot ROM.
    assert_eq!(
        compose_dual(&milan, &rome),
        Err(ImageError::BootOrder {
            half: FlashHalf::Upper,
            generation: Generation::Rome,
            booted: Some(FlashHalf::Lower)
        })
    );
    assert!(compose_dual(&rome, &rome).is_err());
    let erased = vec![0xff; HALF_SIZE];
    assert!(compose_dual(&rome, &erased).is_ok());
    assert!(replace_half(&dual, FlashHalf::Upper, &rome).is_err());
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

#[test]
fn compose_replaces_only_its_half_of_a_dual_vendor_rom() {
    let vendor = [
        synthetic(Generation::Rome, ROME),
        synthetic(Generation::Milan, MILAN),
    ]
    .concat();
    let ffs = ffs(0x1000);
    let out = compose(
        &vendor,
        FlashHalf::Upper,
        &plan(Generation::Milan, MILAN),
        &linked_reset(MILAN),
        &ffs,
    )
    .unwrap();
    let dual = out.dual.unwrap();
    assert_eq!(dual[..HALF_SIZE], vendor[..HALF_SIZE]);
    assert_eq!(dual[HALF_SIZE..], out.half[..]);
    assert_eq!(
        out.half[0x60_0000..0x60_1000],
        ffs[..],
        "FFS at the window start"
    );
    let reset = &out.half[MILAN.flash_offset as usize..];
    assert_eq!(reset[0x1000..0x1000 + TRUST_SIZE], trust());

    // A 16 MiB vendor half yields the half only.
    let half = compose(
        &vendor[..HALF_SIZE],
        FlashHalf::Lower,
        &plan(Generation::Rome, ROME),
        &linked_reset(ROME),
        &ffs,
    )
    .unwrap();
    assert!(half.dual.is_none());
    // The wrong half of the dual ROM for the generation.
    assert!(matches!(
        compose(
            &vendor,
            FlashHalf::Lower,
            &plan(Generation::Milan, MILAN),
            &linked_reset(MILAN),
            &ffs
        ),
        Err(ImageError::WrongGeneration { .. })
    ));
}

#[test]
fn half_plan_serializes_the_generation_by_name() {
    let json = serde_json::to_string(&plan(Generation::Milan, MILAN)).unwrap();
    assert!(json.contains("\"generation\":\"milan\""), "{json}");
    let back: HalfPlan = serde_json::from_str(&json).unwrap();
    assert_eq!(back, plan(Generation::Milan, MILAN));
    assert!(serde_json::from_str::<HalfPlan>(&json.replace("milan", "genoa")).is_err());
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
fn donor_composition_keeps_the_prefix_and_dual_order() {
    let Some(rom) = donor("tyan-s8030gm2ne/S8030GM2NE_BIOS_V4.04/8030V404.ROM") else {
        return;
    };
    check_dual(&rom).unwrap();
    let rome = select_half(&rom, FlashHalf::Lower).unwrap();
    let milan = select_half(&rom, FlashHalf::Upper).unwrap();
    let swapped = [milan, rome].concat();
    assert!(matches!(
        check_dual(&swapped),
        Err(ImageError::BootOrder {
            generation: Generation::Rome,
            ..
        })
    ));

    let ffs = ffs(0x10_0000);
    for (half, generation, slot) in [
        (FlashHalf::Lower, Generation::Rome, ROME),
        (FlashHalf::Upper, Generation::Milan, MILAN),
    ] {
        let out = compose(
            &rom,
            half,
            &plan(generation, slot),
            &linked_reset(slot),
            &ffs,
        )
        .unwrap_or_else(|e| panic!("{generation:?}: {e}"));
        let vendor = select_half(&rom, half).unwrap();
        let prefix = VendorLayout::parse(vendor).unwrap().prefix_end;
        assert_eq!(out.half[..prefix], vendor[..prefix]);
        // The AMI multi-tank descriptor (Rome half, 0x21000) is in the prefix.
        assert_eq!(out.half[0x2_1000..0x2_1058], vendor[0x2_1000..0x2_1058]);
        let dual = out.dual.unwrap();
        let other = match half {
            FlashHalf::Lower => FlashHalf::Upper,
            FlashHalf::Upper => FlashHalf::Lower,
        };
        assert_eq!(dual[other.range()], rom[other.range()]);
        assert_eq!(dual[half.range()], out.half[..]);
    }

    // Milan v4.00: different 0x62 geometry, refused unless rewritten.
    let Some(v400) = donor("tyan-s8030gm2ne/S8030GM2NE_bios_v400/8030V400.ROM") else {
        return;
    };
    check_dual(&v400).unwrap();
    let milan400 = select_half(&v400, FlashHalf::Upper).unwrap();
    assert!(matches!(
        compose_half(
            milan400,
            &plan(Generation::Milan, MILAN),
            &reset_image(MILAN),
            &ffs
        ),
        Err(ImageError::GeometryMismatch { .. })
    ));
    let rewrite = HalfPlan {
        rewrite_reset_geometry: true,
        ..plan(Generation::Milan, MILAN)
    };
    let out = compose_half(milan400, &rewrite, &reset_image(MILAN), &ffs).unwrap();
    assert_eq!(VendorLayout::parse(&out).unwrap().reset, MILAN);
}

#[test]
fn donor_naples_rome_dual_image() {
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
}
