use super::{adapter_source_for_board, adapter_source_for_stage};

#[test]
fn payload_load_linux_boot_on_riscv64() {
    // qemu-riscv64: LinuxBoot + OpenSBI firmware. The adapter must expose
    // only primitive payload data; runtime owns firmware/kernel FFS loading.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("PayloadLoadKind::LinuxBoot"),
        "payload descriptor must identify LinuxBoot; got:\n{src}"
    );
    assert!(
        src.contains("load_firmware: true"),
        "payload descriptor must request firmware loading; got:\n{src}"
    );
    assert!(
        src.contains("fstart_platform::boot_linux"),
        "riscv64 payload_load must call boot_linux; got:\n{src}"
    );
    assert!(
        !src.contains("loading kernel..."),
        "adapter must not own kernel load flow; got:\n{src}"
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

#[test]
fn payload_load_uefi_exposes_primitives_only() {
    let src = adapter_source_for_stage("qemu-q35-uefi", "main");
    assert!(
        src.contains("PayloadLoadKind::Uefi") && src.contains("UefiPayloadDesc"),
        "UEFI adapter must expose primitive launch data; got:\n{src}"
    );
    assert!(
        src.contains("fn with_uefi_services"),
        "UEFI adapter must expose service borrows; got:\n{src}"
    );
    assert!(
        !src.contains("launch_x86_uefi") && !src.contains("launch_flat_uefi"),
        "CrabEFI launch policy belongs to fstart-stage-runtime; got:\n{src}"
    );
    assert!(
        !src.contains("Launching CrabEFI UEFI payload"),
        "UEFI payload sequencing/logging belongs to fstart-stage-runtime; got:\n{src}"
    );
}

// ===== construct_device adapter tests ===============

#[test]
fn construct_device_emits_match_arm_per_enabled_device() {
    // qemu-riscv64 has uart0 (ns16550) as an enabled,
    // non-structural, non-ACPI device. construct_device must have a
    // match arm that references `self.uart0` and emits both the
    // construction and init calls.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(src.contains("fn construct_device"));
    assert!(!src.contains("fn init_device"));
    // Per-arm fast path: if already inited, return Ok.
    assert!(
        src.contains("if this._inited.contains") || src.contains("if self._inited.contains"),
        "construct_device arms must check the adapter init mask; got:\n{src}"
    );
    // Construction path uses Device::new (or ::new_on_bus for bus
    // children — but qemu-riscv64 is flat).  prettyplease emits
    // the turbofish-qualified form `<Ns16550>::new(...)` to
    // disambiguate the trait method resolution.
    assert!(
        src.contains("<Ns16550>::new"),
        "construct_device must call Ns16550::new for uart0; got:\n{src}"
    );
    assert!(
        src.contains(".init()?"),
        "construct_device must call .init() on each device; got:\n{src}"
    );
    assert!(
        src.contains("this._inited.set") || src.contains("self._inited.set"),
        "construct_device must set the adapter init mask; got:\n{src}"
    );
}

#[test]
fn construct_device_does_not_emit_root_walk_for_flat_board() {
    // Runtime owns root-first traversal over StagePlan device-init chains.
    // The generated adapter only constructs the requested concrete device.
    let src = adapter_source_for_board("qemu-riscv64");
    // Flat board: no new_on_bus calls.
    assert!(
        !src.contains("new_on_bus"),
        "flat qemu-riscv64 must not emit new_on_bus; got:\n{src}"
    );
}

#[test]
fn board_adapter_does_not_emit_driver_init_flow() {
    let src = adapter_source_for_stage("orangepi-pc2", "bootblock");
    assert!(
        !src.contains("fn init_all_devices"),
        "DriverInit flow belongs to fstart-stage-runtime, not the board adapter: {src}"
    );
    assert!(
        !src.contains("if !skip.contains"),
        "DriverInit skip policy must not be generated into the board adapter: {src}"
    );
    assert!(
        !src.contains("if gated.contains"),
        "DriverInit boot-media gating policy must not be generated into the board adapter: {src}"
    );
}

// ===== soc_boot_media + load_next_stage adapter tests ==========

#[test]
fn soc_boot_media_reads_sunxi_boot_source() {
    // orangepi-pc2's bootblock uses LoadNextStage(devices=[mmc0])
    // over sunxi-eGON — the board adapter exposes only the primitive
    // BROM-written boot source byte. Matching lives in the executor.
    let src = adapter_source_for_stage("orangepi-pc2", "bootblock");
    assert!(
        src.contains("fn soc_boot_media"),
        "Board impl must expose soc_boot_media; got:\n{src}"
    );
    assert!(
        src.contains("fstart_soc_sunxi::boot_media_at"),
        "sunxi soc_boot_media must read via boot_media_at; got:\n{src}"
    );
    assert!(
        src.contains("_egon_sram_base"),
        "soc_boot_media must read _egon_sram_base field; got:\n{src}"
    );
    assert!(
        !src.contains(
            "self._boot_media = fstart_stage_runtime::BootMediaState::FirmwareImageBlock"
        ),
        "soc_boot_media must not mutate boot-media state; got:\n{src}"
    );
}

#[test]
fn soc_boot_media_is_none_on_non_sunxi_boards() {
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("fn soc_boot_media") && src.contains("None"),
        "qemu-riscv64 must emit a None soc_boot_media primitive; got:\n{src}"
    );
    assert!(
        !src.contains("fstart_soc_sunxi"),
        "qemu-riscv64 adapter must not reference fstart_soc_sunxi; got:\n{src}"
    );
}

#[test]
fn load_next_stage_emits_real_body_on_sunxi_bootblock() {
    // orangepi-pc2 bootblock: LoadNextStage(devices=[mmc0], next_stage:
    // "main"). The adapter must expose primitive stage address/eGON/block
    // dispatch data; runtime owns read/serialize/jump sequencing.
    let src = adapter_source_for_stage("orangepi-pc2", "bootblock");
    assert!(
        !src.contains("fstart_capabilities::next_stage::read_stage_to_addr"),
        "adapter must not read next stage directly; got:\n{src}"
    );
    assert!(
        !src.contains("fstart_capabilities::next_stage::serialize_handoff"),
        "adapter must not serialize handoff directly; got:\n{src}"
    );
    assert!(
        src.contains("fn jump_to_with_handoff"),
        "adapter must expose final jump-with-handoff primitive; got:\n{src}"
    );
    assert!(
        src.contains("next_stage_offset_at"),
        "adapter must expose eGON next_stage_offset_at scalar; got:\n{src}"
    );
    assert!(
        src.contains("next_stage_size_at"),
        "adapter must expose eGON next_stage_size_at scalar; got:\n{src}"
    );
    // Stage-name dispatch: RON declares "bootblock" + "main"
    // stages; the arm for "main" must exist.
    assert!(
        src.contains("\"main\""),
        "next_stage_addr must have a match arm for \"main\"; got:\n{src}"
    );
    // Per-device dispatch references .mmc0.
    assert!(
        src.contains(".mmc0"),
        "block-device primitive must dispatch to self.mmc0; got:\n{src}"
    );
    assert!(
        src.contains("fn next_stage_addr") && src.contains("fn egon_next_stage"),
        "sunxi bootblock must expose next-stage primitives; got:\n{src}"
    );
}

#[test]
fn load_next_stage_dead_code_unreachable_on_non_sunxi_boards() {
    // qemu-riscv64 never calls LoadNextStage; primitive descriptors are absent.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("fn next_stage_addr") && src.contains("None"),
        "qemu-riscv64 must emit no next-stage address descriptor; got:\n{src}"
    );
    assert!(
        !src.contains("next_stage_offset_at"),
        "qemu-riscv64 must not reference eGON header symbols; got:\n{src}"
    );
}

#[test]
fn board_struct_carries_egon_sram_base_only_for_egon_boards() {
    let sunxi = adapter_source_for_stage("orangepi-pc2", "bootblock");
    assert!(
        sunxi.contains("_egon_sram_base: u64"),
        "sunxi/eGON boards must declare _egon_sram_base; got:\n{sunxi}"
    );

    for board in ["qemu-riscv64", "qemu-aarch64", "qemu-armv7"] {
        let src = adapter_source_for_board(board);
        assert!(
            !src.contains("_egon_sram_base"),
            "{board} must not carry sunxi-only _egon_sram_base state; got:\n{src}"
        );
    }
}

// ===== acpi_prepare + smbios_prepare adapter tests ================
