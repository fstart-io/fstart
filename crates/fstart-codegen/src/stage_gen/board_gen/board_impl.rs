//! `impl Board for _BoardDevices` orchestration.

use proc_macro2::TokenStream;
use quote::quote;

use fstart_device_registry::Service;
use fstart_types::Platform;

use super::boot_media::anchor_bytes_stmt;
use super::caps_tables::{
    acpi_load_body, acpi_prepare_body, memory_detect_body, smbios_prepare_body,
};
use super::fdt::{fdt_prepare_body, return_to_fel_body, stage_load_body};
use super::init_caps::{dram_init_body, late_driver_init_body, pci_init_body};
use super::lifecycle::{init_all_devices_body, init_device_body};
use super::logger::install_logger_body;
use super::model::BoardEmitModel;
use super::mp::mp_init_body;
use super::payload::payload_load_body;
use super::phases::{phase_init_body, PhaseSpec};
use super::sunxi::{boot_media_select_body, load_next_stage_body};

/// Emit the `impl fstart_stage_runtime::Board for _BoardDevices` block.
pub(super) fn emit_board_impl(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    let jump_with_handoff_body = match platform {
        Platform::X86_64 => quote! {
            let _ = (entry, handoff_addr);
            fstart_platform::halt()
        },
        _ => quote! { fstart_platform::jump_to_with_handoff(entry, handoff_addr) },
    };

    let sig_verify_body = super::security::sig_verify_body(ctx);
    let fdt_prepare_body = fdt_prepare_body(platform, ctx);
    let install_logger_body = install_logger_body(ctx);
    let stage_load_body = stage_load_body(ctx);
    let return_to_fel_body = return_to_fel_body(platform, ctx);
    let pci_init_body = pci_init_body(ctx);
    let dram_init_body = dram_init_body(ctx);
    let pre_console_init_body = phase_init_body(
        ctx,
        PhaseSpec::new(
            Service::PreConsoleInit,
            "PreConsoleInit",
            "pre_console_init",
        ),
    );
    let early_init_body = phase_init_body(
        ctx,
        PhaseSpec::new(Service::EarlyInit, "EarlyInit", "early_init"),
    );
    let stage_local_init_body = phase_init_body(
        ctx,
        PhaseSpec::new(
            Service::StageLocalInit,
            "StageLocalInit",
            "stage_local_init",
        ),
    );
    let post_dram_init_body = phase_init_body(
        ctx,
        PhaseSpec::new(Service::PostDramInit, "PostDramInit", "post_dram_init"),
    );
    let finalize_init_body = phase_init_body(
        ctx,
        PhaseSpec::new(Service::FinalizeInit, "FinalizeInit", "finalize_init"),
    );

    let acpi_load_body = acpi_load_body(ctx);
    let memory_detect_body = memory_detect_body(ctx);
    let acpi_prepare_body = acpi_prepare_body(ctx);
    let smbios_prepare_body = smbios_prepare_body(ctx);
    let mp_init_body = mp_init_body(ctx);
    let boot_media_select_body = boot_media_select_body(ctx);
    let load_next_stage_body = load_next_stage_body(ctx);
    let payload_load_body = payload_load_body(platform, ctx);
    let init_device_body = init_device_body(ctx);
    let init_all_devices_body = init_all_devices_body(ctx);
    let late_driver_init_body = late_driver_init_body(ctx);
    let boot_media_context_publish = if ctx.stage.uses_ffs {
        let anchor = anchor_bytes_stmt();
        quote! {
            if device.is_none() {
                #anchor
                fstart_services::ffs_context::set_memory_mapped(_anchor_bytes, offset, size);
            }
        }
    } else {
        quote! {}
    };

    quote! {
        #[allow(dead_code, unused_variables)]
        impl fstart_stage_runtime::Board for _BoardDevices {
            fn init_device(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #init_device_body
            }

            fn init_all_devices(
                &mut self,
                skip: &fstart_stage_runtime::DeviceMask,
                gated: &fstart_stage_runtime::DeviceMask,
            ) {
                #init_all_devices_body
            }

            unsafe fn install_logger(&self, id: fstart_types::DeviceId) {
                #install_logger_body
            }

            fn memory_init(&self) {
                fstart_capabilities::memory_init();
            }

            fn late_driver_init_complete(&mut self, count: usize) {
                #late_driver_init_body
                fstart_capabilities::late_driver_init_complete(count);
            }

            fn sig_verify(&self) { #sig_verify_body }
            fn fdt_prepare(&self) { #fdt_prepare_body }
            fn payload_load(&self) -> ! { #payload_load_body }
            fn stage_load(&self, next_stage: &str) -> ! { #stage_load_body }
            fn acpi_prepare(&mut self) { #acpi_prepare_body }
            fn smbios_prepare(&self) { #smbios_prepare_body }

            fn mp_init(
                &mut self,
                cpu_model: &str,
                num_cpus: u16,
                smm: bool,
            ) -> Result<(), fstart_stage_runtime::RuntimeError> {
                #mp_init_body
            }

            fn pre_console_init(
                &mut self,
                ids: &[fstart_types::DeviceId],
            ) -> Result<(), fstart_services::device::DeviceError> {
                #pre_console_init_body
            }

            fn early_init(
                &mut self,
                ids: &[fstart_types::DeviceId],
            ) -> Result<(), fstart_services::device::DeviceError> {
                #early_init_body
            }

            fn stage_local_init(
                &mut self,
                ids: &[fstart_types::DeviceId],
            ) -> Result<(), fstart_services::device::DeviceError> {
                #stage_local_init_body
            }

            fn post_dram_init(
                &mut self,
                ids: &[fstart_types::DeviceId],
            ) -> Result<(), fstart_services::device::DeviceError> {
                #post_dram_init_body
            }

            fn finalize_init(
                &mut self,
                ids: &[fstart_types::DeviceId],
            ) -> Result<(), fstart_services::device::DeviceError> {
                #finalize_init_body
            }

            fn dram_init(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #dram_init_body
            }

            fn pci_init(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #pci_init_body
            }

            fn acpi_load(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #acpi_load_body
            }

            fn memory_detect(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #memory_detect_body
            }

            fn return_to_fel(&self) -> ! { #return_to_fel_body }

            fn boot_media_select(
                &mut self,
                candidates: &[fstart_stage_runtime::BootMediaCandidate],
            ) -> Option<fstart_types::DeviceId> {
                #boot_media_select_body
            }

            fn boot_media_static(
                &mut self,
                device: Option<fstart_types::DeviceId>,
                offset: u64,
                size: u64,
            ) {
                self._boot_media =
                    fstart_stage_runtime::BootMediaState::from_static(device, offset, size);
                #boot_media_context_publish
            }

            fn load_next_stage(&mut self, next_stage: &str) -> ! {
                #load_next_stage_body
            }

            fn halt(&self) -> ! { fstart_platform::halt() }
            fn jump_to(&self, entry: u64) -> ! { fstart_platform::jump_to(entry) }
            fn jump_to_with_handoff(&self, entry: u64, handoff_addr: usize) -> ! {
                #jump_with_handoff_body
            }
        }
    }
}
