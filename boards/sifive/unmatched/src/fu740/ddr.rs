//! FU740 DDR4 controller driver.

use fstart_core::services::DeviceError;

use super::FU740_DRAM_BASE;
use super::regs::{HIFIVE_UNMATCHED_DENALI_CTL, HIFIVE_UNMATCHED_DENALI_PHY};
use crate::config::{Fu740DdrConfig, Fu740DdrProfile};

const FU740_DDR_CTL_BASE: u64 = 0x100b_0000;
const FU740_DDR_PHY_BASE: u64 = 0x100b_2000;
const FU740_DDR_FILTER_BASE: u64 = 0x100b_8000;

const CTL_0_START: u32 = 1 << 0;
const DRAM_CLASS_OFFSET: u32 = 8;
const DRAM_CLASS_DDR4: u32 = 0xa;
const CTL_21_OPTIMAL_RMODW_EN: u32 = 1 << 0;
const CTL_120_DISABLE_RD_INTERLEAVE: u32 = 1 << 16;
const CTL_132_MC_INIT_COMPLETE: u32 = 1 << 8;
const CTL_136_OUT_OF_RANGE: u32 = 1 << 1;
const CTL_136_MULTI_OUT_OF_RANGE: u32 = 1 << 2;
const CTL_136_PORT_CMD_ERROR: u32 = 1 << 7;
const CTL_136_MC_INIT_COMPLETE: u32 = 1 << 8;
const CTL_136_LEVELING_DONE: u32 = 1 << 22;
const CTL_170_WRLVL_EN: u32 = 1 << 0;
const CTL_170_DFI_PHY_WRLELV_MODE: u32 = 1 << 24;
const CTL_181_DFI_PHY_RDLVL_MODE: u32 = 1 << 24;
const CTL_182_DFI_PHY_RDLVL_GATE_MODE: u32 = 1 << 0;
const CTL_184_VREF_EN: u32 = 1 << 24;
const CTL_208_PORT_ADDR_PROT_EN: u32 = 1 << 0;
const CTL_208_AXI0_ADDR_RANGE_EN: u32 = 1 << 8;
const CTL_224_AXI0_RANGE_PROT: u32 = 0x3 << 24;
const CTL_260_RDLVL_EN: u32 = 1 << 16;
const CTL_260_RDLVL_GATE_EN: u32 = 1 << 24;
const PHYS_FILTER_RWX_TOR: u64 = 0x0f00_0000_0000_0000;

/// FU740 Cadence Denali DDR controller.
pub struct Fu740Ddr {
    config: &'static Fu740DdrConfig,
}

unsafe impl Send for Fu740Ddr {}
unsafe impl Sync for Fu740Ddr {}

impl Fu740Ddr {
    pub fn new(config: &'static Fu740DdrConfig) -> Result<Self, DeviceError> {
        if config.dram_size < 0x4000 || config.dram_size > u64::MAX - FU740_DRAM_BASE {
            return Err(DeviceError::ConfigError);
        }
        Ok(Self { config })
    }

    #[inline(always)]
    fn ctl_read(&self, index: usize) -> u32 {
        // SAFETY: index is a FU740 DDR controller register index.
        unsafe {
            fstart_core::mmio::read32((FU740_DDR_CTL_BASE as usize + index * 4) as *const u32)
        }
    }

    #[inline(always)]
    fn ctl_write(&self, index: usize, value: u32) {
        // SAFETY: index is a FU740 DDR controller register index.
        unsafe {
            fstart_core::mmio::write32((FU740_DDR_CTL_BASE as usize + index * 4) as *mut u32, value)
        }
    }

    #[inline(always)]
    fn ctl_set_bits(&self, index: usize, bits: u32) {
        self.ctl_write(index, self.ctl_read(index) | bits);
    }

    #[inline(always)]
    fn ctl_clear_bits(&self, index: usize, bits: u32) {
        self.ctl_write(index, self.ctl_read(index) & !bits);
    }

    #[inline(always)]
    fn phy_write(&self, index: usize, value: u32) {
        // SAFETY: index is a FU740 DDR PHY register index.
        unsafe {
            fstart_core::mmio::write32((FU740_DDR_PHY_BASE as usize + index * 4) as *mut u32, value)
        }
    }

    #[inline(always)]
    fn phy_read(&self, index: usize) -> u32 {
        // SAFETY: index is a FU740 DDR PHY register index.
        unsafe {
            fstart_core::mmio::read32((FU740_DDR_PHY_BASE as usize + index * 4) as *const u32)
        }
    }

    fn write_ctl_regs(&self) {
        let table = match self.config.profile {
            Fu740DdrProfile::HiFiveUnmatched => &HIFIVE_UNMATCHED_DENALI_CTL,
        };
        for (index, value) in table.iter().enumerate() {
            self.ctl_write(index, *value);
        }
    }

    fn phy_reset(&self) {
        let table = match self.config.profile {
            Fu740DdrProfile::HiFiveUnmatched => &HIFIVE_UNMATCHED_DENALI_PHY,
        };
        // The global 1152..1214 range must precede all data-slice registers.
        for (index, value) in table.iter().enumerate().skip(1152).take(63) {
            self.phy_write(index, *value);
        }
        for (index, value) in table.iter().enumerate().take(1152) {
            self.phy_write(index, *value);
        }
    }

    fn enable_training(&self) {
        self.ctl_set_bits(120, CTL_120_DISABLE_RD_INTERLEAVE);
        self.ctl_clear_bits(21, CTL_21_OPTIMAL_RMODW_EN);
        self.ctl_set_bits(170, CTL_170_WRLVL_EN | CTL_170_DFI_PHY_WRLELV_MODE);
        self.ctl_set_bits(181, CTL_181_DFI_PHY_RDLVL_MODE);
        self.ctl_set_bits(260, CTL_260_RDLVL_EN);
        self.ctl_set_bits(260, CTL_260_RDLVL_GATE_EN);
        self.ctl_set_bits(182, CTL_182_DFI_PHY_RDLVL_GATE_MODE);
        if (self.ctl_read(0) >> DRAM_CLASS_OFFSET) & 0xf == DRAM_CLASS_DDR4 {
            self.ctl_set_bits(184, CTL_184_VREF_EN);
        }
    }

    fn mask_interrupts(&self) {
        self.ctl_set_bits(136, CTL_136_LEVELING_DONE);
        self.ctl_set_bits(136, CTL_136_MC_INIT_COMPLETE);
        self.ctl_set_bits(136, CTL_136_OUT_OF_RANGE | CTL_136_MULTI_OUT_OF_RANGE);
        self.ctl_set_bits(136, CTL_136_PORT_CMD_ERROR);
    }

    fn setup_range_protection(&self) {
        self.ctl_write(209, 0);
        self.ctl_write(210, ((self.config.dram_size >> 14) & 0x7f_ffff) as u32 - 1);
        self.ctl_write(212, 0);
        self.ctl_write(214, 0);
        self.ctl_write(216, 0);
        self.ctl_set_bits(224, CTL_224_AXI0_RANGE_PROT);
        self.ctl_write(225, u32::MAX);
        self.ctl_set_bits(208, CTL_208_AXI0_ADDR_RANGE_EN);
        self.ctl_set_bits(208, CTL_208_PORT_ADDR_PROT_EN);
    }

    fn start_and_wait(&self) -> Result<(), DeviceError> {
        self.ctl_set_bits(0, CTL_0_START);
        let mut timeout = 10_000_000_u32;
        while self.ctl_read(132) & CTL_132_MC_INIT_COMPLETE == 0 {
            core::hint::spin_loop();
            timeout -= 1;
            if timeout == 0 {
                fstart_log::error!("DDR: MC_INIT_COMPLETE timeout");
                return Err(DeviceError::InitFailed);
            }
        }
        let ddr_end = FU740_DRAM_BASE + self.config.dram_size;
        // SAFETY: the fixed FU740 physical filter is an aligned 64-bit register.
        unsafe {
            fstart_core::mmio::write64(
                FU740_DDR_FILTER_BASE as *mut u64,
                PHYS_FILTER_RWX_TOR | (ddr_end >> 2),
            )
        };
        Ok(())
    }

    /// Step 12: PHY fixup — errata workaround for RX calibration.
    ///
    /// Iterates over all 8 data slices and checks calibration quality.
    /// Logs errors but does not halt (non-fatal).
    fn phy_fixup(&self) {
        let mut failures = 0_u64;
        let mut slice_base = 0_usize;
        let mut dq = 0_u32;
        for _ in 0..8 {
            for register in 0..4 {
                let value = self.phy_read(slice_base + 34 + register);
                for bit in [0, 16] {
                    let down = (value >> bit) & 0x3f;
                    let up = (value >> (bit + 6)) & 0x3f;
                    if (down == 0 && up == 0x3f) || (up == 0 && down == 0x3f) {
                        failures |= 1 << dq;
                    }
                    dq += 1;
                }
            }
            slice_base += 128;
        }
        if failures != 0 {
            fstart_log::error!("DDR PHY fixup: calibration failures 0x{:016x}", failures);
        }
    }

    /// Program the HiFive Unmatched register tables and train DDR.
    pub fn init(&mut self) -> Result<(), DeviceError> {
        self.write_ctl_regs();
        self.phy_reset();
        self.enable_training();
        self.mask_interrupts();
        self.setup_range_protection();
        self.start_and_wait()?;
        self.phy_fixup();
        Ok(())
    }

    #[must_use]
    pub fn detected_size_bytes(&self) -> u64 {
        // SAFETY: the fixed FU740 physical filter is an aligned 64-bit register.
        let value = unsafe { fstart_core::mmio::read64(FU740_DDR_FILTER_BASE as *const u64) };
        let end = (value & 0x00ff_ffff_ffff_ffff) << 2;
        if end > FU740_DRAM_BASE {
            end - FU740_DRAM_BASE
        } else {
            self.config.dram_size
        }
    }
}
