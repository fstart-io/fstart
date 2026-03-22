//! fstart-stage: the single firmware stage binary crate.
//!
//! This crate's entire behavior is generated at build time from the board
//! RON file via `build.rs` + `fstart-codegen`. The generated code provides
//! `fstart_main()` with platform entry, driver init, and capability sequence.
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
#[cfg(any(feature = "acpi", feature = "pci-ecam"))]
extern crate fstart_alloc;

// Include the generated stage code (fstart_main, driver instances, etc.)
include!(concat!(env!("OUT_DIR"), "/generated_stage.rs"));
