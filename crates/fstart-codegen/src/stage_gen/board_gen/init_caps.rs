//! Core device-initialization capability trampolines.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;

use super::enabled_indices;
use super::model::{device_provides, BoardCtx};

/// Emit the body of `Board::dram_init`.
pub(super) fn dram_init_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let arms: Vec<TokenStream> = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| device_provides(ctx, *idx, Service::MemoryController))
        .map(|idx| {
            let field = format_ident!("{}", ctx.devices[idx].name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            quote! {
                #id_lit => {
                    use fstart_services::MemoryController as _MemoryController;
                    let dev = self.#field
                        .as_mut()
                        .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                    _MemoryController::dram_init(dev)
                        .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                    Ok(())
                }
            }
        })
        .collect();

    quote! {
        match id {
            #(#arms)*
            _ => {
                fstart_log::error!("dram_init: unknown MemoryController id {}", id);
                Err(fstart_services::device::DeviceError::InitFailed)
            }
        }
    }
}

/// Emit the body of `Board::pci_init`.
pub(super) fn pci_init_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| device_provides(ctx, *idx, Service::PciRootBus))
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let inst = &ctx.instances[idx];
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let dev_name = dev.name.as_str();
            let drv_name = inst.meta().name;
            let field = format_ident!("{}", dev.name.as_str());
            quote! {
                #id_lit => {
                    let dev = self.#field
                        .as_mut()
                        .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                    use fstart_services::PciRootBus as _PciRootBus;
                    _PciRootBus::init_bus(dev)
                        .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                    fstart_log::info!(
                        "PCI init complete: {} ({})",
                        #dev_name,
                        #drv_name,
                    );
                    Ok(())
                }
            }
        });

    quote! {
        match id {
            #(#arms)*
            _ => {
                fstart_log::error!("pci_init: unknown device id {}", id);
                fstart_platform::halt();
            }
        }
    }
}

/// Emit the body of `Board::late_driver_init_complete` before the generic
/// completion banner.
pub(super) fn late_driver_init_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let southbridge_ramstage: Vec<TokenStream> =
        enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
            .filter(|idx| device_provides(ctx, *idx, Service::Southbridge))
            .map(|idx| {
                let field = format_ident!("{}", ctx.devices[idx].name.as_str());
                quote! {
                    if let Some(dev) = self.#field.as_mut() {
                        use fstart_services::Southbridge as _Southbridge;
                        _Southbridge::ramstage_init(dev)
                            .map_err(|_| fstart_services::device::DeviceError::InitFailed)
                            .unwrap_or_else(|_| fstart_platform::halt());
                    }
                }
            })
            .collect();

    let southbridge_field = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .find(|idx| device_provides(ctx, *idx, Service::Southbridge))
        .map(|idx| format_ident!("{}", ctx.devices[idx].name.as_str()));

    let mainboard_ramstage: Vec<TokenStream> = enabled_indices(
        ctx.devices,
        ctx.instances,
        ctx.excluded,
    )
    .filter(|idx| device_provides(ctx, *idx, Service::Mainboard))
    .map(|idx| {
        let field = format_ident!("{}", ctx.devices[idx].name.as_str());
        if let Some(sb_field) = southbridge_field.as_ref() {
            quote! {
                if let (Some(dev), Some(sb)) = (self.#field.as_mut(), self.#sb_field.as_mut()) {
                    use fstart_services::Mainboard as _Mainboard;
                    _Mainboard::ramstage_init_with_southbridge(dev, sb)
                        .map_err(|_| fstart_services::device::DeviceError::InitFailed)
                        .unwrap_or_else(|_| fstart_platform::halt());
                }
            }
        } else {
            quote! {
                if let Some(dev) = self.#field.as_mut() {
                    use fstart_services::Mainboard as _Mainboard;
                    _Mainboard::ramstage_init(dev)
                        .map_err(|_| fstart_services::device::DeviceError::InitFailed)
                        .unwrap_or_else(|_| fstart_platform::halt());
                }
            }
        }
    })
    .collect();

    let southbridge_finalize: Vec<TokenStream> =
        enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
            .filter(|idx| device_provides(ctx, *idx, Service::Southbridge))
            .map(|idx| {
                let field = format_ident!("{}", ctx.devices[idx].name.as_str());
                quote! {
                    if let Some(dev) = self.#field.as_mut() {
                        use fstart_services::Southbridge as _Southbridge;
                        _Southbridge::finalize(dev)
                            .map_err(|_| fstart_services::device::DeviceError::InitFailed)
                            .unwrap_or_else(|_| fstart_platform::halt());
                    }
                }
            })
            .collect();

    let mainboard_finalize: Vec<TokenStream> =
        enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
            .filter(|idx| device_provides(ctx, *idx, Service::Mainboard))
            .map(|idx| {
                let field = format_ident!("{}", ctx.devices[idx].name.as_str());
                quote! {
                    if let Some(dev) = self.#field.as_mut() {
                        use fstart_services::Mainboard as _Mainboard;
                        _Mainboard::finalize(dev)
                            .map_err(|_| fstart_services::device::DeviceError::InitFailed)
                            .unwrap_or_else(|_| fstart_platform::halt());
                    }
                }
            })
            .collect();

    quote! {
        #(#southbridge_ramstage)*
        #(#mainboard_ramstage)*
        #(#southbridge_finalize)*
        #(#mainboard_finalize)*
    }
}
