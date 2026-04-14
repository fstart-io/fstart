//! Code generation for payload loading capabilities.
//!
//! Handles Linux boot, UEFI payload (CrabEFI), and FIT image loading.
//! Platform boot protocol code and firmware FFS loading are generated
//! by shared helpers to avoid duplication across payload variants.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_types::memory::RegionKind;
use fstart_types::{
    BoardConfig, FirmwareConfig, FirmwareKind, PayloadConfig, Platform, StageLayout,
};

use super::super::tokens::{anchor_as_bytes_expr, anchor_expr, halt_expr, hex_addr};
use super::super::validation::{is_fit_image, is_fit_runtime, is_linux_boot, is_uefi_payload};

/// Generate code for the PayloadLoad capability.
pub(in crate::stage_gen) fn generate_payload_load(
    config: &BoardConfig,
    platform: Platform,
    embed_anchor: bool,
) -> TokenStream {
    if is_uefi_payload(config) {
        return generate_payload_load_uefi(config, platform, embed_anchor);
    }

    if is_linux_boot(config) {
        return generate_payload_load_linux(config, platform, embed_anchor);
    }

    if is_fit_image(config) {
        if is_fit_runtime(config) {
            return generate_payload_load_fit_runtime(config, platform);
        } else {
            // Buildtime FIT: xtask extracts components from FIT and embeds
            // them as separate FFS entries. Runtime code loads them the same
            // way as LinuxBoot (individual kernel/ramdisk blobs from FFS).
            return generate_payload_load_linux(config, platform, embed_anchor);
        }
    }

    let anchor = anchor_expr(embed_anchor);
    let jump_fn: TokenStream = quote! { fstart_platform::jump_to };
    quote! {
        fstart_capabilities::payload_load(#anchor, &boot_media, #jump_fn);
    }
}

// ---------------------------------------------------------------------------
// Shared helpers for payload generators
// ---------------------------------------------------------------------------

/// Generate code to load firmware (SBI/ATF) from FFS.
///
/// Shared across Linux boot, FIT boot, and UEFI boot paths. Emits a
/// `load_ffs_file_by_type` call for `FileType::Firmware` with appropriate
/// logging and error handling.
fn generate_firmware_load(
    firmware: &FirmwareConfig,
    anchor: &TokenStream,
    halt: &TokenStream,
) -> TokenStream {
    let fw_kind_str = match firmware.kind {
        FirmwareKind::OpenSbi => "SBI firmware",
        FirmwareKind::ArmTrustedFirmware => "ATF BL31",
    };
    let load_msg = format!("loading {fw_kind_str}...");
    let error_msg = format!("FATAL: failed to load {fw_kind_str}");
    quote! {
        fstart_log::info!(#load_msg);
        if !fstart_capabilities::load_ffs_file_by_type(
            #anchor,
            &boot_media,
            fstart_types::ffs::FileType::Firmware,
        ) {
            fstart_log::error!(#error_msg);
            #halt;
        }
    }
}

/// Generate the platform-specific boot protocol sequence.
///
/// Shared between Linux boot and FIT runtime boot. The `kernel_addr`
/// parameter is a TokenStream expression for the kernel's load address
/// (a hex literal for Linux boot, a variable name for FIT boot).
fn generate_platform_boot_protocol(
    platform: Platform,
    kernel_addr: &TokenStream,
    payload: &PayloadConfig,
) -> TokenStream {
    let dtb_addr = hex_addr(payload.dtb_addr.unwrap_or(0));

    match platform {
        Platform::Riscv64 => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            quote! {
                let _fw_info = fstart_platform::FwDynamicInfo::new(
                    #kernel_addr,
                    fstart_platform::boot_hart_id(),
                );
                fstart_log::info!("jumping to SBI firmware...");
                fstart_platform::boot_linux_sbi(
                    #fw_addr,
                    fstart_platform::boot_hart_id(),
                    #dtb_addr,
                    &_fw_info,
                );
            }
        }
        Platform::Aarch64 => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            quote! {
                fstart_log::info!("jumping to ATF BL31...");
                fstart_platform::boot_linux_atf_prepared(#kernel_addr, #dtb_addr, #fw_addr);
            }
        }
        Platform::Armv7 => {
            quote! {
                fstart_log::info!("booting Linux (ARMv7)...");
                fstart_log::info!("  kernel @ {:#x}", #kernel_addr as u64);
                fstart_log::info!("  dtb    @ {:#x}", #dtb_addr as u64);
                fstart_platform::cleanup_before_linux();
                fstart_platform::boot_linux(#kernel_addr as u64, #dtb_addr);
            }
        }
        Platform::X86_64 => {
            let bootargs_str = payload.bootargs.as_deref().unwrap_or("console=ttyS0");
            quote! {
                fstart_log::info!("booting Linux (x86_64)...");
                fstart_log::info!("  kernel @ {:#x}", #kernel_addr as u64);
                // x86 Linux boot protocol: fill zero page and jump.
                fstart_platform::boot_linux(
                    #kernel_addr as u64,
                    _acpi_rsdp_addr,
                    &_e820_entries[.._e820_count],
                    #bootargs_str,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Linux boot
// ---------------------------------------------------------------------------

/// Generate the Linux boot payload sequence for a specific platform.
fn generate_payload_load_linux(
    config: &BoardConfig,
    platform: Platform,
    embed_anchor: bool,
) -> TokenStream {
    let payload = config.payload.as_ref().unwrap(); // caller verified is_linux_boot
    let halt = halt_expr(platform);
    let anchor = anchor_expr(embed_anchor);

    let mut stmts = TokenStream::new();

    stmts.extend(quote! {
        fstart_log::info!("capability: PayloadLoad (LinuxBoot)");
    });

    // Load firmware blob from FFS FIRST -- it goes to a high address
    // (e.g., 0x82000000) that doesn't overlap with the FFS image or
    // currently-executing code.
    if let Some(ref fw) = payload.firmware {
        stmts.extend(generate_firmware_load(
            fw,
            &anchor_expr(embed_anchor),
            &halt,
        ));
    }

    // Load kernel
    stmts.extend(quote! {
        fstart_log::info!("loading kernel...");
        if !fstart_capabilities::load_ffs_file_by_type(
            #anchor,
            &boot_media,
            fstart_types::ffs::FileType::Payload,
        ) {
            fstart_log::error!("FATAL: failed to load kernel");
            #halt;
        }
    });

    // Platform-specific boot protocol.
    let kernel_addr = hex_addr(payload.kernel_load_addr.unwrap_or(0));
    stmts.extend(generate_platform_boot_protocol(
        platform,
        &kernel_addr,
        payload,
    ));

    stmts
}

// ---------------------------------------------------------------------------
// FIT runtime boot
// ---------------------------------------------------------------------------

/// Generate code for the FIT runtime payload load sequence.
///
/// At runtime, the whole FIT (.itb) is stored in FFS. The firmware uses
/// `fstart_capabilities::fit::load_fit_components()` to parse and load
/// FIT components, then boots via the platform-specific protocol.
fn generate_payload_load_fit_runtime(config: &BoardConfig, platform: Platform) -> TokenStream {
    let payload = config.payload.as_ref().unwrap();
    let halt = halt_expr(platform);
    let anchor = anchor_as_bytes_expr();

    let config_expr = match &payload.fit_config {
        Some(name) => {
            let name_str = name.as_str();
            quote! { Some(#name_str) }
        }
        None => quote! { None },
    };

    let mut stmts = TokenStream::new();

    stmts.extend(quote! {
        fstart_log::info!("capability: PayloadLoad (FIT runtime)");
        let _fit_boot = fstart_capabilities::fit::load_fit_components(
            #anchor,
            &boot_media,
            #config_expr,
        ).unwrap_or_else(|e| {
            fstart_log::error!("FATAL: FIT boot failed: {}", fstart_capabilities::fit::error_str(&e));
            #halt;
        });
        let _kernel_load = _fit_boot.kernel_addr;
    });

    // Load firmware blob from FFS (SBI/ATF is separate from FIT)
    if let Some(ref fw) = payload.firmware {
        stmts.extend(generate_firmware_load(fw, &anchor_as_bytes_expr(), &halt));
    }

    // Platform-specific boot protocol (shared with Linux boot).
    let kernel_addr = quote! { _kernel_load };
    stmts.extend(generate_platform_boot_protocol(
        platform,
        &kernel_addr,
        payload,
    ));

    stmts
}

// ---------------------------------------------------------------------------
// UEFI payload (CrabEFI)
// ---------------------------------------------------------------------------

/// Generate the CrabEFI UEFI payload initialization sequence.
///
/// Constructs a `PlatformConfig` from fstart's initialized drivers and calls
/// `crabefi::init_platform()` which never returns.
fn generate_payload_load_uefi(
    config: &BoardConfig,
    platform: Platform,
    _embed_anchor: bool,
) -> TokenStream {
    // Collect static memory map entries (ROM, Reserved) from board config.
    // RAM regions are split at runtime by build_efi_memory_map().
    let mut static_mem_entries = TokenStream::new();
    for region in &config.memory.regions {
        let base = hex_addr(region.base);
        let size = hex_addr(region.size);
        match region.kind {
            RegionKind::Rom => {
                static_mem_entries.extend(quote! {
                    fstart_crabefi::MemoryRegion {
                        base: #base,
                        size: #size,
                        region_type: fstart_crabefi::MemoryType::RuntimeServicesCode,
                    },
                });
            }
            RegionKind::Reserved => {
                static_mem_entries.extend(quote! {
                    fstart_crabefi::MemoryRegion {
                        base: #base,
                        size: #size,
                        region_type: fstart_crabefi::MemoryType::Reserved,
                    },
                });
            }
            RegionKind::Ram => {}
        }
    }

    // RAM region from board config.
    let ram_region = config
        .memory
        .regions
        .iter()
        .find(|r| r.kind == RegionKind::Ram);
    let ram_base_lit = ram_region
        .map(|r| hex_addr(r.base))
        .unwrap_or_else(|| quote! { 0u64 });
    let ram_size_lit = ram_region
        .map(|r| hex_addr(r.size))
        .unwrap_or_else(|| quote! { 0u64 });

    // Firmware data/stack addresses from stage config.
    let (fw_data_addr, fw_stack_size) = match &config.stages {
        StageLayout::Monolithic(mono) => (
            mono.data_addr.unwrap_or(mono.load_addr),
            mono.stack_size as u64,
        ),
        StageLayout::MultiStage(stages) => {
            let last = stages.last().unwrap();
            (
                last.data_addr.unwrap_or(last.load_addr),
                last.stack_size as u64,
            )
        }
    };
    let fw_data_addr_lit = hex_addr(fw_data_addr);
    let fw_stack_size_lit = hex_addr(fw_stack_size);

    // Console device for DebugOutput adapter.
    let console_device = config
        .devices
        .iter()
        .find(|d| d.services.iter().any(|s| s.as_str() == "Console"))
        .map(|d| d.name.as_str());

    let console_setup = if let Some(name) = console_device {
        let dev = format_ident!("{}", name);
        quote! {
            let mut _crabefi_console = fstart_crabefi::ConsoleAdapter(&#dev);
        }
    } else {
        quote! {}
    };

    let debug_output_field = if console_device.is_some() {
        quote! { debug_output: Some(&mut _crabefi_console), }
    } else {
        quote! { debug_output: None, }
    };

    // PCI device for ECAM base.
    let pci_device = config
        .devices
        .iter()
        .find(|d| d.services.iter().any(|s| s.as_str() == "PciRootBus"));

    let ecam_base_field = if let Some(pci_dev) = pci_device {
        let dev = format_ident!("{}", pci_dev.name.as_str());
        quote! { ecam_base: Some(#dev.ecam_base()), }
    } else {
        quote! { ecam_base: None, }
    };

    // Framebuffer device for GOP.
    let fb_device = config
        .devices
        .iter()
        .find(|d| d.services.iter().any(|s| s.as_str() == "Framebuffer"));

    let (fb_setup, framebuffer_field) = if let Some(fb_dev) = fb_device {
        let dev = format_ident!("{}", fb_dev.name.as_str());
        let ok_var = format_ident!("_{}_ok", fb_dev.name.as_str());
        let setup = quote! {
            let _fb_config = if #ok_var {
                let _fb_info = #dev.info();
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
        };
        let field = quote! { framebuffer: _fb_config, };
        (setup, field)
    } else {
        (quote! {}, quote! { framebuffer: None, })
    };

    // FDT sourcing
    let payload = config.payload.as_ref();
    let fdt_addr_expr = if let Some(addr) = payload.and_then(|p| p.src_dtb_addr) {
        let addr_lit = hex_addr(addr);
        quote! { #addr_lit }
    } else {
        match platform {
            Platform::Aarch64 | Platform::Riscv64 => {
                quote! { fstart_platform::boot_dtb_addr() }
            }
            Platform::Armv7 | Platform::X86_64 => quote! { 0u64 },
        }
    };

    let fdt_setup = match platform {
        Platform::Aarch64 | Platform::Riscv64 => {
            quote! {
                let _fdt_addr = #fdt_addr_expr;
                // SAFETY: platform guarantees _fdt_addr points to a valid
                // FDT blob in DRAM (saved from boot register on entry).
                let _fdt_blob: Option<&[u8]> =
                    unsafe { fstart_capabilities::fdt_blob_from_addr(_fdt_addr) };
            }
        }
        Platform::Armv7 | Platform::X86_64 => quote! {
            let _fdt_addr: u64 = 0;
            let _fdt_blob: Option<&[u8]> = None;
        },
    };

    let fdt_field = quote! {
        fdt: _fdt_blob,
    };

    // Generate BL31 firmware loading when configured.
    let payload = config.payload.as_ref();
    let bl31_boot = if let Some(fw) = payload.and_then(|p| p.firmware.as_ref()) {
        if platform == Platform::Aarch64 && fw.kind == FirmwareKind::ArmTrustedFirmware {
            let anchor_fw = anchor_as_bytes_expr();
            let halt = halt_expr(platform);
            let fw_load_addr = hex_addr(fw.load_addr);
            quote! {
                fstart_log::info!("loading TF-A BL31 firmware...");
                if !fstart_capabilities::load_ffs_file_by_type(
                    #anchor_fw,
                    &boot_media,
                    fstart_types::ffs::FileType::Firmware,
                ) {
                    fstart_log::error!("FATAL: failed to load BL31 firmware");
                    #halt;
                }
                fstart_log::info!("booting BL31 (GIC, PSCI, NS switch)...");
                fstart_platform::boot_bl31_and_resume(
                    #fw_load_addr,
                    fstart_platform::boot_dtb_addr(),
                );
                fstart_log::info!("resumed from BL31 at EL2 NS");
            }
        } else {
            quote! {}
        }
    } else {
        quote! {}
    };

    quote! {
        fstart_log::info!("Launching CrabEFI UEFI payload...");

        #bl31_boot

        let _crabefi_timer = fstart_crabefi::ArmGenericTimer::new();
        let _crabefi_reset = fstart_crabefi::PsciReset;
        #console_setup

        // Determine FDT address and prepare the blob.
        #fdt_setup

        // FDT reservation: if an FDT is present, read its page-aligned
        // size so build_efi_memory_map() can carve it out.
        let _fdt_reservation = if _fdt_addr != 0 {
            let fdt_size = unsafe {
                fstart_crabefi::fdt_page_aligned_size(_fdt_addr)
            };
            Some((_fdt_addr, fdt_size))
        } else {
            None
        };

        // Build the EFI memory map.
        let _static_entries: &[fstart_crabefi::MemoryRegion] = &[
            #static_mem_entries
        ];

        let mut _crabefi_mem_buf: [fstart_crabefi::MemoryRegion; 12] = [
            fstart_crabefi::MemoryRegion {
                base: 0, size: 0,
                region_type: fstart_crabefi::MemoryType::Reserved,
            };
            12
        ];

        let _mem_idx = fstart_crabefi::build_efi_memory_map(
            _static_entries,
            #ram_base_lit,
            #ram_size_lit,
            #fw_data_addr_lit,
            #fw_stack_size_lit,
            #fw_stack_size_lit,
            _fdt_reservation,
            &mut _crabefi_mem_buf,
        );

        let _crabefi_memory_map: &[fstart_crabefi::MemoryRegion] =
            &_crabefi_mem_buf[.._mem_idx];
        fstart_log::info!("EFI memory map: {} entries", _mem_idx as u32);

        #fb_setup

        let _crabefi_config = fstart_crabefi::PlatformConfig {
            memory_map: _crabefi_memory_map,
            timer: &_crabefi_timer,
            reset: &_crabefi_reset,
            block_devices: &mut [],
            variable_backend: None,
            #debug_output_field
            console_input: None,
            #framebuffer_field
            acpi_rsdp: None,
            smbios: None,
            #fdt_field
            rng: None,
            #ecam_base_field
            deferred_buffer: None,
            runtime_region: None,
            heap_pre_initialized: false,
        };

        // init_platform() is -> ! (never returns).
        fstart_crabefi::init_platform(_crabefi_config);
    }
}
