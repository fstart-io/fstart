//! Intel ICH8 / ICH8-M southbridge driver.
//!
//! The initial target is the Lenovo ThinkPad X61 (GM965 + ICH8-M/HX). The
//! reusable pre-console path opens the southbridge LPC/GPIO decode needed by
//! board hooks. X61-specific DLPC/dock SuperIO setup lives in the
//! `fstart-mainboard-lenovo-x61` crate.

#![no_std]

use fstart_ecam as ecam;
use fstart_gpio_ich::IchGpio;
use fstart_pmio_ich::{self as pmio, PmIo};
use fstart_services::device::{Device, DeviceError};
use fstart_services::{
    EarlyInit, FinalizeInit, PostDramInit, PreConsoleInit, ServiceError, SmBus, Southbridge,
};
use fstart_smbus_intel::I801SmBus;
use heapless::Vec as HVec;
use serde::{Deserialize, Serialize};

pub use fstart_gpio_ich::{GpioConfig, GpioDir, GpioLevel, GpioMode, GpioPin, GpioReset};
pub use fstart_hda::{
    HdaConfig, HdaController, HdaVerbTable, PinColor, PinConfig, PinConn, PinConnector, PinDevice,
    PinGeoLoc, PinLoc,
};

/// ICH8/ICH8-M PCI config and RCBA constants.
pub mod ich8 {
    pub const LAN_DEV: u8 = 0x19;
    pub const LAN_FUNC: u8 = 0;
    pub const HDA_DEV: u8 = 0x1b;
    pub const HDA_FUNC: u8 = 0;
    pub const PCIE_DEV: u8 = 0x1c;
    pub const PCI_BRIDGE_DEV: u8 = 0x1e;
    pub const PCI_BRIDGE_FUNC: u8 = 0;
    pub const LPC_DEV: u8 = 0x1f;
    pub const LPC_FUNC: u8 = 0;
    pub const IDE_DEV: u8 = 0x1f;
    pub const IDE_FUNC: u8 = 1;
    pub const SATA_DEV: u8 = 0x1f;
    pub const SATA_FUNC: u8 = 2;
    pub const SMBUS_DEV: u8 = 0x1f;
    pub const SMBUS_FUNC: u8 = 3;
    pub const SATA2_DEV: u8 = 0x1f;
    pub const SATA2_FUNC: u8 = 5;
    pub const THERMAL_DEV: u8 = 0x1f;
    pub const THERMAL_FUNC: u8 = 6;
    pub const UHCI1_DEV: u8 = 0x1d;
    pub const EHCI1_DEV: u8 = 0x1d;
    pub const EHCI1_FUNC: u8 = 7;
    pub const UHCI2_DEV: u8 = 0x1a;
    pub const EHCI2_DEV: u8 = 0x1a;
    pub const EHCI2_FUNC: u8 = 7;

    pub const PCI_DEVICE_ID: u16 = 0x02;
    pub const PCI_COMMAND: u16 = 0x04;
    pub const PCI_CMD_IO: u16 = 0x0001;
    pub const PCI_CMD_MEMORY: u16 = 0x0002;
    pub const PCI_CMD_MASTER: u16 = 0x0004;
    pub const PCI_CMD_SERR: u16 = 0x0100;
    pub const PCI_STATUS: u16 = 0x06;
    pub const PCI_CLASS_PROG: u16 = 0x09;
    pub const PCI_CACHE_LINE_SIZE: u16 = 0x0c;
    pub const PCI_BAR0: u16 = 0x10;
    pub const PCI_BAR5: u16 = 0x24;
    pub const PCI_INTERRUPT_LINE: u16 = 0x3c;
    pub const PCI_SEC_STATUS: u16 = 0x1e;
    pub const PCI_BRIDGE_CONTROL: u16 = 0x3e;

    pub const PMBASE: u16 = 0x40;
    pub const ACPI_CNTL: u16 = 0x44;
    pub const ACPI_EN: u8 = 0x80;
    pub const GPIOBASE: u16 = 0x48;
    pub const GPIO_CNTL: u16 = 0x4c;
    pub const GPIO_EN: u8 = 0x10;
    pub const PIRQA_ROUT: u16 = 0x60;
    pub const SERIRQ_CNTL: u16 = 0x64;
    pub const PIRQE_ROUT: u16 = 0x68;
    pub const LPC_IO_DEC: u16 = 0x80;
    pub const LPC_EN: u16 = 0x82;
    pub const GEN1_DEC: u16 = 0x84;
    pub const GEN2_DEC: u16 = 0x88;
    pub const GEN3_DEC: u16 = 0x8c;
    pub const GEN4_DEC: u16 = 0x90;
    pub const GEN_PMCON_1: u16 = 0xa0;
    pub const GEN_PMCON_3: u16 = 0xa4;
    pub const C5_EXIT_TIMING: u16 = 0xa8;
    pub const CXSTATE_CNF: u16 = 0xa9;
    pub const C4TIMING_CNT: u16 = 0xaa;
    pub const PMIR: u16 = 0xac;
    pub const PMIR_CF9GR: u32 = 1 << 20;
    pub const GPIO_ROUT: u16 = 0xb8;
    pub const RCBA: u16 = 0xf0;

    pub const SMB_BASE: u16 = 0x20;
    pub const HOSTC: u16 = 0x40;
    pub const HST_EN: u8 = 1;

    pub const DEFAULT_PMBASE: u16 = 0x0500;
    pub const DEFAULT_GPIOBASE: u16 = 0x0580;
    pub const DEFAULT_SMBUS_BASE: u16 = 0x0400;

    pub const DID_82801HBM_SATA: u16 = 0x2828;
    pub const DID_82801HBM_SATA_AHCI: u16 = 0x2829;
    pub const DID_82801HBM_SATA_RAID: u16 = 0x282a;

    pub const RCBA_V0CTL: u32 = 0x0014;
    pub const RCBA_V1CAP: u32 = 0x001c;
    pub const RCBA_V1CTL: u32 = 0x0020;
    pub const RCBA_V1STS: u32 = 0x0026;
    pub const RCBA_PAT: u32 = 0x0030;
    pub const RCBA_CIR1: u32 = 0x0088;
    pub const RCBA_ESD: u32 = 0x0104;
    pub const RCBA_ULD: u32 = 0x0110;
    pub const RCBA_ULBA: u32 = 0x0118;
    pub const RCBA_LCAP: u32 = 0x01a4;
    pub const RCBA_LCTL: u32 = 0x01a8;
    pub const RCBA_LSTS: u32 = 0x01aa;
    pub const RCBA_CIR2: u32 = 0x01f4;
    pub const RCBA_CIR3: u32 = 0x01fc;
    pub const RCBA_CIR4: u32 = 0x0200;
    pub const RCBA_BCR: u32 = 0x0220;
    pub const RCBA_DMIC: u32 = 0x0234;
    pub const RCBA_RPFN: u32 = 0x0238;
    pub const RCBA_CIR13: u32 = 0x0f20;
    pub const RCBA_CIR5: u32 = 0x1d40;
    pub const RCBA_DMC: u32 = 0x2010;
    pub const RCBA_CIR6: u32 = 0x2024;
    pub const RCBA_CIR7: u32 = 0x2034;
    pub const D31IP: u32 = 0x3100;
    pub const D30IP: u32 = 0x3104;
    pub const D29IP: u32 = 0x3108;
    pub const D28IP: u32 = 0x310c;
    pub const D27IP: u32 = 0x3110;
    pub const D26IP: u32 = 0x3114;
    pub const D25IP: u32 = 0x3118;
    pub const D31IR: u32 = 0x3140;
    pub const D30IR: u32 = 0x3142;
    pub const D29IR: u32 = 0x3144;
    pub const D28IR: u32 = 0x3146;
    pub const D27IR: u32 = 0x3148;
    pub const D26IR: u32 = 0x314c;
    pub const D25IR: u32 = 0x3150;
    pub const OIC: u32 = 0x31ff;
    pub const OIC_AEN: u8 = 1 << 0;
    pub const OIC_OAEN: u8 = 1 << 1;
    pub const IOTR3_LO: u32 = 0x1e98;
    pub const IOTR3_HI: u32 = 0x1e9c;
    pub const RCBA_HPTC: u32 = 0x3404;
    pub const GCS: u32 = 0x3410;
    pub const RCBA_FD: u32 = 0x3418;
    pub const RCBA_CG: u32 = 0x341c;
    pub const RCBA_FDSW: u32 = 0x3420;
    pub const FDSW_LAND: u32 = 1 << 0;
    pub const RCBA_CIR8: u32 = 0x3430;
    pub const RCBA_CIR9: u32 = 0x350c;
    pub const RCBA_CIR10: u32 = 0x352c;
    pub const RCBA_MAP: u32 = 0x35f0;

    pub const FD_SAD2: u32 = 1 << 25;
    pub const FD_TTD: u32 = 1 << 24;
    pub const FD_PE6D: u32 = 1 << 21;
    pub const FD_PE5D: u32 = 1 << 20;
    pub const FD_PE4D: u32 = 1 << 19;
    pub const FD_PE3D: u32 = 1 << 18;
    pub const FD_PE2D: u32 = 1 << 17;
    pub const FD_PE1D: u32 = 1 << 16;
    pub const FD_EHCI1D: u32 = 1 << 15;
    pub const FD_EHCI2D: u32 = 1 << 13;
    pub const FD_U5D: u32 = 1 << 12;
    pub const FD_U4D: u32 = 1 << 11;
    pub const FD_U3D: u32 = 1 << 10;
    pub const FD_U2D: u32 = 1 << 9;
    pub const FD_U1D: u32 = 1 << 8;
    pub const FD_HDAD: u32 = 1 << 4;
    pub const FD_SD: u32 = 1 << 3;
    pub const FD_SAD1: u32 = 1 << 2;

    pub const IDE_TIM_PRI: u16 = 0x40;
    pub const IDE_TIM_SEC: u16 = 0x42;
    pub const IDE_CONFIG: u16 = 0x54;
    pub const IDE_DECODE_ENABLE: u16 = 1 << 15;
    pub const IDE_SITRE: u16 = 1 << 14;
    pub const IDE_ISP_3_CLOCKS: u16 = 2 << 12;
    pub const IDE_RCT_1_CLOCKS: u16 = 3 << 8;
    pub const IDE_IE0: u16 = 1 << 1;
    pub const IDE_TIME0: u16 = 1 << 0;
    pub const FAST_SCB1: u32 = 1 << 15;
    pub const FAST_SCB0: u32 = 1 << 14;
    pub const FAST_PCB1: u32 = 1 << 13;
    pub const FAST_PCB0: u32 = 1 << 12;
    pub const SCB1: u32 = 1 << 3;
    pub const SCB0: u32 = 1 << 2;
    pub const PCB1: u32 = 1 << 1;
    pub const PCB0: u32 = 1 << 0;

    pub const SATA_IDE_TIM_PRI: u16 = 0x40;
    pub const SATA_IDE_TIM_SEC: u16 = 0x42;
    pub const SATA_MAP: u16 = 0x90;
    pub const SATA_PCS: u16 = 0x92;
    pub const SATA_CLK: u16 = 0x94;
    pub const SATA_SIDX: u16 = 0xa0;
    pub const SATA_SDAT: u16 = 0xa4;

    pub const EHCI_USBCMD: usize = 0x20;
    pub const EHCI_USBCMD_RS: u32 = 1 << 0;
    pub const EHCI_USBCMD_HCRST: u32 = 1 << 1;
    pub const EHCI_INTEL_FCREG: u16 = 0xfc;

    pub const D30F0_SMLT: u16 = 0x1b;

    pub const D28FX_XCAP: u16 = 0x42;
    pub const D28FX_XCAP_SLOT: u32 = 1 << 8;
    pub const D28FX_LCAP: u16 = 0x4c;
    pub const D28FX_LCTL: u16 = 0x50;
    pub const D28FX_SLCAP: u16 = 0x54;
    pub const D28FX_IOXAPIC: u16 = 0xd8;
    pub const D28FX_BBCLKG: u16 = 0xe1;
    pub const D28FX_ASPM_MOBILE: u16 = 0xe8;
    pub const D28FX_VC0RCTL: u16 = 0x114;
    pub const D28FX_CTTOMASK: u16 = 0x148;
    pub const D28FX_CEMASK: u16 = 0x154;
    pub const D28FX_CIR_300: u16 = 0x300;
    pub const D28FX_CIR_324: u16 = 0x324;
    pub const D28_SLCAP_SLOTNUM_SHIFT: u32 = 19;
    pub const D28_SLCAP_SCALE_SHIFT: u32 = 16;
    pub const D28_SLCAP_POWER_SHIFT: u32 = 7;
}

const HPET_BASE: usize = 0xfed0_0000;
const SATA_ABAR_BASE: usize = 0xfea0_0000;
const HDA_TEMP_BAR: usize = 0xfed1_0000;
const EHCI_TEMP_BAR: usize = 0xfed1_b000;
const GPE0_STS_ICH8: u16 = 0x20;
const GPE0_EN_ICH8: u16 = 0x28;
const SLP_TYP_S3: u32 = 0x1400;
const GEN_PMCON_3_RTC_POWER_FAILED: u8 = 1 << 1;
const GEN_PMCON_3_RTC_BATTERY_DEAD: u8 = 1 << 2;
const LPC_EN_CNF2: u16 = 1 << 13;
const LPC_EN_CNF1: u16 = 1 << 12;
const LPC_EN_MC: u16 = 1 << 11;
const LPC_EN_KBC: u16 = 1 << 10;
const LPC_EN_FDD: u16 = 1 << 3;
const LPC_EN_LPT: u16 = 1 << 2;
const LPC_EN_COMB: u16 = 1 << 1;
const LPC_EN_COMA: u16 = 1 << 0;
const LPC_EN_COREBOOT_BASE: u16 =
    LPC_EN_CNF2 | LPC_EN_CNF1 | LPC_EN_MC | LPC_EN_KBC | LPC_EN_COMB | LPC_EN_COMA;

/// SATA configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SataConfig {
    pub mode: SataMode,
    pub ports: u8,
    /// AHCI hot-plug port bitmap.
    #[serde(default)]
    pub hotplug_map: u8,
    /// Enable the SATA clock-request path when GPIO35 indicates it is usable.
    #[serde(default)]
    pub clock_request: bool,
    /// Enable the mobile SATA traffic monitor when C-state popup/popdown is enabled.
    #[serde(default)]
    pub traffic_monitor: bool,
}

/// ICH8-M PATA/IDE controller configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IdeConfig {
    /// Enable the primary PATA channel.
    #[serde(default)]
    pub enable_primary: bool,
    /// Enable the secondary PATA channel.
    #[serde(default)]
    pub enable_secondary: bool,
}

/// PCIe slot power-limit fields.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PciePowerLimit {
    /// Power-limit value encoded in PCIe Slot Capabilities.
    #[serde(default)]
    pub value: u8,
    /// Power-limit scale encoded in PCIe Slot Capabilities.
    #[serde(default)]
    pub scale: u8,
}

/// SATA controller operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SataMode {
    Ide,
    Ahci,
}

/// USB controller configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UsbConfig {
    #[serde(default)]
    pub ehci: [bool; 2],
    #[serde(default)]
    pub uhci: [bool; 6],
}

/// Legacy serial-port decode selector in the LPC I/O decode register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LpcSerialDecode {
    /// COM1 at 0x3f8.
    Com1,
    /// COM2 at 0x2f8.
    Com2,
    /// Serial decode at 0x220.
    Io220,
    /// Serial decode at 0x228.
    Io228,
    /// Serial decode at 0x238.
    Io238,
    /// Serial decode at 0x2e8.
    Io2e8,
    /// Serial decode at 0x338.
    Io338,
    /// Serial decode at 0x3e8.
    Io3e8,
}

impl LpcSerialDecode {
    const fn bits(self) -> u16 {
        match self {
            Self::Com1 => 0,
            Self::Com2 => 1,
            Self::Io220 => 2,
            Self::Io228 => 3,
            Self::Io238 => 4,
            Self::Io2e8 => 5,
            Self::Io338 => 6,
            Self::Io3e8 => 7,
        }
    }
}

/// Parallel-port decode selector in the LPC I/O decode register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LpcParallelDecode {
    /// LPT at 0x378.
    Lpt378,
    /// LPT at 0x278.
    Lpt278,
    /// LPT at 0x3bc.
    Lpt3bc,
}

impl LpcParallelDecode {
    const fn bits(self) -> u16 {
        match self {
            Self::Lpt378 => 0,
            Self::Lpt278 => 1,
            Self::Lpt3bc => 2,
        }
    }
}

/// Floppy-controller decode selector in the LPC I/O decode register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LpcFloppyDecode {
    /// FDC at 0x3f0.
    Fdd3f0,
    /// FDC at 0x370.
    Fdd370,
}

impl LpcFloppyDecode {
    const fn bits(self) -> u16 {
        match self {
            Self::Fdd3f0 => 0,
            Self::Fdd370 => 1,
        }
    }
}

const fn default_com_a() -> LpcSerialDecode {
    LpcSerialDecode::Com1
}

const fn default_com_b() -> LpcSerialDecode {
    LpcSerialDecode::Com2
}

/// Fixed legacy I/O decode selections for COM/LPT/FDC ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LpcFixedIoDecode {
    /// COMA selector. COMA is enabled through `LPC_EN_ALL`.
    #[serde(default = "default_com_a")]
    pub com_a: LpcSerialDecode,
    /// COMB selector. COMB is enabled through `LPC_EN_ALL`.
    #[serde(default = "default_com_b")]
    pub com_b: LpcSerialDecode,
    /// Optional LPT selector.
    #[serde(default)]
    pub lpt: Option<LpcParallelDecode>,
    /// Optional FDC selector.
    #[serde(default)]
    pub fdd: Option<LpcFloppyDecode>,
}

impl Default for LpcFixedIoDecode {
    fn default() -> Self {
        Self {
            com_a: LpcSerialDecode::Com1,
            com_b: LpcSerialDecode::Com2,
            lpt: None,
            fdd: None,
        }
    }
}

impl LpcFixedIoDecode {
    const fn encode(self) -> u16 {
        let mut value = self.com_a.bits() | (self.com_b.bits() << 4);
        if let Some(lpt) = self.lpt {
            value |= lpt.bits() << 8;
        }
        if let Some(fdd) = self.fdd {
            value |= fdd.bits() << 12;
        }
        value
    }
}

/// One LPC generic I/O decode window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LpcGenericIoDecode {
    /// I/O base address. Must be 4-byte aligned.
    pub base: u16,
    /// Window size in bytes. Must be a non-zero multiple of 4 up to 256.
    pub size: u16,
}

impl LpcGenericIoDecode {
    fn encode(self) -> Option<u32> {
        if self.base & 0x0003 != 0 || self.size == 0 || self.size > 0x0100 {
            return None;
        }
        if self.size & 0x0003 != 0 {
            return None;
        }
        Some(((u32::from(self.size) - 4) << 16) | u32::from(self.base & 0xfffc) | 1)
    }
}

/// Board-level LPC decode policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LpcDecodeConfig {
    /// Fixed COM/LPT/FDC decode selector register.
    #[serde(default)]
    pub fixed_io: LpcFixedIoDecode,
    /// Up to four generic I/O decode windows, programmed to GEN1..GEN4.
    #[serde(default)]
    pub generic_io: HVec<LpcGenericIoDecode, 4>,
}

/// Board-provided I/O trap programming for ICH8 RCBA.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Ich8IoTrapConfig {
    /// Low dword value for IOTR3.
    pub lo: u32,
    /// High dword value for IOTR3.
    pub hi: u32,
}

/// Board-provided late RCBA interrupt routing for ICH8.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Ich8LateRcbaConfig {
    /// Device interrupt pin routes.
    pub d31ip: u32,
    #[serde(default)]
    pub d30ip: Option<u32>,
    pub d29ip: u32,
    pub d28ip: u32,
    pub d27ip: u32,
    #[serde(default)]
    pub d26ip: Option<u32>,
    #[serde(default)]
    pub d25ip: Option<u32>,
    /// Device interrupt route registers.
    pub d31ir: u16,
    pub d30ir: u16,
    pub d29ir: u16,
    pub d28ir: u16,
    pub d27ir: u16,
    #[serde(default)]
    pub d26ir: Option<u16>,
    #[serde(default)]
    pub d25ir: Option<u16>,
    /// Optional I/O trap #3 programming.
    #[serde(default)]
    pub iotr3: Option<Ich8IoTrapConfig>,
}

/// ICH8 southbridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelIch8Config {
    /// Root Complex Base Address register value.
    pub rcba: u64,
    /// Northbridge DMIBAR base address, used as the RCBA upstream RCRB target.
    pub dmibar: u64,
    /// PIRQ routing (A..H).
    pub pirq_routing: [u8; 8],
    /// GPE0 enable bits (ICH8 low dword at PMBASE+0x28).
    pub gpe0_en: u32,
    /// GPI routing selectors for GPIO0..15 (0=no route, 1=SMI, 2=SCI).
    #[serde(default)]
    pub gpi_routing: [u8; 16],
    /// Alternate GPI SMI enable bits.
    #[serde(default)]
    pub alt_gp_smi_en: u16,
    /// Enable C4-on-C3 in GEN_PMCON_1 for mobile board power policy.
    #[serde(default)]
    pub c4_on_c3: bool,
    /// Enable C5/C6 PMSYNC support.
    #[serde(default)]
    pub c5_enable: bool,
    /// Enable C6 exit timing when C5/C6 PMSYNC support is active.
    #[serde(default)]
    pub c6_enable: bool,
    /// LPC fixed and generic I/O decode policy.
    #[serde(default)]
    pub lpc_decode: LpcDecodeConfig,
    /// Optional HD Audio verb table.
    #[serde(default)]
    pub hda: Option<HdaConfig>,
    /// Optional PATA/IDE controller configuration.
    #[serde(default)]
    pub ide: Option<IdeConfig>,
    /// SATA configuration.
    #[serde(default)]
    pub sata: Option<SataConfig>,
    /// USB configuration.
    #[serde(default)]
    pub usb: Option<UsbConfig>,
    /// PCIe root ports 1..6 enabled.
    #[serde(default = "default_pcie_ports")]
    pub pcie_ports: [bool; 6],
    /// PCIe root ports implemented as slots.
    #[serde(default)]
    pub pcie_slots: [bool; 6],
    /// PCIe slot power limits for ports 1..6.
    #[serde(default)]
    pub pcie_power_limits: [PciePowerLimit; 6],
    /// ECAM base address.
    #[serde(default = "default_ecam_base")]
    pub ecam_base: u64,
    /// SMBus I/O base.
    #[serde(default = "default_smbus_base")]
    pub smbus_base: u16,
    /// GPIO pad configuration.
    #[serde(default)]
    pub gpio: GpioConfig,
    /// ACPI device name (reserved for future ACPI device generation).
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
    /// C3 latency in microseconds.
    #[serde(default = "default_c3_latency")]
    pub c3_latency: u16,
    /// After-power-failure behaviour: 0=off, 1=on, 2=last-state.
    #[serde(default)]
    pub power_on_after_fail: u8,
    /// Hardware throttle duty cycle (PMBASE+0x10 bits [7:5]).
    #[serde(default)]
    pub throttle_duty: u8,
    /// Disable the integrated LAN function through the SUS-well FD register.
    #[serde(default)]
    pub disable_lan: bool,
    /// Disable the second SATA function. ICH8-M boards commonly leave it hidden.
    #[serde(default = "default_true")]
    pub disable_sata2: bool,
    /// Disable the desktop thermal-throttle function. ICH8-M does not expose it.
    #[serde(default = "default_true")]
    pub disable_thermal: bool,
    /// Optional board-provided late RCBA interrupt/trap routing.
    #[serde(default)]
    pub late_rcba: Option<Ich8LateRcbaConfig>,
}

fn default_pcie_ports() -> [bool; 6] {
    [true, true, true, true, true, true]
}

fn default_true() -> bool {
    true
}

fn default_ecam_base() -> u64 {
    0xe000_0000
}

fn default_smbus_base() -> u16 {
    ich8::DEFAULT_SMBUS_BASE
}

fn default_c3_latency() -> u16 {
    85
}

/// Sparse RCBA accessor.
#[derive(Clone, Copy)]
struct Rcba {
    base: usize,
}

impl Rcba {
    const fn new(base: usize) -> Self {
        Self { base }
    }

    #[inline]
    fn read32(&self, off: u32) -> u32 {
        // SAFETY: RCBA has been programmed and enabled in LPC PCI config.
        unsafe { fstart_mmio::read32((self.base + off as usize) as *const u32) }
    }

    #[inline]
    fn write32(&self, off: u32, val: u32) {
        // SAFETY: RCBA has been programmed and enabled in LPC PCI config.
        unsafe { fstart_mmio::write32((self.base + off as usize) as *mut u32, val) }
    }

    #[inline]
    fn read16(&self, off: u32) -> u16 {
        // SAFETY: RCBA has been programmed and enabled in LPC PCI config.
        unsafe { fstart_mmio::read16((self.base + off as usize) as *const u16) }
    }

    #[inline]
    fn read8(&self, off: u32) -> u8 {
        // SAFETY: RCBA has been programmed and enabled in LPC PCI config.
        unsafe { fstart_mmio::read8((self.base + off as usize) as *const u8) }
    }

    #[inline]
    fn write16(&self, off: u32, val: u16) {
        // SAFETY: RCBA has been programmed and enabled in LPC PCI config.
        unsafe { fstart_mmio::write16((self.base + off as usize) as *mut u16, val) }
    }

    #[inline]
    fn write8(&self, off: u32, val: u8) {
        // SAFETY: RCBA has been programmed and enabled in LPC PCI config.
        unsafe { fstart_mmio::write8((self.base + off as usize) as *mut u8, val) }
    }
}

/// Intel ICH8 southbridge driver.
pub struct IntelIch8 {
    config: IntelIch8Config,
    smbus: Option<I801SmBus>,
    pm: PmIo,
}

// SAFETY: firmware performs chipset init on the BSP before concurrency exists.
unsafe impl Send for IntelIch8 {}
// SAFETY: the struct contains only config, optional bus state, and fixed I/O bases.
unsafe impl Sync for IntelIch8 {}

impl IntelIch8 {
    fn lpc(&self) -> ecam::PciDevBdf {
        ecam::PciDevBdf::new(0, ich8::LPC_DEV, ich8::LPC_FUNC)
    }

    fn rcba(&self) -> Rcba {
        Rcba::new((self.config.rcba & 0xffff_c000) as usize)
    }

    fn pm(&self) -> PmIo {
        self.pm
    }

    fn enable_spi_prefetching_and_caching(&self) {
        let lpc = self.lpc();
        // Match coreboot i82801hx bootblock: enable SPI prefetch/cache before
        // extended flash reads from the memory-mapped boot medium.
        let value = lpc.read8(0xdc);
        lpc.write8(0xdc, (value & !(3 << 2)) | (2 << 2));
    }

    fn program_fixed_bars(&self) {
        let lpc = self.lpc();
        lpc.write32(ich8::RCBA, (self.config.rcba as u32 & 0xffff_c000) | 1);
        lpc.write32(ich8::PMBASE, (ich8::DEFAULT_PMBASE as u32) | 1);
        lpc.write8(ich8::ACPI_CNTL, ich8::ACPI_EN);
        lpc.write32(ich8::GPIOBASE, ich8::DEFAULT_GPIOBASE as u32);
        lpc.or8(ich8::GPIO_CNTL, ich8::GPIO_EN);
    }

    fn program_lpc_decode(&self) {
        let lpc = self.lpc();
        let mut generic = [0u32; 4];
        for (idx, range) in self
            .config
            .lpc_decode
            .generic_io
            .iter()
            .copied()
            .enumerate()
        {
            generic[idx] = range.encode().unwrap_or(0);
        }

        let mut lpc_en = LPC_EN_COREBOOT_BASE;
        if self.config.lpc_decode.fixed_io.lpt.is_some() {
            lpc_en |= LPC_EN_LPT;
        }
        if self.config.lpc_decode.fixed_io.fdd.is_some() {
            lpc_en |= LPC_EN_FDD;
        }

        lpc.write8(ich8::SERIRQ_CNTL, 0xd0);
        lpc.write16(ich8::LPC_IO_DEC, self.config.lpc_decode.fixed_io.encode());
        lpc.write16(ich8::LPC_EN, lpc_en);
        lpc.write32(ich8::GEN1_DEC, generic[0]);
        lpc.write32(ich8::GEN2_DEC, generic[1]);
        lpc.write32(ich8::GEN3_DEC, generic[2]);
        lpc.write32(ich8::GEN4_DEC, generic[3]);
    }

    fn reset_watchdog_and_cmos(&self) {
        let rcba = self.rcba();
        rcba.write32(0x3400, 1 << 2);
        rcba.write32(ich8::GCS, rcba.read32(ich8::GCS) | (1 << 5));

        #[cfg(target_arch = "x86_64")]
        {
            let tco = self.pm().tco();
            tco.write16(pmio::TCO1_STS, 0x0008);
            tco.write16(pmio::TCO2_STS, 0x0002);
            let cnt = tco.read16(pmio::TCO1_CNT);
            tco.write16(pmio::TCO1_CNT, cnt | (1 << 11));
        }
    }

    fn setup_gpios(&self) {
        let gpio = IchGpio::new(ich8::DEFAULT_GPIOBASE);
        gpio.setup(&self.config.gpio);
    }

    fn enable_smbus(&mut self) {
        let smbus_pci = ecam::PciDevBdf::new(0, ich8::SMBUS_DEV, ich8::SMBUS_FUNC);
        smbus_pci.and16(0x80, !((1 << 8) | (1 << 10) | (1 << 12) | (1 << 14)));
        let smbus =
            I801SmBus::enable_on_i801(0, ich8::SMBUS_DEV, ich8::SMBUS_FUNC, self.config.smbus_base);
        self.smbus = Some(smbus);
    }

    fn enable_hpet(&self) {
        let rcba = self.rcba();
        let v = rcba.read32(ich8::RCBA_HPTC);
        rcba.write32(ich8::RCBA_HPTC, (v & !0x03) | (1 << 7));
        let _ = rcba.read32(ich8::RCBA_HPTC);

        // SAFETY: HPET base is fixed once enabled through HPTC.
        unsafe {
            let cfg = fstart_mmio::read32((HPET_BASE + 0x10) as *const u32);
            fstart_mmio::write32((HPET_BASE + 0x10) as *mut u32, cfg | 1);
        }
    }

    fn setup_dmi(&self) {
        const VC1_PAT: [u8; 64] = [
            0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00,
            0x00, 0x00, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x00,
            0x00, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let rcba = self.rcba();
        rcba.write32(
            ich8::RCBA_V1CAP,
            (rcba.read32(ich8::RCBA_V1CAP) & !(0x7f << 16)) | (0x12 << 16),
        );
        rcba.write32(ich8::RCBA_CIR1, 0x0010_9000);
        rcba.write16(ich8::RCBA_CIR3, 0x060b);
        rcba.write32(ich8::RCBA_CIR2, 0x8600_0040);
        rcba.write32(ich8::RCBA_CIR4, 0x0000_2008);
        rcba.write8(ich8::RCBA_BCR, 0x45);
        rcba.write32(ich8::RCBA_CIR6, rcba.read32(ich8::RCBA_CIR6) & !(1 << 7));

        rcba.write32(
            ich8::RCBA_V1CTL,
            (rcba.read32(ich8::RCBA_V1CTL) & !(0x7 << 24)) | (1 << 24),
        );
        rcba.write32(
            ich8::RCBA_V1CTL,
            (rcba.read32(ich8::RCBA_V1CTL) & !(0x7f << 1)) | (1 << 7),
        );
        rcba.write32(
            ich8::RCBA_V0CTL,
            rcba.read32(ich8::RCBA_V0CTL) & !(0x7f << 1),
        );
        rcba.write32(
            ich8::RCBA_V1CTL,
            (rcba.read32(ich8::RCBA_V1CTL) & !(0x7 << 17)) | (0x4 << 17),
        );
        for (i, val) in VC1_PAT.iter().enumerate() {
            rcba.write8(ich8::RCBA_PAT + i as u32, *val);
        }
        rcba.write32(ich8::RCBA_V1CTL, rcba.read32(ich8::RCBA_V1CTL) | (1 << 16));
        rcba.write32(ich8::RCBA_V1CTL, rcba.read32(ich8::RCBA_V1CTL) | (1 << 31));

        rcba.write8(ich8::RCBA_ESD + 2, 2);
        rcba.write8(ich8::RCBA_ULD + 3, 1);
        rcba.write8(ich8::RCBA_ULD + 2, 1);
        rcba.write32(ich8::RCBA_ULBA, self.config.dmibar as u32 & 0xffff_f000);

        // Mobile ICH8-M/HX path: enable DMI mobile power savings, then
        // advertise and enable L0s+L1.
        let mut dmc = rcba.read32(ich8::RCBA_DMC);
        dmc = (dmc & !(3 << 10)) | (1 << 10);
        rcba.write32(ich8::RCBA_DMC, dmc);
        rcba.write32(ich8::RCBA_DMC, dmc | (1 << 19));
        rcba.write32(ich8::RCBA_LCAP, rcba.read32(ich8::RCBA_LCAP) | (3 << 10));
        rcba.write32(ich8::RCBA_LCTL, rcba.read32(ich8::RCBA_LCTL) | 3);
    }

    fn poll_vc1(&self) {
        let rcba = self.rcba();
        let mut timeout = 0x7ffff;
        while (rcba.read32(ich8::RCBA_V1STS) & (1 << 1)) != 0 && timeout != 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            fstart_log::error!("intel-ich8: VC1 negotiation timeout");
        }

        if ((rcba.read16(ich8::RCBA_LSTS) >> 4) & 0x3f) == 2 {
            rcba.write32(
                ich8::RCBA_CIR6,
                (rcba.read32(ich8::RCBA_CIR6) & !(7 << 21)) | (3 << 21),
            );
            rcba.write16(0x20c4, rcba.read16(0x20c4) | (1 << 15));
            rcba.write16(0x20e4, rcba.read16(0x20e4) | (1 << 15));
        }

        timeout = 0x7ffff;
        while (rcba.read32(ich8::RCBA_V1STS) & 1) != 0 && timeout != 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            fstart_log::error!("intel-ich8: VC1 arbitration-table update timeout");
        }
    }

    fn configure_power_options(&self) {
        let lpc = self.lpc();
        lpc.or32(ich8::PMIR, ich8::PMIR_CF9GR);

        let mut gen_pmcon3 = lpc.read8(ich8::GEN_PMCON_3) & !1;
        if self.config.power_on_after_fail == 0 {
            gen_pmcon3 |= 1;
        }
        gen_pmcon3 |= 3 << 4;
        gen_pmcon3 &= !(1 << 3);
        lpc.write8(ich8::GEN_PMCON_3, gen_pmcon3);

        let mut gen_pmcon1 = lpc.read16(ich8::GEN_PMCON_1);
        gen_pmcon1 &= !0x3;
        gen_pmcon1 |= (1 << 2) | (1 << 3) | (1 << 5) | (1 << 10);
        if self.config.c4_on_c3 {
            gen_pmcon1 |= 1 << 7;
        }
        if self.config.c5_enable {
            gen_pmcon1 |= 1 << 11;
        }
        lpc.write16(ich8::GEN_PMCON_1, gen_pmcon1);

        if self.config.c5_enable {
            let mut c5 = lpc.read8(ich8::C5_EXIT_TIMING);
            c5 &= !((7 << 3) | 7);
            c5 |= if self.config.c6_enable {
                (5 << 3) | 3
            } else {
                1
            };
            lpc.write8(ich8::C5_EXIT_TIMING, c5);
        }

        self.configure_gpi_routing();
        self.pm().write32(GPE0_STS_ICH8, 0xffff_ffff);
        self.pm().write32(GPE0_EN_ICH8, self.config.gpe0_en);
        self.pm()
            .write16(pmio::ALT_GP_SMI_EN, self.config.alt_gp_smi_en);
        let sts = self.pm().read16(pmio::PM1_STS);
        self.pm().write16(pmio::PM1_STS, sts);
        let mut throttle = self.pm().read32(0x10);
        throttle &= !(7 << 5);
        throttle |= (u32::from(self.config.throttle_duty & 7)) << 5;
        self.pm().write32(0x10, throttle);

        #[cfg(target_arch = "x86_64")]
        unsafe {
            // SAFETY: legacy NMI control ports on x86 PCs.
            let mut port61 = fstart_pio::inb(0x61);
            port61 &= 0x0f;
            port61 &= !(1 << 3);
            port61 |= 1 << 2;
            fstart_pio::outb(0x61, port61);
            let mut nmi = fstart_pio::inb(0x74);
            nmi |= 1 << 7;
            fstart_pio::outb(0x70, nmi);
        }
    }

    fn configure_cstates(&self) {
        let lpc = self.lpc();
        lpc.or8(ich8::CXSTATE_CNF, (1 << 4) | (1 << 3) | (1 << 2));
        lpc.and8_or8(ich8::C4TIMING_CNT, !0x0f, (2 << 2) | 2);
    }

    fn enable_clock_gating(&self) {
        let rcba = self.rcba();
        rcba.write32(ich8::RCBA_DMIC, rcba.read32(ich8::RCBA_DMIC) | 3);
        let mut cg = rcba.read32(ich8::RCBA_CG);
        cg |= (1 << 31) | (1 << 29) | (1 << 28);
        cg |= (1 << 27) | (1 << 26) | (1 << 25) | (1 << 24);
        cg |= (1 << 23) | (1 << 22);
        cg &= !(1 << 21);
        cg &= !(1 << 20);
        cg |= (1 << 19) | (1 << 18) | (1 << 17) | (1 << 16);
        cg |= (1 << 4) | (1 << 3) | (1 << 2) | (1 << 1) | 1;
        rcba.write32(ich8::RCBA_CG, cg);
        rcba.write32(0x38c0, rcba.read32(0x38c0) | 7);
    }

    fn enable_ioapic(&self) {
        let rcba = self.rcba();
        rcba.write8(ich8::OIC, ich8::OIC_AEN | ich8::OIC_OAEN);
        let _ = rcba.read8(ich8::OIC);
    }

    fn rtc_init_status(&self) {
        let lpc = self.lpc();
        let gen_pmcon3 = lpc.read8(ich8::GEN_PMCON_3);
        if (gen_pmcon3 & GEN_PMCON_3_RTC_BATTERY_DEAD) != 0 {
            lpc.write8(
                ich8::GEN_PMCON_3,
                gen_pmcon3 & !GEN_PMCON_3_RTC_BATTERY_DEAD,
            );
            fstart_log::info!("intel-ich8: RTC battery-dead flag was set");
        }
    }

    fn ramstage_lpc_init(&self) {
        let rcba = self.rcba();
        self.enable_ioapic();
        self.lpc().write8(ich8::SERIRQ_CNTL, 0xc0);
        self.configure_power_options();
        self.configure_cstates();
        self.rtc_init_status();
        self.isa_dma_init();
        self.i8259_init();
        self.enable_hpet();
        rcba.write32(0x3400, rcba.read32(0x3400) | (1 << 2));
        self.enable_clock_gating();
        self.enable_acpi_pm1();
    }

    fn pcie_fd_bit(func: usize) -> u32 {
        match func {
            0 => ich8::FD_PE1D,
            1 => ich8::FD_PE2D,
            2 => ich8::FD_PE3D,
            3 => ich8::FD_PE4D,
            4 => ich8::FD_PE5D,
            5 => ich8::FD_PE6D,
            _ => 0,
        }
    }

    fn usb_fd_bit(dev: u8, func: u8) -> u32 {
        match (dev, func) {
            (ich8::UHCI1_DEV, 0) => ich8::FD_U1D,
            (ich8::UHCI1_DEV, 1) => ich8::FD_U2D,
            (ich8::UHCI1_DEV, 2) => ich8::FD_U3D,
            (ich8::EHCI1_DEV, ich8::EHCI1_FUNC) => ich8::FD_EHCI1D,
            (ich8::UHCI2_DEV, 0) => ich8::FD_U4D,
            (ich8::UHCI2_DEV, 1) => ich8::FD_U5D,
            (ich8::EHCI2_DEV, ich8::EHCI2_FUNC) => ich8::FD_EHCI2D,
            _ => 0,
        }
    }

    fn early_chipset_settings(&self) {
        let rcba = self.rcba();
        rcba.write32(ich8::GCS, rcba.read32(ich8::GCS) | (1 << 6));
        rcba.write32(ich8::RCBA_CIR8, (rcba.read32(ich8::RCBA_CIR8) & !0x3) | 0x2);
        rcba.write32(
            ich8::RCBA_CIR9,
            (rcba.read32(ich8::RCBA_CIR9) & !(0x3 << 26)) | (0x2 << 26),
        );
        rcba.write32(
            ich8::RCBA_CIR7,
            (rcba.read32(ich8::RCBA_CIR7) & !(0xf << 16)) | (0x5 << 16),
        );
        rcba.write32(
            ich8::RCBA_CIR13,
            (rcba.read32(ich8::RCBA_CIR13) & !(0xf << 16)) | (0x5 << 16),
        );
        rcba.write32(ich8::RCBA_CIR5, rcba.read32(ich8::RCBA_CIR5) | 1);
        rcba.write32(ich8::RCBA_CIR10, rcba.read32(ich8::RCBA_CIR10) | (3 << 16));
    }

    fn configure_gpi_routing(&self) {
        let mut value = 0u32;
        for (idx, route) in self.config.gpi_routing.iter().enumerate() {
            value |= ((*route as u32) & 0x03) << (idx * 2);
        }
        self.lpc().write32(ich8::GPIO_ROUT, value);
    }

    fn sata_indexed_write32(&self, idx: u8, val: u32) {
        let sata = ecam::PciDevBdf::new(0, ich8::SATA_DEV, ich8::SATA_FUNC);
        sata.write8(ich8::SATA_SIDX, idx);
        sata.write32(ich8::SATA_SDAT, val);
    }

    fn sata_indexed_rmw32(&self, idx: u8, clear: u32, set: u32) {
        let sata = ecam::PciDevBdf::new(0, ich8::SATA_DEV, ich8::SATA_FUNC);
        sata.write8(ich8::SATA_SIDX, idx);
        let val = sata.read32(ich8::SATA_SDAT);
        sata.write32(ich8::SATA_SDAT, (val & !clear) | set);
    }

    fn sata_enable_ahci_mmap(&self, sata: &SataConfig, is_mobile: bool) {
        let port_mask = if is_mobile { 0x07 } else { 0x3f };
        let port_map = sata.ports & port_mask;
        let num_ports = if is_mobile { 3 } else { 6 };
        // SAFETY: `sata_init` programs BAR5 to this fixed ABAR before use.
        unsafe {
            let abar = SATA_ABAR_BASE;
            let ghc = fstart_mmio::read32((abar + 0x04) as *const u32) | (1 << 31);
            fstart_mmio::write32((abar + 0x04) as *mut u32, ghc);
            let mut cap = fstart_mmio::read32(abar as *const u32);
            cap |= 0x0c00_6080;
            cap &= !0x0002_0060;
            fstart_mmio::write32(abar as *mut u32, cap);
            fstart_mmio::write32((abar + 0x0c) as *mut u32, port_map as u32);
            let _ = fstart_mmio::read32((abar + 0x0c) as *const u32);
            let _ = fstart_mmio::read32((abar + 0x0c) as *const u32);
            let vsp = fstart_mmio::read32((abar + 0xa0) as *const u32) & !1;
            fstart_mmio::write32((abar + 0xa0) as *mut u32, vsp);
            for port in 0..num_ports {
                let cmd = abar + 0x118 + port * 0x80;
                let mut value = fstart_mmio::read32(cmd as *const u32);
                if (sata.hotplug_map & (1 << port)) != 0 {
                    value |= 1 << 18;
                }
                fstart_mmio::write32(cmd as *mut u32, value);
            }
        }
    }

    fn sata_program_indexed(&self, is_mobile: bool) {
        self.sata_indexed_rmw32(0x18, (7 << 6) | (7 << 3) | 7, (3 << 3) | 3);
        self.sata_indexed_write32(0x28, 0x00cc_2080);
        let sata = ecam::PciDevBdf::new(0, ich8::SATA_DEV, ich8::SATA_FUNC);
        sata.write8(ich8::SATA_SIDX, 0x40);
        sata.write8(ich8::SATA_SDAT + 2, 0x22);
        sata.write8(ich8::SATA_SIDX, 0x78);
        sata.write8(ich8::SATA_SDAT + 2, 0x22);
        if !is_mobile {
            self.sata_indexed_rmw32(0x84, (7 << 3) | 7, (3 << 3) | 3);
        }
        let desktop_88_clear_set = if is_mobile {
            (0, 0)
        } else {
            (
                (7 << 27) | (7 << 24) | (7 << 11) | (7 << 8),
                (4 << 27) | (4 << 24) | (2 << 11) | (2 << 8),
            )
        };
        self.sata_indexed_rmw32(
            0x88,
            desktop_88_clear_set.0 | (7 << 19) | (7 << 16) | (7 << 3) | 7,
            desktop_88_clear_set.1 | (4 << 19) | (4 << 16) | (2 << 3) | 2,
        );
        let desktop_8c_clear_set = if is_mobile {
            (0, 0)
        } else {
            ((7 << 27) | (7 << 24), (2 << 27) | (2 << 24))
        };
        self.sata_indexed_rmw32(
            0x8c,
            desktop_8c_clear_set.0 | (7 << 19) | (7 << 16) | 0xffff,
            desktop_8c_clear_set.1 | (2 << 19) | (2 << 16) | 0x00aa,
        );
        self.sata_indexed_write32(0x94, 0x0000_0022);
        self.sata_indexed_rmw32(0xa0, (7 << 3) | 7, (3 << 3) | 3);
        self.sata_indexed_rmw32(
            0xa8,
            (7 << 19) | (7 << 16) | (7 << 3) | 7,
            (4 << 19) | (4 << 16) | (2 << 3) | 2,
        );
        self.sata_indexed_rmw32(
            0xac,
            (7 << 19) | (7 << 16) | 0xffff,
            (2 << 19) | (2 << 16) | 0x000a,
        );
    }

    fn sata_init(&self, config: &SataConfig) {
        let sata = ecam::PciDevBdf::new(0, ich8::SATA_DEV, ich8::SATA_FUNC);
        if sata.read16(0) == 0xffff {
            return;
        }
        let devid = sata.read16(ich8::PCI_DEVICE_ID);
        let is_mobile = matches!(
            devid,
            ich8::DID_82801HBM_SATA | ich8::DID_82801HBM_SATA_AHCI | ich8::DID_82801HBM_SATA_RAID
        );
        let port_mask = if is_mobile { 0x07 } else { 0x3f };
        let ports = config.ports & port_mask;
        sata.or16(
            ich8::PCI_COMMAND,
            ich8::PCI_CMD_IO | ich8::PCI_CMD_MEMORY | ich8::PCI_CMD_MASTER,
        );
        match config.mode {
            SataMode::Ahci => {
                sata.write8(ich8::SATA_MAP, 0x60);
                sata.write32(ich8::PCI_BAR5, SATA_ABAR_BASE as u32);
            }
            SataMode::Ide => {
                sata.write8(ich8::SATA_MAP, 0);
                sata.write8(ich8::PCI_CLASS_PROG, 0x8f);
                sata.write32(ich8::PCI_BAR5, 0);
            }
        }
        sata.write16(ich8::SATA_IDE_TIM_PRI, 1 << 15);
        sata.write16(ich8::SATA_IDE_TIM_SEC, 1 << 15);
        let pcs_ports = if matches!(config.mode, SataMode::Ahci) {
            port_mask
        } else {
            ports
        };
        let pcs = (sata.read16(ich8::SATA_PCS) & !0x3f) | (1 << 15) | pcs_ports as u16;
        sata.write16(ich8::SATA_PCS, pcs);
        let mut sclkcg = (((!config.ports as u32) & 0x3f) << 24) | 0x193;
        #[cfg(target_arch = "x86_64")]
        if config.clock_request {
            // SAFETY: GPIOBASE is programmed before SATA init; GPIO35 is in the second bank.
            if unsafe { fstart_pio::inb(ich8::DEFAULT_GPIOBASE + 0x30) } & (1 << (35 - 32)) == 0 {
                sclkcg |= 1 << 30;
            }
        }
        sata.write32(ich8::SATA_CLK, sclkcg);
        if config.traffic_monitor && ((self.lpc().read8(ich8::CXSTATE_CNF) >> 3) & 3) == 3 {
            sata.and8_or8(0x9c, !(0x1f << 2), 3 << 2);
        }
        if matches!(config.mode, SataMode::Ahci) {
            self.sata_enable_ahci_mmap(config, is_mobile);
        }
        self.sata_program_indexed(is_mobile);
        fstart_log::info!("intel-ich8: SATA init complete ports={:#x}", ports as u32);
    }

    fn ehci_reset_controller(&self, dev: u8, func: u8, fd_bit: u32) {
        let ehci = ecam::PciDevBdf::new(0, dev, func);
        if ehci.read16(0) == 0xffff {
            return;
        }
        let rcba = self.rcba();
        let fd = rcba.read32(ich8::RCBA_FD);
        rcba.write32(ich8::RCBA_FD, fd & !fd_bit);
        ehci.write32(ich8::PCI_BAR0, EHCI_TEMP_BAR as u32);
        let cmd = ehci.read16(ich8::PCI_COMMAND);
        ehci.write16(ich8::PCI_COMMAND, cmd | ich8::PCI_CMD_MEMORY);
        // SAFETY: temporary EHCI BAR0 maps the controller MMIO window.
        unsafe {
            let usbcmd = (EHCI_TEMP_BAR + ich8::EHCI_USBCMD) as *mut u32;
            let val = fstart_mmio::read32(usbcmd as *const u32);
            fstart_mmio::write32(
                usbcmd,
                (val & !ich8::EHCI_USBCMD_RS) | ich8::EHCI_USBCMD_HCRST,
            );
        }
        for _ in 0..1000 {
            core::hint::spin_loop();
        }
        ehci.write32(
            ich8::EHCI_INTEL_FCREG,
            (ehci.read32(ich8::EHCI_INTEL_FCREG) & !(3 << 2)) | (1 << 29) | (1 << 17) | (2 << 2),
        );
        ehci.write16(ich8::PCI_COMMAND, cmd & !ich8::PCI_CMD_MEMORY);
        ehci.write32(ich8::PCI_BAR0, 0);
        rcba.write32(ich8::RCBA_FD, fd);
    }

    fn usb_init(&self) {
        let Some(usb) = self.config.usb else {
            return;
        };
        if usb.ehci[0] {
            self.ehci_reset_controller(ich8::EHCI1_DEV, ich8::EHCI1_FUNC, ich8::FD_EHCI1D);
            ecam::PciDevBdf::new(0, ich8::EHCI1_DEV, ich8::EHCI1_FUNC)
                .or16(ich8::PCI_COMMAND, ich8::PCI_CMD_MASTER);
        }
        if usb.ehci[1] {
            self.ehci_reset_controller(ich8::EHCI2_DEV, ich8::EHCI2_FUNC, ich8::FD_EHCI2D);
            ecam::PciDevBdf::new(0, ich8::EHCI2_DEV, ich8::EHCI2_FUNC)
                .or16(ich8::PCI_COMMAND, ich8::PCI_CMD_MASTER);
        }
        const UHCI: [(u8, u8); 5] = [(0x1d, 0), (0x1d, 1), (0x1d, 2), (0x1a, 0), (0x1a, 1)];
        for (idx, (dev, func)) in UHCI.iter().copied().enumerate() {
            if !usb.uhci[idx] {
                continue;
            }
            let uhci = ecam::PciDevBdf::new(0, dev, func);
            if uhci.read16(0) != 0xffff {
                uhci.or16(ich8::PCI_COMMAND, ich8::PCI_CMD_IO | ich8::PCI_CMD_MASTER);
            }
        }
        fstart_log::info!("intel-ich8: USB init complete");
    }

    fn hda_init(&self, config: &HdaConfig) {
        let hda = ecam::PciDevBdf::new(0, ich8::HDA_DEV, ich8::HDA_FUNC);
        if hda.read16(0) == 0xffff {
            fstart_log::info!("intel-ich8: HDA device not present");
            return;
        }

        // Coreboot i82801hx/azalia.c: ESD/link/VC setup before codec reset.
        hda.modify32(0x134, !0x00ff_0000, 2 << 16);
        hda.modify32(0x140, !0x00ff_0000, 2 << 16);
        hda.modify32(0x114, !0x0000_00ff, 1);
        hda.or8(0x44, 7);
        hda.or32(0x120, (1 << 31) | (1 << 24) | 0x80);
        hda.and8(0x4d, !(1 << 7));
        hda.write32(0x74, hda.read32(0x74));

        // fstart has no full PCI resource allocator yet; use a fixed low MMIO
        // BAR in the same reserved chipset window as other early ICH8 blocks.
        hda.write32(ich8::PCI_BAR0, HDA_TEMP_BAR as u32);
        hda.or16(
            ich8::PCI_COMMAND,
            ich8::PCI_CMD_MEMORY | ich8::PCI_CMD_MASTER,
        );

        let controller = HdaController::new(HDA_TEMP_BAR);
        let codec_mask = controller.detect_codecs();
        if codec_mask != 0 {
            let programmed = controller.program_verb_tables(config, codec_mask);
            fstart_log::info!(
                "intel-ich8: HDA codec_mask={:#x}, programmed={} tables",
                codec_mask as u32,
                programmed as u32,
            );
        }
    }

    fn pcie_init(&self) {
        for func in 0u8..6 {
            let port = ecam::PciDevBdf::new(0, ich8::PCIE_DEV, func);
            if port.read16(0) == 0xffff {
                continue;
            }
            port.or32(ich8::D28FX_CIR_300, 1 << 21);
            port.write8(ich8::D28FX_CIR_324, 0x40);
            port.or32(ich8::D28FX_ASPM_MOBILE, 1);
            if (port.read32(ich8::D28FX_LCTL) & 3) == 3 {
                port.or32(ich8::D28FX_ASPM_MOBILE, 1 << 1);
            }
            port.or16(ich8::PCI_COMMAND, ich8::PCI_CMD_MASTER | ich8::PCI_CMD_SERR);
            port.write8(ich8::PCI_CACHE_LINE_SIZE, 0x10);
            port.and16(ich8::PCI_BRIDGE_CONTROL, !1u16);
            port.or32(ich8::D28FX_IOXAPIC, 1 << 7);
            port.or8(ich8::D28FX_BBCLKG, 0x0f);
            port.write32(
                ich8::D28FX_VC0RCTL,
                (port.read32(ich8::D28FX_VC0RCTL) & !0xff) | 1,
            );
            port.or32(ich8::D28FX_CTTOMASK, 1 << 14);
            port.write32(ich8::D28FX_CEMASK, port.read32(ich8::D28FX_CEMASK));
            port.write16(ich8::PCI_STATUS, port.read16(ich8::PCI_STATUS));
            port.write16(ich8::PCI_SEC_STATUS, port.read16(ich8::PCI_SEC_STATUS));
        }
        let fd = self.rcba().read32(ich8::RCBA_FD);
        for func in (0usize..6).rev() {
            if (fd & Self::pcie_fd_bit(func)) == 0 {
                break;
            }
            let port = ecam::PciDevBdf::new(0, ich8::PCIE_DEV, func as u8);
            if port.read16(0) != 0xffff {
                port.or32(ich8::D28FX_CIR_300, 0x3 << 16);
            }
        }
        let rcba = self.rcba();
        let mut rpfn = rcba.read32(ich8::RCBA_RPFN);
        for func in 0usize..6 {
            if (fd & Self::pcie_fd_bit(func)) != 0 {
                rpfn |= 1 << (func * 4 + 3);
            }
        }
        rcba.write32(ich8::RCBA_RPFN, rpfn);
        self.pcie_slot_config();
        self.pcie_aspm_lock();
        fstart_log::info!("intel-ich8: PCIe root port init complete");
    }

    fn pcie_slot_config(&self) {
        let mut slot_number = 1u32;
        for func in 0usize..6 {
            let port = ecam::PciDevBdf::new(0, ich8::PCIE_DEV, func as u8);
            if port.read16(0) == 0xffff {
                continue;
            }
            if self.config.pcie_slots[func] {
                port.or32(ich8::D28FX_XCAP, ich8::D28FX_XCAP_SLOT);
                let limit = self.config.pcie_power_limits[func];
                let mut slcap = port.read32(ich8::D28FX_SLCAP);
                slcap &= !(0x1fff << ich8::D28_SLCAP_SLOTNUM_SHIFT);
                slcap |= slot_number << ich8::D28_SLCAP_SLOTNUM_SHIFT;
                slcap &= !(0x03 << ich8::D28_SLCAP_SCALE_SHIFT);
                slcap |= (u32::from(limit.scale) & 0x03) << ich8::D28_SLCAP_SCALE_SHIFT;
                slcap &= !(0xff << ich8::D28_SLCAP_POWER_SHIFT);
                slcap |= u32::from(limit.value) << ich8::D28_SLCAP_POWER_SHIFT;
                port.write32(ich8::D28FX_SLCAP, slcap);
                slot_number += 1;
            } else {
                port.and32(ich8::D28FX_XCAP, !ich8::D28FX_XCAP_SLOT);
            }
        }
    }

    fn pcie_aspm_lock(&self) {
        for func in 0u8..6 {
            let port = ecam::PciDevBdf::new(0, ich8::PCIE_DEV, func);
            if port.read16(0) != 0xffff {
                port.write32(ich8::D28FX_LCAP, port.read32(ich8::D28FX_LCAP));
            }
        }
    }

    fn ide_init(&self, config: &IdeConfig) {
        let ide = ecam::PciDevBdf::new(0, ich8::IDE_DEV, ich8::IDE_FUNC);
        if ide.read16(0) == 0xffff {
            return;
        }
        ide.or16(ich8::PCI_COMMAND, ich8::PCI_CMD_IO | ich8::PCI_CMD_MASTER);
        ide.write8(ich8::PCI_CLASS_PROG, 0x8a);

        let timing_base = ich8::IDE_SITRE
            | ich8::IDE_ISP_3_CLOCKS
            | ich8::IDE_RCT_1_CLOCKS
            | ich8::IDE_IE0
            | ich8::IDE_TIME0;
        let primary_timing = (ide.read16(ich8::IDE_TIM_PRI) & !ich8::IDE_DECODE_ENABLE)
            | timing_base
            | if config.enable_primary {
                ich8::IDE_DECODE_ENABLE
            } else {
                0
            };
        let secondary_timing = (ide.read16(ich8::IDE_TIM_SEC) & !ich8::IDE_DECODE_ENABLE)
            | timing_base
            | if config.enable_secondary {
                ich8::IDE_DECODE_ENABLE
            } else {
                0
            };
        ide.write16(ich8::IDE_TIM_PRI, primary_timing);
        ide.write16(ich8::IDE_TIM_SEC, secondary_timing);

        let mut ide_config = 0u32;
        if config.enable_primary {
            ide_config |= ich8::FAST_PCB0 | ich8::PCB0 | ich8::FAST_PCB1 | ich8::PCB1;
        }
        if config.enable_secondary {
            ide_config |= ich8::FAST_SCB0 | ich8::SCB0 | ich8::FAST_SCB1 | ich8::SCB1;
        }
        ide.write32(ich8::IDE_CONFIG, ide_config);
        ide.write8(ich8::PCI_INTERRUPT_LINE, 0xff);
    }

    fn pci_bridge_init(&self) {
        let bridge = ecam::PciDevBdf::new(0, ich8::PCI_BRIDGE_DEV, ich8::PCI_BRIDGE_FUNC);
        if bridge.read16(0) == 0xffff {
            return;
        }
        bridge.write8(ich8::PCI_INTERRUPT_LINE, 0xff);
        bridge.and8_or8(ich8::D30F0_SMLT, 0x07, 0x04 << 3);
        bridge.write16(ich8::PCI_STATUS, bridge.read16(ich8::PCI_STATUS));
        bridge.write16(ich8::PCI_SEC_STATUS, bridge.read16(ich8::PCI_SEC_STATUS));
    }

    fn isa_dma_init(&self) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            fstart_pio::outb(0x0d, 0x00);
            fstart_pio::outb(0x0b, 0x40);
            fstart_pio::outb(0x0b, 0x41);
            fstart_pio::outb(0x0b, 0x42);
            fstart_pio::outb(0x0b, 0x43);
            fstart_pio::outb(0xda, 0x00);
            fstart_pio::outb(0xd6, 0xc0);
            fstart_pio::outb(0xd6, 0x41);
            fstart_pio::outb(0xd6, 0x42);
            fstart_pio::outb(0xd6, 0x43);
            fstart_pio::outb(0xd4, 0x00);
            fstart_pio::outb(0x0f, 0x0f);
            let _ = fstart_pio::inb(0x80);
        }
    }

    fn i8259_init(&self) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            fstart_pio::outb(0x20, 0x11);
            fstart_pio::outb(0xa0, 0x11);
            fstart_pio::outb(0x21, 0x20);
            fstart_pio::outb(0xa1, 0x28);
            fstart_pio::outb(0x21, 0x04);
            fstart_pio::outb(0xa1, 0x02);
            fstart_pio::outb(0x21, 0x01);
            fstart_pio::outb(0xa1, 0x01);
            fstart_pio::outb(0x21, 0xff);
            fstart_pio::outb(0xa1, 0xff);
            let elcr2 = fstart_pio::inb(0x4d1);
            fstart_pio::outb(0x4d1, elcr2 | (1 << 1));
        }
    }

    fn enable_acpi_pm1(&self) {
        let pm1 =
            (self.pm().read32(pmio::PM1_CNT) & !pmio::SLP_TYP_MASK) | pmio::BM_RLD | pmio::SCI_EN;
        self.pm().write32(pmio::PM1_CNT, pm1);
    }

    fn function_disable_mask(&self) -> u32 {
        let mut fd = 0u32;
        if self.config.hda.is_none() {
            fd |= ich8::FD_HDAD;
        }
        if self.config.sata.is_none() {
            fd |= ich8::FD_SAD1;
        }
        if self.config.disable_sata2 {
            fd |= ich8::FD_SAD2;
        }
        if self.config.disable_thermal {
            fd |= ich8::FD_TTD;
        }
        match self.config.usb {
            Some(usb) => {
                const UHCI: [(u8, u8); 5] = [(0x1d, 0), (0x1d, 1), (0x1d, 2), (0x1a, 0), (0x1a, 1)];
                for (idx, (dev, func)) in UHCI.iter().copied().enumerate() {
                    if !usb.uhci[idx] {
                        fd |= Self::usb_fd_bit(dev, func);
                    }
                }
                if !usb.ehci[0] {
                    fd |= ich8::FD_EHCI1D;
                }
                if !usb.ehci[1] {
                    fd |= ich8::FD_EHCI2D;
                }
            }
            None => {
                fd |= ich8::FD_U1D
                    | ich8::FD_U2D
                    | ich8::FD_U3D
                    | ich8::FD_U4D
                    | ich8::FD_U5D
                    | ich8::FD_EHCI1D
                    | ich8::FD_EHCI2D;
            }
        }
        for (idx, enabled) in self.config.pcie_ports.iter().enumerate() {
            if !*enabled {
                fd |= ich8::FD_PE1D << idx;
            }
        }
        fd
    }

    fn clear_pci_command(dev: u8, func: u8) {
        let pci = ecam::PciDevBdf::new(0, dev, func);
        if pci.read16(0) != 0xffff {
            pci.and16(
                ich8::PCI_COMMAND,
                !(ich8::PCI_CMD_IO | ich8::PCI_CMD_MEMORY | ich8::PCI_CMD_MASTER),
            );
        }
    }

    fn clear_disabled_device_commands(&self) {
        if self.config.disable_lan {
            Self::clear_pci_command(ich8::LAN_DEV, ich8::LAN_FUNC);
        }
        if self.config.hda.is_none() {
            Self::clear_pci_command(ich8::HDA_DEV, ich8::HDA_FUNC);
        }
        if self.config.sata.is_none() {
            Self::clear_pci_command(ich8::SATA_DEV, ich8::SATA_FUNC);
        }
        if self.config.disable_sata2 {
            Self::clear_pci_command(ich8::SATA2_DEV, ich8::SATA2_FUNC);
        }
        if self.config.disable_thermal {
            Self::clear_pci_command(ich8::THERMAL_DEV, ich8::THERMAL_FUNC);
        }
        if let Some(usb) = self.config.usb {
            const UHCI: [(u8, u8); 5] = [(0x1d, 0), (0x1d, 1), (0x1d, 2), (0x1a, 0), (0x1a, 1)];
            for (idx, (dev, func)) in UHCI.iter().copied().enumerate() {
                if !usb.uhci[idx] {
                    Self::clear_pci_command(dev, func);
                }
            }
            if !usb.ehci[0] {
                Self::clear_pci_command(ich8::EHCI1_DEV, ich8::EHCI1_FUNC);
            }
            if !usb.ehci[1] {
                Self::clear_pci_command(ich8::EHCI2_DEV, ich8::EHCI2_FUNC);
            }
        }
        for (idx, enabled) in self.config.pcie_ports.iter().enumerate() {
            if !*enabled {
                Self::clear_pci_command(ich8::PCIE_DEV, idx as u8);
            }
        }
    }

    fn detect_s3_resume(&self) -> bool {
        if (self.lpc().read8(ich8::GEN_PMCON_3) & GEN_PMCON_3_RTC_POWER_FAILED) != 0 {
            return false;
        }
        let pm1_cnt = self.pm().read32(pmio::PM1_CNT);
        let slp_typ = pm1_cnt & pmio::SLP_TYP_MASK;
        if slp_typ == SLP_TYP_S3 {
            self.pm()
                .write32(pmio::PM1_CNT, pm1_cnt & !pmio::SLP_TYP_MASK);
            true
        } else {
            false
        }
    }

    fn write_pirq_routes(&self) {
        let lpc = self.lpc();
        let pirq_low = u32::from_le_bytes([
            self.config.pirq_routing[0],
            self.config.pirq_routing[1],
            self.config.pirq_routing[2],
            self.config.pirq_routing[3],
        ]);
        let pirq_high = u32::from_le_bytes([
            self.config.pirq_routing[4],
            self.config.pirq_routing[5],
            self.config.pirq_routing[6],
            self.config.pirq_routing[7],
        ]);
        lpc.write32(ich8::PIRQA_ROUT, pirq_low);
        lpc.write32(ich8::PIRQE_ROUT, pirq_high);
    }

    fn configure_default_intmap(&self) {
        let rcba = self.rcba();
        rcba.write32(ich8::D31IP, 0x0400_3210);
        rcba.write32(ich8::D30IP, 0x0000_0001);
        rcba.write32(ich8::D29IP, 0x1000_0321);
        rcba.write32(ich8::D28IP, 0x0021_4321);
        rcba.write32(ich8::D27IP, 0x0000_0001);
        rcba.write32(ich8::D26IP, 0x1000_0021);
        rcba.write32(ich8::D25IP, 0x0000_0001);

        rcba.write16(ich8::D31IR, 0x1100);
        rcba.write16(ich8::D30IR, 0x0000);
        rcba.write16(ich8::D29IR, 0x0002);
        rcba.write16(ich8::D28IR, 0x3210);
        rcba.write16(ich8::D27IR, 0x0003);
        rcba.write16(ich8::D26IR, 0x0003);
        rcba.write16(ich8::D25IR, 0x0001);
        self.enable_ioapic();
    }

    fn configure_late_rcba(&self, config: &Ich8LateRcbaConfig) {
        let rcba = self.rcba();
        rcba.write32(ich8::D31IP, config.d31ip);
        if let Some(d30ip) = config.d30ip {
            rcba.write32(ich8::D30IP, d30ip);
        }
        rcba.write32(ich8::D29IP, config.d29ip);
        rcba.write32(ich8::D28IP, config.d28ip);
        rcba.write32(ich8::D27IP, config.d27ip);
        if let Some(d26ip) = config.d26ip {
            rcba.write32(ich8::D26IP, d26ip);
        }
        if let Some(d25ip) = config.d25ip {
            rcba.write32(ich8::D25IP, d25ip);
        }

        rcba.write16(ich8::D31IR, config.d31ir);
        rcba.write16(ich8::D30IR, config.d30ir);
        rcba.write16(ich8::D29IR, config.d29ir);
        rcba.write16(ich8::D28IR, config.d28ir);
        rcba.write16(ich8::D27IR, config.d27ir);
        if let Some(d26ir) = config.d26ir {
            rcba.write16(ich8::D26IR, d26ir);
        }
        if let Some(d25ir) = config.d25ir {
            rcba.write16(ich8::D25IR, d25ir);
        }

        if let Some(iotr3) = config.iotr3 {
            rcba.write32(ich8::IOTR3_LO, iotr3.lo);
            rcba.write32(ich8::IOTR3_HI, iotr3.hi);
        }
    }
}

impl Device for IntelIch8 {
    const NAME: &'static str = "intel-ich8";
    const COMPATIBLE: &'static [&'static str] = &["intel,ich8", "intel,ich8m", "intel,82801hx"];
    type Config = IntelIch8Config;

    fn new(config: &IntelIch8Config) -> Result<Self, DeviceError> {
        if config
            .lpc_decode
            .generic_io
            .iter()
            .copied()
            .any(|range| range.encode().is_none())
        {
            return Err(DeviceError::ConfigError);
        }

        Ok(Self {
            config: config.clone(),
            smbus: None,
            pm: PmIo::new(ich8::DEFAULT_PMBASE),
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Keep construction side-effect free. The pre-console hook programs
        // RCBA/PMBASE/GPIO/LPC before any hardware-dependent work runs.
        Ok(())
    }
}

impl PreConsoleInit for IntelIch8 {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.enable_spi_prefetching_and_caching();
        self.program_fixed_bars();
        self.reset_watchdog_and_cmos();
        self.program_lpc_decode();
        self.setup_gpios();
        Ok(())
    }
}

impl EarlyInit for IntelIch8 {
    fn early_init(&mut self) -> Result<(), ServiceError> {
        self.enable_spi_prefetching_and_caching();
        self.program_fixed_bars();
        self.program_lpc_decode();
        self.enable_smbus();
        self.write_pirq_routes();
        self.reset_watchdog_and_cmos();
        self.clear_disabled_device_commands();
        let rcba = self.rcba();
        let fd = self.function_disable_mask();
        rcba.write32(ich8::RCBA_FD, fd);
        if self.config.disable_lan {
            rcba.write32(
                ich8::RCBA_FDSW,
                rcba.read32(ich8::RCBA_FDSW) | ich8::FDSW_LAND,
            );
        }
        self.early_chipset_settings();
        self.pm().write32(GPE0_STS_ICH8, 0xffff_ffff);
        self.pm().write32(GPE0_EN_ICH8, self.config.gpe0_en);
        self.setup_gpios();
        self.enable_hpet();
        self.setup_dmi();
        let _ = self.detect_s3_resume();
        fstart_log::info!("intel-ich8: early init complete (fd_mask={:#x})", fd);
        Ok(())
    }
}

impl PostDramInit for IntelIch8 {
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.poll_vc1();
        self.early_chipset_settings();
        self.pcie_init();
        self.pci_bridge_init();
        self.usb_init();
        if let Some(ide) = self.config.ide.as_ref() {
            self.ide_init(ide);
        }
        if let Some(hda) = self.config.hda.as_ref() {
            self.hda_init(hda);
        }
        if let Some(sata) = self.config.sata.as_ref() {
            self.sata_init(sata);
        }
        self.ramstage_lpc_init();
        self.configure_default_intmap();
        if let Some(late_rcba) = self.config.late_rcba.as_ref() {
            self.configure_late_rcba(late_rcba);
        }
        fstart_log::info!("intel-ich8: ramstage init complete");
        Ok(())
    }
}

impl FinalizeInit for IntelIch8 {
    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        let rcba = self.rcba();
        rcba.write32(ich8::RCBA_FDSW, rcba.read32(ich8::RCBA_FDSW) | (1 << 7));
        rcba.write32(ich8::RCBA_MAP, rcba.read32(ich8::RCBA_MAP));
        Ok(())
    }
}

impl Southbridge for IntelIch8 {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        PreConsoleInit::pre_console_init(self)
    }

    fn early_init(&mut self) -> Result<(), ServiceError> {
        EarlyInit::early_init(self)
    }

    fn ramstage_init(&mut self) -> Result<(), ServiceError> {
        PostDramInit::post_dram_init(self)
    }

    fn finalize(&mut self) -> Result<(), ServiceError> {
        FinalizeInit::finalize_init(self)
    }
}

impl SmBus for IntelIch8 {
    fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
        match self.smbus.as_mut() {
            Some(bus) => bus.read_byte(addr, cmd),
            None => Err(ServiceError::HardwareError),
        }
    }

    fn write_byte(&mut self, addr: u8, cmd: u8, value: u8) -> Result<(), ServiceError> {
        match self.smbus.as_mut() {
            Some(bus) => bus.write_byte(addr, cmd, value),
            None => Err(ServiceError::HardwareError),
        }
    }
}

// ---------------------------------------------------------------------------
// ACPI device implementation — ICH8-M southbridge devices
// ---------------------------------------------------------------------------

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi_macros::acpi_dsl;

    use super::*;

    impl AcpiDevice for IntelIch8 {
        type Config = IntelIch8Config;

        /// Produce ICH8-M DSDT content under `\\_SB.PCI0`.
        ///
        /// This is the reusable southbridge ACPI namespace: LPC legacy
        /// devices, PIRQ links, USB/HDA/SATA/SMBus/PCIe/PCI bridge device
        /// nodes, and APIC-mode `_PRT` routing. Mainboard-specific EC,
        /// dock/GPE/SMI trap glue is emitted by mainboard drivers.
        fn dsdt_aml(&self, _config: &Self::Config) -> Vec<u8> {
            let mut aml = acpi_dsl! {
                Scope("\\") {
                    OperationRegion("PMIO", SystemIO, 0x0500u32, 0x80u32);
                    Field("PMIO", ByteAcc, NoLock, Preserve) {
                        Offset(0x11),
                        THRO, 1,
                        Offset(0x42),
                        , 1,
                        GPEC, 1,
                        Offset(0x64),
                        , 9,
                        SCIS, 1,
                    }
                    OperationRegion("GPIO", SystemIO, 0x0580u32, 0x3Cu32);
                    Field("GPIO", ByteAcc, NoLock, Preserve) {
                        Offset(0x0C),
                        GP00, 1, GP01, 1, GP02, 1, GP03, 1,
                        GP04, 1, GP05, 1, GP06, 1, GP07, 1,
                        GP08, 1, GP09, 1, GP10, 1, GP11, 1,
                        GP12, 1, GP13, 1, GP14, 1, GP15, 1,
                        GP16, 1, GP17, 1, GP18, 1, GP19, 1,
                        GP20, 1, GP21, 1, GP22, 1, GP23, 1,
                        GP24, 1, GP25, 1, GP26, 1, GP27, 1,
                        GP28, 1, GP29, 1, GP30, 1, GP31, 1,
                        Offset(0x38),
                        GP32, 1, GP33, 1, GP34, 1, GP35, 1,
                        GP36, 1, GP37, 1, GP38, 1, GP39, 1,
                    }
                }
            };

            aml.extend_from_slice(&acpi_dsl! {
                Scope("\\_SB_.PCI0") {
                    OperationRegion("RCRB", SystemMemory, 0xFED1C000u32, 0x4000u32);
                    Field("RCRB", DWordAcc, Lock, Preserve) {
                        Offset(0x3404),
                        HPAS, 2,
                        , 5,
                        HPTE, 1,
                        Offset(0x3418),
                        , 2,
                        SA1D, 1,
                        SMBD, 1,
                        HDAD, 1,
                        , 3,
                        US1D, 1,
                        US2D, 1,
                        US3D, 1,
                        US4D, 1,
                        US5D, 1,
                        EH2D, 1,
                        LPBD, 1,
                        EH1D, 1,
                        Offset(0x341A),
                        RP1D, 1,
                        RP2D, 1,
                        RP3D, 1,
                        RP4D, 1,
                        RP5D, 1,
                        RP6D, 1,
                        , 2,
                        THRD, 1,
                    }

                    Device("HDEF") {
                        Name("_ADR", 0x001B0000u32);
                        Name("_PRW", Package(5u32, 4u32));
                    }

                    Device("RP01") {
                        Name("_ADR", 0x001C0000u32);
                        Name("_PRT", Package(
                            Package(0x0000FFFFu32, 0u32, 0u32, 16u32),
                            Package(0x0000FFFFu32, 1u32, 0u32, 17u32),
                            Package(0x0000FFFFu32, 2u32, 0u32, 18u32),
                            Package(0x0000FFFFu32, 3u32, 0u32, 19u32)
                        ));
                    }
                    Device("RP02") {
                        Name("_ADR", 0x001C0001u32);
                        Name("_PRT", Package(
                            Package(0x0000FFFFu32, 0u32, 0u32, 17u32),
                            Package(0x0000FFFFu32, 1u32, 0u32, 18u32),
                            Package(0x0000FFFFu32, 2u32, 0u32, 19u32),
                            Package(0x0000FFFFu32, 3u32, 0u32, 16u32)
                        ));
                    }
                    Device("RP03") {
                        Name("_ADR", 0x001C0002u32);
                        Name("_PRT", Package(
                            Package(0x0000FFFFu32, 0u32, 0u32, 18u32),
                            Package(0x0000FFFFu32, 1u32, 0u32, 19u32),
                            Package(0x0000FFFFu32, 2u32, 0u32, 16u32),
                            Package(0x0000FFFFu32, 3u32, 0u32, 17u32)
                        ));
                    }
                    Device("RP04") {
                        Name("_ADR", 0x001C0003u32);
                        Name("_PRT", Package(
                            Package(0x0000FFFFu32, 0u32, 0u32, 19u32),
                            Package(0x0000FFFFu32, 1u32, 0u32, 16u32),
                            Package(0x0000FFFFu32, 2u32, 0u32, 17u32),
                            Package(0x0000FFFFu32, 3u32, 0u32, 18u32)
                        ));
                    }
                    Device("RP05") {
                        Name("_ADR", 0x001C0004u32);
                        Name("_PRT", Package(
                            Package(0x0000FFFFu32, 0u32, 0u32, 16u32),
                            Package(0x0000FFFFu32, 1u32, 0u32, 17u32),
                            Package(0x0000FFFFu32, 2u32, 0u32, 18u32),
                            Package(0x0000FFFFu32, 3u32, 0u32, 19u32)
                        ));
                    }
                    Device("RP06") {
                        Name("_ADR", 0x001C0005u32);
                        Name("_PRT", Package(
                            Package(0x0000FFFFu32, 0u32, 0u32, 17u32),
                            Package(0x0000FFFFu32, 1u32, 0u32, 18u32),
                            Package(0x0000FFFFu32, 2u32, 0u32, 19u32),
                            Package(0x0000FFFFu32, 3u32, 0u32, 16u32)
                        ));
                    }

                    Device("USB1") { Name("_ADR", 0x001D0000u32); Name("_PRW", Package(3u32, 4u32)); Method("_S3D", 0, NotSerialized) { Return(2u32); } Method("_S4D", 0, NotSerialized) { Return(2u32); } }
                    Device("USB2") { Name("_ADR", 0x001D0001u32); Name("_PRW", Package(3u32, 4u32)); Method("_S3D", 0, NotSerialized) { Return(2u32); } Method("_S4D", 0, NotSerialized) { Return(2u32); } }
                    Device("USB3") { Name("_ADR", 0x001D0002u32); Name("_PRW", Package(3u32, 4u32)); Method("_S3D", 0, NotSerialized) { Return(2u32); } Method("_S4D", 0, NotSerialized) { Return(2u32); } }
                    Device("USB4") { Name("_ADR", 0x001A0000u32); Name("_PRW", Package(3u32, 4u32)); Method("_S3D", 0, NotSerialized) { Return(2u32); } Method("_S4D", 0, NotSerialized) { Return(2u32); } }
                    Device("USB5") { Name("_ADR", 0x001A0001u32); Name("_PRW", Package(3u32, 4u32)); Method("_S3D", 0, NotSerialized) { Return(2u32); } Method("_S4D", 0, NotSerialized) { Return(2u32); } }
                    Device("USB6") { Name("_ADR", 0x001A0002u32); Name("_PRW", Package(3u32, 4u32)); Method("_S3D", 0, NotSerialized) { Return(2u32); } Method("_S4D", 0, NotSerialized) { Return(2u32); } }
                    Device("EHC1") { Name("_ADR", 0x001D0007u32); Name("_PRW", Package(13u32, 4u32)); Method("_S3D", 0, NotSerialized) { Return(2u32); } Method("_S4D", 0, NotSerialized) { Return(2u32); } }
                    Device("EHC2") { Name("_ADR", 0x001A0007u32); Name("_PRW", Package(13u32, 4u32)); Method("_S3D", 0, NotSerialized) { Return(2u32); } Method("_S4D", 0, NotSerialized) { Return(2u32); } }

                    Device("PCIB") {
                        Name("_ADR", 0x001E0000u32);
                        Name("_PRT", Package(
                            Package(0x0000FFFFu32, 0u32, 0u32, 16u32),
                            Package(0x0000FFFFu32, 1u32, 0u32, 17u32),
                            Package(0x0000FFFFu32, 2u32, 0u32, 18u32),
                            Package(0x0000FFFFu32, 3u32, 0u32, 19u32),
                            Package(0x0001FFFFu32, 0u32, 0u32, 16u32),
                            Package(0x0002FFFFu32, 0u32, 0u32, 21u32),
                            Package(0x0002FFFFu32, 1u32, 0u32, 22u32),
                            Package(0x0008FFFFu32, 0u32, 0u32, 20u32)
                        ));
                    }

                    Device("SATA") { Name("_ADR", 0x001F0002u32); }
                    Device("SBUS") { Name("_ADR", 0x001F0003u32); }

                    Device("LPCB") {
                        Name("_ADR", 0x001F0000u32);
                        OperationRegion("LPC0", PciConfig, 0x00u32, 0x100u32);
                        Field("LPC0", AnyAcc, NoLock, Preserve) {
                            Offset(0x40),
                            PMBS, 16,
                            Offset(0x60),
                            PRTA, 8, PRTB, 8, PRTC, 8, PRTD, 8,
                            Offset(0x68),
                            PRTE, 8, PRTF, 8, PRTG, 8, PRTH, 8,
                            Offset(0x80),
                            IOD0, 8, IOD1, 8,
                        }

                        Device("LNKA") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 1u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKB") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 2u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKC") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 3u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKD") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 4u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKE") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 5u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKF") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 6u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKG") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 7u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }
                        Device("LNKH") { Name("_HID", EisaId("PNP0C0F")); Name("_UID", 8u32); Method("_STA", 0, NotSerialized) { Return(0x0Bu32); } }

                        Device("DMAC") { Name("_HID", EisaId("PNP0200")); Name("_CRS", ResourceTemplate { IO(0x0000u16, 0x0000u16, 0x01u8, 0x20u8); IO(0x0081u16, 0x0081u16, 0x01u8, 0x11u8); IO(0x0093u16, 0x0093u16, 0x01u8, 0x0Du8); IO(0x00C0u16, 0x00C0u16, 0x01u8, 0x20u8); }); }
                        Device("FWH_") { Name("_HID", EisaId("INT0800")); Name("_CRS", ResourceTemplate { Memory32Fixed(ReadOnly, 0xFF000000u32, 0x01000000u32); }); }
                        Device("HPET") { Name("_HID", EisaId("PNP0103")); Name("_CID", 0x010CD041u32); Name("_CRS", ResourceTemplate { Memory32Fixed(ReadOnly, 0xFED00000u32, 0x400u32); }); }
                        Device("PIC_") { Name("_HID", EisaId("PNP0000")); Name("_CRS", ResourceTemplate { IO(0x0020u16, 0x0020u16, 0x01u8, 0x02u8); IO(0x00A0u16, 0x00A0u16, 0x01u8, 0x02u8); IO(0x04D0u16, 0x04D0u16, 0x01u8, 0x02u8); Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 2u32); }); }
                        Device("MATH") { Name("_HID", EisaId("PNP0C04")); Name("_CRS", ResourceTemplate { IO(0x00F0u16, 0x00F0u16, 0x01u8, 0x01u8); Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 13u32); }); }
                        Device("LDRC") { Name("_HID", EisaId("PNP0C02")); Name("_UID", 2u32); Name("_CRS", ResourceTemplate { IO(0x002Eu16, 0x002Eu16, 0x01u8, 0x02u8); IO(0x004Eu16, 0x004Eu16, 0x01u8, 0x02u8); IO(0x0061u16, 0x0061u16, 0x01u8, 0x01u8); IO(0x0080u16, 0x0080u16, 0x01u8, 0x01u8); IO(0x00B2u16, 0x00B2u16, 0x01u8, 0x02u8); IO(0x0500u16, 0x0500u16, 0x01u8, 0x80u8); IO(0x0580u16, 0x0580u16, 0x01u8, 0x40u8); }); }
                        Device("RTC_") { Name("_HID", EisaId("PNP0B00")); Name("_CRS", ResourceTemplate { IO(0x0070u16, 0x0070u16, 0x01u8, 0x08u8); }); }
                        Device("TIMR") { Name("_HID", EisaId("PNP0100")); Name("_CRS", ResourceTemplate { IO(0x0040u16, 0x0040u16, 0x01u8, 0x04u8); IO(0x0050u16, 0x0050u16, 0x10u8, 0x04u8); Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 0u32); }); }
                        Device("PS2K") { Name("_HID", EisaId("PNP0303")); Name("_CID", EisaId("PNP030B")); Name("_CRS", ResourceTemplate { IO(0x0060u16, 0x0060u16, 0x01u8, 0x01u8); IO(0x0064u16, 0x0064u16, 0x01u8, 0x01u8); Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 1u32); }); Method("_STA", 0, NotSerialized) { Return(0x0Fu32); } }
                        Device("PS2M") { Name("_HID", EisaId("PNP0F13")); Name("_CRS", ResourceTemplate { Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, 12u32); }); Method("_STA", 0, NotSerialized) { Return(0x0Fu32); } }
                    }
                }
            });

            aml
        }

        fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }
}
