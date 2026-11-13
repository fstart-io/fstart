// SPDX-License-Identifier: MIT OR Apache-2.0
//! DSDT body (placed in `\_SB` by the fstart assembler; root objects use
//! absolute names): root bridges, interrupt routing, FCH devices, processor
//! objects, `_PIC`, the sleep states and what the SoC and the board add
//! (ACPI_SMM §3.4, §3.7).
//!
//! Only facts are taken from the vendors' ASL (HIDs, port lists, `_PRT`
//! values, `_S5` = 5); the structure is fstart's own:
//!
//! * one `PNP0A08` per root bridge (die, Rome/Milan NBIO), `_UID` and name
//!   `PCIn` in bus order (the SP3 donors also number by bus, which is why
//!   their `_UID`s run opposite to the GSI windows on Rome/Milan);
//! * `_OSC` grants the OS every control it asks for (OS-first; no CPMRAS
//!   firmware-first AER);
//! * APIC-mode `_PRT` only, from [`super::prt`] and the MADT GSI windows;
//! * `_PIC(1)` clears `IOAPIC::FEATURES_ENABLE.FCH` on every NBIO through
//!   that NBIO's own SMN pair (the donor `NAPE` reaches all NBIOs of
//!   socket 0 through bus 0 instead).

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::aml::{EISAName, IO, Memory32Fixed, Name, PackageBuilder, Path, ResourceTemplate};
use fstart_acpi::aml_linker::{device_vec, scope_vec};
use fstart_acpi::processor::{ProcessorName, processor_device};
use fstart_acpi::{Aml, AmlError, RawAml, acpi_dsl, gpe};
use fstart_driver_amd::ccx::power::PowerAml;
use fstart_driver_amd::fch::aml as fch_aml;
use fstart_driver_amd::nbio::ioapic::acpi as ioapic_acpi;

use super::fixed::{FCH_IOAPIC_BASE, GsiWindow, LAPIC_BASE};
use super::{Inputs, RootBridgeAcpi, SocAcpi, Wake, prt};

fn serialize(aml: &dyn Aml) -> Vec<u8> {
    let mut v = Vec::new();
    aml.to_aml_bytes(&mut v);
    v
}

fn name(path: &str, value: &dyn Aml) -> Vec<u8> {
    serialize(&Name::new(Path::new(path), value))
}

/// Root bridges in bus order, which numbers their `PCIn` names, with their
/// index in [`Inputs::root_bridges`].
fn ordered<'a>(bridges: &'a [RootBridgeAcpi<'a>]) -> Vec<(usize, &'a RootBridgeAcpi<'a>)> {
    let mut v: Vec<_> = bridges.iter().enumerate().collect();
    v.sort_unstable_by_key(|(_, r)| *r.root.buses().start());
    v
}

fn pci_name(uid: usize) -> [u8; 4] {
    [b'P', b'C', b'I', b'0' + uid as u8]
}

fn as_str(n: &[u8; 4]) -> &str {
    core::str::from_utf8(n).unwrap_or("PCI0")
}

/// `Name(_PRT, Package { {addr, pin, 0, gsi} ... })`.
fn prt_aml(entries: impl Iterator<Item = (u8, u8, u32)>) -> Vec<u8> {
    let mut prt = PackageBuilder::new();
    entries.for_each(|(device, pin, gsi)| {
        let mut e = PackageBuilder::new();
        e.add_element(&(u32::from(device) << 16 | 0xFFFF));
        e.add_element(&pin);
        e.add_element(&0u8);
        e.add_element(&gsi);
        prt.add_element(&e);
    });
    name("_PRT", &prt)
}

/// The deepest sleep state a bridge's PME wakes the system from: S4.
const WAKE_STATE: u8 = 4;

/// The device object of the bridge at `device.function` of a root bus:
/// `RPdf`.
pub(super) fn bridge_name(device: u8, function: u8) -> [u8; 4] {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    [
        b'R',
        b'P',
        HEX[usize::from(device & 0xf)],
        HEX[usize::from(function & 7)],
    ]
}

/// The FCH's devices on its root bus that interrupt through FCH INTA-D:
/// the root complex with the IOMMU (device 0) and the FCH's own functions
/// (device 0x14).
const FCH_INTX_DEVICES: [u8; 2] = [0x00, 0x14];

/// The root bus `_PRT` and one device per bridge with its own.
fn routing(
    soc: &SocAcpi,
    fch_intx_gsi: Option<[u8; 4]>,
    gsi_base: Option<u32>,
) -> Result<Vec<u8>, AmlError> {
    let fch = fch_intx_gsi.into_iter().flat_map(|gsi| {
        FCH_INTX_DEVICES.into_iter().flat_map(move |device| {
            (0..4).map(move |pin| (device, pin, gsi[usize::from(pin)].into()))
        })
    });
    let nbio = gsi_base.into_iter().flat_map(|gsi_base| {
        prt::root_entries(soc.bridges)
            .into_iter()
            .map(move |(d, p, pin)| (d, p, gsi_base + u32::from(pin)))
    });
    let mut root = fch.chain(nbio).peekable();
    if root.peek().is_none() {
        return Ok(Vec::new());
    }
    let root = prt_aml(root);
    // Without its NBIO IOAPIC (BAR not placed) the bridges have no APIC
    // routing to describe.
    let Some(gsi_base) = gsi_base else {
        return Ok(root);
    };
    soc.bridges.iter().try_fold(root, |mut out, b| {
        let at = (b.device, b.function);
        let mut body = [
            name("_ADR", &(u32::from(b.device) << 16 | u32::from(b.function))),
            prt_aml((0..4).map(|x| (0, x, gsi_base + u32::from(b.secondary_pin(x))))),
        ]
        .concat();
        if let Some(wake) = soc.wake.filter(|wake| wake.bridges.contains(&at)) {
            body.extend(gpe::prw(wake.gpe, WAKE_STATE));
        }
        if let Some((_, adr)) = soc.xhci.filter(|(bridge, _)| *bridge == at) {
            body.extend(device_vec("XHC0", &name("_ADR", &adr))?);
        }
        out.extend(device_vec(
            as_str(&bridge_name(b.device, b.function)),
            &body,
        )?);
        Ok(out)
    })
}

/// `\_GPE._Lxx` of the wake GPE: notify the wake bridges of every root
/// bridge in `uids` (those with bridge devices).
fn wake_gpe(wake: Option<Wake>, uids: impl Iterator<Item = usize>) -> Vec<u8> {
    let Some(wake) = wake else {
        return Vec::new();
    };
    let paths: Vec<alloc::string::String> = uids
        .flat_map(|uid| {
            wake.bridges.iter().map(move |&(d, f)| {
                alloc::format!(
                    "\\_SB_.{}.{}",
                    as_str(&pci_name(uid)),
                    as_str(&bridge_name(d, f))
                )
            })
        })
        .collect();
    let targets: Vec<&str> = paths.iter().map(|p| p.as_str()).collect();
    gpe::notify_wake(wake.gpe, false, &targets)
}

/// `_OSC` for PCI host bridges: every requested control is granted
/// (OS-first); an unknown UUID is flagged in CDW1.
fn osc() -> Vec<u8> {
    acpi_dsl! {
        Method("_OSC", 4, NotSerialized) {
            CreateDwordField(Arg3, 0u8, "CDW1");
            If (Arg0 != ToUUID("33db4d5b-1ff7-401c-9657-7441c03dd766")) {
                CDW1 = CDW1 | 4u8;
            }
            Return(Arg3);
        }
    }
    .into()
}

/// The LPC bridge (00:14.3): the FCH's ISA devices and the board's.
fn lpc(board_aml: &[u8]) -> Result<Vec<u8>, AmlError> {
    let adr = name("_ADR", &0x0014_0003u32);
    device_vec(
        "LPCB",
        &[adr.as_slice(), &fch_aml::lpc(), board_aml].concat(),
    )
}

/// Motherboard resources of the FCH and the CPUs inside what the root
/// bridge with the FCH forwards: FCH/LAPIC/IOAPIC MMIO and FCH I/O. What
/// the root bridges decode themselves (the ECAM window, the NBIO IOAPICs
/// and IOMMUs in the non-PCI pools) is in the motherboard device of each
/// root bridge.
fn system_resources(soc: &SocAcpi) -> Result<Vec<u8>, AmlError> {
    let mut d = Vec::new();
    [(FCH_IOAPIC_BASE, 0x1000), (LAPIC_BASE, 0x1000)]
        .iter()
        .copied()
        .chain(
            fch_aml::SYSTEM_MMIO
                .iter()
                .map(|&(b, l)| (u64::from(b), u64::from(l))),
        )
        .for_each(|(b, l)| Memory32Fixed::new(true, b as u32, l as u32).to_aml_bytes(&mut d));
    // The FCH's legacy and index ports (the SMI command port is in
    // 0xA2..0xBF) and the ACPI PM blocks.
    fch_aml::SYSTEM_IO
        .iter()
        .chain(&[(soc.ports.event, 0x30)])
        .for_each(|&(port, len)| IO::new(port, port, 1, len).to_aml_bytes(&mut d));
    let body = [
        name("_HID", &EISAName::new("PNP0C02")),
        name("_UID", &0x5359u32),
        name("_CRS", &ResourceTemplate::new(alloc::vec![&RawAml(&d)])),
    ]
    .concat();
    device_vec("SYSR", &body)
}

fn root_bridge(
    inputs: &Inputs,
    gsi_base: Option<u32>,
    uid: usize,
    rb: &RootBridgeAcpi,
    lpc_aml: &[u8],
) -> Result<Vec<u8>, AmlError> {
    let soc = inputs.soc;
    let bus = *rb.root.buses().start();
    let mut body = [
        name("_HID", &EISAName::new("PNP0A08")),
        name("_CID", &EISAName::new("PNP0A03")),
        name("_UID", &(uid as u32)),
        name("_SEG", &0u8),
        name("_BBN", &bus),
        rb.domain.map(|d| name("_PXM", &d)).unwrap_or_default(),
        fstart_acpi::pci_root::crs(rb.root),
        osc(),
        ioapic_acpi::nbio_aml(rb.ioapic_features)?,
        routing(soc, soc.fch_intx_gsi.filter(|_| rb.fch), gsi_base)?,
    ]
    .concat();
    if rb.fch {
        body.extend(lpc(lpc_aml)?);
        body.extend(system_resources(soc)?);
    }
    // Next to the bridge: what it decodes itself, which `_CRS` leaves out.
    let mut aml = device_vec(as_str(&pci_name(uid)), &body)?;
    aml.extend(fstart_acpi::pci_root::motherboard(
        &alloc::format!("MBR{uid:X}"),
        rb.root,
    )?);
    Ok(aml)
}

/// `\PICM`, `\_PIC` and the sleep states.
fn root_objects(soc: &SocAcpi, uids: usize) -> Result<Vec<u8>, AmlError> {
    let nbios: Vec<alloc::string::String> = (0..uids)
        .map(|uid| alloc::format!("\\_SB_.{}.NBIO", as_str(&pci_name(uid))))
        .collect();
    let states: Vec<u8> = soc
        .sleep_states
        .iter()
        .flat_map(|&(state, slp_typ)| {
            let mut package = PackageBuilder::new();
            [slp_typ, 0, 0, 0]
                .iter()
                .for_each(|value| package.add_element(value));
            name(&alloc::format!("_S{state}_"), &package)
        })
        .collect();
    Ok([ioapic_acpi::pic_aml(&nbios)?, scope_vec("\\", &states)?].concat())
}

fn processors(inputs: &Inputs) -> Result<Vec<u8>, AmlError> {
    let power = inputs.power.as_ref().map(PowerAml::new);
    inputs
        .cpus
        .iter()
        .enumerate()
        .try_fold(Vec::new(), |mut out, (uid, cpu)| {
            let index = u16::try_from(uid).map_err(|_| AmlError::InvalidOperand)?;
            let mut body = power
                .as_ref()
                .map(|p| p.for_cpu(cpu.apic_id))
                .unwrap_or_default();
            body.extend_from_slice(&cpu.aml);
            out.extend(processor_device(
                ProcessorName::new(index),
                uid as u32,
                &body,
            )?);
            Ok(out)
        })
}

/// The DSDT body for `fstart_acpi::platform::assemble*` (inside `\_SB`).
pub fn build(inputs: &Inputs, windows: &[GsiWindow]) -> Result<Vec<u8>, AmlError> {
    let bridges = ordered(inputs.root_bridges);
    if bridges.len() > 10 || inputs.cpus.len() > 0x1000 {
        return Err(AmlError::InvalidOperand);
    }
    let gsi_base = |index| {
        windows
            .iter()
            .find(|w| w.bridge == Some(index))
            .map(|w| w.gsi_base)
    };
    let board = inputs.board;
    let lpc_aml: Vec<u8> = board
        .ipmi
        .map(|k| k.dsdt_aml())
        .into_iter()
        .flatten()
        .chain(board.lpc_aml.iter().copied())
        .collect();
    let mut out = root_objects(inputs.soc, bridges.len())?;
    let routed = bridges
        .iter()
        .enumerate()
        .filter(|(_, (index, _))| gsi_base(*index).is_some())
        .map(|(uid, _)| uid);
    out.extend(wake_gpe(inputs.soc.wake, routed));
    for (uid, (index, rb)) in bridges.iter().enumerate() {
        out.extend(root_bridge(inputs, gsi_base(*index), uid, rb, &lpc_aml)?);
    }
    out.extend(fstart_acpi::osc::platform_osc());
    if let Some(tpm) = inputs.tpm {
        out.extend(tpm.dsdt_aml());
    }
    if let (Some(smi), Some(notify)) = (inputs.acpi_smi, inputs.bmc_sx_notify) {
        let port = u16::try_from(smi.smi_cmd).map_err(|_| AmlError::InvalidOperand)?;
        out.extend(notify.aml(port));
    }
    out.extend(processors(inputs)?);
    for device in inputs.devices {
        out.extend(
            fstart_acpi::devices::GenericAcpi {
                name: device.name.as_str(),
                hid: device.hid.as_str(),
                resources: &device.resources,
                gsiv: device.gsiv,
                interrupt_flags: device.interrupt_flags,
            }
            .dsdt_aml(),
        );
    }
    out.extend_from_slice(inputs.sb_aml);
    out.extend_from_slice(&board.sb_aml);
    Ok(out)
}
