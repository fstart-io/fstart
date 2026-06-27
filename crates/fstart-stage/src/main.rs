//! fstart-stage: the single firmware stage binary crate.
//!
//! This crate links a fixed handwritten stage executor with board-specific
//! data and typed adapter glue produced at build time. During the migration to
//! Rust board crates, `build.rs` still reads the board's transitional RON file
//! and emits only the `StagePlan` facts plus concrete driver construction
//! adapter used by `fstart-stage-runtime`.
//!
//! To build for a specific board:
//!   FSTART_BOARD_RON=boards/qemu-riscv64/board.ron \
//!     cargo build -p fstart-stage --target riscv64gc-unknown-none-elf \
//!     --features riscv64,ns16550 -Z build-std=core

#![no_std]
#![no_main]

// When a feature requiring heap allocation is active, pull in fstart-alloc
// to register the global allocator.  Without this explicit extern crate,
// the linker would not include it (nothing else references the crate by
// symbol).
#[cfg(any(
    feature = "acpi",
    feature = "pci-ecam",
    feature = "q35-hostbridge",
    feature = "crabefi"
))]
extern crate fstart_alloc;

// Include board-specific stage data and typed adapter glue.
include!(concat!(env!("OUT_DIR"), "/generated_stage.rs"));
