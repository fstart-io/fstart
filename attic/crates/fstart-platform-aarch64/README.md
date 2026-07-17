# Unported remainder

`src/entry_sunxi.rs` is the H5/A64 AArch32-to-AArch64 BROM entry. It remains
unported attic capital until the sun50i board returns. The EL2/TF-A
ROM-to-high-DRAM relocation entry now lives in `crates/fstart-arch/src/aarch64.rs`.
