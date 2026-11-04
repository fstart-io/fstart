// The browser runs the same image assembly as the native tools, whose
// main thread has 8 MiB of stack; the FFS builder keeps several owned
// manifests (about 120 KiB each) on it. wasm-ld's default is 1 MiB.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo:rustc-link-arg-cdylib=-zstack-size=8388608");
    }
}
