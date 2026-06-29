fn main() {
    let metadata = fstart_board_meta::HostBoardMetadata::from_bindings(
        fstart_board_qemu_sbsa::board_config(),
        fstart_board_qemu_sbsa::build_info(),
        fstart_board_qemu_sbsa::driver_bindings(),
        fstart_board_qemu_sbsa::acpi_only_devices(),
    );
    serde_json::to_writer_pretty(std::io::stdout(), &metadata).expect("emit fstart board metadata");
}
