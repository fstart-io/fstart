//! FDT and generic stage-load trampoline emission.

use proc_macro2::TokenStream;
use quote::quote;

use fstart_types::memory::{FlashLayout, RegionKind};
use fstart_types::{Capability, FdtSource, PayloadConfig, Platform};

use crate::stage_gen::tokens::hex_addr;

use super::boot_media::{anchor_bytes_stmt, match_boot_media};
use super::model::BoardEmitModel;

/// Emit the body of `Board::fdt_prepare`.
pub(super) fn fdt_prepare_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    let has_fdt_prepare = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::FdtPrepare));
    if !has_fdt_prepare {
        return quote! {
            todo!("board_gen::fdt_prepare: stage does not declare FdtPrepare")
        };
    }

    let Some(payload) = ctx.config.payload.as_ref() else {
        return quote! { fstart_capabilities::fdt_prepare_stub(); };
    };

    match &payload.fdt {
        FdtSource::Platform => fdt_prepare_platform_body(platform, payload),
        FdtSource::Override(_dtb_file) => fdt_prepare_override_body(ctx),
        _ => quote! { fstart_capabilities::fdt_prepare_stub(); },
    }
}

fn dtb_src_expr(platform: Platform, payload: &PayloadConfig) -> TokenStream {
    if let Some(addr) = payload.src_dtb_addr {
        return hex_addr(addr);
    }
    match platform {
        Platform::Riscv64 | Platform::Aarch64 => quote! { fstart_platform::boot_dtb_addr() },
        Platform::Armv7 | Platform::X86_64 => quote! { 0u64 },
    }
}

fn dram_size_expr() -> TokenStream {
    quote! {
        self._handoff
            .as_ref()
            .filter(|h| h.dram_size > 0)
            .map(|h| h.dram_size)
            .unwrap_or(self._dram_size_static)
    }
}

fn fdt_prepare_platform_body(platform: Platform, payload: &PayloadConfig) -> TokenStream {
    let dtb_src = dtb_src_expr(platform, payload);
    let dram_size = dram_size_expr();
    quote! {
        fstart_capabilities::fdt_prepare_platform(
            #dtb_src,
            self._dtb_dst_addr,
            self._bootargs,
            self._dram_base,
            #dram_size,
        );
    }
}

fn fdt_prepare_override_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !ctx.stage.uses_ffs {
        return quote! {
            todo!("board_gen::fdt_prepare Override variant requires an FFS-using stage")
        };
    }

    let anchor = anchor_bytes_stmt();
    let dram_size = dram_size_expr();
    let bm_usage = quote! {
        if !fstart_capabilities::load_ffs_file_by_type(
            _anchor_bytes,
            &_bm,
            fstart_types::ffs::FileType::Fdt,
        ) {
            fstart_log::error!("FATAL: failed to load DTB from FFS");
            fstart_platform::halt();
        }
    };
    let none_body = quote! {
        fstart_log::warn!("fdt_prepare Override: no boot media configured, skipping FFS load");
    };
    let match_body = match_boot_media(ctx, &bm_usage, "fdt_prepare", &none_body);

    quote! {
        fstart_log::info!("loading DTB from FFS...");
        #anchor
        #match_body
        fstart_log::info!("DTB loaded to {:#x}", self._dtb_dst_addr);
        fstart_capabilities::fdt_prepare_platform(
            self._dtb_dst_addr,
            self._dtb_dst_addr,
            self._bootargs,
            self._dram_base,
            #dram_size,
        );
    }
}

fn x86_postcar_config_tokens(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let ram_ranges: Vec<TokenStream> = ctx
        .config
        .memory
        .regions
        .iter()
        .filter(|r| r.kind == RegionKind::Ram)
        .map(|r| {
            let base = r.base;
            let size = r.size;
            quote! { fstart_platform::car_teardown::PhysicalRange { base: #base, size: #size } }
        })
        .collect();

    let rom_range = match &ctx.config.memory.flash_layout {
        Some(FlashLayout::IntelIfd(layout)) => {
            let base = layout.base;
            let size = layout.size as u64;
            quote! {
                Some(fstart_platform::car_teardown::PhysicalRange { base: #base, size: #size })
            }
        }
        None => match (ctx.config.memory.flash_base, ctx.config.memory.flash_size) {
            (Some(base), Some(size)) => quote! {
                Some(fstart_platform::car_teardown::PhysicalRange { base: #base, size: #size })
            },
            _ => quote! { None },
        },
    };

    quote! {
        static _FSTART_POSTCAR_RAM_RANGES: &[fstart_platform::car_teardown::PhysicalRange] = &[
            #(#ram_ranges),*
        ];
        static _FSTART_POSTCAR_CONFIG: fstart_platform::car_teardown::PostcarConfig =
            fstart_platform::car_teardown::PostcarConfig {
                ram_ranges: _FSTART_POSTCAR_RAM_RANGES,
                rom_range: #rom_range,
            };
    }
}

/// Emit the body of `Board::stage_load`.
pub(super) fn stage_load_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !ctx.stage.uses_ffs {
        return quote! {
            let _ = next_stage;
            todo!("board_gen::stage_load requires an FFS-using stage")
        };
    }

    let anchor = anchor_bytes_stmt();

    if ctx.config.platform == Platform::X86_64 {
        let postcar_config = x86_postcar_config_tokens(ctx);
        return quote! {
            fstart_log::info!("stage_load: generated trampoline enter");
            #anchor
            #postcar_config
            fstart_log::info!("stage_load: anchor slice ready");
            match self._boot_media {
                fstart_stage_runtime::BootMediaState::Mmio { base, size } => {
                    fstart_log::info!("stage_load: switching to post-CAR DRAM stack for MMIO load");
                    // SAFETY: DRAM has just been trained before StageLoad,
                    // `_FSTART_POSTCAR_CONFIG` describes a DRAM stack and
                    // temporary MTRRs, and this call never returns to CAR.
                    unsafe {
                        fstart_platform::car_teardown::stage_load_mmio(
                            &_FSTART_POSTCAR_CONFIG,
                            next_stage,
                            _anchor_bytes,
                            base,
                            size,
                        );
                    }
                }
                fstart_stage_runtime::BootMediaState::None => {
                    fstart_log::error!("stage_load: no boot media configured");
                }
                fstart_stage_runtime::BootMediaState::Block { device_id, .. } => {
                    fstart_log::error!("stage_load: x86 post-CAR StageLoad supports MMIO only, device {}", device_id);
                }
            }
            fstart_platform::halt()
        };
    }

    let bm_usage = quote! {
        fstart_capabilities::stage_load(
            next_stage,
            _anchor_bytes,
            &_bm,
            fstart_platform::jump_to,
        );
    };
    let none_body = quote! {
        fstart_log::error!("stage_load: no boot media configured");
    };
    let match_body = match_boot_media(ctx, &bm_usage, "stage_load", &none_body);

    quote! {
        fstart_log::info!("stage_load: generated trampoline enter");
        #anchor
        fstart_log::info!("stage_load: anchor slice ready");
        #match_body
        fstart_log::error!(
            "stage_load: capability returned without jumping — halting",
        );
        fstart_platform::halt()
    }
}

/// Emit the body of `Board::return_to_fel`.
pub(super) fn return_to_fel_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    let uses_return_to_fel = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::ReturnToFel));

    if platform != Platform::Armv7 || !uses_return_to_fel {
        return quote! {
            todo!("board_gen::return_to_fel: stage does not declare ReturnToFel")
        };
    }
    quote! {
        fstart_log::info!("returning to FEL mode...");
        // SAFETY: `save_boot_params` ran during platform entry and
        // populated the FEL stash before capability dispatch.
        unsafe { fstart_soc_sunxi::return_to_fel_from_stash() }
    }
}
