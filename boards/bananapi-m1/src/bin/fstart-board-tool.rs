fn main() {
    xtask::board_tool::main(xtask::board_tool::BoardCallbacks {
        board_config: fstart_board_bananapi_m1::board_config,
        build_info: fstart_board_bananapi_m1::build_info,
        driver_bindings: fstart_board_bananapi_m1::driver_bindings,
        acpi_only_devices: None,
    });
}
