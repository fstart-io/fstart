//! Code generation for individual stage capabilities.
//!
//! Each capability (ConsoleInit, MemoryInit, DriverInit, BootMedia, SigVerify,
//! FdtPrepare, PayloadLoad, StageLoad) has a dedicated generator function that
//! emits the corresponding [`proc_macro2::TokenStream`] for inclusion in
//! `fstart_main()`.

mod acpi;
mod payload;
mod smbios;
mod stage_load;

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_device_registry::DriverInstance;
use fstart_types::memory::RegionKind;
use fstart_types::Platform;
use fstart_types::{
    AutoBootDevice, BoardConfig, BootMedium, BuildMode, DeviceConfig, FdtSource, SocImageFormat,
};

use super::flexible::{flexible_enum_for_device, generate_flexible_wrapping};
use super::registry::find_driver_meta;
use super::tokens::{anchor_expr, halt_expr, hex_addr};

// Re-export sub-module functions for use by stage_gen::mod.rs.
pub(super) use acpi::generate_acpi_prepare;
pub(super) use payload::generate_payload_load;
pub(super) use smbios::generate_smbios_prepare;
#[allow(unused_imports)]
pub(super) use stage_load::generate_anchor_scan;
pub(super) use stage_load::generate_load_next_stage;

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

/// Generate code for the PciInit capability.
///
/// Finds the referenced PCI root bus device, calls its `init()` method
/// (which enumerates the bus, sizes BARs, allocates resources, and
/// programs hardware), and logs the result.
pub(super) fn generate_pci_init(
    device_name: &str,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    halt: &TokenStream,
) -> TokenStream {
    let Some((idx, dev)) = devices
        .iter()
        .enumerate()
        .find(|(_, d)| d.name.as_str() == device_name)
    else {
        let msg = format!("PciInit references device '{device_name}' which is not declared");
        return quote! { compile_error!(#msg); };
    };

    let inst = &instances[idx];
    let drv_name = inst.meta().name;

    if !dev.services.iter().any(|s| s.as_str() == "PciRootBus") {
        let msg = format!(
            "PciInit requires PciRootBus service but device '{device_name}' does not provide it"
        );
        return quote! { compile_error!(#msg); };
    }

    let device = format_ident!("{}", device_name);

    quote! {
        #device.init().unwrap_or_else(|_| {
            fstart_log::error!("FATAL: PCI init failed ({})", #drv_name);
            #halt
        });
        fstart_log::info!("PCI init complete: {} ({})", #device_name, #drv_name);
    }
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
        if inst.is_acpi_only() {
            continue;
        }
        let name_str = dev.name.as_str();
        if already_inited.iter().any(|s| s == name_str) {
            continue;
        }

        // Check if this device is boot-media-gated.
        let gated_values = boot_media_gated
            .iter()
            .find(|(n, _)| n == name_str)
            .map(|(_, vals)| vals.as_slice());

        // Framebuffer devices use an ok_var bool so the UEFI payload
        // generator can conditionally expose GOP. All other devices halt
        // on init failure -- a broken mandatory driver is unrecoverable.
        let is_framebuffer = dev.services.iter().any(|s| s.as_str() == "Framebuffer");

        match mode {
            BuildMode::Rigid => {
                let name = format_ident!("{}", name_str);
                if is_framebuffer {
                    let ok_var = format_ident!("_{}_ok", name_str);
                    let driver_name_str = inst.meta().name;
                    if let Some(vals) = gated_values {
                        let val_lits: Vec<_> =
                            vals.iter().map(|v| Literal::u8_unsuffixed(*v)).collect();
                        stmts.extend(quote! {
                            let #ok_var = if matches!(_bm, #(#val_lits)|*) {
                                match #name.init() {
                                    Ok(()) => true,
                                    Err(_) => {
                                        fstart_log::error!("driver init failed: {}", #driver_name_str);
                                        false
                                    }
                                }
                            } else { false };
                        });
                    } else {
                        stmts.extend(quote! {
                            let #ok_var = match #name.init() {
                                Ok(()) => true,
                                Err(_) => {
                                    fstart_log::error!("driver init failed: {}", #driver_name_str);
                                    false
                                }
                            };
                        });
                    }
                } else if let Some(vals) = gated_values {
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
                if is_framebuffer {
                    let ok_var = format_ident!("_{}_ok", name_str);
                    let driver_name_str = inst.meta().name;
                    if let Some(vals) = gated_values {
                        let val_lits: Vec<_> =
                            vals.iter().map(|v| Literal::u8_unsuffixed(*v)).collect();
                        stmts.extend(quote! {
                            let #ok_var = if matches!(_bm, #(#val_lits)|*) {
                                match #inner.init() {
                                    Ok(()) => true,
                                    Err(_) => {
                                        fstart_log::error!("driver init failed: {}", #driver_name_str);
                                        false
                                    }
                                }
                            } else { false };
                        });
                    } else {
                        stmts.extend(quote! {
                            let #ok_var = match #inner.init() {
                                Ok(()) => true,
                                Err(_) => {
                                    fstart_log::error!("driver init failed: {}", #driver_name_str);
                                    false
                                }
                            };
                        });
                    }
                } else if let Some(vals) = gated_values {
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
/// constant(s).  Single-device entries are not gated -- the device must
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
/// This is the first stage's `load_addr` -- where the BROM loads the eGON
/// image. Used by sunxi helpers that read fields from the eGON header
/// at runtime (boot media detection, FFS total size, next-stage offset).
///
/// Returns 0 for monolithic or empty stage layouts (matches the H3 default).
pub(in crate::stage_gen::capabilities) fn egon_sram_base(config: &BoardConfig) -> u64 {
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
pub(in crate::stage_gen::capabilities) fn require_egon_format(
    config: &BoardConfig,
    capability: &str,
) -> Result<(), TokenStream> {
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

/// Generate code for the LateDriverInit capability.
///
/// Currently a stub -- logs execution. Future: iterate devices and call
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
        // SAFETY: save_boot_params has run during early boot, so the
        // FEL stash contains valid BROM state.
        unsafe { fstart_soc_sunxi::return_to_fel_from_stash() };
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
pub(in crate::stage_gen::capabilities) fn boot_media_values_for_device(
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
