//! Core device-initialization capability trampolines.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;

use super::model::BoardEmitModel;

/// Emit the body of `Board::dram_init`.
pub(super) fn dram_init_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let arms: Vec<TokenStream> = ctx
        .runtime_devices
        .providers(Service::MemoryController)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
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
pub(super) fn pci_init_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let arms = ctx
        .runtime_devices
        .providers(Service::PciRootBus)
        .map(|device| {
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            let dev_name = device.name;
            let drv_name = device.instance.meta().name;
            let field = format_ident!("{}", device.name);
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
