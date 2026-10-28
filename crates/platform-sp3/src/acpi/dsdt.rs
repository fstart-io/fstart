//! DSDT body (placed in `\_SB` by the fstart assembler; root objects use
//! absolute names): root bridges, interrupt routing, FCH devices, processor
//! objects, `_PIC`, `\_S0`/`\_S5` and the board hook (ACPI_SMM §3.4, §3.7).
//!
//! Only facts are taken from the donor ASL (HIDs, port lists, `_PRT`
//! values, `_S5` = 5); the structure is fstart's own:
//!
//! * one `PNP0A08` per root bridge (Naples die, Rome/Milan NBIO), `_UID`
//!   and name `PCIn` in bus order (the donors also number by bus, which is
//!   why their `_UID`s run opposite to the GSI windows on Rome/Milan);
//! * `_OSC` grants the OS every control it asks for (OS-first; no CPMRAS
//!   firmware-first AER);
//! * APIC-mode `_PRT` only, from [`super::prt`] and the MADT GSI windows;
//! * `_PIC(1)` clears `IOAPIC::FEATURES_ENABLE.FCH` on every NBIO through
//!   that NBIO's own SMN pair (the donor `NAPE` reaches all NBIOs of
//!   socket 0 through bus 0 instead).

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::aml::{
    AddressSpace, AddressSpaceCacheable, Arg, EISAName, IO, If, Memory32Fixed, Method, MethodCall,
    Name, PackageBuilder, Path, ResourceTemplate, Store,
};
use fstart_acpi::aml_linker::{device_vec, scope_vec};
use fstart_acpi::gas::{AccessSize, AddressSpace as GasSpace, GAS};
use fstart_acpi::processor::{
    Coordination, PerformanceState, PowerState, ProcessorName, cpc_aml, cst_aml, pct_aml,
    processor_device, psd_aml, pss_aml,
};
use fstart_acpi::{Aml, AmlError, RawAml, acpi_dsl, gpe};
use fstart_driver_amd::sp3::fch::acpi as fch_acpi;
use fstart_driver_amd::sp3::smu::v11::cppc;
use fstart_driver_amd::sp3::soc::Generation;

use super::fixed::{FCH_IOAPIC_BASE, GsiWindow, HPET_BASE, LAPIC_BASE, PM_BASE};
use super::{Inputs, Power, RootBridgeAcpi, Window, prt};

/// PCI configuration ports, consumed by the primary root bridge.
const CONFIG_PORTS: (u16, u16) = (0xCF8, 0xCFF);
/// Legacy VGA memory, routed to the primary root bridge.
const VGA: (u32, u32) = (0xA_0000, 0xB_FFFF);

fn serialize(aml: &dyn Aml) -> Vec<u8> {
    let mut v = Vec::new();
    aml.to_aml_bytes(&mut v);
    v
}

fn name(path: &str, value: &dyn Aml) -> Vec<u8> {
    serialize(&Name::new(Path::new(path), value))
}

/// Root bridges in bus order with their `PCIn` names.
fn ordered<'a>(bridges: &'a [RootBridgeAcpi<'a>]) -> Vec<(usize, &'a RootBridgeAcpi<'a>)> {
    let mut v: Vec<_> = bridges.iter().collect();
    v.sort_unstable_by_key(|r| r.bridge.bus_base);
    v.into_iter().enumerate().collect()
}

fn pci_name(uid: usize) -> [u8; 4] {
    [b'P', b'C', b'I', b'0' + uid as u8]
}

fn as_str(n: &[u8; 4]) -> &str {
    core::str::from_utf8(n).unwrap_or("PCI0")
}

/// Split IO windows around the configuration ports.
fn io_windows(io: &[Window<u16>]) -> impl Iterator<Item = (u16, u16)> + '_ {
    let (lo, hi) = CONFIG_PORTS;
    io.iter().flat_map(move |w| {
        let parts = match w.base <= hi && w.limit >= lo {
            true => [
                (w.base < lo).then_some((w.base, lo - 1)),
                (w.limit > hi).then_some((hi + 1, w.limit)),
            ],
            false => [Some((w.base, w.limit)), None],
        };
        parts.into_iter().flatten()
    })
}

fn mmio_descriptor(base: u64, limit: u64, out: &mut Vec<u8>) {
    match u32::try_from(limit) {
        Ok(limit) => AddressSpace::<u32>::new_memory(
            AddressSpaceCacheable::NotCacheable,
            true,
            base as u32,
            limit,
            None,
        )
        .to_aml_bytes(out),
        Err(_) => AddressSpace::<u64>::new_memory(
            AddressSpaceCacheable::NotCacheable,
            true,
            base,
            limit,
            None,
        )
        .to_aml_bytes(out),
    }
}

/// `_CRS` of a root bridge: bus range, config ports and VGA on the primary,
/// then the DF IO and MMIO windows.
fn crs(rb: &RootBridgeAcpi) -> Vec<u8> {
    let mut d = Vec::new();
    AddressSpace::<u16>::new_bus_number(rb.bridge.bus_base.into(), rb.bridge.bus_limit.into())
        .to_aml_bytes(&mut d);
    if rb.bridge.primary {
        IO::new(CONFIG_PORTS.0, CONFIG_PORTS.0, 1, 8).to_aml_bytes(&mut d);
    }
    io_windows(rb.io)
        .for_each(|(lo, hi)| AddressSpace::<u16>::new_io(lo, hi, None).to_aml_bytes(&mut d));
    if rb.bridge.primary {
        mmio_descriptor(VGA.0.into(), VGA.1.into(), &mut d);
    }
    rb.mmio
        .iter()
        .for_each(|w| mmio_descriptor(w.base, w.limit, &mut d));
    name("_CRS", &ResourceTemplate::new(alloc::vec![&RawAml(&d)]))
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

/// PME/wake GPE of the internal bridges (donor `GPRW(0x0B, 4)` on
/// Rome/Milan 7.1 and 8.1 of every NBIO, notified by `\_GPE._L0B`).
const WAKE_GPE: u8 = 0x0B;
const WAKE_STATE: u8 = 4;

/// Internal bridges with `_PRW`. The Naples donor ASL notifies other
/// objects (CRB names); not modelled there.
fn wake_bridges(generation: Generation) -> &'static [(u8, u8)] {
    match generation {
        Generation::Naples => &[],
        Generation::Rome | Generation::Milan => &[(7, 1), (8, 1)],
    }
}

/// The xHCI behind 8.1 (function 3, donor `XHC0`) on Rome/Milan.
const XHCI: Option<((u8, u8), u32)> = Some(((8, 1), 3));

fn bridge_name(device: u8, function: u8) -> [u8; 4] {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    [
        b'R',
        b'P',
        HEX[usize::from(device & 0xf)],
        HEX[usize::from(function & 7)],
    ]
}

/// The root bus `_PRT` and one device per internal bridge with its own.
fn routing(generation: Generation, gsi_base: u32) -> Result<Vec<u8>, AmlError> {
    let root = prt_aml(
        prt::root_entries(generation)
            .into_iter()
            .map(|(d, p, pin)| (d, p, gsi_base + u32::from(pin))),
    );
    prt::bridges(generation).try_fold(root, |mut out, b| {
        let at = (b.device, b.function);
        let mut body = [
            name("_ADR", &(u32::from(b.device) << 16 | u32::from(b.function))),
            prt_aml((0..4).map(|x| (0, x, gsi_base + u32::from(b.secondary_pin(x))))),
        ]
        .concat();
        if wake_bridges(generation).contains(&at) {
            body.extend(gpe::prw(WAKE_GPE, WAKE_STATE));
        }
        if let Some((_, adr)) = XHCI.filter(|(br, _)| *br == at && generation != Generation::Naples)
        {
            body.extend(device_vec("XHC0", &name("_ADR", &adr))?);
        }
        out.extend(device_vec(
            as_str(&bridge_name(b.device, b.function)),
            &body,
        )?);
        Ok(out)
    })
}

/// `\_GPE._L0B` waking every root bridge's wake bridges.
fn wake_gpe(generation: Generation, uids: usize) -> Vec<u8> {
    let paths: Vec<alloc::string::String> = (0..uids)
        .flat_map(|uid| {
            wake_bridges(generation).iter().map(move |&(d, f)| {
                alloc::format!(
                    "\\_SB_.{}.{}",
                    as_str(&pci_name(uid)),
                    as_str(&bridge_name(d, f))
                )
            })
        })
        .collect();
    let targets: Vec<&str> = paths.iter().map(|p| p.as_str()).collect();
    gpe::notify_wake(WAKE_GPE, false, &targets)
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

/// The root complex function (D0F0) with `NAPE`: clear
/// `IOAPIC::FEATURES_ENABLE.FCH` (bit 4) through the SMN pair at D0F0
/// 0xB8/0xBC, so INTx reach the NBIO IOAPIC instead of the FCH.
fn nbio(features_smn: u32) -> Result<Vec<u8>, AmlError> {
    let mut v = Vec::new();
    acpi_dsl! {
        Device("NBIO") {
            Name("_ADR", 0u8);
            OperationRegion("NAPC", PciConfig, 0xB8u8, 8u8);
            Field("NAPC", DWordAcc, NoLock, Preserve) {
                NAPX, 32,
                NAPD, 32,
            }
            Method("NAPE", 0, Serialized) {
                NAPX = #{dword features_smn};
                NAPD = NAPD & 0xFFFFFFEFu32;
            }
        }
    }
    .emit(&mut v)?;
    Ok(v)
}

/// The LPC bridge (00:14.3): the FCH's ISA devices and the board's.
fn lpc(board_aml: &[u8]) -> Result<Vec<u8>, AmlError> {
    let adr = name("_ADR", &0x0014_0003u32);
    let fch = fch_acpi::lpc_aml(HPET_BASE as u32)?;
    device_vec("LPCB", &[adr.as_slice(), &fch, board_aml].concat())
}

/// Motherboard resources the root-bridge windows would otherwise offer:
/// ECAM, FCH/LAPIC/IOAPIC MMIO, NBIO IOAPIC and IOMMU BARs, FCH IO.
fn system_resources(inputs: &Inputs, windows: &[GsiWindow]) -> Result<Vec<u8>, AmlError> {
    let (ecam, buses) = inputs.generation.ecam();
    let mut d = Vec::new();
    let mem = |base: u64, len: u64, d: &mut Vec<u8>| match u32::try_from(base + len) {
        Ok(_) => Memory32Fixed::new(true, base as u32, len as u32).to_aml_bytes(d),
        Err(_) => mmio_descriptor(base, base + len - 1, d),
    };
    mem(ecam, u64::from(buses) << 20, &mut d);
    [(FCH_IOAPIC_BASE, 0x1000), (LAPIC_BASE, 0x1000)]
        .iter()
        .copied()
        .chain(
            fch_acpi::SYSTEM_MMIO
                .iter()
                .map(|&(b, l)| (u64::from(b), u64::from(l))),
        )
        .chain(
            windows
                .iter()
                .filter(|w| w.nbio.is_some())
                .map(|w| (w.base, 0x1000)),
        )
        .chain(
            inputs
                .iommus
                .iter()
                .map(|m| (m.info.base, fstart_driver_amd::sp3::nbio::iommu::MMIO_SIZE)),
        )
        .for_each(|(b, l)| mem(b, l, &mut d));
    // The FCH's legacy and index ports (the SMI command port is in
    // 0xA2..0xBF) and the ACPI PM blocks.
    fch_acpi::SYSTEM_IO
        .iter()
        .chain(&[(PM_BASE, 0x30)])
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
    windows: &[GsiWindow],
    uid: usize,
    rb: &RootBridgeAcpi,
) -> Result<Vec<u8>, AmlError> {
    let b = &rb.bridge;
    let gsi_base = windows
        .iter()
        .find(|w| w.nbio == Some((b.socket, b.index)))
        .map(|w| w.gsi_base);
    let mut body = [
        name("_HID", &EISAName::new("PNP0A08")),
        name("_CID", &EISAName::new("PNP0A03")),
        name("_UID", &(uid as u32)),
        name("_SEG", &0u8),
        name("_BBN", &b.bus_base),
        rb.domain.map(|d| name("_PXM", &d)).unwrap_or_default(),
        crs(rb),
        osc(),
        nbio(rb.ioapic_features)?,
    ]
    .concat();
    // Without its NBIO IOAPIC (BAR not placed) the bridge has no APIC
    // routing to describe.
    if let Some(gsi_base) = gsi_base {
        body.extend(routing(inputs.generation, gsi_base)?);
    }
    if b.primary {
        body.extend(lpc(inputs.board.lpc_aml)?);
        body.extend(system_resources(inputs, windows)?);
    }
    device_vec(as_str(&pci_name(uid)), &body)
}

/// `\PICM`, `\_PIC`, `\_S0`, `\_S5`.
fn root_objects(uids: usize) -> Result<Vec<u8>, AmlError> {
    let paths: Vec<Vec<u8>> = (0..uids)
        .map(|uid| {
            let n = pci_name(uid);
            let p = alloc::format!("\\_SB_.{}.NBIO.NAPE", as_str(&n));
            serialize(&MethodCall::new(Path::new(&p), Vec::new()))
        })
        .collect();
    let calls: Vec<RawAml> = paths.iter().map(|p| RawAml(p)).collect();
    let picm = Path::new("PICM");
    let store = Store::new(&picm, &Arg(0));
    let nape = If::new(&Arg(0), calls.iter().map(|c| c as &dyn Aml).collect());
    let pic = serialize(&Method::new(
        Path::new("_PIC"),
        1,
        false,
        alloc::vec![&store, &nape],
    ));
    let fixed: Vec<u8> = acpi_dsl! {
        Name("PICM", 0u8);
        Name("_S0_", Package(0u8, 0u8, 0u8, 0u8));
        Name("_S5_", Package(5u8, 0u8, 0u8, 0u8));
    }
    .into();
    scope_vec("\\", &[fixed, pic].concat())
}

/// `_PSS`/`_PCT`/`_PSD`/`_CST` of one processor (AM4 lead, am4-re
/// `ACPI_DXE` §2: `_PCT` FFH with control C001_0062, `_PSD` HW_ALL per core,
/// C1 FFH (width 2, offset 2), C2 an IO read of the C-state base + 1;
/// C2 latency 400 µs on Zen 1, 18 µs on Zen 2/3 — not re-derived for SP3).
struct PowerAml {
    shared: Vec<u8>,
    threads_per_core: u8,
}

impl PowerAml {
    fn new(generation: Generation, power: &Power) -> Self {
        let pss: Vec<PerformanceState> = power
            .pstates
            .iter()
            .map(|p| PerformanceState {
                mhz: p.mhz,
                milliwatts: p.milliwatts,
                latency_us: 0,
                bus_master_latency_us: 0,
                control: p.index.into(),
                status: p.index.into(),
            })
            .collect();
        let ffh = |width, offset, access, address| {
            GAS::new(
                GasSpace::FunctionalFixedHardware,
                width,
                offset,
                access,
                address,
            )
        };
        let pct = pct_aml(
            ffh(0, 0, AccessSize::Undefined, 0xC001_0062),
            ffh(0, 0, AccessSize::Undefined, 0),
        );
        let c2_latency = match generation {
            Generation::Naples => 400,
            Generation::Rome | Generation::Milan => 18,
        };
        let cst = power.cstates.map(|c| {
            cst_aml(&[
                PowerState {
                    register: ffh(2, 2, AccessSize::ByteAccess, 0),
                    kind: 1,
                    latency_us: 1,
                    milliwatts: 0,
                },
                PowerState {
                    register: GAS::new(
                        GasSpace::SystemIo,
                        8,
                        0,
                        AccessSize::ByteAccess,
                        u64::from(c.io_base) + 1,
                    ),
                    kind: 2,
                    latency_us: c2_latency,
                    milliwatts: 0,
                },
            ])
        });
        let shared = [
            if pss.is_empty() {
                Vec::new()
            } else {
                [pct, pss_aml(&pss)].concat()
            },
            cst.unwrap_or_default(),
        ]
        .concat();
        Self {
            shared,
            threads_per_core: power.threads_per_core.max(1),
        }
    }

    fn for_cpu(&self, apic_id: u32) -> Vec<u8> {
        let t = u32::from(self.threads_per_core);
        [
            self.shared.clone(),
            psd_aml(apic_id / t, Coordination::HwAll, t),
        ]
        .concat()
    }
}

fn processors(inputs: &Inputs) -> Result<Vec<u8>, AmlError> {
    let power = inputs
        .power
        .as_ref()
        .map(|p| PowerAml::new(inputs.generation, p));
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
            let slot = inputs
                .cppc
                .and_then(|c| Some((c, c.slots.iter().position(|&a| a == cpu.apic_id)?)));
            if let Some((c, slot)) = slot {
                let entries = cppc::cpc_entries(&c.info, slot as u32, c.lowest_mhz, c.nominal_mhz);
                let n = cppc::cpc_entry_count(c.info.revision);
                body.extend(cpc_aml(c.info.revision, &entries[..n]));
            }
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
    let mut out = root_objects(bridges.len())?;
    out.extend(wake_gpe(inputs.generation, bridges.len()));
    for (uid, rb) in &bridges {
        out.extend(root_bridge(inputs, windows, *uid, rb)?);
    }
    out.extend(fstart_acpi::osc::platform_osc());
    if let Some(family) = inputs.tpm {
        out.extend(fstart_acpi::devices::tpm::TpmTisAcpi { family }.dsdt_aml());
    }
    if let (Some(smi), Some(base)) = (inputs.acpi_smi, inputs.bmc_sx_notify) {
        let port = u16::try_from(smi.smi_cmd).map_err(|_| AmlError::InvalidOperand)?;
        out.extend(fstart_acpi::devices::IpmiKcsAcpi::sleep_notify_aml(
            port, base,
        ));
    }
    out.extend(processors(inputs)?);
    out.extend(inputs.fch.sb_aml()?);
    out.extend_from_slice(inputs.board.sb_aml);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_ports_are_cut_out() {
        let io = [
            Window {
                base: 0,
                limit: 0xFFF,
            },
            Window {
                base: 0x1000,
                limit: 0x1FFF,
            },
        ];
        let got: Vec<_> = io_windows(&io).collect();
        assert_eq!(got, [(0, 0xCF7), (0xD00, 0xFFF), (0x1000, 0x1FFF)]);
    }
}
