//! Linux boot-protocol emission helpers.

use proc_macro2::TokenStream;
use quote::quote;

use fstart_types::{PayloadConfig, Platform};

use crate::stage_gen::tokens::hex_addr;

/// Emit the platform-specific boot protocol sequence.
///
/// Reads per-board state from `&self` where needed:
///
/// - `_acpi_rsdp_addr` → `self._acpi_rsdp_addr` (x86_64 only; Linux
///   `boot_linux` wants the RSDP).
///
/// All other values (kernel addr, dtb addr, firmware addr, bootargs)
/// come from the payload literal — they're const, not runtime state.
pub(in crate::stage_gen::board_gen) fn platform_boot_protocol_stmts(
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
