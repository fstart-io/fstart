//! ACPI table generation orchestration.
//!
//! Moves the heap allocation, assembly, `mem::forget`, and hex dump
//! orchestration out of codegen into a testable library function.
//! Codegen emits per-device AML collection calls and a platform config
//! struct literal, then calls [`prepare`] with a closure that fills in
//! the device contributions.
//!
//! The `extern crate alloc` and `Vec` creation live here so generated
//! code no longer needs `extern crate alloc` for ACPI.

extern crate alloc;

use alloc::alloc::{alloc_zeroed, Layout};
use alloc::vec::Vec;

/// Prepare ACPI tables and write them to a heap-allocated DRAM buffer.
///
/// Allocates a 64 KiB buffer (leaked via `core::mem::forget` so the
/// tables persist for the OS), collects per-device DSDT AML and extra
/// tables via the `collect_devices` closure, then calls
/// [`fstart_acpi::platform::assemble_and_write`] to produce the final
/// table set.
///
/// # Arguments
///
/// - `platform` — Platform-level ACPI config (ARM GICv3, timers, etc.).
/// - `collect_devices` — Closure that appends per-device DSDT AML bytes
///   to the first `Vec<u8>` and per-device standalone tables (SPCR, MCFG)
///   to the second `Vec<Vec<u8>>`. Called exactly once.
const RSDP_LEN: usize = 36;
const XSDT_OFF: usize = 48;
const FADT_X_DSDT_OFF: usize = 140;
#[cfg(target_arch = "x86_64")]
const BDA_EBDA_SEG_PTR: *mut u16 = 0x040e as *mut u16;
#[cfg(target_arch = "x86_64")]
const BDA_CONVENTIONAL_MEM_KB: *mut u16 = 0x0413 as *mut u16;
#[cfg(target_arch = "x86_64")]
const EBDA_BASE: usize = 0x0009_f000;
#[cfg(target_arch = "x86_64")]
const EBDA_SIZE: usize = 0x1000;
#[cfg(target_arch = "x86_64")]
const EBDA_RSDP_OFFSET: usize = 0;

fn acpi_table_len(table: &[u8]) -> Option<usize> {
    if table.len() < 36 {
        return None;
    }
    let len = u32::from_le_bytes(table[4..8].try_into().ok()?) as usize;
    (len >= 36 && len <= table.len()).then_some(len)
}

fn print_hex_byte(byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    fstart_log::raw_write_byte(HEX[(byte >> 4) as usize]);
    fstart_log::raw_write_byte(HEX[(byte & 0x0f) as usize]);
}

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
    }
}

fn print_acpi_table(data: &[u8]) {
    let Some(len) = acpi_table_len(data) else {
        return;
    };
    let sig = &data[..4];
    let sig_str = unsafe { core::str::from_utf8_unchecked(sig) };
    let mut w = fstart_log::writer();
    let _ = ufmt::uwriteln!(w, "{} @ 0x0000000000000000", sig_str);
    print_acpixtract_hexdump(&data[..len]);
    fstart_log::raw_write_byte(b'\r');
    fstart_log::raw_write_byte(b'\n');
}

#[cfg(target_arch = "x86_64")]
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

#[cfg(not(target_arch = "x86_64"))]
fn install_rsdp_in_ebda(_rsdp: &[u8]) {}

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

pub fn prepare(
    platform: &fstart_acpi::platform::PlatformConfig,
    collect_devices: impl FnOnce(&mut Vec<u8>, &mut Vec<Vec<u8>>),
) -> u64 {
    prepare_with_options(platform, true, collect_devices)
}

pub fn prepare_with_options(
    platform: &fstart_acpi::platform::PlatformConfig,
    print_hex: bool,
    collect_devices: impl FnOnce(&mut Vec<u8>, &mut Vec<Vec<u8>>),
) -> u64 {
    fstart_log::info!("capability: AcpiPrepare");

    let mut dsdt_aml: Vec<u8> = Vec::new();
    let mut extra_tables: Vec<Vec<u8>> = Vec::new();

    collect_devices(&mut dsdt_aml, &mut extra_tables);

    // Allocate a heap buffer for the ACPI tables. The bump allocator
    // gives a stable DRAM address that persists until reset.
    //
    // 128 KiB provides headroom for boards with large ACPI namespaces
    // (dozens of devices, IORT with many ID mappings). Increase if a
    // board exceeds this limit.
    const BUF_SIZE: usize = 128 * 1024;
    let layout = Layout::from_size_align(BUF_SIZE, 16)
        .unwrap_or_else(|_| panic!("invalid ACPI buffer layout"));
    // SAFETY: `layout` has non-zero size and a valid 16-byte alignment.
    // The allocation is intentionally leaked below so ACPI tables remain
    // available to the OS after firmware hands off control.
    let acpi_ptr = unsafe { alloc_zeroed(layout) };
    if acpi_ptr.is_null() {
        panic!("failed to allocate ACPI table buffer");
    }
    let acpi_addr = acpi_ptr as u64;

    let acpi_len =
        fstart_acpi::platform::assemble_and_write(acpi_addr, platform, &dsdt_aml, &extra_tables);

    assert!(
        acpi_len <= BUF_SIZE,
        "ACPI tables ({} bytes) exceed buffer size ({} bytes)",
        acpi_len,
        BUF_SIZE,
    );

    // Intentionally leak the allocation — tables must persist for the OS.

    fstart_log::info!(
        "AcpiPrepare: {} bytes written to {}",
        acpi_len as u32,
        fstart_log::Hex(acpi_addr),
    );

    // Dump the ACPI tables in coreboot's ACPICA/acpixtract-compatible format.
    // SAFETY: acpi_addr points to the buffer we just wrote, acpi_len bytes are
    // valid and the buffer is leaked (alive forever).
    let acpi_data = unsafe { core::slice::from_raw_parts(acpi_addr as *const u8, acpi_len) };
    install_rsdp_in_ebda(acpi_data);
    if print_hex {
        print_acpi_tables_acpixtract(acpi_data);
    }

    acpi_addr
}
