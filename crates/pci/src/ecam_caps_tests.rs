extern crate std;
use super::*;
#[test]
fn conventional_capability_walk_is_bounded_and_rejects_cycles_and_bad_pointers() {
    let mut ram = [0u32; 1024];
    let device = EcamDevice::at_config_base(PciAddress::new(0, 0, 0, 0), ram.as_mut_ptr() as usize);
    device.write32(0, 0x12341022);
    device.write16(0x06, 0x10);
    device.write8(0x34, 0x40);
    device.write16(0x40, 0x6005);
    device.write16(0x60, 0x0010);
    assert_eq!(device.find_capability(0x10), Some(0x60));
    device.write8(0x61, 0x40);
    assert_eq!(device.find_capability(0x11), None);
    device.write8(0x34, 0x41);
    assert_eq!(device.find_capability(0x10), None);
    device.write8(0x34, 0xfc);
    device.write16(0xfc, 0x0010);
    assert_eq!(device.find_capability(0x10), Some(0xfc));
    device.write8(0x0e, 2);
    assert_eq!(device.find_capability(0x10), None);
}

#[test]
fn extended_capability_walk_is_bounded_and_rejects_cycles() {
    let mut ram = [0u32; 1024];
    let device = EcamDevice::at_config_base(PciAddress::new(0, 0, 0, 0), ram.as_mut_ptr() as usize);
    device.write32(0, 0x12341022);
    assert_eq!(device.ext_capabilities().count(), 0);
    device.write32(0x100, 0x1401_0001); // AER -> 0x140
    device.write32(0x140, 0x2001_000b); // VSEC -> 0x200
    device.write32(0x200, 0x0001_0010); // SR-IOV, end
    let walk: std::vec::Vec<_> = device.ext_capabilities().map(|(o, _)| o).collect();
    assert_eq!(walk, [0x100, 0x140, 0x200]);
    assert_eq!(device.find_ext_capability(0x10), Some(0x200));
    assert_eq!(device.find_ext_capability(0x2e), None);
    device.write32(0x200, 0x1001_0010); // cycle back to 0x100
    assert_eq!(device.ext_capabilities().count(), 3);
    device.write32(0x140, 0x2021_000b); // misaligned pointer
    assert_eq!(device.ext_capabilities().count(), 2);
    // A null capability (ID 0) with a next pointer does not end the list;
    // a header of 0 is the end of one (next 0).
    device.write32(0x140, 0x2000_0000); // null -> 0x200
    device.write32(0x200, 0x0001_0010);
    let walk: std::vec::Vec<_> = device.ext_capabilities().map(|(o, _)| o).collect();
    assert_eq!(walk, [0x100, 0x140, 0x200]);
    assert_eq!(device.find_ext_capability(0x10), Some(0x200));
    // An absent function has no list.
    device.write32(0, u32::MAX);
    assert_eq!(device.ext_capabilities().count(), 0);
}

#[test]
fn dvsec_checks_vendor_id_length_and_pointer_chain() {
    let mut ram = [0u32; 1024];
    let device = EcamDevice::at_config_base(PciAddress::new(0, 0, 0, 0), ram.as_mut_ptr() as usize);
    device.write32(0, 0x12341022);
    device.write32(0x100, 0x1801_0023);
    device.write32(0x104, 0x0100_1234);
    device.write32(0x180, 0x0001_0023);
    device.write32(0x184, 0x0100_1e98);
    device.write16(0x188, 7);
    assert_eq!(device.find_dvsec(0x1e98, 7, 16), Some(0x180));
    assert_eq!(device.find_dvsec(0x1e98, 0, 16), None);
    assert_eq!(device.find_dvsec(0x1e98, 7, 17), None);
    device.write32(0x180, 0x1841_0023); // overlaps declared body
    assert_eq!(device.find_dvsec(0x1e98, 7, 16), None);
    device.write32(0x180, 0x1001_0001); // cycle
    assert_eq!(device.find_dvsec(0x1e98, 7, 16), None);
    device.write32(0x180, 0xff81_0001); // incomplete header
    device.write32(0xff8, 0x0001_0023);
    device.write32(0xffc, 0x0100_1e98);
    assert_eq!(device.find_dvsec(0x1e98, 7, 16), None);
}
