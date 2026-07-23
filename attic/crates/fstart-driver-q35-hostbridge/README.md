# Unported remainder

`src/smm.rs` is the Q35 TSEG/SMI sequence not present in the live QEMU
platform flow. It is archival source, not a crate. `crates/platform-qemu/src/q35.rs`
supersedes the host-bridge setup; port this remainder there when `qemu-q35`
enables SMM (currently `smm: None`).
