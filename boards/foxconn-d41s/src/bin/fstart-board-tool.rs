fn main() {
    xtask::board_tool::main(xtask::board_tool::BoardCallbacks {
        board_config: fstart_board_foxconn_d41s::board_config,
        build_info: fstart_board_foxconn_d41s::build_info,
        driver_bindings: fstart_board_foxconn_d41s::driver_bindings,
        acpi_only_devices: None,
    });
}
