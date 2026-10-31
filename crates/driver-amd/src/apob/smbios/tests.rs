use super::*;
use alloc::vec;
use fstart_arch::x86::boot::cpuid::CacheInfo;
use fstart_core::memory_info::{
    FORM_FACTOR_DIMM, MEMORY_TYPE_DDR4, TYPE_DETAIL_REGISTERED, TYPE_DETAIL_SYNCHRONOUS,
};

fn ids(socket: u32) -> Vec<u32> {
    (0..8)
        .flat_map(|ccd| {
            (0..6).flat_map(move |core| {
                (0..2).map(move |thread| (socket << 7) | (ccd << 4) | (core << 1) | thread)
            })
        })
        .collect()
}
fn snapshot() -> ProcessorSnapshot {
    let mut brand = [0; 48];
    brand[..13].copy_from_slice(b"AMD EPYC 7643");
    let cache = |level, cache_type, size_bytes, associativity, shared_logical| CacheInfo {
        level,
        cache_type,
        size_bytes,
        associativity,
        shared_logical,
        line_size: 64,
        fully_associative: false,
    };
    ProcessorSnapshot {
        vendor: *b"AuthenticAMD",
        brand,
        signature: 0x00a00f11,
        standard_features: 0x178bfbff,
        standard_features_ecx: 0,
        extended_features: (1 << 29) | (1 << 20),
        extended_features_ecx: 1 << 2,
        core_count: 48,
        caches: [
            cache(1, 1, 32768, 8, 2),
            cache(1, 2, 32768, 8, 2),
            cache(2, 3, 512 * 1024, 8, 2),
            cache(3, 3, 32 * 1024 * 1024, 32, 16),
        ],
        cache_count: 4,
    }
}
fn dimms() -> Vec<Dimm> {
    (0..2)
        .flat_map(|socket| {
            (0..8).flat_map(move |channel| {
                (0..2).map(move |slot| {
                    let device = if slot == 0 {
                        MemoryDevice {
                            size_mib: 16384.into(),
                            configured_mts: 2933.into(),
                            max_mts: 3200.into(),
                            total_width: 72.into(),
                            data_width: 64.into(),
                            memory_type: MEMORY_TYPE_DDR4,
                            form_factor: FORM_FACTOR_DIMM,
                            type_detail: (TYPE_DETAIL_SYNCHRONOUS | TYPE_DETAIL_REGISTERED).into(),
                            ranks: 2,
                            channel,
                            slot,
                            ..MemoryDevice::default()
                        }
                    } else {
                        MemoryDevice::empty(channel, slot)
                    };
                    Dimm {
                        socket,
                        die: 0,
                        device,
                    }
                })
            })
        })
        .collect()
}
fn records(bytes: &[u8]) -> Vec<&[u8]> {
    let mut result = Vec::new();
    let mut offset = 24;
    while offset < bytes.len() {
        let len = bytes[offset + 1] as usize;
        assert!(len >= 4);
        result.push(&bytes[offset..offset + len]);
        offset += len;
        while bytes[offset..offset + 2] != [0, 0] {
            offset += 1;
        }
        offset += 2;
    }
    result
}
fn word(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn dword(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[test]
fn two_packages_and_32_positions_without_client_inventory_limit() {
    let ids0 = ids(0);
    let ids1 = ids(1);
    let processors = [
        Processor {
            socket: 0,
            snapshot: snapshot(),
            apic_ids: &ids0,
            enabled_cores: 48,
        },
        Processor {
            socket: 1,
            snapshot: snapshot(),
            apic_ids: &ids1,
            enabled_cores: 48,
        },
    ];
    let dimms = dimms();
    let regions = [
        DramRange {
            base: 0,
            end: 3 << 30,
        },
        DramRange {
            base: 4 << 30,
            end: 257 << 30,
        },
    ];
    let mut bytes = vec![0; smbios::BUFFER_SIZE];
    let used = build_into(
        &mut bytes,
        0x7500_0000,
        &Inputs {
            identity: SmbiosIdentity::new("Test Vendor", "Test Board"),
            processors: &processors,
            dimms: &dimms,
            regions: &regions,
            max_dimm_mib: 256 * 1024,
            socket_upgrade: 0x37,
            ecc: true,
            rom_size: 16 << 20,
            uefi: true,
        },
    )
    .unwrap();
    let records = records(&bytes[..used]);
    let cpus: Vec<_> = records.iter().filter(|record| record[0] == 4).collect();
    assert_eq!(cpus.len(), 2);
    for cpu in cpus {
        assert_eq!(
            (word(cpu, 0x2a), word(cpu, 0x2c), word(cpu, 0x2e)),
            (48, 48, 96)
        );
        assert_eq!(word(cpu, 0x28), 0x6b);
        assert_eq!(cpu[0x19], 0x37);
        assert_ne!(word(cpu, 0x1e), 0xffff);
    }
    let l3s: Vec<_> = records
        .iter()
        .filter(|record| record[0] == 7 && word(record, 5) & 7 == 2)
        .collect();
    assert_eq!(l3s.len(), 2);
    for cache in l3s {
        assert_eq!(word(cache, 9), 0x9000); // 256 MiB/socket, not 32 MiB or 3 GiB
        assert_eq!(cache[18], 0x0b); // 32-way
    }
    let array = records.iter().find(|record| record[0] == 16).unwrap();
    assert_eq!(word(array, 13), 32);
    assert_eq!(array[6], 6);
    assert_eq!(dword(array, 7), 0x80000000);
    assert_eq!(
        u64::from_le_bytes(array[15..23].try_into().unwrap()),
        8 << 40
    );
    let devices: Vec<_> = records.iter().filter(|record| record[0] == 17).collect();
    assert_eq!(devices.len(), 32);
    assert_eq!(
        devices
            .iter()
            .filter(|device| word(device, 12) != 0)
            .count(),
        16
    );
    for device in devices {
        assert_eq!(word(device, 4), word(array, 2));
    }
    let mappings: Vec<_> = records.iter().filter(|record| record[0] == 19).collect();
    assert_eq!(mappings.len(), 2);
    assert_eq!(
        (dword(mappings[0], 4), dword(mappings[0], 8)),
        (0, (3 << 20) - 1)
    );
    assert_eq!(
        (dword(mappings[1], 4), dword(mappings[1], 8)),
        (4 << 20, (257 << 20) - 1)
    );
    for mapping in mappings {
        assert_eq!(word(mapping, 12), word(array, 2));
    }
    assert_eq!(records.last().unwrap()[0], 127);
}

#[test]
fn present_unknown_is_not_empty_and_small_buffers_do_not_publish() {
    let dimms = [
        Dimm {
            socket: 0,
            die: 0,
            device: MemoryDevice::unknown(0, 0),
        },
        Dimm {
            socket: 0,
            die: 0,
            device: MemoryDevice::empty(0, 1),
        },
    ];
    let input = Inputs {
        identity: SmbiosIdentity::new("Test", "Board"),
        processors: &[],
        dimms: &dimms,
        regions: &[],
        max_dimm_mib: 256 * 1024,
        socket_upgrade: 0x37,
        ecc: false,
        rom_size: 16 << 20,
        uefi: false,
    };
    let mut bytes = vec![0; 2048];
    let used = build_into(&mut bytes, 0x1000, &input).unwrap();
    let records = records(&bytes[..used]);
    let devices: Vec<_> = records.iter().filter(|record| record[0] == 17).collect();
    assert_eq!(word(devices[0], 12), 0xffff);
    assert_eq!(word(devices[1], 12), 0);
    let mut tiny = [0xa5; 64];
    assert_eq!(
        build_into(&mut tiny, 0x1000, &input),
        Err(WriteError::TableTooLarge)
    );
    assert_eq!(&tiny[..5], &[0; 5]);
}
