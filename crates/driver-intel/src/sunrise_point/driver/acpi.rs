// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCH-H ACPI from canonical PCI inventory, actual coalesced physical-port
//! identities and the interrupt policy the native owner programmed.
//! Reference acpi/pci_irqs.asl SHA256:
//! a16ed93d96df1f3c106658c6cdd8d71865768f8bd3f74ce5558e20efea49a66a
//! Reference acpi/gpio.asl SHA256:
//! fe78608167b357eb9a4e1e297f4120640a9261e2db16a7cc4af53c385040406e
//! Reference acpi/pcie.asl SHA256:
//! 947a253d966d824bf817913d31162f7438374928d0c3dc3eb6d844fcf1352ecc
use super::*;
use crate::{namespace::SouthbridgeAcpi, southbridge::pirq::PciPin};
use alloc::vec::Vec;
use fstart_acpi::{
    AcpiPath,
    platform::x86::{
        HpetConfig, IAPC_LEGACY_DEVICES, IoApicConfig, IsoConfig, X86Config, X86Fadt,
        X86PlatformProvider,
    },
};
use fstart_acpi_macros::acpi_dsl;
use fstart_pci::PCI_INTERRUPT_PIN;

struct Function {
    address: PciAddress,
    pin: Option<PciPin>,
    gsi: Option<u8>,
    port: Option<u8>,
}
pub(super) struct Snapshot {
    // This owner also exists in CAR: even dropping an empty heap Vec would
    // retain allocator glue in no-heap stages. Bus 0 has at most 32*8 slots.
    functions: heapless::Vec<Function, { 32 * 8 }>,
}
impl Snapshot {
    /// `excluded` contains both disabled and SkipInit functions kept out of
    /// allocation. Only visible functions need routes.
    pub(super) fn capture(
        pci: &PciEcam,
        ports: &pcie::Ports,
        excluded: &[PciAddress],
    ) -> Result<Self, Error> {
        let mut functions = heapless::Vec::new();
        for address in pci
            .devices()
            .chain(excluded.iter().copied())
            .filter(|a| a.bus() == 0 && a.segment() == 0)
        {
            let dev = pci.device(address).ok_or(Error::Policy)?;
            if !dev.is_present() {
                continue;
            }
            let raw = dev.read8(PCI_INTERRUPT_PIN);
            let pin = PciPin::from_config(raw);
            let port = ports.port_number(address);
            // RP remapping preserves the ORIGINAL physical pin. Never look up
            // its new function in the pre-coalescing devintconfig table.
            let gsi = pin.and_then(|pin| root_gsi(address, pin, port.is_some()));
            // The PCI inventory is wider than the native _PRT. It can include
            // functions the board leaves untouched. Do not invent a route or
            // abort PCI initialization just because no known INTx route exists.
            // Root ports get the whole native slot _PRT below, pin or not.
            if gsi.is_none() && raw != 0 && port.is_none() {
                fstart_log::warn!(
                    "PCH ACPI: no INTx route for {:?} (pin {:#04x})",
                    fstart_log::DebugValue(&address),
                    raw
                );
            }
            functions
                .push(Function {
                    address,
                    pin,
                    gsi,
                    port,
                })
                .map_err(|_| Error::Policy)?;
        }
        // PCI0 _CRS and PDRC now come exclusively from the PciRoot retained
        // by the allocator and rendered by namespace::pci_root_resources.
        Ok(Self { functions })
    }

    #[cfg(not(target_os = "none"))]
    pub(super) fn host_baseline(config: &Config) -> Self {
        let mut functions = heapless::Vec::new();
        for (rp, port) in config.pcie.ports.iter().enumerate() {
            if !port.enabled {
                continue;
            }
            let rp = rp as u8;
            let slot = match rp / 8 {
                0 => 0x1c,
                1 => 0x1d,
                _ => 0x1b,
            };
            let address = PciAddress::new(0, 0, slot, rp % 8);
            let pin = PciPin::from_config((rp % 4) + 1);
            let gsi = pin.and_then(|pin| root_gsi(address, pin, true));
            let _ = functions.push(Function {
                address,
                pin,
                gsi,
                port: Some(rp + 1),
            });
        }
        for &devfn in config.interrupt_devices {
            let dev = devfn >> 3;
            let func = devfn & 7;
            if matches!(dev, 0x1b..=0x1d) {
                continue;
            }
            let address = PciAddress::new(0, 0, dev, func);
            let (pin, gsi) = irq::DEVICE_INTERRUPTS
                .iter()
                .find(|e| e.devfn == devfn)
                .map(|e| (Some(e.pin), Some(e.gsi)))
                .unwrap_or((None, None));
            let _ = functions.push(Function {
                address,
                pin,
                gsi,
                port: None,
            });
        }
        Self { functions }
    }
}
fn root_gsi(address: PciAddress, pin: PciPin, root_port: bool) -> Option<u8> {
    if root_port || address.device() == 1 {
        return Some(16 + pin.index());
    }
    let devfn = fstart_pci::devfn(address.device(), address.function());
    if let Some(entry) = irq::DEVICE_INTERRUPTS
        .iter()
        .find(|e| e.devfn == devfn && e.pin == pin)
    {
        return Some(entry.gsi);
    }
    // Native PCI0 _PRT also describes these alternate pins. Publish them
    // only if observed on their actual function, not on unrelated devices.
    match (address.device(), address.function(), pin) {
        (0x1f, 4, PciPin::B) => Some(17), // SMBUS_INTB_IRQ
        (0x1f, 6, PciPin::C) => Some(18), // GbE_INTC_IRQ
        (0x1f, 7, PciPin::D) | (0x14, 3, PciPin::D) => Some(19), // TRACE_HUB/CIO_INTD_IRQ
        (2 | 4 | 5 | 8, 0, PciPin::A) => Some(16), // SA endpoints
        _ => None,
    }
}

impl IntelSunrisePoint {
    fn acpi_ready(&self) {
        #[cfg(not(target_os = "none"))]
        if self.host_baseline {
            assert!(self.acpi.is_some());
            return;
        }
        assert!(
            self.phase == Phase::Initialized && self.failure.is_none() && self.acpi.is_some(),
            "PCH ACPI before successful PCI/device initialization"
        );
        // An installed handler, not merely a configured SMI command port.
        let control = unsafe { self.pmc.register::<u32, pmc::GenPmconA::Register>(0xa0) };
        assert!(
            control.is_set(pmc::GenPmconA::SMI_LOCK) && self.smi.is_some(),
            "native ACPI requires installed/locked SMM"
        );
    }
}
impl X86PlatformProvider for IntelSunrisePoint {
    fn x86_platform_config(&self, online_cpus: u32) -> X86Config {
        self.acpi_ready();
        #[cfg(not(target_os = "none"))]
        let (apic_ids, lapic_base, ioapic_id, hpet_id) = if self.host_baseline {
            (
                (0..online_cpus).collect(),
                0xfee0_0000u64,
                irq::IOAPIC_ID,
                0x8086_a701u32,
            )
        } else {
            let apic_ids: Vec<u32> = fstart_arch::x86::mp::online_apic_ids().collect();
            assert_eq!(apic_ids.len(), online_cpus as usize);
            let ioapic = unsafe { fstart_arch::x86::ioapic::IoApic::new(irq::IOAPIC_BASE) };
            assert_eq!(ioapic.redirection_entries(), u32::from(irq::IOAPIC_ENTRIES));
            assert_eq!(ioapic.id(), irq::IOAPIC_ID);
            let hpet_id = unsafe { fstart_core::mmio::MmioRegister::<u32>::new(0xfed00000) }.get();
            assert!(
                hpet_id != 0 && hpet_id != u32::MAX,
                "HPET decode unavailable"
            );
            (
                apic_ids,
                fstart_arch::x86::lapic::Lapic::from_msr().base() as u64,
                ioapic.id(),
                hpet_id,
            )
        };
        #[cfg(target_os = "none")]
        let (apic_ids, lapic_base, ioapic_id, hpet_id) = {
            let apic_ids: Vec<u32> = fstart_arch::x86::mp::online_apic_ids().collect();
            assert_eq!(apic_ids.len(), online_cpus as usize);
            let ioapic = unsafe { fstart_arch::x86::ioapic::IoApic::new(irq::IOAPIC_BASE) };
            assert_eq!(ioapic.redirection_entries(), u32::from(irq::IOAPIC_ENTRIES));
            assert_eq!(ioapic.id(), irq::IOAPIC_ID);
            let hpet_id = unsafe { fstart_core::mmio::MmioRegister::<u32>::new(0xfed00000) }.get();
            assert!(
                hpet_id != 0 && hpet_id != u32::MAX,
                "HPET decode unavailable"
            );
            (
                apic_ids,
                fstart_arch::x86::lapic::Lapic::from_msr().base() as u64,
                ioapic.id(),
                hpet_id,
            )
        };
        let sci = self.config.legacy.interrupts.sci_irq;
        // Native acpi_create_madt_ioapic_from_hw(): ISA IRQ0 is IO-APIC pin 2
        // whether or not the 8254 is clock gated. With the 8254 gated, the
        // OS drives IRQ0 from the HPET in legacy replacement mode, which uses
        // the same pin; leaving the override out makes Linux probe pin 0.
        let mut isos = alloc::vec![IsoConfig {
            bus: 0,
            source: 0,
            gsi: 2,
            flags: 5,
        }];
        if sci < 16 {
            isos.push(IsoConfig {
                bus: 0,
                source: sci,
                gsi: u32::from(sci),
                flags: 0xd,
            });
        }
        use fstart_acpi::fadt::{Flags, PmProfile};
        X86Config {
            apic_ids,
            lapic_base,
            lapic_nmi_lint: Some(1),
            ioapics: alloc::vec![IoApicConfig {
                id: ioapic_id,
                base: irq::IOAPIC_BASE as u64,
                gsi_base: 0
            }],
            isos,
            hpet: Some(HpetConfig {
                base: 0xfed00000,
                timer_block_id: hpet_id,
                min_tick: 0,
            }),
            legacy_devices: self.config.legacy.pcat,
            sci_irq: sci,
            pmbase: pmc::ACPI_BASE,
            gpe0_offset: 0x80,
            gpe0_length: 32,
            acpi_smi: Some(fstart_core::acpi::AcpiSmiConfig {
                smi_cmd: 0xb2,
                acpi_enable: 0xe1,
                acpi_disable: 0x1e,
            }),
            fadt: X86Fadt {
                pm_profile: if self.config.laptop {
                    PmProfile::Mobile
                } else {
                    PmProfile::Desktop
                },
                iapc_boot_arch: if self.config.legacy.pcat {
                    IAPC_LEGACY_DEVICES
                } else {
                    0
                },
                flags: Flags::Wbinvd as u32
                    | Flags::ProcC1 as u32
                    | Flags::SlpButton as u32
                    | Flags::RtcS4 as u32
                    | Flags::UsePlatformClock as u32,
                p_lvl2_lat: u16::MAX,
                p_lvl3_lat: u16::MAX,
                duty_offset: 0,
                duty_width: 0,
                pm2_cnt_offset: None,
                pm1_evt_word_access: false,
                gpe0_byte_access: false,
            },
        }
    }
}
impl SouthbridgeAcpi for IntelSunrisePoint {
    fn dsdt_aml(&self, config: &Config, pci_root: &AcpiPath) -> Vec<u8> {
        self.acpi_ready();
        assert!(core::ptr::eq(config, self.config));
        let snapshot = self.acpi.as_ref().expect("canonical PCI snapshot");
        let mut bytes: Vec<u8> = acpi_dsl! { Scope("\\") {
            Name("PICM",0u32);
            Method("_PIC",1,NotSerialized) { PICM=Arg0; }
            Name("_S0_",Package(0u32,0u32,0u32,0u32));
            // SLP_TYP 6: the SMM sleep handler powers off as for S5; the
            // OS has saved its image. No retained-memory S3 is advertised.
            Name("_S4_",Package(6u32,0u32,0u32,0u32));
            Name("_S5_",Package(7u32,0u32,0u32,0u32));
        } }
        .into();
        // No _S3 or fake sleep prepare/wake methods. S4/S5 SMI handling
        // does not imply retained-memory or S3 resume support.
        let mut body = Vec::new();
        let mut routes = Vec::new();
        for function in &snapshot.functions {
            if function.address.device() == 0x16 && !config.me.heci1_visible_to_os {
                continue;
            }
            if let (Some(pin), Some(gsi)) = (function.pin, function.gsi) {
                let route = (function.address.device(), pin.index(), gsi);
                if routes
                    .iter()
                    .any(|r: &(u8, u8, u8)| r.0 == route.0 && r.1 == route.1 && r.2 != route.2)
                {
                    panic!("inconsistent slot/pin routing");
                }
                if !routes.contains(&route) {
                    routes.push(route);
                }
            }
            if let Some(port) = function.port {
                // Native pci_irqs.asl lists every root-port slot with all four
                // pins, independent of the port's own (possibly zero) pin.
                for p in 0..4 {
                    let route = (function.address.device(), p, 16 + p);
                    if !routes.contains(&route) {
                        routes.push(route);
                    }
                }
                let adr = (u32::from(function.address.device()) << 16)
                    | u32::from(function.address.function());
                let mut rp: Vec<u8> = acpi_dsl! {Name("_ADR",#{dword adr});}.into();
                if let Some(pin) = function.pin {
                    let pin = pin.index();
                    rp.extend_from_slice(&fstart_acpi::pirq::prt_modes_aml(
                        (0..4).map(|p| (0, p, 16 + (pin + p) % 4)),
                        pci_root,
                    ));
                }
                body.extend_from_slice(
                    &fstart_acpi::aml_linker::device_vec(&alloc::format!("RP{:02}", port), &rp)
                        .expect("physical RP AML"),
                );
            }
        }
        body.extend_from_slice(&fstart_acpi::pirq::prt_modes_aml(
            routes.into_iter(),
            pci_root,
        ));
        // The xHCI companion node (native xhci.asl). Without one, xhci-pci's
        // Intel _DSM probe runs on a null handle and fails loudly; with it,
        // the absent _DSM is the quiet AE_NOT_FOUND. No wake (_PRW) claimed.
        if snapshot
            .functions
            .iter()
            .any(|f| (f.address.device(), f.address.function()) == (0x14, 0))
        {
            body.extend_from_slice(&acpi_dsl! {Device("XHCI") { Name("_ADR",0x00140000u32); }});
        }
        body.extend_from_slice(&crate::southbridge::acpi::pci_irq_links(
            &config.legacy.interrupts.pic_routes,
        ));
        body.extend_from_slice(&pci_osc());
        // gpio.asl's H controller (COM0/1/3, not GPD COM2). Observe every
        // actual IRQ-select word before P2SB masks/hide; no guessed GPE AML.
        let check_gpio_pcr = {
            #[cfg(not(target_os = "none"))]
            {
                !self.host_baseline
            }
            #[cfg(target_os = "none")]
            {
                true
            }
        };
        if check_gpio_pcr {
            for port in [pcr::Port::Gpio0, pcr::Port::Gpio1, pcr::Port::Gpio3] {
                let misc = self
                    .pcr
                    .register::<super::super::gpio::MiscConfig::Register>(port, 0x10)
                    .expect("GPIO IRQ register")
                    .extract();
                assert_ne!(misc.get(), u32::MAX);
                assert_eq!(
                    misc.is_set(super::super::gpio::MiscConfig::IRQ15),
                    config.legacy.interrupts.gpio_irq == 15
                );
            }
        }
        let gpio_irq = u32::from(config.legacy.interrupts.gpio_irq);
        let gpio: Vec<u8> = acpi_dsl! {Device("GPIO") {
            Name("_HID","INT344B");Name("_UID",1u32);Name("_DDN","GPIO Controller");
            Name("_CRS",ResourceTemplate {
                Memory32Fixed(ReadWrite,0xfdaf0000u32,0x10000u32);
                Memory32Fixed(ReadWrite,0xfdae0000u32,0x10000u32);
                Memory32Fixed(ReadWrite,0xfdac0000u32,0x10000u32);
                Interrupt(ResourceConsumer,Level,ActiveLow,Shared,#{dword gpio_irq});
            });
        }}
        .into();
        body.extend_from_slice(&gpio);
        // PCH-specific decode geometry, not the ICH helper's GPIO/PM/TCO map.
        let mut lpc:Vec<u8>=acpi_dsl! {
            Name("_ADR",0x001f0000u32);
            Device("HPET") {
                Name("_HID",EisaId("PNP0103"));
                Name("_CID",0x010CD041u32);
                Name("_CRS",ResourceTemplate {Memory32Fixed(ReadWrite,0xfed00000u32,0x400u32);});
                Method("_STA",0,NotSerialized) { Return(0x0Fu32); }
            }
            Device("RTC_") {Name("_HID",EisaId("PNP0B00"));Name("_CRS",ResourceTemplate {IO(0x70u16,0x70u16,1u8,8u8);IRQ(Edge,ActiveHigh,Exclusive,8u32);});}
            Device("LDRC") {Name("_HID",EisaId("PNP0C02"));Name("_UID",2u32);Name("_CRS",ResourceTemplate {
                IO(0xb2u16,0xb2u16,1u8,2u8);
                IO(0x1800u16,0x1800u16,1u8,0xffu8);
                IO(0x18ffu16,0x18ffu16,1u8,1u8);
                IO(0x400u16,0x400u16,1u8,0x20u8);
                Memory32Fixed(ReadWrite,0xfc800000u32,0x200000u32);
                // Leave the OS GPIO controller's three community windows
                // free of a competing PNP0C02 consumer reservation.
                Memory32Fixed(ReadWrite,0xfd000000u32,0xac0000u32);
                Memory32Fixed(ReadWrite,0xfdad0000u32,0x10000u32);
                Memory32Fixed(ReadWrite,0xfdb00000u32,0x500000u32);
                Memory32Fixed(ReadWrite,0xfe000000u32,0x10000u32);
                Memory32Fixed(ReadWrite,0xfec00000u32,0x1000u32);
            });}
        }.into();
        let pit_gated = {
            #[cfg(not(target_os = "none"))]
            if self.host_baseline {
                false
            } else {
                self.interconnect_outcome
                    .expect("actual PIT gating")
                    .legacy_8254_clock_gated
            }
            #[cfg(target_os = "none")]
            {
                self.interconnect_outcome
                    .expect("actual PIT gating")
                    .legacy_8254_clock_gated
            }
        };
        if config.legacy.pcat && !pit_gated {
            lpc.extend_from_slice(&acpi_dsl! {Device("TIMR") {Name("_HID",EisaId("PNP0100"));Name("_CRS",ResourceTemplate {IO(0x40u16,0x40u16,1u8,4u8);IO(0x50u16,0x50u16,1u8,4u8);IRQ(Edge,ActiveHigh,Exclusive,0u32);});}});
        }
        // These BARs are root producers, but hidden functions cannot claim
        // them as OS PCI consumers. Keep their live apertures reserved rather
        // than letting OS hotplug allocation reuse firmware-owned decodes.
        if !config.spi_visible_to_os {
            lpc.extend_from_slice(&acpi_dsl! {Device("SPIR") {
                Name("_HID",EisaId("PNP0C02"));Name("_UID",4u32);
                Name("_CRS",ResourceTemplate {Memory32Fixed(ReadWrite,0xfe010000u32,0x1000u32);});
            }});
        }
        if !config.me.heci1_visible_to_os {
            let heci_base = config.heci1_base;
            let reserve:Vec<u8>=acpi_dsl! {Device("MER_") {
                Name("_HID",EisaId("PNP0C02"));Name("_UID",5u32);
                Name("_CRS",ResourceTemplate {Memory32Fixed(ReadWrite,#{dword heci_base},0x1000u32);});
            }}.into();
            lpc.extend_from_slice(&reserve);
        }
        body.extend_from_slice(
            &fstart_acpi::aml_linker::device_vec(Self::ACPI_LPC_NAME, &lpc).expect("native LPC"),
        );
        bytes.extend_from_slice(
            &fstart_acpi::aml_linker::scope_vec(pci_root.as_str(), &body)
                .expect("native PCH scope"),
        );
        bytes
    }
    /// Native common/block/acpi/lpit.c: package C10 and SLP_S0 residency.
    fn extra_tables(&self, config: &Config) -> Vec<Vec<u8>> {
        use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
        self.acpi_ready();
        assert!(core::ptr::eq(config, self.config));
        // MWAIT(6,0) hint as FFixedHW: width = vendor 1 (Intel), offset =
        // class 2 (native MWAIT C-state).
        let mwait_c10 = GAS::new(
            AddressSpace::FunctionalFixedHardware,
            1,
            2,
            AccessSize::Undefined,
            0x60,
        );
        // Break-even 30 ms, worst-case exit 3 ms (native, from FSP dumps).
        let state = |residency_counter, counter_frequency| fstart_acpi::lpit::NativeCState {
            entry_trigger: mwait_c10,
            min_residency_us: 30_000,
            max_latency_us: 3000,
            residency_counter,
            counter_frequency,
        };
        const MSR_PKG_C10_RESIDENCY: u64 = 0x632;
        const SLP_S0_RES: u64 = 0x13c;
        alloc::vec![fstart_acpi::lpit::build_lpit(&[
            state(
                GAS::new(
                    AddressSpace::FunctionalFixedHardware,
                    64,
                    0,
                    AccessSize::Undefined,
                    MSR_PKG_C10_RESIDENCY,
                ),
                0, // TSC
            ),
            // The PCH counts SLP_S0 residency in 100-us units.
            state(
                GAS::new(
                    AddressSpace::SystemMemory,
                    32,
                    0,
                    AccessSize::DwordAccess,
                    pmc::PWRM_BASE as u64 + SLP_S0_RES,
                ),
                10_000,
            ),
        ])]
    }
}
fn pci_osc() -> Vec<u8> {
    // Pure negotiation: no hidden AML hardware transitions. Do not grant
    // SHPC/DPC/CPPC capabilities this port has not implemented.
    acpi_dsl! {Method("_OSC",4,Serialized) {
        If (SizeOf(Arg3)<4u32) {Return(Arg3);}
        CreateDwordField(Arg3,0u32,"CDW1");
        If (Arg0!=ToUUID("33db4d5b-1ff7-401c-9657-7441c03dd766")) {CDW1=CDW1|4u32;Return(Arg3);}
        If (Arg1!=1u32) {CDW1=CDW1|8u32;Return(Arg3);}
        If ((Arg2!=3u32)||(SizeOf(Arg3)!=12u32)) {CDW1=CDW1|2u32;Return(Arg3);}
        CreateDwordField(Arg3,8u32,"CDW3");
        Local0=CDW3;
        CDW3=CDW3&0x3du32;
        If (CDW3!=Local0) {CDW1=CDW1|16u32;}
        Return(Arg3);
    }}
    .into()
}

#[cfg(test)]
mod tests;
