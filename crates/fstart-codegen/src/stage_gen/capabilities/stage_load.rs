//! Code generation for stage loading and anchor scanning capabilities.
//!
//! Handles `LoadNextStage` (multi-device, single-device, auto-detect) and
//! the FFS anchor scan for non-first stages.
//!
//! The actual block device read and handoff serialization are delegated
//! to library functions in [`fstart_capabilities::next_stage`]. Codegen
//! handles SoC-specific header parsing (eGON offsets) and multi-device
//! match dispatch.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_device_registry::DriverInstance;
use fstart_types::{BoardConfig, BootMedium, DeviceConfig, LoadDevice, Platform};

use super::super::tokens::hex_addr;
use super::{boot_media_values_for_device, egon_sram_base, require_egon_format};

/// Generate code to locate the FFS anchor block in boot media.
///
/// Used by non-first stages in a multi-stage build that don't have an
/// embedded `FSTART_ANCHOR` static.
///
/// - **Memory-mapped media**: calls `fstart_capabilities::scan_anchor_in_media()`
///   to scan the media slice for `FFS_MAGIC`.
/// - **Block device media (ARMv7)**: calls `fstart_capabilities::read_anchor_at_offset()`
///   to read the anchor at the known offset `ffs_total_size - ANCHOR_SIZE`.
///
/// Emits a `scanned_anchor_data: [u8; ANCHOR_SIZE]` local variable
/// that subsequent FFS capability calls reference via
/// `&scanned_anchor_data[..]`.
#[allow(dead_code)]
pub(in crate::stage_gen) fn generate_anchor_scan(
    medium: &BootMedium,
    config: &BoardConfig,
    halt: &TokenStream,
) -> TokenStream {
    match medium {
        BootMedium::MemoryMapped { .. } => {
            quote! {
                let scanned_anchor_data: [u8; fstart_types::ffs::ANCHOR_SIZE] =
                    fstart_capabilities::scan_anchor_in_media(&boot_media)
                        .unwrap_or_else(|_| {
                            fstart_log::error!("FATAL: FFS anchor not found in boot media");
                            #halt;
                        });
            }
        }
        BootMedium::Device { .. } | BootMedium::AutoDevice { .. } => {
            if let Err(err) = require_egon_format(config, "block device anchor scan") {
                return err;
            }
            let sram_base = hex_addr(egon_sram_base(config));
            let ffs_size_expr: TokenStream =
                quote! { fstart_soc_sunxi::ffs_total_size_at(#sram_base) as usize };
            quote! {
                let scanned_anchor_data: [u8; fstart_types::ffs::ANCHOR_SIZE] = {
                    let ffs_size = #ffs_size_expr;
                    if ffs_size < fstart_types::ffs::ANCHOR_SIZE {
                        fstart_log::error!("FATAL: ffs_total_size too small ({} bytes)", ffs_size as u32);
                        #halt;
                    }
                    let anchor_offset = ffs_size - fstart_types::ffs::ANCHOR_SIZE;
                    fstart_log::info!(
                        "reading FFS anchor at offset {:#x} (ffs_size={:#x})",
                        anchor_offset as u64,
                        ffs_size as u64,
                    );
                    fstart_capabilities::read_anchor_at_offset(&boot_media, anchor_offset)
                        .unwrap_or_else(|_| {
                            fstart_log::error!("FATAL: FFS anchor magic mismatch");
                            #halt;
                        })
                };
            }
        }
    }
}

/// Generate code for the LoadNextStage capability.
///
/// Reads the next stage's offset and size from the eGON header (ARMv7),
/// then delegates to `fstart_capabilities::next_stage::read_stage_to_addr`
/// for the block device read and
/// `fstart_capabilities::next_stage::write_handoff_and_jump` for the
/// handoff serialization and jump.
///
/// When multiple devices are specified, the boot device is auto-detected
/// via `fstart_soc_sunxi::boot_device()` and each match arm calls the
/// library functions with the resolved device.
#[allow(clippy::too_many_arguments)]
pub(in crate::stage_gen) fn generate_load_next_stage(
    load_devices: &[LoadDevice],
    next_stage: &str,
    config: &BoardConfig,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    _platform: Platform,
    capabilities: &[fstart_types::Capability],
    halt: &TokenStream,
) -> TokenStream {
    // Resolve the next stage's load_addr from the board config.
    let next_load_addr = match &config.stages {
        fstart_types::StageLayout::MultiStage(stages) => stages
            .iter()
            .find(|s| s.name.as_str() == next_stage)
            .map(|s| s.load_addr),
        _ => None,
    };
    let Some(next_load_addr) = next_load_addr else {
        let msg = format!(
            "LoadNextStage references next_stage '{next_stage}' which is not defined in stages"
        );
        return quote! { compile_error!(#msg); };
    };
    let load_addr = hex_addr(next_load_addr);

    // LoadNextStage requires an Allwinner eGON-format bootblock.
    if let Err(err) = require_egon_format(config, "LoadNextStage") {
        return err;
    }

    // SRAM base for eGON header access.
    let sram_base = hex_addr(egon_sram_base(config));

    // Handoff buffer: placed 4K below the next stage's load address.
    let handoff_addr = hex_addr(next_load_addr - 0x1000);

    // DRAM size: if DramInit was run in this stage, call the DRAMC
    // driver's detected_size_bytes() to get the runtime-detected value.
    let dram_device = capabilities.iter().find_map(|cap| {
        if let fstart_types::Capability::DramInit { device } = cap {
            Some(device.as_str())
        } else {
            None
        }
    });
    let dram_size_expr = match dram_device {
        Some(dev_name) => {
            let dev = format_ident!("{}", dev_name);
            quote! { #dev.detected_size_bytes() }
        }
        None => quote! { 0u64 },
    };

    // Common: read eGON header values + validate.
    let header_read = quote! {
        let ns_ffs_offset = fstart_soc_sunxi::next_stage_offset_at(#sram_base) as u64;
        let ns_size = fstart_soc_sunxi::next_stage_size_at(#sram_base) as usize;
        if ns_ffs_offset == 0 || ns_size == 0 {
            fstart_log::error!("FATAL: eGON header has zero next_stage_offset/size");
            #halt;
        }
    };

    // Common tail: serialize handoff and jump.
    let jump_tail = quote! {
        fstart_capabilities::next_stage::serialize_handoff(
            #dram_size_expr, #handoff_addr,
        ).unwrap_or_else(|_| {
            fstart_log::error!("FATAL: handoff serialize failed");
            #halt;
        });
        fstart_log::info!("jumping to stage '{}' at {:#x}", #next_stage, #load_addr as u64);
        fstart_platform::jump_to_with_handoff(#load_addr, #handoff_addr as usize);
    };

    if load_devices.len() == 1 {
        // Single device -- no auto-detection needed.
        let ld = &load_devices[0];
        let dev_name_str = ld.name.as_str();
        let dev_ident = format_ident!("{}", dev_name_str);
        let base_off = hex_addr(ld.base_offset);

        return quote! {
            #header_read
            let dev_offset = #base_off + ns_ffs_offset;
            fstart_capabilities::next_stage::read_stage_to_addr(
                &#dev_ident, #dev_name_str, #next_stage,
                dev_offset, #load_addr, ns_size,
            ).unwrap_or_else(|_| {
                fstart_log::error!("FATAL: failed to read stage from {}", #dev_name_str);
                #halt;
            });
            #jump_tail
        };
    }

    // Multiple devices -- auto-detect via eGON header boot_media field.
    let mut match_arms = TokenStream::new();
    for ld in load_devices {
        let dev_name_str = ld.name.as_str();
        let dev_ident = format_ident!("{}", dev_name_str);
        let base_off = hex_addr(ld.base_offset);

        let bm_values = boot_media_values_for_device(dev_name_str, devices, instances);
        for val in &bm_values {
            let val_lit = Literal::u8_unsuffixed(*val);

            match_arms.extend(quote! {
                #val_lit => {
                    let dev_offset = #base_off + ns_ffs_offset;
                    fstart_capabilities::next_stage::read_stage_to_addr(
                        &#dev_ident, #dev_name_str, #next_stage,
                        dev_offset, #load_addr, ns_size,
                    ).unwrap_or_else(|_| {
                        fstart_log::error!("FATAL: failed to read stage from {}", #dev_name_str);
                        #halt;
                    });
                    #jump_tail
                }
            });
        }
    }

    quote! {
        #header_read
        let bm = fstart_soc_sunxi::boot_media_at(#sram_base);
        fstart_log::info!("boot media detect: {:#x}", bm);
        match bm {
            #match_arms
            _ => {
                fstart_log::error!("FATAL: unknown boot medium: {:#x}", bm);
                #halt;
            }
        }
    }
}
