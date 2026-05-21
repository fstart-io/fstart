//! `_BoardDevices` state struct and constructor emission.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::stage_gen::tokens::hex_addr;

use super::model::BoardEmitModel;

/// Emit the `_BoardDevices` struct.
pub(super) fn emit_adapter_struct(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let fields = ctx.runtime_devices.runtime().map(|device| {
        let field_name = format_ident!("{}", device.name);
        let field_type = format_ident!("{}", device.instance.meta().type_name);
        quote! { #field_name: Option<#field_type>, }
    });

    quote! {
        /// Board adapter produced by `fstart-codegen::board_gen`.
        ///
        /// Carries one `Option<Driver>` per enabled device plus the
        /// bookkeeping state [`Board`](fstart_stage_runtime::Board)
        /// trampolines need ([`DeviceMask`] for init tracking,
        /// [`BootMediaState`] for the current boot medium, static
        /// FDT data, and the previous-stage handoff).  Implements
        /// [`fstart_stage_runtime::Board`] so the handwritten
        /// [`run_stage`](fstart_stage_runtime::run_stage) executor
        /// can drive it.
        ///
        /// [`DeviceMask`]: fstart_stage_runtime::DeviceMask
        /// [`BootMediaState`]: fstart_stage_runtime::BootMediaState
        #[allow(dead_code, non_camel_case_types)]
        struct _BoardDevices {
            #(#fields)*
            _inited: fstart_stage_runtime::DeviceMask,
            _boot_media: fstart_stage_runtime::BootMediaState,
            _dtb_dst_addr: u64,
            _bootargs: &'static str,
            _dram_base: u64,
            _dram_size_static: u64,
            _handoff: Option<fstart_types::handoff::StageHandoff>,
            /// RSDP physical address, populated by `acpi_load` and
            /// read by future `acpi_prepare` / `payload_load`
            /// trampolines.  `0` means "not set yet"; boards
            /// without `AcpiLoad` leave it at `0` forever.
            _acpi_rsdp_addr: u64,
            /// eGON header SRAM base address for Allwinner sunxi
            /// boards.  Read by `boot_media_select` and
            /// `load_next_stage` to resolve the hardware boot-media
            /// byte and next-stage header values.
            _egon_sram_base: u64,
        }
    }
}

/// Emit `impl _BoardDevices { const fn new() -> Self }`.
pub(super) fn emit_adapter_new(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let field_inits = ctx.runtime_devices.runtime().map(|device| {
        let field_name = format_ident!("{}", device.name);
        quote! { #field_name: None, }
    });

    let dtb_dst_lit = hex_addr(
        ctx.config
            .payload
            .as_ref()
            .and_then(|p| p.dtb_addr)
            .unwrap_or(0),
    );
    let bootargs_lit = ctx
        .config
        .payload
        .as_ref()
        .and_then(|p| p.bootargs.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("");
    let dram_base_lit = hex_addr(ctx.dram_base);
    let dram_size_lit = hex_addr(ctx.dram_size_static);
    let egon_sram_base_lit = hex_addr(crate::stage_gen::capabilities::egon_sram_base(ctx.config));

    quote! {
        #[allow(dead_code)]
        impl _BoardDevices {
            /// Zero-initialised adapter.  See [`emit_adapter_new`] doc.
            const fn new() -> Self {
                Self {
                    #(#field_inits)*
                    _inited: fstart_stage_runtime::DeviceMask::new(),
                    _boot_media: fstart_stage_runtime::BootMediaState::None,
                    _dtb_dst_addr: #dtb_dst_lit,
                    _bootargs: #bootargs_lit,
                    _dram_base: #dram_base_lit,
                    _dram_size_static: #dram_size_lit,
                    _handoff: None,
                    _acpi_rsdp_addr: 0,
                    _egon_sram_base: #egon_sram_base_lit,
                }
            }
        }
    }
}
