//! Static fixed-flow board adapter selection.
//!
//! These modules are transitional hand-written adapters for static typed mode.
//! Generic stage entry code lives in `main.rs`; board-specific device ownership
//! belongs here until it moves fully into board/platform crates.

#[cfg(all(fstart_board = "qemu-riscv64", feature = "ns16550"))]
mod qemu_riscv64;

#[cfg(all(fstart_board = "qemu-riscv64", feature = "ns16550"))]
pub use qemu_riscv64::StageBoard;

#[cfg(not(all(fstart_board = "qemu-riscv64", feature = "ns16550")))]
mod empty;

#[cfg(not(all(fstart_board = "qemu-riscv64", feature = "ns16550")))]
pub use empty::StageBoard;
