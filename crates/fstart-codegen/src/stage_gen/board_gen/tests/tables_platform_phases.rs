use super::{adapter_source_for_board, adapter_source_for_stage};

#[test]
fn acpi_prepare_emits_real_body_on_sbsa() {
    // qemu-sbsa has `AcpiPrepare` + a populated `acpi` RON config (ARM
    // SBSA platform). The adapter must emit primitive descriptor/table
    // collection only; runtime owns the acpi::prepare call.
    let src = adapter_source_for_board("qemu-sbsa");
    assert!(
        src.contains("let platform_acpi"),
        "acpi_prepare must emit platform_acpi binding; got:\n{src}"
    );
    assert!(
        !src.contains("fstart_capabilities::acpi::prepare"),
        "adapter must not call the ACPI prepare capability fn; got:\n{src}"
    );
    assert!(
        src.contains("fn acpi_platform_config") && src.contains("fn collect_acpi_tables"),
        "adapter must expose primitive ACPI descriptor/table collection; got:\n{src}"
    );
    assert!(
        src.contains("PlatformConfig::Arm"),
        "sbsa acpi_prepare must use the Arm platform variant; got:\n{src}"
    );
}

#[test]
fn acpi_prepare_unreachable_on_boards_without_acpi_config() {
    // qemu-riscv64 has no `acpi` RON config and no AcpiPrepare
    // capability, so the primitive descriptor must be absent.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("fn acpi_platform_config") && src.contains("None"),
        "riscv64 must emit no ACPI platform descriptor; got:\n{src}"
    );
    // And must not emit spurious platform_acpi tokens.
    assert!(
        !src.contains("PlatformConfig::Arm"),
        "riscv64 must not reference Arm platform config; got:\n{src}"
    );
}

#[test]
fn smbios_prepare_emits_real_body_on_sbsa() {
    // qemu-sbsa has `SmBiosPrepare` + a populated `smbios` RON
    // config.  The adapter must expose only the SmbiosDesc literal;
    // runtime owns the call to smbios::prepare.
    let src = adapter_source_for_board("qemu-sbsa");
    assert!(
        !src.contains("fstart_capabilities::smbios::prepare"),
        "adapter must not call the smbios capability fn; got:\n{src}"
    );
    assert!(
        src.contains("fn smbios_desc"),
        "adapter must expose primitive smbios_desc; got:\n{src}"
    );
    assert!(
        src.contains("SmbiosDesc"),
        "smbios_prepare must emit the SmbiosDesc literal; got:\n{src}"
    );
}

#[test]
fn smbios_prepare_unreachable_on_boards_without_smbios_config() {
    // qemu-riscv64 has no `smbios` config and no SmBiosPrepare
    // capability, so the primitive returns no descriptor.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("fn smbios_desc") && src.contains("None"),
        "riscv64 must emit a no-descriptor smbios primitive; got:\n{src}"
    );
    assert!(
        !src.contains("fstart_capabilities::smbios::prepare"),
        "riscv64 must not emit smbios::prepare call; got:\n{src}"
    );
}

// ===== acpi_load + memory_detect adapter tests ====================

#[test]
fn acpi_load_emits_real_body_on_q35() {
    // qemu-q35 declares `AcpiLoad(device: "fw_cfg0")` and the
    // `fw_cfg0` device provides `AcpiTableProvider`. The adapter
    // must expose primitive provider dispatch and RSDP state publication;
    // runtime owns the buffer and acpi_load call.
    let src = adapter_source_for_board("qemu-q35");
    assert!(
        !src.contains("fstart_capabilities::acpi_load"),
        "adapter must not call the ACPI load capability fn; got:\n{src}"
    );
    assert!(
        !src.contains("_ACPI_LOAD_BUF"),
        "adapter must not own the ACPI load buffer; got:\n{src}"
    );
    assert!(
        src.contains("fn with_acpi_table_provider") && src.contains("fn set_acpi_rsdp_addr"),
        "adapter must expose primitive ACPI provider/state methods; got:\n{src}"
    );
    // Device name is baked in.
    assert!(
        src.contains("\"fw_cfg0\""),
        "acpi_load arm must pass the service-selected device name; got:\n{src}"
    );
}

#[test]
fn memory_detect_emits_real_body_on_q35() {
    // qemu-q35 has a service-selected MemoryDetector. The adapter exposes
    // primitive detector borrowing; runtime owns the E820 buffer and
    // memory_detect call.
    let src = adapter_source_for_board("qemu-q35");
    assert!(
        !src.contains("fstart_capabilities::memory_detect"),
        "adapter must not call the memory_detect capability fn; got:\n{src}"
    );
    assert!(
        !src.contains("E820Entry::zeroed()"),
        "adapter must not own the E820 buffer; got:\n{src}"
    );
    assert!(
        src.contains("fn with_memory_detector"),
        "adapter must expose primitive memory detector borrowing; got:\n{src}"
    );
}

#[test]
fn acpi_and_memory_detect_halt_on_non_x86_boards() {
    // qemu-riscv64 has no AcpiTableProvider and does not declare
    // MemoryDetect.  The bodies are real but degenerate to halt paths. Must
    // still compile and must not reference ACPI / E820 symbols beyond the match
    // block's closing brace.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("fn with_acpi_table_provider") && src.contains("RuntimeError::UnknownDevice"),
        "non-ACPI board's provider primitive must reject unknown ids; got:\n{src}"
    );
    assert!(
        src.contains("fn with_memory_detector") && src.contains("RuntimeError::UnknownDevice"),
        "non-detect board's detector primitive must reject unknown ids; got:\n{src}"
    );
    // ACPI buffer must NOT appear in boards with no provider.
    assert!(
        !src.contains("_ACPI_LOAD_BUF"),
        "non-ACPI board must not emit the ACPI buffer; got:\n{src}"
    );
    assert!(
        !src.contains("E820Entry::zeroed()"),
        "non-detect board must not emit the e820 buffer; got:\n{src}"
    );
}

#[test]
fn board_struct_carries_acpi_rsdp_field() {
    // Every board's _BoardDevices carries the `_acpi_rsdp_addr`
    // field so the struct shape is stable across boards that do
    // and don't use AcpiLoad.
    for board in ["qemu-riscv64", "qemu-aarch64", "qemu-armv7"] {
        let src = adapter_source_for_board(board);
        assert!(
            src.contains("_acpi_rsdp_addr: u64"),
            "{board}'s _BoardDevices must declare _acpi_rsdp_addr; got:\n{src}"
        );
        // And new() must const-init it to 0.
        assert!(
            src.contains("_acpi_rsdp_addr: 0"),
            "{board}'s _BoardDevices::new() must initialise _acpi_rsdp_addr to 0; got:\n{src}"
        );
    }
}

// ===== pci_init adapter tests ======================================

#[test]
fn pci_init_emits_real_body_on_aarch64_sbsa() {
    // qemu-sbsa has a service-selected PCI root. The adapter must expose
    // primitive PCI-root borrowing; runtime owns the init call and banner.
    let src = adapter_source_for_board("qemu-sbsa");
    assert!(
        !src.contains("PCI init complete"),
        "adapter must not own PCI init banner/policy; got:\n{src}"
    );
    assert!(
        src.contains("fn with_pci_root"),
        "adapter must expose primitive PCI root borrowing; got:\n{src}"
    );
    assert!(
        src.contains("\"pci0\""),
        "pci_init arm must bake the service-selected device name; got:\n{src}"
    );
}

#[test]
fn pci_init_boards_without_pci_root_have_wildcard_only() {
    // qemu-riscv64 has no PciRootBus provider. The primitive dispatcher has
    // no concrete arms and must not emit PCI policy.
    let src = adapter_source_for_board("qemu-riscv64");
    // The match still exists (empty arm set).  What matters is
    // we do not reference any PCI identifier or "PCI init
    // complete" banner here.
    assert!(
        !src.contains("PCI init complete"),
        "non-pci board must not emit PCI banner; got:\n{src}"
    );
}

#[test]
fn firmware_image_scratch_and_mp_microcode_use_boot_media_state() {
    let src = adapter_source_for_stage("foxconn-d41s", "ramstage");
    assert!(
        src.contains("TempRamArena::new") && !src.contains("copy_firmware_image_to_ram"),
        "provider-backed BootMedia temp_ram_buffer must initialize scratch RAM, not copy the whole image; got:\n{src}"
    );
    assert!(
        src.contains("fn firmware_image") && src.contains("fn set_boot_media_state"),
        "provider firmware-image lookup and boot-media state publication must be primitive Board methods; got:\n{src}"
    );
    assert!(
        src.contains("image.translate(offset)"),
        "MP microcode lookup must derive its blob address from the active FirmwareImage mapping; got:\n{src}"
    );
    assert!(
        src.contains("fn ffs_anchor") && src.contains("FSTART_ANCHOR"),
        "adapter must expose primitive FFS anchor bytes for runtime context publication; got:\n{src}"
    );
}

// ===== return_to_fel adapter tests ================================

#[test]
fn return_to_fel_stays_unreachable_for_boards_without_capability() {
    // No fixture board today actively declares `ReturnToFel` in
    // any stage's capabilities (orangepi-r1 has the entry
    // commented out).  Every adapter must therefore emit the
    // unreachable body that skips referencing `fstart_soc_sunxi`
    // — the crate is only pulled into the dependency graph via
    // the `sunxi` feature on sunxi boards, and non-sunxi armv7
    // boards like `qemu-armv7` would fail to compile if we
    // emitted the real `fstart_soc_sunxi::...` call.
    for board in ["qemu-riscv64", "qemu-aarch64", "qemu-armv7"] {
        let src = adapter_source_for_board(board);
        assert!(
            !src.contains("fstart_soc_sunxi"),
            "{board} does not declare ReturnToFel; adapter must not reference \
                 fstart_soc_sunxi; got:\n{src}"
        );
        assert!(
            src.contains("board_gen::return_to_fel: stage does not declare ReturnToFel"),
            "{board} return_to_fel must be the dead-code unreachable body; got:\n{src}"
        );
    }
}

// ===== stage_load adapter tests ===================================

#[test]
fn stage_load_bootblock_emits_real_body() {
    // qemu-riscv64-multi's bootblock: ConsoleInit + BootMedia +
    // SigVerify + StageLoad("main"). The adapter must expose primitive
    // StageLoad descriptor/anchor data; runtime owns FFS loading.
    let src = adapter_source_for_stage("qemu-riscv64-multi", "bootblock");
    assert!(
        !src.contains("fstart_capabilities::stage_load"),
        "adapter must not call the StageLoad capability fn; got:\n{src}"
    );
    assert!(
        src.contains("fn stage_load_desc"),
        "adapter must expose primitive StageLoad descriptor; got:\n{src}"
    );
    // Anchor preamble is present for this FFS stage; runtime StageLoad uses
    // the primitive anchor accessor rather than generated dispatch flow.
    assert!(src.contains("&FSTART_ANCHOR"));
    assert!(
        !src.contains("stage_load: capability returned without jumping"),
        "adapter must not own StageLoad return policy; got:\n{src}"
    );
}

#[test]
fn stage_load_unreachable_for_non_ffs_stages() {
    // A stage without FFS capabilities has no FSTART_ANCHOR static and no
    // boot-media import path. Runtime will never dispatch StageLoad for this
    // stage, so the adapter exposes no StageLoad descriptor.
    //
    // qemu-riscv64-multi's `main` stage is the canonical non-FFS
    // stage in the fixture set.
    let src = adapter_source_for_stage("qemu-riscv64-multi", "main");
    // No FSTART_ANCHOR referenced anywhere in this stage.
    assert!(
        !src.contains("&FSTART_ANCHOR"),
        "non-FFS stage must not reference FSTART_ANCHOR; got:\n{src}"
    );
    // No generated StageLoad body remains; the descriptor primitive is absent.
    assert!(
        src.contains("fn stage_load_desc") && src.contains("None"),
        "non-FFS stage must expose no StageLoad descriptor; got:\n{src}"
    );
}
