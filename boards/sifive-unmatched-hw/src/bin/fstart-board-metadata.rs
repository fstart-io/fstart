fn main() {
    let metadata = fstart_board_meta::HostBoardMetadata::from_bindings(
        fstart_board_sifive_unmatched_hw::board_config(),
        fstart_board_sifive_unmatched_hw::build_info(),
        fstart_board_sifive_unmatched_hw::driver_bindings(),
        Vec::new(),
    );
    serde_json::to_writer_pretty(std::io::stdout(), &metadata).expect("emit fstart board metadata");
}
