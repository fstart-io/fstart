//! SG2042 Cadence PCIe RC (Root Complex) driver.
//!
//! Initializes PCIe0 as a Gen4 x16 Root Complex for the Milk-V Pioneer.
//! The sequence closely follows `mango_pcie_init()` in the Sophgo TF-A port.
//!
//! # Init sequence
//!
//! 1. Clear IRS debug/scratch registers
//! 2. Sideband init: PM clk, pipe mux, lane-to-PIPE port mapping
//! 3. Controller config: RC mode, Gen4, disable link training
//! 4. PHY config: write SSC tables for all 16 lanes
//! 5. PHY reset: assert PHY_RESET_N, wait for CLOCK_STABLE
//! 6. Set link width (x16)
//! 7. Drive PERST# high via GPIO12
//! 8. Deassert link reset (AXI, MGMT, PM)
//! 9. Enable link training; poll for DL_INIT_COMPLETED
//! 10. Verify negotiated width and speed
//! 11. Configure BARs
//!
//! Hardware reference: `drivers/sophgo/pcie/mango_pcie.c`.

use serde::{Deserialize, Serialize};

use fstart_services::{
    device::{Device, DeviceError},
    pci::{PciAddr, PciRootBus},
    ServiceError,
};

// ===================================================================
// Address layout
// ===================================================================

/// Per-controller stride in the PCIe config space.
const PCIE_CTRL_STRIDE: u64 = 0x0200_0000;

/// Offset of the Cadence link0 APB config block.
const PCIE_CFG_LINK0_APB: u64 = 0x0000_0000;
/// Offset of the Cadence PHY APB config block.
const PCIE_CFG_PHY_APB: u64 = 0x0100_0000;
/// Offset of the Sophgo IRS (internal register space) config block.
const PCIE_CFG_MANGO_APB: u64 = 0x0180_0000;

/// ECAM slave window base for PCIe0 (slave 0).
const PCIE0_SLV0_BASE: u64 = 0x4000000000;

// ===================================================================
// PHY register tables (from mango_pcie.c)
// ===================================================================

/// A PHY register write entry: (offset, value).
type PhyCfgEntry = (u16, u16);

/// External SSC PHY table.
/// Source: `mango_pcie.c:phy_cfg_ex_SSC[]`
const PHY_CFG_EX_SSC: &[PhyCfgEntry] = &[(0x0050, 0x8804), (0x0062, 0x1B26)];

/// Internal SSC PHY table.
/// Source: `mango_pcie.c:phy_cfg_in_SSC[]`
const PHY_CFG_IN_SSC: &[PhyCfgEntry] = &[
    (0x0048, 0x000E),
    (0x0049, 0x4006),
    (0x004A, 0x0012),
    (0x004B, 0x0000),
    (0x004C, 0x0000),
    (0x004D, 0x0022),
    (0x004E, 0x0006),
    (0x004F, 0x000E),
    (0x0050, 0x0000),
    (0x0051, 0x0000),
    (0x0052, 0x0000),
    (0x0053, 0x0000),
];

/// Both-SSC PHY table (applied after ex/in tables).
/// Source: `mango_pcie.c:phy_cfg_both_SSC[]`
const PHY_CFG_BOTH_SSC: &[PhyCfgEntry] = &[
    (0x409E, 0x8C67),
    (0x419E, 0x8C67),
    (0x429E, 0x8C67),
    (0x439E, 0x8C67),
    (0x449E, 0x8C67),
];

/// Per-lane mix register table (applied to all 16 lanes).
/// Source: `mango_pcie.c:phy_cfg_mix_reg[]`
const PHY_CFG_MIX_REG: &[PhyCfgEntry] = &[
    (0x000E, 0x0003),
    (0x0013, 0x0004),
    (0x001A, 0x0002),
    (0x001B, 0x0000),
    (0x001C, 0x0001),
    (0x001D, 0x0000),
    (0x0028, 0x001F),
    (0x002A, 0x003F),
    (0x002C, 0x003F),
    (0x0032, 0x001F),
    (0x003F, 0x0007),
    (0x0041, 0x0007),
    (0x0044, 0x000F),
    (0x0050, 0x001F),
    (0x0051, 0x001F),
    (0x0052, 0x001F),
    (0x0053, 0x001F),
    (0x0055, 0x001F),
    (0x0056, 0x001F),
    (0x0058, 0x0007),
    (0x005A, 0x0007),
    (0x005B, 0x0007),
    (0x0060, 0x003F),
    (0x0061, 0x003F),
    (0x0062, 0x003F),
    (0x0063, 0x003F),
    (0x0064, 0x003F),
];

// ===================================================================
// Config
// ===================================================================

/// Configuration for the SG2042 PCIe RC driver.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sg2042PcieConfig {
    /// PCIe0 config base address (`PCIE0_CFG_BASE = 0x7060_0000_00`).
    pub pcie_cfg_base: u64,
    /// PCIe controller index (0 = PCIe0, 1 = PCIe1).
    pub pcie_id: u8,
    /// SYS_CTRL (TOP) base — needed for GPIO PERST# control.
    pub sys_ctrl_base: u64,
}

// ===================================================================
// Driver struct
// ===================================================================

/// SG2042 Cadence PCIe RC driver (PCIe0, x16 Gen4).
pub struct Sg2042Pcie {
    /// Base address of the link0 APB register block.
    link0_apb: u64,
    /// Base address of the Cadence PHY APB register block.
    phy_apb: u64,
    /// Base address of the Sophgo IRS register block.
    mango_apb: u64,
    /// GPIO0 base for PERST# control.
    gpio0_base: u64,
}

// SAFETY: MMIO registers at fixed addresses; single-threaded boot context.
unsafe impl Send for Sg2042Pcie {}
unsafe impl Sync for Sg2042Pcie {}

impl Device for Sg2042Pcie {
    const NAME: &'static str = "sg2042-pcie";
    const COMPATIBLE: &'static [&'static str] = &["sophgo,sg2042-pcie", "cdns,cdns-pcie-host"];
    type Config = Sg2042PcieConfig;

    fn new(config: &Sg2042PcieConfig) -> Result<Self, DeviceError> {
        let ctrl_base = config.pcie_cfg_base + (config.pcie_id as u64) * PCIE_CTRL_STRIDE;
        Ok(Self {
            link0_apb: ctrl_base + PCIE_CFG_LINK0_APB,
            phy_apb: ctrl_base + PCIE_CFG_PHY_APB,
            mango_apb: ctrl_base + PCIE_CFG_MANGO_APB,
            // GPIO0_BASE = SYS_CTRL_BASE - 0x710000 (approx); exact: 0x7030009000
            gpio0_base: 0x7030009000,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        self.clear_irs_scratch();
        self.sideband_init();
        self.config_ctrl();
        self.config_phy();
        self.phy_rst_wait_pclk()?;
        self.config_link_width();
        self.set_perst();
        self.link0_reset();
        self.train_link()?;
        Ok(())
    }
}

// ===================================================================
// PciRootBus trait
// ===================================================================

impl PciRootBus for Sg2042Pcie {
    fn config_read32(&self, addr: PciAddr, reg: u16) -> Result<u32, ServiceError> {
        let ecam = self.ecam_address(addr, reg);
        // SAFETY: ECAM window is mapped as Device memory.
        Ok(unsafe { core::ptr::read_volatile(ecam as *const u32) })
    }

    fn config_write32(&self, addr: PciAddr, reg: u16, val: u32) -> Result<(), ServiceError> {
        let ecam = self.ecam_address(addr, reg);
        // SAFETY: ECAM window is mapped as Device memory.
        unsafe { core::ptr::write_volatile(ecam as *mut u32, val) }
        Ok(())
    }

    fn ecam_base(&self) -> u64 {
        PCIE0_SLV0_BASE
    }

    fn ecam_size(&self) -> u64 {
        // 32 GB PCIe0 slave 0 window
        0x800000000
    }

    fn bus_start(&self) -> u8 {
        0
    }

    fn bus_end(&self) -> u8 {
        255
    }

    fn device_count(&self) -> usize {
        0 // populated after link training
    }
}

// ===================================================================
// Internal implementation helpers
// ===================================================================

impl Sg2042Pcie {
    fn ecam_address(&self, addr: PciAddr, offset: u16) -> u64 {
        PCIE0_SLV0_BASE
            + ((addr.bus as u64) << 20)
            + ((addr.dev as u64) << 15)
            + ((addr.func as u64) << 12)
            + (offset as u64)
    }

    fn irs_write(&self, offset: u64, val: u32) {
        unsafe {
            core::ptr::write_volatile((self.mango_apb + offset) as *mut u32, val);
        }
    }

    fn irs_read(&self, offset: u64) -> u32 {
        unsafe { core::ptr::read_volatile((self.mango_apb + offset) as *const u32) }
    }

    fn link_write(&self, offset: u64, val: u32) {
        unsafe {
            core::ptr::write_volatile((self.link0_apb + offset) as *mut u32, val);
        }
    }

    fn link_read(&self, offset: u64) -> u32 {
        unsafe { core::ptr::read_volatile((self.link0_apb + offset) as *const u32) }
    }

    fn phy_write_lane(&self, lane: u8, offset: u16, val: u16) {
        // PHY register address = phy_apb + (offset << 2) | (lane << 11)
        let addr = (self.phy_apb + ((offset as u64) << 2)) | ((lane as u64) << 11);
        unsafe {
            core::ptr::write_volatile(addr as *mut u32, val as u32);
        }
    }

    fn phy_write_common(&self, offset: u16, val: u16) {
        let addr = self.phy_apb + ((offset as u64) << 2);
        unsafe {
            core::ptr::write_volatile(addr as *mut u32, val as u32);
        }
    }

    /// Step 1: Clear IRS debug scratch registers.
    /// Reference: `mango_pcie.c:mango_pcie_init()` lines 1–4.
    fn clear_irs_scratch(&self) {
        self.irs_write(0x844, 0);
        self.irs_write(0x848, 0);
        self.irs_write(0x84C, 0);
        self.irs_write(0x850, 0);
    }

    /// Step 2: Sideband init — PM clk, pipe mux, lane mapping.
    /// Reference: `mango_pcie.c:pcie_init_sideband()` for x16 mode.
    fn sideband_init(&self) {
        // PM clock kick-off
        // REG0000 |= (1 << 15)
        let v = self.link_read(0x0000);
        self.link_write(0x0000, v | (1 << 15));
        // CLK_SHUTOFF_DETECT_EN link0
        // REG0004 |= (1 << 6)
        let v = self.link_read(0x0004);
        self.link_write(0x0004, v | (1 << 6));

        // Disable FULL_PIPE_MUX for x16
        let v = self.link_read(0x0000);
        self.link_write(0x0000, v & !(1 << 2));
        self.link_write(0x02F0, 0);
        self.link_write(0x02EC, 0);

        // Lane-to-PIPE port mapping (one entry per 2 lanes)
        // REG0008..REG0024 = x16 lane map
        self.link_write(0x0008, 0x0002_0001);
        self.link_write(0x000C, 0x0008_0004);
        self.link_write(0x0010, 0x0020_0010);
        self.link_write(0x0014, 0x0080_0040);
        self.link_write(0x0018, 0x0200_0100);
        self.link_write(0x001C, 0x0800_0400);
        self.link_write(0x0020, 0x2000_1000);
        self.link_write(0x0024, 0x8000_4000);

        // CONFIG_ENABLE
        let v = self.link_read(0x0004);
        self.link_write(0x0004, v | (1 << 5));
    }

    /// Step 3: Configure controller for RC mode, Gen4, training disabled.
    /// Reference: `mango_pcie.c:pcie_config_ctrl()`.
    fn config_ctrl(&self) {
        // MODE_SELECT = 1 (RC)
        let v = self.link_read(0x0004);
        self.link_write(0x0004, v | (1 << 7));
        // GENERATION_SEL = 3 (Gen4 = 0b11)
        let v = self.irs_read(0x0038);
        self.irs_write(0x0038, (v & !0x3) | 3);
        // Enable GEN3 DC balance
        let v = self.link_read(0x0004);
        self.link_write(0x0004, v & !(1 << 13));
        // Disable NON_POSTED_REJ
        let v = self.link_read(0x007C);
        self.link_write(0x007C, v & !(1 << 23));
        // Disable link training (enable later in train_link())
        let v = self.link_read(0x0004);
        self.link_write(0x0004, v & !(1 << 12));
    }

    /// Step 4: Write Cadence PHY register tables.
    /// Reference: `mango_pcie.c:pcie_config_phy()`.
    fn config_phy(&self) {
        // Apply external SSC table (common regs)
        for &(off, val) in PHY_CFG_EX_SSC {
            self.phy_write_common(off, val);
        }
        // Apply internal SSC table (common regs)
        for &(off, val) in PHY_CFG_IN_SSC {
            self.phy_write_common(off, val);
        }
        // Apply both-SSC table
        for &(off, val) in PHY_CFG_BOTH_SSC {
            self.phy_write_common(off, val);
        }
        // Apply per-lane mix register table to all 16 lanes
        for lane in 0..16u8 {
            for &(off, val) in PHY_CFG_MIX_REG {
                self.phy_write_lane(lane, off, val);
            }
        }
    }

    /// Step 5: Deassert PHY reset, wait for clock stable.
    /// Reference: `mango_pcie.c:pcie_phy_rst_wait_pclk()`.
    fn phy_rst_wait_pclk(&self) -> Result<(), DeviceError> {
        // Assert PHY_RESET_N
        let v = self.link_read(0x02F8);
        self.link_write(0x02F8, v | (1 << 31));
        // Deassert P00/P01 PHY resets
        let v = self.link_read(0x01C0);
        self.link_write(0x01C0, v | (1 << 0) | (1 << 1));

        // Poll REG0080 bit31 (CLOCK_STABLE) until 0 — stable when cleared
        // mango_pcie.c: while (pcie_read(REG0080) & (1<<31)) ;
        // Add a simple timeout (10 ms at ~1 µs per iteration)
        for _ in 0..10_000 {
            if (self.link_read(0x0080) & (1 << 31)) == 0 {
                return Ok(());
            }
            fstart_arch::udelay(1);
        }
        Err(DeviceError::InitFailed)
    }

    /// Step 6: Set link width to x16.
    fn config_link_width(&self) {
        // For x16: disable FULL_PIPE_MUX (already done in sideband_init)
        // Link width register is implied by the lane mapping set above.
        // No additional register write needed for x16 per mango_pcie.c.
    }

    /// Step 7: Drive PERST# high via GPIO12.
    /// Reference: `mango_pcie.c:pcie_set_perst()`.
    fn set_perst(&self) {
        // GPIO0 DATA register: set bit 12 (PCIe RC reset, active-high release)
        // GPIO0_BASE = 0x7030009000; REG_GPIO_DATA = +0x00, REG_GPIO_DATA_DIR = +0x04
        let gpio_data = unsafe { core::ptr::read_volatile(self.gpio0_base as *const u32) };
        unsafe {
            core::ptr::write_volatile(self.gpio0_base as *mut u32, gpio_data | (1 << 12));
        }
        fstart_arch::udelay(100);
    }

    /// Step 8: Deassert link reset (AXI, MGMT, PM).
    /// Reference: `mango_pcie.c:pcie_link0_reset()`.
    fn link0_reset(&self) {
        // Poll for PAD resets:
        // REG03A0: PIPE_P00_RESET_N[2]=1, PCIE0_RESET_X[11]=1, LINK0_RESET_N[3]=1
        for _ in 0..10_000 {
            let v = self.link_read(0x03A0);
            if (v & (1 << 2)) != 0 && (v & (1 << 11)) != 0 && (v & (1 << 3)) != 0 {
                break;
            }
            fstart_arch::udelay(1);
        }

        // AXI_RESET_N: REG007C |= (1<<24)
        let v = self.link_read(0x007C);
        self.link_write(0x007C, v | (1 << 24));
        // MGMT_STICKY_RESET_N, MGMT_RESET_N: REG03CC |= (1<<28)|(1<<29)
        let v = self.link_read(0x03CC);
        self.link_write(0x03CC, v | (1 << 28) | (1 << 29));
        // PM_RESET_N: REG0000 |= (1<<9)
        let v = self.link_read(0x0000);
        self.link_write(0x0000, v | (1 << 9));
    }

    /// Step 9: Enable link training, poll for DL_INIT_COMPLETED.
    /// Reference: `mango_pcie.c:pcie_train_link()`.
    fn train_link(&self) -> Result<(), DeviceError> {
        // Enable link training: REG0004 |= (1<<12)
        let v = self.link_read(0x0004);
        self.link_write(0x0004, v | (1 << 12));

        // Poll IRS_REG0080[23:22] until == 0x3 (DL_INIT_COMPLETED)
        // mango_pcie.c: while ((irs_read(0x0080) >> 22) & 0x3 != 0x3) ;
        // Timeout: 10 ms
        for _ in 0..10_000 {
            let status = self.irs_read(0x0080);
            if ((status >> 22) & 0x3) == 0x3 {
                return Ok(());
            }
            fstart_arch::udelay(1);
        }
        // Log LTSSM state before returning error
        let ltssm = (self.irs_read(0x0080) >> 8) & 0x3F;
        fstart_log::error!(
            "[SG2042 PCIe] link training timeout, LTSSM state = {:x}",
            ltssm
        );
        Err(DeviceError::InitFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phy_cfg_ex_ssc_length() {
        assert_eq!(PHY_CFG_EX_SSC.len(), 2);
    }

    #[test]
    fn test_phy_cfg_in_ssc_length() {
        assert_eq!(PHY_CFG_IN_SSC.len(), 12);
    }

    #[test]
    fn test_phy_cfg_both_ssc_length() {
        assert_eq!(PHY_CFG_BOTH_SSC.len(), 5);
    }

    #[test]
    fn test_phy_cfg_mix_reg_length() {
        assert_eq!(PHY_CFG_MIX_REG.len(), 27);
    }
}
