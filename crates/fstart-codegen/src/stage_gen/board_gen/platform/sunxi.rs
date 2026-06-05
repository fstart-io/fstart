//! Allwinner sunxi/eGON board adapter trampolines.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
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

/// Emit the body of `Board::load_next_stage`.
pub(in crate::stage_gen::board_gen) fn load_next_stage_body(
    ctx: &BoardEmitModel<'_>,
) -> TokenStream {
    let uses_load_next_stage = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::LoadNextStage { .. }));
    let is_egon = ctx.config.soc_image_format == fstart_types::SocImageFormat::AllwinnerEgon;

    if !uses_load_next_stage || !is_egon {
        return quote! {
            let _ = next_stage;
            unreachable!("board_gen::load_next_stage: stage does not use LoadNextStage, \
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

    let dev_arms = ctx
        .runtime_devices
        .providers(Service::BlockDevice)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            let dev_name = device.name;
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
            fstart_soc_sunxi::next_stage_offset_at(self._egon_sram_base as usize) as u64;
        let ns_size = fstart_soc_sunxi::next_stage_size_at(self._egon_sram_base as usize) as usize;
        if ns_ffs_offset == 0 || ns_size == 0 {
            fstart_log::error!("FATAL: eGON header has zero next_stage_offset/size");
            fstart_platform::halt();
        }

        match self._boot_media {
            fstart_stage_runtime::BootMediaState::FirmwareImageBlock { device_id, offset, .. } => {
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
            fstart_stage_runtime::BootMediaState::FirmwareImage { .. } => {
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
