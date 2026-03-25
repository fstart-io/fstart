//! Code generation for SMBIOS table preparation.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

use fstart_types::BoardConfig;

/// Generate code for the SmBiosPrepare capability.
///
/// Emits calls to `fstart_smbios::assemble_and_write` with all the
/// table data from the board RON's `smbios` config section.
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

    // Type 2: Baseboard Information
    let bb_manufacturer = smbios_cfg.baseboard_manufacturer.as_str();
    let bb_product = smbios_cfg.baseboard_product.as_str();
    let has_baseboard =
        !smbios_cfg.baseboard_manufacturer.is_empty() || !smbios_cfg.baseboard_product.is_empty();

    // Type 3: Enclosure
    let chassis_byte = Literal::u8_unsuffixed(smbios_cfg.chassis_type.to_smbios_byte());
    let chassis_manufacturer = if smbios_cfg.chassis_manufacturer.is_empty() {
        smbios_cfg.system_manufacturer.as_str()
    } else {
        smbios_cfg.chassis_manufacturer.as_str()
    };

    // Type 7: Cache Info + Type 4: Processors
    let mut processor_stmts = TokenStream::new();
    for (proc_idx, proc) in smbios_cfg.processors.iter().enumerate() {
        let socket = proc.socket.as_str();
        let manufacturer = proc.manufacturer.as_str();
        let family = Literal::u16_unsuffixed(proc.processor_family.to_smbios_u16());
        let max_speed = Literal::u16_unsuffixed(proc.max_speed_mhz);
        let cores = Literal::u16_unsuffixed(proc.core_count);
        let threads = Literal::u16_unsuffixed(proc.thread_count);

        if proc.caches.is_empty() {
            // No cache info -- use the simple add_processor method.
            processor_stmts.extend(quote! {
                w.add_processor(#socket, #manufacturer, #family, #max_speed, #cores, #threads);
            });
        } else {
            // Emit Type 7 cache entries, then Type 4 with cache handles.
            let mut cache_handle_vars = [None, None, None]; // L1, L2, L3
            for (cache_idx, cache) in proc.caches.iter().enumerate() {
                let designation = cache.designation.as_str();
                let size_kb = Literal::u32_unsuffixed(cache.size_kb);
                let assoc = Literal::u8_unsuffixed(cache.associativity.to_smbios_byte());
                let ct = Literal::u8_unsuffixed(cache.cache_type.to_smbios_byte());
                let level = Literal::u8_unsuffixed(cache.level);
                let var = format_ident!("_cache_h_p{}_{}", proc_idx, cache_idx);

                processor_stmts.extend(quote! {
                    let #var = w.add_cache_info(#designation, #level, #size_kb, #assoc, #ct);
                });

                // Map to L1/L2/L3 slot based on level.
                let slot = (cache.level as usize).saturating_sub(1);
                if slot < 3 {
                    cache_handle_vars[slot] = Some(var);
                }
            }

            let l1 = cache_handle_vars[0]
                .as_ref()
                .map_or_else(|| quote! { 0xFFFFu16 }, |v| quote! { #v });
            let l2 = cache_handle_vars[1]
                .as_ref()
                .map_or_else(|| quote! { 0xFFFFu16 }, |v| quote! { #v });
            let l3 = cache_handle_vars[2]
                .as_ref()
                .map_or_else(|| quote! { 0xFFFFu16 }, |v| quote! { #v });

            processor_stmts.extend(quote! {
                w.add_processor_with_caches(
                    #socket, #manufacturer, #family, #max_speed, #cores, #threads,
                    #l1, #l2, #l3,
                );
            });
        }
    }

    // Type 16/17/19: Memory
    let num_mem_devices = smbios_cfg.memory_devices.len();
    let has_memory = num_mem_devices > 0;

    let mut memory_stmts = TokenStream::new();
    if has_memory {
        // Compute total capacity in KB for the Physical Memory Array.
        let total_capacity_kb: u64 = smbios_cfg
            .memory_devices
            .iter()
            .map(|d| d.size_mb as u64 * 1024)
            .sum();
        let total_kb_lit = Literal::u64_unsuffixed(total_capacity_kb);
        let num_devs_lit = Literal::u16_unsuffixed(num_mem_devices as u16);

        memory_stmts.extend(quote! {
            w.add_physical_memory_array(#total_kb_lit, #num_devs_lit);
        });

        for dev in smbios_cfg.memory_devices.iter() {
            let locator = dev.locator.as_str();
            let size_mb = Literal::u32_unsuffixed(dev.size_mb);
            let speed = Literal::u16_unsuffixed(dev.speed_mhz);
            let mem_type = Literal::u8_unsuffixed(dev.memory_type.to_smbios_byte());
            memory_stmts.extend(quote! {
                w.add_memory_device(#locator, #size_mb, #speed, #mem_type);
            });
        }

        // Type 19: Memory Array Mapped Address.
        // Use the first RAM region from the board config as the mapped range.
        if let Some(ram_region) = config
            .memory
            .regions
            .iter()
            .find(|r| r.kind == fstart_types::memory::RegionKind::Ram)
        {
            let start = Literal::u64_unsuffixed(ram_region.base);
            let end = Literal::u64_unsuffixed(ram_region.base + ram_region.size - 1);
            memory_stmts.extend(quote! {
                w.add_memory_array_mapped_address(#start, #end, 1);
            });
        }
    }

    let baseboard_stmt = if has_baseboard {
        quote! { w.add_baseboard_info(#bb_manufacturer, #bb_product); }
    } else {
        TokenStream::new()
    };

    quote! {
        fstart_log::info!("capability: SmBiosPrepare");
        {
            extern crate alloc;
            use alloc::vec;

            // Allocate a heap buffer for SMBIOS tables (persists until reset).
            const _SMBIOS_BUF_SIZE: usize = 64 * 1024;
            let smbios_buf = vec![0u8; _SMBIOS_BUF_SIZE];
            let smbios_addr = smbios_buf.as_ptr() as u64;
            core::mem::forget(smbios_buf);

            let smbios_len = fstart_smbios::assemble_and_write(smbios_addr, |w| {
                w.add_bios_info(#bios_vendor, #bios_version, #bios_release_date);
                w.add_system_info(#sys_manufacturer, #sys_product, #sys_version, #sys_serial_expr);
                #baseboard_stmt
                w.add_enclosure(#chassis_byte, #chassis_manufacturer);
                #processor_stmts
                #memory_stmts
                w.add_system_boot_info();
                w.add_end_of_table();
            });
            fstart_log::info!(
                "SmBiosPrepare: {} bytes written to {}",
                smbios_len as u32,
                fstart_log::Hex(smbios_addr),
            );
        }
    }
}
