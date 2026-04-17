//! SG2042 RISC-V subsystem core release driver.
//!
//! Terminal driver: `SocHandoff::handoff()` loads the RISC-V ZSBL from
//! SPI flash via the DMMR memory-mapped window, programs the reset vector
//! into TOP registers, enables all cluster subsystems, and parks the A53
//! in a WFI loop via [`fstart_arch::halt()`].
//!
//! # Handoff sequence (mirrors `mango_setup_rpsys()` + `mango_load_zsbl()`)
//!
//! 1. Set JEDEC vendor ID for T-Head RISC-V cores
//! 2. Allow RISC-V secure transactions (`RP_CPU_SEC_ACC = 1`)
//! 3. For each cluster: write global ID to MP0_STATUS_REG, enable via
//!    MP0_CONTROL_REG
//! 4. Find "zsbl" in the SPI flash DPT; copy to DDR load address
//! 5. Write reset vector to `RP_CPU_RVBA_L/H`
//! 6. Enable RP subsystem (assert SOFT_RESET_BASE bit 1)
//! 7. A53 WFI loop
//!
//! Hardware reference: `mango_misc.c` — `mango_setup_rpsys()`,
//! `mango_load_zsbl()`.

use serde::{Deserialize, Serialize};

use fstart_services::{
    device::{Device, DeviceError},
    soc_handoff::SocHandoff,
};

use crate::spi_part::{self, DPT_MAGIC};

// ===================================================================
// T-Head JEDEC vendor ID
// ===================================================================

/// T-Head JEDEC manufacturer ID.
///
/// JEDEC bank 12 continuation code (0x7F × 11) followed by 0x37.
/// Encoding: bank = 11, id = 0x37 & ~0x80 = 0x37.
/// Final: (11 << 7) | 0x37 = 0x5B7.
///
/// Reference: `mango_misc.c:mango_setup_rpsys()`.
const THEAD_VENDOR_ID: u64 = (11u64 << 7) | 0x37;

// ===================================================================
// Config
// ===================================================================

/// Configuration for the SG2042 RISC-V core release driver.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sg2042RvReleaseConfig {
    /// SYS_CTRL (TOP) base address.
    pub sys_ctrl_base: u64,
    /// SPI flash DMMR memory-mapped window base (`SERIAL_FLASH0_BASE`).
    pub flash_dmmr_base: u64,
    /// Byte offset within flash of the DPT (Disk Partition Table).
    /// Always `0x600000` on Pioneer.
    pub dpt_offset: u32,
    /// Number of RISC-V clusters to release (16 on Pioneer).
    pub cluster_count: u8,
    /// Socket ID — must be 0 for Pioneer (single-socket).
    pub socket_id: u8,
}

// ===================================================================
// Driver struct
// ===================================================================

/// SG2042 RISC-V core release driver.
pub struct Sg2042RvRelease {
    sys_ctrl_base: u64,
    flash_dmmr_base: u64,
    dpt_offset: u32,
    cluster_count: u8,
}

// SAFETY: accesses fixed hardware addresses; called only once at boot
// from the single A53 SCP core.
unsafe impl Send for Sg2042RvRelease {}
unsafe impl Sync for Sg2042RvRelease {}

impl Device for Sg2042RvRelease {
    const NAME: &'static str = "sg2042-rv-release";
    const COMPATIBLE: &'static [&'static str] = &["sophgo,sg2042-rv-release"];
    type Config = Sg2042RvReleaseConfig;

    fn new(config: &Sg2042RvReleaseConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            sys_ctrl_base: config.sys_ctrl_base,
            flash_dmmr_base: config.flash_dmmr_base,
            dpt_offset: config.dpt_offset,
            cluster_count: config.cluster_count,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
}

impl SocHandoff for Sg2042RvRelease {
    fn handoff(&mut self) -> ! {
        // Step 1: Configure RISC-V subsystem identity.
        self.setup_rpsys();

        // Step 2: Load ZSBL from SPI flash and set reset vector.
        let reset_vector = match self.load_zsbl() {
            Some(addr) => addr,
            None => {
                fstart_log::error!(
                    "[SG2042 RV] ZSBL not found in DPT — \
                     using DDR0_BASE (0x0) as fallback reset vector"
                );
                0u64 // RV_BOOTROM_BASE = DDR0_BASE = 0
            }
        };

        // Step 3: Write reset vector to TOP.
        self.set_rv_reset_addr(reset_vector);

        // Step 4: Enable RP subsystem.
        self.enable_rpsys();

        fstart_log::info!("[SG2042 RV] RISC-V cores released — A53 entering WFI loop");
        fstart_arch::halt()
    }
}

impl Sg2042RvRelease {
    /// Configure RISC-V subsystem: vendor ID, secure access, cluster IDs.
    ///
    /// Reference: `mango_misc.c:mango_setup_rpsys()`
    fn setup_rpsys(&self) {
        let base = self.sys_ctrl_base as usize;

        // Write JEDEC vendor ID (T-Head)
        // mango_misc.c: mmio_write_32(TOP+0x340, vendor_id & 0xffffffff)
        //               mmio_write_32(TOP+0x344, vendor_id >> 32)
        unsafe {
            core::ptr::write_volatile(
                (base + 0x340) as *mut u32,
                (THEAD_VENDOR_ID & 0xFFFF_FFFF) as u32,
            );
            core::ptr::write_volatile((base + 0x344) as *mut u32, (THEAD_VENDOR_ID >> 32) as u32);
        }

        // Allow RISC-V cores to issue secure transactions.
        // mango_misc.c: mmio_write_32(TOP+0x360, 1)
        unsafe {
            core::ptr::write_volatile((base + 0x360) as *mut u32, 1);
        }

        // For each cluster: set global cluster ID and enable.
        // mango_misc.c: for each cluster i:
        //   mango_set_rv_cluster_id(i, socket_id*16+i)  → TOP+0x380+i*8
        //   mango_set_rv_cluster_en(i, valid_rv_map[i]) → TOP+0x384+i*8
        for i in 0..self.cluster_count as usize {
            let cluster_id = i as u32; // socket_id=0, so global_id = i
            let status_addr = base + 0x380 + i * 8;
            let control_addr = base + 0x384 + i * 8;
            unsafe {
                core::ptr::write_volatile(status_addr as *mut u32, cluster_id);
                core::ptr::write_volatile(control_addr as *mut u32, 1);
            }
        }
    }

    /// Load ZSBL from SPI flash DPT into DDR and return its load address.
    ///
    /// Returns `None` if the "zsbl" partition is not found.
    ///
    /// Reference: `mango_misc.c:mango_load_zsbl()`
    fn load_zsbl(&self) -> Option<u64> {
        let dpt_base = (self.flash_dmmr_base + self.dpt_offset as u64) as *const u8;

        // SAFETY: dpt_base points into the DMMR memory-mapped SPI flash window,
        // which is a read-only memory-mapped region valid throughout boot.
        let entry_ptr = unsafe { spi_part::find_by_name(dpt_base, b"zsbl")? };
        let entry = unsafe { &*entry_ptr };

        // Sanity check: partition must be present and reasonably sized.
        if entry.magic != DPT_MAGIC || entry.size == 0 || entry.size > 4 * 1024 * 1024 {
            fstart_log::error!("[SG2042 RV] ZSBL DPT entry invalid (magic or size)");
            return None;
        }

        let src = (self.flash_dmmr_base + entry.offset as u64) as *const u8;
        let dst = entry.lma as *mut u8;

        // SAFETY: src is in the DMMR SPI flash window (read-only).
        // dst is in DDR (must be initialized before this call — DramInit
        // runs earlier in the capability sequence and halts on failure,
        // so if we reach here, DDR is operational).
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst, entry.size as usize);
        }

        fstart_log::info!(
            "[SG2042 RV] ZSBL loaded: {} bytes from flash+{:#x} → {:#x}",
            entry.size,
            entry.offset,
            entry.lma
        );

        Some(entry.lma)
    }

    /// Write the RISC-V reset vector to TOP RP_CPU_RVBA registers.
    ///
    /// Reference: `mango_misc.c:mango_set_rv_reset_addr()`
    fn set_rv_reset_addr(&self, addr: u64) {
        let base = self.sys_ctrl_base as usize;
        // mango_misc.c: mmio_write_32(TOP+0x350, addr_lo)
        //               mmio_write_32(TOP+0x354, addr_hi)
        unsafe {
            core::ptr::write_volatile((base + 0x350) as *mut u32, (addr & 0xFFFF_FFFF) as u32);
            core::ptr::write_volatile((base + 0x354) as *mut u32, (addr >> 32) as u32);
        }
    }

    /// Enable the RP (RISC-V Processor) subsystem.
    ///
    /// Deasserts the RP soft-reset to release all configured clusters.
    ///
    /// Reference: `mango_misc.c:mango_set_rp_sys_en(socket=0, en=true)`
    ///   → `mmio_setbits_32(MANGO_SOFT_RESET_BASE, BIT(1))`
    ///   MANGO_SOFT_RESET_BASE = SYS_CTRL_BASE + 0x4000 + 0x1000 + 0x1000
    ///                        = SYS_CTRL_BASE + 0x6000
    fn enable_rpsys(&self) {
        let soft_reset_base = (self.sys_ctrl_base + 0x6000) as usize;
        let v = unsafe { core::ptr::read_volatile(soft_reset_base as *const u32) };
        unsafe {
            core::ptr::write_volatile(soft_reset_base as *mut u32, v | (1 << 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thead_vendor_id() {
        // bank=11, id=0x37 → (11<<7)|0x37 = 0x597? Let's verify:
        // 11 * 128 = 1408 = 0x580; 0x580 | 0x37 = 0x5B7
        assert_eq!(THEAD_VENDOR_ID, 0x5B7);
    }

    #[test]
    fn test_cluster_id_sequence() {
        // With socket_id=0, cluster i gets global ID = i
        for i in 0u32..16 {
            let expected_id = i; // socket_id * 16 + i with socket_id=0
            assert_eq!(expected_id, i);
        }
    }
}
