//! Intel Sandy Bridge host bridge / memory controller driver.
//!
//! This crate is the X220-era counterpart to `fstart-driver-intel-gm965` and
//! `fstart-driver-intel-pineview`.  The register names, BAR addresses, and init
//! ordering are intentionally kept close to coreboot's
//! `src/northbridge/intel/sandybridge/` code so each step can be compared
//! against the reference port.
//!
//! Display mode setting is deliberately out of scope for the first X220 port:
//! coreboot hands that to libgfxinit.  fstart only performs the host bridge,
//! ECAM, memory-map, and minimal IGD BAR/opregion plumbing needed by later
//! stages.

#![no_std]
#![allow(dead_code)]
#![allow(
    clippy::empty_line_after_outer_attr,
    clippy::explicit_counter_loop,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use fstart_driver_pci_ecam::{PciEcam, PciEcamConfig};
use fstart_ecam as ecam;
use fstart_mmio::MmioReadWrite;
use fstart_pci::pci_type0_config;
use fstart_services::device::{Device, DeviceError};
use fstart_services::memory_detect::{
    build_pc_compatible_e820, E820Entry, E820Kind, MemoryDetector,
};
use fstart_services::{
    EarlyInit, FinalizeInit, MemoryController, PciBdf, PciHost, PciRootBus, PciWindow,
    PreConsoleInit, ServiceError, StageLocalInit,
};
use serde::{Deserialize, Serialize};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

mod raminit;

/// Sandy Bridge host-bridge PCI device (D0:F0) register offsets used early.
mod host_bridge {
    /// MCHBAR register in PCI config space. Coreboot programs this before
    /// raminit so MCHBAR MMIO registers are reachable at 0xfed10000.
    pub const MCHBAR: u16 = 0x48;
    /// DMIBAR register in PCI config space.
    pub const DMIBAR: u16 = 0x68;
    /// EPBAR register in PCI config space.
    pub const EPBAR: u16 = 0x40;
    /// PCI Express enhanced config-space BAR.
    pub const PCIEXBAR: u16 = 0x60;
    pub const PCI_DEVICE_ID: u16 = 0x02;
    pub const GGC: u16 = 0x50;
    pub const PAVPC: u16 = 0x58;
    pub const DPR: u16 = 0x5c;
    pub const DEVEN: u16 = 0x54;
    pub const DEVEN_IGD: u32 = 1 << 4;
    pub const CAPID0_A: u16 = 0xe4;
    pub const DIDOR: u16 = 0xf3;
    /// PAM0..PAM6 start at 0x80.  Sandy Bridge uses the same shadow decode
    /// model as earlier Intel host bridges: 0x33 opens read/write for BIOS
    /// shadow regions once DRAM exists.
    pub const PAM0: u16 = 0x80;
    pub const MEBASE: u16 = 0x70;
    pub const MESEG_MASK: u16 = 0x78;
    /// Remap base.
    pub const REMAPBASE: u16 = 0x90;
    /// Remap limit.
    pub const REMAPLIMIT: u16 = 0x98;
    /// Top of low usable DRAM.
    pub const TOLUD: u16 = 0xbc;
    /// Top of memory.
    pub const TOM: u16 = 0xa0;
    /// Top of upper usable DRAM.
    pub const TOUUD: u16 = 0xa8;
    /// BDSM / base of data stolen memory.
    pub const BDSM: u16 = 0xb0;
    /// BGSM / base of GTT stolen memory.
    pub const BGSM: u16 = 0xb4;
    /// TSEGMB / base of TSEG.
    pub const TSEGMB: u16 = 0xb8;
}

mod igd {
    pub const DEV: u8 = 2;
    pub const FUNC: u8 = 0;
    pub const PCI_DEVICE_ID: u16 = 0x02;
    pub const MSAC: u16 = 0x62;
}

mod pch_rcba {
    pub const CIR0: usize = 0x0050;
    pub const GEN_PMCON_LOCK: u16 = 0xa6;
    pub const IOBPIRI: usize = 0x2330;
    pub const IOBPD: usize = 0x2334;
    pub const IOBPS: usize = 0x2338;
    pub const CIR1: usize = 0x2088;
    pub const REC: usize = 0x20ac;
    pub const CIR6: usize = 0x2314;
    pub const DMC2: usize = 0x2324;
    pub const UPDCR: usize = 0x1114;
    pub const V0CTL: usize = 0x2014;
    pub const V0STS: usize = 0x201a;
    pub const V1CTL: usize = 0x2020;
    pub const V1STS: usize = 0x2026;
    pub const CIR31: usize = 0x2030;
    pub const CIR32: usize = 0x2040;
    pub const LCAP: usize = 0x21a4;
    pub const DLCTL2: usize = 0x21b0;
    pub const TCLOCKDN: u32 = 1 << 31;
    pub const VCNEGPND: u16 = 2;
}

mod dmibar {
    pub const DMIPVCCAP1: usize = 0x004;
    pub const DMIVC0RCTL: usize = 0x014;
    pub const DMIVC0RSTS: usize = 0x01a;
    pub const DMIVC1RCTL: usize = 0x020;
    pub const DMIVC1RSTS: usize = 0x026;
    pub const DMIVCPRCTL: usize = 0x02c;
    pub const DMIVCPRSTS: usize = 0x032;
    pub const DMIVCMRCTL: usize = 0x038;
    pub const DMIVCMRSTS: usize = 0x03e;
    pub const DMILCAP: usize = 0x084;
    pub const DMIUESTS: usize = 0x1c4;
    pub const DMICESTS: usize = 0x1d0;
    pub const DMILLTC: usize = 0x238;
    pub const DMILCTL: usize = 0x088;
    pub const DMILSTS: usize = 0x08a;
    pub const TXTRN: u16 = 1 << 11;
    pub const VC_NEGOTIATION_PENDING: u16 = 1 << 1;
}

mod mchbar {
    pub const GFXVTBAR: usize = 0x5400;
    pub const VTVC0BAR: usize = 0x5410;
    pub const INTRDIRCTL: usize = 0x5418;
    pub const PAVP_MSG: usize = 0x5500;
    pub const SSKPD_HI: usize = 0x5d14;
    pub const SAPMCTL: usize = 0x5f00;
    pub const SAPMTIMERS: usize = 0x5f10;
    pub const BIOS_RESET_CPL: usize = 0x5da8;
    pub const UMAGFXCTL: usize = 0x6020;
    pub const GFX_POWER: usize = 0x6120;
    pub const VTDTRKLCK: usize = 0x63fc;
    pub const REQLIM: usize = 0x6800;
    pub const DMIVCLIM: usize = 0x7000;
    pub const CRDTLCK: usize = 0x77fc;
    pub const MC_LOCK: usize = 0x50fc;
    pub const VDMBDFBARKVM: usize = 0x5408;
    pub const VDMBDFBARPAVP: usize = 0x5414;
    pub const HDAUDRID: usize = 0x6008;
}

register_bitfields! [u32,
    /// Sandy Bridge device enable register.
    DEVEN_REG [
        IGD OFFSET(4) NUMBITS(1) []
    ],
    /// MCHBAR SAPM control fields used by coreboot's non-display IGD setup.
    SAPMCTL_REG [
        BIT0 OFFSET(0) NUMBITS(1) [],
        BIT9 OFFSET(9) NUMBITS(1) [],
        BIT10 OFFSET(10) NUMBITS(1) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    /// Scratchpad high register.
    SSKPD_HI_REG [
        BIT31 OFFSET(31) NUMBITS(1) []
    ],
    /// GFX power control.
    GFX_POWER_REG [
        BIT0 OFFSET(0) NUMBITS(1) []
    ],
    /// Interrupt direction control.
    INTRDIRCTL_REG [
        BIT4 OFFSET(4) NUMBITS(1) [],
        BIT5 OFFSET(5) NUMBITS(1) []
    ],
    /// VT-d root-table lock registers.
    LOCK_BIT0_REG [
        LOCK OFFSET(0) NUMBITS(1) []
    ],
    /// Limit registers with lock in bit 31.
    LOCK_BIT31_REG [
        LOCK OFFSET(31) NUMBITS(1) []
    ]
];

register_bitfields! [u16,
    /// Graphics and GTT stolen-memory control.
    GGC_REG [
        VGA_DISABLE OFFSET(1) NUMBITS(1) [],
        GMS OFFSET(3) NUMBITS(5) [],
        GGMS OFFSET(8) NUMBITS(2) []
    ]
];

register_bitfields! [u8,
    /// Display ID override register.
    DIDOR_REG [
        PLATFORM_TYPE OFFSET(0) NUMBITS(3) []
    ],
    /// IGD aperture size control.
    MSAC_REG [
        APERTURE_SIZE OFFSET(1) NUMBITS(2) []
    ],
    /// BIOS_RESET_CPL/PAVP/UMA one-bit locks.
    LOCK8_BIT0_REG [
        LOCK OFFSET(0) NUMBITS(1) []
    ]
];

pci_type0_config! {
    /// Sandy Bridge host bridge PCI configuration space.
    pub struct SandybridgeHostPciConfig {
        (0x40 => pub epbar: MmioReadWrite<u64>),
        (0x48 => pub mchbar: MmioReadWrite<u64>),
        (0x50 => pub ggc: MmioReadWrite<u16, GGC_REG::Register>),
        (0x52 => _reserved_ggc),
        (0x54 => pub deven: MmioReadWrite<u32, DEVEN_REG::Register>),
        (0x58 => pub pavpc: MmioReadWrite<u32>),
        (0x5c => pub dpr: MmioReadWrite<u32>),
        (0x60 => pub pciexbar: MmioReadWrite<u64>),
        (0x68 => pub dmibar: MmioReadWrite<u64>),
        (0x70 => pub mebase: MmioReadWrite<u64>),
        (0x78 => pub meseg_mask: MmioReadWrite<u32>),
        (0x7c => _reserved_pam),
        (0x80 => pub pam: [MmioReadWrite<u8>; 7]),
        (0x87 => _reserved_remap),
        (0x90 => pub remapbase: MmioReadWrite<u64>),
        (0x98 => pub remaplimit: MmioReadWrite<u64>),
        (0xa0 => pub tom: MmioReadWrite<u64>),
        (0xa8 => pub touud: MmioReadWrite<u64>),
        (0xb0 => pub bdsm: MmioReadWrite<u32>),
        (0xb4 => pub bgsm: MmioReadWrite<u32>),
        (0xb8 => pub tsegmb: MmioReadWrite<u32>),
        (0xbc => pub tolud: MmioReadWrite<u32>),
        (0xc0 => _reserved_capid),
        (0xe4 => pub capid0_a: MmioReadWrite<u32>),
        (0xe8 => _reserved_didor),
        (0xf3 => pub didor: MmioReadWrite<u8, DIDOR_REG::Register>),
        (0xf4 => @END),
    }
}

pci_type0_config! {
    /// Sandy Bridge IGD PCI configuration space subset.
    pub struct SandybridgeIgdPciConfig {
        (0x40 => _reserved_igd0),
        (0x62 => pub msac: MmioReadWrite<u8, MSAC_REG::Register>),
        (0x63 => @END),
    }
}

register_structs! {
    /// Sandy Bridge MCHBAR sparse overlay for post-early-init control bits.
    pub MchbarRegs {
        (0x0000 => _reserved_mc_lock),
        (0x50fc => pub mc_lock: MmioReadWrite<u8>),
        (0x50fd => _reserved_vtd),
        (0x5400 => pub gfxvtbar: MmioReadWrite<u64>),
        (0x5408 => pub vdmbdfbarkvm: MmioReadWrite<u32>),
        (0x540c => _reserved_vc0),
        (0x5410 => pub vtvc0bar: MmioReadWrite<u64>),
        (0x5418 => pub intrdirctl: MmioReadWrite<u32, INTRDIRCTL_REG::Register>),
        (0x541c => _reserved_pavp),
        (0x5500 => pub pavp_msg: MmioReadWrite<u32, LOCK_BIT0_REG::Register>),
        (0x5504 => _reserved_sskpd),
        (0x5d14 => pub sskpd_hi: MmioReadWrite<u32, SSKPD_HI_REG::Register>),
        (0x5d18 => _reserved_reset),
        (0x5da8 => pub bios_reset_cpl: MmioReadWrite<u8, LOCK8_BIT0_REG::Register>),
        (0x5da9 => _reserved_sapm),
        (0x5f00 => pub sapmctl: MmioReadWrite<u32, SAPMCTL_REG::Register>),
        (0x5f04 => _reserved_sapmtimers),
        (0x5f10 => pub sapmtimers: MmioReadWrite<u32>),
        (0x5f14 => _reserved_bridge_type),
        (0x6008 => pub hdaudrid: MmioReadWrite<u32>),
        (0x600c => _reserved_umagfx),
        (0x6020 => pub umagfxctl: MmioReadWrite<u8, LOCK8_BIT0_REG::Register>),
        (0x6021 => _reserved_gfx_power),
        (0x6120 => pub gfx_power: MmioReadWrite<u32, GFX_POWER_REG::Register>),
        (0x6124 => _reserved_vtd_lock),
        (0x63fc => pub vtdtrklck: MmioReadWrite<u32, LOCK_BIT0_REG::Register>),
        (0x6400 => _reserved_reqlim),
        (0x6800 => pub reqlim: MmioReadWrite<u32, LOCK_BIT31_REG::Register>),
        (0x6804 => _reserved_dmivclim),
        (0x7000 => pub dmivclim: MmioReadWrite<u32, LOCK_BIT31_REG::Register>),
        (0x7004 => _reserved_crdtlck),
        (0x77fc => pub crdtlck: MmioReadWrite<u32, LOCK_BIT0_REG::Register>),
        (0x7800 => @END),
    }
}

#[derive(Clone, Copy)]
struct Mchbar {
    base: usize,
}

impl Mchbar {
    const fn new(base: usize) -> Self {
        Self { base }
    }

    fn regs(&self) -> &'static MchbarRegs {
        // SAFETY: MCHBAR is programmed by early host-bridge init before use.
        unsafe { &*(self.base as *const MchbarRegs) }
    }
}

const GFXVT_BASE: u32 = 0xfed9_0000;
const VTVC0_BASE: u32 = 0xfed9_1000;
const PCI_MMIO32_FALLBACK_BASE: u64 = 0x8000_0000;
const PCI_PIO_BASE: u64 = 0x1000;
const PCI_PIO_SIZE: u64 = 0xf000;
const PCI_DEVICE_ID_INTEL_UM77: u16 = 0x1e58;
const PCH_NATIVE_IOBP_WRITES: &[(u32, u32)] = &[
    (0xea00_7f62, 0x0059_0133),
    (0xec00_7f62, 0x0059_0133),
    (0xec00_7f64, 0x5955_5588),
    (0xea00_40b9, 0x0001_051c),
    (0xeb00_40a1, 0x8000_84ff),
    (0xec00_40a1, 0x8000_84ff),
    (0xea00_4001, 0x0000_8400),
    (0xeb00_4002, 0x4020_1758),
    (0xec00_4002, 0x4020_1758),
    (0xea00_4002, 0x0060_1758),
    (0xea00_40a1, 0x8100_84ff),
    (0xeb00_40b1, 0x0001_c598),
    (0xec00_40b1, 0x0001_c598),
    (0xeb00_40b6, 0x0001_c598),
    (0xea00_00a9, 0x80ff_969f),
    (0xea00_01a9, 0x80ff_969f),
    (0xeb00_40b2, 0x0001_c396),
    (0xeb00_40b3, 0x0001_c396),
    (0xec00_40b2, 0x0001_c396),
    (0xea00_01a9, 0x80ff_94ff),
    (0xea00_0151, 0x0088_037f),
    (0xea00_00a9, 0x80ff_94ff),
    (0xea00_0051, 0x0088_037f),
    (0xea00_7f05, 0x0001_0642),
    (0xea00_40b7, 0x0001_c91c),
    (0xea00_40b8, 0x0001_c91c),
    (0xeb00_40a1, 0x8200_84ff),
    (0xec00_40a1, 0x8200_84ff),
    (0xea00_7f0a, 0xc248_0000),
    (0xec00_404d, 0x01ff_177f),
    (0xec00_0084, 0x5a60_0000),
    (0xec00_0184, 0x5a60_0000),
    (0xec00_0284, 0x5a60_0000),
    (0xec00_0384, 0x5a60_0000),
    (0xec00_0094, 0x000f_0501),
    (0xec00_0194, 0x000f_0501),
    (0xec00_0294, 0x000f_0501),
    (0xec00_0394, 0x000f_0501),
    (0xec00_0096, 0x0000_0001),
    (0xec00_0196, 0x0000_0001),
    (0xec00_0296, 0x0000_0001),
    (0xec00_0396, 0x0000_0001),
    (0xec00_0001, 0x0000_8c08),
    (0xec00_0101, 0x0000_8c08),
    (0xec00_0201, 0x0000_8c08),
    (0xec00_0301, 0x0000_8c08),
    (0xec00_40b5, 0x0001_c518),
    (0xec00_0087, 0x0607_7597),
    (0xec00_0187, 0x0607_7597),
    (0xec00_0287, 0x0607_7597),
    (0xec00_0387, 0x0607_7597),
    (0xea00_0050, 0x00bb_0157),
    (0xea00_0150, 0x00bb_0157),
    (0xec00_7f60, 0x7777_7d77),
    (0xea00_008d, 0x0132_0000),
    (0xea00_018d, 0x0132_0000),
    (0xec00_07b2, 0x0451_4b5e),
    (0xec00_078c, 0x4000_0200),
    (0xec00_0780, 0x0200_0020),
];

/// Integrated graphics setup policy for Sandy Bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandybridgeIgdConfig {
    /// Enable legacy VGA decode for the integrated GPU.
    #[serde(default = "default_true")]
    pub enable_vga: bool,
    /// GTTMMADR address for the IGD.  X220 coreboot uses libgfxinit later;
    /// fstart does not mode-set yet but keeps the BAR explicit in board RON.
    #[serde(default = "default_gtt_mmio_base")]
    pub gtt_mmio_base: u64,
    /// Coreboot IGD UMA index. 0 selects 32 MiB stolen memory.
    #[serde(default)]
    pub uma_index: u8,
    /// Whether Azalia/HDA is enabled for the VT-d protected memory sequence.
    #[serde(default = "default_true")]
    pub azalia_enabled: bool,
    /// Optional signed-FFS VBT blob name.  The board should point this at
    /// coreboot's `variants/x220/data.vbt` when display work is added.
    #[serde(default)]
    pub vbt_file: Option<heapless::String<32>>,
}

impl Default for SandybridgeIgdConfig {
    fn default() -> Self {
        Self {
            enable_vga: true,
            gtt_mmio_base: default_gtt_mmio_base(),
            uma_index: 0,
            azalia_enabled: true,
            vbt_file: None,
        }
    }
}

/// Intel Sandy Bridge northbridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelSandybridgeConfig {
    /// MCHBAR MMIO base.  Coreboot Sandy Bridge boards use 0xfed10000.
    pub mchbar: u64,
    /// DMIBAR MMIO base.  Coreboot Sandy Bridge boards use 0xfed18000.
    pub dmibar: u64,
    /// EPBAR MMIO base.  Coreboot Sandy Bridge boards use 0xfed19000.
    pub epbar: u64,
    /// PCH RCBA MMIO base used for DMI virtual-channel programming.
    #[serde(default = "default_pch_rcba")]
    pub pch_rcba: u64,
    /// PCIEXBAR/ECAM base.  ThinkPad X220 uses 0xe0000000 for 256 MiB.
    pub ecam_base: u64,
    /// Number of ECAM buses decoded by PCIEXBAR.
    #[serde(default = "default_ecam_buses")]
    pub ecam_buses: u16,
    /// SPD EEPROM addresses in Sandy Bridge slot order.  Coreboot X220 sets
    /// `{0x50, 0, 0x51, 0}` for two DDR3 SO-DIMM slots.
    #[serde(default = "default_spd_addresses")]
    pub spd_addresses: [u8; 4],
    /// PCH I801 SMBus I/O base used by native raminit for SPD reads.
    #[serde(default = "default_smbus_base")]
    pub smbus_base: u16,
    /// Maximum memory clock from coreboot devicetree.  X220 uses 666 MHz
    /// (DDR3-1333 data rate).
    #[serde(default = "default_max_mem_clock_mhz")]
    pub max_mem_clock_mhz: u16,
    /// Board has a Lenovo embedded controller; passed through from coreboot's
    /// Sandy Bridge chip config for later native raminit/MRC work.
    #[serde(default)]
    pub ec_present: bool,
    /// Integrated graphics non-display setup policy.
    #[serde(default)]
    pub igd: SandybridgeIgdConfig,
    /// ACPI root bridge name, normally `PCI0`.
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
}

fn default_true() -> bool {
    true
}

fn default_gtt_mmio_base() -> u64 {
    0xe000_0000 - 0x0040_0000
}

fn default_pch_rcba() -> u64 {
    0xfed1_c000
}

fn default_ecam_buses() -> u16 {
    256
}

fn default_spd_addresses() -> [u8; 4] {
    [0x50, 0, 0x51, 0]
}

fn default_max_mem_clock_mhz() -> u16 {
    666
}

fn default_smbus_base() -> u16 {
    0x0400
}

/// Intel Sandy Bridge host bridge / memory controller.
pub struct IntelSandybridge {
    config: &'static IntelSandybridgeConfig,
    pci: Option<PciEcam>,
    detected_bytes: u64,
}

// SAFETY: firmware performs chipset init on the BSP before concurrency exists.
unsafe impl Send for IntelSandybridge {}
// SAFETY: the struct contains immutable config plus owned ECAM state.
unsafe impl Sync for IntelSandybridge {}

impl IntelSandybridge {
    fn hostbridge(&self) -> ecam::EcamDevice {
        ecam::EcamDevice::new(0, 0, 0)
    }

    fn hostbridge_regs(&self) -> &'static SandybridgeHostPciConfig {
        // SAFETY: the Sandy Bridge host bridge is fixed at 00:00.0, ECAM is
        // initialized before this overlay is used, and exact pre-ECAM writes
        // still go through CF8/CFC in `enable_ecam()`.
        unsafe { self.hostbridge().regs::<SandybridgeHostPciConfig>() }
    }

    fn igd_regs(&self) -> &'static SandybridgeIgdPciConfig {
        // SAFETY: IGD is fixed at 00:02.0 on Sandy Bridge when enabled; callers
        // check the device ID before programming IGD-specific fields.
        unsafe { ecam::EcamDevice::new(0, igd::DEV, igd::FUNC).regs() }
    }

    fn mchbar_regs(&self) -> &'static MchbarRegs {
        Mchbar::new(self.config.mchbar as usize).regs()
    }

    fn pciexbar_length_bits(&self) -> u64 {
        match self.config.ecam_buses {
            256 => 0 << 1,
            128 => 1 << 1,
            _ => 2 << 1,
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn enable_ecam(&self) {
        // PCIEXBAR encoding follows coreboot Sandy Bridge: base in bits 38:28,
        // bit 0 enable, bits 2:1 select length (00=256 MiB, 01=128 MiB,
        // 10=64 MiB).  Use legacy CF8/CFC because ECAM is not live yet.
        let value =
            (self.config.ecam_base & 0x0000_003f_f000_0000) | self.pciexbar_length_bits() | 1;
        // SAFETY: one-time legacy PCI config write to enable ECAM before the
        // ECAM MMIO accessor can be used.
        unsafe {
            fstart_pio::pci_cfg_write32(
                0,
                0,
                0,
                (host_bridge::PCIEXBAR + 4) as u8,
                (value >> 32) as u32,
            );
            fstart_pio::pci_cfg_write32(0, 0, 0, host_bridge::PCIEXBAR as u8, value as u32);
        }
        ecam::init(self.config.ecam_base as usize);
        fstart_log::info!("sandybridge: ECAM enabled at {:#x}", self.config.ecam_base);
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn enable_ecam(&self) {
        ecam::init(self.config.ecam_base as usize);
        fstart_log::info!("sandybridge: ECAM enable (stub, non-x86)");
    }

    #[cfg(target_arch = "x86_64")]
    fn mmio_read16(addr: usize) -> u16 {
        // SAFETY: caller passes chipset MMIO addresses programmed by early init.
        unsafe { core::ptr::read_volatile(addr as *const u16) }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn mmio_read16(_addr: usize) -> u16 {
        0
    }

    #[cfg(target_arch = "x86_64")]
    fn mmio_write8(addr: usize, val: u8) {
        // SAFETY: caller passes chipset MMIO addresses programmed by early init.
        unsafe { core::ptr::write_volatile(addr as *mut u8, val) }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn mmio_write8(_addr: usize, _val: u8) {}

    #[cfg(target_arch = "x86_64")]
    fn mmio_write16(addr: usize, val: u16) {
        // SAFETY: caller passes chipset MMIO addresses programmed by early init.
        unsafe { core::ptr::write_volatile(addr as *mut u16, val) }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn mmio_write16(_addr: usize, _val: u16) {}

    #[cfg(target_arch = "x86_64")]
    fn mmio_read8(addr: usize) -> u8 {
        // SAFETY: caller passes chipset MMIO addresses programmed by early init.
        unsafe { core::ptr::read_volatile(addr as *const u8) }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn mmio_read8(_addr: usize) -> u8 {
        0
    }

    #[cfg(target_arch = "x86_64")]
    fn mmio_write32(addr: usize, val: u32) {
        // SAFETY: caller passes chipset MMIO addresses that were programmed by
        // early northbridge init and are only touched by BSP firmware code.
        unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
    }

    #[cfg(target_arch = "x86_64")]
    fn mmio_read32(addr: usize) -> u32 {
        // SAFETY: caller passes chipset MMIO addresses that were programmed by
        // early northbridge init and are only touched by BSP firmware code.
        unsafe { core::ptr::read_volatile(addr as *const u32) }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn mmio_write32(_addr: usize, _val: u32) {}

    #[cfg(not(target_arch = "x86_64"))]
    fn mmio_read32(_addr: usize) -> u32 {
        0
    }

    fn program_hostbridge_bars(&self) {
        let hb = self.hostbridge_regs();
        // Keep these writes comparable to coreboot early_init.c: program EPBAR,
        // MCHBAR, DMIBAR, then enable each BAR with bit 0 set.
        hb.epbar.set((self.config.epbar & 0xffff_f000) | 1);
        hb.mchbar.set((self.config.mchbar & 0xffff_8000) | 1);
        hb.dmibar.set((self.config.dmibar & 0xffff_f000) | 1);

        // Open PAM ranges for legacy BIOS shadow.  Match coreboot Sandy Bridge:
        // PAM0=0x30 for the C0000 segment, PAM1..6=0x33 for read/write below
        // 1 MiB once DRAM is available.
        hb.pam[0].set(0x30);
        for pam in hb.pam[1..].iter() {
            pam.set(0x33);
        }
        fstart_log::info!("sandybridge: host bridge BARs/PAM configured");
    }

    fn program_didor_platform_type(&self) {
        let hb = self.hostbridge_regs();
        let device_id = hb.device_id.get();
        let mobile = (device_id & 0x000c) == 0x0004;
        if (hb.capid0_a.get() & (1 << 10)) != 0 {
            hb.didor
                .modify(DIDOR_REG::PLATFORM_TYPE.val(u8::from(mobile)));
        }
    }

    fn program_vtd_bars(&self) {
        let hb = self.hostbridge_regs();
        if (hb.capid0_a.get() & (1 << 23)) != 0 {
            return;
        }
        let mch = self.mchbar_regs();
        mch.gfxvtbar.set(u64::from(GFXVT_BASE | 1));
        mch.vtvc0bar.set(u64::from(VTVC0_BASE | 1));
        Self::mmio_write32(GFXVT_BASE as usize + 0xff0, 0x8000_0000);
        if self.config.igd.azalia_enabled {
            Self::mmio_write32(VTVC0_BASE as usize + 0xff0, 0x2000_0000);
            Self::mmio_write32(VTVC0_BASE as usize + 0xff0, 0xa000_0000);
        } else {
            Self::mmio_write32(VTVC0_BASE as usize + 0xff0, 0x8000_0000);
        }
    }

    fn enable_integrated_graphics(&self) {
        self.hostbridge_regs().deven.modify(DEVEN_REG::IGD::SET);
    }

    fn setup_graphics_non_display(&self) {
        let igd_dev = ecam::EcamDevice::new(0, igd::DEV, igd::FUNC);
        let devid = igd_dev.read16(igd::PCI_DEVICE_ID);
        const SUPPORTED: [u16; 12] = [
            0x0102, 0x0106, 0x010a, 0x0112, 0x0116, 0x0122, 0x0126, 0x0152, 0x0156, 0x0162, 0x0166,
            0x016a,
        ];
        if !SUPPORTED.contains(&devid) {
            return;
        }
        let hb = self.hostbridge_regs();
        let vga = if self.config.igd.enable_vga {
            GGC_REG::VGA_DISABLE::CLEAR
        } else {
            GGC_REG::VGA_DISABLE::SET
        };
        hb.ggc.modify(
            GGC_REG::GMS.val(u16::from((self.config.igd.uma_index & 0x1f) + 1))
                + GGC_REG::GGMS.val(2)
                + vga,
        );

        self.igd_regs().msac.modify(MSAC_REG::APERTURE_SIZE.val(1));

        let mch = self.mchbar_regs();
        mch.sapmctl
            .modify(SAPMCTL_REG::BIT9::SET + SAPMCTL_REG::BIT10::SET + SAPMCTL_REG::BIT0::SET);
        mch.sskpd_hi.modify(SSKPD_HI_REG::BIT31::SET);
        mch.gfx_power.modify(GFX_POWER_REG::BIT0::CLEAR);
        mch.intrdirctl
            .modify(INTRDIRCTL_REG::BIT4::SET + INTRDIRCTL_REG::BIT5::SET);
    }

    fn retrain_dmi_link(&self) {
        let dmi = self.config.dmibar as usize;
        let ctl = Self::mmio_read8(dmi + dmibar::DMILCTL);
        Self::mmio_write8(dmi + dmibar::DMILCTL, ctl | (1 << 5));
        while (Self::mmio_read16(dmi + dmibar::DMILSTS) & dmibar::TXTRN) != 0 {
            core::hint::spin_loop();
        }
    }

    fn wait_iobp(&self) {
        let rcba = self.config.pch_rcba as usize;
        while (Self::mmio_read8(rcba + pch_rcba::IOBPS) & 1) != 0 {
            core::hint::spin_loop();
        }
    }

    fn read_iobp(&self, address: u32) -> u32 {
        let rcba = self.config.pch_rcba as usize;
        Self::mmio_write32(rcba + pch_rcba::IOBPIRI, address);
        let iobps = Self::mmio_read16(rcba + pch_rcba::IOBPS);
        Self::mmio_write16(rcba + pch_rcba::IOBPS, (iobps & 0x01ff) | 0x0600);
        self.wait_iobp();
        let ret = Self::mmio_read32(rcba + pch_rcba::IOBPD);
        self.wait_iobp();
        let _ = Self::mmio_read8(rcba + pch_rcba::IOBPS);
        ret
    }

    fn write_iobp(&self, address: u32, val: u32) {
        let rcba = self.config.pch_rcba as usize;
        let _ = self.read_iobp(address);
        let iobps = Self::mmio_read16(rcba + pch_rcba::IOBPS);
        Self::mmio_write16(rcba + pch_rcba::IOBPS, (iobps & 0x01ff) | 0x0600);
        self.wait_iobp();
        Self::mmio_write32(rcba + pch_rcba::IOBPD, val);
        self.wait_iobp();
        let iobps = Self::mmio_read16(rcba + pch_rcba::IOBPS);
        Self::mmio_write16(rcba + pch_rcba::IOBPS, (iobps & 0x01ff) | 0x0600);
        let _ = Self::mmio_read8(rcba + pch_rcba::IOBPS);
    }

    fn early_pch_init_native(&self) {
        let rcba = self.config.pch_rcba as usize;
        let lpc = ecam::EcamDevice::new(0, 31, 0);
        let pcie_ports = if lpc.read16(host_bridge::PCI_DEVICE_ID) == PCI_DEVICE_ID_INTEL_UM77 {
            4
        } else {
            8
        };
        for port in 0..pcie_ports {
            let pcie = ecam::EcamDevice::new(0, 28, port);
            pcie.write32(0x338, pcie.read32(0x338) & !(1 << 26));
        }
        lpc.write8(
            pch_rcba::GEN_PMCON_LOCK,
            lpc.read8(pch_rcba::GEN_PMCON_LOCK) | (1 << 1),
        );

        Self::mmio_write32(rcba + pch_rcba::CIR1, 0x0010_9000);
        Self::mmio_write32(
            rcba + pch_rcba::REC,
            Self::mmio_read32(rcba + pch_rcba::REC) | (1 << 30),
        );
        Self::mmio_write32(rcba + 0x100c, 0x0111_0000);
        Self::mmio_write8(rcba + 0x2340, 0x1b);
        Self::mmio_write32(
            rcba + pch_rcba::CIR6,
            Self::mmio_read32(rcba + pch_rcba::CIR6) | (1 << 21),
        );
        Self::mmio_write32(
            rcba + 0x2310,
            (Self::mmio_read32(rcba + 0x2310) & !(3 << 29)) | (1 << 29),
        );
        Self::mmio_write32(rcba + pch_rcba::DMC2, 0x0085_4c74);
        Self::mmio_write32(rcba + 0x2310, Self::mmio_read32(rcba + 0x2310) & !(3 << 22));
        Self::mmio_write32(rcba + 0x2310, Self::mmio_read32(rcba + 0x2310) & !(3 << 20));

        for (addr, val) in PCH_NATIVE_IOBP_WRITES {
            self.write_iobp(*addr, *val);
        }
    }

    fn early_pch_init_native_dmi_pre(&self) {
        let rcba = self.config.pch_rcba as usize;
        let lcap = Self::mmio_read32(rcba + pch_rcba::LCAP);
        Self::mmio_write32(
            rcba + pch_rcba::LCAP,
            (lcap & !0x0003_fc00) | (3 << 10) | (2 << 12) | (2 << 15),
        );
        let reg = Self::mmio_read32(rcba + 0x2340);
        Self::mmio_write32(rcba + 0x2340, (reg & !0x00ff_0000) | (0x3a << 16));
        let dlctl2 = Self::mmio_read8(rcba + pch_rcba::DLCTL2);
        Self::mmio_write8(rcba + pch_rcba::DLCTL2, (dlctl2 & !0x0f) | 2);
    }

    fn early_pch_init_native_dmi_post(&self) {
        let rcba = self.config.pch_rcba as usize;
        let mut cir0 = Self::mmio_read32(rcba + pch_rcba::CIR0);
        cir0 &= !((0xff << 16) | 0x0fff);
        cir0 |= 0x654;
        cir0 |= (0x20 << 16) | (0x0a << 16);
        Self::mmio_write32(rcba + pch_rcba::CIR0, cir0);
        let _ = Self::mmio_read32(rcba + pch_rcba::CIR0);

        let updcr = Self::mmio_read8(rcba + pch_rcba::UPDCR);
        Self::mmio_write8(rcba + pch_rcba::UPDCR, updcr | (1 << 2) | 1);

        Self::mmio_write32(rcba + pch_rcba::V0CTL, (1 << 31) | (0x0c << 1) | 1);
        Self::mmio_write32(rcba + pch_rcba::V1CTL, (1 << 31) | (1 << 24) | (0x11 << 1));
        let _ = Self::mmio_read32(rcba + pch_rcba::V1CTL);
        Self::mmio_write32(rcba + pch_rcba::CIR31, (1 << 31) | (2 << 24) | (0x22 << 1));
        let _ = Self::mmio_read32(rcba + pch_rcba::CIR31);
        Self::mmio_write32(rcba + pch_rcba::CIR32, (1 << 31) | (7 << 24) | (0x40 << 1));
        Self::mmio_write32(
            rcba + pch_rcba::CIR0,
            Self::mmio_read32(rcba + pch_rcba::CIR0) | pch_rcba::TCLOCKDN,
        );
        let _ = Self::mmio_read32(rcba + pch_rcba::CIR0);

        for sts in [pch_rcba::V0STS, pch_rcba::V1STS, 0x2036, 0x2046] {
            while (Self::mmio_read16(rcba + sts) & pch_rcba::VCNEGPND) != 0 {
                core::hint::spin_loop();
            }
        }
    }

    fn early_init_dmi_for_native_raminit(&self) {
        let dmi = self.config.dmibar as usize;
        // Coreboot early_dmi.c write-once settings for Sandy Bridge native
        // raminit.  Ivy Bridge-only DMI recipe is intentionally skipped.
        self.early_pch_init_native_dmi_pre();
        let cap = Self::mmio_read32(dmi + dmibar::DMILCAP);
        Self::mmio_write32(
            dmi + dmibar::DMILCAP,
            (cap & !0x0003_f00f) | 2 | (2 << 12) | (2 << 15),
        );
        self.retrain_dmi_link();
        self.retrain_dmi_link();

        Self::mmio_write32(dmi + dmibar::DMIVC0RCTL, (1 << 31) | (0x0c << 1) | 1);
        Self::mmio_write32(
            dmi + dmibar::DMIVC1RCTL,
            (1 << 31) | (1 << 24) | (0x11 << 1),
        );
        Self::mmio_write32(
            dmi + dmibar::DMIVCPRCTL,
            (1 << 31) | (2 << 24) | (0x22 << 1),
        );
        Self::mmio_write32(
            dmi + dmibar::DMIVCMRCTL,
            (1 << 31) | (7 << 24) | (0x40 << 1),
        );
        let pvccap = Self::mmio_read8(dmi + dmibar::DMIPVCCAP1);
        Self::mmio_write8(dmi + dmibar::DMIPVCCAP1, pvccap | 1);

        self.early_pch_init_native_dmi_post();

        for sts in [
            dmibar::DMIVC0RSTS,
            dmibar::DMIVC1RSTS,
            dmibar::DMIVCPRSTS,
            dmibar::DMIVCMRSTS,
        ] {
            while (Self::mmio_read16(dmi + sts) & dmibar::VC_NEGOTIATION_PENDING) != 0 {
                core::hint::spin_loop();
            }
        }
    }

    fn program_pre_raminit_parity(&self) {
        self.program_didor_platform_type();
        self.program_vtd_bars();
        self.enable_integrated_graphics();
        self.setup_graphics_non_display();
    }

    fn program_native_pch_and_dmi_before_raminit(&self) {
        self.early_pch_init_native();
        self.early_init_dmi_for_native_raminit();
    }

    fn northbridge_ramstage_handoff(&self) {
        let dmi = self.config.dmibar as usize;
        Self::mmio_write32(
            dmi + dmibar::DMILLTC,
            Self::mmio_read32(dmi + dmibar::DMILLTC) | (1 << 29),
        );
        Self::mmio_write32(dmi + dmibar::DMIUESTS, 0xffff_ffff);
        Self::mmio_write32(dmi + dmibar::DMICESTS, 0xffff_ffff);
        Self::mmio_write32(dmi + 0x0d04, Self::mmio_read32(dmi + 0x0d04) | (1 << 4));
        let dmilctl = Self::mmio_read8(dmi + dmibar::DMILCTL);
        Self::mmio_write8(dmi + dmibar::DMILCTL, dmilctl | 0x03);

        let mch = self.mchbar_regs();
        mch.sapmtimers.set((mch.sapmtimers.get() & !0xff) | 0x20);
        mch.bios_reset_cpl.modify(LOCK8_BIT0_REG::LOCK::SET);
        mch.pavp_msg.set(0x0010_0001);
    }

    fn read_memory_limits(&self) -> (u64, u32, u64, u32) {
        let hb = self.hostbridge_regs();
        let tom = hb.tom.get() & !0x000f_ffff;
        let touud = hb.touud.get() & !0x000f_ffff;
        let tolud = hb.tolud.get() & !0x000f_ffff;
        let tseg = hb.tsegmb.get() & !0x000f_ffff;
        (tom, tolud, touud, tseg)
    }

    fn usable_ram_from_limits(&self) -> Result<u64, ServiceError> {
        let (_tom, tolud, touud, tseg) = self.read_memory_limits();
        let low = if tseg != 0 { tseg } else { tolud };
        if low == 0 {
            return Err(ServiceError::NotInitialized);
        }
        let high = touud.saturating_sub(0x1_0000_0000);
        Ok(u64::from(low) + high)
    }

    fn finalize_lockdown(&self) {
        let hb = self.hostbridge_regs();
        hb.ggc.modify(GGC_REG::VGA_DISABLE::SET);
        hb.pavpc.set(hb.pavpc.get() | (1 << 2));
        hb.dpr.set(hb.dpr.get() | 1);
        hb.meseg_mask.set(hb.meseg_mask.get() | (1 << 10));
        hb.remapbase.set(hb.remapbase.get() | 1);
        hb.remaplimit.set(hb.remaplimit.get() | 1);
        hb.tom.set(hb.tom.get() | 1);
        hb.touud.set(hb.touud.get() | 1);
        hb.bdsm.set(hb.bdsm.get() | 1);
        hb.bgsm.set(hb.bgsm.get() | 1);
        hb.tsegmb.set(hb.tsegmb.get() | 1);
        hb.tolud.set(hb.tolud.get() | 1);

        let mch = self.mchbar_regs();
        mch.pavp_msg.modify(LOCK_BIT0_REG::LOCK::SET);
        mch.sapmctl.modify(SAPMCTL_REG::LOCK::SET);
        mch.umagfxctl.modify(LOCK8_BIT0_REG::LOCK::SET);
        mch.vtdtrklck.modify(LOCK_BIT0_REG::LOCK::SET);
        mch.reqlim.modify(LOCK_BIT31_REG::LOCK::SET);
        mch.dmivclim.modify(LOCK_BIT31_REG::LOCK::SET);
        mch.crdtlck.modify(LOCK_BIT0_REG::LOCK::SET);
        mch.mc_lock.set(0x8f);
        mch.vdmbdfbarkvm.set(mch.vdmbdfbarkvm.get());
        // VDMBDFBARPAVP aliases the high dword of VTVC0BAR at 0x5414.
        mch.vtvc0bar.set(mch.vtvc0bar.get());
        mch.hdaudrid.set(mch.hdaudrid.get());
    }

    fn pci_ecam_config(&self) -> PciEcamConfig {
        PciEcamConfig {
            ecam_base: self.config.ecam_base,
            ecam_size: self.ecam_size(),
            // Size 0 asks PciEcam to derive the 32-bit aperture from e820.
            // PCIEXBAR at 0xe0000000 bounds ordinary 32-bit MMIO below ECAM,
            // matching the existing GM965/X61 pattern.
            mmio32_base: PCI_MMIO32_FALLBACK_BASE,
            mmio32_size: 0,
            mmio64_base: 0,
            mmio64_size: 0,
            pio_base: PCI_PIO_BASE,
            pio_size: PCI_PIO_SIZE,
            bus_start: self.bus_start(),
            bus_end: self.bus_end(),
        }
    }

    fn ensure_pci_ecam(&mut self) -> Result<&mut PciEcam, ServiceError> {
        if self.pci.is_none() {
            let cfg = self.pci_ecam_config();
            self.pci = Some(PciEcam::from_config(&cfg).map_err(|_| ServiceError::HardwareError)?);
        }
        self.pci.as_mut().ok_or(ServiceError::NotInitialized)
    }

    fn pci_ecam(&self) -> Result<&PciEcam, ServiceError> {
        self.pci.as_ref().ok_or(ServiceError::NotInitialized)
    }
}

impl Device for IntelSandybridge {
    const NAME: &'static str = "intel-sandybridge";
    const COMPATIBLE: &'static [&'static str] = &["intel,sandybridge", "intel,sandybridge-mch"];
    type Config = IntelSandybridgeConfig;

    fn new(config: &'static Self::Config) -> Result<Self, DeviceError> {
        Ok(Self {
            config,
            pci: None,
            detected_bytes: 0,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
}

impl PreConsoleInit for IntelSandybridge {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        <Self as PciHost>::pre_console_init(self)
    }
}

impl EarlyInit for IntelSandybridge {
    fn early_init(&mut self) -> Result<(), ServiceError> {
        <Self as PciHost>::early_init(self)
    }
}

impl StageLocalInit for IntelSandybridge {
    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        // Ramstage runs from trained DRAM.  Do not repeat pre-DRAM DMI/VC
        // retraining or other write-once host-bridge setup here; bootblock
        // already performed it before raminit.
        self.enable_ecam();
        self.northbridge_ramstage_handoff();
        Ok(())
    }
}

impl PciHost for IntelSandybridge {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        Ok(())
    }

    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.program_hostbridge_bars();
        self.program_pre_raminit_parity();
        Ok(())
    }
}

impl MemoryController for IntelSandybridge {
    fn dram_init(&mut self) -> Result<(), ServiceError> {
        // Native-only path: no MRC fallback. Run the Sandy Bridge DDR3
        // training sequence and publish memory limits only after it succeeds.
        let me_uma_size_mb = raminit::early_me_init_and_uma_size()?;
        self.program_native_pch_and_dmi_before_raminit();
        raminit::run_native_raminit(
            self.config.mchbar,
            self.config.smbus_base,
            self.config.spd_addresses,
            self.config.max_mem_clock_mhz,
            me_uma_size_mb,
        )?;

        self.detected_bytes = self.usable_ram_from_limits()?;
        if self.detected_bytes == 0 {
            fstart_log::error!("sandybridge: DRAM limits are zero after native raminit");
            return Err(ServiceError::NotInitialized);
        }
        Ok(())
    }

    fn detected_size_bytes(&self) -> u64 {
        self.detected_bytes
            .max(self.usable_ram_from_limits().unwrap_or(0))
    }
}

impl FinalizeInit for IntelSandybridge {
    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        self.finalize_lockdown();
        Ok(())
    }
}

impl MemoryDetector for IntelSandybridge {
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        let _ = self.usable_ram_from_limits()?;
        let (_tom, tolud, touud, tseg) = self.read_memory_limits();
        let usable_top = if tseg != 0 { tseg } else { tolud };
        let count = build_pc_compatible_e820(entries, usable_top, touud, tolud)?;
        fstart_arch_x86::mtrr::set_ram_wb_ranges_from(
            entries[..count]
                .iter()
                .filter(|entry| entry.kind == E820Kind::Ram as u32)
                .map(|entry| (entry.addr, entry.size)),
        );
        Ok(count)
    }

    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        self.usable_ram_from_limits()
    }
}

impl PciRootBus for IntelSandybridge {
    fn init_bus(&mut self) -> Result<(), ServiceError> {
        self.ensure_pci_ecam()?.init_bus()
    }

    fn config_read32(&self, addr: PciBdf, reg: u16) -> Result<u32, ServiceError> {
        self.pci_ecam()?.config_read32(addr, reg)
    }

    fn config_write32(&self, addr: PciBdf, reg: u16, val: u32) -> Result<(), ServiceError> {
        self.pci_ecam()?.config_write32(addr, reg, val)
    }

    fn ecam_base(&self) -> u64 {
        self.config.ecam_base
    }

    fn ecam_size(&self) -> u64 {
        u64::from(self.config.ecam_buses) * 1024 * 1024
    }

    fn bus_start(&self) -> u8 {
        0
    }

    fn bus_end(&self) -> u8 {
        self.config.ecam_buses.saturating_sub(1).min(255) as u8
    }

    fn device_count(&self) -> usize {
        self.pci.as_ref().map_or(0, PciRootBus::device_count)
    }

    fn windows(&self) -> &[PciWindow] {
        self.pci.as_ref().map_or(&[], PciRootBus::windows)
    }
}

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec;
    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi_macros::acpi_dsl;

    use super::*;

    impl AcpiDevice for IntelSandybridge {
        type Config = IntelSandybridgeConfig;

        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            let name = config.acpi_name.as_deref().unwrap_or("PCI0");
            let rcba = config.pch_rcba as u32;
            let mchbar = config.mchbar as u32;
            let dmibar = config.dmibar as u32;
            let epbar = config.epbar as u32;
            let ecam_base = config.ecam_base as u32;
            let ecam_end = ecam_base.saturating_sub(1);
            let bus_end = config.ecam_buses.saturating_sub(1).min(255) as u8;
            let (tom, tolud, touud, _tseg) = self.read_memory_limits();
            let mebase = self.hostbridge_regs().mebase.get() as u32 & !0x000f_ffff;
            let pci_mmio_base = if tolud == mebase {
                (tom & 0xffff_ffff) as u32
            } else {
                tolud
            };
            let _mmio64_base = touud.max(0x1_0000_0000);

            acpi_dsl! {
                Device(#{name}) {
                    Name("_HID", EisaId("PNP0A08"));
                    Name("_CID", EisaId("PNP0A03"));
                    Name("_BBN", 0u32);

                    Device("MCHC") {
                        Name("_ADR", 0x00000000u32);
                        OperationRegion("MCHP", PciConfig, 0x00u32, 0x100u32);
                        Field("MCHP", DWordAcc, NoLock, Preserve) {
                            Offset(0x40),
                            EPEN, 1, , 11, EPBR, 27,
                            Offset(0x48),
                            MHEN, 1, , 14, MHBR, 24,
                            Offset(0x54),
                            DVEN, 32,
                            Offset(0x60),
                            PXEN, 1, PXSZ, 2, , 23, PXBR, 13,
                            Offset(0x68),
                            DMEN, 1, , 11, DMBR, 27,
                            Offset(0x70),
                            MEBA, 64,
                            Offset(0x80),
                            , 4, PM0H, 2, , 2,
                            PM1L, 2, , 2, PM1H, 2, , 2,
                            PM2L, 2, , 2, PM2H, 2, , 2,
                            PM3L, 2, , 2, PM3H, 2, , 2,
                            PM4L, 2, , 2, PM4H, 2, , 2,
                            PM5L, 2, , 2, PM5H, 2, , 2,
                            PM6L, 2, , 2, PM6H, 2, , 2,
                            Offset(0xA0),
                            TOM_, 64,
                            Offset(0xBC),
                            TLUD, 32,
                        }
                    }

                    Device("PDRC") {
                        Name("_HID", EisaId("PNP0C02"));
                        Name("_UID", 1u32);
                        Name("_CRS", ResourceTemplate {
                            Memory32Fixed(ReadWrite, #{rcba}, 0x4000u32);
                            Memory32Fixed(ReadWrite, #{mchbar}, 0x8000u32);
                            Memory32Fixed(ReadWrite, #{dmibar}, 0x1000u32);
                            Memory32Fixed(ReadWrite, #{epbar}, 0x1000u32);
                            Memory32Fixed(ReadWrite, 0xFED20000u32, 0x00020000u32);
                            Memory32Fixed(ReadWrite, 0xFED40000u32, 0x00005000u32);
                            Memory32Fixed(ReadWrite, 0xFED45000u32, 0x0004B000u32);
                            Memory32Fixed(ReadWrite, 0x20000000u32, 0x00200000u32);
                            Memory32Fixed(ReadWrite, 0x40000000u32, 0x00200000u32);
                        });
                    }

                    Device("PEGP") { Name("_ADR", 0x00010000u32); Method("_STA", 0, NotSerialized) { Return(0u32); } Device("DEV0") { Name("_ADR", 0x00000000u32); } Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 16u32), Package(0x0000FFFFu32, 1u32, 0u32, 17u32), Package(0x0000FFFFu32, 2u32, 0u32, 18u32), Package(0x0000FFFFu32, 3u32, 0u32, 19u32))); }
                    Device("PEG1") { Name("_ADR", 0x00010001u32); Method("_STA", 0, NotSerialized) { Return(0u32); } Device("DEV0") { Name("_ADR", 0x00000000u32); } Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 17u32), Package(0x0000FFFFu32, 1u32, 0u32, 18u32), Package(0x0000FFFFu32, 2u32, 0u32, 19u32), Package(0x0000FFFFu32, 3u32, 0u32, 16u32))); }
                    Device("PEG2") { Name("_ADR", 0x00010002u32); Method("_STA", 0, NotSerialized) { Return(0u32); } Device("DEV0") { Name("_ADR", 0x00000000u32); } Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 18u32), Package(0x0000FFFFu32, 1u32, 0u32, 19u32), Package(0x0000FFFFu32, 2u32, 0u32, 16u32), Package(0x0000FFFFu32, 3u32, 0u32, 17u32))); }
                    Device("PEG6") { Name("_ADR", 0x00060000u32); Method("_STA", 0, NotSerialized) { Return(0u32); } Device("DEV0") { Name("_ADR", 0x00000000u32); } Name("_PRT", Package(Package(0x0000FFFFu32, 0u32, 0u32, 19u32), Package(0x0000FFFFu32, 1u32, 0u32, 16u32), Package(0x0000FFFFu32, 2u32, 0u32, 17u32), Package(0x0000FFFFu32, 3u32, 0u32, 18u32))); }

                    Name("MCRS", ResourceTemplate {
                        WordBusNumber(0x0000u16, #{bus_end});
                        DWordIO(0x0000u32, 0x0CF7u32);
                        IO(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
                        DWordIO(0x0D00u32, 0xFFFFu32);
                        DWordMemory(Cacheable, ReadWrite, 0x000A0000u32, 0x000BFFFFu32);
                        DWordMemory(Cacheable, ReadWrite, 0x000C0000u32, 0x000FFFFFu32);
                        DWordMemory(NotCacheable, ReadWrite, #{pci_mmio_base}, #{ecam_end});
                        DWordMemory(Cacheable, ReadWrite, 0xFED40000u32, 0xFED44FFFu32);
                    });
                    Method("_CRS", 0, Serialized) { Return(MCRS); }
                }
            }
        }

        fn extra_tables(&self, config: &Self::Config) -> Vec<Vec<u8>> {
            use fstart_acpi::Aml;

            let mut mcfg = fstart_acpi::mcfg::MCFG::new(
                fstart_acpi::OEM_ID,
                fstart_acpi::OEM_TABLE_ID,
                fstart_acpi::OEM_REVISION,
            );
            mcfg.add_ecam(
                config.ecam_base,
                0,
                0,
                config.ecam_buses.saturating_sub(1).min(255) as u8,
            );
            let mut bytes = Vec::new();
            mcfg.to_aml_bytes(&mut bytes);
            vec![bytes]
        }
    }
}
