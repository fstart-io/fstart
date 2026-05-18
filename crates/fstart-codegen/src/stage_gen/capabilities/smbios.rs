//! Code generation for SMBIOS table preparation.
//!
//! Emits a static [`SmbiosDesc`](fstart_capabilities::smbios::SmbiosDesc)
//! descriptor from the board RON config and calls
//! [`prepare`](fstart_capabilities::smbios::prepare) at runtime.
//! All iteration logic (cache handle mapping, memory array linking) is
//! in the library function — codegen only emits the data.

use proc_macro2::{Literal, TokenStream};
use quote::quote;

use fstart_types::memory::RegionKind;
use fstart_types::BoardConfig;

use super::super::tokens::hex_addr;

/// Generate code for the SmBiosPrepare capability.
///
/// Constructs a `fstart_capabilities::smbios::SmbiosDesc` struct literal
/// from the board RON's `smbios` config, then calls the library's
/// `prepare()` function which handles all SMBIOS writer sequencing.
pub(in crate::stage_gen) fn generate_smbios_prepare(config: &BoardConfig) -> TokenStream {
    let smbios_cfg = config.smbios.as_ref().unwrap_or_else(|| {
        panic!("SmBiosPrepare capability requires `smbios` config in board RON");
    });

    // Type 0: BIOS Information
    let bios_vendor = smbios_cfg.bios_vendor.as_str();
    let bios_version = smbios_cfg.bios_version.as_str();
    let bios_release_date = smbios_cfg.bios_release_date.as_str();

    // Type 1: System Information
    let sys_manufacturer = smbios_cfg.system_manufacturer.as_str();
    let sys_product = smbios_cfg.system_product.as_str();
    let sys_version = smbios_cfg.system_version.as_str();
    let sys_serial_expr = if smbios_cfg.system_serial.is_empty() {
        quote! { None }
    } else {
        let s = smbios_cfg.system_serial.as_str();
        quote! { Some(#s) }
    };

    // Type 2: Baseboard
    let bb_manufacturer = smbios_cfg.baseboard_manufacturer.as_str();
    let bb_product = smbios_cfg.baseboard_product.as_str();

    // Type 3: Chassis
    let chassis_byte = Literal::u8_unsuffixed(smbios_cfg.chassis_type.to_smbios_byte());
    let chassis_manufacturer = if smbios_cfg.chassis_manufacturer.is_empty() {
        smbios_cfg.system_manufacturer.as_str()
    } else {
        smbios_cfg.chassis_manufacturer.as_str()
    };

    // Type 4/7: Processors with caches
    //
    // Runtime-detectable fields (max_speed_mhz, core_count, thread_count)
    // may be `None` in RON for x86 boards that rely on CPUID probing.
    // For the codegen output we fall back to 0; a future `SmBiosPrepare`
    // extension will probe CPUID at runtime when the RON value is `None`.
    let processor_items: Vec<TokenStream> = smbios_cfg
        .processors
        .iter()
        .map(|proc| {
            let socket = proc.socket.as_str();
            let manufacturer = proc.manufacturer.as_str();
            let family = Literal::u16_unsuffixed(proc.processor_family.to_smbios_u16());
            let max_speed = Literal::u16_unsuffixed(proc.max_speed_mhz.unwrap_or(0));
            let cores = Literal::u16_unsuffixed(proc.core_count.unwrap_or(0));
            let threads = Literal::u16_unsuffixed(proc.thread_count.unwrap_or(0));

            let cache_items: Vec<TokenStream> = proc
                .caches
                .iter()
                .map(|cache| {
                    let designation = cache.designation.as_str();
                    let level = Literal::u8_unsuffixed(cache.level);
                    let size_kb = Literal::u32_unsuffixed(cache.size_kb);
                    let assoc = Literal::u8_unsuffixed(cache.associativity.to_smbios_byte());
                    let ct = Literal::u8_unsuffixed(cache.cache_type.to_smbios_byte());
                    quote! {
                        fstart_capabilities::smbios::CacheDesc {
                            designation: #designation,
                            level: #level,
                            size_kb: #size_kb,
                            associativity: #assoc,
                            cache_type: #ct,
                        }
                    }
                })
                .collect();

            quote! {
                fstart_capabilities::smbios::ProcessorDesc {
                    socket: #socket,
                    manufacturer: #manufacturer,
                    family: #family,
                    max_speed_mhz: #max_speed,
                    core_count: #cores,
                    thread_count: #threads,
                    caches: &[#(#cache_items),*],
                }
            }
        })
        .collect();

    // Type 16/17: Memory devices
    //
    // Runtime-detectable fields (size_mb, speed_mhz, memory_type) may be
    // `None` in RON for x86 boards that rely on SPD probing over SMBus.
    // We emit 0 / Unknown sentinels here.  These are deliberate
    // runtime-detect placeholders — the `SmBiosPrepare` capability
    // will overwrite them via CPUID (Type 4) and SPD (Type 17)
    // at boot once the runtime probe path is wired.  Until then,
    // boards that leave these fields `None` in RON will report
    // `0 MHz` / `Unknown` to the OS.
    let memory_items: Vec<TokenStream> = smbios_cfg
        .memory_devices
        .iter()
        .map(|dev| {
            let locator = dev.locator.as_str();
            let size_mb = Literal::u32_unsuffixed(dev.size_mb.unwrap_or(0));
            let speed = Literal::u16_unsuffixed(dev.speed_mhz.unwrap_or(0));
            let mem_type = Literal::u8_unsuffixed(
                dev.memory_type
                    .unwrap_or(fstart_types::smbios::MemoryDeviceType::Unknown)
                    .to_smbios_byte(),
            );
            quote! {
                fstart_capabilities::smbios::MemoryDeviceDesc {
                    locator: #locator,
                    size_mb: #size_mb,
                    speed_mhz: #speed,
                    memory_type: #mem_type,
                }
            }
        })
        .collect();

    // Type 19: RAM region from board config
    let (ram_base_lit, ram_end_lit) = config
        .memory
        .regions
        .iter()
        .find(|r| r.kind == RegionKind::Ram)
        .map(|r| (hex_addr(r.base), hex_addr(r.base + r.size - 1)))
        .unwrap_or_else(|| (quote! { 0u64 }, quote! { 0u64 }));

    quote! {
        fstart_capabilities::smbios::prepare(
            &fstart_capabilities::smbios::SmbiosDesc {
                bios_vendor: #bios_vendor,
                bios_version: #bios_version,
                bios_release_date: #bios_release_date,
                sys_manufacturer: #sys_manufacturer,
                sys_product: #sys_product,
                sys_version: #sys_version,
                sys_serial: #sys_serial_expr,
                bb_manufacturer: #bb_manufacturer,
                bb_product: #bb_product,
                chassis_type: #chassis_byte,
                chassis_manufacturer: #chassis_manufacturer,
                processors: &[#(#processor_items),*],
                memory_devices: &[#(#memory_items),*],
                ram_base: #ram_base_lit,
                ram_end: #ram_end_lit,
            },
        );
    }
}
