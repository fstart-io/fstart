// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host UEFI FMP Capsule-on-Disk builder and RO/A(/B) flash layout packer.
//!
//! Produces standard UEFI 2.10 FMP capsules (`EFI_CAPSULE_HEADER` ->
//! `EFI_FIRMWARE_MANAGEMENT_CAPSULE_HEADER` -> v3 image header ->
//! `EFI_FIRMWARE_IMAGE_AUTHENTICATION` (PKCS#7) -> `MSS1` payload header ->
//! padded RW FFS slot image + RMAP manifest `"RW_SLOT\n"`), compatible with
//! `fwupd`/LVFS and CrabEFI's `efi/capsule/apply.rs`.

use fstart_ffs::root::{ROOT_SIZE, Root};
use fstart_ffs::slot_state::{ERASED_BYTE, SECTOR_SIZE, Slot, SlotLayout, SlotStateRecord};
use zerocopy::byteorder::{LE, U16, U32, U64};
use zerocopy::{Immutable, IntoBytes, KnownLayout, Unaligned};

/// `EFI_FIRMWARE_MANAGEMENT_CAPSULE_ID_GUID` (6DCBD5ED-E82D-4C44-BDA1-7194199AD92A).
pub const EFI_FMP_CAPSULE_GUID: [u8; 16] = [
    0xed, 0xd5, 0xcb, 0x6d, 0x2d, 0xe8, 0x44, 0x4c, 0xbd, 0xa1, 0x71, 0x94, 0x19, 0x9a, 0xd9, 0x2a,
];

/// `EFI_CERT_TYPE_PKCS7_GUID` (4AAFD29D-68DF-49EE-8AA9-347D375665A7).
pub const EFI_CERT_TYPE_PKCS7_GUID: [u8; 16] = [
    0x9d, 0xd2, 0xaf, 0x4a, 0xdf, 0x68, 0xee, 0x49, 0x8a, 0xa9, 0x34, 0x7d, 0x37, 0x56, 0x65, 0xa7,
];

pub const CAPSULE_FLAGS_PERSIST_ACROSS_RESET: u32 = 0x0001_0000;
const WIN_CERT_REVISION_2: u16 = 0x0200;
const WIN_CERT_TYPE_EFI_GUID: u16 = 0x0ef1;
const FMP_PAYLOAD_MAGIC: [u8; 4] = *b"MSS1";

#[derive(Clone, Copy, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct EfiCapsuleHeaderWire {
    capsule_guid: [u8; 16],
    header_size: U32<LE>,
    flags: U32<LE>,
    capsule_image_size: U32<LE>,
}

#[derive(Clone, Copy, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FmpCapsuleHeaderWire {
    version: U32<LE>,
    embedded_driver_count: U16<LE>,
    payload_item_count: U16<LE>,
    item_offset_0: U64<LE>,
}

#[derive(Clone, Copy, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FmpImageHeaderV3Wire {
    version: U32<LE>,
    update_image_type_id: [u8; 16],
    update_image_index: u8,
    reserved: [u8; 3],
    update_image_size: U32<LE>,
    update_vendor_code_size: U32<LE>,
    update_hardware_instance: U64<LE>,
    image_capsule_support: U64<LE>,
}

#[derive(Clone, Copy, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FmpAuthHeaderWire {
    monotonic_count: U64<LE>,
    dw_length: U32<LE>,
    w_revision: U16<LE>,
    w_certificate_type: U16<LE>,
    cert_type: [u8; 16],
}

#[derive(Clone, Copy, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FmpPayloadHeaderWire {
    signature: [u8; 4],
    header_size: U32<LE>,
    fw_version: U32<LE>,
    lowest_supported_version: U32<LE>,
}

const _: () = assert!(core::mem::size_of::<EfiCapsuleHeaderWire>() == 28);
const _: () = assert!(core::mem::size_of::<FmpCapsuleHeaderWire>() == 16);
const _: () = assert!(core::mem::size_of::<FmpImageHeaderV3Wire>() == 48);
const _: () = assert!(core::mem::size_of::<FmpAuthHeaderWire>() == 32);
const _: () = assert!(core::mem::size_of::<FmpPayloadHeaderWire>() == 16);

/// Inputs for building a signed UEFI FMP capsule wrapping one RW FFS slot image.
///
/// The ESRT `fw_version` is derived directly from the signed `Root` at byte 0
/// of `rw_slot_image`, so outer FMP metadata and inner Ed25519 `Root` cannot
/// drift.
pub struct CapsuleSpec<'a> {
    /// Stable board/product ESRT system-firmware class GUID (16 bytes, UEFI mixed-endian).
    pub fw_class_guid: [u8; 16],
    /// Lowest supported ESRT `u32` version advertised in the FMP `MSS1` header.
    pub lowest_supported_version: u32,
    /// `EFI_FIRMWARE_IMAGE_AUTHENTICATION.MonotonicCount`.
    pub monotonic_count: u64,
    /// RMAP region name(s), newline-separated (typically `"RW_SLOT\n"`).
    pub rmap_manifest: &'a str,
    /// Target slot size in bytes; `rw_slot_image` is padded with `0xFF` to
    /// this exact size so CrabEFI's whole-region check succeeds.
    pub slot_size: usize,
    /// Self-contained RW FFS slot image bytes (starting with its 512-byte `Root`).
    pub rw_slot_image: &'a [u8],
}

/// Construct the authenticated FMP payload bytes (`MSS1` header + padded
/// `rw_slot_image` + RMAP manifest + manifest length) and the exact buffer
/// signed by PKCS#7 (`payload || monotonic_count.to_le_bytes()`).
pub fn build_fmp_payload_to_sign(spec: &CapsuleSpec<'_>) -> Result<(Vec<u8>, Vec<u8>), String> {
    if spec.rw_slot_image.len() < ROOT_SIZE {
        return Err("RW slot image smaller than 512-byte Root".into());
    }
    if spec.rw_slot_image.len() > spec.slot_size || !spec.slot_size.is_multiple_of(SECTOR_SIZE) {
        return Err("RW slot image exceeds or is misaligned with slot_size".into());
    }
    let root = Root::parse(&spec.rw_slot_image[..ROOT_SIZE])
        .map_err(|e| format!("invalid RW Root in capsule image: {e:?}"))?;

    let manifest_bytes = spec.rmap_manifest.as_bytes();
    let manifest_len =
        u32::try_from(manifest_bytes.len()).map_err(|_| "RMAP manifest exceeds u32")?;

    let hdr = FmpPayloadHeaderWire {
        signature: FMP_PAYLOAD_MAGIC,
        header_size: U32::new(core::mem::size_of::<FmpPayloadHeaderWire>() as u32),
        fw_version: U32::new(root.fw_version),
        lowest_supported_version: U32::new(spec.lowest_supported_version),
    };

    let mut payload = Vec::with_capacity(
        core::mem::size_of::<FmpPayloadHeaderWire>() + spec.slot_size + manifest_bytes.len() + 4,
    );
    payload.extend_from_slice(hdr.as_bytes());
    payload.extend_from_slice(spec.rw_slot_image);
    payload.resize(
        core::mem::size_of::<FmpPayloadHeaderWire>() + spec.slot_size,
        ERASED_BYTE,
    );
    payload.extend_from_slice(manifest_bytes);
    payload.extend_from_slice(&manifest_len.to_le_bytes());

    let mut signed_buf = Vec::with_capacity(payload.len() + 8);
    signed_buf.extend_from_slice(&payload);
    signed_buf.extend_from_slice(&spec.monotonic_count.to_le_bytes());
    Ok((payload, signed_buf))
}

/// Assemble a complete UEFI 2.10 FMP `.cap` binary from `spec` and a DER
/// PKCS#7 detached signature over `build_fmp_payload_to_sign(spec)?.1`.
pub fn assemble_fmp_capsule(spec: &CapsuleSpec<'_>, pkcs7_der: &[u8]) -> Result<Vec<u8>, String> {
    let (payload, _) = build_fmp_payload_to_sign(spec)?;

    let win_cert_len = u32::try_from(4 + 2 + 2 + 16 + pkcs7_der.len())
        .map_err(|_| "PKCS#7 signature exceeds u32")?;
    let auth_len = 8u32
        .checked_add(win_cert_len)
        .ok_or("auth header length overflow")?;
    let update_image_size = auth_len
        .checked_add(u32::try_from(payload.len()).map_err(|_| "payload exceeds u32")?)
        .ok_or("update image size overflow")?;

    let cap_hdr_sz = core::mem::size_of::<EfiCapsuleHeaderWire>() as u32;
    let fmp_hdr_sz = core::mem::size_of::<FmpCapsuleHeaderWire>() as u32;
    let img_hdr_sz = core::mem::size_of::<FmpImageHeaderV3Wire>() as u32;
    let total_size = cap_hdr_sz
        .checked_add(fmp_hdr_sz)
        .and_then(|v| v.checked_add(img_hdr_sz))
        .and_then(|v| v.checked_add(update_image_size))
        .ok_or("capsule size overflow")?;

    let cap_hdr = EfiCapsuleHeaderWire {
        capsule_guid: EFI_FMP_CAPSULE_GUID,
        header_size: U32::new(cap_hdr_sz),
        flags: U32::new(CAPSULE_FLAGS_PERSIST_ACROSS_RESET),
        capsule_image_size: U32::new(total_size),
    };
    let fmp_hdr = FmpCapsuleHeaderWire {
        version: U32::new(1),
        embedded_driver_count: U16::new(0),
        payload_item_count: U16::new(1),
        item_offset_0: U64::new(u64::from(fmp_hdr_sz)),
    };
    let img_hdr = FmpImageHeaderV3Wire {
        version: U32::new(3),
        update_image_type_id: spec.fw_class_guid,
        update_image_index: 1,
        reserved: [0; 3],
        update_image_size: U32::new(update_image_size),
        update_vendor_code_size: U32::new(0),
        update_hardware_instance: U64::new(0),
        // UEFI 2.10 Table 23.8: bit 0 = CAPSULE_SUPPORT_AUTHENTICATION.
        image_capsule_support: U64::new(1),
    };
    let auth_hdr = FmpAuthHeaderWire {
        monotonic_count: U64::new(spec.monotonic_count),
        dw_length: U32::new(win_cert_len),
        w_revision: U16::new(WIN_CERT_REVISION_2),
        w_certificate_type: U16::new(WIN_CERT_TYPE_EFI_GUID),
        cert_type: EFI_CERT_TYPE_PKCS7_GUID,
    };

    let mut out = Vec::with_capacity(total_size as usize);
    out.extend_from_slice(cap_hdr.as_bytes());
    out.extend_from_slice(fmp_hdr.as_bytes());
    out.extend_from_slice(img_hdr.as_bytes());
    out.extend_from_slice(auth_hdr.as_bytes());
    out.extend_from_slice(pkcs7_der);
    out.extend_from_slice(&payload);

    debug_assert_eq!(out.len(), total_size as usize);
    Ok(out)
}

/// Populate `SLOT_STATE_0`, `SLOT_STATE_1`, `RW_A`, and (in RO/A/B) `RW_B`
/// inside a full flash image buffer using one self-contained RW FFS image.
///
/// Validates `layout` against `flash.len()` and derives `fw_version` and
/// `security_version` directly from the `Root` at byte 0 of `rw_slot_image`.
pub fn populate_rw_slots(
    flash: &mut [u8],
    layout: SlotLayout,
    rw_slot_image: &[u8],
) -> Result<(), String> {
    let capacity = u32::try_from(flash.len()).map_err(|_| "flash exceeds u32")?;
    layout
        .validate(capacity)
        .map_err(|e| format!("invalid slot layout: {e:?}"))?;
    if rw_slot_image.len() < ROOT_SIZE {
        return Err("RW slot image smaller than 512-byte Root".into());
    }
    let root =
        Root::parse(&rw_slot_image[..ROOT_SIZE]).map_err(|e| format!("invalid RW Root: {e:?}"))?;
    for slot in [Slot::A, Slot::B] {
        if let Some(ext) = layout.extent(slot)
            && rw_slot_image.len() > ext.size as usize
        {
            return Err(format!(
                "RW image ({} bytes) exceeds {:?} slot size ({} bytes)",
                rw_slot_image.len(),
                slot,
                ext.size
            ));
        }
    }

    let root_digest = fstart_crypto::digest::hash_sha256(&rw_slot_image[..ROOT_SIZE]);
    let (s0, s1) = layout.state_sectors();
    for sector in [s0, s1] {
        let start = sector as usize;
        flash[start..start + SECTOR_SIZE].fill(ERASED_BYTE);
    }

    let fallback = if layout.is_ab() {
        Slot::B
    } else {
        Slot::Recovery
    };
    let initial = SlotStateRecord::initial(
        Slot::A,
        fallback,
        root.fw_version,
        root.security_version,
        &root_digest,
    );
    let encoded = initial.encode();
    flash[s0 as usize..s0 as usize + encoded.len()].copy_from_slice(&encoded);

    for slot in [Slot::A, Slot::B] {
        let Some(ext) = layout.extent(slot) else {
            continue;
        };
        let start = ext.offset as usize;
        let end = start + ext.size as usize;
        flash[start..end].fill(ERASED_BYTE);
        flash[start..start + rw_slot_image.len()].copy_from_slice(rw_slot_image);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_ffs::root::DirectoryRef;
    use fstart_ffs::slot_state::{Phase, ScanStatus, SlotExtent, scan_journal};

    fn fake_rw_image(fw_version: u32, sec_version: u64) -> Vec<u8> {
        let root = Root {
            security_version: sec_version,
            fw_version,
            image_family: *b"TEST-CAPSULE-001",
            key_id: 0,
            descriptors: [None, None],
            directory: DirectoryRef {
                offset: ROOT_SIZE as u64,
                size: 64,
                digest: [0x22; 32],
            },
            signature: [0x33; 64],
        };
        let mut img = vec![0x77u8; 0x1000];
        img[..ROOT_SIZE].copy_from_slice(&root.encode());
        img
    }

    #[test]
    fn capsule_header_binds_root_version_and_pads_to_slot_size() {
        let rw_img = fake_rw_image(0x0001_0002, 5);
        let spec = CapsuleSpec {
            fw_class_guid: [0x11; 16],
            lowest_supported_version: 0x0001_0000,
            monotonic_count: 1,
            rmap_manifest: "RW_SLOT\n",
            slot_size: 0x2000,
            rw_slot_image: &rw_img,
        };
        let cap = assemble_fmp_capsule(&spec, &[0x30, 0x82, 0x01, 0x00]).unwrap();
        assert_eq!(&cap[..16], &EFI_FMP_CAPSULE_GUID);
        let cap_size = u32::from_le_bytes(cap[24..28].try_into().unwrap()) as usize;
        assert_eq!(cap_size, cap.len());
        // FMP image header v3 (48 bytes) starts at 28 + 16 = 44; image_capsule_support is at +40..+48.
        let image_capsule_support = u64::from_le_bytes(cap[84..92].try_into().unwrap());
        assert_eq!(image_capsule_support & 1, 1);
        // MSS1 payload header sits right after 28 + 16 + 48 + 32 + 4 = 128 bytes.
        assert_eq!(&cap[128..132], b"MSS1");
        let mss_ver = u32::from_le_bytes(cap[136..140].try_into().unwrap());
        assert_eq!(mss_ver, 0x0001_0002);
        // Trailing 4 bytes are the RMAP manifest length (8 for "RW_SLOT\n").
        let rmap_len = u32::from_le_bytes(cap[cap.len() - 4..].try_into().unwrap());
        assert_eq!(rmap_len, 8);
        assert_eq!(&cap[cap.len() - 12..cap.len() - 4], b"RW_SLOT\n");
    }

    #[test]
    fn populate_rw_slots_derives_root_versions_and_validates_layout() {
        let layout = SlotLayout::RoAB {
            state_sector_0: 0x0000,
            state_sector_1: 0x1000,
            slot_a: SlotExtent {
                offset: 0x2000,
                size: 0x2000,
            },
            slot_b: SlotExtent {
                offset: 0x4000,
                size: 0x2000,
            },
        };
        let mut flash = vec![0u8; 0x6000];
        let rw_img = fake_rw_image(42, 3);
        populate_rw_slots(&mut flash, layout, &rw_img).unwrap();

        let scan = scan_journal(&flash[0..0x1000], &flash[0x1000..0x2000]);
        let ScanStatus::Valid {
            active_sector,
            record,
        } = scan.status
        else {
            panic!("expected valid initial journal");
        };
        assert_eq!(active_sector, 0);
        assert_eq!(record.phase, Phase::Stable);
        assert_eq!(record.active_slot, Slot::A);
        assert_eq!(record.fallback_slot, Slot::B);
        assert_eq!(record.fw_version, 42);
        assert_eq!(record.sec_version, 3);
        assert_eq!(&flash[0x2000..0x4000], &flash[0x4000..0x6000]);
    }
}
