fn main() {
    let metadata = fstart_board_meta::HostBoardMetadata::from_bindings(
        fstart_board_qemu_armv7::board_config(),
        fstart_board_qemu_armv7::build_info(),
        fstart_board_qemu_armv7::driver_bindings(),
        Vec::new(),
    );
    serde_json::to_writer_pretty(std::io::stdout(), &metadata).expect("emit fstart board metadata");
}
