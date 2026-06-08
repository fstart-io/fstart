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
    // Trivial capability flow lives in the runtime executor, not the adapter.
    assert!(!src.contains("fstart_capabilities::memory_init()"));
    // Boot-media state publication and FFS anchor access are primitive Board methods.
    assert!(src.contains("fn set_boot_media_state"));
    assert!(src.contains("fn ffs_anchor"));
    // qemu-riscv64 uses FFS, so the adapter exposes primitive boot-media
    // dispatch while sig_verify orchestration lives in the runtime executor.
    assert!(src.contains("fn with_boot_media"));
    assert!(src.contains("FirmwareImageMap::new"));
    assert!(!src.contains("fstart_capabilities::sig_verify"));
    // qemu-riscv64 has a LinuxBoot payload with FdtSource::Platform,
    // so the adapter exposes a primitive FDT descriptor with a runtime
    // `boot_dtb_addr()` call (RISC-V / AArch64 default) and the shared
    // DRAM-size-with-handoff expression. The runtime owns the capability call.
    assert!(!src.contains("fstart_capabilities::fdt_prepare_platform"));
    assert!(src.contains("fn fdt_prepare_desc"));
    assert!(src.contains("FdtPrepareSource::Platform"));
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
    // install_logger is a real primitive body: the ConsoleInit id arm calls
    // `fstart_log::init` and returns metadata for runtime-owned readiness
    // logging.
    assert!(
        src.contains("fstart_log::init"),
        "install_logger must call fstart_log::init on the console device, got:\n{src}"
    );
    assert!(
        src.contains("ConsoleReady") && !src.contains("fstart_capabilities::console_ready"),
        "install_logger must return ConsoleReady metadata without logging the banner, got:\n{src}"
    );
    // payload_load orchestration lives in runtime: qemu-riscv64 has LinuxBoot,
    // so the adapter exposes only a primitive descriptor and final platform
    // boot primitive.
    assert!(
        !src.contains("fstart_capabilities::load_ffs_file_by_type"),
        "adapter must not load payload files directly; got:\n{src}"
    );
    assert!(src.contains("fn payload_load_desc"));
    assert!(src.contains("PayloadLoadKind::LinuxBoot"));
    assert!(
        src.contains("fstart_platform::boot_linux"),
        "adapter must expose the final Linux boot primitive; got:\n{src}"
    );
    // All Board methods have real bodies. No old placeholder marker
    // should remain anywhere in the generated source.
    assert!(
        !src.contains("placeholder"),
        "all Board methods must have real bodies now; got:\n{src}"
    );
}

#[test]
fn generated_adapter_keeps_runtime_flow_out() {
    let sources = [
        adapter_source_for_stage("qemu-q35-uefi", "main"),
        adapter_source_for_stage("foxconn-d41s", "ramstage"),
        adapter_source_for_stage("orangepi-r1", "bootblock"),
    ];
    let forbidden = [
        "fstart_mp::mp_init",
        "FfsReader::read_anchor_volatile",
        "fstart_capabilities::console_ready",
        "fstart_capabilities::sig_verify",
        "fstart_capabilities::load_ffs_file_by_type",
        "fstart_capabilities::stage_load(",
        "fstart_capabilities::next_stage::read_stage_to_addr",
        "fstart_capabilities::next_stage::serialize_handoff",
        "fstart_capabilities::memory_detect",
        "fstart_capabilities::acpi::prepare_with_options",
        "fstart_capabilities::smbios::prepare",
        "launch_x86_uefi",
        "launch_flat_uefi",
        "GenericX86CpuDriver",
        "Core2CpuDriver",
        "PineviewCpuDriver",
    ];

    for source in sources {
        for token in forbidden {
            assert!(
                !source.contains(token),
                "generated adapter must not contain runtime flow token `{token}`; got:\n{source}"
            );
        }
    }
}

#[test]
fn adapter_compiles_for_qemu_aarch64() {
    let src = adapter_source_for_board("qemu-aarch64");
    assert!(src.contains("struct _BoardDevices"));
    assert!(src.contains("uart0: Option<Pl011>"));
    // aarch64 qemu board also uses FFS through primitive boot-media dispatch.
    assert!(src.contains("fn with_boot_media"));
    assert!(!src.contains("fstart_capabilities::sig_verify"));
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
    // Bootblock uses SigVerify, so it uses FFS through primitive
    // boot-media dispatch.
    assert!(src.contains("fn with_boot_media"));
    assert!(!src.contains("fstart_capabilities::sig_verify"));
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
fn sig_verify_unreachable_for_non_ffs_stages() {
    // Pick a multi-stage board's non-FFS stage.  The `main` stage
    // of `qemu-riscv64-multi` is ConsoleInit + MemoryInit +
    // DriverInit — no SigVerify/StageLoad/PayloadLoad. The adapter
    // must not reference FSTART_ANCHOR; the runtime executor owns
    // the SigVerify flow.
    let src = adapter_source_for_stage("qemu-riscv64-multi", "main");
    assert!(src.contains("struct _BoardDevices"));
    // No FFS ⇒ sig_verify body is unreachable — referencing
    // FSTART_ANCHOR here would break compilation.
    assert!(
        !src.contains("&FSTART_ANCHOR"),
        "non-FFS stage must not reference FSTART_ANCHOR; got:\n{src}"
    );
    assert!(!src.contains("fstart_capabilities::sig_verify"));
}

#[test]
fn sunxi_board_sig_verify_has_block_device_arm() {
    // `orangepi-pc2` boots from SD/MMC (`sunxi-mmc`, providing
    // `BlockDevice`) via `LoadNextStage` on the bootblock.  Its
    // `main` stage uses `SigVerify` against a firmware image backed by
    // a Rust-selected block device. The emitted `sig_verify` match must
    // have a FirmwareImageBlock arm that references the `mmc0` field.
    let src = adapter_source_for_stage("orangepi-pc2", "main");
    assert!(!src.contains("fstart_capabilities::sig_verify"));
    assert!(src.contains("fn with_boot_media"));
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
        !src.contains("fstart_capabilities::fdt_prepare_platform"),
        "adapter must not call fdt_prepare_platform; got:\n{src}"
    );
    assert!(
        src.contains("fn fdt_prepare_desc") && src.contains("FdtPrepareSource::Platform"),
        "Platform variant must expose an FDT descriptor; got:\n{src}"
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
    // - primitive Override descriptor
    // - boot-media primitive support with a FirmwareImageBlock arm mentioning .mmc0
    // - no generated FFS-load or fdt_prepare_platform orchestration
    let src = adapter_source_for_stage("orangepi-pc2", "main");
    assert!(
        src.contains("FdtPrepareSource::Override"),
        "Override FDT variant must expose primitive Override descriptor; got:\n{src}"
    );
    assert!(
        !src.contains("FileType::Fdt"),
        "adapter must not load override FDT from FFS; got:\n{src}"
    );
    assert!(
        !src.contains("fstart_capabilities::fdt_prepare_platform"),
        "adapter must not patch FDT directly; got:\n{src}"
    );
    assert!(
        src.contains("&FSTART_ANCHOR"),
        "Override FDT variant requires FFS stage; must reference FSTART_ANCHOR; \
             got:\n{src}"
    );
}

// ===== payload_load adapter tests =================================
