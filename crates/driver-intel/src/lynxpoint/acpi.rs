//! Lynx Point ACPI platform configuration and DSDT AML generation.
//!
//! Describes LP's four GPE0 banks and assigned Serial IO BARs, `LDRC` legacy
//! I/O windows (`PMBASE` + LPT-H `GPIOBASE`), `D28IR`-swizzled PCIe root port
//! `_PRT` tables, and full per-controller EHCI RMH port counts.

use super::{IrqRoute, LynxPoint, LynxPointConfig, rcba_window, regs::dev};
use crate::southbridge::smi;
use alloc::{vec, vec::Vec};
use core::sync::atomic::Ordering;
use fstart_acpi::{
    AcpiPath, Aml,
    aml::Name,
    aml_linker::{device_vec, scope_vec},
    platform::{HpetConfig, IoApicConfig, IsoConfig, X86Config, X86Fadt, X86PlatformProvider},
};
use fstart_acpi_macros::acpi_dsl;
use fstart_pci::ecam::EcamDevice;

impl X86PlatformProvider for LynxPoint {
    fn x86_platform_config(&self, online: u32) -> X86Config {
        let lp = self.config.gpio.lp();
        X86Config {
            apic_ids: (0..online).collect(),
            lapic_base: 0xfee0_0000,
            lapic_nmi_lint: Some(1),
            ioapics: vec![IoApicConfig {
                id: 2,
                base: 0xfec0_0000,
                gsi_base: 0,
            }],
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
                    flags: 13,
                },
            ],
            hpet: Some(HpetConfig {
                base: 0xfed0_0000,
                timer_block_id: self.hpet_id,
                min_tick: 0x80,
            }),
            legacy_devices: true,
            sci_irq: 9,
            pmbase: self.config.pm_base(),
            gpe0_offset: if lp { 0x80 } else { 0x20 },
            gpe0_length: if lp { 32 } else { 16 },
            acpi_smi: self.smm_installed.load(Ordering::Acquire).then_some(
                fstart_core::acpi::AcpiSmiConfig {
                    smi_cmd: u32::from(smi::APM_CNT),
                    acpi_enable: smi::APM_CNT_ACPI_ENABLE,
                    acpi_disable: smi::APM_CNT_ACPI_DISABLE,
                },
            ),
            fadt: X86Fadt {
                pm_profile: if lp {
                    fstart_acpi::fadt::PmProfile::Mobile
                } else {
                    fstart_acpi::fadt::PmProfile::Unspecified
                },
                flags: {
                    use fstart_acpi::fadt::Flags;
                    let mut flags = Flags::Wbinvd as u32
                        | Flags::ProcC1 as u32
                        | Flags::SlpButton as u32
                        | Flags::RtcS4 as u32
                        | Flags::UsePlatformClock as u32;
                    if self.config.sealed_case {
                        flags |= Flags::SealedCase as u32;
                    }
                    flags
                },
                pm2_cnt_offset: Some(0x50),
                pm1_evt_word_access: true,
                gpe0_byte_access: true,
                ..X86Fadt::default()
            },
        }
    }
}

/// Compute `[INTA, INTB, INTC, INTD]` APIC GSIs for physical PCIe root port
/// `port` (`1..=8`) from the programmed `D28IR` (`RCBA+0x3146`) nibble map.
///
/// Physical port `port` rotates downstream pin `p` (`0..4`) to Device 28 pin
/// `((port - 1) + p) % 4`, which `D28IR` maps to PIRQ `0..7` (GSI `16 + PIRQ`).
pub(super) fn root_port_gsis(port: u8, d28ir: u16) -> [u32; 4] {
    core::array::from_fn(|pin| {
        let dev28_pin = (usize::from(port - 1) + pin) % 4;
        let pirq = u32::from((d28ir >> (4 * dev28_pin)) & 0x7);
        16 + pirq
    })
}

fn serial_node(i: usize, b0: u32, b1: u32, dma: bool) -> Vec<u8> {
    use fstart_acpi::{
        aml::{Interrupt, Memory32Fixed, ResourceTemplate},
        descriptors::dma::{FixedDma, TransferWidth},
    };
    let (name, hid, irq) = [
        ("SDMA", "INTL9C60", 6),
        ("I2C0", "INT33C2", 7),
        ("I2C1", "INT33C3", 7),
        ("SPI0", "INT33C0", 7),
        ("SPI1", "INT33C1", 7),
        ("UAR0", "INT33C4", 13),
        ("UAR1", "INT33C5", 13),
        ("SDIO", "INT33C6", 5),
    ][i];
    let mut body: Vec<u8> = acpi_dsl! { Name("_UID", 1u32); }.into();
    if i == 7 {
        body.extend_from_slice(&acpi_dsl! { Name("_CID", "PNP0D40"); });
    }
    if (1..7).contains(&i) {
        body.extend(Vec::<u8>::from(acpi_dsl! {
            OperationRegion("PCFG", SystemMemory, #{dword b1}, 0x1000u32);
            Field("PCFG", DWordAcc, NoLock, Preserve) { Offset(0x84), PWST, 2, }
            Method("_PS0", 0, Serialized) { PWST = 0u32; If (PWST != 0u32) {} }
            Method("_PS3", 0, Serialized) { PWST = 3u32; If (PWST != 0u32) {} }
        }));
    }
    let bar0 = Memory32Fixed::new(true, b0, 4096);
    let interrupt = Interrupt::new(true, false, true, true, irq);
    let mut resources: Vec<&dyn Aml> = vec![&bar0, &interrupt];
    let pair = match i {
        1 => Some((0x18, 4)),
        2 => Some((0x1a, 6)),
        4 => Some((0x10, 0)),
        5 => Some((0x16, 2)),
        _ => None,
    }
    .filter(|_| dma)
    .map(|(request, channel)| {
        [0, 1].map(|n| FixedDma {
            request: request + n,
            channel: channel + n,
            width: TransferWidth::Bits32,
        })
    });
    if let Some(pair) = &pair {
        resources.extend(pair.iter().map(|v| v as &dyn Aml));
    }
    Name::new("_CRS".into(), &ResourceTemplate::new(resources)).to_aml_bytes(&mut body);
    Name::new("_HID".into(), &hid).to_aml_bytes(&mut body);
    if i == 1 || i == 2 {
        body.extend_from_slice(&acpi_dsl! {
            Name("SSCN", Package(432u32, 507u32, 30u32));
            Name("FMCN", Package(72u32, 160u32, 30u32));
        });
    }
    device_vec(name, &body).expect("Serial IO device")
}

/// `LPCB.GPIO` owns the LP GPIO range. Keep `GWAK` serialized across the
/// ownership, SCI routing, and pad configuration read-modify-writes.
fn lp_gpio(gpio_base: u16) -> Vec<u8> {
    let base = u32::from(gpio_base);
    let end = base + 0x3ff;
    let conf_base = base + 0x100;
    acpi_dsl! {
        Device("GPIO") {
            Name("_HID", "INT33C7");
            Name("_UID", 1u32);
            Name("_CRS", ResourceTemplate { DWordIO(#{dword base}, #{dword end}); });
            Method("GWAK", 1, Serialized) {
                If (Arg0 < 95u32) {
                    Local0 = #{dword base} + ((Arg0 >> 5u32) << 2u32);
                    Local1 = Local0 + 0x30u32;
                    Local2 = #{dword conf_base} + (Arg0 << 3u32);
                    Local3 = ~(1u32 << (Arg0 & 31u32));
                    OperationRegion("OWNR", SystemIO, Local0, 4u32);
                    Field("OWNR", DWordAcc, NoLock, Preserve) { OWNB, 32, }
                    OperationRegion("ROUT", SystemIO, Local1, 4u32);
                    Field("ROUT", DWordAcc, NoLock, Preserve) { RTGB, 32, }
                    OperationRegion("PADR", SystemIO, Local2, 4u32);
                    Field("PADR", DWordAcc, NoLock, Preserve) { PCFG, 32, }
                    OWNB = OWNB & Local3;
                    RTGB = RTGB & Local3;
                    PCFG = PCFG | 0x0du32;
                }
            }
        }
    }
    .into()
}

fn xhci(lp: bool, gpe: u32) -> Vec<u8> {
    let mut body: Vec<u8> = acpi_dsl! {
        Name("_ADR", 0x0014_0000u32);
        Name("_PRW", Package(#{dword gpe}, 3u32));
        Method("_S3D", 0, NotSerialized) { Return(3u32); }
        Method("_S4D", 0, NotSerialized) { Return(3u32); }
        OperationRegion("XPCI", PciConfig, 0u32, 0x100u32);
        Field("XPCI", AnyAcc, NoLock, Preserve) {
            DVID, 16,
            Offset(0x10), XBAR, 64,
            Offset(0x40), , 11, SWAI, 1,
            Offset(0x44), , 12, SAIP, 2,
            Offset(0x74), DPWR, 2, , 6, PMEE, 1, , 6, PMES, 1,
            Offset(0xb0), , 13, MBIT, 2,
        }
        Method("_PSC", 0, NotSerialized) { Return(DPWR); }
        Method("_PS0", 0, Serialized) { XPOW(0u32); }
        Method("_PS3", 0, Serialized) { XPOW(1u32); }
        Method("XPOW", 1, Serialized) {
            Local0 = XBAR & ~0x0fu32;
            If ((DVID != 0xffffu32) && ((Local0 != 0u32) && ((XBAR & 0xffff_0000u32) != 0xffff_0000u32))) {
                Local1 = Local0 + 0x8000u32;
                OperationRegion("AUXR", SystemMemory, Local1, 0x200u32);
                Field("AUXR", DWordAcc, Lock, Preserve) {
                    Offset(0xe0), , 15, AX15, 1,
                    Offset(0x154), , 31, CLK2, 1,
                    Offset(0x16c), , 2, CLK0, 1, , 11, CLK1, 1,
                }
                If (Arg0) { PMES = 1u32; PMEE = 1u32; }
                DPWR = 0u32;
                If (#{byte u8::from(lp)}) { MBIT = Arg0 * 3u32; CLK0 = Arg0; CLK1 = Arg0; }
                CLK2 = 1u32 - Arg0;
                If (#{byte u8::from(lp)}) {
                    If (Arg0 == 0u32) { XREC(Local0); }
                    AX15 = 1u32 - Arg0;
                }
                SWAI = Arg0;
                SAIP = Arg0;
                If (Arg0) { DPWR = 3u32; }
            }
        }
        Method("XREC", 1, Serialized) {
            Local0 = 0u32;
            Local3 = 0u32;
            While (Local0 < 4u32) {
                Local1 = 10u32;
                While (Local1 > 0u32) {
                    Local2 = 0u32;
                    Local4 = 0u32;
                    While (Local2 < 4u32) {
                        Local5 = XRPT(Arg0 + 0x510u32 + (Local2 << 4u32), Local0);
                        If (Local0 == 1u32) { Local3 = Local3 | (Local5 << Local2); }
                        If ((Local0 == 0u32) || ((Local0 == 2u32) && ((Local3 & (1u32 << Local2)) != 0u32))) {
                            Local4 = Local4 | Local5;
                        }
                        Local2++;
                    }
                    If (Local4 == 0u32) { Break; }
                    Stall(10u32);
                    Local1--;
                }
                Local0++;
            }
        }
        Method("XRPT", 2, Serialized) {
            OperationRegion("XPRT", SystemMemory, Arg0, 4u32);
            Field("XPRT", DWordAcc, Lock, Preserve) { XPSC, 32, }
            If (Arg1 == 0u32) { If ((XPSC & 0x3e0u32) == 0x2e0u32) { Return(1u32); } }
            If (Arg1 == 1u32) {
                If ((XPSC & 0x203e0u32) == 0x2a0u32) {
                    XPSC = (XPSC & ~2u32) | 0x8000_0000u32;
                    Return(1u32);
                }
            }
            If (Arg1 == 2u32) { If ((XPSC & 0x80000u32) == 0u32) { Return(1u32); } }
            If (Arg1 == 3u32) { XPSC = (XPSC & ~2u32) | 0xfe_0000u32; }
            Return(0u32);
        }
    }
    .into();
    let mut hub: Vec<u8> = acpi_dsl! { Name("_ADR", 0u32); }.into();
    for (name, visible) in [("VPLD", 1), ("IPLD", 0)] {
        use fstart_acpi::aml::{BufferData, Package};
        let mut location = vec![0; 20]; // ACPI _PLD revision 2, five dwords
        location[0] = 2;
        location[8] = visible; // UserVisible, bit 64
        let buffer = BufferData::new(location);
        Name::new(name.into(), &Package::new(vec![&buffer])).to_aml_bytes(&mut hub);
    }
    hub.extend_from_slice(&acpi_dsl! {
        Method("GPLD", 1, Serialized) { If (Arg0) { Return(VPLD); } Return(IPLD); }
    });
    for (prefix, count, offset) in [
        ("PRT", if lp { 8 } else { 14 }, 0),
        ("SSP", if lp { 4 } else { 6 }, if lp { 9 } else { 15 }),
    ] {
        for port in 1..=count {
            let adr = (port + offset) as u32;
            let name = alloc::format!("{prefix}{port:X}");
            let port_aml: Vec<u8> = acpi_dsl! { Name("_ADR", #{dword adr}); }.into();
            hub.extend(device_vec(&name, &port_aml).expect("USB port"));
        }
    }
    body.extend(device_vec("HUB7", &hub).expect("root hub"));
    device_vec("XHCI", &body).expect("xHCI")
}

impl crate::namespace::SouthbridgeAcpi for LynxPoint {
    fn dsdt_aml(&self, c: &LynxPointConfig, root: &AcpiPath) -> Vec<u8> {
        let lp = c.gpio.lp();
        let mut body: Vec<u8> =
            acpi_dsl! { Device("LPCB") { Name("_ADR", 0x001f_0000u32); } }.into();
        let wake = if lp { 0x6d } else { 13 };
        // Grant native PCIe Hot Plug, SHPC, PME, AER, Capability Structure, and LTR
        // (PCI Firmware Spec §4.5.1, matching coreboot lynxpoint/acpi/pch.asl).
        body.extend(fstart_acpi::osc::pci_host_osc(0x3f));
        if c.xhci_enabled {
            body.extend(xhci(lp, wake));
        }
        use crate::southbridge::acpi;
        let ldrc = acpi::LegacyLdrc::LynxPoint {
            pm_base: c.pm_base(),
            gpio_base: (!lp).then_some(c.gpio_base()),
        };
        body.extend(scope_vec("LPCB", &acpi::legacy_devices(ldrc)).expect("LPC children"));
        if lp {
            body.extend(scope_vec("LPCB", &lp_gpio(c.gpio_base())).expect("LP GPIO"));
        }
        if c.sata_port_map != 0 {
            body.extend(acpi::sata_node(dev::SATA1));
        }
        if c.smbus_enabled {
            body.extend(acpi::smbus_node(dev::SMBUS));
        }
        if c.hda.is_some() {
            body.extend(acpi::hda_wake_node(dev::HDA, wake, 3));
        }
        let keep_ehci1 = !c.xhci_default || self.ehci_debug.is_some_and(|(d, _)| d == dev::EHCI1);
        let keep_ehci2 =
            !lp && (!c.xhci_default || self.ehci_debug.is_some_and(|(d, _)| d == dev::EHCI2));
        if keep_ehci1 {
            body.extend(acpi::ehci_wake_node(
                "EHC1",
                dev::EHCI1,
                wake,
                3,
                if lp { 6 } else { 8 },
            ));
        }
        if keep_ehci2 {
            body.extend(acpi::ehci_wake_node("EHC2", dev::EHCI2, wake, 3, 6));
        }
        for n in 0..8u8 {
            if let Some(func) = self.root_functions[usize::from(n)] {
                body.extend(acpi::pcie_root_port_with_irqs(
                    &alloc::format!("RP0{}", n + 1),
                    fstart_pci::devfn(dev::PCIE, func),
                    root_port_gsis(n + 1, self.d28ir),
                    false,
                ));
            }
        }
        for (i, bars) in self.serial_bars.iter().enumerate() {
            if let Some((b0, b1)) = bars {
                body.extend(serial_node(i, *b0, *b1, self.serial_bars[0].is_some()));
            }
        }
        if !self.serial_io_populated().is_empty() {
            use fstart_acpi::aml::{Memory32Fixed, ResourceTemplate};
            let ranges: Vec<_> = self
                .serial_bars
                .iter()
                .flatten()
                .map(|&(_, b)| Memory32Fixed::new(true, b, 4096))
                .collect();
            let mut node: Vec<u8> =
                acpi_dsl! { Name("_HID", EisaId("PNP0C02")); Name("_UID", 4u32); }.into();
            Name::new(
                "_CRS".into(),
                &ResourceTemplate::new(ranges.iter().map(|r| r as &dyn Aml).collect()),
            )
            .to_aml_bytes(&mut node);
            body.extend(device_vec("SIOR", &node).expect("Serial IO BAR1 resources"));
        }
        let mut routes = Vec::new();
        for slot in 0..32 {
            for func in 0..8 {
                let d = EcamDevice::new(0, slot, func);
                if d.try_read16(0).is_none_or(|v| v == 0xffff) {
                    continue;
                }
                let pin = d.read8(0x3d);
                if !(1..=4).contains(&pin) {
                    continue;
                }
                let idx = pin - 1;
                if routes.iter().any(|&(s, p, _)| s == slot && p == idx) {
                    continue;
                }
                let router = IrqRoute {
                    device: slot,
                    pirqs: None,
                    pins: None,
                };
                let pirq = router.offsets().map_or(idx, |(_, ir)| {
                    ((rcba_window().read16(ir) >> (4 * idx)) & 7) as u8
                });
                routes.push((slot, idx, 16 + pirq));
            }
        }
        body.extend(fstart_acpi::pirq::prt_name_aml(routes.into_iter()));
        let mut aml: Vec<u8> = acpi_dsl! {
            Scope("\\") {
                Name("_S0_", Package(0u32, 0u32, 0u32, 0u32));
                Name("_S4_", Package(6u32, 4u32, 0u32, 0u32));
                Name("_S5_", Package(7u32, 0u32, 0u32, 0u32));
            }
        }
        .into();
        if c.s1 {
            aml.extend_from_slice(&acpi_dsl! {
                Scope("\\") { Name("_S1_", Package(1u32, 0u32, 0u32, 0u32)); }
            });
        }
        if self.s3 {
            aml.extend_from_slice(&acpi_dsl! {
                Scope("\\") { Name("_S3_", Package(5u32, 0u32, 0u32, 0u32)); }
            });
        }
        aml.extend(scope_vec(root.as_str(), &body).expect("PCH"));
        aml
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntelSouthbridgeDriver;
    use crate::lynxpoint::{Gpios, gpio};
    use crate::namespace::SouthbridgeAcpi;
    use crate::southbridge::gpio_ich::GpioConfig;

    #[test]
    fn platform_selects_word_pm1_but_not_uninstalled_smi() {
        static CONFIG: LynxPointConfig =
            LynxPointConfig::new(Gpios::Lp(&[gpio::Pad::new(gpio::Mode::Unused); 95]));
        let pch = LynxPoint::new_from_config(&CONFIG).unwrap();
        let config = pch.x86_platform_config(2);
        assert_eq!(config.lapic_nmi_lint, Some(1));
        assert!(config.acpi_smi.is_none());
        assert!(config.fadt.pm1_evt_word_access);
        assert!(config.fadt.gpe0_byte_access);
        assert_eq!(
            config.fadt.flags & (fstart_acpi::fadt::Flags::SealedCase as u32),
            0
        );

        static SEALED_PADS: gpio::PadTable = [gpio::Pad::new(gpio::Mode::Unused); 95];
        static SEALED_CFG: LynxPointConfig = LynxPointConfig {
            sealed_case: true,
            ..LynxPointConfig::new(Gpios::Lp(&SEALED_PADS))
        };
        let sealed = LynxPoint::new_from_config(&SEALED_CFG)
            .unwrap()
            .x86_platform_config(2);
        assert_ne!(
            sealed.fadt.flags & (fstart_acpi::fadt::Flags::SealedCase as u32),
            0
        );
    }

    #[test]
    fn i2c_resources_include_both_bars() {
        let a = serial_node(1, 0xd000_1000, 0xd000_2000, true);
        assert!(a.windows(6).any(|b| b == [0x55, 0x18, 0, 4, 0, 2]));
        assert!(
            !serial_node(1, 0xd000_1000, 0xd000_2000, false)
                .windows(6)
                .any(|b| b == [0x55, 0x18, 0, 4, 0, 2])
        );
        for b in [0xd000_1000u32, 0xd000_2000] {
            assert!(a.windows(4).any(|w| w == b.to_le_bytes()));
        }
        assert!(a.windows(7).any(|w| w == b"INT33C2"));
        let sdio = serial_node(7, 0x1000, 0x2000, false);
        assert!(sdio.windows(7).any(|w| w == b"PNP0D40"));
        assert!(!sdio.windows(4).any(|w| w == b"_PS3"));
    }

    #[test]
    fn root_port_gsis_follow_d28ir_swizzle() {
        // Default identity D28IR = 0x3210 (INTA..D -> PIRQA..D = GSI 16..19).
        assert_eq!(root_port_gsis(1, 0x3210), [16, 17, 18, 19]);
        assert_eq!(root_port_gsis(2, 0x3210), [17, 18, 19, 16]);
        assert_eq!(root_port_gsis(5, 0x3210), [16, 17, 18, 19]);
        // Custom D28IR = 0x7654 (INTA..D -> PIRQE..H = GSI 20..23).
        assert_eq!(root_port_gsis(1, 0x7654), [20, 21, 22, 23]);
        assert_eq!(root_port_gsis(2, 0x7654), [21, 22, 23, 20]);
        assert_eq!(root_port_gsis(3, 0x7654), [22, 23, 20, 21]);
        assert_eq!(root_port_gsis(4, 0x7654), [23, 20, 21, 22]);
    }

    #[test]
    fn dsdt_emits_ldrc_ehci_ports_and_swizzled_rp_prt_for_lp_and_h() {
        static LP_PADS: gpio::PadTable = [gpio::Pad::new(gpio::Mode::Unused); 95];
        static H_GPIO: GpioConfig = GpioConfig::new();
        static SWIZZLED_IRQ: [IrqRoute; 1] = [IrqRoute {
            device: 28,
            pins: None,
            pirqs: Some(0x7654),
        }];
        static LP_CFG: LynxPointConfig = LynxPointConfig {
            irq: &SWIZZLED_IRQ,
            xhci_default: false,
            ..LynxPointConfig::new(Gpios::Lp(&LP_PADS))
        };
        static H_CFG: LynxPointConfig = LynxPointConfig {
            irq: &SWIZZLED_IRQ,
            xhci_default: false,
            ..LynxPointConfig::new(Gpios::Traditional(&H_GPIO))
        };

        let root = AcpiPath::SB.child("PCI0");

        let mut lp = LynxPoint::new_from_config(&LP_CFG).unwrap();
        lp.root_functions[0] = Some(0);
        lp.root_functions[1] = Some(1);
        let lp_aml = lp.dsdt_aml(&LP_CFG, &root);
        // LDRC present with PMBASE=0x1000 (len 0xff), and LP exposes GPIO via INT33C7.
        assert!(lp_aml.windows(4).any(|w| w == b"LDRC"));
        assert!(lp_aml.windows(7).any(|w| w == b"INT33C7"));
        // IO(0x1000, 0x1000, 0x01, 0xff) descriptor bytes: 0x47, 0x01, lo, hi, lo, hi, align, len.
        assert!(
            lp_aml
                .windows(8)
                .any(|w| w == [0x47, 0x01, 0x00, 0x10, 0x00, 0x10, 0x01, 0xff])
        );
        // LP EHC1 has 6 RMH ports (PRT1..PRT6), no PRT7, no EHC2.
        assert!(lp_aml.windows(4).any(|w| w == b"EHC1"));
        assert!(!lp_aml.windows(4).any(|w| w == b"EHC2"));
        assert!(lp_aml.windows(4).any(|w| w == b"PRT6"));
        // RP01 and RP02 use swizzled D28IR=0x7654 -> GSIs 20..23.
        assert!(lp_aml.windows(4).any(|w| w == b"RP01"));
        assert!(lp_aml.windows(4).any(|w| w == b"RP02"));
        let expected_rp1 = crate::southbridge::acpi::pcie_root_port_with_irqs(
            "RP01",
            fstart_pci::devfn(dev::PCIE, 0),
            [20, 21, 22, 23],
            false,
        );
        assert!(
            lp_aml
                .windows(expected_rp1.len())
                .any(|w| w == expected_rp1.as_slice())
        );

        let mut h = LynxPoint::new_from_config(&H_CFG).unwrap();
        h.root_functions[0] = Some(0);
        let h_aml = h.dsdt_aml(&H_CFG, &root);
        // H LDRC includes both PMBASE=0x0500 (len 0xff) and GPIOBASE=0x0480 (len 0x40).
        assert!(h_aml.windows(4).any(|w| w == b"LDRC"));
        assert!(!h_aml.windows(7).any(|w| w == b"INT33C7"));
        assert!(
            h_aml
                .windows(8)
                .any(|w| w == [0x47, 0x01, 0x00, 0x05, 0x00, 0x05, 0x01, 0xff])
        );
        assert!(
            h_aml
                .windows(8)
                .any(|w| w == [0x47, 0x01, 0x80, 0x04, 0x80, 0x04, 0x01, 0x40])
        );
        // H EHC1 has 8 ports (PRT8) and EHC2 has 6 ports.
        assert!(h_aml.windows(4).any(|w| w == b"EHC1"));
        assert!(h_aml.windows(4).any(|w| w == b"EHC2"));
        assert!(h_aml.windows(4).any(|w| w == b"PRT8"));
        assert!(
            h_aml
                .windows(expected_rp1.len())
                .any(|w| w == expected_rp1.as_slice())
        );
    }
}
