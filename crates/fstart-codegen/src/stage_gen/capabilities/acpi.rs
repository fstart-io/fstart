//! Code generation for ACPI table preparation.
//!
//! Generates per-device ACPI DSDT AML, extra tables (SPCR, MCFG),
//! ACPI-only device contributions, and platform ACPI assembly.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_device_registry::DriverInstance;
use fstart_types::{BoardConfig, DeviceConfig};

/// Generate code for the AcpiPrepare capability.
///
/// Orchestrates per-device ACPI generation:
/// 1. Collects DSDT AML from each device that has an `AcpiDevice` impl
/// 2. Collects extra tables (SPCR, MCFG) from those devices
/// 3. Collects DSDT AML from ACPI-only extra devices (AHCI, xHCI, PCIe)
/// 4. Calls the platform assembler to build all tables and write to DRAM
pub(in crate::stage_gen) fn generate_acpi_prepare(
    config: &BoardConfig,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
) -> TokenStream {
    let acpi_cfg = config.acpi.as_ref().unwrap_or_else(|| {
        panic!("AcpiPrepare capability requires `acpi` config in board RON");
    });

    let mut device_blocks = TokenStream::new();

    // Per-driver device contributions: iterate devices whose driver
    // has `has_acpi` and whose config contains an `acpi_name` field.
    // ACPI-only devices (Ahci, Xhci, PcieRoot) are handled separately
    // below — they have no runtime driver instance, so the device
    // construction phase skips them and their variables don't exist.
    for (idx, dev) in devices.iter().enumerate() {
        let inst = &instances[idx];
        if inst.is_acpi_only() {
            continue;
        }
        let meta = inst.meta();
        if !meta.has_acpi {
            continue;
        }
        // Check at codegen time whether this device instance has an ACPI
        // name set.  If not, skip it -- the driver has AcpiDevice support
        // but this particular board instance doesn't want ACPI for it.
        if inst.acpi_name().is_none() {
            continue;
        }
        let dev_name = format_ident!("{}", dev.name.as_str());
        let cfg_name = format_ident!("{}_cfg", dev.name.as_str());
        device_blocks.extend(quote! {
            dsdt_aml.extend(fstart_acpi::device::AcpiDevice::dsdt_aml(&#dev_name, &#cfg_name));
            extra_tables.extend(fstart_acpi::device::AcpiDevice::extra_tables(&#dev_name, &#cfg_name));
        });
    }

    // ACPI-only device contributions (devices with no runtime driver).
    let mut extra_idx = 0;
    for (idx, _dev) in devices.iter().enumerate() {
        let inst = &instances[idx];
        if !inst.is_acpi_only() {
            continue;
        }
        let block = generate_acpi_only_device(inst, extra_idx);
        device_blocks.extend(block);
        extra_idx += 1;
    }

    // Platform assembly.
    let platform_block = generate_platform_acpi(&acpi_cfg.platform);

    quote! {
        #platform_block
        fstart_capabilities::acpi::prepare(&platform_acpi, |dsdt_aml, extra_tables| {
            #device_blocks
        });
    }
}

/// Generate code for an ACPI-only device (from the devices[] list).
fn generate_acpi_only_device(
    instance: &fstart_device_registry::DriverInstance,
    idx: usize,
) -> TokenStream {
    let var_name = format_ident!("_acpi_dev_{}", idx);
    match instance {
        fstart_device_registry::DriverInstance::Ahci(dev) => {
            let name = dev.name.as_str();
            let base = Literal::u64_unsuffixed(dev.base);
            let size = Literal::u32_unsuffixed(dev.size);
            let gsiv = Literal::u32_unsuffixed(dev.gsiv);
            quote! {
                let #var_name = fstart_acpi::devices::AhciAcpi {
                    name: #name, base: #base, size: #size, gsiv: #gsiv,
                };
                dsdt_aml.extend(#var_name.dsdt_aml());
            }
        }
        fstart_device_registry::DriverInstance::Xhci(dev) => {
            let name = dev.name.as_str();
            let base = Literal::u64_unsuffixed(dev.base);
            let size = Literal::u32_unsuffixed(dev.size);
            let gsiv = Literal::u32_unsuffixed(dev.gsiv);
            quote! {
                let #var_name = fstart_acpi::devices::XhciAcpi {
                    name: #name, base: #base, size: #size, gsiv: #gsiv,
                };
                dsdt_aml.extend(#var_name.dsdt_aml());
            }
        }
        fstart_device_registry::DriverInstance::PcieRoot(dev) => {
            let name = dev.name.as_str();
            let ecam = Literal::u64_unsuffixed(dev.ecam_base);
            let m32_start = Literal::u32_unsuffixed(dev.mmio32.0);
            let m32_end = Literal::u32_unsuffixed(dev.mmio32.1);
            let m64_start = Literal::u64_unsuffixed(dev.mmio64.0);
            let m64_end = Literal::u64_unsuffixed(dev.mmio64.1);
            let pio = dev
                .pio_base
                .map_or(Literal::u64_unsuffixed(0), Literal::u64_unsuffixed);
            let bus_start = Literal::u8_unsuffixed(dev.bus_range.0);
            let bus_end = Literal::u8_unsuffixed(dev.bus_range.1);
            let irq0 = Literal::u32_unsuffixed(dev.irqs[0]);
            let irq1 = Literal::u32_unsuffixed(dev.irqs[1]);
            let irq2 = Literal::u32_unsuffixed(dev.irqs[2]);
            let irq3 = Literal::u32_unsuffixed(dev.irqs[3]);
            let seg = Literal::u16_unsuffixed(dev.segment);
            quote! {
                let #var_name = fstart_acpi::devices::PcieRootAcpi {
                    name: #name,
                    ecam_base: #ecam,
                    mmio32_base: #m32_start, mmio32_end: #m32_end,
                    mmio64_base: #m64_start, mmio64_end: #m64_end,
                    pio_base: #pio,
                    bus_start: #bus_start, bus_end: #bus_end,
                    irqs: [#irq0, #irq1, #irq2, #irq3],
                    segment: #seg,
                };
                dsdt_aml.extend(#var_name.dsdt_aml());
                extra_tables.extend(#var_name.extra_tables());
            }
        }
        _ => TokenStream::new(),
    }
}

/// Generate the platform ACPI config struct literal.
fn generate_platform_acpi(platform: &fstart_types::acpi::AcpiPlatform) -> TokenStream {
    use fstart_types::acpi::AcpiPlatform;

    match platform {
        AcpiPlatform::Arm(sbsa) => {
            let num_cpus = Literal::u32_unsuffixed(sbsa.num_cpus);
            let gic_dist = Literal::u64_unsuffixed(sbsa.gic_dist_base);
            let gic_redist = Literal::u64_unsuffixed(sbsa.gic_redist_base);
            let t0 = Literal::u32_unsuffixed(sbsa.timer_gsivs.0);
            let t1 = Literal::u32_unsuffixed(sbsa.timer_gsivs.1);
            let t2 = Literal::u32_unsuffixed(sbsa.timer_gsivs.2);
            let t3 = Literal::u32_unsuffixed(sbsa.timer_gsivs.3);

            let gic_redist_length_expr = match sbsa.gic_redist_length {
                Some(len) => {
                    let len_lit = Literal::u32_unsuffixed(len);
                    quote! { Some(#len_lit) }
                }
                None => quote! { None },
            };

            let gic_its_base_expr = match sbsa.gic_its_base {
                Some(addr) => {
                    let addr_lit = Literal::u64_unsuffixed(addr);
                    quote! { Some(#addr_lit) }
                }
                None => quote! { None },
            };

            let watchdog_expr = match &sbsa.watchdog {
                Some(wd) => {
                    let refresh = Literal::u64_unsuffixed(wd.refresh_base);
                    let control = Literal::u64_unsuffixed(wd.control_base);
                    let gsiv = Literal::u32_unsuffixed(wd.gsiv);
                    quote! {
                        Some(fstart_acpi::platform::WatchdogConfig {
                            refresh_base: #refresh,
                            control_base: #control,
                            gsiv: #gsiv,
                        })
                    }
                }
                None => quote! { None },
            };

            let iort_expr = match &sbsa.iort {
                Some(iort) => {
                    let seg = Literal::u32_unsuffixed(iort.pci_segment);
                    let mal = Literal::u8_unsuffixed(iort.memory_address_limit);
                    let idc = Literal::u32_unsuffixed(iort.id_count);
                    let its_ids: Vec<_> = iort
                        .its_ids
                        .iter()
                        .map(|id| Literal::u32_unsuffixed(*id))
                        .collect();
                    quote! {
                        Some(fstart_acpi::platform::IortConfig {
                            its_ids: &[#(#its_ids),*],
                            pci_segment: #seg,
                            memory_address_limit: #mal,
                            id_count: #idc,
                        })
                    }
                }
                None => quote! { None },
            };

            quote! {
                let platform_acpi = fstart_acpi::platform::PlatformConfig::Arm(
                    fstart_acpi::platform::ArmConfig {
                        num_cpus: #num_cpus,
                        gic_dist_base: #gic_dist,
                        gic_redist_base: #gic_redist,
                        gic_redist_length: #gic_redist_length_expr,
                        gic_its_base: #gic_its_base_expr,
                        timer_gsivs: (#t0, #t1, #t2, #t3),
                        watchdog: #watchdog_expr,
                        iort: #iort_expr,
                    }
                );
            }
        }
        AcpiPlatform::X86(x86) => {
            let num_cpus = Literal::u32_unsuffixed(x86.num_cpus);
            let lapic_base = Literal::u64_unsuffixed(x86.lapic_base);
            let sci_irq = Literal::u8_unsuffixed(x86.sci_irq);
            let legacy = x86.legacy_devices;

            let ioapic_entries: Vec<_> = x86
                .ioapics
                .iter()
                .map(|ioapic| {
                    let id = Literal::u8_unsuffixed(ioapic.id);
                    let base = Literal::u64_unsuffixed(ioapic.base);
                    let gsi = Literal::u32_unsuffixed(ioapic.gsi_base);
                    quote! {
                        fstart_acpi::platform::IoApicConfig {
                            id: #id, base: #base, gsi_base: #gsi,
                        }
                    }
                })
                .collect();

            let iso_entries: Vec<_> = x86
                .isos
                .iter()
                .map(|iso| {
                    let bus = Literal::u8_unsuffixed(iso.bus);
                    let source = Literal::u8_unsuffixed(iso.source);
                    let gsi = Literal::u32_unsuffixed(iso.gsi);
                    let flags = Literal::u16_unsuffixed(iso.flags);
                    quote! {
                        fstart_acpi::platform::IsoConfig {
                            bus: #bus, source: #source, gsi: #gsi, flags: #flags,
                        }
                    }
                })
                .collect();

            let hpet_expr = match x86.hpet_base {
                Some(base) => {
                    let base_lit = Literal::u64_unsuffixed(base);
                    quote! { Some(#base_lit) }
                }
                None => quote! { None },
            };

            let num_ioapics = Literal::usize_unsuffixed(x86.ioapics.len());
            let num_isos = Literal::usize_unsuffixed(x86.isos.len());

            quote! {
                static _IOAPICS: [fstart_acpi::platform::IoApicConfig; #num_ioapics] =
                    [#(#ioapic_entries),*];
                static _ISOS: [fstart_acpi::platform::IsoConfig; #num_isos] =
                    [#(#iso_entries),*];
                let platform_acpi = fstart_acpi::platform::PlatformConfig::X86(
                    fstart_acpi::platform::X86Config {
                        num_cpus: #num_cpus,
                        lapic_base: #lapic_base,
                        ioapics: &_IOAPICS,
                        isos: &_ISOS,
                        hpet_base: #hpet_expr,
                        legacy_devices: #legacy,
                        sci_irq: #sci_irq,
                    }
                );
            }
        }
    }
}
