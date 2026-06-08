//! Concrete device construction primitive emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_types::BusAddress;

use super::model::BoardEmitModel;

fn bus_address_tokens(address: Option<BusAddress>) -> TokenStream {
    match address {
        Some(BusAddress::Pci(device, function)) => {
            quote! { Some(fstart_types::BusAddress::Pci(#device, #function)) }
        }
        Some(BusAddress::Lpc(port)) => quote! { Some(fstart_types::BusAddress::Lpc(#port)) },
        Some(BusAddress::I2c(addr)) => quote! { Some(fstart_types::BusAddress::I2c(#addr)) },
        Some(BusAddress::Spi(cs)) => quote! { Some(fstart_types::BusAddress::Spi(#cs)) },
        None => quote! { None },
    }
}

/// Emit the body of `Board::construct_device`.
pub(super) fn construct_device_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    use crate::stage_gen::config_ser::{config_tokens, driver_type_tokens};

    let entries: Vec<(TokenStream, TokenStream)> = ctx
        .runtime_devices
        .runtime_indices()
        .map(|idx| {
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let helper = format_ident!("__fstart_construct_device_{}", idx);
            let step_dev = &ctx.devices[idx];
            let step_inst = &ctx.instances[idx];
            let step_field = format_ident!("{}", step_dev.name.as_str());
            let ty = driver_type_tokens(step_inst);
            let cfg = config_tokens(step_inst);
            let cfg_static = format_ident!("__FSTART_CFG_{}", idx);
            let construct = if step_inst.meta().is_bus_device {
                let parent_name = ctx.runtime_devices.real_parent_name(idx);
                match parent_name {
                    Some(pname) => {
                        let parent = format_ident!("{}", pname);
                        let bus_address = bus_address_tokens(step_dev.bus);
                        quote! {
                            static mut #cfg_static: core::mem::MaybeUninit<<#ty as fstart_services::BusDevice>::Config> = core::mem::MaybeUninit::uninit();
                            // SAFETY: generated stage init is single-threaded and writes this
                            // config before constructing the device. The returned reference is
                            // retained for the firmware lifetime.
                            let _cfg_ref: &'static <#ty as fstart_services::BusDevice>::Config = unsafe {
                                let _cfg_ptr = core::ptr::addr_of_mut!(#cfg_static).cast::<<#ty as fstart_services::BusDevice>::Config>();
                                _cfg_ptr.write(#cfg);
                                &*_cfg_ptr
                            };
                            let _parent_ref = this.#parent
                                .as_ref()
                                .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                            let mut _dev = <#ty>::new_on_bus_at(_cfg_ref, _parent_ref, #bus_address)?;
                            let _parent_mut = this.#parent
                                .as_mut()
                                .ok_or(fstart_services::device::DeviceError::InitFailed)?;
                            _dev.init_on_bus(_parent_mut)?;
                            this.#step_field = Some(_dev);
                        }
                    }
                    None => quote! {
                        static mut #cfg_static: core::mem::MaybeUninit<<#ty as fstart_services::Device>::Config> = core::mem::MaybeUninit::uninit();
                        // SAFETY: generated stage init is single-threaded and writes this
                        // config before constructing the device. The returned reference is
                        // retained for the firmware lifetime.
                        let _cfg_ref: &'static <#ty as fstart_services::Device>::Config = unsafe {
                            let _cfg_ptr = core::ptr::addr_of_mut!(#cfg_static).cast::<<#ty as fstart_services::Device>::Config>();
                            _cfg_ptr.write(#cfg);
                            &*_cfg_ptr
                        };
                        let mut _dev = <#ty>::new(_cfg_ref)?;
                        _dev.init()?;
                        this.#step_field = Some(_dev);
                    },
                }
            } else {
                quote! {
                    static mut #cfg_static: core::mem::MaybeUninit<<#ty as fstart_services::Device>::Config> = core::mem::MaybeUninit::uninit();
                    // SAFETY: generated stage init is single-threaded and writes this
                    // config before constructing the device. The returned reference is
                    // retained for the firmware lifetime.
                    let _cfg_ref: &'static <#ty as fstart_services::Device>::Config = unsafe {
                        let _cfg_ptr = core::ptr::addr_of_mut!(#cfg_static).cast::<<#ty as fstart_services::Device>::Config>();
                        _cfg_ptr.write(#cfg);
                        &*_cfg_ptr
                    };
                    let mut _dev = <#ty>::new(_cfg_ref)?;
                    _dev.init()?;
                    this.#step_field = Some(_dev);
                }
            };
            let helper_def = quote! {
                    #[inline(never)]
                    fn #helper(
                        this: &mut _BoardDevices,
                    ) -> Result<(), fstart_services::device::DeviceError> {
                        if this._inited.contains(#id_lit) {
                            return Ok(());
                        }
                        #construct
                        this._inited.set(#id_lit);
                        Ok(())
                    }
            };
            let arm = quote! { #id_lit => #helper(self), };
            (helper_def, arm)
        })
        .collect();

    let helper_defs = entries.iter().map(|(helper, _)| helper);
    let arms = entries.iter().map(|(_, arm)| arm);

    quote! {
        #(#helper_defs)*
        match id {
            #(#arms)*
            _ => {
                fstart_log::error!("construct_device: unknown device id {}", id);
                Err(fstart_services::device::DeviceError::InitFailed)
            }
        }
    }
}
