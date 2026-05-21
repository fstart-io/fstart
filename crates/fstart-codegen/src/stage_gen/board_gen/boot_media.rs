//! Shared boot-media dispatch helpers.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;

use super::enabled_indices;
use super::model::{device_provides, BoardCtx};

/// Emit the volatile FSTART_ANCHOR byte-slice binding used by FFS operations.
pub(super) fn anchor_bytes_stmt() -> TokenStream {
    quote! {
        // SAFETY: FSTART_ANCHOR is emitted by
        // `generate_anchor_static` in this same stage with proper
        // alignment (`#[link_section = ".fstart.anchor"]` + `#[used]`)
        // and is the size of `AnchorBlock`.  The FFS builder may
        // patch its contents post-link, so downstream
        // `FfsReader::read_anchor_volatile` reads it through
        // `ptr::read_volatile`.
        let _anchor_bytes: &[u8] = unsafe {
            core::slice::from_raw_parts(
                &FSTART_ANCHOR as *const fstart_types::ffs::AnchorBlock as *const u8,
                core::mem::size_of::<fstart_types::ffs::AnchorBlock>(),
            )
        };
    }
}

/// Emit a `match self._boot_media { ... }` that binds a local `_bm` for use by
/// `bm_usage`.
pub(super) fn match_boot_media(
    ctx: &BoardCtx<'_>,
    bm_usage: &TokenStream,
    caller_tag: &str,
    none_body: &TokenStream,
) -> TokenStream {
    let block_arms = block_device_arms(ctx, bm_usage);
    let err_msg = format!("{caller_tag}: unknown block device id {{}}");
    quote! {
        match self._boot_media {
            fstart_stage_runtime::BootMediaState::None => {
                #none_body
            }
            fstart_stage_runtime::BootMediaState::Mmio { base, size } => {
                fstart_log::info!("boot-media match: MMIO base={:#x} size={:#x}", base, size);
                #[cfg(feature = "x86_64")]
                fstart_platform::enable_boot_media_rom_cache();
                // SAFETY: `base..base + size` is the board's
                // memory-mapped flash window; the board author's
                // RON declared this range is readable.
                let _bm = unsafe {
                    fstart_services::boot_media::MemoryMapped::from_raw_addr(
                        base,
                        size as usize,
                    )
                };
                fstart_log::info!("boot-media match: media constructed");
                #bm_usage
            }
            fstart_stage_runtime::BootMediaState::Block {
                device_id,
                offset,
                size,
            } => {
                match device_id {
                    #block_arms
                    _ => {
                        fstart_log::error!(#err_msg, device_id);
                        fstart_platform::halt();
                    }
                }
            }
        }
    }
}

/// Emit one match arm per enabled block device in the board.
fn block_device_arms(ctx: &BoardCtx<'_>, bm_usage: &TokenStream) -> TokenStream {
    let arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| device_provides(ctx, *idx, Service::BlockDevice))
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let field = format_ident!("{}", dev.name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            quote! {
                #id_lit => {
                    let _bm = fstart_services::boot_media::BlockDeviceMedia::new(
                        self.#field
                            .as_ref()
                            .unwrap_or_else(|| fstart_platform::halt()),
                        offset,
                        size as usize,
                    );
                    #bm_usage
                }
            }
        });
    quote! { #(#arms)* }
}
