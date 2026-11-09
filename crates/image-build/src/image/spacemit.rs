//! SpacemiT K3 boot ROM images.
//!
//! The K3 boot ROM loads a first-stage boot loader (FSBL) into on-chip SRAM
//! at `0xC080_0000`, from a block device or over USB fastboot
//! (`fastboot stage FSBL.bin; fastboot continue`). The FSBL is an AIHD
//! container: a 4 KiB head carrying the root public key, the key table and
//! the head signature, then the code with its own AIHD header, CRCs and
//! signature. The code therefore runs from `0xC080_1000`.
//!
//! On block devices (SD, eMMC, UFS) the ROM finds the FSBL through the
//! bootinfo block at 1 MiB, which points at two copies (spl0, spl1).
//!
//! Images are built unsigned: the key, hash and signature fields stay zero.
//! A part whose eFuses do not enable secure boot does not check them.

/// SRAM address the ROM loads the FSBL container to.
pub const K3_FSBL_LOAD_ADDR: u64 = 0xC080_0000;
/// Offset of the code in the container; it runs at
/// `K3_FSBL_LOAD_ADDR + FSBL_CODE_OFFSET`.
pub const FSBL_CODE_OFFSET: usize = 0x1000;

/// Byte offset of the bootinfo block on block devices.
pub const K3_BLOCK_BOOTINFO_OFFSET: u64 = 0x10_0000;
/// Byte offsets of the two FSBL copies on block devices.
pub const K3_BLOCK_SPL0_OFFSET: u32 = 0x18_0000;
pub const K3_BLOCK_SPL1_OFFSET: u32 = 0x20_0000;
/// The largest FSBL container the ROM loads from a block device.
pub const K3_BLOCK_SPL_LIMIT: u32 = 0x7_4000;

const AIHD_MAGIC: &[u8; 4] = b"AIHD";
const RSA2048_LEN: usize = 256;
const HEAD0_OFFSET: usize = RSA2048_LEN;
const KEYDATA_OFFSET: usize = HEAD0_OFFSET + 32;
const HEAD1_OFFSET: usize = FSBL_CODE_OFFSET - 32;
const CODE_ALIGN: usize = 32;
/// Signature key slots the head declares, in key-ID order.
const KEY_NAMES: [&str; 4] = ["spl", "uboot", "kernel", "rootfs"];

const BOOTINFO_MAGIC: u32 = 0xB007_14F0;
const BOOTINFO_VERSION: u32 = 0x0001_0001;
const BOOTINFO_LEN: usize = 80;
const BOOTINFO_CRC_LEN: usize = 64;

/// An AIHD header with its CRC word pair: 32 bytes.
fn aihd_header(image_size: u64, load_addr: u64, image_crc: u32) -> [u8; 32] {
    let mut head = [0u8; 32];
    head[0..4].copy_from_slice(AIHD_MAGIC);
    head[4] = 1; // version
    head[5] = 0; // not secure
    head[8..16].copy_from_slice(&image_size.to_le_bytes());
    head[16..24].copy_from_slice(&load_addr.to_le_bytes());
    let crc = crc32fast::hash(&head[..24]);
    head[24..28].copy_from_slice(&crc.to_le_bytes());
    head[28..32].copy_from_slice(&image_crc.to_le_bytes());
    head
}

/// Wrap flat code linked at `0xC080_1000` in an unsigned K3 FSBL container.
pub fn k3_fsbl(code: &[u8]) -> Result<Vec<u8>, String> {
    if code.is_empty() {
        return Err("K3 FSBL code is empty".into());
    }
    let mut code = code.to_vec();
    code.resize(code.len().next_multiple_of(CODE_ALIGN), 0);
    let total = FSBL_CODE_OFFSET + code.len() + RSA2048_LEN;
    if total > K3_BLOCK_SPL_LIMIT as usize {
        return Err(format!(
            "K3 FSBL is {total:#x} bytes, the boot ROM loads at most {K3_BLOCK_SPL_LIMIT:#x}"
        ));
    }

    let mut image = vec![0u8; total];
    image[HEAD0_OFFSET..KEYDATA_OFFSET].copy_from_slice(&aihd_header(
        HEAD1_OFFSET as u64,
        K3_FSBL_LOAD_ADDR,
        0,
    ));

    let keydata = &mut image[KEYDATA_OFFSET..];
    keydata[4..8].copy_from_slice(&(KEY_NAMES.len() as u32).to_le_bytes());
    for (id, name) in KEY_NAMES.iter().enumerate() {
        let entry = &mut keydata[8 + 20 * id..8 + 20 * (id + 1)];
        entry[..name.len()].copy_from_slice(name.as_bytes());
        entry[16..20].copy_from_slice(&(id as u32).to_le_bytes());
    }

    image[HEAD1_OFFSET..FSBL_CODE_OFFSET].copy_from_slice(&aihd_header(
        code.len() as u64,
        0,
        crc32fast::hash(&code),
    ));
    image[FSBL_CODE_OFFSET..FSBL_CODE_OFFSET + code.len()].copy_from_slice(&code);
    Ok(image)
}

/// The block-device bootinfo pointing the ROM at both FSBL copies.
pub fn k3_block_bootinfo() -> [u8; BOOTINFO_LEN] {
    let mut info = [0u8; BOOTINFO_LEN];
    let mut put = |offset: usize, value: u32| {
        info[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    put(0x00, BOOTINFO_MAGIC);
    put(0x04, BOOTINFO_VERSION);
    put(0x10, 512); // page size
    put(0x14, 0x1_0000); // block size
    put(0x18, 0x1000_0000); // total size
    put(0x20, K3_BLOCK_SPL0_OFFSET);
    put(0x24, K3_BLOCK_SPL1_OFFSET);
    put(0x28, K3_BLOCK_SPL_LIMIT);
    info[0x08..0x0C].copy_from_slice(b"BLCK");
    let crc = crc32fast::hash(&info[..BOOTINFO_CRC_LEN]);
    info[0x40..0x44].copy_from_slice(&crc.to_le_bytes());
    info
}

/// The block-device boot area from the bootinfo block (1 MiB) through the
/// second FSBL copy: written at byte offset [`K3_BLOCK_BOOTINFO_OFFSET`] it
/// leaves the partition table in front of it alone.
pub fn k3_block_boot_area(fsbl: &[u8]) -> Result<Vec<u8>, String> {
    if fsbl.len() > K3_BLOCK_SPL_LIMIT as usize {
        return Err("K3 FSBL exceeds the block-device limit".into());
    }
    let base = K3_BLOCK_BOOTINFO_OFFSET as usize;
    let spl0 = K3_BLOCK_SPL0_OFFSET as usize - base;
    let spl1 = K3_BLOCK_SPL1_OFFSET as usize - base;
    let mut area = vec![0u8; spl1 + fsbl.len()];
    area[..BOOTINFO_LEN].copy_from_slice(&k3_block_bootinfo());
    area[spl0..spl0 + fsbl.len()].copy_from_slice(fsbl);
    area[spl1..].copy_from_slice(fsbl);
    Ok(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A K3 FSBL the boot ROM accepts, holding this code and signed with test
    /// keys: every field outside the keys and signatures must match.
    #[test]
    fn fsbl_matches_the_reference_layout() {
        let code: Vec<u8> = (0..1000).map(|i| i as u8).collect();
        let image = k3_fsbl(&code).unwrap();
        assert_eq!(image.len(), 0x1500);
        assert_eq!(
            image[0x100..0x120],
            hex("4149484401000000e00f000000000000000080c00000000056cddbac00000000")
        );
        assert_eq!(
            image[0x120..0x178],
            hex(concat!(
                "000000000400000073706c000000000000000000000000000000000075626f6f",
                "740000000000000000000000010000006b65726e656c00000000000000000000",
                "02000000726f6f7466730000000000000000000003000000"
            ))
        );
        assert_eq!(
            image[0xfe0..0x1000],
            hex("414948440100000000040000000000000000000000000000d8f736e0d8addd71")
        );
        assert_eq!(image[0x1000..0x1000 + 1000], code[..]);
        assert!(image[0x1000 + 1000..].iter().all(|&b| b == 0));
    }

    #[test]
    fn bootinfo_matches_the_reference_bytes() {
        assert_eq!(
            k3_block_bootinfo()[..],
            hex(concat!(
                "f01407b001000100424c434b0000000000020000000001000000001000000000",
                "0000180000002000004007000000000000000000000000000000000000000000",
                "368817f9000000000000000000000000"
            ))[..]
        );
    }

    #[test]
    fn rejects_code_the_rom_cannot_load() {
        let limit = K3_BLOCK_SPL_LIMIT as usize - FSBL_CODE_OFFSET - RSA2048_LEN;
        assert!(k3_fsbl(&vec![1; limit]).is_ok());
        assert!(k3_fsbl(&vec![1; limit + 1]).is_err());
        assert!(k3_fsbl(&[]).is_err());
    }

    #[test]
    fn boot_area_places_both_copies() {
        let fsbl = k3_fsbl(&[0x13; 64]).unwrap();
        let area = k3_block_boot_area(&fsbl).unwrap();
        assert_eq!(area[..80], k3_block_bootinfo());
        assert_eq!(area[0x8_0000..0x8_0000 + fsbl.len()], fsbl[..]);
        assert_eq!(area[0x10_0000..], fsbl[..]);
    }
}
