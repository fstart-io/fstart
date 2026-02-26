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
use super::validation::is_linux_boot;

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

    let anchor = anchor_as_bytes_expr();
    let jump_fn: TokenStream = match platform {
        "riscv64" => quote! { fstart_platform_riscv64::jump_to },
        "aarch64" => quote! { fstart_platform_aarch64::jump_to },
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
        _ => {
            let msg = format!("LinuxBoot not supported on platform '{platform}'");
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
        _ => quote! { fstart_platform_riscv64::jump_to },
    };
    quote! {
        fstart_capabilities::stage_load(#next_stage, #anchor, &boot_media, #jump_fn);
    }
}
