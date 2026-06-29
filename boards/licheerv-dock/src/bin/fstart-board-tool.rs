fn main() {
    xtask::board_tool::main(xtask::board_tool::BoardCallbacks {
        board_config: fstart_board_licheerv_dock::board_config,
        build_info: fstart_board_licheerv_dock::build_info,
        driver_bindings: fstart_board_licheerv_dock::driver_bindings,
        acpi_only_devices: None,
    });
}
