//! SMBIOS table generation from static descriptors.
//!
//! Moves the SMBIOS table iteration logic out of codegen into a testable
//! library function. Codegen emits a const [`SmbiosDesc`] descriptor
//! (string literals + fixed arrays) and calls [`prepare`], which handles
//! all the writer sequencing, cache handle mapping, and memory array
//! linking that was previously inlined in generated code.
//!
//! The descriptor types use `&str` and `&[T]` so they are
//! const-constructible — codegen can emit them as `const` or inline
//! struct literals with zero runtime overhead.

extern crate alloc;

use alloc::vec;

/// Static descriptor for SMBIOS table generation.
///
/// All strings are `&str` for const-constructibility. Codegen constructs
/// this from the board RON's `smbios` config section.
pub struct SmbiosDesc<'a> {
    /// Type 0: BIOS vendor string.
    pub bios_vendor: &'a str,
    /// Type 0: BIOS version string.
    pub bios_version: &'a str,
    /// Type 0: BIOS release date (MM/DD/YYYY).
    pub bios_release_date: &'a str,

    /// Type 1: System manufacturer.
    pub sys_manufacturer: &'a str,
    /// Type 1: System product name.
    pub sys_product: &'a str,
    /// Type 1: System version.
    pub sys_version: &'a str,
    /// Type 1: System serial number (None = omit).
    pub sys_serial: Option<&'a str>,

    /// Type 2: Baseboard manufacturer (empty = skip Type 2).
    pub bb_manufacturer: &'a str,
    /// Type 2: Baseboard product name.
    pub bb_product: &'a str,

    /// Type 3: Chassis type byte (SMBIOS encoding).
    pub chassis_type: u8,
    /// Type 3: Chassis manufacturer.
    pub chassis_manufacturer: &'a str,

    /// Type 4/7: Processor entries with optional cache descriptors.
    pub processors: &'a [ProcessorDesc<'a>],

    /// Type 16/17: Memory device entries.
    pub memory_devices: &'a [MemoryDeviceDesc<'a>],

    /// Type 19: RAM region start address (0 = skip Type 19).
    pub ram_base: u64,
    /// Type 19: RAM region end address (inclusive).
    pub ram_end: u64,
}

/// Processor descriptor for SMBIOS Type 4 + Type 7 generation.
pub struct ProcessorDesc<'a> {
    /// Socket designation string.
    pub socket: &'a str,
    /// Processor manufacturer.
    pub manufacturer: &'a str,
    /// Processor family (SMBIOS u16 encoding).
    pub family: u16,
    /// Maximum speed in MHz.
    pub max_speed_mhz: u16,
    /// Number of cores.
    pub core_count: u16,
    /// Number of threads.
    pub thread_count: u16,
    /// Cache descriptors (empty = use simple `add_processor`).
    pub caches: &'a [CacheDesc<'a>],
}

/// Cache descriptor for SMBIOS Type 7 generation.
pub struct CacheDesc<'a> {
    /// Cache designation string (e.g., "L1 Data Cache").
    pub designation: &'a str,
    /// Cache level (1, 2, or 3).
    pub level: u8,
    /// Cache size in KiB.
    pub size_kb: u32,
    /// Associativity (SMBIOS byte encoding).
    pub associativity: u8,
    /// Cache type: unified, instruction, or data (SMBIOS byte encoding).
    pub cache_type: u8,
}

/// Memory device descriptor for SMBIOS Type 17 generation.
pub struct MemoryDeviceDesc<'a> {
    /// Device locator string (e.g., "DIMM0", "Onboard").
    pub locator: &'a str,
    /// Size in MiB.
    pub size_mb: u32,
    /// Speed in MHz.
    pub speed_mhz: u16,
    /// Memory type (SMBIOS byte encoding).
    pub memory_type: u8,
}

/// Generate and write SMBIOS tables from a static descriptor.
///
/// Allocates a 64 KiB heap buffer (leaked with `core::mem::forget` so
/// tables persist for the OS), iterates the descriptor to emit all
/// SMBIOS structures, and logs the result.
///
/// Handles:
/// - Type 0 (BIOS), Type 1 (System), Type 2 (Baseboard), Type 3 (Chassis)
/// - Type 4 (Processor) with automatic Type 7 (Cache) handle linking
/// - Type 16 (Physical Memory Array), Type 17 (Memory Device), Type 19 (Mapped Address)
/// - Type 32 (System Boot) and Type 127 (End of Table)
pub fn prepare(desc: &SmbiosDesc) {
    fstart_log::info!("capability: SmBiosPrepare");

    // 64 KiB table area + 32 bytes entry point header.
    // `assemble_and_write` writes ENTRY_POINT_SIZE bytes at `table_addr`
    // then up to MAX_TABLE_AREA bytes starting at `table_addr + 24`.
    const BUF_SIZE: usize = 64 * 1024 + 32;
    let smbios_buf = vec![0u8; BUF_SIZE];
    let smbios_addr = smbios_buf.as_ptr() as u64;
    // Keep the buffer alive -- tables must persist for the OS.
    core::mem::forget(smbios_buf);

    let smbios_len = fstart_smbios::assemble_and_write(smbios_addr, |w| {
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
            if proc.caches.is_empty() {
                w.add_processor(
                    proc.socket,
                    proc.manufacturer,
                    proc.family,
                    proc.max_speed_mhz,
                    proc.core_count,
                    proc.thread_count,
                );
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
                    proc.core_count,
                    proc.thread_count,
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
        "SmBiosPrepare: {} bytes written to {}",
        smbios_len as u32,
        fstart_log::Hex(smbios_addr),
    );
}
