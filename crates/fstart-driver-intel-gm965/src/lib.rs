//! Intel GM965 (Crestline) northbridge driver.
//!
//! This is the GM965/X61 counterpart to the Pineview northbridge driver.  It
//! performs the bootblock/romstage chipset setup that has to happen before the
//! ICH8-M southbridge and LPC-attached console can be used:
//!
//! - enable ECAM by programming PCIEXBAR through legacy CF8/CFC;
//! - map MCHBAR, DMIBAR and EPBAR;
//! - open the PAM shadow ranges;
//! - enable the integrated graphics functions used by the board;
//! - apply the early MCH/DMI tweaks from coreboot's GM965 port.

#![no_std]
#![allow(clippy::modulo_one)]

pub mod raminit;

use core::{cell::UnsafeCell, ptr};

use fstart_ecam as ecam;
use fstart_mmio::MmioReadWrite;
use fstart_mp::{SmmError, SmmInfo, SmmOps};
use fstart_services::device::{Device, DeviceError};
use fstart_services::memory_detect::{E820Entry, E820Kind, MemoryDetector};
use fstart_services::{
    EarlyInit, MemoryController, PciHost, PostDramInit, PreConsoleInit, ServiceError,
    StageLocalInit,
};
use serde::{Deserialize, Serialize};
use tock_registers::interfaces::ReadWriteable;

fn publish_mtrr_wb_ranges(entries: &[E820Entry]) {
    let mut ranges = [(0u64, 0u64); 8];
    let mut count = 0usize;
    for entry in entries {
        if entry.kind == E820Kind::Ram as u32 && entry.size != 0 && count < ranges.len() {
            ranges[count] = (entry.addr, entry.size);
            count += 1;
        }
    }
    fstart_arch_x86::mtrr::set_ram_wb_ranges(&ranges[..count]);
}

use tock_registers::{register_bitfields, register_structs};

/// GM965 host bridge PCI configuration register offsets and bit definitions.
pub mod hostbridge {
    /// Host bridge: bus 0, device 0, function 0.
    pub const HOST_DEV: u8 = 0;
    pub const HOST_FUNC: u8 = 0;

    pub const EPBAR_LO: u16 = 0x40;
    pub const EPBAR_HI: u16 = 0x44;
    pub const GGC: u16 = 0x52;
    pub const MCHBAR_LO: u16 = 0x48;
    pub const MCHBAR_HI: u16 = 0x4c;
    pub const DEVEN: u16 = 0x54;
    pub const PCIEXBAR_LO: u8 = 0x60;
    pub const PCIEXBAR_HI: u8 = 0x64;
    pub const DMIBAR_LO: u16 = 0x68;
    pub const DMIBAR_HI: u16 = 0x6c;
    pub const PAM0: u16 = 0x90;
    pub const REMAPBASE: u16 = 0x98;
    pub const REMAPLIMIT: u16 = 0x9a;
    pub const SMRAM: u16 = 0x9d;
    pub const ESMRAMC: u16 = 0x9e;
    pub const TOM: u16 = 0xa0;
    pub const TOUUD: u16 = 0xa2;
    pub const TOLUD: u16 = 0xb0;
    pub const CAPID0: u16 = 0xe0;
    pub const SKPD: u16 = 0xdc;

    pub const PCI_COMMAND: u16 = 0x04;
    pub const PCI_CMD_MEMORY: u16 = 1 << 1;
    pub const PCI_CMD_MASTER: u16 = 1 << 2;

    pub const DEVEN_D0F0: u32 = 1 << 0;
    pub const DEVEN_D1F0: u32 = 1 << 1;
    pub const DEVEN_D2F0: u32 = 1 << 3;
    pub const DEVEN_D2F1: u32 = 1 << 4;

    pub const PEG_DEV: u8 = 1;
    pub const PEG_FUNC: u8 = 0;
    pub const IGD_DEV: u8 = 2;
    pub const IGD_FUNC: u8 = 0;
    pub const IGD_ALT_FUNC: u8 = 1;
    pub const IGD_BAR0_GTTMMADR: u16 = 0x10;
    pub const IGD_BSM: u16 = 0x5c;
    pub const IGD_MSAC: u16 = 0x62;
    pub const IGD_SWSCI: u16 = 0xe8;
    pub const IGD_GDRST: u16 = 0xc0;
    pub const IGD_DISPLAY_CLOCK: u16 = 0xcc;
    pub const IGD_ASLS: u16 = 0xfc;
    pub const GCFGC: u16 = 0xf0;

    pub const DEFAULT_ECAM_BASE: usize = 0xe000_0000;
}

/// MCHBAR register offsets used by early init and memory-size readback.
pub mod mchbar {
    pub const FSBPMC3: u32 = 0x0040;
    pub const PM_CTRL0: u32 = 0x0040;
    pub const PM_CTRL1: u32 = 0x0044;
    pub const PM_NOCARB: u32 = 0x0090;
    pub const FSBPMC5: u32 = 0x0094;
    pub const PM_NOCARB_HI: u32 = 0x0094;
    pub const DCC: u32 = 0x0200;
    pub const DCC2: u32 = 0x0204;
    pub const CLKCROSS_DATA3: u32 = 0x0208;
    pub const CLKCROSS_DATA2: u32 = 0x020c;
    pub const CLKCROSS_DATA1: u32 = 0x0210;
    pub const WRITE_CTRL: u32 = 0x0218;
    pub const MMARB0: u32 = 0x0220;
    pub const MMARB1: u32 = 0x0224;
    pub const SBTEST: u32 = 0x0230;
    pub const POST_JEDEC_TIM0: u32 = 0x0238;
    pub const POST_JEDEC_TIM1: u32 = 0x023c;
    pub const RCOMP_CTRL: u32 = 0x0400;
    pub const RCOMP_STATUS: u32 = 0x0404;
    pub const RCOMP_CFG: u32 = 0x040c;
    pub const RCOMP_CFG2: u32 = 0x0414;
    pub const RCOMP_CFG3: u32 = 0x0418;
    pub const RCOMP_CFG4: u32 = 0x041c;
    pub const RCOMP_ODT0: u32 = 0x04d0;
    pub const RCOMP_ODT1: u32 = 0x04d4;
    pub const RCOMP_TABLES: u32 = 0x0680;
    pub const PM_SCHED: u32 = 0x0b00;
    pub const PM_SCHED_B90: u32 = 0x0b90;
    pub const IGD_HSYNC_VSYNC: u32 = 0x0bd0;
    pub const PM_BD8: u32 = 0x0bd8;
    pub const CLKCFG: u32 = 0x0c00;
    pub const CLKCFG_C14: u32 = 0x0c14;
    pub const CLKCFG_C16: u32 = 0x0c16;
    pub const CLKCFG_C20: u32 = 0x0c20;
    pub const HGIPMC2_LO: u32 = 0x0c38;
    pub const HGIPMC2_HI: u32 = 0x0c3a;
    pub const MCHBAR_FFC: u32 = 0x0ffc;
    pub const C2C3TT: u32 = 0x0f00;
    pub const C3C4TT: u32 = 0x0f04;
    pub const PM_F08: u32 = 0x0f08;
    pub const PM_F10: u32 = 0x0f10;
    pub const PMSTS: u32 = 0x0f14;
    pub const PM_F60: u32 = 0x0f60;
    pub const PM_F80: u32 = 0x0f80;
    pub const GIPMC1: u32 = 0x0fb0;
    pub const FSBPMC1: u32 = 0x0fb8;
    pub const UPMC3: u32 = 0x0fc0;
    pub const IO_INIT_CFG: u32 = 0x1400;
    pub const IO_INIT_CLK_DEP: u32 = 0x140c;
    pub const IO_INIT_CFG2: u32 = 0x1414;
    pub const IO_INIT_CFG3: u32 = 0x1418;
    pub const IO_INIT_CFG4: u32 = 0x141c;
    pub const IO_INIT_CFG5: u32 = 0x142c;
    pub const DRAM_TYPE_SELECT: u32 = 0x1434;
    pub const IO_INIT_CFG6: u32 = 0x1438;
    pub const IO_INIT_CFG7: u32 = 0x1440;
    pub const IO_RCOMP_CLK_EN: u32 = 0x1444;
    pub const THERMAL_ENABLE: u32 = 0x10ef;
    pub const SSKPD: u32 = 0x0c1c;

    pub const fn cx_drby(ch: usize, rank: usize) -> u32 {
        0x1200 + (ch as u32) * 0x100 + ((rank as u32) / 2) * 4
    }
    pub const fn cx_dra(ch: usize) -> u32 {
        0x1208 + (ch as u32) * 0x100
    }
    pub const fn cx_dra_hi(ch: usize) -> u32 {
        0x120a + (ch as u32) * 0x100
    }
    pub const fn cx_dclkdis(ch: usize) -> u32 {
        0x120c + (ch as u32) * 0x100
    }
    pub const fn cx_drt0(ch: usize) -> u32 {
        0x1210 + (ch as u32) * 0x100
    }
    pub const fn cx_drt1(ch: usize) -> u32 {
        0x1214 + (ch as u32) * 0x100
    }
    pub const fn cx_drt2(ch: usize) -> u32 {
        0x1218 + (ch as u32) * 0x100
    }
    pub const fn cx_drt3(ch: usize) -> u32 {
        0x121c + (ch as u32) * 0x100
    }
    pub const fn cx_drt4(ch: usize) -> u32 {
        0x1220 + (ch as u32) * 0x100
    }
    pub const fn cx_drt5(ch: usize) -> u32 {
        0x1224 + (ch as u32) * 0x100
    }
    pub const fn cx_drt6(ch: usize) -> u32 {
        0x1228 + (ch as u32) * 0x100
    }
    pub const fn cx_drc0(ch: usize) -> u32 {
        0x1230 + (ch as u32) * 0x100
    }
    pub const fn cx_drc1(ch: usize) -> u32 {
        0x1234 + (ch as u32) * 0x100
    }
    pub const fn cx_drc2(ch: usize) -> u32 {
        0x1238 + (ch as u32) * 0x100
    }
    pub const fn cx_odt_low(ch: usize) -> u32 {
        0x1248 + (ch as u32) * 0x100
    }
    pub const fn cx_odt_high(ch: usize) -> u32 {
        0x124c + (ch as u32) * 0x100
    }
    pub const fn cx_ait_lo(ch: usize) -> u32 {
        0x1250 + (ch as u32) * 0x100
    }
    pub const fn cx_ait_hi(ch: usize) -> u32 {
        0x1254 + (ch as u32) * 0x100
    }
    pub const fn cx_odt_misc(ch: usize) -> u32 {
        0x1260 + (ch as u32) * 0x100
    }
    pub const fn cx_odt_timing(ch: usize) -> u32 {
        0x1268 + (ch as u32) * 0x100
    }
    pub const fn cx_pwr_throttle1(ch: usize) -> u32 {
        0x1274 + (ch as u32) * 0x100
    }
    pub const fn cx_odt_ctrl(ch: usize) -> u32 {
        0x12a0 + (ch as u32) * 0x100
    }
    pub const fn train_enable(ch: usize) -> u32 {
        0x12a4 + (ch as u32) * 0x100
    }
    pub const fn cx_train_cfg(ch: usize) -> u32 {
        0x1484 + (ch as u32) * 0x100
    }
    pub const fn cx_train_pi(ch: usize) -> u32 {
        0x1490 + (ch as u32) * 0x100
    }
    pub const fn rec_dqs_level(ch: usize) -> u32 {
        0x14ac + (ch as u32) * 0x100
    }
    pub const fn rec_coarse_low(ch: usize) -> u32 {
        0x14b0 + (ch as u32) * 0x100
    }
    pub const fn rw_ptr_ctrl(ch: usize) -> u32 {
        0x14f0 + (ch as u32) * 0x100
    }
}

/// DMIBAR register offsets used by DMI link init.
pub mod dmibar {
    pub const DMIPVCCAP1: u32 = 0x004;
    pub const DMIVC0RCTL: u32 = 0x014;
    pub const DMIVC1RCAP: u32 = 0x01c;
    pub const DMIVC1RCTL: u32 = 0x020;
    pub const DMIVC1RSTS: u32 = 0x026;
    pub const VC1NP: u8 = 1 << 1;
    pub const DMIESD: u32 = 0x044;
    pub const DMILE1D: u32 = 0x050;
    pub const DMILE1A: u32 = 0x058;
    pub const DMILE2D: u32 = 0x060;
    pub const DMILE2A: u32 = 0x068;
    pub const DMILCAP: u32 = 0x084;
    pub const DMILCTL: u32 = 0x088;
    pub const DMILCTL2: u32 = 0x0204;
}

/// EPBAR register offsets used by DMI/egress init.
pub mod epbar {
    pub const EPPVCCAP1: u32 = 0x004;
    pub const EPVC0RCTL: u32 = 0x014;
    pub const EPVC1RCAP: u32 = 0x01c;
    pub const EPVC1RCTL: u32 = 0x020;
    pub const EPVC1RSTS: u32 = 0x026;
    pub const EPVC1MTS: u32 = 0x028;
    pub const EPESD: u32 = 0x044;
    pub const EPLE1D: u32 = 0x050;
    pub const EPLE1A: u32 = 0x058;
    pub const EPLE2D: u32 = 0x060;
    pub const EPLE2A: u32 = 0x068;

    pub const fn portarb(idx: u32) -> u32 {
        0x100 + 4 * idx
    }
}

register_bitfields! [u32,
    /// FSBPMC5 — Front Side Bus Power Management Control 5.
    pub FSBPMC5_REG [
        NON_ISOCH_DECODE OFFSET(19) NUMBITS(2) []
    ],
    /// DMILCTL2 — DMI Link Control 2.
    pub DMILCTL2_REG [
        DEEMPH_EQ OFFSET(10) NUMBITS(2) []
    ]
];

register_structs! {
    /// Sparse typed overlay for the early GM965 MCHBAR registers.
    pub Gm965MchBarRegs {
        (0x0000 => _pad0: [u8; 0x94]),
        (0x0094 => pub fsbpmc5: MmioReadWrite<u32, FSBPMC5_REG::Register>),
        (0x0098 => _pad1: [u8; 0x0b84]),
        (0x0c1c => pub sskpd: MmioReadWrite<u16>),
        (0x0c1e => @END),
    }
}

register_structs! {
    /// Sparse typed overlay for the early GM965 DMIBAR registers.
    pub Gm965DmiBarRegs {
        (0x0000 => _pad0: [u8; 0x204]),
        (0x0204 => pub dmilctl2: MmioReadWrite<u32, DMILCTL2_REG::Register>),
        (0x0208 => @END),
    }
}

/// Thin MCHBAR accessor.
#[derive(Clone, Copy)]
pub struct MchBar {
    base: usize,
}

impl MchBar {
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    #[inline]
    fn regs(&self) -> &'static Gm965MchBarRegs {
        // SAFETY: the base address is programmed into D0:F0 MCHBAR by early init.
        unsafe { &*(self.base as *const Gm965MchBarRegs) }
    }

    #[inline]
    pub fn read8(&self, off: u32) -> u8 {
        // SAFETY: off is a GM965 MCHBAR register offset.
        unsafe { fstart_mmio::read8((self.base + off as usize) as *const u8) }
    }

    #[inline]
    pub fn write8(&self, off: u32, val: u8) {
        // SAFETY: off is a GM965 MCHBAR register offset.
        unsafe { fstart_mmio::write8((self.base + off as usize) as *mut u8, val) }
    }

    #[inline]
    pub fn read16(&self, off: u32) -> u16 {
        // SAFETY: off is a GM965 MCHBAR register offset.
        unsafe { fstart_mmio::read16((self.base + off as usize) as *const u16) }
    }

    #[inline]
    pub fn write16(&self, off: u32, val: u16) {
        // SAFETY: off is a GM965 MCHBAR register offset.
        unsafe { fstart_mmio::write16((self.base + off as usize) as *mut u16, val) }
    }

    #[inline]
    pub fn read32(&self, off: u32) -> u32 {
        // SAFETY: off is a GM965 MCHBAR register offset.
        unsafe { fstart_mmio::read32((self.base + off as usize) as *const u32) }
    }

    #[inline]
    pub fn write32(&self, off: u32, val: u32) {
        // SAFETY: off is a GM965 MCHBAR register offset.
        unsafe { fstart_mmio::write32((self.base + off as usize) as *mut u32, val) }
    }

    #[inline]
    pub fn setbits8(&self, off: u32, bits: u8) {
        self.write8(off, self.read8(off) | bits);
    }

    #[inline]
    pub fn clrbits8(&self, off: u32, bits: u8) {
        self.write8(off, self.read8(off) & !bits);
    }

    #[inline]
    pub fn clrsetbits8(&self, off: u32, clear: u8, set: u8) {
        self.write8(off, (self.read8(off) & !clear) | set);
    }

    #[inline]
    pub fn setbits16(&self, off: u32, bits: u16) {
        self.write16(off, self.read16(off) | bits);
    }

    #[inline]
    pub fn clrbits16(&self, off: u32, bits: u16) {
        self.write16(off, self.read16(off) & !bits);
    }

    #[inline]
    pub fn clrsetbits16(&self, off: u32, clear: u16, set: u16) {
        self.write16(off, (self.read16(off) & !clear) | set);
    }

    #[inline]
    pub fn setbits32(&self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) | bits);
    }

    #[inline]
    pub fn clrbits32(&self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) & !bits);
    }

    #[inline]
    pub fn clrsetbits32(&self, off: u32, clear: u32, set: u32) {
        self.write32(off, (self.read32(off) & !clear) | set);
    }

    #[inline]
    fn set_non_isoch_decode_mode_b(&self) {
        self.regs()
            .fsbpmc5
            .modify(FSBPMC5_REG::NON_ISOCH_DECODE.val(0b10));
    }
}

/// Thin DMIBAR accessor.
#[derive(Clone, Copy)]
pub struct DmiBar {
    base: usize,
}

impl DmiBar {
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    #[inline]
    fn regs(&self) -> &'static Gm965DmiBarRegs {
        // SAFETY: the base address is programmed into D0:F0 DMIBAR by early init.
        unsafe { &*(self.base as *const Gm965DmiBarRegs) }
    }

    #[inline]
    pub fn read8(&self, off: u32) -> u8 {
        // SAFETY: off is a GM965 DMIBAR register offset.
        unsafe { fstart_mmio::read8((self.base + off as usize) as *const u8) }
    }

    #[inline]
    pub fn write8(&self, off: u32, val: u8) {
        // SAFETY: off is a GM965 DMIBAR register offset.
        unsafe { fstart_mmio::write8((self.base + off as usize) as *mut u8, val) }
    }

    #[inline]
    pub fn read16(&self, off: u32) -> u16 {
        // SAFETY: off is a GM965 DMIBAR register offset.
        unsafe { fstart_mmio::read16((self.base + off as usize) as *const u16) }
    }

    #[inline]
    pub fn write16(&self, off: u32, val: u16) {
        // SAFETY: off is a GM965 DMIBAR register offset.
        unsafe { fstart_mmio::write16((self.base + off as usize) as *mut u16, val) }
    }

    #[inline]
    pub fn read32(&self, off: u32) -> u32 {
        // SAFETY: off is a GM965 DMIBAR register offset.
        unsafe { fstart_mmio::read32((self.base + off as usize) as *const u32) }
    }

    #[inline]
    pub fn write32(&self, off: u32, val: u32) {
        // SAFETY: off is a GM965 DMIBAR register offset.
        unsafe { fstart_mmio::write32((self.base + off as usize) as *mut u32, val) }
    }

    #[inline]
    pub fn setbits8(&self, off: u32, bits: u8) {
        self.write8(off, self.read8(off) | bits);
    }

    #[inline]
    pub fn clrbits8(&self, off: u32, bits: u8) {
        self.write8(off, self.read8(off) & !bits);
    }

    #[inline]
    pub fn clrsetbits8(&self, off: u32, clear: u8, set: u8) {
        self.write8(off, (self.read8(off) & !clear) | set);
    }

    #[inline]
    pub fn setbits16(&self, off: u32, bits: u16) {
        self.write16(off, self.read16(off) | bits);
    }

    #[inline]
    pub fn clrbits16(&self, off: u32, bits: u16) {
        self.write16(off, self.read16(off) & !bits);
    }

    #[inline]
    pub fn clrsetbits16(&self, off: u32, clear: u16, set: u16) {
        self.write16(off, (self.read16(off) & !clear) | set);
    }

    #[inline]
    pub fn setbits32(&self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) | bits);
    }

    #[inline]
    pub fn clrbits32(&self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) & !bits);
    }

    #[inline]
    pub fn clrsetbits32(&self, off: u32, clear: u32, set: u32) {
        self.write32(off, (self.read32(off) & !clear) | set);
    }

    #[inline]
    fn clear_link_deemphasis_equalization(&self) {
        self.regs().dmilctl2.modify(DMILCTL2_REG::DEEMPH_EQ.val(0));
    }
}

/// Thin EPBAR accessor.
#[derive(Clone, Copy)]
pub struct EpBar {
    base: usize,
}

impl EpBar {
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    #[inline]
    pub fn read8(&self, off: u32) -> u8 {
        // SAFETY: off is a GM965 EPBAR register offset.
        unsafe { fstart_mmio::read8((self.base + off as usize) as *const u8) }
    }

    #[inline]
    pub fn write32(&self, off: u32, val: u32) {
        // SAFETY: off is a GM965 EPBAR register offset.
        unsafe { fstart_mmio::write32((self.base + off as usize) as *mut u32, val) }
    }

    #[inline]
    pub fn read32(&self, off: u32) -> u32 {
        // SAFETY: off is a GM965 EPBAR register offset.
        unsafe { fstart_mmio::read32((self.base + off as usize) as *const u32) }
    }

    #[inline]
    pub fn clrbits8(&self, off: u32, bits: u8) {
        // SAFETY: off is a GM965 EPBAR register offset.
        unsafe {
            let ptr = (self.base + off as usize) as *mut u8;
            fstart_mmio::write8(ptr, fstart_mmio::read8(ptr) & !bits);
        }
    }

    #[inline]
    pub fn clrsetbits8(&self, off: u32, clear: u8, set: u8) {
        // SAFETY: off is a GM965 EPBAR register offset.
        unsafe {
            let ptr = (self.base + off as usize) as *mut u8;
            fstart_mmio::write8(ptr, (fstart_mmio::read8(ptr) & !clear) | set);
        }
    }

    #[inline]
    pub fn setbits32(&self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) | bits);
    }

    #[inline]
    pub fn clrsetbits32(&self, off: u32, clear: u32, set: u32) {
        self.write32(off, (self.read32(off) & !clear) | set);
    }
}

/// Integrated graphics configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Gm965IgdConfig {
    /// Enable the integrated VGA function (D2:F0).
    #[serde(default = "default_true")]
    pub enable_vga: bool,
    /// Enable the secondary display function (D2:F1).
    #[serde(default = "default_true")]
    pub enable_pipe_b: bool,
    /// Fixed GTTMMADR BAR0 address used for non-display GMA setup.
    #[serde(default = "default_gtt_mmio_base")]
    pub gtt_mmio_base: u64,
    /// Raw VBT physical address, if firmware has staged a `vbt.bin` blob.
    #[serde(default)]
    pub vbt_addr: Option<u64>,
    /// Raw VBT size at `vbt_addr`.
    #[serde(default)]
    pub vbt_size: u32,
    /// Optional legacy VBIOS/VBT probe base, matching coreboot's 0xc0000 fallback.
    #[serde(default = "default_legacy_vbt_probe")]
    pub legacy_vbt_probe: Option<u64>,
    /// Panel power-up delay in 100us units.
    #[serde(default = "default_panel_power_up_delay")]
    pub panel_power_up_delay: u16,
    /// Panel power-down delay in 100us units.
    #[serde(default = "default_panel_power_down_delay")]
    pub panel_power_down_delay: u16,
    /// Panel backlight-on delay in 100us units.
    #[serde(default = "default_panel_backlight_on_delay")]
    pub panel_backlight_on_delay: u16,
    /// Panel backlight-off delay in 100us units.
    #[serde(default = "default_panel_backlight_off_delay")]
    pub panel_backlight_off_delay: u16,
    /// Panel power-cycle delay in 100ms units.
    #[serde(default = "default_panel_power_cycle_delay")]
    pub panel_power_cycle_delay: u8,
    /// Default backlight PWM frequency in Hz. Zero uses the coreboot fallback.
    #[serde(default)]
    pub default_pwm_freq: u16,
    /// Initial duty cycle percentage.
    #[serde(default = "default_backlight_duty_cycle")]
    pub duty_cycle: u8,
}

impl Default for Gm965IgdConfig {
    fn default() -> Self {
        Self {
            enable_vga: true,
            enable_pipe_b: true,
            gtt_mmio_base: default_gtt_mmio_base(),
            vbt_addr: None,
            vbt_size: 0,
            legacy_vbt_probe: default_legacy_vbt_probe(),
            panel_power_up_delay: default_panel_power_up_delay(),
            panel_power_down_delay: default_panel_power_down_delay(),
            panel_backlight_on_delay: default_panel_backlight_on_delay(),
            panel_backlight_off_delay: default_panel_backlight_off_delay(),
            panel_power_cycle_delay: default_panel_power_cycle_delay(),
            default_pwm_freq: 0,
            duty_cycle: default_backlight_duty_cycle(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_gtt_mmio_base() -> u64 {
    0xfeb0_0000
}

fn default_legacy_vbt_probe() -> Option<u64> {
    Some(0x000c_0000)
}

fn default_panel_power_up_delay() -> u16 {
    2000
}

fn default_panel_power_down_delay() -> u16 {
    2000
}

fn default_panel_backlight_on_delay() -> u16 {
    2000
}

fn default_panel_backlight_off_delay() -> u16 {
    2000
}

fn default_panel_power_cycle_delay() -> u8 {
    6
}

fn default_backlight_duty_cycle() -> u8 {
    100
}

const IGD_OPREGION_BASE_SIZE: usize = 8 * 1024;
const IGD_OPREGION_TOTAL_SIZE: usize = 16 * 1024;
const IGD_VBT_INLINE_OFFSET: usize = 0x400;
const IGD_VBT_INLINE_SIZE: usize = 6 * 1024;
const IGD_VBT_EXT_OFFSET: usize = IGD_OPREGION_BASE_SIZE;
const VBT_SIGNATURE: u32 = 0x5442_5624;

#[repr(align(4096))]
struct IgdOpRegionStore(UnsafeCell<[u8; IGD_OPREGION_TOTAL_SIZE]>);

// SAFETY: The opregion is initialized once during BSP chipset init, then shared
// read-mostly with ACPI/OS graphics drivers through ASLS.
unsafe impl Sync for IgdOpRegionStore {}

static IGD_OPREGION: IgdOpRegionStore =
    IgdOpRegionStore(UnsafeCell::new([0; IGD_OPREGION_TOTAL_SIZE]));

// GM965/ICH8 SMM constants. SMRAM bit definitions match coreboot's
// `cpu/intel/smm/gen1/smmrelocate.c`; PM I/O offsets live in
// `fstart-pmio-ich`.
const SMRAM_G_SMRAME: u8 = 1 << 3;
const SMRAM_D_LCK: u8 = 1 << 4;
const SMRAM_D_OPEN: u8 = 1 << 6;
const SMRAM_C_BASE_SEG: u8 = 0b010;
const ICH8_PMBASE: u16 = 0x0500;
const SMM_DEFAULT_SMBASE: u64 = 0x30000;
const EM64T101_SAVE_STATE_SIZE: usize = 0x400;
const EM64T101_SMBASE_SAVE_STATE_OFFSET: u16 = 0xfef8;

const ZERO_CPU_LAYOUT: fstart_smm::CpuSmmLayout = fstart_smm::CpuSmmLayout {
    smbase: 0,
    entry_addr: 0,
    save_state_base: 0,
    save_state_top: 0,
    stack_bottom: 0,
    stack_top: 0,
};

struct CpuLayoutStore(UnsafeCell<[fstart_smm::CpuSmmLayout; fstart_smm::runtime::MAX_SMM_CPUS]>);
struct SmbaseStore(UnsafeCell<[u64; fstart_smm::runtime::MAX_SMM_CPUS]>);

// SAFETY: firmware invokes SMM installation from the BSP while SMRAM is open;
// these scratch buffers are not shared with APs or interrupt context.
unsafe impl Sync for CpuLayoutStore {}
unsafe impl Sync for SmbaseStore {}

static GM965_SMM_CPU_LAYOUTS: CpuLayoutStore = CpuLayoutStore(UnsafeCell::new(
    [ZERO_CPU_LAYOUT; fstart_smm::runtime::MAX_SMM_CPUS],
));
static GM965_SMM_RELOCATION_SMBASES: SmbaseStore =
    SmbaseStore(UnsafeCell::new([0; fstart_smm::runtime::MAX_SMM_CPUS]));

/// GM965 northbridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelGm965Config {
    /// MCHBAR base address. X61 uses `0xfed14000`.
    pub mchbar: u64,
    /// DMIBAR base address.
    pub dmibar: u64,
    /// EPBAR base address.
    pub epbar: u64,
    /// ECAM (PCIEXBAR) base address. Default: `0xe0000000`.
    #[serde(default = "default_ecam_base")]
    pub ecam_base: u64,
    /// Number of buses decoded by PCIEXBAR (256, 128, or 64).
    #[serde(default = "default_ecam_buses")]
    pub ecam_buses: u16,
    /// Optional integrated graphics function enables.
    #[serde(default)]
    pub igd: Gm965IgdConfig,
    /// SMBus I/O base used for DIMM SPD probing during raminit.
    #[serde(default = "default_smbus_base")]
    pub smbus_base: u16,
    /// SPD EEPROM addresses in GM965 slot order: ch0 slot0/1, ch1 slot0/1.
    #[serde(default = "default_spd_addresses")]
    pub spd_addresses: [u8; 4],
    /// ACPI device name (reserved for future ACPI device generation).
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
}

fn default_ecam_base() -> u64 {
    hostbridge::DEFAULT_ECAM_BASE as u64
}

fn default_ecam_buses() -> u16 {
    64
}

fn default_smbus_base() -> u16 {
    0x0400
}

fn default_spd_addresses() -> [u8; 4] {
    [0x50, 0, 0x51, 0]
}

/// Intel GM965 northbridge driver.
pub struct IntelGm965 {
    config: IntelGm965Config,
    detected_size: u64,
}

// SAFETY: firmware performs chipset init on the BSP before concurrency exists.
unsafe impl Send for IntelGm965 {}
// SAFETY: the struct contains only immutable config and hardware register bases.
unsafe impl Sync for IntelGm965 {}

impl IntelGm965 {
    fn mchbar(&self) -> MchBar {
        MchBar::new(self.config.mchbar as usize)
    }

    fn dmibar(&self) -> DmiBar {
        DmiBar::new(self.config.dmibar as usize)
    }

    fn epbar(&self) -> EpBar {
        EpBar::new(self.config.epbar as usize)
    }

    fn pciexbar_length_bits(&self) -> u32 {
        match self.config.ecam_buses {
            256 => 0 << 1,
            128 => 1 << 1,
            _ => 2 << 1,
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn enable_ecam(&self) {
        let value = (self.config.ecam_base as u32) | self.pciexbar_length_bits() | 1;
        // SAFETY: one-time legacy PCI config write to enable ECAM before the
        // ECAM MMIO accessor can be used.
        unsafe {
            fstart_pio::pci_cfg_write32(0, 0, 0, hostbridge::PCIEXBAR_HI, 0);
            fstart_pio::pci_cfg_write32(0, 0, 0, hostbridge::PCIEXBAR_LO, value);
        }
        ecam::init(self.config.ecam_base as usize);
        fstart_log::info!("gm965: ECAM enabled at {:#x}", self.config.ecam_base);
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn enable_ecam(&self) {
        ecam::init(self.config.ecam_base as usize);
        fstart_log::info!("gm965: ECAM enable (stub, non-x86)");
    }

    fn setup_bars_and_pam(&self) {
        let hb = ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);

        hb.write32(hostbridge::MCHBAR_LO, (self.config.mchbar as u32) | 1);
        hb.write32(hostbridge::MCHBAR_HI, 0);
        hb.write32(hostbridge::DMIBAR_LO, (self.config.dmibar as u32) | 1);
        hb.write32(hostbridge::DMIBAR_HI, 0);
        hb.write32(hostbridge::EPBAR_LO, (self.config.epbar as u32) | 1);
        hb.write32(hostbridge::EPBAR_HI, 0);

        hb.write8(hostbridge::PAM0, 0x30);
        for idx in 1..=6 {
            hb.write8(hostbridge::PAM0 + idx, 0x33);
        }

        let mut deven = hostbridge::DEVEN_D0F0 | hostbridge::DEVEN_D1F0;
        if self.config.igd.enable_vga {
            deven |= hostbridge::DEVEN_D2F0;
        }
        if self.config.igd.enable_pipe_b {
            deven |= hostbridge::DEVEN_D2F1;
        }
        hb.write32(hostbridge::DEVEN, deven);
    }

    fn early_mch_dmi_tweaks(&self) {
        self.mchbar().set_non_isoch_decode_mode_b();
        self.dmibar().clear_link_deemphasis_equalization();
    }

    fn read_detected_size(&self) -> u64 {
        let hb = ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
        let touud = (hb.read16(hostbridge::TOUUD) as u64) << 20;
        if touud != 0 {
            return touud;
        }
        let tolud = ((hb.read16(hostbridge::TOLUD) as u64) & 0xfff0) << 16;
        if tolud != 0 {
            tolud
        } else {
            self.detected_size
        }
    }

    fn tom(&self) -> u64 {
        let hb = ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
        (u64::from(hb.read16(hostbridge::TOM) & 0x01ff)) << 27
    }

    fn touud(&self) -> u64 {
        let hb = ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
        u64::from(hb.read16(hostbridge::TOUUD)) << 20
    }

    fn tolud(&self) -> u32 {
        let hb = ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
        (u32::from(hb.read16(hostbridge::TOLUD) & 0xfff0)) << 16
    }

    fn igd_stolen_base(&self) -> u32 {
        let hb = ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
        if (hb.read32(hostbridge::DEVEN) & hostbridge::DEVEN_D2F0) == 0 {
            return 0;
        }
        ecam::PciDevBdf::new(0, hostbridge::IGD_DEV, hostbridge::IGD_FUNC)
            .read32(hostbridge::IGD_BSM)
    }

    fn tseg_size(&self) -> u32 {
        let esmramc = ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC)
            .read8(hostbridge::ESMRAMC);
        if esmramc & 1 == 0 {
            return 0;
        }
        match (esmramc >> 1) & 3 {
            0 => 1024 * 1024,
            1 => 2 * 1024 * 1024,
            2 => 8 * 1024 * 1024,
            _ => {
                fstart_log::error!("gm965: bad TSEG size encoding");
                0
            }
        }
    }

    fn tseg_base(&self) -> u32 {
        let top_reserved = match self.igd_stolen_base() {
            0 => self.tolud(),
            bsm => bsm,
        };
        top_reserved.saturating_sub(self.tseg_size())
    }

    fn usable_low_memory_top(&self) -> u32 {
        let mut top = self.tolud();
        let bsm = self.igd_stolen_base();
        if bsm != 0 {
            top = top.min(bsm);
        }
        let tseg = self.tseg_base();
        if tseg != 0 {
            top = top.min(tseg);
        }
        top
    }

    fn smm_region(&self) -> (u32, u32) {
        (self.tseg_base(), self.tseg_size())
    }

    fn write_smram(&self, val: u8) {
        ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC)
            .write8(hostbridge::SMRAM, val);
    }

    fn smm_open(&self) {
        self.write_smram(SMRAM_D_OPEN | SMRAM_G_SMRAME | SMRAM_C_BASE_SEG);
    }

    fn smm_close(&self) {
        self.write_smram(SMRAM_G_SMRAME | SMRAM_C_BASE_SEG);
    }

    fn smm_lock(&self) {
        self.write_smram(SMRAM_D_LCK | SMRAM_G_SMRAME | SMRAM_C_BASE_SEG);
    }

    fn smi_enable_for_relocation() {
        let pm = fstart_pmio_ich::PmIo::new(ICH8_PMBASE);
        pm.setbits32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::APMC_EN | fstart_pmio_ich::GBL_SMI_EN | fstart_pmio_ich::EOS,
        );
    }

    fn cr3() -> u64 {
        let cr3: u64;
        // SAFETY: reading CR3 is safe in firmware privileged mode.
        unsafe {
            core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        }
        cr3
    }

    fn build_e820_entries(
        &self,
        entries: &mut [E820Entry],
        usable_top: u32,
        touud: u64,
        tolud: u32,
    ) -> Result<usize, ServiceError> {
        if entries.len() < 6 {
            return Err(ServiceError::HardwareError);
        }

        let mut count = 0usize;
        entries[count] = E820Entry::new(0x0000_0000, 0x0009_f000, E820Kind::Ram);
        count += 1;
        entries[count] = E820Entry::new(0x0009_f000, 0x0000_1000, E820Kind::Reserved);
        count += 1;
        entries[count] = E820Entry::new(0x000f_0000, 0x0001_0000, E820Kind::Reserved);
        count += 1;

        let usable_top = u64::from(usable_top).max(0x0010_0000);
        let low_ram_size = usable_top.saturating_sub(0x0010_0000);
        if low_ram_size != 0 {
            entries[count] = E820Entry::new(0x0010_0000, low_ram_size, E820Kind::Ram);
            count += 1;
        }

        let top_reserved_size = u64::from(tolud).saturating_sub(usable_top);
        if top_reserved_size != 0 {
            entries[count] = E820Entry::new(usable_top, top_reserved_size, E820Kind::Reserved);
            count += 1;
        }

        let upper_ram_size = touud.saturating_sub(0x1_0000_0000);
        if upper_ram_size != 0 {
            entries[count] = E820Entry::new(0x1_0000_0000, upper_ram_size, E820Kind::Ram);
            count += 1;
        }

        Ok(count)
    }

    fn init_egress(&self) -> Result<(), ServiceError> {
        let ep = self.epbar();
        ep.clrbits8(epbar::EPVC0RCTL, !1u8);
        ep.clrsetbits8(epbar::EPPVCCAP1, 7, 1);
        ep.write32(epbar::EPVC1MTS, 0x0a0a_0a0a);
        ep.clrsetbits32(epbar::EPVC1RCAP, 127 << 16, 0x0a << 16);
        ep.clrsetbits32(epbar::EPVC1RCTL, 7 << 24, 1 << 24);
        ep.clrsetbits8(epbar::EPVC1RCTL, !1u8, 1 << 7);
        for idx in 0..7 {
            ep.write32(epbar::portarb(idx), 0x5555_5555);
        }
        ep.write32(epbar::portarb(7), 0x0000_5555);
        ep.setbits32(epbar::EPVC1RCTL, 1 << 16);

        let mut timeout = 0x7ffffu32;
        while (ep.read8(epbar::EPVC1RSTS) & 1) != 0 && timeout != 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            return Err(ServiceError::Timeout);
        }

        ep.setbits32(epbar::EPVC1RCTL, 1 << 31);
        timeout = 0x7ffff;
        while (ep.read8(epbar::EPVC1RSTS) & 2) != 0 && timeout != 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            return Err(ServiceError::Timeout);
        }
        Ok(())
    }

    fn init_dmi(&self) -> Result<(), ServiceError> {
        let dmi = self.dmibar();
        dmi.clrbits8(dmibar::DMIVC0RCTL, !1u8);
        dmi.clrsetbits8(dmibar::DMIPVCCAP1, 7, 1);
        dmi.clrsetbits32(dmibar::DMIVC1RCTL, 7 << 24, 1 << 24);
        dmi.clrsetbits8(dmibar::DMIVC1RCTL, !1u8, 1 << 7);
        dmi.setbits32(dmibar::DMIVC1RCTL, 1 << 31);

        let mut timeout = 0x7ffffu32;
        while (dmi.read8(dmibar::DMIVC1RSTS) & dmibar::VC1NP) != 0 && timeout != 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            return Err(ServiceError::Timeout);
        }

        dmi.setbits32(0x0200, 3 << 13);
        dmi.clrbits32(0x0200, 1 << 21);
        dmi.clrsetbits32(0x0200, 3 << 26, 2 << 26);
        dmi.write32(0x002c, 0x8600_0040);
        dmi.setbits32(0x00fc, (1 << 0) | (1 << 1) | (1 << 4));
        if self.stepping() < 0x02 {
            dmi.setbits32(0x00fc, 1 << 11);
        } else {
            dmi.clrbits32(0x00fc, 1 << 11);
        }
        dmi.clrbits32(dmibar::DMILCTL2, 3 << 10);
        dmi.clrbits32(0x00f4, 1 << 4);
        dmi.setbits32(0x00f0, 3 << 24);
        for off in [0x0f04, 0x0f44, 0x0f84, 0x0fc4] {
            dmi.write32(off, 0x0705_0880);
        }
        for off in [0x0308, 0x0314, 0x0324, 0x0328, 0x0334, 0x0338] {
            dmi.setbits32(off, 0);
        }
        Ok(())
    }

    fn setup_rcrb(&self) {
        let ep = self.epbar();
        let dmi = self.dmibar();
        ep.clrsetbits32(epbar::EPESD, 0xff << 16, 1 << 16);
        ep.clrsetbits32(epbar::EPLE1D, 0xff << 16, (1 << 16) | 1);
        ep.write32(epbar::EPLE1A, self.config.dmibar as u32);

        let peg = ecam::PciDevBdf::new(0, hostbridge::PEG_DEV, hostbridge::PEG_FUNC);
        if peg.read8(0) != 0xff {
            ep.clrsetbits32(epbar::EPLE2D, 0xff << 16, (1 << 16) | 1);
            peg.modify32(0x0144, !(0xff << 16), 1 << 16);
            peg.write32(0x0158, self.config.epbar as u32);
            peg.modify32(0x0150, !(0xff << 16), (1 << 16) | 1);
        }

        dmi.clrsetbits32(dmibar::DMIESD, 0xff << 16, 1 << 16);
        dmi.write32(dmibar::DMILE1A, 0xfed1_c000);
        dmi.clrsetbits32(dmibar::DMILE1D, 0xffff << 16, (2 << 16) | 1);
        dmi.write32(dmibar::DMILE2A, self.config.epbar as u32);
        dmi.clrsetbits32(dmibar::DMILE2D, 0xff << 16, (1 << 16) | 1);
    }

    fn setup_aspm(&self) {
        let dmi = self.dmibar();
        dmi.setbits8(0x0e1c, 1);
        dmi.setbits16(0x0f00, 3 << 8);
        dmi.setbits16(0x0f00, 7 << 3);
        dmi.clrbits32(0x0f14, 1 << 17);
        dmi.clrbits16(0x0e1c, 1 << 8);
        if self.stepping() >= 0x02 {
            dmi.write32(0x0e2c, 0x88d0_7333);
        }
        dmi.setbits8(dmibar::DMILCTL, 3);
        dmi.clrsetbits32(dmibar::DMILCAP, 63 << 12, (2 << 12) | (2 << 15));
        dmi.write8(0x0208 + 3, 0);
        dmi.clrbits32(0x0208, 3 << 20);
    }

    fn gm965_dmi_init(&self) -> Result<(), ServiceError> {
        self.init_egress()?;
        self.init_dmi()?;
        self.setup_rcrb();
        self.setup_aspm();
        fstart_log::info!("intel-gm965: DMI/egress link init complete");
        Ok(())
    }

    fn stepping(&self) -> u8 {
        ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC).read8(0x08)
    }

    fn fsb_clock_index(&self) -> usize {
        let raw = (self.mchbar().read32(mchbar::CLKCFG) & 0x7) as usize;
        if raw <= 3 && raw != 0 {
            raw
        } else {
            2
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn cpu_supports_slfm(&self) -> bool {
        // SAFETY: MSR 0xee is the Intel Core/Core2 extended config MSR used by
        // coreboot to detect SLFM support on this platform.
        unsafe { (fstart_arch_x86::msr::rdmsr(0x00ee) & (1 << 27)) != 0 }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn cpu_supports_slfm(&self) -> bool {
        false
    }

    fn gm965_pm_init(&self) {
        const HGIPMC2_HI: [u16; 4] = [0, 0x0c3d, 0x125c, 0x0f4c];
        const HGIPMC2_LO: [u16; 4] = [0, 0x0bb8, 0x1194, 0x0ea6];
        const CLKCFG_C16: [u8; 4] = [0, 0x0b, 0x10, 0x0d];
        const PM_F00: [u32; 4] = [0, 0x0000_0480, 0x0000_0700, 0x0000_0600];
        const PM_F04: [u32; 4] = [0, 0x0000_1780, 0x0000_2380, 0x0000_1d80];

        let mch = self.mchbar();
        let fsb = self.fsb_clock_index();
        let stepping = self.stepping();
        let peg = ecam::PciDevBdf::new(0, hostbridge::PEG_DEV, hostbridge::PEG_FUNC);

        mch.write16(mchbar::CLKCFG_C14, 0x0010);
        if peg.read8(0) == 0xff {
            mch.setbits16(mchbar::CLKCFG_C14, 0x21);
        }
        mch.write16(mchbar::CLKCFG_C20, 0x0001);
        mch.write32(
            mchbar::UPMC3,
            if stepping == 0x00 {
                0x041f_06fd
            } else if stepping == 0x01 {
                0x041f_0efd
            } else {
                0x061f_0efd
            },
        );
        mch.write8(mchbar::GIPMC1, 0x03);
        mch.setbits8(mchbar::PM_F10, 1 << 1);
        mch.write16(mchbar::HGIPMC2_HI, HGIPMC2_HI[fsb]);
        mch.clrsetbits8(mchbar::CLKCFG_C16, 0x7f, CLKCFG_C16[fsb]);
        mch.write8(mchbar::FSBPMC1, 0x03);
        mch.write16(mchbar::HGIPMC2_LO, HGIPMC2_LO[fsb]);
        mch.setbits8(mchbar::PM_F10, 1 << 5);
        mch.clrsetbits16(mchbar::CLKCFG_C16, !0xc3ffu16, 0x3400);
        mch.write32(mchbar::PM_F60, 0x0103_0419);
        mch.write32(mchbar::C2C3TT, PM_F00[fsb]);
        mch.write32(mchbar::C3C4TT, PM_F04[fsb]);
        mch.write16(mchbar::PM_F08, 0x730f);
        mch.setbits32(mchbar::PM_F80, 1 << 31);
        mch.clrsetbits32(
            mchbar::PM_CTRL0,
            (1 << 19) | (1 << 13),
            (1 << 21) | (1 << 9) | (1 << 2),
        );
        let mut ctrl1 = mch.read32(mchbar::PM_CTRL1) & 0xfeff_ffff;
        ctrl1 |= 0x4220_0020;
        if stepping != 0 {
            ctrl1 |= 0x10;
        }
        mch.write32(mchbar::PM_CTRL1, ctrl1);
        mch.clrsetbits16(mchbar::PM_NOCARB, 0x07, 0x04);
        mch.clrbits32(mchbar::PM_NOCARB_HI, 1 << 18);
        mch.setbits32(
            mchbar::PM_NOCARB_HI,
            (1 << 29) | (1 << 13) | (1 << 11) | (1 << 8),
        );
        if stepping > 0x01 {
            mch.setbits32(mchbar::PM_NOCARB_HI, (1 << 5) | (1 << 4));
        }
        mch.setbits16(mchbar::PM_SCHED, 1);
        mch.clrbits32(mchbar::PM_SCHED_B90, (1 << 23) | (1 << 7));
        mch.setbits32(mchbar::PM_BD8, 0x0c);

        if self.cpu_supports_slfm() {
            mch.clrbits16(mchbar::CLKCFG, 1 << 7);
            mch.setbits16(mchbar::CLKCFG, 1 << 14);
            mch.setbits32(mchbar::PM_CTRL1, 1 << 31);
        } else {
            mch.clrbits16(mchbar::CLKCFG, 1 << 14);
            mch.setbits16(mchbar::CLKCFG, 1 << 7);
            mch.clrbits32(mchbar::PM_CTRL1, 1 << 31);
        }

        fstart_log::info!(
            "intel-gm965: PM init complete stepping={} fsb_idx={}",
            stepping as u32,
            fsb as u32,
        );
    }

    fn thermal_sensor_init(
        &self,
        info: &raminit::RaminitInfo,
        smbus: &mut fstart_smbus_intel::I801SmBus,
    ) {
        const TSE2004_CAPABILITY: u8 = 0x00;
        const TSE2004_CONFIG: u8 = 0x01;
        const TSE2004_ALARM_HIGH: u8 = 0x02;
        const TSE2004_ALARM_LOW: u8 = 0x03;
        const TSE2004_CRITICAL: u8 = 0x04;
        const TSE2004_SLAVE_BASE: u8 = 0x18;

        let mut found = false;
        for (slot, dimm) in info.dimms.iter().enumerate() {
            if !dimm.present {
                continue;
            }
            let slave = TSE2004_SLAVE_BASE + slot as u8;
            if smbus.read_word_data(slave, TSE2004_CAPABILITY).is_err() {
                continue;
            }
            let _ = smbus.write_word_data(slave, TSE2004_ALARM_HIGH, 0x0a80);
            let _ = smbus.write_word_data(slave, TSE2004_CRITICAL, 0x0c80);
            let _ = smbus.write_word_data(slave, TSE2004_ALARM_LOW, 0x0000);
            let _ = smbus.write_word_data(slave, TSE2004_CONFIG, 0x0060);
            found = true;
        }

        if found {
            self.mchbar().write8(mchbar::THERMAL_ENABLE, 0xd0);
            fstart_log::info!("intel-gm965: DIMM thermal sensors enabled");
        }
    }

    fn igd(&self) -> ecam::PciDevBdf {
        ecam::PciDevBdf::new(0, hostbridge::IGD_DEV, hostbridge::IGD_FUNC)
    }

    fn igd_enabled(&self) -> bool {
        self.config.igd.enable_vga
            && (ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC)
                .read32(hostbridge::DEVEN)
                & hostbridge::DEVEN_D2F0)
                != 0
            && self.igd().read16(0) != 0xffff
    }

    fn opregion_write_u16(buf: &mut [u8], off: usize, val: u16) {
        buf[off..off + 2].copy_from_slice(&val.to_le_bytes());
    }

    fn opregion_write_u32(buf: &mut [u8], off: usize, val: u32) {
        buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    fn opregion_write_u64(buf: &mut [u8], off: usize, val: u64) {
        buf[off..off + 8].copy_from_slice(&val.to_le_bytes());
    }

    fn vbt_size(vbt: &[u8]) -> Option<usize> {
        if vbt.len() < 28 || u32::from_le_bytes([vbt[0], vbt[1], vbt[2], vbt[3]]) != VBT_SIGNATURE {
            return None;
        }
        let size = u16::from_le_bytes([vbt[24], vbt[25]]) as usize;
        if size == 0 || size > vbt.len() {
            None
        } else {
            Some(size)
        }
    }

    fn configured_vbt(&self) -> Option<&'static [u8]> {
        let addr = self.config.igd.vbt_addr? as usize;
        let size = self.config.igd.vbt_size as usize;
        if size == 0 {
            return None;
        }
        // SAFETY: board config promises this physical address contains a raw VBT blob.
        let bytes = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
        Self::vbt_size(bytes).map(|vbt_size| &bytes[..vbt_size])
    }

    fn legacy_vbt(&self) -> Option<&'static [u8]> {
        let base = self.config.igd.legacy_vbt_probe? as usize;
        // SAFETY: 0xc0000 legacy option ROM window is readable on PC-compatible x86.
        let rom = unsafe { core::slice::from_raw_parts(base as *const u8, 128 * 1024) };
        let mut off = 0usize;
        while off + 4 < rom.len() {
            if u32::from_le_bytes([rom[off], rom[off + 1], rom[off + 2], rom[off + 3]])
                == VBT_SIGNATURE
            {
                if let Some(size) = Self::vbt_size(&rom[off..]) {
                    return Some(&rom[off..off + size]);
                }
            }
            off += 16;
        }
        None
    }

    fn locate_vbt(&self) -> Option<&'static [u8]> {
        self.configured_vbt().or_else(|| self.legacy_vbt())
    }

    fn init_igd_opregion(&self) {
        if !self.igd_enabled() {
            return;
        }

        let Some(vbt) = self.locate_vbt() else {
            fstart_log::error!("intel-gm965: no valid VBT found for IGD opregion");
            return;
        };

        // SAFETY: BSP-only initialization before handing ASLS to the OS.
        let opregion = unsafe { &mut *IGD_OPREGION.0.get() };
        opregion.fill(0);
        opregion[0..16].copy_from_slice(b"IntelGraphicsMem");
        Self::opregion_write_u32(opregion, 16, (IGD_OPREGION_BASE_SIZE / 1024) as u32);
        opregion[20] = 0;
        opregion[21] = 0;
        opregion[22] = 1;
        opregion[23] = 2;
        if vbt.len() >= 82 {
            opregion[56..60].copy_from_slice(&vbt[78..82]);
        }
        Self::opregion_write_u32(opregion, 88, (1 << 0) | (1 << 2) | (1 << 3) | (1 << 4));

        Self::opregion_write_u32(opregion, 0x100 + 172, 1);
        Self::opregion_write_u32(opregion, 0x300 + 16, 0xff);
        Self::opregion_write_u32(opregion, 0x300 + 20, (1 << 31) | 6);
        Self::opregion_write_u32(opregion, 0x300 + 24, (1 << 31) | 0x64);
        for (idx, level) in [
            0x0000u16, 0x0a19, 0x1433, 0x1e4c, 0x2866, 0x327f, 0x3c99, 0x46b2, 0x50cc, 0x5ae5,
            0x64ff,
        ]
        .iter()
        .copied()
        .enumerate()
        {
            Self::opregion_write_u16(opregion, 0x300 + 28 + idx * 2, 0x8000 | level);
        }

        if vbt.len() <= IGD_VBT_INLINE_SIZE {
            opregion[IGD_VBT_INLINE_OFFSET..IGD_VBT_INLINE_OFFSET + vbt.len()].copy_from_slice(vbt);
        } else {
            let ext_size = (vbt.len() + 511) & !511;
            let ext_size = ext_size.min(IGD_OPREGION_TOTAL_SIZE - IGD_VBT_EXT_OFFSET);
            opregion[IGD_VBT_EXT_OFFSET..IGD_VBT_EXT_OFFSET + vbt.len().min(ext_size)]
                .copy_from_slice(&vbt[..vbt.len().min(ext_size)]);
            Self::opregion_write_u64(opregion, 0x300 + 186, IGD_OPREGION_BASE_SIZE as u64);
            Self::opregion_write_u32(opregion, 0x300 + 194, ext_size as u32);
        }

        let igd = self.igd();
        igd.write32(hostbridge::IGD_ASLS, opregion.as_ptr() as u32);
        let swsci = (igd.read16(hostbridge::IGD_SWSCI) & !1) | (1 << 15);
        igd.write16(hostbridge::IGD_SWSCI, swsci);
        fstart_log::info!(
            "intel-gm965: IGD opregion at {:#x}, VBT {} bytes",
            opregion.as_ptr() as usize,
            vbt.len() as u32,
        );
    }

    fn gtt_mmio_read32(&self, off: usize) -> u32 {
        // SAFETY: GTTMMADR BAR0 has been programmed by `gma_non_display_init`.
        unsafe { fstart_mmio::read32((self.config.igd.gtt_mmio_base as usize + off) as *const u32) }
    }

    fn gtt_mmio_write32(&self, off: usize, val: u32) {
        // SAFETY: GTTMMADR BAR0 has been programmed by `gma_non_display_init`.
        unsafe {
            fstart_mmio::write32(
                (self.config.igd.gtt_mmio_base as usize + off) as *mut u32,
                val,
            )
        }
    }

    fn get_cdclk(&self) -> u32 {
        const CL_VCO_KHZ: [u32; 7] = [
            3_200_000, 4_000_000, 5_333_333, 6_400_000, 3_333_333, 3_566_667, 4_266_667,
        ];
        const DIV_3200: [u32; 3] = [16, 10, 8];
        const DIV_4000: [u32; 3] = [20, 12, 10];
        const DIV_5333: [u32; 3] = [24, 16, 14];
        let hpll_idx = (self.mchbar().read8(0x0c0f) & 7) as usize;
        let gcfgc = self.igd().read16(hostbridge::GCFGC);
        let cdclk_sel = ((gcfgc >> 8) & 0x1f).saturating_sub(1) as usize;
        if hpll_idx >= CL_VCO_KHZ.len() || cdclk_sel > 2 {
            return 200_000_000;
        }
        let vco = CL_VCO_KHZ[hpll_idx];
        let div = match vco {
            3_200_000 => DIV_3200[cdclk_sel],
            4_000_000 => DIV_4000[cdclk_sel],
            5_333_333 => DIV_5333[cdclk_sel],
            _ => return 200_000_000,
        };
        (vco / div) * 1000
    }

    fn freq_to_blc_pwm_ctl(&self, pwm_freq: u16, duty_perc: u8) -> u32 {
        let blc_mod = self.get_cdclk() / (128 * pwm_freq as u32);
        let duty = if duty_perc <= 100 {
            duty_perc as u32
        } else {
            100
        };
        (blc_mod << 16) | (blc_mod * duty / 100)
    }

    fn gma_pm_init_post_vbios(&self) {
        const PP_ON_DELAYS: usize = 0x61208;
        const PP_OFF_DELAYS: usize = 0x6120c;
        const PP_DIVISOR: usize = 0x61210;
        const BLC_PWM_CTL2: usize = 0x61250;
        const BLC_PWM_CTL: usize = 0x61254;
        let conf = &self.config.igd;
        if self.gtt_mmio_read32(PP_ON_DELAYS) == 0 {
            self.gtt_mmio_write32(
                PP_ON_DELAYS,
                ((conf.panel_power_up_delay as u32 & 0x1fff) << 16)
                    | (conf.panel_backlight_on_delay as u32 & 0x1fff),
            );
        }
        if self.gtt_mmio_read32(PP_OFF_DELAYS) == 0 {
            self.gtt_mmio_write32(
                PP_OFF_DELAYS,
                ((conf.panel_power_down_delay as u32 & 0x1fff) << 16)
                    | (conf.panel_backlight_off_delay as u32 & 0x1fff),
            );
        }
        if conf.panel_power_cycle_delay != 0 {
            self.gtt_mmio_write32(
                PP_DIVISOR,
                ((self.get_cdclk() / 20_000 - 1) << 8)
                    | (conf.panel_power_cycle_delay as u32 & 0x1f),
            );
        }
        self.gtt_mmio_write32(BLC_PWM_CTL2, 1 << 31);
        if conf.default_pwm_freq == 0 {
            self.gtt_mmio_write32(BLC_PWM_CTL, 0x0610_0610);
        } else {
            self.gtt_mmio_write32(
                BLC_PWM_CTL,
                self.freq_to_blc_pwm_ctl(conf.default_pwm_freq, conf.duty_cycle),
            );
        }
    }

    fn gtt_setup(&self) {
        const GFX_FLSH_CNTL: usize = 0x02170;
        const PGETBL_CTL: usize = 0x02020;
        let hb = ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
        let tolud = ((hb.read16(hostbridge::TOLUD) as u32) & 0xfff0) << 16;
        if tolud < 512 * 1024 {
            return;
        }
        let gtt_base = tolud - 512 * 1024;
        self.gtt_mmio_write32(GFX_FLSH_CNTL, 0);
        self.gtt_mmio_write32(PGETBL_CTL, gtt_base | 1);
        self.gtt_mmio_write32(GFX_FLSH_CNTL, 0);
    }

    fn gm965_igd_init_no_display(&self) {
        let hb = ecam::PciDevBdf::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
        let deven = hb.read32(hostbridge::DEVEN);
        let peg = ecam::PciDevBdf::new(0, hostbridge::PEG_DEV, hostbridge::PEG_FUNC);
        let peg_enabled = (deven & hostbridge::DEVEN_D1F0) != 0 && peg.read16(0) != 0xffff;
        let mch = self.mchbar();
        if peg_enabled {
            mch.setbits8(mchbar::PM_F10, 1);
            if (mch.read8(0x0c0f) & 0x80) == 0 {
                mch.setbits32(0x1190, 1 << 14);
                mch.setbits16(0x119e, (1 << 15) | (1 << 12));
            }
        } else {
            mch.write32(mchbar::IGD_HSYNC_VSYNC, 0xfd00_0000);
            mch.write8(mchbar::IGD_HSYNC_VSYNC + 4, 0xfd);
            let gcfgc = self.igd().read16(hostbridge::GCFGC);
            let vco_field = ((gcfgc >> 8) & 0x1f) as usize;
            let fsb_bits = (mch.read8(0x0c0f) & 0x07) as usize;
            const DISPLAY_CLOCK_TABLE: [[u16; 4]; 3] =
                [[200, 200, 222, 0], [320, 333, 333, 0], [400, 400, 381, 0]];
            if (1..=3).contains(&vco_field) && fsb_bits <= 3 {
                let clock = DISPLAY_CLOCK_TABLE[vco_field - 1][fsb_bits];
                if clock != 0 {
                    let cc = (self.igd().read16(hostbridge::IGD_DISPLAY_CLOCK) & 0xfc00) | clock;
                    self.igd().write16(hostbridge::IGD_DISPLAY_CLOCK, cc);
                }
            }
            mch.setbits32(mchbar::PM_CTRL0, 1 << 31);
        }
    }

    fn gma_non_display_init(&self) {
        if !self.igd_enabled() {
            return;
        }
        let igd = self.igd();
        igd.write32(
            hostbridge::IGD_BAR0_GTTMMADR,
            (self.config.igd.gtt_mmio_base as u32) & 0xfff0_0000,
        );
        igd.or16(
            hostbridge::PCI_COMMAND,
            hostbridge::PCI_CMD_MEMORY | hostbridge::PCI_CMD_MASTER,
        );
        igd.and8_or8(hostbridge::IGD_MSAC, !0x3, 0x2);
        self.init_igd_opregion();
        igd.write8(hostbridge::IGD_GDRST, 1);
        fstart_arch_x86::udelay(50);
        igd.write8(hostbridge::IGD_GDRST, 0);
        let mut timeout = 1_000_000u32;
        while (igd.read8(hostbridge::IGD_GDRST) & 1) != 0 && timeout != 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        let gtt = (self.config.igd.gtt_mmio_base as usize + 512 * 1024) as *mut u32;
        for idx in 0..(512 * 1024 / core::mem::size_of::<u32>()) {
            // SAFETY: BAR0 is 1 MiB; the upper 512 KiB is the GTT aperture.
            unsafe { core::ptr::write_volatile(gtt.add(idx), 0) };
        }
        self.gtt_setup();
        if self.config.igd.enable_pipe_b {
            let igd_alt = ecam::PciDevBdf::new(0, hostbridge::IGD_DEV, hostbridge::IGD_ALT_FUNC);
            if igd_alt.read16(0) != 0xffff {
                igd_alt.or16(hostbridge::PCI_COMMAND, hostbridge::PCI_CMD_MASTER);
            }
        }
        self.gma_pm_init_post_vbios();
        self.gm965_igd_init_no_display();
        fstart_log::info!("intel-gm965: IGD non-display init complete");
    }

    fn post_dram_chipset_init(&self) -> Result<(), ServiceError> {
        self.gm965_dmi_init()?;
        self.gm965_pm_init();
        self.gma_non_display_init();
        self.write_coreboot_scratchpad_marker();
        Ok(())
    }

    /// Mark the northbridge scratchpad the same way coreboot does after
    /// GM965 romstage completion. Useful for detecting a warm path later.
    pub fn write_coreboot_scratchpad_marker(&self) {
        self.mchbar().write16(mchbar::SSKPD, 0xcafe);
    }
}

impl Device for IntelGm965 {
    const NAME: &'static str = "intel-gm965";
    const COMPATIBLE: &'static [&'static str] = &["intel,gm965", "intel,crestline"];
    type Config = IntelGm965Config;

    fn new(config: &IntelGm965Config) -> Result<Self, DeviceError> {
        Ok(Self {
            config: config.clone(),
            detected_size: 0,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Keep construction side-effect free. `ChipsetPreConsole` calls
        // `init_device()` before ECAM and MCHBAR are enabled; touching PCI
        // config or MCHBAR here can hang silently before the console exists.
        // Runtime DRAM sizing is updated by `dram_init()` and later readers
        // fall back to TOLUD/TOUUD after chipset setup.
        Ok(())
    }
}

impl IntelGm965 {
    fn pre_console_phase(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        Ok(())
    }

    fn early_phase(&mut self) -> Result<(), ServiceError> {
        self.setup_bars_and_pam();
        self.early_mch_dmi_tweaks();
        fstart_log::info!("intel-gm965: early init complete");
        Ok(())
    }
}

impl PreConsoleInit for IntelGm965 {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.pre_console_phase()
    }
}

impl EarlyInit for IntelGm965 {
    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.early_phase()
    }
}

impl StageLocalInit for IntelGm965 {
    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        Ok(())
    }
}

impl PciHost for IntelGm965 {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.pre_console_phase()
    }

    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.early_phase()
    }
}

impl PostDramInit for IntelGm965 {
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.post_dram_chipset_init()
    }
}

fn gm965_ramtest_probe(addr: usize, top: usize) -> Result<(), ServiceError> {
    let addr = addr & !0x3;
    let Some(end) = addr.checked_add(core::mem::size_of::<u32>()) else {
        return Err(ServiceError::HardwareError);
    };
    if addr < 0x0010_0000 || end > top {
        return Ok(());
    }

    let p = addr as *mut u32;
    let addr_pattern = (addr as u32).rotate_left(13) ^ 0xa5a5_5a5a;
    const FIXED_PATTERNS: [u32; 4] = [0x0000_0000, 0xffff_ffff, 0x5555_5555, 0xaaaa_aaaa];

    fstart_log::info!("gm965 ramtest: testing DRAM at {:#x}", addr);
    // SAFETY: Called only after successful GM965 DRAM training. The caller
    // passes the top of fstart-usable low DRAM, so the probed address is below
    // IGD stolen memory, GTT, and TSEG reservations. The original word is
    // restored before returning.
    unsafe {
        let old = ptr::read_volatile(p);
        for pattern in FIXED_PATTERNS
            .iter()
            .copied()
            .chain(core::iter::once(addr_pattern))
        {
            ptr::write_volatile(p, pattern);
            let got = ptr::read_volatile(p);
            if got != pattern {
                ptr::write_volatile(p, old);
                fstart_log::error!(
                    "gm965 ramtest: failed at {:#x}: wrote {:#x}, read {:#x}",
                    addr,
                    pattern,
                    got
                );
                return Err(ServiceError::HardwareError);
            }
        }
        ptr::write_volatile(p, old);
    }
    fstart_log::info!("gm965 ramtest: passed at {:#x}", addr);
    Ok(())
}

fn gm965_lower_memory_test(test_top: u32) -> Result<(), ServiceError> {
    let top = test_top as usize;
    if top <= 0x0010_0000 + core::mem::size_of::<u32>() {
        fstart_log::error!("gm965 ramtest: invalid usable top {:#x}", top);
        return Err(ServiceError::HardwareError);
    }

    gm965_ramtest_probe(0x0010_0000, top)?;

    let postcar_stack_probe = 0x0310_0000usize.saturating_sub(core::mem::size_of::<u32>());
    gm965_ramtest_probe(postcar_stack_probe, top)?;

    let high_probe = if top > 32 * 1024 * 1024 {
        top - 16 * 1024 * 1024
    } else {
        top - 4096
    };
    gm965_ramtest_probe(high_probe, top)
}

impl MemoryDetector for IntelGm965 {
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        let tom = self.tom();
        let tolud = self.tolud();
        let usable_top = self.usable_low_memory_top();
        let raw_touud = self.touud();
        let max_reclaim = 0x1_0000_0000u64.saturating_sub(u64::from(tolud));
        let touud = if raw_touud > 0x1_0000_0000 && raw_touud <= tom.saturating_add(max_reclaim) {
            raw_touud
        } else {
            tom
        };

        if tom <= 0x0010_0000
            || tolud <= 0x0010_0000
            || usable_top <= 0x0010_0000
            || usable_top > tolud
        {
            fstart_log::error!(
                "gm965: invalid memory map TOM/TOUUD/TOLUD/usable {:#x}/{:#x}/{:#x}/{:#x}",
                tom,
                raw_touud,
                tolud,
                usable_top
            );
            return Err(ServiceError::HardwareError);
        }

        let count = self.build_e820_entries(entries, usable_top, touud, tolud)?;
        publish_mtrr_wb_ranges(&entries[..count]);
        fstart_log::info!(
            "gm965: detected memory map usable={:#x} TOLUD={:#x} TOM={:#x} TOUUD={:#x} TSEG={:#x}+{:#x}",
            usable_top,
            tolud,
            tom,
            touud,
            self.tseg_base(),
            self.tseg_size()
        );
        Ok(count)
    }

    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        Ok(self.tom())
    }
}

impl SmmOps for IntelGm965 {
    fn smm_info(&self) -> Option<SmmInfo> {
        let (base, size) = self.smm_region();
        if size == 0 {
            fstart_log::error!("gm965 SMM: TSEG is disabled");
            return None;
        }
        fstart_log::info!("gm965 SMM: TSEG base={:#x} size={:#x}", base, size);
        Some(SmmInfo {
            smbase: u64::from(base),
            smsize: size as usize,
            save_state_size: EM64T101_SAVE_STATE_SIZE,
        })
    }

    fn install_smm_handlers(
        &self,
        info: &SmmInfo,
        num_cpus: u16,
        image: &[u8],
    ) -> Result<(), SmmError> {
        self.smm_open();

        let layouts = unsafe { &mut *GM965_SMM_CPU_LAYOUTS.0.get() };
        let result = unsafe {
            fstart_smm::install_pic_image(
                image,
                fstart_smm::InstallConfig {
                    smram_base: info.smbase,
                    smram_size: info.smsize as u64,
                    num_cpus,
                    save_state_size: info.save_state_size as u32,
                    page_table_size: 0,
                    cr3: Self::cr3(),
                    platform_kind: fstart_smm::SMM_PLATFORM_INTEL_ICH,
                    platform_flags: 0,
                    platform_data: [ICH8_PMBASE as u64, 0x28, 0, 0],
                },
                layouts,
            )
        };

        match result {
            Ok(installed) => {
                let targets = &installed.cpus[..num_cpus as usize];
                let smbases = unsafe { &mut *GM965_SMM_RELOCATION_SMBASES.0.get() };
                smbases.fill(targets[0].smbase);
                for (dst, cpu) in smbases.iter_mut().zip(targets.iter()) {
                    *dst = cpu.smbase;
                }
                let default_handler = unsafe {
                    fstart_smm::install_default_relocation_table_handler(
                        fstart_smm::DefaultRelocationTableConfig {
                            default_smbase: SMM_DEFAULT_SMBASE,
                            target_smbases: smbases,
                            save_state_smbase_offset: EM64T101_SMBASE_SAVE_STATE_OFFSET,
                        },
                    )
                };
                if default_handler.is_err() {
                    self.smm_close();
                    fstart_log::error!("gm965 SMM: failed to install default relocation handler");
                    return Err(SmmError::InstallFailed);
                }

                fstart_log::info!(
                    "gm965 SMM: installed image common={:#x} entry={:#x} cpus={}",
                    installed.common_base,
                    installed.common_entry,
                    installed.cpus.len()
                );
                Ok(())
            }
            Err(_) => {
                self.smm_close();
                fstart_log::error!("gm965 SMM: failed to install SMM image");
                Err(SmmError::InstallFailed)
            }
        }
    }

    fn smm_relocate(&self) {
        Self::smi_enable_for_relocation();
        let lapic = fstart_lapic::Lapic::from_msr();
        lapic.send_ipi_self(fstart_lapic::INT_ASSERT | fstart_lapic::MT_SMI);
        lapic.wait_ready();
    }

    fn pre_smm_init(&self) {
        let pm = fstart_pmio_ich::PmIo::new(ICH8_PMBASE);
        pm.reset_smi_status();
        pm.write32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::APMC_EN | fstart_pmio_ich::GBL_SMI_EN | fstart_pmio_ich::EOS,
        );
    }

    fn post_smm_init(&self) {
        self.smm_close();
        let pm = fstart_pmio_ich::PmIo::new(ICH8_PMBASE);
        pm.reset_smi_status();
        pm.reset_pm1_status();
        pm.tco().reset_tco_status();
        pm.reset_gpe0_status();
        pm.write16(
            fstart_pmio_ich::PM1_EN,
            fstart_pmio_ich::PWRBTN_EN | fstart_pmio_ich::GBL_EN,
        );
        pm.write32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::TCO_EN
                | fstart_pmio_ich::APMC_EN
                | fstart_pmio_ich::SLP_SMI_EN
                | fstart_pmio_ich::GBL_SMI_EN
                | fstart_pmio_ich::EOS,
        );
        self.smm_lock();
        fstart_log::info!("gm965 SMM: permanent SMI enabled and SMRAM locked");
    }
}

impl MemoryController for IntelGm965 {
    fn dram_init(&mut self) -> Result<(), ServiceError> {
        let mut smbus = fstart_smbus_intel::I801SmBus::new(self.config.smbus_base);
        smbus.host_reset();
        let mut info = raminit::probe_dimms(&mut smbus, &self.config.spd_addresses)?;
        self.detected_size = info.total_bytes();
        raminit::cold_boot_train(&mut info, &self.mchbar())?;
        self.memory_test()?;
        self.thermal_sensor_init(&info, &mut smbus);
        Ok(())
    }

    fn detected_size_bytes(&self) -> u64 {
        self.read_detected_size()
    }

    fn memory_test(&self) -> Result<(), ServiceError> {
        let tom = self.tom();
        let tolud = self.tolud();
        let usable_top = self.usable_low_memory_top();
        let raw_touud = self.touud();
        let max_reclaim = 0x1_0000_0000u64.saturating_sub(u64::from(tolud));
        let touud = if raw_touud > 0x1_0000_0000 && raw_touud <= tom.saturating_add(max_reclaim) {
            raw_touud
        } else {
            tom
        };

        let mut entries = [E820Entry::zeroed(); 6];
        let count = self.build_e820_entries(&mut entries, usable_top, touud, tolud)?;
        publish_mtrr_wb_ranges(&entries[..count]);
        fstart_log::info!(
            "gm965: dynamic WB MTRR ranges set (TOLUD {:#x}, usable top {:#x})",
            tolud,
            usable_top
        );
        gm965_lower_memory_test(usable_top)
    }
}

// ---------------------------------------------------------------------------
// ACPI device implementation — GM965 host bridge / PCI0
// ---------------------------------------------------------------------------

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi::Aml;
    use fstart_acpi_macros::acpi_dsl;

    use super::*;

    impl AcpiDevice for IntelGm965 {
        type Config = IntelGm965Config;

        /// Produce GM965/X61 PCI root-bridge DSDT content.
        ///
        /// The generated `PCI0` scope mirrors the coreboot GM965 namespace at
        /// a boot-critical level: host-bridge identity, MCHC PCI config field
        /// access, PDRC reserved chipset MMIO ranges, PEG/GFX device stubs,
        /// root PCI resources, `_OSC`, `_PIC`, sleep states, and CPU device
        /// objects. Southbridge devices attach later through an absolute
        /// `\\_SB.PCI0` scope emitted by the ICH8 driver.
        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            let name = config.acpi_name.as_deref().unwrap_or("PCI0");
            let mchbar = config.mchbar as u32;
            let dmibar = config.dmibar as u32;
            let epbar = config.epbar as u32;
            let gttmmio = config.igd.gtt_mmio_base as u32;
            let rcba: u32 = 0xfed1_c000;
            let p = |s: &str| fstart_acpi::aml::Path::new(s);

            let mut aml = acpi_dsl! {
                Device(#{name}) {
                    Name("_HID", EisaId("PNP0A08"));
                    Name("_CID", EisaId("PNP0A03"));
                    Name("_SEG", 0u32);
                    Name("_BBN", 0u32);
                    Name("_UID", 0u32);

                    Device("MCHC") {
                        Name("_ADR", 0x00000000u32);
                        OperationRegion("MCHP", PciConfig, 0x00u32, 0x100u32);
                        Field("MCHP", DWordAcc, NoLock, Preserve) {
                            Offset(0x40),
                            EPEN, 1,
                            , 11,
                            EPBR, 24,
                            Offset(0x48),
                            MHEN, 1,
                            , 13,
                            MHBR, 22,
                            Offset(0x60),
                            PXEN, 1,
                            PXSZ, 2,
                            , 23,
                            PXBR, 10,
                            Offset(0x68),
                            DMEN, 1,
                            , 11,
                            DMBR, 24,
                            Offset(0x90),
                            , 4,
                            PM0H, 2,
                            , 2,
                            Offset(0x91),
                            PM1L, 2,
                            , 2,
                            PM1H, 2,
                            , 2,
                            Offset(0x92),
                            PM2L, 2,
                            , 2,
                            PM2H, 2,
                            , 2,
                            Offset(0x93),
                            PM3L, 2,
                            , 2,
                            PM3H, 2,
                            , 2,
                            Offset(0x94),
                            PM4L, 2,
                            , 2,
                            PM4H, 2,
                            , 2,
                            Offset(0x95),
                            PM5L, 2,
                            , 2,
                            PM5H, 2,
                            , 2,
                            Offset(0x96),
                            PM6L, 2,
                            , 2,
                            PM6H, 2,
                            , 2,
                            Offset(0xA0),
                            TOM_, 8,
                            Offset(0xB0),
                            , 4,
                            TLUD, 12,
                        }
                    }

                    Name("MCRS", ResourceTemplate {
                        WordBusNumber(0x0000u16, 0x003Fu16);
                        DWordIO(0x0000u32, 0x0CF7u32);
                        IO(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
                        DWordIO(0x0D00u32, 0xFFFFu32);
                        DWordMemory(Cacheable, ReadWrite, 0x000A0000u32, 0x000BFFFFu32);
                        DWordMemory(Cacheable, ReadWrite, 0x000C0000u32, 0x000FFFFFu32);
                        DWordMemory(NotCacheable, ReadWrite, 0x80000000u32, 0xFEBFFFFFu32);
                        Memory32Fixed(ReadWrite, 0xFED40000u32, 0x00005000u32);
                    });
                    Method("_CRS", 0, Serialized) {
                        Return(#{p("MCRS")});
                    }
                    Method("_OSC", 4, NotSerialized) {
                        Return(#{fstart_acpi::aml::Arg(3)});
                    }

                    Device("PDRC") {
                        Name("_HID", EisaId("PNP0C02"));
                        Name("_UID", 1u32);
                        Name("_CRS", ResourceTemplate {
                            Memory32Fixed(ReadWrite, #{rcba}, 0x4000u32);
                            Memory32Fixed(ReadWrite, #{mchbar}, 0x4000u32);
                            Memory32Fixed(ReadWrite, #{dmibar}, 0x1000u32);
                            Memory32Fixed(ReadWrite, #{epbar}, 0x1000u32);
                            Memory32Fixed(ReadWrite, 0xFED20000u32, 0x00020000u32);
                            Memory32Fixed(ReadWrite, 0xFED40000u32, 0x00005000u32);
                            Memory32Fixed(ReadWrite, 0xFED45000u32, 0x0004B000u32);
                        });
                    }

                    Device("PEGP") {
                        Name("_ADR", 0x00010000u32);
                        Name("_PRT", Package(
                            Package(0x0000FFFFu32, 0u32, 0u32, 16u32),
                            Package(0x0000FFFFu32, 1u32, 0u32, 17u32),
                            Package(0x0000FFFFu32, 2u32, 0u32, 18u32),
                            Package(0x0000FFFFu32, 3u32, 0u32, 19u32)
                        ));
                    }
                    Device("GFX0") {
                        Name("_ADR", 0x00020000u32);
                        OperationRegion("GFXC", PciConfig, 0x00u32, 0x100u32);
                        Field("GFXC", DWordAcc, NoLock, Preserve) {
                            Offset(0x10),
                            BAR0, 64,
                            Offset(0xE4),
                            ASLE, 32,
                            Offset(0xFC),
                            ASLS, 32,
                        }
                        OperationRegion("OPRG", SystemMemory, #{p("ASLS")}, 0x400u32);
                        Field("OPRG", DWordAcc, NoLock, Preserve) {
                            Offset(0x58),
                            MBOX, 32,
                            Offset(0x300),
                            ARDY, 1,
                            , 31,
                            ASLC, 32,
                            TCHE, 32,
                            ALSI, 32,
                            BCLP, 32,
                            PFIT, 32,
                            CBLV, 32,
                        }
                        OperationRegion("GFRG", SystemMemory, #{gttmmio}, 0x80000u32);
                        Field("GFRG", DWordAcc, NoLock, Preserve) {
                            Offset(0x61254),
                            BCLV, 16,
                            BCLM, 16,
                        }
                        Name("BRLV", 100u32);
                        Name("BRVA", 0u32);
                        Name("BRIG", Package(100u32, 100u32, 0u32, 10u32, 20u32, 30u32, 40u32, 50u32, 60u32, 70u32, 80u32, 90u32, 100u32));
                        Method("XBCM", 1, Serialized) {
                            BRLV = Arg0;
                            BRVA = 1u32;
                            Local0 = Arg0;
                            If (Local0 > 100u32) { Local0 = 100u32; }
                            If (ASLS == 0u32) { Return(Ones); }
                            If ((MBOX & 4u32) == 0u32) { Return(Ones); }
                            BCLP = Local0 | 0x80000000u32;
                            If (ARDY == 0u32) { Return(Ones); }
                            ASLC = 2u32;
                            ASLE = 1u32;
                            Local1 = 32u32;
                            While (Local1 > 0u32) {
                                Sleep(1u32);
                                If ((ASLC & 2u32) == 0u32) {
                                    If (((ASLC >> 12u32) & 3u32) == 0u32) { Return(0u32); }
                                    Return(Ones);
                                }
                                Local1--;
                            }
                            If (BCLM != 0u32) { BCLV = Local0; }
                            Return(Ones);
                        }
                        Method("XBQC", 0, NotSerialized) {
                            If (BRVA != 0u32) { Return(BRLV); }
                            Return(100u32);
                        }
                        Device("LCD0") {
                            Name("_ADR", 0x0400u32);
                            Method("_BCL", 0, NotSerialized) { Return(#{p("BRIG")}); }
                            Method("_BCM", 1, NotSerialized) { XBCM(Arg0); }
                            Method("_BQC", 0, NotSerialized) { Return(#{fstart_acpi::aml::MethodCall::new(p("XBQC"), alloc::vec![])}); }
                        }
                        Method("_DOS", 1, NotSerialized) { }
                        Method("DECB", 0, NotSerialized) {
                            Local0 = #{fstart_acpi::aml::MethodCall::new(p("XBQC"), alloc::vec![])};
                            If (Local0 > 0u32) { Local0 = Local0 - 10u32; }
                            XBCM(Local0);
                            Notify(LCD0, 0x87u32);
                        }
                        Method("INCB", 0, NotSerialized) {
                            Local0 = #{fstart_acpi::aml::MethodCall::new(p("XBQC"), alloc::vec![])};
                            If (Local0 < 100u32) { Local0 = Local0 + 10u32; }
                            XBCM(Local0);
                            Notify(LCD0, 0x86u32);
                        }
                        Method("_PS0", 0, NotSerialized) { }
                        Method("_PS3", 0, NotSerialized) { }
                        Method("_S0W", 0, NotSerialized) { Return(3u32); }
                        Method("_S3D", 0, NotSerialized) { Return(3u32); }
                    }
                }
            };

            aml.extend_from_slice(&acpi_dsl! {
                Name("PICM", 0u32);
                Method("_PIC", 1, NotSerialized) {
                    PICM = Arg0;
                }
                Name("_S0_", Package(0u32, 0u32, 0u32, 0u32));
                Name("_S3_", Package(5u32, 0u32, 0u32, 0u32));
                Name("_S4_", Package(6u32, 4u32, 0u32, 0u32));
                Name("_S5_", Package(7u32, 0u32, 0u32, 0u32));
                // CPU power-management objects are appended below using the
                // coreboot-derived SpeedStep/C-state generator.
            });

            aml.extend_from_slice(&fstart_cpu_intel::core2::cpu_devices_aml(2));

            aml
        }

        /// Produce an MCFG table for the GM965 PCIEXBAR ECAM window.
        fn extra_tables(&self, config: &Self::Config) -> Vec<Vec<u8>> {
            let end_bus = config.ecam_buses.saturating_sub(1).min(255) as u8;
            let mut mcfg = fstart_acpi::mcfg::MCFG::new(
                fstart_acpi::OEM_ID,
                fstart_acpi::OEM_TABLE_ID,
                fstart_acpi::OEM_REVISION,
            );
            mcfg.add_ecam(config.ecam_base, 0, 0, end_bus);
            let mut bytes = Vec::new();
            mcfg.to_aml_bytes(&mut bytes);
            alloc::vec![bytes]
        }
    }
}
