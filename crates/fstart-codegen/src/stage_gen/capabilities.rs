//! Code generation for individual stage capabilities.
//!
//! Each capability (ConsoleInit, MemoryInit, DriverInit, BootMedia, SigVerify,
//! FdtPrepare, PayloadLoad, StageLoad) has a dedicated generator function that
//! emits the corresponding [`proc_macro2::TokenStream`] for inclusion in
//! `fstart_main()`.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_drivers::DriverInstance;
use fstart_types::{BoardConfig, BootMedium, BuildMode, DeviceConfig, FdtSource, FirmwareKind};

use super::flexible::{flexible_enum_for_device, generate_flexible_wrapping};
use super::registry::find_driver_meta;
use super::tokens::{anchor_as_bytes_expr, halt_expr, hex_addr};
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

/// Generate code for the MemoryInit capability.
pub(super) fn generate_memory_init() -> TokenStream {
    quote! { fstart_capabilities::memory_init(); }
}

/// Generate code for the DriverInit capability.
pub(super) fn generate_driver_init(
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
    sorted_indices: &[usize],
    already_inited: &[String],
    halt: &TokenStream,
    mode: BuildMode,
) -> TokenStream {
    let mut stmts = TokenStream::new();
    let mut count = 0usize;

    for &idx in sorted_indices {
        let dev = &devices[idx];
        let inst = &instances[idx];
        let name_str = dev.name.as_str();
        if already_inited.iter().any(|s| s == name_str) {
            continue;
        }

        match mode {
            BuildMode::Rigid => {
                let name = format_ident!("{}", name_str);
                stmts.extend(quote! {
                    #name.init().unwrap_or_else(|_| #halt);
                });
            }
            BuildMode::Flexible => {
                let inner = if flexible_enum_for_device(dev, inst).is_some() {
                    format_ident!("_{}_inner", name_str)
                } else {
                    format_ident!("{}", name_str)
                };
                stmts.extend(quote! {
                    #inner.init().unwrap_or_else(|_| #halt);
                });
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

/// Generate code for the BootMedia capability.
pub(super) fn generate_boot_media(medium: &BootMedium) -> TokenStream {
    match medium {
        BootMedium::MemoryMapped { .. } => {
            quote! {
                let boot_media = unsafe {
                    MemoryMapped::from_raw_addr(FLASH_BASE, FLASH_SIZE as usize)
                };
            }
        }
        BootMedium::Device { name } => {
            let dev_name = name.as_str();
            // TODO: Device-backed boot media size should come from the
            // driver config (e.g., a `size` field on a block device config).
            // For now, emit a compile_error since no boards use this path.
            let msg =
                format!("BootMedia(Device) for '{dev_name}' not yet supported with typed configs");
            quote! { compile_error!(#msg); }
        }
    }
}

/// Generate code for the SigVerify capability.
pub(super) fn generate_sig_verify() -> TokenStream {
    let anchor = anchor_as_bytes_expr();
    quote! {
        fstart_capabilities::sig_verify(#anchor, &boot_media);
    }
}

/// Generate code for the FdtPrepare capability.
pub(super) fn generate_fdt_prepare(config: &BoardConfig, platform: &str) -> TokenStream {
    let Some(ref payload) = config.payload else {
        return quote! { fstart_capabilities::fdt_prepare_stub(); };
    };

    match &payload.fdt {
        FdtSource::Platform => {
            let dtb_src_expr = if let Some(addr) = payload.src_dtb_addr {
                hex_addr(addr)
            } else {
                match platform {
                    "riscv64" => quote! { fstart_platform_riscv64::boot_dtb_addr() },
                    "aarch64" => quote! { fstart_platform_aarch64::boot_dtb_addr() },
                    "armv7" => quote! { fstart_platform_armv7::boot_dtb_addr() as u64 },
                    _ => quote! { 0 },
                }
            };
            let dtb_dst = hex_addr(payload.dtb_addr.unwrap_or(0));
            let bootargs = payload.bootargs.as_ref().map(|s| s.as_str()).unwrap_or("");
            quote! {
                fstart_capabilities::fdt_prepare_platform(#dtb_src_expr, #dtb_dst, #bootargs);
            }
        }
        _ => {
            quote! { fstart_capabilities::fdt_prepare_stub(); }
        }
    }
}

/// Generate code for the PayloadLoad capability.
pub(super) fn generate_payload_load(config: &BoardConfig, platform: &str) -> TokenStream {
    if is_linux_boot(config) {
        return generate_payload_load_linux(config, platform);
    }

    if is_fit_image(config) {
        if is_fit_runtime(config) {
            return generate_payload_load_fit_runtime(config, platform);
        } else {
            // Buildtime FIT: xtask extracts components from FIT and embeds
            // them as separate FFS entries. Runtime code loads them the same
            // way as LinuxBoot (individual kernel/ramdisk blobs from FFS).
            return generate_payload_load_linux(config, platform);
        }
    }

    let anchor = anchor_as_bytes_expr();
    let jump_fn: TokenStream = match platform {
        "riscv64" => quote! { fstart_platform_riscv64::jump_to },
        "aarch64" => quote! { fstart_platform_aarch64::jump_to },
        "armv7" => quote! { fstart_platform_armv7::jump_to },
        _ => quote! { fstart_platform_riscv64::jump_to },
    };
    quote! {
        fstart_capabilities::payload_load(#anchor, &boot_media, #jump_fn);
    }
}

/// Generate the Linux boot payload sequence for a specific platform.
fn generate_payload_load_linux(config: &BoardConfig, platform: &str) -> TokenStream {
    let payload = config.payload.as_ref().unwrap(); // caller verified is_linux_boot
    let halt = halt_expr(platform);
    let anchor = anchor_as_bytes_expr();

    let mut stmts = TokenStream::new();

    stmts.extend(quote! {
        fstart_log::info!("capability: PayloadLoad (LinuxBoot)");
        fstart_log::info!("loading kernel...");
    });

    stmts.extend(quote! {
        if !fstart_capabilities::load_ffs_file_by_type(
            #anchor,
            &boot_media,
            fstart_types::ffs::FileType::Payload,
        ) {
            fstart_log::error!("FATAL: failed to load kernel");
            #halt;
        }
    });

    // Load firmware blob from FFS
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
    let kernel_addr = hex_addr(payload.kernel_load_addr.unwrap_or(0));

    match platform {
        "riscv64" => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            stmts.extend(quote! {
                let _fw_info = fstart_platform_riscv64::FwDynamicInfo::new(
                    #kernel_addr,
                    fstart_platform_riscv64::boot_hart_id(),
                );
                fstart_log::info!("jumping to SBI firmware...");
                fstart_platform_riscv64::boot_linux_sbi(
                    #fw_addr,
                    fstart_platform_riscv64::boot_hart_id(),
                    #dtb_addr,
                    &_fw_info,
                );
            });
        }
        "aarch64" => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            stmts.extend(quote! {
                let mut _bl33_ep: fstart_platform_aarch64::EntryPointInfo =
                    unsafe { core::mem::zeroed() };
                let mut _bl33_node: fstart_platform_aarch64::BlParamsNode =
                    unsafe { core::mem::zeroed() };
                let mut _bl_params: fstart_platform_aarch64::BlParams =
                    unsafe { core::mem::zeroed() };
                fstart_platform_aarch64::prepare_bl_params(
                    #kernel_addr,
                    #dtb_addr,
                    &mut _bl33_ep,
                    &mut _bl33_node,
                    &mut _bl_params,
                );
                fstart_log::info!("jumping to ATF BL31...");
                fstart_platform_aarch64::boot_linux_atf(#fw_addr, &_bl_params);
            });
        }
        "armv7" => {
            // 32-bit ARM: no ATF/SBI needed — jump directly to kernel.
            // ARM Linux boot protocol: r0=0, r1=~0 (DT-only), r2=DTB.
            stmts.extend(quote! {
                fstart_log::info!("jumping to Linux kernel (direct)...");
                fstart_platform_armv7::boot_linux_direct(
                    #kernel_addr as u32,
                    #dtb_addr as u32,
                );
            });
        }
        _ => {
            let msg = format!("LinuxBoot not supported on platform '{platform}'");
            stmts.extend(quote! { compile_error!(#msg); });
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
fn generate_payload_load_fit_runtime(config: &BoardConfig, platform: &str) -> TokenStream {
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
        "riscv64" => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            stmts.extend(quote! {
                let _fw_info = fstart_platform_riscv64::FwDynamicInfo::new(
                    #kernel_addr,
                    fstart_platform_riscv64::boot_hart_id(),
                );
                fstart_log::info!("jumping to SBI firmware...");
                fstart_platform_riscv64::boot_linux_sbi(
                    #fw_addr,
                    fstart_platform_riscv64::boot_hart_id(),
                    #dtb_addr,
                    &_fw_info,
                );
            });
        }
        "aarch64" => {
            let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
            stmts.extend(quote! {
                let mut _bl33_ep: fstart_platform_aarch64::EntryPointInfo =
                    unsafe { core::mem::zeroed() };
                let mut _bl33_node: fstart_platform_aarch64::BlParamsNode =
                    unsafe { core::mem::zeroed() };
                let mut _bl_params: fstart_platform_aarch64::BlParams =
                    unsafe { core::mem::zeroed() };
                fstart_platform_aarch64::prepare_bl_params(
                    #kernel_addr,
                    #dtb_addr,
                    &mut _bl33_ep,
                    &mut _bl33_node,
                    &mut _bl_params,
                );
                fstart_log::info!("jumping to ATF BL31...");
                fstart_platform_aarch64::boot_linux_atf(#fw_addr, &_bl_params);
            });
        }
        "armv7" => {
            // 32-bit ARM: no ATF/SBI needed — jump directly to kernel.
            stmts.extend(quote! {
                fstart_log::info!("jumping to Linux kernel (direct)...");
                fstart_platform_armv7::boot_linux_direct(
                    #kernel_addr as u32,
                    #dtb_addr as u32,
                );
            });
        }
        _ => {
            let msg = format!("FIT boot not supported on platform '{platform}'");
            stmts.extend(quote! { compile_error!(#msg); });
        }
    }

    stmts
}

/// Generate code for the StageLoad capability.
pub(super) fn generate_stage_load(next_stage: &str, platform: &str) -> TokenStream {
    let anchor = anchor_as_bytes_expr();
    let jump_fn: TokenStream = match platform {
        "riscv64" => quote! { fstart_platform_riscv64::jump_to },
        "aarch64" => quote! { fstart_platform_aarch64::jump_to },
        "armv7" => quote! { fstart_platform_armv7::jump_to },
        _ => quote! { fstart_platform_riscv64::jump_to },
    };
    quote! {
        fstart_capabilities::stage_load(#next_stage, #anchor, &boot_media, #jump_fn);
    }
}
