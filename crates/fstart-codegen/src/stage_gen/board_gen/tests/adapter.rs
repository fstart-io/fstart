use super::{adapter_source_for_board, adapter_source_for_stage};

#[test]
fn adapter_compiles_for_qemu_riscv64() {
    let src = adapter_source_for_board("qemu-riscv64");
    // Structure checks — everything a downstream compile of the
    // generated file will need must be present.
    assert!(src.contains("struct _BoardDevices"));
    assert!(src.contains("impl _BoardDevices"));
    assert!(src.contains("impl fstart_stage_runtime::Board for _BoardDevices"));
    assert!(
        src.contains("const fn new(_handoff: Option<fstart_types::handoff::StageHandoff>) -> Self")
    );
    // At least the NS16550 console device should become a field.
    assert!(src.contains("uart0: Option<Ns16550>"));
    // Adapter carries its boot-media state and FDT / DRAM / handoff
    // bookkeeping fields that drive the migrated trampolines.
    assert!(src.contains("_boot_media: fstart_stage_runtime::BootMediaState"));
    assert!(src.contains("_dtb_dst_addr: u64"));
    assert!(src.contains("_bootargs: &'static str"));
    assert!(src.contains("_dram_base: u64"));
    assert!(src.contains("_dram_size_static: u64"));
    assert!(src.contains("_handoff: Option<fstart_types::handoff::StageHandoff>"));
    // Trivial trampolines wired to the real capability helpers.
    assert!(src.contains("fstart_capabilities::memory_init()"));
    // Platform firmware-image setup is real — writes state.
    assert!(src.contains("boot_media_platform_firmware_image"));
    // qemu-riscv64 uses FFS (SigVerify/PayloadLoad), so `sig_verify`
    // is the real body.
    assert!(src.contains("fstart_capabilities::sig_verify"));
    assert!(src.contains("FirmwareImageMap::new"));
    assert!(
        !src.contains("board_gen::sig_verify: placeholder"),
        "qemu-riscv64 uses FFS; sig_verify must have a real body, got:\n{src}"
    );
    // qemu-riscv64 has a LinuxBoot payload with FdtSource::Platform,
    // so the body is `fdt_prepare_platform` with a runtime
    // `boot_dtb_addr()` call (RISC-V / AArch64 default) and the
    // shared DRAM-size-with-handoff expression.
    assert!(src.contains("fstart_capabilities::fdt_prepare_platform"));
    assert!(src.contains("fstart_platform::boot_dtb_addr()"));
    assert!(src.contains("self._dtb_dst_addr"));
    assert!(src.contains("self._bootargs"));
    assert!(src.contains("self._dram_base"));
    // The handoff-aware DRAM-size expression must read from the
    // field (rather than inline a constant from the method body).
    // prettyplease may break `self._handoff` across lines, so the
    // `._handoff` token alone is a reliable indicator.
    assert!(src.contains("._handoff"));
    assert!(src.contains("_dram_size_static"));
    assert!(
        !src.contains("board_gen::fdt_prepare: placeholder"),
        "qemu-riscv64 has FdtSource::Platform; fdt_prepare must have a real body, got:\n{src}"
    );
    // install_logger is a real body: the ConsoleInit id arm
    // calls `fstart_log::init` + `console_ready`.
    assert!(
        src.contains("fstart_log::init"),
        "install_logger must call fstart_log::init on the console device, got:\n{src}"
    );
    assert!(
        src.contains("fstart_capabilities::console_ready"),
        "install_logger must emit console_ready banner, got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::install_logger: placeholder"),
        "install_logger must have a real body, got:\n{src}"
    );
    // payload_load is real: qemu-riscv64 has LinuxBoot → firmware
    // load + kernel load + platform boot protocol.
    assert!(
        src.contains("fstart_capabilities::load_ffs_file_by_type"),
        "payload_load must call load_ffs_file_by_type for kernel/firmware; got:\n{src}"
    );
    assert!(
        src.contains("fstart_platform::boot_linux"),
        "riscv64 payload_load must use boot_linux; got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::payload_load: placeholder"),
        "payload_load must have a real body; got:\n{src}"
    );
    // init_device + init_all_devices are now migrated too — all
    // Board methods have real bodies.  No `placeholder`
    // marker should remain anywhere in the generated source.
    assert!(
        !src.contains("placeholder"),
        "all Board methods must have real bodies now; got:\n{src}"
    );
}

#[test]
fn adapter_compiles_for_qemu_aarch64() {
    let src = adapter_source_for_board("qemu-aarch64");
    assert!(src.contains("struct _BoardDevices"));
    assert!(src.contains("uart0: Option<Pl011>"));
    // aarch64 qemu board also uses FFS.
    assert!(src.contains("fstart_capabilities::sig_verify"));
}

#[test]
fn adapter_compiles_for_qemu_armv7() {
    let src = adapter_source_for_board("qemu-armv7");
    assert!(src.contains("struct _BoardDevices"));
    // armv7 uses halt from fstart_platform (re-exported from fstart_arch).
    assert!(src.contains("fstart_platform::halt()"));
    // qemu-armv7 uses a PL011 UART (not NS16550).
    assert!(src.contains("uart0: Option<Pl011>"));
}

#[test]
fn x86_adapter_has_jump_to_with_handoff_fallback() {
    // On x86_64, fstart_platform has no jump_to_with_handoff; the
    // emitter must substitute halt() so the trait impl still
    // type-checks in downstream firmware builds.
    let src = adapter_source_for_board("qemu-q35");
    assert!(src.contains("impl fstart_stage_runtime::Board for _BoardDevices"));
    // Ensure we did not emit a call to the missing symbol.
    assert!(
        !src.contains("fstart_platform::jump_to_with_handoff"),
        "x86 adapter must not reference the non-existent jump_to_with_handoff; got:\n{src}"
    );
}

#[test]
fn bootblock_without_driver_init_excludes_bus_children() {
    // Pick a multi-stage board whose first stage lacks
    // `DriverInit`.  `qemu-riscv64-multi` is a good example:
    // its bootblock only does ConsoleInit + SigVerify + StageLoad.
    let src = adapter_source_for_board("qemu-riscv64-multi");
    assert!(src.contains("struct _BoardDevices"));
    // The exact set of fields depends on the board; this test is
    // a smoke test that the filter did not panic or emit an
    // unparseable struct.
    // Bootblock uses SigVerify, so it uses FFS and sig_verify is
    // real.
    assert!(src.contains("fstart_capabilities::sig_verify"));
}

#[test]
fn bootblock_without_driver_init_keeps_capability_referenced_child() {
    // Foxconn's bootblock intentionally omits DriverInit, but ConsoleInit
    // targets a SuperIO child under the LPC bus. Capability targets must be
    // materialised even when unrelated bus children are excluded.
    let src = adapter_source_for_stage("foxconn-d41s", "bootblock");
    assert!(
        src.contains("superio: Option"),
        "bootblock adapter must keep ConsoleInit child; got:\n{src}"
    );
    assert!(src.contains("fn dram_init"));
}

#[test]
fn sig_verify_stub_for_non_ffs_stages() {
    // Pick a multi-stage board's non-FFS stage.  The `main` stage
    // of `qemu-riscv64-multi` is ConsoleInit + MemoryInit +
    // DriverInit — no SigVerify/StageLoad/PayloadLoad. So
    // `sig_verify` stays a dead-code unreachable body because
    // FSTART_ANCHOR does not exist in that stage's generated source.
    let src = adapter_source_for_stage("qemu-riscv64-multi", "main");
    assert!(src.contains("struct _BoardDevices"));
    // No FFS ⇒ sig_verify body is unreachable — referencing
    // FSTART_ANCHOR here would break compilation.
    assert!(
        !src.contains("&FSTART_ANCHOR"),
        "non-FFS stage must not reference FSTART_ANCHOR; got:\n{src}"
    );
    assert!(
        src.contains("board_gen::sig_verify: no FFS-using capability"),
        "expected no-FFS sig_verify stub, got:\n{src}"
    );
}

#[test]
fn sunxi_board_sig_verify_has_block_device_arm() {
    // `orangepi-pc2` boots from SD/MMC (`sunxi-mmc`, providing
    // `BlockDevice`) via `LoadNextStage` on the bootblock.  Its
    // `main` stage uses `SigVerify` against a firmware image backed by
    // a Rust-selected block device. The emitted `sig_verify` match must
    // have a FirmwareImageBlock arm that references the `mmc0` field.
    let src = adapter_source_for_stage("orangepi-pc2", "main");
    assert!(src.contains("fstart_capabilities::sig_verify"));
    assert!(
        src.contains("BlockDeviceMedia::new"),
        "sunxi stage using BlockDevice must construct BlockDeviceMedia, got:\n{src}"
    );
    // `prettyplease` may break `self.mmc0` across lines, so
    // check for the tokens separately.  The `.mmc0` reference
    // on its own is a reliable indicator of the block-device
    // arm since no other construct in the emitted adapter would
    // produce that string.
    assert!(
        src.contains(".mmc0"),
        "block-device arm must reference the mmc0 field, got:\n{src}"
    );
}

// ===== fdt_prepare adapter tests =================================

#[test]
fn fdt_prepare_platform_uses_handoff_aware_dram_size() {
    // qemu-riscv64 is the canonical FdtSource::Platform case.
    // Its body must read every board-level fact from `&self` and
    // use the runtime `boot_dtb_addr()` call (RISC-V default).
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("fstart_capabilities::fdt_prepare_platform"),
        "Platform variant must call fdt_prepare_platform; got:\n{src}"
    );
    assert!(
        src.contains("fstart_platform::boot_dtb_addr()"),
        "RISC-V Platform FdtSource must use runtime boot_dtb_addr(); got:\n{src}"
    );
    // No inlined hex constants for DTB dst / bootargs / DRAM —
    // per invariant #3 those all live in fields on `_BoardDevices`.
    assert!(src.contains("_dtb_dst_addr"));
    assert!(src.contains("_bootargs"));
    assert!(src.contains("_dram_base"));
    // The handoff-aware size expression must be emitted (splits
    // across lines in prettyplease, so check for its pieces).
    assert!(src.contains("._handoff"));
    assert!(src.contains("_dram_size_static"));
}

#[test]
fn fdt_prepare_override_loads_from_ffs_on_sunxi_main() {
    // orangepi-pc2 main stage: FdtSource::Override("…dtb") over
    // a block-device boot medium (SD/MMC from the bootblock's
    // LoadNextStage).  The adapter must emit:
    //
    // - anchor-bytes preamble (&FSTART_ANCHOR)
    // - boot-media match with a FirmwareImageBlock arm mentioning .mmc0
    // - load_ffs_file_by_type call for FileType::Fdt
    // - fdt_prepare_platform(dst, dst, ...) patch call
    let src = adapter_source_for_stage("orangepi-pc2", "main");
    assert!(
        src.contains("load_ffs_file_by_type"),
        "Override FDT variant must load via load_ffs_file_by_type; got:\n{src}"
    );
    assert!(
        src.contains("fstart_types :: ffs :: FileType :: Fdt")
            || src.contains("ffs::FileType::Fdt"),
        "Override FDT variant must reference FileType::Fdt; got:\n{src}"
    );
    assert!(
        src.contains("fstart_capabilities::fdt_prepare_platform"),
        "Override FDT variant must still call fdt_prepare_platform for bootargs \
             patching; got:\n{src}"
    );
    assert!(
        src.contains("&FSTART_ANCHOR"),
        "Override FDT variant requires FFS stage; must reference FSTART_ANCHOR; \
             got:\n{src}"
    );
}

// ===== payload_load adapter tests =================================
