fn main() {
    let metadata = fstart_board_meta::HostBoardMetadata::from_bindings(
        fstart_board_orangepi_r1::board_config(),
        fstart_board_orangepi_r1::build_info(),
        fstart_board_orangepi_r1::driver_bindings(),
        Vec::new(),
    );
    serde_json::to_writer_pretty(std::io::stdout(), &metadata).expect("emit fstart board metadata");
}
