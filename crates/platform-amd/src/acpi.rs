// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ACPI tables of the Family 17h SoCs from what the silicon reports.
//! The SoC answers with data ([`SocAcpi`], [`Inputs`]); this module emits
//! the tables. The board adds its own ([`BoardAcpi`]).
//!
//! Interrupts: the FCH IOAPIC has GSI 0-23 and every die's NBIO IOAPIC 32
//! more, die n from 24 + 32n. The PCIe and internal bridges of a die
//! interrupt through its NBIO IOAPIC as the NBIO routes them; the root bus
//! devices (IOMMU, FCH SMBus/LPC) through the FCH INTA-D. The `_PRT`s
//! describe APIC mode only: the FCH PIC routing isn't programmed, so there
//! is nothing to describe for an OS in PIC mode.
extern crate alloc;

use alloc::{format, vec, vec::Vec};
use core::ops::RangeInclusive;
use fstart_acpi::{
    Aml, AmlError, AmlSink,
    aml::{AddressSpace, AddressSpaceCacheable, IO, Name, ResourceTemplate},
    aml_linker::{device_vec, scope_vec},
    ivrs::{self, DeviceEntry, Dte, Variety},
    mcfg::MCFG,
    pirq::prt_name_aml,
    platform::{HpetConfig, IoApicConfig, IsoConfig, PlatformConfig, X86Config, X86Fadt},
};
use fstart_acpi_macros::acpi_dsl;
use fstart_driver_amd::{
    fch::{AcpiPorts, SPI_BASE},
    nbio::iommu::{self, Iommu},
};

pub const LAPIC_BASE: u64 = 0xfee0_0000;
pub const FCH_IOAPIC_BASE: u32 = fstart_driver_amd::fch::IOAPIC;
/// The FCH IOAPIC has GSI 0; the NBIO IOAPICs follow from ID 1.
pub const FCH_IOAPIC_ID: u8 = 0;
pub const HPET_BASE: u64 = fstart_driver_amd::fch::HPET as u64;
const FCH_IOAPIC_PINS: u32 = 24;
const NBIO_IOAPIC_PINS: u32 = 32;
const SCI_IRQ: u8 = 9;
/// PCI configuration mechanism #1 ports, claimed by the root bridge of die 0.
const PCI_CFG_IO: RangeInclusive<u16> = 0xcf8..=0xcff;

/// First GSI of the NBIO IOAPIC of `die`.
pub const fn nbio_gsi_base(die: u8) -> u32 {
    FCH_IOAPIC_PINS + NBIO_IOAPIC_PINS * die as u32
}

/// Board ACPI content.
#[derive(Debug, Default)]
pub struct BoardAcpi {
    /// AML for `\_SB.PCI0.LPCB`: Super I/O and BMC devices.
    pub lpc_aml: Vec<u8>,
    /// Standalone tables (SPMI, ...).
    pub tables: Vec<Vec<u8>>,
    /// Enabled non-PCI devices to describe under \\_SB.
    pub devices: Vec<fstart_core::acpi::AcpiGenericDevice>,
    /// Further device AML for `\_SB`: FCH controllers and the devices
    /// behind them.
    pub sb_aml: Vec<u8>,
}

/// What a SoC's ACPI tables differ in.
#[derive(Debug, Clone, Copy)]
pub struct SocAcpi {
    /// The FCH ACPI I/O blocks.
    pub ports: AcpiPorts,
    /// The SoC's FADT fields.
    pub fadt: X86Fadt,
    /// How the NBIO routes the interrupts of its PCIe and internal bridges.
    pub bridge_routes: &'static [Route],
    /// IOAPIC GSIs of FCH INTA-D, as programmed in the PCI_INTR table.
    pub fch_intx_gsi: [u8; 4],
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
    /// The enabled PCIe and internal (7.1, 8.1) bridges on the root bus.
    pub bridges: &'a [Bridge],
    /// The IOMMU (00.2), if its MMIO is enabled.
    pub iommu: Option<Iommu>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bridge {
    pub devfn: u8,
    /// The bridge's own interrupt pin register: 0 none, 1-4 INTA-INTD.
    pub pin: u8,
}

pub struct Inputs<'a> {
    pub soc: &'a SocAcpi,
    /// APIC IDs of the online CPUs.
    pub apic_ids: &'a [u32],
    /// The dies in order.
    pub dies: &'a [DieAcpi<'a>],
    pub ecam_base: u64,
    pub ecam_buses: u16,
    /// HPET general capabilities and ID register (0xfed00000).
    pub hpet_id: u32,
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
    /// More dies than the GSIs of their IOAPICs fit in a byte.
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
pub const ABCD: u8 = 0;
pub const BCDA: u8 = 1;
pub const CDAB: u8 = 2;
pub const DABC: u8 = 3;

/// IOAPIC routing of a bridge.
#[derive(Debug, Clone, Copy)]
pub struct Route {
    pub devfn: u8,
    pub group: u8,
    pub swizzle: u8,
    /// IOAPIC pin of the bridge's own interrupt.
    pub bridge_irq: u8,
}
impl Route {
    pub const fn new(device: u8, function: u8, group: u8, swizzle: u8, bridge_irq: u8) -> Self {
        Self {
            devfn: fstart_pci::devfn(device, function),
            group,
            swizzle,
            bridge_irq,
        }
    }
    /// IOAPIC pin of INTx `pin` (0-3) behind the bridge (`pci_calculate_irq`).
    const fn irq(&self, pin: u8) -> u8 {
        self.group * 4 + (self.swizzle + pin) % 4
    }
}

/// Build the platform tables, DSDT content and extra tables.
pub fn build(inputs: &Inputs) -> Result<Tables, Error> {
    if nbio_gsi_base(inputs.dies.len() as u8) > 0x100 {
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
        .chain(inputs.board.devices.iter().map(|device| {
            fstart_acpi::devices::GenericAcpi {
                name: device.name.as_str(),
                hid: device.hid.as_str(),
                resources: &device.resources,
                gsiv: device.gsiv,
            }
            .dsdt_aml()
        }))
        .chain(core::iter::once(inputs.board.sb_aml.clone()))
        .flatten()
        .collect();
    let extra = core::iter::once(mcfg(inputs)?)
        .chain(ivrs(inputs))
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
        pmbase: inputs.soc.ports.event,
        gpe0_offset: inputs.soc.ports.gpe - inputs.soc.ports.event,
        gpe0_length: 8,
        // The OS enables SCI_EN through the FCH SMM handler.
        acpi_smi: Some(fstart_core::acpi::AcpiSmiConfig {
            smi_cmd: fstart_driver_amd::fch::smi::APM_CNT.into(),
            acpi_enable: fstart_driver_amd::fch::smi::ACPI_ENABLE,
            acpi_disable: fstart_driver_amd::fch::smi::ACPI_DISABLE,
        }),
        fadt: inputs.soc.fadt,
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

/// The FCH IOAPIC and the HPET interrupt as the FCH SMBus device 0:14.0.
const FCH_SMBUS: u16 = fstart_pci::bdf(0, 0x14, 0);

/// One IVHD 10h and 11h per die IOMMU, plus 40h when ACPI-only eMMC is
/// enabled. Die 0 owns the FCH IOAPIC and HPET; without its IOMMU no IVRS
/// is emitted.
fn ivrs(inputs: &Inputs) -> Option<Vec<u8>> {
    inputs.dies.first()?.iommu?;
    let devices: Vec<Vec<DeviceEntry>> = (0..inputs.dies.len())
        .map(|index| ivrs_devices(index, inputs))
        .collect();
    let iommus: Vec<ivrs::Iommu> = inputs
        .dies
        .iter()
        .zip(&devices)
        .filter_map(|(die, devices)| {
            let iommu = die.iommu?;
            Some(ivrs::Iommu {
                device_id: fstart_pci::bdf(*die.buses.start(), 0, 2),
                capability_offset: iommu::CAPABILITY,
                segment: 0,
                base: iommu.base,
                registers: ivrs::IommuRegisters {
                    capability: iommu.capability,
                    range: iommu.range,
                    misc: iommu.misc,
                    control: iommu.control,
                    efr: iommu.efr,
                    efr2: iommu.efr2,
                    counter_config: iommu.counter_config,
                },
                devices,
            })
        })
        .collect();
    ivrs::build_ivrs(&ivrs::IvrsConfig {
        iommus: &iommus,
        acpi_hid: inputs
            .board
            .devices
            .iter()
            .any(|d| d.hid.as_str() == "AMDI0040"),
    })
}

/// The device entries of die `index`'s IOMMU: its buses from 00.3 (after
/// the root complex, NBIO IOAPIC and IOMMU), its NBIO IOAPIC and, on die 0,
/// the HPET and the FCH IOAPIC. The handles are the MADT IOAPIC IDs and the
/// HPET number.
fn ivrs_devices(index: usize, inputs: &Inputs) -> Vec<DeviceEntry> {
    let die = &inputs.dies[index];
    let (bus, last_bus) = (*die.buses.start(), *die.buses.end());
    let own = [
        DeviceEntry::Range {
            start: fstart_pci::bdf(bus, 0, 3),
            end: fstart_pci::bdf(last_bus, 0x1f, 7),
            dte: 0,
        },
        DeviceEntry::Special {
            variety: Variety::IoApic,
            handle: die.ioapic_id,
            source: fstart_pci::bdf(bus, 0, 1),
            dte: 0,
        },
    ];
    let fch_ioapic_dte = Dte::LINT1_PASS::SET
        + Dte::LINT0_PASS::SET
        + Dte::SYS_MGT::NoTranslate
        + Dte::NMI_PASS::SET
        + Dte::EXT_INT_PASS::SET
        + Dte::INIT_PASS::SET;
    let fch = [
        DeviceEntry::Special {
            variety: Variety::Hpet,
            handle: 0,
            source: FCH_SMBUS,
            dte: 0,
        },
        DeviceEntry::Special {
            variety: Variety::IoApic,
            handle: FCH_IOAPIC_ID,
            source: FCH_SMBUS,
            dte: fch_ioapic_dte.value,
        },
    ];
    // The reference assigns the ACPI-only eMMC a synthetic requester ID
    // 00:13.1. It is not an enumerated PCI function.
    let emmc = (index == 0
        && inputs
            .board
            .devices
            .iter()
            .any(|d| d.hid.as_str() == "AMDI0040"))
    .then_some(DeviceEntry::AcpiHid {
        device: fstart_pci::bdf(0, 0x13, 1),
        hid: *b"AMDI0040",
        dte: (Dte::LINT1_PASS::SET
            + Dte::LINT0_PASS::SET
            + Dte::SYS_MGT::Translate
            + Dte::NMI_PASS::SET
            + Dte::EXT_INT_PASS::SET
            + Dte::INIT_PASS::SET)
            .value,
    });
    own.into_iter()
        .chain(fch.into_iter().filter(|_| index == 0))
        .chain(emmc)
        .collect()
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
    let routes = inputs.soc.bridge_routes;
    body.extend(root_prt(
        index,
        die.bridges,
        routes,
        inputs.soc.fch_intx_gsi,
    )?);
    for bridge_device in die.bridges {
        body.extend(bridge(index, bridge_device.devfn, routes)?);
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

/// The root bus `_PRT`. On die 0 the IOMMU (device 0) and the FCH SMBus/LPC
/// (device 0x14) interrupt through FCH INTA-D, at their PCI_INTR IOAPIC
/// values (IORA-IORD). Bridges with an interrupt pin of their own (the
/// internal bridges; the GPP bridges have none) interrupt at their bridge IRQ
/// of the die's NBIO IOAPIC, as in the vendor DSDT.
fn root_prt(
    die: u8,
    bridges: &[Bridge],
    routes: &[Route],
    fch_intx_gsi: [u8; 4],
) -> Result<Vec<u8>, Error> {
    let fch = (die == 0)
        .then(|| {
            [0x00, 0x14].into_iter().flat_map(move |slot| {
                (0..4u8).map(move |pin| (slot, pin, fch_intx_gsi[usize::from(pin)]))
            })
        })
        .into_iter()
        .flatten();
    let own = bridges
        .iter()
        .filter(|bridge| (1..=4).contains(&bridge.pin))
        .map(|bridge| {
            let route = routes
                .iter()
                .find(|route| route.devfn == bridge.devfn)
                .ok_or(Error::UnknownBridge {
                    die,
                    devfn: bridge.devfn,
                })?;
            // `build` checked that the GSIs fit.
            let gsi = nbio_gsi_base(die) + u32::from(route.bridge_irq);
            Ok((
                fstart_pci::devfn_parts(bridge.devfn).0,
                bridge.pin - 1,
                gsi as u8,
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    // One entry per slot and pin: the first function wins.
    let entries = fch
        .chain(own)
        .fold(Vec::new(), |mut entries: Vec<(u8, u8, u8)>, entry| {
            if !entries.iter().any(|e| (e.0, e.1) == (entry.0, entry.1)) {
                entries.push(entry);
            }
            entries
        });
    Ok(if entries.is_empty() {
        Vec::new()
    } else {
        prt_name_aml(entries.into_iter())
    })
}

/// A bridge `GPxx` (xx: devfn in hex) with the `_PRT`
/// for the device behind it through the NBIO IOAPIC of the die.
fn bridge(die: u8, devfn: u8, routes: &[Route]) -> Result<Vec<u8>, Error> {
    let route = routes
        .iter()
        .find(|route| route.devfn == devfn)
        .ok_or(Error::UnknownBridge { die, devfn })?;
    let (device, function) = fstart_pci::devfn_parts(devfn);
    let address = u64::from(device) << 16 | u64::from(function);
    let mut body = Vec::new();
    acpi_dsl! { Name("_ADR", #{dword address}); }.emit(&mut body)?;
    let gsi = |pin| nbio_gsi_base(die) + u32::from(route.irq(pin));
    // `build` checked that the GSIs fit.
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

/// Path of the FCH GPIO controller, [`fch_gpio`].
pub const FCH_GPIO_PATH: &str = "\\_SB.GPIO";

/// The FCH GPIO controller `\_SB.GPIO` at the GPIO bank of the ACPI MMIO,
/// interrupting on the level-low, shared FCH IRQ `irq`.
pub fn fch_gpio(irq: u8) -> Result<Vec<u8>, Error> {
    let base = u64::from(fstart_driver_amd::fch::ACPI_MMIO + 0x1500);
    let irq = u64::from(irq);
    let mut body = Vec::new();
    acpi_dsl! {
        Name("_HID", "AMDI0030");
        Name("_CID", "AMDI0030");
        Name("_UID", 0u32);
        Name("_CRS", ResourceTemplate {
            Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword irq});
            Memory32Fixed(ReadWrite, #{dword base}, 0x400u32);
        });
        Method("_STA", 0, NotSerialized) { Return(0x0Fu32); }
    }
    .emit(&mut body)?;
    Ok(device_vec("GPIO", &body)?)
}

/// An FCH DesignWare I2C controller `\_SB.I2C<index>` at `base`,
/// interrupting on the edge-high FCH IRQ `irq`, with `body`: its timing
/// and the devices behind it.
pub fn fch_i2c(index: u8, base: u32, irq: u8, body: &[u8]) -> Result<Vec<u8>, Error> {
    let (uid, base, irq) = (u64::from(index), u64::from(base), u64::from(irq));
    let mut device = Vec::new();
    acpi_dsl! {
        Name("_HID", "AMDI0010");
        Name("_UID", #{byte uid});
        Name("_CRS", ResourceTemplate {
            Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, #{dword irq});
            Memory32Fixed(ReadWrite, #{dword base}, 0x1000u32);
        });
        Method("_STA", 0, NotSerialized) { Return(0x0Fu32); }
    }
    .emit(&mut device)?;
    device.extend_from_slice(body);
    Ok(device_vec(&fch_i2c_name(index), &device)?)
}

/// NameSeg of FCH I2C controller `index`, [`fch_i2c`].
pub fn fch_i2c_name(index: u8) -> alloc::string::String {
    format!("I2C{index:X}")
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::zeppelin::acpi::ACPI;

    static IO: RangeInclusive<u16> = 0..=0xffff;
    const MMIO: [RangeInclusive<u64>; 2] =
        [0xe000_0000..=0xefff_ffff, 0x100_0000_0000..=0x1ff_ffff_ffff];
    const BRIDGES: [Bridge; 4] = [
        Bridge {
            devfn: 0x09,
            pin: 0,
        },
        Bridge {
            devfn: 0x19,
            pin: 0,
        },
        Bridge {
            devfn: 0x39,
            pin: 1,
        },
        Bridge {
            devfn: 0x41,
            pin: 1,
        },
    ];

    fn dies() -> [DieAcpi<'static>; 2] {
        let die = |index: u8| DieAcpi {
            buses: index * 64..=index * 64 + 63,
            ioapic_id: 1 + index,
            ioapic_base: 0xfeb0_0000 - u32::from(index) * 0x0800_0000,
            io: core::slice::from_ref(&IO),
            mmio: &MMIO,
            vga: index == 0,
            bridges: &BRIDGES,
            iommu: Some(Iommu {
                base: 0xfe60_0000 - u64::from(index) * 0x0800_0000,
                ..IOMMU
            }),
        };
        [die(0), die(1)]
    }

    /// An IOMMU with an EFR.
    const IOMMU: Iommu = Iommu {
        base: 0,
        capability: 1 << 27 | 0x0003_000f,
        range: 0,
        misc: 0,
        control: 0,
        efr: 0,
        efr2: 0,
        counter_config: 0,
    };

    fn tables(board: &BoardAcpi) -> Tables {
        let dies = dies();
        build(&Inputs {
            soc: &ACPI,
            apic_ids: &[0, 1, 8, 9, 2, 3],
            dies: &dies,
            ecam_base: 0xf000_0000,
            ecam_buses: 128,
            hpet_id: 0x1022_8201,
            board,
        })
        .unwrap()
    }

    /// The `_PRT` packages after `name`: (address, pin, GSI).
    fn prt_of(aml: &[u8], name: &[u8; 4]) -> std::vec::Vec<(u64, u64, u64)> {
        let start = aml.windows(4).position(|w| w == name).unwrap();
        let prt = start + aml[start..].windows(4).position(|w| w == b"_PRT").unwrap();
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
        // PackageOp, PkgLength (lead byte bits 7:6 count the extra bytes),
        // NumElements; returns that count.
        let package = |at: &mut usize| {
            assert_eq!(aml[*at], 0x12);
            *at += 2 + usize::from(aml[*at + 1] >> 6);
            let count = aml[*at];
            *at += 1;
            count
        };
        let mut at = prt + 4;
        (0..package(&mut at))
            .map(|_| {
                package(&mut at);
                let [address, pin, _, gsi] = [(); 4].map(|_| integer(&mut at));
                (address, pin, gsi)
            })
            .collect()
    }

    /// The (pin, GSI) of a bridge `_PRT`.
    fn bridge_prt(aml: &[u8], name: &[u8; 4]) -> std::vec::Vec<(u64, u64)> {
        prt_of(aml, name)
            .into_iter()
            .map(|(_, pin, gsi)| (pin, gsi))
            .collect()
    }

    #[test]
    fn root_prt_routes_the_internal_bridges_own_interrupts() {
        let aml = tables(&BoardAcpi::default()).dsdt_aml;
        let pci1 = aml.windows(4).position(|w| w == b"PCI1").unwrap();
        let root0 = prt_of(&aml, b"PCI0");
        // FCH INTA-D for the IOMMU and SMBus/LPC, then 7.1 and 8.1 on the
        // NBIO IOAPIC: bridge IRQs 0x14 and 0x08 from GSI 24.
        assert_eq!(
            root0[..4],
            [
                (0xffff, 0, 16),
                (0xffff, 1, 17),
                (0xffff, 2, 18),
                (0xffff, 3, 19)
            ]
        );
        assert_eq!(root0.len(), 10);
        assert_eq!(root0[8..], [(0x7_ffff, 0, 44), (0x8_ffff, 0, 32)]);
        assert_eq!(
            prt_of(&aml[pci1..], b"PCI1"),
            [(0x7_ffff, 0, 76), (0x8_ffff, 0, 64)]
        );
    }

    #[test]
    fn bridge_prts_route_through_the_nbio_ioapic_of_their_die() {
        let aml = tables(&BoardAcpi::default()).dsdt_aml;
        let pci1 = aml.windows(4).position(|w| w == b"PCI1").unwrap();
        // 3.1: group 7, CDAB on die 0: GSI 24 + 28 + 2, 3, 0, 1
        assert_eq!(
            bridge_prt(&aml, b"GP19"),
            [(0, 54), (1, 55), (2, 52), (3, 53)]
        );
        // 7.1 (xHCI): group 3, DABC
        assert_eq!(
            bridge_prt(&aml, b"GP39"),
            [(0, 39), (1, 36), (2, 37), (3, 38)]
        );
        // 8.1 on die 1: group 4, BCDA from GSI 56
        assert_eq!(
            bridge_prt(&aml[pci1..], b"GP41"),
            [(0, 73), (1, 74), (2, 75), (3, 72)]
        );
    }

    #[test]
    fn unknown_bridges_are_rejected() {
        let mut dies = dies();
        dies[1].bridges = &[
            Bridge {
                devfn: 0x0a,
                pin: 0,
            },
            Bridge {
                devfn: 0x29,
                pin: 0,
            },
        ];
        let error = build(&Inputs {
            soc: &ACPI,
            apic_ids: &[0],
            dies: &dies,
            ecam_base: 0xf000_0000,
            ecam_buses: 128,
            hpet_id: 0,
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
    fn ivrs_has_both_dies_with_their_buses_and_the_madt_ioapic_ids() {
        let dies = dies();
        let board = BoardAcpi::default();
        let inputs = Inputs {
            soc: &ACPI,
            apic_ids: &[0],
            dies: &dies,
            ecam_base: 0xf000_0000,
            ecam_buses: 128,
            hpet_id: 0,
            board: &board,
        };
        let special = |variety, handle, source, dte| DeviceEntry::Special {
            variety,
            handle,
            source,
            dte,
        };
        assert_eq!(
            ivrs_devices(0, &inputs),
            [
                DeviceEntry::Range {
                    start: 0x0003,
                    end: 0x3fff,
                    dte: 0
                },
                special(Variety::IoApic, 1, 0x0001, 0),
                special(Variety::Hpet, 0, 0x00a0, 0),
                special(Variety::IoApic, FCH_IOAPIC_ID, 0x00a0, 0xd7),
            ]
        );
        assert_eq!(
            ivrs_devices(1, &inputs),
            [
                DeviceEntry::Range {
                    start: 0x4003,
                    end: 0x7fff,
                    dte: 0
                },
                special(Variety::IoApic, 2, 0x4001, 0),
            ]
        );

        // IVHD 10h and 11h of the IOMMUs at 0:00.2 and 40:00.2.
        let table = ivrs(&inputs).unwrap();
        let mut at = 48;
        let mut blocks = std::vec::Vec::new();
        while at < table.len() {
            blocks.push((
                table[at],
                u16::from_le_bytes([table[at + 4], table[at + 5]]),
            ));
            at += usize::from(u16::from_le_bytes([table[at + 2], table[at + 3]]));
        }
        assert_eq!(
            blocks,
            [
                (0x10, 0x0002),
                (0x11, 0x0002),
                (0x10, 0x4002),
                (0x11, 0x4002)
            ]
        );
        assert!(
            tables(&board)
                .extra
                .iter()
                .any(|table| table[..4] == *b"IVRS")
        );

        // Die 0's IOMMU owns the FCH IOAPIC: no IVRS without it.
        let mut without = dies.clone();
        without[0].iommu = None;
        assert_eq!(
            ivrs(&Inputs {
                dies: &without,
                ..inputs
            }),
            None
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

    /// The FCH GPIO controller and I2C controller 2 with a touchpad and
    /// a HID-over-I2C alternative, as a Picasso board describes them.
    fn fch_devices() -> Vec<u8> {
        use fstart_acpi::devices::i2c::{
            DesignWareSpeed, GpioInterrupt, I2cDevice, designware_timing_aml,
        };
        let interrupt = Some(GpioInterrupt {
            controller: FCH_GPIO_PATH,
            pin: 9,
            edge: false,
            active_low: true,
        });
        let device = |name, ids, address, hid_descriptor| {
            I2cDevice {
                name,
                ids,
                uid: 0,
                bus: "\\_SB.I2C2",
                address,
                speed_hz: 400_000,
                interrupt,
                hid_descriptor,
            }
            .dsdt_aml()
            .unwrap()
        };
        let mut body = designware_timing_aml(DesignWareSpeed::Fast, 117, 245, 45).unwrap();
        body.extend(device("D150", &["ELAN0000"], 0x15, None));
        body.extend(device("D2C1", &["SYNA0000", "PNP0C50"], 0x2c, Some(0x20)));
        let mut aml = fch_gpio(7).unwrap();
        aml.extend(fch_i2c(2, 0xfedc_4000, 14, &body).unwrap());
        aml
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
            devices: vec![],
            sb_aml: fch_devices(),
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
        assert_eq!(&fadt[48..54], &[0xb2, 0, 0, 0, 0xe1, 0x1e]); // SMI_CMD, ACPI_EN/DIS
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
            "Device (GPIO)",
            "Device (I2C2)",
            "Device (D150)",
            "Device (D2C1)",
            "Interrupt (ResourceConsumer, Level, ActiveLow, Shared, ,, )",
            "Interrupt (ResourceConsumer, Edge, ActiveHigh, Exclusive, ,, )",
            "I2cSerialBusV2 (0x0015, ControllerInitiated, 0x00061A80,",
            "\\_SB.I2C2",
            "GpioInt (Level, ActiveLow, Exclusive, PullDefault, 0x0000,",
            "\\_SB.GPIO",
            "Name (FMCN, Package",
            "Method (_DSM, 4, Serialized)",
            "ToUUID (\"3cdff6f7-4267-4555-ad05-b30a3d8938de\")",
        ] {
            assert!(dsl.contains(object), "{object} missing:\n{dsl}");
        }
    }

    /// The table set of an MJ11-EC1 (EPYC 3151, one die) as the stage builds
    /// it, byte for byte: the bridges and windows the enumeration finds,
    /// the board's IPMI KCS with its SPMI.
    #[test]
    fn mj11_ec1_tables_are_pinned() {
        let io = 0x1000..=0xffff;
        let mmio = [0xe000_0000..=0xe7ff_ffff, 0xf800_0000..=0xfe7f_ffff];
        let bridges = [
            Bridge {
                devfn: 0x09,
                pin: 0,
            },
            Bridge {
                devfn: 0x0a,
                pin: 0,
            },
            Bridge {
                devfn: 0x0b,
                pin: 0,
            },
            Bridge {
                devfn: 0x0c,
                pin: 0,
            },
            Bridge {
                devfn: 0x0d,
                pin: 0,
            },
            Bridge {
                devfn: 0x19,
                pin: 0,
            },
            Bridge {
                devfn: 0x1b,
                pin: 0,
            },
            Bridge {
                devfn: 0x39,
                pin: 1,
            },
            Bridge {
                devfn: 0x41,
                pin: 1,
            },
        ];
        let dies = [DieAcpi {
            buses: 0..=127,
            ioapic_id: 1,
            ioapic_base: 0xfeb0_0000,
            io: core::slice::from_ref(&io),
            mmio: &mmio,
            vga: true,
            bridges: &bridges,
            iommu: Some(Iommu {
                base: 0xfe60_0000,
                capability: 0x1909_0040,
                range: 0x0000_ff00,
                misc: 0x0000_1a0b,
                control: 0,
                efr: 0x0000_00a2_9ec4_a7a5,
                efr2: 0,
                counter_config: 0x0000_0000_0002_0402,
            }),
        }];
        let kcs = fstart_acpi::devices::IpmiKcsAcpi {
            port: 0xca2,
            spacing: 1,
            uid: 0,
            spec_revision: 0x0200,
        };
        let board = BoardAcpi {
            lpc_aml: kcs.dsdt_aml(),
            tables: vec![kcs.spmi()],
            devices: vec![],
            sb_aml: vec![],
        };
        let tables = build(&Inputs {
            soc: &ACPI,
            apic_ids: &[0, 1, 2, 3, 8, 9, 10, 11],
            dies: &dies,
            ecam_base: 0xf000_0000,
            ecam_buses: 128,
            hpet_id: 0x1022_8201,
            board: &board,
        })
        .unwrap();
        let mut storage = vec![0; 0x2_0000];
        let len = fstart_acpi::platform::assemble_into(
            &mut storage,
            0x7601_8000,
            &tables.platform,
            &tables.dsdt_aml,
            &tables.extra,
        )
        .unwrap();
        let hash = storage[..len]
            .iter()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
                (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        assert_eq!((len, hash), (2784, 0xba3b_5c8e_f8ab_a62d));
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
