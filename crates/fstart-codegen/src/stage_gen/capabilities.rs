//! Code generation for individual stage capabilities.
//!
//! Each capability (ConsoleInit, MemoryInit, DriverInit, BootMedia, SigVerify,
//! FdtPrepare, PayloadLoad, StageLoad) has a dedicated generator function that
//! emits the corresponding [`proc_macro2::TokenStream`] for inclusion in
//! `fstart_main()`.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_device_registry::DriverInstance;
use fstart_types::memory::RegionKind;
use fstart_types::Platform;
use fstart_types::{
    AutoBootDevice, BoardConfig, BootMedium, BuildMode, DeviceConfig, FdtSource, FirmwareKind,
    LoadDevice, SocImageFormat,
};

use super::flexible::{flexible_enum_for_device, generate_flexible_wrapping};
use super::registry::find_driver_meta;
use super::tokens::{anchor_as_bytes_expr, anchor_expr, halt_expr, hex_addr};
use super::validation::{is_fit_image, is_fit_runtime, is_linux_boot};

/// Generate code for the ConsoleInit capability.
pub(super) fn generate_console_init(
    device_name: &str,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    halt: &TokenStream,
    mode: BuildMode,
) -> TokenStream {
    let Some((idx, dev)) = devices
        .iter()
        .enumerate()
        .find(|(_, d)| d.name.as_str() == device_name)
    else {
        let msg = format!("ConsoleInit references device '{device_name}' which is not declared");
        return quote! { compile_error!(#msg); };
    };

    let inst = &instances[idx];
    let drv_name = inst.meta().name;

    if find_driver_meta(drv_name).is_none() {
        let msg = format!("device '{device_name}' uses unknown driver '{drv_name}'");
        return quote! { compile_error!(#msg); };
    }

    if !dev.services.iter().any(|s| s.as_str() == "Console") {
        let msg = format!(
            "ConsoleInit requires Console service but device '{device_name}' does not provide it"
        );
        return quote! { compile_error!(#msg); };
    }

    let device = format_ident!("{}", device_name);

    match mode {
        BuildMode::Rigid => {
            quote! {
                #device.init().unwrap_or_else(|_| #halt);
                unsafe { fstart_log::init(&#device) };
                fstart_capabilities::console_ready(#device_name, #drv_name);
            }
        }
        BuildMode::Flexible => {
            let inner = if flexible_enum_for_device(dev, inst).is_some() {
                format_ident!("_{}_inner", device_name)
            } else {
                format_ident!("{}", device_name)
            };
            let wrapping = generate_flexible_wrapping(dev, inst);
            quote! {
                #inner.init().unwrap_or_else(|_| #halt);
                #wrapping
                unsafe { fstart_log::init(&#device) };
                fstart_capabilities::console_ready(#device_name, #drv_name);
            }
        }
    }
}

/// Generate code for the ClockInit capability.
///
/// Finds the referenced clock device, calls its `init()` method, and
/// logs the result. Analogous to ConsoleInit but for clock controllers.
pub(super) fn generate_clock_init(
    device_name: &str,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    halt: &TokenStream,
) -> TokenStream {
    let Some((idx, _dev)) = devices
        .iter()
        .enumerate()
        .find(|(_, d)| d.name.as_str() == device_name)
    else {
        let msg = format!("ClockInit references device '{device_name}' which is not declared");
        return quote! { compile_error!(#msg); };
    };

    let inst = &instances[idx];
    let drv_name = inst.meta().name;
    let device = format_ident!("{}", device_name);

    quote! {
        #device.init().unwrap_or_else(|_| #halt);
        fstart_log::info!("clock init complete: {} ({})", #device_name, #drv_name);
    }
}

/// Generate code for the DramInit capability.
///
/// Finds the referenced DRAM controller device, calls its `init()`,
/// and logs the detected memory size.
pub(super) fn generate_dram_init(
    device_name: &str,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    halt: &TokenStream,
) -> TokenStream {
    let Some((idx, _dev)) = devices
        .iter()
        .enumerate()
        .find(|(_, d)| d.name.as_str() == device_name)
    else {
        let msg = format!("DramInit references device '{device_name}' which is not declared");
        return quote! { compile_error!(#msg); };
    };

    let inst = &instances[idx];
    let drv_name = inst.meta().name;
    let device = format_ident!("{}", device_name);

    quote! {
        #device.init().unwrap_or_else(|_| {
            fstart_log::error!("FATAL: DRAM init failed ({})", #drv_name);
            #halt
        });
        fstart_log::info!("DRAM init complete: {} ({})", #device_name, #drv_name);
    }
}

/// Generate code for the MemoryInit capability.
pub(super) fn generate_memory_init() -> TokenStream {
    quote! { fstart_capabilities::memory_init(); }
}

/// Generate code for the DriverInit capability.
///
/// When `boot_media_gated` is non-empty (multi-device `LoadNextStage` or
/// `BootMedia(AutoDevice)`), the listed devices are only initialised if
/// the eGON header's `boot_media` field matches.  This prevents, e.g.,
/// trying to bring up the MMC controller when the BROM booted from SPI
/// and no SD card is inserted.
#[allow(clippy::too_many_arguments)]
pub(super) fn generate_driver_init(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    sorted_indices: &[usize],
    already_inited: &[String],
    boot_media_gated: &[(String, Vec<u8>)],
    platform: Platform,
    halt: &TokenStream,
    mode: BuildMode,
) -> TokenStream {
    let mut stmts = TokenStream::new();
    let mut count = 0usize;

    let has_gated = !boot_media_gated.is_empty() && platform == Platform::Armv7;
    if has_gated {
        stmts.extend(quote! {
            let _bm = fstart_soc_sunxi::boot_media();
        });
    }

    for &idx in sorted_indices {
        let dev = &devices[idx];
        let inst = &instances[idx];
        let name_str = dev.name.as_str();
        if already_inited.iter().any(|s| s == name_str) {
            continue;
        }

        // Check if this device is boot-media-gated.
        let gated_values = boot_media_gated
            .iter()
            .find(|(n, _)| n == name_str)
            .map(|(_, vals)| vals.as_slice());

        match mode {
            BuildMode::Rigid => {
                let name = format_ident!("{}", name_str);
                if let Some(vals) = gated_values {
                    let val_lits: Vec<_> =
                        vals.iter().map(|v| Literal::u8_unsuffixed(*v)).collect();
                    stmts.extend(quote! {
                        if matches!(_bm, #(#val_lits)|*) {
                            #name.init().unwrap_or_else(|_| #halt);
                        }
                    });
                } else {
                    stmts.extend(quote! {
                        #name.init().unwrap_or_else(|_| #halt);
                    });
                }
            }
            BuildMode::Flexible => {
                let inner = if flexible_enum_for_device(dev, inst).is_some() {
                    format_ident!("_{}_inner", name_str)
                } else {
                    format_ident!("{}", name_str)
                };
                if let Some(vals) = gated_values {
                    let val_lits: Vec<_> =
                        vals.iter().map(|v| Literal::u8_unsuffixed(*v)).collect();
                    stmts.extend(quote! {
                        if matches!(_bm, #(#val_lits)|*) {
                            #inner.init().unwrap_or_else(|_| #halt);
                        }
                    });
                } else {
                    stmts.extend(quote! {
                        #inner.init().unwrap_or_else(|_| #halt);
                    });
                }
                stmts.extend(generate_flexible_wrapping(dev, inst));
            }
        }
        count += 1;
    }

    let count_lit = Literal::usize_unsuffixed(count);
    stmts.extend(quote! {
        fstart_capabilities::driver_init_complete(#count_lit);
    });

    stmts
}

/// Collect devices that should only be initialised when the eGON
/// `boot_media` field matches.
///
/// Scans the full capability list for `LoadNextStage` and
/// `BootMedia(AutoDevice)` entries with **multiple** candidate devices.
/// For each candidate, returns the device name and its BROM boot-media
/// constant(s).  Single-device entries are not gated — the device must
/// init unconditionally.
pub(super) fn collect_boot_media_gated_devices(
    capabilities: &[fstart_types::Capability],
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    platform: Platform,
) -> Vec<(String, Vec<u8>)> {
    if platform != Platform::Armv7 {
        return Vec::new();
    }

    let mut gated: Vec<(String, Vec<u8>)> = Vec::new();

    for cap in capabilities {
        match cap {
            fstart_types::Capability::LoadNextStage {
                devices: load_devs, ..
            } if load_devs.len() > 1 => {
                for ld in load_devs.iter() {
                    let name = ld.name.as_str().to_string();
                    if gated.iter().any(|(n, _)| n == &name) {
                        continue;
                    }
                    let vals = boot_media_values_for_device(ld.name.as_str(), devices, instances);
                    gated.push((name, vals));
                }
            }
            fstart_types::Capability::BootMedia(BootMedium::AutoDevice {
                devices: candidates,
            }) if candidates.len() > 1 => {
                for c in candidates.iter() {
                    let name = c.name.as_str().to_string();
                    if gated.iter().any(|(n, _)| n == &name) {
                        continue;
                    }
                    let vals = boot_media_values_for_device(c.name.as_str(), devices, instances);
                    gated.push((name, vals));
                }
            }
            _ => {}
        }
    }

    gated
}

/// Generate code for the BootMedia capability.
pub(super) fn generate_boot_media(
    medium: &BootMedium,
    config: &BoardConfig,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    halt: &TokenStream,
) -> TokenStream {
    match medium {
        BootMedium::MemoryMapped { .. } => {
            quote! {
                let boot_media = unsafe {
                    MemoryMapped::from_raw_addr(FLASH_BASE, FLASH_SIZE as usize)
                };
            }
        }
        BootMedium::Device { name, offset, size } => {
            let dev_name = format_ident!("{}", name.as_str());
            let base_offset = hex_addr(*offset);
            let media_size = hex_addr(*size);
            quote! {
                let boot_media = BlockDeviceMedia::new(&#dev_name, #base_offset, #media_size as usize);
            }
        }
        BootMedium::AutoDevice {
            devices: candidates,
        } => generate_boot_media_auto_device(candidates, config, devices, instances, halt),
    }
}

/// Return the SRAM base address from the board config.
///
/// This is the first stage's `load_addr` — where the BROM loads the eGON
/// image. Used by sunxi helpers that read fields from the eGON header
/// at runtime (boot media detection, FFS total size, next-stage offset).
///
/// Returns 0 for monolithic or empty stage layouts (matches the H3 default).
fn egon_sram_base(config: &BoardConfig) -> u64 {
    match &config.stages {
        fstart_types::StageLayout::MultiStage(stages) => {
            stages.first().map(|s| s.load_addr).unwrap_or(0)
        }
        _ => 0,
    }
}

/// Check that the board uses the Allwinner eGON image format.
///
/// Capabilities that read eGON header fields at runtime (BootMedia AutoDevice,
/// LoadNextStage, anchor scan) must only be generated for eGON boards.
/// A non-eGON aarch64 board (e.g. qemu-aarch64) would fault trying to read
/// sunxi-specific SRAM locations.
fn require_egon_format(config: &BoardConfig, capability: &str) -> Result<(), TokenStream> {
    if config.soc_image_format != SocImageFormat::AllwinnerEgon {
        let msg = format!(
            "{capability} requires soc_image_format: AllwinnerEgon, \
             but board '{}' uses {:?}",
            config.name, config.soc_image_format
        );
        Err(quote! { compile_error!(#msg); })
    } else {
        Ok(())
    }
}

/// Generate code for `BootMedia(AutoDevice { ... })`.
///
/// Emits a block device dispatch enum that wraps each candidate device
/// type and implements `BlockDevice` via match dispatch. At runtime,
/// `fstart_soc_sunxi::boot_device()` selects the matching candidate.
fn generate_boot_media_auto_device(
    candidates: &[AutoBootDevice],
    config: &BoardConfig,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    halt: &TokenStream,
) -> TokenStream {
    if let Err(err) = require_egon_format(config, "BootMedia(AutoDevice)") {
        return err;
    }

    // SRAM base for eGON header access (bootblock load_addr).
    let sram_base = hex_addr(egon_sram_base(config));

    // Build the enum variants and BlockDevice match arms.
    let mut enum_variants = TokenStream::new();
    let mut read_arms = TokenStream::new();
    let mut write_arms = TokenStream::new();
    let mut erase_arms = TokenStream::new();
    let mut size_arms = TokenStream::new();
    let mut block_size_arms = TokenStream::new();
    let mut match_arms = TokenStream::new();

    for candidate in candidates {
        let dev_name_str = candidate.name.as_str();
        let dev_ident = format_ident!("{}", dev_name_str);
        // Convert to CamelCase for enum variant (e.g., "mmc0" -> "Mmc0").
        let variant_name = to_camel_case(dev_name_str);
        let variant_ident = format_ident!("{}", variant_name);
        let offset = hex_addr(candidate.offset);
        let size = hex_addr(candidate.size);

        // Get driver type for the enum variant.
        let Some((idx, _)) = devices
            .iter()
            .enumerate()
            .find(|(_, d)| d.name.as_str() == dev_name_str)
        else {
            let msg =
                format!("AutoDevice references device '{dev_name_str}' which is not declared");
            return quote! { compile_error!(#msg); };
        };
        let inst = &instances[idx];
        let type_name = format_ident!("{}", inst.meta().type_name);

        enum_variants.extend(quote! { #variant_ident(&'a #type_name), });
        read_arms.extend(quote! { Self::#variant_ident(d) => d.read(offset, buf), });
        write_arms.extend(quote! { Self::#variant_ident(d) => d.write(offset, buf), });
        erase_arms.extend(quote! { Self::#variant_ident(d) => d.erase(offset, size), });
        size_arms.extend(quote! { Self::#variant_ident(d) => d.size(), });
        block_size_arms.extend(quote! { Self::#variant_ident(d) => d.block_size(), });

        // Generate match arm for boot_media values.
        let bm_values = boot_media_values_for_device(dev_name_str, devices, instances);
        for val in bm_values {
            let val_lit = Literal::u8_unsuffixed(val);
            match_arms.extend(quote! {
                #val_lit => {
                    (_BootBlockDevice::#variant_ident(&#dev_ident), #offset, #size as usize)
                }
            });
        }
    }

    quote! {
        // Block device dispatch enum for runtime boot device selection.
        enum _BootBlockDevice<'a> {
            #enum_variants
        }

        impl<'a> BlockDevice for _BootBlockDevice<'a> {
            fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, fstart_services::ServiceError> {
                match self { #read_arms }
            }
            fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, fstart_services::ServiceError> {
                match self { #write_arms }
            }
            fn erase(&self, offset: u64, size: u64) -> Result<(), fstart_services::ServiceError> {
                match self { #erase_arms }
            }
            fn size(&self) -> u64 {
                match self { #size_arms }
            }
            fn block_size(&self) -> u32 {
                match self { #block_size_arms }
            }
        }

        let (_boot_block_dev, _boot_offset, _boot_size) = {
            let bm = fstart_soc_sunxi::boot_media_at(#sram_base);
            fstart_log::info!("boot media detect: {:#x}", bm);
            match bm {
                #match_arms
                _ => {
                    fstart_log::error!("FATAL: unknown boot medium: {:#x}", bm);
                    #halt;
                }
            }
        };
        let boot_media = BlockDeviceMedia::new(&_boot_block_dev, _boot_offset, _boot_size);
    }
}

/// Generate code for the SigVerify capability.
pub(super) fn generate_sig_verify(embed_anchor: bool) -> TokenStream {
    let anchor = anchor_expr(embed_anchor);
    quote! {
        fstart_capabilities::sig_verify(#anchor, &boot_media);
    }
}

/// Generate code for the FdtPrepare capability.
///
/// `uses_handoff` indicates whether the stage deserializes a
/// [`StageHandoff`] from a previous stage. If true, `_handoff` is
/// available and its `dram_size` field is preferred over the static
/// board config value (runtime-detected DRAM size from training).
pub(super) fn generate_fdt_prepare(
    config: &BoardConfig,
    platform: Platform,
    uses_handoff: bool,
    embed_anchor: bool,
) -> TokenStream {
    let Some(ref payload) = config.payload else {
        return quote! { fstart_capabilities::fdt_prepare_stub(); };
    };

    // Find the DRAM region from board config for memory node patching.
    let dram_info = find_dram_region(config);
    let dram_expr = generate_dram_expressions(dram_info, uses_handoff);

    match &payload.fdt {
        FdtSource::Platform => {
            let dtb_src_expr = if let Some(addr) = payload.src_dtb_addr {
                hex_addr(addr)
            } else {
                match platform {
                    Platform::Riscv64 => quote! { fstart_platform::boot_dtb_addr() },
                    Platform::Aarch64 => quote! { fstart_platform::boot_dtb_addr() },
                    // ARMv7: no DTB address saved by platform (board-specific).
                    // Use src_dtb_addr in the board RON instead.
                    Platform::Armv7 => quote! { 0u64 },
                }
            };
            let dtb_dst = hex_addr(payload.dtb_addr.unwrap_or(0));
            let bootargs = payload.bootargs.as_ref().map(|s| s.as_str()).unwrap_or("");
            let (dram_base_expr, dram_size_expr) = dram_expr;
            quote! {
                fstart_capabilities::fdt_prepare_platform(
                    #dtb_src_expr, #dtb_dst, #bootargs,
                    #dram_base_expr, #dram_size_expr,
                );
            }
        }
        FdtSource::Override(_dtb_file) => {
            // The DTB was assembled into the FFS as FileType::Fdt.
            // Load it from the FFS image into dtb_addr via boot_media,
            // then patch bootargs in-place using fdt_prepare_platform.
            let halt = halt_expr(platform);
            // All FFS-using stages now embed their own FSTART_ANCHOR.
            let anchor = anchor_expr(embed_anchor);
            let dtb_dst = hex_addr(payload.dtb_addr.unwrap_or(0));
            let bootargs = payload.bootargs.as_ref().map(|s| s.as_str()).unwrap_or("");
            let (dram_base_expr, dram_size_expr) = dram_expr;
            quote! {
                fstart_log::info!("loading DTB from FFS...");
                if !fstart_capabilities::load_ffs_file_by_type(
                    #anchor,
                    &boot_media,
                    fstart_types::ffs::FileType::Fdt,
                ) {
                    fstart_log::error!("FATAL: failed to load DTB from FFS");
                    #halt;
                }
                fstart_log::info!("DTB loaded to {:#x}", #dtb_dst as u64);
                // Patch bootargs and memory node in-place: src=dst since
                // DTB is already at dtb_addr.
                fstart_capabilities::fdt_prepare_platform(
                    #dtb_dst, #dtb_dst, #bootargs,
                    #dram_base_expr, #dram_size_expr,
                );
            }
        }
        _ => {
            quote! { fstart_capabilities::fdt_prepare_stub(); }
        }
    }
}

/// Find the DRAM region in the board config's memory map.
///
/// Returns `(base, size)` of the first `Ram` region whose name contains
/// "dram", or the largest `Ram` region if none match by name, or `None`
/// if no RAM regions exist.
fn find_dram_region(config: &BoardConfig) -> Option<(u64, u64)> {
    // Prefer a region explicitly named "dram".
    if let Some(r) = config
        .memory
        .regions
        .iter()
        .find(|r| r.kind == RegionKind::Ram && r.name.as_str().contains("dram"))
    {
        return Some((r.base, r.size));
    }
    // Fall back to the largest RAM region (excluding small SRAMs).
    config
        .memory
        .regions
        .iter()
        .filter(|r| r.kind == RegionKind::Ram)
        .max_by_key(|r| r.size)
        .map(|r| (r.base, r.size))
}

/// Generate token expressions for DRAM base and size.
///
/// If the stage receives a handoff, the DRAM size is taken from
/// `_handoff.dram_size` when non-zero, falling back to the board
/// config constant. The base address is always a compile-time constant
/// (DRAM doesn't move).
fn generate_dram_expressions(
    dram_info: Option<(u64, u64)>,
    uses_handoff: bool,
) -> (TokenStream, TokenStream) {
    match dram_info {
        Some((base, size)) => {
            let base_hex = hex_addr(base);
            let size_hex = hex_addr(size);
            let size_expr = if uses_handoff {
                // Prefer runtime handoff dram_size if available.
                quote! {
                    _handoff
                        .as_ref()
                        .filter(|h| h.dram_size > 0)
                        .map(|h| h.dram_size)
                        .unwrap_or(#size_hex)
                }
            } else {
                quote! { #size_hex }
            };
            (quote! { #base_hex }, size_expr)
        }
        None => (quote! { 0u64 }, quote! { 0u64 }),
    }
}

/// Generate code for the PayloadLoad capability.
pub(super) fn generate_payload_load(
    config: &BoardConfig,
    platform: Platform,
    embed_anchor: bool,
) -> TokenStream {
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

    // Load firmware blob from FFS FIRST — it goes to a high address
    // (e.g., 0x82000000) that doesn't overlap with the FFS image or
    // currently-executing code.
    if let Some(ref fw) = payload.firmware {
        let fw_kind_str = match fw.kind {
            FirmwareKind::OpenSbi => "SBI firmware",
            FirmwareKind::ArmTrustedFirmware => "ATF BL31",
        };
        let load_msg = format!("loading {fw_kind_str}...");
        let error_msg = format!("FATAL: failed to load {fw_kind_str}");
        let anchor_fw = anchor_expr(embed_anchor);
        stmts.extend(quote! {
            fstart_log::info!(#load_msg);
            if !fstart_capabilities::load_ffs_file_by_type(
                #anchor_fw,
                &boot_media,
                fstart_types::ffs::FileType::Firmware,
            ) {
                fstart_log::error!(#error_msg);
                #halt;
            }
        });
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

    // Load initramfs blob from FFS if configured.
    if payload.initramfs_file.is_some() {
        let initramfs_load_addr = hex_addr(payload.initramfs_load_addr.unwrap_or(0));
        let anchor3 = anchor_expr(embed_anchor);
        stmts.extend(quote! {
            fstart_log::info!("loading initramfs...");
            if !fstart_capabilities::load_ffs_file_by_type(
                #anchor3,
                &boot_media,
                fstart_types::ffs::FileType::Initramfs,
            ) {
                fstart_log::error!("FATAL: failed to load initramfs");
                #halt;
            }
        });

        // After loading, patch the FDT with initrd addresses.
        // The FFS loader tells us the actual size via the segment metadata,
        // but since we loaded it to a known address, we can compute the
        // size from the FFS file entry. For now, re-read the FFS to get
        // the initramfs size and patch the FDT.
        let dtb_for_initrd = hex_addr(payload.dtb_addr.unwrap_or(0));
        let anchor4 = anchor_expr(embed_anchor);
        stmts.extend(quote! {
            // Get initramfs size from FFS metadata for FDT patching
            let _initrd_size = fstart_capabilities::get_ffs_file_size(
                #anchor4,
                &boot_media,
                fstart_types::ffs::FileType::Initramfs,
            );
            if _initrd_size > 0 {
                fstart_capabilities::fdt_set_initrd_addresses(
                    #dtb_for_initrd,
                    #initramfs_load_addr,
                    _initrd_size,
                );
            }
        });
    }

    // Platform-specific boot protocol.
    let dtb_addr = hex_addr(payload.dtb_addr.unwrap_or(0));
    let kernel_addr = hex_addr(payload.kernel_load_addr.unwrap_or(0));

    match platform {
        Platform::Riscv64 => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            stmts.extend(quote! {
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
            });
        }
        Platform::Aarch64 => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            stmts.extend(quote! {
                let mut _bl33_ep: fstart_platform::EntryPointInfo =
                    unsafe { core::mem::zeroed() };
                let mut _bl33_node: fstart_platform::BlParamsNode =
                    unsafe { core::mem::zeroed() };
                let mut _bl_params: fstart_platform::BlParams =
                    unsafe { core::mem::zeroed() };
                fstart_platform::prepare_bl_params(
                    #kernel_addr,
                    #dtb_addr,
                    &mut _bl33_ep,
                    &mut _bl33_node,
                    &mut _bl_params,
                );
                fstart_log::info!("jumping to ATF BL31...");
                fstart_platform::boot_linux_atf(#fw_addr, &_bl_params);
            });
        }
        Platform::Armv7 => {
            // ARMv7 Linux boot protocol: no SBI/ATF, jump directly to kernel.
            // r0=0, r1=0xFFFFFFFF (DT-only), r2=DTB address.
            //
            // CNTFRQ is already programmed by the CCU driver's init() during
            // the ClockInit capability (see fstart-driver-sunxi-ccu), matching
            // U-Boot's board_init() timing.
            //
            // Pre-boot cleanup: disable/invalidate I-cache + branch predictor
            // for a clean handoff (matches U-Boot's cleanup_before_linux).
            stmts.extend(quote! {
                fstart_log::info!("booting Linux (ARMv7)...");
                fstart_log::info!("  kernel @ {:#x}", #kernel_addr as u64);
                fstart_log::info!("  dtb    @ {:#x}", #dtb_addr as u64);

                // Clean up CPU state: disable/invalidate I-cache, flush BP.
                fstart_platform::cleanup_before_linux();

                fstart_platform::boot_linux(#kernel_addr, #dtb_addr);
            });
        }
    }

    stmts
}

/// Generate code for the FIT runtime payload load sequence.
///
/// At runtime, the whole FIT (.itb) is stored in FFS. The firmware:
/// 1. Loads the FIT blob from FFS (FileType::FitImage) into memory
/// 2. Parses it with `fstart_fit::FitImage::parse()`
/// 3. Resolves the configuration (default or named)
/// 4. Copies each component (kernel, ramdisk) to its load address
/// 5. Boots via platform-specific protocol
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
    });

    // Load FIT blob from FFS. For memory-mapped flash, the FIT stays in
    // flash and we parse it in-place (zero copy). For block devices, we'd
    // need to load it into a buffer first (future enhancement).
    stmts.extend(quote! {
        fstart_log::info!("loading FIT image from FFS...");
        let _fit_slice = match fstart_capabilities::find_ffs_file_data(
            #anchor,
            &boot_media,
            fstart_types::ffs::FileType::FitImage,
        ) {
            Some(s) => s,
            None => {
                fstart_log::error!("FATAL: FIT image not found in FFS");
                #halt;
            }
        };

        fstart_log::info!("parsing FIT image ({} bytes)...", _fit_slice.len());
        let _fit = match fstart_fit::FitImage::parse(_fit_slice) {
            Ok(f) => f,
            Err(_) => {
                fstart_log::error!("FATAL: failed to parse FIT image");
                #halt;
            }
        };

        let _boot = match _fit.resolve_boot_images(#config_expr) {
            Ok(b) => b,
            Err(_) => {
                fstart_log::error!("FATAL: failed to resolve FIT configuration");
                #halt;
            }
        };
    });

    // Extract kernel data and copy to load address
    stmts.extend(quote! {
        let _kernel_data = match _boot.kernel.data() {
            Ok(d) => d,
            Err(_) => {
                fstart_log::error!("FATAL: failed to read kernel data from FIT");
                #halt;
            }
        };
        let _kernel_load = match _boot.kernel.load_addr() {
            Some(addr) => addr,
            None => {
                fstart_log::error!("FATAL: kernel has no load address in FIT");
                #halt;
            }
        };
        fstart_log::info!("FIT: loading kernel ({} bytes) to {}", _kernel_data.len(),
            fstart_log::Hex(_kernel_load));
        // SAFETY: load address points to writable RAM per board config.
        unsafe {
            core::ptr::copy_nonoverlapping(
                _kernel_data.as_ptr(),
                _kernel_load as *mut u8,
                _kernel_data.len(),
            );
        }
    });

    // Extract ramdisk if present
    stmts.extend(quote! {
        if let Some(ref _rd) = _boot.ramdisk {
            if let Ok(_rd_data) = _rd.data() {
                if let Some(_rd_load) = _rd.load_addr() {
                    fstart_log::info!("FIT: loading ramdisk ({} bytes) to {}",
                        _rd_data.len(), fstart_log::Hex(_rd_load));
                    // SAFETY: load address points to writable RAM.
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            _rd_data.as_ptr(),
                            _rd_load as *mut u8,
                            _rd_data.len(),
                        );
                    }
                }
            }
        }
    });

    // Load firmware blob from FFS (SBI/ATF is separate from FIT)
    if let Some(ref fw) = payload.firmware {
        let fw_kind_str = match fw.kind {
            FirmwareKind::OpenSbi => "SBI firmware",
            FirmwareKind::ArmTrustedFirmware => "ATF BL31",
        };
        let load_msg = format!("loading {fw_kind_str}...");
        let error_msg = format!("FATAL: failed to load {fw_kind_str}");
        let anchor2 = anchor_as_bytes_expr();
        stmts.extend(quote! {
            fstart_log::info!(#load_msg);
            if !fstart_capabilities::load_ffs_file_by_type(
                #anchor2,
                &boot_media,
                fstart_types::ffs::FileType::Firmware,
            ) {
                fstart_log::error!(#error_msg);
                #halt;
            }
        });
    }

    // Platform-specific boot protocol
    let dtb_addr = hex_addr(payload.dtb_addr.unwrap_or(0));
    let kernel_addr = quote! { _kernel_load };

    match platform {
        Platform::Riscv64 => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            stmts.extend(quote! {
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
            });
        }
        Platform::Aarch64 => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            stmts.extend(quote! {
                let mut _bl33_ep: fstart_platform::EntryPointInfo =
                    unsafe { core::mem::zeroed() };
                let mut _bl33_node: fstart_platform::BlParamsNode =
                    unsafe { core::mem::zeroed() };
                let mut _bl_params: fstart_platform::BlParams =
                    unsafe { core::mem::zeroed() };
                fstart_platform::prepare_bl_params(
                    #kernel_addr,
                    #dtb_addr,
                    &mut _bl33_ep,
                    &mut _bl33_node,
                    &mut _bl_params,
                );
                fstart_log::info!("jumping to ATF BL31...");
                fstart_platform::boot_linux_atf(#fw_addr, &_bl_params);
            });
        }
        Platform::Armv7 => {
            // ARMv7: no ATF/SBI — jump directly to kernel with pre-boot cleanup.
            stmts.extend(quote! {
                fstart_log::info!("booting Linux (ARMv7)...");
                fstart_platform::set_arch_timer_freq(24_000_000);
                fstart_platform::cleanup_before_linux();
                fstart_platform::boot_linux(#kernel_addr as u64, #dtb_addr);
            });
        }
    }

    stmts
}

/// Generate code to locate the FFS anchor block in boot media.
///
/// Used by non-first stages in a multi-stage build that don't have an
/// embedded `FSTART_ANCHOR` static.
///
/// - **Memory-mapped media**: scans the media slice for `FFS_MAGIC`.
/// - **Block device media (ARMv7)**: reads the anchor at the known offset
///   `ffs_total_size - ANCHOR_SIZE`, where `ffs_total_size` was patched
///   into the eGON header by the FFS assembler.
///
/// Emits a `scanned_anchor_data: [u8; ANCHOR_SIZE]` local variable
/// that subsequent FFS capability calls reference via
/// `&scanned_anchor_data[..]`.
pub(super) fn generate_anchor_scan(
    medium: &BootMedium,
    config: &BoardConfig,
    halt: &TokenStream,
) -> TokenStream {
    match medium {
        BootMedium::MemoryMapped { .. } => {
            // Memory-mapped: scan the full media slice for FFS_MAGIC.
            quote! {

                let scanned_anchor_data: [u8; fstart_types::ffs::ANCHOR_SIZE] = {
                    let media_slice = match boot_media.as_slice() {
                        Some(s) => s,
                        None => {
                            fstart_log::error!("FATAL: boot media does not support as_slice");
                            #halt;
                        }
                    };
                    let magic = &fstart_types::ffs::FFS_MAGIC;
                    let mut offset = 0usize;
                    let mut found = false;
                    while offset + magic.len() <= media_slice.len() {
                        if &media_slice[offset..offset + magic.len()] == magic {
                            found = true;
                            break;
                        }
                        offset += 8;
                    }
                    if !found || offset + fstart_types::ffs::ANCHOR_SIZE > media_slice.len() {
                        fstart_log::error!("FATAL: FFS anchor not found in boot media");
                        #halt;
                    }
                    let mut buf = [0u8; fstart_types::ffs::ANCHOR_SIZE];
                    buf.copy_from_slice(
                        &media_slice[offset..offset + fstart_types::ffs::ANCHOR_SIZE],
                    );
                    fstart_log::info!("FFS anchor found at offset {:#x} in boot media", offset as u64);
                    buf
                };
            }
        }
        BootMedium::Device { .. } | BootMedium::AutoDevice { .. } => {
            // Block device: read the anchor at ffs_total_size - ANCHOR_SIZE.
            // The FFS assembler patches ffs_total_size into the eGON header
            // so non-first stages can locate the anchor without scanning
            // the entire device.
            //
            // SAFETY invariant: this reads the eGON header from SRAM even
            // when running from the main stage in DRAM.  This is safe because
            // the header (offsets 0x00–0x60) is at the very start of SRAM
            // while the bootblock stack grows downward from the top, so the
            // header bytes survive across stages.
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
                    let mut buf = [0u8; fstart_types::ffs::ANCHOR_SIZE];
                    match boot_media.read_at(anchor_offset, &mut buf) {
                        Ok(_) => {}
                        Err(_) => {
                            fstart_log::error!("FATAL: failed to read FFS anchor from boot media");
                            #halt;
                        }
                    }
                    // Verify the magic bytes are present.
                    let magic = &fstart_types::ffs::FFS_MAGIC;
                    if buf[..magic.len()] != *magic {
                        fstart_log::error!("FATAL: FFS anchor magic mismatch at offset {:#x}", anchor_offset as u64);
                        #halt;
                    }
                    buf
                };
            }
        }
    }
}

/// Generate code for the LoadNextStage capability.
///
/// Reads the next stage's offset and size from the eGON header (ARMv7),
/// computes the absolute byte offset on the block device, reads that
/// many bytes directly into the next stage's load address, and jumps.
///
/// When multiple devices are specified, the boot device is auto-detected
/// via `fstart_soc_sunxi::boot_device()` and each match arm performs the
/// full read + handoff + jump sequence (the function never returns).
///
/// No intermediate DRAM buffer, no FFS parsing, no LZ4.
#[allow(clippy::too_many_arguments)]
pub(super) fn generate_load_next_stage(
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

    // LoadNextStage requires an Allwinner eGON-format bootblock to read
    // next-stage offset/size from the eGON header at the SRAM base.
    if let Err(err) = require_egon_format(config, "LoadNextStage") {
        return err;
    }

    // SRAM base for eGON header access (bootblock load_addr: H3=0x0, H5=0x10000).
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

    // Platform-specific jump call.
    let jump_call =
        quote! { fstart_platform::jump_to_with_handoff(#load_addr, #handoff_addr as usize); };

    // Build the handoff + jump sequence (shared by all arms).
    let handoff_and_jump = quote! {
        // Serialize handoff for the next stage.
        let _handoff_data = fstart_types::handoff::StageHandoff::new(#dram_size_expr);
        let handoff_buf_addr = #handoff_addr as *mut u8;
        let handoff_buf = unsafe {
            core::slice::from_raw_parts_mut(
                handoff_buf_addr,
                fstart_types::handoff::HANDOFF_MAX_SIZE,
            )
        };
        let handoff_len = fstart_capabilities::handoff::serialize(&_handoff_data, handoff_buf)
            .unwrap_or_else(|_| {
                fstart_log::error!("FATAL: handoff serialize failed");
                #halt
            });
        fstart_log::info!("handoff: {} bytes at {:#x}", handoff_len, #handoff_addr as u64);

        fstart_log::info!("jumping to stage '{}' at {:#x}", #next_stage, #load_addr as u64);
        #jump_call
    };

    if load_devices.len() == 1 {
        // Single device — no auto-detection needed.
        let ld = &load_devices[0];
        let dev_name_str = ld.name.as_str();
        let dev_ident = format_ident!("{}", dev_name_str);
        let base_off = hex_addr(ld.base_offset);

        return quote! {
            let ns_ffs_offset = fstart_soc_sunxi::next_stage_offset_at(#sram_base) as u64;
            let ns_size = fstart_soc_sunxi::next_stage_size_at(#sram_base) as usize;
            if ns_ffs_offset == 0 || ns_size == 0 {
                fstart_log::error!("FATAL: eGON header has zero next_stage_offset/size");
                #halt;
            }
            let dev_offset = #base_off + ns_ffs_offset;
            fstart_log::info!("loading stage '{}': offset={:#x}, size={:#x}, dest={:#x}",
                #next_stage, dev_offset, ns_size as u64, #load_addr as u64);
            {
                let dest_buf = unsafe {
                    core::slice::from_raw_parts_mut(#load_addr as *mut u8, ns_size)
                };
                #dev_ident.read(dev_offset, dest_buf).unwrap_or_else(|_| {
                    fstart_log::error!("FATAL: failed to read stage from {}", #dev_name_str);
                    #halt
                });
            }
            #handoff_and_jump
        };
    }

    // Multiple devices — auto-detect via eGON header boot_media field.
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
                    fstart_log::info!("loading stage '{}' from {}: offset={:#x}, size={:#x}, dest={:#x}",
                        #next_stage, #dev_name_str, dev_offset, ns_size as u64, #load_addr as u64);
                    {
                        let dest_buf = unsafe {
                            core::slice::from_raw_parts_mut(#load_addr as *mut u8, ns_size)
                        };
                        #dev_ident.read(dev_offset, dest_buf).unwrap_or_else(|_| {
                            fstart_log::error!("FATAL: failed to read stage from {}", #dev_name_str);
                            #halt
                        });
                    }
                    #handoff_and_jump
                }
            });
        }
    }

    quote! {
        let ns_ffs_offset = fstart_soc_sunxi::next_stage_offset_at(#sram_base) as u64;
        let ns_size = fstart_soc_sunxi::next_stage_size_at(#sram_base) as usize;
        if ns_ffs_offset == 0 || ns_size == 0 {
            fstart_log::error!("FATAL: eGON header has zero next_stage_offset/size");
            #halt;
        }
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

/// Generate code for the LateDriverInit capability.
///
/// Currently a stub — logs execution. Future: iterate devices and call
/// a `lockdown()` trait method for security hardening.
pub(super) fn generate_late_driver_init() -> TokenStream {
    quote! {
        fstart_capabilities::late_driver_init_complete(0);
    }
}

/// Generate code for the ReturnToFel capability.
///
/// Emits code that restores the saved BROM state and returns to FEL
/// mode. Only supported on armv7 (Allwinner sunxi).
pub(super) fn generate_return_to_fel(platform: Platform) -> TokenStream {
    if platform != Platform::Armv7 {
        let msg = format!("ReturnToFel is only supported on armv7, not '{platform}'");
        return quote! { compile_error!(#msg); };
    }
    quote! {
        fstart_log::info!("returning to FEL mode...");
        unsafe {
            let stash = &fstart_soc_sunxi::FEL_STASH;
            fstart_soc_sunxi::return_to_fel(stash.sp, stash.lr);
        }
    }
}

/// Generate code for the StageLoad capability.
pub(super) fn generate_stage_load(
    next_stage: &str,
    _platform: Platform,
    embed_anchor: bool,
) -> TokenStream {
    let anchor = anchor_expr(embed_anchor);
    let jump_fn: TokenStream = quote! { fstart_platform::jump_to };
    quote! {
        fstart_capabilities::stage_load(#next_stage, #anchor, &boot_media, #jump_fn);
    }
}

// ---------------------------------------------------------------------------
// Boot media value inference
// ---------------------------------------------------------------------------

/// Convert a device name to CamelCase for use as an enum variant name.
///
/// E.g., "mmc0" -> "Mmc0", "spi0" -> "Spi0", "spi-flash0" -> "SpiFlash0".
fn to_camel_case(name: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;
    for ch in name.chars() {
        if ch == '-' || ch == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(ch);
        }
    }
    result
}

/// Determine the eGON `boot_media` values that correspond to a device.
///
/// Maps a device name to the BROM boot_media constants based on the
/// device's driver type and configuration. Used by `LoadNextStage` and
/// `BootMedia(AutoDevice)` codegen to generate match arms for runtime
/// boot device auto-detection.
fn boot_media_values_for_device(
    dev_name: &str,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
) -> Vec<u8> {
    let Some(idx) = devices.iter().position(|d| d.name.as_str() == dev_name) else {
        panic!(
            "boot_media_values_for_device: device '{}' not found in board devices list",
            dev_name
        );
    };
    let inst = &instances[idx];
    let driver_name = inst.meta().name;

    match driver_name {
        "sunxi-mmc" => {
            // All sunxi MMC controllers share the same eGON boot_media
            // constants. Extract mmc_index via the SunxiMmcConfig helper.
            if let DriverInstance::SunxiMmc(cfg) = inst {
                match cfg.mmc_index() {
                    0 => vec![0x00, 0x10], // BOOT_MEDIA_MMC0, BOOT_MEDIA_MMC0_HIGH
                    2 => vec![0x02, 0x12], // BOOT_MEDIA_MMC2, BOOT_MEDIA_MMC2_HIGH
                    other => panic!(
                        "boot_media_values_for_device: unsupported mmc_index {} for device '{}'",
                        other, dev_name
                    ),
                }
            } else {
                unreachable!("driver name is sunxi-mmc but instance is not SunxiMmc")
            }
        }
        "sunxi-spi" => {
            vec![0x03] // BOOT_MEDIA_SPI
        }
        other => panic!(
            "boot_media_values_for_device: driver '{}' on device '{}' has no known boot_media mapping",
            other, dev_name
        ),
    }
}
