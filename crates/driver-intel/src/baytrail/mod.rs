//! Intel Bay Trail (Atom E3800, Celeron/Pentium N2000/J1000 and Z3000)
//! SoC support, ported from coreboot's `soc/intel/baytrail` with its native
//! DDR3 RAM init.
//!
//! The SoC splits into a north cluster ([`north`]: the IOSF transaction
//! router, memory controller, graphics and SMRAM) and a south cluster
//! ([`south`]: the PCU's LPC, PMC, SMBus, SPI and GPIO plus the PCI
//! functions behind it). The Intel platform flow binds them as its
//! northbridge and southbridge. Units without PCI configuration space are
//! reached through the IOSF sideband ([`iosf`]).

#[cfg(feature = "acpi")]
mod acpi;
pub mod bunit;
pub mod devices;
pub mod gpio;
pub mod iosf;
pub mod irq;
pub mod lpc;
pub mod north;
pub mod pmc;
pub mod punit;
pub mod raminit;
pub mod smi;
pub mod south;
pub mod txe;

pub use north::{BaytrailNorth, NorthConfig};
pub use south::{BaytrailSouth, SouthConfig};

/// ECAM: 256 buses at 0xe0000000.
pub const ECAM_BASE: u64 = 0xe000_0000;
pub const ECAM_BUSES: u16 = 256;
/// Transactions in this range abort.
pub const ABORT_BASE: u32 = 0xfeb0_0000;
pub const ABORT_SIZE: u32 = 0x0010_0000;
/// PMC MMIO (PBASE).
pub const PMC_BASE: u32 = 0xfed0_3000;
pub const PMC_SIZE: u32 = 0x400;
/// GPIO pad MMIO (IOBASE).
pub const IO_BASE: u32 = 0xfed0_c000;
pub const IO_SIZE: u32 = 0x4000;
/// Intel legacy block (IBASE): PIRQ routing, ACTL, 8254/8259/HPET.
pub const ILB_BASE: u32 = 0xfed0_8000;
pub const ILB_SIZE: u32 = 0x400;
/// SPI controller (SBASE).
pub const SPI_BASE: u32 = 0xfed0_1000;
pub const SPI_SIZE: u32 = 0x400;
/// Mod PHY (MPBASE).
pub const MPHY_BASE: u32 = 0xfef0_0000;
pub const MPHY_SIZE: u32 = 0x0010_0000;
/// P-unit (PUBASE).
pub const PUNIT_BASE: u32 = 0xfed0_5000;
pub const PUNIT_SIZE: u32 = 0x800;
/// Root complex base (RCBA).
pub const RCBA_BASE: u32 = 0xfed1_c000;
pub const RCBA_SIZE: u32 = 0x400;
/// Scratch MMIO for BARs firmware maps temporarily.
pub const TEMP_BASE: u32 = 0xfd00_0000;
/// IOAPIC, and the ID firmware gives it.
pub const IOAPIC_BASE: u32 = 0xfec0_0000;
pub const IOAPIC_ID: u8 = 2;
/// HPET.
pub const HPET_BASE: u32 = 0xfed0_0000;
/// ACPI I/O (ABASE).
pub const ACPI_BASE: u16 = 0x0400;
pub const ACPI_SIZE: u16 = 0x80;
/// Legacy GPIO I/O (GBASE).
pub const GPIO_BASE: u16 = 0x0500;
pub const GPIO_SIZE: u16 = 0x100;
/// SMBus host I/O.
pub const SMBUS_BASE: u16 = 0xefa0;
/// TSEG size: 8 MiB.
pub const TSEG_MB: u32 = 8;

/// Bus 0 device numbers.
pub mod dev {
    pub const SOC: u8 = 0x00;
    pub const GFX: u8 = 0x02;
    pub const MIPI: u8 = 0x03;
    pub const MMC: u8 = 0x10;
    pub const SDIO: u8 = 0x11;
    pub const SD: u8 = 0x12;
    pub const SATA: u8 = 0x13;
    pub const XHCI: u8 = 0x14;
    pub const LPE: u8 = 0x15;
    pub const MMC45: u8 = 0x17;
    /// Serial IO 1: DMA (0) and I2C 1-7 (functions 1-7).
    pub const SIO1: u8 = 0x18;
    pub const TXE: u8 = 0x1a;
    pub const HDA: u8 = 0x1b;
    /// PCIe root ports 1-4 (functions 0-3).
    pub const PCIE: u8 = 0x1c;
    pub const EHCI: u8 = 0x1d;
    /// Serial IO 2: DMA (0), PWM 1/2, HS-UART 1/2, SPI (functions 1-5).
    pub const SIO2: u8 = 0x1e;
    /// Platform controller unit: LPC (0) and SMBus (3).
    pub const PCU: u8 = 0x1f;
}

/// Device IDs.
pub mod did {
    pub const SOC: u16 = 0x0f00;
    pub const GFX: u16 = 0x0f31;
    pub const MMC: u16 = 0x0f14;
    pub const SDIO: u16 = 0x0f15;
    pub const SD: u16 = 0x0f16;
    pub const IDE1: u16 = 0x0f20;
    pub const IDE2: u16 = 0x0f21;
    pub const AHCI1: u16 = 0x0f22;
    pub const AHCI2: u16 = 0x0f23;
    pub const XHCI: u16 = 0x0f35;
    pub const LPE: u16 = 0x0f28;
    pub const MMC45: u16 = 0x0f50;
    pub const TXE: u16 = 0x0f18;
    pub const HDA: u16 = 0x0f04;
    pub const EHCI: u16 = 0x0f34;
    pub const LPC: u16 = 0x0f1c;
    pub const SMBUS: u16 = 0x0f12;
}

/// Silicon stepping from the LPC revision ID: two revisions per metal
/// stepping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stepping {
    A0,
    A1,
    B0,
    B1,
    B2,
    B3,
    C0,
    D0,
    Unknown,
}

impl Stepping {
    pub const fn from_revision(rid: u8) -> Self {
        const STEPPINGS: [Stepping; 8] = [
            Stepping::A0,
            Stepping::A1,
            Stepping::B0,
            Stepping::B1,
            Stepping::B2,
            Stepping::B3,
            Stepping::C0,
            Stepping::D0,
        ];
        let index = if rid >= 0x11 {
            7 + (rid - 0x11) / 2
        } else if rid >= 0x0e {
            6 + (rid - 0x0e) / 2
        } else if rid >= 0x05 {
            2 + (rid - 0x05) / 2
        } else if rid >= 0x01 {
            (rid - 0x01) / 2
        } else {
            8
        };
        if (index as usize) < STEPPINGS.len() {
            STEPPINGS[index as usize]
        } else {
            Stepping::Unknown
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Stepping::A0 => "A0",
            Stepping::A1 => "A1",
            Stepping::B0 => "B0",
            Stepping::B1 => "B1",
            Stepping::B2 => "B2",
            Stepping::B3 => "B3",
            Stepping::C0 => "C0",
            Stepping::D0 => "D0",
            Stepping::Unknown => "??",
        }
    }
}

/// The stepping of this SoC.
pub fn stepping() -> Stepping {
    Stepping::from_revision(lpc::device().read8(0x08))
}

#[cfg(test)]
mod tests {
    use super::Stepping;

    #[test]
    fn revision_ids_map_to_metal_steppings() {
        assert_eq!(Stepping::from_revision(0x01), Stepping::A0);
        assert_eq!(Stepping::from_revision(0x0c), Stepping::B3);
        assert_eq!(Stepping::from_revision(0x0e), Stepping::C0);
        assert_eq!(Stepping::from_revision(0x11), Stepping::D0);
        assert_eq!(Stepping::from_revision(0x13), Stepping::Unknown);
    }
}
