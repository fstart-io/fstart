//! Shared boot-media dispatch helpers.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;

use super::model::BoardEmitModel;

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

/// Emit the body of primitive `Board::with_block_device`.
pub(super) fn with_block_device_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let arms = ctx
        .runtime_devices
        .providers(Service::BlockDevice)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            let dev_name = device.name;
            quote! {
                #id_lit => Ok(run(
                    self.#field
                        .as_ref()
                        .ok_or(fstart_stage_runtime::RuntimeError::UnknownDevice)?,
                    #dev_name,
                )),
            }
        });
    quote! {
        match id {
            #(#arms)*
            _ => Err(fstart_stage_runtime::RuntimeError::UnknownDevice),
        }
    }
}

/// Emit the body of primitive `Board::active_firmware_window`.
pub(super) fn active_firmware_window_body() -> TokenStream {
    quote! {
        match self._boot_media {
            fstart_stage_runtime::BootMediaState::FirmwareImage { image, .. } => {
                image.contiguous_window().map(|window| (window.cpu_base, window.size))
            }
            fstart_stage_runtime::BootMediaState::None
            | fstart_stage_runtime::BootMediaState::FirmwareImageBlock { .. } => None,
        }
    }
}

/// Emit the body of the primitive `Board::with_boot_media` dispatcher.
pub(super) fn with_boot_media_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let block_arms = block_device_with_boot_media_arms(ctx);
    quote! {
        match self._boot_media {
            fstart_stage_runtime::BootMediaState::None => none,
            fstart_stage_runtime::BootMediaState::FirmwareImage { image, temp_ram_buffer } => {
                let mut scratch = temp_ram_buffer.and_then(|buffer| {
                    // SAFETY: board/platform configuration declares this range
                    // as temporary writable RAM for this stage.
                    unsafe { fstart_services::TempRamArena::new(buffer).ok() }
                });
                fstart_log::info!("boot-media match: firmware image size={:#x} windows={}", image.size, image.window_count);
                #[cfg(feature = "x86_64")]
                fstart_platform::enable_boot_media_rom_cache();
                let map = fstart_services::boot_media::FirmwareImageMap::new(image);
                let media = fstart_services::boot_media::MemoryMapped::new(
                    map,
                    image.size as usize,
                );
                run(&media, scratch.as_mut())
            }
            fstart_stage_runtime::BootMediaState::FirmwareImageBlock {
                device_id,
                offset,
                size,
                temp_ram_buffer,
            } => {
                let _ = temp_ram_buffer;
                match device_id {
                    #block_arms
                    _ => {
                        fstart_log::error!("{}: unknown block device id {}", caller_tag, device_id);
                        fstart_platform::halt();
                    }
                }
            }
        }
    }
}

/// Emit one `with_boot_media` match arm per enabled block device in the board.
fn block_device_with_boot_media_arms(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let arms = ctx
        .runtime_devices
        .providers(Service::BlockDevice)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            quote! {
                #id_lit => {
                    let mut scratch = temp_ram_buffer.and_then(|buffer| {
                        // SAFETY: board/platform configuration declares this range
                        // as temporary writable RAM for this stage.
                        unsafe { fstart_services::TempRamArena::new(buffer).ok() }
                    });
                    let media = fstart_services::boot_media::BlockDeviceMedia::new(
                        self.#field
                            .as_ref()
                            .unwrap_or_else(|| fstart_platform::halt()),
                        offset,
                        size as usize,
                    );
                    run(&media, scratch.as_mut())
                }
            }
        });
    quote! { #(#arms)* }
}
