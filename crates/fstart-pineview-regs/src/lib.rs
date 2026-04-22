//! Intel Pineview northbridge register definitions.
//!
//! Named constants and tock-registers bitfield definitions ported from
//! coreboot's `mchbar_regs.h` and `hostbridge_regs.h`.
//!
//! ## Layout
//!
//! - **Offset constants** ([`mchbar`] module) for the raminit port.
//! - **[`register_bitfields!`]** for registers with meaningful named fields.
//! - **[`MchBar`]**, **[`DmiBar`]**, **[`Rcba`]** MMIO accessors with
//!   barrier-safe reads/writes via `fstart_mmio`.
//! - **[`EcamPci`]** — ECAM-based PCI config accessor. After the single
//!   CF8/CFC write that enables PCIEXBAR, *all* PCI config goes here.

#![no_std]
#![allow(clippy::modulo_one)] // tock-registers alignment test

use fstart_mmio::MmioReadWrite;
use tock_registers::register_bitfields;
use tock_registers::register_structs;

// ===================================================================
// Bitfield definitions
// ===================================================================

register_bitfields! [u32,
    /// GGC — GMCH Graphics Control (PCI config 0x52, 16-bit).
    pub GGC_REG [
        /// VGA Disable.
        VGADIS OFFSET(1) NUMBITS(1) [],
        /// GMS — Graphics Mode Select (stolen memory size).
        GMS OFFSET(4) NUMBITS(4) [],
        /// GGMS — GTT Graphics Memory Size.
        GGMS OFFSET(8) NUMBITS(2) []
    ],
    /// MCH_GCFGC — Graphics Clock Frequency & Gating Control (MCHBAR+0xC8C).
    pub MCH_GCFGC_REG [
        /// Core render clock frequency.
        CRCLK OFFSET(0) NUMBITS(4) [],
        /// Core display clock frequency.
        CDCLK OFFSET(4) NUMBITS(3) [],
        /// Update latch — toggle to apply new clocks.
        UPDATE OFFSET(9) NUMBITS(1) []
    ],
    /// DACGIOCTRL1 — DAC / GIO control 1 (MCHBAR+0xB08).
    pub DACGIOCTRL1_REG [
        /// VGA CRT output enable.
        VGA_CRT_EN OFFSET(15) NUMBITS(1) [],
        /// LVDS disable (bits 25:26).
        LVDS_DIS OFFSET(25) NUMBITS(2) []
    ]
];

// ===================================================================
// Early-init MCHBAR register struct (tock-registers overlay)
// ===================================================================

register_structs! {
    /// Subset of MCHBAR registers used during early init.
    ///
    /// The full raminit uses [`MchBar::read32`]/[`write32`] with offset
    /// constants from the [`mchbar`] module.
    pub MchBarEarlyRegs {
        (0x000 => _pad0: [u8; 0x30]),
        /// HIT0 — Host Interface Timing 0.
        (0x030 => pub hit0: MmioReadWrite<u32>),
        (0x034 => _pad1: [u8; 0x0C]),
        /// HIT4 — Host Interface Timing 4.
        (0x040 => pub hit4: MmioReadWrite<u32>),
        (0x044 => _pad2: [u8; 0xAC4]),
        /// DACGIOCTRL1 — DAC/GIO control 1.
        (0xB08 => pub dacgioctrl1: MmioReadWrite<u32, DACGIOCTRL1_REG::Register>),
        (0xB0C => _pad3: [u8; 0x180]),
        /// MCH_GCFGC — Graphics clock configuration.
        (0xC8C => pub gcfgc: MmioReadWrite<u32, MCH_GCFGC_REG::Register>),
        (0xC90 => _pad4: [u8; 0xA8]),
        /// HPLLVCO — Host PLL VCO.
        (0xD38 => _pad4b: [u8; 0x100]),
        (0xE38 => _pad4c: [u8; 0x1BC]),
        /// CICTRL register.
        (0xFF4 => pub cictrl: MmioReadWrite<u32>),
        /// CISDCTRL register.
        (0xFF8 => pub cisdctrl: MmioReadWrite<u32>),
        (0xFFC => _pad5: [u8; 0x04]),
        (0x1000 => @END),
    }
}

// ===================================================================
// MchBar — sparse MMIO accessor
// ===================================================================

/// MCHBAR accessor for the full register space.
///
/// The MCHBAR spans 0x0000–0x3830+ with hundreds of sparsely placed
/// registers. The [`MchBarEarlyRegs`] tock-registers overlay covers
/// the early-init subset; this wrapper provides raw offset-based access
/// for the raminit code.
pub struct MchBar {
    base: usize,
}

impl MchBar {
    pub const fn new(base: usize) -> Self { Self { base } }

    #[inline] pub fn read32(&self, off: u32) -> u32 {
        // SAFETY: base is the programmed MCHBAR; off is a register offset.
        unsafe { fstart_mmio::read32((self.base + off as usize) as *const u32) }
    }
    #[inline] pub fn write32(&self, off: u32, val: u32) {
        unsafe { fstart_mmio::write32((self.base + off as usize) as *mut u32, val) }
    }
    #[inline] pub fn read16(&self, off: u32) -> u16 {
        unsafe { fstart_mmio::read16((self.base + off as usize) as *const u16) }
    }
    #[inline] pub fn write16(&self, off: u32, val: u16) {
        unsafe { fstart_mmio::write16((self.base + off as usize) as *mut u16, val) }
    }
    #[inline] pub fn read8(&self, off: u32) -> u8 {
        unsafe { fstart_mmio::read8((self.base + off as usize) as *const u8) }
    }
    #[inline] pub fn write8(&self, off: u32, val: u8) {
        unsafe { fstart_mmio::write8((self.base + off as usize) as *mut u8, val) }
    }
    /// `reg = (reg & mask) | set`.
    #[inline] pub fn modify32(&self, off: u32, mask: u32, set: u32) {
        let v = self.read32(off); self.write32(off, (v & mask) | set);
    }
    #[inline] pub fn setbits32(&self, off: u32, bits: u32) { self.modify32(off, !0, bits); }
    #[inline] pub fn clrbits32(&self, off: u32, bits: u32) { self.modify32(off, !bits, 0); }

    /// Typed overlay for the early-init register subset.
    ///
    /// # Safety
    /// `self.base` must be a valid MCHBAR MMIO address.
    pub unsafe fn early_regs(&self) -> &MchBarEarlyRegs {
        unsafe { &*(self.base as *const MchBarEarlyRegs) }
    }
}

/// DMIBAR MMIO accessor.
pub struct DmiBar { base: usize }
impl DmiBar {
    pub const fn new(base: usize) -> Self { Self { base } }
    #[inline] pub fn read32(&self, off: u32) -> u32 {
        unsafe { fstart_mmio::read32((self.base + off as usize) as *const u32) }
    }
    #[inline] pub fn write32(&self, off: u32, val: u32) {
        unsafe { fstart_mmio::write32((self.base + off as usize) as *mut u32, val) }
    }
}

/// RCBA (Root Complex Base Address) MMIO accessor.
pub struct Rcba { base: usize }
impl Rcba {
    pub const fn new(base: usize) -> Self { Self { base } }
    #[inline] pub fn read32(&self, off: u32) -> u32 {
        unsafe { fstart_mmio::read32((self.base + off as usize) as *const u32) }
    }
    #[inline] pub fn write32(&self, off: u32, val: u32) {
        unsafe { fstart_mmio::write32((self.base + off as usize) as *mut u32, val) }
    }
    #[inline] pub fn read8(&self, off: u32) -> u8 {
        unsafe { fstart_mmio::read8((self.base + off as usize) as *const u8) }
    }
    #[inline] pub fn write8(&self, off: u32, val: u8) {
        unsafe { fstart_mmio::write8((self.base + off as usize) as *mut u8, val) }
    }
}

// ===================================================================
// EcamPci — ECAM-based PCI config accessor
// ===================================================================

/// PCIe Enhanced Configuration Access Mechanism (ECAM).
///
/// After the Pineview `early_init` programs PCIEXBAR via the one-time
/// legacy CF8/CFC write, **all** subsequent PCI config access goes
/// through this MMIO-based accessor. No more port I/O.
///
/// The ECAM region is 256 MiB for 256 buses (each bus = 1 MiB,
/// each device = 32 KiB, each function = 4 KiB).
pub struct EcamPci {
    base: usize,
}

impl EcamPci {
    /// Create an accessor for ECAM at the given base address.
    ///
    /// Typical Pineview value: `0xE000_0000`.
    pub const fn new(base: usize) -> Self { Self { base } }

    /// Compute the MMIO address for a PCI config register.
    #[inline]
    fn addr(&self, bus: u8, dev: u8, func: u8, reg: u16) -> usize {
        self.base
            | ((bus as usize) << 20)
            | ((dev as usize) << 15)
            | ((func as usize) << 12)
            | ((reg as usize) & 0xFFF)
    }

    /// Read a 32-bit PCI config register.
    #[inline]
    pub fn read32(&self, bus: u8, dev: u8, func: u8, reg: u16) -> u32 {
        let a = self.addr(bus, dev, func, reg);
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::read32(a as *const u32) }
    }

    /// Write a 32-bit PCI config register.
    #[inline]
    pub fn write32(&self, bus: u8, dev: u8, func: u8, reg: u16, val: u32) {
        let a = self.addr(bus, dev, func, reg);
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::write32(a as *mut u32, val) }
    }

    /// Read a 16-bit PCI config register.
    #[inline]
    pub fn read16(&self, bus: u8, dev: u8, func: u8, reg: u16) -> u16 {
        let a = self.addr(bus, dev, func, reg);
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::read16(a as *const u16) }
    }

    /// Write a 16-bit PCI config register.
    #[inline]
    pub fn write16(&self, bus: u8, dev: u8, func: u8, reg: u16, val: u16) {
        let a = self.addr(bus, dev, func, reg);
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::write16(a as *mut u16, val) }
    }

    /// Read an 8-bit PCI config register.
    #[inline]
    pub fn read8(&self, bus: u8, dev: u8, func: u8, reg: u16) -> u8 {
        let a = self.addr(bus, dev, func, reg);
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::read8(a as *const u8) }
    }

    /// Write an 8-bit PCI config register.
    #[inline]
    pub fn write8(&self, bus: u8, dev: u8, func: u8, reg: u16, val: u8) {
        let a = self.addr(bus, dev, func, reg);
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::write8(a as *mut u8, val) }
    }

    /// Read-modify-write: `reg = (reg & mask) | set`.
    #[inline]
    pub fn modify32(&self, bus: u8, dev: u8, func: u8, reg: u16, mask: u32, set: u32) {
        let v = self.read32(bus, dev, func, reg);
        self.write32(bus, dev, func, reg, (v & mask) | set);
    }

    /// OR bits into a 32-bit register.
    #[inline]
    pub fn or32(&self, bus: u8, dev: u8, func: u8, reg: u16, bits: u32) {
        self.modify32(bus, dev, func, reg, !0, bits);
    }

    /// AND bits out of an 8-bit register.
    #[inline]
    pub fn and8(&self, bus: u8, dev: u8, func: u8, reg: u16, mask: u8) {
        let v = self.read8(bus, dev, func, reg);
        self.write8(bus, dev, func, reg, v & mask);
    }

    /// OR bits into an 8-bit register.
    #[inline]
    pub fn or8(&self, bus: u8, dev: u8, func: u8, reg: u16, bits: u8) {
        let v = self.read8(bus, dev, func, reg);
        self.write8(bus, dev, func, reg, v | bits);
    }
}

// ===================================================================
// Host Bridge PCI config register offsets (bus 0, dev 0, func 0)
// ===================================================================

/// PCI config space registers for the host bridge (0:0.0).
pub mod hostbridge {
    /// EP base address register.
    pub const EPBAR: u16 = 0x40;
    /// Memory Controller Hub base address register.
    pub const MCHBAR: u16 = 0x48;
    /// GMCH Graphics Control.
    pub const GGC: u16 = 0x52;
    /// Device Enable register.
    pub const DEVEN: u16 = 0x54;
    pub const DEVEN_D0F0: u8 = 1 << 0;
    pub const DEVEN_D1F0: u8 = 1 << 1;
    pub const DEVEN_D2F0: u8 = 1 << 3;
    pub const DEVEN_D2F1: u8 = 1 << 4;
    pub const BOARD_DEVEN: u8 = DEVEN_D0F0 | DEVEN_D2F0 | DEVEN_D2F1;
    /// PCIe base address register (PCIEXBAR / ECAM).
    pub const PCIEXBAR: u16 = 0x60;
    /// DMI base address register.
    pub const DMIBAR: u16 = 0x68;
    /// Power Management I/O BAR.
    pub const PMIOBAR: u16 = 0x78;
    /// Default PM I/O base.
    pub const DEFAULT_PMIOBAR: u32 = 0x0000_0400;
    /// Programmable Attribute Map registers (unlock BIOS shadow).
    pub const PAM0: u16 = 0x90;
    pub const PAM1: u16 = 0x91;
    pub const PAM2: u16 = 0x92;
    pub const PAM3: u16 = 0x93;
    pub const PAM4: u16 = 0x94;
    pub const PAM5: u16 = 0x95;
    pub const PAM6: u16 = 0x96;
    /// System Management RAM Control.
    pub const SMRAM: u16 = 0x9d;
    /// Top of Memory.
    pub const TOM: u16 = 0xa0;
    /// Top of Upper Usable DRAM.
    pub const TOUUD: u16 = 0xa2;
    /// Top of Low Usable DRAM.
    pub const TOLUD: u16 = 0xb0;
    /// Scratchpad Data.
    pub const SKPAD: u16 = 0xdc;
    /// Capability ID.
    pub const CAPID0: u16 = 0xe0;
    /// Default ECAM base address for Pineview (64 buses).
    pub const DEFAULT_ECAM_BASE: u32 = 0xE000_0000;
}

/// PCI config space registers for the IGD (0:2.0).
pub mod igd {
    pub const GMADR: u16 = 0x18;
    pub const GTTADR: u16 = 0x1c;
    pub const BSM: u16 = 0x5c;
}

/// ICH7 southbridge PCI config constants (bus 0, various devfns).
pub mod ich7 {
    /// LPC bridge: bus 0, dev 0x1f, func 0.
    pub const LPC_DEV: u8 = 0x1f;
    pub const LPC_FUNC: u8 = 0;
    /// RCBA register in LPC config.
    pub const RCBA_REG: u16 = 0xF0;
    /// SMBus: bus 0, dev 0x1f, func 3.
    pub const SMBUS_DEV: u8 = 0x1f;
    pub const SMBUS_FUNC: u8 = 3;
    /// SMBus I/O base register.
    pub const SMB_BASE: u16 = 0x20;
    /// Host Configuration register.
    pub const HOSTC: u16 = 0x40;
    /// HOSTC enable bit.
    pub const HST_EN: u8 = 1;
    /// PCI Command register.
    pub const PCI_COMMAND: u16 = 0x04;
    /// PCI Command: I/O space enable.
    pub const PCI_CMD_IO: u16 = 0x0001;
    /// Default SMBus I/O base.
    pub const DEFAULT_SMBUS_BASE: u16 = 0x0400;
    /// GCS register in RCBA (General Control and Status).
    pub const GCS: u32 = 0x3410;
}

/// MCHBAR-relative MMIO register offsets.
///
/// Added to the MCHBAR base (typically `0xFED1_0000`) for MMIO access.
/// Used primarily by the DDR2 raminit.
pub mod mchbar {
    /// Register indexed by channel `z` (stride 0x100).
    #[inline] pub const fn gz(r: u32, z: u32) -> u32 { r + z * 0x100 }
    /// Register indexed by lane `y` (stride 4).
    #[inline] pub const fn ly(r: u32, y: u32) -> u32 { r + y * 4 }
    /// Register indexed by channel `x` (stride 0x400).
    #[inline] pub const fn cx(r: u32, x: u32) -> u32 { r + x * 0x400 }
    /// Register indexed by channel `x` and lane `y`.
    #[inline] pub const fn cxly(r: u32, x: u32, y: u32) -> u32 { x * 0x400 + r + y * 4 }

    // ---- Ported from coreboot mchbar_regs.h ----
    pub const HTPACER: u32 = 0x10;
    pub const HPWRCTL1: u32 = 0x14;
    pub const HPWRCTL2: u32 = 0x18;
    pub const HPWRCTL3: u32 = 0x1c;
    pub const HTCLKGTCTL: u32 = 0x20;
    pub const SLIMCFGTMG: u32 = 0x24;
    pub const HTBONUS0: u32 = 0x28;
    pub const HTBONUS1: u32 = 0x2c;
    pub const HIT0: u32 = 0x30;
    pub const HIT1: u32 = 0x34;
    pub const HIT2: u32 = 0x38;
    pub const HIT3: u32 = 0x3c;
    pub const HIT4: u32 = 0x40;
    pub const HIT5: u32 = 0x44;
    pub const HICLKGTCTL: u32 = 0x48;
    pub const HIBONUS: u32 = 0x4c;
    pub const XTPR0: u32 = 0x50;
    pub const XTPR1: u32 = 0x54;
    pub const XTPR2: u32 = 0x58;
    pub const XTPR3: u32 = 0x5c;
    pub const XTPR4: u32 = 0x60;
    pub const XTPR5: u32 = 0x64;
    pub const XTPR6: u32 = 0x68;
    pub const XTPR7: u32 = 0x6c;
    pub const XTPR8: u32 = 0x70;
    pub const XTPR9: u32 = 0x74;
    pub const XTPR10: u32 = 0x78;
    pub const XTPR11: u32 = 0x7c;
    pub const XTPR12: u32 = 0x80;
    pub const XTPR13: u32 = 0x84;
    pub const XTPR14: u32 = 0x88;
    pub const XTPR15: u32 = 0x8c;
    pub const FCCREQ0SET: u32 = 0x90;
    pub const FCCREQ1SET: u32 = 0x98;
    pub const FCCREQ0MSK: u32 = 0xa0;
    pub const FCCREQ1MSK: u32 = 0xa8;
    pub const FCCDATASET: u32 = 0xb0;
    pub const FCCDATAMSK: u32 = 0xb8;
    pub const FCCCTL: u32 = 0xc0;
    pub const CFGPOCTL1: u32 = 0xc8;
    pub const CFGPOCTL2: u32 = 0xcc;
    pub const NOACFGBUSCTL: u32 = 0xd0;
    pub const POC: u32 = 0xf4;
    pub const POCRL: u32 = 0xfa;
    pub const CHDECMISC: u32 = 0x111;
    pub const ZQCALQT: u32 = 0x114;
    pub const SHC2REGI: u32 = 0x115;
    pub const SHC2REGII: u32 = 0x117;
    pub const WRWMCONFIG: u32 = 0x120;
    pub const SHC2REGIII: u32 = 0x124;
    pub const SHPENDREG: u32 = 0x125;
    pub const SHPAGECTRL: u32 = 0x127;
    pub const SHCMPLWRCMD: u32 = 0x129;
    pub const SHC2MINTM: u32 = 0x12a;
    pub const SHC2IDLETM: u32 = 0x12c;
    pub const BYPACTSF: u32 = 0x12d;
    pub const BYPKNRULE: u32 = 0x12e;
    pub const SHBONUSREG: u32 = 0x12f;
    pub const COMPCTRL1: u32 = 0x130;
    pub const COMPCTRL2: u32 = 0x134;
    pub const COMPCTRL3: u32 = 0x138;
    pub const XCOMP: u32 = 0x13c;
    pub const RCMEASBUFXOVR: u32 = 0x140;
    pub const ACTXCOMP: u32 = 0x144;
    pub const FINALXRCOMPRD: u32 = 0x148;
    pub const SCOMP: u32 = 0x14c;
    pub const SCMEASBUFOVR: u32 = 0x150;
    pub const ACTSCOMP: u32 = 0x154;
    pub const FINALXSCOMP: u32 = 0x158;
    pub const XSCSTART: u32 = 0x15a;
    pub const DCOMPRAW1: u32 = 0x15c;
    pub const DCOMPRAW2: u32 = 0x160;
    pub const DCMEASBUFOVR: u32 = 0x164;
    pub const FINALDELCOMP: u32 = 0x168;
    pub const OFREQDELSEL: u32 = 0x16c;
    pub const XCOMPDFCTRL: u32 = 0x170;
    pub const ZQCALCTRL: u32 = 0x178;
    pub const XCOMPCMNBNS: u32 = 0x17a;
    pub const PSMIOVR: u32 = 0x17c;
    pub const CSHRPDCTL: u32 = 0x180;
    pub const CSPDSLVWT: u32 = 0x182;
    pub const CSHRPDSHFTOUTLO: u32 = 0x184;
    pub const CSHRFIFOCTL: u32 = 0x188;
    pub const CSHWRIOBONUS: u32 = 0x189;
    pub const CSHRPDCTL2: u32 = 0x18a;
    pub const CSHRWRIOMLNS: u32 = 0x18c;
    pub const CSHRPDCTL3: u32 = 0x18e;
    pub const CSHRPDCTL4: u32 = 0x190;
    pub const CSHWRIOBONUS2: u32 = 0x192;
    pub const CSHRMSTDYNDLLENB: u32 = 0x193;
    pub const C0TXCCCMISC: u32 = 0x194;
    pub const CSHRMSTRCTL0: u32 = 0x198;
    pub const CSHRMSTRCTL1: u32 = 0x19c;
    pub const CSHRDQSTXPGM: u32 = 0x1a0;
    pub const CSHRDQSCMN: u32 = 0x1a4;
    pub const CSHRDDR3CTL: u32 = 0x1a8;
    pub const CSHRDIGANAOBSCTL: u32 = 0x1b0;
    pub const CSHRMISCCTL: u32 = 0x1b4;
    pub const CSHRMISCCTL1: u32 = 0x1b6;
    pub const CSHRDFTCTL: u32 = 0x1b8;
    pub const MPLLCTL: u32 = 0x1c0;
    pub const MPLLDBG: u32 = 0x1c4;
    pub const CREFPI: u32 = 0x1c8;
    pub const CSHRDQSDQTX: u32 = 0x1e0;
    pub const C0DRB0: u32 = 0x200;
    pub const C0DRB1: u32 = 0x202;
    pub const C0DRB2: u32 = 0x204;
    pub const C0DRB3: u32 = 0x206;
    pub const C0DRA01: u32 = 0x208;
    pub const C0DRA23: u32 = 0x20a;
    pub const CLOCKGATINGIII: u32 = 0x210;
    pub const SHC3C4REG1: u32 = 0x212;
    pub const SHC2REG4: u32 = 0x216;
    pub const C0COREBONUS2: u32 = 0x218;
    pub const C0GNT2LNCH3: u32 = 0x21c;
    pub const C0GNT2LNCH1: u32 = 0x220;
    pub const C0GNT2LNCH2: u32 = 0x224;
    pub const C0MISCTM: u32 = 0x228;
    pub const SHCYCTRKRDWRSFLV: u32 = 0x22c;
    pub const SHCYCTRKRFSHSFLV: u32 = 0x232;
    pub const SHCYCTRKCTLLVOV: u32 = 0x234;
    pub const C0WRDPYN: u32 = 0x239;
    pub const C0C2REG: u32 = 0x23c;
    pub const C0STATRDADJV: u32 = 0x23e;
    pub const C0LATCTRL: u32 = 0x240;
    pub const C0BYPCTRL: u32 = 0x241;
    pub const C0CWBCTRL: u32 = 0x243;
    pub const C0ARBCTRL: u32 = 0x244;
    pub const C0ADDCSCTRL: u32 = 0x246;
    pub const C0STATRDCTRL: u32 = 0x248;
    pub const C0RDFIFOCTRL: u32 = 0x24c;
    pub const C0WRDATACTRL: u32 = 0x24d;
    pub const C0CYCTRKPCHG: u32 = 0x250;
    pub const C0CYCTRKACT: u32 = 0x252;
    pub const C0CYCTRKWR: u32 = 0x256;
    pub const C0CYCTRKRD: u32 = 0x258;
    pub const C0CYCTRKREFR: u32 = 0x25b;
    pub const C0CYCTRKPCHG2: u32 = 0x25d;
    pub const C0RDQCTRL: u32 = 0x25e;
    pub const C0CKECTRL: u32 = 0x260;
    pub const C0CKEDELAY: u32 = 0x264;
    pub const C0PWLRCTRL: u32 = 0x265;
    pub const C0EPCONFIG: u32 = 0x267;
    pub const C0REFRCTRL2: u32 = 0x268;
    pub const C0REFRCTRL: u32 = 0x269;
    pub const C0PVCFG: u32 = 0x26f;
    pub const C0JEDEC: u32 = 0x271;
    pub const C0ARBSPL: u32 = 0x272;
    pub const C0DYNRDCTRL: u32 = 0x274;
    pub const C0WRWMFLSH: u32 = 0x278;
    pub const C0ECCERRLOG: u32 = 0x280;
    pub const C0DITCTRL: u32 = 0x288;
    pub const C0ODTRKCTRL: u32 = 0x294;
    pub const C0ODT: u32 = 0x298;
    pub const C0ODTCTRL: u32 = 0x29c;
    pub const C0GTEW: u32 = 0x2a0;
    pub const C0GTC: u32 = 0x2a4;
    pub const C0DTPEW: u32 = 0x2a8;
    pub const C0DTAEW: u32 = 0x2ac;
    pub const C0DTC: u32 = 0x2b4;
    pub const C0REFCTRL: u32 = 0x2b8;
    pub const C0NOASEL: u32 = 0x2bf;
    pub const C0COREBONUS: u32 = 0x2c0;
    pub const C0DARBTEST: u32 = 0x2c8;
    pub const CLOCKGATINGI: u32 = 0x2d1;
    pub const MEMTDPCTW: u32 = 0x2d4;
    pub const MTDPCTWHOTTH: u32 = 0x2d8;
    pub const MTDPCTWHOTTH2: u32 = 0x2dc;
    pub const MTDPCTWHOTTH3: u32 = 0x2e0;
    pub const MTDPCTWHOTTH4: u32 = 0x2e4;
    pub const MTDPCTWAUXTH: u32 = 0x2e8;
    pub const MTDPCTWIRTH: u32 = 0x2ec;
    pub const MTDPCCRWTWHOTTH: u32 = 0x2f0;
    pub const MTDPCCRWTWHOTTH2: u32 = 0x2f4;
    pub const MTDPCCRWTWHOTTH3: u32 = 0x2f8;
    pub const MTDPCCRWTWHOTTH4: u32 = 0x2fc;
    pub const MTDPCHOTTHINT: u32 = 0x300;
    pub const MTDPCHOTTHINT2: u32 = 0x304;
    pub const MTDPCTLAUXTNTINT: u32 = 0x308;
    pub const MTDPCMISC: u32 = 0x30c;
    pub const C0RCOMPCTRL0: u32 = 0x31c;
    pub const C0RCOMPMULT0: u32 = 0x320;
    pub const C0RCOMPOVR0: u32 = 0x322;
    pub const C0RCOMPOSV0: u32 = 0x326;
    pub const C0SCOMPVREF0: u32 = 0x32a;
    pub const C0SCOMPOVR0: u32 = 0x32c;
    pub const C0SCOMPOFF0: u32 = 0x32e;
    pub const C0DCOMP0: u32 = 0x330;
    pub const C0SLEWBASE0: u32 = 0x332;
    pub const C0SLEWPULUT0: u32 = 0x334;
    pub const C0SLEWPDLUT0: u32 = 0x338;
    pub const C0DCOMPOVR0: u32 = 0x33c;
    pub const C0DCOMPOFF0: u32 = 0x340;
    pub const C0RCOMPCTRL2: u32 = 0x374;
    pub const C0RCOMPMULT2: u32 = 0x378;
    pub const C0RCOMPOVR2: u32 = 0x37a;
    pub const C0RCOMPOSV2: u32 = 0x37e;
    pub const C0SCOMPVREF2: u32 = 0x382;
    pub const C0SCOMPOVR2: u32 = 0x384;
    pub const C0SCOMPOFF2: u32 = 0x386;
    pub const C0DCOMP2: u32 = 0x388;
    pub const C0SLEWBASE2: u32 = 0x38a;
    pub const C0SLEWPULUT2: u32 = 0x38c;
    pub const C0SLEWPDLUT2: u32 = 0x390;
    pub const C0DCOMPOVR2: u32 = 0x394;
    pub const C0DCOMPOFF2: u32 = 0x398;
    pub const C0RCOMPCTRL3: u32 = 0x3a2;
    pub const C0RCOMPMULT3: u32 = 0x3a6;
    pub const C0RCOMPOVR3: u32 = 0x3a8;
    pub const C0RCOMPOSV3: u32 = 0x3ac;
    pub const C0SCOMPVREF3: u32 = 0x3b0;
    pub const C0SCOMPOVR3: u32 = 0x3b2;
    pub const C0SCOMPOFF3: u32 = 0x3b4;
    pub const C0DCOMP3: u32 = 0x3b6;
    pub const C0SLEWBASE3: u32 = 0x3b8;
    pub const C0SLEWPULUT3: u32 = 0x3ba;
    pub const C0SLEWPDLUT3: u32 = 0x3be;
    pub const C0DCOMPOVR3: u32 = 0x3c2;
    pub const C0DCOMPOFF3: u32 = 0x3c6;
    pub const C0RCOMPCTRL4: u32 = 0x3d0;
    pub const C0RCOMPMULT4: u32 = 0x3d4;
    pub const C0RCOMPOVR4: u32 = 0x3d6;
    pub const C0RCOMPOSV4: u32 = 0x3da;
    pub const C0SCOMPVREF4: u32 = 0x3de;
    pub const C0SCOMPOVR4: u32 = 0x3e0;
    pub const C0SCOMPOFF4: u32 = 0x3e2;
    pub const C0DCOMP4: u32 = 0x3e4;
    pub const C0SLEWBASE4: u32 = 0x3e6;
    pub const C0SLEWPULUT4: u32 = 0x3e8;
    pub const C0SLEWPDLUT4: u32 = 0x3ec;
    pub const C0DCOMPOVR4: u32 = 0x3f0;
    pub const C0DCOMPOFF4: u32 = 0x3f4;
    pub const C0RCOMPCTRL5: u32 = 0x3fe;
    pub const C0RCOMPMULT5: u32 = 0x402;
    pub const C0RCOMPOVR5: u32 = 0x404;
    pub const C0RCOMPOSV5: u32 = 0x408;
    pub const C0SCOMPVREF5: u32 = 0x40c;
    pub const C0SCOMPOVR5: u32 = 0x40e;
    pub const C0SCOMPOFF5: u32 = 0x410;
    pub const C0DCOMP5: u32 = 0x412;
    pub const C0SLEWBASE5: u32 = 0x414;
    pub const C0SLEWPULUT5: u32 = 0x416;
    pub const C0SLEWPDLUT5: u32 = 0x41a;
    pub const C0DCOMPOVR5: u32 = 0x41e;
    pub const C0DCOMPOFF5: u32 = 0x422;
    pub const C0RCOMPCTRL6: u32 = 0x42c;
    pub const C0RCOMPMULT6: u32 = 0x430;
    pub const C0RCOMPOVR6: u32 = 0x432;
    pub const C0RCOMPOSV6: u32 = 0x436;
    pub const C0SCOMPVREF6: u32 = 0x43a;
    pub const C0SCOMPOVR6: u32 = 0x43c;
    pub const C0SCOMPOFF6: u32 = 0x43e;
    pub const C0DCOMP6: u32 = 0x440;
    pub const C0SLEWBASE6: u32 = 0x442;
    pub const C0SLEWPULUT6: u32 = 0x444;
    pub const C0SLEWPDLUT6: u32 = 0x448;
    pub const C0DCOMPOVR6: u32 = 0x44c;
    pub const C0DCOMPOFF6: u32 = 0x450;
    pub const C0ODTRECORDX: u32 = 0x45a;
    pub const C0DQSODTRECORDX: u32 = 0x462;
    pub const XCOMPSDR0BNS: u32 = 0x4b0;
    pub const C0TXDQ0R0DLL: u32 = 0x500;
    pub const C0TXDQ0R1DLL: u32 = 0x501;
    pub const C0TXDQ0R2DLL: u32 = 0x502;
    pub const C0TXDQ0R3DLL: u32 = 0x503;
    pub const C0TXDQ1R0DLL: u32 = 0x504;
    pub const C0TXDQ1R1DLL: u32 = 0x505;
    pub const C0TXDQ1R2DLL: u32 = 0x506;
    pub const C0TXDQ1R3DLL: u32 = 0x507;
    pub const C0TXDQ2R0DLL: u32 = 0x508;
    pub const C0TXDQ2R1DLL: u32 = 0x509;
    pub const C0TXDQ2R2DLL: u32 = 0x50a;
    pub const C0TXDQ2R3DLL: u32 = 0x50b;
    pub const C0TXDQ3R0DLL: u32 = 0x50c;
    pub const C0TXDQ3R1DLL: u32 = 0x50d;
    pub const C0TXDQ3R2DLL: u32 = 0x50e;
    pub const C0TXDQ3R3DLL: u32 = 0x50f;
    pub const C0TXDQ4R0DLL: u32 = 0x510;
    pub const C0TXDQ4R1DLL: u32 = 0x511;
    pub const C0TXDQ4R2DLL: u32 = 0x512;
    pub const C0TXDQ4R3DLL: u32 = 0x513;
    pub const C0TXDQ5R0DLL: u32 = 0x514;
    pub const C0TXDQ5R1DLL: u32 = 0x515;
    pub const C0TXDQ5R2DLL: u32 = 0x516;
    pub const C0TXDQ5R3DLL: u32 = 0x517;
    pub const C0TXDQ6R0DLL: u32 = 0x518;
    pub const C0TXDQ6R1DLL: u32 = 0x519;
    pub const C0TXDQ6R2DLL: u32 = 0x51a;
    pub const C0TXDQ6R3DLL: u32 = 0x51b;
    pub const C0TXDQ7R0DLL: u32 = 0x51c;
    pub const C0TXDQ7R1DLL: u32 = 0x51d;
    pub const C0TXDQ7R2DLL: u32 = 0x51e;
    pub const C0TXDQ7R3DLL: u32 = 0x51f;
    pub const C0TXDQS0R0DLL: u32 = 0x520;
    pub const C0TXDQS0R1DLL: u32 = 0x521;
    pub const C0TXDQS0R2DLL: u32 = 0x522;
    pub const C0TXDQS0R3DLL: u32 = 0x523;
    pub const C0TXDQS1R0DLL: u32 = 0x524;
    pub const C0TXDQS1R1DLL: u32 = 0x525;
    pub const C0TXDQS1R2DLL: u32 = 0x526;
    pub const C0TXDQS1R3DLL: u32 = 0x527;
    pub const C0TXDQS2R0DLL: u32 = 0x528;
    pub const C0TXDQS2R1DLL: u32 = 0x529;
    pub const C0TXDQS2R2DLL: u32 = 0x52a;
    pub const C0TXDQS2R3DLL: u32 = 0x52b;
    pub const C0TXDQS3R0DLL: u32 = 0x52c;
    pub const C0TXDQS3R1DLL: u32 = 0x52d;
    pub const C0TXDQS3R2DLL: u32 = 0x52e;
    pub const C0TXDQS3R3DLL: u32 = 0x52f;
    pub const C0TXDQS4R0DLL: u32 = 0x530;
    pub const C0TXDQS4R1DLL: u32 = 0x531;
    pub const C0TXDQS4R2DLL: u32 = 0x532;
    pub const C0TXDQS4R3DLL: u32 = 0x533;
    pub const C0TXDQS5R0DLL: u32 = 0x534;
    pub const C0TXDQS5R1DLL: u32 = 0x535;
    pub const C0TXDQS5R2DLL: u32 = 0x536;
    pub const C0TXDQS5R3DLL: u32 = 0x537;
    pub const C0TXDQS6R0DLL: u32 = 0x538;
    pub const C0TXDQS6R1DLL: u32 = 0x539;
    pub const C0TXDQS6R2DLL: u32 = 0x53a;
    pub const C0TXDQS6R3DLL: u32 = 0x53b;
    pub const C0TXDQS7R0DLL: u32 = 0x53c;
    pub const C0TXDQS7R1DLL: u32 = 0x53d;
    pub const C0TXDQS7R2DLL: u32 = 0x53e;
    pub const C0TXDQS7R3DLL: u32 = 0x53f;
    pub const C0TXCMD0DLL: u32 = 0x580;
    pub const C0TXCK0DLL: u32 = 0x581;
    pub const C0TXCK1DLL: u32 = 0x582;
    pub const C0TXCMD1DLL: u32 = 0x583;
    pub const C0TXCTL0DLL: u32 = 0x584;
    pub const C0TXCTL1DLL: u32 = 0x585;
    pub const C0TXCTL2DLL: u32 = 0x586;
    pub const C0TXCTL3DLL: u32 = 0x587;
    pub const C0RCVMISCCTL1: u32 = 0x588;
    pub const C0RCVMISCCTL2: u32 = 0x58c;
    pub const C0MCHODTMISCCTL1: u32 = 0x590;
    pub const C0DYNSLVDLLEN: u32 = 0x592;
    pub const C0CMDTX1: u32 = 0x594;
    pub const C0CMDTX2: u32 = 0x598;
    pub const C0CTLTX2: u32 = 0x59c;
    pub const C0CKTX: u32 = 0x5a0;
    pub const C0DQSDQTX2: u32 = 0x5c4;
    pub const C0RSTCTL: u32 = 0x5d8;
    pub const C0MISCCTL: u32 = 0x5d9;
    pub const C0MISC2: u32 = 0x5da;
    pub const C0BONUS: u32 = 0x5db;
    pub const CMNDQFIFORST: u32 = 0x5dc;
    pub const C0IOBUFACTCTL: u32 = 0x5dd;
    pub const C0BONUS2: u32 = 0x5de;
    pub const C0DLLPIEN: u32 = 0x5f0;
    pub const C0COARSEDLY0: u32 = 0x5fa;
    pub const C0COARSEDLY1: u32 = 0x5fc;
    pub const SHC3C4REG2: u32 = 0x610;
    pub const SHC3C4REG3: u32 = 0x612;
    pub const SHC3C4REG4: u32 = 0x614;
    pub const SHCYCTRKCKEL: u32 = 0x62c;
    pub const SHCYCTRKACTSFLV: u32 = 0x630;
    pub const SHCYCTRKPCHGSFLV: u32 = 0x634;
    pub const C1COREBONUS: u32 = 0x6c0;
    pub const CLOCKGATINGII: u32 = 0x6d1;
    pub const CLKXSSH2MCBYPPHAS: u32 = 0x6d4;
    pub const CLKXSSH2MCBYP: u32 = 0x6d8;
    pub const CLKXSSH2MCRDQ: u32 = 0x6e0;
    pub const CLKXSSH2MCRDCST: u32 = 0x6e8;
    pub const CLKXSSMC2H: u32 = 0x6f0;
    pub const CLKXSSMC2HALT: u32 = 0x6f8;
    pub const CLKXSSH2MD: u32 = 0x700;
    pub const CLKXSSH2X2MD: u32 = 0x708;
    pub const XSBFTCTL: u32 = 0xb00;
    pub const XSBFTDRR: u32 = 0xb04;
    pub const DACGIOCTRL1: u32 = 0xb08;
    pub const CLKCFG: u32 = 0xc00;
    pub const HMCCMP: u32 = 0xc04;
    pub const HMCCMC: u32 = 0xc08;
    pub const HMPLLO: u32 = 0xc10;
    pub const CPCTL: u32 = 0xc1c;
    pub const SSKPD: u32 = 0xc20;
    pub const HMCCPEXT: u32 = 0xc28;
    pub const HMDCPEXT: u32 = 0xc2c;
    pub const CPBUP: u32 = 0xc30;
    pub const HMBYPEXT: u32 = 0xc34;
    pub const HPLLVCO: u32 = 0xc38;
    pub const HPLLMONCTLA: u32 = 0xc3c;
    pub const HPLLMONCTLB: u32 = 0xc40;
    pub const HPLLMONCTLC: u32 = 0xc44;
    pub const DPLLMONCTLA: u32 = 0xc48;
    pub const DPLLMONCTLB: u32 = 0xc4c;
    pub const HMDCMP: u32 = 0xc50;
    pub const HMBYPCP: u32 = 0xc54;
    pub const FLRCSSEL: u32 = 0xc58;
    pub const DPLLMONCTLC: u32 = 0xc5c;
    pub const MPLLMONCTLA: u32 = 0xc60;
    pub const MPLLMONCTLB: u32 = 0xc64;
    pub const MPLLMONCTLC: u32 = 0xc68;
    pub const PLLFUSEOVR1: u32 = 0xc70;
    pub const PLLFUSEOVR2: u32 = 0xc74;
    pub const GCRCSCP: u32 = 0xc80;
    pub const GCRCSCMP: u32 = 0xc84;
    pub const GCRCSBYPCP: u32 = 0xc86;
    pub const GCPLLO: u32 = 0xc88;
    pub const MCH_GCFGC: u32 = 0xc8c;
    pub const GTDPCTSHOTTH: u32 = 0xd00;
    pub const GTDPCTSHOTTH2: u32 = 0xd04;
    pub const MTDPCTSHOTTH: u32 = 0xd08;
    pub const MTDPCTSHOTTH2: u32 = 0xd0c;
    pub const TSROTDPC: u32 = 0xd10;
    pub const TSMISC: u32 = 0xd14;
    pub const TEST_MC: u32 = 0xe00;
    pub const APSMCTL: u32 = 0xe04;
    pub const DFT_STRAP1: u32 = 0xe08;
    pub const DFT_STRAP2: u32 = 0xe0c;
    pub const CFGFUSE1: u32 = 0xe10;
    pub const FUSEOVR1: u32 = 0xe1c;
    pub const FUSEOVR2: u32 = 0xe20;
    pub const FUSEOVR3: u32 = 0xe24;
    pub const FUSEOVR4: u32 = 0xe28;
    pub const NOA_RCOMP: u32 = 0xe2c;
    pub const NOAR1: u32 = 0xe30;
    pub const NOAR2: u32 = 0xe34;
    pub const NOAR3: u32 = 0xe38;
    pub const NOAR4: u32 = 0xe3c;
    pub const NOAR5: u32 = 0xe40;
    pub const NOAR6: u32 = 0xe44;
    pub const NOAR7: u32 = 0xe48;
    pub const NOAR8: u32 = 0xe4c;
    pub const NOAR9: u32 = 0xe50;
    pub const NOAR10: u32 = 0xe54;
    pub const ODOC1: u32 = 0xe58;
    pub const ODOC2: u32 = 0xe5c;
    pub const ODOSTAT: u32 = 0xe60;
    pub const ODOSTAT2: u32 = 0xe64;
    pub const ODOSTAT3: u32 = 0xe68;
    pub const DPLLMMC: u32 = 0xe6c;
    pub const CFGFUSE2: u32 = 0xe70;
    pub const FUSEOVR5: u32 = 0xe78;
    pub const NOA_LVDSCTRL: u32 = 0xe7c;
    pub const NOABUFMSK: u32 = 0xe80;
    pub const PMCFG: u32 = 0xf10;
    pub const PMSTS: u32 = 0xf14;
    pub const PMMISC: u32 = 0xf18;
    pub const GTDPCNME: u32 = 0xf20;
    pub const GTDPCTW: u32 = 0xf24;
    pub const GTDPCTW2: u32 = 0xf28;
    pub const GTDPTWHOTTH: u32 = 0xf2c;
    pub const GTDPTWHOTTH2: u32 = 0xf30;
    pub const GTDPTWHOTTH3: u32 = 0xf34;
    pub const GTDPTWHOTTH4: u32 = 0xf38;
    pub const GTDPTWAUXTH: u32 = 0xf3c;
    pub const GTDPCTWIRTH: u32 = 0xf40;
    pub const GTDPCTWIRTH2NMISC: u32 = 0xf44;
    pub const GTDPHTM: u32 = 0xf48;
    pub const GTDPHTM2: u32 = 0xf4c;
    pub const GTDPHTM3: u32 = 0xf50;
    pub const GTDPHTM4: u32 = 0xf54;
    pub const GTDPAHTMOV: u32 = 0xf58;
    pub const GTDPAHTMOV2: u32 = 0xf5c;
    pub const GTDPAHTMOV3: u32 = 0xf60;
    pub const GTDPAHTMOV4: u32 = 0xf64;
    pub const GTDPATM: u32 = 0xf68;
    pub const GTDPCGC: u32 = 0xf6c;
    pub const PCWBFC: u32 = 0xf90;
    pub const SCWBFC: u32 = 0xf98;
    pub const SBCTL: u32 = 0xfa0;
    pub const SBCTL2: u32 = 0xfa4;
    pub const PCWBPFC: u32 = 0xfa8;
    pub const SBCTL3: u32 = 0xfac;
    pub const SBCLKGATECTRL: u32 = 0xfb0;
    pub const SBBONUS0: u32 = 0xfb4;
    pub const SBBONUS1: u32 = 0xfb6;
    pub const PSMICTL: u32 = 0xfc0;
    pub const PSMIMBASE: u32 = 0xfc4;
    pub const PSMIMLIMIT: u32 = 0xfc8;
    pub const PSMIDEBUG: u32 = 0xfcc;
    pub const PSMICTL2: u32 = 0xfd0;
    pub const PSMIRPLYNOAMAP: u32 = 0xfd4;
    pub const CICGDIS: u32 = 0xff0;
    pub const CICTRL: u32 = 0xff4;
    pub const CISDCTRL: u32 = 0xff8;
    pub const CIMBSR: u32 = 0xffc;
    pub const GFXC3C4: u32 = 0x1104;
    pub const PMDSLFRC: u32 = 0x1108;
    pub const PMMSPMRES: u32 = 0x110c;
    pub const PMCLKRC: u32 = 0x1110;
    pub const PMPXPRC: u32 = 0x1114;
    pub const PMC6CTL: u32 = 0x111c;
    pub const PMICHTST: u32 = 0x1120;
    pub const PMBAK: u32 = 0x1124;
    pub const C0TXDQDQS0MISC: u32 = 0x2800;
    pub const C0TXDQDQS1MISC: u32 = 0x2804;
    pub const C0TXDQDQS2MISC: u32 = 0x2808;
    pub const C0TXDQDQS3MISC: u32 = 0x280c;
    pub const C0TXDQDQS4MISC: u32 = 0x2810;
    pub const C0TXDQDQS5MISC: u32 = 0x2814;
    pub const C0TXDQDQS6MISC: u32 = 0x2818;
    pub const C0TXDQDQS7MISC: u32 = 0x281c;
    pub const CSHRPDCTL5: u32 = 0x2c00;
    pub const CSHWRIOBONUSX: u32 = 0x2c02;
    pub const C0CALRESULT1: u32 = 0x2c04;
    pub const C0CALRESULT2: u32 = 0x2c08;
    pub const C0MODREFOFFSET1: u32 = 0x2c0c;
    pub const C0MODREFOFFSET2: u32 = 0x2c10;
    pub const C0SLVDLLOUTEN: u32 = 0x2c14;
    pub const C0DYNSLVDLLEN2: u32 = 0x2c15;
    pub const LVDSICR1: u32 = 0x3000;
    pub const LVDSICR2: u32 = 0x3004;
    pub const IOCKTRR1: u32 = 0x3008;
    pub const IOCKTRR2: u32 = 0x300c;
    pub const IOCKTRR3: u32 = 0x3010;
    pub const IOCKTSTTR: u32 = 0x3014;
    pub const IUB: u32 = 0x3800;
    pub const BIR: u32 = 0x3804;
    pub const TSC1: u32 = 0x3808;
    pub const TSC2: u32 = 0x3809;
    pub const TSS: u32 = 0x380a;
    pub const TR: u32 = 0x380b;
    pub const TSTTP: u32 = 0x380c;
    pub const TCO: u32 = 0x3812;
    pub const TST: u32 = 0x3813;
    pub const THERM1: u32 = 0x3814;
    pub const THERM3: u32 = 0x3816;
    pub const TIS: u32 = 0x381a;
    pub const TERRCMD: u32 = 0x3820;
    pub const TSMICMD: u32 = 0x3821;
    pub const TSCICMD: u32 = 0x3822;
    pub const TSC3: u32 = 0x3824;
    pub const EXTTSCS: u32 = 0x3825;
    pub const C0THRMSTS: u32 = 0x3830;
}
