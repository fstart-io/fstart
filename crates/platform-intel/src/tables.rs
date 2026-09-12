//! Shared Intel/x86 ACPI and SMBIOS table handoff helpers.
//!
//! Direct helpers over `fstart-acpi`/`fstart-acpi::smbios`: heap/e820 buffer
//! allocation, table assembly, RSDP EBDA install, and the acpixtract
//! hex dump. Chipset flows call these in `emit_tables`; drivers and the
//! mainboard contribute fragments through the shared `AcpiDevice`
//! abstraction. There is no capability layer in between.

extern crate alloc;

#[cfg(feature = "acpi")]
use alloc::alloc::{Layout, alloc_zeroed};
#[cfg(feature = "smbios")]
use alloc::vec;
#[cfg(feature = "acpi")]
use alloc::vec::Vec;

use fstart_core::services::memory_detect::E820Kind;

#[cfg(feature = "smbios")]
pub use fstart_acpi::smbios::SmbiosDesc;

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

/// Carve a page-aligned handoff region out of the top of e820 RAM.
fn allocate_x86_handoff_region(
    e820: &mut fstart_core::services::memory_detect::E820State,
    size: usize,
    align: u64,
    kind: E820Kind,
) -> Option<u64> {
    let size = ((size as u64) + 0xfff) & !0xfff;
    let align_mask = align.saturating_sub(1);
    let mut selected = 0u64;

    for entry in e820.entries() {
        if entry.kind != E820Kind::Ram as u32 || entry.size < size {
            continue;
        }
        let top = entry.addr.saturating_add(entry.size);
        let base = top.saturating_sub(size) & !align_mask;
        if base >= entry.addr && base > selected {
            selected = base;
        }
    }

    if selected == 0 {
        return None;
    }

    e820.reserve_range_as(selected, size, kind);
    Some(selected)
}

#[cfg(feature = "acpi")]
fn acpi_table_len(table: &[u8]) -> Option<usize> {
    if table.len() < 36 {
        return None;
    }
    let len = u32::from_le_bytes(table[4..8].try_into().ok()?) as usize;
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
    let mut w = fstart_log::writer();
    let _ = ufmt::uwriteln!(w, "{} @ 0x0000000000000000", sig_str);
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
    let xsdt_addr = u64::from_le_bytes(data[24..32].try_into().unwrap_or([0; 8]));
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
    let fadt_addr = u64::from_le_bytes(xsdt[36..44].try_into().unwrap_or([0; 8]));
    let Some(fadt_off) = fadt_addr.checked_sub(base_addr).map(|v| v as usize) else {
        return;
    };
    if fadt_off + FADT_X_DSDT_OFF + 8 <= data.len() {
        let fadt = &data[fadt_off..];
        let dsdt_addr = u64::from_le_bytes(
            fadt[FADT_X_DSDT_OFF..FADT_X_DSDT_OFF + 8]
                .try_into()
                .unwrap_or([0; 8]),
        );
        if let Some(dsdt_off) = dsdt_addr.checked_sub(base_addr).map(|v| v as usize) {
            if dsdt_off < data.len() {
                print_acpi_table(&data[dsdt_off..]);
            }
        }
    }
    for i in 0..entries {
        let off = 36 + i * 8;
        let addr = u64::from_le_bytes(xsdt[off..off + 8].try_into().unwrap_or([0; 8]));
        if let Some(table_off) = addr.checked_sub(base_addr).map(|v| v as usize) {
            if table_off < data.len() {
                print_acpi_table(&data[table_off..]);
            }
        }
    }
    fstart_log::info!("Done printing ACPI tables in ACPICA compatible format");
}

/// Prepare ACPI tables and write them to a top-of-RAM handoff buffer.
///
/// Carves a dedicated allocation out of e820 RAM (marked ACPI reclaim) —
/// falling back to a leaked heap buffer — collects per-device DSDT AML and
/// extra tables via `collect_devices`, assembles the final table set via
/// [`fstart_acpi::platform::assemble_into`], installs a legacy RSDP
/// copy in the EBDA, and hex-dumps the tables. Returns the RSDP address only
/// after bounded assembly succeeds. Errors must abort required-table handoff.
#[cfg(feature = "acpi")]
pub fn prepare_acpi(
    e820: &mut fstart_core::services::memory_detect::E820State,
    platform: &fstart_acpi::platform::PlatformConfig,
    collect_devices: impl FnOnce(&mut Vec<u8>, &mut Vec<Vec<u8>>),
) -> Result<u64, fstart_acpi::AmlError> {
    let mut dsdt_aml: Vec<u8> = Vec::new();
    let mut extra_tables: Vec<Vec<u8>> = Vec::new();

    collect_devices(&mut dsdt_aml, &mut extra_tables);

    // Prefer a dedicated top-of-RAM handoff allocation carved into the e820
    // map as ACPI reclaim memory. This avoids placing ACPI tables inside
    // fstart/CrabEFI's linker runtime-data range, which would fragment EFI
    // RuntimeServicesData descriptors.
    //
    // 128 KiB provides headroom for boards with large ACPI namespaces
    // (dozens of devices, IORT with many ID mappings). Increase if a
    // board exceeds this limit.
    const BUF_SIZE: usize = 128 * 1024;
    let acpi_addr = allocate_x86_handoff_region(e820, BUF_SIZE, 0x1000, E820Kind::Acpi)
        .inspect(|addr| unsafe {
            core::ptr::write_bytes(*addr as *mut u8, 0, BUF_SIZE);
        })
        .unwrap_or_else(|| {
            let layout = Layout::from_size_align(BUF_SIZE, 16)
                .unwrap_or_else(|_| panic!("invalid ACPI buffer layout"));
            // SAFETY: `layout` has non-zero size and a valid 16-byte
            // alignment. The allocation is intentionally leaked so ACPI
            // tables remain available to the OS after handoff.
            let acpi_ptr = unsafe { alloc_zeroed(layout) };
            if acpi_ptr.is_null() {
                panic!("failed to allocate ACPI table buffer");
            }
            acpi_ptr as u64
        });

    let address = usize::try_from(acpi_addr).map_err(|_| fstart_acpi::AmlError::LengthOverflow)?;
    address
        .checked_add(BUF_SIZE)
        .ok_or(fstart_acpi::AmlError::LengthOverflow)?;
    // SAFETY: this complete BUF_SIZE range was just reserved or allocated.
    // The bounded assembler validates capacity before copying any table bytes.
    let storage = unsafe { core::slice::from_raw_parts_mut(address as *mut u8, BUF_SIZE) };
    let acpi_len = fstart_acpi::platform::assemble_into(
        storage,
        acpi_addr,
        platform,
        &dsdt_aml,
        &extra_tables,
    )?;

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
    print_acpi_tables_acpixtract(acpi_data);

    Ok(acpi_addr)
}

#[cfg(feature = "smbios")]
#[derive(Debug, Clone, Copy)]
struct RuntimeCacheDesc {
    designation: &'static str,
    level: u8,
    size_kb: u32,
    associativity: u8,
    cache_type: u8,
}

/// Resolve SMBIOS Type 4 counts, coreboot-style.
///
/// Hardware truth comes from CPUID on the BSP (`0xB`, else leaf 4, else
/// leaf 1); `core_enabled` is capped by actually online APs from MP init,
/// which runs before `emit_tables`. A board descriptor value of 0 means
/// "detect at runtime" (same sentinel as empty `caches`).
#[cfg(feature = "smbios")]
fn resolve_processor_counts(proc: &fstart_acpi::smbios::ProcessorDesc) -> (u16, u16, u16) {
    let (mut cores, mut threads) = (proc.core_count, proc.thread_count);
    if cores == 0 || threads == 0 {
        let (rt_cores, rt_threads) = fstart_arch::x86_64::cpuid::cpu_core_thread_counts();
        if cores == 0 {
            cores = rt_cores;
        }
        if threads == 0 {
            threads = rt_threads;
        }
    }
    let online = fstart_arch::mp::online_cpus();
    let enabled = cores.min(online.max(1));
    (cores.max(1), enabled.max(1), threads.max(enabled))
}

#[cfg(feature = "smbios")]
fn add_runtime_cache_info(w: &mut fstart_acpi::smbios::SmbiosWriter) -> (u16, u16, u16) {
    let caches = runtime_x86_caches::<8>();
    let mut l1 = 0xFFFFu16;
    let mut l2 = 0xFFFFu16;
    let mut l3 = 0xFFFFu16;
    for cache in caches.iter().flatten() {
        let handle = w.add_cache_info(
            cache.designation,
            cache.level,
            cache.size_kb,
            cache.associativity,
            cache.cache_type,
        );
        match cache.level {
            1 => l1 = handle,
            2 => l2 = handle,
            3 => l3 = handle,
            _ => {}
        }
    }
    (l1, l2, l3)
}

#[cfg(feature = "smbios")]
fn runtime_x86_caches<const N: usize>() -> [Option<RuntimeCacheDesc>; N] {
    let mut out = [None; N];
    let (max_leaf, _, _, _) = fstart_arch::x86::cpuid(0);
    if max_leaf < 4 {
        return out;
    }

    let mut count = 0usize;
    for idx in 0..N as u32 {
        let (eax, ebx, ecx, _) = fstart_arch::x86::cpuid_count(4, idx);
        let cache_type = (eax & 0x1f) as u8;
        if cache_type == 0 {
            break;
        }

        let level = ((eax >> 5) & 0x7) as u8;
        let ways = ((ebx >> 22) & 0x3ff) + 1;
        let partitions = ((ebx >> 12) & 0x3ff) + 1;
        let line_size = (ebx & 0xfff) + 1;
        let sets = ecx + 1;
        let size_kb = ways
            .saturating_mul(partitions)
            .saturating_mul(line_size)
            .saturating_mul(sets)
            / 1024;

        out[count] = Some(RuntimeCacheDesc {
            designation: cache_designation(level, cache_type),
            level,
            size_kb,
            associativity: smbios_associativity(ways),
            cache_type: smbios_cache_type(cache_type),
        });
        count += 1;
        if count == N {
            break;
        }
    }
    out
}

#[cfg(feature = "smbios")]
fn cache_designation(level: u8, cache_type: u8) -> &'static str {
    match (level, cache_type) {
        (1, 1) => "L1 Data Cache",
        (1, 2) => "L1 Instruction Cache",
        (1, _) => "L1 Cache",
        (2, _) => "L2 Cache",
        (3, _) => "L3 Cache",
        _ => "CPU Cache",
    }
}

#[cfg(feature = "smbios")]
fn smbios_cache_type(cache_type: u8) -> u8 {
    match cache_type {
        1 => 0x05, // Data
        2 => 0x04, // Instruction
        3 => 0x03, // Unified
        _ => 0x02, // Unknown
    }
}

#[cfg(feature = "smbios")]
fn smbios_associativity(ways: u32) -> u8 {
    match ways {
        1 => 0x03,
        2 => 0x04,
        4 => 0x05,
        8 => 0x06,
        16 => 0x07,
        12 => 0x08,
        24 => 0x09,
        32 => 0x0a,
        48 => 0x0b,
        64 => 0x0c,
        20 => 0x0d,
        _ => 0x02,
    }
}

/// Generate and write SMBIOS tables from a static descriptor plus runtime facts.
///
/// Carves a top-of-RAM handoff buffer out of e820 (falling back to a leaked
/// heap buffer), iterates the descriptor to emit all SMBIOS structures, and
/// logs the result.
///
/// Handles:
/// - Type 0 (BIOS), Type 1 (System), Type 2 (Baseboard), Type 3 (Chassis)
/// - Type 4 (Processor) with runtime Type 7 (Cache) detection when descriptors are empty
/// - Type 16 (Physical Memory Array), Type 17 (Memory Device), Type 19 (Mapped Address)
/// - Type 32 (System Boot) and Type 127 (End of Table)
#[cfg(feature = "smbios")]
pub fn prepare_smbios(
    e820: &mut fstart_core::services::memory_detect::E820State,
    desc: &SmbiosDesc,
) {
    // 64 KiB table area + 32 bytes entry point header.
    // `assemble_and_write` writes ENTRY_POINT_SIZE bytes at `table_addr`
    // then up to MAX_TABLE_AREA bytes starting at `table_addr + 24`.
    const BUF_SIZE: usize = 64 * 1024 + 32;
    let smbios_addr = allocate_x86_handoff_region(e820, BUF_SIZE, 0x1000, E820Kind::Reserved)
        .inspect(|addr| unsafe {
            core::ptr::write_bytes(*addr as *mut u8, 0, BUF_SIZE);
        })
        .unwrap_or_else(|| {
            let smbios_buf = vec![0u8; BUF_SIZE];
            let smbios_addr = smbios_buf.as_ptr() as u64;
            // Keep the buffer alive -- tables must persist for the OS.
            core::mem::forget(smbios_buf);
            smbios_addr
        });

    let smbios_len = fstart_acpi::smbios::assemble_and_write(smbios_addr, |w| {
        // Type 0: BIOS Information
        w.add_bios_info(desc.bios_vendor, desc.bios_version, desc.bios_release_date);

        // Type 1: System Information
        w.add_system_info(
            desc.sys_manufacturer,
            desc.sys_product,
            desc.sys_version,
            desc.sys_serial,
        );

        // Type 2: Baseboard (optional)
        if !desc.bb_manufacturer.is_empty() || !desc.bb_product.is_empty() {
            w.add_baseboard_info(desc.bb_manufacturer, desc.bb_product);
        }

        // Type 3: Enclosure
        w.add_enclosure(desc.chassis_type, desc.chassis_manufacturer);

        // Type 4 + Type 7: Processors and caches
        for proc in desc.processors {
            let (cores, enabled, threads) = resolve_processor_counts(proc);
            if proc.caches.is_empty() {
                let (l1, l2, l3) = add_runtime_cache_info(&mut *w);
                if l1 == 0xFFFF && l2 == 0xFFFF && l3 == 0xFFFF {
                    w.add_processor(
                        proc.socket,
                        proc.manufacturer,
                        proc.family,
                        proc.max_speed_mhz,
                        cores,
                        enabled,
                        threads,
                    );
                } else {
                    w.add_processor_with_caches(
                        proc.socket,
                        proc.manufacturer,
                        proc.family,
                        proc.max_speed_mhz,
                        cores,
                        enabled,
                        threads,
                        l1,
                        l2,
                        l3,
                    );
                }
            } else {
                // Emit Type 7 cache entries first, collecting handles for
                // the L1/L2/L3 slots that Type 4 references.
                let mut l1 = 0xFFFFu16;
                let mut l2 = 0xFFFFu16;
                let mut l3 = 0xFFFFu16;
                for cache in proc.caches {
                    let handle = w.add_cache_info(
                        cache.designation,
                        cache.level,
                        cache.size_kb,
                        cache.associativity,
                        cache.cache_type,
                    );
                    match cache.level {
                        1 => l1 = handle,
                        2 => l2 = handle,
                        3 => l3 = handle,
                        _ => {}
                    }
                }
                w.add_processor_with_caches(
                    proc.socket,
                    proc.manufacturer,
                    proc.family,
                    proc.max_speed_mhz,
                    cores,
                    enabled,
                    threads,
                    l1,
                    l2,
                    l3,
                );
            }
        }

        // Type 16/17/19: Memory
        if !desc.memory_devices.is_empty() {
            let total_capacity_kb: u64 = desc
                .memory_devices
                .iter()
                .map(|d| d.size_mb as u64 * 1024)
                .sum();
            w.add_physical_memory_array(total_capacity_kb, desc.memory_devices.len() as u16);

            for dev in desc.memory_devices {
                w.add_memory_device(dev.locator, dev.size_mb, dev.speed_mhz, dev.memory_type);
            }

            // Type 19: Memory Array Mapped Address
            if desc.ram_end > desc.ram_base {
                w.add_memory_array_mapped_address(desc.ram_base, desc.ram_end, 1);
            }
        }

        // Type 32 + Type 127
        w.add_system_boot_info();
        w.add_end_of_table();
    });

    fstart_log::info!(
        "SMBIOS: {} bytes written to {}",
        smbios_len as u32,
        fstart_log::Hex(smbios_addr),
    );
}
