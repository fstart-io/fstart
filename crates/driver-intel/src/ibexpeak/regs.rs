//! Intel 5 Series / 3400 Series (Ibex Peak) PCH register layouts.
//!
//! Offsets and fields follow coreboot `southbridge/intel/ibexpeak/pch.h`,
//! `southbridge/intel/common/{lpc_def,rcba}.h` and the 5 Series EDS.
//! Undocumented chipset-initialization values that coreboot writes as raw
//! tables stay raw in the driver; every named register is typed here.

use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use fstart_pci::pci_type0_config;
use tock_registers::{register_bitfields, register_structs};

/// Fixed PCH functions on bus 0.
pub mod dev {
    pub const XHCI: (u8, u8) = (0x14, 0);
    pub const MEI1: (u8, u8) = (0x16, 0);
    pub const MEI2: (u8, u8) = (0x16, 1);
    pub const IDER: (u8, u8) = (0x16, 2);
    pub const KT: (u8, u8) = (0x16, 3);
    pub const GBE: (u8, u8) = (0x19, 0);
    pub const EHCI2: (u8, u8) = (0x1a, 0);
    pub const HDA: (u8, u8) = (0x1b, 0);
    pub const PCIE: u8 = 0x1c;
    pub const EHCI1: (u8, u8) = (0x1d, 0);
    pub const PCI_BRIDGE: (u8, u8) = (0x1e, 0);
    pub const LPC: (u8, u8) = (0x1f, 0);
    pub const SATA1: (u8, u8) = (0x1f, 2);
    pub const SMBUS: (u8, u8) = (0x1f, 3);
    pub const SATA2: (u8, u8) = (0x1f, 5);
    pub const THERMAL: (u8, u8) = (0x1f, 6);
}

register_bitfields![u8,
    pub ACPI_CNTL [ ACPI_EN OFFSET(7) NUMBITS(1) [] ],
    pub GPIO_CNTL [ GPIO_EN OFFSET(4) NUMBITS(1) [] ],
    /// SERIRQ_CNTL.
    pub SERIRQ_CNTL [
        START_FRAME_WIDTH OFFSET(0) NUMBITS(2) [],
        FRAME_SIZE OFFSET(2) NUMBITS(4) [],
        CONTINUOUS OFFSET(6) NUMBITS(1) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    /// GEN_PMCON_2.
    pub GEN_PMCON_2 [
        /// Memory placed in self-refresh by the last S3 entry.
        MEM_SR OFFSET(5) NUMBITS(1) [],
        /// DRAM initialization in progress (firmware scratch, sticky).
        DRAM_INIT OFFSET(7) NUMBITS(1) []
    ],
    /// Function Disable SUS well.
    pub FDSW [ LOCK OFFSET(7) NUMBITS(1) [] ],
    /// GEN_PMCON_LOCK.
    pub GEN_PMCON_LOCK [
        ACPI_BASE_LOCK OFFSET(1) NUMBITS(1) [],
        SLP_STR_POL_LOCK OFFSET(2) NUMBITS(1) []
    ]
];

register_bitfields![u16,
    /// LPC_EN fixed decode enables.
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
    /// GEN_PMCON_1.
    pub GEN_PMCON_1 [
        SMI_RATE OFFSET(0) NUMBITS(2) [],
        CLKRUN_EN OFFSET(2) NUMBITS(1) [],
        /// Desktop only (EDS 13.8.1.1).
        PSEUDO_CLKRUN OFFSET(3) NUMBITS(1) [],
        SMI_LOCK OFFSET(4) NUMBITS(1) [],
        BIOS_PCI_EXP_EN OFFSET(10) NUMBITS(1) [],
        PWRBTN_LVL OFFSET(11) NUMBITS(1) []
    ],
    /// GEN_PMCON_3.
    pub GEN_PMCON_3 [
        AFTER_G3 OFFSET(0) NUMBITS(1) [],
        PWR_FLR OFFSET(1) NUMBITS(1) [],
        RTC_BATTERY_DEAD OFFSET(2) NUMBITS(1) [],
        SLP_S4_STRETCH_EN OFFSET(3) NUMBITS(1) [],
        SLP_S4_MIN OFFSET(4) NUMBITS(2) [],
        SLP_S3_MIN OFFSET(10) NUMBITS(2) [],
        SLP_STRETCH_SUS_DIS OFFSET(12) NUMBITS(1) []
    ]
];

register_bitfields![u32,
    /// RCBA register in LPC config space.
    pub RCBA [
        ENABLE OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(14) NUMBITS(18) []
    ],
    /// ETR3 (shares the PMIR offset with earlier ICHs).
    pub ETR3 [
        CWORWRE OFFSET(18) NUMBITS(1) [],
        CF9GR OFFSET(20) NUMBITS(1) [],
        CF9LOCK OFFSET(31) NUMBITS(1) []
    ],
    /// General Control and Status.
    pub GCS [
        /// BIOS Interface Lockdown.
        BILD OFFSET(0) NUMBITS(1) [],
        /// Route port 80h cycles to PCI (clear: LPC).
        RPR OFFSET(2) NUMBITS(1) [],
        NO_REBOOT OFFSET(5) NUMBITS(1) [],
        FERR_MUX OFFSET(6) NUMBITS(1) []
    ],
    /// RTC Configuration.
    pub RC [ UPPER_128_ENABLE OFFSET(2) NUMBITS(1) [] ],
    /// HPET Configuration.
    pub HPTC [
        ADDRESS_SELECT OFFSET(0) NUMBITS(2) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    /// Backed Up Control.
    pub BUC [ GBE_DISABLE OFFSET(5) NUMBITS(1) [] ],
    /// Function Disable 1.
    pub FD [
        BIOS_MUST_WRITE OFFSET(0) NUMBITS(1) [],
        P2P OFFSET(1) NUMBITS(1) [],
        SATA1 OFFSET(2) NUMBITS(1) [],
        SMBUS OFFSET(3) NUMBITS(1) [],
        HDA OFFSET(4) NUMBITS(1) [],
        /// UHCI functions, hidden behind the rate-matching hubs.
        UHCI OFFSET(5) NUMBITS(8) [],
        EHCI2 OFFSET(13) NUMBITS(1) [],
        LPC OFFSET(14) NUMBITS(1) [],
        EHCI1 OFFSET(15) NUMBITS(1) [],
        PCIE OFFSET(16) NUMBITS(8) [],
        THERMAL OFFSET(24) NUMBITS(1) [],
        SATA2 OFFSET(25) NUMBITS(1) [],
        ALWAYS OFFSET(26) NUMBITS(1) [],
        XHCI OFFSET(27) NUMBITS(1) [],
        /// Performance counter function (coreboot pch_default_disable).
        PERF OFFSET(28) NUMBITS(1) []
    ],
    /// Function Disable 2.
    pub FD2 [
        /// Display BDF enable.
        DBDF OFFSET(0) NUMBITS(1) [],
        MEI1 OFFSET(1) NUMBITS(1) [],
        MEI2 OFFSET(2) NUMBITS(1) [],
        IDER OFFSET(3) NUMBITS(1) [],
        KT OFFSET(4) NUMBITS(1) []
    ],
    /// TC Lockdown at RCBA+0x50.
    pub TCLOCKDN [ LOCK OFFSET(31) NUMBITS(1) [] ],
    /// Virtual channel resource control.
    pub VCTL [
        TC_MAP OFFSET(0) NUMBITS(8) [],
        ENABLE OFFSET(31) NUMBITS(1) []
    ],
    /// Per-port USB initialization (USBIR0..13): over-current/current table.
    pub USBIR [ CURRENT OFFSET(0) NUMBITS(12) [] ],
    /// Interrupt pin assignment for one device (DxxIP): 4 bits per function.
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
    /// Primary-reset status.
    pub PRSTS [
        ME_WAKE OFFSET(0) NUMBITS(1) [],
        ME_HOST_POWERDOWN OFFSET(4) NUMBITS(1) [],
        ME_HRST_COLD OFFSET(5) NUMBITS(1) []
    ],
    /// I/O trap low dword.
    pub IOTR_LO [
        TRSE OFFSET(0) NUMBITS(1) [],
        ADDRESS OFFSET(2) NUMBITS(14) [],
        ADDRESS_MASK OFFSET(18) NUMBITS(6) []
    ]
];

register_bitfields![u16,
    /// Interrupt route for one device (DxxIR): PIRQ per INTx pin.
    pub DIR [
        IAR OFFSET(0) NUMBITS(3) [],
        IBR OFFSET(4) NUMBITS(3) [],
        ICR OFFSET(8) NUMBITS(3) [],
        IDR OFFSET(12) NUMBITS(3) []
    ],
    /// Other Interrupt Control.
    pub OIC [
        APIC_RANGE OFFSET(0) NUMBITS(8) [],
        AEN OFFSET(8) NUMBITS(1) [],
        COPROCESSOR_ERROR OFFSET(9) NUMBITS(1) []
    ],
    /// Virtual channel status.
    pub VSTS [ NEGOTIATION_PENDING OFFSET(1) NUMBITS(1) [] ]
];

pci_type0_config! {
    /// Ibex Peak LPC bridge (00:1f.0).
    pub struct IbexPeakLpcConfig {
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
        (0x6c => _lpc_rsvd3),
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
        /// CIR4 (undocumented chipset initialization).
        (0xa9 => pub cir4: MmioReadWrite<u8>),
        (0xaa => _lpc_rsvd7),
        (0xac => pub etr3: MmioReadWrite<u32, ETR3::Register>),
        (0xb0 => _lpc_rsvd8),
        (0xb8 => pub gpio_rout: MmioReadWrite<u32>),
        (0xbc => _lpc_rsvd9),
        (0xf0 => pub rcba: MmioReadWrite<u32, RCBA::Register>),
        (0xf4 => @END),
    }
}

register_structs! {
    /// The named Ibex Peak Root Complex registers (RCBA, 16 KiB).
    pub RcbaRegs {
        (0x0000 => _reserved0),
        (0x0014 => pub v0ctl: MmioReadWrite<u32, VCTL::Register>),
        (0x0018 => _reserved1),
        (0x001a => pub v0sts: MmioReadOnly<u16, VSTS::Register>),
        (0x001c => _reserved2),
        (0x0020 => pub v1ctl: MmioReadWrite<u32, VCTL::Register>),
        (0x0024 => _reserved3),
        (0x0026 => pub v1sts: MmioReadOnly<u16, VSTS::Register>),
        (0x0028 => _reserved4),
        /// Private virtual channel (VCp) resource control.
        (0x0030 => pub vcpctl: MmioReadWrite<u32, VCTL::Register>),
        (0x0034 => _reserved5),
        /// ME virtual channel (VCm) resource control and status.
        (0x0040 => pub vcmctl: MmioReadWrite<u32, VCTL::Register>),
        (0x0044 => _reserved6),
        (0x0046 => pub vcmsts: MmioReadOnly<u16, VSTS::Register>),
        (0x0048 => _reserved7),
        (0x0050 => pub tclockdn: MmioReadWrite<u32, TCLOCKDN::Register>),
        (0x0054 => _reserved8),
        (0x1e00 => pub trsr: MmioReadWrite<u8>),
        (0x1e01 => _reserved9),
        (0x1e10 => pub trcr: MmioReadOnly<u32>),
        (0x1e14 => _reserved10),
        (0x1e18 => pub twdr: MmioReadOnly<u32>),
        (0x1e1c => _reserved11),
        (0x1e80 => pub iotr: [MmioReadWrite<u32>; 8]),
        (0x1ea0 => _reserved12),
        (0x3100 => pub d31ip: MmioReadWrite<u32, DIP::Register>),
        (0x3104 => pub d30ip: MmioReadWrite<u32, DIP::Register>),
        (0x3108 => pub d29ip: MmioReadWrite<u32, DIP::Register>),
        (0x310c => pub d28ip: MmioReadWrite<u32, DIP::Register>),
        (0x3110 => pub d27ip: MmioReadWrite<u32, DIP::Register>),
        (0x3114 => pub d26ip: MmioReadWrite<u32, DIP::Register>),
        (0x3118 => pub d25ip: MmioReadWrite<u32, DIP::Register>),
        (0x311c => _reserved13),
        (0x3124 => pub d22ip: MmioReadWrite<u32, DIP::Register>),
        (0x3128 => pub d20ip: MmioReadWrite<u32, DIP::Register>),
        (0x312c => _reserved14),
        (0x3140 => pub d31ir: MmioReadWrite<u16, DIR::Register>),
        (0x3142 => pub d30ir: MmioReadWrite<u16, DIR::Register>),
        (0x3144 => pub d29ir: MmioReadWrite<u16, DIR::Register>),
        (0x3146 => pub d28ir: MmioReadWrite<u16, DIR::Register>),
        (0x3148 => pub d27ir: MmioReadWrite<u16, DIR::Register>),
        (0x314a => _reserved15),
        (0x314c => pub d26ir: MmioReadWrite<u16, DIR::Register>),
        (0x314e => _reserved16),
        (0x3150 => pub d25ir: MmioReadWrite<u16, DIR::Register>),
        (0x3152 => _reserved17),
        (0x315c => pub d22ir: MmioReadWrite<u16, DIR::Register>),
        (0x315e => _reserved18),
        (0x3160 => pub d20ir: MmioReadWrite<u16, DIR::Register>),
        (0x3162 => _reserved19),
        (0x31fe => pub oic: MmioReadWrite<u16, OIC::Register>),
        (0x3200 => _reserved20),
        (0x3310 => pub prsts: MmioReadWrite<u32, PRSTS::Register>),
        (0x3314 => _reserved21),
        (0x3400 => pub rc: MmioReadWrite<u32, RC::Register>),
        (0x3404 => pub hptc: MmioReadWrite<u32, HPTC::Register>),
        (0x3408 => _reserved22),
        (0x3410 => pub gcs: MmioReadWrite<u32, GCS::Register>),
        (0x3414 => pub buc: MmioReadWrite<u32, BUC::Register>),
        (0x3418 => pub fd: MmioReadWrite<u32, FD::Register>),
        (0x341c => pub cg: MmioReadWrite<u32>),
        (0x3420 => pub fdsw: MmioReadWrite<u8, FDSW::Register>),
        (0x3421 => _reserved23),
        (0x3424 => pub dispbdf: MmioReadWrite<u16>),
        (0x3426 => _reserved24),
        (0x3428 => pub fd2: MmioReadWrite<u32, FD2::Register>),
        (0x342c => _reserved25),
        (0x3500 => pub usbir: [MmioReadWrite<u32, USBIR::Register>; 14]),
        (0x3538 => _reserved26),
        (0x3564 => pub usbirc: MmioReadWrite<u32>),
        (0x3568 => _reserved27),
        (0x3570 => pub usbira: MmioReadWrite<u32>),
        (0x3574 => _reserved28),
        (0x357c => pub usbirb: MmioReadWrite<u32>),
        (0x3580 => _reserved29),
        /// Rate-matching hub mode: zero exposes the EHCI functions at .0.
        (0x3598 => pub rmh_mode: MmioReadWrite<u32>),
        (0x359c => pub usbpdo: MmioReadWrite<u32>),
        (0x35a0 => pub usbocm1: MmioReadWrite<u32>),
        (0x35a4 => pub usbocm2: MmioReadWrite<u32>),
        (0x35a8 => _reserved30),
        (0x35b0 => pub rmhwkctl: MmioReadWrite<u32>),
        (0x35b4 => _reserved31),
        (0x4000 => @END),
    }
}

/// CIR (chipset initialization register) offsets coreboot writes from
/// `early_cir.c`; their fields are not documented.
pub mod cir {
    pub const CIR6: u32 = 0x2024;
    pub const CIR7: u32 = 0x3314;
    pub const CIR8: u32 = 0x3324;
    pub const CIR9: u32 = 0x3330;
    pub const CIR10: u32 = 0x3340;
    pub const CIR13: u32 = 0x3350;
    pub const CIR14: u32 = 0x3368;
    pub const CIR15: u32 = 0x3378;
    pub const CIR16: u32 = 0x3388;
    pub const CIR17: u32 = 0x33a0;
    pub const CIR18: u32 = 0x33a8;
    pub const CIR19: u32 = 0x33c0;
    pub const CIR20: u32 = 0x33cc;
    pub const CIR21: u32 = 0x33d0;
    pub const CIR22: u32 = 0x33d4;
}

/// PM I/O registers beyond the common ICH set.
pub mod pm {
    /// GPE0_STS (64-bit at PMBASE+0x20).
    pub const GPE0_STS: u16 = 0x20;
    /// GPE0_EN (64-bit at PMBASE+0x28).
    pub const GPE0_EN: u16 = 0x28;
    /// USB per-port registers write enable.
    pub const UPRWC: u16 = 0x3c;
    pub const UPRWC_WR_EN: u16 = 1 << 1;
    /// GPE0 TCO SCI status/enable.
    pub const TCOSCI: u32 = 1 << 6;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_registers_sit_at_their_eds_offsets() {
        assert_eq!(core::mem::size_of::<RcbaRegs>(), 0x4000);
        assert_eq!(core::mem::size_of::<IbexPeakLpcConfig>(), 0xf4);
        assert_eq!(FD::PCIE.val(1 << 3).value, 1 << 19);
        assert_eq!(FD2::MEI2::SET.value, 1 << 2);
    }
}
