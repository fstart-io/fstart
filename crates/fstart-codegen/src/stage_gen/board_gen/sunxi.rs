//! Allwinner sunxi/eGON board adapter trampolines.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::{Capability, StageLayout};

use crate::stage_gen::tokens::hex_addr;

use super::enabled_indices;
use super::model::{device_provides, BoardCtx};

/// Emit the body of `Board::boot_media_select`.
pub(super) fn boot_media_select_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let uses_boot_media_select = ctx.stage.capabilities.iter().any(|c| {
        matches!(
            c,
            Capability::LoadNextStage { .. } | Capability::BootMedia(_)
        )
    });
    let is_egon = ctx.config.soc_image_format == fstart_types::SocImageFormat::AllwinnerEgon;

    if !uses_boot_media_select || !is_egon {
        return quote! {
            let _ = candidates;
            todo!("board_gen::boot_media_select: stage does not use LoadNextStage/BootMediaAuto, \
                   or board is not sunxi-eGON")
        };
    }

    quote! {
        // SAFETY: _egon_sram_base is the BROM entry point where the
        // eGON header is mapped in SRAM.
        let _bm = unsafe {
            fstart_soc_sunxi::boot_media_at(self._egon_sram_base as usize)
        };
        fstart_log::info!("boot media detect: {:#x}", _bm);
        for candidate in candidates {
            if candidate.media_ids.iter().any(|&id| id == _bm) {
                self._boot_media = fstart_stage_runtime::BootMediaState::Block {
                    device_id: candidate.device,
                    offset: candidate.offset,
                    size: candidate.size,
                };
                return Some(candidate.device);
            }
        }
        fstart_log::error!(
            "boot_media_select: no candidate matched boot media {:#x}",
            _bm,
        );
        None
    }
}

/// Emit the body of `Board::load_next_stage`.
pub(super) fn load_next_stage_body(ctx: &BoardCtx<'_>) -> TokenStream {
    let uses_load_next_stage = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::LoadNextStage { .. }));
    let is_egon = ctx.config.soc_image_format == fstart_types::SocImageFormat::AllwinnerEgon;

    if !uses_load_next_stage || !is_egon {
        return quote! {
            let _ = next_stage;
            todo!("board_gen::load_next_stage: stage does not use LoadNextStage, \
                   or board is not sunxi-eGON")
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
                    #name => (#load_addr, #handoff_addr),
                }
            })
            .collect::<TokenStream>(),
        _ => quote! {},
    };

    let dev_arms = enabled_indices(ctx.devices, ctx.instances, ctx.excluded)
        .filter(|idx| device_provides(ctx, *idx, Service::BlockDevice))
        .map(|idx| {
            let dev = &ctx.devices[idx];
            let field = format_ident!("{}", dev.name.as_str());
            let id_lit = proc_macro2::Literal::u8_unsuffixed(idx as u8);
            let dev_name = dev.name.as_str();
            quote! {
                #id_lit => {
                    fstart_capabilities::next_stage::read_stage_to_addr(
                        self.#field
                            .as_ref()
                            .unwrap_or_else(|| fstart_platform::halt()),
                        #dev_name,
                        next_stage,
                        dev_offset,
                        load_addr,
                        ns_size,
                    )
                    .unwrap_or_else(|_| {
                        fstart_log::error!(
                            "FATAL: failed to read stage from {}",
                            #dev_name,
                        );
                        fstart_platform::halt();
                    });
                }
            }
        });

    let dram_device = ctx.stage.capabilities.iter().find_map(|cap| match cap {
        Capability::DramInit { device } => Some(device.as_str()),
        _ => None,
    });
    let dram_size_expr = match dram_device {
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
    };

    quote! {
        let (load_addr, handoff_addr): (u64, u64) = match next_stage {
            #stage_arms
            other => {
                fstart_log::error!(
                    "load_next_stage: unknown next-stage name '{}'",
                    other,
                );
                fstart_platform::halt();
            }
        };

        let ns_ffs_offset =
            unsafe { fstart_soc_sunxi::next_stage_offset_at(self._egon_sram_base as usize) } as u64;
        let ns_size =
            unsafe { fstart_soc_sunxi::next_stage_size_at(self._egon_sram_base as usize) } as usize;
        if ns_ffs_offset == 0 || ns_size == 0 {
            fstart_log::error!("FATAL: eGON header has zero next_stage_offset/size");
            fstart_platform::halt();
        }

        match self._boot_media {
            fstart_stage_runtime::BootMediaState::Block { device_id, offset, .. } => {
                let dev_offset = offset + ns_ffs_offset;
                match device_id {
                    #(#dev_arms)*
                    _ => {
                        fstart_log::error!(
                            "load_next_stage: unknown block device id {}",
                            device_id,
                        );
                        fstart_platform::halt();
                    }
                }
            }
            fstart_stage_runtime::BootMediaState::Mmio { .. } => {
                fstart_log::error!(
                    "load_next_stage: boot medium is a memory-mapped region, \
                     not a block device",
                );
                fstart_platform::halt();
            }
            fstart_stage_runtime::BootMediaState::None => {
                fstart_log::error!(
                    "load_next_stage: no boot medium selected",
                );
                fstart_platform::halt();
            }
        }

        let dram_size: u64 = #dram_size_expr;
        fstart_capabilities::next_stage::serialize_handoff(dram_size, handoff_addr)
            .unwrap_or_else(|_| {
                fstart_log::error!("FATAL: handoff serialize failed");
                fstart_platform::halt();
            });
        fstart_log::info!(
            "jumping to stage '{}' at {:#x}",
            next_stage,
            load_addr,
        );
        fstart_platform::jump_to_with_handoff(load_addr, handoff_addr as usize)
    }
}
