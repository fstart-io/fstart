//! Device lifecycle trampoline emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::{DriverInstance, Service};
use fstart_types::{DeviceConfig, DeviceNode};

use super::enabled_indices;
use super::model::BoardCtx;

/// Emit the body of `Board::init_device`.
pub(super) fn init_device_body(ctx: &BoardCtx<'_>) -> TokenStream {
    use crate::stage_gen::config_ser::{config_tokens, driver_type_tokens};

    let entries: Vec<(TokenStream, TokenStream)> =
        enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
            .filter(|idx| {
                let inst = &ctx.instances[*idx];
                !inst.is_structural() && !inst.is_acpi_only()
            })
            .map(|idx| {
                let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
                let helper = format_ident!("__fstart_init_device_{}", idx);
                let chain = chain_from_root(idx, ctx.device_tree, ctx.devices, ctx.instances);
                let steps = chain.iter().map(|&step_idx| {
                    let step_dev = &ctx.devices[step_idx];
                    let step_inst = &ctx.instances[step_idx];
                    let step_field = format_ident!("{}", step_dev.name.as_str());
                    let step_id_lit = proc_macro2::Literal::u8_unsuffixed(step_idx as u8);
                    let ty = driver_type_tokens(step_inst);
                    let cfg = config_tokens(step_inst);
                    let cfg_static = format_ident!("__FSTART_CFG_{}", step_idx);
                    let construct = if step_inst.meta().is_bus_device {
                        let parent_name = walk_to_real_parent(
                            step_idx,
                            ctx.device_tree,
                            ctx.devices,
                            ctx.instances,
                        );
                        match parent_name {
                            Some(pname) => {
                                let parent = format_ident!("{}", pname);
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
                                    let mut _dev = <#ty>::new_on_bus(_cfg_ref, _parent_ref)?;
                                    _dev.init()?;
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
                    quote! {
                        if !this._inited.contains(#step_id_lit) {
                            #construct
                            this._inited.set(#step_id_lit);
                        }
                    }
                });
                let helper_def = quote! {
                    #[inline(never)]
                    fn #helper(
                        this: &mut _BoardDevices,
                    ) -> Result<(), fstart_services::device::DeviceError> {
                        if this._inited.contains(#id_lit) {
                            return Ok(());
                        }
                        #(#steps)*
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
                fstart_log::error!("init_device: unknown device id {}", id);
                Err(fstart_services::device::DeviceError::InitFailed)
            }
        }
    }
}

/// Walk a device's ancestor chain root-first, excluding structural / ACPI-only /
/// disabled ancestors.
fn chain_from_root(
    target_idx: usize,
    device_tree: &[DeviceNode],
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
) -> Vec<usize> {
    let mut chain = Vec::new();
    let mut cursor = Some(target_idx);
    while let Some(idx) = cursor {
        let inst = &instances[idx];
        let dev = &devices[idx];
        if dev.enabled && !inst.is_structural() && !inst.is_acpi_only() {
            chain.push(idx);
        }
        cursor = device_tree[idx].parent.map(|p| p as usize);
    }
    chain.reverse();
    chain
}

/// Walk an ancestor chain until the first non-structural parent.
fn walk_to_real_parent<'a>(
    child_idx: usize,
    device_tree: &[DeviceNode],
    devices: &'a [DeviceConfig],
    instances: &[DriverInstance],
) -> Option<&'a str> {
    let mut current = device_tree[child_idx].parent?;
    loop {
        let idx = usize::from(current);
        if !instances[idx].is_structural() {
            return Some(devices[idx].name.as_str());
        }
        current = device_tree[idx].parent?;
    }
}

/// Emit the body of `Board::init_all_devices`.
pub(super) fn init_all_devices_body(ctx: &BoardCtx<'_>) -> TokenStream {
    use crate::stage_gen::capabilities::boot_media_values_for_device;

    let is_egon = ctx.config.soc_image_format == fstart_types::SocImageFormat::AllwinnerEgon;
    let mut dev_statements = TokenStream::new();
    let mut has_any_gated = false;

    for idx in enabled_indices(ctx.devices, ctx.instances, ctx.excluded) {
        let inst = &ctx.instances[idx];
        if inst.is_structural() || inst.is_acpi_only() {
            continue;
        }
        let dev = &ctx.devices[idx];
        if ctx.instances[idx].provides(Service::PciRootBus) {
            continue;
        }
        let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
        let is_framebuffer = ctx.instances[idx].provides(Service::Framebuffer);
        let on_err = if is_framebuffer {
            quote! {
                fstart_log::warn!("driver init failed (framebuffer, continuing)");
            }
        } else {
            quote! {
                fstart_log::error!("FATAL: driver init failed for id {}", #id_lit);
                fstart_platform::halt();
            }
        };

        let bm_values = if is_egon {
            let dev_name = dev.name.as_str();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                boot_media_values_for_device(dev_name, ctx.devices, ctx.instances)
            }))
            .unwrap_or_default()
        } else {
            Vec::new()
        };

        let init_call = quote! {
            match self.init_device(#id_lit) {
                Ok(()) => {}
                Err(_) => {
                    #on_err
                }
            }
        };

        let gated_check = if !bm_values.is_empty() && is_egon {
            has_any_gated = true;
            let val_lits = bm_values
                .iter()
                .map(|v| proc_macro2::Literal::u8_unsuffixed(*v))
                .collect::<Vec<_>>();
            quote! {
                if gated.contains(#id_lit) {
                    if matches!(_bm, #(#val_lits)|*) {
                        #init_call
                    } else {
                        fstart_log::info!(
                            "skipping driver init (boot-media gated, not active): id {}",
                            #id_lit,
                        );
                    }
                } else {
                    #init_call
                }
            }
        } else {
            quote! {
                #init_call
            }
        };

        dev_statements.extend(quote! {
            if !skip.contains(#id_lit) {
                #gated_check
            }
        });
    }

    let bm_preamble = if has_any_gated && is_egon {
        quote! {
            // SAFETY: _egon_sram_base is the BROM entry point.
            let _bm = unsafe {
                fstart_soc_sunxi::boot_media_at(self._egon_sram_base as usize)
            };
        }
    } else {
        quote! { let _ = gated; }
    };

    quote! {
        #bm_preamble
        #dev_statements
    }
}
