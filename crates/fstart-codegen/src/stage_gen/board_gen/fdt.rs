//! FDT and generic stage-load trampoline emission.

use proc_macro2::TokenStream;
use quote::quote;

use fstart_device_registry::Service;
use fstart_types::memory::{FlashLayout, RegionKind};
use fstart_types::{Capability, FdtSource, PayloadConfig, Platform};

use crate::stage_gen::tokens::hex_addr;

use super::model::BoardEmitModel;

/// Emit the body of primitive `Board::fdt_prepare_desc`.
pub(super) fn fdt_prepare_desc_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    let has_fdt_prepare = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::FdtPrepare));
    if !has_fdt_prepare {
        return quote! { None };
    }

    let dram_size = dram_size_expr();
    let payload = ctx
        .config
        .payload
        .as_ref()
        .expect("FdtPrepare requires a payload with a supported FDT source");

    let source = match &payload.fdt {
        FdtSource::Platform => {
            let dtb_src = dtb_src_expr(platform, payload);
            quote! { fstart_stage_runtime::FdtPrepareSource::Platform { src_dtb_addr: #dtb_src } }
        }
        FdtSource::Override(_dtb_file) => {
            quote! { fstart_stage_runtime::FdtPrepareSource::Override }
        }
        FdtSource::Generated | FdtSource::GeneratedWithOverride(_) => {
            unreachable!("validate_stage_scope_requirements rejects unsupported FDT generation")
        }
    };

    quote! {
        Some(fstart_stage_runtime::FdtPrepareDesc {
            source: #source,
            dst_dtb_addr: self._dtb_dst_addr,
            bootargs: self._bootargs,
            dram_base: self._dram_base,
            dram_size: #dram_size,
        })
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
        None => {
            if let Some((base, size)) = ctx.config.memory.firmware_window() {
                quote! {
                    Some(fstart_platform::car_teardown::PhysicalRange { base: #base, size: #size })
                }
            } else {
                let build_ctx = fstart_device_registry::BuildFirmwareImageContext {
                    flash_layout: ctx.config.memory.flash_layout.as_ref(),
                    intel_ifd: None,
                };
                let image = {
                    let mut images = ctx
                        .runtime_devices
                        .providers(Service::FirmwareImageProvider)
                        .filter_map(|device| {
                            ctx.instances[device.index]
                                .build_firmware_image(&build_ctx)
                                .unwrap_or_else(|err| {
                                    panic!("build firmware image provider failed: {err}")
                                })
                        });
                    let first = images.next();
                    if images.next().is_none() { first } else { None }.or_else(|| {
                        fstart_device_registry::platform_firmware_image(
                            ctx.config.name.as_str(),
                            ctx.config.platform,
                        )
                    })
                };
                match image.and_then(|image| image.contiguous_window()) {
                    Some(window) => {
                        let base = window.cpu_base;
                        let size = window.size;
                        quote! {
                            Some(fstart_platform::car_teardown::PhysicalRange { base: #base, size: #size })
                        }
                    }
                    None => quote! { None },
                }
            }
        }
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

/// Emit the body of primitive `Board::stage_load_desc`.
pub(super) fn stage_load_desc_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let uses_stage_load = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::StageLoad { .. }));
    if !uses_stage_load {
        return quote! { None };
    }
    let x86_postcar = ctx.config.platform == Platform::X86_64;
    quote! {
        Some(fstart_stage_runtime::StageLoadDesc {
            x86_postcar: #x86_postcar,
        })
    }
}

/// Emit the body of the minimal x86 post-CAR stage-load primitive.
pub(super) fn stage_load_postcar_mmio_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let uses_stage_load = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::StageLoad { .. }));
    if ctx.config.platform != Platform::X86_64 || !uses_stage_load {
        return quote! {
            let _ = (next_stage, anchor, image_base, image_size);
            fstart_platform::halt()
        };
    }
    let postcar_config = x86_postcar_config_tokens(ctx);
    quote! {
        #postcar_config
        // SAFETY: the runtime executor only calls this primitive with a
        // contiguous firmware-image window selected from the active boot media.
        unsafe {
            fstart_platform::car_teardown::stage_load_mmio(
                &_FSTART_POSTCAR_CONFIG,
                next_stage,
                anchor,
                image_base,
                image_size,
            );
        }
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
            unreachable!("board_gen::return_to_fel: stage does not declare ReturnToFel")
        };
    }
    quote! {
        fstart_log::info!("returning to FEL mode...");
        // SAFETY: `save_boot_params` ran during platform entry and
        // populated the FEL stash before capability dispatch.
        unsafe { fstart_soc_sunxi::return_to_fel_from_stash() }
    }
}
