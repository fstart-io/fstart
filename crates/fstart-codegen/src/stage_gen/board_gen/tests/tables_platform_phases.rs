use super::{adapter_source_for_board, adapter_source_for_stage};

#[test]
fn acpi_prepare_emits_real_body_on_sbsa() {
    // qemu-sbsa has `AcpiPrepare` + a populated `acpi` RON
    // config (ARM SBSA platform).  The body must emit the
    // platform_acpi binding plus the acpi::prepare call with
    // closure.  per-device `_cfg` bindings may or may not be
    // present depending on whether any driver has `has_acpi` +
    // an `acpi_name` set.
    let src = adapter_source_for_board("qemu-sbsa");
    assert!(
        src.contains("let platform_acpi"),
        "acpi_prepare must emit platform_acpi binding; got:\n{src}"
    );
    assert!(
        src.contains("fstart_capabilities::acpi::prepare"),
        "acpi_prepare must call the capability fn; got:\n{src}"
    );
    assert!(
        src.contains("PlatformConfig::Arm"),
        "sbsa acpi_prepare must use the Arm platform variant; got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::acpi_prepare: placeholder"),
        "acpi_prepare must have a real body; got:\n{src}"
    );
}

#[test]
fn acpi_prepare_stub_on_boards_without_acpi_config() {
    // qemu-riscv64 has no `acpi` RON config and no AcpiPrepare
    // capability, so the body must be dead-code unreachable.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("board_gen::acpi_prepare: stage does not declare AcpiPrepare")
            || src.contains("board_gen::acpi_prepare: board has no `acpi` RON config"),
        "riscv64 must emit the no-config/no-cap stub; got:\n{src}"
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
    // config.  The body must call smbios::prepare with the full
    // SmbiosDesc literal.
    let src = adapter_source_for_board("qemu-sbsa");
    assert!(
        src.contains("fstart_capabilities::smbios::prepare"),
        "smbios_prepare must call the capability fn; got:\n{src}"
    );
    assert!(
        src.contains("SmbiosDesc"),
        "smbios_prepare must emit the SmbiosDesc literal; got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::smbios_prepare: placeholder"),
        "smbios_prepare must have a real body; got:\n{src}"
    );
}

#[test]
fn smbios_prepare_stub_on_boards_without_smbios_config() {
    // qemu-riscv64 has no `smbios` config and no SmBiosPrepare
    // capability, so the body is dead-code unreachable.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("board_gen::smbios_prepare: stage does not declare SmBiosPrepare")
            || src.contains("board_gen::smbios_prepare: board has no `smbios` RON config"),
        "riscv64 must emit the smbios no-config/no-cap stub; got:\n{src}"
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
    // `fw_cfg0` device provides `AcpiTableProvider`.  The body
    // must allocate the 256 KiB buffer, call acpi_load, and
    // write the RSDP into `self._acpi_rsdp_addr`.
    let src = adapter_source_for_board("qemu-q35");
    assert!(
        src.contains("fstart_capabilities::acpi_load"),
        "acpi_load must call the capability fn; got:\n{src}"
    );
    assert!(
        src.contains("256 * 1024"),
        "acpi_load must declare the 256 KiB buffer; got:\n{src}"
    );
    assert!(
        src.contains("_ACPI_LOAD_BUF"),
        "acpi_load must use the static buffer symbol; got:\n{src}"
    );
    // RSDP is stored on `self`.
    assert!(
        src.contains("_acpi_rsdp_addr"),
        "acpi_load must write RSDP into self._acpi_rsdp_addr; got:\n{src}"
    );
    // Device name is baked in.
    assert!(
        src.contains("\"fw_cfg0\""),
        "acpi_load arm must pass the RON device name; got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::acpi_load: placeholder"),
        "acpi_load must have a real body; got:\n{src}"
    );
}

#[test]
fn memory_detect_emits_real_body_on_q35() {
    // qemu-q35 declares `MemoryDetect(device: "fw_cfg0")`.  The
    // body must allocate a 128-entry E820 buffer and call
    // memory_detect.
    let src = adapter_source_for_board("qemu-q35");
    assert!(
        src.contains("fstart_capabilities::memory_detect"),
        "memory_detect must call the capability fn; got:\n{src}"
    );
    assert!(
        src.contains("E820Entry::zeroed()"),
        "memory_detect must initialise the buffer with E820Entry::zeroed(); got:\n{src}"
    );
    assert!(
        src.contains("; 128]"),
        "memory_detect buffer must have 128 entries; got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::memory_detect: placeholder"),
        "memory_detect must have a real body; got:\n{src}"
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
        src.contains("acpi_load: unknown device id"),
        "non-ACPI board's acpi_load must emit the wildcard log; got:\n{src}"
    );
    assert!(
        src.contains("memory_detect: stage does not declare MemoryDetect"),
        "non-memory-detect board's memory_detect must emit the undeclared-stage log; got:\n{src}"
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

// ===== pci_init + generic phase-init adapter tests ================

#[test]
fn pci_init_emits_real_body_on_aarch64_sbsa() {
    // qemu-sbsa uses `PciInit(device: "pci0")`.  The adapter must
    // carry an arm that logs the banner and returns Ok(()).
    let src = adapter_source_for_board("qemu-sbsa");
    assert!(
        src.contains("PCI init complete"),
        "pci_init body must log the banner; got:\n{src}"
    );
    assert!(
        src.contains("\"pci0\""),
        "pci_init arm must bake the RON device name; got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::pci_init: placeholder"),
        "pci_init must have a real body; got:\n{src}"
    );
}

#[test]
fn pci_init_boards_without_pci_root_have_wildcard_only() {
    // qemu-riscv64 has no PciRootBus provider.  The body is just
    // the wildcard arm that halts.  It's dead code (executor never
    // dispatches PciInit on this board), but the trait still
    // requires a body.
    let src = adapter_source_for_board("qemu-riscv64");
    // The match still exists (empty arm set).  What matters is
    // we do not reference any PCI identifier or "PCI init
    // complete" banner here.
    assert!(
        !src.contains("PCI init complete"),
        "non-pci board must not emit PCI banner; got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::pci_init: placeholder"),
        "pci_init must have a real body on any board; got:\n{src}"
    );
}

#[test]
fn early_init_emits_generic_phase_calls_on_foxconn_d41s() {
    let src = adapter_source_for_stage("foxconn-d41s", "bootblock");
    assert!(
        src.contains("fstart_services::EarlyInit"),
        "early_init must import EarlyInit trait; got:\n{src}"
    );
    assert!(
        src.contains("_EarlyInit::early_init"),
        "early_init must call EarlyInit::early_init; got:\n{src}"
    );
    assert!(
        !src.contains("fstart_services::PciHost as _PciHost"),
        "generic phase init must not depend on PciHost topology; got:\n{src}"
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
        src.contains("BootMediaState::from_firmware_image") && src.contains("effective_image"),
        "boot media state must record the provider firmware image; got:\n{src}"
    );
    assert!(
        src.contains("image.translate(offset)"),
        "MP microcode lookup must derive its blob address from the active FirmwareImage mapping; got:\n{src}"
    );
    assert!(
        src.contains("fstart_services::ffs_context::set_memory_mapped"),
        "FFS context must publish the effective copied/mapped firmware image; got:\n{src}"
    );
}

#[test]
fn early_init_verifies_declared_flash_layout_on_lenovo_x61() {
    let src = adapter_source_for_stage("lenovo-x61", "bootblock");
    assert!(
        src.contains("fstart_services::FlashLayoutVerifier::verify_flash_layout"),
        "early_init must verify hardware flash layout through the generic service; got:\n{src}"
    );
    assert!(
        src.contains("FlashLayout::IntelIfd(IntelIfdFlashLayout"),
        "expected flash layout must come from board memory.flash_layout; got:\n{src}"
    );
    assert!(
        src.contains("_EarlyInit::early_init(dev)"),
        "early_init must still call the normal phase service after verification; got:\n{src}"
    );
}

#[test]
fn pre_console_init_emits_generic_phase_calls_on_foxconn_d41s() {
    let src = adapter_source_for_stage("foxconn-d41s", "bootblock");
    assert!(
        src.contains("fstart_services::PreConsoleInit"),
        "pre_console_init must import PreConsoleInit trait; got:\n{src}"
    );
    assert!(
        src.contains("_PreConsoleInit::pre_console_init"),
        "pre_console_init must call PreConsoleInit::pre_console_init; got:\n{src}"
    );
}

#[test]
fn phase_init_boards_without_provider_have_wildcard_only() {
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("unknown or unsupported device id"),
        "boards without a phase provider must still emit wildcard errors; got:\n{src}"
    );
}

// ===== return_to_fel adapter tests ================================

#[test]
fn return_to_fel_stays_stubbed_for_boards_without_capability() {
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
            "{board} return_to_fel must be the dead-code stub; got:\n{src}"
        );
    }
}

// ===== stage_load adapter tests ===================================

#[test]
fn stage_load_bootblock_emits_real_body() {
    // qemu-riscv64-multi's bootblock: ConsoleInit + BootMedia +
    // SigVerify + StageLoad("main").  The `stage_load` trampoline
    // must reconstruct the boot medium and call
    // `fstart_capabilities::stage_load`.
    let src = adapter_source_for_stage("qemu-riscv64-multi", "bootblock");
    assert!(
        src.contains("fstart_capabilities::stage_load"),
        "bootblock stage_load must call the capability fn; got:\n{src}"
    );
    // Anchor preamble is present (shared with sig_verify, but the
    // stage_load arm emits its own dispatch body that uses it).
    assert!(src.contains("&FSTART_ANCHOR"));
    // The trailing `halt()` satisfies the `-> !` return type.
    assert!(
        src.contains("stage_load: capability returned without jumping"),
        "stage_load body must log + halt on non-diverging return; got:\n{src}"
    );
    // Old placeholder stub must be gone.
    assert!(
        !src.contains("board_gen::stage_load: placeholder"),
        "stage_load must have a real body; got:\n{src}"
    );
}

#[test]
fn stage_load_stub_for_non_ffs_stages() {
    // A stage without FFS capabilities has no FSTART_ANCHOR static
    // and no boot-media import path.  `stage_load` on that stage
    // would be dead code (validation forbids StageLoad without
    // BootMedia), so we emit an explicit unreachable body.
    //
    // qemu-riscv64-multi's `main` stage is the canonical non-FFS
    // stage in the fixture set.
    let src = adapter_source_for_stage("qemu-riscv64-multi", "main");
    // No FSTART_ANCHOR referenced anywhere in this stage.
    assert!(
        !src.contains("&FSTART_ANCHOR"),
        "non-FFS stage must not reference FSTART_ANCHOR; got:\n{src}"
    );
    // `stage_load` body is unreachable — the compiler still
    // type-checks the trait impl, but no executor arm dispatches
    // this method for this stage.
    assert!(
        src.contains("board_gen::stage_load requires an FFS-using stage"),
        "non-FFS stage_load must emit the dead-code unreachable body; got:\n{src}"
    );
}
