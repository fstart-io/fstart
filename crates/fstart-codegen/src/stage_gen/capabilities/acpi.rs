//! Code generation for ACPI table preparation.
//!
//! Emits the per-device DSDT AML + extra-table blocks that
//! `board_gen::acpi_prepare_body` assembles into the board's
//! `fstart_capabilities::acpi::prepare` invocation.  The capability
//! orchestration itself lives in `board_gen`; this module only holds
//! the per-variant struct emission.

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
#[allow(dead_code)]
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
    let platform_block = generate_platform_acpi(&acpi_cfg.platform, devices, instances);

    quote! {
        #platform_block
        fstart_capabilities::acpi::prepare(&platform_acpi, |dsdt_aml, extra_tables| {
            #device_blocks
        });
    }
}

fn x86_fadt_pm_tokens(pm: &fstart_types::acpi::X86FadtPmRegisters) -> TokenStream {
    let pm1a_evt_blk = Literal::u32_unsuffixed(pm.pm1a_evt_blk);
    let pm1a_cnt_blk = Literal::u32_unsuffixed(pm.pm1a_cnt_blk);
    let pm_tmr_blk = Literal::u32_unsuffixed(pm.pm_tmr_blk);
    let pm1_evt_len = Literal::u8_unsuffixed(pm.pm1_evt_len);
    let pm1_cnt_len = Literal::u8_unsuffixed(pm.pm1_cnt_len);
    let pm_tmr_len = Literal::u8_unsuffixed(pm.pm_tmr_len);
    let gpe0_blk = Literal::u32_unsuffixed(pm.gpe0_blk);
    let gpe0_blk_len = Literal::u8_unsuffixed(pm.gpe0_blk_len);
    quote! {
        fstart_types::acpi::X86FadtPmRegisters {
            pm1a_evt_blk: #pm1a_evt_blk,
            pm1a_cnt_blk: #pm1a_cnt_blk,
            pm_tmr_blk: #pm_tmr_blk,
            pm1_evt_len: #pm1_evt_len,
            pm1_cnt_len: #pm1_cnt_len,
            pm_tmr_len: #pm_tmr_len,
            gpe0_blk: #gpe0_blk,
            gpe0_blk_len: #gpe0_blk_len,
        }
    }
}

fn x86_fadt_pm_from_southbridge(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
) -> Option<fstart_types::acpi::X86FadtPmRegisters> {
    devices
        .iter()
        .zip(instances.iter())
        .find(|(dev, _)| dev.services.iter().any(|s| s.as_str() == "Southbridge"))
        .and_then(|(_, inst)| inst.x86_fadt_pm_registers())
}

fn pm_profile_tokens(profile: fstart_types::acpi::AcpiPmProfile) -> TokenStream {
    match profile {
        fstart_types::acpi::AcpiPmProfile::Unspecified => {
            quote! { fstart_types::acpi::AcpiPmProfile::Unspecified }
        }
        fstart_types::acpi::AcpiPmProfile::Desktop => {
            quote! { fstart_types::acpi::AcpiPmProfile::Desktop }
        }
        fstart_types::acpi::AcpiPmProfile::Mobile => {
            quote! { fstart_types::acpi::AcpiPmProfile::Mobile }
        }
        fstart_types::acpi::AcpiPmProfile::Workstation => {
            quote! { fstart_types::acpi::AcpiPmProfile::Workstation }
        }
        fstart_types::acpi::AcpiPmProfile::EnterpriseServer => {
            quote! { fstart_types::acpi::AcpiPmProfile::EnterpriseServer }
        }
        fstart_types::acpi::AcpiPmProfile::SohoServer => {
            quote! { fstart_types::acpi::AcpiPmProfile::SohoServer }
        }
        fstart_types::acpi::AcpiPmProfile::AppliancePc => {
            quote! { fstart_types::acpi::AcpiPmProfile::AppliancePc }
        }
        fstart_types::acpi::AcpiPmProfile::PerformanceServer => {
            quote! { fstart_types::acpi::AcpiPmProfile::PerformanceServer }
        }
        fstart_types::acpi::AcpiPmProfile::Tablet => {
            quote! { fstart_types::acpi::AcpiPmProfile::Tablet }
        }
    }
}

/// Generate code for an ACPI-only device (from the devices[] list).
///
/// Exposed at `pub(in crate::stage_gen)` so [`board_gen::acpi_prepare_body`]
/// can reuse the per-variant struct literal emission without
/// duplicating the `DriverInstance` match.
///
/// [`board_gen::acpi_prepare_body`]: crate::stage_gen::board_gen
pub(in crate::stage_gen) fn generate_acpi_only_device(
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
///
/// Emits `let platform_acpi = ...;` — a stateless binding usable in
/// either the old `fstart_main` body or the new
/// [`board_gen::acpi_prepare_body`] method body.
///
/// Exposed at `pub(in crate::stage_gen)` so `board_gen` can reuse it.
///
/// [`board_gen::acpi_prepare_body`]: crate::stage_gen::board_gen
pub(in crate::stage_gen) fn generate_platform_acpi(
    platform: &fstart_types::acpi::AcpiPlatform,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
) -> TokenStream {
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
            // num_cpus = None → 0 sentinel; runtime MADT builder detects via CPUID.
            let num_cpus = Literal::u32_unsuffixed(x86.num_cpus.unwrap_or(0));
            let lapic_base = Literal::u64_unsuffixed(x86.lapic_base);
            let sci_irq = Literal::u8_unsuffixed(x86.sci_irq);
            let legacy = x86.legacy_devices;
            let hw_reduced = x86.hw_reduced;
            let low_power_s0 = x86.low_power_s0;
            let pm_profile = pm_profile_tokens(x86.pm_profile);
            let iapc_boot_arch = Literal::u16_unsuffixed(x86.iapc_boot_arch);
            let fadt_pm = x86
                .fadt_pm
                .or_else(|| x86_fadt_pm_from_southbridge(devices, instances))
                .unwrap_or_else(|| {
                    panic!(
                        "x86 ACPI requires FADT PM register layout from a southbridge driver or `fadt_pm` override"
                    )
                });
            let fadt_pm_expr = x86_fadt_pm_tokens(&fadt_pm);
            let acpi_smi_expr = match x86.acpi_smi {
                Some(smi) => {
                    let smi_cmd = Literal::u32_unsuffixed(smi.smi_cmd);
                    let acpi_enable = Literal::u8_unsuffixed(smi.acpi_enable);
                    let acpi_disable = Literal::u8_unsuffixed(smi.acpi_disable);
                    quote! {
                        Some(fstart_types::acpi::AcpiSmiConfig {
                            smi_cmd: #smi_cmd,
                            acpi_enable: #acpi_enable,
                            acpi_disable: #acpi_disable,
                        })
                    }
                }
                None => quote! { None },
            };

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

            let hpet_expr = match x86.hpet {
                Some(hpet) => {
                    let base = Literal::u64_unsuffixed(hpet.base);
                    let timer_block_id = Literal::u32_unsuffixed(hpet.timer_block_id);
                    let number = Literal::u8_unsuffixed(hpet.number);
                    let min_tick = Literal::u16_unsuffixed(hpet.min_tick);
                    let page_protection = Literal::u8_unsuffixed(hpet.page_protection);
                    quote! {
                        Some(fstart_acpi::platform::HpetConfig {
                            base: #base,
                            timer_block_id: #timer_block_id,
                            number: #number,
                            min_tick: #min_tick,
                            page_protection: #page_protection,
                        })
                    }
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
                        num_cpus: if #num_cpus == 0 { fstart_mp::online_cpus() as u32 } else { #num_cpus },
                        lapic_base: #lapic_base,
                        ioapics: &_IOAPICS,
                        isos: &_ISOS,
                        hpet: #hpet_expr,
                        legacy_devices: #legacy,
                        hw_reduced: #hw_reduced,
                        low_power_s0: #low_power_s0,
                        sci_irq: #sci_irq,
                        pm_profile: #pm_profile,
                        iapc_boot_arch: #iapc_boot_arch,
                        fadt_pm: #fadt_pm_expr,
                        acpi_smi: #acpi_smi_expr,
                    }
                );
            }
        }
    }
}
