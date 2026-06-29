//! Static fixed-flow board adapter selection.
//!
//! `fstart-stage` owns only the final Cargo/cfg selection glue. Concrete static
//! board adapters encode board/platform-owned device construction and handoff
//! policy for the handwritten fixed flow; no Rust stage source is generated.

#[cfg(all(fstart_board = "qemu-riscv64", feature = "ns16550"))]
mod qemu_riscv64;

#[cfg(all(fstart_board = "qemu-riscv64", feature = "ns16550"))]
pub use qemu_riscv64::StageBoard;

#[cfg(not(all(fstart_board = "qemu-riscv64", feature = "ns16550")))]
mod empty;

#[cfg(not(all(fstart_board = "qemu-riscv64", feature = "ns16550")))]
pub use empty::StageBoard;
