//! SMBIOS 3.0 table generation for fstart firmware.
//!
//! Writes SMBIOS structures directly to a target physical address.
//! No heap allocation — uses a cursor-based writer over a raw memory slice.
//!
//! # Supported table types
//!
//! | Type | Name                        |
//! |------|-----------------------------|
//! | 0    | BIOS Information            |
//! | 1    | System Information          |
//! | 2    | Baseboard Information       |
//! | 3    | System Enclosure            |
//! | 4    | Processor Information       |
//! | 16   | Physical Memory Array       |
//! | 17   | Memory Device               |
//! | 19   | Memory Array Mapped Address |
//! | 32   | System Boot Information     |
//! | 127  | End-of-Table                |
//!
//! # Usage
//!
//! ```ignore
//! let total = fstart_acpi::smbios::assemble_and_write(0x10000090000, |w| {
//!     w.add_bios_info("fstart", "0.1.0", "03/10/2026");
//!     w.add_system_info("QEMU", "SBSA Reference", "1.0", None);
//!     w.add_end_of_table();
//! });
//! ```

use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{Immutable, IntoBytes};

/// SMBIOS 3.0 64-bit entry point signature.
const SM3_MAGIC: [u8; 5] = *b"_SM3_";

/// Size of the SMBIOS 3.0 entry point structure (24 bytes).
const ENTRY_POINT_SIZE: usize = 24;

/// Maximum SMBIOS table area (64 KiB is generous for firmware).
const MAX_TABLE_AREA: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// SMBIOS structure type constants
// ---------------------------------------------------------------------------

const TYPE_BIOS_INFO: u8 = 0;
const TYPE_SYSTEM_INFO: u8 = 1;
const TYPE_BASEBOARD_INFO: u8 = 2;
const TYPE_ENCLOSURE: u8 = 3;
const TYPE_PROCESSOR: u8 = 4;
const TYPE_CACHE_INFO: u8 = 7;
const TYPE_PHYS_MEM_ARRAY: u8 = 16;
const TYPE_MEMORY_DEVICE: u8 = 17;
const TYPE_MEM_ARRAY_MAPPED_ADDR: u8 = 19;
const TYPE_SYSTEM_BOOT: u8 = 32;
const TYPE_END_OF_TABLE: u8 = 127;

// ---------------------------------------------------------------------------
// Static descriptors shared by board metadata and stage code
// ---------------------------------------------------------------------------

/// Static descriptor for SMBIOS table generation.
///
/// All strings are `&str` and slices so board crates can define a single
/// const descriptor that is usable by both host metadata and no_std stage code.
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
    /// Number of cores (0 = detect at runtime via CPUID).
    pub core_count: u16,
    /// Number of threads (0 = detect at runtime via CPUID).
    pub thread_count: u16,
    /// Cache descriptors. Empty means the runtime should detect caches when supported.
    pub caches: &'a [CacheDesc<'a>],
}

/// Cache descriptor for SMBIOS Type 7 generation.
#[derive(Clone, Copy)]
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Cap a `u16` to `u8`, returning `0xFF` if the value exceeds 255.
///
/// Used for SMBIOS fields that have a 1-byte "legacy" field with a separate
/// 2-byte extended field for values > 255 (e.g., core count, thread count).
fn cap_u8(val: u16) -> u8 {
    if val > 255 { 0xFF } else { val as u8 }
}

// ---------------------------------------------------------------------------
// Wire-format structures
// ---------------------------------------------------------------------------
//
// Each structure's fixed formatted area is a `repr(C)` struct over zerocopy
// little-endian integers, emitted with a single bounds-checked volatile
// copy. All multi-byte fields use explicit `U16`/`U32`/`U64` (alignment 1),
// so `repr(C)` has no padding by construction; the `size_of` assertions
// keep the Length byte honest. Every historical length mismatch in this
// file becomes a compile error instead of a corrupt table:
//
// - Type 2 omitted its trailing contained-count byte while declaring 0x0F,
//   so parsers ate the first manufacturer string byte as a count.
// - Type 17 wrote SMBIOS 3.3 extended-speed dwords while declaring the 3.2
//   length 0x54, so parsers treated them as string data.
//
// Variable-length string sections are still appended by `SmbiosWriter`
// after the fixed area (`write_string` / `end_strings`).

/// 4-byte header shared by every SMBIOS structure.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawHeader {
    ty: u8,
    len: u8,
    handle: U16,
}

/// Type 0: BIOS Information (SMBIOS 2.4+, 26 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawType0 {
    header: RawHeader,
    vendor: u8,
    version: u8,
    start_segment: U16,
    release_date: u8,
    rom_size: u8,
    characteristics: U64,
    characteristics_ext1: u8,
    characteristics_ext2: u8,
    bios_major: u8,
    bios_minor: u8,
    ec_major: u8,
    ec_minor: u8,
    ext_rom_size: U16,
}
const _: () = assert!(size_of::<RawType0>() == 0x1A);

/// Type 1: System Information (27 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawType1 {
    header: RawHeader,
    manufacturer: u8,
    product: u8,
    version: u8,
    serial: u8,
    uuid: [u8; 16],
    wake_up: u8,
    sku: u8,
    family: u8,
}
const _: () = assert!(size_of::<RawType1>() == 0x1B);

/// Type 2: Baseboard Information (SMBIOS 2.4+, 15 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawType2 {
    header: RawHeader,
    manufacturer: u8,
    product: u8,
    version: u8,
    serial: u8,
    asset_tag: u8,
    feature_flags: u8,
    location: u8,
    chassis: U16,
    board_type: u8,
    contained_count: u8,
}
const _: () = assert!(size_of::<RawType2>() == 0x0F);

/// Type 3: System Enclosure / Chassis (SMBIOS 2.3+, 21 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawType3 {
    header: RawHeader,
    manufacturer: u8,
    kind: u8,
    version: u8,
    serial: u8,
    asset_tag: u8,
    boot_state: u8,
    psu_state: u8,
    thermal_state: u8,
    security: u8,
    oem_defined: U32,
    height: u8,
    power_cords: u8,
    contained_elements: u8,
    contained_record_len: u8,
}
const _: () = assert!(size_of::<RawType3>() == 0x15);

/// Type 7: Cache Information (SMBIOS 3.1+, 27 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawType7 {
    header: RawHeader,
    socket: u8,
    config: U16,
    max_size_kb: U16,
    installed_size_kb: U16,
    supported_sram: U16,
    current_sram: U16,
    speed_ns: u8,
    ecc: u8,
    cache_type: u8,
    associativity: u8,
    max_size2_kb: U32,
    installed_size2_kb: U32,
}
const _: () = assert!(size_of::<RawType7>() == 0x1B);

/// Type 4: Processor Information (SMBIOS 3.0+, 48 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawType4 {
    header: RawHeader,
    socket: u8,
    proc_type: u8,
    family: u8,
    manufacturer: u8,
    processor_id: U64,
    version: u8,
    voltage: u8,
    external_clock: U16,
    max_speed: U16,
    current_speed: U16,
    status: u8,
    upgrade: u8,
    l1_handle: U16,
    l2_handle: U16,
    l3_handle: U16,
    serial: u8,
    asset_tag: u8,
    part_number: u8,
    core_count: u8,
    core_enabled: u8,
    thread_count: u8,
    characteristics: U16,
    family2: U16,
    core_count2: U16,
    core_enabled2: U16,
    thread_count2: U16,
}
const _: () = assert!(size_of::<RawType4>() == 0x30);

/// Type 16: Physical Memory Array (SMBIOS 2.7+, 23 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawType16 {
    header: RawHeader,
    location: u8,
    purpose: u8,
    ecc: u8,
    max_capacity_kb: U32,
    error_handle: U16,
    num_devices: U16,
    ext_max_capacity_bytes: U64,
}
const _: () = assert!(size_of::<RawType16>() == 0x17);

/// Type 17: Memory Device (SMBIOS 3.3+, 92 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawType17 {
    header: RawHeader,
    array_handle: U16,
    error_handle: U16,
    total_width: U16,
    data_width: U16,
    size_mb: U16,
    form_factor: u8,
    device_set: u8,
    device_locator: u8,
    bank_locator: u8,
    memory_type: u8,
    type_detail: U16,
    speed_mts: U16,
    manufacturer: u8,
    serial: u8,
    asset_tag: u8,
    part_number: u8,
    attributes: u8,
    extended_size_mb: U32,
    configured_clock_mts: U16,
    min_voltage_mv: U16,
    max_voltage_mv: U16,
    configured_voltage_mv: U16,
    memory_technology: u8,
    operating_mode_cap: U16,
    firmware_version: u8,
    module_manufacturer: U16,
    module_product: U16,
    controller_manufacturer: U16,
    controller_product: U16,
    nonvolatile_bytes: U64,
    volatile_bytes: U64,
    cache_bytes: U64,
    logical_bytes: U64,
    extended_speed_mts: U32,
    extended_configured_mts: U32,
}
const _: () = assert!(size_of::<RawType17>() == 0x5C);

/// Type 19: Memory Array Mapped Address (SMBIOS 2.7+, 31 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawType19 {
    header: RawHeader,
    start_kb: U32,
    end_kb: U32,
    array_handle: U16,
    partition_width: u8,
    ext_start: U64,
    ext_end: U64,
}
const _: () = assert!(size_of::<RawType19>() == 0x1F);

/// Type 32: System Boot Information (11 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawType32 {
    header: RawHeader,
    reserved: [u8; 6],
    boot_status: u8,
}
const _: () = assert!(size_of::<RawType32>() == 0x0B);

/// SMBIOS 3.0 64-bit entry point (24 bytes).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct RawEntryPoint {
    anchor: [u8; 5],
    checksum: u8,
    length: u8,
    major: u8,
    minor: u8,
    docrev: u8,
    ep_rev: u8,
    reserved: u8,
    max_size: U32,
    table_addr: U64,
}
const _: () = assert!(size_of::<RawEntryPoint>() == ENTRY_POINT_SIZE);

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Cursor-based writer that serializes SMBIOS structures into memory.
///
/// Tracks the current write position, handle counter, string count per
/// structure, overflow status, and the handle of the most recently created
/// physical memory array (for linking Type 17 → Type 16 → Type 19).
pub struct SmbiosWriter {
    /// Base address of the table area (after the entry point).
    base: *mut u8,
    /// Current write offset from `base`.
    offset: usize,
    /// Maximum writable size.
    limit: usize,
    /// Next handle to assign.
    next_handle: u16,
    /// Handle of the most recent Type 16 (Physical Memory Array).
    last_phys_mem_array_handle: u16,
    /// Number of strings written in the current structure.
    ///
    /// Reset by [`write_header`] and incremented by [`write_string`].
    string_count: u8,
    /// Set to `true` if any write exceeds the buffer limit.
    overflow: bool,
}

impl SmbiosWriter {
    /// Create a new writer targeting `table_base` (physical address of the
    /// structure table area, immediately after the entry point).
    ///
    /// # Safety
    ///
    /// `table_base` must point to writable memory of at least `MAX_TABLE_AREA`
    /// bytes. The caller ensures this region is in DRAM and not aliased.
    unsafe fn new(table_base: u64) -> Self {
        Self {
            base: table_base as *mut u8,
            offset: 0,
            limit: MAX_TABLE_AREA,
            next_handle: 1,
            last_phys_mem_array_handle: 0,
            string_count: 0,
            overflow: false,
        }
    }

    /// Create a writer with a custom limit (for testing overflow behavior).
    ///
    /// # Safety
    ///
    /// `table_base` must point to writable memory of at least `limit` bytes.
    #[cfg(test)]
    unsafe fn with_limit(table_base: u64, limit: usize) -> Self {
        Self {
            base: table_base as *mut u8,
            offset: 0,
            limit,
            next_handle: 1,
            last_phys_mem_array_handle: 0,
            string_count: 0,
            overflow: false,
        }
    }

    /// Current write position (bytes from table base).
    fn pos(&self) -> usize {
        self.offset
    }

    /// Whether any write exceeded the buffer limit.
    fn has_overflow(&self) -> bool {
        self.overflow
    }

    /// Allocate and return the next handle.
    fn alloc_handle(&mut self) -> u16 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }

    /// Copy raw bytes at the current position with a single bounds check.
    ///
    /// Bytes are written volatile: tables live in OS-visible DRAM that the
    /// CPU never reads back, so the stores must not be optimized away.
    fn write_raw(&mut self, bytes: &[u8]) {
        if self.offset + bytes.len() > self.limit {
            self.overflow = true;
            return;
        }
        // SAFETY: bounds-checked above, caller guarantees writable memory.
        unsafe {
            let dst = self.base.add(self.offset);
            for (i, &b) in bytes.iter().enumerate() {
                dst.add(i).write_volatile(b);
            }
        }
        self.offset += bytes.len();
    }

    // -----------------------------------------------------------------------
    // Structure framing helpers
    // -----------------------------------------------------------------------

    /// Allocate a handle for a new structure and reset the string counter.
    ///
    /// Returns the header prefix plus the handle for cross-references; the
    /// caller fills the remaining fields and passes the complete value to
    /// [`emplace`]. The Length byte comes from `size_of`, so it cannot
    /// drift from the fields.
    fn begin<T>(&mut self, ty: u8) -> (RawHeader, u16) {
        let handle = self.alloc_handle();
        self.string_count = 0;
        (
            RawHeader {
                ty,
                len: size_of::<T>() as u8,
                handle: U16::new(handle),
            },
            handle,
        )
    }

    /// Emit a complete wire-format structure with one bounds check.
    fn emplace<T: IntoBytes + Immutable>(&mut self, val: &T) {
        self.write_raw(val.as_bytes());
    }

    /// Write a null-terminated string and return its 1-based string index.
    ///
    /// If the string is empty, writes nothing and returns 0 (meaning "no
    /// string" in SMBIOS).  The string counter is automatically incremented
    /// for non-empty strings.
    fn write_string(&mut self, s: &str) -> u8 {
        if s.is_empty() {
            return 0;
        }
        self.string_count += 1;
        self.write_raw(s.as_bytes());
        self.write_raw(&[0]); // null terminator
        self.string_count
    }

    /// Terminate the string section with a final null byte.
    ///
    /// SMBIOS requires the string area to end with a double null.  If no
    /// strings were written, two null bytes are needed (empty string section).
    /// Uses the internally tracked `string_count`.
    fn end_strings(&mut self) {
        if self.string_count == 0 {
            // No strings: need two null bytes to terminate.
            self.write_raw(&[0]);
        }
        self.write_raw(&[0]); // second null (or first if no strings)
    }

    // -----------------------------------------------------------------------
    // Type 0: BIOS Information
    // -----------------------------------------------------------------------

    /// Add a Type 0 (BIOS Information) structure.
    pub fn add_bios_info(&mut self, vendor: &str, version: &str, release_date: &str) {
        let (header, _) = self.begin::<RawType0>(TYPE_BIOS_INFO);
        self.emplace(&RawType0 {
            header,
            vendor: 1,                         // string 1
            version: 2,                        // string 2
            start_segment: U16::new(0),        // N/A for UEFI/firmware
            release_date: 3,                   // string 3
            rom_size: 0xFF,                    // use extended field
            characteristics: U64::new(1 << 7), // PCI supported
            characteristics_ext1: 0,
            characteristics_ext2: 1 << 4, // is virtual machine
            bios_major: 0,
            bios_minor: 1,
            ec_major: 0xFF,            // N/A
            ec_minor: 0xFF,            // N/A
            ext_rom_size: U16::new(0), // SMBIOS 3.1+
        });

        // Strings
        self.write_string(vendor);
        self.write_string(version);
        self.write_string(release_date);
        self.end_strings();
    }

    // -----------------------------------------------------------------------
    // Type 1: System Information
    // -----------------------------------------------------------------------

    /// Add a Type 1 (System Information) structure.
    pub fn add_system_info(
        &mut self,
        manufacturer: &str,
        product: &str,
        version: &str,
        serial: Option<&str>,
    ) {
        // Pre-compute serial string index (depends on whether it's present).
        let has_serial = serial.is_some_and(|s| !s.is_empty());
        let (header, _) = self.begin::<RawType1>(TYPE_SYSTEM_INFO);
        self.emplace(&RawType1 {
            header,
            manufacturer: 1, // string 1
            product: 2,      // string 2
            version: 3,      // string 3
            serial: if has_serial { 4 } else { 0 },
            uuid: [0; 16], // not specified
            wake_up: 0x06, // power switch
            sku: 0,        // no string
            family: 0,     // no string
        });

        // Strings
        self.write_string(manufacturer);
        self.write_string(product);
        self.write_string(version);
        if let Some(s) = serial {
            self.write_string(s);
        }
        self.end_strings();
    }

    // -----------------------------------------------------------------------
    // Type 2: Baseboard Information
    // -----------------------------------------------------------------------

    /// Add a Type 2 (Baseboard Information) structure.
    pub fn add_baseboard_info(&mut self, manufacturer: &str, product: &str) {
        let (header, _) = self.begin::<RawType2>(TYPE_BASEBOARD_INFO);
        self.emplace(&RawType2 {
            header,
            manufacturer: 1,      // string 1
            product: 2,           // string 2
            version: 0,           // no string
            serial: 0,            // no string
            asset_tag: 0,         // no string
            feature_flags: 0x09,  // hosting board, replaceable
            location: 0,          // no string
            chassis: U16::new(0), // unset
            board_type: 0x0A,     // motherboard
            contained_count: 0,
        });

        // Strings
        self.write_string(manufacturer);
        self.write_string(product);
        self.end_strings();
    }

    // -----------------------------------------------------------------------
    // Type 3: System Enclosure
    // -----------------------------------------------------------------------

    /// Add a Type 3 (System Enclosure / Chassis) structure.
    pub fn add_enclosure(&mut self, chassis_type: u8, manufacturer: &str) {
        let (header, _) = self.begin::<RawType3>(TYPE_ENCLOSURE);
        self.emplace(&RawType3 {
            header,
            manufacturer: 1, // string 1
            kind: chassis_type,
            version: 0,          // no string
            serial: 0,           // no string
            asset_tag: 0,        // no string
            boot_state: 0x03,    // safe
            psu_state: 0x03,     // safe
            thermal_state: 0x03, // safe
            security: 0x03,      // none
            oem_defined: U32::new(0),
            height: 0,      // unspecified
            power_cords: 0, // unspecified
            contained_elements: 0,
            contained_record_len: 0,
        });

        // Strings
        self.write_string(manufacturer);
        self.end_strings();
    }

    // -----------------------------------------------------------------------
    // Type 7: Cache Information
    // -----------------------------------------------------------------------

    /// Add a Type 7 (Cache Information) structure.
    ///
    /// Returns the assigned handle for use in Type 4 (Processor) cache
    /// handle fields (L1, L2, L3).
    ///
    /// # Arguments
    ///
    /// * `designation` — Cache socket designation (e.g., "L1 Data Cache").
    /// * `level` — Cache level: 1, 2, or 3.
    /// * `size_kb` — Cache size in KiB.
    /// * `associativity` — SMBIOS associativity byte (use
    ///   `CacheAssociativity::to_smbios_byte()`).
    /// * `cache_type` — SMBIOS system cache type byte (use
    ///   `CacheType::to_smbios_byte()`).
    pub fn add_cache_info(
        &mut self,
        designation: &str,
        level: u8,
        size_kb: u32,
        associativity: u8,
        cache_type: u8,
    ) -> u16 {
        // Cache Configuration (16-bit):
        //   bits 0-2: cache level (0-based, so L1 = 0)
        //   bit 3: socketed (0 = not socketed)
        //   bits 5-6: location (0 = internal)
        //   bit 7: enabled (1 = enabled)
        //   bits 8-9: operational mode (1 = write-back)
        let config = ((level.saturating_sub(1) & 0x07) as u16)
            | (1 << 7)  // enabled
            | (1 << 8); // write-back

        // Legacy size fields: KiB granularity up to 32767, else 64 KiB
        // granularity with bit 15 set.
        let legacy_size = if size_kb <= 0x7FFF {
            size_kb as u16
        } else {
            0x8000 | ((size_kb / 64) as u16 & 0x7FFF)
        };

        let (header, handle) = self.begin::<RawType7>(TYPE_CACHE_INFO);
        self.emplace(&RawType7 {
            header,
            socket: 1, // string 1
            config: U16::new(config),
            max_size_kb: U16::new(legacy_size),
            installed_size_kb: U16::new(legacy_size),
            supported_sram: U16::new(0x0002), // unknown
            current_sram: U16::new(0x0002),   // unknown
            speed_ns: 0,                      // unknown
            ecc: 0,                           // unknown
            cache_type,
            associativity,
            max_size2_kb: U32::new(size_kb),
            installed_size2_kb: U32::new(size_kb),
        });

        // Strings
        self.write_string(designation);
        self.end_strings();

        handle
    }

    // -----------------------------------------------------------------------
    // Type 4: Processor Information
    // -----------------------------------------------------------------------

    /// Add a Type 4 (Processor Information) structure.
    ///
    /// `processor_family` is the SMBIOS "Processor Family 2" 16-bit value
    /// (e.g., `0x0119` for AArch64, `0x28` for x86-64, `0x0135` for RISC-V).
    /// Use [`fstart_core::smbios::ProcessorFamily::to_smbios_u16`] to
    /// convert from the typed enum.
    pub fn add_processor(
        &mut self,
        socket: &str,
        manufacturer: &str,
        processor_family: u16,
        max_speed_mhz: u16,
        core_count: u16,
        core_enabled: u16,
        thread_count: u16,
    ) {
        self.add_processor_with_caches(
            socket,
            manufacturer,
            processor_family,
            max_speed_mhz,
            core_count,
            core_enabled,
            thread_count,
            0xFFFF,
            0xFFFF,
            0xFFFF,
        );
    }

    /// Add a Type 4 (Processor Information) structure with cache handles.
    ///
    /// Like [`add_processor`] but links to Type 7 cache entries.
    /// Pass `0xFFFF` for cache handles that are not available.
    #[allow(clippy::too_many_arguments)]
    pub fn add_processor_with_caches(
        &mut self,
        socket: &str,
        manufacturer: &str,
        processor_family: u16,
        max_speed_mhz: u16,
        core_count: u16,
        core_enabled: u16,
        thread_count: u16,
        l1_cache_handle: u16,
        l2_cache_handle: u16,
        l3_cache_handle: u16,
    ) {
        let (header, _) = self.begin::<RawType4>(TYPE_PROCESSOR);
        self.emplace(&RawType4 {
            header,
            socket: 1,       // string 1
            proc_type: 0x03, // central processor
            family: 0xFE,    // see family2 field
            manufacturer: 2, // string 2
            processor_id: U64::new(0),
            version: 0, // no string
            voltage: 0,
            external_clock: U16::new(0), // unknown
            max_speed: U16::new(max_speed_mhz),
            current_speed: U16::new(max_speed_mhz),
            status: 0x41, // enabled, CPU socket populated
            upgrade: 0,   // unknown
            l1_handle: U16::new(l1_cache_handle),
            l2_handle: U16::new(l2_cache_handle),
            l3_handle: U16::new(l3_cache_handle),
            serial: 0,      // no string
            asset_tag: 0,   // no string
            part_number: 0, // no string
            core_count: cap_u8(core_count),
            core_enabled: cap_u8(core_enabled),
            thread_count: cap_u8(thread_count),
            characteristics: U16::new(0),
            family2: U16::new(processor_family),
            core_count2: U16::new(core_count),
            core_enabled2: U16::new(core_enabled),
            thread_count2: U16::new(thread_count),
        });

        // Strings
        self.write_string(socket);
        self.write_string(manufacturer);
        self.end_strings();
    }

    // -----------------------------------------------------------------------
    // Type 16: Physical Memory Array
    // -----------------------------------------------------------------------

    /// Add a Type 16 (Physical Memory Array) structure.
    ///
    /// `max_capacity_kb` is the maximum memory capacity in kilobytes.
    /// `num_devices` is the number of memory devices (Type 17) that
    /// belong to this array.
    ///
    /// Returns the handle for use in Type 17/19 references.
    pub fn add_physical_memory_array(&mut self, max_capacity_kb: u64, num_devices: u16) -> u16 {
        // Maximum capacity: if >2TB, set to 0x80000000 and use extended field.
        let max_cap_field = if max_capacity_kb > 0x7FFF_FFFF {
            0x8000_0000u32
        } else {
            max_capacity_kb as u32
        };
        let (header, handle) = self.begin::<RawType16>(TYPE_PHYS_MEM_ARRAY);
        self.last_phys_mem_array_handle = handle;
        self.emplace(&RawType16 {
            header,
            location: 0x03, // system board
            purpose: 0x03,  // system memory
            ecc: 0x03,      // none
            max_capacity_kb: U32::new(max_cap_field),
            error_handle: U16::new(0xFFFE), // not provided
            num_devices: U16::new(num_devices),
            ext_max_capacity_bytes: U64::new(max_capacity_kb * 1024),
        });

        // No strings
        self.end_strings();

        handle
    }

    // -----------------------------------------------------------------------
    // Type 17: Memory Device
    // -----------------------------------------------------------------------

    /// Add a Type 17 (Memory Device) structure.
    ///
    /// `locator` is the device locator string (e.g., "DIMM0").
    /// `size_mb` is the memory size in megabytes.
    /// `speed_mhz` is the memory speed in MHz.
    /// `memory_type` is the SMBIOS memory type byte.
    pub fn add_memory_device(
        &mut self,
        locator: &str,
        size_mb: u32,
        speed_mhz: u16,
        memory_type: u8,
    ) {
        // Size field: if size_mb fits in 15 bits, use directly.
        // Otherwise set 0x7FFF and use extended size.
        let size_field = if size_mb <= 0x7FFF {
            size_mb as u16
        } else {
            0x7FFF // see extended size
        };
        let (header, _) = self.begin::<RawType17>(TYPE_MEMORY_DEVICE);
        self.emplace(&RawType17 {
            header,
            array_handle: U16::new(self.last_phys_mem_array_handle),
            error_handle: U16::new(0xFFFE), // not provided
            total_width: U16::new(64),      // assume 64-bit
            data_width: U16::new(64),
            size_mb: U16::new(size_field),
            form_factor: 0x09, // DIMM
            device_set: 0,     // none
            device_locator: 1, // string 1
            bank_locator: 0,   // no string
            memory_type,
            type_detail: U16::new(0), // unknown
            speed_mts: U16::new(speed_mhz),
            manufacturer: 0, // no string
            serial: 0,       // no string
            asset_tag: 0,    // no string
            part_number: 0,  // no string
            attributes: 0,   // unknown rank
            extended_size_mb: U32::new(size_mb),
            configured_clock_mts: U16::new(speed_mhz),
            min_voltage_mv: U16::new(0),        // unknown
            max_voltage_mv: U16::new(0),        // unknown
            configured_voltage_mv: U16::new(0), // unknown
            memory_technology: 0,               // unknown
            operating_mode_cap: U16::new(0),
            firmware_version: 0, // no string
            module_manufacturer: U16::new(0),
            module_product: U16::new(0),
            controller_manufacturer: U16::new(0),
            controller_product: U16::new(0),
            nonvolatile_bytes: U64::new(0), // none
            volatile_bytes: U64::new(size_mb as u64 * 1024 * 1024),
            cache_bytes: U64::new(0),
            logical_bytes: U64::new(0),
            extended_speed_mts: U32::new(speed_mhz as u32),
            extended_configured_mts: U32::new(speed_mhz as u32),
        });

        // Strings
        self.write_string(locator);
        self.end_strings();
    }

    // -----------------------------------------------------------------------
    // Type 19: Memory Array Mapped Address
    // -----------------------------------------------------------------------

    /// Add a Type 19 (Memory Array Mapped Address) structure.
    ///
    /// `start_addr` and `end_addr` are physical byte addresses.
    pub fn add_memory_array_mapped_address(
        &mut self,
        start_addr: u64,
        end_addr: u64,
        partition_width: u8,
    ) {
        // For addresses within 4 GiB, use KB-granularity fields.
        // For larger addresses, set 0xFFFFFFFF and use extended fields.
        let (start_kb, end_kb) = if end_addr > 0xFFFF_FFFF_u64 * 1024 {
            (0xFFFF_FFFF, 0xFFFF_FFFF) // see extended fields
        } else {
            ((start_addr / 1024) as u32, (end_addr / 1024) as u32)
        };
        let (header, _) = self.begin::<RawType19>(TYPE_MEM_ARRAY_MAPPED_ADDR);
        self.emplace(&RawType19 {
            header,
            start_kb: U32::new(start_kb),
            end_kb: U32::new(end_kb),
            array_handle: U16::new(self.last_phys_mem_array_handle),
            partition_width,
            ext_start: U64::new(start_addr),
            ext_end: U64::new(end_addr),
        });

        // No strings
        self.end_strings();
    }

    // -----------------------------------------------------------------------
    // Type 32: System Boot Information
    // -----------------------------------------------------------------------

    /// Add a Type 32 (System Boot Information) structure.
    pub fn add_system_boot_info(&mut self) {
        let (header, _) = self.begin::<RawType32>(TYPE_SYSTEM_BOOT);
        self.emplace(&RawType32 {
            header,
            reserved: [0; 6],
            boot_status: 0, // no errors detected
        });

        // No strings
        self.end_strings();
    }

    // -----------------------------------------------------------------------
    // Type 127: End-of-Table
    // -----------------------------------------------------------------------

    /// Add a Type 127 (End-of-Table) structure.
    pub fn add_end_of_table(&mut self) {
        // Header only: Length comes out as 4 via `size_of::<RawHeader>`.
        let (header, _) = self.begin::<RawHeader>(TYPE_END_OF_TABLE);
        self.emplace(&header);

        // String area: double null
        self.write_raw(&[0, 0]);
    }
}

// ---------------------------------------------------------------------------
// Entry point and public API
// ---------------------------------------------------------------------------

/// Compute a checksum byte such that the sum of all bytes in the structure
/// (including the checksum field) wraps to zero.
fn compute_checksum(data: &[u8]) -> u8 {
    0u8.wrapping_sub(data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)))
}

/// Assemble and write SMBIOS tables to the given physical address.
///
/// The SMBIOS 3.0 entry point is written at `table_addr`, followed by
/// all structure tables.  The closure `f` receives a [`SmbiosWriter`]
/// to add individual structures.
///
/// Returns the total number of bytes written (entry point + tables).
///
/// # Panics
///
/// Panics if the table data exceeds `MAX_TABLE_AREA` (64 KiB).  This
/// indicates a configuration error (too many structures) and must not
/// produce a silently corrupt SMBIOS image.
///
/// # Safety
///
/// `table_addr` must point to writable DRAM with at least
/// `ENTRY_POINT_SIZE + MAX_TABLE_AREA` bytes available.
pub fn assemble_and_write(table_addr: u64, f: impl FnOnce(&mut SmbiosWriter)) -> usize {
    let table_base = table_addr + ENTRY_POINT_SIZE as u64;

    // SAFETY: caller guarantees writable memory at table_addr.
    let mut writer = unsafe { SmbiosWriter::new(table_base) };

    // Let the caller add all structures.
    f(&mut writer);

    assert!(
        !writer.has_overflow(),
        "SMBIOS table data exceeded {} bytes — reduce table count or increase MAX_TABLE_AREA",
        MAX_TABLE_AREA
    );

    let table_size = writer.pos();

    // Write the SMBIOS 3.0 64-bit entry point at table_addr.
    let mut ep = RawEntryPoint {
        anchor: SM3_MAGIC,
        checksum: 0, // filled below
        length: ENTRY_POINT_SIZE as u8,
        major: 3,
        minor: 0,
        docrev: 0,
        ep_rev: 0x01, // SMBIOS 3.0
        reserved: 0,
        max_size: U32::new(table_size as u32),
        table_addr: U64::new(table_base),
    };
    ep.checksum = compute_checksum(ep.as_bytes());

    // SAFETY: caller guarantees writable memory at table_addr.
    unsafe {
        let ep_ptr = table_addr as *mut u8;
        for (i, &b) in ep.as_bytes().iter().enumerate() {
            ep_ptr.add(i).write_volatile(b);
        }
    }

    ENTRY_POINT_SIZE + table_size
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::{vec, vec::Vec};

    /// Allocate a buffer on the heap and run assemble_and_write into it.
    fn write_to_buffer(f: impl FnOnce(&mut SmbiosWriter)) -> (Vec<u8>, usize) {
        let mut buf = vec![0u8; ENTRY_POINT_SIZE + MAX_TABLE_AREA];
        let addr = buf.as_mut_ptr() as u64;
        let total = assemble_and_write(addr, f);
        (buf, total)
    }

    #[test]
    fn test_entry_point_signature_and_checksum() {
        let (buf, total) = write_to_buffer(|w| {
            w.add_end_of_table();
        });

        assert!(total > ENTRY_POINT_SIZE);

        // Check signature
        assert_eq!(&buf[0..5], b"_SM3_");

        // Check checksum: sum of first 24 bytes should be 0 mod 256
        let sum: u8 = buf[..ENTRY_POINT_SIZE]
            .iter()
            .fold(0u8, |a, &b| a.wrapping_add(b));
        assert_eq!(sum, 0, "entry point checksum must be zero");

        // Check version
        assert_eq!(buf[7], 3); // major
        assert_eq!(buf[8], 0); // minor
    }

    #[test]
    fn test_end_of_table_structure() {
        let (buf, _total) = write_to_buffer(|w| {
            w.add_end_of_table();
        });

        // End-of-table starts at offset 24 (after entry point)
        let eot = &buf[ENTRY_POINT_SIZE..];
        assert_eq!(eot[0], TYPE_END_OF_TABLE); // type
        assert_eq!(eot[1], 4); // length
        // handle: u16 LE
        assert_eq!(u16::from_le_bytes([eot[2], eot[3]]), 1);
        // double-null terminator
        assert_eq!(eot[4], 0);
        assert_eq!(eot[5], 0);
    }

    #[test]
    fn test_bios_info_strings() {
        let (buf, _total) = write_to_buffer(|w| {
            w.add_bios_info("fstart", "0.1.0", "03/10/2026");
            w.add_end_of_table();
        });

        let table = &buf[ENTRY_POINT_SIZE..];
        // Type 0
        assert_eq!(table[0], TYPE_BIOS_INFO);
        let struct_len = table[1] as usize;
        // Strings start after the fixed structure
        let string_area = &table[struct_len..];
        let string_data =
            core::str::from_utf8(&string_area[..string_area.iter().position(|&b| b == 0).unwrap()])
                .unwrap();
        assert_eq!(string_data, "fstart");
    }

    #[test]
    fn test_system_info_structure() {
        let (buf, _total) = write_to_buffer(|w| {
            w.add_system_info("QEMU", "SBSA Reference", "1.0", Some("SN12345"));
            w.add_end_of_table();
        });

        let table = &buf[ENTRY_POINT_SIZE..];
        assert_eq!(table[0], TYPE_SYSTEM_INFO);
        assert_eq!(table[1], 0x1B); // struct length
        // manufacturer = string 1, product = string 2, version = string 3, serial = string 4
        assert_eq!(table[4], 1);
        assert_eq!(table[5], 2);
        assert_eq!(table[6], 3);
        assert_eq!(table[7], 4); // serial present
    }

    #[test]
    fn test_processor_info_structure() {
        let (buf, _total) = write_to_buffer(|w| {
            w.add_processor("CPU0", "ARM", 0x0119, 2000, 4, 4, 4);
            w.add_end_of_table();
        });

        let table = &buf[ENTRY_POINT_SIZE..];
        assert_eq!(table[0], TYPE_PROCESSOR);
        assert_eq!(table[1], 0x30); // 48 bytes
        // max speed at offset 0x14 (20-21)
        let max_speed = u16::from_le_bytes([table[0x14], table[0x15]]);
        assert_eq!(max_speed, 2000);
        // processor family 2 at offset 0x28 (40-41)
        let family2 = u16::from_le_bytes([table[0x28], table[0x29]]);
        assert_eq!(family2, 0x0119, "processor family 2 should be AArch64");
    }

    #[test]
    fn test_processor_family_x86() {
        let (buf, _total) = write_to_buffer(|w| {
            w.add_processor("CPU0", "Intel", 0x28, 3600, 8, 8, 16);
            w.add_end_of_table();
        });

        let table = &buf[ENTRY_POINT_SIZE..];
        // processor family 2 at offset 0x28 (40-41)
        let family2 = u16::from_le_bytes([table[0x28], table[0x29]]);
        assert_eq!(family2, 0x28, "processor family 2 should be x86-64");
        // core count at offset 0x23 (35)
        assert_eq!(table[0x23], 8);
        // thread count at offset 0x25 (37)
        assert_eq!(table[0x25], 16);
    }

    #[test]
    fn test_memory_structures() {
        let (buf, _total) = write_to_buffer(|w| {
            w.add_physical_memory_array(1024 * 1024, 1); // 1 GB in KB
            w.add_memory_device("DIMM0", 1024, 2400, 0x1A); // DDR4
            w.add_memory_array_mapped_address(0x10000000000, 0x1003FFFFFFF, 1);
            w.add_end_of_table();
        });

        let table = &buf[ENTRY_POINT_SIZE..];
        assert_eq!(table[0], TYPE_PHYS_MEM_ARRAY); // first structure is Type 16
    }

    #[test]
    fn test_full_table_set() {
        let (buf, total) = write_to_buffer(|w| {
            w.add_bios_info("fstart", "0.1.0", "03/10/2026");
            w.add_system_info("QEMU", "SBSA Reference", "1.0", None);
            w.add_baseboard_info("QEMU", "sbsa-ref");
            w.add_enclosure(0x17, "QEMU"); // rack mount
            w.add_processor("CPU0", "ARM", 0x0119, 2000, 1, 1, 1);
            w.add_physical_memory_array(1024 * 1024, 1);
            w.add_memory_device("DIMM0", 1024, 2400, 0x1A);
            w.add_memory_array_mapped_address(0x10000000000, 0x1003FFFFFFF, 1);
            w.add_system_boot_info();
            w.add_end_of_table();
        });

        // Verify checksum
        let sum: u8 = buf[..ENTRY_POINT_SIZE]
            .iter()
            .fold(0u8, |a, &b| a.wrapping_add(b));
        assert_eq!(sum, 0, "entry point checksum must be zero");

        // Table size is reasonable (not just the entry point)
        assert!(
            total > ENTRY_POINT_SIZE + 100,
            "tables should be non-trivial"
        );
        assert!(total < 2048, "tables should be compact");
    }

    #[test]
    fn test_string_count_tracking() {
        // Verify that the automatic string counter produces correct indices.
        let (buf, _total) = write_to_buffer(|w| {
            w.add_system_info("Mfr", "Prod", "Ver", Some("Serial"));
            w.add_end_of_table();
        });

        let table = &buf[ENTRY_POINT_SIZE..];
        let struct_len = table[1] as usize;
        let string_area = &table[struct_len..];

        // Extract all 4 strings from the string area
        let mut strings = Vec::new();
        let mut pos = 0;
        while pos < string_area.len() {
            if string_area[pos] == 0 {
                break;
            }
            let end = string_area[pos..].iter().position(|&b| b == 0).unwrap() + pos;
            strings.push(core::str::from_utf8(&string_area[pos..end]).unwrap());
            pos = end + 1;
        }
        assert_eq!(strings, vec!["Mfr", "Prod", "Ver", "Serial"]);
    }

    #[test]
    fn test_type2_string_area_alignment() {
        // The declared Length must cover the whole formatted area, or
        // parsers read the first string byte as a field (this used to eat
        // the first manufacturer character: no contained-count byte was
        // written while 0x0F was declared).
        let (buf, _total) = write_to_buffer(|w| {
            w.add_baseboard_info("ACME", "Board");
            w.add_end_of_table();
        });

        let table = &buf[ENTRY_POINT_SIZE..];
        assert_eq!(table[0], TYPE_BASEBOARD_INFO);
        assert_eq!(table[1], 0x0F);
        let len = table[1] as usize;
        assert_eq!(&table[len..len + 5], b"ACME\0");
    }

    #[test]
    fn test_type17_extended_length() {
        // SMBIOS 3.3 extended-speed dwords require length 0x5C; the old
        // 0x54 declaration pushed them into the string area.
        let (buf, _total) = write_to_buffer(|w| {
            w.add_memory_device("DIMM0", 1024, 2400, 0x1A);
            w.add_end_of_table();
        });

        let table = &buf[ENTRY_POINT_SIZE..];
        assert_eq!(table[0], TYPE_MEMORY_DEVICE);
        assert_eq!(table[1], 0x5C);
        // Extended speed / configured speed at offsets 0x54 / 0x58.
        assert_eq!(
            u32::from_le_bytes(table[0x54..0x58].try_into().unwrap()),
            2400
        );
        assert_eq!(
            u32::from_le_bytes(table[0x58..0x5C].try_into().unwrap()),
            2400
        );
        // Locator string follows the formatted area intact.
        assert_eq!(&table[0x5C..0x5C + 6], b"DIMM0\0");
    }

    #[test]
    fn test_overflow_detection() {
        // Allocate a real buffer but limit the writer to 8 bytes.
        let mut buf = vec![0u8; 128];
        let base = buf.as_mut_ptr() as u64;
        let mut writer = unsafe { SmbiosWriter::with_limit(base, 8) };

        // Write exactly 8 bytes: should succeed.
        writer.write_raw(&[0xAA; 8]);
        assert!(!writer.has_overflow(), "should not overflow at limit");

        // Write one more: should trigger overflow.
        writer.write_raw(&[0xBB]);
        assert!(writer.has_overflow(), "should detect overflow past limit");
    }

    #[test]
    fn test_overflow_panics_in_assemble() {
        // assemble_and_write should panic if the closure overflows.
        let mut buf = vec![0u8; ENTRY_POINT_SIZE + MAX_TABLE_AREA];
        let addr = buf.as_mut_ptr() as u64;

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assemble_and_write(addr, |w| {
                // Overwrite the limit to something tiny to force overflow
                // without actually writing 64K of data.
                w.limit = 4;
                w.write_raw(&[0u8; 4]); // exactly at limit
                w.write_raw(&[0xFF]); // overflow
            });
        }));
        assert!(
            result.is_err(),
            "assemble_and_write should panic on overflow"
        );
    }
}
