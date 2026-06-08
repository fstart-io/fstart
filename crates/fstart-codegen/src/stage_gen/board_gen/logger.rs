//! Logger installation primitive emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;

use super::model::BoardEmitModel;

/// Emit the body of `Board::install_logger`.
///
/// Emits a `match id { ... }` where every arm corresponds to an enabled device
/// providing the `Console` service. The runtime executor calls
/// `construct_device(id)` before this primitive, so matching arms can install the
/// already-constructed console as the global logger and return static metadata
/// for runtime-owned readiness logging.
pub(super) fn install_logger_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let arms = ctx
        .runtime_devices
        .providers(Service::Console)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            let dev_name = device.name;
            let drv_name = device.instance.meta().name;
            quote! {
                #id_lit => {
                    // SAFETY: direct `ConsoleInit` codeflow calls
                    // `construct_device(id)` before `install_logger(id)`, so
                    // `self.#field` is `Some`. `fstart_log::init` promotes
                    // the borrow to `'static`; we own the device for the
                    // stage's lifetime, so that is sound.
                    unsafe {
                        fstart_log::init(
                            self.#field
                                .as_ref()
                                .unwrap_or_else(|| fstart_platform::halt()),
                        );
                    }
                    Ok(fstart_stage_runtime::ConsoleReady {
                        device_name: #dev_name,
                        driver_name: #drv_name,
                    })
                }
            }
        });

    quote! {
        match id {
            #(#arms)*
            _ => {
                // Codegen contract violation — `install_logger` is only
                // dispatched for ids declared `ConsoleInit`, and validation
                // requires those ids to provide `Console`. Reaching this arm
                // means an id we didn't emit a field for.
                Err(fstart_stage_runtime::RuntimeError::UnknownDevice)
            }
        }
    }
}
