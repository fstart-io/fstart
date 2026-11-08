// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native ACPI of a single-socket Rs SoC (Genoa, Siena) from initialized
//! resources: CPUs and their P-states, each live root's bus/I/O/MMIO windows,
//! NBIO IOAPIC routing and IOMMUs when the platform initialized them, the
//! FCH timers and the console. Reuses generic AML/table builders, not the
//! Family17h interrupt model, vendor AML/GNVS addresses or uninitialized modes.
use alloc::{format, vec, vec::Vec};
use fstart_acpi::{
    self as acpi, Aml, AmlError, acpi_dsl,
    aml::Name,
    aml_linker::{device_vec, scope_vec},
    ivrs::{self, DeviceEntry, Dte, Variety},
    platform::{HpetConfig, IoApicConfig, IsoConfig, PlatformConfig, X86Config, X86Fadt},
};
use fstart_driver_amd::{
    ccx::power::{Power, PowerAml, Pstate},
    nbio::iommu::Iommu,
};

/// The SLP_TYP of `_S5_`, also the S5 the payload uses to power off.
pub const S5_SLEEP_TYPE: u8 = 5;

#[derive(Clone, Debug)]
pub struct RootInterrupts {
    pub ioapic: IoApicConfig,
    pub bridges: Vec<fstart_driver_amd::nbio::ioapic::rs::Bridge>,
}
#[derive(Clone, Debug)]
pub struct Root<'a> {
    pub physical: u8,
    /// The root bridge as its allocator has it: its buses, and what it
    /// forwards, which is its `_CRS`.
    pub pci: &'a fstart_pci::PciRoot,
    /// The initialized IOMMU; IVRS is published only when every root has one.
    pub iommu: Option<Iommu>,
    /// PCI requester aliases derived from actual topology, never copied BDFs.
    pub aliases: Vec<DeviceEntry>,
    pub interrupts: Option<RootInterrupts>,
    /// The root's SRAT proximity domain (`_PXM`), if known.
    pub proximity: Option<u32>,
}
impl Root<'_> {
    fn first(&self) -> u8 {
        *self.pci.buses().start()
    }
    fn last(&self) -> u8 {
        *self.pci.buses().end()
    }
    /// Whether the root forwards any memory in `base..end`.
    fn forwards(&self, base: u64, end: u64) -> bool {
        self.pci
            .mmio()
            .any(|m| base <= *m.end() && *m.start() < end)
    }
}
/// The OS console: the SoC's MMIO UART (AMDI0020, 48 MHz, 32-bit registers)
/// or a board's port-I/O 16550 (a Super I/O COM port).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Console {
    Mmio { base: u64, irq: u8 },
    Io { port: u16, irq: u8 },
}
pub struct Inputs<'a> {
    pub ids: &'a [u32],
    pub roots: &'a [Root<'a>],
    pub pstates: &'a [Pstate],
    pub cstates: Option<fstart_driver_amd::ccx::power::CstateInfo>,
    pub console: Option<Console>,
    pub hpet_id: u32,
    /// The FCH ACPI PM1 event block; control, timer and GPE0 follow it.
    pub pmbase: u16,
    /// FADT IA-PC boot flags: the board's legacy (LPC/eSPI) devices.
    pub iapc_boot_arch: u16,
    /// Proximity domains (SRAT/MSCT/SLIT when more than one) and the CPU's
    /// physical address width for the MSCT.
    pub numa: Option<&'a super::rs_numa::Numa>,
    pub phys_addr_bits: u8,
    /// The PSP's register window (1 MiB), when mapped.
    pub psp: Option<u64>,
    /// The PSP's EINJ service, when SMM opened it.
    pub einj: Option<Einj>,
}
/// The PSP's ACPI EINJ service: its RAS mailbox register and command
/// buffer (`fstart_driver_amd::psp::einj`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Einj {
    pub mailbox: u64,
    pub buffer: u64,
}
pub struct Tables {
    pub platform: PlatformConfig,
    pub dsdt: Vec<u8>,
    pub extra: Vec<Vec<u8>>,
}

/// Upper bound of the published CPU set (one SP5 socket, SMT2).
const MAX_CPUS: usize = 384;
/// The IOMMU's decoded register aperture; its slot's upper half may hold
/// the root's NBIO IOAPIC.
const IOMMU_APERTURE: u64 = fstart_driver_amd::nbio::iommu::MMIO_SIZE;

pub fn build(input: &Inputs) -> Result<Tables, AmlError> {
    if input.ids.is_empty()
        || input.ids.len() > MAX_CPUS
        || input.roots.is_empty()
        || input.roots.len() > 4
        || input.pmbase < 0x100
        || input.pmbase & 0xf != 0
        || input
            .ids
            .iter()
            .enumerate()
            .any(|(i, id)| input.ids[..i].contains(id))
    {
        return Err(AmlError::InvalidOperand);
    }
    for (index, root) in input.roots.iter().enumerate() {
        if root.physical >= 4
            // Memory for PCI devices below the ECAM window, and I/O.
            || !root.pci.allocatable().iter().any(|w| {
                w.kind == fstart_pci::PciWindowKind::Mmio && w.end() <= 0xe0000000
            })
            || !root
                .pci
                .allocatable()
                .iter()
                .any(|w| w.kind == fstart_pci::PciWindowKind::Io)
            || root.forwards(0xe0000000, 1 << 32)
            || root
                .iommu
                .is_some_and(|i| i.translation_enabled() || i.base == 0)
            || input.roots[..index].iter().any(|r| {
                r.physical == root.physical
                    || (r.first() <= root.last() && root.first() <= r.last())
            })
        {
            return Err(AmlError::InvalidRange);
        }
    }
    let irq_count = input
        .roots
        .iter()
        .filter(|r| r.interrupts.is_some())
        .count();
    let iommu_count = input.roots.iter().filter(|r| r.iommu.is_some()).count();
    if (irq_count != 0 && irq_count != input.roots.len())
        || (iommu_count != 0 && iommu_count != input.roots.len())
    {
        return Err(AmlError::InvalidOperand);
    }
    for root in input.roots {
        if let Some(irq) = &root.interrupts {
            let a = &irq.ioapic;
            if a.id != 0xf1 + root.physical
                || input.ids.contains(&u32::from(a.id))
                || a.gsi_base != 24 + 32 * u32::from(root.physical)
                || a.base & 0xffff != 0
                || a.base < 0x80000000
                || a.base >= 0xe0000000
                || irq.bridges.len() > 24
                || input.roots.iter().any(|r| {
                    let end = a.base + 0x10000;
                    r.forwards(a.base, end)
                        || r.iommu.is_some_and(|i| {
                            i.base
                                .checked_add(IOMMU_APERTURE)
                                .is_none_or(|limit| a.base < limit && i.base < end)
                        })
                        || (r.physical != root.physical
                            && r.interrupts
                                .as_ref()
                                .is_some_and(|i| i.ioapic.base == a.base))
                })
            {
                return Err(AmlError::InvalidRange);
            }
            for (i, b) in irq.bridges.iter().enumerate() {
                if b.slot >= 24
                    || b.interrupt_pin > 4
                    || b.devfn >> 3 == 0
                    || irq.bridges[..i]
                        .iter()
                        .any(|p| p.devfn == b.devfn || p.slot == b.slot)
                {
                    return Err(AmlError::InvalidOperand);
                }
            }
        }
    }
    let mut ioapics = vec![IoApicConfig {
        id: 0,
        base: 0xfec00000,
        gsi_base: 0,
    }];
    let mut extra_ioapics: Vec<_> = input
        .roots
        .iter()
        .filter_map(|r| r.interrupts.as_ref().map(|i| i.ioapic))
        .collect();
    extra_ioapics.sort_unstable_by_key(|i| i.gsi_base);
    ioapics.extend(extra_ioapics);
    let mut ids = input.ids.to_vec();
    ids.sort_unstable_by_key(|id| (id & 1, *id));
    let platform = PlatformConfig::X86(X86Config {
        apic_ids: ids.clone(),
        lapic_base: 0xfee00000,
        lapic_nmi_lint: Some(1),
        ioapics,
        isos: vec![
            IsoConfig {
                bus: 0,
                source: 0,
                gsi: 2,
                flags: 5,
            },
            IsoConfig {
                bus: 0,
                source: 9,
                gsi: 9,
                flags: 0xf,
            },
        ],
        hpet: Some(HpetConfig {
            base: 0xfed00000,
            timer_block_id: input.hpet_id,
            min_tick: fstart_driver_amd::fch::HPET_MIN_TICK,
        }),
        legacy_devices: true,
        sci_irq: fstart_driver_amd::fch::SCI_IRQ,
        pmbase: input.pmbase,
        gpe0_offset: 0x20,
        gpe0_length: 8,
        acpi_smi: None,
        fadt: X86Fadt {
            pm_profile: acpi::fadt::PmProfile::EnterpriseServer,
            iapc_boot_arch: input.iapc_boot_arch,
            // WBINVD, architectural C1, no fixed SleepButton, 32-bit PM timer.
            // No S4-RTC/S3/SMI/low-power/wake or security protection promises.
            flags: 0x125,
            p_lvl2_lat: 101,
            p_lvl3_lat: 1001,
            duty_offset: 0,
            duty_width: 0,
            pm2_cnt_offset: None,
        },
    });
    let sleep: Vec<u8> = acpi_dsl! {
        Name("_S0_", Package(0u8, 0u8, 0u8, 0u8));
        Name("_S5_", Package(#{byte S5_SLEEP_TYPE}, #{byte S5_SLEEP_TYPE}, 0u8, 0u8));
    }
    .into();
    let mut dsdt = scope_vec("\\", &sleep)?;
    for root in input.roots {
        dsdt.extend(root_aml(root)?);
    }
    match input.console {
        Some(Console::Mmio { base: uart, irq }) => {
            let irq = u32::from(irq);
            let mut serial = Vec::new();
            acpi_dsl! {
                Name("_HID","AMDI0020"); Name("_UID",1u8); Name("_STA",15u8);
                Name("_CRS",ResourceTemplate { Interrupt(ResourceConsumer,Edge,ActiveHigh,Exclusive,#{dword irq}); Memory32Fixed(ReadWrite,#{dword uart},0x1000u32); });
                Name("_DSD",Package(ToUUID("daffd814-6eba-4d8c-8a91-bc9bbf4aa301"),Package(Package("clock-frequency",48000000u32),Package("reg-io-width",4u8),Package("reg-shift",2u8))));
            }.emit(&mut serial)?;
            dsdt.extend(device_vec("FUR1", &serial)?);
        }
        Some(Console::Io { port, irq }) => dsdt.extend(
            acpi::devices::GenericAcpi {
                name: "COM1",
                hid: "PNP0501",
                resources: &[fstart_core::acpi::AcpiResource::Pio {
                    base: port,
                    size: 8,
                }],
                gsiv: Some(irq.into()),
                interrupt_flags: Default::default(),
            }
            .dsdt_aml(),
        ),
        None => {}
    }
    dsdt.extend(
        acpi::devices::GenericAcpi {
            name: "HPET",
            hid: "PNP0103",
            resources: &[fstart_core::acpi::AcpiResource::Mmio {
                base: 0xfed00000,
                size: 0x400,
            }],
            gsiv: None,
            interrupt_flags: Default::default(),
        }
        .dsdt_aml(),
    );
    let mut reserved = vec![
        fstart_core::acpi::AcpiResource::Mmio {
            base: 0xe0000000,
            size: 0x10000000,
        },
        fstart_core::acpi::AcpiResource::Mmio {
            base: 0xfed80000,
            size: 0x10000,
        },
        fstart_core::acpi::AcpiResource::Pio {
            base: input.pmbase,
            size: 0x30,
        },
        fstart_core::acpi::AcpiResource::Pio {
            base: 0xcf8,
            size: 8,
        },
        fstart_core::acpi::AcpiResource::Pio {
            base: 0xb00,
            size: 0x20,
        },
    ];
    if let Some(c) = input.cstates {
        let pm = u32::from(input.pmbase)..u32::from(input.pmbase) + 0x30;
        if !pm.contains(&u32::from(c.io_base)) {
            reserved.push(fstart_core::acpi::AcpiResource::Pio {
                base: c.io_base,
                size: 8,
            });
        }
    }
    for root in input.roots {
        if let Some(irq) = &root.interrupts {
            reserved.push(fstart_core::acpi::AcpiResource::Mmio {
                base: irq.ioapic.base,
                size: 0x10000,
            });
        }
        if let Some(iommu) = root.iommu {
            reserved.push(fstart_core::acpi::AcpiResource::Mmio {
                base: iommu.base,
                size: IOMMU_APERTURE,
            });
        }
    }
    if let Some(base) = input.psp {
        reserved.push(fstart_core::acpi::AcpiResource::Mmio {
            base,
            size: fstart_driver_amd::nbio::iohc::PSP_BAR_SIZE,
        });
    }
    dsdt.extend(
        acpi::devices::GenericAcpi {
            name: "MBRD",
            hid: "PNP0C02",
            resources: &reserved,
            gsiv: None,
            interrupt_flags: Default::default(),
        }
        .dsdt_aml(),
    );
    let power = PowerAml::new(&Power {
        pstates: input.pstates,
        cstates: input.cstates,
        threads_per_core: 2,
        // C2 (CC6) exit latency of the NFX firmware's _CST (320h).
        c2_latency_us: 800,
    });
    let mut processors = Vec::new();
    for (uid, id) in ids.iter().enumerate() {
        processors.extend(acpi::processor::processor_device(
            acpi::processor::ProcessorName::new(uid as u16),
            uid as u32,
            &power.for_cpu(*id),
        )?);
    }
    let processors = scope_vec("\\_SB_", &processors)?;
    let mut ssdt = acpi::sdt::Sdt::new(
        *b"SSDT",
        36,
        2,
        acpi::OEM_ID,
        acpi::OEM_TABLE_ID,
        acpi::OEM_REVISION,
    );
    ssdt.append_slice(&processors);
    ssdt.update_checksum();
    let mut mcfg = acpi::mcfg::MCFG::new(acpi::OEM_ID, acpi::OEM_TABLE_ID, acpi::OEM_REVISION);
    mcfg.add_ecam(0xe0000000, 0, 0, 255);
    let mut extra = vec![acpi::serialize(&ssdt), acpi::serialize(&mcfg)];
    // A root's `_PXM` names an SRAT domain: none without SRAT.
    let domains = input.numa.map_or(0, |n| n.domains);
    if input
        .roots
        .iter()
        .any(|r| r.proximity.is_some_and(|d| d as usize >= domains))
    {
        return Err(AmlError::InvalidOperand);
    }
    if let Some(numa) = input.numa {
        if numa
            .cpus
            .iter()
            .any(|c| c.domain as usize >= numa.domains || !input.ids.contains(&c.apic_id))
            || numa.cpus.len() != input.ids.len()
        {
            return Err(AmlError::InvalidOperand);
        }
        // CPUs grouped by domain like the NFX SRAT: Linux numbers its nodes in
        // SRAT order, so node N is then PXM N, as on the vendor firmware.
        let mut cpus = numa.cpus.clone();
        cpus.sort_unstable_by_key(|c| (c.domain, c.apic_id));
        // Both vendor SLITs: 10 local, 12 between the quadrants of a socket.
        extra.extend(fstart_acpi::numa::tables(
            numa.domains,
            &numa.memory,
            cpus.into_iter(),
            input.phys_addr_bits,
            |_, _| 12,
        ));
    }
    if iommu_count != 0 {
        extra.push(ivrs(input)?);
    }
    if let Some(einj) = input.einj {
        extra.push(fstart_acpi::einj::table(
            acpi::OEM_ID,
            acpi::OEM_TABLE_ID,
            acpi::OEM_REVISION,
            &fstart_driver_amd::psp::einj::entries(einj.mailbox, einj.buffer),
        ));
    }
    match input.console {
        Some(Console::Mmio { base, .. }) => {
            extra.push(acpi::serialize(&acpi::spcr::build_spcr_16550_mmio32(
                base, 48000000,
            )));
            extra.push(acpi::serialize(&acpi::dbg2::build_dbg2_16550_mmio32(
                &acpi::dbg2::Dbg2MmioConfig {
                    base_addr: base,
                    addr_size: 0x1000,
                    namespace: "\\_SB_.FUR1",
                },
            )));
        }
        Some(Console::Io { port, irq }) => {
            extra.push(acpi::serialize(&acpi::spcr::build_spcr_16550_io(port, irq)));
        }
        None => {}
    }
    Ok(Tables {
        platform,
        dsdt,
        extra,
    })
}
/// IVRS for roots that all carry an initialized IOMMU.
fn ivrs(input: &Inputs) -> Result<Vec<u8>, AmlError> {
    let entries: Vec<Vec<DeviceEntry>> = input
        .roots
        .iter()
        .map(|root| {
            let mut entries = vec![DeviceEntry::Range {
                start: fstart_pci::bdf(root.first(), 0, 3),
                end: fstart_pci::bdf(root.last(), 31, 7),
                dte: 0,
            }];
            if let Some(irq) = &root.interrupts {
                // NBIO IOAPIC requester is the host bridge's function1;
                // Genoa domain/IVRS contract, not an endpoint alias.
                entries.push(DeviceEntry::Special {
                    variety: Variety::IoApic,
                    handle: irq.ioapic.id,
                    source: fstart_pci::bdf(root.first(), 0, 1),
                    dte: 0,
                });
            }
            if root.first() == 0 {
                entries.extend([
                    DeviceEntry::Special {
                        variety: Variety::IoApic,
                        handle: 0,
                        source: 0xa0,
                        dte: (Dte::LINT1_PASS::SET
                            + Dte::LINT0_PASS::SET
                            + Dte::SYS_MGT::NoTranslate
                            + Dte::NMI_PASS::SET
                            + Dte::EXT_INT_PASS::SET
                            + Dte::INIT_PASS::SET)
                            .value,
                    },
                    DeviceEntry::Special {
                        variety: Variety::Hpet,
                        handle: 0,
                        source: 0xa0,
                        dte: 0,
                    },
                ]);
            }
            entries.extend_from_slice(&root.aliases);
            entries
        })
        .collect();
    let units: Vec<_> = input
        .roots
        .iter()
        .zip(&entries)
        .map(|(root, devices)| {
            let i = root.iommu.ok_or(AmlError::InvalidOperand)?;
            Ok(ivrs::Iommu {
                device_id: fstart_pci::bdf(root.first(), 0, 2),
                capability_offset: 0x40,
                segment: 0,
                base: i.base,
                registers: ivrs::IommuRegisters {
                    capability: i.capability,
                    range: i.range,
                    misc: i.misc,
                    control: i.control,
                    efr: i.efr,
                    efr2: i.efr2,
                    counter_config: i.counter_config,
                },
                devices,
            })
        })
        .collect::<Result<_, AmlError>>()?;
    ivrs::build_ivrs(&ivrs::IvrsConfig {
        iommus: &units,
        acpi_hid: true,
    })
    .ok_or(AmlError::InvalidOperand)
}
fn root_aml(root: &Root<'_>) -> Result<Vec<u8>, AmlError> {
    let uid = u64::from(root.physical);
    let bus = u64::from(root.first());
    let mut body = Vec::new();
    acpi_dsl! {
        Name("_HID",EisaId("PNP0A08")); Name("_CID",EisaId("PNP0A03")); Name("_UID",#{byte uid}); Name("_SEG",0u8); Name("_BBN",#{byte bus}); Name("_CCA",1u8);
    }.emit(&mut body)?;
    if let Some(domain) = root.proximity {
        acpi_dsl! { Name("_PXM", #{dword domain}); }.emit(&mut body)?;
    }
    if let Some(irq) = &root.interrupts {
        use fstart_driver_amd::nbio::ioapic::rs::ROUTES;
        // The root complex's own device 0 functions (RCEC 00.3) raise INTA on
        // IOAPIC pin 1, as the NFX firmware's root `_PRT` publishes on every
        // S8040 root (AR00/AR03: 0xFFFF, pin 0, GSI base + 1).
        let mut entries = alloc::vec![(0u8, 0u8, irq.ioapic.gsi_base + 1)];
        for bridge in &irq.bridges {
            if bridge.interrupt_pin != 0 {
                let entry = (
                    bridge.devfn >> 3,
                    bridge.interrupt_pin - 1,
                    irq.ioapic.gsi_base + u32::from(ROUTES[bridge.slot as usize].map),
                );
                if let Some(old) = entries
                    .iter()
                    .find(|&&(d, p, _)| d == entry.0 && p == entry.1)
                {
                    if old != &entry {
                        return Err(AmlError::InvalidOperand);
                    }
                } else {
                    entries.push(entry);
                }
            }
            let mut child = Vec::new();
            let adr = (u32::from(bridge.devfn >> 3) << 16) | u32::from(bridge.devfn & 7);
            Name::new("_ADR".into(), &adr).to_aml_bytes(&mut child);
            // PCIe downstream ports and integrated NBIF present device0;
            // additional PCI-to-PCI levels use the OS's standard swizzle.
            child.extend(acpi::pirq::prt_name_aml((0..4u8).map(|pin| {
                (
                    0u8,
                    pin,
                    irq.ioapic.gsi_base
                        + u32::from(ROUTES[bridge.slot as usize].secondary_pin(pin)),
                )
            })));
            body.extend(device_vec(&format!("P{:03X}", bridge.devfn), &child)?);
        }
        body.extend(acpi::pirq::prt_name_aml(entries.into_iter()));
    }
    // PCIe capability-structure access only. Native PME/AER/hotplug/LTR/DPC
    // interrupt/wake prerequisites are not initialized/validated by this flow.
    body.extend(acpi::osc::pci_host_osc(0x10));
    // What the root forwards, as its allocator has it.
    body.extend(acpi::pci_root::crs(root.pci));
    device_vec(&format!("S0B{}", root.physical), &body)
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub struct Runtime<'a> {
    pub roots: &'a [fstart_driver_amd::df::v4::Root],
    /// Initialized IOMMUs, one per root, or none.
    pub iommus: &'a [super::rs_iommu::Mapped],
    pub cpu: &'a super::rs_cpu::Snapshot,
    /// NBIO IOAPIC routing, one per root in `roots` order, or none.
    pub interrupts: &'a [RootInterrupts],
    /// The allocator of each root, in `roots` order: its root bridge is
    /// what the tables describe.
    pub pci: &'a [fstart_pci::PciEcam],
    pub console: Option<Console>,
    pub hpet_id: u32,
    pub pmbase: u16,
    pub iapc_boot_arch: u16,
    pub numa: Option<&'a super::rs_numa::Numa>,
    pub psp: Option<u64>,
    pub einj: Option<Einj>,
}
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn install(runtime: Runtime<'_>) -> Result<u64, AmlError> {
    #[repr(C, align(4096))]
    struct Storage([u8; 0x80000]);

    let count = runtime.roots.len();
    if count == 0
        || runtime.pci.len() != count
        || !(runtime.interrupts.is_empty() || runtime.interrupts.len() == count)
        || !(runtime.iommus.is_empty() || runtime.iommus.len() == count)
    {
        return Err(AmlError::InvalidOperand);
    }
    let roots: Vec<Root> = runtime
        .roots
        .iter()
        .enumerate()
        .map(|(index, root)| {
            let iommu = if runtime.iommus.is_empty() {
                None
            } else {
                Some(
                    runtime
                        .iommus
                        .iter()
                        .find(|i| i.root.ios.physical_index == root.ios.physical_index)
                        .ok_or(AmlError::InvalidOperand)?
                        .registers,
                )
            };
            let pci = (runtime.pci[index].roots().first())
                .filter(|pci| pci.buses() == (root.route.base..=root.route.limit))
                .ok_or(AmlError::InvalidOperand)?;
            Ok(Root {
                physical: root.ios.physical_index,
                pci,
                iommu,
                aliases: vec![],
                interrupts: runtime.interrupts.get(index).cloned(),
                proximity: runtime
                    .numa
                    .and_then(|n| super::rs_numa::ios_domain(n.nps, root.ios.physical_index)),
            })
        })
        .collect::<Result<_, AmlError>>()?;
    let tables = build(&Inputs {
        ids: &runtime.cpu.ids,
        roots: &roots,
        pstates: &runtime.cpu.pstates,
        cstates: runtime.cpu.cstates,
        console: runtime.console,
        hpet_id: runtime.hpet_id,
        pmbase: runtime.pmbase,
        iapc_boot_arch: runtime.iapc_boot_arch,
        numa: runtime.numa,
        phys_addr_bits: super::rs_resources::physical_address_bits(),
        psp: runtime.psp,
        einj: runtime.einj,
    })?;
    // Allocate directly in the reserved early heap, not a large BSS or
    // temporary stack array. Deliberately retain through OS handoff/reset.
    let pointer = core::ptr::NonNull::new(unsafe {
        alloc::alloc::alloc_zeroed(alloc::alloc::Layout::new::<Storage>())
    })
    .ok_or(AmlError::Capacity)?;
    let storage = unsafe { &mut (*pointer.cast::<Storage>().as_ptr()).0 };
    let address = storage.as_ptr() as u64;
    let length = acpi::platform::assemble_into(
        storage,
        address,
        &tables.platform,
        &tables.dsdt,
        &tables.extra,
    )?;
    let rsdp = &storage[..36];
    if rsdp.iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0 {
        return Err(AmlError::InvalidOperand);
    }
    let xsdt = u64::from_le_bytes(rsdp[24..32].try_into().unwrap())
        .checked_sub(address)
        .ok_or(AmlError::InvalidOperand)? as usize;
    let xsdt_size = u32::from_le_bytes(storage[xsdt + 4..xsdt + 8].try_into().unwrap()) as usize;
    let entries = storage
        .get(xsdt + 36..xsdt + xsdt_size)
        .ok_or(AmlError::InvalidRange)?;
    for pointer in entries.chunks_exact(8) {
        let offset = u64::from_le_bytes(pointer.try_into().unwrap())
            .checked_sub(address)
            .ok_or(AmlError::InvalidOperand)? as usize;
        let header = storage
            .get(offset..offset + 36)
            .ok_or(AmlError::InvalidRange)?;
        let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let bytes = storage
            .get(offset..offset + size)
            .ok_or(AmlError::InvalidRange)?;
        if bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0 {
            return Err(AmlError::InvalidOperand);
        }
        fstart_log::info!(
            "rs: native ACPI table={:#x} length={} checksum=ok",
            u32::from_le_bytes(header[..4].try_into().unwrap()),
            size
        );
    }
    fstart_log::info!(
        "rs: native ACPI RSDP={:#x} bytes={} tables={} (reserved through OS handoff)",
        address,
        length,
        entries.len() / 8
    );
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root bridge as its allocator would have it: the buses from `first`,
    /// I/O and low memory for PCI devices, the legacy VGA memory if it is
    /// routed there, and a window above 4 GiB if there is one.
    fn pci_root(
        first: u8,
        io: core::ops::Range<u64>,
        memory: core::ops::Range<u64>,
        memory64: Option<core::ops::Range<u64>>,
        vga: bool,
    ) -> &'static fstart_pci::PciRoot {
        use fstart_pci::PciRole::{Forwarded, Window};
        let memory64 = memory64.unwrap_or(0..0);
        let root = fstart_pci::PciRoot::new(
            fstart_pci::PciRootInfo {
                segment: 0,
                ecam_base: 0xe000_0000,
                bus_start: first,
                bus_end: first + 0x3f,
            },
            [
                Window.io(io.start, io.end - io.start),
                Window.mmio(memory.start, memory.end - memory.start),
                Forwarded.mmio(0xa_0000, if vga { 0x2_0000 } else { 0 }),
                Window.mmio(memory64.start, memory64.end - memory64.start),
            ],
        )
        .unwrap();
        alloc::boxed::Box::leak(alloc::boxed::Box::new(root))
    }
    #[test]
    fn native_tables_have_one_ioapic_and_four_real_iommus_not_sp3_defaults() {
        let roots: Vec<_> = [0xc0, 0x80, 0, 0x40]
            .into_iter()
            .enumerate()
            .map(|(n, first)| Root {
                physical: n as u8,
                pci: pci_root(
                    first,
                    0x1000..0x4000,
                    (0xb8000000 + n as u64 * 0x8000000)..(0xc0000000 + n as u64 * 0x8000000),
                    None,
                    n == 0,
                ),
                iommu: Some(Iommu {
                    base: 0xd8000000 + n as u64 * 0x100000,
                    capability: 0x190b640f,
                    misc: 0x203460,
                    efr: 0x25bf732fa2095ade,
                    efr2: 0xd,
                    ..Default::default()
                }),
                aliases: vec![],
                interrupts: None,
                proximity: None,
            })
            .collect();
        let ids: Vec<_> = (0..160).collect();
        let tables = build(&Inputs {
            ids: &ids,
            roots: &roots,
            pstates: &[],
            cstates: None,
            console: Some(Console::Mmio {
                base: 0xfedca000,
                irq: 4,
            }),
            hpet_id: 0x10228201,
            pmbase: 0x400,
            iapc_boot_arch: 0,
            numa: None,
            phys_addr_bits: 48,
            psp: None,
            einj: None,
        })
        .unwrap();
        let mut blob = vec![0u8; 0x80000];
        let length = acpi::platform::assemble_into(
            &mut blob,
            0x8000000,
            &tables.platform,
            &tables.dsdt,
            &tables.extra,
        )
        .unwrap();
        blob.truncate(length);
        assert!(blob.len() < 0x80000);
        let fadt = blob
            .windows(4)
            .position(|signature| signature == b"FACP")
            .unwrap();
        let flags = u32::from_le_bytes(blob[fadt + 112..fadt + 116].try_into().unwrap());
        assert_ne!(flags & (1 << 5), 0, "no fixed SleepButton exists");
        assert_eq!(flags & (1 << 7), 0, "no S4 RTC wake is implemented");
        for extra in &tables.extra {
            assert_eq!(extra.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
        }
        assert_eq!(&tables.extra[0][..4], b"SSDT");
        assert_eq!(
            tables.extra[0]
                .windows(8)
                .filter(|b| *b == b"ACPI0007")
                .count(),
            160
        );
        let spcr = &tables.extra[3];
        assert_eq!((spcr[36], spcr[41], spcr[43], spcr[52]), (0x12, 32, 3, 0));
        let mut routed = roots.clone();
        for root in &mut routed {
            root.interrupts = Some(RootInterrupts {
                ioapic: IoApicConfig {
                    id: 0xf1 + root.physical,
                    base: 0xd8080000 + u64::from(root.physical) * 0x100000,
                    gsi_base: 24 + 32 * u32::from(root.physical),
                },
                bridges: vec![fstart_driver_amd::nbio::ioapic::rs::Bridge {
                    slot: 22,
                    devfn: 0x39,
                    interrupt_pin: 1,
                }],
            });
        }
        let make = |roots: &[Root<'_>]| {
            build(&Inputs {
                ids: &ids,
                roots,
                pstates: &[],
                cstates: None,
                console: Some(Console::Mmio {
                    base: 0xfedca000,
                    irq: 4,
                }),
                hpet_id: 0x10228201,
                pmbase: 0x400,
                iapc_boot_arch: 0,
                numa: None,
                phys_addr_bits: 48,
                psp: None,
                einj: None,
            })
        };
        let tables = make(&routed).unwrap();
        let PlatformConfig::X86(config) = &tables.platform else {
            panic!()
        };
        assert_eq!(config.ioapics.len(), 5);
        for i in 0..4 {
            assert_eq!(
                (config.ioapics[i + 1].id, config.ioapics[i + 1].gsi_base),
                (0xf1 + i as u8, 24 + 32 * i as u32)
            );
        }
        assert_eq!(tables.dsdt.windows(4).filter(|w| *w == b"_PRT").count(), 8);
        for root in &routed {
            let expected = [0x48, 0, 0, 0, 0xf1 + root.physical, 1, root.first(), 1];
            assert_eq!(
                tables.extra[2]
                    .windows(8)
                    .filter(|w| *w == expected)
                    .count(),
                3,
                "all IVHD formats need their actual IOAPIC source"
            );
        }
        let mut blob = vec![0; 0x80000];
        assert!(
            acpi::platform::assemble_into(
                &mut blob,
                0x8000000,
                &tables.platform,
                &tables.dsdt,
                &tables.extra
            )
            .is_ok()
        );
        routed[0].interrupts = None;
        assert!(make(&routed).is_err());
        routed[0].interrupts = routed[1].interrupts.clone();
        assert!(make(&routed).is_err());
    }

    #[test]
    fn three_roots_without_iommus_publish_windows_and_a_port_io_console() {
        // Siena: IOS1 fused off, no IOMMU/NBIO IOAPIC initialized yet.
        let roots: Vec<_> = [(0u8, 0xc0u8), (2, 0), (3, 0x40)]
            .into_iter()
            .enumerate()
            .map(|(n, (physical, first))| Root {
                physical,
                pci: pci_root(
                    first,
                    (0x4000 * n as u64).max(0x1000)..0x4000 * (n as u64 + 1),
                    (0xb8000000 + n as u64 * 0x8000000)..(0xc0000000 + n as u64 * 0x8000000),
                    Some(
                        (0x100_0000_0000 + n as u64 * (1 << 38))
                            ..(0x100_0000_0000 + (n as u64 + 1) * (1 << 38)),
                    ),
                    false,
                ),
                iommu: None,
                aliases: vec![],
                interrupts: None,
                proximity: None,
            })
            .collect();
        let ids: Vec<_> = (0..128).collect();
        let tables = build(&Inputs {
            ids: &ids,
            roots: &roots,
            pstates: &[],
            cstates: None,
            console: Some(Console::Io {
                port: 0x3f8,
                irq: 4,
            }),
            hpet_id: 0x10228201,
            pmbase: 0x800,
            iapc_boot_arch: acpi::platform::x86::IAPC_LEGACY_DEVICES,
            numa: None,
            phys_addr_bits: 48,
            psp: None,
            einj: None,
        })
        .unwrap();
        let signatures: Vec<_> = tables.extra.iter().map(|t| &t[..4]).collect();
        assert_eq!(signatures, [b"SSDT", b"MCFG", b"SPCR"]);
        assert!(tables.dsdt.windows(4).any(|w| w == b"COM1"));
        // One QWord memory producer (descriptor 8Ah) per root.
        assert_eq!(
            tables
                .dsdt
                .windows(3)
                .filter(|w| w[0] == 0x8a && w[1] == 0x2b && w[2] == 0)
                .count(),
            3
        );
        let PlatformConfig::X86(config) = &tables.platform else {
            panic!()
        };
        assert_eq!(config.pmbase, 0x800);
        assert_eq!(config.ioapics.len(), 1);
        let mut blob = vec![0u8; 0x80000];
        acpi::platform::assemble_into(
            &mut blob,
            0x7548_0000,
            &tables.platform,
            &tables.dsdt,
            &tables.extra,
        )
        .unwrap();
        // `_PXM` only with an SRAT that has the domain.
        let mut near = roots.clone();
        for root in &mut near {
            root.proximity =
                crate::rs_numa::ios_domain(fstart_driver_amd::apob::nps::Nps::Nps4, root.physical);
        }
        let inputs = |roots, numa| Inputs {
            ids: &ids,
            roots,
            pstates: &[],
            cstates: None,
            console: None,
            hpet_id: 1,
            pmbase: 0x800,
            iapc_boot_arch: 0,
            numa,
            phys_addr_bits: 48,
            psp: None,
            einj: None,
        };
        assert!(build(&inputs(&near, None)).is_err());
        let numa = crate::rs_numa::Numa {
            nps: fstart_driver_amd::apob::nps::Nps::Nps4,
            domains: 4,
            memory: vec![fstart_acpi::numa::MemoryAffinity {
                base: 0,
                end: 1 << 30,
                domain: 1,
            }],
            cpus: ids
                .iter()
                .map(|&apic_id| fstart_acpi::numa::CpuAffinity {
                    apic_id,
                    domain: apic_id >> 5,
                })
                .collect(),
        };
        let dsdt = build(&inputs(&near, Some(&numa))).unwrap().dsdt;
        // Name(_PXM, DWord): 08 "_PXM" 0C, value. IOS0/2/3: 1/3/2.
        let pxm: Vec<_> = dsdt
            .windows(10)
            .filter(|w| w[..6] == *b"\x08_PXM\x0c")
            .map(|w| u32::from_le_bytes(w[6..].try_into().unwrap()))
            .collect();
        assert_eq!(pxm, [1, 3, 2]);
        let mut far = near.clone();
        far[1].proximity = Some(4);
        assert!(build(&inputs(&far, Some(&numa))).is_err());
        // All roots or none carry IOMMUs.
        let mut mixed = roots.clone();
        mixed[0].iommu = Some(Iommu {
            base: 0xd0000000,
            ..Default::default()
        });
        assert!(
            build(&Inputs {
                ids: &ids,
                roots: &mixed,
                pstates: &[],
                cstates: None,
                console: None,
                hpet_id: 1,
                pmbase: 0x800,
                iapc_boot_arch: 0,
                numa: None,
                phys_addr_bits: 48,
                psp: None,
                einj: None,
            })
            .is_err()
        );
    }
}
