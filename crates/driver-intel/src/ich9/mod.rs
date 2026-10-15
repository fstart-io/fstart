//! Intel ICH9 / ICH9-M southbridge driver.
//!
//! Supports the ICH9 and ICH9-M southbridge; board policy (including the
//! X200/R400/R500 interrupt router and EC decode) is supplied by the caller.

pub use crate::southbridge::gpio_ich::GpiRoute;
use crate::southbridge::gpio_ich::{IchGpio, encode_gpi_routes};
pub use crate::southbridge::pirq::valid_pirq_route;
use crate::southbridge::pmio_ich::{self as pmio, PmIo};
use crate::southbridge::smbus::I801SmBus;
use fstart_core::ConstVec;
use fstart_core::memory::{FlashLayout, IntelIfdFlashLayout, IntelIfdRegion};
use fstart_core::mmio::MmioReadWrite;
use fstart_core::services::{
    ConsoleDevice, FirmwareImage, FirmwareImageProvider, FlashLayoutVerifier, ServiceError, SmBus,
    Southbridge,
};
pub use fstart_driver_usbdebug::Config as EhciDebugConfig;
use fstart_pci::ecam;
use fstart_pci::{
    PCI_COMMAND_BITS, PciAddress, PciFixedBar, PciFixedBarType, PciFixedBars, PciType0Config,
    PciType1Config, pci_type0_config,
};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

pub use crate::southbridge::gpio_ich::{
    GpioConfig, GpioDir, GpioLevel, GpioMode, GpioPin, GpioReset,
};
pub use crate::southbridge::hda::{
    HdaConfig, HdaController, HdaVerbTable, PinColor, PinConfig, PinConn, PinConnector, PinDevice,
    PinGeoLoc, PinLoc,
};

/// ICH9/ICH9-M PCI config and RCBA constants.
pub mod ich9 {
    pub const LAN_DEV: u8 = 0x19;
    pub const LAN_FUNC: u8 = 0;
    pub const HDA_DEV: u8 = 0x1b;
    pub const HDA_FUNC: u8 = 0;
    pub const PCIE_DEV: u8 = 0x1c;
    pub const PCI_BRIDGE_DEV: u8 = 0x1e;
    pub const PCI_BRIDGE_FUNC: u8 = 0;
    pub const LPC_DEV: u8 = 0x1f;
    pub const LPC_FUNC: u8 = 0;
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
    pub const GPIO_ROUT: u16 = 0xb8;
    pub const RCBA: u16 = 0xf0;

    pub const SMB_BASE: u16 = 0x20;
    pub const HOSTC: u16 = 0x40;
    pub const HST_EN: u8 = 1;

    pub const DEFAULT_PMBASE: u16 = 0x0500;
    pub const DEFAULT_GPIOBASE: u16 = 0x0580;
    pub const DEFAULT_SMBUS_BASE: u16 = 0x0400;

    pub const DID_ICH9M_SATA_IDE: u16 = 0x2928;
    pub const DID_ICH9M_SATA_AHCI: u16 = 0x2929;
    pub const DID_ICH9M_LPC_AMT: u16 = 0x2917;
    pub const DID_ICH9M_LPC_BASE: u16 = 0x2919;

    bitflags::bitflags! {
        /// ICH9 Function Disable bits. Exported so board `inspect` output
        /// and tests can decode the mask the driver programs.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct FunctionDisable: u32 {
            const SAD1   = 1 << 2;
            const SD     = 1 << 3;
            const HDAD   = 1 << 4;
            const BIOS_MUST_WRITE = 1;
            const U6D    = 1 << 7;
            const U1D    = 1 << 8;
            const U2D    = 1 << 9;
            const U3D    = 1 << 10;
            const U4D    = 1 << 11;
            const U5D    = 1 << 12;
            const EHCI2D = 1 << 13;
            const EHCI1D = 1 << 15;
            const PE1D   = 1 << 16;
            const PE2D   = 1 << 17;
            const PE3D   = 1 << 18;
            const PE4D   = 1 << 19;
            const PE5D   = 1 << 20;
            const PE6D   = 1 << 21;
            const TTD    = 1 << 24;
            const SAD2   = 1 << 25;
        }
    }

    impl FunctionDisable {
        /// Indexed PCIe port bits for config-array mapping.
        pub const PE: [Self; 6] = [
            Self::PE1D,
            Self::PE2D,
            Self::PE3D,
            Self::PE4D,
            Self::PE5D,
            Self::PE6D,
        ];
    }

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
        USB_TRANSIENT_DISCONNECT OFFSET(8) NUMBITS(2) []
    ],
    /// I/O trap register low dword.
    IOTR_LO [
        TRAP_SMI_ENABLE OFFSET(0) NUMBITS(1) [],
        IO_ADDRESS_DWORD OFFSET(2) NUMBITS(14) [],
        ADDRESS_MASK OFFSET(18) NUMBITS(6) []
    ],
    /// I/O trap register high dword.
    IOTR_HI [
        BYTE_ENABLES OFFSET(0) NUMBITS(4) [],
        BYTE_ENABLE_MASK OFFSET(4) NUMBITS(4) [],
        READ_WRITE OFFSET(16) NUMBITS(1) [],
        READ_WRITE_MASK OFFSET(17) NUMBITS(1) []
    ],
];

register_bitfields! [u16,
    /// ICH9 LPC GEN_PMCON_1 (mobile and desktop bits differ).
    GEN_PMCON_1_REG [
        SMI_RATE OFFSET(0) NUMBITS(2) [],
        CLKRUN_EN OFFSET(2) NUMBITS(1) [],
        SPEEDSTEP_EN OFFSET(3) NUMBITS(1) [],
        CPU_SLP_EN OFFSET(5) NUMBITS(1) [],
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

register_structs! {
    /// Small Type 0 PCI config overlay for exact programming of the class-code
    /// Prog IF byte. The shared generic overlay intentionally exposes Prog IF
    /// as read-only for normal code; ICH9 mode switching requires an explicit
    /// chipset-driver write.
    PciType0ProgIfConfig {
        (0x00 => _vendor_id: fstart_core::mmio::MmioReadOnly<u16>),
        (0x02 => _device_id: fstart_core::mmio::MmioReadOnly<u16>),
        (0x04 => _command: fstart_core::mmio::MmioReadWrite<u16, PCI_COMMAND_BITS::Register>),
        (0x06 => _status_raw: fstart_core::mmio::MmioReadWrite<u16>),
        (0x08 => _revision_id: fstart_core::mmio::MmioReadOnly<u8>),
        (0x09 => pub prog_if: MmioReadWrite<u8>),
        (0x0a => _subclass: fstart_core::mmio::MmioReadOnly<u8>),
        (0x0b => _class_code: fstart_core::mmio::MmioReadOnly<u8>),
        (0x0c => @END),
    }
}

pci_type0_config! {
    /// ICH9 LPC bridge PCI configuration space.
    pub struct Ich9LpcPciConfig {
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
    /// ICH9 Root Complex Base Address MMIO register block.
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
        (0x1e80 => pub iotr0_lo: MmioReadWrite<u32, IOTR_LO::Register>),
        (0x1e84 => pub iotr0_hi: MmioReadWrite<u32, IOTR_HI::Register>),
        (0x1e88 => pub iotr1_lo: MmioReadWrite<u32, IOTR_LO::Register>),
        (0x1e8c => pub iotr1_hi: MmioReadWrite<u32, IOTR_HI::Register>),
        (0x1e90 => pub iotr2_lo: MmioReadWrite<u32, IOTR_LO::Register>),
        (0x1e94 => pub iotr2_hi: MmioReadWrite<u32, IOTR_HI::Register>),
        (0x1e98 => pub iotr3_lo: MmioReadWrite<u32, IOTR_LO::Register>),
        (0x1e9c => pub iotr3_hi: MmioReadWrite<u32, IOTR_HI::Register>),
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
        (0x3414 => pub buc: MmioReadWrite<u32>),
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
const ICH9_SPIBAR_OFFSET: usize = 0x3800;
const SPIBAR_HSFS: usize = 0x04;
const SPIBAR_FREG0: usize = 0x54;
const SPIBAR_FDOC: usize = 0xb0;
const SPIBAR_FDOD: usize = 0xb4;
const HSFS_FDV: u32 = 1 << 14;
const SPI_FREG_BASE_MASK: u32 = 0x1fff;
const SPI_FREG_LIMIT_MASK: u32 = 0x1fff;
const SPI_FREG_LIMIT_SHIFT: u32 = 16;
const SPI_FREG_SHIFT: u32 = 12;
const GPE0_STS_ICH9: u16 = 0x20;
const GPE0_EN_ICH9: u16 = 0x28;
const SLP_TYP_S3: u32 = 0x1400;
const LPC_EN_CNF2: u16 = 1 << 13;
const LPC_EN_CNF1: u16 = 1 << 12;
const LPC_EN_MC: u16 = 1 << 11;
const LPC_EN_KBC: u16 = 1 << 10;
const LPC_EN_GAMEH: u16 = 1 << 9;
const LPC_EN_GAMEL: u16 = 1 << 8;
const LPC_EN_FDD: u16 = 1 << 3;
const LPC_EN_LPT: u16 = 1 << 2;
const LPC_EN_COMB: u16 = 1 << 1;
const LPC_EN_COMA: u16 = 1 << 0;
const LPC_EN_COREBOOT_BASE: u16 = LPC_EN_CNF2
    | LPC_EN_CNF1
    | LPC_EN_MC
    | LPC_EN_KBC
    | LPC_EN_GAMEH
    | LPC_EN_GAMEL
    | LPC_EN_COMB
    | LPC_EN_COMA;

/// SATA configuration.
#[derive(Debug, Clone, Copy)]
pub struct SataConfig {
    pub mode: SataMode,
    pub ports: u8,
    /// AHCI hot-plug port bitmap.
    pub hotplug_map: u8,
    /// Enable the SATA clock-request path when GPIO35 indicates it is usable.
    pub clock_request: bool,
    /// Enable the mobile SATA traffic monitor when C-state popup/popdown is enabled.
    pub traffic_monitor: bool,
}

/// I/O access types matched by an ICH9 trap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoTrapAccess {
    /// Trap read cycles only.
    Read,
    /// Trap write cycles only.
    Write,
    /// Trap both read and write cycles.
    Any,
}

/// Semantic ICH9 I/O trap configuration.
#[derive(Debug, Clone, Copy)]
pub struct IoTrapConfig {
    /// I/O trap register index, 0..3.
    pub index: u8,
    /// Base I/O port. Must be dword-aligned and aligned to `size`.
    pub base: u16,
    /// Trapped range size in bytes. Must be a power-of-two dword multiple, up to 256.
    pub size: u16,
    /// Cycle type to trap.
    pub access: IoTrapAccess,
}

impl IoTrapConfig {
    fn encode(self) -> Option<(u32, u32)> {
        if self.index > 3 || self.base & 0x3 != 0 || self.size == 0 || self.size > 0x100 {
            return None;
        }
        if self.size & 0x3 != 0 || !self.size.is_power_of_two() || self.base & (self.size - 1) != 0
        {
            return None;
        }

        let address_mask = u32::from((self.size / 4) - 1);
        let io_address = u32::from(self.base) >> 2;
        let low = IOTR_LO::TRAP_SMI_ENABLE::SET.value
            | IOTR_LO::IO_ADDRESS_DWORD.val(io_address).value
            | IOTR_LO::ADDRESS_MASK.val(address_mask).value;
        let high = match self.access {
            IoTrapAccess::Write => IOTR_HI::READ_WRITE::CLEAR.value,
            IoTrapAccess::Read => IOTR_HI::READ_WRITE::SET.value,
            IoTrapAccess::Any => {
                IOTR_HI::READ_WRITE::CLEAR.value | IOTR_HI::READ_WRITE_MASK::SET.value
            }
        } | IOTR_HI::BYTE_ENABLE_MASK.val(0xf).value;

        Some((low, high))
    }
}

/// PCIe slot power-limit fields.
#[derive(Debug, Clone, Copy, Default)]
pub struct PciePowerLimit {
    /// Power-limit value encoded in PCIe Slot Capabilities.
    pub value: u8,
    /// Power-limit scale encoded in PCIe Slot Capabilities.
    pub scale: u8,
}

/// The ICH9 port-disable mask is latched with the mode, before PCI enumeration.
fn sata_map(ports: u8, ahci: bool) -> u16 {
    (if ahci { 0x60 } else { 0 }) | (u16::from((ports ^ 0x3f) & 0x3f) << 8)
}

/// SATA controller operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SataMode {
    Ide,
    Ahci,
}

/// USB controller configuration.
#[derive(Debug, Clone, Copy)]
pub struct UsbConfig {
    pub ehci: [bool; 2],
    pub uhci: [bool; 6],
}

/// Legacy serial-port decode selector in the LPC I/O decode register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Fixed legacy I/O decode selections for COM/LPT/FDC ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpcFixedIoDecode {
    /// COMA selector. COMA is enabled through `LPC_EN_ALL`.
    pub com_a: LpcSerialDecode,
    /// COMB selector. COMB is enabled through `LPC_EN_ALL`.
    pub com_b: LpcSerialDecode,
    /// Optional LPT selector.
    pub lpt: Option<LpcParallelDecode>,
    /// Optional FDC selector.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpcGenericIoDecode {
    /// I/O base address. Must be 4-byte aligned.
    pub base: u16,
    /// Window size in bytes. Must be a power of two from 4 through 256.
    pub size: u16,
}

impl LpcGenericIoDecode {
    fn encode(self) -> Option<u32> {
        crate::southbridge::lpc::encode_generic_io_window(self.base, self.size)
    }
}

/// Board-level LPC decode policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpcDecodeConfig {
    /// Fixed COM/LPT/FDC decode selector register.
    pub fixed_io: LpcFixedIoDecode,
    /// Up to four generic I/O decode windows, programmed to GEN1..GEN4.
    pub generic_io: ConstVec<LpcGenericIoDecode, 4>,
}

impl LpcDecodeConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fixed_io: LpcFixedIoDecode {
                com_a: LpcSerialDecode::Com1,
                com_b: LpcSerialDecode::Com2,
                lpt: None,
                fdd: None,
            },
            generic_io: ConstVec::new(empty_lpc_generic_io()),
        }
    }
}

impl Default for LpcDecodeConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// ICH9 southbridge configuration.
#[derive(Debug, Clone, Copy)]
pub struct IntelIch9Config {
    /// Root Complex Base Address register value.
    pub rcba: u64,
    /// Required, board-verified RCBA interrupt router programming, and the `_PRT` it implies.
    ///
    /// The driver programs `DxxIP`/`DxxIR` from this and generates the ACPI
    /// tables from it, so the GSIs an OS derives are the ones the router
    /// delivers on.
    pub pirq: Option<crate::southbridge::pirq::PirqRouting<'static>>,
    /// Keep reset/strap DxxIP/DxxIR values and derive ACPI routes by readback,
    /// as coreboot i82801ix does. Mutually exclusive with an explicit map.
    pub preserve_hardware_pirq: bool,
    /// Northbridge DMIBAR base address, used as the RCBA upstream RCRB target.
    pub dmibar: u64,
    /// PIRQ routing (A..H) for PIC mode.
    pub pirq_routing: [u8; 8],
    /// GPE0 enable bits (ICH9 low dword at PMBASE+0x28).
    pub gpe0_en: u32,
    /// GPI routing selectors for GPIO0..15 (0=no route, 1=SMI, 2=SCI).
    pub gpi_routing: [GpiRoute; 16],
    /// Alternate GPI SMI enable bits.
    pub alt_gp_smi_en: u16,
    /// Enable C4-on-C3 in GEN_PMCON_1 for mobile board power policy.
    pub c4_on_c3: bool,
    /// Enable C5/C6 PMSYNC support.
    pub c5_enable: bool,
    /// Enable C6 exit timing when C5/C6 PMSYNC support is active.
    pub c6_enable: bool,
    /// LPC fixed and generic I/O decode policy.
    pub lpc_decode: LpcDecodeConfig,
    /// Optional HD Audio verb table.
    pub hda: Option<HdaConfig>,
    /// SATA configuration.
    pub sata: Option<SataConfig>,
    /// USB configuration.
    pub usb: Option<UsbConfig>,
    /// Active early EHCI console BAR, excluded from PCI sizing/relocation.
    pub ehci_debug_bar: Option<fstart_driver_usbdebug::Config>,
    /// PCIe root ports 1..6 enabled.
    pub pcie_ports: [bool; 6],
    /// PCIe root ports implemented as slots.
    pub pcie_slots: [bool; 6],
    /// PCIe slot power limits for ports 1..6.
    pub pcie_power_limits: [PciePowerLimit; 6],
    /// PCIe ExpressCard hotplug events on ports 1..6.
    pub pcie_hotplug: [bool; 6],
    /// I/O trap registers to program.
    pub io_traps: ConstVec<IoTrapConfig, 4>,
    /// SMBus I/O base.
    pub smbus_base: u16,
    /// GPIO pad configuration.
    pub gpio: GpioConfig,
    /// ACPI device name (reserved for future ACPI device generation).
    pub acpi_name: Option<&'static str>,
    /// Board is a laptop (coreboot `SYSTEM_TYPE_LAPTOP`): mobile FADT profile.
    pub laptop: bool,
    /// After-power-failure behaviour: 0=off, 1=on, 2=last-state.
    pub power_on_after_fail: u8,
    /// Hardware throttle duty cycle (PMBASE+0x10 bits [7:5]).
    pub throttle_duty: u8,
    /// Disable the integrated LAN function through RCBA BUC.LAND.
    pub disable_lan: bool,
    /// Disable the second SATA function. ICH9-M boards commonly leave it hidden.
    pub disable_sata2: bool,
    /// Disable the desktop thermal-throttle function. ICH9-M does not expose it.
    pub disable_thermal: bool,
}

impl IntelIch9Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rcba: 0xFED1_C000,
            pirq: None,
            preserve_hardware_pirq: false,
            dmibar: 0xFED1_8000,
            pirq_routing: [0x0b, 0x0b, 0x0b, 0x0b, 0x80, 0x80, 0x80, 0x80],
            gpe0_en: 0,
            gpi_routing: [GpiRoute::Disabled; 16],
            alt_gp_smi_en: 0,
            c4_on_c3: true,
            c5_enable: false,
            c6_enable: false,
            lpc_decode: LpcDecodeConfig::new(),
            hda: None,
            sata: None,
            usb: None,
            ehci_debug_bar: None,
            pcie_ports: [true, true, true, true, true, true],
            pcie_slots: [false; 6],
            pcie_power_limits: [PciePowerLimit { value: 0, scale: 0 }; 6],
            pcie_hotplug: [false; 6],
            io_traps: ConstVec::new(empty_io_trap()),
            smbus_base: ich9::DEFAULT_SMBUS_BASE,
            gpio: GpioConfig::new(),
            acpi_name: Some("LPCB"),
            laptop: false,
            power_on_after_fail: 1,
            throttle_duty: 0,
            disable_lan: false,
            disable_sata2: true,
            disable_thermal: true,
        }
    }
}

impl Default for IntelIch9Config {
    fn default() -> Self {
        Self::new()
    }
}

#[must_use]
pub const fn valid_lpc_generic_io(range: LpcGenericIoDecode) -> bool {
    crate::southbridge::lpc::valid_generic_io_window(range.base, range.size)
}

#[must_use]
pub const fn valid_io_trap(trap: IoTrapConfig) -> bool {
    trap.index <= 3
        && trap.base & 0x3 == 0
        && trap.size != 0
        && trap.size <= 0x100
        && trap.size & 0x3 == 0
        && trap.size.is_power_of_two()
        && trap.base & (trap.size - 1) == 0
}

#[must_use]
pub const fn lpc_generic_io_overlaps(a: LpcGenericIoDecode, b: LpcGenericIoDecode) -> bool {
    let a_end = a.base as u32 + a.size as u32;
    let b_end = b.base as u32 + b.size as u32;
    (a.base as u32) < b_end && (b.base as u32) < a_end
}

const fn empty_lpc_generic_io() -> LpcGenericIoDecode {
    LpcGenericIoDecode { base: 0, size: 0 }
}

const fn empty_io_trap() -> IoTrapConfig {
    IoTrapConfig {
        index: 0,
        base: 0,
        size: 4,
        access: IoTrapAccess::Any,
    }
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

    #[inline]
    fn read32(&self, offset: usize) -> u32 {
        // SAFETY: RCBA has been programmed and enabled in LPC PCI config;
        // callers pass documented, 32-bit-aligned RCBA/SPIBAR offsets.
        unsafe { fstart_core::mmio::read32((self.base + offset) as *const u32) }
    }

    #[inline]
    fn write32(&self, offset: usize, value: u32) {
        // SAFETY: RCBA has been programmed and enabled in LPC PCI config;
        // callers pass documented, 32-bit-aligned RCBA/SPIBAR offsets.
        unsafe { fstart_core::mmio::write32((self.base + offset) as *mut u32, value) }
    }
}

/// Intel ICH9 southbridge driver.
pub struct IntelIch9 {
    config: &'static IntelIch9Config,
    smbus: Option<I801SmBus>,
    pm: PmIo,
    /// Advertise `_S3_`; set by the mainstage once resume state persisted.
    s3_enabled: bool,
    /// This boot resumes from S3; set by the mainstage.
    resume: bool,
}

// SAFETY: firmware performs chipset init on the BSP before concurrency exists.
unsafe impl Send for IntelIch9 {}
// SAFETY: the struct contains only config, optional bus state, and fixed I/O bases.
unsafe impl Sync for IntelIch9 {}

impl IntelIch9 {
    fn lpc(&self) -> ecam::EcamDevice {
        ecam::EcamDevice::new(0, ich9::LPC_DEV, ich9::LPC_FUNC)
    }

    fn lpc_regs(&self) -> &'static Ich9LpcPciConfig {
        let lpc = self.lpc();

        // SAFETY: the ICH9 LPC bridge is a fixed chipset device at 00:1f.0,
        // ECAM is initialized by the platform before this driver runs, and the
        // overlay contains the standard Type 0 header plus documented LPC
        // registers. Exact-write/W1C/BAR sequences still use raw helpers.
        unsafe { lpc.regs::<Ich9LpcPciConfig>() }
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

    fn prog_if_regs(dev: ecam::EcamDevice) -> &'static PciType0ProgIfConfig {
        // SAFETY: callers use this only for present Type 0 ICH9 functions
        // whose programming model explicitly allows changing Prog IF.
        unsafe { dev.regs::<PciType0ProgIfConfig>() }
    }

    fn rcba(&self) -> Rcba {
        Rcba::new((self.config.rcba & 0xffff_c000) as usize)
    }

    fn spi_read32(&self, offset: usize) -> u32 {
        self.rcba().read32(ICH9_SPIBAR_OFFSET + offset)
    }

    fn spi_write32(&self, offset: usize, value: u32) {
        self.rcba().write32(ICH9_SPIBAR_OFFSET + offset, value);
    }

    fn spi_descriptor_word(&self, fdoc: u32) -> u32 {
        self.spi_write32(SPIBAR_FDOC, fdoc);
        self.spi_read32(SPIBAR_FDOD)
    }

    fn spi_flash_component_size(&self) -> u32 {
        // Match coreboot southbridge/intel/common/spi.c: observe descriptor
        // component section 0 (FDOC=0x1000) and decode density fields.
        let flmap0 = self.spi_descriptor_word(4);
        let flcomp = self.spi_descriptor_word(0x1000);
        let mut size = 1u32 << (19 + (flcomp & 0x7));
        if (flmap0 >> 8) & 0x3 != 0 {
            size = size.saturating_add(1u32 << (19 + ((flcomp >> 3) & 0x7)));
        }
        size
    }

    fn spi_ifd_region(&self, index: usize) -> (u32, u32) {
        let reg = self.spi_read32(SPIBAR_FREG0 + index * core::mem::size_of::<u32>());
        let base = (reg & SPI_FREG_BASE_MASK) << SPI_FREG_SHIFT;
        let limit = ((reg >> SPI_FREG_LIMIT_SHIFT) & SPI_FREG_LIMIT_MASK) << SPI_FREG_SHIFT;
        if limit < base {
            return (0, 0);
        }
        (base, limit + (1 << SPI_FREG_SHIFT) - base)
    }

    fn verify_ifd_flash_layout(&self, expected: &IntelIfdFlashLayout) -> Result<(), ServiceError> {
        let hsfs = self.spi_read32(SPIBAR_HSFS) & 0xffff;
        if hsfs & HSFS_FDV == 0 {
            fstart_log::error!("intel-ich9: SPI descriptor valid bit is clear");
            return Err(ServiceError::HardwareError);
        }

        let runtime_size = self.spi_flash_component_size();
        if runtime_size != expected.size() {
            fstart_log::error!(
                "intel-ich9: SPI flash size mismatch: configured={:#x} runtime={:#x}",
                expected.size(),
                runtime_size
            );
            return Err(ServiceError::HardwareError);
        }

        for region in expected.regions.as_slice() {
            let Some(index) = region.kind.flreg_index() else {
                continue;
            };
            let (runtime_offset, runtime_size) = self.spi_ifd_region(index);
            if runtime_offset != region.offset || runtime_size != region.size {
                fstart_log::error!(
                    "intel-ich9: SPI {} region mismatch: configured off={:#x} size={:#x} runtime off={:#x} size={:#x}",
                    region.kind.as_str(),
                    region.offset,
                    region.size,
                    runtime_offset,
                    runtime_size
                );
                return Err(ServiceError::HardwareError);
            }
        }

        if let Some(bios) = expected.bios_region() {
            let expected_host_base = expected.base() + u64::from(bios.offset);
            let runtime_host_base = 0x1_0000_0000u64 - u64::from(bios.size);
            if expected_host_base != runtime_host_base {
                fstart_log::error!(
                    "intel-ich9: BIOS host window mismatch: configured={:#x} runtime={:#x}",
                    expected_host_base,
                    runtime_host_base
                );
                return Err(ServiceError::HardwareError);
            }
        }

        fstart_log::info!("intel-ich9: SPI descriptor matches board flash_layout");
        Ok(())
    }

    fn pm(&self) -> PmIo {
        self.pm
    }

    /// SMI routing view of this southbridge for SMM installation.
    #[must_use]
    pub const fn smi(&self) -> crate::southbridge::smi::IchSmi {
        crate::southbridge::smi::IchSmi::new(self.pm.base(), crate::southbridge::smi::ICH8_GPE0)
    }

    fn enable_spi_prefetching_and_caching(&self) {
        let lpc = self.lpc();
        // Match coreboot intel/common/early_spi.h: enable SPI prefetch/cache before
        // extended flash reads from the memory-mapped boot medium.
        let value = lpc.read8(0xdc);
        lpc.write8(0xdc, (value & !(3 << 2)) | (2 << 2));
    }

    fn program_fixed_bars(&self) {
        let lpc = self.lpc_regs();
        // RCBA is a fixed southbridge BAR; keep this as an exact raw write.
        self.lpc()
            .write32(ich9::RCBA, (self.config.rcba as u32 & 0xffff_c000) | 1);
        lpc.pmbase.set((ich9::DEFAULT_PMBASE as u32) | 1);
        lpc.acpi_cntl.set(0x80);
        lpc.gpio_base.set(ich9::DEFAULT_GPIOBASE as u32);
        lpc.gpio_cntl.modify(GPIO_CNTL_REG::GPIO_EN::SET);
    }

    fn program_lpc_decode(&self) {
        let lpc = self.lpc_regs();
        let mut generic = [0u32; 4];
        for (idx, range) in self
            .config
            .lpc_decode
            .generic_io
            .as_slice()
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
        // Coreboot lpc.c "BIOS must program" bits 30 and 9:8. CF9GR is left
        // alone: coreboot's early bit-20 write is truncated to a byte, so CF9
        // resets (raminit recovery, OS reboot) are host resets there too.
        self.lpc_regs()
            .pmir
            .modify(PMIR_REG::USB_TRANSIENT_DISCONNECT.val(3));
        self.lpc().or32(ich9::PMIR, 1 << 30);

        #[cfg(target_arch = "x86_64")]
        {
            // Coreboot early_init only acknowledges watchdog status; the TCO
            // timer keeps running and ramstage reloads it.
            let tco = self.pm().tco();
            tco.write16(pmio::TCO1_STS, 1 << 3);
            tco.write16(pmio::TCO2_STS, 1 << 1);
        }
    }

    fn setup_gpios(&self) {
        let gpio = IchGpio::new(ich9::DEFAULT_GPIOBASE);
        gpio.setup(&self.config.gpio);
    }

    fn enable_smbus(&mut self) {
        let smbus_pci = ecam::EcamDevice::new(0, ich9::SMBUS_DEV, ich9::SMBUS_FUNC);
        smbus_pci.and16(0x80, !((1 << 8) | (1 << 10) | (1 << 12) | (1 << 14)));
        let smbus =
            I801SmBus::enable_on_i801(0, ich9::SMBUS_DEV, ich9::SMBUS_FUNC, self.config.smbus_base);
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
            let cfg = fstart_core::mmio::read32((HPET_BASE + 0x10) as *const u32);
            fstart_core::mmio::write32((HPET_BASE + 0x10) as *mut u32, cfg | 1);
        }
    }

    fn is_mobile(&self) -> bool {
        matches!(
            self.lpc_regs().device_id.get(),
            ich9::DID_ICH9M_LPC_AMT | ich9::DID_ICH9M_LPC_BASE
        )
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

        // Mobile ICH9-M path: enable DMI mobile power savings, then
        // advertise and enable L0s+L1.
        let mobile = self.is_mobile();
        if mobile {
            let dmc = (rcba.regs().dmc.get() & !(3 << 10)) | (1 << 10);
            rcba.regs().dmc.set(dmc);
        }
        rcba.regs().dmc.modify(DMC::MOBILE_POWER_SAVINGS::SET);
        rcba.regs()
            .lcap
            .modify(LCAP_REG::ASPM_SUPPORT.val(if mobile { 3 } else { 1 }));
        rcba.regs()
            .lctl
            .modify(LCTL_REG::ASPM_CONTROL.val(if mobile { 3 } else { 1 }));
    }

    fn poll_vc1(&self) -> Result<(), ServiceError> {
        let rcba = self.rcba();
        let mut timeout = 0x7ffff;
        while (rcba.regs().v1sts.get() & (1 << 1)) != 0 && timeout != 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            fstart_log::error!("intel-ich9: VC1 negotiation timeout");
            return Err(ServiceError::HardwareError);
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
            fstart_log::error!("intel-ich9: VC1 arbitration-table update timeout");
            return Err(ServiceError::HardwareError);
        }
        Ok(())
    }

    fn configure_power_options(&self) {
        let lpc = self.lpc_regs();
        let after_g3 = match self.config.power_on_after_fail {
            0 => 1,                                                     // off
            1 => 0,                                                     // on
            _ => lpc.gen_pmcon_3.read(GEN_PMCON_3_REG::STATE_AFTER_G3), // keep
        };
        lpc.gen_pmcon_3.modify(
            GEN_PMCON_3_REG::STATE_AFTER_G3.val(after_g3)
                + GEN_PMCON_3_REG::SLP_S3_STRETCH.val(3)
                + GEN_PMCON_3_REG::MIN_SLP_S4_ASSERT::CLEAR,
        );

        // ICH9 GEN_PMCON_1 bits 2/3 are CLKRUN and SpeedStep on mobile;
        // bit 5 is CPU_SLP on desktop (coreboot i82801ix/lpc.c).
        let gen_pmcon_1 = GEN_PMCON_1_REG::SMI_RATE.val(0) + GEN_PMCON_1_REG::BIOS_PCI_EXP_EN::SET;
        let gen_pmcon_1 = if self.is_mobile() {
            gen_pmcon_1 + GEN_PMCON_1_REG::CLKRUN_EN::SET + GEN_PMCON_1_REG::SPEEDSTEP_EN::SET
        } else {
            gen_pmcon_1 + GEN_PMCON_1_REG::CPU_SLP_EN::SET
        };
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
        // Like coreboot lpc.c, leave GPE0_STS: on resume it holds the wake
        // source the OS reads.
        self.pm().write32(GPE0_EN_ICH9, self.config.gpe0_en);
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
            let mut port61 = fstart_core::pio::inb(0x61);
            port61 &= 0x0f;
            port61 &= !(1 << 3);
            port61 |= 1 << 2;
            fstart_core::pio::outb(0x61, port61);
            // Port 0x70 is write-only; 0x74 is its readable alias.
            let mut nmi = fstart_core::pio::inb(0x74);
            nmi |= 1 << 7;
            fstart_core::pio::outb(0x70, nmi);
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

    /// Configure the CMOS clock, restoring the build date when it lost power.
    pub fn rtc_init(&self) {
        // Sticky battery-dead flag, cleared here exactly like coreboot.
        let lpc = self.lpc_regs();
        let battery_dead = lpc.gen_pmcon_3.is_set(GEN_PMCON_3_REG::RTC_BATTERY_DEAD);
        if battery_dead {
            lpc.gen_pmcon_3
                .modify(GEN_PMCON_3_REG::RTC_BATTERY_DEAD::CLEAR);
        }

        crate::southbridge::rtc::init_clock("intel-ich9", battery_dead);
    }

    fn ramstage_lpc_init(&self) {
        let rcba = self.rcba();
        self.enable_ioapic();
        self.lpc_regs().serirq_cntl.set(0xd0);
        self.configure_power_options();
        if self.is_mobile() {
            self.configure_cstates();
        }
        // Like coreboot, not on resume: the OS owns the clock and its alarm.
        if !self.resume {
            self.rtc_init();
        }
        self.isa_dma_init();
        self.i8259_init();
        self.enable_hpet();
        rcba.regs()
            .bios_cntl
            .modify(BIOS_CNTL_REG::EXTENDED_CMOS_ENABLE::SET);
        self.enable_clock_gating();
        self.enable_acpi_pm1();
    }

    fn usb_fd_bit(dev: u8, func: u8) -> ich9::FunctionDisable {
        use ich9::FunctionDisable as Fd;
        match (dev, func) {
            (ich9::UHCI1_DEV, 0) => Fd::U1D,
            (ich9::UHCI1_DEV, 1) => Fd::U2D,
            (ich9::UHCI1_DEV, 2) => Fd::U3D,
            (ich9::EHCI1_DEV, ich9::EHCI1_FUNC) => Fd::EHCI1D,
            (ich9::UHCI2_DEV, 0) => Fd::U4D,
            (ich9::UHCI2_DEV, 1) => Fd::U5D,
            (ich9::UHCI2_DEV, 2) => Fd::U6D,
            (ich9::EHCI2_DEV, ich9::EHCI2_FUNC) => Fd::EHCI2D,
            _ => Fd::empty(),
        }
    }

    fn early_chipset_settings(&self) {
        let rcba = self.rcba();
        rcba.regs().gcs.modify(GCS_REG::BOOT_SMI_EN::SET);
        rcba.regs().cir8.modify(CIR8_REG::FIELD_1_0.val(2));
        rcba.regs().fd.modify(FD::RAW.val(rcba.regs().fd.get() | 1));
        rcba.regs().cir9.modify(CIR9_REG::FIELD_27_26.val(2));
        rcba.regs().cir7.modify(CIR7_REG::FIELD_19_16.val(5));
        rcba.regs().cir13.modify(CIR13_REG::FIELD_19_16.val(5));
        // CIR5 bit 0 is excluded by the ICH9 specification update.
        rcba.regs().cir10.modify(CIR10_REG::FIELD_17_16.val(3));
    }

    fn hide_disabled_root_ports(&self) {
        // RPFN function-hide bits are read/write-once. Preserve the hardware
        // function map, only hide root ports explicitly disabled by policy.
        let rcba = self.rcba();
        let mut rpfn = rcba.regs().rpfn.get();
        for (index, enabled) in self.config.pcie_ports.iter().enumerate() {
            if !enabled {
                rpfn |= 1 << (index * 4 + 3);
            }
        }
        rcba.regs().rpfn.set(rpfn);
    }

    fn configure_gpi_routing(&self) {
        self.lpc_regs()
            .gpio_rout
            .set(encode_gpi_routes(&self.config.gpi_routing));
    }

    fn sata_indexed_write32(&self, idx: u8, val: u32) {
        let sata = ecam::EcamDevice::new(0, ich9::SATA_DEV, ich9::SATA_FUNC);
        sata.write8(ich9::SATA_SIDX, idx);
        sata.write32(ich9::SATA_SDAT, val);
    }

    fn sata_indexed_rmw32(&self, idx: u8, clear: u32, set: u32) {
        let sata = ecam::EcamDevice::new(0, ich9::SATA_DEV, ich9::SATA_FUNC);
        sata.write8(ich9::SATA_SIDX, idx);
        let val = sata.read32(ich9::SATA_SDAT);
        sata.write32(ich9::SATA_SDAT, (val & !clear) | set);
    }

    fn sata_enable_ahci_mmap(&self, sata: &SataConfig, is_mobile: bool, abar: usize) {
        let port_mask = if is_mobile { 0x33 } else { 0x3f };
        let port_map = sata.ports & port_mask;
        // SAFETY: `abar` is the BAR5 window PCI enumeration assigned and
        // memory decoding was enabled by the caller.
        unsafe {
            let ghc = fstart_core::mmio::read32((abar + 0x04) as *const u32) | (1 << 31);
            fstart_core::mmio::write32((abar + 0x04) as *mut u32, ghc);
            let mut cap = fstart_core::mmio::read32(abar as *const u32);
            cap |= 0x0c00_6080;
            cap &= !0x0002_0060;
            fstart_core::mmio::write32(abar as *mut u32, cap);
            fstart_core::mmio::write32((abar + 0x0c) as *mut u32, port_map as u32);
            let _ = fstart_core::mmio::read32((abar + 0x0c) as *const u32);
            let _ = fstart_core::mmio::read32((abar + 0x0c) as *const u32);
            let vsp = fstart_core::mmio::read32((abar + 0xa0) as *const u32) & !1;
            fstart_core::mmio::write32((abar + 0xa0) as *mut u32, vsp);
            for port in 0..6 {
                if is_mobile && (port == 2 || port == 3) {
                    continue;
                }
                let cmd = abar + 0x118 + port * 0x80;
                let mut value = fstart_core::mmio::read32(cmd as *const u32);
                if (sata.hotplug_map & (1 << port)) != 0 {
                    value |= 1 << 18;
                }
                fstart_core::mmio::write32(cmd as *mut u32, value);
            }
        }
    }

    fn sata_program_indexed(&self, is_mobile: bool) {
        self.sata_indexed_rmw32(0x18, (7 << 6) | (7 << 3) | 7, (3 << 3) | 3);
        self.sata_indexed_write32(0x28, 0x00cc_2080);
        let sata = ecam::EcamDevice::new(0, ich9::SATA_DEV, ich9::SATA_FUNC);
        sata.write8(ich9::SATA_SIDX, 0x40);
        sata.write8(ich9::SATA_SDAT + 2, 0x22);
        sata.write8(ich9::SATA_SIDX, 0x78);
        sata.write8(ich9::SATA_SDAT + 2, 0x22);
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

    /// Select the SATA controller mode before the bus scan (coreboot
    /// `sata_enable`), so the allocator sizes the BARs the mode exposes
    /// (ABAR only exists in AHCI mode) and `sata_init` can consume them.
    fn sata_enable(&self, config: &SataConfig) {
        let sata = ecam::EcamDevice::new(0, ich9::SATA_DEV, ich9::SATA_FUNC);
        if sata.read16(0) == 0xffff {
            return;
        }
        match config.mode {
            SataMode::Ahci => sata.write16(ich9::SATA_MAP, sata_map(config.ports, true)),
            SataMode::Ide => {
                sata.write16(ich9::SATA_MAP, sata_map(config.ports, false));
                Self::prog_if_regs(sata).prog_if.set(0x8f);
            }
        }
    }

    fn sata_init(&self, config: &SataConfig) {
        let sata = ecam::EcamDevice::new(0, ich9::SATA_DEV, ich9::SATA_FUNC);
        if sata.read16(0) == 0xffff {
            return;
        }
        let sata_regs = Self::type0_regs(sata);
        let devid = sata_regs.device_id.get();
        let is_mobile = matches!(devid, ich9::DID_ICH9M_SATA_IDE | ich9::DID_ICH9M_SATA_AHCI);
        let port_mask = if is_mobile { 0x33 } else { 0x3f };
        let ports = config.ports & port_mask;
        sata_regs.command.modify(
            PCI_COMMAND_BITS::IO_SPACE::SET
                + PCI_COMMAND_BITS::MEMORY_SPACE::SET
                + PCI_COMMAND_BITS::BUS_MASTER::SET,
        );
        if matches!(config.mode, SataMode::Ide) {
            // AHCI ABAR does not decode in IDE mode (coreboot i82801ix/sata.c).
            sata_regs.bar[5].set(0);
        }
        sata.write16(ich9::SATA_IDE_TIM_PRI, 1 << 15);
        sata.write16(ich9::SATA_IDE_TIM_SEC, 1 << 15);
        let pcs_ports = if matches!(config.mode, SataMode::Ahci) {
            port_mask
        } else {
            ports
        };
        let pcs = (sata.read16(ich9::SATA_PCS) & !0x3f) | (1 << 15) | pcs_ports as u16;
        sata.write16(ich9::SATA_PCS, pcs);
        let mut sclkcg = (((!config.ports as u32) & 0x3f) << 24) | 0x193;
        #[cfg(target_arch = "x86_64")]
        if config.clock_request {
            // SAFETY: GPIOBASE is programmed before SATA init; GPIO35 is in the second bank.
            if unsafe { fstart_core::pio::inb(ich9::DEFAULT_GPIOBASE + 0x30) } & (1 << (35 - 32))
                == 0
            {
                sclkcg |= 1 << 30;
            }
        }
        sata.write32(ich9::SATA_CLK, sclkcg);
        if is_mobile
            && config.traffic_monitor
            && ((self.lpc().read8(ich9::CXSTATE_CNF) >> 3) & 3) == 3
        {
            sata.and8_or8(0x9c, !(0x1f << 2), 3 << 2);
        }
        if matches!(config.mode, SataMode::Ahci) {
            // ABAR is whatever PCI enumeration assigned to BAR5, sized in the
            // mode `sata_enable` selected before the bus scan.
            match sata.memory_bar(0x24) {
                Some(abar) => self.sata_enable_ahci_mmap(config, is_mobile, abar as usize),
                None => {
                    fstart_log::error!("intel-ich9: SATA ABAR unassigned, AHCI left unconfigured")
                }
            }
        }
        self.sata_program_indexed(is_mobile);
        fstart_log::info!("intel-ich9: SATA init complete ports={:#x}", ports as u32);
    }

    fn ehci_init_controller(&self, dev: u8, func: u8) {
        let ehci = ecam::EcamDevice::new(0, dev, func);
        if ehci.read16(0) == 0xffff {
            return;
        }

        // Match coreboot i82801ix.c EHCI init: do not assign a temporary
        // BAR or reset the controller here. Resource assignment owns BAR0;
        // ramstage only enables bus mastering and sets Intel EHCIIR bits.
        Self::type0_regs(ehci)
            .command
            .modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
        ehci.modify32(
            ich9::EHCI_INTEL_FCREG,
            !(3 << 2),
            (1 << 29) | (1 << 17) | (2 << 2),
        );
    }

    fn usb_init(&self) {
        let Some(usb) = self.config.usb else {
            return;
        };
        if usb.ehci[0] {
            self.ehci_init_controller(ich9::EHCI1_DEV, ich9::EHCI1_FUNC);
        }
        if usb.ehci[1] {
            self.ehci_init_controller(ich9::EHCI2_DEV, ich9::EHCI2_FUNC);
        }
        const UHCI: [(u8, u8); 6] = [
            (0x1d, 0),
            (0x1d, 1),
            (0x1d, 2),
            (0x1a, 0),
            (0x1a, 1),
            (0x1a, 2),
        ];
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
        fstart_log::info!("intel-ich9: USB init complete");
    }

    fn hda_init(&self, config: &HdaConfig) {
        let hda = ecam::EcamDevice::new(0, ich9::HDA_DEV, ich9::HDA_FUNC);
        if hda.read16(0) == 0xffff {
            fstart_log::info!("intel-ich9: HDA device not present");
            return;
        }

        // Coreboot i82801ix/azalia.c: ESD/link/VC setup before codec reset.
        hda.modify32(0x134, !0x00ff_0000, 2 << 16);
        hda.modify32(0x140, !0x00ff_0000, 2 << 16);
        hda.modify32(0x114, !0x0000_00ff, 1);
        hda.or8(0x44, 7);
        hda.or32(0x120, (1 << 31) | (1 << 24) | 0x80);
        hda.and8(0x4d, !(1 << 7));
        hda.write32(0x74, hda.read32(0x74));

        // MMIO base as enumeration assigned it. The HD Audio BAR is 64-bit
        // capable, so the low dword alone is not necessarily the address.
        let Some(bar0) = hda.memory_bar(0x10) else {
            fstart_log::error!("intel-ich9: HDA BAR0 is unassigned");
            return;
        };
        let bar0 = bar0 as usize;
        Self::type0_regs(hda)
            .command
            .modify(PCI_COMMAND_BITS::MEMORY_SPACE::SET + PCI_COMMAND_BITS::BUS_MASTER::SET);

        let controller = HdaController::new(bar0);
        let codec_mask = controller.detect_codecs();
        if codec_mask != 0 {
            let programmed = controller.program_verb_tables(config, codec_mask);
            fstart_log::info!(
                "intel-ich9: HDA codec_mask={:#x}, programmed={} tables",
                codec_mask as u32,
                programmed as u32,
            );
        }
    }

    fn pcie_prepare_before_disable(&self) {
        for func in 0u8..6 {
            let port = ecam::EcamDevice::new(0, ich9::PCIE_DEV, func);
            if port.read16(0) == 0xffff {
                continue;
            }

            // Match coreboot i82801ix.c CIR programming: set CIR 0x300
            // bit 21 and write 0x40 to CIR 0x324 without touching other hidden
            // config bits.
            port.or32(ich9::D28FX_CIR_300, 1 << 21);
            port.write8(ich9::D28FX_CIR_324, 0x40);
        }
        // Coreboot programs the top contiguous disabled ports before RCBA FD
        // hides them; once hidden the config registers cannot be reached.
        for func in (0usize..6).rev() {
            if self.config.pcie_ports[func] {
                break;
            }
            let port = ecam::EcamDevice::new(0, ich9::PCIE_DEV, func as u8);
            if port.read16(0) != 0xffff {
                port.or32(ich9::D28FX_CIR_300, 0x3 << 16);
            }
        }
    }

    fn pcie_init(&self) {
        for func in 0u8..6 {
            let port = ecam::EcamDevice::new(0, ich9::PCIE_DEV, func);
            if port.read16(0) == 0xffff {
                continue;
            }
            if self.is_mobile() {
                port.or32(ich9::D28FX_ASPM_MOBILE, 1);
            }
            if (port.read32(ich9::D28FX_LCTL) & 3) == 3 {
                port.or32(ich9::D28FX_ASPM_MOBILE, 1 << 1);
            }
            let port_regs = Self::type1_regs(port);
            port_regs.command.modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
            port_regs.cache_line_size.set(0x10);
            port_regs
                .bridge_control
                .set(port_regs.bridge_control.get() & !1u16);
            port.or32(ich9::D28FX_IOXAPIC, 1 << 7);
            port.or8(ich9::D28FX_BBCLKG, 0x0f);
            port.write32(
                ich9::D28FX_VC0RCTL,
                (port.read32(ich9::D28FX_VC0RCTL) & !0xff) | 1,
            );
            port.or32(ich9::D28FX_CTTOMASK, 1 << 14);
            port.write32(ich9::D28FX_CEMASK, port.read32(ich9::D28FX_CEMASK));
            port_regs.status_raw.set(port_regs.status_raw.get());
            port_regs
                .secondary_status_raw
                .set(port_regs.secondary_status_raw.get());
            if self.config.pcie_hotplug[func as usize] {
                port.or32(ich9::D28FX_IOXAPIC, 1 << 30);
                port.write16(ich9::D28FX_XCAP, 0x142);
            }
        }
        // RPFN hide bits were programmed before PCI enumeration, before its
        // read/write-once function map could be locked by downstream accesses.
        self.pcie_slot_config();
        self.pcie_aspm_lock();
        fstart_log::info!("intel-ich9: PCIe root port init complete");
    }

    fn pcie_slot_config(&self) {
        let mut slot_number = 1u32;
        for func in 0usize..6 {
            let port = ecam::EcamDevice::new(0, ich9::PCIE_DEV, func as u8);
            if port.read16(0) == 0xffff {
                continue;
            }
            if self.config.pcie_slots[func] || self.config.pcie_hotplug[func] {
                port.or32(ich9::D28FX_XCAP, ich9::D28FX_XCAP_SLOT);
                let limit = self.config.pcie_power_limits[func];
                let mut slcap = port.read32(ich9::D28FX_SLCAP);
                slcap &= !(0x1fff << ich9::D28_SLCAP_SLOTNUM_SHIFT);
                slcap |= slot_number << ich9::D28_SLCAP_SLOTNUM_SHIFT;
                slcap &= !(0x03 << ich9::D28_SLCAP_SCALE_SHIFT);
                slcap |= (u32::from(limit.scale) & 0x03) << ich9::D28_SLCAP_SCALE_SHIFT;
                slcap &= !(0xff << ich9::D28_SLCAP_POWER_SHIFT);
                slcap |= u32::from(limit.value) << ich9::D28_SLCAP_POWER_SHIFT;
                port.write32(ich9::D28FX_SLCAP, slcap);
                slot_number += 1;
            } else {
                port.and32(ich9::D28FX_XCAP, !ich9::D28FX_XCAP_SLOT);
            }
        }
    }

    fn pcie_aspm_lock(&self) {
        for func in 0u8..6 {
            let port = ecam::EcamDevice::new(0, ich9::PCIE_DEV, func);
            if port.read16(0) != 0xffff {
                port.write32(ich9::D28FX_LCAP, port.read32(ich9::D28FX_LCAP));
            }
        }
    }

    fn pci_bridge_init(&self) {
        let bridge = ecam::EcamDevice::new(0, ich9::PCI_BRIDGE_DEV, ich9::PCI_BRIDGE_FUNC);
        if bridge.read16(0) == 0xffff {
            return;
        }
        let bridge_regs = Self::type1_regs(bridge);
        bridge_regs.interrupt_line.set(0xff);
        bridge.and8_or8(ich9::D30F0_SMLT, 0x07, 0x04 << 3);
        bridge_regs.status_raw.set(bridge_regs.status_raw.get());
        bridge_regs
            .secondary_status_raw
            .set(bridge_regs.secondary_status_raw.get());
    }

    fn isa_dma_init(&self) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            fstart_core::pio::outb(0x0d, 0x00);
            fstart_core::pio::outb(0x0b, 0x40);
            fstart_core::pio::outb(0x0b, 0x41);
            fstart_core::pio::outb(0x0b, 0x42);
            fstart_core::pio::outb(0x0b, 0x43);
            fstart_core::pio::outb(0xda, 0x00);
            fstart_core::pio::outb(0xd6, 0xc0);
            fstart_core::pio::outb(0xd6, 0x41);
            fstart_core::pio::outb(0xd6, 0x42);
            fstart_core::pio::outb(0xd6, 0x43);
            fstart_core::pio::outb(0xd4, 0x00);
            fstart_core::pio::outb(0x0f, 0x0f);
            let _ = fstart_core::pio::inb(0x80);
        }
    }

    fn i8259_init(&self) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            fstart_core::pio::outb(0x20, 0x11);
            fstart_core::pio::outb(0xa0, 0x11);
            fstart_core::pio::outb(0x21, 0x20);
            fstart_core::pio::outb(0xa1, 0x28);
            fstart_core::pio::outb(0x21, 0x04);
            fstart_core::pio::outb(0xa1, 0x02);
            fstart_core::pio::outb(0x21, 0x01);
            fstart_core::pio::outb(0xa1, 0x01);
            fstart_core::pio::outb(0x21, 0xff);
            fstart_core::pio::outb(0xa1, 0xff);
            let elcr2 = fstart_core::pio::inb(0x4d1);
            fstart_core::pio::outb(0x4d1, elcr2 | (1 << 1));
        }
    }

    fn enable_acpi_pm1(&self) {
        // SMM installation clears SCI_EN on both paths. The platform restores
        // it after installation, immediately before returning to the OS on S3.
        let pm1 =
            (self.pm().read32(pmio::PM1_CNT) & !(pmio::SLP_TYP_MASK | pmio::SCI_EN)) | pmio::BM_RLD;
        self.pm().write32(pmio::PM1_CNT, pm1);
    }

    fn function_disable_mask(&self) -> ich9::FunctionDisable {
        use ich9::FunctionDisable as Fd;
        let mut fd = Fd::empty();
        fd.set(Fd::HDAD, self.config.hda.is_none());
        fd.set(Fd::SAD1, self.config.sata.is_none());
        fd.set(Fd::SAD2, self.config.disable_sata2);
        fd.set(Fd::TTD, self.config.disable_thermal);
        match self.config.usb {
            Some(usb) => {
                const UHCI: [(u8, u8); 6] = [
                    (0x1d, 0),
                    (0x1d, 1),
                    (0x1d, 2),
                    (0x1a, 0),
                    (0x1a, 1),
                    (0x1a, 2),
                ];
                for (idx, (dev, func)) in UHCI.iter().copied().enumerate() {
                    if !usb.uhci[idx] {
                        fd |= Self::usb_fd_bit(dev, func);
                    }
                }
                fd.set(Fd::EHCI1D, !usb.ehci[0]);
                fd.set(Fd::EHCI2D, !usb.ehci[1]);
            }
            None => {
                fd |= Fd::U1D
                    | Fd::U2D
                    | Fd::U3D
                    | Fd::U4D
                    | Fd::U5D
                    | Fd::U6D
                    | Fd::EHCI1D
                    | Fd::EHCI2D;
            }
        }
        for (idx, enabled) in self.config.pcie_ports.iter().enumerate() {
            if !*enabled {
                fd |= Fd::PE[idx];
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
            Self::clear_pci_command(ich9::LAN_DEV, ich9::LAN_FUNC);
        }
        if self.config.hda.is_none() {
            Self::clear_pci_command(ich9::HDA_DEV, ich9::HDA_FUNC);
        }
        if self.config.sata.is_none() {
            Self::clear_pci_command(ich9::SATA_DEV, ich9::SATA_FUNC);
        }
        if self.config.disable_sata2 {
            Self::clear_pci_command(ich9::SATA2_DEV, ich9::SATA2_FUNC);
        }
        if self.config.disable_thermal {
            Self::clear_pci_command(ich9::THERMAL_DEV, ich9::THERMAL_FUNC);
        }
        if let Some(usb) = self.config.usb {
            const UHCI: [(u8, u8); 6] = [
                (0x1d, 0),
                (0x1d, 1),
                (0x1d, 2),
                (0x1a, 0),
                (0x1a, 1),
                (0x1a, 2),
            ];
            for (idx, (dev, func)) in UHCI.iter().copied().enumerate() {
                if !usb.uhci[idx] {
                    Self::clear_pci_command(dev, func);
                }
            }
            if !usb.ehci[0] {
                Self::clear_pci_command(ich9::EHCI1_DEV, ich9::EHCI1_FUNC);
            }
            if !usb.ehci[1] {
                Self::clear_pci_command(ich9::EHCI2_DEV, ich9::EHCI2_FUNC);
            }
        }
        for (idx, enabled) in self.config.pcie_ports.iter().enumerate() {
            if !*enabled {
                Self::clear_pci_command(ich9::PCIE_DEV, idx as u8);
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
        // Coreboot platform_is_resuming(): SLP_TYP alone survives a power
        // loss in S3, so require an actual wake.
        if self.pm().read16(pmio::PM1_STS) & pmio::WAK_STS == 0 {
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

    /// Coreboot `i82801ix_pirq_init()`: every function's INTERRUPT_LINE takes
    /// the PIC route of its own INTx pin (PIRQA..D, unswizzled), for legacy
    /// PIC-mode payloads. APIC-mode OS routing comes from `_PRT`.
    fn write_interrupt_lines(&self) {
        let functions =
            |bus: u8| (0u8..32).flat_map(move |dev| (0u8..8).map(move |func| (bus, dev, func)));
        let present = |(bus, dev, func): (u8, u8, u8)| {
            let pci = ecam::EcamDevice::new(bus, dev, func);
            (pci.read16(0) != 0xffff).then_some(pci)
        };
        let last_bus = functions(0)
            .filter_map(present)
            .filter(|pci| pci.read8(0x0e) & 0x7f == 1)
            .map(|bridge| bridge.read8(0x1a))
            .max()
            .unwrap_or(0);
        for pci in (0..=last_bus).flat_map(functions).filter_map(present) {
            let line = match pci.read8(0x3d) {
                pin @ 1..=4 => self.config.pirq_routing[usize::from(pin - 1)],
                _ => 0,
            };
            if line != 0 {
                pci.write8(0x3c, line);
            }
        }
    }

    fn configure_io_traps(&self) {
        let rcba = self.rcba();
        for trap in self.config.io_traps.as_slice().iter().copied() {
            let Some((low, high)) = trap.encode() else {
                continue;
            };
            match trap.index {
                0 => {
                    rcba.regs().iotr0_lo.set(low);
                    rcba.regs().iotr0_hi.set(high);
                }
                1 => {
                    rcba.regs().iotr1_lo.set(low);
                    rcba.regs().iotr1_hi.set(high);
                }
                2 => {
                    rcba.regs().iotr2_lo.set(low);
                    rcba.regs().iotr2_hi.set(high);
                }
                3 => {
                    rcba.regs().iotr3_lo.set(low);
                    rcba.regs().iotr3_hi.set(high);
                }
                _ => {}
            }
        }
    }

    fn configure_default_intmap(&self) {
        let rcba = self.rcba();
        if self.config.preserve_hardware_pirq {
            self.enable_ioapic();
            return;
        }
        let routing = self.config.pirq.as_ref().expect("ICH9 routing validated");
        rcba.regs().d31ip.set(routing.ip_value(0x1f));
        rcba.regs().d30ip.set(routing.ip_value(0x1e));
        rcba.regs().d29ip.set(routing.ip_value(0x1d));
        rcba.regs().d28ip.set(routing.ip_value(0x1c));
        rcba.regs().d27ip.set(routing.ip_value(0x1b));
        rcba.regs().d26ip.set(routing.ip_value(0x1a));
        rcba.regs().d25ip.set(routing.ip_value(0x19));

        rcba.regs().d31ir.set(routing.ir_value(0x1f));
        rcba.regs().d30ir.set(routing.ir_value(0x1e));
        rcba.regs().d29ir.set(routing.ir_value(0x1d));
        rcba.regs().d28ir.set(routing.ir_value(0x1c));
        rcba.regs().d27ir.set(routing.ir_value(0x1b));
        rcba.regs().d26ir.set(routing.ir_value(0x1a));
        rcba.regs().d25ir.set(routing.ir_value(0x19));
        self.enable_ioapic();
    }
}

impl crate::southbridge::smi::SmiControl for IntelIch9 {
    type EnableState = crate::southbridge::smi::SmiEnableState;
    type HandlerConfig = crate::southbridge::smi::IchSmmConfig;

    fn smm_handler_config(&self) -> Self::HandlerConfig {
        self.smi().smm_handler_config()
    }
    fn set_acpi_mode(&self, enabled: bool) {
        self.smi().set_acpi_mode(enabled);
    }
    fn quiesce_for_relocation(&self) -> Self::EnableState {
        self.smi().quiesce_for_relocation()
    }
    fn enable_permanent_smi(&self, previous: Self::EnableState) {
        self.smi().enable_permanent_smi(previous);
    }
    fn lock_permanent_smi(&self) -> bool {
        self.smi().lock_permanent_smi()
    }
}

impl crate::IntelSouthbridgeDriver for IntelIch9 {
    type Config = IntelIch9Config;

    fn new_from_config(config: &'static Self::Config) -> Result<Self, ServiceError> {
        let routing = config.pirq.as_ref();
        if config
            .pirq_routing
            .iter()
            .any(|route| !valid_pirq_route(*route))
        {
            return Err(ServiceError::InvalidParam);
        }
        if config.preserve_hardware_pirq == routing.is_some()
            || routing.is_some_and(|r| r.slot_pins.is_empty() || r.pin_routes.is_empty())
        {
            return Err(ServiceError::InvalidParam);
        }
        if let Some(debug) = config.ehci_debug_bar {
            fstart_driver_usbdebug::EhciDebug::new(debug)?;
            let bar = debug.fixed_bar();
            let controller = match (
                bar.address.bus(),
                bar.address.device(),
                bar.address.function(),
            ) {
                (0, ich9::EHCI1_DEV, ich9::EHCI1_FUNC) => 0,
                (0, ich9::EHCI2_DEV, ich9::EHCI2_FUNC) => 1,
                _ => return Err(ServiceError::InvalidParam),
            };
            if bar.address.segment() != 0
                || bar.register != 0x10
                || bar.kind != PciFixedBarType::Memory32
                || bar.prefetchable
                || bar.size != 0x1000
                || bar.base == 0
                || bar.base > 0xffff_f000
                || bar.base & 0xfff != 0
                || !config.usb.is_some_and(|usb| usb.ehci[controller])
                || [
                    (config.rcba, 0x4000),
                    (config.dmibar, 0x1000),
                    (HPET_BASE as u64, 0x400),
                    (0xfec0_0000, 0x1000), // I/O APIC
                    (0xfee0_0000, 0x1000), // local APIC
                    (debug.ecam_base as u64, 256 << 20),
                    (0xff00_0000, 16 << 20), // firmware flash decode
                ]
                .iter()
                .any(|&(base, size)| {
                    bar.base < base.saturating_add(size) && base < bar.base + bar.size
                })
            {
                return Err(ServiceError::InvalidParam);
            }
        }
        if config.power_on_after_fail > 2
            || config.rcba > u32::MAX as u64
            || config.rcba & 0x3fff != 0
            || config.dmibar > u32::MAX as u64
            || config.dmibar & 0xfff != 0
        {
            return Err(ServiceError::InvalidParam);
        }
        if config.pcie_ports.iter().any(|enabled| *enabled)
            && routing.is_some_and(|routing| {
                crate::southbridge::pirq::PciPin::ALL.iter().any(|pin| {
                    !routing
                        .pin_routes
                        .iter()
                        .any(|route| route.slot == 0x1c && route.pin == *pin)
                })
            })
        {
            return Err(ServiceError::InvalidParam);
        }
        if routing.is_some_and(|routing| {
            routing.assigned_pins().any(|(slot, pin)| {
                !routing
                    .pin_routes
                    .iter()
                    .any(|route| route.slot == slot && route.pin == pin)
            })
        }) || config
            .pcie_hotplug
            .iter()
            .enumerate()
            .any(|(index, enabled)| *enabled && !config.pcie_ports[index])
        {
            return Err(ServiceError::InvalidParam);
        }
        if config
            .sata
            .is_some_and(|sata| sata.ports & !0x3f != 0 || sata.hotplug_map & !sata.ports != 0)
        {
            return Err(ServiceError::InvalidParam);
        }

        if config
            .lpc_decode
            .generic_io
            .as_slice()
            .iter()
            .copied()
            .any(|range| range.encode().is_none())
        {
            return Err(ServiceError::InvalidParam);
        }
        if config
            .io_traps
            .as_slice()
            .iter()
            .copied()
            .any(|trap| trap.encode().is_none())
        {
            return Err(ServiceError::InvalidParam);
        }

        Ok(Self {
            config,
            smbus: None,
            pm: PmIo::new(ich9::DEFAULT_PMBASE),
            s3_enabled: false,
            resume: false,
        })
    }

    #[cfg(feature = "acpi")]
    fn config(&self) -> &'static Self::Config {
        self.config
    }

    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        self.enable_spi_prefetching_and_caching();
        self.program_fixed_bars();
        self.reset_watchdog_and_cmos();
        self.program_lpc_decode();
        self.setup_gpios();
        Ok(())
    }

    /// Raminit-era southbridge init: SMBus, PIRQ routes, function disable,
    /// early chipset settings and HPET. DMI follows training/GM45 PM.
    fn early_init(&mut self) -> Result<(), ServiceError> {
        // Bootblock-level SPI, fixed BAR, CMOS/watchdog, LPC decode, and GPIO
        // setup was already done by pre_console_init(). Avoid replaying those
        // writes here; early_init is the raminit-era southbridge path.
        if self.is_mobile() && self.config.sata.is_some_and(|sata| sata.ports & !0x33 != 0) {
            return Err(ServiceError::InvalidParam);
        }
        self.enable_smbus();
        self.write_pirq_routes();
        self.pcie_prepare_before_disable();
        self.clear_disabled_device_commands();
        let rcba = self.rcba();
        let fd = self.function_disable_mask();
        rcba.regs().fd.set(rcba.regs().fd.get() | fd.bits() | 1);
        if self.config.disable_lan {
            rcba.regs().buc.set(rcba.regs().buc.get() | (1 << 5));
        }
        self.early_chipset_settings();
        self.hide_disabled_root_ports();
        self.pm().write32(GPE0_EN_ICH9, self.config.gpe0_en);
        self.enable_hpet();
        if let Some(sata) = self.config.sata.as_ref() {
            self.sata_enable(sata);
        }
        // The platform samples S3 after early_init; do not clear SLP_TYP here.
        fstart_log::info!("intel-ich9: early init complete (fd_mask={:#x})", fd.bits());
        Ok(())
    }

    fn detect_s3_resume(&self) -> bool {
        IntelIch9::detect_s3_resume(self)
    }

    fn set_s3_enabled(&mut self, enabled: bool) {
        self.s3_enabled = enabled;
    }

    fn set_resume(&mut self, resume: bool) {
        self.resume = resume;
    }

    fn training_identity(&self) -> Option<[u8; 5]> {
        Some(crate::southbridge::lpc::training_identity(
            Self::type0_regs(self.lpc()),
        ))
    }

    fn smbus_mut(&mut self) -> Option<&mut dyn SmBus> {
        self.smbus.as_mut().map(|bus| bus as &mut dyn SmBus)
    }

    fn fixed_pci_bars(&self) -> PciFixedBars {
        let mut bars = PciFixedBars::new();
        bars.push(PciFixedBar {
            address: PciAddress::new(0, 0, ich9::SMBUS_DEV, ich9::SMBUS_FUNC),
            register: ich9::SMB_BASE,
            kind: PciFixedBarType::Io,
            base: u64::from(self.config.smbus_base),
            size: 0x20,
            prefetchable: false,
        })
        .expect("ICH9 fixed PCI resource list capacity");
        if let Some(debug) = self.config.ehci_debug_bar {
            bars.push(debug.fixed_bar())
                .expect("ICH9 fixed PCI resource list capacity");
        }
        bars
    }

    fn prepare_early_post_dram_init(&mut self) -> Result<(), ServiceError> {
        // Coreboot romstage: after init_pm, before gm45_late_init.
        self.setup_dmi();
        Ok(())
    }

    fn early_post_dram_init(&mut self) -> Result<(), ServiceError> {
        // The MCH has now enabled VC1; poll after both peers are configured.
        self.poll_vc1()
    }

    /// DRAM-backed ramstage device init: PCIe/PCI bridge, USB, HDA/SATA,
    /// LPC ramstage setup, interrupt routing, and I/O traps.
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        self.early_chipset_settings();
        self.pcie_init();
        self.pci_bridge_init();
        self.usb_init();
        if let Some(hda) = self.config.hda.as_ref() {
            self.hda_init(hda);
        }
        if let Some(sata) = self.config.sata.as_ref() {
            self.sata_init(sata);
        }
        self.ramstage_lpc_init();
        self.configure_default_intmap();
        self.write_interrupt_lines();
        self.configure_io_traps();
        // Coreboot i82801ix_init(): reload the running TCO watchdog.
        #[cfg(target_arch = "x86_64")]
        self.pm().tco().write16(pmio::TCO_RLD, 0);
        fstart_log::info!("intel-ich9: ramstage init complete");
        Ok(())
    }

    /// Lock down write-once southbridge state before payload handoff.
    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        let rcba = self.rcba();
        rcba.regs().fdsw.modify(FDSW::FUNCTION_DISABLE_LOCK::SET);
        rcba.regs().map.set(rcba.regs().map.get());
        Ok(())
    }
}

impl FirmwareImageProvider for IntelIch9 {
    fn firmware_image(&self) -> Result<FirmwareImage, ServiceError> {
        let hsfs = self.spi_read32(SPIBAR_HSFS) & 0xffff;
        if hsfs & HSFS_FDV == 0 {
            fstart_log::error!("intel-ich9: SPI descriptor valid bit is clear");
            return Err(ServiceError::HardwareError);
        }

        let (_bios_offset, bios_size) = self.spi_ifd_region(
            IntelIfdRegion::Bios
                .flreg_index()
                .ok_or(ServiceError::HardwareError)?,
        );
        if bios_size == 0 {
            fstart_log::error!("intel-ich9: SPI descriptor has no BIOS region");
            return Err(ServiceError::HardwareError);
        }

        // ICH9 exposes the BIOS region top-aligned below 4 GiB regardless of
        // where it sits in the physical SPI descriptor.  The boot firmware
        // image is the BIOS region, so expose it as logical offset 0.
        let mut image = FirmwareImage::EMPTY;
        image.size = u64::from(bios_size);
        image.windows[0] = fstart_core::services::FirmwareWindow::new(
            0,
            0x1_0000_0000u64 - u64::from(bios_size),
            u64::from(bios_size),
        );
        image.window_count = 1;
        Ok(image)
    }
}

impl FlashLayoutVerifier for IntelIch9 {
    fn verify_flash_layout(&self, expected: &FlashLayout) -> Result<(), ServiceError> {
        match expected {
            FlashLayout::IntelIfd(layout) => self.verify_ifd_flash_layout(layout),
            FlashLayout::X86Legacy(_) => Err(ServiceError::NotSupported),
        }
    }
}

impl Southbridge for IntelIch9 {
    fn gpio_get(&self, pin: u32) -> Result<bool, ServiceError> {
        if pin > 75 {
            return Err(ServiceError::InvalidParam);
        }
        Ok(IchGpio::new(ich9::DEFAULT_GPIOBASE).get(pin as u8))
    }

    fn gpio_set(&self, pin: u32, value: bool) -> Result<(), ServiceError> {
        if pin > 75 {
            return Err(ServiceError::InvalidParam);
        }
        IchGpio::new(ich9::DEFAULT_GPIOBASE).set(pin as u8, value);
        Ok(())
    }
}

impl SmBus for IntelIch9 {
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

    fn read_word(&mut self, addr: u8, cmd: u8) -> Result<u16, ServiceError> {
        match self.smbus.as_mut() {
            Some(bus) => bus.read_word(addr, cmd),
            None => Err(ServiceError::HardwareError),
        }
    }

    fn write_word(&mut self, addr: u8, cmd: u8, value: u16) -> Result<(), ServiceError> {
        match self.smbus.as_mut() {
            Some(bus) => bus.write_word(addr, cmd, value),
            None => Err(ServiceError::HardwareError),
        }
    }

    // The controller implements these protocols; without the forwarding the
    // trait default would report them unsupported.
    fn block_read(&mut self, addr: u8, cmd: u8, buf: &mut [u8]) -> Result<usize, ServiceError> {
        match self.smbus.as_mut() {
            Some(bus) => bus.block_read(addr, cmd, buf),
            None => Err(ServiceError::HardwareError),
        }
    }

    fn block_write(&mut self, addr: u8, cmd: u8, data: &[u8]) -> Result<(), ServiceError> {
        match self.smbus.as_mut() {
            Some(bus) => bus.block_write(addr, cmd, data),
            None => Err(ServiceError::HardwareError),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntelSouthbridgeDriver;
    use crate::southbridge::pirq::{PciPin, PinRoute, Pirq, PirqRouting, SlotPins};

    pub(super) const ROUTING: PirqRouting<'static> = PirqRouting {
        slot_pins: &[SlotPins {
            slot: 0x1a,
            functions: [Some(PciPin::A), None, None, None, None, None, None, None],
        }],
        pin_routes: &[
            PinRoute {
                slot: 0x1a,
                pin: PciPin::A,
                line: Pirq::D,
            },
            PinRoute {
                slot: 0x1c,
                pin: PciPin::A,
                line: Pirq::G,
            },
            PinRoute {
                slot: 0x1c,
                pin: PciPin::B,
                line: Pirq::B,
            },
            PinRoute {
                slot: 0x1c,
                pin: PciPin::C,
                line: Pirq::C,
            },
            PinRoute {
                slot: 0x1c,
                pin: PciPin::D,
                line: Pirq::D,
            },
        ],
        bridge_routes: &[],
    };

    #[test]
    fn ich9_requires_verified_interrupt_routing() {
        static MISSING: IntelIch9Config = IntelIch9Config::new();
        assert!(matches!(
            IntelIch9::new_from_config(&MISSING),
            Err(ServiceError::InvalidParam)
        ));
        static INCOMPLETE: IntelIch9Config = IntelIch9Config {
            pirq: Some(PirqRouting {
                slot_pins: ROUTING.slot_pins,
                pin_routes: &[],
                bridge_routes: &[],
            }),
            ..IntelIch9Config::new()
        };
        assert!(matches!(
            IntelIch9::new_from_config(&INCOMPLETE),
            Err(ServiceError::InvalidParam)
        ));
        static VALID: IntelIch9Config = IntelIch9Config {
            pirq: Some(ROUTING),
            ..IntelIch9Config::new()
        };
        assert!(IntelIch9::new_from_config(&VALID).is_ok());
    }

    #[test]
    fn ich9_strap_routing_is_exclusive_with_programmed_routing() {
        static PRESERVED: IntelIch9Config = IntelIch9Config {
            preserve_hardware_pirq: true,
            ..IntelIch9Config::new()
        };
        static BOTH: IntelIch9Config = IntelIch9Config {
            pirq: Some(ROUTING),
            ..PRESERVED
        };
        assert!(IntelIch9::new_from_config(&PRESERVED).is_ok());
        assert!(matches!(
            IntelIch9::new_from_config(&BOTH),
            Err(ServiceError::InvalidParam)
        ));
    }

    #[test]
    fn ich9_rejects_reserved_legacy_pirq_values() {
        for route in 0..=u8::MAX {
            let config = IntelIch9Config {
                preserve_hardware_pirq: true,
                pirq_routing: [route; 8],
                ..IntelIch9Config::new()
            };
            let driver =
                IntelIch9::new_from_config(alloc::boxed::Box::leak(alloc::boxed::Box::new(config)));
            assert_eq!(driver.is_ok(), valid_pirq_route(route));
        }
    }

    #[test]
    fn ich9_debug_bar_requires_enabled_ehci_and_nonoverlapping_mmio() {
        let make_driver = |config| {
            IntelIch9::new_from_config(alloc::boxed::Box::leak(alloc::boxed::Box::new(config)))
        };
        let mut config = IntelIch9Config {
            preserve_hardware_pirq: true,
            usb: Some(UsbConfig {
                ehci: [false, true],
                uhci: [false; 6],
            }),
            ehci_debug_bar: Some(EhciDebugConfig {
                ecam_base: 0xe000_0000,
                bus: 0,
                device: 0x1a,
                function: 7,
                bar: 0xfed0_4000,
            }),
            ..IntelIch9Config::new()
        };
        let driver = make_driver(config).unwrap();
        assert!(driver.fixed_pci_bars().iter().any(|bar| {
            bar.address == PciAddress::new(0, 0, 0x1a, 7) && bar.base == 0xfed0_4000
        }));
        config.usb = None;
        assert!(matches!(
            make_driver(config),
            Err(ServiceError::InvalidParam)
        ));
        config.usb = Some(UsbConfig {
            ehci: [false, true],
            uhci: [false; 6],
        });
        for bar in [HPET_BASE as u32, config.rcba as u32, 0xfed0_4001] {
            config.ehci_debug_bar.as_mut().unwrap().bar = bar;
            assert!(matches!(
                make_driver(config),
                Err(ServiceError::InvalidParam)
            ));
        }
        config.ehci_debug_bar.as_mut().unwrap().device = 32;
        assert!(matches!(
            make_driver(config),
            Err(ServiceError::InvalidParam)
        ));
    }

    #[test]
    fn ich9_generic_decode_requires_power_of_two_aligned_window() {
        let window = |base, size| LpcGenericIoDecode { base, size };
        assert!(valid_lpc_generic_io(window(0x1600, 0x80)));
        assert_eq!(window(0x1600, 0x80).encode(), Some(0x007c_1601));
        assert!(!valid_lpc_generic_io(window(0x1600, 12)));
        assert_eq!(window(0x1600, 12).encode(), None);
        assert!(!valid_lpc_generic_io(window(0x1610, 0x20)));
        assert_eq!(window(0x1610, 0x20).encode(), None);
    }

    #[test]
    fn ich9_sata_map_is_programmed_before_pci_allocation() {
        assert_eq!(sata_map(0x03, true), 0x3c60);
        assert_eq!(sata_map(0x33, false), 0x0c00);
    }

    #[test]
    fn ich9_gpio_rejects_out_of_range_pins_before_io() {
        static CONFIG: IntelIch9Config = IntelIch9Config {
            pirq: Some(ROUTING),
            ..IntelIch9Config::new()
        };
        let driver = IntelIch9::new_from_config(&CONFIG).unwrap();
        assert!(matches!(
            driver.gpio_get(256),
            Err(ServiceError::InvalidParam)
        ));
        assert!(matches!(
            driver.gpio_set(76, false),
            Err(ServiceError::InvalidParam)
        ));
    }

    #[test]
    fn ich9_function_disable_includes_sixth_uhci() {
        static CONFIG: IntelIch9Config = IntelIch9Config {
            pirq: Some(ROUTING),
            ..IntelIch9Config::new()
        };
        let driver = IntelIch9::new_from_config(&CONFIG).unwrap();
        let disabled = driver.function_disable_mask();
        assert!(disabled.contains(ich9::FunctionDisable::U6D));
        assert_eq!(IntelIch9::usb_fd_bit(0x1a, 2), ich9::FunctionDisable::U6D);
    }
}

// ---------------------------------------------------------------------------
// ACPI device implementation — ICH9-M southbridge devices
// ---------------------------------------------------------------------------

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi::platform::{IoApicConfig, IsoConfig, X86Config, X86PlatformProvider};

    use fstart_acpi_macros::acpi_dsl;

    use super::*;
    use crate::southbridge::acpi as acpi_fragments;

    const LAPIC_BASE: u64 = 0xFEE0_0000;
    const IOAPIC_BASE: u64 = 0xFEC0_0000;
    const HPET_BASE: u64 = 0xFED0_0000;
    const SCI_IRQ: u8 = 9;
    const PMBASE: u16 = 0x0500;
    /// APM command port firmware and OSPM use to switch ACPI mode.
    const APM_CNT: u32 = 0x00b2;
    /// Command written to [`APM_CNT`] to enable ACPI mode (coreboot
    /// `APM_CNT_ACPI_ENABLE`, handled by the ICH SMM handler).
    const APM_CNT_ACPI_ENABLE: u8 = 0xe1;
    /// Command written to [`APM_CNT`] to disable ACPI mode.
    const APM_CNT_ACPI_DISABLE: u8 = 0x1e;

    static IOAPICS: [IoApicConfig; 1] = [IoApicConfig {
        id: 0,
        base: IOAPIC_BASE,
        gsi_base: 0,
    }];

    /// Root-port downstream INTx swizzling follows the configured D28IR,
    /// not a fixed A-D to GSI16-19 assumption.
    fn root_port_aml(
        name: &str,
        adr: u32,
        port: u8,
        hotplug: bool,
        routing: &crate::southbridge::pirq::PirqRouting<'_>,
    ) -> Vec<u8> {
        use crate::southbridge::pirq::PciPin;
        let mut body: Vec<u8> = acpi_dsl! { Name("_ADR", #{dword adr}); }.into();
        if hotplug {
            let region: Vec<u8> = acpi_dsl! {
                OperationRegion("RPCS", PciConfig, 0x00u32, 0xFFu32);
                Field("RPCS", AnyAcc, NoLock, Preserve) {
                    Offset(0x4C),
                    , 24,
                    RPPN, 8,
                    Offset(0x5A),
                    , 3,
                    PDC_, 1,
                    Offset(0xDF),
                    , 6,
                    HPCS, 1,
                }
            }
            .into();
            body.extend_from_slice(&region);
        }
        let gsi = |pin: u8| {
            let rotated = (port - 1 + pin) % 4;
            routing.gsi(0x1c, PciPin::from_index(rotated).expect("INTx pin 0..3")) as u32
        };
        let (a, b, c, d) = (gsi(0), gsi(1), gsi(2), gsi(3));
        let prt: Vec<u8> = acpi_dsl! {
            Name("_PRT", Package(
                Package(0x0000FFFFu32, 0u32, 0u32, #{dword a}),
                Package(0x0000FFFFu32, 1u32, 0u32, #{dword b}),
                Package(0x0000FFFFu32, 2u32, 0u32, #{dword c}),
                Package(0x0000FFFFu32, 3u32, 0u32, #{dword d})
            ));
        }
        .into();
        body.extend_from_slice(&prt);
        fstart_acpi::aml_linker::device_vec(name, &body).expect("ICH9 PCIe root port emission")
    }

    static ISOS: [IsoConfig; 2] = [
        // coreboot acpi_create_madt_ioapic_gsi0_default(): ISA IRQ0 -> GSI2,
        // edge-triggered, active-high.
        IsoConfig {
            bus: 0,
            source: 0,
            gsi: 2,
            flags: 0x0005,
        },
        // coreboot southbridge/intel/common/pmbase.c ioapic_get_sci_pin():
        // SCI IRQ9/GSI9, level-triggered, active-high.
        IsoConfig {
            bus: 0,
            source: SCI_IRQ,
            gsi: SCI_IRQ as u32,
            flags: 0x000D,
        },
    ];

    impl X86PlatformProvider for IntelIch9 {
        fn x86_platform_config(&self, online_cpus: u32) -> X86Config {
            X86Config {
                num_cpus: online_cpus,
                lapic_base: LAPIC_BASE,
                ioapics: &IOAPICS,
                isos: &ISOS,
                hpet_base: Some(HPET_BASE),
                legacy_devices: true,
                sci_irq: SCI_IRQ,
                pmbase: PMBASE,
                gpe0_offset: GPE0_STS_ICH9,
                gpe0_length: 16,
                // The SMM image handles the APM command port the way coreboot's
                // smihandler does: 0xe1 sets SCI_EN, 0x1e clears it.
                acpi_smi: Some(fstart_core::acpi::AcpiSmiConfig {
                    smi_cmd: APM_CNT,
                    acpi_enable: APM_CNT_ACPI_ENABLE,
                    acpi_disable: APM_CNT_ACPI_DISABLE,
                }),
                // Coreboot i82801ix/fadt.c.
                fadt: fstart_acpi::platform::X86FadtPolicy {
                    mobile: self.config.laptop,
                    pm2_cnt_offset: Some(0x50),
                    p_lvl3_lat: 0x39,
                    duty_width: 3,
                    docking: true,
                },
            }
        }
    }

    impl AcpiDevice for IntelIch9 {
        type Config = IntelIch9Config;

        /// Produce ICH9-M DSDT content under `\\_SB.PCI0`.
        ///
        /// This is the reusable southbridge ACPI namespace: LPC legacy
        /// devices, PIRQ links, USB/HDA/SATA/SMBus/PCIe/PCI bridge device
        /// nodes, and APIC-mode `_PRT` routing. Mainboard-specific EC,
        /// dock/GPE/SMI trap glue is emitted by mainboard drivers.
        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            // PM/GPIO SystemIO OpRegions are intentionally omitted until an
            // emitted AML method actually consumes them. Declaring unused
            // regions makes ACPI claim the native lpc_ich driver resources.
            // Coreboot sleepstates.asl; S3 only once the platform persisted
            // training and stage caches for this image.
            let mut aml: Vec<u8> = acpi_dsl! {
                Scope("\\") {
                    Name("_S0_", Package(0u32, 0u32, 0u32, 0u32));
                    Name("_S5_", Package(7u32, 0u32, 0u32, 0u32));
                }
            }
            .into();
            if self.s3_enabled {
                aml.extend_from_slice(&acpi_dsl! { Scope("\\") {
                    Name("_S3_", Package(5u32, 0u32, 0u32, 0u32));
                } });
            }
            // When keeping strap routing, _PRT must describe the live router,
            // not an assumed A-D map (coreboot intel/common/rcba_pirq.c).
            let hardware_routes = if config.preserve_hardware_pirq {
                use crate::southbridge::pirq::{PciPin, PinRoute, Pirq};
                let r = self.rcba().regs();
                // Devices 1..3 are outside the RCBA router: coreboot's
                // map_pirq uses the hardwired pin-to-PIRQ A..D identity there.
                let slots = [1, 2, 3, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f];
                let ir = [
                    0x3210,
                    0x3210,
                    0x3210,
                    r.d25ir.get(),
                    r.d26ir.get(),
                    r.d27ir.get(),
                    r.d28ir.get(),
                    r.d29ir.get(),
                    r.d30ir.get(),
                    r.d31ir.get(),
                ];
                Some(core::array::from_fn::<_, 40, _>(|index| {
                    let pin = PciPin::ALL[index % 4];
                    PinRoute {
                        slot: slots[index / 4],
                        pin,
                        line: Pirq::from_index(((ir[index / 4] >> (4 * pin.index())) & 7) as u8)
                            .expect("three-bit PIRQ selector"),
                    }
                }))
            } else {
                None
            };
            let hardware_routing = crate::southbridge::pirq::PirqRouting {
                slot_pins: &[],
                pin_routes: hardware_routes.as_ref().map_or(&[], |r| &r[..]),
                bridge_routes: &[],
            };
            let routing = config.pirq.as_ref().unwrap_or(&hardware_routing);

            // The PCI0 body: this chipset's RCRB window and downstream PCI
            // bridge, plus the fragments every ICH shares (coreboot's
            // `pcie.asl`, `usb.asl`, `azalia.asl` device nodes and the LPC
            // bridge's ISA children, PIRQ links, keyboard controller and
            // root-bus `_PRT`).
            let rcba = config.rcba as u32;
            let mut body: Vec<u8> = acpi_dsl! {
                OperationRegion("RCRB", SystemMemory, #{dword rcba}, 0x4000u32);
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
                    , 2,
                    U6DD, 1,
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
            }
            .into();

            body.extend_from_slice(&fstart_acpi::pirq::prt_name_aml(
                routing
                    .root_bus_routes()
                    .map(|(slot, pin, gsi)| (slot, pin.index(), gsi)),
            ));

            body.extend_from_slice(&acpi_fragments::hda_node());
            // UHCI controllers 1-3 live on device 0x1D, 4-6 on device 0x1A
            // (coreboot i82801ix usb.asl): device 0x1D has no function 3.
            for (function, name) in ["USB1", "USB2", "USB3"].iter().enumerate() {
                let adr = 0x001D_0000u32 | function as u32;
                body.extend_from_slice(&acpi_fragments::uhci_node(name, adr, 3));
            }
            for (function, name) in ["USB4", "USB5", "USB6"].iter().enumerate() {
                let adr = 0x001A_0000u32 | function as u32;
                body.extend_from_slice(&acpi_fragments::uhci_node(name, adr, 3));
            }
            body.extend_from_slice(&acpi_fragments::ehci_node("EHC1", 0x001D_0007, 13, 6));
            body.extend_from_slice(&acpi_fragments::ehci_node("EHC2", 0x001A_0007, 13, 6));
            for port in 1..=6u8 {
                let adr = 0x001C_0000u32 + u32::from(port - 1);
                let name = alloc::format!("RP0{port}");
                body.extend_from_slice(&root_port_aml(
                    &name,
                    adr,
                    port,
                    config.pcie_hotplug[(port - 1) as usize],
                    routing,
                ));
            }
            body.extend_from_slice(&acpi_fragments::sata_node());
            body.extend_from_slice(&acpi_fragments::smbus_node());

            // Downstream slot wiring belongs to the board, not the ICH9.
            let pcib: Vec<u8> = acpi_dsl! {
                Device("PCIB") {
                    Name("_ADR", 0x001E0000u32);
                }
            }
            .into();
            body.extend_from_slice(&pcib);

            // The LPC bridge: its register window, common ISA children and
            // PIRQ links, plus the integrated keyboard controller (`lpc.asl`).
            let mut lpcb: Vec<u8> = acpi_dsl! {
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
            }
            .into();
            lpcb.extend_from_slice(&acpi_fragments::legacy_isa_children(
                ich9::DEFAULT_PMBASE,
                ich9::DEFAULT_GPIOBASE,
            ));
            lpcb.extend_from_slice(&acpi_fragments::pci_irq_links(&config.pirq_routing));
            lpcb.extend_from_slice(&acpi_fragments::ps2_controller_nodes());

            body.extend_from_slice(
                &fstart_acpi::aml_linker::device_vec("LPCB", &lpcb)
                    .expect("ICH9 LPCB device emission"),
            );

            aml.extend_from_slice(
                &fstart_acpi::aml_linker::scope_vec("\\_SB_.PCI0", &body)
                    .expect("ICH9 PCI scope emission"),
            );
            aml
        }

        fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::IntelSouthbridgeDriver;

        /// `Name("_ADR", <dword>)` as the DSL encodes it.
        fn adr_name_aml(adr: u32) -> [u8; 10] {
            let mut bytes = [0u8; 10];
            bytes[0] = 0x08; // NameOp
            bytes[1..5].copy_from_slice(b"_ADR");
            bytes[5] = 0x0C; // DwordPrefix
            bytes[6..10].copy_from_slice(&adr.to_le_bytes());
            bytes
        }

        #[test]
        fn pcie_prt_follows_the_programmed_ich9_d28ir() {
            let aml = root_port_aml("RP01", 0x001c_0000, 1, false, &super::super::tests::ROUTING);
            // D28IR routes INTA to PIRQG, GSI22, not a fixed GSI16.
            assert!(aml.windows(5).any(|bytes| bytes == [0x0c, 22, 0, 0, 0]));
        }

        #[test]
        fn uhci_nodes_match_the_ich9_function_layout() {
            // A real board supplies its verified routing. This ACPI test only
            // checks device addresses and uses a minimal synthetic route.
            let mut config = IntelIch9Config::new();
            config.pirq = Some(super::super::tests::ROUTING);
            let config = alloc::boxed::Box::leak(alloc::boxed::Box::new(config));
            let south = IntelIch9::new_from_config(config).expect("ICH9 config is valid");
            let aml = south.dsdt_aml(config);

            let has_adr = |adr: u32| aml.windows(10).any(|window| window == adr_name_aml(adr));
            // Coreboot i82801ix: USB1-3 on device 0x1D, USB4-6 on 0x1A.
            for function in 0..3u32 {
                assert!(has_adr(0x001D_0000 | function), "missing 0:1D.{function}");
                assert!(has_adr(0x001A_0000 | function), "missing 0:1A.{function}");
            }
            // Device 0x1D has no function 3 on ICH9; USB4 is at 0:1A.0.
            assert!(!has_adr(0x001D_0003), "0:1D.3 does not exist on ICH9");
        }
    }
}
