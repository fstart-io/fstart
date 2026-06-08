//! ACPI/SMBIOS and firmware table capability trampolines.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use super::model::BoardEmitModel;
use fstart_device_registry::Service;

/// Emit the body of primitive `Board::with_acpi_table_provider`.
pub(super) fn with_acpi_table_provider_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let arms = ctx
        .runtime_devices
        .providers(Service::AcpiTableProvider)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            let dev_name = device.name;
            quote! {
                #id_lit => {
                    Ok(run(
                        self.#field
                            .as_ref()
                            .ok_or(fstart_stage_runtime::RuntimeError::UnknownDevice)?,
                        #dev_name,
                    ))
                }
            }
        });
    quote! {
        match id {
            #(#arms)*
            _ => Err(fstart_stage_runtime::RuntimeError::UnknownDevice),
        }
    }
}

/// Emit the body of primitive `Board::with_memory_detector`.
pub(super) fn with_memory_detector_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let arms = ctx
        .runtime_devices
        .providers(Service::MemoryDetector)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            let dev_name = device.name;
            quote! {
                #id_lit => {
                    Ok(run(
                        self.#field
                            .as_ref()
                            .ok_or(fstart_stage_runtime::RuntimeError::UnknownDevice)?,
                        #dev_name,
                    ))
                }
            }
        });
    quote! {
        match id {
            #(#arms)*
            _ => Err(fstart_stage_runtime::RuntimeError::UnknownDevice),
        }
    }
}

/// Emit the body of primitive `Board::acpi_platform_config`.
pub(super) fn acpi_platform_config_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    use crate::stage_gen::capabilities::acpi as cap_acpi;
    use fstart_types::acpi::AcpiPlatform;

    if !ctx.stage.uses_acpi_prepare {
        return quote! { None };
    }

    let Some(acpi_cfg) = ctx.config.acpi.as_ref() else {
        return quote! { None };
    };

    let print_hex = acpi_cfg.print_hex;

    match &acpi_cfg.platform {
        AcpiPlatform::Arm(_) => {
            let platform_block = cap_acpi::generate_platform_acpi(&acpi_cfg.platform);
            quote! {
                let _ = x86_online_cpus;
                #platform_block
                Some((platform_acpi, #print_hex))
            }
        }
        AcpiPlatform::X86 => {
            let mut providers = ctx
                .runtime_devices
                .providers(Service::X86AcpiPlatformProvider);
            let Some(provider) = providers.next() else {
                return quote! { None };
            };
            let field = format_ident!("{}", provider.name);
            quote! {
                let provider = self.#field.as_ref()?;
                let platform_acpi = fstart_acpi::platform::PlatformConfig::X86(
                    fstart_acpi::platform::X86PlatformProvider::x86_platform_config(
                        provider,
                        x86_online_cpus?,
                    )
                );
                Some((platform_acpi, #print_hex))
            }
        }
    }
}

/// Emit the body of primitive `Board::collect_acpi_tables`.
pub(super) fn collect_acpi_tables_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    use crate::stage_gen::capabilities::acpi as cap_acpi;
    use crate::stage_gen::config_ser;

    if !ctx.stage.uses_acpi_prepare || ctx.config.acpi.is_none() {
        return quote! {
            let _ = (dsdt_aml, extra_tables);
            Err(fstart_stage_runtime::RuntimeError::UnknownDevice)
        };
    };

    let mut config_lets = TokenStream::new();
    let mut device_blocks = TokenStream::new();
    let mut acpi_only_blocks = TokenStream::new();

    for device in ctx.runtime_devices.acpi_runtime_devices() {
        let inst = device.instance;
        let field = format_ident!("{}", device.name);
        let cfg_name = format_ident!("{}_cfg", device.name);
        let cfg_literal = config_ser::config_tokens(inst);
        let drv_ty = config_ser::driver_type_tokens(inst);
        let cfg_ty = if device.config.parent.is_some() {
            quote! { <#drv_ty as fstart_services::device::BusDevice>::Config }
        } else {
            quote! { <#drv_ty as fstart_services::device::Device>::Config }
        };
        config_lets.extend(quote! {
            let #cfg_name: #cfg_ty = #cfg_literal;
        });
        device_blocks.extend(quote! {
            dsdt_aml.extend(fstart_acpi::device::AcpiDevice::dsdt_aml(
                self.#field
                    .as_ref()
                    .ok_or(fstart_stage_runtime::RuntimeError::UnknownDevice)?,
                &#cfg_name,
            ));
            extra_tables.extend(fstart_acpi::device::AcpiDevice::extra_tables(
                self.#field
                    .as_ref()
                    .ok_or(fstart_stage_runtime::RuntimeError::UnknownDevice)?,
                &#cfg_name,
            ));
        });
    }

    for (extra_idx, instance) in ctx.acpi_only_devices.iter().enumerate() {
        acpi_only_blocks.extend(cap_acpi::generate_acpi_only_device(instance, extra_idx));
    }

    quote! {
        #config_lets
        #device_blocks
        #acpi_only_blocks
        Ok(())
    }
}

/// Emit the body of primitive `Board::smbios_desc`.
pub(super) fn smbios_desc_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !ctx.stage.uses_smbios {
        return quote! { None };
    }
    if ctx.config.smbios.is_none() {
        return quote! { None };
    }
    let desc = crate::stage_gen::capabilities::generate_smbios_desc(ctx.config);
    quote! { Some(#desc) }
}
