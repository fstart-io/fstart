//! QEMU fw_cfg access used by the QEMU platform flow.

use core::convert::TryInto;

use fstart_core::services::ServiceError;
use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use serde::{Deserialize, Serialize};

const FW_CFG_SIGNATURE: u16 = 0x0000;
const FW_CFG_ID: u16 = 0x0001;
const FW_CFG_FILE_DIR: u16 = 0x0019;

const COMMAND_ALLOCATE: u32 = 1;
const COMMAND_ADD_POINTER: u32 = 2;
const COMMAND_ADD_CHECKSUM: u32 = 3;

/// Stateless fw_cfg transport selected at compile time.
pub trait FwCfgTransport: Copy {
    fn select(self, selector: u16);
    fn read_byte(self) -> u8;
}

/// q35's legacy port-I/O fw_cfg window.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QemuFwCfgIoConfig {
    pub ctl_port: u16,
    pub data_port: u16,
}

impl QemuFwCfgIoConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ctl_port: 0x0510,
            data_port: 0x0511,
        }
    }
}

impl Default for QemuFwCfgIoConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Compatibility name for the q35 config. New virt flows use
/// [`QemuFwCfgMmioConfig`] explicitly.
pub type QemuFwCfgConfig = QemuFwCfgIoConfig;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl FwCfgTransport for QemuFwCfgIoConfig {
    fn select(self, selector: u16) {
        // SAFETY: ports originate in the q35 platform config.
        unsafe { fstart_core::pio::outw(self.ctl_port, selector) };
    }

    fn read_byte(self) -> u8 {
        // SAFETY: port originates in the q35 platform config.
        unsafe { fstart_core::pio::inb(self.data_port) }
    }
}

/// QEMU virt's MMIO fw_cfg window.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QemuFwCfgMmioConfig {
    pub base: u64,
}

impl QemuFwCfgMmioConfig {
    #[must_use]
    pub const fn new(base: u64) -> Self {
        Self { base }
    }
}

impl FwCfgTransport for QemuFwCfgMmioConfig {
    fn select(self, selector: u16) {
        // SAFETY: the board-validated base is QEMU's fw_cfg selector register.
        unsafe { fstart_core::mmio::write16(self.base as *mut u16, selector) };
    }

    fn read_byte(self) -> u8 {
        // SAFETY: the board-validated base + 8 is QEMU's fw_cfg data register.
        unsafe { fstart_core::mmio::read8((self.base + 8) as *const u8) }
    }
}

/// Typed QEMU fw_cfg client. The transport is static: q35 uses port I/O and
/// virt uses MMIO without pulling x86 PIO into ARM/RISC-V builds.
pub struct QemuFwCfg<T = QemuFwCfgIoConfig> {
    transport: T,
}

impl<T: FwCfgTransport> QemuFwCfg<T> {
    #[must_use]
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    fn select(&self, selector: u16) {
        self.transport.select(selector);
    }

    fn read_bytes(&self, buf: &mut [u8]) {
        for byte in buf {
            *byte = self.transport.read_byte();
        }
    }

    fn read_be16(&self) -> u16 {
        let mut buf = [0; 2];
        self.read_bytes(&mut buf);
        u16::from_be_bytes(buf)
    }

    fn read_be32(&self) -> u32 {
        let mut buf = [0; 4];
        self.read_bytes(&mut buf);
        u32::from_be_bytes(buf)
    }

    fn check_signature(&self) -> bool {
        self.select(FW_CFG_SIGNATURE);
        let mut sig = [0; 4];
        self.read_bytes(&mut sig);
        &sig == b"QEMU"
    }

    pub fn init(&mut self) -> Result<(), ServiceError> {
        if !self.check_signature() {
            return Err(ServiceError::HardwareError);
        }
        self.select(FW_CFG_ID);
        let mut id = [0; 4];
        self.read_bytes(&mut id);
        fstart_log::info!("fw_cfg: QEMU signature ok");
        Ok(())
    }

    fn find_file(&self, name: &str) -> Option<(u16, u32)> {
        self.select(FW_CFG_FILE_DIR);
        let count = self.read_be32();

        for _ in 0..count {
            let size = self.read_be32();
            let selector = self.read_be16();
            let _reserved = self.read_be16();
            let mut fname = [0u8; 56];
            self.read_bytes(&mut fname);
            let len = fname.iter().position(|&b| b == 0).unwrap_or(fname.len());
            if len == name.len() && &fname[..len] == name.as_bytes() {
                return Some((selector, size));
            }
        }
        None
    }

    fn read_file(&self, selector: u16, buf: &mut [u8]) {
        self.select(selector);
        self.read_bytes(buf);
    }

    pub fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        let (sel, size) = self
            .find_file("etc/e820")
            .ok_or(ServiceError::NotSupported)?;
        fstart_log::info!("fw_cfg: etc/e820 sel={} size={}", sel as u32, size);

        let entry_size = 20usize;
        let count = ((size as usize) / entry_size).min(entries.len());
        self.select(sel);
        for (idx, entry) in entries.iter_mut().take(count).enumerate() {
            let mut buf = [0u8; 20];
            self.read_bytes(&mut buf);
            entry.addr = u64::from_le_bytes(buf[0..8].try_into().unwrap_or([0; 8]));
            entry.size = u64::from_le_bytes(buf[8..16].try_into().unwrap_or([0; 8]));
            entry.kind = u32::from_le_bytes(buf[16..20].try_into().unwrap_or([0; 4]));
            let addr = entry.addr;
            let size = entry.size;
            let kind = entry.kind;
            fstart_log::info!(
                "  e820[{}]: addr={:#x} size={:#x} type={}",
                idx as u32,
                addr,
                size,
                kind,
            );
        }
        publish_mtrr_wb_ranges(&entries[..count]);
        Ok(count)
    }

    pub fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        let (sel, size) = self
            .find_file("etc/e820")
            .ok_or(ServiceError::NotSupported)?;
        let entry_size = 20usize;
        let count = (size as usize) / entry_size;
        let mut total = 0u64;
        self.select(sel);
        for _ in 0..count {
            let mut buf = [0u8; 20];
            self.read_bytes(&mut buf);
            let region_size = u64::from_le_bytes(buf[8..16].try_into().unwrap_or([0; 8]));
            let kind = u32::from_le_bytes(buf[16..20].try_into().unwrap_or([0; 4]));
            if kind == E820Kind::Ram as u32 {
                total = total.saturating_add(region_size);
            }
        }
        Ok(total)
    }

    pub fn load_acpi_tables(&self, buffer: &mut [u8]) -> Result<u64, ServiceError> {
        fstart_log::info!("fw_cfg: looking for etc/table-loader...");
        let (loader_sel, loader_size) = self
            .find_file("etc/table-loader")
            .ok_or(ServiceError::NotSupported)?;
        fstart_log::info!(
            "fw_cfg: table-loader found (sel={}, size={})",
            loader_sel as u32,
            loader_size,
        );

        const MAX_CMDS: usize = 64;
        static mut LOADER_BUF: [u8; MAX_CMDS * 128] = [0; MAX_CMDS * 128];
        // SAFETY: single-threaded firmware init.
        let loader_buf = unsafe { &mut *core::ptr::addr_of_mut!(LOADER_BUF) };
        let cmd_count = (loader_size as usize) / 128;
        if cmd_count > MAX_CMDS || loader_size as usize > loader_buf.len() {
            return Err(ServiceError::InvalidParam);
        }
        self.read_file(loader_sel, &mut loader_buf[..loader_size as usize]);
        fstart_log::info!("fw_cfg: {} table-loader commands", cmd_count as u32);

        static mut ALLOCS: [Option<AllocEntry>; 32] = [const { None }; 32];
        // SAFETY: single-threaded firmware init.
        let allocs = unsafe { &mut *core::ptr::addr_of_mut!(ALLOCS) };
        allocs.fill(None);
        let mut alloc_count = 0usize;
        let mut cursor = 0usize;

        for cmd_idx in 0..cmd_count {
            let base = cmd_idx * 128;
            let command = u32::from_le_bytes(
                loader_buf[base..base + 4]
                    .try_into()
                    .map_err(|_| ServiceError::InvalidParam)?,
            );
            match command {
                COMMAND_ALLOCATE => {
                    let mut name = [0u8; 56];
                    name.copy_from_slice(&loader_buf[base + 4..base + 60]);
                    let align = u32::from_le_bytes(
                        loader_buf[base + 60..base + 64]
                            .try_into()
                            .map_err(|_| ServiceError::InvalidParam)?,
                    ) as usize;
                    let name_len = name.iter().position(|&b| b == 0).unwrap_or(56);
                    let name_str = core::str::from_utf8(&name[..name_len]).unwrap_or("?");
                    fstart_log::info!("fw_cfg: ALLOCATE '{}'", name_str);
                    let (file_sel, file_size) =
                        self.find_file(name_str).ok_or(ServiceError::IoError)?;
                    let align = align.max(1);
                    cursor = (cursor + align - 1) & !(align - 1);
                    let file_size = file_size as usize;
                    if cursor + file_size > buffer.len() || alloc_count >= allocs.len() {
                        return Err(ServiceError::InvalidParam);
                    }
                    self.read_file(file_sel, &mut buffer[cursor..cursor + file_size]);
                    allocs[alloc_count] = Some(AllocEntry {
                        name,
                        offset: cursor,
                        size: file_size,
                    });
                    alloc_count += 1;
                    cursor += file_size;
                }
                COMMAND_ADD_POINTER => {
                    let dest_name = &loader_buf[base + 4..base + 60];
                    let src_name = &loader_buf[base + 60..base + 116];
                    let ptr_offset = u32::from_le_bytes(
                        loader_buf[base + 116..base + 120]
                            .try_into()
                            .map_err(|_| ServiceError::InvalidParam)?,
                    ) as usize;
                    let ptr_size = loader_buf[base + 120];
                    let dest_off = find_alloc(allocs, dest_name).ok_or(ServiceError::IoError)?;
                    let src_off = find_alloc(allocs, src_name).ok_or(ServiceError::IoError)?;
                    let patch_off = dest_off + ptr_offset;
                    let src_phys = buffer.as_ptr() as u64 + src_off as u64;
                    match ptr_size {
                        4 => {
                            let mut val = u32::from_le_bytes(
                                buffer[patch_off..patch_off + 4]
                                    .try_into()
                                    .map_err(|_| ServiceError::InvalidParam)?,
                            );
                            val = val.wrapping_add(src_phys as u32);
                            buffer[patch_off..patch_off + 4].copy_from_slice(&val.to_le_bytes());
                        }
                        8 => {
                            let mut val = u64::from_le_bytes(
                                buffer[patch_off..patch_off + 8]
                                    .try_into()
                                    .map_err(|_| ServiceError::InvalidParam)?,
                            );
                            val = val.wrapping_add(src_phys);
                            buffer[patch_off..patch_off + 8].copy_from_slice(&val.to_le_bytes());
                        }
                        _ => {}
                    }
                }
                COMMAND_ADD_CHECKSUM => {
                    let name = &loader_buf[base + 4..base + 60];
                    let checksum_offset = u32::from_le_bytes(
                        loader_buf[base + 60..base + 64]
                            .try_into()
                            .map_err(|_| ServiceError::InvalidParam)?,
                    ) as usize;
                    let start = u32::from_le_bytes(
                        loader_buf[base + 64..base + 68]
                            .try_into()
                            .map_err(|_| ServiceError::InvalidParam)?,
                    ) as usize;
                    let length = u32::from_le_bytes(
                        loader_buf[base + 68..base + 72]
                            .try_into()
                            .map_err(|_| ServiceError::InvalidParam)?,
                    ) as usize;
                    let off = find_alloc(allocs, name).ok_or(ServiceError::IoError)?;
                    buffer[off + checksum_offset] = 0;
                    let sum = buffer[off + start..off + start + length]
                        .iter()
                        .fold(0u8, |acc, &b| acc.wrapping_add(b));
                    buffer[off + checksum_offset] = 0u8.wrapping_sub(sum);
                }
                _ => {}
            }
        }

        patch_q35_fadt_pm_timer(buffer, allocs);
        log_loaded_acpi_tables(buffer, allocs);
        let rsdp_off = find_alloc_by_name(allocs, b"etc/acpi/rsdp").ok_or(ServiceError::IoError)?;
        Ok(buffer.as_ptr() as u64 + rsdp_off as u64)
    }
}

fn publish_mtrr_wb_ranges(entries: &[E820Entry]) {
    let mut ranges = [(0u64, 0u64); 8];
    let mut count = 0usize;
    for entry in entries {
        let kind = entry.kind;
        let size = entry.size;
        if kind == E820Kind::Ram as u32 && size != 0 && count < ranges.len() {
            ranges[count] = (entry.addr, size);
            count += 1;
        }
    }
    #[cfg(all(target_arch = "x86_64", feature = "x86_64"))]
    fstart_arch::x86::mtrr::set_ram_wb_ranges(&ranges[..count]);
}

#[derive(Clone)]
struct AllocEntry {
    name: [u8; 56],
    offset: usize,
    size: usize,
}

fn find_alloc(allocs: &[Option<AllocEntry>; 32], name: &[u8]) -> Option<usize> {
    let name_len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    allocs.iter().flatten().find_map(|entry| {
        let entry_len = entry.name.iter().position(|&b| b == 0).unwrap_or(56);
        (entry_len == name_len && entry.name[..entry_len] == name[..name_len])
            .then_some(entry.offset)
    })
}

fn find_alloc_entry_by_name<'a>(
    allocs: &'a [Option<AllocEntry>; 32],
    name: &[u8],
) -> Option<&'a AllocEntry> {
    allocs.iter().flatten().find(|entry| {
        let entry_len = entry.name.iter().position(|&b| b == 0).unwrap_or(56);
        entry_len == name.len() && &entry.name[..entry_len] == name
    })
}

fn find_alloc_by_name(allocs: &[Option<AllocEntry>; 32], name: &[u8]) -> Option<usize> {
    find_alloc_entry_by_name(allocs, name).map(|entry| entry.offset)
}

fn patch_q35_fadt_pm_timer(buffer: &mut [u8], allocs: &[Option<AllocEntry>; 32]) {
    let Some(tables) = find_alloc_entry_by_name(allocs, b"etc/acpi/tables") else {
        return;
    };
    let tables_off = tables.offset;
    let tables_end = tables_off.saturating_add(tables.size).min(buffer.len());
    let mut cursor = tables_off;
    while cursor + 36 <= tables_end {
        if &buffer[cursor..cursor + 4] != b"FACP" {
            cursor += 1;
            continue;
        }
        let Ok(len_bytes) = buffer[cursor + 4..cursor + 8].try_into() else {
            return;
        };
        let len = u32::from_le_bytes(len_bytes) as usize;
        if len < 220 || cursor + len > tables_end {
            cursor += 1;
            continue;
        }
        const PM_TMR_BLK: usize = 76;
        const PM_TMR_LEN: usize = 91;
        const X_PM_TMR_BLK: usize = 208;
        buffer[cursor + PM_TMR_BLK..cursor + PM_TMR_BLK + 4]
            .copy_from_slice(&0x608u32.to_le_bytes());
        buffer[cursor + PM_TMR_LEN] = 4;
        buffer[cursor + X_PM_TMR_BLK] = 1;
        buffer[cursor + X_PM_TMR_BLK + 1] = 32;
        buffer[cursor + X_PM_TMR_BLK + 2] = 0;
        buffer[cursor + X_PM_TMR_BLK + 3] = 0;
        buffer[cursor + X_PM_TMR_BLK + 4..cursor + X_PM_TMR_BLK + 12]
            .copy_from_slice(&0x608u64.to_le_bytes());
        buffer[cursor + 9] = 0;
        let sum = buffer[cursor..cursor + len]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        buffer[cursor + 9] = 0u8.wrapping_sub(sum);
        fstart_log::info!("fw_cfg: patched Q35 FADT PM timer to 0x608");
        return;
    }
}

fn log_loaded_acpi_tables(buffer: &[u8], allocs: &[Option<AllocEntry>; 32]) {
    for entry in allocs.iter().flatten() {
        let name_len = entry.name.iter().position(|&b| b == 0).unwrap_or(56);
        let name = core::str::from_utf8(&entry.name[..name_len]).unwrap_or("?");
        let off = entry.offset;
        if off + 8 > buffer.len() {
            continue;
        }
        if &buffer[off..off + 8] == b"RSD PTR " {
            fstart_log::info!("fw_cfg: ACPI {} RSDP", name);
        } else {
            let sig = core::str::from_utf8(&buffer[off..off + 4]).unwrap_or("????");
            let Ok(len_bytes) = buffer[off + 4..off + 8].try_into() else {
                continue;
            };
            let len = u32::from_le_bytes(len_bytes) as usize;
            fstart_log::info!("fw_cfg: ACPI {} sig={} len={}", name, sig, len as u32);
        }
    }
}
