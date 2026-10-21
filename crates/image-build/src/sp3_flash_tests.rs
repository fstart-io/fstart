// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use fstart_driver_zen::psp::directory::{Directories, Directory};
use std::path::Path;

/// The linked reset image (Naples KNPP-D32's slot, the smallest donor's).
const IMAGE: ResetSlot = ResetSlot {
    flash_offset: 0xde_0000,
    size: 0x22_0000,
    destination: 0x75de_0000,
};
/// The platform's early region ends here.
const DRAM_FLOOR: u64 = 0x75c0_0000;
const FFS_OFFSET: u32 = 0x60_0000;
const HALF: usize = HALF_SIZE;

/// Vendor `0x62` slots (FIRMWARE_IMAGE §2.1).
const fn vendor_slot(generation: Generation) -> ResetSlot {
    match generation {
        Generation::Milan => ResetSlot {
            flash_offset: 0xce_0000,
            size: 0x32_0000,
            destination: 0x75ce_0000,
        },
        _ => ResetSlot {
            flash_offset: 0xdb_0000,
            size: 0x25_0000,
            destination: 0x75db_0000,
        },
    }
}

fn psp_entry(kind: u8, size: u32, location: u64) -> Vec<u8> {
    let mut e = vec![kind, 0, 0, 0];
    e.extend(size.to_le_bytes());
    e.extend(location.to_le_bytes());
    e
}

fn bios_entry(kind: u8, flags: u8, size: u32, location: u64, destination: u64) -> Vec<u8> {
    let mut e = vec![kind, 0, flags, 0];
    e.extend(size.to_le_bytes());
    e.extend(location.to_le_bytes());
    e.extend(destination.to_le_bytes());
    e
}

fn put_dir(h: &mut [u8], at: usize, cookie: &[u8; 4], info: u32, entries: &[Vec<u8>]) {
    let mut d = cookie.to_vec();
    d.extend([0; 4]);
    d.extend((entries.len() as u32).to_le_bytes());
    d.extend(info.to_le_bytes());
    entries.iter().for_each(|e| d.extend(e));
    let sum = fletcher32(&d[8..]);
    d[4..8].copy_from_slice(&sum.to_le_bytes());
    h[at..at + d.len()].copy_from_slice(&d);
}

const PSP: usize = 0x10_0000;
const PL2: usize = 0x10_1000;
const BHD: usize = 0x20_0000;
const BL2: usize = 0x20_1000;

/// A vendor half: EFS, `$PSP` (blob, soft fuse, `0x40` -> `$PL2` with a
/// blob), `$BHD` (APOB, `0x62`, `0x70` -> `$BL2` with microcode and
/// `0x62`). Naples: gen1 headers and MMIO pointers; Rome: offsets, mode 0;
/// Milan: mode 1. Blob bytes differ per generation.
fn synthetic(generation: Generation) -> Vec<u8> {
    let mut h = vec![0xff; HALF];
    let gen1 = generation == Generation::Naples;
    let ptr = |off: usize| off as u64 + if gen1 { MMIO_BASE } else { 0 };
    let info = match generation {
        Generation::Naples => 0,
        Generation::Rome => 0x10,
        Generation::Milan => 0x10 | 1 << 29,
    };
    put32(&mut h, EFS_OFFSET, EFS_SIGNATURE);
    put32(&mut h, EFS_OFFSET + EFS_PSP, ptr(PSP) as u32);
    put32(&mut h, EFS_OFFSET + EFS_WORD, generation_word(generation));
    put32(&mut h, EFS_OFFSET + bios_field(generation), ptr(BHD) as u32);
    let slot = vendor_slot(generation);
    let reset = || {
        bios_entry(
            bios::RESET_IMAGE,
            3,
            slot.size,
            ptr(slot.flash_offset as usize),
            slot.destination,
        )
    };
    put_dir(
        &mut h,
        PSP,
        b"$PSP",
        info,
        &[
            psp_entry(0x00, 0x8000, ptr(0x11_0000)),
            psp_entry(psp::SOFT_FUSE, u32::MAX, 1),
            psp_entry(psp::L2, 0x400, ptr(PL2)),
        ],
    );
    put_dir(
        &mut h,
        PL2,
        b"$PL2",
        info,
        &[psp_entry(0x01, 0x1000, ptr(0x11_8000))],
    );
    put_dir(
        &mut h,
        BHD,
        b"$BHD",
        info,
        &[
            bios_entry(bios::APOB, 0, 0, 0, 0x3000_0000),
            reset(),
            bios_entry(bios::L2, 0, 0x400, ptr(BL2), u64::MAX),
        ],
    );
    put_dir(
        &mut h,
        BL2,
        b"$BL2",
        info,
        &[
            bios_entry(bios::MICROCODE, 0, 0xc80, ptr(0x20_2000), u64::MAX),
            reset(),
        ],
    );
    let g = generation as u8;
    h[0x11_0000..0x11_8000].fill(0x10 + g);
    h[0x11_8000..0x11_9000].fill(0x20 + g);
    h[0x20_2000..0x20_2c80].fill(0x30 + g);
    h[slot.flash_offset as usize..].fill(0xee);
    h
}

fn plan(generations: &[Generation], layout: Layout) -> FlashPlan {
    FlashPlan {
        generations: generations.to_vec(),
        reset: IMAGE,
        ffs_offset: FFS_OFFSET,
        dram_floor: DRAM_FLOOR,
        layout,
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

/// A reset image with all three patch sites and a reset vector.
fn linked_reset() -> Vec<u8> {
    let mut r = vec![0u8; IMAGE.size as usize];
    let n = r.len();
    r[n - 16..n - 12].copy_from_slice(&[0xfa, 0xe9, 0x00, 0x00]);
    TrustBlock::placeholder().write_to(&mut r[0x1000..0x1000 + TRUST_SIZE]);
    LocatorBlock::placeholder().write_to(&mut r[0x2000..0x2000 + LOCATOR_SIZE]);
    r[0x3000..0x3000 + ANCHOR_SIZE].copy_from_slice(FlashAnchor::placeholder().as_bytes());
    r
}

/// Every level and entry of `out` against `vendor`: same entries, same
/// data (directories by cookie only, their pointers move); `reset` checks
/// the `0x62` entries instead.
fn same_tree<E: Entry>(
    out: &Directories<'_, E>,
    vendor: &Directories<'_, E>,
    mut reset: impl FnMut(&Directory<'_, E>, &E) -> bool,
) {
    let (o, v): (Vec<_>, Vec<_>) = (out.levels().collect(), vendor.levels().collect());
    assert_eq!(o.len(), v.len());
    for (od, vd) in o.into_iter().zip(v) {
        assert_eq!(od.cookie(), vd.cookie());
        assert_eq!(od.entries().len(), vd.entries().len());
        for (oe, ve) in od.entries().iter().zip(vd.entries()) {
            assert_eq!((oe.kind(), oe.instance()), (ve.kind(), ve.instance()));
            if reset(od, oe) {
                continue;
            }
            match (od.data(oe), vd.data(ve)) {
                (Ok(a), Ok(b)) if Kind::of(&b[..4.min(b.len())]).is_some() => {
                    assert_eq!(a[..4], b[..4])
                }
                (Ok(a), Ok(b)) => assert!(a == b, "entry {:#x} differs", oe.kind()),
                (a, b) => assert_eq!(a.err(), b.err()),
            }
        }
    }
}

/// From the composed image's EFS (as the boot ROM finds it) `generation`'s
/// PSP and BIOS trees resolve to `vendor`'s bytes, and every `0x62` names
/// the reset image at flash `reset_at`.
fn check_trees(image: &[u8], vendor: &[u8], generation: Generation, reset_at: usize) {
    let half = booted_half_offset(image, generation).unwrap();
    let out = Efs::new(&image[half..]).unwrap();
    let ven = Efs::new(vendor).unwrap();
    same_tree(
        &out.psp_directories(generation).unwrap(),
        &ven.psp_directories(generation).unwrap(),
        |_, _| false,
    );
    let mut resets = 0;
    same_tree(
        &out.bios_directories(generation).unwrap(),
        &ven.bios_directories(generation).unwrap(),
        |dir, e| {
            if e.kind() != bios::RESET_IMAGE {
                return false;
            }
            let range = dir.range(e).unwrap();
            assert_eq!(
                half + range.start..half + range.end,
                reset_at..reset_at + 0x22_0000
            );
            assert_eq!(e.destination(), IMAGE.destination);
            assert!(e.reset() && e.copy() && !e.compressed());
            resets += 1;
            true
        },
    );
    assert!(resets >= 2, "{generation:?}: L1 and L2 0x62");
}

/// The patched reset image at `at` and the FFS once, at `ffs_at`.
fn check_payloads(image: &[u8], at: usize, ffs: &[u8], ffs_at: usize) {
    let reset = &image[at..at + IMAGE.size as usize];
    assert_eq!(reset[0x1000..0x1000 + TRUST_SIZE], trust());
    assert_eq!(
        reset[0x2000..0x2000 + LOCATOR_SIZE],
        ffs[0x800..0x800 + LOCATOR_SIZE]
    );
    let anchor = FlashAnchor::parse(&reset[0x3000..0x3000 + ANCHOR_SIZE]).unwrap();
    assert_eq!(
        (
            anchor.ffs_offset.get() as usize,
            anchor.ffs_size.get() as usize
        ),
        (ffs_at, ffs.len())
    );
    assert_eq!(anchor.flash_size.get() as usize, image.len());
    assert_eq!(image[ffs_at..ffs_at + ffs.len()], *ffs);
    let copies = (0..image.len() - 0x1000)
        .step_by(0x1_0000)
        .filter(|&o| image[o + 0x800..o + 0x800 + LOCATOR_SIZE] == ffs[0x800..0x800 + LOCATOR_SIZE])
        .count();
    assert_eq!(copies, 1, "one FFS");
}

#[test]
fn unified_rome_milan_shares_one_efs() {
    let (rome, milan) = (synthetic(Generation::Rome), synthetic(Generation::Milan));
    let rom = [&rome[..], &milan[..]].concat();
    let ffs = ffs(0x1_0000);
    let both = plan(&[Generation::Rome, Generation::Milan], Layout::Unified);
    let out = compose(&rom, &both, &linked_reset(), &ffs).unwrap();
    let image = &out.image;
    // Prefix end: the $BL2's 64 KiB reservation.
    assert_eq!(out.survey.combo, Some(0x21_1000));
    assert_eq!(out.survey.ffs, 0x60_0000..0xde_0000);
    let efs = |off: usize| get32(image, EFS_OFFSET + off);
    assert_eq!(
        [efs(EFS_PSP), efs(0x20), efs(0x28), efs(EFS_WORD)],
        [0x21_1000, BHD as u32, (HALF + BHD) as u32, 0xffff_fffc]
    );
    assert_eq!(
        get32(image, HALF + EFS_OFFSET),
        u32::MAX,
        "upper EFS erased"
    );
    let combo = Combo::parse(image, 0x21_1000).unwrap().unwrap();
    assert_eq!(combo.lookup_mode(), 0);
    let entries: Vec<_> = combo
        .entries()
        .map(|e| (e.id_select, e.id, e.directory))
        .collect();
    assert_eq!(
        entries,
        [
            (0, 0xbc0b_0000, PSP as u64),
            (0, 0xbc0b_0d00, (HALF + PSP) as u64)
        ]
    );
    for (generation, vendor) in [(Generation::Rome, &rome), (Generation::Milan, &milan)] {
        assert_eq!(booted_half(image, generation), Some(FlashHalf::Lower));
        check_trees(image, vendor, generation, 0xde_0000);
    }
    check_payloads(image, 0xde_0000, &ffs, 0x60_0000);
    // Outside the prefixes, the 2PSP, the FFS and the reset image: erased.
    assert!(image[0x21_2000..0x60_0000].iter().all(|&b| b == 0xff));
    assert!(image[HALF + 0x21_1000..].iter().all(|&b| b == 0xff));
}

#[test]
fn dual_keeps_each_vendor_efs_with_its_own_reset_copy() {
    let (rome, milan) = (synthetic(Generation::Rome), synthetic(Generation::Milan));
    let rom = [&rome[..], &milan[..]].concat();
    let ffs = ffs(0x1_0000);
    let both = plan(&[Generation::Rome, Generation::Milan], Layout::Dual);
    let out = compose(&rom, &both, &linked_reset(), &ffs).unwrap();
    let image = &out.image;
    assert_eq!(out.survey.combo, None);
    assert_eq!(out.survey.ffs, 0x60_0000..0xde_0000);
    check_dual(image).unwrap();
    // The EFS are the vendor's.
    assert_eq!(
        image[EFS_OFFSET..EFS_OFFSET + 0x40],
        rom[EFS_OFFSET..EFS_OFFSET + 0x40]
    );
    let upper = HALF + EFS_OFFSET;
    assert_eq!(image[upper..upper + 0x40], rom[upper..upper + 0x40]);
    check_trees(image, &rome, Generation::Rome, 0xde_0000);
    check_trees(image, &milan, Generation::Milan, HALF + 0xde_0000);
    check_payloads(image, 0xde_0000, &ffs, 0x60_0000);
    assert_eq!(image[0xde_0000..HALF], image[HALF + 0xde_0000..]);
    assert!(
        image[HALF + 0x21_1000..HALF + 0xde_0000]
            .iter()
            .all(|&b| b == 0xff)
    );
}

#[test]
fn unified_naples_rome_keeps_naples_mmio_and_moves_rome_to_mode_1() {
    let (naples, rome) = (synthetic(Generation::Naples), synthetic(Generation::Rome));
    let rom = [&naples[..], &rome[..]].concat();
    let ffs = ffs(0x1_0000);
    let both = plan(&[Generation::Naples, Generation::Rome], Layout::Unified);
    let out = compose(&rom, &both, &linked_reset(), &ffs).unwrap();
    let image = &out.image;
    // Naples' prefix has no reserved directory area: 0x202c80.
    let combo_at = 0x20_3000;
    assert_eq!(out.survey.combo, Some(combo_at));
    assert_eq!(get32(image, EFS_OFFSET + EFS_WORD), 0xffff_fffe);
    assert_eq!(get32(image, EFS_OFFSET + 0x18), 0xff20_0000);
    assert_eq!(get32(image, EFS_OFFSET + 0x20), (HALF + BHD) as u32);
    let combo = Combo::parse(image, combo_at).unwrap().unwrap();
    let dirs: Vec<_> = combo.entries().map(|e| (e.id, e.directory)).collect();
    assert_eq!(
        dirs,
        [
            (0xbc09_0000, 0xff10_0000),
            (0xbc0b_0000, (HALF + PSP) as u64)
        ]
    );
    // The rebased Rome directories hold 32 MiB offsets: address mode 1.
    for dir in [PSP, PL2, BHD, BL2] {
        assert_eq!(get32(image, HALF + dir + 12) >> 29 & 3, 1);
    }
    check_trees(image, &naples, Generation::Naples, 0xde_0000);
    check_trees(image, &rome, Generation::Rome, 0xde_0000);
    // Naples' 0x62 stays in MMIO form.
    let naples_reset = BHD + 16 + 24;
    assert_eq!(get64(image, naples_reset + 8), 0xffde_0000);
}

#[test]
fn layouts_the_composer_refuses() {
    let (naples, rome, milan) = (
        synthetic(Generation::Naples),
        synthetic(Generation::Rome),
        synthetic(Generation::Milan),
    );
    let ffs = ffs(0x1_0000);
    let all = [Generation::Naples, Generation::Rome, Generation::Milan];
    let compose_with =
        |rom: &[u8], p: &FlashPlan, ffs: &[u8]| compose(rom, p, &linked_reset(), ffs);
    // MMIO pointers only reach the lower half.
    let rom = [&rome[..], &naples[..]].concat();
    assert!(matches!(
        compose_with(&rom, &plan(&all, Layout::Unified), &ffs),
        Err(ImageError::UpperMmio { .. })
    ));
    let rom = [&rome[..], &milan[..]].concat();
    let unified = plan(&all, Layout::Unified);
    // The FFS may not reach the reset image.
    assert_eq!(
        compose_with(&rom, &unified, &vec![0; 0x7e_0001]),
        Err(ImageError::FfsTooLarge {
            size: 0x7e_0001,
            capacity: 0x7e_0000
        })
    );
    // Nor may a vendor prefix (a blob moved up to 0xde8000).
    let mut late = rome.clone();
    put64(&mut late, PL2 + 16 + 8, 0xde_8000);
    let sum = fletcher32(&late[PL2 + 8..PL2 + 32]);
    put32(&mut late, PL2 + 4, sum);
    assert!(matches!(
        compose_with(&[&late[..], &milan[..]].concat(), &unified, &ffs),
        Err(ImageError::PrefixOverlap {
            limit: 0xde_0000,
            ..
        })
    ));
    // The FFS lives in the lower half: its generation must be supported.
    assert!(matches!(
        compose_with(&rom, &plan(&[Generation::Milan], Layout::Dual), &ffs),
        Err(ImageError::Layout(_))
    ));
    assert!(matches!(
        compose_with(&[&rome[..], &rome[..]].concat(), &unified, &ffs),
        Err(ImageError::Layout(_))
    ));
    // An EFS signature where a boot ROM searches before 0x20000.
    let mut stray = self::ffs(0x22_0004);
    put32(&mut stray, 0x82_0000 - 0x60_0000, EFS_SIGNATURE);
    assert_eq!(
        compose_with(&rom, &unified, &stray),
        Err(ImageError::StrayEfs { offset: 0x82_0000 })
    );
    let checksum = |mut half: Vec<u8>| {
        half[BL2 + 16] ^= 1;
        compose_with(&[&rome[..], &half[..]].concat(), &unified, &ffs)
    };
    assert!(matches!(
        checksum(milan.clone()),
        Err(ImageError::Directory(DirError::Checksum {
            offset: BL2,
            ..
        }))
    ));
}

#[test]
fn single_generation_and_kept_halves() {
    let (rome, milan) = (synthetic(Generation::Rome), synthetic(Generation::Milan));
    let rom = [&rome[..], &milan[..]].concat();
    let ffs = ffs(0x1_0000);
    // A Rome-only board keeps the Milan half: its EFS still boots Milan.
    let out = compose(
        &rom,
        &plan(&[Generation::Rome], Layout::Unified),
        &linked_reset(),
        &ffs,
    )
    .unwrap();
    assert_eq!(out.survey.combo, None);
    assert_eq!(
        out.survey.halves[1],
        (FlashHalf::Upper, HalfAction::Keep(Some(Generation::Milan)))
    );
    assert_eq!(out.image[HALF..], rom[HALF..]);
    assert_eq!(
        booted_half(&out.image, Generation::Milan),
        Some(FlashHalf::Upper)
    );
    assert_eq!(
        out.image[EFS_OFFSET..EFS_OFFSET + 0x40],
        rom[EFS_OFFSET..EFS_OFFSET + 0x40]
    );
    // A 16 MiB vendor half: the vendor EFS, nothing combined.
    let half = compose(
        &rome,
        &plan(&[Generation::Rome, Generation::Milan], Layout::Unified),
        &linked_reset(),
        &ffs,
    )
    .unwrap();
    // Only the flash anchor's part size differs.
    assert_eq!(half.image[..0xde_0000], out.image[..0xde_0000]);
    check_trees(&half.image, &rome, Generation::Rome, 0xde_0000);
    assert_eq!(
        compose(
            &rom,
            &plan(&[Generation::Naples], Layout::Unified),
            &linked_reset(),
            &ffs
        ),
        Err(ImageError::NoSupportedHalf)
    );
}

#[test]
fn reset_image_gets_trust_locator_and_flash_anchor() {
    let ffs = ffs(0x1000);
    let anchor = FlashAnchor::new(2 * HALF as u32, 0x60_0000, 0x1000);
    let mut reset = linked_reset();
    let all = Patched {
        trust: true,
        locator: true,
        flash: true,
    };
    assert_eq!(patch_reset_image(&mut reset, &ffs, anchor).unwrap(), all);
    assert_eq!(reset[0x1000..0x1000 + TRUST_SIZE], trust());
    assert_eq!(reset[0x3000..0x3000 + ANCHOR_SIZE], *anchor.as_bytes());

    // No sites, nothing to do; a site without an FFS counterpart fails.
    let mut plain = vec![0u8; 0x4000];
    assert_eq!(
        patch_reset_image(&mut plain, &ffs, anchor).unwrap(),
        Patched {
            trust: false,
            locator: false,
            flash: false
        }
    );
    assert!(matches!(
        patch_reset_image(&mut linked_reset(), &[0; 0x1000], anchor),
        Err(ImageError::Patch(_))
    ));
    // A locator for a different image size is not this FFS's anchor.
    assert!(patch_reset_image(&mut linked_reset(), &ffs[..0xff8], anchor).is_err());
    assert!(matches!(
        patch_reset_image(&mut linked_reset(), &ffs, FlashAnchor::placeholder()),
        Err(ImageError::Patch(_))
    ));
}

#[test]
fn flash_plan_serializes_generations_and_layout_by_name() {
    let p = plan(&[Generation::Rome, Generation::Milan], Layout::Dual);
    let json = serde_json::to_string(&p).unwrap();
    assert!(
        json.contains("\"generations\":[\"rome\",\"milan\"]"),
        "{json}"
    );
    assert!(json.contains("\"layout\":\"dual\""), "{json}");
    let back: FlashPlan = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
    assert!(serde_json::from_str::<FlashPlan>(&json.replace("milan", "genoa")).is_err());
    let old = json.replace(",\"layout\":\"dual\"", "");
    assert_eq!(
        serde_json::from_str::<FlashPlan>(&old).unwrap().layout,
        Layout::Unified
    );
    assert_eq!(Layout::parse("dual"), Some(Layout::Dual));
    assert_eq!(Layout::parse("2x16"), None);
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
    // (donor, generation, vendor 0x62 slot, prefix end) per FIRMWARE_IMAGE §2.1/§4.
    let rows = [
        (
            "naples-s8026-v202.bin",
            Generation::Naples,
            0xdb_0000,
            0x27_3000,
        ),
        (
            "naples2p-knppd32-1801.bin",
            Generation::Naples,
            0xde_0000,
            0x4b_a000,
        ),
        (
            "rome-s8026-v202.bin",
            Generation::Rome,
            0xdb_0000,
            0x50_b000,
        ),
        (
            "rome-s8030-v404.bin",
            Generation::Rome,
            0xdb_0000,
            0x54_b000,
        ),
        (
            "rome2p-s8253-v203.bin",
            Generation::Rome,
            0xdb_0000,
            0x54_b000,
        ),
        (
            "milan-s8030-v404.bin",
            Generation::Milan,
            0xce_0000,
            0x58_6000,
        ),
        (
            "milan2p-s8253-v203.bin",
            Generation::Milan,
            0xce_0000,
            0x58_6000,
        ),
    ];
    for (name, generation, slot, prefix_end) in rows {
        let Some(half) = donor(&format!("donors/{name}")) else {
            continue;
        };
        let layout = VendorLayout::parse(&half).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(layout.generation, generation, "{name}");
        assert_eq!(layout.reset.flash_offset, slot, "{name}");
        assert_eq!(layout.reset.dram_end(), 0x7600_0000, "{name}");
        assert_eq!(layout.prefix_end, prefix_end, "{name}");
    }
}

const S8030: &str = "tyan-s8030gm2ne/S8030GM2NE_BIOS_V4.04/8030V404.ROM";

#[test]
fn donor_s8030_unified() {
    let Some(rom) = donor(S8030) else {
        return;
    };
    check_dual(&rom).unwrap();
    let ffs = ffs(0x10_0000);
    let both = plan(&[Generation::Rome, Generation::Milan], Layout::Unified);
    let out = compose(&rom, &both, &linked_reset(), &ffs).unwrap();
    let image = &out.image;
    assert_eq!(out.survey.combo, Some(0x54_b000));
    assert_eq!(out.survey.ffs, 0x60_0000..0xde_0000);
    assert_eq!(get32(image, EFS_OFFSET + EFS_WORD), 0xffff_fffc);
    assert_eq!(get32(image, EFS_OFFSET + 0x28), 0x11b_6000);
    assert_eq!(get32(image, HALF + EFS_OFFSET), u32::MAX);
    // The rest of the lower EFS (SPI/eSPI fields) is the vendor's.
    assert_eq!(
        image[EFS_OFFSET + 0x2c..EFS_OFFSET + 0x60],
        rom[EFS_OFFSET + 0x2c..EFS_OFFSET + 0x60]
    );
    for (generation, half) in [
        (Generation::Rome, FlashHalf::Lower),
        (Generation::Milan, FlashHalf::Upper),
    ] {
        assert_eq!(booted_half(image, generation), Some(FlashHalf::Lower));
        check_trees(image, &rom[half.range()], generation, 0xde_0000);
    }
    check_payloads(image, 0xde_0000, &ffs, 0x60_0000);
    // The AMI multi-tank descriptor (Rome half, 0x21000) is in the prefix.
    assert_eq!(image[0x2_1000..0x2_1058], rom[0x2_1000..0x2_1058]);
    assert!(
        image[HALF + 0x58_6000..].iter().all(|&b| b == 0xff),
        "free upper tail"
    );
}

#[test]
fn donor_s8030_dual() {
    let Some(rom) = donor(S8030) else {
        return;
    };
    let ffs = ffs(0x10_0000);
    let both = plan(&[Generation::Rome, Generation::Milan], Layout::Dual);
    let out = compose(&rom, &both, &linked_reset(), &ffs).unwrap();
    let image = &out.image;
    check_dual(image).unwrap();
    for half in [FlashHalf::Lower, FlashHalf::Upper] {
        let efs = half.offset() + EFS_OFFSET;
        assert_eq!(image[efs..efs + 0x60], rom[efs..efs + 0x60]);
    }
    check_trees(image, &rom[..HALF], Generation::Rome, 0xde_0000);
    check_trees(image, &rom[HALF..], Generation::Milan, HALF + 0xde_0000);
    check_payloads(image, 0xde_0000, &ffs, 0x60_0000);
    assert_eq!(image[0xde_0000..HALF], image[HALF + 0xde_0000..]);
    // Milan v4.00 composes the same way.
    if let Some(v400) = donor("tyan-s8030gm2ne/S8030GM2NE_bios_v400/8030V400.ROM") {
        for layout in [Layout::Unified, Layout::Dual] {
            let p = FlashPlan {
                layout,
                ..both.clone()
            };
            compose(&v400, &p, &linked_reset(), &ffs).unwrap();
        }
    }
}

#[test]
fn donor_s8026_naples_rome_unified() {
    let rom = donor("tyan-s8026/x/8026V202.ROM").or_else(|| {
        let naples = donor("donors/naples-s8026-v202.bin")?;
        Some([naples, donor("donors/rome-s8026-v202.bin")?].concat())
    });
    let Some(rom) = rom else {
        return;
    };
    check_dual(&rom).unwrap();
    let ffs = ffs(0x10_0000);
    let both = plan(&[Generation::Naples, Generation::Rome], Layout::Unified);
    let out = compose(&rom, &both, &linked_reset(), &ffs).unwrap();
    let image = &out.image;
    assert_eq!(get32(image, EFS_OFFSET + EFS_WORD), 0xffff_fffe);
    let combo = Combo::parse(image, out.survey.combo.unwrap())
        .unwrap()
        .unwrap();
    let naples = combo.entries().next().unwrap();
    assert_eq!((naples.id, naples.directory), (0xbc09_0000, 0xff07_7000));
    for (generation, half) in [
        (Generation::Naples, FlashHalf::Lower),
        (Generation::Rome, FlashHalf::Upper),
    ] {
        assert_eq!(booted_half(image, generation), Some(FlashHalf::Lower));
        check_trees(image, &rom[half.range()], generation, 0xde_0000);
    }
    check_payloads(image, 0xde_0000, &ffs, 0x60_0000);
    // Dual still works for the same pair.
    let dual = FlashPlan {
        layout: Layout::Dual,
        ..both
    };
    check_dual(&compose(&rom, &dual, &linked_reset(), &ffs).unwrap().image).unwrap();
}
