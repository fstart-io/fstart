fn main() {
    xtask::board_tool::main(xtask::board_tool::BoardCallbacks {
        board_config: fstart_board_sifive_unmatched_hw::board_config,
        build_info: fstart_board_sifive_unmatched_hw::build_info,
        driver_bindings: fstart_board_sifive_unmatched_hw::driver_bindings,
        acpi_only_devices: None,
    });
}
