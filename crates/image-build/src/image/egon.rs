use std::path::Path;
use zerocopy::little_endian::U32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

const EGON_MAGIC: &[u8; 8] = b"eGON.BT0";
const CHECKSUM_STAMP: u32 = 0x5F0A6C39;
const HEADER_LEN: usize = core::mem::size_of::<EgonBootHeader>();
const SPL_SIGNATURE: &[u8; 4] = b"SPL\x02";

/// Allwinner eGON.BT0 boot header with fstart's loader metadata extension (96 bytes).
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
pub struct EgonBootHeader {
    pub branch: [u8; 4],
    pub magic: [u8; 8],
    pub checksum: U32,
    pub length: U32,
    pub spl_signature: [u8; 4],
    pub _reserved0: [u8; 0x14],
    pub next_data_offset: U32,
    pub next_data_size: U32,
    pub total_image_size: U32,
    pub _reserved1: [u8; 0x28],
}

const _: () = assert!(core::mem::size_of::<EgonBootHeader>() == 96);

/// Whether `head` begins with an Allwinner eGON.BT0 boot header.
#[must_use]
pub fn has_magic(head: &[u8]) -> bool {
    EgonBootHeader::ref_from_prefix(head).is_ok_and(|(hdr, _)| &hdr.magic == EGON_MAGIC)
}

/// Pad a flat SPL to the eGON 8-KiB granule and fill its length, signature
/// and checksum. The FFS assembler applies this to the initial stage before
/// packing; `patch_ffs` later reseals the checksum once the locator is set.
pub fn prepare_image(data: &mut Vec<u8>) -> Result<(), String> {
    let (hdr, _) = EgonBootHeader::ref_from_prefix(data)
        .map_err(|_| "binary too small for Allwinner eGON header (< 96 bytes)".to_string())?;
    if &hdr.magic != EGON_MAGIC {
        return Err("eGON.BT0 magic not found at offset 0x04".to_string());
    }

    let stamp = hdr.checksum.get();
    if stamp != CHECKSUM_STAMP {
        return Err(format!(
            "eGON checksum sentinel not found (got {stamp:#010x}, expected {CHECKSUM_STAMP:#010x})"
        ));
    }

    let raw_size = data.len();
    let image_size = ((raw_size + 0x1FFF) & !0x1FFF) as u32;
    data.resize(image_size as usize, 0);
    {
        let (hdr, _) = EgonBootHeader::mut_from_prefix(data).expect("resized >= HEADER_LEN");
        hdr.length = U32::new(image_size);
        hdr.spl_signature = *SPL_SIGNATURE;
    }

    let checksum = word_sum(data);
    let (hdr, _) = EgonBootHeader::mut_from_prefix(data).expect("resized >= HEADER_LEN");
    hdr.checksum = U32::new(checksum);
    verify(data)?;

    Ok(())
}

pub fn patch_file(bin_path: &Path) -> Result<(), String> {
    let mut data = std::fs::read(bin_path).map_err(|e| format!("failed to read binary: {e}"))?;
    prepare_image(&mut data)?;
    std::fs::write(bin_path, &data).map_err(|e| format!("failed to write patched binary: {e}"))
}

pub fn verify(data: &[u8]) -> Result<(), String> {
    let (hdr, _) = EgonBootHeader::ref_from_prefix(data)
        .map_err(|_| "verify: binary too small".to_string())?;

    let is_arm_branch = hdr.branch[3] == 0xEA;
    let is_riscv_jal = (hdr.branch[0] & 0x7F) == 0x6F;
    if !is_arm_branch && !is_riscv_jal {
        return Err(format!(
            "verify: unrecognized branch instruction (bytes: {:#04x} {:#04x} {:#04x} {:#04x})",
            hdr.branch[0], hdr.branch[1], hdr.branch[2], hdr.branch[3]
        ));
    }

    if &hdr.magic != EGON_MAGIC {
        return Err("verify: eGON.BT0 magic mismatch".to_string());
    }

    let length = hdr.length.get();
    let saved_checksum = hdr.checksum.get();

    if length == 0 || (length & 0x1FF) != 0 {
        return Err(format!(
            "verify: length {length:#x} is not a positive multiple of 512"
        ));
    }
    if length as usize > data.len() {
        return Err(format!(
            "verify: length {length:#x} exceeds buffer size {:#x}",
            data.len()
        ));
    }

    let words = <[U32]>::ref_from_bytes(&data[..length as usize])
        .map_err(|_| "verify: length not a multiple of 4".to_string())?;
    let verify_sum = words.iter().enumerate().fold(0u32, |acc, (i, word)| {
        acc.wrapping_add(if i == 3 { CHECKSUM_STAMP } else { word.get() })
    });

    if verify_sum != saved_checksum {
        return Err(format!(
            "verify: checksum mismatch — stored {saved_checksum:#010x}, \
             computed {verify_sum:#010x}"
        ));
    }

    Ok(())
}

pub fn patch_ffs(ffs_image: &mut [u8], bootblock_size: u32) -> Result<(), String> {
    if (ffs_image.len() as u32) < bootblock_size {
        return Err(format!(
            "FFS image ({:#x} bytes) is smaller than bootblock size ({:#x})",
            ffs_image.len(),
            bootblock_size
        ));
    }
    if ffs_image.len() < HEADER_LEN {
        return Err("FFS image too small for eGON header".to_string());
    }
    {
        let (hdr, _) = EgonBootHeader::mut_from_prefix(ffs_image)
            .map_err(|_| "FFS image too small for eGON header".to_string())?;
        if &hdr.magic != EGON_MAGIC {
            return Err("eGON.BT0 magic not found at start of FFS image".to_string());
        }
        hdr.length = U32::new(bootblock_size);
        hdr.spl_signature = *SPL_SIGNATURE;
        hdr.checksum = U32::new(CHECKSUM_STAMP);
    }

    let checksum = word_sum(&ffs_image[..bootblock_size as usize]);
    let (hdr, _) = EgonBootHeader::mut_from_prefix(ffs_image).expect("checked >= HEADER_LEN");
    hdr.checksum = U32::new(checksum);
    verify(&ffs_image[..bootblock_size as usize])?;

    Ok(())
}

fn word_sum(data: &[u8]) -> u32 {
    let aligned_len = data.len() & !3;
    <[U32]>::ref_from_bytes(&data[..aligned_len])
        .expect("multiple of 4")
        .iter()
        .fold(0u32, |checksum, word| checksum.wrapping_add(word.get()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEST_FILE: AtomicUsize = AtomicUsize::new(0);

    fn egon_image(len: usize) -> Vec<u8> {
        let mut image = vec![0; len];
        image[3] = 0xea;
        image[4..12].copy_from_slice(EGON_MAGIC);
        image[0x0c..0x10].copy_from_slice(&CHECKSUM_STAMP.to_le_bytes());
        image
    }

    #[test]
    fn patch_file_pads_and_verifies_egon_image() {
        let id = NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("fstart-egon-{id}-{}.bin", std::process::id()));
        std::fs::write(&path, egon_image(0x123)).expect("write eGON fixture");

        patch_file(&path).expect("patch eGON image");
        let image = std::fs::read(&path).expect("read patched eGON image");
        std::fs::remove_file(&path).expect("remove eGON fixture");

        assert_eq!(image.len(), 0x2000);
        assert_eq!(
            u32::from_le_bytes(image[0x10..0x14].try_into().unwrap()),
            0x2000
        );
        verify(&image).expect("patched image validates");
    }

    #[test]
    fn patch_ffs_updates_header_and_checksum() {
        let mut image = egon_image(0x4000);
        patch_ffs(&mut image, 0x2000).expect("patch eGON FFS");

        assert_eq!(
            u32::from_le_bytes(image[0x10..0x14].try_into().unwrap()),
            0x2000
        );
        verify(&image[..0x2000]).expect("patched FFS header validates");
    }
}
