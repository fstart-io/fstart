fn main() {
    xtask::board_tool::main(xtask::board_tool::BoardCallbacks {
        board_config: fstart_board_qemu_sbsa::board_config,
        build_info: fstart_board_qemu_sbsa::build_info,
        driver_bindings: fstart_board_qemu_sbsa::driver_bindings,
        acpi_only_devices: Some(fstart_board_qemu_sbsa::acpi_only_devices),
    });
}
