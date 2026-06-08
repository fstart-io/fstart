//! `impl Board for _BoardDevices` orchestration.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use fstart_device_registry::Service;
use fstart_types::Platform;

use super::boot_media::{
    active_firmware_image_range_body, active_firmware_window_body, with_block_device_body,
    with_boot_media_body,
};
use super::caps_tables::{
    acpi_platform_config_body, collect_acpi_tables_body, smbios_desc_body,
    with_acpi_table_provider_body, with_memory_detector_body,
};
use super::fdt::{
    fdt_prepare_desc_body, return_to_fel_body, stage_load_desc_body, stage_load_postcar_mmio_body,
};
use super::init_caps::{dram_init_body, with_pci_root_body};
use super::lifecycle::construct_device_body;
use super::logger::install_logger_body;
use super::model::BoardEmitModel;
use super::mp::mp_init_body;
use super::payload::payload_load_desc_body;
use super::payload_uefi::{
    disable_boot_media_rom_cache_for_handoff_body, park_aps_for_payload_body,
    uefi_boot_bl31_and_resume_body, uefi_fdt_blob_body, uefi_payload_desc_body,
    with_uefi_services_body,
};
use super::phases::phase_init_body;
use super::platform::sunxi::{
    dram_size_for_handoff_body, egon_next_stage_body, next_stage_addr_body, soc_boot_media_body,
};

fn firmware_image_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    let arms = ctx
        .runtime_devices
        .providers(Service::FirmwareImageProvider)
        .map(|device| {
            let field = format_ident!("{}", device.name);
            let id_lit = proc_macro2::Literal::u8_unsuffixed(device.index as u8);
            quote! {
                #id_lit => {
                    fstart_services::FirmwareImageProvider::firmware_image(
                        self.#field
                            .as_ref()
                            .ok_or(fstart_stage_runtime::RuntimeError::UnknownDevice)?,
                    ).map_err(|_| fstart_stage_runtime::RuntimeError::Failed)
                }
            }
        });
    quote! {
        match provider {
            #(#arms)*
            _ => Err(fstart_stage_runtime::RuntimeError::UnknownDevice),
        }
    }
}

fn ffs_anchor_body(ctx: &BoardEmitModel<'_>) -> TokenStream {
    if !ctx.stage.uses_ffs {
        return quote! { None };
    }
    quote! {
        // SAFETY: FSTART_ANCHOR is emitted by `generate_anchor_static` in this
        // same stage with proper alignment (`#[link_section = ".fstart.anchor"]`
        // + `#[used]`) and is the size of `AnchorBlock`.
        Some(unsafe {
            core::slice::from_raw_parts(
                &FSTART_ANCHOR as *const fstart_types::ffs::AnchorBlock as *const u8,
                core::mem::size_of::<fstart_types::ffs::AnchorBlock>(),
            )
        })
    }
}

/// Emit the `impl fstart_stage_runtime::Board for _BoardDevices` block.
pub(super) fn emit_board_impl(platform: Platform, ctx: &BoardEmitModel<'_>) -> TokenStream {
    let jump_with_handoff_body = match platform {
        Platform::X86_64 => quote! {
            let _ = (entry, handoff_addr);
            fstart_platform::halt()
        },
        _ => quote! { fstart_platform::jump_to_with_handoff(entry, handoff_addr) },
    };

    let fdt_prepare_desc_body = fdt_prepare_desc_body(platform, ctx);
    let install_logger_body = install_logger_body(ctx);
    let stage_load_desc_body = stage_load_desc_body(ctx);
    let stage_load_postcar_mmio_body = stage_load_postcar_mmio_body(ctx);
    let return_to_fel_body = return_to_fel_body(platform, ctx);
    let with_pci_root_body = with_pci_root_body(ctx);
    let dram_init_body = dram_init_body(ctx);
    let phase_init_body = phase_init_body(ctx);

    let with_acpi_table_provider_body = with_acpi_table_provider_body(ctx);
    let with_memory_detector_body = with_memory_detector_body(ctx);
    let acpi_platform_config_body = acpi_platform_config_body(ctx);
    let collect_acpi_tables_body = collect_acpi_tables_body(ctx);
    let smbios_desc_body = smbios_desc_body(ctx);
    let mp_init_body = mp_init_body(ctx);
    let soc_boot_media_body = soc_boot_media_body(ctx);
    let next_stage_addr_body = next_stage_addr_body(ctx);
    let egon_next_stage_body = egon_next_stage_body(ctx);
    let dram_size_for_handoff_body = dram_size_for_handoff_body(ctx);
    let payload_load_desc_body = payload_load_desc_body(platform, ctx);
    let uefi_payload_desc_body = uefi_payload_desc_body(platform, ctx);
    let with_uefi_services_body = with_uefi_services_body(ctx);
    let uefi_fdt_blob_body = uefi_fdt_blob_body(platform, ctx);
    let uefi_boot_bl31_and_resume_body = uefi_boot_bl31_and_resume_body(platform);
    let park_aps_for_payload_body = park_aps_for_payload_body(platform);
    let disable_boot_media_rom_cache_for_handoff_body =
        disable_boot_media_rom_cache_for_handoff_body(platform);
    let construct_device_body = construct_device_body(ctx);
    let firmware_image_body = firmware_image_body(ctx);
    let ffs_anchor_body = ffs_anchor_body(ctx);
    let with_boot_media_body = with_boot_media_body(ctx);
    let with_block_device_body = with_block_device_body(ctx);
    let active_firmware_window_body = active_firmware_window_body();
    let active_firmware_image_range_body = active_firmware_image_range_body();

    quote! {
        #[allow(dead_code, unused_variables)]
        impl fstart_stage_runtime::Board for _BoardDevices {
            fn construct_device(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #construct_device_body
            }

            unsafe fn install_logger(
                &self,
                id: fstart_types::DeviceId,
            ) -> Result<fstart_stage_runtime::ConsoleReady, fstart_stage_runtime::RuntimeError> {
                #install_logger_body
            }

            #[cfg(feature = "stage-flow-fdt")]
            fn fdt_prepare_desc(&self) -> Option<fstart_stage_runtime::FdtPrepareDesc> {
                #fdt_prepare_desc_body
            }
            #[cfg(feature = "stage-flow-ffs")]
            fn payload_load_desc(&self) -> Option<fstart_stage_runtime::PayloadLoadDesc> {
                #payload_load_desc_body
            }

            #[cfg(feature = "stage-flow-uefi")]
            fn uefi_payload_desc(&self) -> Option<fstart_stage_runtime::UefiPayloadDesc> {
                #uefi_payload_desc_body
            }

            #[cfg(feature = "stage-flow-uefi")]
            fn with_uefi_services<R>(
                &self,
                run: impl FnOnce(fstart_stage_runtime::UefiServices<'_>) -> R,
            ) -> R {
                #with_uefi_services_body
            }

            #[cfg(feature = "stage-flow-uefi")]
            fn uefi_fdt_blob(&self, addr: u64) -> Option<&'static [u8]> {
                #uefi_fdt_blob_body
            }

            #[cfg(feature = "stage-flow-uefi")]
            fn uefi_boot_bl31_and_resume(&self, fw_load_addr: u64, fdt_addr: u64) {
                #uefi_boot_bl31_and_resume_body
            }

            #[cfg(feature = "stage-flow-uefi")]
            fn park_aps_for_payload(&self) {
                #park_aps_for_payload_body
            }

            #[cfg(feature = "stage-flow-uefi")]
            fn disable_boot_media_rom_cache_for_handoff(&self) {
                #disable_boot_media_rom_cache_for_handoff_body
            }

            #[cfg(feature = "stage-flow-ffs")]
            fn stage_load_desc(&self) -> Option<fstart_stage_runtime::StageLoadDesc> {
                #stage_load_desc_body
            }

            #[cfg(feature = "stage-flow-ffs")]
            fn stage_load_postcar_mmio(
                &self,
                next_stage: &str,
                anchor: &'static [u8],
                image_base: u64,
                image_size: u64,
            ) -> ! {
                #stage_load_postcar_mmio_body
            }
            #[cfg(feature = "stage-flow-acpi")]
            fn acpi_platform_config(
                &self,
            ) -> Option<(fstart_acpi::platform::PlatformConfig, bool)> {
                #acpi_platform_config_body
            }

            #[cfg(feature = "stage-flow-acpi")]
            fn collect_acpi_tables(
                &self,
                dsdt_aml: &mut fstart_stage_runtime::AcpiDsdtAml,
                extra_tables: &mut fstart_stage_runtime::AcpiExtraTables,
            ) -> Result<(), fstart_stage_runtime::RuntimeError> {
                #collect_acpi_tables_body
            }
            #[cfg(feature = "stage-flow-smbios")]
            fn smbios_desc(
                &self,
            ) -> Option<fstart_capabilities::smbios::SmbiosDesc<'static>> {
                #smbios_desc_body
            }

            #[cfg(feature = "stage-flow-mp")]
            fn with_mp_services<R>(
                &mut self,
                smm: bool,
                run: impl FnOnce(fstart_stage_runtime::MpServices<'_>) -> R,
            ) -> Result<R, fstart_stage_runtime::RuntimeError> {
                #mp_init_body
            }

            #[cfg(feature = "stage-flow-mp")]
            fn active_firmware_image_range(&self, offset: u64, size: u64) -> Option<u64> {
                #active_firmware_image_range_body
            }

            fn phase_init(
                &mut self,
                phase: fstart_stage_runtime::StagePhase,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #phase_init_body
            }

            fn dram_init(
                &mut self,
                id: fstart_types::DeviceId,
            ) -> Result<(), fstart_services::device::DeviceError> {
                #dram_init_body
            }

            #[cfg(feature = "stage-flow-pci")]
            fn with_pci_root<R>(
                &mut self,
                id: fstart_types::DeviceId,
                run: impl FnOnce(
                    &mut dyn fstart_services::PciRootBus,
                    &'static str,
                    &'static str,
                ) -> R,
            ) -> Result<R, fstart_stage_runtime::RuntimeError> {
                #with_pci_root_body
            }

            fn with_acpi_table_provider<R>(
                &self,
                id: fstart_types::DeviceId,
                run: impl FnOnce(
                    &dyn fstart_services::acpi_provider::AcpiTableProvider,
                    &'static str,
                ) -> R,
            ) -> Result<R, fstart_stage_runtime::RuntimeError> {
                #with_acpi_table_provider_body
            }

            fn set_acpi_rsdp_addr(&mut self, addr: u64) {
                self._acpi_rsdp_addr = addr;
            }

            fn with_memory_detector<R>(
                &self,
                id: fstart_types::DeviceId,
                run: impl FnOnce(
                    &dyn fstart_services::memory_detect::MemoryDetector,
                    &'static str,
                ) -> R,
            ) -> Result<R, fstart_stage_runtime::RuntimeError> {
                #with_memory_detector_body
            }

            fn return_to_fel(&self) -> ! { #return_to_fel_body }

            fn soc_boot_media(&self) -> Option<u8> {
                #soc_boot_media_body
            }

            fn set_boot_media_state(&mut self, state: fstart_stage_runtime::BootMediaState) {
                self._boot_media = state;
            }

            fn firmware_image(
                &self,
                provider: fstart_types::DeviceId,
            ) -> Result<fstart_services::FirmwareImage, fstart_stage_runtime::RuntimeError> {
                #firmware_image_body
            }

            fn ffs_anchor(&self) -> Option<&'static [u8]> {
                #ffs_anchor_body
            }

            #[cfg(feature = "stage-flow-ffs")]
            fn active_firmware_window(&self) -> Option<(u64, u64)> {
                #active_firmware_window_body
            }

            fn with_boot_media<R>(
                &self,
                caller_tag: &str,
                none: R,
                run: impl FnOnce(
                    &dyn fstart_services::BootMedia,
                    Option<&mut fstart_services::TempRamArena>,
                ) -> R,
            ) -> R {
                #with_boot_media_body
            }

            fn with_block_device<R>(
                &self,
                id: fstart_types::DeviceId,
                run: impl FnOnce(&dyn fstart_services::BlockDevice, &'static str) -> R,
            ) -> Result<R, fstart_stage_runtime::RuntimeError> {
                #with_block_device_body
            }

            #[cfg(feature = "stage-flow-fel")]
            fn next_stage_addr(
                &self,
                next_stage: &str,
            ) -> Option<fstart_stage_runtime::NextStageAddr> {
                #next_stage_addr_body
            }

            #[cfg(feature = "stage-flow-fel")]
            fn egon_next_stage(&self) -> Option<fstart_stage_runtime::EgonNextStage> {
                #egon_next_stage_body
            }

            #[cfg(feature = "stage-flow-fel")]
            fn dram_size_for_handoff(&self) -> u64 {
                #dram_size_for_handoff_body
            }

            fn halt(&self) -> ! { fstart_platform::halt() }
            fn jump_to(&self, entry: u64) -> ! { fstart_platform::jump_to(entry) }
            fn boot_hart_id(&self) -> u64 {
                #[cfg(feature = "riscv64")]
                { fstart_platform::boot_hart_id() }
                #[cfg(not(feature = "riscv64"))]
                { 0 }
            }
            fn acpi_rsdp_addr(&self) -> u64 { self._acpi_rsdp_addr }
            fn boot_linux(&self, params: &fstart_services::boot::BootLinuxParams<'_>) -> ! {
                fstart_platform::boot_linux(params)
            }
            fn jump_to_with_handoff(&self, entry: u64, handoff_addr: usize) -> ! {
                #jump_with_handoff_body
            }
        }
    }
}
