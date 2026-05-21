//! ACPI/SMBIOS and firmware table capability trampolines.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::Capability;

use super::enabled_indices;
use super::model::{device_provides, BoardCtx};

/// Emit the body of `Board::acpi_load`.
pub(super) fn acpi_load_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| device_provides(ctx, *idx, Service::AcpiTableProvider))
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let field = format_ident!("{}", dev.name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let dev_name = dev.name.as_str();
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
pub(super) fn memory_detect_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| device_provides(ctx, *idx, Service::MemoryDetector))
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let field = format_ident!("{}", dev.name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let dev_name = dev.name.as_str();
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
pub(super) fn acpi_prepare_body(ctx: &BoardCtx<'_>) -> TokenStream {
    use crate::stage_gen::capabilities::acpi as cap_acpi;
    use crate::stage_gen::config_ser;

    let has_acpi_prepare = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::AcpiPrepare));
    if !has_acpi_prepare {
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

    for (idx, dev) in ctx.devices.iter().enumerate() {
        let inst = &ctx.instances[idx];
        let meta = inst.meta();
        if meta.has_acpi && inst.acpi_name().is_some() && !inst.is_acpi_only() {
            let field = format_ident!("{}", dev.name.as_str());
            let cfg_name = format_ident!("{}_cfg", dev.name.as_str());
            let cfg_literal = config_ser::config_tokens(inst);
            let drv_ty = config_ser::driver_type_tokens(inst);
            let cfg_ty = if dev.parent.is_some() {
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
    }

    let mut extra_idx = 0usize;
    for (idx, _dev) in ctx.devices.iter().enumerate() {
        let inst = &ctx.instances[idx];
        if !inst.is_acpi_only() {
            continue;
        }
        acpi_only_blocks.extend(cap_acpi::generate_acpi_only_device(inst, extra_idx));
        extra_idx += 1;
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
pub(super) fn smbios_prepare_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let has_smbios_prepare = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::SmBiosPrepare));
    if !has_smbios_prepare {
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
