//! The PCU's LPC bridge (00:1f.0): the base addresses of every fixed south
//! cluster block, the legacy UART, and the blocks behind them: the Intel
//! legacy block (PIRQ routing, SCI routing), RCBA and the SPI controller.

use super::*;
use fstart_core::mmio::MmioReadWrite;
use fstart_pci::ecam::EcamDevice;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::{register_bitfields, register_structs};

/// 00:1f.0.
pub fn address() -> PciAddress {
    PciAddress::new(0, 0, dev::PCU, 0)
}

// LPC configuration registers.
/// Revision ID (8 bits): the silicon stepping.
pub const REVID: u16 = 0x08;
/// ACPI I/O base (ABASE, 32 bits).
pub const ABASE: u16 = 0x40;
/// PMC MMIO base (PBASE, 32 bits).
pub const PBASE: u16 = 0x44;
/// Legacy GPIO I/O base (GBASE, 32 bits).
pub const GBASE: u16 = 0x48;
/// GPIO pad MMIO base (IOBASE, 32 bits).
pub const IOBASE: u16 = 0x4c;
/// Intel legacy block MMIO base (IBASE, 32 bits).
pub const IBASE: u16 = 0x50;
/// SPI controller MMIO base (SBASE, 32 bits).
pub const SBASE: u16 = 0x54;
/// ModPHY MMIO base (MPBASE, 32 bits).
pub const MPBASE: u16 = 0x58;
/// P-unit MMIO base (PUBASE, 32 bits).
pub const PUBASE: u16 = 0x5c;
/// UART control (32 bits).
pub const UART_CONT: u16 = 0x80;
/// Root complex base (RCBA, 32 bits).
pub const RCBA: u16 = 0xf0;

register_bitfields![u16,
    /// SPI `HSFSTS` (SBASE + 0x04, 16 bits): hardware sequencing status.
    pub HSFSTS [
        /// Flash configuration lock-down.
        FLOCKDN OFFSET(15) NUMBITS(1) []
    ]
];

register_bitfields![u32,
    /// LPC `PBASE`..`PUBASE` and `RCBA` (0x44..0x5c and 0xf0, 32 bits): a
    /// memory or I/O base; the enable is bit 1, bit 0 for RCBA.
    pub BASE [
        RCBA_ENABLE OFFSET(0) NUMBITS(1) [],
        ENABLE OFFSET(1) NUMBITS(1) []
    ],
    /// LPC `UART_CONT` (0x80, 32 bits).
    pub UART [
        /// Decode COM1 at 0x3f8.
        COM1_EN OFFSET(0) NUMBITS(1) []
    ],
    /// ILB `ACTL` (IBASE + 0x00, 32 bits): ACPI control.
    pub ACTL [
        /// SCI interrupt select.
        SCIS OFFSET(0) NUMBITS(3) [
            Irq9 = 0,
            Irq10 = 1,
            Irq11 = 2,
            Irq20 = 4,
            Irq21 = 5,
            Irq22 = 6,
            Irq23 = 7
        ]
    ],
    /// RCBA `GCS` (RCBA + 0x00, 32 bits): general control and status.
    pub GCS [
        /// BIOS interface lock-down.
        BILD OFFSET(0) NUMBITS(1) []
    ],
    /// SPI `LVSCC` (SBASE + 0xc4, 32 bits): lower vendor-specific
    /// component capabilities.
    pub LVSCC [
        /// Block/sector erase size.
        BES OFFSET(0) NUMBITS(2) [],
        /// 64-byte write granularity.
        WG_64_BYTE OFFSET(2) NUMBITS(1) [],
        /// Erase opcode.
        EO OFFSET(8) NUMBITS(8) [],
        /// Vendor component lock.
        VCL OFFSET(23) NUMBITS(1) []
    ],
    /// SPI `SCS` (SBASE + 0xf8, 32 bits): SMI control and status.
    pub SCS [
        /// SMI on a BIOS write-protect disable.
        SMIWPEN OFFSET(7) NUMBITS(1) []
    ],
    /// SPI `BCR` (SBASE + 0xfc, 32 bits): BIOS control.
    pub BCR [
        /// BIOS write protect disable.
        WPD OFFSET(0) NUMBITS(1) [],
        /// Lock enable.
        LE OFFSET(1) NUMBITS(1) [],
        /// SPI read configuration.
        SRC OFFSET(2) NUMBITS(2) [
            CacheNoPrefetch = 0,
            NoCacheNoPrefetch = 1,
            CachePrefetch = 2
        ],
        /// Enable InSMM.STS: BIOS writes only from SMM.
        EISS OFFSET(5) NUMBITS(1) []
    ]
];

register_structs! {
    /// The Intel legacy block at [`ILB_BASE`].
    pub IlbRegs {
        (0x00 => pub actl: MmioReadWrite<u32, ACTL::Register>),
        (0x04 => _reserved0),
        /// PIRQA-H PIC routing.
        (0x08 => pub pirq: [MmioReadWrite<u8>; 8]),
        (0x10 => _reserved1),
        /// Per-device INTA-D to PIRQ routing, indexed by PCI device.
        (0x20 => pub ir: [MmioReadWrite<u16>; 32]),
        (0x60 => @END),
    }
}

register_structs! {
    /// The SPI controller at [`SPI_BASE`].
    pub SpiRegs {
        (0x00 => _reserved0),
        (0x04 => pub hsfsts: MmioReadWrite<u16, HSFSTS::Register>),
        (0x06 => _reserved1),
        (0x94 => pub preop: MmioReadWrite<u16>),
        (0x96 => pub optype: MmioReadWrite<u16>),
        (0x98 => pub opmenu: [MmioReadWrite<u32>; 2]),
        (0xa0 => _reserved2),
        (0xc4 => pub lvscc: MmioReadWrite<u32, LVSCC::Register>),
        (0xc8 => pub uvscc: MmioReadWrite<u32>),
        (0xcc => _reserved3),
        (0xf8 => pub scs: MmioReadWrite<u32, SCS::Register>),
        (0xfc => pub bcr: MmioReadWrite<u32, BCR::Register>),
        (0x100 => @END),
    }
}

register_structs! {
    /// Root complex registers at [`RCBA_BASE`].
    pub RcbaRegs {
        (0x000 => pub gcs: MmioReadWrite<u32, GCS::Register>),
        (0x004 => _reserved0),
        (0x200 => pub usb: [MmioReadWrite<u32>; 3]),
        (0x20c => _reserved1),
        (0x220 => pub func_dis: MmioReadWrite<u32>),
        (0x224 => _reserved2),
        (0x240 => pub usb_240: MmioReadWrite<u32>),
        (0x244 => _reserved3),
        (0x284 => pub usb_284: MmioReadWrite<u32>),
        (0x288 => _reserved4),
        (0x318 => pub usb_318: MmioReadWrite<u32>),
        (0x31c => pub usb_31c: MmioReadWrite<u32>),
        (0x320 => @END),
    }
}

/// SPI opcode menu and vendor component lockdown, from the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiLockdown {
    pub preop: u16,
    pub optype: u16,
    pub opmenu: [u32; 2],
    pub lvscc: u32,
    pub uvscc: u32,
}

pub fn device() -> EcamDevice {
    EcamDevice::new_address(address())
}

pub fn ilb() -> &'static IlbRegs {
    // SAFETY: IBASE is programmed by `program_bases` and never moves.
    unsafe { &*(ILB_BASE as *const IlbRegs) }
}

pub fn spi() -> &'static SpiRegs {
    // SAFETY: SBASE is programmed by `program_bases` and never moves.
    unsafe { &*(SPI_BASE as *const SpiRegs) }
}

pub fn rcba() -> &'static RcbaRegs {
    // SAFETY: RCBA is programmed by `program_bases` and never moves.
    unsafe { &*(RCBA_BASE as *const RcbaRegs) }
}

/// Decode every fixed block (coreboot bootblock `program_base_addresses`).
/// Runs through CF8/CFC or ECAM.
pub fn program_bases<P: ConfigRegionAccess>(pci: &P) {
    let enable = BASE::ENABLE::SET.value;
    let bases = [
        (PBASE, PMC_BASE | enable),
        (IOBASE, IO_BASE | enable),
        (IBASE, ILB_BASE | enable),
        (SBASE, SPI_BASE | enable),
        (MPBASE, MPHY_BASE | enable),
        (PUBASE, PUNIT_BASE | enable),
        (RCBA, RCBA_BASE | BASE::RCBA_ENABLE::SET.value),
        (ABASE, u32::from(ACPI_BASE) | enable),
        (GBASE, u32::from(GPIO_BASE) | enable),
    ];
    for (reg, value) in bases {
        // SAFETY: the LPC bridge's own base registers.
        unsafe { pci.write(address(), reg, value) };
    }
}

/// Enable the PCU's legacy COM1 at 0x3f8 and mux its pads.
pub fn enable_com1<P: ConfigRegionAccess>(pci: &P) {
    // SAFETY: UART_CONT of the LPC bridge.
    unsafe { pci.write(address(), UART_CONT, UART::COM1_EN::SET.value) };
    gpio::Bank::Score.select_function(gpio::score_pad::UART_RXD, 1);
    gpio::Bank::Score.select_function(gpio::score_pad::UART_TXD, 1);
}

/// Halt the TCO watchdog.
pub fn tco_disable() {
    use crate::southbridge::pmio_ich as pmio;
    let tco = pmc::pm().tco();
    tco.write16(pmio::TCO1_CNT, tco.read16(pmio::TCO1_CNT) | 1 << 11);
}

/// Cache and prefetch BIOS reads, allow BIOS writes from outside SMM.
pub fn spi_init() {
    let spi = spi();
    spi.scs.modify(SCS::SMIWPEN::CLEAR);
    spi.bcr
        .modify(BCR::SRC::CachePrefetch + BCR::WPD::SET + BCR::EISS::CLEAR);
}

/// Lock the BIOS control and boot block configuration, and the SPI opcode
/// menu when the board supplies one (coreboot `finalize_chipset`).
pub fn lock_spi(lockdown: Option<&SpiLockdown>) {
    let spi = spi();
    spi.bcr.modify(BCR::LE::SET);
    rcba().gcs.modify(GCS::BILD::SET);
    if let Some(cfg) = lockdown {
        spi.preop.set(cfg.preop);
        spi.optype.set(cfg.optype);
        spi.opmenu[0].set(cfg.opmenu[0]);
        spi.opmenu[1].set(cfg.opmenu[1]);
        spi.hsfsts.modify(HSFSTS::FLOCKDN::SET);
        spi.uvscc.set(cfg.uvscc);
        spi.lvscc.set(cfg.lvscc | LVSCC::VCL::SET.value);
    }
}

/// The COM1 enable latched in the LPC bridge.
pub fn com1_enabled() -> bool {
    device().read32(UART_CONT) & UART::COM1_EN::SET.value != 0
}

/// Route SCI to IRQ 9.
pub fn route_sci_irq9() {
    ilb().actl.modify(ACTL::SCIS::Irq9);
}

/// The ABASE the LPC bridge decodes.
pub fn acpi_base() -> u16 {
    (device().read32(ABASE) & 0xfff8) as u16
}
