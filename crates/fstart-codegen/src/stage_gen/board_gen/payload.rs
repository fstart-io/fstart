//! Payload-load capability emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::memory::RegionKind;
use fstart_types::{
    Capability, FirmwareConfig, FirmwareKind, PayloadConfig, Platform, StageLayout,
};

use crate::stage_gen::tokens::hex_addr;

use super::boot_media::{anchor_bytes_stmt, match_boot_media};
use super::model::BoardEmitModel;

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
        let _fit_boot = fstart_capabilities::fit::load_fit_components(
            _anchor_bytes,
            &_bm,
            #config_expr,
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
fn platform_boot_protocol_stmts(
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

/// Emit the UEFI (CrabEFI) payload load body.
///
/// Mirrors `capabilities::payload::generate_payload_load_uefi` but
/// routes device references through `self.<field>` and reads the
/// RSDP + framebuffer-init flag from `self._acpi_rsdp_addr` /
/// `self._inited` respectively.
///
/// Sections (same order as the old generator):
///
/// 1. Optional BL31 load (aarch64 + ATF firmware) via `match_boot_media`.
/// 2. Timer / reset / RNG setup (per-platform).
/// 3. Console adapter (`self.<console>.as_ref()`).
/// 4. FDT blob probe (aarch64 / riscv64 from `boot_dtb_addr`).
/// 5. FDT reservation (non-x86).
/// 6. Memory map build (x86 from e820, others from static RAM + FDT
///    reservation).
/// 7. Framebuffer config gated on `self._inited.contains(fb_id)`.
/// 8. `fstart_crabefi::PlatformConfig { ... }` literal.
/// 9. `fstart_crabefi::init_platform(_crabefi_config)` (→ !).
fn payload_load_uefi_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    let config = ctx.config;
    let payload = config.payload.as_ref().expect("UEFI implies payload");

    // Collect static memory map entries (ROM, Reserved) from board config.
    let mut static_mem_entries = TokenStream::new();
    for region in &config.memory.regions {
        let base = hex_addr(region.base);
        let size = hex_addr(region.size);
        match region.kind {
            RegionKind::Rom => static_mem_entries.extend(quote! {
                fstart_crabefi::MemoryRegion {
                    base: #base, size: #size,
                    region_type: fstart_crabefi::MemoryType::RuntimeServicesCode,
                },
            }),
            RegionKind::Reserved => static_mem_entries.extend(quote! {
                fstart_crabefi::MemoryRegion {
                    base: #base, size: #size,
                    region_type: fstart_crabefi::MemoryType::Reserved,
                },
            }),
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
            let last = stages.last().expect("multi-stage has at least one stage");
            (
                last.data_addr.unwrap_or(last.load_addr),
                last.stack_size as u64,
            )
        }
    };
    let fw_data_addr_lit = hex_addr(fw_data_addr);
    let fw_stack_size_lit = hex_addr(fw_stack_size);

    // Console device for DebugOutput adapter: find the first enabled
    // Console provider.  `_BoardDevices` always stores it in the
    // `self.<name>` field.
    let console_device = ctx.runtime_devices.providers(Service::Console).next();
    let (console_setup, debug_output_field) = match console_device {
        Some(device) => {
            let field = format_ident!("{}", device.name);
            (
                quote! {
                    let _console_ref = self.#field
                        .as_ref()
                        .unwrap_or_else(|| fstart_platform::halt());
                    let mut _crabefi_console = fstart_crabefi::ConsoleAdapter::new(_console_ref);
                    let mut _crabefi_console_input = fstart_crabefi::ConsoleAdapter::new(_console_ref);
                },
                quote! {
                    debug_output: Some(&mut _crabefi_console),
                    console_input: Some(&mut _crabefi_console_input),
                },
            )
        }
        None => (
            quote! {},
            quote! {
                debug_output: None,
                console_input: None,
            },
        ),
    };

    // PCI device for ECAM base.
    let pci_device = ctx.runtime_devices.providers(Service::PciRootBus).next();
    let ecam_base_field = match pci_device {
        Some(device) => {
            let field = format_ident!("{}", device.name);
            quote! {
                ecam_base: Some(
                    self.#field
                        .as_ref()
                        .unwrap_or_else(|| fstart_platform::halt())
                        .ecam_base(),
                ),
            }
        }
        None => quote! { ecam_base: None, },
    };

    // Framebuffer device for GOP — gated on the init mask via
    // `self._inited.contains(fb_id)` rather than the old fstart_main
    // `_fb_ok: bool` local.
    let fb_device = ctx.runtime_devices.providers(Service::Framebuffer).next();
    let (fb_setup, framebuffer_field) = match fb_device {
        Some(device) => {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            let setup = quote! {
                let _fb_config = if self._inited.contains(#id_lit) {
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
            };
            (setup, quote! { framebuffer: _fb_config, })
        }
        None => (quote! {}, quote! { framebuffer: None, }),
    };

    // FDT sourcing — mirrors `dtb_src_expr`-ish logic.
    let fdt_addr_expr = if let Some(addr) = payload.src_dtb_addr {
        hex_addr(addr)
    } else {
        match platform {
            Platform::Aarch64 | Platform::Riscv64 => {
                quote! { fstart_platform::boot_dtb_addr() }
            }
            Platform::Armv7 | Platform::X86_64 => quote! { 0u64 },
        }
    };
    let fdt_setup = match platform {
        Platform::Aarch64 | Platform::Riscv64 => quote! {
            let _fdt_addr = #fdt_addr_expr;
            // SAFETY: platform guarantees _fdt_addr points to a valid
            // FDT blob saved from the boot register on entry.
            let _fdt_blob: Option<&[u8]> =
                unsafe { fstart_capabilities::fdt_blob_from_addr(_fdt_addr) };
        },
        Platform::Armv7 | Platform::X86_64 => quote! {
            let _fdt_addr: u64 = 0;
            let _fdt_blob: Option<&[u8]> = None;
        },
    };
    let fdt_field = quote! { fdt: _fdt_blob, };

    // BL31 load — aarch64 + ATF only.
    let bl31_boot = if let Some(fw) = payload.firmware.as_ref() {
        if platform == Platform::Aarch64 && fw.kind == FirmwareKind::ArmTrustedFirmware {
            let fw_load_addr = hex_addr(fw.load_addr);
            let anchor = anchor_bytes_stmt();
            let bm_usage = quote! {
                if !fstart_capabilities::load_ffs_file_by_type(
                    _anchor_bytes,
                    &_bm,
                    fstart_types::ffs::FileType::Firmware,
                ) {
                    fstart_log::error!("FATAL: failed to load BL31 firmware");
                    fstart_platform::halt();
                }
            };
            let none_body = quote! {
                fstart_log::error!("payload_load (UEFI): no boot media for BL31");
                fstart_platform::halt();
            };
            let match_body = match_boot_media(ctx, &bm_usage, "payload_load", &none_body);
            quote! {
                fstart_log::info!("loading TF-A BL31 firmware...");
                #anchor
                #match_body
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

    // Platform-specific timer, reset, RNG.
    let (timer_setup, timer_field, reset_setup, reset_field, rng_setup, rng_field) = match platform
    {
        Platform::X86_64 => (
            quote! {
                let _crabefi_timer = fstart_crabefi::TscTimer::new();
                fstart_log::info!("TSC timer initialized");
            },
            quote! { timer: &_crabefi_timer, },
            quote! { let _crabefi_reset = fstart_crabefi::X86Reset; },
            quote! { reset: &_crabefi_reset, },
            quote! { let _crabefi_rng = fstart_crabefi::X86Rng::new(); },
            quote! { rng: Some(&_crabefi_rng), },
        ),
        _ => (
            quote! { let _crabefi_timer = fstart_crabefi::ArmGenericTimer::new(); },
            quote! { timer: &_crabefi_timer, },
            quote! { let _crabefi_reset = fstart_crabefi::PsciReset; },
            quote! { reset: &_crabefi_reset, },
            quote! {},
            quote! { rng: None, },
        ),
    };

    // ACPI RSDP: x86 reads from self._acpi_rsdp_addr (populated by
    // AcpiLoad).  On non-x86 the RSDP field stays None.
    let acpi_rsdp_field = if platform == Platform::X86_64 {
        quote! { acpi_rsdp: Some(self._acpi_rsdp_addr), }
    } else {
        quote! { acpi_rsdp: None, }
    };

    // Runtime region: x86 only.
    let runtime_region_field = if platform == Platform::X86_64 {
        quote! { Some(fstart_crabefi::compute_runtime_region()), }
    } else {
        quote! { None, }
    };

    // Memory map — x86 from e820, others from static RAM + FDT.
    let memory_map_setup = if platform == Platform::X86_64 {
        quote! {
            let _e820_state = unsafe { fstart_services::memory_detect::e820_state() };
            let _rom_entries: &[fstart_crabefi::MemoryRegion] = &[
                #static_mem_entries
            ];
            let mut _crabefi_mem_buf: [fstart_crabefi::MemoryRegion; 64] = [
                fstart_crabefi::MemoryRegion {
                    base: 0, size: 0,
                    region_type: fstart_crabefi::MemoryType::Reserved,
                };
                64
            ];
            let _mem_idx = fstart_crabefi::build_efi_memory_map_from_e820(
                _e820_state.entries(),
                0, 0,
                0, 0,
                _rom_entries,
                &mut _crabefi_mem_buf,
            );
            let _crabefi_memory_map: &[fstart_crabefi::MemoryRegion] =
                &_crabefi_mem_buf[.._mem_idx];
            fstart_log::info!("EFI memory map: {} entries", _mem_idx as u32);
        }
    } else {
        quote! {
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
        }
    };

    // FDT reservation — non-x86 only.
    let fdt_reservation_setup = if platform != Platform::X86_64 {
        quote! {
            let _fdt_reservation = if _fdt_addr != 0 {
                let fdt_size = unsafe {
                    fstart_crabefi::fdt_page_aligned_size(_fdt_addr)
                };
                Some((_fdt_addr, fdt_size))
            } else {
                None
            };
        }
    } else {
        quote! {}
    };

    quote! {
        fstart_log::info!("Launching CrabEFI UEFI payload...");

        #bl31_boot

        #timer_setup
        #reset_setup
        #rng_setup
        #console_setup

        #fdt_setup
        #fdt_reservation_setup
        #memory_map_setup

        #fb_setup

        let _crabefi_config = fstart_crabefi::PlatformConfig {
            memory_map: _crabefi_memory_map,
            #timer_field
            #reset_field
            block_devices: &mut [],
            variable_backend: None,
            #debug_output_field
            #framebuffer_field
            #acpi_rsdp_field
            smbios: None,
            #fdt_field
            #rng_field
            #ecam_base_field
            deferred_buffer: None,
            runtime_region: #runtime_region_field
            heap_pre_initialized: false,
        };

        fstart_log::info!(
            "EFI memory map: {} entries, calling init_platform...",
            _mem_idx as u32,
        );

        // init_platform() is `-> !` (never returns).
        fstart_crabefi::init_platform(_crabefi_config)
    }
}
