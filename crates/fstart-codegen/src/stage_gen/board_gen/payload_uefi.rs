//! UEFI/CrabEFI payload-load emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::memory::RegionKind;
use fstart_types::{FirmwareKind, Platform, StageLayout};

use crate::stage_gen::tokens::hex_addr;

use super::boot_media::{anchor_bytes_stmt, match_boot_media};
use super::model::BoardEmitModel;

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
pub(super) fn payload_load_uefi_body(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
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
                    region_type: fstart_crabefi::MemoryType::Reserved,
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

    // Console device for DebugOutput adapter: use this stage's active
    // ConsoleInit device.  Some boards have multiple Console providers (e.g.
    // X61's onboard UART plus dock SuperIO UART); picking the first provider
    // can route all CrabEFI logs to an inactive/debug-invisible port.
    let stage_console_name = ctx.stage.capabilities.iter().find_map(|cap| {
        if let fstart_types::Capability::ConsoleInit { device } = cap {
            Some(device.as_str())
        } else {
            None
        }
    });
    let console_device = stage_console_name
        .and_then(|name| {
            ctx.runtime_devices
                .providers(Service::Console)
                .find(|device| device.name == name)
        })
        .or_else(|| ctx.runtime_devices.providers(Service::Console).next());
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

    // PCI device for ECAM base.  Also reserve the ECAM/MMCONFIG aperture
    // in the EFI memory map; Linux requires MCFG ranges to be reserved and
    // coreboot exposes the same window as Reserved memory.
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
    let ecam_reserved_push = match pci_device {
        Some(device) => {
            let field = format_ident!("{}", device.name);
            quote! {
                _platform_entries_buf[_platform_entries_idx] = fstart_crabefi::MemoryRegion {
                    base: self.#field
                        .as_ref()
                        .unwrap_or_else(|| fstart_platform::halt())
                        .ecam_base(),
                    size: self.#field
                        .as_ref()
                        .unwrap_or_else(|| fstart_platform::halt())
                        .ecam_size(),
                    region_type: fstart_crabefi::MemoryType::Reserved,
                };
                _platform_entries_idx += 1;
            }
        }
        None => quote! {},
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

    // SMBIOS entry point: populated by the SmBiosPrepare capability.
    let smbios_field = if ctx.stage.uses_smbios {
        quote! { smbios: fstart_capabilities::smbios::entry_point(), }
    } else {
        quote! { smbios: None, }
    };

    let acpi_reserved_push = if platform == Platform::X86_64 && ctx.stage.uses_acpi_prepare {
        quote! {
            if let Some((base, size)) = fstart_capabilities::acpi::prepared_region() {
                _platform_entries_buf[_platform_entries_idx] = fstart_crabefi::MemoryRegion {
                    base,
                    size,
                    region_type: fstart_crabefi::MemoryType::AcpiReclaimable,
                };
                _platform_entries_idx += 1;
            }
        }
    } else {
        quote! {}
    };

    let smbios_reserved_push = if ctx.stage.uses_smbios {
        quote! {
            if let Some((base, size)) = fstart_capabilities::smbios::prepared_region() {
                _platform_entries_buf[_platform_entries_idx] = fstart_crabefi::MemoryRegion {
                    base,
                    size,
                    region_type: fstart_crabefi::MemoryType::Reserved,
                };
                _platform_entries_idx += 1;
            }
        }
    } else {
        quote! {}
    };

    // Runtime region: x86 only.  Let CrabEFI carve these out of Conventional
    // RAM so it assigns the right RTCode/RTData attributes (notably XP on
    // RuntimeServicesData).  Do not pre-type these ranges in the platform map.
    let runtime_region_field = if platform == Platform::X86_64 {
        quote! { Some(_runtime_region), }
    } else {
        quote! { None, }
    };

    // Memory map — x86 from e820, others from static RAM + FDT.
    let memory_map_setup = if platform == Platform::X86_64 {
        quote! {
            let _e820_state = unsafe { fstart_services::memory_detect::e820_state() };
            let mut _platform_entries_buf: [fstart_crabefi::MemoryRegion; 8] = [
                fstart_crabefi::MemoryRegion {
                    base: 0, size: 0,
                    region_type: fstart_crabefi::MemoryType::Reserved,
                };
                8
            ];
            let mut _platform_entries_idx = 0usize;
            let _runtime_region = fstart_crabefi::compute_runtime_region();
            for _entry in &[
                #static_mem_entries
            ] {
                _platform_entries_buf[_platform_entries_idx] = *_entry;
                _platform_entries_idx += 1;
            }
            #ecam_reserved_push
            #acpi_reserved_push
            #smbios_reserved_push
            let _rom_entries = &_platform_entries_buf[.._platform_entries_idx];
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

        #[cfg(all(target_arch = "x86_64", feature = "mp"))]
        fstart_mp::park_aps_for_payload();

        #[cfg(target_arch = "x86_64")]
        fstart_platform::disable_boot_media_rom_cache_for_handoff();

        let _crabefi_config = fstart_crabefi::PlatformConfig {
            memory_map: _crabefi_memory_map,
            #timer_field
            #reset_field
            block_devices: &mut [],
            variable_backend: None,
            #debug_output_field
            #framebuffer_field
            #acpi_rsdp_field
            #smbios_field
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
