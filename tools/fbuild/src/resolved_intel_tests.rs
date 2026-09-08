use super::*;

fn profile() -> Profile {
    let manifest: toml::Value =
        toml::from_str(include_str!("../../../crates/platform-intel/Cargo.toml")).unwrap();
    manifest["package"]["metadata"]["fstart"]["layouts"]["gm965-car"]
        .clone()
        .try_into()
        .unwrap()
}

#[test]
fn intel_profile_keeps_full_bios_identity_and_fixed_filesystem_boundary() {
    let mut p = profile();
    p.reservations.validate().unwrap();
    let ifd = resolve_ifd(&p.ifd, &p.reservations).unwrap();
    assert_eq!(ifd.bios_base(), Some(p.reservations.firmware.base));
    assert_eq!(p.reservations.firmware.size, 0x180000);
    assert_eq!(p.reservations.filesystem_capacity().unwrap(), 0x140000);
    p.reservations.bootblock.image.base -= 4096;
    p.reservations.bootblock.image.size += 4096;
    p.reservations.validate().unwrap();
    assert_eq!(p.reservations.filesystem_capacity().unwrap(), 0x13f000);
    assert!(resolve_ifd(&p.ifd, &p.reservations).is_ok());
    p.reservations.firmware.size = p.reservations.filesystem_capacity().unwrap();
    assert!(p.reservations.validate().is_err());
    assert!(resolve_ifd(&p.ifd, &p.reservations).is_err());
}

#[test]
fn ifd_rejects_overlap_duplicate_and_overflow_before_projection() {
    let mut p = profile();
    p.ifd[1].offset = 0;
    assert!(resolve_ifd(&p.ifd, &p.reservations).is_err());
    let mut p = profile();
    p.ifd[1].kind = IfdKind::Descriptor;
    assert!(resolve_ifd(&p.ifd, &p.reservations).is_err());
    let mut p = profile();
    p.ifd[1].offset = u32::MAX - 4095;
    assert!(resolve_ifd(&p.ifd, &p.reservations).is_err());
}
