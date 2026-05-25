//! ACPI/SMBIOS and firmware table capability trampolines.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use super::model::BoardEmitModel;
use fstart_device_registry::Service;
use fstart_types::Capability;

/// Emit the body of `Board::acpi_load`.
pub(super) fn acpi_load_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let arms = ctx
        .runtime_devices
        .providers(Service::AcpiTableProvider)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            let dev_name = device.name;
            quote! {
                #id_lit => {
                    #[repr(align(16))]
                    struct _AcpiLoadBufStore(core::cell::UnsafeCell<[u8; 256 * 1024]>);
                    // SAFETY: single-threaded firmware init, buffer used exactly once.
                    unsafe impl Sync for _AcpiLoadBufStore {}
                    static _ACPI_LOAD_BUF: _AcpiLoadBufStore =
                        _AcpiLoadBufStore(core::cell::UnsafeCell::new([0u8; 256 * 1024]));
                    let _acpi_buf = unsafe { &mut *_ACPI_LOAD_BUF.0.get() };
                    let rsdp = fstart_capabilities::acpi_load(
                        self.#field
                            .as_ref()
                            .unwrap_or_else(|| fstart_platform::halt()),
                        _acpi_buf,
                        #dev_name,
                    )
                    .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                    self._acpi_rsdp_addr = rsdp;
                    Ok(())
                }
            }
        });
    quote! {
        match id {
            #(#arms)*
            _ => {
                fstart_log::error!("acpi_load: unknown device id {}", id);
                fstart_platform::halt();
            }
        }
    }
}

/// Emit the body of `Board::memory_detect`.
pub(super) fn memory_detect_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !ctx
        .stage
        .capabilities
        .iter()
        .any(|cap| matches!(cap, Capability::MemoryDetect { .. }))
    {
        return quote! {
            fstart_log::error!("memory_detect: stage does not declare MemoryDetect");
            fstart_platform::halt();
        };
    }

    let arms = ctx
        .runtime_devices
        .providers(Service::MemoryDetector)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            let dev_name = device.name;
            quote! {
                #id_lit => {
                    let mut _e820_entries =
                        [fstart_services::memory_detect::E820Entry::zeroed(); 128];
                    fstart_capabilities::memory_detect(
                        self.#field
                            .as_ref()
                            .unwrap_or_else(|| fstart_platform::halt()),
                        &mut _e820_entries,
                        #dev_name,
                    )
                    .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                    Ok(())
                }
            }
        });
    quote! {
        match id {
            #(#arms)*
            _ => {
                fstart_log::error!("memory_detect: unknown device id {}", id);
                fstart_platform::halt();
            }
        }
    }
}

/// Emit the body of `Board::acpi_prepare`.
pub(super) fn acpi_prepare_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    use crate::stage_gen::capabilities::acpi as cap_acpi;
    use crate::stage_gen::config_ser;

    if !ctx.stage.uses_acpi_prepare {
        return quote! {
            todo!("board_gen::acpi_prepare: stage does not declare AcpiPrepare")
        };
    }

    let Some(acpi_cfg) = ctx.config.acpi.as_ref() else {
        return quote! {
            todo!("board_gen::acpi_prepare: board has no `acpi` RON config")
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
                self.#field.as_ref().unwrap_or_else(|| fstart_platform::halt()),
                &#cfg_name,
            ));
            extra_tables.extend(fstart_acpi::device::AcpiDevice::extra_tables(
                self.#field.as_ref().unwrap_or_else(|| fstart_platform::halt()),
                &#cfg_name,
            ));
        });
    }

    for (extra_idx, instance) in ctx.acpi_only_devices.iter().enumerate() {
        acpi_only_blocks.extend(cap_acpi::generate_acpi_only_device(instance, extra_idx));
    }

    let platform_block = cap_acpi::generate_platform_acpi(&acpi_cfg.platform);
    let print_hex = acpi_cfg.print_hex;

    quote! {
        #platform_block
        #config_lets
        self._acpi_rsdp_addr = fstart_capabilities::acpi::prepare_with_options(&platform_acpi, #print_hex, |dsdt_aml, extra_tables| {
            #device_blocks
            #acpi_only_blocks
        });
    }
}

/// Emit the body of `Board::smbios_prepare`.
pub(super) fn smbios_prepare_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !ctx.stage.uses_smbios {
        return quote! {
            todo!("board_gen::smbios_prepare: stage does not declare SmBiosPrepare")
        };
    }
    if ctx.config.smbios.is_none() {
        return quote! {
            todo!("board_gen::smbios_prepare: board has no `smbios` RON config")
        };
    }
    crate::stage_gen::capabilities::generate_smbios_prepare(ctx.config)
}
