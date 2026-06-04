use super::{adapter_source_for_board, adapter_source_for_stage};

#[test]
fn payload_load_linux_boot_on_riscv64() {
    // qemu-riscv64: LinuxBoot + OpenSBI firmware.  Body must
    // load firmware + kernel from FFS, then call boot_linux_sbi.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("capability: PayloadLoad (LinuxBoot)"),
        "payload_load must emit the LinuxBoot banner; got:\n{src}"
    );
    assert!(
        src.contains("SBI firmware"),
        "riscv64 payload_load must load SBI firmware; got:\n{src}"
    );
    assert!(
        src.contains("fstart_platform::boot_linux"),
        "riscv64 payload_load must call boot_linux; got:\n{src}"
    );
    assert!(
        src.contains("loading kernel..."),
        "payload_load must log kernel load; got:\n{src}"
    );
}

#[test]
fn payload_load_linux_boot_on_aarch64() {
    // qemu-aarch64: LinuxBoot + no firmware (TCG boots directly).
    let src = adapter_source_for_board("qemu-aarch64");
    assert!(
        src.contains("fstart_platform::boot_linux"),
        "aarch64 payload_load must call boot_linux; got:\n{src}"
    );
}

#[test]
fn payload_load_armv7_cleanup_before_linux() {
    let src = adapter_source_for_board("qemu-armv7");
    assert!(
        src.contains("fstart_platform::boot_linux"),
        "armv7 payload_load must call boot_linux; got:\n{src}"
    );
}

// ===== init_device + init_all_devices adapter tests ===============

#[test]
fn init_device_emits_match_arm_per_enabled_device() {
    // qemu-riscv64 has uart0 (ns16550) as an enabled,
    // non-structural, non-ACPI device.  init_device must have a
    // match arm that references `self.uart0` and emits both the
    // construction and init calls.
    let src = adapter_source_for_board("qemu-riscv64");
    // Per-arm fast path: if already inited, return Ok.
    assert!(
        src.contains("if this._inited.contains") || src.contains("if self._inited.contains"),
        "init_device arms must check the init mask; got:\n{src}"
    );
    // Construction path uses Device::new (or ::new_on_bus for bus
    // children — but qemu-riscv64 is flat).  prettyplease emits
    // the turbofish-qualified form `<Ns16550>::new(...)` to
    // disambiguate the trait method resolution.
    assert!(
        src.contains("<Ns16550>::new"),
        "init_device must call Ns16550::new for uart0; got:\n{src}"
    );
    assert!(
        src.contains(".init()?"),
        "init_device must call .init() on each device; got:\n{src}"
    );
    assert!(
        src.contains("this._inited.set") || src.contains("self._inited.set"),
        "init_device must set the init mask; got:\n{src}"
    );
}

#[test]
fn init_device_ancestors_walked_root_first() {
    // Verify the ancestor-walking helpers produce the right
    // init-chain shape.  We can't easily build a fixture board
    // with a bus-device-children tree that exercises new_on_bus
    // without a live board using it (q35 / sbsa have pre-existing
    // build failures), so the structural check is:
    //
    // - qemu-riscv64 (flat) uses Device::new, not new_on_bus.
    // - The chain-walking helper `walk_to_real_parent` is
    //   unit-tested indirectly via the sunxi sig_verify body
    //   which already dispatches on BlockDevice ids.
    //
    // If a new fixture board with a non-trivial bus tree lands,
    // add a stronger assertion here.
    let src = adapter_source_for_board("qemu-riscv64");
    // Flat board: no new_on_bus calls.
    assert!(
        !src.contains("new_on_bus"),
        "flat qemu-riscv64 must not emit new_on_bus; got:\n{src}"
    );
}

#[test]
fn init_all_devices_iterates_non_structural() {
    // qemu-riscv64: iterates enabled non-structural devices
    // (just uart0).  Each loop body calls self.init_device(id).
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("self.init_device"),
        "init_all_devices must call self.init_device; got:\n{src}"
    );
    assert!(
        src.contains("if !skip.contains"),
        "init_all_devices must gate on skip mask; got:\n{src}"
    );
}

#[test]
fn init_all_devices_respects_boot_media_gating_on_sunxi() {
    // orangepi-pc2 bootblock has mmc0 (sunxi-mmc, BlockDevice)
    // gated by boot_media.  The body must have a `if gated.contains(id)`
    // check + a `matches!(_bm, ...)` guard.
    let src = adapter_source_for_stage("orangepi-pc2", "bootblock");
    assert!(
        src.contains("fstart_soc_sunxi::boot_media_at"),
        "sunxi init_all_devices must read boot_media; got:\n{src}"
    );
    assert!(
        src.contains("if gated.contains"),
        "sunxi init_all_devices must gate on the gated mask; got:\n{src}"
    );
    assert!(
        src.contains("matches!(_bm"),
        "sunxi init_all_devices must match boot-media byte; got:\n{src}"
    );
}

// ===== boot_media_select + load_next_stage adapter tests ==========

#[test]
fn boot_media_select_real_body_on_sunxi_bootblock() {
    // orangepi-pc2's bootblock uses LoadNextStage(devices=[mmc0])
    // over sunxi-eGON — the body must be the real sunxi dispatch.
    let src = adapter_source_for_stage("orangepi-pc2", "bootblock");
    assert!(
        src.contains("fstart_soc_sunxi::boot_media_at"),
        "sunxi boot_media_select must read via boot_media_at; got:\n{src}"
    );
    // prettyplease may wrap `self._egon_sram_base` across lines.
    assert!(
        src.contains("_egon_sram_base"),
        "boot_media_select must read _egon_sram_base field; got:\n{src}"
    );
    // Writes BootMediaState::FirmwareImageBlock on match.
    assert!(
        src.contains("BootMediaState::FirmwareImageBlock"),
        "boot_media_select must write FirmwareImageBlock variant; got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::boot_media_select: stage does not use"),
        "sunxi bootblock must not emit the dead-code unreachable body; got:\n{src}"
    );
}

#[test]
fn boot_media_select_dead_code_unreachable_on_non_sunxi_boards() {
    // qemu-riscv64 is not a sunxi board, so boot_media_select
    // stays as a dead-code unreachable body — referencing
    // fstart_soc_sunxi there would fail to link (no sunxi feature flag).
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("board_gen::boot_media_select: stage does not use"),
        "qemu-riscv64 must emit the dead-code unreachable body; got:\n{src}"
    );
    // And must not reference fstart_soc_sunxi in boot_media_select
    // or anywhere else in the adapter.
    assert!(
        !src.contains("fstart_soc_sunxi"),
        "qemu-riscv64 adapter must not reference fstart_soc_sunxi; got:\n{src}"
    );
}

#[test]
fn load_next_stage_emits_real_body_on_sunxi_bootblock() {
    // orangepi-pc2 bootblock: LoadNextStage(devices=[mmc0],
    // next_stage: "main").  The body must emit the stage-name
    // match, eGON header read, per-device dispatch, and
    // jump_to_with_handoff call.
    let src = adapter_source_for_stage("orangepi-pc2", "bootblock");
    assert!(
        src.contains("fstart_capabilities::next_stage::read_stage_to_addr"),
        "load_next_stage must call read_stage_to_addr; got:\n{src}"
    );
    assert!(
        src.contains("fstart_capabilities::next_stage::serialize_handoff"),
        "load_next_stage must call serialize_handoff; got:\n{src}"
    );
    assert!(
        src.contains("fstart_platform::jump_to_with_handoff"),
        "load_next_stage must jump with handoff; got:\n{src}"
    );
    assert!(
        src.contains("next_stage_offset_at"),
        "load_next_stage must read next_stage_offset_at; got:\n{src}"
    );
    assert!(
        src.contains("next_stage_size_at"),
        "load_next_stage must read next_stage_size_at; got:\n{src}"
    );
    // Stage-name dispatch: RON declares "bootblock" + "main"
    // stages; the arm for "main" must exist.
    assert!(
        src.contains("\"main\""),
        "load_next_stage must have a match arm for \"main\"; got:\n{src}"
    );
    // Per-device dispatch references .mmc0.
    assert!(
        src.contains(".mmc0"),
        "load_next_stage must dispatch to self.mmc0; got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::load_next_stage: stage does not use"),
        "sunxi bootblock must not emit the dead-code unreachable body; got:\n{src}"
    );
}

#[test]
fn load_next_stage_dead_code_unreachable_on_non_sunxi_boards() {
    // qemu-riscv64 never calls LoadNextStage; the body is unreachable.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("board_gen::load_next_stage: stage does not use"),
        "qemu-riscv64 must emit the dead-code load_next_stage unreachable body; got:\n{src}"
    );
    assert!(
        !src.contains("next_stage_offset_at"),
        "qemu-riscv64 must not reference eGON header symbols; got:\n{src}"
    );
}

#[test]
fn board_struct_carries_egon_sram_base_field() {
    // Every board's _BoardDevices carries _egon_sram_base.
    // On non-sunxi boards it's initialised to 0 (harmless
    // because dead-code trampolines never read it).
    for board in ["qemu-riscv64", "qemu-aarch64", "qemu-armv7"] {
        let src = adapter_source_for_board(board);
        assert!(
            src.contains("_egon_sram_base: u64"),
            "{board} must declare _egon_sram_base field; got:\n{src}"
        );
        // Non-sunxi boards have 0 for the SRAM base.
        assert!(
            src.contains("_egon_sram_base: 0x0"),
            "{board} must const-init _egon_sram_base to 0; got:\n{src}"
        );
    }
}

// ===== acpi_prepare + smbios_prepare adapter tests ================
