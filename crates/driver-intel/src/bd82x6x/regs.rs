//! Intel 6/7-series PCH (Cougar Point / Panther Point) registers: the LPC
//! bridge configuration and the root complex register block, after coreboot
//! `southbridge/intel/bd82x6x/pch.h` and `southbridge/intel/common`.

use crate::southbridge::spibar::SpiRegs;
use fstart_core::mmio::MmioReadWrite;
use fstart_pci::pci_type0_config;
use tock_registers::{register_bitfields, register_structs};

/// The PCH's bus-0 functions as `devfn`s.
pub mod dev {
    use fstart_pci::devfn;
    /// Panther Point only.
    pub const XHCI: u8 = devfn(0x14, 0);
    pub const MEI1: u8 = devfn(0x16, 0);
    pub const MEI2: u8 = devfn(0x16, 1);
    pub const IDER: u8 = devfn(0x16, 2);
    pub const KT: u8 = devfn(0x16, 3);
    pub const GBE: u8 = devfn(0x19, 0);
    pub const EHCI2: u8 = devfn(0x1a, 0);
    pub const HDA: u8 = devfn(0x1b, 0);
    /// The root ports' device; the function is the port.
    pub const PCIE: u8 = 0x1c;
    pub const EHCI1: u8 = devfn(0x1d, 0);
    pub const PCI_BRIDGE: u8 = devfn(0x1e, 0);
    pub const LPC: u8 = devfn(0x1f, 0);
    pub const SATA1: u8 = devfn(0x1f, 2);
    pub const SMBUS: u8 = devfn(0x1f, 3);
    pub const SATA2: u8 = devfn(0x1f, 5);
    pub const THERMAL: u8 = devfn(0x1f, 6);
}

register_bitfields![u16,
    pub GEN_PMCON_1 [
        SMI_RATE OFFSET(0) NUMBITS(2) [],
        CLKRUN_EN OFFSET(2) NUMBITS(1) [],
        PSEUDO_CLKRUN_EN OFFSET(3) NUMBITS(1) [],
        SMI_LOCK OFFSET(4) NUMBITS(1) [],
        BIOS_PCI_EXP_EN OFFSET(10) NUMBITS(1) [],
        BIT11 OFFSET(11) NUMBITS(1) []
    ],
    pub GEN_PMCON_3 [
        AFTER_G3 OFFSET(0) NUMBITS(1) [],
        RTC_POWER_FAILED OFFSET(1) NUMBITS(1) [],
        RTC_BATTERY_DEAD OFFSET(2) NUMBITS(1) [],
        SLP_S4_STRETCH_EN OFFSET(3) NUMBITS(1) [],
        SLP_S4_MIN OFFSET(4) NUMBITS(2) [],
        SLP_S3_MIN OFFSET(10) NUMBITS(2) [],
        DISABLE_SLP_STRETCH_SUS OFFSET(12) NUMBITS(1) []
    ]
];

register_bitfields![u8,
    pub GEN_PMCON_LOCK [
        ACPI_BASE_LOCK OFFSET(1) NUMBITS(1) [],
        SLP_STR_POL_LOCK OFFSET(2) NUMBITS(1) []
    ]
];

register_bitfields![u32,
    pub ETR3 [
        CWORWRE OFFSET(18) NUMBITS(1) [],
        CF9GR OFFSET(20) NUMBITS(1) [],
        CF9LOCK OFFSET(31) NUMBITS(1) []
    ],
    pub HPTC [
        ADDRESS_SELECT OFFSET(0) NUMBITS(2) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    pub GCS [
        BILD OFFSET(0) NUMBITS(1) [],
        /// POST cycles to PCI(e) instead of LPC.
        PCI_POST OFFSET(2) NUMBITS(1) [],
        NO_REBOOT OFFSET(5) NUMBITS(1) []
    ],
    pub FD [
        ALWAYS0 OFFSET(0) NUMBITS(1) [],
        P2P OFFSET(1) NUMBITS(1) [],
        SATA1 OFFSET(2) NUMBITS(1) [],
        SMBUS OFFSET(3) NUMBITS(1) [],
        HDA OFFSET(4) NUMBITS(1) [],
        EHCI2 OFFSET(13) NUMBITS(1) [],
        LPC OFFSET(14) NUMBITS(1) [],
        EHCI1 OFFSET(15) NUMBITS(1) [],
        PCIE OFFSET(16) NUMBITS(8) [],
        THERMAL OFFSET(24) NUMBITS(1) [],
        SATA2 OFFSET(25) NUMBITS(1) [],
        ALWAYS26 OFFSET(26) NUMBITS(1) [],
        XHCI OFFSET(27) NUMBITS(1) []
    ],
    pub FD2 [
        DBDF OFFSET(0) NUMBITS(1) [],
        MEI1 OFFSET(1) NUMBITS(1) [],
        MEI2 OFFSET(2) NUMBITS(1) [],
        IDER OFFSET(3) NUMBITS(1) [],
        KT OFFSET(4) NUMBITS(1) []
    ],
    pub VCTL [
        ENABLE OFFSET(31) NUMBITS(1) []
    ],
    pub CIR0 [
        TC_LOCKDOWN OFFSET(31) NUMBITS(1) []
    ]
];

/// USB per-port registers write control, PMBASE + 0x3c.
pub const UPRWC_OFFSET: u16 = 0x3c;

register_bitfields![u16,
    pub UPRWC [
        WR_EN OFFSET(1) NUMBITS(1) []
    ],
    pub VC_STS [
        NEGOTIATION_PENDING OFFSET(1) NUMBITS(1) []
    ],
    pub IOBPS [
        BUSY OFFSET(0) NUMBITS(1) [],
        STATUS OFFSET(1) NUMBITS(2) [],
        OPCODE OFFSET(8) NUMBITS(8) []
    ]
];

pci_type0_config! {
    /// LPC bridge, PCI 00:1f.0.
    pub struct LpcConfig {
        (0x40 => pub pmbase: MmioReadWrite<u32>),
        (0x44 => pub acpi_cntl: MmioReadWrite<u8>),
        (0x45 => _rsvd0),
        (0x48 => pub gpiobase: MmioReadWrite<u32>),
        (0x4c => pub gpio_cntl: MmioReadWrite<u8>),
        (0x4d => _rsvd1),
        (0x60 => pub pirq_abcd: [MmioReadWrite<u8>; 4]),
        (0x64 => pub serirq_cntl: MmioReadWrite<u8>),
        (0x65 => _rsvd2),
        (0x68 => pub pirq_efgh: [MmioReadWrite<u8>; 4]),
        (0x6c => pub ibdf: MmioReadWrite<u16>),
        (0x6e => _rsvd3),
        (0x70 => pub hpet_bdf: [MmioReadWrite<u16>; 8]),
        (0x80 => pub lpc_io_dec: MmioReadWrite<u16>),
        (0x82 => pub lpc_en: MmioReadWrite<u16>),
        (0x84 => pub gen_dec: [MmioReadWrite<u32>; 4]),
        (0x94 => _rsvd4),
        (0x98 => pub lgmr: MmioReadWrite<u32>),
        (0x9c => _rsvd5),
        (0xa0 => pub gen_pmcon_1: MmioReadWrite<u16, GEN_PMCON_1::Register>),
        (0xa2 => pub gen_pmcon_2: MmioReadWrite<u8>),
        (0xa3 => _rsvd6),
        (0xa4 => pub gen_pmcon_3: MmioReadWrite<u16, GEN_PMCON_3::Register>),
        (0xa6 => pub gen_pmcon_lock: MmioReadWrite<u8, GEN_PMCON_LOCK::Register>),
        (0xa7 => _rsvd7),
        (0xa9 => pub cxstate_cnf: MmioReadWrite<u8>),
        (0xaa => _rsvd8),
        (0xac => pub etr3: MmioReadWrite<u32, ETR3::Register>),
        (0xb0 => _rsvd9),
        (0xb8 => pub gpio_rout: MmioReadWrite<u32>),
        (0xbc => _rsvd10),
        (0xd8 => pub bios_dec_en1: MmioReadWrite<u32>),
        (0xdc => pub bios_cntl: MmioReadWrite<u8>),
        (0xdd => _rsvd11),
        (0xf0 => pub rcba: MmioReadWrite<u32>),
        (0xf4 => _rsvd12),
        (0x100 => @END),
    }
}

register_structs! {
    /// Root complex register block (RCBA, 16 KiB).
    pub Rcba {
        (0x0000 => _reserved0),
        (0x0050 => pub cir0: MmioReadWrite<u32, CIR0::Register>),
        (0x0054 => _reserved1),
        (0x0400 => pub rpc: MmioReadWrite<u32>),
        (0x0404 => pub rpfn: MmioReadWrite<u32>),
        (0x0408 => _reserved2),
        (0x080c => pub cir_80c: MmioReadWrite<u32>),
        (0x0810 => _reserved3),
        (0x0900 => pub cir2: MmioReadWrite<u16>),
        (0x0902 => _reserved4),
        (0x100c => pub cir_100c: MmioReadWrite<u32>),
        (0x1010 => _reserved5),
        (0x1100 => pub cir3: MmioReadWrite<u16>),
        (0x1102 => _reserved6),
        (0x1114 => pub updcr: MmioReadWrite<u8>),
        (0x1115 => _reserved7),
        (0x1e00 => pub trsr: MmioReadWrite<u32>),
        (0x1e04 => _reserved8),
        (0x1e10 => pub trcr: MmioReadWrite<u32>),
        (0x1e14 => _reserved9),
        (0x1e18 => pub twdr: MmioReadWrite<u32>),
        (0x1e1c => _reserved10),
        (0x1e80 => pub iotr: [MmioReadWrite<u32>; 8]),
        (0x1ea0 => _reserved11),
        (0x2014 => pub v0ctl: MmioReadWrite<u32>),
        (0x2018 => _reserved12),
        (0x201a => pub v0sts: MmioReadWrite<u16, VC_STS::Register>),
        (0x201c => _reserved13),
        (0x2020 => pub v1ctl: MmioReadWrite<u32>),
        (0x2024 => _reserved14),
        (0x2026 => pub v1sts: MmioReadWrite<u16, VC_STS::Register>),
        (0x2028 => _reserved15),
        (0x2030 => pub cir31: MmioReadWrite<u32>),
        (0x2034 => _reserved16),
        (0x2036 => pub vcpsts: MmioReadWrite<u16, VC_STS::Register>),
        (0x2038 => _reserved17),
        (0x2040 => pub cir32: MmioReadWrite<u32>),
        (0x2044 => _reserved18),
        (0x2046 => pub vcmsts: MmioReadWrite<u16, VC_STS::Register>),
        (0x2048 => _reserved19),
        (0x2088 => pub cir1: MmioReadWrite<u32>),
        (0x208c => _reserved20),
        (0x20ac => pub rec: MmioReadWrite<u32>),
        (0x20b0 => _reserved21),
        (0x21a4 => pub lcap: MmioReadWrite<u32>),
        (0x21a8 => pub lctl: MmioReadWrite<u16>),
        (0x21aa => pub lsts: MmioReadWrite<u16>),
        (0x21ac => _reserved22),
        (0x21b0 => pub dlctl2: MmioReadWrite<u32>),
        (0x21b4 => _reserved23),
        (0x2234 => pub dmic: MmioReadWrite<u32>),
        (0x2238 => pub cir30: MmioReadWrite<u32>),
        (0x223c => _reserved24),
        (0x228c => pub cir5: MmioReadWrite<u32>),
        (0x2290 => _reserved25),
        (0x2304 => pub dmc: MmioReadWrite<u32>),
        (0x2308 => _reserved26),
        (0x2310 => pub cir_2310: MmioReadWrite<u32>),
        (0x2314 => pub cir6: MmioReadWrite<u32>),
        (0x2318 => _reserved27),
        (0x2320 => pub cir9: MmioReadWrite<u32>),
        (0x2324 => pub dmc2: MmioReadWrite<u32>),
        (0x2328 => _reserved28),
        (0x2330 => pub iobpiri: MmioReadWrite<u32>),
        (0x2334 => pub iobpd: MmioReadWrite<u32>),
        (0x2338 => pub iobps: MmioReadWrite<u16, IOBPS::Register>),
        (0x233a => _reserved29),
        (0x2340 => pub cir_2340: MmioReadWrite<u32>),
        (0x2344 => pub cir_2344: MmioReadWrite<u32>),
        (0x2348 => _reserved30),
        (0x3100 => pub d31ip: MmioReadWrite<u32>),
        (0x3104 => pub d30ip: MmioReadWrite<u32>),
        (0x3108 => pub d29ip: MmioReadWrite<u32>),
        (0x310c => pub d28ip: MmioReadWrite<u32>),
        (0x3110 => pub d27ip: MmioReadWrite<u32>),
        (0x3114 => pub d26ip: MmioReadWrite<u32>),
        (0x3118 => pub d25ip: MmioReadWrite<u32>),
        (0x311c => _reserved31),
        (0x3124 => pub d22ip: MmioReadWrite<u32>),
        (0x3128 => pub d20ip: MmioReadWrite<u32>),
        (0x312c => _reserved32),
        (0x3140 => pub d31ir: MmioReadWrite<u16>),
        (0x3142 => pub d30ir: MmioReadWrite<u16>),
        (0x3144 => pub d29ir: MmioReadWrite<u16>),
        (0x3146 => pub d28ir: MmioReadWrite<u16>),
        (0x3148 => pub d27ir: MmioReadWrite<u16>),
        (0x314a => _reserved33),
        (0x314c => pub d26ir: MmioReadWrite<u16>),
        (0x314e => _reserved34),
        (0x3150 => pub d25ir: MmioReadWrite<u16>),
        (0x3152 => _reserved35),
        (0x315c => pub d22ir: MmioReadWrite<u16>),
        (0x315e => _reserved36),
        (0x3160 => pub d20ir: MmioReadWrite<u16>),
        (0x3162 => _reserved37),
        (0x31fe => pub oic: MmioReadWrite<u16>),
        (0x3200 => _reserved38),
        (0x3310 => pub prsts: MmioReadWrite<u32>),
        (0x3314 => pub cir7: MmioReadWrite<u32>),
        (0x3318 => pub pm_cfg: MmioReadWrite<u32>),
        (0x331c => _reserved39),
        (0x3324 => pub cir8: MmioReadWrite<u32>),
        (0x3328 => _reserved40),
        (0x3340 => pub cir10: MmioReadWrite<u32>),
        (0x3344 => pub cir11: MmioReadWrite<u32>),
        (0x3348 => _reserved41),
        (0x3360 => pub cir12: MmioReadWrite<u32>),
        (0x3364 => _reserved42),
        (0x3368 => pub cir14: MmioReadWrite<u32>),
        (0x336c => _reserved43),
        (0x3378 => pub cir15: MmioReadWrite<u32>),
        (0x337c => pub cir13: MmioReadWrite<u32>),
        (0x3380 => _reserved44),
        (0x3388 => pub cir16: MmioReadWrite<u32>),
        (0x338c => _reserved45),
        (0x3390 => pub cir18: MmioReadWrite<u32>),
        (0x3394 => _reserved46),
        (0x33a0 => pub cir17: MmioReadWrite<u32>),
        (0x33a4 => pub cir_33a4: MmioReadWrite<u32>),
        (0x33a8 => _reserved47),
        (0x33b0 => pub cir23: MmioReadWrite<u32>),
        (0x33b4 => _reserved48),
        (0x33c0 => pub cir19: MmioReadWrite<u32>),
        (0x33c4 => _reserved49),
        (0x33c8 => pub pmsync_cfg: MmioReadWrite<u32>),
        (0x33cc => pub cir20: MmioReadWrite<u32>),
        (0x33d0 => pub cir21: MmioReadWrite<u32>),
        (0x33d4 => pub cir22: MmioReadWrite<u32>),
        (0x33d8 => _reserved50),
        (0x3400 => pub rc: MmioReadWrite<u32>),
        (0x3404 => pub hptc: MmioReadWrite<u32, HPTC::Register>),
        (0x3408 => _reserved51),
        (0x3410 => pub gcs: MmioReadWrite<u32, GCS::Register>),
        (0x3414 => pub buc: MmioReadWrite<u8>),
        (0x3415 => _reserved52),
        (0x3418 => pub fd: MmioReadWrite<u32, FD::Register>),
        (0x341c => pub cg: MmioReadWrite<u32>),
        (0x3420 => pub fdsw: MmioReadWrite<u8>),
        (0x3421 => _reserved53),
        (0x3424 => pub dispbdf: MmioReadWrite<u16>),
        (0x3426 => _reserved54),
        (0x3428 => pub fd2: MmioReadWrite<u32, FD2::Register>),
        (0x342c => _reserved55),
        (0x3500 => pub usbir: [MmioReadWrite<u32>; 14]),
        (0x3538 => _reserved56),
        (0x3560 => pub usb_3560: MmioReadWrite<u32>),
        (0x3564 => pub usb_3564: MmioReadWrite<u32>),
        (0x3568 => _reserved57),
        (0x3570 => pub usb_3570: MmioReadWrite<u32>),
        (0x3574 => _reserved58),
        (0x357c => pub usb_357c: MmioReadWrite<u32>),
        (0x3580 => pub usb_3580: MmioReadWrite<u32>),
        (0x3584 => _reserved59),
        (0x3590 => pub miscctl: MmioReadWrite<u32>),
        (0x3594 => _reserved60),
        (0x3598 => pub usb_3598: MmioReadWrite<u32>),
        (0x359c => pub usbpdo: MmioReadWrite<u32>),
        (0x35a0 => pub usbocm1: MmioReadWrite<u32>),
        (0x35a4 => pub usbocm2: MmioReadWrite<u32>),
        (0x35a8 => _reserved61),
        (0x35b0 => pub rmhwkctl: MmioReadWrite<u32>),
        (0x35b4 => _reserved62),
        (0x36d4 => pub cir_36d4: MmioReadWrite<u32>),
        (0x36d8 => _reserved63),
        (0x3800 => pub spi: SpiRegs),
        (0x3a00 => _reserved64),
        (0x3a28 => pub cir24: MmioReadWrite<u32>),
        (0x3a2c => pub cir25: MmioReadWrite<u32>),
        (0x3a30 => _reserved65),
        (0x3a6c => pub cir26: MmioReadWrite<u32>),
        (0x3a70 => _reserved66),
        (0x3a80 => pub cir27: MmioReadWrite<u32>),
        (0x3a84 => pub cir28: MmioReadWrite<u32>),
        (0x3a88 => pub cir29: MmioReadWrite<u32>),
        (0x3a8c => _reserved67),
        /// coreboot clears bits 3:0 with a 32-bit access at this unaligned
        /// offset; the bits live in the low byte.
        (0x3f02 => pub cir_3f02: MmioReadWrite<u16>),
        (0x3f04 => _reserved68),
        (0x4000 => @END),
    }
}
