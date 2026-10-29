// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use fstart_amdfw_format::directory::{
    self, BiosEntry, Directory, Entry, Levels, PspEntry, modify_entries, write_table,
};
use std::path::Path;
use zerocopy::Immutable;

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
        MILAN => ResetSlot {
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

fn psp_entry(kind: u8, size: u32, location: u64) -> PspEntry {
    PspEntry::new(kind, 0, 0, size, location)
}

fn bios_entry(kind: u8, flags: u8, size: u32, location: u64, destination: u64) -> BiosEntry {
    BiosEntry::new(kind, 0, 0, flags, size, location, destination)
}

fn put_dir<E: IntoBytes + Immutable>(
    h: &mut [u8],
    at: usize,
    cookie: &[u8; 4],
    info: u32,
    entries: &[E],
) {
    write_table(&mut h[at..], *cookie, info, entries).unwrap();
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
    let gen1 = generation == NAPLES;
    let ptr = |off: usize| off as u64 + if gen1 { MMIO_BASE } else { 0 };
    let info = match generation {
        MILAN => 0x10 | 1 << 29,
        ROME => 0x10,
        _ => 0,
    };
    let profile = Profile::for_slot(generation);
    let mut efs = Efs::erased();
    efs.new_psp_directory.set(ptr(PSP) as u32);
    efs.generation.set(!profile.generation_mask);
    efs.bios_directory_mut(profile.bios).set(ptr(BHD) as u32);
    h[EFS_OFFSET..][..size_of::<Efs>()].copy_from_slice(efs.as_bytes());
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
            psp_entry(directory::psp::SOFT_FUSE, u32::MAX, 1),
            psp_entry(directory::psp::LEVEL2, 0x400, ptr(PL2)),
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
            bios_entry(bios::LEVEL2, 0, 0x400, ptr(BL2), u64::MAX),
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
    let g = GENERATIONS.iter().position(|&x| x == generation).unwrap() as u8;
    h[0x11_0000..0x11_8000].fill(0x10 + g);
    h[0x11_8000..0x11_9000].fill(0x20 + g);
    h[0x20_2000..0x20_2c80].fill(0x30 + g);
    h[slot.flash_offset as usize..].fill(0xee);
    h
}

fn plan(generations: &[Generation]) -> FlashPlan {
    FlashPlan {
        generations: generations.to_vec(),
        reset: IMAGE,
        ffs_offset: FFS_OFFSET,
        dram_floor: DRAM_FLOOR,
        mirrors: Vec::new(),
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

const COOKIES: [[u8; 4]; 4] = [
    directory::PSP_COOKIE,
    directory::PSP_LEVEL2_COOKIE,
    directory::BIOS_COOKIE,
    directory::BIOS_LEVEL2_COOKIE,
];

/// Every level and entry of `out` against `vendor`: same entries, same
/// data (directories by cookie only, their pointers move); `reset` checks
/// the `0x62` entries instead.
fn same_tree<E: Entry>(
    out: &Levels<'_, E>,
    vendor: &Levels<'_, E>,
    mut reset: impl FnMut(&Directory<'_, E>, &E) -> bool,
) {
    let (o, v): (Vec<_>, Vec<_>) = (out.iter().collect(), vendor.iter().collect());
    assert_eq!(o.len(), v.len());
    for (od, vd) in o.into_iter().zip(v) {
        assert_eq!(od.header.cookie, vd.header.cookie);
        assert_eq!(od.entries.len(), vd.entries.len());
        for (oe, ve) in od.entries.iter().zip(vd.entries) {
            assert_eq!((oe.kind(), oe.instance()), (ve.kind(), ve.instance()));
            if reset(&od, oe) {
                continue;
            }
            match (od.data(oe), vd.data(ve)) {
                (Ok(a), Ok(b)) if COOKIES.iter().any(|c| b.starts_with(c)) => {
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
    let half = booted_half(image, generation).unwrap().offset();
    let out = flash(generation, &image[half..]);
    let ven = flash(generation, vendor);
    same_tree(
        &out.psp_directories().unwrap(),
        &ven.psp_directories().unwrap(),
        |_, _| false,
    );
    let mut resets = 0;
    let slot = generation;
    same_tree(
        &out.bios_directories_checked(slot).unwrap(),
        &ven.bios_directories_checked(slot).unwrap(),
        |dir, e| {
            if e.kind != bios::RESET_IMAGE {
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
fn dual_keeps_each_vendor_efs_with_its_own_reset_copy() {
    let (rome, milan) = (synthetic(ROME), synthetic(MILAN));
    let rom = [&rome[..], &milan[..]].concat();
    let ffs = ffs(0x1_0000);
    let both = plan(&[ROME, MILAN]);
    let out = compose(&rom, &both, &linked_reset(), &ffs).unwrap();
    let image = &out.image;
    assert_eq!(out.survey.ffs, 0x60_0000..0xde_0000);
    check_dual(image).unwrap();
    // The EFS are the vendor's.
    assert_eq!(
        image[EFS_OFFSET..EFS_OFFSET + 0x40],
        rom[EFS_OFFSET..EFS_OFFSET + 0x40]
    );
    let upper = HALF + EFS_OFFSET;
    assert_eq!(image[upper..upper + 0x40], rom[upper..upper + 0x40]);
    check_trees(image, &rome, ROME, 0xde_0000);
    check_trees(image, &milan, MILAN, HALF + 0xde_0000);
    check_payloads(image, 0xde_0000, &ffs, 0x60_0000);
    assert_eq!(image[0xde_0000..HALF], image[HALF + 0xde_0000..]);
    assert!(
        image[HALF + 0x21_1000..HALF + 0xde_0000]
            .iter()
            .all(|&b| b == 0xff)
    );
}

#[test]
fn layouts_the_composer_refuses() {
    let (naples, rome, milan) = (synthetic(NAPLES), synthetic(ROME), synthetic(MILAN));
    let ffs = ffs(0x1_0000);
    let all = [NAPLES, ROME, MILAN];
    let compose_with =
        |rom: &[u8], p: &FlashPlan, ffs: &[u8]| compose(rom, p, &linked_reset(), ffs);
    let _ = naples;
    let rom = [&rome[..], &milan[..]].concat();
    let all = plan(&all);
    // The FFS may not reach the reset image.
    assert_eq!(
        compose_with(&rom, &all, &vec![0; 0x7e_0001]),
        Err(ImageError::FfsTooLarge {
            size: 0x7e_0001,
            capacity: 0x7e_0000
        })
    );
    // Nor may a vendor prefix (a blob moved up to 0xde8000).
    let mut late = rome.clone();
    modify_entries::<PspEntry>(&mut late, PL2, |e| e.address.set(0xde_8000)).unwrap();
    assert!(matches!(
        compose_with(&[&late[..], &milan[..]].concat(), &all, &ffs),
        Err(ImageError::PrefixOverlap {
            limit: 0xde_0000,
            ..
        })
    ));
    // The FFS lives in the lower half: its generation must be supported.
    assert!(matches!(
        compose_with(&rom, &plan(&[MILAN]), &ffs),
        Err(ImageError::Layout(_))
    ));
    assert!(matches!(
        compose_with(&[&rome[..], &rome[..]].concat(), &all, &ffs),
        Err(ImageError::Layout(_))
    ));
    // An EFS signature where a boot ROM searches before 0x20000.
    let mut stray = self::ffs(0x22_0004);
    stray[0x82_0000 - 0x60_0000..][..4].copy_from_slice(&efs::SIGNATURE.to_le_bytes());
    assert_eq!(
        compose_with(&rom, &all, &stray),
        Err(ImageError::StrayEfs { offset: 0x82_0000 })
    );
    let checksum = |mut half: Vec<u8>| {
        half[BL2 + 16] ^= 1;
        compose_with(&[&rome[..], &half[..]].concat(), &all, &ffs)
    };
    assert!(matches!(
        checksum(milan.clone()),
        Err(ImageError::Directory(DirError::Checksum(BL2)))
    ));
}

#[test]
fn single_generation_and_kept_halves() {
    let (rome, milan) = (synthetic(ROME), synthetic(MILAN));
    let rom = [&rome[..], &milan[..]].concat();
    let ffs = ffs(0x1_0000);
    // A Rome-only board keeps the Milan half: its EFS still boots Milan.
    let out = compose(&rom, &plan(&[ROME]), &linked_reset(), &ffs).unwrap();
    assert_eq!(
        out.survey.halves[1],
        (FlashHalf::Upper, HalfAction::Keep(Some(MILAN)))
    );
    assert_eq!(out.image[HALF..], rom[HALF..]);
    assert_eq!(booted_half(&out.image, MILAN), Some(FlashHalf::Upper));
    assert_eq!(
        out.image[EFS_OFFSET..EFS_OFFSET + 0x40],
        rom[EFS_OFFSET..EFS_OFFSET + 0x40]
    );
    // A 16 MiB vendor half.
    let half = compose(&rome, &plan(&[ROME, MILAN]), &linked_reset(), &ffs).unwrap();
    // Only the flash anchor's part size differs.
    assert_eq!(half.image[..0xde_0000], out.image[..0xde_0000]);
    check_trees(&half.image, &rome, ROME, 0xde_0000);
    assert_eq!(
        compose(&rom, &plan(&[NAPLES]), &linked_reset(), &ffs),
        Err(ImageError::NoSupportedHalf)
    );
}

#[test]
fn full_readout_mirrors_the_composed_bank() {
    let milan = synthetic(MILAN);
    let mut readout = vec![0xff; 4 * HALF];
    readout[..HALF].copy_from_slice(&milan);
    readout[2 * HALF..2 * HALF + 0x100].fill(0x5a);
    readout[3 * HALF..].copy_from_slice(&milan);
    readout[3 * HALF + 0x80_0000] = 0x42;
    let plan = FlashPlan {
        mirrors: vec![3 * HALF as u32],
        ..plan(&[MILAN])
    };
    let ffs = ffs(0x1_0000);
    let bank = compose(&milan, &plan, &linked_reset(), &ffs).unwrap();
    assert_eq!(bank.image.len(), HALF);
    let full = compose(&readout, &plan, &linked_reset(), &ffs).unwrap();
    assert_eq!(full.image.len(), 4 * HALF);
    // The same bank (and flash anchor) at 0 and at the mirror; the middle
    // of the readout is kept byte for byte.
    assert_eq!(full.image[..HALF], bank.image[..]);
    assert_eq!(full.image[3 * HALF..], bank.image[..]);
    assert_eq!(full.image[HALF..3 * HALF], readout[HALF..3 * HALF]);
    assert_eq!(full.survey, bank.survey);
    // A readout too small for the mirror is not silently truncated.
    assert!(matches!(
        compose(&readout[..2 * HALF], &plan, &linked_reset(), &ffs),
        Err(ImageError::Layout(_))
    ));
    let overlapping = FlashPlan {
        mirrors: vec![0x80_0000],
        ..plan.clone()
    };
    assert!(matches!(overlapping.check(), Err(ImageError::Plan(_))));
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
fn flash_plan_serializes_generations_by_name() {
    let p = plan(&[ROME, MILAN]);
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
    // (donor, generation, vendor 0x62 slot, prefix end) per FIRMWARE_IMAGE §2.1/§4.
    let rows = [
        ("naples-s8026-v202.bin", NAPLES, 0xdb_0000, 0x29_4000),
        ("naples2p-knppd32-1801.bin", NAPLES, 0xde_0000, 0x4e_0000),
        ("rome-s8026-v202.bin", ROME, 0xdb_0000, 0x50_b000),
        ("rome-s8030-v404.bin", ROME, 0xdb_0000, 0x54_b000),
        ("rome2p-s8253-v203.bin", ROME, 0xdb_0000, 0x54_b000),
        ("milan-s8030-v404.bin", MILAN, 0xce_0000, 0x58_6000),
        ("milan2p-s8253-v203.bin", MILAN, 0xce_0000, 0x58_6000),
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
fn donor_s8030_dual() {
    let Some(rom) = donor(S8030) else {
        return;
    };
    let ffs = ffs(0x10_0000);
    let both = plan(&[ROME, MILAN]);
    let out = compose(&rom, &both, &linked_reset(), &ffs).unwrap();
    let image = &out.image;
    check_dual(image).unwrap();
    for half in [FlashHalf::Lower, FlashHalf::Upper] {
        let efs = half.offset() + EFS_OFFSET;
        assert_eq!(image[efs..efs + 0x60], rom[efs..efs + 0x60]);
    }
    check_trees(image, &rom[..HALF], ROME, 0xde_0000);
    check_trees(image, &rom[HALF..], MILAN, HALF + 0xde_0000);
    check_payloads(image, 0xde_0000, &ffs, 0x60_0000);
    assert_eq!(image[0xde_0000..HALF], image[HALF + 0xde_0000..]);
    // Milan v4.00 composes the same way.
    if let Some(v400) = donor("tyan-s8030gm2ne/S8030GM2NE_bios_v400/8030V400.ROM") {
        compose(&v400, &both, &linked_reset(), &ffs).unwrap();
    }
}

#[test]
fn donor_s8026_naples_rome() {
    let rom = donor("tyan-s8026/x/8026V202.ROM").or_else(|| {
        let naples = donor("donors/naples-s8026-v202.bin")?;
        Some([naples, donor("donors/rome-s8026-v202.bin")?].concat())
    });
    let Some(rom) = rom else {
        return;
    };
    let ffs = ffs(0x10_0000);
    let both = plan(&[NAPLES, ROME]);
    let out = compose(&rom, &both, &linked_reset(), &ffs).unwrap();
    let image = &out.image;
    check_dual(image).unwrap();
    assert_eq!(out.survey.ffs, 0x60_0000..0xde_0000);
    check_trees(image, &rom[..HALF], NAPLES, 0xde_0000);
    check_trees(image, &rom[HALF..], ROME, HALF + 0xde_0000);
    check_payloads(image, 0xde_0000, &ffs, 0x60_0000);
}

/// The test bench's own flash (Tyan S8030GM2NE backup,
/// `reports/HW_S8030_BENCH.md`): a Rome-only vendor build in the lower
/// half, the upper half all zeros.
#[test]
fn bench_s8030_backup() {
    let Some(rom) = donor("hw/s8030gm2ne-CCSF2AK45016/host-pnor.read1.bin") else {
        return;
    };
    let ffs = ffs(0x10_0000);
    let out = compose(&rom, &plan(&[ROME, MILAN]), &linked_reset(), &ffs).unwrap();
    let image = &out.image;
    assert_eq!(
        out.survey.halves[1],
        (FlashHalf::Upper, HalfAction::Keep(None))
    );
    assert_eq!(out.survey.ffs, 0x60_0000..0xde_0000);
    assert_eq!(image[HALF..], rom[HALF..]);
    assert_eq!(booted_half(image, ROME), Some(FlashHalf::Lower));
    // Everything below the first BIOS directory (0x246000: EFS, PSP
    // directories, blobs) is the vendor's; check_trees covers the rest.
    assert_eq!(image[..0x24_6000], rom[..0x24_6000]);
    check_trees(image, &rom[..HALF], ROME, 0xde_0000);
    check_payloads(image, 0xde_0000, &ffs, 0x60_0000);
}

fn sha256(bytes: &[u8]) -> String {
    fstart_crypto::digest::hash_sha256(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The composed images, byte for byte: the synthetic halves and every
/// donor present.
#[test]
fn composes_the_pinned_images() {
    let ffs = ffs(0x10_0000);
    let pinned = |rom: &[u8], generations: &[Generation]| {
        sha256(
            &compose(rom, &plan(generations), &linked_reset(), &ffs)
                .unwrap()
                .image,
        )
    };
    let (naples, rome, milan) = (synthetic(NAPLES), synthetic(ROME), synthetic(MILAN));
    let synthetic = [
        (
            pinned(&[&rome[..], &milan[..]].concat(), &[ROME, MILAN]),
            "6517e9f70527b8a75ac4b714ad2cce39e43dbd2a68fcec3aa2893a04794a3e0f",
        ),
        (
            pinned(&[&naples[..], &rome[..]].concat(), &[NAPLES, ROME]),
            "09c6ff763b9f7e75b691b0569c3c27eba4db73ab2740a857a55e6a6a942a678b",
        ),
        (
            pinned(&rome, &[ROME]),
            "ae0d32d8e5b648ff68ba1a42257dbfc2ac30766f5486994cbb49cf40efbc63cb",
        ),
    ];
    for (hash, expected) in synthetic {
        assert_eq!(hash, expected);
    }
    let donors: [(&str, &[Generation], &str); 11] = [
        (
            "donors/naples-s8026-v202.bin",
            &[NAPLES],
            "d958a6740fbedc092e24c5abcb6f0b06d694cd52c0d71fa1b60aea292766f2b5",
        ),
        (
            "donors/naples2p-knppd32-1801.bin",
            &[NAPLES],
            "e3883adf546aaf7e6788c2eaff05d1a8057035a23c7bb39aae65afdd5bcf45a2",
        ),
        (
            "donors/rome-s8026-v202.bin",
            &[ROME],
            "5ddb79ff968612c380e9675c9066f1b8203b90b72b735401bea91ff74d838bbc",
        ),
        (
            "donors/rome-s8030-v404.bin",
            &[ROME],
            "3e5ca588c41af4424f994cf06d1cc5be3806a76b87406f0bc0819b7fe2fa7dfe",
        ),
        (
            "donors/rome2p-s8253-v203.bin",
            &[ROME],
            "d289b9f753946d4b920954371a0d7def7538b6d9bd8b3685376c64b186e5110e",
        ),
        (
            "donors/milan-s8030-v404.bin",
            &[MILAN],
            "2d8647843b607b8d2388e75cd5d52c5c9fa7309212837e2cc04c3dbc03dd4dcd",
        ),
        (
            "donors/milan2p-s8253-v203.bin",
            &[MILAN],
            "f17605f332c64a0c874d58602e7ed0a48f9181f69006de6a66f1a25dd800f9d1",
        ),
        (
            S8030,
            &[ROME, MILAN],
            "64a83561e74ab97483da32712b2ec33f85601a078aff206f8b55a5c2da93219e",
        ),
        (
            "tyan-s8030gm2ne/S8030GM2NE_bios_v400/8030V400.ROM",
            &[ROME, MILAN],
            "9901bd95a5dd9cc918b22ff9b9323d5ad37e9490adb59307d1b2a83bc812efdf",
        ),
        (
            "tyan-s8026/x/8026V202.ROM",
            &[NAPLES, ROME],
            "a0ee05faf086cc66dd1af8841aa442e246e7c9a89c43c823639a541043c066d5",
        ),
        (
            "hw/s8030gm2ne-CCSF2AK45016/host-pnor.read1.bin",
            &[ROME, MILAN],
            "546e75fe601f9d7d9cec9d50836425e505890d8f9c7945df3df73e448236f26c",
        ),
    ];
    for (path, generations, expected) in donors {
        if let Some(rom) = donor(path) {
            assert_eq!(pinned(&rom, generations), expected, "{path}");
        }
    }
}
