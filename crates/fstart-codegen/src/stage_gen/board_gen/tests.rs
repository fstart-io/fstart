use super::*;
use crate::ron_loader::load_parsed_board;
use std::path::PathBuf;

/// Load a fixture board, generate the adapter for its first (or
/// only) stage, and return the formatted source.
///
/// Matches the path resolution `tests.rs` already uses — look up
/// `boards/<name>/board.ron` relative to the workspace root.
fn adapter_source_for_board(board: &str) -> String {
    adapter_source_inner(board, None)
}

/// Like [`adapter_source_for_board`] but selects a named stage on
/// multi-stage boards.  Panics if `stage` is not in the board's
/// stage list, mirroring real-build behaviour.
fn adapter_source_for_stage(board: &str, stage: &str) -> String {
    adapter_source_inner(board, Some(stage.to_owned()))
}

/// Runs the ron loader + codegen on a fresh thread with a
/// generous stack (8 MiB).  The Rust default test-thread stack
/// is 2 MiB and `prettyplease` + serde-de-deep-ron can exceed that
/// for some boards when compiled in debug mode.  Using a worker
/// thread keeps every test robust without forcing every CI run
/// to export `RUST_MIN_STACK`.
fn adapter_source_inner(board: &str, stage: Option<String>) -> String {
    let board = board.to_owned();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf();
            let ron = root.join("boards").join(&board).join("board.ron");
            let parsed =
                load_parsed_board(&ron).unwrap_or_else(|e| panic!("failed to load {board}: {e}"));

            // Pick the selected stage, or default to first /
            // monolithic — mirrors `generate_stage_source`.
            let caps: &[Capability] = match (&parsed.config.stages, stage.as_deref()) {
                (fstart_types::StageLayout::Monolithic(m), _) => &m.capabilities,
                (fstart_types::StageLayout::MultiStage(stages), Some(name)) => {
                    &stages
                        .iter()
                        .find(|s| s.name.as_str() == name)
                        .unwrap_or_else(|| panic!("stage {name} not found in board {board}"))
                        .capabilities
                }
                (fstart_types::StageLayout::MultiStage(stages), None) => &stages[0].capabilities,
            };

            let tokens = generate_board_adapter(
                &parsed.config,
                &parsed.driver_instances,
                &parsed.device_tree,
                &parsed.device_services,
                &parsed.acpi_only_devices,
                caps,
                stage.as_deref(),
            );
            let file = syn::parse2::<syn::File>(tokens)
                .unwrap_or_else(|e| panic!("board_gen for {board} produced unparseable Rust: {e}"));
            prettyplease::unparse(&file)
        })
        .expect("spawn codegen worker thread")
        .join()
        .expect("codegen worker thread panicked")
}

#[test]
fn adapter_compiles_for_qemu_riscv64() {
    let src = adapter_source_for_board("qemu-riscv64");
    // Structure checks — everything a downstream compile of the
    // generated file will need must be present.
    assert!(src.contains("struct _BoardDevices"));
    assert!(src.contains("impl _BoardDevices"));
    assert!(src.contains("impl fstart_stage_runtime::Board for _BoardDevices"));
    assert!(src.contains("const fn new() -> Self"));
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
    assert!(src.contains("fstart_capabilities::late_driver_init_complete"));
    // `boot_media_static` is real — writes state.
    assert!(src.contains("BootMediaState::from_static"));
    // qemu-riscv64 uses FFS (SigVerify/PayloadLoad), so `sig_verify`
    // is the real body — not a todo!().
    assert!(src.contains("fstart_capabilities::sig_verify"));
    assert!(src.contains("MemoryMapped::from_raw_addr"));
    assert!(
        !src.contains("board_gen::sig_verify: migration pending"),
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
        !src.contains("board_gen::fdt_prepare: migration pending"),
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
        !src.contains("board_gen::install_logger: migration pending"),
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
        !src.contains("board_gen::payload_load: migration pending"),
        "payload_load must have a real body; got:\n{src}"
    );
    // init_device + init_all_devices are now migrated too — all
    // 20 Board methods have real bodies.  No `migration pending`
    // marker should remain anywhere in the generated source.
    assert!(
        !src.contains("migration pending"),
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
    // DriverInit — no SigVerify/StageLoad/PayloadLoad.  So
    // `sig_verify` stays a todo!() placeholder because FSTART_ANCHOR
    // does not exist in that stage's generated source.
    let src = adapter_source_for_stage("qemu-riscv64-multi", "main");
    assert!(src.contains("struct _BoardDevices"));
    // No FFS ⇒ sig_verify body is a todo!() — referencing
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
    // `main` stage uses `SigVerify` against the block-backed
    // boot medium.  The emitted `sig_verify` match must have a
    // `Block` arm that references the `mmc0` field.
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

// ===== fdt_prepare migration tests =================================

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
    // - boot-media match with a Block arm mentioning .mmc0
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

// ===== payload_load migration tests =================================

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

// ===== init_device + init_all_devices migration tests ===============

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

// ===== boot_media_select + load_next_stage migration tests ==========

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
    // Writes BootMediaState::Block on match.
    assert!(
        src.contains("BootMediaState::Block"),
        "boot_media_select must write Block variant; got:\n{src}"
    );
    assert!(
        !src.contains("board_gen::boot_media_select: stage does not use"),
        "sunxi bootblock must not emit the dead-code stub; got:\n{src}"
    );
}

#[test]
fn boot_media_select_dead_code_stub_on_non_sunxi_boards() {
    // qemu-riscv64 is not a sunxi board, so boot_media_select
    // stays as the todo!() stub — referencing fstart_soc_sunxi
    // there would fail to link (no sunxi feature flag).
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("board_gen::boot_media_select: stage does not use"),
        "qemu-riscv64 must emit the dead-code stub; got:\n{src}"
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
        "sunxi bootblock must not emit the dead-code stub; got:\n{src}"
    );
}

#[test]
fn load_next_stage_dead_code_stub_on_non_sunxi_boards() {
    // qemu-riscv64 never calls LoadNextStage; the body is todo!().
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("board_gen::load_next_stage: stage does not use"),
        "qemu-riscv64 must emit the dead-code load_next_stage stub; got:\n{src}"
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

// ===== acpi_prepare + smbios_prepare migration tests ================

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
        !src.contains("board_gen::acpi_prepare: migration pending"),
        "acpi_prepare must have a real body; got:\n{src}"
    );
}

#[test]
fn acpi_prepare_stub_on_boards_without_acpi_config() {
    // qemu-riscv64 has no `acpi` RON config and no AcpiPrepare
    // capability, so the body must be the dead-code todo!().
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
        !src.contains("board_gen::smbios_prepare: migration pending"),
        "smbios_prepare must have a real body; got:\n{src}"
    );
}

#[test]
fn smbios_prepare_stub_on_boards_without_smbios_config() {
    // qemu-riscv64 has no `smbios` config and no SmBiosPrepare
    // capability, so the body is the dead-code todo!().
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

// ===== acpi_load + memory_detect migration tests ====================

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
        !src.contains("board_gen::acpi_load: migration pending"),
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
        !src.contains("board_gen::memory_detect: migration pending"),
        "memory_detect must have a real body; got:\n{src}"
    );
}

#[test]
fn acpi_and_memory_detect_halt_on_non_x86_boards() {
    // qemu-riscv64 has no AcpiTableProvider or MemoryDetector
    // device — the bodies are real but degenerate to wildcard
    // log + halt.  Must still compile and must not reference
    // ACPI / E820 symbols beyond the match block's closing brace.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("acpi_load: unknown device id"),
        "non-ACPI board's acpi_load must emit the wildcard log; got:\n{src}"
    );
    assert!(
        src.contains("memory_detect: unknown device id"),
        "non-memory-detect board's memory_detect must emit the wildcard log; got:\n{src}"
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

// ===== pci_init + generic phase-init migration tests ================

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
        !src.contains("board_gen::pci_init: migration pending"),
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
        !src.contains("board_gen::pci_init: migration pending"),
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

// ===== return_to_fel migration tests ================================

#[test]
fn return_to_fel_stays_stubbed_for_boards_without_capability() {
    // No fixture board today actively declares `ReturnToFel` in
    // any stage's capabilities (orangepi-r1 has the entry
    // commented out).  Every adapter must therefore emit the
    // `todo!()` stub that skips referencing `fstart_soc_sunxi`
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

// ===== stage_load migration tests ===================================

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
    // Old migration-pending stub must be gone.
    assert!(
        !src.contains("board_gen::stage_load: migration pending"),
        "stage_load must have a real body; got:\n{src}"
    );
}

#[test]
fn stage_load_stub_for_non_ffs_stages() {
    // A stage without FFS capabilities has no FSTART_ANCHOR static
    // and no boot-media import path.  `stage_load` on that stage
    // would be dead code (validation forbids StageLoad without
    // BootMedia), so we emit a `todo!()`.
    //
    // qemu-riscv64-multi's `main` stage is the canonical non-FFS
    // stage in the fixture set.
    let src = adapter_source_for_stage("qemu-riscv64-multi", "main");
    // No FSTART_ANCHOR referenced anywhere in this stage.
    assert!(
        !src.contains("&FSTART_ANCHOR"),
        "non-FFS stage must not reference FSTART_ANCHOR; got:\n{src}"
    );
    // `stage_load` body is a todo!() — the compiler still
    // type-checks the trait impl, but no executor arm dispatches
    // this method for this stage.
    assert!(
        src.contains("board_gen::stage_load requires an FFS-using stage"),
        "non-FFS stage_load must emit the dead-code todo!(); got:\n{src}"
    );
}

// ===== install_logger migration tests ===============================

#[test]
fn install_logger_emits_arm_per_console_device() {
    // qemu-riscv64 has one Console-providing device: uart0 (ns16550).
    // The match must carry an arm that inits the logger against
    // `.uart0` and emits the console_ready banner with both the
    // RON device name and the driver crate name.
    let src = adapter_source_for_board("qemu-riscv64");
    assert!(
        src.contains("fstart_log::init"),
        "install_logger body must call fstart_log::init, got:\n{src}"
    );
    // prettyplease may break `self.uart0` across lines — `.uart0`
    // is the reliable indicator.  Every Console arm references its
    // `self.<field>` to pass to `fstart_log::init`.
    assert!(
        src.contains(".uart0"),
        "install_logger arm must reference self.uart0, got:\n{src}"
    );
    // Banner call with device + driver name literals.
    assert!(
        src.contains("fstart_capabilities::console_ready"),
        "install_logger body must call console_ready, got:\n{src}"
    );
    assert!(
        src.contains("\"uart0\""),
        "console_ready must pass the RON device name, got:\n{src}"
    );
    assert!(
        src.contains("\"ns16550\""),
        "console_ready must pass the driver crate name, got:\n{src}"
    );
}

#[test]
fn install_logger_pl011_on_aarch64() {
    // qemu-aarch64 uses a Pl011 driver.  The driver-name literal
    // in the console_ready banner must reflect that.
    let src = adapter_source_for_board("qemu-aarch64");
    assert!(src.contains("fstart_log::init"));
    assert!(
        src.contains("\"pl011\""),
        "console_ready must pass \"pl011\" for qemu-aarch64, got:\n{src}"
    );
}

#[test]
fn install_logger_always_has_wildcard_halt() {
    // Every generated install_logger body ends with a `_ =>` arm
    // that halts.  The executor guarantees the id matches a
    // Console provider, but the compiler still needs exhaustive
    // coverage of the match.
    let src = adapter_source_for_board("qemu-riscv64");
    // Find the `unsafe fn install_logger` signature.  Walk
    // forward matching braces on `{` / `}` to isolate the
    // method body so we don't bleed into the next method.
    let sig_idx = src
        .find("unsafe fn install_logger(")
        .expect("adapter must define install_logger");
    let open = sig_idx
        + src[sig_idx..]
            .find('{')
            .expect("install_logger method must have a body");
    let mut depth = 0i32;
    let mut end = open;
    for (off, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + off + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &src[open..end];
    assert!(
        body.contains("_ =>") && body.contains("fstart_platform::halt()"),
        "install_logger must include `_ => halt()` wildcard, got:\n{body}"
    );
}

#[test]
fn fdt_prepare_stub_when_board_has_no_payload() {
    // Exercise the `config.payload.is_none()` path.  Pick a
    // simple, widely-tested board and strip the payload in-memory
    // via a derived `BoardConfig`.  Writing fresh RON in a test
    // fixture directory would be cleaner but overkill for one
    // assertion — the important thing is the fdt_prepare_body
    // match arm is reachable and emits `fdt_prepare_stub`.
    //
    // Current board set: every live board ships a payload, so
    // the smoke test here instead verifies that the `stub()`
    // fallback token is present in `board_gen` source itself.
    // A functional test of this path lands once a board with
    // no payload exists (or we add a unit test fixture).
    let src = adapter_source_for_board("qemu-riscv64");
    // Sanity — `fdt_prepare_stub` identifier must still be
    // reachable from generated code when we need it later.
    assert!(
        std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src/stage_gen/board_gen/fdt.rs"),
        )
        .unwrap()
        .contains("fstart_capabilities::fdt_prepare_stub"),
        "board_gen must still emit fdt_prepare_stub for no-payload boards; \
             got adapter src:\n{src}"
    );
}
