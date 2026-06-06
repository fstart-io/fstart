//! Allwinner sunxi/eGON board adapter primitives.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_types::{Capability, StageLayout};

use crate::stage_gen::tokens::hex_addr;

use super::super::model::BoardEmitModel;

/// Emit the body of `Board::soc_boot_media`.
pub(in crate::stage_gen::board_gen) fn soc_boot_media_body(
    ctx: &BoardEmitModel<'_>,
) -> TokenStream {
    let uses_soc_boot_media = ctx.stage.capabilities.iter().any(|c| {
        matches!(
            c,
            Capability::LoadNextStage { .. } | Capability::BootMedia(_)
        )
    });
    let is_egon = ctx.config.soc_image_format == fstart_types::SocImageFormat::AllwinnerEgon;

    if !uses_soc_boot_media || !is_egon {
        return quote! { None };
    }

    quote! {
        Some(fstart_soc_sunxi::boot_media_at(self._egon_sram_base as usize))
    }
}

fn uses_load_next_stage(ctx: &BoardEmitModel<'_>) -> bool {
    ctx.stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::LoadNextStage { .. }))
        && ctx.config.soc_image_format == fstart_types::SocImageFormat::AllwinnerEgon
}

/// Emit primitive `Board::next_stage_addr` body.
pub(in crate::stage_gen::board_gen) fn next_stage_addr_body(
    ctx: &BoardEmitModel<'_>,
) -> TokenStream {
    if !uses_load_next_stage(ctx) {
        return quote! {
            let _ = next_stage;
            None
        };
    }

    let stage_arms = match &ctx.config.stages {
        StageLayout::MultiStage(stages) => stages
            .iter()
            .enumerate()
            .map(|(index, s)| {
                let name = s.name.as_str();
                let effective_load_addr =
                    fstart_types::effective_stage_load_addr(ctx.config, index, s);
                let load_addr = hex_addr(effective_load_addr);
                let handoff_addr = hex_addr(effective_load_addr.saturating_sub(0x1000));
                quote! {
                    #name => Some(fstart_stage_runtime::NextStageAddr {
                        load_addr: #load_addr,
                        handoff_addr: #handoff_addr,
                    }),
                }
            })
            .collect::<TokenStream>(),
        _ => quote! {},
    };

    quote! {
        match next_stage {
            #stage_arms
            _ => None,
        }
    }
}

/// Emit primitive `Board::egon_next_stage` body.
pub(in crate::stage_gen::board_gen) fn egon_next_stage_body(
    ctx: &BoardEmitModel<'_>,
) -> TokenStream {
    if !uses_load_next_stage(ctx) {
        return quote! { None };
    }
    quote! {
        Some(fstart_stage_runtime::EgonNextStage {
            offset: fstart_soc_sunxi::next_stage_offset_at(self._egon_sram_base as usize) as u64,
            size: fstart_soc_sunxi::next_stage_size_at(self._egon_sram_base as usize) as usize,
        })
    }
}

/// Emit primitive `Board::dram_size_for_handoff` body.
pub(in crate::stage_gen::board_gen) fn dram_size_for_handoff_body(
    ctx: &BoardEmitModel<'_>,
) -> TokenStream {
    let dram_device = ctx.stage.capabilities.iter().find_map(|cap| match cap {
        Capability::DramInit { device } => Some(device.as_str()),
        _ => None,
    });
    match dram_device {
        Some(dev_name) => {
            let dev = format_ident!("{}", dev_name);
            quote! {
                self._handoff
                    .as_ref()
                    .filter(|h| h.dram_size > 0)
                    .map(|h| h.dram_size)
                    .unwrap_or_else(|| {
                        self.#dev
                            .as_ref()
                            .map(|d| d.detected_size_bytes())
                            .unwrap_or(self._dram_size_static)
                    })
            }
        }
        None => quote! {
            self._handoff
                .as_ref()
                .filter(|h| h.dram_size > 0)
                .map(|h| h.dram_size)
                .unwrap_or(self._dram_size_static)
        },
    }
}
