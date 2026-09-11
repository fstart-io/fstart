use std::path::Path;

const EGON_MAGIC: &[u8; 8] = b"eGON.BT0";
const CHECKSUM_STAMP: u32 = 0x5F0A6C39;
const HEADER_LEN: usize = 96;
const SPL_SIGNATURE: &[u8; 4] = b"SPL\x02";

/// Pad a flat SPL to the eGON 8-KiB granule and fill its length, signature
/// and checksum. The FFS assembler applies this to the initial stage before
/// packing; `patch_ffs` later reseals the checksum once the locator is set.
pub fn prepare_image(data: &mut Vec<u8>) -> Result<(), String> {
    if data.len() < HEADER_LEN {
        return Err("binary too small for Allwinner eGON header (< 96 bytes)".to_string());
    }
    if &data[4..12] != EGON_MAGIC {
        return Err("eGON.BT0 magic not found at offset 0x04".to_string());
    }

    let stamp = u32::from_le_bytes([data[0x0C], data[0x0D], data[0x0E], data[0x0F]]);
    if stamp != CHECKSUM_STAMP {
        return Err(format!(
            "eGON checksum sentinel not found (got {stamp:#010x}, expected {CHECKSUM_STAMP:#010x})"
        ));
    }

    let raw_size = data.len();
    let image_size = ((raw_size + 0x1FFF) & !0x1FFF) as u32;
    data.resize(image_size as usize, 0);
    data[0x10..0x14].copy_from_slice(&image_size.to_le_bytes());
    data[0x14..0x18].copy_from_slice(SPL_SIGNATURE);

    let checksum = word_sum(data);
    data[0x0C..0x10].copy_from_slice(&checksum.to_le_bytes());
    verify(data)?;

    eprintln!(
        "[fstart] Allwinner eGON patched: raw_size={raw_size:#x}, \
         image_size={image_size:#x}, checksum={checksum:#010x}"
    );
    Ok(())
}

pub fn patch_file(bin_path: &Path) -> Result<(), String> {
    let mut data = std::fs::read(bin_path).map_err(|e| format!("failed to read binary: {e}"))?;
    prepare_image(&mut data)?;
    std::fs::write(bin_path, &data).map_err(|e| format!("failed to write patched binary: {e}"))
}

pub fn verify(data: &[u8]) -> Result<(), String> {
    if data.len() < HEADER_LEN {
        return Err("verify: binary too small".to_string());
    }

    let is_arm_branch = data[3] == 0xEA;
    let is_riscv_jal = (data[0] & 0x7F) == 0x6F;
    if !is_arm_branch && !is_riscv_jal {
        return Err(format!(
            "verify: unrecognized branch instruction (bytes: {:#04x} {:#04x} {:#04x} {:#04x})",
            data[0], data[1], data[2], data[3]
        ));
    }

    if &data[4..12] != EGON_MAGIC {
        return Err("verify: eGON.BT0 magic mismatch".to_string());
    }

    let length = u32::from_le_bytes([data[0x10], data[0x11], data[0x12], data[0x13]]);
    let saved_checksum = u32::from_le_bytes([data[0x0C], data[0x0D], data[0x0E], data[0x0F]]);

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

    let mut verify_sum: u32 = 0;
    for i in 0..(length as usize / 4) {
        let off = i * 4;
        let word = if i == 3 {
            CHECKSUM_STAMP
        } else {
            u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
        };
        verify_sum = verify_sum.wrapping_add(word);
    }

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
    if &ffs_image[4..12] != EGON_MAGIC {
        return Err("eGON.BT0 magic not found at start of FFS image".to_string());
    }

    ffs_image[0x10..0x14].copy_from_slice(&bootblock_size.to_le_bytes());
    ffs_image[0x14..0x18].copy_from_slice(SPL_SIGNATURE);
    ffs_image[0x0C..0x10].copy_from_slice(&CHECKSUM_STAMP.to_le_bytes());

    let checksum = word_sum(&ffs_image[..bootblock_size as usize]);
    ffs_image[0x0C..0x10].copy_from_slice(&checksum.to_le_bytes());
    verify(&ffs_image[..bootblock_size as usize])?;

    eprintln!(
        "[fstart] eGON patched in FFS: bootblock_size={bootblock_size:#x}, \
         checksum={checksum:#010x}"
    );
    Ok(())
}

fn word_sum(data: &[u8]) -> u32 {
    data.chunks_exact(4).fold(0u32, |checksum, chunk| {
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        checksum.wrapping_add(word)
    })
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
