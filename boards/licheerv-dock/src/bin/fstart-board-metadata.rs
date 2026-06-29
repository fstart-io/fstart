fn main() {
    let metadata = fstart_board_meta::HostBoardMetadata::from_bindings(
        fstart_board_licheerv_dock::board_config(),
        fstart_board_licheerv_dock::build_info(),
        fstart_board_licheerv_dock::driver_bindings(),
        Vec::new(),
    );
    serde_json::to_writer_pretty(std::io::stdout(), &metadata).expect("emit fstart board metadata");
}
