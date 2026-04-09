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

use alloc::vec;
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
pub fn prepare(
    platform: &fstart_acpi::platform::PlatformConfig,
    collect_devices: impl FnOnce(&mut Vec<u8>, &mut Vec<Vec<u8>>),
) {
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
    let acpi_buf = vec![0u8; BUF_SIZE];
    let acpi_addr = acpi_buf.as_ptr() as u64;

    let acpi_len =
        fstart_acpi::platform::assemble_and_write(acpi_addr, platform, &dsdt_aml, &extra_tables);

    assert!(
        acpi_len <= BUF_SIZE,
        "ACPI tables ({} bytes) exceed buffer size ({} bytes)",
        acpi_len,
        BUF_SIZE,
    );

    // Keep the buffer alive — tables must persist for the OS.
    core::mem::forget(acpi_buf);

    fstart_log::info!(
        "AcpiPrepare: {} bytes written to {}",
        acpi_len as u32,
        fstart_log::Hex(acpi_addr),
    );

    // Dump the ACPI tables as hex for offline disassembly with iasl.
    // SAFETY: acpi_addr points to the buffer we just wrote, acpi_len
    // bytes are valid and the buffer is leaked (alive forever).
    let acpi_data = unsafe { core::slice::from_raw_parts(acpi_addr as *const u8, acpi_len) };
    fstart_log::hex_dump(acpi_data);
}
