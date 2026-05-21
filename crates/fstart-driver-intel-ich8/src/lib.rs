//! Intel ICH8 / ICH8-M southbridge driver.
//!
//! The initial target is the Lenovo ThinkPad X61 (GM965 + ICH8-M/HX). The
//! reusable pre-console path opens the southbridge LPC/GPIO decode needed by
//! board hooks. X61-specific DLPC/dock SuperIO setup lives in the
//! `fstart-mainboard-lenovo-x61` crate.

#![recursion_limit = "256"]
#![no_std]

pub mod smm;

use fstart_ecam as ecam;
use fstart_gpio_ich::IchGpio;
use fstart_mmio::MmioReadWrite;
use fstart_pci::{pci_type0_config, PciType0Config, PciType1Config, PCI_COMMAND_BITS};
use fstart_pmio_ich::{self as pmio, PmIo};
use fstart_services::device::{Device, DeviceError};
use fstart_services::{
    EarlyInit, FinalizeInit, PostDramInit, PreConsoleInit, ServiceError, SmBus, Southbridge,
};
use fstart_smbus_intel::I801SmBus;
use heapless::Vec as HVec;
use serde::{Deserialize, Serialize};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

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
    pub const PMIR_USB_TRANSIENT_DISCONNECT: u32 = 3 << 8;
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

register_bitfields! [u32,
    /// Virtual channel control/status registers.
    VCTL [
        ENABLE OFFSET(31) NUMBITS(1) [],
        VC_NEGOTIATION_PENDING OFFSET(16) NUMBITS(1) [],
        VC_ARB_SELECT OFFSET(17) NUMBITS(3) [],
        ID OFFSET(24) NUMBITS(3) [],
        TC_MAP OFFSET(1) NUMBITS(7) []
    ],
    /// Virtual channel capability register.
    VCAP [
        REFERENCE_CLOCK OFFSET(16) NUMBITS(7) []
    ],
    /// Function Disable register.
    FD [
        RAW OFFSET(0) NUMBITS(32) []
    ],
    /// General Control and Status.
    GCS_REG [
        BBS OFFSET(10) NUMBITS(1) [],
        BOOT_SMI_EN OFFSET(6) NUMBITS(1) [],
        NO_REBOOT OFFSET(5) NUMBITS(1) [],
        SMI_LOCK OFFSET(4) NUMBITS(1) []
    ],
    /// Function Disable SUS Well register.
    FDSW [
        FUNCTION_DISABLE_LOCK OFFSET(7) NUMBITS(1) [],
        LAN_DISABLE OFFSET(0) NUMBITS(1) []
    ],
    /// HPET Configuration.
    HPTC [
        ADDRESS_SELECT OFFSET(0) NUMBITS(2) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    /// Clock gating control.
    CG [
        RAW OFFSET(0) NUMBITS(32) []
    ],
    /// Root-port function-number map.
    RPFN [
        RAW OFFSET(0) NUMBITS(32) []
    ],
    /// DMI controls.
    DMC [
        MOBILE_POWER_SAVINGS OFFSET(19) NUMBITS(1) []
    ],
    DMIC [
        VIRTUAL_CHANNEL_ENABLE OFFSET(0) NUMBITS(2) []
    ],
    LCAP_REG [
        ASPM_SUPPORT OFFSET(10) NUMBITS(2) []
    ],
    CIR5_REG [
        BIT0 OFFSET(0) NUMBITS(1) []
    ],
    CIR6_REG [
        BIT7 OFFSET(7) NUMBITS(1) [],
        FIELD_23_21 OFFSET(21) NUMBITS(3) []
    ],
    CIR8_REG [
        FIELD_1_0 OFFSET(0) NUMBITS(2) []
    ],
    CIR9_REG [
        FIELD_27_26 OFFSET(26) NUMBITS(2) []
    ],
    CIR7_REG [
        FIELD_19_16 OFFSET(16) NUMBITS(4) []
    ],
    CIR13_REG [
        FIELD_19_16 OFFSET(16) NUMBITS(4) []
    ],
    CIR10_REG [
        FIELD_17_16 OFFSET(16) NUMBITS(2) []
    ],
    BIOS_CNTL_REG [
        EXTENDED_CMOS_ENABLE OFFSET(2) NUMBITS(1) []
    ],
    SPI_PREFETCH_REG [
        ENABLE OFFSET(0) NUMBITS(3) []
    ],
    /// LPC PMIR register.
    PMIR_REG [
        USB_TRANSIENT_DISCONNECT OFFSET(8) NUMBITS(2) [],
        CF9GR OFFSET(20) NUMBITS(1) []
    ],
];

register_bitfields! [u16,
    /// LPC GEN_PMCON_1 register.
    GEN_PMCON_1_REG [
        AFTERG3_EN OFFSET(0) NUMBITS(2) [],
        SLP_S4_ASST_EN OFFSET(2) NUMBITS(1) [],
        SUS_PWR_FLR OFFSET(3) NUMBITS(1) [],
        DIS_SLP_X_STRCH_SUS_UP OFFSET(5) NUMBITS(1) [],
        C4_ON_C3_EN OFFSET(7) NUMBITS(1) [],
        BIOS_PCI_EXP_EN OFFSET(10) NUMBITS(1) [],
        C5_EN OFFSET(11) NUMBITS(1) []
    ],
    /// DMI link status.
    LSTS [
        NEGOTIATED_WIDTH OFFSET(4) NUMBITS(6) []
    ],
    LCTL_REG [
        ASPM_CONTROL OFFSET(0) NUMBITS(2) []
    ],
    CIR_20C4_REG [
        BIT15 OFFSET(15) NUMBITS(1) []
    ],
    CIR_20E4_REG [
        BIT15 OFFSET(15) NUMBITS(1) []
    ],
];

register_bitfields! [u8,
    /// LPC ACPI control register.
    ACPI_CNTL_REG [
        ACPI_EN OFFSET(7) NUMBITS(1) []
    ],
    /// LPC GPIO control register.
    GPIO_CNTL_REG [
        GPIO_EN OFFSET(4) NUMBITS(1) []
    ],
    /// LPC GEN_PMCON_3 register.
    GEN_PMCON_3_REG [
        STATE_AFTER_G3 OFFSET(0) NUMBITS(1) [],
        RTC_POWER_FAILED OFFSET(1) NUMBITS(1) [],
        RTC_BATTERY_DEAD OFFSET(2) NUMBITS(1) [],
        MIN_SLP_S4_ASSERT OFFSET(3) NUMBITS(1) [],
        SLP_S3_STRETCH OFFSET(4) NUMBITS(2) []
    ],
    /// LPC C-state configuration.
    CXSTATE_CNF_REG [
        C3_POPUP_ENABLE OFFSET(2) NUMBITS(1) [],
        BM_STS_ZERO_ENABLE OFFSET(3) NUMBITS(1) [],
        C3_POPDOWN_ENABLE OFFSET(4) NUMBITS(1) []
    ],
    /// LPC C4 timing control.
    C4TIMING_CNT_REG [
        VALUE OFFSET(0) NUMBITS(4) []
    ],
    /// LPC C5 exit timing.
    C5_EXIT_TIMING_REG [
        C5_EXIT OFFSET(0) NUMBITS(3) [],
        C6_EXIT OFFSET(3) NUMBITS(3) []
    ],
    /// Other Interrupt Control.
    OIC_REG [
        AEN OFFSET(0) NUMBITS(1) [],
        OAEN OFFSET(1) NUMBITS(1) []
    ],
];

pci_type0_config! {
    /// ICH8 LPC bridge PCI configuration space.
    pub struct Ich8LpcPciConfig {
        (0x40 => pub pmbase: MmioReadWrite<u32>),
        (0x44 => pub acpi_cntl: MmioReadWrite<u8, ACPI_CNTL_REG::Register>),
        (0x45 => _reserved_lpc0),
        (0x48 => pub gpio_base: MmioReadWrite<u32>),
        (0x4c => pub gpio_cntl: MmioReadWrite<u8, GPIO_CNTL_REG::Register>),
        (0x4d => _reserved_lpc1),
        (0x60 => pub pirqa_rout: MmioReadWrite<u32>),
        (0x64 => pub serirq_cntl: MmioReadWrite<u8>),
        (0x65 => _reserved_lpc2),
        (0x68 => pub pirqe_rout: MmioReadWrite<u32>),
        (0x6c => _reserved_lpc3),
        (0x80 => pub lpc_io_dec: MmioReadWrite<u16>),
        (0x82 => pub lpc_en: MmioReadWrite<u16>),
        (0x84 => pub gen_dec: [MmioReadWrite<u32>; 4]),
        (0x94 => _reserved_lpc4),
        (0xa0 => pub gen_pmcon_1: MmioReadWrite<u16, GEN_PMCON_1_REG::Register>),
        (0xa2 => _reserved_lpc5),
        (0xa4 => pub gen_pmcon_3: MmioReadWrite<u8, GEN_PMCON_3_REG::Register>),
        (0xa5 => _reserved_lpc6),
        (0xa8 => pub c5_exit_timing: MmioReadWrite<u8, C5_EXIT_TIMING_REG::Register>),
        (0xa9 => pub cxstate_cnf: MmioReadWrite<u8, CXSTATE_CNF_REG::Register>),
        (0xaa => pub c4timing_cnt: MmioReadWrite<u8, C4TIMING_CNT_REG::Register>),
        (0xab => _reserved_lpc7),
        (0xac => pub pmir: MmioReadWrite<u32, PMIR_REG::Register>),
        (0xb0 => _reserved_lpc8),
        (0xb8 => pub gpio_rout: MmioReadWrite<u32>),
        (0xbc => _reserved_lpc9),
        (0x1000 => @END),
    }
}

register_structs! {
    /// ICH8 Root Complex Base Address MMIO register block.
    RcbaRegs {
        (0x0000 => _reserved_start),
        (0x0014 => pub v0ctl: MmioReadWrite<u32, VCTL::Register>),
        (0x0018 => _reserved_v0ctl),
        (0x001c => pub v1cap: MmioReadWrite<u32, VCAP::Register>),
        (0x0020 => pub v1ctl: MmioReadWrite<u32, VCTL::Register>),
        (0x0024 => _reserved_v1ctl),
        (0x0026 => pub v1sts: MmioReadWrite<u16>),
        (0x0028 => _reserved0),
        (0x0030 => pub pat: [MmioReadWrite<u8>; 64]),
        (0x0070 => _reserved1),
        (0x0088 => pub cir1: MmioReadWrite<u32>),
        (0x008c => _reserved2),
        (0x0104 => pub esd: [MmioReadWrite<u8>; 4]),
        (0x0108 => _reserved3),
        (0x0110 => pub uld: [MmioReadWrite<u8>; 4]),
        (0x0114 => _reserved4),
        (0x0118 => pub ulba: MmioReadWrite<u32>),
        (0x011c => _reserved5),
        (0x01a4 => pub lcap: MmioReadWrite<u32, LCAP_REG::Register>),
        (0x01a8 => pub lctl: MmioReadWrite<u16, LCTL_REG::Register>),
        (0x01aa => pub lsts: MmioReadWrite<u16, LSTS::Register>),
        (0x01ac => _reserved6),
        (0x01f4 => pub cir2: MmioReadWrite<u32>),
        (0x01f8 => _reserved7),
        (0x01fc => pub cir3: MmioReadWrite<u16>),
        (0x01fe => _reserved8),
        (0x0200 => pub cir4: MmioReadWrite<u32>),
        (0x0204 => _reserved9),
        (0x0220 => pub bcr: MmioReadWrite<u8>),
        (0x0221 => _reserved10),
        (0x0234 => pub dmic: MmioReadWrite<u32, DMIC::Register>),
        (0x0238 => pub rpfn: MmioReadWrite<u32, RPFN::Register>),
        (0x023c => _reserved11),
        (0x0f20 => pub cir13: MmioReadWrite<u32, CIR13_REG::Register>),
        (0x0f24 => _reserved12),
        (0x1d40 => pub cir5: MmioReadWrite<u32, CIR5_REG::Register>),
        (0x1d44 => _reserved13),
        (0x1e98 => pub iotr3_lo: MmioReadWrite<u32>),
        (0x1e9c => pub iotr3_hi: MmioReadWrite<u32>),
        (0x1ea0 => _reserved14),
        (0x2010 => pub dmc: MmioReadWrite<u32, DMC::Register>),
        (0x2014 => _reserved15),
        (0x2024 => pub cir6: MmioReadWrite<u32, CIR6_REG::Register>),
        (0x2028 => _reserved16),
        (0x2034 => pub cir7: MmioReadWrite<u32, CIR7_REG::Register>),
        (0x2038 => _reserved17),
        (0x20c4 => pub cir_20c4: MmioReadWrite<u16, CIR_20C4_REG::Register>),
        (0x20c6 => _reserved18),
        (0x20e4 => pub cir_20e4: MmioReadWrite<u16, CIR_20E4_REG::Register>),
        (0x20e6 => _reserved19),
        (0x3100 => pub d31ip: MmioReadWrite<u32>),
        (0x3104 => pub d30ip: MmioReadWrite<u32>),
        (0x3108 => pub d29ip: MmioReadWrite<u32>),
        (0x310c => pub d28ip: MmioReadWrite<u32>),
        (0x3110 => pub d27ip: MmioReadWrite<u32>),
        (0x3114 => pub d26ip: MmioReadWrite<u32>),
        (0x3118 => pub d25ip: MmioReadWrite<u32>),
        (0x311c => _reserved20),
        (0x3140 => pub d31ir: MmioReadWrite<u16>),
        (0x3142 => pub d30ir: MmioReadWrite<u16>),
        (0x3144 => pub d29ir: MmioReadWrite<u16>),
        (0x3146 => pub d28ir: MmioReadWrite<u16>),
        (0x3148 => pub d27ir: MmioReadWrite<u16>),
        (0x314a => _reserved21),
        (0x314c => pub d26ir: MmioReadWrite<u16>),
        (0x314e => _reserved22),
        (0x3150 => pub d25ir: MmioReadWrite<u16>),
        (0x3152 => _reserved23),
        (0x31ff => pub oic: MmioReadWrite<u8, OIC_REG::Register>),
        (0x3200 => _reserved24),
        (0x3400 => pub bios_cntl: MmioReadWrite<u32, BIOS_CNTL_REG::Register>),
        (0x3404 => pub hptc: MmioReadWrite<u32, HPTC::Register>),
        (0x3408 => _reserved25),
        (0x3410 => pub gcs: MmioReadWrite<u32, GCS_REG::Register>),
        (0x3414 => _reserved26),
        (0x3418 => pub fd: MmioReadWrite<u32, FD::Register>),
        (0x341c => pub cg: MmioReadWrite<u32, CG::Register>),
        (0x3420 => pub fdsw: MmioReadWrite<u32, FDSW::Register>),
        (0x3424 => _reserved27),
        (0x3430 => pub cir8: MmioReadWrite<u32, CIR8_REG::Register>),
        (0x3434 => _reserved28),
        (0x350c => pub cir9: MmioReadWrite<u32, CIR9_REG::Register>),
        (0x3510 => _reserved29),
        (0x352c => pub cir10: MmioReadWrite<u32, CIR10_REG::Register>),
        (0x3530 => _reserved30),
        (0x35f0 => pub map: MmioReadWrite<u32>),
        (0x35f4 => _reserved31),
        (0x38c0 => pub spi_prefetch: MmioReadWrite<u32, SPI_PREFETCH_REG::Register>),
        (0x38c4 => @END),
    }
}

const HPET_BASE: usize = 0xfed0_0000;
const SATA_ABAR_BASE: usize = 0xfea0_0000;
const GPE0_STS_ICH8: u16 = 0x20;
const GPE0_EN_ICH8: u16 = 0x28;
const SLP_TYP_S3: u32 = 0x1400;
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
    fn regs(&self) -> &'static RcbaRegs {
        // SAFETY: RCBA has been programmed and enabled in LPC PCI config.
        unsafe { &*(self.base as *const RcbaRegs) }
    }
}

/// Intel ICH8 southbridge driver.
pub struct IntelIch8 {
    config: &'static IntelIch8Config,
    smbus: Option<I801SmBus>,
    pm: PmIo,
}

// SAFETY: firmware performs chipset init on the BSP before concurrency exists.
unsafe impl Send for IntelIch8 {}
// SAFETY: the struct contains only config, optional bus state, and fixed I/O bases.
unsafe impl Sync for IntelIch8 {}

impl IntelIch8 {
    fn lpc(&self) -> ecam::EcamDevice {
        ecam::EcamDevice::new(0, ich8::LPC_DEV, ich8::LPC_FUNC)
    }

    fn lpc_regs(&self) -> &'static Ich8LpcPciConfig {
        let lpc = self.lpc();

        // SAFETY: the ICH8 LPC bridge is a fixed chipset device at 00:1f.0,
        // ECAM is initialized by the platform before this driver runs, and the
        // overlay contains the standard Type 0 header plus documented LPC
        // registers. Exact-write/W1C/BAR sequences still use raw helpers.
        unsafe { lpc.regs::<Ich8LpcPciConfig>() }
    }

    fn type0_regs(dev: ecam::EcamDevice) -> &'static PciType0Config {
        // SAFETY: callers use this for present Type 0 devices and avoid typed
        // access for BAR sizing, W1C status, and exact-write errata sequences.
        unsafe { dev.regs::<PciType0Config>() }
    }

    fn type1_regs(dev: ecam::EcamDevice) -> &'static PciType1Config {
        // SAFETY: callers use this for present Type 1 bridge devices and avoid
        // typed access for W1C status and exact-write errata sequences.
        unsafe { dev.regs::<PciType1Config>() }
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
        let lpc = self.lpc_regs();
        // RCBA is a fixed southbridge BAR; keep this as an exact raw write.
        self.lpc()
            .write32(ich8::RCBA, (self.config.rcba as u32 & 0xffff_c000) | 1);
        lpc.pmbase.set((ich8::DEFAULT_PMBASE as u32) | 1);
        lpc.acpi_cntl.set(0x80);
        lpc.gpio_base.set(ich8::DEFAULT_GPIOBASE as u32);
        lpc.gpio_cntl.modify(GPIO_CNTL_REG::GPIO_EN::SET);
    }

    fn program_lpc_decode(&self) {
        let lpc = self.lpc_regs();
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

        lpc.serirq_cntl.set(0xd0);
        lpc.lpc_io_dec.set(self.config.lpc_decode.fixed_io.encode());
        lpc.lpc_en.set(lpc_en);
        for (idx, value) in generic.iter().copied().enumerate() {
            lpc.gen_dec[idx].set(value);
        }
    }

    fn reset_watchdog_and_cmos(&self) {
        // Disable PCI interrupts.
        self.lpc_regs()
            .command
            .modify(PCI_COMMAND_BITS::INT_DISABLE::SET);

        let rcba = self.rcba();
        /*
         * Enable upper 128 bytes of CMOS (RCBA offset 0x3400).
         * Bit 2 enables the extended CMOS range.
         */
        rcba.regs().bios_cntl.set(1 << 2);
        rcba.regs().gcs.modify(GCS_REG::NO_REBOOT::SET);

        #[cfg(target_arch = "x86_64")]
        {
            let tco = self.pm().tco();
            // Halt TCO timer.
            let cnt = tco.read16(pmio::TCO1_CNT);
            tco.write16(pmio::TCO1_CNT, cnt | (1 << 11));
            // Clear timeout status.
            tco.write16(pmio::TCO1_STS, 1 << 3);
            tco.write16(pmio::TCO2_STS, 1 << 1);
        }
    }

    fn setup_gpios(&self) {
        let gpio = IchGpio::new(ich8::DEFAULT_GPIOBASE);
        gpio.setup(&self.config.gpio);
    }

    fn enable_smbus(&mut self) {
        let smbus_pci = ecam::EcamDevice::new(0, ich8::SMBUS_DEV, ich8::SMBUS_FUNC);
        smbus_pci.and16(0x80, !((1 << 8) | (1 << 10) | (1 << 12) | (1 << 14)));
        let smbus =
            I801SmBus::enable_on_i801(0, ich8::SMBUS_DEV, ich8::SMBUS_FUNC, self.config.smbus_base);
        self.smbus = Some(smbus);
    }

    fn enable_hpet(&self) {
        let rcba = self.rcba();
        rcba.regs()
            .hptc
            .modify(HPTC::ENABLE::SET + HPTC::ADDRESS_SELECT.val(0));
        let _ = rcba.regs().hptc.get();

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
        rcba.regs().v1cap.modify(VCAP::REFERENCE_CLOCK.val(0x12));
        rcba.regs().cir1.set(0x0010_9000);
        rcba.regs().cir3.set(0x060b);
        rcba.regs().cir2.set(0x8600_0040);
        rcba.regs().cir4.set(0x0000_2008);
        rcba.regs().bcr.set(0x45);
        rcba.regs().cir6.modify(CIR6_REG::BIT7::CLEAR);

        rcba.regs().v1ctl.modify(VCTL::ID.val(1));
        rcba.regs().v1ctl.modify(VCTL::TC_MAP.val(0x40));
        rcba.regs().v0ctl.modify(VCTL::TC_MAP.val(0));
        rcba.regs().v1ctl.modify(VCTL::VC_ARB_SELECT.val(4));
        for (i, val) in VC1_PAT.iter().enumerate() {
            rcba.regs().pat[i].set(*val);
        }
        rcba.regs().v1ctl.modify(VCTL::VC_NEGOTIATION_PENDING::SET);
        rcba.regs().v1ctl.modify(VCTL::ENABLE::SET);

        rcba.regs().esd[2].set(2);
        rcba.regs().uld[3].set(1);
        rcba.regs().uld[2].set(1);
        rcba.regs()
            .ulba
            .set(self.config.dmibar as u32 & 0xffff_f000);

        // Mobile ICH8-M/HX path: enable DMI mobile power savings, then
        // advertise and enable L0s+L1.
        let mut dmc = rcba.regs().dmc.get();
        dmc = (dmc & !(3 << 10)) | (1 << 10);
        rcba.regs().dmc.set(dmc);
        rcba.regs().dmc.modify(DMC::MOBILE_POWER_SAVINGS::SET);
        rcba.regs().lcap.modify(LCAP_REG::ASPM_SUPPORT.val(3));
        rcba.regs().lctl.modify(LCTL_REG::ASPM_CONTROL.val(3));
    }

    fn poll_vc1(&self) {
        let rcba = self.rcba();
        let mut timeout = 0x7ffff;
        while (rcba.regs().v1sts.get() & (1 << 1)) != 0 && timeout != 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            fstart_log::error!("intel-ich8: VC1 negotiation timeout");
        }

        if ((rcba.regs().lsts.get() >> 4) & 0x3f) == 2 {
            rcba.regs().cir6.modify(CIR6_REG::FIELD_23_21.val(3));
            rcba.regs().cir_20c4.modify(CIR_20C4_REG::BIT15::SET);
            rcba.regs().cir_20e4.modify(CIR_20E4_REG::BIT15::SET);
        }

        timeout = 0x7ffff;
        while (rcba.regs().v1sts.get() & 1) != 0 && timeout != 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            fstart_log::error!("intel-ich8: VC1 arbitration-table update timeout");
        }
    }

    fn configure_power_options(&self) {
        let lpc = self.lpc_regs();
        // Match coreboot/vendor BIOS: enable USB transient-disconnect detect
        // (D31:F0 0xad bits [1:0]) and global reset on CF9 writes.
        lpc.pmir
            .modify(PMIR_REG::USB_TRANSIENT_DISCONNECT.val(3) + PMIR_REG::CF9GR::SET);

        lpc.gen_pmcon_3.modify(
            GEN_PMCON_3_REG::STATE_AFTER_G3.val(if self.config.power_on_after_fail == 0 {
                1
            } else {
                0
            }) + GEN_PMCON_3_REG::SLP_S3_STRETCH.val(3)
                + GEN_PMCON_3_REG::MIN_SLP_S4_ASSERT::CLEAR,
        );

        let gen_pmcon_1 = GEN_PMCON_1_REG::AFTERG3_EN.val(0)
            + GEN_PMCON_1_REG::SLP_S4_ASST_EN::SET
            + GEN_PMCON_1_REG::SUS_PWR_FLR::SET
            + GEN_PMCON_1_REG::DIS_SLP_X_STRCH_SUS_UP::SET
            + GEN_PMCON_1_REG::BIOS_PCI_EXP_EN::SET;
        let gen_pmcon_1 = match (self.config.c4_on_c3, self.config.c5_enable) {
            (true, true) => {
                gen_pmcon_1 + GEN_PMCON_1_REG::C4_ON_C3_EN::SET + GEN_PMCON_1_REG::C5_EN::SET
            }
            (true, false) => gen_pmcon_1 + GEN_PMCON_1_REG::C4_ON_C3_EN::SET,
            (false, true) => gen_pmcon_1 + GEN_PMCON_1_REG::C5_EN::SET,
            (false, false) => gen_pmcon_1,
        };
        lpc.gen_pmcon_1.modify(gen_pmcon_1);

        if self.config.c5_enable {
            if self.config.c6_enable {
                lpc.c5_exit_timing.modify(
                    C5_EXIT_TIMING_REG::C6_EXIT.val(5) + C5_EXIT_TIMING_REG::C5_EXIT.val(3),
                );
            } else {
                lpc.c5_exit_timing.modify(
                    C5_EXIT_TIMING_REG::C6_EXIT.val(0) + C5_EXIT_TIMING_REG::C5_EXIT.val(1),
                );
            }
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
            let mut nmi = fstart_pio::inb(0x70);
            nmi |= 1 << 7;
            fstart_pio::outb(0x70, nmi);
        }
    }

    fn configure_cstates(&self) {
        let lpc = self.lpc_regs();
        lpc.cxstate_cnf.modify(
            CXSTATE_CNF_REG::C3_POPUP_ENABLE::SET
                + CXSTATE_CNF_REG::BM_STS_ZERO_ENABLE::SET
                + CXSTATE_CNF_REG::C3_POPDOWN_ENABLE::SET,
        );
        lpc.c4timing_cnt.modify(C4TIMING_CNT_REG::VALUE.val(0x0a));
    }

    fn enable_clock_gating(&self) {
        let rcba = self.rcba();
        rcba.regs().dmic.modify(DMIC::VIRTUAL_CHANNEL_ENABLE.val(3));
        let mut cg = rcba.regs().cg.get();
        cg |= (1 << 31) | (1 << 29) | (1 << 28);
        cg |= (1 << 27) | (1 << 26) | (1 << 25) | (1 << 24);
        cg |= (1 << 23) | (1 << 22);
        cg &= !(1 << 21);
        cg &= !(1 << 20);
        cg |= (1 << 19) | (1 << 18) | (1 << 17) | (1 << 16);
        cg |= (1 << 4) | (1 << 3) | (1 << 2) | (1 << 1) | 1;
        rcba.regs().cg.set(cg);
        rcba.regs()
            .spi_prefetch
            .modify(SPI_PREFETCH_REG::ENABLE.val(7));
    }

    fn enable_ioapic(&self) {
        let rcba = self.rcba();
        // Keep APIC range select at zero; coreboot writes this byte exactly.
        rcba.regs().oic.set(0x03);
        let _ = rcba.regs().oic.get();
    }

    fn rtc_init_status(&self) {
        let lpc = self.lpc_regs();
        if lpc.gen_pmcon_3.is_set(GEN_PMCON_3_REG::RTC_BATTERY_DEAD) {
            lpc.gen_pmcon_3
                .modify(GEN_PMCON_3_REG::RTC_BATTERY_DEAD::CLEAR);
            fstart_log::info!("intel-ich8: RTC battery-dead flag was set");
        }
    }

    fn ramstage_lpc_init(&self) {
        let rcba = self.rcba();
        self.enable_ioapic();
        self.lpc_regs().serirq_cntl.set(0xd0);
        self.configure_power_options();
        self.configure_cstates();
        self.rtc_init_status();
        self.isa_dma_init();
        self.i8259_init();
        self.enable_hpet();
        rcba.regs()
            .bios_cntl
            .modify(BIOS_CNTL_REG::EXTENDED_CMOS_ENABLE::SET);
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
        rcba.regs().gcs.modify(GCS_REG::BOOT_SMI_EN::SET);
        rcba.regs().cir8.modify(CIR8_REG::FIELD_1_0.val(2));
        rcba.regs().cir9.modify(CIR9_REG::FIELD_27_26.val(2));
        rcba.regs().cir7.modify(CIR7_REG::FIELD_19_16.val(5));
        rcba.regs().cir13.modify(CIR13_REG::FIELD_19_16.val(5));
        rcba.regs().cir5.modify(CIR5_REG::BIT0::SET);
        rcba.regs().cir10.modify(CIR10_REG::FIELD_17_16.val(3));
    }

    fn configure_gpi_routing(&self) {
        let mut value = 0u32;
        for (idx, route) in self.config.gpi_routing.iter().enumerate() {
            value |= ((*route as u32) & 0x03) << (idx * 2);
        }
        self.lpc_regs().gpio_rout.set(value);
    }

    fn sata_indexed_write32(&self, idx: u8, val: u32) {
        let sata = ecam::EcamDevice::new(0, ich8::SATA_DEV, ich8::SATA_FUNC);
        sata.write8(ich8::SATA_SIDX, idx);
        sata.write32(ich8::SATA_SDAT, val);
    }

    fn sata_indexed_rmw32(&self, idx: u8, clear: u32, set: u32) {
        let sata = ecam::EcamDevice::new(0, ich8::SATA_DEV, ich8::SATA_FUNC);
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
        let sata = ecam::EcamDevice::new(0, ich8::SATA_DEV, ich8::SATA_FUNC);
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
        let sata = ecam::EcamDevice::new(0, ich8::SATA_DEV, ich8::SATA_FUNC);
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
        let sata_regs = Self::type0_regs(sata);
        sata_regs.command.modify(
            PCI_COMMAND_BITS::IO_SPACE::SET
                + PCI_COMMAND_BITS::MEMORY_SPACE::SET
                + PCI_COMMAND_BITS::BUS_MASTER::SET,
        );
        match config.mode {
            SataMode::Ahci => {
                sata.write8(ich8::SATA_MAP, 0x60);
                sata.write32(ich8::PCI_BAR5, SATA_ABAR_BASE as u32);
            }
            SataMode::Ide => {
                sata.write8(ich8::SATA_MAP, 0);
                // Prog IF is modeled read-only in the generic header overlay,
                // so keep this exact raw programming write.
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

    fn ehci_init_controller(&self, dev: u8, func: u8) {
        let ehci = ecam::EcamDevice::new(0, dev, func);
        if ehci.read16(0) == 0xffff {
            return;
        }

        // Match coreboot's current ICH8 EHCI init: do not assign a temporary
        // BAR or reset the controller here. Resource assignment owns BAR0;
        // ramstage only enables bus mastering and sets Intel EHCIIR bits.
        Self::type0_regs(ehci)
            .command
            .modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
        ehci.or32(ich8::EHCI_INTEL_FCREG, (1 << 29) | (1 << 17));
    }

    fn usb_init(&self) {
        let Some(usb) = self.config.usb else {
            return;
        };
        if usb.ehci[0] {
            self.ehci_init_controller(ich8::EHCI1_DEV, ich8::EHCI1_FUNC);
        }
        if usb.ehci[1] {
            self.ehci_init_controller(ich8::EHCI2_DEV, ich8::EHCI2_FUNC);
        }
        const UHCI: [(u8, u8); 5] = [(0x1d, 0), (0x1d, 1), (0x1d, 2), (0x1a, 0), (0x1a, 1)];
        for (idx, (dev, func)) in UHCI.iter().copied().enumerate() {
            if !usb.uhci[idx] {
                continue;
            }
            let uhci = ecam::EcamDevice::new(0, dev, func);
            if uhci.read16(0) != 0xffff {
                Self::type0_regs(uhci)
                    .command
                    .modify(PCI_COMMAND_BITS::IO_SPACE::SET + PCI_COMMAND_BITS::BUS_MASTER::SET);
            }
        }
        fstart_log::info!("intel-ich8: USB init complete");
    }

    fn hda_init(&self, config: &HdaConfig) {
        let hda = ecam::EcamDevice::new(0, ich8::HDA_DEV, ich8::HDA_FUNC);
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

        let hda_regs = Self::type0_regs(hda);
        let bar0 = hda_regs.bar[0].get() & !0x0f;
        if bar0 == 0 {
            fstart_log::error!("intel-ich8: HDA BAR0 is unassigned");
            return;
        }
        hda_regs
            .command
            .modify(PCI_COMMAND_BITS::MEMORY_SPACE::SET + PCI_COMMAND_BITS::BUS_MASTER::SET);

        let controller = HdaController::new(bar0 as usize);
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

    fn pcie_port_cir_init(&self) {
        for func in 0u8..6 {
            let port = ecam::EcamDevice::new(0, ich8::PCIE_DEV, func);
            if port.read16(0) == 0xffff {
                continue;
            }

            // Match coreboot i82801hx/pcie.c CIR programming: set CIR 0x300
            // bit 21 and write 0x40 to CIR 0x324 without touching other hidden
            // config bits.
            port.or32(ich8::D28FX_CIR_300, 1 << 21);
            port.write8(ich8::D28FX_CIR_324, 0x40);
        }
    }

    fn pcie_init(&self) {
        self.pcie_port_cir_init();

        for func in 0u8..6 {
            let port = ecam::EcamDevice::new(0, ich8::PCIE_DEV, func);
            if port.read16(0) == 0xffff {
                continue;
            }
            port.or32(ich8::D28FX_ASPM_MOBILE, 1);
            if (port.read32(ich8::D28FX_LCTL) & 3) == 3 {
                port.or32(ich8::D28FX_ASPM_MOBILE, 1 << 1);
            }
            let port_regs = Self::type1_regs(port);
            port_regs
                .command
                .modify(PCI_COMMAND_BITS::BUS_MASTER::SET + PCI_COMMAND_BITS::SERR_ENABLE::SET);
            port_regs.cache_line_size.set(0x10);
            port_regs
                .bridge_control
                .set(port_regs.bridge_control.get() & !1u16);
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
        let fd = self.rcba().regs().fd.get();
        for func in (0usize..6).rev() {
            if (fd & Self::pcie_fd_bit(func)) == 0 {
                break;
            }
            let port = ecam::EcamDevice::new(0, ich8::PCIE_DEV, func as u8);
            if port.read16(0) != 0xffff {
                port.or32(ich8::D28FX_CIR_300, 0x3 << 16);
            }
        }
        let rcba = self.rcba();
        let mut rpfn = rcba.regs().rpfn.get();
        for func in 0usize..6 {
            if (fd & Self::pcie_fd_bit(func)) != 0 {
                rpfn |= 1 << (func * 4 + 3);
            }
        }
        rcba.regs().rpfn.set(rpfn);
        self.pcie_slot_config();
        self.pcie_aspm_lock();
        fstart_log::info!("intel-ich8: PCIe root port init complete");
    }

    fn pcie_slot_config(&self) {
        let mut slot_number = 1u32;
        for func in 0usize..6 {
            let port = ecam::EcamDevice::new(0, ich8::PCIE_DEV, func as u8);
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
            let port = ecam::EcamDevice::new(0, ich8::PCIE_DEV, func);
            if port.read16(0) != 0xffff {
                port.write32(ich8::D28FX_LCAP, port.read32(ich8::D28FX_LCAP));
            }
        }
    }

    fn ide_init(&self, config: &IdeConfig) {
        let ide = ecam::EcamDevice::new(0, ich8::IDE_DEV, ich8::IDE_FUNC);
        if ide.read16(0) == 0xffff {
            return;
        }
        let ide_regs = Self::type0_regs(ide);
        ide_regs
            .command
            .modify(PCI_COMMAND_BITS::IO_SPACE::SET + PCI_COMMAND_BITS::BUS_MASTER::SET);
        // Prog IF is modeled read-only in the generic header overlay, so keep
        // this exact raw programming write.
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
        ide_regs.interrupt_line.set(0xff);
    }

    fn pci_bridge_init(&self) {
        let bridge = ecam::EcamDevice::new(0, ich8::PCI_BRIDGE_DEV, ich8::PCI_BRIDGE_FUNC);
        if bridge.read16(0) == 0xffff {
            return;
        }
        Self::type1_regs(bridge).interrupt_line.set(0xff);
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
        let pci = ecam::EcamDevice::new(0, dev, func);
        if pci.read16(0) != 0xffff {
            Self::type0_regs(pci).command.modify(
                PCI_COMMAND_BITS::IO_SPACE::CLEAR
                    + PCI_COMMAND_BITS::MEMORY_SPACE::CLEAR
                    + PCI_COMMAND_BITS::BUS_MASTER::CLEAR,
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
        if self
            .lpc_regs()
            .gen_pmcon_3
            .is_set(GEN_PMCON_3_REG::RTC_POWER_FAILED)
        {
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
        let lpc = self.lpc_regs();
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
        lpc.pirqa_rout.set(pirq_low);
        lpc.pirqe_rout.set(pirq_high);
    }

    fn configure_default_intmap(&self) {
        let rcba = self.rcba();
        rcba.regs().d31ip.set(0x0400_3210);
        rcba.regs().d30ip.set(0x0000_0001);
        rcba.regs().d29ip.set(0x1000_0321);
        rcba.regs().d28ip.set(0x0021_4321);
        rcba.regs().d27ip.set(0x0000_0001);
        rcba.regs().d26ip.set(0x1000_0021);
        rcba.regs().d25ip.set(0x0000_0001);

        rcba.regs().d31ir.set(0x1100);
        rcba.regs().d30ir.set(0x0000);
        rcba.regs().d29ir.set(0x0002);
        rcba.regs().d28ir.set(0x3210);
        rcba.regs().d27ir.set(0x0003);
        rcba.regs().d26ir.set(0x0003);
        rcba.regs().d25ir.set(0x0001);
        self.enable_ioapic();
    }

    fn configure_late_rcba(&self, config: &Ich8LateRcbaConfig) {
        let rcba = self.rcba();
        rcba.regs().d31ip.set(config.d31ip);
        if let Some(d30ip) = config.d30ip {
            rcba.regs().d30ip.set(d30ip);
        }
        rcba.regs().d29ip.set(config.d29ip);
        rcba.regs().d28ip.set(config.d28ip);
        rcba.regs().d27ip.set(config.d27ip);
        if let Some(d26ip) = config.d26ip {
            rcba.regs().d26ip.set(d26ip);
        }
        if let Some(d25ip) = config.d25ip {
            rcba.regs().d25ip.set(d25ip);
        }

        rcba.regs().d31ir.set(config.d31ir);
        rcba.regs().d30ir.set(config.d30ir);
        rcba.regs().d29ir.set(config.d29ir);
        rcba.regs().d28ir.set(config.d28ir);
        rcba.regs().d27ir.set(config.d27ir);
        if let Some(d26ir) = config.d26ir {
            rcba.regs().d26ir.set(d26ir);
        }
        if let Some(d25ir) = config.d25ir {
            rcba.regs().d25ir.set(d25ir);
        }

        if let Some(iotr3) = config.iotr3 {
            rcba.regs().iotr3_lo.set(iotr3.lo);
            rcba.regs().iotr3_hi.set(iotr3.hi);
        }
    }
}

impl Device for IntelIch8 {
    const NAME: &'static str = "intel-ich8";
    const COMPATIBLE: &'static [&'static str] = &["intel,ich8", "intel,ich8m", "intel,82801hx"];
    type Config = IntelIch8Config;

    fn new(config: &'static IntelIch8Config) -> Result<Self, DeviceError> {
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
            config,
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
        // Bootblock-level SPI, fixed BAR, CMOS/watchdog, LPC decode, and GPIO
        // setup was already done by pre_console_init(). Avoid replaying those
        // writes here; early_init is the raminit-era southbridge path.
        self.enable_smbus();
        self.write_pirq_routes();
        self.clear_disabled_device_commands();
        let rcba = self.rcba();
        let fd = self.function_disable_mask();
        rcba.regs().fd.set(fd);
        if self.config.disable_lan {
            rcba.regs().fdsw.modify(FDSW::LAN_DISABLE::SET);
        }
        self.early_chipset_settings();
        self.pm().write32(GPE0_STS_ICH8, 0xffff_ffff);
        self.pm().write32(GPE0_EN_ICH8, self.config.gpe0_en);
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
        rcba.regs().fdsw.modify(FDSW::FUNCTION_DISABLE_LOCK::SET);
        rcba.regs().map.set(rcba.regs().map.get());
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

    fn gpio_get(&self, pin: u32) -> Result<bool, ServiceError> {
        Ok(IchGpio::new(ich8::DEFAULT_GPIOBASE).get(pin as u8))
    }

    fn gpio_set(&self, pin: u32, value: bool) -> Result<(), ServiceError> {
        IchGpio::new(ich8::DEFAULT_GPIOBASE).set(pin as u8, value);
        Ok(())
    }

    fn ramstage_init(&mut self) -> Result<(), ServiceError> {
        PostDramInit::post_dram_init(self)
    }

    fn finalize(&mut self) -> Result<(), ServiceError> {
        FinalizeInit::finalize_init(self)
    }
}

impl fstart_superio::LpcBaseProvider for IntelIch8 {
    fn lpc_base(&self) -> u16 {
        // The X61 laptop-side NSC PC87382/DLPC uses extended PnP config port
        // 0x164e.  The dock-side PC87392 at 0x2e is board-switched and handled
        // directly by the X61 mainboard hook while disconnected from generic
        // driver init.
        0x164e
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
