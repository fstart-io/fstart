//! Payload-load capability emission.

use proc_macro2::TokenStream;
use quote::quote;

use fstart_types::{Capability, FirmwareConfig, FirmwareKind, Platform};

use crate::stage_gen::tokens::hex_addr;

use super::boot_media::{anchor_bytes_stmt, match_boot_media};
use super::model::BoardEmitModel;
use super::payload_uefi::payload_load_uefi_body;
use super::platform::linux::platform_boot_protocol_stmts;

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
