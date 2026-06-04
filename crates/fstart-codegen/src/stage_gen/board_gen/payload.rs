//! Payload-load capability emission.

use proc_macro2::TokenStream;
use quote::quote;

use fstart_types::{Capability, FirmwareConfig, FirmwareKind, PayloadConfig, Platform};

use crate::stage_gen::tokens::hex_addr;

use super::boot_media::{anchor_bytes_stmt, match_boot_media};
use super::model::BoardEmitModel;
use super::payload_uefi::payload_load_uefi_body;

/// Emit the body of `Board::payload_load`.
///
/// Dispatches on the payload kind (mirroring the old
/// `capabilities::payload::generate_payload_load`):
///
/// - **UEFI** (`is_uefi_payload`) → build a `PlatformConfig` from
///   `self.<devices>` + static board data, optionally BL31 load for
///   aarch64 + ATF, call `fstart_crabefi::init_platform(_)` (→ !).
/// - **LinuxBoot** and **FitBuildtime** → load firmware (SBI/ATF/etc.)
///   plus kernel from FFS via a `match_boot_media` dispatch, then
///   call the platform boot protocol.
/// - **FitRuntime** → parse the embedded FIT via
///   `fstart_capabilities::fit::load_fit_components`, load optional
///   firmware, then the platform boot protocol.
/// - **No payload** → `fstart_capabilities::payload_load(..)` generic
///   stub, used by bare stages that have no specific boot target.
///
/// All FFS-touching variants go through [`match_boot_media`] so the
/// boot medium reconstruction stays a single code path.  The UEFI
/// path references `self._acpi_rsdp_addr` and `self._inited` for
/// runtime state instead of the old fstart_main-scoped
/// `_acpi_rsdp_addr` / `_<name>_ok` locals.
pub(super) fn payload_load_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    use crate::stage_gen::validation::{
        is_fit_image, is_fit_runtime, is_linux_boot, is_uefi_payload,
    };

    // If the stage doesn’t declare PayloadLoad, the executor never
    // dispatches this method.  Emit a dead-code stub rather than
    // referencing crates (fstart_crabefi, etc.) that may not be
    // linked into this stage.
    let has_payload_load = ctx
        .stage
        .capabilities
        .iter()
        .any(|c| matches!(c, Capability::PayloadLoad));
    if !has_payload_load {
        return quote! {
            todo!("board_gen::payload_load: stage does not declare PayloadLoad")
        };
    }

    if is_uefi_payload(ctx.config) {
        return payload_load_uefi_body(platform, ctx);
    }
    if is_linux_boot(ctx.config) || (is_fit_image(ctx.config) && !is_fit_runtime(ctx.config)) {
        return payload_load_linux_body(platform, ctx);
    }
    if is_fit_image(ctx.config) && is_fit_runtime(ctx.config) {
        return payload_load_fit_runtime_body(platform, ctx);
    }

    // Generic FFS payload: `fstart_capabilities::payload_load(anchor,
    // &bm, fstart_platform::jump_to)`.  Only reached for boards with
    // a raw `PayloadLoad` capability and no specific `PayloadKind` —
    // today none of the fixture boards hit this branch, but keeping
    // it means `board_gen` produces valid code for any future raw
    // payload stage.
    if !ctx.stage.uses_ffs {
        return quote! {
            todo!("board_gen::payload_load: generic payload requires an FFS-using stage")
        };
    }
    let anchor = anchor_bytes_stmt();
    let bm_usage = quote! {
        fstart_capabilities::payload_load(_anchor_bytes, &_bm, fstart_platform::jump_to);
    };
    let none_body = quote! {
        fstart_log::error!("payload_load: no boot media configured");
    };
    let match_body = match_boot_media(ctx, &bm_usage, "payload_load", &none_body);
    quote! {
        #anchor
        #match_body
        // `payload_load` is `-> !` — if the capability returns, halt.
        fstart_log::error!("payload_load: returned unexpectedly — halting");
        fstart_platform::halt()
    }
}

/// Emit the LinuxBoot + FIT-buildtime payload load body.
///
/// Steps (all inside a [`match_boot_media`] dispatch):
///
/// 1. Optional firmware (SBI/ATF) load via `load_ffs_file_by_type`
///    with `FileType::Firmware`.
/// 2. Kernel load via `load_ffs_file_by_type` with `FileType::Payload`.
///
/// Then, outside the boot-media block, the platform boot protocol
/// (via [`platform_boot_protocol_stmts`]).
fn payload_load_linux_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !ctx.stage.uses_ffs {
        return quote! {
            todo!("board_gen::payload_load (LinuxBoot): requires an FFS-using stage")
        };
    }
    let payload = ctx
        .config
        .payload
        .as_ref()
        .expect("LinuxBoot implies payload");
    let anchor = anchor_bytes_stmt();

    // `bm_usage` is the body of each boot-media arm: firmware load
    // (if configured) followed by kernel load.
    let firmware_load_tokens = match payload.firmware.as_ref() {
        Some(fw) => firmware_load_inside_match(fw),
        None => TokenStream::new(),
    };
    let bm_usage = quote! {
        #firmware_load_tokens
        fstart_log::info!("loading kernel...");
        if !fstart_capabilities::load_ffs_file_by_type(
            _anchor_bytes,
            &_bm,
            fstart_types::ffs::FileType::Payload,
        ) {
            fstart_log::error!("FATAL: failed to load kernel");
            fstart_platform::halt();
        }
    };
    let none_body = quote! {
        fstart_log::error!("payload_load (LinuxBoot): no boot media configured");
    };
    let match_body = match_boot_media(ctx, &bm_usage, "payload_load", &none_body);

    let kernel_addr = hex_addr(payload.kernel_load_addr.unwrap_or(0));
    let platform_boot = platform_boot_protocol_stmts(platform, &kernel_addr, payload);

    quote! {
        fstart_log::info!("capability: PayloadLoad (LinuxBoot)");
        #anchor
        #match_body
        #platform_boot
    }
}

/// Emit the FIT-runtime payload load body.
///
/// Calls `fstart_capabilities::fit::load_fit_components` against the
/// current boot medium, captures `_kernel_load`, optionally loads
/// firmware, then runs the platform boot protocol with
/// `#kernel_addr = _kernel_load`.
fn payload_load_fit_runtime_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !ctx.stage.uses_ffs {
        return quote! {
            todo!("board_gen::payload_load (FIT runtime): requires an FFS-using stage")
        };
    }
    let payload = ctx
        .config
        .payload
        .as_ref()
        .expect("FIT runtime implies payload");
    let anchor = anchor_bytes_stmt();

    let config_expr = match &payload.fit_config {
        Some(name) => {
            let name_str = name.as_str();
            quote! { Some(#name_str) }
        }
        None => quote! { None },
    };

    let firmware_load_tokens = match payload.firmware.as_ref() {
        Some(fw) => firmware_load_inside_match(fw),
        None => TokenStream::new(),
    };

    // FIT runtime's `bm_usage` has to return a value (the kernel
    // address) from the closure.  Rather than fighting the
    // closure pattern we emit the whole sequence inline.
    let bm_usage = quote! {
        let _fit_boot = fstart_capabilities::fit::load_fit_components_with_scratch(
            _anchor_bytes,
            &_bm,
            #config_expr,
            _scratch.as_mut(),
        )
        .unwrap_or_else(|e| {
            fstart_log::error!(
                "FATAL: FIT boot failed: {}",
                fstart_capabilities::fit::error_str(&e),
            );
            fstart_platform::halt();
        });
        _kernel_load = _fit_boot.kernel_addr;
        #firmware_load_tokens
    };
    let none_body = quote! {
        fstart_log::error!("payload_load (FIT runtime): no boot media configured");
    };
    let match_body = match_boot_media(ctx, &bm_usage, "payload_load", &none_body);

    let kernel_addr = quote! { _kernel_load };
    let platform_boot = platform_boot_protocol_stmts(platform, &kernel_addr, payload);

    quote! {
        fstart_log::info!("capability: PayloadLoad (FIT runtime)");
        #anchor
        let mut _kernel_load: u64 = 0;
        #match_body
        #platform_boot
        fstart_platform::halt()
    }
}

/// Emit the firmware-load fragment used inside a `match_boot_media`
/// arm (LinuxBoot or FIT runtime).  Assumes `_anchor_bytes` and `_bm`
/// are in scope.
fn firmware_load_inside_match(firmware: &FirmwareConfig) -> TokenStream {
    let fw_kind_str = match firmware.kind {
        FirmwareKind::OpenSbi => "SBI firmware",
        FirmwareKind::ArmTrustedFirmware => "ATF BL31",
    };
    let load_msg = format!("loading {fw_kind_str}...");
    let error_msg = format!("FATAL: failed to load {fw_kind_str}");
    quote! {
        fstart_log::info!(#load_msg);
        if !fstart_capabilities::load_ffs_file_by_type(
            _anchor_bytes,
            &_bm,
            fstart_types::ffs::FileType::Firmware,
        ) {
            fstart_log::error!(#error_msg);
            fstart_platform::halt();
        }
    }
}

/// Emit the platform-specific boot protocol sequence.
///
/// Matches the old `capabilities::payload::generate_platform_boot_protocol`
/// but reads per-board state from `&self` where needed:
///
/// - `_acpi_rsdp_addr` → `self._acpi_rsdp_addr` (x86_64 only; Linux
///   `boot_linux` wants the RSDP).
///
/// All other values (kernel addr, dtb addr, firmware addr, bootargs)
/// come from the payload literal — they're const, not runtime state.
pub(super) fn platform_boot_protocol_stmts(
    platform: Platform,
    kernel_addr: &TokenStream,
    payload: &PayloadConfig,
) -> TokenStream {
    let dtb_addr = hex_addr(payload.dtb_addr.unwrap_or(0));
    let fw_addr = hex_addr(payload.firmware.as_ref().map(|f| f.load_addr).unwrap_or(0));
    let bootargs_str = payload.bootargs.as_deref().unwrap_or("");
    let print_x86_mtrrs = payload.print_x86_mtrrs;

    // Platform-specific preamble: RISC-V needs hart_id, x86 needs
    // e820.  All other fields come from the board adapter's `self`.
    let hart_id_expr = match platform {
        Platform::Riscv64 => quote! { fstart_platform::boot_hart_id() },
        _ => quote! { 0u64 },
    };
    let e820_setup = match platform {
        Platform::X86_64 => quote! {
            unsafe extern "C" {
                static _text_start: u8;
                static _writable_end: u8;
            }
            unsafe {
                let _stage_start = &_text_start as *const u8 as u64;
                let _stage_end = &_writable_end as *const u8 as u64;
                // The generated heap backing store is a static in this range,
                // so reserving the linked stage image also reserves all bump
                // allocator contents, including ACPI/SMBIOS tables leaked for
                // OS consumption.
                fstart_services::memory_detect::e820_state_mut()
                    .reserve_range(_stage_start, _stage_end.saturating_sub(_stage_start));
            }
            let _e820_state = unsafe { fstart_services::memory_detect::e820_state() };
        },
        _ => quote! {},
    };
    let e820_expr = match platform {
        Platform::X86_64 => quote! { _e820_state.entries() },
        _ => quote! { &[] },
    };
    let zero_page_addr = match platform {
        Platform::X86_64 => quote! { 0x90000u64 },
        _ => quote! { 0u64 },
    };
    let rsdp_expr = match platform {
        Platform::X86_64 => quote! { self._acpi_rsdp_addr },
        _ => quote! { 0u64 },
    };

    quote! {
        fstart_log::info!("booting Linux...");
        fstart_log::info!("  kernel @ {:#x}", #kernel_addr as u64);
        #e820_setup
        let _boot_params = fstart_services::boot::BootLinuxParams {
            kernel_addr: #kernel_addr as u64,
            dtb_addr: #dtb_addr,
            fw_addr: #fw_addr,
            rsdp_addr: #rsdp_expr,
            bootargs: #bootargs_str,
            e820_entries: #e820_expr,
            zero_page_addr: #zero_page_addr,
            hart_id: #hart_id_expr,
            print_x86_mtrrs: #print_x86_mtrrs,
        };
        fstart_platform::boot_linux(&_boot_params);
    }
}
