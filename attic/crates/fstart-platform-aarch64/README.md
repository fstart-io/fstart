# Unported remainder

`src/entry_sunxi.rs` is the H5/A64 AArch32-to-AArch64 BROM entry and
`src/entry_relocate.rs` is the SBSA/TF-A ROM-to-high-DRAM relocation path.
They remain unported attic capital. QEMU virt entry and handoff support now
lives in `crates/fstart-arch/src/aarch64.rs`.
