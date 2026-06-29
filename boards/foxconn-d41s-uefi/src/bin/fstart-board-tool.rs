fn main() {
    xtask::board_tool::main(xtask::board_tool::BoardCallbacks {
        board_config: fstart_board_foxconn_d41s_uefi::board_config,
        build_info: fstart_board_foxconn_d41s_uefi::build_info,
        driver_bindings: fstart_board_foxconn_d41s_uefi::driver_bindings,
        acpi_only_devices: None,
    });
}
