use super::*;

#[repr(align(8))]
struct Buffer([u8; 16 * 1024]);

fn minimal() -> [u8; 72] {
    let mut bytes = [0; 72];
    for (offset, value) in [
        (0, 0xd00d_feedu32),
        (4, 72),
        (8, 56),
        (12, 72),
        (16, 40),
        (20, 17),
        (24, 16),
        (36, 16),
        (56, 1),
        (64, 2),
        (68, 9),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    bytes
}

#[test]
fn malformed_source_ranges_and_small_capacity_fail_before_copy() {
    #[cfg(feature = "ffs")]
    crate::loaded::initialize();
    let mut source = Buffer([0; 16 * 1024]);
    let mut destination = Buffer([0x77; 16 * 1024]);
    source.0[..72].copy_from_slice(&minimal());
    let source_addr = source.0.as_ptr() as u64;
    let destination_addr = destination.0.as_mut_ptr() as u64;
    let config = Workspace {
        source: MemoryWindow {
            start: source_addr,
            size: 72,
        },
        destination: MemoryWindow {
            start: destination_addr,
            size: 71,
        },
    };
    assert!(prepare_in(config, source_addr, destination_addr, None).is_err());
    assert_eq!(destination.0[0], 0x77);
    let config = Workspace {
        destination: MemoryWindow {
            size: 16 * 1024,
            ..config.destination
        },
        ..config
    };
    source.0[4..8].copy_from_slice(&0x10000u32.to_be_bytes());
    assert!(prepare_in(config, source_addr, destination_addr, None).is_err());
    assert_eq!(destination.0[0], 0x77);
    source.0[..72].copy_from_slice(&minimal());
    assert_eq!(
        prepare_in(config, source_addr, destination_addr, None).unwrap(),
        72
    );
    assert_eq!(&destination.0[..72], &minimal());
    source.0[16..20].copy_from_slice(&64u32.to_be_bytes());
    assert!(prepare_in(config, source_addr, destination_addr, None).is_err());
}

#[cfg(feature = "fdt")]
#[test]
fn cumulative_patch_growth_is_bounded_before_mutation() {
    #[cfg(feature = "ffs")]
    crate::loaded::initialize();
    let mut buffer = Buffer([0; 16 * 1024]);
    buffer.0[..72].copy_from_slice(&minimal());
    let address = buffer.0.as_mut_ptr() as u64;
    let region = MemoryWindow {
        start: address,
        size: 72 + PATCH_GROWTH - 1,
    };
    let config = Workspace {
        source: region,
        destination: region,
    };
    assert!(prepare_in(config, address, address, Some(("console=ttyS0", 0, 0))).is_err());
    assert_eq!(&buffer.0[..72], &minimal());
}
