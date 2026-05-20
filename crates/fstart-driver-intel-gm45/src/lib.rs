//! Intel GM45 (Cantiga) northbridge driver.
//!
//! This is the GM45/X200 counterpart to the Pineview northbridge driver.  It
//! performs the bootblock/romstage chipset setup that has to happen before the
//! ICH9-M southbridge and LPC-attached console can be used:
//!
//! - enable ECAM by programming PCIEXBAR through legacy CF8/CFC;
//! - map MCHBAR, DMIBAR and EPBAR;
//! - open the PAM shadow ranges;
//! - enable the integrated graphics functions used by the board;
//! - apply the early MCH/DMI tweaks from coreboot's GM45 port.

#![no_std]
#![allow(clippy::modulo_one)]

#[cfg(feature = "ffs-vbt")]
extern crate alloc;

pub mod raminit;

#[cfg(feature = "ffs-vbt")]
use alloc::vec::Vec;
use core::{cell::UnsafeCell, ptr};

use fstart_driver_pci_ecam::{PciEcam, PciEcamConfig};
use fstart_ecam as ecam;
use fstart_mmio::{MmioReadWrite, RawMmioBar};
use fstart_mp::{SmmError, SmmInfo, SmmOps};
use fstart_pci::pci_type0_config;
use fstart_services::device::{Device, DeviceError};
use fstart_services::memory_detect::{
    build_pc_compatible_e820, E820Entry, E820Kind, MemoryDetector,
};
use fstart_services::{
    EarlyInit, MemoryController, PciBdf, PciHost, PciRootBus, PciWindow, PostDramInit,
    PreConsoleInit, ServiceError, StageLocalInit,
};
use serde::{Deserialize, Serialize};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use tock_registers::{register_bitfields, register_structs};

/// GM45 fixed PCI locations and platform defaults.
pub mod hostbridge {
    /// Host bridge: bus 0, device 0, function 0.
    pub const HOST_DEV: u8 = 0;
    pub const HOST_FUNC: u8 = 0;

    pub const PEG_DEV: u8 = 1;
    pub const PEG_FUNC: u8 = 0;
    pub const IGD_DEV: u8 = 2;
    pub const IGD_FUNC: u8 = 0;
    pub const IGD_ALT_FUNC: u8 = 1;

    pub const DEFAULT_ECAM_BASE: usize = 0xe000_0000;
}

const HOST_PCIEXBAR_LO: u8 = 0x60;
const HOST_PCIEXBAR_HI: u8 = 0x64;

register_bitfields! [u32,
    /// DEVEN — device enable bits in GM45 host bridge PCI config space.
    pub DEVEN_REG [
        D0F0 OFFSET(0) NUMBITS(1) [],
        D1F0 OFFSET(1) NUMBITS(1) [],
        D2F0 OFFSET(3) NUMBITS(1) [],
        D2F1 OFFSET(4) NUMBITS(1) [],
        D3F1 OFFSET(7) NUMBITS(1) [],
        D3F2 OFFSET(8) NUMBITS(1) [],
        D3F3 OFFSET(9) NUMBITS(1) [],
        D4F0 OFFSET(14) NUMBITS(1) []
    ],
    /// FSBPMC5 — Front Side Bus Power Management Control 5.
    pub FSBPMC5_REG [
        NON_ISOCH_DECODE OFFSET(19) NUMBITS(2) []
    ],
    /// DMILCTL2 — DMI Link Control 2.
    pub DMILCTL2_REG [
        DEEMPH_EQ OFFSET(10) NUMBITS(2) []
    ],
    /// VC resource capability register fields used by DMI/egress setup.
    pub VC_CAP_REG [
        LOW_PRIORITY_EXTENDED_VC_COUNT OFFSET(0) NUMBITS(3) [],
        TC_VC0_MAP OFFSET(16) NUMBITS(7) []
    ],
    /// VC resource control register fields used by DMI/egress setup.
    pub VC_RCTL_REG [
        VC_ENABLE OFFSET(7) NUMBITS(1) [],
        TC_VC1_MAP OFFSET(16) NUMBITS(1) [],
        VC_ID OFFSET(24) NUMBITS(3) [],
        LOAD_PORT_ARB_TABLE OFFSET(31) NUMBITS(1) []
    ],
    /// Internal source/destination component ID fields.
    pub ROUTE_DESC_REG [
        TARGET_COMPONENT_ID OFFSET(16) NUMBITS(8) [],
        ENABLE OFFSET(0) NUMBITS(1) []
    ]
];

register_bitfields! [u16,
    /// VC resource status bits.
    pub VC_RSTS_REG [
        NEGOTIATION_PENDING OFFSET(0) NUMBITS(1) [],
        PORT_ARB_TABLE_STATUS OFFSET(1) NUMBITS(1) []
    ],
    /// IGD SWSCI register bits.
    pub IGD_SWSCI_REG [
        SCI_SELECT OFFSET(0) NUMBITS(1) [],
        SCI_TRIGGER OFFSET(15) NUMBITS(1) []
    ],
    /// IGD display clock control.
    pub IGD_DISPLAY_CLOCK_REG [
        CLOCK_SELECT OFFSET(0) NUMBITS(10) []
    ]
];

register_bitfields! [u8,
    /// SMRAM control register bits.
    pub SMRAM_REG [
        C_BASE_SEG OFFSET(0) NUMBITS(3) [],
        G_SMRAME OFFSET(3) NUMBITS(1) [],
        D_LCK OFFSET(4) NUMBITS(1) [],
        D_OPEN OFFSET(6) NUMBITS(1) []
    ],
    /// ESMRAMC control bits.
    pub ESMRAMC_REG [
        T_EN OFFSET(0) NUMBITS(1) [],
        TSEG_SIZE OFFSET(1) NUMBITS(2) []
    ],
    /// IGD MSAC aperture size control.
    pub IGD_MSAC_REG [
        APERTURE_SIZE OFFSET(0) NUMBITS(2) []
    ],
    /// IGD reset control.
    pub IGD_GDRST_REG [
        RESET OFFSET(0) NUMBITS(1) []
    ],
    /// IGD GCFGC low byte render/graphics mode fields.
    pub GCFGC_LO_REG [
        GMADR_RENDER OFFSET(4) NUMBITS(2) [],
        GMADR_FORMAT OFFSET(6) NUMBITS(2) []
    ],
    /// IGD GCFGC high byte stolen-memory field.
    pub GCFGC_HI_REG [
        STOLEN_MEMORY OFFSET(5) NUMBITS(3) []
    ]
];

register_structs! {
    /// Per-channel GM45 DRAM controller registers with 0x100-byte stride.
    pub Gm45MchDramChannelRegs {
        (0x0000 => pub drby: [MmioReadWrite<u32>; 2]),
        (0x0008 => pub dra: MmioReadWrite<u32>),
        (0x000c => pub dclkdis: MmioReadWrite<u32>),
        (0x0010 => pub drt: [MmioReadWrite<u32>; 7]),
        (0x002c => _pad0),
        (0x0030 => pub drc0: MmioReadWrite<u32>),
        (0x0034 => pub drc1: MmioReadWrite<u32>),
        (0x0038 => pub drc2: MmioReadWrite<u32>),
        (0x003c => _pad1),
        (0x0048 => pub odt_low: MmioReadWrite<u32>),
        (0x004c => pub odt_high: MmioReadWrite<u32>),
        (0x0050 => pub ait_lo: MmioReadWrite<u32>),
        (0x0054 => pub ait_hi: MmioReadWrite<u32>),
        (0x0058 => _pad2),
        (0x0060 => pub odt_misc: MmioReadWrite<u32>),
        (0x0064 => _pad3),
        (0x0068 => pub odt_timing: MmioReadWrite<u8>),
        (0x0069 => _pad4),
        (0x0074 => pub pwr_throttle1: MmioReadWrite<u32>),
        (0x0078 => _pad5),
        (0x00a0 => pub odt_ctrl: MmioReadWrite<u8>),
        (0x00a1 => _pad6),
        (0x00a4 => pub train_enable: MmioReadWrite<u32>),
        (0x00a8 => _pad7),
        (0x0100 => @END),
    }
}

register_structs! {
    /// Per-channel GM45 I/O training registers with 0x100-byte stride.
    pub Gm45MchIoTrainingChannelRegs {
        (0x0000 => pub io_init_cfg: MmioReadWrite<u32>),
        (0x0004 => _pad0),
        (0x000c => pub io_init_clk_dep: MmioReadWrite<u32>),
        (0x0010 => _pad1),
        (0x0014 => pub io_init_cfg2: MmioReadWrite<u32>),
        (0x0018 => pub io_init_cfg3: MmioReadWrite<u32>),
        (0x001c => pub io_init_cfg4: MmioReadWrite<u32>),
        (0x0020 => _pad2),
        (0x0028 => pub undoc_1428: MmioReadWrite<u32>),
        (0x002c => pub io_init_cfg5: MmioReadWrite<u32>),
        (0x0030 => _pad3),
        (0x0034 => pub dram_type_select: MmioReadWrite<u32>),
        (0x0038 => pub io_init_cfg6: MmioReadWrite<u32>),
        (0x003c => _pad4),
        (0x0040 => pub io_init_cfg7: MmioReadWrite<u32>),
        (0x0044 => pub io_rcomp_clk_en: MmioReadWrite<u32>),
        (0x0048 => _pad5),
        (0x0070 => pub wrty: [MmioReadWrite<u32>; 4]),
        (0x0080 => _pad6),
        (0x0084 => pub train_cfg: MmioReadWrite<u32>),
        (0x0088 => _pad7),
        (0x0090 => pub train_pi: [MmioReadWrite<u32>; 2]),
        (0x0098 => _pad8),
        (0x00a0 => pub recy: [MmioReadWrite<u32>; 4]),
        (0x00b0 => pub rdty: [MmioReadWrite<u32>; 8]),
        (0x00d0 => _pad9),
        (0x00f0 => pub rw_ptr_ctrl: MmioReadWrite<u32>),
        (0x00f4 => _pad10),
        (0x0100 => @END),
    }
}

impl Gm45MchIoTrainingChannelRegs {
    /// Write-timing register for logical group `0..4`.
    #[inline]
    pub fn wrty(&self, group: usize) -> &MmioReadWrite<u32> {
        &self.wrty[3 - group]
    }

    /// Receive-enable coarse timing register for logical group `0..4`.
    #[inline]
    pub fn recy(&self, group: usize) -> &MmioReadWrite<u32> {
        &self.recy[3 - group]
    }

    /// Read-timing register for logical lane `0..8`.
    #[inline]
    pub fn rdty(&self, lane: usize) -> &MmioReadWrite<u32> {
        &self.rdty[7 - lane]
    }
}

register_structs! {
    /// Sparse typed overlay for the early GM45 MCHBAR registers.
    pub Gm45MchBarRegs {
        (0x0000 => _pad0),
        (0x0040 => pub pm_ctrl0: MmioReadWrite<u32>),
        (0x0044 => pub pm_ctrl1: MmioReadWrite<u32>),
        (0x0048 => _pad1),
        (0x0090 => pub pm_nocarb: MmioReadWrite<u16>),
        (0x0092 => _pad2),
        (0x0094 => pub fsbpmc5: MmioReadWrite<u32, FSBPMC5_REG::Register>),
        (0x0098 => _pad3),
        (0x0b00 => pub pm_sched: MmioReadWrite<u16>),
        (0x0b02 => _pad4),
        (0x0b90 => pub pm_sched_b90: MmioReadWrite<u32>),
        (0x0b94 => _pad5),
        (0x0bd0 => pub igd_hsync_vsync: MmioReadWrite<u32>),
        (0x0bd4 => pub igd_hsync_vsync_hi: MmioReadWrite<u8>),
        (0x0bd5 => _pad6),
        (0x0bd8 => pub pm_bd8: MmioReadWrite<u32>),
        (0x0bdc => _pad7),
        (0x0c00 => pub clkcfg: MmioReadWrite<u32>),
        (0x0c04 => _pad8),
        (0x0c14 => pub clkcfg_c14: MmioReadWrite<u16>),
        (0x0c16 => pub clkcfg_c16: MmioReadWrite<u16>),
        (0x0c18 => _pad9),
        (0x0c1c => pub sskpd: MmioReadWrite<u16>),
        (0x0c1e => _pad10),
        (0x0c20 => pub clkcfg_c20: MmioReadWrite<u16>),
        (0x0c22 => _pad11),
        (0x0c38 => pub hgipmc2_lo: MmioReadWrite<u16>),
        (0x0c3a => pub hgipmc2_hi: MmioReadWrite<u16>),
        (0x0c3c => _pad12),
        (0x0f00 => pub c2c3tt: MmioReadWrite<u32>),
        (0x0f04 => pub c3c4tt: MmioReadWrite<u32>),
        (0x0f08 => pub pm_f08: MmioReadWrite<u16>),
        (0x0f0a => _pad13),
        (0x0f10 => pub pm_f10: MmioReadWrite<u16>),
        (0x0f12 => _pad14),
        (0x0f60 => pub pm_f60: MmioReadWrite<u32>),
        (0x0f64 => _pad15),
        (0x0f80 => pub pm_f80: MmioReadWrite<u32>),
        (0x0f84 => _pad16),
        (0x0fb0 => pub gipmc1: MmioReadWrite<u8>),
        (0x0fb1 => _pad17),
        (0x0fb8 => pub fsbpmc1: MmioReadWrite<u8>),
        (0x0fb9 => _pad18),
        (0x0fc0 => pub upmc3: MmioReadWrite<u32>),
        (0x0fc4 => _pad19),
        (0x10ef => pub thermal_enable: MmioReadWrite<u8>),
        (0x10f0 => _pad20),
        (0x1200 => pub dram_channel: [Gm45MchDramChannelRegs; 2]),
        (0x1400 => pub io_training_channel: [Gm45MchIoTrainingChannelRegs; 2]),
        (0x1600 => @END),
    }
}

register_structs! {
    /// Sparse typed overlay for the early GM45 DMIBAR registers.
    pub Gm45DmiBarRegs {
        (0x0000 => _pad0),
        (0x0004 => pub dmipvccap1: MmioReadWrite<u32, VC_CAP_REG::Register>),
        (0x0008 => _pad1),
        (0x0014 => pub dmivc0rctl: MmioReadWrite<u32, VC_RCTL_REG::Register>),
        (0x0018 => _pad2),
        (0x0020 => pub dmivc1rctl: MmioReadWrite<u32, VC_RCTL_REG::Register>),
        (0x0024 => _pad3),
        (0x0026 => pub dmivc1rsts: MmioReadWrite<u16, VC_RSTS_REG::Register>),
        (0x0028 => _pad4),
        (0x0044 => pub dmiesd: MmioReadWrite<u32, ROUTE_DESC_REG::Register>),
        (0x0048 => _pad5),
        (0x0050 => pub dmile1d: MmioReadWrite<u32, ROUTE_DESC_REG::Register>),
        (0x0054 => _pad6),
        (0x0058 => pub dmile1a: MmioReadWrite<u32>),
        (0x005c => _pad7),
        (0x0060 => pub dmile2d: MmioReadWrite<u32, ROUTE_DESC_REG::Register>),
        (0x0064 => _pad8),
        (0x0068 => pub dmile2a: MmioReadWrite<u32>),
        (0x006c => _pad9),
        (0x0084 => pub dmilcap: MmioReadWrite<u32>),
        (0x0088 => pub dmilctl: MmioReadWrite<u16>),
        (0x008a => _pad10),
        (0x0204 => pub dmilctl2: MmioReadWrite<u32, DMILCTL2_REG::Register>),
        (0x0208 => @END),
    }
}

register_structs! {
    /// Sparse typed overlay for GM45 EPBAR registers.
    pub Gm45EpBarRegs {
        (0x0000 => _pad0),
        (0x0004 => pub eppvccap1: MmioReadWrite<u32, VC_CAP_REG::Register>),
        (0x0008 => _pad1),
        (0x0014 => pub epvc0rctl: MmioReadWrite<u32, VC_RCTL_REG::Register>),
        (0x0018 => _pad2),
        (0x001c => pub epvc1rcap: MmioReadWrite<u32, VC_CAP_REG::Register>),
        (0x0020 => pub epvc1rctl: MmioReadWrite<u32, VC_RCTL_REG::Register>),
        (0x0024 => _pad3),
        (0x0026 => pub epvc1rsts: MmioReadWrite<u16, VC_RSTS_REG::Register>),
        (0x0028 => pub epvc1mts: MmioReadWrite<u32>),
        (0x002c => pub epvc1itc: MmioReadWrite<u32>),
        (0x0030 => _pad4),
        (0x0038 => pub epvc1ist: MmioReadWrite<u32>),
        (0x003c => _pad5),
        (0x0044 => pub epesd: MmioReadWrite<u32, ROUTE_DESC_REG::Register>),
        (0x0048 => _pad6),
        (0x0050 => pub eple1d: MmioReadWrite<u32, ROUTE_DESC_REG::Register>),
        (0x0054 => _pad7),
        (0x0058 => pub eple1a: MmioReadWrite<u32>),
        (0x005c => _pad8),
        (0x0060 => pub eple2d: MmioReadWrite<u32, ROUTE_DESC_REG::Register>),
        (0x0064 => _pad9),
        (0x0068 => pub eple2a: MmioReadWrite<u32>),
        (0x006c => _pad10),
        (0x0100 => pub portarb: [MmioReadWrite<u32>; 8]),
        (0x0120 => @END),
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
    fn regs(&self) -> &'static Gm45MchBarRegs {
        // SAFETY: the base address is programmed into D0:F0 MCHBAR by early init.
        unsafe { &*(self.base as *const Gm45MchBarRegs) }
    }

    /// Return the typed DRAM-channel register block for `ch`.
    #[inline]
    pub fn dram_channel(&self, ch: usize) -> &'static Gm45MchDramChannelRegs {
        &self.regs().dram_channel[ch]
    }

    /// Return the typed DRAM-channel register block for compile-time channel indices.
    #[inline]
    pub fn dram_channel_const<const CH: usize>(&self) -> &'static Gm45MchDramChannelRegs {
        const {
            assert!(CH < 2);
        }
        &self.regs().dram_channel[CH]
    }

    /// Return the typed I/O-training register block for `ch`.
    #[inline]
    pub fn io_training_channel(&self, ch: usize) -> &'static Gm45MchIoTrainingChannelRegs {
        &self.regs().io_training_channel[ch]
    }

    /// Return the typed I/O-training register block for compile-time channel indices.
    #[inline]
    pub fn io_training_channel_const<const CH: usize>(
        &self,
    ) -> &'static Gm45MchIoTrainingChannelRegs {
        const {
            assert!(CH < 2);
        }
        &self.regs().io_training_channel[CH]
    }

    #[inline]
    fn set_non_isoch_decode_mode_b(&self) {
        self.regs()
            .fsbpmc5
            .modify(FSBPMC5_REG::NON_ISOCH_DECODE.val(0b10));
    }
}

impl fstart_mmio::RawMmioBar for MchBar {
    const SIZE: usize = 0x4000;

    #[inline]
    fn base(&self) -> usize {
        self.base
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
    fn regs(&self) -> &'static Gm45DmiBarRegs {
        // SAFETY: the base address is programmed into D0:F0 DMIBAR by early init.
        unsafe { &*(self.base as *const Gm45DmiBarRegs) }
    }

    #[inline]
    fn clear_link_deemphasis_equalization(&self) {
        self.regs().dmilctl2.modify(DMILCTL2_REG::DEEMPH_EQ.val(0));
    }
}

impl fstart_mmio::RawMmioBar for DmiBar {
    const SIZE: usize = 0x4000;

    #[inline]
    fn base(&self) -> usize {
        self.base
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
    fn regs(&self) -> &'static Gm45EpBarRegs {
        // SAFETY: the base address is programmed into D0:F0 EPBAR by early init.
        unsafe { &*(self.base as *const Gm45EpBarRegs) }
    }
}

impl fstart_mmio::RawMmioBar for EpBar {
    const SIZE: usize = 0x4000;

    #[inline]
    fn base(&self) -> usize {
        self.base
    }
}

/// Integrated graphics configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gm45IgdConfig {
    /// Enable the integrated VGA function (D2:F0).
    #[serde(default = "default_true")]
    pub enable_vga: bool,
    /// Enable the secondary display function (D2:F1).
    #[serde(default = "default_true")]
    pub enable_pipe_b: bool,
    /// Fixed GTTMMADR BAR0 address used for non-display GMA setup.
    #[serde(default = "default_gtt_mmio_base")]
    pub gtt_mmio_base: u64,
    /// Board-relative VBT file path stored as a compressed FFS data file.
    #[serde(default)]
    pub vbt_file: Option<heapless::String<128>>,
    /// Raw VBT physical address, if firmware has staged a `vbt.bin` blob.
    #[serde(default)]
    pub vbt_addr: Option<u64>,
    /// Raw VBT size at `vbt_addr`.
    #[serde(default)]
    pub vbt_size: u32,
    /// Optional legacy VBIOS/VBT probe base.
    #[serde(default)]
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

impl Default for Gm45IgdConfig {
    fn default() -> Self {
        Self {
            enable_vga: true,
            enable_pipe_b: true,
            gtt_mmio_base: default_gtt_mmio_base(),
            vbt_file: None,
            vbt_addr: None,
            vbt_size: 0,
            legacy_vbt_probe: None,
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

const PCI_PIO_BASE: u64 = 0x1000;
const PCI_PIO_SIZE: u64 = 0xf000;

#[allow(clippy::large_enum_variant)]
enum VbtBytes<'a> {
    Borrowed(&'a [u8]),
    #[cfg(feature = "ffs-vbt")]
    Owned(Vec<u8>),
}

impl VbtBytes<'_> {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            #[cfg(feature = "ffs-vbt")]
            Self::Owned(bytes) => bytes.as_slice(),
        }
    }
}

static IGD_OPREGION: fstart_igd_opregion::IgdOpRegionStore =
    fstart_igd_opregion::IgdOpRegionStore::new();

// GM45/ICH9M SMM constants. SMRAM bit definitions match coreboot's
// `cpu/intel/smm/gen1/smmrelocate.c`; PM I/O offsets live in
// `fstart-pmio-ich`.
const SMRAM_G_SMRAME: u8 = SMRAM_REG::G_SMRAME::SET.value;
const SMRAM_D_LCK: u8 = SMRAM_REG::D_LCK::SET.value;
const SMRAM_D_OPEN: u8 = SMRAM_REG::D_OPEN::SET.value;
const SMRAM_C_BASE_SEG: u8 = SMRAM_REG::C_BASE_SEG.val(0b010).value;
const ICH9_PMBASE: u16 = 0x0500;
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

static GM45_SMM_CPU_LAYOUTS: CpuLayoutStore = CpuLayoutStore(UnsafeCell::new(
    [ZERO_CPU_LAYOUT; fstart_smm::runtime::MAX_SMM_CPUS],
));

/// GM45 northbridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelGm45Config {
    /// MCHBAR base address. X200 uses `0xfed14000`.
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
    pub igd: Gm45IgdConfig,
    /// SMBus I/O base used for DIMM SPD probing during raminit.
    #[serde(default = "default_smbus_base")]
    pub smbus_base: u16,
    /// SPD EEPROM addresses in GM45 slot order: ch0 slot0/1, ch1 slot0/1.
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

/// Intel GM45 northbridge driver.
pub struct IntelGm45 {
    config: &'static IntelGm45Config,
    detected_size: u64,
    pci: Option<PciEcam>,
}

// SAFETY: firmware performs chipset init on the BSP before concurrency exists.
unsafe impl Send for IntelGm45 {}
// SAFETY: the struct contains only immutable config and hardware register bases.
unsafe impl Sync for IntelGm45 {}

pci_type0_config! {
    /// GM45 host bridge PCI configuration space.
    pub struct Gm45HostBridgePciConfig {
        (0x40 => pub epbar_lo: MmioReadWrite<u32>),
        (0x44 => pub epbar_hi: MmioReadWrite<u32>),
        (0x48 => pub mchbar_lo: MmioReadWrite<u32>),
        (0x4c => pub mchbar_hi: MmioReadWrite<u32>),
        (0x50 => _reserved_hb0),
        (0x52 => pub ggc: MmioReadWrite<u16>),
        (0x54 => pub deven: MmioReadWrite<u32, DEVEN_REG::Register>),
        (0x58 => _reserved_hb1),
        (0x68 => pub dmibar_lo: MmioReadWrite<u32>),
        (0x6c => pub dmibar_hi: MmioReadWrite<u32>),
        (0x70 => _reserved_hb2),
        (0x78 => pub pmbase: MmioReadWrite<u16>),
        (0x7a => _reserved_hb3),
        (0x90 => pub pam: [MmioReadWrite<u8>; 7]),
        (0x97 => _reserved_hb4),
        (0x98 => pub remapbase: MmioReadWrite<u16>),
        (0x9a => pub remaplimit: MmioReadWrite<u16>),
        (0x9c => _reserved_hb5),
        (0x9d => pub smram: MmioReadWrite<u8, SMRAM_REG::Register>),
        (0x9e => pub esmramc: MmioReadWrite<u8, ESMRAMC_REG::Register>),
        (0x9f => _reserved_hb6),
        (0xa0 => pub tom: MmioReadWrite<u16>),
        (0xa2 => pub touud: MmioReadWrite<u16>),
        (0xa4 => _reserved_hb7),
        (0xb0 => pub tolud: MmioReadWrite<u16>),
        (0xb2 => _reserved_hb8),
        (0xdc => pub skpd: MmioReadWrite<u32>),
        (0xe0 => pub capid0: MmioReadWrite<u32>),
        (0xe4 => @END),
    }
}

pci_type0_config! {
    /// GM45 integrated graphics PCI configuration space.
    pub struct Gm45IgdPciConfig {
        (0x40 => _reserved_igd0),
        (0x5c => pub bsm: MmioReadWrite<u32>),
        (0x60 => _reserved_igd1),
        (0x62 => pub msac: MmioReadWrite<u8, IGD_MSAC_REG::Register>),
        (0x63 => _reserved_igd2),
        (0xc0 => pub gdrst: MmioReadWrite<u8, IGD_GDRST_REG::Register>),
        (0xc1 => _reserved_igd3),
        (0xcc => pub display_clock: MmioReadWrite<u16, IGD_DISPLAY_CLOCK_REG::Register>),
        (0xce => _reserved_igd4),
        (0xe8 => pub swsci: MmioReadWrite<u16, IGD_SWSCI_REG::Register>),
        (0xea => _reserved_igd5),
        (0xf0 => pub gcfgc_lo: MmioReadWrite<u8, GCFGC_LO_REG::Register>),
        (0xf1 => pub gcfgc_hi: MmioReadWrite<u8, GCFGC_HI_REG::Register>),
        (0xf2 => _reserved_igd6),
        (0xfc => pub asls: MmioReadWrite<u32>),
        (0x100 => @END),
    }
}

impl IntelGm45 {
    fn hostbridge_regs(&self) -> &'static Gm45HostBridgePciConfig {
        let hb = ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC);
        // SAFETY: GM45 host bridge is fixed at 00:00.0 and ECAM is live
        // before callers use the overlay.
        unsafe { hb.regs::<Gm45HostBridgePciConfig>() }
    }

    fn igd_regs(&self) -> &'static Gm45IgdPciConfig {
        let igd = ecam::EcamDevice::new(0, hostbridge::IGD_DEV, hostbridge::IGD_FUNC);
        // SAFETY: GM45 IGD is fixed at 00:02.0 and ECAM is live before
        // callers use the overlay. Callers check device presence as needed.
        unsafe { igd.regs::<Gm45IgdPciConfig>() }
    }

    fn igd_alt_regs(&self) -> &'static Gm45IgdPciConfig {
        let igd = ecam::EcamDevice::new(0, hostbridge::IGD_DEV, hostbridge::IGD_ALT_FUNC);
        // SAFETY: GM45 secondary IGD function is fixed at 00:02.1 and ECAM is
        // live before callers use the overlay. Callers check presence first.
        unsafe { igd.regs::<Gm45IgdPciConfig>() }
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
            fstart_pio::pci_cfg_write32(0, 0, 0, HOST_PCIEXBAR_HI, 0);
            fstart_pio::pci_cfg_write32(0, 0, 0, HOST_PCIEXBAR_LO, value);
        }
        ecam::init(self.config.ecam_base as usize);
        fstart_log::info!("gm45: ECAM enabled at {:#x}", self.config.ecam_base);
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn enable_ecam(&self) {
        ecam::init(self.config.ecam_base as usize);
        fstart_log::info!("gm45: ECAM enable (stub, non-x86)");
    }

    fn setup_bars_and_pam(&self) {
        let hb = self.hostbridge_regs();

        hb.mchbar_lo.set((self.config.mchbar as u32) | 1);
        hb.mchbar_hi.set(0);
        hb.dmibar_lo.set((self.config.dmibar as u32) | 1);
        hb.dmibar_hi.set(0);
        hb.epbar_lo.set((self.config.epbar as u32) | 1);
        hb.epbar_hi.set(0);
        hb.pmbase.set(ICH9_PMBASE | 1);

        // Coreboot opens C0000-FFFFF as RAM read/write shadow before option
        // ROM/VBT probing: PAM0=0x30, PAM1..PAM6=0x33.
        hb.pam[0].set(0x30);
        for pam in &hb.pam[1..] {
            pam.set(0x33);
        }

        // Coreboot GM45 early_init does not rewrite DEVEN; X200 keeps ME
        // function 00:03.0 enabled and only disables selected functions later
        // from devicetree policy.  Preserve firmware/default enables here and
        // only make sure host/PEG/IGD functions required by fstart are on.
        let me_functions =
            DEVEN_REG::D3F1::SET.value | DEVEN_REG::D3F2::SET.value | DEVEN_REG::D3F3::SET.value;
        let mut deven = (hb.deven.get() | DEVEN_REG::D0F0::SET.value | DEVEN_REG::D1F0::SET.value)
            & !me_functions;
        if self.config.igd.enable_vga {
            deven |= DEVEN_REG::D2F0::SET.value;
        }
        if self.config.igd.enable_pipe_b {
            deven |= DEVEN_REG::D2F1::SET.value;
        }
        hb.deven.set(deven);
    }

    fn early_mch_dmi_tweaks(&self) {
        self.mchbar().set_non_isoch_decode_mode_b();
        self.dmibar().clear_link_deemphasis_equalization();
    }

    fn read_detected_size(&self) -> u64 {
        let hb = self.hostbridge_regs();
        let touud = (hb.touud.get() as u64) << 20;
        if touud != 0 {
            return touud;
        }
        let tolud = ((hb.tolud.get() as u64) & 0xfff0) << 16;
        if tolud != 0 {
            tolud
        } else {
            self.detected_size
        }
    }

    fn tom(&self) -> u64 {
        let hb = self.hostbridge_regs();
        (u64::from(hb.tom.get() & 0x01ff)) << 27
    }

    fn touud(&self) -> u64 {
        let hb = self.hostbridge_regs();
        u64::from(hb.touud.get()) << 20
    }

    fn tolud(&self) -> u32 {
        let hb = self.hostbridge_regs();
        (u32::from(hb.tolud.get() & 0xfff0)) << 16
    }

    fn igd_stolen_base(&self) -> u32 {
        let hb = self.hostbridge_regs();
        if !hb.deven.is_set(DEVEN_REG::D2F0) {
            return 0;
        }
        self.igd_regs().bsm.get()
    }

    fn tseg_size(&self) -> u32 {
        let esmramc = self.hostbridge_regs().esmramc.get();
        if esmramc & 1 == 0 {
            return 0;
        }
        match (esmramc >> 1) & 3 {
            0 => 1024 * 1024,
            1 => 2 * 1024 * 1024,
            2 => 8 * 1024 * 1024,
            _ => {
                fstart_log::error!("gm45: bad TSEG size encoding");
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
        self.hostbridge_regs().smram.set(val);
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
        let pm = fstart_pmio_ich::PmIo::new(ICH9_PMBASE);
        pm.setbits32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::APMC_EN | fstart_pmio_ich::GBL_SMI_EN | fstart_pmio_ich::EOS,
        );
    }

    fn init_egress(&self) -> Result<(), ServiceError> {
        let ep = self.epbar();
        let regs = ep.regs();
        regs.epvc0rctl.set(regs.epvc0rctl.get() & 1);
        regs.eppvccap1
            .modify(VC_CAP_REG::LOW_PRIORITY_EXTENDED_VC_COUNT.val(1));
        regs.epvc1mts.set(0x0a0a_0a0a);
        regs.epvc1rcap.modify(VC_CAP_REG::TC_VC0_MAP.val(0x0a));
        regs.epvc1rctl.modify(
            VC_RCTL_REG::VC_ID.val(1) + VC_RCTL_REG::VC_ENABLE::SET + VC_RCTL_REG::TC_VC1_MAP::SET,
        );
        for portarb in regs.portarb.iter().take(7) {
            portarb.set(0x5555_5555);
        }
        regs.portarb[7].set(0x0000_5555);

        let mut timeout = 0x7ffffu32;
        while regs.epvc1rsts.is_set(VC_RSTS_REG::NEGOTIATION_PENDING) && timeout != 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            return Err(ServiceError::Timeout);
        }

        regs.epvc1rctl.modify(VC_RCTL_REG::LOAD_PORT_ARB_TABLE::SET);
        timeout = 0x7ffff;
        while regs.epvc1rsts.is_set(VC_RSTS_REG::PORT_ARB_TABLE_STATUS) && timeout != 0 {
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
        let regs = dmi.regs();
        regs.dmivc0rctl.set(regs.dmivc0rctl.get() & 1);
        regs.dmipvccap1
            .modify(VC_CAP_REG::LOW_PRIORITY_EXTENDED_VC_COUNT.val(1));
        regs.dmivc1rctl.modify(
            VC_RCTL_REG::VC_ID.val(1)
                + VC_RCTL_REG::VC_ENABLE::SET
                + VC_RCTL_REG::LOAD_PORT_ARB_TABLE::SET,
        );

        let mut timeout = 0x7ffffu32;
        while regs.dmivc1rsts.is_set(VC_RSTS_REG::PORT_ARB_TABLE_STATUS) && timeout != 0 {
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
        regs.dmilctl2.modify(DMILCTL2_REG::DEEMPH_EQ.val(0));
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
        let ep_regs = ep.regs();
        let dmi = self.dmibar();
        let dmi_regs = dmi.regs();
        ep_regs
            .epesd
            .modify(ROUTE_DESC_REG::TARGET_COMPONENT_ID.val(1));
        ep_regs
            .eple1d
            .modify(ROUTE_DESC_REG::TARGET_COMPONENT_ID.val(1) + ROUTE_DESC_REG::ENABLE::SET);
        ep_regs.eple1a.set(self.config.dmibar as u32);

        let peg = ecam::EcamDevice::new(0, hostbridge::PEG_DEV, hostbridge::PEG_FUNC);
        if peg.read8(0) != 0xff {
            ep_regs
                .eple2d
                .modify(ROUTE_DESC_REG::TARGET_COMPONENT_ID.val(1) + ROUTE_DESC_REG::ENABLE::SET);
            peg.modify32(0x0144, !(0xff << 16), 1 << 16);
            peg.write32(0x0158, self.config.epbar as u32);
            peg.modify32(0x0150, !(0xff << 16), (1 << 16) | 1);
        }

        dmi_regs
            .dmiesd
            .modify(ROUTE_DESC_REG::TARGET_COMPONENT_ID.val(1));
        dmi_regs.dmile1a.set(0xfed1_c000);
        dmi_regs
            .dmile1d
            .modify(ROUTE_DESC_REG::TARGET_COMPONENT_ID.val(2) + ROUTE_DESC_REG::ENABLE::SET);
        // Coreboot `setup_rcrb()`: DMIBAR link2 points back to component ID 1
        // (MCH), so the target address is MCHBAR, not EPBAR.
        dmi_regs.dmile2a.set(self.config.mchbar as u32);
        dmi_regs
            .dmile2d
            .modify(ROUTE_DESC_REG::TARGET_COMPONENT_ID.val(1) + ROUTE_DESC_REG::ENABLE::SET);
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
        let regs = dmi.regs();
        regs.dmilctl.set(regs.dmilctl.get() | 3);
        regs.dmilcap
            .set((regs.dmilcap.get() & !(63 << 12)) | (2 << 12) | (2 << 15));
        dmi.write8(0x0208 + 3, 0);
        dmi.clrbits32(0x0208, 3 << 20);
    }

    fn gm45_dmi_init(&self) -> Result<(), ServiceError> {
        self.init_egress()?;
        self.init_dmi()?;
        self.setup_rcrb();
        self.setup_aspm();
        fstart_log::info!("intel-gm45: DMI/egress link init complete");
        Ok(())
    }

    fn stepping(&self) -> u8 {
        ecam::EcamDevice::new(0, hostbridge::HOST_DEV, hostbridge::HOST_FUNC).read8(0x08)
    }

    fn fsb_clock_index(&self) -> usize {
        let raw = (self.mchbar().regs().clkcfg.get() & 0x7) as usize;
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
        unsafe { (fstart_arch_x86::x86::msr::rdmsr(0x00ee) & (1 << 27)) != 0 }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn cpu_supports_slfm(&self) -> bool {
        false
    }

    fn gm45_pm_init(&self) {
        const HGIPMC2_HI: [u16; 4] = [0, 0x0c3d, 0x125c, 0x0f4c];
        const HGIPMC2_LO: [u16; 4] = [0, 0x0bb8, 0x1194, 0x0ea6];
        const CLKCFG_C16: [u8; 4] = [0, 0x0b, 0x10, 0x0d];
        const PM_F00: [u32; 4] = [0, 0x0000_0480, 0x0000_0700, 0x0000_0600];
        const PM_F04: [u32; 4] = [0, 0x0000_1780, 0x0000_2380, 0x0000_1d80];

        let mch = self.mchbar();
        let regs = mch.regs();
        let fsb = self.fsb_clock_index();
        let stepping = self.stepping();
        let peg = ecam::EcamDevice::new(0, hostbridge::PEG_DEV, hostbridge::PEG_FUNC);

        regs.clkcfg_c14.set(0x0010);
        if peg.read8(0) == 0xff {
            regs.clkcfg_c14.set(regs.clkcfg_c14.get() | 0x21);
        }
        regs.clkcfg_c20.set(0x0001);
        regs.upmc3.set(if stepping == 0x00 {
            0x041f_06fd
        } else if stepping == 0x01 {
            0x041f_0efd
        } else {
            0x061f_0efd
        });
        regs.gipmc1.set(0x03);
        regs.pm_f10.set(regs.pm_f10.get() | (1 << 1));
        regs.hgipmc2_hi.set(HGIPMC2_HI[fsb]);
        regs.clkcfg_c16
            .set((regs.clkcfg_c16.get() & !0x7f) | CLKCFG_C16[fsb] as u16);
        regs.fsbpmc1.set(0x03);
        regs.hgipmc2_lo.set(HGIPMC2_LO[fsb]);
        regs.pm_f10.set(regs.pm_f10.get() | (1 << 5));
        regs.clkcfg_c16
            .set((regs.clkcfg_c16.get() & 0xc3ff) | 0x3400);
        regs.pm_f60.set(0x0103_0419);
        regs.c2c3tt.set(PM_F00[fsb]);
        regs.c3c4tt.set(PM_F04[fsb]);
        regs.pm_f08.set(0x730f);
        regs.pm_f80.set(regs.pm_f80.get() | (1 << 31));
        regs.pm_ctrl0.set(
            (regs.pm_ctrl0.get() & !((1 << 19) | (1 << 13))) | (1 << 21) | (1 << 9) | (1 << 2),
        );
        let mut ctrl1 = regs.pm_ctrl1.get() & 0xfeff_ffff;
        ctrl1 |= 0x4220_0020;
        if stepping != 0 {
            ctrl1 |= 0x10;
        }
        regs.pm_ctrl1.set(ctrl1);
        regs.pm_nocarb.set((regs.pm_nocarb.get() & !0x07) | 0x04);
        regs.fsbpmc5
            .set((regs.fsbpmc5.get() & !(1 << 18)) | (1 << 29) | (1 << 13) | (1 << 11) | (1 << 8));
        if stepping > 0x01 {
            regs.fsbpmc5.set(regs.fsbpmc5.get() | (1 << 5) | (1 << 4));
        }
        regs.pm_sched.set(regs.pm_sched.get() | 1);
        regs.pm_sched_b90
            .set(regs.pm_sched_b90.get() & !((1 << 23) | (1 << 7)));
        regs.pm_bd8.set(regs.pm_bd8.get() | 0x0c);

        if self.cpu_supports_slfm() {
            regs.clkcfg.set((regs.clkcfg.get() & !(1 << 7)) | (1 << 14));
            regs.pm_ctrl1.set(regs.pm_ctrl1.get() | (1 << 31));
        } else {
            regs.clkcfg.set((regs.clkcfg.get() & !(1 << 14)) | (1 << 7));
            regs.pm_ctrl1.set(regs.pm_ctrl1.get() & !(1 << 31));
        }

        fstart_log::info!(
            "intel-gm45: PM init complete stepping={} fsb_idx={}",
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
            self.mchbar().regs().thermal_enable.set(0xd0);
            fstart_log::info!("intel-gm45: DIMM thermal sensors enabled");
        }
    }

    fn igd_enabled(&self) -> bool {
        self.config.igd.enable_vga
            && self.hostbridge_regs().deven.is_set(DEVEN_REG::D2F0)
            && self.igd_regs().vendor_id.get() != 0xffff
    }

    #[cfg(feature = "ffs-vbt")]
    fn ffs_vbt(&self) -> Option<Vec<u8>> {
        let file_name = self.config.igd.vbt_file.as_ref()?;
        let ctx = fstart_services::ffs_context::memory_mapped()?;
        // SAFETY: the generated stage publishes a static anchor and a valid
        // memory-mapped boot-media window when BootMedia runs.
        let anchor_bytes = unsafe { ctx.anchor_bytes() };
        let image = unsafe { ctx.image_bytes() };
        let anchor = unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_bytes).ok()? };
        let image_size = if anchor.total_image_size > 0 {
            (anchor.total_image_size as usize).min(image.len())
        } else {
            image.len()
        };
        let image = &image[..image_size];
        let manifest = fstart_ffs::FfsReader::new(image)
            .read_manifest(&anchor)
            .ok()?;

        for region in &manifest.regions {
            let fstart_types::ffs::RegionContent::Container { children } = &region.content else {
                continue;
            };
            for entry in children {
                if entry.name.as_str() != file_name.as_str() {
                    continue;
                }
                let fstart_types::ffs::EntryContent::File {
                    file_type,
                    segments,
                    digests,
                } = &entry.content
                else {
                    return None;
                };
                if *file_type != fstart_types::ffs::FileType::Data || segments.len() != 1 {
                    return None;
                }
                let seg = segments.first()?;
                let offset = (region.offset + entry.offset + seg.offset) as usize;
                let stored_size = seg.stored_size as usize;
                let end = offset.checked_add(stored_size)?;
                let stored = image.get(offset..end)?;
                let mut out = Vec::new();
                match seg.compression {
                    fstart_types::ffs::Compression::None => {
                        out.extend_from_slice(stored);
                    }
                    fstart_types::ffs::Compression::Lz4 => {
                        out.resize(seg.loaded_size as usize, 0);
                        let len =
                            fstart_ffs::lz4::decompress_block(stored, out.as_mut_slice()).ok()?;
                        out.truncate(len);
                    }
                }
                fstart_crypto::digest::verify_digest_set(out.as_slice(), digests).ok()?;
                let vbt_size = fstart_igd_opregion::vbt_size(out.as_slice())?;
                out.truncate(vbt_size);
                return Some(out);
            }
        }
        None
    }

    fn configured_vbt(&self) -> Option<&'static [u8]> {
        let addr = self.config.igd.vbt_addr? as usize;
        let size = self.config.igd.vbt_size as usize;
        if size == 0 {
            return None;
        }
        // SAFETY: board config promises this physical address contains a raw VBT blob.
        let bytes = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
        fstart_igd_opregion::vbt_size(bytes).map(|vbt_size| &bytes[..vbt_size])
    }

    fn legacy_vbt(&self) -> Option<&'static [u8]> {
        let base = self.config.igd.legacy_vbt_probe? as usize;
        fstart_igd_opregion::legacy_vbt(base)
    }

    fn locate_vbt(&self) -> Option<VbtBytes<'static>> {
        #[cfg(feature = "ffs-vbt")]
        if let Some(vbt) = self.ffs_vbt() {
            return Some(VbtBytes::Owned(vbt));
        }
        self.configured_vbt()
            .or_else(|| self.legacy_vbt())
            .map(VbtBytes::Borrowed)
    }

    fn init_igd_opregion(&self) {
        if !self.igd_enabled() {
            return;
        }

        let Some(vbt) = self.locate_vbt() else {
            fstart_log::error!("intel-gm45: no valid VBT found for IGD opregion");
            return;
        };
        let vbt = vbt.as_slice();

        // SAFETY: BSP-only initialization before handing ASLS to the OS.
        let opregion_addr = unsafe {
            IGD_OPREGION.with_mut(|opregion| fstart_igd_opregion::build_opregion(opregion, vbt))
        };

        let igd = self.igd_regs();
        igd.asls.set(opregion_addr as u32);
        igd.swsci
            .modify(IGD_SWSCI_REG::SCI_SELECT::CLEAR + IGD_SWSCI_REG::SCI_TRIGGER::SET);
        fstart_log::info!(
            "intel-gm45: IGD opregion at {:#x}, VBT {} bytes",
            opregion_addr,
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
        let gcfgc_hi = self.igd_regs().gcfgc_hi.get() as u16;
        let cdclk_sel = (gcfgc_hi & 0x1f).saturating_sub(1) as usize;
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
        let hb = self.hostbridge_regs();
        let tolud = ((hb.tolud.get() as u32) & 0xfff0) << 16;
        if tolud < 512 * 1024 {
            return;
        }
        let gtt_base = tolud - 512 * 1024;
        self.gtt_mmio_write32(GFX_FLSH_CNTL, 0);
        self.gtt_mmio_write32(PGETBL_CTL, gtt_base | 1);
        self.gtt_mmio_write32(GFX_FLSH_CNTL, 0);
    }

    fn gm45_igd_init_no_display(&self) {
        let hb = self.hostbridge_regs();
        let deven = hb.deven.get();
        let peg = ecam::EcamDevice::new(0, hostbridge::PEG_DEV, hostbridge::PEG_FUNC);
        let peg_enabled = (deven & DEVEN_REG::D1F0::SET.value) != 0 && peg.read16(0) != 0xffff;
        let mch = self.mchbar();
        let mch_regs = mch.regs();
        if peg_enabled {
            mch_regs.pm_f10.set(mch_regs.pm_f10.get() | 1);
            if (mch.read8(0x0c0f) & 0x80) == 0 {
                mch.setbits32(0x1190, 1 << 14);
                mch.setbits16(0x119e, (1 << 15) | (1 << 12));
            }
        } else {
            mch_regs.igd_hsync_vsync.set(0xfd00_0000);
            mch_regs.igd_hsync_vsync_hi.set(0xfd);
            let vco_field = (self.igd_regs().gcfgc_hi.get() & 0x1f) as usize;
            let fsb_bits = (mch.read8(0x0c0f) & 0x07) as usize;
            const DISPLAY_CLOCK_TABLE: [[u16; 4]; 3] =
                [[200, 200, 222, 0], [320, 333, 333, 0], [400, 400, 381, 0]];
            if (1..=3).contains(&vco_field) && fsb_bits <= 3 {
                let clock = DISPLAY_CLOCK_TABLE[vco_field - 1][fsb_bits];
                if clock != 0 {
                    self.igd_regs()
                        .display_clock
                        .modify(IGD_DISPLAY_CLOCK_REG::CLOCK_SELECT.val(clock));
                }
            }
            mch_regs.pm_ctrl0.set(mch_regs.pm_ctrl0.get() | (1 << 31));
        }
    }

    fn gma_non_display_init(&self) {
        if !self.igd_enabled() {
            return;
        }
        let igd = self.igd_regs();
        igd.bar[0].set((self.config.igd.gtt_mmio_base as u32) & 0xfff0_0000);
        igd.command.modify(
            fstart_pci::PCI_COMMAND_BITS::MEMORY_SPACE::SET
                + fstart_pci::PCI_COMMAND_BITS::BUS_MASTER::SET,
        );
        igd.msac.modify(IGD_MSAC_REG::APERTURE_SIZE.val(2));
        self.init_igd_opregion();
        igd.gdrst.modify(IGD_GDRST_REG::RESET::SET);
        fstart_arch_x86::udelay(50);
        igd.gdrst.set(0);
        let mut timeout = 1_000_000u32;
        while igd.gdrst.is_set(IGD_GDRST_REG::RESET) && timeout != 0 {
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
            let igd_alt = self.igd_alt_regs();
            if igd_alt.vendor_id.get() != 0xffff {
                igd_alt
                    .command
                    .modify(fstart_pci::PCI_COMMAND_BITS::BUS_MASTER::SET);
            }
        }
        self.gma_pm_init_post_vbios();
        self.gm45_igd_init_no_display();
        fstart_log::info!("intel-gm45: IGD non-display init complete");
    }

    fn post_dram_chipset_init(&self) -> Result<(), ServiceError> {
        self.gm45_dmi_init()?;
        self.gm45_pm_init();
        self.gma_non_display_init();
        // Match coreboot GM45 romstage's post-raminit D4:F0 hide and the X200
        // devicetree's ME 03.1-03.3 disabled functions.
        let hb = self.hostbridge_regs();
        let me_functions =
            DEVEN_REG::D3F1::SET.value | DEVEN_REG::D3F2::SET.value | DEVEN_REG::D3F3::SET.value;
        hb.deven
            .set(hb.deven.get() & !(DEVEN_REG::D4F0::SET.value | me_functions));
        self.write_coreboot_scratchpad_marker();
        Ok(())
    }

    /// Mark the northbridge scratchpad the same way coreboot does after
    /// GM45 romstage completion. Useful for detecting a warm path later.
    pub fn write_coreboot_scratchpad_marker(&self) {
        self.mchbar().regs().sskpd.set(0xcafe);
    }
}

impl Device for IntelGm45 {
    const NAME: &'static str = "intel-gm45";
    const COMPATIBLE: &'static [&'static str] = &["intel,gm45", "intel,cantiga"];
    type Config = IntelGm45Config;

    fn new(config: &'static IntelGm45Config) -> Result<Self, DeviceError> {
        Ok(Self {
            config,
            detected_size: 0,
            pci: None,
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

impl IntelGm45 {
    fn pre_console_phase(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        Ok(())
    }

    fn early_phase(&mut self) -> Result<(), ServiceError> {
        self.setup_bars_and_pam();
        self.early_mch_dmi_tweaks();
        fstart_log::info!("intel-gm45: early init complete");
        Ok(())
    }
}

impl PreConsoleInit for IntelGm45 {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.pre_console_phase()
    }
}

impl EarlyInit for IntelGm45 {
    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.early_phase()
    }
}

impl StageLocalInit for IntelGm45 {
    fn stage_local_init(&mut self) -> Result<(), ServiceError> {
        self.enable_ecam();
        Ok(())
    }
}

impl PciHost for IntelGm45 {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.pre_console_phase()
    }

    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.early_phase()
    }
}

impl PostDramInit for IntelGm45 {
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.post_dram_chipset_init()
    }
}

impl IntelGm45 {
    fn pci_ecam_config(&self) -> PciEcamConfig {
        PciEcamConfig {
            ecam_base: self.config.ecam_base,
            ecam_size: self.ecam_size(),
            // Size 0 asks PciEcam to derive the 32-bit aperture from the
            // runtime e820 map published by GM45 MemoryDetect. The upper
            // limit is PCIEXBAR, because GM45 decodes ECAM at 0xe000_0000
            // on X200 and chipset fixed MMIO lives above that.
            mmio32_base: 0,
            mmio32_size: 0,
            mmio64_base: 0,
            mmio64_size: 0,
            // Reserve legacy/LPC fixed decodes below 0x1000.
            pio_base: PCI_PIO_BASE,
            pio_size: PCI_PIO_SIZE,
            bus_start: self.bus_start(),
            bus_end: self.bus_end(),
        }
    }

    fn ensure_pci_ecam(&mut self) -> Result<&mut PciEcam, ServiceError> {
        if self.pci.is_none() {
            let config = self.pci_ecam_config();
            self.pci =
                Some(PciEcam::from_config(&config).map_err(|_| ServiceError::HardwareError)?);
        }
        self.pci.as_mut().ok_or(ServiceError::NotInitialized)
    }

    fn pci_ecam(&self) -> Result<&PciEcam, ServiceError> {
        self.pci.as_ref().ok_or(ServiceError::NotInitialized)
    }
}

impl PciRootBus for IntelGm45 {
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

fn gm45_ramtest_probe(addr: usize, top: usize) -> Result<(), ServiceError> {
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

    fstart_log::info!("gm45 ramtest: testing DRAM at {:#x}", addr);
    // SAFETY: Called only after successful GM45 DRAM training. The caller
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
                    "gm45 ramtest: failed at {:#x}: wrote {:#x}, read {:#x}",
                    addr,
                    pattern,
                    got
                );
                return Err(ServiceError::HardwareError);
            }
        }
        ptr::write_volatile(p, old);
    }
    fstart_log::info!("gm45 ramtest: passed at {:#x}", addr);
    Ok(())
}

fn gm45_lower_memory_test(test_top: u32) -> Result<(), ServiceError> {
    let top = test_top as usize;
    if top <= 0x0010_0000 + core::mem::size_of::<u32>() {
        fstart_log::error!("gm45 ramtest: invalid usable top {:#x}", top);
        return Err(ServiceError::HardwareError);
    }

    gm45_ramtest_probe(0x0010_0000, top)?;

    let postcar_stack_probe = 0x0310_0000usize.saturating_sub(core::mem::size_of::<u32>());
    gm45_ramtest_probe(postcar_stack_probe, top)?;

    let high_probe = if top > 32 * 1024 * 1024 {
        top - 16 * 1024 * 1024
    } else {
        top - 4096
    };
    gm45_ramtest_probe(high_probe, top)
}

impl MemoryDetector for IntelGm45 {
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
                "gm45: invalid memory map TOM/TOUUD/TOLUD/usable {:#x}/{:#x}/{:#x}/{:#x}",
                tom,
                raw_touud,
                tolud,
                usable_top
            );
            return Err(ServiceError::HardwareError);
        }

        let count = build_pc_compatible_e820(entries, usable_top, touud, tolud)?;
        fstart_arch_x86::mtrr::set_ram_wb_ranges_from(
            entries[..count]
                .iter()
                .filter(|entry| entry.kind == E820Kind::Ram as u32)
                .map(|entry| (entry.addr, entry.size)),
        );
        fstart_log::info!(
            "gm45: detected memory map usable={:#x} TOLUD={:#x} TOM={:#x} TOUUD={:#x} TSEG={:#x}+{:#x}",
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

impl SmmOps for IntelGm45 {
    fn smm_info(&self) -> Option<SmmInfo> {
        let (base, size) = self.smm_region();
        if size == 0 {
            fstart_log::error!("gm45 SMM: TSEG is disabled");
            return None;
        }
        fstart_log::info!("gm45 SMM: TSEG base={:#x} size={:#x}", base, size);
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

        let layouts = unsafe { &mut *GM45_SMM_CPU_LAYOUTS.0.get() };
        let result = unsafe {
            fstart_smm::install_pic_image(
                image,
                fstart_smm::InstallConfig {
                    smram_base: info.smbase,
                    smram_size: info.smsize as u64,
                    num_cpus,
                    save_state_size: info.save_state_size as u32,
                    page_table_size: 0,
                    cr3: fstart_arch_x86::x86::controlregs::cr3(),
                    platform_kind: fstart_smm::SMM_PLATFORM_INTEL_ICH,
                    platform_flags: fstart_smm::SMM_PLATFORM_FLAG_ICH_GPE0_64BIT,
                    platform_data: [ICH9_PMBASE as u64, 0x20, 0, 0],
                },
                layouts,
            )
        };

        match result {
            Ok(installed) => {
                let targets = &installed.cpus[..num_cpus as usize];
                fstart_mp::prepare_default_smm_relocation(targets);
                let default_handler = unsafe {
                    fstart_smm::install_default_relocation_callback_stub(
                        image,
                        fstart_smm::DefaultRelocationCallbackConfig {
                            default_smbase: fstart_mp::SMM_DEFAULT_SMBASE,
                            cr3: fstart_arch_x86::x86::controlregs::cr3(),
                            callback: fstart_mp::default_smm_relocation_handler as *const ()
                                as usize as u64,
                            stack_top: fstart_mp::SMM_DEFAULT_ENTRY_STACK_TOP,
                        },
                    )
                };
                if default_handler.is_err() {
                    self.smm_close();
                    fstart_log::error!("gm45 SMM: failed to install default relocation handler");
                    return Err(SmmError::InstallFailed);
                }

                fstart_log::info!(
                    "gm45 SMM: installed image common={:#x} entry={:#x} cpus={}",
                    installed.common_base,
                    installed.common_entry,
                    installed.cpus.len()
                );
                Ok(())
            }
            Err(_) => {
                self.smm_close();
                fstart_log::error!("gm45 SMM: failed to install SMM image");
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
        let pm = fstart_pmio_ich::PmIo::new(ICH9_PMBASE);
        pm.reset_smi_status();
        pm.write32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::APMC_EN | fstart_pmio_ich::GBL_SMI_EN | fstart_pmio_ich::EOS,
        );
    }

    fn post_smm_init(&self) {
        self.smm_close();
        let pm = fstart_pmio_ich::PmIo::new(ICH9_PMBASE);
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
        fstart_log::info!("gm45 SMM: permanent SMI enabled and SMRAM locked");
    }
}

impl MemoryController for IntelGm45 {
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
        let count = build_pc_compatible_e820(&mut entries, usable_top, touud, tolud)?;
        fstart_arch_x86::mtrr::set_ram_wb_ranges_from(
            entries[..count]
                .iter()
                .filter(|entry| entry.kind == E820Kind::Ram as u32)
                .map(|entry| (entry.addr, entry.size)),
        );
        fstart_log::info!(
            "gm45: dynamic WB MTRR ranges set (TOLUD {:#x}, usable top {:#x})",
            tolud,
            usable_top
        );
        gm45_lower_memory_test(usable_top)
    }
}

// ---------------------------------------------------------------------------
// ACPI device implementation — GM45 host bridge / PCI0
// ---------------------------------------------------------------------------

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi::Aml;
    use fstart_acpi_macros::acpi_dsl;

    use super::*;

    impl AcpiDevice for IntelGm45 {
        type Config = IntelGm45Config;

        /// Produce GM45/X200 PCI root-bridge DSDT content.
        ///
        /// The generated `PCI0` scope mirrors the coreboot GM45 namespace at
        /// a boot-critical level: host-bridge identity, MCHC PCI config field
        /// access, PDRC reserved chipset MMIO ranges, PEG/GFX device stubs,
        /// root PCI resources, `_OSC`, `_PIC`, sleep states, and CPU device
        /// objects. Southbridge devices attach later through an absolute
        /// `\\_SB.PCI0` scope emitted by the ICH9 driver.
        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            let name = config.acpi_name.as_deref().unwrap_or("PCI0");
            let mchbar = config.mchbar as u32;
            let dmibar = config.dmibar as u32;
            let epbar = config.epbar as u32;
            let ecam_base = config.ecam_base as u32;
            let ecam_size =
                (u64::from(config.ecam_buses) * 1024 * 1024).min(u64::from(u32::MAX)) as u32;
            let pci_mmio_base = self.tolud().max(0x8000_0000);
            // Coreboot's GM45 hostbridge advertises the PCI MMIO aperture up
            // through the fixed IGD GTT/MMIO window below chipset MMIO.
            let pci_mmio_limit = 0xfebf_ffffu32;
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
                        DWordMemory(NotCacheable, ReadWrite, #{pci_mmio_base}, #{pci_mmio_limit});
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
                            Memory32Fixed(ReadWrite, #{ecam_base}, #{ecam_size});
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
                // S3 resume requires GM45 warm-boot raminit/CBMEM recovery;
                // do not advertise it until that path is implemented.
                Name("_S4_", Package(6u32, 4u32, 0u32, 0u32));
                Name("_S5_", Package(7u32, 0u32, 0u32, 0u32));
                // CPU power-management objects are appended below using the
                // coreboot-derived SpeedStep/C-state generator.
            });

            aml.extend_from_slice(&fstart_cpu_intel::core2::cpu_devices_aml(2));

            aml
        }

        /// Produce an MCFG table for the GM45 PCIEXBAR ECAM window.
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
