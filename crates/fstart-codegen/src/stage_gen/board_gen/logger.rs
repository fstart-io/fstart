//! Logger installation trampoline emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;

use super::enabled_indices;
use super::model::{device_provides, BoardCtx};

/// Emit the body of `Board::install_logger`.
///
/// Emits a `match id { ... }` where every arm corresponds to an enabled device
/// providing the `Console` service. The executor calls `init_device(id)` before
/// this trampoline, so matching arms can install the already-constructed
/// console as the global logger.
pub(super) fn install_logger_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| device_provides(ctx, *idx, Service::Console))
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let inst = &ctx.instances[idx];
            let field = format_ident!("{}", dev.name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let dev_name = dev.name.as_str();
            let drv_name = inst.meta().name;
            quote! {
                #id_lit => {
                    // SAFETY: the executor's `CapOp::ConsoleInit`
                    // arm calls `init_device(id)` before
                    // `install_logger(id)`, so `self.#field` is
                    // `Some`.  `fstart_log::init` promotes the
                    // borrow to `'static`; we own the device for
                    // the stage's lifetime, so that is sound.
                    unsafe {
                        fstart_log::init(
                            self.#field
                                .as_ref()
                                .unwrap_or_else(|| fstart_platform::halt()),
                        );
                    }
                    fstart_capabilities::console_ready(#dev_name, #drv_name);
                }
            }
        });

    quote! {
        match id {
            #(#arms)*
            _ => {
                // Executor contract violation — `install_logger` is
                // only dispatched for ids declared `ConsoleInit` in
                // `StagePlan`, which `plan_gen` only emits for
                // Console providers.  Reaching this arm means an
                // id we didn't emit a field for, so no recovery is
                // possible.
                fstart_platform::halt();
            }
        }
    }
}
