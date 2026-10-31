//! Shared Intel/x86 ACPI and SMBIOS table handoff helpers.
//!
//! Direct helpers over `fstart-acpi`/`fstart-acpi::smbios`: firmware-store
//! entries, table assembly, RSDP EBDA install, and the acpixtract
//! hex dump. Chipset flows call these in `emit_tables`; drivers and the
//! mainboard contribute fragments through the shared `AcpiDevice`
//! abstraction. There is no capability layer in between.

extern crate alloc;

#[cfg(feature = "acpi")]
use alloc::vec::Vec;

#[cfg(feature = "smbios")]
pub use fstart_acpi::smbios::SmbiosIdentity;

#[cfg(feature = "acpi")]
const RSDP_LEN: usize = 36;
#[cfg(feature = "acpi")]
const XSDT_OFF: usize = 48;
#[cfg(feature = "acpi")]
const FADT_X_DSDT_OFF: usize = 140;
#[cfg(feature = "acpi")]
const BDA_EBDA_SEG_PTR: *mut u16 = 0x040e as *mut u16;
#[cfg(feature = "acpi")]
const BDA_CONVENTIONAL_MEM_KB: *mut u16 = 0x0413 as *mut u16;
#[cfg(feature = "acpi")]
const EBDA_BASE: usize = 0x0009_f000;
#[cfg(feature = "acpi")]
const EBDA_SIZE: usize = 0x1000;
#[cfg(feature = "acpi")]
const EBDA_RSDP_OFFSET: usize = 0;

#[cfg(feature = "acpi")]
fn read_le_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let raw = bytes.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_le_bytes(raw.try_into().ok()?))
}

#[cfg(feature = "acpi")]
fn acpi_table_len(table: &[u8]) -> Option<usize> {
    let raw = table.get(4..8)?;
    let len = u32::from_le_bytes(raw.try_into().ok()?) as usize;
    (len >= 36 && len <= table.len()).then_some(len)
}

#[cfg(feature = "acpi")]
fn print_hex_byte(byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    fstart_log::raw_write_byte(HEX[(byte >> 4) as usize]);
    fstart_log::raw_write_byte(HEX[(byte & 0x0f) as usize]);
}

#[cfg(feature = "acpi")]
fn print_acpixtract_hexdump(data: &[u8]) {
    for (offset, line) in data.chunks(16).enumerate() {
        let byte_offset = offset * 16;
        let mut w = fstart_log::writer();
        let _ = ufmt::uwrite!(w, "    {:04X}:", byte_offset);
        for byte in line {
            fstart_log::raw_write_byte(b' ');
            print_hex_byte(*byte);
        }
        for _ in line.len()..16 {
            let _ = ufmt::uwrite!(w, "   ");
        }
        let _ = ufmt::uwrite!(w, "  ");
        for byte in line {
            let ch = if byte.is_ascii_graphic() || *byte == b' ' {
                *byte
            } else {
                b'.'
            };
            fstart_log::raw_write_byte(ch);
        }
        fstart_log::raw_write_byte(b'\r');
        fstart_log::raw_write_byte(b'\n');
        fstart_log::flush();
    }
}

#[cfg(feature = "acpi")]
fn print_acpi_table(data: &[u8]) {
    let Some(len) = acpi_table_len(data) else {
        return;
    };
    let sig = &data[..4];
    let sig_str = core::str::from_utf8(sig).unwrap_or("????");
    // The writer holds the console lock; release it before the hexdump takes
    // it again per line.
    {
        let mut w = fstart_log::writer();
        let _ = ufmt::uwriteln!(w, "{} @ 0x0000000000000000", sig_str);
    }
    print_acpixtract_hexdump(&data[..len]);
    fstart_log::raw_write_byte(b'\r');
    fstart_log::raw_write_byte(b'\n');
}

#[cfg(feature = "acpi")]
fn install_rsdp_in_ebda(rsdp: &[u8]) {
    if rsdp.len() < RSDP_LEN {
        return;
    }
    // SAFETY: 0x40e is the BIOS Data Area EBDA segment pointer and
    // 0x413 is the conventional-memory size in KiB on x86 PC compatibles.
    // Put EBDA at 0x9f000, matching the low-memory e820 reservation used by
    // Pineview, then copy a low RSDP there like coreboot's low-table RSDP
    // copy. This keeps ACPI discoverable for legacy scanners in addition to
    // the Linux boot_params pointer to the high ACPI table set.
    unsafe {
        core::ptr::write_volatile(BDA_EBDA_SEG_PTR, (EBDA_BASE >> 4) as u16);
        core::ptr::write_volatile(BDA_CONVENTIONAL_MEM_KB, (EBDA_BASE / 1024) as u16);
        core::ptr::write_bytes(EBDA_BASE as *mut u8, 0, EBDA_SIZE);
        core::ptr::copy_nonoverlapping(
            rsdp.as_ptr(),
            (EBDA_BASE + EBDA_RSDP_OFFSET) as *mut u8,
            RSDP_LEN,
        );
    }
    fstart_log::info!(
        "ACPI: EBDA at {}, RSDP copied to {}",
        fstart_log::Hex(EBDA_BASE as u64),
        fstart_log::Hex((EBDA_BASE + EBDA_RSDP_OFFSET) as u64)
    );
}

#[cfg(feature = "acpi")]
fn print_acpi_tables_acpixtract(data: &[u8]) {
    fstart_log::info!("Printing ACPI tables in ACPICA compatible format");
    // Layout from fstart_acpi::platform::assemble(): RSDP, XSDT, DSDT, FADT,
    // then platform/extra SDTs.  RSDP is not printed by coreboot's acpidump
    // loop; it prints DSDT first, then every XSDT entry.
    if data.len() < RSDP_LEN {
        fstart_log::warn!("ACPI dump: buffer too small");
        return;
    }
    let Some(xsdt_addr) = read_le_u64(data, 24) else {
        fstart_log::warn!("ACPI dump: truncated RSDP XSDT pointer");
        return;
    };
    let Some(base_addr) = xsdt_addr.checked_sub(XSDT_OFF as u64) else {
        fstart_log::warn!(
            "ACPI dump: invalid XSDT address {}",
            fstart_log::Hex(xsdt_addr)
        );
        return;
    };
    let Some(xsdt_off) = xsdt_addr.checked_sub(base_addr).map(|v| v as usize) else {
        return;
    };
    let Some(xsdt_len) = acpi_table_len(data.get(xsdt_off..).unwrap_or(&[])) else {
        fstart_log::warn!("ACPI dump: XSDT not found at offset {}", xsdt_off);
        return;
    };
    let xsdt = &data[xsdt_off..xsdt_off + xsdt_len];
    fstart_log::info!("ACPI dump: XSDT offset {} length {}", xsdt_off, xsdt_len);
    let entries = (xsdt_len.saturating_sub(36)) / 8;
    if entries == 0 || xsdt.len() < 44 {
        fstart_log::warn!("ACPI dump: XSDT has no entries");
        return;
    }
    // Entry 0 is FADT; FADT bytes begin after DSDT, so use its DSDT pointer
    // to print DSDT before the XSDT-listed tables like coreboot does.
    let Some(fadt_addr) = read_le_u64(xsdt, 36) else {
        fstart_log::warn!("ACPI dump: truncated FADT pointer");
        return;
    };
    let Some(fadt_off) = fadt_addr.checked_sub(base_addr).map(|v| v as usize) else {
        return;
    };
    if let Some(fadt) = data
        .get(fadt_off..)
        .and_then(|tail| acpi_table_len(tail).and_then(|len| tail.get(..len)))
    {
        if let Some(dsdt_addr) = read_le_u64(fadt, FADT_X_DSDT_OFF) {
            if let Some(dsdt_off) = dsdt_addr.checked_sub(base_addr).map(|v| v as usize) {
                if dsdt_off < data.len() {
                    print_acpi_table(&data[dsdt_off..]);
                }
            }
        }
    }
    for i in 0..entries {
        let off = 36 + i * 8;
        let Some(addr) = read_le_u64(xsdt, off) else {
            fstart_log::warn!("ACPI dump: truncated XSDT entry");
            return;
        };
        if let Some(table_off) = addr.checked_sub(base_addr).map(|v| v as usize) {
            if table_off < data.len() {
                print_acpi_table(&data[table_off..]);
            }
        }
    }
    fstart_log::info!("Done printing ACPI tables in ACPICA compatible format");
}

/// Prepare ACPI tables in a firmware-store entry.
///
/// Collects per-device DSDT AML and extra tables via `collect_devices`,
/// assembles the final table set via [`fstart_acpi::platform::assemble_into`]
/// into a page-aligned [`fstart_store::tag::ACPI`] entry trimmed to the
/// assembled size, installs a legacy RSDP copy in the EBDA, and hex-dumps
/// the tables. Returns the RSDP address only after bounded assembly
/// succeeds. Errors must abort required-table handoff.
#[cfg(feature = "acpi")]
pub fn prepare_acpi(
    store: &mut fstart_store::Store,
    platform: &fstart_acpi::platform::PlatformConfig,
    collect_devices: impl FnOnce(&mut Vec<u8>, &mut Vec<Vec<u8>>),
) -> Result<u64, fstart_acpi::AmlError> {
    let mut dsdt_aml: Vec<u8> = Vec::new();
    let mut extra_tables: Vec<Vec<u8>> = Vec::new();

    collect_devices(&mut dsdt_aml, &mut extra_tables);

    // Room for boards with large ACPI namespaces (dozens of devices, IORT
    // with many ID mappings); the entry is trimmed to the assembled size.
    const BUF_SIZE: usize = 128 * 1024;
    let entry = store
        .add(fstart_store::tag::ACPI, BUF_SIZE, 12)
        .map_err(|_| fstart_acpi::AmlError::LengthOverflow)?;
    let acpi_addr = store.address(&entry) as u64;
    // SAFETY: a fresh store entry nothing else references.
    let storage = unsafe { store.bytes_mut(&entry) };
    storage.fill(0);
    let acpi_len = fstart_acpi::platform::assemble_into(
        storage,
        acpi_addr,
        platform,
        &dsdt_aml,
        &extra_tables,
    )?;
    store
        .resize_last(entry, acpi_len)
        .map_err(|_| fstart_acpi::AmlError::LengthOverflow)?;

    fstart_log::info!(
        "ACPI: {} bytes written to {}",
        acpi_len as u32,
        fstart_log::Hex(acpi_addr),
    );

    // Dump the ACPI tables in coreboot's ACPICA/acpixtract-compatible format.
    // SAFETY: acpi_addr points to the buffer we just wrote, acpi_len bytes are
    // valid and the buffer persists (e820-reserved or leaked).
    let acpi_data = unsafe { core::slice::from_raw_parts(acpi_addr as *const u8, acpi_len) };
    install_rsdp_in_ebda(acpi_data);
    if fstart_log::log_enabled(fstart_log::Level::Trace) {
        print_acpi_tables_acpixtract(acpi_data);
    }

    Ok(acpi_addr)
}

/// Facts the SMBIOS writer cannot derive from the board descriptor.
#[cfg(feature = "smbios")]
pub struct SmbiosRuntime<'a> {
    /// Flash chip capacity in bytes.
    pub rom_size: u32,
    /// Raminit's DRAM inventory, when the chipset publishes one.
    pub memory: Option<&'a fstart_core::memory_info::MemoryInfo>,
    /// Enumerated PCI hierarchy; root-bus functions are onboard devices.
    pub pci: Option<&'a fstart_pci::PciEcam>,
}

/// Resolve SMBIOS Type 4 counts from runtime CPU and MP state.
#[cfg(feature = "smbios")]
fn runtime_processor_counts() -> (u16, u16, u16) {
    let (cores, threads) = fstart_arch::x86::boot::cpuid::cpu_core_thread_counts();
    let online = fstart_arch::x86::mp::online_cpus();
    let enabled = cores.min(online.max(1));
    (cores.max(1), enabled.max(1), threads.max(enabled))
}

/// SMBIOS processor family from the CPUID brand string; 0x02 is "Unknown".
#[cfg(any(feature = "smbios", feature = "host"))]
fn processor_family(brand: &str) -> u16 {
    const FAMILIES: [(&str, u16); 11] = [
        ("Core(TM)2 Duo", 0xBF),
        ("Core(TM)2 Solo", 0xC0),
        ("Core(TM)2 Extreme", 0xC1),
        ("Core(TM)2 Quad", 0xC2),
        ("Core(TM) Duo", 0x28),
        ("Core(TM) Solo", 0xBD),
        ("Atom", 0x2B),
        ("Xeon", 0xB3),
        ("Celeron", 0x0F),
        ("Pentium(R) M", 0xB9),
        ("Pentium(R) 4", 0xB2),
    ];
    FAMILIES
        .iter()
        .find(|(name, _)| brand.contains(name))
        .map_or(0x02, |(_, family)| *family)
}

/// Type 4 for the executing CPU package, with its Type 7 caches.
#[cfg(feature = "smbios")]
fn add_processor(w: &mut fstart_acpi::smbios::SmbiosWriter, socket: &str) {
    let cpu = fstart_arch::x86::boot::cpuid::ProcessorSnapshot::read();
    let (cores, enabled_cores, threads) = runtime_processor_counts();
    let clock = fstart_arch::x86::bus_clock();
    let max_speed_mhz = clock.map_or(0, |clock| clock.max_core_mhz() as u16);
    w.add_package(&fstart_acpi::smbios::Package {
        socket,
        cpu: &cpu,
        // The cache sizes of one core.
        apic_ids: &[],
        cores,
        enabled_cores,
        threads,
        family: processor_family(cpu.brand()),
        upgrade: 0x02, // unknown
        external_clock_mhz: clock.map_or(0, |clock| clock.fsb_mhz as u16),
        max_speed_mhz,
        current_speed_mhz: max_speed_mhz,
    });
}

/// Physical ranges occupied by `total` bytes of DRAM: below TOLUD, plus the
/// part the chipset remapped above 4 GiB (up to `high_end`).
#[cfg(any(feature = "smbios", feature = "host"))]
fn dram_ranges(total: u64, high_end: u64) -> impl Iterator<Item = (u64, u64)> {
    const FOUR_GIB: u64 = 1 << 32;
    let high = high_end.saturating_sub(FOUR_GIB).min(total);
    [(0, total - high), (FOUR_GIB, FOUR_GIB + high)]
        .into_iter()
        .filter(|(start, end)| end > start)
}

/// Types 16, 17 and 19 from raminit's slot inventory.
#[cfg(feature = "smbios")]
fn add_memory(
    w: &mut fstart_acpi::smbios::SmbiosWriter,
    memory: &fstart_core::memory_info::MemoryInfo,
    e820: &[fstart_core::services::memory_detect::E820Entry],
) {
    use alloc::format;
    let devices = memory.devices();
    w.add_physical_memory_array(
        u64::from(memory.max_capacity_mib.get()) * 1024,
        devices.len() as u16,
        memory.ecc,
    );
    for device in devices {
        let locator = format!("Channel-{}-DIMM-{}", device.channel, device.slot);
        let bank_locator = format!("BANK {}", device.channel);
        w.add_dimm(device, &locator, &bank_locator);
    }

    // A present/unknown DIMM must not turn the sentinel into petabytes of
    // fictitious DRAM. Types 16/17 remain useful; omit this inferred map.
    if devices
        .iter()
        .any(|device| device.size_mib.get() == fstart_core::memory_info::SIZE_UNKNOWN)
    {
        return;
    }
    let total = devices
        .iter()
        .map(|device| u64::from(device.size_mib.get()) << 20)
        .sum();
    let high_end = e820
        .iter()
        .filter(|entry| entry.kind == fstart_core::services::memory_detect::E820Kind::Ram as u32)
        .map(|entry| entry.addr.saturating_add(entry.size))
        .max()
        .unwrap_or(0);
    let modules = devices
        .iter()
        .filter(|device| device.is_populated())
        .count() as u8;
    for (start, end) in dram_ranges(total, high_end) {
        w.add_memory_array_mapped_address(start, end - 1, modules);
    }
}

/// SMBIOS onboard device type and label for a PCI class, as coreboot's
/// devicetree walk reports them.
#[cfg(any(feature = "smbios", feature = "host"))]
fn onboard_device(class: u16) -> Option<(u8, &'static str)> {
    Some(match class {
        0x0300 | 0x0302 => (0x03, "Onboard Video"),
        0x0100 => (0x04, "Onboard SCSI"),
        0x0200 => (0x05, "Onboard LAN"),
        0x0401 | 0x0403 => (0x07, "Onboard Audio"),
        0x0101 => (0x08, "Onboard IDE"),
        0x0106 => (0x09, "Onboard SATA"),
        0x0107 => (0x0a, "Onboard SAS"),
        _ => return None,
    })
}

/// Type 41 for each root-bus function: chipset devices are on the mainboard,
/// add-in cards sit behind bridges.
#[cfg(feature = "smbios")]
fn add_onboard_devices(w: &mut fstart_acpi::smbios::SmbiosWriter, pci: &fstart_pci::PciEcam) {
    let mut instances = [0u8; 16];
    for address in pci
        .devices()
        .filter(|address| address.bus() == pci.bus_start())
    {
        let Some(class) = pci
            .device(address)
            .map(|device| (device.read32(0x08) >> 16) as u16)
        else {
            continue;
        };
        let Some((device_type, designation)) = onboard_device(class) else {
            continue;
        };
        let instance = &mut instances[usize::from(device_type)];
        *instance += 1;
        w.add_onboard_device(
            designation,
            device_type,
            *instance,
            address.segment(),
            address.bus(),
            address.device() << 3 | address.function(),
        );
    }
}

/// Generate and write SMBIOS tables from a static descriptor plus runtime facts.
///
/// Writes into a [`fstart_store::tag::SMBIOS`] entry trimmed to the
/// assembled size, emits all SMBIOS structures, and logs the result.
///
/// Handles:
/// - Type 0 (BIOS), Type 1 (System), Type 3 (Chassis), Type 2 (Baseboard)
/// - Type 4 (Processor) with runtime Type 7 (Cache) detection
/// - Type 11 (OEM Strings) when the board provides one
/// - Type 16/17/19 from raminit's inventory, else Type 16 from e820 alone
/// - Type 41 (Onboard Devices) for root-bus PCI functions
/// - Type 32 (System Boot) and Type 127 (End of Table)
#[cfg(feature = "smbios")]
pub fn prepare_smbios(
    store: &mut fstart_store::Store,
    e820: &fstart_core::services::memory_detect::E820State,
    desc: &SmbiosIdentity,
    runtime: &SmbiosRuntime,
) -> Result<u64, fstart_core::services::ServiceError> {
    // 64 KiB table area after the entry point.
    const BUF_SIZE: usize = fstart_acpi::smbios::BUFFER_SIZE;
    // Page-aligned so the ACPI NVS range before it ends on a page boundary.
    let entry = store
        .add(fstart_store::tag::SMBIOS, BUF_SIZE, 12)
        .map_err(|_| fstart_core::services::ServiceError::InvalidParam)?;
    let smbios_addr = store.address(&entry) as u64;
    // SAFETY: a fresh store entry nothing else references.
    let bytes = unsafe { store.bytes_mut(&entry) };
    bytes.fill(0);

    let total_ram = e820.total_ram();
    let entries = e820.entries();
    let smbios_len = fstart_acpi::smbios::assemble_into(bytes, smbios_addr, |w| {
        w.add_identity(desc, runtime.rom_size, cfg!(feature = "payload-uefi-basic"));
        for socket in desc.processor_sockets {
            add_processor(&mut *w, socket);
        }
        match runtime.memory {
            Some(memory) => add_memory(&mut *w, memory, entries),
            // Without an inventory only the capacity is known; e820 holes make
            // `0..total_ram` no address map, so Types 17/19 are omitted.
            None if total_ram != 0 => {
                w.add_physical_memory_array(total_ram / 1024, 0, 0x02);
            }
            None => {}
        }
        if let Some(pci) = runtime.pci {
            add_onboard_devices(&mut *w, pci);
        }
        w.add_system_boot_info();
        w.add_end_of_table();
    })
    .map_err(|_| fstart_core::services::ServiceError::InvalidParam)?;

    store
        .resize_last(entry, smbios_len)
        .map_err(|_| fstart_core::services::ServiceError::InvalidParam)?;

    fstart_log::info!(
        "SMBIOS: {} bytes written to {}",
        smbios_len as u32,
        fstart_log::Hex(smbios_addr),
    );
    Ok(smbios_addr)
}

#[cfg(all(test, feature = "host"))]
mod tests {
    use super::*;

    #[test]
    fn processor_family_follows_the_brand_string() {
        for (brand, family) in [
            ("Intel(R) Core(TM)2 Duo CPU     T7300  @ 2.00GHz", 0xBF),
            ("Intel(R) Atom(TM) CPU 230   @ 1.60GHz", 0x2B),
            ("Genuine Intel(R) CPU           T2400  @ 1.83GHz", 0x02),
        ] {
            assert_eq!(processor_family(brand), family, "{brand}");
        }
    }

    #[test]
    fn dram_ranges_split_at_the_remapped_window() {
        const GIB: u64 = 1 << 30;
        let ranges = |total, high_end| dram_ranges(total, high_end).collect::<alloc::vec::Vec<_>>();
        assert_eq!(ranges(2 * GIB, 2 * GIB), [(0, 2 * GIB)]);
        // 4 GiB with TOLUD at 3 GiB, 1 GiB remapped above 4 GiB.
        assert_eq!(ranges(4 * GIB, 5 * GIB), [(0, 3 * GIB), (4 * GIB, 5 * GIB)]);
    }

    #[test]
    fn onboard_devices_cover_coreboot_classes() {
        assert_eq!(onboard_device(0x0200), Some((0x05, "Onboard LAN")));
        assert_eq!(onboard_device(0x0403), Some((0x07, "Onboard Audio")));
        assert_eq!(onboard_device(0x0380), None);
        assert_eq!(onboard_device(0x0c03), None);
    }
}
