//! UEFI/CrabEFI primitive descriptor and service emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::memory::RegionKind;
use fstart_types::{FirmwareKind, Platform, StageLayout};

use crate::stage_gen::tokens::hex_addr;
use crate::stage_gen::validation::is_uefi_payload;

use super::model::BoardEmitModel;

fn stage_has_uefi_payload(ctx: &BoardEmitModel<'_>) -> bool {
    ctx.stage
        .capabilities
        .iter()
        .any(|cap| matches!(cap, fstart_types::Capability::PayloadLoad))
        && is_uefi_payload(ctx.config)
}

/// Emit primitive `Board::uefi_payload_desc` data.
pub(super) fn uefi_payload_desc_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !stage_has_uefi_payload(ctx) {
        return quote! { None };
    }

    let payload = ctx.config.payload.as_ref().expect("UEFI implies payload");

    let mut static_mem_entries = TokenStream::new();
    for region in &ctx.config.memory.regions {
        let base = hex_addr(region.base);
        let size = hex_addr(region.size);
        match region.kind {
            RegionKind::Rom | RegionKind::Reserved => static_mem_entries.extend(quote! {
                fstart_crabefi::MemoryRegion {
                    base: #base,
                    size: #size,
                    region_type: fstart_crabefi::MemoryType::Reserved,
                },
            }),
            RegionKind::Ram => {}
        }
    }

    let ram_region = ctx
        .config
        .memory
        .regions
        .iter()
        .find(|region| region.kind == RegionKind::Ram);
    let ram_base_lit = ram_region
        .map(|region| hex_addr(region.base))
        .unwrap_or_else(|| quote! { 0u64 });
    let ram_size_lit = ram_region
        .map(|region| hex_addr(region.size))
        .unwrap_or_else(|| quote! { 0u64 });

    let (fw_data_addr, fw_stack_size) = match &ctx.config.stages {
        StageLayout::Monolithic(mono) => (
            mono.data_addr.unwrap_or(mono.load_addr),
            mono.stack_size as u64,
        ),
        StageLayout::MultiStage(stages) => {
            let last = stages.last().expect("multi-stage has at least one stage");
            (
                last.data_addr.unwrap_or(last.load_addr),
                last.stack_size as u64,
            )
        }
    };
    let fw_data_addr_lit = hex_addr(fw_data_addr);
    let fw_stack_size_lit = hex_addr(fw_stack_size);

    let fdt_addr = if let Some(addr) = payload.src_dtb_addr {
        hex_addr(addr)
    } else {
        match platform {
            Platform::Aarch64 | Platform::Riscv64 => quote! { fstart_platform::boot_dtb_addr() },
            Platform::Armv7 | Platform::X86_64 => quote! { 0u64 },
        }
    };

    let bl31_load_addr = if let Some(fw) = payload.firmware.as_ref() {
        if platform == Platform::Aarch64 && fw.kind == FirmwareKind::ArmTrustedFirmware {
            let addr = hex_addr(fw.load_addr);
            quote! { Some(#addr) }
        } else {
            quote! { None }
        }
    } else {
        quote! { None }
    };

    let mode = if platform == Platform::X86_64 {
        quote! { fstart_stage_runtime::UefiLaunchMode::X86 }
    } else {
        quote! { fstart_stage_runtime::UefiLaunchMode::Flat }
    };
    let acpi_rsdp = platform == Platform::X86_64;
    let smbios = ctx.stage.uses_smbios;
    let reserve_acpi = platform == Platform::X86_64 && ctx.stage.uses_acpi_prepare;
    let reserve_smbios = ctx.stage.uses_smbios;

    quote! {
        const _UEFI_STATIC_ENTRIES: &[fstart_crabefi::MemoryRegion] = &[
            #static_mem_entries
        ];
        Some(fstart_stage_runtime::UefiPayloadDesc {
            mode: #mode,
            static_entries: _UEFI_STATIC_ENTRIES,
            ram_base: #ram_base_lit,
            ram_size: #ram_size_lit,
            fw_data_addr: #fw_data_addr_lit,
            fw_stack_size: #fw_stack_size_lit,
            fdt_addr: #fdt_addr,
            bl31_load_addr: #bl31_load_addr,
            acpi_rsdp: #acpi_rsdp,
            smbios: #smbios,
            reserve_acpi: #reserve_acpi,
            reserve_smbios: #reserve_smbios,
        })
    }
}

/// Emit primitive `Board::with_uefi_services` service borrows.
pub(super) fn with_uefi_services_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !stage_has_uefi_payload(ctx) {
        return quote! {
            run(fstart_stage_runtime::UefiServices {
                console: None,
                framebuffer: None,
                ecam: None,
            })
        };
    }

    let console_device = ctx.runtime_devices.providers(Service::Console).next();
    let console_setup = match console_device {
        Some(device) => {
            let field = format_ident!("{}", device.name);
            quote! {
                let _uefi_console: Option<&dyn fstart_services::Console> = Some(
                    self.#field.as_ref().unwrap_or_else(|| fstart_platform::halt())
                );
            }
        }
        None => quote! { let _uefi_console: Option<&dyn fstart_services::Console> = None; },
    };

    let pci_device = ctx.runtime_devices.providers(Service::PciRootBus).next();
    let ecam_setup = match pci_device {
        Some(device) => {
            let field = format_ident!("{}", device.name);
            quote! {
                let _uefi_ecam = {
                    let _pci = self.#field.as_ref().unwrap_or_else(|| fstart_platform::halt());
                    Some((_pci.ecam_base(), _pci.ecam_size()))
                };
            }
        }
        None => quote! { let _uefi_ecam = None; },
    };

    let fb_device = ctx.runtime_devices.providers(Service::Framebuffer).next();
    let framebuffer_setup = match fb_device {
        Some(device) => {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            quote! {
                let _uefi_framebuffer = if self._inited.contains(#id_lit) {
                    let _fb_ref = self.#field
                        .as_ref()
                        .unwrap_or_else(|| fstart_platform::halt());
                    let _fb_info = _fb_ref.info();
                    Some(fstart_crabefi::FramebufferConfig {
                        physical_address: _fb_info.base_addr,
                        width: _fb_info.width,
                        height: _fb_info.height,
                        stride: _fb_info.stride,
                        bits_per_pixel: _fb_info.bits_per_pixel,
                        red_mask_pos: _fb_info.red_pos,
                        red_mask_size: _fb_info.red_size,
                        green_mask_pos: _fb_info.green_pos,
                        green_mask_size: _fb_info.green_size,
                        blue_mask_pos: _fb_info.blue_pos,
                        blue_mask_size: _fb_info.blue_size,
                    })
                } else {
                    None
                };
            }
        }
        None => quote! { let _uefi_framebuffer = None; },
    };

    quote! {
        #console_setup
        #ecam_setup
        #framebuffer_setup
        run(fstart_stage_runtime::UefiServices {
            console: _uefi_console,
            framebuffer: _uefi_framebuffer,
            ecam: _uefi_ecam,
        })
    }
}

/// Emit primitive `Board::uefi_fdt_blob` implementation.
pub(super) fn uefi_fdt_blob_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !stage_has_uefi_payload(ctx) {
        return quote! { None };
    }

    match platform {
        Platform::Aarch64 | Platform::Riscv64 => quote! {
            // SAFETY: platform entry saved a valid boot FDT address; runtime
            // passes that address back unchanged through the UEFI descriptor.
            unsafe { fstart_capabilities::fdt_blob_from_addr(addr) }
        },
        Platform::Armv7 | Platform::X86_64 => quote! {
            let _ = addr;
            None
        },
    }
}

/// Emit primitive `Board::uefi_boot_bl31_and_resume` implementation.
pub(super) fn uefi_boot_bl31_and_resume_body(platform: Platform) -> TokenStream {
    if platform == Platform::Aarch64 {
        quote! {
            fstart_platform::boot_bl31_and_resume(fw_load_addr, fdt_addr);
        }
    } else {
        quote! {
            let _ = (fw_load_addr, fdt_addr);
        }
    }
}

/// Emit primitive `Board::park_aps_for_payload` implementation.
pub(super) fn park_aps_for_payload_body(platform: Platform) -> TokenStream {
    if platform == Platform::X86_64 {
        quote! {
            #[cfg(feature = "mp")]
            fstart_mp::park_aps_for_payload();
        }
    } else {
        quote! {}
    }
}

/// Emit primitive `Board::disable_boot_media_rom_cache_for_handoff` implementation.
pub(super) fn disable_boot_media_rom_cache_for_handoff_body(platform: Platform) -> TokenStream {
    if platform == Platform::X86_64 {
        quote! {
            fstart_platform::disable_boot_media_rom_cache_for_handoff();
        }
    } else {
        quote! {}
    }
}
