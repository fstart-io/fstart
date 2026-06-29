fn main() {
    xtask::board_tool::main(xtask::board_tool::BoardCallbacks {
        board_config: fstart_board_lenovo_x61::board_config,
        build_info: fstart_board_lenovo_x61::build_info,
        driver_bindings: fstart_board_lenovo_x61::driver_bindings,
        acpi_only_devices: None,
    });
}
