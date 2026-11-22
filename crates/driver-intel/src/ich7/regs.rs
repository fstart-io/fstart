//! ICH7 RCBA and LPC PCI configuration register definitions.

use fstart_core::mmio::MmioReadWrite;
use fstart_pci::pci_type0_config;
use tock_registers::{register_bitfields, register_structs};

register_bitfields! [u32,
    /// General Control and Status.
    pub GCS_REG [
        FERR_MUX_ENABLE OFFSET(6) NUMBITS(1) [],
        NO_REBOOT OFFSET(5) NUMBITS(1) [],
        BILD OFFSET(0) NUMBITS(1) []
    ],
    /// HPET Configuration.
    pub HPTC_REG [
        ADDRESS_SELECT OFFSET(0) NUMBITS(2) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    /// Function Disable register.
    pub FD_REG [
        PERFORMANCE_COUNTER_DISABLE OFFSET(0) NUMBITS(1) [],
        RAW OFFSET(0) NUMBITS(32) []
    ],
    /// Function Disable 2 / TC lockdown register.
    pub FD2_REG [
        TCLOCKDN OFFSET(31) NUMBITS(1) []
    ],
    /// Clock-gating control.
    pub CG_REG [
        PCIE_ROOT_PORT_STATIC OFFSET(0) NUMBITS(1) [],
        RAW OFFSET(0) NUMBITS(32) []
    ],
    /// SATA prefetch/caching control.
    pub SATA_PREFETCH_REG [
        ENABLE OFFSET(0) NUMBITS(1) []
    ],
    pub CIR8_REG [
        FIELD_1_0 OFFSET(0) NUMBITS(2) []
    ],
    pub CIR_2034_REG [
        FIELD_19_16 OFFSET(16) NUMBITS(4) []
    ],
    /// LPC ETR3 reset-control register.
    pub ETR3_REG [
        CWORWRE OFFSET(18) NUMBITS(1) [],
        CF9GR OFFSET(20) NUMBITS(1) [],
        CF9LOCK OFFSET(31) NUMBITS(1) []
    ],
];

register_bitfields! [u16,
    /// LPC GEN_PMCON_1 register.
    pub GEN_PMCON_1_REG [
        SMI_RATE OFFSET(0) NUMBITS(2) [],
        CLKRUN_EN OFFSET(2) NUMBITS(1) [],
        SPEEDSTEP_EN OFFSET(3) NUMBITS(1) [],
        SMI_LOCK OFFSET(4) NUMBITS(1) [],
        CPUSLP_EN OFFSET(5) NUMBITS(1) [],
        C4ONC3_EN OFFSET(7) NUMBITS(1) [],
        BIOS_PCI_EXP_EN OFFSET(10) NUMBITS(1) []
    ],
    /// SPI status (SPIS).
    pub SPI_REG [
        CYCLE_IN_PROGRESS OFFSET(0) NUMBITS(1) [],
        CYCLE_DONE OFFSET(2) NUMBITS(1) [],
        CYCLE_ERROR OFFSET(3) NUMBITS(1) [],
        LOCK OFFSET(15) NUMBITS(1) []
    ],
    /// SPI control (SPIC).
    pub SPI_CTRL_REG [
        ACCESS_REQUEST OFFSET(0) NUMBITS(1) [],
        CYCLE_GO OFFSET(1) NUMBITS(1) [],
        OPCODE_INDEX OFFSET(4) NUMBITS(3) [],
        DATA_BYTES OFFSET(8) NUMBITS(6) [],
        DATA_CYCLE OFFSET(14) NUMBITS(1) []
    ],
    pub PCIE_TUNING16 [
        BIT7 OFFSET(7) NUMBITS(1) []
    ],
];

register_bitfields! [u8,
    /// SATA PCS port-enable bits (the low nibble).
    pub SATA_PORT_ENABLE [PORTS OFFSET(0) NUMBITS(4) []],
    /// LPC SIRQ_CNTL. The frame-size field is read-only (21 frames).
    pub SERIRQ_REG [
        ENABLE OFFSET(7) NUMBITS(1) [],
        MODE OFFSET(6) NUMBITS(1) [
            Quiet = 0,
            Continuous = 1
        ],
        FRAME_SIZE OFFSET(2) NUMBITS(4) [],
        START_PULSE OFFSET(0) NUMBITS(2) [
            Clocks4 = 0,
            Clocks6 = 1,
            Clocks8 = 2
        ],
    ],
    /// LPC ACPI control register.
    pub ACPI_CNTL_REG [
        ACPI_EN OFFSET(7) NUMBITS(1) []
    ],
    /// LPC GEN_PMCON_3 register.
    pub GEN_PMCON_3_REG [
        STATE_AFTER_G3 OFFSET(0) NUMBITS(1) [],
        RTC_BATTERY_DEAD OFFSET(2) NUMBITS(1) [],
        MIN_SLP_S4_ASSERT OFFSET(3) NUMBITS(1) [],
        SLP_S3_STRETCH OFFSET(4) NUMBITS(2) []
    ],
    /// LPC GEN_PMCON_LOCK register.
    pub GEN_PMCON_LOCK_REG [
        ACPI_BASE_LOCK OFFSET(1) NUMBITS(1) [],
        SLP_STR_POL_LOCK OFFSET(2) NUMBITS(1) []
    ],
    /// Other Interrupt Control.
    pub OIC_REG [
        AEN OFFSET(0) NUMBITS(1) [],
        OAEN OFFSET(1) NUMBITS(1) []
    ],
    /// Function Disable SUS Well byte used during final lockdown.
    pub FDSW8 [
        LOCK OFFSET(7) NUMBITS(1) []
    ],
    pub PCIE_TUNING8 [
        BIT7 OFFSET(7) NUMBITS(1) []
    ],
];

register_structs! {
    /// ICH7 Root Complex Base Address MMIO register block.
    pub(crate) RcbaRegs {
        (0x0000 => _reserved_start),
        (0x0014 => pub v0ctl: MmioReadWrite<u32>),
        (0x0018 => _reserved_dmi0),
        (0x001c => pub v1cap: MmioReadWrite<u32>),
        (0x0020 => pub v1ctl: MmioReadWrite<u32>),
        (0x0024 => _reserved_dmi1),
        (0x0050 => pub fd2: MmioReadWrite<u32, FD2_REG::Register>),
        (0x0054 => _reserved0),
        (0x0088 => pub cir1: MmioReadWrite<u32>),
        (0x008c => _reserved1),
        (0x0104 => pub esd: MmioReadWrite<u32>),
        (0x0108 => _reserved_ul0),
        (0x0110 => pub uld: MmioReadWrite<u32>),
        (0x0114 => _reserved_ul1),
        (0x0118 => pub ulba_lo: MmioReadWrite<u32>),
        (0x011c => _reserved_ul2),
        (0x0120 => pub rp1d: MmioReadWrite<u32>),
        (0x0124 => _reserved_rp0),
        (0x0130 => pub rp2d: MmioReadWrite<u32>),
        (0x0134 => _reserved_rp1),
        (0x0140 => pub rp3d: MmioReadWrite<u32>),
        (0x0144 => _reserved_rp2),
        (0x0150 => pub rp4d: MmioReadWrite<u32>),
        (0x0154 => _reserved_rp3),
        (0x0160 => pub hdd: MmioReadWrite<u32>),
        (0x0164 => _reserved_rp4),
        (0x0170 => pub rp5d: MmioReadWrite<u32>),
        (0x0174 => _reserved_rp5),
        (0x0180 => pub rp6d: MmioReadWrite<u32>),
        (0x0184 => _reserved_rp6),
        (0x01a4 => pub lcap: MmioReadWrite<u32>),
        (0x01a8 => pub lctl: MmioReadWrite<u16>),
        (0x01aa => _reserved_lctl),
        (0x01f4 => pub cir2: MmioReadWrite<u32>),
        (0x01f8 => _reserved2),
        (0x01fc => pub cir3: MmioReadWrite<u16>),
        (0x01fe => _reserved3),
        (0x0200 => pub cir4: MmioReadWrite<u16>),
        (0x0202 => _reserved4),
        (0x0214 => pub cir5: MmioReadWrite<u32>),
        (0x0218 => pub cir6: MmioReadWrite<u32>),
        (0x021c => _reserved5),
        (0x0220 => pub bcr: MmioReadWrite<u8>),
        (0x0221 => _reserved6),
        (0x1d40 => pub sata_prefetch: MmioReadWrite<u32, SATA_PREFETCH_REG::Register>),
        (0x1d44 => _reserved7),
        (0x2010 => pub misc_2010: MmioReadWrite<u32>),
        (0x2014 => _reserved7b),
        (0x2027 => pub cir_2027: MmioReadWrite<u8>),
        (0x2028 => _reserved8),
        (0x2034 => pub cir_2034: MmioReadWrite<u32, CIR_2034_REG::Register>),
        (0x2038 => _reserved9),
        (0x21a4 => pub cir_21a4: MmioReadWrite<u32>),
        (0x21a8 => _reserved10),
        (0x3020 => pub spi: MmioReadWrite<u16, SPI_REG::Register>),
        (0x3022 => pub spi_ctrl: MmioReadWrite<u16, SPI_CTRL_REG::Register>),
        (0x3024 => pub spi_addr: MmioReadWrite<u32>),
        (0x3028 => pub spi_data: [MmioReadWrite<u8>; 64]),
        (0x3068 => _reserved_spi),
        (0x3074 => pub spi_preop: [MmioReadWrite<u8>; 2]),
        (0x3076 => pub spi_optype: MmioReadWrite<u16>),
        (0x3078 => pub spi_opmenu: [MmioReadWrite<u8>; 8]),
        (0x3080 => _reserved_spi2),
        (0x3100 => pub d31ip: MmioReadWrite<u32>),
        (0x3104 => pub d30ip: MmioReadWrite<u32>),
        (0x3108 => pub d29ip: MmioReadWrite<u32>),
        (0x310c => pub d28ip: MmioReadWrite<u32>),
        (0x3110 => pub d27ip: MmioReadWrite<u32>),
        (0x3114 => _reserved11),
        (0x3140 => pub d31ir: MmioReadWrite<u16>),
        (0x3142 => pub d30ir: MmioReadWrite<u16>),
        (0x3144 => pub d29ir: MmioReadWrite<u16>),
        (0x3146 => pub d28ir: MmioReadWrite<u16>),
        (0x3148 => pub d27ir: MmioReadWrite<u16>),
        (0x314a => _reserved12),
        (0x31ff => pub oic: MmioReadWrite<u8, OIC_REG::Register>),
        (0x3200 => _reserved13),
        (0x3400 => pub bios_cntl: MmioReadWrite<u32>),
        (0x3404 => pub hptc: MmioReadWrite<u32, HPTC_REG::Register>),
        (0x3408 => _reserved14),
        (0x3410 => pub gcs: MmioReadWrite<u32, GCS_REG::Register>),
        (0x3414 => _reserved15),
        (0x3418 => pub fd: MmioReadWrite<u32, FD_REG::Register>),
        (0x341c => pub cg: MmioReadWrite<u32, CG_REG::Register>),
        (0x3420 => pub fdsw8: MmioReadWrite<u8, FDSW8::Register>),
        (0x3421 => _reserved16),
        (0x3430 => pub cir8: MmioReadWrite<u32, CIR8_REG::Register>),
        (0x3434 => _reserved17),
        (0x3e08 => pub port_3e08: MmioReadWrite<u16, PCIE_TUNING16::Register>),
        (0x3e0a => _reserved19),
        (0x3e0e => pub port_3e0e: MmioReadWrite<u8, PCIE_TUNING8::Register>),
        (0x3e0f => _reserved20),
        (0x3e48 => pub port_3e48: MmioReadWrite<u16, PCIE_TUNING16::Register>),
        (0x3e4a => _reserved21),
        (0x3e4e => pub port_3e4e: MmioReadWrite<u8, PCIE_TUNING8::Register>),
        (0x3e4f => @END),
    }
}

pci_type0_config! {
    /// ICH7 LPC bridge PCI configuration space.
    pub struct Ich7LpcPciConfig {
        (0x40 => pub pmbase: MmioReadWrite<u32>),
        (0x44 => pub acpi_cntl: MmioReadWrite<u8, ACPI_CNTL_REG::Register>),
        (0x45 => _reserved_lpc0),
        (0x48 => pub gpio_base: MmioReadWrite<u32>),
        (0x4c => pub gpio_cntl: MmioReadWrite<u8>),
        (0x4d => _reserved_lpc1),
        (0x60 => pub pirqa_rout: MmioReadWrite<u32>),
        (0x64 => pub serirq_cntl: MmioReadWrite<u8, SERIRQ_REG::Register>),
        (0x65 => _reserved_lpc2),
        (0x68 => pub pirqe_rout: MmioReadWrite<u32>),
        (0x6c => _reserved_lpc8),
        (0x80 => pub lpc_io_dec: MmioReadWrite<u16>),
        (0x82 => pub lpc_en: MmioReadWrite<u16>),
        (0x84 => pub gen_dec: [MmioReadWrite<u32>; 4]),
        (0x94 => _reserved_lpc3),
        (0xa0 => pub gen_pmcon_1: MmioReadWrite<u16, GEN_PMCON_1_REG::Register>),
        (0xa2 => _reserved_lpc4),
        (0xa4 => pub gen_pmcon_3: MmioReadWrite<u8, GEN_PMCON_3_REG::Register>),
        (0xa5 => _reserved_lpc5),
        (0xa6 => pub gen_pmcon_lock: MmioReadWrite<u8, GEN_PMCON_LOCK_REG::Register>),
        (0xa7 => _reserved_lpc6),
        (0xac => pub etr3: MmioReadWrite<u32, ETR3_REG::Register>),
        (0xb0 => _reserved_lpc7),
        (0x1000 => @END),
    }
}
