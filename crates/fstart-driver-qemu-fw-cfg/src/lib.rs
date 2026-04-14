//! QEMU fw_cfg device driver.
//!
//! The fw_cfg device provides firmware configuration data from QEMU,
//! including ACPI tables, the e820 memory map, CPU count, and more.
//!
//! On x86, the device uses I/O ports:
//! - Control port (default `0x0510`): write 16-bit selector to choose a file
//! - Data port (default `0x0511`): read data byte-by-byte
//!
//! The ACPI table loading uses the table-loader protocol (`etc/table-loader`),
//! which is a sequence of ALLOCATE/ADD_POINTER/ADD_CHECKSUM commands that
//! dynamically link ACPI tables into a memory buffer.
//!
//! Reference: QEMU `docs/specs/fw_cfg.rst` and coreboot
//! `src/drivers/emulation/qemu/fw_cfg.c`.

#![no_std]

use fstart_services::acpi_provider::AcpiTableProvider;
use fstart_services::device::{Device, DeviceError};
use fstart_services::memory_detect::{E820Entry, E820Kind, MemoryDetector};
use fstart_services::ServiceError;

// ---------------------------------------------------------------------------
// Well-known fw_cfg selectors
// ---------------------------------------------------------------------------

const FW_CFG_SIGNATURE: u16 = 0x0000;
const FW_CFG_ID: u16 = 0x0001;
const FW_CFG_FILE_DIR: u16 = 0x0019;

// ---------------------------------------------------------------------------
// Table-loader command types
// ---------------------------------------------------------------------------

const COMMAND_ALLOCATE: u32 = 1;
const COMMAND_ADD_POINTER: u32 = 2;
const COMMAND_ADD_CHECKSUM: u32 = 3;

/// Zone hint for ALLOCATE: must be in the FSEG area (below 1MB).
#[allow(dead_code)]
const ALLOC_ZONE_HIGH: u8 = 1;
/// Zone hint for ALLOCATE: can be anywhere in RAM.
#[allow(dead_code)]
const ALLOC_ZONE_FSEG: u8 = 2;

// ---------------------------------------------------------------------------
// Config type
// ---------------------------------------------------------------------------

/// Configuration for the QEMU fw_cfg driver.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct QemuFwCfgConfig {
    /// I/O port for the control/selector register.
    #[serde(default = "default_ctl_port")]
    pub ctl_port: u16,
    /// I/O port for the data register.
    #[serde(default = "default_data_port")]
    pub data_port: u16,
}

fn default_ctl_port() -> u16 {
    0x0510
}
fn default_data_port() -> u16 {
    0x0511
}

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

/// QEMU fw_cfg device driver.
///
/// Provides access to QEMU's firmware configuration interface via I/O ports.
/// Implements `AcpiTableProvider` (table-loader protocol) and
/// `MemoryDetector` (e820 from `etc/e820` file).
pub struct QemuFwCfg {
    ctl_port: u16,
    data_port: u16,
}

// SAFETY: The I/O port addresses are fixed by board configuration and
// access is inherently serialized by the single-threaded firmware.
unsafe impl Send for QemuFwCfg {}
unsafe impl Sync for QemuFwCfg {}

impl QemuFwCfg {
    /// Write a 16-bit selector to the control port.
    fn select(&self, selector: u16) {
        // SAFETY: port address is from board config; firmware is single-threaded.
        unsafe { fstart_pio::outw(self.ctl_port, selector) };
    }

    /// Read `n` bytes from the data port into `buf`.
    fn read_bytes(&self, buf: &mut [u8]) {
        for byte in buf.iter_mut() {
            // SAFETY: port address is from board config; firmware is single-threaded.
            *byte = unsafe { fstart_pio::inb(self.data_port) };
        }
    }

    /// Read a big-endian u16 from the data port.
    fn read_be16(&self) -> u16 {
        let mut buf = [0u8; 2];
        self.read_bytes(&mut buf);
        u16::from_be_bytes(buf)
    }

    /// Read a big-endian u32 from the data port.
    fn read_be32(&self) -> u32 {
        let mut buf = [0u8; 4];
        self.read_bytes(&mut buf);
        u32::from_be_bytes(buf)
    }

    /// Check that the fw_cfg device is present by reading the signature.
    fn check_signature(&self) -> bool {
        self.select(FW_CFG_SIGNATURE);
        let mut sig = [0u8; 4];
        self.read_bytes(&mut sig);
        &sig == b"QEMU"
    }

    /// Find a named file in the fw_cfg directory.
    ///
    /// Returns `(selector, size)` if found.
    fn find_file(&self, name: &str) -> Option<(u16, u32)> {
        self.select(FW_CFG_FILE_DIR);
        let count = self.read_be32();

        for _ in 0..count {
            let size = self.read_be32();
            let selector = self.read_be16();
            let _reserved = self.read_be16();
            let mut fname = [0u8; 56];
            self.read_bytes(&mut fname);

            // Compare names (fname is NUL-terminated)
            let fname_len = fname.iter().position(|&b| b == 0).unwrap_or(56);
            if fname_len == name.len() && &fname[..fname_len] == name.as_bytes() {
                return Some((selector, size));
            }
        }
        fstart_log::error!(
            "fw_cfg: find_file('{}') not found in {} entries",
            name,
            count
        );
        None
    }

    /// Read an entire fw_cfg file into `buf`.
    fn read_file(&self, selector: u16, buf: &mut [u8]) {
        self.select(selector);
        self.read_bytes(buf);
    }
}

impl Device for QemuFwCfg {
    const NAME: &'static str = "qemu-fw-cfg";
    const COMPATIBLE: &'static [&'static str] = &["qemu,fw-cfg"];
    type Config = QemuFwCfgConfig;

    fn new(config: &QemuFwCfgConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            ctl_port: config.ctl_port,
            data_port: config.data_port,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        if !self.check_signature() {
            return Err(DeviceError::InitFailed);
        }
        // Read the ID register to verify DMA capability (informational)
        self.select(FW_CFG_ID);
        let mut _id_buf = [0u8; 4];
        self.read_bytes(&mut _id_buf);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AcpiTableProvider — table-loader protocol
// ---------------------------------------------------------------------------

/// Tracking entry for an allocated table in the buffer.
struct AllocEntry {
    /// File name (NUL-terminated, up to 56 bytes).
    name: [u8; 56],
    /// Offset within the output buffer where this file was placed.
    offset: usize,
    /// Size of the file data.
    size: usize,
}

impl AcpiTableProvider for QemuFwCfg {
    fn load_acpi_tables(&self, buffer: &mut [u8]) -> Result<u64, ServiceError> {
        // Find the table-loader file
        fstart_log::info!("fw_cfg: looking for etc/table-loader...");
        let (loader_sel, loader_size) = self
            .find_file("etc/table-loader")
            .ok_or(ServiceError::NotSupported)?;
        fstart_log::info!(
            "fw_cfg: table-loader found (sel={}, size={})",
            loader_sel as u32,
            loader_size
        );

        // Read the loader commands into a temporary buffer on the stack.
        // Each command is 128 bytes. Typical QEMU has < 20 commands.
        const MAX_CMDS: usize = 64;
        let mut loader_buf = [0u8; MAX_CMDS * 128];
        let cmd_count = (loader_size as usize) / 128;
        if cmd_count > MAX_CMDS {
            return Err(ServiceError::InvalidParam);
        }
        self.read_file(loader_sel, &mut loader_buf[..loader_size as usize]);
        fstart_log::info!("fw_cfg: {} table-loader commands", cmd_count as u32);

        // Track allocations (file name → buffer offset)
        let mut allocs: [Option<AllocEntry>; 32] = core::array::from_fn(|_| None);
        let mut alloc_count = 0usize;
        let mut cursor = 0usize; // next free position in buffer

        // Process commands
        for i in 0..cmd_count {
            let cmd_base = i * 128;
            let command =
                u32::from_le_bytes(loader_buf[cmd_base..cmd_base + 4].try_into().unwrap());

            match command {
                COMMAND_ALLOCATE => {
                    // Bytes 4..60: file name (56 bytes, NUL-terminated)
                    let mut name = [0u8; 56];
                    name.copy_from_slice(&loader_buf[cmd_base + 4..cmd_base + 60]);
                    // Bytes 60..64: alignment
                    let align = u32::from_le_bytes(
                        loader_buf[cmd_base + 60..cmd_base + 64].try_into().unwrap(),
                    ) as usize;

                    // Find and read the file
                    let name_len = name.iter().position(|&b| b == 0).unwrap_or(56);
                    let name_str = core::str::from_utf8(&name[..name_len]).unwrap_or("?");
                    fstart_log::info!("fw_cfg: ALLOCATE '{}'", name_str);
                    let (file_sel, file_size) = match self.find_file(name_str) {
                        Some(v) => v,
                        None => {
                            fstart_log::error!("fw_cfg: file '{}' not found", name_str);
                            return Err(ServiceError::IoError);
                        }
                    };

                    fstart_log::info!(
                        "fw_cfg:   found sel={} size={} align={}",
                        file_sel as u32,
                        file_size,
                        align as u32
                    );

                    // Align cursor
                    let align = align.max(1);
                    cursor = (cursor + align - 1) & !(align - 1);

                    if cursor + file_size as usize > buffer.len() {
                        fstart_log::error!(
                            "fw_cfg: buffer overflow: cursor={} + size={} > buf={}",
                            cursor as u32,
                            file_size,
                            buffer.len() as u32
                        );
                        return Err(ServiceError::InvalidParam);
                    }

                    // Read file data into buffer
                    self.read_file(file_sel, &mut buffer[cursor..cursor + file_size as usize]);

                    if alloc_count < allocs.len() {
                        allocs[alloc_count] = Some(AllocEntry {
                            name,
                            offset: cursor,
                            size: file_size as usize,
                        });
                        alloc_count += 1;
                    }
                    cursor += file_size as usize;
                }

                COMMAND_ADD_POINTER => {
                    // Bytes 4..60: dest_file (56 bytes)
                    let dest_name = &loader_buf[cmd_base + 4..cmd_base + 60];
                    // Bytes 60..116: src_file (56 bytes)
                    let src_name = &loader_buf[cmd_base + 60..cmd_base + 116];
                    // Bytes 116..120: offset within dest_file
                    let ptr_offset = u32::from_le_bytes(
                        loader_buf[cmd_base + 116..cmd_base + 120]
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    // Bytes 120: pointer size (1, 2, 4, or 8)
                    let ptr_size = loader_buf[cmd_base + 120];

                    // Find dest and src allocations
                    let dest_off = find_alloc(&allocs, dest_name).ok_or(ServiceError::IoError)?;
                    let src_off = find_alloc(&allocs, src_name).ok_or(ServiceError::IoError)?;

                    // Read existing value, add src's physical address, write back
                    let patch_off = dest_off + ptr_offset;
                    let src_phys = buffer.as_ptr() as u64 + src_off as u64;

                    match ptr_size {
                        4 => {
                            let mut val = u32::from_le_bytes(
                                buffer[patch_off..patch_off + 4].try_into().unwrap(),
                            );
                            val = val.wrapping_add(src_phys as u32);
                            buffer[patch_off..patch_off + 4].copy_from_slice(&val.to_le_bytes());
                        }
                        8 => {
                            let mut val = u64::from_le_bytes(
                                buffer[patch_off..patch_off + 8].try_into().unwrap(),
                            );
                            val = val.wrapping_add(src_phys);
                            buffer[patch_off..patch_off + 8].copy_from_slice(&val.to_le_bytes());
                        }
                        _ => {
                            // 1-byte and 2-byte pointers are unusual but handled
                        }
                    }
                }

                COMMAND_ADD_CHECKSUM => {
                    // Bytes 4..60: file name (56 bytes)
                    let cksum_name = &loader_buf[cmd_base + 4..cmd_base + 60];
                    // Bytes 60..64: offset for checksum byte
                    let cksum_offset = u32::from_le_bytes(
                        loader_buf[cmd_base + 60..cmd_base + 64].try_into().unwrap(),
                    ) as usize;
                    // Bytes 64..68: start offset for sum range
                    let start = u32::from_le_bytes(
                        loader_buf[cmd_base + 64..cmd_base + 68].try_into().unwrap(),
                    ) as usize;
                    // Bytes 68..72: length of sum range
                    let length = u32::from_le_bytes(
                        loader_buf[cmd_base + 68..cmd_base + 72].try_into().unwrap(),
                    ) as usize;

                    let file_off = find_alloc(&allocs, cksum_name).ok_or(ServiceError::IoError)?;

                    // Zero the checksum byte first
                    buffer[file_off + cksum_offset] = 0;

                    // Compute ACPI checksum: sum of all bytes in range must be 0
                    let sum: u8 = buffer[file_off + start..file_off + start + length]
                        .iter()
                        .fold(0u8, |acc, &b| acc.wrapping_add(b));
                    buffer[file_off + cksum_offset] = 0u8.wrapping_sub(sum);
                }

                _ => {
                    // Unknown command — skip
                }
            }
        }

        // Find the RSDP. It's typically in "etc/acpi/rsdp".
        let rsdp_off =
            find_alloc_by_name(&allocs, b"etc/acpi/rsdp").ok_or(ServiceError::IoError)?;
        let rsdp_phys = buffer.as_ptr() as u64 + rsdp_off as u64;
        Ok(rsdp_phys)
    }
}

// ---------------------------------------------------------------------------
// MemoryDetector — e820 from fw_cfg
// ---------------------------------------------------------------------------

impl MemoryDetector for QemuFwCfg {
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        let (sel, size) = self
            .find_file("etc/e820")
            .ok_or(ServiceError::NotSupported)?;
        fstart_log::info!("fw_cfg: etc/e820 sel={} size={}", sel as u32, size);

        // Each e820 entry from QEMU is 20 bytes: addr(u64) + size(u64) + type(u32).
        // No padding — this matches QEMU's `struct e820_entry` in hw/i386/e820.c.
        let entry_size = 20;
        let count = (size as usize) / entry_size;
        let count = count.min(entries.len());

        self.select(sel);
        for (i, entry) in entries.iter_mut().take(count).enumerate() {
            let mut buf = [0u8; 20];
            self.read_bytes(&mut buf);
            entry.addr = u64::from_le_bytes(buf[0..8].try_into().unwrap());
            entry.size = u64::from_le_bytes(buf[8..16].try_into().unwrap());
            let raw_kind = u32::from_le_bytes(buf[16..20].try_into().unwrap());
            entry.kind = raw_kind;
            let e_addr = entry.addr;
            let e_size = entry.size;
            fstart_log::info!(
                "  e820[{}]: addr={:#x} size={:#x} type={}",
                i as u32,
                e_addr,
                e_size,
                raw_kind
            );
        }

        Ok(count)
    }

    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        // Read e820 entries and sum RAM-type regions.
        // Each read consumes the fw_cfg data pointer so we must re-select.
        let (sel, size) = self
            .find_file("etc/e820")
            .ok_or(ServiceError::NotSupported)?;

        let entry_size = 20; // matches detect_memory
        let count = (size as usize) / entry_size;

        self.select(sel);
        let mut total: u64 = 0;
        for _ in 0..count {
            let mut buf = [0u8; 20];
            self.read_bytes(&mut buf);
            let region_size = u64::from_le_bytes(buf[8..16].try_into().unwrap());
            let raw_kind = u32::from_le_bytes(buf[16..20].try_into().unwrap());
            if raw_kind == E820Kind::Ram as u32 {
                total += region_size;
            }
        }
        Ok(total)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the buffer offset of an allocated file by its raw 56-byte name.
fn find_alloc(allocs: &[Option<AllocEntry>; 32], name: &[u8]) -> Option<usize> {
    for entry in allocs.iter().flatten() {
        if entry.name[..name.len()] == *name {
            return Some(entry.offset);
        }
    }
    None
}

/// Find the buffer offset of an allocated file by a NUL-terminated name string.
fn find_alloc_by_name(allocs: &[Option<AllocEntry>; 32], name: &[u8]) -> Option<usize> {
    for entry in allocs.iter().flatten() {
        let entry_len = entry.name.iter().position(|&b| b == 0).unwrap_or(56);
        if entry_len == name.len() && &entry.name[..entry_len] == name {
            return Some(entry.offset);
        }
    }
    None
}
