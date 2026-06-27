//! Metadata helper binary for `qemu-riscv64`.

fn main() {
    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "board-config".to_string());

    match command.as_str() {
        "board-config" => print!(
            "{}",
            fstart_codegen::ron_loader::rust_board_config_to_ron(
                fstart_board_qemu_riscv64::board_config(),
                fstart_board_qemu_riscv64::driver_instances(),
            )
            .expect("Rust board metadata must serialize for current stage build handoff")
        ),
        "board-info" => print_ron(&fstart_board_qemu_riscv64::board_info()),
        "build-info" => print_ron(&fstart_board_qemu_riscv64::build_info()),
        "name" => println!("{}", fstart_board_qemu_riscv64::board_name()),
        other => {
            eprintln!("unknown metadata command '{other}'");
            std::process::exit(2);
        }
    }
}

fn print_ron<T: serde::Serialize>(value: &T) {
    let pretty = ron::ser::PrettyConfig::default();
    let text = ron::ser::to_string_pretty(value, pretty).expect("metadata must serialize");
    print!("{text}");
}
