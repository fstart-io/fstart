use anyhow::{Result, bail};

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let check = match args.as_slice() {
        [] => false,
        [arg] if arg == "--check" => true,
        _ => bail!("usage: cargo register-gen [--check]"),
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    fstart_register_gen::run(root, check)
}
