//! Intel i945 (Lakeport) northbridge driver.
//!
//! Ported from coreboot `northbridge/intel/i945/early_init.c`,
//! `memmap.c`, `errata.c`, and the BAR/topology halves of `romstage.c`.
//! Covers the desktop 82945G/GZ/GC/P/PL family; the mobile 945GM/PM path
//! (PCIe x16 link training, GM errata) is ported but gated on
//! [`I945Variant::Mobile`] since only the desktop GC has a board port.
//!
//! Responsibilities, in `romstage.c` order:
//!
//! - enable ECAM by programming PCIEXBAR through legacy CF8/CFC;
//! - map EPBAR, MCHBAR, DMIBAR and X60BAR;
//! - program GGC (UMA size), TSEG (ESMRAMC) and the PAM shadow ranges;
//! - set up the egress port, DMI (both MCH and ICH7 RCBA sides) and root
//!   complex topology;
//! - train the mobile PCIe x16 link (mobile variant only).
//!
//! DRAM init lives in [`raminit`]. All PCI config access goes through ECAM
//! MMIO once [`IntelI945::pre_console_init`] has enabled PCIEXBAR.

pub mod raminit;

use core::{cell::UnsafeCell, ptr};

use fstart_arch::mp::{SmmError, SmmInfo};

use fstart_core::mmio::MmioReadWrite;
use fstart_core::services::memory_detect::{
    E820Entry, E820Kind, MemoryDetector, build_pc_compatible_e820,
};
use fstart_core::services::{MemoryController, ServiceError};
use fstart_pci::ecam;
use fstart_pci::{PciRootError, PciRootInfo, PciRootProvider, PciRootWindows, PciWindow, PciWindowKind};
use serde::Serialize;
use tock_registers::interfaces::Readable;
use tock_registers::{register_bitfields, register_structs};

/// i945 host bridge (D0:F0) PCI configuration offsets.
pub mod hostbridge {
    /// Host bridge: bus 0, device 0, function 0.
    pub const HOST_DEV: u8 = 0;
    pub const HOST_FUNC: u8 = 0;

    pub const EPBAR: u16 = 0x40;
    pub const MCHBAR: u16 = 0x44;
    pub const PCIEXBAR: u16 = 0x48;
    pub const DMIBAR: u16 = 0x4c;
    pub const X60BAR: u16 = 0x60;

    /// GMCH graphics control (16-bit write in coreboot).
    pub const GGC: u16 = 0x52;
    pub const DEVEN: u16 = 0x54;
    pub const DEVEN_D0F0: u16 = 1 << 0;
    pub const DEVEN_D1F0: u16 = 1 << 1;
    pub const DEVEN_D2F0: u16 = 1 << 3;
    pub const DEVEN_D2F1: u16 = 1 << 4;

    pub const PAM0: u16 = 0x90;
    pub const TOLUD: u16 = 0x9c;
    pub const SMRAM: u16 = 0x9d;
    pub const ESMRAMC: u16 = 0x9e;
    pub const TOM: u16 = 0xa0;
    pub const SKPAD: u16 = 0xdc;

    /// PCI device IDs used by `early_initialization` chipset detection.
    pub const DID_DESKTOP: u32 = 0x2770_8086;
    pub const DID_MOBILE_A: u32 = 0x27a0_8086;
    pub const DID_MOBILE_B: u32 = 0x27ac_8086;

    pub const PEG_DEV: u8 = 1;
    pub const PEG_FUNC: u8 = 0;
    pub const IGD_DEV: u8 = 2;
    pub const IGD_FUNC: u8 = 0;
    pub const IGD_BSM: u16 = 0x5c;
    /// Graphics clock frequency and gating control.
    pub const IGD_GCFC: u16 = 0xf0;

    /// PEG (D0:F1) extended offsets from `i945.h`.
    pub mod peg {
        pub const SLOTSTS: u16 = 0xba;
        pub const SLOTCAP: u16 = 0xb4;
        pub const PEG_CAP: u16 = 0xa2;
        pub const PEG_LC: u16 = 0xec;
        pub const PEGTC: u16 = 0x204;
        pub const PEGSTS: u16 = 0x214;
        pub const PEGCC: u16 = 0x208;
        pub const LE1D: u16 = 0x150;
        pub const LE1A: u16 = 0x158;
        pub const VC0RCTL: u16 = 0x114;
        pub const PVCCAP1: u16 = 0x104;
    }
}

/// i945 MCHBAR register offsets from `i945.h`.
pub mod mchbar {
    pub const CLKCFG: u32 = 0x0c00;
    pub const UPMC1: u32 = 0x0c14;
    pub const SSKPD: u32 = 0x0c1c;
    pub const DFT_STRAP1: u32 = 0x0e08;
    pub const FSBSNPCTL: u32 = 0x0048;
    pub const FSBPMC3: u32 = 0x0040;
    pub const SLFRCS: u32 = 0x0f14;
    pub const MMARB1: u32 = 0x0224;
}

/// i945 EPBAR register offsets from `i945.h`.
pub mod epbar {
    pub const EPVC0RCTL: u32 = 0x014;
    pub const EPPVCCAP1: u32 = 0x004;
    pub const EPVC1RCAP: u32 = 0x01c;
    pub const EPVC1RCTL: u32 = 0x020;
    pub const EPVC1RSTS: u32 = 0x026;
    pub const EPVC1MTS: u32 = 0x028;
    pub const EPVC1IST_LO: u32 = 0x038;
    pub const EPVC1IST_HI: u32 = 0x03c;
    pub const EPESD: u32 = 0x044;
    pub const EPLE1D: u32 = 0x050;
    pub const EPLE1A: u32 = 0x058;
    pub const EPLE2D: u32 = 0x060;
    pub const PORTARB: u32 = 0x100;
    /// VC1 resource control register at EPBAR+0x2c (unnamed in `i945.h`).
    pub const VC1_MISC: u32 = 0x02c;
}

/// i945 DMIBAR register offsets from `i945.h`.
pub mod dmibar {
    pub const DMIVC0RCTL0: u32 = 0x014;
    pub const DMIPVCCAP1: u32 = 0x004;
    pub const DMIVC1RCTL: u32 = 0x020;
    pub const DMIVC1RSTS: u32 = 0x026;
    pub const DMILE1D: u32 = 0x050;
    pub const DMILE1A: u32 = 0x058;
    pub const DMILE2D: u32 = 0x060;
    pub const DMILE2A: u32 = 0x068;
    pub const DMILCAP: u32 = 0x084;
    pub const DMILCTL: u32 = 0x088;
    pub const DMICC: u32 = 0x208;
    pub const DMICTL1: u32 = 0x0f0;
    pub const DMICTL2: u32 = 0x0fc;
    pub const DMI_UNCERRSTS: u32 = 0x1c4;
    pub const DMI_CORERRSTS: u32 = 0x1d0;
    pub const DMI_MISC_200: u32 = 0x200;
    pub const DMI_MISC_204: u32 = 0x204;
    pub const DMI_MISC_224: u32 = 0x224;
    pub const DMI_MISC_228: u32 = 0x228;
    pub const DMI_MISC_2C: u32 = 0x02c;
    pub const DMI_MISC_32: u32 = 0x032;
    pub const DMIDRCCFG: u32 = 0xeb4;
}

/// ICH7 RCBA offsets touched by the i945 northbridge flow.
///
/// These live in `southbridge/intel/i82801gx/i82801gx.h`; they are repeated
/// here because the DMI/topology setup in coreboot's `i945/early_init.c`
/// programs both sides of the link from the northbridge flow.
pub mod rcba {
    pub const V0CTL: u32 = 0x0014;
    pub const V1CAP: u32 = 0x001c;
    pub const V1CTL: u32 = 0x0020;
    pub const ESD: u32 = 0x0104;
    pub const ULD: u32 = 0x0110;
    pub const ULBA: u32 = 0x0118;
    pub const RP1D: u32 = 0x0120;
    pub const RP2D: u32 = 0x0130;
    pub const RP3D: u32 = 0x0140;
    pub const RP4D: u32 = 0x0150;
    pub const HDD: u32 = 0x0160;
    pub const RP5D: u32 = 0x0170;
    pub const RP6D: u32 = 0x0180;
    pub const LCAP: u32 = 0x01a4;
    pub const LCTL: u32 = 0x01a8;
    pub const MISC_2010: u32 = 0x2010;
    pub const GCS: u32 = 0x3410;
    pub const CG: u32 = 0x341c;
}

/// i945 silicon stepping target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum I945Variant {
    /// 82945G/GZ/P/PL desktop parts.
    Desktop,
    /// 82945GC (D945GCLF): desktop with the extra egress-port cases.
    DesktopGc,
    /// 945GM/PM/GMS/GU/GT mobile parts: PCIe x16 training + GM errata.
    Mobile,
}

/// Closed i945 chipset policy consumed by the fixed stage flow.
///
/// Board-attached devices stay in board hooks/code; fixed chipset windows
/// (MCHBAR/DMIBAR/EPBAR/RCBA/SMBus base) are platform constants.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntelI945Config {
    /// MCHBAR base address (`0xFED14000`).
    pub mchbar: u64,
    /// DMIBAR base address (`0xFED18000`).
    pub dmibar: u64,
    /// EPBAR base address (`0xFED19000`).
    pub epbar: u64,
    /// ECAM (PCIEXBAR) base address. Default: `0xF0000000`.
    #[serde(default = "default_ecam_base")]
    pub ecam_base: u64,
    /// Number of buses decoded by PCIEXBAR (256, 128, or 64).
    #[serde(default = "default_ecam_buses")]
    pub ecam_buses: u16,
    /// ICH7 RCBA base programmed by the southbridge driver.
    pub rcba: u64,
    /// Chipset variant selecting GC/mobile code paths.
    pub variant: I945Variant,
    /// Graphics Mode Select: UMA size index into
    /// `{0, 1, 4, 8, 16, 32, 48, 64} MiB`. Default 4 (16 MiB).
    #[serde(default = "default_gfx_gms")]
    pub gfx_gms: u8,
    /// PCI MMIO window in MiB reserved below 4 GiB when programming TOLUD.
    /// coreboot refuses sizes below 768 MiB; default matches D945GCLF (768).
    #[serde(default = "default_pci_mmio_size")]
    pub pci_mmio_size: u32,
    /// SMBus I/O base used for DIMM SPD probing during raminit.
    #[serde(default = "default_smbus_base")]
    pub smbus_base: u16,
    /// SPD EEPROM addresses in i945 slot order: ch0 (2 slots), ch1 (2 slots).
    #[serde(default = "default_spd_addresses")]
    pub spd_addresses: [u8; 4],
}

impl IntelI945Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mchbar: 0xFED1_4000,
            dmibar: 0xFED1_8000,
            epbar: 0xFED1_9000,
            ecam_base: default_ecam_base(),
            ecam_buses: default_ecam_buses(),
            rcba: 0xFED1_C000,
            variant: I945Variant::DesktopGc,
            gfx_gms: default_gfx_gms(),
            pci_mmio_size: default_pci_mmio_size(),
            smbus_base: default_smbus_base(),
            spd_addresses: default_spd_addresses(),
        }
    }
}

const fn default_ecam_base() -> u64 {
    0xF000_0000
}

const fn default_ecam_buses() -> u16 {
    64
}

const fn default_gfx_gms() -> u8 {
    4
}

const fn default_pci_mmio_size() -> u32 {
    768
}

const fn default_smbus_base() -> u16 {
    0x0400
}

const fn default_spd_addresses() -> [u8; 4] {
    [0x50, 0x51, 0x52, 0x53]
}

/// Graphics Mode Select to UMA size in KiB (`ggc2uma` from `memmap.c`).
#[must_use]
pub const fn decode_igd_memory_size_kb(gms: u32) -> u32 {
    const GGC2UMA_KB: [u32; 8] = [0, 1024, 4096, 8192, 16384, 32768, 49152, 65536];
    if gms >= GGC2UMA_KB.len() as u32 {
        panic!("i945: bad Graphics Mode Select (GMS) setting");
    }
    GGC2UMA_KB[gms as usize]
}

/// ESMRAMC TSEG size decode in bytes (`decode_tseg_size` from `memmap.c`).
#[must_use]
pub const fn decode_tseg_size(esmramc: u8) -> u32 {
    if esmramc & 1 == 0 {
        return 0;
    }
    match (esmramc >> 1) & 3 {
        0 => 1 << 20,
        1 => 2 << 20,
        2 => 8 << 20,
        _ => panic!("i945: bad TSEG setting"),
    }
}

register_structs! {
    /// Sparse typed overlay for the early i945 host-bridge registers.
    pub I945HostBridgePciConfig {
        (0x00 => _pad0: [u8; 0x52]),
        (0x52 => pub ggc: MmioReadWrite<u16, GGC_REG::Register>),
        (0x54 => pub deven: MmioReadWrite<u16, DEVEN_REG::Register>),
        (0x56 => _pad1: [u8; 0xa0 - 0x56]),
        (0xa0 => pub tom: MmioReadWrite<u16, TOM_REG::Register>),
        (0xa2 => @END),
    }
}

register_bitfields! [u16,
    GGC_REG [
        GMS OFFSET(4) NUMBITS(3) [],
        RAW OFFSET(0) NUMBITS(16) []
    ],
    DEVEN_REG [
        D0F0 OFFSET(0) NUMBITS(1) [],
        D1F0 OFFSET(1) NUMBITS(1) [],
        D2F0 OFFSET(3) NUMBITS(1) [],
        D2F1 OFFSET(4) NUMBITS(1) [],
        RAW OFFSET(0) NUMBITS(16) []
    ],
    TOM_REG [
        RAW OFFSET(0) NUMBITS(16) []
    ]
];

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
    pub fn read8(&self, off: u32) -> u8 {
        // SAFETY: off is an i945 MCHBAR register offset.
        unsafe { fstart_core::mmio::read8((self.base + off as usize) as *const u8) }
    }

    #[inline]
    pub fn write8(&self, off: u32, val: u8) {
        // SAFETY: off is an i945 MCHBAR register offset.
        unsafe { fstart_core::mmio::write8((self.base + off as usize) as *mut u8, val) }
    }

    #[inline]
    pub fn read16(&self, off: u32) -> u16 {
        // SAFETY: off is an i945 MCHBAR register offset.
        unsafe { fstart_core::mmio::read16((self.base + off as usize) as *const u16) }
    }

    #[inline]
    pub fn write16(&self, off: u32, val: u16) {
        // SAFETY: off is an i945 MCHBAR register offset.
        unsafe { fstart_core::mmio::write16((self.base + off as usize) as *mut u16, val) }
    }

    #[inline]
    pub fn read32(&self, off: u32) -> u32 {
        // SAFETY: off is an i945 MCHBAR register offset.
        unsafe { fstart_core::mmio::read32((self.base + off as usize) as *const u32) }
    }

    #[inline]
    pub fn write32(&self, off: u32, val: u32) {
        // SAFETY: off is an i945 MCHBAR register offset.
        unsafe { fstart_core::mmio::write32((self.base + off as usize) as *mut u32, val) }
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
}

macro_rules! bar_accessor {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Clone, Copy)]
        pub struct $name {
            base: usize,
        }

        impl $name {
            pub const fn new(base: usize) -> Self {
                Self { base }
            }

            #[inline]
            pub fn read8(&self, off: u32) -> u8 {
                // SAFETY: off is a register offset in this BAR.
                unsafe { fstart_core::mmio::read8((self.base + off as usize) as *const u8) }
            }

            #[inline]
            pub fn write8(&self, off: u32, val: u8) {
                // SAFETY: off is a register offset in this BAR.
                unsafe { fstart_core::mmio::write8((self.base + off as usize) as *mut u8, val) }
            }

            #[inline]
            pub fn read16(&self, off: u32) -> u16 {
                // SAFETY: off is a register offset in this BAR.
                unsafe { fstart_core::mmio::read16((self.base + off as usize) as *const u16) }
            }

            #[inline]
            pub fn write16(&self, off: u32, val: u16) {
                // SAFETY: off is a register offset in this BAR.
                unsafe { fstart_core::mmio::write16((self.base + off as usize) as *mut u16, val) }
            }

            #[inline]
            pub fn read32(&self, off: u32) -> u32 {
                // SAFETY: off is a register offset in this BAR.
                unsafe { fstart_core::mmio::read32((self.base + off as usize) as *const u32) }
            }

            #[inline]
            pub fn write32(&self, off: u32, val: u32) {
                // SAFETY: off is a register offset in this BAR.
                unsafe { fstart_core::mmio::write32((self.base + off as usize) as *mut u32, val) }
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
        }
    };
}

bar_accessor!(EpBar, "Thin EPBAR accessor.");
bar_accessor!(DmiBar, "Thin DMIBAR accessor.");
bar_accessor!(RcbaBar, "Thin RCBA accessor for the MCH-side DMI/topology setup.");

/// SMRAM control bits (shared with GM965; same register layout).
const SMRAM_G_SMRAME: u8 = 1 << 3;
const SMRAM_D_LCK: u8 = 1 << 4;
const SMRAM_D_OPEN: u8 = 1 << 6;
const SMRAM_C_BASE_SEG: u8 = 0b010;
/// ICH7 PMBASE programmed by the southbridge driver; must match the
/// platform's `ICH7_PMBASE` for SMM setup.
const ICH7_PM_BASE: u16 = 0x0500;
const EM64T101_SAVE_STATE_SIZE: usize = 0x400;

const ZERO_CPU_LAYOUT: fstart_smm::CpuSmmLayout = fstart_smm::CpuSmmLayout {
    smbase: 0,
    entry_addr: 0,
    save_state_base: 0,
    save_state_top: 0,
    stack_bottom: 0,
    stack_top: 0,
};

struct CpuLayoutStore(UnsafeCell<[fstart_smm::CpuSmmLayout; fstart_smm::runtime::MAX_SMM_CPUS]>);

// SAFETY: firmware invokes SMM installation from the BSP while SMRAM is open;
// this scratch buffer is not shared with APs or interrupt context.
unsafe impl Sync for CpuLayoutStore {}

static I945_SMM_CPU_LAYOUTS: CpuLayoutStore = CpuLayoutStore(UnsafeCell::new(
    [ZERO_CPU_LAYOUT; fstart_smm::runtime::MAX_SMM_CPUS],
));

/// Intel i945 northbridge driver.
pub struct IntelI945 {
    config: &'static IntelI945Config,
    detected_size: u64,
    boot_path: crate::BootPath,
    /// PCI mmio32 window derived from the e820 map after memory detection.
    mmio32_window: Option<(u64, u64)>,
}

/// CF9 full reset, mirroring coreboot `full_reset()`.
#[cfg(target_arch = "x86_64")]
pub(crate) fn cf9_reset() -> ! {
    // SAFETY: I/O port 0xcf9 is the standard Intel reset control register.
    unsafe {
        fstart_core::pio::outb(0xcf9, 0x06);
        fstart_core::pio::outb(0xcf9, 0x0e);
    }
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn cf9_reset() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

impl IntelI945 {
    fn hb() -> ecam::EcamDevice {
        ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC)
    }

    fn hostbridge_regs() -> &'static I945HostBridgePciConfig {
        // SAFETY: i945 host bridge is fixed at 00:00.0 and ECAM is live
        // before callers use the overlay.
        unsafe { Self::hb().regs::<I945HostBridgePciConfig>() }
    }

    fn mchbar(&self) -> MchBar {
        MchBar::new(self.config.mchbar as usize)
    }

    fn dmibar(&self) -> DmiBar {
        DmiBar::new(self.config.dmibar as usize)
    }

    fn epbar(&self) -> EpBar {
        EpBar::new(self.config.epbar as usize)
    }

    fn rcba(&self) -> RcbaBar {
        RcbaBar::new(self.config.rcba as usize)
    }

    fn pciexbar_length_bits(&self) -> u32 {
        match self.config.ecam_buses {
            256 => 0 << 1,
            128 => 1 << 1,
            _ => 2 << 1,
        }
    }

    /// i945 silicon stepping (`i945_silicon_revision`).
    #[must_use]
    pub fn silicon_revision() -> u8 {
        Self::hb().read8(0x08)
    }

    #[cfg(target_arch = "x86_64")]
    fn enable_ecam(&self) {
        let value = (self.config.ecam_base as u32) | self.pciexbar_length_bits() | 1;
        // SAFETY: one-time legacy PCI config write to enable ECAM before the
        // ECAM MMIO accessor can be used.
        unsafe {
            fstart_core::pio::pci_cfg_write32(
                0,
                hostbridge::HOST_DEV,
                hostbridge::HOST_FUNC,
                hostbridge::PCIEXBAR as u8,
                value,
            );
        }
        ecam::init(self.config.ecam_base as usize);
        fstart_log::info!("i945: ECAM enabled at {:#x}", self.config.ecam_base);
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn enable_ecam(&self) {
        ecam::init(self.config.ecam_base as usize);
        fstart_log::info!("i945: ECAM enable (stub, non-x86)");
    }

    /// Program EPBAR/MCHBAR/DMIBAR/X60BAR, GGC, TSEG and PAM
    /// (`i945_setup_bars`).
    fn setup_bars(&self) {
        let hb = Self::hb();

        if Self::silicon_revision() == 0 {
            fstart_log::info!("i945: warning: silicon revision A0 might not work correctly");
        }

        hb.write32(hostbridge::EPBAR, (self.config.epbar as u32) | 1);
        hb.write32(hostbridge::MCHBAR, (self.config.mchbar as u32) | 1);
        hb.write32(hostbridge::DMIBAR, (self.config.dmibar as u32) | 1);
        hb.write32(hostbridge::X60BAR, 0xfed1_3000 | 1);

        // UMA size from board config (GMS field); TSEG 2M (covered by SMRR
        // MTRRs, which require TSEG_BASE aligned to TSEG_SIZE).
        let gms = u16::from(self.config.gfx_gms & 7);
        hb.write16(hostbridge::GGC, gms << 4);
        hb.and8_or8(
            hostbridge::ESMRAMC,
            !0x07,
            (1 << 1) | (1 << 0),
        );

        // C0000-FFFFF RAM on both reads and writes.
        hb.write8(hostbridge::PAM0, 0x30);
        for pam in hostbridge::PAM0 + 1..=hostbridge::PAM0 + 6 {
            hb.write8(pam, 0x33);
        }

        // Wait for MCHBAR to come up (CAPID0 bit 49 clear path).
        if hb.read32(0xe4) & 0x0002_0000 == 0 {
            while self.mchbar().read8(0) & 0x80 == 0 {
                core::hint::spin_loop();
            }
        }
        fstart_log::info!("i945: static northbridge registers done");
    }

    /// Egress port VC setup (`i945_setup_egress_port`).
    fn setup_egress_port(&self) {
        let ep = self.epbar();
        let mch = self.mchbar();

        ep.clrsetbits32(epbar::EPVC0RCTL, 0xffff_ff00, 0);
        ep.clrsetbits32(epbar::EPPVCCAP1, 7, 1);

        let clkcfg = mch.read32(mchbar::CLKCFG) & 7;
        let mut misc = ep.read32(epbar::VC1_MISC) & 0xffff_ff00;
        match self.config.variant {
            I945Variant::DesktopGc if clkcfg == 0 => misc |= 0x1a,
            _ => {}
        }
        match clkcfg {
            1 => misc |= 0x0d,
            2 => misc |= 0x14,
            3 => misc |= 0x10,
            _ => {}
        }
        ep.write32(epbar::VC1_MISC, misc);

        ep.write32(epbar::EPVC1MTS, 0x0a0a_0a0a);
        ep.clrsetbits32(epbar::EPVC1RCAP, 0x7f << 16, 0x0a << 16);

        let ist = match (self.config.variant, clkcfg) {
            (I945Variant::DesktopGc, 0) => Some(0x0138_0138),
            (_, 1) => Some(0x009c_009c),
            (_, 2) => Some(0x00f0_00f0),
            (_, 3) => Some(0x00c0_00c0),
            _ => None,
        };
        if let Some(v) = ist {
            ep.write32(epbar::EPVC1IST_LO, v);
            ep.write32(epbar::EPVC1IST_HI, v);
        }

        // Internal graphics enabled: allow isochronous traffic.
        if Self::hb().read8(hostbridge::DEVEN)
            & (hostbridge::DEVEN_D2F0 | hostbridge::DEVEN_D2F1) as u8
            != 0
        {
            mch.setbits32(mchbar::MMARB1, 1 << 17);
        }

        ep.clrsetbits32(epbar::EPVC1RCTL, 7 << 24, 1 << 24);
        ep.clrsetbits32(epbar::EPVC1RCTL, 0xffff_ff00, 1 << 7);

        ep.write32(epbar::PORTARB, 0x0100_0001);
        ep.write32(epbar::PORTARB + 0x04, 0x0004_0000);
        ep.write32(epbar::PORTARB + 0x08, 0x0000_1000);
        ep.write32(epbar::PORTARB + 0x0c, 0x0000_0040);
        ep.write32(epbar::PORTARB + 0x10, 0x0100_0001);
        ep.write32(epbar::PORTARB + 0x14, 0x0004_0000);
        ep.write32(epbar::PORTARB + 0x18, 0x0000_1000);
        ep.write32(epbar::PORTARB + 0x1c, 0x0000_0040);

        ep.setbits32(epbar::EPVC1RCTL, 1 << 16);
        ep.setbits32(epbar::EPVC1RCTL, 1 << 16);

        let mut timeout = 0x7fff_ffu32;
        while ep.read16(epbar::EPVC1RSTS) & 1 != 0 && timeout != 0 {
            timeout -= 1;
        }
        if timeout == 0 {
            fstart_log::error!("i945: port arbitration table load timeout");
        }

        ep.setbits32(epbar::EPVC1RCTL, 1 << 31);
        timeout = 0x7fff;
        while ep.read16(epbar::EPVC1RSTS) & (1 << 1) != 0 && timeout != 0 {
            timeout -= 1;
        }
        if timeout == 0 {
            fstart_log::error!("i945: VC1 negotiation timeout");
        }
    }

    /// ICH7 side of the DMI RCRB setup (`ich7_setup_dmi_rcrb`).
    fn ich7_setup_dmi_rcrb(&self) {
        let rcba = self.rcba();
        let lctl = rcba.read16(rcba::LCTL);
        rcba.write16(rcba::LCTL, (lctl & !3) | 3);
        rcba.write32(rcba::V0CTL, 0x8000_0001);
        rcba.write32(rcba::V1CAP, 0x0312_8010);

        for (dev, func) in [(0x1c, 0), (0x1c, 4), (0x1c, 5)] {
            let rp = ecam::EcamDevice::new(0, dev, func);
            rp.write16(0x42, 0x0141);
        }
        ecam::EcamDevice::new(0, 0x1c, 4).write32(0x54, 0x0048_0ce0);
        ecam::EcamDevice::new(0, 0x1c, 5).write32(0x54, 0x0050_0ce0);

        let mut v1ctl = rcba.read32(rcba::V1CTL);
        v1ctl &= !((0x7f << 1) | (7 << 17) | (7 << 24));
        v1ctl |= (0x40 << 1) | (4 << 17) | (1 << 24) | (1 << 31);
        rcba.write32(rcba::V1CTL, v1ctl);
        rcba.setbits32(rcba::LCAP, 3 << 10);
    }

    /// MCH side of the DMI RCRB setup (`i945_setup_dmi_rcrb`).
    fn setup_dmi_rcrb(&self) {
        let dmi = self.dmibar();
        let mch = self.mchbar();

        dmi.clrsetbits32(dmibar::DMIVC0RCTL0, 0xffff_ff00, 0);
        dmi.clrsetbits32(dmibar::DMIPVCCAP1, 7, 1);
        // VC ID 1 must match the ICH7 side.
        dmi.clrsetbits32(dmibar::DMIVC1RCTL, 7 << 24, 1 << 24);
        dmi.clrsetbits32(dmibar::DMIVC1RCTL, 0xffff_ff00, 1 << 7);
        dmi.setbits32(dmibar::DMIVC1RCTL, 1 << 31);

        let mut timeout = 0x7_ffffu32;
        while dmi.read16(dmibar::DMIVC1RSTS) & (1 << 1) != 0 && timeout != 0 {
            timeout -= 1;
        }
        if timeout == 0 {
            fstart_log::error!("i945: DMI VC1 negotiation timeout");
        }

        // ASPM L0.
        dmi.clrsetbits32(dmibar::DMILCAP, 7 << 12, 2 << 12);
        dmi.clrsetbits32(dmibar::DMILCAP, 7 << 15, 2 << 15);
        let mut cc = dmi.read32(dmibar::DMICC) & 0x00ff_ffff;
        cc &= !3;
        cc |= 1 << 0;
        cc &= !(3 << 20);
        cc |= 1 << 20;
        dmi.write32(dmibar::DMICC, cc);
        dmi.setbits32(dmibar::DMILCTL, 3);

        let snp = (mch.read32(mchbar::FSBSNPCTL) & !(0xff << 2)) | (0xaa << 2);
        mch.write32(mchbar::FSBSNPCTL, snp);
        dmi.write32(dmibar::DMI_MISC_2C, 0x8600_0040);
        // x4 DMI.
        dmi.clrsetbits32(dmibar::DMI_MISC_204, 0x3ff, 0x13f);

        if Self::hb().read8(hostbridge::DEVEN)
            & (hostbridge::DEVEN_D2F0 | hostbridge::DEVEN_D2F1) as u8
            != 0
        {
            dmi.setbits32(dmibar::DMI_MISC_200, 1 << 21);
        } else {
            dmi.clrbits32(dmibar::DMI_MISC_200, 1 << 21);
        }
        dmi.clrbits32(dmibar::DMI_MISC_204, (1 << 11) | (1 << 10));
        dmi.clrsetbits32(dmibar::DMI_MISC_204, 0xff << 12, 0x0d << 12);
        dmi.setbits32(dmibar::DMICTL1, 3 << 24);
        dmi.clrsetbits32(dmibar::DMI_MISC_200, 3 << 26, 2 << 26);
        dmi.clrbits32(dmibar::DMIDRCCFG, 1 << 31);
        dmi.setbits32(dmibar::DMICTL2, 1 << 31);

        if Self::silicon_revision() >= 3 {
            for off in [0xec0, 0xed4, 0xee8, 0xefc] {
                dmi.clrsetbits32(off, 0xf << 28, 2 << 28);
            }
        }

        timeout = 0x7fff_ff;
        while dmi.read8(dmibar::DMI_MISC_32) & (1 << 1) != 0 && timeout != 0 {
            timeout -= 1;
        }
        if timeout == 0 {
            fstart_log::error!("i945: DMI hardware timeout");
        }

        dmi.write32(dmibar::DMI_UNCERRSTS, 0xffff_ffff);
        dmi.write32(dmibar::DMI_CORERRSTS, 0xffff_ffff);
        dmi.write32(dmibar::DMI_MISC_228, 0xffff_ffff);

        // Program read-only write-once registers.
        for off in [0x308, 0x314, 0x324, 0x328, 0x334, 0x338] {
            dmi.setbits32(off, 0);
        }

        if Self::silicon_revision() == 1 && mch.read8(mchbar::DFT_STRAP1) & (1 << 5) != 0
            && mch.read32(0x214) & 0xf != 0x3
        {
            fstart_log::error!("i945: DMI link requires A1 stepping workaround; rebooting");
            dmi.clrsetbits32(dmibar::DMI_MISC_224, 7, 3);
            cf9_reset();
        }
    }

    /// Mobile PCIe x16 link training (`i945_setup_pci_express_x16`).
    ///
    /// Desktop boards (including D945GCLF, whose devicetree disables D1:F0)
    /// never call this.
    fn setup_pci_express_x16(&self) {
        use hostbridge::peg::*;
        let p2peg = ecam::EcamDevice::new(0, hostbridge::PEG_DEV, hostbridge::PEG_FUNC);
        let mch = self.mchbar();

        p2peg.or16(hostbridge::DEVEN, hostbridge::DEVEN_D1F0);
        p2peg.and32(PEGCC, !(1 << 8));

        // Force PCIRST# via secondary bus reset.
        p2peg.or8(0x3e, 1 << 6);
        p2peg.and8_or8(0x3e, !(1 << 6), 0);

        let slotsts = p2peg.read16(SLOTSTS);
        if slotsts & 0x48 == 0 {
            self.disable_pciexpress_x16_link();
            return;
        }
        p2peg.write16(SLOTSTS, slotsts | (1 << 4) | (1 << 0));
        // Temporary bus number for link probing.
        p2peg.and8_or8(0x19, 0, 0x0a);
        p2peg.and32(0x224, !(1 << 8));
        mch.clrbits16(mchbar::UPMC1, (1 << 5) | (1 << 0));
        p2peg.or16(PEG_CAP, 1 << 8);

        // SLOTCAP becomes read-only after the first write.
        let slotcap = (p2peg.read32(SLOTCAP) & 0x0007_ffff) & 0xfffe_007f;
        p2peg.write32(SLOTCAP, slotcap);

        let mut timeout = 0x7_ffffu32;
        while (p2peg.read32(PEGSTS) >> 16) & 3 != 3 && timeout != 0 {
            timeout -= 1;
        }
        let peg_plugin = ecam::EcamDevice::new(0x0a, 0, 0);
        let mut id = peg_plugin.read32(0x00);
        if (id == 0 || id == 0xffff_ffff) && timeout != 0 {
            // First training attempt raced; retry at x1 before giving up.
            p2peg.modify32(PEGSTS, !(0xf << 1), 1);
            p2peg.or8(0x3e, 1 << 6);
            p2peg.and8_or8(0x3e, !(1 << 6), 0);
            timeout = 0x7_ffff;
            while (p2peg.read32(PEGSTS) >> 16) & 3 != 3 && timeout != 0 {
                timeout -= 1;
            }
            id = peg_plugin.read32(0x00);
        }
        if id == 0 || id == 0xffff_ffff {
            self.disable_pciexpress_x16_link();
            return;
        }

        let width = (p2peg.read16(0xb2) >> 4) & 0x3f;
        fstart_log::info!("i945: PCIe x{} link training succeeded", width);

        if peg_plugin.read32(0x08) >> 8 == 0x0300_00 {
            fstart_log::info!("i945: PCIe device is VGA; disabling IGD");
            Self::hb().write16(hostbridge::GGC, 1 << 1);
            Self::hb().and16(
                hostbridge::DEVEN,
                !(hostbridge::DEVEN_D2F0 | hostbridge::DEVEN_D2F1),
            );
        }

        p2peg.or32(0xec, (1 << 2) | (1 << 1) | (1 << 0));
        p2peg.and32(VC0RCTL, !0x0000_00fe);
        p2peg.and32(PVCCAP1, !7);

        p2peg.write16(0x06, 0xffff);
        p2peg.write16(0x1e, 0xffff);
        p2peg.write16(0xaa, 0xffff);
        p2peg.write32(0x1c4, 0xffff_ffff);
        p2peg.write32(0x1d0, 0xffff_ffff);
        p2peg.write32(0x1f0, 0xffff_ffff);
        p2peg.write32(0x228, 0xffff_ffff);
        for off in [0x308, 0x314, 0x324, 0x328] {
            p2peg.or32(off, 0);
        }
        p2peg.or32(0xf0, 3 << 26);
        p2peg.or32(0xf0, 3 << 24);
        p2peg.or32(0xf0, 1 << 5);
        p2peg.modify32(0x200, !(3 << 26), 2 << 26);

        let mut e80 = p2peg.read32(0xe80);
        if Self::silicon_revision() >= 2 {
            e80 |= 1 << 12;
        } else {
            e80 &= !(1 << 12);
        }
        p2peg.write32(0xe80, e80);
        p2peg.and32(0xeb4, !(1 << 31));
        p2peg.or32(0xfc, 1 << 31);

        if Self::silicon_revision() >= 3 {
            for off in [
                0xec0, 0xed4, 0xee8, 0xefc, 0xf10, 0xf24, 0xf38, 0xf4c, 0xf60, 0xf74, 0xf88, 0xf9c,
                0xfb0, 0xfc4, 0xfd8, 0xfec,
            ] {
                p2peg.modify32(off, !(0xf << 28), 2 << 28);
            }
        }

        if Self::silicon_revision() <= 2 {
            let mut v = p2peg.read32(0xe80) & (0xf << 4);
            if mch.read32(mchbar::DFT_STRAP1) & (1 << 20) == 0 {
                v |= 7 << 4;
            }
            p2peg.write32(0xe80, v);
        }
    }

    fn disable_pciexpress_x16_link(&self) {
        use hostbridge::peg::*;
        let p2peg = ecam::EcamDevice::new(0, hostbridge::PEG_DEV, hostbridge::PEG_FUNC);
        fstart_log::info!("i945: disabling PCI Express x16 link");
        self.mchbar().setbits16(mchbar::UPMC1, (1 << 5) | (1 << 0));
        p2peg.or8(0x3e, 1 << 6);
        p2peg.or32(0x224, 1 << 8);
        p2peg.and8_or8(0x3e, !(1 << 6), 0);

        let mut timeout = 0x7fff_ffu32;
        while p2peg.read32(PEGSTS) & 0x000f_0000 != 0 && timeout != 0 {
            timeout -= 1;
        }
        if timeout == 0 {
            fstart_log::error!("i945: PEG detect-state timeout");
        }
        Self::hb().and16(hostbridge::DEVEN, !hostbridge::DEVEN_D1F0);
    }

    /// Root complex topology, MCH side (`i945_setup_root_complex_topology`).
    fn setup_root_complex_topology(&self) {
        let ep = self.epbar();
        let dmi = self.dmibar();

        ep.clrsetbits32(epbar::EPESD, 0x00ff_0000, 1 << 16);
        ep.setbits32(epbar::EPLE1D, (1 << 16) | (1 << 0));
        ep.write32(epbar::EPLE1A, self.config.dmibar as u32);
        ep.setbits32(epbar::EPLE2D, (1 << 16) | (1 << 0));

        // Coreboot masks twice: top byte cleared, then bits 16-23 rebuilt.
        dmi.clrbits32(dmibar::DMILE1D, 0xff00_0000);
        dmi.clrsetbits32(dmibar::DMILE1D, 0x00ff_0000, (2 << 16) | (1 << 0));
        dmi.write32(dmibar::DMILE1A, self.config.rcba as u32);
        dmi.setbits32(dmibar::DMILE2D, (1 << 16) | (1 << 0));
        dmi.write32(dmibar::DMILE2A, self.config.epbar as u32);

        if Self::hb().read8(hostbridge::DEVEN) & hostbridge::DEVEN_D1F0 as u8 != 0 {
            let p2peg = ecam::EcamDevice::new(0, hostbridge::PEG_DEV, hostbridge::PEG_FUNC);
            p2peg.write32(hostbridge::peg::LE1A, self.config.epbar as u32);
            p2peg.or32(hostbridge::peg::LE1D, 1 << 0);
        }
    }

    /// Root complex topology, ICH7 side (`ich7_setup_root_complex_topology`).
    fn ich7_setup_root_complex_topology(&self) {
        let rcba = self.rcba();
        rcba.clrsetbits32(rcba::ESD, 0, 2 << 16);
        rcba.clrsetbits32(rcba::ULD, 0, (1 << 24) | (1 << 16));
        // ULBA is 64-bit; coreboot writes the low 32 bits.
        rcba.write32(rcba::ULBA, self.config.dmibar as u32);
        for off in [
            rcba::RP1D,
            rcba::RP2D,
            rcba::RP3D,
            rcba::RP4D,
            rcba::HDD,
            rcba::RP5D,
            rcba::RP6D,
        ] {
            rcba.clrsetbits32(off, 0, 2 << 16);
        }
    }

    /// ICH7 PCIe root-port clocks and slot power (`ich7_setup_pci_express`).
    fn ich7_setup_pci_express(&self) {
        self.rcba().setbits32(rcba::CG, 1 << 0);
        ecam::EcamDevice::new(0, 0x1c, 0).write32(0x54, 0x0000_0060);
        ecam::EcamDevice::new(0, 0x1c, 0).write32(0xd8, 0x0011_0000);
    }

    /// Mobile GM errata fixup (`fixup_i945gm_errata` from `errata.c`).
    fn fixup_mobile_errata(&self) {
        self.mchbar()
            .clrsetbits32(mchbar::FSBPMC3, (1 << 13) | (1 << 29), 0);
    }

    /// Early chipset init before DRAM (`i945_early_initialization`).
    fn early_initialization(&self) {
        match Self::hb().read32(0x00) {
            hostbridge::DID_DESKTOP => fstart_log::info!("i945: Intel 82945G family chipset"),
            hostbridge::DID_MOBILE_A | hostbridge::DID_MOBILE_B => {
                fstart_log::info!("i945: mobile 945GM/PM chipset")
            }
            other => fstart_log::info!("i945: unknown host bridge {:#010x}", other),
        }

        self.setup_bars();

        // Change port80 to LPC; set the early-RCRB misc bit.
        self.rcba().clrbits32(rcba::GCS, 0x04);
        self.rcba().setbits32(rcba::MISC_2010, 1 << 10);
    }

    /// Post-DRAM chipset init (`i945_late_initialization`).
    fn late_initialization(&self) {
        self.setup_egress_port();
        self.ich7_setup_root_complex_topology();
        self.ich7_setup_pci_express();
        self.ich7_setup_dmi_rcrb();
        self.setup_dmi_rcrb();

        if self.config.variant == I945Variant::Mobile {
            self.setup_pci_express_x16();
            self.fixup_mobile_errata();
        }
        self.setup_root_complex_topology();
        raminit::dump_mchbar_registers(&self.mchbar());

        // SSKPD scratchpad marks raminit completion for the resume path.
        self.mchbar().write16(mchbar::SSKPD, 0xcafe);
    }

    fn tolud(&self) -> u32 {
        u32::from(Self::hb().read8(hostbridge::TOLUD) & 0xf8) << 24
    }

    fn tom(&self) -> u64 {
        // TOM is TOLUD in a different format (raminit programs tom = tolud >> 3).
        u64::from(Self::hostbridge_regs().tom.get()) << 27
    }

    fn igd_stolen_base(&self) -> u32 {
        if Self::hostbridge_regs().deven.get()
            & (hostbridge::DEVEN_D2F0 | hostbridge::DEVEN_D2F1)
            == 0
        {
            return 0;
        }
        ecam::EcamDevice::new(0, hostbridge::IGD_DEV, hostbridge::IGD_FUNC)
            .read32(hostbridge::IGD_BSM)
    }

    fn tseg_size(&self) -> u32 {
        decode_tseg_size(Self::hb().read8(hostbridge::ESMRAMC))
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
        Self::hb().write8(hostbridge::SMRAM, val);
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
        let pm = crate::southbridge::pmio_ich::PmIo::new(ICH7_PM_BASE);
        pm.setbits32(
            crate::southbridge::pmio_ich::SMI_EN,
            crate::southbridge::pmio_ich::APMC_EN
                | crate::southbridge::pmio_ich::GBL_SMI_EN
                | crate::southbridge::pmio_ich::EOS,
        );
    }
}

impl crate::IntelNorthbridgeDriver for IntelI945 {
    type Config = IntelI945Config;

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError> {
        Ok(Self {
            config,
            detected_size: 0,
            boot_path: crate::BootPath::Normal,
            mmio32_window: None,
        })
    }

    fn config(&self) -> &'static Self::Config {
        self.config
    }

    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        Ok(())
    }

    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.early_initialization();
        fstart_log::info!("intel-i945: early init complete");
        Ok(())
    }

    fn detect_warm_reset(&self) -> bool {
        self.mchbar().read16(mchbar::SSKPD) == 0xcafe
    }

    fn set_boot_path(&mut self, boot_path: crate::BootPath) {
        self.boot_path = boot_path;
    }

    fn early_post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.late_initialization();
        Ok(())
    }

    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        Ok(())
    }

    fn memory_detected(&mut self, e820: &fstart_core::services::memory_detect::E820State) {
        self.mmio32_window = default_mmio32_window_from_e820(e820, self.config.ecam_base);
    }
}

fn default_mmio32_window_from_e820(
    state: &fstart_core::services::memory_detect::E820State,
    limit: u64,
) -> Option<(u64, u64)> {
    if state.count() == 0 {
        return None;
    }

    let mut low_ram_top = 0x0010_0000u64;
    let mut reserved_after_ram_top = 0u64;
    for entry in state.entries() {
        let end = entry.addr.saturating_add(entry.size).min(0x1_0000_0000);
        if entry.kind == E820Kind::Ram as u32 && entry.addr < 0x1_0000_0000 {
            low_ram_top = low_ram_top.max(end);
        }
    }
    for entry in state.entries() {
        let end = entry.addr.saturating_add(entry.size).min(0x1_0000_0000);
        if entry.kind != E820Kind::Ram as u32
            && entry.addr >= low_ram_top
            && entry.addr < 0x1_0000_0000
        {
            reserved_after_ram_top = reserved_after_ram_top.max(end);
        }
    }

    let base = (reserved_after_ram_top.max(low_ram_top) + 0x000f_ffff) & !0x000f_ffff;
    let limit = limit.min(0x1_0000_0000);
    (base < limit).then_some((base, limit - base))
}

impl PciRootProvider for IntelI945 {
    fn root_info(&self) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: self.config.ecam_base,
            bus_start: 0,
            bus_end: self.config.ecam_buses.saturating_sub(1).min(255) as u8,
        }
    }

    fn resource_windows(&self) -> Result<PciRootWindows, PciRootError> {
        let mut windows = PciRootWindows::new();
        let (mmio32_base, mmio32_size) =
            self.mmio32_window.unwrap_or((0xC000_0000, 0));

        if mmio32_size != 0 {
            windows
                .push(PciWindow {
                    kind: PciWindowKind::Mmio,
                    base: mmio32_base,
                    size: mmio32_size,
                    prefetchable: false,
                })
                .map_err(|_| PciRootError::TooManyWindows)?;
        }
        windows
            .push(PciWindow {
                kind: PciWindowKind::Io,
                base: 0,
                size: 0x10000,
                prefetchable: false,
            })
            .map_err(|_| PciRootError::TooManyWindows)?;

        Ok(windows)
    }
}

impl fstart_arch::mp::SmmOps for IntelI945 {
    fn smm_info(&self) -> Option<SmmInfo> {
        let (base, size) = self.smm_region();
        if size == 0 {
            fstart_log::error!("i945 SMM: TSEG is disabled");
            return None;
        }
        fstart_log::info!("i945 SMM: TSEG base={:#x} size={:#x}", base, size);
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

        let layouts = unsafe { &mut *I945_SMM_CPU_LAYOUTS.0.get() };
        let result = unsafe {
            fstart_smm::install_pic_image(
                image,
                fstart_smm::InstallConfig {
                    smram_base: info.smbase,
                    smram_size: info.smsize as u64,
                    num_cpus,
                    save_state_size: info.save_state_size as u32,
                    page_table_size: 0,
                    cr3: fstart_arch::x86::controlregs::cr3(),
                    platform_kind: fstart_smm::SMM_PLATFORM_INTEL_ICH,
                    // ICH7 has a single 32-bit GPE0 block (no 64-bit flag).
                    platform_flags: 0,
                    platform_data: [ICH7_PM_BASE as u64, 0x28, 0, 0],
                },
                layouts,
            )
        };

        match result {
            Ok(installed) => {
                let targets = &installed.cpus[..num_cpus as usize];
                fstart_arch::mp::prepare_default_smm_relocation(targets);
                let default_handler = unsafe {
                    fstart_smm::install_default_relocation_callback_stub(
                        image,
                        fstart_smm::DefaultRelocationCallbackConfig {
                            default_smbase: fstart_arch::mp::SMM_DEFAULT_SMBASE,
                            cr3: fstart_arch::x86::controlregs::cr3(),
                            callback: fstart_arch::mp::default_smm_relocation_handler as *const ()
                                as usize as u64,
                            stack_top: fstart_arch::mp::SMM_DEFAULT_ENTRY_STACK_TOP,
                        },
                    )
                };
                if default_handler.is_err() {
                    self.smm_close();
                    fstart_log::error!("i945 SMM: failed to install default relocation handler");
                    return Err(SmmError::InstallFailed);
                }

                fstart_log::info!(
                    "i945 SMM: installed image common={:#x} entry={:#x} cpus={}",
                    installed.common_base,
                    installed.common_entry,
                    installed.cpus.len()
                );
                Ok(())
            }
            Err(_) => {
                self.smm_close();
                fstart_log::error!("i945 SMM: failed to install SMM image");
                Err(SmmError::InstallFailed)
            }
        }
    }

    fn smm_relocate(&self) {
        Self::smi_enable_for_relocation();
        let lapic = fstart_arch::lapic::Lapic::from_msr();
        lapic.send_ipi_self(fstart_arch::lapic::INT_ASSERT | fstart_arch::lapic::MT_SMI);
        lapic.wait_ready();
    }

    fn pre_smm_init(&self) {
        let pm = crate::southbridge::pmio_ich::PmIo::new(ICH7_PM_BASE);
        pm.reset_smi_status();
        pm.write32(
            crate::southbridge::pmio_ich::SMI_EN,
            crate::southbridge::pmio_ich::APMC_EN
                | crate::southbridge::pmio_ich::GBL_SMI_EN
                | crate::southbridge::pmio_ich::EOS,
        );
    }

    fn post_smm_init(&self) {
        self.smm_close();
        let pm = crate::southbridge::pmio_ich::PmIo::new(ICH7_PM_BASE);
        pm.reset_smi_status();
        pm.reset_pm1_status();
        pm.tco().reset_tco_status();
        pm.reset_gpe0_status();
        pm.write16(
            crate::southbridge::pmio_ich::PM1_EN,
            crate::southbridge::pmio_ich::PWRBTN_EN | crate::southbridge::pmio_ich::GBL_EN,
        );
        pm.write32(
            crate::southbridge::pmio_ich::SMI_EN,
            crate::southbridge::pmio_ich::TCO_EN
                | crate::southbridge::pmio_ich::APMC_EN
                | crate::southbridge::pmio_ich::SLP_SMI_EN
                | crate::southbridge::pmio_ich::GBL_SMI_EN
                | crate::southbridge::pmio_ich::EOS,
        );
        self.smm_lock();
        fstart_log::info!("i945 SMM: permanent SMI enabled and SMRAM locked");
    }
}

// ---------------------------------------------------------------------------
// ACPI device implementation — i945 host bridge / PCI0
// ---------------------------------------------------------------------------

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi_macros::acpi_dsl;

    use super::*;

    impl AcpiDevice for IntelI945 {
        type Config = IntelI945Config;

        /// Produce i945 PCI root-bridge DSDT content.
        ///
        /// Mirrors coreboot `northbridge/intel/i945/acpi/{hostbridge,peg}.asl`:
        /// host-bridge identity, MCHC PCI config field access with the i945
        /// BAR layout (EPBAR/MCHBAR/PCIEXBAR/DMIBAR at 0x40-0x4c), root PCI
        /// resources, `_OSC`, and the PDRC chipset-reserved ranges.
        /// Southbridge devices attach through the `\\_SB.PCI0` scope emitted
        /// by the ICH7 driver. GFX0/opregion arrives with display support;
        /// PEGP only exists on the mobile variant.
        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            let mchbar = config.mchbar as u32;
            let dmibar = config.dmibar as u32;
            let epbar = config.epbar as u32;
            let ecam_base = config.ecam_base as u32;
            let ecam_size =
                (u64::from(config.ecam_buses) * 1024 * 1024).min(u64::from(u32::MAX)) as u32;
            let pci_mmio_base = self.tolud().max(0x8000_0000);
            let pci_mmio_limit = 0xfebf_ffffu32;
            let rcba = config.rcba as u32;
            let mobile = config.variant == I945Variant::Mobile;

            let mut aml = acpi_dsl! {
                Device("PCI0") {
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
                            EPBR, 20,
                            MHEN, 1,
                            , 13,
                            MHBR, 18,
                            PXEN, 1,
                            PXSZ, 2,
                            , 23,
                            PXBR, 6,
                            DMEN, 1,
                            , 11,
                            DMBR, 20,
                            Offset(0x90),
                            , 4,
                            PM0H, 2,
                            , 2,
                            PM1L, 2,
                            , 2,
                            PM1H, 2,
                            , 2,
                            PM2L, 2,
                            , 2,
                            PM2H, 2,
                            , 2,
                            PM3L, 2,
                            , 2,
                            PM3H, 2,
                            , 2,
                            PM4L, 2,
                            , 2,
                            PM4H, 2,
                            , 2,
                            PM5L, 2,
                            , 2,
                            PM5H, 2,
                            , 2,
                            PM6L, 2,
                            , 2,
                            PM6H, 2,
                            , 2,
                            Offset(0x9c),
                            , 3,
                            TLUD, 5,
                            Offset(0xa0),
                            TOM_, 16,
                        }
                    }

                    Name("MCRS", ResourceTemplate {
                        WordBusNumber(0x0000u16, 0x003Fu16);
                        DWordIO(0x0000u32, 0x0CF7u32);
                        IO(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
                        DWordIO(0x0D00u32, 0xFFFFu32);
                        DWordMemory(Cacheable, ReadWrite, 0x000A0000u32, 0x000BFFFFu32);
                        DWordMemory(Cacheable, ReadWrite, 0x000C0000u32, 0x000FFFFFu32);
                        DWordMemory(NotCacheable, ReadWrite, #{pci_mmio_base}, #{pci_mmio_limit});
                        Memory32Fixed(ReadWrite, 0xFED40000u32, 0x00005000u32);
                    });
                    Method("_CRS", 0, Serialized) {
                        Return(#{fstart_acpi::aml::Path::new("MCRS")});
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
                            Memory32Fixed(ReadWrite, #{ecam_base}, #{ecam_size});
                        });
                    }
                }
            };

            if mobile {
                aml.extend(acpi_dsl! {
                    Scope("\\_SB_.PCI0") {
                        Device("PEGP") {
                            Name("_ADR", 0x00010000u32);
                            Name("_PRT", Package(
                                Package(0x0000FFFFu32, 0u32, 0u32, 16u32),
                                Package(0x0000FFFFu32, 1u32, 0u32, 17u32),
                                Package(0x0000FFFFu32, 2u32, 0u32, 18u32),
                                Package(0x0000FFFFu32, 3u32, 0u32, 19u32)
                            ));
                        }
                    }
                });
            }
            aml
        }
    }
}

impl MemoryDetector for IntelI945 {
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        let tom = self.tom();
        let tolud = self.tolud();
        let usable_top = self.usable_low_memory_top();

        // i945 has no memory remap above 4 GiB: TOM is the single top.
        if tom <= 0x0010_0000 || tolud <= 0x0010_0000 || usable_top <= 0x0010_0000 {
            fstart_log::error!(
                "i945: invalid memory map TOM/TOLUD/usable {:#x}/{:#x}/{:#x}",
                tom,
                tolud,
                usable_top
            );
            return Err(ServiceError::HardwareError);
        }

        let count = build_pc_compatible_e820(entries, usable_top, tom, tolud)?;        fstart_arch::x86::mtrr::set_ram_wb_ranges_from(
            entries[..count]
                .iter()
                .filter(|entry| entry.kind == E820Kind::Ram as u32)
                .map(|entry| (entry.addr, entry.size)),
        );
        fstart_log::info!(
            "i945: detected memory map usable={:#x} TOLUD={:#x} TOM={:#x} TSEG={:#x}+{:#x}",
            usable_top,
            tolud,
            tom,
            self.tseg_base(),
            self.tseg_size()
        );
        Ok(count)
    }

    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        Ok(self.tom())
    }
}

impl MemoryController for IntelI945 {
    fn dram_init(&mut self) -> Result<(), ServiceError> {
        let mut smbus =
            crate::southbridge::smbus::I801SmBus::new(self.config.smbus_base);
        smbus.host_reset();
        let result = raminit::sdram_initialize(self, &mut smbus);
        if let Ok(size) = self.total_ram_bytes() {
            self.detected_size = size;
        }
        result
    }

    fn detected_size_bytes(&self) -> u64 {
        self.detected_size
    }

    fn memory_test(&self) -> Result<(), ServiceError> {
        let top = self.usable_low_memory_top() as usize;
        ramtest_probe(0x0010_0000, top)?;
        ramtest_probe(top.saturating_sub(0x1000), top)?;
        Ok(())
    }
}

/// Test-only probe helper shared with `raminit`.
pub(crate) fn ramtest_probe(addr: usize, top: usize) -> Result<(), ServiceError> {
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

    // SAFETY: Called only after successful i945 DRAM training within the
    // caller-provided usable top. The original word is restored.
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
                    "i945 ramtest: failed at {:#x}: wrote {:#x}, read {:#x}",
                    addr,
                    pattern,
                    got
                );
                return Err(ServiceError::HardwareError);
            }
        }
        ptr::write_volatile(p, old);
    }
    Ok(())
}
