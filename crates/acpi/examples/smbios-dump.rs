//! Synthetic, hardware-independent SMBIOS fixture for an external decoder:
//! cargo run -p fstart-acpi --example smbios-dump -- /tmp/smbios.bin
//! dmidecode --from-dump /tmp/smbios.bin
//! No firmware or physical memory access; refuses to overwrite an existing file.

use fstart_acpi::smbios::{self, ProcessorInfo, SmbiosIdentity};
use fstart_core::{
    memory_info::{
        FORM_FACTOR_DIMM, MEMORY_TYPE_DDR4, MemoryDevice, TYPE_DETAIL_REGISTERED,
        TYPE_DETAIL_SYNCHRONOUS,
    },
    smbios::CacheAssociativity,
};
use std::{fs::OpenOptions, io::Write};

fn main() -> std::io::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .expect("usage: smbios-dump OUTPUT_FILE");
    let mut bytes = vec![0; smbios::BUFFER_SIZE];
    // dmidecode's dump format puts the table at file offset 32. The entry
    // point itself is relocated to offset zero when writing the dump below.
    let used = smbios::assemble_into(
        &mut bytes,
        (32 - smbios::ENTRY_POINT_SIZE) as u64,
        |writer| {
            writer.add_identity(
                &SmbiosIdentity::new("Example Vendor", "Example Server"),
                16 << 20,
                true,
            );
            let l3 = writer.add_cache_info(
                "Example L3",
                3,
                256 * 1024,
                CacheAssociativity::from_ways(32, false).to_smbios_byte(),
                0x05,
            );
            writer.add_processor(&ProcessorInfo {
                socket: "Example Socket",
                manufacturer: "Example Vendor",
                version: "Example CPU",
                family: 0x02,
                id: 0,
                external_clock_mhz: 0,
                max_speed_mhz: 0,
                current_speed_mhz: 0,
                upgrade: 0x02,
                core_count: 48,
                core_enabled: 48,
                thread_count: 96,
                characteristics: 0,
                caches: [0xffff, 0xffff, l3],
            });
            writer.add_physical_memory_array(8 << 30, 32, 6);
            for slot in 0..32 {
                let device = if slot == 0 {
                    MemoryDevice {
                        size_mib: 16384.into(),
                        max_mts: 3200.into(),
                        configured_mts: 2933.into(),
                        data_width: 64.into(),
                        total_width: 72.into(),
                        memory_type: MEMORY_TYPE_DDR4,
                        form_factor: FORM_FACTOR_DIMM,
                        ranks: 2,
                        slot,
                        type_detail: (TYPE_DETAIL_SYNCHRONOUS | TYPE_DETAIL_REGISTERED).into(),
                        ..MemoryDevice::default()
                    }
                } else {
                    MemoryDevice::empty(0, slot)
                };
                writer.add_dimm(&device, &format!("Example DIMM {slot}"), "Example Bank");
            }
            writer.add_memory_array_mapped_address(0, (16 << 30) - 1, 1);
            writer.add_system_boot_info();
            writer.add_end_of_table();
        },
    )
    .expect("synthetic inventory fits the bounded buffer");
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&bytes[..smbios::ENTRY_POINT_SIZE])?;
    file.write_all(&[0; 32 - smbios::ENTRY_POINT_SIZE])?;
    file.write_all(&bytes[smbios::ENTRY_POINT_SIZE..used])
}
