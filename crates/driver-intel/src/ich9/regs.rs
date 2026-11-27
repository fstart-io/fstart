//! ICH9 RCBA and LPC PCI configuration register definitions.

use crate::southbridge::lpc::{IOTR_HI, IOTR_LO};
use fstart_core::mmio::MmioReadWrite;
use fstart_pci::{PCI_COMMAND_BITS, pci_type0_config};
use tock_registers::{register_bitfields, register_structs};

register_bitfields! [u32,
    /// Virtual channel control/status registers.
    pub VCTL [
        ENABLE OFFSET(31) NUMBITS(1) [],
        VC_NEGOTIATION_PENDING OFFSET(16) NUMBITS(1) [],
        VC_ARB_SELECT OFFSET(17) NUMBITS(3) [],
        ID OFFSET(24) NUMBITS(3) [],
        TC_MAP OFFSET(1) NUMBITS(7) []
    ],
    /// Virtual channel capability register.
    pub VCAP [
        REFERENCE_CLOCK OFFSET(16) NUMBITS(7) []
    ],
    /// Function Disable register.
    pub FD [
        RAW OFFSET(0) NUMBITS(32) []
    ],
    /// General Control and Status.
    pub GCS_REG [
        BBS OFFSET(10) NUMBITS(1) [],
        BOOT_SMI_EN OFFSET(6) NUMBITS(1) [],
        NO_REBOOT OFFSET(5) NUMBITS(1) [],
        SMI_LOCK OFFSET(4) NUMBITS(1) []
    ],
    /// Function Disable SUS Well register.
    pub FDSW [
        FUNCTION_DISABLE_LOCK OFFSET(7) NUMBITS(1) [],
        LAN_DISABLE OFFSET(0) NUMBITS(1) []
    ],
    /// HPET Configuration.
    pub HPTC [
        ADDRESS_SELECT OFFSET(0) NUMBITS(2) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    /// Clock gating control.
    pub CG [
        RAW OFFSET(0) NUMBITS(32) []
    ],
    /// Root-port function-number map.
    pub RPFN [
        RAW OFFSET(0) NUMBITS(32) []
    ],
    /// DMI controls.
    pub DMC [
        MOBILE_POWER_SAVINGS OFFSET(19) NUMBITS(1) []
    ],
    pub DMIC [
        VIRTUAL_CHANNEL_ENABLE OFFSET(0) NUMBITS(2) []
    ],
    pub LCAP_REG [
        ASPM_SUPPORT OFFSET(10) NUMBITS(2) []
    ],
    pub CIR5_REG [
        BIT0 OFFSET(0) NUMBITS(1) []
    ],
    pub CIR6_REG [
        BIT7 OFFSET(7) NUMBITS(1) [],
        FIELD_23_21 OFFSET(21) NUMBITS(3) []
    ],
    pub CIR8_REG [
        FIELD_1_0 OFFSET(0) NUMBITS(2) []
    ],
    pub CIR9_REG [
        FIELD_27_26 OFFSET(26) NUMBITS(2) []
    ],
    pub CIR7_REG [
        FIELD_19_16 OFFSET(16) NUMBITS(4) []
    ],
    pub CIR13_REG [
        FIELD_19_16 OFFSET(16) NUMBITS(4) []
    ],
    pub CIR10_REG [
        FIELD_17_16 OFFSET(16) NUMBITS(2) []
    ],
    pub BIOS_CNTL_REG [
        EXTENDED_CMOS_ENABLE OFFSET(2) NUMBITS(1) []
    ],
    pub SPI_PREFETCH_REG [
        ENABLE OFFSET(0) NUMBITS(3) []
    ],
    /// LPC PMIR register.
    pub PMIR_REG [
        USB_TRANSIENT_DISCONNECT OFFSET(8) NUMBITS(2) []
    ],
];

register_bitfields! [u16,
    /// ICH9 LPC GEN_PMCON_1 (mobile and desktop bits differ).
    pub GEN_PMCON_1_REG [
        SMI_RATE OFFSET(0) NUMBITS(2) [],
        CLKRUN_EN OFFSET(2) NUMBITS(1) [],
        SPEEDSTEP_EN OFFSET(3) NUMBITS(1) [],
        CPU_SLP_EN OFFSET(5) NUMBITS(1) [],
        C4_ON_C3_EN OFFSET(7) NUMBITS(1) [],
        BIOS_PCI_EXP_EN OFFSET(10) NUMBITS(1) [],
        C5_EN OFFSET(11) NUMBITS(1) []
    ],
    /// DMI link status.
    pub LSTS [
        NEGOTIATED_WIDTH OFFSET(4) NUMBITS(6) []
    ],
    pub LCTL_REG [
        ASPM_CONTROL OFFSET(0) NUMBITS(2) []
    ],
    pub CIR_20C4_REG [
        BIT15 OFFSET(15) NUMBITS(1) []
    ],
    pub CIR_20E4_REG [
        BIT15 OFFSET(15) NUMBITS(1) []
    ],
];

register_bitfields! [u8,
    /// LPC ACPI control register.
    pub ACPI_CNTL_REG [
        ACPI_EN OFFSET(7) NUMBITS(1) []
    ],
    /// LPC GPIO control register.
    pub GPIO_CNTL_REG [
        GPIO_EN OFFSET(4) NUMBITS(1) []
    ],
    /// LPC GEN_PMCON_3 register.
    pub GEN_PMCON_3_REG [
        STATE_AFTER_G3 OFFSET(0) NUMBITS(1) [],
        RTC_POWER_FAILED OFFSET(1) NUMBITS(1) [],
        RTC_BATTERY_DEAD OFFSET(2) NUMBITS(1) [],
        MIN_SLP_S4_ASSERT OFFSET(3) NUMBITS(1) [],
        SLP_S3_STRETCH OFFSET(4) NUMBITS(2) []
    ],
    /// LPC C-state configuration.
    pub CXSTATE_CNF_REG [
        C3_POPUP_ENABLE OFFSET(2) NUMBITS(1) [],
        BM_STS_ZERO_ENABLE OFFSET(3) NUMBITS(1) [],
        C3_POPDOWN_ENABLE OFFSET(4) NUMBITS(1) []
    ],
    /// LPC C4 timing control.
    pub C4TIMING_CNT_REG [
        VALUE OFFSET(0) NUMBITS(4) []
    ],
    /// LPC C5 exit timing.
    pub C5_EXIT_TIMING_REG [
        C5_EXIT OFFSET(0) NUMBITS(3) [],
        C6_EXIT OFFSET(3) NUMBITS(3) []
    ],
    /// Other Interrupt Control.
    pub OIC_REG [
        AEN OFFSET(0) NUMBITS(1) [],
        OAEN OFFSET(1) NUMBITS(1) []
    ],
];

register_structs! {
    /// Small Type 0 PCI config overlay for exact programming of the class-code
    /// Prog IF byte. The shared generic overlay intentionally exposes Prog IF
    /// as read-only for normal code; ICH9 mode switching requires an explicit
    /// chipset-driver write.
    pub PciType0ProgIfConfig {
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
    pub RcbaRegs {
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
        (0x3104 => _d30ip), // Read-only: the PCI bridge has no interrupt pin.
        (0x3108 => pub d29ip: MmioReadWrite<u32>),
        (0x310c => pub d28ip: MmioReadWrite<u32>),
        (0x3110 => pub d27ip: MmioReadWrite<u32>),
        (0x3114 => pub d26ip: MmioReadWrite<u32>),
        (0x3118 => pub d25ip: MmioReadWrite<u32>),
        (0x311c => _reserved20),
        (0x3140 => pub d31ir: MmioReadWrite<u16>),
        (0x3142 => _d30ir), // Read-only: not a route for downstream slots.
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
