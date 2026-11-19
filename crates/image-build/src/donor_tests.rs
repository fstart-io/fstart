// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use std::io::Write;

/// A ZIP of `members`, as a vendor update ships them.
pub(crate) fn zip(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in members {
        archive.start_file(*name, options).unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

/// "Firmware" of these tests: an image starting with 0x5a.
fn ours(image: &[u8]) -> bool {
    image[0] == 0x5a
}

#[test]
fn a_raw_image_is_its_own_donor() {
    let raw = vec![0xff; 1 << 20];
    let image = image(&raw, &ours).unwrap();
    assert!(matches!(image.bytes, Cow::Borrowed(bytes) if bytes == raw));
    assert_eq!(image.member, None);
}

#[test]
fn a_vendor_zip_donates_the_image_with_the_boards_firmware() {
    let rom = vec![0x5a; 1 << 20];
    // A capsule and a signed update of the same size, the flash tool, an
    // image of another board and the ROM, as MiTAC's updates have them.
    let capsule = vec![0x5a; 1 << 20];
    let mut signed = vec![0; 101];
    signed.extend_from_slice(&rom);
    let other = vec![0xff; 1 << 20];
    let donor = zip(&[
        ("flash.nsh", b"AfuEfix64.efi BIOS.rom /p /b /n /x /k /l"),
        ("8040_001.CAP", &capsule),
        ("BIOS.hpm", &signed),
        ("AfuEfix64.efi", &[0; 4096]),
        ("other/BIOS.bin", &other),
        ("BIOS8040020200.ROM", &rom),
    ]);
    let image = image(&donor, &ours).unwrap();
    assert_eq!(image.bytes, rom);
    assert_eq!(image.member.as_deref(), Some("BIOS8040020200.ROM"));
}

#[test]
fn copies_of_one_image_are_one() {
    // Vendor updates ship one copy per flash tool (flashabl.rom, image.bin).
    let rom = vec![0x5a; 1 << 20];
    let donor = zip(&[("x/flashabl.rom", &rom), ("x/SPI_UPD/image.bin", &rom)]);
    let image = image(&donor, &ours).unwrap();
    assert_eq!(image.bytes, rom);
    assert_eq!(image.member.as_deref(), Some("x/flashabl.rom"));
}

#[test]
fn a_zip_without_one_image_for_the_board_is_no_donor() {
    let (a, b) = (vec![0x5a; 1 << 20], [vec![0x5a], vec![1; (1 << 20) - 1]]);
    let error = image(&zip(&[("a.rom", &a), ("b.fd", &b.concat())]), &ours).unwrap_err();
    assert!(
        error.contains("different") && error.contains("a.rom, b.fd"),
        "{error}"
    );
    let error = image(&zip(&[("other.rom", &[0xff; 1 << 20])]), &ours).unwrap_err();
    assert!(
        error.contains("no ROM/bin/fd image with firmware"),
        "{error}"
    );
    // Small members are no flash image.
    let error = image(&zip(&[("tiny.rom", &[0x5a; 4096])]), &ours).unwrap_err();
    assert!(error.contains("holds no ROM/bin/fd image"), "{error}");
    assert!(image(&zip(&[]), &ours).is_err());
}

#[test]
fn a_zip_of_many_images_is_not_expanded() {
    let rom = vec![0x5a; 1 << 20];
    let names: Vec<String> = (0..=MAX_DONOR_COPIES)
        .map(|i| format!("copy{i}.rom"))
        .collect();
    let members: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &rom[..])).collect();
    let error = image(&zip(&members), &ours).unwrap_err();
    assert!(error.contains("too many"), "{error}");
}

/// A ZIP of `members`, stored: names and headers at known bytes.
fn stored(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in members {
        archive.start_file(*name, options).unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

/// Where `bytes` (a ZIP header signature, a name) are in `zip`, in order.
fn headers(zip: &[u8], bytes: &[u8]) -> Vec<usize> {
    (0..=zip.len() - bytes.len())
        .filter(|&at| zip[at..].starts_with(bytes))
        .collect()
}

#[test]
fn a_zip_naming_a_member_twice_is_no_donor() {
    // Two different images of the board under one name: a ZIP reader
    // would take one and hide the other.
    let (a, b) = (vec![0x5a; 1 << 20], [vec![0x5a], vec![1; (1 << 20) - 1]]);
    let mut donor = stored(&[("BIOS.ROM", &a), ("BIOS.RON", &b.concat())]);
    for at in headers(&donor, b"BIOS.RON") {
        donor[at + 7] = b'M';
    }
    let error = image(&donor, &ours).unwrap_err();
    assert!(error.contains("names a member twice"), "{error}");
}

#[test]
fn an_unreadable_image_member_is_an_error() {
    let rom = vec![0x5a; 1 << 20];
    let mut donor = stored(&[("BIOS.ROM", &rom), ("copy.rom", &rom)]);
    // The second member's local header is not one.
    let local = headers(&donor, b"PK\x03\x04");
    donor[local[1] + 3] = 0;
    let error = image(&donor, &ours).unwrap_err();
    assert!(error.contains("copy.rom"), "{error}");
}

#[test]
fn a_second_image_ends_the_search() {
    let (a, b) = (vec![0x5a; 1 << 20], [vec![0x5a], vec![1; (1 << 20) - 1]]);
    let mut donor = stored(&[("a.rom", &a), ("b.rom", &b.concat()), ("c.rom", &a)]);
    // A member after the second image is never read.
    let local = headers(&donor, b"PK\x03\x04");
    donor[local[2] + 3] = 0;
    let error = image(&donor, &ours).unwrap_err();
    assert!(
        error.contains("different") && error.contains("(a.rom, b.rom)"),
        "{error}"
    );
}

#[test]
fn a_zip_expanding_too_far_is_not_expanded() {
    let rom = vec![0x5a; 1 << 20];
    let mut donor = stored(&[("a.rom", &rom), ("b.rom", &rom), ("c.rom", &rom)]);
    // Members claiming 100 MiB each, together more than two images.
    for at in headers(&donor, b"PK\x01\x02") {
        donor[at + 24..at + 28].copy_from_slice(&(100u32 << 20).to_le_bytes());
    }
    let error = image(&donor, &ours).unwrap_err();
    assert!(error.contains("too many"), "{error}");
}
