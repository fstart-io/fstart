//! Build-time board lowering library.
//!
//! Lowers Rust board metadata and generates non-Rust build artifacts:
//! - Linker scripts from memory maps
//! - Feature flags lists
//!
//! Used by `fstart-stage/build.rs` and by `xtask`.

pub mod board_loader;
pub mod linker;

// Re-export the parsed board type so callers can use it directly.
pub use board_loader::ParsedBoard;
