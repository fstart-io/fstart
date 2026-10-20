// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin ACPI tables.
//!
//! Interrupts: the FCH IOAPIC has GSI 0-23 and every die's NBIO IOAPIC 32
//! more, die n from 24 + 32n. The PCIe and internal bridges of a die
//! interrupt through its NBIO IOAPIC as the NBIO base table routes them; the
//! root bus devices (IOMMU, FCH SMBus/LPC) through the FCH INTA-D. The
//! `_PRT`s describe APIC mode only: the FCH PIC routing isn't programmed, so
//! there is nothing to describe for an OS in PIC mode.
extern crate alloc;

use super::BoardAcpi;
use alloc::{format, vec, vec::Vec};
use core::ops::RangeInclusive;
use fstart_acpi::{
    Aml, AmlError, AmlSink,
    aml::{AddressSpace, AddressSpaceCacheable, IO, Name, ResourceTemplate},
    aml_linker::{device_vec, scope_vec},
    fadt::{Flags, PmProfile},
    mcfg::MCFG,
    pirq::prt_name_aml,
    platform::{
        HpetConfig, IoApicConfig, IsoConfig, PlatformConfig, X86Config, X86Fadt,
        x86::IAPC_LEGACY_DEVICES,
    },
};
use fstart_acpi_macros::acpi_dsl;
use fstart_driver_amd::zeppelin::{MAX_DIES, SPI_BASE, acpi_io};

pub const LAPIC_BASE: u64 = 0xfee0_0000;
pub const FCH_IOAPIC_BASE: u32 = 0xfec0_0000;
/// The FCH IOAPIC has GSI 0; the NBIO IOAPICs follow from ID 1.
pub const FCH_IOAPIC_ID: u8 = 0;
pub const HPET_BASE: u64 = 0xfed0_0000;
const FCH_IOAPIC_PINS: u32 = 24;
const NBIO_IOAPIC_PINS: u32 = 32;
const SCI_IRQ: u8 = 9;
/// PCI configuration mechanism #1 ports, claimed by the root bridge of die 0.
const PCI_CFG_IO: RangeInclusive<u16> = 0xcf8..=0xcff;

/// First GSI of the NBIO IOAPIC of `die`.
pub const fn nbio_gsi_base(die: u8) -> u32 {
    FCH_IOAPIC_PINS + NBIO_IOAPIC_PINS * die as u32
}

/// What the platform set up on one die.
#[derive(Debug, Clone)]
pub struct DieAcpi<'a> {
    /// Root bus range decoded to the die.
    pub buses: RangeInclusive<u8>,
    /// ID and base the NBIO IOAPIC was programmed with (ID 1 + die).
    pub ioapic_id: u8,
    pub ioapic_base: u32,
    /// The I/O and MMIO windows PCI resources were allocated from (the DF
    /// windows of the die).
    pub io: &'a [RangeInclusive<u16>],
    pub mmio: &'a [RangeInclusive<u64>],
    /// The legacy VGA ranges are routed to this die.
    pub vga: bool,
    /// devfn of the enabled PCIe and internal (7.1, 8.1) bridges.
    pub bridges: &'a [u8],
}

pub struct Inputs<'a> {
    /// APIC IDs of the online CPUs.
    pub apic_ids: &'a [u32],
    /// The dies in order.
    pub dies: &'a [DieAcpi<'a>],
    pub ecam_base: u64,
    pub ecam_buses: u16,
    /// HPET general capabilities and ID register (0xfed00000).
    pub hpet_id: u32,
    /// IOAPIC GSIs of FCH INTA-D, as programmed in the PCI_INTR table.
    pub fch_intx_gsi: [u8; 4],
    pub board: &'a BoardAcpi,
}

/// Feed to `fstart_acpi::platform::assemble_into(.., &platform, &dsdt_aml, &extra)`.
pub struct Tables {
    pub platform: PlatformConfig,
    /// The DSDT content of `\_SB`.
    pub dsdt_aml: Vec<u8>,
    /// MCFG and the board tables.
    pub extra: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A bridge with no IOAPIC routing.
    UnknownBridge {
        die: u8,
        devfn: u8,
    },
    /// No bus or more than 256 buses in the ECAM window.
    EcamBuses,
    /// More dies than the package has.
    Dies,
    Aml(AmlError),
}
impl From<AmlError> for Error {
    fn from(error: AmlError) -> Self {
        Self::Aml(error)
    }
}

/// Swizzle of a bridge route: the INTx pin A-D maps to IOAPIC pin
/// `group * 4 + (swizzle + pin) % 4`.
const ABCD: u8 = 0;
const BCDA: u8 = 1;
const CDAB: u8 = 2;
const DABC: u8 = 3;

/// IOAPIC routing of a bridge.
#[derive(Debug, Clone, Copy)]
struct Route {
    devfn: u8,
    group: u8,
    swizzle: u8,
}
const fn route(device: u8, function: u8, group: u8, swizzle: u8) -> Route {
    Route {
        devfn: device << 3 | function,
        group,
        swizzle,
    }
}
impl Route {
    /// IOAPIC pin of INTx `pin` (0-3) behind the bridge (`pci_calculate_irq`).
    const fn irq(&self, pin: u8) -> u8 {
        self.group * 4 + (self.swizzle + pin) % 4
    }
}

/// In bridge order: what the NBIO base table
/// programs into SMN 0x14300040 + 4 * bridge (`irq << 16 | swizzle << 4 |
/// group`; the hardware swizzle k is the ACPI swizzle (4 - k) % 4).
const ROUTES: [Route; 18] = [
    route(1, 1, 0, ABCD),
    route(1, 2, 1, ABCD),
    route(1, 3, 2, ABCD),
    route(1, 4, 3, ABCD),
    route(1, 5, 4, ABCD),
    route(1, 6, 5, ABCD),
    route(1, 7, 6, ABCD),
    route(2, 1, 7, ABCD),
    route(3, 1, 7, CDAB),
    route(3, 2, 6, CDAB),
    route(3, 3, 5, CDAB),
    route(3, 4, 4, CDAB),
    route(3, 5, 3, CDAB),
    route(3, 6, 2, CDAB),
    route(3, 7, 1, CDAB),
    route(4, 1, 0, CDAB),
    // Internal bridges A (xHCI) and B (SATA, xGbE)
    route(7, 1, 3, DABC),
    route(8, 1, 4, BCDA),
];

/// Build the platform tables, DSDT content and extra tables.
pub fn build(inputs: &Inputs) -> Result<Tables, Error> {
    if inputs.dies.len() > MAX_DIES {
        return Err(Error::Dies);
    }
    let root_bridges = inputs
        .dies
        .iter()
        .enumerate()
        .map(|(index, die)| root_bridge(index as u8, die, inputs))
        .collect::<Result<Vec<_>, _>>()?;
    let dsdt_aml = [scope_vec("\\", &sleep_states())?, ecam_reservation(inputs)?]
        .into_iter()
        .chain(root_bridges)
        .flatten()
        .collect();
    let extra = core::iter::once(mcfg(inputs)?)
        .chain(inputs.board.tables.iter().cloned())
        .collect();
    Ok(Tables {
        platform: PlatformConfig::X86(platform(inputs)),
        dsdt_aml,
        extra,
    })
}

/// MPS INTI flags
const EDGE_HIGH: u16 = 0x5;
const LEVEL_LOW: u16 = 0xf;

fn platform(inputs: &Inputs) -> X86Config {
    let fch = IoApicConfig {
        id: FCH_IOAPIC_ID,
        base: FCH_IOAPIC_BASE.into(),
        gsi_base: 0,
    };
    let nbio = inputs
        .dies
        .iter()
        .enumerate()
        .map(|(index, die)| IoApicConfig {
            id: die.ioapic_id,
            base: die.ioapic_base.into(),
            gsi_base: nbio_gsi_base(index as u8),
        });
    X86Config {
        apic_ids: madt_order(inputs.apic_ids),
        lapic_base: LAPIC_BASE,
        lapic_nmi_lint: Some(1),
        ioapics: core::iter::once(fch).chain(nbio).collect(),
        isos: vec![
            // The PIT on GSI 2.
            IsoConfig {
                bus: 0,
                source: 0,
                gsi: 2,
                flags: EDGE_HIGH,
            },
            // The SCI, level-triggered active-low.
            IsoConfig {
                bus: 0,
                source: SCI_IRQ,
                gsi: SCI_IRQ.into(),
                flags: LEVEL_LOW,
            },
        ],
        hpet: Some(HpetConfig {
            base: HPET_BASE,
            timer_block_id: inputs.hpet_id,
            // No minimum tick.
            min_tick: 0,
        }),
        // ACPI_HAVE_PCAT_8259
        legacy_devices: true,
        sci_irq: SCI_IRQ,
        pmbase: acpi_io::BASE,
        gpe0_offset: acpi_io::GPE0 - acpi_io::BASE,
        gpe0_length: 8,
        // No SMM: the platform enables SCI_EN itself.
        acpi_smi: None,
        fadt: X86Fadt {
            // The default SMBIOS enclosure is a desktop.
            pm_profile: PmProfile::Desktop,
                iapc_boot_arch: IAPC_LEGACY_DEVICES,
            flags: [
                Flags::Wbinvd,
                Flags::ProcC1,
                Flags::SlpButton,
                Flags::RtcS4,
                Flags::TmrValExt,
                Flags::PciExpWak,
                Flags::UsePlatformClock,
                Flags::S4RtcStsValid,
                Flags::RemotePowerOnCapable,
            ]
            .iter()
            .fold(0, |flags, &flag| flags | flag as u32),
            // C2/C3 not supported here, _CST would describe them.
            p_lvl2_lat: 101,
            p_lvl3_lat: 1001,
            duty_offset: 0,
        },
    }
}

/// MADT order: the first thread of every core in
/// APIC ID order, then the second threads. Thread 1 of a Zen core is APIC ID
/// bit 0.
fn madt_order(apic_ids: &[u32]) -> Vec<u32> {
    let mut ids = apic_ids.to_vec();
    ids.sort_unstable_by_key(|&id| (id & 1, id));
    ids
}

/// The sleep states without ACPI resume (S3) support: S0, S1, S4, S5.
fn sleep_states() -> Vec<u8> {
    acpi_dsl! {
        Name("_S0_", Package(0u8, 0u8, 0u8, 0u8));
        Name("_S1_", Package(1u8, 1u8, 0u8, 0u8));
        Name("_S4_", Package(4u8, 4u8, 0u8, 0u8));
        Name("_S5_", Package(5u8, 5u8, 0u8, 0u8));
    }
    .into()
}

/// The ECAM window is a motherboard resource.
fn ecam_reservation(inputs: &Inputs) -> Result<Vec<u8>, Error> {
    if inputs.ecam_buses == 0 || inputs.ecam_buses > 256 {
        return Err(Error::EcamBuses);
    }
    let base = inputs.ecam_base;
    let size = u64::from(inputs.ecam_buses) << 20;
    let mut aml = Vec::new();
    acpi_dsl! {
        Device("PERC") {
            Name("_HID", EisaId("PNP0C02"));
            Name("_CRS", ResourceTemplate {
                Memory32Fixed(ReadWrite, #{dword base}, #{dword size});
            });
        }
    }
    .emit(&mut aml)?;
    Ok(aml)
}

fn mcfg(inputs: &Inputs) -> Result<Vec<u8>, Error> {
    let last_bus = inputs
        .ecam_buses
        .checked_sub(1)
        .and_then(|bus| u8::try_from(bus).ok())
        .ok_or(Error::EcamBuses)?;
    let mut mcfg = MCFG::new(
        fstart_acpi::OEM_ID,
        fstart_acpi::OEM_TABLE_ID,
        fstart_acpi::OEM_REVISION,
    );
    mcfg.add_ecam(inputs.ecam_base, 0, 0, last_bus);
    let mut bytes = Vec::new();
    mcfg.to_aml_bytes(&mut bytes);
    Ok(bytes)
}

/// The host bridge `PCIn` of a die: `_HID`, `_UID`, `_SEG`, `_BBN`, `_OSC`,
/// `_CRS` and the bridge `_PRT`s.
fn root_bridge(index: u8, die: &DieAcpi, inputs: &Inputs) -> Result<Vec<u8>, Error> {
    let uid = u64::from(index);
    let bus = u64::from(*die.buses.start());
    let mut body = Vec::new();
    acpi_dsl! {
        Name("_HID", EisaId("PNP0A08"));
        Name("_CID", EisaId("PNP0A03"));
        Name("_UID", #{byte uid});
        Name("_SEG", 0u8);
        Name("_BBN", #{byte bus});
        Method("_OSC", 4, NotSerialized) {
            // PCI host bridge: let the OS control everything.
            If (Arg0 == ToUUID("33DB4D5B-1FF7-401C-9657-7441C03DD766")) {
                Return(Arg3);
            } Else {
                // Unrecognized UUID
                CreateDwordField(Arg3, 0u8, "CDW1");
                CDW1 = CDW1 | 4u32;
                Return(Arg3);
            }
        }
    }
    .emit(&mut body)?;
    body.extend(root_resources(die, index == 0));
    if index == 0 {
        body.extend(root_prt(inputs.fch_intx_gsi));
    }
    for &devfn in die.bridges {
        body.extend(bridge(index, devfn)?);
    }
    if index == 0 {
        body.extend(lpc_bridge(&inputs.board.lpc_aml)?);
    }
    Ok(device_vec(&format!("PCI{index}"), &body)?)
}

/// A DWord or QWord memory window.
enum Mmio {
    Low(AddressSpace<u32>),
    High(AddressSpace<u64>),
}
impl Mmio {
    fn new(range: &RangeInclusive<u64>, cacheable: AddressSpaceCacheable) -> Self {
        let (min, max) = (*range.start(), *range.end());
        match (u32::try_from(min), u32::try_from(max)) {
            (Ok(min), Ok(max)) => {
                Self::Low(AddressSpace::new_memory(cacheable, true, min, max, None))
            }
            _ => Self::High(AddressSpace::new_memory(cacheable, true, min, max, None)),
        }
    }
}
impl Aml for Mmio {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        match self {
            Self::Low(window) => window.to_aml_bytes(sink),
            Self::High(window) => window.to_aml_bytes(sink),
        }
    }
}

/// The parts of `window` outside the configuration ports, which the root
/// bridge of die 0 consumes rather than produces.
fn around_cfg_ports(window: &RangeInclusive<u16>) -> impl Iterator<Item = RangeInclusive<u16>> {
    let below = *window.start()..=(*window.end()).min(PCI_CFG_IO.start() - 1);
    let above = (*window.start()).max(PCI_CFG_IO.end() + 1)..=*window.end();
    [below, above].into_iter().filter(|range| !range.is_empty())
}

/// `pci_domain_fill_ssdt`: `Name(_CRS)` with the bus range, the
/// configuration ports (die 0), the I/O and MMIO windows and VGA.
fn root_resources(die: &DieAcpi, cfg_ports: bool) -> Vec<u8> {
    let buses =
        AddressSpace::<u16>::new_bus_number((*die.buses.start()).into(), (*die.buses.end()).into());
    let cfg = IO::new(
        *PCI_CFG_IO.start(),
        *PCI_CFG_IO.start(),
        1,
        PCI_CFG_IO.len() as u8,
    );
    let io: Vec<_> = die
        .io
        .iter()
        .flat_map(around_cfg_ports)
        .map(|range| {
            AddressSpace::<u32>::new_io((*range.start()).into(), (*range.end()).into(), None)
        })
        .collect();
    let mmio: Vec<_> = die
        .mmio
        .iter()
        .map(|range| Mmio::new(range, AddressSpaceCacheable::NotCacheable))
        .collect();
    let vga = Mmio::new(&(0xa_0000..=0xb_ffff), AddressSpaceCacheable::Cacheable);
    let resources: Vec<&dyn Aml> = core::iter::once(&buses as &dyn Aml)
        .chain(cfg_ports.then_some(&cfg as &dyn Aml))
        .chain(io.iter().map(|window| window as &dyn Aml))
        .chain(mmio.iter().map(|window| window as &dyn Aml))
        .chain(die.vga.then_some(&vga as &dyn Aml))
        .collect();
    let mut bytes = Vec::new();
    Name::new("_CRS".into(), &ResourceTemplate::new(resources)).to_aml_bytes(&mut bytes);
    bytes
}

/// The root bus `_PRT`: the IOMMU (device 0) and the FCH SMBus/LPC (device
/// 0x14) interrupt through FCH INTA-D, at their PCI_INTR IOAPIC values
/// (IORA-IORD), one entry per pin.
fn root_prt(fch_intx_gsi: [u8; 4]) -> Vec<u8> {
    let pins = (0..4u8).map(move |pin| (pin, fch_intx_gsi[usize::from(pin)]));
    prt_name_aml(
        [0x00, 0x14]
            .into_iter()
            .flat_map(move |slot| pins.clone().map(move |(pin, gsi)| (slot, pin, gsi))),
    )
}

/// A bridge `GPxx` (xx: devfn in hex) with the `_PRT`
/// for the device behind it through the NBIO IOAPIC of the die.
fn bridge(die: u8, devfn: u8) -> Result<Vec<u8>, Error> {
    let route = ROUTES
        .iter()
        .find(|route| route.devfn == devfn)
        .ok_or(Error::UnknownBridge { die, devfn })?;
    let address = u64::from(devfn >> 3) << 16 | u64::from(devfn & 7);
    let mut body = Vec::new();
    acpi_dsl! { Name("_ADR", #{dword address}); }.emit(&mut body)?;
    let gsi = |pin| nbio_gsi_base(die) + u32::from(route.irq(pin));
    // At most two dies: the highest GSI is 87.
    body.extend(prt_name_aml((0..4).map(|pin| (0, pin, gsi(pin) as u8))));
    Ok(device_vec(&format!("GP{devfn:02X}"), &body)?)
}

/// The FCH legacy devices behind 0:14.3, then the board's.
fn lpc_bridge(board_aml: &[u8]) -> Result<Vec<u8>, Error> {
    let spi = u64::from(SPI_BASE);
    let espi = spi + 0x1_0000;
    let mut body = Vec::new();
    acpi_dsl! {
        Name("_ADR", 0x0014_0003u32);
        // SPI and eSPI controller BARs
        Device("LDRC") {
            Name("_HID", EisaId("PNP0C02"));
            Name("_UID", 0x3278u32);
            Name("_CRS", ResourceTemplate {
                Memory32Fixed(ReadWrite, #{dword spi}, 0x1000u32);
                Memory32Fixed(ReadWrite, #{dword espi}, 0x1000u32);
            });
        }
        Device("RTC0") {
            Name("_HID", EisaId("PNP0B00"));
            Name("_CRS", ResourceTemplate {
                IRQ(Edge, ActiveHigh, Shared, 8u8);
                IO(0x0070u16, 0x0070u16, 0x00u8, 0x02u8);
            });
        }
        Device("TMR_") {
            Name("_HID", EisaId("PNP0100"));
            Name("_CRS", ResourceTemplate {
                IRQ(Edge, ActiveHigh, Shared, 0u8);
                IO(0x0040u16, 0x0040u16, 0x00u8, 0x04u8);
            });
        }
        // Reserved: the board has no speaker
        Device("SPKR") {
            Name("_HID", EisaId("PNP0C02"));
            Name("_UID", 0x7239u32);
            Name("_CRS", ResourceTemplate {
                IO(0x0061u16, 0x0061u16, 0x00u8, 0x01u8);
            });
        }
        Device("PIC_") {
            Name("_HID", EisaId("PNP0000"));
            Name("_CRS", ResourceTemplate {
                IRQNoFlags(2u8);
                IO(0x0020u16, 0x0020u16, 0x00u8, 0x02u8);
                IO(0x00a0u16, 0x00a0u16, 0x00u8, 0x02u8);
            });
        }
        // 8237 DMA
        Device("MAD_") {
            Name("_HID", EisaId("PNP0200"));
            Name("_CRS", ResourceTemplate {
                IO(0x0000u16, 0x0000u16, 0x10u8, 0x10u8);
                IO(0x0081u16, 0x0081u16, 0x01u8, 0x03u8);
                IO(0x0087u16, 0x0087u16, 0x01u8, 0x01u8);
                IO(0x0089u16, 0x0089u16, 0x01u8, 0x03u8);
                IO(0x008fu16, 0x008fu16, 0x01u8, 0x01u8);
                IO(0x00c0u16, 0x00c0u16, 0x10u8, 0x20u8);
            });
        }
        Device("COPR") {
            Name("_HID", EisaId("PNP0C04"));
            Name("_CRS", ResourceTemplate {
                IO(0x00f0u16, 0x00f0u16, 0x00u8, 0x10u8);
                IRQNoFlags(13u8);
            });
        }
        Device("HPET") {
            Name("_HID", EisaId("PNP0103"));
            OperationRegion("HPMM", SystemMemory, 0xfed00010u32, 0x04u32);
            Field("HPMM", AnyAcc, NoLock, Preserve) {
                , 1,
                LGEN, 1,
            }
            // The FCH init leaves legacy replacement off, so IRQ 0 and 8
            // stay with TMR and RTC0 until the OS enables it.
            Name("CRS0", ResourceTemplate {
                Memory32Fixed(ReadWrite, 0xfed00000u32, 0x400u32);
                IRQ(Edge, ActiveHigh, Shared, 0u8);
                IRQ(Edge, ActiveHigh, Shared, 8u8);
            });
            Name("CRS1", ResourceTemplate {
                Memory32Fixed(ReadWrite, 0xfed00000u32, 0x400u32);
            });
            Method("_CRS", 0, NotSerialized) {
                If (LGEN) { Return(CRS0); }
                Return(CRS1);
            }
            // PMx00 HpetEn
            OperationRegion("ERMM", SystemMemory, 0xfed80300u32, 0x01u32);
            Field("ERMM", AnyAcc, NoLock, Preserve) {
                , 6,
                HPEN, 1,
            }
            Method("_STA", 0, NotSerialized) {
                If (HPEN) { Return(0x0fu8); }
                Return(0x01u8);
            }
        }
    }
    .emit(&mut body)?;
    body.extend_from_slice(board_aml);
    Ok(device_vec("LPCB", &body)?)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use fstart_driver_amd::zeppelin::nbio::{BASE, Op};

    /// The ACPI routing is the one the NBIO base table programs.
    #[test]
    fn routes_match_the_nbio_ioapic_bridge_table() {
        for (bridge, route) in ROUTES.iter().enumerate() {
            let address = 0x1430_0040 + 4 * bridge as u32;
            let set = BASE
                .iter()
                .find_map(|reg| match reg.op {
                    Op::Update { set, .. } if reg.address == address => Some(set),
                    _ => None,
                })
                .unwrap();
            assert_eq!(u32::from(route.group), set & 7, "bridge {bridge}");
            assert_eq!(
                u32::from(route.swizzle),
                (4 - ((set >> 4) & 3)) % 4,
                "bridge {bridge}"
            );
        }
    }

    static IO: RangeInclusive<u16> = 0..=0xffff;
    const MMIO: [RangeInclusive<u64>; 2] =
        [0xe000_0000..=0xefff_ffff, 0x100_0000_0000..=0x1ff_ffff_ffff];
    const BRIDGES: [u8; 4] = [0x09, 0x19, 0x39, 0x41];

    fn dies() -> [DieAcpi<'static>; 2] {
        let die = |index: u8| DieAcpi {
            buses: index * 64..=index * 64 + 63,
            ioapic_id: 1 + index,
            ioapic_base: 0xfeb0_0000 - u32::from(index) * 0x0800_0000,
            io: core::slice::from_ref(&IO),
            mmio: &MMIO,
            vga: index == 0,
            bridges: &BRIDGES,
        };
        [die(0), die(1)]
    }

    fn tables(board: &BoardAcpi) -> Tables {
        let dies = dies();
        build(&Inputs {
            apic_ids: &[0, 1, 8, 9, 2, 3],
            dies: &dies,
            ecam_base: 0xf000_0000,
            ecam_buses: 128,
            hpet_id: 0x1022_8201,
            fch_intx_gsi: [16, 17, 18, 19],
            board,
        })
        .unwrap()
    }

    /// The `_PRT` packages of a bridge: (pin, GSI).
    fn prt_of(aml: &[u8], name: &[u8; 4]) -> std::vec::Vec<(u64, u64)> {
        let start = aml.windows(4).position(|w| w == name).unwrap();
        let prt = start + aml[start..].windows(4).position(|w| w == b"_PRT").unwrap();
        // Name(_PRT, Package(4) { Package(4) { address, pin, 0, gsi }, .. })
        let mut at = prt + 4 + 3;
        let integer = |at: &mut usize| {
            let a = *at;
            let (value, len) = match aml[a] {
                0x00 => (0, 1),
                0x01 => (1, 1),
                0x0a => (u64::from(aml[a + 1]), 2),
                0x0b => (u64::from(u16::from_le_bytes([aml[a + 1], aml[a + 2]])), 3),
                0x0c => (
                    u64::from(u32::from_le_bytes(aml[a + 1..a + 5].try_into().unwrap())),
                    5,
                ),
                op => panic!("unexpected AML integer {op:#x}"),
            };
            *at += len;
            value
        };
        (0..4)
            .map(|_| {
                at += 3;
                let [_, pin, _, gsi] = [(); 4].map(|_| integer(&mut at));
                (pin, gsi)
            })
            .collect()
    }

    #[test]
    fn bridge_prts_route_through_the_nbio_ioapic_of_their_die() {
        let aml = tables(&BoardAcpi::default()).dsdt_aml;
        let pci1 = aml.windows(4).position(|w| w == b"PCI1").unwrap();
        // 3.1: group 7, CDAB on die 0: GSI 24 + 28 + 2, 3, 0, 1
        assert_eq!(prt_of(&aml, b"GP19"), [(0, 54), (1, 55), (2, 52), (3, 53)]);
        // 7.1 (xHCI): group 3, DABC
        assert_eq!(prt_of(&aml, b"GP39"), [(0, 39), (1, 36), (2, 37), (3, 38)]);
        // 8.1 on die 1: group 4, BCDA from GSI 56
        assert_eq!(
            prt_of(&aml[pci1..], b"GP41"),
            [(0, 73), (1, 74), (2, 75), (3, 72)]
        );
    }

    #[test]
    fn unknown_bridges_are_rejected() {
        let mut dies = dies();
        dies[1].bridges = &[0x0a, 0x29];
        let error = build(&Inputs {
            apic_ids: &[0],
            dies: &dies,
            ecam_base: 0xf000_0000,
            ecam_buses: 128,
            hpet_id: 0,
            fch_intx_gsi: [16, 17, 18, 19],
            board: &BoardAcpi::default(),
        });
        assert_eq!(
            error.err(),
            Some(Error::UnknownBridge {
                die: 1,
                devfn: 0x29
            })
        );
    }

    #[test]
    fn madt_lists_first_threads_first_and_the_nbio_ioapics() {
        let tables = tables(&BoardAcpi::default());
        let PlatformConfig::X86(config) = &tables.platform else {
            unreachable!()
        };
        assert_eq!(config.apic_ids, [0, 2, 8, 1, 3, 9]);
        let ioapics: std::vec::Vec<_> = config
            .ioapics
            .iter()
            .map(|ioapic| (ioapic.id, ioapic.base, ioapic.gsi_base))
            .collect();
        assert_eq!(
            ioapics,
            [
                (0, 0xfec0_0000, 0),
                (1, 0xfeb0_0000, 24),
                (2, 0xf6b0_0000, 56)
            ]
        );
    }

    #[test]
    fn root_bridge_resources_skip_the_configuration_ports() {
        let ranges: std::vec::Vec<_> = around_cfg_ports(&(0..=0xffff)).collect();
        assert_eq!(ranges, [0..=0xcf7, 0xd00..=0xffff]);
        assert_eq!(around_cfg_ports(&(0x1000..=0xffff)).count(), 1);

        // Die 0 consumes the configuration ports and gets the VGA window.
        const CFG: [u8; 8] = [0x47, 1, 0xf8, 0x0c, 0xf8, 0x0c, 1, 8];
        let vga = 0xa_0000u32.to_le_bytes();
        let [die0, die1] = dies();
        let die0 = root_resources(&die0, true);
        let die1 = root_resources(&die1, false);
        assert!(die0.windows(8).any(|w| w == CFG) && die0.windows(4).any(|w| w == vga));
        assert!(!die1.windows(8).any(|w| w == CFG) && !die1.windows(4).any(|w| w == vga));
    }

    /// Assemble the table set and disassemble the DSDT with `iasl`.
    #[test]
    fn dsdt_disassembles() {
        let board = BoardAcpi {
            lpc_aml: fstart_acpi::devices::IpmiKcsAcpi {
                port: 0xca2,
                spacing: 1,
                uid: 0,
                spec_revision: 0x0200,
            }
            .dsdt_aml(),
            tables: vec![],
        };
        let tables = tables(&board);
        let mut storage = vec![0; 64 * 1024];
        let len = fstart_acpi::platform::assemble_into(
            &mut storage,
            0x7f00_0000,
            &tables.platform,
            &tables.dsdt_aml,
            &tables.extra,
        )
        .unwrap();
        let fadt = signature(&storage[..len], b"FACP");
        assert_eq!(&fadt[46..48], &9u16.to_le_bytes()); // SCI_INT
        assert_eq!(&fadt[56..60], &0x800u32.to_le_bytes()); // PM1a_EVT
        assert_eq!(&fadt[80..84], &0x820u32.to_le_bytes()); // GPE0
        assert_eq!(&fadt[109..111], &1u16.to_le_bytes()); // IAPC_BOOT_ARCH
        assert_eq!(fadt[128], 6); // RESET_VALUE
        assert!(signature(&storage[..len], b"MCFG").len() == 60);

        let Ok(dsl) = iasl_disassemble(&fstart_acpi::platform::build_dsdt(&tables.dsdt_aml)) else {
            std::eprintln!("iasl not found, skipping the DSDT disassembly");
            return;
        };
        for object in [
            "Name (_S5,",
            "Device (PCI1)",
            "Device (GP41)",
            "Device (SPMI)",
            "PNP0C02",
        ] {
            assert!(dsl.contains(object), "{object} missing:\n{dsl}");
        }
    }

    fn signature<'a>(tables: &'a [u8], signature: &[u8; 4]) -> &'a [u8] {
        let start = tables.windows(4).position(|w| w == signature).unwrap();
        let len = u32::from_le_bytes(tables[start + 4..start + 8].try_into().unwrap());
        &tables[start..start + len as usize]
    }

    fn iasl_disassemble(dsdt: &[u8]) -> std::io::Result<std::string::String> {
        let dir =
            std::env::temp_dir().join(std::format!("fstart-zeppelin-dsdt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("dsdt.aml"), dsdt)?;
        let output = std::process::Command::new("iasl")
            .current_dir(&dir)
            .args(["-d", "dsdt.aml"])
            .output()?;
        let log = std::string::String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && !log.contains("Error"),
            "iasl: {log}"
        );
        std::fs::read_to_string(dir.join("dsdt.dsl"))
    }
}
