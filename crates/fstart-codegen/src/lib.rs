//! Build-time code generation library.
//!
//! Reads board.ron files and generates:
//! - Stage `fstart_main()` entry point with capability call sequence
//! - Static driver instantiation (rigid mode)
//! - Linker scripts from memory maps
//! - Feature flags lists
//!
//! Used by `fstart-stage/build.rs` and by `xtask`.

pub mod linker;
pub mod ron_loader;
pub mod stage_gen;

// Re-export the parsed board type so callers can use it directly.
pub use ron_loader::ParsedBoard;
