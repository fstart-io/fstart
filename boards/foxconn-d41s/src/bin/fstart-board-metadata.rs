fn main() {
    let metadata = fstart_board_meta::HostBoardMetadata::from_bindings(
        fstart_board_foxconn_d41s::board_config(),
        fstart_board_foxconn_d41s::build_info(),
        fstart_board_foxconn_d41s::driver_bindings(),
        Vec::new(),
    );
    serde_json::to_writer_pretty(std::io::stdout(), &metadata).expect("emit fstart board metadata");
}
