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
