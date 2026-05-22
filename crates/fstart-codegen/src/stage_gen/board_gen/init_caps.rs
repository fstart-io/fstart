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
                    _MemoryController::dram_init_with_boot_path(
                        dev,
                        fstart_services::resume::boot_path(),
                    )
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

/// Emit the body of `Board::resume_detect`.
pub(super) fn resume_detect_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let arms: Vec<TokenStream> = ctx
        .runtime_devices
        .providers(Service::ResumeDetector)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            quote! {
                #id_lit => {
                    use fstart_services::ResumeDetector as _ResumeDetector;
                    let dev = self.#field
                        .as_mut()
                        .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                    let path = _ResumeDetector::detect_boot_path(dev)
                        .map_err(|_| fstart_services::device::DeviceError::InitFailed)?;
                    fstart_services::resume::set_boot_path(path);
                    match path {
                        fstart_types::BootPath::Normal => fstart_log::info!("resume_detect: normal boot"),
                        fstart_types::BootPath::S3Resume => fstart_log::info!("resume_detect: ACPI S3 resume"),
                    }
                    Ok(())
                }
            }
        })
        .collect();

    quote! {
        match id {
            #(#arms)*
            _ => {
                fstart_log::error!("resume_detect: unknown ResumeDetector id {}", id);
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

/// Emit the body of `Board::late_driver_init_complete` before the generic
/// completion banner.
pub(super) fn late_driver_init_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let southbridge_ramstage: Vec<TokenStream> = ctx
        .runtime_devices
        .providers(Service::Southbridge)
        .map(|device| {
            let field = format_ident!("{}", device.name);
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

    let southbridge_field = ctx
        .runtime_devices
        .providers(Service::Southbridge)
        .next()
        .map(|device| format_ident!("{}", device.name));

    let mainboard_ramstage: Vec<TokenStream> = ctx
        .runtime_devices
        .providers(Service::Mainboard)
        .map(|device| {
            let field = format_ident!("{}", device.name);
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

    let southbridge_finalize: Vec<TokenStream> = ctx
        .runtime_devices
        .providers(Service::Southbridge)
        .map(|device| {
            let field = format_ident!("{}", device.name);
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

    let mainboard_finalize: Vec<TokenStream> = ctx
        .runtime_devices
        .providers(Service::Mainboard)
        .map(|device| {
            let field = format_ident!("{}", device.name);
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
