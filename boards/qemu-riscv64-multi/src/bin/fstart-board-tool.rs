fn main() {
    xtask::board_tool::main(xtask::board_tool::BoardCallbacks {
        board_config: fstart_board_qemu_riscv64_multi::board_config,
        build_info: fstart_board_qemu_riscv64_multi::build_info,
        driver_bindings: fstart_board_qemu_riscv64_multi::driver_bindings,
        acpi_only_devices: None,
    });
}
