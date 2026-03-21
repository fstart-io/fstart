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
//! let total = fstart_smbios::assemble_and_write(0x10000090000, |w| {
//!     w.add_bios_info("fstart", "0.1.0", "03/10/2026");
//!     w.add_system_info("QEMU", "SBSA Reference", "1.0", None);
//!     w.add_end_of_table();
//! });
//! ```

#![cfg_attr(not(any(test, feature = "std")), no_std)]

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
// Helpers
// ---------------------------------------------------------------------------

/// Cap a `u16` to `u8`, returning `0xFF` if the value exceeds 255.
///
/// Used for SMBIOS fields that have a 1-byte "legacy" field with a separate
/// 2-byte extended field for values > 255 (e.g., core count, thread count).
fn cap_u8(val: u16) -> u8 {
    if val > 255 {
        0xFF
    } else {
        val as u8
    }
}

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

    /// Write a single byte at the current position.
    fn write_u8(&mut self, val: u8) {
        if self.offset < self.limit {
            // SAFETY: bounds-checked above, caller guarantees writable memory.
            unsafe { self.base.add(self.offset).write_volatile(val) };
            self.offset += 1;
        } else {
            self.overflow = true;
        }
    }

    /// Write a little-endian u16.
    fn write_u16(&mut self, val: u16) {
        self.write_u8(val as u8);
        self.write_u8((val >> 8) as u8);
    }

    /// Write a little-endian u32.
    fn write_u32(&mut self, val: u32) {
        self.write_u16(val as u16);
        self.write_u16((val >> 16) as u16);
    }

    /// Write a little-endian u64.
    fn write_u64(&mut self, val: u64) {
        self.write_u32(val as u32);
        self.write_u32((val >> 32) as u32);
    }

    /// Write `count` zero bytes.
    fn write_zeros(&mut self, count: usize) {
        for _ in 0..count {
            self.write_u8(0);
        }
    }

    // -----------------------------------------------------------------------
    // Structure framing helpers
    // -----------------------------------------------------------------------

    /// Write the 4-byte SMBIOS structure header and reset the string counter.
    ///
    /// Returns the assigned handle for use in cross-references.
    fn write_header(&mut self, type_id: u8, struct_len: u8) -> u16 {
        let handle = self.alloc_handle();
        self.write_u8(type_id);
        self.write_u8(struct_len);
        self.write_u16(handle);
        self.string_count = 0;
        handle
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
        for &b in s.as_bytes() {
            self.write_u8(b);
        }
        self.write_u8(0); // null terminator
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
            self.write_u8(0);
        }
        self.write_u8(0); // second null (or first if no strings)
    }

    // -----------------------------------------------------------------------
    // Type 0: BIOS Information
    // -----------------------------------------------------------------------

    /// Add a Type 0 (BIOS Information) structure.
    pub fn add_bios_info(&mut self, vendor: &str, version: &str, release_date: &str) {
        self.write_header(TYPE_BIOS_INFO, 0x1A); // SMBIOS 2.4+: 26 bytes

        // Fixed fields
        self.write_u8(1); // vendor (string 1)
        self.write_u8(2); // version (string 2)
        self.write_u16(0); // BIOS starting address segment (N/A for UEFI/firmware)
        self.write_u8(3); // release date (string 3)
        self.write_u8(0xFF); // BIOS ROM size (0xFF = use extended field)
        self.write_u64(1 << 7); // characteristics: PCI supported
        self.write_u8(0); // characteristics ext byte 1
        self.write_u8(1 << 4); // characteristics ext byte 2: is virtual machine
        self.write_u8(0); // system BIOS major release
        self.write_u8(1); // system BIOS minor release
        self.write_u8(0xFF); // embedded controller firmware major (N/A)
        self.write_u8(0xFF); // embedded controller firmware minor (N/A)
        self.write_u16(0); // extended BIOS ROM size (SMBIOS 3.1+)

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
        self.write_header(TYPE_SYSTEM_INFO, 0x1B); // 27 bytes

        // Pre-compute serial string index (depends on whether it's present).
        let has_serial = serial.is_some_and(|s| !s.is_empty());
        let serial_idx = if has_serial { 4 } else { 0 };

        // Fixed fields
        self.write_u8(1); // manufacturer (string 1)
        self.write_u8(2); // product name (string 2)
        self.write_u8(3); // version (string 3)
        self.write_u8(serial_idx); // serial number
        self.write_zeros(16); // UUID: all zeros (not specified)
        self.write_u8(0x06); // wake-up type: power switch
        self.write_u8(0); // SKU (no string)
        self.write_u8(0); // family (no string)

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
        self.write_header(TYPE_BASEBOARD_INFO, 0x0F); // 15 bytes

        // Fixed fields
        self.write_u8(1); // manufacturer (string 1)
        self.write_u8(2); // product (string 2)
        self.write_u8(0); // version (no string)
        self.write_u8(0); // serial number (no string)
        self.write_u8(0); // asset tag (no string)
        self.write_u8(0x09); // feature flags: hosting board, replaceable
        self.write_u8(0); // location in chassis (no string)
        self.write_u16(0); // chassis handle (unset)
        self.write_u8(0x0A); // board type: motherboard

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
        self.write_header(TYPE_ENCLOSURE, 0x15); // 21 bytes (SMBIOS 2.3+)

        // Fixed fields
        self.write_u8(1); // manufacturer (string 1)
        self.write_u8(chassis_type); // type
        self.write_u8(0); // version (no string)
        self.write_u8(0); // serial number (no string)
        self.write_u8(0); // asset tag (no string)
        self.write_u8(0x03); // boot-up state: safe
        self.write_u8(0x03); // power supply state: safe
        self.write_u8(0x03); // thermal state: safe
        self.write_u8(0x03); // security status: none
        self.write_u32(0); // OEM-defined
        self.write_u8(0); // height (unspecified)
        self.write_u8(0); // number of power cords (unspecified)
        self.write_u8(0); // contained element count
        self.write_u8(0); // contained element record length

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
        let handle = self.write_header(TYPE_CACHE_INFO, 0x1B); // 27 bytes (SMBIOS 3.1+)

        // Cache Configuration (16-bit):
        //   bits 0-2: cache level (0-based, so L1 = 0)
        //   bit 3: socketed (0 = not socketed)
        //   bits 5-6: location (0 = internal)
        //   bit 7: enabled (1 = enabled)
        //   bits 8-9: operational mode (1 = write-back)
        let config = ((level.saturating_sub(1) & 0x07) as u16)
            | (1 << 7)  // enabled
            | (1 << 8); // write-back

        self.write_u8(1); // socket designation (string 1)
        self.write_u16(config); // cache configuration

        // Maximum Cache Size (legacy, KiB granularity, max 32767 KiB).
        if size_kb <= 0x7FFF {
            self.write_u16(size_kb as u16); // max cache size
        } else {
            self.write_u16(0x8000 | ((size_kb / 64) as u16 & 0x7FFF)); // 64K granularity
        }

        // Installed Size (same as max).
        if size_kb <= 0x7FFF {
            self.write_u16(size_kb as u16);
        } else {
            self.write_u16(0x8000 | ((size_kb / 64) as u16 & 0x7FFF));
        }

        self.write_u16(0x0002); // supported SRAM type: unknown
        self.write_u16(0x0002); // current SRAM type: unknown
        self.write_u8(0); // cache speed (unknown)
        self.write_u8(0); // error correction type: unknown
        self.write_u8(cache_type); // system cache type
        self.write_u8(associativity); // associativity

        // SMBIOS 3.1+ extended fields (32-bit sizes in KiB).
        self.write_u32(size_kb); // maximum cache size 2
        self.write_u32(size_kb); // installed cache size 2

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
    /// Use [`fstart_types::smbios::ProcessorFamily::to_smbios_u16`] to
    /// convert from the typed enum.
    pub fn add_processor(
        &mut self,
        socket: &str,
        manufacturer: &str,
        processor_family: u16,
        max_speed_mhz: u16,
        core_count: u16,
        thread_count: u16,
    ) {
        self.add_processor_with_caches(
            socket,
            manufacturer,
            processor_family,
            max_speed_mhz,
            core_count,
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
        thread_count: u16,
        l1_cache_handle: u16,
        l2_cache_handle: u16,
        l3_cache_handle: u16,
    ) {
        self.write_header(TYPE_PROCESSOR, 0x30); // 48 bytes (SMBIOS 3.0+)

        // Fixed fields
        self.write_u8(1); // socket designation (string 1)
        self.write_u8(0x03); // processor type: central processor
        self.write_u8(0xFE); // processor family: see family2 field
        self.write_u8(2); // manufacturer (string 2)
        self.write_u64(0); // processor ID (zeroed)
        self.write_u8(0); // version (no string)
        self.write_u8(0); // voltage
        self.write_u16(0); // external clock (unknown)
        self.write_u16(max_speed_mhz); // max speed
        self.write_u16(max_speed_mhz); // current speed
        self.write_u8(0x41); // status: enabled, CPU socket populated
        self.write_u8(0); // processor upgrade: unknown
        self.write_u16(l1_cache_handle); // L1 cache handle
        self.write_u16(l2_cache_handle); // L2 cache handle
        self.write_u16(l3_cache_handle); // L3 cache handle
        self.write_u8(0); // serial number (no string)
        self.write_u8(0); // asset tag (no string)
        self.write_u8(0); // part number (no string)
        self.write_u8(cap_u8(core_count)); // core count (legacy)
        self.write_u8(cap_u8(core_count)); // core enabled (legacy)
        self.write_u8(cap_u8(thread_count)); // thread count (legacy)
        self.write_u16(0); // processor characteristics
        self.write_u16(processor_family); // processor family 2
                                          // SMBIOS 3.0 extended fields
        self.write_u16(core_count); // core count 2
        self.write_u16(core_count); // core enabled 2
        self.write_u16(thread_count); // thread count 2

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
        let handle = self.write_header(TYPE_PHYS_MEM_ARRAY, 0x17); // 23 bytes (SMBIOS 2.7+)
        self.last_phys_mem_array_handle = handle;

        // Fixed fields
        self.write_u8(0x03); // location: system board
        self.write_u8(0x03); // use: system memory
        self.write_u8(0x03); // error correction: none
                             // Maximum capacity: if >2TB, set to 0x80000000 and use extended field.
        let max_cap_field = if max_capacity_kb > 0x7FFF_FFFF {
            0x8000_0000u32
        } else {
            max_capacity_kb as u32
        };
        self.write_u32(max_cap_field);
        self.write_u16(0xFFFE); // memory error info handle: not provided
        self.write_u16(num_devices);
        // Extended maximum capacity (SMBIOS 2.7+, in bytes)
        self.write_u64(max_capacity_kb * 1024);

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
        self.write_header(TYPE_MEMORY_DEVICE, 0x54); // 84 bytes (SMBIOS 3.3+)

        // Fixed fields
        self.write_u16(self.last_phys_mem_array_handle); // physical memory array handle
        self.write_u16(0xFFFE); // memory error info handle: not provided
        self.write_u16(64); // total width (bits) — assume 64-bit
        self.write_u16(64); // data width (bits)
                            // Size field: if size_mb fits in 15 bits, use directly.
                            // Otherwise set 0x7FFF and use extended size.
        if size_mb <= 0x7FFF {
            self.write_u16(size_mb as u16); // size in MB
        } else {
            self.write_u16(0x7FFF); // see extended size
        }
        self.write_u8(0x09); // form factor: DIMM
        self.write_u8(0); // device set: none
        self.write_u8(1); // device locator (string 1)
        self.write_u8(0); // bank locator (no string)
        self.write_u8(memory_type); // memory type
        self.write_u16(0); // type detail: unknown
        self.write_u16(speed_mhz); // speed (MT/s)
        self.write_u8(0); // manufacturer (no string)
        self.write_u8(0); // serial number (no string)
        self.write_u8(0); // asset tag (no string)
        self.write_u8(0); // part number (no string)
        self.write_u8(0); // attributes: unknown rank
                          // Extended size (SMBIOS 2.7+, in MB) — always written
        self.write_u32(size_mb);
        self.write_u16(speed_mhz); // configured memory clock speed
                                   // SMBIOS 2.8+ fields
        self.write_u16(0); // minimum voltage (unknown)
        self.write_u16(0); // maximum voltage (unknown)
        self.write_u16(0); // configured voltage (unknown)
                           // SMBIOS 3.2+ fields
        self.write_u8(0); // memory technology: unknown
        self.write_u16(0); // memory operating mode capability
        self.write_u8(0); // firmware version (no string)
        self.write_u16(0); // module manufacturer ID
        self.write_u16(0); // module product ID
        self.write_u16(0); // memory subsystem controller manufacturer ID
        self.write_u16(0); // memory subsystem controller product ID
        self.write_u64(0); // non-volatile size (0 = none)
        self.write_u64(size_mb as u64 * 1024 * 1024); // volatile size (bytes)
        self.write_u64(0); // cache size (0)
        self.write_u64(0); // logical size (0)
                           // SMBIOS 3.3+ fields
        self.write_u32(speed_mhz as u32); // extended speed (MT/s)
        self.write_u32(speed_mhz as u32); // extended configured memory speed

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
        self.write_header(TYPE_MEM_ARRAY_MAPPED_ADDR, 0x1F); // 31 bytes (SMBIOS 2.7+)

        // For addresses within 4 GiB, use KB-granularity fields.
        // For larger addresses, set 0xFFFFFFFF and use extended fields.
        let use_extended = end_addr > 0xFFFF_FFFF_u64 * 1024;
        if use_extended {
            self.write_u32(0xFFFF_FFFF); // starting address (see extended)
            self.write_u32(0xFFFF_FFFF); // ending address (see extended)
        } else {
            self.write_u32((start_addr / 1024) as u32); // starting address in KB
            self.write_u32((end_addr / 1024) as u32); // ending address in KB
        }
        self.write_u16(self.last_phys_mem_array_handle); // memory array handle
        self.write_u8(partition_width);
        // Extended addresses (SMBIOS 2.7+, in bytes)
        self.write_u64(start_addr);
        self.write_u64(end_addr);

        // No strings
        self.end_strings();
    }

    // -----------------------------------------------------------------------
    // Type 32: System Boot Information
    // -----------------------------------------------------------------------

    /// Add a Type 32 (System Boot Information) structure.
    pub fn add_system_boot_info(&mut self) {
        self.write_header(TYPE_SYSTEM_BOOT, 0x0B); // 11 bytes

        // Fixed fields
        self.write_zeros(6); // reserved
        self.write_u8(0); // boot status: no errors detected

        // No strings
        self.end_strings();
    }

    // -----------------------------------------------------------------------
    // Type 127: End-of-Table
    // -----------------------------------------------------------------------

    /// Add a Type 127 (End-of-Table) structure.
    pub fn add_end_of_table(&mut self) {
        self.write_header(TYPE_END_OF_TABLE, 4); // header only

        // String area: double null
        self.write_u8(0);
        self.write_u8(0);
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
    let ep_ptr = table_addr as *mut u8;
    let mut ep = [0u8; ENTRY_POINT_SIZE];
    // signature: _SM3_
    ep[0..5].copy_from_slice(&SM3_MAGIC);
    // checksum: computed after filling all fields
    // ep[5] = checksum (filled below)
    // length
    ep[6] = ENTRY_POINT_SIZE as u8;
    // major version
    ep[7] = 3;
    // minor version
    ep[8] = 0;
    // docrev
    ep[9] = 0;
    // entry point revision (0x01 for SMBIOS 3.0)
    ep[10] = 0x01;
    // reserved
    ep[11] = 0;
    // maximum structure table size (little-endian u32)
    let max_size = table_size as u32;
    ep[12] = max_size as u8;
    ep[13] = (max_size >> 8) as u8;
    ep[14] = (max_size >> 16) as u8;
    ep[15] = (max_size >> 24) as u8;
    // structure table address (little-endian u64)
    let addr_bytes = table_base.to_le_bytes();
    ep[16..24].copy_from_slice(&addr_bytes);
    // compute checksum
    ep[5] = compute_checksum(&ep);

    // Write entry point to memory.
    // SAFETY: caller guarantees writable memory at table_addr.
    unsafe {
        for (i, &b) in ep.iter().enumerate() {
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
            w.add_processor("CPU0", "ARM", 0x0119, 2000, 4, 4);
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
            w.add_processor("CPU0", "Intel", 0x28, 3600, 8, 16);
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
            w.add_processor("CPU0", "ARM", 0x0119, 2000, 1, 1);
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
    fn test_overflow_detection() {
        // Allocate a real buffer but limit the writer to 8 bytes.
        let mut buf = vec![0u8; 128];
        let base = buf.as_mut_ptr() as u64;
        let mut writer = unsafe { SmbiosWriter::with_limit(base, 8) };

        // Write exactly 8 bytes: should succeed.
        for _ in 0..8 {
            writer.write_u8(0xAA);
        }
        assert!(!writer.has_overflow(), "should not overflow at limit");

        // Write one more: should trigger overflow.
        writer.write_u8(0xBB);
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
                w.write_header(TYPE_BIOS_INFO, 0x1A); // 4 bytes → exactly at limit
                w.write_u8(0xFF); // overflow
            });
        }));
        assert!(
            result.is_err(),
            "assemble_and_write should panic on overflow"
        );
    }
}
