//! Intel 8 Series (Lynx Point-H and Lynx Point-LP) PCH register layouts.
//!
//! Offsets and bitfields follow the Intel 8 Series PCH datasheet and coreboot
//! `southbridge/intel/lynxpoint/{pch.h,lpc.c,lp_gpio.h,thermal.c,serialio.c}`.
//! Undocumented chipset-initialization tables stay raw through [`MmioWindow`];
//! named registers are typed here.

use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use fstart_pci::pci_type0_config;
use tock_registers::{register_bitfields, register_structs};

/// Fixed Lynx Point PCH functions on bus 0 (8-bit `devfn`).
pub mod dev {
    use fstart_pci::devfn;

    /// Audio DSP (Lynx Point-LP only).
    pub const ADSP: u8 = devfn(0x13, 0);
    /// xHCI USB 3.0 controller.
    pub const XHCI: u8 = devfn(0x14, 0);
    /// Serial IO device number (LP: functions 0..6 = DMA, I2C0, I2C1, SPI0, SPI1, UART0, UART1).
    pub const SERIAL_IO: u8 = 0x15;
    /// MEI / HECI #1.
    pub const MEI1: u8 = devfn(0x16, 0);
    /// MEI / HECI #2.
    pub const MEI2: u8 = devfn(0x16, 1);
    /// IDE-R.
    pub const IDER: u8 = devfn(0x16, 2);
    /// Keyboard and Text (KT) redirection.
    pub const KT: u8 = devfn(0x16, 3);
    /// SDIO controller (Lynx Point-LP only).
    pub const SDIO: u8 = devfn(0x17, 0);
    /// Gigabit Ethernet controller.
    pub const GBE: u8 = devfn(0x19, 0);
    /// EHCI #2 (Lynx Point-H only).
    pub const EHCI2: u8 = devfn(0x1a, 0);
    /// High Definition Audio controller.
    pub const HDA: u8 = devfn(0x1b, 0);
    /// PCIe root ports device number (functions 0..5 on LP, 0..7 on H).
    pub const PCIE: u8 = 0x1c;
    /// EHCI #1.
    pub const EHCI1: u8 = devfn(0x1d, 0);
    /// LPC bridge.
    pub const LPC: u8 = devfn(0x1f, 0);
    /// Primary SATA controller (AHCI).
    pub const SATA1: u8 = devfn(0x1f, 2);
    /// SMBus controller.
    pub const SMBUS: u8 = devfn(0x1f, 3);
    /// Secondary SATA controller (legacy IDE on LPT-H; hidden in AHCI mode).
    pub const SATA2: u8 = devfn(0x1f, 5);
    /// Thermal subsystem.
    pub const THERMAL: u8 = devfn(0x1f, 6);
}

/// Lynx Point-LP GPIO I/O port offsets relative to `GPIOBASE`.
pub mod lp_gpio {
    /// Total number of physical GPIO pads on Lynx Point-LP (0..94).
    pub const PAD_COUNT: usize = 95;
    /// GPIO ownership banks (`GPIO_OWN0..2`, 3 × 32-bit).
    pub const OWNER_BASE: u16 = 0x00;
    /// PIRQ-to-IOAPIC routing enable (`GPI_ROUT2` / `PIRQ_TO_IOAPIC`).
    pub const PIRQ_TO_IOAPIC: u16 = 0x10;
    /// GPIO blink enable (`GP_BLINK`).
    pub const BLINK: u16 = 0x18;
    /// GPI SMI routing banks (`GPI_ROUT0..2`, 3 × 32-bit).
    pub const SMI_ROUT_BASE: u16 = 0x30;
    /// Alternate GPI SMI status (`ALT_GPI_SMI_STS`).
    pub const ALT_GPI_SMI_STS: u16 = 0x50;
    /// Alternate GPI SMI enable (`ALT_GPI_SMI_EN`).
    pub const ALT_GPI_SMI_EN: u16 = 0x54;
    /// GPIO reset selection banks (`GP_RST_SEL0..2`, 3 × 32-bit).
    pub const RESET_BASE: u16 = 0x60;
    /// GPI interrupt enable banks (`GPI_IE0..2`, 3 × 32-bit).
    pub const IRQ_EN_BASE: u16 = 0x90;
    /// Per-pad configuration pair base (`GP_n_CONFIG0` at `+8*n`, `CONFIG1` at `+8*n+4`).
    pub const CONF_BASE: u16 = 0x100;
}

register_bitfields![u8,
    /// LPC ACPI Control (`ACPI_CNTL`, PCI `0x44`).
    pub ACPI_CNTL [
        ACPI_EN OFFSET(7) NUMBITS(1) []
    ],
    /// LPC GPIO Control (`GPIO_CNTL`, PCI `0x4c`).
    pub GPIO_CNTL [
        GPIO_EN OFFSET(4) NUMBITS(1) []
    ],
    /// LPC Serial IRQ Control (`SERIRQ_CNTL`, PCI `0x64`).
    pub SERIRQ_CNTL [
        START_FRAME_WIDTH OFFSET(0) NUMBITS(2) [],
        FRAME_SIZE OFFSET(2) NUMBITS(4) [],
        CONTINUOUS OFFSET(6) NUMBITS(1) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    /// General PM Configuration 2 (`GEN_PMCON_2`, PCI `0xa2`).
    pub GEN_PMCON_2 [
        /// DRAM was placed in self-refresh on the last S3 entry.
        MEM_SR OFFSET(5) NUMBITS(1) [],
        /// DRAM initialization in progress (sticky firmware scratch bit).
        DRAM_INIT OFFSET(7) NUMBITS(1) []
    ],
    /// General PM Configuration Lock (`GEN_PMCON_LOCK`, PCI `0xa6`).
    pub GEN_PMCON_LOCK [
        ACPI_BASE_LOCK OFFSET(1) NUMBITS(1) [],
        SLP_STR_POL_LOCK OFFSET(2) NUMBITS(1) []
    ],
    /// Function Disable SUS well (`FDSW`, RCBA `0x3420`).
    pub FDSW [
        LOCK OFFSET(7) NUMBITS(1) []
    ]
];

register_bitfields![u16,
    /// LPC Fixed I/O Decode Enables (`LPC_EN`, PCI `0x82`).
    pub LPC_EN [
        COMA OFFSET(0) NUMBITS(1) [],
        COMB OFFSET(1) NUMBITS(1) [],
        LPT OFFSET(2) NUMBITS(1) [],
        FDD OFFSET(3) NUMBITS(1) [],
        GAMEL OFFSET(8) NUMBITS(1) [],
        GAMEH OFFSET(9) NUMBITS(1) [],
        KBC OFFSET(10) NUMBITS(1) [],
        MC OFFSET(11) NUMBITS(1) [],
        CNF1 OFFSET(12) NUMBITS(1) [],
        CNF2 OFFSET(13) NUMBITS(1) []
    ],
    /// General PM Configuration 1 (`GEN_PMCON_1`, PCI `0xa0`).
    pub GEN_PMCON_1 [
        SMI_RATE OFFSET(0) NUMBITS(2) [],
        CLKRUN_EN OFFSET(2) NUMBITS(1) [],
        SMI_LOCK OFFSET(4) NUMBITS(1) [],
        BIOS_PCI_EXP_EN OFFSET(10) NUMBITS(1) []
    ],
    /// General PM Configuration 3 (`GEN_PMCON_3`, PCI `0xa4`).
    pub GEN_PMCON_3 [
        AFTER_G3 OFFSET(0) NUMBITS(1) [],
        PWR_FLR OFFSET(1) NUMBITS(1) [],
        RTC_BATTERY_DEAD OFFSET(2) NUMBITS(1) [],
        SLP_S4_STRETCH_EN OFFSET(3) NUMBITS(1) [],
        SLP_S4_MIN OFFSET(4) NUMBITS(2) [],
        SLP_S3_MIN OFFSET(10) NUMBITS(2) [],
        SLP_STRETCH_SUS_DIS OFFSET(12) NUMBITS(1) []
    ],
    /// Device Interrupt Route (`DxxIR`, RCBA `0x3140..=0x3168`): 3-bit PIRQ index per INTx pin.
    pub DIR [
        IAR OFFSET(0) NUMBITS(3) [],
        IBR OFFSET(4) NUMBITS(3) [],
        ICR OFFSET(8) NUMBITS(3) [],
        IDR OFFSET(12) NUMBITS(3) []
    ],
    /// Other Interrupt Control (`OIC`, RCBA `0x31fe`).
    pub OIC [
        APIC_RANGE OFFSET(0) NUMBITS(8) [],
        AEN OFFSET(8) NUMBITS(1) [],
        COPROCESSOR_ERROR OFFSET(9) NUMBITS(1) []
    ],
    /// IOBP Status (`IOBPS`, RCBA `0x2338`).
    pub IOBP_STATUS [
        BUSY OFFSET(0) NUMBITS(1) [],
        RESPONSE OFFSET(1) NUMBITS(2) [],
        OPCODE OFFSET(8) NUMBITS(8) [
            Read = 6,
            Write = 7
        ]
    ],
    /// Hardware Sequencing Flash Status (`HSFS`, RCBA `0x3804`).
    pub HSFS [
        FLOCKDN OFFSET(15) NUMBITS(1) []
    ]
];

register_bitfields![u32,
    /// Root Complex Base Address (`RCBA`, LPC PCI `0xf0`).
    pub RCBA [
        ENABLE OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(14) NUMBITS(18) []
    ],
    /// Extended Test Mode Register 3 / PMIR (`ETR3`, LPC PCI `0xac`).
    pub ETR3 [
        /// xHCI Smart Auto scratch bit (coreboot `PCH_LPC_PMIR` bit 16).
        SMART_AUTO OFFSET(16) NUMBITS(1) [],
        /// CF9h Without Resume Well Reset Enable.
        CWORWRE OFFSET(18) NUMBITS(1) [],
        /// CF9h Global Reset.
        CF9GR OFFSET(20) NUMBITS(1) [],
        /// Power-management bit set on LP.
        LP_PM_BIT21 OFFSET(21) NUMBITS(1) [],
        /// Lock CF9h Global Reset.
        CF9LOCK OFFSET(31) NUMBITS(1) []
    ],
    /// Traffic Class Lockdown (`TCLOCKDN`, RCBA `0x0050`).
    pub TCLOCKDN [
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    /// Device Interrupt Pin (`DxxIP`, RCBA `0x3100..=0x3128`): 4 bits per function.
    pub DIP [
        F0 OFFSET(0) NUMBITS(4) [],
        F1 OFFSET(4) NUMBITS(4) [],
        F2 OFFSET(8) NUMBITS(4) [],
        F3 OFFSET(12) NUMBITS(4) [],
        F4 OFFSET(16) NUMBITS(4) [],
        F5 OFFSET(20) NUMBITS(4) [],
        F6 OFFSET(24) NUMBITS(4) [],
        F7 OFFSET(28) NUMBITS(4) []
    ],
    /// Primary Reset Status (`PRSTS`, RCBA `0x3310`).
    pub PRSTS [
        ME_WAKE OFFSET(0) NUMBITS(1) [],
        ME_HOST_POWERDOWN OFFSET(4) NUMBITS(1) [],
        ME_HRST_COLD OFFSET(5) NUMBITS(1) []
    ],
    /// PMSYNC Configuration (`PMSYNC_CONFIG`, RCBA `0x33c4`).
    pub PMSYNC_CONFIG [
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    /// RTC Configuration (`RC`, RCBA `0x3400`).
    pub RC [
        UPPER_128_ENABLE OFFSET(2) NUMBITS(1) []
    ],
    /// HPET Configuration (`HPTC`, RCBA `0x3404`).
    pub HPTC [
        ADDRESS_SELECT OFFSET(0) NUMBITS(2) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    /// General Control and Status (`GCS`, RCBA `0x3410`).
    pub GCS [
        /// BIOS Interface Lockdown.
        BILD OFFSET(0) NUMBITS(1) [],
        /// Route port 80h cycles to PCI (clear: LPC).
        RPR OFFSET(2) NUMBITS(1) [],
        NO_REBOOT OFFSET(5) NUMBITS(1) []
    ],
    /// Backed Up Control (`BUC`, RCBA `0x3414`).
    pub BUC [
        GBE_DISABLE OFFSET(5) NUMBITS(1) []
    ],
    /// Function Disable 1 (`FD`, RCBA `0x3418`).
    pub FD [
        BIOS_MUST_WRITE OFFSET(0) NUMBITS(1) [],
        ADSP OFFSET(1) NUMBITS(1) [],
        SATA1 OFFSET(2) NUMBITS(1) [],
        SMBUS OFFSET(3) NUMBITS(1) [],
        HDA OFFSET(4) NUMBITS(1) [],
        EHCI2 OFFSET(13) NUMBITS(1) [],
        LPC OFFSET(14) NUMBITS(1) [],
        EHCI1 OFFSET(15) NUMBITS(1) [],
        PCIE OFFSET(16) NUMBITS(8) [],
        THERMAL OFFSET(24) NUMBITS(1) [],
        SATA2 OFFSET(25) NUMBITS(1) [],
        XHCI OFFSET(27) NUMBITS(1) []
    ],
    /// Function Disable 2 (`FD2`, RCBA `0x3428`).
    pub FD2 [
        DBDF OFFSET(0) NUMBITS(1) [],
        MEI1 OFFSET(1) NUMBITS(1) [],
        MEI2 OFFSET(2) NUMBITS(1) [],
        IDER OFFSET(3) NUMBITS(1) [],
        KT OFFSET(4) NUMBITS(1) []
    ],
    /// LPC Final Configuration Lock (RCBA `0x3a6c`).
    pub LPC_FINAL_LOCK [
        LOCK OFFSET(0) NUMBITS(1) []
    ],
    /// HPET General Configuration (`GEN_CONF`, HPET `+0x10`).
    pub HPET_GC [
        ENABLE OFFSET(0) NUMBITS(1) []
    ],
    /// MEI Host General Status / DRAM Init Done (`PCI_ME_H_GS`, PCI `0x4c`).
    pub MEI_DID [
        /// MESEG_BASE[35:20] in 1 MiB units.
        BASE OFFSET(0) NUMBITS(16) [],
        STATUS OFFSET(24) NUMBITS(4) [],
        COMMAND OFFSET(28) NUMBITS(4) []
    ],
    /// Lynx Point-LP GPIO `GP_n_CONFIG0` (`GPIOBASE + 0x100 + 8*n`).
    pub LP_GPIO_CONF0 [
        GPIO OFFSET(0) NUMBITS(1) [],
        INPUT OFFSET(2) NUMBITS(1) [],
        INVERT OFFSET(3) NUMBITS(1) [],
        LEVEL_IRQ OFFSET(4) NUMBITS(1) [],
        RX OFFSET(30) NUMBITS(1) [],
        TX OFFSET(31) NUMBITS(1) []
    ],
    /// Lynx Point-LP GPIO `GP_n_CONFIG1` (`GPIOBASE + 0x104 + 8*n`).
    pub LP_GPIO_CONF1 [
        PULL OFFSET(0) NUMBITS(2) [],
        SENSE_DISABLE OFFSET(2) NUMBITS(1) []
    ],
    /// Serial IO `PPR_RESETS` (`BAR0 + 0x804`).
    pub SERIAL_IO_RESET [
        FUNC_RESET OFFSET(0) NUMBITS(2) []
    ],
    /// Serial IO `PPR_GEN` (`BAR0 + 0x808`).
    pub SERIAL_IO_GENERAL [
        LTR_MODE OFFSET(2) NUMBITS(1) [],
        VOLT_1V8 OFFSET(3) NUMBITS(1) []
    ]
];

pci_type0_config! {
    /// Lynx Point LPC bridge (00:1f.0).
    pub struct LynxPointLpcConfig {
        (0x40 => pub pmbase: MmioReadWrite<u32>),
        (0x44 => pub acpi_cntl: MmioReadWrite<u8, ACPI_CNTL::Register>),
        (0x45 => _lpc_rsvd0),
        (0x48 => pub gpiobase: MmioReadWrite<u32>),
        (0x4c => pub gpio_cntl: MmioReadWrite<u8, GPIO_CNTL::Register>),
        (0x4d => _lpc_rsvd1),
        (0x60 => pub pirq_a_d: [MmioReadWrite<u8>; 4]),
        (0x64 => pub serirq_cntl: MmioReadWrite<u8, SERIRQ_CNTL::Register>),
        (0x65 => _lpc_rsvd2),
        (0x68 => pub pirq_e_h: [MmioReadWrite<u8>; 4]),
        /// IOAPIC Bus/Device/Function (`IBDF`).
        (0x6c => pub ibdf: MmioReadWrite<u16>),
        (0x6e => _lpc_rsvd3),
        /// HPET 0..7 Bus/Device/Function (`HnBDF`).
        (0x70 => pub hbdf: [MmioReadWrite<u16>; 8]),
        (0x80 => pub lpc_io_dec: MmioReadWrite<u16>),
        (0x82 => pub lpc_en: MmioReadWrite<u16, LPC_EN::Register>),
        (0x84 => pub gen_dec: [MmioReadWrite<u32>; 4]),
        (0x94 => _lpc_rsvd4),
        (0xa0 => pub gen_pmcon_1: MmioReadWrite<u16, GEN_PMCON_1::Register>),
        (0xa2 => pub gen_pmcon_2: MmioReadWrite<u8, GEN_PMCON_2::Register>),
        (0xa3 => _lpc_rsvd5),
        (0xa4 => pub gen_pmcon_3: MmioReadWrite<u16, GEN_PMCON_3::Register>),
        (0xa6 => pub gen_pmcon_lock: MmioReadWrite<u8, GEN_PMCON_LOCK::Register>),
        (0xa7 => _lpc_rsvd6),
        (0xa9 => pub cir4: MmioReadWrite<u8>),
        (0xaa => _lpc_rsvd7),
        (0xac => pub etr3: MmioReadWrite<u32, ETR3::Register>),
        (0xb0 => _lpc_rsvd8),
        (0xb8 => pub gpio_rout: MmioReadWrite<u32>),
        (0xbc => _lpc_rsvd9),
        (0xdc => pub bios_cntl: MmioReadWrite<u8>),
        (0xdd => _lpc_rsvd10),
        (0xf0 => pub rcba: MmioReadWrite<u32, RCBA::Register>),
        (0xf4 => @END),
    }
}

register_structs! {
    /// Named Lynx Point Root Complex registers (`RCBA`, 16 KiB).
    pub RcbaRegs {
        (0x0000 => _reserved0),
        (0x0050 => pub tclockdn: MmioReadWrite<u32, TCLOCKDN::Register>),
        (0x0054 => _reserved1),
        (0x0404 => pub rpfn: MmioReadWrite<u32>),
        (0x0408 => _reserved2),
        /// Root Port Clock Request Grant (`RPC`).
        (0x103c => pub rpc: MmioReadWrite<u16>),
        (0x103e => _reserved3),
        /// Private virtual channel resource control (`VCpCTL`), checked by HDA init.
        (0x2030 => pub vcpctl: MmioReadWrite<u32>),
        (0x2034 => _reserved4),
        (0x2088 => pub dmi_2088: MmioReadWrite<u32>),
        (0x208c => _reserved5),
        (0x20ac => pub dmi_20ac: MmioReadWrite<u32>),
        (0x20b0 => _reserved6),
        /// DMI link configuration (`LCFG`, R/WO).
        (0x21a4 => pub dmi_lcfg: MmioReadWrite<u32>),
        /// DMI link control (`LCTL`).
        (0x21a8 => pub dmi_lctl: MmioReadWrite<u16>),
        (0x21aa => _reserved7),
        (0x2324 => pub dmi_2324: MmioReadWrite<u32>),
        (0x2328 => _reserved8),
        /// IOBP register address (`IOBPIRI`).
        (0x2330 => pub iobp_addr: MmioReadWrite<u32>),
        /// IOBP data (`IOBPD`).
        (0x2334 => pub iobp_data: MmioReadWrite<u32>),
        /// IOBP control and status (`IOBPS`).
        (0x2338 => pub iobp_stat: MmioReadWrite<u16, IOBP_STATUS::Register>),
        /// IOBP byte enables (`IOBPBE`).
        (0x233a => pub iobp_be: MmioReadWrite<u16>),
        (0x233c => _reserved9),
        (0x2340 => pub dmi_2340: MmioReadWrite<u32>),
        (0x2344 => _reserved10),
        (0x3100 => pub d31ip: MmioReadWrite<u32, DIP::Register>),
        (0x3104 => pub d30ip: MmioReadWrite<u32, DIP::Register>),
        (0x3108 => pub d29ip: MmioReadWrite<u32, DIP::Register>),
        (0x310c => pub d28ip: MmioReadWrite<u32, DIP::Register>),
        (0x3110 => pub d27ip: MmioReadWrite<u32, DIP::Register>),
        (0x3114 => pub d26ip: MmioReadWrite<u32, DIP::Register>),
        (0x3118 => pub d25ip: MmioReadWrite<u32, DIP::Register>),
        (0x311c => _reserved11),
        (0x3124 => pub d22ip: MmioReadWrite<u32, DIP::Register>),
        (0x3128 => pub d20ip: MmioReadWrite<u32, DIP::Register>),
        (0x312c => _reserved12),
        (0x3140 => pub d31ir: MmioReadWrite<u16, DIR::Register>),
        (0x3142 => pub d30ir: MmioReadWrite<u16, DIR::Register>),
        (0x3144 => pub d29ir: MmioReadWrite<u16, DIR::Register>),
        (0x3146 => pub d28ir: MmioReadWrite<u16, DIR::Register>),
        (0x3148 => pub d27ir: MmioReadWrite<u16, DIR::Register>),
        (0x314a => _reserved13),
        (0x314c => pub d26ir: MmioReadWrite<u16, DIR::Register>),
        (0x314e => _reserved14),
        (0x3150 => pub d25ir: MmioReadWrite<u16, DIR::Register>),
        (0x3152 => _reserved15),
        (0x3158 => pub d23ir: MmioReadWrite<u16, DIR::Register>),
        (0x315a => _reserved16),
        (0x315c => pub d22ir: MmioReadWrite<u16, DIR::Register>),
        (0x315e => _reserved17),
        (0x3160 => pub d20ir: MmioReadWrite<u16, DIR::Register>),
        (0x3162 => _reserved18),
        (0x3164 => pub d21ir: MmioReadWrite<u16, DIR::Register>),
        (0x3166 => _reserved19),
        (0x3168 => pub d19ir: MmioReadWrite<u16, DIR::Register>),
        (0x316a => _reserved20),
        /// Serial IO interrupt control (`RCBA+0x31e0`).
        (0x31e0 => pub sio_irq_ctrl: MmioReadWrite<u32>),
        (0x31e4 => _reserved21),
        (0x31fe => pub oic: MmioReadWrite<u16, OIC::Register>),
        (0x3200 => _reserved22),
        (0x3310 => pub prsts: MmioReadWrite<u32, PRSTS::Register>),
        (0x3314 => _reserved23),
        (0x333c => pub pm_333c: MmioReadWrite<u32>),
        (0x3340 => _reserved24),
        (0x33c4 => pub pmsync_config: MmioReadWrite<u32, PMSYNC_CONFIG::Register>),
        (0x33c8 => _reserved25),
        (0x3400 => pub rc: MmioReadWrite<u32, RC::Register>),
        (0x3404 => pub hptc: MmioReadWrite<u32, HPTC::Register>),
        (0x3408 => _reserved26),
        (0x3410 => pub gcs: MmioReadWrite<u32, GCS::Register>),
        (0x3414 => pub buc: MmioReadWrite<u32, BUC::Register>),
        (0x3418 => pub fd: MmioReadWrite<u32, FD::Register>),
        (0x341c => pub cg: MmioReadWrite<u32>),
        (0x3420 => pub fdsw: MmioReadWrite<u8, FDSW::Register>),
        (0x3421 => _reserved27),
        (0x3424 => pub dispbdf: MmioReadWrite<u16>),
        (0x3426 => _reserved28),
        (0x3428 => pub fd2: MmioReadWrite<u32, FD2::Register>),
        (0x342c => _reserved29),
        (0x35b0 => pub rmhwkctl: MmioReadWrite<u32>),
        (0x35b4 => _reserved30),
        (0x3804 => pub hsfs: MmioReadWrite<u16, HSFS::Register>),
        (0x3806 => _reserved31),
        /// SPIBAR+0x93 (Flash softstrap / VSCC option byte).
        (0x3893 => pub spi_opt: MmioReadWrite<u8>),
        (0x3894 => _reserved32),
        /// Flash Descriptor Observability Control (`FDOC`, SPIBAR+0xb0).
        (0x38b0 => pub fdoc: MmioReadWrite<u32>),
        /// Flash Descriptor Observability Data (`FDOD`, SPIBAR+0xb4).
        (0x38b4 => pub fdod: MmioReadOnly<u32>),
        (0x38b8 => _reserved33),
        /// Soft Reset Control (`SPIBAR+0xf4`).
        (0x38f4 => pub soft_reset_ctrl: MmioReadWrite<u32>),
        /// Soft Reset Data (`SPIBAR+0xf8`).
        (0x38f8 => pub soft_reset_data: MmioReadWrite<u32>),
        (0x38fc => _reserved34),
        (0x3a6c => pub lpc_final_lock: MmioReadWrite<u32, LPC_FINAL_LOCK::Register>),
        (0x3a70 => _reserved35),
        (0x4000 => @END),
    },
    /// Thermal subsystem temporary BAR (`TBARB`, 256 bytes).
    pub ThermalMmio {
        (0x00 => _reserved0),
        /// Thermal Sensor Status (`TSS`).
        (0x06 => pub tss: MmioReadWrite<u8>),
        (0x07 => _reserved1),
        /// Thermal Sensor Reporting Enable Lock (`TSREL`).
        (0x0a => pub tsrel: MmioReadWrite<u8>),
        (0x0b => _reserved2),
        /// Catastrophic Trip Point (`CTT`).
        (0x10 => pub ctt: MmioReadWrite<u16>),
        (0x12 => _reserved3),
        /// Thermal Alert Status (`TAS`).
        (0x80 => pub tas: MmioReadWrite<u8>),
        (0x81 => _reserved4),
        /// Thermal Sensor PCI Interrupt Enable (`TSPIEN`).
        (0x82 => pub tspien: MmioReadWrite<u8>),
        (0x83 => _reserved5),
        /// Thermal Sensor GP Event Enable (`TSGPEN`).
        (0x84 => pub tsgpen: MmioReadWrite<u8>),
        (0x85 => _reserved6),
        (0x100 => @END),
    },
    /// HPET MMIO register block (`0xfed0_0000`).
    pub HpetMmio {
        (0x00 => pub gcid: MmioReadOnly<u32>),
        (0x04 => _reserved0),
        (0x10 => pub gcfg: MmioReadWrite<u32, HPET_GC::Register>),
        (0x14 => _reserved1),
        (0x20 => @END),
    },
    /// Serial IO I2C/SPI/UART controller BAR0 registers.
    pub SerialIoMmio {
        (0x000 => _reserved0),
        (0x800 => pub clocks: MmioReadWrite<u32>),
        (0x804 => pub resets: MmioReadWrite<u32, SERIAL_IO_RESET::Register>),
        (0x808 => pub general: MmioReadWrite<u32, SERIAL_IO_GENERAL::Register>),
        (0x80c => _reserved1),
        (0x814 => pub ltr_ctrl: MmioReadWrite<u32>),
        (0x818 => _reserved2),
        (0x820 => @END),
    },
    /// Serial IO SDIO controller BAR0 registers.
    pub SdioMmio {
        (0x0000 => _reserved0),
        (0x003c => pub host_ctrl2: MmioReadWrite<u32>),
        (0x0040 => _reserved1),
        (0x1008 => pub cap_bypass_ctrl: MmioReadWrite<u32>),
        (0x100c => _reserved2),
        (0x1010 => pub cap_bypass_reg1: MmioReadWrite<u32>),
        (0x1014 => _reserved3),
        (0x1020 => @END),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    #[test]
    fn register_layouts_match_lynxpoint_eds_offsets() {
        assert_eq!(size_of::<RcbaRegs>(), 0x4000);
        assert_eq!(size_of::<LynxPointLpcConfig>(), 0xf4);
        assert_eq!(size_of::<ThermalMmio>(), 0x100);
        assert_eq!(size_of::<HpetMmio>(), 0x20);
        assert_eq!(size_of::<SerialIoMmio>(), 0x820);
        assert_eq!(size_of::<SdioMmio>(), 0x1020);

        assert_eq!(offset_of!(RcbaRegs, tclockdn), 0x0050);
        assert_eq!(offset_of!(RcbaRegs, rpfn), 0x0404);
        assert_eq!(offset_of!(RcbaRegs, iobp_addr), 0x2330);
        assert_eq!(offset_of!(RcbaRegs, iobp_stat), 0x2338);
        assert_eq!(offset_of!(RcbaRegs, d28ir), 0x3146);
        assert_eq!(offset_of!(RcbaRegs, d19ir), 0x3168);
        assert_eq!(offset_of!(RcbaRegs, sio_irq_ctrl), 0x31e0);
        assert_eq!(offset_of!(RcbaRegs, oic), 0x31fe);
        assert_eq!(offset_of!(RcbaRegs, pmsync_config), 0x33c4);
        assert_eq!(offset_of!(RcbaRegs, fd), 0x3418);
        assert_eq!(offset_of!(RcbaRegs, fdsw), 0x3420);
        assert_eq!(offset_of!(RcbaRegs, fd2), 0x3428);
        assert_eq!(offset_of!(RcbaRegs, rmhwkctl), 0x35b0);
        assert_eq!(offset_of!(RcbaRegs, hsfs), 0x3804);
        assert_eq!(offset_of!(RcbaRegs, lpc_final_lock), 0x3a6c);

        assert_eq!(offset_of!(LynxPointLpcConfig, ibdf), 0x6c);
        assert_eq!(offset_of!(LynxPointLpcConfig, hbdf), 0x70);
        assert_eq!(offset_of!(LynxPointLpcConfig, gen_pmcon_1), 0xa0);
        assert_eq!(offset_of!(LynxPointLpcConfig, etr3), 0xac);
        assert_eq!(offset_of!(LynxPointLpcConfig, bios_cntl), 0xdc);

        assert_eq!(offset_of!(ThermalMmio, tss), 0x06);
        assert_eq!(offset_of!(ThermalMmio, tsrel), 0x0a);
        assert_eq!(offset_of!(ThermalMmio, ctt), 0x10);
        assert_eq!(offset_of!(ThermalMmio, tas), 0x80);
        assert_eq!(offset_of!(ThermalMmio, tspien), 0x82);
        assert_eq!(offset_of!(ThermalMmio, tsgpen), 0x84);
        assert_eq!(offset_of!(LynxPointLpcConfig, rcba), 0xf0);

        assert_eq!(FD::EHCI2::SET.value, 1 << 13);
        assert_eq!(FD::EHCI1::SET.value, 1 << 15);
        assert_eq!(FD::SATA2::SET.value, 1 << 25);
        assert_eq!(FD::XHCI::SET.value, 1 << 27);
        assert_eq!(GEN_PMCON_1::SMI_LOCK::SET.value, 1 << 4);
    }
}
