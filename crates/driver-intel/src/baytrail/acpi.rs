//! Bay Trail ACPI: the PCI host bridge and fixed resources, the processor
//! objects, MCFG, the south cluster namespace (`_PRT`, LPC, PCIe, USB,
//! SATA, SMBus, ACPI-mode LPSS/SCC/LPE) and the FADT/MADT facts (coreboot
//! `soc/intel/baytrail/acpi/*.asl`, `acpi.c` and `fadt.c`).

extern crate alloc;

use super::devices::{AcpiModeDevice, Function};
use super::north::{BaytrailNorth, NorthConfig};
use super::south::{BaytrailSouth, SouthConfig};
use super::*;
use crate::southbridge::acpi as fragments;
use crate::southbridge::smi::{APM_CNT, APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE};
use alloc::vec::Vec;
use fstart_acpi::acpi_dsl;
use fstart_acpi::aml_linker::{device_vec, scope_vec};
use fstart_acpi::device::AcpiDevice;
use fstart_acpi::platform::x86::{
    HpetConfig, IoApicConfig, IsoConfig, X86Config, X86Fadt, X86PlatformProvider,
};
use fstart_pci::PciRootProvider;

const LAPIC_BASE: u64 = 0xfee0_0000;
const SCI_IRQ: u8 = 9;

impl AcpiDevice for BaytrailNorth {
    type Config = NorthConfig;

    /// `\_SB.PCI0` with the live memory map, the fixed blocks the south
    /// cluster decodes, the graphics device and the processors.
    fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
        // Live bounds on the target; a fixed 2 GiB map in host tests.
        #[cfg(target_os = "none")]
        let (tolm, lpe_present) = (self.tolm(), devices::lpe::present());
        #[cfg(not(target_os = "none"))]
        let (tolm, lpe_present) = (0x8000_0000u32, true);
        let ecam = config.ecam_base as u32;
        let ecam_end = ecam + u32::from(ECAM_BUSES) * 0x10_0000;
        let pmem_max = ecam - 1;
        let lpe = if lpe_present {
            devices::lpe::FIRMWARE_BASE as u32
        } else {
            0
        };
        let lpe_len = if lpe != 0 {
            devices::lpe::FIRMWARE_SIZE as u32
        } else {
            0
        };
        let lpe_max = (lpe + lpe_len).saturating_sub(1).max(lpe);
        let pci0: Vec<u8> = acpi_dsl! {
            Name("_HID", EisaId("PNP0A08"));
            Name("_CID", EisaId("PNP0A03"));
            Name("_BBN", 0u32);
            Name("MCRS", ResourceTemplate {
                WordBusNumber(0x0000u16, 0x00ffu16);
                DWordIO(0x0000u32, 0x0cf7u32);
                IO(0x0cf8u16, 0x0cf8u16, 0x01u8, 0x08u8);
                DWordIO(0x0d00u32, 0xffffu32);
                DWordMemory(Cacheable, ReadWrite, 0x000a0000u32, 0x000bffffu32);
                // Option ROM and BIOS extension shadow.
                DWordMemory(Cacheable, ReadWrite, 0x000c0000u32, 0x000dffffu32);
                DWordMemory(Cacheable, ReadWrite, 0x000e0000u32, 0x000effffu32);
                DWordMemory(Cacheable, ReadWrite, 0x000f0000u32, 0x000fffffu32);
                // LPE firmware window, when LPE is enabled.
                DWordMemory(Cacheable, ReadWrite, #{dword lpe}, #{dword lpe_max});
                // PCI memory: TOLM to ECAM, then above ECAM (the north
                // cluster's allocation windows).
                DWordMemory(Cacheable, ReadWrite, #{dword tolm}, #{dword pmem_max});
                DWordMemory(Cacheable, ReadWrite, #{dword ecam_end}, 0xfdffffffu32);
                // TPM.
                DWordMemory(Cacheable, ReadWrite, 0xfed40000u32, 0xfed44fffu32);
            });
            Method("_CRS", 0, Serialized) {
                Return(MCRS);
            }
            Method("_OSC", 4, NotSerialized) {
                Return(Arg3);
            }
            Device("PDRC") {
                Name("_HID", EisaId("PNP0C02"));
                Name("_UID", 1u32);
                Name("_CRS", ResourceTemplate {
                    Memory32Fixed(ReadWrite, #{dword ecam}, 0x10000000u32);
                    Memory32Fixed(ReadWrite, 0xfeb00000u32, 0x00100000u32);
                    Memory32Fixed(ReadWrite, 0xfed03000u32, 0x400u32);
                    Memory32Fixed(ReadWrite, 0xfed08000u32, 0x400u32);
                    Memory32Fixed(ReadWrite, 0xfed01000u32, 0x400u32);
                    Memory32Fixed(ReadWrite, 0xfef00000u32, 0x00100000u32);
                    Memory32Fixed(ReadWrite, 0xfed05000u32, 0x800u32);
                    Memory32Fixed(ReadWrite, 0xfed1c000u32, 0x400u32);
                    Memory32Fixed(ReadWrite, 0xfed0c000u32, 0x4000u32);
                });
            }
            Device("GFX0") {
                Name("_ADR", 0x00020000u32);
            }
        }
        .into();
        let mut aml = device_vec("PCI0", &pci0).expect("Bay Trail PCI0 emission");
        aml.extend(Vec::<u8>::from(acpi_dsl! {
            Name("TOLM", #{dword tolm});
        }));
        #[cfg(target_os = "none")]
        {
            let facts = crate::cpu::silvermont_aml::CpuFacts::read();
            let cpus = usize::from(fstart_arch::x86::mp::online_cpus().max(1));
            aml.extend(crate::cpu::silvermont_aml::cpu_devices_aml(cpus, &facts));
        }
        aml
    }

    fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
        alloc::vec![fstart_acpi::build_mcfg(&[self.root_info()])]
    }
}

/// An ACPI-mode function's device node: name, `_HID`, `_UID`, its
/// interrupts and both BARs. PCI no longer sees the hidden function, so
/// `_CRS` is what keeps the OS off BAR1 (the PCI config shadow) too.
fn acpi_mode_node(device: &AcpiModeDevice) -> Option<Vec<u8>> {
    let (bar0, bar1) = (device.bar0, device.bar1);
    let (name, body): (&str, Vec<u8>) = match device.function {
        Function::SioDma1 | Function::SioDma2 => {
            let (uid, int) = if device.function == Function::SioDma1 {
                (1u32, 42u32)
            } else {
                (2, 43)
            };
            let name = if uid == 1 { "SDM1" } else { "SDM2" };
            (
                name,
                acpi_dsl! {
                    Name("_HID", "INTL9C60");
                    Name("_UID", #{dword uid});
                    Name("_CRS", ResourceTemplate {
                        Memory32Fixed(ReadWrite, #{dword bar0}, 0x1000u32);
                        Memory32Fixed(ReadWrite, #{dword bar1}, 0x1000u32);
                        Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, #{dword int});
                    });
                }
                .into(),
            )
        }
        Function::I2c(n) => {
            const NAMES: [&str; 7] = ["I2C1", "I2C2", "I2C3", "I2C4", "I2C5", "I2C6", "I2C7"];
            let uid = u32::from(n);
            let int = 31 + uid;
            (
                NAMES[usize::from(n - 1)],
                acpi_dsl! {
                    Name("_HID", "80860F41");
                    Name("_UID", #{dword uid});
                    Name("SSCN", Package(0x200u32, 0x200u32, 0x6u32));
                    Name("FMCN", Package(0x55u32, 0x99u32, 0x6u32));
                    Name("_CRS", ResourceTemplate {
                        Memory32Fixed(ReadWrite, #{dword bar0}, 0x1000u32);
                        Memory32Fixed(ReadWrite, #{dword bar1}, 0x1000u32);
                        Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, #{dword int});
                    });
                }
                .into(),
            )
        }
        Function::Spi => (
            "SPI1",
            acpi_dsl! {
                Name("_HID", "80860F0E");
                Name("_UID", 1u32);
                Name("_CRS", ResourceTemplate {
                    Memory32Fixed(ReadWrite, #{dword bar0}, 0x1000u32);
                    Memory32Fixed(ReadWrite, #{dword bar1}, 0x1000u32);
                    Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, 41u32);
                });
            }
            .into(),
        ),
        Function::Pwm1 | Function::Pwm2 => {
            let uid = if device.function == Function::Pwm1 {
                1u32
            } else {
                2
            };
            (
                if uid == 1 { "PWM1" } else { "PWM2" },
                acpi_dsl! {
                    Name("_HID", "80860F09");
                    Name("_UID", #{dword uid});
                    Name("_CRS", ResourceTemplate {
                        Memory32Fixed(ReadWrite, #{dword bar0}, 0x1000u32);
                        Memory32Fixed(ReadWrite, #{dword bar1}, 0x1000u32);
                    });
                }
                .into(),
            )
        }
        Function::Hsuart1 | Function::Hsuart2 => {
            let (uid, int) = if device.function == Function::Hsuart1 {
                (1u32, 39u32)
            } else {
                (2, 40)
            };
            (
                if uid == 1 { "UAR1" } else { "UAR2" },
                acpi_dsl! {
                    Name("_HID", "80860F0A");
                    Name("_UID", #{dword uid});
                    Name("_CRS", ResourceTemplate {
                        Memory32Fixed(ReadWrite, #{dword bar0}, 0x1000u32);
                        Memory32Fixed(ReadWrite, #{dword bar1}, 0x1000u32);
                        Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, #{dword int});
                    });
                }
                .into(),
            )
        }
        Function::Mmc45 | Function::Mmc => (
            "EMMC",
            acpi_dsl! {
                Name("_HID", "80860F14");
                Name("_CID", "PNP0D40");
                Name("_UID", 1u32);
                Name("_CRS", ResourceTemplate {
                    Memory32Fixed(ReadWrite, #{dword bar0}, 0x1000u32);
                    Memory32Fixed(ReadWrite, #{dword bar1}, 0x1000u32);
                    Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, 44u32);
                });
            }
            .into(),
        ),
        Function::Sdio => (
            "SDIO",
            acpi_dsl! {
                Name("_HID", "INT33BB");
                Name("_CID", "PNP0D40");
                Name("_UID", 2u32);
                Name("_CRS", ResourceTemplate {
                    Memory32Fixed(ReadWrite, #{dword bar0}, 0x1000u32);
                    Memory32Fixed(ReadWrite, #{dword bar1}, 0x1000u32);
                    Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, 46u32);
                });
            }
            .into(),
        ),
        Function::Sd => (
            "SDCD",
            acpi_dsl! {
                Name("_HID", "80860F16");
                Name("_CID", "PNP0D40");
                Name("_UID", 3u32);
                Name("_CRS", ResourceTemplate {
                    Memory32Fixed(ReadWrite, #{dword bar0}, 0x1000u32);
                    Memory32Fixed(ReadWrite, #{dword bar1}, 0x1000u32);
                    Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, 47u32);
                });
            }
            .into(),
        ),
        Function::Lpe => {
            let fw = device.extra;
            (
                "LPEA",
                acpi_dsl! {
                    Name("_HID", "80860F28");
                    Name("_CID", "80860F28");
                    Name("_UID", 1u32);
                    Name("_CRS", ResourceTemplate {
                        Memory32Fixed(ReadWrite, #{dword bar0}, 0x200000u32);
                        Memory32Fixed(ReadWrite, #{dword bar1}, 0x1000u32);
                        Memory32Fixed(ReadWrite, #{dword fw}, 0x100000u32);
                        Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, 24u32);
                        Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, 25u32);
                        Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, 26u32);
                        Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, 27u32);
                        Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, 28u32);
                        Interrupt(ResourceConsumer, Level, ActiveLow, Exclusive, 29u32);
                    });
                }
                .into(),
            )
        }
        _ => return None,
    };
    device_vec(name, &body).ok()
}

/// The three GPIO communities (coreboot `acpi/gpio.asl`): Linux binds the
/// Bay Trail pinctrl driver to them.
fn gpio_nodes() -> Vec<u8> {
    let mut aml = Vec::new();
    for (name, uid, offset, gsi) in [
        ("GPSC", 1u32, 0x0000u32, 49u32),
        ("GPNC", 2, 0x1000, 48),
        ("GPSS", 3, 0x2000, 50),
    ] {
        let base = IO_BASE + offset;
        let body: Vec<u8> = acpi_dsl! {
            Name("_HID", "INT33FC");
            Name("_CID", "INT33FC");
            Name("_UID", #{dword uid});
            Name("_HRV", 6u32);
            Name("_CRS", ResourceTemplate {
                Memory32Fixed(ReadWrite, #{dword base}, 0x1000u32);
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword gsi});
            });
        }
        .into();
        aml.extend(device_vec(name, &body).expect("Bay Trail GPIO device emission"));
    }
    aml
}

impl AcpiDevice for BaytrailSouth {
    type Config = SouthConfig;

    fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
        let mut body = fstart_acpi::pirq::prt_name_aml(
            config
                .pirq
                .root_bus_routes()
                .map(|(slot, pin, gsi)| (slot, pin.index(), gsi)),
        );
        // LPC: legacy devices and the PIRQ links. The links report the PIC
        // routes; an unrouted PIRQ reads as disabled (bit 7 on ICH).
        let mut lpcb: Vec<u8> = acpi_dsl! { Name("_ADR", 0x001f0000u32); }.into();
        lpcb.extend(fragments::legacy_isa_children(ACPI_BASE, GPIO_BASE));
        let links = config
            .pic_routes
            .map(|irq| if irq == 0 { 0x80 } else { irq });
        lpcb.extend(fragments::pci_irq_links(&links));
        body.extend(device_vec("LPCB", &lpcb).expect("Bay Trail LPCB emission"));
        body.extend(fragments::sata_node(0x0013_0000));
        body.extend(fragments::smbus_node());
        body.extend(Vec::<u8>::from(acpi_dsl! {
            Device("XHCI") {
                Name("_ADR", 0x00140000u32);
                Name("_PRW", Package(0x0du32, 3u32));
                Name("_S3D", 3u32);
            }
        }));
        body.extend(fragments::ehci_node("EHC1", 0x001d_0000, 0x0d, 4));
        for port in 1..=4u8 {
            let adr = 0x001c_0000u32 + u32::from(port - 1);
            let name = alloc::format!("RP0{port}");
            body.extend(fragments::pcie_root_port(&name, adr, port, false));
        }
        for device in self.acpi_devices() {
            if let Some(node) = acpi_mode_node(device) {
                body.extend(node);
            }
        }
        let mut aml = scope_vec("\\_SB_.PCI0", &body).expect("Bay Trail PCI0 scope emission");
        aml.extend(scope_vec("\\_SB_", &gpio_nodes()).expect("Bay Trail GPIO scope emission"));
        aml
    }
}

impl X86PlatformProvider for BaytrailSouth {
    fn x86_platform_config(&self, online_cpus: u32) -> X86Config {
        let apic_ids = (0..online_cpus as u16)
            .map(|cpu| fstart_arch::x86::mp::apic_id(cpu).unwrap_or(u32::from(cpu)))
            .collect();
        X86Config {
            apic_ids,
            lapic_base: LAPIC_BASE,
            lapic_nmi_lint: None,
            ioapics: alloc::vec![IoApicConfig {
                id: IOAPIC_ID,
                base: u64::from(IOAPIC_BASE),
                gsi_base: 0,
            }],
            isos: alloc::vec![
                // ISA IRQ0 on GSI2, edge, active high.
                IsoConfig {
                    bus: 0,
                    source: 0,
                    gsi: 2,
                    flags: 0x0005,
                },
                // SCI on IRQ9, level, active high.
                IsoConfig {
                    bus: 0,
                    source: SCI_IRQ,
                    gsi: u32::from(SCI_IRQ),
                    flags: 0x000d,
                },
            ],
            hpet: Some(HpetConfig {
                base: u64::from(HPET_BASE),
                timer_block_id: 0x8086_a201,
                min_tick: 0,
            }),
            legacy_devices: true,
            sci_irq: SCI_IRQ,
            pmbase: ACPI_BASE,
            gpe0_offset: pmc::GPE0_STS,
            gpe0_length: 16,
            acpi_smi: Some(fstart_core::acpi::AcpiSmiConfig {
                smi_cmd: u32::from(APM_CNT),
                acpi_enable: APM_CNT_ACPI_ENABLE,
                acpi_disable: APM_CNT_ACPI_DISABLE,
            }),
            fadt: X86Fadt::default(),
        }
    }
}
